//! sqlx-backed driver for the MySQL, PostgreSQL and SQLite families. Statements are sent
//! as plain text (simple / text protocol) so every value can be shown as the server
//! formats it, independent of column type.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::TryStreamExt;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow, MySqlSslMode};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow, PgSslMode};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{AssertSqlSafe, Column, Connection, Either, Row, TypeInfo, ValueRef};

use super::{dialect, split, BoxFut, Sink, CancelSlot, ColumnInfo, Driver, ResultSet, RunOpts, Session, SqlDriver};
use crate::assets::{DbConn, DbKind, Family};

const POOL_MAX: u32 = 4;
/// Longest cell text kept (the grid shows a preview; values are display strings).
const CELL_MAX: usize = 64 * 1024;

enum Pools {
    My(MySqlPool, MySqlConnectOptions),
    Pg { opts: PgConnectOptions, default_db: String, pools: tokio::sync::Mutex<HashMap<String, PgPool>>, timeout: Duration },
    Lite(SqlitePool),
}

pub struct SqlxDriver { kind: DbKind, pools: Pools }

/// Open a pool and prove it with one connection. `endpoint` overrides host/port (local end
/// of an SSH tunnel).
pub async fn connect(c: &DbConn, password: Option<String>, endpoint: Option<(String, u16)>) -> Result<Driver> {
    let (host, port) = endpoint.unwrap_or_else(|| (c.host.clone(), c.effective_port()));
    let timeout = c.connect_timeout();
    let pools = match c.kind.family() {
        Family::Mysql => {
            let mut o = MySqlConnectOptions::new().host(&host).port(port).username(&c.user).charset("utf8mb4");
            if let Some(p) = &password { o = o.password(p); }
            if !c.database.is_empty() { o = o.database(&c.database); }
            o = o.ssl_mode(match c.tls.as_str() { "disable" => MySqlSslMode::Disabled, "require" => MySqlSslMode::Required, "verify-ca" => MySqlSslMode::VerifyCa, "verify-full" => MySqlSslMode::VerifyIdentity, _ => MySqlSslMode::Preferred });
            if !c.tls_ca.is_empty() { o = o.ssl_ca(&c.tls_ca); }
            let ro = c.read_only;
            let o2 = o.clone();
            let pool = MySqlPoolOptions::new().max_connections(POOL_MAX).acquire_timeout(timeout)
                .after_connect(move |conn, _| Box::pin(async move {
                    if ro { sqlx::raw_sql("SET SESSION TRANSACTION READ ONLY").execute(&mut *conn).await?; }
                    Ok(())
                }))
                .connect_with(o);
            Pools::My(tokio::time::timeout(timeout, pool).await.context("timeout")??, o2)
        }
        Family::Postgres => {
            let mut o = PgConnectOptions::new_without_pgpass().host(&host).port(port).username(&c.user).application_name("ShellAX");
            if let Some(p) = &password { o = o.password(p); }
            o = o.ssl_mode(match c.tls.as_str() { "disable" => PgSslMode::Disable, "require" => PgSslMode::Require, "verify-ca" => PgSslMode::VerifyCa, "verify-full" => PgSslMode::VerifyFull, _ => PgSslMode::Prefer });
            if !c.tls_ca.is_empty() { o = o.ssl_root_cert(&c.tls_ca); }
            if c.read_only { o = o.options([("default_transaction_read_only", "on")]); }
            let default_db = if c.database.is_empty() { "postgres".to_string() } else { c.database.clone() };
            let pool = tokio::time::timeout(timeout, pg_pool(&o, &default_db, timeout)).await.context("timeout")??;
            let mut m = HashMap::new();
            m.insert(default_db.clone(), pool);
            Pools::Pg { opts: o, default_db, pools: tokio::sync::Mutex::new(m), timeout }
        }
        Family::Sqlite => {
            anyhow::ensure!(!c.file.is_empty(), "no database file");
            let o = SqliteConnectOptions::new().filename(&c.file).read_only(c.read_only).create_if_missing(false).busy_timeout(Duration::from_secs(5));
            Pools::Lite(SqlitePoolOptions::new().max_connections(2).acquire_timeout(timeout).connect_with(o).await?)
        }
        Family::Other => anyhow::bail!("{} driver is not available yet", c.kind.label()),
    };
    Ok(Arc::new(SqlxDriver { kind: c.kind, pools }))
}

async fn pg_pool(o: &PgConnectOptions, db: &str, timeout: Duration) -> Result<PgPool> {
    Ok(PgPoolOptions::new().max_connections(POOL_MAX).acquire_timeout(timeout).connect_with(o.clone().database(db)).await?)
}

