//! Theme system: UI color tokens + terminal palette, loaded from TOML.
//! Built-in themes are embedded from `themes/*.toml`; user themes from `~/.config/shellax/themes/*.toml`.
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui::{rgb, Hsla};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Dark,
    Light,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UiColors {
    pub crust: String,
    pub mantle: String,
    pub base: String,
    pub surface0: String,
    pub surface1: String,
    pub surface2: String,
    pub overlay0: String,
    pub overlay1: String,
    pub text: String,
    pub subtext: String,
    pub accent: String,
    pub success: String,
    pub error: String,
    pub warning: String,
    pub info: String,
}
#[derive(Clone, Debug, Deserialize)]
pub struct TermColors {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub cursor_text: String,
    pub selection: String,
    pub suggestion: String,
    pub ansi: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ThemeFile {
    pub name: String,
    pub appearance: Appearance,
    pub ui: UiColors,
    pub terminal: TermColors,
}

/// Resolved theme (parsed colors).
#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub appearance: Appearance,
    pub crust: u32,
    pub mantle: u32,
    pub base: u32,
    pub surface0: u32,
    pub surface1: u32,
    pub surface2: u32,
    pub overlay0: u32,
    pub overlay1: u32,
    pub text: u32,
    pub subtext: u32,
    pub accent: u32,
    pub success: u32,
    pub error: u32,
    pub warning: u32,
    pub info: u32,
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
    pub cursor_text: u32,
    pub selection: u32,
    pub suggestion: u32,
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
        for (i, s) in f.terminal.ansi.iter().enumerate() {
            ansi[i] = parse_hex(s)?;
        }
        let u = &f.ui;
        let t = &f.terminal;
        Ok(Theme {
            name: f.name,
            appearance: f.appearance,
            crust: parse_hex(&u.crust)?,
            mantle: parse_hex(&u.mantle)?,
            base: parse_hex(&u.base)?,
            surface0: parse_hex(&u.surface0)?,
            surface1: parse_hex(&u.surface1)?,
            surface2: parse_hex(&u.surface2)?,
            overlay0: parse_hex(&u.overlay0)?,
            overlay1: parse_hex(&u.overlay1)?,
            text: parse_hex(&u.text)?,
            subtext: parse_hex(&u.subtext)?,
            accent: parse_hex(&u.accent)?,
            success: parse_hex(&u.success)?,
            error: parse_hex(&u.error)?,
            warning: parse_hex(&u.warning)?,
            info: parse_hex(&u.info)?,
            fg: parse_hex(&t.foreground)?,
            bg: parse_hex(&t.background)?,
            cursor: parse_hex(&t.cursor)?,
            cursor_text: parse_hex(&t.cursor_text)?,
            selection: parse_hex(&t.selection)?,
            suggestion: parse_hex(&t.suggestion)?,
            ansi,
        })
    }

    /// Terminal palette carried by this UI theme, under the same name.
    pub fn to_scheme(&self) -> Scheme {
        Scheme {
            name: self.name.clone(),
            appearance: self.appearance,
            fg: self.fg,
            bg: self.bg,
            cursor: self.cursor,
            cursor_text: self.cursor_text,
            selection: self.selection,
            selection_fg: None,
            suggestion: self.suggestion,
            ansi: self.ansi,
        }
    }
}

/// Terminal content palette. Independent of the UI theme: foreground, background, cursor,
/// selection and the 16 ANSI colors. Indexed 16–255 and truecolor are not themed.
#[derive(Clone, Debug)]
pub struct Scheme {
    pub name: String,
    pub appearance: Appearance,
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
    pub cursor_text: u32,
    pub selection: u32,
    pub selection_fg: Option<u32>,
    pub suggestion: u32,
    pub ansi: [u32; 16],
}

#[derive(Deserialize)]
struct SchemeFile {
    name: String,
    #[serde(default)]
    appearance: Option<Appearance>,
    foreground: String,
    background: String,
    cursor: String,
    cursor_text: String,
    selection: String,
    #[serde(default)]
    selection_foreground: Option<String>,
    #[serde(default)]
    suggestion: Option<String>,
    ansi: Vec<String>,
}

impl Scheme {
    pub fn from_toml(src: &str) -> anyhow::Result<Scheme> {
        let f: SchemeFile = toml::from_str(src)?;
        anyhow::ensure!(f.ansi.len() == 16, "ansi needs 16 colors");
        let mut ansi = [0u32; 16];
        for (i, s) in f.ansi.iter().enumerate() {
            ansi[i] = parse_hex(s)?;
        }
        let fg = parse_hex(&f.foreground)?;
        let bg = parse_hex(&f.background)?;
        Ok(Self::finish(
            f.name,
            fg,
            bg,
            Some(parse_hex(&f.cursor)?),
            Some(parse_hex(&f.cursor_text)?),
            Some(parse_hex(&f.selection)?),
            f.selection_foreground
                .as_deref()
                .map(parse_hex)
                .transpose()?,
            f.suggestion.as_deref().map(parse_hex).transpose()?,
            ansi,
            f.appearance,
        ))
    }

    /// Alacritty `[colors]` only. Does not build UI colors.
    pub fn from_alacritty(name: &str, src: &str) -> anyhow::Result<Scheme> {
        let v: toml::Value = toml::from_str(src)?;
        let colors = v
            .get("colors")
            .ok_or_else(|| anyhow::anyhow!("no [colors]"))?;
        let get = |sec: &str, key: &str| -> anyhow::Result<u32> {
            let s = colors
                .get(sec)
                .and_then(|t| t.get(key))
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing colors.{sec}.{key}"))?;
            parse_hex(s)
        };
        let opt = |sec: &str, key: &str| get(sec, key).ok();
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let mut ansi = [0u32; 16];
        for (i, n) in names.iter().enumerate() {
            ansi[i] = get("normal", n)?;
            ansi[i + 8] = get("bright", n)?;
        }
        let label = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or(name)
            .to_string();
        Ok(Self::finish(
            label,
            get("primary", "foreground")?,
            get("primary", "background")?,
            opt("cursor", "cursor"),
            opt("cursor", "text"),
            opt("selection", "background"),
            opt("selection", "text"),
            None,
            ansi,
            None,
        ))
    }

