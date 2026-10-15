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
}
impl Default for PolicyCtx {
    fn default() -> Self { Self { mode: Mode::Ask, max_auto: Risk::R1, rules: vec![], dangerous_confirm: true, prod_confirm: true, prod_hosts: vec![], approval_timeout: Duration::from_secs(120), timeout_deny: true } }
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
pub enum Answer { Once, Session, Deny(String) }

/// Command prefix used for "always allow this prefix in this session": the program plus its
/// sub-command when there is one (`git push`, `kubectl get`, `rm`).
pub fn command_prefix(cmd: &str) -> String {
    let mut it = cmd.split_whitespace().filter(|w| !w.contains('='));
    let a = it.next().unwrap_or("");
    match it.next() { Some(b) if !b.starts_with('-') && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') => format!("{a} {b}"), _ => a.to_string() }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Endpoint { pub url: String, pub token: String, pub pid: u32, #[serde(default, skip_serializing_if = "String::is_empty")] pub host: String }

pub fn endpoint_path(data_dir: &Path) -> PathBuf { data_dir.join("policy-endpoint.json") }

pub fn read_endpoint(data_dir: &Path) -> Option<Endpoint> {
    if let (Ok(url), Ok(token)) = (std::env::var("SHELLAX_URL"), std::env::var("SHELLAX_TOKEN")) { return Some(Endpoint { url, token, pid: 0, host: std::env::var("SHELLAX_HOST").unwrap_or_default() }); }
    serde_json::from_str(&std::fs::read_to_string(endpoint_path(data_dir)).ok()?).ok()
}

pub type CtxFn = Arc<dyn Fn() -> PolicyCtx + Send + Sync>;

/// `/v1/open` target: the running App receives ssh:// / sftp:// links here (single instance).
pub static OPEN_TX: std::sync::OnceLock<flume::Sender<String>> = std::sync::OnceLock::new();

pub struct Server { pub port: u16, pub token: String }

pub fn new_token() -> String { use rand::RngCore; let mut b = [0u8; 24]; rand::thread_rng().fill_bytes(&mut b); b.iter().map(|x| format!("{x:02x}")).collect() }

/// Start the server on `bind` (e.g. "127.0.0.1:0"). Writes the endpoint file when given.
pub fn start(bind: &str, token: Option<String>, ctx: CtxFn, approvals: Option<flume::Sender<Approval>>, audit: Arc<AuditLog>, endpoint_file: Option<PathBuf>) -> Result<Server> {
    start_with_ops(bind, token, ctx, approvals, audit, endpoint_file, None)
}

/// Like [`start`], plus the agent-facing remote operations (`/v1/hosts`, `/v1/exec`,
/// `/v1/upload`, `/v1/download`) backed by the App's connection pool.
pub fn start_with_ops(bind: &str, token: Option<String>, ctx: CtxFn, approvals: Option<flume::Sender<Approval>>, audit: Arc<AuditLog>, endpoint_file: Option<PathBuf>, ops: Option<Arc<dyn crate::broker::Ops>>) -> Result<Server> {
    let listener = TcpListener::bind(bind)?;
    let port = listener.local_addr()?.port();
    let token = token.unwrap_or_else(new_token);
    if let Some(p) = &endpoint_file {
        if let Some(d) = p.parent() { std::fs::create_dir_all(d)?; }
        let ep = Endpoint { url: format!("http://127.0.0.1:{port}"), token: token.clone(), pid: std::process::id(), host: String::new() };
        write_private(p, &serde_json::to_string_pretty(&ep)?)?;
    }
    let shared = Arc::new(Shared { token: token.clone(), ctx, approvals, audit, next: AtomicU64::new(1), session_allow: Default::default(), ops });
    std::thread::Builder::new().name("policy-server".into()).spawn(move || {
        for s in listener.incoming().flatten() {
            let sh = shared.clone();
            std::thread::spawn(move || { let _ = handle(s, &sh); });
        }
    })?;
    Ok(Server { port, token })
}

fn write_private(p: &Path, text: &str) -> Result<()> {
    let tmp = p.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

struct Shared { token: String, ctx: CtxFn, approvals: Option<flume::Sender<Approval>>, audit: Arc<AuditLog>, next: AtomicU64, ops: Option<Arc<dyn crate::broker::Ops>>,
    /// (principal, agent session, command prefix) → highest risk the user allowed for it
    session_allow: std::sync::Mutex<std::collections::HashMap<(String, String, String), Risk>> }

fn ct_eq(a: &str, b: &str) -> bool { a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0 }

fn respond(mut s: TcpStream, status: &str, body: &str, extra: &[(&str, String)]) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in extra { h.push_str(&format!("{k}: {v}\r\n")); }
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
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let (mut len, mut auth, mut host_hdr) = (0usize, String::new(), String::new());
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" { break; }
        if let Some((k, v)) = h.split_once(':') {
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.parse().unwrap_or(0),
                "authorization" => auth = v.strip_prefix("Bearer ").unwrap_or(&v).to_string(),
                "x-shellax-token" => auth = v,
                "x-shellax-host" => host_hdr = v,
                _ => {}
            }
        }
    }
    if len > 1 << 20 { return Ok(respond(stream, "413 Payload Too Large", "{}", &[])?); }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).to_string();
    if method == "GET" && path == "/v1/health" { return Ok(respond(stream, "200 OK", r#"{"ok":true,"service":"shellax-policy"}"#, &[])?); }
    if !ct_eq(&auth, &sh.token) {
        let _ = sh.audit.append("policy.auth_fail", "policy", &format!("path={path}"));
        return Ok(respond(stream, "401 Unauthorized", r#"{"error":"bad token"}"#, &[])?);
    }
    if method != "POST" { return Ok(respond(stream, "405 Method Not Allowed", "{}", &[])?); }
    if path == "/v1/check" {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Check { principal: String, action: String, command: String, host: String, cwd: String, interactive: Option<bool> }
        let c: Check = match serde_json::from_str(&body) { Ok(c) => c, Err(e) => return Ok(respond(stream, "400 Bad Request", &json!({"error": e.to_string()}).to_string(), &[])?) };
        let principal = if c.principal.is_empty() { "cli".to_string() } else { c.principal };
        let req = HookReq { event: "Check".into(), tool: "Bash".into(), command: c.command, action: if c.action.is_empty() { "exec".into() } else { c.action }, cwd: c.cwd, session: String::new() };
        let host = if c.host.is_empty() { host_hdr } else { c.host };
        let (eff, risk, reason) = sh.decide_peer(&principal, &req, &host, c.interactive.unwrap_or(true), Some(&stream));
        let d = match eff { Effect::Allow => "allow", Effect::Ask => "ask", Effect::Deny => "deny" };
        return Ok(respond(stream, "200 OK", &json!({"decision": d, "risk": risk.label(), "reason": reason}).to_string(), &[])?);
    }
    if path == "/v1/open" {
        // only local callers (the second launch of the App); a remote agent may not open sessions here
        if !host_hdr.is_empty() { return Ok(respond(stream, "403 Forbidden", r#"{"error":"local callers only"}"#, &[])?); }
        let url = serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["url"].as_str().map(str::to_string)).unwrap_or_default();
        if let Err(e) = crate::link::parse(&url) { return Ok(respond(stream, "400 Bad Request", &json!({"error": e}).to_string(), &[])?); }
        let Some(tx) = OPEN_TX.get() else { return Ok(respond(stream, "501 Not Implemented", r#"{"error":"no UI"}"#, &[])?) };
        let _ = sh.audit.append("link.open", "user", &format!("url={:?} via=second-instance", crate::link::parse(&url).map(|l| format!("{:?}://{}{}", l.kind, l.user.map(|u| u + "@").unwrap_or_default(), l.host)).unwrap_or_default()));
        let _ = tx.send(url);
        return Ok(respond(stream, "200 OK", r#"{"ok":true}"#, &[])?);
    }
    if matches!(path.as_str(), "/v1/hosts" | "/v1/exec" | "/v1/upload" | "/v1/download") {
        let (status, out) = remote_op(sh, &path, &body, &host_hdr, Some(&stream));
        return Ok(respond(stream, status, &out.to_string(), &[])?);
    }
    if let Some(rest) = path.strip_prefix("/v1/hook/") {
        let mut it = rest.split('/');
        let Some(agent) = it.next().and_then(Agent::parse) else { return Ok(respond(stream, "404 Not Found", r#"{"error":"unknown agent"}"#, &[])?) };
        let mut req = match hooks::parse_request(&body) { Ok(r) => r, Err(e) => return Ok(respond(stream, "400 Bad Request", &json!({"error": e.to_string()}).to_string(), &[])?) };
        if let Some(ev) = it.next().filter(|e| !e.is_empty()) { req.event = ev.to_string(); }
        let (eff, _risk, reason) = sh.decide_peer(agent.id(), &req, &host_hdr, true, Some(&stream));
        let (out, code) = hooks::render(agent, &req.event, eff, &reason);
        return Ok(respond(stream, "200 OK", &out, &[("X-shellax-Exit", code.to_string())])?);
    }
    Ok(respond(stream, "404 Not Found", "{}", &[])?)
}

/// US-10: agent-initiated remote operations. Every call names a host from the App's list and
/// goes through the same permission engine (and approval card) as hook requests.
fn remote_op(sh: &Shared, path: &str, body: &str, host_hdr: &str, peer: Option<&TcpStream>) -> (&'static str, serde_json::Value) {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Op { principal: String, session: String, host: String, command: String, local: String, remote: String, timeout_secs: u64 }
    let Some(ops) = sh.ops.clone() else { return ("501 Not Implemented", json!({"error": "remote operations need the shellax app"})) };
    let o: Op = match serde_json::from_str(body) { Ok(o) => o, Err(e) => return ("400 Bad Request", json!({"error": e.to_string()})) };
    let principal = if o.principal.is_empty() { "cli".to_string() } else { o.principal.clone() };
    if path == "/v1/hosts" {
        let hs = ops.hosts();
        let _ = sh.audit.append("shellax.hosts", &principal, &format!("n={}", hs.len()));
        return ("200 OK", json!({"hosts": hs}));
    }
    // a caller on a remote host (reverse forward) must not move files on the App's machine
    if !host_hdr.is_empty() && path != "/v1/exec" { return ("403 Forbidden", json!({"error": "upload/download are only available to local callers"})); }
    if o.host.is_empty() { return ("400 Bad Request", json!({"error": "host is required"})); }
    let (action, command, kind) = match path {
        "/v1/exec" => ("exec", o.command.clone(), "shellax.exec"),
        "/v1/upload" => ("write", format!("cp {} {}", crate::ssh::sh_quote(&o.local), crate::ssh::sh_quote(&o.remote)), "shellax.upload"),
        _ => ("exec", format!("cat {}", crate::ssh::sh_quote(&o.remote)), "shellax.download"),
    };
    if command.trim().is_empty() || (path != "/v1/exec" && (o.local.is_empty() || o.remote.is_empty())) { return ("400 Bad Request", json!({"error": "missing command / local / remote"})); }
    let req = HookReq { event: "shellax".into(), tool: kind.trim_start_matches("shellax.").into(), command: command.clone(), action: action.into(), cwd: String::new(), session: o.session.clone() };
    let (eff, risk, reason) = sh.decide_peer(&principal, &req, &o.host, true, peer);
    // the caller may have gone away while the card was up: never run on its behalf
    if eff == Effect::Allow && peer.is_some_and(peer_closed) {
        let _ = sh.audit.append(kind, "policy", &format!("host={} session={} effect=Cancelled reason=\"caller disconnected\" command={command:?}", o.host, if o.session.is_empty() { "-" } else { &o.session }));
        return ("499 Client Closed Request", json!({"decision": "deny", "reason": "caller disconnected"}));
    }
    let d = match eff { Effect::Allow => "allow", Effect::Ask => "ask", Effect::Deny => "deny" };
    if eff != Effect::Allow { return ("200 OK", json!({"decision": d, "risk": risk.label(), "reason": reason})); }
    let r = match path {
        "/v1/exec" => ops.exec(&o.host, &o.command, Duration::from_secs(if o.timeout_secs == 0 { 600 } else { o.timeout_secs })).map(|(code, out)| json!({"exit": code, "output": String::from_utf8_lossy(&out)})),
        "/v1/upload" => ops.upload(&o.host, Path::new(&o.local), &o.remote).map(|n| json!({"bytes": n})),
        _ => ops.download(&o.host, &o.remote, Path::new(&o.local)).map(|n| json!({"bytes": n})),
    };
    match r {
        Ok(mut v) => {
            let _ = sh.audit.append(kind, &principal, &format!("host={} session={} effect=Allow command={command:?} result={}", o.host, if o.session.is_empty() { "-" } else { &o.session }, v.get("exit").map(|e| format!("exit {e}")).or_else(|| v.get("bytes").map(|b| format!("{b} bytes"))).unwrap_or_default()));
            v["decision"] = json!("allow"); v["risk"] = json!(risk.label()); v["reason"] = json!(reason);
            ("200 OK", v)
        }
        Err(e) => { let _ = sh.audit.append(kind, &principal, &format!("host={} error={:?}", o.host, format!("{e:#}"))); ("502 Bad Gateway", json!({"decision": "allow", "error": format!("{e:#}")})) }
    }
}

/// True when the HTTP caller has closed its end of the connection (EOF on a non-blocking peek).
pub fn peer_closed(s: &TcpStream) -> bool {
    if s.set_nonblocking(true).is_err() { return false; }
    let mut b = [0u8; 1];
    let r = s.peek(&mut b);
    let _ = s.set_nonblocking(false);
    match r { Ok(0) => true, Ok(_) => false, Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) }
}

impl Shared {
    /// Decide; while an approval card is pending, watch `peer` — if the caller disconnects the
    /// card is withdrawn (its reply channel is dropped, the UI removes it) and the request denied.
    fn decide_peer(&self, principal: &str, req: &HookReq, host: &str, interactive: bool, peer: Option<&TcpStream>) -> (Effect, Risk, String) {
        let ctx = (self.ctx)();
        let prod = ctx.prod_confirm && !host.is_empty() && ctx.prod_hosts.iter().any(|h| h == host || policy::glob(h, host));
        let risk = if req.action == "exec" { policy::classify(&req.command) } else { Risk::R1 };
        let where_ = if host.is_empty() { "local".to_string() } else { host.to_string() };
        let _ = self.audit.append("hook.request", principal, &format!("event={} tool={} action={} host={where_} session={} cwd={:?} command={:?}", req.event, req.tool, req.action, if req.session.is_empty() { "-" } else { req.session.as_str() }, req.cwd, req.command));
        let (mut eff, mut reason) = if req.action == "tool" {
            // non-shell, non-write tools: only explicit rules decide; otherwise no opinion (allow)
            match policy::evaluate(&ctx.rules, principal, "tool", &req.command) { Some((_, r)) => (r.effect, format!("rule {}", r.id)), None => (Effect::Allow, "not a shell/write tool".into()) }
        } else {
            policy::decide_host(ctx.mode, ctx.max_auto, &ctx.rules, principal, &req.action, &req.command, ctx.dangerous_confirm, prod, host)
        };
        let skey = (principal.to_string(), if req.session.is_empty() { "-".to_string() } else { req.session.clone() }, command_prefix(&req.command));
        if eff == Effect::Ask {
            // "always allow this prefix for this session" — never across a higher risk level
            if let Some(max) = self.session_allow.lock().unwrap().get(&skey) { if risk <= *max {
                eff = Effect::Allow; reason = format!("{reason} → allowed for this session ({:?})", skey.2);
                let _ = self.audit.append("hook.approval", "user", &format!("session-rule host={where_} session={} prefix={:?} command={:?}", skey.1, skey.2, req.command));
            } }
        }
        if eff == Effect::Ask && interactive {
            if let Some(tx) = &self.approvals {
                let (rtx, rrx) = flume::bounded(1);
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                let deadline = Instant::now() + ctx.approval_timeout;
                let a = Approval { id, principal: principal.into(), req: req.clone(), host: where_.clone(), risk, reason: reason.clone(), deadline, reply: rtx };
                if tx.send(a).is_ok() {
                    let mut gone = false;
                    let got = loop {
                        let slice = Duration::from_millis(200);
                        match rrx.recv_timeout(slice) {
                            Ok(a) => break Some(a),
                            Err(flume::RecvTimeoutError::Disconnected) => break None,
                            Err(flume::RecvTimeoutError::Timeout) => {}
                        }
                        if peer.is_some_and(peer_closed) { gone = true; break None; }
                        if ctx.timeout_deny && Instant::now() >= deadline { break None; }
                    };
                    drop(rrx); // withdraws the card: the UI drops approvals whose receiver is gone
                    let got = if gone { Some(Answer::Deny("\u{0}gone".into())) } else { got };
                    match got {
                        Some(Answer::Deny(w)) if w == "\u{0}gone" => {
                            eff = Effect::Deny; reason = format!("{reason} → caller disconnected, approval withdrawn");
                            let _ = self.audit.append("hook.approval", "policy", &format!("id={id} withdrawn caller-disconnected host={where_} session={} command={:?}", skey.1, req.command));
                        }
                        Some(Answer::Once) => { eff = Effect::Allow; reason = format!("{reason} → approved by user"); let _ = self.audit.append("hook.approval", "user", &format!("id={id} allow host={where_} session={} command={:?}", skey.1, req.command)); }
                        Some(Answer::Session) => {
                            eff = Effect::Allow; reason = format!("{reason} → approved by user for this session ({:?})", skey.2);
                            let _ = self.audit.append("hook.approval", "user", &format!("id={id} allow-session host={where_} session={} prefix={:?} command={:?}", skey.1, skey.2, req.command));
                            self.session_allow.lock().unwrap().insert(skey.clone(), risk);
                        }
                        Some(Answer::Deny(why)) => {
                            eff = Effect::Deny;
                            reason = if why.trim().is_empty() { format!("{reason} → denied by user") } else { format!("denied by user: {}", why.trim()) };
                            let _ = self.audit.append("hook.approval", "user", &format!("id={id} deny host={where_} session={} feedback={:?} command={:?}", skey.1, why.trim(), req.command));
                        }
                        None => { eff = Effect::Deny; reason = format!("{reason} → no answer within {}s, denied", ctx.approval_timeout.as_secs()); let _ = self.audit.append("hook.approval", "policy", &format!("id={id} timeout deny session={} host={where_}", skey.1)); }
                    }
                }
            }
        }
        let _ = self.audit.append("hook.decision", "policy", &format!("principal={principal} host={where_} session={} risk={} effect={eff:?} reason={reason:?} command={:?}", skey.1, risk.label(), req.command));
        (eff, risk, reason)
    }
}

/// Minimal blocking HTTP client used by the CLI (no TLS; localhost or a forwarded port).
pub fn post(url: &str, path: &str, token: &str, host_hdr: Option<&str>, body: &str, timeout: Duration) -> Result<(u16, String, Option<i32>)> {
    let rest = url.strip_prefix("http://").ok_or_else(|| anyhow!("only http:// endpoints are supported"))?;
    let addr = rest.trim_end_matches('/');
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(timeout))?;
    let mut req = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if let Some(h) = host_hdr { req.push_str(&format!("X-shellax-Host: {h}\r\n")); }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())?;
    s.write_all(body.as_bytes())?;
    let mut resp = String::new();
    s.read_to_string(&mut resp)?;
    let (head, body) = resp.split_once("\r\n\r\n").ok_or_else(|| anyhow!("bad HTTP response"))?;
    let status = head.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    let exit = head.lines().find_map(|l| l.strip_prefix("X-shellax-Exit: ")).and_then(|x| x.trim().parse().ok());
    Ok((status, body.to_string(), exit))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn srv(ctx: PolicyCtx, approvals: Option<flume::Sender<Approval>>) -> (Server, PathBuf) {
        let dir = std::env::temp_dir().join(format!("as-ps-{}-{}", std::process::id(), new_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLog::new(dir.join("audit.jsonl")));
        let s = start("127.0.0.1:0", None, Arc::new(move || ctx.clone()), approvals, audit, Some(endpoint_path(&dir))).unwrap();
        (s, dir)
    }
    #[test]
    fn check_auth_hooks_and_audit() {
        let (s, dir) = srv(PolicyCtx { prod_hosts: vec!["prod-*".into()], ..Default::default() }, None);
        let ep = read_endpoint(&dir).unwrap();
        assert_eq!(ep.token, s.token);
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(endpoint_path(&dir)).unwrap().permissions().mode() & 0o777, 0o600); }
        let t = Duration::from_secs(5);
        assert_eq!(post(&ep.url, "/v1/check", "wrong", None, "{}", t).unwrap().0, 401);
        let (_, b, _) = post(&ep.url, "/v1/check", &ep.token, None, r#"{"command":"ls -la"}"#, t).unwrap();
        assert!(b.contains("\"allow\""), "{b}");
        let (_, b, _) = post(&ep.url, "/v1/check", &ep.token, None, r#"{"command":"rm -rf /"}"#, t).unwrap();
        assert!(b.contains("\"deny\""));
        let (_, b, _) = post(&ep.url, "/v1/hook/claude/PreToolUse", &ep.token, None, r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#, t).unwrap();
        assert!(b.contains("\"ask\""), "{b}");
        let (_, b, code) = post(&ep.url, "/v1/hook/grok", &ep.token, None, r#"{"toolName":"bash","toolInput":{"command":"mkfs.ext4 /dev/sda1"}}"#, t).unwrap();
        assert!(b.contains("deny") && code == Some(2));
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(log.contains("hook.request") && log.contains("hook.decision") && log.contains("policy.auth_fail"));
        assert!(crate::audit::verify(&dir.join("audit.jsonl")).is_ok());
    }
    #[test]
    fn approvals_and_timeout() {
        let (tx, rx) = flume::unbounded::<Approval>();
        let (s, dir) = srv(PolicyCtx { approval_timeout: Duration::from_millis(600), ..Default::default() }, Some(tx));
        let url = format!("http://127.0.0.1:{}", s.port);
        let tok = s.token.clone();
        std::thread::spawn(move || { let a = rx.recv().unwrap(); assert_eq!(a.req.command, "rm a"); a.reply.send(Answer::Once).unwrap(); let _b = rx.recv().unwrap(); /* never answer */ std::thread::sleep(Duration::from_secs(2)); });
        let (_, b, _) = post(&url, "/v1/hook/claude", &tok, None, r#"{"tool_name":"Bash","tool_input":{"command":"rm a"}}"#, Duration::from_secs(5)).unwrap();
        assert!(b.contains("\"allow\"") && b.contains("approved"), "{b}");
        let (_, b, _) = post(&url, "/v1/hook/claude", &tok, None, r#"{"tool_name":"Bash","tool_input":{"command":"rm b"}}"#, Duration::from_secs(5)).unwrap();
        assert!(b.contains("\"deny\"") && b.contains("no answer"), "{b}");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn session_allow_and_deny_with_reason() {
        let (tx, rx) = flume::unbounded::<Approval>();
        let (s, dir) = srv(PolicyCtx { approval_timeout: Duration::from_secs(3), ..Default::default() }, Some(tx));
        let url = format!("http://127.0.0.1:{}", s.port);
        let tok = s.token.clone();
        let asked = Arc::new(AtomicU64::new(0));
        let a2 = asked.clone();
        std::thread::spawn(move || while let Ok(a) = rx.recv() {
            let n = a2.fetch_add(1, Ordering::SeqCst);
            let ans = if n == 0 { Answer::Session } else { Answer::Deny("use trash-cli instead".into()) };
            a.reply.send(ans).unwrap();
        });
        let t = Duration::from_secs(5);
        let body = |sess: &str, cmd: &str| format!(r#"{{"session_id":"{sess}","tool_name":"Bash","tool_input":{{"command":"{cmd}"}}}}"#);
        let (_, b, _) = post(&url, "/v1/hook/claude", &tok, None, &body("s1", "git push origin a"), t).unwrap();
        assert!(b.contains("\"allow\"") && b.contains("this session"), "{b}");
        let (_, b, _) = post(&url, "/v1/hook/claude", &tok, None, &body("s1", "git push origin b"), t).unwrap();
        assert!(b.contains("\"allow\""), "{b}");
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        let (_, b, _) = post(&url, "/v1/hook/claude", &tok, None, &body("s2", "git push origin c"), t).unwrap();
        assert!(b.contains("\"deny\"") && b.contains("use trash-cli instead"), "{b}");
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(command_prefix("git push origin a"), "git push");
        assert_eq!(command_prefix("FOO=1 rm -rf x"), "rm");
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(log.contains("allow-session") && log.contains("session-rule") && log.contains("feedback"));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn production_host_header() {
        let (s, _dir) = srv(PolicyCtx { mode: Mode::Auto, max_auto: Risk::R2, prod_hosts: vec!["prod-*".into()], ..Default::default() }, None);
        let url = format!("http://127.0.0.1:{}", s.port);
        let t = Duration::from_secs(5);
        let (_, b, _) = post(&url, "/v1/check", &s.token, None, r#"{"command":"touch x"}"#, t).unwrap();
        assert!(b.contains("\"allow\""));
        let (_, b, _) = post(&url, "/v1/check", &s.token, Some("prod-web"), r#"{"command":"touch x"}"#, t).unwrap();
        assert!(b.contains("\"ask\""), "{b}");
    }

    struct FakeOps(std::sync::Mutex<Vec<String>>);
    impl crate::broker::Ops for FakeOps {
        fn hosts(&self) -> Vec<crate::broker::HostInfo> { vec![crate::broker::HostInfo { name: "web1".into(), user: "u".into(), host: "10.0.0.1".into(), port: 22, production: false }] }
        fn exec(&self, host: &str, c: &str, _t: Duration) -> Result<(Option<u32>, Vec<u8>)> { self.0.lock().unwrap().push(format!("exec {host} {c}")); Ok((Some(0), b"hi\n".to_vec())) }
        fn upload(&self, host: &str, l: &Path, r: &str) -> Result<u64> { self.0.lock().unwrap().push(format!("up {host} {} {r}", l.display())); Ok(3) }
        fn download(&self, host: &str, r: &str, l: &Path) -> Result<u64> { self.0.lock().unwrap().push(format!("down {host} {r} {}", l.display())); Ok(4) }
    }
    #[test]
    fn remote_ops_go_through_policy() {
        let dir = std::env::temp_dir().join(format!("as-ops-{}-{}", std::process::id(), new_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLog::new(dir.join("audit.jsonl")));
        let ops = Arc::new(FakeOps(Default::default()));
        let (ap_tx, ap_rx) = flume::unbounded::<Approval>();
        let s = start_with_ops("127.0.0.1:0", None, Arc::new(PolicyCtx::default), Some(ap_tx), audit, None, Some(ops.clone())).unwrap();
        let url = format!("http://127.0.0.1:{}", s.port); let t = Duration::from_secs(10);
        let (c, b, _) = post(&url, "/v1/hosts", &s.token, None, "{}", t).unwrap();
        assert_eq!(c, 200); assert!(b.contains("web1") && !b.contains("password"), "{b}");
        let (_, b, _) = post(&url, "/v1/exec", &s.token, None, r#"{"host":"web1","command":"uptime"}"#, t).unwrap();
        assert!(b.contains("\"allow\"") && b.contains("hi"), "{b}");
        let (_, b, _) = post(&url, "/v1/exec", &s.token, None, r#"{"host":"web1","command":"rm -rf /"}"#, t).unwrap();
        assert!(b.contains("\"deny\""), "{b}");
        // R2+ asks: user denies on the card, nothing runs
        std::thread::spawn(move || { let a = ap_rx.recv().unwrap(); assert_eq!(a.host, "web1"); a.reply.send(Answer::Deny("no".into())).unwrap(); let a = ap_rx.recv().unwrap(); a.reply.send(Answer::Once).unwrap(); });
        let (_, b, _) = post(&url, "/v1/exec", &s.token, None, r#"{"host":"web1","command":"systemctl restart nginx","session":"s9"}"#, t).unwrap();
        assert!(b.contains("\"deny\""), "{b}");
        let (_, b, _) = post(&url, "/v1/upload", &s.token, None, r#"{"host":"web1","local":"/tmp/a","remote":"/tmp/b"}"#, t).unwrap();
        assert!(b.contains("\"bytes\":3"), "{b}");
        let (c, _, _) = post(&url, "/v1/download", &s.token, Some("web1"), r#"{"host":"web1","local":"/tmp/a","remote":"/etc/passwd"}"#, t).unwrap();
        assert_eq!(c, 403);
        let calls = ops.0.lock().unwrap().clone();
        assert_eq!(calls, vec!["exec web1 uptime".to_string(), "up web1 /tmp/a /tmp/b".into()]);
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(log.contains("shellax.exec") && log.contains("shellax.upload") && log.contains("shellax.hosts"));
    }

    #[test]
    fn approval_withdrawn_when_caller_disconnects() {
        let dir = std::env::temp_dir().join(format!("as-wd-{}-{}", std::process::id(), new_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let audit = Arc::new(AuditLog::new(dir.join("audit.jsonl")));
        let ops = Arc::new(FakeOps(Default::default()));
        let (ap_tx, ap_rx) = flume::unbounded::<Approval>();
        let s = start_with_ops("127.0.0.1:0", None, Arc::new(PolicyCtx::default), Some(ap_tx), audit, None, Some(ops.clone())).unwrap();
        for (path, body) in [("/v1/exec", r#"{"host":"web1","command":"systemctl restart nginx"}"#), ("/v1/hook/claude/PreToolUse", r#"{"tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#)] {
            let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
            write!(c, "POST {path} HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}", s.token, body.len()).unwrap();
            let a = ap_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(!a.reply.is_disconnected());
            drop(c); // caller goes away (agent killed / Ctrl-C)
            let t0 = Instant::now();
            while !a.reply.is_disconnected() { assert!(t0.elapsed() < Duration::from_secs(3), "card not withdrawn"); std::thread::sleep(Duration::from_millis(50)); }
            let _ = a.reply.send(Answer::Once); // a late click must not run anything
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(ops.0.lock().unwrap().is_empty());
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert_eq!(log.matches("withdrawn caller-disconnected").count(), 2, "{log}");
    }
}
