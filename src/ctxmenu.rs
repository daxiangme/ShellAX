//! Right-click context menu (PRD 5.20): object classification, detection of paths / URLs /
//! IPs in terminal output, POSIX shell quoting, built-in + user-defined menu items with exact
//! command previews, per-host tool detection with install hints. Pure logic; the views render it.
//!
//! Command items are only ever *inserted* into the terminal input line (no newline); executing
//! them is the user's Enter and goes through the normal permission engine.
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;

use crate::i18n::{t, tf};

// ---------------------------------------------------------------- quoting
fn safe_char(c: char) -> bool { c.is_ascii_alphanumeric() || "_./=:@%+,-".contains(c) }

/// POSIX shell quoting: bare when only safe ASCII characters, else single quotes with `'\''`.
/// Spaces, quotes, `$`, backticks, CJK and any other Unicode end up inside single quotes.
pub fn quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(safe_char) && !s.starts_with('-') && !s.starts_with('=') { return s.to_string(); }
    format!("'{}'", s.replace('\'', r"'\''"))
}
/// Quote a path argument: keeps a leading `~/` unquoted (so the shell still expands it) and
/// prefixes `./` to relative paths that start with `-` (never parsed as an option).
pub fn quote_path(p: &str) -> String {
    if p == "~" { return "~".into(); }
    if let Some(rest) = p.strip_prefix("~/") { return format!("~/{}", if rest.is_empty() { String::new() } else { quote(rest) }); }
    if p.starts_with('-') { return quote(&format!("./{p}")); }
    quote(p)
}

// ---------------------------------------------------------------- paths
pub fn file_name(p: &str) -> &str { let t = p.trim_end_matches('/'); t.rsplit('/').next().unwrap_or(t) }
pub fn parent_dir(p: &str) -> String {
    let t = p.trim_end_matches('/');
    match t.rfind('/') { Some(0) => "/".into(), Some(i) => t[..i].to_string(), None => ".".into() }
}
fn join(dir: &str, name: &str) -> String { if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") } }

/// Normalise `a/./b/../c` (purely lexical; `..` at the root stays at the root).
pub fn normalize(p: &str) -> String {
    let abs = p.starts_with('/');
    let mut out: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg { "" | "." => {}, ".." => { if out.last().is_some_and(|s| *s != "..") { out.pop(); } else if !abs { out.push(".."); } }, s => out.push(s) }
    }
    let j = out.join("/");
    if abs { format!("/{j}") } else if j.is_empty() { ".".into() } else { j }
}

/// Resolve a path seen in terminal output: absolute stays, `~/` uses `home`, relative uses the
/// shell's cwd. Returns (path, resolved?) — unresolved relative paths are kept as typed, which
/// still works because the command is inserted into the same shell (same cwd).
pub fn resolve(path: &str, cwd: Option<&str>, home: Option<&str>) -> (String, bool) {
    if path.starts_with('/') { return (normalize(path), true); }
    if path == "~" || path.starts_with("~/") {
        return match home { Some(h) => (normalize(&format!("{h}{}", &path[1..])), true), None => (path.to_string(), true) };
    }
    match cwd.filter(|c| c.starts_with('/') || c == &"~" || c.starts_with("~/")) { Some(c) => (normalize(&join(c, path)), true), None => (path.to_string(), false) }
}

/// Fallback cwd from a classic `user@host:dir$ ` prompt on the cursor line (`~` kept as is).
pub fn prompt_cwd(line: &str) -> Option<String> {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    let re = R.get_or_init(|| regex::Regex::new(r"[\w.-]+@[\w.-]+:\s*([~/][^$#]*?)\s*[$#]").unwrap());
    re.captures_iter(line).last().map(|c| c[1].to_string())
}

/// Parse an OSC 7 payload (`file://host/path`, percent-encoded) into the path.
pub fn parse_osc7(payload: &str) -> Option<String> {
    let rest = payload.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let raw = &rest[slash..];
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]); i += 1;
    }
    let path = String::from_utf8(out).ok()?;
    Some(windows_drive_path(&path).unwrap_or(path))
}

/// `/C:/Users/x`（OSC 7 里的 Windows 路径）→ `C:\Users\x`；其他路径返回 None。
fn windows_drive_path(p: &str) -> Option<String> {
    let b = p.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' && (b.len() == 3 || b[3] == b'/') {
        Some(p[1..].replace('/', "\\"))
    } else { None }
}

/// OSC 9;9（Windows Terminal / ConPTY 约定，PowerShell 与 cmd 提示符输出）的负载：`"C:\Users\x"` 或不带引号。
pub fn parse_osc9_cwd(payload: &str) -> Option<String> {
    let p = payload.trim().trim_matches('"').trim();
    if p.is_empty() { None } else { Some(p.to_string()) }
}

// ---------------------------------------------------------------- classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArcKind { Zip, Tar, TarGz, TarXz, TarBz2, SevenZ, Rar, Gz }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Archive(ArcKind), Dir, Text, Script, Executable, Log, File }

impl Kind {
    pub fn type_name(&self) -> &'static str {
        match self { Kind::Archive(_) => "archive", Kind::Dir => "dir", Kind::Text => "text", Kind::Script => "script", Kind::Executable => "executable", Kind::Log => "log", Kind::File => "file" }
    }
}
pub const TYPE_NAMES: &[&str] = &["archive", "dir", "text", "script", "executable", "log", "file", "url", "ip", "selection"];

