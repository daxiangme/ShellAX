//! Table structure changes (the structure designer): column / index / foreign-key edits
//! turned into per-dialect ALTER statements, plus the catalog queries that read the
//! current indexes and foreign keys back.
use anyhow::{bail, Result};

use crate::assets::Family;

use super::dialect::{quote_ident, quote_lit};
use super::ColumnInfo;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColDef {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    /// SQL expression (`'a'`, `0`, `now()`), None = no default.
    pub default: Option<String>,
    pub comment: String,
    /// Kept as-is when a MySQL column is redefined (`auto_increment`, `on update …`).
    pub extra: String,
}

impl ColDef {
    pub fn from_info(f: Family, c: &ColumnInfo) -> Self {
        let default = match f { Family::Mysql => c.default.as_deref().and_then(|d| mysql_default_expr(d, &c.extra)), _ => c.default.clone() };
        Self { name: c.name.clone(), type_name: c.type_name.clone(), nullable: c.nullable != Some(false), default, comment: c.comment.clone(), extra: c.extra.clone() }
    }
    /// Generated / identity columns: their definition is not editable here.
    pub fn generated(&self) -> bool { let e = self.extra.to_ascii_lowercase(); e.contains("generated") || e.contains("identity") }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexDef {
    pub name: String,
    /// Column names (or expressions, as the catalog prints them).
    pub columns: Vec<String>,
    pub unique: bool,
    /// Backs a constraint (PostgreSQL UNIQUE constraint): dropped as a constraint.
    pub constraint: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FkDef {
    pub name: String,
    pub columns: Vec<String>,
    /// Referenced table as written in SQL (already quoted / qualified as needed).
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    /// `NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL`, `SET DEFAULT`; empty = engine default.
    pub on_delete: String,
    pub on_update: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    AddColumn(ColDef),
    ModifyColumn { old: ColDef, new: ColDef },
    DropColumn(String),
    AddIndex(IndexDef),
    DropIndex(IndexDef),
    AddFk(FkDef),
    DropFk(String),
}

pub const FK_ACTIONS: [&str; 5] = ["NO ACTION", "RESTRICT", "CASCADE", "SET NULL", "SET DEFAULT"];

/// MySQL `COLUMN_DEFAULT` as an expression. MariaDB already quotes literals (and says
/// `NULL`); MySQL 8 gives the bare literal, or an expression when `EXTRA` has
/// `DEFAULT_GENERATED`.
pub fn mysql_default_expr(d: &str, extra: &str) -> Option<String> {
    if d.eq_ignore_ascii_case("NULL") { return None; }
    let bare_ok = d.starts_with('\'') || d.parse::<f64>().is_ok() || d.contains('(') || d.to_ascii_uppercase().starts_with("CURRENT_TIMESTAMP") || extra.to_ascii_uppercase().contains("DEFAULT_GENERATED");
    Some(if bare_ok { d.to_string() } else { quote_lit(Family::Mysql, d) })
}

fn idents(f: Family, v: &[String]) -> String { v.iter().map(|c| quote_ident(f, c)).collect::<Vec<_>>().join(", ") }

/// Index key list: plain names are quoted; expressions and already-quoted names (as the
/// catalog prints them) are kept.
fn index_keys(f: Family, v: &[String]) -> String {
    v.iter().map(|c| if c.contains('(') || c.starts_with('"') || c.starts_with('`') { c.clone() } else { quote_ident(f, c) }).collect::<Vec<_>>().join(", ")
}

fn col_sql(f: Family, c: &ColDef) -> String {
    let mut s = format!("{} {}", quote_ident(f, &c.name), c.type_name.trim());
    if !c.nullable { s.push_str(" NOT NULL"); }
    if let Some(d) = c.default.as_deref().filter(|d| !d.trim().is_empty()) { s.push_str(&format!(" DEFAULT {}", d.trim())); }
    if f == Family::Mysql {
        let e = c.extra.to_ascii_lowercase();
        if e.contains("auto_increment") { s.push_str(" AUTO_INCREMENT"); }
        if let Some(p) = e.find("on update") { s.push(' '); s.push_str(&c.extra[p..].to_ascii_uppercase()); }
        if !c.comment.is_empty() { s.push_str(&format!(" COMMENT {}", quote_lit(f, &c.comment))); }
    }
    s
}

fn fk_sql(f: Family, k: &FkDef) -> String {
    let mut s = format!("CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})", quote_ident(f, &k.name), idents(f, &k.columns), k.ref_table, idents(f, &k.ref_columns));
    if !k.on_delete.is_empty() { s.push_str(&format!(" ON DELETE {}", k.on_delete)); }
    if !k.on_update.is_empty() { s.push_str(&format!(" ON UPDATE {}", k.on_update)); }
    s
}

fn check(c: &ColDef) -> Result<()> {
    if c.name.trim().is_empty() { bail!(crate::i18n::t("db.alter_err_name").to_string()); }
    if c.type_name.trim().is_empty() { bail!(crate::i18n::tf("db.alter_err_type", &[("col", &c.name)])); }
    Ok(())
}

/// Statements for `ops` on `table` (qualified and quoted). `schema` qualifies PostgreSQL
/// index names on DROP INDEX.
pub fn statements(f: Family, table: &str, schema: Option<&str>, ops: &[Op]) -> Result<Vec<String>> {
    let at = |s: String| format!("ALTER TABLE {table} {s}");
    let mut out = vec![];
    for op in ops {
        match op {
            Op::AddColumn(c) => {
                check(c)?;
                if f == Family::Sqlite && !c.nullable && c.default.is_none() { bail!(crate::i18n::tf("db.alter_err_sqlite_notnull", &[("col", &c.name)])); }
                out.push(at(format!("ADD COLUMN {}", col_sql(f, c))));
                if f == Family::Postgres && !c.comment.is_empty() { out.push(format!("COMMENT ON COLUMN {table}.{} IS {}", quote_ident(f, &c.name), quote_lit(f, &c.comment))); }
            }
            Op::ModifyColumn { old, new } => {
                check(new)?;
                match f {
                    Family::Mysql => {
                        if old != new { out.push(at(format!("CHANGE COLUMN {} {}", quote_ident(f, &old.name), col_sql(f, new)))); }
                    }
                    Family::Postgres => {
                        let n = quote_ident(f, &new.name);
                        if old.name != new.name { out.push(at(format!("RENAME COLUMN {} TO {n}", quote_ident(f, &old.name)))); }
                        if old.type_name.trim() != new.type_name.trim() { out.push(at(format!("ALTER COLUMN {n} TYPE {} USING {n}::{}", new.type_name.trim(), new.type_name.trim()))); }
                        if old.default != new.default {
                            match new.default.as_deref().filter(|d| !d.trim().is_empty()) { Some(d) => out.push(at(format!("ALTER COLUMN {n} SET DEFAULT {}", d.trim()))), None => out.push(at(format!("ALTER COLUMN {n} DROP DEFAULT"))) }
                        }
                        if old.nullable != new.nullable { out.push(at(format!("ALTER COLUMN {n} {} NOT NULL", if new.nullable { "DROP" } else { "SET" }))); }
                        if old.comment != new.comment { out.push(format!("COMMENT ON COLUMN {table}.{n} IS {}", if new.comment.is_empty() { "NULL".into() } else { quote_lit(f, &new.comment) })); }
                    }
                    _ => {
                        if old.name != new.name { out.push(at(format!("RENAME COLUMN {} TO {}", quote_ident(f, &old.name), quote_ident(f, &new.name)))); }
                        if (&old.type_name, old.nullable, &old.default) != (&new.type_name, new.nullable, &new.default) { bail!(crate::i18n::tf("db.alter_err_sqlite_modify", &[("col", &new.name)])); }
                    }
                }
            }
            Op::DropColumn(n) => out.push(at(format!("DROP COLUMN {}", quote_ident(f, n)))),
            Op::AddIndex(i) => {
                if i.name.trim().is_empty() || i.columns.is_empty() { bail!(crate::i18n::t("db.alter_err_index").to_string()); }
                out.push(format!("CREATE {}INDEX {} ON {table} ({})", if i.unique { "UNIQUE " } else { "" }, quote_ident(f, &i.name), index_keys(f, &i.columns)));
            }
            Op::DropIndex(i) => out.push(match f {
                Family::Mysql => format!("DROP INDEX {} ON {table}", quote_ident(f, &i.name)),
                Family::Postgres if i.constraint => at(format!("DROP CONSTRAINT {}", quote_ident(f, &i.name))),
                Family::Postgres => match schema.filter(|s| !s.is_empty()) { Some(s) => format!("DROP INDEX {}.{}", quote_ident(f, s), quote_ident(f, &i.name)), None => format!("DROP INDEX {}", quote_ident(f, &i.name)) },
                _ => format!("DROP INDEX {}", quote_ident(f, &i.name)),
            }),
            Op::AddFk(k) => {
                if f == Family::Sqlite { bail!(crate::i18n::t("db.alter_err_sqlite_fk").to_string()); }
                if k.name.trim().is_empty() || k.columns.is_empty() || k.columns.len() != k.ref_columns.len() || k.ref_table.trim().is_empty() { bail!(crate::i18n::t("db.alter_err_fk").to_string()); }
                out.push(at(format!("ADD {}", fk_sql(f, k))));
            }
            Op::DropFk(n) => {
                if f == Family::Sqlite { bail!(crate::i18n::t("db.alter_err_sqlite_fk").to_string()); }
                out.push(at(match f { Family::Mysql => format!("DROP FOREIGN KEY {}", quote_ident(f, n)), _ => format!("DROP CONSTRAINT {}", quote_ident(f, n)) }));
            }
        }
    }
    // drops before adds so a dropped name can be reused
    let rank = |s: &String| if s.contains(" DROP FOREIGN KEY ") || s.contains(" DROP CONSTRAINT ") || s.starts_with("DROP INDEX") { 0 } else if s.starts_with("CREATE ") || s.contains(" ADD CONSTRAINT ") { 2 } else { 1 };
    out.sort_by_key(rank);
    Ok(out)
}

/// Rows (index name, unique 1/0, key list joined by U+001F, backs-a-constraint 1/0), primary key excluded.
pub fn list_indexes(f: Family, db: &str, schema: Option<&str>, table: &str) -> String {
    match f {
        Family::Mysql => format!(
            "SELECT INDEX_NAME, MIN(1 - NON_UNIQUE), GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX SEPARATOR '\u{1f}'), 0 \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} AND INDEX_NAME <> 'PRIMARY' GROUP BY INDEX_NAME ORDER BY INDEX_NAME",
            quote_lit(f, db), quote_lit(f, table)),
        Family::Postgres => {
            let q = quote_lit(f, &super::dialect::qualified(f, db, schema, table));
            format!(
                "SELECT ic.relname, CASE WHEN i.indisunique THEN 1 ELSE 0 END, \
                 array_to_string(ARRAY(SELECT pg_get_indexdef(i.indexrelid, k, true) FROM generate_series(1, i.indnkeyatts) k ORDER BY k), chr(31)), \
                 CASE WHEN EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid) THEN 1 ELSE 0 END \
                 FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid WHERE i.indrelid = {q}::regclass AND NOT i.indisprimary ORDER BY 1")
        }
        Family::Mssql => format!(
            "SELECT i.name, CAST(i.is_unique AS int), STRING_AGG(c.name, CHAR(31)) WITHIN GROUP (ORDER BY ic.key_ordinal), CAST(i.is_unique_constraint AS int) \
             FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 0 \
             JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
             WHERE i.object_id = OBJECT_ID({}) AND i.is_primary_key = 0 AND i.name IS NOT NULL GROUP BY i.name, i.is_unique, i.is_unique_constraint ORDER BY i.name",
            quote_lit(f, &super::dialect::qualified(f, db, schema, table))),
        Family::Dameng => super::dm::indexes_sql(super::dm::owner(db, schema), table),
        _ => format!(
            "SELECT il.name, il.\"unique\", (SELECT group_concat(name, char(31)) FROM (SELECT name FROM pragma_index_info(il.name) ORDER BY seqno)), 0 \
             FROM pragma_index_list({}) il WHERE il.origin <> 'pk' ORDER BY il.name", quote_lit(f, table)),
    }
}

/// Rows (name, columns, referenced table SQL, referenced columns, on delete, on update);
/// column lists joined by U+001F.
pub fn list_fks(f: Family, db: &str, schema: Option<&str>, table: &str) -> String {
    match f {
        Family::Mysql => format!(
            "SELECT k.CONSTRAINT_NAME, GROUP_CONCAT(k.COLUMN_NAME ORDER BY k.ORDINAL_POSITION SEPARATOR '\u{1f}'), \
             CONCAT('`', REPLACE(k.REFERENCED_TABLE_SCHEMA, '`', '``'), '`.`', REPLACE(k.REFERENCED_TABLE_NAME, '`', '``'), '`'), \
             GROUP_CONCAT(k.REFERENCED_COLUMN_NAME ORDER BY k.ORDINAL_POSITION SEPARATOR '\u{1f}'), MIN(r.DELETE_RULE), MIN(r.UPDATE_RULE) \
             FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
               ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME \
             WHERE k.TABLE_SCHEMA = {} AND k.TABLE_NAME = {} AND k.REFERENCED_TABLE_NAME IS NOT NULL GROUP BY k.CONSTRAINT_NAME ORDER BY 1",
            quote_lit(f, db), quote_lit(f, table)),
        Family::Postgres => {
            let q = quote_lit(f, &super::dialect::qualified(f, db, schema, table));
            let act = |c: &str| format!("CASE {c} WHEN 'a' THEN 'NO ACTION' WHEN 'r' THEN 'RESTRICT' WHEN 'c' THEN 'CASCADE' WHEN 'n' THEN 'SET NULL' WHEN 'd' THEN 'SET DEFAULT' END");
            format!(
                "SELECT c.conname, \
                 array_to_string(ARRAY(SELECT a.attname FROM unnest(c.conkey) WITH ORDINALITY u(n, o) JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = u.n ORDER BY u.o), chr(31)), \
                 c.confrelid::regclass::text, \
                 array_to_string(ARRAY(SELECT a.attname FROM unnest(c.confkey) WITH ORDINALITY u(n, o) JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = u.n ORDER BY u.o), chr(31)), \
                 {}, {} FROM pg_constraint c WHERE c.conrelid = {q}::regclass AND c.contype = 'f' ORDER BY 1", act("c.confdeltype"), act("c.confupdtype"))
        }
        Family::Mssql => format!(
            "SELECT fk.name, STRING_AGG(pc.name, CHAR(31)) WITHIN GROUP (ORDER BY fkc.constraint_column_id), \
             MIN(QUOTENAME(OBJECT_SCHEMA_NAME(fk.referenced_object_id)) + '.' + QUOTENAME(OBJECT_NAME(fk.referenced_object_id))), \
             STRING_AGG(rc.name, CHAR(31)) WITHIN GROUP (ORDER BY fkc.constraint_column_id), \
             MIN(REPLACE(fk.delete_referential_action_desc, '_', ' ')), MIN(REPLACE(fk.update_referential_action_desc, '_', ' ')) \
             FROM sys.foreign_keys fk JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
             JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             WHERE fk.parent_object_id = OBJECT_ID({}) GROUP BY fk.name ORDER BY 1",
            quote_lit(f, &super::dialect::qualified(f, db, schema, table))),
        _ => format!(
            "SELECT 'fk_' || id, group_concat(\"from\", char(31)), '\"' || replace(\"table\", '\"', '\"\"') || '\"', group_concat(\"to\", char(31)), MIN(on_delete), MIN(on_update) \
             FROM (SELECT * FROM pragma_foreign_key_list({}) ORDER BY id, seq) GROUP BY id ORDER BY id", quote_lit(f, table)),
    }
}

pub fn split_list(s: &str) -> Vec<String> { if s.is_empty() { vec![] } else { s.split('\u{1f}').map(String::from).collect() } }

/// Comma-separated user input (`a, b`) as names.
pub fn parse_names(s: &str) -> Vec<String> { s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect() }

#[cfg(test)]
mod tests {
    use super::*;

    fn col(n: &str, t: &str) -> ColDef { ColDef { name: n.into(), type_name: t.into(), nullable: true, ..Default::default() } }

    #[test]
    fn mysql() {
        let old = ColDef { extra: "auto_increment".into(), nullable: false, ..col("id", "int(11)") };
        let new = ColDef { name: "uid".into(), type_name: "bigint".into(), ..old.clone() };
        let s = statements(Family::Mysql, "`d`.`t`", None, &[
            Op::AddColumn(ColDef { default: Some("'x'".into()), comment: "c'".into(), nullable: false, ..col("a b", "varchar(10)") }),
            Op::ModifyColumn { old: old.clone(), new },
            Op::ModifyColumn { old: old.clone(), new: old },
            Op::DropColumn("z".into()),
            Op::AddIndex(IndexDef { name: "ix".into(), columns: vec!["a b".into()], unique: true, constraint: false }),
            Op::DropIndex(IndexDef { name: "ix".into(), ..Default::default() }),
            Op::AddFk(FkDef { name: "fk".into(), columns: vec!["uid".into()], ref_table: "`p`".into(), ref_columns: vec!["id".into()], on_delete: "CASCADE".into(), on_update: String::new() }),
            Op::DropFk("fk".into()),
        ]).unwrap();
        assert_eq!(s, vec![
            "DROP INDEX `ix` ON `d`.`t`",
            "ALTER TABLE `d`.`t` DROP FOREIGN KEY `fk`",
            "ALTER TABLE `d`.`t` ADD COLUMN `a b` varchar(10) NOT NULL DEFAULT 'x' COMMENT 'c'''",
            "ALTER TABLE `d`.`t` CHANGE COLUMN `id` `uid` bigint NOT NULL AUTO_INCREMENT",
            "ALTER TABLE `d`.`t` DROP COLUMN `z`",
            "CREATE UNIQUE INDEX `ix` ON `d`.`t` (`a b`)",
            "ALTER TABLE `d`.`t` ADD CONSTRAINT `fk` FOREIGN KEY (`uid`) REFERENCES `p` (`id`) ON DELETE CASCADE",
        ]);
        assert_eq!(mysql_default_expr("abc", ""), Some("'abc'".into()));
        assert_eq!(mysql_default_expr("'abc'", ""), Some("'abc'".into()));
        assert_eq!(mysql_default_expr("NULL", ""), None);
        assert_eq!(mysql_default_expr("current_timestamp()", ""), Some("current_timestamp()".into()));
        assert_eq!(mysql_default_expr("3", ""), Some("3".into()));
    }

    #[test]
    fn postgres_and_sqlite() {
        let old = col("n", "integer");
        let new = ColDef { name: "m".into(), type_name: "bigint".into(), nullable: false, default: Some("0".into()), comment: "c".into(), ..Default::default() };
        let s = statements(Family::Postgres, "\"s\".\"t\"", Some("s"), &[Op::ModifyColumn { old, new }, Op::DropIndex(IndexDef { name: "u".into(), constraint: true, ..Default::default() }), Op::DropIndex(IndexDef { name: "i".into(), ..Default::default() })]).unwrap();
        assert_eq!(s, vec![
            "ALTER TABLE \"s\".\"t\" DROP CONSTRAINT \"u\"",
            "DROP INDEX \"s\".\"i\"",
            "ALTER TABLE \"s\".\"t\" RENAME COLUMN \"n\" TO \"m\"",
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"m\" TYPE bigint USING \"m\"::bigint",
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"m\" SET DEFAULT 0",
            "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"m\" SET NOT NULL",
            "COMMENT ON COLUMN \"s\".\"t\".\"m\" IS 'c'",
        ]);
        assert!(statements(Family::Sqlite, "\"t\"", None, &[Op::ModifyColumn { old: col("a", "int"), new: col("a", "text") }]).is_err());
        assert!(statements(Family::Sqlite, "\"t\"", None, &[Op::AddColumn(ColDef { nullable: false, ..col("a", "int") })]).is_err());
        assert_eq!(statements(Family::Sqlite, "\"t\"", None, &[Op::ModifyColumn { old: col("a", "int"), new: col("b", "int") }]).unwrap(), vec!["ALTER TABLE \"t\" RENAME COLUMN \"a\" TO \"b\""]);
        assert!(statements(Family::Postgres, "t", None, &[Op::AddColumn(col("", "int"))]).is_err());
        assert_eq!(index_keys(Family::Postgres, &["a".into(), "lower(b)".into()]), "\"a\", lower(b)");
        assert_eq!(parse_names(" a, ,b "), vec!["a", "b"]);
    }
}
