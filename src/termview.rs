//! One terminal pane: input handling, selection/clipboard, mouse reporting, scrollback,
//! autosuggestion and agent detection. Painting is split into two child entities so that
//! a cursor blink repaints only the cursor layer while the grid is replayed from GPUI's
//! view cache (`Entity::cached`).
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use gpui::{
    canvas, div, fill, point, prelude::*, px, size, Bounds, ClipboardItem, Context, Entity, FocusHandle, Font,
    FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollDelta, ScrollWheelEvent, ShapedLine, SharedString, StrikethroughStyle, StyleRefinement, Subscription,
    TextAlign, TextRun, UnderlineStyle, Window,
};
use ssh_term_proto::agent::{self, AgentState, DetectionInput, Manifest};
use ssh_term_proto::boxdraw;
use ssh_term_proto::ssh::SshTarget;
use ssh_term_proto::suggest::{History, InputTracker, Key};
use ssh_term_proto::term::{Session, Size};

use crate::theme::{c, cur, term_color, Theme};
use crate::{config, font_size, line_h};

pub const PAD: f32 = 10.0;
const SCROLLBAR_W: f32 = 8.0;

// ---------------------------------------------------------------- perf counters (PERF_LOG=1)
pub static GRID_PAINTS: AtomicU64 = AtomicU64::new(0);
pub static ROWS_BUILT: AtomicU64 = AtomicU64::new(0);
pub static CURSOR_PAINTS: AtomicU64 = AtomicU64::new(0);

/// Terminal font: primary family + fallback chain, ligatures off (calt/liga/dlig = 0).
pub fn mono_font(bold: bool, italic: bool) -> Font {
    let st = config::get();
    let fc = &st.font;
    let mut f = gpui::font(SharedString::from(fc.family.clone()));
    if !fc.ligatures {
        f.features = gpui::FontFeatures(Arc::new(vec![("calt".into(), 0), ("liga".into(), 0), ("dlig".into(), 0)]));
    }
    f.fallbacks = Some(gpui::FontFallbacks(Arc::new(fc.fallbacks.clone())));
    if bold { f.weight = FontWeight::BOLD; }
    if italic { f.style = gpui::FontStyle::Italic; }
    f
}

#[derive(Clone, PartialEq)]
struct Style { fg: Hsla, bold: bool, italic: bool, underline: Option<UnderlineStyle>, strike: bool }

fn text_run(len: usize, s: &Style) -> TextRun {
    TextRun {
        len, font: mono_font(s.bold, s.italic), color: s.fg, background_color: None,
        underline: s.underline,
        strikethrough: s.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(s.fg) }),
    }
}
fn plain_run(len: usize, fg: Hsla) -> TextRun { text_run(len, &Style { fg, bold: false, italic: false, underline: None, strike: false }) }

/// What a pane connects to (used to open more panes of the same kind).
#[derive(Clone)]
pub enum Spec { Local, Ssh(SshTarget) }
impl Spec {
    pub fn history_key(&self) -> String { match self { Spec::Local => "local".into(), Spec::Ssh(t) => t.label() } }
}

/// Buffer coordinate: grid line (negative = scrollback) and column.
type BPoint = (i32, usize);

/// State shared by the pane and its two paint layers.
#[derive(Default)]
pub struct PaintState {
    pub cell_w: f32,
    pub origin: gpui::Point<Pixels>,
    pub bounds: Bounds<Pixels>,
    pub focused: bool,
    pub blink_on: bool,
    pub suggestion: Option<String>,
    pub sel: Option<(BPoint, BPoint)>,
    pub hover_link: Option<(i32, usize, usize)>,
}

// ---------------------------------------------------------------- grid layer (cached)
struct RowPaint {
    bgs: Vec<(usize, usize, Hsla)>,
    text: Option<ShapedLine>,
    wide: Vec<(usize, ShapedLine)>,
    boxes: Vec<(usize, char, Hsla)>,
}

#[derive(PartialEq, Clone)]
struct CacheKey { cols: usize, rows: usize, theme: SharedString, cw: u32, gen: u64, offset: usize, sel: Option<(BPoint, BPoint)>, hover: Option<(i32, usize, usize)> }

pub struct GridLayer {
    session: Option<Arc<Session>>,
    st: Rc<RefCell<PaintState>>,
    cache: Rc<RefCell<(Option<CacheKey>, Vec<Option<RowPaint>>)>>,
}

fn in_sel(sel: Option<(BPoint, BPoint)>, p: BPoint) -> bool {
    let Some((a, b)) = sel else { return false };
    let (s, e) = if a <= b { (a, b) } else { (b, a) };
    p >= s && p <= e
}

