//! 工具模块界面：一个通用视图按 `ToolSpec` 渲染输入卡片、选项与输出卡片；WebSocket 为交互式视图。
//! 视图在打开标签时创建、关闭时释放；没有后台轮询，耗时工具在后台执行器上运行。
use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Entity, EventEmitter, ExternalPaths, FontWeight, SharedString, Window};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::tools::{self, Args, FieldKind, Output, ToolSpec};

use crate::card_layout::{content_card, sub_card, sub_card_body, CARD_PAD, SUB_PAD};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use gpui_component::Sizable as _;

pub enum ToolEvent { SendToTerminal(String) }

pub struct ToolView {
    pub spec: ToolSpec,
    pub title: String,
    texts: HashMap<String, Entity<InputState>>,
    areas: HashMap<String, Entity<EditorState>>,
    picks: HashMap<String, String>,
    files: HashMap<String, Vec<PathBuf>>,
    out_ed: Entity<EditorState>,
    output: Output,
    error: Option<String>,
    notice: Option<String>,
    busy: bool,
    seq: u64,
    pending_out: Option<String>,
    ws: Option<WsState>,
    rt: tokio::runtime::Handle,
    _subs: Vec<gpui::Subscription>,
}

struct WsState { url: Entity<InputState>, msg: Entity<InputState>, tx: Option<tools::ws::WsSender>, log: Vec<(bool, String)>, open: bool }

impl EventEmitter<ToolEvent> for ToolView {}

