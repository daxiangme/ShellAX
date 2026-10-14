//! Per-OS directory conventions.
//! Linux/macOS: `~/.config/coossh` and `$XDG_DATA_HOME|~/.local/share/coossh`.
//! Windows: config in `%APPDATA%\coossh`, data in `%LOCALAPPDATA%\coossh`.
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

/// One-time move of the pre-rename directories (AgentShell → coossh): config and data dirs are
/// renamed in place when the new one does not exist yet. Explicit overrides (COOSSH_CONFIG) are
/// left alone. Errors are ignored (the app then simply starts with fresh settings).
pub fn migrate_legacy() {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let mut pairs = vec![(data.join("agentshell"), data.join("coossh"))];
    if std::env::var_os("COOSSH_CONFIG").is_none() {
        #[cfg(windows)]
        if let Some(p) = std::env::var_os("APPDATA") { let p = PathBuf::from(p); pairs.push((p.join("AgentShell"), p.join("coossh"))); }
        pairs.push((home.join(".config/agentshell"), home.join(".config/coossh")));
    }
    for (old, new) in pairs { migrate_dir(&old, &new); }
}

/// Rename `old` to `new` unless `new` already exists. Returns true when moved.
pub fn migrate_dir(old: &std::path::Path, new: &std::path::Path) -> bool {
    if new.exists() || !old.is_dir() { return false; }
    // case-insensitive file systems (Windows/macOS): "AgentShell" and "coossh" differ, so a plain rename works
    std::fs::rename(old, new).is_ok()
}


/// Windows GUI builds have no console: point stderr (eprintln!, panics) at
/// `<data_dir>/logs/coossh.log` (previous run kept as coossh.old.log).
/// No-op elsewhere or when COOSSH_NO_LOGFILE is set.
pub fn init_log_file(data_dir: &std::path::Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        if std::env::var_os("COOSSH_NO_LOGFILE").is_some() { return None; }
        let dir = data_dir.join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join("coossh.log");
        let _ = std::fs::rename(&p, dir.join("coossh.old.log"));
        let f = std::fs::File::create(&p).ok()?;
        let h = f.into_raw_handle();
        // SAFETY: h is a valid, owned file handle that we intentionally leak for the process lifetime.
        unsafe { windows_sys::Win32::System::Console::SetStdHandle(windows_sys::Win32::System::Console::STD_ERROR_HANDLE, h as _); }
        eprintln!("coossh {} started {}", env!("CARGO_PKG_VERSION"), chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"));
        Some(p)
    }
    #[cfg(not(windows))]
    { let _ = data_dir; None }
}

/// Directory holding config.toml, themes/ and context_menu.toml.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("APPDATA") { return PathBuf::from(p).join("coossh"); }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/coossh")
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrates_once() {
        let d = std::env::temp_dir().join(format!("coossh-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("agentshell")).unwrap();
        std::fs::write(d.join("agentshell/config.toml"), "x").unwrap();
        assert!(super::migrate_dir(&d.join("agentshell"), &d.join("coossh")));
        assert_eq!(std::fs::read_to_string(d.join("coossh/config.toml")).unwrap(), "x");
        std::fs::create_dir_all(d.join("agentshell")).unwrap();
        assert!(!super::migrate_dir(&d.join("agentshell"), &d.join("coossh")), "new dir exists: keep it");
        let _ = std::fs::remove_dir_all(&d);
    }
}
