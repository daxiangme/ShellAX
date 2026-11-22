//! 工具模块界面：一个通用视图按 `ToolSpec` 渲染输入卡片、选项与输出卡片；WebSocket 为交互式视图。
//! 视图在打开标签时创建、关闭时释放；没有后台轮询，耗时工具在后台执行器上运行。
//! 生成类工具的历史由 Root 按工具 id 持有（会话级、只在内存），所以关闭标签后再打开仍在。
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, AnyElement, ClickEvent, ClipboardItem, Context, Entity, EventEmitter,
    ExternalPaths, FontWeight, SharedString, Window,
};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState, Textarea, TextareaState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::tools::history::History;
use ssh_term_proto::tools::{self, Args, FieldKind, InSize, OutKind, Output, ToolSpec};

use crate::card_layout::{content_card, sub_card, sub_card_body, CARD_PAD, SUB_PAD};
use crate::result_list::{self, ClickFn, RowMode};
use crate::theme::{c, cur};
use crate::tools_layout::{self as lay, ToolSplit};
use crate::ui::{self, icon, IconName};
use gpui_component::Sizable as _;

pub enum ToolEvent {
    SendToTerminal(String),
}

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
    notice_seq: u64,
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
    /// PDF 还没有预览图时的说明：正在生成、未安装渲染扩展，或渲染失败。
    pdf_note: Option<String>,
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

struct WsState {
    url: Entity<InputState>,
    msg: Entity<InputState>,
    tx: Option<tools::ws::WsSender>,
    log: Vec<(bool, String)>,
    open: bool,
}

impl EventEmitter<ToolEvent> for ToolView {}

