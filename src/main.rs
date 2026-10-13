//! ssh-term-proto v3 UI: host sidebar, tab bar, split panes, status bar, themes, font fallback,
//! wide chars and history autosuggestions. Terminal grid painted by our own canvas element.
mod config;
mod theme;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::TermMode;
use gpui::{
    canvas, div, fill, point, prelude::*, px, size, App, Bounds, Context, Entity, FocusHandle, Font,
    FontWeight, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, SharedString,
    TextAlign, TextRun, Window, WindowAppearance, WindowBounds, WindowOptions,
};
use ssh_term_proto::suggest::{History, InputTracker, Key};
use ssh_term_proto::{local, ssh, term::{Session, Size}};
use theme::{c, cur, term_color, Appearance, Theme};

const PAD: f32 = 10.0;

static FONT: std::sync::OnceLock<config::FontConfig> = std::sync::OnceLock::new();
fn fcfg() -> &'static config::FontConfig { FONT.get_or_init(config::FontConfig::platform_default) }
fn font_size() -> f32 { fcfg().size }
fn line_h() -> f32 { (fcfg().size * fcfg().line_height).round() }

/// Terminal font: primary family + fallback chain, ligatures off (calt/liga/dlig = 0).
fn mono_font(bold: bool) -> Font {
    let fc = fcfg();
    let mut f = gpui::font(SharedString::from(fc.family.clone()));
    if !fc.ligatures {
        f.features = gpui::FontFeatures(Arc::new(vec![("calt".into(), 0), ("liga".into(), 0), ("dlig".into(), 0)]));
    }
    f.fallbacks = Some(gpui::FontFallbacks(Arc::new(fc.fallbacks.clone())));
    if bold { f.weight = FontWeight::BOLD; }
    f
}
fn run(len: usize, color: Hsla, bold: bool) -> TextRun {
    TextRun { len, font: mono_font(bold), color, background_color: None, underline: None, strikethrough: None }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind { Local, Ssh }

// ---------------------------------------------------------------- terminal pane
struct TermView {
    title: SharedString,
    kind: Kind,
    session: Option<Arc<Session>>,
    error: Option<String>,
    focus: FocusHandle,
    cell_w: Rc<Cell<f32>>,
    origin: Rc<Cell<gpui::Point<Pixels>>>,
    blink_on: bool,
    sel: Option<((usize, usize), (usize, usize))>,
    selecting: bool,
    history: Arc<Mutex<History>>,
    tracker: InputTracker,
    suggestion: Option<String>,
}

/// Snapshot of what the input line looks like right now.
struct LineState { before_cursor: String, at_eol: bool, prompt_col: Option<usize>, alt_screen: bool }

impl TermView {
    fn new(title: &str, kind: Kind, session: anyhow::Result<Arc<Session>>, history: Arc<Mutex<History>>, cx: &mut Context<Self>) -> Self {
        let (session, error) = match session { Ok(s) => (Some(s), None), Err(e) => (None, Some(format!("{e:#}"))) };
        if let Some(s) = session.clone() {
            cx.spawn(async move |this, cx| {
                let mut tick = 0u32;
                loop {
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                    tick += 1;
                    let blink = tick % 33 == 0;
                    let dirty = s.dirty.swap(false, Ordering::AcqRel);
                    if dirty || blink {
                        let ok = this.update(cx, |v, cx| {
                            if blink { v.blink_on = !v.blink_on; }
                            if dirty { v.blink_on = true; }
                            cx.notify();
                        });
                        if ok.is_err() { break; }
                    }
                }
            }).detach();
        }
        Self {
            title: title.to_string().into(), kind, session, error, focus: cx.focus_handle(),
            cell_w: Rc::new(Cell::new(8.0)), origin: Rc::new(Cell::new(point(px(0.), px(0.)))),
            blink_on: true, sel: None, selecting: false, history, tracker: InputTracker::new(), suggestion: None,
        }
    }

    fn status(&self, t: &Theme) -> (&'static str, Hsla) {
        match &self.session {
            Some(s) if s.exited.load(Ordering::Acquire) => ("已断开", c(t.overlay0)),
            Some(_) => ("已连接", c(t.success)),
            None => ("连接失败", c(t.error)),
        }
    }

    fn cell_at(&self, p: gpui::Point<Pixels>) -> (usize, usize) {
        let o = self.origin.get();
        let x = (f32::from(p.x - o.x) / self.cell_w.get()).max(0.0) as usize;
        let y = (f32::from(p.y - o.y) / line_h()).max(0.0) as usize;
        (y, x)
    }

    fn line_state(&self) -> Option<LineState> {
        let s = self.session.as_ref()?;
        let t = s.term.lock().unwrap();
        let g = t.grid();
        let cur = g.cursor.point;
        // logical line = cursor row plus any soft-wrapped rows above it (WRAPLINE on their last cell)
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
        let ls = self.line_state()?;
        if ls.alt_screen || !ls.at_eol { return None; }
        let (input, _) = self.tracker.current_input(&ls.before_cursor, ls.prompt_col)?;
        self.history.lock().unwrap().suggest(&input)
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.session.clone() else { return };
        let k = &ev.keystroke;
        let m = &k.modifiers;
        let plain = !m.control && !m.alt && !m.platform && !m.shift;
        // accept suggestion: Right or Tab when a suggestion is visible; otherwise the key goes to the shell
        if plain && (k.key == "right" || k.key == "tab") {
            if let Some(sug) = self.compute_suggestion() {
                s.write(sug.clone().into_bytes());
                self.tracker.key(Key::Text(&sug));
                self.suggestion = None;
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        let app_cursor = s.term.lock().unwrap().mode().contains(TermMode::APP_CURSOR);
        let Some(bytes) = key_to_bytes(k, app_cursor) else { return };
        // update input tracking / history
        match k.key.as_str() {
            "enter" => {
                if let Some(ls) = self.line_state() {
                    if !ls.alt_screen {
                        if let Some(cmd) = self.tracker.submitted_input(&ls.before_cursor, ls.prompt_col) { self.history.lock().unwrap().add(&cmd); }
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
        self.sel = None;
        s.write(bytes);
        cx.stop_propagation();
    }
}

fn key_to_bytes(k: &gpui::Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
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
        "delete" => return Some(b"\x1b[3~".to_vec()),
        _ => {}
    }
    if m.control && k.key.len() == 1 {
        let b = k.key.as_bytes()[0].to_ascii_lowercase();
        if b.is_ascii_lowercase() { return Some(vec![b - b'a' + 1]); }
        return match b { b'[' => Some(vec![0x1b]), b'\\' => Some(vec![0x1c]), b']' => Some(vec![0x1d]), _ => None };
    }
    if m.platform { return None; }
    let text = k.key_char.clone().or_else(|| (k.key == "space").then(|| " ".to_string()))?;
    let mut out = Vec::new();
    if m.alt { out.push(0x1b); }
    out.extend_from_slice(text.as_bytes());
    Some(out)
}

fn in_sel(sel: Option<((usize, usize), (usize, usize))>, l: usize, col: usize) -> bool {
    let Some((a, b)) = sel else { return false };
    let (s, e) = if a <= b { (a, b) } else { (b, a) };
    (l, col) >= s && (l, col) <= e
}

impl Render for TermView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        // seed history once the backend has fetched ~/.bash_history (local file or remote via exec channel)
        if let Some(s) = &self.session {
            if let Some(seed) = s.history_seed.lock().unwrap().take() {
                self.history.lock().unwrap().extend_from_bash_history(&seed);
            }
        }
        self.suggestion = self.compute_suggestion();
        let focused = self.focus.is_focused(window);
        let session = self.session.clone();
        let cell_w_rc = self.cell_w.clone();
        let origin_rc = self.origin.clone();
        let show_blink = self.blink_on;
        let sel = self.sel;
        let suggestion = self.suggestion.clone();
        let th2 = th.clone();

        let grid = canvas(
            move |bounds, window, _cx| {
                let ts = window.text_system();
                let fid = ts.resolve_font(&mono_font(false));
                let cw = ts.advance(fid, px(font_size()), 'M').map(|a| f32::from(a.width)).unwrap_or(8.0);
                cell_w_rc.set(cw);
                let cols = ((f32::from(bounds.size.width) - 2.0 * PAD) / cw).floor().max(10.0) as usize;
                let rows = ((f32::from(bounds.size.height) - 2.0 * PAD) / line_h()).floor().max(3.0) as usize;
                if let Some(s) = &session { s.resize(Size { cols, rows }); }
                (session, cw)
            },
            move |bounds, (session, cw), window, cx| {
                let th = th2;
                let lh = line_h();
                let base = c(th.bg);
                window.paint_quad(fill(bounds, base));
                let Some(s) = session else { return };
                let origin = bounds.origin + point(px(PAD), px(PAD));
                origin_rc.set(origin);
                let t = s.term.lock().unwrap();
                let g = t.grid();
                let cursor = g.cursor.point;
                let show_cursor = t.mode().contains(TermMode::SHOW_CURSOR);
                let ncols = g.columns();
                for l in 0..g.screen_lines() {
                    let row = &g[Line(l as i32)];
                    let y = origin.y + px(l as f32 * lh);
                    let mut text = String::new();
                    let mut runs: Vec<TextRun> = Vec::new();
                    // wide glyphs (CJK/emoji) are drawn separately into a 2-cell slot
                    let mut wide: Vec<(usize, String, Hsla, bool)> = Vec::new();
                    let mut bg_start = 0usize;
                    let mut bg_cur: Option<Hsla> = None;
                    let push = |text: &mut String, runs: &mut Vec<TextRun>, ch: char, fg: Hsla, bold: bool| {
                        let len = ch.len_utf8();
                        text.push(ch);
                        match runs.last_mut() {
                            Some(r) if r.color == fg && (r.font.weight == FontWeight::BOLD) == bold => r.len += len,
                            _ => runs.push(run(len, fg, bold)),
                        }
                    };
                    for col in 0..=ncols {
                        let mut bg = base;
                        if col < ncols {
                            let cell = &row[Column(col)];
                            let mut fg = term_color(&th, cell.fg, true);
                            bg = term_color(&th, cell.bg, false);
                            if cell.flags.contains(Flags::DIM) { fg = fg.opacity(0.65); }
                            if cell.flags.contains(Flags::INVERSE) { std::mem::swap(&mut fg, &mut bg); }
                            if in_sel(sel, l, col) { bg = c(th.selection); }
                            let bold = cell.flags.contains(Flags::BOLD);
                            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                                // second half of a wide char: its placeholder space was already pushed
                            } else if cell.flags.contains(Flags::WIDE_CHAR) {
                                let mut g = String::new();
                                g.push(cell.c);
                                if let Some(z) = cell.zerowidth() { g.extend(z.iter()); }
                                wide.push((col, g, fg, bold));
                                push(&mut text, &mut runs, ' ', fg, bold);
                                push(&mut text, &mut runs, ' ', fg, bold);
                            } else {
                                let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' { ' ' } else { cell.c };
                                push(&mut text, &mut runs, ch, fg, bold);
                            }
                        }
                        let bg_opt = (bg != base && col < ncols).then_some(bg);
                        if col == ncols || bg_opt != bg_cur {
                            if let Some(b) = bg_cur {
                                let x0 = origin.x + px(bg_start as f32 * cw);
                                window.paint_quad(fill(Bounds::new(point(x0, y), size(px((col - bg_start) as f32 * cw), px(lh))), b));
                            }
                            bg_cur = bg_opt;
                            bg_start = col;
                        }
                    }
                    if !text.trim_end().is_empty() {
                        let shaped = window.text_system().shape_line(text.into(), px(font_size()), &runs, Some(px(cw)));
                        let _ = shaped.paint(point(origin.x, y), px(lh), TextAlign::Left, None, window, cx);
                    }
                    for (col, g, fg, bold) in wide {
                        let shaped = window.text_system().shape_line(g.clone().into(), px(font_size()), &[run(g.len(), fg, bold)], None);
                        let w = f32::from(shaped.width).min(2.0 * cw);
                        let x = origin.x + px(col as f32 * cw + (2.0 * cw - w) / 2.0);
                        let _ = shaped.paint(point(x, y), px(lh), TextAlign::Left, None, window, cx);
                    }
                }
                // autosuggestion (grey) right after the cursor
                if let Some(sug) = &suggestion {
                    let x = origin.x + px(cursor.column.0 as f32 * cw);
                    let y = origin.y + px(cursor.line.0 as f32 * lh);
                    let max = ncols.saturating_sub(cursor.column.0);
                    let sug: String = sug.chars().take(max).collect();
                    if !sug.is_empty() {
                        let sh = window.text_system().shape_line(sug.clone().into(), px(font_size()), &[run(sug.len(), c(th.suggestion), false)], Some(px(cw)));
                        let _ = sh.paint(point(x, y), px(lh), TextAlign::Left, None, window, cx);
                    }
                }
                if show_cursor && cursor.line.0 >= 0 {
                    let x = origin.x + px(cursor.column.0 as f32 * cw);
                    let y = origin.y + px(cursor.line.0 as f32 * lh);
                    let cell = &g[cursor.line][cursor.column];
                    let w = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 * cw } else { cw };
                    let b = Bounds::new(point(x, y + px(1.)), size(px(w), px(lh - 2.)));
                    if focused {
                        if show_blink {
                            window.paint_quad(fill(b, c(th.cursor)));
                            let ch = if cell.c != ' ' && cell.c != '\0' { Some(cell.c) } else { suggestion.as_ref().and_then(|s| s.chars().next()) };
                            if let Some(ch) = ch {
                                let sh = window.text_system().shape_line(ch.to_string().into(), px(font_size()), &[run(ch.len_utf8(), c(th.cursor_text), false)], Some(px(w)));
                                let _ = sh.paint(point(x, y), px(lh), TextAlign::Left, None, window, cx);
                            }
                        }
                    } else {
                        window.paint_quad(gpui::outline(b, c(th.overlay1), gpui::BorderStyle::Solid));
                    }
                }
            },
        )
        .size_full();

        let mut body = div().flex_1().size_full().relative().child(grid);
        if let Some(e) = &self.error {
            body = body.child(div().absolute().top(px(16.)).left(px(16.)).right(px(16.)).p_3().rounded_md()
                .bg(c(th.mantle)).border_1().border_color(c(th.error)).text_color(c(th.error)).text_sm().child(format!("连接失败：{e}")));
        }

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, w, cx| {
                w.focus(&this.focus, cx);
                let p = this.cell_at(ev.position);
                this.sel = Some((p, p)); this.selecting = true; cx.notify();
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.selecting && ev.pressed_button == Some(MouseButton::Left) {
                    let p = this.cell_at(ev.position);
                    if let Some((a, _)) = this.sel { this.sel = Some((a, p)); cx.notify(); }
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _w, cx| {
                this.selecting = false;
                if let Some((a, b)) = this.sel { if a == b { this.sel = None; } }
                cx.notify();
            }))
            .flex_1().h_full().flex().flex_col().bg(c(th.bg)).overflow_hidden()
            .border_t_2().border_color(if focused { c(th.accent) } else { c(th.bg) })
            .child(
                div().h(px(26.)).px_3().flex().items_center().gap_2().bg(c(th.mantle)).text_size(px(11.5))
                    .text_color(if focused { c(th.text) } else { c(th.overlay1) })
                    .child(div().size(px(7.)).rounded_full().bg(self.status(&th).1))
                    .child(self.title.clone()),
            )
            .child(body)
    }
}

// ---------------------------------------------------------------- app shell
struct Host { name: SharedString, detail: SharedString, kind: Kind, online: bool }
struct Tab { title: SharedString, panes: Vec<Entity<TermView>> }

#[derive(Clone, Copy, PartialEq)]
enum ThemeMode { System, Fixed(usize) }

struct Root {
    rt: Arc<tokio::runtime::Runtime>,
    hosts: Vec<Host>,
    tabs: Vec<Tab>,
    active: usize,
    themes: Vec<Arc<Theme>>,
    mode: ThemeMode,
    histories: HashMap<String, Arc<Mutex<History>>>,
    _appearance_sub: Option<gpui::Subscription>,
}

fn ssh_target() -> ssh::SshTarget {
    ssh::SshTarget {
        host: std::env::var("SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("SSH_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(22),
        user: std::env::var("SSH_USER").unwrap_or_else(|_| std::env::var("USER").unwrap_or_default()),
        key: std::env::var("SSH_KEY").map(Into::into).unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".ssh/id_ed25519")
        }),
    }
}

