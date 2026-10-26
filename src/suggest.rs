//! History-based autosuggestions (fish / zsh-autosuggestions style), implemented from scratch.
//!
//! Pieces:
//! - [`History`]: per-host command list, newest last, deduplicated, with a sensitive-command filter.
//! - [`InputTracker`]: figures out "what the user has typed on the prompt line" either from an
//!   OSC 133;B prompt mark (shell integration) or from a keystroke buffer that is cross-checked
//!   against the screen (works over SSH with no remote install, and naturally skips unechoed
//!   password input).

/// Maximum entries kept per history.
pub const MAX_HISTORY: usize = 5000;

/// One recorded command. `ts` is unix seconds; missing on lines written before timestamps existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistItem {
    pub cmd: String,
    pub ts: Option<i64>,
}

#[derive(Default, Debug, Clone)]
pub struct History {
    entries: Vec<HistItem>,
    /// Our own history file (append-only, one command per line, mode 0600). None = memory only.
    path: Option<std::path::PathBuf>,
}

/// File name stem for a history key. `@.-_` are kept so `user@host:22` stays readable.
pub fn history_stem(key: &str) -> String {
    key.chars().map(|c| if c.is_ascii_alphanumeric() || "@.-_".contains(c) { c } else { '_' }).collect()
}

/// `{data_dir}/history/{sanitized key}.txt`.
pub fn history_path(dir: &std::path::Path, key: &str) -> std::path::PathBuf {
    dir.join("history").join(format!("{}.txt", history_stem(key)))
}

/// `1700000000\tcmd` is a timestamped line. Anything else, including a command that happens to
/// contain a tab, stays a legacy command — unless the prefix is a plausible unix second.
pub fn parse_hist_line(line: &str) -> (Option<i64>, &str) {
    if let Some((a, b)) = line.split_once('\t') {
        if let Ok(n) = a.parse::<i64>() {
            if (1_000_000_000..10_000_000_000).contains(&n) { return (Some(n), b); }
        }
    }
    (None, line)
}

fn now_ts() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Strip zsh EXTENDED_HISTORY prefix ": <epoch>:<duration>;".
fn strip_zsh_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix(": ") {
        if let Some(i) = rest.find(';') {
            let meta = &rest[..i];
            if meta.split(':').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) { return &rest[i + 1..]; }
        }
    }
    line
}

impl History {
    pub fn new() -> Self { Self::default() }

    /// Open (or create) a persistent history file and load it.
    pub fn open(path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        let mut h = Self::default();
        h.path = Some(path.clone());
        if let Ok(body) = std::fs::read_to_string(&path) {
            for l in body.lines() {
                let (ts, cmd) = parse_hist_line(l);
                h.insert_item(cmd, ts);
            }
            // compact when the file grew far past the cap
            if body.lines().count() > MAX_HISTORY * 2 { h.rewrite(); }
        }
        h
    }

    /// Load from a bash- or zsh-style history file body (`#<epoch>` lines skipped, zsh
    /// `: ts:0;cmd` prefixes stripped, zsh multi-line continuations dropped). Not persisted.
    pub fn extend_from_bash_history(&mut self, body: &str) {
        let mut cont = false;
        for line in body.lines() {
            let was_cont = cont;
            cont = line.ends_with('\\');
            if was_cont || cont || line.starts_with('#') { continue; }
            self.insert(strip_zsh_prefix(line));
        }
    }

    /// Record a command typed in this app (persisted when a file is attached). Returns false if filtered out.
    pub fn add(&mut self, cmd: &str) -> bool {
        let ts = now_ts();
        let ok = self.insert_item(cmd, Some(ts));
        if ok {
            if let Some(p) = &self.path {
                use std::io::Write;
                let mut o = std::fs::OpenOptions::new();
                o.create(true).append(true);
                #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); }
                if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
                if let Ok(mut f) = o.open(p) {
                    let stored = self.entries.last().map(|e| e.cmd.as_str()).unwrap_or("");
                    let _ = writeln!(f, "{ts}\t{stored}");
                }
            }
        }
        ok
    }

