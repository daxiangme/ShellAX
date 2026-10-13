//! Theme system: UI color tokens + terminal palette, loaded from TOML.
//! Built-in themes are embedded from `themes/*.toml`; user themes from `~/.config/agentshell/themes/*.toml`.
use std::sync::{Arc, RwLock};

use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui::{rgb, Hsla, Rgba};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Appearance { Dark, Light }

#[derive(Clone, Debug, Deserialize)]
pub struct UiColors {
    pub crust: String, pub mantle: String, pub base: String,
    pub surface0: String, pub surface1: String, pub surface2: String,
    pub overlay0: String, pub overlay1: String,
    pub text: String, pub subtext: String,
    pub accent: String, pub success: String, pub error: String, pub warning: String, pub info: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct TermColors {
    pub foreground: String, pub background: String, pub cursor: String, pub cursor_text: String,
    pub selection: String, pub suggestion: String, pub ansi: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ThemeFile { pub name: String, pub appearance: Appearance, pub ui: UiColors, pub terminal: TermColors }

/// Resolved theme (parsed colors).
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    pub crust: u32, pub mantle: u32, pub base: u32,
    pub surface0: u32, pub surface1: u32, pub surface2: u32,
    pub overlay0: u32, pub overlay1: u32, pub text: u32, pub subtext: u32,
    pub accent: u32, pub success: u32, pub error: u32, pub warning: u32, pub info: u32,
    pub fg: u32, pub bg: u32, pub cursor: u32, pub cursor_text: u32, pub selection: u32, pub suggestion: u32,
    pub ansi: [u32; 16],
}

pub fn parse_hex(s: &str) -> anyhow::Result<u32> {
    let h = s.trim().trim_start_matches('#').trim_start_matches("0x");
    anyhow::ensure!(h.len() == 6, "bad color {s:?}");
    Ok(u32::from_str_radix(h, 16)?)
}

impl Theme {
    pub fn from_toml(src: &str) -> anyhow::Result<Theme> {
        let f: ThemeFile = toml::from_str(src)?;
        anyhow::ensure!(f.terminal.ansi.len() == 16, "terminal.ansi needs 16 colors");
        let mut ansi = [0u32; 16];
        for (i, s) in f.terminal.ansi.iter().enumerate() { ansi[i] = parse_hex(s)?; }
        let u = &f.ui; let t = &f.terminal;
        Ok(Theme {
            name: f.name, appearance: f.appearance,
            crust: parse_hex(&u.crust)?, mantle: parse_hex(&u.mantle)?, base: parse_hex(&u.base)?,
            surface0: parse_hex(&u.surface0)?, surface1: parse_hex(&u.surface1)?, surface2: parse_hex(&u.surface2)?,
            overlay0: parse_hex(&u.overlay0)?, overlay1: parse_hex(&u.overlay1)?,
            text: parse_hex(&u.text)?, subtext: parse_hex(&u.subtext)?,
            accent: parse_hex(&u.accent)?, success: parse_hex(&u.success)?, error: parse_hex(&u.error)?,
            warning: parse_hex(&u.warning)?, info: parse_hex(&u.info)?,
            fg: parse_hex(&t.foreground)?, bg: parse_hex(&t.background)?, cursor: parse_hex(&t.cursor)?,
            cursor_text: parse_hex(&t.cursor_text)?, selection: parse_hex(&t.selection)?, suggestion: parse_hex(&t.suggestion)?,
            ansi,
        })
    }