/// Archive kind and the extension it was recognised by (e.g. "tar.gz").
pub fn archive_kind(name: &str) -> Option<(ArcKind, &'static str)> {
    let n = name.to_lowercase();
    const T: &[(&str, ArcKind)] = &[(".tar.gz", ArcKind::TarGz), (".tgz", ArcKind::TarGz), (".tar.xz", ArcKind::TarXz), (".txz", ArcKind::TarXz),
        (".tar.bz2", ArcKind::TarBz2), (".tbz2", ArcKind::TarBz2), (".tbz", ArcKind::TarBz2), (".tar", ArcKind::Tar), (".zip", ArcKind::Zip),
        (".7z", ArcKind::SevenZ), (".rar", ArcKind::Rar), (".gz", ArcKind::Gz)];
    T.iter().find(|(e, _)| n.ends_with(e) && n.len() > e.len()).map(|(e, k)| (*k, &e[1..]))
}
/// Extension used for {ext}/{stem}: compound archive extensions count as one ("a.tar.gz" → "tar.gz").
pub fn ext_of(name: &str) -> String {
    if let Some((_, e)) = archive_kind(name) { return name[name.len() - e.len()..].to_string(); }
    match name.rfind('.') { Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_string(), _ => String::new() }
}
pub fn stem_of(name: &str) -> String {
    let e = ext_of(name);
    if e.is_empty() { name.to_string() } else { name[..name.len() - e.len() - 1].to_string() }
}

const TEXT_EXT: &[&str] = &["txt", "md", "markdown", "rst", "conf", "cfg", "ini", "toml", "yaml", "yml", "json", "xml", "env", "csv", "tsv", "properties",
    "service", "socket", "timer", "html", "css", "c", "h", "cpp", "hpp", "rs", "go", "java", "kt", "ts", "tsx", "jsx", "sql", "tf", "nginx", "list", "repo", "pem", "crt", "pub", "diff", "patch"];
const TEXT_NAMES: &[&str] = &["makefile", "dockerfile", "readme", "license", "hosts", "fstab", "crontab", "authorized_keys", "known_hosts", ".bashrc", ".zshrc", ".profile", ".bash_profile", ".gitignore", ".vimrc", ".tmux.conf"];
const SCRIPT_EXT: &[(&str, &str)] = &[("sh", "bash"), ("bash", "bash"), ("zsh", "zsh"), ("py", "python3"), ("pl", "perl"), ("rb", "ruby"), ("js", "node"), ("mjs", "node"), ("php", "php"), ("lua", "lua"), ("fish", "fish")];

pub fn interpreter(name: &str) -> Option<&'static str> {
    let e = ext_of(name).to_lowercase();
    SCRIPT_EXT.iter().find(|(x, _)| *x == e).map(|(_, i)| *i)
}
fn is_log(n: &str) -> bool {
    n.ends_with(".log") || regex_log().is_match(n) || (n.contains("log") && (n.ends_with(".out") || n.ends_with(".txt") || n.ends_with(".err")))
}
fn regex_log() -> &'static regex::Regex { static R: OnceLock<regex::Regex> = OnceLock::new(); R.get_or_init(|| regex::Regex::new(r"\.log\.\d+$").unwrap()) }

/// All kinds that apply, most specific first. `mode` = Unix permission bits (0 = unknown).
pub fn kinds(name: &str, is_dir: bool, mode: u32) -> Vec<Kind> {
    if is_dir { return vec![Kind::Dir]; }
    let n = name.to_lowercase();
    let mut v = vec![];
    if let Some((a, _)) = archive_kind(&n) { v.push(Kind::Archive(a)); }
    if is_log(&n) { v.push(Kind::Log); }
    if interpreter(&n).is_some() { v.push(Kind::Script); }
    if mode & 0o111 != 0 { v.push(Kind::Executable); }
    let e = ext_of(&n);
    if v.iter().all(|k| !matches!(k, Kind::Archive(_))) && (TEXT_EXT.contains(&e.as_str()) || TEXT_NAMES.contains(&n.as_str()) || matches!(v.first(), Some(Kind::Log | Kind::Script))) { v.push(Kind::Text); }
    if v.is_empty() { v.push(Kind::File); }
    v
}

// ---------------------------------------------------------------- detection in terminal output
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found { Path { path: String, line: Option<u32> }, Url(String), HostPort { host: String, port: Option<u16> } }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected { pub found: Found, pub start: usize, pub end: usize }

