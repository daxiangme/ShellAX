//! AgentShell prototype v5 UI: sidebar (host search / groups / function area), tabs with split
//! panes, settings page, command palette, host dialog, tool panels, status bar.
mod actions;
mod config;
mod host_dialog;
mod palette;
mod panels;
mod settings_view;
mod termview;
mod theme;
mod ui;

use ssh_term_proto::i18n::{t, tf};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use gpui::{
    div, prelude::*, px, size, AnyElement, App, Bounds, Context, Entity, FontWeight, Hsla, KeyDownEvent, SharedString,
    Window, WindowAppearance, WindowBounds, WindowOptions,
};
use gpui_component::input::{Input, InputEvent, InputState};
use ssh_term_proto::agent::{self, AgentState, Manifest};
use ssh_term_proto::audit::AuditLog;
use ssh_term_proto::hosts::{self, HostEntry};
use ssh_term_proto::layout::{auto_direction, Dir, Node};
use ssh_term_proto::policy::Mode;
use ssh_term_proto::ssh::{self, HostKeyDecision, HostKeyPrompt, SshTarget};
use ssh_term_proto::suggest::History;
use ssh_term_proto::{local, term::Size};

use actions::Action;
use host_dialog::{HostDialog, HostEvent};
use palette::{Item, Palette, PaletteEvent};
use settings_view::{Cat, SettingsEvent, SettingsView};
use termview::{Spec, TermView};
use theme::{c, cur, Appearance, Theme};
use ui::{icon, IconName};

pub fn font_size() -> f32 { config::get().font.size.clamp(6.0, 48.0) }
pub fn line_h() -> f32 { let f = config::get(); (f.font.size.clamp(6.0, 48.0) * f.font.line_height.clamp(0.8, 3.0)).round() }

/// One shared audit writer (the hash chain needs a single tail cache per process).
pub(crate) fn audit_log() -> Arc<AuditLog> {
    static A: std::sync::OnceLock<Arc<AuditLog>> = std::sync::OnceLock::new();
    A.get_or_init(|| Arc::new(AuditLog::new(config::get().audit_path()))).clone()
}

/// API-key storage: OS keychain first, encrypted 0600 file under data_dir() as fallback.
pub(crate) fn secret_store() -> ssh_term_proto::secrets::SecretStore { ssh_term_proto::secrets::SecretStore::new(data_dir()) }

pub(crate) fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"))
        .join("agentshell")
}

// ---------------------------------------------------------------- app shell
struct Tab { layout: Node<Entity<TermView>>, focused: Entity<TermView> }

pub(crate) enum Overlay { None, Settings(Entity<SettingsView>), Palette(Entity<Palette>), Host(Entity<HostDialog>), Audit, Sftp, Port, Ai }

pub(crate) struct Root {
    rt: Arc<tokio::runtime::Runtime>,
    default_target: Option<SshTarget>,
    tabs: Vec<Tab>,
    active: usize,
    themes: Vec<Arc<Theme>>,
    histories: HashMap<String, Arc<Mutex<History>>>,
    manifests: Arc<Vec<Manifest>>,
    hk_tx: flume::Sender<HostKeyPrompt>,
    hk_pending: Vec<HostKeyPrompt>,
    approvals: Vec<ssh_term_proto::policy_server::Approval>,
    pub(crate) policy_port: Option<u16>,
    overlay: Overlay,
    host_search: Entity<InputState>,
    lang_seen: u8,
    collapsed: HashSet<String>,
    sidebar_visible: bool,
    audit: Arc<AuditLog>,
    pub(crate) audit_verify: Option<Result<usize, (usize, String)>>,
    settings_dirty: Vec<String>,
    toast: Option<(String, u64)>,
    toast_seq: u64,
    _subs: Vec<gpui::Subscription>,
}