    /// iTerm2 `.itermcolors`. Does not build UI colors.
    pub fn from_itermcolors(name: &str, src: &str) -> anyhow::Result<Scheme> {
        let colors = parse_itermcolors(src)?;
        let get = |k: &str| colors.iter().find(|(n, _)| n == k).map(|(_, c)| *c);
        let mut ansi = [0u32; 16];
        for (i, a) in ansi.iter_mut().enumerate() {
            *a = get(&format!("Ansi {i} Color"))
                .ok_or_else(|| anyhow::anyhow!("missing Ansi {i} Color"))?;
        }
        let fg =
            get("Foreground Color").ok_or_else(|| anyhow::anyhow!("missing Foreground Color"))?;
        let bg =
            get("Background Color").ok_or_else(|| anyhow::anyhow!("missing Background Color"))?;
        Ok(Self::finish(
            name.into(),
            fg,
            bg,
            get("Cursor Color"),
            get("Cursor Text Color"),
            get("Selection Color"),
            get("Selected Text Color"),
            None,
            ansi,
            None,
        ))
    }

    fn finish(
        name: String,
        fg: u32,
        bg: u32,
        cursor: Option<u32>,
        cursor_text: Option<u32>,
        selection: Option<u32>,
        selection_fg: Option<u32>,
        suggestion: Option<u32>,
        ansi: [u32; 16],
        appearance: Option<Appearance>,
    ) -> Scheme {
        let appearance = appearance.unwrap_or(if luma(bg) > 0.5 {
            Appearance::Light
        } else {
            Appearance::Dark
        });
        Scheme {
            name,
            appearance,
            fg,
            bg,
            cursor: cursor.unwrap_or(fg),
            cursor_text: cursor_text.unwrap_or(bg),
            selection: selection.unwrap_or_else(|| mix(bg, fg, 0.3)),
            selection_fg,
            suggestion: suggestion.unwrap_or_else(|| mix(bg, fg, 0.45)),
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
    let start = src
        .find("<dict>")
        .ok_or_else(|| anyhow::anyhow!("not a plist"))?
        + 6;
    let mut rest = &src[start..];
    let mut out = Vec::new();
    while let Some((key, after)) = between(rest, "<key>", "</key>") {
        let Some((body, after2)) = between(after, "<dict>", "</dict>") else {
            break;
        };
        let mut comp = [None::<f32>; 3];
        let mut b = body;
        while let Some((k, a)) = between(b, "<key>", "</key>") {
            // the value is everything up to the next <key> (a <real>, <integer>, <string>, ...)
            let end = a.find("<key>").unwrap_or(a.len());
            let (val, a2) = (&a[..end], &a[end..]);
            let v = between(val, "<real>", "</real>")
                .or_else(|| between(val, "<integer>", "</integer>"))
                .map(|x| x.0)
                .unwrap_or("");
            let i = match k.trim() {
                "Red Component" => Some(0),
                "Green Component" => Some(1),
                "Blue Component" => Some(2),
                _ => None,
            };
            if let (Some(i), Ok(x)) = (i, v.trim().parse::<f32>()) {
                comp[i] = Some(x);
            }
            b = a2;
        }
        if let [Some(r), Some(g), Some(bl)] = comp {
            let ch = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
            out.push((
                key.trim().to_string(),
                (ch(r) << 16) | (ch(g) << 8) | ch(bl),
            ));
        }
        rest = after2;
    }
    anyhow::ensure!(!out.is_empty(), "no colors found");
    Ok(out)
}

/// `~/.config/shellax/themes` — full ShellAX UI themes only.
pub fn user_dir() -> Option<std::path::PathBuf> {
    Some(ssh_term_proto::platform::config_dir().join("themes"))
}

/// `~/.config/shellax/schemes` — terminal color schemes (no UI colors).
pub fn scheme_dir() -> Option<std::path::PathBuf> {
    Some(ssh_term_proto::platform::config_dir().join("schemes"))
}

/// What `import_file` stored.
pub enum Imported {
    Theme {
        dest: std::path::PathBuf,
        name: String,
        dark: bool,
    },
    Scheme {
        dest: std::path::PathBuf,
        name: String,
        dark: bool,
    },
}

/// Copy a file into the theme dir or the scheme dir.
/// A ShellAX theme (has `[ui]`) stays a UI theme. iTerm2, Alacritty and scheme TOML become a color scheme only.
pub fn import_file(src: &std::path::Path) -> anyhow::Result<Imported> {
    let text = std::fs::read_to_string(src)?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported");
    let file_name = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("no file name"))?;
    let ext = src.extension().and_then(|x| x.to_str());
    if ext == Some("itermcolors") {
        let scheme = Scheme::from_itermcolors(stem, &text)?;
        return Ok(store_scheme(src, file_name, scheme)?);
    }
    if let Ok(th) = Theme::from_toml(&text) {
        let dir = user_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
        std::fs::create_dir_all(&dir)?;
        let dest = dir.join(file_name);
        std::fs::copy(src, &dest)?;
        return Ok(Imported::Theme {
            name: th.name,
            dark: th.appearance == Appearance::Dark,
            dest,
        });
    }
    let scheme = Scheme::from_toml(&text).or_else(|_| Scheme::from_alacritty(stem, &text))?;
    Ok(store_scheme(src, file_name, scheme)?)
}

fn store_scheme(
    src: &std::path::Path,
    file_name: &std::ffi::OsStr,
    scheme: Scheme,
) -> anyhow::Result<Imported> {
    let dir = scheme_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(file_name);
    std::fs::copy(src, &dest)?;
    Ok(Imported::Scheme {
        name: scheme.name,
        dark: scheme.appearance == Appearance::Dark,
        dest,
    })
}

fn luma(c: u32) -> f32 {
    (0.2126 * ((c >> 16) & 255) as f32
        + 0.7152 * ((c >> 8) & 255) as f32
        + 0.0722 * (c & 255) as f32)
        / 255.0
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| -> [f32; 3] {
        [
            ((s >> 16) & 255) as f32,
            ((s >> 8) & 255) as f32,
            (s & 255) as f32,
        ]
    };
    let (x, y) = (ch(a), ch(b));
    let m: Vec<u32> = (0..3)
        .map(|i| (x[i] + (y[i] - x[i]) * t).round().clamp(0.0, 255.0) as u32)
        .collect();
    (m[0] << 16) | (m[1] << 8) | m[2]
}

pub const BUILTIN: &[&str] = &[
    include_str!("../themes/light.toml"),
    include_str!("../themes/dark.toml"),
    include_str!("../themes/linear-carbon.toml"),
    include_str!("../themes/linear-paper.toml"),
    include_str!("../themes/tokyo-night.toml"),
    include_str!("../themes/shellax-midnight.toml"),
    include_str!("../themes/shellax-daylight.toml"),
    include_str!("../themes/catppuccin-mocha.toml"),
    include_str!("../themes/catppuccin-latte.toml"),
];

pub const BUILTIN_SCHEMES: &[&str] = &[
    include_str!("../schemes/campbell.toml"),
    include_str!("../schemes/one-half-dark.toml"),
    include_str!("../schemes/one-half-light.toml"),
    include_str!("../schemes/dracula.toml"),
    include_str!("../schemes/nord.toml"),
    include_str!("../schemes/gruvbox-dark.toml"),
    include_str!("../schemes/gruvbox-light.toml"),
    include_str!("../schemes/tokyo-night.toml"),
    include_str!("../schemes/solarized-dark.toml"),
    include_str!("../schemes/solarized-light.toml"),
];

pub fn load_all() -> Vec<Arc<Theme>> {
    let mut v: Vec<Arc<Theme>> = BUILTIN
        .iter()
        .map(|s| Arc::new(Theme::from_toml(s).expect("builtin theme")))
        .collect();
    if let Some(dir) = user_dir() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                    continue;
                }
                let src = match std::fs::read_to_string(&p) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("theme {}: {e:#}", p.display());
                        continue;
                    }
                };
                match Theme::from_toml(&src) {
                    Ok(t) => v.push(Arc::new(t)),
                    // Alacritty and scheme files left in the theme dir are loaded as color schemes, not UI themes.
                    Err(e) => {
                        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("imported");
                        if Scheme::from_toml(&src).is_err()
                            && Scheme::from_alacritty(stem, &src).is_err()
                        {
                            eprintln!("theme {}: {e:#}", p.display());
                        }
                    }
                }
            }
        }
    }
    v
}

