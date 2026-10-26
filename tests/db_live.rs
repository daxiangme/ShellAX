//! Driver-layer tests against real servers. Ignored by default; run with
//!   SHELLAX_TEST_MYSQL_URL=mysql://u:p@127.0.0.1:3306/db \
//!   SHELLAX_TEST_PG_URL=postgres://u:p@127.0.0.1:5432/db \
//!   cargo test --test db_live -- --ignored --test-threads=1
//! The SSH tunnel test also needs TEST_SSH_PORT2 / TEST_SSH_USER / TEST_SSH_KEY (see testenv/).
//! Each test creates and drops its own objects (prefix `sx_it_`).
use std::sync::Arc;
use std::time::{Duration, Instant};

use ssh_term_proto::assets::{parse_uri, DbConn, Family};
use ssh_term_proto::db::{driver, edit, export, CancelSlot, Driver, ObjKind, RunOpts};

fn env(k: &str) -> Option<String> { std::env::var(k).ok().filter(|v| !v.is_empty()) }

fn conn(var: &str) -> Option<(DbConn, Option<String>)> { env(var).and_then(|u| parse_uri(&u)) }

fn rt() -> tokio::runtime::Runtime { tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap() }

async fn ok(d: &Driver, db: &str, sql: &str) {
    let r = d.execute(db, sql, false).await.unwrap();
    if let Some(e) = r.iter().find_map(|r| r.error.clone()) { panic!("{sql}: {e}"); }
}

async fn rows(d: &Driver, db: &str, schema: Option<&str>, sql: &str) -> Vec<Vec<Option<String>>> {
    let opts = RunOpts { schema: schema.map(String::from), ..Default::default() };
    let mut r = d.execute_with(db, sql, false, &opts).await.unwrap();
    let r = r.pop().unwrap();
    assert!(r.error.is_none(), "{sql}: {:?}", r.error);
    r.rows
}

fn s(v: &str) -> Option<String> { Some(v.to_string()) }

/// Long query cancelled from another task: must come back fast with the cancelled marker.
async fn cancel_case(d: &Driver, db: &str, sql: &str) {
    let slot = Arc::new(CancelSlot::default());
    let d2 = d.clone();
    let (db2, sql2, slot2) = (db.to_string(), sql.to_string(), slot.clone());
    let started = Instant::now();
    let h = tokio::spawn(async move { d2.execute_with(&db2, &sql2, false, &RunOpts { cancel: Some(slot2), ..Default::default() }).await });
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(d.cancel(db, &slot).await.unwrap(), "nothing to cancel");
    let r = tokio::time::timeout(Duration::from_secs(5), h).await.expect("cancel did not stop the query").unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(6));
    let e = r.last().unwrap().error.clone().expect("cancelled query reported success");
    assert!(!e.is_empty());
    // the pool is still usable afterwards
    assert_eq!(rows(d, db, None, "SELECT 1").await, vec![vec![s("1")]]);
}

/// Export the table, wipe it, import back, compare.
async fn roundtrip(d: &Driver, db: &str, schema: Option<&str>, table: &str, order: &str) {
    let f = d.family();
    let q = ssh_term_proto::db::dialect::qualified(f, db, schema, table);
    let before = d.execute_with(db, &format!("SELECT * FROM {q} ORDER BY {order}"), false, &RunOpts::default()).await.unwrap().pop().unwrap();
    assert!(before.error.is_none(), "{:?}", before.error);
    let cols = d.columns(db, schema, table).await.unwrap();
    for fmt in [export::Format::Csv, export::Format::Json, export::Format::Sql] {
        let text = export::export(fmt, f, &q, &before.columns, &before.rows);
        ok(d, db, &format!("DELETE FROM {q}")).await;
        let stmts = match fmt {
            export::Format::Sql => ssh_term_proto::db::split::split(&text).into_iter().map(|s| s.text).collect(),
            _ => {
                let (h, r) = if fmt == export::Format::Csv { export::parse_csv(&text).unwrap() } else { export::parse_json(&text).unwrap() };
                export::import_statements(f, &q, &cols, &h, &r, 200).unwrap()
            }
        };
        d.apply(db, schema, stmts).await.unwrap_or_else(|e| panic!("{fmt:?} import: {e}"));
        let after = d.execute_with(db, &format!("SELECT * FROM {q} ORDER BY {order}"), false, &RunOpts::default()).await.unwrap().pop().unwrap();
        assert_eq!(after.rows, before.rows, "{fmt:?} round trip");
    }
}

