use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::term::{BackendMsg, Session, Size};

/// bash shell-integration snippet: emits OSC 133;A before the prompt, ;B at the end of the prompt
/// (input start) and ;C when a command starts executing.
pub const BASH_INTEGRATION: &str = r#"
[ -f ~/.bashrc ] && . ~/.bashrc
__as_ps1_done=${__as_ps1_done:-0}
if [ "$__as_ps1_done" = 0 ]; then
  PS1='\[\e]133;D;$?\a\e]7;file://${HOSTNAME}${PWD}\a\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
  PS0='\e]133;C\a'"$PS0"
  __as_ps1_done=1
fi
"#;

/// zsh integration via a temporary ZDOTDIR: sources the user's real .zshenv/.zshrc, then adds
/// precmd/preexec hooks. 133;B is (re)appended in precmd because prompt themes rebuild PS1 there.
pub const ZSH_ZSHENV: &str = r#"
__as_orig="${SHELLAX_ORIG_ZDOTDIR:-$HOME}"
[ -f "$__as_orig/.zshenv" ] && . "$__as_orig/.zshenv"
"#;
pub const ZSH_ZSHRC: &str = r#"
__as_orig="${SHELLAX_ORIG_ZDOTDIR:-$HOME}"
ZDOTDIR="$__as_orig"
[ -f "$__as_orig/.zshrc" ] && . "$__as_orig/.zshrc"
: ${HISTFILE:=$__as_orig/.zsh_history}
__as_precmd() {
  print -n "\e]133;D;$?\a"
  print -n "\e]7;file://${HOST}${PWD}\a"
  print -n '\e]133;A\a'
  [[ "$PS1" == *'133;B'* ]] || PS1="$PS1"$'%{\e]133;B\a%}'
}
__as_preexec() { print -n '\e]133;C\a' }
autoload -Uz add-zsh-hook
add-zsh-hook precmd __as_precmd
add-zsh-hook preexec __as_preexec
"#;

/// Shell started for a local pane without an explicit shell: $SHELL, else the platform default.
pub fn default_shell_path() -> String { std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(default_shell) }

/// Switches this process's console to UTF-8 for the duration of a spawn, then restores it.
///
/// `CreateProcess` copies the parent's input and output code pages into the child, and Windows
/// PowerShell 5.1 snapshots `[Console]::OutputEncoding` from that page before it prints anything.
/// Restoring matters: the GUI process keeps using this console afterwards.
#[cfg(windows)]
fn utf8_console() -> ConsolePages {
    use windows_sys::Win32::System::Console::{GetConsoleCP, GetConsoleOutputCP, SetConsoleCP, SetConsoleOutputCP};
    let saved = ConsolePages { input: unsafe { GetConsoleCP() }, output: unsafe { GetConsoleOutputCP() } };
    const UTF8: u32 = 65001;
    unsafe { SetConsoleOutputCP(UTF8); SetConsoleCP(UTF8); }
    saved
}

#[cfg(windows)]
struct ConsolePages { input: u32, output: u32 }

#[cfg(windows)]
impl Drop for ConsolePages {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};
        unsafe { SetConsoleOutputCP(self.output); SetConsoleCP(self.input); }
    }
}

/// Login shell when $SHELL is unset. Windows: PowerShell 7 (pwsh) if on PATH, else
/// Windows PowerShell, else %COMSPEC% (cmd.exe).
fn default_shell() -> String {
    #[cfg(windows)]
    {
        let on_path = |exe: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()));
        if on_path("pwsh.exe") { return "pwsh.exe".into(); }
        if on_path("powershell.exe") { return "powershell.exe".into(); }
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    }
    #[cfg(not(windows))]
    { "/bin/bash".into() }
}

fn home() -> Option<std::path::PathBuf> { std::env::var_os("HOME").map(Into::into) }

/// Spawn the user's shell in a local PTY and wire it to a new Session.
/// Shell integration (OSC 133 hooks for bash/zsh): 0 = off, 1 = only for the default $SHELL, 2 = always.
pub static INTEGRATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Color aliases appended to a local bash/zsh startup file. Session only.
const COLOR_ALIASES: &str = r#"
if command -v dircolors >/dev/null 2>&1; then eval "$(dircolors -b 2>/dev/null)"; fi
alias ls='ls --color=auto'
alias ll='ls -alF --color=auto'
alias grep='grep --color=auto'
alias egrep='egrep --color=auto'
alias fgrep='fgrep --color=auto'
"#;

/// `shell`: explicit shell path; None = $SHELL (bash/zsh get OSC 133 integration).
pub fn spawn_local(size: Size, shell: Option<&str>) -> Result<Arc<Session>> { spawn_local_in(size, shell, None) }

