//! DM8 driver over dm-conn / dm-pool. One pool per connection. Queries are plain SQL; Stop
//! drops the in-flight future so the borrowed connection is discarded instead of returned.
//! Nothing in this module opens a socket except [`connect`].
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dm_rust::{ConnectOptions, Value};
use dm_rust::{Pool, PoolConfig, PooledConnection};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

use super::{split, BoxFut, CancelSlot, ColumnInfo, ResultSet, RunOpts, Session, Sink, SqlDriver};
use crate::assets::{DbConn, DbKind};

/// Unreserved characters stay literal; everything else (including `@ : / ? # %`) is `%XX`.
const URL_ENC: AsciiSet = NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Same cap as the sqlx pools.
const POOL_MAX: usize = 4;
const CELL_MAX: usize = 64 * 1024;

/// `select NAME from SYSOBJECTS where TYPE$ = 'SCH'`. Users (`ALL_USERS`) are not schemas.
pub const SCHEMAS_SQL: &str =
    "select s.NAME from SYSOBJECTS s where s.TYPE$ = 'SCH' order by s.NAME";

pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
pub fn quote_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Schema name for catalog SQL. On this engine the tree stores the schema in `db`.
pub fn owner<'a>(db: &'a str, schema: Option<&'a str>) -> &'a str {
    if !db.is_empty() {
        db
    } else {
        schema.unwrap_or("")
    }
}

pub fn set_schema_sql(schema: &str) -> String {
    format!("SET SCHEMA {}", quote_ident(schema))
}

pub fn tables_sql(owner: &str) -> String {
    format!(
        "select t.TABLE_NAME, c.COMMENTS from ALL_TABLES t left join ALL_TAB_COMMENTS c on c.OWNER = t.OWNER and c.TABLE_NAME = t.TABLE_NAME where t.OWNER = {} order by t.TABLE_NAME",
        quote_lit(owner)
    )
}
pub fn views_sql(owner: &str) -> String {
    format!(
        "select VIEW_NAME from ALL_VIEWS where OWNER = {} order by VIEW_NAME",
        quote_lit(owner)
    )
}
pub fn list_tables_sql(owner: &str) -> String {
    let o = quote_lit(owner);
    format!("select TABLE_NAME, 'TABLE' from ALL_TABLES where OWNER = {o} union all select VIEW_NAME, 'VIEW' from ALL_VIEWS where OWNER = {o} order by 1")
}

/// Rows: (name, type, nullable YES/NO, key PRI or empty, default, comment, extra).
pub fn columns_sql(owner: &str, table: &str) -> String {
    let (o, t) = (quote_lit(owner), quote_lit(table));
    format!(
        "select c.COLUMN_NAME, \
         case when c.DATA_TYPE in ('CHAR','CHARACTER','VARCHAR','VARCHAR2','NCHAR','NVARCHAR','NVARCHAR2') then c.DATA_TYPE || '(' || trim(to_char(c.DATA_LENGTH)) || ')' \
              when c.DATA_TYPE in ('DECIMAL','DEC','NUMERIC','NUMBER') and c.DATA_PRECISION is not null then c.DATA_TYPE || '(' || trim(to_char(c.DATA_PRECISION)) || ',' || trim(to_char(nvl(c.DATA_SCALE, 0))) || ')' \
              else c.DATA_TYPE end, \
         case when c.NULLABLE = 'Y' then 'YES' else 'NO' end, \
         case when exists (select 1 from ALL_CONSTRAINTS k join ALL_CONS_COLUMNS cc on k.OWNER = cc.OWNER and k.CONSTRAINT_NAME = cc.CONSTRAINT_NAME \
              where k.OWNER = c.OWNER and k.TABLE_NAME = c.TABLE_NAME and k.CONSTRAINT_TYPE = 'P' and cc.COLUMN_NAME = c.COLUMN_NAME) then 'PRI' else '' end, \
         c.DATA_DEFAULT, \
         (select m.COMMENTS from ALL_COL_COMMENTS m where m.OWNER = c.OWNER and m.TABLE_NAME = c.TABLE_NAME and m.COLUMN_NAME = c.COLUMN_NAME), \
         '' \
         from ALL_TAB_COLUMNS c where c.OWNER = {o} and c.TABLE_NAME = {t} order by c.COLUMN_ID"
    )
}

