//! Unified asset model (v0.2 PRD §8.1): every connectable thing (local shell, SSH host,
//! database, Redis, Docker endpoint) is an asset. All kinds share one group tree, the
//! color tags and the production rule; each sidebar module shows only its own kind.
//! Pure data + functions so they are unit-tested without a UI.
use serde::{Deserialize, Serialize};

use crate::hosts::HostEntry;
use crate::hosttree::GroupEntry;
use crate::outline::normalize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind { Local, Ssh, Database, Redis, Docker }

/// Database engines in the release scope. `driver_ready` says which ones have a working
/// driver in this build; the others are listed in the dialog as not yet available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DbKind {
    #[default]
    Mysql, Mariadb, Postgres, Sqlite, Sqlserver, Oracle, Clickhouse, Mongodb,
    Duckdb, Starrocks, Oceanbase, Opengauss, Kingbase, Dameng, Tdengine,
}

/// Wire protocol family: decides which driver adapter serves a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family { Mysql, Postgres, Sqlite, Mssql, Clickhouse, Duckdb, Other }

impl DbKind {
    pub const ALL: [DbKind; 15] = [
        DbKind::Mysql, DbKind::Mariadb, DbKind::Postgres, DbKind::Sqlite, DbKind::Sqlserver, DbKind::Oracle,
        DbKind::Clickhouse, DbKind::Mongodb, DbKind::Duckdb, DbKind::Starrocks, DbKind::Oceanbase,
        DbKind::Opengauss, DbKind::Kingbase, DbKind::Dameng, DbKind::Tdengine,
    ];
    pub fn id(self) -> &'static str {
        match self {
            DbKind::Mysql => "mysql", DbKind::Mariadb => "mariadb", DbKind::Postgres => "postgres", DbKind::Sqlite => "sqlite",
            DbKind::Sqlserver => "sqlserver", DbKind::Oracle => "oracle", DbKind::Clickhouse => "clickhouse", DbKind::Mongodb => "mongodb",
            DbKind::Duckdb => "duckdb", DbKind::Starrocks => "starrocks", DbKind::Oceanbase => "oceanbase", DbKind::Opengauss => "opengauss",
            DbKind::Kingbase => "kingbase", DbKind::Dameng => "dameng", DbKind::Tdengine => "tdengine",
        }
    }
    pub fn from_id(s: &str) -> Option<DbKind> { DbKind::ALL.into_iter().find(|k| k.id() == s) }
    /// Product name shown in the UI (proper nouns, not translated).
    pub fn label(self) -> &'static str {
        match self {
            DbKind::Mysql => "MySQL", DbKind::Mariadb => "MariaDB", DbKind::Postgres => "PostgreSQL", DbKind::Sqlite => "SQLite",
            DbKind::Sqlserver => "SQL Server", DbKind::Oracle => "Oracle", DbKind::Clickhouse => "ClickHouse", DbKind::Mongodb => "MongoDB",
            DbKind::Duckdb => "DuckDB", DbKind::Starrocks => "StarRocks", DbKind::Oceanbase => "OceanBase", DbKind::Opengauss => "openGauss",
            DbKind::Kingbase => "KingbaseES", DbKind::Dameng => "DM", DbKind::Tdengine => "TDengine",
        }
    }
    pub fn family(self) -> Family {
        match self {
            DbKind::Mysql | DbKind::Mariadb | DbKind::Starrocks | DbKind::Oceanbase => Family::Mysql,
            DbKind::Postgres | DbKind::Opengauss | DbKind::Kingbase => Family::Postgres,
            DbKind::Sqlite => Family::Sqlite,
            DbKind::Sqlserver => Family::Mssql,
            DbKind::Clickhouse => Family::Clickhouse,
            #[cfg(feature = "duckdb")]
            DbKind::Duckdb => Family::Duckdb,
            _ => Family::Other,
        }
    }
    pub fn default_port(self) -> Option<u16> {
        match self {
            DbKind::Mysql | DbKind::Mariadb | DbKind::Oceanbase => Some(3306),
            DbKind::Starrocks => Some(9030),
            DbKind::Postgres => Some(5432),
            DbKind::Opengauss => Some(5432),
            DbKind::Kingbase => Some(54321),
            DbKind::Sqlserver => Some(1433),
            DbKind::Oracle => Some(1521),
            DbKind::Clickhouse => Some(8123),
            DbKind::Mongodb => Some(27017),
            DbKind::Dameng => Some(5236),
            DbKind::Tdengine => Some(6041),
            DbKind::Sqlite | DbKind::Duckdb => None,
        }
    }
    /// File-based engines take a path instead of host/port/user.
    pub fn is_file(self) -> bool { matches!(self, DbKind::Sqlite | DbKind::Duckdb) }
    /// A driver is wired up in this build.
    pub fn driver_ready(self) -> bool { self.family() != Family::Other }
    /// Has schemas between database and tables (PostgreSQL family).
    pub fn has_schemas(self) -> bool { self.family() == Family::Postgres }
}

