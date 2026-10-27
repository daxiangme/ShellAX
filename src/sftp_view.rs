//! Dual-pane SFTP browser. Each pane picks its own source (this computer or any saved host),
//! so files can move local ↔ remote, local ↔ local and remote ↔ remote (relayed through a
//! temporary directory on this computer). Remote panes reuse the pooled SSH connection.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnimationExt, AnyElement, Context, Entity, ExternalPaths, FontWeight, Pixels, Point, Window};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_component::Sizable;
use russh_sftp::client::SftpSession;
use ssh_term_proto::hosts::HostEntry;
use ssh_term_proto::hosttree;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::session_store::SftpSide;
use ssh_term_proto::sftp::{self, Dir, Entry, JobState, Queue, Side};
use ssh_term_proto::sftp_follow::Follow;
use ssh_term_proto::ssh::{self, SshTarget};

use crate::theme::{c, cur, Appearance};
use crate::ui::{self, fs_body, fs_small, icon, IconName, H_COMPACT, H_DEFAULT, R_MD, R_SM};

// 左右留白：只由工作区包裹层的 `card_layout::CARD_PAD` 提供（与数据库等模块一致）。
// 标题栏、双栏面板、传输队列、状态行都不再叠加自己的左右内边距，所以标题图标的左边缘与面板卡片左边缘对齐，右侧同理。
// 页面标题不是弹窗：行高用工具栏高度，标题与下方内容之间只留 `CARD_PAD`，避免 48px 弹窗头在文字上下留出大块空白。

#[derive(Clone)]
pub struct DragFile { pub pane: usize, pub path: String, pub name: String }
pub struct DragPreview(String);
impl Render for DragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        div().px_2().py_1().rounded(px(ui::R_MD)).bg(c(th.surface1)).text_size(px(ui::fs_body())).text_color(c(th.text)).child(self.0.clone())
    }
}

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum DirAction { Right, Down, Close }

#[derive(Clone, Copy, PartialEq)]
enum Edit { Mkdir, Rename, Chmod }

pub enum SftpEvent {
    /// Insert a command (no Enter) into a session on this host (None = a local pane).
    Insert { target: Option<SshTarget>, command: String },
    /// Open a new tab on this host / locally and insert `cd -- dir`.
    CdTab { target: Option<SshTarget>, dir: String },
    /// Split or close the pane that contains this SFTP view.
    #[allow(dead_code)]
    Pane(DirAction),
    /// The title changed (a host connected / the source changed).
    Retitle,
}
impl gpui::EventEmitter<SftpEvent> for SftpView {}

/// A remote source: one SFTP session and its own transfer queue.
struct Remote {
    name: String,
    target: SshTarget,
    sess: Option<Arc<SftpSession>>,
    /// Keeps the pooled SSH connection alive while the pane shows this host.
    conn: Option<Arc<ssh::Conn>>,
    connecting: bool,
    error: Option<String>,
    home: String,
    queue: Queue,
    /// Directory to open first instead of the home directory.
    want: Option<String>,
    token: u64,
}

enum Src { Empty, Local, Remote(Box<Remote>) }

struct Picker { search: Entity<InputState>, group: String }

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey { Name, Mtime, Size, Kind }
#[derive(Clone, Copy)]
struct PaneSort { key: SortKey, desc: bool }
impl Default for PaneSort { fn default() -> Self { Self { key: SortKey::Name, desc: false } } }

struct Pane {
    src: Src,
    picker: Option<Picker>,
    /// Local directory ("" = drive list on Windows) and remote directory.
    ldir: PathBuf,
    rdir: String,
    entries: Vec<Entry>,
    loading: bool,
    gen: u64,
    sort: PaneSort,
    hidden: bool,
    filter: Option<Entity<InputState>>,
    back: Vec<String>,
    fwd: Vec<String>,
    sel: Option<usize>,
    multi: Vec<usize>,
}
impl Pane {
    fn new(src: Src) -> Self {
        Self { src, picker: None, ldir: default_local_dir(), rdir: String::new(), entries: vec![], loading: false, gen: 0, sort: PaneSort::default(), hidden: false, filter: None, back: vec![], fwd: vec![], sel: None, multi: vec![] }
    }
    fn remote(&self) -> Option<&Remote> { match &self.src { Src::Remote(r) => Some(r), _ => None } }
    fn remote_mut(&mut self) -> Option<&mut Remote> { match &mut self.src { Src::Remote(r) => Some(r), _ => None } }
    fn sess(&self) -> Option<Arc<SftpSession>> { self.remote().and_then(|r| r.sess.clone()) }
    fn is_local(&self) -> bool { matches!(self.src, Src::Local) }
    /// Ready to list / receive files.
    fn ready(&self) -> bool { match &self.src { Src::Local => true, Src::Remote(r) => r.sess.is_some(), Src::Empty => false } }
    fn side(&self) -> Side { if self.is_local() { Side::Local } else { Side::Remote } }
    fn dir(&self) -> String { if self.is_local() { self.ldir.to_string_lossy().into_owned() } else { self.rdir.clone() } }
    fn path_of(&self, name: &str) -> String { if self.is_local() { self.ldir.join(name).to_string_lossy().into_owned() } else { sftp::join(&self.rdir, name) } }
    fn label(&self) -> String { match &self.src { Src::Local => t("sftp.local").to_string(), Src::Remote(r) => r.name.clone(), Src::Empty => String::new() } }
    fn target(&self) -> Option<SshTarget> { self.remote().map(|r| r.target.clone()) }
    fn clear_sel(&mut self) { self.sel = None; self.multi.clear(); }
    fn selection(&self) -> Vec<Entry> {
        let mut idx = self.multi.clone();
        if idx.is_empty() { if let Some(i) = self.sel { idx.push(i); } }
        idx.sort(); idx.dedup();
        idx.into_iter().filter_map(|i| self.entries.get(i).cloned()).collect()
    }
    fn save(&self) -> SftpSide {
        match &self.src {
            Src::Empty => SftpSide { kind: "empty".into(), target: None, name: String::new(), dir: String::new() },
            Src::Local => SftpSide { kind: "local".into(), target: None, name: String::new(), dir: self.dir() },
            Src::Remote(r) => SftpSide { kind: "remote".into(), target: Some(r.target.clone()), name: r.name.clone(), dir: if r.sess.is_some() { self.rdir.clone() } else { r.want.clone().unwrap_or_default() } },
        }
    }
}

pub struct SftpView {
    rt: tokio::runtime::Handle,
    prompts: ssh::Prompts,
    panes: [Pane; 2],
    /// Pane the toolbar / keyboard acts on (last clicked).
    active: usize,
    pub status: String,
    error: bool,
    edit: Option<(Edit, usize, String, Entity<InputState>)>,
    confirm_delete: Option<(usize, String)>,
    /// Built-in editor: (pane, path, version when opened, editor, overwrite on next save).
    editor: Option<(usize, String, sftp::Version, Entity<EditorState>, bool)>,
    ticking: bool,
    _subs: Vec<gpui::Subscription>,
    sync: Option<SyncUi>,
    ctx_menu: Option<(usize, crate::ctxmenu_view::MenuState)>,
    /// "Actions" dropdown: pane and anchor.
    ops_menu: Option<(usize, Point<Pixels>, std::time::Instant)>,
    pending_edit: Option<String>,
    follow_src: Option<(gpui::WeakEntity<crate::termview::TermView>, bool)>,
    pub follow: Follow,
    follow_task: bool,
    next_token: u64,
}

/// Sync panel state: mode, delete flag, computed plan, delete confirmation pending.
struct SyncUi { mode: ssh_term_proto::dirsync::Mode, delete: bool, plan: Option<Vec<ssh_term_proto::dirsync::Op>>, busy: bool, confirm_delete: bool }