impl Render for GridLayer {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.clone();
        let st = self.st.clone();
        let st2 = self.st.clone();
        let cache = self.cache.clone();
        let th = cur();
        canvas(
            move |bounds, window, _cx| {
                let ts = window.text_system();
                let fid = ts.resolve_font(&mono_font(false, false));
                let cw = ts.advance(fid, px(font_size()), 'M').map(|a| f32::from(a.width)).unwrap_or(8.0);
                let mut s = st.borrow_mut();
                s.cell_w = cw;
                s.bounds = bounds;
                s.origin = bounds.origin + point(px(PAD), px(PAD));
                let cols = ((f32::from(bounds.size.width) - 2.0 * PAD - SCROLLBAR_W) / cw).floor().max(10.0) as usize;
                let rows = ((f32::from(bounds.size.height) - 2.0 * PAD) / line_h()).floor().max(3.0) as usize;
                if let Some(s) = &session { s.resize(Size { cols, rows }); }
                session
            },
            move |bounds, session, window, cx| {
                GRID_PAINTS.fetch_add(1, Ordering::Relaxed);
                let lh = line_h();
                let base = c(th.bg);
                window.paint_quad(fill(bounds, base));
                let Some(s) = session else { return };
                let st = st2.borrow();
                let (cw, origin, sel, hover) = (st.cell_w, st.origin, st.sel, st.hover_link);
                drop(st);
                let mut term = s.term.lock().unwrap();
                let (ncols, nrows, offset, history) = { let g = term.grid(); (g.columns(), g.screen_lines(), g.display_offset(), g.history_size()) };
                let key = CacheKey { cols: ncols, rows: nrows, theme: th.name.clone().into(), cw: cw.to_bits(), gen: config::generation(), offset, sel, hover };
                let mut cache = cache.borrow_mut();
                let full = cache.0.as_ref() != Some(&key) || offset != 0;
                let mut dirty = vec![full; nrows];
                if !full {
                    match term.damage() {
                        alacritty_terminal::term::TermDamage::Full => dirty.iter_mut().for_each(|d| *d = true),
                        alacritty_terminal::term::TermDamage::Partial(it) => for d in it { if d.line < nrows { dirty[d.line] = true; } },
                    }
                }
                term.reset_damage();
                if full { cache.0 = Some(key); cache.1.clear(); }
                cache.1.resize_with(nrows, || None);
                let g = term.grid();
                for l in 0..nrows {
                    if dirty[l] || cache.1[l].is_none() {
                        let line = l as i32 - offset as i32;
                        cache.1[l] = Some(build_row(g, line, ncols, &th, sel, hover, cw, window));
                        ROWS_BUILT.fetch_add(1, Ordering::Relaxed);
                    }
                }
                drop(term);
                for (l, row) in cache.1.iter().enumerate() {
                    let Some(row) = row else { continue };
                    let y = origin.y + px(l as f32 * lh);
                    for &(a, b, col) in &row.bgs {
                        window.paint_quad(fill(Bounds::new(point(origin.x + px(a as f32 * cw), y), size(px((b - a) as f32 * cw), px(lh))), col));
                    }
                    if let Some(t) = &row.text { let _ = t.paint(point(origin.x, y), px(lh), TextAlign::Left, None, window, cx); }
                    for (col, t) in &row.wide {
                        let w = f32::from(t.width).min(2.0 * cw);
                        let _ = t.paint(point(origin.x + px(*col as f32 * cw + (2.0 * cw - w) / 2.0), y), px(lh), TextAlign::Left, None, window, cx);
                    }
                    for &(col, ch, color) in &row.boxes { paint_box(window, ch, origin.x + px(col as f32 * cw), origin.x + px((col + 1) as f32 * cw), y, lh, color); }
                }
                // scrollbar (only when there is scrollback)
                if history > 0 {
                    let track_h = f32::from(bounds.size.height) - 8.0;
                    let total = (history + nrows) as f32;
                    let thumb_h = (nrows as f32 / total * track_h).max(24.0);
                    let top = (history - offset) as f32 / history as f32 * (track_h - thumb_h);
                    let x = bounds.origin.x + bounds.size.width - px(SCROLLBAR_W);
                    let alpha = if offset > 0 { 0.75 } else { 0.35 };
                    window.paint_quad(fill(Bounds::new(point(x + px(1.), bounds.origin.y + px(4. + top)), size(px(SCROLLBAR_W - 3.), px(thumb_h))), c(th.overlay1).opacity(alpha)).corner_radii(px(3.)));
                }
            },
        )
        .size_full()
    }
}

fn paint_box(window: &mut Window, ch: char, x: Pixels, x_next: Pixels, y: Pixels, lh: f32, color: Hsla) {
    // snap the cell to whole pixels; width from the next cell's edge so neighbours tile without gaps
    let (x0, x1, y) = (f32::from(x).round(), f32::from(x_next).round(), f32::from(y).round());
    let Some(prims) = boxdraw::prims(ch, x1 - x0, lh) else { return };
    for (rx, ry, w, h, a) in boxdraw::to_rects(&prims, lh) {
        window.paint_quad(fill(Bounds::new(point(px(x0 + rx), px(y + ry)), size(px(w), px(h))), color.opacity(a)));
    }
}

#[allow(clippy::too_many_arguments)]
fn build_row(g: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>, line: i32, ncols: usize, th: &Theme,
             sel: Option<(BPoint, BPoint)>, hover: Option<(i32, usize, usize)>, cw: f32, window: &mut Window) -> RowPaint {
    let row = &g[Line(line)];
    let base = c(th.bg);
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut last_style: Option<Style> = None;
    let mut wide_raw: Vec<(usize, String, Style)> = Vec::new();
    let mut boxes = Vec::new();
    let mut bgs = Vec::new();
    let mut bg_start = 0usize;
    let mut bg_cur: Option<Hsla> = None;
    let mut push = |text: &mut String, runs: &mut Vec<TextRun>, ch: char, st: &Style| {
        let len = ch.len_utf8();
        text.push(ch);
        if last_style.as_ref() == Some(st) { runs.last_mut().unwrap().len += len; } else { runs.push(text_run(len, st)); last_style = Some(st.clone()); }
    };
    for col in 0..=ncols {
        let mut bg = base;
        if col < ncols {
            let cell = &row[Column(col)];
            let mut fg = term_color(th, cell.fg, true);
            bg = term_color(th, cell.bg, false);
            if cell.flags.contains(Flags::DIM) { fg = fg.opacity(0.65); }
            if cell.flags.contains(Flags::INVERSE) { std::mem::swap(&mut fg, &mut bg); }
            if in_sel(sel, (line, col)) { bg = c(th.selection); }
            let link = cell.hyperlink().is_some();
            let hovered = hover.is_some_and(|(l, a, b)| l == line && col >= a && col < b);
            let ucolor = cell.underline_color().map(|u| term_color(th, u, true)).unwrap_or(fg);
            let underline = if cell.flags.intersects(Flags::ALL_UNDERLINES) || hovered {
                Some(UnderlineStyle { thickness: px(if cell.flags.contains(Flags::DOUBLE_UNDERLINE) { 2. } else { 1. }), color: Some(ucolor), wavy: cell.flags.contains(Flags::UNDERCURL) })
            } else if link {
                Some(UnderlineStyle { thickness: px(1.), color: Some(ucolor.opacity(0.7)), wavy: false })
            } else { None };
            let st = Style { fg, bold: cell.flags.contains(Flags::BOLD), italic: cell.flags.contains(Flags::ITALIC), underline, strike: cell.flags.contains(Flags::STRIKEOUT) };
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            } else if cell.flags.contains(Flags::WIDE_CHAR) {
                let mut gs = String::new();
                gs.push(cell.c);
                if let Some(z) = cell.zerowidth() { gs.extend(z.iter()); }
                wide_raw.push((col, gs, st.clone()));
                push(&mut text, &mut runs, ' ', &st);
                push(&mut text, &mut runs, ' ', &st);
            } else if boxdraw::is_custom(cell.c) && boxdraw::prims(cell.c, cw, 16.0).is_some() && !cell.flags.contains(Flags::HIDDEN) {
                boxes.push((col, cell.c, fg));
                push(&mut text, &mut runs, ' ', &st);
            } else {
                let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' { ' ' } else { cell.c };
                push(&mut text, &mut runs, ch, &st);
            }
        }
        let bg_opt = (bg != base && col < ncols).then_some(bg);
        if col == ncols || bg_opt != bg_cur {
            if let Some(b) = bg_cur { bgs.push((bg_start, col, b)); }
            bg_cur = bg_opt;
            bg_start = col;
        }
    }
    let has_deco = runs.iter().any(|r| r.underline.is_some() || r.strikethrough.is_some());
    let shaped = (!text.trim_end().is_empty() || has_deco)
        .then(|| window.text_system().shape_line(text.into(), px(font_size()), &runs, Some(px(cw))));
    let wide = wide_raw.into_iter().map(|(col, gs, st)| (col, window.text_system().shape_line(gs.clone().into(), px(font_size()), &[text_run(gs.len(), &st)], None))).collect();
    RowPaint { bgs, text: shaped, wide, boxes }
}