/// `[[db]]` in config.toml. Secrets are never stored here: the password lives in the
/// secret store under [`DbConn::password_secret`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct DbConn {
    /// Stable id (survives renames); secrets and caches key on it.
    pub id: String,
    pub name: String,
    pub kind: DbKind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub user: String,
    /// Default database (MySQL / PostgreSQL); empty = server default.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub database: String,
    /// SQLite / DuckDB file.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Name of a sidebar SSH host used as jump host; empty = direct.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ssh_via: String,
    /// Client refuses writes; MySQL / PostgreSQL sessions are also set read-only.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    /// disable | prefer | require | verify-ca | verify-full
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tls: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tls_ca: String,
    /// 0 = default (10 s).
    #[serde(skip_serializing_if = "is_zero")]
    pub connect_timeout_secs: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}
fn is_zero(v: &u64) -> bool { *v == 0 }

impl DbConn {
    pub fn password_secret(&self) -> String { format!("db:{}:password", self.id) }
    pub fn effective_port(&self) -> u16 { self.port.or(self.kind.default_port()).unwrap_or(0) }
    pub fn group_path(&self) -> String { self.group.as_deref().map(normalize).unwrap_or_default() }
    pub fn connect_timeout(&self) -> std::time::Duration { std::time::Duration::from_secs(if self.connect_timeout_secs == 0 { 10 } else { self.connect_timeout_secs }) }
    /// One-line address for lists: `user@host:port/db`, or the file path.
    pub fn address(&self) -> String {
        if self.kind.is_file() { return self.file.clone(); }
        let mut s = String::new();
        if !self.user.is_empty() { s.push_str(&self.user); s.push('@'); }
        s.push_str(&self.host);
        s.push(':');
        s.push_str(&self.effective_port().to_string());
        if !self.database.is_empty() { s.push('/'); s.push_str(&self.database); }
        s
    }
}

/// `[[redis]]` in config.toml. The password lives in the secret store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct RedisConn {
    pub id: String,
    pub name: String,
    /// standalone | sentinel | cluster
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mode: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Extra seed nodes `host:port, …` (cluster) or the sentinels (sentinel mode).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub nodes: String,
    /// Sentinel master name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub master: String,
    /// ACL user; empty = default user.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub user: String,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub db: u32,
    /// Namespace separator in the key tree; empty = ":".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub separator: String,
    /// "" = plain, "require" = TLS, "insecure" = TLS without certificate checks.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tls: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ssh_via: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub connect_timeout_secs: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}
fn is_zero_u32(v: &u32) -> bool { *v == 0 }

impl RedisConn {
    pub fn password_secret(&self) -> String { format!("redis:{}:password", self.id) }
    pub fn effective_port(&self) -> u16 { self.port.unwrap_or(if self.mode == "sentinel" { 26379 } else { 6379 }) }
    pub fn sep(&self) -> &str { if self.separator.is_empty() { ":" } else { &self.separator } }
    pub fn group_path(&self) -> String { self.group.as_deref().map(normalize).unwrap_or_default() }
    pub fn connect_timeout(&self) -> std::time::Duration { std::time::Duration::from_secs(if self.connect_timeout_secs == 0 { 10 } else { self.connect_timeout_secs }) }
    pub fn address(&self) -> String {
        let mut s = String::new();
        if !self.user.is_empty() { s.push_str(&self.user); s.push('@'); }
        s.push_str(&format!("{}:{}", self.host, self.effective_port()));
        if self.db != 0 { s.push_str(&format!("/{}", self.db)); }
        s
    }
    /// All seed `(host, port)` pairs: the main address plus `nodes`.
    pub fn seeds(&self) -> Vec<(String, u16)> {
        let mut v = vec![];
        if !self.host.is_empty() { v.push((self.host.clone(), self.effective_port())); }
        for n in self.nodes.split([',', ' ', '\n']).map(str::trim).filter(|s| !s.is_empty()) {
            let (h, p) = match n.rsplit_once(':') { Some((h, p)) => (h.trim_matches(['[', ']']).to_string(), p.parse().unwrap_or(6379)), None => (n.to_string(), 6379) };
            if !v.contains(&(h.clone(), p)) { v.push((h, p)); }
        }
        v
    }
}

