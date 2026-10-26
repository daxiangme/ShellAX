//! One terminal pane: input handling, selection/clipboard, mouse reporting, scrollback,
//! autosuggestion and agent detection. Painting is split into two child entities so that
//! a cursor blink repaints only the cursor layer while the grid is replayed from GPUI's
//! view cache (`Entity::cached`).
use gpui::AnimationExt as _;
use ssh_term_proto::i18n::{t, tf};
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
use ssh_term_proto::policy::{self, Effect, Risk};
use ssh_term_proto::ssh::SshTarget;
use ssh_term_proto::suggest::{History, InputTracker, Key};
use ssh_term_proto::term::{Session, Size};

use crate::theme::{c, cur, scheme, term_color, Scheme, Theme};
use crate::{config, font_size, line_h};

const SCROLLBAR_W: f32 = 8.0;

/// Pixels the scrollbar takes from the cell grid.
///
/// The bar is painted in the rightmost `SCROLLBAR_W` px. Right padding already
/// covers that strip when it is at least that wide, so those columns stay in
/// the grid. With thinner padding, only the uncovered part is reserved, and
/// only while a bar is drawn (the active grid has scrollback; the alternate
/// screen's grid does not).
fn scrollbar_reserve(pad: f32, history: usize) -> f32 {
    if history == 0 { 0.0 } else { (SCROLLBAR_W - pad).max(0.0) }
}

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
#[allow(clippy::large_enum_variant)] // one per pane
pub enum Spec { Local, Ssh(SshTarget) }
impl Spec {
    pub fn history_key(&self) -> String { match self { Spec::Local => "local".into(), Spec::Ssh(t) => t.label() } }
}

type RowCache = (Option<CacheKey>, Vec<Option<RowPaint>>);

/// Buffer coordinate: grid line (negative = scrollback) and column.
type BPoint = (i32, usize);

