//! Redis access (v0.2 PRD §6.4): standalone, sentinel and cluster connections (ACL user,
//! TLS, SSH tunnel), SCAN-based key listing (never KEYS), typed reads with paging, edits,
//! TTL, server info and slow log, and a console. The UI talks only to [`Kv`].
pub mod cmd;
pub mod tree;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use redis::aio::MultiplexedConnection;
use redis::{cluster_async::ClusterConnection, ConnectionAddr, IntoConnectionInfo, RedisConnectionInfo, Value};

use crate::assets::RedisConn;
use crate::forward::Running;
use crate::ssh::{Prompts, SshTarget};

/// Keys asked per SCAN call.
pub const SCAN_COUNT: usize = 1000;
/// Elements per page of a hash / list / set / zset / stream.
pub const PAGE: usize = 200;

#[derive(Clone)]
enum Conn { Single(MultiplexedConnection), Cluster(ClusterConnection) }

impl Conn {
    async fn query(&self, c: &redis::Cmd) -> redis::RedisResult<Value> {
        match self {
            Conn::Single(m) => { let mut m = m.clone(); c.query_async(&mut m).await }
            Conn::Cluster(m) => { let mut m = m.clone(); c.query_async(&mut m).await }
        }
    }
}

pub struct Kv {
    conn: Conn,
    /// Cluster: one connection per master, for SCAN / DBSIZE / INFO that are per node.
    nodes: Vec<(String, MultiplexedConnection)>,
    pub mode: String,
    pub db: u32,
    pub version: String,
    pub via: Option<String>,
    tunnels: Vec<Arc<Running>>,
}

/// Where SCAN continues: node index (cluster) and that node's cursor. `done` = finished.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Cursor { pub node: usize, pub cursor: u64, pub done: bool }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyMeta { pub key: String, pub kind: String, pub ttl_ms: i64, pub len: Option<u64>, pub bytes: Option<u64>, pub encoding: String }

/// One page of a key's content.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// Whole value (strings longer than 1 MiB are cut; `full_len` says how long it is).
    Str { value: Vec<u8>, full_len: u64 },
    /// hash: (field, value); set: (member, ""); zset: (member, score); list: (index, value)
    Pairs { items: Vec<(String, String)>, next: Option<u64> },
    Stream { entries: Vec<(String, Vec<(String, String)>)>, next: Option<String>, groups: Vec<(String, u64, u64, String)> },
}

pub const STR_MAX: u64 = 1 << 20;

fn rcmd(args: &[&str]) -> redis::Cmd { let mut c = redis::cmd(args[0]); for a in &args[1..] { c.arg(*a); } c }

fn text(v: &Value) -> String { cmd::scalar(v).unwrap_or_default() }
fn int(v: &Value) -> i64 { match v { Value::Int(i) => *i, other => text(other).parse().unwrap_or(0) } }
fn list(v: Value) -> Vec<Value> {
    match v { Value::Array(a) | Value::Set(a) => a, Value::Map(m) => m.into_iter().flat_map(|(k, v)| [k, v]).collect(), Value::Nil => vec![], o => vec![o] }
}

fn info(c: &RedisConn, password: Option<&str>) -> RedisConnectionInfo {
    let mut r = RedisConnectionInfo::default().set_db(c.db as i64);
    if !c.user.is_empty() { r = r.set_username(&c.user); }
    if let Some(p) = password.filter(|p| !p.is_empty()) { r = r.set_password(p); }
    r
}

fn addr(c: &RedisConn, host: &str, port: u16) -> ConnectionAddr {
    match c.tls.as_str() {
        "" | "disable" => ConnectionAddr::Tcp(host.to_string(), port),
        t => ConnectionAddr::TcpTls { host: host.to_string(), port, insecure: t == "insecure", tls_params: None },
    }
}

fn conn_info(c: &RedisConn, password: Option<&str>, host: &str, port: u16) -> Result<redis::ConnectionInfo> {
    Ok(format!("redis://{host}:{port}").as_str().into_connection_info()?.set_addr(addr(c, host, port)).set_redis_settings(info(c, password)))
}