/// `[[docker]]` in config.toml: the local engine or one reached over an SSH host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct DockerEndpoint {
    pub id: String,
    pub name: String,
    /// local | ssh
    #[serde(skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    /// Sidebar SSH host name (endpoint = ssh).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ssh_host: String,
    /// Engine socket on that machine; empty = /var/run/docker.sock (local: platform default).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub socket: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl DockerEndpoint {
    pub fn is_ssh(&self) -> bool { self.endpoint == "ssh" }
    pub fn socket_path(&self) -> &str { if self.socket.is_empty() { "/var/run/docker.sock" } else { &self.socket } }
    pub fn group_path(&self) -> String { self.group.as_deref().map(normalize).unwrap_or_default() }
}

/// New random id: 12 lowercase hex chars.
pub fn new_id() -> String {
    use rand::Rng;
    let mut r = rand::thread_rng();
    (0..12).map(|_| char::from_digit(r.gen_range(0..16), 16).unwrap()).collect()
}

/// Give every connection an id and a unique name. Returns true if anything changed.
pub fn normalize_dbs(dbs: &mut [DbConn]) -> bool {
    let mut changed = false;
    let mut seen = std::collections::HashSet::new();
    for d in dbs.iter_mut() {
        if d.id.is_empty() || !seen.insert(d.id.clone()) { d.id = new_id(); seen.insert(d.id.clone()); changed = true; }
        if d.name.trim().is_empty() { d.name = if d.kind.is_file() { leaf_name(&d.file) } else { d.address() }; changed = true; }
    }
    changed
}
/// Fill missing / duplicate ids and empty names of `[[redis]]` and `[[docker]]` entries.
pub fn normalize_kv_docker(rs: &mut [RedisConn], ds: &mut [DockerEndpoint]) -> bool {
    let mut changed = false;
    let mut seen = std::collections::HashSet::new();
    for r in rs.iter_mut() {
        if r.id.is_empty() || !seen.insert(r.id.clone()) { r.id = new_id(); seen.insert(r.id.clone()); changed = true; }
        if r.name.trim().is_empty() { r.name = r.address(); changed = true; }
    }
    for d in ds.iter_mut() {
        if d.id.is_empty() || !seen.insert(d.id.clone()) { d.id = new_id(); seen.insert(d.id.clone()); changed = true; }
        if d.name.trim().is_empty() { d.name = if d.is_ssh() { d.ssh_host.clone() } else { "local".into() }; changed = true; }
    }
    changed
}
fn leaf_name(p: &str) -> String { p.rsplit(['/', '\\']).next().unwrap_or(p).to_string() }

/// Parse `mysql://user:pass@host:3306/db`, `postgres://…`, `postgresql://…`, `sqlite:///path`
/// into a connection plus the password (kept out of the struct).
pub fn parse_uri(uri: &str) -> Option<(DbConn, Option<String>)> {
    let (scheme, rest) = uri.trim().split_once("://")?;
    let kind = match scheme.to_ascii_lowercase().as_str() {
        "mysql" => DbKind::Mysql, "mariadb" => DbKind::Mariadb,
        "postgres" | "postgresql" => DbKind::Postgres,
        "sqlite" => DbKind::Sqlite,
        "sqlserver" | "mssql" => DbKind::Sqlserver,
        "mongodb" | "mongodb+srv" => DbKind::Mongodb,
        "clickhouse" => DbKind::Clickhouse,
        _ => return None,
    };
    let mut c = DbConn { kind, ..Default::default() };
    if kind.is_file() { c.file = rest.split('?').next().unwrap_or("").to_string(); return Some((c, None)); }
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (auth_host, db) = match rest.split_once('/') { Some((a, d)) => (a, d), None => (rest, "") };
    let (auth, hostport) = match auth_host.rsplit_once('@') { Some((a, h)) => (Some(a), h), None => (None, auth_host) };
    let mut pass = None;
    if let Some(a) = auth {
        let (u, p) = match a.split_once(':') { Some((u, p)) => (u, Some(p)), None => (a, None) };
        c.user = pct_decode(u);
        pass = p.map(pct_decode);
    }
    let (host, port) = split_host_port(hostport);
    c.host = host;
    c.port = port;
    c.database = pct_decode(db);
    Some((c, pass))
}
fn split_host_port(s: &str) -> (String, Option<u16>) {
    if let Some(r) = s.strip_prefix('[') {
        // [v6]:port
        if let Some((h, p)) = r.split_once(']') { return (h.to_string(), p.strip_prefix(':').and_then(|p| p.parse().ok())); }
    }
    match s.rsplit_once(':') { Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().ok()), _ => (s.to_string(), None) }
}
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Groups referenced by any asset (and their ancestors) exist in `groups`. Hosts are handled
/// by `hosttree::ensure_groups`; this adds the non-SSH assets.
pub fn ensure_asset_groups(dbs: &[DbConn], groups: &mut Vec<GroupEntry>) {
    for d in dbs {
        let g = d.group_path();
        if g.is_empty() { continue; }
        let mut acc = String::new();
        for part in g.split('/') {
            acc = if acc.is_empty() { part.to_string() } else { format!("{acc}/{part}") };
            if !groups.iter().any(|x| x.path == acc) { groups.push(GroupEntry { path: acc.clone(), tags: vec![] }); }
        }
    }
}

