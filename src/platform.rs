//! Per-OS directory conventions.
//! Linux/macOS: `~/.config/agentshell` and `$XDG_DATA_HOME|~/.local/share/agentshell`.
//! Windows: config in `%APPDATA%\AgentShell`, data in `%LOCALAPPDATA%\AgentShell`.
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
}

/// Windows GUI builds have no console: point stderr (eprintln!, panics) at
/// `<data_dir>/logs/agentshell.log` (previous run kept as agentshell.old.log).
/// No-op elsewhere or when AGENTSHELL_NO_LOGFILE is set.
pub fn init_log_file(data_dir: &std::path::Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        if std::env::var_os("AGENTSHELL_NO_LOGFILE").is_some() { return None; }
        let dir = data_dir.join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join("agentshell.log");
        let _ = std::fs::rename(&p, dir.join("agentshell.old.log"));
        let f = std::fs::File::create(&p).ok()?;
        let h = f.into_raw_handle();
        // SAFETY: h is a valid, owned file handle that we intentionally leak for the process lifetime.
        unsafe { windows_sys::Win32::System::Console::SetStdHandle(windows_sys::Win32::System::Console::STD_ERROR_HANDLE, h as _); }
        eprintln!("AgentShell {} started {}", env!("CARGO_PKG_VERSION"), chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"));
        Some(p)
    }
    #[cfg(not(windows))]
    { let _ = data_dir; None }
}

/// Directory holding config.toml, themes/ and context_menu.toml.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("APPDATA") { return PathBuf::from(p).join("AgentShell"); }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/agentshell")
}
