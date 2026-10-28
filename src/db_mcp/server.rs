//! Streamable HTTP MCP listener. Binds 127.0.0.1 only. Every request needs the bearer token.
//!
//! A JSON body is returned when the client accepts `application/json` (or sends no Accept).
//! A client that only accepts `text/event-stream` gets one SSE event. Notifications get 202.
//! `Host` / `Origin`, when present, must be localhost.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::auth::{authorize, resolve, ConnPolicy};
use super::sql::{self, Engine};
use super::CONFIRM_WAIT;

pub struct Limits { pub max_rows: u32, pub timeout: Duration }

#[derive(Clone)]
pub struct PublicConn {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub database: String,
    pub read_only: bool,
    pub production: bool,
    pub engine: Engine,
}

pub struct ConnView {
    pub id: String,
    pub name: String,
    pub policy: ConnPolicy,
    pub engine: Engine,
    pub database: String,
}

#[derive(Clone)]
pub struct ColumnOut {
    pub name: String,
    pub type_name: String,
    pub nullable: Option<bool>,
    pub comment: String,
}

#[derive(Clone)]
pub struct SetOut {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: u64,
    pub truncated: bool,
    pub error: Option<String>,
}

pub struct RunOut { pub sets: Vec<SetOut>, pub row_count: u64 }

pub struct ConfirmReq {
    pub client: String,
    pub conn_name: String,
    pub sql: String,
    pub estimate: Option<u64>,
}

pub struct AuditEvent {
    pub client: String,
    pub tool: String,
    pub conn: String,
    pub rows: u64,
    pub sql: String,
    pub effect: String,
}

pub trait Backend: Send + Sync {
    fn limits(&self) -> Limits;
    fn connections(&self) -> Vec<PublicConn>;
    /// Exposed connection only. Hidden connections use the same "not found" error.
    fn prepare(&self, id_or_name: &str) -> Result<ConnView, String>;
    fn databases(&self, conn: &str) -> Result<Vec<String>, String>;
    fn tables(&self, conn: &str, database: &str) -> Result<Vec<String>, String>;
    fn columns(&self, conn: &str, database: &str, table: &str) -> Result<Vec<ColumnOut>, String>;
    fn run_sql(&self, conn: &str, database: &str, sql: &str, max_rows: u32, timeout: Duration) -> Result<RunOut, String>;
    fn confirm(&self, req: ConfirmReq) -> bool;
    fn audit(&self, ev: &AuditEvent);
}

pub struct Running {
    port: u16,
    token: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
}

impl Running {
    pub fn port(&self) -> u16 { self.port }
    pub fn token(&self) -> String { self.token.lock().unwrap().clone() }
    /// The next request must use this token. The previous value stops working immediately.
    pub fn set_token(&self, token: String) { *self.token.lock().unwrap() = token; }
}

impl Drop for Running {
    fn drop(&mut self) { self.stop.store(true, Ordering::Relaxed); }
}

struct Shared {
    token: Arc<Mutex<String>>,
    backend: Arc<dyn Backend>,
    sessions: Mutex<std::collections::HashMap<String, String>>,
}

pub fn start(port: u16, token: String, backend: Arc<dyn Backend>) -> anyhow::Result<Running> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let token = Arc::new(Mutex::new(token));
    let stop = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared { token: token.clone(), backend, sessions: Mutex::new(std::collections::HashMap::new()) });
    let stop_flag = stop.clone();
    thread::Builder::new().name("db-mcp".into()).spawn(move || {
        while !stop_flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let sh = shared.clone();
                    thread::spawn(move || { let _ = handle(stream, &sh); });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => thread::sleep(Duration::from_millis(15)),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    })?;
    Ok(Running { port, token, stop })
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn host_ok(host: &str) -> bool {
    let h = host.trim();
    if h.is_empty() { return true; }
    let name = match h.rsplit_once(':') {
        Some((n, p)) if !n.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => n,
        _ => h,
    };
    let name = name.trim_matches(|c| c == '[' || c == ']');
    matches!(name, "127.0.0.1" | "localhost")
}

fn origin_ok(origin: &str) -> bool {
    let o = origin.trim();
    if o.is_empty() { return true; }
    let rest = o.trim_start_matches("https://").trim_start_matches("http://");
    host_ok(rest.split('/').next().unwrap_or(""))
}