fn upsert_scheme(v: &mut Vec<Arc<Scheme>>, s: Scheme) {
    if let Some(i) = v.iter().position(|e| e.name.eq_ignore_ascii_case(&s.name)) {
        v[i] = Arc::new(s);
    } else {
        v.push(Arc::new(s));
    }
}

fn parse_scheme_src(stem: &str, ext: Option<&str>, src: &str) -> anyhow::Result<Scheme> {
    if ext == Some("itermcolors") {
        return Scheme::from_itermcolors(stem, src);
    }
    Scheme::from_toml(src).or_else(|_| Scheme::from_alacritty(stem, src))
}

/// Theme palettes first (so each UI theme can be selected as a scheme), then the built-in
/// catalog, then user files. A later file with the same name replaces the earlier one.
pub fn load_schemes(themes: &[Arc<Theme>]) -> Vec<Arc<Scheme>> {
    let mut v: Vec<Arc<Scheme>> = themes.iter().map(|t| Arc::new(t.to_scheme())).collect();
    for src in BUILTIN_SCHEMES {
        match Scheme::from_toml(src) {
            Ok(s) => upsert_scheme(&mut v, s),
            Err(e) => eprintln!("scheme: {e:#}"),
        }
    }
    let scan = |dir: std::path::PathBuf, only_non_themes: bool, v: &mut Vec<Arc<Scheme>>| {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str());
            if !matches!(ext, Some("toml" | "itermcolors")) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&p) else {
                continue;
            };
            if only_non_themes && ext == Some("toml") && Theme::from_toml(&src).is_ok() {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("imported");
            match parse_scheme_src(stem, ext, &src) {
                Ok(s) => upsert_scheme(v, s),
                Err(e) => eprintln!("scheme {}: {e:#}", p.display()),
            }
        }
    };
    if let Some(dir) = user_dir() {
        scan(dir, true, &mut v);
    }
    if let Some(dir) = scheme_dir() {
        scan(dir, false, &mut v);
    }
    v
}

static CURRENT: RwLock<Option<Arc<Theme>>> = RwLock::new(None);
pub fn set_current(t: Arc<Theme>) {
    *CURRENT.write().unwrap() = Some(t);
}
pub fn cur() -> Arc<Theme> {
    CURRENT
        .read()
        .unwrap()
        .clone()
        .unwrap_or_else(|| Arc::new(Theme::from_toml(BUILTIN[0]).unwrap()))
}

static SCHEME: RwLock<Option<Arc<Scheme>>> = RwLock::new(None);
pub fn set_scheme(s: Arc<Scheme>) {
    *SCHEME.write().unwrap() = Some(s);
}
pub fn scheme() -> Arc<Scheme> {
    SCHEME
        .read()
        .unwrap()
        .clone()
        .unwrap_or_else(|| Arc::new(cur().to_scheme()))
}

pub fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// macOS-style themes get the system accent color (PRD 5.22).
/// User-visible theme name: the built-in "Light" / "Dark" are localized.
pub fn display_name(name: &str) -> String {
    match name {
        "Light" => ssh_term_proto::i18n::t("theme.light").to_string(),
        "Dark" => ssh_term_proto::i18n::t("theme.dark").to_string(),
        n => n.to_string(),
    }
}
pub fn is_macos_theme(t: &Theme) -> bool {
    t.name == "Light" || t.name == "Dark"
}

