//! A small SQL tokenizer for highlighting and completion context. It never fails: anything
//! it does not recognise is `Other`.
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok { Keyword, Ident, Quoted, String, Number, Comment, Operator, Space, Other }

pub const KEYWORDS: &[&str] = &[
    "ADD", "ALL", "ALTER", "AND", "ANY", "AS", "ASC", "AUTO_INCREMENT", "BEGIN", "BETWEEN", "BY", "CASCADE", "CASE", "CAST", "CHECK", "COLUMN",
    "COMMIT", "CONSTRAINT", "CREATE", "CROSS", "CURRENT_DATE", "CURRENT_TIMESTAMP", "DATABASE", "DEFAULT", "DELETE", "DESC", "DESCRIBE", "DISTINCT",
    "DROP", "ELSE", "END", "EXCEPT", "EXISTS", "EXPLAIN", "FALSE", "FOREIGN", "FROM", "FULL", "GROUP", "HAVING", "IF", "ILIKE", "IN", "INDEX",
    "INNER", "INSERT", "INTERSECT", "INTO", "IS", "JOIN", "KEY", "LEFT", "LIKE", "LIMIT", "NOT", "NULL", "OFFSET", "ON", "OR", "ORDER", "OUTER",
    "OVER", "PARTITION", "PRIMARY", "REFERENCES", "RENAME", "REPLACE", "RETURNING", "RIGHT", "ROLLBACK", "SCHEMA", "SELECT", "SET", "SHOW",
    "TABLE", "THEN", "TO", "TRUE", "TRUNCATE", "UNION", "UNIQUE", "UPDATE", "USE", "USING", "VALUES", "VIEW", "WHEN", "WHERE", "WITH",
    "COUNT", "SUM", "AVG", "MIN", "MAX", "COALESCE", "NOW",
];

pub fn is_keyword(w: &str) -> bool { KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(w)) }

fn ident_char(c: char) -> bool { c.is_alphanumeric() || c == '_' || c == '$' }

/// Tokens covering all of `s`, in order.
pub fn tokens(s: &str) -> Vec<(Range<usize>, Tok)> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    let until = |from: usize, pat: &str| s[from..].find(pat).map(|p| from + p + pat.len()).unwrap_or(s.len());
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        let start = i;
        let tok = if c.is_whitespace() {
            while i < s.len() && s[i..].chars().next().unwrap().is_whitespace() { i += s[i..].chars().next().unwrap().len_utf8(); }
            Tok::Space
        } else if s[i..].starts_with("--") || c == '#' {
            i = s[i..].find('\n').map(|p| i + p).unwrap_or(s.len());
            Tok::Comment
        } else if s[i..].starts_with("/*") {
            i = until(i + 2, "*/");
            Tok::Comment
        } else if c == '\'' || c == '"' || c == '`' {
            i += 1;
            loop {
                match s[i..].find(c) {
                    None => { i = s.len(); break; }
                    Some(p) => { i += p + 1; if b.get(i) == Some(&(c as u8)) { i += 1; } else { break; } }
                }
            }
            if c == '\'' { Tok::String } else { Tok::Quoted }
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            while i < s.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') { i += 1; }
            Tok::Number
        } else if ident_char(c) {
            while i < s.len() && ident_char(s[i..].chars().next().unwrap()) { i += s[i..].chars().next().unwrap().len_utf8(); }
            if is_keyword(&s[start..i]) { Tok::Keyword } else { Tok::Ident }
        } else {
            i += c.len_utf8();
            if "=<>!+-*/%|&^~:,;.()".contains(c) { Tok::Operator } else { Tok::Other }
        };
        out.push((start..i, tok));
    }
    out
}

/// The statement around `offset` (split on `;` outside strings and comments).
pub fn statement_at(s: &str, offset: usize) -> Range<usize> {
    let mut start = 0;
    for (r, t) in tokens(s) {
        if t == Tok::Operator && &s[r.clone()] == ";" {
            if r.start >= offset { return start..r.start; }
            start = r.end;
        }
    }
    start..s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes() {
        let s = "SELECT a, 'x''y' FROM `t` -- c\nWHERE n = 1.5 /* k */";
        let t: Vec<(&str, Tok)> = tokens(s).into_iter().filter(|(_, t)| *t != Tok::Space).map(|(r, t)| (&s[r], t)).collect();
        assert_eq!(t, vec![("SELECT", Tok::Keyword), ("a", Tok::Ident), (",", Tok::Operator), ("'x''y'", Tok::String), ("FROM", Tok::Keyword), ("`t`", Tok::Quoted), ("-- c", Tok::Comment), ("WHERE", Tok::Keyword), ("n", Tok::Ident), ("=", Tok::Operator), ("1.5", Tok::Number), ("/* k */", Tok::Comment)]);
        assert_eq!(tokens("'open").len(), 1);
        let q = "select 1; select ';' ; x";
        assert_eq!(&q[statement_at(q, 12)], " select ';' ");
        assert_eq!(&q[statement_at(q, q.len())], " x");
    }
}
