//! Dual-pane SFTP browser (local | remote) with a resumable transfer queue.
//! Remote side reuses the pooled SSH connection of the focused tab (no second login).
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnimationExt, AnyElement, Context, Entity, ExternalPaths, FontWeight, Window};
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_component::Sizable;
use russh_sftp::client::SftpSession;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp::{self, Dir, Entry, JobState, Queue, Side};
use ssh_term_proto::ssh::{self, SshTarget};
use ssh_term_proto::sftp_mode::{Follow, Mode};

use crate::theme::{c, cur, Appearance};
use crate::ui::{self, icon, IconName};

#[derive(Clone)]
pub struct DragFile { pub side: Side, pub path: String, pub name: String }
pub struct DragPreview(String);
impl Render for DragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        div().px_2().py_1().rounded_md().bg(c(th.surface1)).text_size(px(12.)).text_color(c(th.text)).child(self.0.clone())
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum DirAction { Right, Down, Close }

#[derive(Clone, Copy, PartialEq)]
enum Edit { Mkdir, Rename, Chmod }

pub enum SftpEvent {
    Close,
    /// Insert a command (no Enter) into the session for this host (remote) or a local pane.
    Insert { remote: bool, command: String },
    /// Open a new tab on this host / locally and insert `cd -- dir`.
    CdTab { remote: bool, dir: String },
    /// Move this view to another display mode (PRD 5.21); the entity (state, queue) is kept.
    Move(Mode),
    /// Split or close the pane that contains this SFTP view.
    Pane(DirAction),
}
impl gpui::EventEmitter<SftpEvent> for SftpView {}