#[test]
#[ignore]
fn mysql_end_to_end() {
    let Some((c, pw)) = conn("SHELLAX_TEST_MYSQL_URL") else { eprintln!("SHELLAX_TEST_MYSQL_URL not set"); return };
    let db = c.database.clone();
    assert!(!db.is_empty(), "URL needs a database");
    rt().block_on(async {
        let d = driver::connect(&c, pw, None).await.unwrap();
        assert!(!d.server_version().await.unwrap().is_empty());
        assert!(d.databases().await.unwrap().contains(&db));
        ok(&d, &db, "DROP VIEW IF EXISTS sx_it_v; DROP TABLE IF EXISTS sx_it_t").await;
        ok(&d, &db, "CREATE TABLE sx_it_t (id INT PRIMARY KEY AUTO_INCREMENT, name VARCHAR(40) NOT NULL DEFAULT '' COMMENT 'nm', amount DECIMAL(10,2), j JSON, ts DATETIME, flag TINYINT(1), bin VARBINARY(8), KEY ix_name (name))").await;
        ok(&d, &db, "CREATE VIEW sx_it_v AS SELECT id, name FROM sx_it_t").await;
        let mut vals = vec![];
        for i in 1..=1203 { vals.push(format!("('n{:04}', {}.5, '{{\"k\": {i}}}', '2026-01-02 03:04:05', {}, NULL)", i, i, i % 2)); }
        ok(&d, &db, &format!("INSERT INTO sx_it_t (name, amount, j, ts, flag, bin) VALUES {}", vals.join(","))).await;
        ok(&d, &db, "UPDATE sx_it_t SET name = 'o''k \\\\ \"q\"', bin = x'00ff' WHERE id = 1").await;
        let t = d.tables(&db, None).await.unwrap();
        assert!(t.iter().any(|t| t.name == "sx_it_t" && t.kind == ObjKind::Table));
        assert!(t.iter().any(|t| t.name == "sx_it_v" && t.kind == ObjKind::View));
        let cols = d.columns(&db, None, "sx_it_t").await.unwrap();
        assert!(cols[0].primary && cols[1].comment == "nm" && cols[1].nullable == Some(false));
        // paging, sort, filter
        let (p, total) = d.page(&db, None, "sx_it_t", "", Some(("id", false)), 0).await.unwrap();
        assert_eq!((p.rows.len(), total), (500, Some(1203)));
        let (p3, _) = d.page(&db, None, "sx_it_t", "", Some(("id", false)), 1000).await.unwrap();
        assert_eq!(p3.rows.len(), 203);
        let (p, total) = d.page(&db, None, "sx_it_t", "flag = 1 AND id < 10", Some(("id", true)), 0).await.unwrap();
        assert_eq!(total, Some(5));
        assert_eq!(p.rows[0][0], s("9"));
        assert_eq!(p.rows[4][1], s("o'k \\ \"q\""));
        assert_eq!(p.rows[4][6], s("0x00FF"));
        assert!(d.page(&db, None, "sx_it_t", "nope = 1", None, 0).await.is_err());
        // DDL
        let ddl = d.ddl(&db, None, "sx_it_t").await.unwrap();
        assert!(ddl.starts_with("CREATE TABLE `sx_it_t`") && ddl.contains("ix_name"), "{ddl}");
        assert!(d.ddl(&db, None, "sx_it_v").await.unwrap().contains("VIEW"));
        // staged edits: commit, then a failing batch rolls back
        let tq = ssh_term_proto::db::dialect::qualified(Family::Mysql, &db, None, "sx_it_t");
        let pk = edit::pk_cols(&cols).unwrap();
        let row1 = p.rows[4].clone();
        let changes = vec![
            edit::Change::Delete { key: vec![("id".into(), s("2"))] },
            edit::Change::Update { key: edit::key_of(&cols, &pk, &row1), set: vec![("name".into(), s("x'y\\z")), ("amount".into(), None), ("j".into(), s("[1,2]"))] },
            edit::Change::Insert { values: vec![("name".into(), s("new")), ("ts".into(), s("2026-10-26 15:00:00"))] },
        ];
        let stmts: Vec<String> = changes.iter().map(|c| edit::to_sql_typed(Family::Mysql, &tq, &cols, c)).collect();
        assert_eq!(d.apply(&db, None, stmts).await.unwrap(), 3);
        // MySQL normalises JSON text, MariaDB keeps it
        let mut got = rows(&d, &db, None, "SELECT name, amount, j FROM sx_it_t WHERE id = 1").await;
        got[0][2] = got[0][2].take().map(|j| j.replace(' ', ""));
        assert_eq!(got, vec![vec![s("x'y\\z"), None, s("[1,2]")]]);
        assert_eq!(rows(&d, &db, None, "SELECT COUNT(*) FROM sx_it_t").await, vec![vec![s("1203")]]);
        let bad = vec![edit::to_sql(Family::Mysql, &tq, &edit::Change::Delete { key: vec![("id".into(), s("3"))] }), "INSERT INTO sx_it_t (id) VALUES (1)".to_string()];
        assert!(d.apply(&db, None, bad).await.is_err());
        assert_eq!(rows(&d, &db, None, "SELECT COUNT(*) FROM sx_it_t WHERE id = 3").await, vec![vec![s("1")]]);
        // streaming export of the whole table
        let path = std::env::temp_dir().join(format!("sx_it_my_{}.csv", std::process::id()));
        let n = d.export_table(&db, None, "sx_it_t", "", None, export::Format::Csv, &path, None).await.unwrap();
        assert_eq!(n, 1203);
        let (_, back) = export::parse_csv(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.len(), 1203);
        let _ = std::fs::remove_file(&path);
        // import round trip on a small table
        ok(&d, &db, "DELETE FROM sx_it_t WHERE id > 20").await;
        roundtrip(&d, &db, None, "sx_it_t", "id").await;
        // cancel
        cancel_case(&d, &db, "SELECT SLEEP(30)").await;
        cancel_case(&d, &db, "SELECT COUNT(*) FROM information_schema.columns a, information_schema.columns b, information_schema.columns c").await;
        ok(&d, &db, "DROP VIEW sx_it_v; DROP TABLE sx_it_t").await;
        d.close().await.unwrap();
    });
}