impl ToolView {
    pub fn new(spec: ToolSpec, rt: tokio::runtime::Handle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subs = vec![];
        let (mut texts, mut areas, mut picks) = (HashMap::new(), HashMap::new(), HashMap::new());
        for f in spec.inputs.iter().chain(&spec.options) {
            match f.kind {
                FieldKind::Text | FieldKind::Number => {
                    let (d, ph) = (f.default.clone(), if f.placeholder.is_empty() { f.label.clone() } else { f.placeholder.clone() });
                    let st = cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(d));
                    subs.push(cx.subscribe_in(&st, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| match ev {
                        InputEvent::Change => if this.spec.live { this.run(cx) },
                        InputEvent::PressEnter { .. } => this.run(cx),
                        _ => {}
                    }));
                    texts.insert(f.id.clone(), st);
                }
                FieldKind::Multiline => {
                    let (d, ph) = (f.default.clone(), if f.placeholder.is_empty() { t("tools.ui_input_ph").to_string() } else { f.placeholder.clone() });
                    let st = cx.new(|cx| EditorState::new(window, cx).line_number(false).soft_wrap(true).placeholder(ph).default_value(d));
                    subs.push(cx.subscribe_in(&st, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| if matches!(ev, InputEvent::Change) && this.spec.live { this.run(cx) }));
                    areas.insert(f.id.clone(), st);
                }
                FieldKind::Select | FieldKind::Toggle => { picks.insert(f.id.clone(), f.default.clone()); }
                FieldKind::File | FieldKind::Files => {}
            }
        }
        let out_ed = cx.new(|cx| EditorState::new(window, cx).line_number(false).soft_wrap(true));
        let ws = spec.interactive.then(|| WsState {
            url: cx.new(|cx| InputState::new(window, cx).placeholder("wss://echo.websocket.org").default_value("wss://echo.websocket.org")),
            msg: cx.new(|cx| InputState::new(window, cx).placeholder(t("tools.ws_message"))),
            tx: None, log: vec![], open: false,
        });
        if let Some(w) = &ws {
            subs.push(cx.subscribe_in(&w.msg, window, |this: &mut Self, _, ev: &InputEvent, w, cx| if let InputEvent::PressEnter { .. } = ev { this.ws_send(w, cx) }));
        }
        let title = spec.name.clone();
        let mut me = Self { spec, title, texts, areas, picks, files: HashMap::new(), out_ed, output: Output::default(), error: None, notice: None, busy: false, seq: 0, pending_out: None, ws, rt, _subs: subs };
        if me.spec.live && !me.spec.interactive && !me.spec.heavy { me.run(cx); }
        me
    }

    /// 把文本放进第一个输入框并运行（来自终端“用工具打开”）。
    pub fn preset(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.spec.inputs.first().cloned() else { return };
        if let Some(a) = self.areas.get(&f.id) { a.update(cx, |e, cx| e.set_value(text, window, cx)); }
        else if let Some(s) = self.texts.get(&f.id) { s.update(cx, |e, cx| e.set_value(text, window, cx)); }
        self.run(cx);
    }

    fn args(&self, cx: &gpui::App) -> Args {
        let mut a = Args::default();
        for (k, s) in &self.texts { a.set(k, s.read(cx).value().to_string()); }
        for (k, s) in &self.areas { a.set(k, s.read(cx).value().to_string()); }
        for (k, v) in &self.picks { a.set(k, v.clone()); }
        a.files = self.files.clone();
        a
    }

    pub fn run(&mut self, cx: &mut Context<Self>) {
        if self.spec.interactive { return; }
        let args = self.args(cx);
        let id = self.spec.id.clone();
        self.seq += 1;
        let seq = self.seq;
        self.notice = None;
        if self.spec.heavy {
            if args.files.values().all(|v| v.is_empty()) && self.spec.inputs.iter().any(|f| matches!(f.kind, FieldKind::File | FieldKind::Files)) { return; }
            self.busy = true;
            cx.notify();
            let pack = self.spec.pack.clone();
            let task = cx.background_executor().spawn(async move { if pack.is_empty() { tools::run(&id, &args) } else { ssh_term_proto::ext::run_installed(&pack, &id, &args) } });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                let _ = this.update(cx, |v, cx| if v.seq == seq { v.busy = false; v.apply(r); cx.notify(); });
            }).detach();
        } else {
            let r = tools::run(&id, &args);
            self.apply(r);
            cx.notify();
        }
    }

    fn apply(&mut self, r: anyhow::Result<Output>) {
        match r {
            Ok(o) => { self.error = None; self.pending_out = Some(o.text.clone()); self.output = o; }
            Err(e) => { self.error = Some(e.to_string()); }
        }
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for s in self.texts.values() { s.update(cx, |e, cx| e.set_value("", window, cx)); }
        for s in self.areas.values() { s.update(cx, |e, cx| e.set_value("", window, cx)); }
        self.files.clear();
        self.output = Output::default();
        self.error = None;
        self.notice = None;
        self.out_ed.update(cx, |e, cx| e.set_value("", window, cx));
        if let Some(w) = &mut self.ws { w.log.clear(); }
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = if let Some(w) = &self.ws { w.log.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n") } else { self.output.text.clone() };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice = Some(t("tools.ui_copied").into());
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let (name, bytes) = match &self.output.file {
            Some((n, b)) => (n.clone(), b.clone()),
            None => (format!("{}.txt", self.spec.id), self.output.text.clone().into_bytes()),
        };
        let dir = dirs_home();
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let r = std::fs::write(&path, bytes);
            let _ = this.update(cx, |v, cx| {
                v.notice = Some(match r { Ok(()) => tf("tools.ui_saved", &[("path", &path.display())]), Err(e) => tf("tools.ui_save_failed", &[("e", &e)]) });
                cx.notify();
            });
        }).detach();
    }

    fn pick_files(&mut self, id: String, multiple: bool, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple, prompt: None });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let _ = this.update(cx, |v, cx| v.set_files(id, paths, cx));
        }).detach();
    }

    fn set_files(&mut self, id: String, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let multiple = self.spec.inputs.iter().any(|f| f.id == id && f.kind == FieldKind::Files);
        let e = self.files.entry(id).or_default();
        if multiple { e.extend(paths); } else { *e = paths.into_iter().take(1).collect(); }
        self.run(cx);
    }

    fn set_pick(&mut self, id: String, v: String, cx: &mut Context<Self>) {
        self.picks.insert(id, v);
        if self.spec.live || !self.output.text.is_empty() { self.run(cx); } else { cx.notify(); }
    }

    // ---------------- WebSocket
    fn ws_toggle(&mut self, cx: &mut Context<Self>) {
        let Some(w) = &mut self.ws else { return };
        if let Some(tx) = w.tx.take() { let _ = tx.send(None); w.open = false; cx.notify(); return; }
        let url = w.url.read(cx).value().trim().to_string();
        if url.is_empty() { return; }
        let (tx, rx) = tools::ws::connect(&self.rt, url);
        w.tx = Some(tx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv_async().await {
                let done = matches!(ev, tools::ws::WsEvent::Closed(_) | tools::ws::WsEvent::Error(_));
                let alive = this.update(cx, |v, cx| { v.ws_event(ev); cx.notify(); }).is_ok();
                if done || !alive { break; }
            }
        }).detach();
    }

    fn ws_event(&mut self, ev: tools::ws::WsEvent) {
        use tools::ws::WsEvent as E;
        let Some(w) = &mut self.ws else { return };
        let now = chrono::Local::now().format("%H:%M:%S");
        let line = match ev {
            E::Open => { w.open = true; format!("{now}  {}", t("tools.ws_open")) }
            E::Text(s) => format!("{now}  ← {s}"),
            E::Binary(n) => format!("{now}  ← {}", tf("tools.ws_binary", &[("n", &n)])),
            E::Closed(r) => { w.open = false; w.tx = None; format!("{now}  {}", tf("tools.ws_closed", &[("r", &r)])) }
            E::Error(e) => { w.open = false; w.tx = None; format!("{now}  {}", tf("tools.ws_error", &[("e", &e)])) }
        };
        w.log.push((false, line));
        if w.log.len() > 2000 { w.log.drain(..500); }
    }

    fn ws_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.ws else { return };
        let Some(tx) = &w.tx else { return };
        let text = w.msg.read(cx).value().to_string();
        if text.is_empty() || !w.open { return; }
        let _ = tx.send(Some(text.clone()));
        w.log.push((true, format!("{}  → {text}", chrono::Local::now().format("%H:%M:%S"))));
        w.msg.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn render_ws(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let w = self.ws.as_ref().unwrap();
        let connected = w.tx.is_some();
        let bar = div().flex().items_center().gap_2()
            .child(div().flex_1().child(ui::text_field(&w.url, false)))
            .child(ui::button("ws-conn", if connected { t("tools.ws_disconnect") } else { t("tools.ws_connect") }, if connected { 2 } else { 1 })
                .on_click(cx.listener(|this, _, _w, cx| this.ws_toggle(cx))));
        let send = div().flex().items_center().gap_2()
            .child(div().flex_1().child(ui::text_field(&w.msg, false)))
            .child(ui::button("ws-send", t("tools.ws_send"), 0).on_click(cx.listener(|this, _, w, cx| this.ws_send(w, cx))));
        let mut log = sub_card_body().id("ws-log").overflow_y_scroll().flex().flex_col().gap(px(2.))
            .font_family(SharedString::from(crate::config::get().font.family.clone())).text_size(px(ui::fs_small()));
        for (i, (mine, l)) in w.log.iter().enumerate() {
            log = log.child(div().id(("wsl", i)).whitespace_normal().text_color(if *mine { c(th.accent) } else { c(th.text) }).child(l.clone()));
        }
        div().flex_1().min_h_0().flex().flex_col().gap(px(CARD_PAD))
            .child(sub_card().p(px(SUB_PAD)).flex().flex_col().gap_2().child(bar).child(send))
            .child(sub_card().flex_1().min_h_0().flex().flex_col().child(self.out_head(false, cx)).child(log))
            .into_any_element()
    }

    /// 输出卡片的标题行：复制 / 保存 / 清空 / 发送到终端。
    fn out_head(&self, full: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let small = |id: &'static str, ic: IconName, tip: &'static str| div().id(id).h(px(ui::H_COMPACT)).px_2().rounded(px(ui::R_MD)).flex().items_center().gap_1().cursor_pointer()
            .text_size(px(ui::fs_small())).text_color(c(cur().subtext)).hover(|s| s.bg(crate::theme::hover_bg()))
            .child(icon(ic, ui::IC_XS, c(cur().overlay1))).child(tip);
        ui::toolbar()
            .child(div().flex_1().text_size(px(ui::fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.overlay1)).child(t("tools.ui_output")))
            .when_some(self.notice.clone(), |d, n| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.success)).child(n)))
            .when(self.busy, |d| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("tools.ui_running"))))
            .child(small("out-copy", IconName::Copy, t("tools.ui_copy")).on_click(cx.listener(|this, _, _w, cx| this.copy(cx))))
            .when(full, |d| d.child(small("out-save", IconName::Save, t("tools.ui_save")).on_click(cx.listener(|this, _, _w, cx| this.save(cx))))
                .child(small("out-term", IconName::SquareTerminal, t("tools.ui_send_term")).on_click(cx.listener(|this, _, _w, cx| { let s = this.output.text.clone(); if !s.is_empty() { cx.emit(ToolEvent::SendToTerminal(s)); } }))))
            .child(small("out-clear", IconName::Eraser, t("tools.ui_clear")).on_click(cx.listener(|this, _, w, cx| this.clear(w, cx))))
    }

    fn field_label(s: &str) -> impl IntoElement {
        div().text_size(px(ui::fs_small())).text_color(c(cur().overlay1)).child(s.to_string())
    }
}

