//! Theme system: UI color tokens + terminal palette, loaded from TOML.
//! Built-in themes are embedded from `themes/*.toml`; user themes from `~/.config/coossh/themes/*.toml`.
use std::sync::{Arc, RwLock};

use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui::{rgb, Hsla};
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
        Ok(Self::derive(name, fg, bg, ansi, get("cursor", "cursor").ok(), get("cursor", "text").ok(), get("selection", "background").ok()))
    }

    /// Import an iTerm2 `.itermcolors` file (XML plist of `<key>Ansi 0 Color</key><dict>…Red Component…</dict>`).
    pub fn from_itermcolors(name: &str, src: &str) -> anyhow::Result<Theme> {
        let colors = parse_itermcolors(src)?;
        let get = |k: &str| colors.iter().find(|(n, _)| n == k).map(|(_, c)| *c);
        let mut ansi = [0u32; 16];
        for (i, a) in ansi.iter_mut().enumerate() { *a = get(&format!("Ansi {i} Color")).ok_or_else(|| anyhow::anyhow!("missing Ansi {i} Color"))?; }
        let fg = get("Foreground Color").ok_or_else(|| anyhow::anyhow!("missing Foreground Color"))?;
        let bg = get("Background Color").ok_or_else(|| anyhow::anyhow!("missing Background Color"))?;
        Ok(Self::derive(name, fg, bg, ansi, get("Cursor Color"), get("Cursor Text Color"), get("Selection Color")))
    }

    /// UI tokens derived from a terminal palette (used by the Alacritty / iTerm2 importers).
    fn derive(name: &str, fg: u32, bg: u32, ansi: [u32; 16], cursor: Option<u32>, cursor_text: Option<u32>, selection: Option<u32>) -> Theme {
        let light = luma(bg) > 0.5;
        let mix = |a: u32, b: u32, t: f32| -> u32 {
            let ch = |s: u32| -> [f32; 3] { [((s >> 16) & 255) as f32, ((s >> 8) & 255) as f32, (s & 255) as f32] };
            let (x, y) = (ch(a), ch(b));
            let m: Vec<u32> = (0..3).map(|i| (x[i] + (y[i] - x[i]) * t).round() as u32).collect();
            (m[0] << 16) | (m[1] << 8) | m[2]
        };
        Theme {
            name: name.into(), appearance: if light { Appearance::Light } else { Appearance::Dark },
            crust: mix(bg, if light { 0xffffff } else { 0 }, 0.35), mantle: mix(bg, if light { 0xffffff } else { 0 }, 0.18), base: bg,
            surface0: mix(bg, fg, 0.12), surface1: mix(bg, fg, 0.2), surface2: mix(bg, fg, 0.3),
            overlay0: mix(bg, fg, 0.45), overlay1: mix(bg, fg, 0.55), text: fg, subtext: mix(bg, fg, 0.75),
            accent: ansi[4], success: ansi[2], error: ansi[1], warning: ansi[3], info: ansi[6],
            fg, bg, cursor: cursor.unwrap_or(fg), cursor_text: cursor_text.unwrap_or(bg),
            selection: selection.unwrap_or(mix(bg, fg, 0.3)), suggestion: mix(bg, fg, 0.45),
            ansi,
        }
    }
}

