//! Local keep-alive daemon (`shellax-cli daemon`): owns local PTYs so shells / agents survive
//! the window closing; the GUI attaches over a Unix socket and gets the recent output replayed.
//!
//! Wire protocol: client sends one JSON line `Req`, then frames `[kind u8][len u32 BE][data]`.
//! client→daemon kinds: 0 input, 1 resize (cols u16, rows u16). daemon→client: 0 output, 2 exited.
#![cfg(unix)]
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};

use crate::term::{BackendMsg, Session, Size};

const REPLAY: usize = 512 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct Req { pub op: String, pub id: String, pub cols: u16, pub rows: u16, pub shell: String, pub cwd: String }

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Info { pub id: String, pub pid: u32, pub alive: bool, pub attached: bool, pub cwd: String }

struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    buf: Vec<u8>,
    client: Option<UnixStream>,
    alive: bool,
    pid: u32,
}
type Map = Arc<Mutex<HashMap<String, Arc<Mutex<Pty>>>>>;

pub fn socket_path(data_dir: &Path) -> PathBuf { data_dir.join("shellaxd.sock") }

fn frame(w: &mut impl Write, kind: u8, data: &[u8]) -> std::io::Result<()> {
    let mut v = Vec::with_capacity(5 + data.len());
    v.push(kind);
    v.extend_from_slice(&(data.len() as u32).to_be_bytes());
    v.extend_from_slice(data);
    w.write_all(&v)
}
fn read_frame(r: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut h = [0u8; 5];
    r.read_exact(&mut h)?;
    let n = u32::from_be_bytes([h[1], h[2], h[3], h[4]]) as usize;
    let mut d = vec![0u8; n.min(1 << 24)];
    r.read_exact(&mut d)?;
    Ok((h[0], d))
}

/// Run the daemon in the foreground (blocks). Exits when idle with no sessions for `idle_exit`.
pub fn serve(sock: &Path) -> Result<()> {
    if let Some(d) = sock.parent() { std::fs::create_dir_all(d)?; }
    if UnixStream::connect(sock).is_ok() { return Err(anyhow!("daemon already running at {}", sock.display())); }
    let _ = std::fs::remove_file(sock);
    let l = UnixListener::bind(sock)?;
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))?; }
    let map: Map = Default::default();
    for c in l.incoming() {
        let Ok(c) = c else { continue };
        let map = map.clone();
        std::thread::spawn(move || { let _ = handle(c, map); });
    }
    Ok(())
}

fn handle(c: UnixStream, map: Map) -> Result<()> {
    let mut rd = BufReader::new(c.try_clone()?);
    let mut line = String::new();
    rd.read_line(&mut line)?;
    let req: Req = serde_json::from_str(line.trim())?;
    let mut c = c;
    match req.op.as_str() {
        "list" => {
            let v: Vec<Info> = map.lock().unwrap().iter().map(|(id, p)| { let p = p.lock().unwrap(); Info { id: id.clone(), pid: p.pid, alive: p.alive, attached: p.client.is_some(), cwd: std::fs::read_link(format!("/proc/{}/cwd", p.pid)).map(|x| x.display().to_string()).unwrap_or_default() } }).collect();
            writeln!(c, "{}", serde_json::to_string(&v)?)?;
        }
        "kill" => {
            if let Some(p) = map.lock().unwrap().remove(&req.id) { let p = p.lock().unwrap(); unsafe { libc_kill(p.pid as i32, 1); } }
            writeln!(c, "ok")?;
        }
        "attach" => {
            let existing = map.lock().unwrap().get(&req.id).cloned();
            let pty = match existing {
                Some(p) if p.lock().unwrap().alive => p,
                _ => { let p = spawn(&req, map.clone())?; map.lock().unwrap().insert(req.id.clone(), p.clone()); p }
            };
            {
                let mut p = pty.lock().unwrap();
                let _ = p.master.resize(PtySize { rows: req.rows.max(2), cols: req.cols.max(2), pixel_width: 0, pixel_height: 0 });
                if let Some(old) = p.client.take() { let _ = old.shutdown(std::net::Shutdown::Both); }
                frame(&mut c, 0, &p.buf)?;
                p.client = Some(c.try_clone()?);
            }
            // input loop
            while let Ok((k, d)) = read_frame(&mut rd) {
                let mut p = pty.lock().unwrap();
                match k {
                    0 => { let _ = p.writer.write_all(&d); let _ = p.writer.flush(); }
                    1 if d.len() == 4 => { let _ = p.master.resize(PtySize { cols: u16::from_be_bytes([d[0], d[1]]), rows: u16::from_be_bytes([d[2], d[3]]), pixel_width: 0, pixel_height: 0 }); }
                    _ => {}
                }
            }
            // detach (keep the PTY running) unless this client was replaced
            let mut p = pty.lock().unwrap();
            if p.client.as_ref().map(|x| x.peer_addr().is_err()).unwrap_or(false) { p.client = None; }
            p.client = None;
        }
        _ => writeln!(c, "unknown op")?,
    }
    Ok(())
}

