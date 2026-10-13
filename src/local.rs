use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::term::{BackendMsg, Session, Size};

/// bash shell-integration snippet: emits OSC 133;A before the prompt, ;B at the end of the prompt
/// (input start) and ;C when a command starts executing.
pub const BASH_INTEGRATION: &str = r#"
[ -f ~/.bashrc ] && . ~/.bashrc
__as_ps1_done=${__as_ps1_done:-0}
if [ "$__as_ps1_done" = 0 ]; then
  PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
  PS0='\e]133;C\a'"$PS0"
  __as_ps1_done=1
fi
"#;

/// Spawn the user's shell in a local PTY and wire it to a new Session.
pub fn spawn_local(size: Size, shell: Option<&str>) -> Result<Arc<Session>> {
    let (session, out_tx, from_ui) = Session::new(size);
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: size.rows as u16, cols: size.cols as u16, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = match shell {
        Some(s) => CommandBuilder::new(s),
        None => {
            let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
            if sh.ends_with("bash") {
                // shell integration: OSC 133 A/B/C marks via a wrapper rcfile (sources the user's ~/.bashrc first)
                let rc = std::env::temp_dir().join("agentshell-bash-integration.sh");
                let _ = std::fs::write(&rc, BASH_INTEGRATION);
                let mut c = CommandBuilder::new(&sh);
                c.args(["--rcfile", rc.to_str().unwrap_or_default()]);
                c
            } else { CommandBuilder::new(&sh) }
        }
    };
    cmd.env("TERM", "xterm-256color");
    if let Ok(home) = std::env::var("HOME") { cmd.cwd(home); }
    let mut child = pair.slave.spawn_command(cmd)?;
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(body) = std::fs::read_to_string(std::path::Path::new(&home).join(".bash_history")) {
            *session.history_seed.lock().unwrap() = Some(body);
        }
    }
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => { if out_tx.send(buf[..n].to_vec()).is_err() { break; } }
            }
        }
    });
    std::thread::spawn(move || {
        while let Ok(msg) = from_ui.recv() {
            match msg {
                BackendMsg::Input(b) => { let _ = writer.write_all(&b); let _ = writer.flush(); }
                BackendMsg::Resize { cols, rows } => {
                    let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                }
            }
        }
        let _ = child.kill();
    });
    Ok(session)
}
