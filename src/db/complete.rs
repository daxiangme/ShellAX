//! Completion candidates from keywords and the schema loaded in the tree.
use std::collections::HashMap;

use super::lex::{self, Tok};

#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub tables: Vec<String>,
    /// Lower-cased table name -> columns (filled for tables whose columns were loaded).
    pub columns: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Keyword, Table, Column }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item { pub label: String, pub kind: Kind, pub detail: String }

fn unquote(s: &str) -> String { s.trim_matches(|c| c == '`' || c == '"' || c == '[' || c == ']').to_string() }

/// Word prefix before `offset` and its `qualifier.` if any.
pub fn word_at(s: &str, offset: usize) -> (String, Option<String>) {
    let before = &s[..offset];
    let w0 = before.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_').len();
    let prefix = before[w0..].to_string();
    let qual = before[..w0].strip_suffix('.').map(|q| {
        let qs = q.trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '`' || c == '"').len();
        unquote(&q[qs..])
    }).filter(|q| !q.is_empty());
    (prefix, qual)
}

/// Tables named after FROM / JOIN / UPDATE / INTO in `stmt`, with their aliases.
pub fn table_refs(stmt: &str) -> Vec<(String, Option<String>)> {
    let toks: Vec<(String, Tok)> = lex::tokens(stmt).into_iter().filter(|(_, t)| !matches!(t, Tok::Space | Tok::Comment)).map(|(r, t)| (stmt[r].to_string(), t)).collect();
    let mut out = vec![];
    let mut i = 0;
    while i < toks.len() {
        let kw = toks[i].0.to_ascii_uppercase();
        if matches!(kw.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") || (kw == "," && out.len() > 0) {
            let mut j = i + 1;
            // schema.table → table
            let mut name = None;
            while j < toks.len() && matches!(toks[j].1, Tok::Ident | Tok::Quoted) {
                name = Some(unquote(&toks[j].0));
                if toks.get(j + 1).is_some_and(|t| t.0 == ".") { j += 2; } else { j += 1; break; }
            }
            if let Some(n) = name {
                let mut alias = None;
                if toks.get(j).is_some_and(|t| t.0.eq_ignore_ascii_case("AS")) { j += 1; }
                if let Some(t) = toks.get(j).filter(|t| matches!(t.1, Tok::Ident | Tok::Quoted)) { alias = Some(unquote(&t.0)); j += 1; }
                out.push((n, alias));
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn matches(label: &str, prefix: &str) -> Option<u8> {
    let (l, p) = (label.to_lowercase(), prefix.to_lowercase());
    if l.starts_with(&p) { Some(0) } else if p.len() >= 2 && l.contains(&p) { Some(1) } else { None }
}

pub fn suggest(text: &str, offset: usize, schema: &Schema) -> Vec<Item> {
    let (prefix, qual) = word_at(text, offset);
    let stmt = &text[lex::statement_at(text, offset)];
    let refs = table_refs(stmt);
    let cols_of = |t: &str| schema.columns.get(&t.to_lowercase()).cloned().unwrap_or_default();
    let mut items: Vec<(u8, u8, Item)> = vec![];
    if let Some(q) = qual {
        let table = refs.iter().find(|(t, a)| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&q)) || t.eq_ignore_ascii_case(&q)).map(|(t, _)| t.clone()).unwrap_or(q);
        for c in cols_of(&table) { if let Some(r) = matches(&c, &prefix) { items.push((r, 0, Item { label: c, kind: Kind::Column, detail: table.clone() })); } }
    } else {
        if prefix.is_empty() { return vec![]; }
        for (t, _) in &refs { for c in cols_of(t) { if let Some(r) = matches(&c, &prefix) { items.push((r, 0, Item { label: c, kind: Kind::Column, detail: t.clone() })); } } }
        for t in &schema.tables { if let Some(r) = matches(t, &prefix) { items.push((r, 1, Item { label: t.clone(), kind: Kind::Table, detail: String::new() })); } }
        for k in lex::KEYWORDS { if let Some(0) = matches(k, &prefix) { items.push((0, 2, Item { label: k.to_string(), kind: Kind::Keyword, detail: String::new() })); } }
    }
    items.sort_by(|a, b| (a.0, a.1, a.2.label.len(), &a.2.label).cmp(&(b.0, b.1, b.2.label.len(), &b.2.label)));
    let mut seen = std::collections::HashSet::new();
    items.into_iter().map(|x| x.2).filter(|i| seen.insert((i.label.clone(), i.kind as u8))).filter(|i| !i.label.eq_ignore_ascii_case(&prefix) || i.kind != Kind::Keyword).take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        let mut s = Schema { tables: vec!["users".into(), "orders".into()], ..Default::default() };
        s.columns.insert("users".into(), vec!["id".into(), "name".into(), "email".into()]);
        s.columns.insert("orders".into(), vec!["id".into(), "user_id".into(), "total".into()]);
        s
    }

    #[test]
    fn suggests() {
        let q = "select u.na from users u join orders o on o.";
        assert_eq!(word_at(q, 11), ("na".into(), Some("u".into())));
        assert_eq!(table_refs(q), vec![("users".into(), Some("u".into())), ("orders".into(), Some("o".into()))]);
        let l: Vec<String> = suggest(q, 11, &schema()).into_iter().map(|i| i.label).collect();
        assert_eq!(l, vec!["name"]);
        let l: Vec<String> = suggest(q, q.len(), &schema()).into_iter().map(|i| i.label).collect();
        assert_eq!(l, vec!["id", "total", "user_id"]);
        let q2 = "select * from us";
        assert_eq!(suggest(q2, q2.len(), &schema())[0].label, "users");
        let q3 = "sel";
        assert_eq!(suggest(q3, 3, &schema())[0].label, "SELECT");
        let q4 = "select tot from public.orders";
        assert_eq!(suggest(q4, 10, &schema())[0].label, "total");
    }
}
