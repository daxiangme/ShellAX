//! Audit log: append-only JSONL with a SHA-256 hash chain (PRD 6.x).
//! Each record stores `prev` (hash of the previous record) and `hash` =
//! sha256(prev || seq || ts || kind || actor || detail). `verify` recomputes the chain.
use crate::i18n::{t, tf};
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

    /// Retention (audit.retention_days): drop records older than `days` (0 = keep forever).
    /// The `prev` of the first kept record is stored in `<log>.anchor` so `verify` still checks the
    /// remaining chain end to end; the pruning itself is appended as an `audit.prune` record.
    /// Returns the number of records removed.
    pub fn prune(&self, days: u64) -> std::io::Result<usize> {
        if days == 0 { return Ok(0); }
        let cutoff = chrono::Local::now() - chrono::Duration::days(days as i64);
        let removed = {
            let mut tail = self.tail.lock().unwrap();
            let all = read_all(&self.path);
            let keep_from = all.iter().position(|r| chrono::DateTime::parse_from_rfc3339(&r.ts).map(|t| t >= cutoff).unwrap_or(true)).unwrap_or(all.len());
            if keep_from == 0 { return Ok(0); }
            let kept = &all[keep_from..];
            let anchor = kept.first().map(|r| r.prev.clone()).unwrap_or_else(|| all[keep_from - 1].hash.clone());
            let tmp = self.path.with_extension("jsonl.tmp");
            {
                let mut f = std::fs::File::create(&tmp)?;
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600)); }
                for r in kept { writeln!(f, "{}", serde_json::to_string(r).unwrap())?; }
                f.sync_all()?;
            }
            std::fs::write(anchor_path(&self.path), &anchor)?;
            std::fs::rename(&tmp, &self.path)?;
            *tail = Some(kept.last().map(|r| (r.seq, r.hash.clone())).unwrap_or((all[keep_from - 1].seq, anchor)));
            keep_from
        };
        self.append("audit.prune", "user", &format!("removed {removed} records older than {days} days"))?;
        Ok(removed)
    }
}

fn anchor_path(p: &Path) -> PathBuf { let mut s = p.as_os_str().to_owned(); s.push(".anchor"); PathBuf::from(s) }

pub fn read_all(path: &Path) -> Vec<Record> {
    let Ok(f) = std::fs::File::open(path) else { return vec![] };
    std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

/// Verify the chain. Ok(count) or Err((line_no, reason)).
pub fn verify(path: &Path) -> Result<usize, (usize, String)> {
    let Ok(f) = std::fs::File::open(path) else { return Ok(0) };
    // after retention pruning the chain starts at the stored anchor instead of genesis
    let mut prev = std::fs::read_to_string(anchor_path(path)).map(|s| s.trim().to_string()).unwrap_or_else(|_| GENESIS.to_string());
    let mut n = 0;
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| (i + 1, e.to_string()))?;
        if line.trim().is_empty() { continue; }
        let r: Record = serde_json::from_str(&line).map_err(|e| (i + 1, tf("audit.unparsable", &[("e", &e)])))?;
        if r.prev != prev { return Err((i + 1, t("audit.prev_mismatch").into())); }
        if digest(&r.prev, r.seq, &r.ts, &r.kind, &r.actor, &r.detail) != r.hash { return Err((i + 1, t("audit.hash_mismatch").into())); }
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

    #[test]
    fn prune_keeps_chain_verifiable() {
        let p = std::env::temp_dir().join(format!("audit-prune-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p); let _ = std::fs::remove_file(anchor_path(&p));
        // two old records (written by hand with a 2020 timestamp), then two current ones
        let mut prev = GENESIS.to_string();
        let mut body = String::new();
        for (seq, d) in [(1u64, "old1"), (2, "old2")] {
            let ts = "2020-01-01T00:00:00+08:00";
            let hash = digest(&prev, seq, ts, "k", "user", d);
            body += &serde_json::to_string(&Record { seq, ts: ts.into(), kind: "k".into(), actor: "user".into(), detail: d.into(), prev: prev.clone(), hash: hash.clone() }).unwrap();
            body.push('\n'); prev = hash;
        }
        std::fs::write(&p, body).unwrap();
        let log = AuditLog::new(&p);
        log.append("k", "user", "new1").unwrap();
        assert_eq!(verify(&p), Ok(3));
        assert_eq!(log.prune(30).unwrap(), 2);
        let all = read_all(&p);
        assert_eq!(all.iter().map(|r| r.detail.as_str()).collect::<Vec<_>>()[0], "new1");
        assert_eq!(all.last().unwrap().kind, "audit.prune");
        assert_eq!(verify(&p), Ok(2));
        log.append("k", "user", "after").unwrap();
        assert_eq!(verify(&p), Ok(3));
        assert_eq!(log.prune(30).unwrap(), 0);
        let _ = std::fs::remove_file(&p); let _ = std::fs::remove_file(anchor_path(&p));
    }
}
