//! Keyword-triggered notifications: regex rules matched against completed output lines.
//! Rate-limited per rule so a flood of matches produces one notification per window.
#[derive(Clone, Debug)]
pub struct Rule { pub re: regex::Regex, pub title: String }
pub struct Notifier { /// Lines to ignore (the echo of a command the user just entered).
    pub skip: usize, rules: Vec<Rule>, last: Vec<Option<std::time::Instant>>, pub min_gap: std::time::Duration, partial: String }
impl Notifier {
    pub fn new(rules: Vec<(String, String)>) -> Self {
        let rules: Vec<Rule> = rules.into_iter().filter_map(|(p, t)| regex::Regex::new(&p).ok().map(|re| Rule { re, title: if t.is_empty() { p } else { t } })).collect();
        Self { skip: 0, last: vec![None; rules.len()], rules, min_gap: std::time::Duration::from_secs(10), partial: String::new() }
    }
    pub fn is_empty(&self) -> bool { self.rules.is_empty() }
    /// Feed raw output; returns (title, matched line) per fired rule.
    pub fn feed(&mut self, data: &[u8], now: std::time::Instant) -> Vec<(String, String)> {
        if self.rules.is_empty() { return vec![]; }
        self.partial.push_str(&String::from_utf8_lossy(data));
        let mut out = vec![];
        while let Some(i) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=i).collect();
            let line = strip_ansi(line.trim_end());
            if self.skip > 0 { self.skip -= 1; continue; }
            for (k, r) in self.rules.iter().enumerate() {
                if r.re.is_match(&line) && self.last[k].is_none_or(|t| now.duration_since(t) >= self.min_gap) { self.last[k] = Some(now); out.push((r.title.clone(), line.chars().take(200).collect())); }
            }
        }
        if self.partial.len() > 8192 { self.partial.clear(); }
        out
    }
}
pub fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\r").unwrap()).replace_all(s, "").into_owned()
}
#[cfg(test)]
mod tests {
    #[test]
    fn fires_and_rate_limits() {
        let mut n = super::Notifier::new(vec![("(?i)build (failed|done)".into(), "构建".into()), ("[".into(), "bad".into())]);
        let t = std::time::Instant::now();
        n.skip = 1;
        assert!(n.feed(b"$ echo build failed\r\n", t).is_empty(), "echo of the typed command is skipped");
        assert!(n.feed(b"compiling\r\nBUILD ", t).is_empty());
        let f = n.feed(b"\x1b[31mFAILED\x1b[0m\r\n", t);
        assert_eq!(f, vec![("构建".to_string(), "BUILD FAILED".to_string())]);
        assert!(n.feed(b"build done\n", t + std::time::Duration::from_secs(1)).is_empty());
        assert_eq!(n.feed(b"build done\n", t + std::time::Duration::from_secs(11)).len(), 1);
    }
}