    fn insert_item(&mut self, cmd: &str, ts: Option<i64>) -> bool {
        // Leading space = user asked not to record (bash HISTCONTROL=ignorespace convention).
        if cmd.starts_with(' ') { return false; }
        let cmd = cmd.trim_end();
        if cmd.trim().is_empty() || cmd.len() > 1000 || is_sensitive(cmd) { return false; }
        self.entries.retain(|e| e.cmd != cmd);
        self.entries.push(HistItem { cmd: cmd.to_string(), ts });
        if self.entries.len() > MAX_HISTORY { self.entries.remove(0); }
        true
    }

    fn insert(&mut self, cmd: &str) -> bool { self.insert_item(cmd, None) }

    fn serialized(&self) -> String {
        let mut s = String::new();
        for e in &self.entries {
            if let Some(ts) = e.ts {
                s.push_str(&ts.to_string());
                s.push('\t');
            }
            s.push_str(&e.cmd);
            s.push('\n');
        }
        s
    }

    fn rewrite(&self) {
        let Some(p) = &self.path else { return };
        if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
        let _ = std::fs::write(p, self.serialized());
    }

    /// Drop one command and rewrite the file.
    pub fn remove(&mut self, cmd: &str) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.cmd != cmd);
        let changed = self.entries.len() != n;
        if changed { self.rewrite(); }
        changed
    }

    pub fn items(&self) -> &[HistItem] { &self.entries }

    /// Forget everything (and truncate the persisted file).
    pub fn clear(&mut self) {
        self.entries.clear();
        if let Some(p) = &self.path { let _ = std::fs::write(p, ""); }
    }

    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// Dropdown candidates for `input`: prefix matches first, then substring, then fuzzy
    /// (in-order characters); newest first inside each group, deduplicated, sensitive commands
    /// and the input itself excluded. Each item carries the matched char ranges for highlighting.
    pub fn matches(&self, input: &str, max: usize) -> Vec<Match> {
        rank_matches(self.entries.iter().rev().map(|e| e.cmd.as_str()), input, max)
    }

    /// Remaining text to suggest for `input`, newest matching entry wins.
    pub fn suggest(&self, input: &str) -> Option<String> {
        if input.trim().is_empty() { return None; }
        self.entries.iter().rev()
            .find(|e| e.cmd.len() > input.len() && e.cmd.starts_with(input))
            .map(|e| e.cmd[input.len()..].to_string())
    }
}

/// One dropdown candidate; `hl` are char-index ranges `[start, end)` of the matched text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match { pub cmd: String, pub hl: Vec<(usize, usize)> }

/// Rank `cmds` (newest first) against `input`. See [`History::matches`].
pub fn rank_matches<'a>(cmds: impl Iterator<Item = &'a str>, input: &str, max: usize) -> Vec<Match> {
    let q = input.trim_start();
    if q.trim().is_empty() || max == 0 { return vec![]; }
    let ql = q.to_lowercase();
    let qn = q.chars().count();
    let mut seen = std::collections::HashSet::new();
    let (mut pre, mut sub, mut fuz) = (vec![], vec![], vec![]);
    for cmd in cmds {
        if cmd == q || cmd.trim().is_empty() || !seen.insert(cmd) || is_sensitive(cmd) { continue; }
        if cmd.starts_with(q) { if pre.len() < max { pre.push(Match { cmd: cmd.into(), hl: vec![(0, qn)] }); } continue; }
        let lower = cmd.to_lowercase();
        // char-index search on the lowercased text (same char count for ASCII and most scripts)
        if lower.chars().count() == cmd.chars().count() {
            if let Some(b) = lower.find(&ql) {
                let st = lower[..b].chars().count();
                if sub.len() < max { sub.push(Match { cmd: cmd.into(), hl: vec![(st, st + qn)] }); }
                continue;
            }
            if fuz.len() < max && qn >= 2 {
                // fuzzy hits stay in the list but only contiguous runs of 2+ chars are highlighted
                if let Some(hl) = fuzzy_ranges(&lower, &ql) { fuz.push(Match { cmd: cmd.into(), hl: hl.into_iter().filter(|(a, b)| b - a >= 2).collect() }); }
            }
        }
        if pre.len() >= max { break; }
    }
    pre.into_iter().chain(sub).chain(fuz).take(max).collect()
}

