//! Session resume command planning and validation across CLI coding agents.
//!
//! Portions (marked [3p]) Copyright (c) herdr contributors. Licensed under the Apache License,
//! Version 2.0; modified by ShellAX. See THIRD_PARTY_NOTICES.md.

use std::path::Path;

use serde::{Deserialize, Serialize};

const MAX_SESSION_ID_LEN: usize = 512;
const MAX_SESSION_PATH_LEN: usize = 4096;
const MAX_RESUME_ARGS: usize = 64;
const MAX_RESUME_ARGV_BYTES: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionRef {
    pub kind: AgentSessionRefKind,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRefKind {
    Id,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResumePlan {
    pub agent: String,
    pub argv: Vec<String>,
    pub dedupe_key: String,
}

impl AgentSessionRef {
    pub fn id(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_id(&value).then_some(Self {
            kind: AgentSessionRefKind::Id,
            value,
        })
    }

    pub fn path(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_session_path(&value).then_some(Self {
            kind: AgentSessionRefKind::Path,
            value,
        })
    }
}

/// Validate a resume argv before typing it into a shell. [3p]
pub fn validate_resume_argv(argv: &[String]) -> Result<(), String> {
    let Some(command) = argv.first() else {
        return Err("resume_argv must not be empty".into());
    };
    if argv.len() > MAX_RESUME_ARGS {
        return Err(format!(
            "resume_argv allows at most {MAX_RESUME_ARGS} arguments"
        ));
    }
    if argv.iter().map(String::len).sum::<usize>() > MAX_RESUME_ARGV_BYTES {
        return Err(format!(
            "resume_argv allows at most {MAX_RESUME_ARGV_BYTES} bytes"
        ));
    }
    if argv.iter().any(|arg| arg.chars().any(char::is_control)) {
        return Err("resume_argv must not contain control characters".into());
    }
    if argv.iter().any(|arg| arg.contains('\'')) {
        return Err("resume_argv must not contain apostrophes".into());
    }
    let plain_command = !command.is_empty()
        && !command.starts_with('-')
        && command
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if !plain_command {
        return Err("resume_argv must start with a plain command name, not a path".into());
    }
    Ok(())
}

/// Build the CLI command argv to resume `agent` on `session_ref`. [3p]
pub fn plan(agent: &str, session_ref: &AgentSessionRef) -> Option<AgentResumePlan> {
    let agent = agent.trim().to_ascii_lowercase();
    let v = &session_ref.value;
    if v.contains('\'') || v.chars().any(char::is_control) {
        return None;
    }
    let argv: Vec<String> = match (agent.as_str(), session_ref.kind) {
        ("claude" | "claude-code", AgentSessionRefKind::Id) => {
            vec!["claude".into(), "--resume".into(), v.clone()]
        }
        ("codex", AgentSessionRefKind::Id) => vec!["codex".into(), "resume".into(), v.clone()],
        ("copilot" | "github-copilot", AgentSessionRefKind::Id) => {
            vec!["copilot".into(), format!("--resume={v}")]
        }
        ("devin", AgentSessionRefKind::Id) => vec!["devin".into(), "--resume".into(), v.clone()],
        ("droid", AgentSessionRefKind::Id) => vec!["droid".into(), "--resume".into(), v.clone()],
        ("kimi", AgentSessionRefKind::Id) => vec!["kimi".into(), "--session".into(), v.clone()],
        ("pi", AgentSessionRefKind::Path | AgentSessionRefKind::Id) => {
            vec!["pi".into(), "--session".into(), v.clone()]
        }
        ("omp", AgentSessionRefKind::Path | AgentSessionRefKind::Id) => {
            vec!["omp".into(), format!("--resume={v}")]
        }
        ("hermes", AgentSessionRefKind::Id) => vec!["hermes".into(), "--resume".into(), v.clone()],
        ("opencode", AgentSessionRefKind::Id) => {
            vec!["opencode".into(), "--session".into(), v.clone()]
        }
        ("qodercli", AgentSessionRefKind::Id) => {
            vec!["qodercli".into(), "--resume".into(), v.clone()]
        }
        ("qwen", AgentSessionRefKind::Id) => vec!["qwen".into(), "--resume".into(), v.clone()],
        ("kilo", AgentSessionRefKind::Id) => vec!["kilo".into(), "--session".into(), v.clone()],
        ("cursor" | "cursor-agent", AgentSessionRefKind::Id) => vec![
            if cfg!(windows) {
                "cursor-agent.cmd"
            } else {
                "cursor-agent"
            }
            .into(),
            "--resume".into(),
            v.clone(),
        ],
        ("antigravity" | "agy", AgentSessionRefKind::Id) => {
            vec!["agy".into(), "--conversation".into(), v.clone()]
        }
        ("grok", AgentSessionRefKind::Id) => vec!["grok".into(), "--resume".into(), v.clone()],
        ("gemini", AgentSessionRefKind::Id) => vec!["gemini".into(), "--resume".into(), v.clone()],
        ("letta", AgentSessionRefKind::Id) => {
            if let Some(agent_id) = v.strip_prefix("default:") {
                if agent_id.is_empty() {
                    return None;
                }
                vec![
                    "letta".into(),
                    "--conversation".into(),
                    "default".into(),
                    "--agent".into(),
                    agent_id.into(),
                ]
            } else {
                vec!["letta".into(), "--conversation".into(), v.clone()]
            }
        }
        _ => return None,
    };
    validate_resume_argv(&argv).ok()?;
    Some(AgentResumePlan {
        dedupe_key: format!("{agent}\u{0}{:?}\u{0}{v}", session_ref.kind),
        agent,
        argv,
    })
}

/// Format a validated resume `argv` as a single shell command line.
pub fn shell_line(argv: &[String]) -> Option<String> {
    validate_resume_argv(argv).ok()?;
    Some(
        argv.iter()
            .map(|a| {
                if a.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'=')
                }) && !a.is_empty()
                {
                    a.clone()
                } else {
                    format!("'{a}'")
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Fallback resume command when no explicit session ID was reported yet.
pub fn fallback_continue_command(agent: &str) -> Option<&'static str> {
    match agent.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => Some("claude --continue"),
        "codex" => Some("codex resume --last"),
        "gemini" => Some("gemini --resume latest"),
        "opencode" => Some("opencode --continue"),
        _ => None,
    }
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_ID_LEN
        && !value.chars().any(char::is_control)
        && !value.contains('\'')
}

fn valid_session_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_PATH_LEN
        && !value.chars().any(char::is_control)
        && !value.contains('\'')
        && Path::new(value).is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn validates_resume_argv_strictly() {
        assert!(validate_resume_argv(&argv(&["claude", "--resume", "01a0de21"])).is_ok());
        assert!(validate_resume_argv(&argv(&["cursor-agent.cmd", "--resume", "id"])).is_ok());
        for bad in [
            argv(&[]),
            argv(&[""]),
            argv(&["/usr/bin/claude", "--resume", "id"]),
            argv(&["-claude"]),
            argv(&["claude", "bad\nline"]),
            argv(&["claude", "can's"]),
        ] {
            assert!(validate_resume_argv(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn builds_plans_across_agents() {
        let r = AgentSessionRef::id("e8168069-1323").unwrap();
        assert_eq!(
            plan("claude", &r).unwrap().argv,
            vec!["claude", "--resume", "e8168069-1323"]
        );
        assert_eq!(
            plan("codex", &r).unwrap().argv,
            vec!["codex", "resume", "e8168069-1323"]
        );
        assert_eq!(
            plan("opencode", &r).unwrap().argv,
            vec!["opencode", "--session", "e8168069-1323"]
        );
        assert!(AgentSessionRef::id("bad'quote").is_none());
        assert!(AgentSessionRef::id("bad\x07ctrl").is_none());
    }
}
