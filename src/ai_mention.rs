//! `@` references in the built-in assistant. A mention names a database connection,
//! one database, or one table that the MCP backend already exposes. The snapshot is
//! schema only. A later read-only SELECT uses the same gate as the MCP `query` tool.
use std::collections::HashMap;
use std::ops::Range;

use ssh_term_proto::db_mcp::{analyze, authorize, resolve, AuditEvent, Backend, Kind, Level, PublicConn};

/// What the `@` menu is offering.
#[derive(Clone)]
pub enum MentionPick {
    Conn(PublicConn),
    Db { id: String, name: String, database: String },
    Table { id: String, name: String, database: String, table: String },
}

/// Open `@` menu. `at` is the byte offset of `@` in the composer.
#[derive(Clone)]
pub struct MentionMenu {
    pub at: usize,
    pub query: String,
    pub items: Vec<MentionPick>,
    pub sel: usize,
    pub loading: bool,
}

/// Next step for the menu: show rows, or load databases / tables first.
pub enum MentionPlan { Ready(Vec<MentionPick>), NeedDbs(String), NeedTables(String, String) }

/// One referenced object. A bare connection id (older tokens) is the whole connection.
#[derive(Clone, Debug, PartialEq)]
pub struct DbRef { pub conn: String, pub database: Option<String>, pub table: Option<String> }

/// One read the model asked for. `connection` is an id or a saved name.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadReq { pub connection: String, pub database: String, pub sql: String }

use crate::ai_ctx;

/// `@query` being typed at `cursor` (UTF-8 bytes). `None` when the caret is inside a
/// token, the `@` is part of a word, or the query already contains whitespace.
pub fn mention_at(text: &str, cursor: usize, tokens: &[Range<usize>]) -> Option<(usize, String)> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) { return None; }
    if tokens.iter().any(|r| cursor > r.start && cursor < r.end) { return None; }
    let mut from = cursor;
    while from > 0 {
        let Some(at) = text[..from].rfind('@') else { return None };
        if tokens.iter().any(|r| at >= r.start && at < r.end) { from = at; continue; }
        if at > 0 && !text[..at].chars().next_back().is_some_and(|c| c.is_whitespace()) { return None; }
        let query = &text[at + '@'.len_utf8()..cursor];
        if query.chars().any(|c| c.is_whitespace()) { return None; }
        return Some((at, query.to_string()));
    }
    None
}

/// Exposed connections whose name or default database contains `query`.
pub fn filter_conns(conns: &[PublicConn], query: &str) -> Vec<PublicConn> {
    let q = query.trim().to_lowercase();
    conns.iter().filter(|c| q.is_empty() || c.name.to_lowercase().contains(&q) || c.database.to_lowercase().contains(&q)).take(8).cloned().collect()
}

/// `@name`, `@name/db`, or `@name/db/table`. A slash with an unambiguous connection
/// asks for that connection's databases or tables; otherwise the menu stays on connections.
pub fn mention_plan(conns: &[PublicConn], query: &str, dbs: &HashMap<String, Vec<String>>, tables: &HashMap<String, Vec<String>>) -> MentionPlan {
    if !query.contains('/') {
        return MentionPlan::Ready(filter_conns(conns, query).into_iter().map(MentionPick::Conn).collect());
    }
    let mut parts = query.splitn(3, '/');
    let conn_q = parts.next().unwrap_or("");
    let db_q = parts.next().unwrap_or("");
    let table_q = parts.next();
    let exact: Vec<_> = conns.iter().filter(|c| c.name.eq_ignore_ascii_case(conn_q)).cloned().collect();
    if exact.len() != 1 {
        return MentionPlan::Ready(filter_conns(conns, conn_q).into_iter().map(MentionPick::Conn).collect());
    }
    let conn = exact.into_iter().next().unwrap();
    let Some(db_list) = dbs.get(&conn.id) else { return MentionPlan::NeedDbs(conn.id); };
    if table_q.is_none() {
        let q = db_q.to_lowercase();
        return MentionPlan::Ready(db_list.iter().filter(|d| q.is_empty() || d.to_lowercase().contains(&q)).take(8).map(|d| MentionPick::Db { id: conn.id.clone(), name: conn.name.clone(), database: d.clone() }).collect());
    }
    let db = db_q.to_string();
    if db.is_empty() {
        return MentionPlan::Ready(db_list.iter().take(8).map(|d| MentionPick::Db { id: conn.id.clone(), name: conn.name.clone(), database: d.clone() }).collect());
    }
    let key = format!("{}\u{1f}{db}", conn.id);
    let Some(tb_list) = tables.get(&key) else { return MentionPlan::NeedTables(conn.id, db); };
    let q = table_q.unwrap_or("").to_lowercase();
    MentionPlan::Ready(tb_list.iter().filter(|t| q.is_empty() || t.to_lowercase().contains(&q)).take(8).map(|t| MentionPick::Table { id: conn.id.clone(), name: conn.name.clone(), database: db.clone(), table: t.clone() }).collect())
}

