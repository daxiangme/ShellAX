//! Dual-pane SFTP browser. Each pane picks its own source (this computer or any saved host),
//! so files can move local ↔ remote, local ↔ local and remote ↔ remote (relayed through a
//! temporary directory on this computer). Remote panes reuse the pooled SSH connection.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, ExternalPaths, FontWeight, Pixels, Point,
    Window,
};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_component::Sizable;
use russh_sftp::client::SftpSession;
use ssh_term_proto::hosts::HostEntry;
use ssh_term_proto::hosttree;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::session_store::SftpSide;
use ssh_term_proto::sftp::{self, Dir, Entry, JobState, Queue, Side, WhenExists};
use ssh_term_proto::sftp_follow::Follow;
use ssh_term_proto::ssh::{self, SshTarget};

use crate::filelist::{self, Sort, SortKey};
use crate::theme::{c, cur, Appearance};
use crate::ui::{self, fs_body, fs_small, icon, IconName, H_COMPACT, H_DEFAULT, R_MD};

// 左右留白：只由工作区包裹层的 `card_layout::CARD_PAD` 提供（与数据库等模块一致）。
// 标题栏、双栏面板、传输队列、状态行都不再叠加自己的左右内边距，所以标题图标的左边缘与面板卡片左边缘对齐，右侧同理。
// 页面标题不是弹窗：行高用工具栏高度，标题与下方内容之间只留 `CARD_PAD`，避免 48px 弹窗头在文字上下留出大块空白。

#[derive(Clone)]
pub struct DragFile {
    pub pane: usize,
    pub path: String,
    pub name: String,
}
pub struct DragPreview(String);
impl Render for DragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        div()
            .px_2()
            .py_1()
            .rounded(px(ui::R_MD))
            .bg(c(th.surface1))
            .text_size(px(ui::fs_body()))
            .text_color(c(th.text))
            .child(self.0.clone())
    }
}

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum DirAction {
    Right,
    Down,
    Close,
}

#[derive(Clone, Copy, PartialEq)]
enum Edit {
    Mkdir,
    Rename,
    Chmod,
}