fn default_local_dir() -> PathBuf {
    for key in ["USERPROFILE", "HOME"] {
        if let Some(p) = std::env::var_os(key) {
            let p = PathBuf::from(p);
            if p.is_dir() { return p; }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Saved host name for a target (sidebar name), else `user@host:port`.
fn host_name(t: &SshTarget) -> String {
    config_hosts().iter().find(|(_, x)| x.pool_key() == t.pool_key()).map(|(h, _)| h.name.clone()).unwrap_or_else(|| t.label())
}
fn config_hosts() -> std::sync::Arc<Vec<(HostEntry, SshTarget)>> { crate::config::resolved_hosts() }

/// Recursive local copy; returns the number of files copied.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<usize> {
    if from.is_dir() {
        if to.starts_with(from) { return Err(std::io::Error::other("cannot copy a folder into itself")); }
        std::fs::create_dir_all(to)?;
        let mut n = 0;
        for e in std::fs::read_dir(from)?.flatten() { n += copy_tree(&e.path(), &to.join(e.file_name()))?; }
        Ok(n)
    } else { std::fs::copy(from, to).map(|_| 1) }
}

impl SftpView {
    /// Left pane local; right pane connects to `target` (or shows the "connect to host" state).
    /// Does not connect yet: set `initial_dir`, then call [`start`].
    pub fn new_deferred(target: Option<SshTarget>, rt: tokio::runtime::Handle, prompts: ssh::Prompts, _w: &mut Window, _cx: &mut Context<Self>) -> Self {
        let mut me = Self { rt, prompts, panes: [Pane::new(Src::Empty), Pane::new(Src::Empty)], active: 1, status: String::new(), error: false, edit: None, confirm_delete: None, editor: None, ticking: false, _subs: vec![], sync: None, ctx_menu: None, ops_menu: None, pending_edit: None, follow_src: None, follow: Follow::new(false), follow_task: false, next_token: 0 };
        if let Some(t) = target { me.panes[1].src = Src::Remote(Box::new(me.new_remote(host_name(&t), t))); }
        // 左栏默认是本机文件；主机选择器只在栏头下拉菜单里打开。右栏没有目标主机时仍显示主机列表
        me.panes[0].src = Src::Local;
        me.refresh_local(0);
        me
    }
    fn new_remote(&mut self, name: String, target: SshTarget) -> Remote {
        self.next_token += 1;
        Remote { name, target, sess: None, conn: None, connecting: false, error: None, home: String::new(), queue: Queue::default(), want: None, token: self.next_token }
    }
    /// Connect every remote pane that is not connected yet.
    pub fn start(&mut self, cx: &mut Context<Self>) { for pi in 0..2 { if self.panes[pi].remote().is_some_and(|r| r.sess.is_none()) { self.connect(pi, cx); } } }
    /// Directory the remote pane opens first (sftp:// link path).
    pub fn set_initial_dir(&mut self, d: Option<String>) { if let Some(r) = self.panes[1].remote_mut() { r.want = d; } }
    /// Host of the remote pane (right pane first).
    pub fn target(&self) -> Option<SshTarget> { self.panes[1].target().or_else(|| self.panes[0].target()) }
    fn remote_pane(&self) -> Option<usize> { [1, 0].into_iter().find(|&i| self.panes[i].remote().is_some()) }
    fn local_pane(&self) -> Option<usize> { [0, 1].into_iter().find(|&i| self.panes[i].is_local()) }
    /// Tab / window title: "SFTP · host" once a host is chosen.
    pub fn title(&self) -> String {
        match self.remote_pane() { Some(i) => ssh_term_proto::sftp_follow::title(&self.panes[i].label()), None => "SFTP".into() }
    }
    pub fn local_dir(&self) -> String { self.local_pane().map(|i| self.panes[i].dir()).unwrap_or_default() }
    pub fn set_local_dir(&mut self, d: &str) {
        let Some(i) = self.local_pane() else { return };
        if !d.is_empty() && Path::new(d).is_dir() { self.panes[i].ldir = d.into(); self.refresh_local(i); }
    }
    /// Restore both panes from a saved layout.
    pub fn restore(&mut self, sides: &[SftpSide], cx: &mut Context<Self>) {
        for (pi, s) in sides.iter().take(2).enumerate() {
            match s.kind.as_str() {
                "local" => { self.panes[pi].src = Src::Local; if !s.dir.is_empty() && Path::new(&s.dir).is_dir() { self.panes[pi].ldir = s.dir.clone().into(); } self.refresh_local(pi); }
                "remote" => if let Some(t) = s.target.clone() {
                    let name = if s.name.is_empty() { host_name(&t) } else { s.name.clone() };
                    let mut r = self.new_remote(name, t);
                    r.want = (!s.dir.is_empty()).then(|| s.dir.clone());
                    self.panes[pi].src = Src::Remote(Box::new(r));
                    self.panes[pi].entries.clear();
                },
                _ => { self.panes[pi].src = Src::Empty; self.panes[pi].entries.clear(); }
            }
        }
        cx.notify();
    }
    /// Attach (or detach with None) the terminal whose cwd this view follows.
    pub fn set_follow_source(&mut self, src: Option<(gpui::WeakEntity<crate::termview::TermView>, bool)>, on: bool, cx: &mut Context<Self>) {
        self.follow_src = src;
        self.follow.set(on && self.follow_src.is_some());
        if self.follow_src.is_some() && !self.follow_task {
            self.follow_task = true;
            cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(std::time::Duration::from_millis(700)).await;
                let alive = this.update(cx, |v, cx| {
                    let Some((src, local)) = v.follow_src.clone() else { v.follow_task = false; return false };
                    if !v.follow.on { return true; }
                    let Some(term) = src.upgrade() else { v.follow_src = None; v.follow_task = false; return false };
                    let cwd = term.read(cx).shell_cwd();
                    if local {
                        if let Some(pi) = v.local_pane() {
                            let home = std::env::var("HOME").unwrap_or_default();
                            let cur = v.panes[pi].dir();
                            if let Some(d) = v.follow.step(cwd.as_deref(), &home, &cur) { v.goto(pi, d.clone(), cx); v.set_status(tf("sftp.followed", &[("d", &d)]), false); }
                        }
                    } else if let Some(pi) = [1, 0].into_iter().find(|&i| v.panes[i].sess().is_some()) {
                        let home = v.panes[pi].remote().map(|r| r.home.clone()).unwrap_or_default();
                        let cur = v.panes[pi].rdir.clone();
                        if let Some(d) = v.follow.step(cwd.as_deref(), &home, &cur) { v.goto(pi, d.clone(), cx); v.set_status(tf("sftp.followed", &[("d", &d)]), false); }
                    }
                    true
                });
                if !matches!(alive, Ok(true)) { break; }
            }).detach();
        }
        cx.notify();
    }
    /// Snapshot for layout persistence (legacy fields kept for older readers).
    pub fn save(&self) -> ssh_term_proto::session_store::PaneSave {
        let r = self.remote_pane();
        ssh_term_proto::session_store::PaneSave::Sftp {
            target: self.target().unwrap_or_default(),
            local_dir: self.local_dir(),
            remote_dir: r.map(|i| self.panes[i].save().dir).unwrap_or_default(),
            follow: self.follow.on,
            panes: vec![self.panes[0].save(), self.panes[1].save()],
        }
    }
    fn set_status(&mut self, s: String, err: bool) { self.status = s; self.error = err; }

    // ---- sources
    fn connect(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = self.prompts.clone();
        let Some(r) = self.panes[pi].remote_mut() else { return };
        if r.connecting { return; }
        r.connecting = true;
        r.error = None;
        let (t, want, token, name) = (r.target.clone(), r.want.take(), r.token, r.name.clone());
        self.panes[pi].entries.clear();
        self.panes[pi].clear_sel();
        self.set_status(tf("sftp.connecting", &[("t", &name)]), false);
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let r = async {
                let (conn, _) = ssh::connect_shared(&t, &p).await?;
                let s = conn.sftp().await?;
                let real_home = s.canonicalize(".").await.unwrap_or_default();
                let dir = match want.as_deref().map(|d| d.strip_prefix("~/").map(str::to_string).unwrap_or_else(|| d.trim_start_matches('~').to_string())) {
                    Some(d) if !d.is_empty() => match s.canonicalize(d.clone()).await { Ok(p) => p, Err(_) => s.canonicalize(".").await? },
                    _ => s.canonicalize(".").await?,
                };
                let ls = sftp::list_remote(&s, &dir, false).await?;
                Ok::<_, anyhow::Error>((Arc::new(s), dir, ls, conn, real_home))
            }.await;
            let _ = tx.send(r);
        });
        cx.spawn(async move |this, cx| {
            let r = rx.recv_async().await;
            let _ = this.update(cx, |v, cx| {
                let hidden = v.panes[pi].hidden;
                let out = {
                    let Some(rm) = v.panes[pi].remote_mut().filter(|r| r.token == token) else { return };
                    rm.connecting = false;
                    match r {
                        Ok(Ok((s, dir, ls, conn, rh))) => { rm.home = rh; rm.sess = Some(s); rm.conn = Some(conn); Some(Ok((dir, ls))) }
                        Ok(Err(e)) => { let m = format!("{e:#}"); rm.error = Some(m.clone()); Some(Err(m)) }
                        Err(_) => None,
                    }
                };
                match out {
                    Some(Ok((dir, ls))) => {
                        let pane = &mut v.panes[pi];
                        pane.rdir = dir; pane.entries = ls; pane.back.clear(); pane.fwd.clear();
                        v.set_status(tf("sftp.connected", &[("t", &name)]), false);
                        if hidden { v.refresh(pi, cx); }
                        cx.emit(SftpEvent::Retitle);
                    }
                    Some(Err(m)) => v.set_status(tf("sftp.connect_failed", &[("e", &m)]), true),
                    None => {}
                }
                cx.notify();
            });
        }).detach();
    }
    fn choose_host(&mut self, pi: usize, h: HostEntry, t: SshTarget, cx: &mut Context<Self>) {
        let _ = crate::audit_log().append("sftp.pick_host", "user", &format!("pane={pi} host={}", t.label()));
        let r = self.new_remote(h.name.clone(), t);
        let p = &mut self.panes[pi];
        p.src = Src::Remote(Box::new(r));
        p.picker = None; p.entries.clear(); p.back.clear(); p.fwd.clear(); p.clear_sel(); p.rdir.clear();
        self.sync = None;
        self.connect(pi, cx);
        cx.emit(SftpEvent::Retitle);
        cx.notify();
    }
    /// Sidebar click in the SFTP module: connect a pane that is still choosing a host
    /// (active pane first). Returns false when every pane already has a source.
    pub fn connect_free_pane(&mut self, t: SshTarget, cx: &mut Context<Self>) -> bool {
        let free = |p: &Pane| matches!(p.src, Src::Empty) || p.picker.is_some();
        let Some(pi) = [self.active, 1 - self.active.min(1)].into_iter().find(|&i| free(&self.panes[i])) else { return false };
        let name = host_name(&t);
        let h = crate::config::get().all_hosts().into_iter().find(|h| h.name == name).unwrap_or_else(|| HostEntry { name: name.clone(), host: t.host.clone(), ..Default::default() });
        self.choose_host(pi, h, t, cx);
        true
    }
    fn choose_local(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        p.src = Src::Local;
        p.picker = None; p.back.clear(); p.fwd.clear(); p.clear_sel();
        self.sync = None;
        self.refresh_local(pi);
        cx.emit(SftpEvent::Retitle);
        cx.notify();
    }
    fn disconnect(&mut self, pi: usize, cx: &mut Context<Self>) {
        if let Some(r) = self.panes[pi].remote() { let _ = crate::audit_log().append("sftp.disconnect", "user", &format!("pane={pi} host={}", r.target.label())); }
        let p = &mut self.panes[pi];
        p.src = Src::Empty; p.entries.clear(); p.clear_sel(); p.back.clear(); p.fwd.clear(); p.filter = None;
        self.sync = None;
        if self.editor.as_ref().is_some_and(|e| e.0 == pi) { self.editor = None; }
        cx.emit(SftpEvent::Retitle);
        cx.notify();
    }
    fn open_picker(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        let st = cx.new(|cx| InputState::new(window, cx).placeholder(t("sftp.search_hosts")));
        st.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(cx.subscribe_in(&st, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
        self.panes[pi].picker = Some(Picker { search: st, group: String::new() });
        self.ops_menu = None;
        cx.notify();
    }
    fn picker_back(&mut self, pi: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.panes[pi].picker.as_mut() {
            // an empty pane always shows the host list, so there is nothing to go back to
            if p.group.is_empty() { if !matches!(self.panes[pi].src, Src::Empty) { self.panes[pi].picker = None; } } else { p.group = hosttree::parent_of(&p.group).to_string(); }
        }
        cx.notify();
    }
    fn toggle_filter(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes[pi].filter.take().is_none() {
            let st = cx.new(|cx| InputState::new(window, cx).placeholder(t("sftp.filter_ph")));
            st.update(cx, |s, cx| s.focus(window, cx));
            self._subs.push(cx.subscribe_in(&st, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
            self.panes[pi].filter = Some(st);
        }
        cx.notify();
    }

    // ---- listing / navigation
    fn refresh_local(&mut self, pi: usize) {
        let p = &mut self.panes[pi];
        if !p.is_local() { return; }
        if p.ldir.as_os_str().is_empty() { p.entries = windows_drives(); return; }
        match sftp::list_local(&p.ldir, p.hidden) {
            Ok(v) => p.entries = v,
            Err(e) => { p.entries.clear(); let m = format!("{}: {e}", p.ldir.display()); self.set_status(m, true) }
        }
    }
    fn refresh(&mut self, pi: usize, cx: &mut Context<Self>) {
        if self.panes[pi].is_local() { return self.refresh_local(pi); }
        let p = &mut self.panes[pi];
        let Some(s) = p.sess() else { return };
        p.gen = p.gen.wrapping_add(1);
        let (gen, dir, hidden) = (p.gen, p.rdir.clone(), p.hidden);
        p.loading = true;
        p.entries.clear();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(sftp::list_remote(&s, &dir, hidden).await); });
        cx.spawn(async move |this, cx| {
            if let Ok(r) = rx.recv_async().await {
                let _ = this.update(cx, |v, cx| {
                    let p = &mut v.panes[pi];
                    if p.gen != gen { return; }
                    p.loading = false;
                    match r { Ok(ls) => p.entries = ls, Err(e) => v.set_status(format!("{e:#}"), true) }
                    cx.notify();
                });
            }
        }).detach();
    }
    fn refresh_all(&mut self, cx: &mut Context<Self>) { self.refresh(0, cx); self.refresh(1, cx); }
    fn set_dir(&mut self, pi: usize, dir: String, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        p.clear_sel();
        if p.is_local() { p.ldir = PathBuf::from(dir); } else { p.rdir = dir; }
        self.refresh(pi, cx);
        cx.notify();
    }
    /// Navigate (recorded in back/forward history).
    fn goto(&mut self, pi: usize, dir: String, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        let cur = p.dir();
        if cur != dir { p.back.push(cur); p.fwd.clear(); }
        self.set_dir(pi, dir, cx);
    }
    fn go_back(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        let Some(d) = p.back.pop() else { return };
        p.fwd.push(p.dir());
        self.set_dir(pi, d, cx);
    }
    fn go_fwd(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        let Some(d) = p.fwd.pop() else { return };
        p.back.push(p.dir());
        self.set_dir(pi, d, cx);
    }
    fn open(&mut self, pi: usize, i: usize, cx: &mut Context<Self>) {
        let p = &self.panes[pi];
        let Some(e) = p.entries.get(i).filter(|e| e.is_dir).cloned() else { return };
        let dir = if p.is_local() {
            if p.ldir.as_os_str().is_empty() { format!("{}:\\", e.name.trim_end_matches(':')) } else { p.ldir.join(&e.name).to_string_lossy().into_owned() }
        } else { sftp::join(&p.rdir, &e.name) };
        self.goto(pi, dir, cx);
    }
    fn up(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = &self.panes[pi];
        let dir = if p.is_local() {
            if p.ldir.as_os_str().is_empty() { return; }
            if is_drive_root(&p.ldir) { String::new() } else { match p.ldir.parent() { Some(x) => x.to_string_lossy().into_owned(), None => return } }
        } else { sftp::parent(&p.rdir) };
        self.goto(pi, dir, cx);
    }
    fn select(&mut self, pi: usize, i: usize, m: gpui::Modifiers) {
        if self.active != pi { self.panes[self.active].clear_sel(); self.active = pi; }
        let p = &mut self.panes[pi];
        if m.control || m.platform {
            if let Some(x) = p.multi.iter().position(|&x| x == i) { p.multi.remove(x); } else { p.multi.push(i); }
            p.sel = Some(i);
        } else if m.shift && p.sel.is_some() {
            let a = p.sel.unwrap_or(i);
            p.multi = (a.min(i)..=a.max(i)).collect();
        } else { p.sel = Some(i); p.multi = vec![i]; }
    }
    fn selected(&self) -> Option<(usize, Entry)> {
        let p = &self.panes[self.active];
        p.sel.and_then(|i| p.entries.get(i).cloned()).map(|e| (self.active, e))
    }
    fn open_ctx_menu(&mut self, pi: usize, i: usize, pos: Point<Pixels>, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::{FileObj, Place, Target};
        let in_sel = self.active == pi && (self.panes[pi].multi.contains(&i) || self.panes[pi].sel == Some(i));
        if !in_sel { self.panes[self.active].clear_sel(); self.active = pi; self.panes[pi].sel = Some(i); self.panes[pi].multi = vec![i]; }
        let p = &self.panes[pi];
        let files: Vec<FileObj> = p.selection().into_iter().map(|e| FileObj { path: p.path_of(&e.name), is_dir: e.is_dir, mode: e.mode }).collect();
        if files.is_empty() { return; }
        let tgt = p.target();
        let (place, host) = match &tgt { Some(t) => (Place::RemotePanel, t.host.clone()), None => (Place::LocalPanel, "local".to_string()) };
        let key = crate::ctxmenu_view::host_key(tgt.as_ref());
        self.ctx_menu = Some((pi, crate::ctxmenu_view::build(Target::Files(files), place, &host, key, pos, None)));
        if let Some(rx) = crate::ctxmenu_view::ensure_probe(tgt) {
            cx.spawn(async move |this, cx| {
                let _ = rx.recv_async().await;
                let _ = this.update(cx, |v, cx| {
                    if let Some((pi, m)) = &v.ctx_menu {
                        let pi = *pi;
                        let (place, host) = match v.panes[pi].target() { Some(t) => (Place::RemotePanel, t.host), None => (Place::LocalPanel, "local".into()) };
                        let nm = m.rebuilt(place, &host);
                        v.ctx_menu = Some((pi, nm));
                        cx.notify();
                    }
                });
            }).detach();
        }
        cx.notify();
    }
    fn ctx_pick(&mut self, i: usize, _w: &mut Window, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::Act;
        let Some((pi, m)) = self.ctx_menu.take() else { return };
        let Some(item) = m.menu.items.get(i).cloned() else { return };
        let target = self.panes[pi].target();
        let _ = crate::audit_log().append("ctxmenu.pick", "user", &format!("host={} panel={} item={} custom={}", target.as_ref().map(|t| t.host.as_str()).unwrap_or("local"), if target.is_some() { "remote" } else { "local" }, item.id, item.custom));
        match item.act {
            Act::Insert(command) => cx.emit(SftpEvent::Insert { target, command }),
            Act::CdNewTab(dir) => cx.emit(SftpEvent::CdTab { target, dir }),
            Act::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
            Act::OpenEditor(p) => self.edit_in(pi, p, cx),
            Act::Download(paths) | Act::Upload(paths) => { let names = paths.into_iter().map(|p| { let n = p.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("file").to_string(); (p, n) }).collect(); self.send(pi, 1 - pi, names, cx) }
            Act::OpenUrl(u) => cx.open_url(&u),
            Act::Search(_) | Act::AiExplain(_) | Act::PasteInput(_) | Act::UseAiContext | Act::OpenTool(..) => {}
        }
        cx.notify();
    }
    fn ctx_close(&mut self, cx: &mut Context<Self>) { if self.ctx_menu.take().is_some() { cx.notify(); } }

    // ---- transfers
    /// Run on the tokio runtime, then apply on the UI thread; errors go to the status line.
    fn remote_op<T: Send + 'static>(&mut self, cx: &mut Context<Self>, fut: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static, apply: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static) {
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(fut.await); });
        cx.spawn(async move |this, cx| {
            if let Ok(r) = rx.recv_async().await {
                let _ = this.update(cx, |v, cx| { match r { Ok(x) => apply(v, x, cx), Err(e) => v.set_status(format!("{e:#}"), true) } cx.notify(); });
            }
        }).detach();
    }
    fn after_change(&mut self, msg: String, cx: &mut Context<Self>) {
        self.set_status(msg, false);
        self.refresh_all(cx);
    }
    /// Copy `items` (path, name) from pane `from` into the current directory of pane `to`.
    fn send(&mut self, from: usize, to: usize, items: Vec<(String, String)>, cx: &mut Context<Self>) {
        if items.is_empty() { return self.set_status(t("sftp.select_first").into(), true); }
        if from == to { return; }
        if !self.panes[to].ready() || !self.panes[from].ready() { return self.set_status(t("sftp.not_connected").into(), true); }
        if self.panes[to].is_local() && self.panes[to].ldir.as_os_str().is_empty() { return self.set_status(t("sftp.no_target_dir").into(), true); }
        match (self.panes[from].is_local(), self.panes[to].is_local()) {
            (true, false) => for (p, _) in items { self.upload_to(to, PathBuf::from(p), cx) },
            (false, true) => for (p, n) in items { self.download_from(from, p, n, to, cx) },
            (true, true) => self.copy_local(items, to, cx),
            (false, false) => for (p, n) in items { self.relay(from, p, n, to, cx) },
        }
        cx.notify();
    }
    /// Upload a local file / folder (also from the system file manager) into remote pane `to`.
    fn upload_to(&mut self, to: usize, local: PathBuf, cx: &mut Context<Self>) {
        let Some(s) = self.panes[to].sess() else { return self.set_status(t("sftp.not_connected").into(), true) };
        let name = local.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let remote = sftp::join(&self.panes[to].rdir, &name);
        let Some(q) = self.panes[to].remote().map(|r| r.queue.clone()) else { return };
        self.start_worker(cx);
        self.remote_op(cx, async move { q.push_upload_tree(&s, &local, &remote).await }, |v, n, _| v.set_status(tf("sftp.queued", &[("n", &n)]), false));
    }
    fn download_from(&mut self, from: usize, remote: String, name: String, to: usize, cx: &mut Context<Self>) {
        let Some(s) = self.panes[from].sess() else { return self.set_status(t("sftp.not_connected").into(), true) };
        let local = self.panes[to].ldir.join(name);
        let Some(q) = self.panes[from].remote().map(|r| r.queue.clone()) else { return };
        self.start_worker(cx);
        self.remote_op(cx, async move { q.push_download_tree(&s, &remote, &local).await }, |v, n, _| v.set_status(tf("sftp.queued", &[("n", &n)]), false));
    }
    fn copy_local(&mut self, items: Vec<(String, String)>, to: usize, cx: &mut Context<Self>) {
        let dst = self.panes[to].ldir.clone();
        let _ = crate::audit_log().append("sftp.copy_local", "user", &format!("n={} to={:?}", items.len(), dst.display()));
        self.remote_op(cx, async move {
            tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
                let mut n = 0;
                for (p, name) in items {
                    let (a, b) = (PathBuf::from(&p), dst.join(&name));
                    if a == b { anyhow::bail!("{}", tf("sftp.same_place", &[("p", &p)])); }
                    n += copy_tree(&a, &b).map_err(|e| anyhow::anyhow!("{p}: {e}"))?;
                }
                Ok(n)
            }).await?
        }, |v, n, cx| v.after_change(tf("sftp.copied", &[("n", &n)]), cx));
    }
    /// Remote → remote: download into a temporary folder on this computer, then queue the upload.
    fn relay(&mut self, from: usize, path: String, name: String, to: usize, cx: &mut Context<Self>) {
        let (Some(sa), Some(sb)) = (self.panes[from].sess(), self.panes[to].sess()) else { return self.set_status(t("sftp.not_connected").into(), true) };
        let Some(qb) = self.panes[to].remote().map(|r| r.queue.clone()) else { return };
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let tmp_root = std::env::temp_dir().join(format!("shellax-relay-{stamp}"));
        let tmp = tmp_root.join(&name);
        let dest = sftp::join(&self.panes[to].rdir, &name);
        let (ha, hb) = (self.panes[from].label(), self.panes[to].label());
        let _ = crate::audit_log().append("sftp.relay", "user", &format!("from={ha} path={path:?} to={hb} dest={dest:?}"));
        self.set_status(tf("sftp.relaying", &[("p", &name), ("a", &ha), ("b", &hb)]), false);
        self.start_worker(cx);
        self.remote_op(cx, async move {
            std::fs::create_dir_all(&tmp_root)?;
            let tq = Queue::default();
            tq.push_download_tree(&sa, &path, &tmp).await?;
            tq.drain(&sa).await;
            let failed: Vec<String> = tq.jobs.lock().unwrap().iter().filter(|j| j.state() != JobState::Done).map(|j| j.error.lock().unwrap().clone().unwrap_or_else(|| j.name())).collect();
            if !failed.is_empty() { let _ = std::fs::remove_dir_all(&tmp_root); anyhow::bail!("{}", failed.join("; ")); }
            qb.push_upload_tree(&sb, &tmp, &dest).await
        }, |v, n, cx| { v.start_worker(cx); v.set_status(tf("sftp.relay_queued", &[("n", &n)]), false) });
    }
    /// Remote panes with their transfer queues.
    pub(crate) fn queues(&self) -> Vec<(usize, Queue)> { (0..2).filter_map(|i| self.panes[i].remote().map(|r| (i, r.queue.clone()))).collect() }
    fn start_worker(&mut self, cx: &mut Context<Self>) {
        for p in &self.panes { if let Some(r) = p.remote() { if let Some(s) = r.sess.clone() { r.queue.start(&self.rt, s, || {}); } } }
        if self.ticking { return; }
        self.ticking = true;
        // repaint while jobs are active; refresh listings when they finish
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
            let Ok(busy) = this.update(cx, |v, cx| {
                let busy = v.queues().iter().any(|(_, q)| q.jobs.lock().unwrap().iter().any(|j| matches!(j.state(), JobState::Queued | JobState::Running)));
                if !busy { v.ticking = false; v.refresh_all(cx); }
                cx.notify();
                busy
            }) else { break };
            if !busy { break; }
        }).detach();
    }
    /// Send the selection of the active pane to the other pane.
    fn transfer_from(&mut self, from: usize, cx: &mut Context<Self>) {
        if self.active != from || self.panes[from].selection().is_empty() { self.set_status(t("sftp.select_first").into(), true); cx.notify(); return; }
        let p = &self.panes[from];
        let items = p.selection().into_iter().map(|e| (p.path_of(&e.name), e.name)).collect();
        self.send(from, 1 - from, items, cx);
        cx.notify();
    }

    // ---- file operations
    fn begin_edit(&mut self, kind: Edit, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.ops_menu = None;
        if !self.panes[pi].ready() { self.set_status(t("sftp.not_connected").into(), true); cx.notify(); return; }
        let sel = if self.active == pi { self.selected().map(|s| s.1) } else { None };
        let (name, init) = match (kind, sel) {
            (Edit::Mkdir, _) => (String::new(), String::new()),
            (Edit::Rename, Some(e)) => (e.name.clone(), e.name.clone()),
            (Edit::Chmod, Some(e)) => (e.name.clone(), format!("{:o}", e.mode)),
            _ => { self.set_status(t("sftp.select_first").into(), true); cx.notify(); return }
        };
        let ph = match kind { Edit::Mkdir => t("sftp.new_folder_name"), Edit::Rename => t("sftp.new_name"), Edit::Chmod => "644" };
        let st = cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(init));
        st.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(cx.subscribe_in(&st, window, |v, _, ev: &InputEvent, _w, cx| if matches!(ev, InputEvent::PressEnter { .. }) { v.commit_edit(cx) }));
        self.edit = Some((kind, pi, name, st));
        cx.notify();
    }
    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some((kind, pi, name, st)) = self.edit.take() else { return };
        let val = st.read(cx).value().trim().to_string();
        if val.is_empty() { cx.notify(); return; }
        let p = &self.panes[pi];
        let (s, side) = (p.sess(), p.side());
        let (a, b) = (p.path_of(&name), p.path_of(&val));
        let msg = match kind { Edit::Mkdir => tf("sftp.created", &[("p", &b)]), Edit::Rename => tf("sftp.renamed", &[("a", &name), ("b", &val)]), Edit::Chmod => tf("sftp.chmoded", &[("p", &name), ("m", &val)]) };
        let Some(mode) = (kind != Edit::Chmod).then_some(0).or_else(|| sftp::parse_mode(&val)) else { self.set_status(t("sftp.bad_mode").into(), true); cx.notify(); return };
        self.remote_op(cx, async move {
            match kind {
                Edit::Mkdir => sftp::mkdir(s.as_deref(), side, &b).await,
                Edit::Rename => sftp::rename(s.as_deref(), side, &a, &b).await,
                Edit::Chmod => sftp::chmod(s.as_deref(), side, &a, mode).await,
            }
        }, move |v, _, cx| v.after_change(msg, cx));
        cx.notify();
    }
    /// Open the selected remote file of pane `pi` in the built-in editor.
    fn edit_selected(&mut self, pi: usize, cx: &mut Context<Self>) {
        self.ops_menu = None;
        let sel = if self.active == pi { self.selected().map(|s| s.1) } else { None };
        let Some(e) = sel.filter(|e| !e.is_dir && !self.panes[pi].is_local()) else { self.set_status(t("sftp.edit_select").into(), true); cx.notify(); return };
        let path = self.panes[pi].path_of(&e.name);
        self.edit_in(pi, path, cx);
    }
    /// Open a remote text file of the host pane in the built-in editor (waits for the connection).
    pub fn edit_path(&mut self, path: String, cx: &mut Context<Self>) {
        match self.remote_pane() { Some(pi) => self.edit_in(pi, path, cx), None => self.set_status(t("sftp.not_connected").into(), true) }
    }
    fn edit_in(&mut self, pi: usize, path: String, cx: &mut Context<Self>) {
        let Some(s) = self.panes[pi].sess() else { if self.panes[pi].remote().is_some() { self.pending_edit = Some(path); } cx.notify(); return };
        let host = self.panes[pi].target().map(|t| t.host).unwrap_or_default();
        let p2 = path.clone();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(sftp::read_text(&s, &p2).await); });
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let Some(window) = cx.update(|cx| cx.active_window()) else { return };
            let _ = window.update(cx, |_, window, cx| this.update(cx, |v, cx| {
                match r {
                    Ok((text, ver)) => {
                        let st = cx.new(|cx| EditorState::new(window, cx).line_number(true).default_value(text));
                        let _ = crate::audit_log().append("sftp.edit.open", "user", &format!("host={host} path={path:?} bytes={}", ver.0));
                        v.editor = Some((pi, path.clone(), ver, st, false));
                        v.set_status(tf("sftp.edit_opened", &[("p", &path)]), false);
                    }
                    Err(e) => v.set_status(format!("{e:#}"), true),
                }
                cx.notify();
            }));
        }).detach();
    }
    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some((pi, path, ver, st, force)) = self.editor.clone() else { return };
        let Some(s) = self.panes[pi].sess() else { return };
        let text = st.read(cx).value().to_string();
        let (p2, host) = (path.clone(), self.panes[pi].target().map(|t| t.host).unwrap_or_default());
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(sftp::write_text(&s, &p2, &text, (!force).then_some(ver)).await.map(|v| (v, text.len()))); });
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok((nv, n)) => {
                        let _ = crate::audit_log().append("sftp.edit.save", "user", &format!("host={host} path={path:?} bytes={n} overwrite={force}"));
                        if let Some(ed) = v.editor.as_mut() { ed.2 = nv; ed.4 = false; }
                        v.after_change(tf("sftp.edit_saved", &[("p", &path), ("n", &n)]), cx);
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        if msg.contains("changed on the server") { if let Some(ed) = v.editor.as_mut() { ed.4 = true; } let _ = crate::audit_log().append("sftp.edit.conflict", "policy", &format!("host={host} path={path:?}")); }
                        v.set_status(msg, true);
                    }
                }
                cx.notify();
            });
        }).detach();
    }
    fn ask_delete(&mut self, pi: usize, cx: &mut Context<Self>) {
        self.ops_menu = None;
        match self.selected().filter(|(p, _)| *p == pi) {
            Some((_, e)) => self.confirm_delete = Some((pi, self.panes[pi].path_of(&e.name))),
            None => self.set_status(t("sftp.select_first").into(), true),
        }
        cx.notify();
    }
    fn delete_confirmed(&mut self, cx: &mut Context<Self>) {
        let Some((pi, path)) = self.confirm_delete.take() else { return };
        let (s, side) = (self.panes[pi].sess(), self.panes[pi].side());
        let p2 = path.clone();
        self.remote_op(cx, async move { sftp::remove(s.as_deref(), side, &p2).await }, move |v, _, cx| v.after_change(tf("sftp.deleted", &[("p", &path)]), cx));
        self.panes[pi].clear_sel();
        cx.notify();
    }

    // ---- directory sync (one local pane + one connected host pane)
    fn sync_pair(&self) -> Option<(usize, usize)> {
        let l = self.local_pane()?;
        let r = 1 - l;
        self.panes[r].sess().is_some().then_some((l, r))
    }
    fn sync_preview(&mut self, cx: &mut Context<Self>) {
        let Some((l, r)) = self.sync_pair() else { return };
        let (Some(s), Some(su)) = (self.panes[r].sess(), self.sync.as_mut()) else { return };
        su.busy = true; su.plan = None; su.confirm_delete = false;
        let (mode, del, ld, rd) = (su.mode, su.delete, self.panes[l].ldir.clone(), self.panes[r].rdir.clone());
        self.remote_op(cx, async move {
            let l = ssh_term_proto::dirsync::scan_local(&ld)?;
            let r = sftp::scan_remote(&s, &rd).await?;
            Ok(ssh_term_proto::dirsync::plan(&l, &r, mode, del))
        }, |v, plan, _| { if let Some(su) = v.sync.as_mut() { su.busy = false; v.status = tf("sync.previewed", &[("n", &plan.len())]); su.plan = Some(plan); } });
    }
    fn sync_run(&mut self, cx: &mut Context<Self>) {
        let Some((l, r)) = self.sync_pair() else { return };
        let (Some(s), Some(su)) = (self.panes[r].sess(), self.sync.as_mut()) else { return };
        let Some(plan) = su.plan.clone() else { return };
        let dels = plan.iter().filter(|o| o.is_delete()).count();
        if dels > 0 && !su.confirm_delete { su.confirm_delete = true; cx.notify(); return; }
        su.busy = true;
        let smode = su.mode;
        self.start_worker(cx);
        let Some(q) = self.panes[r].remote().map(|x| x.queue.clone()) else { return };
        let (ld, rd, host) = (self.panes[l].ldir.clone(), self.panes[r].rdir.clone(), self.panes[r].target().map(|t| t.host).unwrap_or_default());
        let _ = crate::audit_log().append("sftp.sync", "user", &format!("host={host} local={:?} remote={rd:?} mode={smode:?} ops={} deletes={dels}", ld.display(), plan.len()));
        self.remote_op(cx, async move { sftp::execute_sync(&s, &q, &ld, &rd, &plan).await }, |v, (q, d, c), cx| {
            v.sync = None;
            v.start_worker(cx);
            v.after_change(tf("sync.done", &[("q", &q), ("d", &d), ("c", &c)]), cx);
        });
    }
}

