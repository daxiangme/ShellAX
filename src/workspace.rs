//! Per-module workspaces: every left-strip module (terminal, SFTP, database, Redis, Docker)
//! owns its own set of tabs. The tab row and the main area show only the current module's
//! tabs; a module without tabs shows its own empty state. The last active tab of each module
//! is remembered, and opening a tab from anywhere brings its module to the front.
use gpui::{div, prelude::*, px, AnyElement, App, Context, EntityId, FontWeight, SharedString, Window};
use ssh_term_proto::i18n::t;

use crate::activity::Tool;
use crate::actions::Action;
use crate::termview::{Embed, TermView};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{config, Root, Tab};
use ssh_term_proto::sftp_mode::Mode as SftpMode;

impl Tool {
    /// The workspace a module shows: port forwarding works on terminal sessions.
    pub fn workspace(self) -> Tool { match self { Tool::Ports => Tool::Hosts, t => t } }
}

/// Module a pane belongs to, from what it hosts (an explicit `ws` wins, e.g. a container shell).
pub(crate) fn pane_module(v: &TermView, cx: &App) -> Tool {
    if let Some(w) = v.ws { return w; }
    if v.sftp.is_some() { return Tool::Sftp; }
    if v.db.is_some() { return Tool::Database; }
    match &v.embed {
        Some(Embed::Kv(_)) => Tool::Redis,
        Some(Embed::Dock(_) | Embed::Log(_)) => Tool::Docker,
        Some(Embed::Mongo(_)) => Tool::Database,
        Some(Embed::Gone(g)) => g.read(cx).module,
        Some(Embed::Tool(_)) => Tool::Tools,
        None => Tool::Hosts,
    }
}

impl Root {
    pub(crate) fn tab_module(tab: &Tab, cx: &App) -> Tool { pane_module(tab.layout.leaves()[0].read(cx), cx) }
    fn tab_key(tab: &Tab) -> EntityId { tab.layout.leaves()[0].entity_id() }
    /// Indexes of the current module's tabs, in order.
    pub(crate) fn ws_tabs(&self, cx: &App) -> Vec<usize> {
        let ws = self.tool.workspace();
        (0..self.tabs.len()).filter(|&i| Self::tab_module(&self.tabs[i], cx) == ws).collect()
    }
    /// Is the active tab one of the current module's (false = module empty state is shown)?
    pub(crate) fn active_in_ws(&self, cx: &App) -> bool { self.tabs.get(self.active).is_some_and(|t| Self::tab_module(t, cx) == self.tool.workspace()) }

    /// Called every frame: when code made another tab active (opened a DB tab from the tree, a
    /// container shell, a restored layout …) its module comes to the front.
    pub(crate) fn sync_workspace(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let (key, m) = (Self::tab_key(tab), Self::tab_module(tab, cx));
        match self.ws_seen {
            None => {
                // first frame after start / restore: show the remembered module's own tab
                self.ws_seen = Some(key);
                if m != self.tool.workspace() {
                    if let Some(&i) = self.ws_tabs(cx).last() { self.active = i; self.ws_seen = Some(Self::tab_key(&self.tabs[i])); }
                }
            }
            Some(k) if k != key => {
                self.ws_seen = Some(key);
                if m != self.tool.workspace() {
                    self.tool = m;
                    let _ = config::update(|s| s.sidebar.tool = m.id().into());
                }
            }
            _ => {}
        }
        if m == self.tool.workspace() { self.ws_last.insert(m.id(), key); }
    }

    /// The module changed: show its last active tab (or its empty state).
    pub(crate) fn show_module(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ws = self.tool.workspace();
        if self.active_in_ws(cx) { return; }
        let mine = self.ws_tabs(cx);
        let last = self.ws_last.get(ws.id()).copied();
        let pick = mine.iter().copied().find(|&i| Some(Self::tab_key(&self.tabs[i])) == last).or(mine.last().copied());
        if let Some(i) = pick {
            self.active = i;
            self.ws_seen = Some(Self::tab_key(&self.tabs[i]));
            self.leave_settings();
            self.focus_active(window, cx);
        } else {
            // keep the hidden tab active but do not let it pull its module back
            self.ws_seen = self.tabs.get(self.active).map(Self::tab_key);
            window.blur(cx);
        }
        cx.notify();
    }