/// System accent color. macOS: `defaults read -g AppleAccentColor` (-1 graphite … 6 pink; missing = blue);
/// GNOME 47+: `gsettings get org.gnome.desktop.interface accent-color`. None = keep the theme's blue.
pub fn accent_from(macos: Option<&str>, gnome: Option<&str>, dark: bool) -> Option<u32> {
    let pick = |l: u32, d: u32| Some(if dark { d } else { l });
    if let Some(v) = macos.map(str::trim).filter(|v| !v.is_empty()) {
        return match v.parse::<i32>().ok()? {
            -1 => pick(0x8c8c8c, 0x98989d),
            0 => pick(0xff3b30, 0xff453a),
            1 => pick(0xff9500, 0xff9f0a),
            2 => pick(0xffcc00, 0xffd60a),
            3 => pick(0x28cd41, 0x32d74b),
            4 => pick(0x007aff, 0x0a84ff),
            5 => pick(0xaf52de, 0xbf5af2),
            6 => pick(0xff2d55, 0xff375f),
            _ => None,
        };
    }
    let g = gnome?.trim().trim_matches('\'').to_ascii_lowercase();
    match g.as_str() {
        "blue" => pick(0x007aff, 0x0a84ff),
        "teal" => pick(0x30b0c7, 0x40c8e0),
        "green" => pick(0x28cd41, 0x32d74b),
        "yellow" => pick(0xffcc00, 0xffd60a),
        "orange" => pick(0xff9500, 0xff9f0a),
        "red" => pick(0xff3b30, 0xff453a),
        "pink" => pick(0xff2d55, 0xff375f),
        "purple" => pick(0xaf52de, 0xbf5af2),
        "slate" => pick(0x8c8c8c, 0x98989d),
        _ => None,
    }
}
fn read_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new(cmd)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}
/// Query the OS once (cached); `None` when the OS has no accent setting.
pub fn system_accent(dark: bool) -> Option<u32> {
    static RAW: std::sync::OnceLock<(Option<String>, Option<String>)> = std::sync::OnceLock::new();
    let (m, g) = RAW.get_or_init(|| {
        if std::env::var_os("SHELLAX_NO_SYSTEM_ACCENT").is_some() {
            return (None, None);
        }
        if cfg!(target_os = "macos") {
            (
                Some(
                    read_cmd("defaults", &["read", "-g", "AppleAccentColor"])
                        .unwrap_or_else(|| "4".into()),
                ),
                None,
            )
        } else if cfg!(target_os = "linux") {
            (
                None,
                read_cmd(
                    "gsettings",
                    &["get", "org.gnome.desktop.interface", "accent-color"],
                ),
            )
        } else {
            (None, None)
        }
    });
    accent_from(m.as_deref(), g.as_deref(), dark)
}

/// UI font (PRD 5.22): system font, terminal font is separate. First installed family wins.
pub fn pick_ui_font(installed: &[String]) -> String {
    const CHAIN: &[&str] = &[
        ".SystemUIFont",
        "SF Pro Text",
        "SF Pro",
        "PingFang SC",
        "Segoe UI Variable Text",
        "Segoe UI",
        "Microsoft YaHei UI",
        "Inter",
        "Cantarell",
        "Noto Sans",
        "Ubuntu",
        "Noto Sans CJK SC",
        "DejaVu Sans",
        "Tahoma",
        "Arial",
    ];
    if cfg!(target_os = "macos") {
        return ".SystemUIFont".into();
    }
    CHAIN
        .iter()
        .find(|f| installed.iter().any(|i| i.eq_ignore_ascii_case(f)))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Inter".into())
}
/// First installed family that is plausibly monospace: the configured fallbacks whose name says
/// "mono", then the platform monospace chain. Used both to replace a missing family and to replace
/// an installed but proportional one (the terminal grid requires one advance per cell).
pub fn pick_mono_family(fallbacks: &[String], installed: &[String]) -> Option<String> {
    const MONO: &[&str] = &[
        "Cascadia Mono",
        "Consolas",
        "Menlo",
        "SF Mono",
        "JetBrains Mono",
        "DejaVu Sans Mono",
        "Noto Sans Mono",
        "Liberation Mono",
        "Courier New",
    ];
    let has = |f: &str| installed.iter().any(|i| i.eq_ignore_ascii_case(f));
    fallbacks
        .iter()
        .map(|s| s.as_str())
        .filter(|f| {
            let l = f.to_ascii_lowercase();
            l.contains("mono") && !l.contains("symbols")
        })
        .chain(MONO.iter().copied())
        .find(|f| has(f))
        .map(str::to_string)
}
static UI_FONT: RwLock<Option<gpui::SharedString>> = RwLock::new(None);
pub fn set_ui_font(f: String) {
    *UI_FONT.write().unwrap() = Some(f.into());
}
pub fn ui_font() -> gpui::SharedString {
    UI_FONT
        .read()
        .unwrap()
        .clone()
        .unwrap_or_else(|| "Inter".into())
}

fn xterm256(t: &Scheme, i: u8) -> u32 {
    match i {
        0..=15 => t.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let v = |x: u8| if x == 0 { 0 } else { 55 + 40 * x as u32 };
            (v(i / 36) << 16) | (v((i / 6) % 6) << 8) | v(i % 6)
        }
        _ => {
            let g = 8 + 10 * (i as u32 - 232);
            (g << 16) | (g << 8) | g
        }
    }
}

