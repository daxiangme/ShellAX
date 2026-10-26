//! Database access layer (v0.2 PRD §6): one [`SqlDriver`] trait over the engines, result
//! sets as display strings, introspection built on top of plain queries. The UI never
//! talks to a driver crate directly.
pub mod dialect;
pub mod driver;
pub mod live;
pub mod split;
pub mod edit;
pub mod export;
pub mod lex;
pub mod complete;
pub mod history;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use crate::assets::{DbKind, Family};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Rows kept per result set; the grid pages tables itself, ad-hoc queries are capped.
pub const MAX_ROWS: usize = 10_000;
/// Table browse page size.
pub const PAGE: usize = 500;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub nullable: Option<bool>,
    pub primary: bool,
    pub default: Option<String>,
    pub comment: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    pub sql: String,
    pub columns: Vec<ColumnInfo>,
    /// `None` = SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: u64,
    pub returns_rows: bool,
    /// More rows existed than were kept.
    pub truncated: bool,
    pub elapsed: Duration,
    pub error: Option<String>,
}

impl ResultSet {
    pub fn cell(&self, row: usize, col: usize) -> Option<&str> { self.rows.get(row)?.get(col)?.as_deref() }
    fn first(&self) -> Option<&str> { self.cell(0, 0) }
    fn col(&self, i: usize) -> impl Iterator<Item = String> + '_ { self.rows.iter().map(move |r| r.get(i).cloned().flatten().unwrap_or_default()) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjKind { Table, View }

#[derive(Debug, Clone, PartialEq)]
pub struct TableInfo { pub name: String, pub kind: ObjKind }

/// Server-side identity of the session a statement runs on, kept while it runs so another
/// session can stop it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Session { Server(u64), /** raw `sqlite3*` (interrupt is thread-safe) */ Sqlite(usize) }

/// Shared between a running request and the UI's Stop button.
#[derive(Default, Debug)]
pub struct CancelSlot { session: Mutex<Option<Session>>, requested: AtomicBool }

impl CancelSlot {
    pub fn requested(&self) -> bool { self.requested.load(Ordering::Acquire) }
    /// The driver sets this right after acquiring a connection and clears it before release.
    pub fn bind(&self, s: Option<Session>) { *self.session.lock().unwrap() = s; }
    /// Marks the request cancelled and runs `f` with the live session (if any) under the lock,
    /// so the session cannot be released meanwhile.
    pub fn with_session<T>(&self, f: impl FnOnce(Option<Session>) -> T) -> T {
        self.requested.store(true, Ordering::Release);
        let g = self.session.lock().unwrap();
        f(*g)
    }
}

/// Per-request options.
#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    /// PostgreSQL: `search_path` for the request (falls back to the server default).
    pub schema: Option<String>,
    /// Wrap the statements in one transaction: all commit or all roll back.
    pub transaction: bool,
    pub cancel: Option<Arc<CancelSlot>>,
}

/// Engine adapter. Only batch execution is engine specific; everything else is plain SQL
/// from [`dialect`] run through it.
pub trait SqlDriver: Send + Sync {
    fn kind(&self) -> DbKind;
    /// Run statements in order on one session with `db` as default database. Stops at the
    /// first failing statement (its result set carries the error).
    fn run<'a>(&'a self, db: &'a str, stmts: Vec<String>, max_rows: usize, opts: &'a RunOpts) -> BoxFut<'a, Vec<ResultSet>>;
    /// Stop what `slot`'s request is running, from a separate session (MySQL `KILL QUERY`,
    /// PostgreSQL `pg_cancel_backend`, SQLite interrupt). Ok(false) = nothing was running.
    fn cancel<'a>(&'a self, db: &'a str, slot: &'a CancelSlot) -> BoxFut<'a, bool>;
    fn close(&self) -> BoxFut<'_, ()>;
}

impl dyn SqlDriver {
    pub fn family(&self) -> Family { self.kind().family() }

    async fn one(&self, db: &str, sql: String) -> Result<ResultSet> {
        let mut v = self.run(db, vec![sql], MAX_ROWS, &RunOpts::default()).await?;
        let r = v.pop().ok_or_else(|| anyhow::anyhow!("no result"))?;
        match &r.error { Some(e) => Err(anyhow::anyhow!(e.clone())), None => Ok(r) }
    }

    pub async fn server_version(&self) -> Result<String> {
        let q = match self.family() { Family::Mysql => "SELECT VERSION()", Family::Postgres => "SHOW server_version", _ => "SELECT sqlite_version()" };
        Ok(self.one("", q.into()).await?.first().unwrap_or_default().to_string())
    }

    pub async fn databases(&self) -> Result<Vec<String>> { Ok(self.one("", dialect::list_databases(self.family()).into()).await?.col(0).collect()) }

    pub async fn schemas(&self, db: &str) -> Result<Vec<String>> {
        match dialect::list_schemas(self.family()) { Some(q) => Ok(self.one(db, q.into()).await?.col(0).collect()), None => Ok(vec![]) }
    }

    pub async fn tables(&self, db: &str, schema: Option<&str>) -> Result<Vec<TableInfo>> {
        let r = self.one(db, dialect::list_tables(self.family(), db, schema)).await?;
        Ok(r.rows.iter().map(|row| {
            let name = row.first().cloned().flatten().unwrap_or_default();
            let ty = row.get(1).cloned().flatten().unwrap_or_default().to_ascii_uppercase();
            TableInfo { name, kind: if ty.contains("VIEW") { ObjKind::View } else { ObjKind::Table } }
        }).collect())
    }

