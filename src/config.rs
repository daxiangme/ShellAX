//! User config: `~/.config/agentshell/config.toml` (all keys optional), env overrides for quick tests.
//!
//! ```toml
//! theme = "Catppuccin Mocha"   # or "system"
//! [font]
//! family = "JetBrains Mono"
//! fallbacks = ["Noto Sans Mono CJK SC", "Symbols Nerd Font Mono", "Noto Color Emoji"]
//! size = 13.5
//! line_height = 1.4            # multiple of font size
//! ligatures = false
//! ```
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileConfig { pub theme: Option<String>, #[serde(default)] pub font: FontFile }
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

pub struct Config { pub theme: Option<String>, pub font: FontConfig }

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
    Config { theme: std::env::var("THEME").ok().or(file.theme), font: f }
}
