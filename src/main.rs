//! AgentShell prototype v5 UI: sidebar (host search / groups / function area), tabs with split
//! panes, settings page, command palette, host dialog, tool panels, status bar.
mod actions;
mod config;
mod host_dialog;
mod palette;
mod panels;
mod settings_view;
mod sftp_view;
mod port_view;
mod host_tools_view;
mod chat_view;
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
pub(crate) fn secret_store() -> ssh_term_proto::secrets::SecretStore {
    // credentials.use_keychain = false → encrypted local file only
    if config::get().credentials.use_keychain { ssh_term_proto::secrets::SecretStore::new(data_dir()) } else { ssh_term_proto::secrets::SecretStore::file_only(data_dir()) }
}

pub(crate) fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"))
        .join("agentshell")
}

// ---------------------------------------------------------------- app shell
#[derive(Default)]
struct PaneInit { cwd: Option<String>, daemon_id: Option<String>, resume: Option<String> }

#[derive(Clone)]
struct DividerDrag { tab: usize, path: Vec<bool> }
impl Render for DividerDrag { fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement { div() } }

/// Dragging a tab onto an edge of a pane in the active tab moves it there as a split (PRD US-03).
#[derive(Clone)]
struct TabDrag { idx: usize, title: SharedString }
impl Render for TabDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        div().px_3().py_1().rounded_lg().bg(c(th.base)).border_1().border_color(c(th.accent)).text_size(px(12.)).text_color(c(th.text)).child(self.title.clone())
    }
}
/// Dragging a config host onto a group header moves it into that group.
#[derive(Clone)]
struct HostDrag { name: String }
impl Render for HostDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        div().px_3().py_1().rounded_lg().bg(c(th.base)).border_1().border_color(c(th.accent)).text_size(px(12.)).text_color(c(th.text)).child(self.name.clone())
    }
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Edge { Left, Right, Top, Bottom }

#[derive(Clone)]
enum Modal { Broadcast(Vec<String>), Snapshots, Recordings, QuickStart { dir: String } }

struct Tab { layout: Node<Entity<TermView>>, focused: Entity<TermView> }

pub(crate) enum Overlay { None, Settings(Entity<SettingsView>), Palette(Entity<Palette>), Host(Entity<HostDialog>), Audit, Sftp, Port, Ai, Chat, HostTools(Entity<host_tools_view::HostToolsView>) }

pub(crate) struct Root {
    rt: Arc<tokio::runtime::Runtime>,
    default_target: Option<SshTarget>,
    tabs: Vec<Tab>,
    active: usize,
    themes: Vec<Arc<Theme>>,
    histories: HashMap<String, Arc<Mutex<History>>>,
    manifests: Arc<Vec<Manifest>>,
    prompts: ssh::Prompts,
    auth_pending: Vec<ssh::AuthPrompt>,
    auth_input: Option<Entity<InputState>>,
    auth_save: bool,
    hk_pending: Vec<HostKeyPrompt>,
    approvals: Vec<ssh_term_proto::policy_server::Approval>,
    /// 'deny and tell the agent why' input (shown after clicking that button)
    deny_reason: Option<Entity<InputState>>,
    pub(crate) policy_port: Option<u16>,
    overlay: Overlay,
    host_search: Entity<InputState>,
    lang_seen: u8,
    bg_seen: Option<gpui::WindowBackgroundAppearance>,
    ui_size_seen: f32,
    collapsed: HashSet<String>,
    /// OS id per SSH target label from a one-shot probe (sidebar badge).
    host_os: Arc<Mutex<HashMap<String, String>>>,
    os_probing: HashSet<String>,
    sidebar_visible: bool,
    audit: Arc<AuditLog>,
    pub(crate) audit_verify: Option<Result<usize, (usize, String)>>,
    pub(crate) audit_filter: ssh_term_proto::audit::Filter,
    settings_dirty: Vec<String>,
    toast: Option<(String, u64)>,
    toast_seq: u64,
    pub(crate) sftp_views: HashMap<String, Entity<sftp_view::SftpView>>,
    pub(crate) port_view: Option<Entity<port_view::PortView>>,
    pub(crate) chat_view: Option<Entity<chat_view::ChatView>>,
    quit_confirm: bool,
    /// Generic confirmation / picker modal (broadcast prod warning, snapshots, recordings, quick start).
    modal: Option<Modal>,
    quit_ok: bool,
    pub(crate) sftp_current: Option<Entity<sftp_view::SftpView>>,
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
    let s = config::get();
    Some(hosts::resolve(&e, known_hosts, &s.all_hosts(), &s.ssh_opts()))
}