/// State shared by the pane and its two paint layers.
#[derive(Default)]
pub struct PaintState {
    /// Debounces / freezes PTY resizes during animations and drags.
    pub gate: crate::motion::SizeGate,
    pub cell_w: f32,
    pub origin: gpui::Point<Pixels>,
    pub bounds: Bounds<Pixels>,
    pub focused: bool,
    pub blink_on: bool,
    pub suggestion: Option<String>,
    /// IME composition text (pre-edit), drawn at the cursor and underlined until committed.
    pub preedit: Option<String>,
    pub sel: Option<(BPoint, BPoint)>,
    /// rectangular (Alt+drag) selection
    pub sel_block: bool,
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
struct CacheKey { cols: usize, rows: usize, theme: SharedString, cw: u32, gen: u64, offset: usize, sel: Option<(BPoint, BPoint)>, block: bool, hover: Option<(i32, usize, usize)> }

pub struct GridLayer {
    session: Option<Arc<Session>>,
    st: Rc<RefCell<PaintState>>,
    cache: Rc<RefCell<RowCache>>,
    /// Decoded inline images by placement id.
    images: Rc<RefCell<std::collections::HashMap<u64, Option<Arc<gpui::RenderImage>>>>>,
}

/// Inline image → gpui frame (BGRA).
fn render_image(img: &ssh_term_proto::graphics::Image) -> Option<Arc<gpui::RenderImage>> {
    use ssh_term_proto::graphics::Pixels as P;
    let mut rgba = match &img.pixels {
        P::Rgba { w, h, data } => image::RgbaImage::from_raw(*w, *h, data.clone())?,
        P::Png(b) => image::load_from_memory_with_format(b, image::ImageFormat::Png).ok()?.to_rgba8(),
    };
    for p in rgba.pixels_mut() { p.0.swap(0, 2); }
    Some(Arc::new(gpui::RenderImage::new(vec![image::Frame::new(rgba)])))
}

/// One char per column (wide-char spacers become spaces) so string offsets equal columns.
fn row_chars(g: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>, line: i32) -> String {
    let r = &g[Line(line)];
    (0..g.columns()).map(|c| { let cell = &r[Column(c)]; if cell.c == '\0' || cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { ' ' } else { cell.c } }).collect()
}

fn in_sel(sel: Option<(BPoint, BPoint)>, block: bool, p: BPoint) -> bool { ssh_term_proto::term::in_selection(sel, p, block) }

impl Render for GridLayer {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.clone();
        let st = self.st.clone();
        let st2 = self.st.clone();
        let cache = self.cache.clone();
        let images = self.images.clone();
        let th = cur();
        let sc = scheme();
        canvas(
            move |bounds, window, _cx| {
                let ts = window.text_system();
                let fid = ts.resolve_font(&mono_font(false, false));
                let cw = ts.advance(fid, px(font_size()), 'M').map(|a| f32::from(a.width)).unwrap_or(8.0);
                let mh = f32::from(ts.ascent(fid, px(font_size()))) + f32::from(ts.descent(fid, px(font_size()))).abs();
                if mh.is_finite() && mh > 0.0 { crate::FONT_METRIC_H.store(mh.to_bits(), Ordering::Relaxed); }
                let pad = crate::term_pad();
                // Drop the term lock before resize(); that path locks the term again.
                let history = session.as_ref().map(|s| s.term.lock().unwrap().grid().history_size()).unwrap_or(0);
                let reserve = scrollbar_reserve(pad, history);
                let mut s = st.borrow_mut();
                s.cell_w = cw;
                s.bounds = bounds;
                s.origin = bounds.origin + point(px(pad), px(pad));
                let cols = ((f32::from(bounds.size.width) - 2.0 * pad - reserve) / cw).floor().max(10.0) as usize;
                let rows = ((f32::from(bounds.size.height) - 2.0 * pad) / line_h()).floor().max(3.0) as usize;
                if let Some(sess) = &session {
                    // one resize after animations / drags settle, not one per frame
                    let cur = sess.size();
                    if let Some((c2, r2)) = s.gate.poll((cols, rows), (cur.cols, cur.rows), crate::motion::layout_busy(), std::time::Instant::now()) {
                        sess.resize(Size { cols: c2, rows: r2 });
                    }
                    if s.gate.waiting() { window.request_animation_frame(); }
                    *sess.cell_px.lock().unwrap() = (cw * window.scale_factor(), line_h() * window.scale_factor());
                }
                session
            },
            move |bounds, session, window, cx| {
                GRID_PAINTS.fetch_add(1, Ordering::Relaxed);
                let lh = line_h();
                // appearance.opacity: the grid background carries the window alpha
                let base = c(sc.bg).opacity(config::get().appearance.opacity.clamp(0.5, 1.0));
                window.paint_quad(fill(bounds, base));
                let Some(s) = session else { return };
                let st = st2.borrow();
                let (cw, origin, sel, block, hover) = (st.cell_w, st.origin, st.sel, st.sel_block, st.hover_link);
                drop(st);
                let mut term = s.term.lock().unwrap();
                let (ncols, nrows, offset, history) = { let g = term.grid(); (g.columns(), g.screen_lines(), g.display_offset(), g.history_size()) };
                let key = CacheKey { cols: ncols, rows: nrows, theme: format!("{}+{}", th.name, sc.name).into(), cw: cw.to_bits(), gen: config::generation(), offset, sel, block, hover };
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
                for (l, &d) in dirty.iter().enumerate().take(nrows) {
                    if d || cache.1[l].is_none() {
                        let line = l as i32 - offset as i32;
                        cache.1[l] = Some(build_row(g, line, ncols, &th, &sc, sel, block, hover, cw, window));
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
                // Inline Sixel/Kitty images at their anchored rows (natural pixel size).
                let placed = s.images.lock().unwrap().clone();
                if !placed.is_empty() {
                    let hist_now = s.term.lock().unwrap().grid().history_size() as i64;
                    let mut cache = images.borrow_mut();
                    let sf = window.scale_factor();
                    for p in &placed {
                        let line = p.row - hist_now + offset as i64;
                        if line + p.rows as i64 <= 0 || line >= nrows as i64 { continue; }
                        let Some(ri) = cache.entry(p.id).or_insert_with(|| render_image(&p.image)).clone() else { continue };
                        let (w, h) = p.image.size();
                        let ib = Bounds::new(point(origin.x + px(p.col as f32 * cw), origin.y + px(line as f32 * lh)), size(px(w as f32 / sf), px(h as f32 / sf)));
                        let _ = window.paint_image(bounds, ib, gpui::Corners::default(), ri, 0, false);
                    }
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

thread_local! {
    static HL_RULES: RefCell<(u64, Rc<Vec<ssh_term_proto::highlight::Rule>>)> = RefCell::new((u64::MAX, Rc::new(vec![])));
}
/// Compiled keyword-highlight rules for the current settings generation.
fn hl_rules() -> Rc<Vec<ssh_term_proto::highlight::Rule>> {
    let gen = config::generation();
    HL_RULES.with(|h| {
        let mut h = h.borrow_mut();
        if h.0 != gen {
            let cfg = config::get();
            let rules = if cfg.terminal.keyword_highlight { ssh_term_proto::highlight::compile(cfg.terminal.highlights.iter().map(|r| (r.pattern.as_str(), r.color.as_str(), r.bold))).0 } else { vec![] };
            *h = (gen, Rc::new(rules));
        }
        h.1.clone()
    })
}
type HlCell = Option<(ssh_term_proto::highlight::Color, bool)>;
/// Per-column highlight (colour, bold) for a grid row; wide-char spacers are skipped so CJK
/// keywords match.
fn row_highlights(row: &alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>, ncols: usize) -> Vec<HlCell> {
    let rules = hl_rules();
    if rules.is_empty() { return vec![]; }
    let (mut chars, mut cols) = (Vec::with_capacity(ncols), Vec::with_capacity(ncols));
    for col in 0..ncols {
        let cell = &row[Column(col)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { continue; }
        chars.push(if cell.c == '\0' { ' ' } else { cell.c }); cols.push(col);
    }
    let mut out: Vec<HlCell> = vec![None; ncols];
    for (a, b, ri) in ssh_term_proto::highlight::spans(&rules, &chars) {
        let (c0, c1) = (cols[a], if b < cols.len() { cols[b] } else { ncols });
        for o in &mut out[c0..c1] { *o = Some((rules[ri].color, rules[ri].bold)); }
    }
    out
}
fn hl_color(th: &Theme, col: ssh_term_proto::highlight::Color) -> Hsla {
    use ssh_term_proto::highlight::Color as H;
    c(match col { H::Error => th.error, H::Warning => th.warning, H::Success => th.success, H::Info => th.info, H::Accent => th.accent, H::Rgb(v) => v })
}

#[allow(clippy::too_many_arguments)]
fn build_row(g: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>, line: i32, ncols: usize, th: &Theme, sc: &Scheme,
             sel: Option<(BPoint, BPoint)>, block: bool, hover: Option<(i32, usize, usize)>, cw: f32, window: &mut Window) -> RowPaint {
    let row = &g[Line(line)];
    let base = c(sc.bg);
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut last_style: Option<Style> = None;
    let mut wide_raw: Vec<(usize, String, Style)> = Vec::new();
    let mut boxes = Vec::new();
    let mut bgs = Vec::new();
    let mut bg_start = 0usize;
    let mut bg_cur: Option<Hsla> = None;
    let hl = row_highlights(row, ncols);
    let mut push = |text: &mut String, runs: &mut Vec<TextRun>, ch: char, st: &Style| {
        let len = ch.len_utf8();
        text.push(ch);
        if last_style.as_ref() == Some(st) { runs.last_mut().unwrap().len += len; } else { runs.push(text_run(len, st)); last_style = Some(st.clone()); }
    };
    for col in 0..=ncols {
        let mut bg = base;
        if col < ncols {
            let cell = &row[Column(col)];
            let mut fg = term_color(sc, cell.fg, true);
            bg = term_color(sc, cell.bg, false);
            if cell.flags.contains(Flags::DIM) { fg = fg.opacity(0.65); }
            if cell.flags.contains(Flags::INVERSE) { std::mem::swap(&mut fg, &mut bg); }
            if in_sel(sel, block, (line, col)) {
                bg = c(sc.selection);
                if let Some(sf) = sc.selection_fg { fg = c(sf); }
            }
            let link = cell.hyperlink().is_some();
            let hovered = hover.is_some_and(|(l, a, b)| l == line && col >= a && col < b);
            let ucolor = cell.underline_color().map(|u| term_color(sc, u, true)).unwrap_or(fg);
            let underline = if cell.flags.intersects(Flags::ALL_UNDERLINES) || hovered {
                Some(UnderlineStyle { thickness: px(if cell.flags.contains(Flags::DOUBLE_UNDERLINE) { 2. } else { 1. }), color: Some(ucolor), wavy: cell.flags.contains(Flags::UNDERCURL) })
            } else if link {
                Some(UnderlineStyle { thickness: px(1.), color: Some(ucolor.opacity(0.7)), wavy: false })
            } else { None };
            let mut hl_bold = false;
            if let Some((color, b)) = hl.get(col).cloned().flatten() { if !in_sel(sel, block, (line, col)) && !cell.flags.contains(Flags::INVERSE) { fg = hl_color(th, color); hl_bold = b; } }
            let st = Style { fg, bold: hl_bold || cell.flags.contains(Flags::BOLD), italic: cell.flags.contains(Flags::ITALIC), underline, strike: cell.flags.contains(Flags::STRIKEOUT) };
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
        let sc = scheme();
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
            // IME pre-edit: normal text colour, underlined, over the cursor cell(s)
            if let Some(pe) = st.preedit.as_ref().filter(|p| !p.is_empty()) {
                let x = origin.x + px(cursor.column.0 as f32 * cw);
                let y = origin.y + px(cursor.line.0.max(0) as f32 * lh);
                let cells: usize = pe.chars().map(|c| if (c as u32) < 0x1100 { 1 } else { 2 }).sum();
                window.paint_quad(fill(Bounds::new(point(x, y), size(px(cells as f32 * cw), px(lh))), c(sc.bg)));
                let sh = window.text_system().shape_line(pe.clone().into(), px(font_size()), &[plain_run(pe.len(), c(sc.fg))], None);
                let _ = sh.paint(point(x, y), px(lh), TextAlign::Left, None, window, cx);
                window.paint_quad(fill(Bounds::new(point(x, y + px(lh - 2.)), size(px(cells as f32 * cw), px(1.5))), c(th.accent)));
                return;
            }
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
                    let sh = window.text_system().shape_line(text.clone().into(), px(font_size()), &[plain_run(text.len(), c(sc.suggestion))], Some(px(cw)));
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
                    window.paint_quad(fill(bar, c(sc.cursor)));
                }
            } else if st.focused {
                if st.blink_on {
                    window.paint_quad(fill(b, c(sc.cursor)));
                    let ch = if cell.c != ' ' && cell.c != '\0' { Some(cell.c) } else { st.suggestion.as_ref().and_then(|s| s.chars().next()) };
                    if let Some(ch) = ch {
                        let sh = window.text_system().shape_line(ch.to_string().into(), px(font_size()), &[plain_run(ch.len_utf8(), c(sc.cursor_text))], Some(px(w)));
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

// ---------------------------------------------------------------- history dropdown
/// Rows kept in the dropdown (about 10 visible, the rest scroll).
const HIST_POP_MAX: usize = 50;
const HIST_POP_VISIBLE: usize = 10;
const HIST_POP_ROW: f32 = 26.;
/// Wait this long after the input changes before opening (avoids flashing while typing fast).
const HIST_POP_DELAY_MS: u64 = 120;

/// `sel`: None until the user picks a row with Up/Down (Enter then runs the typed line as is).
struct HistPop { input: String, items: Vec<ssh_term_proto::suggest::Match>, sel: Option<usize>, scroll: gpui::ScrollHandle }

/// What a key does while the history dropdown is open.
#[derive(Debug, PartialEq, Eq)]
enum PopKey { Select(usize), Accept(usize), Close, CloseAndPass, Pass }

/// Key handling for the dropdown: nothing is selected when it opens; Up/Down select, Enter/Tab
/// fill the selected row; Enter with nothing selected closes it and runs the typed line; Esc closes.
fn pop_key(sel: Option<usize>, n: usize, key: &str) -> PopKey {
    if n == 0 { return PopKey::Pass; }
    match (key, sel) {
        ("down", None) => PopKey::Select(0),
        ("down", Some(i)) => PopKey::Select((i + 1) % n),
        ("up", None) => PopKey::Select(n - 1),
        ("up", Some(i)) => PopKey::Select((i + n - 1) % n),
        ("enter" | "tab", Some(i)) => PopKey::Accept(i.min(n - 1)),
        ("enter", None) => PopKey::CloseAndPass,
        ("escape", _) => PopKey::Close,
        _ => PopKey::Pass,
    }
}

/// Char ranges → byte ranges of `s`.
fn char_to_byte_ranges(s: &str, hl: &[(usize, usize)]) -> Vec<std::ops::Range<usize>> {
    let idx: Vec<usize> = s.char_indices().map(|(b, _)| b).chain(std::iter::once(s.len())).collect();
    hl.iter().filter(|(a, b)| a < b && *b < idx.len()).map(|(a, b)| idx[*a]..idx[*b]).collect()
}

// ---------------------------------------------------------------- pane
pub struct TermView {
    /// SFTP browser hosted in this layout leaf (PRD 5.21 tab/split mode); no shell session then.
    pub sftp: Option<Entity<crate::sftp_view::SftpView>>,
    /// Database tab (table browser / SQL editor) instead of a shell.
    pub db: Option<Entity<crate::db_view::DbView>>,
    /// Redis / container / log view hosted instead of a shell.
    pub embed: Option<Embed>,
    /// Module workspace this pane belongs to when its content does not say (container shell).
    pub ws: Option<crate::activity::Tool>,
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
    /// Pane creation time (connecting card appears after motion::PROGRESS_DELAY_MS).
    born: Instant,
    progress_tick: bool,
    blink_running: bool,
    software_gpu: Option<bool>,
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
    /// `cd` line to send once the shell is connected (saved directory).
    pending_cd: Option<String>,
    pub ai: Option<AiCard>,
    /// Local pane held by the keep-alive daemon (id there).
    pub daemon_id: Option<String>,
    /// After a layout restore: agent resume command offered as a button (never auto-run).
    pub resume_offer: Option<String>,
    /// OSC 52 write waiting for the user (terminal.osc52_write = ask)
    pub clip_ask: Option<String>,
    /// AI input held back for a pane running an external agent (write_to_pty = ask):
    /// (text, press Enter, approved_by, capture channel).
    pty_ask: Option<(String, bool, String, Option<flume::Sender<StepOutput>>)>,
    pty_write_ok: bool,
    /// terminal search (Ctrl+Shift+F): query, current match, last result
    pub search: Option<SearchBar>,
    /// Last command failed (OSC 133;D non-zero): (command, exit code) → "Explain" button.
    pub failed: Option<(String, i32)>,
    exit_seen: u64,
    ai_inline_seq: u64,
    /// History dropdown under the cursor while typing at the prompt.
    hist_pop: Option<HistPop>,
    hist_pop_seq: u64,
    /// Enter already written to the PTY in this key handler.
    enter_sent: bool,
    /// Input the dropdown was closed for (Esc / accepted); it reopens once the input changes.
    hist_pop_dismissed: Option<String>,
    /// AI inline completion for an input prefix: (prefix, suffix shown as grey text).
    ai_inline: Option<(String, String)>,
    /// Last agent seen in this pane (kept for the layout snapshot after it exits).
    pub last_agent: Option<String>,
    bell_until: Option<Instant>,
    /// command submitted at the prompt and when (for notify.long_command_secs)
    cmd_started: Option<(Instant, String)>,
    /// AI plan step being captured: (absolute start line, command, where to send the result).
    ai_capture: Option<(usize, String, flume::Sender<StepOutput>)>,
    /// Right-click context menu (PRD 5.20).
    pub ctx_menu: Option<crate::ctxmenu_view::MenuState>,
    /// Command inserted from the context menu and not yet submitted / cleared.
    pub ctx_pending: Option<String>,
    /// Context-menu command waiting for confirmation on Enter: (command, risk · reason).
    pub ctx_confirm: Option<(String, String)>,
    /// Context-menu command refused by the permission engine (shown until dismissed).
    pub ctx_denied: Option<String>,
    _subs: Vec<Subscription>,
}

/// Natural-language → command card (built-in AI). Accepting inserts text only; executing goes
/// through the permission engine and is audited.
#[derive(Clone)]
pub enum AiCard {
    Loading { request: String },
    Ready { request: String, command: String, explanation: String, redactions: usize, provider: String },
    Confirm { command: String, reason: String },
    Denied { command: String, reason: String },
    Error { request: String, message: String },
    /// First AI request ever: what will be sent, and to whom (PRD: notice before first send).
    Consent { request: String },
}

/// Snapshot of what the input line looks like right now.
struct LineState { before_cursor: String, at_eol: bool, prompt_col: Option<usize>, alt_screen: bool }

#[derive(Default, Clone)]
pub struct SearchBar { pub query: String, pub cur: Option<(BPoint, BPoint)>, pub found: Option<bool> }

/// Result of one executed AI plan step (already redacted / withheld per policy).
#[derive(Clone, Debug, PartialEq)]
pub struct StepOutput { pub cmd: String, pub exit: Option<i32>, pub output: String, pub withheld: bool }

pub enum TermEvent { Keyword(String, String), Explain, Agent(String, AgentState), LongDone(String, u64, i32), Audit(&'static str, String), CtxEditor(String), CtxCdTab(String), CtxAiExplain(String), CtxOpenTool(String, String), CtxUseAi, Copied, Toast(String), RetryConnect, EditHost }

/// IME support (PRD 3.3: IME must work for agents too). Committed text goes to the PTY like typing;
/// composition text is only drawn (never sent) until the IME commits it.
impl TermView {
    /// Pane title in the current UI language (follows a language switch without reopening).
    /// A layout leaf that hosts an SFTP view instead of a shell (tab / split mode, PRD 5.21).
    pub fn new_sftp(v: Entity<crate::sftp_view::SftpView>, history: Arc<Mutex<History>>, manifests: Arc<Vec<Manifest>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = v.read(cx).title();
        let spec = match v.read(cx).target() { Some(t) => Spec::Ssh(t), None => Spec::Local };
        let mut me = Self::new(&title, spec, Err(anyhow::anyhow!("sftp")), history, manifests, window, cx);
        me.error = None;
        me.sftp = Some(v);
        me
    }
    /// A leaf that hosts a database view instead of a shell.
    pub fn new_db(v: Entity<crate::db_view::DbView>, history: Arc<Mutex<History>>, manifests: Arc<Vec<Manifest>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = v.read(cx).title.clone();
        let mut me = Self::new(&title, Spec::Local, Err(anyhow::anyhow!("db")), history, manifests, window, cx);
        me.error = None;
        me._subs.push(cx.observe(&v, |me: &mut Self, v, cx| { let t = v.read(cx).title.clone(); if me.title.as_ref() != t.as_str() { me.title = t.into(); } cx.notify(); }));
        me.db = Some(v);
        me
    }
    /// Hosts an SFTP or database view rather than a shell.
    /// A leaf hosting a Redis, container or log view.
    pub fn new_embed(e: Embed, history: Arc<Mutex<History>>, manifests: Arc<Vec<Manifest>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = e.title(cx);
        let mut me = Self::new(&title, Spec::Local, Err(anyhow::anyhow!("embed")), history, manifests, window, cx);
        me.error = None;
        me.embed = Some(e);
        me
    }
    pub fn embedded(&self) -> bool { self.sftp.is_some() || self.db.is_some() || self.embed.is_some() }
    pub fn shown_title(&self) -> SharedString {
        if self.embedded() { return self.title.clone(); }
        match (&self.spec, self.title.rsplit_once(" · ")) {
            (Spec::Local, Some((_, sh))) => tf("main.local_title", &[("shell", &sh)]).into(),
            _ => self.title.clone(),
        }
    }
}

impl gpui::EntityInputHandler for TermView {
    fn text_for_range(&mut self, _r: std::ops::Range<usize>, _a: &mut Option<std::ops::Range<usize>>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<String> { None }
    fn selected_text_range(&mut self, _i: bool, _w: &mut Window, _cx: &mut Context<Self>) -> Option<gpui::UTF16Selection> {
        let n = self.st.borrow().preedit.as_ref().map(|p| p.encode_utf16().count()).unwrap_or(0);
        Some(gpui::UTF16Selection { range: n..n, reversed: false })
    }
    fn marked_text_range(&self, _w: &mut Window, _cx: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        self.st.borrow().preedit.as_ref().filter(|p| !p.is_empty()).map(|p| 0..p.encode_utf16().count())
    }
    fn unmark_text(&mut self, _w: &mut Window, cx: &mut Context<Self>) { self.st.borrow_mut().preedit = None; self.cursor.update(cx, |_, cx| cx.notify()); }
    fn replace_text_in_range(&mut self, _r: Option<std::ops::Range<usize>>, text: &str, _w: &mut Window, cx: &mut Context<Self>) {
        self.st.borrow_mut().preedit = None;
        if let Some(s) = self.session.clone() { if !text.is_empty() { self.tracker.key(Key::Text(text)); fanout(&s, text.as_bytes().to_vec()); self.scroll_to_bottom(cx); } }
        self.cursor.update(cx, |_, cx| cx.notify());
    }
    fn replace_and_mark_text_in_range(&mut self, _r: Option<std::ops::Range<usize>>, text: &str, _sel: Option<std::ops::Range<usize>>, _w: &mut Window, cx: &mut Context<Self>) {
        self.st.borrow_mut().preedit = (!text.is_empty()).then(|| text.to_string());
        self.cursor.update(cx, |_, cx| cx.notify());
    }
    fn bounds_for_range(&mut self, _r: std::ops::Range<usize>, _eb: Bounds<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        // candidate window goes just below the cursor cell
        let s = self.session.as_ref()?;
        let cur = s.term.lock().unwrap().grid().cursor.point;
        let st = self.st.borrow();
        Some(Bounds::new(point(st.origin.x + px(cur.column.0 as f32 * st.cell_w), st.origin.y + px(cur.line.0.max(0) as f32 * line_h())), size(px(st.cell_w), px(line_h()))))
    }
    fn character_index_for_point(&mut self, _p: gpui::Point<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<usize> { None }
}
impl gpui::EventEmitter<TermEvent> for TermView {}

impl TermView {
    pub fn new(title: &str, spec: Spec, session: anyhow::Result<Arc<Session>>, history: Arc<Mutex<History>>,
               manifests: Arc<Vec<Manifest>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (session, error) = match session { Ok(s) => (Some(s), None), Err(e) => (None, Some(format!("{e:#}"))) };
        let st = Rc::new(RefCell::new(PaintState { cell_w: 8.0, blink_on: true, ..Default::default() }));
        let grid = cx.new(|_| GridLayer { session: session.clone(), st: st.clone(), cache: Default::default(), images: Default::default() });
        let cursor = cx.new(|_| CursorLayer { session: session.clone(), st: st.clone() });
        if let Some(s) = session.clone() {
            let rules: Vec<(String, String)> = config::get().notify.keywords.iter().filter(|k| !k.pattern.is_empty()).map(|k| (k.pattern.clone(), k.title.clone())).collect();
            if !rules.is_empty() { *s.notifier.lock().unwrap() = Some(ssh_term_proto::notify_rules::Notifier::new(rules)); }
        }
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
            born: Instant::now(), progress_tick: false, last_activity: Instant::now(), blink_running: false, software_gpu: std::env::var("SHELLAX_FORCE_BLINK").ok().map(|_| false), last_mouse_cell: None, mouse_down_btn: None,
            manifests, last_command: None, agent: None, agent_seen: Instant::now(), last_detect: Instant::now() - Duration::from_secs(5),
            detect_scheduled: false, pending_paste: None, pending_cd: None, ai: None, daemon_id: None, resume_offer: None, clip_ask: None, pty_ask: None, pty_write_ok: false, search: None, last_agent: None, failed: None, exit_seen: 0, ai_inline_seq: 0, hist_pop: None, hist_pop_seq: 0, enter_sent: false, hist_pop_dismissed: None, ai_inline: None, bell_until: None, cmd_started: None, ai_capture: None, ctx_menu: None, ctx_pending: None, ctx_confirm: None, ctx_denied: None, sftp: None, db: None, embed: None, ws: None, _subs: subs,
        }
    }

    pub fn status(&self, th: &Theme) -> (&'static str, Hsla) {
        match &self.session {
            Some(s) if s.conn_error.lock().unwrap().is_some() => (t("ui.connection_failed"), c(th.error)),
            Some(s) if s.exited.load(Ordering::Acquire) => (t("ui.disconnected"), c(th.overlay0)),
            Some(s) if s.connected.load(Ordering::Acquire) => (t("ui.connected"), c(th.success)),
            Some(_) => (t("ui.connecting"), c(th.warning)),
            None if self.sftp.is_some() => ("SFTP", c(th.accent)),
            None if self.db.is_some() => ("DB", c(th.accent)),
            None if self.embed.is_some() => (self.embed.as_ref().unwrap().badge(), c(th.accent)),
            None => (t("ui.connection_failed"), c(th.error)),
        }
    }

    /// OSC 52 from a program. Only SSH panes honor it (`ssh.osc52_write` / `ssh.osc52_read`).
    /// A local pane ignores these sequences, so a program cannot replace a ⌘C / Ctrl+C copy.
    /// Writes default to ask. Over 1 MiB is still copied when allowed, and the UI says so.
    fn handle_osc52(&mut self, s: &Arc<Session>, cx: &mut Context<Self>) {
        let stores: Vec<String> = std::mem::take(&mut *s.shared.clip_store.lock().unwrap());
        let loads: Vec<ssh_term_proto::term::ClipFormatter> = std::mem::take(&mut *s.shared.clip_load.lock().unwrap());
        if !matches!(self.spec, Spec::Ssh(_)) {
            return;
        }
        let cfg = config::get();
        for text in stores {
            use ssh_term_proto::term::{osc52_write_decision, ClipDecision};
            match osc52_write_decision(&cfg.ssh.osc52_write, text.len()) {
                ClipDecision::Allow => self.commit_clip_write(text, cx),
                ClipDecision::Ask => { self.clip_ask = Some(text); cx.notify(); }
                ClipDecision::Deny => cx.emit(TermEvent::Audit("osc52.write", format!("denied {} bytes", text.len()))),
            }
        }
        for f in loads {
            if cfg.ssh.osc52_read {
                let text = cx.read_from_clipboard().and_then(|i| i.text()).unwrap_or_default();
                cx.emit(TermEvent::Audit("osc52.read", format!("allowed {} bytes", text.len())));
                s.write(f(&text).into_bytes());
            } else {
                cx.emit(TermEvent::Audit("osc52.read", "denied".into()));
            }
        }
    }

    fn commit_clip_write(&self, text: String, cx: &mut Context<Self>) {
        let n = text.len();
        cx.emit(TermEvent::Audit("osc52.write", format!("allowed {n} bytes")));
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if ssh_term_proto::term::osc52_is_large(n) {
            cx.emit(TermEvent::Toast(t("term.osc52_large").to_string()));
        }
    }

    /// Search bar input: typing edits the query (incremental), Enter / Shift+Enter = next / previous,
    /// Escape closes. Matches are shown as the selection and scrolled into view.
    fn search_key(&mut self, k: &gpui::Keystroke, cx: &mut Context<Self>) {
        let Some(bar) = self.search.as_mut() else { return };
        let forward = match k.key.as_str() {
            "escape" => { self.search = None; cx.notify(); return; }
            "enter" => !k.modifiers.shift,
            "backspace" => { bar.query.pop(); bar.cur = None; false }
            _ => {
                let Some(ch) = k.key_char.as_ref().filter(|_| !k.modifiers.control && !k.modifiers.platform) else { return };
                bar.query.push_str(ch); bar.cur = None; false
            }
        };
        self.search_step(forward, cx);
    }
    pub fn search_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let Some(bar) = self.search.as_mut() else { return };
        let mut t = s.term.lock().unwrap();
        // a fresh query searches backwards from the bottom (newest output first)
        let m = ssh_term_proto::term::search(&*t, &bar.query, bar.cur.map(|c| c.0), forward);
        bar.found = (!bar.query.is_empty()).then_some(m.is_some());
        if let Some((a, b)) = m {
            bar.cur = Some((a, b));
            t.scroll_to_point(alacritty_terminal::index::Point::new(Line(a.0), Column(a.1)));
            drop(t);
            let mut st = self.st.borrow_mut();
            st.sel = Some((a, b)); st.sel_block = false;
        } else { drop(t); }
        self.repaint_grid(cx);
        cx.notify();
    }

    fn repaint_grid(&self, cx: &mut Context<Self>) { self.grid.update(cx, |_, cx| cx.notify()); self.cursor.update(cx, |_, cx| cx.notify()); }

    /// Called (coalesced) when the backend produced output.
    fn on_output(&mut self, cx: &mut Context<Self>) {
        self.try_flush_cd();
        if let Some(s) = &self.session {
            if let Some(seed) = s.history_seed.lock().unwrap().take() { if config::get().suggest.remote_history { self.history.lock().unwrap().extend_from_bash_history(&seed); } }
            if s.exited.load(Ordering::Acquire) || s.conn_error.lock().unwrap().is_some() { cx.notify(); }
            let fired: Vec<_> = std::mem::take(&mut *s.notices.lock().unwrap());
            for (title, line) in fired { cx.emit(TermEvent::Keyword(title, line)); }
        }
        if let Some(s) = self.session.clone() { self.handle_osc52(&s, cx); }
        if let Some(s) = &self.session {
            let rang = s.shared.bell.swap(false, Ordering::AcqRel);
            if rang && config::get().terminal.bell == "sound" { play_sound(); }
            if rang && config::get().terminal.bell == "visual" {
                self.bell_until = Some(Instant::now() + Duration::from_millis(180));
                cx.notify();
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                    let _ = this.update(cx, |v, cx| { v.bell_until = None; cx.notify(); });
                }).detach();
            }
        }
        if let Some(s) = &self.session {
            let seq = s.exit_seq.load(Ordering::Relaxed);
            if seq != self.exit_seen {
                self.exit_seen = seq;
                let code = *s.last_exit.lock().unwrap();
                if let Some((t0, cmd)) = self.cmd_started.take() {
                    let secs = t0.elapsed().as_secs();
                    let lim = config::get().notify.long_command_secs;
                    if lim > 0 && secs >= lim && self.agent.is_none() { cx.emit(TermEvent::LongDone(cmd, secs, code.unwrap_or(0))); }
                }
                if let Some((start, cmd, tx)) = self.ai_capture.take() { let _ = tx.send(self.step_output(start, cmd, code)); }
                let was = self.failed.is_some();
                self.failed = match (code, &self.last_command) { (Some(c), Some(cmd)) if c != 0 && self.agent.is_none() => Some((cmd.clone(), c)), _ => None };
                if was || self.failed.is_some() { cx.notify(); }
            }
        }
        self.last_activity = Instant::now();
        self.st.borrow_mut().blink_on = true;
        let sug = self.compute_suggestion();
        if sug.is_none() { self.schedule_ai_suggestion(cx); }
        self.st.borrow_mut().suggestion = sug.or_else(|| self.ai_suggestion());
        self.refresh_hist_pop(cx);
        self.repaint_grid(cx);
        self.schedule_detect(cx);
        // connection state changes show in the header
        if self.status(&cur()).0 != t("ui.connected") { cx.notify(); }
    }

    // ---------------- blink: only while focused, window active and recently used; repaints the cursor layer only
    fn kick_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.last_activity = Instant::now();
        self.st.borrow_mut().blink_on = true;
        if self.blink_running || !config::get().terminal.cursor_blink { return; }
        // On a software rasterizer (llvmpipe/lavapipe) every blink is a full-window CPU redraw
        // (~110 ms per frame at 1920x1200, ~23% CPU while blinking): keep the cursor steady there.
        if *self.software_gpu.get_or_insert_with(|| window.gpu_specs().is_some_and(|g| g.is_software_emulated)) { return; }
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

    // ---------------- agent detection (manifests), throttled to ~2/s while output flows
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
        if let (Some((pi, ps)), Some((ni, ns))) = (self.agent, new) {
            if pi == ni && ps != ns && ps != AgentState::Unknown && ns != AgentState::Unknown {
                cx.emit(TermEvent::Agent(agent::display_name(&self.manifests[ni].id).to_string(), ns));
            }
        }
        if new != self.agent { self.agent = new; if let Some((i, _)) = new { self.last_agent = Some(agent::display_name(&self.manifests[i].id).to_string()); } cx.notify(); }
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

    fn current_input(&self) -> Option<String> {
        let ls = self.line_state()?;
        if ls.alt_screen || !ls.at_eol { return None; }
        Some(self.tracker.current_input(&ls.before_cursor, ls.prompt_col)?.0)
    }
    /// AI grey-text suggestion for the current input, if one has arrived for exactly this prefix.
    fn ai_suggestion(&self) -> Option<String> {
        let (p, s) = self.ai_inline.as_ref()?;
        (self.current_input()?.as_str() == p.as_str()).then(|| s.clone())
    }
    /// Debounced AI completion request (suggest.ai): only when history has nothing, never for
    /// lines containing secrets, never in the alternate screen or while an agent runs.
    fn schedule_ai_suggestion(&mut self, cx: &mut Context<Self>) {
        let cfg = config::get();
        if !(cfg.ai.enabled && cfg.suggest.enabled && cfg.suggest.ai) || self.agent.is_some() { return; }
        let Some(input) = self.current_input() else { return };
        if input.trim().len() < 3 || self.ai_inline.as_ref().is_some_and(|(p, _)| *p == input) { return; }
        if ssh_term_proto::ai::redact(&input).1 > 0 || ssh_term_proto::suggest::is_sensitive(&input) { return; }
        self.ai_inline_seq += 1;
        let seq = self.ai_inline_seq;
        let Some(mut provider) = cfg.ai.default_provider().cloned() else { return };
        provider.model = provider.command_model().to_string();
        provider.max_tokens = provider.max_tokens.min(64);
        let key = crate::secret_store().get(&provider.secret_name()).map(|x| x.0).unwrap_or_default();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(600)).await;
            let Ok(still) = this.update(cx, |v, _| v.ai_inline_seq == seq && v.current_input().as_deref() == Some(input.as_str())) else { return };
            if !still { return; }
            let inp2 = input.clone();
            let r = cx.background_executor().spawn(async move {
                ssh_term_proto::ai::chat(&provider, &key, ssh_term_proto::ai::COMPLETE_SYSTEM_PROMPT, &format!("Partial command: {inp2}"), None).ok()
            }).await;
            let _ = this.update(cx, |v, cx| {
                if v.ai_inline_seq != seq { return; }
                if let Some(suf) = r.and_then(|r| ssh_term_proto::ai::completion_suffix(&input, &r.text)) {
                    v.ai_inline = Some((input.clone(), suf));
                    v.st.borrow_mut().suggestion = v.ai_suggestion();
                    v.repaint_grid(cx);
                }
            });
        }).detach();
    }
    /// Input the history dropdown may show for: setting on, at a normal prompt (no alternate
    /// screen, no agent CLI, typed text echoed — so not a password prompt), cursor at line end.
    fn hist_pop_input(&self) -> Option<String> {
        let cfg = config::get();
        if !(cfg.suggest.enabled && cfg.suggest.popup) || self.agent.is_some() || self.search.is_some() { return None; }
        self.current_input().filter(|i| !i.trim().is_empty())
    }
    fn hist_pop_items(&self, input: &str) -> Vec<ssh_term_proto::suggest::Match> {
        self.history.lock().unwrap().matches(input, HIST_POP_MAX)
    }
    /// Keep the dropdown in step with the prompt line. While open it updates at once (no
    /// flicker); opening waits briefly so fast typing does not flash it.
    fn refresh_hist_pop(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.hist_pop_input() else {
            self.hist_pop_dismissed = None;
            if self.hist_pop.take().is_some() { cx.notify(); }
            return;
        };
        if self.hist_pop.as_ref().is_some_and(|p| p.input == input) { return; }
        if self.hist_pop_dismissed.as_deref() == Some(input.as_str()) { if self.hist_pop.take().is_some() { cx.notify(); } return; }
        self.hist_pop_dismissed = None;
        if let Some(p) = self.hist_pop.as_mut() {
            let items = self.history.lock().unwrap().matches(&input, HIST_POP_MAX);
            if items.is_empty() { self.hist_pop = None; } else { p.input = input; p.items = items; p.sel = None; p.scroll.scroll_to_item(0); }
            cx.notify();
            return;
        }
        self.hist_pop_seq += 1;
        let seq = self.hist_pop_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(HIST_POP_DELAY_MS)).await;
            let _ = this.update(cx, |v, cx| {
                if v.hist_pop_seq != seq || v.hist_pop.is_some() { return; }
                let Some(now) = v.hist_pop_input() else { return };
                if now != input || v.hist_pop_dismissed.as_deref() == Some(now.as_str()) { return; }
                let items = v.hist_pop_items(&now);
                if items.is_empty() { return; }
                v.hist_pop = Some(HistPop { input: now, items, sel: None, scroll: gpui::ScrollHandle::new() });
                cx.notify();
            });
        }).detach();
    }
    /// Replace the prompt line with history entry `i` (never presses Enter).
    fn hist_pop_accept(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(p) = self.hist_pop.take() else { return };
        let Some(m) = p.items.get(i) else { return };
        let Some(s) = self.session.clone() else { return };
        let cmd = m.cmd.replace(['\r', '\n'], " ");
        let mut bytes = vec![self.kill_line_byte()];
        bytes.extend_from_slice(cmd.as_bytes());
        s.write(bytes);
        self.tracker.key(Key::KillLine);
        self.tracker.key(Key::Text(&cmd));
        self.ai_inline = None;
        self.st.borrow_mut().suggestion = None;
        self.hist_pop_dismissed = Some(cmd);
        self.scroll_to_bottom(cx);
        cx.notify();
    }
    /// Keys while the dropdown is open. Returns true when consumed.
    fn hist_pop_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(p) = self.hist_pop.as_mut() else { return false };
        match pop_key(p.sel, p.items.len(), key) {
            PopKey::Select(i) => { p.sel = Some(i); p.scroll.scroll_to_item(i); cx.notify(); true }
            PopKey::Accept(i) => { self.hist_pop_accept(i, cx); true }
            PopKey::Close => { self.hist_pop_dismissed = Some(p.input.clone()); self.hist_pop = None; cx.notify(); true }
            // close, then let the key reach the shell (runs the typed line)
            PopKey::CloseAndPass => { self.hist_pop = None; cx.notify(); false }
            PopKey::Pass => false,
        }
    }
    fn hist_pop_el(&self, window: &Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let p = self.hist_pop.as_ref()?;
        let s = self.session.as_ref()?;
        let (col, line) = { let t = s.term.lock().unwrap(); let g = t.grid(); if g.display_offset() != 0 { return None; } (g.cursor.point.column.0, g.cursor.point.line.0.max(0)) };
        let (cw, origin, lh) = { let st = self.st.borrow(); (st.cell_w, st.origin, line_h()) };
        let th = cur();
        let rows = p.items.len().min(HIST_POP_VISIBLE);
        let h = rows as f32 * HIST_POP_ROW + 10.;
        let x = origin.x + px(col as f32 * cw);
        let below = origin.y + px((line + 1) as f32 * lh + 2.);
        let flip = below + px(h) > window.viewport_size().height - px(8.);
        let (anchor, pos) = if flip { (gpui::Anchor::BottomLeft, point(x, origin.y + px(line as f32 * lh - 2.))) } else { (gpui::Anchor::TopLeft, point(x, below)) };
        let mono = SharedString::from(config::get().font.family.clone());
        let mut list = div().id("hist-pop-list").track_scroll(&p.scroll).max_h(px(rows as f32 * HIST_POP_ROW)).overflow_y_scroll().flex().flex_col();
        for (i, m) in p.items.iter().enumerate() {
            let selected = p.sel == Some(i);
            let hl: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = char_to_byte_ranges(&m.cmd, &m.hl).into_iter()
                .map(|r| (r, gpui::HighlightStyle { color: Some(c(th.accent)), font_weight: Some(FontWeight::BOLD), ..Default::default() })).collect();
            let text = gpui::StyledText::new(SharedString::from(m.cmd.clone())).with_highlights(hl);
            list = list.child(div().id(("hist-pop", i)).h(px(HIST_POP_ROW)).flex_shrink_0().px_2().mx_1().rounded_md().flex().items_center().gap_2().cursor_pointer()
                .text_color(c(th.text))
                .when(selected, |d| d.bg(crate::theme::selected_bg()))
                .when(!selected, |d| d.hover(|d| d.bg(crate::theme::hover_bg())))
                .on_mouse_down(MouseButton::Left, cx.listener(move |v, _, _, cx| { cx.stop_propagation(); v.hist_pop_accept(i, cx); }))
                .child(crate::ui::icon(crate::ui::IconName::Clock, 13., if selected { c(th.accent) } else { c(th.overlay1) }))
                .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(mono.clone()).text_size(px(13.)).child(text)));
        }
        let pop = div().occlude().min_w(px(260.)).max_w(px(640.)).py_1().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface1)).shadow_lg()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| { if let Some(p) = v.hist_pop.take() { v.hist_pop_dismissed = Some(p.input); cx.notify(); } }))
            .child(list);
        Some(gpui::deferred(gpui::anchored().anchor(anchor).position(pos).snap_to_window_with_margin(px(8.)).child(pop)).with_priority(5).into_any_element())
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
        let block = self.st.borrow().sel_block;
        let (s, e) = if block { ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))) } else if a <= b { (a, b) } else { (b, a) };
        let sess = self.session.as_ref()?;
        let t = sess.term.lock().unwrap();
        let g = t.grid();
        let ncols = g.columns();
        let mut out = String::new();
        for l in s.0..=e.0 {
            if l < g.topmost_line().0 || l > g.bottommost_line().0 { continue; }
            let row = &g[Line(l)];
            let c0 = if block || l == s.0 { s.1 } else { 0 };
            let c1 = if block || l == e.0 { e.1.min(ncols - 1) } else { ncols - 1 };
            let mut line = String::new();
            for col in c0..=c1 {
                let cell = &row[Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { continue; }
                line.push(if cell.c == '\0' { ' ' } else { cell.c });
                if let Some(z) = cell.zerowidth() { line.extend(z.iter()); }
            }
            let wrapped = row[Column(ncols - 1)].flags.contains(Flags::WRAPLINE);
            if block { out.push_str(line.trim_end()); if l != e.0 { out.push('\n'); } continue; }
            if wrapped && l != e.0 { out.push_str(&line); } else { out.push_str(line.trim_end()); if l != e.0 { out.push('\n'); } }
        }
        Some(out)
    }

    /// Copy the selection. Returns false when there is nothing to copy, and does not notify then.
    pub fn copy(&self, cx: &mut Context<Self>) -> bool {
        let Some(t) = self.selection_text().filter(|t| !t.is_empty()) else { return false };
        cx.write_to_clipboard(ClipboardItem::new_string(t));
        cx.emit(TermEvent::Copied);
        true
    }

    /// 剪贴板文本；若复制的是文件/文件夹，则返回其完整路径（多个以空格分隔并按目标 shell 加引号）。
    fn clipboard_paste_text(&self, cx: &mut Context<Self>) -> Option<String> {
        if let Some(paths) = ssh_term_proto::clipfiles::read() {
            let win_shell = cfg!(windows) && matches!(self.spec, Spec::Local);
            return Some(ssh_term_proto::clipfiles::format(&paths, win_shell));
        }
        cx.read_from_clipboard().and_then(|i| i.text())
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.clipboard_paste_text(cx) else { return };
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
        fanout(&s, bytes);
    }

    // ---------------- keyboard
    /// Tab / Shift+Tab stay in the terminal: completion, or accept a suggestion when one is showing.
    pub fn inject_tab(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ev = KeyDownEvent {
            keystroke: gpui::Keystroke {
                key: "tab".into(),
                key_char: None,
                modifiers: gpui::Modifiers { shift, ..Default::default() },
            },
            is_held: false,
            prefer_character_input: false,
        };
        self.on_key(&ev, window, cx);
    }
    pub(crate) fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        let bare_tab = k.key == "tab" && !m.control && !m.alt && !m.platform;
        let Some(s) = self.session.clone() else { if bare_tab { cx.stop_propagation(); } return; };
        self.kick_blink(window, cx);
        if self.ai.is_some() && !m.control && !m.alt && !m.platform {
            match k.key.as_str() {
                "escape" => { self.ai_cancel(cx); cx.stop_propagation(); return; }
                "tab" | "right" if matches!(self.ai, Some(AiCard::Ready { .. })) => { self.ai_insert(cx); cx.stop_propagation(); return; }
                _ => {}
            }
        }
        if self.ctx_menu.is_some() && k.key == "escape" { self.ctx_menu = None; cx.notify(); cx.stop_propagation(); return; }
        if self.ctx_confirm.is_some() && k.key == "escape" { self.ctx_confirm = None; cx.notify(); cx.stop_propagation(); return; }
        if self.search.is_some() { self.search_key(k, cx); cx.stop_propagation(); return; }
        // app shortcuts
        if m.control && m.shift && !m.alt {
            match k.key.to_ascii_lowercase().as_str() {
                "f" => { self.search = Some(SearchBar::default()); cx.notify(); cx.stop_propagation(); return; }
                "c" => { self.copy(cx); cx.stop_propagation(); return; }
                "v" => { self.paste(cx); cx.stop_propagation(); return; }
                _ => {}
            }
        }
        // ⌘C on macOS, Ctrl+C on Windows/Linux, only while text is selected.
        // A bare Ctrl+C with no selection still falls through and interrupts the program.
        let has_sel = self.st.borrow().sel.is_some();
        if should_copy_selection(cfg!(target_os = "macos"), &k.key, m.control, m.platform, m.shift, m.alt, has_sel) && self.copy(cx) {
            cx.stop_propagation();
            return;
        }
        // ⌘V on macOS, Ctrl+V on Windows/Linux. Ctrl+Shift+V is handled above.
        if should_paste(cfg!(target_os = "macos"), &k.key, m.control, m.platform, m.shift, m.alt) {
            self.paste(cx);
            cx.stop_propagation();
            return;
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
        if plain && self.hist_pop.is_some() && self.hist_pop_key(k.key.as_str(), cx) { cx.stop_propagation(); return; }
        let accept = config::get().suggest.accept.clone();
        let key_ok = match accept.as_str() { "right" => k.key == "right", "tab" => k.key == "tab", _ => k.key == "right" || k.key == "tab" };
        if plain && key_ok {
            if let Some(sug) = self.compute_suggestion().or_else(|| self.ai_suggestion()) {
                self.ai_inline = None;
                s.write(sug.clone().into_bytes());
                self.tracker.key(Key::Text(&sug));
                self.st.borrow_mut().suggestion = None;
                self.scroll_to_bottom(cx);
                cx.stop_propagation();
                return;
            }
        }
        let app_cursor = self.mode().contains(TermMode::APP_CURSOR);
        let Some(bytes) = key_to_bytes(k, app_cursor) else { if bare_tab { cx.stop_propagation(); } return; };
        match k.key.as_str() {
            "enter" if plain && self.nl_request().is_some() => {
                let req = self.nl_request().unwrap_or_default();
                // clear the typed "# ..." line instead of submitting it (it would be a shell comment anyway)
                s.write(vec![self.kill_line_byte()]);
                self.tracker.key(Key::KillLine);
                self.ai_request(req, cx);
                cx.stop_propagation();
                return;
            }
            "enter" if self.ctx_pending.is_some() => {
                // a command inserted from the context menu: the permission engine decides on Enter
                let cmd = self.line_state().and_then(|ls| self.tracker.submitted_input(&ls.before_cursor, ls.prompt_col)).or_else(|| self.ctx_pending.clone()).unwrap_or_default();
                if self.ctx_gate(cmd, cx) { self.ctx_submit(cx); }
                cx.stop_propagation();
                return;
            }
            "enter" => {
                // send Enter to the shell first; history / audit bookkeeping (file appends) must not
                // sit between the key press and the shell seeing it
                let ls = self.line_state();
                fanout(&s, bytes.clone());
                self.enter_sent = true;
                if let Some(ls) = ls {
                    if !ls.alt_screen {
                        if let Some(cmd) = self.tracker.submitted_input(&ls.before_cursor, ls.prompt_col) {
                            self.history.lock().unwrap().add(&cmd);
                            // remember the command word for agent identification over SSH (not while an agent runs)
                            if config::get().audit.log_commands { let _ = crate::audit_log().append("command", "user", &ssh_term_proto::ai::redact(&cmd).0); }
                            if self.agent.is_none() { self.cmd_started = Some((Instant::now(), cmd.clone())); self.last_command = Some(cmd); }
                        }
                    }
                }
                self.tracker.key(Key::Enter);
            }
            "backspace" => { self.tracker.key(Key::Backspace); }
            _ if m.control && k.key == "c" => { self.tracker.key(Key::Interrupt); self.ctx_pending = None; self.ctx_confirm = None; }
            _ if m.control && k.key == "u" => { self.tracker.key(Key::KillLine); self.ctx_pending = None; }
            _ => {
                let printable = !m.control && !m.alt && !m.platform && bytes.first().is_some_and(|b| *b >= 0x20 && *b != 0x7f);
                if printable { self.tracker.key(Key::Text(std::str::from_utf8(&bytes).unwrap_or(""))); } else { self.tracker.key(Key::Other); }
            }
        }
        if self.st.borrow().sel.is_some() { self.st.borrow_mut().sel = None; self.grid.update(cx, |_, cx| cx.notify()); }
        self.scroll_to_bottom(cx);
        if !std::mem::take(&mut self.enter_sent) { fanout(&s, bytes); }
        cx.stop_propagation();
    }

    // ---------------- built-in AI: natural language -> command
    /// "# list big files" typed at the prompt (prefix configurable), when AI is enabled.
    fn nl_request(&self) -> Option<String> {
        let cfg = config::get();
        if !cfg.ai.enabled || cfg.ai.nl_prefix.is_empty() { return None; }
        let ls = self.line_state()?;
        if ls.alt_screen { return None; }
        let (input, _) = self.tracker.current_input(&ls.before_cursor, ls.prompt_col)?;
        let rest = input.trim_start().strip_prefix(cfg.ai.nl_prefix.as_str())?.trim();
        (!rest.is_empty()).then(|| rest.to_string())
    }
    /// Shortcut entry: use the current input line (prefix optional) as the request.
    pub fn ai_from_line(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let req = self.nl_request().or_else(|| {
            let ls = self.line_state()?;
            let (input, _) = self.tracker.current_input(&ls.before_cursor, ls.prompt_col)?;
            let t = input.trim().to_string();
            (!t.is_empty()).then_some(t)
        });
        match req {
            Some(r) => { s.write(vec![self.kill_line_byte()]); self.tracker.key(Key::KillLine); self.ai_request(r, cx); }
            None => { self.ai = Some(AiCard::Error { request: String::new(), message: tf("ai.nl_hint", &[("prefix", &config::get().ai.nl_prefix)]) }); cx.notify(); }
        }
    }
    /// Logical screen lines (soft-wrapped rows joined, so a wrapped secret is redacted as a whole).
    fn screen_lines(&self) -> Vec<String> {
        let Some(s) = &self.session else { return vec![] };
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let ncols = g.columns();
        let mut out = vec![];
        let mut cur = String::new();
        for l in g.topmost_line().0..=g.bottommost_line().0 {
            let row = &g[Line(l)];
            for c in 0..ncols {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { continue; }
                cur.push(if cell.c == '\0' { ' ' } else { cell.c });
            }
            if !row[Column(ncols - 1)].flags.contains(Flags::WRAPLINE) { out.push(cur.trim_end().to_string()); cur.clear(); }
        }
        if !cur.is_empty() { out.push(cur.trim_end().to_string()); }
        out
    }
    /// Names this pane's host is known by (config name, address, user@host:port) for per-host AI rules.
    pub fn ai_host_names(&self) -> Vec<String> {
        match &self.spec {
            Spec::Local => vec!["local".into()],
            Spec::Ssh(t) => {
                let mut v = vec![t.host.clone(), t.label()];
                v.extend(config::get().all_hosts().into_iter().filter(|e| e.host == t.host && e.port.unwrap_or(22) == t.port).map(|e| e.name));
                v
            }
        }
    }
    /// The host is listed in ai.no_output_hosts (independent of ai.send_output).
    pub fn output_blocked_for_host(&self) -> bool {
        let mut a = config::get().ai.clone(); a.send_output = true;
        let names = self.ai_host_names();
        !a.output_allowed(&names.iter().map(String::as_str).collect::<Vec<_>>())
    }
    /// ai.send_output and the host is not in ai.no_output_hosts.
    pub fn ai_output_allowed(&self) -> bool {
        let names = self.ai_host_names();
        config::get().ai.output_allowed(&names.iter().map(String::as_str).collect::<Vec<_>>())
    }
    /// One-paragraph notice of what an AI request from this pane sends.
    pub fn ai_consent_text(&self) -> String {
        let cfg = config::get();
        let p = cfg.ai.default_provider().map(|p| format!("{} ({})", p.name, p.base_url)).unwrap_or_default();
        tf("ai.consent_body", &[("provider", &p), ("scope", &cfg.ai.context),
            ("output", &if self.ai_output_allowed() { t("ai.consent_output_yes") } else { t("ai.consent_output_no") }),
            ("redact", &if cfg.ai.redact { t("ai.consent_redact_on") } else { t("ai.consent_redact_off") })])
    }
    /// Redacted terminal context per the AI settings: (text, redactions, host label).
    /// Absolute line index (scrollback + screen) of the cursor, matching `screen_lines()` indices.
    fn abs_cursor_line(&self) -> usize {
        let Some(s) = &self.session else { return 0 };
        let t = s.term.lock().unwrap();
        let g = t.grid();
        (g.cursor.point.line.0 - g.topmost_line().0).max(0) as usize
    }
    /// Output of an AI plan step for feeding back to the model: lines after the command line up
    /// to the new prompt (last 200), redacted; withheld for no-output hosts / ai.send_output off.
    fn step_output(&self, start: usize, cmd: String, code: Option<i32>) -> StepOutput {
        let allowed = self.ai_output_allowed();
        let output = if !allowed { String::new() } else {
            let lines = self.screen_lines();
            let end = self.abs_cursor_line().min(lines.len());
            let from = (start + 1).min(end);
            let body: Vec<&str> = lines[from..end].iter().map(String::as_str).collect();
            let body = &body[body.len().saturating_sub(200)..];
            ssh_term_proto::ai::redact(body.join("\n").trim_end()).0
        };
        StepOutput { cmd: ssh_term_proto::ai::redact(&cmd).0, exit: code, output, withheld: !allowed }
    }
    /// Run an approved AI plan step and report its exit code and output when the shell signals
    /// completion (OSC 133;D).
    pub(crate) fn ai_run_capture(&mut self, command: String, approved_by: &str, tx: flume::Sender<StepOutput>, cx: &mut Context<Self>) {
        if self.session.is_none() { let _ = tx.send(StepOutput { cmd: command, exit: None, output: String::new(), withheld: true }); return; }
        let tx = match self.pty_gate(&command, true, approved_by, Some(tx), cx) {
            Ok(Some(tx)) => tx,
            Ok(None) => return,
            Err(None) => return, // held for the write_to_pty card
            Err(Some(tx)) => { let _ = tx.send(StepOutput { cmd: command, exit: None, output: "[not typed: write_to_pty = deny]".into(), withheld: true }); return; }
        };
        let start = self.abs_cursor_line();
        self.ai_capture = Some((start, command.clone(), tx));
        self.ai_run_now(command, approved_by, cx);
    }
    pub fn ai_context(&self) -> (String, usize, String) {
        let cfg = config::get();
        let screen = self.screen_lines();
        let sel = self.selection_text();
        let inp = ssh_term_proto::ai::ContextInput { selection: sel.as_deref(), last_command: self.last_command.as_deref(), screen: &screen };
        let opts = ssh_term_proto::ai::ContextOpts { scope: ssh_term_proto::ai::Scope::from_id(&cfg.ai.context), lines: cfg.ai.context_lines as usize, send_output: self.ai_output_allowed(), redact: cfg.ai.redact };
        let (t, n) = ssh_term_proto::ai::build_context(&inp, &opts);
        (t, n, match &self.spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() })
    }
    /// What the chat composer will actually attach. Screen scope sends the recent terminal
    /// text even when `ai.send_output` is off. Hosts in `no_output_hosts` still get only the
    /// last command. The `#` command card keeps using `ai_context`.
    pub fn chat_context(&self) -> (String, usize, String, &'static str) {
        let cfg = config::get();
        let host = match &self.spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() };
        let scope = ssh_term_proto::ai::Scope::from_id(&cfg.ai.context);
        let blocked = self.output_blocked_for_host();
        let screen = self.screen_lines();
        let sel = self.selection_text();
        let inp = ssh_term_proto::ai::ContextInput { selection: sel.as_deref(), last_command: self.last_command.as_deref(), screen: &screen };
        let lines = cfg.ai.context_lines as usize;
        let redact = cfg.ai.redact;
        let pack = |scope, send_output| {
            let opts = ssh_term_proto::ai::ContextOpts { scope, lines, send_output, redact };
            ssh_term_proto::ai::build_context(&inp, &opts)
        };
        if matches!(scope, ssh_term_proto::ai::Scope::Screen) && !blocked {
            let (t, n) = pack(ssh_term_proto::ai::Scope::Screen, true);
            if !t.trim().is_empty() { return (t, n, host, "screen"); }
        }
        if matches!(scope, ssh_term_proto::ai::Scope::Selection) && !blocked && self.ai_output_allowed() {
            let (t, n) = pack(ssh_term_proto::ai::Scope::Selection, true);
            if !t.trim().is_empty() { return (t, n, host, "selection"); }
        }
        if matches!(scope, ssh_term_proto::ai::Scope::None) { return (String::new(), 0, host, "none"); }
        let (t, n) = pack(ssh_term_proto::ai::Scope::Command, false);
        if t.trim().is_empty() { (String::new(), 0, host, "none") } else { (t, n, host, "command") }
    }
    /// For "explain failure": command, exit code, redacted output tail (200 lines, always redacted).
    pub fn failure_context(&self) -> Option<(String, i32, String, String)> {
        let (cmd, code) = self.failed.clone()?;
        let lines = self.screen_lines();
        let tail: Vec<&str> = lines.iter().rev().skip_while(|l| l.trim().is_empty()).take(200).map(|s| s.as_str()).collect::<Vec<_>>().into_iter().rev().collect();
        // hosts in ai.no_output_hosts never send output, not even for "explain failure"
        let (out, _) = if !self.output_blocked_for_host() { ssh_term_proto::ai::redact(&tail.join("\n")) } else { (t("ai.output_withheld").to_string(), 0) };
        let (cmd, _) = ssh_term_proto::ai::redact(&cmd);
        Some((cmd, code, out, match &self.spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() }))
    }
    pub fn ai_request(&mut self, request: String, cx: &mut Context<Self>) {
        let cfg = config::get();
        if !cfg.ai.enabled { self.ai = Some(AiCard::Error { request, message: t("ai.disabled").into() }); cx.notify(); return; }
        let Some(mut provider) = cfg.ai.default_provider().cloned() else {
            self.ai = Some(AiCard::Error { request, message: t("ai.no_provider").into() }); cx.notify(); return;
        };
        if !cfg.ai.first_send_ack { self.ai = Some(AiCard::Consent { request }); cx.notify(); return; }
        let key = crate::secret_store().get(&provider.secret_name()).map(|x| x.0).unwrap_or_default();
        provider.model = provider.command_model().to_string(); // fast model for command generation if set
        // `#` requests still honour send_output. Chat uses chat_context and can send the screen anyway.
        let (ctx_text, mut n_red, _host) = self.ai_context();
        let (req_sent, n2) = if cfg.ai.redact { ssh_term_proto::ai::redact(&request) } else { (request.clone(), 0) };
        n_red += n2;
        let shell = match &self.spec { Spec::Local => { let s = crate::config::get().terminal.shell.clone(); if s.is_empty() { ssh_term_proto::local::default_shell_path() } else { s } } Spec::Ssh(_) => "remote POSIX shell".into() };
        let host = match &self.spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() };
        let user = ssh_term_proto::ai::build_user_prompt(&req_sent, &shell, &host, &ctx_text);
        let _ = crate::audit_log().append("ai.request", "user", &format!(
            "provider={} model={} request={:?} context_scope={} context_chars={} redactions={}",
            provider.name, provider.model, req_sent, cfg.ai.context, ctx_text.chars().count(), n_red));
        self.ai = Some(AiCard::Loading { request: request.clone() });
        cx.notify();
        let pname = provider.name.clone();
        let task = cx.background_executor().spawn(async move {
            ssh_term_proto::ai::chat(&provider, &key, ssh_term_proto::ai::SYSTEM_PROMPT, &user, None).map_err(|e| format!("{e:#}"))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                if !matches!(v.ai, Some(AiCard::Loading { .. })) { return; } // cancelled
                v.ai = Some(match r {
                    Ok(reply) => {
                        let (command, explanation) = ssh_term_proto::ai::extract_command(&reply.text);
                        let _ = crate::audit_log().append("ai.response", "ai", &format!("command={command:?} latency_ms={}", reply.latency.as_millis()));
                        if command.is_empty() { AiCard::Error { request, message: t("ai.empty_reply").into() } }
                        else { AiCard::Ready { request, command, explanation, redactions: n_red, provider: pname } }
                    }
                    Err(e) => AiCard::Error { request, message: e },
                });
                cx.notify();
            });
        }).detach();
    }
    fn ai_cancel(&mut self, cx: &mut Context<Self>) { self.ai = None; cx.notify(); }
    /// Insert the suggested command at the prompt. Never sends Enter.
    pub fn ai_insert(&mut self, cx: &mut Context<Self>) {
        let Some(AiCard::Ready { command, .. }) = self.ai.clone() else { return };
        if self.pty_gate(&command, false, "user", None, cx).is_err() { return; }
        self.insert_now(command, cx);
    }
    /// Insert text from the chat panel (never presses Enter), through the write_to_pty gate.
    pub(crate) fn ai_insert_text(&mut self, text: String, cx: &mut Context<Self>) {
        if self.pty_gate(&text, false, "user", None, cx).is_err() { return; }
        self.insert_now(text, cx);
    }
    fn insert_now(&mut self, command: String, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let one_line = command.replace(['\r', '\n'], " ");
        s.write(one_line.clone().into_bytes());
        self.tracker.key(Key::Text(&one_line));
        let _ = crate::audit_log().append("ai.insert", "user", &format!("command={one_line:?}"));
        self.ai = None;
        cx.notify();
    }
    /// Permission-engine verdict for an AI-proposed command in this pane (mode, rules, production host).
    pub fn ai_policy(&self, command: &str) -> (Effect, Risk, String) {
        let cfg = config::get();
        let max = match cfg.permissions.max_auto_risk.as_str() { "R0" => Risk::R0, "R2" => Risk::R2, _ => Risk::R1 };
        let prod = cfg.permissions.prod_confirm && matches!(&self.spec, Spec::Ssh(t) if t.production);
        let (eff, reason) = policy::decide_host(cfg.permissions.mode, max, &cfg.permissions.rules, "ai", "exec", command, cfg.permissions.dangerous_confirm, prod, &self.audit_host());
        (eff, policy::classify(command), reason)
    }
    /// Host name used in audit records ("local" for local panes), matching SHELLAX_HOST.
    /// Byte that clears the half-typed input line. Local PowerShell/cmd on Windows (PSReadLine)
    /// ignores Ctrl+U and echoes it as `^U`, so use Esc (RevertLine) there; POSIX shells get Ctrl+U.
    fn kill_line_byte(&self) -> u8 {
        if cfg!(windows) && matches!(self.spec, Spec::Local) { 0x1b } else { 0x15 }
    }
    pub fn audit_host(&self) -> String { match &self.spec { Spec::Local => "local".into(), Spec::Ssh(t) => t.host.clone() } }
    /// Execute: the permission engine decides (mode + rules); every outcome is audited.
    pub fn ai_execute(&mut self, cx: &mut Context<Self>) {
        let Some(AiCard::Ready { command, .. }) = self.ai.clone() else { return };
        let cfg = config::get();
        let max = match cfg.permissions.max_auto_risk.as_str() { "R0" => Risk::R0, "R2" => Risk::R2, _ => Risk::R1 };
        let prod = cfg.permissions.prod_confirm && matches!(&self.spec, Spec::Ssh(t) if t.production);
        let (eff, reason) = policy::decide_host(cfg.permissions.mode, max, &cfg.permissions.rules, "ai", "exec", &command, cfg.permissions.dangerous_confirm, prod, &self.audit_host());
        let risk = policy::classify(&command);
        let _ = crate::audit_log().append("ai.exec.decision", "policy", &format!("mode={} host={} risk={} effect={:?} command={command:?} reason={reason:?}", cfg.permissions.mode.label(), self.audit_host(), risk.label(), eff));
        match eff {
            Effect::Allow => self.ai_run(command, "policy", cx),
            Effect::Ask => { self.ai = Some(AiCard::Confirm { command, reason }); cx.notify(); }
            Effect::Deny => { self.ai = Some(AiCard::Denied { command, reason }); cx.notify(); }
        }
    }
    fn ai_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(AiCard::Confirm { command, .. }) = self.ai.clone() else { return };
        self.ai_run(command, "user", cx);
    }
    /// write_to_pty gate for AI input into this pane. Returns true when the write may proceed now;
    /// otherwise the input is held for the card (ask) or dropped (deny), audited either way.
    fn pty_gate(&mut self, text: &str, enter: bool, by: &str, tx: Option<flume::Sender<StepOutput>>, cx: &mut Context<Self>) -> Result<Option<flume::Sender<StepOutput>>, Option<flume::Sender<StepOutput>>> {
        let agent = self.agent.map(|(i, _)| agent::display_name(&self.manifests[i].id).to_string());
        let mut mode = config::get().agents.write_to_pty.clone();
        if let Some(m) = ssh_term_proto::managed::get() { mode = m.cap_pty_write(&mode); }
        match policy::write_to_pty(&mode, agent.is_some(), self.pty_write_ok) {
            Effect::Allow => { if let Some(a) = &agent { let _ = crate::audit_log().append("write_to_pty", by, &format!("allow mode={mode} agent={a} host={} chars={}", self.audit_host(), text.chars().count())); } Ok(tx) }
            Effect::Ask => {
                let _ = crate::audit_log().append("write_to_pty", "policy", &format!("ask mode={mode} agent={} host={} chars={}", agent.unwrap_or_default(), self.audit_host(), text.chars().count()));
                self.pty_ask = Some((text.to_string(), enter, by.to_string(), tx)); self.ai = None; cx.notify(); Err(None)
            }
            Effect::Deny => {
                let _ = crate::audit_log().append("write_to_pty", "policy", &format!("deny mode={mode} agent={} host={}", agent.unwrap_or_default(), self.audit_host()));
                self.ai = None; cx.notify(); Err(tx)
            }
        }
    }
    fn pty_answer(&mut self, allow: bool, cx: &mut Context<Self>) {
        let Some((text, enter, by, tx)) = self.pty_ask.take() else { return };
        let _ = crate::audit_log().append("write_to_pty", "user", &format!("{} host={} chars={}", if allow { "approved" } else { "rejected" }, self.audit_host(), text.chars().count()));
        if !allow { if let Some(tx) = tx { let _ = tx.send(StepOutput { cmd: text, exit: None, output: "[not typed: user rejected writing into the agent pane]".into(), withheld: true }); } cx.notify(); return; }
        self.pty_write_ok = true;
        if enter {
            if let Some(tx) = tx { let start = self.abs_cursor_line(); self.ai_capture = Some((start, text.clone(), tx)); }
            self.ai_run_now(text, &by, cx);
        } else { self.insert_now(text, cx); }
    }
    pub(crate) fn ai_run(&mut self, command: String, approved_by: &str, cx: &mut Context<Self>) {
        if self.pty_gate(&command, true, approved_by, None, cx).is_err() { return; }
        self.ai_run_now(command, approved_by, cx);
    }
    fn ai_run_now(&mut self, command: String, approved_by: &str, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let one_line = command.replace(['\r', '\n'], " ");
        let _ = crate::audit_log().append("ai.exec", approved_by, &format!("host={} command={one_line:?}", self.audit_host()));
        self.tracker.key(Key::Text(&one_line));
        self.last_command = Some(one_line.clone());
        s.write(format!("{one_line}\r").into_bytes());
        self.tracker.key(Key::Enter);
        self.ai = None;
        self.scroll_to_bottom(cx);
        cx.notify();
    }

    fn ai_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let card = self.ai.clone()?;
        let th = cur();
        let mono = SharedString::from(config::get().font.family.clone());
        let btn = |id: &'static str, label: &'static str, kind: u8| div().id(id).px_3().py(px(4.)).rounded_md().cursor_pointer().text_size(px(12.))
            .bg(match kind { 1 => c(th.accent), 2 => c(th.error), _ => c(th.surface0) })
            .text_color(if kind == 0 { c(th.text) } else { c(th.base) }).child(label);
        let (border, title, body, buttons): (Hsla, String, gpui::Div, gpui::Div) = match card {
            AiCard::Loading { request } => (c(th.accent), tf("ai.card_loading", &[("req", &request)]), div(), div().flex().gap_2()
                .child(btn("ai-cancel", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| v.ai_cancel(cx))))),
            AiCard::Ready { request, command, explanation, redactions, provider } => (c(th.accent), tf("ai.card_ready", &[("req", &request), ("provider", &provider)]),
                div().flex().flex_col().gap_1()
                    .child(div().px_2().py_1().rounded_md().bg(c(th.bg)).font_family(mono).text_size(px(13.)).text_color(c(th.text)).child(command))
                    .when(!explanation.is_empty(), |d| d.child(div().text_color(c(th.overlay1)).child(explanation)))
                    .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(tf("ai.card_meta", &[("n", &redactions), ("mode", &config::get().permissions.mode.label())]))),
                div().flex().gap_2()
                    .child(btn("ai-cancel", t("ai.btn_cancel"), 0).on_click(cx.listener(|v, _, _w, cx| v.ai_cancel(cx))))
                    .child(btn("ai-exec", t("ai.btn_exec"), 0).on_click(cx.listener(|v, _, _w, cx| v.ai_execute(cx))))
                    .child(btn("ai-insert", t("ai.btn_insert"), 1).on_click(cx.listener(|v, _, _w, cx| v.ai_insert(cx))))),
            AiCard::Confirm { command, reason, .. } => (c(th.warning), t("ai.card_confirm").to_string(),
                div().flex().flex_col().gap_1()
                    .child(div().px_2().py_1().rounded_md().bg(c(th.bg)).font_family(mono).text_size(px(13.)).text_color(c(th.text)).child(command))
                    .child(div().text_color(c(th.warning)).child(reason)),
                div().flex().gap_2()
                    .child(btn("ai-cancel", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { let _ = crate::audit_log().append("ai.exec.rejected", "user", &format!("host={}", v.audit_host())); v.ai_cancel(cx) })))
                    .child(btn("ai-confirm", t("ai.btn_confirm"), 2).on_click(cx.listener(|v, _, _w, cx| v.ai_confirm(cx))))),
            AiCard::Denied { command, reason } => (c(th.error), t("ai.card_denied").to_string(),
                div().flex().flex_col().gap_1()
                    .child(div().px_2().py_1().rounded_md().bg(c(th.bg)).font_family(mono).text_size(px(13.)).text_color(c(th.text)).child(command))
                    .child(div().text_color(c(th.error)).child(reason)),
                div().flex().gap_2().child(btn("ai-cancel", t("ai.btn_close"), 0).on_click(cx.listener(|v, _, _w, cx| v.ai_cancel(cx))))),
            AiCard::Error { request, message } => (c(th.error), if request.is_empty() { t("ai.card_hint").to_string() } else { tf("ai.card_error", &[("req", &request)]) },
                div().text_color(c(th.error)).child(message),
                div().flex().gap_2().child(btn("ai-cancel", t("ai.btn_close"), 0).on_click(cx.listener(|v, _, _w, cx| v.ai_cancel(cx))))),
            AiCard::Consent { request } => (c(th.warning), t("ai.consent_title").to_string(),
                div().text_color(c(th.subtext)).child(self.ai_consent_text()),
                div().flex().gap_2()
                    .child(btn("ai-cancel", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| v.ai_cancel(cx))))
                    .child(btn("ai-consent", t("ai.consent_ok"), 1).on_click(cx.listener(move |v, _, _w, cx| { crate::ai_consent_ack(); v.ai_request(request.clone(), cx) })))),
        };
        Some(div().id("ai-card").occlude().absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(border)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .flex().flex_col().gap_2().text_size(px(12.)).text_color(c(th.text))
            .child(div().flex().items_center().gap_2().child(crate::ui::icon(crate::ui::IconName::Sparkles, 13., c(th.accent)))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title)))
            .child(body)
            .child(div().flex().items_center().gap_3().child(div().flex_1().min_w_0().text_size(px(11.)).text_color(c(th.overlay0)).child(t("ai.card_keys"))).child(buttons.flex_shrink_0()))
            .into_any_element())
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
        let hist = self.session.as_ref().map(|s| s.term.lock().unwrap().grid().history_size()).unwrap_or(0);
        if hist == 0 { return false; }
        let b = self.st.borrow().bounds;
        // The track is the rightmost SCROLLBAR_W px. A wider slop would land on the last cell
        // now that the gutter is no longer kept empty.
        p.x >= b.origin.x + b.size.width - px(SCROLLBAR_W) && p.x <= b.origin.x + b.size.width
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
                let block_mod = if config::get().terminal.block_select == "ctrl" { ev.modifiers.control } else { ev.modifiers.alt };
                st.sel_block = block_mod && ev.click_count < 2;
                st.sel = if ev.click_count >= 2 { Some(((p.0, 0), (p.0, usize::MAX >> 1))) } else { Some((p, p)) };
                drop(st);
                self.selecting = ev.click_count < 2;
                self.grid.update(cx, |_, cx| cx.notify());
            }
            MouseButton::Middle => self.paste(cx),
            MouseButton::Right => self.open_ctx_menu(ev.position, cell, cx),
            _ => {}
        }
    }

    // ---------------- right-click context menu (PRD 5.20)
    fn ssh_target(&self) -> Option<SshTarget> { match &self.spec { Spec::Ssh(t) => Some(t.clone()), Spec::Local => None } }
    fn ctx_place(&self) -> ssh_term_proto::ctxmenu::Place { ssh_term_proto::ctxmenu::Place::Terminal { remote: matches!(self.spec, Spec::Ssh(_)) } }
    /// Run `cmd` (no newline) once the shell is connected. Retries until then.
    pub fn queue_cd(&mut self, cmd: String, cx: &mut Context<Self>) {
        self.pending_cd = Some(cmd);
        cx.spawn(async move |this, cx| {
            for _ in 0..24 {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let done = this.update(cx, |v, _| v.try_flush_cd()).unwrap_or(true);
                if done { return; }
            }
        }).detach();
    }
    /// true when there is nothing left to send.
    fn try_flush_cd(&mut self) -> bool {
        if self.pending_cd.is_none() { return true; }
        let Some(s) = self.session.clone() else { return true; };
        if !s.connected.load(Ordering::Acquire) || s.exited.load(Ordering::Acquire) { return s.exited.load(Ordering::Acquire); }
        if let Some(cmd) = self.pending_cd.take() { s.write(format!("{cmd}\r").into_bytes()); }
        true
    }
    /// Shell cwd: OSC 7 → local /proc probe → `user@host:dir$` prompt on the cursor line.
    pub fn shell_cwd(&self) -> Option<String> {
        let s = self.session.as_ref()?;
        if let Some(c) = s.osc7_cwd.lock().unwrap().clone() { return Some(c); }
        if let Some(c) = s.cwd_probe.lock().unwrap().as_ref().and_then(|f| f()) { return Some(c); }
        let t = s.term.lock().unwrap();
        let g = t.grid();
        ssh_term_proto::ctxmenu::prompt_cwd(&row_chars(g, g.cursor.point.line.0))
    }
    /// Enter `path` in this shell. Clears a half-typed line first.
    pub fn cd_to(&mut self, path: &str, cx: &mut Context<Self>) {
        let cmd = cd_command_for(path);
        let Some(s) = self.session.clone() else { return };
        if !s.connected.load(Ordering::Acquire) || s.exited.load(Ordering::Acquire) {
            self.queue_cd(cmd, cx);
            return;
        }
        let mut bytes = ssh_term_proto::ctxmenu::insert_bytes(&cmd);
        bytes[0] = self.kill_line_byte();
        bytes.push(b'\r');
        s.write(bytes);
        self.tracker.key(Key::KillLine);
        self.tracker.key(Key::Text(&cmd));
        self.tracker.key(Key::Enter);
        self.last_command = Some(cmd.clone());
        let _ = crate::audit_log().append("cwd.cd", "user", &cmd);
        self.scroll_to_bottom(cx);
        cx.notify();
    }
    /// Text of a screen row with wide-char spacers removed, and the char index of `col`.
    fn line_at(&self, (row, col): (usize, usize)) -> Option<(String, usize)> {
        let s = self.session.as_ref()?;
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let line = row as i32 - g.display_offset() as i32;
        let r = &g[Line(line)];
        let (mut text, mut idx) = (String::new(), 0);
        let mut n = 0;
        for c in 0..g.columns() {
            let cell = &r[Column(c)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { if c == col { idx = n.max(1) - 1; } continue; }
            if c == col { idx = n; }
            text.push(if cell.c == '\0' { ' ' } else { cell.c });
            n += 1;
        }
        Some((text, idx))
    }
    pub fn open_ctx_menu(&mut self, pos: gpui::Point<Pixels>, cell: (usize, usize), cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::{detect_at, resolve, FileObj, Found, Target};
        let sel = self.selection_text().filter(|s| !s.trim().is_empty());
        let mut note = None;
        let target = if let Some(sel) = sel { Target::Selection(sel) } else {
            let mut blank = || {
                let paste = self.clipboard_paste_text(cx).filter(|t| !t.trim().is_empty());
                if paste.is_none() { note = Some(t("cm.clipboard_empty").to_string()); }
                Target::Blank { paste }
            };
            match self.line_at(cell) {
                None => blank(),
                Some((line, ci)) => match detect_at(&line, ci).map(|d| d.found) {
                    Some(Found::Url(u)) => Target::Url(u),
                    Some(Found::HostPort { host, port }) => Target::HostPort { host, port },
                    Some(Found::Path { path, .. }) => {
                        let cwd = self.shell_cwd();
                        let home = matches!(self.spec, Spec::Local).then(|| std::env::var("USERPROFILE").ok().or_else(|| std::env::var("HOME").ok())).flatten();
                        let (p, ok) = resolve(&path, cwd.as_deref(), home.as_deref());
                        if !ok { note = Some(t("cm.cwd_unknown").to_string()); }
                        let is_dir = path.ends_with('/');
                        let p = if p.len() > 1 { p.trim_end_matches('/').to_string() } else { p };
                        Target::Files(vec![FileObj { path: p, is_dir, mode: 0 }])
                    }
                    None => blank(),
                },
            }
        };
        let key = crate::ctxmenu_view::host_key(self.ssh_target().as_ref());
        self.ctx_menu = Some(crate::ctxmenu_view::build(target, self.ctx_place(), &self.audit_host(), key, pos, note));
        if crate::chat_view::dock_open() {
            if let Some(m) = &mut self.ctx_menu {
                let group = m.menu.items.last().map(|i| i.group.saturating_add(1)).unwrap_or(0);
                m.menu.items.push(ssh_term_proto::ctxmenu::MenuItem {
                    id: "ai_context".into(), label: t("cm.use_ai_context").into(), act: ssh_term_proto::ctxmenu::Act::UseAiContext,
                    missing: vec![], hint: None, unknown: false, custom: false, group,
                });
            }
        }
        if let Some(rx) = crate::ctxmenu_view::ensure_probe(self.ssh_target()) {
            cx.spawn(async move |this, cx| {
                let _ = rx.recv_async().await;
                let _ = this.update(cx, |v, cx| { if let Some(m) = &v.ctx_menu { let (pl, h) = (v.ctx_place(), v.audit_host()); v.ctx_menu = Some(m.rebuilt(pl, &h)); cx.notify(); } });
            }).detach();
        }
        cx.notify();
    }
    fn ctx_pick(&mut self, i: usize, _w: &mut Window, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::Act;
        let Some(m) = self.ctx_menu.take() else { return };
        let Some(item) = m.menu.items.get(i).cloned() else { return };
        cx.emit(TermEvent::Audit("ctxmenu.pick", format!("host={} item={} custom={}", self.audit_host(), item.id, item.custom)));
        match item.act {
            Act::Insert(cmd) => self.insert_command(&cmd, cx),
            Act::Copy(text) => {
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    cx.emit(TermEvent::Copied);
                }
            }
            Act::OpenEditor(p) => cx.emit(TermEvent::CtxEditor(p)),
            Act::CdNewTab(d) => cx.emit(TermEvent::CtxCdTab(d)),
            Act::OpenUrl(u) => cx.open_url(&u),
            Act::Search(q) => { self.search = Some(SearchBar { query: q.replace('\n', " "), cur: None, found: None }); self.search_step(false, cx); }
            Act::AiExplain(text) => cx.emit(TermEvent::CtxAiExplain(text)),
            Act::OpenTool(id, text) => cx.emit(TermEvent::CtxOpenTool(id, text)),
            Act::PasteInput(text) => self.insert_text(&text),
            Act::UseAiContext => cx.emit(TermEvent::CtxUseAi),
            Act::Download(_) | Act::Upload(_) => {}
        }
        cx.notify();
    }
    /// Type text into the input line as one line, without Enter.
    pub fn insert_text(&mut self, text: &str) {
        if let Some(s) = self.session.clone() { let one = text.replace(['\r', '\n'], " "); s.write(one.clone().into_bytes()); self.tracker.key(Key::Text(&one)); }
    }
    fn ctx_close(&mut self, cx: &mut Context<Self>) { if self.ctx_menu.take().is_some() { cx.notify(); } }
    pub fn ready(&self) -> bool { self.session.is_some() }
    /// Multi-line insert is only safe when the shell will not execute the pasted newlines.
    pub fn can_paste_block(&self) -> bool { self.ready() && self.mode().contains(TermMode::BRACKETED_PASTE) }

    /// Permission verdict for a saved script. Any deny stops the whole script; any ask waits for the panel.
    pub fn gate_script(&self, lines: &[String]) -> ScriptGate {
        let cfg = config::get();
        let max = match cfg.permissions.max_auto_risk.as_str() { "R0" => Risk::R0, "R2" => Risk::R2, _ => Risk::R1 };
        let prod = cfg.permissions.prod_confirm && matches!(&self.spec, Spec::Ssh(t) if t.production);
        let mut asks = vec![];
        for line in lines {
            let (eff, why) = policy::decide_host(cfg.permissions.mode, max, &cfg.permissions.rules, "user:snippet", "exec", line, cfg.permissions.dangerous_confirm, prod, &self.audit_host());
            let risk = policy::classify(line);
            let _ = crate::audit_log().append("snippet.decision", "policy", &format!("host={} risk={} effect={eff:?} command={:?} reason={why:?}", self.audit_host(), risk.label(), ssh_term_proto::ai::redact(line).0));
            match eff {
                Effect::Deny => return ScriptGate::Deny { line: line.clone(), why },
                Effect::Ask => asks.push(format!("{line} — {why}")),
                Effect::Allow => {}
            }
        }
        if asks.is_empty() { ScriptGate::Allow } else { ScriptGate::Ask(asks) }
    }

    /// Clear the prompt, then send each line with Enter. Caller has already passed the permission gate.
    pub fn run_script_lines(&mut self, lines: &[String], cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let mut bytes = vec![self.kill_line_byte()];
        for line in lines {
            let one = line.replace(['\r', '\n'], " ");
            bytes.extend(one.as_bytes());
            bytes.push(b'\r');
            self.history.lock().unwrap().add(&one);
            if config::get().audit.log_commands { let _ = crate::audit_log().append("command", "user", &ssh_term_proto::ai::redact(&one).0); }
            let _ = crate::audit_log().append("snippet.exec", "user", &format!("host={} command={:?}", self.audit_host(), ssh_term_proto::ai::redact(&one).0));
            self.tracker.key(Key::KillLine);
            self.tracker.key(Key::Text(&one));
            self.tracker.key(Key::Enter);
            if self.agent.is_none() { self.cmd_started = Some((Instant::now(), one.clone())); self.last_command = Some(one); }
        }
        s.write(bytes);
        self.scroll_to_bottom(cx);
        cx.notify();
    }

    /// Insert a multi-line body without executing it. False when bracketed paste is off or there is no session.
    pub fn insert_block(&mut self, body: &str, cx: &mut Context<Self>) -> bool {
        if !self.can_paste_block() { return false; }
        let Some(s) = self.session.clone() else { return false; };
        let mut bytes = vec![self.kill_line_byte()];
        bytes.extend(paste_bytes(body, true));
        s.write(bytes);
        self.tracker.key(Key::KillLine);
        self.tracker.key(Key::Other);
        cx.emit(TermEvent::Audit("snippet.insert", format!("host={} chars={}", self.audit_host(), body.chars().count())));
        self.scroll_to_bottom(cx);
        cx.notify();
        true
    }

    /// Put a command on the input line without running it (Ctrl+U clears what was typed first).
    pub fn insert_command(&mut self, cmd: &str, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let one = cmd.replace(['\r', '\n'], " ");
        { let mut b = ssh_term_proto::ctxmenu::insert_bytes(&one); b[0] = self.kill_line_byte(); s.write(b); }
        self.tracker.key(Key::KillLine);
        self.tracker.key(Key::Text(&one));
        self.ctx_pending = Some(one.clone());
        self.ctx_denied = None;
        cx.emit(TermEvent::Audit("ctxmenu.insert", format!("host={} command={:?}", self.audit_host(), ssh_term_proto::ai::redact(&one).0)));
        self.scroll_to_bottom(cx);
        cx.notify();
    }
    /// Permission engine for a context-menu command on Enter. true = submit now.
    fn ctx_gate(&mut self, cmd: String, cx: &mut Context<Self>) -> bool {
        let cfg = config::get();
        let max = match cfg.permissions.max_auto_risk.as_str() { "R0" => Risk::R0, "R2" => Risk::R2, _ => Risk::R1 };
        let prod = cfg.permissions.prod_confirm && matches!(&self.spec, Spec::Ssh(t) if t.production);
        let (eff, why) = policy::decide_host(cfg.permissions.mode, max, &cfg.permissions.rules, "user:ctxmenu", "exec", &cmd, cfg.permissions.dangerous_confirm, prod, &self.audit_host());
        let risk = policy::classify(&cmd);
        cx.emit(TermEvent::Audit("ctxmenu.decision", format!("host={} risk={} effect={eff:?} command={:?} reason={why:?}", self.audit_host(), risk.label(), ssh_term_proto::ai::redact(&cmd).0)));
        match eff {
            Effect::Allow => true,
            Effect::Ask => { self.ctx_confirm = Some((cmd, format!("{} · {why}", risk.label()))); cx.notify(); false }
            Effect::Deny => { self.ctx_denied = Some(tf("cm.denied", &[("cmd", &cmd), ("why", &why)])); cx.notify(); false }
        }
    }
    /// Press Enter for the pending context-menu command (normal history / audit bookkeeping).
    fn ctx_submit(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let cmd = self.line_state().and_then(|ls| self.tracker.submitted_input(&ls.before_cursor, ls.prompt_col)).or_else(|| self.ctx_pending.clone()).unwrap_or_default();
        if !cmd.is_empty() {
            self.history.lock().unwrap().add(&cmd);
            if config::get().audit.log_commands { let _ = crate::audit_log().append("command", "user", &ssh_term_proto::ai::redact(&cmd).0); }
            if self.agent.is_none() { self.cmd_started = Some((Instant::now(), cmd.clone())); self.last_command = Some(cmd.clone()); }
        }
        cx.emit(TermEvent::Audit("ctxmenu.exec", format!("host={} command={:?}", self.audit_host(), ssh_term_proto::ai::redact(&cmd).0)));
        self.tracker.key(Key::Enter);
        self.ctx_pending = None; self.ctx_confirm = None;
        self.scroll_to_bottom(cx);
        fanout(&s, b"\r".to_vec());
        cx.notify();
    }

    fn link_at(&self, (row, col): (usize, usize)) -> Option<String> {
        let s = self.session.as_ref()?;
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let line = row as i32 - g.display_offset() as i32;
        if let Some(h) = g[Line(line)][Column(col)].hyperlink() { return Some(h.uri().to_string()); }
        ssh_term_proto::term::url_at(&row_chars(g, line), col).map(|(_, _, u)| u)
    }

    fn link_span(&self, (row, col): (usize, usize)) -> Option<(i32, usize, usize)> {
        let s = self.session.as_ref()?;
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let line = row as i32 - g.display_offset() as i32;
        let r = &g[Line(line)];
        let Some(h) = r[Column(col)].hyperlink() else {
            return ssh_term_proto::term::url_at(&row_chars(g, line), col).map(|(a, b, _)| (line, a, b));
        };
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

/// Result of judging every line of a saved script before anything is written to the terminal.
pub enum ScriptGate {
    Allow,
    Ask(Vec<String>),
    Deny { line: String, why: String },
}

/// Bytes to send for a paste. Bracketed paste wraps the text and strips any embedded
/// ESC so pasted content can't terminate the bracket early; otherwise newlines become CR.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    // 统一换行为 \r，并去除除 Tab/换行外的控制字符（C0、DEL、C1、ESC），避免粘贴时被终端程序解释成乱码或按键序列
    let norm: String = text.replace("\r\n", "\r").replace('\n', "\r").chars()
        .filter(|&c| c == '\t' || c == '\r' || !(c.is_control()))
        .filter(|&c| !matches!(c, '\u{feff}' | '\u{fffd}'))
        .collect();
    if bracketed { format!("\x1b[200~{norm}\x1b[201~").into_bytes() } else { norm.into_bytes() }
}

