//! Dual-pane SFTP browser (local | remote) with a resumable transfer queue.
//! Remote side reuses the pooled SSH connection of the focused tab (no second login).
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, ExternalPaths, FontWeight, Window};
use gpui_component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_component::Sizable;
use russh_sftp::client::SftpSession;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp::{self, Dir, Entry, JobState, Queue, Side};
use ssh_term_proto::ssh::{self, SshTarget};

use crate::theme::{c, cur};
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
enum Edit { Mkdir, Rename, Chmod }

pub enum SftpEvent { Close }
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
}

impl SftpView {
    /// Like [`new`] but does not connect yet (set `initial_dir`, then call [`start`]).
    pub fn new_deferred(target: SshTarget, rt: tokio::runtime::Handle, prompts: ssh::Prompts, _w: &mut Window, _cx: &mut Context<Self>) -> Self {
        let local_dir = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        let mut me = Self { target, rt, prompts, sess: None, conn: None, connecting: false, status: String::new(), error: false, local_dir, remote_dir: String::new(),
            local: vec![], remote: vec![], sel: None, hidden: false, queue: Queue::default(), edit: None, confirm_delete: None, editor: None, ticking: false, initial_dir: None, _subs: vec![] };
        me.refresh_local();
        me
    }
    pub fn start(&mut self, cx: &mut Context<Self>) { self.connect(cx); }
    pub fn new(target: SshTarget, rt: tokio::runtime::Handle, prompts: ssh::Prompts, _w: &mut Window, cx: &mut Context<Self>) -> Self {
        let local_dir = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        let mut me = Self { target, rt, prompts, sess: None, conn: None, connecting: false, status: String::new(), error: false, local_dir, remote_dir: String::new(),
            local: vec![], remote: vec![], sel: None, hidden: false, queue: Queue::default(), edit: None, confirm_delete: None, editor: None, ticking: false, initial_dir: None, _subs: vec![] };
        me.refresh_local();
        me.connect(cx);
        me
    }
    fn set_status(&mut self, s: String, err: bool) { self.status = s; self.error = err; }
    fn refresh_local(&mut self) {
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
                let home = match want.as_deref().map(|d| d.strip_prefix("~/").map(str::to_string).unwrap_or_else(|| d.trim_start_matches('~').to_string())) {
                    Some(d) if !d.is_empty() => match s.canonicalize(d.clone()).await { Ok(p) => p, Err(_) => s.canonicalize(".").await? },
                    _ => s.canonicalize(".").await?,
                };
                let ls = sftp::list_remote(&s, &home, false).await?;
                Ok::<_, anyhow::Error>((Arc::new(s), home, ls, conn))
            }.await;
            let _ = tx.send(r);
        });
        cx.spawn(async move |this, cx| {
            let r = rx.recv_async().await;
            let _ = this.update(cx, |v, cx| {
                v.connecting = false;
                match r {
                    Ok(Ok((s, home, ls, conn))) => { v.sess = Some(s); v.conn = Some(conn); v.remote_dir = home; v.remote = ls; v.set_status(tf("sftp.connected", &[("t", &v.target.label())]), false); }
                    Ok(Err(e)) => v.set_status(tf("sftp.connect_failed", &[("e", &format!("{e:#}"))]), true),
                    Err(_) => {}
                }
                cx.notify();
            });
        }).detach();
    }
    fn refresh_remote(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.sess.clone() else { return };
        let (dir, hidden) = (self.remote_dir.clone(), self.hidden);
        self.remote_op(cx, async move { sftp::list_remote(&s, &dir, hidden).await }, |v, ls, _| v.remote = ls);
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
            Side::Local => { if let Some(e) = self.local.get(i).filter(|e| e.is_dir) { self.local_dir = self.local_dir.join(&e.name); self.refresh_local(); } }
            Side::Remote => { if let Some(e) = self.remote.get(i).filter(|e| e.is_dir) { self.remote_dir = sftp::join(&self.remote_dir, &e.name); self.refresh_remote(cx); } }
        }
        cx.notify();
    }
    fn up(&mut self, side: Side, cx: &mut Context<Self>) {
        self.sel = None;
        match side {
            Side::Local => { if let Some(p) = self.local_dir.parent() { self.local_dir = p.to_path_buf(); self.refresh_local(); } }
            Side::Remote => { self.remote_dir = sftp::parent(&self.remote_dir); self.refresh_remote(cx); }
        }
        cx.notify();
    }
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
    fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((Side::Remote, e)) = self.selected().filter(|(_, e)| !e.is_dir) else { self.set_status(t("sftp.edit_select").into(), true); cx.notify(); return };
        let Some(s) = self.sess.clone() else { return };
        let path = self.path_of(Side::Remote, &e.name);
        let p2 = path.clone();
        let (tx, rx) = flume::bounded(1);
        self.rt.spawn(async move { let _ = tx.send(sftp::read_text(&s, &p2).await); });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(r) = rx.recv_async().await else { return };
            let _ = this.update_in(cx, |v, window, cx| {
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
            });
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

