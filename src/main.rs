#![cfg_attr(windows, windows_subsystem = "windows")]
//! shellax prototype v5 UI: sidebar (host search / groups / function area), tabs with split
//! panes, settings page, command palette, host dialog, tool panels, status bar.
mod actions;
mod app_menu;
mod chrome;
mod motion;
use gpui::AnimationExt as _;
mod filekind;
mod sidebar;
mod activity;
mod db_dialog;
mod db_ui;
mod db_view;
mod kv_view;
mod doc_view;
mod dock_view;
mod kv_ui;
mod host_picker;
mod db_editor;
mod tabs_ui;
mod config;
mod host_dialog;
mod palette;
mod panels;
mod settings_view;
mod setup_view;
mod sftp_view;
mod port_view;
mod host_tools_view;
mod chat_view;
mod library;
mod termview;
mod theme;
mod ui;
mod update_ui;
mod ctxmenu_view;

use ssh_term_proto::i18n::{t, tf};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use gpui::{
    div, prelude::*, px, size, AnyElement, App, Bounds, Context, Entity, FontWeight, Hsla, KeyDownEvent, SharedString,
    Window, WindowAppearance,
};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::agent::{self, AgentState, Manifest};
use ssh_term_proto::audit::AuditLog;
use ssh_term_proto::hosts::{self, HostEntry};
use ssh_term_proto::layout::{auto_direction, Dir, Node};
use ssh_term_proto::policy::Mode;
use ssh_term_proto::sftp_mode::Mode as SftpMode;
use ssh_term_proto::ssh::{self, HostKeyDecision, HostKeyPrompt, SshTarget};
use ssh_term_proto::suggest::History;
use ssh_term_proto::{local, term::Size};

use actions::Action;
use host_dialog::{HostDialog, HostEvent};
use palette::{Item, Palette, PaletteEvent};
use settings_view::{Cat, SettingsEvent, SettingsView};
use termview::{Spec, TermView};
use theme::{c, cur, Appearance, Scheme, Theme};
use ui::{icon, IconName};

pub fn font_size() -> f32 { config::get().font.size.clamp(6.0, 48.0) }
/// Font line metrics (ascent + descent, px at the current font size), measured by the terminal
/// view from the real font (DirectWrite / CoreText / FreeType differ); 0 = not measured yet.
pub static FONT_METRIC_H: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Terminal cell height: font metrics × font.line_height (1.0 = glyph height).
pub fn line_h() -> f32 {
    let f = config::get();
    let m = f32::from_bits(FONT_METRIC_H.load(std::sync::atomic::Ordering::Relaxed));
    let base = if m > 0.0 { m } else { f.font.size.clamp(6.0, 48.0) * 1.2 };
    (base * f.font.line_height.clamp(0.8, 3.0)).round().max(6.0)
}
/// Terminal content padding (terminal.padding, px).
pub fn term_pad() -> f32 { config::get().terminal.padding.clamp(0.0, 40.0) }

/// True when `family` resolves to a font whose glyphs share one advance. The terminal paints every
/// row as a single shaped string and advances cells by `advance('M')` (see `termview::build_row`),
/// so a proportional family — or a family GPUI cannot resolve and silently replaces with the UI
/// default — misaligns the whole grid: letters drift, CJK overlaps ASCII, the cursor lands elsewhere.
fn monospace_ok(cx: &gpui::App, family: &str, size: f32) -> bool {
    let ts = cx.text_system();
    let fid = ts.resolve_font(&gpui::font(SharedString::from(family.to_string())));
    let advance = |ch: char| ts.advance(fid, px(size), ch).ok().map(|a| f32::from(a.width)).unwrap_or(0.0);
    let (i, m, zero) = (advance('i'), advance('M'), advance('0'));
    i > 0.0 && (i - m).abs() < 0.05 && (m - zero).abs() < 0.05
}

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

/// Old dialogs let users type a password into the "private key" field. A value that is not an
/// existing file and does not look like a path is moved into the secret store as that host's
/// password and removed from config.toml (auth becomes "password").
fn migrate_key_field_passwords() {
    let Some(store) = ssh::SECRETS.get() else { return };
    let s = config::get();
    let looks_like_path = |v: &str| v.contains(['/', '\\']) || v.starts_with('~') || v.starts_with('.') || v.ends_with(".pem") || v.ends_with(".key") || v.ends_with(".ppk") || v.starts_with("id_");
    let mut moved = vec![];
    for h in &s.hosts {
        let Some(v) = h.identity_file.as_deref().filter(|v| !v.trim().is_empty()) else { continue };
        if hosts::expand_tilde(v).exists() || looks_like_path(v) { continue; }
        let tg = hosts::resolve(&HostEntry { identity_file: None, ..h.clone() }, &s.known_hosts_path(), &s.all_hosts(), &s.ssh_opts());
        if store.set(&tg.password_secret(), v).is_ok() { moved.push(h.name.clone()); }
    }
    if moved.is_empty() { return; }
    let _ = config::update(|s| for h in s.hosts.iter_mut().filter(|h| moved.contains(&h.name)) {
        h.identity_file = None;
        let a = h.auth.clone().unwrap_or_default();
        if !a.split(',').any(|m| m.trim() == "password") { h.auth = Some("password".into()); }
    });
    eprintln!("moved a password from the private-key field into the secret store for: {}", moved.join(", "));
}

/// Layout file of this window: layout.json, or the file a "move tab to new window" child was
/// started with (SHELLAX_LAYOUT_FILE, PRD 5.23).
pub(crate) fn layout_file() -> std::path::PathBuf {
    std::env::var_os("SHELLAX_LAYOUT_FILE").map(Into::into).unwrap_or_else(|| data_dir().join("layout.json"))
}
/// A secondary window process (moved tab): no policy service / tray / update check of its own.
pub(crate) fn secondary() -> bool { std::env::var_os("SHELLAX_LAYOUT_FILE").is_some() }

pub(crate) fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(Into::into)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"))
        .join("shellax")
}

/// Newest `*.log` directly in `dir` or in `dir/logs` (quick start "Agent + tail -f").
fn newest_log(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    [dir.to_path_buf(), dir.join("logs")].iter().filter_map(|d| std::fs::read_dir(d).ok()).flat_map(|r| r.flatten())
        .filter(|e| e.path().extension().is_some_and(|x| x == "log") && e.path().is_file())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()).map(|e| e.path())
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

struct Tab { layout: Node<Entity<TermView>>, focused: Entity<TermView>, /** custom name (PRD 5.23) */ name: Option<String> }

pub(crate) enum Overlay { None, Palette(Entity<Palette>), Host(Entity<HostDialog>), Audit, Sftp, Port, HostTools(Entity<host_tools_view::HostToolsView>), Db(Entity<db_dialog::DbDialog>), Asset(Entity<kv_ui::AssetDialog>) }

/// Which right-hand card is open. AI, saved commands and history share one slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SideDock { None, Ai, Snippets, History }

pub(crate) struct Root {
    rt: Arc<tokio::runtime::Runtime>,
    default_target: Option<SshTarget>,
    tabs: Vec<Tab>,
    active: usize,
    themes: Vec<Arc<Theme>>,
    schemes: Vec<Arc<Scheme>>,
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
    /// Settings tab, kept alive while other tabs are shown. `None` means the tab is closed;
    /// `Some(false)` means it exists but another tab is in front; `Some(true)` means it is the
    /// active tab. Switching away must not drop it — only its own close button does.
    settings: Option<(Entity<SettingsView>, bool)>,
    host_search: Entity<InputState>,
    lang_seen: u8,
    bg_seen: Option<gpui::WindowBackgroundAppearance>,
    ui_size_seen: f32,
    /// Sidebar context / "+" menu and its dialogs (PRD 5.23).
    sb_menu: Option<sidebar::SbMenu>,
    /// Last open menus, kept for one exit fade after they close (motion::EXIT_MS).
    menu_last: (Option<sidebar::SbMenu>, Option<(usize, gpui::Point<gpui::Pixels>)>),
    menu_ghost: Option<(Option<sidebar::SbMenu>, Option<(usize, gpui::Point<gpui::Pixels>)>, u64)>,
    menu_ghost_seq: u64,
    /// When the sidebar menu opened, so the opening click does not immediately dismiss it.
    sb_menu_at: std::time::Instant,
    sb_modal: Option<sidebar::SbModal>,
    /// Finder tag filter (colors) from the sidebar Tags section.
    tag_filter: Vec<String>,
    /// live sidebar width (px); saved to sidebar.width when a resize drag ends
    sb_width: f32,
    /// Right-hand card: AI, saved commands, or history. Only one is open.
    dock: SideDock,
    lib_search: Entity<InputState>,
    lib_group: String,
    lib_hist_all: bool,
    lib_editor: Option<library::LibEditor>,
    lib_vars: Option<library::LibVars>,
    lib_confirm: Option<library::LibConfirm>,
    lib_notice: String,
    /// live AI dock width (px); saved to sidebar.ai_width when a resize drag ends
    ai_width: f32,
    /// tab right-click menu (tab index, position)
    tab_menu: Option<(usize, gpui::Point<gpui::Pixels>)>,
    tab_menu_at: std::time::Instant,
    /// “+”新建菜单：锚点、打开/关闭时间、菜单内折叠的分组、键盘选中项、搜索框
    new_menu: Option<gpui::Point<gpui::Pixels>>,
    new_menu_at: std::time::Instant,
    new_menu_closed: std::time::Instant,
    new_menu_collapsed: Vec<String>,
    new_menu_sel: usize,
    new_menu_search: Entity<InputState>,
    tab_scroll: gpui::ScrollHandle,
    /// inline tab rename (tab index, input)
    tab_rename: Option<(usize, Entity<InputState>)>,
    /// recently closed tabs, newest last
    closed_tabs: Vec<(ssh_term_proto::session_store::NodeSave, Option<String>)>,
    /// OS id per SSH target label from a one-shot probe (sidebar badge).
    host_os: Arc<Mutex<HashMap<String, String>>>,
    os_probing: HashSet<String>,
    sidebar_visible: bool,
    /// Active module of the left activity strip.
    tool: activity::Tool,
    bottom_open: bool,
    bottom_tab: activity::Bottom,
    bottom_h: f32,
    /// Execution log shown in the bottom tool window (database statements, …).
    exec_log: activity::ExecLog,
    dbp: db_ui::DbPanel,
    /// Sidebar collapse/expand animation in flight: (expanding, seq).
    sb_anim: Option<(bool, u64)>,
    audit: Arc<AuditLog>,
    pub(crate) audit_verify: Option<Result<usize, (usize, String)>>,
    pub(crate) audit_filter: ssh_term_proto::audit::Filter,
    settings_dirty: Vec<String>,
    toast: Option<(String, u64)>,
    /// Toast is playing its exit fade.
    toast_out: bool,
    toast_seq: u64,
    pub(crate) sftp_views: HashMap<String, Entity<sftp_view::SftpView>>,
    pub(crate) port_view: Option<Entity<port_view::PortView>>,
    pub(crate) chat_view: Option<Entity<chat_view::ChatView>>,
    quit_confirm: bool,
    /// Generic confirmation / picker modal (broadcast prod warning, snapshots, recordings, quick start).
    modal: Option<Modal>,
    quit_ok: bool,
    pub(crate) sftp_current: Option<Entity<sftp_view::SftpView>>,
    /// Detached SFTP windows (PRD 5.21).
    sftp_windows: Vec<(Entity<sftp_view::SftpView>, gpui::AnyWindowHandle)>,
    /// Split divider being dragged (drawn thicker).
    drag_div: Option<(usize, Vec<bool>)>,
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