/// In-order character match of `q` in `s` (both lowercased); merged char ranges.
fn fuzzy_ranges(s: &str, q: &str) -> Option<Vec<(usize, usize)>> {
    let mut want = q.chars().filter(|c| !c.is_whitespace()).peekable();
    let mut out: Vec<(usize, usize)> = vec![];
    for (i, ch) in s.chars().enumerate() {
        let Some(&w) = want.peek() else { break };
        if ch == w {
            want.next();
            match out.last_mut() { Some(r) if r.1 == i => r.1 = i + 1, _ => out.push((i, i + 1)) }
        }
    }
    want.peek().is_none().then_some(out)
}

/// Heuristic filter for commands that likely contain secrets. Such commands are neither stored
/// nor suggested.
/// Extra user regexes (settings: suggest.filter_patterns), checked in addition to the built-ins.
pub static EXTRA_FILTERS: std::sync::RwLock<Vec<regex::Regex>> = std::sync::RwLock::new(Vec::new());

pub fn set_extra_filters(patterns: &[String]) -> Vec<String> {
    let mut bad = vec![];
    let v = patterns.iter().filter_map(|p| regex::Regex::new(p).map_err(|_| bad.push(p.clone())).ok()).collect();
    *EXTRA_FILTERS.write().unwrap() = v;
    bad
}

pub fn is_sensitive(cmd: &str) -> bool {
    if EXTRA_FILTERS.read().unwrap().iter().any(|r| r.is_match(cmd)) { return true; }
    let l = cmd.to_ascii_lowercase();
    const WORDS: &[&str] = &[
        "password", "passwd=", "pwd=", "secret", "token", "api_key", "apikey", "access_key",
        "private_key", "authorization:", "bearer ", "sshpass", "aws_secret",
    ];
    if WORDS.iter().any(|w| l.contains(w)) { return true; }
    // mysql -pSECRET / mysql --password=...
    if (l.starts_with("mysql") || l.contains(" mysql ")) && l.split_whitespace().any(|t| t.starts_with("-p") && t.len() > 2) { return true; }
    // curl -u user:pass, URLs with inline credentials
    if l.split_whitespace().collect::<Vec<_>>().windows(2).any(|w| (w[0] == "-u" || w[0] == "--user") && w[1].contains(':')) { return true; }
    if l.contains("://") && l.split("://").nth(1).is_some_and(|r| r.split('/').next().is_some_and(|h| h.contains(':') && h.contains('@'))) { return true; }
    // export FOO_KEY=... style assignments
    l.split_whitespace().any(|t| t.contains('=') && {
        let k = t.split('=').next().unwrap_or("");
        k.ends_with("_key") || k.ends_with("_pass") || k.ends_with("_pw")
    })
}

/// How we currently know the input line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source { Osc133, Keystrokes }

/// Tracks the user's in-progress command line for one terminal pane.
#[derive(Debug, Default, Clone)]
pub struct InputTracker {
    buf: String,
    /// false once the user did something we can't model (history nav, Ctrl-R, Tab completion, cursor moves…)
    valid: bool,
}

pub enum Key<'a> { Text(&'a str), Backspace, Enter, Interrupt, KillLine, Other }

impl InputTracker {
    pub fn new() -> Self { Self { buf: String::new(), valid: true } }

    /// Feed a key the user typed. Returns the submitted command on Enter (if we knew it).
    pub fn key(&mut self, k: Key) -> Option<String> {
        match k {
            Key::Text(t) => { if t.chars().any(|c| c.is_control()) { self.valid = false } else { self.buf.push_str(t) } None }
            Key::Backspace => { self.buf.pop(); None }
            Key::KillLine | Key::Interrupt => { self.reset(); None }
            Key::Enter => { let out = self.valid.then(|| self.buf.clone()); self.reset(); out }
            Key::Other => { self.valid = false; None }
        }
    }
    pub fn reset(&mut self) { self.buf.clear(); self.valid = true; }
    pub fn buffer(&self) -> Option<&str> { self.valid.then_some(self.buf.as_str()) }