impl Root {
    fn history_for(&mut self, key: &str) -> Arc<Mutex<History>> {
        self.histories.entry(key.to_string()).or_insert_with(|| Arc::new(Mutex::new(History::new()))).clone()
    }
    fn open_pane(&mut self, kind: Kind, cx: &mut Context<Self>) -> Entity<TermView> {
        let init = Size { cols: 80, rows: 24 };
        match kind {
            Kind::Local => {
                let h = self.history_for("local");
                cx.new(|cx| TermView::new("本地 · bash", Kind::Local, local::spawn_local(init, None), h, cx))
            }
            Kind::Ssh => {
                let t = ssh_target();
                let title = format!("{}@{}:{}", t.user, t.host, t.port);
                let h = self.history_for(&title);
                let s = ssh::spawn_ssh(self.rt.handle(), t, init);
                cx.new(|cx| TermView::new(&title, Kind::Ssh, s, h, cx))
            }
        }
    }
    fn new_tab(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.open_pane(kind, cx);
        let title = pane.read(cx).title.clone();
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(Tab { title, panes: vec![pane] });
        self.active = self.tabs.len() - 1;
        window.focus(&fh, cx);
        cx.notify();
    }
    fn focused_pane(&self, window: &Window, cx: &App) -> Option<Entity<TermView>> {
        let tab = self.tabs.get(self.active)?;
        tab.panes.iter().find(|p| p.read(cx).focus.is_focused(window)).cloned().or_else(|| tab.panes.first().cloned())
    }
    fn apply_theme(&self, window: &Window) {
        let idx = match self.mode {
            ThemeMode::Fixed(i) => i,
            ThemeMode::System => {
                let want = match window.appearance() { WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark, _ => Appearance::Light };
                self.themes.iter().position(|t| t.appearance == want).unwrap_or(0)
            }
        };
        theme::set_current(self.themes[idx].clone());
    }
    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cur_idx = self.themes.iter().position(|t| t.name == cur().name).unwrap_or(0);
        self.mode = ThemeMode::Fixed((cur_idx + 1) % self.themes.len());
        self.apply_theme(window);
        self.refresh_all(cx);
    }
    fn refresh_all(&self, cx: &mut Context<Self>) {
        for p in self.tabs.iter().flat_map(|t| &t.panes) { p.update(cx, |_, cx| cx.notify()); }
        cx.notify();
    }
}

