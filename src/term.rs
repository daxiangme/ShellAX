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
    /// OSC 52 clipboard writes requested by the program (decided by the UI: allow / ask / deny).
    pub clip_store: Arc<Mutex<Vec<String>>>,
    /// OSC 52 clipboard reads; the formatter turns clipboard text into the reply sequence.
    pub clip_load: Arc<Mutex<Vec<ClipFormatter>>>,
}

pub type ClipFormatter = Arc<dyn Fn(&str) -> String + Sync + Send + 'static>;

/// OSC 52 write policy (`ssh.osc52_write`: allow | ask | deny).
/// Payloads over [`OSC52_MAX`] are still copied when the user allows them; the UI warns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipDecision { Allow, Ask, Deny }
pub const OSC52_MAX: usize = 1 << 20;
pub fn osc52_write_decision(mode: &str, _len: usize) -> ClipDecision {
    match mode { "allow" => ClipDecision::Allow, "deny" | "off" => ClipDecision::Deny, _ => ClipDecision::Ask }
}
pub fn osc52_is_large(len: usize) -> bool { len > OSC52_MAX }

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
            Event::ClipboardStore(_, text) => self.shared.clip_store.lock().unwrap().push(text),
            Event::ClipboardLoad(_, f) => self.shared.clip_load.lock().unwrap().push(f),
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
    inject_tx: flume::Sender<Vec<u8>>,
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
    /// Shell cwd reported via OSC 7 (`file://host/path`), local or remote.
    pub osc7_cwd: Arc<Mutex<Option<String>>>,
    /// OSC 133;D exit status of the last command and a counter bumped on each report.
    pub last_exit: Mutex<Option<i32>>,
    pub exit_seq: AtomicU64,
    /// Backend connection state (SSH connects asynchronously; local is connected immediately).
    pub connected: AtomicBool,
    pub conn_error: Mutex<Option<String>>,
    /// Inline images (Sixel/Kitty) placed at absolute grid rows (history-adjusted).
    pub images: Mutex<Vec<Placed>>,
    /// Cell size in pixels reported by the UI (for reserving rows under an image).
    pub cell_px: Mutex<(f32, f32)>,
    /// asciicast v2 recorder (output stream, redacted by default).
    pub recorder: Mutex<Option<crate::asciicast::Recorder>>,
    /// Keyword-triggered notifications; fired (title, line) pairs are queued in `notices`.
    pub notifier: Mutex<Option<crate::notify_rules::Notifier>>,
    pub notices: Mutex<Vec<(String, String)>>,
}
/// An inline image anchored at `row` = history_size + screen line at decode time.
#[derive(Clone, Debug)]
pub struct Placed { pub id: u64, pub row: i64, pub col: usize, pub image: crate::graphics::Image, pub rows: usize, pub cols: usize }

