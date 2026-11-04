//! Homebrew 左侧清单，以及唯一的一条命令队列。`brew` 自己有锁，同时只能跑一个。
use std::collections::VecDeque;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, Entity, SharedString, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::brew::{self, Kind, Op, Package, Service};
use ssh_term_proto::i18n::{t, tf};

use crate::activity::{self, log_push};
use crate::termview::Embed;
use crate::theme::{c, cur};
use crate::ui::{self, IconName};
use crate::Root;

const LOG_MAX: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Outdated,
    All,
    Detail,
}

#[derive(Default)]
pub struct BrewPanel {
    search: Option<Entity<InputState>>,
}

enum Evt {
    Line(String),
    Done(Outcome),
}

struct Outcome {
    code: i32,
    installed: Option<String>,
    services: Option<String>,
    search: Option<String>,
}

pub struct BrewHub {
    pub(crate) brew: Option<PathBuf>,
    pub(crate) packages: Vec<Package>,
    pub(crate) services: Vec<Service>,
    pub(crate) loaded: bool,
    pub(crate) error: Option<String>,
    pub(crate) log: Vec<String>,
    pub(crate) current: Option<Op>,
    pub(crate) queue: VecDeque<Op>,
    pub(crate) query: String,
    pub(crate) hits: Vec<brew::Hit>,
    pub(crate) search_error: Option<String>,
    running: bool,
    started: bool,
    exec: activity::ExecLog,
}

pub(crate) struct ListSnap {
    pub missing: bool,
    pub loaded: bool,
    pub error: Option<String>,
    pub packages: Vec<Package>,
    pub services: Vec<Service>,
    pub status: Option<String>,
}

impl BrewHub {
    pub(crate) fn new(exec: activity::ExecLog) -> Self {
        Self {
            brew: None,
            packages: vec![],
            services: vec![],
            loaded: false,
            error: None,
            log: vec![],
            current: None,
            queue: VecDeque::new(),
            query: String::new(),
            hits: vec![],
            search_error: None,
            running: false,
            started: false,
            exec,
        }
    }

    pub(crate) fn start(&mut self, cx: &mut Context<Self>) {
        if self.started {
            return;
        }
        self.started = true;
        if self.ensure_brew() {
            self.enqueue(Op::Refresh, cx);
        } else {
            self.loaded = true;
        }
    }

    pub(crate) fn list_snap(&self) -> ListSnap {
        ListSnap {
            missing: self.brew.is_none(),
            loaded: self.loaded,
            error: self.error.clone(),
            packages: self.packages.clone(),
            services: self.services.clone(),
            status: running_text(self.current.as_ref(), self.queue.len()),
        }
    }

    pub(crate) fn set_query(&mut self, q: String, cx: &mut Context<Self>) {
        if self.query == q {
            return;
        }
        self.query = q;
        cx.notify();
    }

    pub(crate) fn enqueue(&mut self, op: Op, cx: &mut Context<Self>) {
        if !self.ensure_brew() {
            self.loaded = true;
            self.brew = None;
            cx.notify();
            return;
        }
        if matches!(op, Op::Refresh) && self.refresh_pending() {
            cx.notify();
            return;
        }
        self.queue.push_back(op);
        self.pump(cx);
        cx.notify();
    }

    pub(crate) fn clear_log(&mut self, cx: &mut Context<Self>) {
        self.log.clear();
        cx.notify();
    }

    fn refresh_pending(&self) -> bool {
        matches!(self.current, Some(Op::Refresh))
            || self.queue.iter().any(|q| matches!(q, Op::Refresh))
    }