/// Minimal plist reader for .itermcolors: top-level `<key>Name</key><dict>` blocks with
/// `Red/Green/Blue Component` reals (0..1). Returns (name, 0xRRGGBB).
fn parse_itermcolors(src: &str) -> anyhow::Result<Vec<(String, u32)>> {
    fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<(&'a str, &'a str)> {
        let a = s.find(open)? + open.len();
        let b = s[a..].find(close)? + a;
        Some((&s[a..b], &s[b + close.len()..]))
    }
    let start = src.find("<dict>").ok_or_else(|| anyhow::anyhow!("not a plist"))? + 6;
    let mut rest = &src[start..];
    let mut out = Vec::new();
    while let Some((key, after)) = between(rest, "<key>", "</key>") {
        let Some((body, after2)) = between(after, "<dict>", "</dict>") else { break };
        let mut comp = [None::<f32>; 3];
        let mut b = body;
        while let Some((k, a)) = between(b, "<key>", "</key>") {
            // the value is everything up to the next <key> (a <real>, <integer>, <string>, ...)
            let end = a.find("<key>").unwrap_or(a.len());
            let (val, a2) = (&a[..end], &a[end..]);
            let v = between(val, "<real>", "</real>").or_else(|| between(val, "<integer>", "</integer>")).map(|x| x.0).unwrap_or("");
            let i = match k.trim() { "Red Component" => Some(0), "Green Component" => Some(1), "Blue Component" => Some(2), _ => None };
            if let (Some(i), Ok(x)) = (i, v.trim().parse::<f32>()) { comp[i] = Some(x); }
            b = a2;
        }
        if let [Some(r), Some(g), Some(bl)] = comp {
            let ch = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
            out.push((key.trim().to_string(), (ch(r) << 16) | (ch(g) << 8) | ch(bl)));
        }
        rest = after2;
    }
    anyhow::ensure!(!out.is_empty(), "no colors found");
    Ok(out)
}

/// `~/.config/coossh/themes`
pub fn user_dir() -> Option<std::path::PathBuf> {
    Some(ssh_term_proto::platform::config_dir().join("themes"))
}

/// Parse any supported theme file (coossh TOML, Alacritty TOML, iTerm2 .itermcolors).
pub fn parse_file(p: &std::path::Path) -> anyhow::Result<Theme> {
    let src = std::fs::read_to_string(p)?;
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("imported");
    match p.extension().and_then(|x| x.to_str()) {
        Some("itermcolors") => Theme::from_itermcolors(stem, &src),
        _ => Theme::from_toml(&src).or_else(|_| Theme::from_alacritty_toml(stem, &src)),
    }
}

/// Validate and copy a theme file into the user theme dir. Returns (dest, theme name).
pub fn import_file(src: &std::path::Path, dir: &std::path::Path) -> anyhow::Result<(std::path::PathBuf, String, bool)> {
    let th = parse_file(src)?;
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(src.file_name().ok_or_else(|| anyhow::anyhow!("no file name"))?);
    std::fs::copy(src, &dest)?;
    Ok((dest, th.name, th.appearance == Appearance::Dark))
}

fn luma(c: u32) -> f32 {
    (0.2126 * ((c >> 16) & 255) as f32 + 0.7152 * ((c >> 8) & 255) as f32 + 0.0722 * (c & 255) as f32) / 255.0
}

pub const BUILTIN: &[&str] = &[
    include_str!("../themes/light.toml"),
    include_str!("../themes/dark.toml"),
    include_str!("../themes/catppuccin-mocha.toml"),
    include_str!("../themes/catppuccin-latte.toml"),
    include_str!("../themes/coossh-midnight.toml"),
    include_str!("../themes/coossh-daylight.toml"),
];

