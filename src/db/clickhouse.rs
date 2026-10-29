//! ClickHouse driver over the HTTP interface. Results are requested as
//! `TSVWithNamesAndTypes`, so any query shape works without typed row structs; every value
//! arrives as the server formats it. Each request carries a `query_id` so Stop can
//! `KILL QUERY` it from another request.
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{bail, Result};

use super::{split, BoxFut, CancelSlot, ColumnInfo, ResultSet, RunOpts, Session, Sink, SqlDriver};
use crate::assets::{DbConn, DbKind};

pub struct ClickhouseDriver { base: String, user: String, password: String, database: String, read_only: bool, agent: ureq::Agent }

static QID: AtomicU64 = AtomicU64::new(1);
const CELL_MAX: usize = 64 * 1024;

pub async fn connect(c: &DbConn, password: Option<String>, host: String, port: u16) -> Result<ClickhouseDriver> {
    let scheme = if matches!(c.tls.as_str(), "require" | "verify-ca" | "verify-full") { "https" } else { "http" };
    let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host };
    let agent = ureq::AgentBuilder::new().timeout_connect(c.connect_timeout()).build();
    let d = ClickhouseDriver {
        base: format!("{scheme}://{host}:{port}/"),
        user: if c.user.is_empty() { "default".into() } else { c.user.clone() },
        password: password.unwrap_or_default(), database: c.database.clone(), read_only: c.read_only, agent,
    };
    let probe = d.clone_light();
    tokio::task::spawn_blocking(move || probe.request("", "SELECT 1", None, false).map(|_| ())).await??;
    Ok(d)
}

/// Undo TSV escaping; `\N` is NULL.
pub fn unescape(s: &str) -> Option<String> {
    if s == "\\N" { return None; }
    if !s.contains('\\') { return Some(s.to_string()); }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(ch) = it.next() {
        if ch != '\\' { out.push(ch); continue; }
        match it.next() {
            Some('t') => out.push('\t'), Some('n') => out.push('\n'), Some('r') => out.push('\r'), Some('0') => out.push('\0'),
            Some('b') => out.push('\u{8}'), Some('f') => out.push('\u{c}'), Some(o) => out.push(o), None => out.push('\\'),
        }
    }
    Some(out)
}

fn clip(mut s: String) -> String {
    if s.len() > CELL_MAX { let mut i = CELL_MAX; while !s.is_char_boundary(i) { i -= 1; } s.truncate(i); s.push('…'); }
    s
}

/// The statement already names an output format.
fn has_format(sql: &str) -> bool {
    let toks: Vec<String> = sql.split_whitespace().map(|t| t.to_ascii_uppercase()).collect();
    toks.len() >= 2 && toks[toks.len() - 2] == "FORMAT"
}

impl ClickhouseDriver {
    fn clone_light(&self) -> ClickhouseDriver {
        ClickhouseDriver { base: self.base.clone(), user: self.user.clone(), password: self.password.clone(), database: self.database.clone(), read_only: self.read_only, agent: self.agent.clone() }
    }

    fn request(&self, db: &str, sql: &str, qid: Option<u64>, tsv: bool) -> Result<ureq::Response> {
        let db = if db.is_empty() { self.database.as_str() } else { db };
        let mut r = self.agent.post(&self.base).set("X-ClickHouse-User", &self.user).set("X-ClickHouse-Key", &self.password);
        if !db.is_empty() { r = r.query("database", db); }
        if let Some(q) = qid { r = r.query("query_id", &format!("shellax-{q}")); }
        if self.read_only { r = r.query("readonly", "2"); }
        let body = if tsv { format!("{}\nFORMAT TSVWithNamesAndTypes", sql.trim_end().trim_end_matches(';')) } else { sql.to_string() };
        match r.send_string(&body) {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(_, resp)) => bail!(resp.into_string().unwrap_or_default().trim().to_string()),
            Err(e) => Err(e.into()),
        }
    }

    /// Read a TSVWithNamesAndTypes body; `each` gets every row (full values).
    fn read_tsv(resp: ureq::Response, mut each: impl FnMut(&[ColumnInfo], Vec<Option<String>>) -> Result<bool>) -> Result<Vec<ColumnInfo>> {
        let mut lines = BufReader::new(resp.into_reader()).lines();
        let names: Vec<String> = match lines.next() { Some(l) => l?.split('\t').map(|s| unescape(s).unwrap_or_default()).collect(), None => return Ok(vec![]) };
        let types: Vec<String> = match lines.next() { Some(l) => l?.split('\t').map(|s| unescape(s).unwrap_or_default()).collect(), None => vec![] };
        let cols: Vec<ColumnInfo> = names.into_iter().enumerate().map(|(i, n)| { let t = types.get(i).cloned().unwrap_or_default(); ColumnInfo { name: n, nullable: Some(t.starts_with("Nullable(")), type_name: t, ..Default::default() } }).collect();
        for l in lines {
            let l = l?;
            if !each(&cols, l.split('\t').map(unescape).collect())? { break; }
        }
        Ok(cols)
    }

    fn run_one(&self, db: &str, sql: &str, max: usize, qid: u64) -> ResultSet {
        let t0 = Instant::now();
        let reads = split::classify(sql) == split::Class::Read && !has_format(sql);
        let r: Result<ResultSet> = (|| {
            if reads {
                let resp = self.request(db, sql, Some(qid), true)?;
                let mut rows = vec![];
                let mut truncated = false;
                let columns = Self::read_tsv(resp, |_, row| {
                    if rows.len() >= max { truncated = true; return Ok(false); }
                    rows.push(row.into_iter().map(|c| c.map(clip)).collect());
                    Ok(true)
                })?;
                Ok(ResultSet { sql: sql.into(), columns, rows, truncated, returns_rows: true, ..Default::default() })
            } else {
                let resp = self.request(db, sql, Some(qid), false)?;
                let affected = resp.header("X-ClickHouse-Summary").and_then(|h| serde_json::from_str::<serde_json::Value>(h).ok()).and_then(|v| v["written_rows"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0);
                let text = resp.into_string().unwrap_or_default();
                if text.trim().is_empty() { return Ok(ResultSet { sql: sql.into(), affected, ..Default::default() }); }
                // statements with their own FORMAT, or SHOW/EXPLAIN-like output: one text column
                let rows = text.lines().map(|l| vec![Some(clip(l.to_string()))]).collect();
                Ok(ResultSet { sql: sql.into(), columns: vec![ColumnInfo { name: "result".into(), type_name: "String".into(), ..Default::default() }], rows, returns_rows: true, affected, ..Default::default() })
            }
        })();
        let mut rs = r.unwrap_or_else(|e| ResultSet { sql: sql.into(), error: Some(e.to_string()), ..Default::default() });
        rs.elapsed = t0.elapsed();
        rs
    }
}

