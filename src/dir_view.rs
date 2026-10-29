//! 右侧「目录」卡片：浏览某个终端标签的当前目录。SSH 走已有的 SFTP，本地走 std::fs（后台线程）。
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Focusable, Window};
use gpui_component::input::{Editor, EditorState, InputEvent, InputState};
use russh_sftp::client::SftpSession;
use ssh_term_proto::ctxmenu::{self, archive_commands, archive_kind, targz_command, zip_command};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp::{self, Entry, Queue, Side};
use ssh_term_proto::sftp_follow::Follow;
use ssh_term_proto::ssh::{self, SshTarget};

use crate::filelist::{self, Sort};
use crate::theme::{c, cur, Appearance};
use crate::ui::{self, icon, IconName};

/// 一个可浏览的终端标签快照。`key` 是该标签第一个窗格的实体 id。
pub struct DirSnap {
    pub key: u64,
    pub label: String,
    pub remote: bool,
    pub ssh: Option<SshTarget>,
    pub cwd: Option<String>,
}

#[derive(Clone)]
pub struct DirChoice {
    pub key: u64,
    pub label: String,
    pub remote: bool,
}

/// 父视图每帧写进来的目标。卡片只读它，不自己去翻标签。
#[derive(Clone, Default)]
pub struct DirFeed {
    pub pick: u64,
    pub choices: Vec<DirChoice>,
    pub remote: bool,
    pub ssh: Option<SshTarget>,
    pub cwd: Option<String>,
    pub label: String,
    pub host_key: String,
}

/// 重名标签：第一份保持原名，后面的加 ` (2)`。选中项优先用用户点的，其次活动标签，最后第一项。
pub fn build_feed(snaps: &[DirSnap], picked: Option<u64>, active_key: Option<u64>) -> DirFeed {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for s in snaps { *counts.entry(s.label.as_str()).or_default() += 1; }
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut choices = Vec::with_capacity(snaps.len());
    for s in snaps {
        let n = seen.entry(s.label.as_str()).or_insert(0);
        *n += 1;
        let label = if counts[s.label.as_str()] > 1 && *n > 1 { format!("{} ({n})", s.label) } else { s.label.clone() };
        choices.push(DirChoice { key: s.key, label, remote: s.remote });
    }
    let pick = picked.filter(|k| choices.iter().any(|c| c.key == *k))
        .or_else(|| active_key.filter(|k| choices.iter().any(|c| c.key == *k)))
        .or_else(|| choices.first().map(|c| c.key))
        .unwrap_or(0);
    let snap = snaps.iter().find(|s| s.key == pick);
    DirFeed {
        pick,
        label: choices.iter().find(|c| c.key == pick).map(|c| c.label.clone()).unwrap_or_default(),
        choices,
        remote: snap.is_some_and(|s| s.remote),
        ssh: snap.and_then(|s| s.ssh.clone()),
        cwd: snap.and_then(|s| s.cwd.clone()),
        host_key: snap.and_then(|s| s.ssh.as_ref().map(|t| t.pool_key())).unwrap_or_default(),
    }
}

/// 本地家目录：HOME，其次 USERPROFILE，再不行用进程当前目录。
pub fn local_home() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .or_else(|_| std::env::current_dir().map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or_else(|_| "/".into())
}

pub enum DirEvent { Pick(u64) }

/// 目录卡片自己的剪贴板：路径、是否目录、来自本机还是哪台主机。
#[derive(Clone)]
struct Clip {
    remote: bool,
    host_key: String,
    ssh: Option<SshTarget>,
    path: String,
    name: String,
    is_dir: bool,
}

#[derive(Clone)]
enum NameKind { Mkdir, Rename { from: String } }

enum Sheet {
    Name { kind: NameKind, input: gpui::Entity<InputState> },
    Delete { path: String },
    /// `same`：源和目标是同一个路径，只能另存，不能覆盖。
    Conflict { clip: Clip, alt: String, same: bool },
    Cmd { title: String, cmd: String },
}

struct OpenDoc {
    path: String,
    ver: sftp::Version,
    text: gpui::Entity<EditorState>,
    readonly: bool,
    force: bool,
}

pub struct DirView {
    rt: tokio::runtime::Handle,
    prompts: ssh::Prompts,
    feed: Rc<RefCell<DirFeed>>,
    path_in: gpui::Entity<InputState>,
    search_in: gpui::Entity<InputState>,
    _subs: Vec<gpui::Subscription>,
    choices: Vec<DirChoice>,
    pick: Option<u64>,
    label: String,
    remote: bool,
    host_key: String,
    ssh: Option<SshTarget>,
    dir: String,
    entries: Vec<Entry>,
    loading: bool,
    error: Option<String>,
    path_err: Option<String>,
    gen: u64,
    token: u64,
    sort: Sort,
    back: Vec<String>,
    sel: Option<usize>,
    /// 每个目标里用户手动走过的目录。切换时优先于该目标的 cwd，避免沿用上一个目标的路径。
    browsed: std::collections::HashMap<u64, String>,
    follow: Follow,
    home: String,
    sess: Option<Arc<SftpSession>>,
    /// 持有连接，避免池里的弱引用在浏览期间被回收。读取发生在 `session`。
    conn: Option<Arc<ssh::Conn>>,
    connecting: bool,
    menu: Option<(gpui::Point<gpui::Pixels>, std::time::Instant)>,
    /// 文件右键：(位置, 打开时间, 条目下标；空白处为 None)。
    ops: Option<(gpui::Point<gpui::Pixels>, std::time::Instant, Option<usize>)>,
    sheet: Option<Sheet>,
    open: Option<OpenDoc>,
    clip: Option<Clip>,
    busy: bool,
    status: String,
    status_err: bool,
}

impl gpui::EventEmitter<DirEvent> for DirView {}