    /// Import an Alacritty color scheme (TOML `[colors.primary]`, `[colors.normal]`, `[colors.bright]`).
    /// UI tokens are derived from the terminal colors.
    pub fn from_alacritty_toml(name: &str, src: &str) -> anyhow::Result<Theme> {
        let v: toml::Value = toml::from_str(src)?;
        let colors = v.get("colors").ok_or_else(|| anyhow::anyhow!("no [colors]"))?;
        let get = |sec: &str, key: &str| -> anyhow::Result<u32> {
            let s = colors.get(sec).and_then(|t| t.get(key)).and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing colors.{sec}.{key}"))?;
            parse_hex(s)
        };
        let names = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
        let mut ansi = [0u32; 16];
        for (i, n) in names.iter().enumerate() { ansi[i] = get("normal", n)?; ansi[i + 8] = get("bright", n)?; }
        let fg = get("primary", "foreground")?;
        let bg = get("primary", "background")?;
        let light = luma(bg) > 0.5;
        let mix = |a: u32, b: u32, t: f32| -> u32 {
            let ch = |s: u32| -> [f32; 3] { [((s >> 16) & 255) as f32, ((s >> 8) & 255) as f32, (s & 255) as f32] };
            let (x, y) = (ch(a), ch(b));
            let m: Vec<u32> = (0..3).map(|i| (x[i] + (y[i] - x[i]) * t).round() as u32).collect();
            (m[0] << 16) | (m[1] << 8) | m[2]
        };
        Ok(Theme {
            name: name.into(), appearance: if light { Appearance::Light } else { Appearance::Dark },
            crust: mix(bg, if light { 0xffffff } else { 0 }, 0.35), mantle: mix(bg, if light { 0xffffff } else { 0 }, 0.18), base: bg,
            surface0: mix(bg, fg, 0.12), surface1: mix(bg, fg, 0.2), surface2: mix(bg, fg, 0.3),
            overlay0: mix(bg, fg, 0.45), overlay1: mix(bg, fg, 0.55), text: fg, subtext: mix(bg, fg, 0.75),
            accent: ansi[4], success: ansi[2], error: ansi[1], warning: ansi[3], info: ansi[6],
            fg, bg, cursor: get("cursor", "cursor").unwrap_or(fg), cursor_text: get("cursor", "text").unwrap_or(bg),
            selection: get("selection", "background").unwrap_or(mix(bg, fg, 0.3)), suggestion: mix(bg, fg, 0.45),
            ansi,
        })
    }
}

fn luma(c: u32) -> f32 {
    (0.2126 * ((c >> 16) & 255) as f32 + 0.7152 * ((c >> 8) & 255) as f32 + 0.0722 * (c & 255) as f32) / 255.0
}

pub const BUILTIN: &[&str] = &[
    include_str!("../themes/catppuccin-mocha.toml"),
    include_str!("../themes/catppuccin-latte.toml"),
    include_str!("../themes/agentshell-midnight.toml"),
    include_str!("../themes/agentshell-daylight.toml"),
];

pub fn load_all() -> Vec<Arc<Theme>> {
    let mut v: Vec<Arc<Theme>> = BUILTIN.iter().map(|s| Arc::new(Theme::from_toml(s).expect("builtin theme"))).collect();
    if let Ok(home) = std::env::var("HOME") {
        let dir = std::path::Path::new(&home).join(".config/agentshell/themes");
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                    if let Ok(src) = std::fs::read_to_string(&p) {
                        let t = Theme::from_toml(&src).or_else(|_| Theme::from_alacritty_toml(
                            p.file_stem().and_then(|s| s.to_str()).unwrap_or("imported"), &src));
                        match t { Ok(t) => v.push(Arc::new(t)), Err(e) => eprintln!("theme {}: {e:#}", p.display()) }
                    }
                }
            }
        }
    }
    v
}

static CURRENT: RwLock<Option<Arc<Theme>>> = RwLock::new(None);
pub fn set_current(t: Arc<Theme>) { *CURRENT.write().unwrap() = Some(t); }
pub fn cur() -> Arc<Theme> {
    CURRENT.read().unwrap().clone().unwrap_or_else(|| Arc::new(Theme::from_toml(BUILTIN[0]).unwrap()))
}

pub fn c(hex: u32) -> Hsla { Rgba::from(rgb(hex)).into() }

fn xterm256(t: &Theme, i: u8) -> u32 {
    match i {
        0..=15 => t.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let v = |x: u8| if x == 0 { 0 } else { 55 + 40 * x as u32 };
            (v(i / 36) << 16) | (v((i / 6) % 6) << 8) | v(i % 6)
        }
        _ => { let g = 8 + 10 * (i as u32 - 232); (g << 16) | (g << 8) | g }
    }
}

pub fn term_color(t: &Theme, col: Color, is_fg: bool) -> Hsla {
    match col {
        Color::Spec(x) => c(((x.r as u32) << 16) | ((x.g as u32) << 8) | x.b as u32),
        Color::Indexed(i) => c(xterm256(t, i)),
        Color::Named(n) => {
            let i = n as usize;
            match i {
                0..=15 => c(t.ansi[i]),
                259..=266 => c(t.ansi[i - 259]).opacity(0.7),
                _ => match n {
                    NamedColor::Background => c(t.bg),
                    NamedColor::Cursor => c(t.cursor),
                    NamedColor::DimForeground => c(t.fg).opacity(0.7),
                    _ => if is_fg { c(t.fg) } else { c(t.bg) },
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtins_parse() { for s in BUILTIN { Theme::from_toml(s).unwrap(); } }
    #[test]
    fn alacritty_import() {
        let src = r##"
[colors.primary]
background = "#282c34"
foreground = "#abb2bf"
[colors.normal]
black="#1e2127"
red="#e06c75"
green="#98c379"
yellow="#d19a66"
blue="#61afef"
magenta="#c678dd"
cyan="#56b6c2"
white="#abb2bf"
[colors.bright]
black="#5c6370"
red="#e06c75"
green="#98c379"
yellow="#d19a66"
blue="#61afef"
magenta="#c678dd"
cyan="#56b6c2"
white="#ffffff"
"##;
        let t = Theme::from_alacritty_toml("x", src).unwrap();
        assert_eq!(t.bg, 0x282c34);
        assert_eq!(t.ansi[9], 0xe06c75);
        assert_eq!(t.appearance, Appearance::Dark);
    }
}