pub struct SftpView {
    pub target: SshTarget,
    rt: tokio::runtime::Handle,
    prompts: ssh::Prompts,
    sess: Option<Arc<SftpSession>>,
    /// Keeps the pooled SSH connection alive while the browser is open.
    conn: Option<Arc<ssh::Conn>>,
    connecting: bool,
    pub status: String,
    error: bool,
    local_dir: PathBuf,
    remote_dir: String,
    /// Bumped on each remote listing so a slow reply cannot overwrite a newer directory.
    remote_list_gen: u64,
    remote_loading: bool,
    local: Vec<Entry>,
    remote: Vec<Entry>,
    sel: Option<(Side, usize)>,
    hidden: bool,
    pub queue: Queue,
    edit: Option<(Edit, Side, String, Entity<InputState>)>,
    confirm_delete: Option<(Side, String)>,
    /// Built-in editor for a remote text file: (path, version when opened, editor, overwrite on next save).
    editor: Option<(String, sftp::Version, Entity<EditorState>, bool)>,
    ticking: bool,
    /// Directory to open first (sftp:// link path) instead of the remote home.
    pub initial_dir: Option<String>,
    _subs: Vec<gpui::Subscription>,
    /// Directory sync panel: Some when open.
    sync: Option<SyncUi>,
    /// Multi-selection (indices on the `sel` side; Ctrl toggles, Shift extends).
    multi: Vec<usize>,
    /// Right-click menu (PRD 5.20) and the side it was opened on.
    ctx_menu: Option<(Side, crate::ctxmenu_view::MenuState)>,
    /// Remote file to open in the editor once connected (terminal "open in built-in editor").
    pending_edit: Option<String>,
    /// Current display mode (PRD 5.21); set by the owner when the view is placed.
    pub mode: Mode,
    /// Follow a terminal's cwd (split mode): source pane, whether it is local, follow state.
    follow_src: Option<(gpui::WeakEntity<crate::termview::TermView>, bool)>,
    pub follow: Follow,
    remote_home: String,
    follow_task: bool,
    local_sort: PaneSort,
    remote_sort: PaneSort,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey { Name, Mtime, Size }
#[derive(Clone, Copy)]
struct PaneSort { key: SortKey, desc: bool }
impl Default for PaneSort { fn default() -> Self { Self { key: SortKey::Name, desc: false } } }

/// Sync panel state: mode, delete flag, computed plan, delete confirmation pending.
struct SyncUi { mode: ssh_term_proto::dirsync::Mode, delete: bool, plan: Option<Vec<ssh_term_proto::dirsync::Op>>, busy: bool, confirm_delete: bool }

impl SftpView {
    fn default_local_dir() -> PathBuf {
        for key in ["USERPROFILE", "HOME"] {
            if let Some(p) = std::env::var_os(key) {
                let p = PathBuf::from(p);
                if p.is_dir() { return p; }
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
    /// Like [`new`] but does not connect yet (set `initial_dir`, then call [`start`]).
    pub fn new_deferred(target: SshTarget, rt: tokio::runtime::Handle, prompts: ssh::Prompts, _w: &mut Window, _cx: &mut Context<Self>) -> Self {
        let local_dir = Self::default_local_dir();
        let mut me = Self { target, rt, prompts, sess: None, conn: None, connecting: false, status: String::new(), error: false, local_dir, remote_dir: String::new(),
            local: vec![], remote: vec![], sel: None, hidden: false, queue: Queue::default(), edit: None, confirm_delete: None, editor: None, ticking: false, initial_dir: None, _subs: vec![], sync: None, multi: vec![], ctx_menu: None, pending_edit: None, mode: Mode::Popup, follow_src: None, follow: Follow::new(false), remote_home: String::new(), follow_task: false, remote_list_gen: 0, remote_loading: false, local_sort: PaneSort::default(), remote_sort: PaneSort::default() };
        me.refresh_local();
        me
    }
    pub fn start(&mut self, cx: &mut Context<Self>) { self.connect(cx); }
    pub fn new(target: SshTarget, rt: tokio::runtime::Handle, prompts: ssh::Prompts, _w: &mut Window, cx: &mut Context<Self>) -> Self {
        let local_dir = Self::default_local_dir();
        let mut me = Self { target, rt, prompts, sess: None, conn: None, connecting: false, status: String::new(), error: false, local_dir, remote_dir: String::new(),
            local: vec![], remote: vec![], sel: None, hidden: false, queue: Queue::default(), edit: None, confirm_delete: None, editor: None, ticking: false, initial_dir: None, _subs: vec![], sync: None, multi: vec![], ctx_menu: None, pending_edit: None, mode: Mode::Popup, follow_src: None, follow: Follow::new(false), remote_home: String::new(), follow_task: false, remote_list_gen: 0, remote_loading: false, local_sort: PaneSort::default(), remote_sort: PaneSort::default() };
        me.refresh_local();
        me.connect(cx);
        me
    }
    pub fn local_dir(&self) -> String { self.local_dir.to_string_lossy().into_owned() }
    pub fn set_local_dir(&mut self, d: &str) { if !d.is_empty() && std::path::Path::new(d).is_dir() { self.local_dir = d.into(); self.refresh_local(); } }
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
                        let home = std::env::var("HOME").unwrap_or_default();
                        let cur = v.local_dir();
                        if let Some(d) = v.follow.step(cwd.as_deref(), &home, &cur) { v.set_local_dir(&d); v.set_status(tf("sftp.followed", &[("d", &d)]), false); cx.notify(); }
                    } else if v.sess.is_some() {
                        let (home, cur) = (v.remote_home.clone(), v.remote_dir.clone());
                        if let Some(d) = v.follow.step(cwd.as_deref(), &home, &cur) { v.remote_dir = d.clone(); v.sel = None; v.multi.clear(); v.refresh_remote(cx); v.set_status(tf("sftp.followed", &[("d", &d)]), false); cx.notify(); }
                    }
                    true
                });
                if !matches!(alive, Ok(true)) { break; }
            }).detach();
        }
        cx.notify();
    }
    /// Snapshot for layout persistence.
    pub fn save(&self) -> ssh_term_proto::session_store::PaneSave {
        ssh_term_proto::session_store::PaneSave::Sftp { target: self.target.clone(), local_dir: self.local_dir(), remote_dir: self.remote_dir.clone(), follow: self.follow.on }
    }
    fn pane_actions(&self, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let btn = |i: usize, ic: IconName, tip: &'static str, act: DirAction| div().id(("sftp-pane", i)).size(px(24.)).rounded(px(6.)).flex().items_center().justify_center().cursor_pointer()
            .hover(|d| d.bg(c(cur().surface1)))
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip).build(w, cx))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(SftpEvent::Pane(act))))
            .child(icon(ic, 13., c(th.overlay1)));
        div().flex().items_center().gap(px(2.)).pr_1()
            .child(btn(0, IconName::Columns2, t("ui.split_left_right"), DirAction::Right))
            .child(btn(1, IconName::Rows2, t("ui.split_top_bottom"), DirAction::Down))
            .child(btn(2, IconName::X, t("ui.close_pane"), DirAction::Close))
    }
    fn mode_bar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let mut bar = div().flex().items_center().gap_1();
        for (i, m) in Mode::ALL.into_iter().enumerate() {
            let on = self.mode == m;
            let (ic, key) = match m { Mode::Popup => (IconName::Maximize, "sftp.mode_popup"), Mode::Tab => (IconName::PanelTop, "sftp.mode_tab"), Mode::Split => (IconName::Columns2, "sftp.mode_split"), Mode::Window => (IconName::ExternalLink, "sftp.mode_window") };
            bar = bar.child(div().id(("sftp-mode", i)).h(px(24.)).px_2().rounded_md().flex().items_center().gap_1().cursor_pointer().text_size(px(11.))
                .when(on, |d| d.bg(c(th.accent).opacity(0.18)).text_color(c(th.accent))).when(!on, |d| d.text_color(c(th.subtext)).hover(|d| d.bg(c(cur().surface0))))
                .on_click(cx.listener(move |v, _, _, cx| if v.mode != m { cx.emit(SftpEvent::Move(m)) }))
                .child(icon(ic, 12., if on { c(th.accent) } else { c(th.overlay1) })).when(self.mode != Mode::Split, |d| d.child(t(key))));
        }
        if self.follow_src.is_some() {
            bar = bar.child(gpui_component::switch::Switch::new("sftp-follow").checked(self.follow.on).label(t("sftp.follow_cwd")).small()
                .on_click(cx.listener(|v, on: &bool, _, cx| { v.follow.set(*on); cx.notify() })));
        }
        bar
    }
    fn frame(&self) -> gpui::Div {
        let th = cur();
        if self.mode.embedded() { div().size_full().min_h_0().flex().flex_col().bg(c(th.base)).overflow_hidden() } else { ui::card(1100.).h(px(700.)) }
    }
    fn set_status(&mut self, s: String, err: bool) { self.status = s; self.error = err; }
    fn sync_preview(&mut self, cx: &mut Context<Self>) {
        let (Some(s), Some(su)) = (self.sess.clone(), self.sync.as_mut()) else { return };
        su.busy = true; su.plan = None; su.confirm_delete = false;
        let (mode, del, ld, rd) = (su.mode, su.delete, self.local_dir.clone(), self.remote_dir.clone());
        self.remote_op(cx, async move {
            let l = ssh_term_proto::dirsync::scan_local(&ld)?;
            let r = sftp::scan_remote(&s, &rd).await?;
            Ok(ssh_term_proto::dirsync::plan(&l, &r, mode, del))
        }, |v, plan, _| { if let Some(su) = v.sync.as_mut() { su.busy = false; v.status = tf("sync.previewed", &[("n", &plan.len())]); su.plan = Some(plan); } });
    }
    fn sync_run(&mut self, cx: &mut Context<Self>) {
        let (Some(s), Some(su)) = (self.sess.clone(), self.sync.as_mut()) else { return };
        let Some(plan) = su.plan.clone() else { return };
        let dels = plan.iter().filter(|o| o.is_delete()).count();
        if dels > 0 && !su.confirm_delete { su.confirm_delete = true; cx.notify(); return; }
        su.busy = true;
        self.start_worker(cx);
        let Some(su) = self.sync.as_ref() else { return };
        let (q, ld, rd, host) = (self.queue.clone(), self.local_dir.clone(), self.remote_dir.clone(), self.target.host.clone());
        let _ = crate::audit_log().append("sftp.sync", "user", &format!("host={host} local={:?} remote={rd:?} mode={:?} ops={} deletes={dels}", ld.display(), su.mode, plan.len()));
        self.remote_op(cx, async move { sftp::execute_sync(&s, &q, &ld, &rd, &plan).await }, |v, (q, d, c), cx| {
            v.sync = None;
            v.start_worker(cx);
            v.after_change(tf("sync.done", &[("q", &q), ("d", &d), ("c", &c)]), cx);
        });
    }
    fn sync_panel(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        use ssh_term_proto::dirsync::Mode;
        let su = self.sync.as_ref()?;
        let th = cur();
        let modes = vec![("up".to_string(), t("sync.up").to_string()), ("down".to_string(), t("sync.down").to_string()), ("both".to_string(), t("sync.both").to_string())];
        let sel = match su.mode { Mode::LocalToRemote => "up", Mode::RemoteToLocal => "down", Mode::TwoWay => "both" };
        let me = cx.entity().downgrade();
        let mut list = div().id("sync-plan").max_h(px(180.)).overflow_y_scroll().flex().flex_col().text_size(px(12.)).font_family("monospace");
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
        Some(div().mx_4().mt_2().p_3().rounded_md().border_1().border_color(c(th.accent).opacity(0.5)).flex().flex_col().gap_2().text_size(px(12.)).text_color(c(th.text))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(tf("sync.title", &[("l", &self.local_dir.display()), ("r", &self.remote_dir)])))
            .child(bar)
            .children(su.confirm_delete.then(|| div().flex().items_center().gap_2().text_color(c(th.error)).child(icon(IconName::TriangleAlert, 14., c(th.error))).child(tf("sync.delete_warn", &[("n", &dels)]))))
            .child(list))
    }
    fn refresh_local(&mut self) {
        if self.local_dir.as_os_str().is_empty() {
            self.local = windows_drives();
            return;
        }
        match sftp::list_local(&self.local_dir, self.hidden) { Ok(v) => self.local = v, Err(e) => { self.local.clear(); self.set_status(format!("{}: {e}", self.local_dir.display()), true) } }
    }
    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting { return; }
        self.connecting = true;
        self.set_status(tf("sftp.connecting", &[("t", &self.target.label())]), false);
        let (t, p) = (self.target.clone(), self.prompts.clone());
        let want = self.initial_dir.take();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let r = async {
                let (conn, _) = ssh::connect_shared(&t, &p).await?;
                let s = conn.sftp().await?;
                let real_home = s.canonicalize(".").await.unwrap_or_default();
                let home = match want.as_deref().map(|d| d.strip_prefix("~/").map(str::to_string).unwrap_or_else(|| d.trim_start_matches('~').to_string())) {
                    Some(d) if !d.is_empty() => match s.canonicalize(d.clone()).await { Ok(p) => p, Err(_) => s.canonicalize(".").await? },
                    _ => s.canonicalize(".").await?,
                };
                let ls = sftp::list_remote(&s, &home, false).await?;
                Ok::<_, anyhow::Error>((Arc::new(s), home, ls, conn, real_home))
            }.await;
            let _ = tx.send(r);
        });
        cx.spawn(async move |this, cx| {
            let r = rx.recv_async().await;
            let _ = this.update(cx, |v, cx| {
                v.connecting = false;
                match r {
                    Ok(Ok((s, home, ls, conn, rh))) => { v.remote_home = rh; v.sess = Some(s); v.conn = Some(conn); v.remote_dir = home; v.remote = ls; v.set_status(tf("sftp.connected", &[("t", &v.target.label())]), false); }
                    Ok(Err(e)) => v.set_status(tf("sftp.connect_failed", &[("e", &format!("{e:#}"))]), true),
                    Err(_) => {}
                }
                cx.notify();
            });
        }).detach();
    }
    fn refresh_remote(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.sess.clone() else { return };
        self.remote_list_gen = self.remote_list_gen.wrapping_add(1);
        let gen = self.remote_list_gen;
        let (dir, hidden) = (self.remote_dir.clone(), self.hidden);
        self.remote_loading = true;
        self.remote.clear();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(sftp::list_remote(&s, &dir, hidden).await); });
        cx.spawn(async move |this, cx| {
            if let Ok(r) = rx.recv_async().await {
                let _ = this.update(cx, |v, cx| {
                    if v.remote_list_gen != gen { return; }
                    v.remote_loading = false;
                    match r {
                        Ok(ls) => v.remote = ls,
                        Err(e) => v.set_status(format!("{e:#}"), true),
                    }
                    cx.notify();
                });
            }
        }).detach();
    }
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
        self.refresh_local();
        self.refresh_remote(cx);
    }
    fn path_of(&self, side: Side, name: &str) -> String {
        match side { Side::Local => self.local_dir.join(name).to_string_lossy().into_owned(), Side::Remote => sftp::join(&self.remote_dir, name) }
    }
    fn open(&mut self, side: Side, i: usize, cx: &mut Context<Self>) {
        self.sel = None;
        match side {
            Side::Local => {
                if let Some(e) = self.local.get(i).filter(|e| e.is_dir).cloned() {
                    if self.local_dir.as_os_str().is_empty() {
                        let letter = e.name.trim_end_matches(':');
                        self.local_dir = PathBuf::from(format!("{letter}:\\"));
                    } else {
                        self.local_dir = self.local_dir.join(&e.name);
                    }
                    self.refresh_local();
                }
            }
            Side::Remote => { if let Some(e) = self.remote.get(i).filter(|e| e.is_dir) { self.remote_dir = sftp::join(&self.remote_dir, &e.name); self.refresh_remote(cx); } }
        }
        cx.notify();
    }
    fn up(&mut self, side: Side, cx: &mut Context<Self>) {
        self.sel = None;
        match side {
            Side::Local => {
                if self.local_dir.as_os_str().is_empty() { return; }
                if is_drive_root(&self.local_dir) { self.local_dir = PathBuf::new(); }
                else if let Some(p) = self.local_dir.parent() { self.local_dir = p.to_path_buf(); }
                self.refresh_local();
            }
            Side::Remote => { self.remote_dir = sftp::parent(&self.remote_dir); self.refresh_remote(cx); }
        }
        cx.notify();
    }
    fn goto_local(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.sel = None;
        self.multi.clear();
        self.local_dir = path;
        self.refresh_local();
        cx.notify();
    }
    fn goto_remote(&mut self, path: String, cx: &mut Context<Self>) {
        self.sel = None;
        self.multi.clear();
        self.remote_dir = path;
        self.refresh_remote(cx);
        cx.notify();
    }
    fn select(&mut self, side: Side, i: usize, m: gpui::Modifiers) {
        let same = self.sel.is_some_and(|(s, _)| s == side);
        if (m.control || m.platform) && same {
            if let Some(p) = self.multi.iter().position(|&x| x == i) { self.multi.remove(p); } else { self.multi.push(i); }
            self.sel = Some((side, i));
        } else if m.shift && same {
            let a = self.sel.map(|s| s.1).unwrap_or(i);
            self.multi = (a.min(i)..=a.max(i)).collect();
        } else { self.sel = Some((side, i)); self.multi = vec![i]; }
    }
    /// Selected entries on `side` (multi-selection, else the single selection).
    fn selection_on(&self, side: Side) -> Vec<Entry> {
        let list = match side { Side::Local => &self.local, Side::Remote => &self.remote };
        if self.sel.map(|s| s.0) != Some(side) { return vec![]; }
        let mut idx = self.multi.clone();
        if idx.is_empty() { if let Some((_, i)) = self.sel { idx.push(i); } }
        idx.sort(); idx.dedup();
        idx.into_iter().filter_map(|i| list.get(i).cloned()).collect()
    }
    fn open_ctx_menu(&mut self, side: Side, i: usize, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::{FileObj, Place, Target};
        // right-click outside the current selection selects just that entry
        let in_sel = self.sel.is_some_and(|(s, _)| s == side) && (self.multi.contains(&i) || self.sel == Some((side, i)));
        if !in_sel { self.sel = Some((side, i)); self.multi = vec![i]; }
        let files: Vec<FileObj> = self.selection_on(side).into_iter().map(|e| FileObj { path: self.path_of(side, &e.name), is_dir: e.is_dir, mode: e.mode }).collect();
        if files.is_empty() { return; }
        let remote = side == Side::Remote;
        let (place, host, tgt) = if remote { (Place::RemotePanel, self.target.host.clone(), Some(self.target.clone())) } else { (Place::LocalPanel, "local".to_string(), None) };
        let key = crate::ctxmenu_view::host_key(tgt.as_ref());
        self.ctx_menu = Some((side, crate::ctxmenu_view::build(Target::Files(files), place, &host, key, pos, None)));
        if let Some(rx) = crate::ctxmenu_view::ensure_probe(tgt) {
            cx.spawn(async move |this, cx| {
                let _ = rx.recv_async().await;
                let _ = this.update(cx, |v, cx| { if let Some((sd, m)) = &v.ctx_menu { let r = *sd == Side::Remote; let host = if r { v.target.host.clone() } else { "local".into() }; let nm = m.rebuilt(if r { Place::RemotePanel } else { Place::LocalPanel }, &host); v.ctx_menu = Some((*sd, nm)); cx.notify(); } });
            }).detach();
        }
        cx.notify();
    }
    fn ctx_pick(&mut self, i: usize, _w: &mut Window, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu::Act;
        let Some((side, m)) = self.ctx_menu.take() else { return };
        let Some(item) = m.menu.items.get(i).cloned() else { return };
        let remote = side == Side::Remote;
        let _ = crate::audit_log().append("ctxmenu.pick", "user", &format!("host={} panel={} item={} custom={}", if remote { self.target.host.as_str() } else { "local" }, if remote { "remote" } else { "local" }, item.id, item.custom));
        match item.act {
            Act::Insert(command) => cx.emit(SftpEvent::Insert { remote, command }),
            Act::CdNewTab(dir) => cx.emit(SftpEvent::CdTab { remote, dir }),
            Act::Copy(text) => cx.write_to_clipboard(gpui::ClipboardItem::new_string(text)),
            Act::OpenEditor(p) => self.edit_path(p, cx),
            Act::Download(paths) => for p in paths { self.download(p, cx) },
            Act::Upload(paths) => for p in paths { self.upload(PathBuf::from(p), cx) },
            Act::OpenUrl(u) => cx.open_url(&u),
            Act::Search(_) | Act::AiExplain(_) | Act::PasteInput(_) | Act::UseAiContext => {}
        }
        cx.notify();
    }
    fn ctx_close(&mut self, cx: &mut Context<Self>) { if self.ctx_menu.take().is_some() { cx.notify(); } }
    fn selected(&self) -> Option<(Side, Entry)> {
        let (side, i) = self.sel?;
        let e = match side { Side::Local => self.local.get(i), Side::Remote => self.remote.get(i) }?;
        Some((side, e.clone()))
    }

    // ---- transfers
    pub fn upload(&mut self, local: PathBuf, cx: &mut Context<Self>) {
        let Some(s) = self.sess.clone() else { return self.set_status(t("sftp.not_connected").into(), true) };
        let name = local.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let remote = sftp::join(&self.remote_dir, &name);
        let q = self.queue.clone();
        self.start_worker(cx);
        self.remote_op(cx, async move { q.push_upload_tree(&s, &local, &remote).await }, |v, n, _| v.set_status(tf("sftp.queued", &[("n", &n)]), false));
    }
    pub fn download(&mut self, remote: String, cx: &mut Context<Self>) {
        let Some(s) = self.sess.clone() else { return self.set_status(t("sftp.not_connected").into(), true) };
        let local = self.local_dir.join(remote.rsplit('/').next().unwrap_or("download"));
        let q = self.queue.clone();
        self.start_worker(cx);
        self.remote_op(cx, async move { q.push_download_tree(&s, &remote, &local).await }, |v, n, _| v.set_status(tf("sftp.queued", &[("n", &n)]), false));
    }
    fn start_worker(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.sess.clone() { self.queue.start(&self.rt, s, || {}); }
        if self.ticking { return; }
        self.ticking = true;
        // repaint while jobs are active; refresh listings when they finish
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
            let Ok(busy) = this.update(cx, |v, cx| {
                let busy = v.queue.jobs.lock().unwrap().iter().any(|j| matches!(j.state(), JobState::Queued | JobState::Running));
                if !busy { v.ticking = false; v.refresh_local(); v.refresh_remote(cx); }
                cx.notify();
                busy
            }) else { break };
            if !busy { break; }
        }).detach();
    }
    fn transfer_selected(&mut self, cx: &mut Context<Self>) {
        match self.selected() {
            Some((Side::Local, e)) => self.upload(self.local_dir.join(&e.name), cx),
            Some((Side::Remote, e)) => self.download(sftp::join(&self.remote_dir, &e.name), cx),
            None => self.set_status(t("sftp.select_first").into(), true),
        }
        cx.notify();
    }

    // ---- file operations
    fn begin_edit(&mut self, kind: Edit, window: &mut Window, cx: &mut Context<Self>) {
        let (side, name, init) = match (kind, self.selected()) {
            (Edit::Mkdir, sel) => (sel.map(|s| s.0).unwrap_or(Side::Remote), String::new(), String::new()),
            (Edit::Rename, Some((side, e))) => (side, e.name.clone(), e.name.clone()),
            (Edit::Chmod, Some((side, e))) => (side, e.name.clone(), format!("{:o}", e.mode)),
            _ => { self.set_status(t("sftp.select_first").into(), true); cx.notify(); return }
        };
        let ph = match kind { Edit::Mkdir => t("sftp.new_folder_name"), Edit::Rename => t("sftp.new_name"), Edit::Chmod => "644" };
        let st = cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(init));
        st.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(cx.subscribe_in(&st, window, |v, _, ev: &InputEvent, _w, cx| if matches!(ev, InputEvent::PressEnter { .. }) { v.commit_edit(cx) }));
        self.edit = Some((kind, side, name, st));
        cx.notify();
    }
    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some((kind, side, name, st)) = self.edit.take() else { return };
        let val = st.read(cx).value().trim().to_string();
        if val.is_empty() { cx.notify(); return; }
        let s = self.sess.clone();
        let (a, b) = (self.path_of(side, &name), self.path_of(side, &val));
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
    /// Open the selected remote file in the built-in editor (UTF-8 text up to 2 MB).
    fn edit_selected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((Side::Remote, e)) = self.selected().filter(|(_, e)| !e.is_dir) else { self.set_status(t("sftp.edit_select").into(), true); cx.notify(); return };
        let path = self.path_of(Side::Remote, &e.name);
        self.edit_path(path, cx);
    }
    /// Open a remote text file in the built-in editor (waits for the connection if needed).
    pub fn edit_path(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(s) = self.sess.clone() else { self.pending_edit = Some(path); cx.notify(); return };
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
                        let _ = crate::audit_log().append("sftp.edit.open", "user", &format!("host={} path={path:?} bytes={}", v.target.host, ver.0));
                        v.editor = Some((path.clone(), ver, st, false));
                        v.set_status(tf("sftp.edit_opened", &[("p", &path)]), false);
                    }
                    Err(e) => v.set_status(format!("{e:#}"), true),
                }
                cx.notify();
            }));
        }).detach();
    }
    fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some((path, ver, st, force)) = self.editor.clone() else { return };
        let Some(s) = self.sess.clone() else { return };
        let text = st.read(cx).value().to_string();
        let (p2, host) = (path.clone(), self.target.host.clone());
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(sftp::write_text(&s, &p2, &text, (!force).then_some(ver)).await.map(|v| (v, text.len()))); });
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok((nv, n)) => {
                        let _ = crate::audit_log().append("sftp.edit.save", "user", &format!("host={host} path={path:?} bytes={n} overwrite={force}"));
                        if let Some(ed) = v.editor.as_mut() { ed.1 = nv; ed.3 = false; }
                        v.after_change(tf("sftp.edit_saved", &[("p", &path), ("n", &n)]), cx);
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        if msg.contains("changed on the server") { if let Some(ed) = v.editor.as_mut() { ed.3 = true; } let _ = crate::audit_log().append("sftp.edit.conflict", "policy", &format!("host={host} path={path:?}")); }
                        v.set_status(msg, true);
                    }
                }
                cx.notify();
            });
        }).detach();
    }
    fn delete_confirmed(&mut self, cx: &mut Context<Self>) {
        let Some((side, path)) = self.confirm_delete.take() else { return };
        let s = self.sess.clone();
        let p2 = path.clone();
        self.remote_op(cx, async move { sftp::remove(s.as_deref(), side, &p2).await }, move |v, _, cx| v.after_change(tf("sftp.deleted", &[("p", &path)]), cx));
        self.sel = None;
        cx.notify();
    }
}