extern "C" { #[link_name = "kill"] fn libc_kill(pid: i32, sig: i32) -> i32; }

fn spawn(req: &Req, map: Map) -> Result<Arc<Mutex<Pty>>> {
    let pair = native_pty_system().openpty(PtySize { rows: req.rows.max(2), cols: req.cols.max(2), pixel_width: 0, pixel_height: 0 })?;
    let sh = if req.shell.is_empty() { std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()) } else { req.shell.clone() };
    let mut cmd = CommandBuilder::new(&sh);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("SHELLAX_DAEMON", "1");
    let cwd = if req.cwd.is_empty() { std::env::var("HOME").unwrap_or_else(|_| "/".into()) } else { req.cwd.clone() };
    cmd.cwd(cwd);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let pid = child.process_id().unwrap_or(0);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let p = Arc::new(Mutex::new(Pty { master: pair.master, writer, buf: Vec::new(), client: None, alive: true, pid }));
    let p2 = p.clone();
    let id = req.id.clone();
    std::thread::spawn(move || {
        let mut b = [0u8; 16384];
        loop {
            let n = match reader.read(&mut b) { Ok(0) | Err(_) => break, Ok(n) => n };
            let mut p = p2.lock().unwrap();
            p.buf.extend_from_slice(&b[..n]);
            if p.buf.len() > REPLAY { let cut = p.buf.len() - REPLAY; p.buf.drain(..cut); }
            if let Some(c) = p.client.as_mut() { if frame(c, 0, &b[..n]).is_err() { p.client = None; } }
        }
        let _ = child.wait();
        let mut p = p2.lock().unwrap();
        p.alive = false;
        if let Some(c) = p.client.as_mut() { let _ = frame(c, 2, b""); let _ = c.shutdown(std::net::Shutdown::Both); }
        drop(p);
        map.lock().unwrap().remove(&id);
    });
    Ok(p)
}

/// Client: list sessions held by the daemon.
pub fn list(sock: &Path) -> Result<Vec<Info>> {
    let mut c = UnixStream::connect(sock)?;
    writeln!(c, "{}", serde_json::to_string(&Req { op: "list".into(), ..Default::default() })?)?;
    let mut s = String::new();
    BufReader::new(c).read_line(&mut s)?;
    Ok(serde_json::from_str(s.trim())?)
}
pub fn kill(sock: &Path, id: &str) -> Result<()> {
    let mut c = UnixStream::connect(sock)?;
    writeln!(c, "{}", serde_json::to_string(&Req { op: "kill".into(), id: id.into(), ..Default::default() })?)?;
    let mut s = String::new();
    BufReader::new(c).read_line(&mut s)?;
    Ok(())
}

/// Make sure a daemon is listening (spawning `exe daemon` detached if needed).
pub fn ensure(sock: &Path, exe: &Path) -> Result<()> {
    if UnixStream::connect(sock).is_ok() { return Ok(()); }
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon").arg(sock).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    unsafe { cmd.pre_exec(|| { setsid(); Ok(()) }); }
    cmd.spawn()?;
    for _ in 0..50 { if UnixStream::connect(sock).is_ok() { return Ok(()); } std::thread::sleep(std::time::Duration::from_millis(50)); }
    Err(anyhow!("daemon did not start"))
}
extern "C" { fn setsid() -> i32; }

/// Attach a terminal Session to daemon PTY `id` (created on first attach).
pub fn attach(sock: &Path, id: &str, size: Size, shell: &str, cwd: &str) -> Result<Arc<Session>> {
    let mut c = UnixStream::connect(sock)?;
    writeln!(c, "{}", serde_json::to_string(&Req { op: "attach".into(), id: id.into(), cols: size.cols as u16, rows: size.rows as u16, shell: shell.into(), cwd: cwd.into() })?)?;
    let (session, out_tx, from_ui) = Session::new(size);
    session.connected.store(true, std::sync::atomic::Ordering::Release);
    // the daemon runs on this machine: probe the shell's foreground process and cwd via /proc
    let pid = Arc::new(std::sync::OnceLock::<u32>::new());
    let lookup = { let (sock, id, pid) = (sock.to_path_buf(), id.to_string(), pid.clone()); move || -> Option<u32> {
        if let Some(p) = pid.get() { return Some(*p); }
        let p = list(&sock).ok()?.into_iter().find(|i| i.id == id)?.pid;
        let _ = pid.set(p);
        Some(p)
    } };
    let l2 = lookup.clone();
    *session.fg_probe.lock().unwrap() = Some(Box::new(move || {
        let pid = lookup()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &stat[stat.rfind(')')? + 2..];
        let tpgid: i32 = rest.split(' ').nth(5)?.parse().ok()?;
        if tpgid <= 0 { return None; }
        Some(std::fs::read_to_string(format!("/proc/{tpgid}/comm")).ok()?.trim().to_string())
    }));
    *session.cwd_probe.lock().unwrap() = Some(Box::new(move || std::fs::read_link(format!("/proc/{}/cwd", l2()?)).ok().map(|p| p.display().to_string())));
    let mut rd = c.try_clone()?;
    let s2 = session.clone();
    std::thread::spawn(move || {
        while let Ok((k, d)) = read_frame(&mut rd) {
            if k == 2 { break; }
            if out_tx.send(d).is_err() { break; }
            s2.poke();
        }
        // dropping out_tx → session exits
    });
    std::thread::spawn(move || {
        while let Ok(m) = from_ui.recv() {
            let r = match m {
                BackendMsg::Close => break,
                BackendMsg::Input(b) => frame(&mut c, 0, &b),
                BackendMsg::Resize { cols, rows } => { let mut d = { cols }.to_be_bytes().to_vec(); d.extend_from_slice(&rows.to_be_bytes()); frame(&mut c, 1, &d) }
            };
            if r.is_err() { break; }
        }
        let _ = c.shutdown(std::net::Shutdown::Both);
    });
    Ok(session)
}
