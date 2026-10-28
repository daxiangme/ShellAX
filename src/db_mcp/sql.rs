//! Parse one statement and decide what it is allowed to do.
//!
//! MySQL-family connections use the MySQL dialect. DM uses the generic ANSI dialect.
//! Anything the parser cannot read is rejected. A second statement is rejected.
//! `UPDATE` / `DELETE` without a `WHERE` is rejected later by authorization, but this
//! module is what records that the clause is missing.

use std::collections::HashSet;
use std::ops::ControlFlow;

use sqlparser::ast::{self, ObjectName, Query, SetExpr, Statement, TableFactor, Visit, Visitor};
use sqlparser::dialect::{Dialect, GenericDialect, MySqlDialect};
use sqlparser::parser::Parser;

use crate::assets::Family;
use crate::db::dialect;

/// Which parser to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine { Mysql, Dameng }

impl Engine {
    pub fn family(self) -> Family {
        match self { Engine::Mysql => Family::Mysql, Engine::Dameng => Family::Dameng }
    }
}

/// What the statement is allowed to do. Nested writes raise a `SELECT` to the write they contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Select, Dml, Delete, Ddl }

fn rank(k: Kind) -> u8 {
    match k { Kind::Select => 0, Kind::Dml => 1, Kind::Delete => 2, Kind::Ddl => 3 }
}

/// A table the statement reads or writes. `qualifier` is the database (MySQL) or schema (DM).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableUse {
    pub qualifier: String,
    pub name: String,
    pub written: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub kind: Kind,
    pub tables: Vec<TableUse>,
    /// False when any `UPDATE` or `DELETE` outside a plain `EXPLAIN` has no `WHERE`.
    pub where_ok: bool,
    /// The statement is `EXPLAIN` / `DESCRIBE` of another statement (not `EXPLAIN ANALYZE`).
    pub explain: bool,
    /// `EXPLAIN ANALYZE` — the engine will run the inner statement.
    pub explain_analyze: bool,
    /// `WHERE` text of a top-level `UPDATE` / `DELETE`, for a `COUNT(*)` estimate.
    pub where_sql: Option<String>,
    /// Row count of a top-level `INSERT ... VALUES`, when it is a literal list.
    pub insert_rows: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlError { Empty, Multi, Unparseable, Unsupported }

impl SqlError {
    pub fn message(&self) -> &'static str {
        match self {
            SqlError::Empty => "SQL is empty",
            SqlError::Multi => "multiple statements are not allowed",
            SqlError::Unparseable => "SQL could not be parsed",
            SqlError::Unsupported => "statement is not supported",
        }
    }
}

pub fn analyze(sql: &str, engine: Engine) -> Result<Analysis, SqlError> {
    let sql = sql.trim().trim_end_matches(';').trim();
    if sql.is_empty() { return Err(SqlError::Empty); }
    let stmts = parse(sql, engine)?;
    if stmts.is_empty() { return Err(SqlError::Empty); }
    if stmts.len() != 1 { return Err(SqlError::Multi); }
    let stmt = &stmts[0];
    let mut scan = Scan { kind: Kind::Select, plain: 0, bad: false, where_ok: true, ctes: vec![], relations: vec![] };
    let _ = stmt.visit(&mut scan);
    if scan.bad { return Err(SqlError::Unsupported); }
    let mut writes = HashSet::new();
    mark_writes(stmt, &mut writes);
    let is_cte = |qual: &str, name: &str| qual.is_empty() && scan.ctes.iter().any(|c| c.eq_ignore_ascii_case(name));
    writes.retain(|(q, n)| !is_cte(q, n));
    let mut tables = Vec::new();
    for (q, n) in scan.relations {
        if is_cte(&q, &n) { continue; }
        let written = writes.iter().any(|(a, b)| a.eq_ignore_ascii_case(&q) && b.eq_ignore_ascii_case(&n));
        push_table(&mut tables, q, n, written);
    }
    for (q, n) in writes { push_table(&mut tables, q, n, true); }
    let (explain, explain_analyze) = match stmt {
        Statement::Explain { analyze, .. } => (true, *analyze),
        _ => (false, false),
    };
    Ok(Analysis {
        kind: scan.kind,
        tables,
        where_ok: scan.where_ok,
        explain,
        explain_analyze,
        where_sql: where_sql_of(stmt),
        insert_rows: insert_rows_of(stmt),
    })
}

