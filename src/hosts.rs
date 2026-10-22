//! Host list: entries from ~/.config/shellax/config.toml `[[hosts]]` plus concrete `Host`
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
    /// Finder-style color tags ("red", "blue", …; PRD 5.23).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// ProxyJump host (stored; jump connections not implemented yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<String>,
    /// Connect this host through the outbound proxy configured in Settings → Proxy.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub use_proxy: bool,
    /// OpenSSH certificate file for the "cert" auth method (PRD 5.24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
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
    type Block = (Vec<String>, Vec<(String, String)>);
    let mut blocks: Vec<Block> = vec![(vec!["*".into()], vec![])];
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
                        "proxyjump" if e.jump.is_none() && v != "none" => { e.jump = Some(v.clone()); }
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

/// Connection options that come from settings rather than the host entry.
#[derive(Debug, Clone, Default)]
pub struct SshOpts {
    pub default_user: String,
    pub default_port: u16,
    pub agent_forward: bool,
    pub kex: Vec<String>,
    pub ciphers: Vec<String>,
    /// Global ProxyJump used when a host has none.
    pub jump: String,
    pub remote_hooks: bool,
    /// credentials.use_ssh_agent (false: "auto" skips the agent)
    pub no_agent: bool,
    /// credentials.extra_keys, tried after the host's own key
    pub extra_keys: Vec<String>,
}

/// Resolve an entry (including its ProxyJump chain `a,b,…`, each a host name from `all` or
/// `[user@]host[:port]`) into a connect target.
pub fn resolve(e: &HostEntry, known_hosts: &Path, all: &[HostEntry], o: &SshOpts) -> SshTarget {
    resolve_depth(e, known_hosts, all, o, 0)
}

fn resolve_depth(e: &HostEntry, known_hosts: &Path, all: &[HostEntry], o: &SshOpts, depth: u8) -> SshTarget {
    let mut t = to_target(e, known_hosts);
    if e.port.is_none() && o.default_port != 0 { t.port = o.default_port; }
    if e.user.is_none() && !o.default_user.is_empty() { t.user = o.default_user.clone(); }
    t.agent_forward = o.agent_forward;
    t.kex = o.kex.clone();
    t.ciphers = o.ciphers.clone();
    t.remote_hooks = o.remote_hooks;
    if o.no_agent && t.auth == "auto" { t.auth = "auto-noagent".into(); }
    for k in &o.extra_keys { let p = expand_tilde(k.trim()); if !k.trim().is_empty() && !t.keys.contains(&p) { t.keys.insert(t.keys.len().min(1), p); } }
    let spec = e.jump.clone().filter(|j| !j.is_empty()).or_else(|| (depth == 0 && !o.jump.is_empty() && o.jump != e.name && o.jump != e.host).then(|| o.jump.clone()));
    if let (Some(spec), true) = (spec, depth < 4) {
        // OpenSSH semantics: "a,b" means connect to a, then through a to b, then to the target
        let mut prev: Option<Box<SshTarget>> = None;
        for hop in spec.split(',').map(str::trim).filter(|h| !h.is_empty()) {
            let he = all.iter().find(|h| h.name == hop).cloned().unwrap_or_else(|| parse_hop(hop));
            let mut ht = resolve_depth(&HostEntry { jump: None, ..he }, known_hosts, all, &SshOpts { jump: String::new(), remote_hooks: false, ..o.clone() }, depth + 1);
            ht.agent_forward = false;
            ht.jump = prev.take();
            prev = Some(Box::new(ht));
        }
        t.jump = prev;
    }
    if depth == 0 { arm_first_hop_proxy(&mut t); }
    t
}

/// The leaf's "use proxy" switch applies to the TCP hop that actually leaves this machine.
fn arm_first_hop_proxy(t: &mut SshTarget) {
    if !t.use_proxy { return; }
    let mut cur = &mut *t;
    while cur.jump.is_some() { cur = cur.jump.as_mut().unwrap(); }
    cur.use_proxy = true;
}

/// `[user@]host[:port]` → entry.
pub fn parse_hop(s: &str) -> HostEntry {
    let (user, rest) = match s.split_once('@') { Some((u, r)) => (Some(u.to_string()), r), None => (None, s) };
    let (host, port) = match rest.rsplit_once(':') { Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().ok()), _ => (rest.to_string(), None) };
    HostEntry { name: s.to_string(), host, user, port, source: "jump", ..Default::default() }
}

