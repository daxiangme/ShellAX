//! Per-family SQL text: identifier/literal quoting, introspection queries and paged
//! table reads. All queries are sent as plain text, so every value arrives as a string.
use crate::assets::Family;

pub fn quote_ident(f: Family, s: &str) -> String {
    match f {
        Family::Mysql => format!("`{}`", s.replace('`', "``")),
        _ => format!("\"{}\"", s.replace('"', "\"\"")),
    }
}

pub fn quote_lit(f: Family, s: &str) -> String {
    let mut v = s.replace('\'', "''");
    if f == Family::Mysql { v = v.replace('\\', "\\\\"); }
    format!("'{v}'")
}

/// `db.table` (MySQL), `schema.table` (PostgreSQL), `table` (SQLite).
pub fn qualified(f: Family, db: &str, schema: Option<&str>, table: &str) -> String {
    match f {
        Family::Mysql if !db.is_empty() => format!("{}.{}", quote_ident(f, db), quote_ident(f, table)),
        Family::Postgres => match schema { Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(f, s), quote_ident(f, table)), _ => quote_ident(f, table) },
        _ => quote_ident(f, table),
    }
}

pub fn list_databases(f: Family) -> &'static str {
    match f {
        Family::Mysql => "SHOW DATABASES",
        Family::Postgres => "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY datname",
        _ => "SELECT name FROM pragma_database_list ORDER BY seq",
    }
}

pub fn list_schemas(f: Family) -> Option<&'static str> {
    (f == Family::Postgres).then_some("SELECT nspname FROM pg_namespace WHERE nspname !~ '^pg_' AND nspname <> 'information_schema' ORDER BY nspname")
}

/// Rows: (name, type) where type contains `VIEW` for views.
pub fn list_tables(f: Family, db: &str, schema: Option<&str>) -> String {
    match f {
        Family::Mysql => format!("SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME", quote_lit(f, db)),
        Family::Postgres => format!(
            "SELECT table_name, table_type FROM information_schema.tables WHERE table_schema = {} \
             UNION ALL SELECT matviewname, 'MATERIALIZED VIEW' FROM pg_matviews WHERE schemaname = {} ORDER BY 1",
            quote_lit(f, schema.unwrap_or("public")), quote_lit(f, schema.unwrap_or("public"))),
        _ => "SELECT name, type FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name".into(),
    }
}

/// Rows: (name, type, nullable YES/NO, key PRI/…, default, comment).
pub fn list_columns(f: Family, db: &str, schema: Option<&str>, table: &str) -> String {
    match f {
        Family::Mysql => format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, COLUMN_COMMENT FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION", quote_lit(f, db), quote_lit(f, table)),
        Family::Postgres => {
            let s = quote_lit(f, schema.unwrap_or("public"));
            let t = quote_lit(f, table);
            format!(
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END, \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indisprimary AND a.attnum = ANY(i.indkey)) THEN 'PRI' ELSE '' END, \
                 pg_get_expr(d.adbin, d.adrelid), col_description(c.oid, a.attnum) \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 WHERE n.nspname = {s} AND c.relname = {t} AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum")
        }
        _ => format!(
            "SELECT name, type, CASE WHEN \"notnull\" = 1 THEN 'NO' ELSE 'YES' END, CASE WHEN pk > 0 THEN 'PRI' ELSE '' END, dflt_value, '' \
             FROM pragma_table_info({}) ORDER BY cid", quote_lit(f, table)),
    }
}

/// One page of a table, optionally filtered (`where_` is a raw SQL condition typed by the
/// user) and ordered.
pub fn select_page(f: Family, table: &str, where_: &str, order: Option<(&str, bool)>, limit: usize, offset: usize) -> String {
    let mut s = format!("SELECT * FROM {table}");
    if !where_.trim().is_empty() { s.push_str(" WHERE "); s.push_str(where_.trim()); }
    if let Some((col, desc)) = order { s.push_str(&format!(" ORDER BY {}{}", quote_ident(f, col), if desc { " DESC" } else { "" })); }
    s.push_str(&format!(" LIMIT {limit} OFFSET {offset}"));
    s
}

pub fn count(table: &str, where_: &str) -> String {
    if where_.trim().is_empty() { format!("SELECT COUNT(*) FROM {table}") } else { format!("SELECT COUNT(*) FROM {table} WHERE {}", where_.trim()) }
}

/// Statement that switches the session's default database, where the engine has one.
pub fn use_database(f: Family, db: &str) -> Option<String> { (f == Family::Mysql && !db.is_empty()).then(|| format!("USE {}", quote_ident(f, db))) }

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
    }
}
