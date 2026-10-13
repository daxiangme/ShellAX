//! Keyword highlighting (PRD 3.3 P1): user regex rules recolour matching text in terminal rows
//! (e.g. ERROR red, WARN yellow) without touching the PTY data. Pure logic here; the renderer
//! maps the returned column spans onto cells.
use regex::Regex;

/// Colour of a rule: a theme role or an explicit `#rrggbb`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color { Error, Warning, Success, Info, Accent, Rgb(u32) }

impl Color {
    pub fn parse(s: &str) -> Option<Color> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "error" | "red" => Color::Error, "warning" | "yellow" => Color::Warning, "success" | "green" => Color::Success,
            "info" | "blue" | "cyan" => Color::Info, "accent" | "purple" | "magenta" => Color::Accent,
            h => Color::Rgb(u32::from_str_radix(h.strip_prefix('#')?, 16).ok().filter(|_| h.len() == 7)?),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Rule { pub re: Regex, pub color: Color, pub bold: bool }

/// Compile `(pattern, colour, bold)` triples; invalid ones are skipped and reported.
pub fn compile<'a>(rules: impl IntoIterator<Item = (&'a str, &'a str, bool)>) -> (Vec<Rule>, Vec<String>) {
    let (mut ok, mut bad) = (vec![], vec![]);
    for (p, c, b) in rules {
        match (Regex::new(p), Color::parse(c)) {
            (Ok(re), Some(color)) if !p.is_empty() => ok.push(Rule { re, color, bold: b }),
            (Err(e), _) => bad.push(format!("{p}: {e}")),
            _ => bad.push(format!("{p}: bad colour {c:?}")),
        }
    }
    (ok, bad)
}

/// Column spans `(start, end_exclusive, rule index)` for one row given as one char per column.
/// Earlier rules win where matches overlap.
pub fn spans(rules: &[Rule], cols: &[char]) -> Vec<(usize, usize, usize)> {
    if rules.is_empty() || cols.iter().all(|c| c.is_whitespace()) { return vec![]; }
    let text: String = cols.iter().collect();
    // byte offset → column
    let mut b2c = Vec::with_capacity(text.len() + 1);
    for (i, ch) in cols.iter().enumerate() { for _ in 0..ch.len_utf8() { b2c.push(i); } }
    b2c.push(cols.len());
    let mut taken = vec![false; cols.len()];
    let mut out = vec![];
    for (ri, r) in rules.iter().enumerate() {
        for m in r.re.find_iter(&text) {
            if m.start() == m.end() { continue; }
            let (a, b) = (b2c[m.start()], b2c[m.end()]);
            if taken[a..b].iter().any(|t| *t) { continue; }
            taken[a..b].iter_mut().for_each(|t| *t = true);
            out.push((a, b, ri));
        }
    }
    out.sort();
    out
}

/// Default rules shipped with the app.
pub const DEFAULTS: &[(&str, &str, bool)] = &[
    (r"(?i)\b(error|fatal|failed|failure|panic|denied|refused)\b", "error", true),
    (r"(?i)\b(warn|warning|deprecated)\b", "warning", true),
    (r"\b(OK|PASS|PASSED|SUCCESS|done)\b", "success", false),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rules_and_spans() {
        let (rules, bad) = compile(DEFAULTS.iter().copied().chain([("[", "red", false), ("x", "nope", false), ("中文", "#ff8800", false)]));
        assert_eq!(rules.len(), 4); assert_eq!(bad.len(), 2);
        let row: Vec<char> = "ERROR: build failed, warning 中文 ok".chars().collect();
        let s = spans(&rules, &row);
        assert_eq!(s, vec![(0, 5, 0), (13, 19, 0), (21, 28, 1), (29, 31, 3)]);
        assert_eq!(rules[3].color, Color::Rgb(0xff8800));
        assert!(spans(&rules, &"   ".chars().collect::<Vec<_>>()).is_empty());
    }
}
