//! User config: `~/.config/agentshell/config.toml` (all keys optional), env overrides for quick tests.
//!
//! ```toml
//! theme = "Catppuccin Mocha"   # or "system"
//! known_hosts = "~/.ssh/known_hosts"
//! ssh_config = "~/.ssh/config"
//! [font]
//! family = "JetBrains Mono"
//! fallbacks = ["Noto Sans Mono CJK SC", "Symbols Nerd Font Mono", "Noto Color Emoji"]
//! size = 13.5
//! line_height = 1.4            # multiple of font size
//! ligatures = false
//! [[hosts]]
//! name = "prod"
//! host = "10.0.0.5"
//! user = "deploy"
//! port = 22
//! identity_file = "~/.ssh/id_ed25519"
//! ```
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileConfig {
    pub theme: Option<String>,
    #[serde(default)] pub font: FontFile,
    /// Extra hosts shown in the sidebar (in addition to ~/.ssh/config).
    #[serde(default)] pub hosts: Vec<ssh_term_proto::hosts::HostEntry>,
    /// known_hosts file (default ~/.ssh/known_hosts).
    pub known_hosts: Option<String>,
    /// ssh_config file (default ~/.ssh/config).
    pub ssh_config: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Default)]
pub struct FontFile {
    pub family: Option<String>, pub fallbacks: Option<Vec<String>>,
    pub size: Option<f32>, pub line_height: Option<f32>, pub ligatures: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct FontConfig { pub family: String, pub fallbacks: Vec<String>, pub size: f32, pub line_height: f32, pub ligatures: bool }

impl FontConfig {
    /// Platform defaults: a monospace primary + CJK + Nerd Font symbols + color emoji.
    pub fn platform_default() -> Self {
        let (family, fallbacks): (&str, &[&str]) = if cfg!(target_os = "macos") {
            ("Menlo", &["PingFang SC", "Symbols Nerd Font Mono", "Apple Color Emoji"])
        } else if cfg!(target_os = "windows") {
            ("Cascadia Mono", &["Microsoft YaHei UI", "Symbols Nerd Font Mono", "Segoe UI Emoji", "Segoe UI Symbol"])
        } else {
            ("JetBrains Mono", &["Noto Sans Mono CJK SC", "Symbols Nerd Font Mono", "Noto Color Emoji", "DejaVu Sans Mono"])
        };
        Self { family: family.into(), fallbacks: fallbacks.iter().map(|s| s.to_string()).collect(), size: 13.5, line_height: 1.4, ligatures: false }
    }
}

pub struct Config {
    pub theme: Option<String>,
    pub font: FontConfig,
    pub hosts: Vec<ssh_term_proto::hosts::HostEntry>,
    pub known_hosts: std::path::PathBuf,
}

pub fn load() -> Config {
    let file: FileConfig = std::env::var("HOME").ok()
        .and_then(|h| std::fs::read_to_string(std::path::Path::new(&h).join(".config/agentshell/config.toml")).ok())
        .and_then(|s| toml::from_str(&s).map_err(|e| eprintln!("config.toml: {e}")).ok())
        .unwrap_or_default();
    let mut f = FontConfig::platform_default();
    if let Some(v) = file.font.family { f.family = v; }
    if let Some(v) = file.font.fallbacks { f.fallbacks = v; }
    if let Some(v) = file.font.size { f.size = v; }
    if let Some(v) = file.font.line_height { f.line_height = v; }
    if let Some(v) = file.font.ligatures { f.ligatures = v; }
    if let Ok(v) = std::env::var("TERM_FONT") { f.family = v; }
    if let Some(v) = std::env::var("TERM_FONT_SIZE").ok().and_then(|v| v.parse().ok()) { f.size = v; }
    use ssh_term_proto::hosts::{expand_tilde, load_ssh_config};
    let ssh_config = std::env::var("AGENTSHELL_SSH_CONFIG").ok().or(file.ssh_config).unwrap_or_else(|| "~/.ssh/config".into());
    let mut hosts: Vec<_> = file.hosts.into_iter().map(|mut h| { h.source = "config"; h }).collect();
    for h in load_ssh_config(&expand_tilde(&ssh_config)) { if !hosts.iter().any(|x| x.name == h.name) { hosts.push(h); } }
    let known_hosts = expand_tilde(&std::env::var("AGENTSHELL_KNOWN_HOSTS").ok().or(file.known_hosts).unwrap_or_else(|| "~/.ssh/known_hosts".into()));
    Config { theme: std::env::var("THEME").ok().or(file.theme), font: f, hosts, known_hosts }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_hosts_and_known_hosts() {
        let src = "theme = \"system\"\nknown_hosts = \"~/kh\"\n[font]\nsize = 14.0\n[[hosts]]\nname = \"prod\"\nhost = \"10.0.0.5\"\nuser = \"deploy\"\nport = 2200\n";
        let f: FileConfig = toml::from_str(src).unwrap();
        assert_eq!(f.known_hosts.as_deref(), Some("~/kh"));
        assert_eq!(f.font.size, Some(14.0));
        assert_eq!((f.hosts[0].name.as_str(), f.hosts[0].port), ("prod", Some(2200)));
    }
}
