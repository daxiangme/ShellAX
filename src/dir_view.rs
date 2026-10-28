//! 右侧「目录」卡片：浏览某个终端标签的当前目录。SSH 走已有的 SFTP，本地走 std::fs（后台线程）。
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Focusable, Window};
use gpui_component::input::{InputEvent, InputState};
use russh_sftp::client::SftpSession;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp::{self, Entry};
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
    follow: Follow,
    home: String,
    sess: Option<Arc<SftpSession>>,
    /// 持有连接，避免池里的弱引用在浏览期间被回收。读取发生在 `session`。
    conn: Option<Arc<ssh::Conn>>,
    connecting: bool,
    menu: Option<(gpui::Point<gpui::Pixels>, std::time::Instant)>,
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
            gen: 0, token: 0, sort: Sort::default(), back: vec![], sel: None,
            follow: Follow::new(true), home: String::new(), sess: None, conn: None, connecting: false, menu: None,
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
            self.choices = feed.choices.clone();
            self.label = feed.label.clone();
            self.retarget(&feed, window, cx);
            return;
        }
        self.choices = feed.choices;
        self.label = feed.label;
        if !self.loading && !self.connecting && !self.path_focused(window, cx) {
            if let Some(dir) = filelist::follow_step(&mut self.follow, feed.cwd.as_deref(), &self.home, &self.dir, self.remote) {
                self.navigate(dir, true, cx);
            }
        }
        self.sync_path(window, cx);
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
    }

    /// 先写好身份和 follow.last，再发起连接 / 列表，最后才改输入框。
    /// 输入框的 `set_value` 会通知它自己，下一帧再进来时不能当成又换了一个目标。
    fn retarget(&mut self, feed: &DirFeed, window: &mut Window, cx: &mut Context<Self>) {
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
        let want = feed.cwd.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| if feed.remote { "~".into() } else { local_home() });
        if feed.remote {
            if let Some(d) = filelist::resolve_cwd(&want, "", true) {
                self.dir = d.clone();
                self.follow.last = Some(d);
            } else {
                self.dir = want.clone();
                self.follow.last = None;
            }
            self.connect(want, cx);
        } else {
            self.home = local_home();
            let dir = filelist::resolve_cwd(&want, &self.home, false).unwrap_or_else(|| self.home.clone());
            self.dir = dir.clone();
            self.follow.last = Some(dir);
            self.list(cx);
        }
        if !self.search_in.read(cx).value().is_empty() {
            self.search_in.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.sync_path(window, cx);
    }

    fn sync_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.path_focused(window, cx) { return; }
        self.path_err = None;
        let cur = self.path_in.read(cx).value();
        if cur.as_ref() != self.dir {
            let dir = self.dir.clone();
            self.path_in.update(cx, |s, cx| s.set_value(dir, window, cx));
        }
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
            let r = rx.recv_async().await;
            let _ = this.update(cx, |v, cx| {
                if v.token != token { return; }
                v.connecting = false;
                match r {
                    Ok(Ok((s, conn, dir, home, resolved, ls))) => {
                        v.sess = Some(s);
                        v.conn = Some(conn);
                        v.home = home;
                        v.dir = dir;
                        v.follow.last = Some(if resolved.is_empty() { v.dir.clone() } else { resolved });
                        match ls {
                            Ok(entries) => { v.entries = entries; v.error = None; }
                            Err(e) => { v.entries.clear(); v.error = Some(e); }
                        }
                    }
                    Ok(Err(e)) => { v.entries.clear(); v.error = Some(format!("{e:#}")); }
                    Err(_) => {}
                }
                cx.notify();
            });
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

    fn navigate(&mut self, dir: String, push: bool, cx: &mut Context<Self>) {
        let dir = filelist::tidy(&dir, self.remote);
        if dir.is_empty() { return; }
        self.path_err = None;
        if dir == self.dir {
            self.list(cx);
            return;
        }
        if push && !self.dir.is_empty() { self.back.push(self.dir.clone()); }
        self.dir = dir;
        self.sel = None;
        self.list(cx);
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        let Some(prev) = self.back.pop() else { return; };
        self.dir = prev;
        self.sel = None;
        self.path_err = None;
        self.list(cx);
    }

    fn open_entry(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(e) = self.entries.get(i) else { return; };
        if !e.is_dir { return; }
        let dir = filelist::child_path(&self.dir, &e.name, self.remote);
        self.navigate(dir, true, cx);
    }

    fn up(&mut self, cx: &mut Context<Self>) {
        let Some(p) = filelist::parent_of(&self.dir, self.remote) else { return; };
        self.navigate(p, true, cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.path_err = None;
        self.list(cx);
        cx.notify();
    }

    fn submit_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.path_in.read(cx).value().to_string();
        match filelist::resolve_cwd(raw.trim(), &self.home, self.remote) {
            Some(d) => {
                self.path_err = None;
                self.path_in.update(cx, |s, cx| s.set_value(d.clone(), window, cx));
                self.navigate(d, true, cx);
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
            .when(back_ok, |d| d.on_click(cx.listener(|v, _, _, cx| { v.go_back(cx); cx.notify(); })));
        let refresh = ui::icon_button("dir-refresh", IconName::RefreshCw, ui::IconBtn::Md, t("sftp.refresh"))
            .on_click(cx.listener(|v, _, _, cx| v.refresh(cx)));
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
        let mut list = div().id("dir-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        if self.connecting {
            return list.child(div().p(px(ui::PANEL_PX)).flex().items_center().gap_2().text_size(px(ui::fs_small())).text_color(c(th.subtext))
                .child(icon(IconName::LoaderCircle, ui::IC_SM, c(th.accent)))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(tf("sftp.connecting", &[("t", &self.label)]))))
                .child(filelist::listing_skeleton("dir-skel")).into_any_element();
        }
        if let Some(e) = &self.error {
            if self.entries.is_empty() && !self.loading {
                return list.child(ui::empty_state(IconName::CircleAlert, t("sftp.failed_title"), Some(e.clone().into()), vec![
                    ui::button("dir-retry", t("sftp.retry"), 1).on_click(cx.listener(|v, _, _, cx| v.refresh(cx))).into_any_element(),
                ], false)).into_any_element();
            }
        }
        let dark = th.appearance == Appearance::Dark;
        if filelist::parent_of(&self.dir, self.remote).is_some() {
            list = list.child(filelist::parent_row("dir-up", dark)
                .on_click(cx.listener(|v, ev: &gpui::ClickEvent, _, cx| if ev.click_count() >= 2 { v.up(cx); cx.notify(); })));
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
                .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _, cx| {
                    if ev.click_count() >= 2 { v.open_entry(i, cx); } else { v.sel = Some(i); }
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
}

impl gpui::Render for DirView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply(window, cx);
        if self.choices.is_empty() {
            return div().size_full().min_h_0().flex().flex_col()
                .child(ui::empty_state(IconName::Folder, t("dir.empty_title"), Some(t("dir.empty_desc").into()), vec![], false))
                .into_any_element();
        }
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
        if let Some(menu) = self.menu_el(cx) { col = col.child(menu); }
        col.into_any_element()
    }
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
}