    fn ensure_brew(&mut self) -> bool {
        if self.brew.as_ref().is_some_and(|p| p.is_file()) {
            return true;
        }
        let prefix = std::env::var("HOMEBREW_PREFIX").ok();
        self.brew = brew::find_brew(&brew::candidates(prefix.as_deref()));
        self.brew.is_some()
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(op) = self.queue.pop_front() else {
            self.current = None;
            return;
        };
        let Some(bin) = self.brew.clone() else {
            self.queue.push_front(op);
            return;
        };
        self.running = true;
        self.current = Some(op.clone());
        let (tx, rx) = flume::unbounded();
        std::thread::spawn(move || {
            let outcome = run_op(&bin, &op, &tx);
            let _ = tx.send(Evt::Done(outcome));
        });
        cx.spawn(async move |this, cx| loop {
            let first = match rx.recv_async().await {
                Ok(ev) => ev,
                Err(_) => break,
            };
            let mut batch = vec![first];
            while batch.len() < 40 {
                match rx.try_recv() {
                    Ok(ev) => batch.push(ev),
                    Err(_) => break,
                }
            }
            let done = batch.iter().any(|e| matches!(e, Evt::Done(_)));
            if this
                .update(cx, |h, cx| {
                    for ev in batch {
                        h.on_evt(ev, cx);
                    }
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
            if done {
                break;
            }
        })
        .detach();
    }

    fn on_evt(&mut self, ev: Evt, cx: &mut Context<Self>) {
        match ev {
            Evt::Line(s) => self.push_log(s),
            Evt::Done(o) => {
                let op = self.current.take();
                self.running = false;
                self.loaded = true;
                if let Some(op) = op {
                    self.finish(op, o);
                }
                self.pump(cx);
            }
        }
    }

    fn finish(&mut self, op: Op, o: Outcome) {
        let ok = o.code == 0;
        if matches!(op, Op::Refresh) {
            if let Some(raw) = o.installed.as_deref() {
                match brew::parse_installed(raw) {
                    Ok(p) => {
                        let f = p.iter().filter(|x| x.kind == Kind::Formula).count();
                        let c = p.iter().filter(|x| x.kind == Kind::Cask).count();
                        self.packages = p;
                        self.error = if ok {
                            None
                        } else {
                            Some(tf("brew.failed", &[("code", &o.code)]))
                        };
                        self.push_log(tf("brew.refreshed", &[("f", &f), ("c", &c)]));
                    }
                    Err(e) => {
                        self.error = Some(tf("brew.parse_error", &[("e", &brief(&e))]));
                        self.push_log(brief(&e));
                    }
                }
            } else if !ok {
                self.error = Some(tf("brew.failed", &[("code", &o.code)]));
            }
            if let Some(raw) = o.services.as_deref() {
                match brew::parse_services(raw) {
                    Ok(s) => self.services = s,
                    Err(e) => self.push_log(brief(&e)),
                }
            }
        } else if matches!(op, Op::Search { .. }) {
            self.hits = brew::parse_search(o.search.as_deref().unwrap_or(""));
            self.search_error = if ok || !self.hits.is_empty() {
                None
            } else {
                Some(tf("brew.failed", &[("code", &o.code)]))
            };
        } else if ok {
            self.error = None;
        } else {
            self.error = Some(tf("brew.failed", &[("code", &o.code)]));
        }
        if !ok {
            self.push_log(tf("brew.failed", &[("code", &o.code)]));
        }
        log_push(
            &self.exec,
            "brew",
            &op.shell(),
            ok,
            if ok {
                "ok".into()
            } else {
                format!("exit {}", o.code)
            },
        );
        if ok && op.follow_with_refresh() {
            self.queue_refresh();
        }
    }

    fn queue_refresh(&mut self) {
        if self.refresh_pending() {
            return;
        }
        self.queue.push_back(Op::Refresh);
    }

    fn push_log(&mut self, line: String) {
        let line = line.trim().to_string();
        if line.is_empty() {
            return;
        }
        self.log.push(line);
        if self.log.len() > LOG_MAX {
            let n = self.log.len() - LOG_MAX;
            self.log.drain(0..n);
        }
    }
}

pub(crate) fn running_text(current: Option<&Op>, queued: usize) -> Option<String> {
    let cur = current?;
    let cmd = cur.shell();
    if queued == 0 {
        Some(cmd)
    } else {
        Some(format!("{cmd}  (+{queued})"))
    }
}

fn run_op(brew_bin: &PathBuf, op: &Op, tx: &flume::Sender<Evt>) -> Outcome {
    let _ = tx.send(Evt::Line(format!("$ {}", op.shell())));
    let mut code = 0;
    let mut installed = None;
    let mut services = None;
    let mut search = None;
    for (i, cmd) in op.commands().into_iter().enumerate() {
        match run_cmd(brew_bin, &cmd, tx) {
            Ok((c, stdout)) => {
                if matches!(op, Op::Refresh) && i == 0 {
                    installed = Some(stdout);
                    if c != 0 {
                        code = c;
                        break;
                    }
                } else if matches!(op, Op::Refresh) && i == 1 {
                    services = Some(stdout);
                    if c != 0 {
                        let _ = tx.send(Evt::Line(format!("brew services exited {c}")));
                    }
                } else if matches!(op, Op::Search { .. }) {
                    search = Some(stdout);
                    if c != 0 {
                        code = c;
                        break;
                    }
                } else if c != 0 {
                    code = c;
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(Evt::Line(e.to_string()));
                code = 1;
                break;
            }
        }
    }
    Outcome {
        code,
        installed,
        services,
        search,
    }
}

fn run_cmd(
    brew_bin: &PathBuf,
    cmd: &brew::Cmd,
    tx: &flume::Sender<Evt>,
) -> std::io::Result<(i32, String)> {
    let mut c = Command::new(brew_bin);
    c.args(&cmd.args);
    for (k, v) in brew::ENV {
        c.env(k, v);
    }
    c.env(
        "PATH",
        brew::child_path(brew_bin, &std::env::var("PATH").unwrap_or_default()),
    );
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn()?;
    let mut out = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("stdout"))?;
    let mut err = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("stderr"))?;
    let tx_err = tx.clone();
    let err_thread = std::thread::spawn(move || stream_pipe(&mut err, &tx_err));
    let capture = cmd.capture;
    let tx_out = tx.clone();
    let out_thread = std::thread::spawn(move || {
        if capture {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        } else {
            stream_pipe(&mut out, &tx_out);
            String::new()
        }
    });
    let status = child.wait()?;
    let stdout = out_thread.join().unwrap_or_default();
    let _ = err_thread.join();
    Ok((status.code().unwrap_or(1), stdout))
}

fn stream_pipe(r: &mut impl Read, tx: &flume::Sender<Evt>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match r.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                while let Some(i) = buf.iter().position(|b| *b == b'\n' || *b == b'\r') {
                    let line: Vec<u8> = buf.drain(..=i).collect();
                    let s = String::from_utf8_lossy(&line[..line.len().saturating_sub(1)])
                        .trim()
                        .to_string();
                    if !s.is_empty() {
                        let _ = tx.send(Evt::Line(s));
                    }
                }
            }
            Err(_) => break,
        }
    }
    if !buf.is_empty() {
        let s = String::from_utf8_lossy(&buf).trim().to_string();
        if !s.is_empty() {
            let _ = tx.send(Evt::Line(s));
        }
    }
}