#[test]
#[ignore]
fn postgres_end_to_end() {
    let Some((c, pw)) = conn("SHELLAX_TEST_PG_URL") else { eprintln!("SHELLAX_TEST_PG_URL not set"); return };
    let db = c.database.clone();
    rt().block_on(async {
        let d = driver::connect(&c, pw, None).await.unwrap();
        assert!(!d.server_version().await.unwrap().is_empty());
        assert!(d.databases().await.unwrap().contains(&db));
        ok(&d, &db, "DROP SCHEMA IF EXISTS sx_it CASCADE; CREATE SCHEMA sx_it").await;
        ok(&d, &db, "CREATE TABLE sx_it.t (id uuid PRIMARY KEY, n integer NOT NULL, amount numeric(12,3), j jsonb, js json, tags text[], nums int[], ts timestamptz, tsn timestamp, d date, flag boolean, b bytea, iv interval, note varchar(30) DEFAULT 'x')").await;
        ok(&d, &db, "COMMENT ON COLUMN sx_it.t.n IS 'number'; CREATE INDEX t_n ON sx_it.t (n); CREATE VIEW sx_it.v AS SELECT id, n FROM sx_it.t; CREATE MATERIALIZED VIEW sx_it.mv AS SELECT n FROM sx_it.t").await;
        ok(&d, &db, "INSERT INTO sx_it.t (id, n, amount, j, js, tags, nums, ts, tsn, d, flag, b, iv, note) \
            SELECT md5(g::text)::uuid, g, g * 1.125, jsonb_build_object('k', g), '{\"a\": [1, 2]}', ARRAY['a b', 'c\"d', NULL], ARRAY[g, g + 1], \
            '2026-01-02 03:04:05+00', '2026-01-02 03:04:05.5', '2026-01-02', g % 2 = 0, '\\x00ff'::bytea, '1 day 02:00', 'o''k' FROM generate_series(1, 1203) g").await;
        // schemas / search_path
        assert!(d.schemas(&db).await.unwrap().contains(&"sx_it".to_string()));
        ok(&d, &db, "CREATE TABLE public.sx_it_pub (x int); INSERT INTO public.sx_it_pub VALUES (7)").await;
        assert_eq!(rows(&d, &db, Some("sx_it"), "SELECT count(*) FROM t").await, vec![vec![s("1203")]]);
        assert_eq!(rows(&d, &db, Some("sx_it"), "SELECT x FROM sx_it_pub").await, vec![vec![s("7")]]);
        let r = d.execute_with(&db, "SELECT count(*) FROM t", false, &RunOpts::default()).await.unwrap();
        assert!(r[0].error.is_some(), "search_path leaked across pooled sessions");
        ok(&d, &db, "DROP TABLE public.sx_it_pub").await;
        let t = d.tables(&db, Some("sx_it")).await.unwrap();
        let kind = |n: &str| t.iter().find(|t| t.name == n).map(|t| t.kind);
        assert_eq!((kind("t"), kind("v"), kind("mv")), (Some(ObjKind::Table), Some(ObjKind::View), Some(ObjKind::View)));
        let cols = d.columns(&db, Some("sx_it"), "t").await.unwrap();
        assert!(cols[0].primary && cols[1].comment == "number" && cols[13].default.as_deref() == Some("'x'::character varying"));
        // paging / sort / filter
        let (p, total) = d.page(&db, Some("sx_it"), "t", "", Some(("n", false)), 0).await.unwrap();
        assert_eq!((p.rows.len(), total), (500, Some(1203)));
        let (p, total) = d.page(&db, Some("sx_it"), "t", "n <= 4 AND flag", Some(("n", true)), 0).await.unwrap();
        assert_eq!(total, Some(2));
        let r0 = p.rows[0].clone();
        assert_eq!(r0[1], s("4"));
        assert_eq!(r0[2], s("4.500"));
        assert_eq!(r0[5], s("{\"a b\",\"c\\\"d\",NULL}"));
        assert_eq!(r0[11], s("\\x00ff"));
        // DDL
        let ddl = d.ddl(&db, Some("sx_it"), "t").await.unwrap();
        assert!(ddl.contains("PRIMARY KEY") && ddl.contains("COMMENT ON COLUMN") && ddl.contains("CREATE INDEX t_n"), "{ddl}");
        assert!(d.ddl(&db, Some("sx_it"), "v").await.unwrap().starts_with("CREATE OR REPLACE VIEW"));
        assert!(d.ddl(&db, Some("sx_it"), "mv").await.unwrap().starts_with("CREATE MATERIALIZED VIEW"));
        // typed staged edits round-trip the display text of each type
        let tq = ssh_term_proto::db::dialect::qualified(Family::Postgres, &db, Some("sx_it"), "t");
        let pk = edit::pk_cols(&cols).unwrap();
        let set = vec![
            ("amount".to_string(), s("12345.678")), ("j".into(), s("{\"x\": [1, null]}")), ("js".into(), s("[true]")),
            ("tags".into(), s("{x,\"y z\",NULL}")), ("nums".into(), s("{9,8}")), ("ts".into(), s("2026-10-26 07:00:00+00")),
            ("tsn".into(), s("2026-10-26 15:00:00.25")), ("d".into(), s("2026-10-26")), ("flag".into(), s("f")),
            ("iv".into(), s("3 days")), ("note".into(), None),
        ];
        let up = edit::Change::Update { key: edit::key_of(&cols, &pk, &r0), set: set.clone() };
        let new_id = "00000000-0000-0000-0000-000000000001";
        let ins = edit::Change::Insert { values: vec![("id".into(), s(new_id)), ("n".into(), s("0")), ("nums".into(), s("{}")), ("j".into(), s("null"))] };
        let del = edit::Change::Delete { key: vec![("id".into(), r0[0].clone())] };
        let stmts: Vec<String> = [up.clone(), ins].iter().map(|c| edit::to_sql_typed(Family::Postgres, &tq, &cols, c)).collect();
        assert_eq!(d.apply(&db, Some("sx_it"), stmts).await.unwrap(), 2);
        let got = rows(&d, &db, None, &format!("SELECT amount, j, js, tags, nums, ts, tsn, d, flag, iv, note FROM sx_it.t WHERE id = '{}'", r0[0].as_deref().unwrap())).await;
        assert_eq!(got[0], set.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>());
        assert_eq!(rows(&d, &db, None, &format!("SELECT n, nums, j FROM sx_it.t WHERE id = '{new_id}'")).await, vec![vec![s("0"), s("{}"), s("null")]]);
        // failing batch rolls back (bad array literal after a delete)
        let bad = vec![edit::to_sql_typed(Family::Postgres, &tq, &cols, &del), edit::to_sql_typed(Family::Postgres, &tq, &cols, &edit::Change::Update { key: vec![("id".into(), s(new_id))], set: vec![("nums".into(), s("{x}"))] })];
        assert!(d.apply(&db, Some("sx_it"), bad).await.is_err());
        assert_eq!(rows(&d, &db, None, "SELECT count(*) FROM sx_it.t").await, vec![vec![s("1204")]]);
        // streaming export of the whole table (JSON)
        let path = std::env::temp_dir().join(format!("sx_it_pg_{}.json", std::process::id()));
        let n = d.export_table(&db, Some("sx_it"), "t", "", Some(("n", false)), export::Format::Json, &path, None).await.unwrap();
        assert_eq!(n, 1204);
        let (_, back) = export::parse_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.len(), 1204);
        let _ = std::fs::remove_file(&path);
        // import round trip
        ok(&d, &db, "DELETE FROM sx_it.t WHERE n > 20; DROP MATERIALIZED VIEW sx_it.mv").await;
        roundtrip(&d, &db, Some("sx_it"), "t", "n").await;
        // cancel
        cancel_case(&d, &db, "SELECT pg_sleep(30)").await;
        cancel_case(&d, &db, "SELECT count(*) FROM generate_series(1, 2000000000)").await;
        ok(&d, &db, "DROP SCHEMA sx_it CASCADE").await;
        d.close().await.unwrap();
    });
}