impl DirView {
    pub fn new(rt: tokio::runtime::Handle, prompts: ssh::Prompts, feed: Rc<RefCell<DirFeed>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("dir.path_ph")));
        let search_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("sftp.filter_ph")));
        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&search_in, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) { cx.notify(); }
        }));
        subs.push(cx.subscribe_in(&path_in, window, |v, _, ev: &InputEvent, w, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) { v.submit_path(w, cx); }
        }));
        Self {
            rt, prompts, feed, path_in, search_in, _subs: subs,
            choices: vec![], pick: None, label: String::new(), remote: false, host_key: String::new(), ssh: None,
            dir: String::new(), entries: vec![], loading: false, error: None, path_err: None,
            gen: 0, token: 0, sort: Sort::default(), back: vec![], sel: None, browsed: std::collections::HashMap::new(),
            follow: Follow::new(true), home: String::new(), sess: None, conn: None, connecting: false, menu: None,
            ops: None, sheet: None, open: None, clip: None, busy: false, status: String::new(), status_err: false,
        }
    }

    fn session(&self) -> Option<Arc<SftpSession>> {
        self.conn.as_ref()?;
        self.sess.clone()
    }

    fn path_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.path_in.read(cx).focus_handle(cx).is_focused(window)
    }

    /// 每帧对齐父视图写入的目标。身份没变就只跟着终端 cwd；变了才重连。
    /// 不在这里 `notify`：列表是异步回来才刷新，避免渲染期把自己再排进一帧。
    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let feed = self.feed.borrow().clone();
        if feed.choices.is_empty() {
            self.clear_idle();
            return;
        }
        let switched = self.pick != Some(feed.pick) || self.remote != feed.remote || self.host_key != feed.host_key;
        if switched {
            self.open = None;
            self.sheet = None;
            self.ops = None;
            self.choices = feed.choices.clone();
            self.label = feed.label.clone();
            self.retarget(&feed, window, cx);
            return;
        }
        self.choices = feed.choices;
        self.label = feed.label;
        if !self.loading && !self.connecting && !self.path_focused(window, cx) && !self.hold() {
            if let Some(dir) = filelist::follow_step(&mut self.follow, feed.cwd.as_deref(), &self.home, &self.dir, self.remote) {
                self.navigate(dir, true, false, window, cx);
            }
        }
        self.sync_path(false, window, cx);
    }

    fn clear_idle(&mut self) {
        let busy = self.pick.is_some() || self.sess.is_some() || self.connecting || self.loading;
        if busy {
            self.token = self.token.wrapping_add(1);
            self.gen = self.gen.wrapping_add(1);
            self.sess = None;
            self.conn = None;
            self.connecting = false;
            self.loading = false;
        }
        self.choices.clear();
        self.label.clear();
        self.pick = None;
        self.entries.clear();
        self.error = None;
        self.open = None;
        self.sheet = None;
        self.ops = None;
    }

    /// 先写好身份和要列出的目录，再发起连接 / 列表，最后才改输入框。
    /// 输入框的 `set_value` 会通知它自己，下一帧再进来时不能当成又换了一个目标。
    fn retarget(&mut self, feed: &DirFeed, window: &mut Window, cx: &mut Context<Self>) {
        // 同一个标签换成了另一台主机：旧的手动路径属于上一台，不能带过去。
        if self.pick == Some(feed.pick) && (self.remote != feed.remote || self.host_key != feed.host_key) {
            self.browsed.remove(&feed.pick);
        }
        self.pick = Some(feed.pick);
        self.remote = feed.remote;
        self.host_key = feed.host_key.clone();
        self.ssh = feed.ssh.clone();
        self.token = self.token.wrapping_add(1);
        self.gen = self.gen.wrapping_add(1);
        self.sess = None;
        self.conn = None;
        self.connecting = false;
        self.loading = false;
        self.error = None;
        self.path_err = None;
        self.entries.clear();
        self.back.clear();
        self.sel = None;
        self.follow.set(true);
        self.home.clear();
        let fallback = if feed.remote { "~".into() } else { local_home() };
        let want = filelist::path_for_target(&self.browsed, feed.pick, feed.cwd.as_deref(), &fallback).to_string();
        if feed.remote {
            self.dir = filelist::resolve_cwd(&want, "", true).unwrap_or_else(|| want.clone());
            self.connect(want, cx);
        } else {
            self.home = local_home();
            self.dir = filelist::resolve_cwd(&want, &self.home, false).unwrap_or_else(|| self.home.clone());
            self.list(cx);
        }
        // last 记终端 cwd，而不是列出来的目录：手动走过的路径才能留住，直到终端自己再移动。
        self.follow.last = self.terminal_dir().or_else(|| Some(self.dir.clone()));
        if !self.search_in.read(cx).value().is_empty() {
            self.search_in.update(cx, |s, cx| s.set_value("", window, cx));
        }
        // 渲染里 set_value 不会再排一帧。先写上新目录，帧结束后再强制写一次，把输入框标脏。
        self.sync_path(true, window, cx);
        cx.defer_in(window, |v, window, cx| v.sync_path(true, window, cx));
    }

    fn terminal_dir(&self) -> Option<String> {
        let cwd = self.feed.borrow().cwd.clone()?;
        filelist::resolve_cwd(&cwd, &self.home, self.remote)
    }

    fn remember_here(&mut self) {
        let Some(k) = self.pick else { return };
        if self.dir.is_empty() { return; }
        self.browsed.insert(k, self.dir.clone());
    }

    /// `force` 用于换目标：即使用户正在编辑上一个目标的路径，也要改成新目标的目录。
    fn sync_path(&mut self, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let focused = !force && self.path_focused(window, cx);
        if !focused { self.path_err = None; }
        let cur = self.path_in.read(cx).value();
        if let Some(next) = filelist::path_field_update(focused, cur.as_ref(), &self.dir) {
            let next = next.to_string();
            self.path_in.update(cx, |s, cx| s.set_value(next, window, cx));
        } else if force {
            let dir = self.dir.clone();
            self.path_in.update(cx, |s, cx| s.set_value(dir, window, cx));
        }
    }

    /// 连接回调里的结果。返回 false 表示这一代已经被更新的目标作废。
    fn take_connect(
        &mut self,
        token: u64,
        r: Result<Result<(Arc<SftpSession>, Arc<ssh::Conn>, String, String, String, Result<Vec<Entry>, String>), anyhow::Error>, flume::RecvError>,
    ) -> bool {
        if self.token != token { return false; }
        self.connecting = false;
        match r {
            Ok(Ok((s, conn, dir, home, _resolved, ls))) => {
                self.sess = Some(s);
                self.conn = Some(conn);
                self.home = home;
                self.dir = dir;
                if let Some(k) = self.pick {
                    if self.browsed.contains_key(&k) { self.browsed.insert(k, self.dir.clone()); }
                }
                self.follow.last = self.terminal_dir().or_else(|| Some(self.dir.clone()));
                match ls {
                    Ok(entries) => { self.entries = entries; self.error = None; }
                    Err(e) => { self.entries.clear(); self.error = Some(e); }
                }
            }
            Ok(Err(e)) => { self.entries.clear(); self.error = Some(format!("{e:#}")); }
            Err(_) => {}
        }
        true
    }

    fn connect(&mut self, mut want: String, cx: &mut Context<Self>) {
        if want.is_empty() { want = "~".into(); }
        let Some(target) = self.ssh.clone() else {
            self.connecting = false;
            self.loading = false;
            self.error = Some(t("sftp.failed_title").to_string());
            return;
        };
        self.token = self.token.wrapping_add(1);
        let token = self.token;
        self.connecting = true;
        self.loading = false;
        self.error = None;
        self.entries.clear();
        let prompts = self.prompts.clone();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let r = async move {
                let (conn, _) = ssh::connect_shared(&target, &prompts).await?;
                let s = conn.sftp().await?;
                let real_home = s.canonicalize(".".to_string()).await.unwrap_or_default();
                let home = filelist::tidy(&real_home, true);
                let resolved = filelist::resolve_cwd(&want, &home, true).unwrap_or_else(|| {
                    let t = filelist::tidy(&want, true);
                    if t.is_empty() { "/".into() } else { t }
                });
                let dir = match s.canonicalize(resolved.clone()).await {
                    Ok(p) => {
                        let t = filelist::tidy(&p, true);
                        if t.is_empty() { resolved.clone() } else { t }
                    }
                    Err(_) => resolved.clone(),
                };
                let dir = if dir.is_empty() { "/".into() } else { dir };
                let ls = match sftp::list_remote(&s, &dir, false).await {
                    Ok(v) => Ok(v),
                    Err(e) => Err(format!("{e:#}")),
                };
                Ok::<_, anyhow::Error>((Arc::new(s), conn, dir, home, resolved, ls))
            }.await;
            let _ = tx.send(r);
        });
        cx.spawn(async move |this, cx| {
            let mut pending = Some(rx.recv_async().await);
            // 窗口不在更新栈上时才能写输入框。写不了也要把列表接上，下一帧 apply 再对齐路径。
            let wrote = this.update_in(cx, |v, window, cx| {
                let Some(r) = pending.take() else { return; };
                if !v.take_connect(token, r) { return; }
                v.sync_path(false, window, cx);
                cx.notify();
            });
            if wrote.is_err() {
                if let Some(r) = pending.take() {
                    let _ = this.update(cx, |v, cx| if v.take_connect(token, r) { cx.notify(); });
                }
            }
        }).detach();
    }

    fn list(&mut self, cx: &mut Context<Self>) {
        if self.remote && self.session().is_none() {
            self.connect(self.dir.clone(), cx);
            return;
        }
        if !self.remote && self.dir.is_empty() { self.dir = local_home(); }
        self.gen = self.gen.wrapping_add(1);
        let gen = self.gen;
        let dir = self.dir.clone();
        self.loading = true;
        self.error = None;
        self.entries.clear();
        if self.remote {
            let Some(s) = self.sess.clone() else { self.loading = false; return; };
            let (tx, rx) = flume::bounded(1);
            self.rt.spawn(async move { let _ = tx.send(sftp::list_remote(&s, &dir, false).await); });
            cx.spawn(async move |this, cx| {
                if let Ok(r) = rx.recv_async().await {
                    let _ = this.update(cx, |v, cx| {
                        if v.gen != gen { return; }
                        v.loading = false;
                        match r {
                            Ok(ls) => { v.entries = ls; v.error = None; }
                            Err(e) => { v.entries.clear(); v.error = Some(format!("{e:#}")); }
                        }
                        cx.notify();
                    });
                }
            }).detach();
        } else {
            let path = std::path::PathBuf::from(dir);
            let task = cx.background_executor().spawn(async move { sftp::list_local(&path, false) });
            cx.spawn(async move |this, cx| {
                let r = task.await;
                let _ = this.update(cx, |v, cx| {
                    if v.gen != gen { return; }
                    v.loading = false;
                    match r {
                        Ok(ls) => { v.entries = ls; v.error = None; }
                        Err(e) => { v.entries.clear(); v.error = Some(format!("{e:#}")); }
                    }
                    cx.notify();
                });
            }).detach();
        }
    }

    fn navigate(&mut self, dir: String, push: bool, manual: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dir = filelist::tidy(&dir, self.remote);
        if dir.is_empty() { return; }
        self.path_err = None;
        if dir != self.dir {
            if push && !self.dir.is_empty() { self.back.push(self.dir.clone()); }
            self.dir = dir;
            self.sel = None;
        }
        if manual { self.remember_here(); }
        // 在点击回调里写输入框（绘制阶段之外），避免只靠渲染期 set_value。
        self.sync_path(false, window, cx);
        self.list(cx);
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prev) = self.back.pop() else { return; };
        self.dir = prev;
        self.sel = None;
        self.path_err = None;
        self.remember_here();
        self.sync_path(false, window, cx);
        self.list(cx);
    }

    fn open_entry(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(e) = self.entries.get(i) else { return; };
        if !e.is_dir { return; }
        let dir = filelist::child_path(&self.dir, &e.name, self.remote);
        self.navigate(dir, true, true, window, cx);
    }

    fn up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = filelist::parent_of(&self.dir, self.remote) else { return; };
        self.navigate(p, true, true, window, cx);
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.path_err = None;
        self.sync_path(false, window, cx);
        self.list(cx);
        cx.notify();
    }

    fn submit_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.path_in.read(cx).value().to_string();
        match filelist::resolve_cwd(raw.trim(), &self.home, self.remote) {
            Some(d) => {
                self.path_err = None;
                self.path_in.update(cx, |s, cx| s.set_value(d.clone(), window, cx));
                self.navigate(d, true, true, window, cx);
            }
            None => self.path_err = Some(t("dir.bad_path").to_string()),
        }
        cx.notify();
    }

    fn target_btn(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let th = cur();
        let ic = if self.remote { crate::file_icon::server_icon(ui::IC_MD) } else { crate::file_icon::local_icon(ui::IC_MD) };
        div().id("dir-target").flex_shrink_0().mx(px(ui::LIST_INSET)).mt(px(ui::SP_2)).mb(px(ui::SP_1))
            .h(px(ui::H_DEFAULT)).px(px(ui::ROW_PX)).border_1().border_color(crate::theme::divider()).rounded(px(ui::R_MD))
            .flex().items_center().gap_2().cursor_pointer()
            .hover(|d| d.bg(crate::theme::hover_bg()))
            .tooltip(ui::tip(t("dir.target")))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|v, ev: &gpui::MouseDownEvent, _, cx| {
                v.menu = if v.menu.is_some() { None } else { Some((ev.position, std::time::Instant::now())) };
                cx.stop_propagation();
                cx.notify();
            }))
            .child(ic)
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(self.label.clone()))
            .child(icon(IconName::ChevronDown, ui::IC_XS, c(th.overlay1)))
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> gpui::Div {
        let back_ok = !self.back.is_empty();
        let back = ui::icon_button("dir-back", IconName::ArrowLeft, ui::IconBtn::Md, t("dir.back"))
            .when(!back_ok, |d| d.opacity(0.4))
            .when(back_ok, |d| d.on_click(cx.listener(|v, _, window, cx| { v.go_back(window, cx); cx.notify(); })));
        let refresh = ui::icon_button("dir-refresh", IconName::RefreshCw, ui::IconBtn::Md, t("sftp.refresh"))
            .on_click(cx.listener(|v, _, window, cx| v.refresh(window, cx)));
        ui::toolbar()
            .child(back)
            .child(refresh)
            .child(div().flex_1().min_w_0().child(ui::text_field(&self.path_in, false)))
    }

    fn sort_head(&self, cx: &mut Context<Self>) -> AnyElement {
        let sort = self.sort;
        let width = filelist::cols_narrow().size.unwrap_or(76.);
        let name = filelist::sort_cell("dir-sort-name", t("sftp.col_name"), sort.key == filelist::SortKey::Name, sort.desc)
            .on_click(cx.listener(|v, _, _, cx| { v.sort.toggle(filelist::SortKey::Name); cx.notify(); }));
        let size = filelist::sort_cell("dir-sort-size", t("sftp.col_size"), sort.key == filelist::SortKey::Size, sort.desc)
            .on_click(cx.listener(|v, _, _, cx| { v.sort.toggle(filelist::SortKey::Size); cx.notify(); }));
        filelist::sort_header()
            .child(name.flex_1().min_w_0())
            .child(size.w(px(width)).flex_shrink_0())
            .into_any_element()
    }

    fn file_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().id("dir-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col()
            .on_mouse_down(gpui::MouseButton::Right, cx.listener(|v, ev: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                v.ops = Some((ev.position, std::time::Instant::now(), None));
                cx.notify();
            }));
        if self.connecting {
            return list.child(div().p(px(ui::PANEL_PX)).flex().items_center().gap_2().text_size(px(ui::fs_small())).text_color(c(th.subtext))
                .child(icon(IconName::LoaderCircle, ui::IC_SM, c(th.accent)))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(tf("sftp.connecting", &[("t", &self.label)]))))
                .child(filelist::listing_skeleton("dir-skel")).into_any_element();
        }
        if let Some(e) = &self.error {
            if self.entries.is_empty() && !self.loading {
                return list.child(ui::empty_state(IconName::CircleAlert, t("sftp.failed_title"), Some(e.clone().into()), vec![
                    ui::button("dir-retry", t("sftp.retry"), 1).on_click(cx.listener(|v, _, window, cx| v.refresh(window, cx))).into_any_element(),
                ], false)).into_any_element();
            }
        }
        let dark = th.appearance == Appearance::Dark;
        if filelist::parent_of(&self.dir, self.remote).is_some() {
            list = list.child(filelist::parent_row("dir-up", dark)
                .on_click(cx.listener(|v, ev: &gpui::ClickEvent, window, cx| if ev.click_count() >= 2 { v.up(window, cx); cx.notify(); })));
        }
        if self.loading {
            return list.child(div().p(px(ui::PANEL_PX)).flex().items_center().gap_2().text_size(px(ui::fs_small())).text_color(c(th.subtext))
                .child(icon(IconName::LoaderCircle, ui::IC_SM, c(th.accent))).child(t("sftp.listing")))
                .child(filelist::listing_skeleton("dir-skel-load")).into_any_element();
        }
        let q = self.search_in.read(cx).value().trim().to_lowercase();
        let cols = filelist::cols_narrow();
        let mut shown = 0usize;
        for i in filelist::sorted_indices(&self.entries, self.sort) {
            let e = &self.entries[i];
            if !q.is_empty() && !e.name.to_lowercase().contains(&q) { continue; }
            shown += 1;
            if shown > 2000 { break; }
            let selected = self.sel == Some(i);
            list = list.child(filelist::entry_row(("dir-row", i), e, selected, dark, cols)
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |v, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    v.sel = Some(i);
                    v.ops = Some((ev.position, std::time::Instant::now(), Some(i)));
                    cx.notify();
                }))
                .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, window, cx| {
                    if ev.click_count() >= 2 { v.open_entry(i, window, cx); } else { v.sel = Some(i); }
                    cx.notify();
                })));
        }
        if shown == 0 && !q.is_empty() {
            list = list.child(div().p(px(ui::PANEL_PX)).text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("sftp.filter_none")));
        }
        list.into_any_element()
    }

    fn menu_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pos, _) = self.menu?;
        let mut list = ui::popover().id("dir-target-menu").occlude().max_h(px(360.)).overflow_y_scroll().p(px(ui::SP_1)).flex().flex_col()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                if v.menu.as_ref().is_some_and(|m| m.1.elapsed() < std::time::Duration::from_millis(250)) { return; }
                v.menu = None;
                cx.notify();
            }));
        for (i, c) in self.choices.iter().enumerate() {
            let key = c.key;
            let current = self.pick == Some(key);
            let ic = if c.remote { IconName::Server } else { IconName::SquareTerminal };
            list = list.child(ui::menu_item(("dir-mi", i), Some(ic), c.label.clone(), true, false)
                .when(current, |d| d.bg(crate::theme::selected_bg()))
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.menu = None;
                    if v.pick != Some(key) { cx.emit(DirEvent::Pick(key)); }
                    cx.notify();
                })));
        }
        Some(gpui::deferred(gpui::anchored().position(pos).snap_to_window_with_margin(px(ui::EDGE_MARGIN)).child(crate::chrome::pop_in("dir-target-pop", list))).with_priority(3).into_any_element())
    }

    fn hold(&self) -> bool { self.busy || self.sheet.is_some() || self.open.is_some() || self.ops.is_some() }
    fn path_of(&self, name: &str) -> String { filelist::child_path(&self.dir, name, self.remote) }
    fn host_id(&self) -> String {
        if !self.remote { return "local".into(); }
        self.ssh.as_ref().map(|t| t.host.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| self.host_key.clone())
    }
    fn audit(&self, kind: &str, detail: &str) {
        let _ = crate::audit_log().append(kind, "user", &format!("host={} {detail}", self.host_id()));
    }
    fn note(&mut self, msg: String, err: bool) { self.status = msg; self.status_err = err; }

    fn begin_name(&mut self, kind: NameKind, initial: String, window: &mut Window, cx: &mut Context<Self>) {
        self.ops = None;
        let ph = match kind { NameKind::Mkdir => t("sftp.new_folder_name"), NameKind::Rename { .. } => t("sftp.new_name") };
        let st = cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(initial));
        st.update(cx, |s, cx| s.focus(window, cx));
        self._subs.push(cx.subscribe_in(&st, window, |v, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) { v.commit_name(cx); }
        }));
        self.sheet = Some(Sheet::Name { kind, input: st });
        cx.notify();
    }

    fn commit_name(&mut self, cx: &mut Context<Self>) {
        let (kind, val) = match &self.sheet {
            Some(Sheet::Name { kind, input }) => (kind.clone(), input.read(cx).value().trim().to_string()),
            _ => return,
        };
        if !valid_name(&val) { self.note(t("dir.bad_name").into(), true); cx.notify(); return; }
        self.sheet = None;
        let dest = self.path_of(&val);
        let (sess, side) = (self.session(), if self.remote { Side::Remote } else { Side::Local });
        match kind {
            NameKind::Mkdir => {
                self.audit("dir.mkdir", &format!("path={dest:?}"));
                let shown = dest.clone();
                self.spawn_ok(cx, async move { sftp::mkdir(sess.as_deref(), side, &dest).await }, tf("sftp.created", &[("p", &shown)]));
            }
            NameKind::Rename { from } => {
                let shown = ctxmenu::file_name(&from).to_string();
                self.audit("dir.rename", &format!("from={from:?} to={dest:?}"));
                let to = val.clone();
                self.spawn_ok(cx, async move { sftp::rename(sess.as_deref(), side, &from, &dest).await }, tf("sftp.renamed", &[("a", &shown), ("b", &to)]));
            }
        }
    }

    fn delete_path(&mut self, path: String, cx: &mut Context<Self>) {
        self.ops = None;
        self.sheet = Some(Sheet::Delete { path });
        cx.notify();
    }

    fn do_delete(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Delete { path }) = self.sheet.take() else { return };
        let (sess, side) = (self.session(), if self.remote { Side::Remote } else { Side::Local });
        self.audit("dir.delete", &format!("path={path:?}"));
        let p = path.clone();
        self.spawn_ok(cx, async move { sftp::remove(sess.as_deref(), side, &p).await }, tf("sftp.deleted", &[("p", &path)]));
    }

    fn open_doc(&mut self, path: String, readonly: bool, cx: &mut Context<Self>) {
        self.ops = None;
        if self.remote && self.session().is_none() { self.note(t("dir.not_connected").into(), true); cx.notify(); return; }
        self.busy = true;
        self.note(t("dir.busy").into(), false);
        let remote = self.remote;
        let sess = self.session();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let r = if remote {
                match sess {
                    Some(s) => sftp::read_text(&s, &path).await,
                    None => Err(anyhow::anyhow!("no SFTP session")),
                }
            } else {
                let p = path.clone();
                tokio::task::spawn_blocking(move || sftp::read_text_local(std::path::Path::new(&p))).await.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
            };
            let _ = tx.send((path, r));
        });
        cx.spawn(async move |this, cx| {
            let Ok((path, r)) = rx.recv_async().await else { return };
            let Some(window) = cx.update(|cx| cx.active_window()) else { return };
            let _ = window.update(cx, |_, window, cx| this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok((text, ver)) => {
                        let st = cx.new(|cx| EditorState::new(window, cx).line_number(true).default_value(text));
                        if !readonly { v.audit("dir.edit.open", &format!("path={path:?} bytes={}", ver.0)); }
                        v.open = Some(OpenDoc { path: path.clone(), ver, text: st, readonly, force: false });
                        v.note(tf("sftp.edit_opened", &[("p", &path)]), false);
                    }
                    Err(e) => v.note(format!("{e:#}"), true),
                }
                cx.notify();
            }));
        }).detach();
        cx.notify();
    }

    fn save_doc(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = &self.open else { return };
        if doc.readonly { return; }
        let (path, ver, st, force) = (doc.path.clone(), doc.ver, doc.text.clone(), doc.force);
        let text = st.read(cx).value().to_string();
        let n = text.len();
        let remote = self.remote;
        let sess = self.session();
        self.busy = true;
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let r = if remote {
                match sess {
                    Some(s) => sftp::write_text(&s, &path, &text, (!force).then_some(ver)).await,
                    None => Err(anyhow::anyhow!("no SFTP session")),
                }
            } else {
                let (p, text) = (path.clone(), text);
                tokio::task::spawn_blocking(move || sftp::write_text_local(std::path::Path::new(&p), &text, (!force).then_some(ver))).await.unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
            };
            let _ = tx.send((path, n, force, r));
        });
        cx.spawn(async move |this, cx| {
            let Ok((path, n, force, r)) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(nv) => {
                        if let Some(o) = v.open.as_mut() { if o.path == path { o.ver = nv; o.force = false; } }
                        v.audit("dir.edit.save", &format!("path={path:?} bytes={n} overwrite={force}"));
                        v.note(tf("sftp.edit_saved", &[("p", &path), ("n", &n)]), false);
                        v.list(cx);
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        if msg.contains("changed") { if let Some(o) = v.open.as_mut() { o.force = true; } }
                        v.note(msg, true);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn copy_entry(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(e) = self.entries.get(i).cloned() else { return };
        let name = e.name.clone();
        self.clip = Some(Clip { remote: self.remote, host_key: self.host_key.clone(), ssh: self.ssh.clone(), path: self.path_of(&e.name), name: e.name, is_dir: e.is_dir });
        self.ops = None;
        self.note(tf("dir.copied", &[("name", &name)]), false);
        cx.notify();
    }

    fn copy_path_text(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(e) = self.entries.get(i) else { return };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(self.path_of(&e.name)));
        self.ops = None;
        self.note(t("dir.copied_path").into(), false);
        cx.notify();
    }

    fn ask_cmd(&mut self, title: String, cmd: String, cx: &mut Context<Self>) {
        self.ops = None;
        self.sheet = Some(Sheet::Cmd { title, cmd });
        cx.notify();
    }

    fn run_cmd(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Cmd { cmd, .. }) = self.sheet.take() else { return };
        self.audit("dir.archive", &format!("cmd={cmd:?}"));
        if self.remote && self.conn.is_none() { self.note(t("dir.not_connected").into(), true); cx.notify(); return; }
        self.busy = true;
        self.note(t("dir.busy").into(), false);
        let remote = self.remote;
        let conn = self.conn.clone();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(run_shell(remote, conn, cmd).await); });
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(out) => { v.note(if out.is_empty() { t("dir.cmd_ok").into() } else { out }, false); v.list(cx); }
                    Err(e) => v.note(format!("{e:#}"), true),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    fn begin_paste(&mut self, cx: &mut Context<Self>) {
        let Some(clip) = self.clip.clone() else { return };
        if self.remote && self.session().is_none() { self.note(t("dir.not_connected").into(), true); cx.notify(); return; }
        self.ops = None;
        self.busy = true;
        self.note(t("dir.busy").into(), false);
        let remote = self.remote;
        let sess = self.session();
        let dir = self.dir.clone();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move {
            let dest = filelist::child_path(&dir, &clip.name, remote);
            let same = paths_equal(&clip.path, &dest);
            let exists = if same { true } else { path_exists(remote, sess.as_deref(), &dest).await };
            let alt = if exists || same { next_free(remote, sess.as_deref(), &dir, &clip.name).await } else { String::new() };
            let _ = tx.send((clip, exists, same, alt));
        });
        cx.spawn(async move |this, cx| {
            let Ok((clip, exists, same, alt)) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                if !exists && !same {
                    let name = clip.name.clone();
                    v.run_paste(clip, name, false, cx);
                } else { v.sheet = Some(Sheet::Conflict { clip, alt, same }); v.note(String::new(), false); cx.notify(); }
            });
        }).detach();
        cx.notify();
    }

    fn run_paste(&mut self, clip: Clip, name: String, overwrite: bool, cx: &mut Context<Self>) {
        self.sheet = None;
        let dest = self.path_of(&name);
        if clip.is_dir && nested(&clip.path, &dest) {
            self.busy = false;
            self.note(t("dir.same_place").into(), true);
            cx.notify();
            return;
        }
        self.audit("dir.paste", &format!("from={:?} to={dest:?} overwrite={overwrite}", clip.path));
        self.busy = true;
        self.note(t("dir.busy").into(), false);
        let job = PasteJob {
            src_remote: clip.remote, dst_remote: self.remote,
            same_host: clip.remote && self.remote && clip.host_key == self.host_key,
            overwrite, is_dir: clip.is_dir, src_path: clip.path, dest: dest.clone(),
            prompts: self.prompts.clone(), src_ssh: clip.ssh, dst_ssh: self.ssh.clone(),
            src_sess: if clip.remote && clip.host_key == self.host_key { self.session() } else { None },
            dst_sess: if self.remote { self.session() } else { None },
            conn: self.conn.clone(),
        };
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(do_paste(job).await); });
        let ok = tf("dir.pasted", &[("name", &name)]);
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(()) => { v.note(ok, false); v.list(cx); }
                    Err(e) => v.note(format!("{e:#}"), true),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    fn spawn_ok(&mut self, cx: &mut Context<Self>, fut: impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static, ok: String) {
        self.busy = true;
        self.note(t("dir.busy").into(), false);
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(fut.await); });
        cx.spawn(async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(()) => { v.note(ok, false); v.list(cx); }
                    Err(e) => v.note(format!("{e:#}"), true),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    fn ops_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pos, _, idx) = self.ops?;
        let entry = idx.and_then(|i| self.entries.get(i).cloned());
        let mut n = 0usize;
        let mut id = || { n += 1; ("dir-op", n) };
        let mut list = ui::popover().id("dir-ops").occlude().min_w(px(220.)).p(px(5.)).flex().flex_col()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                if v.ops.as_ref().is_some_and(|m| m.1.elapsed() < std::time::Duration::from_millis(250)) { return; }
                v.ops = None;
                cx.notify();
            }));
        let can_paste = self.clip.is_some();
        if let Some(e) = entry {
            let i = idx.unwrap();
            let path = self.path_of(&e.name);
            if !e.is_dir {
                list = list.child(ui::menu_item(id(), Some(IconName::Eye), t("dir.view"), true, false).on_click(cx.listener(move |v, _, _, cx| {
                    let p = v.entries.get(i).map(|e| v.path_of(&e.name)).unwrap_or_default();
                    v.open_doc(p, true, cx);
                })));
                list = list.child(ui::menu_item(id(), Some(IconName::FileText), t("sftp.edit"), true, false).on_click(cx.listener(move |v, _, _, cx| {
                    let p = v.entries.get(i).map(|e| v.path_of(&e.name)).unwrap_or_default();
                    v.open_doc(p, false, cx);
                })));
            }
            let from = path.clone();
            let old = e.name.clone();
            list = list.child(ui::menu_item(id(), Some(IconName::Pencil), t("sftp.rename"), true, false).on_click(cx.listener(move |v, _, window, cx| {
                v.begin_name(NameKind::Rename { from: from.clone() }, old.clone(), window, cx);
            })));
            list = list.child(ui::menu_item(id(), Some(IconName::Copy), t("dir.copy"), true, false).on_click(cx.listener(move |v, _, _, cx| v.copy_entry(i, cx))));
            list = list.child(ui::menu_item(id(), Some(IconName::ClipboardCopy), t("cm.copy_path"), true, false).on_click(cx.listener(move |v, _, _, cx| v.copy_path_text(i, cx))));
            list = list.child(ui::menu_sep());
            let zip = zip_command(&path);
            let tgz = targz_command(&path);
            list = list.child(ui::menu_item(id(), Some(IconName::FileArchive), t("cm.zip_dir"), true, false).on_click(cx.listener(move |v, _, _, cx| v.ask_cmd(t("cm.zip_dir").into(), zip.clone(), cx))));
            list = list.child(ui::menu_item(id(), Some(IconName::FileArchive), t("cm.targz_dir"), true, false).on_click(cx.listener(move |v, _, _, cx| v.ask_cmd(t("cm.targz_dir").into(), tgz.clone(), cx))));
            if let Some((k, _)) = archive_kind(&e.name) {
                let cmd = archive_commands(k, &path).extract_to;
                let title = tf("cm.extract_to", &[("dir", &format!("{}/", ctxmenu::stem_of(&e.name)))]);
                let label = title.clone();
                list = list.child(ui::menu_item(id(), Some(IconName::FolderOpen), label, true, false).on_click(cx.listener(move |v, _, _, cx| v.ask_cmd(title.clone(), cmd.clone(), cx))));
            }
            list = list.child(ui::menu_sep());
            let del = path;
            list = list.child(ui::menu_item(id(), Some(IconName::Trash), t("sftp.delete"), true, true).on_click(cx.listener(move |v, _, _, cx| v.delete_path(del.clone(), cx))));
        } else {
            list = list.child(ui::menu_item(id(), Some(IconName::FolderPlus), t("sftp.mkdir"), true, false).on_click(cx.listener(|v, _, window, cx| v.begin_name(NameKind::Mkdir, String::new(), window, cx))));
            list = list.child(ui::menu_item(id(), Some(IconName::ClipboardPaste), t("dir.paste"), can_paste, false).on_click(cx.listener(|v, _, _, cx| v.begin_paste(cx))));
            list = list.child(ui::menu_sep());
            list = list.child(ui::menu_item(id(), Some(IconName::RefreshCw), t("sftp.refresh"), true, false).on_click(cx.listener(|v, _, window, cx| { v.ops = None; v.refresh(window, cx); })));
        }
        Some(gpui::deferred(gpui::anchored().position(pos).snap_to_window_with_margin(px(ui::EDGE_MARGIN)).child(crate::chrome::pop_in("dir-ops-pop", list))).with_priority(4).into_any_element())
    }

    fn sheet_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let th = cur();
        let bar = |danger: bool| div().mx(px(ui::LIST_INSET)).mb(px(ui::SP_2)).p(px(ui::SP_2)).rounded(px(ui::R_MD)).border_1()
            .border_color(if danger { c(th.error).opacity(0.5) } else { c(th.accent).opacity(0.5) })
            .bg(if danger { c(th.error).opacity(0.08) } else { gpui::transparent_black() })
            .flex().flex_col().gap_2().text_size(px(ui::fs_small())).text_color(c(th.text))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        match self.sheet.as_ref()? {
            Sheet::Name { kind, input } => {
                let what = match kind { NameKind::Mkdir => t("sftp.mkdir").to_string(), NameKind::Rename { .. } => t("sftp.rename").to_string() };
                Some(bar(false).child(what).child(ui::text_field(input, false))
                    .child(div().flex().gap_2()
                        .child(ui::button("dir-name-ok", t("ui.save"), 1).on_click(cx.listener(|v, _, _, cx| v.commit_name(cx))))
                        .child(ui::button("dir-name-x", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _, cx| { v.sheet = None; cx.notify(); }))))
                    .into_any_element())
            }
            Sheet::Delete { path } => {
                Some(bar(true)
                    .child(tf("sftp.confirm_delete", &[("p", path), ("side", &self.label)]))
                    .child(div().flex().gap_2()
                        .child(ui::button("dir-del-ok", t("sftp.delete"), 2).on_click(cx.listener(|v, _, _, cx| v.do_delete(cx))))
                        .child(ui::button("dir-del-x", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _, cx| { v.sheet = None; cx.notify(); }))))
                    .into_any_element())
            }
            Sheet::Conflict { clip, alt, same } => {
                let msg = if *same { tf("dir.dup_here", &[("name", alt)]) } else { tf("dir.exists", &[("name", &clip.name)]) };
                let alt_label = tf("dir.paste_as", &[("name", alt)]);
                let (clip_ow, name_ow) = (clip.clone(), clip.name.clone());
                let (clip_alt, alt_name) = (clip.clone(), alt.clone());
                let mut row = div().flex().gap_2().flex_wrap();
                if !same { row = row.child(ui::button("dir-ow", t("dir.overwrite"), 2).on_click(cx.listener(move |v, _, _, cx| v.run_paste(clip_ow.clone(), name_ow.clone(), true, cx)))); }
                row = row.child(ui::button("dir-alt", alt_label, 1).on_click(cx.listener(move |v, _, _, cx| v.run_paste(clip_alt.clone(), alt_name.clone(), false, cx))))
                    .child(ui::button("dir-cxl", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _, cx| { v.sheet = None; cx.notify(); })));
                Some(bar(false).child(msg).child(row).into_any_element())
            }
            Sheet::Cmd { title, cmd } => {
                let cmd = cmd.clone();
                Some(bar(false).child(title.clone())
                    .child(div().whitespace_normal().text_color(c(th.subtext)).child(tf("dir.confirm_cmd", &[("cmd", &cmd)])))
                    .child(div().flex().gap_2()
                        .child(ui::button("dir-cmd-ok", t("dir.run"), 1).on_click(cx.listener(|v, _, _, cx| v.run_cmd(cx))))
                        .child(ui::button("dir-cmd-x", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _, cx| { v.sheet = None; cx.notify(); }))))
                    .into_any_element())
            }
        }
    }

    fn editor_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let (path, st, readonly, force) = self.open.as_ref().map(|o| (o.path.clone(), o.text.clone(), o.readonly, o.force)).unwrap();
        let title = if readonly { t("dir.view_title") } else { t("dir.edit_title") };
        let head = div().flex().items_center().gap_2().px(px(ui::PANEL_PX)).pt(px(ui::SP_2)).text_size(px(ui::fs_small())).text_color(c(th.text))
            .child(icon(IconName::FileText, ui::IC_SM, c(th.accent)))
            .child(div().flex_shrink_0().text_color(c(th.subtext)).child(title))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().child(path))
            .when(!readonly, |d| d.child(ui::button("dir-ed-save", if force { t("sftp.edit_overwrite") } else { t("ui.save") }, if force { 2 } else { 1 }).on_click(cx.listener(|v, _, _, cx| v.save_doc(cx)))))
            .child(ui::button("dir-ed-x", t("sftp.edit_close"), 0).on_click(cx.listener(|v, _, _, cx| { v.open = None; cx.notify(); })));
        let mut col = div().size_full().min_h_0().flex().flex_col().child(head)
            .child(div().flex_1().min_h_0().m(px(ui::SP_2)).border_1().border_color(crate::theme::divider()).rounded(px(ui::R_LG)).overflow_hidden()
                .child(Editor::new(&st).h_full().readonly(readonly)));
        if !self.status.is_empty() {
            col = col.child(div().px(px(ui::PANEL_PX)).py(px(ui::SP_1)).text_size(px(ui::fs_small())).text_color(if self.status_err { c(th.error) } else { c(th.subtext) }).child(self.status.clone()));
        }
        col.into_any_element()
    }
}

