//! 工具模块界面：一个通用视图按 `ToolSpec` 渲染输入卡片、选项与输出卡片；WebSocket 为交互式视图。
//! 视图在打开标签时创建、关闭时释放；没有后台轮询，耗时工具在后台执行器上运行。
//! 生成类工具的历史由 Root 按工具 id 持有（会话级、只在内存），所以关闭标签后再打开仍在。
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{div, prelude::*, px, AnyElement, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, ExternalPaths, FontWeight, SharedString, Window};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState, Textarea, TextareaState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::tools::history::History;
use ssh_term_proto::tools::{self, Args, FieldKind, InSize, OutKind, Output, ToolSpec};

use crate::result_list::{self, ClickFn, RowMode};
use crate::card_layout::{content_card, sub_card, sub_card_body, CARD_PAD, SUB_PAD};
use crate::tools_layout::{self as lay, ToolSplit};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use gpui_component::Sizable as _;

pub enum ToolEvent { SendToTerminal(String) }

pub struct ToolView {
    pub spec: ToolSpec,
    pub title: String,
    texts: HashMap<String, Entity<InputState>>,
    areas: HashMap<String, Entity<EditorState>>,
    /// 短多行输入（随内容增高）。
    tareas: HashMap<String, Entity<TextareaState>>,
    picks: HashMap<String, String>,
    files: HashMap<String, Vec<PathBuf>>,
    out_ed: Entity<EditorState>,
    output: Output,
    error: Option<String>,
    notice: Option<String>,
    busy: bool,
    seq: u64,
    pending_out: Option<String>,
    /// 生成类工具的历史（与 Root 共享）。
    hist: Rc<RefCell<History>>,
    /// 刚复制过的行（用于短暂显示对勾）与其计时序号。
    copied: Option<String>,
    copy_seq: u64,
    /// 正则工具的结果页：`match` / `replace`。
    tab: &'static str,
    /// 文件结果的图片预览（从内存加载，不落盘）。
    preview: Option<Arc<gpui::Image>>,
    /// 最近一次保存的路径（保存后才能“在文件夹中显示”）。
    saved: Option<PathBuf>,
    ws: Option<WsState>,
    /// 输入区占两个大文本区的比例（全局一个值，持久化在配置里）；拖动中以本地值为准，松手后保存。
    split: f32,
    dragging: bool,
    /// 上一次渲染是否左右并排、是否有选项卡片（拖动换算比例用）。
    side: bool,
    has_opts: bool,
    /// 画布测量：工具区宽度（决定是否并排）与选项卡片高度。
    body_w: Rc<Cell<f32>>,
    opts_h: Rc<Cell<f32>>,
    rt: tokio::runtime::Handle,
    _subs: Vec<gpui::Subscription>,
}

struct WsState { url: Entity<InputState>, msg: Entity<InputState>, tx: Option<tools::ws::WsSender>, log: Vec<(bool, String)>, open: bool }

impl EventEmitter<ToolEvent> for ToolView {}