fn clip(mut s: String, full: bool) -> String {
    if !full && s.len() > CELL_MAX { let mut e = CELL_MAX; while !s.is_char_boundary(e) { e -= 1; } s.truncate(e); s.push('…'); }
    s
}
/// Binary values are shown as `0x…` hex (a preview of 64 bytes unless `full`).
fn bytes_text(b: &[u8], ty: &str, full: bool) -> String {
    let binary = ty.contains("BLOB") || ty.contains("BINARY") || ty.contains("BYTEA");
    match std::str::from_utf8(b) {
        Ok(s) if !binary || !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r') => clip(s.to_string(), full),
        _ => {
            let hex: String = b.iter().take(if full { usize::MAX } else { 64 }).map(|x| format!("{x:02X}")).collect();
            format!("0x{hex}{}", if !full && b.len() > 64 { format!("… ({} B)", b.len()) } else { String::new() })
        }
    }
}

fn my_cell(r: &MySqlRow, i: usize, full: bool) -> Option<String> {
    let raw = r.try_get_raw(i).ok()?;
    if raw.is_null() { return None; }
    let ty = r.columns()[i].type_info().name().to_ascii_uppercase();
    let b: Vec<u8> = r.try_get_unchecked(i).ok()?;
    Some(bytes_text(&b, &ty, full))
}
fn pg_cell(r: &PgRow, i: usize, full: bool) -> Option<String> {
    let raw = r.try_get_raw(i).ok()?;
    if raw.is_null() { return None; }
    match raw.as_str() { Ok(s) => Some(clip(s.to_string(), full)), Err(_) => raw.as_bytes().ok().map(|b| bytes_text(b, "BYTEA", full)) }
}
fn lite_cell(r: &SqliteRow, i: usize, full: bool) -> Option<String> {
    let raw = r.try_get_raw(i).ok()?;
    if raw.is_null() { return None; }
    let ty = raw.type_info().name().to_ascii_uppercase();
    match ty.as_str() {
        "INTEGER" => r.try_get_unchecked::<i64, _>(i).ok().map(|v| v.to_string()),
        "REAL" => r.try_get_unchecked::<f64, _>(i).ok().map(|v| v.to_string()),
        "BLOB" => r.try_get_unchecked::<Vec<u8>, _>(i).ok().map(|b| bytes_text(&b, "BLOB", full)),
        _ => r.try_get_unchecked::<String, _>(i).ok().map(|s| clip(s, full)),
    }
}

fn pg_search_path(schema: Option<&str>) -> String {
    match schema.filter(|s| !s.is_empty()) { Some(s) => format!("SET search_path TO {}, public", dialect::quote_ident(Family::Postgres, s)), None => "SET search_path TO DEFAULT".into() }
}

fn columns_of<R: Row>(r: &R) -> Vec<ColumnInfo> {
    r.columns().iter().map(|c| ColumnInfo { name: c.name().to_string(), type_name: c.type_info().name().to_string(), ..Default::default() }).collect()
}

/// Run one statement on an open connection, collecting rows up to `max`.
macro_rules! run_one {
    ($conn:expr, $sql:expr, $max:expr, $cell:path) => {{
        let sql: String = $sql;
        let started = Instant::now();
        let mut rs = ResultSet { returns_rows: split::returns_rows(&sql), sql: sql.clone(), ..Default::default() };
        let res: Result<(), sqlx::Error> = async {
            let mut s = sqlx::raw_sql(AssertSqlSafe(sql)).fetch_many(&mut *$conn);
            while let Some(item) = s.try_next().await? {
                match item {
                    Either::Left(q) => rs.affected += q.rows_affected(),
                    Either::Right(row) => {
                        if rs.columns.is_empty() { rs.columns = columns_of(&row); rs.returns_rows = true; }
                        if rs.rows.len() >= $max { rs.truncated = true; break; }
                        rs.rows.push((0..row.columns().len()).map(|i| $cell(&row, i, false)).collect());
                    }
                }
            }
            Ok(())
        }.await;
        rs.elapsed = started.elapsed();
        if let Err(e) = res { rs.error = Some(e.to_string()); }
        rs
    }};
}

