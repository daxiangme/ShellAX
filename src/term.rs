use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};

/// Scrollback lines for new sessions (settings: terminal.scrollback).
pub static SCROLLBACK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(10_000);
use alacritty_terminal::vte::ansi::Processor;

/// Messages from the UI/tests to a backend (PTY or SSH).
pub enum BackendMsg {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    /// Pane closed: end the backend (local: kill shell; ssh: close channel; daemon: detach only).
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: usize,
    pub rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize { self.rows }
    fn screen_lines(&self) -> usize { self.rows }
    fn columns(&self) -> usize { self.cols }
}

/// Shared, cheaply-cloned bits the terminal listener writes into.
#[derive(Clone, Default)]
pub struct Shared {
    pub title: Arc<Mutex<String>>,
    /// OSC 9;4 progress payload (e.g. "4;1;-1"), used by agent detection.
    pub progress: Arc<Mutex<String>>,
    pub bell: Arc<AtomicBool>,
}

/// Listener that forwards terminal-generated replies (e.g. DA/DSR answers) back to the backend
/// and records title changes.
#[derive(Clone)]
pub struct Listener {
    tx: flume::Sender<BackendMsg>,
    shared: Shared,
}
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(s) => { let _ = self.tx.send(BackendMsg::Input(s.into_bytes())); }
            Event::Title(t) => *self.shared.title.lock().unwrap() = t,
            Event::ResetTitle => self.shared.title.lock().unwrap().clear(),
            Event::Bell => self.shared.bell.store(true, Ordering::Release),
            _ => {}
        }
    }
}

/// Max bytes parsed per lock acquisition; keeps the UI responsive during floods (`cat bigfile`).
const BATCH_BYTES: usize = 256 * 1024;

pub type FgProbe = Box<dyn Fn() -> Option<String> + Send + Sync>;

/// A terminal session: the parsed grid plus the channel to its backend.
pub struct Session {
    pub term: Arc<Mutex<Term<Listener>>>,
    pub exited: Arc<AtomicBool>,
    /// Event-driven wakeup: the parser signals after each batch (bounded(1) = coalesced).
    pub wake: flume::Receiver<()>,
    pub shared: Shared,
    /// Total bytes parsed (perf counter).
    pub bytes: Arc<AtomicU64>,
    tx: flume::Sender<BackendMsg>,
    wake_tx: flume::Sender<()>,
    size: Mutex<Size>,
    /// OSC 133;B position (screen line, column) = where the user's input starts, if the shell reports it.
    pub prompt_mark: Arc<Mutex<Option<(i32, usize)>>>,
    /// Remote/local shell history text fetched by the backend (bash/zsh history body).
    pub history_seed: Mutex<Option<String>>,
    /// Local PTYs: returns the foreground process name (for agent identification).
    pub fg_probe: Mutex<Option<FgProbe>>,
    /// Current working directory of the shell (local panes), for layout snapshots.
    pub cwd_probe: Mutex<Option<FgProbe>>,
    /// Backend connection state (SSH connects asynchronously; local is connected immediately).
    pub connected: AtomicBool,
    pub conn_error: Mutex<Option<String>>,
}

/// Find the next OSC marker of interest in `b`: 133 (shell integration) or 9;4 (progress).
/// Returns (start, end, kind, payload).
fn find_osc(b: &[u8]) -> Option<(usize, usize, u8, &[u8])> {
    let mut i = 0;
    while i + 4 < b.len() {
        let p = b[i..].windows(2).position(|w| w == b"\x1b]")? + i;
        let rest = &b[p + 2..];
        let (kind, skip) = if rest.starts_with(b"133;") { (b'1', 4) } else if rest.starts_with(b"9;4") { (b'9', 2) } else { i = p + 2; continue };
        let body = &rest[skip..];
        let end = body.iter().enumerate().find_map(|(j, &c)| {
            if c == 0x07 { Some((j, j + 1)) } else if c == 0x1b && body.get(j + 1) == Some(&b'\\') { Some((j, j + 2)) } else { None }
        })?;
        return Some((p, p + 2 + skip + end.1, kind, &body[..end.0]));
    }
    None
}

