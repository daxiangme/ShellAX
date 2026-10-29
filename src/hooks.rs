//! External coding-agent hooks (PRD 6.9): request parsing, decision rendering and installers
//! for Claude Code, Codex and Grok Build. Formats follow each vendor's public docs:
//! - Claude Code: https://code.claude.com/docs/en/hooks (PreToolUse, `hookSpecificOutput.permissionDecision`
//!   allow|deny|ask; `onFailure: "block"` makes a failing hook block)
//! - Codex: https://developers.openai.com/codex/hooks (`~/.codex/hooks.json`; PreToolUse can only deny;
//!   PermissionRequest `decision.behavior` allow|deny; hooks must be trusted in `/hooks`)
//! - Grok Build: https://docs.x.ai/build/features/hooks (`~/.grok/hooks/*.json`; PreToolUse
//!   `{"decision": "allow|deny|ask", "reason"}`; failures are fail-open)
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::policy::Effect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent { Claude, Codex, Grok }

impl Agent {
    pub fn id(self) -> &'static str { match self { Agent::Claude => "claude", Agent::Codex => "codex", Agent::Grok => "grok" } }
    pub fn parse(s: &str) -> Option<Agent> {
        match s.to_ascii_lowercase().as_str() { "claude" | "claude-code" => Some(Agent::Claude), "codex" => Some(Agent::Codex), "grok" | "grok-build" => Some(Agent::Grok), _ => None }
    }
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Grok];
}

/// A normalised hook request.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HookReq {
    /// "PreToolUse" or "PermissionRequest"
    pub event: String,
    pub tool: String,
    /// Shell command for Bash-like tools, otherwise a short JSON of the tool input.
    pub command: String,
    /// "exec" for shell tools, "write" for edit/write tools, "tool" otherwise
    pub action: String,
    pub cwd: String,
    pub session: String,
}

fn s(v: &Value, keys: &[&str]) -> String {
    for k in keys { if let Some(x) = v.get(*k).and_then(|x| x.as_str()) { return x.to_string(); } }
    String::new()
}

/// Parse the JSON a hook receives (stdin for command hooks, body for HTTP hooks).
pub fn parse_request(body: &str) -> Result<HookReq> {
    let v: Value = serde_json::from_str(body).context("hook input is not JSON")?;
    let event_raw = s(&v, &["hook_event_name", "hookEventName", "event"]);
    let event = match event_raw.as_str() { "pre_tool_use" | "preToolUse" | "" => "PreToolUse".to_string(), "permission_request" | "permissionRequest" => "PermissionRequest".into(), e => e.to_string() };
    let tool = s(&v, &["tool_name", "toolName", "tool"]);
    let input = v.get("tool_input").or_else(|| v.get("toolInput")).or_else(|| v.get("input")).cloned().unwrap_or(Value::Null);
    let lt = tool.to_ascii_lowercase();
    let shell_like = matches!(lt.as_str(), "bash" | "shell" | "run_terminal_cmd" | "run_shell_command" | "run_terminal_command" | "execute_command" | "shell_command" | "run_command" | "exec_command" | "local_shell" | "terminal" | "powershell");
    let (action, command) = if shell_like {
        let c = match &input { Value::String(x) => x.clone(), _ => s(&input, &["command", "cmd", "script"]) };
        // Codex may send argv arrays
        let c = if c.is_empty() { input.get("command").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default() } else { c };
        ("exec", c)
    } else if matches!(lt.as_str(), "edit" | "write" | "multiedit" | "notebookedit" | "apply_patch" | "edit_file" | "write_file" | "search_replace") {
        let p = s(&input, &["file_path", "path", "target_file"]);
        ("write", if p.is_empty() { input.to_string().chars().take(400).collect() } else { p })
    } else {
        ("tool", input.to_string().chars().take(400).collect())
    };
    Ok(HookReq { event, tool, command, action: action.into(), cwd: s(&v, &["cwd"]), session: s(&v, &["session_id", "sessionId"]) })
}

/// Hook response: (stdout/body, exit code for command hooks).
pub fn render(agent: Agent, event: &str, effect: Effect, reason: &str) -> (String, i32) {
    let reason = format!("ShellAX: {reason}");
    match (agent, event) {
        (Agent::Claude, "PermissionRequest") | (Agent::Codex, "PermissionRequest") => {
            let d = match effect {
                Effect::Allow => json!({"behavior": "allow"}),
                Effect::Deny => json!({"behavior": "deny", "message": reason}),
                Effect::Ask => return (String::new(), 0), // no decision: the agent's own prompt appears
            };
            (json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": d}}).to_string(), 0)
        }
        (Agent::Claude, _) => {
            let d = match effect { Effect::Allow => "allow", Effect::Deny => "deny", Effect::Ask => "ask" };
            (json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": d, "permissionDecisionReason": reason}}).to_string(), 0)
        }
        (Agent::Codex, _) => match effect {
            // Codex PreToolUse can only deny; Ask was already resolved by our approval card (or
            // falls through to Codex's own approval flow).
            Effect::Deny => (json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}).to_string(), 0),
            _ => (String::new(), 0),
        },
        (Agent::Grok, _) => {
            let d = match effect { Effect::Allow => "allow", Effect::Deny => "deny", Effect::Ask => "ask" };
            (json!({"decision": d, "reason": reason}).to_string(), if effect == Effect::Deny { 2 } else { 0 })
        }
    }
}