impl ToolView {
    pub fn new(
        mut spec: ToolSpec,
        rt: tokio::runtime::Handle,
        hist: Rc<RefCell<History>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        tools::apply_hints(&mut spec);
        let mut subs = vec![];
        let (mut texts, mut areas, mut picks, mut tareas) = (
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        for f in spec.inputs.iter().chain(&spec.options) {
            match f.kind {
                FieldKind::Text | FieldKind::Number => {
                    let (d, ph) = (
                        f.default.clone(),
                        if f.placeholder.is_empty() {
                            f.label.clone()
                        } else {
                            f.placeholder.clone()
                        },
                    );
                    let st =
                        cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(d));
                    subs.push(cx.subscribe_in(
                        &st,
                        window,
                        |this: &mut Self, _, ev: &InputEvent, _w, cx| match ev {
                            InputEvent::Change => {
                                if this.spec.live {
                                    this.run(cx)
                                }
                            }
                            InputEvent::PressEnter { .. } => this.run(cx),
                            _ => {}
                        },
                    ));
                    texts.insert(f.id.clone(), st);
                }
                FieldKind::Multiline if matches!(f.size(), InSize::Auto(..)) => {
                    let (d, ph) = (
                        f.default.clone(),
                        if f.placeholder.is_empty() {
                            t("tools.ui_input_ph").to_string()
                        } else {
                            f.placeholder.clone()
                        },
                    );
                    let InSize::Auto(lo, hi) = f.size() else {
                        unreachable!()
                    };
                    let st = cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .placeholder(ph)
                            .auto_grow(lo as usize, hi as usize)
                    });
                    if !d.is_empty() {
                        st.update(cx, |e, cx| e.set_value(d, window, cx));
                    }
                    subs.push(cx.subscribe_in(
                        &st,
                        window,
                        |this: &mut Self, _, ev: &InputEvent, _w, cx| {
                            if matches!(ev, InputEvent::Change) && this.spec.live {
                                this.run(cx)
                            }
                        },
                    ));
                    tareas.insert(f.id.clone(), st);
                }
                FieldKind::Multiline => {
                    let (d, ph) = (
                        f.default.clone(),
                        if f.placeholder.is_empty() {
                            t("tools.ui_input_ph").to_string()
                        } else {
                            f.placeholder.clone()
                        },
                    );
                    let st = cx.new(|cx| {
                        EditorState::new(window, cx)
                            .line_number(false)
                            .soft_wrap(true)
                            .placeholder(ph)
                            .default_value(d)
                    });
                    subs.push(cx.subscribe_in(
                        &st,
                        window,
                        |this: &mut Self, _, ev: &InputEvent, _w, cx| {
                            if matches!(ev, InputEvent::Change) && this.spec.live {
                                this.run(cx)
                            }
                        },
                    ));
                    areas.insert(f.id.clone(), st);
                }
                FieldKind::Select | FieldKind::Toggle | FieldKind::Color => {
                    picks.insert(f.id.clone(), f.default.clone());
                }
                FieldKind::File | FieldKind::Files => {}
            }
        }
        let out_ed = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(false)
                .soft_wrap(true)
        });
        let ws = spec.interactive.then(|| WsState {
            url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("wss://echo.websocket.org")
                    .default_value("wss://echo.websocket.org")
            }),
            msg: cx.new(|cx| InputState::new(window, cx).placeholder(t("tools.ws_message"))),
            tx: None,
            log: vec![],
            open: false,
        });
        if let Some(w) = &ws {
            subs.push(cx.subscribe_in(
                &w.msg,
                window,
                |this: &mut Self, _, ev: &InputEvent, w, cx| {
                    if let InputEvent::PressEnter { .. } = ev {
                        this.ws_send(w, cx)
                    }
                },
            ));
        }
        let title = spec.name.clone();
        let spec_id = spec.id.clone();
        let mut me = Self {
            spec,
            title,
            texts,
            areas,
            tareas,
            picks,
            files: HashMap::new(),
            out_ed,
            output: Output::default(),
            error: None,
            notice: None,
            notice_seq: 0,
            busy: false,
            seq: 0,
            pending_out: None,
            hist,
            copied: None,
            copy_seq: 0,
            tab: if spec_id == "md_html" {
                "preview"
            } else {
                "match"
            },
            preview: None,
            pdf_note: None,
            saved: None,
            ws,
            split: lay::clamp_split(crate::config::get().sidebar.tools_split),
            dragging: false,
            side: false,
            has_opts: false,
            body_w: Rc::new(Cell::new(0.)),
            opts_h: Rc::new(Cell::new(0.)),
            rt,
            _subs: subs,
        };
        if (me.spec.live && !me.spec.interactive && !me.spec.heavy)
            || me.spec.id == "ifaces"
            || (tools::has_history(&me.spec.id)
                && me.spec.inputs.is_empty()
                && me.hist.borrow().is_empty())
        {
            me.run(cx);
        }
        if me.spec.id == "timestamp" {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_secs(1))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            })
            .detach();
        }
        me
    }

    /// 把文本放进第一个输入框并运行（来自终端“用工具打开”）。
    pub fn preset(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.spec.inputs.first().cloned() else {
            return;
        };
        self.set_field(&f.id, text, window, cx);
        self.run(cx);
    }

    /// 把文本写进某个文本输入（单行、大文本或短多行）。
    fn set_field(&mut self, id: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(a) = self.areas.get(id) {
            a.update(cx, |e, cx| e.set_value(text, window, cx));
        } else if let Some(a) = self.tareas.get(id) {
            a.update(cx, |e, cx| e.set_value(text, window, cx));
        } else if let Some(s) = self.texts.get(id) {
            s.update(cx, |e, cx| e.set_value(text, window, cx));
        }
    }

    fn lookup_mine(&self) -> bool {
        self.spec.id == "ip_lookup" && self.picks.get("mine").map(|s| s.as_str()) == Some("true")
    }

    /// IP 查询：查本机时随时可运行；查指定地址时必须先填 IP。
    fn lookup_can_run(&self, cx: &gpui::App) -> bool {
        if self.spec.id != "ip_lookup" {
            return true;
        }
        if self.lookup_mine() {
            return true;
        }
        self.texts
            .get("input")
            .is_some_and(|e| !e.read(cx).value().trim().is_empty())
    }

    fn args(&self, cx: &gpui::App) -> Args {
        let mut a = Args::default();
        for (k, s) in &self.texts {
            a.set(k, s.read(cx).value().to_string());
        }
        for (k, s) in &self.areas {
            a.set(k, s.read(cx).value().to_string());
        }
        for (k, s) in &self.tareas {
            a.set(k, s.read(cx).value().to_string());
        }
        for (k, v) in &self.picks {
            a.set(k, v.clone());
        }
        a.files = self.files.clone();
        a
    }

    pub fn run(&mut self, cx: &mut Context<Self>) {
        if self.spec.interactive || !self.lookup_can_run(cx) {
            return;
        }
        let args = self.args(cx);
        let id = self.spec.id.clone();
        self.seq += 1;
        let seq = self.seq;
        self.notice = None;
        self.preview = None;
        self.pdf_note = None;
        if self.spec.heavy {
            if args.files.values().all(|v| v.is_empty())
                && self
                    .spec
                    .inputs
                    .iter()
                    .any(|f| matches!(f.kind, FieldKind::File | FieldKind::Files))
            {
                return;
            }
            self.busy = true;
            cx.notify();
            let pack = self.spec.pack.clone();
            let task = cx.background_executor().spawn(async move {
                if pack.is_empty() {
                    tools::run(&id, &args)
                } else {
                    ssh_term_proto::ext::run_installed(&pack, &id, &args)
                }
            });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                let _ = this.update(cx, |v, cx| {
                    if v.seq == seq {
                        v.busy = false;
                        v.apply(r, cx);
                        cx.notify();
                    }
                });
            })
            .detach();
        } else {
            let r = tools::run(&id, &args);
            self.apply(r, cx);
            cx.notify();
        }
    }

    fn apply(&mut self, r: anyhow::Result<Output>, cx: &mut Context<Self>) {
        match r {
            Ok(o) => {
                self.error = None;
                match o.kind {
                    // 生成类：追加一批到历史，旧结果保留
                    OutKind::Rows => {
                        self.hist.borrow_mut().push(
                            chrono::Local::now().format("%H:%M:%S").to_string(),
                            o.meta.clone(),
                            o.rows.clone(),
                        );
                    }
                    OutKind::Text => self.pending_out = Some(o.text.clone()),
                    // 带原文预览的键值结果（正则）：替换页的文本放进输出编辑器
                    OutKind::Kv => {
                        if o.source.is_some() {
                            self.pending_out = Some(o.alt.clone().unwrap_or_default());
                        }
                    }
                }
                self.preview =
                    o.info
                        .as_ref()
                        .and_then(|i| i.preview.as_ref())
                        .map(|(fmt, bytes)| {
                            let f = match fmt {
                                tools::fileinfo::PreviewFmt::Png => gpui::ImageFormat::Png,
                                tools::fileinfo::PreviewFmt::Jpeg => gpui::ImageFormat::Jpeg,
                            };
                            Arc::new(gpui::Image::from_bytes(f, bytes.clone()))
                        });
                self.pdf_note = None;
                self.saved = None;
                self.output = o;
                self.start_pdf_preview(cx);
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.preview = None;
                self.pdf_note = None;
            }
        }
    }

    /// PDF 结果没有自带预览图。已安装「PDF 渲染」时在后台画第一页，否则在输出区说明原因。
    fn start_pdf_preview(&mut self, cx: &mut Context<Self>) {
        let pdf = self
            .output
            .info
            .as_ref()
            .is_some_and(|i| i.kind == tools::fileinfo::FileKind::Pdf);
        let Some((_, data)) = pdf.then(|| self.output.file.clone()).flatten() else {
            return;
        };
        if !ssh_term_proto::ext::installed()
            .iter()
            .any(|i| i.manifest.id == "pdf-render")
        {
            self.pdf_note = Some(t("tools.pdf_preview_need").into());
            return;
        }
        self.pdf_note = Some(t("tools.pdf_previewing").into());
        let seq = self.seq;
        cx.spawn(async move |this, cx| {
            let png = cx
                .background_executor()
                .spawn(async move {
                    let path = std::env::temp_dir().join(format!(
                        "shellax-pdf-preview-{}-{seq}.pdf",
                        std::process::id()
                    ));
                    std::fs::write(&path, &data).map_err(|e| e.to_string())?;
                    let mut a = tools::Args::default();
                    a.set("pages", "1");
                    a.set("dpi", "96");
                    a.files.insert("file".into(), vec![path.clone()]);
                    let r = ssh_term_proto::ext::run_installed("pdf-render", "pdf_to_img", &a)
                        .map_err(|e| e.to_string());
                    let _ = std::fs::remove_file(&path);
                    r.and_then(|o| o.file.map(|(_, b)| b).ok_or_else(|| "empty".into()))
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.seq != seq {
                    return;
                }
                match png {
                    Ok(bytes) => {
                        if let Some((w, h)) = image::ImageReader::new(std::io::Cursor::new(&bytes))
                            .with_guessed_format()
                            .ok()
                            .and_then(|r| r.into_dimensions().ok())
                        {
                            if let Some(info) = v.output.info.as_mut() {
                                info.preview_dims = Some((w, h));
                            }
                        }
                        v.preview = Some(Arc::new(gpui::Image::from_bytes(
                            gpui::ImageFormat::Png,
                            bytes,
                        )));
                        v.pdf_note = None;
                    }
                    Err(e) => v.pdf_note = Some(tf("tools.pdf_preview_failed", &[("e", &e)])),
                }
                cx.notify();
            });
        })
        .detach();
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
            self.preview = None;
            self.pdf_note = None;
        }
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    /// 把当前结果放回第一个输入框并重新运行（加密后再解密、格式化后再压缩等来回操作，自动翻转双向开关）。
    fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.output_text();
        let Some(f) = self.spec.inputs.first().cloned() else {
            return;
        };
        if !self.areas.contains_key(&f.id) && !self.tareas.contains_key(&f.id) {
            return;
        }
        if let Some(dir) = self.picks.get("dir").cloned() {
            let flipped = match dir.as_str() {
                "enc" | "enc_uri" => Some("dec"),
                "dec" => Some("enc"),
                _ => None,
            };
            if let Some(next) = flipped {
                self.picks.insert("dir".into(), next.into());
            }
        } else if let Some(mode) = self.picks.get("mode").cloned() {
            let flipped = match (self.spec.id.as_str(), mode.as_str()) {
                ("go_json", "json2go") => Some("go2json"),
                ("go_json", "go2json") => Some("json2go"),
                ("plural", "plural") => Some("singular"),
                ("plural", "singular") => Some("plural"),
                ("json" | "sql" | "css" | "html", "pretty") => Some("minify"),
                ("json" | "sql" | "css" | "html", "minify") => Some("pretty"),
                _ => None,
            };
            if let Some(next) = flipped {
                self.picks.insert("mode".into(), next.into());
            }
        }
        self.set_field(&f.id, text, window, cx);
        self.run(cx);
    }

    fn paste_field(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.set_field(id, text, window, cx);
        if !self.spec.heavy || self.spec.id == "ip_lookup" || self.spec.id == "ip_latency" {
            self.run(cx);
        } else {
            cx.notify();
        }
    }

    fn fill_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mode = self.picks.get("mode").map(|s| s.as_str()).unwrap_or("");
        let dir = self.picks.get("dir").map(|s| s.as_str()).unwrap_or("");
        match self.spec.id.as_str() {
            "regex" => {
                self.set_field(
                    "pattern",
                    r"(?P<user>[a-zA-Z0-9._%+-]+)@(?P<host>[a-zA-Z0-9.-]+\.[a-zA-Z]{2,})".into(),
                    window,
                    cx,
                );
                self.set_field(
                    "input",
                    "Contact ops@shellax.dev or support@example.org for SSH access.".into(),
                    window,
                    cx,
                );
                self.set_field("replace", "[$user at $host]".into(), window, cx);
                self.run(cx);
                return;
            }
            "hmac" => {
                self.set_field("input", "GET /v1/sessions?limit=20".into(), window, cx);
                self.set_field("key", "sk_live_shellax_secret_2026".into(), window, cx);
                self.run(cx);
                return;
            }
            _ => {}
        }
        let Some(sample) = tool_sample(&self.spec.id, mode, dir) else {
            return;
        };
        if let Some(f) = self.spec.inputs.first().cloned() {
            self.set_field(&f.id, sample.into(), window, cx);
            self.run(cx);
        }
    }

    fn gen_random_field(
        &mut self,
        id: &str,
        len: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hex_mode = self.picks.get("keymode").map(|s| s.as_str()) == Some("hex");
        let alphabet: &[u8] = if hex_mode {
            b"0123456789abcdef"
        } else {
            b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789"
        };
        let n = if hex_mode { len * 2 } else { len };
        let mut seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e3779b97f4a7c15)
            ^ (self.seq.wrapping_mul(0x517cc1b727220a95));
        let mut s = String::with_capacity(n);
        for _ in 0..n {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            s.push(alphabet[(seed as usize) % alphabet.len()] as char);
        }
        self.set_field(id, s, window, cx);
        if self.spec.live {
            self.run(cx);
        } else {
            cx.notify();
        }
    }

    /// 清空历史（只清结果，输入与选项都保留）。
    fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.hist.borrow_mut().clear();
        self.output = Output::default();
        self.preview = None;
        self.pdf_note = None;
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    fn clear_log(&mut self, cx: &mut Context<Self>) {
        if let Some(w) = &mut self.ws {
            w.log.clear();
        }
        cx.notify();
    }

    /// 复制/保存/发送到终端用的文本：生成类取最新一批，其余取输出全文。
    fn output_text(&self) -> String {
        if tools::has_history(&self.spec.id) {
            return self
                .hist
                .borrow()
                .latest()
                .map(|b| b.text())
                .unwrap_or_default();
        }
        if self.tab == "replace" && self.output.source.is_some() {
            return self.output.alt.clone().unwrap_or_default();
        }
        self.output.text.clone()
    }

    fn copy_text(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(key);
        self.copy_seq += 1;
        let seq = self.copy_seq;
        self.flash_notice(t("tools.ui_copied").into(), cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.copy_seq == seq {
                    v.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// 成功确认。两秒后收起；失败或新提示会换掉序号。
    fn flash_notice(&mut self, msg: String, cx: &mut Context<Self>) {
        self.notice_seq = self.notice_seq.wrapping_add(1);
        let seq = self.notice_seq;
        self.notice = Some(msg.clone());
        ui::flash_after(
            cx,
            seq,
            |v| v.notice_seq,
            move |v| {
                if v.notice.as_deref() == Some(msg.as_str()) {
                    v.notice = None;
                }
            },
        );
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = if let Some(w) = &self.ws {
            w.log
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            self.output_text()
        };
        self.copy_text("all".into(), text, cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let (name, bytes) = match &self.output.file {
            Some((n, b)) => (n.clone(), b.clone()),
            None => (
                format!("{}.txt", self.spec.id),
                self.output_text().into_bytes(),
            ),
        };
        let dir = dirs_home();
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let r = std::fs::write(&path, bytes);
            let _ = this.update(cx, |v, cx| {
                if r.is_ok() {
                    v.saved = Some(path.clone());
                    v.flash_notice(tf("tools.ui_saved", &[("path", &path.display())]), cx);
                } else {
                    v.notice_seq = v.notice_seq.wrapping_add(1);
                    v.notice = Some(tf("tools.ui_save_failed", &[("e", &r.unwrap_err())]));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_files(&mut self, id: String, multiple: bool, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let _ = this.update(cx, |v, cx| v.set_files(id, paths, cx));
        })
        .detach();
    }

    fn set_files(&mut self, id: String, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let multiple = self
            .spec
            .inputs
            .iter()
            .any(|f| f.id == id && f.kind == FieldKind::Files);
        let e = self.files.entry(id).or_default();
        if multiple {
            e.extend(paths);
        } else {
            *e = paths.into_iter().take(1).collect();
        }
        self.run(cx);
    }

    fn set_pick(&mut self, id: String, v: String, cx: &mut Context<Self>) {
        self.picks.insert(id, v);
        // 生成类改选项不自动再生成（否则每次改选项都会追加一批）
        if self.spec.live || (!self.output.text.is_empty() && !tools::has_history(&self.spec.id)) {
            self.run(cx);
        } else {
            cx.notify();
        }
    }

    fn set_pick_in(
        &mut self,
        id: String,
        v: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.spec.id == "units" && id == "kind" {
            let def_unit = default_unit_for(&v);
            self.set_field("from", def_unit.into(), window, cx);
        }
        self.set_pick(id, v, cx);
    }

    // ---------------- WebSocket
    fn ws_toggle(&mut self, cx: &mut Context<Self>) {
        let Some(w) = &mut self.ws else { return };
        if let Some(tx) = w.tx.take() {
            let _ = tx.send(None);
            w.open = false;
            cx.notify();
            return;
        }
        let url = w.url.read(cx).value().trim().to_string();
        if url.is_empty() {
            return;
        }
        let (tx, rx) = tools::ws::connect(&self.rt, url);
        w.tx = Some(tx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv_async().await {
                let done = matches!(
                    ev,
                    tools::ws::WsEvent::Closed(_) | tools::ws::WsEvent::Error(_)
                );
                let alive = this
                    .update(cx, |v, cx| {
                        v.ws_event(ev);
                        cx.notify();
                    })
                    .is_ok();
                if done || !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn ws_event(&mut self, ev: tools::ws::WsEvent) {
        use tools::ws::WsEvent as E;
        let Some(w) = &mut self.ws else { return };
        let now = chrono::Local::now().format("%H:%M:%S");
        let line = match ev {
            E::Open => {
                w.open = true;
                format!("{now}  {}", t("tools.ws_open"))
            }
            E::Text(s) => format!("{now}  ← {s}"),
            E::Binary(n) => format!("{now}  ← {}", tf("tools.ws_binary", &[("n", &n)])),
            E::Closed(r) => {
                w.open = false;
                w.tx = None;
                format!("{now}  {}", tf("tools.ws_closed", &[("r", &r)]))
            }
            E::Error(e) => {
                w.open = false;
                w.tx = None;
                format!("{now}  {}", tf("tools.ws_error", &[("e", &e)]))
            }
        };
        w.log.push((false, line));
        if w.log.len() > 2000 {
            w.log.drain(..500);
        }
    }

    fn ws_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(w) = &mut self.ws else { return };
        let Some(tx) = &w.tx else { return };
        let text = w.msg.read(cx).value().to_string();
        if text.is_empty() || !w.open {
            return;
        }
        let _ = tx.send(Some(text.clone()));
        w.log.push((
            true,
            format!("{}  → {text}", chrono::Local::now().format("%H:%M:%S")),
        ));
        w.msg.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn render_ws(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let w = self.ws.as_ref().unwrap();
        let connected = w.tx.is_some();
        let is_open = w.open;
        let msg_count = w.log.len();
        let status_col = if is_open {
            c(th.success)
        } else if connected {
            c(th.warning)
        } else {
            c(th.overlay1)
        };
        let status_txt = if is_open {
            t("ux.ws_connected")
        } else if connected {
            t("tools.ui_running")
        } else {
            t("ux.ws_disconnected")
        };
        let mut presets = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
        for (i, url) in [
            "wss://echo.websocket.org",
            "ws://127.0.0.1:8080",
            "ws://localhost:3000/ws",
        ]
        .into_iter()
        .enumerate()
        {
            let u = url.to_string();
            presets = presets.child(
                ui::chip(("ws-pre", i), url, false).on_click(cx.listener(
                    move |this, _, win, cx| {
                        if let Some(ws) = &this.ws {
                            ws.url.update(cx, |s, cx| s.set_value(u.clone(), win, cx));
                        }
                        cx.notify();
                    },
                )),
            );
        }
        presets = presets
            .child(div().flex_1())
            .child(ui::tag(status_txt, status_col))
            .when(msg_count > 0, |d| {
                d.child(
                    div()
                        .text_size(px(ui::fs_caption()))
                        .text_color(c(th.overlay1))
                        .child(tf("ux.ws_msgs", &[("n", &msg_count)])),
                )
            });
        let bar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().child(ui::text_field(&w.url, false)))
            .child(
                ui::button(
                    "ws-conn",
                    if connected {
                        t("tools.ws_disconnect")
                    } else {
                        t("tools.ws_connect")
                    },
                    if connected { 2 } else { 1 },
                )
                .on_click(cx.listener(|this, _, _w, cx| this.ws_toggle(cx))),
            );
        let send = div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().child(ui::text_field(&w.msg, false)))
            .child(
                ui::button("ws-paste", t("ux.paste"), 0).on_click(cx.listener(
                    |this, _, win, cx| {
                        if let (Some(ws), Some(txt)) =
                            (&this.ws, cx.read_from_clipboard().and_then(|c| c.text()))
                        {
                            ws.msg.update(cx, |s, cx| s.set_value(txt, win, cx));
                        }
                    },
                )),
            )
            .child(
                ui::button("ws-send", t("tools.ws_send"), 1)
                    .on_click(cx.listener(|this, _, w, cx| this.ws_send(w, cx))),
            );
        let mut log = sub_card_body()
            .id("ws-log")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(ui::SP_1))
            .font_family(ui::mono_family())
            .text_size(px(ui::fs_mono()));
        for (i, (mine, l)) in w.log.iter().enumerate() {
            let line_copy = l.clone();
            log = log.child(
                div()
                    .id(("wsl", i))
                    .px(px(ui::SP_2))
                    .py(px(2.))
                    .rounded(px(ui::R_SM))
                    .cursor_pointer()
                    .hover(|s| s.bg(crate::theme::hover_bg()))
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        this.copy_text(format!("ws-{i}"), line_copy.clone(), cx)
                    }))
                    .whitespace_normal()
                    .text_color(if *mine { c(th.accent) } else { c(th.text) })
                    .child(l.clone()),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(
                sub_card()
                    .p(px(SUB_PAD))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(presets)
                    .child(bar),
            )
            .child(
                sub_card()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.out_head(false, cx))
                    .child(log)
                    .child(
                        div()
                            .p(px(SUB_PAD))
                            .border_t_1()
                            .border_color(crate::theme::divider())
                            .child(send),
                    ),
            )
            .into_any_element()
    }

    /// 输出卡片的标题行：复制 / 保存 / 清空 / 发送到终端。
    fn out_head(&self, full: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let small = |id: &'static str, ic: IconName, tip: &'static str| {
            div()
                .id(id)
                .h(px(ui::H_COMPACT))
                .px_2()
                .rounded(px(ui::R_MD))
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .text_size(px(ui::fs_small()))
                .text_color(c(cur().subtext))
                .hover(|s| s.bg(crate::theme::hover_bg()))
                .child(icon(ic, ui::IC_XS, c(cur().overlay1)))
                .child(tip)
        };
        let plain = self.output.kind == OutKind::Text
            && self.output.file.is_none()
            && !self.output.text.is_empty()
            && self.ws.is_none()
            && !tools::has_history(&self.spec.id);
        let stat = plain.then(|| tools::text_stats(&self.output.text));
        let can_swap = plain
            && self.spec.id != "md_html"
            && self
                .spec
                .inputs
                .first()
                .is_some_and(|f| f.kind == FieldKind::Multiline);
        ui::toolbar()
            .child(
                div()
                    .flex_1()
                    .text_size(px(ui::fs_small()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(th.overlay1))
                    .child(t("tools.ui_output")),
            )
            .when_some(stat, |d, (l, n)| {
                d.child(
                    div()
                        .text_size(px(ui::fs_caption()))
                        .text_color(c(th.overlay1))
                        .child(tf("tools.ui_stat", &[("lines", &l), ("chars", &n)])),
                )
            })
            .when_some(self.notice.clone(), |d, n| {
                d.child(
                    div()
                        .text_size(px(ui::fs_small()))
                        .text_color(c(th.success))
                        .child(n),
                )
            })
            .when(self.busy, |d| {
                d.child(
                    div()
                        .text_size(px(ui::fs_small()))
                        .text_color(c(th.overlay1))
                        .child(t("tools.ui_running")),
                )
            })
            .child(
                small("out-copy", IconName::Copy, t("tools.ui_copy"))
                    .on_click(cx.listener(|this, _, _w, cx| this.copy(cx))),
            )
            .when(full && can_swap, |d| {
                d.child(
                    small("out-swap", IconName::ArrowLeftRight, t("tools.ui_swap"))
                        .on_click(cx.listener(|this, _, w, cx| this.swap(w, cx))),
                )
            })
            .when(full, |d| {
                d.child(
                    small("out-save", IconName::Save, t("tools.ui_save"))
                        .on_click(cx.listener(|this, _, _w, cx| this.save(cx))),
                )
                .child(
                    small(
                        "out-term",
                        IconName::SquareTerminal,
                        t("tools.ui_send_term"),
                    )
                    .on_click(cx.listener(|this, _, _w, cx| {
                        let s = this.output_text();
                        if !s.is_empty() {
                            cx.emit(ToolEvent::SendToTerminal(s));
                        }
                    })),
                )
            })
            .when(self.ws.is_some(), |d| {
                d.child(
                    small("out-clear", IconName::Eraser, t("tools.ui_clear"))
                        .on_click(cx.listener(|this, _, _w, cx| this.clear_log(cx))),
                )
            })
            .when(self.ws.is_none() && !self.spec.inputs.is_empty(), |d| {
                d.child(
                    small("out-clear-in", IconName::Eraser, t("tools.ui_clear_input"))
                        .on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx))),
                )
            })
            .when(
                tools::has_history(&self.spec.id) && !self.hist.borrow().is_empty(),
                |d| {
                    d.child(
                        small(
                            "out-clear-hist",
                            IconName::Trash,
                            t("tools.ui_clear_history"),
                        )
                        .on_click(cx.listener(|this, _, _w, cx| this.clear_history(cx))),
                    )
                },
            )
    }

    /// 生成类历史：按批次分组，新批次在最上；每条一行，可逐条复制 / 删除。
    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let hist = self.hist.clone();
        let h = hist.borrow();
        if h.is_empty() {
            return ui::empty_state(
                IconName::Clock,
                t("tools.hist_empty"),
                Some(t("tools.hist_empty_desc").into()),
                vec![],
                true,
            )
            .into_any_element();
        }
        let mut list = div()
            .id("hist-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py(px(ui::LIST_INSET))
            .flex()
            .flex_col()
            .gap(px(ui::SP_1));
        for (bi, b) in h.batches.iter().enumerate() {
            let bid = b.id;
            let listener = |f: fn(&mut Self, u64, &mut Context<Self>)| -> ClickFn {
                Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| f(this, bid, cx)))
            };
            let text = b.text();
            list = list.child(result_list::batch_head(result_list::BatchHead {
                id: bid,
                time: b.time.clone(),
                meta: b.meta.clone(),
                count: b.rows.len(),
                folded: b.folded,
                masked: b.has_secret().then_some(b.masked),
                on_fold: listener(|v, id, cx| {
                    v.hist.borrow_mut().toggle_fold(id);
                    cx.notify();
                }),
                on_mask: listener(|v, id, cx| {
                    v.hist.borrow_mut().toggle_mask(id);
                    cx.notify();
                }),
                on_copy: Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.copy_text(format!("b{bid}"), text.clone(), cx)
                })),
                on_delete: listener(|v, id, cx| {
                    v.hist.borrow_mut().remove_batch(id);
                    cx.notify();
                }),
            }));
            if b.folded {
                continue;
            }
            for (ri, r) in b.rows.iter().enumerate() {
                let key = format!("r{bid}-{ri}");
                let (val, k2) = (r.value.clone(), key.clone());
                let on_copy: ClickFn =
                    Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.copy_text(k2.clone(), val.clone(), cx)
                    }));
                let on_del: ClickFn = Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.hist.borrow_mut().remove_row(bid, ri);
                    cx.notify();
                }));
                list = list.child(result_list::result_row(
                    SharedString::from(format!("hr-{bid}-{ri}-{bi}")),
                    r,
                    RowMode::List,
                    b.masked && r.secret,
                    self.copied.as_deref() == Some(key.as_str()),
                    on_copy,
                    Some(on_del),
                ));
            }
        }
        list.into_any_element()
    }

    /// 多格式结果：每种写法一行（主结果加大加粗），可逐条复制；颜色工具在最上面带色块。
    fn render_kv(&self, fit: bool, cx: &mut Context<Self>) -> AnyElement {
        // 贴合内容：高度随行数，超出可用高度时卡片内部滚动；否则占满输出卡片
        let mut list = div()
            .id("kv-scroll")
            .when(!fit, |d| d.flex_1())
            .min_h_0()
            .overflow_y_scroll()
            .py(px(ui::LIST_INSET))
            .flex()
            .flex_col()
            .gap(px(ui::SP_1));
        if let (Some([r, g, b, a]), false) = (self.output.color, self.spec.id == "color") {
            list = list.child(
                div()
                    .px(px(ui::LIST_INSET + ui::ROW_PX))
                    .pb(px(ui::SP_2))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(px(ui::SP_6 * 1.5))
                            .w_full()
                            .rounded(px(ui::R_MD))
                            .border_1()
                            .border_color(crate::theme::divider())
                            .bg(gpui::Rgba {
                                r: r as f32 / 255.,
                                g: g as f32 / 255.,
                                b: b as f32 / 255.,
                                a: a as f32 / 255.,
                            }),
                    ),
            );
        }
        for (i, r) in self.output.rows.iter().enumerate() {
            let key = format!("kv{i}");
            let (val, k2) = (r.value.clone(), key.clone());
            let on_copy: ClickFn = Box::new(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.copy_text(k2.clone(), val.clone(), cx)
            }));
            list = list.child(result_list::result_row(
                SharedString::from(format!("kvr-{i}")),
                r,
                RowMode::Kv,
                false,
                self.copied.as_deref() == Some(key.as_str()),
                on_copy,
                None,
            ));
        }
        list.into_any_element()
    }

    /// 文件结果：文件卡片（名称、大小、页数 / 文件数、保存与在文件夹中显示）、压缩前后对比、图片预览与包里回传的说明文字。
    fn render_file(&self, fill: bool, cx: &mut Context<Self>) -> AnyElement {
        use tools::fileinfo::{human, FileKind};
        let th = cur();
        let Some((name, data)) = &self.output.file else {
            return div().into_any_element();
        };
        let fallback = tools::fileinfo::inspect(name, data);
        let info = self.output.info.as_ref().unwrap_or(&fallback);
        let mut parts = vec![human(info.size)];
        if let Some((w, h)) = info.dims {
            parts.push(tf("tools.file_dims", &[("w", &w), ("h", &h)]));
        }
        if let Some(n) = info.pages {
            parts.push(tf("tools.file_pages", &[("n", &n)]));
        }
        if let Some(n) = info.entries {
            parts.push(tf("tools.file_entries", &[("n", &n)]));
        }
        let on_reveal: Option<ClickFn> = self.saved.clone().map(|p| -> ClickFn {
            Box::new(move |_, _, _| {
                let _ = ssh_term_proto::updater::reveal(&p);
            })
        });
        let card = result_list::file_card(result_list::FileCard {
            name: SharedString::from(info.name.clone()),
            meta: parts.join(" · "),
            kind: info.kind,
            on_save: Box::new(cx.listener(|this, _: &ClickEvent, _w, cx| this.save(cx))),
            on_reveal,
        });
        let mut col = div()
            .id("file-scroll")
            .min_h_0()
            .flex()
            .flex_col()
            .pb(px(SUB_PAD))
            .child(card);
        col = if fill {
            col.flex_1()
        } else {
            col.overflow_y_scroll()
        };
        // 图片压缩：压缩前 → 压缩后
        if let (true, Some(before), FileKind::Image) = (
            self.spec.id == "img_compress",
            self.output.in_size,
            info.kind,
        ) {
            let after = info.size;
            let pct = format!("{:.1}", tools::fileinfo::change_pct(before, after).abs());
            let (txt, col_t) = if after <= before {
                (
                    tf(
                        "tools.cmp_saved",
                        &[("a", &human(before)), ("b", &human(after)), ("pct", &pct)],
                    ),
                    th.success,
                )
            } else {
                (
                    tf(
                        "tools.cmp_bigger",
                        &[("a", &human(before)), ("b", &human(after)), ("pct", &pct)],
                    ),
                    th.warning,
                )
            };
            col = col.child(
                div()
                    .mx(px(SUB_PAD))
                    .mt(px(ui::SP_2))
                    .text_size(px(ui::fs_body()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c(col_t))
                    .child(txt),
            );
        }
        if !self.output.text.is_empty() && self.output.rows.is_empty() {
            col = col.child(
                div()
                    .mx(px(SUB_PAD))
                    .mt(px(ui::SP_2))
                    .whitespace_normal()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.subtext))
                    .child(self.output.text.clone()),
            );
        }
        if info.kind == FileKind::Pdf && self.preview.is_none() {
            let msg = self.pdf_note.clone().unwrap_or_default();
            col = col.child(
                div()
                    .mx(px(SUB_PAD))
                    .mt(px(ui::SP_3))
                    .flex_1()
                    .min_h(px(lay::PREVIEW_MIN_H))
                    .rounded(px(ui::R_MD))
                    .border_1()
                    .border_color(crate::theme::divider())
                    .bg(crate::theme::hover_bg())
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(ui::SP_3))
                    .child(
                        div()
                            .text_size(px(ui::fs_small()))
                            .text_color(c(th.subtext))
                            .whitespace_normal()
                            .child(msg),
                    ),
            );
        }
        if let Some(img) = &self.preview {
            // 预览框占满剩余空间；图片在框内等比缩放（Contain），框的宽高再以原始像素为上限，所以小图不会被放大
            let (w, h) = info
                .preview_dims
                .map(|(w, h)| (w as f32, h as f32))
                .unwrap_or((320., 240.));
            let zip = info.kind == FileKind::Zip;
            col = col.child(
                div()
                    .mx(px(SUB_PAD))
                    .mt(px(ui::SP_3))
                    .flex_1()
                    .min_h(px(lay::PREVIEW_MIN_H))
                    .flex()
                    .flex_col()
                    .gap(px(ui::SP_1))
                    .when(zip, |d| {
                        d.child(
                            div()
                                .text_size(px(ui::fs_caption()))
                                .text_color(c(th.overlay1))
                                .child(t("tools.preview_zip_note")),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .rounded(px(ui::R_MD))
                            .border_1()
                            .border_color(crate::theme::divider())
                            .bg(crate::theme::hover_bg())
                            .overflow_hidden()
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full()
                                    .p(px(ui::SP_2))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        gpui::img(img.clone())
                                            .w_full()
                                            .h_full()
                                            .max_w(px(w))
                                            .max_h(px(h)),
                                    ),
                            ),
                    ),
            );
        }
        col.into_any_element()
    }

    /// Markdown 转 HTML：预览页渲染输入的 Markdown，源码页是生成的 HTML（复制 / 保存始终是 HTML）。
    fn render_markdown(&self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let tabs = [
            ("preview".to_string(), t("tools.tab_preview").to_string()),
            ("source".to_string(), t("tools.tab_source").to_string()),
        ];
        let bar = div()
            .mx(px(SUB_PAD))
            .mt(px(ui::SP_2))
            .flex_shrink_0()
            .child(ui::segmented(
                "md-tab",
                &tabs,
                self.tab,
                move |v, _w, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.tab = if v == "source" { "source" } else { "preview" };
                        cx.notify();
                    });
                },
            ));
        let body: AnyElement = if self.tab == "source" {
            sub_card_body()
                .flex_1()
                .min_h_0()
                .child(Editor::new(&self.out_ed).h_full())
                .into_any_element()
        } else {
            let src = self
                .areas
                .get("input")
                .map(|a| a.read(cx).value().to_string())
                .unwrap_or_default();
            div()
                .id("md-preview")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(SUB_PAD))
                .py(px(ui::SP_2))
                .child(gpui_component::text::TextView::markdown("tool-md", src).selectable(true))
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(body)
            .into_any_element()
    }

    /// 正则结果：匹配 / 替换两页。匹配页是原文高亮预览 + 逐处匹配；替换页是替换后的全文。
    fn render_regex(&self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let tabs = [
            ("match".to_string(), t("tools.tab_match").to_string()),
            ("replace".to_string(), t("tools.tab_replace").to_string()),
        ];
        let bar = div()
            .mx(px(SUB_PAD))
            .mt(px(ui::SP_2))
            .flex_shrink_0()
            .child(ui::segmented(
                "rx-tab",
                &tabs,
                self.tab,
                move |v, _w, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.tab = if v == "replace" { "replace" } else { "match" };
                        cx.notify();
                    });
                },
            ));
        let body: AnyElement = if self.tab == "replace" {
            if self.output.alt.is_none() {
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(SUB_PAD))
                    .text_size(px(ui::fs_small()))
                    .text_color(c(cur().subtext))
                    .child(t("tools.rx_replace_hint"))
                    .into_any_element()
            } else {
                sub_card_body()
                    .flex_1()
                    .min_h_0()
                    .child(Editor::new(&self.out_ed).h_full())
                    .into_any_element()
            }
        } else {
            let src = self
                .output
                .source
                .as_ref()
                .map(|s| result_list::source_preview("rx-src", s));
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .children(src)
                .child(self.render_kv(false, cx))
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(body)
            .into_any_element()
    }

    /// 输出区是否已有可显示的结果。
    fn has_result(&self) -> bool {
        let o = &self.output;
        !o.rows.is_empty() || o.file.is_some() || !o.text.is_empty() || o.verdict.is_some()
    }

    /// 输出是否占满剩余空间：文本、图片预览、历史、批量列表、正则占满；键值行和无预览的文件卡片贴合内容。
    /// 结果出现前按工具本身的形态决定。
    fn out_fill(&self) -> bool {
        if tools::has_history(&self.spec.id) {
            return true;
        }
        let fit = tools::out_fit(&self.spec.id);
        if self.output.file.is_some() {
            if self.preview.is_some() {
                return true;
            }
            // PDF 即使还没有预览图也撑开，用来放「正在生成」或未安装说明。
            return self
                .output
                .info
                .as_ref()
                .is_some_and(|i| i.kind == tools::fileinfo::FileKind::Pdf);
        }
        if !self.has_result() {
            return !fit;
        }
        match self.output.kind {
            OutKind::Kv => self.output.source.is_some() || !fit,
            OutKind::Text | OutKind::Rows => true,
        }
    }

    /// 拖动输入与输出之间的分隔条：上下排列按纵向、并排按横向换算输入区占的比例。
    fn drag_split(&mut self, ev: &gpui::DragMoveEvent<ToolSplit>, cx: &mut Context<Self>) {
        let b = ev.bounds;
        let r = if self.side {
            lay::split_ratio(
                f32::from(ev.event.position.x),
                f32::from(b.left()),
                f32::from(b.size.width) - CARD_PAD,
                CARD_PAD,
            )
        } else {
            let gaps = if self.has_opts { 2. } else { 1. };
            let free = f32::from(b.size.height) - self.opts_h.get() - gaps * CARD_PAD;
            lay::split_ratio(
                f32::from(ev.event.position.y),
                f32::from(b.top()),
                free,
                CARD_PAD,
            )
        };
        self.dragging = true;
        if (r - self.split).abs() > 0.002 {
            self.split = r;
            cx.notify();
        }
    }

    fn field_label(s: &str) -> impl IntoElement {
        div()
            .text_size(px(ui::fs_small()))
            .text_color(c(cur().overlay1))
            .child(s.to_string())
    }

    /// 选项多或带颜色色板时分多行展示；单个分段选择不再强撑满整行。
    fn opts_stacked(spec: &ToolSpec) -> bool {
        spec.options.len() >= 5 || spec.options.iter().any(|f| f.kind == FieldKind::Color)
    }

    /// 平铺间距只在位置选了平铺时出现。两个水印工具的字段都叫 `gap` / `pos`。
    fn option_shown(&self, f: &tools::Field) -> bool {
        if f.id == "gap" {
            return self.picks.get("pos").map(|s| s.as_str()) == Some("tile");
        }
        true
    }

    /// 紧凑内联选项控件（用于顶部工具栏、生成器工作台、查询栏）：标签与控件同行，不强撑 100% 宽。
    fn inline_option_el(
        &self,
        f: &tools::Field,
        me: &gpui::WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cur_v = self.picks.get(&f.id).cloned().unwrap_or_default();
        match f.kind {
            FieldKind::Select => {
                let (id, me) = (f.id.clone(), me.clone());
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .flex_shrink_0()
                    .when(!f.label.is_empty(), |d| d.child(Self::field_label(&f.label)))
                    .child(ui::segmented(
                        intern(&format!("opt-{}", f.id)),
                        &f.choices,
                        &cur_v,
                        move |v, w, cx| {
                            let _ = me.update(cx, |this, cx| this.set_pick_in(id.clone(), v, w, cx));
                        },
                    ))
                    .into_any_element()
            }
            FieldKind::Toggle => {
                let id = f.id.clone();
                gpui_component::switch::Switch::new(SharedString::from(format!("tg-{}", f.id)))
                    .checked(cur_v == "true")
                    .label(f.label.clone())
                    .small()
                    .on_click(cx.listener(move |this, on: &bool, w, cx| {
                        this.set_pick_in(id.clone(), on.to_string(), w, cx)
                    }))
                    .into_any_element()
            }
            FieldKind::Text | FieldKind::Number => {
                let w = if f.kind == FieldKind::Number {
                    88.
                } else {
                    136.
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .flex_shrink_0()
                    .when(!f.label.is_empty(), |d| d.child(Self::field_label(&f.label)))
                    .child(div().w(px(w)).child(ui::text_field(&self.texts[&f.id], false)))
                    .into_any_element()
            }
            FieldKind::Color => self.option_el(f, me, cx),
            _ => div().into_any_element(),
        }
    }

    fn option_el(
        &self,
        f: &tools::Field,
        me: &gpui::WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cur_v = self.picks.get(&f.id).cloned().unwrap_or_default();
        match f.kind {
            FieldKind::Select => {
                let (id, me) = (f.id.clone(), me.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Self::field_label(&f.label))
                    .child(ui::segmented(
                        intern(&format!("opt-{}", f.id)),
                        &f.choices,
                        &cur_v,
                        move |v, w, cx| {
                            let _ = me.update(cx, |this, cx| this.set_pick_in(id.clone(), v, w, cx));
                        },
                    ))
                    .into_any_element()
            }
            FieldKind::Toggle => {
                let id = f.id.clone();
                gpui_component::switch::Switch::new(SharedString::from(format!("tg-{}", f.id)))
                    .checked(cur_v == "true")
                    .label(f.label.clone())
                    .small()
                    .on_click(cx.listener(move |this, on: &bool, w, cx| {
                        this.set_pick_in(id.clone(), on.to_string(), w, cx)
                    }))
                    .into_any_element()
            }
            FieldKind::Text | FieldKind::Number => div()
                .w(px(140.))
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::field_label(&f.label))
                .child(ui::text_field(&self.texts[&f.id], false))
                .into_any_element(),
            FieldKind::Color => {
                let (id, me) = (f.id.clone(), me.clone());
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Self::field_label(&f.label))
                    .child(ui::swatch_row(
                        "mark-color",
                        &mark_colors(),
                        Some(cur_v.as_str()),
                        move |v, w, cx| {
                            let _ = me.update(cx, |this, cx| this.set_pick_in(id.clone(), v, w, cx));
                        },
                    ))
                    .into_any_element()
            }
            _ => div().into_any_element(),
        }
    }

    fn action_pill(
        id: impl Into<gpui::ElementId>,
        ic: IconName,
        label: &'static str,
    ) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        div()
            .id(id)
            .h(px(ui::H_COMPACT))
            .px_2()
            .rounded(px(ui::R_MD))
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .flex_shrink_0()
            .text_size(px(ui::fs_small()))
            .text_color(c(th.subtext))
            .hover(|s| s.bg(crate::theme::hover_bg()))
            .child(icon(ic, ui::IC_XS, c(th.overlay1)))
            .child(label)
    }

    /// 构建标准输出卡片（统一承载错误、判定横幅、历史、文件、Markdown、正则或文本/KV）。
    fn build_out_card(&self, out_fill: bool, both: bool, side: bool, cx: &mut Context<Self>) -> gpui::Div {
        let grow = |share: f32| (share * 1000.).max(1.);
        let mut out = sub_card().flex().flex_col().child(self.out_head(true, cx));
        if let Some(e) = &self.error {
            out = out.child(
                ui::notice(ui::Notice::Error, e.clone())
                    .mx(px(SUB_PAD))
                    .my(px(ui::SP_2)),
            );
        }
        if let (Some(v), false) = (&self.output.verdict, self.error.is_some()) {
            out = out.child(result_list::verdict_banner(v));
        }
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
            if let (Some([r, g, b, a]), false) = (self.output.color, self.spec.id == "color") {
                out = out.child(
                    div()
                        .px(px(SUB_PAD))
                        .pt(px(SUB_PAD))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(ui::H_DEFAULT))
                                .rounded(px(ui::R_MD))
                                .border_1()
                                .border_color(crate::theme::divider())
                                .bg(gpui::Rgba {
                                    r: r as f32 / 255.,
                                    g: g as f32 / 255.,
                                    b: b as f32 / 255.,
                                    a: a as f32 / 255.,
                                }),
                        ),
                );
            }
            out = out.child(
                sub_card_body()
                    .min_h(px(lay::OUT_EDITOR_MIN_H))
                    .child(Editor::new(&self.out_ed).h_full()),
            );
        }
        if out_fill {
            let base = out.min_h(px(lay::OUT_FILL_MIN_H)).min_w(px(if side {
                lay::SIDE_PANE_MIN_W
            } else {
                0.
            }));
            if both {
                base.flex_basis(px(0.))
                    .flex_grow(grow(1. - self.split))
                    .flex_shrink(1.)
            } else {
                base.flex_1()
            }
        } else {
            out.flex_grow(0.)
                .flex_shrink(1.)
                .min_h(px(lay::fit_min_h()))
        }
    }

    /// 正则测试专属工作台：顶部 `/pattern/flags` + 常用正则预设 + 替换串，下方测试文本与匹配结果分栏对照。
    fn render_regex_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let side = lay::side_by_side(self.body_w.get());
        self.side = side;
        let opts_h = self.opts_h.clone();
        let mut flags_row = div().flex().items_center().gap(px(ui::SP_1));
        for (fid, short, tip_txt) in [
            ("i", "i", t("tools.o_ignore_case")),
            ("m", "m", t("tools.o_multiline")),
            ("s", "s", t("tools.o_dotall")),
        ] {
            let on = self.picks.get(fid).map(|s| s.as_str()) == Some("true");
            let fid_s = fid.to_string();
            flags_row = flags_row.child(
                div()
                    .id(SharedString::from(format!("rx-fl-{fid}")))
                    .tooltip(ui::tip(tip_txt))
                    .child(
                        ui::chip(SharedString::from(format!("rx-fc-{fid}")), short, on).on_click(
                            cx.listener(move |this, _, w, cx| {
                                let cur_on =
                                    this.picks.get(&fid_s).map(|s| s.as_str()) == Some("true");
                                this.set_pick_in(fid_s.clone(), (!cur_on).to_string(), w, cx);
                            }),
                        ),
                    ),
            );
        }
        let mut presets = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
        presets = presets.child(
            div()
                .text_size(px(ui::fs_caption()))
                .text_color(c(th.overlay1))
                .child(t("ux.presets")),
        );
        for (i, (label, pat, sample)) in [
            (
                t("ux.rx_email"),
                r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}",
                "ops@shellax.dev, billing@company.co.uk",
            ),
            (
                t("ux.rx_phone"),
                r"1[3-9]\d{9}",
                "联系人：13800138000 / 备用：18612345678",
            ),
            (
                t("ux.rx_ipv4"),
                r"\b(?:\d{1,3}\.){3}\d{1,3}\b",
                "eth0 192.168.1.10 -> gateway 10.0.0.1",
            ),
            (
                t("ux.rx_url"),
                r"https?://[^\s/$.?#].[^\s]*",
                "Docs: https://shellax.dev/docs and http://localhost:8080/health",
            ),
            (
                t("ux.rx_date"),
                r"\b\d{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12]\d|3[01])\b",
                "deploy 2026-11-22, rollback 2026-10-01",
            ),
            (
                t("ux.rx_tag"),
                r"<([a-zA-Z][a-zA-Z0-9]*)\b[^>]*>(.*?)</\1>",
                "<div class=\"card\">Hello</div> <span>World</span>",
            ),
            (
                t("ux.rx_cjk"),
                r"[\p{Han}]+",
                "ShellAX 终端与数据库 Workbench 2.0",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let (p, sm) = (pat.to_string(), sample.to_string());
            presets = presets.child(ui::chip(("rx-pre", i), label, false).on_click(cx.listener(
                move |this, _, w, cx| {
                    this.set_field("pattern", p.clone(), w, cx);
                    if this
                        .areas
                        .get("input")
                        .is_some_and(|a| a.read(cx).value().trim().is_empty())
                    {
                        this.set_field("input", sm.clone(), w, cx);
                    }
                    this.run(cx);
                },
            )));
        }
        let top_bar = sub_card()
            .w_full()
            .relative()
            .p(px(SUB_PAD))
            .flex()
            .flex_col()
            .gap_2()
            .flex_shrink_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .font_family(ui::mono_family())
                            .text_size(px(ui::fs_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.accent))
                            .child("/"),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            ui::text_field(&self.texts["pattern"], false)
                                .font_family(ui::mono_family()),
                        ),
                    )
                    .child(
                        div()
                            .font_family(ui::mono_family())
                            .text_size(px(ui::fs_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.accent))
                            .child("/"),
                    )
                    .child(flags_row)
                    .child(
                        Self::action_pill("rx-sample", IconName::Sparkles, t("ux.sample"))
                            .on_click(cx.listener(|this, _, w, cx| this.fill_sample(w, cx))),
                    ),
            )
            .child(presets)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(64.))
                            .flex_shrink_0()
                            .child(Self::field_label(t("tools.f_replace"))),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            ui::text_field(&self.texts["replace"], false)
                                .font_family(ui::mono_family()),
                        ),
                    ),
            )
            .child(
                gpui::canvas(
                    move |b, _, _| opts_h.set(f32::from(b.size.height)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        self.has_opts = true;
        let grow = |share: f32| (share * 1000.).max(1.);
        let stat = self
            .areas
            .get("input")
            .map(|a| tools::text_stats(&a.read(cx).value()))
            .filter(|s| s.1 > 0);
        let in_head = ui::toolbar()
            .child(
                div()
                    .flex_1()
                    .text_size(px(ui::fs_small()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(th.overlay1))
                    .child(t("ux.rx_test_text")),
            )
            .when_some(stat, |d, (l, n)| {
                d.child(
                    div()
                        .text_size(px(ui::fs_caption()))
                        .text_color(c(th.overlay1))
                        .child(tf("tools.ui_stat", &[("lines", &l), ("chars", &n)])),
                )
            })
            .child(
                Self::action_pill("rx-paste", IconName::ClipboardPaste, t("ux.paste"))
                    .on_click(cx.listener(|this, _, w, cx| this.paste_field("input", w, cx))),
            )
            .child(
                Self::action_pill("rx-clear", IconName::Eraser, t("tools.ui_clear"))
                    .on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx))),
            );
        let input = sub_card()
            .flex()
            .flex_col()
            .min_h(px(lay::OUT_FILL_MIN_H))
            .min_w(px(if side { lay::SIDE_PANE_MIN_W } else { 0. }))
            .flex_basis(px(0.))
            .flex_grow(grow(self.split))
            .flex_shrink(1.)
            .child(in_head)
            .child(
                sub_card_body()
                    .min_h(px(lay::OUT_EDITOR_MIN_H))
                    .child(Editor::new(&self.areas["input"]).h_full()),
            );
        let out = self.build_out_card(true, true, side, cx);
        let split_handle = lay::handle("tool-split", side).on_click(cx.listener(
            |this, ev: &gpui::ClickEvent, _w, cx| {
                if ev.click_count() >= 2 {
                    this.split = lay::SPLIT_DEFAULT;
                    this.dragging = true;
                    cx.notify();
                }
            },
        ));
        if side {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(top_bar)
                .child(lay::gap(false))
                .child(
                    div()
                        .id("tool-split-row")
                        .flex_1()
                        .min_h(px(lay::OUT_FILL_MIN_H))
                        .flex()
                        .flex_row()
                        .on_drag_move(cx.listener(
                            |this, ev: &gpui::DragMoveEvent<ToolSplit>, _w, cx| {
                                this.drag_split(ev, cx)
                            },
                        ))
                        .child(input)
                        .child(split_handle)
                        .child(out),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .on_drag_move(cx.listener(|this, ev: &gpui::DragMoveEvent<ToolSplit>, _w, cx| {
                    this.drag_split(ev, cx)
                }))
                .child(top_bar)
                .child(lay::gap(false))
                .child(input)
                .child(split_handle)
                .child(out)
                .into_any_element()
        }
    }

    /// 时间戳专属工作台：实时跳动的「当前时间」英雄横幅（秒/毫秒一键复制或填入）+ 偏移快捷按钮 + 多时区结果。
    fn render_timestamp_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let now = chrono::Local::now();
        let now_s = now.timestamp();
        let now_ms = now.timestamp_millis();
        let now_iso = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let s_str = now_s.to_string();
        let ms_str = now_ms.to_string();
        let iso_str = now_iso.clone();
        let cp_pill = |id: &'static str, lbl: &'static str, val: String, cx: &mut Context<Self>| {
            let v2 = val.clone();
            let copied = self.copied.as_deref() == Some(id);
            div()
                .id(id)
                .px(px(ui::SP_2 + 2.))
                .py(px(4.))
                .rounded(px(ui::R_MD))
                .bg(if copied {
                    c(th.success).opacity(0.14)
                } else {
                    crate::theme::hover_bg()
                })
                .border_1()
                .border_color(crate::theme::divider())
                .cursor_pointer()
                .hover(|s| s.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |this, _, _w, cx| {
                    this.copy_text(id.into(), v2.clone(), cx);
                }))
                .flex()
                .items_center()
                .gap(px(ui::SP_2))
                .child(
                    div()
                        .text_size(px(ui::fs_caption()))
                        .text_color(c(th.overlay1))
                        .child(lbl),
                )
                .child(
                    div()
                        .font_family(ui::mono_family())
                        .text_size(px(ui::fs_body()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(th.text))
                        .child(val),
                )
                .child(icon(
                    if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    },
                    ui::IC_XS,
                    if copied { c(th.success) } else { c(th.overlay1) },
                ))
        };
        let hero = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(ui::SP_2))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(ui::SP_2))
                    .child(ui::tag(t("ux.now_title"), c(th.accent)))
                    .child(cp_pill("ts-now-s", "Unix (s)", s_str.clone(), cx))
                    .child(cp_pill("ts-now-ms", "Unix (ms)", ms_str.clone(), cx))
                    .child(cp_pill("ts-now-iso", "Local", iso_str, cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(ui::SP_2))
                    .child(div().flex_1().min_w(px(200.)).child(
                        ui::text_field(&self.texts["input"], false).font_family(ui::mono_family()),
                    ))
                    .child(
                        ui::chip("ts-fill-s", t("ux.use_sec"), false).on_click(cx.listener(
                            move |this, _, w, cx| {
                                let v = chrono::Local::now().timestamp().to_string();
                                this.set_field("input", v, w, cx);
                                this.run(cx);
                            },
                        )),
                    )
                    .child(
                        ui::chip("ts-fill-ms", t("ux.use_ms"), false).on_click(cx.listener(
                            move |this, _, w, cx| {
                                let v = chrono::Local::now().timestamp_millis().to_string();
                                this.set_field("input", v, w, cx);
                                this.run(cx);
                            },
                        )),
                    )
                    .child(
                        ui::chip("ts-zero", t("ux.today_zero"), false).on_click(cx.listener(
                            move |this, _, w, cx| {
                                let v = chrono::Local::now()
                                    .format("%Y-%m-%d 00:00:00")
                                    .to_string();
                                this.set_field("input", v, w, cx);
                                this.run(cx);
                            },
                        )),
                    )
                    .child(
                        ui::chip("ts-m1h", t("ux.minus_1h"), false).on_click(cx.listener(
                            move |this, _, w, cx| {
                                let v = (chrono::Local::now().timestamp() - 3600).to_string();
                                this.set_field("input", v, w, cx);
                                this.run(cx);
                            },
                        )),
                    )
                    .child(
                        ui::chip("ts-p1d", t("ux.plus_1d"), false).on_click(cx.listener(
                            move |this, _, w, cx| {
                                let v = (chrono::Local::now().timestamp() + 86400).to_string();
                                this.set_field("input", v, w, cx);
                                this.run(cx);
                            },
                        )),
                    )
                    .child(
                        Self::action_pill("ts-paste", IconName::ClipboardPaste, t("ux.paste"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.paste_field("input", w, cx)),
                            ),
                    )
                    .child(
                        Self::action_pill("ts-clear", IconName::Eraser, t("tools.ui_clear"))
                            .on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx))),
                    ),
            );
        let show_out = self.has_result() || self.error.is_some();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(hero)
            .when(show_out, |d| d.child(self.build_out_card(false, false, false, cx)))
            .into_any_element()
    }

    /// Cron 专属工作台：5/6 字段可视化拆解 + 常用调度预设卡片 + 未来触发时间表。
    fn render_cron_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let raw = self
            .texts
            .get("input")
            .map(|e| e.read(cx).value().to_string())
            .unwrap_or_default();
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        let labels = [
            t("ux.cron_f_min"),
            t("ux.cron_f_hour"),
            t("ux.cron_f_day"),
            t("ux.cron_f_mon"),
            t("ux.cron_f_dow"),
        ];
        let mut fields_bar = div().flex().items_center().flex_wrap().gap(px(ui::SP_2));
        let offset = if tokens.len() == 6 { 1 } else { 0 };
        if offset == 1 {
            fields_bar = fields_bar.child(
                div()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(ui::R_SM))
                    .bg(crate::theme::hover_bg())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(ui::fs_caption()))
                            .text_color(c(th.overlay1))
                            .child("Sec"),
                    )
                    .child(
                        div()
                            .font_family(ui::mono_family())
                            .text_size(px(ui::fs_body()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.accent))
                            .child(tokens[0].to_string()),
                    ),
            );
        }
        for (i, lbl) in labels.iter().enumerate() {
            let tok = tokens.get(i + offset).copied().unwrap_or("—");
            fields_bar = fields_bar.child(
                div()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(ui::R_SM))
                    .bg(crate::theme::hover_bg())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(ui::fs_caption()))
                            .text_color(c(th.overlay1))
                            .child((*lbl).to_string()),
                    )
                    .child(
                        div()
                            .font_family(ui::mono_family())
                            .text_size(px(ui::fs_body()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.accent))
                            .child(tok.to_string()),
                    ),
            );
        }
        let mut presets = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
        for (i, (lbl, expr)) in [
            (t("ux.cron_every_min"), "* * * * *"),
            (t("ux.cron_5m"), "*/5 * * * *"),
            (t("ux.cron_15m"), "*/15 * * * *"),
            (t("ux.cron_hourly"), "0 * * * *"),
            (t("ux.cron_daily"), "0 0 * * *"),
            (t("ux.cron_morning"), "0 9 * * *"),
            (t("ux.cron_workday"), "30 9 * * 1-5"),
            (t("ux.cron_weekly"), "0 0 * * 1"),
            (t("ux.cron_monthly"), "0 0 1 * *"),
        ]
        .into_iter()
        .enumerate()
        {
            let on = raw.trim() == expr;
            let ex = expr.to_string();
            presets = presets.child(ui::chip(("cron-pre", i), lbl, on).on_click(cx.listener(
                move |this, _, w, cx| {
                    this.set_field("input", ex.clone(), w, cx);
                    this.run(cx);
                },
            )));
        }
        let me = cx.entity().downgrade();
        let top = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(ui::SP_2))
            .child(presets)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .child(div().flex_1().min_w_0().child(
                        ui::text_field(&self.texts["input"], false).font_family(ui::mono_family()),
                    ))
                    .children(
                        self.spec
                            .options
                            .first()
                            .map(|f| self.inline_option_el(f, &me, cx)),
                    )
                    .child(
                        Self::action_pill("cron-paste", IconName::ClipboardPaste, t("ux.paste"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.paste_field("input", w, cx)),
                            ),
                    ),
            )
            .child(fields_bar);
        let show_out = self.has_result() || self.error.is_some();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(top)
            .when(show_out, |d| d.child(self.build_out_card(false, false, false, cx)))
            .into_any_element()
    }

    /// 颜色转换专属工作台：调色板快捷色样 + 双背景可读性对比大色卡 + 多格式一键复制。
    fn render_color_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let cur_in = self
            .texts
            .get("input")
            .map(|e| e.read(cx).value().trim().to_lowercase())
            .unwrap_or_default();
        let swatches: Vec<(String, gpui::Hsla, String)> = [
            "#3b82f6", "#6366f1", "#8b5cf6", "#ec4899", "#ef4444", "#f97316", "#eab308", "#10b981",
            "#06b6d4", "#64748b", "#0f172a", "#f8fafc",
        ]
        .iter()
        .map(|hex| {
            let n = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
            let col = gpui::Rgba {
                r: ((n >> 16) & 255) as f32 / 255.,
                g: ((n >> 8) & 255) as f32 / 255.,
                b: (n & 255) as f32 / 255.,
                a: 1.,
            };
            ((*hex).to_string(), col.into(), (*hex).to_string())
        })
        .collect();
        let me = cx.entity().downgrade();
        let mut studio = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(ui::SP_3))
            .child(ui::swatch_row(
                "color-studio-sw",
                &swatches,
                Some(cur_in.as_str()),
                move |v, w, cx| {
                    let _ = me.update(cx, |this, cx| {
                        this.set_field("input", v, w, cx);
                        this.run(cx);
                    });
                },
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .child(div().flex_1().min_w_0().child(
                        ui::text_field(&self.texts["input"], false).font_family(ui::mono_family()),
                    ))
                    .child(
                        Self::action_pill("col-paste", IconName::ClipboardPaste, t("ux.paste"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.paste_field("input", w, cx)),
                            ),
                    )
                    .child(
                        Self::action_pill("col-clear", IconName::Eraser, t("tools.ui_clear"))
                            .on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx))),
                    ),
            );
        if let Some([r, g, b, a]) = self.output.color {
            let bg = gpui::Rgba {
                r: r as f32 / 255.,
                g: g as f32 / 255.,
                b: b as f32 / 255.,
                a: a as f32 / 255.,
            };
            let hex_label = format!("#{:02x}{:02x}{:02x}", r, g, b);
            studio = studio.child(
                div()
                    .flex()
                    .gap(px(ui::SP_2))
                    .child(
                        div()
                            .flex_1()
                            .h(px(ui::H_LARGE * 2.))
                            .rounded(px(ui::R_MD))
                            .border_1()
                            .border_color(crate::theme::divider())
                            .bg(bg)
                            .px(px(ui::SP_3))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .child(
                                div()
                                    .text_size(px(ui::fs_caption()))
                                    .text_color(gpui::white().opacity(0.82))
                                    .child(t("ux.color_light_fg")),
                            )
                            .child(
                                div()
                                    .font_family(ui::mono_family())
                                    .text_size(px(ui::fs_title()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(gpui::white())
                                    .child(hex_label.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(ui::H_LARGE * 2.))
                            .rounded(px(ui::R_MD))
                            .border_1()
                            .border_color(crate::theme::divider())
                            .bg(bg)
                            .px(px(ui::SP_3))
                            .flex()
                            .flex_col()
                            .justify_center()
                            .child(
                                div()
                                    .text_size(px(ui::fs_caption()))
                                    .text_color(c(0x0f172a).opacity(0.76))
                                    .child(t("ux.color_dark_fg")),
                            )
                            .child(
                                div()
                                    .font_family(ui::mono_family())
                                    .text_size(px(ui::fs_title()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(c(0x0f172a))
                                    .child(hex_label),
                            ),
                    ),
            );
        }
        let show_out = self.has_result() || self.error.is_some();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(studio)
            .when(show_out, |d| d.child(self.build_out_card(false, false, false, cx)))
            .into_any_element()
    }

    /// 单位换算专属工作台：顶部分类切换 + 动态源单位快捷胶囊（不再出现切分类后单位不匹配的报错）。
    fn render_units_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let cur_kind = self
            .picks
            .get("kind")
            .cloned()
            .unwrap_or_else(|| "length".into());
        let cur_from = self
            .texts
            .get("from")
            .map(|e| e.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let mut kind_chips = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
        if let Some(kf) = self.spec.options.first() {
            for (i, (val, lbl)) in kf.choices.iter().enumerate() {
                let on = cur_kind == *val;
                let v = val.clone();
                let me2 = me.clone();
                kind_chips =
                    kind_chips.child(ui::chip(("uk-chip", i), lbl.clone(), on).on_click(
                        move |_, w, cx| {
                            let _ = me2.update(cx, |this, cx| {
                                this.set_pick_in("kind".into(), v.clone(), w, cx)
                            });
                        },
                    ));
            }
        }
        let mut unit_pills = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
        for (i, u) in unit_choices_for(&cur_kind).iter().enumerate() {
            let on = cur_from == *u;
            let u_s = (*u).to_string();
            unit_pills = unit_pills.child(ui::chip(("uu-pill", i), *u, on).on_click(cx.listener(
                move |this, _, w, cx| {
                    this.set_field("from", u_s.clone(), w, cx);
                    this.run(cx);
                },
            )));
        }
        let top = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(ui::SP_2))
            .child(kind_chips)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .child(div().flex_1().min_w(px(140.)).child(
                        ui::text_field(&self.texts["input"], false).font_family(ui::mono_family()),
                    ))
                    .child(div().w(px(96.)).child(
                        ui::text_field(&self.texts["from"], false).font_family(ui::mono_family()),
                    )),
            )
            .child(unit_pills);
        let show_out = self.has_result() || self.error.is_some();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(top)
            .when(show_out, |d| d.child(self.build_out_card(false, false, false, cx)))
            .into_any_element()
    }

    /// 生成器与零输入工具专属工作台（随机密码、UUID、随机数、网卡信息、贷款计算）。
    fn render_generator_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let id = self.spec.id.as_str();
        if id == "loan" {
            let mut grid = div().flex().items_end().flex_wrap().gap(px(ui::SP_3));
            for f in self.spec.options.clone() {
                grid = grid.child(self.option_el(&f, &me, cx));
            }
            let mut presets = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
            for (i, (lbl, p, r, y)) in [
                ("100万 · 30年 · 3.1%", "1000000", "3.1", "30"),
                ("200万 · 30年 · 3.1%", "2000000", "3.1", "30"),
                ("50万 · 10年 · 3.45%", "500000", "3.45", "10"),
            ]
            .into_iter()
            .enumerate()
            {
                presets = presets.child(ui::chip(("loan-pre", i), lbl, false).on_click(
                    cx.listener(move |this, _, w, cx| {
                        this.set_field("principal", p.into(), w, cx);
                        this.set_field("rate", r.into(), w, cx);
                        this.set_field("years", y.into(), w, cx);
                        this.run(cx);
                    }),
                ));
            }
            let card = sub_card()
                .p(px(SUB_PAD))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .gap(px(ui::SP_2))
                .child(presets)
                .child(grid);
            let show_out = self.has_result() || self.error.is_some();
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(CARD_PAD))
                .child(card)
                .when(show_out, |d| d.child(self.build_out_card(false, false, false, cx)))
                .into_any_element();
        }
        let mut quick = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
        if id == "password" {
            let cur_len = self
                .texts
                .get("len")
                .map(|e| e.read(cx).value().trim().to_string())
                .unwrap_or_default();
            for (i, l) in ["8", "12", "16", "24", "32", "64"].into_iter().enumerate() {
                let on = cur_len == l;
                let ls = l.to_string();
                quick = quick.child(
                    ui::chip(("pw-len", i), format!("{l} chars"), on).on_click(cx.listener(
                        move |this, _, w, cx| {
                            this.set_field("len", ls.clone(), w, cx);
                            this.run(cx);
                        },
                    )),
                );
            }
        } else if id == "uuid" {
            let cur_cnt = self
                .texts
                .get("count")
                .map(|e| e.read(cx).value().trim().to_string())
                .unwrap_or_default();
            for (i, c_n) in ["1", "5", "10", "20", "50"].into_iter().enumerate() {
                let on = cur_cnt == c_n;
                let cs = c_n.to_string();
                quick = quick.child(
                    ui::chip(("uuid-cnt", i), format!("×{c_n}"), on).on_click(cx.listener(
                        move |this, _, w, cx| {
                            this.set_field("count", cs.clone(), w, cx);
                            this.run(cx);
                        },
                    )),
                );
            }
        }
        let mut controls = div().flex().items_center().flex_wrap().gap(px(ui::SP_3));
        for f in self.spec.options.clone() {
            controls = controls.child(self.inline_option_el(&f, &me, cx));
        }
        let has_latest = if tools::has_history(id) {
            !self.hist.borrow().is_empty()
        } else {
            self.has_result()
        };
        let deck = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(ui::SP_2))
            .when(id == "password" || id == "uuid", |d| d.child(quick))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(ui::SP_3))
                    .child(div().flex_1().min_w_0().child(controls))
                    .when(has_latest, |d| {
                        d.child(
                            ui::button("gen-copy-latest", t("ux.copy_latest"), 0)
                                .on_click(cx.listener(|this, _, _w, cx| this.copy(cx))),
                        )
                    })
                    .child(
                        ui::button(
                            "tool-run",
                            if id == "ifaces" {
                                t("tools.ui_refresh")
                            } else {
                                t("ux.generate")
                            },
                            1,
                        )
                        .on_click(cx.listener(|this, _, _w, cx| this.run(cx))),
                    ),
            );
        let out_fill = self.out_fill();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(deck)
            .child(self.build_out_card(out_fill, false, false, cx))
            .into_any_element()
    }

    /// 单行查询栏工具（IP 查询、TCP 延迟、CIDR、IP 转换、进制转换、身份证校验、金额大写）：合一搜索栏 + 预设胶囊。
    fn render_omnibox_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let id = self.spec.id.clone();
        let mine = self.lookup_mine();
        let can_run = self.lookup_can_run(cx);
        let mut opts_inline = div().flex().items_center().flex_wrap().gap(px(ui::SP_2));
        for f in self.spec.options.clone() {
            if self.option_shown(&f) {
                opts_inline = opts_inline.child(self.inline_option_el(&f, &me, cx));
            }
        }
        let mut presets = div().flex().items_center().flex_wrap().gap(px(ui::SP_1));
        let mut has_presets = false;
        match id.as_str() {
            "ip_lookup" => {
                has_presets = true;
                presets = presets.child(
                    ui::chip("ip-mine-chip", t("tools.net_run_mine"), mine).on_click(cx.listener(
                        |this, _, w, cx| {
                            this.set_pick_in("mine".into(), "true".into(), w, cx);
                            this.run(cx);
                        },
                    )),
                );
                for (i, ip) in ["8.8.8.8", "1.1.1.1", "223.5.5.5"].into_iter().enumerate() {
                    let s = ip.to_string();
                    presets = presets.child(ui::chip(("ip-pre", i), ip, false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.picks.insert("mine".into(), "false".into());
                            this.set_field("input", s.clone(), w, cx);
                            this.run(cx);
                        }),
                    ));
                }
            }
            "ip_latency" => {
                has_presets = true;
                for (i, (lbl, host, port)) in [
                    ("Cloudflare 1.1.1.1:443", "1.1.1.1", "443"),
                    ("Google DNS 8.8.8.8:53", "8.8.8.8", "53"),
                    ("AliDNS 223.5.5.5:53", "223.5.5.5", "53"),
                    ("GitHub 443", "github.com", "443"),
                ]
                .into_iter()
                .enumerate()
                {
                    presets = presets.child(ui::chip(("lat-pre", i), lbl, false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.set_field("input", host.into(), w, cx);
                            this.set_field("port", port.into(), w, cx);
                            this.run(cx);
                        }),
                    ));
                }
            }
            "cidr" => {
                has_presets = true;
                for (i, cidr) in [
                    "192.168.1.0/24",
                    "10.0.0.0/16",
                    "172.16.0.0/12",
                    "192.168.1.0/28",
                ]
                .into_iter()
                .enumerate()
                {
                    let s = cidr.to_string();
                    presets = presets.child(ui::chip(("cidr-pre", i), cidr, false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.set_field("input", s.clone(), w, cx);
                            this.run(cx);
                        }),
                    ));
                }
            }
            "radix" => {
                has_presets = true;
                for (i, v) in ["0xff", "0b101010", "0o755", "65535", "2147483647"]
                    .into_iter()
                    .enumerate()
                {
                    let s = v.to_string();
                    presets = presets.child(ui::chip(("rx-num-pre", i), v, false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.set_field("input", s.clone(), w, cx);
                            this.run(cx);
                        }),
                    ));
                }
            }
            "rmb" | "en_amount" => {
                has_presets = true;
                for (i, v) in ["1234.56", "10000", "99999.99", "1000000"]
                    .into_iter()
                    .enumerate()
                {
                    let s = v.to_string();
                    presets = presets.child(ui::chip(("amt-pre", i), v, false).on_click(
                        cx.listener(move |this, _, w, cx| {
                            this.set_field("input", s.clone(), w, cx);
                            this.run(cx);
                        }),
                    ));
                }
            }
            _ => {}
        }
        let run_label = if mine {
            t("tools.net_run_mine")
        } else {
            t("tools.ui_run")
        };
        let omnibox = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(ui::SP_2))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(ui::SP_2))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(180.))
                            .when(mine, |d| d.opacity(0.45))
                            .child(
                                ui::text_field(&self.texts["input"], false)
                                    .font_family(ui::mono_family()),
                            ),
                    )
                    .child(
                        Self::action_pill("omni-paste", IconName::ClipboardPaste, t("ux.paste"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.paste_field("input", w, cx)),
                            ),
                    )
                    .child(
                        Self::action_pill("omni-clear", IconName::Eraser, t("tools.ui_clear"))
                            .on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx))),
                    )
                    .when(!self.spec.options.is_empty(), |d| d.child(opts_inline))
                    .when(!self.spec.live, |d| {
                        d.child(if can_run {
                            ui::button("tool-run", run_label, 1)
                                .on_click(cx.listener(|this, _, _w, cx| this.run(cx)))
                        } else {
                            ui::button_disabled("tool-run", run_label, 1)
                        })
                    }),
            )
            .when(has_presets, |d| d.child(presets));
        let show_out = self.has_result() || self.error.is_some() || self.busy;
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(omnibox)
            .when(show_out, |d| d.child(self.build_out_card(false, false, false, cx)))
            .into_any_element()
    }

    /// 对称加解密专属工作台（AES / DES / RC4 / Rabbit）：加解密模式与算法参数置顶，密钥/IV 双栏随机生成，下方明密文互转。
    fn render_cipher_workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let me = cx.entity().downgrade();
        let mut bar = div().flex().items_center().flex_wrap().gap(px(ui::SP_2));
        for f in self.spec.options.clone() {
            bar = bar.child(self.inline_option_el(&f, &me, cx));
        }
        let is_dec = self.picks.get("dir").map(|s| s.as_str()) == Some("dec");
        let cta_label = if is_dec {
            t("tools.o_decrypt")
        } else {
            t("tools.o_encrypt")
        };
        let mode_val = self.picks.get("mode").cloned().unwrap_or_default();
        let show_iv = self.texts.contains_key("iv") && mode_val != "ecb";
        let key_len = match self.spec.id.as_str() {
            "des" if self.picks.get("algo").map(|s| s.as_str()) == Some("des") => 8,
            "des" => 24,
            _ => 16,
        };
        let iv_len = if self.spec.id == "des" { 8 } else { 16 };
        let key_row = div()
            .flex()
            .items_end()
            .flex_wrap()
            .gap(px(ui::SP_3))
            .child(
                div()
                    .flex_1()
                    .min_w(px(200.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().child(Self::field_label(t("tools.f_key"))))
                            .child(
                                Self::action_pill("cip-rnd-k", IconName::Dices, t("ux.gen_key"))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.gen_random_field("key", key_len, w, cx);
                                    })),
                            ),
                    )
                    .child(
                        ui::text_field(&self.texts["key"], false).font_family(ui::mono_family()),
                    ),
            )
            .when(show_iv, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w(px(200.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .child(div().flex_1().child(Self::field_label(t("tools.f_iv"))))
                                .child(
                                    Self::action_pill(
                                        "cip-rnd-iv",
                                        IconName::Dices,
                                        t("ux.gen_key"),
                                    )
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.gen_random_field("iv", iv_len, w, cx);
                                    })),
                                ),
                        )
                        .child(
                            ui::text_field(&self.texts["iv"], false)
                                .font_family(ui::mono_family()),
                        ),
                )
            });
        let top_card = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(ui::SP_2))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(ui::SP_2))
                    .child(div().flex_1().min_w_0().child(bar))
                    .when(!self.output.text.is_empty(), |d| {
                        d.child(
                            ui::button("cip-swap", t("tools.ui_swap"), 0)
                                .on_click(cx.listener(|this, _, w, cx| this.swap(w, cx))),
                        )
                    })
                    .child(
                        ui::button("tool-run", cta_label, 1)
                            .on_click(cx.listener(|this, _, _w, cx| this.run(cx))),
                    ),
            )
            .child(key_row);
        let stat = self
            .tareas
            .get("input")
            .map(|a| tools::text_stats(&a.read(cx).value()))
            .filter(|s| s.1 > 0);
        let in_card = sub_card()
            .p(px(SUB_PAD))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Self::field_label(t("tools.f_input"))))
                    .when_some(stat, |d, (l, n)| {
                        d.child(
                            div()
                                .text_size(px(ui::fs_caption()))
                                .text_color(c(th.overlay1))
                                .child(tf("tools.ui_stat", &[("lines", &l), ("chars", &n)])),
                        )
                    })
                    .child(
                        Self::action_pill("cip-paste", IconName::ClipboardPaste, t("ux.paste"))
                            .on_click(
                                cx.listener(|this, _, w, cx| this.paste_field("input", w, cx)),
                            ),
                    )
                    .child(
                        Self::action_pill("cip-clear", IconName::Eraser, t("tools.ui_clear"))
                            .on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx))),
                    ),
            )
            .child(
                Textarea::new(&self.tareas["input"])
                    .w_full()
                    .font_family(ui::mono_family())
                    .text_size(px(ui::fs_mono())),
            );
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(CARD_PAD))
            .child(top_card)
            .child(in_card)
            .child(self.build_out_card(true, false, false, cx))
            .into_any_element()
    }
}