struct Req {
    method: String,
    path: String,
    len: usize,
    auth: String,
    accept: String,
    host: String,
    origin: String,
    session: String,
    body: String,
}

fn read_req(stream: &mut TcpStream) -> std::io::Result<Req> {
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut r = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let (mut len, mut auth, mut accept, mut host, mut origin, mut session) = (0usize, String::new(), String::new(), String::new(), String::new(), String::new());
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" { break; }
        let Some((k, v)) = h.split_once(':') else { continue };
        let v = v.trim().to_string();
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => len = v.parse().unwrap_or(0),
            "authorization" => auth = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")).unwrap_or(&v).trim().to_string(),
            "accept" => accept = v,
            "host" => host = v,
            "origin" => origin = v,
            "mcp-session-id" => session = v,
            _ => {}
        }
    }
    if len > 1 << 20 {
        return Ok(Req { method, path, len: usize::MAX, auth, accept, host, origin, session, body: String::new() });
    }
    let mut body = vec![0u8; len];
    if len > 0 { r.read_exact(&mut body)?; }
    let body = String::from_utf8_lossy(&body).into_owned();
    Ok(Req { method, path, len, auth, accept, host, origin, session, body })
}

fn peer_closed(s: &TcpStream) -> bool {
    if s.set_nonblocking(true).is_err() { return false; }
    let mut b = [0u8; 1];
    let r = s.peek(&mut b);
    let _ = s.set_nonblocking(false);
    match r {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted),
    }
}

fn wants_sse(accept: &str) -> bool {
    let a = accept.to_ascii_lowercase();
    if a.is_empty() { return false; }
    let json = a.contains("application/json") || a.contains("*/*");
    let sse = a.contains("text/event-stream");
    sse && !json
}

fn write_resp(mut s: TcpStream, status: &str, body: &str, content_type: &str, session: Option<&str>) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if let Some(id) = session { h.push_str(&format!("Mcp-Session-Id: {id}\r\n")); }
    h.push_str("\r\n");
    s.write_all(h.as_bytes())?;
    if !body.is_empty() { s.write_all(body.as_bytes())?; }
    s.flush()
}

fn handle(mut stream: TcpStream, sh: &Shared) -> anyhow::Result<()> {
    let _ = stream.set_nonblocking(false);
    let req = read_req(&mut stream)?;
    if req.len == usize::MAX { return Ok(write_resp(stream, "413 Payload Too Large", "{}", "application/json", None)?); }
    if req.method == "GET" || req.method == "DELETE" {
        return Ok(write_resp(stream, "405 Method Not Allowed", "", "application/json", None)?);
    }
    if req.method != "POST" || req.path != "/mcp" {
        return Ok(write_resp(stream, "404 Not Found", "{}", "application/json", None)?);
    }
    let token = sh.token.lock().unwrap().clone();
    if !ct_eq(&req.auth, &token) {
        sh.backend.audit(&AuditEvent { client: "-".into(), tool: "auth".into(), conn: "-".into(), rows: 0, sql: String::new(), effect: "deny".into() });
        return Ok(write_resp(stream, "401 Unauthorized", r#"{"error":"bad token"}"#, "application/json", None)?);
    }
    if !host_ok(&req.host) || !origin_ok(&req.origin) {
        return Ok(write_resp(stream, "403 Forbidden", r#"{"error":"localhost only"}"#, "application/json", None)?);
    }
    let rpc: Value = match serde_json::from_str(&req.body) { Ok(v) => v, Err(_) => {
        return Ok(write_resp(stream, "400 Bad Request", &err_body(Value::Null, -32700, "parse error"), "application/json", None)?);
    }};
    if rpc.is_array() {
        return Ok(write_resp(stream, "400 Bad Request", &err_body(Value::Null, -32600, "batch is not supported"), "application/json", None)?);
    }
    let id = rpc.get("id").cloned().unwrap_or(Value::Null);
    let method = rpc.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let params = rpc.get("params").cloned().unwrap_or(Value::Null);
    let notification = !rpc.get("id").is_some_and(|v| !v.is_null());
    if notification || method.starts_with("notifications/") {
        return Ok(write_resp(stream, "202 Accepted", "", "application/json", None)?);
    }
    let mut session = req.session.clone();
    if method == "initialize" && session.is_empty() { session = super::new_token(); }
    if method == "initialize" {
        let name = client_name(params.get("clientInfo").and_then(|c| c.get("name")).and_then(|n| n.as_str()).unwrap_or("mcp"));
        if !session.is_empty() { sh.sessions.lock().unwrap().insert(session.clone(), name); }
    }
    let client = sh.sessions.lock().unwrap().get(&session).cloned().unwrap_or_else(|| "mcp".into());
    let alive = { let s = stream.try_clone().ok(); move || s.as_ref().is_none_or(|s| !peer_closed(s)) };
    let result = dispatch(sh.backend.as_ref(), &client, &method, &params, &alive);
    let body = match result {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}).to_string(),
        Err((code, msg)) => err_body(id, code, &msg),
    };
    let sid = if session.is_empty() { None } else { Some(session.as_str()) };
    if wants_sse(&req.accept) {
        let ev = format!("event: message\ndata: {body}\n\n");
        return Ok(write_resp(stream, "200 OK", &ev, "text/event-stream", sid)?);
    }
    Ok(write_resp(stream, "200 OK", &body, "application/json", sid)?)
}

fn err_body(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn client_name(s: &str) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).take(80).collect();
    if t.trim().is_empty() { "mcp".into() } else { t }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n).collect::<String>() + "…" }
}