/// Record the user's acknowledgement of the first-send AI notice (persisted, audited).
pub(crate) fn ai_consent_ack() {
    let _ = config::update(|s| s.ai.first_send_ack = true);
    let _ = audit_log().append("ai.consent", "user", "first-send notice acknowledged");
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
        self.open_pane_with(spec, PaneInit::default(), window, cx)
    }
    fn open_pane_with(&mut self, spec: Spec, pi: PaneInit, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let mut pi = pi;
        // reattaching to a still-running daemon shell: the agent is alive, nothing to resume
        #[cfg(unix)]
        if let Some(id) = &pi.daemon_id {
            if ssh_term_proto::daemon::list(&ssh_term_proto::daemon::socket_path(&data_dir())).is_ok_and(|l| l.iter().any(|i| &i.id == id && i.alive)) { pi.resume = None; }
        }
        let v = self.open_pane_inner(spec, &pi, window, cx);
        self._subs.push(cx.subscribe_in(&v, window, |this, pane, ev: &termview::TermEvent, w, cx| {
            match ev {
                termview::TermEvent::Agent(name, st) => {
                    if !config::get().agents.notify_state { return; }
                    // blocked is already covered by approval / agent_blocked; announce idle (done) and blocked
                    let key = match st { AgentState::Blocked => "main.notify_agent_blocked", AgentState::Idle => "main.notify_agent_idle", _ => return };
                    if this.pane_visible_focused(pane, w, cx) { return; }
                    this.notify_kind(&tf(key, &[("agent", name)]), *st == AgentState::Blocked, cx);
                    return;
                }
                termview::TermEvent::LongDone(cmd, secs, code) => {
                    if this.pane_visible_focused(pane, w, cx) && w.is_window_active() { return; }
                    let short: String = cmd.chars().take(60).collect();
                    this.notify_kind(&tf("main.notify_long_done", &[("cmd", &short), ("secs", &secs.to_string()), ("code", &code.to_string())]), false, cx);
                    return;
                }
                termview::TermEvent::Audit(kind, detail) => { this.log(kind, detail); return; }
                termview::TermEvent::Keyword(title, line) => {
                    this.log("notify.keyword", format!("rule={title:?} line={:?}", ssh_term_proto::ai::redact(&line).0));
                    this.notify_kind(&format!("{title}: {line}"), false, cx);
                    this.toast(format!("🔔 {title}: {line}"), cx);
                    return;
                }
                termview::TermEvent::Explain => {}
            }
            let fc = pane.update(cx, |p, cx| { let f = p.failure_context(); p.failed = None; cx.notify(); f });
            if let Some((cmd, code, out, host)) = fc {
                this.log("ai.explain", format!("{cmd:?} exit={code}"));
                let chat = this.open_chat(w, cx);
                chat.update(cx, |c, cx| c.explain(cmd, code, out, host, cx));
            }
        }));
        v.update(cx, |v, _| { v.resume_offer = pi.resume.clone(); if v.daemon_id.is_none() { v.daemon_id = pi.daemon_id.clone(); } });
        v
    }
    fn open_pane_inner(&mut self, spec: Spec, pi: &PaneInit, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let init = Size { cols: 80, rows: 24 };
        let h = self.history_for(&spec.history_key());
        let m = self.manifests.clone();
        match spec {
            Spec::Local => {
                let shell = config::get().terminal.shell.clone();
                let sh = if shell.is_empty() { std::env::var("SHELL").unwrap_or_else(|_| "bash".into()) } else { shell.clone() };
                let title = tf("main.local_title", &[("shell", &sh.rsplit('/').next().unwrap_or("sh"))]);
                let cwd = pi.cwd.as_deref().map(std::path::Path::new);
                #[cfg(unix)]
                if config::get().session.local_daemon {
                    let id = pi.daemon_id.clone().unwrap_or_else(ssh_term_proto::session_store::uuid_like);
                    let sock = ssh_term_proto::daemon::socket_path(&data_dir());
                    let exe = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("agentshell"))).unwrap_or_else(|| "agentshell".into());
                    match ssh_term_proto::daemon::ensure(&sock, &exe).and_then(|_| ssh_term_proto::daemon::attach(&sock, &id, init, &shell, pi.cwd.as_deref().unwrap_or(""))) {
                        Ok(sess) => {
                            let title = format!("{title} · {}", t("main.daemon_tag"));
                            let v = cx.new(|cx| TermView::new(&title, Spec::Local, Ok(sess), h, m, window, cx));
                            v.update(cx, |v, _| v.daemon_id = Some(id));
                            return v;
                        }
                        Err(e) => self.toast(tf("main.daemon_failed", &[("e", &format!("{e:#}"))]), cx),
                    }
                }
                let sess = local::spawn_local_in(init, (!shell.is_empty()).then_some(shell.as_str()), cwd);
                cx.new(|cx| TermView::new(&title, Spec::Local, sess, h, m, window, cx))
            }
            Spec::Ssh(mut t) => {
                if config::get().session.persistent == "tmux" && t.tmux.is_none() { t.tmux = Some(ssh_term_proto::session_store::new_session_name()); }
                let title = match &t.tmux { Some(n) => format!("{} · tmux:{n}", t.label()), None => t.label() };
                self.log("ssh.connect", format!("{} (known_hosts: {})", t.label(), t.known_hosts.display()));
                let s = ssh::spawn_ssh(self.rt.handle(), t.clone(), init, self.prompts.clone());
                let pv = self.port_view(window, cx);
                pv.update(cx, |v, cx| v.auto_start(&t, cx));
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
    /// SSH target of the focused tab, else of any SSH tab, else the env default target.
    pub(crate) fn ssh_target_for_tools(&self, cx: &App) -> Option<SshTarget> {
        let of = |v: &Entity<TermView>| match &v.read(cx).spec { Spec::Ssh(t) => Some(t.clone()), _ => None };
        self.tabs.get(self.active).and_then(|t| of(&t.focused))
            .or_else(|| self.tabs.iter().flat_map(|t| t.layout.leaves()).find_map(|v| of(&v)))
            .or_else(|| self.default_target.clone())
    }
    pub(crate) fn open_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<chat_view::ChatView> {
        let src = self.focused_pane(window, cx).map(|p| p.downgrade());
        let v = match &self.chat_view {
            Some(v) => v.clone(),
            None => {
                let v = cx.new(|cx| chat_view::ChatView::new(window, cx));
                self._subs.push(cx.subscribe_in(&v, window, |this, _, ev: &chat_view::ChatEvent, w, cx| match ev {
                    chat_view::ChatEvent::Close => this.close_overlay(w, cx),
                    chat_view::ChatEvent::Insert(cmd) => {
                        // AI output is only inserted, never executed (the user presses Enter)
                        if let Some(p) = this.tabs.get(this.active).map(|t| t.focused.clone()) { let cmd = cmd.clone(); p.update(cx, |t, cx| t.ai_insert_text(cmd, cx)); }
                        this.log("ai.chat.insert", cmd);
                        this.close_overlay(w, cx);
                    }
                }));
                self.chat_view = Some(v.clone());
                v
            }
        };
        v.update(cx, |c, cx| { if src.is_some() { c.source = src; } c.focus(window, cx); });
        self.overlay = Overlay::Chat;
        cx.notify();
        v
    }
    pub(crate) fn port_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<port_view::PortView> {
        if let Some(v) = &self.port_view { return v.clone(); }
        let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
        let v = cx.new(|cx| port_view::PortView::new(rt, pr, window, cx));
        self._subs.push(cx.subscribe_in(&v, window, |this, _, _ev: &port_view::PortEvent, w, cx| this.close_overlay(w, cx)));
        self.port_view = Some(v.clone());
        v
    }
    /// Open an ssh:// or sftp:// link: a configured host when one matches (keeps its auth,
    /// jump host and production flag), else an ad-hoc target. Host keys are verified as usual.
    pub(crate) fn open_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let l = match ssh_term_proto::link::parse(url) { Ok(l) => l, Err(e) => return self.toast(e, cx) };
        if l.dropped_password { self.toast(t("main.link_password_dropped").to_string(), cx); }
        let s = config::get();
        let all = s.all_hosts();
        let found = all.iter().find(|e| (e.name == l.host || e.host == l.host) && l.user.as_ref().is_none_or(|u| e.user.as_ref() == Some(u)) && l.port.is_none_or(|p| e.port.unwrap_or(22) == p)).cloned();
        let entry = found.unwrap_or_else(|| HostEntry { name: format!("{}{}{}", l.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default(), l.host, l.port.map(|p| format!(":{p}")).unwrap_or_default()), host: l.host.clone(), port: l.port, user: l.user.clone(), source: "link", ..Default::default() });
        let target = hosts::resolve(&entry, &s.known_hosts_path(), &all, &s.ssh_opts());
        self.log("link.open", format!("kind={:?} host={} user={} configured={}", l.kind, target.host, target.user, entry.source != "link"));
        match l.kind {
            ssh_term_proto::link::Kind::Ssh => self.new_tab(Spec::Ssh(target), window, cx),
            ssh_term_proto::link::Kind::Sftp => {
                let key = target.pool_key();
                let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
                let dir = l.path.clone();
                let v = match self.sftp_views.get(&key) { Some(v) => v.clone(), None => {
                    let v = cx.new(|cx| { let mut v = sftp_view::SftpView::new_deferred(target, rt, pr, window, cx); v.initial_dir = dir; v.start(cx); v });
                    self._subs.push(cx.subscribe_in(&v, window, |this, _, _ev: &sftp_view::SftpEvent, w, cx| this.close_overlay(w, cx)));
                    self.sftp_views.insert(key, v.clone()); v
                } };
                self.sftp_current = Some(v);
                self.overlay = Overlay::Sftp;
            }
        }
        cx.notify();
    }
    fn open_sftp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sftp_current = self.ssh_target_for_tools(cx).map(|t| {
            let key = t.pool_key();
            if let Some(v) = self.sftp_views.get(&key) { return v.clone(); }
            let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
            let v = cx.new(|cx| sftp_view::SftpView::new(t, rt, pr, window, cx));
            self._subs.push(cx.subscribe_in(&v, window, |this, _, _ev: &sftp_view::SftpEvent, w, cx| this.close_overlay(w, cx)));
            self.sftp_views.insert(key, v.clone());
            v
        });
        self.overlay = Overlay::Sftp;
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
    /// Agent quick launch: split the focused pane (same host) and start the configured agent in it.
    pub(crate) fn launch_agent(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(q) = config::get().agents.quick_launch.get(idx).cloned().filter(|q| !q.command.trim().is_empty()) else { return self.toast(t("ui.launch_agent_none").to_string(), cx) };
        self.split(None, window, cx);
        let Some(pane) = self.tabs.get(self.active).map(|t| t.focused.clone()) else { return };
        let host = pane.read(cx).audit_host();
        self.log("agent.launch", format!("name={:?} command={:?} host={host}", q.name, q.command));
        let cmd = q.command.trim().replace(['\r', '\n'], " ");
        cx.spawn(async move |_, cx| {
            for _ in 0..100 {
                let ready = pane.update(cx, |v, _| v.session.clone());
                if let Some(s) = ready { cx.background_executor().timer(std::time::Duration::from_millis(400)).await; s.write(format!("{cmd}\r").into_bytes()); return; }
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            }
        }).detach();
    }
    // ---- broadcast input (P2)
    fn toggle_broadcast(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        use ssh_term_proto::broadcast::{start, Start, Target};
        if termview::broadcast_count() > 0 && !confirmed {
            termview::set_broadcast(&[]); self.log("broadcast.stop", "user"); self.toast(t("bc.stopped").into(), cx); self.refresh_all(cx); return;
        }
        let Some(tab) = self.tabs.get(self.active) else { return };
        let panes: Vec<_> = tab.layout.leaves().into_iter().filter_map(|v| { let r = v.read(cx); r.session.clone().map(|s| (s, r.spec.clone(), r.audit_host())) }).collect();
        let targets: Vec<Target> = panes.iter().enumerate().map(|(i, (_, sp, h))| Target { id: i as u64, label: h.clone(), production: matches!(sp, Spec::Ssh(t) if t.production) }).collect();
        match start(&targets, confirmed) {
            Start::TooFew => self.toast(t("bc.too_few").into(), cx),
            Start::NeedConfirm(prod) => { self.log("broadcast.confirm", format!("prod={prod:?}")); self.modal = Some(Modal::Broadcast(prod)); }
            Start::Ok(ids) => {
                let members: Vec<_> = ids.iter().map(|&i| panes[i as usize].0.clone()).collect();
                termview::set_broadcast(&members);
                self.log("broadcast.start", format!("panes={} hosts={:?} prod_confirmed={confirmed}", members.len(), targets.iter().map(|t| &t.label).collect::<Vec<_>>()));
                self.modal = None; self.refresh_all(cx);
            }
        }
        cx.notify();
    }
    // ---- asciicast recording (P2)
    fn recordings_dir() -> std::path::PathBuf {
        let d = config::get().recording.dir.clone();
        if d.trim().is_empty() { data_dir().join("recordings") } else { ssh_term_proto::hosts::expand_tilde(&d) }
    }
    fn toggle_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.focused_pane(window, cx) else { return };
        let (sess, host) = { let v = p.read(cx); (v.session.clone(), v.audit_host()) };
        let Some(s) = sess else { return };
        if s.stop_recording() { self.log("record.stop", format!("host={host}")); self.toast(t("rec.stopped").into(), cx); }
        else {
            let safe: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
            let path = Self::recordings_dir().join(format!("{}-{safe}.cast", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            let redact = config::get().recording.redact;
            match s.start_recording(&path, &host, redact) {
                Ok(()) => { self.log("record.start", format!("host={host} file={} redact={redact}", path.display())); self.toast(tf("rec.started", &[("f", &path.display())]), cx); }
                Err(e) => self.toast(format!("{e}"), cx),
            }
        }
        p.update(cx, |_, cx| cx.notify());
    }
    fn list_recordings() -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(Self::recordings_dir()).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "cast")).collect()).unwrap_or_default();
        v.sort(); v.reverse(); v.truncate(12); v
    }
    /// Replay in a new tab via `agentshell play` (real terminal timing, idle capped at 2s).
    fn play_recording(&mut self, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        let bin = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("agentshell"))).filter(|p| p.exists()).map(|p| p.display().to_string()).unwrap_or_else(|| "agentshell".into());
        self.new_tab(Spec::Local, window, cx);
        let Some(pane) = self.tabs.get(self.active).map(|t| t.focused.clone()) else { return };
        self.log("record.play", format!("file={}", path.display()));
        let cmd = format!("clear; '{bin}' play '{}'", path.display().to_string().replace('\'', ""));
        cx.spawn(async move |_, cx| {
            for _ in 0..100 {
                if let Some(s) = pane.update(cx, |v, _| v.session.clone()) { cx.background_executor().timer(std::time::Duration::from_millis(400)).await; s.write(format!("{cmd}\r").into_bytes()); return; }
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            }
        }).detach();
    }
    // ---- layout snapshots (P2)
    fn snapshots_path() -> std::path::PathBuf { data_dir().join("layout-snapshots.json") }
    fn push_snapshot(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let l = self.snapshot(cx);
        if l.tabs.is_empty() { return false; }
        let Ok(v) = serde_json::to_value(&l) else { return false };
        let mut st = ssh_term_proto::snapshot::Store::load(&Self::snapshots_path());
        let mut l2 = v.clone(); l2["saved_at"] = serde_json::Value::Null;
        let pushed = st.snaps.first().map(|f| { let mut a = f.layout.clone(); a["saved_at"] = serde_json::Value::Null; a != l2 }).unwrap_or(true)
            && st.push(ssh_term_proto::snapshot::Snap { at: chrono::Local::now().timestamp(), name: format!("{name} · {} tabs / {} panes", l.tabs.len(), self.tabs.iter().map(|t| t.layout.leaves().len()).sum::<usize>()), layout: v }, 10);
        if pushed { let _ = st.save(&Self::snapshots_path()); self.log("layout.snapshot", format!("name={name}")); }
        pushed
    }
    fn rollback_to(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let st = ssh_term_proto::snapshot::Store::load(&Self::snapshots_path());
        let Some(snap) = st.snaps.get(idx).cloned() else { return };
        let Ok(l) = serde_json::from_value::<ssh_term_proto::session_store::LayoutSave>(snap.layout) else { return self.toast("snapshot unreadable".into(), cx) };
        self.push_snapshot("before-rollback", cx);
        termview::set_broadcast(&[]);
        for v in self.tabs.iter().flat_map(|t| t.layout.leaves()).collect::<Vec<_>>() { if v.read(cx).daemon_id.is_none() { self.end_pane(&v, cx); } }
        self.tabs.clear();
        self.restore_from(l, window, cx);
        self.log("layout.rollback", format!("to={:?}", snap.name));
        self.modal = None;
    }
    // ---- quick start: directory + preset layout
    fn quick_start(&mut self, dir: String, preset: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        let pi = || PaneInit { cwd: Some(dir.clone()), ..Default::default() };
        let leaf = |r: &mut Self, window: &mut Window, cx: &mut Context<Self>| Node::Leaf(r.open_pane_with(Spec::Local, pi(), window, cx));
        let sp = |dir, a, b| Node::Split { dir, ratio: 0.5, a: Box::new(a), b: Box::new(b) };
        let layout = match preset {
            "cols2" => { let a = leaf(self, window, cx); let b = leaf(self, window, cx); sp(Dir::Vertical, a, b) }
            "main3" => { let a = leaf(self, window, cx); let b = leaf(self, window, cx); let c2 = leaf(self, window, cx); sp(Dir::Vertical, a, sp(Dir::Horizontal, b, c2)) }
            "grid4" => { let a = leaf(self, window, cx); let b = leaf(self, window, cx); let c2 = leaf(self, window, cx); let d = leaf(self, window, cx); sp(Dir::Horizontal, sp(Dir::Vertical, a, b), sp(Dir::Vertical, c2, d)) }
            _ => leaf(self, window, cx),
        };
        let focused = layout.leaves()[0].clone();
        self.tabs.push(Tab { layout, focused });
        self.active = self.tabs.len() - 1;
        self.log("quickstart.open", format!("dir={dir:?} preset={preset}"));
        if preset == "agent" {
            // shell + configured Agent 1 side by side, both in `dir`
            self.launch_agent(0, window, cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }
    fn render_modal(&mut self, m: Modal, cx: &mut Context<Self>) -> impl IntoElement {
        let th = theme::cur();
        let title = |ic, s: String, col| div().flex().items_center().gap_2().child(icon(ic, 16., col)).child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(s));
        let note = |s: String| div().text_size(px(12.)).text_color(c(th.subtext)).child(s);
        let row = |id: SharedString, main: String, sub: String| div().id(id).px_3().py_2().rounded_md().cursor_pointer().bg(c(th.surface0)).hover(|d| d.bg(c(th.surface1)))
            .flex().flex_col().child(div().text_size(px(12.5)).text_color(c(th.text)).child(main)).child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(sub));
        let cancel = ui::button("modal-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, w, cx| { this.modal = None; this.focus_active(w, cx); cx.notify(); }));
        let mut card = ui::card(560.).p_5().gap_3();
        match m {
            Modal::Broadcast(prod) => {
                card = card.child(title(IconName::TriangleAlert, t("bc.prod_title").into(), c(th.error)))
                    .child(note(tf("bc.prod_body", &[("hosts", &prod.join(", "))])))
                    .child(div().flex().gap_2().justify_end().child(cancel)
                        .child(ui::button("bc-ok", t("bc.prod_ok"), 2).on_click(cx.listener(|this, _, w, cx| { this.toggle_broadcast(true, cx); this.focus_active(w, cx); }))));
            }
            Modal::Snapshots => {
                let st = ssh_term_proto::snapshot::Store::load(&Self::snapshots_path());
                card = card.child(title(IconName::RotateCcw, t("snap.title").into(), c(th.accent))).child(note(t("snap.body").into()));
                if st.snaps.is_empty() { card = card.child(note(t("snap.none").into())); }
                for (i, sn) in st.snaps.iter().enumerate() {
                    let when = chrono::DateTime::from_timestamp(sn.at, 0).map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M:%S").to_string()).unwrap_or_default();
                    card = card.child(row(format!("snap-{i}").into(), sn.name.clone(), when).on_click(cx.listener(move |this, _, w, cx| { this.rollback_to(i, w, cx); cx.notify(); })));
                }
                card = card.child(div().flex().gap_2().justify_end()
                    .child(ui::button("snap-save", t("snap.action"), 1).on_click(cx.listener(|this, _, _w, cx| { if this.push_snapshot("manual", cx) { this.toast(t("snap.saved").into(), cx); } cx.notify(); })))
                    .child(cancel));
            }
            Modal::Recordings => {
                card = card.child(title(IconName::Play, t("rec.play_action").into(), c(th.accent))).child(note(tf("rec.dir", &[("d", &Self::recordings_dir().display())])));
                let list = Self::list_recordings();
                if list.is_empty() { card = card.child(note(t("rec.none").into())); }
                for (i, p) in list.into_iter().enumerate() {
                    let sz = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    let name = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    card = card.child(row(format!("rec-{i}").into(), name, format!("{} KB", sz.div_ceil(1024))).on_click(cx.listener(move |this, _, w, cx| this.play_recording(p.clone(), w, cx))));
                }
                card = card.child(div().flex().justify_end().child(cancel));
            }
            Modal::QuickStart { dir } => {
                card = card.child(title(IconName::FolderOpen, t("qs.action").into(), c(th.accent)));
                let mut dirs: Vec<String> = vec![dir.clone()];
                let home = std::env::var("HOME").unwrap_or_default();
                for d in std::iter::once(home).chain(self.recent_dirs(cx)) { if !d.is_empty() && !dirs.contains(&d) { dirs.push(d); } }
                let mut dl = div().flex().flex_wrap().gap_1();
                for (i, d) in dirs.into_iter().take(8).enumerate() {
                    let sel = d == dir;
                    dl = dl.child(div().id(SharedString::from(format!("qs-dir-{i}"))).px_2().py(px(3.)).rounded_md().cursor_pointer().text_size(px(11.5))
                        .bg(if sel { c(th.accent) } else { c(th.surface0) }).text_color(if sel { c(th.base) } else { c(th.text) }).child(d.clone())
                        .on_click(cx.listener(move |this, _, _w, cx| { this.modal = Some(Modal::QuickStart { dir: d.clone() }); cx.notify(); })));
                }
                dl = dl.child(div().id("qs-browse").px_2().py(px(3.)).rounded_md().cursor_pointer().text_size(px(11.5)).bg(c(th.surface1)).text_color(c(th.text)).child(t("qs.browse"))
                    .on_click(cx.listener(|_this, _, _w, cx| {
                        let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
                        cx.spawn(async move |this, cx| { if let Ok(Ok(Some(p))) = rx.await { if let Some(d) = p.first() { let d = d.display().to_string(); let _ = this.update(cx, |this, cx| { this.modal = Some(Modal::QuickStart { dir: d }); cx.notify(); }); } } }).detach();
                    })));
                card = card.child(note(t("qs.dir").into())).child(dl).child(note(t("qs.layout").into()));
                let mut pl = div().flex().flex_wrap().gap_2();
                for (id, key) in [("single", "qs.single"), ("cols2", "qs.cols2"), ("main3", "qs.main3"), ("grid4", "qs.grid4"), ("agent", "qs.agent")] {
                    let d = dir.clone();
                    pl = pl.child(ui::button(SharedString::from(format!("qs-{id}")), t(key), 1).on_click(cx.listener(move |this, _, w, cx| this.quick_start(d.clone(), id, w, cx))));
                }
                card = card.child(pl).child(div().flex().justify_end().child(cancel));
            }
        }
        ui::modal(card)
    }
    /// Recent local working directories (open panes and layout snapshots).
    fn recent_dirs(&self, cx: &App) -> Vec<String> {
        let mut v: Vec<String> = self.tabs.iter().flat_map(|t| t.layout.leaves()).filter_map(|p| p.read(cx).session.as_ref().and_then(|s| s.cwd_probe.lock().unwrap().as_ref().and_then(|f| f()))).collect();
        let st = ssh_term_proto::snapshot::Store::load(&Self::snapshots_path());
        for sn in st.snaps { let s = sn.layout.to_string(); for m in regex::Regex::new(r#""cwd":"([^"]+)""#).unwrap().captures_iter(&s) { v.push(m[1].to_string()); } }
        let mut seen = HashSet::new(); v.retain(|d| seen.insert(d.clone())); v
    }
    fn close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.focused_pane(window, cx) else { return };
        self.end_pane(&target, cx);
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
    /// Move tab `src` into the active tab as a split at `edge` of `target`.
    fn drop_tab(&mut self, src: usize, target: Entity<TermView>, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        if src == self.active || src >= self.tabs.len() { return; }
        let moved = self.tabs.remove(src);
        if src < self.active { self.active -= 1; }
        let (dir, before) = match edge { Edge::Left => (Dir::Vertical, true), Edge::Right => (Dir::Vertical, false), Edge::Top => (Dir::Horizontal, true), Edge::Bottom => (Dir::Horizontal, false) };
        let focus = moved.focused.clone();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if tab.layout.insert_at(&target, moved.layout, dir, before) { tab.focused = focus; }
        }
        self.log("layout.tab_drop", format!("edge={edge:?}"));
        self.focus_active(window, cx);
        cx.notify();
    }
    /// Move a config host into `group` ("" = ungrouped) and save.
    fn move_host_to_group(&mut self, name: &str, group: &str, cx: &mut Context<Self>) {
        let g = ssh_term_proto::outline::normalize(group);
        let r = config::update(|s| if let Some(h) = s.hosts.iter_mut().find(|h| h.name == name) { h.group = (!g.is_empty()).then(|| g.clone()); });
        if let Some(e) = r { self.toast(e, cx); }
        self.log("hosts.move", format!("host={name} group={g:?}"));
        cx.notify();
    }
    /// Probe the OS of connected SSH panes once per target (read-only `uname`), for the sidebar badge.
    fn probe_os(&mut self, cx: &mut Context<Self>) {
        let todo: Vec<SshTarget> = self.tabs.iter().flat_map(|t| t.layout.leaves()).filter_map(|p| {
            let v = p.read(cx);
            match &v.spec { Spec::Ssh(t) if v.session.as_ref().is_some_and(|s| s.connected.load(Ordering::Acquire)) => Some(t.clone()), _ => None }
        }).collect();
        for tg in todo {
            // Cache per host entry (user@host:port plus its jump chain), not per bare address.
            let label = tg.pool_key();
            if !self.os_probing.insert(label.clone()) { continue; }
            self.log("hosts.os_probe", format!("host={label} command={:?}", ssh_term_proto::outline::PROBE));
            let (map, p) = (self.host_os.clone(), self.prompts.clone());
            let ent = cx.entity().downgrade();
            let (tx, rx) = flume::bounded::<()>(1);
            self.rt.spawn(async move {
                let r = async { let (c, _) = ssh::connect_shared(&tg, &p).await?; c.exec(ssh_term_proto::outline::PROBE, std::time::Duration::from_secs(10)).await }.await;
                if let Ok((_, o)) = r { if let Some(os) = ssh_term_proto::outline::os_from_probe(&String::from_utf8_lossy(&o)) { map.lock().unwrap().insert(label, os); } }
                let _ = tx.send(());
            });
            cx.spawn(async move |_, cx| { let _ = rx.recv_async().await; let _ = ent.update(cx, |_, cx| cx.notify()); }).detach();
        }
    }
    /// A pane is closed by the user: stop its backend (a daemon pane is killed in the daemon too).
    fn end_pane(&self, v: &Entity<TermView>, cx: &App) {
        let v = v.read(cx);
        #[cfg(unix)]
        if let Some(id) = &v.daemon_id { let _ = ssh_term_proto::daemon::kill(&ssh_term_proto::daemon::socket_path(&data_dir()), id); }
        if let Some(s) = &v.session { s.close(); }
    }

    // ---- layout persistence (PRD 3.6)
    fn snapshot(&self, cx: &App) -> ssh_term_proto::session_store::LayoutSave {
        use ssh_term_proto::session_store::{resume_command, NodeSave, PaneSave};
        fn walk(n: &Node<Entity<TermView>>, cx: &App) -> NodeSave {
            match n {
                Node::Leaf(v) => {
                    let v = v.read(cx);
                    let resume = v.agent_label().map(|a| a.0).or(v.last_agent.clone()).and_then(|a| resume_command(&a)).map(str::to_string).or(v.resume_offer.clone());
                    let pane = match &v.spec {
                        Spec::Local => PaneSave::Local { cwd: v.session.as_ref().and_then(|s| s.cwd_probe.lock().unwrap().as_ref().and_then(|f| f())).unwrap_or_default(), daemon_id: v.daemon_id.clone(), resume },
                        Spec::Ssh(t) => PaneSave::Ssh { target: t.clone(), resume },
                    };
                    NodeSave::Leaf { pane }
                }
                Node::Split { dir, ratio, a, b } => NodeSave::Split { vertical: *dir == Dir::Vertical, ratio: *ratio, a: Box::new(walk(a, cx)), b: Box::new(walk(b, cx)) },
            }
        }
        ssh_term_proto::session_store::LayoutSave { version: 1, saved_at: chrono::Local::now().to_rfc3339(), active: self.active, tabs: self.tabs.iter().map(|t| walk(&t.layout, cx)).collect() }
    }
    pub(crate) fn save_layout(&self, cx: &App) {
        let l = self.snapshot(cx);
        if l.tabs.is_empty() { return; }
        if let Err(e) = ssh_term_proto::session_store::save(&data_dir().join("layout.json"), &l) { eprintln!("layout save: {e:#}"); }
        if let Ok(v) = serde_json::to_value(&l) {
            let mut st = ssh_term_proto::snapshot::Store::load(&Self::snapshots_path());
            let same = st.snaps.first().is_some_and(|f| { let (mut a, mut b) = (f.layout.clone(), v.clone()); a["saved_at"] = serde_json::Value::Null; b["saved_at"] = serde_json::Value::Null; a == b });
            if !same && st.push(ssh_term_proto::snapshot::Snap { at: chrono::Local::now().timestamp(), name: format!("auto · {} tabs", l.tabs.len()), layout: v }, 10) { let _ = st.save(&Self::snapshots_path()); }
        }
    }
    /// Rebuild tabs from layout.json. Shells start fresh in the saved cwd; nothing is re-run.
    fn restore_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(l) = ssh_term_proto::session_store::load(&data_dir().join("layout.json")) else { return false };
        if l.tabs.is_empty() { return false; }
        self.restore_from(l, window, cx)
    }
    fn restore_from(&mut self, l: ssh_term_proto::session_store::LayoutSave, window: &mut Window, cx: &mut Context<Self>) -> bool {
        use ssh_term_proto::session_store::{NodeSave, PaneSave};
        if l.tabs.is_empty() { return false; }
        fn build(r: &mut Root, n: &NodeSave, window: &mut Window, cx: &mut Context<Root>) -> Node<Entity<TermView>> {
            match n {
                NodeSave::Leaf { pane } => Node::Leaf(match pane.clone() {
                    PaneSave::Local { cwd, daemon_id, resume } => r.open_pane_with(Spec::Local, PaneInit { cwd: (!cwd.is_empty()).then_some(cwd), daemon_id, resume }, window, cx),
                    PaneSave::Ssh { target, resume } => r.open_pane_with(Spec::Ssh(target), PaneInit { resume, ..Default::default() }, window, cx),
                }),
                NodeSave::Split { vertical, ratio, a, b } => Node::Split { dir: if *vertical { Dir::Vertical } else { Dir::Horizontal }, ratio: *ratio, a: Box::new(build(r, a, window, cx)), b: Box::new(build(r, b, window, cx)) },
            }
        }
        for n in &l.tabs {
            let layout = build(self, n, window, cx);
            let focused = layout.leaves()[0].clone();
            self.tabs.push(Tab { layout, focused });
        }
        self.active = l.active.min(self.tabs.len() - 1);
        self.log("layout.restore", format!("{} tabs from {}", self.tabs.len(), l.saved_at));
        self.toast(t("main.layout_restored").into(), cx);
        self.focus_active(window, cx);
        true
    }
    fn should_close(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> bool {
        self.save_layout(cx);
        if self.quit_ok { return true; }
        let agents = self.running_agents(cx);
        let live = self.tabs.iter().flat_map(|t| t.layout.leaves()).filter(|v| { let v = v.read(cx); v.daemon_id.is_none() && v.session.as_ref().is_some_and(|s| !s.exited.load(Ordering::Acquire)) }).count();
        if config::get().session.confirm_close && (agents > 0 || live > 0) { self.quit_confirm = true; cx.notify(); return false; }
        self.log("app.quit", "window closed");
        true
    }
    fn running_agents(&self, cx: &App) -> usize { self.tabs.iter().flat_map(|t| t.layout.leaves()).filter(|v| v.read(cx).agent_label().is_some()).count() }

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
        if let Some(mp) = ssh_term_proto::managed::get() { if !mp.mode_allowed(m) { self.log("permission.mode", format!("{} refused by organization policy", m.label())); return self.toast(tf("main.org_mode_refused", &[("m", &m.label())]), cx); } }
        let err = config::update(|s| s.permissions.mode = m);
        self.log("permission.mode", format!("{} → {}", old.label(), m.label()));
        self.report(err, cx);
        self.toast(tf("main.mode_toast", &[("mode", &m.label()), ("desc", &m.desc())]), cx);
    }

    /// Desktop notification (notify.desktop + notify.agent_blocked). Best effort: without a
    /// notification daemon the call fails silently (logged to stderr).
    pub(crate) fn notify_desktop(&self, body: &str, cx: &mut Context<Self>) { self.notify_kind(body, true, cx) }

    /// The pane is on screen in the active tab, focused, and the window is active: no need to notify.
    fn pane_visible_focused(&self, pane: &Entity<TermView>, w: &mut Window, cx: &mut Context<Self>) -> bool {
        w.is_window_active() && pane.read(cx).focus.is_focused(w)
    }

    /// `blocked` = an agent needs the user (gated by notify.agent_blocked); otherwise a general event.
    pub(crate) fn notify_kind(&self, body: &str, blocked: bool, _cx: &mut Context<Self>) {
        let n = &config::get().notify;
        if n.sound { termview::play_sound(); }
        if !n.desktop || (blocked && !n.agent_blocked) { return; }
        self.log("notify", body);
        let body = body.to_string();
        std::thread::spawn(move || {
            if let Err(e) = notify_rust::Notification::new().appname("AgentShell").summary("AgentShell").body(&body).show() { eprintln!("notification: {e}"); }
        });
    }

    fn answer_approval(&mut self, ans: ssh_term_proto::policy_server::Answer, cx: &mut Context<Self>) {
        if !self.approvals.is_empty() {
            let a = self.approvals.remove(0);
            let _ = a.reply.send(ans);
        }
        self.deny_reason = None;
        cx.notify();
    }

    fn answer_auth(&mut self, submit: bool, cx: &mut Context<Self>) {
        if self.auth_pending.is_empty() { return; }
        let p = self.auth_pending.remove(0);
        let val = self.auth_input.as_ref().map(|i| i.read(cx).value().to_string()).unwrap_or_default();
        let what = match &p.kind { ssh::AuthKind::Password => "ssh.auth.password", ssh::AuthKind::Passphrase(_) => "ssh.auth.passphrase", ssh::AuthKind::Interactive { .. } => "ssh.auth.kbd" };
        // the secret itself is never logged
        self.log(what, format!("{} {} save={}", p.label, if submit { "submitted" } else { "cancelled" }, self.auth_save && submit));
        let interactive = matches!(p.kind, ssh::AuthKind::Interactive { .. });
        // keyboard-interactive answers may legitimately be empty and are never saved
        let ok = submit && (interactive || !val.is_empty());
        let _ = p.reply.send(if ok { Some((val, self.auth_save && !interactive)) } else { None });
        self.auth_input = None;
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
            SettingsEvent::ThemesChanged => { this.themes = theme::load_all(); cx.notify(); }
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
            Action::CyclePermissionMode => { let mut m = config::get().permissions.mode.next(); if let Some(o) = ssh_term_proto::managed::get() { for _ in 0..3 { if o.mode_allowed(m) { break; } m = m.next(); } } self.set_mode(m, cx); }
            Action::ModeAsk => self.set_mode(Mode::Ask, cx),
            Action::ModePlan => self.set_mode(Mode::Plan, cx),
            Action::ModeAuto => self.set_mode(Mode::Auto, cx),
            Action::OpenSftp => self.open_sftp(window, cx),
            Action::OpenHostTools => {
                let tg = self.focused_pane(window, cx).and_then(|p| match &p.read(cx).spec { Spec::Ssh(t) => Some(t.clone()), _ => None });
                let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
                let v = cx.new(|cx| host_tools_view::HostToolsView::new(tg, rt, pr, cx));
                self._subs.push(cx.subscribe_in(&v, window, |this, _, _ev: &host_tools_view::HostToolsEvent, w, cx| this.close_overlay(w, cx)));
                self.overlay = Overlay::HostTools(v);
            }
            Action::OpenPortForward => { let tg = self.ssh_target_for_tools(cx); let pv = self.port_view(window, cx); pv.update(cx, |v, _| if v.target.is_none() || tg.is_some() { v.target = tg }); self.overlay = Overlay::Port; }
            Action::OpenAi => self.overlay = Overlay::Ai,
            Action::LaunchAgent1 => self.launch_agent(0, window, cx),
            Action::LaunchAgent2 => self.launch_agent(1, window, cx),
            Action::LaunchAgent3 => self.launch_agent(2, window, cx),
            Action::ToggleBroadcast => self.toggle_broadcast(false, cx),
            Action::ToggleRecording => self.toggle_recording(window, cx),
            Action::PlayRecording => self.modal = Some(Modal::Recordings),
            Action::SnapshotLayout => { if self.push_snapshot("manual", cx) { self.toast(t("snap.saved").into(), cx); } }
            Action::RollbackLayout => self.modal = Some(Modal::Snapshots),
            Action::QuickStart => {
                let dir = self.focused_pane(window, cx).and_then(|p| p.read(cx).session.as_ref().and_then(|s| s.cwd_probe.lock().unwrap().as_ref().and_then(|f| f()))).unwrap_or_else(|| std::env::var("HOME").unwrap_or_default());
                self.modal = Some(Modal::QuickStart { dir });
            }
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
        let all = s.all_hosts();
        let o = s.ssh_opts();
        all.iter().map(|h| { let t = hosts::resolve(h, &kh, &all, &o); (h.clone(), t) }).collect()
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
        let ungrouped = t("ui.ungrouped").to_string();
        let mut nested_paths: Vec<String> = vec![];
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
            let g = if h.source == "config" {
                match h.group.as_deref().map(ssh_term_proto::outline::normalize).filter(|g| !g.is_empty()) { Some(g) => { nested_paths.push(g.clone()); format!("\u{1}{g}") } None => ungrouped.clone() }
            } else { "~/.ssh/config".into() };
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
        // section order: quick connect, nested config groups (a/b/c tree, PRD 3.1), ungrouped, ~/.ssh/config
        let host_os = self.host_os.lock().unwrap().clone();
        let mut groups: HashMap<String, Vec<E>> = groups.into_iter().collect();
        struct Sec { key: String, name: String, depth: usize, drop: Option<String>, count: usize }
        let mut secs: Vec<Sec> = vec![];
        let qc = t("ui.quick_connect").to_string();
        let flat = |k: &str, drop: Option<String>, secs: &mut Vec<Sec>, groups: &HashMap<String, Vec<E>>| if let Some(v) = groups.get(k) { secs.push(Sec { key: k.into(), name: k.into(), depth: 0, drop, count: v.len() }); };
        flat(&qc, None, &mut secs, &groups);
        let host_groups: Vec<String> = { let mut v = nested_paths.clone(); if !q.is_empty() { v.retain(|p| groups.contains_key(&format!("\u{1}{p}"))); } v };
        for r in ssh_term_proto::outline::rows(&host_groups) {
            let count = ssh_term_proto::outline::count_under(&r.path, &host_groups);
            secs.push(Sec { key: format!("\u{1}{}", r.path), name: r.name, depth: r.depth, drop: Some(r.path), count });
        }
        flat(&ungrouped, Some(String::new()), &mut secs, &groups);
        flat("~/.ssh/config", None, &mut secs, &groups);
        let collapsed_set = self.collapsed.clone();
        for sec in secs {
            let gname = sec.key.clone();
            let collapsed = self.collapsed.contains(&gname) && q.is_empty();
            if q.is_empty() && sec.depth > 0 {
                let path = gname.trim_start_matches('\u{1}').to_string();
                let parent = path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
                if ssh_term_proto::outline::hidden_by(&parent, &|p| collapsed_set.contains(&format!("\u{1}{p}"))) { continue; }
            }
            let g2 = gname.clone();
            let mut hdr = div().id(SharedString::from(format!("grp-{gname}"))).mx_2().mt_2().pr_2().pl(px(8. + 12. * sec.depth as f32)).h(px(24.)).flex().items_center().gap_1().cursor_pointer().rounded_md()
                .hover(|d| d.bg(c(cur().surface0).opacity(0.5)))
                .on_click(cx.listener(move |this, _, _w, cx| { if !this.collapsed.remove(&g2) { this.collapsed.insert(g2.clone()); } cx.notify(); }))
                .child(icon(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }, 12., c(th.overlay0)))
                .when(sec.drop.is_some() && !sec.key.starts_with(&ungrouped), |d| d.child(icon(IconName::Folder, 11., c(th.overlay0))))
                .child(div().text_size(px(10.5)).font_weight(FontWeight::MEDIUM).text_color(c(th.overlay0)).child(sec.name.clone()))
                .child(div().flex_1())
                .child(div().text_size(px(10.5)).text_color(c(th.overlay0)).child(sec.count.to_string()));
            if let Some(target) = sec.drop.clone() {
                hdr = hdr.drag_over::<HostDrag>(|st, _, _, _| st.bg(c(cur().accent).opacity(0.25)))
                    .on_drop(cx.listener(move |this, d: &HostDrag, _w, cx| this.move_host_to_group(&d.name, &target, cx)));
            }
            list = list.child(hdr);
            let entries = groups.remove(&gname).unwrap_or_default();
            if collapsed { continue; }
            for e in entries {
                let accent = if matches!(e.spec, Spec::Local) { c(th.info) } else if e.prod { c(th.error) } else { c(th.warning) };
                let dot = if online(&e.label) { c(th.success) } else { c(th.surface2) };
                let spec = e.spec.clone();
                let gid = SharedString::from(format!("host-row-{idx}"));
                let os_key = match &e.spec { Spec::Ssh(t) => t.pool_key(), _ => e.label.clone() };
                let os = host_os.get(&os_key).map(|s| s.as_str());
                let mut row = div().id(("host", idx)).group(gid.clone()).mx_2().px_2().pl(px(8. + 12. * sec.depth as f32)).py(px(5.)).rounded_lg().flex().items_center().gap_3().cursor_pointer()
                    .hover(|s| s.bg(c(cur().surface0)))
                    .on_click(cx.listener(move |this, _, w, cx| this.new_tab(spec.clone(), w, cx)))
                    .child(div().size(px(26.)).rounded_md().flex().items_center().justify_center().bg(accent.opacity(0.18)).text_color(accent).text_size(px(if os.is_some() { 10.5 } else { 12. })).font_weight(FontWeight::BOLD)
                        .child(os.map(|o| ssh_term_proto::outline::badge(o).0).unwrap_or(e.letter)))
                    .child(div().flex_1().min_w_0().flex().flex_col().overflow_hidden()
                        .child(div().flex().items_center().gap_1().child(div().text_size(px(12.5)).text_color(c(th.text)).child(e.name.clone()))
                            .when(e.prod, |d| d.child(ui::tag(t("ui.prod"), c(th.error)))))
                        .child(div().text_size(px(10.5)).text_color(c(th.overlay0)).child(match os { Some(o) => format!("{} · {}", e.detail, ssh_term_proto::outline::badge(o).1), None => e.detail.clone() })));
                if let Some(h) = &e.edit { let d = HostDrag { name: h.name.clone() }; row = row.on_drag(d, |d: &HostDrag, _, _, cx| cx.new(|_| d.clone())); }
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
        let items: [(Action, &str, IconName); 8] = [
            (Action::OpenSftp, t("ui.sftp_files"), IconName::FolderSync),
            (Action::OpenPortForward, t("ui.port_forwarding"), IconName::ArrowLeftRight),
            (Action::OpenHostTools, t("hosttools.title"), IconName::ChartPie),
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
        t.font_size = px(config::get().appearance.ui_font_size.clamp(10., 20.));
        t.font_family = SharedString::from("Inter");
        t.radius = px(6.);
        t.focus_ring = false;
        t.shadow = false;
    });
    cx.refresh_windows();
}

impl Root {
    fn render_node(&self, tab: usize, node: &Node<Entity<TermView>>, path: Vec<bool>, th: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf(p) => {
                let mut d = div().relative().flex_1().size_full().flex().overflow_hidden().child(p.clone());
                if cx.has_active_drag() && self.tabs.len() > 1 {
                    // drop zones for a dragged tab: left/right quarter, top/bottom half of the middle (PRD US-03)
                    for (i, edge) in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom].into_iter().enumerate() {
                        let z = div().id(SharedString::from(format!("dz-{tab}-{}-{i}", p.entity_id()))).absolute();
                        let z = match edge {
                            Edge::Left => z.top_0().left_0().h_full().w(gpui::relative(0.25)),
                            Edge::Right => z.top_0().right_0().h_full().w(gpui::relative(0.25)),
                            Edge::Top => z.top_0().left(gpui::relative(0.25)).w(gpui::relative(0.5)).h(gpui::relative(0.5)),
                            Edge::Bottom => z.bottom_0().left(gpui::relative(0.25)).w(gpui::relative(0.5)).h(gpui::relative(0.5)),
                        };
                        let target = p.clone();
                        d = d.child(z.drag_over::<TabDrag>(|st, _, _, _| st.bg(c(cur().accent).opacity(0.28)))
                            .on_drop(cx.listener(move |this, dd: &TabDrag, w, cx| this.drop_tab(dd.idx, target.clone(), edge, w, cx))));
                    }
                }
                d.into_any_element()
            }
            Node::Split { dir, ratio, a, b } => {
                let vertical = *dir == Dir::Vertical;
                let id = SharedString::from(format!("split-{tab}-{}", path.iter().map(|b| if *b { '1' } else { '0' }).collect::<String>()));
                let d = div().id(id.clone()).flex_1().size_full().flex().overflow_hidden();
                let d = if vertical { d.flex_row() } else { d.flex_col() };
                let (pa, pb) = { let mut x = path.clone(); x.push(false); let mut y = path.clone(); y.push(true); (x, y) };
                let first = div().flex().overflow_hidden().flex_shrink_0().child(self.render_node(tab, a, pa, th, cx));
                let first = if vertical { first.h_full().w(gpui::relative(*ratio)) } else { first.w_full().h(gpui::relative(*ratio)) };
                let second = div().flex_1().flex().overflow_hidden().child(self.render_node(tab, b, pb, th, cx));
                let drag = DividerDrag { tab, path: path.clone() };
                let divider = div().id(SharedString::from(format!("{id}-div"))).flex_shrink_0().bg(c(th.surface1)).hover(|s| s.bg(c(cur().accent)))
                    .on_drag(drag, |d: &DividerDrag, _, _, cx| cx.new(|_| d.clone()));
                let divider = if vertical { divider.w(px(3.)).h_full().cursor_col_resize() } else { divider.h(px(3.)).w_full().cursor_row_resize() };
                let me_path = path.clone();
                d.on_drag_move(cx.listener(move |this, ev: &gpui::DragMoveEvent<DividerDrag>, _w, cx| {
                    let dd = ev.drag(cx).clone();
                    if dd.path != me_path || dd.tab != tab { return; }
                    let b = ev.bounds;
                    let r = if vertical { (ev.event.position.x - b.left()) / b.size.width } else { (ev.event.position.y - b.top()) / b.size.height };
                    if let Some(t) = this.tabs.get_mut(tab) { if t.layout.set_ratio(&dd.path, r) { cx.notify(); } }
                }))
                .child(first).child(divider).child(second).into_any_element()
            }
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
        self.probe_os(cx);
        // appearance.opacity: transparent window background, terminal area drawn with alpha
        let op = s.appearance.opacity.clamp(0.5, 1.0);
        let want = if op < 0.999 { gpui::WindowBackgroundAppearance::Transparent } else { gpui::WindowBackgroundAppearance::Opaque };
        if self.bg_seen != Some(want) { self.bg_seen = Some(want); window.set_background_appearance(want); }
        let ui = s.appearance.ui_font_size.clamp(10., 20.);
        if self.ui_size_seen != ui { self.ui_size_seen = ui; gpui_component::Theme::global_mut(cx).font_size = px(ui); }

        let mut tabbar = div().h(px(40.)).px_2().flex().items_center().gap_1().bg(c(th.crust)).border_b_1().border_color(c(th.surface0));
        if !self.sidebar_visible {
            tabbar = tabbar.child(div().id("sb-show").size(px(28.)).rounded_lg().flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(|this, _, _w, cx| { this.sidebar_visible = true; cx.notify(); })).child(icon(IconName::Layers, 14., c(th.overlay1))));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            let panes = t.layout.leaves();
            let title: SharedString = {
                let first = shown_title(panes[0].read(cx));
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
                .child(div().text_size(px(12.)).text_color(if active { c(th.text) } else { c(th.overlay1) }).child(title.clone()));
            if let Some((name, st)) = agent {
                let col = state_color(st, &th);
                tabel = tabel.child(div().px(px(6.)).rounded_md().bg(col.opacity(0.18)).text_color(col).text_size(px(11.)).child(format!("● {name} {}", st.label())));
            }
            let drag = TabDrag { idx: i, title: title.clone() };
            tabel = tabel.on_drag(drag, |d: &TabDrag, _, _, cx| cx.new(|_| d.clone()));
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
            Some(tab) => div().flex_1().flex().overflow_hidden().child(self.render_node(self.active, &tab.layout, vec![], &th, cx)),
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
                (st, col, d, shown_title(v), tf("main.history_n", &[("n", &n)]))
            }
            None => ("—", c(th.overlay0), String::new(), SharedString::from(""), String::new()),
        };
        let theme_label = if s.appearance.theme == "system" { tf("main.theme_system", &[("name", &th.name)]) } else { th.name.clone() };
        let org = ssh_term_proto::managed::get();
        let mode = org.as_ref().map_or(s.permissions.mode, |m| m.clamp_mode(s.permissions.mode));
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
                .child(icon(IconName::Shield, 12., mode_col)).child(tf("main.perm_status", &[("mode", &mode.label())]))
                .when(org.is_some(), |d| d.child(div().pl_1().child(t("main.org_badge")))));

        let mut root = div().size_full().relative().flex().flex_row().bg(c(th.crust).opacity(op)).font_family(SharedString::from("Inter")).text_size(px(s.appearance.ui_font_size.clamp(10., 20.))).text_color(c(th.text))
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
            Overlay::HostTools(v) => Some(ui::modal(v.clone()).into_any_element()),
            Overlay::Ai => Some(self.ai_panel(cx)),
            Overlay::Chat => self.chat_view.clone().map(|v| ui::modal(v).into_any_element()),
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
        // password / passphrase prompt
        if self.hk_pending.is_empty() && !self.auth_pending.is_empty() {
            if self.auth_input.is_none() {
                let masked = !matches!(self.auth_pending[0].kind, ssh::AuthKind::Interactive { echo: true, .. });
                let inp = cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(t("ui.auth_placeholder")));
                inp.update(cx, |i, cx| i.focus(window, cx));
                self.auth_input = Some(inp);
            }
            let p = &self.auth_pending[0];
            let (title, detail) = match &p.kind {
                ssh::AuthKind::Password => (t("ui.auth_password_title"), tf("main.auth_password_for", &[("target", &p.label)])),
                ssh::AuthKind::Passphrase(path) => (t("ui.auth_passphrase_title"), tf("main.auth_passphrase_for", &[("key", &path.display())])),
                ssh::AuthKind::Interactive { instructions, prompt, .. } => {
                    let mut d = tf("main.auth_kbd_for", &[("target", &p.label), ("prompt", prompt)]);
                    if !instructions.is_empty() { d = format!("{instructions}\n{d}"); }
                    (t("ui.auth_kbd_title"), d)
                }
            };
            let can_save = !matches!(p.kind, ssh::AuthKind::Interactive { .. });
            let inp = self.auth_input.clone().unwrap();
            root = root.child(ui::modal(ui::card(480.).p_5().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::KeyRound, 16., c(th.accent)))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title)))
                .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child(detail))
                .child(gpui_component::input::Input::new(&inp))
                .when(can_save, |d| d.child(div().flex().items_center().gap_2()
                    .child(gpui_component::switch::Switch::new("auth-save").checked(self.auth_save).on_click(cx.listener(|this, v: &bool, _w, cx| { this.auth_save = *v; cx.notify(); })))
                    .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(t("ui.auth_save")))))
                .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(t("ui.auth_note")))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("au-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _w, cx| this.answer_auth(false, cx))))
                    .child(ui::button("au-ok", t("ui.connect"), 1).on_click(cx.listener(|this, _, _w, cx| this.answer_auth(true, cx)))))));
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
                .when_some(self.deny_reason.clone(), |d, inp| d.child(div().flex().items_center().gap_2()
                    .child(div().flex_1().child(gpui_component::input::Input::new(&inp)))
                    .child(ui::button("ap-deny-send", t("ui.deny_send_reason"), 2).on_click(cx.listener(|this, _, _w, cx| {
                        let why = this.deny_reason.as_ref().map(|i| i.read(cx).value().to_string()).unwrap_or_default();
                        this.answer_approval(ssh_term_proto::policy_server::Answer::Deny(why), cx);
                    })))))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("ap-deny", t("ui.deny"), 0).on_click(cx.listener(|this, _, _w, cx| this.answer_approval(ssh_term_proto::policy_server::Answer::Deny(String::new()), cx))))
                    .child(ui::button("ap-deny-why", t("ui.deny_with_reason"), 0).on_click(cx.listener(|this, _, w, cx| {
                        let inp = cx.new(|cx| InputState::new(w, cx).placeholder(t("ui.deny_reason_placeholder")));
                        inp.update(cx, |i, cx| i.focus(w, cx));
                        this.deny_reason = Some(inp); cx.notify();
                    })))
                    .child(ui::button("ap-session", tf("ui.allow_session_prefix", &[("p", &ssh_term_proto::policy_server::command_prefix(&a.req.command))]), 0).on_click(cx.listener(|this, _, _w, cx| this.answer_approval(ssh_term_proto::policy_server::Answer::Session, cx))))
                    .child(ui::button("ap-allow", t("ui.allow_once"), 1).on_click(cx.listener(|this, _, _w, cx| this.answer_approval(ssh_term_proto::policy_server::Answer::Once, cx)))))));
        } }
        if self.quit_confirm {
            let agents = self.running_agents(cx);
            let panes = self.tabs.iter().map(|t| t.layout.leaves().len()).sum::<usize>();
            let daemon = self.tabs.iter().flat_map(|t| t.layout.leaves()).filter(|v| v.read(cx).daemon_id.is_some()).count();
            root = root.child(ui::modal(ui::card(520.).p_5().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::TriangleAlert, 16., c(th.warning)))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("main.quit_title"))))
                .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child(tf("main.quit_body", &[("p", &panes), ("a", &agents), ("d", &daemon)])))
                .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(t("main.quit_note")))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("quit-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _w, cx| { this.quit_confirm = false; cx.notify(); })))
                    .child(ui::button("quit-ok", t("main.quit_ok"), 2).on_click(cx.listener(|this, _, w, cx| {
                        this.quit_ok = true; this.save_layout(cx); this.log("app.quit", "confirmed");
                        for v in this.tabs.iter().flat_map(|t| t.layout.leaves()) { if let Some(s) = &v.read(cx).session { if v.read(cx).daemon_id.is_none() { s.close(); } } }
                        w.remove_window(); cx.quit();
                    }))))));
        }
        if let Some(m) = self.modal.clone() { root = root.child(self.render_modal(m, cx)); }
        if let Some((msg, _)) = &self.toast {
            root = root.child(div().absolute().bottom(px(40.)).right(px(16.)).max_w(px(460.)).px_4().py_2().rounded_lg().bg(c(th.base)).border_1().border_color(c(th.surface1)).shadow_lg()
                .flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text)).child(icon(IconName::Info, 14., c(th.accent))).child(msg.clone()));
        }
        root
    }
}

