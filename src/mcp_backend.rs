//! In-process backend for the database MCP listener.
//!
//! Schema listing and SQL run on the shared Tokio runtime. The HTTP thread blocks
//! here; the confirmation dialog is answered on the UI thread through `ask`.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ssh_term_proto::assets::{self, DbConn};
use ssh_term_proto::audit::AuditLog;
use ssh_term_proto::db::{self, RunOpts};
use ssh_term_proto::db_mcp::{self, AuditEvent, Backend, ColumnOut, ConfirmReq, ConnView, Limits, PublicConn, RunOut, SetOut};
use ssh_term_proto::ssh::Prompts;

use crate::config;

/// One write waiting for the in-app dialog. Dropping `reply` denies the write.
pub(crate) struct Ask {
    pub req: ConfirmReq,
    pub reply: flume::Sender<bool>,
    pub deadline: Instant,
}

struct Slot { fp: u64, live: Arc<db::live::Live> }

pub(crate) struct McpBackend {
    rt: Arc<tokio::runtime::Runtime>,
    prompts: Prompts,
    confirm_tx: flume::Sender<Ask>,
    audit: Arc<AuditLog>,
    cache: Mutex<HashMap<String, Slot>>,
}

impl McpBackend {
    pub(crate) fn new(rt: Arc<tokio::runtime::Runtime>, prompts: Prompts, confirm_tx: flume::Sender<Ask>, audit: Arc<AuditLog>) -> Self {
        Self { rt, prompts, confirm_tx, audit, cache: Mutex::new(HashMap::new()) }
    }

    fn conn(id: &str) -> Result<DbConn, String> {
        config::get().db.iter().find(|c| c.id == id && db_mcp::supported(c.kind)).cloned().ok_or_else(|| "connection not found".into())
    }

    fn via(conn: &DbConn, prompts: &Prompts) -> Result<Option<(ssh_term_proto::ssh::SshTarget, Prompts)>, String> {
        if conn.ssh_via.is_empty() { return Ok(None); }
        let hosts = config::resolved_hosts();
        match hosts.iter().find(|(h, _)| h.name == conn.ssh_via) {
            Some((_, t)) => Ok(Some((t.clone(), prompts.clone()))),
            None => Err("connection failed".into()),
        }
    }

    fn hide(err: &str, conn: &DbConn, password: &str) -> String {
        let mut s = db_mcp::scrub(err);
        for secret in [conn.host.as_str(), conn.user.as_str(), conn.file.as_str(), conn.ssh_via.as_str(), conn.tls_ca.as_str(), conn.tls_server_name.as_str(), password] {
            if secret.len() >= 3 { s = s.replace(secret, "***"); }
        }
        s
    }

    fn fingerprint(conn: &DbConn, password: &str) -> u64 {
        let mut h = DefaultHasher::new();
        conn.kind.id().hash(&mut h);
        conn.host.hash(&mut h);
        conn.port.hash(&mut h);
        conn.user.hash(&mut h);
        conn.database.hash(&mut h);
        conn.file.hash(&mut h);
        conn.ssh_via.hash(&mut h);
        conn.read_only.hash(&mut h);
        conn.tls.hash(&mut h);
        conn.tls_ca.hash(&mut h);
        conn.tls_server_name.hash(&mut h);
        conn.connect_timeout_secs.hash(&mut h);
        let mut pw = DefaultHasher::new();
        password.hash(&mut pw);
        pw.finish().hash(&mut h);
        h.finish()
    }

    fn live(&self, id: &str) -> Result<(Arc<db::live::Live>, DbConn), String> {
        let conn = Self::conn(id)?;
        let password = crate::secret_store().get(&conn.password_secret()).map(|x| x.0);
        let fp = Self::fingerprint(&conn, password.as_deref().unwrap_or(""));
        if let Some(hit) = self.cache.lock().unwrap().get(id).filter(|s| s.fp == fp).map(|s| s.live.clone()) {
            return Ok((hit, conn));
        }
        let via = Self::via(&conn, &self.prompts)?;
        let pw_for_err = password.clone().unwrap_or_default();
        let opened = self.rt.block_on(db::live::open(&conn, password, via));
        let live = match opened {
            Ok(l) => Arc::new(l),
            Err(e) => return Err(Self::hide(&format!("{e:#}"), &conn, &pw_for_err)),
        };
        let old = self.cache.lock().unwrap().insert(id.to_string(), Slot { fp, live: live.clone() });
        if let Some(old) = old {
            let rt = self.rt.clone();
            rt.spawn(async move { old.live.close().await; });
        }
        Ok((live, conn))
    }
}

impl Backend for McpBackend {
    fn limits(&self) -> Limits {
        let c = config::get().db_mcp.clone();
        Limits { max_rows: c.max_rows.clamp(1, db::MAX_ROWS as u32), timeout: Duration::from_secs(c.timeout_secs.clamp(1, 300)) }
    }