impl gpui::Render for DirView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply(window, cx);
        if self.choices.is_empty() {
            return div().size_full().min_h_0().flex().flex_col()
                .child(ui::empty_state(IconName::Folder, t("dir.empty_title"), Some(t("dir.empty_desc").into()), vec![], false))
                .into_any_element();
        }
        if self.open.is_some() { return self.editor_page(cx); }
        let th = cur();
        let show_summary = !self.connecting && !self.loading && !(self.error.is_some() && self.entries.is_empty());
        let hard_err = self.error.is_some() && self.entries.is_empty() && !self.connecting && !self.loading;
        let mut col = div().size_full().min_h_0().flex().flex_col().child(self.target_btn(cx)).child(self.toolbar(cx))
            .child(ui::field_row().py(px(ui::SP_2)).child(ui::search_field(&self.search_in, false)));
        if let Some(e) = &self.path_err {
            col = col.child(ui::notice(ui::Notice::Warn, e.clone()).flex_shrink_0().mb(px(ui::SP_1)));
        }
        if show_summary {
            let q = self.search_in.read(cx).value();
            let (n, m, bytes) = filelist::tally(&self.entries, q.as_ref());
            let size = filelist::human(bytes);
            col = col.child(div().h(px(ui::H_COMPACT)).px(px(ui::PANEL_PX)).flex_shrink_0().flex().items_center()
                .text_size(px(ui::fs_small())).text_color(c(th.subtext))
                .child(tf("dir.summary", &[("n", &n), ("m", &m), ("size", &size)])));
        }
        if !hard_err { col = col.child(self.sort_head(cx)); }
        col = col.child(self.file_list(cx));
        if let Some(sheet) = self.sheet_el(cx) { col = col.child(sheet); }
        if !self.status.is_empty() {
            col = col.child(div().px(px(ui::PANEL_PX)).py(px(ui::SP_1)).flex_shrink_0().text_size(px(ui::fs_small()))
                .text_color(if self.status_err { c(th.error) } else { c(th.subtext) }).child(self.status.clone()));
        }
        if let Some(menu) = self.menu_el(cx) { col = col.child(menu); }
        if let Some(menu) = self.ops_el(cx) { col = col.child(menu); }
        col.into_any_element()
    }
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains('/') && !s.contains('\\') && !s.contains('\0')
}

