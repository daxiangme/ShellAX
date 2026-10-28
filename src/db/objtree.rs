//! Object groups shown under an open database / schema in the connection tree, per engine,
//! and the catalog query listing each group's members (first column = name).
use crate::assets::{DbKind, Family};
use super::dialect::quote_lit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Tables, Views, MatViews, Functions, Procedures, Events, Triggers, Sequences, Synonyms,
    Dictionaries, Collections, GridFs, Indexes, Queries, Backups,
}

impl Group {
    pub fn id(self) -> &'static str {
        match self {
            Group::Tables => "tables", Group::Views => "views", Group::MatViews => "matviews", Group::Functions => "functions",
            Group::Procedures => "procedures", Group::Events => "events", Group::Triggers => "triggers", Group::Sequences => "sequences",
            Group::Synonyms => "synonyms", Group::Dictionaries => "dictionaries", Group::Collections => "collections", Group::GridFs => "gridfs",
            Group::Indexes => "indexes", Group::Queries => "queries", Group::Backups => "backups",
        }
    }
    pub fn from_id(s: &str) -> Option<Group> { ALL.iter().copied().find(|g| g.id() == s) }
    /// i18n key of the group label.
    pub fn label_key(self) -> String { format!("dbt.g_{}", self.id()) }
    /// Groups whose members come from the server catalog (others: saved files / placeholder).
    pub fn from_catalog(self) -> bool { !matches!(self, Group::Queries | Group::Backups) }
    /// Members open as a data grid on double-click.
    pub fn opens_data(self) -> bool { matches!(self, Group::Tables | Group::Views | Group::MatViews) }
}

const ALL: [Group; 15] = [
    Group::Tables, Group::Views, Group::MatViews, Group::Functions, Group::Procedures, Group::Events, Group::Triggers, Group::Sequences,
    Group::Synonyms, Group::Dictionaries, Group::Collections, Group::GridFs, Group::Indexes, Group::Queries, Group::Backups,
];

/// Groups under one open database (or schema, for engines with a schema level), in display order.
pub fn groups(kind: DbKind) -> &'static [Group] {
    use Group::*;
    match kind {
        DbKind::Mongodb => &[Collections, Views, GridFs, Indexes, Queries],
        DbKind::Sqlite | DbKind::Duckdb => &[Tables, Views, Triggers, Queries],
        _ => match kind.family() {
            Family::Mysql => &[Tables, Views, Functions, Procedures, Events, Triggers, Queries, Backups],
            Family::Postgres => &[Tables, Views, MatViews, Functions, Sequences, Triggers, Queries, Backups],
            Family::Mssql => &[Tables, Views, Procedures, Functions, Synonyms, Queries],
            Family::Clickhouse => &[Tables, Views, MatViews, Dictionaries, Functions, Queries],
            Family::Sqlite | Family::Duckdb => &[Tables, Views, Triggers, Queries],
            Family::Dameng => &[Tables, Views, Queries],
            Family::Other => &[Tables, Views, Queries],
        },
    }
}

