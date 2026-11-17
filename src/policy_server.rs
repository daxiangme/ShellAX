//! Local policy service (PRD 6.9): a tiny HTTP server on 127.0.0.1 that answers permission
//! questions from external agents' hooks (via the `shellax` CLI or Claude Code `http` hooks)
//! and from remote hosts through an SSH reverse port forward. Every request needs the bearer
//! token from the endpoint file (0600). "Ask" decisions are forwarded to the UI as an
//! [`Approval`] and resolved by the user, by timeout (`agents.on_timeout`), or passed back as
//! "ask" when no UI is attached. Every request and outcome is written to the audit log.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::AuditLog;
use crate::hooks::{self, Agent, HookReq};
use crate::policy::{self, Effect, Mode, Risk, Rule};

/// Policy inputs, read fresh for every request.
#[derive(Clone, Debug)]
pub struct PolicyCtx {
    pub mode: Mode,
    pub max_auto: Risk,
    pub rules: Vec<Rule>,
    pub dangerous_confirm: bool,
    pub prod_confirm: bool,
    /// Host names / addresses marked production.
    pub prod_hosts: Vec<String>,
    pub approval_timeout: Duration,
    /// On approval timeout: true = deny, false = keep waiting (until the hook itself times out).
    pub timeout_deny: bool,
    /// Policy when an agent/API writes to or closes a pane it did not spawn (`ask` | `allow` | `deny`).
    pub write_to_pty: String,
    /// Policy when an agent/API spawns a new pane or child agent (`allow` | `ask` | `deny`).
    pub api_spawn: String,
}
impl Default for PolicyCtx {
    fn default() -> Self {
        Self {
            mode: Mode::Ask,
            max_auto: Risk::R1,
            rules: vec![],
            dangerous_confirm: true,
            prod_confirm: true,
            prod_hosts: vec![],
            approval_timeout: Duration::from_secs(120),
            timeout_deny: true,
            write_to_pty: "ask".into(),
            api_spawn: "allow".into(),
        }
    }
}

/// A pending "ask" for the UI.
#[derive(Debug)]
pub struct Approval {
    pub id: u64,
    pub principal: String,
    pub req: HookReq,
    pub host: String,
    pub risk: Risk,
    pub reason: String,
    pub deadline: Instant,
    pub reply: flume::Sender<Answer>,
}

/// User's answer on the approval card (PRD 3.4: once / always this prefix for the session / reject
/// with feedback). The deny reason is passed back to the agent verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Once,
    Session,
    Deny(String),
}

/// Command prefix used for "always allow this prefix in this session": the program plus its
/// sub-command when there is one (`git push`, `kubectl get`, `rm`).
pub fn command_prefix(cmd: &str) -> String {
    let mut it = cmd.split_whitespace().filter(|w| !w.contains('='));
    let a = it.next().unwrap_or("");
    match it.next() {
        Some(b)
            if !b.starts_with('-')
                && b.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            format!("{a} {b}")
        }
        _ => a.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
}

pub fn endpoint_path(data_dir: &Path) -> PathBuf {
    data_dir.join("policy-endpoint.json")
}

pub fn read_endpoint(data_dir: &Path) -> Option<Endpoint> {
    if let (Ok(url), Ok(token)) = (std::env::var("SHELLAX_URL"), std::env::var("SHELLAX_TOKEN")) {
        return Some(Endpoint {
            url,
            token,
            pid: 0,
            host: std::env::var("SHELLAX_HOST").unwrap_or_default(),
        });
    }
    serde_json::from_str(&std::fs::read_to_string(endpoint_path(data_dir)).ok()?).ok()
}

pub type CtxFn = Arc<dyn Fn() -> PolicyCtx + Send + Sync>;

/// `/v1/open` target: the running App receives ssh:// / sftp:// links here (single instance).
pub static OPEN_TX: std::sync::OnceLock<flume::Sender<String>> = std::sync::OnceLock::new();

/// `/v1/pane/*` & `/v1/agent/*` target: orchestration commands dispatched onto the GPUI UI thread.
pub static PANE_TX: std::sync::OnceLock<flume::Sender<PaneCmd>> = std::sync::OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub tab: usize,
    pub title: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<crate::agent::AgentState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

#[derive(Debug)]
pub enum PaneCmd {
    List {
        reply: flume::Sender<Vec<PaneInfo>>,
    },
    IsChild {
        caller_pane: String,
        target_pane: String,
        reply: flume::Sender<bool>,
    },
    Split {
        caller_pane: Option<String>,
        from: Option<String>,
        dir: crate::layout::Dir,
        cmd: Option<String>,
        cwd: Option<String>,
        focus: bool,
        reply: flume::Sender<Result<String, String>>,
    },
    SendText {
        pane_id: String,
        text: String,
        enter: bool,
        reply: flume::Sender<Result<(), String>>,
    },
    SendKeys {
        pane_id: String,
        keys: Vec<String>,
        reply: flume::Sender<Result<(), String>>,
    },
    Read {
        pane_id: String,
        lines: usize,
        reply: flume::Sender<Result<String, String>>,
    },
    Close {
        pane_id: String,
        reply: flume::Sender<Result<(), String>>,
    },
    AgentStart {
        caller_pane: Option<String>,
        agent: String,
        cwd: Option<String>,
        prompt: Option<String>,
        task: Option<String>,
        reply: flume::Sender<Result<serde_json::Value, String>>,
    },
    AgentPrompt {
        pane_id: String,
        prompt: String,
        reply: flume::Sender<Result<serde_json::Value, String>>,
    },
    Wait {
        pane_id: String,
        until: Vec<crate::agent::AgentState>,
        timeout: Duration,
        reply: flume::Sender<Result<serde_json::Value, String>>,
    },
    ReportAgent {
        pane_id: String,
        agent: String,
        event: String,
        session_id: Option<String>,
        transcript_path: Option<String>,
        state: Option<crate::agent::AgentState>,
    },
}

pub struct Server {
    pub port: u16,
    pub token: String,
}

pub fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Start the server on `bind` (e.g. "127.0.0.1:0"). Writes the endpoint file when given.
pub fn start(
    bind: &str,
    token: Option<String>,
    ctx: CtxFn,
    approvals: Option<flume::Sender<Approval>>,
    audit: Arc<AuditLog>,
    endpoint_file: Option<PathBuf>,
) -> Result<Server> {
    start_with_ops(bind, token, ctx, approvals, audit, endpoint_file, None)
}

/// Like [`start`], plus the agent-facing remote operations (`/v1/hosts`, `/v1/exec`,
/// `/v1/upload`, `/v1/download`) backed by the App's connection pool.
pub fn start_with_ops(
    bind: &str,
    token: Option<String>,
    ctx: CtxFn,
    approvals: Option<flume::Sender<Approval>>,
    audit: Arc<AuditLog>,
    endpoint_file: Option<PathBuf>,
    ops: Option<Arc<dyn crate::broker::Ops>>,
) -> Result<Server> {
    let listener = TcpListener::bind(bind)?;
    let port = listener.local_addr()?.port();
    let token = token.unwrap_or_else(new_token);
    if let Some(p) = &endpoint_file {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let ep = Endpoint {
            url: format!("http://127.0.0.1:{port}"),
            token: token.clone(),
            pid: std::process::id(),
            host: String::new(),
        };
        write_private(p, &serde_json::to_string_pretty(&ep)?)?;
    }
    let shared = Arc::new(Shared {
        token: token.clone(),
        ctx,
        approvals,
        audit,
        next: AtomicU64::new(1),
        session_allow: Default::default(),
        ops,
    });
    std::thread::Builder::new()
        .name("policy-server".into())
        .spawn(move || {
            for s in listener.incoming().flatten() {
                let sh = shared.clone();
                std::thread::spawn(move || {
                    let _ = handle(s, &sh);
                });
            }
        })?;
    Ok(Server { port, token })
}

