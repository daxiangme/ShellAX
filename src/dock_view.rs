//! Container engine tab content: containers grouped by compose project with actions,
//! inspect, images (pull / remove), volumes and networks. Logs and exec open their own tabs.
use std::future::Future;
use std::sync::Arc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FontWeight, SharedString, Window};
use gpui_component::input::{EditorState, InputEvent, InputState};
use ssh_term_proto::assets::DockerEndpoint;
use ssh_term_proto::dock::{self, Action, Container, Dock, Image, NetworkRow, VolumeRow};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh::{Prompts, SshTarget};

use crate::activity::{log_push, ExecLog};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

#[derive(Clone, Copy, PartialEq)]
pub enum Sub { Containers, Images, Volumes, Networks }

pub enum DockEvent { Logs(String, String), Exec(String) }

enum Ask { Act(String, String, Action), RemoveImage(String, String) }

pub struct DockView {
    pub ep: DockerEndpoint,
    pub title: String,
    production: bool,
    pub dock: Option<Arc<Dock>>,
    conn_err: Option<String>,
    rt: tokio::runtime::Handle,
    log: ExecLog,
    sub: Sub,
    containers: Vec<Container>,
    show_all: bool,
    images: Vec<Image>,
    volumes: Vec<VolumeRow>,
    networks: Vec<NetworkRow>,
    loading: bool,
    error: Option<String>,
    selected: Option<String>,
    inspect_ed: Entity<EditorState>,
    pending_inspect: Option<String>,
    pull_in: Entity<InputState>,
    pull_log: Vec<String>,
    pulling: bool,
    ask: Option<Ask>,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<DockEvent> for DockView {}

fn small(s: impl Into<SharedString>) -> gpui::Div { div().text_size(px(12.)).text_color(c(cur().subtext)).child(s.into()) }
fn mono(s: impl Into<SharedString>) -> gpui::Div { div().font_family("monospace").text_size(px(12.)).text_color(c(cur().text)).child(s.into()) }
fn ago(ts: i64) -> String { chrono::DateTime::from_timestamp(ts, 0).map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default() }

pub fn state_color(state: &str) -> gpui::Hsla {
    let th = cur();
    match state { "running" => c(th.success), "paused" | "restarting" => c(th.warning), "dead" => c(th.error), _ => c(th.overlay0) }
}

impl DockView {
    pub fn new(ep: DockerEndpoint, via: Result<Option<(SshTarget, Prompts)>, String>, production: bool, rt: tokio::runtime::Handle, log: ExecLog, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inspect_ed = cx.new(|cx| EditorState::new(window, cx).language("json").line_number(true));
        let pull_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("docker.pull_placeholder")));
        let subs = vec![cx.subscribe_in(&pull_in, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| { if let InputEvent::PressEnter { .. } = ev { this.pull(cx); } })];
        let me = Self { title: ep.name.clone(), production, dock: None, conn_err: None, rt, log, sub: Sub::Containers, containers: vec![], show_all: true, images: vec![], volumes: vec![], networks: vec![],
            loading: false, error: None, selected: None, inspect_ed, pending_inspect: None, pull_in, pull_log: vec![], pulling: false, ask: None, _subs: subs, ep };
        let mut me = me;
        let via = match via { Ok(v) => v, Err(e) => { me.conn_err = Some(e); return me; } };
        let e = me.ep.clone();
        let h = me.rt.spawn(async move { Dock::open(&e, via).await.map(Arc::new) });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| {
                let what = format!("connect {}", v.ep.name);
                match r {
                    Ok(d) => { log_push(&v.log, "docker", &what, true, d.version.clone()); v.dock = Some(d); v.refresh(cx); }
                    Err(e) => { log_push(&v.log, "docker", &what, false, format!("{e:#}")); v.conn_err = Some(format!("{e:#}")); }
                }
                cx.notify();
            });
        }).detach();
        me
    }

    pub fn save(&self) -> ssh_term_proto::session_store::PaneSave { ssh_term_proto::session_store::PaneSave::Docker { endpoint: self.ep.id.clone() } }
    pub fn crumbs(&self) -> String {
        let mut s = format!("{} › {}", t("tool.docker"), self.ep.name);
        if let Some(c) = self.selected.as_ref().and_then(|id| self.containers.iter().find(|c| &c.id == id)) { s.push_str(" › "); s.push_str(&c.name); }
        s
    }
    pub fn rt(&self) -> tokio::runtime::Handle { self.rt.clone() }

    fn job<T: Send + 'static, F: Future<Output = anyhow::Result<T>> + Send + 'static>(&mut self, f: impl FnOnce(Arc<Dock>) -> F, done: impl FnOnce(&mut Self, anyhow::Result<T>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let Some(d) = self.dock.clone() else { return };
        let h = self.rt.spawn(f(d));
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| { done(v, r, cx); cx.notify(); });
        }).detach();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        match self.sub {
            Sub::Containers => self.job(|d| async move { d.containers().await }, |v, r, _| { v.loading = false; match r { Ok(c) => v.containers = c, Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
            Sub::Images => self.job(|d| async move { d.images().await }, |v, r, _| { v.loading = false; match r { Ok(c) => v.images = c, Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
            Sub::Volumes => self.job(|d| async move { d.volumes().await }, |v, r, _| { v.loading = false; match r { Ok(c) => v.volumes = c, Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
            Sub::Networks => self.job(|d| async move { d.networks().await }, |v, r, _| { v.loading = false; match r { Ok(c) => v.networks = c, Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
        }
    }

    fn act(&mut self, id: String, name: String, a: Action, cx: &mut Context<Self>) {
        if self.ep.read_only { self.error = Some(t("docker.read_only").into()); cx.notify(); return; }
        // stop / kill / remove (and anything on production) asks first
        if self.production || matches!(a, Action::Stop | Action::Kill | Action::Remove | Action::Restart) { self.ask = Some(Ask::Act(id, name, a)); cx.notify(); return; }
        self.run_act(id, name, a, cx);
    }
    fn run_act(&mut self, id: String, name: String, a: Action, cx: &mut Context<Self>) {
        let what = format!("{a:?} {name}");
        self.job(move |d| async move { d.act(&id, a).await }, move |v, r, cx| {
            log_push(&v.log, "docker", &what, r.is_ok(), r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default());
            if let Err(e) = r { v.error = Some(format!("{e:#}")); }
            v.refresh(cx);
        }, cx);
    }
    fn inspect(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = Some(id.clone());
        self.job(move |d| async move { d.inspect(&id).await }, |v, r, _| match r { Ok(s) => v.pending_inspect = Some(s), Err(e) => v.error = Some(format!("{e:#}")) }, cx);
    }
    fn pull(&mut self, cx: &mut Context<Self>) {
        if self.ep.read_only { self.error = Some(t("docker.read_only").into()); return; }
        let r = self.pull_in.read(cx).value().trim().to_string();
        if r.is_empty() || self.pulling { return; }
        self.pulling = true;
        self.pull_log.clear();
        let (tx, rx) = flume::unbounded::<String>();
        cx.spawn(async move |this, cx| {
            while let Ok(l) = rx.recv_async().await {
                if this.update(cx, |v, cx| { if v.pull_log.last() != Some(&l) { v.pull_log.push(l); } if v.pull_log.len() > 200 { v.pull_log.remove(0); } cx.notify(); }).is_err() { break; }
            }
        }).detach();
        let r2 = r.clone();
        self.job(move |d| async move { d.pull(&r, Some(tx)).await }, move |v, res, cx| {
            v.pulling = false;
            log_push(&v.log, "docker", &format!("pull {r2}"), res.is_ok(), res.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default());
            match res { Ok(()) => v.pull_log.push(tf("docker.pulled", &[("r", &r2)])), Err(e) => v.error = Some(format!("{e:#}")) }
            v.refresh(cx);
        }, cx);
    }
    fn remove_image(&mut self, id: String, cx: &mut Context<Self>) {
        let what = format!("rmi {id}");
        self.job(move |d| async move { d.remove_image(&id, false).await }, move |v, r, cx| {
            log_push(&v.log, "docker", &what, r.is_ok(), r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default());
            if let Err(e) = r { v.error = Some(format!("{e:#}")); }
            v.refresh(cx);
        }, cx);
    }
    pub fn close(&mut self) { if let Some(d) = self.dock.take() { d.close(); } }

    // ---- render
    fn containers_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let shown: Vec<Container> = self.containers.iter().filter(|c| self.show_all || c.state == "running").cloned().collect();
        let mut list = div().id("dk-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        if shown.is_empty() && !self.loading { list = list.child(div().p_4().child(small(t("docker.no_containers")))); }
        let mut n = 0usize;
        for (project, cs) in dock::group(&shown) {
            let running = cs.iter().filter(|c| c.state == "running").count();
            let head = match &project { Some(p) => format!("{p}  ·  {running}/{}", cs.len()), None => t("docker.standalone").to_string() };
            list = list.child(div().h(px(28.)).px_3().flex().items_center().gap_2().bg(c(th.mantle))
                .child(icon(if project.is_some() { IconName::Layers } else { IconName::Box }, 13., c(th.accent)))
                .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(head)));
            for ct in cs {
                n += 1;
                let sel = self.selected.as_ref() == Some(&ct.id);
                let running = ct.state == "running";
                let paused = ct.state == "paused";
                let (id, name) = (ct.id.clone(), ct.name.clone());
                let mk = |lbl: SharedString, key: &'static str, a: Action, cx: &mut Context<Self>| {
                    let (id, name) = (id.clone(), name.clone());
                    ui::button((key, n), lbl, if a == Action::Remove { 2 } else { 0 }).on_click(cx.listener(move |v, _, _w, cx| { cx.stop_propagation(); v.act(id.clone(), name.clone(), a, cx); }))
                };
                let mut btns = div().flex().gap_1().flex_shrink_0();
                if running { btns = btns.child(mk(t("docker.stop").into(), "dk-stop", Action::Stop, cx)).child(mk(t("docker.restart").into(), "dk-rs", Action::Restart, cx)).child(mk(t("docker.pause").into(), "dk-pz", Action::Pause, cx)); }
                else if paused { btns = btns.child(mk(t("docker.unpause").into(), "dk-up", Action::Unpause, cx)); }
                else { btns = btns.child(mk(t("docker.start").into(), "dk-start", Action::Start, cx)).child(mk(t("ui.delete").into(), "dk-rm", Action::Remove, cx)); }
                let (lid, lname, eid) = (ct.id.clone(), ct.name.clone(), ct.id.clone());
                btns = btns.child(ui::button(("dk-logs", n), t("docker.logs"), 0).on_click(cx.listener(move |_, _, _w, cx| { cx.stop_propagation(); cx.emit(DockEvent::Logs(lid.clone(), lname.clone())); })));
                if running { btns = btns.child(ui::button(("dk-exec", n), t("docker.exec"), 0).on_click(cx.listener(move |_, _, _w, cx| { cx.stop_propagation(); cx.emit(DockEvent::Exec(eid.clone())); }))); }
                let iid = ct.id.clone();
                list = list.child(div().id(("dk-c", n)).min_h(px(44.)).px_3().py_1().flex().items_center().gap_3().border_b_1().border_color(c(th.surface0).opacity(0.5)).cursor_pointer()
                    .when(sel, |d| d.bg(c(th.accent).opacity(0.14))).hover(|d| d.bg(c(th.surface0).opacity(0.4)))
                    .on_click(cx.listener(move |v, _, _w, cx| { v.inspect(iid.clone(), cx); cx.notify(); }))
                    .child(div().size(px(8.)).rounded_full().flex_shrink_0().bg(state_color(&ct.state)))
                    .child(div().flex_1().min_w_0().flex().flex_col()
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(13.)).text_color(c(th.text)).child(match &ct.service { Some(s) => format!("{}  ({s})", ct.name), None => ct.name.clone() }))
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(small(format!("{} · {} · {}", ct.image, ct.status, ct.ports)))))
                    .child(btns));
            }
        }
        let detail = self.selected.is_some().then(|| div().w(px(440.)).flex_shrink_0().h_full().flex().flex_col().border_l_1().border_color(c(th.surface0))
            .child(div().h(px(32.)).px_3().flex().items_center().gap_2().child(small(t("docker.inspect"))).child(div().flex_1())
                .child(div().id("dk-insp-x").cursor_pointer().child(icon(IconName::X, 13., c(th.overlay1))).on_click(cx.listener(|v, _, _w, cx| { v.selected = None; cx.notify(); }))))
            .child(div().flex_1().min_h_0().child(gpui_component::input::Editor::new(&self.inspect_ed).h_full())));
        div().flex_1().min_h_0().flex().child(div().flex_1().min_w_0().flex().flex_col().child(list)).children(detail).into_any_element()
    }

    fn images_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().id("dk-img").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        for (i, im) in self.images.iter().enumerate() {
            let tags = if im.tags.is_empty() { "<none>".to_string() } else { im.tags.join(", ") };
            let (id, label) = (im.id.clone(), tags.clone());
            list = list.child(div().min_h(px(36.)).px_3().flex().items_center().gap_3().border_b_1().border_color(c(th.surface0).opacity(0.5))
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(13.)).text_color(c(th.text)).child(tags))
                    .child(small(format!("{} · {} · {}", im.id.trim_start_matches("sha256:").chars().take(12).collect::<String>(), dock::human_size(im.size), ago(im.created)))))
                .when(im.containers > 0, |d| d.child(small(tf("docker.used_by", &[("n", &im.containers)]))))
                .child(ui::button(("dk-rmi", i), t("ui.delete"), 2).on_click(cx.listener(move |v, _, _w, cx| {
                    if v.ep.read_only { v.error = Some(t("docker.read_only").into()); } else { v.ask = Some(Ask::RemoveImage(id.clone(), label.clone())); }
                    cx.notify();
                }))));
        }
        let mut pull = div().p_3().flex().flex_col().gap_2().border_b_1().border_color(c(th.surface0))
            .child(div().flex().gap_2().child(div().flex_1().child(ui::text_field(&self.pull_in, false)))
                .child(ui::button("dk-pull", if self.pulling { t("docker.pulling") } else { t("docker.pull") }, 1).on_click(cx.listener(|v, _, _w, cx| { v.pull(cx); cx.notify(); }))));
        if !self.pull_log.is_empty() {
            let mut lg = div().id("dk-pull-log").max_h(px(140.)).overflow_y_scroll().p_2().rounded(px(6.)).bg(c(th.mantle)).flex().flex_col();
            for l in self.pull_log.iter().rev().take(40) { lg = lg.child(mono(l.clone())); }
            pull = pull.child(lg);
        }
        div().flex_1().min_h_0().flex().flex_col().child(pull).child(list).into_any_element()
    }

    fn simple_el(&self, rows: Vec<(String, String)>) -> AnyElement {
        let th = cur();
        let mut list = div().id("dk-simple").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        for (a, b) in rows {
            list = list.child(div().min_h(px(32.)).px_3().flex().items_center().gap_3().border_b_1().border_color(c(th.surface0).opacity(0.5))
                .child(div().w(px(320.)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(13.)).text_color(c(th.text)).child(a))
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(small(b))));
        }
        list.into_any_element()
    }

    fn ask_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.ask.as_ref()?;
        let th = cur();
        let msg = match ask { Ask::Act(_, name, a) => tf("docker.confirm_act", &[("a", &format!("{a:?}").to_lowercase()), ("name", name)]), Ask::RemoveImage(_, l) => tf("docker.confirm_rmi", &[("name", l)]) };
        let prod = self.production.then(|| ui::tag(t("tag.production"), c(th.error)));
        Some(ui::modal(ui::card(460.)
            .child(div().p_5().flex().flex_col().gap_3()
                .child(div().flex().items_center().gap_2().child(icon(IconName::TriangleAlert, 16., c(th.warning))).child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("db.confirm_title"))).children(prod))
                .child(small(msg)))
            .child(div().px_5().py_3().flex().justify_end().gap_2().border_t_1().border_color(c(th.surface0))
                .child(ui::button("dk-cf-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.ask = None; cx.notify(); })))
                .child(ui::button("dk-cf-yes", t("db.confirm_run"), 2).on_click(cx.listener(|v, _, _w, cx| {
                    match v.ask.take() { Some(Ask::Act(id, name, a)) => v.run_act(id, name, a, cx), Some(Ask::RemoveImage(id, _)) => v.remove_image(id, cx), None => {} }
                    cx.notify();
                }))))).into_any_element())
    }
}