/// Token id for a connection, `db:<id>`, or the same plus database and table separated by U+001F.
pub fn encode_ref(conn: &str, database: Option<&str>, table: Option<&str>) -> String {
    match (database, table) {
        (Some(db), Some(tb)) => format!("db:{conn}\u{1f}{db}\u{1f}{tb}"),
        (Some(db), None) => format!("db:{conn}\u{1f}{db}"),
        _ => format!("db:{conn}"),
    }
}

pub fn parse_ref(id: &str) -> DbRef {
    if let Some(rest) = id.strip_prefix("db:") {
        let mut p = rest.split('\u{1f}');
        DbRef { conn: p.next().unwrap_or("").into(), database: p.next().filter(|s| !s.is_empty()).map(str::to_string), table: p.next().filter(|s| !s.is_empty()).map(str::to_string) }
    } else {
        DbRef { conn: id.into(), database: None, table: None }
    }
}

/// Label stored on the chip, without the leading `@`.
pub fn pick_label(pick: &MentionPick) -> (String, String) {
    match pick {
        MentionPick::Conn(c) => {
            let name = if c.name.trim().is_empty() { c.id.clone() } else { c.name.clone() };
            (encode_ref(&c.id, None, None), name)
        }
        MentionPick::Db { id, name, database } => (encode_ref(id, Some(database), None), format!("{name}/{database}")),
        MentionPick::Table { id, name, database, table } => (encode_ref(id, Some(database), Some(table)), format!("{name}/{database}/{table}")),
    }
}

/// Pull one ```read {json} ``` block out of a reply. The rest is what the user should see.
pub fn take_read_block(text: &str) -> Option<(String, ReadReq)> {
    let start = text.find("```read")?;
    let after = text[start + "```read".len()..].trim_start_matches(['\r', '\n']);
    let end = after.find("```")?;
    let body = after[..end].trim();
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let connection = v.get("connection")?.as_str().unwrap_or("").trim().to_string();
    let database = v.get("database").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let sql = v.get("sql")?.as_str().unwrap_or("").trim().to_string();
    if connection.is_empty() || sql.is_empty() { return None; }
    let mut shown = text[..start].trim_end().to_string();
    let rest = after[end + 3..].trim();
    if !rest.is_empty() {
        if !shown.is_empty() { shown.push_str("\n\n"); }
        shown.push_str(rest);
    }
    Some((shown, ReadReq { connection, database, sql }))
}