fn write_private(p: &Path, text: &str) -> Result<()> {
    let tmp = p.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

struct Shared {
    token: String,
    ctx: CtxFn,
    approvals: Option<flume::Sender<Approval>>,
    audit: Arc<AuditLog>,
    next: AtomicU64,
    ops: Option<Arc<dyn crate::broker::Ops>>,
    /// (principal, agent session, command prefix) → highest risk the user allowed for it
    session_allow: std::sync::Mutex<std::collections::HashMap<(String, String, String), Risk>>,
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn respond(
    mut s: TcpStream,
    status: &str,
    body: &str,
    extra: &[(&str, String)],
) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in extra {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    s.write_all(h.as_bytes())?;
    s.write_all(body.as_bytes())?;
    s.flush()
}

fn handle(stream: TcpStream, sh: &Shared) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or("").to_string(),
        parts.next().unwrap_or("").to_string(),
    );
    let (mut len, mut auth, mut host_hdr, mut pane_hdr) =
        (0usize, String::new(), String::new(), String::new());
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.parse().unwrap_or(0),
                "authorization" => auth = v.strip_prefix("Bearer ").unwrap_or(&v).to_string(),
                "x-shellax-token" => auth = v,
                "x-shellax-host" => host_hdr = v,
                "x-shellax-pane" => pane_hdr = v,
                _ => {}
            }
        }
    }
    if len > 1 << 20 {
        return Ok(respond(stream, "413 Payload Too Large", "{}", &[])?);
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).to_string();
    let (route, query) = path
        .split_once('?')
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .unwrap_or_else(|| (path.clone(), String::new()));
    if method == "GET" && route == "/v1/health" {
        return Ok(respond(
            stream,
            "200 OK",
            r#"{"ok":true,"service":"shellax-policy"}"#,
            &[],
        )?);
    }
    if !ct_eq(&auth, &sh.token) {
        let _ = sh
            .audit
            .append("policy.auth_fail", "policy", &format!("path={route}"));
        return Ok(respond(
            stream,
            "401 Unauthorized",
            r#"{"error":"bad token"}"#,
            &[],
        )?);
    }
    if route.starts_with("/v1/pane/") || route.starts_with("/v1/agent/") {
        let (status, out) = handle_pane_api(
            &method,
            &route,
            &query,
            &body,
            &pane_hdr,
            &host_hdr,
            sh,
            &stream,
        );
        return Ok(respond(stream, status, &out.to_string(), &[])?);
    }
    if method != "POST" {
        return Ok(respond(stream, "405 Method Not Allowed", "{}", &[])?);
    }
    if route == "/v1/check" {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Check {
            principal: String,
            action: String,
            command: String,
            host: String,
            cwd: String,
            interactive: Option<bool>,
        }
        let c: Check = match serde_json::from_str(&body) {
            Ok(c) => c,
            Err(e) => {
                return Ok(respond(
                    stream,
                    "400 Bad Request",
                    &json!({"error": e.to_string()}).to_string(),
                    &[],
                )?)
            }
        };
        let principal = if c.principal.is_empty() {
            "cli".to_string()
        } else {
            c.principal
        };
        let req = HookReq {
            event: "Check".into(),
            tool: "Bash".into(),
            command: c.command,
            action: if c.action.is_empty() {
                "exec".into()
            } else {
                c.action
            },
            cwd: c.cwd,
            session: String::new(),
            ..Default::default()
        };
        let host = if c.host.is_empty() { host_hdr } else { c.host };
        let (eff, risk, reason) = sh.decide_peer(
            &principal,
            &req,
            &host,
            c.interactive.unwrap_or(true),
            Some(&stream),
        );
        let d = match eff {
            Effect::Allow => "allow",
            Effect::Ask => "ask",
            Effect::Deny => "deny",
        };
        return Ok(respond(
            stream,
            "200 OK",
            &json!({"decision": d, "risk": risk.label(), "reason": reason}).to_string(),
            &[],
        )?);
    }
    if path == "/v1/open" {
        // only local callers (the second launch of the App); a remote agent may not open sessions here
        if !host_hdr.is_empty() {
            return Ok(respond(
                stream,
                "403 Forbidden",
                r#"{"error":"local callers only"}"#,
                &[],
            )?);
        }
        let url = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["url"].as_str().map(str::to_string))
            .unwrap_or_default();
        if let Err(e) = crate::link::parse(&url) {
            return Ok(respond(
                stream,
                "400 Bad Request",
                &json!({"error": e}).to_string(),
                &[],
            )?);
        }
        let Some(tx) = OPEN_TX.get() else {
            return Ok(respond(
                stream,
                "501 Not Implemented",
                r#"{"error":"no UI"}"#,
                &[],
            )?);
        };
        let _ = sh.audit.append(
            "link.open",
            "user",
            &format!(
                "url={:?} via=second-instance",
                crate::link::parse(&url)
                    .map(|l| format!(
                        "{:?}://{}{}",
                        l.kind,
                        l.user.map(|u| u + "@").unwrap_or_default(),
                        l.host
                    ))
                    .unwrap_or_default()
            ),
        );
        let _ = tx.send(url);
        return Ok(respond(stream, "200 OK", r#"{"ok":true}"#, &[])?);
    }
    if matches!(
        path.as_str(),
        "/v1/hosts" | "/v1/exec" | "/v1/upload" | "/v1/download"
    ) {
        let (status, out) = remote_op(sh, &path, &body, &host_hdr, Some(&stream));
        return Ok(respond(stream, status, &out.to_string(), &[])?);
    }
    if let Some(rest) = route.strip_prefix("/v1/hook/") {
        let mut it = rest.split('/');
        let Some(agent) = it.next().and_then(Agent::parse) else {
            return Ok(respond(
                stream,
                "404 Not Found",
                r#"{"error":"unknown agent"}"#,
                &[],
            )?);
        };
        let mut req = match hooks::parse_request(&body) {
            Ok(r) => r,
            Err(e) => {
                return Ok(respond(
                    stream,
                    "400 Bad Request",
                    &json!({"error": e.to_string()}).to_string(),
                    &[],
                )?)
            }
        };
        if let Some(ev) = it.next().filter(|e| !e.is_empty()) {
            req.event = ev.to_string();
        }
        // Forward session identity and lifecycle state to the UI pane when `X-Shellax-Pane` is set.
        if !pane_hdr.is_empty() && !req.is_subagent {
            if let Some(tx) = PANE_TX.get() {
                let state = hooks::state_for_event(&req.event);
                if state.is_some() || !req.session.is_empty() {
                    let _ = tx.send(PaneCmd::ReportAgent {
                        pane_id: pane_hdr.clone(),
                        agent: agent.id().to_string(),
                        event: req.event.clone(),
                        session_id: (!req.session.is_empty()).then(|| req.session.clone()),
                        transcript_path: (!req.transcript_path.is_empty())
                            .then(|| req.transcript_path.clone()),
                        state,
                    });
                }
            }
        }
        if hooks::is_state_only_event(&req.event) {
            return Ok(respond(
                stream,
                "200 OK",
                "",
                &[("X-shellax-Exit", "0".to_string())],
            )?);
        }
        let (eff, _risk, reason) = sh.decide_peer(agent.id(), &req, &host_hdr, true, Some(&stream));
        let (out, code) = hooks::render(agent, &req.event, eff, &reason);
        return Ok(respond(
            stream,
            "200 OK",
            &out,
            &[("X-shellax-Exit", code.to_string())],
        )?);
    }
    Ok(respond(stream, "404 Not Found", "{}", &[])?)
}