/// Where each agent's user-level hook config lives.
pub fn config_path(agent: Agent, home: &Path) -> PathBuf {
    match agent {
        Agent::Claude => home.join(".claude/settings.json"),
        Agent::Codex => home.join(".codex/hooks.json"),
        Agent::Grok => home.join(".grok/hooks/shellax.json"),
    }
}

pub const MARKER: &str = "shellax-cli hook";

fn handler(agent: Agent, cli: &str, event: &str, timeout: u64) -> Value {
    let cmd = format!("{} hook {} {}", shell_quote(cli), agent.id(), event);
    match agent {
        Agent::Claude => json!({"type": "command", "command": cmd, "timeout": timeout, "onFailure": "block", "statusMessage": "ShellAX policy check"}),
        Agent::Codex => json!({"type": "command", "command": cmd, "timeout": timeout, "statusMessage": "ShellAX policy check"}),
        Agent::Grok => json!({"type": "command", "command": cmd, "timeout": timeout}),
    }
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) { s.into() } else { format!("'{}'", s.replace('\'', "'\\''")) }
}

fn is_ours(h: &Value) -> bool { h.get("command").and_then(|c| c.as_str()).is_some_and(|c| c.contains(" hook ") && (c.contains("shellax") || c.contains("coossh") || c.contains("agentshell") || c.contains(MARKER))) }

/// Remove our handlers from a hooks object (keeps everyone else's).
fn strip_ours(hooks: &mut serde_json::Map<String, Value>) {
    for (_ev, groups) in hooks.iter_mut() {
        if let Some(arr) = groups.as_array_mut() {
            for g in arr.iter_mut() {
                if let Some(hs) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) { hs.retain(|h| !is_ours(h)); }
            }
            arr.retain(|g| g.get("hooks").and_then(|h| h.as_array()).is_none_or(|h| !h.is_empty()));
        }
    }
    hooks.retain(|_, g| g.as_array().is_none_or(|a| !a.is_empty()));
}

/// Merge our hook into an existing config document (idempotent).
pub fn merge(agent: Agent, existing: Option<&str>, cli: &str, timeout: u64) -> Result<String> {
    let mut doc: Value = match existing { Some(t) if !t.trim().is_empty() => serde_json::from_str(t).map_err(|e| anyhow!("existing config is not valid JSON: {e}"))?, _ => json!({}) };
    let obj = doc.as_object_mut().ok_or_else(|| anyhow!("config root is not an object"))?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or_else(|| anyhow!("`hooks` is not an object"))?;
    strip_ours(hooks);
    let events: &[&str] = match agent { Agent::Claude | Agent::Grok => &["PreToolUse"], Agent::Codex => &["PreToolUse", "PermissionRequest"] };
    for ev in events {
        let list = hooks.entry(ev.to_string()).or_insert_with(|| json!([]));
        let list = list.as_array_mut().ok_or_else(|| anyhow!("hooks.{ev} is not an array"))?;
        list.push(json!({"matcher": "Bash", "hooks": [handler(agent, cli, ev, timeout)]}));
    }
    Ok(serde_json::to_string_pretty(&doc)? + "\n")
}

pub fn unmerge(existing: &str) -> Result<String> {
    let mut doc: Value = serde_json::from_str(existing)?;
    if let Some(h) = doc.get_mut("hooks").and_then(|h| h.as_object_mut()) { strip_ours(h); }
    if doc.get("hooks").and_then(|h| h.as_object()).is_some_and(|h| h.is_empty()) { doc.as_object_mut().unwrap().remove("hooks"); }
    Ok(serde_json::to_string_pretty(&doc)? + "\n")
}

#[derive(Debug)]
pub struct InstallReport { pub path: PathBuf, pub backup: Option<PathBuf>, pub content: String, pub changed: bool }

