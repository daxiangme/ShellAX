//! DuckDB driver (embedded, optional `duckdb` build feature). One connection guarded by a
//! mutex; statements run on a blocking thread. Stop uses DuckDB's interrupt handle.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use duckdb::types::{TimeUnit, Value};
use duckdb::{AccessMode, Config, Connection};

use super::{split, BoxFut, CancelSlot, ColumnInfo, ResultSet, RunOpts, Session, Sink, SqlDriver};
use crate::assets::{DbConn, DbKind};

pub struct DuckDriver { conn: Arc<Mutex<Connection>>, interrupt: Arc<duckdb::InterruptHandle> }

const CELL_MAX: usize = 64 * 1024;

pub async fn connect(c: &DbConn) -> Result<DuckDriver> {
    anyhow::ensure!(!c.file.is_empty(), "no database file");
    let (file, ro) = (c.file.clone(), c.read_only);
    let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
        let cfg = Config::default().access_mode(if ro { AccessMode::ReadOnly } else { AccessMode::ReadWrite })?;
        Ok(Connection::open_with_flags(&file, cfg)?)
    }).await??;
    let interrupt = conn.interrupt_handle();
    Ok(DuckDriver { conn: Arc::new(Mutex::new(conn)), interrupt })
}

fn micros_text(us: i64) -> String {
    chrono::DateTime::from_timestamp_micros(us).map(|t| { let n = t.naive_utc(); if us % 1_000_000 == 0 { n.format("%Y-%m-%d %H:%M:%S").to_string() } else { n.format("%Y-%m-%d %H:%M:%S%.6f").to_string() } }).unwrap_or_else(|| us.to_string())
}