fn human(n: u64) -> String {
    if n < 1024 { format!("{n} B") } else if n < 1 << 20 { format!("{:.1} KB", n as f64 / 1024.0) } else if n < 1 << 30 { format!("{:.1} MB", n as f64 / 1048576.0) } else { format!("{:.1} GB", n as f64 / 1073741824.0) }
}

fn is_drive_root(path: &Path) -> bool {
    let s = path.to_string_lossy().replace('/', "\\");
    let t = s.trim_end_matches('\\');
    t.len() == 2 && t.as_bytes().get(1) == Some(&b':')
}

fn windows_drives() -> Vec<Entry> {
    let mut v = Vec::new();
    for c in b'A'..=b'Z' {
        let root = format!("{}:\\", c as char);
        if Path::new(&root).exists() {
            v.push(Entry { name: format!("{}:", c as char), is_dir: true, size: 0, mode: 0, mtime: 0 });
        }
    }
    v
}

fn local_parts(path: &Path) -> Vec<(String, PathBuf)> {
    let mut cur = PathBuf::new();
    let mut out = Vec::new();
    for c in path.components() {
        use std::path::Component;
        match c {
            Component::Prefix(p) => {
                cur.push(p.as_os_str());
                let label = p.as_os_str().to_string_lossy().trim_end_matches(['\\', '/']).to_string();
                out.push((label, cur.clone()));
            }
            Component::RootDir => { cur.push(c); if out.is_empty() { out.push(("/".into(), cur.clone())); } }
            Component::Normal(s) => {
                cur.push(s);
                out.push((s.to_string_lossy().into_owned(), cur.clone()));
            }
            _ => { cur.push(c); }
        }
    }
    out
}