impl Render for DockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        if let Some(s) = self.pending_inspect.take() { self.inspect_ed.update(cx, |e, cx| e.set_value(s, window, cx)); }
        let mut head = div().h(px(40.)).px_3().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0)).bg(c(th.mantle))
            .child(icon(IconName::Container, 14., c(th.accent)))
            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.ep.name.clone()))
            .child(small(self.dock.as_ref().map(|d| format!("{} · {}", d.via.clone().unwrap_or_else(|| t("docker.local").to_string()), d.version)).unwrap_or_default()));
        if self.production { head = head.child(ui::tag(t("tag.production"), c(th.error))); }
        if self.ep.read_only { head = head.child(ui::tag(t("db.read_only"), c(th.warning))); }
        head = head.child(div().flex_1());
        if self.sub == Sub::Containers {
            head = head.child(ui::button("dk-all", if self.show_all { t("docker.show_running") } else { t("docker.show_all") }, 0).on_click(cx.listener(|v, _, _w, cx| { v.show_all = !v.show_all; cx.notify(); })));
        }
        head = head.child(ui::button("dk-refresh", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.refresh(cx); cx.notify(); })));
        let subs: Vec<(String, String)> = [("c", t("docker.containers")), ("i", t("docker.images")), ("v", t("docker.volumes")), ("n", t("docker.networks"))].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let cur_sub = match self.sub { Sub::Containers => "c", Sub::Images => "i", Sub::Volumes => "v", Sub::Networks => "n" };
        let me = cx.entity().downgrade();
        head = head.child(ui::segmented("dk-sub", &subs, cur_sub, move |s, _w, cx| {
            let sub = match s.as_str() { "i" => Sub::Images, "v" => Sub::Volumes, "n" => Sub::Networks, _ => Sub::Containers };
            let _ = me.update(cx, |v, cx| { v.sub = sub; v.refresh(cx); cx.notify(); });
        }));
        let body: AnyElement = if let Some(e) = &self.conn_err {
            div().flex_1().p_5().flex().flex_col().gap_2().child(div().text_size(px(13.)).text_color(c(th.error)).child(t("ui.connection_failed"))).child(mono(e.clone()).whitespace_normal()).into_any_element()
        } else if self.dock.is_none() {
            div().flex_1().flex().items_center().justify_center().child(small(t("ui.connecting"))).into_any_element()
        } else {
            match self.sub {
                Sub::Containers => self.containers_el(cx),
                Sub::Images => self.images_el(cx),
                Sub::Volumes => self.simple_el(self.volumes.iter().map(|v| (v.name.clone(), format!("{} · {}", v.driver, v.mountpoint))).collect()),
                Sub::Networks => self.simple_el(self.networks.iter().map(|n| (n.name.clone(), format!("{} · {} · {}", n.driver, n.scope, n.id.chars().take(12).collect::<String>()))).collect()),
            }
        };
        let err = self.error.clone().map(|e| div().mx_3().mt_2().p_2().rounded(px(6.)).bg(c(th.error).opacity(0.12)).text_size(px(12.)).text_color(c(th.error)).whitespace_normal().child(e));
        let ask = self.ask_el(cx);
        div().size_full().flex().flex_col().relative().bg(c(th.base)).child(head).children(err).child(body).children(ask)
    }
}