    /// Decide the current input given the screen text left of the cursor on the cursor row.
    /// `prompt_col` is the OSC 133;B column on this row, if shell integration reported one.
    /// Returns None when we are not confident (e.g. typed text is not echoed => password prompt).
    pub fn current_input(&self, before_cursor: &str, prompt_col: Option<usize>) -> Option<(String, Source)> {
        if let Some(col) = prompt_col {
            let s: String = before_cursor.chars().skip(col).collect();
            return Some((s, Source::Osc133));
        }
        let b = self.buffer()?;
        if b.is_empty() || !before_cursor.ends_with(b) { return None; }
        Some((b.to_string(), Source::Keystrokes))
    }
}

impl InputTracker {
    /// Like [`current_input`](Self::current_input) but for the moment Enter is pressed: the echo of
    /// the last few keystrokes may still be in flight (fast typing over SSH), so accept the buffer if
    /// the screen already shows at least half of it (and at least one char). Unechoed input
    /// (passwords) shows nothing and is therefore never recorded.
    pub fn submitted_input(&self, before_cursor: &str, prompt_col: Option<usize>) -> Option<String> {
        if let Some((s, Source::Osc133)) = self.current_input(before_cursor, prompt_col) {
            // with OSC 133 the screen is authoritative, but pending echo may truncate it: prefer the
            // keystroke buffer when it extends what is on screen
            if let Some(b) = self.buffer() { if b.starts_with(s.as_str()) && !b.is_empty() { return Some(b.to_string()); } }
            return Some(s);
        }
        let b = self.buffer()?;
        let n = b.chars().count();
        let shown = (1..=n).rev().find(|&k| {
            let pre: String = b.chars().take(k).collect();
            before_cursor.ends_with(&pre)
        })?;
        (shown * 2 >= n).then(|| b.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zsh_history_and_persistence() {
        let mut h = History::new();
        h.extend_from_bash_history(": 1700000000:0;git status\n: 1700000001:0;echo a \\\nb\nls -la\n");
        assert_eq!(h.suggest("git s").as_deref(), Some("tatus"));
        assert_eq!(h.suggest("ls -").as_deref(), Some("la"));
        assert!(h.suggest("b").is_none());
        let p = std::env::temp_dir().join(format!("as-hist-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut a = History::open(&p);
        assert!(a.add("cargo build --release"));
        assert!(!a.add("export TOKEN=abc"));
        let b = History::open(&p);
        assert_eq!(b.suggest("cargo b").as_deref(), Some("uild --release"));
        assert_eq!(b.len(), 1);
        let _ = std::fs::remove_file(&p);
    }
    #[test]
    fn submitted_tolerates_pending_echo_but_not_passwords() {
        let mut t = InputTracker::new();
        for ch in ["l", "s", " ", "-", "l", "a"] { t.key(Key::Text(ch)); }
        assert_eq!(t.submitted_input("$ ls -l", None).as_deref(), Some("ls -la"));
        assert_eq!(t.submitted_input("$ l", None), None, "too little echoed");
        assert_eq!(t.submitted_input("Password: ", None), None);
    }


    #[test]
    fn dropdown_ranking() {
        let cmds = ["git status", "ls -la", "git stash pop", "cat .gitignore", "grep it x", "git status", "export API_KEY=1"];
        let m = rank_matches(cmds.iter().copied(), "git", 10);
        let names: Vec<&str> = m.iter().map(|x| x.cmd.as_str()).collect();
        // prefix (newest first, deduped) before substring before fuzzy
        assert_eq!(names, ["git status", "git stash pop", "cat .gitignore", "grep it x"]);
        assert_eq!(m[0].hl, vec![(0, 3)]);
        assert_eq!(m[2].hl, vec![(5, 8)]);
        assert_eq!(m[3].hl, vec![(5, 7)]);
        // exact input excluded, case-insensitive substring, sensitive dropped, max respected
        assert!(rank_matches(["ls"].iter().copied(), "ls", 10).is_empty());
        assert_eq!(rank_matches(["Make build"].iter().copied(), "build", 10)[0].hl, vec![(5, 10)]);
        assert!(rank_matches(cmds.iter().copied(), "api", 10).iter().all(|x| !x.cmd.contains("API_KEY")));
        assert_eq!(rank_matches(cmds.iter().copied(), "s", 2).len(), 2);
        assert!(rank_matches(cmds.iter().copied(), "  ", 10).is_empty());
    }

    #[test]
    fn suggests_newest_prefix_match() {
        let mut h = History::new();
        h.add("git status");
        h.add("git stash pop");
        h.add("ls -la");
        assert_eq!(h.suggest("git st").as_deref(), Some("ash pop"));
        h.add("git status");
        assert_eq!(h.suggest("git st").as_deref(), Some("atus"));
        assert_eq!(h.suggest("ls -la"), None, "exact match has nothing left");
        assert_eq!(h.suggest(""), None);
        assert_eq!(h.suggest("   "), None);
        assert_eq!(h.len(), 3, "dedup");
    }

    #[test]
    fn filters_sensitive_and_ignorespace() {
        let mut h = History::new();
        assert!(!h.add(" rm -rf secret-dir"));
        assert!(!h.add("export GITHUB_TOKEN=abc"));
        assert!(!h.add("mysql -uroot -phunter2"));
        assert!(!h.add("curl -u bob:pw https://x"));
        assert!(!h.add("git clone https://bob:pw@example.com/r.git"));
        assert!(!h.add("export DB_PASS=x"));
        assert!(h.add("mysql -uroot -p"));
        assert!(h.add("curl https://example.com"));
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn bash_history_import_skips_timestamps() {
        let mut h = History::new();
        h.extend_from_bash_history("#1700000000\nls\ncd /tmp\n");
        assert_eq!(h.len(), 2);
        assert_eq!(h.suggest("cd").as_deref(), Some(" /tmp"));
    }

    #[test]
    fn timestamped_lines_round_trip_and_do_not_leak_into_suggestions() {
        let p = std::env::temp_dir().join(format!("as-hist-ts-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, "git status\n1700000000\tls -la\n").unwrap();
        let h = History::open(&p);
        assert_eq!(h.len(), 2);
        assert!(h.items().iter().any(|e| e.cmd == "git status" && e.ts.is_none()));
        assert!(h.items().iter().any(|e| e.cmd == "ls -la" && e.ts == Some(1_700_000_000)));
        assert_eq!(h.suggest("ls").as_deref(), Some(" -la"));
        assert!(h.suggest("170").is_none(), "the timestamp is not part of the command");
        let mut w = History::open(&p);
        assert!(w.add("pwd"));
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.lines().any(|l| l.ends_with("\tpwd") || l.contains("\tpwd")));
        assert!(w.remove("pwd"));
        assert!(!std::fs::read_to_string(&p).unwrap().contains("pwd"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn tracker_keystrokes_and_echo_check() {
        let mut t = InputTracker::new();
        t.key(Key::Text("g")); t.key(Key::Text("ix")); t.key(Key::Backspace); t.key(Key::Text("t"));
        assert_eq!(t.buffer(), Some("git"));
        // echoed on screen after a prompt
        assert_eq!(t.current_input("user@h:~$ git", None).map(|x| x.0).as_deref(), Some("git"));
        // not echoed (password prompt) => no suggestion
        assert_eq!(t.current_input("[sudo] password for user: ", None), None);
        assert_eq!(t.key(Key::Enter).as_deref(), Some("git"));
        assert_eq!(t.buffer(), Some(""));
    }

    #[test]
    fn tracker_invalidated_by_unknown_keys() {
        let mut t = InputTracker::new();
        t.key(Key::Text("ls"));
        t.key(Key::Other); // e.g. Up arrow recalled history
        assert_eq!(t.buffer(), None);
        assert_eq!(t.current_input("$ cargo build", None), None);
        assert_eq!(t.key(Key::Enter), None, "unknown command line is not recorded");
        assert_eq!(t.buffer(), Some(""), "valid again after Enter");
    }

    #[test]
    fn osc133_mark_wins() {
        let t = InputTracker::new();
        let (s, src) = t.current_input("$ cargo bu", Some(2)).unwrap();
        assert_eq!((s.as_str(), src), ("cargo bu", Source::Osc133));
    }
}