fn remote_skeleton() -> AnyElement {
    let th = cur();
    let widths = [148., 96., 188., 120., 164., 84., 210., 132.];
    let mut col = div().flex().flex_col();
    for (i, w) in widths.into_iter().enumerate() {
        col = col.child(div().h(px(32.)).px_3().flex().items_center().gap(px(10.))
            .child(div().id(("skel-ico", i)).size(px(16.)).flex_shrink_0().rounded(px(R_SM)).bg(c(th.surface1)))
            .child(div().id(("skel-name", i)).h(px(12.)).w(px(w)).rounded(px(R_SM)).bg(c(th.surface1)))
            .child(div().flex_1())
            .child(div().id(("skel-meta", i)).h(px(10.)).w(px(64.)).rounded(px(R_SM)).bg(c(th.surface0))));
    }
    if crate::motion::reduced() { return col.into_any_element(); }
    col.with_animation(
        "sftp-skel",
        gpui::Animation::new(std::time::Duration::from_millis(1100))
            .repeat_synced()
            .with_max_fps(30.)
            .with_easing(|t| (t * std::f32::consts::PI).sin()),
        |d, t| d.opacity(0.35 + 0.65 * t),
    ).into_any_element()
}

fn fmt_mtime(t: u64) -> String {
    if t == 0 { return "—".into(); }
    chrono::DateTime::from_timestamp(t as i64, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

/// File extension used for the "type" column ("" for folders and extension-less files).
fn ext_of(e: &Entry) -> String {
    if e.is_dir { return String::new(); }
    match e.name.rsplit_once('.') { Some((stem, x)) if !stem.is_empty() && !x.is_empty() => x.to_lowercase(), _ => String::new() }
}
fn kind_label(e: &Entry) -> String {
    if e.is_dir { return t("sftp.type_folder").to_string(); }
    let x = ext_of(e);
    if x.is_empty() { t("sftp.type_plain").to_string() } else { tf("sftp.type_file", &[("x", &x.to_uppercase())]) }
}

/// Directories stay above files. Within each group, the chosen column decides the order.
fn sorted_indices(entries: &[Entry], sort: PaneSort) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ea, eb) = (&entries[a], &entries[b]);
        let dir = match (ea.is_dir, eb.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        };
        let ord = match sort.key {
            SortKey::Name => ea.name.to_lowercase().cmp(&eb.name.to_lowercase()),
            SortKey::Mtime => ea.mtime.cmp(&eb.mtime),
            SortKey::Size => ea.size.cmp(&eb.size),
            SortKey::Kind => ext_of(ea).cmp(&ext_of(eb)).then(ea.name.to_lowercase().cmp(&eb.name.to_lowercase())),
        };
        dir.then(if sort.desc { ord.reverse() } else { ord }).then(a.cmp(&b))
    });
    idx
}

