//! Saved queries: plain files `<root>/<connection id>/<scope>/<name>.sql` (scope = database,
//! `database/schema`, or empty for connection level; Redis / Mongo commands use `.txt`).
//! The root sits in the config directory, so copying it carries the queries along.
use std::path::{Path, PathBuf};
use anyhow::{bail, Result};

pub fn root() -> PathBuf { crate::platform::config_dir().join("queries") }

fn clean(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "_".into() } else { s }
}

/// Directory of one tree node's saved queries.
pub fn dir(root: &Path, conn: &str, scope: &[&str]) -> PathBuf {
    let mut p = root.join(clean(conn));
    for s in scope { p.push(clean(s)); }
    p
}

const EXTS: [&str; 2] = ["sql", "txt"];

/// Saved query names (without extension), sorted.
pub fn list(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| { let p = e.path(); let ext = p.extension()?.to_str()?.to_string(); EXTS.contains(&ext.as_str()).then(|| p.file_stem()?.to_str().map(str::to_string)).flatten() })
        .collect();
    v.sort_by_key(|s| s.to_lowercase());
    v
}

pub fn path(dir: &Path, name: &str, ext: &str) -> PathBuf { dir.join(format!("{}.{ext}", clean(name))) }

fn find(dir: &Path, name: &str) -> Option<PathBuf> { EXTS.iter().map(|e| path(dir, name, e)).find(|p| p.is_file()) }

pub fn read(dir: &Path, name: &str) -> Result<String> {
    match find(dir, name) { Some(p) => Ok(std::fs::read_to_string(p)?), None => bail!("not found: {name}") }
}

pub fn write(dir: &Path, name: &str, ext: &str, text: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let p = path(dir, name, ext);
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// A free name: `base`, `base 2`, `base 3` …
pub fn free_name(dir: &Path, base: &str) -> String {
    let taken = list(dir);
    if !taken.iter().any(|n| n == base) { return base.to_string(); }
    (2..).map(|i| format!("{base} {i}")).find(|n| !taken.contains(n)).unwrap()
}

pub fn rename(dir: &Path, from: &str, to: &str) -> Result<()> {
    let Some(src) = find(dir, from) else { bail!("not found: {from}") };
    if find(dir, to).is_some() { bail!("exists: {to}") }
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("sql").to_string();
    std::fs::rename(&src, path(dir, to, &ext))?;
    Ok(())
}

/// Copy under a free name; returns the new name.
pub fn duplicate(dir: &Path, name: &str) -> Result<String> {
    let Some(src) = find(dir, name) else { bail!("not found: {name}") };
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("sql").to_string();
    let n = free_name(dir, name);
    std::fs::copy(&src, path(dir, &n, &ext))?;
    Ok(n)
}

pub fn delete(dir: &Path, name: &str) -> Result<()> {
    if let Some(p) = find(dir, name) { std::fs::remove_file(p)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crud() {
        let root = std::env::temp_dir().join(format!("saved-q-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let d = dir(&root, "c1", &["app", "pub/lic"]);
        assert!(d.ends_with("c1/app/pub_lic"));
        assert!(list(&d).is_empty());
        write(&d, "q", "sql", "select 1").unwrap();
        assert_eq!(free_name(&d, "q"), "q 2");
        assert_eq!(duplicate(&d, "q").unwrap(), "q 2");
        rename(&d, "q 2", "b").unwrap();
        assert!(rename(&d, "b", "q").is_err());
        assert_eq!(list(&d), ["b", "q"]);
        assert_eq!(read(&d, "b").unwrap(), "select 1");
        write(&d, "k", "txt", "GET a").unwrap();
        assert_eq!(read(&d, "k").unwrap(), "GET a");
        delete(&d, "b").unwrap();
        assert_eq!(list(&d), ["k", "q"]);
        assert_eq!(clean(" ../x "), "_x");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