/// US-10: agent-initiated remote operations. Every call names a host from the App's list and
/// goes through the same permission engine (and approval card) as hook requests.
fn remote_op(
    sh: &Shared,
    path: &str,
    body: &str,
    host_hdr: &str,
    peer: Option<&TcpStream>,
) -> (&'static str, serde_json::Value) {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Op {
        principal: String,
        session: String,
        host: String,
        command: String,
        local: String,
        remote: String,
        timeout_secs: u64,
    }
    let Some(ops) = sh.ops.clone() else {
        return (
            "501 Not Implemented",
            json!({"error": "remote operations need the shellax app"}),
        );
    };
    let o: Op = match serde_json::from_str(body) {
        Ok(o) => o,
        Err(e) => return ("400 Bad Request", json!({"error": e.to_string()})),
    };
    let principal = if o.principal.is_empty() {
        "cli".to_string()
    } else {
        o.principal.clone()
    };
    if path == "/v1/hosts" {
        let hs = ops.hosts();
        let _ = sh
            .audit
            .append("shellax.hosts", &principal, &format!("n={}", hs.len()));
        return ("200 OK", json!({"hosts": hs}));
    }
    // a caller on a remote host (reverse forward) must not move files on the App's machine
    if !host_hdr.is_empty() && path != "/v1/exec" {
        return (
            "403 Forbidden",
            json!({"error": "upload/download are only available to local callers"}),
        );
    }
    if o.host.is_empty() {
        return ("400 Bad Request", json!({"error": "host is required"}));
    }
    let (action, command, kind) = match path {
        "/v1/exec" => ("exec", o.command.clone(), "shellax.exec"),
        "/v1/upload" => (
            "write",
            format!(
                "cp {} {}",
                crate::ssh::sh_quote(&o.local),
                crate::ssh::sh_quote(&o.remote)
            ),
            "shellax.upload",
        ),
        _ => (
            "exec",
            format!("cat {}", crate::ssh::sh_quote(&o.remote)),
            "shellax.download",
        ),
    };
    if command.trim().is_empty()
        || (path != "/v1/exec" && (o.local.is_empty() || o.remote.is_empty()))
    {
        return (
            "400 Bad Request",
            json!({"error": "missing command / local / remote"}),
        );
    }
    let req = HookReq {
        event: "shellax".into(),
        tool: kind.trim_start_matches("shellax.").into(),
        command: command.clone(),
        action: action.into(),
        cwd: String::new(),
        session: o.session.clone(),
        ..Default::default()
    };
    let (eff, risk, reason) = sh.decide_peer(&principal, &req, &o.host, true, peer);
    // the caller may have gone away while the card was up: never run on its behalf
    if eff == Effect::Allow && peer.is_some_and(peer_closed) {
        let _ = sh.audit.append(kind, "policy", &format!("host={} session={} effect=Cancelled reason=\"caller disconnected\" command={command:?}", o.host, if o.session.is_empty() { "-" } else { &o.session }));
        return (
            "499 Client Closed Request",
            json!({"decision": "deny", "reason": "caller disconnected"}),
        );
    }
    let d = match eff {
        Effect::Allow => "allow",
        Effect::Ask => "ask",
        Effect::Deny => "deny",
    };
    if eff != Effect::Allow {
        return (
            "200 OK",
            json!({"decision": d, "risk": risk.label(), "reason": reason}),
        );
    }
    let r = match path {
        "/v1/exec" => ops
            .exec(
                &o.host,
                &o.command,
                Duration::from_secs(if o.timeout_secs == 0 {
                    600
                } else {
                    o.timeout_secs
                }),
            )
            .map(|(code, out)| json!({"exit": code, "output": String::from_utf8_lossy(&out)})),
        "/v1/upload" => ops
            .upload(&o.host, Path::new(&o.local), &o.remote)
            .map(|n| json!({"bytes": n})),
        _ => ops
            .download(&o.host, &o.remote, Path::new(&o.local))
            .map(|n| json!({"bytes": n})),
    };
    match r {
        Ok(mut v) => {
            let _ = sh.audit.append(
                kind,
                &principal,
                &format!(
                    "host={} session={} effect=Allow command={command:?} result={}",
                    o.host,
                    if o.session.is_empty() {
                        "-"
                    } else {
                        &o.session
                    },
                    v.get("exit")
                        .map(|e| format!("exit {e}"))
                        .or_else(|| v.get("bytes").map(|b| format!("{b} bytes")))
                        .unwrap_or_default()
                ),
            );
            v["decision"] = json!("allow");
            v["risk"] = json!(risk.label());
            v["reason"] = json!(reason);
            ("200 OK", v)
        }
        Err(e) => {
            let _ = sh.audit.append(
                kind,
                &principal,
                &format!("host={} error={:?}", o.host, format!("{e:#}")),
            );
            (
                "502 Bad Gateway",
                json!({"decision": "allow", "error": format!("{e:#}")}),
            )
        }
    }
}

/// True when the HTTP caller has closed its end of the connection (EOF on a non-blocking peek).
pub fn peer_closed(s: &TcpStream) -> bool {
    if s.set_nonblocking(true).is_err() {
        return false;
    }
    let mut b = [0u8; 1];
    let r = s.peek(&mut b);
    let _ = s.set_nonblocking(false);
    match r {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ),
    }
}

