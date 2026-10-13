use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

/// Messages from the UI/tests to a backend (PTY or SSH).
pub enum BackendMsg {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
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

/// Listener that forwards terminal-generated replies (e.g. DA/DSR answers) back to the backend.
#[derive(Clone)]
pub struct Listener {
    tx: flume::Sender<BackendMsg>,
}
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(s) = event {
            let _ = self.tx.send(BackendMsg::Input(s.into_bytes()));
        }
    }
}

/// A terminal session: the parsed grid plus the channel to its backend.
pub struct Session {
    pub term: Arc<Mutex<Term<Listener>>>,
    pub dirty: Arc<AtomicBool>,
    pub exited: Arc<AtomicBool>,
    tx: flume::Sender<BackendMsg>,
    size: Mutex<Size>,
    /// OSC 133;B position (screen line, column) = where the user's input starts, if the shell reports it.
    pub prompt_mark: Arc<Mutex<Option<(i32, usize)>>>,
    /// Remote/local shell history text fetched by the backend (bash_history body).
    pub history_seed: Mutex<Option<String>>,
}

/// Find the next OSC 133 marker in `b`; returns (start, end, kind) where `end` is just past the terminator.
fn find_osc133(b: &[u8]) -> Option<(usize, usize, u8)> {
    let pat = b"\x1b]133;";
    let start = b.windows(pat.len()).position(|w| w == pat)?;
    let kind = *b.get(start + pat.len())?;
    let rest = &b[start + pat.len()..];
    let end = rest.iter().enumerate().find_map(|(i, &c)| {
        if c == 0x07 { Some(i + 1) } else if c == 0x1b && rest.get(i + 1) == Some(&b'\\') { Some(i + 2) } else { None }
    })?;
    Some((start, start + pat.len() + end, kind))
}

impl Session {
    /// Create a session. Returns the session, a sender the backend uses to push output
    /// bytes, and the receiver the backend reads input/resize messages from.
    pub fn new(size: Size) -> (Arc<Self>, flume::Sender<Vec<u8>>, flume::Receiver<BackendMsg>) {
        let (to_backend, from_ui) = flume::unbounded();
        let (out_tx, out_rx) = flume::unbounded::<Vec<u8>>();
        let term = Term::new(Config::default(), &size, Listener { tx: to_backend.clone() });
        let s = Arc::new(Self {
            term: Arc::new(Mutex::new(term)),
            dirty: Arc::new(AtomicBool::new(true)),
            exited: Arc::new(AtomicBool::new(false)),
            tx: to_backend,
            size: Mutex::new(size),
            prompt_mark: Arc::new(Mutex::new(None)),
            history_seed: Mutex::new(None),
        });
        let s2 = s.clone();
        std::thread::Builder::new()
            .name("term-parser".into())
            .spawn(move || {
                let mut parser: Processor = Processor::new();
                while let Ok(bytes) = out_rx.recv() {
                    let mut t = s2.term.lock().unwrap();
                    let mut feed = |t: &mut Term<Listener>, mut b: &[u8]| {
                        // split at OSC 133 markers so we can sample the cursor exactly at the mark
                        while let Some((st, en, kind)) = find_osc133(b) {
                            parser.advance(t, &b[..st]);
                            let cur = t.grid().cursor.point;
                            let mut m = s2.prompt_mark.lock().unwrap();
                            match kind { b'B' => *m = Some((cur.line.0, cur.column.0)), _ => *m = None }
                            b = &b[en..];
                        }
                        parser.advance(t, b);
                    };
                    feed(&mut t, &bytes);
                    // drain whatever else is already queued under the same lock
                    while let Ok(more) = out_rx.try_recv() {
                        feed(&mut t, &more);
                    }
                    drop(t);
                    s2.dirty.store(true, Ordering::Release);
                }
                s2.exited.store(true, Ordering::Release);
                s2.dirty.store(true, Ordering::Release);
            })
            .expect("spawn parser");
        (s, out_tx, from_ui)
    }

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
        self.dirty.store(true, Ordering::Release);
    }

    /// Plain-text dump of the visible screen (used by headless tests).
    pub fn screen_text(&self) -> String {
        let t = self.term.lock().unwrap();
        let g = t.grid();
        let mut out = String::new();
        for l in 0..g.screen_lines() {
            let row = &g[Line(l as i32)];
            let mut line: String = (0..g.columns()).map(|c| row[Column(c)].c).collect();
            line.truncate(line.trim_end().len());
            out.push_str(&line);
            out.push('\n');
        }
        out
    }
}