fn detectors() -> &'static [(u8, regex::Regex)] {
    static R: OnceLock<Vec<(u8, regex::Regex)>> = OnceLock::new();
    R.get_or_init(|| vec![
        (b'u', regex::Regex::new(r#"(?i)\b(?:https?|ftp|ssh|sftp)://[^\s<>"'`]+"#).unwrap()),
        (b'6', regex::Regex::new(r"\[([0-9A-Fa-f:]+:[0-9A-Fa-f:.]*)\](?::(\d{1,5}))?").unwrap()),
        (b'4', regex::Regex::new(r"\b((?:25[0-5]|2[0-4]\d|1?\d?\d)(?:\.(?:25[0-5]|2[0-4]\d|1?\d?\d)){3})(?::(\d{1,5}))?\b").unwrap()),
        (b'l', regex::Regex::new(r"\b(localhost):(\d{1,5})\b").unwrap()),
        // absolute, ~/, ./, ../ paths (CJK allowed), optional :line
        (b'p', regex::Regex::new(r#"(?:(?:~|\.{1,2})?/|[\p{L}\p{N}_.\-]*\p{L}[\p{L}\p{N}_.\-]*/)[^\s'"`<>|;:,()\[\]{}]*(?::\d+){0,2}|~(?:\s|$)"#).unwrap()),
        // bare file names with an extension: build.log, 配置.yaml, a.tar.gz (optional :line)
        (b'f', regex::Regex::new(r"[\p{L}\p{N}_][\p{L}\p{N}_\-.+]*\.[A-Za-z][A-Za-z0-9]{0,7}(?::\d+){0,2}").unwrap()),
    ])
}

/// Find the path / URL / host:port under char column `col` of `line`.
/// `start`/`end` are char indices (end exclusive).
pub fn detect_at(line: &str, col: usize) -> Option<Detected> {
    let char_idx = |b: usize| line[..b].chars().count();
    for (kind, re) in detectors() {
        for c in re.captures_iter(line) {
            let m = c.get(0).unwrap();
            let mut text = m.as_str();
            // trailing punctuation that is almost never part of the object
            while let Some(last) = text.chars().last() { if ".,;:!?)]}'\"。，".contains(last) && text.len() > 1 { text = &text[..text.len() - last.len_utf8()]; } else { break; } }
            let (s, e) = (char_idx(m.start()), char_idx(m.start()) + text.chars().count());
            if col < s || col >= e { continue; }
            let num = |i: usize| c.get(i).and_then(|g| g.as_str().parse::<u32>().ok());
            let found = match kind {
                b'u' => Found::Url(text.to_string()),
                b'6' => Found::HostPort { host: c[1].to_string(), port: num(2).and_then(|p| u16::try_from(p).ok()) },
                b'4' => {
                    // "1.2.3.4" inside a longer dotted token (version strings) is not an IP
                    let before = line[..m.start()].chars().last();
                    let after = line[m.end()..].chars().next();
                    if before.is_some_and(|b| b == '.' || b.is_ascii_alphanumeric()) || after.is_some_and(|a| a == '.' && line[m.end() + 1..].starts_with(|x: char| x.is_ascii_digit())) { continue; }
                    Found::HostPort { host: c[1].to_string(), port: num(2).and_then(|p| u16::try_from(p).ok()) }
                }
                b'l' => Found::HostPort { host: "localhost".into(), port: num(2).and_then(|p| u16::try_from(p).ok()) },
                _ => {
                    let t = text.trim_end();
                    let (p, ln) = match regex_line().captures(t) { Some(lc) => (lc[1].to_string(), lc[2].parse().ok()), None => (t.to_string(), None) };
                    if *kind == b'f' && p.chars().all(|ch| ch.is_ascii_digit() || ch == '.') { continue; }
                    Found::Path { path: p, line: ln }
                }
            };
            return Some(Detected { found, start: s, end: e });
        }
    }
    None
}
fn regex_line() -> &'static regex::Regex { static R: OnceLock<regex::Regex> = OnceLock::new(); R.get_or_init(|| regex::Regex::new(r"^(.*?):(\d+)(?::\d+)?$").unwrap()) }

// ---------------------------------------------------------------- tools
pub const PROBE_TOOLS: &[&str] = &["unzip", "zip", "tar", "gzip", "xz", "bzip2", "7z", "7za", "7zz", "unrar", "less", "tail", "du", "grep", "diff",
    "bash", "zsh", "python3", "perl", "ruby", "node", "php", "lua", "fish", "curl", "nc", "ping", "ssh"];

/// Read-only probe: which tools exist, plus OS identification.
pub fn probe_command() -> String {
    format!("for t in {}; do command -v \"$t\" >/dev/null 2>&1 && printf 'T:%s\\n' \"$t\"; done; printf 'U:%s\\n' \"$(uname -s 2>/dev/null)\"; \
        [ -r /etc/os-release ] && sed -n 's/^ID=//p;s/^ID_LIKE=//p' /etc/os-release | tr -d '\"' | sed 's/^/O:/'; true", PROBE_TOOLS.join(" "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pm { Apt, Yum, Dnf, Apk, Pacman, Zypper, Brew, Unknown }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools { pub have: BTreeSet<String>, pub os: String, pub pm: Pm }
impl Tools {
    pub fn has(&self, t: &str) -> bool { self.have.contains(t) }
}
pub fn parse_probe(out: &str) -> Tools {
    let mut have = BTreeSet::new();
    let (mut uname, mut ids) = (String::new(), vec![]);
    for l in out.lines() {
        let l = l.trim();
        if let Some(t) = l.strip_prefix("T:") { have.insert(t.to_string()); if matches!(t, "7za" | "7zz") { have.insert("7z".into()); } }
        else if let Some(u) = l.strip_prefix("U:") { uname = u.to_string(); }
        else if let Some(o) = l.strip_prefix("O:") { ids.extend(o.split_whitespace().map(|s| s.to_lowercase())); }
    }
    let first = ids.first().cloned().unwrap_or_default();
    let pm = if uname == "Darwin" { Pm::Brew } else {
        ids.iter().find_map(|id| match id.as_str() {
            "debian" | "ubuntu" | "raspbian" | "linuxmint" | "pop" => Some(Pm::Apt),
            "fedora" => Some(Pm::Dnf),
            "rhel" | "centos" | "rocky" | "almalinux" | "amzn" | "ol" => Some(Pm::Yum),
            "alpine" => Some(Pm::Apk),
            "arch" | "manjaro" | "endeavouros" => Some(Pm::Pacman),
            s if s.starts_with("opensuse") || s == "sles" || s == "suse" => Some(Pm::Zypper),
            _ => None,
        }).unwrap_or(Pm::Unknown)
    };
    let os = if uname == "Darwin" { "macos".into() } else if first.is_empty() { uname.to_lowercase() } else { first };
    Tools { have, os, pm }
}
fn package(tool: &str, pm: Pm) -> String {
    let p = match (tool, pm) {
        ("7z", Pm::Apt) => "p7zip-full", ("7z", Pm::Yum | Pm::Dnf) => "p7zip p7zip-plugins", ("7z", Pm::Apk) => "7zip", ("7z", Pm::Brew) => "sevenzip", ("7z", _) => "p7zip",
        ("unrar", Pm::Brew) => "--cask rar",
        ("xz", Pm::Apt) => "xz-utils",
        ("nc", Pm::Apt | Pm::Apk) => "netcat-openbsd", ("nc", Pm::Yum | Pm::Dnf) => "nmap-ncat", ("nc", Pm::Pacman) => "openbsd-netcat", ("nc", _) => "netcat",
        ("ping", Pm::Apt) => "iputils-ping", ("ping", Pm::Yum | Pm::Dnf | Pm::Pacman) => "iputils",
        ("python3", Pm::Brew) => "python", ("python3", Pm::Pacman) => "python",
        ("node", Pm::Apt | Pm::Yum | Pm::Dnf | Pm::Apk) => "nodejs",
        ("ssh", Pm::Apt) => "openssh-client", ("ssh", Pm::Yum | Pm::Dnf) => "openssh-clients", ("ssh", _) => "openssh",
        (t, _) => t,
    };
    p.to_string()
}
/// Install command suggestion for a missing tool on a host with this package manager.
pub fn install_hint(tool: &str, pm: Pm) -> String {
    let p = package(tool, pm);
    match pm {
        Pm::Apt => format!("sudo apt install -y {p}"), Pm::Yum => format!("sudo yum install -y {p}"), Pm::Dnf => format!("sudo dnf install -y {p}"),
        Pm::Apk => format!("sudo apk add {p}"), Pm::Pacman => format!("sudo pacman -S --needed {p}"), Pm::Zypper => format!("sudo zypper install -y {p}"),
        Pm::Brew => format!("brew install {p}"), Pm::Unknown => tf("cm.install_unknown", &[("pkg", &p)]),
    }
}

pub const TOOLS_TTL: Duration = Duration::from_secs(600);
type ToolCache = Mutex<HashMap<String, (Instant, Option<Tools>)>>;
fn tool_cache() -> &'static ToolCache { static C: OnceLock<ToolCache> = OnceLock::new(); C.get_or_init(Default::default) }
/// Cached tools for a host key; `None` = not probed yet / expired / probe in flight.
pub fn cached_tools(key: &str) -> Option<Tools> {
    tool_cache().lock().unwrap().get(key).filter(|(at, _)| at.elapsed() < TOOLS_TTL).and_then(|(_, t)| t.clone())
}
/// Mark a probe as started; returns false when one is already running or cached.
pub fn begin_probe(key: &str) -> bool {
    let mut c = tool_cache().lock().unwrap();
    if c.get(key).is_some_and(|(at, t)| at.elapsed() < if t.is_some() { TOOLS_TTL } else { Duration::from_secs(30) }) { return false; }
    c.insert(key.to_string(), (Instant::now(), None));
    true
}
pub fn store_tools(key: &str, t: Tools) { tool_cache().lock().unwrap().insert(key.to_string(), (Instant::now(), Some(t))); }
pub fn clear_tool_cache() { tool_cache().lock().unwrap().clear(); }

/// Probe the local machine (blocking).
pub fn probe_local() -> Tools {
    let out = std::process::Command::new("sh").arg("-c").arg(probe_command()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    parse_probe(&out)
}

// ---------------------------------------------------------------- user-defined items
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct CustomItem {
    pub label: String,
    #[serde(default)] pub label_en: Option<String>,
    pub command: String,
    #[serde(default)] pub extensions: Vec<String>,
    #[serde(default)] pub glob: Vec<String>,
    #[serde(default)] pub types: Vec<String>,
    #[serde(default)] pub hosts: Vec<String>,
    #[serde(default)] pub tools: Vec<String>,
}
pub const PLACEHOLDERS: &[&str] = &["path", "name", "dir", "stem", "ext", "host"];

/// Placeholder names used by a template; Err on an unterminated `{`.
pub fn placeholders(tpl: &str) -> Result<Vec<String>, String> {
    let mut v = vec![];
    let mut it = tpl.char_indices().peekable();
    while let Some((i, ch)) = it.next() {
        match ch {
            '{' if it.peek().map(|x| x.1) == Some('{') => { it.next(); }
            '}' if it.peek().map(|x| x.1) == Some('}') => { it.next(); }
            '{' => { let rest = &tpl[i + 1..]; let Some(e) = rest.find('}') else { return Err(tf("cm.err_brace", &[("tpl", &tpl)])) }; v.push(rest[..e].to_string()); for _ in 0..=rest[..e].chars().count() { it.next(); } }
            _ => {}
        }
    }
    Ok(v)
}
/// Expand `{path} {name} {dir} {stem} {ext} {host}`; every value is shell-quoted. `{{` / `}}` are literal braces.
pub fn expand(tpl: &str, vars: &HashMap<&str, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut it = tpl.char_indices().peekable();
    while let Some((i, ch)) = it.next() {
        match ch {
            '{' if it.peek().map(|x| x.1) == Some('{') => { it.next(); out.push('{'); }
            '}' if it.peek().map(|x| x.1) == Some('}') => { it.next(); out.push('}'); }
            '{' => {
                let rest = &tpl[i + 1..];
                let Some(e) = rest.find('}') else { return Err(tf("cm.err_brace", &[("tpl", &tpl)])) };
                let key = &rest[..e];
                let Some(v) = vars.get(key) else { return Err(tf("cm.err_placeholder", &[("name", &format!("{{{key}}}"))])) };
                out.push_str(v);
                for _ in 0..=key.chars().count() { it.next(); }
            }
            c => out.push(c),
        }
    }
    Ok(out)
}
/// Quoted placeholder values for one path. `paths` (multi-select) fills {path} with all of them.
pub fn vars_for(paths: &[String], host: &str) -> HashMap<&'static str, String> {
    let first = paths.first().cloned().unwrap_or_default();
    let name = file_name(&first).to_string();
    let mut m = HashMap::new();
    m.insert("path", paths.iter().map(|p| quote_path(p)).collect::<Vec<_>>().join(" "));
    m.insert("dir", quote_path(&parent_dir(&first)));
    m.insert("stem", quote(&stem_of(&name)));
    m.insert("ext", quote(&ext_of(&name)));
    m.insert("name", quote(&name));
    m.insert("host", quote(host));
    m
}

/// Parse context_menu.toml: `[[item]]` tables. Bad items are reported and skipped; good ones load.
pub fn parse_custom(src: &str) -> (Vec<CustomItem>, Vec<String>) {
    let table: toml::Table = match toml::from_str(src) { Ok(t) => t, Err(e) => return (vec![], vec![tf("cm.err_toml", &[("e", &e.message().trim())])]) };
    let (mut ok, mut errs) = (vec![], vec![]);
    for k in table.keys().filter(|k| *k != "item") { errs.push(tf("cm.err_unknown_key", &[("key", k)])); }
    let Some(items) = table.get("item") else { return (ok, errs) };
    let Some(arr) = items.as_array() else { errs.push(t("cm.err_item_array").to_string()); return (ok, errs) };
    for (i, v) in arr.iter().enumerate() {
        let n = i + 1;
        let it: CustomItem = match v.clone().try_into() { Ok(x) => x, Err(e) => { errs.push(tf("cm.err_item", &[("n", &n), ("e", &e.message().trim())])); continue } };
        if let Err(e) = validate(&it) { errs.push(tf("cm.err_item", &[("n", &n), ("e", &e)])); continue; }
        ok.push(it);
    }
    (ok, errs)
}
pub fn validate(it: &CustomItem) -> Result<(), String> {
    if it.label.trim().is_empty() { return Err(t("cm.err_label").into()); }
    if it.command.trim().is_empty() { return Err(t("cm.err_command").into()); }
    if it.command.contains(['\n', '\r']) { return Err(t("cm.err_newline").into()); }
    for p in placeholders(&it.command)? { if !PLACEHOLDERS.contains(&p.as_str()) { return Err(tf("cm.err_placeholder", &[("name", &format!("{{{p}}}"))])); } }
    for ty in &it.types { if !TYPE_NAMES.contains(&ty.as_str()) { return Err(tf("cm.err_type", &[("name", ty)])); } }
    Ok(())
}

/// context_menu.toml next to config.toml (SHELLAX_CONFIG's directory, else ~/.config/shellax).
pub fn custom_path() -> PathBuf {
    if let Ok(p) = std::env::var("SHELLAX_CONTEXT_MENU") { return p.into(); }
    let cfg = std::env::var("SHELLAX_CONFIG").map(PathBuf::from).unwrap_or_else(|_| crate::platform::config_dir().join("config.toml"));
    cfg.parent().unwrap_or(Path::new(".")).join("context_menu.toml")
}
type Loaded = (Option<SystemTime>, Arc<Vec<CustomItem>>, Arc<Vec<String>>);
/// Hot reload: re-parse whenever the file's mtime changes (checked on every call).
pub fn load_custom() -> (Arc<Vec<CustomItem>>, Arc<Vec<String>>) {
    static C: OnceLock<Mutex<Option<(PathBuf, Loaded)>>> = OnceLock::new();
    let path = custom_path();
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut g = C.get_or_init(Default::default).lock().unwrap();
    if let Some((p, (m, items, errs))) = g.as_ref() { if *p == path && *m == mtime { return (items.clone(), errs.clone()); } }
    let (items, errs) = match std::fs::read_to_string(&path) { Ok(s) => parse_custom(&s), Err(_) => (vec![], vec![]) };
    let (items, errs) = (Arc::new(items), Arc::new(errs));
    *g = Some((path, (mtime, items.clone(), errs.clone())));
    (items, errs)
}
pub const CUSTOM_EXAMPLE: &str = r#"# shellax 右键菜单自定义项 / custom context-menu items (hot-reloaded)
# 占位符 placeholders: {path} {name} {dir} {stem} {ext} {host} (auto shell-quoted)
# types: archive dir text script executable log file url ip selection

[[item]]
label = "用 jq 美化"
label_en = "Pretty-print with jq"
command = "jq . {path} | less"
extensions = ["json"]
tools = ["jq"]
"#;

// ---------------------------------------------------------------- menu model
/// One file-system object the menu is about.
#[derive(Debug, Clone, PartialEq)]
pub struct FileObj { pub path: String, pub is_dir: bool, pub mode: u32 }

#[derive(Debug, Clone, PartialEq)]
pub enum Target { Files(Vec<FileObj>), Url(String), HostPort { host: String, port: Option<u16> }, Selection(String), Blank { paste: Option<String> } }

/// Where the menu was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place { Terminal { remote: bool }, LocalPanel, RemotePanel }

#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    /// Insert into the terminal input line, never executed here.
    Insert(String),
    Copy(String),
    OpenEditor(String),
    Download(Vec<String>),
    Upload(Vec<String>),
    /// New tab on the same host, then insert `cd -- dir` (not executed).
    CdNewTab(String),
    OpenUrl(String),
    Search(String),
    AiExplain(String),
    PasteInput(String),
    /// Point the open AI assistant at the terminal that opened this menu.
    UseAiContext,
    /// Open a built-in tool (id) with the selection as its input.
    OpenTool(String, String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub act: Act,
    /// Tools the command needs that the host does not have → greyed out.
    pub missing: Vec<String>,
    /// Install suggestion for the first missing tool.
    pub hint: Option<String>,
    /// Tools not known yet (probe still running).
    pub unknown: bool,
    pub custom: bool,
    pub group: u8,
}
impl MenuItem {
    pub fn enabled(&self) -> bool { self.missing.is_empty() }
    /// Exact command preview for insert items.
    pub fn preview(&self) -> Option<&str> { match &self.act { Act::Insert(c) => Some(c), Act::CdNewTab(_) => None, _ => None } }
}

pub struct Ctx<'a> {
    /// Host for {host} and host patterns ("local" for the local machine).
    pub host: &'a str,
    pub place: Place,
    pub tools: Option<&'a Tools>,
    pub custom: &'a [CustomItem],
    pub lang_en: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Menu { pub header: String, pub items: Vec<MenuItem> }

struct B<'a> { ctx: &'a Ctx<'a>, items: Vec<MenuItem>, group: u8 }
impl B<'_> {
    fn push(&mut self, id: &str, label: String, act: Act, tools: &[&str]) {
        let (mut missing, mut unknown) = (vec![], false);
        match self.ctx.tools {
            Some(t) => missing = tools.iter().filter(|x| !t.has(x)).map(|s| s.to_string()).collect(),
            None => unknown = !tools.is_empty(),
        }
        let hint = missing.first().map(|m| install_hint(m, self.ctx.tools.map(|t| t.pm).unwrap_or(Pm::Unknown)));
        self.items.push(MenuItem { id: id.into(), label, act, missing, hint, unknown, custom: false, group: self.group });
    }
    fn ins(&mut self, id: &str, label: &str, cmd: String, tools: &[&str]) { self.push(id, label.to_string(), Act::Insert(cmd), tools); }
    fn sep(&mut self) { if self.items.last().is_some_and(|i| i.group == self.group) { self.group += 1; } }
}

fn archive_items(b: &mut B, a: ArcKind, path: &str) {
    let name = file_name(path);
    let dir = parent_dir(path);
    let to = join(&dir, &stem_of(name));
    let (q, qd, qt) = (quote_path(path), quote_path(&dir), quote_path(&to));
    let (here, todir, list, tools): (String, String, String, &[&str]) = match a {
        ArcKind::Zip => (format!("unzip -o {q} -d {qd}"), format!("mkdir -p {qt} && unzip -o {q} -d {qt}"), format!("unzip -l {q}"), &["unzip"]),
        ArcKind::Tar => (format!("tar -xf {q} -C {qd}"), format!("mkdir -p {qt} && tar -xf {q} -C {qt}"), format!("tar -tvf {q}"), &["tar"]),
        ArcKind::TarGz => (format!("tar -xzf {q} -C {qd}"), format!("mkdir -p {qt} && tar -xzf {q} -C {qt}"), format!("tar -tzvf {q}"), &["tar", "gzip"]),
        ArcKind::TarXz => (format!("tar -xJf {q} -C {qd}"), format!("mkdir -p {qt} && tar -xJf {q} -C {qt}"), format!("tar -tJvf {q}"), &["tar", "xz"]),
        ArcKind::TarBz2 => (format!("tar -xjf {q} -C {qd}"), format!("mkdir -p {qt} && tar -xjf {q} -C {qt}"), format!("tar -tjvf {q}"), &["tar", "bzip2"]),
        ArcKind::SevenZ => (format!("7z x -y {q} -o{qd}"), format!("mkdir -p {qt} && 7z x -y {q} -o{qt}"), format!("7z l {q}"), &["7z"]),
        ArcKind::Rar => (format!("unrar x -o+ {q} {}", quote_path(&format!("{}/", dir.trim_end_matches('/')))), format!("mkdir -p {qt} && unrar x -o+ {q} {}", quote_path(&format!("{to}/"))), format!("unrar l {q}"), &["unrar"]),
        ArcKind::Gz => (format!("gunzip -k {q}"), format!("mkdir -p {qt} && gunzip -c {q} > {}", quote_path(&join(&to, &stem_of(name)))), format!("gzip -l {q}"), &["gzip"]),
    };
    b.ins("extract_here", t("cm.extract_here"), here, tools);
    b.ins("extract_to", &tf("cm.extract_to", &[("dir", &format!("{}/", stem_of(name)))]), todir, tools);
    b.ins("list", t("cm.list"), list, tools);
}

/// Build the menu for a target: built-in items by object type, then matching custom items.
pub fn build(target: &Target, ctx: &Ctx) -> Menu {
    let mut b = B { ctx, items: vec![], group: 0 };
    let header;
    match target {
        Target::Files(files) if !files.is_empty() => {
            let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
            let qall = paths.iter().map(|p| quote_path(p)).collect::<Vec<_>>().join(" ");
            if files.len() == 1 {
                let f = &files[0];
                let name = file_name(&f.path).to_string();
                let ks = kinds(&name, f.is_dir, f.mode);
                header = format!("{name} · {}", kind_label(ks[0]));
                let q = quote_path(&f.path);
                for k in &ks {
                    match *k {
                        Kind::Archive(a) => { archive_items(&mut b, a, &f.path); b.sep(); }
                        Kind::Dir => {
                            let parent = parent_dir(&f.path);
                            let (qp, qn) = (quote_path(&parent), quote(&name));
                            b.ins("zip_dir", t("cm.zip_dir"), format!("cd {qp} && zip -qr {} {qn}", quote(&format!("{name}.zip"))), &["zip"]);
                            b.ins("targz_dir", t("cm.targz_dir"), format!("tar -czf {} -C {qp} {qn}", quote_path(&join(&parent, &format!("{name}.tar.gz")))), &["tar", "gzip"]);
                            b.push("cd_tab", t("cm.cd_tab").into(), Act::CdNewTab(f.path.clone()), &[]);
                            b.ins("du", t("cm.du"), format!("du -sh -- {q}"), &["du"]);
                            b.sep();
                        }
                        Kind::Log => {
                            b.ins("tail_f", t("cm.tail_f"), format!("tail -f -- {q}"), &["tail"]);
                            b.ins("grep", t("cm.grep"), format!("grep -n -- 'PATTERN' {q}"), &["grep"]);
                            b.sep();
                        }
                        Kind::Script => {
                            let i = interpreter(&name).unwrap_or("sh");
                            b.ins("run", t("cm.run"), format!("{i} {q}"), &[i]);
                            if f.mode & 0o111 == 0 { b.ins("chmod_x", t("cm.chmod_x"), format!("chmod +x -- {q}"), &[]); }
                            b.sep();
                        }
                        Kind::Executable => {
                            if !ks.contains(&Kind::Script) {
                                let run = if f.path.contains('/') { q.clone() } else { quote_path(&format!("./{}", f.path)) };
                                b.ins("run_exec", t("cm.run"), run, &[]);
                                b.sep();
                            }
                        }
                        Kind::Text => {
                            let remote_file = matches!(ctx.place, Place::RemotePanel | Place::Terminal { remote: true });
                            if remote_file { b.push("edit", t("cm.edit").into(), Act::OpenEditor(f.path.clone()), &[]); }
                            else { b.ins("edit_local", t("cm.edit_local"), format!("\"${{EDITOR:-vi}}\" {q}"), &[]); }
                            if !ks.contains(&Kind::Log) { b.ins("tail_f", t("cm.tail_f"), format!("tail -f -- {q}"), &["tail"]); }
                            b.ins("less", t("cm.less"), format!("less -- {q}"), &["less"]);
                            b.ins("diff", t("cm.diff"), format!("diff -u -- {q} {}", quote_path(&format!("{}.orig", f.path))), &["diff"]);
                            b.sep();
                        }
                        Kind::File => {}
                    }
                }
                if !f.is_dir && !ks.iter().any(|k| matches!(k, Kind::Script | Kind::Executable)) && ks.iter().all(|k| !matches!(k, Kind::Archive(_))) {
                    b.ins("chmod_x", t("cm.chmod_x"), format!("chmod +x -- {q}"), &[]);
                }
            } else {
                header = tf("cm.n_items", &[("n", &files.len())]);
                let dir = parent_dir(&paths[0]);
                let names = files.iter().map(|f| quote(file_name(&f.path))).collect::<Vec<_>>().join(" ");
                b.ins("zip_sel", t("cm.zip_sel"), format!("cd {} && zip -qr archive.zip {names}", quote_path(&dir)), &["zip"]);
                b.ins("targz_sel", t("cm.targz_sel"), format!("tar -czf {} -C {} {names}", quote_path(&join(&dir, "archive.tar.gz")), quote_path(&dir)), &["tar", "gzip"]);
                if files.len() == 2 && files.iter().all(|f| !f.is_dir) { b.ins("diff2", t("cm.diff"), format!("diff -u -- {qall}"), &["diff"]); }
                b.ins("du", t("cm.du"), format!("du -shc -- {qall}"), &["du"]);
                b.sep();
            }
            // common
            let single = files.len() == 1;
            let p0 = &paths[0];
            let dir = parent_dir(p0);
            let qdir = quote_path(&format!("{}/", dir.trim_end_matches('/')));
            let r = if files.iter().any(|f| f.is_dir) { "-r " } else { "" };
            b.ins("cp", t("cm.copy_to"), if single { format!("cp {r}-- {} {}", quote_path(p0), quote_path(&format!("{p0}.copy"))) } else { format!("cp {r}-- {qall} {qdir}") }, &[]);
            b.ins("mv", t("cm.move_to"), format!("mv -- {qall} {qdir}"), &[]);
            if single { b.ins("rename", t("cm.rename"), format!("mv -- {} {}", quote_path(p0), quote_path(p0)), &[]); }
            b.ins("rm", t("cm.delete"), format!("rm {r}-- {qall}"), &[]);
            b.ins("chmod", t("cm.chmod"), format!("chmod {}-- {qall}", if files.iter().any(|f| f.is_dir) { "-R 755 " } else { "644 " }), &[]);
            b.ins("chown", t("cm.chown"), format!("chown {}\"$USER\": -- {qall}", if r.is_empty() { "" } else { "-R " }), &[]);
            b.sep();
            match ctx.place {
                Place::RemotePanel => b.push("download", t("cm.download").into(), Act::Download(paths.clone()), &[]),
                Place::LocalPanel => b.push("upload", t("cm.upload").into(), Act::Upload(paths.clone()), &[]),
                Place::Terminal { .. } => {}
            }
            b.push("copy_path", t("cm.copy_path").into(), Act::Copy(paths.join("\n")), &[]);
            if single { b.push("copy_name", t("cm.copy_name").into(), Act::Copy(file_name(p0).to_string()), &[]); }
        }
        Target::Files(_) => header = String::new(),
        Target::Url(u) => {
            header = format!("URL · {}", ellipsis(u, 60));
            b.push("open_url", t("cm.open_url").into(), Act::OpenUrl(u.clone()), &[]);
            b.push("copy", t("cm.copy").into(), Act::Copy(u.clone()), &[]);
            if u.starts_with("http") || u.starts_with("ftp") {
                b.ins("curl_head", t("cm.curl_head"), format!("curl -sSI -- {}", quote(u)), &["curl"]);
                b.ins("curl_get", t("cm.curl_get"), format!("curl -fLO -- {}", quote(u)), &["curl"]);
            }
        }
        Target::HostPort { host, port } => {
            let shown = match port { Some(p) if host.contains(':') => format!("[{host}]:{p}"), Some(p) => format!("{host}:{p}"), None => host.clone() };
            header = format!("{} · {shown}", t("cm.address"));
            b.push("copy", t("cm.copy").into(), Act::Copy(shown.clone()), &[]);
            b.ins("ping", t("cm.ping"), format!("ping -c 4 {}", quote(host)), &["ping"]);
            if let Some(p) = port {
                b.ins("nc", t("cm.nc"), format!("nc -zv {} {p}", quote(host)), &["nc"]);
                let u = if host.contains(':') { format!("http://[{host}]:{p}/") } else { format!("http://{host}:{p}/") };
                b.ins("curl_head", t("cm.curl_head"), format!("curl -sSI -- {}", quote(&u)), &["curl"]);
                b.push("open_url", t("cm.open_url").into(), Act::OpenUrl(u), &[]);
            } else {
                b.ins("ssh", t("cm.ssh"), format!("ssh {}", quote(host)), &["ssh"]);
            }
        }
        Target::Selection(s) => {
            header = format!("{} · {}", t("cm.selection"), ellipsis(&s.replace('\n', "⏎"), 40));
            b.push("copy", t("cm.copy").into(), Act::Copy(s.clone()), &[]);
            b.push("search", t("cm.search").into(), Act::Search(s.clone()), &[]);
            b.push("ai_explain", t("cm.ai_explain").into(), Act::AiExplain(s.clone()), &[]);
            b.push("paste_input", t("cm.paste_input").into(), Act::PasteInput(s.clone()), &[]);
            b.sep();
            let specs = crate::tools::builtin();
            for id in crate::tools::terminal_targets(s) {
                let Some(sp) = specs.iter().find(|x| x.id == id) else { continue };
                b.push(&format!("tool_{id}"), crate::i18n::tf("cm.open_with_tool", &[("tool", &sp.name)]), Act::OpenTool(id.to_string(), s.clone()), &[]);
            }
        }
        Target::Blank { paste } => {
            header = t("cm.blank").to_string();
            if let Some(s) = paste {
                b.push("paste_input", t("cm.paste").into(), Act::PasteInput(s.clone()), &[]);
            }
        }
    }
    custom_items(&mut b, target);
    Menu { header, items: b.items }
}

fn ellipsis(s: &str, n: usize) -> String { if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) } }

pub fn kind_label(k: Kind) -> &'static str {
    match k { Kind::Archive(_) => t("cm.k_archive"), Kind::Dir => t("cm.k_dir"), Kind::Text => t("cm.k_text"), Kind::Script => t("cm.k_script"), Kind::Executable => t("cm.k_exec"), Kind::Log => t("cm.k_log"), Kind::File => t("cm.k_file") }
}

/// Does a custom item apply to this target on this host?
pub fn custom_matches(it: &CustomItem, target: &Target, host: &str) -> bool {
    if !it.hosts.is_empty() && !it.hosts.iter().any(|h| crate::policy::glob(h, host)) { return false; }
    let one = |types: &[&str], name: Option<&str>| -> bool {
        if !it.types.is_empty() && !it.types.iter().any(|t| types.contains(&t.as_str())) { return false; }
        if !it.extensions.is_empty() {
            let Some(n) = name else { return false };
            let n = n.to_lowercase();
            if !it.extensions.iter().any(|e| { let e = e.trim_start_matches('.').to_lowercase(); n.ends_with(&format!(".{e}")) }) { return false; }
        }
        if !it.glob.is_empty() { let Some(n) = name else { return false }; if !it.glob.iter().any(|g| crate::policy::glob(g, n)) { return false; } }
        true
    };
    match target {
        Target::Files(fs) if !fs.is_empty() => fs.iter().all(|f| {
            let n = file_name(&f.path);
            let mut ty: Vec<&str> = kinds(n, f.is_dir, f.mode).iter().map(|k| k.type_name()).collect();
            if !f.is_dir { ty.push("file"); }
            one(&ty, Some(n))
        }),
        Target::Files(_) => false,
        Target::Url(_) => one(&["url"], None),
        Target::HostPort { .. } => one(&["ip"], None),
        Target::Selection(_) => one(&["selection"], None),
        Target::Blank { .. } => false,
    }
}
fn custom_items(b: &mut B, target: &Target) {
    let ctx = b.ctx;
    let mut first = true;
    for (i, it) in ctx.custom.iter().enumerate() {
        if !custom_matches(it, target, ctx.host) { continue; }
        let paths: Vec<String> = match target {
            Target::Files(fs) => fs.iter().map(|f| f.path.clone()).collect(),
            Target::Url(u) => vec![u.clone()],
            Target::HostPort { host, .. } => vec![host.clone()],
            Target::Selection(s) => vec![s.clone()],
            Target::Blank { .. } => vec![],
        };
        let Ok(cmd) = expand(&it.command, &vars_for(&paths, ctx.host)) else { continue };
        if first { b.sep(); first = false; }
        let label = if ctx.lang_en { it.label_en.clone().unwrap_or_else(|| it.label.clone()) } else { it.label.clone() };
        let tools: Vec<&str> = it.tools.iter().map(String::as_str).collect();
        b.push(&format!("custom{i}"), label, Act::Insert(cmd), &tools);
        if let Some(last) = b.items.last_mut() { last.custom = true; }
    }
}

/// Bytes to put a command on the input line without running it: Ctrl+U clears whatever was
/// typed, then the command text. Never contains CR/LF.
pub fn insert_bytes(cmd: &str) -> Vec<u8> {
    let mut v = vec![0x15];
    v.extend(cmd.replace(['\r', '\n'], " ").into_bytes());
    v
}

#[cfg(test)]
mod tests;
