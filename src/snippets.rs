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
    /// Legacy field: older files grouped snippets by module (terminal | sftp | database | redis |
    /// docker). The list is now shared by every module, so the value is read but ignored and
    /// never written back.
    #[serde(default, skip_serializing)]
    pub module: String,
}

/// Example snippets offered when the whole list is empty: terminal, SQL, Redis and Docker
/// examples together, told apart by their group. (name, group, body)
pub fn starters() -> Vec<(&'static str, &'static str, &'static str)> {
    ["terminal", "database", "redis", "docker"].into_iter().flat_map(|m| starters_for(m).iter().copied()).collect()
}

fn starters_for(module: &str) -> &'static [(&'static str, &'static str, &'static str)] {
    match module {
        "terminal" => &[("disk", "ops", "df -h"), ("listening ports", "ops", "ss -tlnp"), ("tail log", "ops", "tail -n 200 -f {{file}}"), ("top memory", "ops", "ps aux --sort=-%mem | head -n 15")],
        "database" => &[("row count", "sql", "SELECT COUNT(*) FROM {{table}};"), ("latest rows", "sql", "SELECT * FROM {{table}} ORDER BY {{column}} DESC LIMIT 50;"), ("duplicates", "sql", "SELECT {{column}}, COUNT(*) FROM {{table}} GROUP BY {{column}} HAVING COUNT(*) > 1;")],
        "redis" => &[("memory", "info", "INFO memory"), ("key type / ttl", "keys", "TYPE {{key}}"), ("scan prefix", "keys", "SCAN 0 MATCH {{prefix}}* COUNT 100"), ("slow log", "info", "SLOWLOG GET 20")],
        "docker" => &[("containers", "docker", "docker ps -a"), ("logs", "docker", "docker logs --tail 200 -f {{container}}"), ("disk usage", "docker", "docker system df"), ("prune images", "docker", "docker image prune -f")],
        _ => &[],
    }
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
    fn legacy_module_is_read_but_never_written() {
        let s: Store = toml::from_str("[[snippets]]\nid = \"x\"\nname = \"n\"\nbody = \"ls\"\n[[snippets]]\nid = \"y\"\nname = \"q\"\nbody = \"INFO\"\nmodule = \"redis\"\n").unwrap();
        assert_eq!(s.snippets[0].module, "");
        assert_eq!(s.snippets[1].module, "redis");
        // every old snippet stays in the one shared list, whichever module it was saved under
        assert_eq!(s.snippets.len(), 2);
        let out = toml::to_string_pretty(&s).unwrap();
        assert!(!out.contains("module"), "{out}");
    }

    #[test]
    fn starters_cover_all_kinds_in_one_set() {
        let all = starters();
        for g in ["ops", "sql", "info", "keys", "docker"] { assert!(all.iter().any(|(_, gr, _)| *gr == g), "{g}"); }
        let mut names: Vec<_> = all.iter().map(|(n, _, _)| *n).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len(), "starter names must be unique");
    }

    #[test]
    fn round_trip_fill_and_script_lines() {
        let dir = std::env::temp_dir().join(format!("shellax-snip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("snippets.toml");
        let item = Snippet { id: "a".into(), name: "磁盘".into(), group: "巡检".into(), note: "df".into(), body: "df -h {{path}}".into(), module: String::new() };
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