// ---------------------------------------------------------------- cursor + suggestion layer
pub struct CursorLayer { session: Option<Arc<Session>>, st: Rc<RefCell<PaintState>> }

impl Render for CursorLayer {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.clone();
        let st = self.st.clone();
        let th = cur();
        canvas(|_, _, _| {}, move |_bounds, _, window, cx| {
            CURSOR_PAINTS.fetch_add(1, Ordering::Relaxed);
            let Some(s) = session else { return };
            let st = st.borrow();
            let (cw, origin, lh) = (st.cell_w, st.origin, line_h());
            let t = s.term.lock().unwrap();
            let g = t.grid();
            if g.display_offset() != 0 { return; }
            let cursor = g.cursor.point;
            let (ncols, nrows) = (g.columns(), g.screen_lines());
            // suggestion: grey text from the cursor, wrapping onto following rows
            if let Some(sug) = &st.suggestion {
                let (mut col, mut row) = (cursor.column.0, cursor.line.0.max(0) as usize);
                let mut chunk = String::new();
                let mut chunk_start = col;
                let mut pieces: Vec<(usize, usize, String)> = Vec::new();
                for ch in sug.chars() {
                    if col >= ncols { pieces.push((row, chunk_start, std::mem::take(&mut chunk))); row += 1; col = 0; chunk_start = 0; if row >= nrows { break; } }
                    chunk.push(ch);
                    col += 1;
                }
                if row < nrows && !chunk.is_empty() { pieces.push((row, chunk_start, chunk)); }
                for (r, start, text) in pieces {
                    if text.is_empty() { continue; }
                    let sh = window.text_system().shape_line(text.clone().into(), px(font_size()), &[plain_run(text.len(), c(th.suggestion))], Some(px(cw)));
                    let _ = sh.paint(point(origin.x + px(start as f32 * cw), origin.y + px(r as f32 * lh)), px(lh), TextAlign::Left, None, window, cx);
                }
            }
            if !t.mode().contains(TermMode::SHOW_CURSOR) || cursor.line.0 < 0 { return; }
            let x = origin.x + px(cursor.column.0 as f32 * cw);
            let y = origin.y + px(cursor.line.0 as f32 * lh);
            let cell = &g[cursor.line][cursor.column];
            let w = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 * cw } else { cw };
            let b = Bounds::new(point(x, y + px(1.)), size(px(w), px(lh - 2.)));
            let style = config::get().terminal.cursor_style.clone();
            if st.focused && style != "block" {
                if st.blink_on {
                    let bar = if style == "bar" { Bounds::new(b.origin, size(px(2.), b.size.height)) }
                              else { Bounds::new(point(x, y + px(lh - 3.)), size(px(w), px(2.))) };
                    window.paint_quad(fill(bar, c(th.cursor)));
                }
            } else if st.focused {
                if st.blink_on {
                    window.paint_quad(fill(b, c(th.cursor)));
                    let ch = if cell.c != ' ' && cell.c != '\0' { Some(cell.c) } else { st.suggestion.as_ref().and_then(|s| s.chars().next()) };
                    if let Some(ch) = ch {
                        let sh = window.text_system().shape_line(ch.to_string().into(), px(font_size()), &[plain_run(ch.len_utf8(), c(th.cursor_text))], Some(px(w)));
                        let _ = sh.paint(point(x, y), px(lh), TextAlign::Left, None, window, cx);
                    }
                }
            } else {
                window.paint_quad(gpui::outline(b, c(th.overlay1), gpui::BorderStyle::Solid));
            }
        })
        .size_full()
    }
}

// ---------------------------------------------------------------- pane
pub struct TermView {
    pub title: SharedString,
    pub spec: Spec,
    pub session: Option<Arc<Session>>,
    pub error: Option<String>,
    pub focus: FocusHandle,
    st: Rc<RefCell<PaintState>>,
    grid: Entity<GridLayer>,
    cursor: Entity<CursorLayer>,
    selecting: bool,
    scroll_drag: bool,
    pub history: Arc<Mutex<History>>,
    tracker: InputTracker,
    last_activity: Instant,
    blink_running: bool,
    last_mouse_cell: Option<(usize, usize)>,
    mouse_down_btn: Option<u8>,
    // agent detection
    manifests: Arc<Vec<Manifest>>,
    last_command: Option<String>,
    pub agent: Option<(usize, AgentState)>,
    agent_seen: Instant,
    last_detect: Instant,
    detect_scheduled: bool,
    pub pending_paste: Option<String>,
    bell_until: Option<Instant>,
    _subs: Vec<Subscription>,
}