pub fn term_color(t: &Scheme, col: Color, is_fg: bool) -> Hsla {
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
                    _ => {
                        if is_fg {
                            c(t.fg)
                        } else {
                            c(t.bg)
                        }
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtins_parse() {
        for s in BUILTIN {
            Theme::from_toml(s).unwrap();
        }
    }
    #[test]
    fn macos_themes() {
        let l = Theme::from_toml(BUILTIN[0]).unwrap();
        let d = Theme::from_toml(BUILTIN[1]).unwrap();
        assert_eq!(
            (l.name.as_str(), l.appearance, l.accent),
            ("Light", Appearance::Light, 0x007aff)
        );
        assert_eq!(
            (d.name.as_str(), d.appearance, d.accent),
            ("Dark", Appearance::Dark, 0x0a84ff)
        );
        assert!(is_macos_theme(&l) && is_macos_theme(&d));
        assert!(luma(l.bg) > 0.9 && luma(l.fg) < 0.2);
        assert!(luma(d.bg) < 0.2 && luma(d.fg) > 0.8);
        // Proper 3-step harmonious depth: outer frame (`crust`) < sidebar (`mantle`) < content (`base`)
        for s in BUILTIN {
            let t = Theme::from_toml(s).unwrap();
            assert!(
                luma(t.crust) < luma(t.mantle) && luma(t.mantle) < luma(t.base),
                "{}: expected crust < mantle < base for smooth surface harmony",
                t.name
            );
        }
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
        if cfg!(target_os = "macos") {
            return;
        }
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            pick_ui_font(&v(&["DejaVu Sans", "Noto Sans", "Inter"])),
            "Inter"
        );
        assert_eq!(
            pick_ui_font(&v(&["DejaVu Sans", "pingfang sc"])),
            "PingFang SC"
        );
        assert_eq!(pick_ui_font(&v(&["Segoe UI", "Inter"])), "Segoe UI");
        assert_eq!(pick_ui_font(&v(&[])), "Inter");
    }
    #[test]
    fn term_font_fallback() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let fb = v(&["Microsoft YaHei UI", "Symbols Nerd Font Mono"]);
        assert_eq!(
            pick_mono_family(&fb, &v(&["Cascadia Mono", "Consolas"])).as_deref(),
            Some("Cascadia Mono")
        );
        assert_eq!(
            pick_mono_family(&fb, &v(&["Segoe UI", "Consolas"])).as_deref(),
            Some("Consolas")
        );
        assert_eq!(
            pick_mono_family(&fb, &v(&["Symbols Nerd Font Mono", "Consolas"])).as_deref(),
            Some("Consolas")
        );
        assert_eq!(pick_mono_family(&[], &v(&[])), None);
    }
    #[test]
    fn mono_family_skips_proportional_fonts() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let fb = v(&["Microsoft YaHei UI", "Symbols Nerd Font Mono"]);
        assert_eq!(
            pick_mono_family(&fb, &v(&["Microsoft YaHei UI", "Consolas"])).as_deref(),
            Some("Consolas")
        );
        assert_eq!(
            pick_mono_family(&[], &v(&["Segoe UI", "Microsoft YaHei UI"])),
            None
        );
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
        let t = Scheme::from_alacritty("x", src).unwrap();
        assert_eq!(t.bg, 0x282c34);
        assert_eq!(t.ansi[9], 0xe06c75);
        assert_eq!(t.appearance, Appearance::Dark);
    }
    #[test]
    fn catalog_and_theme_palettes() {
        let themes: Vec<Arc<Theme>> = BUILTIN
            .iter()
            .map(|s| Arc::new(Theme::from_toml(s).unwrap()))
            .collect();
        assert!(themes.iter().any(|t| t.name == "Catppuccin Latte"));
        for src in BUILTIN_SCHEMES {
            let s = Scheme::from_toml(src).unwrap();
            let want = if luma(s.bg) > 0.5 {
                Appearance::Light
            } else {
                Appearance::Dark
            };
            assert_eq!(s.appearance, want, "{}", s.name);
        }
        let nord = Scheme::from_toml(include_str!("../schemes/nord.toml")).unwrap();
        assert_eq!(
            (nord.bg, nord.fg, nord.ansi[1]),
            (0x2e3440, 0xd8dee9, 0xbf616a)
        );
        let campbell = Scheme::from_toml(include_str!("../schemes/campbell.toml")).unwrap();
        assert_eq!(campbell.selection_fg, Some(0x0c0c0c));
        assert!(themes
            .iter()
            .any(|t| t.to_scheme().name == "ShellAX Daylight"));
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
        for i in 0..16 {
            body += &entry(&format!("Ansi {i} Color"), i as f32 / 15.0, 0.0, 1.0);
        }
        body += &entry("Background Color", 0.0, 0.0, 0.0);
        body += &entry("Foreground Color", 1.0, 1.0, 1.0);
        body += &entry("Cursor Color", 1.0, 0.0, 0.0);
        let src = format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
        );
        let t = Scheme::from_itermcolors("x", &src).unwrap();
        assert_eq!(t.bg, 0x000000);
        assert_eq!(t.fg, 0xffffff);
        assert_eq!(t.cursor, 0xff0000);
        assert_eq!(t.ansi[0], 0x0000ff);
        assert_eq!(t.ansi[15], 0xff00ff);
        assert_eq!(t.appearance, Appearance::Dark);
        assert!(Scheme::from_itermcolors("x", "<plist><dict></dict></plist>").is_err());
    }
}

// ---- 统一的三层背景：外层框架 / 卡片 / 悬停与选中。界面各处只用这几个，不再各自调深浅。

/// 当前配置的窗口不透明度（开关两档：关闭 1.0，开启 0.90 模糊液态玻璃）。
pub fn window_opacity() -> f32 {
    if crate::config::get().appearance.opacity < 0.999 {
        0.90
    } else {
        1.0
    }
}
/// 是否开启了模糊液态玻璃窗口半透明效果。
pub fn glass_enabled() -> bool {
    window_opacity() < 0.999
}
/// 在不透明实心模式下混合两种主题 HEX 颜色（100% 不透明，彻底避免 GPUI Metal 半透层叠导致的暗底透出）。
fn mix_solid(a_hex: u32, b_hex: u32, weight_a: f32) -> gpui::Hsla {
    let (a, b) = (gpui::Rgba::from(c(a_hex)), gpui::Rgba::from(c(b_hex)));
    let w = weight_a.clamp(0.0, 1.0);
    gpui::Rgba {
        r: a.r * w + b.r * (1.0 - w),
        g: a.g * w + b.g * (1.0 - w),
        b: a.b * w + b.b * (1.0 - w),
        a: 1.0,
    }
    .into()
}