fn paths_equal(a: &str, b: &str) -> bool {
    a.trim_end_matches(['/', '\\']) == b.trim_end_matches(['/', '\\'])
}

/// `child` is `parent` or a path inside it.
fn nested(parent: &str, child: &str) -> bool {
    let p = parent.trim_end_matches(['/', '\\']);
    let c = child.trim_end_matches(['/', '\\']);
    c == p || c.starts_with(&format!("{p}/")) || c.starts_with(&format!("{p}\\"))
}

async fn path_exists(remote: bool, sess: Option<&SftpSession>, path: &str) -> bool {
    if remote {
        match sess { Some(s) => s.metadata(path.to_string()).await.is_ok(), None => false }
    } else {
        std::path::Path::new(path).exists()
    }
}

async fn next_free(remote: bool, sess: Option<&SftpSession>, dir: &str, name: &str) -> String {
    for n in 2..100 {
        let alt = sftp::alt_name(name, n);
        if !path_exists(remote, sess, &filelist::child_path(dir, &alt, remote)).await { return alt; }
    }
    sftp::alt_name(name, 2)
}

async fn open_sftp(ssh: &SshTarget, prompts: &ssh::Prompts, existing: Option<Arc<SftpSession>>) -> anyhow::Result<Arc<SftpSession>> {
    if let Some(s) = existing { return Ok(s); }
    let conn = match ssh::pooled(ssh).await {
        Some(c) => c,
        None => ssh::connect_shared(ssh, prompts).await?.0,
    };
    Ok(Arc::new(conn.sftp().await?))
}