fn env_target(known_hosts: &std::path::Path) -> Option<SshTarget> {
    std::env::var("SSH_HOST").ok()?;
    let e = HostEntry {
        name: "env".into(),
        host: std::env::var("SSH_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("SSH_PORT").ok().and_then(|p| p.parse().ok()),
        user: std::env::var("SSH_USER").ok(),
        identity_file: std::env::var("SSH_KEY").ok(),
        source: "env",
        ..Default::default()
    };
    Some(hosts::to_target(&e, known_hosts))
}

impl Root {
    fn log(&self, kind: &str, detail: impl AsRef<str>) { let _ = self.audit.append(kind, "user", detail.as_ref()); }

    pub(crate) fn toast(&mut self, msg: String, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((msg, seq));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            let _ = this.update(cx, |r, cx| { if r.toast.as_ref().is_some_and(|t| t.1 == seq) { r.toast = None; cx.notify(); } });
        }).detach();
    }

    fn history_for(&mut self, key: &str) -> Arc<Mutex<History>> {
        if !config::get().suggest.local_history { return Arc::new(Mutex::new(History::new())); }
        self.histories.entry(key.to_string()).or_insert_with(|| {
            let fname: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || "@.-_".contains(c) { c } else { '_' }).collect();
            Arc::new(Mutex::new(History::open(data_dir().join("history").join(format!("{fname}.txt")))))
        }).clone()
    }
    fn open_pane(&mut self, spec: Spec, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let init = Size { cols: 80, rows: 24 };
        let h = self.history_for(&spec.history_key());
        let m = self.manifests.clone();
        match spec {
            Spec::Local => {
                let shell = config::get().terminal.shell.clone();
                let sh = if shell.is_empty() { std::env::var("SHELL").unwrap_or_else(|_| "bash".into()) } else { shell.clone() };
                let title = tf("main.local_title", &[("shell", &sh.rsplit('/').next().unwrap_or("sh"))]);
                let sess = local::spawn_local(init, (!shell.is_empty()).then_some(shell.as_str()));
                cx.new(|cx| TermView::new(&title, Spec::Local, sess, h, m, window, cx))
            }
            Spec::Ssh(t) => {
                let title = t.label();
                self.log("ssh.connect", format!("{} (known_hosts: {})", t.label(), t.known_hosts.display()));
                let s = ssh::spawn_ssh(self.rt.handle(), t.clone(), init, self.hk_tx.clone());
                cx.new(|cx| TermView::new(&title, Spec::Ssh(t), Ok(s), h, m, window, cx))
            }
        }
    }
    fn new_tab(&mut self, spec: Spec, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.open_pane(spec, window, cx);
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(Tab { layout: Node::Leaf(pane.clone()), focused: pane });
        self.active = self.tabs.len() - 1;
        self.overlay = Overlay::None;
        window.focus(&fh, cx);
        cx.notify();
    }
    fn focused_pane(&self, window: &Window, cx: &App) -> Option<Entity<TermView>> {
        let tab = self.tabs.get(self.active)?;
        tab.layout.leaves().into_iter().find(|p| p.read(cx).focus.is_focused(window)).or_else(|| Some(tab.focused.clone()))
    }
    pub(crate) fn focused_ssh_label(&self, cx: &App) -> Option<String> {
        let tab = self.tabs.get(self.active)?;
        match &tab.focused.read(cx).spec { Spec::Ssh(t) => Some(t.label()), _ => None }
    }
    fn split(&mut self, dir: Option<Dir>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.focused_pane(window, cx) else { return self.new_tab(Spec::Local, window, cx) };
        let (spec, sz) = { let v = target.read(cx); (v.spec.clone(), v.session.as_ref().map(|s| s.size())) };
        let dir = dir.or_else(|| sz.and_then(|s| auto_direction(s.rows, s.cols))).unwrap_or(Dir::Vertical);
        let pane = self.open_pane(spec, window, cx);
        let fh = pane.read(cx).focus.clone();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout.split(&target, pane.clone(), dir);
            tab.focused = pane;
        }
        window.focus(&fh, cx);
        cx.notify();
    }
    fn close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.focused_pane(window, cx) else { return };
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if !tab.layout.remove(&target) {
            self.tabs.remove(self.active);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        } else {
            tab.focused = tab.layout.leaves()[0].clone();
        }
        self.focus_active(window, cx);
        cx.notify();
    }
    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) { let fh = tab.focused.read(cx).focus.clone(); window.focus(&fh, cx); }
    }

    // ---- theme
    fn theme_index(&self, window: &Window) -> usize {
        let s = config::get();
        let by_name = |n: &str| self.themes.iter().position(|t| t.name.eq_ignore_ascii_case(n));
        if s.appearance.theme == "system" {
            let dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
            let want = if dark { &s.appearance.dark_theme } else { &s.appearance.light_theme };
            by_name(want).or_else(|| self.themes.iter().position(|t| t.appearance == if dark { Appearance::Dark } else { Appearance::Light })).unwrap_or(0)
        } else {
            by_name(&s.appearance.theme).unwrap_or(0)
        }
    }
    fn apply_theme(&self, window: &Window, cx: &mut App) {
        let th = self.themes[self.theme_index(window)].clone();
        theme::set_current(th.clone());
        sync_component_theme(&th, cx);
    }
    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cur_idx = self.themes.iter().position(|t| t.name == cur().name).unwrap_or(0);
        let next = self.themes[(cur_idx + 1) % self.themes.len()].name.clone();
        self.report(config::update(|s| s.appearance.theme = next), cx);
        self.apply_theme(window, cx);
        self.refresh_all(cx);
    }
    fn refresh_all(&self, cx: &mut Context<Self>) { cx.refresh_windows(); cx.notify(); }
    fn report(&mut self, err: Option<String>, cx: &mut Context<Self>) { if let Some(e) = err { self.toast(e, cx); } }

    fn set_mode(&mut self, m: Mode, cx: &mut Context<Self>) {
        let old = config::get().permissions.mode;
        if old == m { return; }
        let err = config::update(|s| s.permissions.mode = m);
        self.log("permission.mode", format!("{} → {}", old.label(), m.label()));
        self.report(err, cx);
        self.toast(tf("main.mode_toast", &[("mode", &m.label()), ("desc", &m.desc())]), cx);
    }

    /// Desktop notification (notify.desktop + notify.agent_blocked). Best effort: without a
    /// notification daemon the call fails silently (logged to stderr).
    pub(crate) fn notify_desktop(&self, body: &str, _cx: &mut Context<Self>) {
        let n = &config::get().notify;
        if !(n.desktop && n.agent_blocked) { return; }
        let body = body.to_string();
        std::thread::spawn(move || {
            if let Err(e) = notify_rust::Notification::new().appname("AgentShell").summary("AgentShell").body(&body).show() { eprintln!("notification: {e}"); }
        });
    }

    fn answer_approval(&mut self, allow: bool, cx: &mut Context<Self>) {
        if !self.approvals.is_empty() {
            let a = self.approvals.remove(0);
            let _ = a.reply.send(allow);
        }
        cx.notify();
    }

    fn answer_hostkey(&mut self, d: HostKeyDecision, cx: &mut Context<Self>) {
        if !self.hk_pending.is_empty() {
            let p = self.hk_pending.remove(0);
            let what = match d { HostKeyDecision::AcceptAndSave => "hostkey.accept_save", HostKeyDecision::AcceptOnce => "hostkey.accept_once", HostKeyDecision::Reject => "hostkey.reject" };
            self.log(what, format!("{}:{} {} {}", p.host, p.port, p.algorithm, p.fingerprint));
            let _ = p.reply.send(d);
        }
        cx.notify();
    }

    // ---- overlays
    pub(crate) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::Settings(_)) && !self.settings_dirty.is_empty() {
            let mut paths = std::mem::take(&mut self.settings_dirty);
            paths.sort();
            paths.dedup();
            self.log("settings.change", paths.join(", "));
        }
        self.overlay = Overlay::None;
        self.focus_active(window, cx);
        cx.notify();
    }
    pub(crate) fn open_settings(&mut self, cat: Cat, window: &mut Window, cx: &mut Context<Self>) {
        let names = self.themes.iter().map(|t| (t.name.clone(), t.appearance == Appearance::Dark)).collect();
        let v = cx.new(|cx| SettingsView::new(cat, names, window, cx));
        self._subs.push(cx.subscribe_in(&v, window, |this, _, ev: &SettingsEvent, window, cx| match ev {
            SettingsEvent::Close => this.close_overlay(window, cx),
            SettingsEvent::OpenAudit => { this.close_overlay(window, cx); this.overlay = Overlay::Audit; }
            SettingsEvent::Changed(path) => {
                this.settings_dirty.push(path.clone());
                if path == "suggest.clear_history" { for h in this.histories.values() { h.lock().unwrap().clear(); } }
                this.apply_theme(window, cx);
                this.refresh_all(cx);
            }
        }));
        v.update(cx, |s, cx| s.focus_search(window, cx));
        self.overlay = Overlay::Settings(v);
        cx.notify();
    }
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, Overlay::Palette(_)) { return self.close_overlay(window, cx); }
        let hosts = self.host_targets().into_iter().map(|(e, t)| (e.name.clone(), t.label())).collect();
        let p = cx.new(|cx| Palette::new(hosts, window, cx));
        self._subs.push(cx.subscribe_in(&p, window, |this, _, ev: &PaletteEvent, window, cx| match ev {
            PaletteEvent::Close => this.close_overlay(window, cx),
            PaletteEvent::Run(Item::Act(a)) => { this.close_overlay(window, cx); this.run(*a, window, cx); }
            PaletteEvent::Run(Item::Host(i, _, _)) => {
                let t = this.host_targets().get(*i).map(|(_, t)| t.clone());
                this.close_overlay(window, cx);
                if let Some(t) = t { this.new_tab(Spec::Ssh(t), window, cx); }
            }
        }));
        self.overlay = Overlay::Palette(p);
        cx.notify();
    }
    fn open_host_dialog(&mut self, existing: Option<HostEntry>, window: &mut Window, cx: &mut Context<Self>) {
        let d = cx.new(|cx| HostDialog::new(existing, window, cx));
        self._subs.push(cx.subscribe_in(&d, window, |this, _, ev: &HostEvent, window, cx| {
            match ev {
                HostEvent::Saved(n) => { this.log("host.save", n); this.toast(tf("main.host_saved", &[("n", &n)]), cx); }
                HostEvent::Deleted(n) => { this.log("host.delete", n); this.toast(tf("main.host_deleted", &[("n", &n)]), cx); }
                HostEvent::Close => {}
            }
            this.close_overlay(window, cx);
        }));
        self.overlay = Overlay::Host(d);
        cx.notify();
    }

    fn run(&mut self, a: Action, window: &mut Window, cx: &mut Context<Self>) {
        match a {
            Action::CommandPalette => self.open_palette(window, cx),
            Action::OpenSettings => self.open_settings(Cat::General, window, cx),
            Action::NewLocalTab => self.new_tab(Spec::Local, window, cx),
            Action::NewHost => self.open_host_dialog(None, window, cx),
            Action::SplitAuto => self.split(None, window, cx),
            Action::SplitRight => self.split(Some(Dir::Vertical), window, cx),
            Action::SplitDown => self.split(Some(Dir::Horizontal), window, cx),
            Action::ClosePane => self.close_pane(window, cx),
            Action::NextTab | Action::PrevTab => {
                let n = self.tabs.len();
                if n > 0 { self.active = if a == Action::NextTab { (self.active + 1) % n } else { (self.active + n - 1) % n }; self.focus_active(window, cx); }
            }
            Action::CycleTheme => self.cycle_theme(window, cx),
            Action::ThemeSystem => { self.report(config::update(|s| s.appearance.theme = "system".into()), cx); self.apply_theme(window, cx); self.refresh_all(cx); }
            Action::CyclePermissionMode => { let m = config::get().permissions.mode.next(); self.set_mode(m, cx); }
            Action::ModeAsk => self.set_mode(Mode::Ask, cx),
            Action::ModePlan => self.set_mode(Mode::Plan, cx),
            Action::ModeAuto => self.set_mode(Mode::Auto, cx),
            Action::OpenSftp => self.overlay = Overlay::Sftp,
            Action::OpenPortForward => self.overlay = Overlay::Port,
            Action::OpenAi => self.overlay = Overlay::Ai,
            Action::AiCommand => {
                if let Some(v) = self.focused_pane(window, cx) { v.update(cx, |v, cx| v.ai_from_line(cx)); }
                self.focus_active(window, cx);
            }
            Action::OpenAudit => { self.audit_verify = None; self.overlay = Overlay::Audit; }
            Action::OpenAbout => self.open_settings(Cat::About, window, cx),
            Action::ClearHistory => {
                for h in self.histories.values() { h.lock().unwrap().clear(); }
                let dir = data_dir().join("history");
                if let Ok(d) = std::fs::read_dir(&dir) { for e in d.flatten() { let _ = std::fs::write(e.path(), ""); } }
                self.toast(t("ui.command_suggestion_history_cleared").into(), cx);
            }
            Action::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Action::SettingsPermissions => self.open_settings(Cat::Permissions, window, cx),
            Action::SettingsAppearance => self.open_settings(Cat::Appearance, window, cx),
            Action::SettingsTerminal => self.open_settings(Cat::Terminal, window, cx),
            Action::SettingsSsh => self.open_settings(Cat::Ssh, window, cx),
            Action::SettingsKeys => self.open_settings(Cat::Keys, window, cx),
        }
        cx.notify();
    }

    fn host_targets(&self) -> Vec<(HostEntry, SshTarget)> {
        let s = config::get();
        let kh = s.known_hosts_path();
        s.all_hosts().into_iter().map(|h| { let t = hosts::to_target(&h, &kh); (h, t) }).collect()
    }

    // ---------------------------------------------------------------- sidebar
    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let open: Vec<(String, bool)> = self.tabs.iter().flat_map(|t| t.layout.leaves()).map(|p| {
            let v = p.read(cx);
            let label = match &v.spec { Spec::Local => "local".to_string(), Spec::Ssh(t) => t.label() };
            (label, v.session.as_ref().is_some_and(|s| s.connected.load(Ordering::Acquire)))
        }).collect();
        let online = |label: &str| open.iter().any(|(l, on)| l == label && *on);
        let q = self.host_search.read(cx).value().to_string().to_lowercase();

        // groups: (group name, entries)
        struct E { name: String, detail: String, spec: Spec, letter: &'static str, prod: bool, edit: Option<HostEntry>, label: String }
        let mut groups: Vec<(String, Vec<E>)> = vec![];
        let mut push = |g: &str, e: E| {
            if !q.is_empty() && !format!("{} {}", e.name, e.detail).to_lowercase().contains(&q) { return; }
            match groups.iter_mut().find(|(n, _)| n == g) { Some((_, v)) => v.push(e), None => groups.push((g.to_string(), vec![e])) }
        };
        let sh = { let s = config::get().terminal.shell.clone(); if s.is_empty() { std::env::var("SHELL").unwrap_or_else(|_| "bash".into()) } else { s } };
        push(t("ui.quick_connect"), E { name: t("ui.local_terminal").into(), detail: format!("{} · portable-pty", sh.rsplit('/').next().unwrap_or("sh")), spec: Spec::Local, letter: "L", prod: false, edit: None, label: "local".into() });
        if let Some(dt) = self.default_target.clone() {
            push(t("ui.quick_connect"), E { name: dt.label(), detail: t("ui.ssh_host_environment_variable").into(), label: dt.label(), spec: Spec::Ssh(dt), letter: "S", prod: false, edit: None });
        }
        for (h, t) in self.host_targets() {
            let g = if h.source == "config" { h.group.clone().unwrap_or_else(|| ssh_term_proto::i18n::t("ui.ungrouped").into()) } else { "~/.ssh/config".into() };
            let edit = (h.source == "config").then(|| h.clone());
            push(&g, E { name: h.name.clone(), detail: t.label(), label: t.label(), spec: Spec::Ssh(t), letter: "S", prod: h.production, edit });
        }

        let header = div().h(px(48.)).px_4().flex().items_center().gap_2()
            .child(div().size(px(22.)).rounded_md().bg(c(th.accent).opacity(0.18)).flex().items_center().justify_center().child(icon(IconName::SquareTerminal, 13., c(th.accent))))
            .child(div().text_color(c(th.text)).font_weight(FontWeight::SEMIBOLD).text_size(px(13.5)).child("AgentShell"))
            .child(div().flex_1())
            .child(div().id("sb-collapse").size(px(24.)).rounded_md().flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, _w, cx| { this.sidebar_visible = false; cx.notify(); })).child(icon(IconName::Layers, 14., c(th.overlay1))));
        let search = div().px_3().pb_2().flex().items_center().gap_2()
            .child(div().flex_1().child(Input::new(&self.host_search).prefix(icon(IconName::Search, 13., c(th.overlay1))).cleanable(true)))
            .child(div().id("new-host").size(px(30.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                .bg(c(th.surface0)).border_1().border_color(c(th.surface1)).hover(|d| d.bg(c(cur().surface1)))
                .on_click(cx.listener(|this, _, w, cx| this.open_host_dialog(None, w, cx)))
                .child(icon(IconName::Plus, 15., c(th.text))));

        let mut list = div().id("host-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().pb_2();
        let mut idx = 0usize;
        for (gname, entries) in groups {
            let collapsed = self.collapsed.contains(&gname) && q.is_empty();
            let g2 = gname.clone();
            list = list.child(div().id(SharedString::from(format!("grp-{gname}"))).mx_2().mt_2().px_2().h(px(24.)).flex().items_center().gap_1().cursor_pointer().rounded_md()
                .hover(|d| d.bg(c(cur().surface0).opacity(0.5)))
                .on_click(cx.listener(move |this, _, _w, cx| { if !this.collapsed.remove(&g2) { this.collapsed.insert(g2.clone()); } cx.notify(); }))
                .child(icon(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }, 12., c(th.overlay0)))
                .child(div().text_size(px(10.5)).font_weight(FontWeight::MEDIUM).text_color(c(th.overlay0)).child(gname.clone()))
                .child(div().flex_1())
                .child(div().text_size(px(10.5)).text_color(c(th.overlay0)).child(entries.len().to_string())));
            if collapsed { continue; }
            for e in entries {
                let accent = if matches!(e.spec, Spec::Local) { c(th.info) } else if e.prod { c(th.error) } else { c(th.warning) };
                let dot = if online(&e.label) { c(th.success) } else { c(th.surface2) };
                let spec = e.spec.clone();
                let gid = SharedString::from(format!("host-row-{idx}"));
                let mut row = div().id(("host", idx)).group(gid.clone()).mx_2().px_2().py(px(5.)).rounded_lg().flex().items_center().gap_3().cursor_pointer()
                    .hover(|s| s.bg(c(cur().surface0)))
                    .on_click(cx.listener(move |this, _, w, cx| this.new_tab(spec.clone(), w, cx)))
                    .child(div().size(px(26.)).rounded_md().flex().items_center().justify_center().bg(accent.opacity(0.18)).text_color(accent).text_size(px(12.)).font_weight(FontWeight::BOLD).child(e.letter))
                    .child(div().flex_1().min_w_0().flex().flex_col().overflow_hidden()
                        .child(div().flex().items_center().gap_1().child(div().text_size(px(12.5)).text_color(c(th.text)).child(e.name.clone()))
                            .when(e.prod, |d| d.child(ui::tag(t("ui.prod"), c(th.error)))))
                        .child(div().text_size(px(10.5)).text_color(c(th.overlay0)).child(e.detail.clone())));
                if let Some(h) = e.edit.clone() {
                    row = row.child(div().id(("host-edit", idx)).size(px(22.)).rounded_md().flex().items_center().justify_center().invisible()
                        .group_hover(gid, |s| s.visible()).hover(|s| s.bg(c(cur().surface1)))
                        .on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.open_host_dialog(Some(h.clone()), w, cx); }))
                        .child(icon(IconName::Pencil, 12., c(th.overlay1))));
                }
                list = list.child(row.child(div().size(px(7.)).rounded_full().bg(dot)));
                idx += 1;
            }
        }
        if idx == 0 { list = list.child(div().px_4().py_3().text_size(px(11.5)).text_color(c(th.overlay0)).child(t("ui.no_matching_hosts"))); }

        // ---- function area (bottom-left)
        let items: [(Action, &str, IconName); 7] = [
            (Action::OpenSftp, t("ui.sftp_files"), IconName::FolderSync),
            (Action::OpenPortForward, t("ui.port_forwarding"), IconName::ArrowLeftRight),
            (Action::OpenAi, "AI / Agent", IconName::Bot),
            (Action::OpenAudit, t("ui.audit_log"), IconName::ScrollText),
            (Action::CommandPalette, t("ui.command_palette"), IconName::Command),
            (Action::OpenSettings, t("ui.settings"), IconName::Settings),
            (Action::OpenAbout, t("ui.help_about"), IconName::CircleQuestionMark),
        ];
        let n_agents = self.tabs.iter().flat_map(|t| t.layout.leaves()).filter(|p| p.read(cx).agent.is_some()).count();
        let mut tools = div().px_2().py_2().flex().flex_col().gap(px(1.)).border_t_1().border_color(c(th.surface0));
        for (i, (a, label, ic)) in items.into_iter().enumerate() {
            let b = a.binding();
            tools = tools.child(div().id(("tool", i)).h(px(30.)).px_2().rounded_md().flex().items_center().gap_3().cursor_pointer()
                .hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(move |this, _, w, cx| this.run(a, w, cx)))
                .child(icon(ic, 15., c(th.overlay1)))
                .child(div().flex_1().text_size(px(12.5)).text_color(c(th.subtext)).child(label))
                .when(a == Action::OpenAi && n_agents > 0, |d| d.child(ui::tag(format!("{n_agents}"), c(th.accent))))
                .when(!b.is_empty() && matches!(a, Action::CommandPalette | Action::OpenSettings), |d| d.child(ui::kbd(&b))));
        }

        div().w(px(248.)).h_full().flex().flex_col().bg(c(th.crust)).border_r_1().border_color(c(th.surface0))
            .child(header).child(search).child(list).child(tools)
    }
}

