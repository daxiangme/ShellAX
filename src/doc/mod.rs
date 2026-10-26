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
            cred.source = Some(if c.database.is_empty() { "admin".into() } else { c.database.clone() });
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

    pub async fn delete(&self, db: &str, coll: &str, id: &Bson) -> Result<u64> {
        self.guard()?;
        Ok(self.client.database(db).collection::<Document>(coll).delete_one(doc! { "_id": id.clone() }).await?.deleted_count)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
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