fn default_unit_for(kind: &str) -> &'static str {
    match kind {
        "length" => "m",
        "mass" => "kg",
        "temp" => "°C",
        "data" => "MB",
        "time" => "s",
        "area" => "m²",
        "volume" => "l",
        "speed" => "km/h",
        _ => "m",
    }
}

fn unit_choices_for(kind: &str) -> &'static [&'static str] {
    match kind {
        "length" => &["mm", "cm", "m", "km", "in", "ft", "yd", "mi", "里", "尺"],
        "mass" => &["mg", "g", "kg", "t", "oz", "lb", "斤", "两"],
        "temp" => &["°C", "°F", "K"],
        "data" => &["B", "KB", "MB", "GB", "TB", "KiB", "MiB", "GiB", "TiB"],
        "time" => &["ms", "s", "min", "h", "d", "wk", "yr"],
        "area" => &["cm²", "m²", "ha", "km²", "ft²", "acre", "亩"],
        "volume" => &["ml", "l", "m³", "gal", "fl oz"],
        "speed" => &["m/s", "km/h", "mph", "kn"],
        _ => &[],
    }
}

fn tool_sample(id: &str, mode: &str, dir: &str) -> Option<&'static str> {
    Some(match id {
        "json" | "json_ts" => {
            r#"{"service":"shellax","port":8080,"tls":true,"nodes":["hk-01","sg-02"],"limits":{"cpu":4,"mem_mb":8192}}"#
        }
        "go_json" if mode == "go2json" => {
            "type Server struct {\n\tID string `json:\"id\"`\n\tHost string `json:\"host\"`\n\tPort int `json:\"port\"`\n\tActive bool `json:\"active\"`\n}"
        }
        "go_json" => r#"{"id":"srv_01","host":"10.0.1.12","port":22,"active":true}"#,
        "sql" => {
            "select u.id, u.email, count(o.id) as orders_total from users u left join orders o on o.user_id = u.id where u.status = 'active' group by u.id, u.email order by orders_total desc limit 20;"
        }
        "css" => {
            ".card{display:flex;align-items:center;gap:12px;padding:16px;border-radius:10px;background:#181825}"
        }
        "html" => {
            "<section class=\"panel\"><header><h2>ShellAX</h2></header><main><p>Ready.</p></main></section>"
        }
        "md_html" => {
            "# ShellAX Release Notes\n\n- **SSH & SFTP**: Zero-copy streaming\n- **Database & Redis**: Live schema inspector\n\n```bash\ncargo run --release\n```"
        }
        "base64" if dir == "dec" => "U2hlbGxBWCDlva/lj5Hnu4jnq6/nvo7pnaLlt6XkvZzog70=",
        "base64" => "ShellAX 极速远程开发与运维工作台",
        "url" if dir == "dec" => "https%3A%2F%2Fshellax.dev%2Fsearch%3Fq%3Dssh%20tunnel%26lang%3Dzh-CN",
        "url" => "https://shellax.dev/search?q=ssh tunnel&lang=zh-CN",
        "unicode" if dir == "dec" => "\\u7ec8\\u7aef\\u4e0e\\u6570\\u636e\\u5e93",
        "unicode" => "终端与数据库",
        "dedupe" => "10.0.0.1\n10.0.0.2\n10.0.0.1\n10.0.0.3\n10.0.0.2",
        "trim" => "   host = 127.0.0.1   \n\n   port = 6379  ",
        "case" => "shellax_connection_manager_v2",
        "plural" => "repository\ncontainer\nindex\nleaf",
        "cjk_space" => "使用Rust与GPUI构建macOS原生客户端2026版",
        "sum_list" => "1280.50\n420.00\n899.90\n350.00",
        "naming" => "user session token\nssh jump host",
        "batch_time" => "1700000000\n1735689600\n2026-11-22 08:30:00",
        "md5" | "sha" => "shellax-release-v0.2.0",
        "random_group" => "Alice\nBob\nCharlie\nDiana\nEthan\nFiona",
        _ => return None,
    })
}