/// Catalog query for a group (column 0 = object name), `None` when the engine has no such list.
pub fn list_sql(f: Family, g: Group, db: &str, schema: Option<&str>) -> Option<String> {
    let l = |s: &str| quote_lit(f, s);
    let sc = |d: &str| l(schema.filter(|s| !s.is_empty()).unwrap_or(d));
    Some(match (f, g) {
        (Family::Mysql, Group::Tables) => format!("SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} AND TABLE_TYPE <> 'VIEW' ORDER BY 1", l(db)),
        (Family::Mysql, Group::Views) => format!("SELECT TABLE_NAME FROM information_schema.VIEWS WHERE TABLE_SCHEMA = {} ORDER BY 1", l(db)),
        (Family::Mysql, Group::Functions) => format!("SELECT ROUTINE_NAME FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = {} AND ROUTINE_TYPE = 'FUNCTION' ORDER BY 1", l(db)),
        (Family::Mysql, Group::Procedures) => format!("SELECT ROUTINE_NAME FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = {} AND ROUTINE_TYPE = 'PROCEDURE' ORDER BY 1", l(db)),
        (Family::Mysql, Group::Events) => format!("SELECT EVENT_NAME FROM information_schema.EVENTS WHERE EVENT_SCHEMA = {} ORDER BY 1", l(db)),
        (Family::Mysql, Group::Triggers) => format!("SELECT TRIGGER_NAME FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = {} ORDER BY 1", l(db)),

        (Family::Postgres, Group::Tables) => format!("SELECT table_name FROM information_schema.tables WHERE table_schema = {} AND table_type = 'BASE TABLE' ORDER BY 1", sc("public")),
        (Family::Postgres, Group::Views) => format!("SELECT table_name FROM information_schema.views WHERE table_schema = {} ORDER BY 1", sc("public")),
        (Family::Postgres, Group::MatViews) => format!("SELECT matviewname FROM pg_matviews WHERE schemaname = {} ORDER BY 1", sc("public")),
        (Family::Postgres, Group::Functions) => format!("SELECT DISTINCT p.proname FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = {} ORDER BY 1", sc("public")),
        (Family::Postgres, Group::Sequences) => format!("SELECT sequence_name FROM information_schema.sequences WHERE sequence_schema = {} ORDER BY 1", sc("public")),
        (Family::Postgres, Group::Triggers) => format!("SELECT DISTINCT trigger_name FROM information_schema.triggers WHERE trigger_schema = {} ORDER BY 1", sc("public")),

        (Family::Mssql, Group::Tables) => format!("SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_SCHEMA = {} AND TABLE_TYPE = 'BASE TABLE' ORDER BY 1", sc("dbo")),
        (Family::Mssql, Group::Views) => format!("SELECT TABLE_NAME FROM INFORMATION_SCHEMA.VIEWS WHERE TABLE_SCHEMA = {} ORDER BY 1", sc("dbo")),
        (Family::Mssql, Group::Procedures) => format!("SELECT ROUTINE_NAME FROM INFORMATION_SCHEMA.ROUTINES WHERE ROUTINE_SCHEMA = {} AND ROUTINE_TYPE = 'PROCEDURE' ORDER BY 1", sc("dbo")),
        (Family::Mssql, Group::Functions) => format!("SELECT ROUTINE_NAME FROM INFORMATION_SCHEMA.ROUTINES WHERE ROUTINE_SCHEMA = {} AND ROUTINE_TYPE = 'FUNCTION' ORDER BY 1", sc("dbo")),
        (Family::Mssql, Group::Synonyms) => format!("SELECT s.name FROM sys.synonyms s WHERE SCHEMA_NAME(s.schema_id) = {} ORDER BY 1", sc("dbo")),

        (Family::Clickhouse, Group::Tables) => format!("SELECT name FROM system.tables WHERE database = {} AND engine NOT LIKE '%View' AND engine <> 'Dictionary' ORDER BY name", l(db)),
        (Family::Clickhouse, Group::Views) => format!("SELECT name FROM system.tables WHERE database = {} AND engine = 'View' ORDER BY name", l(db)),
        (Family::Clickhouse, Group::MatViews) => format!("SELECT name FROM system.tables WHERE database = {} AND engine = 'MaterializedView' ORDER BY name", l(db)),
        (Family::Clickhouse, Group::Dictionaries) => format!("SELECT name FROM system.dictionaries WHERE database = {} ORDER BY name", l(db)),
        (Family::Clickhouse, Group::Functions) => "SELECT name FROM system.functions WHERE origin = 'SQLUserDefined' ORDER BY name".into(),

        (Family::Dameng, Group::Tables) => super::dm::tables_sql(super::dm::owner(db, schema)),
        (Family::Dameng, Group::Views) => super::dm::views_sql(super::dm::owner(db, schema)),

        (Family::Sqlite, Group::Tables) => "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name".into(),
        (Family::Sqlite, Group::Views) => "SELECT name FROM sqlite_master WHERE type = 'view' ORDER BY name".into(),
        (Family::Sqlite, Group::Triggers) => "SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name".into(),

        (Family::Duckdb, Group::Tables) => format!("SELECT table_name FROM information_schema.tables WHERE table_catalog = current_database() AND table_schema = {} AND table_type = 'BASE TABLE' ORDER BY 1", sc("main")),
        (Family::Duckdb, Group::Views) => format!("SELECT table_name FROM information_schema.tables WHERE table_catalog = current_database() AND table_schema = {} AND table_type = 'VIEW' ORDER BY 1", sc("main")),
        (Family::Duckdb, Group::Triggers) => return None,
        _ => return None,
    })
}