#[test]
#[ignore]
fn ssh_tunnel() {
    use ssh_term_proto::ssh::{self, SshTarget};
    let (Some(port), Some(user), Some(key)) = (env("TEST_SSH_PORT2"), env("TEST_SSH_USER"), env("TEST_SSH_KEY")) else { eprintln!("TEST_SSH_* not set"); return };
    let targets: Vec<(DbConn, Option<String>)> = ["SHELLAX_TEST_MYSQL_URL", "SHELLAX_TEST_PG_URL"].iter().filter_map(|v| conn(v)).collect();
    assert!(!targets.is_empty());
    let (htx, hrx) = flume::unbounded::<ssh::HostKeyPrompt>();
    std::thread::spawn(move || while let Ok(p) = hrx.recv() { let _ = p.reply.send(ssh::HostKeyDecision::AcceptOnce); });
    let (atx, _arx) = flume::unbounded::<ssh::AuthPrompt>();
    let kh = std::env::temp_dir().join(format!("sx-it-kh-{}", std::process::id()));
    rt().block_on(async {
        for (c, pw) in targets {
            let t = SshTarget { host: "127.0.0.1".into(), port: port.parse().unwrap(), user: user.clone(), auth: "key".into(), keys: vec![key.clone().into()], known_hosts: kh.clone(), ..Default::default() };
            let live = ssh_term_proto::db::live::open(&c, pw, Some((t, ssh::Prompts { hostkey: htx.clone(), auth: atx.clone() }))).await.unwrap();
            assert!(live.via.is_some() && !live.version.is_empty());
            assert_eq!(rows(&live.driver, &c.database, None, "SELECT 1").await, vec![vec![s("1")]]);
            live.close().await;
        }
    });
}