fn human(n: u64) -> String {
    if n < 1024 { format!("{n} B") } else if n < 1 << 20 { format!("{:.1} KB", n as f64 / 1024.0) } else if n < 1 << 30 { format!("{:.1} MB", n as f64 / 1048576.0) } else { format!("{:.1} GB", n as f64 / 1073741824.0) }
}

fn is_drive_root(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy().replace('/', "\\");
    let t = s.trim_end_matches('\\');
    t.len() == 2 && t.as_bytes().get(1) == Some(&b':')
}

fn windows_drives() -> Vec<Entry> {
    let mut v = Vec::new();
    for c in b'A'..=b'Z' {
        let root = format!("{}:\\", c as char);
        if std::path::Path::new(&root).exists() {
            v.push(Entry { name: format!("{}:", c as char), is_dir: true, size: 0, mode: 0, mtime: 0 });
        }
    }
    v
}

fn local_parts(path: &std::path::Path) -> Vec<(String, PathBuf)> {
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
            Component::RootDir => { cur.push(c); }
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
            .child(div().id(("skel-ico", i)).size(px(16.)).flex_shrink_0().rounded(px(4.)).bg(c(th.surface1)))
            .child(div().id(("skel-name", i)).h(px(12.)).w(px(w)).rounded(px(4.)).bg(c(th.surface1)))
            .child(div().flex_1())
            .child(div().id(("skel-meta", i)).h(px(10.)).w(px(64.)).rounded(px(4.)).bg(c(th.surface0))));
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