fn dispatch(b: &dyn Backend, client: &str, method: &str, params: &Value, alive: &dyn Fn() -> bool) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "shellax-db", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(b, client, name, &args, alive) {
                Ok(v) => Ok(v),
                Err(msg) => Ok(tool_err(&msg)),
            }
        }
        _ => Err((-32601, format!("unknown method {method}"))),
    }
}

fn tool_text(v: &Value) -> Value {
    json!({"content": [{"type": "text", "text": v.to_string()}], "isError": false})
}

fn tool_err(msg: &str) -> Value {
    json!({"content": [{"type": "text", "text": msg}], "isError": true})
}

fn tools() -> Value {
    let t = |name, desc, schema| json!({"name": name, "description": desc, "inputSchema": schema});
    let conn = json!({"type": "string", "description": "Connection id from list_connections"});
    let db = json!({"type": "string", "description": "Database or schema. Empty uses the connection default."});
    let sql = json!({"type": "string", "description": "A single SQL statement"});
    json!([
        t("list_connections", "List connections this server may use. Hosts, users and passwords are not returned.", json!({"type": "object", "properties": {}})),
        t("describe_schema", "List visible databases, or the tables in one database.", json!({"type": "object", "properties": {"connection": conn, "database": db}, "required": ["connection"]})),
        t("describe_table", "List columns of one visible table.", json!({"type": "object", "properties": {"connection": conn, "database": db, "table": {"type": "string"}}, "required": ["connection", "table"]})),
        t("query", "Run one read-only statement (SELECT). Writes are rejected.", json!({"type": "object", "properties": {"connection": conn, "database": db, "sql": sql}, "required": ["connection", "sql"]})),
        t("execute", "Run one authorized INSERT, UPDATE, DELETE or DDL statement. Writes wait for confirmation in the app unless the table is set to run directly.", json!({"type": "object", "properties": {"connection": conn, "database": db, "sql": sql}, "required": ["connection", "sql"]})),
        t("explain", "Show the plan for one read statement. EXPLAIN ANALYZE is rejected.", json!({"type": "object", "properties": {"connection": conn, "database": db, "sql": sql}, "required": ["connection", "sql"]})),
    ])
}

fn call_tool(b: &dyn Backend, client: &str, name: &str, args: &Value, alive: &dyn Fn() -> bool) -> Result<Value, String> {
    match name {
        "list_connections" => {
            let rows = b.connections();
            let n = rows.len() as u64;
            let body = json!({"connections": rows.iter().map(|c| json!({
                "id": c.id, "name": c.name, "kind": c.kind, "database": c.database,
                "read_only": c.read_only, "production": c.production
            })).collect::<Vec<_>>()});
            audit(b, client, name, "-", n, "", "allow");
            Ok(tool_text(&body))
        }
        "describe_schema" => describe_schema(b, client, args),
        "describe_table" => describe_table(b, client, args),
        "query" | "execute" | "explain" => sql_tool(b, client, name, args, alive),
        _ => Err("unknown tool".into()),
    }
}

