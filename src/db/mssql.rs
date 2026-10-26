//! SQL Server driver (TDS via tiberius). One session per driver, guarded by a mutex; the
//! database is switched with `USE` per request. Reads go through the batch protocol so
//! every row set comes back; other statements report affected row counts.
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, FromSql, QueryItem};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use super::{dialect, split, BoxFut, CancelSlot, ColumnInfo, ResultSet, RunOpts, Session, Sink, SqlDriver};
use crate::assets::{DbConn, DbKind, Family};

type Conn = Client<Compat<TcpStream>>;

pub struct MssqlDriver { cfg: Config, addr: (String, u16), timeout: Duration, read_only: bool, conn: tokio::sync::Mutex<Conn> }

const CELL_MAX: usize = 64 * 1024;

fn config(c: &DbConn, password: Option<&str>, host: &str, port: u16) -> Config {
    let mut cfg = Config::new();
    cfg.host(host);
    cfg.port(port);
    cfg.application_name("ShellAX");
    if !c.database.is_empty() { cfg.database(&c.database); }
    cfg.authentication(AuthMethod::sql_server(&c.user, password.unwrap_or("")));
    match c.tls.as_str() {
        "disable" => cfg.encryption(EncryptionLevel::NotSupported),
        "verify-ca" | "verify-full" => { cfg.encryption(EncryptionLevel::Required); if !c.tls_ca.is_empty() { cfg.trust_cert_ca(&c.tls_ca); } }
        // default / require: encrypt, accept the server's (often self-signed) certificate
        _ => { cfg.encryption(EncryptionLevel::Required); cfg.trust_cert(); }
    }
    cfg
}

async fn dial(cfg: &Config, addr: &(String, u16), timeout: Duration) -> Result<Conn> {
    let tcp = tokio::time::timeout(timeout, TcpStream::connect((addr.0.as_str(), addr.1))).await.context("timeout")??;
    tcp.set_nodelay(true)?;
    Ok(tokio::time::timeout(timeout, Client::connect(cfg.clone(), tcp.compat_write())).await.context("timeout")??)
}

pub async fn connect(c: &DbConn, password: Option<String>, host: String, port: u16) -> Result<MssqlDriver> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cfg = config(c, password.as_deref(), &host, port);
    let addr = (host, port);
    let timeout = c.connect_timeout();
    let conn = dial(&cfg, &addr, timeout).await?;
    Ok(MssqlDriver { cfg, addr, timeout, read_only: c.read_only, conn: tokio::sync::Mutex::new(conn) })
}

fn clip(mut s: String, full: bool) -> String {
    if !full && s.len() > CELL_MAX { let mut i = CELL_MAX; while !s.is_char_boundary(i) { i -= 1; } s.truncate(i); s.push('…'); }
    s
}

/// Display text of one value (`None` = NULL).
pub fn cell(d: &ColumnData<'static>, full: bool) -> Option<String> {
    fn chrono_text<T: for<'a> FromSql<'a> + ToString>(d: &ColumnData<'static>) -> Option<String> { T::from_sql(d).ok().flatten().map(|v| v.to_string()) }
    let s = match d {
        ColumnData::U8(v) => v.map(|x| x.to_string()),
        ColumnData::I16(v) => v.map(|x| x.to_string()),
        ColumnData::I32(v) => v.map(|x| x.to_string()),
        ColumnData::I64(v) => v.map(|x| x.to_string()),
        ColumnData::F32(v) => v.map(|x| x.to_string()),
        ColumnData::F64(v) => v.map(|x| x.to_string()),
        ColumnData::Bit(v) => v.map(|x| if x { "1".into() } else { "0".into() }),
        ColumnData::String(v) => v.as_ref().map(|x| x.to_string()),
        ColumnData::Guid(v) => v.map(|x| x.to_string().to_uppercase()),
        ColumnData::Binary(v) => v.as_ref().map(|b| format!("0x{}", b.iter().map(|x| format!("{x:02X}")).collect::<String>())),
        ColumnData::Numeric(v) => v.map(|x| x.to_string()),
        ColumnData::Xml(v) => v.as_ref().map(|x| x.to_string()),
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => chrono_text::<chrono::NaiveDateTime>(d),
        ColumnData::Date(_) => chrono_text::<chrono::NaiveDate>(d),
        ColumnData::Time(_) => chrono_text::<chrono::NaiveTime>(d),
        ColumnData::DateTimeOffset(_) => chrono_text::<chrono::DateTime<chrono::FixedOffset>>(d),
    };
    s.map(|s| clip(s, full))
}

fn columns(cols: &[tiberius::Column]) -> Vec<ColumnInfo> {
    cols.iter().map(|c| ColumnInfo { name: c.name().to_string(), type_name: format!("{:?}", c.column_type()).to_lowercase(), ..Default::default() }).collect()
}

/// Run `sql` as a batch, collecting every row set (the last statement's error ends it).
async fn batch(c: &mut Conn, sql: &str, max: usize) -> Result<Vec<ResultSet>> {
    let mut out: Vec<ResultSet> = vec![];
    let mut s = c.simple_query(sql).await?;
    while let Some(item) = s.try_next().await? {
        match item {
            QueryItem::Metadata(m) => out.push(ResultSet { sql: sql.to_string(), columns: columns(m.columns()), returns_rows: true, ..Default::default() }),
            QueryItem::Row(r) => {
                if out.is_empty() { out.push(ResultSet { sql: sql.to_string(), columns: columns(r.columns()), returns_rows: true, ..Default::default() }); }
                let rs = out.last_mut().unwrap();
                if rs.rows.len() >= max { rs.truncated = true; continue; }
                rs.rows.push(r.cells().map(|(_, d)| cell(d, false)).collect());
            }
        }
    }
    Ok(out)
}

impl MssqlDriver {
    async fn prepare(&self, c: &mut Conn, db: &str, slot: Option<&CancelSlot>) -> Result<()> {
        if let Some(u) = dialect::use_database(Family::Mssql, db) { c.simple_query(u).await?.into_results().await?; }
        if let Some(sl) = slot {
            let r = c.simple_query("SELECT @@SPID").await?.into_row().await?;
            let id: i16 = r.and_then(|r| r.get(0)).unwrap_or(0);
            sl.bind(Some(Session::Server(id as u64)));
        }
        Ok(())
    }
    /// The session may have dropped (network, KILL): reconnect once.
    async fn lock(&self) -> Result<tokio::sync::MutexGuard<'_, Conn>> {
        let mut g = self.conn.lock().await;
        let alive = match g.simple_query("SELECT 1").await { Ok(s) => s.into_results().await.is_ok(), Err(_) => false };
        if !alive { *g = dial(&self.cfg, &self.addr, self.timeout).await?; }
        Ok(g)
    }
}