impl SftpView {
    fn sort_of(&self, side: Side) -> PaneSort {
        match side { Side::Local => self.local_sort, Side::Remote => self.remote_sort }
    }
    fn toggle_sort(&mut self, side: Side, key: SortKey, cx: &mut Context<Self>) {
        let slot = match side { Side::Local => &mut self.local_sort, Side::Remote => &mut self.remote_sort };
        if slot.key == key { slot.desc = !slot.desc; }
        else {
            *slot = PaneSort { key, desc: !matches!(key, SortKey::Name) };
        }
        cx.notify();
    }
    fn sort_head(&self, side: Side, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let sort = self.sort_of(side);
        let mark = |key: SortKey| if sort.key != key { String::new() } else if sort.desc { " ↓".into() } else { " ↑".into() };
        let cell = |id: (&'static str, usize), label: String, w: f32, key: SortKey| {
            div().id(id).h(px(28.)).w(px(w)).flex_shrink_0().px_1().flex().items_center().rounded(px(4.)).cursor_pointer()
                .text_size(px(12.)).text_color(if sort.key == key { c(th.accent) } else { c(th.overlay1) })
                .hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(move |v, _, _, cx| v.toggle_sort(side, key, cx)))
                .child(format!("{label}{}", mark(key)))
        };
        let sid: u8 = if side == Side::Local { 0 } else { 1 };
        let name_id: &'static str = if sid == 0 { "sort-ln" } else { "sort-rn" };
        div().h(px(28.)).px_3().flex().items_center().gap_2().bg(c(th.mantle)).border_b_1().border_color(c(th.surface0))
            .child(div().size(px(16.)).flex_shrink_0())
            .child(div().id((name_id, 0usize)).flex_1().min_w_0().h(px(28.)).px_1().flex().items_center().rounded(px(4.)).cursor_pointer()
                .text_size(px(12.)).text_color(if sort.key == SortKey::Name { c(th.accent) } else { c(th.overlay1) })
                .hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(move |v, _, _, cx| v.toggle_sort(side, SortKey::Name, cx)))
                .child(format!("{}{}", t("sftp.col_name"), mark(SortKey::Name))))
            .when(!compact, |d| d.child(div().w(px(88.)).flex_shrink_0().text_size(px(12.)).text_color(c(th.overlay1)).child(t("sftp.col_mode"))))
            .child(cell((if sid == 0 { "sort-lt" } else { "sort-rt" }, 1), t("sftp.col_mtime").into(), 132., SortKey::Mtime))
            .child(cell((if sid == 0 { "sort-ls" } else { "sort-rs" }, 2), t("sftp.col_size").into(), 76., SortKey::Size))
            .into_any_element()
    }
    fn pane(&self, side: Side, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let entries = match side {
            Side::Local => &self.local,
            Side::Remote => &self.remote,
        };
        let sid = if side == Side::Local { "l" } else { "r" };
        let compact = self.mode == Mode::Split;
        let mut list = div().id(if side == Side::Local { "sftp-local" } else { "sftp-remote" }).flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        let at_drives = side == Side::Local && self.local_dir.as_os_str().is_empty();
        if !at_drives {
            list = list.child(div().id((sid, usize::MAX)).h(px(32.)).px_3().flex().items_center().gap_2().text_size(px(13.)).text_color(c(th.subtext)).cursor_pointer()
                .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(move |v, _, _, cx| v.up(side, cx)))
                .child(icon(IconName::FolderUp, 16., c(th.overlay1))).child(".."));
        }
        if side == Side::Remote && self.remote_loading {
            list = list.child(remote_skeleton());
        } else if side == Side::Remote && self.sess.is_none() && self.connecting {
            // connecting: skeleton only, no Reconnect button (states are mutually exclusive)
            list = list.child(div().p_4().text_size(px(12.)).text_color(c(th.subtext)).child(t("sftp.connecting_short").to_string())).child(remote_skeleton());
        } else if side == Side::Remote && self.sess.is_none() {
            list = list.child(div().p_4().text_size(px(12.)).text_color(if self.error { c(th.error) } else { c(th.subtext) }).child(self.status.clone()))
                .child(div().px_4().child(ui::button("sftp-reconnect", t("sftp.reconnect"), 1).on_click(cx.listener(|v, _, _, cx| v.connect(cx)))));
        }
        let dark = th.appearance == Appearance::Dark;
        let order = sorted_indices(entries, self.sort_of(side));
        for i in order.into_iter().take(1000) {
            let e = &entries[i];
            let selected = self.sel.is_some_and(|(s, _)| s == side) && (self.multi.contains(&i) || self.sel == Some((side, i)));
            let path = self.path_of(side, &e.name);
            let drag = DragFile { side, path, name: e.name.clone() };
            let g = crate::filekind::glyph(&e.name, e.is_dir, e.mode, &crate::theme::scheme().ansi, dark, th.overlay1);
            let hidden = e.name.starts_with('.') && e.name != "." && e.name != "..";
            list = list.child(div().id((sid, i)).h(px(32.)).px_3().flex().items_center().gap_2().text_size(px(13.)).text_color(c(th.text)).cursor_pointer()
                .when(hidden, |d| d.opacity(0.72))
                .when(selected, |d| d.bg(c(th.accent).opacity(0.2))).when(!selected, |d| d.hover(|d| d.bg(c(cur().surface0))))
                .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _, cx| { if ev.click_count() >= 2 { v.open(side, i, cx) } else { v.select(side, i, ev.modifiers()); cx.notify() } }))
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| { v.open_ctx_menu(side, i, ev.position, cx); cx.stop_propagation(); }))
                .on_drag(drag, |d: &DragFile, _, _, cx| cx.new(|_| DragPreview(d.name.clone())))
                .child(icon(g.icon, 16., c(g.color)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.name.clone()))
                .when(!compact, |d| d.child(div().w(px(88.)).text_size(px(12.)).text_color(c(th.overlay0)).child(sftp::mode_string(e.mode))))
                .child(div().w(px(132.)).flex_shrink_0().text_size(px(12.)).text_color(c(th.overlay0)).child(fmt_mtime(e.mtime)))
                .child(div().w(px(76.)).flex_shrink_0().text_size(px(12.)).text_color(c(th.overlay0)).child(if e.is_dir { "—".to_string() } else { human(e.size) })));
        }
        div().flex_1().min_w_0().h_full().flex().flex_col().border_1().border_color(c(th.surface0)).rounded_lg().overflow_hidden()
            .drag_over::<DragFile>(move |st, d, _, _| if d.side != side { st.border_color(c(cur().accent)) } else { st })
            .drag_over::<ExternalPaths>(move |st, _, _, _| if side == Side::Remote { st.border_color(c(cur().accent)) } else { st })
            .on_drop(cx.listener(move |v, d: &DragFile, _, cx| {
                if d.side == side { return; }
                match side { Side::Remote => v.upload(PathBuf::from(&d.path), cx), Side::Local => v.download(d.path.clone(), cx) }
                cx.notify();
            }))
            .on_drop(cx.listener(move |v, p: &ExternalPaths, _, cx| { if side == Side::Remote { for f in p.paths() { v.upload(f.clone(), cx); } cx.notify(); } }))
            .child(self.path_bar(side, cx))
            .child(self.sort_head(side, compact, cx))
            .child(list)
            .into_any_element()
    }

    fn path_bar(&self, side: Side, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut bar = div().h(px(32.)).px_2().flex().items_center().gap(px(2.)).bg(c(th.mantle)).overflow_hidden();
        let crumb = |id: (&'static str, usize), label: String, last: bool| {
            div().id(id).h(px(24.)).px_1().flex_shrink_0().rounded(px(4.)).flex().items_center().cursor_pointer()
                .text_size(px(12.)).text_color(if last { c(th.text) } else { c(th.subtext) })
                .when(last, |d| d.font_weight(FontWeight::MEDIUM))
                .hover(|d| d.bg(c(cur().surface0)))
                .child(label)
        };
        match side {
            Side::Local => {
                bar = bar.child(icon(IconName::Monitor, 13., c(th.overlay1)));
                let drives = self.local_dir.as_os_str().is_empty();
                let go_drives = cx.listener(|v, _, _, cx| v.goto_local(PathBuf::new(), cx));
                bar = bar.child(crumb(("crumb-l", 0), t("sftp.this_pc").to_string(), drives).on_click(go_drives));
                if !drives {
                    let parts = local_parts(&self.local_dir);
                    for (i, (label, path)) in parts.iter().enumerate() {
                        let path = path.clone();
                        let last = i + 1 == parts.len();
                        bar = bar.child(icon(IconName::ChevronRight, 11., c(th.overlay0)))
                            .child(crumb(("crumb-l", i + 1), label.clone(), last).on_click(cx.listener(move |v, _, _, cx| v.goto_local(path.clone(), cx))));
                    }
                }
            }
            Side::Remote => {
                bar = bar.child(icon(IconName::Server, 13., c(th.overlay1)));
                let host = self.target.label();
                bar = bar.child(crumb(("crumb-r", 0), host, false).on_click(cx.listener(|v, _, _, cx| v.goto_remote("/".into(), cx))));
                let parts = remote_parts(&self.remote_dir);
                for (i, (label, path)) in parts.iter().enumerate() {
                    let path = path.clone();
                    let last = i + 1 == parts.len();
                    bar = bar.child(icon(IconName::ChevronRight, 11., c(th.overlay0)))
                        .child(crumb(("crumb-r", i + 1), label.clone(), last).on_click(cx.listener(move |v, _, _, cx| v.goto_remote(path.clone(), cx))));
                }
            }
        }
        bar.into_any_element()
    }

    fn queue_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let jobs = self.queue.jobs.lock().unwrap().clone();
        let mut col = div().id("sftp-queue").h(px(if self.mode == Mode::Split { 84. } else { 130. })).overflow_y_scroll().flex().flex_col().border_1().border_color(c(th.surface0)).rounded_lg();
        col = col.child(div().h(px(28.)).px_3().flex().items_center().gap_2().bg(c(th.mantle)).text_size(px(12.)).text_color(c(th.subtext))
            .child(tf("sftp.queue_title", &[("n", &jobs.len())])).child(div().flex_1())
            .child(ui::button("sftp-clear", t("sftp.clear_done"), 3).h(px(20.)).on_click(cx.listener(|v, _, _, cx| { v.queue.clear_done(); cx.notify() }))));
        if jobs.is_empty() { col = col.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("sftp.queue_empty"))); }
        for j in jobs.iter().rev() {
            let st = j.state();
            let (label, color) = match st { JobState::Queued => (t("sftp.st_queued"), c(th.overlay1)), JobState::Running => (t("sftp.st_running"), c(th.info)), JobState::Done => (t("sftp.st_done"), c(th.success)), JobState::Failed => (t("sftp.st_failed"), c(th.error)), JobState::Paused => (t("sftp.st_paused"), c(th.warning)) };
            let pct = j.progress();
            let done = j.done.load(std::sync::atomic::Ordering::Relaxed);
            let total = j.total.load(std::sync::atomic::Ordering::Relaxed);
            let resumed = j.resumed_from.load(std::sync::atomic::Ordering::Relaxed);
            let id = j.id;
            let btn = match st {
                JobState::Running | JobState::Queued => Some(ui::button(("job-p", id as usize), t("sftp.pause"), 3).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| { v.queue.pause(id); cx.notify() }))),
                JobState::Paused | JobState::Failed => Some(ui::button(("job-r", id as usize), t("sftp.resume"), 3).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| { v.queue.resume(id); v.start_worker(cx); cx.notify() }))),
                JobState::Done => None,
            };
            col = col.child(div().px_3().py(px(4.)).flex().items_center().gap_2().border_t_1().border_color(c(th.surface0)).text_size(px(12.)).text_color(c(th.text))
                .child(icon(if j.dir == Dir::Upload { IconName::Upload } else { IconName::Download }, 13., c(th.accent)))
                .child(div().w(px(200.)).overflow_hidden().child(j.name()))
                .child(div().flex_1().h(px(6.)).rounded_full().bg(c(th.surface0)).child(div().h_full().rounded_full().bg(color).w(gpui::relative(pct.clamp(0., 1.)))))
                .child(div().w(px(150.)).text_size(px(11.)).text_color(c(th.overlay1)).child(format!("{} / {}{}", human(done), human(total), if resumed > 0 { format!(" · {}", tf("sftp.resumed", &[("n", &human(resumed))])) } else { String::new() })))
                .child(div().w(px(60.)).child(ui::tag(label, color)))
                .children(btn)
                .when_some(j.error.lock().unwrap().clone(), |d, e| d.child(div().text_color(c(th.error)).text_size(px(11.)).child(e))));
        }
        col.into_any_element()
    }
}