/// Generated DDL must recreate the object: run it under another name and compare columns.
#[test]
#[ignore]
fn ddl_recreates() {
    rt().block_on(async {
        if let Some((c, pw)) = conn("SHELLAX_TEST_PG_URL") {
            let db = c.database.clone();
            let d = driver::connect(&c, pw, None).await.unwrap();
            ok(&d, &db, "DROP SCHEMA IF EXISTS \"Sx It\" CASCADE; DROP SCHEMA IF EXISTS sx_it2 CASCADE; CREATE SCHEMA \"Sx It\"; CREATE SCHEMA sx_it2").await;
            ok(&d, &db, "CREATE TABLE \"Sx It\".\"Parent\" (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, code text UNIQUE)").await;
            ok(&d, &db, "CREATE TABLE \"Sx It\".\"Child\" (id serial PRIMARY KEY, pid bigint REFERENCES \"Sx It\".\"Parent\"(id) ON DELETE CASCADE, \"Mixed Col\" varchar(10) NOT NULL DEFAULT 'a', total numeric(10,2) GENERATED ALWAYS AS (id * 2) STORED, CHECK (char_length(\"Mixed Col\") > 0)); CREATE INDEX child_lower ON \"Sx It\".\"Child\" (lower(\"Mixed Col\"))").await;
            for t in ["Parent", "Child"] {
                let ddl = d.ddl(&db, Some("Sx It"), t).await.unwrap();
                let ddl2 = ddl.replace("\"Sx It\".", "sx_it2.").replace("\"Sx It\"", "sx_it2");
                let r = d.execute(&db, &ddl2, false).await.unwrap();
                assert!(r.iter().all(|r| r.error.is_none()), "{t}: {ddl2}\n{:?}", r.iter().find_map(|r| r.error.clone()));
                let a = d.columns(&db, Some("Sx It"), t).await.unwrap();
                let b = d.columns(&db, Some("sx_it2"), t).await.unwrap();
                assert_eq!(a.iter().map(|c| (&c.name, &c.type_name, c.nullable, c.primary, &c.extra)).collect::<Vec<_>>(), b.iter().map(|c| (&c.name, &c.type_name, c.nullable, c.primary, &c.extra)).collect::<Vec<_>>(), "{t}");
            }
            ok(&d, &db, "DROP SCHEMA \"Sx It\" CASCADE; DROP SCHEMA sx_it2 CASCADE").await;
        }
        if let Some((c, pw)) = conn("SHELLAX_TEST_MYSQL_URL") {
            let db = c.database.clone();
            let d = driver::connect(&c, pw, None).await.unwrap();
            ok(&d, &db, "DROP TABLE IF EXISTS `Sx Child`, `sx child2`").await;
            ok(&d, &db, "CREATE TABLE `Sx Child` (id INT AUTO_INCREMENT PRIMARY KEY, `Mixed Col` VARCHAR(10) NOT NULL DEFAULT 'a' COMMENT 'it''s', g INT AS (id * 2) VIRTUAL, UNIQUE KEY u (`Mixed Col`))").await;
            let ddl = d.ddl(&db, None, "Sx Child").await.unwrap();
            ok(&d, &db, &ddl.replacen("`Sx Child`", "`sx child2`", 1)).await;
            ok(&d, &db, "DROP TABLE `Sx Child`, `sx child2`").await;
        }
    });
}