macro_rules! run_all {
    ($conn:expr, $stmts:expr, $max:expr, $cell:path, $opts:expr, $begin:expr) => {{
        let mut out = Vec::new();
        if $opts.transaction { sqlx::raw_sql($begin).execute(&mut *$conn).await?; }
        for s in $stmts {
            let r = run_one!($conn, s, $max, $cell);
            let failed = r.error.is_some();
            out.push(r);
            if failed { break; }
        }
        if let Some(c) = &$opts.cancel { if c.requested() { if let Some(last) = out.last_mut() { if last.error.is_some() { last.error = Some(crate::i18n::t("db.cancelled_by_user").to_string()); } } } }
        if $opts.transaction {
            let ok = out.iter().all(|r: &ResultSet| r.error.is_none());
            sqlx::raw_sql(if ok { "COMMIT" } else { "ROLLBACK" }).execute(&mut *$conn).await?;
        }
        out
    }};
}

/// Stream one query's rows into `sink` (full cell values), returning the row count.
macro_rules! stream_one {
    ($conn:expr, $sql:expr, $cell:path, $sink:expr) => {{
        let mut n = 0u64;
        let mut cols: Vec<ColumnInfo> = vec![];
        let mut s = sqlx::raw_sql(AssertSqlSafe($sql)).fetch_many(&mut *$conn);
        while let Some(item) = s.try_next().await? {
            if let Either::Right(row) = item {
                if cols.is_empty() { cols = columns_of(&row); }
                $sink(&cols, (0..row.columns().len()).map(|i| $cell(&row, i, true)).collect())?;
                n += 1;
            }
        }
        n
    }};
}

impl SqlxDriver {
    async fn pg_for(&self, db: &str) -> Result<PgPool> {
        let Pools::Pg { opts, default_db, pools, timeout } = &self.pools else { unreachable!() };
        let db = if db.is_empty() { default_db.as_str() } else { db };
        let mut m = pools.lock().await;
        if let Some(p) = m.get(db) { return Ok(p.clone()); }
        let p = pg_pool(opts, db, *timeout).await?;
        m.insert(db.to_string(), p.clone());
        Ok(p)
    }
}

impl SqlDriver for SqlxDriver {
    fn kind(&self) -> DbKind { self.kind }

