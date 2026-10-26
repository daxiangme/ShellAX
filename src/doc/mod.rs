//! MongoDB access (basic): databases and collections, `find` with a JSON filter / sort /
//! projection and paging, documents as relaxed Extended JSON for viewing and editing
//! (replace by `_id`), insert and delete. Reached directly or through an SSH tunnel.
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use mongodb::bson::{doc, Bson, Document};
use mongodb::options::ClientOptions;
use mongodb::Client;

use crate::assets::DbConn;
use crate::forward::Running;
use crate::ssh::{Prompts, SshTarget};

pub const PAGE: u64 = 50;

pub struct Mongo { pub client: Client, pub version: String, pub via: Option<String>, pub read_only: bool, tunnels: Vec<Arc<Running>> }

/// One page of a `find`.
#[derive(Debug, Clone, Default)]
pub struct FindPage { pub docs: Vec<Document>, pub total: Option<u64> }

/// Parse a filter / sort / projection typed by the user (relaxed or canonical Extended JSON);
/// empty means `{}`.
pub fn parse_doc(s: &str) -> Result<Document> {
    let s = s.trim();
    if s.is_empty() { return Ok(Document::new()); }
    let v: serde_json::Value = serde_json::from_str(s).with_context(|| crate::i18n::t("mongo.err_json").to_string())?;
    match Bson::try_from(v)? { Bson::Document(d) => Ok(d), _ => bail!(crate::i18n::t("mongo.err_object").to_string()) }
}