/// xterm mouse encoding. `button`: 0/1/2 = L/M/R, 3 = release (X10), 64/65 = wheel, +32 = motion,
/// modifiers already added. Rows/cols are 0-based screen cells.
fn is_windows_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// `(label, path)` from the root down to `path`, so a breadcrumb can cd to any prefix.
pub(crate) fn cwd_crumbs(path: &str) -> Vec<(String, String)> {
    let path = path.trim();
    if path.is_empty() { return vec![]; }
    if is_windows_path(path) {
        let parts: Vec<&str> = path.trim_end_matches(['\\', '/']).split(['\\', '/']).filter(|s| !s.is_empty()).collect();
        let mut acc = String::new();
        let mut out = Vec::with_capacity(parts.len());
        for (i, part) in parts.into_iter().enumerate() {
            if i == 0 { acc = format!("{part}\\"); }
            else {
                if !acc.ends_with('\\') { acc.push('\\'); }
                acc.push_str(part);
            }
            out.push((part.to_string(), acc.clone()));
        }
        return out;
    }
    let abs = path.starts_with('/');
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    let mut acc = String::new();
    if abs { out.push(("/".into(), "/".into())); acc = "/".into(); }
    for part in parts {
        acc = if acc == "/" { format!("/{part}") } else if acc.is_empty() { part.to_string() } else { format!("{acc}/{part}") };
        out.push((part.to_string(), acc.clone()));
    }
    out
}