pub enum SftpEvent {
    /// Insert a command (no Enter) into a session on this host (None = a local pane).
    Insert {
        target: Option<SshTarget>,
        command: String,
    },
    /// Open a new tab on this host / locally and insert `cd -- dir`.
    CdTab {
        target: Option<SshTarget>,
        dir: String,
    },
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

enum Src {
    Empty,
    Local,
    Remote(Box<Remote>),
}

struct Picker {
    search: Entity<InputState>,
    group: String,
}

struct Pane {
    src: Src,
    picker: Option<Picker>,
    /// Local directory ("" = drive list on Windows) and remote directory.
    ldir: PathBuf,
    rdir: String,
    entries: Vec<Entry>,
    loading: bool,
    gen: u64,
    sort: Sort,
    hidden: bool,
    filter: Option<Entity<InputState>>,
    back: Vec<String>,
    fwd: Vec<String>,
    sel: Option<usize>,
    /// Selected file names. Survives a refresh that reorders the list.
    picked: Vec<String>,
    /// When set, each row shows a checkbox in front of the name.
    pick_mode: bool,
}
impl Pane {
    fn new(src: Src) -> Self {
        Self {
            src,
            picker: None,
            ldir: default_local_dir(),
            rdir: String::new(),
            entries: vec![],
            loading: false,
            gen: 0,
            sort: Sort::default(),
            hidden: false,
            filter: None,
            back: vec![],
            fwd: vec![],
            sel: None,
            picked: vec![],
            pick_mode: false,
        }
    }
    fn remote(&self) -> Option<&Remote> {
        match &self.src {
            Src::Remote(r) => Some(r),
            _ => None,
        }
    }
    fn remote_mut(&mut self) -> Option<&mut Remote> {
        match &mut self.src {
            Src::Remote(r) => Some(r),
            _ => None,
        }
    }
    fn sess(&self) -> Option<Arc<SftpSession>> {
        self.remote().and_then(|r| r.sess.clone())
    }
    fn is_local(&self) -> bool {
        matches!(self.src, Src::Local)
    }
    /// Ready to list / receive files.
    fn ready(&self) -> bool {
        match &self.src {
            Src::Local => true,
            Src::Remote(r) => r.sess.is_some(),
            Src::Empty => false,
        }
    }
    fn side(&self) -> Side {
        if self.is_local() {
            Side::Local
        } else {
            Side::Remote
        }
    }
    fn dir(&self) -> String {
        if self.is_local() {
            self.ldir.to_string_lossy().into_owned()
        } else {
            self.rdir.clone()
        }
    }
    fn path_of(&self, name: &str) -> String {
        if self.is_local() {
            self.ldir.join(name).to_string_lossy().into_owned()
        } else {
            sftp::join(&self.rdir, name)
        }
    }
    fn label(&self) -> String {
        match &self.src {
            Src::Local => t("sftp.local").to_string(),
            Src::Remote(r) => r.name.clone(),
            Src::Empty => String::new(),
        }
    }
    fn target(&self) -> Option<SshTarget> {
        self.remote().map(|r| r.target.clone())
    }
    fn clear_sel(&mut self) {
        self.sel = None;
        self.picked.clear();
    }
    fn retain_picked(&mut self) {
        let have: HashSet<&str> = self.entries.iter().map(|e| e.name.as_str()).collect();
        self.picked.retain(|n| have.contains(n.as_str()));
        if self
            .sel
            .and_then(|i| self.entries.get(i))
            .is_none()
        {
            self.sel = self
                .picked
                .first()
                .and_then(|n| self.entries.iter().position(|e| &e.name == n));
        }
    }
    fn selection(&self) -> Vec<Entry> {
        self.picked
            .iter()
            .filter_map(|n| self.entries.iter().find(|e| &e.name == n).cloned())
            .collect()
    }
    fn toggle_name(&mut self, name: &str, index: usize) {
        if let Some(x) = self.picked.iter().position(|n| n == name) {
            self.picked.remove(x);
        } else {
            self.picked.push(name.to_string());
        }
        self.sel = Some(index);
    }
    fn save(&self) -> SftpSide {
        match &self.src {
            Src::Empty => SftpSide {
                kind: "empty".into(),
                target: None,
                name: String::new(),
                dir: String::new(),
            },
            Src::Local => SftpSide {
                kind: "local".into(),
                target: None,
                name: String::new(),
                dir: self.dir(),
            },
            Src::Remote(r) => SftpSide {
                kind: "remote".into(),
                target: Some(r.target.clone()),
                name: r.name.clone(),
                dir: if r.sess.is_some() {
                    self.rdir.clone()
                } else {
                    r.want.clone().unwrap_or_default()
                },
            },
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
    status_seq: u64,
    error: bool,
    edit: Option<(Edit, usize, String, Entity<InputState>)>,
    confirm_delete: Option<(usize, Vec<String>)>,
    /// Built-in editor. `readonly` is the directory-style viewer.
    editor: Option<OpenFile>,
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
    /// Transfer waiting on same-name choices.
    batch: Option<Batch>,
    conflict_rest: bool,
    /// Files remembered by Copy / Cut. `from` is the source pane when it is one of the two panes.
    clip: Option<Clip>,
    /// Cut sources to delete once these queue jobs finish.
    cut_after: Option<CutAfter>,
    /// Job ids already refreshed after completion.
    seen_done: HashSet<u64>,
    blank_menu: Option<(usize, Point<Pixels>, std::time::Instant)>,
    /// A same-name prompt is on screen for the current batch item.
    asking: bool,
}

#[derive(Clone)]
struct XItem {
    src: String,
    name: String,
    is_dir: bool,
}

struct Batch {
    /// None means the files come from outside this window (a system file drop).
    from: Option<usize>,
    to: usize,
    items: Vec<XItem>,
    index: usize,
    rest: Option<WhenExists>,
    cut: bool,
    cut_srcs: Vec<String>,
    cut_ids: Vec<u64>,
    queued: usize,
}

#[derive(Clone)]
struct Clip {
    from: Option<usize>,
    cut: bool,
    items: Vec<XItem>,
}

#[derive(Clone)]
struct OpenFile {
    pane: usize,
    path: String,
    ver: sftp::Version,
    text: Entity<EditorState>,
    force: bool,
    readonly: bool,
}

struct CutAfter {
    from: usize,
    paths: Vec<String>,
    ids: Vec<u64>,
}

/// Sync panel state: mode, delete flag, computed plan, delete confirmation pending.
struct SyncUi {
    mode: ssh_term_proto::dirsync::Mode,
    delete: bool,
    plan: Option<Vec<ssh_term_proto::dirsync::Op>>,
    busy: bool,
    confirm_delete: bool,
}

fn default_local_dir() -> PathBuf {
    for key in ["USERPROFILE", "HOME"] {
        if let Some(p) = std::env::var_os(key) {
            let p = PathBuf::from(p);
            if p.is_dir() {
                return p;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Saved host name for a target (sidebar name), else `user@host:port`.
fn host_name(t: &SshTarget) -> String {
    config_hosts()
        .iter()
        .find(|(_, x)| x.pool_key() == t.pool_key())
        .map(|(h, _)| h.name.clone())
        .unwrap_or_else(|| t.label())
}
fn config_hosts() -> std::sync::Arc<Vec<(HostEntry, SshTarget)>> {
    crate::config::resolved_hosts()
}

impl SftpView {
    /// Left pane local; right pane connects to `target` (or shows the "connect to host" state).
    /// Does not connect yet: set `initial_dir`, then call [`start`].
    pub fn new_deferred(
        target: Option<SshTarget>,
        rt: tokio::runtime::Handle,
        prompts: ssh::Prompts,
        _w: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        let mut me = Self {
            rt,
            prompts,
            panes: [Pane::new(Src::Empty), Pane::new(Src::Empty)],
            active: 1,
            status: String::new(),
            status_seq: 0,
            error: false,
            edit: None,
            confirm_delete: None,
            editor: None,
            ticking: false,
            _subs: vec![],
            sync: None,
            ctx_menu: None,
            ops_menu: None,
            pending_edit: None,
            follow_src: None,
            follow: Follow::new(false),
            follow_task: false,
            next_token: 0,
            batch: None,
            conflict_rest: false,
            clip: None,
            cut_after: None,
            seen_done: HashSet::new(),
            blank_menu: None,
            asking: false,
        };
        if let Some(t) = target {
            me.panes[1].src = Src::Remote(Box::new(me.new_remote(host_name(&t), t)));
        }
        // 左栏默认是本机文件；主机选择器只在栏头下拉菜单里打开。右栏没有目标主机时仍显示主机列表
        me.panes[0].src = Src::Local;
        me.refresh_local(0);
        me
    }
    fn new_remote(&mut self, name: String, target: SshTarget) -> Remote {
        self.next_token += 1;
        Remote {
            name,
            target,
            sess: None,
            conn: None,
            connecting: false,
            error: None,
            home: String::new(),
            queue: Queue::default(),
            want: None,
            token: self.next_token,
        }
    }
    /// Connect every remote pane that is not connected yet.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        for pi in 0..2 {
            if self.panes[pi].remote().is_some_and(|r| r.sess.is_none()) {
                self.connect(pi, cx);
            }
        }
    }
    /// Directory the remote pane opens first (sftp:// link path).
    pub fn set_initial_dir(&mut self, d: Option<String>) {
        if let Some(r) = self.panes[1].remote_mut() {
            r.want = d;
        }
    }
    /// Host of the remote pane (right pane first).
    pub fn target(&self) -> Option<SshTarget> {
        self.panes[1].target().or_else(|| self.panes[0].target())
    }
    fn remote_pane(&self) -> Option<usize> {
        [1, 0]
            .into_iter()
            .find(|&i| self.panes[i].remote().is_some())
    }
    fn local_pane(&self) -> Option<usize> {
        [0, 1].into_iter().find(|&i| self.panes[i].is_local())
    }
    /// Tab / window title: "SFTP · host" once a host is chosen.
    pub fn title(&self) -> String {
        match self.remote_pane() {
            Some(i) => ssh_term_proto::sftp_follow::title(&self.panes[i].label()),
            None => "SFTP".into(),
        }
    }
    pub fn local_dir(&self) -> String {
        self.local_pane()
            .map(|i| self.panes[i].dir())
            .unwrap_or_default()
    }
    pub fn set_local_dir(&mut self, d: &str) {
        let Some(i) = self.local_pane() else { return };
        if !d.is_empty() && Path::new(d).is_dir() {
            self.panes[i].ldir = d.into();
            self.refresh_local(i);
        }
    }
    /// Restore both panes from a saved layout.
    pub fn restore(&mut self, sides: &[SftpSide], cx: &mut Context<Self>) {
        for (pi, s) in sides.iter().take(2).enumerate() {
            match s.kind.as_str() {
                "local" => {
                    self.panes[pi].src = Src::Local;
                    if !s.dir.is_empty() && Path::new(&s.dir).is_dir() {
                        self.panes[pi].ldir = s.dir.clone().into();
                    }
                    self.refresh_local(pi);
                }
                "remote" => {
                    if let Some(t) = s.target.clone() {
                        let name = if s.name.is_empty() {
                            host_name(&t)
                        } else {
                            s.name.clone()
                        };
                        let mut r = self.new_remote(name, t);
                        r.want = (!s.dir.is_empty()).then(|| s.dir.clone());
                        self.panes[pi].src = Src::Remote(Box::new(r));
                        self.panes[pi].entries.clear();
                    }
                }
                _ => {
                    self.panes[pi].src = Src::Empty;
                    self.panes[pi].entries.clear();
                }
            }
        }
        cx.notify();
    }
    /// Attach (or detach with None) the terminal whose cwd this view follows.
    pub fn set_follow_source(
        &mut self,
        src: Option<(gpui::WeakEntity<crate::termview::TermView>, bool)>,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        self.follow_src = src;
        self.follow.set(on && self.follow_src.is_some());
        if self.follow_src.is_some() && !self.follow_task {
            self.follow_task = true;
            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(700))
                    .await;
                let alive = this.update(cx, |v, cx| {
                    let Some((src, local)) = v.follow_src.clone() else {
                        v.follow_task = false;
                        return false;
                    };
                    if !v.follow.on {
                        return true;
                    }
                    let Some(term) = src.upgrade() else {
                        v.follow_src = None;
                        v.follow_task = false;
                        return false;
                    };
                    let cwd = term.read(cx).shell_cwd();
                    if local {
                        if let Some(pi) = v.local_pane() {
                            let home = std::env::var("HOME").unwrap_or_default();
                            let cur = v.panes[pi].dir();
                            if let Some(d) = v.follow.step(cwd.as_deref(), &home, &cur) {
                                v.goto(pi, d.clone(), cx);
                                v.set_status(tf("sftp.followed", &[("d", &d)]), false);
                            }
                        }
                    } else if let Some(pi) =
                        [1, 0].into_iter().find(|&i| v.panes[i].sess().is_some())
                    {
                        let home = v.panes[pi]
                            .remote()
                            .map(|r| r.home.clone())
                            .unwrap_or_default();
                        let cur = v.panes[pi].rdir.clone();
                        if let Some(d) = v.follow.step(cwd.as_deref(), &home, &cur) {
                            v.goto(pi, d.clone(), cx);
                            v.set_status(tf("sftp.followed", &[("d", &d)]), false);
                        }
                    }
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            })
            .detach();
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
    fn set_status(&mut self, s: String, err: bool) {
        self.status_seq = self.status_seq.wrapping_add(1);
        self.status = s;
        self.error = err;
    }

    /// 成功确认。两秒后收起；之后的状态会换掉序号。
    fn flash_status(&mut self, s: String, cx: &mut Context<Self>) {
        self.set_status(s.clone(), false);
        let seq = self.status_seq;
        ui::flash_after(
            cx,
            seq,
            |v| v.status_seq,
            move |v| {
                if v.status == s {
                    v.status.clear();
                    v.error = false;
                }
            },
        );
    }

    // ---- sources
    fn connect(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = self.prompts.clone();
        let Some(r) = self.panes[pi].remote_mut() else {
            return;
        };
        if r.connecting {
            return;
        }
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
                let dir = match want.as_deref().map(|d| {
                    d.strip_prefix("~/")
                        .map(str::to_string)
                        .unwrap_or_else(|| d.trim_start_matches('~').to_string())
                }) {
                    Some(d) if !d.is_empty() => match s.canonicalize(d.clone()).await {
                        Ok(p) => p,
                        Err(_) => s.canonicalize(".").await?,
                    },
                    _ => s.canonicalize(".").await?,
                };
                let ls = sftp::list_remote(&s, &dir, false).await?;
                Ok::<_, anyhow::Error>((Arc::new(s), dir, ls, conn, real_home))
            }
            .await;
            let _ = tx.send(r);
        });
        cx.spawn(async move |this, cx| {
            let r = rx.recv_async().await;
            let _ = this.update(cx, |v, cx| {
                let hidden = v.panes[pi].hidden;
                let out = {
                    let Some(rm) = v.panes[pi].remote_mut().filter(|r| r.token == token) else {
                        return;
                    };
                    rm.connecting = false;
                    match r {
                        Ok(Ok((s, dir, ls, conn, rh))) => {
                            rm.home = rh;
                            rm.sess = Some(s);
                            rm.conn = Some(conn);
                            Some(Ok((dir, ls)))
                        }
                        Ok(Err(e)) => {
                            let m = format!("{e:#}");
                            rm.error = Some(m.clone());
                            Some(Err(m))
                        }
                        Err(_) => None,
                    }
                };
                match out {
                    Some(Ok((dir, ls))) => {
                        let pane = &mut v.panes[pi];
                        pane.rdir = dir;
                        pane.entries = ls;
                        pane.back.clear();
                        pane.fwd.clear();
                        v.set_status(tf("sftp.connected", &[("t", &name)]), false);
                        if hidden {
                            v.refresh(pi, cx);
                        }
                        cx.emit(SftpEvent::Retitle);
                    }
                    Some(Err(m)) => v.set_status(tf("sftp.connect_failed", &[("e", &m)]), true),
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn choose_host(&mut self, pi: usize, h: HostEntry, t: SshTarget, cx: &mut Context<Self>) {
        let _ = crate::audit_log().append(
            "sftp.pick_host",
            "user",
            &format!("pane={pi} host={}", t.label()),
        );
        let r = self.new_remote(h.name.clone(), t);
        let p = &mut self.panes[pi];
        p.src = Src::Remote(Box::new(r));
        p.picker = None;
        p.entries.clear();
        p.back.clear();
        p.fwd.clear();
        p.clear_sel();
        p.rdir.clear();
        self.sync = None;
        self.connect(pi, cx);
        cx.emit(SftpEvent::Retitle);
        cx.notify();
    }
    /// Sidebar click in the SFTP module: connect a pane that is still choosing a host
    /// (active pane first). Returns false when every pane already has a source.
    pub fn connect_free_pane(&mut self, t: SshTarget, cx: &mut Context<Self>) -> bool {
        let free = |p: &Pane| matches!(p.src, Src::Empty) || p.picker.is_some();
        let Some(pi) = [self.active, 1 - self.active.min(1)]
            .into_iter()
            .find(|&i| free(&self.panes[i]))
        else {
            return false;
        };
        let name = host_name(&t);
        let h = crate::config::get()
            .all_hosts()
            .into_iter()
            .find(|h| h.name == name)
            .unwrap_or_else(|| HostEntry {
                name: name.clone(),
                host: t.host.clone(),
                ..Default::default()
            });
        self.choose_host(pi, h, t, cx);
        true
    }
    fn choose_local(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        p.src = Src::Local;
        p.picker = None;
        p.back.clear();
        p.fwd.clear();
        p.clear_sel();
        self.sync = None;
        self.refresh_local(pi);
        cx.emit(SftpEvent::Retitle);
        cx.notify();
    }
    fn disconnect(&mut self, pi: usize, cx: &mut Context<Self>) {
        if let Some(r) = self.panes[pi].remote() {
            let _ = crate::audit_log().append(
                "sftp.disconnect",
                "user",
                &format!("pane={pi} host={}", r.target.label()),
            );
        }
        let p = &mut self.panes[pi];
        p.src = Src::Empty;
        p.entries.clear();
        p.clear_sel();
        p.back.clear();
        p.fwd.clear();
        p.filter = None;
        self.sync = None;
        if self.editor.as_ref().is_some_and(|e| e.pane == pi) {
            self.editor = None;
        }
        cx.emit(SftpEvent::Retitle);
        cx.notify();
    }
    fn open_picker(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        let st = cx.new(|cx| InputState::new(window, cx).placeholder(t("sftp.search_hosts")));
        st.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(
            cx.subscribe_in(&st, window, |_, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify()
                }
            }),
        );
        self.panes[pi].picker = Some(Picker {
            search: st,
            group: String::new(),
        });
        self.ops_menu = None;
        cx.notify();
    }
    fn picker_back(&mut self, pi: usize, cx: &mut Context<Self>) {
        if let Some(p) = self.panes[pi].picker.as_mut() {
            // an empty pane always shows the host list, so there is nothing to go back to
            if p.group.is_empty() {
                if !matches!(self.panes[pi].src, Src::Empty) {
                    self.panes[pi].picker = None;
                }
            } else {
                p.group = hosttree::parent_of(&p.group).to_string();
            }
        }
        cx.notify();
    }
    fn toggle_filter(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes[pi].filter.take().is_none() {
            let st = cx.new(|cx| InputState::new(window, cx).placeholder(t("sftp.filter_ph")));
            st.update(cx, |s, cx| s.focus(window, cx));
            self._subs.push(
                cx.subscribe_in(&st, window, |_, _, ev: &InputEvent, _, cx| {
                    if matches!(ev, InputEvent::Change) {
                        cx.notify()
                    }
                }),
            );
            self.panes[pi].filter = Some(st);
        }
        cx.notify();
    }

    // ---- listing / navigation
    fn refresh_local(&mut self, pi: usize) {
        self.refresh_local_mode(pi, false);
    }
    fn refresh_local_mode(&mut self, pi: usize, quiet: bool) {
        let p = &mut self.panes[pi];
        if !p.is_local() {
            return;
        }
        if p.ldir.as_os_str().is_empty() {
            p.entries = windows_drives();
            p.retain_picked();
            return;
        }
        match sftp::list_local(&p.ldir, p.hidden) {
            Ok(v) => {
                p.entries = v;
                p.retain_picked();
            }
            Err(e) => {
                if !quiet {
                    p.entries.clear();
                }
                let m = format!("{}: {e}", p.ldir.display());
                self.set_status(m, true)
            }
        }
    }
    fn refresh(&mut self, pi: usize, cx: &mut Context<Self>) {
        self.refresh_in(pi, false, cx);
    }
    /// `quiet` keeps the current rows until the new listing arrives.
    fn refresh_in(&mut self, pi: usize, quiet: bool, cx: &mut Context<Self>) {
        if self.panes[pi].is_local() {
            self.refresh_local_mode(pi, quiet);
            return;
        }
        let p = &mut self.panes[pi];
        let Some(s) = p.sess() else { return };
        p.gen = p.gen.wrapping_add(1);
        let (gen, dir, hidden) = (p.gen, p.rdir.clone(), p.hidden);
        if !quiet {
            p.loading = true;
            p.entries.clear();
        }
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let _ = tx.send(sftp::list_remote(&s, &dir, hidden).await);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(r) = rx.recv_async().await {
                let _ = this.update(cx, |v, cx| {
                    let p = &mut v.panes[pi];
                    if p.gen != gen {
                        return;
                    }
                    p.loading = false;
                    match r {
                        Ok(ls) => {
                            p.entries = ls;
                            p.retain_picked();
                        }
                        Err(e) => v.set_status(format!("{e:#}"), true),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.refresh(0, cx);
        self.refresh(1, cx);
    }
    fn refresh_quiet(&mut self, cx: &mut Context<Self>) {
        self.refresh_in(0, true, cx);
        self.refresh_in(1, true, cx);
    }
    fn set_dir(&mut self, pi: usize, dir: String, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        p.clear_sel();
        if p.is_local() {
            p.ldir = PathBuf::from(dir);
        } else {
            p.rdir = dir;
        }
        self.refresh(pi, cx);
        cx.notify();
    }
    /// Navigate (recorded in back/forward history).
    fn goto(&mut self, pi: usize, dir: String, cx: &mut Context<Self>) {
        let p = &mut self.panes[pi];
        let cur = p.dir();
        if cur != dir {
            p.back.push(cur);
            p.fwd.clear();
        }
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
        let Some(e) = p.entries.get(i).filter(|e| e.is_dir).cloned() else {
            return;
        };
        let dir = if p.is_local() {
            if p.ldir.as_os_str().is_empty() {
                format!("{}:\\", e.name.trim_end_matches(':'))
            } else {
                p.ldir.join(&e.name).to_string_lossy().into_owned()
            }
        } else {
            sftp::join(&p.rdir, &e.name)
        };
        self.goto(pi, dir, cx);
    }
    fn up(&mut self, pi: usize, cx: &mut Context<Self>) {
        let p = &self.panes[pi];
        let dir = if p.is_local() {
            if p.ldir.as_os_str().is_empty() {
                return;
            }
            if is_drive_root(&p.ldir) {
                String::new()
            } else {
                match p.ldir.parent() {
                    Some(x) => x.to_string_lossy().into_owned(),
                    None => return,
                }
            }
        } else {
            sftp::parent(&p.rdir)
        };
        self.goto(pi, dir, cx);
    }
    fn select(&mut self, pi: usize, i: usize, m: gpui::Modifiers) {
        if self.active != pi {
            self.panes[self.active].clear_sel();
            self.active = pi;
        }
        let p = &mut self.panes[pi];
        let Some(name) = p.entries.get(i).map(|e| e.name.clone()) else {
            return;
        };
        if p.pick_mode && !m.shift {
            p.toggle_name(&name, i);
        } else if m.control || m.platform {
            p.toggle_name(&name, i);
        } else if m.shift && p.sel.is_some() {
            let a = p.sel.unwrap_or(i);
            let (lo, hi) = (a.min(i), a.max(i));
            p.picked = (lo..=hi)
                .filter_map(|k| p.entries.get(k).map(|e| e.name.clone()))
                .collect();
            p.sel = Some(i);
        } else {
            p.sel = Some(i);
            p.picked = vec![name];
        }
    }
    fn open_ctx_menu(&mut self, pi: usize, i: usize, pos: Point<Pixels>, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::{FileObj, Place, Target};
        let name = self.panes[pi]
            .entries
            .get(i)
            .map(|e| e.name.clone())
            .unwrap_or_default();
        let in_sel = self.active == pi && self.panes[pi].picked.iter().any(|n| n == &name);
        if !in_sel {
            self.panes[self.active].clear_sel();
            self.active = pi;
            self.panes[pi].sel = Some(i);
            self.panes[pi].picked = vec![name];
        }
        let p = &self.panes[pi];
        let files: Vec<FileObj> = p
            .selection()
            .into_iter()
            .map(|e| FileObj {
                path: p.path_of(&e.name),
                is_dir: e.is_dir,
                mode: e.mode,
            })
            .collect();
        if files.is_empty() {
            return;
        }
        let tgt = p.target();
        let (place, host) = match &tgt {
            Some(t) => (Place::RemotePanel, t.host.clone()),
            None => (Place::LocalPanel, "local".to_string()),
        };
        let key = crate::ctxmenu_view::host_key(tgt.as_ref());
        self.ctx_menu = Some((
            pi,
            crate::ctxmenu_view::build(Target::Files(files), place, &host, key, pos, None),
        ));
        if let Some(rx) = crate::ctxmenu_view::ensure_probe(tgt) {
            cx.spawn(async move |this, cx| {
                let _ = rx.recv_async().await;
                let _ = this.update(cx, |v, cx| {
                    if let Some((pi, m)) = &v.ctx_menu {
                        let pi = *pi;
                        let (place, host) = match v.panes[pi].target() {
                            Some(t) => (Place::RemotePanel, t.host),
                            None => (Place::LocalPanel, "local".into()),
                        };
                        let nm = m.rebuilt(place, &host);
                        v.ctx_menu = Some((pi, nm));
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }
    fn ctx_pick(&mut self, i: usize, w: &mut Window, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::Act;
        let Some((pi, m)) = self.ctx_menu.take() else {
            return;
        };
        let Some(item) = m.menu.items.get(i).cloned() else {
            return;
        };
        let target = self.panes[pi].target();
        let _ = crate::audit_log().append(
            "ctxmenu.pick",
            "user",
            &format!(
                "host={} panel={} item={} custom={}",
                target.as_ref().map(|t| t.host.as_str()).unwrap_or("local"),
                if target.is_some() { "remote" } else { "local" },
                item.id,
                item.custom
            ),
        );
        match item.act {
            Act::Insert(command) => cx.emit(SftpEvent::Insert { target, command }),
            Act::CdNewTab(dir) => cx.emit(SftpEvent::CdTab { target, dir }),
            Act::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
            Act::OpenEditor(p) => self.open_text(pi, p, false, cx),
            Act::SftpView(p) => self.open_text(pi, p, true, cx),
            Act::Download(paths) | Act::Upload(paths) => {
                let names = paths
                    .into_iter()
                    .map(|p| {
                        let n = p
                            .trim_end_matches(['/', '\\'])
                            .rsplit(['/', '\\'])
                            .next()
                            .unwrap_or("file")
                            .to_string();
                        (p, n)
                    })
                    .collect();
                self.send(pi, 1 - pi, names, cx)
            }
            Act::OpenUrl(u) => cx.open_url(&u),
            Act::SftpDelete(paths) => self.ask_delete_paths(pi, paths, cx),
            Act::SftpRename(_) => self.begin_edit(Edit::Rename, pi, w, cx),
            Act::SftpChmod(_) => self.begin_edit(Edit::Chmod, pi, w, cx),
            Act::SftpCopy(paths) => self.remember(pi, paths, false, cx),
            Act::SftpCut(paths) => self.remember(pi, paths, true, cx),
            Act::Search(_)
            | Act::AiExplain(_)
            | Act::PasteInput(_)
            | Act::UseAiContext
            | Act::OpenTool(..) => {}
        }
        cx.notify();
    }
    fn ctx_close(&mut self, cx: &mut Context<Self>) {
        if self.ctx_menu.take().is_some() {
            cx.notify();
        }
    }

    // ---- transfers
    /// Run on the tokio runtime, then apply on the UI thread; errors go to the status line.
    fn remote_op<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        fut: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
        apply: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(r) = rx.recv_async().await {
                let _ = this.update(cx, |v, cx| {
                    match r {
                        Ok(x) => apply(v, x, cx),
                        Err(e) if v.batch.is_some() => v.fail_batch(e, cx),
                        Err(e) => v.set_status(format!("{e:#}"), true),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }
    fn after_change(&mut self, msg: String, cx: &mut Context<Self>) {
        self.flash_status(msg, cx);
        self.refresh_all(cx);
    }
    /// Copy `items` (path, name) from pane `from` into the current directory of pane `to`.
    fn send(
        &mut self,
        from: usize,
        to: usize,
        items: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return self.set_status(t("sftp.select_first").into(), true);
        }
        if from == to {
            return;
        }
        if !self.panes[to].ready() || !self.panes[from].ready() {
            return self.set_status(t("sftp.not_connected").into(), true);
        }
        if self.panes[to].is_local() && self.panes[to].ldir.as_os_str().is_empty() {
            return self.set_status(t("sftp.no_target_dir").into(), true);
        }
        let xitems = items
            .into_iter()
            .map(|(src, name)| {
                let is_dir = self.panes[from]
                    .entries
                    .iter()
                    .any(|e| e.name == name && e.is_dir)
                    || Path::new(&src).is_dir();
                XItem { src, name, is_dir }
            })
            .collect();
        self.begin_batch(Some(from), to, xitems, false, cx);
    }
    fn begin_batch(
        &mut self,
        from: Option<usize>,
        to: usize,
        items: Vec<XItem>,
        cut: bool,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        if self.batch.is_some() {
            self.set_status(t("sftp.busy_conflict").into(), true);
            cx.notify();
            return;
        }
        if self.panes[to].is_local() && self.panes[to].ldir.as_os_str().is_empty() {
            self.set_status(t("sftp.no_target_dir").into(), true);
            cx.notify();
            return;
        }
        self.conflict_rest = false;
        self.batch = Some(Batch {
            from,
            to,
            items,
            index: 0,
            rest: None,
            cut,
            cut_srcs: vec![],
            cut_ids: vec![],
            queued: 0,
        });
        self.pump(cx);
    }
    fn pump(&mut self, cx: &mut Context<Self>) {
        let Some(b) = &self.batch else { return };
        if b.index >= b.items.len() {
            self.finish_batch(cx);
            return;
        }
        let name = b.items[b.index].name.clone();
        let to = b.to;
        let dest = self.panes[to].path_of(&name);
        let side = self.panes[to].side();
        let sess = self.panes[to].sess();
        self.remote_op(
            cx,
            async move { Ok(sftp::exists_path(sess.as_deref(), side, &dest).await) },
            move |v, exists, cx| {
                if !exists {
                    v.use_name(name, cx);
                    return;
                }
                if let Some(choice) = v.batch.as_ref().and_then(|b| b.rest) {
                    v.apply_choice(choice, cx);
                } else {
                    v.asking = true;
                    v.set_status(tf("sftp.conflict", &[("name", &name)]), false);
                    cx.notify();
                }
            },
        );
    }
    fn use_name(&mut self, dest_name: String, cx: &mut Context<Self>) {
        let Some(item) = self.batch.as_ref().and_then(|b| b.items.get(b.index).cloned()) else {
            return;
        };
        self.asking = false;
        if let Some(b) = self.batch.as_mut() {
            b.index += 1;
        }
        self.enqueue(&item, &dest_name, cx);
    }
    fn apply_choice(&mut self, choice: WhenExists, cx: &mut Context<Self>) {
        self.asking = false;
        if self.conflict_rest {
            if let Some(b) = self.batch.as_mut() {
                b.rest = Some(choice);
            }
        }
        let Some(b) = &self.batch else { return };
        let name = b.items[b.index].name.clone();
        let to = b.to;
        let dir = self.panes[to].dir();
        let side = self.panes[to].side();
        let sess = self.panes[to].sess();
        self.remote_op(
            cx,
            async move { Ok(sftp::resolve_name(sess.as_deref(), side, &dir, &name, choice).await) },
            |v, dest, cx| {
                v.asking = false;
                match dest {
                    Some(dest) => v.use_name(dest, cx),
                    None => {
                        if let Some(b) = v.batch.as_mut() {
                            b.index += 1;
                        }
                        v.pump(cx);
                    }
                }
            },
        );
    }
    fn fail_batch(&mut self, err: anyhow::Error, cx: &mut Context<Self>) {
        self.batch = None;
        self.asking = false;
        self.conflict_rest = false;
        self.set_status(format!("{err:#}"), true);
        cx.notify();
    }
    fn finish_batch(&mut self, cx: &mut Context<Self>) {
        let Some(b) = self.batch.take() else { return };
        self.conflict_rest = false;
        self.asking = false;
        if b.cut {
            if let Some(from) = b.from {
                if !b.cut_ids.is_empty() {
                    self.cut_after = Some(CutAfter {
                        from,
                        paths: b.cut_srcs,
                        ids: b.cut_ids,
                    });
                } else if b.queued > 0 && b.cut_srcs.is_empty() {
                    // Local copies already removed their sources.
                }
            }
        }
        if b.queued == 0 {
            self.set_status(t("sftp.all_skipped").into(), false);
        }
        cx.notify();
    }
    fn enqueue(&mut self, item: &XItem, dest_name: &str, cx: &mut Context<Self>) {
        let Some(b) = &self.batch else { return };
        let (from, to, cut) = (b.from, b.to, b.cut);
        let from_local = from
            .map(|i| self.panes[i].is_local())
            .unwrap_or(true);
        let to_local = self.panes[to].is_local();
        let src = item.src.clone();
        let dest_path = self.panes[to].path_of(dest_name);
        if src == dest_path {
            self.set_status(tf("sftp.same_place", &[("p", &src)]), true);
            self.pump(cx);
            return;
        }
        match (from_local, to_local) {
            (true, false) => self.queue_upload(to, PathBuf::from(&src), dest_name, src, cx),
            (false, true) => self.queue_download(from.unwrap_or(0), src, dest_name, to, cx),
            (true, true) => self.queue_local(src, dest_name, to, cut, item.is_dir, cx),
            (false, false) => self.queue_relay(from.unwrap_or(0), src, dest_name, to, cx),
        }
    }
    fn note_queued(&mut self, ids: Vec<u64>, src: String, cx: &mut Context<Self>) {
        if let Some(b) = self.batch.as_mut() {
            b.queued += 1;
            if b.cut {
                b.cut_ids.extend(ids);
                b.cut_srcs.push(src);
            }
        }
        self.start_worker(cx);
        self.pump(cx);
    }
    fn queue_upload(
        &mut self,
        to: usize,
        local: PathBuf,
        dest_name: &str,
        src: String,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self.panes[to].sess() else {
            self.batch = None;
            self.asking = false;
            return self.set_status(t("sftp.not_connected").into(), true);
        };
        let remote = sftp::join(&self.panes[to].rdir, dest_name);
        let Some(q) = self.panes[to].remote().map(|r| r.queue.clone()) else {
            self.batch = None;
            self.asking = false;
            return;
        };
        self.remote_op(
            cx,
            async move {
                let before = q.max_id();
                let queued = async {
                    let n = q.push_upload_tree(&s, &local, &remote).await?;
                    Ok::<_, anyhow::Error>((n, q.ids_after(before)))
                }
                .await;
                Ok(queued)
            },
            move |v, queued, cx| match queued {
                Ok((_n, ids)) => {
                    v.set_status(tf("sftp.queued", &[("n", &ids.len().max(1))]), false);
                    v.note_queued(ids, src, cx);
                }
                Err(e) => v.fail_batch(e, cx),
            },
        );
    }
    fn queue_download(
        &mut self,
        from: usize,
        remote: String,
        dest_name: &str,
        to: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = self.panes[from].sess() else {
            self.batch = None;
            self.asking = false;
            return self.set_status(t("sftp.not_connected").into(), true);
        };
        let local = self.panes[to].ldir.join(dest_name);
        let Some(q) = self.panes[from].remote().map(|r| r.queue.clone()) else {
            self.batch = None;
            self.asking = false;
            return;
        };
        let src = remote.clone();
        self.remote_op(
            cx,
            async move {
                let before = q.max_id();
                let queued = async {
                    let n = q.push_download_tree(&s, &remote, &local).await?;
                    Ok::<_, anyhow::Error>((n, q.ids_after(before)))
                }
                .await;
                Ok(queued)
            },
            move |v, queued, cx| match queued {
                Ok((_n, ids)) => {
                    v.set_status(tf("sftp.queued", &[("n", &ids.len().max(1))]), false);
                    v.note_queued(ids, src, cx);
                }
                Err(e) => v.fail_batch(e, cx),
            },
        );
    }
    fn queue_local(
        &mut self,
        src: String,
        dest_name: &str,
        to: usize,
        cut: bool,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        let dst = self.panes[to].ldir.join(dest_name);
        let src_path = src.clone();
        self.remote_op(
            cx,
            async move {
                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    if PathBuf::from(&src_path) == dst {
                        anyhow::bail!("{}", tf("sftp.same_place", &[("p", &src_path)]));
                    }
                    sftp::copy_tree(&PathBuf::from(&src_path), &dst)
                        .map_err(|e| anyhow::anyhow!("{src_path}: {e}"))?;
                    if cut {
                        let p = PathBuf::from(&src_path);
                        if is_dir {
                            std::fs::remove_dir_all(&p)?;
                        } else {
                            std::fs::remove_file(&p)?;
                        }
                    }
                    Ok(())
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e:#}"))?
            },
            move |v, _, cx| {
                if let Some(b) = v.batch.as_mut() {
                    b.queued += 1;
                }
                v.after_change(t("sftp.copied_one").into(), cx);
                v.pump(cx);
            },
        );
    }
    fn queue_relay(
        &mut self,
        from: usize,
        path: String,
        dest_name: &str,
        to: usize,
        cx: &mut Context<Self>,
    ) {
        let (Some(sa), Some(sb)) = (self.panes[from].sess(), self.panes[to].sess()) else {
            self.batch = None;
            self.asking = false;
            return self.set_status(t("sftp.not_connected").into(), true);
        };
        let Some(qb) = self.panes[to].remote().map(|r| r.queue.clone()) else {
            self.batch = None;
            self.asking = false;
            return;
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_root = std::env::temp_dir().join(format!("shellax-relay-{stamp}"));
        let tmp = tmp_root.join(dest_name);
        let dest = sftp::join(&self.panes[to].rdir, dest_name);
        let src = path.clone();
        self.remote_op(
            cx,
            async move {
                std::fs::create_dir_all(&tmp_root)?;
                let tq = Queue::default();
                tq.push_download_tree(&sa, &path, &tmp).await?;
                tq.drain(&sa).await;
                let failed: Vec<String> = tq
                    .jobs
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|j| j.state() != JobState::Done)
                    .map(|j| j.error.lock().unwrap().clone().unwrap_or_else(|| j.name()))
                    .collect();
                if !failed.is_empty() {
                    let _ = std::fs::remove_dir_all(&tmp_root);
                    anyhow::bail!("{}", failed.join("; "));
                }
                let before = qb.max_id();
                let _n = qb.push_upload_tree(&sb, &tmp, &dest).await?;
                Ok(qb.ids_after(before))
            },
            move |v, ids, cx| {
                v.set_status(tf("sftp.relay_queued", &[("n", &ids.len().max(1))]), false);
                v.note_queued(ids, src, cx);
            },
        );
    }
    fn poll_cut(&mut self, cx: &mut Context<Self>) {
        let Some(cut) = &self.cut_after else { return };
        let mut failed = false;
        let mut pending = false;
        for id in &cut.ids {
            let state = self.queues().iter().find_map(|(_, q)| q.state_of(*id));
            match state {
                Some(JobState::Failed) => failed = true,
                Some(JobState::Done) => {}
                Some(_) => pending = true,
                None => {}
            }
        }
        if pending {
            return;
        }
        let cut = self.cut_after.take().unwrap();
        if failed {
            self.set_status(t("sftp.cut_kept").into(), true);
            return;
        }
        let (s, side) = (self.panes[cut.from].sess(), self.panes[cut.from].side());
        let paths = cut.paths;
        self.remote_op(
            cx,
            async move {
                for p in &paths {
                    sftp::remove(s.as_deref(), side, p).await?;
                }
                Ok(paths.len())
            },
            |v, n, cx| v.after_change(tf("sftp.cut_done", &[("n", &n)]), cx),
        );
    }
    /// Remote panes with their transfer queues.
    pub(crate) fn queues(&self) -> Vec<(usize, Queue)> {
        (0..2)
            .filter_map(|i| self.panes[i].remote().map(|r| (i, r.queue.clone())))
            .collect()
    }
    fn start_worker(&mut self, cx: &mut Context<Self>) {
        for p in &self.panes {
            if let Some(r) = p.remote() {
                if let Some(s) = r.sess.clone() {
                    r.queue.start(&self.rt, s, || {});
                }
            }
        }
        if self.ticking {
            return;
        }
        self.ticking = true;
        // Jobs are already queued. Refresh the panes when one finishes, without clearing the list.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let Ok(busy) = this.update(cx, |v, cx| {
                let mut busy = false;
                let mut freshly_done = false;
                for (_, q) in v.queues() {
                    let jobs = q.jobs.lock().unwrap();
                    for j in jobs.iter() {
                        match j.state() {
                            JobState::Queued | JobState::Running => busy = true,
                            JobState::Done => {
                                if v.seen_done.insert(j.id) {
                                    freshly_done = true;
                                }
                            }
                            JobState::Failed | JobState::Paused => {}
                        }
                    }
                }
                v.poll_cut(cx);
                if freshly_done {
                    v.refresh_quiet(cx);
                }
                if !busy {
                    v.ticking = false;
                    if !freshly_done {
                        v.refresh_quiet(cx);
                    }
                }
                cx.notify();
                busy
            }) else {
                break;
            };
            if !busy {
                break;
            }
        })
        .detach();
    }
    /// Send the selection of the active pane to the other pane.
    fn transfer_from(&mut self, from: usize, cx: &mut Context<Self>) {
        if self.active != from || self.panes[from].selection().is_empty() {
            self.set_status(t("sftp.select_first").into(), true);
            cx.notify();
            return;
        }
        let p = &self.panes[from];
        let items = p
            .selection()
            .into_iter()
            .map(|e| (p.path_of(&e.name), e.name))
            .collect();
        self.send(from, 1 - from, items, cx);
        cx.notify();
    }

    // ---- file operations
    fn begin_edit(&mut self, kind: Edit, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.ops_menu = None;
        if !self.panes[pi].ready() {
            self.set_status(t("sftp.not_connected").into(), true);
            cx.notify();
            return;
        }
        let sel = if self.active == pi {
            let picked = self.panes[pi].selection();
            if picked.len() == 1 {
                picked.into_iter().next()
            } else {
                None
            }
        } else {
            None
        };
        let (name, init) = match (kind, sel) {
            (Edit::Mkdir, _) => (String::new(), String::new()),
            (Edit::Rename, Some(e)) => (e.name.clone(), e.name.clone()),
            (Edit::Chmod, Some(e)) => (e.name.clone(), format!("{:o}", e.mode)),
            _ => {
                self.set_status(t("sftp.select_first").into(), true);
                cx.notify();
                return;
            }
        };
        let ph = match kind {
            Edit::Mkdir => t("sftp.new_folder_name"),
            Edit::Rename => t("sftp.new_name"),
            Edit::Chmod => "644",
        };
        let st = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(ph)
                .default_value(init)
        });
        st.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(
            cx.subscribe_in(&st, window, |v, _, ev: &InputEvent, _w, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    v.commit_edit(cx)
                }
            }),
        );
        self.edit = Some((kind, pi, name, st));
        cx.notify();
    }
    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some((kind, pi, name, st)) = self.edit.take() else {
            return;
        };
        let val = st.read(cx).value().trim().to_string();
        if val.is_empty() {
            cx.notify();
            return;
        }
        let p = &self.panes[pi];
        let (s, side) = (p.sess(), p.side());
        let (a, b) = (p.path_of(&name), p.path_of(&val));
        let msg = match kind {
            Edit::Mkdir => tf("sftp.created", &[("p", &b)]),
            Edit::Rename => tf("sftp.renamed", &[("a", &name), ("b", &val)]),
            Edit::Chmod => tf("sftp.chmoded", &[("p", &name), ("m", &val)]),
        };
        let Some(mode) = (kind != Edit::Chmod)
            .then_some(0)
            .or_else(|| sftp::parse_mode(&val))
        else {
            self.set_status(t("sftp.bad_mode").into(), true);
            cx.notify();
            return;
        };
        self.remote_op(
            cx,
            async move {
                match kind {
                    Edit::Mkdir => sftp::mkdir(s.as_deref(), side, &b).await,
                    Edit::Rename => sftp::rename(s.as_deref(), side, &a, &b).await,
                    Edit::Chmod => sftp::chmod(s.as_deref(), side, &a, mode).await,
                }
            },
            move |v, _, cx| v.after_change(msg, cx),
        );
        cx.notify();
    }
    /// Open the selected remote file of pane `pi` in the built-in editor.
    fn edit_selected(&mut self, pi: usize, cx: &mut Context<Self>) {
        self.ops_menu = None;
        let sel = if self.active == pi {
            let picked = self.panes[pi].selection();
            if picked.len() == 1 {
                picked.into_iter().next()
            } else {
                None
            }
        } else {
            None
        };
        let Some(e) = sel.filter(|e| !e.is_dir && !self.panes[pi].is_local()) else {
            self.set_status(t("sftp.edit_select").into(), true);
            cx.notify();
            return;
        };
        let path = self.panes[pi].path_of(&e.name);
        self.open_text(pi, path, false, cx);
    }
    /// Open the selected file read-only, local or remote, like the directory panel.
    fn view_selected(&mut self, pi: usize, cx: &mut Context<Self>) {
        self.ops_menu = None;
        self.blank_menu = None;
        let sel = if self.active == pi {
            let picked = self.panes[pi].selection();
            if picked.len() == 1 {
                picked.into_iter().next()
            } else {
                None
            }
        } else {
            None
        };
        let Some(e) = sel.filter(|e| !e.is_dir) else {
            self.set_status(t("sftp.view_select").into(), true);
            cx.notify();
            return;
        };
        let path = self.panes[pi].path_of(&e.name);
        self.open_text(pi, path, true, cx);
    }
    /// Open a remote text file of the host pane in the built-in editor (waits for the connection).
    pub fn edit_path(&mut self, path: String, cx: &mut Context<Self>) {
        match self.remote_pane() {
            Some(pi) => self.open_text(pi, path, false, cx),
            None => self.set_status(t("sftp.not_connected").into(), true),
        }
    }
    fn open_text(&mut self, pi: usize, path: String, readonly: bool, cx: &mut Context<Self>) {
        let local = self.panes[pi].is_local();
        let host = self.panes[pi].target().map(|t| t.host).unwrap_or_default();
        let (tx, rx) = flume::bounded(1);
        if local {
            let p2 = path.clone();
            self.rt.spawn(async move {
                let r = tokio::task::spawn_blocking(move || {
                    sftp::read_text_local(std::path::Path::new(&p2))
                })
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
                let _ = tx.send(r);
            });
        } else {
            let Some(s) = self.panes[pi].sess() else {
                if self.panes[pi].remote().is_some() && !readonly {
                    self.pending_edit = Some(path);
                } else {
                    self.set_status(t("sftp.not_connected").into(), true);
                }
                cx.notify();
                return;
            };
            let p2 = path.clone();
            self.rt.spawn(async move {
                let _ = tx.send(sftp::read_text(&s, &p2).await);
            });
        }
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let Some(window) = cx.update(|cx| cx.active_window()) else {
                return;
            };
            let _ = window.update(cx, |_, window, cx| {
                this.update(cx, |v, cx| {
                    match r {
                        Ok((text, ver)) => {
                            let st = cx.new(|cx| {
                                EditorState::new(window, cx)
                                    .line_number(true)
                                    .default_value(text)
                            });
                            if !readonly {
                                let _ = crate::audit_log().append(
                                    "sftp.edit.open",
                                    "user",
                                    &format!("host={host} path={path:?} bytes={}", ver.0),
                                );
                            }
                            v.editor = Some(OpenFile {
                                pane: pi,
                                path: path.clone(),
                                ver,
                                text: st,
                                force: false,
                                readonly,
                            });
                            let key = if readonly {
                                "sftp.view_opened"
                            } else {
                                "sftp.edit_opened"
                            };
                            v.set_status(tf(key, &[("p", &path)]), false);
                        }
                        Err(e) => v.set_status(format!("{e:#}"), true),
                    }
                    cx.notify();
                })
            });
        })
        .detach();
    }
    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(ed) = self.editor.clone() else {
            return;
        };
        if ed.readonly {
            return;
        }
        let (pi, path, ver, st, force) = (ed.pane, ed.path, ed.ver, ed.text, ed.force);
        let Some(s) = self.panes[pi].sess() else {
            return;
        };
        let text = st.read(cx).value().to_string();
        let (p2, host) = (
            path.clone(),
            self.panes[pi].target().map(|t| t.host).unwrap_or_default(),
        );
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let _ = tx.send(
                sftp::write_text(&s, &p2, &text, (!force).then_some(ver))
                    .await
                    .map(|v| (v, text.len())),
            );
        });
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok((nv, n)) => {
                        let _ = crate::audit_log().append(
                            "sftp.edit.save",
                            "user",
                            &format!("host={host} path={path:?} bytes={n} overwrite={force}"),
                        );
                        if let Some(ed) = v.editor.as_mut() {
                            ed.ver = nv;
                            ed.force = false;
                        }
                        v.after_change(tf("sftp.edit_saved", &[("p", &path), ("n", &n)]), cx);
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        if msg.contains("changed on the server") {
                            if let Some(ed) = v.editor.as_mut() {
                                ed.force = true;
                            }
                            let _ = crate::audit_log().append(
                                "sftp.edit.conflict",
                                "policy",
                                &format!("host={host} path={path:?}"),
                            );
                        }
                        v.set_status(msg, true);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn remember(&mut self, pi: usize, paths: Vec<String>, cut: bool, cx: &mut Context<Self>) {
        let items = paths
            .into_iter()
            .map(|src| {
                let name = src
                    .trim_end_matches(['/', '\\'])
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("file")
                    .to_string();
                let is_dir = self.panes[pi]
                    .entries
                    .iter()
                    .any(|e| e.name == name && e.is_dir);
                XItem { src, name, is_dir }
            })
            .collect::<Vec<_>>();
        let n = items.len();
        self.clip = Some(Clip {
            from: Some(pi),
            cut,
            items,
        });
        self.set_status(
            tf(
                if cut { "sftp.cut_n" } else { "sftp.copied_n" },
                &[("n", &n)],
            ),
            false,
        );
        cx.notify();
    }
    fn paste_into(&mut self, to: usize, cx: &mut Context<Self>) {
        let Some(clip) = self.clip.clone() else { return };
        self.blank_menu = None;
        self.begin_batch(clip.from, to, clip.items, clip.cut, cx);
    }
    fn visible_names(&self, pi: usize, cx: &gpui::App) -> Vec<String> {
        let p = &self.panes[pi];
        let q = p
            .filter
            .as_ref()
            .map(|f| f.read(cx).value().trim().to_lowercase())
            .unwrap_or_default();
        filelist::sorted_indices(&p.entries, p.sort)
            .into_iter()
            .filter(|&i| {
                let e = &p.entries[i];
                q.is_empty() || e.name.to_lowercase().contains(&q)
            })
            .map(|i| p.entries[i].name.clone())
            .take(2000)
            .collect()
    }
    fn ask_delete(&mut self, pi: usize, cx: &mut Context<Self>) {
        let paths = self.panes[pi]
            .selection()
            .iter()
            .map(|e| self.panes[pi].path_of(&e.name))
            .collect();
        self.ask_delete_paths(pi, paths, cx);
    }
    fn ask_delete_paths(&mut self, pi: usize, paths: Vec<String>, cx: &mut Context<Self>) {
        self.ops_menu = None;
        self.blank_menu = None;
        if paths.is_empty() {
            self.set_status(t("sftp.select_first").into(), true);
        } else {
            self.confirm_delete = Some((pi, paths));
        }
        cx.notify();
    }
    fn delete_confirmed(&mut self, cx: &mut Context<Self>) {
        let Some((pi, paths)) = self.confirm_delete.take() else {
            return;
        };
        let (s, side) = (self.panes[pi].sess(), self.panes[pi].side());
        let n = paths.len();
        let shown = paths.first().cloned().unwrap_or_default();
        self.remote_op(
            cx,
            async move {
                for p in &paths {
                    sftp::remove(s.as_deref(), side, p).await?;
                }
                Ok(())
            },
            move |v, _, cx| {
                let msg = if n == 1 {
                    tf("sftp.deleted", &[("p", &shown)])
                } else {
                    tf("sftp.deleted_n", &[("n", &n)])
                };
                v.after_change(msg, cx);
            },
        );
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
        let Some((l, r)) = self.sync_pair() else {
            return;
        };
        let (Some(s), Some(su)) = (self.panes[r].sess(), self.sync.as_mut()) else {
            return;
        };
        su.busy = true;
        su.plan = None;
        su.confirm_delete = false;
        let (mode, del, ld, rd) = (
            su.mode,
            su.delete,
            self.panes[l].ldir.clone(),
            self.panes[r].rdir.clone(),
        );
        self.remote_op(
            cx,
            async move {
                let l = ssh_term_proto::dirsync::scan_local(&ld)?;
                let r = sftp::scan_remote(&s, &rd).await?;
                Ok(ssh_term_proto::dirsync::plan(&l, &r, mode, del))
            },
            |v, plan, _| {
                if let Some(su) = v.sync.as_mut() {
                    su.busy = false;
                    v.status = tf("sync.previewed", &[("n", &plan.len())]);
                    su.plan = Some(plan);
                }
            },
        );
    }
    fn sync_run(&mut self, cx: &mut Context<Self>) {
        let Some((l, r)) = self.sync_pair() else {
            return;
        };
        let (Some(s), Some(su)) = (self.panes[r].sess(), self.sync.as_mut()) else {
            return;
        };
        let Some(plan) = su.plan.clone() else { return };
        let dels = plan.iter().filter(|o| o.is_delete()).count();
        if dels > 0 && !su.confirm_delete {
            su.confirm_delete = true;
            cx.notify();
            return;
        }
        su.busy = true;
        let smode = su.mode;
        let Some(q) = self.panes[r].remote().map(|x| x.queue.clone()) else {
            return;
        };
        let (ld, rd, host) = (
            self.panes[l].ldir.clone(),
            self.panes[r].rdir.clone(),
            self.panes[r].target().map(|t| t.host).unwrap_or_default(),
        );
        let _ = crate::audit_log().append(
            "sftp.sync",
            "user",
            &format!(
                "host={host} local={:?} remote={rd:?} mode={smode:?} ops={} deletes={dels}",
                ld.display(),
                plan.len()
            ),
        );
        self.remote_op(
            cx,
            async move { sftp::execute_sync(&s, &q, &ld, &rd, &plan).await },
            |v, (q, d, c), cx| {
                v.sync = None;
                v.start_worker(cx);
                v.after_change(tf("sync.done", &[("q", &q), ("d", &d), ("c", &c)]), cx);
            },
        );
    }
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
            v.push(Entry {
                name: format!("{}:", c as char),
                is_dir: true,
                size: 0,
                mode: 0,
                mtime: 0,
            });
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
                let label = p
                    .as_os_str()
                    .to_string_lossy()
                    .trim_end_matches(['\\', '/'])
                    .to_string();
                out.push((label, cur.clone()));
            }
            Component::RootDir => {
                cur.push(c);
                if out.is_empty() {
                    out.push(("/".into(), cur.clone()));
                }
            }
            Component::Normal(s) => {
                cur.push(s);
                out.push((s.to_string_lossy().into_owned(), cur.clone()));
            }
            _ => {
                cur.push(c);
            }
        }
    }
    out
}

fn remote_parts(path: &str) -> Vec<(String, String)> {
    let mut acc = String::from("/");
    let mut out = vec![("/".into(), "/".into())];
    if path == "/" || path.is_empty() {
        return out;
    }
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        acc = if acc == "/" {
            format!("/{seg}")
        } else {
            format!("{acc}/{seg}")
        };
        out.push((seg.to_string(), acc.clone()));
    }
    out
}

/// Element id unique per pane.
fn pid(pi: usize, i: usize) -> usize {
    pi * 1_000_000 + i
}

/// Small square icon button.
fn icon_btn(id: (&'static str, usize), ic: IconName, enabled: bool) -> gpui::Stateful<gpui::Div> {
    let th = cur();
    div()
        .id(id)
        .size(px(H_COMPACT))
        .flex_shrink_0()
        .rounded(px(R_MD))
        .flex()
        .items_center()
        .justify_center()
        .when(enabled, |d| {
            d.cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg()))
        })
        .child(icon(
            ic,
            ui::IC_SM,
            if enabled {
                c(th.subtext)
            } else {
                c(th.overlay0).opacity(0.5)
            },
        ))
}

impl SftpView {
    fn follow_bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let mut bar = div().flex().items_center().gap_1();
        if self.follow_src.is_some() {
            bar = bar.child(
                gpui_component::switch::Switch::new("sftp-follow")
                    .checked(self.follow.on)
                    .label(t("sftp.follow_cwd"))
                    .small()
                    .on_click(cx.listener(|v, on: &bool, _, cx| {
                        v.follow.set(*on);
                        cx.notify()
                    })),
            );
        }
        bar
    }
    /// 页内标题行：沿用弹窗头的图标和字号，但高度改为工具栏，并取消左右内边距以与下方卡片对齐。
    fn page_head(&self, title: &str, cx: &mut Context<Self>) -> gpui::Div {
        ui::dialog_header(
            Some(IconName::FolderSync),
            c(cur().accent),
            title,
            None,
            None,
        )
        .px_0()
        .h(px(ui::TOOLBAR_H))
        .child(self.follow_bar(cx))
    }
    fn frame(&self) -> gpui::Div {
        let th = cur();
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(c(th.base))
            .overflow_hidden()
    }
    fn sync_panel(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        use ssh_term_proto::dirsync::Mode;
        let (l, r) = self.sync_pair()?;
        let su = self.sync.as_ref()?;
        let th = cur();
        let modes = vec![
            ("up".to_string(), t("sync.up").to_string()),
            ("down".to_string(), t("sync.down").to_string()),
            ("both".to_string(), t("sync.both").to_string()),
        ];
        let sel = match su.mode {
            Mode::LocalToRemote => "up",
            Mode::RemoteToLocal => "down",
            Mode::TwoWay => "both",
        };
        let me = cx.entity().downgrade();
        let mut list = div()
            .id("sync-plan")
            .max_h(px(180.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .text_size(px(fs_small()))
            .font_family(ui::mono_family());
        if let Some(plan) = &su.plan {
            if plan.is_empty() {
                list = list.child(div().text_color(c(th.success)).child(t("sync.in_sync")));
            }
            for op in plan.iter().take(300) {
                let col = if op.is_delete() {
                    c(th.error)
                } else if matches!(op, ssh_term_proto::dirsync::Op::Conflict(_)) {
                    c(th.warning)
                } else {
                    c(th.text)
                };
                list = list.child(div().text_color(col).child(format!(
                    "{}  {}",
                    op.sign(),
                    op.path()
                )));
            }
            if plan.len() > 300 {
                list = list.child(format!("… +{}", plan.len() - 300));
            }
        }
        let dels = su
            .plan
            .as_ref()
            .map(|p| p.iter().filter(|o| o.is_delete()).count())
            .unwrap_or(0);
        let mut bar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(ui::segmented("sync-mode", &modes, sel, move |v, _, cx| {
                let _ = me.update(cx, |p, cx| {
                    if let Some(su) = p.sync.as_mut() {
                        su.mode = match v.as_str() {
                            "down" => Mode::RemoteToLocal,
                            "both" => Mode::TwoWay,
                            _ => Mode::LocalToRemote,
                        };
                        su.plan = None;
                    }
                    cx.notify()
                });
            }))
            .child(
                gpui_component::switch::Switch::new("sync-del")
                    .checked(su.delete)
                    .label(t("sync.delete_extra"))
                    .on_click(cx.listener(|v, on: &bool, _, cx| {
                        if let Some(su) = v.sync.as_mut() {
                            su.delete = *on;
                            su.plan = None;
                        }
                        cx.notify()
                    })),
            )
            .child(div().flex_1())
            .child(
                ui::button(
                    "sync-preview",
                    if su.busy {
                        t("sync.scanning")
                    } else {
                        t("sync.preview")
                    },
                    0,
                )
                .on_click(cx.listener(|v, _, _, cx| v.sync_preview(cx))),
            );
        if su.plan.as_ref().is_some_and(|p| !p.is_empty()) {
            bar = bar.child(
                ui::button(
                    "sync-run",
                    if su.confirm_delete {
                        tf("sync.confirm_delete", &[("n", &dels)])
                    } else {
                        t("sync.run").to_string()
                    },
                    if su.confirm_delete || dels > 0 { 2 } else { 1 },
                )
                .on_click(cx.listener(|v, _, _, cx| v.sync_run(cx))),
            );
        }
        bar = bar.child(
            ui::button("sync-close", t("ui.cancel"), 3).on_click(cx.listener(|v, _, _, cx| {
                v.sync = None;
                cx.notify()
            })),
        );
        Some(
            div()
                .mt_2()
                .p_3()
                .rounded(px(R_MD))
                .border_1()
                .border_color(c(th.accent).opacity(0.5))
                .flex()
                .flex_col()
                .gap_2()
                .text_size(px(fs_small()))
                .text_color(c(th.text))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(tf(
                    "sync.title",
                    &[
                        ("l", &self.panes[l].ldir.display()),
                        ("r", &self.panes[r].rdir),
                    ],
                )))
                .child(bar)
                .children(su.confirm_delete.then(|| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_color(c(th.error))
                        .child(icon(IconName::TriangleAlert, ui::IC_SM, c(th.error)))
                        .child(tf("sync.delete_warn", &[("n", &dels)]))
                }))
                .child(list),
        )
    }

