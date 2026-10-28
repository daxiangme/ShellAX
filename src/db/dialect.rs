//! Per-family SQL text: identifier/literal quoting, introspection queries and paged
//! table reads. All queries are sent as plain text, so every value arrives as a string.
use crate::assets::Family;

pub fn quote_ident(f: Family, s: &str) -> String {
    match f {
        Family::Mysql | Family::Clickhouse => format!("`{}`", s.replace('`', "``")),
        Family::Mssql => format!("[{}]", s.replace(']', "]]")),
        _ => format!("\"{}\"", s.replace('"', "\"\"")),
    }
}

pub fn quote_lit(f: Family, s: &str) -> String {
    let mut v = s.replace('\'', "''");
    if matches!(f, Family::Mysql | Family::Clickhouse) { v = v.replace('\\', "\\\\"); }
    if f == Family::Mssql { return format!("N'{v}'"); }
    format!("'{v}'")
}

/// `db.table` (MySQL), `schema.table` (PostgreSQL), `table` (SQLite).
pub fn qualified(f: Family, db: &str, schema: Option<&str>, table: &str) -> String {
    match f {
        Family::Mysql | Family::Clickhouse if !db.is_empty() => format!("{}.{}", quote_ident(f, db), quote_ident(f, table)),
        Family::Mssql => format!("{}.{}", quote_ident(f, schema.filter(|s| !s.is_empty()).unwrap_or("dbo")), quote_ident(f, table)),
        Family::Duckdb => match schema { Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(f, s), quote_ident(f, table)), _ => quote_ident(f, table) },
        Family::Postgres => match schema { Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(f, s), quote_ident(f, table)), _ => quote_ident(f, table) },
        Family::Dameng => {
            let s = if !db.is_empty() { Some(db) } else { schema.filter(|s| !s.is_empty()) };
            match s { Some(s) => format!("{}.{}", quote_ident(f, s), quote_ident(f, table)), None => quote_ident(f, table) }
        }
        _ => quote_ident(f, table),
    }
}

pub fn list_databases(f: Family) -> &'static str {
    match f {
        Family::Mysql => "SHOW DATABASES",
        Family::Postgres => "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname",
        Family::Mssql => "SELECT name FROM sys.databases WHERE HAS_DBACCESS(name) = 1 ORDER BY name",
        Family::Clickhouse => "SELECT name FROM system.databases ORDER BY name",
        Family::Duckdb => "SELECT database_name FROM duckdb_databases() WHERE NOT internal ORDER BY database_name",
        Family::Dameng => super::dm::SCHEMAS_SQL,
        _ => "SELECT name FROM pragma_database_list ORDER BY seq",
    }
}

pub fn list_schemas(f: Family) -> Option<&'static str> {
    match f {
        Family::Postgres => Some("SELECT nspname FROM pg_namespace WHERE nspname !~ '^pg_' AND nspname <> 'information_schema' ORDER BY nspname"),
        Family::Mssql => Some("SELECT s.name FROM sys.schemas s WHERE s.schema_id < 16384 AND s.name NOT IN ('sys','INFORMATION_SCHEMA','guest') AND s.name NOT LIKE 'db[_]%' ORDER BY s.name"),
        Family::Duckdb => Some("SELECT schema_name FROM information_schema.schemata WHERE catalog_name = current_database() AND schema_name NOT IN ('information_schema','pg_catalog') ORDER BY schema_name"),
        _ => None,
    }
}

/// Rows: (name, type) where type contains `VIEW` for views.
pub fn list_tables(f: Family, db: &str, schema: Option<&str>) -> String {
    match f {
        Family::Mysql => format!("SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME", quote_lit(f, db)),
        Family::Postgres => format!(
            "SELECT table_name, table_type FROM information_schema.tables WHERE table_schema = {} \
             UNION ALL SELECT matviewname, 'MATERIALIZED VIEW' FROM pg_matviews WHERE schemaname = {} ORDER BY 1",
            quote_lit(f, schema.unwrap_or("public")), quote_lit(f, schema.unwrap_or("public"))),
        Family::Mssql => format!("SELECT TABLE_NAME, TABLE_TYPE FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME", quote_lit(f, schema.unwrap_or("dbo"))),
        Family::Clickhouse => format!("SELECT name, if(engine LIKE '%View', 'VIEW', 'TABLE') FROM system.tables WHERE database = {} ORDER BY name", quote_lit(f, db)),
        Family::Duckdb => format!("SELECT table_name, table_type FROM information_schema.tables WHERE table_catalog = current_database() AND table_schema = {} ORDER BY table_name", quote_lit(f, schema.unwrap_or("main"))),
        Family::Dameng => super::dm::list_tables_sql(super::dm::owner(db, schema)),
        _ => "SELECT name, type FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name".into(),
    }
}

