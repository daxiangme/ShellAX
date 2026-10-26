//! SQL text utilities shared by the editor and the drivers: statement splitting that
//! respects quotes, comments and PostgreSQL dollar quoting; the statement under the
//! cursor; and a coarse read/write/dangerous classification used by the read-only and
//! production guards. Not a parser: it only needs to be right about boundaries and the
//! leading keyword.

/// A statement with its byte range in the source (trimmed of surrounding whitespace).
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt { pub start: usize, pub end: usize, pub text: String }

/// Split `sql` at top-level `;`. Empty statements (and comment-only ones) are dropped.
pub fn split(sql: &str) -> Vec<Stmt> {
    let b = sql.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    let mut seg = 0;
    let push = |out: &mut Vec<Stmt>, s: usize, e: usize| {
        let raw = &sql[s..e];
        let lead = raw.len() - raw.trim_start().len();
        let t = raw.trim();
        if !t.is_empty() && !strip_comments(t).trim().is_empty() { out.push(Stmt { start: s + lead, end: s + lead + t.len(), text: t.to_string() }); }
    };
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(b, i),
            b'-' if b.get(i + 1) == Some(&b'-') => i = skip_line(b, i),
            b'#' => i = skip_line(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => i = skip_block(b, i),
            b'$' => i = skip_dollar(sql, i),
            b';' => { push(&mut out, seg, i); i += 1; seg = i; }
            _ => i += 1,
        }
    }
    push(&mut out, seg, b.len());
    out
}