/// Install (or with `remove` uninstall) our hook for `agent` under `home`. A timestamped backup
/// of the previous file is written next to it before any change. `dry_run` writes nothing.
pub fn install(agent: Agent, home: &Path, cli: &str, timeout: u64, remove: bool, dry_run: bool) -> Result<InstallReport> {
    let path = config_path(agent, home);
    let old = std::fs::read_to_string(&path).ok();
    let content = if remove {
        match &old { Some(o) => unmerge(o)?, None => return Ok(InstallReport { path, backup: None, content: String::new(), changed: false }) }
    } else { merge(agent, old.as_deref(), cli, timeout)? };
    // our own Grok file: delete instead of leaving an empty doc
    let changed = old.as_deref() != Some(content.as_str());
    if dry_run || !changed { return Ok(InstallReport { path, backup: None, content, changed }); }
    let mut backup = None;
    if let Some(o) = &old {
        let b = path.with_file_name(format!("{}.shellax-bak-{}", path.file_name().unwrap().to_string_lossy(), chrono::Local::now().format("%Y%m%d-%H%M%S")));
        std::fs::write(&b, o)?;
        backup = Some(b);
    }
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    if remove && agent == Agent::Grok { let _ = std::fs::remove_file(&path); } else {
        let tmp = path.with_extension("shellax-tmp");
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(InstallReport { path, backup, content, changed })
}

/// Is our hook present in this agent's config?
pub fn installed(agent: Agent, home: &Path) -> bool {
    std::fs::read_to_string(config_path(agent, home)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("hooks").cloned()).and_then(|h| h.as_object().cloned())
        .is_some_and(|h| h.values().filter_map(|g| g.as_array()).flatten().filter_map(|g| g.get("hooks").and_then(|x| x.as_array())).flatten().any(is_ours))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_vendor_inputs() {
        let c = parse_request(r#"{"session_id":"s1","cwd":"/p","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#).unwrap();
        assert_eq!((c.event.as_str(), c.action.as_str(), c.command.as_str(), c.session.as_str()), ("PreToolUse", "exec", "rm -rf build", "s1"));
        let g = parse_request(r#"{"hook_event_name":"pre_tool_use","toolName":"bash","toolInput":{"command":"ls"}}"#).unwrap();
        assert_eq!((g.event.as_str(), g.command.as_str()), ("PreToolUse", "ls"));
        let x = parse_request(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":["git","push"]}}"#).unwrap();
        assert_eq!((x.event.as_str(), x.command.as_str()), ("PermissionRequest", "git push"));
        let w = parse_request(r#"{"tool_name":"Edit","tool_input":{"file_path":"/etc/hosts"}}"#).unwrap();
        assert_eq!((w.action.as_str(), w.command.as_str()), ("write", "/etc/hosts"));
        assert!(parse_request("nope").is_err());
    }
    #[test]
    fn renders_vendor_outputs() {
        let (o, code) = render(Agent::Claude, "PreToolUse", Effect::Deny, "r");
        let v: Value = serde_json::from_str(&o).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny"); assert_eq!(code, 0);
        assert_eq!(serde_json::from_str::<Value>(&render(Agent::Claude, "PreToolUse", Effect::Ask, "r").0).unwrap()["hookSpecificOutput"]["permissionDecision"], "ask");
        assert_eq!(render(Agent::Codex, "PreToolUse", Effect::Allow, "r").0, "");
        let (o, _) = render(Agent::Codex, "PermissionRequest", Effect::Allow, "r");
        assert_eq!(serde_json::from_str::<Value>(&o).unwrap()["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let (o, code) = render(Agent::Grok, "PreToolUse", Effect::Deny, "r");
        assert_eq!(serde_json::from_str::<Value>(&o).unwrap()["decision"], "deny"); assert_eq!(code, 2);
    }
    #[test]
    fn merge_is_idempotent_and_keeps_others() {
        let user = r#"{"model":"x","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"mine.sh"}]}],"Stop":[{"hooks":[{"type":"command","command":"s.sh"}]}]}}"#;
        let a = merge(Agent::Claude, Some(user), "/opt/shellax", 130).unwrap();
        let b = merge(Agent::Claude, Some(&a), "/opt/shellax", 130).unwrap();
        assert_eq!(a, b);
        let v: Value = serde_json::from_str(&a).unwrap();
        assert_eq!(v["model"], "x");
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(v["hooks"]["PreToolUse"][1]["hooks"][0]["onFailure"], "block");
        let back: Value = serde_json::from_str(&unmerge(&a).unwrap()).unwrap();
        assert_eq!(back, serde_json::from_str::<Value>(user).unwrap());
        let cx: Value = serde_json::from_str(&merge(Agent::Codex, None, "shellax", 130).unwrap()).unwrap();
        assert!(cx["hooks"]["PermissionRequest"].is_array());
        assert!(merge(Agent::Claude, Some("{bad"), "a", 1).is_err());
    }
    #[test]
    fn install_backs_up_and_uninstalls() {
        let home = std::env::temp_dir().join(format!("as-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), r#"{"theme":"dark"}"#).unwrap();
        let r = install(Agent::Claude, &home, "/x/shellax", 130, false, false).unwrap();
        assert!(r.backup.as_ref().unwrap().exists() && installed(Agent::Claude, &home));
        assert!(!install(Agent::Claude, &home, "/x/shellax", 130, false, false).unwrap().changed);
        install(Agent::Claude, &home, "", 0, true, false).unwrap();
        assert!(!installed(Agent::Claude, &home));
        install(Agent::Grok, &home, "/x/shellax", 130, false, false).unwrap();
        assert!(installed(Agent::Grok, &home));
        install(Agent::Grok, &home, "", 0, true, false).unwrap();
        assert!(!home.join(".grok/hooks/shellax.json").exists());
        let _ = std::fs::remove_dir_all(&home);
    }
}