/// 侧边栏与辅助面板层不透明度：macOS 上顺接外层磨砂框架 `chrome_bg` 并显著抑制半透叠层造成的暗底发灰（浅色尤需高纯度通透白感）。
pub fn panel_alpha() -> f32 {
    let op = window_opacity();
    if op >= 0.999 {
        1.0
    } else if cfg!(target_os = "macos") {
        if cur().appearance == Appearance::Dark {
            0.88
        } else {
            0.95
        }
    } else {
        (op + 0.01).clamp(0.55, 0.96)
    }
}
/// 中间内容卡片实际不透明度：保持工作区最高纯净度与文字锐利度，消除浅色下内容区比外框更暗的反差倒挂。
pub fn card_alpha() -> f32 {
    let op = window_opacity();
    if op >= 0.999 {
        1.0
    } else if cfg!(target_os = "macos") {
        if cur().appearance == Appearance::Dark {
            0.94
        } else {
            0.97
        }
    } else {
        (op + 0.03).clamp(0.55, 0.98)
    }
}
/// 实心卡片底色：用于主内容区内悬浮按钮条、分段控件选中态等需要遮挡下方滚动文字的元素。
pub fn solid_card_bg() -> gpui::Hsla {
    c(scheme().bg)
}
/// 实心侧栏底色：用于左侧面板虚拟列表吸顶标题，与 `panel_bg` 色相一致且不透出下方滚动行。
pub fn solid_panel_bg() -> gpui::Hsla {
    c(cur().mantle)
}
/// 侧边栏 / 右侧停靠栏 / 底部工具面板底色：取 `mantle`，作为外框 `crust` 到主内容 `base` 的自然过渡阶梯。
pub fn panel_bg() -> gpui::Hsla {
    c(cur().mantle).opacity(panel_alpha())
}
/// 中间主内容卡片底色（终端、SFTP、数据库等工作区）。
pub fn card_bg() -> gpui::Hsla {
    c(scheme().bg).opacity(card_alpha())
}
/// 设置页主卡片底色：居中衔接侧边栏 `panel_bg` 与工作区 `card_bg`，保证浅色明快干净、深色柔和抬升。
pub fn settings_card_bg() -> gpui::Hsla {
    if !glass_enabled() {
        return card_bg();
    }
    if cfg!(target_os = "macos") {
        let a = if cur().appearance == Appearance::Dark {
            0.91
        } else {
            0.96
        };
        c(cur().base).opacity(a)
    } else {
        card_bg()
    }
}
/// 设置页左侧导航栏底色：统一使用主题 `mantle` 阶梯，与主窗口侧边栏保持一致的平滑明度，不因黑底混合变灰。
pub fn settings_nav_bg() -> gpui::Hsla {
    c(cur().mantle)
}
/// 设置页右侧分组卡片底色：深浅与毛玻璃/实心模式下均使用 100% 干净抬升表面，浅色明亮通透、深色温和分层。
pub fn settings_group_bg() -> gpui::Hsla {
    let th = cur();
    if th.appearance == Appearance::Dark {
        mix_solid(th.surface0, th.base, 0.34)
    } else {
        mix_solid(0xffffff, th.base, 0.72)
    }
}
/// 弹出菜单、命令面板、悬浮 HUD 与模态对话框卡片底色：
/// GPUI 渲染层不支持对画布内下层文字做实时高斯模糊，若浮层小于 1.0 不透明度会导致底部文字/按钮直接穿透（鬼影）。
/// 因此所有浮层统一使用 100% 实心抬升表面（深色融合 28% `surface0` 呈现悬浮层次，浅色保持纯净 `base`）。
pub fn overlay_card_bg(_opaque_fallback: f32) -> gpui::Hsla {
    let th = cur();
    if th.appearance == Appearance::Dark {
        mix_solid(th.surface0, th.base, 0.28)
    } else {
        c(th.base)
    }
}
/// 模态弹窗与命令面板的全屏背景遮罩：以高浓度外框色阶柔化压暗底层工作区，配合 macOS 原生液态玻璃衬底突出前景弹窗。
pub fn modal_scrim_bg() -> gpui::Hsla {
    let th = cur();
    if th.appearance == Appearance::Dark {
        c(th.crust).opacity(0.76)
    } else {
        c(th.crust).opacity(0.72)
    }
}
/// 模态弹窗、下拉菜单、气泡卡片的高清轮廓边线。
pub fn overlay_border() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.13)
    } else {
        gpui::black().opacity(0.11)
    }
}
/// 液态玻璃卡片边缘细描边：深浅色均采用极柔和低对比发丝边，避免生硬亮白框。
pub fn glass_edge() -> Option<gpui::Hsla> {
    if !glass_enabled() {
        return None;
    }
    Some(if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.07)
    } else {
        gpui::black().opacity(0.055)
    })
}
/// 液态玻璃卡片的微阴影；关闭半透明时返回 `None`。
pub fn glass_island_shadow() -> Option<Vec<gpui::BoxShadow>> {
    if !glass_enabled() {
        return None;
    }
    let dark = cur().appearance == Appearance::Dark;
    Some(vec![gpui::BoxShadow {
        color: gpui::black().opacity(if dark { 0.10 } else { 0.025 }),
        offset: gpui::point(gpui::px(0.), gpui::px(1.)),
        blur_radius: gpui::px(6.),
        spread_radius: gpui::px(0.),
        inset: false,
    }])
}
/// 卡片内底栏（如 Agent 底部状态栏、终端路径栏）：使用轻柔过渡色，不形成突兀深黑条或灰条。
pub fn bar_bg() -> gpui::Hsla {
    let th = cur();
    if th.appearance == Appearance::Dark {
        mix_solid(th.crust, th.mantle, 0.35)
    } else {
        mix_solid(th.mantle, th.base, 0.75)
    }
}
/// 外层框架：窗口、标题栏、左右两侧图标栏、标签栏、状态栏。
/// macOS 液态玻璃下保留充足的磨砂底色浓度（0.78 / 0.84），既透出柔和环境光，又确保最左/最右未选中图标始终清晰可见，
/// 并缩小外框与左右面板卡片之间的明度跨度。
pub fn chrome_bg() -> gpui::Hsla {
    let op = window_opacity();
    let base = outer_bg();
    if op >= 0.999 {
        base
    } else if cfg!(target_os = "macos") {
        base.opacity(if cur().appearance == Appearance::Dark {
            0.78
        } else {
            0.84
        })
    } else {
        base.opacity((op - 0.04).clamp(0.65, 0.95))
    }
}
/// 悬停行：Linear/Raycast 风格中性细微高亮，避免带色偏。
pub fn hover_bg() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.05)
    } else {
        gpui::black().opacity(0.04)
    }
}
/// 选中行：克制的中性高亮层（Linear 风格，不用大面积蓝块淹没侧边栏，把强调色留给主按钮与 2px 指示条）。
pub fn selected_bg() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.085)
    } else {
        gpui::black().opacity(0.06)
    }
}
/// 强调色淡底：用于激活态筛选胶囊、徽标与状态指示底。
pub fn accent_bg() -> gpui::Hsla {
    c(cur().accent).opacity(if cur().appearance == Appearance::Dark {
        0.14
    } else {
        0.10
    })
}
/// 任意实心色块（如主按钮、红绿徽标、警告条）上的文字/图标前景色：按感知亮度自动切换纯白或深墨色。
pub fn on_color(hex: u32) -> gpui::Hsla {
    if luma(hex) < 0.62 {
        gpui::white()
    } else {
        gpui::rgb(0x111118).into()
    }
}
/// 实心强调色表面上的文字/图标前景色：按感知亮度自动切换纯白或深墨色，确保主按钮清晰锐利。
pub fn on_accent() -> gpui::Hsla {
    on_color(cur().accent)
}
/// 状态栏等弱化文字：取次文本色 subtext（与规范的文本层级一致）。
pub fn muted_text() -> gpui::Hsla {
    c(cur().subtext)
}
/// 弹出菜单 / 命令面板 / 下拉的键盘高亮行：中性微亮混色，文字保持 text。
pub fn menu_sel_bg() -> gpui::Hsla {
    mix_solid(cur().text, scheme().bg, 0.10)
}
/// 统一分隔线 / 1px 极细发丝线（深浅主题自适应低饱和中性灰，避免“框套框”粗糙感）。
pub fn divider() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.065)
    } else {
        gpui::black().opacity(0.065)
    }
}
/// 交互控件边框（次级按钮、分段滑块等），仅比发丝线略高半档。
pub fn control_border() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.095)
    } else {
        gpui::black().opacity(0.09)
    }
}
/// 输入框与搜索框静息态底色：微凹中性淡层。
pub fn input_bg() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.04)
    } else {
        gpui::black().opacity(0.032)
    }
}
/// 输入框与搜索框静息态边框：极轻发丝轮廓，聚焦时才切换为 `accent`。
pub fn input_border() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.07)
    } else {
        gpui::black().opacity(0.075)
    }
}
/// 选中/激活控件的强调色发丝边框。
pub fn active_border() -> gpui::Hsla {
    c(cur().accent).opacity(if cur().appearance == Appearance::Dark {
        0.34
    } else {
        0.28
    })
}
/// 浮层卡片轮廓边：液态玻璃模式用高光边，实色模式用极细发丝边。
pub fn island_border() -> gpui::Hsla {
    glass_edge().unwrap_or_else(divider)
}
/// 按钮 / 分段滑块 / 键帽的 1px 触感微阴影。
pub fn control_shadow() -> Vec<gpui::BoxShadow> {
    let dark = cur().appearance == Appearance::Dark;
    vec![gpui::BoxShadow {
        color: gpui::black().opacity(if dark { 0.26 } else { 0.06 }),
        offset: gpui::point(gpui::px(0.), gpui::px(1.)),
        blur_radius: gpui::px(2.),
        spread_radius: gpui::px(0.),
        inset: false,
    }]
}
/// 抬升小表面底色（次级按钮、键帽、空状态图标块、快捷入口图标槽）。
pub fn elevated_surface() -> gpui::Hsla {
    let th = cur();
    if th.appearance == Appearance::Dark {
        mix_solid(th.surface0, th.base, 0.78)
    } else {
        mix_solid(0xffffff, th.base, 0.84)
    }
}
/// 岛内子卡片底色：卡片底上叠一层很淡的中性表面。
pub fn sub_bg() -> gpui::Hsla {
    if cur().appearance == Appearance::Dark {
        gpui::white().opacity(0.032)
    } else {
        gpui::black().opacity(0.024)
    }
}
/// 自定义图标色板（8 色，id 与 `ssh_term_proto::iconset::COLORS` 一一对应）：每色给出浅色、深色主题各一组
/// `[后层, 前层, 字形]`；字形是叠在文件夹前层上的小图案，取与前层反差足够的颜色。蓝色即默认的 SFTP 文件夹蓝。
pub const ICON_TONES: [(&str, [u32; 3], [u32; 3]); 8] = [
    (
        "blue",
        [0x5aa9ec, 0x8cc8f8, 0x1f5f9f],
        [0x3d8fd6, 0x62aef0, 0xffffff],
    ),
    (
        "green",
        [0x4fb872, 0x86d6a2, 0x1f7a42],
        [0x3a9e5c, 0x5fc07f, 0xffffff],
    ),
    (
        "orange",
        [0xf09a3a, 0xf8bf70, 0x9a560a],
        [0xd97f22, 0xf2a24a, 0xffffff],
    ),
    (
        "red",
        [0xe8625a, 0xf59a93, 0x9c2a24],
        [0xcf4a43, 0xec746d, 0xffffff],
    ),
    (
        "purple",
        [0xa279e0, 0xc5a8f2, 0x5d3a9b],
        [0x8a5ec8, 0xae86e6, 0xffffff],
    ),
    (
        "cyan",
        [0x38bcd0, 0x76dae8, 0x0f6f80],
        [0x25a2b8, 0x4cc2d6, 0xffffff],
    ),
    (
        "gray",
        [0x8f98a5, 0xbcc3cc, 0x4b5563],
        [0x6f7885, 0x959eab, 0xffffff],
    ),
    (
        "yellow",
        [0xe8b923, 0xf7d760, 0x6b4f00],
        [0xcc9d10, 0xecc13c, 0x4a3600],
    ),
];
/// 色板里某个 id 的 `(后层, 前层)` 与字形色；未设置或 id 未知时取蓝色（默认）。
pub fn folder_tone(id: Option<&str>) -> ((gpui::Hsla, gpui::Hsla), gpui::Hsla) {
    let row = id
        .and_then(|i| ICON_TONES.iter().find(|r| r.0 == i))
        .unwrap_or(&ICON_TONES[0]);
    let t = if cur().appearance == Appearance::Dark {
        row.2
    } else {
        row.1
    };
    ((c(t[0]), c(t[1])), c(t[2]))
}
/// 主机图标的强调部分：设置了色板颜色时取其后层色，否则跟随主题强调色。
pub fn host_accent(id: Option<&str>) -> gpui::Hsla {
    match id.filter(|i| ICON_TONES.iter().any(|r| r.0 == *i)) {
        Some(i) => folder_tone(Some(i)).0 .0,
        None => entity_accent(),
    }
}
/// 主机 / 终端填充图标的机身：次要图标色与卡片底色各一半，浅色主题里是浅灰，深色主题里是深灰。
pub fn entity_body() -> gpui::Hsla {
    let (a, b) = (
        gpui::Rgba::from(c(cur().overlay1)),
        gpui::Rgba::from(solid_card_bg()),
    );
    gpui::Rgba {
        r: (a.r + b.r) / 2.,
        g: (a.g + b.g) / 2.,
        b: (a.b + b.b) / 2.,
        a: 1.,
    }
    .into()
}
/// 主机 / 终端填充图标的指示灯与屏幕：强调色。
pub fn entity_accent() -> gpui::Hsla {
    c(cur().accent)
}
/// 资源占用（0–1）的状态色：低于 70% 成功色，70–90% 警告色，高于 90% 错误色。
pub fn usage_color(frac: f32) -> gpui::Hsla {
    let th = cur();
    if frac > 0.9 {
        c(th.error)
    } else if frac > 0.7 {
        c(th.warning)
    } else {
        c(th.success)
    }
}
/// 延迟状态色：低于 100ms 成功色，100–300ms 警告色，更高或超时（None）错误色。
pub fn latency_color(ms: Option<u32>) -> gpui::Hsla {
    let th = cur();
    match ms {
        Some(v) if v < 100 => c(th.success),
        Some(v) if v <= 300 => c(th.warning),
        _ => c(th.error),
    }
}
/// 危险操作悬停底色。
pub fn danger_bg() -> gpui::Hsla {
    c(cur().error).opacity(0.12)
}

