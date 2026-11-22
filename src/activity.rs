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

pub const STRIP_W: f32 = 36.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Hosts,
    Agents,
    Sftp,
    Database,
    Redis,
    Docker,
    Brew,
    Ports,
    Tools,
}

impl Tool {
    /// 左侧模块栏。Agent 紧跟在主机/终端之后；Homebrew 只在 macOS 上出现。
    pub fn strip() -> Vec<Tool> {
        let mut v = vec![
            Tool::Hosts,
            Tool::Agents,
            Tool::Sftp,
            Tool::Database,
            Tool::Redis,
            Tool::Docker,
        ];
        if cfg!(target_os = "macos") {
            v.push(Tool::Brew);
        }
        v.push(Tool::Ports);
        v.push(Tool::Tools);
        v
    }
    pub fn id(self) -> &'static str {
        match self {
            Tool::Hosts => "hosts",
            Tool::Agents => "agents",
            Tool::Sftp => "sftp",
            Tool::Database => "database",
            Tool::Redis => "redis",
            Tool::Docker => "docker",
            Tool::Brew => "brew",
            Tool::Ports => "ports",
            Tool::Tools => "tools",
        }
    }
    pub fn from_id(s: &str) -> Tool {
        Self::strip()
            .into_iter()
            .find(|t| t.id() == s)
            .unwrap_or(Tool::Hosts)
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Hosts => t("tool.hosts"),
            Tool::Agents => t("tool.agents"),
            Tool::Sftp => t("tool.sftp"),
            Tool::Database => t("tool.database"),
            Tool::Redis => t("tool.redis"),
            Tool::Docker => t("tool.docker"),
            Tool::Brew => t("tool.brew"),
            Tool::Ports => t("tool.ports"),
            Tool::Tools => t("tool.tools"),
        }
    }
    pub fn icon(self) -> IconName {
        match self {
            Tool::Hosts => IconName::Server,
            Tool::Agents => IconName::BrainCircuit,
            Tool::Sftp => IconName::FolderSync,
            Tool::Database => IconName::Database,
            Tool::Redis => IconName::Layers,
            Tool::Docker => IconName::Container,
            Tool::Brew => IconName::Beer,
            Tool::Ports => IconName::ArrowLeftRight,
            Tool::Tools => IconName::PencilRuler,
        }
    }
    pub fn action(self) -> Action {
        match self {
            Tool::Hosts => Action::ToolHosts,
            Tool::Agents => Action::ToolAgents,
            Tool::Sftp => Action::ToolSftp,
            Tool::Database => Action::ToolDatabase,
            Tool::Redis => Action::ToolRedis,
            Tool::Docker => Action::ToolDocker,
            Tool::Brew => Action::ToolBrew,
            Tool::Ports => Action::ToolPorts,
            Tool::Tools => Action::ToolTools,
        }
    }
    pub fn from_action(a: Action) -> Option<Tool> {
        Self::strip().into_iter().find(|t| t.action() == a)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bottom {
    Transfers,
    Log,
}
impl Bottom {
    pub fn id(self) -> &'static str {
        match self {
            Bottom::Transfers => "transfers",
            Bottom::Log => "log",
        }
    }
    pub fn from_id(s: &str) -> Bottom {
        if s == "log" {
            Bottom::Log
        } else {
            Bottom::Transfers
        }
    }
}

/// One line of the execution log (database statements, later Redis / Docker commands).
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub at: String,
    pub source: String,
    pub text: String,
    pub ok: bool,
    pub detail: String,
}
pub type ExecLog = Arc<Mutex<VecDeque<LogEntry>>>;
const LOG_MAX: usize = 1000;
pub fn log_push(log: &ExecLog, source: &str, text: &str, ok: bool, detail: String) {
    let mut l = log.lock().unwrap();
    if l.len() >= LOG_MAX {
        l.pop_front();
    }
    l.push_back(LogEntry {
        at: chrono::Local::now().format("%H:%M:%S").to_string(),
        source: source.into(),
        text: text.into(),
        ok,
        detail,
    });
}

