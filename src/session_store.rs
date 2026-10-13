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
}
impl PaneSave {
    pub fn resume(&self) -> Option<&str> { match self { PaneSave::Local { resume, .. } | PaneSave::Ssh { resume, .. } => resume.as_deref() } }
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
pub struct LayoutSave { pub version: u32, pub saved_at: String, pub active: usize, pub tabs: Vec<NodeSave> }

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
pub fn new_session_name() -> String { format!("agentshell-{}", &uuid_like()[..8]) }
pub fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:016x}{:08x}", n as u64 ^ 0x9e37_79b9_7f4a_7c15, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let l = LayoutSave { version: 1, saved_at: "x".into(), active: 1, tabs: vec![
            NodeSave::Leaf { pane: PaneSave::Local { cwd: "/tmp".into(), daemon_id: Some("d1".into()), resume: Some("claude --continue".into()) } },
            NodeSave::Split { vertical: true, ratio: 0.3, a: Box::new(NodeSave::Leaf { pane: PaneSave::Local { cwd: String::new(), daemon_id: None, resume: None } }),
                b: Box::new(NodeSave::Leaf { pane: PaneSave::Ssh { target: SshTarget { host: "h".into(), port: 22, user: "u".into(), tmux: Some("agentshell-1".into()), ..Default::default() }, resume: None } }) },
        ] };
        let p = std::env::temp_dir().join(format!("stp-layout-{}.json", std::process::id()));
        save(&p, &l).unwrap();
        assert_eq!(load(&p).unwrap(), l);
        let _ = std::fs::remove_file(p);
        assert_eq!(resume_command("Claude Code"), Some("claude --continue"));
        assert_eq!(resume_command("Codex"), Some("codex resume --last"));
        assert!(new_session_name().starts_with("agentshell-"));
    }
}