pub(crate) fn sync_component_theme(th: &Theme, cx: &mut App) {
    use gpui_component::{Theme as CTheme, ThemeMode};
    CTheme::change(if th.appearance == Appearance::Dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let th = th.clone();
    CTheme::update(cx, move |t| {
        t.colors.background = c(th.mantle);
        t.colors.foreground = c(th.text);
        t.colors.border = c(th.surface1);
        t.colors.input = c(th.surface1);
        t.colors.ring = c(th.accent);
        t.colors.caret = c(th.accent);
        t.colors.primary = c(th.accent);
        t.colors.primary_foreground = c(th.base);
        t.colors.muted_foreground = c(th.overlay0);
        t.colors.accent = c(th.surface0);
        t.colors.accent_foreground = c(th.text);
        t.colors.selection = c(th.accent).opacity(0.3);
        t.colors.switch = c(th.surface2);
        t.colors.switch_thumb = gpui::white();
        t.colors.popover = c(th.base);
        t.colors.popover_foreground = c(th.text);
        t.font_size = px(13.);
        t.font_family = SharedString::from("Inter");
        t.radius = px(6.);
        t.focus_ring = false;
        t.shadow = false;
    });
    cx.refresh_windows();
}

fn render_node(node: &Node<Entity<TermView>>, th: &Theme) -> AnyElement {
    match node {
        Node::Leaf(p) => div().flex_1().size_full().flex().overflow_hidden().child(p.clone()).into_any_element(),
        Node::Split { dir, a, b } => {
            let d = div().flex_1().size_full().flex().overflow_hidden();
            let (d, divider) = match dir {
                Dir::Vertical => (d.flex_row(), div().w(px(1.)).h_full().bg(c(th.surface1))),
                Dir::Horizontal => (d.flex_col(), div().h(px(1.)).w_full().bg(c(th.surface1))),
            };
            d.child(render_node(a, th)).child(divider).child(render_node(b, th)).into_any_element()
        }
    }
}

pub(crate) fn state_color(st: AgentState, th: &Theme) -> Hsla {
    match st { AgentState::Working => c(th.warning), AgentState::Blocked => c(th.error), AgentState::Idle => c(th.success), AgentState::Unknown => c(th.overlay1) }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // live language switch: refresh placeholders of long-lived inputs
        let lang = ssh_term_proto::i18n::current() as u8;
        if self.lang_seen != lang {
            self.lang_seen = lang;
            self.host_search.update(cx, |s, cx| s.set_placeholder(t("ui.search_hosts"), window, cx));
        }
        let th = cur();
        let s = config::get();

        let mut tabbar = div().h(px(40.)).px_2().flex().items_center().gap_1().bg(c(th.crust)).border_b_1().border_color(c(th.surface0));
        if !self.sidebar_visible {
            tabbar = tabbar.child(div().id("sb-show").size(px(28.)).rounded_lg().flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, _w, cx| { this.sidebar_visible = true; cx.notify(); })).child(icon(IconName::Layers, 14., c(th.overlay1))));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            let panes = t.layout.leaves();
            let title: SharedString = {
                let first = panes[0].read(cx).title.clone();
                if panes.len() > 1 { format!("{first} +{}", panes.len() - 1).into() } else { first }
            };
            let agent = panes.iter().filter_map(|p| p.read(cx).agent_label())
                .max_by_key(|(_, st)| match st { AgentState::Blocked => 3, AgentState::Working => 2, AgentState::Idle => 1, _ => 0 });
            let mut tabel = div().id(("tab", i)).h(px(30.)).pl_3().pr_1().rounded_lg().flex().items_center().gap_2().cursor_pointer()
                .bg(if active { c(th.base) } else { c(th.crust) })
                .border_1().border_color(if active { c(th.surface0) } else { c(th.crust) })
                .hover(|s| s.bg(c(cur().mantle)))
                .on_click(cx.listener(move |this, _, w, cx| { this.active = i; this.focus_active(w, cx); cx.notify(); }))
                .child(div().size(px(6.)).rounded_full().bg(if active { c(th.accent) } else { c(th.surface2) }))
                .child(div().text_size(px(12.)).text_color(if active { c(th.text) } else { c(th.overlay1) }).child(title));
            if let Some((name, st)) = agent {
                let col = state_color(st, &th);
                tabel = tabel.child(div().px(px(6.)).rounded_md().bg(col.opacity(0.18)).text_color(col).text_size(px(11.)).child(format!("● {name} {}", st.label())));
            }
            tabel = tabel.child(div().id(("close", i)).size(px(20.)).rounded_md().flex().items_center().justify_center()
                .hover(|s| s.bg(c(cur().surface1)))
                .on_click(cx.listener(move |this, _, w, cx| {
                    cx.stop_propagation();
                    this.tabs.remove(i);
                    this.active = this.active.min(this.tabs.len().saturating_sub(1));
                    this.focus_active(w, cx);
                    cx.notify();
                }))
                .child(icon(IconName::X, 12., c(th.overlay0))));
            tabbar = tabbar.child(tabel);
        }
        let tb_btn = |id: &'static str, ic: IconName, label: &'static str| div().id(id).px_2().h(px(26.)).rounded_md().flex().items_center().gap_1().cursor_pointer()
            .text_size(px(11.5)).text_color(c(th.overlay1)).hover(|s| s.bg(c(cur().surface0))).child(icon(ic, 13., c(th.overlay1))).child(label);
        tabbar = tabbar.child(
            div().id("new-tab").size(px(28.)).rounded_lg().flex().items_center().justify_center().cursor_pointer()
                .hover(|s| s.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, w, cx| this.new_tab(Spec::Local, w, cx)))
                .child(icon(IconName::Plus, 15., c(th.overlay1))),
        ).child(div().flex_1())
        .child(tb_btn("split-right", IconName::LayoutGrid, t("ui.split_left_right")).on_click(cx.listener(|this, _, w, cx| this.split(Some(Dir::Vertical), w, cx))))
        .child(tb_btn("split-down", IconName::LayoutGrid, t("ui.split_top_bottom")).on_click(cx.listener(|this, _, w, cx| this.split(Some(Dir::Horizontal), w, cx))))
        .child(tb_btn("close-pane", IconName::X, t("ui.close_pane")).on_click(cx.listener(|this, _, w, cx| this.close_pane(w, cx))));

        let panes = match self.tabs.get(self.active) {
            Some(tab) => div().flex_1().flex().overflow_hidden().child(render_node(&tab.layout, &th)),
            None => div().flex_1().flex().flex_col().items_center().justify_center().gap_3().bg(c(th.base))
                .child(icon(IconName::SquareTerminal, 36., c(th.overlay0)))
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(t("ui.no_open_terminals")))
                .child(div().flex().gap_2()
                    .child(ui::button("empty-local", t("ui.open_local_terminal"), 1).on_click(cx.listener(|this, _, w, cx| this.new_tab(Spec::Local, w, cx))))
                    .child(ui::button("empty-palette", t("ui.command_palette"), 0).on_click(cx.listener(|this, _, w, cx| this.open_palette(w, cx))))),
        };

        let (status, scolor, dims, title, hist) = match self.focused_pane(window, cx) {
            Some(p) => {
                let v = p.read(cx);
                let (st, col) = v.status(&th);
                let d = v.session.as_ref().map(|s| { let z = s.size(); format!("{}×{}", z.cols, z.rows) }).unwrap_or_default();
                let n = v.history.lock().unwrap().len();
                (st, col, d, v.title.clone(), tf("main.history_n", &[("n", &n)]))
            }
            None => ("—", c(th.overlay0), String::new(), SharedString::from(""), String::new()),
        };
        let theme_label = if s.appearance.theme == "system" { tf("main.theme_system", &[("name", &th.name)]) } else { th.name.clone() };
        let mode = s.permissions.mode;
        let mode_col = match mode { Mode::Ask => c(th.accent), Mode::Plan => c(th.info), Mode::Auto => c(th.warning) };
        let sb_btn = |id: &'static str| div().id(id).px_2().h(px(20.)).rounded_md().flex().items_center().gap_1().cursor_pointer().hover(|s| s.bg(c(cur().surface0)));
        let statusbar = div().h(px(26.)).px_3().flex().items_center().gap_3().bg(c(th.crust)).border_t_1().border_color(c(th.surface0))
            .text_size(px(11.)).text_color(c(th.subtext))
            .child(div().flex().items_center().gap_2().child(div().size(px(7.)).rounded_full().bg(scolor)).child(status))
            .child(title)
            .child(div().flex_1())
            .child(hist)
            .child(dims)
            .child("UTF-8")
            .child(sb_btn("theme-btn").on_click(cx.listener(|this, _, w, cx| this.cycle_theme(w, cx))).child(icon(IconName::Palette, 12., c(th.overlay1))).child(theme_label))
            .child(sb_btn("mode-btn").bg(mode_col.opacity(0.16)).text_color(mode_col).font_weight(FontWeight::SEMIBOLD)
                .on_click(cx.listener(|this, _, w, cx| this.run(Action::CyclePermissionMode, w, cx)))
                .child(icon(IconName::Shield, 12., mode_col)).child(tf("main.perm_status", &[("mode", &mode.label())])));

        let mut root = div().size_full().relative().flex().flex_row().bg(c(th.crust)).font_family(SharedString::from("Inter")).text_color(c(th.text))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                if ev.keystroke.key == "escape" && !matches!(this.overlay, Overlay::None | Overlay::Palette(_)) {
                    this.close_overlay(w, cx);
                    cx.stop_propagation();
                    return;
                }
                let ks = actions::keystroke_str(&ev.keystroke);
                if let Some(a) = actions::find(&ks) { this.run(a, w, cx); cx.stop_propagation(); }
            }));
        if self.sidebar_visible { root = root.child(self.sidebar(cx)); }
        root = root.child(div().flex_1().h_full().flex().flex_col().child(tabbar).child(panes).child(statusbar));

        let overlay: Option<AnyElement> = match &self.overlay {
            Overlay::None => None,
            Overlay::Settings(v) => Some(ui::modal(v.clone()).into_any_element()),
            Overlay::Palette(p) => Some(div().absolute().top_0().left_0().size_full().child(p.clone()).into_any_element()),
            Overlay::Host(d) => Some(div().absolute().top_0().left_0().size_full().child(d.clone()).into_any_element()),
            Overlay::Audit => Some(self.audit_panel(cx)),
            Overlay::Sftp => Some(self.sftp_panel(cx)),
            Overlay::Port => Some(self.port_panel(cx)),
            Overlay::Ai => Some(self.ai_panel(cx)),
        };
        if let Some(o) = overlay { root = root.child(o); }

        // host key confirmation (first connection / TOFU)
        if let Some(p) = self.hk_pending.first() {
            root = root.child(ui::modal(ui::card(560.).p_5().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::ShieldCheck, 16., c(th.warning)))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.first_connection_verify_host_key"))))
                .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child(tf("main.not_in_kh", &[("host", &p.host), ("port", &p.port), ("path", &s.known_hosts_path().display())])))
                .child(div().p_3().rounded_md().bg(c(th.mantle)).flex().flex_col().gap_1().text_size(px(12.)).text_color(c(th.text))
                    .child(tf("main.algorithm", &[("a", &p.algorithm)]))
                    .child(div().font_family(SharedString::from(s.font.family.clone())).child(tf("main.fingerprint", &[("fp", &p.fingerprint)]))))
                .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(t("ui.verify_the_fingerprint_through_another")))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("hk-reject", t("ui.deny"), 0).on_click(cx.listener(|this, _, _w, cx| this.answer_hostkey(HostKeyDecision::Reject, cx))))
                    .child(ui::button("hk-once", t("ui.this_time_only"), 0).on_click(cx.listener(|this, _, _w, cx| this.answer_hostkey(HostKeyDecision::AcceptOnce, cx))))
                    .child(ui::button("hk-save", t("ui.accept_and_save"), 1).on_click(cx.listener(|this, _, _w, cx| this.answer_hostkey(HostKeyDecision::AcceptAndSave, cx)))))));
        }
        // approval card for agent hook requests
        if self.hk_pending.is_empty() { if let Some(a) = self.approvals.first() {
            let left = a.deadline.saturating_duration_since(std::time::Instant::now()).as_secs();
            let risk_col = if a.risk >= ssh_term_proto::policy::Risk::R3 { th.error } else { th.warning };
            root = root.child(ui::modal(ui::card(600.).p_5().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::TriangleAlert, 16., c(risk_col)))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.approval_title")))
                    .child(div().flex_1())
                    .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(tf("main.approval_left", &[("n", &left), ("q", &self.approvals.len())]))))
                .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child(tf("main.approval_who", &[("agent", &a.principal), ("host", &a.host), ("tool", &a.req.tool), ("risk", &a.risk.label())])))
                .child(div().p_3().rounded_md().bg(c(th.mantle)).text_size(px(12.5)).text_color(c(th.text)).font_family(SharedString::from(s.font.family.clone())).child(a.req.command.clone()))
                .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(tf("main.approval_why", &[("why", &a.reason), ("cwd", &a.req.cwd)])))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("ap-deny", t("ui.deny"), 0).on_click(cx.listener(|this, _, _w, cx| this.answer_approval(false, cx))))
                    .child(ui::button("ap-allow", t("ui.allow_once"), 1).on_click(cx.listener(|this, _, _w, cx| this.answer_approval(true, cx)))))));
        } }
        if let Some((msg, _)) = &self.toast {
            root = root.child(div().absolute().bottom(px(40.)).right(px(16.)).max_w(px(460.)).px_4().py_2().rounded_lg().bg(c(th.base)).border_1().border_color(c(th.surface1)).shadow_lg()
                .flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text)).child(icon(IconName::Info, 14., c(th.accent))).child(msg.clone()));
        }
        root
    }
}

