//! IDE-style tool-window shell (v0.2 PRD §3): the 40px activity strip on the far left, the
//! per-module tool window it opens, and the bottom tool window (transfer queue, execution
//! log). Clicking the active module's icon collapses its window; Alt+1…7 switch modules.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, Window};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp::JobState;

use crate::actions::Action;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{chrome, config, Root, SideDock};

pub const STRIP_W: f32 = 40.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool { Hosts, Sftp, Database, Redis, Docker, Ports, Snippets }

impl Tool {
    pub const TOP: [Tool; 7] = [Tool::Hosts, Tool::Sftp, Tool::Database, Tool::Redis, Tool::Docker, Tool::Ports, Tool::Snippets];
    pub fn id(self) -> &'static str {
        match self { Tool::Hosts => "hosts", Tool::Sftp => "sftp", Tool::Database => "database", Tool::Redis => "redis", Tool::Docker => "docker", Tool::Ports => "ports", Tool::Snippets => "snippets" }
    }
    pub fn from_id(s: &str) -> Tool { Tool::TOP.into_iter().find(|t| t.id() == s).unwrap_or(Tool::Hosts) }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Hosts => t("tool.hosts"), Tool::Sftp => t("tool.sftp"), Tool::Database => t("tool.database"), Tool::Redis => t("tool.redis"),
            Tool::Docker => t("tool.docker"), Tool::Ports => t("tool.ports"), Tool::Snippets => t("tool.snippets"),
        }
    }
    pub fn icon(self) -> IconName {
        match self {
            Tool::Hosts => IconName::Server, Tool::Sftp => IconName::FolderSync, Tool::Database => IconName::Database, Tool::Redis => IconName::Layers,
            Tool::Docker => IconName::Container, Tool::Ports => IconName::ArrowLeftRight, Tool::Snippets => IconName::Code,
        }
    }
    pub fn action(self) -> Action {
        match self {
            Tool::Hosts => Action::ToolHosts, Tool::Sftp => Action::ToolSftp, Tool::Database => Action::ToolDatabase, Tool::Redis => Action::ToolRedis,
            Tool::Docker => Action::ToolDocker, Tool::Ports => Action::ToolPorts, Tool::Snippets => Action::ToolSnippets,
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
        match tab {
            Some(t) if self.bottom_open && self.bottom_tab != t => self.bottom_tab = t,
            Some(t) => { self.bottom_tab = t; self.bottom_open = !self.bottom_open; }
            None => self.bottom_open = !self.bottom_open,
        }
        let (open, tab) = (self.bottom_open, self.bottom_tab.id());
        let _ = config::update(|s| { s.sidebar.bottom_open = open; s.sidebar.bottom_tab = tab.into(); });
        cx.notify();
    }

    fn strip_btn(&self, id: impl Into<gpui::ElementId>, ic: IconName, tip: String, active: bool, badge: bool) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        div().id(id).relative().size(px(32.)).rounded(px(8.)).flex().items_center().justify_center().cursor_pointer()
            .when(active, |d| d.bg(c(th.accent).opacity(0.12)))
            .when(!active, |d| d.hover(|s| s.bg(c(cur().surface0))))
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .child(icon(ic, 17., if active { c(th.accent) } else { c(th.overlay1) }))
            .when(badge, |d| d.child(div().absolute().top(px(4.)).right(px(4.)).size(px(6.)).rounded_full().bg(c(th.accent))))
    }

    /// The left activity strip: modules on top; AI, transfers, audit and settings at the bottom.
    pub(crate) fn activity_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        // layout follows `sidebar_visible` (stays true until a collapse animation ends) so icons never jump
        let open = self.sidebar_visible;
        let mut col = div().id("activity").w(px(STRIP_W)).flex_shrink_0().h_full().flex().flex_col().items_center().gap(px(4.)).pb(px(8.));
        // no window head while the panel is collapsed: keep the top row draggable
        if !open { col = col.child(chrome::drag_region(div().id("act-drag")).h(px(chrome::TAB_H)).w_full().flex_shrink_0().window_control_area(gpui::WindowControlArea::Drag)); } else { col = col.pt(px(4.)); }
        for (i, tool) in Tool::TOP.into_iter().enumerate() {
            let tip = crate::sidebar::action_tip(tool.label(), &tool.action().binding());
            col = col.child(self.strip_btn(("act", i), tool.icon(), tip, open && self.tool == tool, false)
                .on_click(cx.listener(move |this, _, _w, cx| this.select_tool(tool, _w, cx))));
        }
        col = col.child(div().flex_1());
        let busy = self.transfers_active(cx);
        let n_agents = self.tabs.iter().flat_map(|t| t.layout.leaves()).filter(|p| p.read(cx).agent.is_some()).count();
        col = col
            .child(self.strip_btn("act-ai", IconName::Bot, crate::sidebar::action_tip("AI / Agent", &Action::OpenAi.binding()), self.dock == SideDock::Ai, n_agents > 0)
                .on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenAi, w, cx))))
            .child(self.strip_btn("act-transfers", IconName::ArrowDownUp, crate::sidebar::action_tip(t("tool.transfers"), &Action::ToggleBottomPanel.binding()), self.bottom_open && self.bottom_tab == Bottom::Transfers, busy)
                .on_click(cx.listener(|this, _, _w, cx| this.toggle_bottom(Some(Bottom::Transfers), cx))))
            .child(self.strip_btn("act-audit", IconName::ScrollText, crate::sidebar::action_tip(t("ui.audit_log"), &Action::OpenAudit.binding()), false, false)
                .on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenAudit, w, cx))))
            .child(self.strip_btn("act-settings", IconName::Settings, crate::sidebar::action_tip(t("ui.settings"), &Action::OpenSettings.binding()), self.settings_active(), false)
                .on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenSettings, w, cx))));
        let vib = if chrome::NATIVE_LIGHTS { 0.72 } else { 1.0 };
        col.bg(c(th.crust).opacity(vib))
    }

    /// Body of the left tool window for the active module.
    pub(crate) fn tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tool {
            Tool::Hosts | Tool::Sftp => self.sidebar(window, cx).into_any_element(),
            Tool::Database => {
                let p = self.db_panel(window, cx);
                let vib = if chrome::NATIVE_LIGHTS { 0.72 } else { 1.0 };
                self.sb_frame(cx).bg(c(cur().crust).opacity(vib)).child(p).into_any_element()
            }
            Tool::Redis | Tool::Docker => {
                let k = if self.tool == Tool::Redis { crate::kv_ui::AssetKind::Redis } else { crate::kv_ui::AssetKind::Docker };
                let p = self.asset_panel(k, cx);
                let vib = if chrome::NATIVE_LIGHTS { 0.72 } else { 1.0 };
                self.sb_frame(cx).bg(c(cur().crust).opacity(vib)).child(p).into_any_element()
            }
            other => {
                let body = self.empty_tool(other, cx);
                let vib = if chrome::NATIVE_LIGHTS { 0.72 } else { 1.0 };
                self.sb_frame(cx).bg(c(cur().crust).opacity(vib)).child(panel_title(other.label(), None)).child(body).into_any_element()
            }
        }
    }

    /// Modules whose full window is not built yet: what it will do plus the existing entry points.
    fn empty_tool(&mut self, tool: Tool, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let (desc, soon) = match tool {
            Tool::Redis => (t("tool.redis_desc"), true),
            Tool::Docker => (t("tool.docker_desc"), false),
            Tool::Ports => (t("tool.ports_desc"), false),
            _ => (t("tool.snippets_desc"), false),
        };
        let full = |b: gpui::Stateful<gpui::Div>| b.w_full().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().justify_center();
        let mut card = div().mx_3().my_2().p_3().min_w_0().overflow_hidden().rounded(px(10.)).bg(c(th.surface0).opacity(0.5)).flex().flex_col().gap_2()
            .child(div().flex().items_center().gap_2().child(icon(tool.icon(), 18., c(th.accent))).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(tool.label().to_string()))
                .when(soon, |d| d.child(div().px(px(6.)).rounded(px(4.)).bg(c(th.surface1)).text_size(px(10.)).text_color(c(th.subtext)).child(t("tool.soon")))))
            .child(div().w_full().min_w_0().whitespace_normal().text_size(px(12.)).text_color(c(th.subtext)).child(desc));
        card = match tool {
            Tool::Docker => card.child(full(ui::button("tool-docker-open", t("tool.docker_open"), 1)).on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenHostTools, w, cx)))),
            Tool::Ports => card.child(full(ui::button("tool-ports-open", t("tool.ports_open"), 1)).on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenPortForward, w, cx)))),
            Tool::Snippets => card
                .child(full(ui::button("tool-snip-open", t("tool.snippets_open"), 1)).on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenSnippets, w, cx))))
                .child(full(ui::button("tool-hist-open", t("lib.history"), 0)).on_click(cx.listener(|this, _, w, cx| this.run(Action::OpenHistory, w, cx)))),
            _ => card,
        };
        div().flex_1().min_h_0().flex().flex_col().child(card).into_any_element()
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
        let tab = |id: &'static str, label: String, on: bool, which: Bottom| div().id(id).px_2().h(px(22.)).rounded(px(6.)).flex().items_center().cursor_pointer().text_size(px(12.))
            .when(on, |d| d.bg(c(cur().accent).opacity(0.16)).text_color(c(cur().accent)).font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(c(cur().subtext)).hover(|s| s.bg(c(cur().surface0))))
            .child(label)
            .on_click(cx.listener(move |this, _, _w, cx| { this.bottom_tab = which; let _ = config::update(|s| s.sidebar.bottom_tab = which.id().into()); cx.notify(); }));
        let jobs = self.transfer_jobs(cx);
        let log: Vec<LogEntry> = self.exec_log.lock().unwrap().iter().rev().take(300).cloned().collect();
        let head = div().h(px(30.)).px_2().flex().items_center().gap_1().flex_shrink_0()
            .child(tab("bt-transfers", tf("tool.transfers_n", &[("n", &jobs.len())]), self.bottom_tab == Bottom::Transfers, Bottom::Transfers))
            .child(tab("bt-log", t("tool.exec_log").to_string(), self.bottom_tab == Bottom::Log, Bottom::Log))
            .child(div().flex_1())
            .when(self.bottom_tab == Bottom::Log && !log.is_empty(), |d| d.child(div().id("bt-clear").size(px(22.)).rounded(px(6.)).flex().items_center().justify_center().cursor_pointer().hover(|s| s.bg(c(cur().surface0)))
                .tooltip(|w, cx| gpui_component::tooltip::Tooltip::new(t("tool.clear")).build(w, cx))
                .on_click(cx.listener(|this, _, _w, cx| { this.exec_log.lock().unwrap().clear(); cx.notify(); }))
                .child(icon(IconName::Trash, 13., c(th.overlay1)))))
            .child(div().id("bt-close").size(px(22.)).rounded(px(6.)).flex().items_center().justify_center().cursor_pointer().hover(|s| s.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, _w, cx| this.toggle_bottom(None, cx)))
                .child(icon(IconName::PanelBottomClose, 13., c(th.overlay1))));
        let mut body = div().id("bottom-body").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().px_2().pb_2().text_size(px(12.));
        let empty = |s: &str| div().p_3().text_color(c(cur().overlay1)).child(s.to_string());
        match self.bottom_tab {
            Bottom::Transfers => {
                if jobs.is_empty() { body = body.child(empty(t("tool.transfers_empty"))); }
                for (i, (src, j)) in jobs.iter().rev().enumerate() {
                    let st = j.state();
                    let (label, col) = match st {
                        JobState::Queued => (t("tool.job_queued"), th.overlay1), JobState::Running => (t("tool.job_running"), th.accent),
                        JobState::Done => (t("tool.job_done"), th.success), JobState::Failed => (t("tool.job_failed"), th.error), JobState::Paused => (t("tool.job_paused"), th.warning),
                    };
                    let p = j.progress().clamp(0., 1.);
                    let arrow = if j.dir == ssh_term_proto::sftp::Dir::Upload { IconName::Upload } else { IconName::Download };
                    body = body.child(div().id(("job", i)).h(px(26.)).flex().items_center().gap_2().px_1().rounded(px(6.)).hover(|s| s.bg(c(cur().surface0).opacity(0.6)))
                        .child(icon(arrow, 13., c(th.overlay1)))
                        .child(div().w(px(180.)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(j.name()))
                        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.subtext)).child(format!("{src} · {}", j.remote)))
                        .child(div().w(px(120.)).h(px(4.)).flex_shrink_0().rounded_full().bg(c(th.surface1)).child(div().h_full().rounded_full().bg(c(col)).w(px(120. * p))))
                        .child(div().w(px(56.)).flex_shrink_0().text_color(c(col)).child(label.to_string()))
                        .when_some(j.error.lock().unwrap().clone(), |d, e| d.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(e.clone()).build(w, cx))));
                }
            }
            Bottom::Log => {
                if log.is_empty() { body = body.child(empty(t("tool.log_empty"))); }
                for (i, e) in log.iter().enumerate() {
                    let detail = e.detail.clone();
                    body = body.child(div().id(("log", i)).min_h(px(22.)).flex().items_center().gap_2().px_1().rounded(px(6.)).hover(|s| s.bg(c(cur().surface0).opacity(0.6)))
                        .child(div().w(px(56.)).flex_shrink_0().text_color(c(th.overlay1)).child(e.at.clone()))
                        .child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(if e.ok { c(th.success) } else { c(th.error) }))
                        .child(div().w(px(120.)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_color(c(th.subtext)).child(e.source.clone()))
                        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().font_family(gpui::SharedString::from(config::get().font.family.clone())).child(e.text.replace('\n', " ")))
                        .child(div().flex_shrink_0().text_color(if e.ok { c(th.subtext) } else { c(th.error) }).max_w(px(260.)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(e.detail.clone()))
                        .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(detail.clone()).build(w, cx)));
                }
            }
        }
        div().id("bottom-panel").relative().w_full().h(px(h)).flex_shrink_0().flex().flex_col().rounded(px(10.)).bg(c(th.mantle)).overflow_hidden()
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

/// Title row of a tool window: name on the left, optional actions on the right.
pub fn panel_title(label: &str, actions: Option<AnyElement>) -> impl IntoElement {
    let th = cur();
    div().h(px(28.)).px(px(10.)).flex_shrink_0().flex().items_center().gap_1()
        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.overlay1)).child(label.to_uppercase()))
        .when_some(actions, |d, a| d.child(a))
}