fn brief(s: &str) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= 200 {
        s
    } else {
        format!("{}…", s.chars().take(200).collect::<String>())
    }
}

fn hit(text: &str, q: &str) -> bool {
    q.is_empty() || text.to_lowercase().contains(q)
}

impl Root {
    pub(crate) fn brew_hub(&mut self, cx: &mut Context<Self>) -> Entity<BrewHub> {
        if let Some(h) = &self.brew {
            return h.clone();
        }
        let exec = self.exec_log.clone();
        let h = cx.new(|_| BrewHub::new(exec));
        self._subs.push(cx.observe(&h, |_, _, cx| cx.notify()));
        h.update(cx, |h, cx| h.start(cx));
        self.brew = Some(h.clone());
        h
    }

    fn show_brew_list(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.open_brew_content(false, page, None, window, cx);
    }

    /// 打开可更新列表。模块里还没有标签时用它。
    pub(crate) fn open_brew(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_brew_content(false, Page::Outdated, None, window, cx);
    }

    /// 「+」打开安装页。已有安装标签时切回去。
    pub(crate) fn open_brew_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_brew_content(true, Page::Outdated, None, window, cx);
    }

    /// 内容不同就新开标签；相同内容切回已有标签。
    fn open_brew_content(
        &mut self,
        install: bool,
        page: Page,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.brew_hub(cx);
        if let Some(i) = self.tabs.iter().position(|t| {
            matches!(&t.layout.leaves()[0].read(cx).embed, Some(Embed::Brew(v)) if v.read(cx).same(install, page, id.as_deref()))
        }) {
            self.activate_brew(i, window, cx);
            return;
        }
        let hub = self.brew.clone().unwrap();
        let v = if install {
            cx.new(|cx| crate::brew_view::BrewView::install(hub, window, cx))
        } else {
            cx.new(|cx| crate::brew_view::BrewView::open(hub, page, id, cx))
        };
        self._subs.push(cx.subscribe_in(&v, window, |this, _, ev: &crate::brew_view::BrewEvent, w, cx| {
            let crate::brew_view::BrewEvent::Open(page, id) = ev;
            this.open_brew_content(false, *page, id.clone(), w, cx);
        }));
        let pane = self.embed_pane(Embed::Brew(v), window, cx);
        self.tool = activity::Tool::Brew;
        self.push_pane(pane, window, cx);
    }

    fn activate_brew(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = i;
        self.tool = activity::Tool::Brew;
        self.leave_settings();
        self.overlay = crate::Overlay::None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// 当前 Homebrew 标签在看什么，用来高亮左侧入口。
    fn brew_focus(&self, cx: &gpui::App) -> Option<(bool, Page, Option<String>)> {
        let tab = self.tabs.get(self.active)?;
        if Self::tab_module(tab, cx) != activity::Tool::Brew {
            return None;
        }
        let leaf = tab.layout.leaves().into_iter().next()?;
        let embed = leaf.read(cx).embed.clone();
        let Embed::Brew(v) = embed? else {
            return None;
        };
        let v = v.read(cx);
        Some((v.install, v.page(), v.selected_id()))
    }

    pub(crate) fn brew_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !cfg!(target_os = "macos") {
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(ui::panel_header(
                    t("tool.brew"),
                    None,
                    Some(IconName::Beer),
                    vec![],
                ))
                .child(ui::empty_state(
                    IconName::Beer,
                    t("brew.macos_only"),
                    None,
                    vec![],
                    false,
                ))
                .into_any_element();
        }
        let search = match &self.brewp.search {
            Some(s) => s.clone(),
            None => {
                let s = cx.new(|cx| InputState::new(window, cx).placeholder(t("brew.search")));
                self._subs.push(cx.subscribe_in(
                    &s,
                    window,
                    |this, input, ev: &InputEvent, _, cx| {
                        if matches!(ev, InputEvent::Change) {
                            let q = input.read(cx).value().to_string();
                            if let Some(h) = this.brew.clone() {
                                h.update(cx, |h, cx| h.set_query(q, cx));
                            }
                            cx.notify();
                        }
                    },
                ));
                self.brewp.search = Some(s.clone());
                s
            }
        };
        let q = search.read(cx).value().trim().to_lowercase();
        let hub = self.brew_hub(cx);
        let snap = hub.read(cx).list_snap();
        let refresh = ui::icon_button(
            "brew-refresh",
            IconName::RefreshCw,
            ui::IconBtn::Md,
            t("brew.refresh"),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.brew_hub(cx)
                .update(cx, |h, cx| h.enqueue(Op::Refresh, cx));
        }))
        .into_any_element();
        let mut body = div().flex_1().min_h_0().flex().flex_col();
        if snap.missing {
            let copy = ui::button("brew-copy-install", t("brew.copy_install"), 1)
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(brew::INSTALL_CMD.into()));
                    this.toast(t("brew.copied").into(), cx);
                }))
                .into_any_element();
            body = body
                .child(ui::empty_state(
                    IconName::Beer,
                    t("brew.missing"),
                    Some(t("brew.missing_desc").into()),
                    vec![copy],
                    false,
                ))
                .child(
                    div()
                        .px(px(ui::PANEL_PX))
                        .pb(px(ui::SP_3))
                        .text_size(px(ui::fs_small()))
                        .font_family(ui::mono_family())
                        .text_color(c(cur().subtext))
                        .whitespace_normal()
                        .child(brew::INSTALL_CMD.to_string()),
                );
        } else if !snap.loaded && snap.packages.is_empty() {
            body = body.child(ui::empty_state(
                IconName::Beer,
                t("brew.loading"),
                snap.status.map(|s| s.into()),
                vec![],
                false,
            ));
        } else {
            body = body.child(
                ui::field_row()
                    .pb(px(ui::SP_2))
                    .child(ui::search_field(&search, false)),
            );
            if let Some(e) = snap.error.clone() {
                body = body.child(ui::notice(ui::Notice::Error, e).mb(px(ui::SP_1)));
            }
            if let Some(s) = snap.status.clone() {
                body = body.child(ui::notice(ui::Notice::Info, s).mb(px(ui::SP_1)));
            }
            let searching = !q.is_empty();
            let focus = self.brew_focus(cx);
            let outdated_n = snap.packages.iter().filter(|p| p.outdated).count();
            let services: Vec<&Service> = snap
                .services
                .iter()
                .filter(|s| !searching || hit(&s.name, &q))
                .collect();
            let mut list = div()
                .id("brew-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .pb(px(ui::SP_2));
            if !services.is_empty() {
                list = list.child(ui::section_header(
                    "brew-svc-cap",
                    t("brew.services").to_string(),
                    None,
                    None,
                ));
                for s in &services {
                    let name = s.name.clone();
                    let pkg = snap
                        .packages
                        .iter()
                        .find(|p| p.name == s.name)
                        .map(|p| p.id.clone());
                    let id = pkg.clone().unwrap_or_else(|| brew::service_id(&s.name));
                    let on = focus.as_ref().is_some_and(|(_, page, sel)| {
                        *page == Page::Detail && sel.as_deref() == Some(id.as_str())
                    });
                    let col = svc_color(&s.status);
                    list = list.child(
                        ui::list_row(SharedString::from(format!("brew-svc-{}", s.name)), 0, on)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                let id = pkg.clone().unwrap_or_else(|| brew::service_id(&name));
                                this.open_brew_content(false, Page::Detail, Some(id), w, cx);
                            }))
                            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(col))
                            .child(ui::row_title(s.name.clone()).flex_1())
                            .child(ui::row_meta(
                                t(brew::service_label_key(&s.status)).to_string(),
                            )),
                    );
                }
            }
            let out_on = focus.as_ref().is_some_and(|(install, page, _)| !install && *page == Page::Outdated);
            let all_on = focus.as_ref().is_some_and(|(install, page, _)| !install && *page == Page::All);
            list = list.child(
                ui::list_row("brew-nav-out", 0, out_on)
                    .mt(px(ui::SP_2))
                    .on_click(
                        cx.listener(|this, _, w, cx| this.show_brew_list(Page::Outdated, w, cx)),
                    )
                    .child(ui::row_title(tf("brew.nav_outdated", &[("n", &outdated_n)])).flex_1()),
            );
            list = list.child(
                ui::list_row("brew-nav-all", 0, all_on)
                    .on_click(cx.listener(|this, _, w, cx| this.show_brew_list(Page::All, w, cx)))
                    .child(ui::row_title(t("brew.nav_all").to_string()).flex_1())
                    .child(ui::row_meta(snap.packages.len().to_string())),
            );
            body = body.child(list);
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(ui::panel_header(
                t("tool.brew"),
                None,
                Some(IconName::Beer),
                vec![refresh],
            ))
            .child(body)
            .into_any_element()
    }
}

fn svc_color(status: &str) -> gpui::Hsla {
    let th = cur();
    match status {
        "started" => c(th.success),
        "error" | "exited" => c(th.error),
        _ => c(th.overlay0),
    }
}