/// Cancelling a streaming export stops it and leaves no partial file.
#[test]
#[ignore]
fn export_cancel() {
    let Some((c, pw)) = conn("SHELLAX_TEST_PG_URL") else { return };
    let db = c.database.clone();
    rt().block_on(async {
        let d = driver::connect(&c, pw, None).await.unwrap();
        ok(&d, &db, "DROP VIEW IF EXISTS public.sx_it_slow; CREATE VIEW public.sx_it_slow AS SELECT g, pg_sleep(0.01)::text AS z FROM generate_series(1, 100000) g").await;
        let path = std::env::temp_dir().join(format!("sx_it_cancel_{}.csv", std::process::id()));
        let slot = Arc::new(CancelSlot::default());
        let (d2, db2, p2, s2) = (d.clone(), db.clone(), path.clone(), slot.clone());
        let h = tokio::spawn(async move { d2.export_table(&db2, Some("public"), "sx_it_slow", "", None, export::Format::Csv, &p2, Some(s2)).await });
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(d.cancel(&db, &slot).await.unwrap());
        let r = tokio::time::timeout(Duration::from_secs(5), h).await.unwrap().unwrap();
        assert!(r.is_err());
        assert!(!path.exists());
        ok(&d, &db, "DROP VIEW public.sx_it_slow").await;
    });
}