/// Display text of one value (`None` = NULL).
pub fn value_text(v: &Value) -> Option<String> {
    Some(match v {
        Value::Null => return None,
        Value::Boolean(b) => b.to_string(),
        Value::TinyInt(x) => x.to_string(), Value::SmallInt(x) => x.to_string(), Value::Int(x) => x.to_string(), Value::BigInt(x) => x.to_string(),
        Value::HugeInt(x) => x.to_string(), Value::UHugeInt(x) => x.to_string(),
        Value::UTinyInt(x) => x.to_string(), Value::USmallInt(x) => x.to_string(), Value::UInt(x) => x.to_string(), Value::UBigInt(x) => x.to_string(),
        Value::Float(x) => x.to_string(), Value::Double(x) => x.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::Timestamp(u, x) => micros_text(u.to_micros(*x)),
        Value::Text(s) | Value::Enum(s) => s.clone(),
        Value::Blob(b) | Value::Geometry(b) => format!("\\x{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
        Value::Date32(d) => (chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap() + chrono::Duration::days(*d as i64)).to_string(),
        Value::Time64(u, x) => { let us = TimeUnit::to_micros(u, *x); chrono::NaiveTime::from_num_seconds_from_midnight_opt((us / 1_000_000) as u32, ((us % 1_000_000) * 1000) as u32).map(|t| t.to_string()).unwrap_or_default() }
        Value::List(l) | Value::Array(l) => format!("[{}]", l.iter().map(|x| value_text(x).unwrap_or_else(|| "NULL".into())).collect::<Vec<_>>().join(", ")),
        other => format!("{other:?}"),
    })
}

fn clip(mut s: String, full: bool) -> String {
    if !full && s.len() > CELL_MAX { let mut i = CELL_MAX; while !s.is_char_boundary(i) { i -= 1; } s.truncate(i); s.push('…'); }
    s
}

/// Run one query, feeding rows to `each` (returns false to stop). Returns the columns.
fn query(c: &Connection, sql: &str, full: bool, mut each: impl FnMut(&[ColumnInfo], Vec<Option<String>>) -> Result<bool>) -> Result<Vec<ColumnInfo>> {
    let mut st = c.prepare(sql)?;
    let mut rows = st.query([])?;
    let mut cols: Vec<ColumnInfo> = vec![];
    while let Some(r) = rows.next()? {
        if cols.is_empty() {
            let s = r.as_ref();
            cols = (0..s.column_count()).map(|i| ColumnInfo { name: s.column_name(i).map(|x| x.to_string()).unwrap_or_default(), type_name: format!("{:?}", s.column_type(i)).to_lowercase(), ..Default::default() }).collect();
        }
        let vals: Vec<Option<String>> = (0..cols.len()).map(|i| r.get::<_, Value>(i).ok().and_then(|v| value_text(&v)).map(|s| clip(s, full))).collect();
        if !each(&cols, vals)? { break; }
    }
    drop(rows);
    if cols.is_empty() {
        cols = st.column_names().into_iter().map(|n| ColumnInfo { name: n, ..Default::default() }).collect();
    }
    Ok(cols)
}

fn run_one(c: &Connection, sql: &str, max: usize) -> ResultSet {
    let t0 = Instant::now();
    let r: Result<ResultSet> = if split::classify(sql) == split::Class::Read {
        let mut rows = vec![];
        let mut truncated = false;
        query(c, sql, false, |_, row| { if rows.len() >= max { truncated = true; return Ok(false); } rows.push(row); Ok(true) })
            .map(|columns| ResultSet { sql: sql.into(), columns, rows, truncated, returns_rows: true, ..Default::default() })
    } else {
        c.execute(sql, []).map(|n| ResultSet { sql: sql.into(), affected: n as u64, ..Default::default() }).map_err(Into::into)
    };
    let mut rs = r.unwrap_or_else(|e| ResultSet { sql: sql.into(), error: Some(e.to_string()), ..Default::default() });
    rs.elapsed = t0.elapsed();
    rs
}

impl SqlDriver for DuckDriver {
    fn kind(&self) -> DbKind { DbKind::Duckdb }

    fn run<'a>(&'a self, _db: &'a str, stmts: Vec<String>, max_rows: usize, opts: &'a RunOpts) -> BoxFut<'a, Vec<ResultSet>> {
        Box::pin(async move {
            let conn = self.conn.clone();
            let tx = opts.transaction;
            let slot = opts.cancel.clone();
            if let Some(s) = &slot { s.bind(Some(Session::Sqlite(0))); }
            let mut out = tokio::task::spawn_blocking(move || -> Result<Vec<ResultSet>> {
                let c = conn.lock().unwrap();
                if tx { c.execute_batch("BEGIN TRANSACTION")?; }
                let mut out = vec![];
                for s in stmts { let r = run_one(&c, &s, max_rows); let failed = r.error.is_some(); out.push(r); if failed { break; } }
                if tx { c.execute_batch(if out.iter().all(|r| r.error.is_none()) { "COMMIT" } else { "ROLLBACK" })?; }
                Ok(out)
            }).await??;
            if let Some(s) = &slot {
                s.bind(None);
                if s.requested() { if let Some(l) = out.last_mut() { if l.error.is_some() { l.error = Some(crate::i18n::t("db.cancelled_by_user").to_string()); } } }
            }
            Ok(out)
        })
    }

    fn stream<'a>(&'a self, _db: &'a str, sql: String, opts: &'a RunOpts, sink: Sink<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let conn = self.conn.clone();
            if let Some(s) = &opts.cancel { s.bind(Some(Session::Sqlite(0))); }
            let (tx, rx) = flume::bounded::<(Vec<ColumnInfo>, Vec<Option<String>>)>(256);
            let h = tokio::task::spawn_blocking(move || -> Result<()> {
                let c = conn.lock().unwrap();
                query(&c, &sql, true, |cols, row| Ok(tx.send((cols.to_vec(), row)).is_ok()))?;
                Ok(())
            });
            let (mut n, mut err) = (0, None);
            while let Ok((c, row)) = rx.recv_async().await { if let Err(e) = sink(&c, row) { err = Some(e); break; } n += 1; }
            drop(rx);
            let r = h.await?;
            if let Some(s) = &opts.cancel { s.bind(None); if s.requested() { anyhow::bail!(crate::i18n::t("db.cancelled_by_user").to_string()); } }
            if let Some(e) = err { return Err(e); }
            r?;
            Ok(n)
        })
    }

    fn cancel<'a>(&'a self, _db: &'a str, slot: &'a CancelSlot) -> BoxFut<'a, bool> {
        Box::pin(async move {
            Ok(slot.with_session(|s| { if s.is_some() { self.interrupt.interrupt(); true } else { false } }))
        })
    }

    fn close(&self) -> BoxFut<'_, ()> { Box::pin(async move { Ok(()) }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckdb_end_to_end() {
        let dir = std::env::temp_dir().join(format!("sx-duck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let c = DbConn { kind: DbKind::Duckdb, file: dir.join("t.duckdb").to_string_lossy().into(), ..Default::default() };
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            let d: super::super::Driver = Arc::new(connect(&c).await.unwrap());
            assert!(d.server_version().await.unwrap().starts_with('v'));
            let r = d.execute("", "CREATE TABLE t (id INTEGER PRIMARY KEY, name VARCHAR NOT NULL, ts TIMESTAMP, d DATE, l INTEGER[]); INSERT INTO t VALUES (1, '中文', '2024-01-02 03:04:05', '2024-01-02', [1,2]), (2, 'b', NULL, NULL, NULL)", false).await.unwrap();
            assert!(r.iter().all(|x| x.error.is_none()), "{r:?}");
            assert_eq!(r[1].affected, 2);
            let q = d.execute("", "SELECT * FROM t ORDER BY id", false).await.unwrap();
            assert_eq!(q[0].rows[0], vec![Some("1".into()), Some("中文".into()), Some("2024-01-02 03:04:05".into()), Some("2024-01-02".into()), Some("[1, 2]".into())]);
            assert_eq!(q[0].rows[1][2], None);
            assert!(d.schemas("").await.unwrap().contains(&"main".to_string()));
            assert_eq!(d.tables("", Some("main")).await.unwrap()[0].name, "t");
            let cols = d.columns("", Some("main"), "t").await.unwrap();
            assert!(cols[0].primary && cols[1].nullable == Some(false));
            let (page, total) = d.page("", Some("main"), "t", "", Some(("id", true)), 0).await.unwrap();
            assert_eq!((page.rows[0][0].clone(), total), (Some("2".into()), Some(2)));
            assert!(d.ddl("", Some("main"), "t").await.unwrap().starts_with("CREATE TABLE"));
            assert!(d.apply("", None, vec!["DELETE FROM t WHERE id = 2".into(), "INSERT INTO t VALUES (1, 'dup', NULL, NULL, NULL)".into()]).await.is_err());
            assert_eq!(d.execute("", "SELECT count(*) FROM t", false).await.unwrap()[0].rows[0][0].as_deref(), Some("2"));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