    pub async fn columns(&self, db: &str, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>> {
        let r = self.one(db, dialect::list_columns(self.family(), db, schema, table)).await?;
        Ok(r.rows.iter().map(|row| {
            let g = |i: usize| row.get(i).cloned().flatten();
            ColumnInfo {
                name: g(0).unwrap_or_default(),
                type_name: g(1).unwrap_or_default(),
                nullable: g(2).map(|v| v.eq_ignore_ascii_case("YES")),
                primary: g(3).is_some_and(|k| k == "PRI"),
                default: g(4),
                comment: g(5).unwrap_or_default(),
            }
        }).collect())
    }

    /// One page of a table plus (on the first page) its total row count.
    pub async fn page(&self, db: &str, schema: Option<&str>, table: &str, where_: &str, order: Option<(&str, bool)>, offset: usize) -> Result<(ResultSet, Option<u64>)> {
        let f = self.family();
        let t = dialect::qualified(f, db, schema, table);
        let mut stmts = vec![dialect::select_page(f, &t, where_, order, PAGE, offset)];
        if offset == 0 { stmts.push(dialect::count(&t, where_)); }
        let mut v = self.run(db, stmts, PAGE, &RunOpts::default()).await?;
        if let Some(e) = v.iter().find_map(|r| r.error.clone()) { anyhow::bail!(e); }
        let total = if offset == 0 { v.pop().and_then(|r| r.first().and_then(|s| s.parse().ok())) } else { None };
        let page = v.pop().ok_or_else(|| anyhow::anyhow!("no result"))?;
        Ok((page, total))
    }

    /// User SQL: split, guard, run. `read_only` refuses anything that is not a read.
    pub async fn execute(&self, db: &str, sql: &str, read_only: bool) -> Result<Vec<ResultSet>> { self.execute_with(db, sql, read_only, &RunOpts::default()).await }

    pub async fn execute_with(&self, db: &str, sql: &str, read_only: bool, opts: &RunOpts) -> Result<Vec<ResultSet>> {
        let stmts: Vec<String> = split::split(sql).into_iter().map(|s| s.text).collect();
        if stmts.is_empty() { return Ok(vec![]); }
        if read_only {
            if let Some(s) = stmts.iter().find(|s| split::classify(s) != split::Class::Read) {
                anyhow::bail!("{}: {}", crate::i18n::t("db.err_read_only"), s.chars().take(80).collect::<String>());
            }
        }
        self.run(db, stmts, MAX_ROWS, opts).await
    }
}

impl dyn SqlDriver {
    /// Create statement for a table or view, as the server reports it (PostgreSQL tables
    /// are assembled from the catalog).
    pub async fn ddl(&self, db: &str, schema: Option<&str>, table: &str) -> Result<String> {
        let f = self.family();
        let q = dialect::qualified(f, db, schema, table);
        let relkind = if f == Family::Postgres { self.one(db, format!("SELECT relkind::text FROM pg_class WHERE oid = {}::regclass", dialect::quote_lit(f, &q))).await?.first().unwrap_or_default().to_string() } else { String::new() };
        let view = relkind == "v" || relkind == "m";
        match f {
            Family::Mysql => {
                let r = self.one(db, format!("SHOW CREATE TABLE {q}")).await?;
                Ok(r.cell(0, 1).unwrap_or_default().to_string() + ";")
            }
            Family::Postgres if view => {
                let r = self.one(db, format!("SELECT pg_get_viewdef({}::regclass, true)", dialect::quote_lit(f, &q))).await?;
                Ok(format!("CREATE {} {q} AS\n{}", if relkind == "m" { "MATERIALIZED VIEW" } else { "OR REPLACE VIEW" }, r.first().unwrap_or_default().trim_end()))
            }
            Family::Postgres => {
                let lit = dialect::quote_lit(f, &q);
                let cols = self.columns(db, schema, table).await?;
                let cons = self.one(db, dialect::pg_constraints(&lit)).await?;
                let idx = self.one(db, dialect::pg_indexes(&lit)).await?;
                let cons: Vec<(String, String)> = cons.rows.iter().map(|r| (r.first().cloned().flatten().unwrap_or_default(), r.get(1).cloned().flatten().unwrap_or_default())).collect();
                Ok(dialect::pg_create_table(&q, &cols, &cons, &idx.col(0).collect::<Vec<_>>()))
            }
            _ => {
                let r = self.one(db, format!("SELECT sql FROM sqlite_master WHERE tbl_name = {} AND sql IS NOT NULL ORDER BY type = 'index', name", dialect::quote_lit(f, table))).await?;
                Ok(r.col(0).map(|s| s + ";").collect::<Vec<_>>().join("\n\n"))
            }
        }
    }

    /// Run `stmts` as one transaction; the first error rolls everything back and is returned.
    pub async fn apply(&self, db: &str, schema: Option<&str>, stmts: Vec<String>) -> Result<u64> {
        let opts = RunOpts { schema: schema.map(String::from), transaction: true, cancel: None };
        let v = self.run(db, stmts, 1, &opts).await?;
        if let Some(e) = v.iter().find_map(|r| r.error.clone()) { anyhow::bail!(e); }
        Ok(v.iter().map(|r| r.affected).sum())
    }
}

pub type Driver = Arc<dyn SqlDriver>;