fn shell_text(code: u32, body: &[u8]) -> anyhow::Result<String> {
    let text = String::from_utf8_lossy(body).trim().to_string();
    if code == 0 { return Ok(text); }
    let head = tf("dir.cmd_failed", &[("code", &code)]);
    if text.is_empty() { anyhow::bail!("{head}") } else { anyhow::bail!("{head}\n{text}") }
}

async fn run_shell(remote: bool, conn: Option<Arc<ssh::Conn>>, cmd: String) -> anyhow::Result<String> {
    if remote {
        let Some(c) = conn else { anyhow::bail!("{}", t("dir.not_connected")) };
        let (code, body) = c.exec(&cmd, std::time::Duration::from_secs(600)).await?;
        return shell_text(code.unwrap_or(1), &body);
    }
    let out = if cfg!(windows) {
        tokio::process::Command::new("cmd").arg("/C").arg(&cmd).output().await?
    } else {
        tokio::process::Command::new("sh").arg("-c").arg(&cmd).output().await?
    };
    let mut body = out.stdout;
    body.extend_from_slice(&out.stderr);
    shell_text(out.status.code().unwrap_or(1) as u32, &body)
}

struct PasteJob {
    src_remote: bool,
    dst_remote: bool,
    same_host: bool,
    overwrite: bool,
    is_dir: bool,
    src_path: String,
    dest: String,
    prompts: ssh::Prompts,
    src_ssh: Option<SshTarget>,
    dst_ssh: Option<SshTarget>,
    src_sess: Option<Arc<SftpSession>>,
    dst_sess: Option<Arc<SftpSession>>,
    conn: Option<Arc<ssh::Conn>>,
}