impl Render for SftpView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        if self.sess.is_some() { if let Some(p) = self.pending_edit.take() { self.edit_path(p, cx); } }
        let th = cur();
        let act = |id: &'static str, ic: IconName, label: &str| ui::button(id, label.to_string(), 0).child(icon(ic, 13., c(cur().subtext)));
        let toolbar = div().px_4().pt_3().flex().flex_wrap().items_center().gap_2()
            .child(act("sftp-up", IconName::Upload, t("sftp.upload")).on_click(cx.listener(|v, _, _, cx| v.transfer_selected(cx))))
            .child(act("sftp-down", IconName::Download, t("sftp.download")).on_click(cx.listener(|v, _, _, cx| v.transfer_selected(cx))))
            .child(act("sftp-mkdir", IconName::FolderPlus, t("sftp.mkdir")).on_click(cx.listener(|v, _, w, cx| v.begin_edit(Edit::Mkdir, w, cx))))
            .child(act("sftp-rename", IconName::Pencil, t("sftp.rename")).on_click(cx.listener(|v, _, w, cx| v.begin_edit(Edit::Rename, w, cx))))
            .child(act("sftp-chmod", IconName::Lock, t("sftp.chmod")).on_click(cx.listener(|v, _, w, cx| v.begin_edit(Edit::Chmod, w, cx))))
            .child(act("sftp-edit", IconName::FileText, t("sftp.edit")).on_click(cx.listener(|v, _, w, cx| v.edit_selected(w, cx))))
            .child(ui::button("sftp-del", t("sftp.delete"), 2).on_click(cx.listener(|v, _, _, cx| {
                match v.selected() { Some((side, e)) => v.confirm_delete = Some((side, v.path_of(side, &e.name))), None => v.set_status(t("sftp.select_first").into(), true) }
                cx.notify();
            })))
            .child(act("sftp-sync", IconName::RefreshCw, t("sync.button")).on_click(cx.listener(|v, _, _, cx| { v.sync = Some(SyncUi { mode: ssh_term_proto::dirsync::Mode::LocalToRemote, delete: false, plan: None, busy: false, confirm_delete: false }); cx.notify() })))
            .child(div().flex_1())
            .child(ui::button("sftp-hidden", if self.hidden { t("sftp.hide_hidden") } else { t("sftp.show_hidden") }, 3).on_click(cx.listener(|v, _, _, cx| { v.hidden = !v.hidden; v.refresh_local(); v.refresh_remote(cx); cx.notify() })))
            .child(ui::button("sftp-refresh", t("sftp.refresh"), 0).on_click(cx.listener(|v, _, _, cx| { v.refresh_local(); v.refresh_remote(cx); cx.notify() })));
        let edit_bar = self.edit.as_ref().map(|(k, side, name, st)| {
            let what = match k { Edit::Mkdir => t("sftp.mkdir").to_string(), Edit::Rename => tf("sftp.rename_x", &[("x", name)]), Edit::Chmod => tf("sftp.chmod_x", &[("x", name)]) };
            let where_ = if *side == Side::Local { t("sftp.local") } else { t("sftp.remote") };
            div().mx_4().mt_2().p_2().rounded_md().border_1().border_color(c(th.accent).opacity(0.5)).flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                .child(format!("{what} ({where_})")).child(div().w(px(260.)).child(Input::new(st).small()))
                .child(ui::button("edit-ok", t("ui.save"), 1).on_click(cx.listener(|v, _, _, cx| v.commit_edit(cx))))
                .child(ui::button("edit-cancel", t("ui.cancel"), 3).on_click(cx.listener(|v, _, _, cx| { v.edit = None; cx.notify() })))
        });
        let confirm = self.confirm_delete.as_ref().map(|(side, p)| {
            div().mx_4().mt_2().p_2().rounded_md().bg(c(th.error).opacity(0.1)).border_1().border_color(c(th.error).opacity(0.5)).flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                .child(icon(IconName::TriangleAlert, 14., c(th.error)))
                .child(div().flex_1().child(tf("sftp.confirm_delete", &[("p", p), ("side", &if *side == Side::Local { t("sftp.local") } else { t("sftp.remote") })])))
                .child(ui::button("del-ok", t("sftp.delete"), 2).on_click(cx.listener(|v, _, _, cx| v.delete_confirmed(cx))))
                .child(ui::button("del-cancel", t("ui.cancel"), 3).on_click(cx.listener(|v, _, _, cx| { v.confirm_delete = None; cx.notify() })))
        });
        if let Some((path, _, st, force)) = self.editor.clone() {
            let ed = div().flex_1().min_h_0().px_4().pt_3().flex().flex_col().gap_2()
                .child(div().flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
                    .child(icon(IconName::FileText, 14., c(th.accent)))
                    .child(div().flex_1().min_w_0().overflow_hidden().child(format!("{}:{path}", self.target.label())))
                    .child(ui::button("ed-save", if force { t("sftp.edit_overwrite") } else { t("ui.save") }, if force { 2 } else { 1 }).on_click(cx.listener(|v, _, _, cx| v.save_editor(cx))))
                    .child(ui::button("ed-close", t("sftp.edit_close"), 3).on_click(cx.listener(|v, _, _, cx| { v.editor = None; cx.notify() }))))
                .child(div().flex_1().min_h_0().border_1().border_color(c(th.surface0)).rounded_lg().overflow_hidden().child(Editor::new(&st).h_full()));
            let status = div().px_4().py_2().text_size(px(12.)).text_color(if self.error { c(th.error) } else { c(th.overlay1) }).child(self.status.clone());
            return self.frame()
                .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                    .child(icon(IconName::FolderSync, 16., c(th.accent)))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("sftp.editor_title")))
                    .child(div().flex_1()).child(self.mode_bar(cx)))
                .child(ed).child(status);
        }
        let sync = self.sync_panel(cx);
        let body = div().flex_1().min_h_0().px_4().pt_3().flex().gap_3()
            .child(self.pane(Side::Local, cx))
            .child(div().w(px(28.)).flex().flex_col().items_center().justify_center().gap_3()
                .child(div().id("arrow-up").cursor_pointer().on_click(cx.listener(|v, _, _, cx| v.transfer_selected(cx))).child(icon(IconName::ArrowRight, 16., c(th.accent))))
                .child(div().id("arrow-down").cursor_pointer().on_click(cx.listener(|v, _, _, cx| v.transfer_selected(cx))).child(icon(IconName::ArrowLeft, 16., c(th.accent)))))
            .child(self.pane(Side::Remote, cx));
        let status = div().px_4().py_2().text_size(px(12.)).text_color(if self.error { c(th.error) } else { c(th.overlay1) }).child(format!("{} · {}", self.status, t("sftp.dnd_hint")));
        let popup = !self.mode.embedded();
        self.frame()
            .child(div().h(px(44.)).flex_shrink_0().px_4().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(IconName::FolderSync, 16., c(th.accent)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.sftp_files")))
                .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child(self.target.label()))
                .child(div().flex_1())
                .when(matches!(self.mode, Mode::Tab | Mode::Split), |d| d.child(self.pane_actions(cx)))
                .child(self.mode_bar(cx))
                .when(popup, |d| d.child(div().id("sftp-close").size(px(28.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                    .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(|_, _, _, cx| cx.emit(SftpEvent::Close)))
                    .child(icon(IconName::X, 16., c(th.overlay1))))))
            .children(self.ctx_menu.as_ref().map(|(_, m)| crate::ctxmenu_view::render(m, cx, Self::ctx_pick, Self::ctx_close)))
            .child(toolbar).children(edit_bar).children(confirm).children(sync)
            .child(body)
            .child(div().px_4().pt_3().flex_shrink_0().child(self.queue_view(cx)))
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
        let v = vec![e("z.log", false, 10, 300), e("a", true, 0, 100), e("m.log", false, 99, 200), e("b", true, 0, 400)];
        let names = |s: PaneSort| sorted_indices(&v, s).into_iter().map(|i| v[i].name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(PaneSort { key: SortKey::Name, desc: false }), ["a", "b", "m.log", "z.log"]);
        assert_eq!(names(PaneSort { key: SortKey::Mtime, desc: true }), ["b", "a", "z.log", "m.log"]);
        assert_eq!(names(PaneSort { key: SortKey::Size, desc: true }), ["a", "b", "m.log", "z.log"]);
    }
}