fn index_where(owner: &str, table: &str) -> String {
    format!(
        "i.TABLE_OWNER = {} and i.TABLE_NAME = {} and i.INDEX_TYPE <> 'CLUSTER' and not exists (select 1 from ALL_CONSTRAINTS c where c.OWNER = i.OWNER and c.INDEX_NAME = i.INDEX_NAME)",
        quote_lit(owner), quote_lit(table)
    )
}

/// Rows: (name, unique 1/0, columns joined by U+001F, constraint 0). Cluster and constraint indexes are excluded.
pub fn indexes_sql(owner: &str, table: &str) -> String {
    format!(
        "select i.INDEX_NAME, case when i.UNIQUENESS = 'UNIQUE' then '1' else '0' end, \
         (select listagg(c.COLUMN_NAME || case when c.DESCEND = 'DESC' then ' DESC' else '' end, chr(31)) within group (order by c.COLUMN_POSITION) \
            from ALL_IND_COLUMNS c where c.TABLE_OWNER = i.TABLE_OWNER and c.TABLE_NAME = i.TABLE_NAME and c.INDEX_NAME = i.INDEX_NAME), \
         '0' from ALL_INDEXES i where {} order by i.INDEX_NAME",
        index_where(owner, table)
    )
}

pub fn normal_index_names_sql(owner: &str, table: &str) -> String {
    format!(
        "select i.INDEX_NAME from ALL_INDEXES i where {} order by i.INDEX_NAME",
        index_where(owner, table)
    )
}

pub fn object_kind_sql(owner: &str, name: &str) -> String {
    let (o, n) = (quote_lit(owner), quote_lit(name));
    format!("select 'TABLE' from ALL_TABLES where OWNER = {o} and TABLE_NAME = {n} union all select 'VIEW' from ALL_VIEWS where OWNER = {o} and VIEW_NAME = {n}")
}

/// `DBMS_METADATA.GET_DDL(kind, object, schema)`.
pub fn get_ddl_sql(kind: &str, name: &str, schema: &str) -> String {
    format!(
        "select DBMS_METADATA.GET_DDL({}, {}, {}) from dual",
        quote_lit(kind),
        quote_lit(name),
        quote_lit(schema)
    )
}

pub fn table_comment_sql(owner: &str, table: &str) -> String {
    format!(
        "select COMMENTS from ALL_TAB_COMMENTS where OWNER = {} and TABLE_NAME = {}",
        quote_lit(owner),
        quote_lit(table)
    )
}

pub fn column_comments_sql(owner: &str, table: &str) -> String {
    format!(
        "select COLUMN_NAME, COMMENTS from ALL_COL_COMMENTS where OWNER = {} and TABLE_NAME = {} and COMMENTS is not null order by COLUMN_NAME",
        quote_lit(owner), quote_lit(table)
    )
}

pub fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// GET_DDL text plus table/column comments and normal index DDL. `\r\n` and lone `\r` become `\n`.
pub fn assemble_ddl(
    base: &str,
    kind: &str,
    qualified: &str,
    table_comment: Option<&str>,
    columns: &[(&str, &str)],
    index_ddls: &[&str],
) -> String {
    let mut parts = Vec::new();
    let base = normalize_newlines(base);
    let base = base.trim();
    if !base.is_empty() {
        let mut b = base.to_string();
        if !b.ends_with(';') {
            b.push(';');
        }
        parts.push(b);
    }
    let on = if kind.eq_ignore_ascii_case("VIEW") {
        "VIEW"
    } else {
        "TABLE"
    };
    if let Some(c) = table_comment.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("COMMENT ON {on} {qualified} IS {};", quote_lit(c)));
    }
    for (col, c) in columns {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        parts.push(format!(
            "COMMENT ON COLUMN {qualified}.{} IS {};",
            quote_ident(col),
            quote_lit(c)
        ));
    }
    for ddl in index_ddls {
        let d = normalize_newlines(ddl);
        let d = d.trim();
        if d.is_empty() {
            continue;
        }
        let mut d = d.to_string();
        if !d.ends_with(';') {
            d.push(';');
        }
        parts.push(d);
    }
    normalize_newlines(&parts.join("\n"))
}