    fn run<'a>(&'a self, db: &'a str, stmts: Vec<String>, max_rows: usize, opts: &'a RunOpts) -> BoxFut<'a, Vec<ResultSet>> {
        Box::pin(async move {
            let slot = opts.cancel.as_deref();
            let out = match &self.pools {
                Pools::My(p, _) => {
                    let mut c = p.acquire().await?;
                    if let Some(u) = dialect::use_database(Family::Mysql, db) { sqlx::raw_sql(AssertSqlSafe(u)).execute(&mut *c).await?; }
                    if let Some(sl) = slot { let id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()").fetch_one(&mut *c).await?; sl.bind(Some(Session::Server(id))); }
                    let r = run_all!(c, stmts, max_rows, my_cell, opts, "START TRANSACTION");
                    r
                }
                Pools::Pg { .. } => {
                    let p = self.pg_for(db).await?;
                    let mut c = p.acquire().await?;
                    // pooled sessions are reused: always (re)set the search path
                    sqlx::raw_sql(AssertSqlSafe(pg_search_path(opts.schema.as_deref()))).execute(&mut *c).await?;
                    if let Some(sl) = slot { let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *c).await?; sl.bind(Some(Session::Server(pid as u64))); }
                    let r = run_all!(c, stmts, max_rows, pg_cell, opts, "BEGIN");
                    r
                }
                Pools::Lite(p) => {
                    let mut c = p.acquire().await?;
                    if let Some(sl) = slot { let h = c.lock_handle().await?.as_raw_handle(); sl.bind(Some(Session::Sqlite(h.as_ptr() as usize))); }
                    let r = run_all!(c, stmts, max_rows, lite_cell, opts, "BEGIN");
                    r
                }
            };
            if let Some(sl) = slot { sl.bind(None); }
            Ok(out)
        })
    }

    fn stream<'a>(&'a self, db: &'a str, sql: String, opts: &'a RunOpts, sink: Sink<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let slot = opts.cancel.as_deref();
            let r: Result<u64> = async {
                Ok(match &self.pools {
                    Pools::My(p, _) => {
                        let mut c = p.acquire().await?;
                        if let Some(u) = dialect::use_database(Family::Mysql, db) { sqlx::raw_sql(AssertSqlSafe(u)).execute(&mut *c).await?; }
                        if let Some(sl) = slot { let id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()").fetch_one(&mut *c).await?; sl.bind(Some(Session::Server(id))); }
                        stream_one!(c, sql, my_cell, sink)
                    }
                    Pools::Pg { .. } => {
                        let p = self.pg_for(db).await?;
                        let mut c = p.acquire().await?;
                        sqlx::raw_sql(AssertSqlSafe(pg_search_path(opts.schema.as_deref()))).execute(&mut *c).await?;
                        if let Some(sl) = slot { let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *c).await?; sl.bind(Some(Session::Server(pid as u64))); }
                        stream_one!(c, sql, pg_cell, sink)
                    }
                    Pools::Lite(p) => {
                        let mut c = p.acquire().await?;
                        if let Some(sl) = slot { let h = c.lock_handle().await?.as_raw_handle(); sl.bind(Some(Session::Sqlite(h.as_ptr() as usize))); }
                        stream_one!(c, sql, lite_cell, sink)
                    }
                })
            }.await;
            if let Some(sl) = slot { sl.bind(None); if sl.requested() && r.is_err() { anyhow::bail!(crate::i18n::t("db.cancelled_by_user").to_string()); } }
            r
        })
    }

    fn cancel<'a>(&'a self, db: &'a str, slot: &'a CancelSlot) -> BoxFut<'a, bool> {
        Box::pin(async move {
            // SQLite: interrupt in place, under the slot lock (the handle stays valid)
            if let Pools::Lite(_) = &self.pools {
                return Ok(slot.with_session(|s| match s {
                    Some(Session::Sqlite(h)) => { unsafe { libsqlite3_sys::sqlite3_interrupt(h as *mut libsqlite3_sys::sqlite3) }; true }
                    _ => false,
                }));
            }
            let Some(Session::Server(id)) = slot.with_session(|s| s) else { return Ok(false) };
            match &self.pools {
                Pools::My(_, o) => {
                    let mut c = sqlx::mysql::MySqlConnection::connect_with(o).await?;
                    sqlx::raw_sql(AssertSqlSafe(format!("KILL QUERY {id}"))).execute(&mut c).await?;
                    let _ = c.close().await;
                }
                Pools::Pg { opts, default_db, .. } => {
                    let db = if db.is_empty() { default_db.as_str() } else { db };
                    let mut c = sqlx::postgres::PgConnection::connect_with(&opts.clone().database(db)).await?;
                    sqlx::raw_sql(AssertSqlSafe(format!("SELECT pg_cancel_backend({id})"))).execute(&mut c).await?;
                    let _ = c.close().await;
                }
                Pools::Lite(_) => {}
            }
            Ok(true)
        })
    }

    fn close(&self) -> BoxFut<'_, ()> {
        Box::pin(async move {
            match &self.pools {
                Pools::My(p, _) => p.close().await,
                Pools::Pg { pools, .. } => { for p in pools.lock().await.values() { p.close().await; } }
                Pools::Lite(p) => p.close().await,
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_end_to_end() {
        let dir = std::env::temp_dir().join(format!("shellax-db-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.db");
        let _ = std::fs::remove_file(&file);
        rusqlite_free_create(&file);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let c = DbConn { kind: DbKind::Sqlite, file: file.to_string_lossy().into(), ..Default::default() };
            let d = connect(&c, None, None).await.unwrap();
            let r = d.execute("main", "create table t(id integer primary key, name text, v real, b blob); insert into t(name, v, b) values ('a', 1.5, x'00ff'), (null, 2, null); select * from t order by id", false).await.unwrap();
            assert_eq!(r.len(), 3);
            assert_eq!(r[1].affected, 2);
            let s = &r[2];
            assert_eq!(s.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["id", "name", "v", "b"]);
            assert_eq!(s.rows[0], vec![Some("1".into()), Some("a".into()), Some("1.5".into()), Some("0x00FF".into())]);
            assert_eq!(s.rows[1][1], None);
            let tables = d.tables("main", None).await.unwrap();
            assert_eq!(tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["t"]);
            let cols = d.columns("main", None, "t").await.unwrap();
            assert!(cols[0].primary && cols[0].name == "id" && cols[1].nullable == Some(true));
            let (page, total) = d.page("main", None, "t", "", Some(("id", true)), 0).await.unwrap();
            assert_eq!((page.rows.len(), total), (2, Some(2)));
            assert_eq!(page.rows[0][0].as_deref(), Some("2"));
            let e = d.execute("main", "select * from nope; select 1", false).await.unwrap();
            assert_eq!(e.len(), 1);
            assert!(e[0].error.is_some());
            assert!(d.execute("main", "delete from t", true).await.is_err());
            assert!(!d.server_version().await.unwrap().is_empty());
            d.close().await.unwrap();
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Create an empty database file (connect refuses to create one).
    fn rusqlite_free_create(p: &std::path::Path) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let o = SqliteConnectOptions::new().filename(p).create_if_missing(true);
            let pool = SqlitePool::connect_with(o).await.unwrap();
            pool.close().await;
        });
    }
}