    /// After tab `closed_module`'s tab went away: stay inside that module when it has tabs left.
    pub(crate) fn after_close(&mut self, closed_module: Tool, near: usize, cx: &App) {
        let same: Vec<usize> = (0..self.tabs.len()).filter(|&i| Self::tab_module(&self.tabs[i], cx) == closed_module).collect();
        if let Some(&i) = same.iter().rev().find(|&&i| i < near).or(same.first()) { self.active = i; }
        self.ws_seen = self.tabs.get(self.active).map(Self::tab_key);
    }

    /// Snippet module of the current workspace.
    pub(crate) fn snippet_module(&self) -> &'static str { match self.tool.workspace() { Tool::Hosts => "terminal", t => t.id() } }

    /// "+" in the tab row outside the terminal module; false = terminal (use the new-tab menu).
    pub(crate) fn new_module_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.tool.workspace() {
            Tool::Sftp => self.open_sftp_mode(None, SftpMode::Tab, window, cx),
            Tool::Database => self.db_new_query(None, window, cx),
            Tool::Redis => self.open_asset_dialog(crate::kv_ui::AssetKind::Redis, None, window, cx),
            Tool::Docker => self.open_asset_dialog(crate::kv_ui::AssetKind::Docker, None, window, cx),
            Tool::Tools => { if !self.sidebar_visible { self.toggle_sidebar(cx); } }
            _ => return false,
        }
        true
    }

    /// Next / previous tab inside the current module.
    pub(crate) fn cycle_ws_tab(&mut self, forward: bool, cx: &App) {
        let mine = self.ws_tabs(cx);
        if mine.is_empty() { return; }
        let pos = mine.iter().position(|&i| i == self.active);
        let n = mine.len();
        let next = match pos { Some(p) => if forward { (p + 1) % n } else { (p + n - 1) % n }, None => if forward { 0 } else { n - 1 } };
        self.active = mine[next];
    }

    /// Main area when the current module has no open tabs.
    pub(crate) fn ws_empty(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let tool = self.tool.workspace();
        let s = config::get();
        let row = |id: SharedString, ic: IconName, main: String, sub: String| div().id(id).w(px(360.)).h(px(36.)).px_3().rounded(px(8.)).flex().items_center().gap_2().cursor_pointer()
            .bg(c(th.surface0).opacity(0.5)).hover(|d| d.bg(c(cur().surface1)))
            .child(icon(ic, 14., c(th.accent)))
            .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(13.)).text_color(c(th.text)).child(main))
            .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(sub));
        let (title, hint) = match tool {
            Tool::Sftp => (t("ws.sftp_title"), t("ws.sftp_hint")),
            Tool::Database => (t("ws.db_title"), t("ws.db_hint")),
            Tool::Redis => (t("ws.redis_title"), t("ws.redis_hint")),
            Tool::Docker => (t("ws.docker_title"), t("ws.docker_hint")),
            Tool::Tools => (t("ws.tools_title"), t("ws.tools_hint")),
            _ => (t("ui.no_open_terminals"), ""),
        };
        let mut list = div().flex().flex_col().gap_1().max_h(px(320.)).id("ws-list").overflow_y_scroll();
        let mut buttons = div().flex().gap_2();
        match tool {
            Tool::Sftp => {
                for (i, h) in s.hosts.iter().take(12).enumerate() {
                    let name = h.name.clone();
                    list = list.child(row(format!("ws-h-{i}").into(), IconName::Server, h.name.clone(), h.host.clone())
                        .on_click(cx.listener(move |this, _, w, cx| { let tg = this.target_for(&name); this.open_sftp_mode(tg, SftpMode::Tab, w, cx); })));
                }
                buttons = buttons.child(ui::button("ws-sftp-new", t("ws.sftp_open"), 1).on_click(cx.listener(|this, _, w, cx| this.open_sftp_mode(None, SftpMode::Tab, w, cx))));
            }
            Tool::Database => {
                for (i, d) in s.db.iter().take(12).enumerate() {
                    let id = d.id.clone();
                    list = list.child(row(format!("ws-d-{i}").into(), IconName::Database, d.name.clone(), d.kind.label().to_string())
                        .on_click(cx.listener(move |this, _, w, cx| this.db_new_query(Some(id.clone()), w, cx))));
                }
                buttons = buttons.child(ui::button("ws-db-new", t("db.new_connection"), 1).on_click(cx.listener(|this, _, w, cx| this.run(Action::NewDbConnection, w, cx))));
            }
            Tool::Redis => {
                for (i, r) in s.redis.iter().take(12).enumerate() {
                    let id = r.id.clone();
                    list = list.child(row(format!("ws-r-{i}").into(), IconName::Layers, r.name.clone(), String::new())
                        .on_click(cx.listener(move |this, _, w, cx| this.open_redis_tab(&id, w, cx))));
                }
                buttons = buttons.child(ui::button("ws-redis-new", t("ws.redis_new"), 1).on_click(cx.listener(|this, _, w, cx| this.open_asset_dialog(crate::kv_ui::AssetKind::Redis, None, w, cx))));
            }
            Tool::Docker => {
                for (i, e) in s.docker.iter().take(12).enumerate() {
                    let id = e.id.clone();
                    list = list.child(row(format!("ws-k-{i}").into(), IconName::Container, e.name.clone(), String::new())
                        .on_click(cx.listener(move |this, _, w, cx| this.open_docker_tab(&id, w, cx))));
                }
                buttons = buttons.child(ui::button("ws-docker-new", t("ws.docker_new"), 1).on_click(cx.listener(|this, _, w, cx| this.open_asset_dialog(crate::kv_ui::AssetKind::Docker, None, w, cx))));
            }
            Tool::Tools => {
                let all = ssh_term_proto::tools::builtin();
                let mut ids: Vec<String> = s.sidebar.tools_fav.iter().chain(&s.sidebar.tools_recent).cloned().collect();
                ids.dedup();
                if ids.is_empty() { ids = ["json", "timestamp", "base64", "url", "uuid", "password"].iter().map(|x| x.to_string()).collect(); }
                let mut seen = std::collections::HashSet::new();
                for (i, sp) in ids.iter().filter(|x| seen.insert((*x).clone())).filter_map(|x| all.iter().find(|s| &s.id == x)).take(10).enumerate() {
                    let id = sp.id.clone();
                    list = list.child(row(format!("ws-t-{i}").into(), IconName::PencilRuler, sp.name.clone(), ssh_term_proto::tools::cat_label(&sp.cat))
                        .on_click(cx.listener(move |this, _, w, cx| this.open_tool(&id, None, w, cx))));
                }
            }
            _ => {
                buttons = buttons.child(ui::button("empty-local", t("ui.open_local_terminal"), 1).on_click(cx.listener(|this, _, w, cx| this.new_tab(crate::termview::Spec::Local, w, cx))))
                    .child(ui::button("empty-palette", t("ui.command_palette"), 0).on_click(cx.listener(|this, _, w, cx| this.open_palette(w, cx))))
                    .when(!self.closed_tabs.is_empty(), |d| d.child(ui::button("empty-reopen", t("tab.reopen"), 0).on_click(cx.listener(|this, _, w, cx| this.reopen_closed(w, cx)))));
            }
        }
        div().flex_1().size_full().flex().flex_col().items_center().justify_center().gap_3().bg(c(th.base))
            .child(icon(if tool == Tool::Hosts { IconName::SquareTerminal } else { tool.icon() }, 36., c(th.overlay0)))
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title.to_string()))
            .when(!hint.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(c(th.subtext)).child(hint.to_string())))
            .child(list)
            .child(buttons)
            .into_any_element()
    }
}