fn enc(s: &str) -> String {
    utf8_percent_encode(s, &URL_ENC).to_string()
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Pieces of a `dm://` URL. Building and parsing do not touch the network.
#[derive(Debug, Clone, PartialEq)]
pub struct DmUrl {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub schema: Option<String>,
    pub tls_ca: Option<String>,
    pub tls_server_name: Option<String>,
    pub tls_verify: bool,
}

impl DmUrl {
    pub fn from_conn(c: &DbConn, password: &str) -> Self {
        DmUrl {
            host: c.host.clone(),
            port: c.effective_port(),
            user: c.user.clone(),
            password: password.to_string(),
            schema: (!c.database.is_empty()).then(|| c.database.clone()),
            tls_ca: (!c.tls_ca.is_empty()).then(|| c.tls_ca.clone()),
            tls_server_name: (!c.tls_server_name.is_empty()).then(|| c.tls_server_name.clone()),
            tls_verify: c.tls != "insecure",
        }
    }

    /// `dm://user:pass@host:port[/schema]?ssl_ca=&ssl_server_name=&ssl_verify=false`.
    pub fn to_url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let mut s = format!(
            "dm://{}:{}@{}:{}",
            enc(&self.user),
            enc(&self.password),
            host,
            self.port
        );
        if let Some(schema) = self.schema.as_ref().filter(|s| !s.is_empty()) {
            s.push('/');
            s.push_str(&enc(schema));
        }
        let mut q = Vec::new();
        if let Some(ca) = self.tls_ca.as_ref().filter(|s| !s.is_empty()) {
            q.push(format!("ssl_ca={}", enc(ca)));
        }
        if let Some(n) = self.tls_server_name.as_ref().filter(|s| !s.is_empty()) {
            q.push(format!("ssl_server_name={}", enc(n)));
        }
        if !self.tls_verify {
            q.push("ssl_verify=false".into());
        }
        if !q.is_empty() {
            s.push('?');
            s.push_str(&q.join("&"));
        }
        s
    }

    /// Round-trip through the driver's parser. IPv6 brackets are stripped from the host.
    pub fn parse(url: &str) -> Option<Self> {
        let o = ConnectOptions::from_url(url.trim()).ok()?;
        Some(DmUrl {
            host: o.host.trim_matches(['[', ']']).to_string(),
            port: o.port,
            user: o.user,
            password: o.password,
            schema: o.schema.filter(|s| !s.is_empty()),
            tls_ca: o
                .tls_ca_file
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty()),
            tls_server_name: o.tls_server_name.filter(|s| !s.is_empty()),
            tls_verify: o.tls_verify,
        })
    }

    /// Driver options. `endpoint` replaces host/port (the local end of an SSH tunnel).
    /// A tunnel to a non-loopback host uses that host as the certificate name when none was set.
    pub fn options(
        &self,
        endpoint: Option<(String, u16)>,
        connect_timeout: Duration,
    ) -> ConnectOptions {
        let (host, port) = endpoint
            .clone()
            .unwrap_or_else(|| (self.host.clone(), self.port));
        let mut o = ConnectOptions::new(host, port, self.user.clone(), self.password.clone());
        o.app_name = "ShellAX".into();
        o.connect_timeout = connect_timeout;
        o.request_timeout = None;
        o.login_encrypt = true;
        o.schema = self.schema.clone().filter(|s| !s.is_empty());
        o.tls_verify = self.tls_verify;
        if let Some(ca) = self.tls_ca.as_ref().filter(|s| !s.is_empty()) {
            o.tls_ca_file = Some(ca.into());
        }
        o.tls_server_name = self.tls_server_name.clone().filter(|s| !s.is_empty());
        let tunneled = endpoint.as_ref().is_some_and(|(h, _)| h != &self.host);
        if o.tls_server_name.is_none()
            && tunneled
            && !self.host.is_empty()
            && !is_loopback(&self.host)
        {
            o.tls_server_name = Some(self.host.clone());
        }
        o
    }
}