fn icon_badge(letter: &str, color: Hsla) -> impl IntoElement {
    div().size(px(26.)).rounded_md().flex().items_center().justify_center()
        .bg(color.opacity(0.18)).text_color(color).text_size(px(12.)).font_weight(FontWeight::BOLD).child(letter.to_string())
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        for h in &mut self.hosts {
            h.online = self.tabs.iter().flat_map(|t| &t.panes).any(|p| {
                let v = p.read(cx);
                v.kind == h.kind && v.session.as_ref().is_some_and(|s| !s.exited.load(Ordering::Acquire))
            });
        }

        let mut sidebar = div().w(px(232.)).h_full().flex().flex_col().bg(c(th.crust)).border_r_1().border_color(c(th.surface0))
            .child(div().h(px(44.)).px_4().flex().items_center().gap_2()
                .child(div().size(px(10.)).rounded_full().bg(c(th.accent)))
                .child(div().text_color(c(th.text)).font_weight(FontWeight::SEMIBOLD).text_size(px(13.)).child("AgentShell")))
            .child(div().px_4().pt_2().pb_1().text_size(px(10.5)).text_color(c(th.overlay0)).child("主机"));
        for (i, h) in self.hosts.iter().enumerate() {
            let kind = h.kind;
            let accent = if kind == Kind::Local { c(th.info) } else { c(th.warning) };
            let dot = if h.online { c(th.success) } else { c(th.surface2) };
            sidebar = sidebar.child(
                div().id(("host", i)).mx_2().px_2().py(px(6.)).rounded_lg().flex().items_center().gap_3().cursor_pointer()
                    .hover(|s| s.bg(c(cur().surface0)))
                    .on_click(cx.listener(move |this, _, w, cx| this.new_tab(kind, w, cx)))
                    .child(icon_badge(if kind == Kind::Local { "L" } else { "S" }, accent))
                    .child(div().flex_1().flex().flex_col().overflow_hidden()
                        .child(div().text_size(px(12.5)).text_color(c(th.text)).child(h.name.clone()))
                        .child(div().text_size(px(10.5)).text_color(c(th.overlay0)).child(h.detail.clone())))
                    .child(div().size(px(8.)).rounded_full().bg(dot)),
            );
        }
        sidebar = sidebar.child(div().flex_1()).child(
            div().m_3().p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface0)).text_size(px(11.)).text_color(c(th.subtext))
                .flex().flex_col().gap_1()
                .child("点击主机打开新标签")
                .child("Ctrl+Shift+T 切换主题")
                .child("→ / Tab 接受灰色建议"));

        let mut tabbar = div().h(px(40.)).px_2().flex().items_center().gap_1().bg(c(th.crust)).border_b_1().border_color(c(th.surface0));
        for (i, t) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            tabbar = tabbar.child(
                div().id(("tab", i)).h(px(30.)).pl_3().pr_1().rounded_lg().flex().items_center().gap_2().cursor_pointer()
                    .bg(if active { c(th.base) } else { c(th.crust) })
                    .border_1().border_color(if active { c(th.surface0) } else { c(th.crust) })
                    .hover(|s| s.bg(c(cur().mantle)))
                    .on_click(cx.listener(move |this, _, _w, cx| { this.active = i; cx.notify(); }))
                    .child(div().size(px(6.)).rounded_full().bg(if active { c(th.accent) } else { c(th.surface2) }))
                    .child(div().text_size(px(12.)).text_color(if active { c(th.text) } else { c(th.overlay1) }).child(t.title.clone()))
                    .child(div().id(("close", i)).size(px(20.)).rounded_md().flex().items_center().justify_center()
                        .text_size(px(13.)).text_color(c(th.overlay0)).hover(|s| s.bg(c(cur().surface1)).text_color(c(cur().text)))
                        .on_click(cx.listener(move |this, _, _w, cx| {
                            cx.stop_propagation();
                            if this.tabs.len() > 1 { this.tabs.remove(i); this.active = this.active.min(this.tabs.len() - 1); cx.notify(); }
                        }))
                        .child("×")),
            );
        }
        tabbar = tabbar.child(
            div().id("new-tab").size(px(28.)).rounded_lg().flex().items_center().justify_center().cursor_pointer()
                .text_color(c(th.overlay1)).text_size(px(16.)).hover(|s| s.bg(c(cur().surface0)).text_color(c(cur().text)))
                .on_click(cx.listener(|this, _, w, cx| this.new_tab(Kind::Local, w, cx)))
                .child("+"),
        );

        let mut panes = div().flex_1().flex().flex_row().overflow_hidden();
        if let Some(tab) = self.tabs.get(self.active) {
            for (i, p) in tab.panes.iter().enumerate() {
                if i > 0 { panes = panes.child(div().w(px(1.)).h_full().bg(c(th.surface0))); }
                panes = panes.child(p.clone());
            }
        }

        let (status, scolor, dims, title, hist) = match self.focused_pane(window, cx) {
            Some(p) => {
                let v = p.read(cx);
                let (st, col) = v.status(&th);
                let d = v.session.as_ref().map(|s| { let z = s.size(); format!("{}×{}", z.cols, z.rows) }).unwrap_or_default();
                let n = v.history.lock().unwrap().len();
                (st, col, d, v.title.clone(), format!("历史 {n}"))
            }
            None => ("—", c(th.overlay0), String::new(), SharedString::from(""), String::new()),
        };
        let theme_label = match self.mode { ThemeMode::System => format!("◐ {}（跟随系统）", th.name), ThemeMode::Fixed(_) => format!("◐ {}", th.name) };
        let statusbar = div().h(px(26.)).px_3().flex().items_center().gap_4().bg(c(th.crust)).border_t_1().border_color(c(th.surface0))
            .text_size(px(11.)).text_color(c(th.subtext))
            .child(div().flex().items_center().gap_2().child(div().size(px(7.)).rounded_full().bg(scolor)).child(status))
            .child(title)
            .child(div().flex_1())
            .child(hist)
            .child(dims)
            .child("UTF-8")
            .child(div().id("theme-btn").px_2().rounded_md().cursor_pointer().hover(|s| s.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, w, cx| this.cycle_theme(w, cx))).child(theme_label))
            .child(div().id("theme-sys").px_2().rounded_md().cursor_pointer().hover(|s| s.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, w, cx| { this.mode = ThemeMode::System; this.apply_theme(w); this.refresh_all(cx); }))
                .child("系统"))
            .child(div().px_2().py(px(1.)).rounded_md().bg(c(th.accent).opacity(0.18)).text_color(c(th.accent)).font_weight(FontWeight::SEMIBOLD).child("AI: Ask 模式"));

        div().size_full().flex().flex_row().bg(c(th.crust)).font_family(SharedString::from("Inter"))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                let m = &ev.keystroke.modifiers;
                if m.control && m.shift && ev.keystroke.key.eq_ignore_ascii_case("t") {
                    this.cycle_theme(w, cx);
                    cx.stop_propagation();
                }
            }))
            .child(sidebar)
            .child(div().flex_1().h_full().flex().flex_col().child(tabbar).child(panes).child(statusbar))
    }
}