fn remote_parts(path: &str) -> Vec<(String, String)> {
    let mut acc = String::from("/");
    let mut out = vec![("/".into(), "/".into())];
    if path == "/" || path.is_empty() { return out; }
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        acc = if acc == "/" { format!("/{seg}") } else { format!("{acc}/{seg}") };
        out.push((seg.to_string(), acc.clone()));
    }
    out
}

/// Element id unique per pane.
fn pid(pi: usize, i: usize) -> usize { pi * 1_000_000 + i }

/// Small square icon button.
fn icon_btn(id: (&'static str, usize), ic: IconName, enabled: bool) -> gpui::Stateful<gpui::Div> {
    let th = cur();
    div().id(id).size(px(H_COMPACT)).flex_shrink_0().rounded(px(R_MD)).flex().items_center().justify_center()
        .when(enabled, |d| d.cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg())))
        .child(icon(ic, ui::IC_SM, if enabled { c(th.subtext) } else { c(th.overlay0).opacity(0.5) }))
}

impl SftpView {
    fn follow_bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut bar = div().flex().items_center().gap_1();
        if self.follow_src.is_some() {
            bar = bar.child(gpui_component::switch::Switch::new("sftp-follow").checked(self.follow.on).label(t("sftp.follow_cwd")).small()
                .on_click(cx.listener(|v, on: &bool, _, cx| { v.follow.set(*on); cx.notify() })));
        }
        bar
    }
    /// 页内标题行：沿用弹窗头的图标和字号，但高度改为工具栏，并取消左右内边距以与下方卡片对齐。
    fn page_head(&self, title: &str, cx: &mut Context<Self>) -> gpui::Div {
        ui::dialog_header(Some(IconName::FolderSync), c(cur().accent), title, None, None)
            .px_0()
            .h(px(ui::TOOLBAR_H))
            .child(self.follow_bar(cx))
    }
    fn frame(&self) -> gpui::Div {
        let th = cur();
        div().size_full().min_h_0().flex().flex_col().bg(c(th.base)).overflow_hidden()
    }
    fn sync_panel(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        use ssh_term_proto::dirsync::Mode;
        let (l, r) = self.sync_pair()?;
        let su = self.sync.as_ref()?;
        let th = cur();
        let modes = vec![("up".to_string(), t("sync.up").to_string()), ("down".to_string(), t("sync.down").to_string()), ("both".to_string(), t("sync.both").to_string())];
        let sel = match su.mode { Mode::LocalToRemote => "up", Mode::RemoteToLocal => "down", Mode::TwoWay => "both" };
        let me = cx.entity().downgrade();
        let mut list = div().id("sync-plan").max_h(px(180.)).overflow_y_scroll().flex().flex_col().text_size(px(fs_small())).font_family(ui::mono_family());
        if let Some(plan) = &su.plan {
            if plan.is_empty() { list = list.child(div().text_color(c(th.success)).child(t("sync.in_sync"))); }
            for op in plan.iter().take(300) {
                let col = if op.is_delete() { c(th.error) } else if matches!(op, ssh_term_proto::dirsync::Op::Conflict(_)) { c(th.warning) } else { c(th.text) };
                list = list.child(div().text_color(col).child(format!("{}  {}", op.sign(), op.path())));
            }
            if plan.len() > 300 { list = list.child(format!("… +{}", plan.len() - 300)); }
        }
        let dels = su.plan.as_ref().map(|p| p.iter().filter(|o| o.is_delete()).count()).unwrap_or(0);
        let mut bar = div().flex().items_center().gap_2()
            .child(ui::segmented("sync-mode", &modes, sel, move |v, _, cx| { let _ = me.update(cx, |p, cx| { if let Some(su) = p.sync.as_mut() { su.mode = match v.as_str() { "down" => Mode::RemoteToLocal, "both" => Mode::TwoWay, _ => Mode::LocalToRemote }; su.plan = None; } cx.notify() }); }))
            .child(gpui_component::switch::Switch::new("sync-del").checked(su.delete).label(t("sync.delete_extra")).on_click(cx.listener(|v, on: &bool, _, cx| { if let Some(su) = v.sync.as_mut() { su.delete = *on; su.plan = None; } cx.notify() })))
            .child(div().flex_1())
            .child(ui::button("sync-preview", if su.busy { t("sync.scanning") } else { t("sync.preview") }, 0).on_click(cx.listener(|v, _, _, cx| v.sync_preview(cx))));
        if su.plan.as_ref().is_some_and(|p| !p.is_empty()) {
            bar = bar.child(ui::button("sync-run", if su.confirm_delete { tf("sync.confirm_delete", &[("n", &dels)]) } else { t("sync.run").to_string() }, if su.confirm_delete || dels > 0 { 2 } else { 1 }).on_click(cx.listener(|v, _, _, cx| v.sync_run(cx))));
        }
        bar = bar.child(ui::button("sync-close", t("ui.cancel"), 3).on_click(cx.listener(|v, _, _, cx| { v.sync = None; cx.notify() })));
        Some(div().mt_2().p_3().rounded(px(R_MD)).border_1().border_color(c(th.accent).opacity(0.5)).flex().flex_col().gap_2().text_size(px(fs_small())).text_color(c(th.text))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tf("sync.title", &[("l", &self.panes[l].ldir.display()), ("r", &self.panes[r].rdir)])))
            .child(bar)
            .children(su.confirm_delete.then(|| div().flex().items_center().gap_2().text_color(c(th.error)).child(icon(IconName::TriangleAlert, ui::IC_SM, c(th.error))).child(tf("sync.delete_warn", &[("n", &dels)]))))
            .child(list))
    }

    fn toggle_sort(&mut self, pi: usize, key: SortKey, cx: &mut Context<Self>) {
        let slot = &mut self.panes[pi].sort;
        if slot.key == key { slot.desc = !slot.desc; } else { *slot = PaneSort { key, desc: matches!(key, SortKey::Mtime | SortKey::Size) }; }
        cx.notify();
    }
    fn sort_head(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let sort = self.panes[pi].sort;
        let mark = |key: SortKey| if sort.key != key { String::new() } else if sort.desc { " ↓".into() } else { " ↑".into() };
        let cell = |id: usize, label: String, key: SortKey| {
            div().id(("sftp-sort", pid(pi, id))).h(px(H_DEFAULT)).px_1().flex().items_center().rounded(px(R_SM)).cursor_pointer().overflow_hidden().whitespace_nowrap()
                .text_size(px(fs_small())).text_color(if sort.key == key { c(th.accent) } else { c(th.overlay1) })
                .hover(|d| d.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |v, _, _, cx| v.toggle_sort(pi, key, cx)))
                .child(format!("{label}{}", mark(key)))
        };
        let (wt, ws, wk) = col_widths();
        ui::table_header().px_3().gap_2()
            .child(div().size(px(16.)).flex_shrink_0())
            .child(cell(0, t("sftp.col_name").into(), SortKey::Name).flex_1().min_w_0())
            .child(cell(1, t("sftp.col_mtime").into(), SortKey::Mtime).w(px(wt)).flex_shrink_0())
            .child(cell(2, t("sftp.col_size").into(), SortKey::Size).w(px(ws)).flex_shrink_0())
            .child(cell(3, t("sftp.col_type").into(), SortKey::Kind).w(px(wk)).flex_shrink_0())
            .into_any_element()
    }

    /// Header: source (click to change host), filter toggle, actions menu.
    fn pane_header(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let p = &self.panes[pi];
        let (ic, name) = match &p.src { Src::Remote(_) => (IconName::Server, p.label()), _ => (IconName::Monitor, p.label()) };
        let tags = p.target().and_then(|t| config_hosts().iter().find(|(_, x)| x.pool_key() == t.pool_key()).cloned()).map(|(h, _)| h.tags).unwrap_or_default();
        let s = crate::config::get();
        let filter_on = p.filter.is_some();
        ui::toolbar()
            .child(div().id(("sftp-src", pi)).h(px(H_DEFAULT)).px_2().min_w_0().rounded(px(R_MD)).flex().items_center().gap_2().cursor_pointer()
                .hover(|d| d.bg(crate::theme::hover_bg()))
                .tooltip(|w, cx| gpui_component::tooltip::Tooltip::new(t("sftp.change_source")).build(w, cx))
                .on_click(cx.listener(move |v, _, w, cx| v.open_picker(pi, w, cx)))
                .child(icon(ic, ui::IC_MD, c(th.accent)))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(name))
                .children(tags.iter().take(3).map(|tg| div().size(px(7.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(tg)))))
                .child(icon(IconName::ChevronDown, ui::IC_XS, c(th.overlay1))))
            .child(div().flex_1())
            .child(ui::button(("sftp-filter", pi), t("sftp.filter"), 3).h(px(H_COMPACT)).px_2()
                .when(filter_on, |d| d.bg(crate::theme::selected_bg()).text_color(c(th.accent)))
                .child(icon(IconName::ListFilter, ui::IC_SM, if filter_on { c(th.accent) } else { c(th.subtext) }))
                .on_click(cx.listener(move |v, _, w, cx| v.toggle_filter(pi, w, cx))))
            .child(ui::button(("sftp-ops", pi), t("sftp.actions"), 3).h(px(H_COMPACT)).px_2()
                .child(icon(IconName::ChevronDown, ui::IC_XS, c(th.subtext)))
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| {
                    v.ops_menu = if v.ops_menu.as_ref().is_some_and(|m| m.0 == pi) { None } else { Some((pi, ev.position, std::time::Instant::now())) };
                    cx.stop_propagation();
                    cx.notify();
                })))
            .into_any_element()
    }

    fn nav_row(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let p = &self.panes[pi];
        let mut bar = div().h(px(ui::H_LARGE)).flex_shrink_0().px_2().flex().items_center().gap(px(2.)).bg(crate::theme::sub_bg()).overflow_hidden()
            .child(icon_btn(("sftp-back", pi), IconName::ArrowLeft, !p.back.is_empty()).on_click(cx.listener(move |v, _, _, cx| v.go_back(pi, cx))))
            .child(icon_btn(("sftp-fwd", pi), IconName::ArrowRight, !p.fwd.is_empty()).on_click(cx.listener(move |v, _, _, cx| v.go_fwd(pi, cx))))
            .child(div().w(px(4.)));
        let crumb = |i: usize, label: String, last: bool| {
            div().id(("sftp-crumb", pid(pi, i))).h(px(H_COMPACT)).px_1().flex_shrink_0().rounded(px(R_SM)).flex().items_center().cursor_pointer()
                .text_size(px(fs_small())).text_color(if last { c(th.text) } else { c(th.subtext) })
                .when(last, |d| d.font_weight(FontWeight::MEDIUM))
                .hover(|d| d.bg(crate::theme::hover_bg()))
                .child(label)
        };
        let mut parts: Vec<(String, String)> = vec![];
        if p.is_local() {
            if cfg!(windows) { parts.push((t("sftp.this_pc").to_string(), String::new())); }
            if !p.ldir.as_os_str().is_empty() { parts.extend(local_parts(&p.ldir).into_iter().map(|(l, d)| (l, d.to_string_lossy().into_owned()))); }
        } else {
            parts = remote_parts(&p.rdir);
        }
        let n = parts.len();
        for (i, (label, dir)) in parts.into_iter().enumerate() {
            if i > 0 { bar = bar.child(icon(IconName::ChevronRight, ui::IC_XS, c(th.overlay0))); }
            bar = bar.child(crumb(i, label, i + 1 == n).on_click(cx.listener(move |v, _, _, cx| v.goto(pi, dir.clone(), cx))));
        }
        bar.into_any_element()
    }

    fn file_list(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let p = &self.panes[pi];
        let mut list = div().id(("sftp-list", pi)).flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        if let Some(r) = p.remote() {
            if r.connecting {
                return list.child(div().p_4().flex().items_center().gap_2().text_size(px(fs_small())).text_color(c(th.subtext))
                    .child(icon(IconName::LoaderCircle, ui::IC_SM, c(th.accent))).child(tf("sftp.connecting", &[("t", &r.name)])))
                    .child(remote_skeleton()).into_any_element();
            }
            if let Some(e) = &r.error {
                return list.child(ui::empty_state(IconName::CircleAlert, t("sftp.failed_title"), Some(e.clone().into()), vec![
                        ui::button(("sftp-retry", pi), t("sftp.retry"), 1).on_click(cx.listener(move |v, _, _, cx| v.connect(pi, cx))).into_any_element(),
                        ui::button(("sftp-goback", pi), t("sftp.pick_other"), 0).on_click(cx.listener(move |v, _, w, cx| v.open_picker(pi, w, cx))).into_any_element(),
                    ], true)).into_any_element();
            }
        }
        let at_drives = p.is_local() && p.ldir.as_os_str().is_empty();
        let dark = th.appearance == Appearance::Dark;
        if !at_drives {
            list = list.child(div().id(("sftp-up", pi)).h(px(26.)).flex_shrink_0().px_3().flex().items_center().gap_2().text_size(px(fs_body())).text_color(c(th.subtext)).cursor_pointer()
                .hover(|d| d.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _, cx| if ev.click_count() >= 2 { v.up(pi, cx) }))
                .child(crate::file_icon::entry_icon("..", true, 0, ui::IC_LG, dark)).child(".."));
        }
        if p.loading { return list.child(remote_skeleton()).into_any_element(); }
        let q = p.filter.as_ref().map(|f| f.read(cx).value().trim().to_lowercase()).unwrap_or_default();
        let (wt, ws, wk) = col_widths();
        let selected_on = self.active == pi;
        let mut shown = 0;
        for i in sorted_indices(&p.entries, p.sort) {
            let e = &p.entries[i];
            if !q.is_empty() && !e.name.to_lowercase().contains(&q) { continue; }
            shown += 1;
            if shown > 2000 { break; }
            let selected = selected_on && (p.multi.contains(&i) || p.sel == Some(i));
            let drag = DragFile { pane: pi, path: p.path_of(&e.name), name: e.name.clone() };
            let hidden = e.name.starts_with('.');
            list = list.child(div().id(("sftp-row", pid(pi, i))).h(px(26.)).flex_shrink_0().px_3().flex().items_center().gap_2().text_size(px(fs_body())).text_color(c(th.text)).cursor_pointer()
                .when(hidden, |d| d.opacity(0.72))
                .when(selected, |d| d.bg(c(th.accent).opacity(0.2))).when(!selected, |d| d.hover(|d| d.bg(crate::theme::hover_bg())))
                .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _, cx| { if ev.click_count() >= 2 { v.open(pi, i, cx) } else { v.select(pi, i, ev.modifiers()); cx.notify() } }))
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| { v.open_ctx_menu(pi, i, ev.position, cx); cx.stop_propagation(); }))
                .on_drag(drag, |d: &DragFile, _, _, cx| cx.new(|_| DragPreview(d.name.clone())))
                .child(crate::file_icon::entry_icon(&e.name, e.is_dir, e.mode, ui::IC_LG, dark))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.name.clone()))
                .child(div().w(px(wt)).flex_shrink_0().px_1().overflow_hidden().whitespace_nowrap().text_size(px(fs_small())).text_color(c(th.overlay0)).child(fmt_mtime(e.mtime)))
                .child(div().w(px(ws)).flex_shrink_0().px_1().overflow_hidden().whitespace_nowrap().text_size(px(fs_small())).text_color(c(th.overlay0)).child(if e.is_dir { "—".to_string() } else { human(e.size) }))
                .child(div().w(px(wk)).flex_shrink_0().px_1().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(fs_small())).text_color(c(th.overlay0)).child(kind_label(e))));
        }
        if shown == 0 && !q.is_empty() {
            list = list.child(div().p_4().text_size(px(fs_small())).text_color(c(th.overlay1)).child(t("sftp.filter_none")));
        }
        list.into_any_element()
    }

    /// "Connect to a host" placeholder of a pane without a source.
    fn empty_state(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        ui::empty_state(IconName::Folder, t("sftp.connect_title"), Some(t("sftp.connect_desc").into()), vec![
            ui::button(("sftp-choose", pi), t("sftp.choose_host"), 1).on_click(cx.listener(move |v, _, w, cx| v.open_picker(pi, w, cx))).into_any_element(),
        ], true).into_any_element()
    }

    /// In-pane host picker: back / title / Local, search, groups, hosts.
    fn picker_view(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let Some(pk) = self.panes[pi].picker.as_ref() else { return div().into_any_element() };
        let s = crate::config::get();
        let query = pk.search.read(cx).value().trim().to_string();
        let group = pk.group.clone();
        let title = if group.is_empty() { t("sftp.choose_host").to_string() } else { hosttree::leaf_of(&group).to_string() };
        let can_back = !group.is_empty() || !matches!(self.panes[pi].src, Src::Empty);
        let head = ui::toolbar()
            .when(can_back, |d| d.child(icon_btn(("sftp-pk-back", pi), IconName::ArrowLeft, true).on_click(cx.listener(move |v, _, _, cx| v.picker_back(pi, cx)))))
            .when(!can_back, |d| d.pl_1())
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title))
            .child(ui::button(("sftp-pk-local", pi), t("sftp.local_btn"), 0).h(px(H_COMPACT)).px_2()
                .child(icon(IconName::Monitor, ui::IC_SM, c(th.subtext)))
                .on_click(cx.listener(move |v, _, _, cx| v.choose_local(pi, cx))));
        let search = ui::field_row().py_2().child(ui::search_field(&pk.search, false));
        let section = |label: String| div().px_3().pt_2().pb_1().text_size(px(fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.overlay1)).child(label);
        let mut list = div().id(("sftp-pk-list", pi)).flex_1().min_h_0().overflow_y_scroll().flex().flex_col().pb_2();
        let resolved = config_hosts();
        let q = hosttree::parse_query(&query, &s.tags.all());
        let hosts: Vec<usize> = (0..s.hosts.len()).filter(|&i| {
            let h = &s.hosts[i];
            if query.is_empty() { h.group.as_deref().unwrap_or("") == group } else { hosttree::host_matches(h, &s.groups, &q) }
        }).collect();
        let groups: Vec<&hosttree::GroupEntry> = if query.is_empty() { s.groups.iter().filter(|g| hosttree::parent_of(&g.path) == group && g.path != group).collect() } else { vec![] };
        if !groups.is_empty() {
            list = list.child(section(t("sftp.groups").to_string()));
            for (gi, g) in groups.into_iter().enumerate() {
                let path = g.path.clone();
                let n = hosttree::hosts_under(&s.hosts, &g.path).len();
                list = list.child(div().id(("sftp-pk-g", pid(pi, gi))).mx_2().h(px(36.)).flex_shrink_0().px_2().rounded(px(R_MD)).flex().items_center().gap_2().cursor_pointer()
                    .hover(|d| d.bg(crate::theme::hover_bg()))
                    .on_click(cx.listener(move |v, _, _, cx| { if let Some(p) = v.panes[pi].picker.as_mut() { p.group = path.clone(); } cx.notify() }))
                    .child(icon(IconName::Folder, ui::IC_MD, c(th.accent)))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(fs_body())).text_color(c(th.text)).child(hosttree::leaf_of(&g.path).to_string()))
                    .children(g.tags.iter().take(3).map(|tg| div().size(px(7.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(tg)))))
                    .child(div().text_size(px(fs_small())).text_color(c(th.overlay0)).child(n.to_string()))
                    .child(icon(IconName::ChevronRight, ui::IC_SM, c(th.overlay1))));
            }
        }
        if !hosts.is_empty() {
            list = list.child(section(t("sftp.hosts").to_string()));
            for i in hosts {
                let h = s.hosts[i].clone();
                let Some(target) = resolved.iter().find(|(x, _)| x.name == h.name).map(|(_, t)| t.clone()) else { continue };
                let tags = hosttree::effective_tags(&h, &s.groups);
                let sub = format!("ssh, {}", target.user);
                let (h2, t2) = (h.clone(), target.clone());
                list = list.child(div().id(("sftp-pk-h", pid(pi, i))).mx_2().h(px(44.)).flex_shrink_0().px_2().rounded(px(R_MD)).flex().items_center().gap_2().cursor_pointer()
                    .hover(|d| d.bg(crate::theme::hover_bg()))
                    .on_click(cx.listener(move |v, _, _, cx| v.choose_host(pi, h2.clone(), t2.clone(), cx)))
                    .child(div().size(px(28.)).flex_shrink_0().rounded(px(R_MD)).bg(c(th.surface0)).flex().items_center().justify_center().child(icon(IconName::Server, ui::IC_MD, c(th.accent))))
                    .child(div().flex_1().min_w_0().flex().flex_col()
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(fs_body())).text_color(c(th.text)).child(h.name.clone()))
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(fs_small())).text_color(c(th.overlay1)).child(sub)))
                    .children(tags.iter().take(3).map(|tg| div().size(px(8.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(tg))))));
            }
        }
        if list_is_empty(&s.hosts, &s.groups, &group, &query, &q) {
            list = list.child(div().p_4().text_size(px(fs_small())).text_color(c(th.overlay1)).child(if query.is_empty() { t("sftp.no_hosts") } else { t("sftp.no_match") }));
        }
        div().flex_1().min_h_0().flex().flex_col().child(head).child(search).child(list).into_any_element()
    }

    fn pane(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let p = &self.panes[pi];
        let mut card = div().id(("sftp-pane-card", pi)).flex_1().min_w_0().h_full().flex().flex_col().border_1().border_color(crate::theme::divider()).rounded(px(ui::R_LG)).overflow_hidden();
        if p.picker.is_some() { return card.child(self.picker_view(pi, cx)).into_any_element(); }
        if matches!(p.src, Src::Empty) { return card.child(self.empty_state(pi, cx)).into_any_element(); }
        let ready = p.ready();
        card = card
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |v, _, _, _| { if v.active != pi { v.panes[v.active].clear_sel(); v.active = pi; } }))
            .drag_over::<DragFile>(move |st, d, _, _| if d.pane != pi && ready { st.border_color(c(cur().accent)) } else { st })
            .drag_over::<ExternalPaths>(move |st, _, _, _| if ready { st.border_color(c(cur().accent)) } else { st })
            .on_drop(cx.listener(move |v, d: &DragFile, _, cx| { if d.pane != pi { v.send(d.pane, pi, vec![(d.path.clone(), d.name.clone())], cx); } }))
            .on_drop(cx.listener(move |v, ps: &ExternalPaths, _, cx| {
                let items: Vec<(String, String)> = ps.paths().iter().map(|f| (f.to_string_lossy().into_owned(), f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())).collect();
                if v.panes[pi].is_local() { v.copy_local(items, pi, cx); } else { for (f, _) in items { v.upload_to(pi, PathBuf::from(f), cx); } }
                cx.notify();
            }))
            .child(self.pane_header(pi, cx));
        if ready {
            card = card.child(self.nav_row(pi, cx));
            if let Some(f) = &p.filter {
                card = card.child(ui::field_row().py(px(6.)).border_b_1().border_color(crate::theme::divider()).child(ui::text_field(f, false).cleanable(true).prefix(icon(IconName::ListFilter, ui::IC_SM, c(th.overlay1)))));
            }
            card = card.child(self.sort_head(pi, cx));
        }
        card.child(self.file_list(pi, cx)).into_any_element()
    }

    /// "Actions" dropdown for one pane.
    fn ops_menu_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pi, pos, _) = self.ops_menu.clone()?;
        let p = &self.panes[pi];
        let ready = p.ready();
        let remote = p.remote().is_some();
        let other_ready = self.panes[1 - pi].ready();
        let has_sel = self.active == pi && !p.selection().is_empty();
        let one_sel = self.active == pi && p.sel.is_some();
        let to_label = if p.is_local() && !self.panes[1 - pi].is_local() { t("sftp.upload").to_string() } else if !p.is_local() && self.panes[1 - pi].is_local() { t("sftp.download").to_string() } else { t("sftp.copy_other").to_string() };
        let mut k = 0usize;
        let mut id = || { k += 1; ("sftp-opm", pid(pi, k)) };
        let mut list = ui::popover().id("sftp-ops-menu").occlude().min_w(px(220.)).p(px(5.)).flex().flex_col()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                if v.ops_menu.as_ref().is_some_and(|m| m.2.elapsed() < std::time::Duration::from_millis(250)) { return; }
                v.ops_menu = None;
                cx.notify();
            }));
        macro_rules! item { ($ic:expr, $label:expr, $en:expr, $danger:expr, $f:expr) => {{
            let f = $f;
            let el = ui::menu_item(id(), Some($ic), $label, $en, $danger);
            list = list.child(if $en { el.on_click(cx.listener(move |v: &mut Self, _, w: &mut Window, cx: &mut Context<Self>| { v.ops_menu = None; f(v, w, cx); cx.notify(); })) } else { el });
        }} }
        item!(IconName::FolderPlus, t("sftp.mkdir"), ready, false, move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.begin_edit(Edit::Mkdir, pi, w, cx));
        item!(IconName::RefreshCw, t("sftp.refresh"), ready, false, move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.refresh(pi, cx));
        item!(if p.hidden { IconName::EyeOff } else { IconName::Eye }, if p.hidden { t("sftp.hide_hidden") } else { t("sftp.show_hidden") }, ready, false, move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| { v.panes[pi].hidden = !v.panes[pi].hidden; v.refresh(pi, cx) });
        list = list.child(ui::menu_sep());
        item!(if p.is_local() { IconName::Upload } else { IconName::Download }, to_label, ready && other_ready && has_sel, false, move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.transfer_from(pi, cx));
        item!(IconName::Pencil, t("sftp.rename"), ready && one_sel, false, move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.begin_edit(Edit::Rename, pi, w, cx));
        item!(IconName::Lock, t("sftp.chmod"), ready && one_sel, false, move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.begin_edit(Edit::Chmod, pi, w, cx));
        item!(IconName::FileText, t("sftp.edit"), ready && remote && one_sel, false, move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.edit_selected(pi, cx));
        if self.sync_pair().is_some() {
            item!(IconName::FolderSync, t("sync.button"), true, false, |v: &mut Self, _w: &mut Window, _cx: &mut Context<Self>| { v.sync = Some(SyncUi { mode: ssh_term_proto::dirsync::Mode::LocalToRemote, delete: false, plan: None, busy: false, confirm_delete: false }); });
        }
        item!(IconName::Trash, t("sftp.delete"), ready && one_sel, true, move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.ask_delete(pi, cx));
        list = list.child(ui::menu_sep());
        item!(IconName::ArrowLeftRight, t("sftp.change_source"), true, false, move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.open_picker(pi, w, cx));
        if remote {
            item!(IconName::Unplug, t("sftp.disconnect"), true, true, move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.disconnect(pi, cx));
        }
        Some(gpui::deferred(gpui::anchored().position(pos).snap_to_window_with_margin(px(8.)).child(crate::chrome::pop_in("sftp-ops-pop", list))).with_priority(3).into_any_element())
    }

    fn queue_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let qs = self.queues();
        let jobs: Vec<(usize, Queue, Arc<sftp::Job>)> = qs.iter().flat_map(|(pi, q)| q.jobs.lock().unwrap().clone().into_iter().map(move |j| (*pi, q.clone(), j))).collect();
        let mut col = div().id("sftp-queue").h(px(130.)).overflow_y_scroll().flex().flex_col().border_1().border_color(crate::theme::divider()).rounded(px(ui::R_LG));
        col = col.child(ui::table_header().px_3().gap_2()
            .child(tf("sftp.queue_title", &[("n", &jobs.len())])).child(div().flex_1())
            .child(ui::button_sm("sftp-clear", t("sftp.clear_done"), 3).on_click(cx.listener(|v, _, _, cx| { for (_, q) in v.queues() { q.clear_done(); } cx.notify() }))));
        if jobs.is_empty() { col = col.child(div().p_3().text_size(px(fs_small())).text_color(c(th.overlay1)).child(t("sftp.queue_empty"))); }
        let two = qs.len() > 1;
        for (pi, q, j) in jobs.iter().rev() {
            let st = j.state();
            let (label, color) = match st { JobState::Queued => (t("sftp.st_queued"), c(th.overlay1)), JobState::Running => (t("sftp.st_running"), c(th.info)), JobState::Done => (t("sftp.st_done"), c(th.success)), JobState::Failed => (t("sftp.st_failed"), c(th.error)), JobState::Paused => (t("sftp.st_paused"), c(th.warning)) };
            let pct = j.progress();
            let done = j.done.load(std::sync::atomic::Ordering::Relaxed);
            let total = j.total.load(std::sync::atomic::Ordering::Relaxed);
            let resumed = j.resumed_from.load(std::sync::atomic::Ordering::Relaxed);
            let id = j.id;
            let key = pid(*pi, id as usize);
            let (q1, q2) = (q.clone(), q.clone());
            let btn = match st {
                JobState::Running | JobState::Queued => Some(ui::button_sm(("job-p", key), t("sftp.pause"), 3).on_click(cx.listener(move |_, _, _, cx| { q1.pause(id); cx.notify() }))),
                JobState::Paused | JobState::Failed => Some(ui::button_sm(("job-r", key), t("sftp.resume"), 3).on_click(cx.listener(move |v, _, _, cx| { q2.resume(id); v.start_worker(cx); cx.notify() }))),
                JobState::Done => None,
            };
            let host = if two { format!("{} · ", self.panes[*pi].label()) } else { String::new() };
            col = col.child(div().px_3().py(px(4.)).flex().items_center().gap_2().border_t_1().border_color(crate::theme::divider()).text_size(px(fs_small())).text_color(c(th.text))
                .child(icon(if j.dir == Dir::Upload { IconName::Upload } else { IconName::Download }, ui::IC_SM, c(th.accent)))
                .child(div().w(px(200.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("{host}{}", j.name())))
                .child(div().flex_1().h(px(6.)).rounded_full().bg(c(th.surface0)).child(div().h_full().rounded_full().bg(color).w(gpui::relative(pct.clamp(0., 1.)))))
                .child(div().w(px(150.)).text_size(px(fs_small())).text_color(c(th.overlay1)).child(format!("{} / {}{}", human(done), human(total), if resumed > 0 { format!(" · {}", tf("sftp.resumed", &[("n", &human(resumed))])) } else { String::new() })))
                .child(div().w(px(60.)).child(ui::tag(label, color)))
                .children(btn)
                .when_some(j.error.lock().unwrap().clone(), |d, e| d.child(div().text_color(c(th.error)).text_size(px(fs_small())).child(e))));
        }
        col.into_any_element()
    }
}