/// Form fill for a pasted `dm://` URL, plus the password (kept out of config).
pub fn parse_conn(url: &str) -> Option<(DbConn, Option<String>)> {
    let u = DmUrl::parse(url)?;
    let pass = (!u.password.is_empty()).then_some(u.password);
    let c = DbConn {
        kind: DbKind::Dameng,
        host: u.host,
        port: Some(u.port),
        user: u.user,
        database: u.schema.unwrap_or_default(),
        tls: if u.tls_verify {
            String::new()
        } else {
            "insecure".into()
        },
        tls_ca: u.tls_ca.unwrap_or_default(),
        tls_server_name: u.tls_server_name.unwrap_or_default(),
        ..Default::default()
    };
    Some((c, pass))
}

fn cancelled_text() -> String {
    crate::i18n::t("db.cancelled_by_user").to_string()
}

async fn wait_cancel(slot: Option<&CancelSlot>) {
    match slot {
        None => std::future::pending::<()>().await,
        Some(slot) => loop {
            if slot.requested() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        },
    }
}

/// Clears the cancel slot when the request future is dropped or finishes.
struct Unbind<'a>(&'a CancelSlot);
impl Drop for Unbind<'_> {
    fn drop(&mut self) {
        self.0.bind(None);
    }
}

fn clip(mut s: String, full: bool) -> String {
    if !full && s.len() > CELL_MAX {
        let mut i = CELL_MAX;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        s.truncate(i);
        s.push('…');
    }
    s
}

fn cell_text(v: &Value, full: bool) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Bytes(b) => {
            let n = if full { b.len() } else { b.len().min(64) };
            let hex: String = b.iter().take(n).map(|x| format!("{x:02X}")).collect();
            let more = if !full && b.len() > 64 {
                format!("… ({} B)", b.len())
            } else {
                String::new()
            };
            Some(format!("0x{hex}{more}"))
        }
        other => other.to_text().map(|s| clip(s, full)),
    }
}

fn columns_of(cols: &[dm_rust::Column]) -> Vec<ColumnInfo> {
    cols.iter()
        .map(|c| ColumnInfo {
            name: c.name.clone(),
            type_name: c.type_name.clone(),
            // Wire flag: 0 = not null, anything else = nullable.
            nullable: Some(c.nullable != 0),
            ..Default::default()
        })
        .collect()
}

fn from_query(sql: &str, q: dm_rust::QueryResult, max: usize) -> ResultSet {
    let mut truncated = false;
    let mut rows = Vec::new();
    for row in &q.rows {
        if rows.len() >= max {
            truncated = true;
            break;
        }
        rows.push(row.values.iter().map(|v| cell_text(v, false)).collect());
    }
    ResultSet {
        sql: sql.into(),
        columns: columns_of(&q.columns),
        rows,
        truncated,
        returns_rows: true,
        ..Default::default()
    }
}

pub struct DmDriver {
    pool: Pool,
    version: String,
    read_only: bool,
}

/// Open one pool and prove it with a single connection. `host`/`port` may be the local tunnel end.
pub async fn connect(
    c: &DbConn,
    password: Option<String>,
    host: String,
    port: u16,
) -> Result<DmDriver> {
    let timeout = c.connect_timeout();
    let opts =
        DmUrl::from_conn(c, password.as_deref().unwrap_or("")).options(Some((host, port)), timeout);
    let pool = Pool::new(
        opts,
        PoolConfig {
            max_size: POOL_MAX,
            acquire_timeout: timeout,
            ..PoolConfig::default()
        },
    );
    let conn = tokio::time::timeout(timeout, pool.get())
        .await
        .context("timeout")??;
    let version = conn.server_version().unwrap_or("").to_string();
    drop(conn);
    Ok(DmDriver {
        pool,
        version,
        read_only: c.read_only,
    })
}

impl DmDriver {
    async fn checkout(&self, slot: Option<&CancelSlot>) -> Result<PooledConnection> {
        tokio::select! {
            biased;
            _ = wait_cancel(slot) => anyhow::bail!(cancelled_text()),
            r = self.pool.get() => r.map_err(|e| anyhow::anyhow!(e)),
        }
    }