fn audit(b: &dyn Backend, client: &str, tool: &str, conn: &str, rows: u64, sql: &str, effect: &str) {
    b.audit(&AuditEvent { client: client.into(), tool: tool.into(), conn: conn.into(), rows, sql: clip(sql, 8000), effect: effect.into() });
}

fn arg<'a>(args: &'a Value, key: &str) -> &'a str { args.get(key).and_then(|v| v.as_str()).unwrap_or("").trim() }

fn describe_schema(b: &dyn Backend, client: &str, args: &Value) -> Result<Value, String> {
    let key = arg(args, "connection");
    let db = arg(args, "database").to_string();
    let view = match b.prepare(key) {
        Ok(v) => v,
        Err(e) => { audit(b, client, "describe_schema", key, 0, &db, "deny"); return Err(e); }
    };
    if db.is_empty() {
        let all = match b.databases(&view.id) {
            Ok(v) => v,
            Err(e) => { audit(b, client, "describe_schema", &view.id, 0, "", "deny"); return Err(super::scrub(&e)); }
        };
        let names: Vec<_> = all.into_iter().filter(|d| resolve(&view.policy, d, "").0 > super::auth::Level::Hidden).collect();
        let n = names.len() as u64;
        audit(b, client, "describe_schema", &view.id, n, "", "allow");
        return Ok(tool_text(&json!({"connection": view.id, "databases": names})));
    }
    if resolve(&view.policy, &db, "").0 == super::auth::Level::Hidden {
        audit(b, client, "describe_schema", &view.id, 0, &db, "deny");
        return Err("object is not visible".into());
    }
    let all = match b.tables(&view.id, &db) {
        Ok(v) => v,
        Err(e) => { audit(b, client, "describe_schema", &view.id, 0, &db, "deny"); return Err(super::scrub(&e)); }
    };
    let names: Vec<_> = all.into_iter().filter(|t| resolve(&view.policy, &db, t).0 > super::auth::Level::Hidden).collect();
    let n = names.len() as u64;
    audit(b, client, "describe_schema", &view.id, n, &db, "allow");
    Ok(tool_text(&json!({"connection": view.id, "database": db, "tables": names})))
}

fn describe_table(b: &dyn Backend, client: &str, args: &Value) -> Result<Value, String> {
    let key = arg(args, "connection");
    let table = arg(args, "table").to_string();
    if table.is_empty() {
        audit(b, client, "describe_table", key, 0, "", "deny");
        return Err("table is required".into());
    }
    let view = match b.prepare(key) {
        Ok(v) => v,
        Err(e) => { audit(b, client, "describe_table", key, 0, &table, "deny"); return Err(e); }
    };
    let db = { let d = arg(args, "database"); if d.is_empty() { view.database.clone() } else { d.to_string() } };
    if resolve(&view.policy, &db, &table).0 == super::auth::Level::Hidden {
        audit(b, client, "describe_table", &view.id, 0, &table, "deny");
        return Err("object is not visible".into());
    }
    let cols = match b.columns(&view.id, &db, &table) {
        Ok(v) => v,
        Err(e) => { audit(b, client, "describe_table", &view.id, 0, &table, "deny"); return Err(super::scrub(&e)); }
    };
    let body = json!({"connection": view.id, "database": db, "table": table, "columns": cols.iter().map(|c| json!({
        "name": c.name, "type": c.type_name, "nullable": c.nullable, "comment": clip(&c.comment, 500)
    })).collect::<Vec<_>>()});
    audit(b, client, "describe_table", &view.id, cols.len() as u64, &table, "allow");
    Ok(tool_text(&body))
}