    fn toggle_sort(&mut self, pi: usize, key: SortKey, cx: &mut Context<Self>) {
        self.panes[pi].sort.toggle(key);
        cx.notify();
    }
    fn sort_head(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let sort = self.panes[pi].sort;
        let cell = |id: usize, label: String, key: SortKey| {
            filelist::sort_cell(
                ("sftp-sort", pid(pi, id)),
                label,
                sort.key == key,
                sort.desc,
            )
            .on_click(cx.listener(move |v, _, _, cx| v.toggle_sort(pi, key, cx)))
        };
        let cols = filelist::cols_wide();
        filelist::sort_header()
            .child(
                cell(0, t("sftp.col_name").into(), SortKey::Name)
                    .flex_1()
                    .min_w_0(),
            )
            .child(
                cell(1, t("sftp.col_mtime").into(), SortKey::Mtime)
                    .w(px(cols.mtime.unwrap_or(0.)))
                    .flex_shrink_0(),
            )
            .child(
                cell(2, t("sftp.col_size").into(), SortKey::Size)
                    .w(px(cols.size.unwrap_or(0.)))
                    .flex_shrink_0(),
            )
            .child(
                cell(3, t("sftp.col_type").into(), SortKey::Kind)
                    .w(px(cols.kind.unwrap_or(0.)))
                    .flex_shrink_0(),
            )
            .into_any_element()
    }