fn skip_quoted(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' && q != b'`' { j += 2; continue; }
        if b[j] == q { if b.get(j + 1) == Some(&q) { j += 2; continue; } return j + 1; }
        j += 1;
    }
    b.len()
}
fn skip_line(b: &[u8], i: usize) -> usize { b[i..].iter().position(|&c| c == b'\n').map(|p| i + p + 1).unwrap_or(b.len()) }
fn skip_block(b: &[u8], i: usize) -> usize { b[i + 2..].windows(2).position(|w| w == b"*/").map(|p| i + 2 + p + 2).unwrap_or(b.len()) }
/// `$tag$ … $tag$` (tag may be empty). A lone `$1` parameter is not a quote.
fn skip_dollar(s: &str, i: usize) -> usize {
    let b = s.as_bytes();
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') { j += 1; }
    if j >= b.len() || b[j] != b'$' || b.get(i + 1).is_some_and(|c| c.is_ascii_digit()) { return i + 1; }
    let tag = &s[i..=j];
    match s[j + 1..].find(tag) { Some(p) => j + 1 + p + tag.len(), None => b.len() }
}

/// Remove `--`, `#` and `/* */` comments (outside quotes).
pub fn strip_comments(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < b.len() {
        let n = match b[i] {
            b'\'' | b'"' | b'`' => { let e = skip_quoted(b, i); out.push_str(&sql[i..e]); i = e; continue; }
            b'-' if b.get(i + 1) == Some(&b'-') => skip_line(b, i),
            b'#' => skip_line(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => skip_block(b, i),
            _ => { let c = sql[i..].chars().next().unwrap(); out.push(c); i += c.len_utf8(); continue; }
        };
        out.push(' ');
        i = n;
    }
    out
}

/// The statement containing byte offset `cursor` (or the nearest one before it).
pub fn at_cursor(sql: &str, cursor: usize) -> Option<Stmt> {
    let v = split(sql);
    v.iter().find(|s| cursor >= s.start && cursor <= s.end).cloned().or_else(|| v.iter().rev().find(|s| s.end <= cursor).cloned()).or_else(|| v.first().cloned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class { Read, Write, Dangerous }

fn words(sql: &str) -> Vec<String> {
    strip_comments(sql).split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).map(|w| w.to_ascii_uppercase()).collect()
}

/// Coarse class of one statement. Dangerous = DROP / TRUNCATE / ALTER … DROP, or
/// DELETE / UPDATE without WHERE. Unknown statements count as writes.
pub fn classify(stmt: &str) -> Class {
    let w = words(stmt);
    let Some(first) = w.first().map(String::as_str) else { return Class::Read };
    let has = |k: &str| w.iter().any(|x| x == k);
    match first {
        "SELECT" | "SHOW" | "EXPLAIN" | "DESCRIBE" | "DESC" | "USE" | "VALUES" | "TABLE" => {
            if first == "SELECT" && has("INTO") && !has("FROM") { Class::Write } else { Class::Read }
        }
        "PRAGMA" => if stmt.contains('=') { Class::Write } else { Class::Read },
        "WITH" => if has("DELETE") || has("UPDATE") || has("INSERT") { Class::Write } else { Class::Read },
        "DROP" | "TRUNCATE" => Class::Dangerous,
        "ALTER" if has("DROP") => Class::Dangerous,
        "DELETE" | "UPDATE" if !has("WHERE") => Class::Dangerous,
        _ => Class::Write,
    }
}

/// True when the statement returns rows (decides result grid vs. "N rows affected").
pub fn returns_rows(stmt: &str) -> bool {
    let w = words(stmt);
    matches!(w.first().map(String::as_str), Some("SELECT" | "SHOW" | "EXPLAIN" | "DESCRIBE" | "DESC" | "WITH" | "VALUES" | "TABLE" | "PRAGMA")) || w.iter().any(|x| x == "RETURNING")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> { split(s).into_iter().map(|s| s.text).collect() }

    #[test]
    fn splits_respecting_quotes_and_comments() {
        assert_eq!(texts("select 1; select 'a;b' ; -- c;\nselect \"x;\";"), vec!["select 1", "select 'a;b'", "-- c;\nselect \"x;\""]);
        assert_eq!(texts("/* a; */ select 1;;  ; "), vec!["/* a; */ select 1"]);
        assert_eq!(texts("select 'it''s;'; select `a;b` from t"), vec!["select 'it''s;'", "select `a;b` from t"]);
        assert_eq!(texts("select 'a\\';b'; x"), vec!["select 'a\\';b'", "x"]);
        assert_eq!(texts("-- only comment;\n"), Vec::<String>::new());
    }

    #[test]
    fn dollar_quotes() {
        let f = "create function f() returns int as $$ begin return 1; end $$ language plpgsql; select 2";
        assert_eq!(split(f).len(), 2);
        let g = "do $body$ begin perform 1; end $body$; select $1";
        assert_eq!(texts(g), vec!["do $body$ begin perform 1; end $body$", "select $1"]);
    }

    #[test]
    fn statement_at_cursor() {
        let s = "select 1;\nselect 2;\n\nselect 3";
        assert_eq!(at_cursor(s, 0).unwrap().text, "select 1");
        assert_eq!(at_cursor(s, 12).unwrap().text, "select 2");
        assert_eq!(at_cursor(s, 20).unwrap().text, "select 2");
        assert_eq!(at_cursor(s, s.len()).unwrap().text, "select 3");
        assert!(at_cursor("", 0).is_none());
    }

    #[test]
    fn classification() {
        assert_eq!(classify("SELECT * FROM t"), Class::Read);
        assert_eq!(classify("  -- x\n show tables"), Class::Read);
        assert_eq!(classify("with a as (select 1) select * from a"), Class::Read);
        assert_eq!(classify("insert into t values (1)"), Class::Write);
        assert_eq!(classify("update t set a=1 where id=2"), Class::Write);
        assert_eq!(classify("update t set a=1"), Class::Dangerous);
        assert_eq!(classify("delete from t"), Class::Dangerous);
        assert_eq!(classify("DROP TABLE t"), Class::Dangerous);
        assert_eq!(classify("alter table t drop column c"), Class::Dangerous);
        assert_eq!(classify("pragma journal_mode=wal"), Class::Write);
        assert!(returns_rows("select 1") && !returns_rows("insert into t values(1)") && returns_rows("insert into t values(1) returning id"));
    }
}