/// Rows: (name, type, nullable YES/NO, key PRI/…, default, comment, extra).
pub fn list_columns(f: Family, db: &str, schema: Option<&str>, table: &str) -> String {
    match f {
        Family::Mysql => format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, COLUMN_COMMENT, EXTRA FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION", quote_lit(f, db), quote_lit(f, table)),
        Family::Postgres => {
            let s = quote_lit(f, schema.unwrap_or("public"));
            let t = quote_lit(f, table);
            format!(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END, \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indisprimary AND a.attnum = ANY(i.indkey)) THEN 'PRI' ELSE '' END, \
                 pg_get_expr(d.adbin, d.adrelid), col_description(c.oid, a.attnum), \
                 CASE a.attidentity WHEN 'a' THEN 'identity always' WHEN 'd' THEN 'identity by default' ELSE CASE a.attgenerated WHEN 's' THEN 'generated stored' WHEN 'v' THEN 'generated virtual' ELSE '' END END \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 WHERE n.nspname = {s} AND c.relname = {t} AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum")
        }
        Family::Mssql => format!(
            "SELECT c.name, TYPE_NAME(c.user_type_id) + CASE WHEN TYPE_NAME(c.user_type_id) IN ('varchar','char','varbinary','binary') THEN '(' + IIF(c.max_length = -1, 'max', CAST(c.max_length AS varchar(10))) + ')' \
             WHEN TYPE_NAME(c.user_type_id) IN ('nvarchar','nchar') THEN '(' + IIF(c.max_length = -1, 'max', CAST(c.max_length / 2 AS varchar(10))) + ')' \
             WHEN TYPE_NAME(c.user_type_id) IN ('decimal','numeric') THEN '(' + CAST(c.precision AS varchar(5)) + ',' + CAST(c.scale AS varchar(5)) + ')' ELSE '' END, \
             IIF(c.is_nullable = 1, 'YES', 'NO'), \
             IIF(EXISTS (SELECT 1 FROM sys.index_columns ic JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id WHERE i.is_primary_key = 1 AND ic.object_id = c.object_id AND ic.column_id = c.column_id), 'PRI', ''), \
             OBJECT_DEFINITION(c.default_object_id), CAST(ep.value AS nvarchar(4000)), IIF(c.is_identity = 1, 'identity', IIF(c.is_computed = 1, 'computed', '')) \
             FROM sys.columns c LEFT JOIN sys.extended_properties ep ON ep.major_id = c.object_id AND ep.minor_id = c.column_id AND ep.name = 'MS_Description' \
             WHERE c.object_id = OBJECT_ID({}) ORDER BY c.column_id", quote_lit(f, &qualified(f, db, schema, table))),
        Family::Clickhouse => format!(
            "SELECT name, type, if(type LIKE 'Nullable(%', 'YES', 'NO'), if(is_in_primary_key, 'PRI', ''), if(default_expression = '', NULL, default_expression), comment, default_kind \
             FROM system.columns WHERE database = {} AND table = {} ORDER BY position", quote_lit(f, db), quote_lit(f, table)),
        Family::Duckdb => format!(
            "SELECT c.column_name, c.data_type, c.is_nullable, \
             CASE WHEN EXISTS (SELECT 1 FROM duckdb_constraints() k WHERE k.constraint_type = 'PRIMARY KEY' AND k.schema_name = c.table_schema AND k.table_name = c.table_name AND list_contains(k.constraint_column_names, c.column_name)) THEN 'PRI' ELSE '' END, \
             c.column_default, '', '' FROM information_schema.columns c WHERE c.table_catalog = current_database() AND c.table_schema = {} AND c.table_name = {} ORDER BY c.ordinal_position",
            quote_lit(f, schema.unwrap_or("main")), quote_lit(f, table)),
        Family::Dameng => super::dm::columns_sql(super::dm::owner(db, schema), table),
        _ => format!(
            "SELECT name, type, CASE WHEN \"notnull\" = 1 THEN 'NO' ELSE 'YES' END, CASE WHEN pk > 0 THEN 'PRI' ELSE '' END, dflt_value, '', CASE hidden WHEN 2 THEN 'generated virtual' WHEN 3 THEN 'generated stored' ELSE '' END \
             FROM pragma_table_xinfo({}) ORDER BY cid", quote_lit(f, table)),
    }
}