impl SqlDriver for MssqlDriver {
    fn kind(&self) -> DbKind { DbKind::Sqlserver }

    fn run<'a>(&'a self, db: &'a str, stmts: Vec<String>, max_rows: usize, opts: &'a RunOpts) -> BoxFut<'a, Vec<ResultSet>> {
        Box::pin(async move {
            let slot = opts.cancel.as_deref();
            let mut c = self.lock().await?;
            self.prepare(&mut c, db, slot).await?;
            if self.read_only && stmts.iter().any(|s| split::classify(s) != split::Class::Read) {
                if let Some(sl) = slot { sl.bind(None); }
                anyhow::bail!(crate::i18n::t("db.err_read_only").to_string());
            }
            if opts.transaction { c.simple_query("BEGIN TRANSACTION").await?.into_results().await?; }
            let mut out = vec![];
            for s in stmts {
                let t0 = Instant::now();
                let r: Result<Vec<ResultSet>> = if split::classify(&s) == split::Class::Read {
                    batch(&mut c, &s, max_rows).await
                } else {
                    match c.execute(s.as_str(), &[]).await {
                        Ok(e) => Ok(vec![ResultSet { sql: s.clone(), affected: e.total(), ..Default::default() }]),
                        Err(e) => Err(e.into()),
                    }
                };
                match r {
                    Ok(mut v) => {
                        if v.is_empty() { v.push(ResultSet { sql: s.clone(), ..Default::default() }); }
                        for x in &mut v { x.elapsed = t0.elapsed(); }
                        out.extend(v);
                    }
                    Err(e) => {
                        let mut msg = e.to_string();
                        if slot.is_some_and(|s| s.requested()) { msg = crate::i18n::t("db.cancelled_by_user").to_string(); }
                        out.push(ResultSet { sql: s, error: Some(msg), elapsed: t0.elapsed(), ..Default::default() });
                        break;
                    }
                }
            }
            if opts.transaction {
                let ok = out.iter().all(|r| r.error.is_none());
                c.simple_query(if ok { "IF @@TRANCOUNT > 0 COMMIT" } else { "IF @@TRANCOUNT > 0 ROLLBACK" }).await?.into_results().await?;
            }
            if let Some(sl) = slot { sl.bind(None); }
            Ok(out)
        })
    }

    fn stream<'a>(&'a self, db: &'a str, sql: String, opts: &'a RunOpts, sink: Sink<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let slot = opts.cancel.as_deref();
            let mut c = self.lock().await?;
            self.prepare(&mut c, db, slot).await?;
            let r: Result<u64> = async {
                let mut n = 0;
                let mut cols = vec![];
                let mut s = c.simple_query(sql).await?;
                while let Some(item) = s.try_next().await? {
                    match item {
                        QueryItem::Metadata(m) => cols = columns(m.columns()),
                        QueryItem::Row(r) => { sink(&cols, r.cells().map(|(_, d)| cell(d, true)).collect())?; n += 1; }
                    }
                }
                Ok(n)
            }.await;
            if let Some(sl) = slot { sl.bind(None); if sl.requested() && r.is_err() { anyhow::bail!(crate::i18n::t("db.cancelled_by_user").to_string()); } }
            r
        })
    }

    fn cancel<'a>(&'a self, _db: &'a str, slot: &'a CancelSlot) -> BoxFut<'a, bool> {
        Box::pin(async move {
            let Some(Session::Server(id)) = slot.with_session(|s| s) else { return Ok(false) };
            let mut c = dial(&self.cfg, &self.addr, self.timeout).await?;
            c.simple_query(format!("KILL {id}")).await?.into_results().await?;
            let _ = c.close().await;
            Ok(true)
        })
    }

    fn close(&self) -> BoxFut<'_, ()> { Box::pin(async move { Ok(()) }) }
}