/// `SELECT COUNT(*)` for a single-table `UPDATE` / `DELETE` that has a `WHERE` and no joins.
pub fn count_sql(a: &Analysis, current_db: &str, engine: Engine) -> Option<String> {
    if a.explain_analyze || !matches!(a.kind, Kind::Dml | Kind::Delete) { return None; }
    let written: Vec<_> = a.tables.iter().filter(|t| t.written).collect();
    if written.len() != 1 || a.tables.iter().any(|t| !t.written) { return None; }
    let pred = a.where_sql.as_deref().filter(|s| !s.is_empty())?;
    let t = written[0];
    let db = if t.qualifier.is_empty() { current_db } else { t.qualifier.as_str() };
    let q = dialect::qualified(engine.family(), db, None, &t.name);
    Some(format!("SELECT COUNT(*) FROM {q} WHERE {pred}"))
}

fn parse(sql: &str, engine: Engine) -> Result<Vec<Statement>, SqlError> {
    // A trailing semicolon is one statement. Two statements separated by `;` stay two:
    // `parse_sql` splits on it, and we reject `len != 1` before this helper returns.
    match engine {
        Engine::Mysql => Parser::parse_sql(&MySqlDialect {} as &dyn Dialect, sql),
        Engine::Dameng => Parser::parse_sql(&GenericDialect {} as &dyn Dialect, sql),
    }.map_err(|_| SqlError::Unparseable)
}

fn kind_of(stmt: &Statement) -> Option<Kind> {
    Some(match stmt {
        Statement::Query(_) | Statement::ExplainTable { .. } => Kind::Select,
        Statement::Insert(_) | Statement::Update(_) => Kind::Dml,
        Statement::Delete(_) => Kind::Delete,
        Statement::Truncate(_) | Statement::CreateTable(_) | Statement::CreateIndex(_) | Statement::CreateView(_)
            | Statement::AlterTable(_) | Statement::Drop { .. } | Statement::RenameTable(_) => Kind::Ddl,
        _ => return None,
    })
}

struct Scan {
    kind: Kind,
    plain: u32,
    bad: bool,
    where_ok: bool,
    ctes: Vec<String>,
    relations: Vec<(String, String)>,
}

impl Visitor for Scan {
    type Break = ();

    fn pre_visit_statement(&mut self, stmt: &Statement) -> ControlFlow<()> {
        // Inside plain EXPLAIN the inner statement is not executed, so it must not raise the kind
        // or trip the missing-WHERE rule. EXPLAIN ANALYZE does execute it.
        if self.plain == 0 {
            match kind_of(stmt) {
                Some(k) => if rank(k) > rank(self.kind) { self.kind = k; },
                None if matches!(stmt, Statement::Explain { .. }) => {}
                None => self.bad = true,
            }
            match stmt {
                Statement::Update(u) if u.selection.is_none() => self.where_ok = false,
                Statement::Delete(d) if d.selection.is_none() => self.where_ok = false,
                _ => {}
            }
        }
        if matches!(stmt, Statement::Explain { analyze: false, .. }) { self.plain += 1; }
        ControlFlow::Continue(())
    }