    async fn set_schema(
        &self,
        conn: &mut PooledConnection,
        db: &str,
        opts: &RunOpts,
        slot: Option<&CancelSlot>,
    ) -> Result<()> {
        let schema = opts
            .schema
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(db);
        if schema.is_empty() {
            return Ok(());
        }
        let sql = set_schema_sql(schema);
        let r = tokio::select! {
            biased;
            _ = wait_cancel(slot) => Err(anyhow::anyhow!(cancelled_text())),
            r = conn.execute(&sql, &[]) => r.map_err(|e| anyhow::anyhow!(e)),
        };
        r?;
        Ok(())
    }

    async fn run_stmt(
        &self,
        conn: &mut PooledConnection,
        sql: &str,
        max: usize,
        slot: Option<&CancelSlot>,
    ) -> Result<ResultSet> {
        if split::returns_rows(sql) {
            let q = tokio::select! {
                biased;
                _ = wait_cancel(slot) => Err(anyhow::anyhow!(cancelled_text())),
                r = conn.query(sql, &[]) => r.map_err(|e| anyhow::anyhow!(e)),
            }?;
            Ok(from_query(sql, q, max))
        } else {
            let e = tokio::select! {
                biased;
                _ = wait_cancel(slot) => Err(anyhow::anyhow!(cancelled_text())),
                r = conn.execute(sql, &[]) => r.map_err(|e| anyhow::anyhow!(e)),
            }?;
            Ok(ResultSet {
                sql: sql.into(),
                affected: e.rows_affected.max(0) as u64,
                ..Default::default()
            })
        }
    }

    /// Commit or roll back. A broken or cancelled connection is discarded so the pool stays usable.
    async fn end_tx(mut conn: PooledConnection, failed: bool, cancelled: bool) -> Option<String> {
        if cancelled || conn.is_broken() {
            conn.discard();
            return None;
        }
        let r = if failed {
            conn.rollback().await
        } else {
            conn.commit().await
        };
        match r {
            Ok(()) => None,
            Err(e) => {
                conn.discard();
                Some(e.to_string())
            }
        }
    }

    async fn run_on(
        &self,
        db: &str,
        stmts: Vec<String>,
        max_rows: usize,
        opts: &RunOpts,
        slot: Option<&CancelSlot>,
    ) -> Result<Vec<ResultSet>> {
        let mut conn = self.checkout(slot).await?;
        self.set_schema(&mut conn, db, opts, slot).await?;
        if self.read_only
            && stmts
                .iter()
                .any(|s| split::classify(s) != split::Class::Read)
        {
            anyhow::bail!(crate::i18n::t("db.err_read_only").to_string());
        }
        let mut in_tx = false;
        if opts.transaction {
            conn.begin().await?;
            in_tx = true;
        }
        let mut out = Vec::new();
        let mut failed = false;
        for s in stmts {
            let t0 = Instant::now();
            match self.run_stmt(&mut conn, &s, max_rows, slot).await {
                Ok(mut rs) => {
                    rs.elapsed = t0.elapsed();
                    out.push(rs);
                }
                Err(e) => {
                    let msg = if slot.is_some_and(|c| c.requested()) {
                        cancelled_text()
                    } else {
                        e.to_string()
                    };
                    out.push(ResultSet {
                        sql: s,
                        error: Some(msg),
                        elapsed: t0.elapsed(),
                        ..Default::default()
                    });
                    failed = true;
                    break;
                }
            }
        }
        let cancelled = slot.is_some_and(|c| c.requested());
        if in_tx {
            if let Some(msg) = Self::end_tx(conn, failed, cancelled).await {
                out.push(ResultSet {
                    error: Some(msg),
                    ..Default::default()
                });
            }
        } else if cancelled || conn.is_broken() {
            conn.discard();
        }
        Ok(out)
    }
}

impl SqlDriver for DmDriver {
    fn kind(&self) -> DbKind {
        DbKind::Dameng
    }
    fn startup_version(&self) -> Option<String> {
        Some(self.version.clone())
    }