fn main() {
    // organization policy first: read-only, highest priority (PRD 6.5)
    let _ = ssh_term_proto::managed::load();
    let (settings, load_err) = config::Settings::load();
    if let Some(e) = &load_err { eprintln!("{e}"); }
    config::install(settings);
    // ssh:// / sftp:// links (and Xshell -url): hand them to a running instance if there is one
    let links = ssh_term_proto::link::from_args(&std::env::args().collect::<Vec<_>>());
    if !links.is_empty() {
        if let Some(ep) = ssh_term_proto::policy_server::read_endpoint(&data_dir()).filter(|e| e.host.is_empty()) {
            let ok = links.iter().all(|u| matches!(ssh_term_proto::policy_server::post(&ep.url, "/v1/open", &ep.token, None, &serde_json::json!({"url": u}).to_string(), std::time::Duration::from_secs(5)), Ok((200, _, _))));
            if ok { eprintln!("opened in the running AgentShell"); return; }
        }
    }
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
                    let (au_tx, au_rx) = flume::unbounded::<ssh::AuthPrompt>();
                    let _ = ssh::SECRETS.set(secret_store());
                    let host_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.search_hosts")));
                    let audit = audit_log();
                    let mut root = Root {
                        rt, default_target, tabs: vec![], active: 0, themes, histories: HashMap::new(),
                        manifests: Arc::new(agent::bundled()), prompts: ssh::Prompts { hostkey: hk_tx, auth: au_tx }, auth_pending: vec![], auth_input: None, auth_save: false, hk_pending: vec![], approvals: vec![], deny_reason: None, policy_port: None, overlay: Overlay::None, sftp_views: HashMap::new(), sftp_current: None, port_view: None, quit_confirm: false, modal: None, quit_ok: false, chat_view: None,
                        host_search: host_search.clone(), lang_seen: ssh_term_proto::i18n::current() as u8, bg_seen: None, ui_size_seen: 0., collapsed: HashSet::new(), host_os: Default::default(), os_probing: HashSet::new(), sidebar_visible: true, audit, audit_verify: None, audit_filter: Default::default(),
                        settings_dirty: vec![], toast: None, toast_seq: 0, _subs: vec![],
                    };
                    root.apply_theme(window, cx);
                    root._subs.push(cx.observe_window_appearance(window, |this: &mut Root, w, cx| { this.apply_theme(w, cx); this.refresh_all(cx); }));
                    root._subs.push(cx.subscribe_in(&host_search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
                    cx.spawn(async move |this, cx| {
                        while let Ok(p) = au_rx.recv_async().await {
                            if this.update(cx, |r, cx| { r.auth_pending.push(p); cx.notify(); }).is_err() { break; }
                        }
                    }).detach();
                    cx.spawn(async move |this, cx| {
                        while let Ok(p) = hk_rx.recv_async().await {
                            if this.update(cx, |r, cx| { r.hk_pending.push(p); cx.notify(); }).is_err() { break; }
                        }
                    }).detach();
                    // audit retention (audit.retention_days)
                    match root.audit.prune(ssh_term_proto::managed::get().and_then(|m| m.retention_floor()).map_or(config::get().audit.retention_days, |f| config::get().audit.retention_days.max(f))) { Ok(0) => {}, Ok(n) => eprintln!("audit: pruned {n} records"), Err(e) => eprintln!("audit prune: {e}") }
                    // update check (general.check_updates); quiet on network errors
                    if config::get().general.check_updates && std::env::var_os("AGENTSHELL_NO_UPDATE_CHECK").is_none() {
                        let url = config::get().general.manifest_url();
                        cx.spawn(async move |this, cx| {
                            let r = cx.background_executor().spawn(async move { ssh_term_proto::update::check(&url) }).await;
                            match r {
                                Ok(Some(m)) => { let _ = this.update(cx, |r, cx| { r.log("update.available", &m.version); r.toast(tf("main.update_available", &[("v", &m.version), ("url", &m.url)]), cx); }); }
                                Ok(None) => {}
                                Err(e) => eprintln!("update check: {e:#}"),
                            }
                        }).detach();
                    }
                    // local policy service for external agents' hooks
                    let (ap_tx, ap_rx) = flume::unbounded::<ssh_term_proto::policy_server::Approval>();
                    let ctx: ssh_term_proto::policy_server::CtxFn = Arc::new(|| config::get().policy_ctx());
                    let bind = format!("127.0.0.1:{}", config::get().agents.policy_port);
                    // US-10: agents run commands / transfer files on configured hosts through the App's pool
                    let targets: ssh_term_proto::broker::TargetsFn = Arc::new(|| {
                        let s = config::get(); let kh = s.known_hosts_path(); let all = s.all_hosts(); let o = s.ssh_opts();
                        all.iter().map(|h| (h.name.clone(), hosts::resolve(h, &kh, &all, &o))).collect()
                    });
                    let ops: Arc<dyn ssh_term_proto::broker::Ops> = Arc::new(ssh_term_proto::broker::SshOps { rt: root.rt.handle().clone(), targets, prompts: root.prompts.clone() });
                    match ssh_term_proto::policy_server::start_with_ops(&bind, None, ctx, Some(ap_tx), root.audit.clone(), Some(ssh_term_proto::policy_server::endpoint_path(&data_dir())), Some(ops)) {
                        Ok(srv) => { root.policy_port = Some(srv.port); *ssh::POLICY_ENDPOINT.lock().unwrap() = Some((srv.port, srv.token.clone())); root.log("policy.server", format!("listening on 127.0.0.1:{}", srv.port)); }
                        Err(e) => root.toast(tf("main.policy_server_failed", &[("e", &e)]), cx),
                    }
                    // links: from this launch's command line and from later launches (/v1/open)
                    let (otx, orx) = flume::unbounded::<String>();
                    let _ = ssh_term_proto::policy_server::OPEN_TX.set(otx.clone());
                    for u in &links { let _ = otx.send(u.clone()); }
                    cx.spawn_in(window, async move |this, cx| {
                        while let Ok(u) = orx.recv_async().await {
                            if this.update_in(cx, |r, w, cx| { w.activate_window(); r.open_link(&u, w, cx); }).is_err() { break; }
                        }
                    }).detach();
                    cx.spawn(async move |this, cx| {
                        while let Ok(a) = ap_rx.recv_async().await {
                            if this.update(cx, |r, cx| { r.approvals.push(a); r.notify_desktop(t("ui.approval_title"), cx); cx.notify(); }).is_err() { break; }
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
                    // system tray: running-agent count + show / new terminal / quit (Linux SNI)
                    #[cfg(target_os = "linux")]
                    if config::get().general.tray && std::env::var("AGENTSHELL_NO_TRAY").is_err() {
                        let (ttx, trx) = flume::unbounded::<ssh_term_proto::tray::TrayCmd>();
                        let zh = matches!(ssh_term_proto::i18n::current(), ssh_term_proto::i18n::Lang::ZhCn);
                        let trt = root.rt.handle().clone();
                        let (htx, hrx) = flume::bounded(1);
                        root.rt.spawn(async move { let _ = htx.send(ssh_term_proto::tray::spawn(ssh_term_proto::tray::AppTray { agents: 0, waiting: 0, zh, tx: ttx }).await); });
                        cx.spawn_in(window, async move |this, cx| {
                            let handle = match hrx.recv_async().await { Ok(Ok(h)) => { let _ = this.update(cx, |r, _| r.log("tray", "StatusNotifierItem registered")); Some(h) } Ok(Err(e)) => { let _ = this.update(cx, |r, _| r.log("tray", format!("{e:#}"))); None } Err(_) => None };
                            let Some(handle) = handle else { return };
                            let mut last = (usize::MAX, usize::MAX);
                            loop {
                                while let Ok(cmd) = trx.try_recv() {
                                    let _ = this.update_in(cx, |r, w, cx| match cmd {
                                        ssh_term_proto::tray::TrayCmd::Show => w.activate_window(),
                                        ssh_term_proto::tray::TrayCmd::NewLocal => { w.activate_window(); r.new_tab(Spec::Local, w, cx); }
                                        ssh_term_proto::tray::TrayCmd::Quit => { r.log("tray", "quit"); if r.should_close(w, cx) { w.remove_window(); cx.quit(); } else { w.activate_window(); } }
                                    });
                                }
                                let Ok(now) = this.update(cx, |r, cx| {
                                    let leaves: Vec<_> = r.tabs.iter().flat_map(|t| t.layout.leaves()).collect();
                                    let labels: Vec<_> = leaves.iter().filter_map(|v| v.read(cx).agent_label()).collect();
                                    (labels.len(), labels.iter().filter(|(_, st)| *st == agent::AgentState::Blocked).count())
                                }) else { break };
                                if now != last { last = now; let h = handle.clone(); trt.spawn(async move { h.update(move |t| { t.agents = now.0; t.waiting = now.1; }).await; }); }
                                cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
                            }
                        }).detach();
                    }
                    if let Some(m) = ssh_term_proto::managed::get() {
                        match &m.error { Some(e) => { root.log("policy.managed", format!("path={} error={e:?} fail-closed", m.path.display())); root.toast(tf("main.org_policy_error", &[("e", e)]), cx); }
                            None => root.log("policy.managed", format!("path={} hard_deny={} modes={:?} hosts={} retention_floor={:?}", m.path.display(), m.hard_deny.len(), m.mode.allowed, m.hosts.len(), m.audit.retention_days)) }
                    }
                    if let Some(e) = load_err.clone() { root.toast(e, cx); }
                    let st = config::get();
                    let want_restore = (st.general.startup == "restore" || (st.session.restore_layout && st.general.startup != "blank")) && std::env::var("AGENTSHELL_NO_RESTORE").is_err();
                    let restored = want_restore && root.restore_layout(window, cx);
                    // keep layout.json fresh (crash-safe) and save on close
                    cx.spawn(async move |this, cx| loop {
                        cx.background_executor().timer(std::time::Duration::from_secs(5)).await;
                        if this.update(cx, |r, cx| r.save_layout(cx)).is_err() { break; }
                    }).detach();
                    let me = cx.entity().downgrade();
                    window.on_window_should_close(cx, move |w, cx| me.update(cx, |r, cx| r.should_close(w, cx)).unwrap_or(true));
                    if !restored && config::get().general.startup != "blank" {
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

fn shown_title(v: &TermView) -> SharedString { v.shown_title() }