fn cd_command_for(path: &str) -> String {
    if is_windows_path(path) { format!("Set-Location -LiteralPath '{}'", path.replace('\'', "''")) }
    else { format!("cd -- {}", ssh_term_proto::ctxmenu::quote_path(path)) }
}

pub fn encode_mouse(button: u8, pressed: bool, row: usize, col: usize, sgr: bool) -> Vec<u8> {
    if sgr {
        format!("\x1b[<{};{};{}{}", button, col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
    } else {
        let b = if pressed { button } else { 3 + (button & !3) };
        let enc = |v: usize| (32 + 1 + v).min(255) as u8;
        vec![0x1b, b'[', b'M', 32 + b, enc(col), enc(row)]
    }
}

/// True when this key copies the terminal selection instead of going to the shell.
/// macOS uses ⌘C. Windows and Linux use Ctrl+C. Every OS also accepts Ctrl+Shift+C.
/// A selection is required, so Ctrl+C with nothing selected still interrupts the program.
pub fn should_copy_selection(macos: bool, key: &str, control: bool, platform: bool, shift: bool, alt: bool, has_selection: bool) -> bool {
    if !has_selection || alt || !key.eq_ignore_ascii_case("c") { return false; }
    if control && shift && !platform { return true; }
    if macos && platform && !control && !shift { return true; }
    if !macos && control && !platform && !shift { return true; }
    false
}

/// True when this key pastes the clipboard into the terminal.
/// macOS uses ⌘V. Windows and Linux use Ctrl+V. Every OS also accepts Ctrl+Shift+V.
pub fn should_paste(macos: bool, key: &str, control: bool, platform: bool, shift: bool, alt: bool) -> bool {
    if alt || !key.eq_ignore_ascii_case("v") { return false; }
    if control && shift && !platform { return true; }
    if macos && platform && !control && !shift { return true; }
    if !macos && control && !platform && !shift { return true; }
    false
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

// ---------------- broadcast input (P2): one keystroke stream to every pane in the group
static BROADCAST: std::sync::Mutex<Vec<std::sync::Weak<Session>>> = std::sync::Mutex::new(Vec::new());
/// Replace the broadcast group (empty = off).
pub fn set_broadcast(members: &[Arc<Session>]) { *BROADCAST.lock().unwrap() = members.iter().map(Arc::downgrade).collect(); }
pub fn broadcast_count() -> usize { BROADCAST.lock().unwrap().iter().filter(|w| w.strong_count() > 0).count() }
pub fn in_broadcast(s: &Arc<Session>) -> bool { BROADCAST.lock().unwrap().iter().any(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(s))) }
/// Write user input: to every live member when `s` is broadcasting, else to `s` only.
fn fanout(s: &Arc<Session>, bytes: Vec<u8>) {
    if bytes.contains(&b'\r') { if let Some(n) = s.notifier.lock().unwrap().as_mut() { n.skip = 1; } }
    let members: Vec<Arc<Session>> = { let g = BROADCAST.lock().unwrap(); if !g.iter().any(|w| std::ptr::eq(w.as_ptr(), Arc::as_ptr(s))) { drop(g); return s.write(bytes); } g.iter().filter_map(|w| w.upgrade()).collect() };
    for m in members { if !m.exited.load(Ordering::Acquire) { m.write(bytes.clone()); } }
}

impl Render for TermView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        if let Some(e) = &self.embed {
            return div().track_focus(&self.focus).flex_1().size_full().flex().flex_col().overflow_hidden().child(e.view());
        }
        if let Some(v) = self.db.clone() {
            return div().track_focus(&self.focus).flex_1().size_full().flex().flex_col().overflow_hidden().child(v);
        }
        if let Some(v) = self.sftp.clone() {
            return div().track_focus(&self.focus).flex_1().size_full().flex().flex_col().overflow_hidden().child(v);
        }
        let mut body = div().flex_1().size_full().relative()
            .child(self.grid.clone().cached(StyleRefinement::default().size_full()))
            .child(div().absolute().top_0().left_0().size_full().child(self.cursor.clone()));
        if let Some(sess) = &self.session {
            let rec = sess.is_recording();
            let bc = in_broadcast(sess);
            if bc {
                body = body.child(div().absolute().bottom(px(6.)).left(px(8.)).right(px(8.)).h(px(24.)).px_3().rounded_md().flex().items_center().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD)
                    .bg(c(th.error).opacity(0.92)).text_color(c(th.base)).child(tf("bc.banner", &[("n", &broadcast_count())])));
                body = body.child(div().absolute().top_0().left_0().size_full().border_2().border_color(c(th.error)));
            }
            if rec {
                body = body.child(div().absolute().top(px(4.)).right(px(16.)).px_2().py(px(1.)).rounded_md().bg(c(th.error).opacity(0.12)).border_1().border_color(c(th.error))
                    .text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.error)).child(t("rec.indicator")));
            }
        }
        let connecting = matches!(self.spec, Spec::Ssh(_)) && !self.embedded() && self.session.as_ref().is_some_and(|s| !s.connected.load(Ordering::Acquire) && !s.exited.load(Ordering::Acquire) && s.conn_error.lock().unwrap().is_none());
        if connecting {
            let delay = Duration::from_millis(crate::motion::PROGRESS_DELAY_MS);
            if self.born.elapsed() >= delay {
                let host = match &self.spec { Spec::Ssh(t) => t.label(), _ => String::new() };
                let bar = div().h(px(2.)).w(px(96.)).rounded_full().bg(c(th.accent));
                let bar: gpui::AnyElement = if crate::motion::reduced() { bar.into_any_element() } else {
                    bar.with_animation("conn-progress", gpui::Animation::new(Duration::from_millis(1200)).repeat().with_max_fps(30.).with_easing(gpui::ease_in_out), |d, t| d.ml(px(t * 264.))).into_any_element()
                };
                body = body.child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center()
                    .child(div().w(px(360.)).p_4().rounded(px(10.)).bg(c(th.mantle)).border_1().border_color(c(th.surface2)).shadow_lg().flex().flex_col().gap_2()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(format!("{}…", t("ui.connecting"))))
                        .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(host))
                        .child(div().mt_1().h(px(2.)).w_full().rounded_full().bg(c(th.surface1)).overflow_hidden().child(bar))));
            } else if !self.progress_tick {
                self.progress_tick = true;
                let wait = delay.saturating_sub(self.born.elapsed());
                cx.spawn(async move |this, cx| { cx.background_executor().timer(wait).await; let _ = this.update(cx, |v, cx| { v.progress_tick = false; cx.notify(); }); }).detach();
            }
        }
        let fail = self.error.clone().or_else(|| self.session.as_ref().and_then(|s| s.conn_error.lock().unwrap().clone()));
        if let (Some(e), false) = (fail, self.embedded()) {
            let is_ssh = matches!(self.spec, Spec::Ssh(_));
            let mut actions = div().flex().items_center().gap_2().mt(px(12.))
                .child(crate::ui::button("conn-retry", t("term.retry"), 1).on_click(cx.listener(|_, _, _, cx| cx.emit(TermEvent::RetryConnect))));
            if is_ssh { actions = actions.child(crate::ui::button("conn-edit", t("term.edit_host"), 0).on_click(cx.listener(|_, _, _, cx| cx.emit(TermEvent::EditHost)))); }
            body = body.child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().bg(c(th.base).opacity(0.6))
                .child(div().w(px(420.)).max_w(gpui::relative(0.9)).p_4().rounded(px(10.)).bg(c(th.mantle)).border_1().border_color(c(th.surface2)).shadow_lg().flex().flex_col()
                    .child(div().flex().items_center().gap_2().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text))
                        .child(div().size(px(8.)).rounded_full().bg(c(th.error))).child(t("ui.connection_failed")))
                    .child(div().mt(px(8.)).text_size(px(12.)).line_height(px(18.)).text_color(c(th.subtext)).child(e))
                    .child(actions)));
        }
        if let Some(text) = &self.pending_paste {
            let lines = text.trim_end().lines().count();
            let preview: String = text.lines().next().unwrap_or("").chars().take(60).collect();
            let btn = |id: &'static str, label: &'static str, primary: bool| div().id(id).px_3().py(px(4.)).rounded_md().cursor_pointer().text_size(px(12.))
                .bg(if primary { c(th.accent) } else { c(th.surface0) }).text_color(if primary { c(th.base) } else { c(th.text) }).child(label);
            body = body.child(div().absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.warning))
                .flex().items_center().gap_3().text_size(px(12.)).text_color(c(th.text))
                .child(div().flex_1().flex().flex_col().gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(tf("term.paste_confirm", &[("lines", &lines)])))
                    .child(div().text_color(c(th.overlay1)).child(format!("{preview} …"))))
                .child(btn("paste-cancel", t("ui.cancel"), false).on_click(cx.listener(|v, _, _w, cx| { v.pending_paste = None; cx.notify(); })))
                .child(btn("paste-ok", t("ui.paste"), true).on_click(cx.listener(|v, _, _w, cx| { if let Some(t) = v.pending_paste.take() { v.paste_text(t, cx); } cx.notify(); }))));
        }
        {
            // IME: register this pane as the text input handler while it has focus
            let (focus, me) = (self.focus.clone(), cx.entity());
            body = body.child(canvas(|_, _, _| {}, move |bounds, _, window, cx| { window.handle_input(&focus, gpui::ElementInputHandler::new(bounds, me.clone()), cx); }).absolute().size_full());
        }
        if let Some(card) = self.ai_card(cx) { body = body.child(card); }
        if let Some(cmd) = self.resume_offer.clone().filter(|_| config::get().session.prompt_resume_agent) {
            body = body.child(div().absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_2().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.accent))
                .flex().flex_wrap().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                .child(tf("term.restored_resume", &[("cmd", &cmd)]))
                .child(div().id("resume-run").px_3().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.accent)).text_color(c(th.base)).child(t("term.resume_agent"))
                    .on_click(cx.listener(|v, _, _w, cx| { if let (Some(cmd), Some(s)) = (v.resume_offer.take(), v.session.clone()) { s.write(format!("{cmd}\r").into_bytes()); } cx.notify(); })))
                .child(div().id("resume-dismiss").px_2().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.surface0)).child(t("term.dismiss"))
                    .on_click(cx.listener(|v, _, _w, cx| { v.resume_offer = None; cx.notify(); }))));
        }
        if let Some(bar) = self.search.clone() {
            let status = match bar.found { Some(true) => t("term.search_found"), Some(false) => t("term.search_none"), None => t("term.search_hint") };
            body = body.child(div().id("search-bar").occlude().absolute().top(px(6.)).right(px(16.)).w(px(360.)).p_2().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.accent))
                .flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                .child(crate::ui::icon(crate::ui::IconName::Search, 13., c(th.overlay1)))
                .child(div().flex_1().px_1().rounded_sm().bg(c(th.base)).font_family(SharedString::from(config::get().font.family.clone())).child(format!("{}▏", bar.query)))
                .child(div().text_color(if bar.found == Some(false) { c(th.error) } else { c(th.subtext) }).child(status))
                .child(div().id("search-prev").px_1().cursor_pointer().child("↑").on_click(cx.listener(|v, _, _w, cx| v.search_step(false, cx))))
                .child(div().id("search-next").px_1().cursor_pointer().child("↓").on_click(cx.listener(|v, _, _w, cx| v.search_step(true, cx))))
                .child(div().id("search-close").px_1().cursor_pointer().child("✕").on_click(cx.listener(|v, _, _w, cx| { v.search = None; cx.notify(); }))));
        }
        if let Some(text) = self.clip_ask.clone() {
            let preview: String = text.chars().take(80).collect::<String>().replace(['\n', '\r'], " ");
            let large = ssh_term_proto::term::osc52_is_large(text.len());
            let ask = if large { tf("term.osc52_ask_large", &[("n", &text.len()), ("preview", &preview)]) } else { tf("term.osc52_ask", &[("n", &text.len()), ("preview", &preview)]) };
            body = body.child(div().absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_2().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.warning))
                .flex().flex_wrap().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                .child(ask)
                .child(div().id("osc52-allow").px_3().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.accent)).text_color(c(th.base)).child(t("ui.allow"))
                    .on_click(cx.listener(|v, _, _w, cx| { if let Some(t) = v.clip_ask.take() { v.commit_clip_write(t, cx); } cx.notify(); })))
                .child(div().id("osc52-deny").px_2().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.surface0)).child(t("ui.deny"))
                    .on_click(cx.listener(|v, _, _w, cx| { if let Some(t) = v.clip_ask.take() { cx.emit(TermEvent::Audit("osc52.write", format!("rejected {} bytes", t.len()))); } cx.notify(); }))));
        }
        if let Some((text, enter, _, _)) = self.pty_ask.clone() {
            let preview: String = text.chars().take(120).collect::<String>().replace(['\n', '\r'], " ");
            let agent = self.agent.map(|(i, _)| agent::display_name(&self.manifests[i].id).to_string()).unwrap_or_default();
            body = body.child(div().absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_2().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.warning))
                .flex().flex_wrap().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                .child(tf(if enter { "term.pty_ask_run" } else { "term.pty_ask_insert" }, &[("agent", &agent), ("preview", &preview)]))
                .child(div().id("pty-allow").px_3().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.accent)).text_color(c(th.base)).child(t("ui.allow"))
                    .on_click(cx.listener(|v, _, _w, cx| v.pty_answer(true, cx))))
                .child(div().id("pty-deny").px_2().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.surface0)).child(t("ui.deny"))
                    .on_click(cx.listener(|v, _, _w, cx| v.pty_answer(false, cx)))));
        }
        if let Some((cmd, why)) = self.ctx_confirm.clone() {
            body = body.child(div().absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_2().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.warning))
                .flex().flex_wrap().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                .child(tf("cm.confirm", &[("cmd", &cmd), ("why", &why)]))
                .child(div().id("cm-run").px_3().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.accent)).text_color(c(th.base)).child(t("cm.run_now"))
                    .on_click(cx.listener(|v, _, _w, cx| { if let Some((cmd, _)) = v.ctx_confirm.clone() { cx.emit(TermEvent::Audit("ctxmenu.approval", format!("approved command={:?}", ssh_term_proto::ai::redact(&cmd).0))); } v.ctx_submit(cx); })))
                .child(div().id("cm-cancel").px_2().py(px(3.)).rounded_md().cursor_pointer().bg(c(th.surface0)).child(t("ui.cancel"))
                    .on_click(cx.listener(|v, _, _w, cx| { cx.emit(TermEvent::Audit("ctxmenu.approval", "rejected".into())); v.ctx_confirm = None; cx.notify(); }))));
        }
        if let Some(msg) = self.ctx_denied.clone() {
            body = body.child(div().id("cm-denied").absolute().bottom(px(12.)).left(px(12.)).right(px(12.)).p_2().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.error))
                .text_size(px(12.)).text_color(c(th.error)).cursor_pointer().child(msg).on_click(cx.listener(|v, _, _w, cx| { v.ctx_denied = None; cx.notify(); })));
        }
        if let Some(m) = &self.ctx_menu {
            body = body.child(crate::ctxmenu_view::render(m, cx, Self::ctx_pick, Self::ctx_close));
        } else if let Some(e) = self.hist_pop_el(_window, cx) {
            body = body.child(e);
        }
        if self.bell_until.is_some_and(|t| t > Instant::now()) {
            body = body.child(div().absolute().top_0().left_0().size_full().border_2().border_color(c(th.warning)).bg(c(th.warning).opacity(0.06)));
        }
        // 命令失败时的“AI 解释”入口：浮在终端右下角，不占布局空间；智能体状态已显示在标签上
        if let Some((_cmd, code)) = self.failed.clone() {
            body = body.child(div().absolute().bottom(px(10.)).right(px(14.)).flex().items_center().gap_1().pl_2().pr_1().py(px(3.)).rounded_full()
                .bg(c(th.base)).border_1().border_color(c(th.error).opacity(0.4)).shadow_md().text_size(px(12.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(div().id("explain-fail").cursor_pointer().text_color(c(th.error)).hover(|s| s.underline())
                    .child(tf("ai.explain_btn", &[("code", &code)]))
                    .on_click(cx.listener(|_v, _, _w, cx| { cx.stop_propagation(); cx.emit(TermEvent::Explain); })))
                .child(div().id("explain-fail-x").px_1().rounded_full().cursor_pointer().text_color(c(th.overlay1)).hover(|s| s.text_color(c(th.text))).child("×")
                    .on_click(cx.listener(|v, _, _w, cx| { cx.stop_propagation(); v.failed = None; cx.notify(); }))));
        }
        div()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|v, ev: &KeyDownEvent, w, cx| {
                let k = &ev.keystroke;
                if k.key == "tab" && !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform {
                    v.on_key(ev, w, cx);
                    w.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(|v, ev, w, cx| v.on_mouse_down(MouseButton::Left, ev, w, cx)))
            .on_mouse_down(MouseButton::Middle, cx.listener(|v, ev, w, cx| v.on_mouse_down(MouseButton::Middle, ev, w, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(|v, ev, w, cx| v.on_mouse_down(MouseButton::Right, ev, w, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|v, ev, w, cx| v.on_mouse_up(MouseButton::Left, ev, w, cx)))
            .on_mouse_up(MouseButton::Middle, cx.listener(|v, ev, w, cx| v.on_mouse_up(MouseButton::Middle, ev, w, cx)))
            .on_mouse_up(MouseButton::Right, cx.listener(|v, ev, w, cx| v.on_mouse_up(MouseButton::Right, ev, w, cx)))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .flex_1().size_full().flex().flex_col().bg(c(scheme().bg).opacity(config::get().appearance.opacity.clamp(0.5, 1.0))).overflow_hidden()
            .child(body)
    }
}

/// Short system sound (notify.sound / terminal.bell = "sound"). Best effort, never blocks the UI.
pub fn play_sound() {
    std::thread::spawn(|| {
        #[cfg(target_os = "macos")]
        let tries: &[&[&str]] = &[&["afplay", "/System/Library/Sounds/Ping.aiff"]];
        #[cfg(not(target_os = "macos"))]
        let tries: &[&[&str]] = &[&["canberra-gtk-play", "-i", "bell"], &["paplay", "/usr/share/sounds/freedesktop/stereo/bell.oga"], &["aplay", "-q", "/usr/share/sounds/alsa/Front_Center.wav"]];
        for t in tries {
            if std::process::Command::new(t[0]).args(&t[1..]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success()) { return; }
        }
        eprint!("\x07");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hist_pop_keys() {
        // nothing selected on open: Enter runs the typed line, Tab / Right go to the shell
        assert_eq!(pop_key(None, 3, "enter"), PopKey::CloseAndPass);
        assert_eq!(pop_key(None, 3, "tab"), PopKey::Pass);
        assert_eq!(pop_key(None, 3, "right"), PopKey::Pass);
        assert_eq!(pop_key(None, 3, "a"), PopKey::Pass);
        // Up/Down select (wrapping), then Enter / Tab fill
        assert_eq!(pop_key(None, 3, "down"), PopKey::Select(0));
        assert_eq!(pop_key(None, 3, "up"), PopKey::Select(2));
        assert_eq!(pop_key(Some(2), 3, "down"), PopKey::Select(0));
        assert_eq!(pop_key(Some(0), 3, "up"), PopKey::Select(2));
        assert_eq!(pop_key(Some(1), 3, "enter"), PopKey::Accept(1));
        assert_eq!(pop_key(Some(1), 3, "tab"), PopKey::Accept(1));
        assert_eq!(pop_key(Some(1), 3, "right"), PopKey::Pass);
        assert_eq!(pop_key(None, 3, "escape"), PopKey::Close);
        assert_eq!(pop_key(Some(1), 3, "escape"), PopKey::Close);
        assert_eq!(pop_key(None, 0, "enter"), PopKey::Pass);
    }
    #[test]
    fn hist_pop_highlight_ranges() {
        assert_eq!(char_to_byte_ranges("git 状态 x", &[(4, 6)]), vec![4..10]);
        assert_eq!(char_to_byte_ranges("ls", &[(0, 2), (5, 9), (1, 1)]), vec![0..2]);
    }
    #[test]
    fn paste_bytes_are_utf8() {
        let s = "计划已经写成任务书：/Users/x/docs/tasks/01.1-人物与场景视觉升级.md。";
        assert_eq!(paste_bytes(s, false), s.as_bytes());
        assert_eq!(paste_bytes(s, true), format!("\x1b[200~{s}\x1b[201~").into_bytes());
    }
    #[test]
    fn bracketed_paste_strips_esc() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb");
        assert_eq!(paste_bytes("x\x1b[201~rm -rf /\n", true), b"\x1b[200~x[201~rm -rf /\r\x1b[201~");
        assert_eq!(paste_bytes("a\r\nb\x00\x08\x7f\u{9b}c\t中文😀", true), "\x1b[200~a\rbc\t中文😀\x1b[201~".as_bytes());
    }
    #[test]
    fn mouse_encoding() {
        assert_eq!(encode_mouse(0, true, 4, 9, true), b"\x1b[<0;10;5M");
        assert_eq!(encode_mouse(0, false, 4, 9, true), b"\x1b[<0;10;5m");
        assert_eq!(encode_mouse(0, true, 0, 0, false), vec![0x1b, b'[', b'M', 32, 33, 33]);
        assert_eq!(encode_mouse(0, false, 0, 0, false)[3], 35);
        assert_eq!(encode_mouse(64, true, 1, 1, true), b"\x1b[<64;2;2M");
    }
    #[test]
    fn scrollbar_sits_in_padding_until_it_would_cover_cells() {
        assert_eq!(scrollbar_reserve(8.0, 0), 0.0);
        assert_eq!(scrollbar_reserve(8.0, 40), 0.0);
        assert_eq!(scrollbar_reserve(0.0, 0), 0.0);
        assert_eq!(scrollbar_reserve(0.0, 40), SCROLLBAR_W);
        assert_eq!(scrollbar_reserve(3.0, 10), SCROLLBAR_W - 3.0);
        assert_eq!(scrollbar_reserve(40.0, 10), 0.0);
    }
    #[test]
    fn crumbs_keep_each_prefix() {
        assert_eq!(cwd_crumbs("/var/log"), vec![("/".into(), "/".into()), ("var".into(), "/var".into()), ("log".into(), "/var/log".into())]);
        assert_eq!(cwd_crumbs("D:\\a\\b"), vec![("D:".into(), "D:\\".into()), ("a".into(), "D:\\a".into()), ("b".into(), "D:\\a\\b".into())]);
        assert_eq!(cd_command_for("/var/log"), "cd -- /var/log");
        assert_eq!(cd_command_for(r"D:\a b"), "Set-Location -LiteralPath 'D:\\a b'");
    }
    #[test]
    fn copy_shortcut_follows_the_selection_and_the_os() {
        let copy = |macos, key, control, platform, shift, alt, sel| should_copy_selection(macos, key, control, platform, shift, alt, sel);
        assert!(copy(true, "c", false, true, false, false, true));
        assert!(!copy(true, "c", false, true, false, false, false));
        assert!(!copy(true, "c", true, false, false, false, true));
        assert!(copy(true, "c", true, false, true, false, true));
        assert!(copy(false, "c", true, false, false, false, true));
        assert!(!copy(false, "c", true, false, false, false, false));
        assert!(copy(false, "C", true, false, true, false, true));
        assert!(!copy(false, "c", true, false, false, true, true));
        assert!(!copy(false, "c", false, true, false, false, true));
        assert!(!copy(false, "v", true, false, false, false, true));
    }
    #[test]
    fn paste_shortcut_follows_the_os() {
        let paste = |macos, key, control, platform, shift, alt| should_paste(macos, key, control, platform, shift, alt);
        assert!(paste(true, "v", false, true, false, false));
        assert!(!paste(true, "v", true, false, false, false));
        assert!(paste(true, "v", true, false, true, false));
        assert!(paste(false, "v", true, false, false, false));
        assert!(paste(false, "V", true, false, true, false));
        assert!(!paste(false, "v", false, true, false, false));
        assert!(!paste(false, "v", true, false, false, true));
        assert!(!paste(true, "c", false, true, false, false));
    }
}

/// Non-terminal content a layout leaf can host.
#[derive(Clone)]
pub enum Embed {
    Kv(Entity<crate::kv_view::KvView>),
    Dock(Entity<crate::dock_view::DockView>),
    Log(Entity<crate::dock_view::LogView>),
    Mongo(Entity<crate::doc_view::DocView>),
    Gone(Entity<crate::kv_ui::GoneView>),
    Tool(Entity<crate::tools_view::ToolView>),
}

impl Embed {
    pub fn view(&self) -> gpui::AnyView { match self { Embed::Kv(v) => v.clone().into(), Embed::Dock(v) => v.clone().into(), Embed::Log(v) => v.clone().into(), Embed::Mongo(v) => v.clone().into(), Embed::Gone(v) => v.clone().into(), Embed::Tool(v) => v.clone().into() } }
    pub fn title(&self, cx: &gpui::App) -> String { match self { Embed::Kv(v) => v.read(cx).title.clone(), Embed::Dock(v) => v.read(cx).title.clone(), Embed::Log(v) => v.read(cx).title.clone(), Embed::Mongo(v) => v.read(cx).title.clone(), Embed::Gone(v) => v.read(cx).what.clone(), Embed::Tool(v) => v.read(cx).title.clone() } }
    pub fn crumbs(&self, cx: &gpui::App) -> String { match self { Embed::Kv(v) => v.read(cx).crumbs(), Embed::Dock(v) => v.read(cx).crumbs(), Embed::Log(v) => v.read(cx).title.clone(), Embed::Mongo(v) => v.read(cx).crumbs(), Embed::Gone(v) => v.read(cx).what.clone(), Embed::Tool(v) => v.read(cx).title.clone() } }
    pub fn badge(&self) -> &'static str { match self { Embed::Kv(_) => "Redis", Embed::Dock(_) | Embed::Log(_) => "Docker", Embed::Mongo(_) => "MongoDB", Embed::Gone(_) => "—", Embed::Tool(_) => t("tool.tools") } }
    /// Layout entry; `None` for a placeholder (the tab is dropped on save).
    pub fn save(&self, cx: &gpui::App) -> Option<ssh_term_proto::session_store::PaneSave> { match self { Embed::Kv(v) => Some(v.read(cx).save()), Embed::Dock(v) => Some(v.read(cx).save()), Embed::Log(v) => Some(v.read(cx).save()), Embed::Mongo(v) => Some(v.read(cx).save()), Embed::Gone(v) => v.read(cx).save.clone(), Embed::Tool(_) => None } }
}