impl Drop for DockView { fn drop(&mut self) { self.close(); } }

// ---------------- logs tab
pub struct LogView {
    pub title: String,
    endpoint: String,
    name: String,
    dock: Option<Arc<Dock>>,
    conn_err: Option<String>,
    rt: tokio::runtime::Handle,
    id: String,
    lines: Vec<String>,
    follow: bool,
    timestamps: bool,
    task: Option<tokio::task::JoinHandle<()>>,
    filter_in: Entity<InputState>,
    _subs: Vec<gpui::Subscription>,
}

const LOG_CAP: usize = 20_000;

impl LogView {
    /// `dock`: an open connection to reuse; otherwise `ep` / `via` open one (layout restore).
    #[allow(clippy::too_many_arguments)]
    pub fn new(dock: Option<Arc<Dock>>, ep: Option<DockerEndpoint>, via: Result<Option<(SshTarget, Prompts)>, String>, endpoint: String, rt: tokio::runtime::Handle, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter_in = cx.new(|cx| InputState::new(window, cx).placeholder(t("docker.log_filter")));
        let subs = vec![cx.subscribe_in(&filter_in, window, |_: &mut Self, _, _: &InputEvent, _w, cx| cx.notify())];
        let mut me = Self { title: format!("{} {name}", t("docker.logs")), endpoint, name, dock, conn_err: None, rt, id, lines: vec![], follow: true, timestamps: false, task: None, filter_in, _subs: subs };
        if me.dock.is_some() { me.start(cx); return me; }
        let Some(ep) = ep else { me.conn_err = Some(t("kv.gone").into()); return me };
        let via = match via { Ok(v) => v, Err(e) => { me.conn_err = Some(e); return me } };
        let h = me.rt.spawn(async move { Dock::open(&ep, via).await.map(Arc::new) });
        cx.spawn(async move |this, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let _ = this.update(cx, |v, cx| { match r { Ok(d) => { v.dock = Some(d); v.start(cx); } Err(e) => v.conn_err = Some(format!("{e:#}")) } cx.notify(); });
        }).detach();
        me
    }
    pub fn save(&self) -> ssh_term_proto::session_store::PaneSave { ssh_term_proto::session_store::PaneSave::DockerLogs { endpoint: self.endpoint.clone(), container: self.id.clone(), name: self.name.clone() } }
    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(dock) = self.dock.clone() else { return };
        if let Some(t) = self.task.take() { t.abort(); }
        self.lines.clear();
        let (tx, rx) = flume::bounded::<String>(4096);
        let _g = self.rt.enter();
        self.task = Some(dock.logs(&self.id, 1000, self.follow, self.timestamps, tx));
        cx.spawn(async move |this, cx| {
            while let Ok(l) = rx.recv_async().await {
                let mut batch = vec![l];
                while let Ok(m) = rx.try_recv() { batch.push(m); if batch.len() > 2000 { break; } }
                if this.update(cx, |v, cx| { v.lines.extend(batch); if v.lines.len() > LOG_CAP { let d = v.lines.len() - LOG_CAP; v.lines.drain(..d); } cx.notify(); }).is_err() { break; }
            }
        }).detach();
    }
}