impl SqlDriver for ClickhouseDriver {
    fn kind(&self) -> DbKind { DbKind::Clickhouse }

    fn run<'a>(&'a self, db: &'a str, stmts: Vec<String>, max_rows: usize, opts: &'a RunOpts) -> BoxFut<'a, Vec<ResultSet>> {
        Box::pin(async move {
            // ClickHouse has no multi-statement transactions: `transaction` runs in order and stops at the first error
            let me = self.clone_light();
            let db = db.to_string();
            let qid = QID.fetch_add(1, Ordering::Relaxed);
            let slot = opts.cancel.clone();
            if let Some(s) = &slot { s.bind(Some(Session::Server(qid))); }
            let out = tokio::task::spawn_blocking(move || {
                let mut out = vec![];
                for s in stmts {
                    let r = me.run_one(&db, &s, max_rows, qid);
                    let failed = r.error.is_some();
                    out.push(r);
                    if failed { break; }
                }
                out
            }).await?;
            let mut out = out;
            if let Some(s) = &slot {
                s.bind(None);
                if s.requested() { if let Some(l) = out.last_mut() { if l.error.is_some() { l.error = Some(crate::i18n::t("db.cancelled_by_user").to_string()); } } }
            }
            Ok(out)
        })
    }

    fn stream<'a>(&'a self, db: &'a str, sql: String, opts: &'a RunOpts, sink: Sink<'a>) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let me = self.clone_light();
            let db = db.to_string();
            let qid = QID.fetch_add(1, Ordering::Relaxed);
            if let Some(s) = &opts.cancel { s.bind(Some(Session::Server(qid))); }
            // rows cross from the blocking reader to the async sink through a bounded channel
            let (tx, rx) = flume::bounded::<(Vec<ColumnInfo>, Vec<Option<String>>)>(256);
            let h = tokio::task::spawn_blocking(move || -> Result<()> {
                let resp = me.request(&db, &sql, Some(qid), true)?;
                ClickhouseDriver::read_tsv(resp, |c, row| Ok(tx.send((c.to_vec(), row)).is_ok()))?;
                Ok(())
            });
            let mut n = 0;
            let mut err = None;
            while let Ok((c, row)) = rx.recv_async().await {
                if let Err(e) = sink(&c, row) { err = Some(e); break; }
                n += 1;
            }
            drop(rx);
            let r = h.await?;
            if let Some(s) = &opts.cancel { s.bind(None); if s.requested() { bail!(crate::i18n::t("db.cancelled_by_user").to_string()); } }
            if let Some(e) = err { return Err(e); }
            r?;
            Ok(n)
        })
    }

    fn cancel<'a>(&'a self, _db: &'a str, slot: &'a CancelSlot) -> BoxFut<'a, bool> {
        Box::pin(async move {
            let Some(Session::Server(id)) = slot.with_session(|s| s) else { return Ok(false) };
            let me = self.clone_light();
            tokio::task::spawn_blocking(move || me.request("", &format!("KILL QUERY WHERE query_id = 'shellax-{id}' ASYNC"), None, false).map(|_| ())).await??;
            Ok(true)
        })
    }

    fn close(&self) -> BoxFut<'_, ()> { Box::pin(async move { Ok(()) }) }
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tsv() {
        assert_eq!(unescape("\\N"), None);
        assert_eq!(unescape("a\\tb\\nc\\\\d").as_deref(), Some("a\tb\nc\\d"));
        assert!(has_format("select 1 format JSON"));
        assert!(!has_format("select format"));
    }
}