#[derive(Clone)]
pub struct BottomResize;
impl Render for BottomResize {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Root {
    /// Activity-strip click / Alt+N: switch module; on the active one, collapse or reopen its window.
    pub(crate) fn select_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if tool == self.tool {
            self.toggle_sidebar(cx);
        } else {
            self.tool = tool;
            if !self.sidebar_visible {
                self.toggle_sidebar(cx);
            }
            let _ = config::update(|s| s.sidebar.tool = tool.id().into());
            self.show_module(window, cx);
            if tool == Tool::Brew && !self.active_in_ws(cx) {
                self.open_brew(window, cx);
            } else if tool == Tool::Ports && !self.active_in_ws(cx) {
                let tg = self.ssh_target_for_tools(cx);
                self.open_ports_tab(tg, window, cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn toggle_bottom(&mut self, tab: Option<Bottom>, cx: &mut Context<Self>) {
        let was = self.bottom_open && !self.bottom_anim.is_some_and(|a| !a.0);
        let open = match tab {
            Some(t) if was && self.bottom_tab != t => {
                self.bottom_tab = t;
                true
            }
            Some(t) => {
                self.bottom_tab = t;
                !was
            }
            None => !was,
        };
        if open != was && !crate::motion::reduced() {
            // slide; when closing the panel stays mounted until the slide ends
            let seq = self.bottom_anim.map(|a| a.1 + 1).unwrap_or(1);
            self.bottom_anim = Some((open, seq));
            let dur = if open {
                crate::motion::PANEL_OPEN_MS
            } else {
                crate::motion::PANEL_CLOSE_MS
            };
            crate::motion::hold(dur + 20);
            if open {
                self.bottom_open = true;
            }
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(crate::motion::ms(dur)).await;
                let _ = this.update(cx, |r, cx| {
                    if r.bottom_anim.is_some_and(|a| a.1 == seq) {
                        r.bottom_anim = None;
                        if !open {
                            r.bottom_open = false;
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        } else {
            self.bottom_open = open;
        }
        let tab = self.bottom_tab.id();
        let _ = config::update(|s| {
            s.sidebar.bottom_open = open;
            s.sidebar.bottom_tab = tab.into();
        });
        cx.notify();
    }

    pub(crate) fn strip_btn(
        &self,
        id: impl Into<gpui::ElementId>,
        ic: IconName,
        tip: String,
        active: bool,
        badge: bool,
    ) -> gpui::Stateful<gpui::Div> {
        self.strip_btn_with(id, None, ic, tip, active, badge)
    }
    /// Strip button whose glyph is one of our own SVGs (`svg`) instead of a stock icon.
    pub(crate) fn strip_btn_with(
        &self,
        id: impl Into<gpui::ElementId>,
        svg: Option<&'static str>,
        ic: IconName,
        tip: String,
        active: bool,
        badge: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        ui::icon_button_svg(
            id,
            svg.map_or_else(|| ic.path(), gpui::SharedString::from),
            ui::IconBtn::Strip,
            tip,
            active,
            false,
        )
        .relative()
        .when(badge, |d| {
            d.child(
                div()
                    .absolute()
                    .top(px(4.))
                    .right(px(4.))
                    .size(px(6.))
                    .rounded_full()
                    .bg(c(th.accent)),
            )
        })
    }

    /// The left activity strip: modules only (session tools live in the right strip, settings in the title bar).
    pub(crate) fn activity_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // 展开 / 收起 / 动画中结构一致（顶部标题行由主布局提供），图标位置不变；高亮在收起动画开始时就撤掉
        let open = self.sidebar_visible && self.sb_reveal.as_ref().is_none_or(|r| r.expanding());
        let mut col = div()
            .id("activity")
            .w(px(STRIP_W))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .pb(px(8.))
            .pt(px(4.));
        let (att_count, att_urgent) = crate::attention::summary_and_sync_badge(self, cx);
        let ports_on = self
            .port_view
            .as_ref()
            .map(|v| v.read(cx).active_count())
            .unwrap_or(0);
        let th = cur();
        for (i, tool) in Tool::strip().into_iter().enumerate() {
            let tip = crate::sidebar::action_tip(tool.label(), &tool.action().binding());
            let active = open && self.tool == tool;
            let mut btn = self
                .strip_btn_with(
                    ("act", i),
                    match tool {
                        Tool::Agents => Some("shellax/agent.svg"),
                        Tool::Redis => Some("shellax/db/redis.svg"),
                        Tool::Docker => Some("shellax/db/docker.svg"),
                        Tool::Tools => Some("shellax/db/toolbox.svg"),
                        _ => None,
                    },
                    tool.icon(),
                    tip,
                    active,
                    false,
                )
                .on_click(cx.listener(move |this, _, _w, cx| this.select_tool(tool, _w, cx)));
            if active {
                btn = btn.child(
                    div()
                        .absolute()
                        .left(px(-2.))
                        .top(px(8.))
                        .bottom(px(8.))
                        .w(px(2.))
                        .rounded_full()
                        .bg(c(th.accent)),
                );
            }
            if tool == Tool::Agents && att_count > 0 {
                let bhex = if att_urgent { th.error } else { th.success };
                btn = btn.child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(2.))
                        .min_w(px(14.))
                        .h(px(14.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(c(bhex))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(ui::fs_caption()))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(crate::theme::on_color(bhex))
                        .child(if att_count > 9 {
                            "9+".to_string()
                        } else {
                            att_count.to_string()
                        }),
                );
            }
            if tool == Tool::Ports && ports_on > 0 {
                btn = btn.child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(2.))
                        .min_w(px(14.))
                        .h(px(14.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(c(th.success))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(ui::fs_caption()))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(crate::theme::on_color(th.success))
                        .child(if ports_on > 9 {
                            "9+".to_string()
                        } else {
                            ports_on.to_string()
                        }),
                );
            }
            col = col.child(btn);
        }
        // 外层框架色由根节点统一提供
        col
    }

    /// Body of the left tool window for the active module.
    pub(crate) fn tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tool {
            Tool::Hosts | Tool::Sftp => self.sidebar(window, cx).into_any_element(),
            Tool::Agents => {
                let p = self.agents_panel(window, cx);
                let h = self.sb_resize_handle(cx);
                self.sb_frame(cx).child(p).child(h).into_any_element()
            }
            Tool::Database => {
                let p = self.db_panel(window, cx);
                {
                    let h = self.sb_resize_handle(cx);
                    self.sb_frame(cx).child(p).child(h).into_any_element()
                }
            }
            Tool::Redis | Tool::Docker => {
                let k = if self.tool == Tool::Redis {
                    crate::kv_ui::AssetKind::Redis
                } else {
                    crate::kv_ui::AssetKind::Docker
                };
                let p = self.asset_panel(k, window, cx);
                {
                    let h = self.sb_resize_handle(cx);
                    self.sb_frame(cx).child(p).child(h).into_any_element()
                }
            }
            Tool::Tools => {
                let p = self.tools_panel(window, cx);
                {
                    let h = self.sb_resize_handle(cx);
                    self.sb_frame(cx).child(p).child(h).into_any_element()
                }
            }
            Tool::Brew => {
                let p = self.brew_panel(window, cx);
                {
                    let h = self.sb_resize_handle(cx);
                    self.sb_frame(cx).child(p).child(h).into_any_element()
                }
            }
            Tool::Ports => {
                let p = self.ports_panel(window, cx);
                {
                    let h = self.sb_resize_handle(cx);
                    self.sb_frame(cx).child(p).child(h).into_any_element()
                }
            }
        }
    }

    /// 打开或激活端口转发工作台标签页。
    pub(crate) fn open_ports_tab(
        &mut self,
        tg: Option<ssh_term_proto::ssh::SshTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pv = self.port_view(window, cx);
        pv.update(cx, |v, cx| {
            if v.target.is_none() || tg.is_some() {
                v.target = tg;
            }
            v.ensure_default_target();
            cx.notify();
        });
        if let Some(i) = self.tabs.iter().position(|t| {
            matches!(
                &t.layout.leaves()[0].read(cx).embed,
                Some(crate::termview::Embed::Port(_))
            )
        }) {
            self.active = i;
            self.tool = Tool::Ports;
            self.leave_settings();
            self.overlay = crate::Overlay::None;
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        let pane = self.embed_pane(crate::termview::Embed::Port(pv), window, cx);
        self.tool = Tool::Ports;
        self.push_pane(pane, window, cx);
    }

    /// 左侧端口转发面板：按 SSH 主机分组展示已保存规则、实时运行状态及一键启停。
    fn ports_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let pv = self.port_view(window, cx);
        let (items, active_n, host_filter) = {
            let v = pv.read(cx);
            (v.items.clone(), v.active_count(), v.host_filter.clone())
        };
        let total_n = items.len();
        let summary = tf(
            "ux.pf_active_summary",
            &[("on", &active_n), ("total", &total_n)],
        );

        let mut actions: Vec<AnyElement> = Vec::new();
        if active_n > 0 {
            let pv_stop = pv.clone();
            actions.push(
                ui::icon_button(
                    "pf-sb-stop-all",
                    IconName::Square,
                    ui::IconBtn::Sm,
                    t("ux.pf_stop_all"),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    pv_stop.update(cx, |v, cx| v.stop_all(cx));
                }))
                .into_any_element(),
            );
        }
        actions.push(
            ui::icon_button(
                "pf-sb-open",
                IconName::Plus,
                ui::IconBtn::Sm,
                t("ux.pf_open_ws"),
            )
            .on_click(cx.listener(|this, _, w, cx| {
                let tg = this.ssh_target_for_tools(cx);
                this.open_ports_tab(tg, w, cx);
            }))
            .into_any_element(),
        );

        let head = ui::panel_header(Tool::Ports.label(), Some(summary.into()), None, actions);

        if items.is_empty() {
            let open = ui::button("tool-ports-open", t("ux.pf_open_ws"), 1)
                .w_full()
                .justify_center()
                .on_click(cx.listener(|this, _, w, cx| {
                    let tg = this.ssh_target_for_tools(cx);
                    this.open_ports_tab(tg, w, cx);
                }))
                .into_any_element();
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(head)
                .child(ui::empty_state(
                    IconName::ArrowLeftRight,
                    t("ui.no_port_forwards"),
                    Some(t("tool.ports_desc").into()),
                    vec![open],
                    false,
                ))
                .into_any_element();
        }

        let mut hosts: Vec<String> = Vec::new();
        for r in &items {
            if !hosts.iter().any(|h| h == &r.spec.host) {
                hosts.push(r.spec.host.clone());
            }
        }

        let mut list = div()
            .id("pf-sb-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py(px(ui::SP_1))
            .flex()
            .flex_col()
            .gap(px(2.));

        for (hi, hname) in hosts.iter().enumerate() {
            let host_rules: Vec<_> = items
                .iter()
                .filter(|r| &r.spec.host == hname)
                .cloned()
                .collect();
            let host_on = host_rules.iter().filter(|r| r.is_on()).count();
            let h_selected = host_filter.as_deref() == Some(hname.as_str());
            let h_cl = hname.clone();
            let pv_h = pv.clone();
            list = list.child(
                div()
                    .id(("pf-sb-h", hi))
                    .mx(px(ui::SP_1))
                    .mt(px(if hi == 0 { 2. } else { 6. }))
                    .px(px(ui::SP_2))
                    .h(px(ui::SECTION_H))
                    .rounded(px(ui::R_SM))
                    .cursor_pointer()
                    .bg(if h_selected {
                        crate::theme::selected_bg()
                    } else {
                        gpui::transparent_black()
                    })
                    .hover(|s| s.bg(crate::theme::hover_bg()))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        let h_target = h_cl.clone();
                        pv_h.update(cx, |v, cx| {
                            if v.host_filter.as_deref() == Some(h_target.as_str()) {
                                v.host_filter = None;
                            } else {
                                v.host_filter = Some(h_target);
                            }
                            cx.notify();
                        });
                        this.open_ports_tab(None, w, cx);
                    }))
                    .child(icon(IconName::Server, ui::IC_XS, c(th.overlay1)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(ui::fs_caption()))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(th.subtext))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(hname.clone()),
                    )
                    .child(ui::tag(
                        format!("{host_on}/{}", host_rules.len()),
                        if host_on > 0 {
                            c(th.success)
                        } else {
                            c(th.overlay1)
                        },
                    )),
            );

