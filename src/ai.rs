//! Built-in AI client (BYOK): provider config, redaction, context building, minimal chat calls
//! for OpenAI-compatible (/chat/completions) and Anthropic (/v1/messages) APIs.
//! Written from the providers' public API documentation. The API key is passed in per call
//! and only ever placed in an HTTP header; it is never part of a returned error string.
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind { #[default] OpenAi, Anthropic }
impl Kind {
    pub const ALL: [Kind; 2] = [Kind::OpenAi, Kind::Anthropic];
    pub fn id(self) -> &'static str { match self { Kind::OpenAi => "openai", Kind::Anthropic => "anthropic" } }
    pub fn from_id(s: &str) -> Kind { if s == "anthropic" { Kind::Anthropic } else { Kind::OpenAi } }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub base_url: String,
    pub model: String,
    pub timeout_secs: u64,
    pub temperature: f32,
    pub max_tokens: u32,
    /// optional faster/cheaper model for command generation; empty = use `model`
    pub fast_model: String,
    /// preset template id this provider was created from ("openai", "ollama", "custom", …)
    pub preset: String,
}
impl Default for Provider {
    fn default() -> Self {
        Self { id: String::new(), name: String::new(), kind: Kind::OpenAi, base_url: "https://api.openai.com/v1".into(),
               model: "gpt-4.1-mini".into(), timeout_secs: 30, temperature: 0.2, max_tokens: 512, fast_model: String::new(), preset: "custom".into() }
    }
}
impl Provider {
    /// Keychain account name for this provider's API key.
    pub fn secret_name(&self) -> String { format!("ai-provider:{}", self.id) }
    /// Model used for natural-language → command.
    pub fn command_model(&self) -> &str { if self.fast_model.trim().is_empty() { &self.model } else { self.fast_model.trim() } }
}

// ---------- redaction ----------
fn redactors() -> &'static [(Regex, &'static str)] {
    static R: std::sync::OnceLock<Vec<(Regex, &'static str)>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        let p = |s: &str| Regex::new(s).unwrap();
        vec![
            (p(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(-----END [A-Z ]*PRIVATE KEY-----|$)"), "[REDACTED_PRIVATE_KEY]"),
            (p(r"(?i)\b(authorization\s*:\s*(bearer|basic|token)\s+)[A-Za-z0-9._~+/=-]+"), "${1}[REDACTED]"),
            (p(r"\b(sk|pk|rk)-[A-Za-z0-9_-]{8,}"), "[REDACTED_KEY]"),
            (p(r"\b(ghp|gho|ghu|ghs|ghr|github_pat)_[A-Za-z0-9_]{10,}"), "[REDACTED_TOKEN]"),
            (p(r"\bxox[abprs]-[A-Za-z0-9-]{10,}"), "[REDACTED_TOKEN]"),
            (p(r"\b(AKIA|ASIA)[0-9A-Z]{16}\b"), "[REDACTED_AWS_KEY]"),
            (p(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"), "[REDACTED_JWT]"),
            (p(r"(?i)\b([A-Z0-9_]*(password|passwd|pwd|secret|token|api_?key|access_?key)[A-Z0-9_]*\s*[=:]\s*)('[^']*'|\x22[^\x22]*\x22|\S+)"), "${1}[REDACTED]"),
            (p(r"(?i)(--?(password|passwd|token|api-key|secret)[= ])\S+"), "${1}[REDACTED]"),
            (p(r"(\b[a-z][a-z0-9+.-]*://[^:/\s@]+:)[^@\s/]+@"), "${1}[REDACTED]@"),
        ]
    })
}
/// Remove secrets from text before it leaves the machine. Returns (text, number of replacements).
pub fn redact(s: &str) -> (String, usize) {
    let mut out = s.to_string();
    let mut n = 0;
    for (re, rep) in redactors() {
        n += re.find_iter(&out).count();
        out = re.replace_all(&out, *rep).into_owned();
    }
    (out, n)
}

// ---------- context ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope { None, Selection, Command, Screen }
impl Scope {
    pub fn from_id(s: &str) -> Scope {
        match s { "none" => Scope::None, "selection" => Scope::Selection, "command" => Scope::Command, _ => Scope::Screen }
    }
}
pub struct ContextInput<'a> { pub selection: Option<&'a str>, pub last_command: Option<&'a str>, pub screen: &'a [String] }
pub struct ContextOpts { pub scope: Scope, pub lines: usize, pub send_output: bool, pub redact: bool }

/// Build the context block that will be sent. With `send_output=false` only command lines
/// (the last command) may be sent; selection/screen content is treated as output and withheld.
pub fn build_context(inp: &ContextInput, o: &ContextOpts) -> (String, usize) {
    let raw = match o.scope {
        Scope::None => String::new(),
        Scope::Command => inp.last_command.unwrap_or("").to_string(),
        Scope::Selection if o.send_output => inp.selection.unwrap_or("").to_string(),
        Scope::Screen if o.send_output => {
            let lines: Vec<&String> = inp.screen.iter().filter(|l| !l.trim().is_empty()).collect();
            let start = lines.len().saturating_sub(o.lines.max(1));
            lines[start..].iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
        }
        _ => inp.last_command.unwrap_or("").to_string(),
    };
    if o.redact { redact(&raw) } else { (raw, 0) }
}

pub const SYSTEM_PROMPT: &str = "You translate a natural-language request into ONE shell command for the user's terminal. \
Reply ONLY with JSON: {\"command\": \"...\", \"explanation\": \"one short sentence\"}. Never chain unrelated commands. \
Prefer safe, read-only commands when the request is ambiguous.";

pub fn build_user_prompt(request: &str, shell: &str, host: &str, context: &str) -> String {
    let mut s = format!("Shell: {shell}\nHost: {host}\nRequest: {request}\n");
    if !context.is_empty() { s.push_str("Terminal context:\n```\n"); s.push_str(context); s.push_str("\n```\n"); }
    s
}

/// Pull {command, explanation} out of a model reply (JSON, fenced code, or plain first line).
pub fn extract_command(reply: &str) -> (String, String) {
    let t = reply.trim();
    let json_part = t.find('{').and_then(|a| t.rfind('}').map(|b| &t[a..=b]));
    if let Some(j) = json_part {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(j) {
            if let Some(c) = v.get("command").and_then(|c| c.as_str()) {
                return (c.trim().to_string(), v.get("explanation").and_then(|e| e.as_str()).unwrap_or("").trim().to_string());
            }
        }
    }
    if let Some(a) = t.find("```") {
        let rest = &t[a + 3..];
        let rest = rest.split_once('\n').map(|x| x.1).unwrap_or(rest);
        if let Some(b) = rest.find("```") { return (rest[..b].trim().lines().next().unwrap_or("").to_string(), String::new()); }
    }
    (t.lines().next().unwrap_or("").trim().trim_matches('`').to_string(), String::new())
}

// ---------- HTTP ----------
pub struct Reply { pub text: String, pub latency: Duration, pub model: String }

fn scrub(s: &str, key: &str) -> String {
    let mut s: String = s.chars().take(300).collect();
    if key.len() >= 4 { s = s.replace(key, "[key]"); }
    s
}

/// One chat round-trip. `max_tokens` overrides the provider default (used by "test connection").
pub fn chat(p: &Provider, key: &str, system: &str, user: &str, max_tokens: Option<u32>) -> Result<Reply> {
    let base = crate::models::api_base(p.kind, &p.base_url);
    let agent = crate::proxy::ureq_builder(&base, ureq::AgentBuilder::new().timeout(Duration::from_secs(p.timeout_secs.max(1))))?.build();
    let base = base.as_str();
    let mt = max_tokens.unwrap_or(p.max_tokens);
    let t0 = Instant::now();
    let res = match p.kind {
        Kind::OpenAi => {
            let mut req = agent.post(&format!("{base}/chat/completions")).set("content-type", "application/json");
            if !key.is_empty() { req = req.set("authorization", &format!("Bearer {key}")); }
            req.send_json(serde_json::json!({
                "model": p.model, "max_tokens": mt, "temperature": p.temperature,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]
            }))
        }
        Kind::Anthropic => {
            let url = format!("{base}/messages");
            agent.post(&url).set("x-api-key", key).set("anthropic-version", "2023-06-01").set("content-type", "application/json")
                .send_json(serde_json::json!({
                    "model": p.model, "max_tokens": mt, "temperature": p.temperature, "system": system,
                    "messages": [{"role": "user", "content": user}]
                }))
        }
    };
    let latency = t0.elapsed();
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            return Err(anyhow!("HTTP {code}: {}", scrub(&body, key)));
        }
        Err(e) => return Err(anyhow!("{}", scrub(&e.to_string(), key))),
    };
    let v: serde_json::Value = resp.into_json().map_err(|e| anyhow!("bad JSON: {e}"))?;
    let text = match p.kind {
        Kind::OpenAi => v.pointer("/choices/0/message/content").and_then(|x| x.as_str()),
        Kind::Anthropic => v.pointer("/content/0/text").and_then(|x| x.as_str()),
    }.ok_or_else(|| anyhow!("unexpected response: {}", scrub(&v.to_string(), key)))?.to_string();
    let model = v.get("model").and_then(|m| m.as_str()).unwrap_or(&p.model).to_string();
    Ok(Reply { text, latency, model })
}