/// A pasted `mongodb://` / `mongodb+srv://` URI split for the connection form:
/// (URI with credentials removed, user, password, database from the path).
/// Credentials are kept out of the stored URI so the password stays in the secret store.
pub fn split_uri(raw: &str) -> Option<(String, String, Option<String>, String)> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    if scheme != "mongodb" && scheme != "mongodb+srv" { return None; }
    let (auth, hosts) = match rest.rfind('@') { Some(i) if rest[..i].find('/').is_none() => (&rest[..i], &rest[i + 1..]), _ => ("", rest) };
    if hosts.is_empty() { return None; }
    let dec = |s: &str| percent_decode(s);
    let (user, pass) = match auth.split_once(':') { Some((u, p)) => (dec(u), Some(dec(p))), None => (dec(auth), None) };
    let path = hosts.split_once('/').map(|x| x.1).unwrap_or("");
    let db = path.split('?').next().unwrap_or("").to_string();
    Some((format!("{scheme}://{hosts}"), user, pass, db))
}
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && s.is_char_boundary(i + 3) {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexRow { pub name: String, pub keys: String, pub unique: bool, pub ttl: Option<u64> }

/// `[ {stage}, ... ]` typed by the user (a single object is taken as one stage).
pub fn parse_pipeline(s: &str) -> Result<Vec<Document>> {
    let s = s.trim();
    if s.is_empty() { return Ok(vec![]); }
    let v: serde_json::Value = serde_json::from_str(s).with_context(|| crate::i18n::t("mongo.err_json").to_string())?;
    let items = match v { serde_json::Value::Array(a) => a, o => vec![o] };
    items.into_iter().map(|i| match Bson::try_from(i)? { Bson::Document(d) => Ok(d), _ => bail!(crate::i18n::t("mongo.err_object").to_string()) }).collect()
}

/// Document as pretty relaxed Extended JSON.
pub fn to_json(d: &Document) -> String {
    serde_json::to_string_pretty(&Bson::Document(d.clone()).into_relaxed_extjson()).unwrap_or_default()
}

/// One-line preview of a document for list rows.
pub fn preview(d: &Document, max: usize) -> String {
    let s = Bson::Document(d.clone()).into_relaxed_extjson().to_string();
    if s.chars().count() <= max { s } else { s.chars().take(max).collect::<String>() + "…" }
}

/// Text for the `_id` column.
pub fn id_text(d: &Document) -> String {
    match d.get("_id") {
        Some(Bson::ObjectId(o)) => o.to_hex(),
        Some(Bson::String(s)) => s.clone(),
        Some(b) => b.clone().into_relaxed_extjson().to_string(),
        None => String::new(),
    }
}

impl Mongo {
    /// `c.host` may hold a full `mongodb://` / `mongodb+srv://` URI (then user/password/port
    /// come from it unless set on the connection).
    pub async fn open(c: &DbConn, password: Option<String>, via: Option<(SshTarget, Prompts)>) -> Result<Mongo> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut tunnels = vec![];
        let mut via_label = None;
        let uri = if c.host.starts_with("mongodb") { c.host.clone() } else {
            let (host, port) = match via {
                Some((t, prompts)) => {
                    let (r, p) = crate::db::live::tunnel_to(&t, &prompts, &c.host, c.effective_port()).await?;
                    tunnels.push(r);
                    via_label = Some(t.label());
                    ("127.0.0.1".to_string(), p)
                }
                None => (c.host.clone(), c.effective_port()),
            };
            let host = if host.contains(':') { format!("[{host}]") } else { host };
            format!("mongodb://{host}:{port}/?directConnection=true")
        };
        let mut o = ClientOptions::parse(&uri).await?;
        o.app_name = Some("ShellAX".into());
        o.connect_timeout = Some(c.connect_timeout());
        o.server_selection_timeout = Some(c.connect_timeout());
        if !c.user.is_empty() {
            let mut cred = mongodb::options::Credential::default();
            cred.username = Some(c.user.clone());
            cred.password = password.clone();
            cred.source = o.credential.as_ref().and_then(|x| x.source.clone()).or(Some(if c.database.is_empty() { "admin".into() } else { c.database.clone() }));
            o.credential = Some(cred);
        }
        if matches!(c.tls.as_str(), "require" | "verify-ca" | "verify-full") {
            let mut t = mongodb::options::TlsOptions::default();
            if c.tls == "require" { t.allow_invalid_certificates = Some(true); }
            if !c.tls_ca.is_empty() { t.ca_file_path = Some(c.tls_ca.clone().into()); }
            o.tls = Some(mongodb::options::Tls::Enabled(t));
        }
        let client = Client::with_options(o)?;
        let info = match client.database("admin").run_command(doc! { "buildInfo": 1 }).await {
            Ok(i) => i,
            Err(e) => { for t in &tunnels { t.stop().await; } return Err(e.into()); }
        };
        Ok(Mongo { client, version: info.get_str("version").unwrap_or_default().to_string(), via: via_label, read_only: c.read_only, tunnels })
    }

    pub async fn close(&self) { self.client.clone().shutdown().await; for t in &self.tunnels { t.stop().await; } }

    pub async fn databases(&self) -> Result<Vec<String>> { let mut v = self.client.list_database_names().await?; v.sort(); Ok(v) }
    pub async fn collections(&self, db: &str) -> Result<Vec<String>> { let mut v = self.client.database(db).list_collection_names().await?; v.sort(); Ok(v) }

    /// Members of a tree group of database `db`: "collections", "views", "gridfs" (bucket names) or
    /// "indexes" (`collection.index`).
    pub async fn tree_group(&self, db: &str, group: &str) -> Result<Vec<String>> {
        let mut cur = self.client.database(db).list_collections().await?;
        let mut items = vec![];
        while cur.advance().await? {
            let s = cur.deserialize_current()?;
            items.push((s.name, matches!(s.collection_type, mongodb::results::CollectionType::View)));
        }
        if group != "indexes" { return Ok(tree_names(&items, group)); }
        let mut v = vec![];
        for c in tree_names(&items, "collections") { for i in self.indexes(db, &c).await? { v.push(format!("{c}.{}", i.name)); } }
        Ok(v)
    }

    pub async fn find(&self, db: &str, coll: &str, filter: &str, sort: &str, projection: &str, skip: u64) -> Result<FindPage> {
        let (f, s, p) = (parse_doc(filter)?, parse_doc(sort)?, parse_doc(projection)?);
        let c = self.client.database(db).collection::<Document>(coll);
        let mut q = c.find(f.clone()).skip(skip).limit(PAGE as i64);
        if !s.is_empty() { q = q.sort(s); }
        if !p.is_empty() { q = q.projection(p); }
        let mut cur = q.await?;
        let mut docs = vec![];
        while cur.advance().await? { docs.push(cur.deserialize_current()?); }
        let total = if skip == 0 { Some(c.count_documents(f).await?) } else { None };
        Ok(FindPage { docs, total })
    }

    fn guard(&self) -> Result<()> { if self.read_only { bail!(crate::i18n::t("db.err_read_only").to_string()); } Ok(()) }

    /// Replace the document with the same `_id` by `json` (its `_id` must not change).
    pub async fn replace(&self, db: &str, coll: &str, id: &Bson, json: &str) -> Result<()> {
        self.guard()?;
        let d = parse_doc(json)?;
        if d.get("_id").is_some_and(|x| x != id) { bail!(crate::i18n::t("mongo.err_id_changed").to_string()); }
        let r = self.client.database(db).collection::<Document>(coll).replace_one(doc! { "_id": id.clone() }, d).await?;
        if r.matched_count == 0 { bail!(crate::i18n::t("mongo.err_gone").to_string()); }
        Ok(())
    }

    pub async fn insert(&self, db: &str, coll: &str, json: &str) -> Result<Bson> {
        self.guard()?;
        Ok(self.client.database(db).collection::<Document>(coll).insert_one(parse_doc(json)?).await?.inserted_id)
    }

    /// Indexes: (name, key spec as JSON, unique, ttl seconds).
    pub async fn indexes(&self, db: &str, coll: &str) -> Result<Vec<IndexRow>> {
        let mut cur = self.client.database(db).collection::<Document>(coll).list_indexes().await?;
        let mut v = vec![];
        while cur.advance().await? {
            let m = cur.deserialize_current()?;
            let o = m.options.unwrap_or_default();
            v.push(IndexRow { name: o.name.unwrap_or_default(), keys: Bson::Document(m.keys).into_relaxed_extjson().to_string(), unique: o.unique.unwrap_or(false), ttl: o.expire_after.map(|d| d.as_secs()) });
        }
        Ok(v)
    }

    /// Run a pipeline (JSON array of stages); at most `limit` result documents.
    /// Read-only connections refuse `$out` / `$merge`.
    pub async fn aggregate(&self, db: &str, coll: &str, pipeline: &str, limit: usize) -> Result<Vec<Document>> {
        let stages = parse_pipeline(pipeline)?;
        if stages.iter().any(|s| s.contains_key("$out") || s.contains_key("$merge")) { self.guard()?; }
        let mut cur = self.client.database(db).collection::<Document>(coll).aggregate(stages).await?;
        let mut v = vec![];
        while v.len() < limit && cur.advance().await? { v.push(cur.deserialize_current()?); }
        Ok(v)
    }

    /// Every document matching the query, as a pretty JSON array, written to `path`.
    pub async fn export(&self, db: &str, coll: &str, filter: &str, sort: &str, projection: &str, path: &std::path::Path) -> Result<u64> {
        let (f, s, p) = (parse_doc(filter)?, parse_doc(sort)?, parse_doc(projection)?);
        let c = self.client.database(db).collection::<Document>(coll);
        let mut q = c.find(f);
        if !s.is_empty() { q = q.sort(s); }
        if !p.is_empty() { q = q.projection(p); }
        let mut cur = q.await?;
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        w.write_all(b"[")?;
        let mut n = 0u64;
        while cur.advance().await? {
            let d: Document = cur.deserialize_current()?;
            w.write_all(if n == 0 { b"\n" } else { b",\n" })?;
            w.write_all(serde_json::to_string(&Bson::Document(d).into_relaxed_extjson())?.as_bytes())?;
            n += 1;
        }
        w.write_all(b"\n]\n")?;
        w.flush()?;
        Ok(n)
    }

    pub async fn delete(&self, db: &str, coll: &str, id: &Bson) -> Result<u64> {
        self.guard()?;
        Ok(self.client.database(db).collection::<Document>(coll).delete_one(doc! { "_id": id.clone() }).await?.deleted_count)
    }
}