impl Drop for LogView { fn drop(&mut self) { if let Some(t) = self.task.take() { t.abort(); } } }

impl Render for LogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let f = self.filter_in.read(cx).value().trim().to_lowercase();
        let shown: Vec<&String> = self.lines.iter().filter(|l| f.is_empty() || l.to_lowercase().contains(&f)).collect();
        let start = shown.len().saturating_sub(3000);
        let mut out = div().id("dk-log").flex_1().min_h_0().overflow_y_scroll().p_2().bg(c(th.mantle)).flex().flex_col();
        if let Some(e) = &self.conn_err { out = out.child(mono(e.clone()).text_color(c(th.error)).whitespace_normal()); }
        for l in &shown[start..] { out = out.child(mono((*l).clone()).whitespace_normal()); }
        div().size_full().flex().flex_col().bg(c(th.base))
            .child(div().h(px(40.)).px_3().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(IconName::FileText, 14., c(th.accent)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.title.clone()))
                .child(small(tf("docker.log_lines", &[("n", &self.lines.len())])))
                .child(div().flex_1())
                .child(div().w(px(220.)).child(ui::search_field(&self.filter_in, false)))
                .child(ui::button("dk-log-ts", if self.timestamps { t("docker.hide_ts") } else { t("docker.show_ts") }, 0).on_click(cx.listener(|v, _, _w, cx| { v.timestamps = !v.timestamps; v.start(cx); cx.notify(); })))
                .child(ui::button("dk-log-follow", if self.follow { t("docker.unfollow") } else { t("docker.follow") }, 0).on_click(cx.listener(|v, _, _w, cx| { v.follow = !v.follow; v.start(cx); cx.notify(); })))
                .child(ui::button("dk-log-clear", t("docker.clear"), 0).on_click(cx.listener(|v, _, _w, cx| { v.lines.clear(); cx.notify(); }))))
            .child(out)
    }
}