fn sql_tool(b: &dyn Backend, client: &str, name: &str, args: &Value, alive: &dyn Fn() -> bool) -> Result<Value, String> {
    let key = arg(args, "connection").to_string();
    let sql = arg(args, "sql").to_string();
    let database = arg(args, "database").to_string();
    if key.is_empty() || sql.is_empty() {
        audit(b, client, name, "-", 0, &sql, "deny");
        return Err("connection and sql are required".into());
    }
    let view = match b.prepare(&key) {
        Ok(v) => v,
        Err(e) => { audit(b, client, name, &key, 0, &sql, "deny"); return Err(e); }
    };
    let db = if database.is_empty() { view.database.clone() } else { database };
    let analysis = match sql::analyze(&sql, view.engine) {
        Ok(a) => a,
        Err(e) => { audit(b, client, name, &view.id, 0, &sql, "deny"); return Err(e.message().into()); }
    };
    let gate = match name {
        "query" if analysis.explain_analyze => Some("query does not run EXPLAIN ANALYZE"),
        "query" if analysis.kind != sql::Kind::Select => Some("query is read-only"),
        "explain" if analysis.explain_analyze => Some("explain does not run EXPLAIN ANALYZE; use execute"),
        "explain" if analysis.kind != sql::Kind::Select => Some("explain only accepts a read statement"),
        "execute" if analysis.kind == sql::Kind::Select && !analysis.explain_analyze => Some("execute does not run read-only SQL; use query"),
        _ => None,
    };
    if let Some(m) = gate {
        audit(b, client, name, &view.id, 0, &sql, "deny");
        return Err(m.into());
    }
    match authorize(&analysis, &view.policy, &db) {
        Err(d) => { audit(b, client, name, &view.id, 0, &sql, "deny"); return Err(d.message().into()); }
        Ok(dec) if dec.confirm => {
            let estimate = estimate_rows(b, &view, &analysis, &db);
            let _ = CONFIRM_WAIT;
            if !alive() { audit(b, client, name, &view.id, 0, &sql, "deny"); return Err("caller disconnected".into()); }
            if !b.confirm(ConfirmReq { client: client.into(), conn_name: view.name.clone(), sql: sql.clone(), estimate }) {
                audit(b, client, name, &view.id, 0, &sql, "deny");
                return Err("confirmation denied".into());
            }
        }
        Ok(_) => {}
    }
    if !alive() { audit(b, client, name, &view.id, 0, &sql, "deny"); return Err("caller disconnected".into()); }
    let limits = b.limits();
    let run = if name == "explain" && !analysis.explain { format!("EXPLAIN {sql}") } else { sql.clone() };
    match b.run_sql(&view.id, &db, &run, limits.max_rows, limits.timeout) {
        Ok(out) => {
            if let Some(err) = out.sets.iter().find_map(|s| s.error.clone()) {
                audit(b, client, name, &view.id, 0, &sql, "deny");
                return Err(super::scrub(&err));
            }
            audit(b, client, name, &view.id, out.row_count, &sql, "allow");
            Ok(tool_text(&format_out(&out)))
        }
        Err(e) => { audit(b, client, name, &view.id, 0, &sql, "deny"); Err(super::scrub(&e)) }
    }
}

fn estimate_rows(b: &dyn Backend, view: &ConnView, a: &sql::Analysis, db: &str) -> Option<u64> {
    if a.explain_analyze { return None; }
    if let Some(n) = a.insert_rows { return Some(n); }
    let q = sql::count_sql(a, db, view.engine)?;
    let limits = b.limits();
    let out = b.run_sql(&view.id, db, &q, 1, limits.timeout).ok()?;
    out.sets.first()?.rows.first()?.first()?.as_deref()?.parse().ok()
}