/// Column widths (mtime, size, type); narrower in split mode.
fn col_widths() -> (f32, f32, f32) { (132., 76., 88.) }

fn list_is_empty(hosts: &[HostEntry], groups: &[hosttree::GroupEntry], group: &str, query: &str, q: &hosttree::Query) -> bool {
    if query.is_empty() {
        !hosts.iter().any(|h| h.group.as_deref().unwrap_or("") == group) && !groups.iter().any(|g| hosttree::parent_of(&g.path) == group && g.path != group)
    } else { !hosts.iter().any(|h| hosttree::host_matches(h, groups, q)) }
}

impl Render for SftpView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // a pane without a source shows the remote host list right away
        for pi in 0..2 {
            if matches!(self.panes[pi].src, Src::Empty) && self.panes[pi].picker.is_none() {
                let st = cx.new(|cx| InputState::new(window, cx).placeholder(t("sftp.search_hosts")));
                self._subs.push(cx.subscribe_in(&st, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
                self.panes[pi].picker = Some(Picker { search: st, group: String::new() });
            }
        }
        if let Some(pi) = self.remote_pane() { if self.panes[pi].sess().is_some() { if let Some(p) = self.pending_edit.take() { self.edit_in(pi, p, cx); } } }
        let th = cur();
        let edit_bar = self.edit.as_ref().map(|(k, pi, name, st)| {
            let what = match k { Edit::Mkdir => t("sftp.mkdir").to_string(), Edit::Rename => tf("sftp.rename_x", &[("x", name)]), Edit::Chmod => tf("sftp.chmod_x", &[("x", name)]) };
            let where_ = self.panes[*pi].label();
            div().mt_2().p_2().rounded(px(R_MD)).border_1().border_color(c(th.accent).opacity(0.5)).flex().items_center().gap_2().text_size(px(fs_small())).text_color(c(th.text))
                .child(format!("{what} ({where_})")).child(div().w(px(260.)).child(ui::text_field(st, false)))
                .child(ui::button("edit-ok", t("ui.save"), 1).on_click(cx.listener(|v, _, _, cx| v.commit_edit(cx))))
                .child(ui::button("edit-cancel", t("ui.cancel"), 3).on_click(cx.listener(|v, _, _, cx| { v.edit = None; cx.notify() })))
        });
        let confirm = self.confirm_delete.as_ref().map(|(pi, p)| {
            div().mt_2().p_2().rounded(px(R_MD)).bg(c(th.error).opacity(0.1)).border_1().border_color(c(th.error).opacity(0.5)).flex().items_center().gap_2().text_size(px(fs_small())).text_color(c(th.text))
                .child(icon(IconName::TriangleAlert, ui::IC_SM, c(th.error)))
                .child(div().flex_1().child(tf("sftp.confirm_delete", &[("p", p), ("side", &self.panes[*pi].label())])))
                .child(ui::button("del-ok", t("sftp.delete"), 2).on_click(cx.listener(|v, _, _, cx| v.delete_confirmed(cx))))
                .child(ui::button("del-cancel", t("ui.cancel"), 3).on_click(cx.listener(|v, _, _, cx| { v.confirm_delete = None; cx.notify() })))
        });
        if let Some((pi, path, _, st, force)) = self.editor.clone() {
            let ed = div().flex_1().min_h_0().pt(px(crate::card_layout::CARD_PAD)).flex().flex_col().gap_2()
                .child(div().flex().items_center().gap_2().text_size(px(fs_small())).text_color(c(th.text))
                    .child(icon(IconName::FileText, ui::IC_SM, c(th.accent)))
                    .child(div().flex_1().min_w_0().overflow_hidden().child(format!("{}:{path}", self.panes[pi].label())))
                    .child(ui::button("ed-save", if force { t("sftp.edit_overwrite") } else { t("ui.save") }, if force { 2 } else { 1 }).on_click(cx.listener(|v, _, _, cx| v.save_editor(cx))))
                    .child(ui::button("ed-close", t("sftp.edit_close"), 3).on_click(cx.listener(|v, _, _, cx| { v.editor = None; cx.notify() }))))
                .child(div().flex_1().min_h_0().border_1().border_color(crate::theme::divider()).rounded(px(ui::R_LG)).overflow_hidden().child(Editor::new(&st).h_full()));
            let status = div().py_2().text_size(px(fs_small())).text_color(if self.error { c(th.error) } else { c(th.overlay1) }).child(self.status.clone());
            return self.frame()
                .child(self.page_head(t("sftp.editor_title"), cx))
                .child(ed).child(status);
        }
        let sync = self.sync_panel(cx);
        let both = self.panes[0].ready() && self.panes[1].ready();
        let arrow = |id: &'static str, ic: IconName, from: usize| div().id(id).size(px(H_COMPACT)).rounded(px(R_MD)).flex().items_center().justify_center()
            .when(both, |d| d.cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg())))
            .child(icon(ic, ui::IC_MD, if both { c(th.accent) } else { c(th.overlay0).opacity(0.5) }))
            .on_click(cx.listener(move |v, _, _, cx| v.transfer_from(from, cx)));
        let mid = div().w(px(H_COMPACT)).flex().flex_col().items_center().justify_center().gap_3()
            .child(arrow("sftp-to-right", IconName::ArrowRight, 0))
            .child(arrow("sftp-to-left", IconName::ArrowLeft, 1));
        let (left, right) = (self.pane(0, cx), self.pane(1, cx));
        let body = div().flex_1().min_h_0().pt(px(crate::card_layout::CARD_PAD)).flex().gap(px(ui::SP_1)).child(left).child(mid).child(right);
        let status = div().py_2().text_size(px(fs_small())).text_color(if self.error { c(th.error) } else { c(th.overlay1) }).child(if self.status.is_empty() { t("sftp.dnd_hint").to_string() } else { format!("{} · {}", self.status, t("sftp.dnd_hint")) });
        let ctx = self.ctx_menu.as_ref().map(|(_, m)| crate::ctxmenu_view::render(m, cx, Self::ctx_pick, Self::ctx_close));
        let ops = self.ops_menu_el(cx);
        self.frame()
            .child(self.page_head(t("ui.sftp_files"), cx))
            .children(ctx).children(ops)
            .children(edit_bar).children(confirm).children(sync)
            .child(body)
            .child(div().pt_3().flex_shrink_0().child(self.queue_view(cx)))
            .child(status.flex_shrink_0())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, dir: bool, size: u64, mtime: u64) -> Entry {
        Entry { name: name.into(), is_dir: dir, size, mode: 0, mtime }
    }

    #[test]
    fn dirs_stay_above_files() {
        let v = vec![e("z.log", false, 10, 300), e("a", true, 0, 100), e("m.log", false, 99, 200), e("b", true, 0, 400), e("c.txt", false, 1, 1)];
        let names = |s: PaneSort| sorted_indices(&v, s).into_iter().map(|i| v[i].name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(PaneSort { key: SortKey::Name, desc: false }), ["a", "b", "c.txt", "m.log", "z.log"]);
        assert_eq!(names(PaneSort { key: SortKey::Mtime, desc: true }), ["b", "a", "z.log", "m.log", "c.txt"]);
        assert_eq!(names(PaneSort { key: SortKey::Kind, desc: false }), ["a", "b", "m.log", "z.log", "c.txt"]);
    }

    #[test]
    fn local_copy_tree() {
        let d = std::env::temp_dir().join(format!("sftp-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/f.txt"), "x").unwrap();
        assert_eq!(copy_tree(&d.join("a"), &d.join("c")).unwrap(), 1);
        assert!(copy_tree(&d.join("a"), &d.join("a/b/inner")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
