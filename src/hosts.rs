//! Host list: entries from ~/.config/agentshell/config.toml `[[hosts]]` plus concrete `Host`
//! blocks in ~/.ssh/config. Original code written from the ssh_config(5) man page
//! (supports Host, HostName, User, Port, IdentityFile, Include is ignored; wildcard patterns
//! are not listed but their options apply as defaults, first-obtained-value-wins).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ssh::SshTarget;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct HostEntry {
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<String>,
    /// Sidebar group (config.toml hosts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// "key" | "agent" | "password" (password auth: not implemented yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// Production host: shown with a red tag; permission rules can target it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub production: bool,
    /// ProxyJump host (stored; jump connections not implemented yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<String>,
    /// "config" or "ssh_config"
    #[serde(skip)]
    pub source: &'static str,
}

fn home() -> PathBuf { std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default() }

pub fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~/") { Some(rest) => home().join(rest), None => PathBuf::from(p) }
}

/// Parse an ssh_config body into concrete host entries.
pub fn parse_ssh_config(body: &str) -> Vec<HostEntry> {
    // collect blocks: (patterns, options)
    let mut blocks: Vec<(Vec<String>, Vec<(String, String)>)> = vec![(vec!["*".into()], vec![])];
    let mut in_match = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let (k, v) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.to_ascii_lowercase(), v.trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim().trim_matches('"').to_string()),
            None => continue,
        };
        match k.as_str() {
            "host" => { in_match = false; blocks.push((v.split_whitespace().map(str::to_string).collect(), vec![])); }
            "match" => { in_match = true; }
            _ if in_match => {}
            _ => blocks.last_mut().unwrap().1.push((k, v)),
        }
    }
    let matches = |pat: &str, name: &str| -> bool { glob(pat, name) };
    let mut out = Vec::new();
    for (pats, _) in &blocks {
        for alias in pats {
            if alias.contains(['*', '?', '!']) { continue; }
            // first obtained value wins, over all blocks whose patterns match this alias, in file order
            let mut e = HostEntry { name: alias.clone(), host: alias.clone(), user: None, port: None, identity_file: None, source: "ssh_config", ..Default::default() };
            let (mut hn, mut us, mut po, mut id) = (false, false, false, false);
            for (bp, opts) in &blocks {
                let pos = bp.iter().any(|p| !p.starts_with('!') && matches(p, alias));
                let neg = bp.iter().any(|p| p.strip_prefix('!').is_some_and(|p| matches(p, alias)));
                if !pos || neg { continue; }
                for (k, v) in opts {
                    match k.as_str() {
                        "hostname" if !hn => { e.host = v.replace("%h", alias); hn = true; }
                        "user" if !us => { e.user = Some(v.clone()); us = true; }
                        "port" if !po => { e.port = v.parse().ok(); po = true; }
                        "identityfile" if !id => { e.identity_file = Some(v.clone()); id = true; }
                        _ => {}
                    }
                }
            }
            if !out.iter().any(|o: &HostEntry| o.name == e.name) { out.push(e); }
        }
    }
    out
}

/// ssh_config glob: `*` any run, `?` one char.
fn glob(pat: &str, s: &str) -> bool {
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    fn rec(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|i| rec(&p[1..], &s[i..])),
            Some('?') => !s.is_empty() && rec(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && rec(&p[1..], &s[1..]),
        }
    }
    rec(&p, &s)
}

pub fn load_ssh_config(path: &Path) -> Vec<HostEntry> {
    std::fs::read_to_string(path).map(|b| parse_ssh_config(&b)).unwrap_or_default()
}

/// Turn an entry into a connect target. Defaults: $USER, port 22, ~/.ssh/id_ed25519 + id_rsa.
pub fn to_target(e: &HostEntry, known_hosts: &Path) -> SshTarget {
    let mut keys = Vec::new();
    if let Some(f) = &e.identity_file { keys.push(expand_tilde(f)); }
    for d in ["id_ed25519", "id_ecdsa", "id_rsa"] { let p = home().join(".ssh").join(d); if !keys.contains(&p) { keys.push(p); } }
    SshTarget {
        host: e.host.clone(),
        port: e.port.unwrap_or(22),
        user: e.user.clone().unwrap_or_else(|| std::env::var("USER").unwrap_or_default()),
        keys,
        known_hosts: known_hosts.to_path_buf(),
        production: e.production,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ssh_config() {
        let cfg = "\
# comment
Host web prod-*
  HostName 10.0.0.5
  User deploy
Host db
  HostName db.internal
  Port 2200
  IdentityFile ~/.ssh/db_key
Host *
  User fallback
  Port 22
Match host foo
  User nobody
";
        let h = parse_ssh_config(cfg);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].name.as_str(), h[0].host.as_str(), h[0].user.as_deref(), h[0].port), ("web", "10.0.0.5", Some("deploy"), Some(22)));
        assert_eq!((h[1].host.as_str(), h[1].user.as_deref(), h[1].port), ("db.internal", Some("fallback"), Some(2200)));
        assert_eq!(h[1].identity_file.as_deref(), Some("~/.ssh/db_key"));
    }
    #[test]
    fn glob_works() { assert!(glob("prod-*", "prod-1")); assert!(glob("h?st", "host")); assert!(!glob("a*", "b")); }
}