    fn run<'a>(
        &'a self,
        db: &'a str,
        stmts: Vec<String>,
        max_rows: usize,
        opts: &'a RunOpts,
    ) -> BoxFut<'a, Vec<ResultSet>> {
        Box::pin(async move {
            let slot = opts.cancel.as_deref();
            if let Some(s) = slot {
                s.bind(Some(Session::Server(1)));
            }
            let _unbind = slot.map(Unbind);
            self.run_on(db, stmts, max_rows, opts, slot).await
        })
    }

    fn stream<'a>(
        &'a self,
        db: &'a str,
        sql: String,
        opts: &'a RunOpts,
        sink: Sink<'a>,
    ) -> BoxFut<'a, u64> {
        Box::pin(async move {
            let slot = opts.cancel.as_deref();
            if let Some(s) = slot {
                s.bind(Some(Session::Server(1)));
            }
            let _unbind = slot.map(Unbind);
            let mut conn = self.checkout(slot).await?;
            self.set_schema(&mut conn, db, opts, slot).await?;
            let q = tokio::select! {
                biased;
                _ = wait_cancel(slot) => Err(anyhow::anyhow!(cancelled_text())),
                r = conn.query(&sql, &[]) => r.map_err(|e| anyhow::anyhow!(e)),
            };
            let broken = conn.is_broken() || slot.is_some_and(|s| s.requested());
            if broken {
                conn.discard();
            } else {
                drop(conn);
            }
            let q = q?;
            let cols = columns_of(&q.columns);
            let mut n = 0u64;
            for row in q.rows {
                if slot.is_some_and(|s| s.requested()) {
                    anyhow::bail!(cancelled_text());
                }
                sink(
                    &cols,
                    row.values.iter().map(|v| cell_text(v, true)).collect(),
                )?;
                n += 1;
            }
            Ok(n)
        })
    }

    fn cancel<'a>(&'a self, _db: &'a str, slot: &'a CancelSlot) -> BoxFut<'a, bool> {
        Box::pin(async move { Ok(slot.with_session(|s| s.is_some())) })
    }

    fn close(&self) -> BoxFut<'_, ()> {
        self.pool.close();
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dm_rust::Compression;

    fn sample() -> DmUrl {
        DmUrl {
            host: "db.example.com".into(),
            port: 5236,
            user: "SYSDBA".into(),
            password: "p@ss:w/rd?%#".into(),
            schema: Some("MY SCHEMA".into()),
            tls_ca: Some("/tmp/ca.pem".into()),
            tls_server_name: Some("db.example.com".into()),
            tls_verify: false,
        }
    }

    #[test]
    fn url_roundtrip_escapes_credentials_and_tls() {
        let u = sample();
        let s = u.to_url();
        assert!(!s.contains("p@ss"));
        assert!(s.contains("p%40ss"));
        assert!(s.contains("%3A"));
        assert!(s.contains("%2F"));
        assert!(s.contains("ssl_ca=%2Ftmp%2Fca.pem"));
        assert!(s.contains("ssl_server_name=db.example.com"));
        assert!(s.ends_with("ssl_verify=false") || s.contains("ssl_verify=false"));
        assert!(!s.contains('+'));
        let back = DmUrl::parse(&s).unwrap();
        assert_eq!(back, u);
        let o = ConnectOptions::from_url(&s).unwrap();
        assert_eq!(o.password, u.password);
        assert_eq!(o.user, "SYSDBA");
        assert_eq!(o.schema.as_deref(), Some("MY SCHEMA"));
        assert!(!o.tls_verify);
        assert_eq!(
            o.tls_ca_file
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned()),
            u.tls_ca
        );
        assert_eq!(o.tls_server_name.as_deref(), Some("db.example.com"));
    }

    #[test]
    fn url_omits_verify_when_on_and_keeps_empty_password() {
        let u = DmUrl {
            host: "h".into(),
            port: 5236,
            user: "a".into(),
            password: String::new(),
            schema: None,
            tls_ca: None,
            tls_server_name: None,
            tls_verify: true,
        };
        let s = u.to_url();
        assert_eq!(s, "dm://a:@h:5236");
        assert!(!s.contains("ssl_verify"));
        let back = DmUrl::parse(&s).unwrap();
        assert!(back.tls_verify && back.password.is_empty() && back.schema.is_none());
    }

    #[test]
    fn ipv6_is_bracketed_and_parses() {
        let u = DmUrl {
            host: "::1".into(),
            port: 5236,
            user: "a".into(),
            password: "b".into(),
            schema: None,
            tls_ca: None,
            tls_server_name: None,
            tls_verify: true,
        };
        let s = u.to_url();
        assert!(s.contains("[::1]:5236"), "{s}");
        assert_eq!(DmUrl::parse(&s).unwrap().host, "::1");
    }

    #[test]
    fn options_use_raw_password_and_tunnel_server_name() {
        let u = sample();
        let direct = u.options(
            Some(("db.example.com".into(), 5236)),
            Duration::from_secs(8),
        );
        assert_eq!(direct.password, "p@ss:w/rd?%#");
        assert_eq!(direct.host, "db.example.com");
        assert_eq!(direct.tls_server_name.as_deref(), Some("db.example.com"));
        assert_eq!(direct.app_name, "ShellAX");
        assert_eq!(direct.connect_timeout, Duration::from_secs(8));
        assert!(direct.request_timeout.is_none());
        assert!(direct.login_encrypt);
        assert!(!direct.read_only);
        assert_eq!(direct.compression, Compression::Off);
        assert_eq!(direct.schema.as_deref(), Some("MY SCHEMA"));

        let mut bare = u.clone();
        bare.tls_server_name = None;
        bare.tls_verify = true;
        let tun = bare.options(Some(("127.0.0.1".into(), 40000)), Duration::from_secs(10));
        assert_eq!((tun.host.as_str(), tun.port), ("127.0.0.1", 40000));
        assert_eq!(tun.tls_server_name.as_deref(), Some("db.example.com"));
        assert!(tun.tls_verify);

        let local = DmUrl {
            host: "127.0.0.1".into(),
            port: 5236,
            user: "a".into(),
            password: "b".into(),
            schema: None,
            tls_ca: None,
            tls_server_name: None,
            tls_verify: true,
        };
        let same = local.options(Some(("127.0.0.1".into(), 5236)), Duration::from_secs(1));
        assert!(same.tls_server_name.is_none());
    }

    #[test]
    fn quote_escapes_and_schema_sql_is_not_all_users() {
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_lit("a'b"), "'a''b'");
        assert_eq!(set_schema_sql("a\"b"), "SET SCHEMA \"a\"\"b\"");
        assert!(SCHEMAS_SQL.contains("SYSOBJECTS") && SCHEMAS_SQL.contains("TYPE$ = 'SCH'"));
        assert!(!SCHEMAS_SQL.to_ascii_uppercase().contains("ALL_USERS"));
        assert!(!SCHEMAS_SQL.to_ascii_uppercase().contains("DBA_USERS"));
    }

    #[test]
    fn metadata_sql_filters_and_escapes() {
        let ix = indexes_sql("O'WN", "T");
        assert!(ix.contains("INDEX_TYPE <> 'CLUSTER'"));
        assert!(ix.contains("ALL_CONSTRAINTS"));
        assert!(ix.contains("'O''WN'"));
        assert!(ix.contains("chr(31)") || ix.contains("CHR(31)"));
        assert!(!ix.to_ascii_uppercase().contains("ALL_USERS"));
        let names = normal_index_names_sql("S", "T");
        assert!(names.contains("INDEX_TYPE <> 'CLUSTER'") && names.contains("ALL_CONSTRAINTS"));
        let cols = columns_sql("S", "T'x");
        assert!(cols.contains("'YES'") && cols.contains("'NO'") && cols.contains("'PRI'"));
        assert!(
            cols.contains("ALL_TAB_COLUMNS")
                && cols.contains("ALL_COL_COMMENTS")
                && cols.contains("'T''x'")
        );
        let ddl = get_ddl_sql("TABLE", "EMP", "META_T");
        assert_eq!(
            ddl,
            "select DBMS_METADATA.GET_DDL('TABLE', 'EMP', 'META_T') from dual"
        );
        assert!(object_kind_sql("S", "T").contains("ALL_VIEWS"));
        assert!(tables_sql("S").contains("ALL_TABLES") && views_sql("S").contains("ALL_VIEWS"));
        assert_eq!(owner("S", None), "S");
        assert_eq!(owner("", Some("X")), "X");
    }

    #[test]
    fn assemble_ddl_normalizes_and_appends_comments() {
        let out = assemble_ddl(
            "CREATE TABLE \"S\".\"T\"\r\n(\r\n\"ID\" INT\r\n)\r\n",
            "TABLE",
            "\"S\".\"T\"",
            Some("员工表"),
            &[("NAME", "姓名"), ("X", "  "), ("Y", ""), ("C", "a'b")],
            &["CREATE INDEX \"S\".\"I\" ON \"S\".\"T\"(\"A\" ASC)\r\n", ""],
        );
        assert!(!out.contains('\r'), "{out:?}");
        assert!(out.contains("CREATE TABLE \"S\".\"T\"\n(\n\"ID\" INT\n);"));
        assert!(out.contains("COMMENT ON TABLE \"S\".\"T\" IS '员工表';"));
        assert!(out.contains("COMMENT ON COLUMN \"S\".\"T\".\"NAME\" IS '姓名';"));
        assert!(out.contains("COMMENT ON COLUMN \"S\".\"T\".\"C\" IS 'a''b';"));
        assert!(!out.contains("\"X\""));
        assert!(out.contains("CREATE INDEX \"S\".\"I\" ON \"S\".\"T\"(\"A\" ASC);"));
        let view = assemble_ddl(
            "create view v as select 1 from dual\r",
            "view",
            "\"S\".\"V\"",
            Some("  "),
            &[],
            &[],
        );
        assert!(view.starts_with("create view v as select 1 from dual;"));
        assert!(!view.contains("COMMENT"));
        assert!(!view.contains('\r'));
        let commented = assemble_ddl(
            "create view v as select 1\r\n",
            "VIEW",
            "\"S\".\"V\"",
            Some("说明"),
            &[],
            &[],
        );
        assert!(commented.contains("COMMENT ON VIEW \"S\".\"V\" IS '说明';"));
    }

    #[test]
    fn dialect_and_tree_use_dm_sql() {
        use crate::assets::{DbKind, Family};
        use crate::db::{alter, dialect, objtree};
        assert_eq!(dialect::list_databases(Family::Dameng), SCHEMAS_SQL);
        let t = dialect::list_tables(Family::Dameng, "S", None);
        assert!(t.contains("ALL_TABLES") && t.contains("ALL_VIEWS") && t.contains("'S'"));
        assert!(dialect::list_columns(Family::Dameng, "S", None, "T").contains("'PRI'"));
        assert_eq!(
            dialect::qualified(Family::Dameng, "S", None, "T\"x"),
            "\"S\".\"T\"\"x\""
        );
        assert!(dialect::use_database(Family::Dameng, "S").is_none());
        let listed =
            objtree::list_sql(Family::Dameng, objtree::Group::Tables, "A'B", None).unwrap();
        assert!(listed.contains("ALL_TABLES") && listed.contains("'A''B'"));
        assert!(
            objtree::list_sql(Family::Dameng, objtree::Group::Views, "S", None)
                .unwrap()
                .contains("ALL_VIEWS")
        );
        assert!(objtree::list_sql(Family::Dameng, objtree::Group::Queries, "S", None).is_none());
        let g = objtree::groups(DbKind::Dameng);
        assert_eq!(g.first().copied(), Some(objtree::Group::Tables));
        assert_eq!(g.get(1).copied(), Some(objtree::Group::Views));
        assert_eq!(g.last().copied(), Some(objtree::Group::Queries));
        assert!(objtree::view_definition_sql(Family::Dameng, "S", None, "V")
            .unwrap()
            .contains("GET_DDL('VIEW', 'V', 'S')"));
        let ix = alter::list_indexes(Family::Dameng, "S", None, "T");
        assert!(ix.contains("INDEX_TYPE <> 'CLUSTER'") && ix.contains("ALL_CONSTRAINTS"));
    }
}
