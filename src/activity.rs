//! IDE-style tool-window shell (v0.2 PRD §3): the 40px activity strip on the far left, the
//! per-module tool window it opens, and the bottom tool window (transfer queue, execution
//! log). Clicking the active module's icon collapses its window; Alt+1…6 switch modules.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use gpui::{div, prelude::*, px, AnyElement, Context, Window};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp::JobState;

use crate::actions::Action;
use crate::card_layout;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{config, Root};

pub const STRIP_W: f32 = 40.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool { Hosts, Sftp, Database, Redis, Docker, Ports, Tools }

impl Tool {
    pub const TOP: [Tool; 7] = [Tool::Hosts, Tool::Sftp, Tool::Database, Tool::Redis, Tool::Docker, Tool::Ports, Tool::Tools];
    pub fn id(self) -> &'static str {
        match self { Tool::Hosts => "hosts", Tool::Sftp => "sftp", Tool::Database => "database", Tool::Redis => "redis", Tool::Docker => "docker", Tool::Ports => "ports", Tool::Tools => "tools" }
    }
    pub fn from_id(s: &str) -> Tool { Tool::TOP.into_iter().find(|t| t.id() == s).unwrap_or(Tool::Hosts) }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Hosts => t("tool.hosts"), Tool::Sftp => t("tool.sftp"), Tool::Database => t("tool.database"), Tool::Redis => t("tool.redis"),
            Tool::Docker => t("tool.docker"), Tool::Ports => t("tool.ports"), Tool::Tools => t("tool.tools"),
        }
    }
    pub fn icon(self) -> IconName {
        match self {
            Tool::Hosts => IconName::Server, Tool::Sftp => IconName::FolderSync, Tool::Database => IconName::Database, Tool::Redis => IconName::Layers,
            Tool::Docker => IconName::Container, Tool::Ports => IconName::ArrowLeftRight, Tool::Tools => IconName::PencilRuler,
        }
    }
    pub fn action(self) -> Action {
        match self {
            Tool::Hosts => Action::ToolHosts, Tool::Sftp => Action::ToolSftp, Tool::Database => Action::ToolDatabase, Tool::Redis => Action::ToolRedis,
            Tool::Docker => Action::ToolDocker, Tool::Ports => Action::ToolPorts, Tool::Tools => Action::ToolTools,
        }
    }
    pub fn from_action(a: Action) -> Option<Tool> { Tool::TOP.into_iter().find(|t| t.action() == a) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bottom { Transfers, Log }
impl Bottom {
    pub fn id(self) -> &'static str { match self { Bottom::Transfers => "transfers", Bottom::Log => "log" } }
    pub fn from_id(s: &str) -> Bottom { if s == "log" { Bottom::Log } else { Bottom::Transfers } }
}

/// One line of the execution log (database statements, later Redis / Docker commands).
#[derive(Clone, Debug)]
pub struct LogEntry { pub at: String, pub source: String, pub text: String, pub ok: bool, pub detail: String }
pub type ExecLog = Arc<Mutex<VecDeque<LogEntry>>>;
const LOG_MAX: usize = 1000;
pub fn log_push(log: &ExecLog, source: &str, text: &str, ok: bool, detail: String) {
    let mut l = log.lock().unwrap();
    if l.len() >= LOG_MAX { l.pop_front(); }
    l.push_back(LogEntry { at: chrono::Local::now().format("%H:%M:%S").to_string(), source: source.into(), text: text.into(), ok, detail });
}

#[derive(Clone)]
pub struct BottomResize;
impl Render for BottomResize { fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement { div() } }

impl Root {
    /// Activity-strip click / Alt+N: switch module; on the active one, collapse or reopen its window.
    pub(crate) fn select_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if tool == self.tool { self.toggle_sidebar(cx); } else {
            self.tool = tool;
            if !self.sidebar_visible { self.toggle_sidebar(cx); }
            let _ = config::update(|s| s.sidebar.tool = tool.id().into());
            self.show_module(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn toggle_bottom(&mut self, tab: Option<Bottom>, cx: &mut Context<Self>) {
        let was = self.bottom_open && !self.bottom_anim.is_some_and(|a| !a.0);
        let open = match tab {
            Some(t) if was && self.bottom_tab != t => { self.bottom_tab = t; true }
            Some(t) => { self.bottom_tab = t; !was }
            None => !was,
        };
        if open != was && !crate::motion::reduced() {
            // slide; when closing the panel stays mounted until the slide ends
            let seq = self.bottom_anim.map(|a| a.1 + 1).unwrap_or(1);
            self.bottom_anim = Some((open, seq));
            let dur = if open { crate::motion::PANEL_OPEN_MS } else { crate::motion::PANEL_CLOSE_MS };
            crate::motion::hold(dur + 20);
            if open { self.bottom_open = true; }
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(crate::motion::ms(dur)).await;
                let _ = this.update(cx, |r, cx| if r.bottom_anim.is_some_and(|a| a.1 == seq) { r.bottom_anim = None; if !open { r.bottom_open = false; } cx.notify(); });
            }).detach();
        } else {
            self.bottom_open = open;
        }
        let tab = self.bottom_tab.id();
        let _ = config::update(|s| { s.sidebar.bottom_open = open; s.sidebar.bottom_tab = tab.into(); });
        cx.notify();
    }

    pub(crate) fn strip_btn(&self, id: impl Into<gpui::ElementId>, ic: IconName, tip: String, active: bool, badge: bool) -> gpui::Stateful<gpui::Div> {
        self.strip_btn_with(id, None, ic, tip, active, badge)
    }
    /// Strip button whose glyph is one of our own SVGs (`svg`) instead of a stock icon.
    pub(crate) fn strip_btn_with(&self, id: impl Into<gpui::ElementId>, svg: Option<&'static str>, ic: IconName, tip: String, active: bool, badge: bool) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        ui::icon_button_svg(id, svg.map_or_else(|| ic.path(), gpui::SharedString::from), ui::IconBtn::Strip, tip, active, false).relative()
            .when(badge, |d| d.child(div().absolute().top(px(4.)).right(px(4.)).size(px(6.)).rounded_full().bg(c(th.accent))))
    }

    /// The left activity strip: modules only (session tools live in the right strip, settings in the title bar).
    pub(crate) fn activity_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // 展开 / 收起 / 动画中结构一致（顶部标题行由主布局提供），图标位置不变；高亮在收起动画开始时就撤掉
        let open = self.sidebar_visible && self.sb_reveal.as_ref().is_none_or(|r| r.expanding());
        let mut col = div().id("activity").w(px(STRIP_W)).flex_shrink_0().h_full().flex().flex_col().items_center().gap(px(4.)).pb(px(8.)).pt(px(4.));
        for (i, tool) in Tool::TOP.into_iter().enumerate() {
            let tip = crate::sidebar::action_tip(tool.label(), &tool.action().binding());
            col = col.child(self.strip_btn_with(("act", i), match tool { Tool::Redis => Some("shellax/db/redis.svg"), Tool::Docker => Some("shellax/db/docker.svg"), Tool::Tools => Some("shellax/db/toolbox.svg"), _ => None }, tool.icon(), tip, open && self.tool == tool, false)
                .on_click(cx.listener(move |this, _, _w, cx| this.select_tool(tool, _w, cx))));
        }
        // 外层框架色由根节点统一提供
        col
    }

    /// Body of the left tool window for the active module.
    pub(crate) fn tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tool {
            Tool::Hosts | Tool::Sftp => self.sidebar(window, cx).into_any_element(),
            Tool::Database => {
                let p = self.db_panel(window, cx);
                { let h = self.sb_resize_handle(cx); self.sb_frame(cx).child(p).child(h).into_any_element() }
            }
            Tool::Redis | Tool::Docker => {
                let k = if self.tool == Tool::Redis { crate::kv_ui::AssetKind::Redis } else { crate::kv_ui::AssetKind::Docker };
                let p = self.asset_panel(k, cx);
                { let h = self.sb_resize_handle(cx); self.sb_frame(cx).child(p).child(h).into_any_element() }
            }
            Tool::Tools => {
                let p = self.tools_panel(window, cx);
                { let h = self.sb_resize_handle(cx); self.sb_frame(cx).child(p).child(h).into_any_element() }
            }
            other => {
                let body = self.empty_tool(other, cx);
                { let h = self.sb_resize_handle(cx); self.sb_frame(cx).child(body).child(h).into_any_element() }
            }
        }
    }

    /// 尚无完整窗口的模块（端口 / Docker）：标题行 + 空状态，说明用途并给出现有入口。
    fn empty_tool(&mut self, tool: Tool, cx: &mut Context<Self>) -> AnyElement {
        let (ic, desc, open_label, open_action, open_id) = match tool {
            Tool::Docker => (IconName::Container, t("tool.docker_desc"), t("tool.docker_open"), Action::OpenHostTools, "tool-docker-open"),
            _ => (IconName::ArrowLeftRight, t("tool.ports_desc"), t("tool.ports_open"), Action::OpenPortForward, "tool-ports-open"),
        };
        let open = ui::button(open_id, open_label, 1).w_full().justify_center().on_click(cx.listener(move |this, _, w, cx| this.run(open_action, w, cx))).into_any_element();
        div().flex_1().min_h_0().flex().flex_col()
            .child(ui::panel_header(tool.label(), None, None, vec![]))
            .child(ui::empty_state(ic, desc, None, vec![open], false))
            .into_any_element()
    }

    pub(crate) fn transfers_active(&self, cx: &gpui::App) -> bool {
        self.transfer_jobs(cx).iter().any(|(_, j)| matches!(j.state(), JobState::Queued | JobState::Running))
    }

    /// Jobs of every open file browser, labelled with its title.
    fn transfer_jobs(&self, cx: &gpui::App) -> Vec<(String, Arc<ssh_term_proto::sftp::Job>)> {
        let mut out = vec![];
        for tab in &self.tabs {
            for p in tab.layout.leaves() {
                let v = p.read(cx);
                if let Some(s) = &v.sftp {
                    let sv = s.read(cx);
                    let label = sv.title(); for (_, q) in sv.queues() { for j in q.jobs.lock().unwrap().iter() { out.push((label.clone(), j.clone())); } }
                }
            }
        }
        out
    }

    /// Bottom tool window under the editor area.
    pub(crate) fn bottom_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let h = self.bottom_h;
        let tab = |id: &'static str, label: String, on: bool, which: Bottom| ui::chip(id, label, on)
            .on_click(cx.listener(move |this, _, _w, cx| { this.bottom_tab = which; let _ = config::update(|s| s.sidebar.bottom_tab = which.id().into()); cx.notify(); })).into_any_element();
        let jobs = self.transfer_jobs(cx);
        let log: Vec<LogEntry> = self.exec_log.lock().unwrap().iter().rev().take(300).cloned().collect();
        let mut actions: Vec<AnyElement> = vec![];
        if self.bottom_tab == Bottom::Log && !log.is_empty() {
            actions.push(ui::icon_button_state("bt-clear", IconName::Trash, ui::IconBtn::Md, t("tool.clear"), false, true)
                .on_click(cx.listener(|this, _, _w, cx| { this.exec_log.lock().unwrap().clear(); cx.notify(); })).into_any_element());
        }
        actions.push(ui::icon_button("bt-close", IconName::PanelBottomClose, ui::IconBtn::Md, t("ui.close")).on_click(cx.listener(|this, _, _w, cx| this.toggle_bottom(None, cx))).into_any_element());
        let head = ui::panel_header_with(vec![
            tab("bt-transfers", tf("tool.transfers_n", &[("n", &jobs.len())]), self.bottom_tab == Bottom::Transfers, Bottom::Transfers),
            tab("bt-log", t("tool.exec_log").to_string(), self.bottom_tab == Bottom::Log, Bottom::Log),
        ], actions);
        let mut body = div().id("bottom-body").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().px(px(ui::SP_2)).pb(px(ui::SP_2)).text_size(px(ui::fs_small()));
        let empty = |ic: IconName, s: &str| ui::empty_state(ic, s.to_string(), None, vec![], true);
        match self.bottom_tab {
            Bottom::Transfers => {
                if jobs.is_empty() { body = body.child(empty(IconName::ArrowDownUp, t("tool.transfers_empty"))); }
                for (i, (src, j)) in jobs.iter().rev().enumerate() {
                    let st = j.state();
                    let (label, col) = match st {
                        JobState::Queued => (t("tool.job_queued"), th.overlay1), JobState::Running => (t("tool.job_running"), th.accent),
                        JobState::Done => (t("tool.job_done"), th.success), JobState::Failed => (t("tool.job_failed"), th.error), JobState::Paused => (t("tool.job_paused"), th.warning),
                    };
                    let p = j.progress().clamp(0., 1.);
                    let arrow = if j.dir == ssh_term_proto::sftp::Dir::Upload { IconName::Upload } else { IconName::Download };
                    body = body.child(div().id(("job", i)).min_h(px(ui::row_h())).flex().items_center().gap_2().px_1().rounded(px(ui::R_MD)).hover(|s| s.bg(crate::theme::hover_bg()))
                        .child(icon(arrow, ui::IC_XS, c(th.overlay1)))
                        .child(div().w(px(180.)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(j.name()))
                        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.subtext)).child(format!("{src} · {}", j.remote)))
                        .child(div().w(px(120.)).h(px(4.)).flex_shrink_0().rounded_full().bg(c(th.surface1)).child(div().h_full().rounded_full().bg(c(col)).w(px(120. * p))))
                        .child(div().w(px(56.)).flex_shrink_0().text_color(c(col)).child(label.to_string()))
                        .when_some(j.error.lock().unwrap().clone(), |d, e| d.tooltip(crate::ui::tip(e.clone()))));
                }
            }
            Bottom::Log => {
                if log.is_empty() { body = body.child(empty(IconName::ScrollText, t("tool.log_empty"))); }
                for (i, e) in log.iter().enumerate() {
                    let detail = e.detail.clone();
                    body = body.child(div().id(("log", i)).min_h(px(ui::row_h())).flex().items_center().gap_2().px_1().rounded(px(ui::R_MD)).hover(|s| s.bg(crate::theme::hover_bg()))
                        .child(div().w(px(56.)).flex_shrink_0().text_color(c(th.overlay1)).child(e.at.clone()))
                        .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(if e.ok { c(th.success) } else { c(th.error) }))
                        .child(div().w(px(120.)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.subtext)).child(e.source.clone()))
                        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().font_family(ui::mono_family()).text_size(px(ui::fs_mono())).child(e.text.replace('\n', " ")))
                        .child(div().flex_shrink_0().text_color(if e.ok { c(th.subtext) } else { c(th.error) }).max_w(px(260.)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(e.detail.clone()))
                        .tooltip(crate::ui::tip(detail.clone())));
                }
            }
        }
        card_layout::island("bottom-panel").relative().w_full().h(px(h)).flex_shrink_0()
            .child(div().id("bottom-resize").absolute().top_0().left_0().w_full().h(px(4.)).cursor_row_resize()
                .hover(|d| d.bg(c(cur().accent).opacity(0.35)))
                .on_drag(BottomResize, |d: &BottomResize, _, _, cx| cx.new(|_| d.clone()))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| if ev.click_count() >= 2 { this.bottom_h = config::BOTTOM_H; this.save_bottom_h(); cx.notify(); })))
            .child(head).child(body)
            .into_any_element()
    }

    pub(crate) fn save_bottom_h(&mut self) {
        let h = self.bottom_h.round();
        if (config::get().sidebar.bottom_height - h).abs() >= 1.0 { let _ = config::update(|s| s.sidebar.bottom_height = h); }
    }
}