/// 选项控件需要 `&'static str` 作为元素 id 前缀：按字段 id 驻留一次（数量有限）。
fn intern(s: &str) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static SET: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    let mut set = SET.get_or_init(Default::default).lock().unwrap();
    if let Some(v) = set.get(s) { return v; }
    let v: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(v);
    v
}

fn dirs_home() -> PathBuf { std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")) }

impl Render for ToolView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(s) = self.pending_out.take() { self.out_ed.update(cx, |e, cx| e.set_value(s, window, cx)); }
        let th = cur();
        let head = div().flex().items_center().gap_2().px_1().pb(px(CARD_PAD)).flex_shrink_0()
            .child(div().text_size(px(ui::fs_title())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.spec.name.clone()))
            .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(self.spec.desc.clone()));
        if self.spec.interactive {
            let body = self.render_ws(cx);
            return content_card("tool-card").child(head).child(body).into_any_element();
        }
        let me = cx.entity().downgrade();
        // 输入卡片
        let mut input = sub_card().p(px(SUB_PAD)).flex().flex_col().gap_2();
        let mut tall = false;
        for f in self.spec.inputs.clone() {
            let mut row = div().flex().flex_col().gap_1().child(Self::field_label(&f.label));
            match f.kind {
                FieldKind::Multiline => { tall = true; row = row.flex_1().min_h(px(90.)).child(div().flex_1().min_h(px(80.)).child(Editor::new(&self.areas[&f.id]).h_full())); }
                FieldKind::Text | FieldKind::Number => { row = row.child(ui::text_field(&self.texts[&f.id], false)); }
                FieldKind::File | FieldKind::Files => {
                    let id = f.id.clone();
                    let multiple = f.kind == FieldKind::Files;
                    let names: Vec<String> = self.files.get(&f.id).map(|v| v.iter().map(|p| p.display().to_string()).collect()).unwrap_or_default();
                    let id2 = id.clone();
                    row = row.child(div().id(SharedString::from(format!("drop-{id}"))).min_h(px(64.)).p_3().rounded(px(ui::R_MD)).border_1().border_dashed().border_color(crate::theme::divider())
                        .flex().flex_col().items_center().justify_center().gap_1().cursor_pointer().hover(|s| s.bg(crate::theme::hover_bg()))
                        .drag_over::<ExternalPaths>(|st, _, _, _| st.border_color(c(cur().accent)))
                        .on_drop(cx.listener(move |v, ps: &ExternalPaths, _, cx| v.set_files(id2.clone(), ps.paths().to_vec(), cx)))
                        .on_click(cx.listener(move |v, _, _, cx| v.pick_files(id.clone(), multiple, cx)))
                        .child(icon(IconName::FileUp, ui::IC_LG, c(th.overlay1)))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(t("tools.ui_drop_hint")))
                        .children(names.into_iter().map(|n| div().text_size(px(ui::fs_small())).text_color(c(th.text)).child(n))));
                }
                _ => {}
            }
            input = input.child(row);
        }
        if tall { input = input.flex_1().min_h_0(); }
        // 选项
        let mut opts = div().flex().flex_wrap().items_end().gap_3();
        for f in self.spec.options.clone() {
            let cur_v = self.picks.get(&f.id).cloned().unwrap_or_default();
            let el: AnyElement = match f.kind {
                FieldKind::Select => {
                    let (id, me) = (f.id.clone(), me.clone());
                    div().flex().flex_col().gap_1().child(Self::field_label(&f.label))
                        .child(div().child(ui::segmented(intern(&format!("opt-{}", f.id)), &f.choices, &cur_v, move |v, _w, cx| { let _ = me.update(cx, |this, cx| this.set_pick(id.clone(), v, cx)); })))
                        .into_any_element()
                }
                FieldKind::Toggle => {
                    let id = f.id.clone();
                    gpui_component::switch::Switch::new(SharedString::from(format!("tg-{}", f.id))).checked(cur_v == "true").label(f.label.clone()).small()
                        .on_click(cx.listener(move |this, on: &bool, _, cx| this.set_pick(id.clone(), on.to_string(), cx))).into_any_element()
                }
                FieldKind::Text | FieldKind::Number => div().w(px(140.)).flex().flex_col().gap_1().child(Self::field_label(&f.label)).child(ui::text_field(&self.texts[&f.id], false)).into_any_element(),
                _ => div().into_any_element(),
            };
            opts = opts.child(el);
        }
        let run_btn = ui::button("tool-run", if self.spec.live { t("tools.ui_refresh") } else { t("tools.ui_run") }, 1).on_click(cx.listener(|this, _, _w, cx| this.run(cx)));
        let opts_card = sub_card().p(px(SUB_PAD)).flex().items_end().gap_3().flex_shrink_0()
            .child(div().flex_1().min_w_0().child(opts)).child(run_btn);
        // 输出卡片
        let mut out = sub_card().flex().flex_col().min_h(px(120.)).child(self.out_head(true, cx));
        if let Some(e) = &self.error { out = out.child(ui::notice(ui::Notice::Error, e.clone()).mx(px(SUB_PAD)).my(px(ui::SP_2))); }
        if let Some([r, g, b, a]) = self.output.color {
            out = out.child(div().px(px(SUB_PAD)).pt(px(SUB_PAD)).flex().items_center().gap_2()
                .child(div().size(px(28.)).rounded(px(ui::R_MD)).border_1().border_color(crate::theme::divider()).bg(gpui::Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: a as f32 / 255. })));
        }
        out = out.child(sub_card_body().min_h(px(80.)).child(Editor::new(&self.out_ed).h_full()));
        out = out.flex_1().min_h_0();
        content_card("tool-card").child(head)
            .child(div().id("tool-body").flex_1().min_h_0().flex().flex_col().gap(px(CARD_PAD))
                .when(!self.spec.inputs.is_empty(), |d| d.child(input))
                .when(!self.spec.options.is_empty() || !self.spec.live, |d| d.child(opts_card))
                .child(out))
            .into_any_element()
    }
}