impl SftpView {
    fn pane(&self, side: Side, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let (title, entries) = match side {
            Side::Local => (tf("panels.local_pane", &[("path", &self.local_dir.display())]), &self.local),
            Side::Remote => (tf("panels.remote_pane", &[("target", &format!("{}:{}", self.target.label(), self.remote_dir))]), &self.remote),
        };
        let sid = if side == Side::Local { "l" } else { "r" };
        let mut list = div().id(if side == Side::Local { "sftp-local" } else { "sftp-remote" }).flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        list = list.child(div().id((sid, usize::MAX)).h(px(26.)).px_3().flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.subtext)).cursor_pointer()
            .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(move |v, _, _, cx| v.up(side, cx)))
            .child(icon(IconName::FolderUp, 14., c(th.overlay1))).child(".."));
        if side == Side::Remote && self.sess.is_none() {
            list = list.child(div().p_4().text_size(px(12.)).text_color(if self.error { c(th.error) } else { c(th.overlay1) }).child(if self.connecting { t("sftp.connecting_short").to_string() } else { self.status.clone() }))
                .child(div().px_4().child(ui::button("sftp-reconnect", t("sftp.reconnect"), 0).on_click(cx.listener(|v, _, _, cx| v.connect(cx)))));
        }
        for (i, e) in entries.iter().enumerate().take(1000) {
            let selected = self.sel == Some((side, i));
            let path = self.path_of(side, &e.name);
            let drag = DragFile { side, path, name: e.name.clone() };
            list = list.child(div().id((sid, i)).h(px(26.)).px_3().flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text)).cursor_pointer()
                .when(selected, |d| d.bg(c(th.accent).opacity(0.2))).when(!selected, |d| d.hover(|d| d.bg(c(cur().surface0))))
                .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _, cx| { if ev.click_count() >= 2 { v.open(side, i, cx) } else { v.sel = Some((side, i)); cx.notify() } }))
                .on_drag(drag, |d: &DragFile, _, _, cx| cx.new(|_| DragPreview(d.name.clone())))
                .child(icon(if e.is_dir { IconName::Folder } else { IconName::FileText }, 14., if e.is_dir { c(th.warning) } else { c(th.overlay1) }))
                .child(div().flex_1().min_w_0().overflow_hidden().child(e.name.clone()))
                .child(div().w(px(80.)).text_size(px(11.)).text_color(c(th.overlay0)).child(sftp::mode_string(e.mode)))
                .child(div().w(px(70.)).text_size(px(11.)).text_color(c(th.overlay0)).child(if e.is_dir { "—".to_string() } else { human(e.size) })));
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
            .child(div().h(px(30.)).px_3().flex().items_center().gap_2().bg(c(th.mantle)).text_size(px(12.)).text_color(c(th.subtext))
                .child(icon(if side == Side::Local { IconName::Monitor } else { IconName::Server }, 13., c(th.overlay1)))
                .child(div().flex_1().min_w_0().overflow_hidden().child(title)))
            .child(list)
            .into_any_element()
    }

    fn queue_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let jobs = self.queue.jobs.lock().unwrap().clone();
        let mut col = div().id("sftp-queue").h(px(130.)).overflow_y_scroll().flex().flex_col().border_1().border_color(c(th.surface0)).rounded_lg();
        col = col.child(div().h(px(26.)).px_3().flex().items_center().gap_2().bg(c(th.mantle)).text_size(px(11.5)).text_color(c(th.subtext))
            .child(tf("sftp.queue_title", &[("n", &jobs.len())])).child(div().flex_1())
            .child(ui::button("sftp-clear", t("sftp.clear_done"), 3).h(px(20.)).on_click(cx.listener(|v, _, _, cx| { v.queue.clear_done(); cx.notify() }))));
        if jobs.is_empty() { col = col.child(div().p_3().text_size(px(11.5)).text_color(c(th.overlay1)).child(t("sftp.queue_empty"))); }
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
            col = col.child(div().px_3().py(px(4.)).flex().items_center().gap_2().border_t_1().border_color(c(th.surface0)).text_size(px(11.5)).text_color(c(th.text))
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
        let th = cur();
        let act = |id: &'static str, ic: IconName, label: &str| ui::button(id, label.to_string(), 0).child(icon(ic, 13., c(cur().subtext)));
        let toolbar = div().px_4().pt_3().flex().items_center().gap_2()
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
            let status = div().px_4().py_2().text_size(px(11.5)).text_color(if self.error { c(th.error) } else { c(th.overlay1) }).child(self.status.clone());
            return ui::card(1100.).h(px(700.))
                .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                    .child(icon(IconName::FolderSync, 16., c(th.accent)))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("sftp.editor_title"))))
                .child(ed).child(status);
        }
        let body = div().flex_1().min_h_0().px_4().pt_3().flex().gap_3()
            .child(self.pane(Side::Local, cx))
            .child(div().w(px(28.)).flex().flex_col().items_center().justify_center().gap_3()
                .child(div().id("arrow-up").cursor_pointer().on_click(cx.listener(|v, _, _, cx| v.transfer_selected(cx))).child(icon(IconName::ArrowRight, 16., c(th.accent))))
                .child(div().id("arrow-down").cursor_pointer().on_click(cx.listener(|v, _, _, cx| v.transfer_selected(cx))).child(icon(IconName::ArrowLeft, 16., c(th.accent)))))
            .child(self.pane(Side::Remote, cx));
        let status = div().px_4().py_2().text_size(px(11.5)).text_color(if self.error { c(th.error) } else { c(th.overlay1) }).child(format!("{} · {}", self.status, t("sftp.dnd_hint")));
        ui::card(1100.).h(px(700.))
            .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(IconName::FolderSync, 16., c(th.accent)))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.sftp_files")))
                .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child(self.target.label()))
                .child(div().flex_1())
                .child(div().id("sftp-close").size(px(28.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                    .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(|_, _, _, cx| cx.emit(SftpEvent::Close)))
                    .child(icon(IconName::X, 16., c(th.overlay1)))))
            .child(toolbar).children(edit_bar).children(confirm)
            .child(body)
            .child(div().px_4().pt_3().child(self.queue_view(cx)))
            .child(status)
    }
}