/// System prompt for the free-form assistant panel.
pub const CHAT_SYSTEM_PROMPT: &str = "You are the shellax terminal assistant. Reply in plain sentences. Do not use bold, headings, or numbered lists. \
Never claim you ran anything. Terminal context, if given, is already redacted. \
Shell commands go in fenced code blocks: one command per block, no leading $, no comments, no extra lines. \
If the user only wants an explanation, include no code block. \
If the user wants one action, include exactly one code block. \
If the user wants several checks, write one short sentence before each code block.";
/// System prompt for "explain this failure".
pub const EXPLAIN_SYSTEM_PROMPT: &str = "You explain why a shell command failed. Given the command, its exit status and the (redacted) tail of the terminal output, explain the most likely cause in 2-5 plain sentences and suggest exactly one fix command in its own fenced code block. No bold, no extra commands. Do not run anything.";

/// Prompt for "explain this failed command".
pub fn build_explain_prompt(cmd: &str, code: i32, host: &str, output_tail: &str) -> String {
    format!("Host: {host}\nCommand: {cmd}\nExit status: {code}\nTerminal output (tail, redacted):\n```\n{output_tail}\n```")
}

/// Streaming multi-turn chat. `messages` = (role "user"/"assistant", content). Calls `on_delta`
/// for each text fragment as it arrives (OpenAI `stream: true` SSE / Anthropic message stream).
pub fn chat_stream(p: &Provider, key: &str, system: &str, messages: &[(String, String)], mut on_delta: impl FnMut(&str) -> bool) -> Result<Reply> {
    use std::io::BufRead;
    let base = crate::models::api_base(p.kind, &p.base_url);
    let agent = crate::proxy::ureq_builder(&base, ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(p.timeout_secs.clamp(1, 30))).timeout_read(Duration::from_secs(p.timeout_secs.max(1))))?.build();
    let msgs: Vec<serde_json::Value> = messages.iter().map(|(r, c)| serde_json::json!({"role": r, "content": c})).collect();
    let t0 = Instant::now();
    let res = match p.kind {
        Kind::OpenAi => {
            let mut all = vec![serde_json::json!({"role": "system", "content": system})];
            all.extend(msgs);
            let mut req = agent.post(&format!("{base}/chat/completions")).set("content-type", "application/json").set("accept", "text/event-stream");
            if !key.is_empty() { req = req.set("authorization", &format!("Bearer {key}")); }
            req.send_json(serde_json::json!({"model": p.model, "max_tokens": p.max_tokens, "temperature": p.temperature, "stream": true, "messages": all}))
        }
        Kind::Anthropic => agent.post(&format!("{base}/messages")).set("x-api-key", key).set("anthropic-version", "2023-06-01").set("content-type", "application/json")
            .send_json(serde_json::json!({"model": p.model, "max_tokens": p.max_tokens, "temperature": p.temperature, "system": system, "stream": true, "messages": msgs})),
    };
    let resp = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => { let body = r.into_string().unwrap_or_default(); return Err(anyhow!("HTTP {code}: {}", scrub(&body, key))); }
        Err(e) => return Err(anyhow!("{}", scrub(&e.to_string(), key))),
    };
    let ctype = resp.header("content-type").unwrap_or("").to_string();
    let mut text = String::new();
    let mut model = p.model.clone();
    if !ctype.contains("event-stream") {
        // server ignored stream=true: plain JSON body
        let v: serde_json::Value = resp.into_json().map_err(|e| anyhow!("bad JSON: {e}"))?;
        let t = v.pointer("/choices/0/message/content").or_else(|| v.pointer("/content/0/text")).and_then(|x| x.as_str()).unwrap_or_default().to_string();
        on_delta(&t);
        return Ok(Reply { text: t, latency: t0.elapsed(), model });
    }
    let rd = std::io::BufReader::new(resp.into_reader());
    for line in rd.lines() {
        let line = line.map_err(|e| anyhow!("stream: {e}"))?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data == "[DONE]" { break; }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { continue };
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) { model = m.to_string(); }
        let delta = v.pointer("/choices/0/delta/content").or_else(|| v.pointer("/delta/text")).and_then(|x| x.as_str());
        if let Some(d) = delta { text.push_str(d); if !on_delta(d) { break; } }
        if v.get("type").and_then(|t| t.as_str()) == Some("message_stop") { break; }
        if let Some(e) = v.pointer("/error/message").and_then(|x| x.as_str()) { return Err(anyhow!("{}", scrub(e, key))); }
    }
    Ok(Reply { text, latency: t0.elapsed(), model })
}