pub fn load_all() -> Vec<Arc<Theme>> {
    let mut v: Vec<Arc<Theme>> = BUILTIN.iter().map(|s| Arc::new(Theme::from_toml(s).expect("builtin theme"))).collect();
    if let Some(dir) = user_dir() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if matches!(p.extension().and_then(|x| x.to_str()), Some("toml" | "itermcolors")) {
                    match parse_file(&p) { Ok(t) => v.push(Arc::new(t)), Err(e) => eprintln!("theme {}: {e:#}", p.display()) }
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

pub fn c(hex: u32) -> Hsla { rgb(hex).into() }

/// macOS-style themes get the system accent color (PRD 5.22).
/// User-visible theme name: the built-in "Light" / "Dark" are localized.
pub fn display_name(name: &str) -> String {
    match name { "Light" => ssh_term_proto::i18n::t("theme.light").to_string(), "Dark" => ssh_term_proto::i18n::t("theme.dark").to_string(), n => n.to_string() }
}
pub fn is_macos_theme(t: &Theme) -> bool { t.name == "Light" || t.name == "Dark" }

/// System accent color. macOS: `defaults read -g AppleAccentColor` (-1 graphite … 6 pink; missing = blue);
/// GNOME 47+: `gsettings get org.gnome.desktop.interface accent-color`. None = keep the theme's blue.
pub fn accent_from(macos: Option<&str>, gnome: Option<&str>, dark: bool) -> Option<u32> {
    let pick = |l: u32, d: u32| Some(if dark { d } else { l });
    if let Some(v) = macos.map(str::trim).filter(|v| !v.is_empty()) {
        return match v.parse::<i32>().ok()? {
            -1 => pick(0x8c8c8c, 0x98989d), 0 => pick(0xff3b30, 0xff453a), 1 => pick(0xff9500, 0xff9f0a), 2 => pick(0xffcc00, 0xffd60a),
            3 => pick(0x28cd41, 0x32d74b), 4 => pick(0x007aff, 0x0a84ff), 5 => pick(0xaf52de, 0xbf5af2), 6 => pick(0xff2d55, 0xff375f), _ => None,
        };
    }
    let g = gnome?.trim().trim_matches('\'').to_ascii_lowercase();
    match g.as_str() {
        "blue" => pick(0x007aff, 0x0a84ff), "teal" => pick(0x30b0c7, 0x40c8e0), "green" => pick(0x28cd41, 0x32d74b), "yellow" => pick(0xffcc00, 0xffd60a),
        "orange" => pick(0xff9500, 0xff9f0a), "red" => pick(0xff3b30, 0xff453a), "pink" => pick(0xff2d55, 0xff375f), "purple" => pick(0xaf52de, 0xbf5af2),
        "slate" => pick(0x8c8c8c, 0x98989d), _ => None,
    }
}
fn read_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(cmd).args(args).stderr(std::process::Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}
/// Query the OS once (cached); `None` when the OS has no accent setting.
pub fn system_accent(dark: bool) -> Option<u32> {
    static RAW: std::sync::OnceLock<(Option<String>, Option<String>)> = std::sync::OnceLock::new();
    let (m, g) = RAW.get_or_init(|| {
        if std::env::var_os("COOSSH_NO_SYSTEM_ACCENT").is_some() { return (None, None); }
        if cfg!(target_os = "macos") { (Some(read_cmd("defaults", &["read", "-g", "AppleAccentColor"]).unwrap_or_else(|| "4".into())), None) }
        else if cfg!(target_os = "linux") { (None, read_cmd("gsettings", &["get", "org.gnome.desktop.interface", "accent-color"])) }
        else { (None, None) }
    });
    accent_from(m.as_deref(), g.as_deref(), dark)
}

/// UI font (PRD 5.22): system font, terminal font is separate. First installed family wins.
pub fn pick_ui_font(installed: &[String]) -> String {
    const CHAIN: &[&str] = &[".SystemUIFont", "SF Pro Text", "SF Pro", "PingFang SC", "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI",
        "Inter", "Cantarell", "Noto Sans", "Ubuntu", "Noto Sans CJK SC", "DejaVu Sans", "Tahoma", "Arial"];
    if cfg!(target_os = "macos") { return ".SystemUIFont".into(); }
    CHAIN.iter().find(|f| installed.iter().any(|i| i.eq_ignore_ascii_case(f))).map(|s| s.to_string()).unwrap_or_else(|| "Inter".into())
}
/// Terminal font actually used: the configured family if installed, else the first installed
/// configured fallback, else a platform monospace (Win10 has Consolas but not Cascadia Mono).
pub fn pick_term_font(family: &str, fallbacks: &[String], installed: &[String]) -> Option<String> {
    const MONO: &[&str] = &["Cascadia Mono", "Consolas", "Menlo", "SF Mono", "JetBrains Mono", "DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono", "Courier New"];
    let has = |f: &str| installed.iter().any(|i| i.eq_ignore_ascii_case(f));
    if installed.is_empty() || has(family) { return None; }
    fallbacks.iter().map(|s| s.as_str()).filter(|f| { let l = f.to_ascii_lowercase(); l.contains("mono") && !l.contains("symbols") })
        .chain(MONO.iter().copied()).find(|f| has(f)).map(str::to_string)
}
static UI_FONT: RwLock<Option<gpui::SharedString>> = RwLock::new(None);
pub fn set_ui_font(f: String) { *UI_FONT.write().unwrap() = Some(f.into()); }
pub fn ui_font() -> gpui::SharedString { UI_FONT.read().unwrap().clone().unwrap_or_else(|| "Inter".into()) }

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
    fn macos_themes() {
        let l = Theme::from_toml(BUILTIN[0]).unwrap();
        let d = Theme::from_toml(BUILTIN[1]).unwrap();
        assert_eq!((l.name.as_str(), l.appearance, l.accent), ("Light", Appearance::Light, 0x007aff));
        assert_eq!((d.name.as_str(), d.appearance, d.accent), ("Dark", Appearance::Dark, 0x0a84ff));
        assert!(is_macos_theme(&l) && is_macos_theme(&d));
        // Terminal.app-like basics: black on white / light on dark, red ≈ 0x990000
        assert_eq!((l.fg, l.bg, l.ansi[1]), (0x000000, 0xffffff, 0x990000));
        assert!(luma(d.bg) < 0.2 && luma(d.fg) > 0.8);
    }
    #[test]
    fn accent_mapping() {
        assert_eq!(accent_from(None, None, false), None);
        assert_eq!(accent_from(Some("4"), None, false), Some(0x007aff));
        assert_eq!(accent_from(Some("4"), None, true), Some(0x0a84ff));
        assert_eq!(accent_from(Some("5\n"), None, false), Some(0xaf52de));
        assert_eq!(accent_from(Some("-1"), None, false), Some(0x8c8c8c));
        assert_eq!(accent_from(Some("x"), None, false), None);
        assert_eq!(accent_from(None, Some("'green'\n"), false), Some(0x28cd41));
        assert_eq!(accent_from(None, Some("'nonsense'"), false), None);
    }
    #[test]
    fn ui_font_chain() {
        if cfg!(target_os = "macos") { return; }
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_ui_font(&v(&["DejaVu Sans", "Noto Sans", "Inter"])), "Inter");
        assert_eq!(pick_ui_font(&v(&["DejaVu Sans", "pingfang sc"])), "PingFang SC");
        assert_eq!(pick_ui_font(&v(&["Segoe UI", "Inter"])), "Segoe UI");
        assert_eq!(pick_ui_font(&v(&[])), "Inter");
    }
    #[test]
    fn term_font_fallback() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let fb = v(&["Microsoft YaHei UI", "Symbols Nerd Font Mono"]);
        assert_eq!(pick_term_font("Cascadia Mono", &fb, &v(&["Cascadia Mono", "Consolas"])), None);
        assert_eq!(pick_term_font("Cascadia Mono", &fb, &v(&["Segoe UI", "Consolas"])).as_deref(), Some("Consolas"));
        assert_eq!(pick_term_font("Cascadia Mono", &fb, &v(&["Symbols Nerd Font Mono", "Consolas"])).as_deref(), Some("Consolas"));
        assert_eq!(pick_term_font("X", &[], &v(&[])), None);
    }
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

