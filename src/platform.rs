//! Per-OS directory conventions.
//! Linux/macOS: `~/.config/shellax` and `$XDG_DATA_HOME|~/.local/share/shellax`.
//! Windows: config in `%APPDATA%\ShellAX`, data in `%LOCALAPPDATA%\shellax`.
use std::path::PathBuf;

/// Call first thing in `main`. On Windows, fills in `HOME` from `%USERPROFILE%`
/// (the code uses `HOME` for `~` expansion, ~/.ssh and the file panes) and points
/// `XDG_DATA_HOME` at `%LOCALAPPDATA%`, so the shared data_dir logic lands there.
pub fn init_env() {
    #[cfg(windows)]
    {
        if std::env::var_os("HOME").is_none() {
            if let Some(p) = std::env::var_os("USERPROFILE") { std::env::set_var("HOME", p); }
        }
        if std::env::var_os("XDG_DATA_HOME").is_none() {
            if let Some(p) = std::env::var_os("LOCALAPPDATA") { std::env::set_var("XDG_DATA_HOME", p); }
        }
    }
    migrate_legacy();
}

/// One-time move of the pre-rename directories (AgentShell → coossh → ShellAX): config and data dirs are
/// renamed in place when the new one does not exist yet. Explicit overrides (SHELLAX_CONFIG) are
/// left alone. Errors are ignored (the app then simply starts with fresh settings).
pub fn migrate_legacy() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    // newest name first: coossh, then AgentShell; the first one found is moved, later ones are kept
    let mut pairs = vec![(data.join("coossh"), data.join("shellax")), (data.join("agentshell"), data.join("shellax"))];
    if std::env::var_os("SHELLAX_CONFIG").is_none() {
        #[cfg(windows)]
        if let Some(p) = std::env::var_os("APPDATA") {
            let p = PathBuf::from(p);
            pairs.push((p.join("coossh"), p.join("ShellAX")));
            pairs.push((p.join("AgentShell"), p.join("ShellAX")));
        }
        pairs.push((home.join(".config/coossh"), home.join(".config/shellax")));
        pairs.push((home.join(".config/agentshell"), home.join(".config/shellax")));
    }
    for (old, new) in pairs { migrate_dir(&old, &new); }
}

/// Rename `old` to `new` unless `new` already exists. Returns true when moved.
pub fn migrate_dir(old: &std::path::Path, new: &std::path::Path) -> bool {
    if new.exists() || !old.is_dir() { return false; }
    // different names, so a plain rename also works on case-insensitive file systems
    std::fs::rename(old, new).is_ok()
}


/// Windows GUI builds have no console: point stderr (eprintln!, panics) at
/// `<data_dir>/logs/shellax.log` (previous run kept as shellax.old.log).
/// No-op elsewhere or when SHELLAX_NO_LOGFILE is set.
pub fn init_log_file(data_dir: &std::path::Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        if std::env::var_os("SHELLAX_NO_LOGFILE").is_some() { return None; }
        let dir = data_dir.join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join("shellax.log");
        let _ = std::fs::rename(&p, dir.join("shellax.old.log"));
        let f = std::fs::File::create(&p).ok()?;
        let h = f.into_raw_handle();
        // SAFETY: h is a valid, owned file handle that we intentionally leak for the process lifetime.
        unsafe { windows_sys::Win32::System::Console::SetStdHandle(windows_sys::Win32::System::Console::STD_ERROR_HANDLE, h as _); }
        eprintln!("ShellAX {} started {}", env!("CARGO_PKG_VERSION"), chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"));
        Some(p)
    }
    #[cfg(not(windows))]
    { let _ = data_dir; None }
}

/// Directory holding config.toml, themes/ and context_menu.toml.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("APPDATA") { return PathBuf::from(p).join("ShellAX"); }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/shellax")
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DetectedTerminalFont {
    pub family: String,
    pub size: Option<f32>,
    pub source: String,
}