/// Tags of a connection including those inherited from its groups.
pub fn db_effective_tags(d: &DbConn, groups: &[GroupEntry]) -> Vec<String> {
    let mut t = d.tags.clone();
    let g = d.group_path();
    for e in groups { if !g.is_empty() && crate::hosttree::under(&g, &e.path) { for x in &e.tags { if !t.contains(x) { t.push(x.clone()); } } } }
    t
}
pub fn db_is_production(d: &DbConn, groups: &[GroupEntry], prod_tag: &str) -> bool { db_effective_tags(d, groups).iter().any(|t| t == prod_tag) }

/// Rewrite group paths of connections after a group rename/move (`old` → `new`).
pub fn regroup_dbs(dbs: &mut [DbConn], old: &str, new: &str) {
    for d in dbs.iter_mut() {
        let g = d.group_path();
        if crate::hosttree::under(&g, old) { d.group = Some(format!("{new}{}", &g[old.len()..])).filter(|s| !s.is_empty()); }
    }
}

/// A row of a module's sidebar tree: groups (all kinds share them) and the module's assets.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeRow { Group { path: String, depth: usize, count: usize }, Item { index: usize, depth: usize } }

/// Rows for a module: every group path in sidebar order, each followed by its items; ungrouped
/// items last. `item_group` gives the group path of item `i`; `keep` filters items (search).
/// Groups without matching items are shown only when `show_empty` (dimmed by the caller).
pub fn tree_rows(groups: &[GroupEntry], collapsed: &[String], item_groups: &[String], keep: &dyn Fn(usize) -> bool, show_empty: bool) -> Vec<TreeRow> {
    let mut rows = vec![];
    let count_under = |p: &str| item_groups.iter().enumerate().filter(|(i, g)| keep(*i) && crate::hosttree::under(g, p)).count();
    fn walk(parent: &str, depth: usize, groups: &[GroupEntry], collapsed: &[String], item_groups: &[String], keep: &dyn Fn(usize) -> bool, show_empty: bool, rows: &mut Vec<TreeRow>, count_under: &dyn Fn(&str) -> usize) {
        for g in groups.iter().filter(|g| crate::hosttree::parent_of(&g.path) == parent) {
            let n = count_under(&g.path);
            if n == 0 && !show_empty { continue; }
            rows.push(TreeRow::Group { path: g.path.clone(), depth, count: n });
            if collapsed.contains(&g.path) { continue; }
            walk(&g.path, depth + 1, groups, collapsed, item_groups, keep, show_empty, rows, count_under);
            for (i, ig) in item_groups.iter().enumerate() { if ig == &g.path && keep(i) { rows.push(TreeRow::Item { index: i, depth: depth + 1 }); } }
        }
    }
    walk("", 0, groups, collapsed, item_groups, keep, show_empty, &mut rows, &count_under);
    for (i, ig) in item_groups.iter().enumerate() {
        let known = !ig.is_empty() && groups.iter().any(|g| &g.path == ig);
        if !known && keep(i) { rows.push(TreeRow::Item { index: i, depth: 0 }); }
    }
    rows
}