impl Shared {
    /// Decide; while an approval card is pending, watch `peer` — if the caller disconnects the
    /// card is withdrawn (its reply channel is dropped, the UI removes it) and the request denied.
    fn decide_peer(
        &self,
        principal: &str,
        req: &HookReq,
        host: &str,
        interactive: bool,
        peer: Option<&TcpStream>,
    ) -> (Effect, Risk, String) {
        let ctx = (self.ctx)();
        let prod = ctx.prod_confirm
            && !host.is_empty()
            && ctx
                .prod_hosts
                .iter()
                .any(|h| h == host || policy::glob(h, host));
        let risk = if req.action == "exec" {
            policy::classify(&req.command)
        } else {
            Risk::R1
        };
        let where_ = if host.is_empty() {
            "local".to_string()
        } else {
            host.to_string()
        };
        let _ = self.audit.append(
            "hook.request",
            principal,
            &format!(
                "event={} tool={} action={} host={where_} session={} cwd={:?} command={:?}",
                req.event,
                req.tool,
                req.action,
                if req.session.is_empty() {
                    "-"
                } else {
                    req.session.as_str()
                },
                req.cwd,
                req.command
            ),
        );
        let (mut eff, mut reason) = if req.action == "tool" {
            // non-shell, non-write tools: only explicit rules decide; otherwise no opinion (allow)
            match policy::evaluate(&ctx.rules, principal, "tool", &req.command) {
                Some((_, r)) => (r.effect, format!("rule {}", r.id)),
                None => (Effect::Allow, "not a shell/write tool".into()),
            }
        } else {
            policy::decide_host(
                ctx.mode,
                ctx.max_auto,
                &ctx.rules,
                principal,
                &req.action,
                &req.command,
                ctx.dangerous_confirm,
                prod,
                host,
            )
        };
        let skey = (
            principal.to_string(),
            if req.session.is_empty() {
                "-".to_string()
            } else {
                req.session.clone()
            },
            command_prefix(&req.command),
        );
        if eff == Effect::Ask {
            // "always allow this prefix for this session" — never across a higher risk level
            if let Some(max) = self.session_allow.lock().unwrap().get(&skey) {
                if risk <= *max {
                    eff = Effect::Allow;
                    reason = format!("{reason} → allowed for this session ({:?})", skey.2);
                    let _ = self.audit.append(
                        "hook.approval",
                        "user",
                        &format!(
                            "session-rule host={where_} session={} prefix={:?} command={:?}",
                            skey.1, skey.2, req.command
                        ),
                    );
                }
            }
        }
        if eff == Effect::Ask && interactive {
            if let Some(tx) = &self.approvals {
                let (rtx, rrx) = flume::bounded(1);
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                let deadline = Instant::now() + ctx.approval_timeout;
                let a = Approval {
                    id,
                    principal: principal.into(),
                    req: req.clone(),
                    host: where_.clone(),
                    risk,
                    reason: reason.clone(),
                    deadline,
                    reply: rtx,
                };
                if tx.send(a).is_ok() {
                    let mut gone = false;
                    let got = loop {
                        let slice = Duration::from_millis(200);
                        match rrx.recv_timeout(slice) {
                            Ok(a) => break Some(a),
                            Err(flume::RecvTimeoutError::Disconnected) => break None,
                            Err(flume::RecvTimeoutError::Timeout) => {}
                        }
                        if peer.is_some_and(peer_closed) {
                            gone = true;
                            break None;
                        }
                        if ctx.timeout_deny && Instant::now() >= deadline {
                            break None;
                        }
                    };
                    drop(rrx); // withdraws the card: the UI drops approvals whose receiver is gone
                    let got = if gone {
                        Some(Answer::Deny("\u{0}gone".into()))
                    } else {
                        got
                    };
                    match got {
                        Some(Answer::Deny(w)) if w == "\u{0}gone" => {
                            eff = Effect::Deny;
                            reason = format!("{reason} → caller disconnected, approval withdrawn");
                            let _ = self.audit.append("hook.approval", "policy", &format!("id={id} withdrawn caller-disconnected host={where_} session={} command={:?}", skey.1, req.command));
                        }
                        Some(Answer::Once) => {
                            eff = Effect::Allow;
                            reason = format!("{reason} → approved by user");
                            let _ = self.audit.append(
                                "hook.approval",
                                "user",
                                &format!(
                                    "id={id} allow host={where_} session={} command={:?}",
                                    skey.1, req.command
                                ),
                            );
                        }
                        Some(Answer::Session) => {
                            eff = Effect::Allow;
                            reason = format!(
                                "{reason} → approved by user for this session ({:?})",
                                skey.2
                            );
                            let _ = self.audit.append("hook.approval", "user", &format!("id={id} allow-session host={where_} session={} prefix={:?} command={:?}", skey.1, skey.2, req.command));
                            self.session_allow
                                .lock()
                                .unwrap()
                                .insert(skey.clone(), risk);
                        }
                        Some(Answer::Deny(why)) => {
                            eff = Effect::Deny;
                            reason = if why.trim().is_empty() {
                                format!("{reason} → denied by user")
                            } else {
                                format!("denied by user: {}", why.trim())
                            };
                            let _ = self.audit.append("hook.approval", "user", &format!("id={id} deny host={where_} session={} feedback={:?} command={:?}", skey.1, why.trim(), req.command));
                        }
                        None => {
                            eff = Effect::Deny;
                            reason = format!(
                                "{reason} → no answer within {}s, denied",
                                ctx.approval_timeout.as_secs()
                            );
                            let _ = self.audit.append(
                                "hook.approval",
                                "policy",
                                &format!("id={id} timeout deny session={} host={where_}", skey.1),
                            );
                        }
                    }
                }
            }
        }
        let _ = self.audit.append("hook.decision", "policy", &format!("principal={principal} host={where_} session={} risk={} effect={eff:?} reason={reason:?} command={:?}", skey.1, risk.label(), req.command));
        (eff, risk, reason)
    }
}

fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == name {
            return Some(
                percent_encoding::percent_decode_str(v)
                    .decode_utf8_lossy()
                    .into_owned(),
            );
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn check_pane_mutation_auth(
    sh: &Shared,
    caller_pane: &str,
    target_pane: Option<&str>,
    action_kind: &str,
    preview: &str,
    is_spawn: bool,
    tx: &flume::Sender<PaneCmd>,
    peer: &TcpStream,
) -> Result<(), (&'static str, serde_json::Value)> {
    let ctx = (sh.ctx)();
    if is_spawn {
        match ctx.api_spawn.as_str() {
            "deny" => {
                let _ = sh.audit.append(
                    action_kind,
                    "policy",
                    &format!("caller={caller_pane:?} effect=Deny reason=api_spawn=deny preview={preview:?}"),
                );
                return Err(("403 Forbidden", json!({"error": "api_spawn disabled by policy"})));
            }
            "ask" => {
                let req = HookReq {
                    event: "AgentApi".into(),
                    tool: "ShellAX".into(),
                    command: preview.to_string(),
                    action: action_kind.into(),
                    ..Default::default()
                };
                let (eff, _, reason) = sh.decide_peer("agent-api", &req, "", true, Some(peer));
                if eff != Effect::Allow {
                    return Err(("403 Forbidden", json!({"error": reason})));
                }
            }
            _ => {}
        }
        return Ok(());
    }

    // Mutating an existing pane (`send_text`, `send_keys`, `close`, `agent/prompt`):
    // caller may freely control child panes it spawned itself; otherwise enforce `write_to_pty`.
    let target = target_pane.unwrap_or("");
    let mut is_child = false;
    if !caller_pane.is_empty() && !target.is_empty() {
        let (rtx, rrx) = flume::bounded(1);
        if tx
            .send(PaneCmd::IsChild {
                caller_pane: caller_pane.to_string(),
                target_pane: target.to_string(),
                reply: rtx,
            })
            .is_ok()
        {
            is_child = rrx.recv_timeout(Duration::from_secs(3)).unwrap_or(false);
        }
    }
    if is_child {
        return Ok(());
    }
    match ctx.write_to_pty.as_str() {
        "allow" => Ok(()),
        "deny" => {
            let _ = sh.audit.append(
                action_kind,
                "policy",
                &format!("caller={caller_pane:?} target={target:?} effect=Deny reason=write_to_pty=deny"),
            );
            Err(("403 Forbidden", json!({"error": "write_to_pty denied by policy"})))
        }
        _ => {
            let req = HookReq {
                event: "PaneWrite".into(),
                tool: "ShellAX".into(),
                command: format!("pane={target} {preview}"),
                action: "write_to_pty".into(),
                ..Default::default()
            };
            let (eff, _, reason) = sh.decide_peer("agent-api", &req, "", true, Some(peer));
            if eff == Effect::Allow {
                Ok(())
            } else {
                Err(("403 Forbidden", json!({"error": reason})))
            }
        }
    }
}

fn audit_pane_op(sh: &Shared, kind: &str, caller: &str, target: &str, text: &str) {
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(text.as_bytes()));
    let (redacted, _) = crate::ai::redact(text);
    let snippet: String = redacted.chars().take(80).collect();
    let _ = sh.audit.append(
        kind,
        "agent-api",
        &format!(
            "caller={caller:?} target={target:?} sha256={} preview={snippet:?}",
            &hash[..16]
        ),
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_pane_api(
    method: &str,
    route: &str,
    query: &str,
    body: &str,
    pane_hdr: &str,
    host_hdr: &str,
    sh: &Shared,
    peer: &TcpStream,
) -> (&'static str, serde_json::Value) {
    let mutating = !matches!(route, "/v1/pane/list" | "/v1/pane/read" | "/v1/agent/wait");
    if mutating && !host_hdr.is_empty() {
        return (
            "403 Forbidden",
            json!({"error": "local callers only"}),
        );
    }
    let Some(tx) = PANE_TX.get() else {
        return ("501 Not Implemented", json!({"error": "no UI"}));
    };

    match (method, route) {
        ("GET" | "POST", "/v1/pane/list") => {
            let (rtx, rrx) = flume::bounded(1);
            if tx.send(PaneCmd::List { reply: rtx }).is_err() {
                return ("503 Service Unavailable", json!({"error": "UI unavailable"}));
            }
            match rrx.recv_timeout(Duration::from_secs(5)) {
                Ok(panes) => ("200 OK", json!({"panes": panes})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("GET" | "POST", "/v1/pane/read") => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let pane_id = query_param(query, "pane_id")
                .or_else(|| v.get("pane_id").and_then(|x| x.as_str()).map(str::to_string))
                .unwrap_or_default();
            let lines = query_param(query, "lines")
                .and_then(|s| s.parse::<usize>().ok())
                .or_else(|| v.get("lines").and_then(|x| x.as_u64()).map(|n| n as usize))
                .unwrap_or(200)
                .clamp(1, 5000);
            if pane_id.is_empty() {
                return ("400 Bad Request", json!({"error": "missing pane_id"}));
            }
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::Read {
                pane_id: pane_id.clone(),
                lines,
                reply: rtx,
            });
            match rrx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(raw)) => {
                    let (text, redacted_count) = crate::ai::redact(&raw);
                    (
                        "200 OK",
                        json!({"pane_id": pane_id, "text": text, "redacted": redacted_count}),
                    )
                }
                Ok(Err(e)) => ("404 Not Found", json!({"error": e})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("POST", "/v1/pane/split") => {
            #[derive(Deserialize, Default)]
            #[serde(default)]
            struct SplitReq {
                from: Option<String>,
                dir: String,
                cmd: Option<String>,
                cwd: Option<String>,
                focus: bool,
            }
            let r: SplitReq = match serde_json::from_str(body) {
                Ok(x) => x,
                Err(e) => return ("400 Bad Request", json!({"error": e.to_string()})),
            };
            let preview = r.cmd.as_deref().unwrap_or("shell");
            if let Err(resp) = check_pane_mutation_auth(
                sh,
                pane_hdr,
                None,
                "pane.split",
                preview,
                true,
                tx,
                peer,
            ) {
                return resp;
            }
            let dir = match r.dir.to_ascii_lowercase().as_str() {
                "down" | "bottom" | "h" | "horizontal" => crate::layout::Dir::Horizontal,
                _ => crate::layout::Dir::Vertical,
            };
            let caller = (!pane_hdr.is_empty()).then(|| pane_hdr.to_string());
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::Split {
                caller_pane: caller,
                from: r.from,
                dir,
                cmd: r.cmd.clone(),
                cwd: r.cwd,
                focus: r.focus,
                reply: rtx,
            });
            match rrx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(new_id)) => {
                    audit_pane_op(sh, "pane.split", pane_hdr, &new_id, preview);
                    ("200 OK", json!({"pane_id": new_id}))
                }
                Ok(Err(e)) => ("400 Bad Request", json!({"error": e})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("POST", "/v1/pane/send_text") => {
            #[derive(Deserialize, Default)]
            #[serde(default)]
            struct SendReq {
                pane_id: String,
                text: String,
                enter: bool,
            }
            let r: SendReq = match serde_json::from_str(body) {
                Ok(x) => x,
                Err(e) => return ("400 Bad Request", json!({"error": e.to_string()})),
            };
            if r.pane_id.is_empty() {
                return ("400 Bad Request", json!({"error": "missing pane_id"}));
            }
            if let Err(resp) = check_pane_mutation_auth(
                sh,
                pane_hdr,
                Some(&r.pane_id),
                "pane.send_text",
                &r.text,
                false,
                tx,
                peer,
            ) {
                return resp;
            }
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::SendText {
                pane_id: r.pane_id.clone(),
                text: r.text.clone(),
                enter: r.enter,
                reply: rtx,
            });
            match rrx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => {
                    audit_pane_op(sh, "pane.send_text", pane_hdr, &r.pane_id, &r.text);
                    ("200 OK", json!({"ok": true}))
                }
                Ok(Err(e)) => ("404 Not Found", json!({"error": e})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("POST", "/v1/pane/send_keys") => {
            #[derive(Deserialize, Default)]
            #[serde(default)]
            struct KeysReq {
                pane_id: String,
                keys: Vec<String>,
            }
            let r: KeysReq = match serde_json::from_str(body) {
                Ok(x) => x,
                Err(e) => return ("400 Bad Request", json!({"error": e.to_string()})),
            };
            if r.pane_id.is_empty() {
                return ("400 Bad Request", json!({"error": "missing pane_id"}));
            }
            let joined = r.keys.join(" ");
            if let Err(resp) = check_pane_mutation_auth(
                sh,
                pane_hdr,
                Some(&r.pane_id),
                "pane.send_keys",
                &joined,
                false,
                tx,
                peer,
            ) {
                return resp;
            }
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::SendKeys {
                pane_id: r.pane_id.clone(),
                keys: r.keys,
                reply: rtx,
            });
            match rrx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => {
                    audit_pane_op(sh, "pane.send_keys", pane_hdr, &r.pane_id, &joined);
                    ("200 OK", json!({"ok": true}))
                }
                Ok(Err(e)) => ("404 Not Found", json!({"error": e})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("POST", "/v1/pane/close") => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let pane_id = v
                .get("pane_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if pane_id.is_empty() {
                return ("400 Bad Request", json!({"error": "missing pane_id"}));
            }
            if let Err(resp) = check_pane_mutation_auth(
                sh,
                pane_hdr,
                Some(&pane_id),
                "pane.close",
                "close",
                false,
                tx,
                peer,
            ) {
                return resp;
            }
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::Close {
                pane_id: pane_id.clone(),
                reply: rtx,
            });
            match rrx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(())) => {
                    audit_pane_op(sh, "pane.close", pane_hdr, &pane_id, "close");
                    ("200 OK", json!({"ok": true}))
                }
                Ok(Err(e)) => ("404 Not Found", json!({"error": e})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("POST", "/v1/agent/start") => {
            #[derive(Deserialize, Default)]
            #[serde(default)]
            struct StartReq {
                agent: String,
                cwd: Option<String>,
                prompt: Option<String>,
                task: Option<String>,
            }
            let r: StartReq = match serde_json::from_str(body) {
                Ok(x) => x,
                Err(e) => return ("400 Bad Request", json!({"error": e.to_string()})),
            };
            if r.agent.trim().is_empty() {
                return ("400 Bad Request", json!({"error": "missing agent"}));
            }
            let preview = format!("{} {}", r.agent, r.prompt.as_deref().unwrap_or(""));
            if let Err(resp) = check_pane_mutation_auth(
                sh,
                pane_hdr,
                None,
                "agent.start",
                &preview,
                true,
                tx,
                peer,
            ) {
                return resp;
            }
            let caller = (!pane_hdr.is_empty()).then(|| pane_hdr.to_string());
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::AgentStart {
                caller_pane: caller,
                agent: r.agent,
                cwd: r.cwd,
                prompt: r.prompt,
                task: r.task,
                reply: rtx,
            });
            match rrx.recv_timeout(Duration::from_secs(40)) {
                Ok(Ok(val)) => {
                    let target = val.get("pane_id").and_then(|x| x.as_str()).unwrap_or("");
                    audit_pane_op(sh, "agent.start", pane_hdr, target, &preview);
                    ("200 OK", val)
                }
                Ok(Err(e)) => ("400 Bad Request", json!({"error": e})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("POST", "/v1/agent/prompt") => {
            #[derive(Deserialize, Default)]
            #[serde(default)]
            struct PromptReq {
                pane_id: String,
                prompt: String,
            }
            let r: PromptReq = match serde_json::from_str(body) {
                Ok(x) => x,
                Err(e) => return ("400 Bad Request", json!({"error": e.to_string()})),
            };
            if r.pane_id.is_empty() {
                return ("400 Bad Request", json!({"error": "missing pane_id"}));
            }
            if let Err(resp) = check_pane_mutation_auth(
                sh,
                pane_hdr,
                Some(&r.pane_id),
                "agent.prompt",
                &r.prompt,
                false,
                tx,
                peer,
            ) {
                return resp;
            }
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::AgentPrompt {
                pane_id: r.pane_id.clone(),
                prompt: r.prompt.clone(),
                reply: rtx,
            });
            match rrx.recv_timeout(Duration::from_secs(35)) {
                Ok(Ok(val)) => {
                    audit_pane_op(sh, "agent.prompt", pane_hdr, &r.pane_id, &r.prompt);
                    ("200 OK", val)
                }
                Ok(Err(e)) => ("404 Not Found", json!({"error": e})),
                Err(_) => ("504 Gateway Timeout", json!({"error": "timeout"})),
            }
        }
        ("GET" | "POST", "/v1/agent/wait") => {
            let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let pane_id = query_param(query, "pane_id")
                .or_else(|| v.get("pane_id").and_then(|x| x.as_str()).map(str::to_string))
                .unwrap_or_default();
            if pane_id.is_empty() {
                return ("400 Bad Request", json!({"error": "missing pane_id"}));
            }
            let until_raw = query_param(query, "until")
                .or_else(|| v.get("until").and_then(|x| x.as_str()).map(str::to_string))
                .unwrap_or_else(|| "done,blocked".into());
            let mut until: Vec<crate::agent::AgentState> = until_raw
                .split(',')
                .filter_map(crate::agent::AgentState::parse_word)
                .collect();
            if until.is_empty() {
                until = vec![
                    crate::agent::AgentState::Done,
                    crate::agent::AgentState::Blocked,
                    crate::agent::AgentState::Idle,
                ];
            }
            let timeout_secs = query_param(query, "timeout")
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.get("timeout").and_then(|x| x.as_u64()))
                .unwrap_or(600)
                .clamp(1, 3600);
            let timeout = Duration::from_secs(timeout_secs);
            let deadline = Instant::now() + timeout;
            let (rtx, rrx) = flume::bounded(1);
            let _ = tx.send(PaneCmd::Wait {
                pane_id,
                until,
                timeout,
                reply: rtx,
            });
            loop {
                match rrx.recv_timeout(Duration::from_millis(250)) {
                    Ok(Ok(val)) => return ("200 OK", val),
                    Ok(Err(e)) => {
                        return if e == "timeout" {
                            ("408 Request Timeout", json!({"error": "timeout"}))
                        } else {
                            ("404 Not Found", json!({"error": e}))
                        };
                    }
                    Err(flume::RecvTimeoutError::Disconnected) => {
                        return ("503 Service Unavailable", json!({"error": "UI closed"}));
                    }
                    Err(flume::RecvTimeoutError::Timeout) => {
                        if peer_closed(peer) {
                            return ("499 Client Closed Request", json!({"error": "cancelled"}));
                        }
                        if Instant::now() >= deadline + Duration::from_secs(2) {
                            return ("408 Request Timeout", json!({"error": "timeout"}));
                        }
                    }
                }
            }
        }
        _ => ("404 Not Found", json!({"error": "unknown endpoint"})),
    }
}

/// Minimal blocking HTTP client used by the CLI (no TLS; localhost or a forwarded port).
pub fn post(
    url: &str,
    path: &str,
    token: &str,
    host_hdr: Option<&str>,
    body: &str,
    timeout: Duration,
) -> Result<(u16, String, Option<i32>)> {
    let pane = std::env::var("SHELLAX_PANE").ok().filter(|s| !s.is_empty());
    request(
        "POST",
        url,
        path,
        token,
        host_hdr,
        pane.as_deref(),
        body,
        timeout,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn request(
    method: &str,
    url: &str,
    path: &str,
    token: &str,
    host_hdr: Option<&str>,
    pane_hdr: Option<&str>,
    body: &str,
    timeout: Duration,
) -> Result<(u16, String, Option<i32>)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| anyhow!("only http:// endpoints are supported"))?;
    let addr = rest.trim_end_matches('/');
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(timeout))?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if let Some(h) = host_hdr {
        req.push_str(&format!("X-shellax-Host: {h}\r\n"));
    }
    if let Some(p) = pane_hdr.filter(|s| !s.is_empty()) {
        req.push_str(&format!("X-shellax-Pane: {p}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())?;
    if !body.is_empty() {
        s.write_all(body.as_bytes())?;
    }
    let mut resp = String::new();
    s.read_to_string(&mut resp)?;
    let (head, body) = resp
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("bad HTTP response"))?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    let exit = head
        .lines()
        .find_map(|l| l.strip_prefix("X-shellax-Exit: "))
        .and_then(|x| x.trim().parse().ok());
    Ok((status, body.to_string(), exit))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn srv(ctx: PolicyCtx, approvals: Option<flume::Sender<Approval>>) -> (Server, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("as-ps-{}-{}", std::process::id(), new_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLog::new(dir.join("audit.jsonl")));
        let s = start(
            "127.0.0.1:0",
            None,
            Arc::new(move || ctx.clone()),
            approvals,
            audit,
            Some(endpoint_path(&dir)),
        )
        .unwrap();
        (s, dir)
    }
    #[test]
    fn check_auth_hooks_and_audit() {
        let (s, dir) = srv(
            PolicyCtx {
                prod_hosts: vec!["prod-*".into()],
                ..Default::default()
            },
            None,
        );
        let ep = read_endpoint(&dir).unwrap();
        assert_eq!(ep.token, s.token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(endpoint_path(&dir))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let t = Duration::from_secs(5);
        assert_eq!(
            post(&ep.url, "/v1/check", "wrong", None, "{}", t)
                .unwrap()
                .0,
            401
        );
        let (_, b, _) = post(
            &ep.url,
            "/v1/check",
            &ep.token,
            None,
            r#"{"command":"ls -la"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"allow\""), "{b}");
        let (_, b, _) = post(
            &ep.url,
            "/v1/check",
            &ep.token,
            None,
            r#"{"command":"rm -rf /"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"deny\""));
        let (_, b, _) = post(
            &ep.url,
            "/v1/hook/claude/PreToolUse",
            &ep.token,
            None,
            r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"ask\""), "{b}");
        let (_, b, code) = post(
            &ep.url,
            "/v1/hook/grok",
            &ep.token,
            None,
            r#"{"toolName":"bash","toolInput":{"command":"mkfs.ext4 /dev/sda1"}}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("deny") && code == Some(2));
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(
            log.contains("hook.request")
                && log.contains("hook.decision")
                && log.contains("policy.auth_fail")
        );
        assert!(crate::audit::verify(&dir.join("audit.jsonl")).is_ok());
    }
    #[test]
    fn approvals_and_timeout() {
        let (tx, rx) = flume::unbounded::<Approval>();
        let (s, dir) = srv(
            PolicyCtx {
                approval_timeout: Duration::from_millis(600),
                ..Default::default()
            },
            Some(tx),
        );
        let url = format!("http://127.0.0.1:{}", s.port);
        let tok = s.token.clone();
        std::thread::spawn(move || {
            let a = rx.recv().unwrap();
            assert_eq!(a.req.command, "rm a");
            a.reply.send(Answer::Once).unwrap();
            let _b = rx.recv().unwrap(); /* never answer */
            std::thread::sleep(Duration::from_secs(2));
        });
        let (_, b, _) = post(
            &url,
            "/v1/hook/claude",
            &tok,
            None,
            r#"{"tool_name":"Bash","tool_input":{"command":"rm a"}}"#,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(b.contains("\"allow\"") && b.contains("approved"), "{b}");
        let (_, b, _) = post(
            &url,
            "/v1/hook/claude",
            &tok,
            None,
            r#"{"tool_name":"Bash","tool_input":{"command":"rm b"}}"#,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(b.contains("\"deny\"") && b.contains("no answer"), "{b}");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn session_allow_and_deny_with_reason() {
        let (tx, rx) = flume::unbounded::<Approval>();
        let (s, dir) = srv(
            PolicyCtx {
                approval_timeout: Duration::from_secs(3),
                ..Default::default()
            },
            Some(tx),
        );
        let url = format!("http://127.0.0.1:{}", s.port);
        let tok = s.token.clone();
        let asked = Arc::new(AtomicU64::new(0));
        let a2 = asked.clone();
        std::thread::spawn(move || {
            while let Ok(a) = rx.recv() {
                let n = a2.fetch_add(1, Ordering::SeqCst);
                let ans = if n == 0 {
                    Answer::Session
                } else {
                    Answer::Deny("use trash-cli instead".into())
                };
                a.reply.send(ans).unwrap();
            }
        });
        let t = Duration::from_secs(5);
        let body = |sess: &str, cmd: &str| {
            format!(
                r#"{{"session_id":"{sess}","tool_name":"Bash","tool_input":{{"command":"{cmd}"}}}}"#
            )
        };
        let (_, b, _) = post(
            &url,
            "/v1/hook/claude",
            &tok,
            None,
            &body("s1", "git push origin a"),
            t,
        )
        .unwrap();
        assert!(b.contains("\"allow\"") && b.contains("this session"), "{b}");
        let (_, b, _) = post(
            &url,
            "/v1/hook/claude",
            &tok,
            None,
            &body("s1", "git push origin b"),
            t,
        )
        .unwrap();
        assert!(b.contains("\"allow\""), "{b}");
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        let (_, b, _) = post(
            &url,
            "/v1/hook/claude",
            &tok,
            None,
            &body("s2", "git push origin c"),
            t,
        )
        .unwrap();
        assert!(
            b.contains("\"deny\"") && b.contains("use trash-cli instead"),
            "{b}"
        );
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(command_prefix("git push origin a"), "git push");
        assert_eq!(command_prefix("FOO=1 rm -rf x"), "rm");
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(
            log.contains("allow-session")
                && log.contains("session-rule")
                && log.contains("feedback")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn production_host_header() {
        let (s, _dir) = srv(
            PolicyCtx {
                mode: Mode::Auto,
                max_auto: Risk::R2,
                prod_hosts: vec!["prod-*".into()],
                ..Default::default()
            },
            None,
        );
        let url = format!("http://127.0.0.1:{}", s.port);
        let t = Duration::from_secs(5);
        let (_, b, _) = post(
            &url,
            "/v1/check",
            &s.token,
            None,
            r#"{"command":"touch x"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"allow\""));
        let (_, b, _) = post(
            &url,
            "/v1/check",
            &s.token,
            Some("prod-web"),
            r#"{"command":"touch x"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"ask\""), "{b}");
    }

    struct FakeOps(std::sync::Mutex<Vec<String>>);
    impl crate::broker::Ops for FakeOps {
        fn hosts(&self) -> Vec<crate::broker::HostInfo> {
            vec![crate::broker::HostInfo {
                name: "web1".into(),
                user: "u".into(),
                host: "10.0.0.1".into(),
                port: 22,
                production: false,
            }]
        }
        fn exec(&self, host: &str, c: &str, _t: Duration) -> Result<(Option<u32>, Vec<u8>)> {
            self.0.lock().unwrap().push(format!("exec {host} {c}"));
            Ok((Some(0), b"hi\n".to_vec()))
        }
        fn upload(&self, host: &str, l: &Path, r: &str) -> Result<u64> {
            self.0
                .lock()
                .unwrap()
                .push(format!("up {host} {} {r}", l.display()));
            Ok(3)
        }
        fn download(&self, host: &str, r: &str, l: &Path) -> Result<u64> {
            self.0
                .lock()
                .unwrap()
                .push(format!("down {host} {r} {}", l.display()));
            Ok(4)
        }
    }
    #[test]
    fn remote_ops_go_through_policy() {
        let dir =
            std::env::temp_dir().join(format!("as-ops-{}-{}", std::process::id(), new_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLog::new(dir.join("audit.jsonl")));
        let ops = Arc::new(FakeOps(Default::default()));
        let (ap_tx, ap_rx) = flume::unbounded::<Approval>();
        let s = start_with_ops(
            "127.0.0.1:0",
            None,
            Arc::new(PolicyCtx::default),
            Some(ap_tx),
            audit,
            None,
            Some(ops.clone()),
        )
        .unwrap();
        let url = format!("http://127.0.0.1:{}", s.port);
        let t = Duration::from_secs(10);
        let (c, b, _) = post(&url, "/v1/hosts", &s.token, None, "{}", t).unwrap();
        assert_eq!(c, 200);
        assert!(b.contains("web1") && !b.contains("password"), "{b}");
        let (_, b, _) = post(
            &url,
            "/v1/exec",
            &s.token,
            None,
            r#"{"host":"web1","command":"uptime"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"allow\"") && b.contains("hi"), "{b}");
        let (_, b, _) = post(
            &url,
            "/v1/exec",
            &s.token,
            None,
            r#"{"host":"web1","command":"rm -rf /"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"deny\""), "{b}");
        // R2+ asks: user denies on the card, nothing runs
        std::thread::spawn(move || {
            let a = ap_rx.recv().unwrap();
            assert_eq!(a.host, "web1");
            a.reply.send(Answer::Deny("no".into())).unwrap();
            let a = ap_rx.recv().unwrap();
            a.reply.send(Answer::Once).unwrap();
        });
        let (_, b, _) = post(
            &url,
            "/v1/exec",
            &s.token,
            None,
            r#"{"host":"web1","command":"systemctl restart nginx","session":"s9"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"deny\""), "{b}");
        let (_, b, _) = post(
            &url,
            "/v1/upload",
            &s.token,
            None,
            r#"{"host":"web1","local":"/tmp/a","remote":"/tmp/b"}"#,
            t,
        )
        .unwrap();
        assert!(b.contains("\"bytes\":3"), "{b}");
        let (c, _, _) = post(
            &url,
            "/v1/download",
            &s.token,
            Some("web1"),
            r#"{"host":"web1","local":"/tmp/a","remote":"/etc/passwd"}"#,
            t,
        )
        .unwrap();
        assert_eq!(c, 403);
        let calls = ops.0.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "exec web1 uptime".to_string(),
                "up web1 /tmp/a /tmp/b".into()
            ]
        );
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(
            log.contains("shellax.exec")
                && log.contains("shellax.upload")
                && log.contains("shellax.hosts")
        );
    }

    #[test]
    fn approval_withdrawn_when_caller_disconnects() {
        let dir =
            std::env::temp_dir().join(format!("as-wd-{}-{}", std::process::id(), new_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLog::new(dir.join("audit.jsonl")));
        let ops = Arc::new(FakeOps(Default::default()));
        let (ap_tx, ap_rx) = flume::unbounded::<Approval>();
        let s = start_with_ops(
            "127.0.0.1:0",
            None,
            Arc::new(PolicyCtx::default),
            Some(ap_tx),
            audit,
            None,
            Some(ops.clone()),
        )
        .unwrap();
        for (path, body) in [
            (
                "/v1/exec",
                r#"{"host":"web1","command":"systemctl restart nginx"}"#,
            ),
            (
                "/v1/hook/claude/PreToolUse",
                r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#,
            ),
        ] {
            let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}", s.token, body.len()).unwrap();
            let a = ap_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(!a.reply.is_disconnected());
            drop(c); // caller goes away (agent killed / Ctrl-C)
            let t0 = Instant::now();
            while !a.reply.is_disconnected() {
                assert!(t0.elapsed() < Duration::from_secs(3), "card not withdrawn");
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = a.reply.send(Answer::Once); // a late click must not run anything
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(ops.0.lock().unwrap().is_empty());
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert_eq!(
            log.matches("withdrawn caller-disconnected").count(),
            2,
            "{log}"
        );
    }

    #[test]
    fn pane_and_agent_orchestration_api() {
        let (ptx, prx) = flume::unbounded::<PaneCmd>();
        let _ = PANE_TX.set(ptx);
        let reported = Arc::new(std::sync::Mutex::new(Vec::<(String, String, Option<String>)>::new()));
        let rep2 = reported.clone();
        std::thread::spawn(move || {
            while let Ok(cmd) = prx.recv() {
                match cmd {
                    PaneCmd::List { reply } => {
                        let _ = reply.send(vec![PaneInfo {
                            pane_id: "child-1".into(),
                            tab: 0,
                            title: "claude".into(),
                            cwd: "/tmp/proj".into(),
                            agent: Some("claude".into()),
                            state: Some(crate::agent::AgentState::Idle),
                            session_id: Some("sess-abc".into()),
                            parent: Some("lead-0".into()),
                            task: Some("feat-x".into()),
                        }]);
                    }
                    PaneCmd::Read { pane_id, reply, .. } => {
                        if pane_id == "child-1" {
                            let _ = reply.send(Ok(
                                "status: ready\nOPENAI_API_KEY=sk-proj-1234567890abcdef1234567890abcdef\n"
                                    .into(),
                            ));
                        } else {
                            let _ = reply.send(Err("not found".into()));
                        }
                    }
                    PaneCmd::IsChild {
                        caller_pane,
                        target_pane,
                        reply,
                    } => {
                        let _ = reply.send(caller_pane == "lead-0" && target_pane == "child-1");
                    }
                    PaneCmd::SendText { reply, .. } => {
                        let _ = reply.send(Ok(()));
                    }
                    PaneCmd::Split { reply, .. } => {
                        let _ = reply.send(Ok("child-2".into()));
                    }
                    PaneCmd::Wait { reply, .. } => {
                        let _ = reply.send(Ok(json!({"pane_id": "child-1", "state": "done"})));
                    }
                    PaneCmd::ReportAgent {
                        pane_id,
                        event,
                        session_id,
                        ..
                    } => {
                        rep2.lock().unwrap().push((pane_id, event, session_id));
                    }
                    _ => {}
                }
            }
        });

        let (s, dir) = srv(
            PolicyCtx {
                write_to_pty: "deny".into(),
                api_spawn: "allow".into(),
                ..Default::default()
            },
            None,
        );
        let url = format!("http://127.0.0.1:{}", s.port);
        let t = Duration::from_secs(5);

        // 1. /v1/pane/list returns structured panes
        let (code, body, _) = request("GET", &url, "/v1/pane/list", &s.token, None, None, "", t).unwrap();
        assert_eq!(code, 200);
        assert!(body.contains("child-1") && body.contains("sess-abc"), "{body}");

        // 2. /v1/pane/read redacts secrets automatically
        let (code, body, _) = request(
            "GET",
            &url,
            "/v1/pane/read?pane_id=child-1&lines=50",
            &s.token,
            None,
            None,
            "",
            t,
        )
        .unwrap();
        assert_eq!(code, 200);
        assert!(body.contains("status: ready"), "{body}");
        assert!(!body.contains("sk-proj-1234567890abcdef"), "secret leaked: {body}");

        // 3. Parent pane ("lead-0") can write to its own child ("child-1") even when write_to_pty = deny
        let (code, body, _) = request(
            "POST",
            &url,
            "/v1/pane/send_text",
            &s.token,
            None,
            Some("lead-0"),
            r#"{"pane_id":"child-1","text":"cargo test","enter":true}"#,
            t,
        )
        .unwrap();
        assert_eq!(code, 200, "{body}");

        // 4. Unrelated pane ("stranger-9") is blocked by write_to_pty = deny
        let (code, body, _) = request(
            "POST",
            &url,
            "/v1/pane/send_text",
            &s.token,
            None,
            Some("stranger-9"),
            r#"{"pane_id":"other-2","text":"echo hi","enter":true}"#,
            t,
        )
        .unwrap();
        assert_eq!(code, 403, "{body}");

        // 5. Remote forwarded callers (X-shellax-Host set) cannot mutate local panes
        let (code, _, _) = request(
            "POST",
            &url,
            "/v1/pane/split",
            &s.token,
            Some("remote-box"),
            Some("lead-0"),
            r#"{"dir":"right"}"#,
            t,
        )
        .unwrap();
        assert_eq!(code, 403);

        // 6. State-only lifecycle hooks (Stop / Notification / SessionStart / UserPromptSubmit)
        //    never block (200 OK `{}`) and forward ReportAgent to UI
        let (code, body, _) = request(
            "POST",
            &url,
            "/v1/hook/claude/Stop",
            &s.token,
            None,
            Some("child-1"),
            r#"{"session_id":"sess-xyz","transcript_path":"/tmp/t.jsonl"}"#,
            t,
        )
        .unwrap();
        assert_eq!(code, 200);
        assert!(body.trim().is_empty() || body.trim() == "{}");
        std::thread::sleep(Duration::from_millis(50));
        let reps = reported.lock().unwrap().clone();
        assert!(
            reps.iter()
                .any(|(p, ev, sid)| p == "child-1" && ev == "Stop" && sid.as_deref() == Some("sess-xyz")),
            "missing hook report: {reps:?}"
        );

        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(log.contains("pane.send_text") && !log.contains("sk-proj-1234567890abcdef"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