/// Matches a target font name (e.g. from Windows Terminal or VS Code) against installed fonts.
/// Supports Nerd Font aliases (e.g. "JetBrainsMono Nerd Font Mono" -> "JetBrainsMono NFM").
pub fn match_installed_font(target: &str, installed: &[String]) -> Option<String> {
    let target = target.trim();
    if target.is_empty() { return None; }
    if let Some(f) = installed.iter().find(|i| i.eq_ignore_ascii_case(target)) {
        return Some(f.clone());
    }
    let lower = target.to_lowercase();
    let mut aliases = Vec::new();
    if lower.contains("nerd font mono") {
        aliases.push(target.replace("Nerd Font Mono", "NFM"));
        aliases.push(target.replace(" Nerd Font Mono", " NFM"));
        aliases.push(target.replace("Nerd Font Mono", "NF"));
        aliases.push(target.replace(" Nerd Font Mono", " NF"));
    } else if lower.contains("nerd font") {
        aliases.push(target.replace("Nerd Font", "NF"));
        aliases.push(target.replace(" Nerd Font", " NF"));
        aliases.push(target.replace("Nerd Font", "NFM"));
        aliases.push(target.replace(" Nerd Font", " NFM"));
    }
    for a in &aliases {
        if let Some(f) = installed.iter().find(|i| i.eq_ignore_ascii_case(a.trim())) {
            return Some(f.clone());
        }
    }
    let clean = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let clean_target = clean(target);
    if !clean_target.is_empty() {
        if let Some(f) = installed.iter().find(|i| clean(i) == clean_target) {
            return Some(f.clone());
        }
        if let Some(f) = installed.iter().find(|i| {
            let ci = clean(i);
            !ci.is_empty() && (ci.starts_with(&clean_target) || clean_target.starts_with(&ci))
        }) {
            return Some(f.clone());
        }
    }
    None
}

fn strip_json_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") { continue; }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// A font that only carries icons. The app bundles Symbols Nerd Font Mono and registers it at
/// startup, so it shows up in the installed list, but it has no letters: using it as the terminal
/// font falls every glyph back to a proportional face and the grid falls apart.
fn icon_only_font(name: &str) -> bool {
    name.to_lowercase().contains("symbols")
}

/// Detects the terminal font configured in the system (e.g. Windows Terminal, VS Code)
/// or any installed Nerd Font on the machine.
pub fn detect_system_terminal_font(installed: &[String]) -> Option<DetectedTerminalFont> {
    #[cfg(windows)]
    {
        if let Some(df) = detect_windows_terminal_font(installed) {
            return Some(df);
        }
        if let Some(df) = detect_vscode_terminal_font(installed) {
            return Some(df);
        }
    }
    detect_installed_nerd_font(installed)
}

#[cfg(windows)]
fn detect_windows_terminal_font(installed: &[String]) -> Option<DetectedTerminalFont> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    let candidates = [
        local.join(r"Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\settings.json"),
        local.join(r"Packages\Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe\LocalState\settings.json"),
        local.join(r"Microsoft\Windows Terminal\settings.json"),
    ];
    for p in candidates {
        if !p.exists() { continue; }
        let Ok(text) = std::fs::read_to_string(&p) else { continue; };
        let stripped = strip_json_comments(&text);
        let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&stripped) else { continue; };

        let mut found_face = None;
        let mut found_size = None;
        if let Some(list) = json.get("profiles").and_then(|pr| pr.get("list")).and_then(|l| l.as_array()) {
            for prof in list {
                let name = prof.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_lowercase();
                if name.contains("powershell") || name.contains("pwsh") || name.contains("cmd") || name.contains("命令提示符") {
                    if let Some(face) = prof.get("font").and_then(|f| f.get("face")).and_then(|f| f.as_str())
                        .or_else(|| prof.get("fontFace").and_then(|f| f.as_str())) {
                        if !face.trim().is_empty() {
                            found_face = Some(face.trim().to_string());
                            found_size = prof.get("font").and_then(|f| f.get("size")).and_then(|s| s.as_f64())
                                .or_else(|| prof.get("fontSize").and_then(|s| s.as_f64())).map(|s| s as f32);
                            break;
                        }
                    }
                }
            }
        }

        if found_face.is_none() {
            if let Some(defaults) = json.get("profiles").and_then(|pr| pr.get("defaults")) {
                if let Some(face) = defaults.get("font").and_then(|f| f.get("face")).and_then(|f| f.as_str())
                    .or_else(|| defaults.get("fontFace").and_then(|f| f.as_str())) {
                    if !face.trim().is_empty() {
                        found_face = Some(face.trim().to_string());
                        found_size = defaults.get("font").and_then(|f| f.get("size")).and_then(|s| s.as_f64())
                            .or_else(|| defaults.get("fontSize").and_then(|s| s.as_f64())).map(|s| s as f32);
                    }
                }
            }
        }

        if let Some(raw_face) = found_face {
            // Only adopt a face that is actually installed here. The settings file may have been
            // copied from another machine (or name a font the user removed); returning the raw
            // name would persist an unresolvable family, and GPUI then silently substitutes a
            // proportional UI font, which tears the terminal grid apart.
            if let Some(matched) = match_installed_font(&raw_face, installed) {
                return Some(DetectedTerminalFont {
                    family: matched,
                    size: found_size,
                    source: "Windows Terminal".into(),
                });
            }
        }
    }
    None
}