/// Statement dropping all rows of a table.
pub fn truncate_sql(f: Family, qualified: &str) -> String {
    match f { Family::Sqlite => format!("DELETE FROM {qualified}"), _ => format!("TRUNCATE TABLE {qualified}") }
}

/// Query showing a view's definition (column 0 or the last column carries the text).
pub fn view_definition_sql(f: Family, db: &str, schema: Option<&str>, view: &str) -> Option<String> {
    let l = |s: &str| quote_lit(f, s);
    Some(match f {
        Family::Mysql => format!("SELECT VIEW_DEFINITION FROM information_schema.VIEWS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {}", l(db), l(view)),
        Family::Postgres => format!("SELECT pg_get_viewdef(to_regclass({}), true)", l(&format!("\"{}\".\"{}\"", schema.unwrap_or("public").replace('"', "\"\""), view.replace('"', "\"\"")))),
        Family::Mssql => format!("SELECT OBJECT_DEFINITION(OBJECT_ID({}))", l(&format!("[{}].[{}]", schema.unwrap_or("dbo"), view))),
        Family::Clickhouse => format!("SELECT create_table_query FROM system.tables WHERE database = {} AND name = {}", l(db), l(view)),
        Family::Sqlite => format!("SELECT sql FROM sqlite_master WHERE type = 'view' AND name = {}", l(view)),
        Family::Duckdb => format!("SELECT sql FROM duckdb_views() WHERE view_name = {}", l(view)),
        Family::Dameng => super::dm::get_ddl_sql("VIEW", view, super::dm::owner(db, schema)),
        Family::Other => return None,
    })
}

/// Case-insensitive substring filter on member names.
pub fn filter<'a>(names: &'a [String], q: &str) -> Vec<&'a String> {
    let q = q.trim().to_lowercase();
    names.iter().filter(|n| q.is_empty() || n.to_lowercase().contains(&q)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_per_engine() {
        assert_eq!(groups(DbKind::Mysql).iter().map(|g| g.id()).collect::<Vec<_>>(), ["tables", "views", "functions", "procedures", "events", "triggers", "queries", "backups"]);
        assert!(groups(DbKind::Postgres).contains(&Group::MatViews) && groups(DbKind::Postgres).contains(&Group::Sequences));
        assert!(groups(DbKind::Sqlserver).contains(&Group::Synonyms));
        assert!(groups(DbKind::Clickhouse).contains(&Group::Dictionaries));
        assert_eq!(groups(DbKind::Sqlite), groups(DbKind::Duckdb));
        assert!(groups(DbKind::Mongodb).contains(&Group::GridFs));
        for k in DbKind::ALL { assert_eq!(groups(k).last().map(|g| g.from_catalog()), Some(false), "{k:?}"); }
    }
    #[test]
    fn catalog_sql_quotes_names() {
        let s = list_sql(Family::Mysql, Group::Events, "a'b", None).unwrap();
        assert!(s.contains("'a''b'"));
        assert!(list_sql(Family::Postgres, Group::Tables, "x", None).unwrap().contains("'public'"));
        assert!(list_sql(Family::Postgres, Group::Tables, "x", Some("s1")).unwrap().contains("'s1'"));
        assert!(list_sql(Family::Mssql, Group::Synonyms, "x", None).unwrap().contains("N'dbo'"));
        assert!(list_sql(Family::Sqlite, Group::Queries, "x", None).is_none());
        assert_eq!(Group::from_id("matviews"), Some(Group::MatViews));
    }
    #[test]
    fn helpers() {
        assert_eq!(truncate_sql(Family::Sqlite, "\"t\""), "DELETE FROM \"t\"");
        assert_eq!(truncate_sql(Family::Mysql, "`t`"), "TRUNCATE TABLE `t`");
        let v = vec!["Users".to_string(), "orders".into()];
        assert_eq!(filter(&v, "US"), vec![&v[0]]);
        assert!(view_definition_sql(Family::Sqlite, "", None, "v").unwrap().contains("'v'"));
    }
}