/// Snapshot of what the input line looks like right now.
struct LineState { before_cursor: String, at_eol: bool, prompt_col: Option<usize>, alt_screen: bool }

impl TermView {
    pub fn new(title: &str, spec: Spec, session: anyhow::Result<Arc<Session>>, history: Arc<Mutex<History>>,
               manifests: Arc<Vec<Manifest>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (session, error) = match session { Ok(s) => (Some(s), None), Err(e) => (None, Some(format!("{e:#}"))) };
        let st = Rc::new(RefCell::new(PaintState { cell_w: 8.0, blink_on: true, ..Default::default() }));
        let grid = cx.new(|_| GridLayer { session: session.clone(), st: st.clone(), cache: Default::default() });
        let cursor = cx.new(|_| CursorLayer { session: session.clone(), st: st.clone() });
        if let Some(s) = session.clone() {
            // event-driven: wake only when the parser has new content; coalesce to <= ~120 Hz
            cx.spawn(async move |this, cx| {
                let min_gap = Duration::from_millis(8);
                let mut last = Instant::now() - min_gap;
                while s.wake.recv_async().await.is_ok() {
                    let el = last.elapsed();
                    if el < min_gap { cx.background_executor().timer(min_gap - el).await; }
                    while s.wake.try_recv().is_ok() {}
                    last = Instant::now();
                    if this.update(cx, |v, cx| v.on_output(cx)).is_err() { return; }
                }
                let _ = this.update(cx, |v, cx| v.on_output(cx));
            }).detach();
        }
        let focus = cx.focus_handle();
        let subs = vec![
            cx.on_focus(&focus, window, |v, w, cx| { v.st.borrow_mut().focused = true; v.kick_blink(w, cx); cx.notify(); }),
            cx.on_blur(&focus, window, |v, _w, cx| { v.st.borrow_mut().focused = false; v.st.borrow_mut().blink_on = true; v.cursor.update(cx, |_, cx| cx.notify()); cx.notify(); }),
        ];
        Self {
            title: title.to_string().into(), spec, session, error, focus, st, grid, cursor,
            selecting: false, scroll_drag: false, history, tracker: InputTracker::new(),
            last_activity: Instant::now(), blink_running: false, last_mouse_cell: None, mouse_down_btn: None,
            manifests, last_command: None, agent: None, agent_seen: Instant::now(), last_detect: Instant::now() - Duration::from_secs(5),
            detect_scheduled: false, pending_paste: None, bell_until: None, _subs: subs,
        }
    }

    pub fn status(&self, t: &Theme) -> (&'static str, Hsla) {
        match &self.session {
            Some(s) if s.conn_error.lock().unwrap().is_some() => ("连接失败", c(t.error)),
            Some(s) if s.exited.load(Ordering::Acquire) => ("已断开", c(t.overlay0)),
            Some(s) if s.connected.load(Ordering::Acquire) => ("已连接", c(t.success)),
            Some(_) => ("连接中", c(t.warning)),
            None => ("连接失败", c(t.error)),
        }
    }

    fn repaint_grid(&self, cx: &mut Context<Self>) { self.grid.update(cx, |_, cx| cx.notify()); self.cursor.update(cx, |_, cx| cx.notify()); }

