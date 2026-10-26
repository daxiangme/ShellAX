//! Query history (most recent first, bounded) and named saved queries, kept as JSON in the
//! data directory.
use serde::{Deserialize, Serialize};

pub const MAX_HISTORY: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub sql: String,
    pub conn: String,
    #[serde(default)]
    pub database: String,
    /// Unix seconds.
    pub at: i64,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Saved {
    pub name: String,
    pub sql: String,
    #[serde(default)]
    pub conn: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Store {
    #[serde(default)]
    pub history: Vec<Entry>,
    #[serde(default)]
    pub saved: Vec<Saved>,
}

impl Store {
    pub fn path() -> Option<std::path::PathBuf> { Some(crate::platform::config_dir().join("db_queries.json")) }

    pub fn load() -> Store {
        Self::path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(p) = Self::path() else { return Ok(()) };
        if let Some(d) = p.parent() { std::fs::create_dir_all(d)?; }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    /// Record a run; a repeat of the latest entry for the same connection just moves it up.
    pub fn record(&mut self, e: Entry) {
        let sql = e.sql.trim();
        if sql.is_empty() { return; }
        self.history.retain(|h| !(h.sql.trim() == sql && h.conn == e.conn));
        self.history.insert(0, e);
        self.history.truncate(MAX_HISTORY);
    }

    /// Save under `name`, replacing a query of the same name.
    pub fn save_query(&mut self, s: Saved) {
        self.saved.retain(|x| x.name != s.name);
        self.saved.push(s);
        self.saved.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    }

    pub fn remove_saved(&mut self, name: &str) { self.saved.retain(|x| x.name != name); }

    pub fn search(&self, q: &str) -> Vec<&Entry> {
        let q = q.to_lowercase();
        self.history.iter().filter(|e| q.is_empty() || e.sql.to_lowercase().contains(&q)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_and_saved() {
        let mut s = Store::default();
        let e = |sql: &str, at| Entry { sql: sql.into(), conn: "c".into(), database: String::new(), at, ok: true, ms: 1 };
        s.record(e("select 1", 1));
        s.record(e("select 2", 2));
        s.record(e(" select 1 ", 3));
        s.record(e("  ", 4));
        assert_eq!(s.history.iter().map(|h| h.at).collect::<Vec<_>>(), vec![3, 2]);
        for i in 0..600 { s.record(e(&format!("q{i}"), i)); }
        assert_eq!(s.history.len(), MAX_HISTORY);
        assert_eq!(s.search("q599").len(), 1);
        s.save_query(Saved { name: "b".into(), sql: "1".into(), conn: String::new() });
        s.save_query(Saved { name: "A".into(), sql: "2".into(), conn: String::new() });
        s.save_query(Saved { name: "b".into(), sql: "3".into(), conn: String::new() });
        assert_eq!(s.saved.iter().map(|x| (x.name.as_str(), x.sql.as_str())).collect::<Vec<_>>(), vec![("A", "2"), ("b", "3")]);
        let back: Store = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