/// Turn an entry into a connect target. Defaults: $USER, port 22.
/// An explicit `key` auth method with a chosen identity uses only that file.
/// `auto` still falls back to ~/.ssh/id_ed25519, id_ecdsa and id_rsa.
pub fn to_target(e: &HostEntry, known_hosts: &Path) -> SshTarget {
    let mut keys = Vec::new();
    let chosen = e.identity_file.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(expand_tilde);
    let auth = e.auth.as_deref().unwrap_or("auto");
    let only_chosen = chosen.is_some() && auth.split(',').any(|m| m.trim() == "key") && !matches!(auth, "" | "auto" | "auto-noagent");
    if let Some(f) = chosen { keys.push(f); }
    if !only_chosen {
        for d in ["id_ed25519", "id_ecdsa", "id_rsa"] { let p = home().join(".ssh").join(d); if !keys.contains(&p) { keys.push(p); } }
    }
    SshTarget {
        cert: e.cert.as_deref().map(expand_tilde),
        host: e.host.clone(),
        port: e.port.unwrap_or(22),
        user: e.user.clone().unwrap_or_else(|| std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default()),
        keys,
        known_hosts: known_hosts.to_path_buf(),
        production: e.production,
        auth: e.auth.clone().unwrap_or_else(|| "auto".into()),
        use_proxy: e.use_proxy,
        ..Default::default()
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
    fn jump_chain_resolution() {
        let all = parse_ssh_config("Host bastion\n  HostName 10.0.0.1\n  User jump\nHost inner\n  HostName 10.0.0.2\n  ProxyJump bastion,ops@10.0.0.3:2222\n");
        let inner = all.iter().find(|h| h.name == "inner").unwrap();
        assert_eq!(inner.jump.as_deref(), Some("bastion,ops@10.0.0.3:2222"));
        let t = resolve(inner, Path::new("/kh"), &all, &SshOpts::default());
        let j1 = t.jump.as_ref().unwrap();
        assert_eq!((j1.user.as_str(), j1.host.as_str(), j1.port), ("ops", "10.0.0.3", 2222));
        let j0 = j1.jump.as_ref().unwrap();
        assert_eq!((j0.user.as_str(), j0.host.as_str()), ("jump", "10.0.0.1"));
        assert!(j0.jump.is_none());
        assert_eq!(t.pool_key(), "jump@10.0.0.1:22>ops@10.0.0.3:2222>".to_string() + &t.label());
        let g = resolve(all.iter().find(|h| h.name == "bastion").unwrap(), Path::new("/kh"), &all, &SshOpts { jump: "bastion".into(), ..Default::default() });
        assert!(g.jump.is_none(), "global jump is not applied to the jump host itself");
    }
    #[test]
    fn use_proxy_marks_the_first_hop() {
        let e = HostEntry { name: "inner".into(), host: "10.0.0.2".into(), jump: Some("ops@10.0.0.3:2222".into()), use_proxy: true, source: "config", ..Default::default() };
        let t = resolve(&e, Path::new("/kh"), &[], &SshOpts::default());
        assert!(t.use_proxy);
        let hop = t.jump.as_ref().unwrap();
        assert!(hop.use_proxy && hop.jump.is_none());
        assert!(hop.pool_key().ends_with("+proxy"));
        let direct = HostEntry { name: "web".into(), host: "example.com".into(), use_proxy: true, source: "config", ..Default::default() };
        let d = resolve(&direct, Path::new("/kh"), &[], &SshOpts::default());
        assert!(d.jump.is_none() && d.use_proxy && d.pool_key().ends_with("+proxy"));
    }
    #[test]
    fn glob_works() { assert!(glob("prod-*", "prod-1")); assert!(glob("h?st", "host")); assert!(!glob("a*", "b")); }
    #[test]
    fn explicit_key_auth_uses_only_the_chosen_file() {
        let e = HostEntry { name: "h".into(), host: "h".into(), identity_file: Some("~/.ssh/shellax_ed25519".into()), auth: Some("key".into()), source: "config", ..Default::default() };
        let t = to_target(&e, Path::new("known_hosts"));
        assert_eq!(t.auth, "key");
        assert_eq!(t.keys.len(), 1);
        assert!(t.keys[0].ends_with("shellax_ed25519"));
        let auto = HostEntry { auth: Some("auto".into()), ..e };
        assert!(to_target(&auto, Path::new("known_hosts")).keys.len() > 1);
    }
}