/// 界面字号基准 B（设置 › 外观 › 界面字号，已限制在 10–20），默认 13；`ui::fs_*` 由它推导。
static UI_SIZE: AtomicU32 = AtomicU32::new(13f32.to_bits());
pub fn set_ui_size(v: f32) {
    UI_SIZE.store(v.clamp(10., 20.).to_bits(), Ordering::Relaxed);
}
pub fn ui_size() -> f32 {
    f32::from_bits(UI_SIZE.load(Ordering::Relaxed))
}

/// 窗口外层（侧栏外框、标签行、状态栏所在）背景：优先忠实使用主题精心调校的 `crust`；
/// 仅当用户单独切换了与 `crust` 几乎完全同色的外部终端配色时，才做 2.8% 的温和明度微调，避免产生突兀反差。
pub fn outer_bg() -> gpui::Hsla {
    let mut o = c(cur().crust);
    let bg = c(scheme().bg);
    if (o.l - bg.l).abs() < 0.012 {
        o = bg;
        o.l = if bg.l > 0.5 {
            (bg.l - 0.028).max(0.)
        } else {
            (bg.l - 0.025).max(0.035)
        };
    }
    o
}

#[cfg(test)]
mod opacity_tests {
    use super::*;

    fn hsla_luma(col: gpui::Hsla) -> f32 {
        let r = gpui::Rgba::from(col);
        0.2126 * r.r + 0.7152 * r.g + 0.0722 * r.b
    }

