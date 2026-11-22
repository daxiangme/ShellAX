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

/// Build a session reference from a raw ID or (for `pi` / `omp`) absolute session file path.
pub fn session_ref_for_agent(agent: &str, value: impl Into<String>) -> Option<AgentSessionRef> {
    let value = value.into();
    let norm = crate::agent::normalize_id(agent);
    if matches!(norm.as_str(), "pi" | "omp") && Path::new(&value).is_absolute() {
        AgentSessionRef::path(value)
    } else {
        AgentSessionRef::id(value)
    }
}

/// Build a strictly validated shell resume line from structured `(agent, session_id)`.
pub fn build_validated_resume_line(agent: &str, session_id: &str) -> Option<String> {
    let sref = session_ref_for_agent(agent, session_id)?;
    let p = plan(agent, &sref)?;
    shell_line(&p.argv)
}

/// Parse a previously saved resume shell line (including legacy `layout.json` entries before
/// structured `agent_session` persistence) back into `(normalized_agent_id, Option<AgentSessionRef>)`.
/// Generic fallback flags (`--continue`, `--last`, `latest`) return `(agent_id, None)`.
pub fn parse_resume_command(cmd: &str) -> Option<(String, Option<AgentSessionRef>)> {
    let tokens = split_simple_shell_line(cmd)?;
    let first = tokens.first()?.as_str();
    let stem = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_ascii_lowercase();
    let stem = stem
        .strip_suffix(".exe")
        .or_else(|| stem.strip_suffix(".cmd"))
        .or_else(|| stem.strip_suffix(".bat"))
        .unwrap_or(&stem);
    let agent = match stem {
        "claude" | "claude-code" => "claude",
        "codex" => "codex",
        "copilot" | "github-copilot" => "copilot",
        "devin" => "devin",
        "droid" => "droid",
        "kimi" => "kimi",
        "pi" => "pi",
        "omp" => "omp",
        "hermes" => "hermes",
        "opencode" => "opencode",
        "qodercli" => "qodercli",
        "qwen" => "qwen",
        "kilo" => "kilo",
        "cursor" | "cursor-agent" => "cursor",
        "agy" | "antigravity" => "agy",
        "grok" => "grok",
        "gemini" => "gemini",
        "letta" => "letta",
        _ => return None,
    };
    let rest = &tokens[1..];
    let pick_id = |raw: &str| -> Option<AgentSessionRef> {
        let v = raw.trim();
        if v.is_empty() || v.starts_with('-') || v.eq_ignore_ascii_case("latest") {
            return None;
        }
        session_ref_for_agent(agent, v)
    };
    let sref = match (agent, rest) {
        ("codex", [sub, val]) if sub == "resume" => pick_id(val),
        ("copilot" | "omp", [arg]) => arg.strip_prefix("--resume=").and_then(pick_id),
        ("letta", [f1, val]) if f1 == "--conversation" => pick_id(val),
        ("letta", [f1, def, f2, aid])
            if f1 == "--conversation" && def == "default" && f2 == "--agent" =>
        {
            let aid = aid.trim();
            if aid.is_empty() || aid.starts_with('-') {
                None
            } else {
                AgentSessionRef::id(format!("default:{aid}"))
            }
        }
        ("agy", [f, val]) if f == "--conversation" => pick_id(val),
        ("kimi" | "pi" | "opencode" | "kilo", [f, val]) if f == "--session" => pick_id(val),
        (
            "claude" | "devin" | "droid" | "hermes" | "qodercli" | "qwen" | "cursor" | "grok"
            | "gemini",
            [f, val],
        ) if f == "--resume" || f == "-r" => pick_id(val),
        (
            "claude" | "devin" | "droid" | "hermes" | "qodercli" | "qwen" | "cursor" | "grok"
            | "gemini",
            [arg],
        ) => arg.strip_prefix("--resume=").and_then(pick_id),
        _ => None,
    };
    Some((agent.to_string(), sref))
}

fn split_simple_shell_line(line: &str) -> Option<Vec<String>> {
    let s = line.trim();
    if s.is_empty() || s.chars().any(char::is_control) {
        return None;
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                cur.push(ch);
            }
        } else if ch == '\'' {
            in_single = true;
        } else if matches!(ch, ';' | '|' | '&' | '$' | '`' | '<' | '>' | '(' | ')' | '"') {
            return None;
        } else if ch.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if in_single {
        return None;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    (!out.is_empty()).then_some(out)
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

    #[test]
    fn parses_legacy_and_structured_resume_commands() {
        assert_eq!(
            parse_resume_command("claude --resume 01a0de21-1234"),
            Some((
                "claude".into(),
                AgentSessionRef::id("01a0de21-1234"),
            ))
        );
        assert_eq!(
            parse_resume_command("codex resume e8168069"),
            Some(("codex".into(), AgentSessionRef::id("e8168069")))
        );
        assert_eq!(
            parse_resume_command("agy --conversation 547101a9-aa33"),
            Some(("agy".into(), AgentSessionRef::id("547101a9-aa33")))
        );
        assert_eq!(
            parse_resume_command("copilot --resume=abc-123"),
            Some(("copilot".into(), AgentSessionRef::id("abc-123")))
        );
        assert_eq!(
            parse_resume_command("pi --session '/tmp/my sessions/a.jsonl'"),
            Some((
                "pi".into(),
                AgentSessionRef::path("/tmp/my sessions/a.jsonl"),
            ))
        );
        assert_eq!(
            parse_resume_command("letta --conversation default --agent ag-9"),
            Some(("letta".into(), AgentSessionRef::id("default:ag-9")))
        );
        // Generic fallback commands must NOT masquerade as exact session IDs.
        assert_eq!(
            parse_resume_command("claude --continue"),
            Some(("claude".into(), None))
        );
        assert_eq!(
            parse_resume_command("codex resume --last"),
            Some(("codex".into(), None))
        );
        assert_eq!(
            parse_resume_command("gemini --resume latest"),
            Some(("gemini".into(), None))
        );
        // Unknown or shell-injected strings are rejected.
        assert_eq!(parse_resume_command("rm -rf /"), None);
        assert_eq!(
            parse_resume_command("claude --resume id; echo pwn"),
            None
        );
        assert_eq!(
            build_validated_resume_line("antigravity", "547101a9-aa33").as_deref(),
            Some("agy --conversation 547101a9-aa33")
        );
    }
}