/// Find the next OSC marker of interest in `b`: 133 (shell integration) or 9;4 (progress).
/// Returns (start, end, kind, payload).
fn find_osc(b: &[u8]) -> Option<(usize, usize, u8, &[u8])> {
    let mut i = 0;
    while i + 4 < b.len() {
        let p = b[i..].windows(2).position(|w| w == b"\x1b]")? + i;
        let rest = &b[p + 2..];
        let (kind, skip) = if rest.starts_with(b"133;") { (b'1', 4) } else if rest.starts_with(b"9;4") { (b'9', 2) } else if rest.starts_with(b"7;") { (b'7', 2) } else if rest.starts_with(b"9;9;") { (b'c', 4) } else { i = p + 2; continue };
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
        // OSC 52 copy and paste both reach the listener; the UI applies terminal.osc52_write / osc52_read
        let cfg = Config { scrolling_history: SCROLLBACK.load(Ordering::Relaxed), osc52: alacritty_terminal::term::Osc52::CopyPaste, ..Config::default() };
        let term = Term::new(cfg, &size, Listener { tx: to_backend.clone(), shared: shared.clone() });
        let s = Arc::new(Self {
            term: Arc::new(Mutex::new(term)),
            exited: Arc::new(AtomicBool::new(false)),
            wake: wake_rx,
            shared,
            bytes: Arc::new(AtomicU64::new(0)),
            tx: to_backend,
            inject_tx: out_tx.clone(),
            wake_tx: wake_tx.clone(),
            size: Mutex::new(size),
            prompt_mark: Arc::new(Mutex::new(None)),
            history_seed: Mutex::new(None),
            fg_probe: Mutex::new(None),
            cwd_probe: Mutex::new(None),
            osc7_cwd: Arc::new(Mutex::new(None)),
            last_exit: Mutex::new(None),
            exit_seq: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            conn_error: Mutex::new(None),
            images: Mutex::new(vec![]),
            cell_px: Mutex::new((8., 16.)),
            recorder: Mutex::new(None),
            notifier: Mutex::new(None),
            notices: Mutex::new(vec![]),
        });
        let s2 = s.clone();
        std::thread::Builder::new()
            .name("term-parser".into())
            .spawn(move || {
                let mut parser: Processor = Processor::new();
                let mut gfx = crate::graphics::Extractor::default();
                let mut next_img = 1u64;
                let mut pending: Option<Vec<u8>> = None;
                loop {
                    let first = match pending.take() { Some(b) => b, None => match out_rx.recv() { Ok(b) => b, Err(_) => break } };
                    let first = s2.side_effects(first, &mut gfx);
                    let mut t = s2.term.lock().unwrap();
                    let feed = |t: &mut Term<Listener>, parser: &mut Processor, mut b: &[u8]| {
                        // split at OSC markers so we can sample the cursor exactly at the 133;B mark
                        while let Some((st, en, kind, payload)) = find_osc(b) {
                            parser.advance(t, &b[..st]);
                            if kind == b'1' {
                                let cur = t.grid().cursor.point;
                                let mut m = s2.prompt_mark.lock().unwrap();
                                match payload.first() { Some(b'B') => *m = Some((cur.line.0, cur.column.0)), _ => *m = None }
                                if payload.first() == Some(&b'D') {
                                    let code = std::str::from_utf8(payload).ok().and_then(|p| p.split(';').nth(1)).and_then(|c| c.trim().parse::<i32>().ok());
                                    *s2.last_exit.lock().unwrap() = code;
                                    s2.exit_seq.fetch_add(1, Ordering::Relaxed);
                                }
                            } else if kind == b'7' {
                                if let Some(p) = crate::ctxmenu::parse_osc7(&String::from_utf8_lossy(payload)) { *s2.osc7_cwd.lock().unwrap() = Some(p); }
                            } else if kind == b'c' {
                                if let Some(p) = crate::ctxmenu::parse_osc9_cwd(&String::from_utf8_lossy(payload)) { *s2.osc7_cwd.lock().unwrap() = Some(p); }
                            } else {
                                *s2.shared.progress.lock().unwrap() = String::from_utf8_lossy(payload).into_owned();
                            }
                            parser.advance(t, &b[st..en]);
                            b = &b[en..];
                        }
                        parser.advance(t, b);
                    };
                    let mut n = first.0.len();
                    let place = |t: &mut Term<Listener>, imgs: Vec<crate::graphics::Image>, parser: &mut Processor, next_img: &mut u64| for image in imgs {
                        let (cw, ch) = *s2.cell_px.lock().unwrap();
                        let (pw, ph) = image.size();
                        let (cols, rows) = ((pw as f32 / cw.max(1.)).ceil() as usize, (ph as f32 / ch.max(1.)).ceil().max(1.) as usize);
                        let cur = t.grid().cursor.point;
                        let row = t.grid().history_size() as i64 + cur.line.0 as i64;
                        s2.images.lock().unwrap().push(Placed { id: *next_img, row, col: cur.column.0, image, rows, cols });
                        *next_img += 1;
                        // Reserve the rows so following text starts below the picture.
                        parser.advance(t, &b"\r\n".repeat(rows));
                    };
                    let run = |t: &mut Term<Listener>, parser: &mut Processor, next_img: &mut u64, (bytes, imgs): (Vec<u8>, Vec<crate::graphics::Image>)| {
                        // feed text up to each image's offset, place the image, continue
                        let mut pos = 0;
                        for im in imgs { let at = im.at.min(bytes.len()); feed(t, parser, &bytes[pos..at]); pos = at; place(t, vec![im], parser, next_img); }
                        feed(t, parser, &bytes[pos..]);
                    };
                    run(&mut t, &mut parser, &mut next_img, first);
                    // batch: drain what's already queued, but release the lock every BATCH_BYTES
                    while n < BATCH_BYTES {
                        match out_rx.try_recv() { Ok(more) => { let more = s2.side_effects(more, &mut gfx); n += more.0.len(); run(&mut t, &mut parser, &mut next_img, more); } Err(_) => break }
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

    /// Recorder / notifier taps and graphics extraction on raw output.
    fn side_effects(&self, b: Vec<u8>, gfx: &mut crate::graphics::Extractor) -> (Vec<u8>, Vec<crate::graphics::Image>) {
        if let Some(r) = self.recorder.lock().unwrap().as_mut() { let _ = r.output(&b, false); }
        if let Some(n) = self.notifier.lock().unwrap().as_mut() {
            let f = n.feed(&b, std::time::Instant::now());
            if !f.is_empty() { self.notices.lock().unwrap().extend(f); }
        }
        if !b.contains(&0x1b) && !gfx.busy() { return (b, vec![]); }
        // Erase display / scrollback / full reset drops inline images (they belong to the erased text).
        if b.windows(4).any(|w| w == b"\x1b[2J" || w == b"\x1b[3J") || b.windows(2).any(|w| w == b"\x1bc") { self.images.lock().unwrap().clear(); }
        gfx.feed(&b)
    }
    pub fn start_recording(&self, path: &std::path::Path, title: &str, redact: bool) -> std::io::Result<()> {
        let sz = self.size();
        *self.recorder.lock().unwrap() = Some(crate::asciicast::Recorder::create(path, sz.cols as u16, sz.rows as u16, title, redact)?);
        Ok(())
    }
    pub fn stop_recording(&self) -> bool { self.recorder.lock().unwrap().take().map(|r| r.finish()).is_some() }
    pub fn is_recording(&self) -> bool { self.recorder.lock().unwrap().is_some() }
    /// Feed bytes straight into the parser (playback); goes through the same path as backend output.
    pub fn inject(&self, b: Vec<u8>) -> bool { self.inject_tx.send(b).is_ok() }

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

/// Smart case (PRD 3.2): a pattern without upper-case letters matches case-insensitively.
pub fn smart_case(p: &str) -> String { if p.chars().any(|c| c.is_uppercase()) { p.to_string() } else { format!("(?i){p}") } }

/// Regex search over screen + scrollback. Returns the match as ((line, col), (line, col)) in grid
/// coordinates (negative lines = scrollback). `from` is where the previous match started.
pub fn search<L: alacritty_terminal::event::EventListener>(term: &Term<L>, pattern: &str, from: Option<(i32, usize)>, forward: bool) -> Option<((i32, usize), (i32, usize))> {
    use alacritty_terminal::index::{Direction, Point, Side};
    use alacritty_terminal::term::search::RegexSearch;
    if pattern.is_empty() { return None; }
    let mut re = RegexSearch::new(&smart_case(pattern)).ok()?;
    let g = term.grid();
    let origin = match from {
        Some((l, c)) => {
            let p = Point::new(Line(l), Column(c));
            if forward { p.add(g, alacritty_terminal::index::Boundary::None, 1) } else { p.sub(g, alacritty_terminal::index::Boundary::None, 1) }
        }
        None => if forward { Point::new(g.topmost_line(), Column(0)) } else { Point::new(g.bottommost_line(), Column(g.columns() - 1)) },
    };
    let m = term.search_next(&mut re, origin, if forward { Direction::Right } else { Direction::Left }, Side::Left, None)?;
    Some(((m.start().line.0, m.start().column.0), (m.end().line.0, m.end().column.0)))
}

/// Selection hit test; `block` = rectangular (Alt+drag) selection.
pub fn in_selection(sel: Option<((i32, usize), (i32, usize))>, p: (i32, usize), block: bool) -> bool {
    let Some((a, b)) = sel else { return false };
    if block {
        let (l0, l1) = (a.0.min(b.0), a.0.max(b.0));
        let (c0, c1) = (a.1.min(b.1), a.1.max(b.1));
        return p.0 >= l0 && p.0 <= l1 && p.1 >= c0 && p.1 <= c1;
    }
    let (s, e) = if a <= b { (a, b) } else { (b, a) };
    p >= s && p <= e
}

/// Plain-text URL under column `col` of `line` (one char per column). Returns (start, end_exclusive, url).
pub fn url_at(line: &str, col: usize) -> Option<(usize, usize, String)> {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"(?:https?|ftp|file)://[^\s<>"'`|]+|www\.[A-Za-z0-9-]+\.[^\s<>"'`|]+"#).unwrap());
    let chars: Vec<char> = line.chars().collect();
    let s: String = chars.iter().collect();
    for m in RE.find_iter(&s) {
        let a = s[..m.start()].chars().count();
        let mut url = m.as_str().to_string();
        // trailing punctuation is usually prose, keep balanced closing parens
        while let Some(c) = url.chars().last() {
            let unbalanced = c == ')' && url.matches('(').count() < url.matches(')').count();
            if ".,;:!?'\"".contains(c) || unbalanced || c == ']' { url.pop(); } else { break }
        }
        let b = a + url.chars().count();
        if col >= a && col < b {
            if url.starts_with("www.") { url = format!("https://{url}"); }
            return Some((a, b, url));
        }
    }
    None
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
        let (_, _, k7, p7) = find_osc(b"x\x1b]7;file://h/tmp/a b\x07y").unwrap();
        assert_eq!((k7, p7), (b'7', &b"file://h/tmp/a b"[..]));
        let (_, _, kc, pc) = find_osc(b"x\x1b]9;9;\"C:\\Users\"\x1b\\y").unwrap();
        assert_eq!((kc, pc), (b'c', &b"\"C:\\Users\""[..]));
        let b = b"\x1b]9;4;1;-1\x1b\\";
        let (_, _, k, p) = find_osc(b).unwrap();
        assert_eq!((k, p), (b'9', &b"4;1;-1"[..]));
    }

    #[test]
    fn osc52_store_and_load_reach_listener() {
        let (sess, out, rx) = Session::new(Size { cols: 40, rows: 5 });
        // "hello" = aGVsbG8=
        out.send(b"\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07".to_vec()).unwrap();
        let t0 = std::time::Instant::now();
        while sess.shared.clip_load.lock().unwrap().is_empty() && t0.elapsed().as_secs() < 3 { std::thread::sleep(std::time::Duration::from_millis(10)); }
        assert_eq!(*sess.shared.clip_store.lock().unwrap(), vec!["hello".to_string()]);
        let f = sess.shared.clip_load.lock().unwrap().pop().unwrap();
        let reply = f("hi");
        assert!(reply.starts_with("\x1b]52;c;aGk="), "{reply:?}");
        // nothing is sent back by the terminal itself (reads are answered only by the UI)
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn osc52_policy() {
        assert_eq!(osc52_write_decision("allow", 10), ClipDecision::Allow);
        assert_eq!(osc52_write_decision("ask", 10), ClipDecision::Ask);
        assert_eq!(osc52_write_decision("deny", 10), ClipDecision::Deny);
        assert_eq!(osc52_write_decision("allow", OSC52_MAX + 1), ClipDecision::Allow);
        assert!(osc52_is_large(OSC52_MAX + 1));
        assert!(!osc52_is_large(OSC52_MAX));
    }

    #[test]
    fn url_detection() {
        let l = "see https://example.com/a_(b)?x=1, or www.rust-lang.org. done";
        let (a, b, u) = url_at(l, 6).unwrap();
        assert_eq!((a, u.as_str()), (4, "https://example.com/a_(b)?x=1"));
        assert_eq!(b, 4 + u.chars().count());
        assert_eq!(url_at(l, 40).unwrap().2, "https://www.rust-lang.org");
        assert!(url_at(l, 0).is_none());
        assert!(url_at("中文 http://x.cn/路径 尾", 4).unwrap().2.starts_with("http://x.cn/"));
    }

    #[test]
    fn block_selection_hit_test() {
        let sel = Some(((0, 5), (2, 2)));
        assert!(in_selection(sel, (1, 3), true));
        assert!(!in_selection(sel, (1, 6), true));
        assert!(in_selection(sel, (1, 6), false)); // linear selection covers the whole middle line
        assert!(!in_selection(sel, (0, 1), false));
    }

    #[test]
    fn regex_search_smart_case_and_direction() {
        let (sess, out, _rx) = Session::new(Size { cols: 30, rows: 4 });
        out.send(b"alpha Foo\r\nbeta foo\r\ngamma FOO\r\n".to_vec()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let t = sess.term.lock().unwrap();
        let a = search(&*t, "foo", None, true).unwrap();
        let b = search(&*t, "foo", Some(a.0), true).unwrap();
        assert!(b.0 > a.0, "{a:?} {b:?}");
        // smart case: "Foo" only matches the exact case
        let only: Vec<_> = { let mut v = vec![]; let mut at = None; for _ in 0..4 { let Some(m) = search(&*t, "Foo", at, true) else { break }; if v.contains(&m) { break } at = Some(m.0); v.push(m); } v };
        assert_eq!(only.len(), 1);
        assert_eq!(only[0], ((0, 6), (0, 8)));
        // backwards from the end finds the last one
        assert_eq!(search(&*t, "foo", None, false).unwrap().0 .0, 2);
        assert_eq!(smart_case("abc"), "(?i)abc");
    }
}