fn format_out(out: &RunOut) -> Value {
    let sets: Vec<_> = out.sets.iter().map(|s| json!({
        "columns": s.columns,
        "rows": s.rows.iter().map(|r| r.iter().map(|c| c.as_ref().map(|v| clip(v, 4000))).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "affected": s.affected,
        "truncated": s.truncated,
    })).collect();
    json!({"sets": sets, "rows": out.row_count})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Fake {
        runs: Mutex<Vec<String>>,
        confirms: AtomicUsize,
    }
    impl Fake {
        fn new() -> Self { Self { runs: Mutex::new(vec![]), confirms: AtomicUsize::new(0) } }
    }
    impl Backend for Fake {
        fn limits(&self) -> Limits { Limits { max_rows: 50, timeout: Duration::from_secs(5) } }
        fn connections(&self) -> Vec<PublicConn> {
            vec![PublicConn { id: "c1".into(), name: "app".into(), kind: "mysql".into(), database: "app".into(), read_only: false, production: false, engine: Engine::Mysql }]
        }
        fn prepare(&self, id: &str) -> Result<ConnView, String> {
            if id != "c1" && id != "app" { return Err("connection not found".into()); }
            Ok(ConnView {
                id: "c1".into(), name: "app".into(), engine: Engine::Mysql, database: "app".into(),
                policy: crate::db_mcp::policy_for(&[], "c1", false, false),
            })
        }
        fn databases(&self, _: &str) -> Result<Vec<String>, String> { Ok(vec!["app".into()]) }
        fn tables(&self, _: &str, _: &str) -> Result<Vec<String>, String> { Ok(vec!["t".into()]) }
        fn columns(&self, _: &str, _: &str, _: &str) -> Result<Vec<ColumnOut>, String> { Ok(vec![]) }
        fn run_sql(&self, _: &str, _: &str, sql: &str, _: u32, _: Duration) -> Result<RunOut, String> {
            self.runs.lock().unwrap().push(sql.to_string());
            Ok(RunOut { row_count: 1, sets: vec![SetOut { columns: vec!["n".into()], rows: vec![vec![Some("1".into())]], affected: 0, truncated: false, error: None }] })
        }
        fn confirm(&self, _: ConfirmReq) -> bool { self.confirms.fetch_add(1, Ordering::Relaxed); true }
        fn audit(&self, _: &AuditEvent) {}
    }

    fn post(port: u16, token: Option<&str>, body: &str, host: &str) -> (u16, String) {
        let mut last = String::new();
        for _ in 0..20 {
            let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) else { thread::sleep(Duration::from_millis(20)); continue };
            let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
            let req = format!("POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            s.write_all(req.as_bytes()).unwrap();
            let mut buf = String::new();
            s.read_to_string(&mut buf).unwrap();
            last = buf;
            break;
        }
        let status = last.split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0);
        (status, last)
    }

    #[test]
    fn token_is_required_and_regeneration_invalidates_the_old_one() {
        let fake = Arc::new(Fake::new());
        let running = start(0, "secret-token".into(), fake.clone()).unwrap();
        let port = running.port();
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"test-client"}}}"#;
        assert_eq!(post(port, None, init, "127.0.0.1").0, 401);
        assert_eq!(post(port, Some("nope"), init, "127.0.0.1").0, 401);
        assert_eq!(post(port, Some("secret-token"), init, "evil.example").0, 403);
        let (st, body) = post(port, Some("secret-token"), init, "127.0.0.1");
        assert_eq!(st, 200, "{body}");
        assert!(body.contains("2025-03-26") && body.contains("shellax-db"));
        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let (_, tools) = post(port, Some("secret-token"), list, "localhost");
        assert!(tools.contains("list_connections") && tools.contains("describe_table") && tools.contains("execute"));
        running.set_token("rotated".into());
        assert_eq!(post(port, Some("secret-token"), list, "127.0.0.1").0, 401);
        assert_eq!(post(port, Some("rotated"), list, "127.0.0.1").0, 200);
        drop(running);
    }

    #[test]
    fn multi_statement_and_missing_where_do_not_run() {
        let fake = Arc::new(Fake::new());
        let running = start(0, "t".into(), fake.clone()).unwrap();
        let port = running.port();
        let call = |sql: &str, tool: &str| {
            let body = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name": tool, "arguments": {"connection": "c1", "sql": sql}}}).to_string();
            post(port, Some("t"), &body, "127.0.0.1")
        };
        let (_, multi) = call("SELECT 1; SELECT 2", "query");
        assert!(multi.contains("multiple statements"), "{multi}");
        let (_, bare) = call("UPDATE t SET a = 1", "execute");
        assert!(bare.contains("WHERE"), "{bare}");
        let (_, ddl) = call("DROP TABLE t", "query");
        assert!(ddl.contains("read-only"), "{ddl}");
        assert!(fake.runs.lock().unwrap().is_empty(), "ran {:?}", fake.runs.lock().unwrap());
        assert_eq!(fake.confirms.load(Ordering::Relaxed), 0);
        let (_, ok) = call("SELECT 1", "query");
        assert!(ok.contains("\"isError\":false") || ok.contains("\"isError\": false"), "{ok}");
        assert_eq!(fake.runs.lock().unwrap().len(), 1);
        drop(running);
    }
}