impl Session {
    /// Create a session. Returns the session, a sender the backend uses to push output
    /// bytes, and the receiver the backend reads input/resize messages from.
    pub fn new(size: Size) -> (Arc<Self>, flume::Sender<Vec<u8>>, flume::Receiver<BackendMsg>) {
        let (to_backend, from_ui) = flume::unbounded();
        let (out_tx, out_rx) = flume::unbounded::<Vec<u8>>();
        let (wake_tx, wake_rx) = flume::bounded::<()>(1);
        let shared = Shared::default();
        let cfg = Config { scrolling_history: SCROLLBACK.load(Ordering::Relaxed), ..Config::default() };
        let term = Term::new(cfg, &size, Listener { tx: to_backend.clone(), shared: shared.clone() });
        let s = Arc::new(Self {
            term: Arc::new(Mutex::new(term)),
            exited: Arc::new(AtomicBool::new(false)),
            wake: wake_rx,
            shared,
            bytes: Arc::new(AtomicU64::new(0)),
            tx: to_backend,
            wake_tx: wake_tx.clone(),
            size: Mutex::new(size),
            prompt_mark: Arc::new(Mutex::new(None)),
            history_seed: Mutex::new(None),
            fg_probe: Mutex::new(None),
            cwd_probe: Mutex::new(None),
            connected: AtomicBool::new(false),
            conn_error: Mutex::new(None),
        });
        let s2 = s.clone();
        std::thread::Builder::new()
            .name("term-parser".into())
            .spawn(move || {
                let mut parser: Processor = Processor::new();
                let mut pending: Option<Vec<u8>> = None;
                loop {
                    let first = match pending.take() { Some(b) => b, None => match out_rx.recv() { Ok(b) => b, Err(_) => break } };
                    let mut t = s2.term.lock().unwrap();
                    let mut feed = |t: &mut Term<Listener>, mut b: &[u8]| {
                        // split at OSC markers so we can sample the cursor exactly at the 133;B mark
                        while let Some((st, en, kind, payload)) = find_osc(b) {
                            parser.advance(t, &b[..st]);
                            if kind == b'1' {
                                let cur = t.grid().cursor.point;
                                let mut m = s2.prompt_mark.lock().unwrap();
                                match payload.first() { Some(b'B') => *m = Some((cur.line.0, cur.column.0)), _ => *m = None }
                            } else {
                                *s2.shared.progress.lock().unwrap() = String::from_utf8_lossy(payload).into_owned();
                            }
                            parser.advance(t, &b[st..en]);
                            b = &b[en..];
                        }
                        parser.advance(t, b);
                    };
                    let mut n = first.len();
                    feed(&mut t, &first);
                    // batch: drain what's already queued, but release the lock every BATCH_BYTES
                    while n < BATCH_BYTES {
                        match out_rx.try_recv() { Ok(more) => { n += more.len(); feed(&mut t, &more); } Err(_) => break }
                    }
                    drop(t);
                    s2.bytes.fetch_add(n as u64, Ordering::Relaxed);
                    let _ = wake_tx.try_send(());
                    if let Ok(more) = out_rx.try_recv() { pending = Some(more); }
                }
                s2.exited.store(true, Ordering::Release);
                let _ = wake_tx.try_send(());
            })
            .expect("spawn parser");
        (s, out_tx, from_ui)
    }

    /// Wake the UI (e.g. after a backend state change that produced no output).
    pub fn close(&self) { let _ = self.tx.send(BackendMsg::Close); }
    pub fn poke(&self) { let _ = self.wake_tx.try_send(()); }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.tx.send(BackendMsg::Input(bytes.into()));
    }

    pub fn size(&self) -> Size { *self.size.lock().unwrap() }

    pub fn resize(&self, size: Size) {
        if size.cols == 0 || size.rows == 0 { return; }
        let mut cur = self.size.lock().unwrap();
        if *cur == size { return; }
        *cur = size;
        self.term.lock().unwrap().resize(size);
        let _ = self.tx.send(BackendMsg::Resize { cols: size.cols as u16, rows: size.rows as u16 });
    }

    pub fn title(&self) -> String { self.shared.title.lock().unwrap().clone() }
    pub fn foreground_process(&self) -> Option<String> { self.fg_probe.lock().unwrap().as_ref().and_then(|f| f()) }

    /// Plain-text dump of the visible screen (used by headless tests and agent detection).
    pub fn screen_text(&self) -> String {
        let t = self.term.lock().unwrap();
        let g = t.grid();
        let mut out = String::new();
        for l in 0..g.screen_lines() {
            let row = &g[Line(l as i32)];
            let mut line: String = (0..g.columns()).map(|c| row[Column(c)].c).filter(|c| *c != '\0').collect();
            line.truncate(line.trim_end().len());
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn osc_scan() {
        let b = b"ab\x1b]0;t\x07cd\x1b]133;B\x07ef";
        let (s, e, k, p) = find_osc(b).unwrap();
        assert_eq!((k, p), (b'1', &b"B"[..]));
        assert_eq!(&b[s..e], b"\x1b]133;B\x07");
        let b = b"\x1b]9;4;1;-1\x1b\\";
        let (_, _, k, p) = find_osc(b).unwrap();
        assert_eq!((k, p), (b'9', &b"4;1;-1"[..]));
    }
}