/// Group path of an SSH host entry (same normalization as the host tree).
pub fn host_group(h: &HostEntry) -> String { h.group.as_deref().map(normalize).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_parsing() {
        let (c, p) = parse_uri("mysql://app:p%40ss@10.0.3.12:3307/orders?ssl=1").unwrap();
        assert_eq!((c.kind, c.user.as_str(), c.host.as_str(), c.port, c.database.as_str()), (DbKind::Mysql, "app", "10.0.3.12", Some(3307), "orders"));
        assert_eq!(p.as_deref(), Some("p@ss"));
        let (c, p) = parse_uri("postgresql://db.local/app").unwrap();
        assert_eq!((c.kind, c.host.as_str(), c.port, c.database.as_str(), p), (DbKind::Postgres, "db.local", None, "app", None));
        let (c, _) = parse_uri("postgres://u@[::1]:6543").unwrap();
        assert_eq!((c.host.as_str(), c.port), ("::1", Some(6543)));
        let (c, _) = parse_uri("sqlite:///home/me/a.db").unwrap();
        assert_eq!((c.kind, c.file.as_str()), (DbKind::Sqlite, "/home/me/a.db"));
        assert!(parse_uri("http://x").is_none());
        assert!(parse_uri("nonsense").is_none());
    }

    #[test]
    fn kinds_roundtrip_and_ports() {
        for k in DbKind::ALL { assert_eq!(DbKind::from_id(k.id()), Some(k)); }
        assert_eq!(DbKind::Postgres.default_port(), Some(5432));
        assert!(DbKind::Sqlite.is_file() && DbKind::Sqlite.default_port().is_none());
        assert!(DbKind::Mysql.driver_ready() && DbKind::Opengauss.driver_ready() && !DbKind::Mongodb.driver_ready());
        let c = DbConn { kind: DbKind::Mysql, host: "h".into(), user: "u".into(), database: "d".into(), ..Default::default() };
        assert_eq!(c.address(), "u@h:3306/d");
    }

    #[test]
    fn normalize_assigns_ids_and_names() {
        let mut v = vec![DbConn { kind: DbKind::Sqlite, file: "/x/y/app.db".into(), ..Default::default() }, DbConn { id: "a".into(), name: "n".into(), ..Default::default() }, DbConn { id: "a".into(), name: "m".into(), ..Default::default() }];
        assert!(normalize_dbs(&mut v));
        assert_eq!(v[0].name, "app.db");
        assert_eq!(v[0].id.len(), 12);
        assert_eq!(v[1].id, "a");
        assert_ne!(v[2].id, "a");
        assert!(!normalize_dbs(&mut v));
    }

    #[test]
    fn groups_shared_and_tags_inherited() {
        let mut groups = vec![GroupEntry { path: "生产".into(), tags: vec!["red".into()] }];
        let dbs = vec![DbConn { id: "1".into(), name: "a".into(), group: Some("生产/华东".into()), ..Default::default() }, DbConn { id: "2".into(), name: "b".into(), ..Default::default() }];
        ensure_asset_groups(&dbs, &mut groups);
        assert_eq!(groups.iter().map(|g| g.path.as_str()).collect::<Vec<_>>(), vec!["生产", "生产/华东"]);
        assert!(db_is_production(&dbs[0], &groups, "red"));
        assert!(!db_is_production(&dbs[1], &groups, "red"));
        let ig: Vec<String> = dbs.iter().map(|d| d.group_path()).collect();
        let rows = tree_rows(&groups, &[], &ig, &|_| true, false);
        assert_eq!(rows, vec![
            TreeRow::Group { path: "生产".into(), depth: 0, count: 1 },
            TreeRow::Group { path: "生产/华东".into(), depth: 1, count: 1 },
            TreeRow::Item { index: 0, depth: 2 },
            TreeRow::Item { index: 1, depth: 0 },
        ]);
        let rows = tree_rows(&groups, &["生产".into()], &ig, &|_| true, false);
        assert_eq!(rows.len(), 2);
        let rows = tree_rows(&groups, &[], &ig, &|i| i == 1, false);
        assert_eq!(rows, vec![TreeRow::Item { index: 1, depth: 0 }]);
        let mut d2 = dbs.clone();
        regroup_dbs(&mut d2, "生产", "线上");
        assert_eq!(d2[0].group.as_deref(), Some("线上/华东"));
    }

    #[test]
    fn config_roundtrip_skips_empty() {
        let c = DbConn { id: "x".into(), name: "n".into(), kind: DbKind::Postgres, host: "h".into(), ..Default::default() };
        let s = toml::to_string(&c).unwrap();
        assert!(!s.contains("file") && !s.contains("read_only") && s.contains("kind = \"postgres\""));
        let back: DbConn = toml::from_str(&s).unwrap();
        assert_eq!(back, c);
    }
}

/// Tags of any asset in group `group` (own tags plus its groups' tags) include `prod_tag`.
pub fn is_production(tags: &[String], group: &str, groups: &[GroupEntry], prod_tag: &str) -> bool {
    tags.iter().any(|t| t == prod_tag) || groups.iter().any(|e| !group.is_empty() && crate::hosttree::under(group, &e.path) && e.tags.iter().any(|t| t == prod_tag))
}