#[cfg(test)]
mod iterm_tests {
    use super::*;
    fn entry(name: &str, r: f32, g: f32, b: f32) -> String {
        format!("<key>{name}</key>\n<dict>\n<key>Alpha Component</key><real>1</real>\n<key>Blue Component</key><real>{b}</real>\n<key>Color Space</key><string>sRGB</string>\n<key>Green Component</key><real>{g}</real>\n<key>Red Component</key><real>{r}</real>\n</dict>\n")
    }
    #[test]
    fn imports_itermcolors() {
        let mut body = String::new();
        for i in 0..16 { body += &entry(&format!("Ansi {i} Color"), i as f32 / 15.0, 0.0, 1.0); }
        body += &entry("Background Color", 0.0, 0.0, 0.0);
        body += &entry("Foreground Color", 1.0, 1.0, 1.0);
        body += &entry("Cursor Color", 1.0, 0.0, 0.0);
        let src = format!("<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n");
        let t = Theme::from_itermcolors("x", &src).unwrap();
        assert_eq!(t.bg, 0x000000); assert_eq!(t.fg, 0xffffff); assert_eq!(t.cursor, 0xff0000);
        assert_eq!(t.ansi[0], 0x0000ff); assert_eq!(t.ansi[15], 0xff00ff);
        assert_eq!(t.appearance, Appearance::Dark);
        assert!(Theme::from_itermcolors("x", "<plist><dict></dict></plist>").is_err());
    }
}