    /// Called (coalesced) when the backend produced output.
    fn on_output(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = &self.session {
            if let Some(seed) = s.history_seed.lock().unwrap().take() { if config::get().suggest.remote_history { self.history.lock().unwrap().extend_from_bash_history(&seed); } }
            if s.exited.load(Ordering::Acquire) || s.conn_error.lock().unwrap().is_some() { cx.notify(); }
        }
        if let Some(s) = &self.session {
            if s.shared.bell.swap(false, Ordering::AcqRel) && config::get().terminal.bell == "visual" {
                self.bell_until = Some(Instant::now() + Duration::from_millis(180));
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    let _ = this.update(cx, |v, cx| { v.bell_until = None; cx.notify(); });
                }).detach();
            }
        }
        self.last_activity = Instant::now();
        self.st.borrow_mut().blink_on = true;
        self.st.borrow_mut().suggestion = self.compute_suggestion();
        self.repaint_grid(cx);
        self.schedule_detect(cx);
        // connection state changes show in the header
        if self.status(&cur()).0 != "已连接" { cx.notify(); }
    }

    // ---------------- blink: only while focused, window active and recently used; repaints the cursor layer only
    fn kick_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.last_activity = Instant::now();
        self.st.borrow_mut().blink_on = true;
        if self.blink_running || !config::get().terminal.cursor_blink { return; }
        self.blink_running = true;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(530)).await;
                let keep = this.update_in(cx, |v, window, cx| {
                    let idle = v.last_activity.elapsed() > Duration::from_secs(10);
                    let focused = v.st.borrow().focused && window.is_window_active();
                    if idle || !focused {
                        v.st.borrow_mut().blink_on = true;
                        v.blink_running = false;
                        v.cursor.update(cx, |_, cx| cx.notify());
                        return false;
                    }
                    let on = !v.st.borrow().blink_on;
                    v.st.borrow_mut().blink_on = on;
                    v.cursor.update(cx, |_, cx| cx.notify());
                    true
                });
                if !matches!(keep, Ok(true)) { break; }
            }
        }).detach();
    }

    // ---------------- agent detection (herdr manifests), throttled to ~2/s while output flows
    fn schedule_detect(&mut self, cx: &mut Context<Self>) {
        if self.last_detect.elapsed() >= Duration::from_millis(400) { self.detect_agent(cx); }
        if !self.detect_scheduled {
            self.detect_scheduled = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = this.update(cx, |v, cx| { v.detect_scheduled = false; v.detect_agent(cx); });
            }).detach();
        }
    }

    fn detect_agent(&mut self, cx: &mut Context<Self>) {
        self.last_detect = Instant::now();
        let Some(s) = &self.session else { return };
        let fg = s.foreground_process();
        let title = s.title();
        let progress = s.shared.progress.lock().unwrap().clone();
        let is_shell = fg.as_deref().is_some_and(|p| matches!(p, "bash" | "zsh" | "sh" | "fish" | "dash"));
        let idx = if is_shell { None } else {
            // local: fg process is authoritative; ssh: last submitted command / title
            let cmd = if fg.is_some() { None } else { self.last_command.as_deref() };
            agent::identify(&self.manifests, fg.as_deref(), cmd, &title).filter(|i| {
                let a = &config::get().agents;
                match self.manifests[*i].id.as_str() { "claude" => a.detect_claude, "codex" => a.detect_codex, "grok" => a.detect_grok, _ => true }
            })
        };
        let new = idx.map(|i| {
            let screen = s.screen_text();
            let d = self.manifests[i].detect(DetectionInput { screen: &screen, osc_title: &title, osc_progress: &progress });
            let prev = self.agent.filter(|(p, _)| *p == i).map(|(_, st)| st);
            let state = if d.skip_state_update || d.state == AgentState::Unknown { prev.unwrap_or(AgentState::Unknown) } else { d.state };
            (i, state)
        });
        let new = match new {
            Some((i, st)) if st != AgentState::Unknown => { self.agent_seen = Instant::now(); Some((i, st)) }
            Some((i, st)) => if self.agent_seen.elapsed() < Duration::from_secs(10) { Some((i, st)) } else { None },
            None => None,
        };
        if new != self.agent { self.agent = new; cx.notify(); }
    }

    pub fn agent_label(&self) -> Option<(String, AgentState)> {
        self.agent.map(|(i, st)| (agent::display_name(&self.manifests[i].id).to_string(), st))
    }

    // ---------------- input line / suggestions
    fn line_state(&self) -> Option<LineState> {
        let s = self.session.as_ref()?;
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let cur = g.cursor.point;
        let ncols = g.columns();
        let mut first = cur.line.0;
        while first > g.topmost_line().0 && g[Line(first - 1)][Column(ncols - 1)].flags.contains(Flags::WRAPLINE) { first -= 1; }
        let mark = *s.prompt_mark.lock().unwrap();
        let mut before = String::new();
        let mut prompt_col = None;
        for l in first..=cur.line.0 {
            let row = &g[Line(l)];
            let end = if l == cur.line.0 { cur.column.0.min(ncols) } else { ncols };
            for col in 0..end {
                if mark == Some((l, col)) { prompt_col = Some(before.chars().count()); }
                let cell = &row[Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { continue; }
                before.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            if l == cur.line.0 && mark == Some((l, end)) { prompt_col = Some(before.chars().count()); }
        }
        let row = &g[cur.line];
        let at_eol = (cur.column.0..ncols).all(|col| { let ch = row[Column(col)].c; ch == ' ' || ch == '\0' });
        Some(LineState { before_cursor: before, at_eol, prompt_col, alt_screen: t.mode().contains(TermMode::ALT_SCREEN) })
    }

    fn compute_suggestion(&self) -> Option<String> {
        if !config::get().suggest.enabled { return None; }
        let ls = self.line_state()?;
        if ls.alt_screen || !ls.at_eol { return None; }
        let (input, _) = self.tracker.current_input(&ls.before_cursor, ls.prompt_col)?;
        self.history.lock().unwrap().suggest(&input)
    }

    fn mode(&self) -> TermMode { self.session.as_ref().map(|s| *s.term.lock().unwrap().mode()).unwrap_or(TermMode::empty()) }

    fn scroll_to_bottom(&self, cx: &mut Context<Self>) {
        if let Some(s) = &self.session {
            let mut t = s.term.lock().unwrap();
            if t.grid().display_offset() != 0 { t.scroll_display(Scroll::Bottom); drop(t); self.repaint_grid(cx); }
        }
    }

    // ---------------- clipboard
    pub fn selection_text(&self) -> Option<String> {
        let (a, b) = self.st.borrow().sel?;
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        let sess = self.session.as_ref()?;
        let t = sess.term.lock().unwrap();
        let g = t.grid();
        let ncols = g.columns();
        let mut out = String::new();
        for l in s.0..=e.0 {
            if l < g.topmost_line().0 || l > g.bottommost_line().0 { continue; }
            let row = &g[Line(l)];
            let c0 = if l == s.0 { s.1 } else { 0 };
            let c1 = if l == e.0 { e.1.min(ncols - 1) } else { ncols - 1 };
            let mut line = String::new();
            for col in c0..=c1 {
                let cell = &row[Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { continue; }
                line.push(if cell.c == '\0' { ' ' } else { cell.c });
                if let Some(z) = cell.zerowidth() { line.extend(z.iter()); }
            }
            let wrapped = row[Column(ncols - 1)].flags.contains(Flags::WRAPLINE);
            if wrapped && l != e.0 { out.push_str(&line); } else { out.push_str(line.trim_end()); if l != e.0 { out.push('\n'); } }
        }
        Some(out)
    }

    pub fn copy(&self, cx: &mut Context<Self>) {
        if let Some(t) = self.selection_text().filter(|t| !t.is_empty()) { cx.write_to_clipboard(ClipboardItem::new_string(t)); }
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else { return };
        // multi-line paste outside bracketed-paste mode would execute each line: confirm first
        if config::get().terminal.paste_confirm && text.trim_end().contains('\n') && !self.mode().contains(TermMode::BRACKETED_PASTE) {
            self.pending_paste = Some(text);
            cx.notify();
            return;
        }
        self.paste_text(text, cx);
    }

    pub fn paste_text(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let bytes = paste_bytes(&text, self.mode().contains(TermMode::BRACKETED_PASTE));
        self.tracker.key(Key::Other); // pasted text can't be tracked reliably
        self.scroll_to_bottom(cx);
        s.write(bytes);
    }

    // ---------------- keyboard
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let k = &ev.keystroke;
        let m = &k.modifiers;
        self.kick_blink(window, cx);
        // app shortcuts
        if m.control && m.shift && !m.alt {
            match k.key.to_ascii_lowercase().as_str() {
                "c" => { self.copy(cx); cx.stop_propagation(); return; }
                "v" => { self.paste(cx); cx.stop_propagation(); return; }
                _ => {}
            }
        }
        if m.shift && !m.control && k.key == "insert" { self.paste(cx); cx.stop_propagation(); return; }
        if m.shift && !m.control && (k.key == "pageup" || k.key == "pagedown") && !self.mode().contains(TermMode::ALT_SCREEN) {
            let rows = s.size().rows as i32 - 1;
            s.term.lock().unwrap().scroll_display(Scroll::Delta(if k.key == "pageup" { rows } else { -rows }));
            self.repaint_grid(cx);
            cx.stop_propagation();
            return;
        }
        let plain = !m.control && !m.alt && !m.platform && !m.shift;
        let accept = config::get().suggest.accept.clone();
        let key_ok = match accept.as_str() { "right" => k.key == "right", "tab" => k.key == "tab", _ => k.key == "right" || k.key == "tab" };
        if plain && key_ok {
            if let Some(sug) = self.compute_suggestion() {
                s.write(sug.clone().into_bytes());
                self.tracker.key(Key::Text(&sug));
                self.st.borrow_mut().suggestion = None;
                self.scroll_to_bottom(cx);
                cx.stop_propagation();
                return;
            }
        }
        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        let Some(bytes) = key_to_bytes(k, app_cursor) else { return };
        match k.key.as_str() {
            "enter" => {
                if let Some(ls) = self.line_state() {
                    if !ls.alt_screen {
                        if let Some(cmd) = self.tracker.submitted_input(&ls.before_cursor, ls.prompt_col) {
                            self.history.lock().unwrap().add(&cmd);
                            // remember the command word for agent identification over SSH (not while an agent runs)
                            if self.agent.is_none() { self.last_command = Some(cmd); }
                        }
                    }
                }
                self.tracker.key(Key::Enter);
            }
            "backspace" => { self.tracker.key(Key::Backspace); }
            _ if m.control && k.key == "c" => { self.tracker.key(Key::Interrupt); }
            _ if m.control && k.key == "u" => { self.tracker.key(Key::KillLine); }
            _ => {
                let printable = !m.control && !m.alt && !m.platform && bytes.first().is_some_and(|b| *b >= 0x20 && *b != 0x7f);
                if printable { self.tracker.key(Key::Text(std::str::from_utf8(&bytes).unwrap_or(""))); } else { self.tracker.key(Key::Other); }
            }
        }
        if self.st.borrow().sel.is_some() { self.st.borrow_mut().sel = None; self.grid.update(cx, |_, cx| cx.notify()); }
        self.scroll_to_bottom(cx);
        s.write(bytes);
        cx.stop_propagation();
    }

    // ---------------- mouse
    fn cell_at(&self, p: gpui::Point<Pixels>) -> (usize, usize) {
        let st = self.st.borrow();
        let x = (f32::from(p.x - st.origin.x) / st.cell_w).max(0.0) as usize;
        let y = (f32::from(p.y - st.origin.y) / line_h()).max(0.0) as usize;
        let sz = self.session.as_ref().map(|s| s.size()).unwrap_or(Size { cols: 80, rows: 24 });
        (y.min(sz.rows.saturating_sub(1)), x.min(sz.cols.saturating_sub(1)))
    }
    fn offset(&self) -> usize { self.session.as_ref().map(|s| s.term.lock().unwrap().grid().display_offset()).unwrap_or(0) }
    fn to_buf(&self, (row, col): (usize, usize)) -> BPoint { (row as i32 - self.offset() as i32, col) }

    /// Whether mouse events go to the application (vim/htop) rather than local selection.
    fn mouse_reporting(&self, shift: bool) -> bool { !shift && config::get().terminal.mouse_reporting && self.mode().intersects(TermMode::MOUSE_MODE) }

    fn report_mouse(&mut self, button: u8, pressed: bool, cell: (usize, usize), mods: &gpui::Modifiers) {
        let Some(s) = &self.session else { return };
        let mode = self.mode();
        let mut b = button;
        if mods.shift { b += 4; }
        if mods.alt { b += 8; }
        if mods.control { b += 16; }
        s.write(encode_mouse(b, pressed, cell.0, cell.1, mode.contains(TermMode::SGR_MOUSE)));
    }

    fn in_scrollbar(&self, p: gpui::Point<Pixels>) -> bool {
        let b = self.st.borrow().bounds;
        p.x >= b.origin.x + b.size.width - px(SCROLLBAR_W + 2.) && p.x <= b.origin.x + b.size.width
    }

    fn scrollbar_jump(&mut self, p: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(s) = &self.session else { return };
        let b = self.st.borrow().bounds;
        let frac = (f32::from(p.y - b.origin.y) / f32::from(b.size.height)).clamp(0.0, 1.0);
        let mut t = s.term.lock().unwrap();
        let hist = t.grid().history_size();
        let target = ((1.0 - frac) * hist as f32).round() as i32;
        let cur_off = t.grid().display_offset() as i32;
        t.scroll_display(Scroll::Delta(target - cur_off));
        drop(t);
        self.repaint_grid(cx);
    }

    fn on_mouse_down(&mut self, btn: MouseButton, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.kick_blink(window, cx);
        if btn == MouseButton::Left && self.in_scrollbar(ev.position) && !self.mode().contains(TermMode::ALT_SCREEN) {
            self.scroll_drag = true;
            self.scrollbar_jump(ev.position, cx);
            return;
        }
        let cell = self.cell_at(ev.position);
        if self.mouse_reporting(ev.modifiers.shift) {
            let code = match btn { MouseButton::Left => 0, MouseButton::Middle => 1, MouseButton::Right => 2, _ => return };
            self.mouse_down_btn = Some(code);
            self.last_mouse_cell = Some(cell);
            self.report_mouse(code, true, cell, &ev.modifiers);
            return;
        }
        match btn {
            MouseButton::Left => {
                // Ctrl/Cmd+click opens OSC 8 hyperlinks
                if ev.modifiers.control || ev.modifiers.platform {
                    if let Some(uri) = self.link_at(cell) { cx.open_url(&uri); return; }
                }
                let p = self.to_buf(cell);
                let mut st = self.st.borrow_mut();
                st.sel = if ev.click_count >= 2 { Some(((p.0, 0), (p.0, usize::MAX >> 1))) } else { Some((p, p)) };
                drop(st);
                self.selecting = ev.click_count < 2;
                self.grid.update(cx, |_, cx| cx.notify());
            }
            MouseButton::Middle => self.paste(cx),
            _ => {}
        }
    }

    fn link_at(&self, (row, col): (usize, usize)) -> Option<String> {
        let s = self.session.as_ref()?;
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let line = row as i32 - g.display_offset() as i32;
        g[Line(line)][Column(col)].hyperlink().map(|h| h.uri().to_string())
    }

    fn link_span(&self, (row, col): (usize, usize)) -> Option<(i32, usize, usize)> {
        let s = self.session.as_ref()?;
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let line = row as i32 - g.display_offset() as i32;
        let r = &g[Line(line)];
        let h = r[Column(col)].hyperlink()?;
        let same = |c: usize| r[Column(c)].hyperlink().is_some_and(|x| x == h);
        let mut a = col; while a > 0 && same(a - 1) { a -= 1; }
        let mut b = col + 1; while b < g.columns() && same(b) { b += 1; }
        Some((line, a, b))
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.scroll_drag && ev.pressed_button == Some(MouseButton::Left) { self.scrollbar_jump(ev.position, cx); return; }
        let cell = self.cell_at(ev.position);
        if self.mouse_reporting(ev.modifiers.shift) {
            let mode = self.mode();
            let report = match (self.mouse_down_btn, ev.pressed_button) {
                (Some(_), Some(_)) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
                _ => mode.contains(TermMode::MOUSE_MOTION),
            };
            if report && self.last_mouse_cell != Some(cell) {
                self.last_mouse_cell = Some(cell);
                let b = self.mouse_down_btn.filter(|_| ev.pressed_button.is_some()).unwrap_or(3) + 32;
                self.report_mouse(b, true, cell, &ev.modifiers);
            }
            return;
        }
        if self.selecting && ev.pressed_button == Some(MouseButton::Left) {
            let p = self.to_buf(cell);
            let changed = { let mut st = self.st.borrow_mut(); match st.sel { Some((a, b)) if b != p => { st.sel = Some((a, p)); true } _ => false } };
            if changed { self.grid.update(cx, |_, cx| cx.notify()); }
            return;
        }
        // hover underline for links while Ctrl is held
        let hover = if ev.modifiers.control || ev.modifiers.platform { self.link_span(cell) } else { None };
        if hover != self.st.borrow().hover_link { self.st.borrow_mut().hover_link = hover; self.grid.update(cx, |_, cx| cx.notify()); }
    }

    fn on_mouse_up(&mut self, btn: MouseButton, ev: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        self.scroll_drag = false;
        if self.mouse_reporting(ev.modifiers.shift) && self.mouse_down_btn.is_some() {
            let cell = self.cell_at(ev.position);
            let code = match btn { MouseButton::Left => 0, MouseButton::Middle => 1, MouseButton::Right => 2, _ => 0 };
            self.report_mouse(code, false, cell, &ev.modifiers);
            self.mouse_down_btn = None;
            return;
        }
        if btn == MouseButton::Left {
            self.selecting = false;
            let empty = matches!(self.st.borrow().sel, Some((a, b)) if a == b);
            if empty { self.st.borrow_mut().sel = None; self.grid.update(cx, |_, cx| cx.notify()); }
            else if self.st.borrow().sel.is_some() {
                // X11 convention: selecting also fills the primary selection
                #[cfg(target_os = "linux")]
                if let Some(t) = self.selection_text() { cx.write_to_primary(ClipboardItem::new_string(t)); }
                if config::get().terminal.copy_on_select { self.copy(cx); }
            }
        }
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let lines = match ev.delta { ScrollDelta::Lines(p) => p.y * 3.0, ScrollDelta::Pixels(p) => f32::from(p.y) / line_h() };
        let n = lines.round() as i32;
        if n == 0 { return; }
        let mode = self.mode();
        if self.mouse_reporting(ev.modifiers.shift) {
            let cell = self.cell_at(ev.position);
            for _ in 0..n.abs() { self.report_mouse(if n > 0 { 64 } else { 65 }, true, cell, &ev.modifiers); }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let seq: &[u8] = match (n > 0, mode.contains(TermMode::APP_CURSOR)) { (true, true) => b"\x1bOA", (true, false) => b"\x1b[A", (false, true) => b"\x1bOB", (false, false) => b"\x1b[B" };
            s.write(seq.repeat(n.unsigned_abs() as usize));
        } else {
            s.term.lock().unwrap().scroll_display(Scroll::Delta(n));
            self.repaint_grid(cx);
        }
        cx.stop_propagation();
    }
}

/// Bytes to send for a paste. Bracketed paste wraps the text and strips any embedded
/// ESC so pasted content can't terminate the bracket early; otherwise newlines become CR.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let norm = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let clean: String = norm.chars().filter(|&c| c != '\x1b').collect();
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else { norm.into_bytes() }
}