/// Inline command completion: returns only the text to append after `prefix` (may be empty).
pub const COMPLETE_SYSTEM_PROMPT: &str = "Complete the user's partially typed shell command. Reply with the COMPLETE command only, one line, no explanation, no code fence.";
pub fn completion_suffix(prefix: &str, reply: &str) -> Option<String> {
    let line = reply.trim().trim_matches('`').lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.strip_prefix("$ ").unwrap_or(line);
    let suf = line.strip_prefix(prefix)?;
    (!suf.is_empty() && !suf.contains('\n')).then(|| suf.to_string())
}

/// Minimal real request (max_tokens = 1) to validate URL, key and model.
pub fn test_connection(p: &Provider, key: &str) -> Result<Reply> { chat(p, key, "ping", "ping", Some(1)) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_common_secrets() {
        let s = "export OPENAI_API_KEY=sk-abcdefghijklmnop\ncurl -H 'Authorization: Bearer abc.def.ghi' https://u:hunter2@h/x\nmysql --password=topsecret\nAKIAABCDEFGHIJKLMNOP\n-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
        let (r, n) = redact(s);
        for leak in ["sk-abcdefghijklmnop", "abc.def.ghi", "hunter2", "topsecret", "AKIAABCDEFGHIJKLMNOP", "AAAA"] {
            assert!(!r.contains(leak), "{leak} leaked: {r}");
        }
        assert!(n >= 6);
        assert_eq!(redact("ls -la /var/log").0, "ls -la /var/log");
    }
    #[test]
    fn context_scope_and_output_gate() {
        let screen = vec!["$ cat .env".to_string(), "TOKEN=abc123secret".into(), "$ ls".into(), "a b".into()];
        let inp = ContextInput { selection: Some("sel"), last_command: Some("ls"), screen: &screen };
        let o = |scope, send_output| ContextOpts { scope, lines: 3, send_output, redact: true };
        assert_eq!(build_context(&inp, &o(Scope::None, true)).0, "");
        assert_eq!(build_context(&inp, &o(Scope::Command, true)).0, "ls");
        assert_eq!(build_context(&inp, &o(Scope::Screen, false)).0, "ls", "output withheld when send_output=false");
        let (c, n) = build_context(&inp, &o(Scope::Screen, true));
        assert_eq!(c.lines().count(), 3);
        assert!(!c.contains("abc123secret") && n == 1, "{c}");
        assert_eq!(build_context(&inp, &o(Scope::Selection, true)).0, "sel");
    }
    #[test]
    fn extracts_command() {
        assert_eq!(extract_command("{\"command\":\"df -h\",\"explanation\":\"disk\"}").0, "df -h");
        assert_eq!(extract_command("Sure:\n```bash\nls -la\n```").0, "ls -la");
        assert_eq!(extract_command("`uptime`").0, "uptime");
    }

    /// Minimal one-shot HTTP server: returns (base_url, handle yielding the raw request).
    fn mock(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = c.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                let s = String::from_utf8_lossy(&buf).to_string();
                if let Some(i) = s.find("\r\n\r\n") {
                    let len = s.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    if buf.len() >= i + 4 + len { break; }
                }
                if n == 0 { break; }
            }
            let resp = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            c.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).to_string()
        });
        (format!("http://{addr}/v1"), h)
    }
    #[test]
    fn openai_request_key_only_in_header_and_redacted_prompt() {
        let (base, h) = mock(200, r#"{"model":"mock-1","choices":[{"message":{"content":"{\"command\":\"df -h\",\"explanation\":\"disk\"}"}}]}"#);
        let p = Provider { id: "t".into(), base_url: base, model: "m".into(), ..Default::default() };
        let (ctx, _) = redact("export API_KEY=sk-livesecretvalue123");
        let r = chat(&p, "sk-TESTKEY-9999", SYSTEM_PROMPT, &build_user_prompt("disk usage", "bash", "local", &ctx), None).unwrap();
        assert_eq!(extract_command(&r.text).0, "df -h");
        let req = h.join().unwrap();
        let (head, body) = req.split_once("\r\n\r\n").unwrap();
        assert!(head.to_lowercase().contains("authorization: bearer sk-testkey-9999"));
        assert!(!body.contains("sk-TESTKEY-9999"), "key leaked into body");
        assert!(!body.contains("sk-livesecretvalue123"), "unredacted context sent");
    }
    #[test]
    fn errors_never_echo_the_key() {
        let (base, h) = mock(401, r#"{"error":"invalid key sk-TESTKEY-9999"}"#);
        let p = Provider { id: "t".into(), base_url: base, kind: Kind::OpenAi, ..Default::default() };
        let e = test_connection(&p, "sk-TESTKEY-9999").err().unwrap().to_string();
        h.join().unwrap();
        assert!(e.contains("401") && !e.contains("sk-TESTKEY-9999"), "{e}");
    }
    #[test]
    fn anthropic_headers() {
        let (base, h) = mock(200, r#"{"model":"claude-x","content":[{"type":"text","text":"ok"}]}"#);
        let p = Provider { id: "a".into(), kind: Kind::Anthropic, base_url: base, ..Default::default() };
        let r = test_connection(&p, "ak-1234").unwrap();
        assert_eq!(r.text, "ok");
        let req = h.join().unwrap().to_lowercase();
        assert!(req.starts_with("post /v1/messages") && req.contains("x-api-key: ak-1234") && req.contains("anthropic-version: 2023-06-01"));
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    #[test]
    fn completion_suffix_rules() {
        assert_eq!(completion_suffix("git st", "git status"), Some("atus".into()));
        assert_eq!(completion_suffix("git st", "`git stash list`"), Some("ash list".into()));
        assert_eq!(completion_suffix("ls", "rm -rf /"), None);
        assert_eq!(completion_suffix("ls", "ls"), None);
    }
    #[test]
    fn openai_sse_stream_is_parsed() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 4096]; let _ = s.read(&mut b);
            let body = "data: {\"model\":\"m1\",\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\ndata: [DONE]\n\n";
            let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}", body.len());
        });
        let p = Provider { id: "t".into(), name: "t".into(), kind: Kind::OpenAi, base_url: format!("http://127.0.0.1:{port}/v1"), model: "m0".into(), timeout_secs: 5, max_tokens: 50, ..Default::default() };
        let mut parts = vec![];
        let r = chat_stream(&p, "", "s", &[("user".into(), "hi".into())], |d| { parts.push(d.to_string()); true }).unwrap();
        assert_eq!(parts, vec!["Hel", "lo"]);
        assert_eq!(r.text, "Hello");
        assert_eq!(r.model, "m1");
    }
}
