//! Live MongoDB test. Ignored by default; run with SHELLAX_TEST_MONGO=host:port
//! `cargo test --test mongo_live -- --ignored`.
use ssh_term_proto::assets::{DbConn, DbKind};
use ssh_term_proto::doc::{id_text, Mongo};

#[test]
#[ignore]
fn mongo_find_edit() {
    let Some(a) = std::env::var("SHELLAX_TEST_MONGO").ok().filter(|v| !v.is_empty()) else { eprintln!("SHELLAX_TEST_MONGO not set"); return };
    let (h, p) = a.rsplit_once(':').unwrap();
    let c = DbConn { kind: DbKind::Mongodb, host: h.into(), port: p.parse().ok(), ..Default::default() };
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        let m = Mongo::open(&c, None, None).await.unwrap();
        assert!(m.version.starts_with('7'));
        let coll = m.client.database("sxtest").collection::<mongodb::bson::Document>("people");
        let _ = coll.drop().await;
        for i in 0..120 { m.insert("sxtest", "people", &format!(r#"{{"n": {i}, "name": "p{i}", "tags": ["a"], "at": {{"$date": "2024-01-02T03:04:05Z"}}}}"#)).await.unwrap(); }
        assert!(m.databases().await.unwrap().contains(&"sxtest".to_string()));
        assert_eq!(m.collections("sxtest").await.unwrap(), vec!["people".to_string()]);
        let pg = m.find("sxtest", "people", r#"{"n": {"$gte": 10}}"#, r#"{"n": -1}"#, "", 0).await.unwrap();
        assert_eq!((pg.docs.len(), pg.total), (50, Some(110)));
        assert_eq!(pg.docs[0].get_i32("n").unwrap(), 119);
        let pg2 = m.find("sxtest", "people", r#"{"n": {"$gte": 10}}"#, r#"{"n": 1}"#, r#"{"name": 1}"#, 100).await.unwrap();
        assert_eq!((pg2.docs.len(), pg2.total), (10, None));
        assert!(pg2.docs[0].get("n").is_none());
        let d = &pg.docs[0];
        let id = d.get("_id").unwrap().clone();
        assert_eq!(id_text(d).len(), 24);
        let mut j: serde_json::Value = serde_json::from_str(&ssh_term_proto::doc::to_json(d)).unwrap();
        j["name"] = "changed".into();
        m.replace("sxtest", "people", &id, &j.to_string()).await.unwrap();
        let back = m.find("sxtest", "people", r#"{"name": "changed"}"#, "", "", 0).await.unwrap();
        assert_eq!(back.total, Some(1));
        assert!(matches!(back.docs[0].get("at"), Some(mongodb::bson::Bson::DateTime(_))), "dates survive the JSON round trip");
        j["_id"] = serde_json::json!({"$oid": "65a000000000000000000001"});
        assert!(m.replace("sxtest", "people", &id, &j.to_string()).await.is_err());
        assert_eq!(m.delete("sxtest", "people", &id).await.unwrap(), 1);
        assert!(m.find("sxtest", "people", "{bad", "", "", 0).await.is_err());
        coll.create_index(mongodb::IndexModel::builder().keys(mongodb::bson::doc! {"n": 1}).options(mongodb::options::IndexOptions::builder().unique(true).build()).build()).await.unwrap();
        let ix = m.indexes("sxtest", "people").await.unwrap();
        assert!(ix.iter().any(|i| i.name == "_id_") && ix.iter().any(|i| i.unique && i.keys.contains("\"n\"")));
        let agg = m.aggregate("sxtest", "people", r#"[{"$match":{"n":{"$gte":10}}},{"$count":"c"}]"#, 100).await.unwrap();
        assert_eq!(agg.len(), 1);
        let path = std::env::temp_dir().join(format!("sx-mongo-{}.json", std::process::id()));
        let n = m.export("sxtest", "people", r#"{"n":{"$lt":5}}"#, r#"{"n":1}"#, "", &path).await.unwrap();
        let arr: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((n as usize, arr.len()), (arr.len(), 5));
        let _ = std::fs::remove_file(&path);
        coll.drop().await.unwrap();
        m.close().await;
    });
}