impl ToolView {
    pub fn new(mut spec: ToolSpec, rt: tokio::runtime::Handle, hist: Rc<RefCell<History>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        tools::apply_hints(&mut spec);
        let mut subs = vec![];
        let (mut texts, mut areas, mut picks, mut tareas) = (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
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
                FieldKind::Multiline if matches!(f.size(), InSize::Auto(..)) => {
                    let (d, ph) = (f.default.clone(), if f.placeholder.is_empty() { t("tools.ui_input_ph").to_string() } else { f.placeholder.clone() });
                    let InSize::Auto(lo, hi) = f.size() else { unreachable!() };
                    let st = cx.new(|cx| TextareaState::new(window, cx).placeholder(ph).auto_grow(lo as usize, hi as usize));
                    if !d.is_empty() { st.update(cx, |e, cx| e.set_value(d, window, cx)); }
                    subs.push(cx.subscribe_in(&st, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| if matches!(ev, InputEvent::Change) && this.spec.live { this.run(cx) }));
                    tareas.insert(f.id.clone(), st);
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
        let spec_id = spec.id.clone();
        let mut me = Self { spec, title, texts, areas, tareas, picks, files: HashMap::new(), out_ed, output: Output::default(), error: None, notice: None, busy: false, seq: 0, pending_out: None, hist, copied: None, copy_seq: 0, tab: if spec_id == "md_html" { "preview" } else { "match" }, preview: None, saved: None, ws, split: lay::clamp_split(crate::config::get().sidebar.tools_split), dragging: false, side: false, has_opts: false, body_w: Rc::new(Cell::new(0.)), opts_h: Rc::new(Cell::new(0.)), rt, _subs: subs };
        if me.spec.live && !me.spec.interactive && !me.spec.heavy { me.run(cx); }
        me
    }

    /// 把文本放进第一个输入框并运行（来自终端“用工具打开”）。
    pub fn preset(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.spec.inputs.first().cloned() else { return };
        self.set_field(&f.id, text, window, cx);
        self.run(cx);
    }

    /// 把文本写进某个文本输入（单行、大文本或短多行）。
    fn set_field(&mut self, id: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.areas.get(id) { a.update(cx, |e, cx| e.set_value(text, window, cx)); }
        else if let Some(a) = self.tareas.get(id) { a.update(cx, |e, cx| e.set_value(text, window, cx)); }
        else if let Some(s) = self.texts.get(id) { s.update(cx, |e, cx| e.set_value(text, window, cx)); }
    }

    fn args(&self, cx: &gpui::App) -> Args {
        let mut a = Args::default();
        for (k, s) in &self.texts { a.set(k, s.read(cx).value().to_string()); }
        for (k, s) in &self.areas { a.set(k, s.read(cx).value().to_string()); }
        for (k, s) in &self.tareas { a.set(k, s.read(cx).value().to_string()); }
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
            Ok(o) => {
                self.error = None;
                match o.kind {
                    // 生成类：追加一批到历史，旧结果保留
                    OutKind::Rows => { self.hist.borrow_mut().push(chrono::Local::now().format("%H:%M:%S").to_string(), o.meta.clone(), o.rows.clone()); }
                    OutKind::Text => self.pending_out = Some(o.text.clone()),
                    // 带原文预览的键值结果（正则）：替换页的文本放进输出编辑器
                    OutKind::Kv => if o.source.is_some() { self.pending_out = Some(o.alt.clone().unwrap_or_default()); },
                }
                self.preview = o.info.as_ref().and_then(|i| i.preview.as_ref()).map(|(fmt, bytes)| {
                    let f = match fmt { tools::fileinfo::PreviewFmt::Png => gpui::ImageFormat::Png, tools::fileinfo::PreviewFmt::Jpeg => gpui::ImageFormat::Jpeg };
                    Arc::new(gpui::Image::from_bytes(f, bytes.clone()))
                });
                self.saved = None;
                self.output = o;
            }
            Err(e) => { self.error = Some(e.to_string()); }
        }
    }

    /// 清空输入框与文件（选项保持不变）；生成类的历史不动。
    fn clear_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.spec.inputs.iter().map(|f| f.id.clone()).collect();
        for id in &ids {
            self.set_field(id, String::new(), window, cx);
            self.files.remove(id);
        }
        if !tools::has_history(&self.spec.id) {
            self.output = Output::default();
            self.out_ed.update(cx, |e, cx| e.set_value("", window, cx));
        }
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    /// 把当前结果放回第一个输入框并重新运行（加密后再解密、格式化后再压缩等来回操作）。
    fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.output_text();
        let Some(f) = self.spec.inputs.first().cloned() else { return };
        if !self.areas.contains_key(&f.id) && !self.tareas.contains_key(&f.id) { return; }
        self.set_field(&f.id, text, window, cx);
        self.run(cx);
    }

    /// 清空历史（只清结果，输入与选项都保留）。
    fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.hist.borrow_mut().clear();
        self.output = Output::default();
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    fn clear_log(&mut self, cx: &mut Context<Self>) {
        if let Some(w) = &mut self.ws { w.log.clear(); }
        cx.notify();
    }

    /// 复制/保存/发送到终端用的文本：生成类取最新一批，其余取输出全文。
    fn output_text(&self) -> String {
        if tools::has_history(&self.spec.id) { return self.hist.borrow().latest().map(|b| b.text()).unwrap_or_default(); }
        if self.tab == "replace" && self.output.source.is_some() { return self.output.alt.clone().unwrap_or_default(); }
        self.output.text.clone()
    }