    /// Show / hide the sidebar with a width animation (motion::SIDEBAR_MS). The terminal is
    /// resized once the animation has finished, not on every frame.
    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        let show = !self.sidebar_visible;
        let _ = config::update(|s| s.sidebar.panel_hidden = !show);
        if motion::reduced() { self.sidebar_visible = show; self.sb_anim = None; cx.notify(); return; }
        let seq = self.sb_anim.map(|a| a.1 + 1).unwrap_or(1);
        self.sb_anim = Some((show, seq));
        if show { self.sidebar_visible = true; }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(motion::ms(motion::SIDEBAR_MS)).await;
            let _ = this.update(cx, |r, cx| { if r.sb_anim.is_some_and(|a| a.1 == seq) { r.sb_anim = None; if !show { r.sidebar_visible = false; } cx.notify(); } });
        }).detach();
    }

    pub(crate) fn toast(&mut self, msg: String, cx: &mut Context<Self>) {
        self.toast_after(msg, std::time::Duration::from_secs(4), cx);
    }

    pub(crate) fn toast_after(&mut self, msg: String, dur: std::time::Duration, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let seq = self.toast_seq;
        self.toast = Some((msg, seq));
        self.toast_out = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(dur).await;
            let reduced = motion::reduced();
            let _ = this.update(cx, |r, cx| { if r.toast.as_ref().is_some_and(|t| t.1 == seq) { if reduced { r.toast = None; } else { r.toast_out = true; } cx.notify(); } });
            if reduced { return; }
            cx.background_executor().timer(motion::ms(motion::TOAST_EXIT_MS)).await;
            let _ = this.update(cx, |r, cx| { if r.toast.as_ref().is_some_and(|t| t.1 == seq) { r.toast = None; r.toast_out = false; cx.notify(); } });
        }).detach();
    }

    fn history_for(&mut self, key: &str) -> Arc<Mutex<History>> {
        if !config::get().suggest.local_history { return Arc::new(Mutex::new(History::new())); }
        self.histories.entry(key.to_string()).or_insert_with(|| {
            Arc::new(Mutex::new(History::open(ssh_term_proto::suggest::history_path(&data_dir(), key))))
        }).clone()
    }

    pub(crate) fn dock_is(&self, a: actions::Action) -> bool {
        matches!((self.dock, a), (SideDock::Ai, actions::Action::OpenAi) | (SideDock::Snippets, actions::Action::OpenSnippets) | (SideDock::History, actions::Action::OpenHistory))
    }

    fn toggle_dock(&mut self, which: SideDock, window: &mut Window, cx: &mut Context<Self>) {
        if self.dock == which {
            self.dock = SideDock::None;
            chat_view::set_dock_open(false);
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        self.dock = which;
        if which == SideDock::Ai {
            self.open_chat(window, cx);
        } else {
            chat_view::set_dock_open(false);
            cx.notify();
        }
    }
    fn open_pane(&mut self, spec: Spec, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        self.open_pane_with(spec, PaneInit::default(), window, cx)
    }
    fn open_pane_with(&mut self, spec: Spec, pi: PaneInit, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        #[cfg_attr(not(unix), allow(unused_mut))]
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
                    let line = ssh_term_proto::ai::redact(line).0;
                    this.log("notify.keyword", format!("rule={title:?} line={line:?}"));
                    this.notify_kind(&format!("{title}: {line}"), false, cx);
                    this.toast(format!("🔔 {title}: {line}"), cx);
                    return;
                }
                termview::TermEvent::CtxEditor(path) => {
                    let target = match &pane.read(cx).spec { Spec::Ssh(t) => t.clone(), Spec::Local => return };
                    let v = this.sftp_for(target, w, cx);
                    v.update(cx, |v, cx| v.edit_path(path.clone(), cx));
                    this.sftp_current = Some(v);
                    this.overlay = Overlay::Sftp;
                    cx.notify();
                    return;
                }
                termview::TermEvent::CtxCdTab(dir) => {
                    let spec = pane.read(cx).spec.clone();
                    this.new_tab(spec, w, cx);
                    if let Some(p) = this.tabs.get(this.active).map(|t| t.focused.clone()) { Self::insert_when_ready(p, format!("cd -- {}", ssh_term_proto::ctxmenu::quote_path(dir)), cx); }
                    return;
                }
                termview::TermEvent::CtxAiExplain(text) => {
                    this.log("ai.explain_selection", format!("chars={}", text.chars().count()));
                    let chat = this.open_chat(w, cx);
                    let text = text.clone();
                    chat.update(cx, |c, cx| c.explain_text(text, cx));
                    return;
                }
                termview::TermEvent::CtxUseAi => {
                    let chat = this.open_chat(w, cx);
                    let src = pane.downgrade();
                    chat.update(cx, |c, cx| { c.use_terminal(src); cx.notify(); });
                    this.toast(t("cm.ai_context_set").to_string(), cx);
                    return;
                }
                termview::TermEvent::Copied => { this.toast_after(t("term.copied").to_string(), std::time::Duration::from_secs(2), cx); return; }
                termview::TermEvent::Toast(msg) => { this.toast(msg.clone(), cx); return; }
                termview::TermEvent::RetryConnect => {
                    let spec = pane.read(cx).spec.clone();
                    if let Some(ti) = this.tabs.iter().position(|t| t.layout.leaves().iter().any(|x| x == pane)) {
                        let np = this.open_pane(spec, w, cx);
                        let tab = &mut this.tabs[ti];
                        if tab.layout.replace(pane, np.clone()) { if tab.focused == *pane { tab.focused = np.clone(); } }
                        this.end_pane(pane, cx);
                        let fh = np.read(cx).focus.clone(); w.focus(&fh, cx);
                        cx.notify();
                    }
                    return;
                }
                termview::TermEvent::EditHost => {
                    if let Spec::Ssh(t) = &pane.read(cx).spec {
                        let lbl = t.label();
                        let name = this.host_targets().into_iter().find(|(_, x)| x.label() == lbl).map(|(h, _)| h);
                        this.open_host_dialog(name, None, w, cx);
                    }
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
                let title = tf("main.local_title", &[("shell", &shell_display_name())]);
                let cwd = pi.cwd.as_deref().map(std::path::Path::new);
                #[cfg(unix)]
                if config::get().session.local_daemon {
                    let id = pi.daemon_id.clone().unwrap_or_else(ssh_term_proto::session_store::uuid_like);
                    let sock = ssh_term_proto::daemon::socket_path(&data_dir());
                    let exe = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(format!("shellax-cli{}", std::env::consts::EXE_SUFFIX)))).unwrap_or_else(|| "shellax-cli".into());
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
                // tab title: the saved host's name (alias), not user@host:port
                let lbl = t.label();
                let shown = self.host_targets().into_iter().find(|(_, x)| x.label() == lbl).map(|(h, _)| h.name).unwrap_or_else(|| t.host.clone());
                let title = match &t.tmux { Some(n) => format!("{shown} · tmux:{n}"), None => shown };
                self.log("ssh.connect", format!("{} (known_hosts: {})", t.label(), t.known_hosts.display()));
                let s = ssh::spawn_ssh(self.rt.handle(), t.clone(), init, self.prompts.clone());
                let pv = self.port_view(window, cx);
                pv.update(cx, |v, cx| v.auto_start(&t, cx));
                cx.new(|cx| TermView::new(&title, Spec::Ssh(t), Ok(s), h, m, window, cx))
            }
        }
    }
    /// Proxy was requested and Settings has no usable address. Toast, and optionally open the proxy page.
    /// Returns true when the caller should not open the connection.
    fn block_proxy(&mut self, spec: &Spec, open_settings: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Spec::Ssh(t) = spec else { return false };
        let Some(msg) = t.proxy_block_message() else { return false };
        self.toast(msg, cx);
        if open_settings { self.open_settings(Cat::Proxy, window, cx); }
        true
    }
    fn new_tab(&mut self, spec: Spec, window: &mut Window, cx: &mut Context<Self>) {
        if self.block_proxy(&spec, true, window, cx) { return; }
        let pane = self.open_pane(spec, window, cx);
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(Tab { layout: Node::Leaf(pane.clone()), focused: pane, name: None });
        self.active = self.tabs.len() - 1;
        // A new terminal tab comes to the front, but the settings tab stays open behind it.
        self.leave_settings();
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
                    chat_view::ChatEvent::Close => { this.dock = SideDock::None; chat_view::set_dock_open(false); this.focus_active(w, cx); cx.notify(); }
                    chat_view::ChatEvent::Insert(cmd) => {
                        // AI output is only inserted, never executed (the user presses Enter)
                        if let Some(p) = this.tabs.get(this.active).map(|t| t.focused.clone()) { let cmd = cmd.clone(); p.update(cx, |t, cx| t.ai_insert_text(cmd, cx)); }
                        this.log("ai.chat.insert", cmd);
                    }
                }));
                self.chat_view = Some(v.clone());
                v
            }
        };
        v.update(cx, |c, cx| { c.docked = true; if src.is_some() { c.source = src; } c.focus(window, cx); });
        self.dock = SideDock::Ai;
        chat_view::set_dock_open(true);
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
                let v = match self.sftp_views.get(&key).filter(|v| v.read(cx).mode == SftpMode::Popup) { Some(v) => v.clone(), None => {
                    let v = cx.new(|cx| { let mut v = sftp_view::SftpView::new_deferred(Some(target), rt, pr, window, cx); v.set_initial_dir(dir); v.start(cx); v });
                    self._subs.push(cx.subscribe_in(&v, window, Self::on_sftp_event));
                    self.sftp_views.insert(key, v.clone()); v
                } };
                self.sftp_current = Some(v);
                self.overlay = Overlay::Sftp;
            }
        }
        cx.notify();
    }
    /// SFTP view for a host (cached per pool key), subscribed to its events.
    fn sftp_for(&mut self, t: SshTarget, window: &mut Window, cx: &mut Context<Self>) -> Entity<sftp_view::SftpView> {
        let key = t.pool_key();
        // the cached popup view may have been moved to a tab/split/window: then make a fresh popup one
        if let Some(v) = self.sftp_views.get(&key).filter(|v| v.read(cx).mode == SftpMode::Popup) { return v.clone(); }
        let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
        let v = cx.new(|cx| sftp_view::SftpView::new(Some(t), rt, pr, window, cx));
        self._subs.push(cx.subscribe_in(&v, window, Self::on_sftp_event));
        self.sftp_views.insert(key, v.clone());
        v
    }
    fn on_sftp_event(&mut self, v: &Entity<sftp_view::SftpView>, ev: &sftp_view::SftpEvent, w: &mut Window, cx: &mut Context<Self>) {
        match ev {
            sftp_view::SftpEvent::Close => self.close_overlay(w, cx),
            sftp_view::SftpEvent::Move(m) => self.move_sftp(v.clone(), *m, w, cx),
            sftp_view::SftpEvent::Pane(act) => self.sftp_pane_act(v, *act, w, cx),
            sftp_view::SftpEvent::Retitle => {
                let title: SharedString = v.read(cx).title().into();
                for tab in &self.tabs { for p in tab.layout.leaves() { if p.read(cx).sftp.as_ref() == Some(v) { p.update(cx, |p, cx| { p.title = title.clone(); cx.notify() }); } } }
                if let Some((_, h)) = self.sftp_windows.iter().find(|(w, _)| w == v) { let tt = title.clone(); let _ = h.update(cx, |_, w, _| w.set_window_title(&tt)); }
                self.save_layout(cx);
                cx.notify();
            }
            sftp_view::SftpEvent::Insert { target, command } => {
                let target = target.clone();
                if v.read(cx).mode == SftpMode::Popup { self.close_overlay(w, cx); }
                if v.read(cx).mode == SftpMode::Window { w.activate_window(); }
                let pane = self.pane_for(target, w, cx);
                Self::insert_when_ready(pane, command.clone(), cx);
            }
            sftp_view::SftpEvent::CdTab { target, dir } => {
                let spec = match target { Some(t) => Spec::Ssh(t.clone()), None => Spec::Local };
                if v.read(cx).mode == SftpMode::Popup { self.close_overlay(w, cx); }
                self.new_tab(spec, w, cx);
                if let Some(p) = self.tabs.get(self.active).map(|t| t.focused.clone()) { Self::insert_when_ready(p, format!("cd -- {}", ssh_term_proto::ctxmenu::quote_path(dir)), cx); }
            }
        }
    }
    /// Split or close the terminal pane that is showing this SFTP view.
    fn sftp_pane_act(&mut self, v: &Entity<sftp_view::SftpView>, act: sftp_view::DirAction, window: &mut Window, cx: &mut Context<Self>) {
        let found = self.tabs.iter().enumerate().find_map(|(ti, t)| {
            t.layout.leaves().into_iter().find(|p| p.read(cx).sftp.as_ref() == Some(v)).map(|p| (ti, p))
        });
        let Some((ti, p)) = found else { return };
        self.active = ti;
        self.leave_settings();
        self.tabs[ti].focused = p.clone();
        let fh = p.read(cx).focus.clone();
        window.focus(&fh, cx);
        match act {
            sftp_view::DirAction::Right => self.split(Some(Dir::Vertical), window, cx),
            sftp_view::DirAction::Down => self.split(Some(Dir::Horizontal), window, cx),
            sftp_view::DirAction::Close => self.close_pane(window, cx),
        }
    }
    /// The session for a host (PRD 5.20): focused pane if it matches, else any pane on that host
    /// (its tab is activated), else a new tab.
    fn pane_for(&mut self, target: Option<SshTarget>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let want = |v: &TermView| !v.embedded() && match (&v.spec, &target) { (Spec::Local, None) => true, (Spec::Ssh(t), Some(r)) => t.pool_key() == r.pool_key(), _ => false };
        if let Some(tab) = self.tabs.get(self.active) { if want(tab.focused.read(cx)) { let p = tab.focused.clone(); let fh = p.read(cx).focus.clone(); window.focus(&fh, cx); return p; } }
        let found = self.tabs.iter().enumerate().find_map(|(ti, tab)| tab.layout.leaves().into_iter().find(|p| want(p.read(cx))).map(|p| (ti, p)));
        if let Some((ti, p)) = found {
            self.active = ti; self.leave_settings(); self.tabs[ti].focused = p.clone();
            let fh = p.read(cx).focus.clone(); window.focus(&fh, cx); cx.notify();
            return p;
        }
        self.new_tab(match target { Some(t) => Spec::Ssh(t), None => Spec::Local }, window, cx);
        self.tabs[self.active].focused.clone()
    }
    /// Insert a command (no Enter) once the pane's shell is up.
    fn insert_when_ready(pane: Entity<TermView>, cmd: String, cx: &mut Context<Self>) {
        cx.spawn(async move |_, cx| {
            let mut waited = false;
            for _ in 0..150 {
                let ready = pane.update(cx, |v, _| v.session.as_ref().is_some_and(|s| s.connected.load(std::sync::atomic::Ordering::Relaxed)));
                if ready {
                    if waited { cx.background_executor().timer(std::time::Duration::from_millis(600)).await; }
                    pane.update(cx, |v, cx| v.insert_command(&cmd, cx));
                    return;
                }
                waited = true;
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            }
        }).detach();
    }
    /// Traffic lights for the main window; close goes through the normal quit confirmation.
    fn lights(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        chrome::traffic_lights("lights", window, move |w, cx| { let _ = me.update(cx, |r, cx| if r.should_close(w, cx) { w.remove_window(); cx.quit(); }); })
    }
    // ---- SFTP display modes (PRD 5.21)
    fn new_sftp_view(&mut self, t: Option<SshTarget>, remote: Option<String>, local: Option<String>, panes: Vec<ssh_term_proto::session_store::SftpSide>, window: &mut Window, cx: &mut Context<Self>) -> Entity<sftp_view::SftpView> {
        let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
        let v = cx.new(|cx| {
            let mut v = sftp_view::SftpView::new_deferred(t, rt, pr, window, cx);
            v.set_initial_dir(remote);
            if let Some(l) = &local { v.set_local_dir(l); }
            if !panes.is_empty() { v.restore(&panes, cx); }
            v.start(cx);
            v
        });
        self._subs.push(cx.subscribe_in(&v, window, Self::on_sftp_event));
        v
    }
    /// Wrap an SFTP view in a layout leaf (tab / split).
    fn sftp_pane(&mut self, v: Entity<sftp_view::SftpView>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let key = match v.read(cx).target() { Some(t) => Spec::Ssh(t), None => Spec::Local }.history_key();
        let (h, m) = (self.history_for(&key), self.manifests.clone());
        cx.new(|cx| TermView::new_sftp(v, h, m, window, cx))
    }
    /// After restore: an SFTP leaf saved with follow=on follows the first terminal in its tab.
    fn relink_follow(&self, layout: &Node<Entity<TermView>>, cx: &mut Context<Self>) {
        let leaves = layout.leaves();
        let Some(term) = leaves.iter().find(|p| !p.read(cx).embedded()).cloned() else { return };
        let local = matches!(term.read(cx).spec, Spec::Local);
        for p in &leaves {
            if let Some(sv) = p.read(cx).sftp.clone() {
                let on = sv.read(cx).follow.on;
                sv.update(cx, |v, cx| { v.mode = SftpMode::Split; v.set_follow_source(Some((term.downgrade(), local)), on, cx) });
            }
        }
    }
    /// Open SFTP for `target` (default: focused/any SSH host) in `mode`. Popup reuses the per-host
    /// view; tab/split/window create a new view (several per host are allowed).
    fn open_sftp_mode(&mut self, target: Option<SshTarget>, mode: SftpMode, window: &mut Window, cx: &mut Context<Self>) {
        // no host: left pane local, right pane offers "choose host" (no picker dialog)
        let Some(t) = target.or_else(|| self.ssh_target_for_tools(cx)) else {
            self.log("sftp.open", format!("host=- mode={}", mode.as_str()));
            let v = self.new_sftp_view(None, None, None, vec![], window, cx);
            return self.place_sftp(v, mode, window, cx);
        };
        if self.block_proxy(&Spec::Ssh(t.clone()), true, window, cx) { return; }
        self.log("sftp.open", format!("host={} mode={}", t.label(), mode.as_str()));
        if mode == SftpMode::Popup {
            let v = self.sftp_for(t, window, cx);
            self.detach_sftp(&v, window, cx);
            return self.place_sftp(v, SftpMode::Popup, window, cx);
        }
        let v = self.new_sftp_view(Some(t), None, None, vec![], window, cx);
        self.place_sftp(v, mode, window, cx);
    }
    /// Move an existing view (state, selection, transfer queue kept: same entity) to `mode`.
    fn move_sftp(&mut self, v: Entity<sftp_view::SftpView>, mode: SftpMode, window: &mut Window, cx: &mut Context<Self>) {
        let from = v.read(cx).mode;
        self.log("sftp.mode", format!("host={} from={} to={}", v.read(cx).target().map(|t| t.label()).unwrap_or_else(|| "-".into()), from.as_str(), mode.as_str()));
        self.detach_sftp(&v, window, cx);
        self.place_sftp(v, mode, window, cx);
    }
    /// Remove a view from wherever it is shown (popup, layout leaf, detached window).
    fn detach_sftp(&mut self, v: &Entity<sftp_view::SftpView>, window: &mut Window, cx: &mut Context<Self>) {
        if self.sftp_current.as_ref() == Some(v) { self.sftp_current = None; if matches!(self.overlay, Overlay::Sftp) { self.overlay = Overlay::None; } }
        let mut ti = 0;
        while ti < self.tabs.len() {
            let leaf = self.tabs[ti].layout.leaves().into_iter().find(|p| p.read(cx).sftp.as_ref() == Some(v));
            if let Some(leaf) = leaf {
                if !self.tabs[ti].layout.remove(&leaf) {
                    self.tabs.remove(ti);
                    if self.active >= ti && self.active > 0 { self.active -= 1; }
                    self.active = self.active.min(self.tabs.len().saturating_sub(1));
                    continue;
                }
                self.tabs[ti].focused = self.tabs[ti].layout.leaves()[0].clone();
            }
            ti += 1;
        }
        if let Some(i) = self.sftp_windows.iter().position(|(w, _)| w == v) {
            let (_, h) = self.sftp_windows.remove(i);
            let _ = h.update(cx, |_, w, _| w.remove_window());
        }
        v.update(cx, |v, cx| v.set_follow_source(None, false, cx));
        self.focus_active(window, cx);
    }
    fn place_sftp(&mut self, v: Entity<sftp_view::SftpView>, mode: SftpMode, window: &mut Window, cx: &mut Context<Self>) {
        v.update(cx, |v, cx| { v.mode = mode; cx.notify() });
        match mode {
            SftpMode::Popup => { self.sftp_current = Some(v); self.overlay = Overlay::Sftp; }
            SftpMode::Tab => {
                let pane = self.sftp_pane(v, window, cx);
                let fh = pane.read(cx).focus.clone();
                self.tabs.push(Tab { layout: Node::Leaf(pane.clone()), focused: pane, name: None });
                self.active = self.tabs.len() - 1;
                self.leave_settings();
                self.overlay = Overlay::None;
                window.focus(&fh, cx);
            }
            SftpMode::Split => {
                // anchor: focused terminal (not an SFTP leaf); else a terminal on that host; else a new one
                let t = v.read(cx).target();
                let anchor = self.pane_for(t, window, cx);
                let local = matches!(anchor.read(cx).spec, Spec::Local);
                let pane = self.sftp_pane(v.clone(), window, cx);
                if let Some(ti) = self.tabs.iter().position(|t| t.layout.leaves().contains(&anchor)) {
                    self.active = ti;
                    // side by side unless the terminal is already too narrow for two columns
                    let dir = match anchor.read(cx).session.as_ref().map(|s| s.size()) { Some(s) if s.cols < 90 && s.rows > 30 => Dir::Horizontal, _ => Dir::Vertical };
                    self.tabs[ti].layout.split(&anchor, pane.clone(), dir);
                    self.tabs[ti].focused = anchor.clone();
                }
                let on = config::get().session.sftp_follow_cwd;
                v.update(cx, |v, cx| v.set_follow_source(Some((anchor.downgrade(), local)), on, cx));
                self.overlay = Overlay::None;
                let fh = anchor.read(cx).focus.clone();
                window.focus(&fh, cx);
            }
            SftpMode::Window => {
                let title = v.read(cx).title();
                let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
                let vv = v.clone();
                let opts = chrome::window_options(bounds, Some(title));
                match cx.open_window(opts, move |w, cx| {
                    let inner = vv.clone();
                    w.on_window_should_close(cx, move |_, cx| { inner.update(cx, |_, cx| cx.emit(sftp_view::SftpEvent::Move(SftpMode::Tab))); false });
                    if !chrome::NATIVE_LIGHTS { w.set_client_inset(px(0.)); }
                    let d = cx.new(|_| SftpWindow { view: vv });
                    cx.new(|cx| gpui_component::Root::new(d, w, cx))
                }) {
                    Ok(h) => { self.sftp_windows.push((v, h.into())); self.overlay = Overlay::None; }
                    Err(e) => { self.toast(format!("{e:#}"), cx); self.place_sftp(v, SftpMode::Tab, window, cx); return; }
                }
            }
        }
        cx.notify();
    }
    #[allow(dead_code)]
    fn open_sftp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sftp_current = self.ssh_target_for_tools(cx).map(|t| {
            let key = t.pool_key();
            if let Some(v) = self.sftp_views.get(&key) { return v.clone(); }
            let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
            let v = cx.new(|cx| sftp_view::SftpView::new(Some(t), rt, pr, window, cx));
            self._subs.push(cx.subscribe_in(&v, window, Self::on_sftp_event));
            self.sftp_views.insert(key, v.clone());
            v
        });
        self.overlay = Overlay::Sftp;
    }
    fn split(&mut self, dir: Option<Dir>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.focused_pane(window, cx) else { return self.new_tab(Spec::Local, window, cx) };
        let (spec, sz) = { let v = target.read(cx); (v.spec.clone(), v.session.as_ref().map(|s| s.size())) };
        if self.block_proxy(&spec, true, window, cx) { return; }
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
    /// Replay in a new tab via `shellax-cli play` (real terminal timing, idle capped at 2s).
    fn play_recording(&mut self, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = None;
        let bin = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(format!("shellax-cli{}", std::env::consts::EXE_SUFFIX)))).filter(|p| p.exists()).map(|p| p.display().to_string()).unwrap_or_else(|| "shellax-cli".into());
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
            "cols2" | "agent" => { let a = leaf(self, window, cx); let b = leaf(self, window, cx); sp(Dir::Vertical, a, b) }
            "main3" => { let a = leaf(self, window, cx); let b = leaf(self, window, cx); let c2 = leaf(self, window, cx); sp(Dir::Vertical, a, sp(Dir::Horizontal, b, c2)) }
            "grid4" => { let a = leaf(self, window, cx); let b = leaf(self, window, cx); let c2 = leaf(self, window, cx); let d = leaf(self, window, cx); sp(Dir::Horizontal, sp(Dir::Vertical, a, b), sp(Dir::Vertical, c2, d)) }
            _ => leaf(self, window, cx),
        };
        let focused = layout.leaves()[0].clone();
        self.tabs.push(Tab { layout, focused, name: None });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        self.log("quickstart.open", format!("dir={dir:?} preset={preset}"));
        if preset == "agent" {
            // left: configured Agent 1; right: shell following the newest *.log in dir (or logs/)
            let panes = self.tabs[self.active].layout.leaves();
            let agent_cmd = config::get().agents.quick_launch.first().map(|q| q.command.trim().replace(['\r', '\n'], " ")).filter(|c| !c.is_empty());
            let tail = newest_log(std::path::Path::new(&dir)).map(|p| format!("tail -F -n 100 '{}'", p.display().to_string().replace('\'', "'\\''")));
            self.log("agent.launch", format!("quickstart command={agent_cmd:?} tail={tail:?}"));
            for (pane, cmd) in [(panes[0].clone(), agent_cmd), (panes[1].clone(), tail)] {
                let Some(cmd) = cmd else { continue };
                cx.spawn(async move |_, cx| {
                    for _ in 0..100 {
                        if let Some(s) = pane.update(cx, |v, _| v.session.clone()) { cx.background_executor().timer(std::time::Duration::from_millis(400)).await; s.write(format!("{cmd}\r").into_bytes()); return; }
                        cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                    }
                }).detach();
            }
        }
        self.focus_active(window, cx);
        cx.notify();
    }
    fn render_modal(&mut self, m: Modal, cx: &mut Context<Self>) -> impl IntoElement {
        let th = theme::cur();
        let title = |ic, s: String, col| div().flex().items_center().gap_2().child(icon(ic, 16., col)).child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(s));
        let note = |s: String| div().text_size(px(12.)).text_color(c(th.subtext)).child(s);
        let row = |id: SharedString, main: String, sub: String| div().id(id).px_3().py_2().rounded_md().cursor_pointer().bg(c(th.surface0)).hover(|d| d.bg(c(th.surface1)))
            .flex().flex_col().child(div().text_size(px(13.)).text_color(c(th.text)).child(main)).child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(sub));
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
                    dl = dl.child(div().id(SharedString::from(format!("qs-dir-{i}"))).px_2().py(px(3.)).rounded_md().cursor_pointer().text_size(px(12.))
                        .bg(if sel { c(th.accent) } else { c(th.surface0) }).text_color(if sel { c(th.base) } else { c(th.text) }).child(d.clone())
                        .on_click(cx.listener(move |this, _, _w, cx| { this.modal = Some(Modal::QuickStart { dir: d.clone() }); cx.notify(); })));
                }
                dl = dl.child(div().id("qs-browse").px_2().py(px(3.)).rounded_md().cursor_pointer().text_size(px(12.)).bg(c(th.surface1)).text_color(c(th.text)).child(t("qs.browse"))
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
        let re = regex::Regex::new(r#""cwd":"([^"]+)""#).unwrap();
        for sn in st.snaps { let s = sn.layout.to_string(); for m in re.captures_iter(&s) { v.push(m[1].to_string()); } }
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
    /// Saved form of one tab's split tree (layout file, duplicate, reopen, move to window).
    fn save_node(n: &Node<Entity<TermView>>, cx: &App) -> ssh_term_proto::session_store::NodeSave {
        use ssh_term_proto::session_store::{resume_command, NodeSave, PaneSave};
        match n {
            Node::Leaf(v) => {
                let v = v.read(cx);
                if let Some(sv) = &v.sftp { return NodeSave::Leaf { pane: sv.read(cx).save() }; }
                if let Some(dv) = &v.db { return NodeSave::Leaf { pane: dv.read(cx).save(cx) }; }
                if let Some(e) = &v.embed { if let Some(pane) = e.save(cx) { return NodeSave::Leaf { pane }; } }
                let resume = v.agent_label().map(|a| a.0).or(v.last_agent.clone()).and_then(|a| resume_command(&a)).map(str::to_string).or(v.resume_offer.clone());
                let pane = match &v.spec {
                    Spec::Local => PaneSave::Local { cwd: v.session.as_ref().and_then(|s| s.cwd_probe.lock().unwrap().as_ref().and_then(|f| f())).unwrap_or_default(), daemon_id: v.daemon_id.clone(), resume },
                    Spec::Ssh(t) => PaneSave::Ssh { target: t.clone(), resume },
                };
                NodeSave::Leaf { pane }
            }
            Node::Split { dir, ratio, a, b } => NodeSave::Split { vertical: *dir == Dir::Vertical, ratio: *ratio, a: Box::new(Self::save_node(a, cx)), b: Box::new(Self::save_node(b, cx)) },
        }
    }
    fn snapshot(&self, cx: &App) -> ssh_term_proto::session_store::LayoutSave {
        let names: Vec<Option<String>> = self.tabs.iter().map(|t| t.name.clone()).collect();
        ssh_term_proto::session_store::LayoutSave { version: 1, saved_at: chrono::Local::now().to_rfc3339(), active: self.active, tabs: self.tabs.iter().map(|t| Self::save_node(&t.layout, cx)).collect(),
            names: if names.iter().any(|n| n.is_some()) { names } else { vec![] } }
    }
    pub(crate) fn save_layout(&self, cx: &App) {
        let l = self.snapshot(cx);
        if l.tabs.is_empty() { return; }
        if let Err(e) = ssh_term_proto::session_store::save(&layout_file(), &l) { eprintln!("layout save: {e:#}"); }
        if let Ok(v) = serde_json::to_value(&l) {
            let mut st = ssh_term_proto::snapshot::Store::load(&Self::snapshots_path());
            let same = st.snaps.first().is_some_and(|f| { let (mut a, mut b) = (f.layout.clone(), v.clone()); a["saved_at"] = serde_json::Value::Null; b["saved_at"] = serde_json::Value::Null; a == b });
            if !same && st.push(ssh_term_proto::snapshot::Snap { at: chrono::Local::now().timestamp(), name: format!("auto · {} tabs", l.tabs.len()), layout: v }, 10) { let _ = st.save(&Self::snapshots_path()); }
        }
    }
    /// Rebuild tabs from layout.json. Shells start fresh in the saved cwd; nothing is re-run.
    fn restore_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(l) = ssh_term_proto::session_store::load(&layout_file()) else { return false };
        if l.tabs.is_empty() { return false; }
        self.restore_from(l, window, cx)
    }
    pub(crate) fn build_node(&mut self, n: &ssh_term_proto::session_store::NodeSave, window: &mut Window, cx: &mut Context<Self>) -> Node<Entity<TermView>> {
        use ssh_term_proto::session_store::{NodeSave, PaneSave};
        let r = self;
        match n {
            NodeSave::Leaf { pane } => Node::Leaf(match pane.clone() {
                PaneSave::Local { cwd, daemon_id, resume } => r.open_pane_with(Spec::Local, PaneInit { cwd: (!cwd.is_empty()).then_some(cwd), daemon_id, resume }, window, cx),
                PaneSave::Ssh { target, resume } => r.open_pane_with(Spec::Ssh(target), PaneInit { resume, ..Default::default() }, window, cx),
                PaneSave::Sftp { target, local_dir, remote_dir, follow, panes } => {
                    let target = (!target.host.is_empty()).then_some(target);
                    let v = r.new_sftp_view(target, (!remote_dir.is_empty()).then_some(remote_dir), (!local_dir.is_empty()).then_some(local_dir), panes, window, cx);
                    v.update(cx, |v, _| { v.mode = SftpMode::Tab; v.follow.set(follow); });
                    r.sftp_pane(v, window, cx)
                }
                PaneSave::Db { conn, database, schema, table, sql, editor_h } => {
                    let p = r.db_pane(crate::db_view::Target { conn, database, schema, table }, sql, window, cx);
                    if let (Some(h), Some(v)) = (editor_h, p.read(cx).db.clone()) { v.update(cx, |v, _| v.set_editor_h(h)); }
                    p
                }
                PaneSave::Redis { conn, key } => r.redis_pane(&conn, key, window, cx),
                PaneSave::Docker { endpoint } => r.docker_pane(&endpoint, window, cx),
                PaneSave::DockerLogs { endpoint, container, name } => r.docker_logs_pane(&endpoint, container, name, window, cx),
                PaneSave::Mongo { conn, database, collection, filter, sort } => r.mongo_pane(&conn, (database, collection, filter, sort), window, cx),
            }),
            NodeSave::Split { vertical, ratio, a, b } => Node::Split { dir: if *vertical { Dir::Vertical } else { Dir::Horizontal }, ratio: *ratio, a: Box::new(r.build_node(a, window, cx)), b: Box::new(r.build_node(b, window, cx)) },
        }
    }
    fn restore_from(&mut self, l: ssh_term_proto::session_store::LayoutSave, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if l.tabs.is_empty() { return false; }
        for (i, n) in l.tabs.iter().enumerate() {
            let layout = self.build_node(n, window, cx);
            self.relink_follow(&layout, cx);
            let focused = layout.leaves()[0].clone();
            self.tabs.push(Tab { layout, focused, name: l.names.get(i).cloned().flatten() });
        }
        self.active = l.active.min(self.tabs.len() - 1);
        self.leave_settings();
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
    /// 退出并更新：启动安装或替换流程后退出；无法自动安装时显示安装包并提示。
    /// “发现新版本”通知卡片：关闭、稍后、下载更新 / 退出并更新、查看更新说明。下载进度就地显示。
    fn update_offer_el(&mut self, m: ssh_term_proto::update::Manifest, cx: &mut Context<Self>) -> impl IntoElement {
        use ssh_term_proto::updater::Phase;
        let th = theme::cur();
        let notes_url = format!("https://github.com/{}/releases/tag/v{}", ssh_term_proto::update::REPO, m.version.trim_start_matches('v'));
        let ready = update_ui::ready_for(&m.version);
        let phase = update_ui::phase();
        let mut status: Option<(String, bool)> = None;
        let mut downloading = false;
        match &phase {
            Phase::Downloading { version, progress } if *version == m.version => {
                downloading = true;
                let done = progress.done.load(Ordering::Relaxed);
                let total = progress.total.load(Ordering::Relaxed);
                let p = if total > 0 { format!("{}%", done * 100 / total) } else { "…".into() };
                status = Some((tf("update.downloading", &[("v", &m.version), ("p", &p), ("s", &update_ui::human(done))]), false));
            }
            Phase::Failed { version, error } if *version == m.version => status = Some((update_ui::friendly(error), true)),
            _ => {}
        }
        let mut actions = div().flex().items_center().gap_2().justify_end()
            .child(div().id("upd-notes").text_size(px(ui::FS_SECONDARY)).text_color(c(th.accent)).cursor_pointer().hover(|s| s.underline())
                .child(t("update.view_notes")).on_click(move |_, _w, cx| cx.open_url(&notes_url)))
            .child(div().flex_1());
        if downloading {
            actions = actions.child(ui::button("upd-offer-cancel", t("ui.cancel"), 0).on_click(cx.listener(|_, _, _w, cx| { update_ui::cancel(); cx.notify(); })));
        } else {
            actions = actions.child(ui::button("upd-offer-later", t("update.later"), 0).on_click(cx.listener(|_, _, _w, cx| { update_ui::dismiss_offer(true); cx.notify(); })));
            if let Some(rec) = ready {
                actions = actions.child(ui::button("upd-offer-quit", t("update.quit_and_update"), 1).on_click(cx.listener(move |this, _, w, cx| { update_ui::dismiss_offer(false); this.apply_update(&rec, w, cx); })));
            } else if ssh_term_proto::updater::downloadable(&m) {
                let mm = m.clone();
                let label = if status.as_ref().is_some_and(|s| s.1) { t("update.retry") } else { t("update.download") };
                actions = actions.child(ui::button("upd-offer-dl", label, 1).on_click(cx.listener(move |_, _, _w, cx| { update_ui::start(mm.clone(), true); cx.notify(); })));
            } else {
                let url = if m.url.is_empty() { format!("https://github.com/{}/releases/latest", ssh_term_proto::update::REPO) } else { m.url.clone() };
                actions = actions.child(ui::button("upd-offer-open", t("settings.update_download"), 1).on_click(move |_, _w, cx| cx.open_url(&url)));
            }
        }
        div().id("upd-offer").absolute().bottom(px(40.)).right(px(16.)).w(px(380.)).p_4().rounded(px(ui::R_LG)).bg(c(th.base)).border_1().border_color(c(th.surface1)).shadow_lg()
            .flex().flex_col().gap_3().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().flex().items_center().gap_2()
                .child(icon(IconName::Download, 16., c(th.accent)))
                .child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(tf("main.update_available", &[("v", &m.version)])))
                .child(div().id("upd-offer-x").p_1().rounded(px(ui::R_MD)).cursor_pointer().hover(|s| s.bg(c(th.surface0)))
                    .child(icon(IconName::X, 14., c(th.overlay1)))
                    .on_click(cx.listener(|_, _, _w, cx| { update_ui::dismiss_offer(true); cx.notify(); }))))
            .child(div().text_size(px(ui::FS_SECONDARY)).text_color(c(th.subtext)).child(tf("update.offer_body", &[("cur", &ssh_term_proto::update::CURRENT), ("v", &m.version)])))
            .children(status.map(|(msg, err)| div().text_size(px(12.)).text_color(c(if err { th.error } else { th.overlay1 })).child(msg)))
            .child(actions)
    }

    pub(crate) fn apply_update(&mut self, rec: &ssh_term_proto::updater::Record, w: &mut Window, cx: &mut Context<Self>) {
        match ssh_term_proto::updater::apply(rec) {
            Ok(ssh_term_proto::updater::Apply::QuitNow) => {
                self.log("update.apply", &rec.version);
                self.quit_ok = true; self.save_layout(cx);
                for v in self.tabs.iter().flat_map(|t| t.layout.leaves()) { if let Some(s) = &v.read(cx).session { if v.read(cx).daemon_id.is_none() { s.close(); } } }
                w.remove_window(); cx.quit();
            }
            Ok(ssh_term_proto::updater::Apply::Revealed) => self.toast_after(t("update.revealed").into(), std::time::Duration::from_secs(8), cx),
            Err(e) => self.toast_after(tf("update.apply_failed", &[("e", &format!("{e:#}"))]), std::time::Duration::from_secs(8), cx),
        }
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
    /// Terminal colors follow the active UI theme. Switching Daylight / Midnight / Catppuccin
    /// switches the palette with it; there is no separate scheme selection.
    fn resolve_scheme(&self, _window: &Window, ui: &Theme) -> Arc<Scheme> {
        Arc::new(ui.to_scheme())
    }
    fn apply_theme(&self, window: &Window, cx: &mut App) {
        let mut th = self.themes[self.theme_index(window)].clone();
        // macOS themes: accent follows the system accent color (default macOS blue)
        if theme::is_macos_theme(&th) { if let Some(a) = theme::system_accent(th.appearance == Appearance::Dark) { let mut t = (*th).clone(); t.accent = a; th = Arc::new(t); } }
        let sc = self.resolve_scheme(window, &th);
        theme::set_current(th.clone());
        theme::set_scheme(sc);
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
            if let Err(e) = notify_rust::Notification::new().appname("ShellAX").summary("ShellAX").body(&body).show() { eprintln!("notification: {e}"); }
        });
    }

    fn answer_approval(&mut self, ans: ssh_term_proto::policy_server::Answer, cx: &mut Context<Self>) {
        if !self.approvals.is_empty() {
            let a = self.approvals.remove(0);
            if a.reply.send(ans).is_err() {
                // the caller is gone and the request was already denied: a late answer executes nothing
                self.log("hook.approval", format!("id={} late-answer-ignored (approval withdrawn)", a.id));
                self.toast(t("main.approval_late").into(), cx);
            }
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
    /// The settings tab is in front. A real modal (palette, host dialog, …) covers it.
    fn settings_active(&self) -> bool {
        self.settings.as_ref().is_some_and(|(_, on)| *on) && matches!(self.overlay, Overlay::None)
    }
    /// Another tab comes to the front. The settings tab stays open, it just stops being the one shown.
    fn leave_settings(&mut self) {
        if let Some((_, on)) = &mut self.settings { *on = false; }
    }
    /// Close the settings tab itself (its close button or Done), and log the changes made while it lived.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_none() { return; }
        if !self.settings_dirty.is_empty() {
            let mut paths = std::mem::take(&mut self.settings_dirty);
            paths.sort();
            paths.dedup();
            self.log("settings.change", paths.join(", "));
        }
        self.settings = None;
        self.focus_active(window, cx);
        cx.notify();
    }
    pub(crate) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        if !self.settings_active() { self.focus_active(window, cx); }
        cx.notify();
    }
    pub(crate) fn open_settings(&mut self, cat: Cat, window: &mut Window, cx: &mut Context<Self>) {
        // Already a tab: bring it to the front and switch category, keeping search and edits.
        if let Some((v, on)) = &mut self.settings {
            *on = true;
            let v = v.clone();
            v.update(cx, |s, cx| s.select_category(cat, window, cx));
            cx.notify();
            return;
        }
        let names = self.themes.iter().map(|t| (t.name.clone(), t.appearance == Appearance::Dark)).collect();
        let schemes = self.schemes.iter().map(|s| (s.name.clone(), s.appearance == Appearance::Dark)).collect();
        let v = cx.new(|cx| SettingsView::new(cat, names, schemes, window, cx));
        self._subs.push(cx.subscribe_in(&v, window, |this, _, ev: &SettingsEvent, window, cx| match ev {
            SettingsEvent::Close => this.close_settings(window, cx),
            SettingsEvent::ApplyUpdate(rec) => this.apply_update(rec, window, cx),
            SettingsEvent::OpenAudit => { this.leave_settings(); this.overlay = Overlay::Audit; }
            SettingsEvent::ThemesChanged => {
                this.themes = theme::load_all();
                this.schemes = theme::load_schemes(&this.themes);
                this.apply_theme(window, cx);
                cx.notify();
            }
            SettingsEvent::Changed(path) => {
                this.settings_dirty.push(path.clone());
                if path == "suggest.clear_history" { for h in this.histories.values() { h.lock().unwrap().clear(); } }
                this.apply_theme(window, cx);
                this.refresh_all(cx);
            }
        }));
        v.update(cx, |s, cx| s.focus_search(window, cx));
        self.settings = Some((v, true));
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
    fn open_host_dialog(&mut self, existing: Option<HostEntry>, group: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (prompts, rt) = (self.prompts.clone(), self.rt.handle().clone());
        let d = cx.new(|cx| HostDialog::new(existing, group, prompts, rt, window, cx));
        self._subs.push(cx.subscribe_in(&d, window, |this, _, ev: &HostEvent, window, cx| {
            match ev {
                HostEvent::Saved(n) => { this.log("host.save", n); this.toast(tf("main.host_saved", &[("n", &n)]), cx); this.close_overlay(window, cx); }
                HostEvent::Deleted(n) => { this.log("host.delete", n); this.toast(tf("main.host_deleted", &[("n", &n)]), cx); this.close_overlay(window, cx); }
                HostEvent::Close => this.close_overlay(window, cx),
                HostEvent::OpenProxySettings => this.open_settings(Cat::Proxy, window, cx),
            }
        }));
        self.overlay = Overlay::Host(d);
        cx.notify();
    }

    fn run(&mut self, a: Action, window: &mut Window, cx: &mut Context<Self>) {
        match a {
            Action::CommandPalette => self.open_palette(window, cx),
            Action::OpenSettings => self.open_settings(Cat::General, window, cx),
            Action::NewLocalTab => self.new_tab(Spec::Local, window, cx),
            Action::NewHost => self.open_host_dialog(None, None, window, cx),
            Action::SplitAuto => self.split(None, window, cx),
            Action::SplitRight => self.split(Some(Dir::Vertical), window, cx),
            Action::SplitDown => self.split(Some(Dir::Horizontal), window, cx),
            Action::ClosePane => self.close_pane(window, cx),
            Action::NextTab | Action::PrevTab => {
                // The settings tab is one stop in the cycle, between the last terminal and the first.
                let n = self.tabs.len();
                let forward = a == Action::NextTab;
                if self.settings_active() {
                    self.leave_settings();
                    if n > 0 { self.active = if forward { 0 } else { n - 1 }; }
                    self.focus_active(window, cx);
                } else if n > 0 {
                    let at_end = if forward { self.active + 1 >= n } else { self.active == 0 };
                    if at_end && self.settings.is_some() {
                        if let Some((_, on)) = &mut self.settings { *on = true; }
                    } else {
                        self.active = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
                    }
                    if !self.settings_active() { self.focus_active(window, cx); }
                }
            }
            Action::CycleTheme => self.cycle_theme(window, cx),
            Action::ThemeSystem => { self.report(config::update(|s| s.appearance.theme = "system".into()), cx); self.apply_theme(window, cx); self.refresh_all(cx); }
            Action::CyclePermissionMode => { let mut m = config::get().permissions.mode.next(); if let Some(o) = ssh_term_proto::managed::get() { for _ in 0..3 { if o.mode_allowed(m) { break; } m = m.next(); } } self.set_mode(m, cx); }
            Action::ModeAsk => self.set_mode(Mode::Ask, cx),
            Action::ModePlan => self.set_mode(Mode::Plan, cx),
            Action::ModeAuto => self.set_mode(Mode::Auto, cx),
            Action::OpenSftp => { let m = SftpMode::parse(&config::get().session.sftp_mode); self.open_sftp_mode(None, m, window, cx) }
            Action::OpenSftpPopup => self.open_sftp_mode(None, SftpMode::Popup, window, cx),
            Action::OpenSftpTab => self.open_sftp_mode(None, SftpMode::Tab, window, cx),
            Action::OpenSftpSplit => self.open_sftp_mode(None, SftpMode::Split, window, cx),
            Action::OpenSftpWindow => self.open_sftp_mode(None, SftpMode::Window, window, cx),
            Action::OpenHostTools => {
                let tg = self.focused_pane(window, cx).and_then(|p| match &p.read(cx).spec { Spec::Ssh(t) => Some(t.clone()), _ => None });
                let (rt, pr) = (self.rt.handle().clone(), self.prompts.clone());
                let v = cx.new(|cx| host_tools_view::HostToolsView::new(tg, rt, pr, cx));
                self._subs.push(cx.subscribe_in(&v, window, |this, _, _ev: &host_tools_view::HostToolsEvent, w, cx| this.close_overlay(w, cx)));
                self.overlay = Overlay::HostTools(v);
            }
            Action::OpenPortForward => { let tg = self.ssh_target_for_tools(cx); let pv = self.port_view(window, cx); pv.update(cx, |v, _| if v.target.is_none() || tg.is_some() { v.target = tg }); self.overlay = Overlay::Port; }
            Action::OpenAi => self.toggle_dock(SideDock::Ai, window, cx),
            Action::OpenSnippets => self.toggle_dock(SideDock::Snippets, window, cx),
            Action::OpenHistory => self.toggle_dock(SideDock::History, window, cx),
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
            Action::ToolHosts | Action::ToolSftp | Action::ToolDatabase | Action::ToolRedis | Action::ToolDocker | Action::ToolPorts | Action::ToolSnippets => {
                if let Some(t) = activity::Tool::from_action(a) { self.select_tool(t, cx); }
            }
            Action::ToggleBottomPanel => self.toggle_bottom(None, cx),
            Action::NewDbConnection => self.open_db_dialog(None, window, cx),
            Action::OpenAbout => self.open_settings(Cat::About, window, cx),
            Action::ClearHistory => {
                for h in self.histories.values() { h.lock().unwrap().clear(); }
                let dir = data_dir().join("history");
                if let Ok(d) = std::fs::read_dir(&dir) { for e in d.flatten() { let _ = std::fs::write(e.path(), ""); } }
                self.toast(t("ui.command_suggestion_history_cleared").into(), cx);
            }
            Action::ToggleSidebar => self.toggle_sidebar(cx),
            Action::SettingsPermissions => self.open_settings(Cat::Permissions, window, cx),
            Action::SettingsAppearance => self.open_settings(Cat::Appearance, window, cx),
            Action::SettingsTerminal => self.open_settings(Cat::Terminal, window, cx),
            Action::SettingsSsh => self.open_settings(Cat::Ssh, window, cx),
            Action::SettingsKeys => self.open_settings(Cat::Keys, window, cx),
            // ⌘Q / 菜单「退出」：走和关闭窗口同一条确认路径（有运行中的 Agent 时先问一句）。
            Action::Quit => {
                if self.should_close(window, cx) {
                    self.log("app.quit", "quit command");
                    window.remove_window();
                    cx.quit();
                }
            }
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
        t.font_family = theme::ui_font();
        t.radius = px(6.);
        t.focus_ring = false;
        t.shadow = false;
    });
    cx.refresh_windows();
}