/// Split `listCollections` results (name, is view) into tree groups (sorted). GridFS buckets show as
/// one entry (`fs`) in their own group instead of their `.files` / `.chunks` collections.
pub fn tree_names(items: &[(String, bool)], group: &str) -> Vec<String> {
    let gridfs = |n: &str| n.strip_suffix(".files").filter(|b| items.iter().any(|(x, _)| *x == format!("{b}.chunks"))).map(str::to_string);
    let bucket = |n: &str| gridfs(n).is_some() || n.strip_suffix(".chunks").is_some_and(|b| items.iter().any(|(x, _)| *x == format!("{b}.files")));
    let mut v: Vec<String> = match group {
        "views" => items.iter().filter(|(_, view)| *view).map(|(n, _)| n.clone()).collect(),
        "gridfs" => items.iter().filter_map(|(n, _)| gridfs(n)).collect(),
        _ => items.iter().filter(|(n, view)| !*view && !bucket(n) && !n.starts_with("system.")).map(|(n, _)| n.clone()).collect(),
    };
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    #[test]
    fn tree_groups_split() {
        let it: Vec<(String, bool)> = [("users", false), ("v_active", true), ("fs.files", false), ("fs.chunks", false), ("system.views", false), ("a.files", false)].iter().map(|(n, v)| (n.to_string(), *v)).collect();
        assert_eq!(super::tree_names(&it, "collections"), vec!["a.files", "users"]);
        assert_eq!(super::tree_names(&it, "views"), vec!["v_active"]);
        assert_eq!(super::tree_names(&it, "gridfs"), vec!["fs"]);
    }
    use super::*;
    #[test]
    fn uris() {
        let (u, user, pw, db) = split_uri("mongodb+srv://ad%40x:p%3Aw@cl0.example.net/shop?retryWrites=true&authSource=admin").unwrap();
        assert_eq!((u.as_str(), user.as_str(), pw.as_deref(), db.as_str()), ("mongodb+srv://cl0.example.net/shop?retryWrites=true&authSource=admin", "ad@x", Some("p:w"), "shop"));
        let (u, user, pw, db) = split_uri("mongodb://h1:27017,h2:27018/?replicaSet=rs0").unwrap();
        assert_eq!((u.as_str(), user.as_str(), pw, db.as_str()), ("mongodb://h1:27017,h2:27018/?replicaSet=rs0", "", None, ""));
        assert!(split_uri("mysql://a@b/c").is_none());
    }
    #[test]
    fn pipelines() {
        let p = parse_pipeline(r#"[{"$match":{"a":1}},{"$limit":5}]"#).unwrap();
        assert_eq!((p.len(), p[1].get_i64("$limit").ok().or(p[1].get_i32("$limit").ok().map(i64::from))), (2, Some(5)));
        assert_eq!(parse_pipeline(r#"{"$count":"n"}"#).unwrap().len(), 1);
        assert!(parse_pipeline("[1]").is_err() && parse_pipeline("").unwrap().is_empty());
    }
    #[test]
    fn json_docs() {
        let d = parse_doc(r#"{"_id": {"$oid": "65a000000000000000000001"}, "n": 1, "at": {"$date": "2024-01-02T03:04:05Z"}}"#).unwrap();
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert!(matches!(d.get("at"), Some(Bson::DateTime(_))));
        assert_eq!(id_text(&d), "65a000000000000000000001");
        assert!(to_json(&d).contains("\"$oid\""));
        assert!(parse_doc("[1]").is_err() && parse_doc("{").is_err());
        assert_eq!(parse_doc("  ").unwrap(), Document::new());
        assert!(preview(&d, 10).ends_with('…'));
    }
}