            for (ri, r) in host_rules.into_iter().enumerate() {
                let st = r.state();
                let on = r.is_on();
                let dot_col = match &st {
                    ssh_term_proto::forward::State::Running { .. } => c(th.success),
                    ssh_term_proto::forward::State::Starting => c(th.info),
                    ssh_term_proto::forward::State::Error(_) => c(th.error),
                    ssh_term_proto::forward::State::Stopped => c(th.overlay0),
                };
                let kind_lbl = match r.spec.kind {
                    ssh_term_proto::forward::Kind::L => "L",
                    ssh_term_proto::forward::Kind::R => "R",
                    ssh_term_proto::forward::Kind::D => "D",
                };
                let listen_addr = crate::port_view::listen_addr_of(&r, &st);
                let title_str = match r.spec.kind {
                    ssh_term_proto::forward::Kind::D => {
                        format!(":{} · SOCKS5", r.spec.bind_port)
                    }
                    _ => format!(
                        ":{} → {}:{}",
                        r.spec.bind_port, r.spec.dest_host, r.spec.dest_port
                    ),
                };
                let grp = gpui::SharedString::from(format!("pf-sb-r-{hi}-{ri}"));
                let r_toggle = r.clone();
                let pv_toggle = pv.clone();
                let addr_copy = listen_addr.clone();
                list = list.child(
                    ui::list_row(grp.clone(), 0, false)
                        .group(grp.clone())
                        .pl(px(ui::SP_3))
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.open_ports_tab(None, w, cx);
                        }))
                        .child(div().size(px(6.)).rounded_full().bg(dot_col).flex_shrink_0())
                        .child(ui::tag(kind_lbl, c(th.accent)))
                        .child(ui::row_title(title_str).flex_1().min_w_0())
                        .child(
                            ui::row_actions(grp)
                                .child(
                                    ui::icon_button(
                                        gpui::SharedString::from(format!("pf-sbc-{hi}-{ri}")),
                                        IconName::Copy,
                                        ui::IconBtn::Sm,
                                        t("ux.pf_copy_addr"),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            addr_copy.clone(),
                                        ));
                                        this.toast(
                                            tf("ux.pf_copied_addr", &[("addr", &addr_copy)]),
                                            cx,
                                        );
                                    })),
                                )
                                .child(
                                    ui::icon_button(
                                        gpui::SharedString::from(format!("pf-sbt-{hi}-{ri}")),
                                        if on {
                                            IconName::Square
                                        } else {
                                            IconName::Play
                                        },
                                        ui::IconBtn::Sm,
                                        if on { t("pf.stop") } else { t("pf.start") },
                                    )
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        let r_cl = r_toggle.clone();
                                        pv_toggle.update(cx, |v, cx| {
                                            if r_cl.is_on() {
                                                v.stop_rule(r_cl, cx);
                                            } else {
                                                v.start_rule(r_cl, cx);
                                            }
                                        });
                                    })),
                                ),
                        ),
                );
            }
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(head)
            .child(list)
            .into_any_element()
    }

    pub(crate) fn transfers_active(&self, cx: &gpui::App) -> bool {
        self.transfer_jobs(cx)
            .iter()
            .any(|(_, j)| matches!(j.state(), JobState::Queued | JobState::Running))
    }

    /// Jobs of every open file browser, labelled with its title.
    fn transfer_jobs(&self, cx: &gpui::App) -> Vec<(String, Arc<ssh_term_proto::sftp::Job>)> {
        let mut out = vec![];
        for tab in &self.tabs {
            for p in tab.layout.leaves() {
                let v = p.read(cx);
                if let Some(s) = &v.sftp {
                    let sv = s.read(cx);
                    let label = sv.title();
                    for (_, q) in sv.queues() {
                        for j in q.jobs.lock().unwrap().iter() {
                            out.push((label.clone(), j.clone()));
                        }
                    }
                }
            }
        }
        out
    }

    /// Bottom tool window under the editor area.
    pub(crate) fn bottom_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let h = self.bottom_h;
        let tab = |id: &'static str, label: String, on: bool, which: Bottom| {
            ui::chip(id, label, on)
                .on_click(cx.listener(move |this, _, _w, cx| {
                    this.bottom_tab = which;
                    let _ = config::update(|s| s.sidebar.bottom_tab = which.id().into());
                    cx.notify();
                }))
                .into_any_element()
        };
        let jobs = self.transfer_jobs(cx);
        let log: Vec<LogEntry> = self
            .exec_log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(300)
            .cloned()
            .collect();
        let mut actions: Vec<AnyElement> = vec![];
        if self.bottom_tab == Bottom::Log && !log.is_empty() {
            actions.push(
                ui::icon_button_state(
                    "bt-clear",
                    IconName::Trash,
                    ui::IconBtn::Md,
                    t("tool.clear"),
                    false,
                    true,
                )
                .on_click(cx.listener(|this, _, _w, cx| {
                    this.exec_log.lock().unwrap().clear();
                    cx.notify();
                }))
                .into_any_element(),
            );
        }
        actions.push(
            ui::icon_button(
                "bt-close",
                IconName::PanelBottomClose,
                ui::IconBtn::Md,
                t("ui.close"),
            )
            .on_click(cx.listener(|this, _, _w, cx| this.toggle_bottom(None, cx)))
            .into_any_element(),
        );
        let head = ui::panel_header_with(
            vec![
                tab(
                    "bt-transfers",
                    tf("tool.transfers_n", &[("n", &jobs.len())]),
                    self.bottom_tab == Bottom::Transfers,
                    Bottom::Transfers,
                ),
                tab(
                    "bt-log",
                    t("tool.exec_log").to_string(),
                    self.bottom_tab == Bottom::Log,
                    Bottom::Log,
                ),
            ],
            actions,
        );
        let mut body = div()
            .id("bottom-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .px(px(ui::SP_2))
            .pb(px(ui::SP_2))
            .text_size(px(ui::fs_small()));
        let empty = |ic: IconName, s: &str| ui::empty_state(ic, s.to_string(), None, vec![], true);
        match self.bottom_tab {
            Bottom::Transfers => {
                if jobs.is_empty() {
                    body = body.child(empty(IconName::ArrowDownUp, t("tool.transfers_empty")));
                }
                for (i, (src, j)) in jobs.iter().rev().enumerate() {
                    let st = j.state();
                    let (label, col) = match st {
                        JobState::Queued => (t("tool.job_queued"), th.overlay1),
                        JobState::Running => (t("tool.job_running"), th.accent),
                        JobState::Done => (t("tool.job_done"), th.success),
                        JobState::Failed => (t("tool.job_failed"), th.error),
                        JobState::Paused => (t("tool.job_paused"), th.warning),
                    };
                    let p = j.progress().clamp(0., 1.);
                    let arrow = if j.dir == ssh_term_proto::sftp::Dir::Upload {
                        IconName::Upload
                    } else {
                        IconName::Download
                    };
                    body = body.child(
                        div()
                            .id(("job", i))
                            .min_h(px(ui::row_h()))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_1()
                            .rounded(px(ui::R_MD))
                            .hover(|s| s.bg(crate::theme::hover_bg()))
                            .child(icon(arrow, ui::IC_XS, c(th.overlay1)))
                            .child(
                                div()
                                    .w(px(180.))
                                    .flex_shrink_0()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(j.name()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(c(th.subtext))
                                    .child(format!("{src} · {}", j.remote)),
                            )
                            .child(
                                div()
                                    .w(px(120.))
                                    .h(px(4.))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(c(th.text).opacity(0.09))
                                    .child(
                                        div().h_full().rounded_full().bg(c(col)).w(px(120. * p)),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(56.))
                                    .flex_shrink_0()
                                    .text_color(c(col))
                                    .child(label.to_string()),
                            )
                            .when_some(j.error.lock().unwrap().clone(), |d, e| {
                                d.tooltip(crate::ui::tip(e.clone()))
                            }),
                    );
                }
            }
            Bottom::Log => {
                if log.is_empty() {
                    body = body.child(empty(IconName::ScrollText, t("tool.log_empty")));
                }
                for (i, e) in log.iter().enumerate() {
                    let detail = e.detail.clone();
                    body = body.child(
                        div()
                            .id(("log", i))
                            .min_h(px(ui::row_h()))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_1()
                            .rounded(px(ui::R_MD))
                            .hover(|s| s.bg(crate::theme::hover_bg()))
                            .child(
                                div()
                                    .w(px(62.))
                                    .flex_shrink_0()
                                    .font_family(ui::mono_family())
                                    .text_size(px(ui::fs_caption()))
                                    .text_color(c(th.overlay1))
                                    .child(e.at.clone()),
                            )
                            .child(
                                div()
                                    .size(px(6.))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(if e.ok { c(th.success) } else { c(th.error) }),
                            )
                            .child(
                                div()
                                    .w(px(120.))
                                    .flex_shrink_0()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(c(th.subtext))
                                    .child(e.source.clone()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .font_family(ui::mono_family())
                                    .text_size(px(ui::fs_mono()))
                                    .child(e.text.replace('\n', " ")),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(if e.ok { c(th.subtext) } else { c(th.error) })
                                    .max_w(px(260.))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(e.detail.clone()),
                            )
                            .tooltip(crate::ui::tip(detail.clone())),
                    );
                }
            }
        }
        card_layout::island("bottom-panel")
            .relative()
            .w_full()
            .h(px(h))
            .flex_shrink_0()
            .child(
                div()
                    .id("bottom-resize")
                    .absolute()
                    .top_0()
                    .left_0()
                    .w_full()
                    .h(px(4.))
                    .cursor_row_resize()
                    .hover(|d| d.bg(c(cur().accent).opacity(0.35)))
                    .on_drag(BottomResize, |d: &BottomResize, _, _, cx| {
                        cx.new(|_| d.clone())
                    })
                    .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| {
                        if ev.click_count() >= 2 {
                            this.bottom_h = config::BOTTOM_H;
                            this.save_bottom_h();
                            cx.notify();
                        }
                    })),
            )
            .child(head)
            .child(body)
            .into_any_element()
    }

    pub(crate) fn save_bottom_h(&mut self) {
        let h = self.bottom_h.round();
        if (config::get().sidebar.bottom_height - h).abs() >= 1.0 {
            let _ = config::update(|s| s.sidebar.bottom_height = h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brew_sits_between_docker_and_ports_only_on_macos() {
        let ids: Vec<_> = Tool::strip()
            .into_iter()
            .map(|t| t.id())
            .collect::<Vec<_>>();
        if cfg!(target_os = "macos") {
            let i = ids.iter().position(|s| *s == "brew").expect("brew");
            assert_eq!(ids[i - 1], "docker");
            assert_eq!(ids[i + 1], "ports");
            assert_eq!(Tool::from_id("brew"), Tool::Brew);
        } else {
            assert!(!ids.contains(&"brew"));
            assert_eq!(Tool::from_id("brew"), Tool::Hosts);
        }
    }
}
