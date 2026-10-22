//! Saved commands (snippets). Stored beside config, not inside config.toml.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub body: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    snippets: Vec<Snippet>,
}

pub fn path() -> std::path::PathBuf { crate::platform::config_dir().join("snippets.toml") }

pub fn new_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{n:x}")
}

pub fn load() -> Vec<Snippet> { load_from(&path()) }

pub fn load_from(path: &std::path::Path) -> Vec<Snippet> {
    let Ok(body) = std::fs::read_to_string(path) else { return vec![] };
    match toml::from_str::<Store>(&body) {
        Ok(s) => s.snippets,
        Err(e) => { eprintln!("snippets: {e}"); vec![] }
    }
}

pub fn save(items: &[Snippet]) -> Result<(), String> { save_to(&path(), items) }

pub fn save_to(path: &std::path::Path, items: &[Snippet]) -> Result<(), String> {
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir).map_err(|e| e.to_string())?; }
    let body = toml::to_string_pretty(&Store { snippets: items.to_vec() }).map_err(|e| e.to_string())?;
    std::fs::write(path, body).map_err(|e| e.to_string())
}

pub fn upsert(item: Snippet) -> Result<(), String> {
    let mut all = load();
    if let Some(slot) = all.iter_mut().find(|s| s.id == item.id) { *slot = item; } else { all.push(item); }
    save(&all)
}

pub fn delete(id: &str) -> Result<(), String> {
    let mut all = load();
    all.retain(|s| s.id != id);
    save(&all)
}

pub fn duplicate(id: &str) -> Result<Option<Snippet>, String> {
    let mut all = load();
    let Some(src) = all.iter().find(|s| s.id == id).cloned() else { return Ok(None) };
    let copy = Snippet { id: new_id(), name: format!("{} (2)", src.name), ..src };
    all.push(copy.clone());
    save(&all)?;
    Ok(Some(copy))
}

/// `{{name}}` placeholders, first-seen order, trimmed, unique.
pub fn placeholder_names(body: &str) -> Vec<String> {
    let Ok(re) = regex::Regex::new(r"\{\{\s*([^{}\n]+?)\s*\}\}") else { return vec![] };
    let mut out = vec![];
    for c in re.captures_iter(body) {
        let name = c[1].trim().to_string();
        if !name.is_empty() && !out.iter().any(|n| n == &name) { out.push(name); }
    }
    out
}

/// Replace every placeholder. Empty or missing values are returned as the error list.
pub fn fill(body: &str, values: &[(String, String)]) -> Result<String, Vec<String>> {
    let names = placeholder_names(body);
    let missing: Vec<String> = names.into_iter().filter(|n| !values.iter().any(|(k, v)| k == n && !v.trim().is_empty())).collect();
    if !missing.is_empty() { return Err(missing); }
    let re = regex::Regex::new(r"\{\{\s*([^{}\n]+?)\s*\}\}").unwrap();
    Ok(re.replace_all(body, |c: &regex::Captures| {
        let name = c[1].trim();
        values.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default()
    }).into_owned())
}

/// Lines that would actually be sent. Blank lines and full-line `#` comments are skipped.
pub fn script_lines(body: &str) -> Vec<String> {
    body.lines().filter(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    }).map(|l| l.trim_end().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_fill_and_script_lines() {
        let dir = std::env::temp_dir().join(format!("shellax-snip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("snippets.toml");
        let item = Snippet { id: "a".into(), name: "磁盘".into(), group: "巡检".into(), note: "df".into(), body: "df -h {{path}}".into() };
        save_to(&p, &[item.clone()]).unwrap();
        let loaded = load_from(&p);
        assert_eq!(loaded, vec![item]);
        assert_eq!(placeholder_names("echo {{ name }} {{name}} {{host}}"), vec!["name".to_string(), "host".into()]);
        assert_eq!(fill("echo {{name}}", &[("name".into(), "ok".into())]).unwrap(), "echo ok");
        assert!(fill("echo {{name}}", &[("name".into(), "  ".into())]).is_err());
        let lines = script_lines("# comment\n\ndf -h\n  echo ok\n");
        assert_eq!(lines, vec!["df -h".to_string(), "  echo ok".into()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