/// Like `spawn_local`, starting in `cwd` (layout restore) when it exists.
pub fn spawn_local_in(size: Size, shell: Option<&str>, cwd: Option<&std::path::Path>) -> Result<Arc<Session>> {
    let (session, out_tx, from_ui) = Session::new(size);
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: size.rows as u16, cols: size.cols as u16, pixel_width: 0, pixel_height: 0 })?;
    let sh = shell.map(str::to_string).unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| default_shell()));
    let integrate = match INTEGRATE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        2 => true,
        _ => shell.is_none() || std::env::var("SHELLAX_INTEGRATE").is_ok(),
    };
    let style = crate::ssh::STYLE.load(std::sync::atomic::Ordering::Relaxed);
    let mut cmd = CommandBuilder::new(&sh);
    let mut hist_file = ".bash_history";
    if (integrate || style) && sh.ends_with("bash") {
        let mut body = if integrate { BASH_INTEGRATION.to_string() } else { "[ -f ~/.bashrc ] && . ~/.bashrc\n".into() };
        if style { body.push_str(COLOR_ALIASES); }
        let rc = std::env::temp_dir().join("shellax-bash-integration.sh");
        let _ = std::fs::write(&rc, body);
        cmd.args(["--rcfile", rc.to_str().unwrap_or_default()]);
    } else if (integrate || style) && sh.ends_with("zsh") {
        let dir = std::env::temp_dir().join("shellax-zsh");
        let _ = std::fs::create_dir_all(&dir);
        let zshenv = if integrate { ZSH_ZSHENV.to_string() } else { "[ -f \"$HOME/.zshenv\" ] && . \"$HOME/.zshenv\"\n".into() };
        let mut zshrc = if integrate { ZSH_ZSHRC.to_string() } else { "[ -f \"$HOME/.zshrc\" ] && . \"$HOME/.zshrc\"\n".into() };
        if style { zshrc.push_str(COLOR_ALIASES); }
        let _ = std::fs::write(dir.join(".zshenv"), zshenv);
        let _ = std::fs::write(dir.join(".zshrc"), zshrc);
        if let Ok(z) = std::env::var("ZDOTDIR") { cmd.env("SHELLAX_ORIG_ZDOTDIR", z); }
        cmd.env("ZDOTDIR", dir.as_os_str());
        hist_file = ".zsh_history";
    }
    if sh.ends_with("zsh") { hist_file = ".zsh_history"; }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // Windows PowerShell 5.1 reads the console code page once, at startup, and then encodes
    // everything it writes with it. A machine whose OEM code page is not 65001 therefore emits
    // GBK (or another legacy page) into a terminal that decodes UTF-8, which is exactly the
    // spaced-out banner. The code page is inherited from the spawning process, and a ConPTY child
    // has no console of its own for `chcp` to change afterwards, so it has to be set here — and
    // put back, because this also moves the GUI process's own console.
    #[cfg(windows)]
    let _code_page = utf8_console();
    match cwd.filter(|c| c.is_dir()) { Some(c) => cmd.cwd(c), None => if let Some(h) = home() { cmd.cwd(h); } }
    let mut child = pair.slave.spawn_command(cmd)?;
    if let Some(pid) = child.process_id() {
        *session.cwd_probe.lock().unwrap() = Some(Box::new(move || std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.display().to_string())));
    }
    if let Some(h) = home() {
        if let Ok(body) = std::fs::read(h.join(hist_file)) {
            *session.history_seed.lock().unwrap() = Some(String::from_utf8_lossy(&body).into_owned());
        }
    }
    drop(pair.slave);
    session.connected.store(true, std::sync::atomic::Ordering::Release);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = Arc::new(Mutex::new(pair.master));
    // Windows/ConPTY has no tcgetpgrp: no foreground probe (agent auto-detection off there)
    #[cfg(unix)]
    {
        // foreground process of the pty (tcgetpgrp) -> /proc/<pid>/comm; used for agent identification
        let m = master.clone();
        *session.fg_probe.lock().unwrap() = Some(Box::new(move || {
            let pid = m.lock().ok()?.process_group_leader()?;
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            // node-based CLIs show up as "node"; use the script basename from argv then
            let args: Vec<String> = cmdline.split(|b| *b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            let comm = comm.trim().to_string();
            if matches!(comm.as_str(), "node" | "bun" | "python3" | "python") {
                if let Some(a) = args.get(1) { return Some(a.rsplit('/').next().unwrap_or(a).to_string()); }
            }
            Some(comm)
        }));
    }

    std::thread::Builder::new().name("pty-reader".into()).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => { if out_tx.send(buf[..n].to_vec()).is_err() { break; } }
            }
        }
    })?;
    std::thread::Builder::new().name("pty-writer".into()).spawn(move || {
        while let Ok(msg) = from_ui.recv() {
            match msg {
                BackendMsg::Close => break,
                BackendMsg::Input(b) => { let _ = writer.write_all(&b); let _ = writer.flush(); }
                BackendMsg::Resize { cols, rows } => {
                    let _ = master.lock().unwrap().resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                }
            }
        }
        let _ = child.kill();
    })?;
    Ok(session)
}