fn main() {
    let cfg = config::load();
    let _ = FONT.set(cfg.font.clone());
    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().unwrap());
    let t = ssh_target();
    let ssh_name: SharedString = format!("{}@{}:{}", t.user, t.host, t.port).into();
    let themes = theme::load_all();
    let mode = match cfg.theme.as_deref() {
        None | Some("system") => ThemeMode::System,
        Some(name) => themes.iter().position(|t| t.name.eq_ignore_ascii_case(name)).map(ThemeMode::Fixed).unwrap_or(ThemeMode::System),
    };
    gpui_platform::application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1440.), px(860.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                cx.new(|cx| {
                    let mut root = Root {
                        rt, hosts: vec![
                            Host { name: "本地".into(), detail: "bash · portable-pty".into(), kind: Kind::Local, online: false },
                            Host { name: ssh_name.clone(), detail: "russh · ed25519".into(), kind: Kind::Ssh, online: false },
                        ],
                        tabs: vec![], active: 0, themes, mode, histories: HashMap::new(), _appearance_sub: None,
                    };
                    root.apply_theme(window);
                    // follow OS light/dark changes while in "system" mode (hot switch, no restart)
                    root._appearance_sub = Some(cx.observe_window_appearance(window, |this: &mut Root, w, cx| {
                        if this.mode == ThemeMode::System { this.apply_theme(w); this.refresh_all(cx); }
                    }));
                    let l = root.open_pane(Kind::Local, cx);
                    let r = root.open_pane(Kind::Ssh, cx);
                    let fh = l.read(cx).focus.clone();
                    root.tabs.push(Tab { title: "本地 | sshtest".into(), panes: vec![l, r] });
                    window.focus(&fh, cx);
                    root
                })
            },
        ).unwrap();
        cx.activate(true);
    });
}