/// xterm mouse encoding. `button`: 0/1/2 = L/M/R, 3 = release (X10), 64/65 = wheel, +32 = motion,
/// modifiers already added. Rows/cols are 0-based screen cells.
pub fn encode_mouse(button: u8, pressed: bool, row: usize, col: usize, sgr: bool) -> Vec<u8> {
    if sgr {
        format!("\x1b[<{};{};{}{}", button, col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
    } else {
        let b = if pressed { button } else { 3 + (button & !3) };
        let enc = |v: usize| (32 + 1 + v).min(255) as u8;
        vec![0x1b, b'[', b'M', 32 + b, enc(col), enc(row)]
    }
}

pub fn key_to_bytes(k: &gpui::Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let m = &k.modifiers;
    let arrow = |ch: char| Some(if app_cursor { format!("\x1bO{ch}") } else { format!("\x1b[{ch}") }.into_bytes());
    match k.key.as_str() {
        "enter" => return Some(b"\r".to_vec()),
        "backspace" => return Some(vec![0x7f]),
        "tab" => return Some(if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
        "escape" => return Some(vec![0x1b]),
        "up" => return arrow('A'), "down" => return arrow('B'),
        "right" => return arrow('C'), "left" => return arrow('D'),
        "home" => return Some(b"\x1b[H".to_vec()), "end" => return Some(b"\x1b[F".to_vec()),
        "pageup" => return Some(b"\x1b[5~".to_vec()), "pagedown" => return Some(b"\x1b[6~".to_vec()),
        "delete" => return Some(b"\x1b[3~".to_vec()), "insert" => return Some(b"\x1b[2~".to_vec()),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            let n: u8 = f[1..].parse().unwrap();
            let s = match n { 1 => "\x1bOP", 2 => "\x1bOQ", 3 => "\x1bOR", 4 => "\x1bOS", 5 => "\x1b[15~", 6 => "\x1b[17~", 7 => "\x1b[18~",
                8 => "\x1b[19~", 9 => "\x1b[20~", 10 => "\x1b[21~", 11 => "\x1b[23~", 12 => "\x1b[24~", _ => return None };
            return Some(s.as_bytes().to_vec());
        }
        _ => {}
    }
    if m.control && k.key.len() == 1 {
        let b = k.key.as_bytes()[0].to_ascii_lowercase();
        if b.is_ascii_lowercase() { return Some(vec![b - b'a' + 1]); }
        return match b { b'[' => Some(vec![0x1b]), b'\\' => Some(vec![0x1c]), b']' => Some(vec![0x1d]), b' ' | b'@' => Some(vec![0]), _ => None };
    }
    if m.control && k.key == "space" { return Some(vec![0]); }
    if m.platform { return None; }
    let text = k.key_char.clone().or_else(|| (k.key == "space").then(|| " ".to_string()))?;
    let mut out = Vec::new();
    if m.alt { out.push(0x1b); }
    out.extend_from_slice(text.as_bytes());
    Some(out)
}

impl Render for TermView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let focused = self.st.borrow().focused;
        let mut body = div().flex_1().size_full().relative()
            .child(self.grid.clone().cached(StyleRefinement::default().size_full()))
            .child(div().absolute().top_0().left_0().size_full().child(self.cursor.clone()));
        if let Some(e) = &self.error {
            body = body.child(div().absolute().top(px(16.)).left(px(16.)).right(px(16.)).p_3().rounded_md()
                .bg(c(th.mantle)).border_1().border_color(c(th.error)).text_color(c(th.error)).text_sm().child(format!("连接失败：{e}")));
        }
        if let Some(text) = &self.pending_paste {
            let lines = text.trim_end().lines().count();
            let preview: String = text.lines().next().unwrap_or("").chars().take(60).collect();
            let btn = |id: &'static str, label: &'static str, primary: bool| div().id(id).px_3().py(px(4.)).rounded_md().cursor_pointer().text_size(px(12.))
                .bg(if primary { c(th.accent) } else { c(th.surface0) }).text_color(if primary { c(th.base) } else { c(th.text) }).child(label);
            body = body.child(div().absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.warning))
                .flex().items_center().gap_3().text_size(px(12.)).text_color(c(th.text))
                .child(div().flex_1().flex().flex_col().gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("粘贴 {lines} 行文本？每一行都可能被立即执行")))
                    .child(div().text_color(c(th.overlay1)).child(format!("{preview} …"))))
                .child(btn("paste-cancel", "取消", false).on_click(cx.listener(|v, _, _w, cx| { v.pending_paste = None; cx.notify(); })))
                .child(btn("paste-ok", "粘贴", true).on_click(cx.listener(|v, _, _w, cx| { if let Some(t) = v.pending_paste.take() { v.paste_text(t, cx); } cx.notify(); }))));
        }
        if self.bell_until.is_some_and(|t| t > Instant::now()) {
            body = body.child(div().absolute().top_0().left_0().size_full().border_2().border_color(c(th.warning)).bg(c(th.warning).opacity(0.06)));
        }
        let (status, scolor) = self.status(&th);
        let mut header = div().h(px(26.)).px_3().flex().items_center().gap_2().bg(c(th.mantle)).text_size(px(11.5))
            .text_color(if focused { c(th.text) } else { c(th.overlay1) })
            .child(div().size(px(7.)).rounded_full().bg(scolor))
            .child(self.title.clone())
            .child(div().text_color(c(th.overlay0)).child(status));
        if let Some((name, st)) = self.agent_label() {
            let col = match st { AgentState::Working => c(th.warning), AgentState::Blocked => c(th.error), AgentState::Idle => c(th.success), _ => c(th.overlay1) };
            header = header.child(div().px_2().rounded_md().bg(col.opacity(0.18)).text_color(col).child(format!("{name} · {}", st.label_zh())));
        }
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, ev, w, cx| v.on_mouse_down(MouseButton::Left, ev, w, cx)))
            .on_mouse_down(MouseButton::Middle, cx.listener(|v, ev, w, cx| v.on_mouse_down(MouseButton::Middle, ev, w, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|v, ev, w, cx| v.on_mouse_down(MouseButton::Right, ev, w, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, ev, w, cx| v.on_mouse_up(MouseButton::Left, ev, w, cx)))
            .on_mouse_up(MouseButton::Middle, cx.listener(|v, ev, w, cx| v.on_mouse_up(MouseButton::Middle, ev, w, cx)))
            .on_mouse_up(MouseButton::Right, cx.listener(|v, ev, w, cx| v.on_mouse_up(MouseButton::Right, ev, w, cx)))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .flex_1().size_full().flex().flex_col().bg(c(th.bg)).overflow_hidden()
            .border_t_2().border_color(if focused { c(th.accent) } else { c(th.bg) })
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bracketed_paste_strips_esc() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb");
        assert_eq!(paste_bytes("x\x1b[201~rm -rf /\n", true), b"\x1b[200~x[201~rm -rf /\r\x1b[201~");
    }
    #[test]
    fn mouse_encoding() {
        assert_eq!(encode_mouse(0, true, 4, 9, true), b"\x1b[<0;10;5M");
        assert_eq!(encode_mouse(0, false, 4, 9, true), b"\x1b[<0;10;5m");
        assert_eq!(encode_mouse(0, true, 0, 0, false), vec![0x1b, b'[', b'M', 32, 33, 33]);
        assert_eq!(encode_mouse(0, false, 0, 0, false)[3], 35);
        assert_eq!(encode_mouse(64, true, 1, 1, true), b"\x1b[<64;2;2M");
    }
}