    fn post_visit_statement(&mut self, stmt: &Statement) -> ControlFlow<()> {
        if matches!(stmt, Statement::Explain { analyze: false, .. }) { self.plain = self.plain.saturating_sub(1); }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
        if let Some(w) = &query.with {
            for c in &w.cte_tables { self.ctes.push(c.alias.name.value.clone()); }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        if let Some(p) = split_name(relation) { self.relations.push(p); }
        ControlFlow::Continue(())
    }
}

fn push_table(tables: &mut Vec<TableUse>, qualifier: String, name: String, written: bool) {
    if let Some(t) = tables.iter_mut().find(|t| t.qualifier.eq_ignore_ascii_case(&qualifier) && t.name.eq_ignore_ascii_case(&name)) {
        t.written |= written;
        return;
    }
    tables.push(TableUse { qualifier, name, written });
}

fn split_name(name: &ObjectName) -> Option<(String, String)> {
    let idents: Vec<&str> = name.0.iter().filter_map(|p| p.as_ident().map(|i| i.value.as_str())).collect();
    let table = idents.last()?.to_string();
    if table.is_empty() { return None; }
    let qual = if idents.len() >= 2 { idents[idents.len() - 2].to_string() } else { String::new() };
    Some((qual, table))
}

fn mark_writes(stmt: &Statement, writes: &mut HashSet<(String, String)>) {
    match stmt {
        Statement::Explain { analyze: true, statement, .. } => mark_writes(statement, writes),
        Statement::Explain { .. } => {}
        Statement::Insert(i) => {
            if let ast::TableObject::TableName(n) = &i.table { add_obj(n, writes); }
            if let Some(q) = &i.source { walk_query(q, writes); }
        }
        Statement::Update(u) => {
            add_factor(&u.table.relation, writes);
            for j in &u.table.joins { add_factor(&j.relation, writes); }
        }
        Statement::Delete(d) => {
            let from = match &d.from { ast::FromTable::WithFromKeyword(t) | ast::FromTable::WithoutKeyword(t) => t };
            for twj in from {
                add_factor(&twj.relation, writes);
                for j in &twj.joins { add_factor(&j.relation, writes); }
            }
            for n in &d.tables { add_obj(n, writes); }
        }
        Statement::Query(q) => walk_query(q, writes),
        Statement::CreateTable(c) => add_obj(&c.name, writes),
        Statement::CreateIndex(c) => add_obj(&c.table_name, writes),
        Statement::CreateView(c) => add_obj(&c.name, writes),
        Statement::AlterTable(a) => add_obj(&a.name, writes),
        Statement::Truncate(t) => { for n in &t.table_names { add_obj(&n.name, writes); } }
        Statement::Drop { object_type, names, table, .. } => {
            if let Some(t) = table { add_obj(t, writes); }
            if matches!(object_type, ast::ObjectType::Table | ast::ObjectType::View) {
                for n in names { add_obj(n, writes); }
            }
        }
        Statement::RenameTable(rs) => {
            for r in rs { add_obj(&r.old_name, writes); add_obj(&r.new_name, writes); }
        }
        _ => {}
    }
}

fn walk_query(q: &Query, writes: &mut HashSet<(String, String)>) {
    if let Some(w) = &q.with { for c in &w.cte_tables { walk_query(&c.query, writes); } }
    walk_set(&q.body, writes);
}

fn walk_set(body: &SetExpr, writes: &mut HashSet<(String, String)>) {
    match body {
        SetExpr::Query(q) => walk_query(q, writes),
        SetExpr::SetOperation { left, right, .. } => { walk_set(left, writes); walk_set(right, writes); }
        SetExpr::Insert(s) | SetExpr::Update(s) | SetExpr::Delete(s) | SetExpr::Merge(s) => mark_writes(s, writes),
        _ => {}
    }
}

fn add_factor(tf: &TableFactor, writes: &mut HashSet<(String, String)>) {
    if let TableFactor::Table { name, .. } = tf { add_obj(name, writes); }
}

fn add_obj(name: &ObjectName, writes: &mut HashSet<(String, String)>) {
    if let Some(p) = split_name(name) { writes.insert(p); }
}

fn executed<'a>(stmt: &'a Statement) -> &'a Statement {
    match stmt { Statement::Explain { analyze: true, statement, .. } => statement, _ => stmt }
}

fn where_sql_of(stmt: &Statement) -> Option<String> {
    match executed(stmt) {
        Statement::Update(u) => u.selection.as_ref().map(|e| e.to_string()),
        Statement::Delete(d) => d.selection.as_ref().map(|e| e.to_string()),
        _ => None,
    }
}

