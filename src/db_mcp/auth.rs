//! Grant resolution for one connection.
//!
//! The most specific row wins: table, then database (DM stores the schema in that slot),
//! then the connection. A hidden connection stays hidden even if a table row is more
//! generous — the connection has to be raised first, and the same rule applies to a
//! hidden database. A read-only connection never rises above read.

use serde::{Deserialize, Serialize};

use super::sql::{Analysis, Kind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Hidden,
    #[default]
    Read,
    Write,
    Ddl,
}

impl Level {
    pub fn label_key(self) -> &'static str {
        match self {
            Level::Hidden => "mcp.level_hidden",
            Level::Read => "mcp.level_read",
            Level::Write => "mcp.level_write",
            Level::Ddl => "mcp.level_ddl",
        }
    }
    pub fn cycle(self) -> Self {
        match self {
            Level::Hidden => Level::Read,
            Level::Read => Level::Write,
            Level::Write => Level::Ddl,
            Level::Ddl => Level::Hidden,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grant {
    pub conn: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub database: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub table: String,
    pub level: Level,
    /// Skip the confirmation dialog for writes to this table. Only a table row can set it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

impl Default for Grant {
    fn default() -> Self {
        Self { conn: String::new(), database: String::new(), schema: String::new(), table: String::new(), level: Level::Read, auto: false }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnPolicy {
    pub fallback: Level,
    pub read_only: bool,
    pub grants: Vec<Grant>,
}

pub fn policy_for(grants: &[Grant], conn_id: &str, read_only: bool, production: bool) -> ConnPolicy {
    ConnPolicy {
        fallback: if production { Level::Hidden } else { Level::Read },
        read_only,
        grants: grants.iter().filter(|g| g.conn == conn_id).cloned().collect(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deny { Hidden, Beyond, NoWhere }

impl Deny {
    pub fn message(self) -> &'static str {
        match self {
            Deny::Hidden => "object is not visible",
            Deny::Beyond => "not allowed by the grant",
            Deny::NoWhere => "UPDATE/DELETE without WHERE is blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision { pub confirm: bool }

/// Level that applies to `database.table`. Empty `table` is the database; both empty is the connection.
pub fn resolve(policy: &ConnPolicy, database: &str, table: &str) -> (Level, bool) {
    let (conn, _) = effective(policy, "", "");
    if conn == Level::Hidden { return (Level::Hidden, false); }
    if !database.is_empty() {
        let (db, _) = effective(policy, database, "");
        if db == Level::Hidden { return (Level::Hidden, false); }
    }
    if table.is_empty() { return effective(policy, database, ""); }
    effective(policy, database, table)
}

pub fn exposed(policy: &ConnPolicy) -> bool { resolve(policy, "", "").0 > Level::Hidden }

pub fn authorize(a: &Analysis, policy: &ConnPolicy, current_db: &str) -> Result<Decision, Deny> {
    if !a.where_ok { return Err(Deny::NoWhere); }
    let need_kind = match a.kind {
        Kind::Select => Level::Read,
        Kind::Dml | Kind::Delete => Level::Write,
        Kind::Ddl => Level::Ddl,
    };
    if a.tables.is_empty() {
        let (lvl, _) = resolve(policy, "", "");
        if lvl < need_kind { return Err(if lvl == Level::Hidden { Deny::Hidden } else { Deny::Beyond }); }
        return Ok(Decision { confirm: need_kind > Level::Read });
    }
    let any_written = a.tables.iter().any(|t| t.written);
    let mut confirm = need_kind > Level::Read;
    let mut saw_write = false;
    let mut all_auto = true;
    for t in &a.tables {
        let db = if t.qualifier.is_empty() { current_db } else { t.qualifier.as_str() };
        let (lvl, auto) = resolve(policy, db, &t.name);
        let force_ddl = a.kind == Kind::Ddl && !any_written;
        let written = t.written || force_ddl;
        let need = if force_ddl { Level::Ddl } else if written { need_kind } else { Level::Read };
        if lvl < need { return Err(if lvl == Level::Hidden { Deny::Hidden } else { Deny::Beyond }); }
        if written && need > Level::Read {
            saw_write = true;
            if !auto { all_auto = false; }
        }
    }
    if confirm && saw_write && all_auto { confirm = false; }
    Ok(Decision { confirm })
}

/// Most specific matching grant. Empty fields are wildcards. A table row with no database is ignored.
fn effective(policy: &ConnPolicy, database: &str, table: &str) -> (Level, bool) {
    let mut best: Option<(&Grant, u8)> = None;
    for g in &policy.grants {
        if !g.schema.is_empty() { continue; }
        if !g.table.is_empty() && g.database.is_empty() { continue; }
        let db_ok = g.database.is_empty() || g.database.eq_ignore_ascii_case(database);
        let tb_ok = g.table.is_empty() || g.table.eq_ignore_ascii_case(table);
        if !(db_ok && tb_ok) { continue; }
        let spec = u8::from(!g.database.is_empty()) + u8::from(!g.table.is_empty());
        if best.as_ref().is_none_or(|(_, s)| spec > *s) { best = Some((g, spec)); }
    }
    let (mut level, auto) = match best {
        Some((g, _)) => (g.level, !g.table.is_empty() && g.auto),
        None => (policy.fallback, false),
    };
    if policy.read_only && level > Level::Read { level = Level::Read; }
    (level, auto && level >= Level::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_mcp::sql::TableUse;

    fn grant(db: &str, table: &str, level: Level, auto: bool) -> Grant {
        Grant { conn: "c".into(), database: db.into(), schema: String::new(), table: table.into(), level, auto }
    }
    fn policy(grants: Vec<Grant>, read_only: bool, production: bool) -> ConnPolicy {
        policy_for(&grants, "c", read_only, production)
    }
    fn stmt(kind: Kind, tables: Vec<TableUse>, where_ok: bool) -> Analysis {
        Analysis { kind, tables, where_ok, explain: false, explain_analyze: false, where_sql: None, insert_rows: None }
    }
    fn tbl(name: &str, written: bool) -> TableUse { TableUse { qualifier: String::new(), name: name.into(), written } }

    #[test]
    fn grants_and_defaults() {
        let read = policy(vec![], false, false);
        assert!(exposed(&read));
        assert_eq!(resolve(&read, "app", "users").0, Level::Read);
        let prod = policy(vec![], false, true);
        assert!(!exposed(&prod));
        assert_eq!(resolve(&prod, "app", "users").0, Level::Hidden);
        let raised = policy(vec![grant("", "", Level::Read, false)], false, true);
        assert!(exposed(&raised));

        let hidden_conn = policy(vec![grant("", "", Level::Hidden, false), grant("app", "users", Level::Ddl, true)], false, false);
        assert!(!exposed(&hidden_conn));
        assert_eq!(resolve(&hidden_conn, "app", "users").0, Level::Hidden);

        let hidden_db = policy(vec![grant("app", "", Level::Hidden, false), grant("app", "users", Level::Write, true)], false, false);
        assert!(exposed(&hidden_db));
        assert_eq!(resolve(&hidden_db, "app", "").0, Level::Hidden);
        assert_eq!(resolve(&hidden_db, "app", "users").0, Level::Hidden);

        let table = policy(vec![grant("app", "users", Level::Write, false)], false, false);
        assert_eq!(resolve(&table, "app", "users").0, Level::Write);
        assert_eq!(resolve(&table, "app", "other").0, Level::Read);

        let capped = policy(vec![grant("app", "users", Level::Ddl, true)], true, false);
        assert_eq!(resolve(&capped, "app", "users").0, Level::Read);
    }

    #[test]
    fn authorize_respects_kind_where_and_auto() {
        let read = policy(vec![grant("app", "t", Level::Read, false)], false, false);
        let upd = stmt(Kind::Dml, vec![tbl("t", true)], true);
        assert_eq!(authorize(&upd, &read, "app").err(), Some(Deny::Beyond));
        assert!(authorize(&stmt(Kind::Select, vec![tbl("t", false)], true), &read, "app").unwrap().confirm == false);

        let write = policy(vec![grant("app", "t", Level::Write, false)], false, false);
        assert_eq!(authorize(&stmt(Kind::Ddl, vec![tbl("t", true)], true), &write, "app").err(), Some(Deny::Beyond));
        let ok = authorize(&upd, &write, "app").unwrap();
        assert!(ok.confirm);

        let auto = policy(vec![grant("app", "t", Level::Write, true)], false, false);
        assert!(!authorize(&upd, &auto, "app").unwrap().confirm);
        // A database-level auto flag does not skip the dialog.
        let inherited = policy(vec![grant("app", "", Level::Write, true)], false, false);
        assert!(authorize(&upd, &inherited, "app").unwrap().confirm);

        let ddl = policy(vec![grant("app", "t", Level::Ddl, false)], false, false);
        assert_eq!(authorize(&stmt(Kind::Delete, vec![tbl("t", true)], false), &ddl, "app").err(), Some(Deny::NoWhere));
        assert_eq!(authorize(&stmt(Kind::Dml, vec![tbl("t", true)], false), &ddl, "app").err(), Some(Deny::NoWhere));

        let hidden = policy(vec![grant("app", "secret", Level::Hidden, false)], false, false);
        assert_eq!(authorize(&stmt(Kind::Select, vec![tbl("secret", false)], true), &hidden, "app").err(), Some(Deny::Hidden));

        let ro = policy(vec![grant("app", "t", Level::Ddl, true)], true, false);
        assert_eq!(authorize(&upd, &ro, "app").err(), Some(Deny::Beyond));
    }
}