#[cfg(windows)]
fn detect_vscode_terminal_font(installed: &[String]) -> Option<DetectedTerminalFont> {
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from)?;
    let p = appdata.join(r"Code\User\settings.json");
    if !p.exists() { return None; }
    let text = std::fs::read_to_string(&p).ok()?;
    let stripped = strip_json_comments(&text);
    let json: serde_json::Value = serde_json::from_str(&stripped).ok()?;
    let face = json.get("terminal.integrated.fontFamily").and_then(|f| f.as_str())?;
    let face = face.trim().trim_matches('\'').trim_matches('"');
    if face.is_empty() { return None; }
    let size = json.get("terminal.integrated.fontSize").and_then(|s| s.as_f64()).map(|s| s as f32);
    // Same rule as the Windows Terminal probe: an uninstalled face is not a detection result.
    let matched = match_installed_font(face, installed)?;
    Some(DetectedTerminalFont {
        family: matched,
        size,
        source: "VS Code".into(),
    })
}

pub fn detect_installed_nerd_font(installed: &[String]) -> Option<DetectedTerminalFont> {
    const PREFERRED: &[&str] = &[
        "JetBrainsMono NFM",
        "JetBrainsMono NF",
        "CaskaydiaCove Nerd Font Mono",
        "CaskaydiaCove Nerd Font",
        "CaskaydiaCove NF",
        "MesloLGS NF",
        "FiraCode Nerd Font Mono",
        "FiraCode Nerd Font",
        "Hack Nerd Font Mono",
        "Hack Nerd Font",
        "UbuntuMono Nerd Font",
    ];
    for pref in PREFERRED {
        if let Some(f) = installed.iter().find(|i| i.eq_ignore_ascii_case(pref) && !icon_only_font(i)) {
            return Some(DetectedTerminalFont {
                family: f.clone(),
                size: None,
                source: "Nerd Font".into(),
            });
        }
    }
    for f in installed {
        let l = f.to_lowercase();
        if icon_only_font(f) { continue; }
        if l.contains("nerd font") || l.ends_with(" nfm") || l.ends_with(" nf") {
            return Some(DetectedTerminalFont {
                family: f.clone(),
                size: None,
                source: "Nerd Font".into(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrates_once() {
        let d = std::env::temp_dir().join(format!("shellax-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("agentshell")).unwrap();
        std::fs::write(d.join("agentshell/config.toml"), "x").unwrap();
        assert!(super::migrate_dir(&d.join("agentshell"), &d.join("shellax")));
        assert_eq!(std::fs::read_to_string(d.join("shellax/config.toml")).unwrap(), "x");
        std::fs::create_dir_all(d.join("agentshell")).unwrap();
        assert!(!super::migrate_dir(&d.join("agentshell"), &d.join("shellax")), "new dir exists: keep it");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn matches_nerd_font_aliases() {
        let installed = vec![
            "Cascadia Mono".into(),
            "Consolas".into(),
            "JetBrainsMono NFM".into(),
            "JetBrainsMono NF".into(),
        ];
        assert_eq!(super::match_installed_font("JetBrainsMono Nerd Font Mono", &installed), Some("JetBrainsMono NFM".into()));
        assert_eq!(super::match_installed_font("JetBrainsMono Nerd Font", &installed), Some("JetBrainsMono NF".into()));
        assert_eq!(super::match_installed_font("cascadia mono", &installed), Some("Cascadia Mono".into()));
        assert_eq!(super::match_installed_font("NonExistent", &installed), None);
    }

    #[test]
    fn detects_installed_nerd_font_fallback() {
        let installed = vec![
            "Arial".into(),
            "Segoe UI".into(),
            "JetBrainsMono NFM".into(),
        ];
        let det = super::detect_installed_nerd_font(&installed).unwrap();
        assert_eq!(det.family, "JetBrainsMono NFM");
        assert_eq!(det.source, "Nerd Font");
    }

    #[test]
    fn skips_the_bundled_icon_font() {
        // Symbols Nerd Font Mono is registered by the app itself and contains no letters, so a
        // machine whose Windows Terminal and VS Code name no font must not be offered it.
        let installed = vec!["Segoe UI".into(), "Symbols Nerd Font Mono".into(), "Consolas".into()];
        assert_eq!(super::detect_installed_nerd_font(&installed), None);
        assert_eq!(super::detect_system_terminal_font(&installed), None);
    }

    #[cfg(windows)]
    #[test]
    fn detects_windows_terminal_font_live() {
        let installed = vec!["JetBrainsMono NFM".into(), "JetBrainsMono NF".into()];
        if let Some(det) = super::detect_system_terminal_font(&installed) {
            assert!(!det.family.is_empty());
        }
    }
}