/// Structure designer statements applied for real, then read back from the catalog.
async fn alter_case(d: &Driver, db: &str, schema: Option<&str>) {
    use ssh_term_proto::db::alter::{self, ColDef, FkDef, IndexDef, Op};
    let f = d.family();
    let q = |t: &str| ssh_term_proto::db::dialect::qualified(f, db, schema, t);
    let (p, c) = (q("sx_it_p"), q("sx_it_c"));
    let _ = d.execute(db, &format!("DROP TABLE IF EXISTS {c}; DROP TABLE IF EXISTS {p}"), false).await;
    let serial = match f { Family::Mysql => "INT AUTO_INCREMENT PRIMARY KEY", Family::Postgres => "serial PRIMARY KEY", _ => "INTEGER PRIMARY KEY" };
    ok(d, db, &format!("CREATE TABLE {p} (id {serial}, code VARCHAR(10)); CREATE TABLE {c} (id {serial}, pid INT, note VARCHAR(20) DEFAULT 'n', old_col INT)")).await;
    let cols = d.columns(db, schema, "sx_it_c").await.unwrap();
    let def = |n: &str| ColDef::from_info(f, cols.iter().find(|c| c.name == n).unwrap());
    let note = def("note");
    assert_eq!(note.default.as_deref().map(|d| d.split("::").next().unwrap()), Some("'n'"), "{:?}", note);
    let mut ops = vec![
        Op::AddColumn(ColDef { name: "added col".into(), type_name: "VARCHAR(5)".into(), nullable: false, default: Some("'z'".into()), comment: "new".into(), extra: String::new() }),
        Op::DropColumn("old_col".into()),
        Op::AddIndex(IndexDef { name: "ix_c_note".into(), columns: vec!["memo".into(), "added col".into()], unique: true, constraint: false }),
    ];
    if f != Family::Sqlite {
        ops.push(Op::ModifyColumn { old: note.clone(), new: ColDef { name: "memo".into(), type_name: "VARCHAR(40)".into(), nullable: false, default: Some("'m'".into()), comment: "memo".into(), ..note.clone() } });
        ops.push(Op::AddFk(FkDef { name: "fk_c_p".into(), columns: vec!["pid".into()], ref_table: p.clone(), ref_columns: vec!["id".into()], on_delete: "CASCADE".into(), on_update: String::new() }));
    } else {
        ops.push(Op::ModifyColumn { old: note.clone(), new: ColDef { name: "memo".into(), ..note.clone() } });
    }
    let stmts = alter::statements(f, &c, schema, &ops).unwrap();
    d.apply(db, schema, stmts.clone()).await.unwrap_or_else(|e| panic!("{e}\n{}", stmts.join(";\n")));
    let cols = d.columns(db, schema, "sx_it_c").await.unwrap();
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["id", "pid", "memo", "added col"]);
    let memo = &cols[2];
    if f != Family::Sqlite {
        assert!(memo.type_name.to_lowercase().contains("40") && memo.nullable == Some(false) && memo.comment == "memo", "{memo:?}");
        assert!(memo.default.as_deref().unwrap().contains('m'));
    }
    let ix = d.indexes(db, schema, "sx_it_c").await.unwrap();
    let ix = ix.iter().find(|i| i.name == "ix_c_note").unwrap_or_else(|| panic!("{ix:?}"));
    assert!(ix.unique && ix.columns.len() == 2, "{ix:?}");
    if f != Family::Sqlite {
        let fks = d.foreign_keys(db, schema, "sx_it_c").await.unwrap();
        assert_eq!(fks.len(), 1);
        assert_eq!((fks[0].name.as_str(), fks[0].columns.clone(), fks[0].ref_columns.clone(), fks[0].on_delete.as_str()), ("fk_c_p", vec!["pid".to_string()], vec!["id".to_string()], "CASCADE"));
        // read back, drop both, re-add the FK from what was read
        let st = alter::statements(f, &c, schema, &[Op::DropFk(fks[0].name.clone()), Op::DropIndex(ix.clone())]).unwrap();
        d.apply(db, schema, st).await.unwrap();
        assert!(d.foreign_keys(db, schema, "sx_it_c").await.unwrap().is_empty());
        d.apply(db, schema, alter::statements(f, &c, schema, &[Op::AddFk(fks[0].clone()), Op::AddIndex(ix.clone())]).unwrap()).await.unwrap();
        assert_eq!(d.foreign_keys(db, schema, "sx_it_c").await.unwrap(), fks);
        // a failing batch: PostgreSQL rolls back the whole change
        let bad = alter::statements(f, &c, schema, &[Op::DropColumn("pid".into()), Op::DropColumn("nope".into())]).unwrap();
        assert!(d.apply(db, schema, bad).await.is_err());
        if f == Family::Postgres { assert!(d.columns(db, schema, "sx_it_c").await.unwrap().iter().any(|c| c.name == "pid")); }
    } else {
        let fks = d.foreign_keys(db, schema, "sx_it_c").await.unwrap();
        assert!(fks.is_empty());
        d.apply(db, schema, alter::statements(f, &c, schema, &[Op::DropIndex(ix.clone())]).unwrap()).await.unwrap();
    }
    ok(d, db, &format!("DROP TABLE {c}; DROP TABLE {p}")).await;
}

#[test]
#[ignore]
fn structure_designer() {
    rt().block_on(async {
        let dir = std::env::temp_dir().join(format!("sx-it-lite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.db");
        std::fs::write(&file, b"").unwrap();
        let lite = DbConn { kind: ssh_term_proto::assets::DbKind::Sqlite, file: file.to_string_lossy().into(), ..Default::default() };
        let d = driver::connect(&lite, None, None).await.unwrap();
        alter_case(&d, "main", None).await;
        d.close().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        if let Some((c, pw)) = conn("SHELLAX_TEST_MYSQL_URL") {
            let d = driver::connect(&c, pw, None).await.unwrap();
            alter_case(&d, &c.database, None).await;
        }
        if let Some((c, pw)) = conn("SHELLAX_TEST_PG_URL") {
            let d = driver::connect(&c, pw, None).await.unwrap();
            ok(&d, &c.database, "CREATE SCHEMA IF NOT EXISTS sx_alt").await;
            alter_case(&d, &c.database, Some("sx_alt")).await;
            ok(&d, &c.database, "DROP SCHEMA sx_alt CASCADE").await;
        }
    });
}