async fn single(ci: redis::ConnectionInfo, timeout: Duration) -> Result<MultiplexedConnection> {
    let client = redis::Client::open(ci)?;
    let cfg = redis::AsyncConnectionConfig::new().set_connection_timeout(Some(timeout)).set_response_timeout(None);
    Ok(tokio::time::timeout(timeout, client.get_multiplexed_async_connection_with_config(&cfg)).await.context("timeout")??)
}

impl Kv {
    /// Connect `c`. With `via`, the server (standalone only; sentinel and cluster need
    /// direct reach to every node) is reached through an SSH tunnel.
    pub async fn open(c: &RedisConn, password: Option<String>, via: Option<(SshTarget, Prompts)>) -> Result<Kv> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let timeout = c.connect_timeout();
        let pw = password.as_deref();
        let mode = if c.mode.is_empty() { "standalone".to_string() } else { c.mode.clone() };
        let mut tunnels = vec![];
        let mut via_label = None;
        let mut nodes = vec![];
        let conn = match mode.as_str() {
            "cluster" => {
                if via.is_some() { bail!(crate::i18n::t("redis.err_cluster_ssh").to_string()); }
                let seeds: Vec<redis::ConnectionInfo> = c.seeds().iter().map(|(h, p)| conn_info(c, pw, h, *p)).collect::<Result<_>>()?;
                let mut b = redis::cluster::ClusterClientBuilder::new(seeds).connection_timeout(timeout);
                if !c.user.is_empty() { b = b.username(&c.user); }
                if let Some(p) = pw.filter(|p| !p.is_empty()) { b = b.password(p); }
                if !c.tls.is_empty() && c.tls != "disable" { b = b.tls(if c.tls == "insecure" { redis::TlsMode::Insecure } else { redis::TlsMode::Secure }); }
                let cc = tokio::time::timeout(timeout, b.build()?.get_async_connection()).await.context("timeout")??;
                // masters for per-node commands
                let conn = Conn::Cluster(cc);
                let slots = conn.query(&rcmd(&["CLUSTER", "SLOTS"])).await?;
                let mut seen = vec![];
                for range in list(slots) {
                    let r = list(range);
                    if let Some(m) = r.get(2).cloned() {
                        let m = list(m);
                        let (h, p) = (text(&m[0]), int(&m[1]) as u16);
                        if !seen.contains(&(h.clone(), p)) { seen.push((h, p)); }
                    }
                }
                seen.sort();
                for (h, p) in seen {
                    let mut ci = conn_info(c, pw, &h, p)?;
                    ci = ci.set_redis_settings(info(c, pw).set_db(0));
                    nodes.push((format!("{h}:{p}"), single(ci, timeout).await?));
                }
                conn
            }
            "sentinel" => {
                if via.is_some() { bail!(crate::i18n::t("redis.err_cluster_ssh").to_string()); }
                if c.master.is_empty() { bail!(crate::i18n::t("redis.err_master").to_string()); }
                let sentinels: Vec<redis::ConnectionInfo> = c.seeds().iter().map(|(h, p)| Ok(format!("redis://{h}:{p}").as_str().into_connection_info()?.set_addr(addr(c, h, *p)))).collect::<Result<_>>()?;
                let mut node = redis::sentinel::SentinelNodeConnectionInfo::default().set_redis_connection_info(info(c, pw));
                if !c.tls.is_empty() && c.tls != "disable" { node = node.set_tls_mode(if c.tls == "insecure" { redis::TlsMode::Insecure } else { redis::TlsMode::Secure }); }
                let mut sc = redis::sentinel::SentinelClient::build(sentinels, c.master.clone(), Some(node), redis::sentinel::SentinelServerType::Master)?;
                Conn::Single(tokio::time::timeout(timeout, sc.get_async_connection()).await.context("timeout")??)
            }
            _ => {
                let (host, port) = match via {
                    Some((t, prompts)) => {
                        let (r, p) = crate::db::live::tunnel_to(&t, &prompts, &c.host, c.effective_port()).await?;
                        tunnels.push(r);
                        via_label = Some(t.label());
                        ("127.0.0.1".to_string(), p)
                    }
                    None => (c.host.clone(), c.effective_port()),
                };
                let mut ci = conn_info(c, pw, &host, port)?;
                // TLS through a tunnel: verify against the real host name
                if !tunnels.is_empty() { if let ConnectionAddr::TcpTls { insecure, .. } = addr(c, &c.host, port) { ci = ci.set_addr(ConnectionAddr::TcpTls { host, port, insecure: insecure || true, tls_params: None }); } }
                match single(ci, timeout).await {
                    Ok(m) => Conn::Single(m),
                    Err(e) => { for t in &tunnels { t.stop().await; } return Err(e); }
                }
            }
        };
        let mut kv = Kv { conn, nodes, mode, db: c.db, version: String::new(), via: via_label, tunnels };
        let srv = kv.info_section("server").await.unwrap_or_default();
        kv.version = srv.iter().find(|(k, _)| k == "redis_version" || k == "valkey_version").map(|(_, v)| v.clone()).unwrap_or_default();
        Ok(kv)
    }

    pub async fn close(&self) { for t in &self.tunnels { t.stop().await; } }
    pub fn is_cluster(&self) -> bool { matches!(self.conn, Conn::Cluster(_)) }

    pub async fn raw(&self, args: &[String]) -> Result<Value> {
        anyhow::ensure!(!args.is_empty(), "empty command");
        let mut c = redis::cmd(&args[0]);
        for a in &args[1..] { c.arg(a.as_bytes()); }
        Ok(self.conn.query(&c).await?)
    }
    async fn q(&self, args: &[&str]) -> Result<Value> { Ok(self.conn.query(&rcmd(args)).await?) }
    async fn qb(&self, c: redis::Cmd) -> Result<Value> { Ok(self.conn.query(&c).await?) }

    /// A console line: run it and render the reply. `SELECT` is refused (the tab's db is fixed).
    pub async fn console(&self, line: &str) -> Result<String> {
        let args = cmd::split_line(line).map_err(anyhow::Error::msg)?;
        if args.is_empty() { return Ok(String::new()); }
        if args[0].eq_ignore_ascii_case("select") { bail!(crate::i18n::t("redis.err_select").to_string()); }
        match self.raw(&args).await {
            Ok(v) => Ok(cmd::render(&v)),
            Err(e) => Ok(format!("(error) {e}")),
        }
    }

    // ---- keys
    /// Next batch of keys. `pattern` is a glob (`*` = all), `kind` filters by type.
    pub async fn scan(&self, mut cur: Cursor, pattern: &str, kind: Option<&str>) -> Result<(Vec<String>, Cursor)> {
        let pat = if pattern.trim().is_empty() { "*" } else { pattern.trim() };
        let mut c = redis::cmd("SCAN");
        c.arg(cur.cursor).arg("MATCH").arg(pat).arg("COUNT").arg(SCAN_COUNT);
        if let Some(k) = kind { c.arg("TYPE").arg(k); }
        let v = if self.nodes.is_empty() { self.qb(c).await? } else {
            let Some((_, n)) = self.nodes.get(cur.node) else { return Ok((vec![], Cursor { done: true, ..cur })) };
            let mut n = n.clone();
            c.query_async(&mut n).await?
        };
        let mut parts = list(v).into_iter();
        let next: u64 = parts.next().map(|v| text(&v).parse().unwrap_or(0)).unwrap_or(0);
        let keys: Vec<String> = parts.next().map(list).unwrap_or_default().iter().map(text).collect();
        cur.cursor = next;
        if next == 0 {
            if self.nodes.is_empty() || cur.node + 1 >= self.nodes.len() { cur.done = true; } else { cur.node += 1; }
        }
        Ok((keys, cur))
    }

    /// Keys in the database (sum over masters in a cluster).
    pub async fn dbsize(&self) -> Result<u64> {
        if self.nodes.is_empty() { return Ok(int(&self.q(&["DBSIZE"]).await?) as u64); }
        let mut n = 0;
        for (_, c) in &self.nodes { let mut c = c.clone(); let v: i64 = redis::cmd("DBSIZE").query_async(&mut c).await?; n += v as u64; }
        Ok(n)
    }

    pub async fn meta(&self, key: &str) -> Result<KeyMeta> {
        let kind = text(&self.qb(redis::cmd("TYPE").arg(key).clone()).await?);
        if kind == "none" { bail!(crate::i18n::tf("redis.err_no_key", &[("key", &key)])); }
        let ttl_ms = int(&self.qb(redis::cmd("PTTL").arg(key).clone()).await?);
        let len_cmd = match kind.as_str() { "string" => Some("STRLEN"), "hash" => Some("HLEN"), "list" => Some("LLEN"), "set" => Some("SCARD"), "zset" => Some("ZCARD"), "stream" => Some("XLEN"), _ => None };
        let len = match len_cmd { Some(c) => self.qb(redis::cmd(c).arg(key).clone()).await.ok().map(|v| int(&v) as u64), None => None };
        let bytes = self.qb(redis::cmd("MEMORY").arg("USAGE").arg(key).clone()).await.ok().and_then(|v| if matches!(v, Value::Nil) { None } else { Some(int(&v) as u64) });
        let encoding = self.qb(redis::cmd("OBJECT").arg("ENCODING").arg(key).clone()).await.map(|v| text(&v)).unwrap_or_default();
        Ok(KeyMeta { key: key.to_string(), kind, ttl_ms, len, bytes, encoding })
    }

    /// One page of `key`'s content. `from` = SCAN cursor (hash/set), index (list/zset);
    /// streams page by entry id (`from_id`, exclusive after the first page).
    pub async fn read(&self, key: &str, kind: &str, from: u64, from_id: Option<&str>, filter: &str) -> Result<Content> {
        let pat = if filter.trim().is_empty() { "*".to_string() } else { filter.trim().to_string() };
        let pairs = |v: Vec<Value>| v.chunks(2).map(|c| (text(&c[0]), c.get(1).map(text).unwrap_or_default())).collect::<Vec<_>>();
        Ok(match kind {
            "string" => {
                let full_len = int(&self.qb(redis::cmd("STRLEN").arg(key).clone()).await?) as u64;
                let v = self.qb(redis::cmd("GETRANGE").arg(key).arg(0).arg(STR_MAX as i64 - 1).clone()).await?;
                Content::Str { value: match v { Value::BulkString(b) => b, o => text(&o).into_bytes() }, full_len }
            }
            "hash" | "set" => {
                let c = if kind == "hash" { "HSCAN" } else { "SSCAN" };
                let mut items = vec![];
                let mut cursor = from;
                // SCAN may return few or no items per call; gather up to a page
                loop {
                    let v = list(self.qb(redis::cmd(c).arg(key).arg(cursor).arg("MATCH").arg(&pat).arg("COUNT").arg(PAGE).clone()).await?);
                    cursor = v.first().map(|x| text(x).parse().unwrap_or(0)).unwrap_or(0);
                    let got = v.get(1).cloned().map(list).unwrap_or_default();
                    if kind == "hash" { items.extend(pairs(got)); } else { items.extend(got.iter().map(|m| (text(m), String::new()))); }
                    if cursor == 0 || items.len() >= PAGE { break; }
                }
                Content::Pairs { items, next: (cursor != 0).then_some(cursor) }
            }
            "list" => {
                let v = list(self.qb(redis::cmd("LRANGE").arg(key).arg(from).arg(from + PAGE as u64 - 1).clone()).await?);
                let n = v.len();
                Content::Pairs { items: v.iter().enumerate().map(|(i, x)| ((from + i as u64).to_string(), text(x))).collect(), next: (n == PAGE).then_some(from + PAGE as u64) }
            }
            "zset" => {
                let v = list(self.qb(redis::cmd("ZRANGE").arg(key).arg(from).arg(from + PAGE as u64 - 1).arg("WITHSCORES").clone()).await?);
                // RESP3 may nest [member, score] pairs
                let flat: Vec<Value> = v.into_iter().flat_map(|x| match x { Value::Array(a) => a, o => vec![o] }).collect();
                let items = pairs(flat);
                let n = items.len();
                Content::Pairs { items, next: (n == PAGE).then_some(from + PAGE as u64) }
            }
            "stream" => {
                let start = match from_id { Some(id) => format!("({id}"), None => "-".into() };
                let v = list(self.qb(redis::cmd("XRANGE").arg(key).arg(&start).arg("+").arg("COUNT").arg(PAGE).clone()).await?);
                let entries: Vec<(String, Vec<(String, String)>)> = v.into_iter().map(|e| { let e = list(e); (e.first().map(text).unwrap_or_default(), e.get(1).cloned().map(|f| pairs(list(f))).unwrap_or_default()) }).collect();
                let next = (entries.len() == PAGE).then(|| entries.last().map(|e| e.0.clone())).flatten();
                let groups = match self.qb(redis::cmd("XINFO").arg("GROUPS").arg(key).clone()).await {
                    Ok(g) => list(g).into_iter().map(|g| {
                        let kv = pairs(list(g));
                        let get = |k: &str| kv.iter().find(|x| x.0 == k).map(|x| x.1.clone()).unwrap_or_default();
                        (get("name"), get("consumers").parse().unwrap_or(0), get("pending").parse().unwrap_or(0), get("last-delivered-id"))
                    }).collect(),
                    Err(_) => vec![],
                };
                Content::Stream { entries, next, groups }
            }
            other => bail!(crate::i18n::tf("redis.err_type", &[("t", &other)])),
        })
    }

    // ---- edits
    pub async fn set_string(&self, key: &str, v: &[u8], keep_ttl: bool) -> Result<()> {
        let mut c = redis::cmd("SET"); c.arg(key).arg(v);
        if keep_ttl { c.arg("KEEPTTL"); }
        self.qb(c).await?; Ok(())
    }
    pub async fn hset(&self, key: &str, field: &str, v: &str) -> Result<()> { self.qb(redis::cmd("HSET").arg(key).arg(field).arg(v).clone()).await?; Ok(()) }
    pub async fn hdel(&self, key: &str, field: &str) -> Result<()> { self.qb(redis::cmd("HDEL").arg(key).arg(field).clone()).await?; Ok(()) }
    pub async fn lset(&self, key: &str, idx: i64, v: &str) -> Result<()> { self.qb(redis::cmd("LSET").arg(key).arg(idx).arg(v).clone()).await?; Ok(()) }
    pub async fn lpush(&self, key: &str, v: &str, tail: bool) -> Result<()> { self.qb(redis::cmd(if tail { "RPUSH" } else { "LPUSH" }).arg(key).arg(v).clone()).await?; Ok(()) }
    /// Remove the element at `idx`: mark it with a unique value, then LREM that value.
    pub async fn ldel(&self, key: &str, idx: i64) -> Result<()> {
        let mark = format!("__shellax_del_{}__", crate::assets::new_id());
        self.lset(key, idx, &mark).await?;
        self.qb(redis::cmd("LREM").arg(key).arg(1).arg(&mark).clone()).await?;
        Ok(())
    }
    pub async fn sadd(&self, key: &str, m: &str) -> Result<()> { self.qb(redis::cmd("SADD").arg(key).arg(m).clone()).await?; Ok(()) }
    pub async fn srem(&self, key: &str, m: &str) -> Result<()> { self.qb(redis::cmd("SREM").arg(key).arg(m).clone()).await?; Ok(()) }
    pub async fn zadd(&self, key: &str, m: &str, score: f64) -> Result<()> { self.qb(redis::cmd("ZADD").arg(key).arg(score).arg(m).clone()).await?; Ok(()) }
    pub async fn zrem(&self, key: &str, m: &str) -> Result<()> { self.qb(redis::cmd("ZREM").arg(key).arg(m).clone()).await?; Ok(()) }
    pub async fn xadd(&self, key: &str, fields: &[(String, String)]) -> Result<String> {
        anyhow::ensure!(!fields.is_empty(), "no fields");
        let mut c = redis::cmd("XADD"); c.arg(key).arg("*");
        for (f, v) in fields { c.arg(f).arg(v); }
        Ok(text(&self.qb(c).await?))
    }
    pub async fn xdel(&self, key: &str, id: &str) -> Result<()> { self.qb(redis::cmd("XDEL").arg(key).arg(id).clone()).await?; Ok(()) }

    /// New key of `kind` with a first element (`a`, `b` per type: string value; hash field/value;
    /// list/set member; zset member/score; stream field/value). Fails if the key exists.
    pub async fn create(&self, key: &str, kind: &str, a: &str, b: &str) -> Result<()> {
        if int(&self.qb(redis::cmd("EXISTS").arg(key).clone()).await?) > 0 { bail!(crate::i18n::tf("redis.err_exists", &[("key", &key)])); }
        match kind {
            "string" => self.set_string(key, a.as_bytes(), false).await,
            "hash" => self.hset(key, a, b).await,
            "list" => self.lpush(key, a, true).await,
            "set" => self.sadd(key, a).await,
            "zset" => self.zadd(key, a, b.trim().parse().map_err(|_| anyhow::anyhow!(crate::i18n::t("redis.err_score").to_string()))?).await,
            "stream" => self.xadd(key, &[(a.to_string(), b.to_string())]).await.map(|_| ()),
            other => bail!(crate::i18n::tf("redis.err_type", &[("t", &other)])),
        }
    }

    /// TTL in seconds; `None` = persist.
    pub async fn set_ttl(&self, key: &str, secs: Option<i64>) -> Result<()> {
        match secs {
            Some(s) if s > 0 => { self.qb(redis::cmd("EXPIRE").arg(key).arg(s).clone()).await?; }
            Some(_) => bail!(crate::i18n::t("redis.err_ttl").to_string()),
            None => { self.qb(redis::cmd("PERSIST").arg(key).clone()).await?; }
        }
        Ok(())
    }
    /// Rename without overwriting an existing key.
    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        if int(&self.qb(redis::cmd("RENAMENX").arg(from).arg(to).clone()).await?) == 0 { bail!(crate::i18n::tf("redis.err_exists", &[("key", &to)])); }
        Ok(())
    }
    pub async fn copy(&self, from: &str, to: &str) -> Result<()> {
        if int(&self.qb(redis::cmd("COPY").arg(from).arg(to).clone()).await?) == 0 { bail!(crate::i18n::tf("redis.err_exists", &[("key", &to)])); }
        Ok(())
    }
    pub async fn delete(&self, keys: &[String]) -> Result<u64> {
        let mut n = 0;
        // one key per call: works across cluster slots
        for k in keys { n += int(&self.qb(redis::cmd("UNLINK").arg(k).clone()).await?) as u64; }
        Ok(n)
    }
    /// Keys matching `pattern` (all of them, via SCAN), for a pattern delete preview.
    pub async fn matching(&self, pattern: &str, limit: usize) -> Result<Vec<String>> {
        let mut cur = Cursor::default();
        let mut out = vec![];
        while !cur.done && out.len() < limit {
            let (k, c) = self.scan(cur, pattern, None).await?;
            out.extend(k);
            cur = c;
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    // ---- server
    pub async fn info_section(&self, section: &str) -> Result<Vec<(String, String)>> {
        let v = if self.nodes.is_empty() { self.q(&["INFO", section]).await? } else {
            let mut c = self.nodes[0].1.clone();
            redis::cmd("INFO").arg(section).query_async(&mut c).await?
        };
        Ok(parse_info(&text(&v)).into_iter().map(|(_, k, v)| (k, v)).collect())
    }
    /// Number of logical databases (`CONFIG GET databases`; 16 when the server refuses, 1 in a cluster).
    pub async fn database_count(&self) -> u32 {
        if !self.nodes.is_empty() || self.is_cluster() { return 1; }
        match self.q(&["CONFIG", "GET", "databases"]).await {
            Ok(v) => list(v).get(1).map(text).and_then(|s| s.trim().parse().ok()).filter(|n| *n > 0).unwrap_or(16),
            Err(_) => 16,
        }
    }
    /// Key counts per database from `INFO keyspace`.
    pub async fn keyspace(&self) -> Vec<(u32, u64)> { self.info_section("keyspace").await.map(|v| keyspace_counts(&v)).unwrap_or_default() }
    /// INFO of the server (first master in a cluster) as (section, key, value).
    pub async fn info(&self) -> Result<Vec<(String, String, String)>> {
        let v = if self.nodes.is_empty() { self.q(&["INFO", "everything"]).await.or(self.q(&["INFO"]).await)? } else {
            let mut c = self.nodes[0].1.clone();
            redis::cmd("INFO").query_async(&mut c).await?
        };
        Ok(parse_info(&text(&v)))
    }
    pub fn node_names(&self) -> Vec<String> { self.nodes.iter().map(|n| n.0.clone()).collect() }

    /// Slow log: (id, unix time, microseconds, command, client).
    pub async fn slowlog(&self, n: usize) -> Result<Vec<SlowEntry>> {
        let v = if self.nodes.is_empty() { self.qb(redis::cmd("SLOWLOG").arg("GET").arg(n).clone()).await? } else {
            let mut c = self.nodes[0].1.clone();
            redis::cmd("SLOWLOG").arg("GET").arg(n).query_async(&mut c).await?
        };
        Ok(list(v).into_iter().map(|e| {
            let e = list(e);
            let args: Vec<String> = e.get(3).cloned().map(list).unwrap_or_default().iter().map(text).collect();
            SlowEntry { id: e.first().map(int).unwrap_or(0), at: e.get(1).map(int).unwrap_or(0), micros: e.get(2).map(int).unwrap_or(0), command: args.join(" "), client: e.get(4).map(text).unwrap_or_default() }
        }).collect())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlowEntry { pub id: i64, pub at: i64, pub micros: i64, pub command: String, pub client: String }

/// `# Section` / `key:value` lines into (section, key, value).
/// `db3` → `keys=12,expires=0,…` pairs to (3, 12).
pub fn keyspace_counts(v: &[(String, String)]) -> Vec<(u32, u64)> {
    v.iter().filter_map(|(k, v)| {
        let n = k.strip_prefix("db")?.parse().ok()?;
        let keys = v.split(',').find_map(|p| p.strip_prefix("keys="))?.parse().ok()?;
        Some((n, keys))
    }).collect()
}

pub fn parse_info(s: &str) -> Vec<(String, String, String)> {
    let mut sec = String::new();
    let mut out = vec![];
    for line in s.lines().map(str::trim) {
        if let Some(h) = line.strip_prefix('#') { sec = h.trim().to_string(); continue; }
        if let Some((k, v)) = line.split_once(':') { out.push((sec.clone(), k.to_string(), v.to_string())); }
    }
    out
}

/// Pretty JSON when `s` parses as a JSON object or array.
pub fn pretty_json(s: &str) -> Option<String> {
    let t = s.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) { return None; }
    serde_json::from_str::<serde_json::Value>(s).ok().and_then(|v| serde_json::to_string_pretty(&v).ok())
}

/// Hex dump: offset, 16 bytes, ASCII.
pub fn hex_dump(b: &[u8]) -> String {
    b.chunks(16).enumerate().map(|(i, c)| {
        let hex: Vec<String> = c.iter().map(|x| format!("{x:02x}")).collect();
        let asc: String = c.iter().map(|&x| if x.is_ascii_graphic() || x == b' ' { x as char } else { '.' }).collect();
        format!("{:08x}  {:<47}  {asc}", i * 16, hex.join(" "))
    }).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn keyspace() {
        let v = vec![("db0".to_string(), "keys=12,expires=1,avg_ttl=0".to_string()), ("db3".into(), "keys=4,expires=0".into()), ("x".into(), "keys=1".into())];
        assert_eq!(super::keyspace_counts(&v), vec![(0, 12), (3, 4)]);
    }
    use super::*;
    #[test]
    fn info_parse() {
        let v = parse_info("# Server\r\nredis_version:7.2.4\r\n\r\n# Keyspace\r\ndb0:keys=3,expires=0\r\n");
        assert_eq!(v, vec![("Server".into(), "redis_version".into(), "7.2.4".into()), ("Keyspace".into(), "db0".into(), "keys=3,expires=0".into())]);
    }
    #[test]
    fn json_and_hex() {
        assert_eq!(pretty_json("{\"a\":1}").unwrap(), "{\n  \"a\": 1\n}");
        assert!(pretty_json("12").is_none());
        assert_eq!(hex_dump(b"AB\x00"), "00000000  41 42 00                                         AB.");
    }
    #[test]
    fn seeds() {
        let c = RedisConn { host: "a".into(), nodes: "b:7001, [::1]:7002 c".into(), ..Default::default() };
        assert_eq!(c.seeds(), vec![("a".to_string(), 6379), ("b".into(), 7001), ("::1".into(), 7002), ("c".into(), 6379)]);
    }
}