async fn do_paste(j: PasteJob) -> anyhow::Result<()> {
    if j.is_dir && nested(&j.src_path, &j.dest) { anyhow::bail!("{}", t("dir.same_place")); }
    if !j.src_remote && !j.dst_remote {
        let (a, b, dest) = (std::path::PathBuf::from(&j.src_path), std::path::PathBuf::from(&j.dest), j.dest.clone());
        if j.overwrite { sftp::remove(None, Side::Local, &dest).await?; }
        tokio::task::spawn_blocking(move || sftp::copy_tree(&a, &b)).await?.map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(());
    }
    if j.same_host {
        let s = j.dst_sess.as_ref().ok_or_else(|| anyhow::anyhow!(t("dir.not_connected")))?;
        if j.overwrite { sftp::remove(Some(s), Side::Remote, &j.dest).await?; }
        let c = j.conn.ok_or_else(|| anyhow::anyhow!(t("dir.not_connected")))?;
        let cmd = format!("cp -a -- {} {}", ctxmenu::quote_path(&j.src_path), ctxmenu::quote_path(&j.dest));
        let (code, body) = c.exec(&cmd, std::time::Duration::from_secs(600)).await?;
        shell_text(code.unwrap_or(1), &body).map(|_| ())?;
        return Ok(());
    }
    if j.overwrite {
        if j.dst_remote {
            let ssh = j.dst_ssh.as_ref().ok_or_else(|| anyhow::anyhow!(t("dir.not_connected")))?;
            let s = open_sftp(ssh, &j.prompts, j.dst_sess.clone()).await?;
            sftp::remove(Some(&s), Side::Remote, &j.dest).await?;
        } else {
            sftp::remove(None, Side::Local, &j.dest).await?;
        }
    }
    let src_s = if j.src_remote {
        Some(open_sftp(j.src_ssh.as_ref().ok_or_else(|| anyhow::anyhow!(t("dir.not_connected")))?, &j.prompts, j.src_sess.clone()).await?)
    } else { None };
    let dst_s = if j.dst_remote {
        Some(open_sftp(j.dst_ssh.as_ref().ok_or_else(|| anyhow::anyhow!(t("dir.not_connected")))?, &j.prompts, j.dst_sess.clone()).await?)
    } else { None };
    if !j.src_remote && j.dst_remote {
        let q = Queue::default();
        q.push_upload_tree(dst_s.as_ref().unwrap(), std::path::Path::new(&j.src_path), &j.dest).await?;
        q.drain(dst_s.as_ref().unwrap()).await;
        if let Some(e) = q.failure_summary() { anyhow::bail!(e) }
        return Ok(());
    }
    if j.src_remote && !j.dst_remote {
        if let Some(p) = std::path::Path::new(&j.dest).parent() { std::fs::create_dir_all(p)?; }
        let q = Queue::default();
        q.push_download_tree(src_s.as_ref().unwrap(), &j.src_path, std::path::Path::new(&j.dest)).await?;
        q.drain(src_s.as_ref().unwrap()).await;
        if let Some(e) = q.failure_summary() { anyhow::bail!(e) }
        return Ok(());
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp_root = std::env::temp_dir().join(format!("shellax-dir-{stamp}"));
    let tmp = tmp_root.join(ctxmenu::file_name(&j.src_path));
    std::fs::create_dir_all(&tmp_root)?;
    let q1 = Queue::default();
    q1.push_download_tree(src_s.as_ref().unwrap(), &j.src_path, &tmp).await?;
    q1.drain(src_s.as_ref().unwrap()).await;
    if let Some(e) = q1.failure_summary() { let _ = std::fs::remove_dir_all(&tmp_root); anyhow::bail!(e) }
    let q2 = Queue::default();
    q2.push_upload_tree(dst_s.as_ref().unwrap(), &tmp, &j.dest).await?;
    q2.drain(dst_s.as_ref().unwrap()).await;
    let fail = q2.failure_summary();
    let _ = std::fs::remove_dir_all(&tmp_root);
    if let Some(e) = fail { anyhow::bail!(e) }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(key: u64, label: &str, remote: bool, cwd: &str) -> DirSnap {
        DirSnap { key, label: label.into(), remote, ssh: None, cwd: Some(cwd.into()) }
    }

    #[test]
    fn dedup_labels_and_sticky_pick() {
        let snaps = vec![snap(1, "Local · bash", false, "/a"), snap(2, "Local · bash", false, "/b"), snap(3, "web", true, "/c")];
        let f = build_feed(&snaps, Some(1), Some(3));
        assert_eq!(f.pick, 1);
        assert_eq!(f.choices[0].label, "Local · bash");
        assert_eq!(f.choices[1].label, "Local · bash (2)");
        assert_eq!(f.choices[2].label, "web");
        assert_eq!(f.cwd.as_deref(), Some("/a"));
        let f = build_feed(&snaps, Some(99), Some(2));
        assert_eq!(f.pick, 2);
        assert_eq!(f.cwd.as_deref(), Some("/b"));
        let f = build_feed(&snaps, None, None);
        assert_eq!(f.pick, 1);
    }

    #[test]
    fn follow_skips_until_terminal_moves() {
        let mut f = Follow::new(true);
        f.last = filelist::resolve_cwd("/home/u", "", true);
        assert_eq!(filelist::follow_step(&mut f, Some("/home/u"), "", "/tmp", true), None);
        assert_eq!(filelist::follow_step(&mut f, Some("/var"), "", "/tmp", true).as_deref(), Some("/var"));
    }

    #[test]
    fn name_and_nesting() {
        assert!(valid_name("a.txt"));
        assert!(!valid_name(""));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(".."));
        assert!(nested("/a/b", "/a/b"));
        assert!(nested("/a/b", "/a/b/c"));
        assert!(!nested("/a/b", "/a/bc"));
        assert!(paths_equal("/tmp/a/", "/tmp/a"));
    }
}