/// One page of a table, optionally filtered (`where_` is a raw SQL condition typed by the
/// user) and ordered.
pub fn select_page(f: Family, table: &str, where_: &str, order: Option<(&str, bool)>, limit: usize, offset: usize) -> String {
    let mut s = format!("SELECT * FROM {table}");
    if !where_.trim().is_empty() { s.push_str(" WHERE "); s.push_str(where_.trim()); }
    if let Some((col, desc)) = order { s.push_str(&format!(" ORDER BY {}{}", quote_ident(f, col), if desc { " DESC" } else { "" })); }
    if f == Family::Mssql {
        // OFFSET/FETCH needs an ORDER BY
        if order.is_none() { s.push_str(" ORDER BY (SELECT NULL)"); }
        s.push_str(&format!(" OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY"));
        return s;
    }
    s.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
    s
}

pub fn count(table: &str, where_: &str) -> String {
    if where_.trim().is_empty() { format!("SELECT COUNT(*) FROM {table}") } else { format!("SELECT COUNT(*) FROM {table} WHERE {}", where_.trim()) }
}

/// Statement that switches the session's default database, where the engine has one.
pub fn use_database(f: Family, db: &str) -> Option<String> { (matches!(f, Family::Mysql | Family::Mssql) && !db.is_empty()).then(|| format!("USE {}", quote_ident(f, db))) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_ident(Family::Mysql, "a`b"), "`a``b`");
        assert_eq!(quote_ident(Family::Postgres, "a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_lit(Family::Mysql, "o'r\\"), "'o''r\\\\'");
        assert_eq!(quote_lit(Family::Sqlite, "o'r\\"), "'o''r\\'");
        assert_eq!(qualified(Family::Mysql, "shop", None, "orders"), "`shop`.`orders`");
        assert_eq!(qualified(Family::Postgres, "app", Some("public"), "t"), "\"public\".\"t\"");
        assert_eq!(qualified(Family::Sqlite, "main", None, "t"), "\"t\"");
    }

    #[test]
    fn paging_sql() {
        let t = qualified(Family::Mysql, "d", None, "t");
        assert_eq!(select_page(Family::Mysql, &t, "", None, 500, 0), "SELECT * FROM `d`.`t` LIMIT 500 OFFSET 0");
        assert_eq!(select_page(Family::Postgres, "\"t\"", " id > 3 ", Some(("id", true)), 10, 20), "SELECT * FROM \"t\" WHERE id > 3 ORDER BY \"id\" DESC LIMIT 10 OFFSET 20");
        assert_eq!(count("\"t\"", ""), "SELECT COUNT(*) FROM \"t\"");
        assert!(list_tables(Family::Mysql, "x'y", None).contains("'x''y'"));
        assert_eq!(use_database(Family::Mysql, "a"), Some("USE `a`".into()));
        assert_eq!(use_database(Family::Postgres, "a"), None);
        assert_eq!(select_page(Family::Mssql, "[dbo].[t]", "", None, 10, 20), "SELECT * FROM [dbo].[t] ORDER BY (SELECT NULL) OFFSET 20 ROWS FETCH NEXT 10 ROWS ONLY");
        assert_eq!(qualified(Family::Mssql, "d", None, "a]b"), "[dbo].[a]]b]");
        assert_eq!(quote_lit(Family::Mssql, "o'k"), "N'o''k'");
        assert_eq!(qualified(Family::Clickhouse, "d", None, "t"), "`d`.`t`");
    }
}

