//! Secret storage for API keys. Order: OS keychain via the `keyring` crate (macOS Keychain,
//! Windows Credential Manager, Linux Secret Service over D-Bus); if that is unavailable, an
//! encrypted local file (ChaCha20-Poly1305, files mode 0600). Secrets never go to config.toml.
//!
//! The fallback key lives next to the encrypted file, so the file protects against accidental
//! disclosure (backups, screenshots of the file, copying config around), not against another
//! process running as the same user. The UI states which backend holds each key.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;

pub const SERVICE: &str = "shellax";
/// Keyring service names used before the rename to ShellAX (newest first).
const LEGACY_SERVICES: [&str; 2] = ["coossh", "agentshell"];

#[derive(Debug, Clone, PartialEq)]
pub enum Location { Keychain, File(PathBuf) }

pub struct SecretStore { dir: PathBuf, use_keyring: bool }

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok())).collect()
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.create(true).write(true).truncate(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); }
        let mut f = o.open(&tmp)?;
        f.write_all(data)?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

impl SecretStore {
    /// `dir`: where the fallback files live. Keychain use can be disabled with
    /// SHELLAX_NO_KEYRING=1 (tests, headless machines).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), use_keyring: std::env::var("SHELLAX_NO_KEYRING").is_err() }
    }
    pub fn file_only(dir: impl Into<PathBuf>) -> Self { Self { dir: dir.into(), use_keyring: false } }
    pub fn file_path(&self) -> PathBuf { self.dir.join("secrets.enc") }
    fn key_path(&self) -> PathBuf { self.dir.join("secrets.key") }

    fn cipher(&self) -> Result<ChaCha20Poly1305> {
        let kp = self.key_path();
        let key = match std::fs::read(&kp) {
            Ok(k) if k.len() == 32 => k,
            _ => { let mut k = vec![0u8; 32]; rand::thread_rng().fill_bytes(&mut k); write_private(&kp, &k)?; k }
        };
        Ok(ChaCha20Poly1305::new(Key::from_slice(&key)))
    }
    fn load_file(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.file_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn file_set(&self, name: &str, secret: &str) -> Result<()> {
        let c = self.cipher()?;
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = c.encrypt(Nonce::from_slice(&nonce), secret.as_bytes()).map_err(|_| anyhow!("encrypt failed"))?;
        let mut m = self.load_file();
        m.insert(name.into(), format!("{}{}", hex(&nonce), hex(&ct)));
        write_private(&self.file_path(), serde_json::to_string_pretty(&m)?.as_bytes())
    }
    fn file_get(&self, name: &str) -> Option<String> {
        let v = self.load_file().get(name)?.clone();
        let raw = unhex(&v)?;
        if raw.len() < 13 { return None; }
        let pt = self.cipher().ok()?.decrypt(Nonce::from_slice(&raw[..12]), &raw[12..]).ok()?;
        String::from_utf8(pt).ok()
    }
    fn file_delete(&self, name: &str) -> Result<()> {
        let mut m = self.load_file();
        if m.remove(name).is_some() { write_private(&self.file_path(), serde_json::to_string_pretty(&m)?.as_bytes())?; }
        Ok(())
    }

    fn keyring_set(&self, name: &str, secret: &str) -> Result<()> {
        let e = keyring::Entry::new(SERVICE, name).context("keyring")?;
        e.set_password(secret).context("keyring set")?;
        // read back: some backends accept writes they cannot serve later
        anyhow::ensure!(e.get_password().ok().as_deref() == Some(secret), "keyring read-back mismatch");
        Ok(())
    }
    fn keyring_get(&self, name: &str) -> Option<String> {
        if let Some(v) = keyring::Entry::new(SERVICE, name).ok().and_then(|e| e.get_password().ok()) { return Some(v); }
        // pre-rename entry ("coossh" / "agentshell" service): copy it over once
        let v = LEGACY_SERVICES.iter().find_map(|s| keyring::Entry::new(s, name).ok()?.get_password().ok())?;
        let _ = self.keyring_set(name, &v);
        Some(v)
    }

    /// Store; returns where it ended up and, if the keychain failed, why.
    pub fn set(&self, name: &str, secret: &str) -> Result<(Location, Option<String>)> {
        let why;
        if self.use_keyring {
            match self.keyring_set(name, secret) {
                Ok(()) => { let _ = self.file_delete(name); return Ok((Location::Keychain, None)); }
                Err(e) => why = Some(format!("{e:#}")),
            }
        } else {
            why = Some("keychain disabled (SHELLAX_NO_KEYRING)".into());
        }
        self.file_set(name, secret)?;
        Ok((Location::File(self.file_path()), why))
    }
    pub fn get(&self, name: &str) -> Option<(String, Location)> {
        if self.use_keyring { if let Some(s) = self.keyring_get(name) { return Some((s, Location::Keychain)); } }
        self.file_get(name).map(|s| (s, Location::File(self.file_path())))
    }
    pub fn delete(&self, name: &str) {
        if self.use_keyring { if let Ok(e) = keyring::Entry::new(SERVICE, name) { let _ = e.delete_credential(); } }
        let _ = self.file_delete(name);
    }
}

/// "sk-abc…wxyz" -> "••••••••wxyz" (only the last 4 characters are ever shown).
pub fn mask(secret: &str) -> String {
    let n = secret.chars().count();
    let tail: String = secret.chars().skip(n.saturating_sub(4)).collect();
    if n <= 4 { "••••".into() } else { format!("••••••••{tail}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_backend_roundtrip_encrypted_and_private() {
        let dir = std::env::temp_dir().join(format!("as-secret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = SecretStore::file_only(&dir);
        let (loc, _) = st.set("ai:demo", "sk-test-SECRET-1234").unwrap();
        assert_eq!(loc, Location::File(dir.join("secrets.enc")));
        assert_eq!(st.get("ai:demo").unwrap().0, "sk-test-SECRET-1234");
        let raw = std::fs::read_to_string(dir.join("secrets.enc")).unwrap();
        assert!(!raw.contains("SECRET") && !raw.contains("1234"), "plaintext leaked into file");
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            for f in ["secrets.enc", "secrets.key"] {
                assert_eq!(std::fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
        st.delete("ai:demo");
        assert!(st.get("ai:demo").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn masks_all_but_last_four() {
        assert_eq!(mask("sk-abcdefgh1234"), "••••••••1234");
        assert_eq!(mask("abc"), "••••");
    }
}