/// Run one SELECT through the MCP read gate. Writes, multiple statements and hidden objects are refused.
pub fn run_read(b: &dyn Backend, req: &ReadReq) -> Result<String, String> {
    let view = b.prepare(&req.connection).map_err(|e| e)?;
    let db = if req.database.is_empty() { view.database.clone() } else { req.database.clone() };
    let analysis = match analyze(&req.sql, view.engine) {
        Ok(a) => a,
        Err(e) => { b.audit(&audit_sql(&view.id, &req.sql, 0, "deny")); return Err(e.message().into()); }
    };
    if analysis.kind != Kind::Select || analysis.explain_analyze {
        b.audit(&audit_sql(&view.id, &req.sql, 0, "deny"));
        return Err("only a single read-only SELECT is allowed".into());
    }
    if let Err(d) = authorize(&analysis, &view.policy, &db) {
        b.audit(&audit_sql(&view.id, &req.sql, 0, "deny"));
        return Err(d.message().into());
    }
    let limits = b.limits();
    let max = limits.max_rows.min(50);
    match b.run_sql(&view.id, &db, &req.sql, max, limits.timeout) {
        Ok(out) => {
            if let Some(err) = out.sets.iter().find_map(|s| s.error.clone()) {
                b.audit(&audit_sql(&view.id, &req.sql, 0, "deny"));
                return Err(ssh_term_proto::db_mcp::scrub(&err));
            }
            b.audit(&audit_sql(&view.id, &req.sql, out.row_count, "allow"));
            Ok(format_rows(&out))
        }
        Err(e) => { b.audit(&audit_sql(&view.id, &req.sql, 0, "deny")); Err(ssh_term_proto::db_mcp::scrub(&e)) }
    }
}

fn format_rows(out: &ssh_term_proto::db_mcp::RunOut) -> String {
    let Some(set) = out.sets.first() else { return format!("{} rows", out.row_count); };
    let mut text = set.columns.join(" | ");
    for row in set.rows.iter().take(20) {
        text.push('\n');
        text.push_str(&row.iter().map(|c| c.as_deref().unwrap_or("NULL").chars().take(200).collect::<String>()).collect::<Vec<_>>().join(" | "));
    }
    if set.truncated || set.rows.len() > 20 { text.push_str("\n…"); }
    text
}

fn audit_sql(conn: &str, sql: &str, rows: u64, effect: &str) -> AuditEvent {
    AuditEvent { client: "ai".into(), tool: "query".into(), conn: conn.into(), rows, sql: sql.chars().take(8000).collect(), effect: effect.into() }
}

/// Read-only schema of one exposed connection, database, or table. Hidden objects are left out.
pub fn schema_snapshot(b: &dyn Backend, id: &str) -> Result<String, ()> {
    let target = parse_ref(id);
    let view = b.prepare(&target.conn).map_err(|_| ())?;
    let mut out = format!(
        "Id: {}\nConnection: {} ({:?}) default database {}{}{}\n",
        view.id,
        view.name,
        view.engine,
        if view.database.is_empty() { "(none)" } else { view.database.as_str() },
        if view.policy.read_only { ", read-only" } else { "" },
        if view.policy.fallback == Level::Hidden { ", production" } else { "" },
    );
    if let Some(table) = &target.table {
        let db = target.database.clone().unwrap_or_else(|| view.database.clone());
        if db.is_empty() || resolve(&view.policy, &db, table).0 <= Level::Hidden { b.audit(&audit(&view.id, 0, "deny")); return Err(()); }
        out.push_str(&format!("Table {db}.{table}:\n"));
        match b.columns(&view.id, &db, table) {
            Ok(cols) => {
                let cs: Vec<String> = cols.iter().map(|c| format!("{} {}", c.name, c.type_name)).collect();
                out.push_str(&format!("{table}({})\n", cs.join(", ")));
                b.audit(&audit(&view.id, 1, "allow"));
                return Ok(out);
            }
            Err(e) => { out.push_str(&format!("Columns unavailable: {e}\n")); b.audit(&audit(&view.id, 0, "deny")); return Ok(out); }
        }
    }
    if let Some(only) = &target.database {
        if resolve(&view.policy, only, "").0 <= Level::Hidden { b.audit(&audit(&view.id, 0, "deny")); return Err(()); }
    }
    let visible_db = match b.databases(&view.id) {
        Ok(v) => v.into_iter().filter(|d| resolve(&view.policy, d, "").0 > Level::Hidden).collect::<Vec<_>>(),
        Err(e) => { out.push_str(&format!("Databases unavailable: {e}\n")); b.audit(&audit(&view.id, 0, "deny")); return Ok(out); }
    };
    out.push_str(&format!("Databases: {}\n", if visible_db.is_empty() { "(none)".into() } else { visible_db.join(", ") }));
    let db = if let Some(only) = &target.database {
        only.clone()
    } else if !view.database.is_empty() && resolve(&view.policy, &view.database, "").0 > Level::Hidden {
        view.database.clone()
    } else {
        visible_db.first().cloned().unwrap_or_default()
    };
    if db.is_empty() { b.audit(&audit(&view.id, 0, "allow")); return Ok(out); }
    let tables = match b.tables(&view.id, &db) {
        Ok(v) => v.into_iter().filter(|t| resolve(&view.policy, &db, t).0 > Level::Hidden).collect::<Vec<_>>(),
        Err(e) => { out.push_str(&format!("Tables unavailable: {e}\n")); b.audit(&audit(&view.id, 0, "deny")); return Ok(out); }
    };
    out.push_str(&format!("Tables in {db} ({}):\n", tables.len()));
    let mut schema = String::new();
    for (i, name) in tables.iter().enumerate() {
        if i < 40 {
            match b.columns(&view.id, &db, name) {
                Ok(cols) => {
                    let cs: Vec<String> = cols.iter().map(|c| format!("{} {}", c.name, c.type_name)).collect();
                    schema.push_str(&format!("{name}({})\n", cs.join(", ")));
                }
                Err(_) => schema.push_str(&format!("{name}\n")),
            }
        } else {
            schema.push_str(&format!("{name}\n"));
        }
        if schema.len() > 12_000 { break; }
    }
    out.push_str(&ai_ctx::cap(schema, 12_000));
    b.audit(&audit(&view.id, tables.len() as u64, "allow"));
    Ok(out)
}