fn main() {
    let (settings, load_err) = config::Settings::load();
    if let Some(e) = &load_err { eprintln!("{e}"); }
    config::install(settings);
    if std::env::var("PERF_LOG").is_ok() {
        std::thread::spawn(|| {
            use termview::{CURSOR_PAINTS, GRID_PAINTS, ROWS_BUILT};
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let (g, r, c) = (GRID_PAINTS.swap(0, Ordering::Relaxed), ROWS_BUILT.swap(0, Ordering::Relaxed), CURSOR_PAINTS.swap(0, Ordering::Relaxed));
                if g + r + c > 0 { eprintln!("[perf] grid_paints/s={g} rows_built/s={r} cursor_paints/s={c}"); }
            }
        });
    }
    let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().unwrap());
    let themes = theme::load_all();
    let default_target = if std::env::var("NO_SSH").is_ok() { None } else { env_target(&config::get().known_hosts_path()) };
    gpui_platform::application().with_assets(gpui_kit_assets::AllAssets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1440.), px(860.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            move |window, cx| {
                let view = cx.new(|cx| {
                    let (hk_tx, hk_rx) = flume::unbounded::<HostKeyPrompt>();
                    let host_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.search_hosts")));
                    let audit = audit_log();
                    let mut root = Root {
                        rt, default_target, tabs: vec![], active: 0, themes, histories: HashMap::new(),
                        manifests: Arc::new(agent::bundled()), hk_tx, hk_pending: vec![], approvals: vec![], policy_port: None, overlay: Overlay::None,
                        host_search: host_search.clone(), lang_seen: ssh_term_proto::i18n::current() as u8, collapsed: HashSet::new(), sidebar_visible: true, audit, audit_verify: None,
                        settings_dirty: vec![], toast: None, toast_seq: 0, _subs: vec![],
                    };
                    root.apply_theme(window, cx);
                    root._subs.push(cx.observe_window_appearance(window, |this: &mut Root, w, cx| { this.apply_theme(w, cx); this.refresh_all(cx); }));
                    root._subs.push(cx.subscribe_in(&host_search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
                    cx.spawn(async move |this, cx| {
                        while let Ok(p) = hk_rx.recv_async().await {
                            if this.update(cx, |r, cx| { r.hk_pending.push(p); cx.notify(); }).is_err() { break; }
                        }
                    }).detach();
                    // local policy service for external agents' hooks
                    let (ap_tx, ap_rx) = flume::unbounded::<ssh_term_proto::policy_server::Approval>();
                    let ctx: ssh_term_proto::policy_server::CtxFn = Arc::new(|| config::get().policy_ctx());
                    let bind = format!("127.0.0.1:{}", config::get().agents.policy_port);
                    match ssh_term_proto::policy_server::start(&bind, None, ctx, Some(ap_tx), root.audit.clone(), Some(ssh_term_proto::policy_server::endpoint_path(&data_dir()))) {
                        Ok(srv) => { root.policy_port = Some(srv.port); root.log("policy.server", format!("listening on 127.0.0.1:{}", srv.port)); }
                        Err(e) => root.toast(tf("main.policy_server_failed", &[("e", &e)]), cx),
                    }
                    cx.spawn(async move |this, cx| {
                        while let Ok(a) = ap_rx.recv_async().await {
                            if this.update(cx, |r, cx| { r.approvals.push(a); r.notify_desktop(&t("ui.approval_title"), cx); cx.notify(); }).is_err() { break; }
                        }
                    }).detach();
                    cx.spawn(async move |this, cx| loop {
                        cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                        if this.update(cx, |r, cx| if !r.approvals.is_empty() {
                            let now = std::time::Instant::now();
                            r.approvals.retain(|a| a.deadline > now && !a.reply.is_disconnected());
                            cx.notify();
                        }).is_err() { break; }
                    }).detach();
                    if let Some(e) = load_err.clone() { root.toast(e, cx); }
                    if config::get().general.startup != "blank" {
                        let l = root.open_pane(Spec::Local, window, cx);
                        let mut layout = Node::Leaf(l.clone());
                        if let Some(t) = root.default_target.clone() {
                            let r = root.open_pane(Spec::Ssh(t), window, cx);
                            layout.split(&l, r, Dir::Vertical);
                        }
                        let fh = l.read(cx).focus.clone();
                        root.tabs.push(Tab { layout, focused: l });
                        window.focus(&fh, cx);
                    }
                    root
                });
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        ).unwrap();
        cx.activate(true);
    });
}