    fn copy_text(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice = Some(t("tools.ui_copied").into());
        self.copied = Some(key);
        self.copy_seq += 1;
        let seq = self.copy_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1200)).await;
            let _ = this.update(cx, |v, cx| if v.copy_seq == seq { v.copied = None; v.notice = None; cx.notify(); });
        }).detach();
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = if let Some(w) = &self.ws { w.log.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join("\n") } else { self.output_text() };
        self.copy_text("all".into(), text, cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let (name, bytes) = match &self.output.file {
            Some((n, b)) => (n.clone(), b.clone()),
            None => (format!("{}.txt", self.spec.id), self.output_text().into_bytes()),
        };
        let dir = dirs_home();
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let r = std::fs::write(&path, bytes);
            let _ = this.update(cx, |v, cx| {
                if r.is_ok() { v.saved = Some(path.clone()); }
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
        // 生成类改选项不自动再生成（否则每次改选项都会追加一批）
        if self.spec.live || (!self.output.text.is_empty() && !tools::has_history(&self.spec.id)) { self.run(cx); } else { cx.notify(); }
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
        let plain = self.output.kind == OutKind::Text && self.output.file.is_none() && !self.output.text.is_empty() && self.ws.is_none() && !tools::has_history(&self.spec.id);
        let stat = plain.then(|| tools::text_stats(&self.output.text));
        let can_swap = plain && self.spec.id != "md_html" && self.spec.inputs.first().is_some_and(|f| f.kind == FieldKind::Multiline);
        ui::toolbar()
            .child(div().flex_1().text_size(px(ui::fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.overlay1)).child(t("tools.ui_output")))
            .when_some(stat, |d, (l, n)| d.child(div().text_size(px(ui::fs_caption())).text_color(c(th.overlay1)).child(tf("tools.ui_stat", &[("lines", &l), ("chars", &n)]))))
            .when_some(self.notice.clone(), |d, n| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.success)).child(n)))
            .when(self.busy, |d| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("tools.ui_running"))))
            .child(small("out-copy", IconName::Copy, t("tools.ui_copy")).on_click(cx.listener(|this, _, _w, cx| this.copy(cx))))
            .when(full && can_swap, |d| d.child(small("out-swap", IconName::ArrowLeftRight, t("tools.ui_swap")).on_click(cx.listener(|this, _, w, cx| this.swap(w, cx)))))
            .when(full, |d| d.child(small("out-save", IconName::Save, t("tools.ui_save")).on_click(cx.listener(|this, _, _w, cx| this.save(cx))))
                .child(small("out-term", IconName::SquareTerminal, t("tools.ui_send_term")).on_click(cx.listener(|this, _, _w, cx| { let s = this.output_text(); if !s.is_empty() { cx.emit(ToolEvent::SendToTerminal(s)); } }))))
            .when(self.ws.is_some(), |d| d.child(small("out-clear", IconName::Eraser, t("tools.ui_clear")).on_click(cx.listener(|this, _, _w, cx| this.clear_log(cx)))))
            .when(self.ws.is_none() && !self.spec.inputs.is_empty(), |d| d.child(small("out-clear-in", IconName::Eraser, t("tools.ui_clear_input")).on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx)))))
            .when(tools::has_history(&self.spec.id) && !self.hist.borrow().is_empty(), |d| d.child(small("out-clear-hist", IconName::Trash, t("tools.ui_clear_history")).on_click(cx.listener(|this, _, _w, cx| this.clear_history(cx)))))
    }

    /// 生成类历史：按批次分组，新批次在最上；每条一行，可逐条复制 / 删除。
    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let hist = self.hist.clone();
        let h = hist.borrow();
        if h.is_empty() {
            return ui::empty_state(IconName::Clock, t("tools.hist_empty"), Some(t("tools.hist_empty_desc").into()), vec![], true).into_any_element();
        }
        let mut list = div().id("hist-scroll").flex_1().min_h_0().overflow_y_scroll().py(px(ui::LIST_INSET)).flex().flex_col().gap(px(ui::SP_1));
        for (bi, b) in h.batches.iter().enumerate() {
            let bid = b.id;
            let listener = |f: fn(&mut Self, u64, &mut Context<Self>)| -> ClickFn { Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| f(this, bid, cx))) };
            let text = b.text();
            list = list.child(result_list::batch_head(result_list::BatchHead {
                id: bid, time: b.time.clone(), meta: b.meta.clone(), count: b.rows.len(), folded: b.folded,
                masked: b.has_secret().then_some(b.masked),
                on_fold: listener(|v, id, cx| { v.hist.borrow_mut().toggle_fold(id); cx.notify(); }),
                on_mask: listener(|v, id, cx| { v.hist.borrow_mut().toggle_mask(id); cx.notify(); }),
                on_copy: Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| this.copy_text(format!("b{bid}"), text.clone(), cx))),
                on_delete: listener(|v, id, cx| { v.hist.borrow_mut().remove_batch(id); cx.notify(); }),
            }));
            if b.folded { continue; }
            for (ri, r) in b.rows.iter().enumerate() {
                let key = format!("r{bid}-{ri}");
                let (val, k2) = (r.value.clone(), key.clone());
                let on_copy: ClickFn = Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| this.copy_text(k2.clone(), val.clone(), cx)));
                let on_del: ClickFn = Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| { this.hist.borrow_mut().remove_row(bid, ri); cx.notify(); }));
                list = list.child(result_list::result_row(SharedString::from(format!("hr-{bid}-{ri}-{bi}")), r, RowMode::List, b.masked && r.secret,
                    self.copied.as_deref() == Some(key.as_str()), on_copy, Some(on_del)));
            }
        }
        list.into_any_element()
    }

    /// 多格式结果：每种写法一行（主结果加大加粗），可逐条复制；颜色工具在最上面带色块。
    fn render_kv(&self, fit: bool, cx: &mut Context<Self>) -> AnyElement {
        // 贴合内容：高度随行数，超出可用高度时卡片内部滚动；否则占满输出卡片
        let mut list = div().id("kv-scroll").when(!fit, |d| d.flex_1()).min_h_0().overflow_y_scroll().py(px(ui::LIST_INSET)).flex().flex_col().gap(px(ui::SP_1));
        if let Some([r, g, b, a]) = self.output.color {
            list = list.child(div().px(px(ui::LIST_INSET + ui::ROW_PX)).pb(px(ui::SP_2)).flex().items_center()
                .child(div().h(px(ui::SP_6 * 1.5)).w_full().rounded(px(ui::R_MD)).border_1().border_color(crate::theme::divider())
                    .bg(gpui::Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: a as f32 / 255. })));
        }
        for (i, r) in self.output.rows.iter().enumerate() {
            let key = format!("kv{i}");
            let (val, k2) = (r.value.clone(), key.clone());
            let on_copy: ClickFn = Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| this.copy_text(k2.clone(), val.clone(), cx)));
            list = list.child(result_list::result_row(SharedString::from(format!("kvr-{i}")), r, RowMode::Kv, false, self.copied.as_deref() == Some(key.as_str()), on_copy, None));
        }
        list.into_any_element()
    }

    /// 文件结果：文件卡片（名称、大小、页数 / 文件数、保存与在文件夹中显示）、压缩前后对比、图片预览与包里回传的说明文字。
    fn render_file(&self, fill: bool, cx: &mut Context<Self>) -> AnyElement {
        use tools::fileinfo::{human, FileKind};
        let th = cur();
        let Some((name, data)) = &self.output.file else { return div().into_any_element() };
        let fallback = tools::fileinfo::inspect(name, data);
        let info = self.output.info.as_ref().unwrap_or(&fallback);
        let mut parts = vec![human(info.size)];
        if let Some((w, h)) = info.dims { parts.push(tf("tools.file_dims", &[("w", &w), ("h", &h)])); }
        if let Some(n) = info.pages { parts.push(tf("tools.file_pages", &[("n", &n)])); }
        if let Some(n) = info.entries { parts.push(tf("tools.file_entries", &[("n", &n)])); }
        let on_reveal: Option<ClickFn> = self.saved.clone().map(|p| -> ClickFn { Box::new(move |_, _, _| { let _ = ssh_term_proto::updater::reveal(&p); }) });
        let card = result_list::file_card(result_list::FileCard {
            name: SharedString::from(info.name.clone()), meta: parts.join(" · "), kind: info.kind,
            on_save: Box::new(cx.listener(|this, _: &ClickEvent, _w, cx| this.save(cx))), on_reveal,
        });
        let mut col = div().id("file-scroll").min_h_0().flex().flex_col().pb(px(SUB_PAD)).child(card);
        col = if fill { col.flex_1() } else { col.overflow_y_scroll() };
        // 图片压缩：压缩前 → 压缩后
        if let (true, Some(before), FileKind::Image) = (self.spec.id == "img_compress", self.output.in_size, info.kind) {
            let after = info.size;
            let pct = format!("{:.1}", tools::fileinfo::change_pct(before, after).abs());
            let (txt, col_t) = if after <= before {
                (tf("tools.cmp_saved", &[("a", &human(before)), ("b", &human(after)), ("pct", &pct)]), th.success)
            } else {
                (tf("tools.cmp_bigger", &[("a", &human(before)), ("b", &human(after)), ("pct", &pct)]), th.warning)
            };
            col = col.child(div().mx(px(SUB_PAD)).mt(px(ui::SP_2)).text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(col_t)).child(txt));
        }
        if !self.output.text.is_empty() && self.output.rows.is_empty() {
            col = col.child(div().mx(px(SUB_PAD)).mt(px(ui::SP_2)).whitespace_normal().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(self.output.text.clone()));
        }
        if let Some(img) = &self.preview {
            // 预览框占满剩余空间；图片在框内等比缩放（Contain），框的宽高再以原始像素为上限，所以小图不会被放大
            let (w, h) = info.preview_dims.map(|(w, h)| (w as f32, h as f32)).unwrap_or((320., 240.));
            let zip = info.kind == FileKind::Zip;
            col = col.child(div().mx(px(SUB_PAD)).mt(px(ui::SP_3)).flex_1().min_h(px(lay::PREVIEW_MIN_H)).flex().flex_col().gap(px(ui::SP_1))
                .when(zip, |d| d.child(div().text_size(px(ui::fs_caption())).text_color(c(th.overlay1)).child(t("tools.preview_zip_note"))))
                .child(div().relative().flex_1().min_h_0().rounded(px(ui::R_MD)).border_1().border_color(crate::theme::divider()).bg(crate::theme::hover_bg()).overflow_hidden()
                    .child(div().absolute().top_0().left_0().size_full().p(px(ui::SP_2)).flex().items_center().justify_center()
                        .child(gpui::img(img.clone()).w_full().h_full().max_w(px(w)).max_h(px(h))))));
        }
        col.into_any_element()
    }

    /// Markdown 转 HTML：预览页渲染输入的 Markdown，源码页是生成的 HTML（复制 / 保存始终是 HTML）。
    fn render_markdown(&self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let tabs = [("preview".to_string(), t("tools.tab_preview").to_string()), ("source".to_string(), t("tools.tab_source").to_string())];
        let bar = div().mx(px(SUB_PAD)).mt(px(ui::SP_2)).flex_shrink_0()
            .child(ui::segmented("md-tab", &tabs, self.tab, move |v, _w, cx| { let _ = me.update(cx, |this, cx| { this.tab = if v == "source" { "source" } else { "preview" }; cx.notify(); }); }));
        let body: AnyElement = if self.tab == "source" {
            sub_card_body().flex_1().min_h_0().child(Editor::new(&self.out_ed).h_full()).into_any_element()
        } else {
            let src = self.areas.get("input").map(|a| a.read(cx).value().to_string()).unwrap_or_default();
            div().id("md-preview").flex_1().min_h_0().overflow_y_scroll().px(px(SUB_PAD)).py(px(ui::SP_2)).child(gpui_component::text::TextView::markdown("tool-md", src).selectable(true)).into_any_element()
        };
        div().flex_1().min_h_0().flex().flex_col().child(bar).child(body).into_any_element()
    }

    /// 正则结果：匹配 / 替换两页。匹配页是原文高亮预览 + 逐处匹配；替换页是替换后的全文。
    fn render_regex(&self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let tabs = [("match".to_string(), t("tools.tab_match").to_string()), ("replace".to_string(), t("tools.tab_replace").to_string())];
        let bar = div().mx(px(SUB_PAD)).mt(px(ui::SP_2)).flex_shrink_0()
            .child(ui::segmented("rx-tab", &tabs, self.tab, move |v, _w, cx| { let _ = me.update(cx, |this, cx| { this.tab = if v == "replace" { "replace" } else { "match" }; cx.notify(); }); }));
        let body: AnyElement = if self.tab == "replace" {
            if self.output.alt.is_none() {
                div().flex_1().min_h_0().p(px(SUB_PAD)).text_size(px(ui::fs_small())).text_color(c(cur().subtext)).child(t("tools.rx_replace_hint")).into_any_element()
            } else {
                sub_card_body().flex_1().min_h_0().child(Editor::new(&self.out_ed).h_full()).into_any_element()
            }
        } else {
            let src = self.output.source.as_ref().map(|s| result_list::source_preview("rx-src", s));
            div().flex_1().min_h_0().flex().flex_col().children(src).child(self.render_kv(false, cx)).into_any_element()
        };
        div().flex_1().min_h_0().flex().flex_col().child(bar).child(body).into_any_element()
    }

    /// 输出区是否已有可显示的结果。
    fn has_result(&self) -> bool {
        let o = &self.output;
        !o.rows.is_empty() || o.file.is_some() || !o.text.is_empty() || o.verdict.is_some()
    }

    /// 输出是否占满剩余空间：文本、图片预览、历史、批量列表、正则占满；键值行和无预览的文件卡片贴合内容。
    /// 结果出现前按工具本身的形态决定。
    fn out_fill(&self) -> bool {
        if tools::has_history(&self.spec.id) { return true; }
        let fit = tools::out_fit(&self.spec.id);
        if self.output.file.is_some() { return self.preview.is_some(); }
        if !self.has_result() { return !fit; }
        match self.output.kind {
            OutKind::Kv => self.output.source.is_some() || !fit,
            OutKind::Text | OutKind::Rows => true,
        }
    }

    /// 拖动输入与输出之间的分隔条：上下排列按纵向、并排按横向换算输入区占的比例。
    fn drag_split(&mut self, ev: &gpui::DragMoveEvent<ToolSplit>, cx: &mut Context<Self>) {
        let b = ev.bounds;
        let r = if self.side {
            lay::split_ratio(f32::from(ev.event.position.x), f32::from(b.left()), f32::from(b.size.width) - CARD_PAD, CARD_PAD)
        } else {
            let gaps = if self.has_opts { 2. } else { 1. };
            let free = f32::from(b.size.height) - self.opts_h.get() - gaps * CARD_PAD;
            lay::split_ratio(f32::from(ev.event.position.y), f32::from(b.top()), free, CARD_PAD)
        };
        self.dragging = true;
        if (r - self.split).abs() > 0.002 { self.split = r; cx.notify(); }
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
            return content_card("tool-card").p(px(CARD_PAD)).child(head).child(body).into_any_element();
        }
        // 分隔比例：拖动中用本地值，松手后写回配置（所有工具共用）；平时跟随配置，其它工具标签改过也会同步
        if !cx.has_active_drag() {
            if self.dragging {
                self.dragging = false;
                let r = (self.split * 1000.).round() / 1000.;
                if (crate::config::get().sidebar.tools_split - r).abs() >= 0.002 { let _ = crate::config::update(|s| s.sidebar.tools_split = r); }
            } else { self.split = lay::clamp_split(crate::config::get().sidebar.tools_split); }
        }
        let me = cx.entity().downgrade();
        let sizes: Vec<InSize> = self.spec.inputs.iter().map(|f| f.size()).collect();
        let in_fill = sizes.contains(&InSize::Fill);
        let has_inputs = !sizes.is_empty();
        let has_opts = !self.spec.options.is_empty() || !self.spec.live;
        // 输入有大文本框时输出一定占满剩余空间（两者共用一条分隔）；贴合内容的输出在出结果前不画卡片
        let out_fill = in_fill || self.out_fill();
        let show_out = out_fill || self.has_result() || self.error.is_some();
        let both = in_fill && out_fill;
        let side = both && lay::side_by_side(self.body_w.get());
        self.side = side;
        self.has_opts = has_opts;
        let grow = |share: f32| (share * 1000.).max(1.);
        // 输入卡片
        let mut input = sub_card().p(px(SUB_PAD)).flex().flex_col().gap_2();
        for f in self.spec.inputs.clone() {
            let mut row = div().flex().flex_col().gap_1();
            // 多行输入的标签行右侧显示行数与字符数
            let stat = match f.kind {
                FieldKind::Multiline => self.areas.get(&f.id).map(|a| tools::text_stats(&a.read(cx).value()))
                    .or_else(|| self.tareas.get(&f.id).map(|a| tools::text_stats(&a.read(cx).value()))).filter(|s| s.1 > 0),
                _ => None,
            };
            row = row.child(div().flex().items_center().child(div().flex_1().child(Self::field_label(&f.label)))
                .children(stat.map(|(l, n)| div().text_size(px(ui::fs_caption())).text_color(c(th.overlay1)).child(tf("tools.ui_stat", &[("lines", &l), ("chars", &n)])))));
            match f.size() {
                InSize::Fill => { row = row.flex_1().min_h(px(lay::IN_FILL_MIN_H)).child(div().flex_1().min_h(px(lay::OUT_EDITOR_MIN_H)).child(Editor::new(&self.areas[&f.id]).h_full())); }
                InSize::Auto(..) => { row = row.child(Textarea::new(&self.tareas[&f.id]).w_full().font_family(ui::mono_family()).text_size(px(ui::fs_mono()))); }
                InSize::Line => { row = row.child(ui::text_field(&self.texts[&f.id], false)); }
                InSize::Zone => {
                    let id = f.id.clone();
                    let multiple = f.kind == FieldKind::Files;
                    let names: Vec<String> = self.files.get(&f.id).map(|v| v.iter().map(|p| p.display().to_string()).collect()).unwrap_or_default();
                    let id2 = id.clone();
                    row = row.child(div().id(SharedString::from(format!("drop-{id}"))).min_h(px(lay::FILE_ZONE_MIN_H)).max_h(px(lay::FILE_ZONE_MAX_H)).overflow_y_scroll().p_3().rounded(px(ui::R_MD)).border_1().border_dashed().border_color(crate::theme::divider())
                        .flex().flex_col().items_center().justify_center().gap_1().cursor_pointer().hover(|s| s.bg(crate::theme::hover_bg()))
                        .drag_over::<ExternalPaths>(|st, _, _, _| st.border_color(c(cur().accent)))
                        .on_drop(cx.listener(move |v, ps: &ExternalPaths, _, cx| v.set_files(id2.clone(), ps.paths().to_vec(), cx)))
                        .on_click(cx.listener(move |v, _, _, cx| v.pick_files(id.clone(), multiple, cx)))
                        .child(icon(IconName::FileUp, ui::IC_LG, c(th.overlay1)))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(t("tools.ui_drop_hint")))
                        .children(names.into_iter().map(|n| div().text_size(px(ui::fs_small())).text_color(c(th.text)).child(n))));
                }
            }
            input = input.child(row);
        }
        // 高度：大文本占剩余空间（与输出共用分隔）；其余按内容，不被压缩
        input = if in_fill {
            let base = input.min_h(px(lay::in_min_h(&sizes))).min_w(px(if side { lay::SIDE_PANE_MIN_W } else { 0. }));
            if both { base.flex_basis(px(0.)).flex_grow(grow(self.split)).flex_shrink(1.) } else { base.flex_1() }
        } else { input.flex_shrink_0() };
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
        let opts_h = self.opts_h.clone();
        let opts_card = sub_card().relative().p(px(SUB_PAD)).flex().items_end().gap_3().flex_shrink_0()
            .child(div().flex_1().min_w_0().child(opts)).child(run_btn)
            .child(gpui::canvas(move |b, _, _| opts_h.set(f32::from(b.size.height)), |_, _, _, _| {}).absolute().size_full());
        // 输出卡片
        let mut out = sub_card().flex().flex_col().child(self.out_head(true, cx));
        if let Some(e) = &self.error { out = out.child(ui::notice(ui::Notice::Error, e.clone()).mx(px(SUB_PAD)).my(px(ui::SP_2))); }
        if let (Some(v), false) = (&self.output.verdict, self.error.is_some()) { out = out.child(result_list::verdict_banner(v)); }
        if tools::has_history(&self.spec.id) {
            out = out.child(self.render_history(cx));
        } else if self.output.file.is_some() {
            out = out.child(self.render_file(out_fill, cx));
        } else if self.spec.id == "md_html" && !self.output.text.is_empty() {
            out = out.child(self.render_markdown(cx));
        } else if self.output.kind == OutKind::Kv && self.output.source.is_some() {
            out = out.child(self.render_regex(cx));
        } else if self.output.kind == OutKind::Kv {
            out = out.child(self.render_kv(!out_fill, cx));
        } else {
        if let Some([r, g, b, a]) = self.output.color {
            out = out.child(div().px(px(SUB_PAD)).pt(px(SUB_PAD)).flex().items_center().gap_2()
                .child(div().size(px(ui::H_DEFAULT)).rounded(px(ui::R_MD)).border_1().border_color(crate::theme::divider()).bg(gpui::Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: a as f32 / 255. })));
        }
        out = out.child(sub_card_body().min_h(px(lay::OUT_EDITOR_MIN_H)).child(Editor::new(&self.out_ed).h_full()));
        }
        // 高度：占满剩余空间（有大文本输入时与输入共用分隔），或贴合内容、放不下时卡片内部滚动
        out = if out_fill {
            let base = out.min_h(px(lay::OUT_FILL_MIN_H)).min_w(px(if side { lay::SIDE_PANE_MIN_W } else { 0. }));
            if both { base.flex_basis(px(0.)).flex_grow(grow(1. - self.split)).flex_shrink(1.) } else { base.flex_1() }
        } else { out.flex_grow(0.).flex_shrink(1.).min_h(px(lay::fit_min_h())) };
        // 工具区：宽度测量放在画布里；放不下时整个区域滚动
        let (body_w, wk) = (self.body_w.clone(), cx.entity().downgrade());
        let probe = gpui::canvas(move |b, _, cx| {
            let w = f32::from(b.size.width);
            let old = body_w.replace(w);
            if lay::side_by_side(old) != lay::side_by_side(w) { let _ = wk.update(cx, |_, cx| cx.notify()); }
        }, |_, _, _, _| {}).absolute().size_full();
        let body = div().id("tool-body").relative().flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        let body = if side {
            // 并排：选项在上通栏，下面左输入右输出，中间竖向分隔条
            body.when(has_opts, |d| d.child(opts_card).child(lay::gap(false)))
                .child(div().id("tool-split-row").flex_1().min_h(px(lay::OUT_FILL_MIN_H)).flex().flex_row()
                    .on_drag_move(cx.listener(|this, ev: &gpui::DragMoveEvent<ToolSplit>, _w, cx| this.drag_split(ev, cx)))
                    .child(input)
                    .child(lay::handle("tool-split", true).on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| if ev.click_count() >= 2 { this.split = lay::SPLIT_DEFAULT; this.dragging = true; cx.notify(); })))
                    .child(out))
        } else {
            body.on_drag_move(cx.listener(|this, ev: &gpui::DragMoveEvent<ToolSplit>, _w, cx| this.drag_split(ev, cx)))
                .when(has_inputs, |d| d.child(input))
                .when(has_inputs && (has_opts || show_out), |d| d.child(if both {
                    lay::handle("tool-split", false).on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| if ev.click_count() >= 2 { this.split = lay::SPLIT_DEFAULT; this.dragging = true; cx.notify(); })).into_any_element()
                } else { lay::gap(false).into_any_element() }))
                .when(has_opts, |d| d.child(opts_card))
                .when(has_opts && show_out, |d| d.child(lay::gap(false)))
                .when(show_out, |d| d.child(out))
        };
        content_card("tool-card").p(px(CARD_PAD)).child(head).child(body.child(probe)).into_any_element()
    }
}
