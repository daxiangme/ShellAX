//! List and reopen Claude, Codex, and Grok conversations using each CLI's own commands.
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Claude,
    Codex,
    Grok,
}

impl Kind {
    pub fn all() -> [Kind; 3] {
        [Kind::Claude, Kind::Codex, Kind::Grok]
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Grok => "grok",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Grok => "Grok",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub status: String,
    /// Still running in the CLI's own background supervisor.
    pub running: bool,
    pub context: Option<String>,
}

/// Command that prints a session list. Codex has no non-interactive list.
pub fn list_argv(kind: Kind) -> Option<Vec<String>> {
    match kind {
        Kind::Claude => Some(vec!["claude".into(), "agents".into(), "--json".into()]),
        Kind::Grok => Some(vec![
            "grok".into(),
            "sessions".into(),
            "list".into(),
            "-n".into(),
            "50".into(),
        ]),
        Kind::Codex => None,
    }
}

pub fn new_argv(kind: Kind) -> Vec<String> {
    vec![kind.id().into()]
}

/// Open the Codex command center inside a terminal.
pub fn codex_center_argv() -> Vec<String> {
    vec!["codex".into(), "agents".into()]
}

pub fn enter_argv(kind: Kind, id: &str, running: bool) -> Option<Vec<String>> {
    let id = id.trim();
    if id.is_empty()
        || id
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "\"'\\$`".contains(c))
    {
        return None;
    }
    Some(match kind {
        Kind::Claude if running => vec!["claude".into(), "attach".into(), id.into()],
        Kind::Claude => vec!["claude".into(), "--resume".into(), id.into()],
        Kind::Codex => vec!["codex".into(), "resume".into(), id.into()],
        Kind::Grok => vec!["grok".into(), "--resume".into(), id.into()],
    })
}

pub fn argv_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.chars().any(|c| c.is_whitespace()) {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse_list(kind: Kind, text: &str) -> Result<Vec<Chat>, String> {
    match kind {
        Kind::Claude => parse_claude(text),
        Kind::Grok => Ok(parse_grok(text)),
        Kind::Codex => Err("codex agents has no scripted list".into()),
    }
}

fn parse_claude(text: &str) -> Result<Vec<Chat>, String> {
    let text = text.trim();
    let start = text
        .find(['[', '{'])
        .ok_or_else(|| "claude agents --json did not print JSON".to_string())?;
    let v: Value = serde_json::from_str(&text[start..])
        .map_err(|e| format!("claude agents --json: {e}"))?;
    let items = v
        .as_array()
        .cloned()
        .or_else(|| {
            ["sessions", "agents", "items"]
                .iter()
                .find_map(|k| v.get(*k).and_then(|x| x.as_array()).cloned())
        })
        .ok_or_else(|| "claude agents --json was not a list".to_string())?;
    Ok(items.iter().filter_map(chat_from_json).collect())
}

fn chat_from_json(v: &Value) -> Option<Chat> {
    let id = first_str(v, &["id", "sessionId", "session_id", "uuid"])?;
    let name = first_str(v, &["name", "title", "displayName", "summary"]).unwrap_or_else(|| id.clone());
    let cwd = first_str(v, &["cwd", "directory", "path"]).unwrap_or_default();
    let status = first_str(v, &["status", "state"]).unwrap_or_default();
    let running = v
        .get("running")
        .and_then(|x| x.as_bool())
        .or_else(|| v.get("active").and_then(|x| x.as_bool()))
        .unwrap_or(true);
    let context = first_str(
        v,
        &["context", "contextUsage", "context_usage", "usage"],
    )
    .or_else(|| {
        v.get("contextPercent")
            .or_else(|| v.get("context_percent"))
            .and_then(|n| n.as_f64())
            .map(|n| format!("{n:.0}%"))
    });
    Some(Chat {
        id,
        name,
        cwd,
        status,
        running,
        context,
    })
}

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        v.get(*k).and_then(|x| match x {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

fn parse_grok(text: &str) -> Vec<Chat> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let id = uuid_in(line).unwrap_or_default();
        if id.is_empty() && !line.chars().any(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let name = if id.is_empty() {
            line.to_string()
        } else {
            line.replace(&id, "").split_whitespace().collect::<Vec<_>>().join(" ")
        };
        out.push(Chat {
            id,
            name: if name.is_empty() { line.to_string() } else { name },
            cwd: String::new(),
            status: String::new(),
            running: false,
            context: None,
        });
    }
    out
}

fn uuid_in(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 36 <= bytes.len() {
        let s = &line[i..i + 36];
        if is_uuid(s) {
            return Some(s.to_string());
        }
        i += 1;
    }
    None
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, c) in b.iter().enumerate() {
        let dash = matches!(i, 8 | 13 | 18 | 23);
        if dash {
            if *c != b'-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Context usage printed by an agent on its own screen. None when the screen does not say.
pub fn context_usage(screen: &str) -> Option<String> {
    let tail: String = screen
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let lower = tail.to_lowercase();
    if let Some(i) = lower.find("context") {
        let window = &tail[i..tail.len().min(i + 48)];
        if let Some(pct) = percent_in(window) {
            return Some(format!("{pct}%"));
        }
    }
    for line in tail.lines().rev() {
        if let Some(pct) = percent_in(line) {
            let l = line.to_lowercase();
            if l.contains("context") || l.contains("token") || l.contains("ctx") {
                return Some(format!("{pct}%"));
            }
        }
    }
    None
}

fn percent_in(s: &str) -> Option<u8> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && b[i] == b'%' {
                if let Ok(n) = s[start..i].parse::<u8>() {
                    if n <= 100 {
                        return Some(n);
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_json_list_and_enter_command() {
        let text = r#"note
[{"id":"abc","name":"auth","cwd":"/src","status":"running","contextPercent":12}]"#;
        let rows = parse_list(Kind::Claude, text).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "auth");
        assert_eq!(rows[0].context.as_deref(), Some("12%"));
        assert_eq!(
            enter_argv(Kind::Claude, &rows[0].id, true).unwrap(),
            vec!["claude", "attach", "abc"]
        );
        assert_eq!(
            enter_argv(Kind::Claude, "abc", false).unwrap(),
            vec!["claude", "--resume", "abc"]
        );
        assert!(enter_argv(Kind::Claude, "a b", true).is_none());
    }

    #[test]
    fn grok_text_list_and_codex_has_no_list() {
        let rows = parse_grok("0194f0a2-3b1c-7d8e-9f01-23456789abcd  auth rewrite\n");
        assert_eq!(rows[0].id, "0194f0a2-3b1c-7d8e-9f01-23456789abcd");
        assert!(rows[0].name.contains("auth"));
        assert!(list_argv(Kind::Codex).is_none());
        assert_eq!(
            enter_argv(Kind::Codex, "sess-1", false).unwrap(),
            vec!["codex", "resume", "sess-1"]
        );
        assert_eq!(
            enter_argv(Kind::Grok, &rows[0].id, false).unwrap()[0],
            "grok"
        );
    }

    #[test]
    fn reads_context_from_the_screen() {
        assert_eq!(
            context_usage("working\ncontext left 40%\n"),
            Some("40%".into())
        );
        assert_eq!(context_usage("hello"), None);
    }
}
