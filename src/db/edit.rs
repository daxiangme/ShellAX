//! Staged grid edits turned into SQL. Rows are addressed by primary key; a table without
//! one cannot be edited (there is no safe way to say which row changed).
use crate::assets::Family;

use super::dialect::{quote_ident, quote_lit};
use super::ColumnInfo;

pub type Value = Option<String>;

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// `key`: primary-key columns and their original values; `set`: changed columns.
    Update { key: Vec<(String, Value)>, set: Vec<(String, Value)> },
    /// Only the columns the user filled; the rest take their defaults.
    Insert { values: Vec<(String, Value)> },
    Delete { key: Vec<(String, Value)> },
}

/// Primary-key column indexes, or None when the table has no primary key.
pub fn pk_cols(cols: &[ColumnInfo]) -> Option<Vec<usize>> {
    let v: Vec<usize> = cols.iter().enumerate().filter(|(_, c)| c.primary).map(|(i, _)| i).collect();
    (!v.is_empty()).then_some(v)
}

/// The key of `row`: (column, original value) for each primary-key column.
pub fn key_of(cols: &[ColumnInfo], pk: &[usize], row: &[Value]) -> Vec<(String, Value)> {
    pk.iter().map(|&i| (cols[i].name.clone(), row.get(i).cloned().flatten())).collect()
}

/// Column types whose grid text is not the stored value (hex previews of binary data).
pub fn editable_type(type_name: &str) -> bool {
    let t = type_name.to_ascii_uppercase();
    !(t.contains("BLOB") || t.contains("BINARY") || t.contains("BYTEA") || t == "GEOMETRY")
}


fn binary_type(t: &str) -> bool { let t = t.to_ascii_uppercase(); t.contains("BLOB") || t.contains("BINARY") || t.contains("BYTEA") }