/// `CREATE TABLE` text for PostgreSQL, which has no single built-in for it: columns, then
/// constraint definitions, then the indexes that do not back a constraint.
pub fn pg_create_table(table: &str, cols: &[super::ColumnInfo], constraints: &[(String, String)], indexes: &[String]) -> String {
    let f = Family::Postgres;
    let mut lines: Vec<String> = cols.iter().map(|c| {
        // serial columns: integer + nextval() of their own sequence, which the DDL cannot assume exists
        let serial = c.default.as_deref().is_some_and(|d| d.starts_with("nextval(")) && matches!(c.type_name.as_str(), "integer" | "bigint" | "smallint");
        let ty = if serial { match c.type_name.as_str() { "bigint" => "bigserial", "smallint" => "smallserial", _ => "serial" } } else { c.type_name.as_str() };
        let mut l = format!("  {} {ty}", quote_ident(f, &c.name));
        match (c.extra.as_str(), &c.default) {
            ("identity always", _) => l.push_str(" GENERATED ALWAYS AS IDENTITY"),
            ("identity by default", _) => l.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
            ("generated stored", Some(d)) => l.push_str(&format!(" GENERATED ALWAYS AS ({d}) STORED")),
            ("generated virtual", Some(d)) => l.push_str(&format!(" GENERATED ALWAYS AS ({d}) VIRTUAL")),
            (_, Some(d)) if !serial => l.push_str(&format!(" DEFAULT {d}")),
            _ => {}
        }
        if c.nullable == Some(false) { l.push_str(" NOT NULL"); }
        l
    }).collect();
    lines.extend(constraints.iter().map(|(n, d)| format!("  CONSTRAINT {} {d}", quote_ident(f, n))));
    let mut s = format!("CREATE TABLE {table} (\n{}\n);", lines.join(",\n"));
    for c in cols.iter().filter(|c| !c.comment.is_empty()) {
        s.push_str(&format!("\nCOMMENT ON COLUMN {table}.{} IS {};", quote_ident(f, &c.name), quote_lit(f, &c.comment)));
    }
    for i in indexes { s.push_str(&format!("\n{i};")); }
    s
}

/// PostgreSQL: rows (name, definition) of a table's constraints, and its other indexes.
pub fn pg_constraints(table_lit: &str) -> String {
    format!("SELECT conname, pg_get_constraintdef(oid, true) FROM pg_constraint WHERE conrelid = {table_lit}::regclass AND contype IN ('p','u','f','c','x') ORDER BY contype = 'p' DESC, conname")
}
pub fn pg_indexes(table_lit: &str) -> String {
    format!("SELECT pg_get_indexdef(i.indexrelid) FROM pg_index i WHERE i.indrelid = {table_lit}::regclass \
             AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid) ORDER BY 1")
}

#[cfg(test)]
mod ddl_tests {
    use super::*;
    use crate::db::ColumnInfo;

    #[test]
    fn pg_ddl() {
        let cols = vec![
            ColumnInfo { name: "id".into(), type_name: "integer".into(), nullable: Some(false), primary: true, default: Some("nextval('t_id_seq'::regclass)".into()), ..Default::default() },
            ColumnInfo { name: "g".into(), type_name: "integer".into(), default: Some("(id * 2)".into()), extra: "generated stored".into(), ..Default::default() },
            ColumnInfo { name: "name".into(), type_name: "character varying(20)".into(), nullable: Some(true), comment: "it's".into(), ..Default::default() },
        ];
        let s = pg_create_table("\"public\".\"t\"", &cols, &[("t_pkey".into(), "PRIMARY KEY (id)".into())], &["CREATE INDEX t_name ON public.t USING btree (name)".into()]);
        assert_eq!(s, "CREATE TABLE \"public\".\"t\" (\n  \"id\" serial NOT NULL,\n  \"g\" integer GENERATED ALWAYS AS ((id * 2)) STORED,\n  \"name\" character varying(20),\n  CONSTRAINT \"t_pkey\" PRIMARY KEY (id)\n);\nCOMMENT ON COLUMN \"public\".\"t\".\"name\" IS 'it''s';\nCREATE INDEX t_name ON public.t USING btree (name);");
    }
}

/// `CREATE TABLE` text for SQL Server assembled from columns (no built-in DDL for tables).
pub fn mssql_create_table(q: &str, cols: &[super::ColumnInfo]) -> String {
    let f = Family::Mssql;
    let mut lines: Vec<String> = cols.iter().map(|c| {
        let mut l = format!("    {} {}", quote_ident(f, &c.name), c.type_name);
        if c.extra == "identity" { l.push_str(" IDENTITY(1,1)"); }
        if c.nullable == Some(false) { l.push_str(" NOT NULL"); }
        if let Some(d) = &c.default { l.push_str(" DEFAULT "); l.push_str(d); }
        l
    }).collect();
    let pk: Vec<String> = cols.iter().filter(|c| c.primary).map(|c| quote_ident(f, &c.name)).collect();
    if !pk.is_empty() { lines.push(format!("    PRIMARY KEY ({})", pk.join(", "))); }
    format!("CREATE TABLE {q} (\n{}\n);", lines.join(",\n"))
}
