//! Layout persistence (PRD 3.6): tabs, split tree, per-pane host / cwd / tmux session /
//! daemon id / agent resume command. Restoring never re-runs commands: panes reopen as fresh
//! shells in the saved cwd (local: spawn there; SSH: `cd` after connect), and an agent resume
//! command is only offered as a button.
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ssh::SshTarget;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneSave {
    Local { #[serde(default)] cwd: String, #[serde(default)] daemon_id: Option<String>, #[serde(default)] resume: Option<String> },
    /// `cwd` is the remote shell's last directory. Older files omit it.
    Ssh { target: SshTarget, #[serde(default)] cwd: String, #[serde(default)] resume: Option<String> },
    /// SFTP browser pane/tab (PRD 5.21): reopens on the same host and directories.
    /// `panes` (left, right) describes each pane's source; older files only have `target`.
    Sftp { target: SshTarget, #[serde(default)] local_dir: String, #[serde(default)] remote_dir: String, #[serde(default)] follow: bool, #[serde(default, skip_serializing_if = "Vec::is_empty")] panes: Vec<SftpSide> },
    /// Database tab: a table browser (`table` set) or a SQL editor with its unsaved text.
    Db { conn: String, #[serde(default)] database: String, #[serde(default, skip_serializing_if = "Option::is_none")] schema: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] table: Option<String>, #[serde(default, skip_serializing_if = "String::is_empty")] sql: String, #[serde(default, skip_serializing_if = "Option::is_none")] editor_h: Option<f32> },
    /// Redis tab on a saved connection, with the key that was open.
    Redis { conn: String, #[serde(default, skip_serializing_if = "Option::is_none")] key: Option<String> },
    /// Container engine tab.
    Docker { endpoint: String },
    /// Container log tab.
    DockerLogs { endpoint: String, container: String, #[serde(default)] name: String },
    /// MongoDB tab: database / collection and the query in the filter bar.
    Mongo { conn: String, #[serde(default)] database: String, #[serde(default)] collection: String, #[serde(default, skip_serializing_if = "String::is_empty")] filter: String, #[serde(default, skip_serializing_if = "String::is_empty")] sort: String },
}

/// One pane of an SFTP view: `kind` is "local", "remote" or "empty".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SftpSide {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<SshTarget>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub dir: String,
}
impl PaneSave {
    pub fn resume(&self) -> Option<&str> { match self { PaneSave::Local { resume, .. } | PaneSave::Ssh { resume, .. } => resume.as_deref(), _ => None } }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "node", rename_all = "lowercase")]
#[allow(clippy::large_enum_variant)] // a handful per layout file
pub enum NodeSave {
    Leaf { pane: PaneSave },
    Split { vertical: bool, #[serde(default = "half")] ratio: f32, a: Box<NodeSave>, b: Box<NodeSave> },
}
fn half() -> f32 { 0.5 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct LayoutSave {
    pub version: u32, pub saved_at: String, pub active: usize, pub tabs: Vec<NodeSave>,
    /// Custom tab names (PRD 5.23), parallel to `tabs`; empty when no tab was renamed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<Option<String>>,
}

pub fn save(path: &Path, l: &LayoutSave) -> Result<()> {
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(l)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
pub fn load(path: &Path) -> Option<LayoutSave> { serde_json::from_slice(&std::fs::read(path).ok()?).ok() }

/// Directory to write into the layout file.
///
/// `osc` is the shell's own report (OSC 7 or OSC 9;9). It wins over `process`, the cwd of
/// whatever process the probe can see: on Windows a PowerShell `Set-Location` does not update
/// the process cwd (it stays at the directory the shell was spawned in), and while a program is
/// in the foreground that probe can be the program's directory rather than the shell's.
/// A bare name — the folder label a window title often carries — is not a directory and is skipped,
/// so the title and the restored cwd cannot drift apart.
pub fn pick_saved_cwd(osc: Option<&str>, process: Option<&str>, prompt: Option<&str>) -> String {
    fn usable(s: Option<&str>) -> Option<&str> {
        s.map(str::trim).filter(|s| !s.is_empty() && !s.chars().any(|c| c.is_control()))
    }
    for s in [usable(osc), usable(process), usable(prompt)].into_iter().flatten() {
        if is_cwd_path(s) { return s.to_string(); }
    }
    String::new()
}

fn is_cwd_path(p: &str) -> bool {
    p == "~" || p.starts_with("~/") || p.starts_with("~\\") || p.starts_with('/') || p.starts_with('\\') || is_windows_abs(p)
}
fn is_windows_abs(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/')
}

/// Local spawn directory for a restored shell. `None` means the directory is missing or not a
/// real path: the caller starts the shell in its default directory instead.
///
/// `home` expands a leading `~`. Candidates include the OSC 7 Windows form (`/C:/...`), an MSYS
/// `/c/...` path, and slash-direction variants, and the first one `is_dir` accepts is used.
pub fn restore_local_dir(saved: &str, home: Option<&str>, is_dir: impl Fn(&str) -> bool) -> Option<String> {
    let s = saved.trim().trim_matches('"').trim();
    if s.is_empty() || s.chars().any(|c| c.is_control()) { return None; }
    let mut cands = Vec::new();
    let mut add = |p: String| { if !p.is_empty() && !cands.iter().any(|e: &String| e == &p) { cands.push(p); } };
    add(s.to_string());
    if let Some(h) = home { if let Some(e) = expand_tilde(s, h) { add(e); } }
    if let Some(p) = crate::ctxmenu::parse_osc7(s) { add(p); }
    if let Some(w) = osc_windows_path(s) { add(w); }
    if s.contains('/') { add(s.replace('/', "\\")); }
    if s.contains('\\') { add(s.replace('\\', "/")); }
    drop(add);
    cands.into_iter().find(|c| is_cwd_path(c) && !c.starts_with('~') && is_dir(c))
}

/// `restore_local_dir` against the real filesystem. `~` uses HOME, then USERPROFILE.
pub fn restore_local_dir_fs(saved: &str) -> Option<String> {
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty()).or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()));
    restore_local_dir(saved, home.as_deref(), |p| std::path::Path::new(p).is_dir())
}

fn expand_tilde(p: &str, home: &str) -> Option<String> {
    let home = home.trim().trim_end_matches(['/', '\\']);
    if home.is_empty() { return None; }
    let rest = if p == "~" { "" } else { p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"))? };
    let rest = rest.trim_start_matches(['/', '\\']);
    let sep = if home.contains('\\') { '\\' } else { '/' };
    Some(if rest.is_empty() { home.to_string() } else { format!("{home}{sep}{rest}") })
}

/// `/C:/Users/x` or MSYS `/c/Users/x` → `C:\Users\x`. Other strings return None.
fn osc_windows_path(p: &str) -> Option<String> {
    let b = p.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        let rest = &p[1..];
        return Some(if rest.len() == 2 { format!("{rest}\\") } else { rest.replace('/', "\\") });
    }
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
        let drive = (b[1] as char).to_ascii_uppercase();
        let rest = p[3..].replace('/', "\\");
        return Some(if rest.is_empty() { format!("{drive}:\\") } else { format!("{drive}:\\{rest}") });
    }
    None
}

/// `cd` to send to a remote shell after it connects. POSIX quoting only; nothing but the
/// directory change (a newline or other control character would run a second command, so those
/// paths are refused). Not a Windows path: remote shells are quoted as POSIX.
pub fn remote_cd_command(path: &str) -> Option<String> {
    let p = path.trim();
    if p.is_empty() || p.chars().any(|c| c.is_control()) { return None; }
    if !(p.starts_with('/') || p == "~" || p.starts_with("~/")) { return None; }
    Some(format!("cd -- {}", crate::ctxmenu::quote_path(p)))
}

/// Command that resumes the most recent conversation of a detected agent (by display name).
pub fn resume_command(agent: &str) -> Option<&'static str> {
    let a = agent.to_lowercase();
    if a.contains("claude") { Some("claude --continue") }
    else if a.contains("codex") { Some("codex resume --last") }
    else if a.contains("grok") { Some("grok --resume") }
    else { None }
}

/// Stable tmux session name for a pane.
pub fn new_session_name() -> String { format!("shellax-{}", &uuid_like()[..8]) }
pub fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:016x}{:08x}", n as u64 ^ 0x9e37_79b9_7f4a_7c15, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sftp_tabs_roundtrip() {
        let t = SshTarget { host: "h".into(), port: 2222, user: "u".into(), ..Default::default() };
        let l = LayoutSave { version: 1, saved_at: "x".into(), active: 0, names: vec![], tabs: vec![
            NodeSave::Leaf { pane: PaneSave::Sftp { target: t.clone(), local_dir: "/tmp/本地 dir".into(), remote_dir: "/srv/a b".into(), follow: false, panes: vec![] } },
            NodeSave::Split { vertical: true, ratio: 0.6, a: Box::new(NodeSave::Leaf { pane: PaneSave::Ssh { target: t.clone(), cwd: String::new(), resume: None } }),
                b: Box::new(NodeSave::Leaf { pane: PaneSave::Sftp { target: t, local_dir: String::new(), remote_dir: "/".into(), follow: true, panes: vec![SftpSide { kind: "local".into(), dir: "/tmp".into(), ..Default::default() }, SftpSide { kind: "empty".into(), ..Default::default() }] } }) },
        ] };
        let p = std::env::temp_dir().join(format!("stp-layout-sftp-{}.json", std::process::id()));
        save(&p, &l).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back, l);
        assert!(std::fs::read_to_string(&p).unwrap().contains("\"kind\": \"sftp\""));
        assert_eq!(back.tabs[0].clone(), l.tabs[0]);
        // minimal / older sftp entry still loads with defaults
        let j = r#"{"version":1,"saved_at":"","active":0,"tabs":[{"node":"leaf","pane":{"kind":"sftp","target":{"host":"h","port":22,"user":"u"}}}]}"#;
        let o: LayoutSave = serde_json::from_str(j).unwrap();
        assert!(matches!(&o.tabs[0], NodeSave::Leaf { pane: PaneSave::Sftp { follow: false, remote_dir, .. } } if remote_dir.is_empty()));
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn roundtrip() {
        let l = LayoutSave { version: 1, saved_at: "x".into(), active: 1, tabs: vec![
            NodeSave::Leaf { pane: PaneSave::Local { cwd: "/tmp".into(), daemon_id: Some("d1".into()), resume: Some("claude --continue".into()) } },
            NodeSave::Split { vertical: true, ratio: 0.3, a: Box::new(NodeSave::Leaf { pane: PaneSave::Local { cwd: String::new(), daemon_id: None, resume: None } }),
                b: Box::new(NodeSave::Leaf { pane: PaneSave::Ssh { target: SshTarget { host: "h".into(), port: 22, user: "u".into(), tmux: Some("shellax-1".into()), ..Default::default() }, cwd: "/srv/app".into(), resume: None } }) },
        ], names: vec![Some("构建 · web".into()), None] };
        let p = std::env::temp_dir().join(format!("stp-layout-{}.json", std::process::id()));
        save(&p, &l).unwrap();
        assert_eq!(load(&p).unwrap(), l);
        // files written before PRD 5.23 have no names
        let old: LayoutSave = serde_json::from_str(r#"{"version":1,"saved_at":"x","active":0,"tabs":[]}"#).unwrap();
        assert!(old.names.is_empty());
        let _ = std::fs::remove_file(p);
        assert_eq!(resume_command("Claude Code"), Some("claude --continue"));
        assert_eq!(resume_command("Codex"), Some("codex resume --last"));
        assert!(new_session_name().starts_with("shellax-"));
    }

    #[test]
    fn cwd_roundtrip_keeps_local_and_remote_directories() {
        let l = LayoutSave { version: 1, saved_at: "t".into(), active: 0, names: vec![], tabs: vec![
            NodeSave::Leaf { pane: PaneSave::Local { cwd: r"C:\Users\me\my dir".into(), daemon_id: None, resume: Some("claude --continue".into()) } },
            NodeSave::Leaf { pane: PaneSave::Ssh { target: SshTarget { host: "h".into(), port: 22, user: "u".into(), ..Default::default() }, cwd: "/home/u/my dir".into(), resume: None } },
        ] };
        let p = std::env::temp_dir().join(format!("stp-layout-cwd-{}.json", std::process::id()));
        save(&p, &l).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back, l);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains(r"C:\\Users\\me\\my dir"), "{text}");
        assert!(text.contains("/home/u/my dir"), "{text}");
        // resume is stored beside the directory and is not the thing that gets executed on restore
        assert!(text.contains("claude --continue"));
        assert!(!text.contains("cd -- claude"));
        let _ = std::fs::remove_file(&p);
        // files written before remote cwd existed still load, with an empty directory
        let old: LayoutSave = serde_json::from_str(r#"{"version":1,"saved_at":"","active":0,"tabs":[{"node":"leaf","pane":{"kind":"ssh","target":{"host":"h","port":22,"user":"u"}}}]}"#).unwrap();
        assert!(matches!(&old.tabs[0], NodeSave::Leaf { pane: PaneSave::Ssh { cwd, resume: None, .. } } if cwd.is_empty()));
        let old_local: LayoutSave = serde_json::from_str(r#"{"version":1,"saved_at":"","active":0,"tabs":[{"node":"leaf","pane":{"kind":"local"}}]}"#).unwrap();
        assert!(matches!(&old_local.tabs[0], NodeSave::Leaf { pane: PaneSave::Local { cwd, .. } } if cwd.is_empty()));
    }

    #[test]
    fn missing_local_dir_falls_back_and_windows_forms_are_accepted() {
        let no = |_: &str| false;
        assert_eq!(restore_local_dir("", None, no), None);
        assert_eq!(restore_local_dir("   ", None, no), None);
        assert_eq!(restore_local_dir("/gone", None, no), None);
        assert_eq!(restore_local_dir("~/missing", Some("/home/u"), no), None);
        // a folder label is not a directory, even if a relative path of that name exists
        assert_eq!(restore_local_dir("myproject", None, |_| true), None);
        assert_eq!(restore_local_dir("/proj", None, |p| p == "/proj"), Some("/proj".into()));
        assert_eq!(restore_local_dir("~/proj", Some("/home/u"), |p| p == "/home/u/proj"), Some("/home/u/proj".into()));
        assert_eq!(restore_local_dir("~\\proj", Some(r"C:\Users\me"), |p| p == r"C:\Users\me\proj"), Some(r"C:\Users\me\proj".into()));
        // OSC 7 drive form, MSYS form, and forward slashes, when only the native path exists
        let win = |p: &str| p == r"C:\Users\me\my dir";
        assert_eq!(restore_local_dir("/C:/Users/me/my dir", None, win), Some(r"C:\Users\me\my dir".into()));
        assert_eq!(restore_local_dir("/c/Users/me/my dir", None, win), Some(r"C:\Users\me\my dir".into()));
        assert_eq!(restore_local_dir("C:/Users/me/my dir", None, win), Some(r"C:\Users\me\my dir".into()));
        assert_eq!(restore_local_dir("file://PC/C:/Users/me/my%20dir", None, win), Some(r"C:\Users\me\my dir".into()));
        // the original path wins when it itself exists (do not rewrite a real Unix path)
        assert_eq!(restore_local_dir("/c/Users/me", None, |p| p == "/c/Users/me"), Some("/c/Users/me".into()));
        let dir = std::env::temp_dir().join(format!("stp-cwd-exists-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = dir.to_string_lossy().to_string();
        assert_eq!(restore_local_dir_fs(&s).as_deref(), Some(s.as_str()));
        assert_eq!(restore_local_dir_fs(&format!("{s}-missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_cd_quotes_only_the_directory() {
        assert_eq!(remote_cd_command("/var/log"), Some("cd -- /var/log".into()));
        assert_eq!(remote_cd_command("/tmp/my dir"), Some("cd -- '/tmp/my dir'".into()));
        assert_eq!(remote_cd_command("/tmp/it's"), Some(r"cd -- '/tmp/it'\''s'".into()));
        assert_eq!(remote_cd_command("~/work dir"), Some("cd -- ~/'work dir'".into()));
        assert_eq!(remote_cd_command("~"), Some("cd -- ~".into()));
        assert_eq!(remote_cd_command("-rf"), None);
        assert_eq!(remote_cd_command("/tmp/$(rm)"), Some("cd -- '/tmp/$(rm)'".into()));
        assert_eq!(remote_cd_command("/tmp/foo\nrm -rf /"), None);
        assert_eq!(remote_cd_command(""), None);
        assert_eq!(remote_cd_command("  "), None);
        assert_eq!(remote_cd_command("myproject"), None);
        assert_eq!(remote_cd_command(r"C:\Users\me"), None);
        let cmd = remote_cd_command("/srv/app").unwrap();
        assert!(cmd.starts_with("cd -- "));
        assert!(!cmd.contains('\n') && !cmd.contains("claude"));
    }

    #[test]
    fn osc_cwd_beats_the_foreground_process_and_a_bare_title() {
        // shell OSC wins over the foreground program's directory
        assert_eq!(pick_saved_cwd(Some(r"C:\work\app"), Some(r"C:\work\app\.cache\fg"), None), r"C:\work\app");
        assert_eq!(pick_saved_cwd(Some("/srv/app"), Some("/tmp/fg-child"), Some("~/from-prompt")), "/srv/app");
        // no OSC: process probe, then the prompt line
        assert_eq!(pick_saved_cwd(None, Some("/tmp/fg-child"), Some("~/from-prompt")), "/tmp/fg-child");
        assert_eq!(pick_saved_cwd(Some("  "), Some(""), Some("~/from-prompt")), "~/from-prompt");
        assert_eq!(pick_saved_cwd(None, None, Some("~")), "~");
        // the window title is the folder name only; it must not be saved as the cwd,
        // and it must not hide a real path from another source
        assert_eq!(pick_saved_cwd(Some("myproject"), Some(r"C:\work\app"), None), r"C:\work\app");
        assert_eq!(pick_saved_cwd(Some("myproject"), None, None), "");
        assert_eq!(pick_saved_cwd(None, None, None), "");
        assert_eq!(pick_saved_cwd(Some("/ok\nrm"), Some("/safe"), None), "/safe");
    }
}