/// `0x…` hex text (the drivers' rendering of binary values) as bytes in hex, if it is one.
fn hex_text(s: &str) -> Option<&str> {
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("\\x"))?;
    (h.len() % 2 == 0 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

/// PostgreSQL type without modifiers (`numeric(12,3)` → `numeric`, `character varying(30)[]`
/// → `character varying[]`): an explicit cast to a length-limited type would silently
/// truncate, while the assignment that follows still enforces the limit.
pub fn pg_base_type(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut depth = 0;
    for ch in t.chars() {
        match ch { '(' => depth += 1, ')' if depth > 0 => depth -= 1, c if depth == 0 => out.push(c), _ => {} }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Literal for `v` in a column of type `type_name` (as the catalog spells it). PostgreSQL
/// values carry an explicit cast so arrays, json, uuid, ranges etc. resolve the same way
/// in every position (VALUES lists, WHERE, SET). Binary columns accept `0x…` hex text.
pub fn literal(f: Family, type_name: &str, v: &Value) -> String {
    let Some(s) = v else { return "NULL".into() };
    if binary_type(type_name) {
        if let Some(h) = hex_text(s) {
            return match f { Family::Postgres => format!("'\\x{h}'::bytea"), _ => format!("X'{h}'") };
        }
    }
    match f {
        Family::Postgres if !type_name.trim().is_empty() => format!("{}::{}", quote_lit(f, s), pg_base_type(type_name)),
        _ => quote_lit(f, s),
    }
}


/// One statement for `c` on `table` (already qualified and quoted), values untyped.
pub fn to_sql(f: Family, table: &str, c: &Change) -> String { to_sql_typed(f, table, &[], c) }

/// Like [`to_sql`], with each value written by [`literal`] using the type of the column of
/// the same name in `cols` (the table's catalog columns).
pub fn to_sql_typed(f: Family, table: &str, cols: &[ColumnInfo], c: &Change) -> String {
    let ty = |name: &str| cols.iter().find(|c| c.name == name).map(|c| c.type_name.as_str()).unwrap_or("");
    let lit = |name: &str, v: &Value| literal(f, ty(name), v);
    let where_key = |key: &[(String, Value)]| key.iter().map(|(c, v)| match v { None => format!("{} IS NULL", quote_ident(f, c)), Some(_) => format!("{} = {}", quote_ident(f, c), lit(c, v)) }).collect::<Vec<_>>().join(" AND ");
    match c {
        Change::Update { key, set } => {
            let s = set.iter().map(|(c, v)| format!("{} = {}", quote_ident(f, c), lit(c, v))).collect::<Vec<_>>().join(", ");
            format!("UPDATE {table} SET {s} WHERE {}", where_key(key))
        }
        Change::Insert { values } if values.is_empty() => match f {
            Family::Mysql => format!("INSERT INTO {table} () VALUES ()"),
            _ => format!("INSERT INTO {table} DEFAULT VALUES"),
        },
        Change::Insert { values } => {
            let cols = values.iter().map(|(c, _)| quote_ident(f, c)).collect::<Vec<_>>().join(", ");
            let vals = values.iter().map(|(c, v)| lit(c, v)).collect::<Vec<_>>().join(", ");
            format!("INSERT INTO {table} ({cols}) VALUES ({vals})")
        }
        Change::Delete { key } => format!("DELETE FROM {table} WHERE {}", where_key(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<ColumnInfo> {
        vec![ColumnInfo { name: "id".into(), primary: true, ..Default::default() }, ColumnInfo { name: "name".into(), ..Default::default() }]
    }

    #[test]
    fn statements() {
        let c = cols();
        let pk = pk_cols(&c).unwrap();
        let key = key_of(&c, &pk, &[Some("7".into()), Some("x".into())]);
        assert_eq!(to_sql(Family::Mysql, "`t`", &Change::Update { key: key.clone(), set: vec![("name".into(), Some("o'k".into()))] }), "UPDATE `t` SET `name` = 'o''k' WHERE `id` = '7'");
        assert_eq!(to_sql(Family::Postgres, "\"t\"", &Change::Update { key: key.clone(), set: vec![("name".into(), None)] }), "UPDATE \"t\" SET \"name\" = NULL WHERE \"id\" = '7'");
        assert_eq!(to_sql(Family::Sqlite, "\"t\"", &Change::Delete { key: vec![("id".into(), None)] }), "DELETE FROM \"t\" WHERE \"id\" IS NULL");
        assert_eq!(to_sql(Family::Mysql, "`t`", &Change::Insert { values: vec![("name".into(), Some("a".into()))] }), "INSERT INTO `t` (`name`) VALUES ('a')");
        assert_eq!(to_sql(Family::Postgres, "\"t\"", &Change::Insert { values: vec![] }), "INSERT INTO \"t\" DEFAULT VALUES");
        assert!(pk_cols(&[ColumnInfo::default()]).is_none());
        assert!(!editable_type("longblob") && editable_type("varchar(10)"));
        let tc = vec![ColumnInfo { name: "id".into(), type_name: "uuid".into(), primary: true, ..Default::default() }, ColumnInfo { name: "v".into(), type_name: "character varying(30)[]".into(), ..Default::default() }, ColumnInfo { name: "b".into(), type_name: "bytea".into(), ..Default::default() }];
        assert_eq!(to_sql_typed(Family::Postgres, "t", &tc, &Change::Update { key: vec![("id".into(), Some("u".into()))], set: vec![("v".into(), Some("{a}".into())), ("b".into(), Some("0x00ff".into()))] }),
            "UPDATE t SET \"v\" = '{a}'::character varying[], \"b\" = '\\x00ff'::bytea WHERE \"id\" = 'u'::uuid");
        assert_eq!(pg_base_type("timestamp(3) with time zone"), "timestamp with time zone");
        assert_eq!(literal(Family::Mysql, "varbinary(8)", &Some("0x00FF".into())), "X'00FF'");
        assert_eq!(literal(Family::Mysql, "int", &None), "NULL");
    }
}
