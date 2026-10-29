//! SSH key generation and import (PRD 3.x credentials). Keys are written in OpenSSH format with
//! 0600 permissions; an optional passphrase encrypts the private key.
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use russh::keys::ssh_key::{rand_core::UnwrapErr, LineEnding};
use russh::keys::{Algorithm, HashAlg, PrivateKey};

fn rng() -> UnwrapErr<getrandom::SysRng> { UnwrapErr(getrandom::SysRng) }

#[cfg(unix)]
fn private_perms(p: &Path) -> Result<()> { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?; Ok(()) }
#[cfg(not(unix))]
fn private_perms(_p: &Path) -> Result<()> { Ok(()) }

/// First free `<dir>/<stem>`, `<stem>_2`, … (checks both the key and the .pub).
pub fn free_path(dir: &Path, stem: &str) -> PathBuf {
    let mut i = 1;
    loop {
        let name = if i == 1 { stem.to_string() } else { format!("{stem}_{i}") };
        let p = dir.join(&name);
        if !p.exists() && !p.with_extension("pub").exists() && !dir.join(format!("{name}.pub")).exists() { return p; }
        i += 1;
    }
}

/// Generate a key (`ed25519` or `ecdsa`/`rsa` not offered: ed25519 is the default and only
/// choice in the UI). Returns (private path, public key line, SHA256 fingerprint).
pub fn generate(path: &Path, comment: &str, passphrase: Option<&str>) -> Result<(PathBuf, String, String)> {
    if path.exists() { bail!("{} already exists", path.display()); }
    let mut key = PrivateKey::random(&mut rng(), Algorithm::Ed25519)?;
    key.set_comment(comment);
    let fp = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let public = key.public_key().to_openssh()?;
    let key = match passphrase.filter(|p| !p.is_empty()) { Some(p) => key.encrypt(&mut rng(), p)?, None => key };
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    let pem = key.to_openssh(LineEnding::LF)?;
    std::fs::write(path, pem.as_bytes())?;
    private_perms(path)?;
    std::fs::write(pub_path(path), format!("{public}\n"))?;
    Ok((path.to_path_buf(), public, fp))
}

pub fn pub_path(p: &Path) -> PathBuf { PathBuf::from(format!("{}.pub", p.display())) }

/// Import an existing private key: validate it parses (encrypted keys are accepted), copy it
/// into `dir` with 0600 and write the .pub next to it. Returns the new path and fingerprint.
pub fn import(src: &Path, dir: &Path) -> Result<(PathBuf, String)> {
    let text = std::fs::read_to_string(src).with_context(|| format!("read {}", src.display()))?;
    let key = PrivateKey::from_openssh(&text).context("not an OpenSSH private key")?;
    let fp = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let stem = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "imported_key".into());
    let dest = free_path(dir, &stem);
    std::fs::create_dir_all(dir)?;
    std::fs::write(&dest, text.as_bytes())?;
    private_perms(&dest)?;
    std::fs::write(pub_path(&dest), format!("{}\n", key.public_key().to_openssh()?))?;
    Ok((dest, fp))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generate_import_and_load() {
        let dir = std::env::temp_dir().join(format!("as-keygen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = free_path(&dir, "shellax_ed25519");
        let (_, public, fp) = generate(&p, "test@shellax", Some("pw")).unwrap();
        assert!(public.starts_with("ssh-ed25519 ") && fp.starts_with("SHA256:"));
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600); }
        assert!(matches!(russh::keys::load_secret_key(&p, None), Err(russh::keys::Error::KeyIsEncrypted)));
        assert!(russh::keys::load_secret_key(&p, Some("pw")).is_ok());
        assert_eq!(free_path(&dir, "shellax_ed25519").file_name().unwrap(), "shellax_ed25519_2");
        let (q, fp2) = import(&p, &dir.join("imp")).unwrap();
        assert_eq!(fp, fp2);
        assert!(q.exists() && pub_path(&q).exists());
        let p2 = dir.join("plain");
        generate(&p2, "x", None).unwrap();
        assert!(russh::keys::load_secret_key(&p2, None).is_ok());
        assert!(import(&dir.join("plain.pub"), &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