    /// Header: source (click to change host), filter toggle, actions menu.
    fn pane_header(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let p = &self.panes[pi];
        let (ic, name) = match &p.src {
            Src::Remote(_) => (crate::file_icon::server_icon(ui::IC_MD), p.label()),
            _ => (crate::file_icon::local_icon(ui::IC_MD), p.label()),
        };
        let tags = p
            .target()
            .and_then(|t| {
                config_hosts()
                    .iter()
                    .find(|(_, x)| x.pool_key() == t.pool_key())
                    .cloned()
            })
            .map(|(h, _)| h.tags)
            .unwrap_or_default();
        let s = crate::config::get();
        let filter_on = p.filter.is_some();
        ui::toolbar()
            .child(
                div()
                    .id(("sftp-src", pi))
                    .h(px(H_DEFAULT))
                    .px_2()
                    .min_w_0()
                    .rounded(px(R_MD))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(|d| d.bg(crate::theme::hover_bg()))
                    .tooltip(crate::ui::tip(t("sftp.change_source")))
                    .on_click(cx.listener(move |v, _, w, cx| v.open_picker(pi, w, cx)))
                    .child(ic)
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(fs_body()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.text))
                            .child(name),
                    )
                    .children(tags.iter().take(3).map(|tg| {
                        div()
                            .size(px(7.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(c(s.tags.rgb(tg)))
                    }))
                    .child(icon(IconName::ChevronDown, ui::IC_XS, c(th.overlay1))),
            )
            .child(div().flex_1())
            .child(
                ui::button(("sftp-filter", pi), t("sftp.filter"), 3)
                    .h(px(H_COMPACT))
                    .px_2()
                    .when(filter_on, |d| {
                        d.bg(crate::theme::selected_bg()).text_color(c(th.accent))
                    })
                    .child(icon(
                        IconName::ListFilter,
                        ui::IC_SM,
                        if filter_on {
                            c(th.accent)
                        } else {
                            c(th.subtext)
                        },
                    ))
                    .on_click(cx.listener(move |v, _, w, cx| v.toggle_filter(pi, w, cx))),
            )
            .child(
                ui::button(("sftp-ops", pi), t("sftp.actions"), 3)
                    .h(px(H_COMPACT))
                    .px_2()
                    .child(icon(IconName::ChevronDown, ui::IC_XS, c(th.subtext)))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| {
                            v.ops_menu = if v.ops_menu.as_ref().is_some_and(|m| m.0 == pi) {
                                None
                            } else {
                                Some((pi, ev.position, std::time::Instant::now()))
                            };
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }

    fn nav_row(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = &self.panes[pi];
        let mut bar = div()
            .h(px(ui::H_LARGE))
            .flex_shrink_0()
            .px_2()
            .flex()
            .items_center()
            .gap(px(2.))
            .bg(crate::theme::sub_bg())
            .overflow_hidden()
            .child(
                icon_btn(("sftp-back", pi), IconName::ArrowLeft, !p.back.is_empty())
                    .on_click(cx.listener(move |v, _, _, cx| v.go_back(pi, cx))),
            )
            .child(
                icon_btn(("sftp-fwd", pi), IconName::ArrowRight, !p.fwd.is_empty())
                    .on_click(cx.listener(move |v, _, _, cx| v.go_fwd(pi, cx))),
            )
            .child(div().w(px(4.)));
        let mut parts: Vec<(String, String)> = vec![];
        if p.is_local() {
            if cfg!(windows) {
                parts.push((t("sftp.this_pc").to_string(), String::new()));
            }
            if !p.ldir.as_os_str().is_empty() {
                parts.extend(
                    local_parts(&p.ldir)
                        .into_iter()
                        .map(|(l, d)| (l, d.to_string_lossy().into_owned())),
                );
            }
        } else {
            parts = remote_parts(&p.rdir);
        }
        let n = parts.len();
        for (i, (label, dir)) in parts.into_iter().enumerate() {
            if i > 0 {
                bar = bar.child(ui::crumb_sep());
            }
            bar = bar.child(
                ui::crumb(
                    ("sftp-crumb", pid(pi, i)),
                    None,
                    Some(label.into()),
                    i + 1 == n,
                )
                .on_click(cx.listener(move |v, _, _, cx| v.goto(pi, dir.clone(), cx))),
            );
        }
        bar.into_any_element()
    }

    fn file_list(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let p = &self.panes[pi];
        let mut list = div()
            .id(("sftp-list", pi))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        if let Some(r) = p.remote() {
            if r.connecting {
                return list
                    .child(
                        div()
                            .p_4()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(fs_small()))
                            .text_color(c(th.subtext))
                            .child(icon(IconName::LoaderCircle, ui::IC_SM, c(th.accent)))
                            .child(tf("sftp.connecting", &[("t", &r.name)])),
                    )
                    .child(filelist::listing_skeleton(if pi == 0 {
                        "sftp-skel-0"
                    } else {
                        "sftp-skel-1"
                    }))
                    .into_any_element();
            }
            if let Some(e) = &r.error {
                return list
                    .child(ui::empty_state(
                        IconName::CircleAlert,
                        t("sftp.failed_title"),
                        Some(e.clone().into()),
                        vec![
                            ui::button(("sftp-retry", pi), t("sftp.retry"), 1)
                                .on_click(cx.listener(move |v, _, _, cx| v.connect(pi, cx)))
                                .into_any_element(),
                            ui::button(("sftp-goback", pi), t("sftp.pick_other"), 0)
                                .on_click(cx.listener(move |v, _, w, cx| v.open_picker(pi, w, cx)))
                                .into_any_element(),
                        ],
                        true,
                    ))
                    .into_any_element();
            }
        }
        let at_drives = p.is_local() && p.ldir.as_os_str().is_empty();
        let dark = th.appearance == Appearance::Dark;
        if !at_drives {
            list = list.child(
                filelist::parent_row(("sftp-up", pi), dark)
                    .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _, cx| {
                        if ev.click_count() >= 2 {
                            v.up(pi, cx)
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| {
                            v.open_blank(pi, ev.position, cx);
                            cx.stop_propagation();
                        }),
                    ),
            );
        }
        if p.loading {
            return list
                .child(filelist::listing_skeleton(if pi == 0 {
                    "sftp-skel-0"
                } else {
                    "sftp-skel-1"
                }))
                .into_any_element();
        }
        let q = p
            .filter
            .as_ref()
            .map(|f| f.read(cx).value().trim().to_lowercase())
            .unwrap_or_default();
        let cols = filelist::cols_wide();
        let selected_on = self.active == pi;
        let mut shown = 0;
        for i in filelist::sorted_indices(&p.entries, p.sort) {
            let e = &p.entries[i];
            if !q.is_empty() && !e.name.to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            if shown > 2000 {
                break;
            }
            let selected = selected_on && p.picked.iter().any(|n| n == &e.name);
            let drag = DragFile {
                pane: pi,
                path: p.path_of(&e.name),
                name: e.name.clone(),
            };
            let row_id = pid(pi, i);
            let name = e.name.clone();
            let check = p.pick_mode.then(|| {
                let on = p.picked.iter().any(|n| n == &name);
                let toggle_name = name.clone();
                let f: Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static> =
                    Box::new(cx.listener(move |v, _, _, cx| {
                        if v.active != pi {
                            v.panes[v.active].clear_sel();
                            v.active = pi;
                        }
                        v.panes[pi].toggle_name(&toggle_name, i);
                        cx.stop_propagation();
                        cx.notify();
                    }));
                (("sftp-ck", row_id).into(), on, f)
            });
            list = list.child(
                filelist::entry_checked(("sftp-row", row_id), e, selected, dark, cols, check)
                    .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _, cx| {
                        if ev.click_count() >= 2 {
                            v.open(pi, i, cx)
                        } else {
                            v.select(pi, i, ev.modifiers());
                            cx.notify()
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| {
                            v.open_ctx_menu(pi, i, ev.position, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_drag(drag, |d: &DragFile, _, _, cx| {
                        cx.new(|_| DragPreview(d.name.clone()))
                    }),
            );
        }
        list = list.on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| {
                v.open_blank(pi, ev.position, cx);
                cx.stop_propagation();
            }),
        );
        if shown == 0 && !q.is_empty() {
            list = list.child(
                div()
                    .p_4()
                    .text_size(px(fs_small()))
                    .text_color(c(th.overlay1))
                    .child(t("sftp.filter_none")),
            );
        }
        list.into_any_element()
    }

    /// "Connect to a host" placeholder of a pane without a source.
    fn empty_state(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        ui::empty_state(
            IconName::Folder,
            t("sftp.connect_title"),
            Some(t("sftp.connect_desc").into()),
            vec![ui::button(("sftp-choose", pi), t("sftp.choose_host"), 1)
                .on_click(cx.listener(move |v, _, w, cx| v.open_picker(pi, w, cx)))
                .into_any_element()],
            true,
        )
        .into_any_element()
    }

    /// In-pane host picker: back / title / Local, search, groups, hosts.
    fn picker_view(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let Some(pk) = self.panes[pi].picker.as_ref() else {
            return div().into_any_element();
        };
        let s = crate::config::get();
        let query = pk.search.read(cx).value().trim().to_string();
        let group = pk.group.clone();
        let title = if group.is_empty() {
            t("sftp.choose_host").to_string()
        } else {
            hosttree::leaf_of(&group).to_string()
        };
        let can_back = !group.is_empty() || !matches!(self.panes[pi].src, Src::Empty);
        let head = ui::toolbar()
            .when(can_back, |d| {
                d.child(
                    icon_btn(("sftp-pk-back", pi), IconName::ArrowLeft, true)
                        .on_click(cx.listener(move |v, _, _, cx| v.picker_back(pi, cx))),
                )
            })
            .when(!can_back, |d| d.pl_1())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(fs_body()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(th.text))
                    .child(title),
            )
            .child(
                ui::button(("sftp-pk-local", pi), t("sftp.local_btn"), 0)
                    .h(px(H_COMPACT))
                    .px_2()
                    .child(crate::file_icon::local_icon(ui::IC_SM))
                    .on_click(cx.listener(move |v, _, _, cx| v.choose_local(pi, cx))),
            );
        let search = ui::field_row()
            .py_2()
            .child(ui::search_field(&pk.search, false));
        let section = |label: String| {
            div()
                .px_3()
                .pt_2()
                .pb_1()
                .text_size(px(fs_small()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(th.overlay1))
                .child(label)
        };
        let mut list = div()
            .id(("sftp-pk-list", pi))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb_2();
        let resolved = config_hosts();
        let q = hosttree::parse_query(&query, &s.tags.all());
        let hosts: Vec<usize> = (0..s.hosts.len())
            .filter(|&i| {
                let h = &s.hosts[i];
                if query.is_empty() {
                    h.group.as_deref().unwrap_or("") == group
                } else {
                    hosttree::host_matches(h, &s.groups, &q)
                }
            })
            .collect();
        let groups: Vec<&hosttree::GroupEntry> = if query.is_empty() {
            s.groups
                .iter()
                .filter(|g| hosttree::parent_of(&g.path) == group && g.path != group)
                .collect()
        } else {
            vec![]
        };
        if !groups.is_empty() {
            list = list.child(section(t("sftp.groups").to_string()));
            for (gi, g) in groups.into_iter().enumerate() {
                let path = g.path.clone();
                let n = hosttree::hosts_under(&s.hosts, &g.path).len();
                list = list.child(
                    div()
                        .id(("sftp-pk-g", pid(pi, gi)))
                        .mx_2()
                        .h(px(36.))
                        .flex_shrink_0()
                        .px_2()
                        .rounded(px(R_MD))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(p) = v.panes[pi].picker.as_mut() {
                                p.group = path.clone();
                            }
                            cx.notify()
                        }))
                        .child(icon(IconName::Folder, ui::IC_MD, c(th.accent)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(fs_body()))
                                .text_color(c(th.text))
                                .child(hosttree::leaf_of(&g.path).to_string()),
                        )
                        .children(g.tags.iter().take(3).map(|tg| {
                            div()
                                .size(px(7.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(c(s.tags.rgb(tg)))
                        }))
                        .child(
                            div()
                                .text_size(px(fs_small()))
                                .text_color(c(th.overlay0))
                                .child(n.to_string()),
                        )
                        .child(icon(IconName::ChevronRight, ui::IC_SM, c(th.overlay1))),
                );
            }
        }
        if !hosts.is_empty() {
            list = list.child(section(t("sftp.hosts").to_string()));
            for i in hosts {
                let h = s.hosts[i].clone();
                let Some(target) = resolved
                    .iter()
                    .find(|(x, _)| x.name == h.name)
                    .map(|(_, t)| t.clone())
                else {
                    continue;
                };
                let tags = hosttree::effective_tags(&h, &s.groups);
                let sub = format!("ssh, {}", target.user);
                let (h2, t2) = (h.clone(), target.clone());
                list = list.child(
                    div()
                        .id(("sftp-pk-h", pid(pi, i)))
                        .mx_2()
                        .h(px(44.))
                        .flex_shrink_0()
                        .px_2()
                        .rounded(px(R_MD))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            v.choose_host(pi, h2.clone(), t2.clone(), cx)
                        }))
                        .child(
                            div()
                                .size(px(28.))
                                .flex_shrink_0()
                                .rounded(px(R_MD))
                                .bg(c(th.surface0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(IconName::Server, ui::IC_MD, c(th.accent))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(fs_body()))
                                        .text_color(c(th.text))
                                        .child(h.name.clone()),
                                )
                                .child(
                                    div()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(fs_small()))
                                        .text_color(c(th.overlay1))
                                        .child(sub),
                                ),
                        )
                        .children(tags.iter().take(3).map(|tg| {
                            div()
                                .size(px(8.))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(c(s.tags.rgb(tg)))
                        })),
                );
            }
        }
        if list_is_empty(&s.hosts, &s.groups, &group, &query, &q) {
            list = list.child(
                div()
                    .p_4()
                    .text_size(px(fs_small()))
                    .text_color(c(th.overlay1))
                    .child(if query.is_empty() {
                        t("sftp.no_hosts")
                    } else {
                        t("sftp.no_match")
                    }),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(head)
            .child(search)
            .child(list)
            .into_any_element()
    }

    fn pane(&self, pi: usize, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let p = &self.panes[pi];
        let mut card = div()
            .id(("sftp-pane-card", pi))
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .border_1()
            .border_color(crate::theme::divider())
            .rounded(px(ui::R_LG))
            .overflow_hidden();
        if p.picker.is_some() {
            return card.child(self.picker_view(pi, cx)).into_any_element();
        }
        if matches!(p.src, Src::Empty) {
            return card.child(self.empty_state(pi, cx)).into_any_element();
        }
        let ready = p.ready();
        card = card
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |v, _, _, _| {
                    if v.active != pi {
                        v.panes[v.active].clear_sel();
                        v.active = pi;
                    }
                }),
            )
            .drag_over::<DragFile>(move |st, d, _, _| {
                if d.pane != pi && ready {
                    st.border_color(c(cur().accent))
                } else {
                    st
                }
            })
            .drag_over::<ExternalPaths>(move |st, _, _, _| {
                if ready {
                    st.border_color(c(cur().accent))
                } else {
                    st
                }
            })
            .on_drop(cx.listener(move |v, d: &DragFile, _, cx| {
                if d.pane != pi {
                    v.send(d.pane, pi, vec![(d.path.clone(), d.name.clone())], cx);
                }
            }))
            .on_drop(cx.listener(move |v, ps: &ExternalPaths, _, cx| {
                let items = ps
                    .paths()
                    .iter()
                    .map(|f| XItem {
                        src: f.to_string_lossy().into_owned(),
                        name: f
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        is_dir: f.is_dir(),
                    })
                    .collect();
                v.begin_batch(None, pi, items, false, cx);
                cx.notify();
            }))
            .child(self.pane_header(pi, cx));
        if ready {
            card = card.child(self.nav_row(pi, cx));
            if let Some(f) = &p.filter {
                card = card.child(
                    ui::field_row()
                        .py(px(6.))
                        .border_b_1()
                        .border_color(crate::theme::divider())
                        .child(ui::text_field(f, false).cleanable(true).prefix(icon(
                            IconName::ListFilter,
                            ui::IC_SM,
                            c(th.overlay1),
                        ))),
                );
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
        let one_sel = self.active == pi && p.selection().len() == 1;
        let to_label = if p.is_local() && !self.panes[1 - pi].is_local() {
            t("sftp.upload").to_string()
        } else if !p.is_local() && self.panes[1 - pi].is_local() {
            t("sftp.download").to_string()
        } else {
            t("sftp.copy_other").to_string()
        };
        let mut k = 0usize;
        let mut id = || {
            k += 1;
            ("sftp-opm", pid(pi, k))
        };
        let mut list = ui::popover()
            .id("sftp-ops-menu")
            .occlude()
            .min_w(px(220.))
            .p(px(5.))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                if v.ops_menu
                    .as_ref()
                    .is_some_and(|m| m.2.elapsed() < std::time::Duration::from_millis(250))
                {
                    return;
                }
                v.ops_menu = None;
                cx.notify();
            }));
        macro_rules! item {
            ($ic:expr, $label:expr, $en:expr, $danger:expr, $f:expr) => {{
                let f = $f;
                let el = ui::menu_item(id(), Some($ic), $label, $en, $danger);
                list = list.child(if $en {
                    el.on_click(cx.listener(
                        move |v: &mut Self, _, w: &mut Window, cx: &mut Context<Self>| {
                            v.ops_menu = None;
                            f(v, w, cx);
                            cx.notify();
                        },
                    ))
                } else {
                    el
                });
            }};
        }
        item!(
            IconName::FolderPlus,
            t("sftp.mkdir"),
            ready,
            false,
            move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.begin_edit(
                Edit::Mkdir,
                pi,
                w,
                cx
            )
        );
        item!(
            IconName::RefreshCw,
            t("sftp.refresh"),
            ready,
            false,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.refresh(pi, cx)
        );
        item!(
            IconName::ListChecks,
            if p.pick_mode {
                t("sftp.multi_off")
            } else {
                t("sftp.multi")
            },
            ready,
            false,
            move |v: &mut Self, _w: &mut Window, _cx: &mut Context<Self>| {
                v.panes[pi].pick_mode = !v.panes[pi].pick_mode;
            }
        );
        item!(
            if p.hidden {
                IconName::EyeOff
            } else {
                IconName::Eye
            },
            if p.hidden {
                t("sftp.hide_hidden")
            } else {
                t("sftp.show_hidden")
            },
            ready,
            false,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| {
                v.panes[pi].hidden = !v.panes[pi].hidden;
                v.refresh(pi, cx)
            }
        );
        list = list.child(ui::menu_sep());
        item!(
            if p.is_local() {
                IconName::Upload
            } else {
                IconName::Download
            },
            to_label,
            ready && other_ready && has_sel,
            false,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.transfer_from(pi, cx)
        );
        item!(
            IconName::Pencil,
            t("sftp.rename"),
            ready && one_sel,
            false,
            move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.begin_edit(
                Edit::Rename,
                pi,
                w,
                cx
            )
        );
        item!(
            IconName::Lock,
            t("sftp.chmod"),
            ready && one_sel,
            false,
            move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.begin_edit(
                Edit::Chmod,
                pi,
                w,
                cx
            )
        );
        item!(
            IconName::Eye,
            t("dir.view"),
            ready && one_sel && p.selection().first().is_some_and(|e| !e.is_dir),
            false,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.view_selected(pi, cx)
        );
        item!(
            IconName::FileText,
            t("sftp.edit"),
            ready && remote && one_sel,
            false,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.edit_selected(pi, cx)
        );
        if self.sync_pair().is_some() {
            item!(
                IconName::FolderSync,
                t("sync.button"),
                true,
                false,
                |v: &mut Self, _w: &mut Window, _cx: &mut Context<Self>| {
                    v.sync = Some(SyncUi {
                        mode: ssh_term_proto::dirsync::Mode::LocalToRemote,
                        delete: false,
                        plan: None,
                        busy: false,
                        confirm_delete: false,
                    });
                }
            );
        }
        item!(
            IconName::Trash,
            t("sftp.delete"),
            ready && has_sel,
            true,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.ask_delete(pi, cx)
        );
        list = list.child(ui::menu_sep());
        item!(
            IconName::ArrowLeftRight,
            t("sftp.change_source"),
            true,
            false,
            move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| v.open_picker(pi, w, cx)
        );
        if remote {
            item!(
                IconName::Unplug,
                t("sftp.disconnect"),
                true,
                true,
                move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.disconnect(pi, cx)
            );
        }
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(pos)
                    .snap_to_window_with_margin(px(8.))
                    .child(crate::chrome::pop_in("sftp-ops-pop", list)),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    fn open_blank(&mut self, pi: usize, pos: Point<Pixels>, cx: &mut Context<Self>) {
        self.ctx_menu = None;
        self.ops_menu = None;
        if self.active != pi {
            self.panes[self.active].clear_sel();
            self.active = pi;
        }
        self.blank_menu = Some((pi, pos, std::time::Instant::now()));
        cx.notify();
    }
    fn blank_menu_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pi, pos, _) = self.blank_menu.clone()?;
        let p = &self.panes[pi];
        let ready = p.ready();
        let has_sel = self.active == pi && !p.selection().is_empty();
        let other_ready = self.panes[1 - pi].ready();
        let mut k = 0usize;
        let mut id = || {
            k += 1;
            ("sftp-blank", pid(pi, k))
        };
        let mut list = ui::popover()
            .id("sftp-blank-menu")
            .occlude()
            .min_w(px(220.))
            .p(px(5.))
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                if v.blank_menu
                    .as_ref()
                    .is_some_and(|m| m.2.elapsed() < std::time::Duration::from_millis(250))
                {
                    return;
                }
                v.blank_menu = None;
                cx.notify();
            }));
        macro_rules! item {
            ($ic:expr, $label:expr, $en:expr, $f:expr) => {{
                let f = $f;
                let el = ui::menu_item(id(), Some($ic), $label, $en, false);
                list = list.child(if $en {
                    el.on_click(cx.listener(
                        move |v: &mut Self, _, w: &mut Window, cx: &mut Context<Self>| {
                            v.blank_menu = None;
                            f(v, w, cx);
                            cx.notify();
                        },
                    ))
                } else {
                    el
                });
            }};
        }
        item!(
            IconName::FolderPlus,
            t("sftp.mkdir"),
            ready,
            move |v: &mut Self, w: &mut Window, cx: &mut Context<Self>| {
                v.begin_edit(Edit::Mkdir, pi, w, cx)
            }
        );
        item!(
            IconName::RefreshCw,
            t("sftp.refresh"),
            ready,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.refresh(pi, cx)
        );
        item!(
            IconName::ListChecks,
            if p.pick_mode {
                t("sftp.multi_off")
            } else {
                t("sftp.multi")
            },
            ready,
            move |v: &mut Self, _w: &mut Window, _cx: &mut Context<Self>| {
                v.panes[pi].pick_mode = !v.panes[pi].pick_mode;
            }
        );
        item!(
            IconName::CheckCheck,
            t("sftp.select_all"),
            ready,
            move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| {
                v.panes[pi].picked = v.visible_names(pi, cx);
                v.panes[pi].sel = v.panes[pi].picked.first().and_then(|n| {
                    v.panes[pi].entries.iter().position(|e| &e.name == n)
                });
            }
        );
        item!(
            IconName::Square,
            t("sftp.select_none"),
            ready && has_sel,
            move |v: &mut Self, _w: &mut Window, _cx: &mut Context<Self>| {
                v.panes[pi].clear_sel();
            }
        );
        if has_sel {
            let to_label = if p.is_local() && !self.panes[1 - pi].is_local() {
                t("sftp.upload")
            } else if !p.is_local() && self.panes[1 - pi].is_local() {
                t("sftp.download")
            } else {
                t("sftp.copy_other")
            };
            item!(
                IconName::ArrowRight,
                to_label,
                ready && other_ready,
                move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.transfer_from(pi, cx)
            );
            item!(
                IconName::Trash,
                t("sftp.delete"),
                ready,
                move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.ask_delete(pi, cx)
            );
        }
        if self.clip.is_some() {
            item!(
                IconName::ClipboardPaste,
                t("sftp.paste"),
                ready,
                move |v: &mut Self, _w: &mut Window, cx: &mut Context<Self>| v.paste_into(pi, cx)
            );
        }
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(pos)
                    .snap_to_window_with_margin(px(8.))
                    .child(crate::chrome::pop_in("sftp-blank-pop", list)),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
    fn conflict_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.asking {
            return None;
        }
        let b = self.batch.as_ref()?;
        let name = b.items.get(b.index)?.name.clone();
        let th = cur();
        let rest = self.conflict_rest;
        Some(
            div()
                .mt_2()
                .p_2()
                .rounded(px(R_MD))
                .bg(c(th.warning).opacity(0.12))
                .border_1()
                .border_color(c(th.warning).opacity(0.5))
                .flex()
                .items_center()
                .gap_2()
                .flex_wrap()
                .text_size(px(fs_small()))
                .text_color(c(th.text))
                .child(icon(IconName::TriangleAlert, ui::IC_SM, c(th.warning)))
                .child(div().flex_1().min_w(px(160.)).child(tf(
                    "sftp.conflict",
                    &[("name", &name)],
                )))
                .child(
                    ui::button("sftp-over", t("sftp.overwrite"), 2).on_click(cx.listener(
                        |v, _, _, cx| v.apply_choice(WhenExists::Overwrite, cx),
                    )),
                )
                .child(
                    ui::button("sftp-skip", t("sftp.skip"), 3).on_click(cx.listener(
                        |v, _, _, cx| v.apply_choice(WhenExists::Skip, cx),
                    )),
                )
                .child(
                    ui::button("sftp-keep", t("sftp.keep_both"), 1).on_click(cx.listener(
                        |v, _, _, cx| v.apply_choice(WhenExists::KeepBoth, cx),
                    )),
                )
                .child(
                    ui::button(
                        "sftp-rest",
                        t("sftp.apply_rest"),
                        if rest { 1 } else { 3 },
                    )
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.conflict_rest = !v.conflict_rest;
                        cx.notify();
                    })),
                )
                .child(
                    ui::button("sftp-stop", t("ui.cancel"), 3).on_click(cx.listener(|v, _, _, cx| {
                        v.batch = None;
                        v.asking = false;
                        v.conflict_rest = false;
                        cx.notify();
                    })),
                )
                .into_any_element(),
        )
    }
    fn queue_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let qs = self.queues();
        let jobs: Vec<(usize, Queue, Arc<sftp::Job>)> = qs
            .iter()
            .flat_map(|(pi, q)| {
                q.jobs
                    .lock()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .map(move |j| (*pi, q.clone(), j))
            })
            .collect();
        let mut col = div()
            .id("sftp-queue")
            .h(px(130.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .border_1()
            .border_color(crate::theme::divider())
            .rounded(px(ui::R_LG));
        col = col.child(
            ui::table_header()
                .px_3()
                .gap_2()
                .child(tf("sftp.queue_title", &[("n", &jobs.len())]))
                .child(div().flex_1())
                .child(
                    ui::button_sm("sftp-clear", t("sftp.clear_done"), 3).on_click(cx.listener(
                        |v, _, _, cx| {
                            for (_, q) in v.queues() {
                                q.clear_done();
                            }
                            cx.notify()
                        },
                    )),
                ),
        );
        if jobs.is_empty() {
            col = col.child(
                div()
                    .p_3()
                    .text_size(px(fs_small()))
                    .text_color(c(th.overlay1))
                    .child(t("sftp.queue_empty")),
            );
        }
        let two = qs.len() > 1;
        for (pi, q, j) in jobs.iter().rev() {
            let st = j.state();
            let (label, color) = match st {
                JobState::Queued => (t("sftp.st_queued"), c(th.overlay1)),
                JobState::Running => (t("sftp.st_running"), c(th.info)),
                JobState::Done => (t("sftp.st_done"), c(th.success)),
                JobState::Failed => (t("sftp.st_failed"), c(th.error)),
                JobState::Paused => (t("sftp.st_paused"), c(th.warning)),
            };
            let pct = j.progress();
            let done = j.done.load(std::sync::atomic::Ordering::Relaxed);
            let total = j.total.load(std::sync::atomic::Ordering::Relaxed);
            let resumed = j.resumed_from.load(std::sync::atomic::Ordering::Relaxed);
            let id = j.id;
            let key = pid(*pi, id as usize);
            let (q1, q2) = (q.clone(), q.clone());
            let btn = match st {
                JobState::Running | JobState::Queued => Some(
                    ui::button_sm(("job-p", key), t("sftp.pause"), 3).on_click(cx.listener(
                        move |_, _, _, cx| {
                            q1.pause(id);
                            cx.notify()
                        },
                    )),
                ),
                JobState::Paused | JobState::Failed => Some(
                    ui::button_sm(("job-r", key), t("sftp.resume"), 3).on_click(cx.listener(
                        move |v, _, _, cx| {
                            q2.resume(id);
                            v.start_worker(cx);
                            cx.notify()
                        },
                    )),
                ),
                JobState::Done => None,
            };
            let host = if two {
                format!("{} · ", self.panes[*pi].label())
            } else {
                String::new()
            };
            col = col.child(
                div()
                    .px_3()
                    .py(px(4.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(crate::theme::divider())
                    .text_size(px(fs_small()))
                    .text_color(c(th.text))
                    .child(icon(
                        if j.dir == Dir::Upload {
                            IconName::Upload
                        } else {
                            IconName::Download
                        },
                        ui::IC_SM,
                        c(th.accent),
                    ))
                    .child(
                        div()
                            .w(px(200.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(format!("{host}{}", j.name())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(6.))
                            .rounded_full()
                            .bg(c(th.surface0))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(color)
                                    .w(gpui::relative(pct.clamp(0., 1.))),
                            ),
                    )
                    .child(
                        div()
                            .w(px(150.))
                            .text_size(px(fs_small()))
                            .text_color(c(th.overlay1))
                            .child(format!(
                                "{} / {}{}",
                                filelist::human(done),
                                filelist::human(total),
                                if resumed > 0 {
                                    format!(
                                        " · {}",
                                        tf("sftp.resumed", &[("n", &filelist::human(resumed))])
                                    )
                                } else {
                                    String::new()
                                }
                            )),
                    )
                    .child(div().w(px(60.)).child(ui::tag(label, color)))
                    .children(btn)
                    .when_some(j.error.lock().unwrap().clone(), |d, e| {
                        d.child(
                            div()
                                .text_color(c(th.error))
                                .text_size(px(fs_small()))
                                .child(e),
                        )
                    }),
            );
        }
        col.into_any_element()
    }
}

fn list_is_empty(
    hosts: &[HostEntry],
    groups: &[hosttree::GroupEntry],
    group: &str,
    query: &str,
    q: &hosttree::Query,
) -> bool {
    if query.is_empty() {
        !hosts
            .iter()
            .any(|h| h.group.as_deref().unwrap_or("") == group)
            && !groups
                .iter()
                .any(|g| hosttree::parent_of(&g.path) == group && g.path != group)
    } else {
        !hosts.iter().any(|h| hosttree::host_matches(h, groups, q))
    }
}

impl Render for SftpView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // a pane without a source shows the remote host list right away
        for pi in 0..2 {
            if matches!(self.panes[pi].src, Src::Empty) && self.panes[pi].picker.is_none() {
                let st =
                    cx.new(|cx| InputState::new(window, cx).placeholder(t("sftp.search_hosts")));
                self._subs.push(
                    cx.subscribe_in(&st, window, |_, _, ev: &InputEvent, _, cx| {
                        if matches!(ev, InputEvent::Change) {
                            cx.notify()
                        }
                    }),
                );
                self.panes[pi].picker = Some(Picker {
                    search: st,
                    group: String::new(),
                });
            }
        }
        if let Some(pi) = self.remote_pane() {
            if self.panes[pi].sess().is_some() {
                if let Some(p) = self.pending_edit.take() {
                    self.open_text(pi, p, false, cx);
                }
            }
        }
        let th = cur();
        let edit_bar = self.edit.as_ref().map(|(k, pi, name, st)| {
            let what = match k {
                Edit::Mkdir => t("sftp.mkdir").to_string(),
                Edit::Rename => tf("sftp.rename_x", &[("x", name)]),
                Edit::Chmod => tf("sftp.chmod_x", &[("x", name)]),
            };
            let where_ = self.panes[*pi].label();
            div()
                .mt_2()
                .p_2()
                .rounded(px(R_MD))
                .border_1()
                .border_color(c(th.accent).opacity(0.5))
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(fs_small()))
                .text_color(c(th.text))
                .child(format!("{what} ({where_})"))
                .child(div().w(px(260.)).child(ui::text_field(st, false)))
                .child(
                    ui::button("edit-ok", t("ui.save"), 1)
                        .on_click(cx.listener(|v, _, _, cx| v.commit_edit(cx))),
                )
                .child(
                    ui::button("edit-cancel", t("ui.cancel"), 3).on_click(cx.listener(
                        |v, _, _, cx| {
                            v.edit = None;
                            cx.notify()
                        },
                    )),
                )
        });
        let confirm = self.confirm_delete.as_ref().map(|(pi, paths)| {
            let side = self.panes[*pi].label();
            let text = if paths.len() == 1 {
                tf(
                    "sftp.confirm_delete",
                    &[("p", &paths[0]), ("side", &side)],
                )
            } else {
                tf(
                    "sftp.confirm_delete_n",
                    &[("n", &paths.len()), ("side", &side)],
                )
            };
            div()
                .mt_2()
                .p_2()
                .rounded(px(R_MD))
                .bg(c(th.error).opacity(0.1))
                .border_1()
                .border_color(c(th.error).opacity(0.5))
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(fs_small()))
                .text_color(c(th.text))
                .child(icon(IconName::TriangleAlert, ui::IC_SM, c(th.error)))
                .child(div().flex_1().child(text))
                .child(
                    ui::button("del-ok", t("sftp.delete"), 2)
                        .on_click(cx.listener(|v, _, _, cx| v.delete_confirmed(cx))),
                )
                .child(
                    ui::button("del-cancel", t("ui.cancel"), 3).on_click(cx.listener(
                        |v, _, _, cx| {
                            v.confirm_delete = None;
                            cx.notify()
                        },
                    )),
                )
        });
        if let Some(ed) = self.editor.clone() {
            let (pi, path, st, force, readonly) = (ed.pane, ed.path, ed.text, ed.force, ed.readonly);
            let heading = if readonly {
                t("sftp.view_title")
            } else {
                t("sftp.editor_title")
            };
            let ed = div()
                .flex_1()
                .min_h_0()
                .pt(px(crate::card_layout::CARD_PAD))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(fs_small()))
                        .text_color(c(th.text))
                        .child(icon(
                            if readonly {
                                IconName::Eye
                            } else {
                                IconName::FileText
                            },
                            ui::IC_SM,
                            c(th.accent),
                        ))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(c(th.subtext))
                                .child(if readonly {
                                    t("dir.view_title")
                                } else {
                                    t("dir.edit_title")
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(format!("{}:{path}", self.panes[pi].label())),
                        )
                        .when(!readonly, |d| {
                            d.child(
                                ui::button(
                                    "ed-save",
                                    if force {
                                        t("sftp.edit_overwrite")
                                    } else {
                                        t("ui.save")
                                    },
                                    if force { 2 } else { 1 },
                                )
                                .on_click(cx.listener(|v, _, _, cx| v.save_editor(cx))),
                            )
                        })
                        .child(ui::button("ed-close", t("sftp.edit_close"), 3).on_click(
                            cx.listener(|v, _, _, cx| {
                                v.editor = None;
                                cx.notify()
                            }),
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .border_1()
                        .border_color(crate::theme::divider())
                        .rounded(px(ui::R_LG))
                        .overflow_hidden()
                        .child(Editor::new(&st).h_full().readonly(readonly)),
                );
            let status = div()
                .py_2()
                .text_size(px(fs_small()))
                .text_color(if self.error {
                    c(th.error)
                } else {
                    c(th.overlay1)
                })
                .child(self.status.clone());
            return self
                .frame()
                .child(self.page_head(heading, cx))
                .child(ed)
                .child(status);
        }
        let sync = self.sync_panel(cx);
        let both = self.panes[0].ready() && self.panes[1].ready();
        let arrow = |id: &'static str, ic: IconName, from: usize| {
            div()
                .id(id)
                .size(px(H_COMPACT))
                .rounded(px(R_MD))
                .flex()
                .items_center()
                .justify_center()
                .when(both, |d| {
                    d.cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg()))
                })
                .child(icon(
                    ic,
                    ui::IC_MD,
                    if both {
                        c(th.accent)
                    } else {
                        c(th.overlay0).opacity(0.5)
                    },
                ))
                .on_click(cx.listener(move |v, _, _, cx| v.transfer_from(from, cx)))
        };
        let mid = div()
            .w(px(H_COMPACT))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(arrow("sftp-to-right", IconName::ArrowRight, 0))
            .child(arrow("sftp-to-left", IconName::ArrowLeft, 1));
        let (left, right) = (self.pane(0, cx), self.pane(1, cx));
        let body = div()
            .flex_1()
            .min_h_0()
            .pt(px(crate::card_layout::CARD_PAD))
            .flex()
            .gap(px(ui::SP_1))
            .child(left)
            .child(mid)
            .child(right);
        let status = div()
            .py_2()
            .text_size(px(fs_small()))
            .text_color(if self.error {
                c(th.error)
            } else {
                c(th.overlay1)
            })
            .child(if self.status.is_empty() {
                t("sftp.dnd_hint").to_string()
            } else {
                format!("{} · {}", self.status, t("sftp.dnd_hint"))
            });
        let ctx = self
            .ctx_menu
            .as_ref()
            .map(|(_, m)| crate::ctxmenu_view::render(m, cx, Self::ctx_pick, Self::ctx_close));
        let ops = self.ops_menu_el(cx);
        let blank = self.blank_menu_el(cx);
        let conflict = self.conflict_bar(cx);
        self.frame()
            .child(self.page_head(t("ui.sftp_files"), cx))
            .children(ctx)
            .children(ops)
            .children(blank)
            .children(edit_bar)
            .children(confirm)
            .children(conflict)
            .children(sync)
            .child(body)
            .child(div().pt_3().flex_shrink_0().child(self.queue_view(cx)))
            .child(status.flex_shrink_0())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_copy_tree() {
        let d = std::env::temp_dir().join(format!("sftp-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/b/f.txt"), "x").unwrap();
        assert_eq!(sftp::copy_tree(&d.join("a"), &d.join("c")).unwrap(), 1);
        assert!(sftp::copy_tree(&d.join("a"), &d.join("a/b/inner")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn ent(name: &str) -> Entry {
        Entry {
            name: name.into(),
            is_dir: false,
            size: 1,
            mode: 0,
            mtime: 0,
        }
    }

    #[test]
    fn picked_names_follow_the_file_after_reorder() {
        let mut p = Pane::new(Src::Local);
        p.entries = vec![ent("b"), ent("a")];
        p.picked = vec!["a".into()];
        p.sel = Some(1);
        p.entries = vec![ent("a"), ent("b")];
        p.retain_picked();
        assert_eq!(p.selection().len(), 1);
        assert_eq!(p.selection()[0].name, "a");
        p.entries = vec![ent("b")];
        p.retain_picked();
        assert!(p.selection().is_empty());
    }
}
