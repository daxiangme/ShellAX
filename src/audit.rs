//! Audit log: append-only JSONL with a SHA-256 hash chain (PRD 6.x).
//! Each record stores `prev` (hash of the previous record) and `hash` =
//! sha256(prev || seq || ts || kind || actor || detail). `verify` recomputes the chain.
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub seq: u64,
    /// RFC 3339 with local offset.
    pub ts: String,
    /// e.g. "ssh.connect", "hostkey.accept", "permission.mode", "settings.change"
    pub kind: String,
    /// "user" or an agent id
    pub actor: String,
    pub detail: String,
    pub prev: String,
    pub hash: String,
}

fn digest(prev: &str, seq: u64, ts: &str, kind: &str, actor: &str, detail: &str) -> String {
    let mut h = Sha256::new();
    for part in [prev, &seq.to_string(), ts, kind, actor, detail] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub struct AuditLog { path: PathBuf, tail: Mutex<Option<(u64, String)>> }

impl AuditLog {
    pub fn new(path: impl Into<PathBuf>) -> Self { Self { path: path.into(), tail: Mutex::new(None) } }
    pub fn path(&self) -> &Path { &self.path }

    pub fn append(&self, kind: &str, actor: &str, detail: &str) -> std::io::Result<Record> {
        let mut tail = self.tail.lock().unwrap();
        if tail.is_none() { *tail = Some(read_all(&self.path).last().map(|r| (r.seq, r.hash.clone())).unwrap_or((0, GENESIS.into()))); }
        let (seq, prev) = tail.clone().unwrap();
        let seq = seq + 1;
        let ts = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let hash = digest(&prev, seq, &ts, kind, actor, detail);
        let r = Record { seq, ts, kind: kind.into(), actor: actor.into(), detail: detail.into(), prev, hash: hash.clone() };
        if let Some(d) = self.path.parent() { std::fs::create_dir_all(d)?; }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600)); }
        writeln!(f, "{}", serde_json::to_string(&r).unwrap())?;
        *tail = Some((seq, hash));
        Ok(r)
    }
}

pub fn read_all(path: &Path) -> Vec<Record> {
    let Ok(f) = std::fs::File::open(path) else { return vec![] };
    std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

/// Verify the chain. Ok(count) or Err((line_no, reason)).
pub fn verify(path: &Path) -> Result<usize, (usize, String)> {
    let Ok(f) = std::fs::File::open(path) else { return Ok(0) };
    let mut prev = GENESIS.to_string();
    let mut n = 0;
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| (i + 1, e.to_string()))?;
        if line.trim().is_empty() { continue; }
        let r: Record = serde_json::from_str(&line).map_err(|e| (i + 1, format!("无法解析: {e}")))?;
        if r.prev != prev { return Err((i + 1, "prev 与上一条记录的 hash 不一致（记录被删除或重排）".into())); }
        if digest(&r.prev, r.seq, &r.ts, &r.kind, &r.actor, &r.detail) != r.hash { return Err((i + 1, "hash 不匹配（记录被篡改）".into())); }
        prev = r.hash;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chain_detects_tampering() {
        let p = std::env::temp_dir().join(format!("audit-test-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let log = AuditLog::new(&p);
        log.append("a", "user", "one").unwrap();
        log.append("b", "user", "two").unwrap();
        // a fresh handle continues the chain from the file
        AuditLog::new(&p).append("c", "claude", "three").unwrap();
        assert_eq!(verify(&p), Ok(3));
        let body = std::fs::read_to_string(&p).unwrap().replace("\"two\"", "\"TWO\"");
        std::fs::write(&p, body).unwrap();
        assert_eq!(verify(&p).unwrap_err().0, 2);
        let _ = std::fs::remove_file(&p);
    }
}