impl Root {
    /// Current directory of the focused terminal, as a clickable path. Sits in the
    /// card's bottom corner band so the rounded edge stays covered.
    fn cwd_bar(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let th = cur();
        let pane = self.focused_pane(window, cx);
        if !config::get().terminal.show_cwd { return None; }
        let cwd = pane.as_ref().and_then(|p| p.read(cx).shell_cwd()).filter(|p| !p.is_empty());
        let crumbs = cwd.as_deref().map(termview::cwd_crumbs).unwrap_or_default();
        if crumbs.is_empty() { return None; }
        let mut row = div().id("cwd-bar").h(px(28.)).flex_shrink_0().px_2().flex().items_center().gap(px(2.)).overflow_x_scroll()
            .bg(c(th.mantle)).rounded_bl(px(12.)).rounded_br(px(12.));
        let last = crumbs.len().saturating_sub(1);
        for (i, (label, path)) in crumbs.into_iter().enumerate() {
            if i > 0 { row = row.child(icon(IconName::ChevronRight, 11., c(th.overlay0))); }
            let pane = pane.clone();
            let here = i == last;
            row = row.child(div().id(("cwd", i)).h(px(24.)).px_1().flex_shrink_0().rounded(px(4.)).flex().items_center().gap(px(3.)).cursor_pointer()
                .hover(|d| d.bg(c(cur().surface0)))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    let Some(pane) = pane.clone() else { return };
                    let path = path.clone();
                    pane.update(cx, |v, cx| v.cd_to(&path, cx));
                })
                .child(icon(IconName::Folder, 12., c(th.overlay1)))
                .child(div().text_size(px(12.)).text_color(if here { c(th.text) } else { c(th.subtext) }).whitespace_nowrap().child(label)));
        }
        Some(row.into_any_element())
    }

    fn render_node(&self, tab: usize, node: &Node<Entity<TermView>>, path: Vec<bool>, th: &Theme, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf(p) => {
                let pg = SharedString::from(format!("pane-g-{}", p.entity_id()));
                let mut d = div().relative().group(pg.clone()).flex_1().size_full().flex().overflow_hidden().child(p.clone());
                // new split pane: brief fade-in overlay (no size animation, so the PTY is resized only once)
                if !motion::reduced() && self.tabs.get(tab).is_some_and(|t| t.layout.leaves().len() > 1) {
                    d = d.child(div().absolute().top_0().left_0().size_full().bg(c(th.bg))
                        .with_animation(("pane-in", p.entity_id().as_u64() as usize), gpui::Animation::new(motion::ms(motion::APPEAR_MS)).with_easing(gpui::ease_out_quint()), |d, t| d.opacity(1. - t)));
                }
                if !cx.has_active_drag() && !p.read(cx).embedded() { d = d.child(self.pane_header(p, pg, cx)); }
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
                // 1px hairline; 5px hit area; thickens (3px accent) on hover / while dragging
                let dg = SharedString::from(format!("{id}-dg"));
                let dragging = cx.has_active_drag() && self.drag_div.as_ref().is_some_and(|(t, p)| *t == tab && *p == path);
                let line = div().bg(if dragging { c(th.accent) } else { c(th.overlay1) }).group_hover(dg.clone(), |s| s.bg(c(cur().accent).opacity(0.8)));
                let line = if vertical { line.h_full().w(px(if dragging { 3. } else { 1. })).group_hover(dg.clone(), |s| s.w(px(3.))) } else { line.w_full().h(px(if dragging { 3. } else { 1. })).group_hover(dg.clone(), |s| s.h(px(3.))) };
                let divider = div().id(SharedString::from(format!("{id}-div"))).group(dg).flex_shrink_0().flex().items_center().justify_center().bg(c(theme::scheme().bg)).child(line)
                    .on_drag(drag, |d: &DividerDrag, _, _, cx| cx.new(|_| d.clone()));
                let divider = if vertical { divider.w(px(5.)).h_full().cursor_col_resize() } else { divider.h(px(5.)).w_full().cursor_row_resize() };
                let me_path = path.clone();
                d.on_drag_move(cx.listener(move |this, ev: &gpui::DragMoveEvent<DividerDrag>, _w, cx| {
                    let dd = ev.drag(cx).clone();
                    if dd.path != me_path || dd.tab != tab { return; }
                    let b = ev.bounds;
                    let r = if vertical { (ev.event.position.x - b.left()) / b.size.width } else { (ev.event.position.y - b.top()) / b.size.height };
                    this.drag_div = Some((tab, dd.path.clone()));
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
            // the native menu is built from these strings; rebuild it in the new language.
            // Deferred: set_menus talks to AppKit, so keep it out of the paint pass.
            cx.defer(|cx| app_menu::refresh(cx));
        }
        let th = cur();
        let s = config::get();
        if !cx.has_active_drag() { self.drag_div = None; self.save_sb_width(); self.save_ai_width(); self.save_bottom_h(); }
        self.probe_os(cx);
        // appearance.opacity: transparent window background, terminal area drawn with alpha
        let op = s.appearance.opacity.clamp(0.5, 1.0);
        let want = if chrome::NATIVE_LIGHTS { gpui::WindowBackgroundAppearance::Blurred } else if op < 0.999 { gpui::WindowBackgroundAppearance::Transparent } else { gpui::WindowBackgroundAppearance::Opaque };
        if self.bg_seen != Some(want) { self.bg_seen = Some(want); window.set_background_appearance(want); }
        let ui = s.appearance.ui_font_size.clamp(10., 20.);
        if self.ui_size_seen != ui { self.ui_size_seen = ui; gpui_component::Theme::global_mut(cx).font_size = px(ui); }
        // Settings is a tab of its own. It stays in the tab strip while other tabs are in front,
        // and only its close button (or Done) drops it.
        let settings_tab = self.settings.clone();
        let settings_on = self.settings_active();

        // 标签栏即标题栏：没有单独的标题行。标签间隙、“+”右侧空白是窗口拖动区。
        let mac = chrome::style() == chrome::Style::Mac;
        let me = cx.entity().downgrade();
        let win_ctl = chrome::window_controls("winctl", window, move |w, cx| { let _ = me.update(cx, |r, cx| if r.should_close(w, cx) { w.remove_window(); cx.quit(); }); });

        // Modern pill tab strip: bar blends with the title bar (crust), active tab is one soft
        // elevated pill (no border / accent bar), inactive tabs are bare text with a faint hover fill.
        let dark_ui = th.appearance == theme::Appearance::Dark;
        let pill_bg = if dark_ui { c(th.surface0) } else { gpui::white() };
        let hover_bg = c(th.text).opacity(0.05);
        let pill_shadow = vec![gpui::BoxShadow { color: gpui::black().opacity(if dark_ui { 0.28 } else { 0.07 }), offset: gpui::point(px(0.), px(1.)), blur_radius: px(2.), spread_radius: px(0.), inset: false }];
        // 标签之间的间隙：拖动区（Windows 上为 HTCAPTION，其他平台由整行 drag_region 处理）
        let gap = |sep: bool| div().w(px(5.)).h(px(28.)).flex_shrink_0().flex().items_center().justify_center()
            .window_control_area(gpui::WindowControlArea::Drag)
            .when(sep, |d| d.child(div().w(px(1.)).h(px(14.)).bg(c(th.text).opacity(0.08))));
        // 标签行没有自己的底色，直接落在窗口外层背景上
        let mut row = chrome::drag_region(div().id("tabbar")).h(px(chrome::TAB_H)).flex_shrink_0().flex().items_center().overflow_hidden();
        // 侧栏折叠时标签栏到达窗口左缘：为红黄绿按钮留位
        row = if mac && !self.sidebar_visible { row.child(self.lights(window, cx)) } else { row.pl(px(6.)) };
        let mut tabbar = div().id("tab-scroll").h_full().min_w_0().flex_shrink(1.).flex().items_center().overflow_x_scroll().track_scroll(&self.tab_scroll);
        row = row.child(div().id("sb-collapse").size(px(28.)).flex_shrink_0().rounded(px(7.)).flex().items_center().justify_center().cursor_pointer()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .hover(move |d| d.bg(hover_bg))
            .tooltip(|w, cx| gpui_component::tooltip::Tooltip::new(t("ui.show_hide_sidebar")).build(w, cx))
            .on_click(cx.listener(|this, _, _w, cx| { this.toggle_sidebar(cx); cx.notify(); }))
            .child(icon(IconName::PanelLeft, 14., c(th.overlay1))));
        // base titles first, so identical ones can get a " (2)", " (3)" suffix
        let base_titles: Vec<SharedString> = self.tabs.iter().map(|t| {
            let panes = t.layout.leaves();
            match &t.name {
                Some(n) => n.clone().into(),
                None => { let first = shown_title(panes[0].read(cx)); if panes.len() > 1 { format!("{first} +{}", panes.len() - 1).into() } else { first } }
            }
        }).collect();
        for (i, t) in self.tabs.iter().enumerate() {
            let active = i == self.active && !settings_on;
            let panes = t.layout.leaves();
            let nth = base_titles[..i].iter().filter(|x| **x == base_titles[i]).count();
            let title: SharedString = if nth > 0 { format!("{} ({})", base_titles[i], nth + 1).into() } else { base_titles[i].clone() };
            let tip: Option<String> = match &panes[0].read(cx).spec { Spec::Ssh(st) => Some(st.label()), _ => None };
            let agent = panes.iter().filter_map(|p| p.read(cx).agent_label())
                .max_by_key(|(_, st)| match st { AgentState::Blocked => 3, AgentState::Working => 2, AgentState::Idle => 1, _ => 0 });
            // hairline separator only between two inactive tabs
            if i > 0 { tabbar = tabbar.child(gap(!active && (settings_on || i != self.active + 1))); }
            let tg = SharedString::from(format!("tab-g-{i}"));
            let mut tabel = div().id(("tab", i)).relative().group(tg.clone()).h(px(28.)).w(px(TAB_W)).flex_shrink(1.).min_w(px(TAB_MIN_W)).pl(px(10.)).pr(px(4.)).rounded(px(7.)).flex().items_center().gap(px(6.)).cursor_pointer()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(active, |d| d.bg(pill_bg).shadow(pill_shadow.clone()).rounded_tl(px(10.)).rounded_tr(px(10.)))
                .when_some(tip, |d, tip| d.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx)))
                .when(!active, |d| d.hover(move |s| s.bg(hover_bg)))
                .on_click(cx.listener({ let title = title.to_string(); move |this, ev: &gpui::ClickEvent, w, cx| {
                    this.active = i;
                    // Switching to a terminal tab only puts it in front; the settings tab stays open.
                    this.leave_settings();
                    if ev.click_count() >= 2 { this.start_rename(i, title.clone(), w, cx); } else { this.focus_active(w, cx); }
                    cx.notify();
                } }))
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| { cx.stop_propagation(); this.tab_menu_at = std::time::Instant::now(); this.tab_menu = Some((i, ev.position)); cx.notify(); }))
                .on_mouse_down(gpui::MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(gpui::MouseButton::Middle, cx.listener(move |this, _, w, cx| {
                    cx.stop_propagation();
                    this.close_tab(i, w, cx);
                }))
                .child({
                    // small monochrome kind icon: terminal / ssh / sftp
                    let v = panes[0].read(cx);
                    let ic = if matches!(v.embed, Some(termview::Embed::Kv(_))) { IconName::Database } else if v.embed.is_some() { IconName::Container } else if v.db.is_some() { IconName::Database } else if v.sftp.is_some() { IconName::FolderSync } else if matches!(v.spec, Spec::Ssh(_)) { IconName::Server } else { IconName::SquareTerminal };
                    icon(ic, 13., if active { c(th.subtext) } else { c(th.overlay1) })
                })
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(if active { c(th.text) } else { c(th.overlay1) }).when(active, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(match &self.tab_rename { Some((j, inp)) if *j == i => gpui_component::Sizable::with_size(gpui_component::input::Input::new(inp).appearance(false), gpui_component::Size::XSmall).into_any_element(), _ => title.clone().into_any_element() }));
            if let Some((name, st)) = agent {
                let col = state_color(st, &th);
                tabel = tabel.child(div().px(px(6.)).rounded_md().bg(col.opacity(0.18)).text_color(col).text_size(px(11.)).child(format!("● {name} {}", st.label())));
            }
            let drag = TabDrag { idx: i, title: title.clone() };
            tabel = tabel.on_drag(drag, |d: &TabDrag, _, _, cx| cx.new(|_| d.clone()));
            // trailing slot: status dot (connecting / failed / disconnected) at rest, close × on hover or when active
            let (_, scol) = panes[0].read(cx).status(&th);
            let show_dot = !active && !panes[0].read(cx).embedded() && scol != c(th.success) && matches!(panes[0].read(cx).spec, Spec::Ssh(_));
            tabel = tabel.child(div().relative().size(px(18.)).flex_shrink_0()
                .when(show_dot, |d| d.child(div().absolute().top(px(6.)).left(px(6.)).size(px(6.)).rounded_full().bg(scol).group_hover(tg.clone(), |s| s.invisible())))
                .child(div().id(("close", i)).absolute().top_0().left_0().size(px(18.)).rounded_full().flex().items_center().justify_center()
                    .when(!active, |d| d.invisible().group_hover(tg.clone(), |s| s.visible()))
                    .hover(|s| s.bg(c(cur().text).opacity(0.1)))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        cx.stop_propagation();
                        this.close_tab(i, w, cx);
                    }))
                    .child(icon(IconName::X, 11., c(th.overlay1)))));
            // tab appear: fade + grow from a narrow pill (keyed by the first pane, so reorders don't replay it)
            let tabel: AnyElement = if motion::reduced() { tabel.into_any_element() } else {
                tabel.with_animation(("tab-in", panes[0].entity_id().as_u64() as usize), gpui::Animation::new(motion::ms(motion::APPEAR_MS)).with_easing(gpui::ease_out_quint()),
                    |d, t| d.opacity(t).min_w(px(TAB_MIN_W * t))).into_any_element()
            };
            // 选中态只切换底色，不包额外的动画外层：外层会改变元素路径，使“出现”动画对新旧两个选中标签重播而闪烁。
            tabbar = tabbar.child(tabel);
        }
        // Settings tab (not a session): it sits after the terminals and borrows the tab look, but
        // stays out of `tabs`, so layout restore, broadcast and SSH actions never see it. It lives
        // until its own close button, exactly like a terminal tab.
        if settings_tab.is_some() {
            let tg = SharedString::from("settings-tab-g");
            if !self.tabs.is_empty() { tabbar = tabbar.child(gap(!settings_on && self.active + 1 != self.tabs.len())); }
            tabbar = tabbar.child(div().id("settings-tab").group(tg.clone()).h(px(28.)).w(px(TAB_W)).flex_shrink(1.).min_w(px(TAB_MIN_W)).pl(px(10.)).pr(px(4.)).rounded(px(7.)).flex().items_center().gap(px(6.)).cursor_pointer()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(settings_on, |d| d.bg(pill_bg).shadow(pill_shadow.clone()).rounded_tl(px(10.)).rounded_tr(px(10.)))
                .when(!settings_on, |d| d.hover(move |s| s.bg(hover_bg)))
                .on_click(cx.listener(|this, _, w, cx| this.open_settings(Cat::General, w, cx)))
                .child(icon(IconName::Settings, 13., if settings_on { c(th.subtext) } else { c(th.overlay1) }))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).text_color(if settings_on { c(th.text) } else { c(th.overlay1) }).when(settings_on, |d| d.font_weight(FontWeight::MEDIUM)).child(t("ui.settings")))
                .child(div().id("settings-tab-close").size(px(18.)).rounded_full().flex().items_center().justify_center()
                    .when(!settings_on, |d| d.invisible().group_hover(tg, |s| s.visible()))
                    .hover(|s| s.bg(c(cur().text).opacity(0.1)))
                    .on_click(cx.listener(|this, _, w, cx| { cx.stop_propagation(); this.close_settings(w, cx); }))
                    .child(icon(IconName::X, 12., c(th.overlay0)))));
        }
        // "+" then the empty title-bar area (window drag / double-click maximize / Windows snap),
        // then the window's own min / max / close buttons (Windows/Linux style, PRD 5.23)
        row = row.child(tabbar).child(
            div().id("new-tab").size(px(28.)).flex_shrink_0().ml(px(2.)).rounded(px(7.)).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).flex().items_center().justify_center().cursor_pointer()
                .hover(move |s| s.bg(hover_bg)).active(move |s| s.bg(pill_bg))
                .tooltip(|w, cx| gpui_component::tooltip::Tooltip::new(t("ui.new_tab")).build(w, cx))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, w, cx| {
                    // 菜单刚被“点击外部”关闭（就是这次点击）时不再重新打开，实现开关切换
                    if this.new_menu.is_some() || this.new_menu_closed.elapsed() < std::time::Duration::from_millis(250) { this.new_menu = None; cx.notify(); return; }
                    let p = ev.position();
                    this.open_new_menu(gpui::point(p.x - px(14.), p.y + px(18.)), w, cx);
                }))
                .child(icon(IconName::Plus, 15., c(th.overlay1))),
        ).child(chrome::drag_spacer("title-spacer").min_w(px(chrome::MIN_DRAG_W)).flex_shrink_0().flex_grow(1.));
        row = match win_ctl { Some(wc) => row.child(wc), None => row.pr(px(8.)) };

        let workspace = if settings_on {
            settings_tab.unwrap().0.into_any_element()
        } else {
            match self.tabs.get(self.active) {
            Some(tab) => self.render_node(self.active, &tab.layout, vec![], &th, cx),
            None => div().flex_1().size_full().flex().flex_col().items_center().justify_center().gap_3().bg(c(th.base))
                .child(icon(IconName::SquareTerminal, 36., c(th.overlay0)))
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(t("ui.no_open_terminals")))
                .child(div().flex().gap_2()
                    .child(ui::button("empty-local", t("ui.open_local_terminal"), 1).on_click(cx.listener(|this, _, w, cx| this.new_tab(Spec::Local, w, cx))))
                    .child(ui::button("empty-palette", t("ui.command_palette"), 0).on_click(cx.listener(|this, _, w, cx| this.open_palette(w, cx))))
                    .when(!self.closed_tabs.is_empty(), |d| d.child(ui::button("empty-reopen", t("tab.reopen"), 0).on_click(cx.listener(|this, _, w, cx| this.reopen_closed(w, cx))))))
                .into_any_element(),
            }
        };
        let cwd_bar = if settings_on { None } else { self.cwd_bar(window, cx) };
        let center = div().id("center").flex_1().min_w_0().min_h_0().flex().flex_col().rounded(px(10.)).overflow_hidden().bg(c(theme::scheme().bg))
            // GPUI 的 overflow_hidden 只按矩形裁剪，不按圆角裁剪：内容（终端、设置页、SFTP 等）若贴边
            // 绘制自己的背景就会盖住圆角。上下留出内边距，让四个圆角露出卡片自身的背景。
            .pt(px(CARD_INSET))
            .when(cwd_bar.is_none(), |d| d.pb(px(if settings_on { CARD_INSET } else { 12. })))
            .child(div().flex_1().min_h_0().overflow_hidden().flex().flex_col().child(workspace))
            .children(cwd_bar);

        let (status, scolor, dims, title, hist) = match self.focused_pane(window, cx) {
            Some(p) => {
                let v = p.read(cx);
                let (st, col) = v.status(&th);
                if let Some(e) = &v.embed {
                    (st, col, String::new(), SharedString::from(e.crumbs(cx)), String::new())
                } else if let Some(dbv) = &v.db {
                    // database pane: connection › database › schema › table instead of terminal info
                    let crumbs = dbv.read(cx).crumbs();
                    (st, col, String::new(), SharedString::from(crumbs), String::new())
                } else {
                let d = v.session.as_ref().map(|s| { let z = s.size(); format!("{}×{}", z.cols, z.rows) }).unwrap_or_default();
                let n = v.history.lock().unwrap().len();
                (st, col, d, shown_title(v), tf("main.history_n", &[("n", &n)]))
                }
            }
            None => ("—", c(th.overlay0), String::new(), SharedString::from(""), String::new()),
        };
        let theme_label = if s.appearance.theme == "system" { tf("main.theme_system", &[("name", &theme::display_name(&th.name))]) } else { theme::display_name(&th.name) };
        let org = ssh_term_proto::managed::get();
        let mode = org.as_ref().map_or(s.permissions.mode, |m| m.clamp_mode(s.permissions.mode));
        let mode_col = match mode { Mode::Ask => c(th.accent), Mode::Plan => c(th.info), Mode::Auto => c(th.warning) };
        let sb_btn = |id: &'static str| div().id(id).px_2().h(px(20.)).rounded_md().flex().items_center().gap_1().cursor_pointer().hover(|s| s.bg(c(cur().surface0)));
        let statusbar = div().h(px(28.)).px_3().flex().items_center().gap_3()
            .text_size(px(11.)).text_color(c(th.subtext))
            // 原标题栏右上角的“本地终端”入口
            .child(sb_btn("sb-local").tooltip(|w, cx| gpui_component::tooltip::Tooltip::new(t("ui.local_terminal")).build(w, cx))
                .on_click(cx.listener(|this, _, w, cx| this.new_tab(Spec::Local, w, cx)))
                .child(icon(IconName::SquareTerminal, 12., c(th.overlay1))).child(t("ui.local_terminal")))
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

        // 顶层：活动图标栏 + 工具窗口（全高）| 右列 [标签栏 / 内容 / 底部工具窗口]
        let mut content = div().flex_1().min_h_0().w_full().flex().flex_row().items_stretch().gap(px(CARD_GAP));
        let strip = self.activity_strip(cx).into_any_element();
        if self.sidebar_visible {
            let sb = self.tool_panel(window, cx);
            // 侧栏顶部一行（与标签行同高，可拖动窗口）：红黄绿按钮风格时软件名在按钮右侧，否则靠左带小图标
            let name = div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).whitespace_nowrap().child("ShellAX");
            let mut head = chrome::drag_region(div().id("sb-chrome")).h(px(chrome::TAB_H)).flex_shrink_0().flex().items_center().overflow_hidden()
                .window_control_area(gpui::WindowControlArea::Drag);
            head = if mac { head.child(self.lights(window, cx)).child(name) } else { head.pl(px(12.)).gap(px(6.)).child(icon(IconName::SquareTerminal, 14., c(th.accent))).child(name) };
            let panel: AnyElement = match self.sb_anim {
                Some((show, seq)) => {
                    let w = self.sb_width;
                    // ease-out both ways; the inner box keeps its full width so content never reflows,
                    // the outer box clips it while its width animates
                    let dur = if show { motion::SIDEBAR_MS } else { motion::SIDEBAR_MS - 30 };
                    let ease = |t: f32| 1. - (1. - t).powi(3);
                    div().h_full().flex_shrink_0().overflow_hidden().w(px(if show { 0. } else { w }))
                        .child(div().h_full().w(px(w)).flex().flex_col().child(sb))
                        .with_animation(("sb-anim", seq as usize), gpui::Animation::new(motion::ms(dur)).with_easing(ease),
                            move |d, t| { let k = if show { t } else { 1. - t }; d.w(px(w * k)).opacity(0.35 + 0.65 * k) }).into_any_element()
                }
                None => sb,
            };
            let vib = if chrome::NATIVE_LIGHTS { 0.72 } else { 1.0 };
            content = content.child(div().h_full().flex_shrink_0().flex().flex_col()
                .child(head.bg(c(th.crust).opacity(vib)).child(chrome::drag_spacer("sb-drag")))
                .child(div().flex_1().min_h_0().flex().flex_row().child(strip).child(panel)));
        } else {
            content = content.child(strip);
        }
        let mut work = div().flex_1().min_w_0().min_h_0().flex().flex_row().gap(px(CARD_GAP)).child(center);
        work = match self.dock {
            SideDock::Ai => work.child(self.ai_dock(cx)),
            SideDock::Snippets | SideDock::History => work.child(self.library_dock(window, cx)),
            SideDock::None => work,
        };
        let work_row = div().flex_1().min_h_0().w_full().flex().flex_row().items_stretch().pr(px(4.)).pb(px(CARD_GAP)).gap(px(CARD_GAP))
            .child(work).child(self.tool_rail(cx));
        let mut right = div().id("right-col").flex_1().min_w_0().h_full().flex().flex_col().child(row).child(work_row);
        if self.bottom_open {
            right = right.on_drag_move(cx.listener(|this, ev: &gpui::DragMoveEvent<activity::BottomResize>, _w, cx| {
                let h = (f32::from(ev.bounds.bottom() - ev.event.position.y) - (CARD_GAP + 2.)).clamp(120., 600.);
                if (h - this.bottom_h).abs() >= 1.0 { this.bottom_h = h; cx.notify(); }
            })).child(div().w_full().pr(px(4.)).pb(px(CARD_GAP)).child(self.bottom_panel(cx)));
        }
        let body = content.child(right);
        let mut root = div().size_full().relative().flex().flex_col().bg(theme::outer_bg().opacity(if chrome::NATIVE_LIGHTS { 0. } else { op })).font_family(theme::ui_font()).text_size(px(s.appearance.ui_font_size.clamp(10., 20.))).text_color(c(th.text))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                let k = &ev.keystroke;
                let bare_tab = k.key == "tab" && !k.modifiers.control && !k.modifiers.alt && !k.modifiers.platform;
                if bare_tab {
                    if let Some(p) = this.tabs.get(this.active).and_then(|t| t.layout.leaves().into_iter().find(|p| p.read(cx).focus.is_focused(w) && !p.read(cx).embedded())) {
                        p.update(cx, |v, cx| v.on_key(ev, w, cx));
                        cx.stop_propagation();
                        return;
                    }
                }
                if ev.keystroke.key == "escape" && !matches!(this.overlay, Overlay::None | Overlay::Palette(_)) {
                    this.close_overlay(w, cx);
                    cx.stop_propagation();
                    return;
                }
                let ks = actions::keystroke_str(&ev.keystroke);
                if let Some(a) = actions::find(&ks) { this.run(a, w, cx); cx.stop_propagation(); }
            }))
            // Native menu bar (macOS) dispatches GPUI actions; they land here and run the very same
            // commands as the keyboard and the palette. stop_propagation keeps a handled action from
            // also reaching the key listeners below.
            .on_action(cx.listener(|this, _: &app_menu::QuitApp, w, cx| { this.run(Action::Quit, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenSettings, w, cx| { this.run(Action::OpenSettings, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenAbout, w, cx| { this.run(Action::OpenAbout, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::NewTerminalTab, w, cx| { this.run(Action::NewLocalTab, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::NewHostDialog, w, cx| { this.run(Action::NewHost, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::SplitRightPane, w, cx| { this.run(Action::SplitRight, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::SplitDownPane, w, cx| { this.run(Action::SplitDown, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::ClosePane, w, cx| { this.run(Action::ClosePane, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenSftp, w, cx| { this.run(Action::OpenSftp, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::ToggleSidebar, w, cx| { this.run(Action::ToggleSidebar, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::CycleTheme, w, cx| { this.run(Action::CycleTheme, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenPalette, w, cx| { this.run(Action::CommandPalette, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenAiPanel, w, cx| { this.run(Action::OpenAi, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenSnippets, w, cx| { this.run(Action::OpenSnippets, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenHistory, w, cx| { this.run(Action::OpenHistory, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|this, _: &app_menu::OpenAuditLog, w, cx| { this.run(Action::OpenAudit, w, cx); cx.stop_propagation(); }))
            .on_action(cx.listener(|_, _: &app_menu::MinimizeWindow, w, cx| { w.minimize_window(); cx.stop_propagation(); }))
            // 编辑菜单只作用于终端；焦点在设置或对话框的输入框时不抢它们的复制粘贴。
            .on_action(cx.listener(|this, _: &app_menu::CopySelection, w, cx| {
                if matches!(this.overlay, Overlay::None) {
                    if let Some(p) = this.focused_pane(w, cx) { p.update(cx, |v, cx| { let _ = v.copy(cx); }); }
                }
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &app_menu::PasteClipboard, w, cx| {
                if matches!(this.overlay, Overlay::None) {
                    if let Some(p) = this.focused_pane(w, cx) { p.update(cx, |v, cx| v.paste(cx)); }
                }
                cx.stop_propagation();
            }));
        root = root.child(body).child(statusbar);
        if let Some(e) = chrome::resize_edges(window) { root = root.child(e); }

        // menu exit fade: a menu that just closed is drawn once more, fading out
        {
            let closed_sb = if self.sb_menu.is_none() { self.menu_last.0.take() } else { None };
            let closed_tab = if self.tab_menu.is_none() { self.menu_last.1.take() } else { None };
            if (closed_sb.is_some() || closed_tab.is_some()) && !motion::reduced() {
                self.menu_ghost_seq += 1;
                let seq = self.menu_ghost_seq;
                self.menu_ghost = Some((closed_sb, closed_tab, seq));
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(motion::ms(motion::EXIT_MS)).await;
                    let _ = this.update(cx, |r, cx| { if r.menu_ghost.as_ref().is_some_and(|g| g.2 == seq) { r.menu_ghost = None; cx.notify(); } });
                }).detach();
            }
            if self.sb_menu.is_some() { self.menu_last.0 = self.sb_menu.clone(); }
            if self.tab_menu.is_some() { self.menu_last.1 = self.tab_menu; }
        }
        let host_menu = self.sb_menu.clone().map(|m| self.sb_menu_el(m, cx));
        let overlay: Option<AnyElement> = match &self.overlay {
            Overlay::None => None,
            Overlay::Palette(p) => Some(div().absolute().top_0().left_0().size_full().child(p.clone()).into_any_element()),
            Overlay::Host(d) => Some(div().absolute().top_0().left_0().size_full().child(d.clone()).into_any_element()),
            Overlay::Db(d) => Some(div().absolute().top_0().left_0().size_full().child(d.clone()).into_any_element()),
            Overlay::Asset(d) => Some(div().absolute().top_0().left_0().size_full().child(d.clone()).into_any_element()),
            Overlay::Audit => Some(self.audit_panel(cx)),
            Overlay::Sftp => Some(self.sftp_panel(cx)),
            Overlay::Port => Some(self.port_panel(cx)),
            Overlay::HostTools(v) => Some(ui::modal(v.clone()).into_any_element()),
        };
        if let Some(o) = overlay { root = root.child(o); }
        if let Some(m) = host_menu { root = root.child(m); }

        // host key confirmation (first connection / TOFU)
        if let Some(p) = self.hk_pending.first() {
            root = root.child(ui::modal(ui::card(560.).p_5().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::ShieldCheck, 16., c(th.warning)))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.first_connection_verify_host_key"))))
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(tf("main.not_in_kh", &[("host", &p.host), ("port", &p.port), ("path", &s.known_hosts_path().display())])))
                .child(div().p_3().rounded_md().bg(c(th.mantle)).flex().flex_col().gap_1().text_size(px(12.)).text_color(c(th.text))
                    .child(tf("main.algorithm", &[("a", &p.algorithm)]))
                    .child(div().font_family(SharedString::from(s.font.family.clone())).child(tf("main.fingerprint", &[("fp", &p.fingerprint)]))))
                .child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(t("ui.verify_the_fingerprint_through_another")))
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
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(detail))
                .child(ui::text_field(&inp, false))
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
                    .child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(tf("main.approval_left", &[("n", &left), ("q", &self.approvals.len())]))))
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(tf("main.approval_who", &[("agent", &a.principal), ("host", &a.host), ("tool", &a.req.tool), ("risk", &a.risk.label())])))
                .child(div().p_3().rounded_md().bg(c(th.mantle)).text_size(px(13.)).text_color(c(th.text)).font_family(SharedString::from(s.font.family.clone())).child(a.req.command.clone()))
                .child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(tf("main.approval_why", &[("why", &a.reason), ("cwd", &a.req.cwd)])))
                .when_some(self.deny_reason.clone(), |d, inp| d.child(div().flex().items_center().gap_2()
                    .child(div().flex_1().child(ui::text_field(&inp, false)))
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
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(tf("main.quit_body", &[("p", &panes), ("a", &agents), ("d", &daemon)])))
                .child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(t("main.quit_note")))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("quit-cancel", t("ui.cancel"), 0).on_click(cx.listener(|this, _, _w, cx| { this.quit_confirm = false; cx.notify(); })))
                    .child(ui::button("quit-ok", t("main.quit_ok"), 2).on_click(cx.listener(|this, _, w, cx| {
                        this.quit_ok = true; this.save_layout(cx); this.log("app.quit", "confirmed");
                        for v in this.tabs.iter().flat_map(|t| t.layout.leaves()) { if let Some(s) = &v.read(cx).session { if v.read(cx).daemon_id.is_none() { s.close(); } } }
                        w.remove_window(); cx.quit();
                    }))))));
        }
        if let Some(rec) = update_ui::prompt() {
            root = root.child(ui::modal(ui::card(440.).p_5().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::Download, 16., c(th.accent)))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(tf("update.ready_title", &[("v", &rec.version)]))))
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(t("update.ready_body")))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("upd-later", t("update.later"), 0).on_click(cx.listener(|_, _, _w, cx| { update_ui::dismiss_prompt(); cx.notify(); })))
                    .child(ui::button("upd-quit", t("update.quit_and_update"), 1).on_click(cx.listener(move |this, _, w, cx| { update_ui::dismiss_prompt(); this.apply_update(&rec, w, cx); }))))));
        }
        if let Some(m) = self.modal.clone() { root = root.child(self.render_modal(m, cx)); }
        if let Some(m) = self.sb_modal.clone() { root = root.child(self.sb_modal_el(m, cx)); }
        if let Some((i, pos)) = self.tab_menu { root = root.child(self.tab_menu_el(i, pos, cx)); }
        if let Some(pos) = self.new_menu { root = root.child(self.new_menu_el(pos, cx)); }
        if let Some((gs, gt, seq)) = self.menu_ghost.clone() {
            if self.sb_menu.is_none() { if let Some(m) = gs { let el = self.sb_menu_el(m, cx); root = root.child(chrome::pop_out(("menu-out-sb", seq as usize), div().absolute().top_0().left_0().size_full().child(el))); } }
            if self.tab_menu.is_none() { if let Some((i, pos)) = gt { if i < self.tabs.len() { let el = self.tab_menu_el(i, pos, cx); root = root.child(chrome::pop_out(("menu-out-tab", seq as usize), div().absolute().top_0().left_0().size_full().child(el))); } } }
        }
        if let (Some(m), None) = (update_ui::offered(), update_ui::prompt()) {
            root = root.child(self.update_offer_el(m, cx));
        }
        if let Some((msg, seq)) = &self.toast {
            // info toast: neutral text + info icon, 3px semantic bar on the left
            let el = div().absolute().bottom(px(40.)).right(px(16.)).min_w(px(320.)).max_w(px(400.)).pl(px(12.)).pr_4().py_2().rounded(px(ui::R_LG)).bg(c(th.base)).border_1().border_color(c(th.surface1)).shadow_lg()
                .flex().items_center().gap_2().text_size(px(ui::FS_SECONDARY)).text_color(c(th.text))
                .child(div().w(px(3.)).h(px(18.)).rounded_full().bg(c(th.info)))
                .child(icon(IconName::Info, 14., c(th.info))).child(msg.clone());
            let el = if motion::reduced() { el.into_any_element() }
                else if self.toast_out { el.with_animation(("toast-out", *seq as usize), gpui::Animation::new(motion::ms(motion::TOAST_EXIT_MS)).with_easing(gpui::ease_in_out), |d, t| d.opacity(1. - t)).into_any_element() }
                else { el.with_animation(("toast-in", *seq as usize), gpui::Animation::new(motion::ms(motion::TOAST_ENTER_MS)).with_easing(gpui::ease_out_quint()), |d, t| d.opacity(t).bottom(px(40. - (1. - t) * 8.))).into_any_element() };
            root = root.child(el);
        }
        root
    }
}

/// 标签宽度：先在 TAB_W 与 TAB_MIN_W 之间压缩，再放不下就横向滚动。
/// 终端卡片与外层之间的间距。
const CARD_GAP: f32 = 6.;
/// 卡片内容与卡片上下边缘的内边距，保证圆角不被内容盖住。
const CARD_INSET: f32 = 8.;
const TAB_W: f32 = 220.;
const TAB_MIN_W: f32 = 110.;

fn main() {
    ssh_term_proto::platform::init_env();
    ssh_term_proto::platform::init_log_file(&data_dir());
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
            if ok { eprintln!("opened in the running ShellAX"); return; }
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
    let schemes = theme::load_schemes(&themes);
    let default_target = if std::env::var("NO_SSH").is_ok() { None } else { env_target(&config::get().known_hosts_path()) };
    gpui_platform::application().with_assets(gpui_kit_assets::AllAssets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        // macOS menu bar (no-op elsewhere): app / file / edit / view / window / help, with ⌘Q.
        app_menu::install(cx);
        // bundled icon font (Nerd Fonts symbols, MIT): prompts with powerline / devicon glyphs render
        // even when no Nerd Font is installed (e.g. stock Windows). Used through font.fallbacks.
        if let Err(e) = cx.text_system().add_fonts(vec![std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf").as_slice())]) { eprintln!("nerd font: {e:#}"); }
        // Decided before the font probe below: that probe writes config.toml, which would otherwise
        // look like an existing profile and skip setup on the very first launch.
        let show_setup = setup_view::needed();
        let installed = cx.text_system().all_font_names();
        theme::set_ui_font(theme::pick_ui_font(&installed));
        let fc = config::get().font.clone();
        let has = |f: &str| installed.iter().any(|i| i.eq_ignore_ascii_case(f));
        let usable = |f: &str| has(f) && monospace_ok(cx, f, fc.size);
        let is_stock_default = cfg!(target_os = "windows") && fc.family == "Cascadia Mono";
        let mut chosen: Option<(String, Option<f32>)> = None;
        if is_stock_default || !usable(&fc.family) {
            // Prefer the font the user's other terminal uses, but only when it is installed here and
            // actually monospaced. An uninstalled family used to be persisted as-is, after which GPUI
            // substituted a proportional UI font for the whole grid (the "乱码" on a fresh machine).
            if let Some(det) = ssh_term_proto::platform::detect_system_terminal_font(&installed) {
                if usable(&det.family) {
                    eprintln!("detected system terminal font: {:?} from {}", det.family, det.source);
                    chosen = Some((det.family, det.size));
                }
            }
            if chosen.is_none() {
                chosen = theme::pick_mono_family(&fc.fallbacks, &installed).filter(|f| usable(f)).map(|f| (f, None));
            }
        } else if let Some(det) = ssh_term_proto::platform::detect_system_terminal_font(&installed) {
            if usable(&det.family) && !fc.fallbacks.iter().any(|fb| fb.eq_ignore_ascii_case(&det.family)) {
                let mut s = (*config::get()).clone();
                s.font.fallbacks.insert(0, det.family);
                config::install(s.clone());
                let _ = s.save();
            }
        }
        match chosen {
            Some((family, size)) => {
                let size = size.filter(|sz| (8.0..=32.0).contains(sz));
                if family != fc.family || size.is_some_and(|sz| (sz - fc.size).abs() > 0.01) {
                    eprintln!("terminal font {:?} is missing or not monospaced; using {family:?}", fc.family);
                    let mut s = (*config::get()).clone();
                    s.font.family = family;
                    if let Some(sz) = size { s.font.size = sz; }
                    config::install(s.clone());
                    let _ = s.save();
                }
            }
            None if !usable(&fc.family) => eprintln!("terminal font {:?} is missing or not monospaced, and no installed monospace fallback was found", fc.family),
            None => {}
        }
        // First launch: the setup window is the only window until it closes, so the main window
        // never flashes behind it. Closing setup (finished, skipped, or the window button) continues.
        if show_setup {
            open_setup(themes.clone(), installed.clone(), cx, rt, themes, schemes, default_target, links, load_err);
            return;
        }
        open_main(cx, rt, themes, schemes, default_target, links, load_err);
    });
}

fn open_main(cx: &mut App, rt: Arc<tokio::runtime::Runtime>, themes: Vec<Arc<Theme>>, schemes: Vec<Arc<Scheme>>, default_target: Option<SshTarget>, links: Vec<String>, load_err: Option<String>) {
        let bounds = Bounds::centered(None, size(px(1440.), px(860.)), cx);
        cx.open_window(
            chrome::window_options(bounds, Some("ShellAX".into())),
            move |window, cx| {
                // no transparent shadow inset around client-drawn windows (no compositor alpha on X11)
                if !chrome::NATIVE_LIGHTS { window.set_client_inset(px(0.)); }
                let view = cx.new(|cx| {
                    let (hk_tx, hk_rx) = flume::unbounded::<HostKeyPrompt>();
                    let (au_tx, au_rx) = flume::unbounded::<ssh::AuthPrompt>();
                    let _ = ssh::SECRETS.set(secret_store());
                    migrate_key_field_passwords();
                    let host_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.search_hosts")));
                    let new_menu_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("newmenu.search")));
                    let lib_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("lib.search")));
                    let audit = audit_log();
                    let mut root = Root {
                        rt, default_target, tabs: vec![], active: 0, themes, schemes, histories: HashMap::new(),
                        manifests: Arc::new(agent::bundled()), prompts: ssh::Prompts { hostkey: hk_tx, auth: au_tx }, auth_pending: vec![], auth_input: None, auth_save: false, hk_pending: vec![], approvals: vec![], deny_reason: None, policy_port: None, overlay: Overlay::None, settings: None, sftp_views: HashMap::new(), sftp_current: None, sftp_windows: vec![], drag_div: None, port_view: None, quit_confirm: false, modal: None, quit_ok: false, chat_view: None,
                        host_search: host_search.clone(), lang_seen: ssh_term_proto::i18n::current() as u8, bg_seen: None, ui_size_seen: 0., sb_menu: None, menu_last: (None, None), menu_ghost: None, menu_ghost_seq: 0, sb_menu_at: std::time::Instant::now(), sb_modal: None, tag_filter: vec![], tab_menu: None, tab_menu_at: std::time::Instant::now(), new_menu: None, new_menu_at: std::time::Instant::now(), new_menu_closed: std::time::Instant::now(), new_menu_collapsed: config::get().sidebar.collapsed.clone(), new_menu_sel: 0, new_menu_search: new_menu_search.clone(), tab_scroll: gpui::ScrollHandle::new(), tab_rename: None, closed_tabs: vec![], sb_width: config::get().sidebar.width.clamp(180.0, 480.0), dock: SideDock::None, lib_search: lib_search.clone(), lib_group: String::new(), lib_hist_all: false, lib_editor: None, lib_vars: None, lib_confirm: None, lib_notice: String::new(), ai_width: { let w = config::get().sidebar.ai_width; if (280.0..=560.0).contains(&w) { w } else { config::AI_DOCK_W } }, host_os: Default::default(), os_probing: HashSet::new(), sidebar_visible: !config::get().sidebar.collapsed_panel(), sb_anim: None, tool: activity::Tool::from_id(&config::get().sidebar.tool), bottom_open: config::get().sidebar.bottom_open, bottom_tab: activity::Bottom::from_id(&config::get().sidebar.bottom_tab), bottom_h: config::get().sidebar.bottom_height.clamp(120., 600.), exec_log: Default::default(), dbp: Default::default(), audit, audit_verify: None, audit_filter: Default::default(),
                        settings_dirty: vec![], toast: None, toast_out: false, toast_seq: 0, _subs: vec![],
                    };
                    crate::ctxmenu_view::init(root.rt.handle().clone(), root.prompts.clone());
                    root.apply_theme(window, cx);
                    root._subs.push(cx.observe_window_appearance(window, |this: &mut Root, w, cx| { this.apply_theme(w, cx); this.refresh_all(cx); }));
                    root._subs.push(cx.subscribe_in(&new_menu_search, window, |this: &mut Root, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { this.new_menu_sel = 0; cx.notify() }));
                    root._subs.push(cx.subscribe_in(&host_search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
                    root._subs.push(cx.subscribe_in(&lib_search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
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
                    // 应用内更新：读下载记录并清理旧安装包；下载进度变化时刷新主窗口和设置页
                    if !secondary() {
                        update_ui::init();
                        cx.spawn(async move |this, cx| {
                            let mut seen = u64::MAX;
                            loop {
                                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                                let g = update_ui::GEN.load(Ordering::Relaxed);
                                if g == seen && !update_ui::downloading() { continue; }
                                seen = g;
                                if this.update(cx, |r, cx| { if let Some((v, _)) = &r.settings { v.update(cx, |_, cx| cx.notify()); } cx.notify(); }).is_err() { break; }
                            }
                        }).detach();
                    }
                    // update check (general.check_updates); quiet on network errors
                    if !secondary() && config::get().general.check_updates && std::env::var_os("SHELLAX_NO_UPDATE_CHECK").is_none() {
                        let g = config::get().general.clone();
                        cx.spawn(async move |this, cx| {
                            let r = cx.background_executor().spawn(async move { g.check_update() }).await;
                            update_ui::mark_checked();
                            match r {
                                Ok(Some(m)) => { let _ = this.update(cx, |r, cx| {
                                    r.log("update.available", &m.version);
                                    if config::get().general.auto_download && ssh_term_proto::updater::downloadable(&m) && update_ui::ready_for(&m.version).is_none() { update_ui::start(m.clone(), true); }
                                    else { update_ui::offer(m.clone()); }
                                    cx.notify();
                                }); }
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
                    // a moved-tab window shares the main window's policy service (hooks read its endpoint file)
                    if secondary() { root.log("policy.server", "secondary window: using the main window's policy service"); } else {
                    match ssh_term_proto::policy_server::start_with_ops(&bind, None, ctx, Some(ap_tx), root.audit.clone(), Some(ssh_term_proto::policy_server::endpoint_path(&data_dir())), Some(ops)) {
                        Ok(srv) => { root.policy_port = Some(srv.port); *ssh::POLICY_ENDPOINT.lock().unwrap() = Some((srv.port, srv.token.clone())); root.log("policy.server", format!("listening on 127.0.0.1:{}", srv.port)); }
                        Err(e) => root.toast(tf("main.policy_server_failed", &[("e", &e)]), cx),
                    }}
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
                            // SHELLAX_TEST_SLOW_WITHDRAW: test hook that keeps a withdrawn card on screen so a late "allow" can be clicked
                            let slow = std::env::var_os("SHELLAX_TEST_SLOW_WITHDRAW").is_some();
                            r.approvals.retain(|a| a.deadline > now && (slow || !a.reply.is_disconnected()));
                            cx.notify();
                        }).is_err() { break; }
                    }).detach();
                    // system tray: running-agent count + show / new terminal / quit (Linux SNI)
                    #[cfg(target_os = "linux")]
                    if !secondary() && config::get().general.tray && std::env::var("SHELLAX_NO_TRAY").is_err() {
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
                    let want_restore = secondary() || ((st.general.startup == "restore" || (st.session.restore_layout && st.general.startup != "blank")) && std::env::var("SHELLAX_NO_RESTORE").is_err());
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
                            if !root.block_proxy(&Spec::Ssh(t.clone()), false, window, cx) {
                                let r = root.open_pane(Spec::Ssh(t), window, cx);
                                layout.split(&l, r, Dir::Vertical);
                            }
                        }
                        let fh = l.read(cx).focus.clone();
                        root.tabs.push(Tab { layout, focused: l, name: None });
                        window.focus(&fh, cx);
                    }
                    let weak = cx.entity().downgrade();
                    root._subs.push(cx.intercept_keystrokes(move |ev, window, cx| {
                        let k = &ev.keystroke;
                        if k.key != "tab" || k.modifiers.control || k.modifiers.alt || k.modifiers.platform { return; }
                        let Some(ent) = weak.upgrade() else { return; };
                        let shift = k.modifiers.shift;
                        let _ = ent.update(cx, |this, cx| {
                            if !matches!(this.overlay, Overlay::None) || this.sb_modal.is_some() { return; }
                            let Some(p) = this.tabs.get(this.active).and_then(|t| t.layout.leaves().into_iter().find(|p| p.read(cx).focus.is_focused(window) && !p.read(cx).embedded())) else { return; };
                            p.update(cx, |v, cx| v.inject_tab(shift, window, cx));
                            window.prevent_default();
                            cx.stop_propagation();
                        });
                    }));
                    root
                });
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            },
        ).unwrap();
        cx.activate(true);
}

/// First-run window. It is the only window until it closes, and the choices only reach config.toml
/// when the user finishes, so closing or skipping leaves the defaults.
fn open_setup(setup_themes: Vec<Arc<Theme>>, installed_fonts: Vec<String>, cx: &mut App, rt: Arc<tokio::runtime::Runtime>, themes: Vec<Arc<Theme>>, schemes: Vec<Arc<Scheme>>, default_target: Option<SshTarget>, links: Vec<String>, load_err: Option<String>) {
    let bounds = Bounds::centered(None, size(px(760.), px(600.)), cx);
    if cx.open_window(chrome::window_options(bounds, Some("ShellAX".into())), |window, cx| {
        if !chrome::NATIVE_LIGHTS { window.set_client_inset(px(0.)); }
        let font_search = cx.new(|cx| gpui_component::input::InputState::new(window, cx));
        let view = cx.new(|_cx| {
            let setup = setup_view::SetupView::new(setup_themes, &installed_fonts, font_search);
            // Paint with the suggested language before the first frame, so a Chinese Windows
            // does not flash the English page and then switch.
            ssh_term_proto::i18n::set_language(setup.language());
            setup
        });
        // Taken exactly once. Both finishing and the title-bar close come through here: finishing
        // emits the event below, and the close button raises the same callback. remove_window alone
        // does not, which is why the app used to vanish instead of opening the main window.
        let pending = std::rc::Rc::new(std::cell::RefCell::new(Some((rt, themes, schemes, default_target, links, load_err))));
        let continue_to_main = move |completed: bool, w: &mut Window, cx: &mut App| {
            if let Some((rt, themes, schemes, default_target, links, load_err)) = pending.borrow_mut().take() {
                // Reaching the main window at all counts as having seen setup, including the
                // title-bar close, which never goes through the Skip button.
                setup_view::mark_seen();
                // Setup previews its theme in memory. Apply it once more against the window that is
                // about to open, so "follow the system" resolves here instead of only when the row
                // was clicked during setup.
                if let Some(th) = setup_view::committed_theme(completed, &themes, cx) {
                    crate::theme::set_current(th.clone());
                    crate::theme::set_scheme(std::sync::Arc::new(th.to_scheme()));
                    crate::sync_component_theme(&th, cx);
                }
                open_main(cx, rt, themes, schemes, default_target, links, load_err);
            }
            w.remove_window();
        };
        let on_done = continue_to_main.clone();
        window.subscribe(&view, cx, move |_, ev: &setup_view::SetupEvent, w, cx| {
            let setup_view::SetupEvent::Done(completed) = ev;
            on_done(*completed, w, cx);
        }).detach();
        window.on_window_should_close(cx, move |w, cx| { continue_to_main(false, w, cx); false });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    }).is_err() { return }
}

fn shown_title(v: &TermView) -> SharedString { v.shown_title() }

/// Name of the local shell actually started (PRD 5.23 fix: was always "bash" on Windows).
pub(crate) fn shell_display_name() -> String {
    let s = config::get().terminal.shell.clone();
    let p = if s.is_empty() { ssh_term_proto::local::default_shell_path() } else { s };
    let base = p.rsplit(['/', '\\']).next().unwrap_or(&p).to_string();
    base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(&base).to_string()
}

/// Root view of a detached SFTP window (PRD 5.21). Closing it re-attaches the view as a tab.
struct SftpWindow { view: Entity<sftp_view::SftpView> }
impl Render for SftpWindow {
    fn render(&mut self, w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let v = self.view.clone();
        let title = v.read(_cx).title();
        // own title bar: traffic lights (close = re-attach as tab), drag to move
        let bar = chrome::drag_region(div().id("sftp-win-titlebar")).h(px(chrome::TITLE_H)).flex_shrink_0().flex().items_center().bg(c(th.crust)).border_b_1().border_color(c(th.surface1).opacity(0.6))
            .child(chrome::traffic_lights("sftp-lights", w, move |_, cx| v.update(cx, |_, cx| cx.emit(sftp_view::SftpEvent::Move(SftpMode::Tab)))))
            .child(div().flex_1().text_center().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(c(th.subtext)).child(title))
            .child(div().w(px(chrome::LIGHTS_W)));
        div().size_full().relative().flex().flex_col().bg(c(th.base)).font_family(theme::ui_font()).text_color(c(th.text))
            .child(bar).child(self.view.clone()).children(chrome::resize_edges(w))
    }
}