/// 选项控件需要 `&'static str` 作为元素 id 前缀：按字段 id 驻留一次（数量有限）。
/// 水印等选项用的预设色。id 与提示都是 `#rrggbb`，扩展包按这个字符串解析。
fn mark_colors() -> Vec<(String, gpui::Hsla, String)> {
    const HEX: &[&str] = &[
        "#ffffff", "#000000", "#ff0000", "#ff9500", "#ffcc00", "#34c759", "#00c7be", "#007aff",
        "#af52de", "#8e8e93",
    ];
    HEX.iter()
        .map(|hex| {
            let n = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
            let col = gpui::Rgba {
                r: ((n >> 16) & 255) as f32 / 255.,
                g: ((n >> 8) & 255) as f32 / 255.,
                b: (n & 255) as f32 / 255.,
                a: 1.,
            };
            ((*hex).to_string(), col.into(), (*hex).to_string())
        })
        .collect()
}

fn intern(s: &str) -> &'static str {
    use std::sync::{Mutex, OnceLock};
    static SET: OnceLock<Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    let mut set = SET.get_or_init(Default::default).lock().unwrap();
    if let Some(v) = set.get(s) {
        return v;
    }
    let v: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(v);
    v
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Render for ToolView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(s) = self.pending_out.take() {
            self.out_ed.update(cx, |e, cx| e.set_value(s, window, cx));
        }
        let th = cur();
        let head = div()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .pb(px(CARD_PAD))
            .flex_shrink_0()
            .child(ui::icon(
                ui::tool_icon(&self.spec),
                ui::IC_MD,
                c(th.accent),
            ))
            .child(
                div()
                    .text_size(px(ui::fs_title()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(th.text))
                    .child(self.spec.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.overlay1))
                    .child(self.spec.desc.clone()),
            );
        if self.spec.interactive {
            let body = self.render_ws(cx);
            return content_card("tool-card")
                .p(px(CARD_PAD))
                .child(head)
                .child(body)
                .into_any_element();
        }
        // 分隔比例：拖动中用本地值，松手后写回配置（所有工具共用）；平时跟随配置，其它工具标签改过也会同步
        if !cx.has_active_drag() {
            if self.dragging {
                self.dragging = false;
                let r = (self.split * 1000.).round() / 1000.;
                if (crate::config::get().sidebar.tools_split - r).abs() >= 0.002 {
                    let _ = crate::config::update(|s| s.sidebar.tools_split = r);
                }
            } else {
                self.split = lay::clamp_split(crate::config::get().sidebar.tools_split);
            }
        }
        let (body_w, wk) = (self.body_w.clone(), cx.entity().downgrade());
        let probe = gpui::canvas(
            move |b, _, cx| {
                let w = f32::from(b.size.width);
                let old = body_w.replace(w);
                if lay::side_by_side(old) != lay::side_by_side(w) {
                    let _ = wk.update(cx, |_, cx| cx.notify());
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        // 按功能形态分派专属布局，告别千篇一律的表单堆叠
        let bespoke: Option<AnyElement> = match self.spec.id.as_str() {
            "regex" => Some(self.render_regex_workbench(cx)),
            "timestamp" => Some(self.render_timestamp_workbench(cx)),
            "cron" => Some(self.render_cron_workbench(cx)),
            "color" => Some(self.render_color_workbench(cx)),
            "units" => Some(self.render_units_workbench(cx)),
            "aes" | "des" | "rc4" | "rabbit" => Some(self.render_cipher_workbench(cx)),
            "ip_lookup" | "ip_latency" | "cidr" | "ip_convert" | "radix" | "id_card"
            | "en_amount" | "rmb" => Some(self.render_omnibox_workbench(cx)),
            _ if self.spec.inputs.is_empty() && self.spec.pack.is_empty() => {
                Some(self.render_generator_workbench(cx))
            }
            _ => None,
        };
        if let Some(custom) = bespoke {
            return content_card("tool-card")
                .p(px(CARD_PAD))
                .child(head)
                .child(
                    div()
                        .id("tool-body")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .child(custom)
                        .child(probe),
                )
                .into_any_element();
        }

        let me = cx.entity().downgrade();
        let sizes: Vec<InSize> = self.spec.inputs.iter().map(|f| f.size()).collect();
        let in_fill = sizes.contains(&InSize::Fill);
        let has_inputs = !sizes.is_empty();
        let shown: Vec<_> = self
            .spec
            .options
            .clone()
            .into_iter()
            .filter(|f| self.option_shown(f))
            .collect();
        // 实时工具没有选项时不画冗余的中间「刷新」卡片；非实时工具或有选项的工具显示紧凑工具条
        let has_opts = !shown.is_empty() || !self.spec.live;
        let out_fill = in_fill || self.out_fill();
        let show_out = out_fill || self.has_result() || self.error.is_some();
        let both = in_fill && out_fill;
        let side = both && lay::side_by_side(self.body_w.get());
        self.side = side;
        self.has_opts = has_opts;
        let grow = |share: f32| (share * 1000.).max(1.);
        let mode_s = self.picks.get("mode").map(|s| s.as_str()).unwrap_or("");
        let dir_s = self.picks.get("dir").map(|s| s.as_str()).unwrap_or("");
        let has_sample =
            tool_sample(&self.spec.id, mode_s, dir_s).is_some() || self.spec.id == "hmac";
        let multi_line_horizontal = !in_fill
            && self.spec.inputs.len() == 2
            && self
                .spec
                .inputs
                .iter()
                .all(|f| matches!(f.size(), InSize::Line));

        // 输入卡片
        let mut input = sub_card().p(px(SUB_PAD)).flex().flex_col().gap_2();
        let mut line_pair = div().flex().items_end().flex_wrap().gap(px(ui::SP_3));
        for (idx, f) in self.spec.inputs.clone().into_iter().enumerate() {
            let fid = f.id.clone();
            let fid_paste = fid.clone();
            let fid_today = fid.clone();
            let is_date = (self.spec.id == "date_calc" && (f.id == "a" || f.id == "b"))
                || (self.spec.id == "workday" && f.id == "start");
            let is_text_like = matches!(
                f.kind,
                FieldKind::Text | FieldKind::Number | FieldKind::Multiline
            );
            let mut row = div().flex().flex_col().gap_1();
            if multi_line_horizontal {
                row = row.flex_1().min_w(px(200.));
            }
            let stat = match f.kind {
                FieldKind::Multiline => self
                    .areas
                    .get(&f.id)
                    .map(|a| tools::text_stats(&a.read(cx).value()))
                    .or_else(|| {
                        self.tareas
                            .get(&f.id)
                            .map(|a| tools::text_stats(&a.read(cx).value()))
                    })
                    .filter(|s| s.1 > 0),
                _ => None,
            };
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_1))
                    .child(div().flex_1().child(Self::field_label(&f.label)))
                    .children(stat.map(|(l, n)| {
                        div()
                            .text_size(px(ui::fs_caption()))
                            .text_color(c(th.overlay1))
                            .child(tf("tools.ui_stat", &[("lines", &l), ("chars", &n)]))
                    }))
                    .when(is_date, |d| {
                        d.child(
                            Self::action_pill(
                                SharedString::from(format!("in-today-{fid}")),
                                IconName::Calendar,
                                t("ux.today"),
                            )
                            .on_click(cx.listener(move |this, _, w, cx| {
                                let today = chrono::Local::now().format("%Y-%m-%d").to_string();
                                this.set_field(&fid_today, today, w, cx);
                                this.run(cx);
                            })),
                        )
                    })
                    .when(idx == 0 && has_sample, |d| {
                        d.child(
                            Self::action_pill("in-sample", IconName::Sparkles, t("ux.sample"))
                                .on_click(cx.listener(|this, _, w, cx| this.fill_sample(w, cx))),
                        )
                    })
                    .when(is_text_like, |d| {
                        d.child(
                            Self::action_pill(
                                SharedString::from(format!("in-paste-{fid}")),
                                IconName::ClipboardPaste,
                                t("ux.paste"),
                            )
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.paste_field(&fid_paste, w, cx)
                            })),
                        )
                    })
                    .when(idx == 0 && is_text_like, |d| {
                        d.child(
                            Self::action_pill(
                                "in-clear",
                                IconName::Eraser,
                                t("tools.ui_clear"),
                            )
                            .on_click(cx.listener(|this, _, w, cx| this.clear_input(w, cx))),
                        )
                    }),
            );
            match f.size() {
                InSize::Fill => {
                    row = row.flex_1().min_h(px(lay::IN_FILL_MIN_H)).child(
                        div()
                            .flex_1()
                            .min_h(px(lay::OUT_EDITOR_MIN_H))
                            .child(Editor::new(&self.areas[&f.id]).h_full()),
                    );
                }
                InSize::Auto(..) => {
                    row = row.child(
                        Textarea::new(&self.tareas[&f.id])
                            .w_full()
                            .font_family(ui::mono_family())
                            .text_size(px(ui::fs_mono())),
                    );
                }
                InSize::Line => {
                    row = row.child(ui::text_field(&self.texts[&f.id], false));
                }
                InSize::Zone => {
                    let id = f.id.clone();
                    let multiple = f.kind == FieldKind::Files;
                    let names: Vec<String> = self
                        .files
                        .get(&f.id)
                        .map(|v| v.iter().map(|p| p.display().to_string()).collect())
                        .unwrap_or_default();
                    let id2 = id.clone();
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("drop-{id}")))
                            .min_h(px(lay::FILE_ZONE_MIN_H))
                            .max_h(px(lay::FILE_ZONE_MAX_H))
                            .overflow_y_scroll()
                            .p_3()
                            .rounded(px(ui::R_MD))
                            .border_1()
                            .border_dashed()
                            .border_color(crate::theme::divider())
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .cursor_pointer()
                            .hover(|s| s.bg(crate::theme::hover_bg()))
                            .drag_over::<ExternalPaths>(|st, _, _, _| {
                                st.border_color(c(cur().accent))
                            })
                            .on_drop(cx.listener(move |v, ps: &ExternalPaths, _, cx| {
                                v.set_files(id2.clone(), ps.paths().to_vec(), cx)
                            }))
                            .on_click(cx.listener(move |v, _, _, cx| {
                                v.pick_files(id.clone(), multiple, cx)
                            }))
                            .child(icon(IconName::FileUp, ui::IC_LG, c(th.overlay1)))
                            .child(
                                div()
                                    .text_size(px(ui::fs_small()))
                                    .text_color(c(th.subtext))
                                    .child(t("tools.ui_drop_hint")),
                            )
                            .children(names.into_iter().map(|n| {
                                div()
                                    .text_size(px(ui::fs_small()))
                                    .text_color(c(th.text))
                                    .child(n)
                            })),
                    );
                }
            }
            if multi_line_horizontal {
                line_pair = line_pair.child(row);
            } else {
                input = input.child(row);
            }
        }
        if multi_line_horizontal {
            input = input.child(line_pair);
        }
        input = if in_fill {
            let base = input.min_h(px(lay::in_min_h(&sizes))).min_w(px(if side {
                lay::SIDE_PANE_MIN_W
            } else {
                0.
            }));
            if both {
                base.flex_basis(px(0.))
                    .flex_grow(grow(self.split))
                    .flex_shrink(1.)
            } else {
                base.flex_1()
            }
        } else {
            input.flex_shrink_0()
        };

        let stack = Self::opts_stacked(&self.spec);
        let mut opts = if stack {
            div().w_full().flex().flex_col().gap_3()
        } else {
            div().flex().flex_wrap().items_center().gap_3()
        };
        for f in &shown {
            opts = opts.child(if stack {
                self.option_el(f, &me, cx)
            } else {
                self.inline_option_el(f, &me, cx)
            });
        }
        let show_run_btn = !self.spec.live;
        let run_btn = ui::button("tool-run", t("tools.ui_run"), 1)
            .on_click(cx.listener(|this, _, _w, cx| this.run(cx)));
        let opts_h = self.opts_h.clone();
        let opts_card = if stack {
            sub_card()
                .w_full()
                .relative()
                .p(px(SUB_PAD))
                .flex()
                .flex_col()
                .gap_3()
                .flex_shrink_0()
                .child(opts)
                .when(show_run_btn, |d| {
                    d.child(div().w_full().flex().justify_end().child(run_btn))
                })
        } else {
            sub_card()
                .w_full()
                .relative()
                .px(px(SUB_PAD))
                .py(px(ui::SP_2))
                .flex()
                .items_center()
                .gap_3()
                .flex_shrink_0()
                .child(div().flex_1().min_w_0().child(opts))
                .when(show_run_btn, |d| d.child(run_btn))
        }
        .child(
            gpui::canvas(
                move |b, _, _| opts_h.set(f32::from(b.size.height)),
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        );

        let out = self.build_out_card(out_fill, both, side, cx);
        let body = div()
            .id("tool-body")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        // 大文本双栏工具统一把紧凑选项条置顶，无论左右并排还是窄屏上下分栏都更顺手
        let top_opts = in_fill && has_opts;
        let body = if side {
            body.when(has_opts, |d| d.child(opts_card).child(lay::gap(false)))
                .child(
                    div()
                        .id("tool-split-row")
                        .flex_1()
                        .min_h(px(lay::OUT_FILL_MIN_H))
                        .flex()
                        .flex_row()
                        .on_drag_move(cx.listener(
                            |this, ev: &gpui::DragMoveEvent<ToolSplit>, _w, cx| {
                                this.drag_split(ev, cx)
                            },
                        ))
                        .child(input)
                        .child(lay::handle("tool-split", true).on_click(cx.listener(
                            |this, ev: &gpui::ClickEvent, _w, cx| {
                                if ev.click_count() >= 2 {
                                    this.split = lay::SPLIT_DEFAULT;
                                    this.dragging = true;
                                    cx.notify();
                                }
                            },
                        )))
                        .child(out),
                )
        } else if top_opts {
            body.on_drag_move(cx.listener(
                |this, ev: &gpui::DragMoveEvent<ToolSplit>, _w, cx| this.drag_split(ev, cx),
            ))
            .child(opts_card)
            .child(lay::gap(false))
            .child(input)
            .child(
                lay::handle("tool-split", false).on_click(cx.listener(
                    |this, ev: &gpui::ClickEvent, _w, cx| {
                        if ev.click_count() >= 2 {
                            this.split = lay::SPLIT_DEFAULT;
                            this.dragging = true;
                            cx.notify();
                        }
                    },
                )),
            )
            .child(out)
        } else {
            body.on_drag_move(cx.listener(
                |this, ev: &gpui::DragMoveEvent<ToolSplit>, _w, cx| this.drag_split(ev, cx),
            ))
            .when(has_inputs, |d| d.child(input))
            .when(has_inputs && (has_opts || show_out), |d| {
                d.child(if both {
                    lay::handle("tool-split", false)
                        .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| {
                            if ev.click_count() >= 2 {
                                this.split = lay::SPLIT_DEFAULT;
                                this.dragging = true;
                                cx.notify();
                            }
                        }))
                        .into_any_element()
                } else {
                    lay::gap(false).into_any_element()
                })
            })
            .when(has_opts, |d| d.child(opts_card))
            .when(has_opts && show_out, |d| d.child(lay::gap(false)))
            .when(show_out, |d| d.child(out))
        };
        content_card("tool-card")
            .p(px(CARD_PAD))
            .child(head)
            .child(body.child(probe))
            .into_any_element()
    }
}