fn insert_rows_of(stmt: &Statement) -> Option<u64> {
    let Statement::Insert(i) = executed(stmt) else { return None };
    if let Some(q) = &i.source {
        if let SetExpr::Values(v) = q.body.as_ref() { return Some(v.rows.len() as u64); }
    }
    if !i.assignments.is_empty() { return Some(1); }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(sql: &str, engine: Engine) -> Analysis { analyze(sql, engine).unwrap_or_else(|e| panic!("{sql}: {e:?}")) }
    fn has(a: &Analysis, name: &str) -> bool { a.tables.iter().any(|t| t.name.eq_ignore_ascii_case(name)) }
    fn written(a: &Analysis, name: &str) -> bool { a.tables.iter().any(|t| t.name.eq_ignore_ascii_case(name) && t.written) }

    #[test]
    fn classifies_mysql_and_dm() {
        let s = ok("SELECT id FROM users WHERE id = 1", Engine::Mysql);
        assert_eq!(s.kind, Kind::Select);
        assert!(s.where_ok && has(&s, "users") && !written(&s, "users"));

        let j = ok("SELECT a.id FROM a JOIN b ON a.id = b.id", Engine::Mysql);
        assert!(has(&j, "a") && has(&j, "b") && !written(&j, "a"));

        let q = ok("SELECT * FROM `app`.`users`", Engine::Mysql);
        assert_eq!(q.tables[0].qualifier.as_str(), "app");
        assert_eq!(q.tables[0].name.as_str(), "users");

        let d = ok("SELECT * FROM \"HR\".\"EMP\"", Engine::Dameng);
        assert_eq!((d.tables[0].qualifier.as_str(), d.tables[0].name.as_str()), ("HR", "EMP"));
        assert_eq!(d.kind, Kind::Select);
    }

    #[test]
    fn dml_delete_ddl_and_where() {
        let i = ok("INSERT INTO t (a) VALUES (1), (2)", Engine::Mysql);
        assert_eq!(i.kind, Kind::Dml);
        assert_eq!(i.insert_rows, Some(2));
        assert!(written(&i, "t") && i.where_ok);

        let set = ok("INSERT INTO t SET a = 1", Engine::Mysql);
        assert_eq!(set.insert_rows, Some(1));

        let u = ok("UPDATE t SET a = 1 WHERE id = 2", Engine::Mysql);
        assert_eq!(u.kind, Kind::Dml);
        assert!(written(&u, "t") && u.where_ok && u.where_sql.is_some());

        let nw = ok("UPDATE t SET a = 1", Engine::Mysql);
        assert!(!nw.where_ok);

        let del = ok("DELETE FROM t WHERE id = 1", Engine::Dameng);
        assert_eq!(del.kind, Kind::Delete);
        assert!(written(&del, "t") && del.where_ok);

        let bare = ok("DELETE FROM t", Engine::Mysql);
        assert!(!bare.where_ok);

        let ddl = ok("ALTER TABLE t ADD COLUMN b INT", Engine::Mysql);
        assert_eq!(ddl.kind, Kind::Ddl);
        assert!(written(&ddl, "t"));
        assert_eq!(ok("DROP TABLE t", Engine::Mysql).kind, Kind::Ddl);
        assert_eq!(ok("CREATE TABLE t (id INT)", Engine::Dameng).kind, Kind::Ddl);
        assert!(written(&ok("TRUNCATE TABLE t", Engine::Mysql), "t"));
    }

    #[test]
    fn rejects_multi_unparseable_and_unknown() {
        assert_eq!(analyze("SELECT 1; SELECT 2", Engine::Mysql).err(), Some(SqlError::Multi));
        assert_eq!(analyze("DELETE FROM t; DROP TABLE t", Engine::Dameng).err(), Some(SqlError::Multi));
        assert_eq!(analyze("   ", Engine::Mysql).err(), Some(SqlError::Empty));
        assert_eq!(analyze("@@@", Engine::Mysql).err(), Some(SqlError::Unparseable));
        assert_eq!(analyze("BEGIN", Engine::Mysql).err(), Some(SqlError::Unsupported));
    }

    #[test]
    fn explain_does_not_run_the_inner_write_unless_analyze() {
        let plain = ok("EXPLAIN UPDATE t SET a = 1", Engine::Mysql);
        assert!(plain.explain && !plain.explain_analyze);
        assert_eq!(plain.kind, Kind::Select);
        assert!(plain.where_ok);
        assert!(!written(&plain, "t"));

        let analyzed = ok("EXPLAIN ANALYZE UPDATE t SET a = 1 WHERE id = 1", Engine::Mysql);
        assert!(analyzed.explain_analyze);
        assert_eq!(analyzed.kind, Kind::Dml);
        assert!(written(&analyzed, "t") && analyzed.where_ok);

        let no_where = ok("EXPLAIN ANALYZE DELETE FROM t", Engine::Mysql);
        assert_eq!(no_where.kind, Kind::Delete);
        assert!(!no_where.where_ok);
    }

    #[test]
    fn data_modifying_cte_is_a_write() {
        let sql = "WITH d AS (DELETE FROM t WHERE id = 1) SELECT * FROM d";
        let a = ok(sql, Engine::Dameng);
        assert_eq!(a.kind, Kind::Delete);
        assert!(written(&a, "t"));
        assert!(!has(&a, "d"));
        assert!(a.where_ok);
    }

    #[test]
    fn count_sql_only_for_a_single_filtered_write() {
        let u = ok("UPDATE t SET a = 1 WHERE id = 2", Engine::Mysql);
        let q = count_sql(&u, "app", Engine::Mysql).unwrap();
        assert!(q.contains("COUNT(*)") && q.contains("`t`") && q.contains("WHERE"));
        assert!(count_sql(&ok("UPDATE t SET a = 1", Engine::Mysql), "app", Engine::Mysql).is_none());
        assert!(count_sql(&ok("SELECT * FROM t", Engine::Mysql), "app", Engine::Mysql).is_none());
    }
}
