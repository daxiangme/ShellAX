//! Layout snapshots: keep the last N serialized layouts (JSON file), restore any of them.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Snap { pub at: i64, pub name: String, pub layout: serde_json::Value }
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct Store { pub snaps: Vec<Snap> }
impl Store {
    pub fn load(p: &std::path::Path) -> Self { std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default() }
    pub fn save(&self, p: &std::path::Path) -> std::io::Result<()> {
        if let Some(d) = p.parent() { std::fs::create_dir_all(d)?; }
        let tmp = p.with_extension("tmp"); std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?; std::fs::rename(tmp, p)
    }
    /// Push a snapshot unless identical to the newest; keep at most `keep` (newest first).
    pub fn push(&mut self, s: Snap, keep: usize) -> bool {
        if self.snaps.first().is_some_and(|f| f.layout == s.layout) { return false; }
        self.snaps.insert(0, s); self.snaps.truncate(keep.max(1)); true
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn ring_and_roundtrip() {
        let mut st = super::Store::default();
        for i in 0..5 { assert!(st.push(super::Snap { at: i, name: format!("s{i}"), layout: serde_json::json!({"n": i}) }, 3)); }
        assert!(!st.push(super::Snap { at: 9, name: "dup".into(), layout: serde_json::json!({"n": 4}) }, 3));
        assert_eq!(st.snaps.iter().map(|s| s.at).collect::<Vec<_>>(), vec![4, 3, 2]);
        let p = std::env::temp_dir().join(format!("snap-{}.json", std::process::id()));
        st.save(&p).unwrap(); assert_eq!(super::Store::load(&p).snaps, st.snaps);
    }
}
