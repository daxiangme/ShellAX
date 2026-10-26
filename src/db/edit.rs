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

fn lit(f: Family, v: &Value) -> String { match v { None => "NULL".into(), Some(s) => quote_lit(f, s) } }

fn where_key(f: Family, key: &[(String, Value)]) -> String {
    key.iter().map(|(c, v)| match v { None => format!("{} IS NULL", quote_ident(f, c)), Some(_) => format!("{} = {}", quote_ident(f, c), lit(f, v)) }).collect::<Vec<_>>().join(" AND ")
}

/// One statement for `c` on `table` (already qualified and quoted).
pub fn to_sql(f: Family, table: &str, c: &Change) -> String {
    match c {
        Change::Update { key, set } => {
            let s = set.iter().map(|(c, v)| format!("{} = {}", quote_ident(f, c), lit(f, v))).collect::<Vec<_>>().join(", ");
            format!("UPDATE {table} SET {s} WHERE {}", where_key(f, key))
        }
        Change::Insert { values } if values.is_empty() => match f {
            Family::Mysql => format!("INSERT INTO {table} () VALUES ()"),
            _ => format!("INSERT INTO {table} DEFAULT VALUES"),
        },
        Change::Insert { values } => {
            let cols = values.iter().map(|(c, _)| quote_ident(f, c)).collect::<Vec<_>>().join(", ");
            let vals = values.iter().map(|(_, v)| lit(f, v)).collect::<Vec<_>>().join(", ");
            format!("INSERT INTO {table} ({cols}) VALUES ({vals})")
        }
        Change::Delete { key } => format!("DELETE FROM {table} WHERE {}", where_key(f, key)),
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
    }
}
