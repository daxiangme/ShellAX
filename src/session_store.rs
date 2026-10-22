//! Layout persistence (PRD 3.6): tabs, split tree, per-pane host / cwd / tmux session /
//! daemon id / agent resume command. Restoring never re-runs commands: panes reopen as fresh
//! shells in the saved cwd, and an agent resume command is only offered as a button.
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::ssh::SshTarget;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PaneSave {
    Local { #[serde(default)] cwd: String, #[serde(default)] daemon_id: Option<String>, #[serde(default)] resume: Option<String> },
    Ssh { target: SshTarget, #[serde(default)] resume: Option<String> },
    /// SFTP browser pane/tab (PRD 5.21): reopens on the same host and directories.
    Sftp { target: SshTarget, #[serde(default)] local_dir: String, #[serde(default)] remote_dir: String, #[serde(default)] follow: bool },
}
impl PaneSave {
    pub fn resume(&self) -> Option<&str> { match self { PaneSave::Local { resume, .. } | PaneSave::Ssh { resume, .. } => resume.as_deref(), PaneSave::Sftp { .. } => None } }
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
            NodeSave::Leaf { pane: PaneSave::Sftp { target: t.clone(), local_dir: "/tmp/本地 dir".into(), remote_dir: "/srv/a b".into(), follow: false } },
            NodeSave::Split { vertical: true, ratio: 0.6, a: Box::new(NodeSave::Leaf { pane: PaneSave::Ssh { target: t.clone(), resume: None } }),
                b: Box::new(NodeSave::Leaf { pane: PaneSave::Sftp { target: t, local_dir: String::new(), remote_dir: "/".into(), follow: true } }) },
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
                b: Box::new(NodeSave::Leaf { pane: PaneSave::Ssh { target: SshTarget { host: "h".into(), port: 22, user: "u".into(), tmux: Some("shellax-1".into()), ..Default::default() }, resume: None } }) },
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
}
