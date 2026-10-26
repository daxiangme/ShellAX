//! Result export (CSV / JSON / SQL INSERT) and import parsing (CSV / JSON). Values are the
//! display strings the drivers produce; NULL stays distinct from the empty string where
//! the format can say so.
use anyhow::{bail, Context, Result};

use crate::assets::Family;

use super::dialect::{quote_ident, quote_lit};
use super::ColumnInfo;

type Row = Vec<Option<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format { Csv, Json, Sql }

impl Format {
    pub fn ext(self) -> &'static str { match self { Format::Csv => "csv", Format::Json => "json", Format::Sql => "sql" } }
    pub fn from_path(p: &std::path::Path) -> Option<Format> {
        match p.extension()?.to_str()?.to_ascii_lowercase().as_str() { "csv" | "tsv" | "txt" => Some(Format::Csv), "json" => Some(Format::Json), "sql" => Some(Format::Sql), _ => None }
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// RFC 4180 CSV with a header row; NULL is an empty field.
pub fn to_csv(cols: &[ColumnInfo], rows: &[Row]) -> String {
    let mut out = String::new();
    out.push_str(&cols.iter().map(|c| csv_field(&c.name)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");
    for r in rows {
        out.push_str(&r.iter().map(|v| v.as_deref().map(csv_field).unwrap_or_default()).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

fn numeric_type(t: &str) -> bool {
    let t = t.to_ascii_uppercase();
    ["INT", "DECIMAL", "NUMERIC", "FLOAT", "DOUBLE", "REAL", "SERIAL"].iter().any(|k| t.contains(k)) && !t.contains("INTERVAL") && !t.contains("POINT")
}

/// Array of objects. Numeric columns become JSON numbers when the text is a finite number.
pub fn to_json(cols: &[ColumnInfo], rows: &[Row]) -> String {
    let arr: Vec<serde_json::Value> = rows.iter().map(|r| {
        let mut m = serde_json::Map::new();
        for (i, c) in cols.iter().enumerate() {
            let v = match r.get(i).cloned().flatten() {
                None => serde_json::Value::Null,
                Some(s) if numeric_type(&c.type_name) => s.parse::<serde_json::Number>().map(serde_json::Value::Number).unwrap_or(serde_json::Value::String(s)),
                Some(s) => serde_json::Value::String(s),
            };
            m.insert(c.name.clone(), v);
        }
        serde_json::Value::Object(m)
    }).collect();
    serde_json::to_string_pretty(&arr).unwrap_or_default()
}

/// Multi-row INSERT statements, `batch` rows each.
pub fn insert_statements(f: Family, table: &str, header: &[String], rows: &[Row], batch: usize) -> Vec<String> {
    let cols = header.iter().map(|c| quote_ident(f, c)).collect::<Vec<_>>().join(", ");
    rows.chunks(batch.max(1)).map(|ch| {
        let vals = ch.iter().map(|r| format!("({})", (0..header.len()).map(|i| match r.get(i).cloned().flatten() { None => "NULL".into(), Some(s) => quote_lit(f, &s) }).collect::<Vec<_>>().join(", "))).collect::<Vec<_>>().join(",\n  ");
        format!("INSERT INTO {table} ({cols}) VALUES\n  {vals}")
    }).collect()
}

pub fn to_sql(f: Family, table: &str, cols: &[ColumnInfo], rows: &[Row]) -> String {
    let header: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
    insert_statements(f, table, &header, rows, 100).into_iter().map(|s| s + ";\n").collect()
}

pub fn export(fmt: Format, f: Family, table: &str, cols: &[ColumnInfo], rows: &[Row]) -> String {
    match fmt { Format::Csv => to_csv(cols, rows), Format::Json => to_json(cols, rows), Format::Sql => to_sql(f, table, cols, rows) }
}

/// Parse CSV (comma, or tab when the first line has tabs and no commas). The first row is
/// the header; empty unquoted fields are NULL, `""` is the empty string.
pub fn parse_csv(text: &str) -> Result<(Vec<String>, Vec<Row>)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first = text.lines().next().unwrap_or("");
    let sep = if first.contains('\t') && !first.contains(',') { '\t' } else { ',' };
    let mut rows: Vec<Row> = vec![];
    let mut row: Row = vec![];
    let mut field = String::new();
    let (mut quoted, mut in_q, mut after_q) = (false, false, false);
    let mut it = text.chars().peekable();
    let end_field = |row: &mut Row, field: &mut String, quoted: &mut bool| { row.push(if field.is_empty() && !*quoted { None } else { Some(std::mem::take(field)) }); *quoted = false; };
    while let Some(ch) = it.next() {
        if in_q {
            if ch == '"' { if it.peek() == Some(&'"') { it.next(); field.push('"'); } else { in_q = false; after_q = true; } } else { field.push(ch); }
            continue;
        }
        match ch {
            '"' if field.is_empty() && !after_q => { in_q = true; quoted = true; }
            c if c == sep => { end_field(&mut row, &mut field, &mut quoted); after_q = false; }
            '\r' => {}
            '\n' => { end_field(&mut row, &mut field, &mut quoted); after_q = false; if !(row.len() == 1 && row[0].is_none()) { rows.push(std::mem::take(&mut row)); } else { row.clear(); } }
            c => field.push(c),
        }
    }
    if in_q { bail!("unterminated quoted field"); }
    if !field.is_empty() || quoted || !row.is_empty() { end_field(&mut row, &mut field, &mut quoted); rows.push(row); }
    if rows.is_empty() { bail!("empty file"); }
    let header: Vec<String> = rows.remove(0).into_iter().map(|h| h.unwrap_or_default().trim().to_string()).collect();
    if header.iter().any(|h| h.is_empty()) { bail!("header has an empty column name"); }
    Ok((header, rows))
}

/// Parse a JSON array of objects; the header is the union of keys in first-seen order.
/// Nested values are stored as JSON text.
pub fn parse_json(text: &str) -> Result<(Vec<String>, Vec<Row>)> {
    let v: serde_json::Value = serde_json::from_str(text).context("JSON")?;
    let arr = v.as_array().context("expected a JSON array of objects")?;
    let mut header: Vec<String> = vec![];
    for o in arr { for k in o.as_object().context("expected a JSON array of objects")?.keys() { if !header.contains(k) { header.push(k.clone()); } } }
    let rows = arr.iter().map(|o| {
        let o = o.as_object().unwrap();
        header.iter().map(|h| match o.get(h) { None | Some(serde_json::Value::Null) => None, Some(serde_json::Value::String(s)) => Some(s.clone()), Some(serde_json::Value::Bool(b)) => Some(if *b { "1".into() } else { "0".into() }), Some(x) => Some(x.to_string()) }).collect()
    }).collect();
    Ok((header, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<ColumnInfo> { vec![ColumnInfo { name: "id".into(), type_name: "INT".into(), ..Default::default() }, ColumnInfo { name: "note".into(), type_name: "TEXT".into(), ..Default::default() }] }

    #[test]
    fn csv_round_trip() {
        let rows = vec![vec![Some("1".into()), Some("a,\"b\"\nc".into())], vec![Some("2".into()), None], vec![Some("3".into()), Some(String::new())]];
        let s = to_csv(&cols(), &rows);
        assert!(s.starts_with("id,note\r\n1,\"a,\"\"b\"\"\nc\"\r\n2,\r\n3,\r\n"));
        let (h, back) = parse_csv(&s).unwrap();
        assert_eq!(h, vec!["id", "note"]);
        // an empty unquoted field reads back as NULL
        assert_eq!(back[0], rows[0]);
        assert_eq!(back[1], rows[1]);
        let (_, q) = parse_csv("a\tb\n\"\"\t\n").unwrap();
        assert_eq!(q[0], vec![Some(String::new()), None]);
    }

    #[test]
    fn json_and_sql() {
        let rows = vec![vec![Some("1".into()), None], vec![Some("x".into()), Some("y".into())]];
        let j = to_json(&cols(), &rows);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v[0]["id"], serde_json::json!(1));
        assert_eq!(v[0]["note"], serde_json::Value::Null);
        assert_eq!(v[1]["id"], serde_json::json!("x"));
        let (h, back) = parse_json(r#"[{"a":1,"b":null},{"b":"s","c":{"k":true}}]"#).unwrap();
        assert_eq!(h, vec!["a", "b", "c"]);
        assert_eq!(back[1], vec![None, Some("s".into()), Some("{\"k\":true}".into())]);
        let s = insert_statements(Family::Mysql, "`t`", &["a".into(), "b".into()], &[vec![Some("1".into()), None], vec![Some("o'k".into())]], 1);
        assert_eq!(s, vec!["INSERT INTO `t` (`a`, `b`) VALUES\n  ('1', NULL)", "INSERT INTO `t` (`a`, `b`) VALUES\n  ('o''k', NULL)"]);
        assert_eq!(Format::from_path(std::path::Path::new("a.JSON")), Some(Format::Json));
    }
}