fn audit(conn: &str, rows: u64, effect: &str) -> AuditEvent {
    AuditEvent { client: "ai".into(), tool: "describe_schema".into(), conn: conn.into(), rows, sql: String::new(), effect: effect.into() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_term_proto::db_mcp::{ColumnOut, ConfirmReq, ConnPolicy, ConnView, Engine, Grant, Level, Limits, RunOut, SetOut};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn mention_query_stops_at_a_word_and_at_tokens() {
        assert_eq!(mention_at("@ord", 4, &[]), Some((0, "ord".into())));
        assert_eq!(mention_at("see @订单", "see @订单".len(), &[]).map(|(_, q)| q), Some("订单".into()));
        assert_eq!(mention_at("mail@host", 9, &[]), None);
        assert_eq!(mention_at("@foo bar", 8, &[]), None);
        assert_eq!(mention_at("@alice hi @b", 12, &[0..6]), Some((10, "b".into())));
        assert_eq!(mention_at("@alice", 3, &[0..6]), None);
    }

    #[test]
    fn filter_matches_name_or_database() {
        let c = |name: &str, db: &str| PublicConn { id: name.into(), name: name.into(), kind: "mysql".into(), database: db.into(), read_only: false, production: false, engine: Engine::Mysql };
        let all = vec![c("orders", "shop"), c("logs", "audit")];
        assert_eq!(filter_conns(&all, "shop").len(), 1);
        assert_eq!(filter_conns(&all, "").len(), 2);
        assert!(filter_conns(&all, "nope").is_empty());
    }

    struct Fake { audits: Mutex<Vec<String>> }
    impl Backend for Fake {
        fn limits(&self) -> Limits { Limits { max_rows: 10, timeout: Duration::from_secs(1) } }
        fn connections(&self) -> Vec<PublicConn> { vec![] }
        fn prepare(&self, id: &str) -> Result<ConnView, String> {
            if id != "c1" { return Err("connection not found".into()); }
            Ok(ConnView {
                id: "c1".into(), name: "orders".into(), database: "shop".into(), engine: Engine::Mysql,
                policy: ConnPolicy {
                    fallback: Level::Read, read_only: true,
                    grants: vec![Grant { conn: "c1".into(), database: "secret".into(), schema: String::new(), table: String::new(), level: Level::Hidden, auto: false }],
                },
            })
        }
        fn databases(&self, _: &str) -> Result<Vec<String>, String> { Ok(vec!["shop".into(), "secret".into()]) }
        fn tables(&self, _: &str, db: &str) -> Result<Vec<String>, String> {
            assert_ne!(db, "secret");
            Ok(vec!["users".into()])
        }
        fn columns(&self, _: &str, _: &str, table: &str) -> Result<Vec<ColumnOut>, String> {
            Ok(vec![ColumnOut { name: "id".into(), type_name: "int".into(), nullable: Some(false), comment: String::new() }, ColumnOut { name: "email".into(), type_name: "varchar".into(), nullable: Some(true), comment: String::new() }].into_iter().filter(|c| table == "users" || c.name == "id").collect())
        }
        fn run_sql(&self, _: &str, _: &str, sql: &str, _: u32, _: Duration) -> Result<RunOut, String> {
            self.audits.lock().unwrap().push(format!("sql:{sql}"));
            Ok(RunOut { row_count: 1, sets: vec![SetOut { columns: vec!["n".into()], rows: vec![vec![Some("1".into())]], affected: 0, truncated: false, error: None }] })
        }
        fn confirm(&self, _: ConfirmReq) -> bool { false }
        fn audit(&self, ev: &AuditEvent) { self.audits.lock().unwrap().push(ev.effect.clone()); }
    }

    #[test]
    fn snapshot_lists_visible_tables_and_skips_hidden_databases() {
        let b = Fake { audits: Mutex::new(vec![]) };
        let text = schema_snapshot(&b, "c1").unwrap();
        assert!(text.contains("users(id int, email varchar)"), "{text}");
        assert!(text.contains("Databases: shop\n"), "{text}");
        assert!(!text.contains("secret"), "{text}");
        assert_eq!(b.audits.lock().unwrap().last().map(String::as_str), Some("allow"));
        assert!(schema_snapshot(&b, "missing").is_err());
        let table = schema_snapshot(&b, &encode_ref("c1", Some("shop"), Some("users"))).unwrap();
        assert!(table.contains("Table shop.users:"), "{table}");
        assert!(!table.contains("Databases:"), "{table}");
    }

    #[test]
    fn slash_opens_databases_then_tables() {
        let c = |id: &str, name: &str| PublicConn { id: id.into(), name: name.into(), kind: "mysql".into(), database: "shop".into(), read_only: false, production: false, engine: Engine::Mysql };
        let conns = vec![c("c1", "orders"), c("c2", "logs")];
        let mut dbs = HashMap::new();
        dbs.insert("c1".into(), vec!["shop".into(), "other".into()]);
        let mut tables = HashMap::new();
        tables.insert("c1\u{1f}shop".into(), vec!["users".into(), "orders".into()]);
        match mention_plan(&conns, "ord", &dbs, &tables) { MentionPlan::Ready(v) => assert_eq!(v.len(), 1), _ => panic!("expected connections") }
        match mention_plan(&conns, "orders/", &HashMap::new(), &HashMap::new()) { MentionPlan::NeedDbs(id) => assert_eq!(id, "c1"), _ => panic!("expected database load") }
        match mention_plan(&conns, "orders/sh", &dbs, &tables) {
            MentionPlan::Ready(v) => match &v[0] { MentionPick::Db { database, .. } => assert_eq!(database, "shop"), _ => panic!("expected a database") },
            _ => panic!("expected databases"),
        }
        match mention_plan(&conns, "orders/shop/", &dbs, &tables) {
            MentionPlan::Ready(v) => assert!(matches!(&v[0], MentionPick::Table { table, .. } if table == "users")),
            _ => panic!("expected tables"),
        }
    }

    #[test]
    fn read_block_runs_select_and_refuses_writes() {
        let reply = "Looking at the counts.\n```read\n{\"connection\":\"c1\",\"database\":\"shop\",\"sql\":\"SELECT 1\"}\n```\n";
        let (shown, req) = take_read_block(reply).unwrap();
        assert!(!shown.contains("```read"), "{shown}");
        assert_eq!(req.sql, "SELECT 1");
        let b = Fake { audits: Mutex::new(vec![]) };
        let text = run_read(&b, &req).unwrap();
        assert!(text.contains('1'), "{text}");
        assert!(run_read(&b, &ReadReq { connection: "c1".into(), database: "shop".into(), sql: "UPDATE users SET email = 'a'".into() }).is_err());
    }
}
