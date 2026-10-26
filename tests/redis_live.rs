//! Live Redis tests. Ignored by default; run with `cargo test --test redis_live -- --ignored`
//! and SHELLAX_TEST_REDIS=host:port (e.g. 127.0.0.1:6379). The SSH case also needs
//! TEST_SSH_PORT2 / TEST_SSH_USER / TEST_SSH_KEY (see testenv/).
use ssh_term_proto::assets::RedisConn;
use ssh_term_proto::kv::{Content, Cursor, Kv};

fn env(k: &str) -> Option<String> { std::env::var(k).ok().filter(|v| !v.is_empty()) }
fn rt() -> tokio::runtime::Runtime { tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap() }
fn target() -> Option<RedisConn> {
    let a = env("SHELLAX_TEST_REDIS")?;
    let (h, p) = a.rsplit_once(':')?;
    Some(RedisConn { id: "t".into(), host: h.into(), port: p.parse().ok(), db: 9, ..Default::default() })
}

#[test]
#[ignore]
fn redis_types_and_ops() {
    let Some(c) = target() else { eprintln!("SHELLAX_TEST_REDIS not set"); return };
    rt().block_on(async {
        let kv = Kv::open(&c, None, None).await.unwrap();
        assert!(!kv.version.is_empty());
        kv.console("FLUSHDB").await.unwrap();
        for i in 0..1500 { kv.raw(&["SET".into(), format!("app:user:{i}"), "x".into()]).await.unwrap(); }
        // full SCAN paging
        let mut cur = Cursor::default(); let mut all = vec![];
        while !cur.done { let (k, n) = kv.scan(cur, "app:*", None).await.unwrap(); all.extend(k); cur = n; }
        all.sort(); all.dedup();
        assert_eq!(all.len(), 1500);
        assert_eq!(kv.dbsize().await.unwrap(), 1500);
        let root = ssh_term_proto::kv::tree::Node::build(all.iter().map(|s| s.as_str()), ":");
        assert_eq!(root.count, 1500);
        // types
        kv.create("s", "string", "{\"a\":1}", "").await.unwrap();
        assert!(kv.create("s", "string", "x", "").await.is_err());
        kv.create("h", "hash", "f", "v").await.unwrap();
        kv.create("l", "list", "a", "").await.unwrap();
        kv.lpush("l", "b", true).await.unwrap(); kv.lpush("l", "c", true).await.unwrap();
        kv.ldel("l", 1).await.unwrap();
        kv.create("st", "set", "m", "").await.unwrap();
        kv.create("z", "zset", "m", "2.5").await.unwrap();
        kv.create("x", "stream", "f", "v").await.unwrap();
        kv.raw(&["XGROUP".into(), "CREATE".into(), "x".into(), "g".into(), "0".into()]).await.unwrap();
        match kv.read("s", "string", 0, None, "").await.unwrap() { Content::Str { value, .. } => assert!(ssh_term_proto::kv::pretty_json(&String::from_utf8(value).unwrap()).is_some()), o => panic!("{o:?}") }
        assert_eq!(kv.read("h", "hash", 0, None, "").await.unwrap(), Content::Pairs { items: vec![("f".into(), "v".into())], next: None });
        assert_eq!(kv.read("l", "list", 0, None, "").await.unwrap(), Content::Pairs { items: vec![("0".into(), "a".into()), ("1".into(), "c".into())], next: None });
        assert_eq!(kv.read("z", "zset", 0, None, "").await.unwrap(), Content::Pairs { items: vec![("m".into(), "2.5".into())], next: None });
        match kv.read("x", "stream", 0, None, "").await.unwrap() { Content::Stream { entries, groups, .. } => { assert_eq!(entries.len(), 1); assert_eq!(groups[0].0, "g"); } o => panic!("{o:?}") }
        let m = kv.meta("h").await.unwrap();
        assert_eq!((m.kind.as_str(), m.ttl_ms, m.len), ("hash", -1, Some(1)));
        kv.set_ttl("h", Some(100)).await.unwrap();
        assert!(kv.meta("h").await.unwrap().ttl_ms > 0);
        kv.set_ttl("h", None).await.unwrap();
        kv.rename("h", "h2").await.unwrap();
        assert!(kv.rename("s", "h2").await.is_err());
        assert_eq!(kv.matching("app:user:1*", 10_000).await.unwrap().len(), 611);
        assert_eq!(kv.delete(&["h2".into(), "st".into()]).await.unwrap(), 2);
        assert!(kv.console("SELECT 1").await.is_err());
        assert_eq!(kv.console("GET nope").await.unwrap(), "(nil)");
        assert!(kv.info().await.unwrap().iter().any(|r| r.1 == "redis_version"));
        kv.slowlog(10).await.unwrap();
        kv.console("FLUSHDB").await.unwrap();
        kv.close().await;
    });
}

#[test]
#[ignore]
fn redis_over_ssh() {
    use ssh_term_proto::ssh::{self, SshTarget};
    let Some(c) = target() else { return };
    let (Some(port), Some(user), Some(key)) = (env("TEST_SSH_PORT2"), env("TEST_SSH_USER"), env("TEST_SSH_KEY")) else { eprintln!("TEST_SSH_* not set"); return };
    let (htx, hrx) = flume::unbounded::<ssh::HostKeyPrompt>();
    std::thread::spawn(move || while let Ok(p) = hrx.recv() { let _ = p.reply.send(ssh::HostKeyDecision::AcceptOnce); });
    let (atx, _arx) = flume::unbounded::<ssh::AuthPrompt>();
    let kh = std::env::temp_dir().join(format!("sx-rd-kh-{}", std::process::id()));
    rt().block_on(async {
        let t = SshTarget { host: "127.0.0.1".into(), port: port.parse().unwrap(), user, auth: "key".into(), keys: vec![key.into()], known_hosts: kh, ..Default::default() };
        let kv = Kv::open(&c, None, Some((t, ssh::Prompts { hostkey: htx, auth: atx }))).await.unwrap();
        assert!(kv.via.is_some());
        assert_eq!(kv.console("PING").await.unwrap(), "PONG");
        kv.close().await;
    });
}