    /// Approximate gpui-pre-apple Metal premultiplied RGB compositing over `chrome_bg`
    /// (`dst.rgb = a * src.rgb + (1 - a) * (ch * chrome.rgb)`).
    fn over_chrome(layer: gpui::Hsla, ch_col: gpui::Hsla) -> f32 {
        let l = gpui::Rgba::from(layer);
        let ch = gpui::Rgba::from(ch_col);
        let r = l.a * l.r + (1.0 - l.a) * (ch.a * ch.r);
        let g = l.a * l.g + (1.0 - l.a) * (ch.a * ch.g);
        let b = l.a * l.b + (1.0 - l.a) * (ch.a * ch.b);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    #[test]
    fn window_opacity_toggle_drives_glass_layers_and_solid_mode() {
        crate::config::preview(|s| s.appearance.opacity = 1.0);
        assert!(!glass_enabled());
        assert!((card_alpha() - 1.0).abs() < 1e-5);
        assert!((panel_alpha() - 1.0).abs() < 1e-5);
        assert!(glass_edge().is_none());
        assert_eq!(chrome_bg(), outer_bg());

        let themes = load_all();
        for name in ["ShellAX Midnight", "ShellAX Daylight", "Light", "Catppuccin Latte"] {
            let th = themes
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("missing builtin theme {name}"))
                .clone();
            let sch = Arc::new(th.to_scheme());
            set_current(th);
            set_scheme(sch);
            crate::config::preview(|s| {
                s.appearance.opacity = 0.90;
                s.appearance.theme = name.into();
            });
            assert!(glass_enabled());
            assert!((overlay_card_bg(0.88).a - 1.0).abs() < 1e-5);
            assert!((settings_group_bg().a - 1.0).abs() < 1e-5);
            assert!(glass_edge().is_some());
            assert!(glass_island_shadow().is_some());
            if cfg!(target_os = "macos") {
                let ch = chrome_bg();
                assert!(
                    (0.74..=0.86).contains(&ch.a),
                    "{name}: chrome_bg alpha out of expected frosted range: {}",
                    ch.a
                );
                let pa = panel_alpha();
                let sa = settings_card_bg().a;
                let ca = card_alpha();
                assert!(
                    ch.a < pa && pa < sa && sa < ca && pa >= 0.88,
                    "{name}: layer alphas must increase monotonically to avoid black-bleed greying: ch={}, pa={pa}, sa={sa}, ca={ca}",
                    ch.a
                );
                let chrome_l = hsla_luma(outer_bg());
                let sidebar_l = over_chrome(panel_bg(), ch);
                let nav_l = hsla_luma(settings_nav_bg());
                let settings_l = over_chrome(settings_card_bg(), ch);
                let group_l = hsla_luma(settings_group_bg());
                let card_l = over_chrome(card_bg(), ch);
                if cur().appearance == Appearance::Light {
                    assert!(
                        sidebar_l <= settings_l + 0.002 && settings_l <= group_l + 0.002,
                        "{name}: Light surface progression must never sink dark: chrome={chrome_l:.3}, sidebar={sidebar_l:.3}, nav={nav_l:.3}, settings={settings_l:.3}, group={group_l:.3}"
                    );
                    assert!(
                        (sidebar_l - chrome_l).abs() <= 0.05 && (settings_l - sidebar_l).abs() <= 0.05,
                        "{name}: Light neighboring panels should transition gently: chrome={chrome_l:.3}, sidebar={sidebar_l:.3}, settings={settings_l:.3}, card={card_l:.3}"
                    );
                }
            }
        }
    }
}