    fn connections(&self) -> Vec<PublicConn> {
        let s = config::get();
        let prod = s.tags.production.clone();
        s.db.iter().filter(|c| db_mcp::supported(c.kind)).filter_map(|c| {
            let production = assets::db_is_production(c, &s.groups, &prod);
            let policy = db_mcp::policy_for(&s.db_mcp.grants, &c.id, c.read_only, production);
            if !db_mcp::exposed(&policy) { return None; }
            Some(PublicConn {
                id: c.id.clone(), name: c.name.clone(), kind: c.kind.id().into(), database: c.database.clone(),
                read_only: c.read_only, production, engine: db_mcp::engine_of(c.kind)?,
            })
        }).collect()
    }

    fn prepare(&self, id_or_name: &str) -> Result<ConnView, String> {
        let s = config::get();
        let conn = if let Some(c) = s.db.iter().find(|c| c.id == id_or_name && db_mcp::supported(c.kind)) {
            c
        } else {
            let named: Vec<_> = s.db.iter().filter(|c| db_mcp::supported(c.kind) && c.name.eq_ignore_ascii_case(id_or_name)).collect();
            if named.len() == 1 { named[0] } else { return Err("connection not found".into()); }
        };
        let production = assets::db_is_production(conn, &s.groups, &s.tags.production);
        let policy = db_mcp::policy_for(&s.db_mcp.grants, &conn.id, conn.read_only, production);
        if !db_mcp::exposed(&policy) { return Err("connection not found".into()); }
        let engine = db_mcp::engine_of(conn.kind).ok_or_else(|| "connection not found".to_string())?;
        Ok(ConnView { id: conn.id.clone(), name: conn.name.clone(), policy, engine, database: conn.database.clone() })
    }

    fn databases(&self, conn: &str) -> Result<Vec<String>, String> {
        let (live, c) = self.live(conn)?;
        self.rt.block_on(live.driver.databases()).map_err(|e| Self::hide(&format!("{e:#}"), &c, ""))
    }

    fn tables(&self, conn: &str, database: &str) -> Result<Vec<String>, String> {
        let (live, c) = self.live(conn)?;
        let db = database.to_string();
        self.rt.block_on(async move { live.driver.tables(&db, None).await }).map(|v| v.into_iter().map(|t| t.name).collect()).map_err(|e| Self::hide(&format!("{e:#}"), &c, ""))
    }

    fn columns(&self, conn: &str, database: &str, table: &str) -> Result<Vec<ColumnOut>, String> {
        let (live, c) = self.live(conn)?;
        let (db, table) = (database.to_string(), table.to_string());
        self.rt.block_on(async move { live.driver.columns(&db, None, &table).await }).map(|v| v.into_iter().map(|col| ColumnOut {
            name: col.name, type_name: col.type_name, nullable: col.nullable, comment: col.comment,
        }).collect()).map_err(|e| Self::hide(&format!("{e:#}"), &c, ""))
    }

    fn run_sql(&self, conn: &str, database: &str, sql: &str, max_rows: u32, timeout: Duration) -> Result<RunOut, String> {
        let (live, c) = self.live(conn)?;
        let max_rows = (max_rows as usize).clamp(1, db::MAX_ROWS);
        let (db, sql) = (database.to_string(), sql.to_string());
        let opts = RunOpts::default();
        let ran = self.rt.block_on(async move { tokio::time::timeout(timeout, live.driver.run(&db, vec![sql], max_rows, &opts)).await });
        let sets = match ran {
            Err(_) => return Err("statement timed out".into()),
            Ok(Err(e)) => return Err(Self::hide(&format!("{e:#}"), &c, "")),
            Ok(Ok(v)) => v,
        };
        let mut row_count = 0u64;
        let sets = sets.into_iter().map(|s| {
            let n = if s.returns_rows { s.rows.len() as u64 } else { s.affected };
            row_count += n;
            SetOut {
                columns: s.columns.into_iter().map(|c| c.name).collect(),
                rows: s.rows,
                affected: s.affected,
                truncated: s.truncated,
                error: s.error.map(|e| Self::hide(&e, &c, "")),
            }
        }).collect();
        Ok(RunOut { sets, row_count })
    }

    fn confirm(&self, req: ConfirmReq) -> bool {
        let (tx, rx) = flume::bounded(1);
        let deadline = Instant::now() + db_mcp::CONFIRM_WAIT;
        if self.confirm_tx.send(Ask { req, reply: tx, deadline }).is_err() { return false; }
        rx.recv_deadline(deadline).unwrap_or(false)
    }

    fn audit(&self, ev: &AuditEvent) {
        let detail = format!("client={:?} tool={} conn={} rows={} effect={} sql={:?}", ev.client, ev.tool, ev.conn, ev.rows, ev.effect, ev.sql);
        let _ = self.audit.append("db.mcp", "mcp", &detail);
    }
}
