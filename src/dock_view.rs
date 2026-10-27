//! Container engine tab content: containers grouped by compose project with actions,
//! inspect, images (pull / remove), volumes and networks. Logs and exec open their own tabs.
use std::future::Future;
use std::sync::Arc;

use std::collections::HashMap;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FontWeight, Pixels, Point, SharedString, Window};
use gpui_component::input::{EditorState, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::Sizable as _;
use ssh_term_proto::assets::DockerEndpoint;
use ssh_term_proto::dock::{self, Action, ComposeOp, Container, CreateSpec, Dock, Image, NetworkRow, Stats, VolumeRow};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh::{Prompts, SshTarget};

use crate::activity::{log_push, ExecLog};
use crate::theme::{c, cur};
use crate::ui::{self, icon, mono, small, IconName};

#[derive(Clone, Copy, PartialEq)]
pub enum Sub { Containers, Stats, Images, Volumes, Networks }

pub enum DockEvent { Logs(String, String), Exec(String) }

enum Ask { Act(String, String, Action), RemoveImage(String, String), Compose(String, Option<String>, Option<String>, ComposeOp), Create(CreateSpec), Exec(String, String) }

/// Label shown for a container action.
pub fn act_label(a: Action) -> SharedString {
    t(match a { Action::Start => "docker.start", Action::Stop => "docker.stop", Action::Restart => "docker.restart", Action::Pause => "docker.pause", Action::Unpause => "docker.unpause", Action::Kill => "docker.kill", Action::Remove => "ui.delete" }).into()
}
fn compose_label(op: ComposeOp) -> SharedString {
    t(match op { ComposeOp::Up => "docker.compose_up", ComposeOp::Stop => "docker.stop", ComposeOp::Restart => "docker.restart", ComposeOp::Down => "docker.compose_down" }).into()
}

struct CreateForm {
    image: Entity<InputState>,
    name: Entity<InputState>,
    ports: Entity<TextareaState>,
    env: Entity<TextareaState>,
    vols: Entity<TextareaState>,
    cmd: Entity<InputState>,
    restart: String,
    start: bool,
    err: Option<String>,
    busy: bool,
}

struct ExecBox { id: String, name: String, input: Entity<InputState>, output: String, running: bool }

/// Row context menu: window position, container id / name / state.
struct RowMenu { at: Point<Pixels>, id: String, name: String, state: String }

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
    menu: Option<RowMenu>,
    create: Option<CreateForm>,
    exec_box: Option<ExecBox>,
    stats: HashMap<String, Stats>,
    stats_seq: u64,
    compose_busy: Option<String>,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<DockEvent> for DockView {}

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
            loading: false, error: None, selected: None, inspect_ed, pending_inspect: None, pull_in, pull_log: vec![], pulling: false, ask: None, menu: None, create: None, exec_box: None, stats: HashMap::new(), stats_seq: 0, compose_busy: None, _subs: subs, ep };
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
            Sub::Stats => self.job(|d| async move {
                let cs = d.containers().await?;
                let running: Vec<String> = cs.iter().filter(|c| c.state == "running").map(|c| c.id.clone()).collect();
                let samples = futures_util::future::join_all(running.iter().map(|id| d.stats(id))).await;
                let st: HashMap<String, Stats> = running.into_iter().zip(samples).filter_map(|(id, s)| s.ok().map(|s| (id, s))).collect();
                Ok((cs, st))
            }, |v, r, _| { v.loading = false; match r { Ok((c, st)) => { v.containers = c; v.stats = st; } Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
            Sub::Images => self.job(|d| async move { d.images().await }, |v, r, _| { v.loading = false; match r { Ok(c) => v.images = c, Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
            Sub::Volumes => self.job(|d| async move { d.volumes().await }, |v, r, _| { v.loading = false; match r { Ok(c) => v.volumes = c, Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
            Sub::Networks => self.job(|d| async move { d.networks().await }, |v, r, _| { v.loading = false; match r { Ok(c) => v.networks = c, Err(e) => v.error = Some(format!("{e:#}")) } }, cx),
        }
    }

    fn act(&mut self, id: String, name: String, a: Action, cx: &mut Context<Self>) {
        if self.ep.read_only { self.error = Some(t("docker.read_only").into()); cx.notify(); return; }
        // stop / kill / remove (and anything on production) asks first
        if self.production || matches!(a, Action::Stop | Action::Kill | Action::Remove | Action::Restart | Action::Pause) { self.ask = Some(Ask::Act(id, name, a)); cx.notify(); return; }
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

    pub fn ai_label(&self) -> String { format!("Docker · {}", self.ep.name) }
    /// Assistant context: containers with state; `deep` adds CPU/memory samples and the recent
    /// logs of failing or restarting containers. Read-only.
    pub fn ai_context(&self, deep: bool) -> Option<tokio::task::JoinHandle<String>> {
        let d = self.dock.clone()?;
        let head = format!("Docker endpoint {} ({}, engine {}){}\n", self.ep.name, d.via.clone().unwrap_or_else(|| "local".into()), d.version, if self.production { " PRODUCTION" } else { "" });
        Some(self.rt.spawn(async move {
            let mut out = head;
            let cs = d.containers().await.unwrap_or_default();
            out.push_str("Containers (name | image | state | status | ports | compose project/service):\n");
            for c in &cs { out.push_str(&format!("{} | {} | {} | {} | {} | {}\n", c.name, c.image, c.state, c.status, c.ports, match (&c.project, &c.service) { (Some(p), Some(s)) => format!("{p}/{s}"), _ => "-".into() })); }
            if deep {
                let running: Vec<&Container> = cs.iter().filter(|c| c.state == "running").take(20).collect();
                let st = futures_util::future::join_all(running.iter().map(|c| d.stats(&c.id))).await;
                out.push_str("Resource samples:\n");
                for (c, s) in running.iter().zip(st) { if let Ok(s) = s { out.push_str(&format!("{} cpu {:.1}% mem {} / {}\n", c.name, s.cpu, dock::human_size(s.mem as i64), dock::human_size(s.limit as i64))); } }
                let bad = |c: &Container| c.state == "restarting" || c.state == "dead" || c.status.contains("unhealthy") || (c.state == "exited" && !c.status.contains("(0)"));
                for c in cs.iter().filter(|c| bad(c)).take(5) {
                    let (tx, rx) = flume::unbounded();
                    let _ = d.logs(&c.id, 60, false, true, tx).await;
                    let lines: Vec<String> = rx.drain().collect();
                    out.push_str(&format!("Recent logs of {} ({}):\n{}\n", c.name, c.status, crate::ai_ctx::cap(lines.join("\n"), 4000)));
                }
            }
            crate::ai_ctx::cap(out, 20_000)
        }))
    }

    /// Stats view: sample every 3s while it is shown.
    fn start_stats(&mut self, cx: &mut Context<Self>) {
        self.stats_seq += 1;
        let seq = self.stats_seq;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
            let go = this.update(cx, |v, cx| { let go = v.sub == Sub::Stats && v.stats_seq == seq; if go && !v.loading { v.refresh(cx); } go }).unwrap_or(false);
            if !go { break; }
        }).detach();
    }

    fn compose(&mut self, project: String, workdir: Option<String>, files: Option<String>, op: ComposeOp, cx: &mut Context<Self>) {
        if self.ep.read_only { self.error = Some(t("docker.read_only").into()); cx.notify(); return; }
        if self.production || op.dangerous() { self.ask = Some(Ask::Compose(project, workdir, files, op)); cx.notify(); return; }
        self.run_compose(project, workdir, files, op, cx);
    }
    fn run_compose(&mut self, project: String, workdir: Option<String>, files: Option<String>, op: ComposeOp, cx: &mut Context<Self>) {
        let sock = self.ep.is_ssh().then(|| self.ep.socket_path().to_string()).filter(|s| s != "/var/run/docker.sock");
        let what = format!("compose {} {project}", op.arg());
        self.compose_busy = Some(project.clone());
        self.job(move |d| async move { d.compose(&project, workdir.as_deref(), files.as_deref(), op, sock.as_deref()).await }, move |v, r, cx| {
            v.compose_busy = None;
            log_push(&v.log, "docker", &what, r.is_ok(), match &r { Ok(o) => o.lines().last().unwrap_or_default().to_string(), Err(e) => format!("{e:#}") });
            if let Err(e) = r { v.error = Some(format!("{e:#}")); }
            v.refresh(cx);
        }, cx);
    }

    fn open_create(&mut self, image: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.ep.read_only { self.error = Some(t("docker.read_only").into()); cx.notify(); return; }
        let image_v = image.to_string();
        let ta = |ph: &'static str, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| TextareaState::new(window, cx).placeholder(t(ph)).auto_grow(2, 5));
        self.create = Some(CreateForm {
            image: cx.new(|cx| InputState::new(window, cx).placeholder(t("docker.pull_placeholder")).default_value(image_v)),
            name: cx.new(|cx| InputState::new(window, cx).placeholder(t("docker.f_name_ph"))),
            ports: ta("docker.f_ports_ph", window, cx),
            env: ta("docker.f_env_ph", window, cx),
            vols: ta("docker.f_vols_ph", window, cx),
            cmd: cx.new(|cx| InputState::new(window, cx).placeholder(t("docker.f_cmd_ph"))),
            restart: "no".into(), start: true, err: None, busy: false,
        });
        cx.notify();
    }
    fn submit_create(&mut self, cx: &mut Context<Self>) {
        let Some(f) = &self.create else { return };
        let lines = |e: &Entity<TextareaState>, cx: &Context<Self>| e.read(cx).value().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>();
        let spec = CreateSpec {
            image: f.image.read(cx).value().trim().to_string(), name: f.name.read(cx).value().trim().to_string(),
            ports: lines(&f.ports, cx), env: lines(&f.env, cx), volumes: lines(&f.vols, cx),
            restart: f.restart.clone(), command: f.cmd.read(cx).value().trim().to_string(), start: f.start,
        };
        // validate before anything reaches the daemon
        if let Err(e) = dock::create_body(&spec) { if let Some(f) = self.create.as_mut() { f.err = Some(format!("{e:#}")); } cx.notify(); return; }
        if self.production { self.ask = Some(Ask::Create(spec)); cx.notify(); return; }
        self.run_create(spec, cx);
    }
    fn run_create(&mut self, spec: CreateSpec, cx: &mut Context<Self>) {
        if let Some(f) = self.create.as_mut() { f.busy = true; f.err = None; }
        let what = format!("create {}", spec.image);
        self.job(move |d| async move {
            // pull first when the image is missing locally
            if d.api.inspect_image(spec.image.trim()).await.is_err() { d.pull(spec.image.trim(), None).await?; }
            d.create(&spec).await
        }, move |v, r, cx| {
            log_push(&v.log, "docker", &what, r.is_ok(), r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default());
            match r {
                Ok(_) => { v.create = None; if v.sub != Sub::Containers { v.sub = Sub::Containers; } v.refresh(cx); }
                Err(e) => if let Some(f) = v.create.as_mut() { f.busy = false; f.err = Some(format!("{e:#}")); },
            }
        }, cx);
    }

    fn open_exec(&mut self, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.ep.read_only { self.error = Some(t("docker.read_only").into()); cx.notify(); return; }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t("docker.exec_ph")));
        self._subs.push(cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &InputEvent, _w, cx| { if let InputEvent::PressEnter { .. } = ev { this.submit_exec(cx); } }));
        input.update(cx, |i, cx| i.focus(window, cx));
        self.exec_box = Some(ExecBox { id, name, input, output: String::new(), running: false });
        cx.notify();
    }
    fn submit_exec(&mut self, cx: &mut Context<Self>) {
        let Some(b) = &self.exec_box else { return };
        let cmd = b.input.read(cx).value().trim().to_string();
        if cmd.is_empty() || b.running { return; }
        let id = b.id.clone();
        if self.production { self.ask = Some(Ask::Exec(id, cmd)); cx.notify(); return; }
        self.run_exec(id, cmd, cx);
    }
    fn run_exec(&mut self, id: String, cmd: String, cx: &mut Context<Self>) {
        let Some(b) = self.exec_box.as_mut() else { return };
        b.running = true;
        let what = format!("exec {}: {cmd}", b.name);
        let shown = format!("$ {cmd}\n");
        self.job(move |d| async move { d.exec_once(&id, &cmd).await }, move |v, r, _| {
            log_push(&v.log, "docker", &what, r.is_ok(), r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default());
            if let Some(b) = v.exec_box.as_mut() {
                b.running = false;
                let body = match r { Ok(o) => o, Err(e) => format!("{e:#}") };
                b.output.push_str(&shown); b.output.push_str(&body); if !b.output.ends_with('\n') { b.output.push('\n'); }
                if b.output.len() > 512 * 1024 { let cut = b.output.len() - 512 * 1024; let cut = (cut..b.output.len()).find(|i| b.output.is_char_boundary(*i)).unwrap_or(0); b.output.drain(..cut); }
            }
        }, cx);
    }

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
            let mut hrow = div().h(px(30.)).px_3().flex().items_center().gap_2().bg(crate::theme::sub_bg())
                .child(icon(if project.is_some() { IconName::Layers } else { IconName::Box }, ui::IC_SM, c(th.accent)))
                .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(head));
            if let Some(p) = &project {
                hrow = hrow.child(div().flex_1());
                if self.compose_busy.as_deref() == Some(p.as_str()) { hrow = hrow.child(icon(IconName::LoaderCircle, ui::IC_XS, c(th.accent))); }
                let (wd, files) = (cs.iter().find_map(|c| c.workdir.clone()), cs.iter().find_map(|c| c.config_files.clone()));
                for (k, op) in [ComposeOp::Up, ComposeOp::Restart, ComposeOp::Stop, ComposeOp::Down].into_iter().enumerate() {
                    let (p, wd, files) = (p.clone(), wd.clone(), files.clone());
                    n += 1;
                    hrow = hrow.child(ui::button(("dk-cmp", n * 8 + k), compose_label(op), if op == ComposeOp::Down { 2 } else { 0 })
                        .on_click(cx.listener(move |v, _, _w, cx| v.compose(p.clone(), wd.clone(), files.clone(), op, cx))));
                }
            }
            list = list.child(hrow);
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
                if running { btns = btns.child(mk(t("docker.stop").into(), "dk-stop", Action::Stop, cx)).child(mk(t("docker.restart").into(), "dk-rs", Action::Restart, cx)); }
                else if paused { btns = btns.child(mk(t("docker.unpause").into(), "dk-up", Action::Unpause, cx)); }
                else { btns = btns.child(mk(t("docker.start").into(), "dk-start", Action::Start, cx)).child(mk(t("ui.delete").into(), "dk-rm", Action::Remove, cx)); }
                let (lid, lname, eid) = (ct.id.clone(), ct.name.clone(), ct.id.clone());
                btns = btns.child(ui::button(("dk-logs", n), t("docker.logs"), 0).on_click(cx.listener(move |_, _, _w, cx| { cx.stop_propagation(); cx.emit(DockEvent::Logs(lid.clone(), lname.clone())); })));
                if running { btns = btns.child(ui::button(("dk-exec", n), t("docker.exec"), 0).on_click(cx.listener(move |_, _, _w, cx| { cx.stop_propagation(); cx.emit(DockEvent::Exec(eid.clone())); }))); }
                let (mid, mname, mstate) = (ct.id.clone(), ct.name.clone(), ct.state.clone());
                btns = btns.child(div().id(("dk-more", n)).size(px(26.)).rounded(px(ui::R_MD)).flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg()))
                    .on_click(cx.listener(move |v, ev: &gpui::ClickEvent, _w, cx| { cx.stop_propagation(); v.menu = Some(RowMenu { at: ev.position(), id: mid.clone(), name: mname.clone(), state: mstate.clone() }); cx.notify(); }))
                    .child(icon(IconName::Ellipsis, ui::IC_SM, c(th.overlay1))));
                let (rid, rname, rstate) = (ct.id.clone(), ct.name.clone(), ct.state.clone());
                let iid = ct.id.clone();
                list = list.child(div().id(("dk-c", n)).min_h(px(44.)).px_3().py_1().flex().items_center().gap_3().border_b_1().border_color(crate::theme::divider()).cursor_pointer()
                    .when(sel, |d| d.bg(crate::theme::selected_bg())).hover(|d| d.bg(crate::theme::hover_bg()))
                    .on_click(cx.listener(move |v, _, _w, cx| { v.inspect(iid.clone(), cx); cx.notify(); }))
                    .on_mouse_down(gpui::MouseButton::Right, cx.listener(move |v, ev: &gpui::MouseDownEvent, _w, cx| { v.menu = Some(RowMenu { at: ev.position, id: rid.clone(), name: rname.clone(), state: rstate.clone() }); cx.notify(); }))
                    .child(div().size(px(8.)).rounded_full().flex_shrink_0().bg(state_color(&ct.state)))
                    .child(div().flex_1().min_w_0().flex().flex_col()
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(match &ct.service { Some(s) => format!("{}  ({s})", ct.name), None => ct.name.clone() }))
                        .child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().child(small(format!("{} · {} · {}", ct.image, ct.status, ct.ports)))))
                    .child(btns));
            }
        }
        let detail = self.selected.is_some().then(|| div().w(px(440.)).flex_shrink_0().h_full().flex().flex_col().border_l_1().border_color(crate::theme::divider())
            .child(div().h(px(32.)).px_3().flex().items_center().gap_2().child(small(t("docker.inspect"))).child(div().flex_1())
                .child(div().id("dk-insp-x").cursor_pointer().child(icon(IconName::X, ui::IC_SM, c(th.overlay1))).on_click(cx.listener(|v, _, _w, cx| { v.selected = None; cx.notify(); }))))
            .child(div().flex_1().min_h_0().child(gpui_component::input::Editor::new(&self.inspect_ed).h_full())));
        div().flex_1().min_h_0().flex().child(div().flex_1().min_w_0().flex().flex_col().child(list)).children(detail).into_any_element()
    }

    fn images_el(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().id("dk-img").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        for (i, im) in self.images.iter().enumerate() {
            let tags = if im.tags.is_empty() { "<none>".to_string() } else { im.tags.join(", ") };
            let (id, label) = (im.id.clone(), tags.clone());
            list = list.child(div().min_h(px(36.)).px_3().flex().items_center().gap_3().border_b_1().border_color(crate::theme::divider())
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(tags))
                    .child(small(format!("{} · {} · {}", im.id.trim_start_matches("sha256:").chars().take(12).collect::<String>(), dock::human_size(im.size), ago(im.created)))))
                .when(im.containers > 0, |d| d.child(small(tf("docker.used_by", &[("n", &im.containers)]))))
                .child({ let img = im.tags.first().cloned().unwrap_or_else(|| im.id.clone()); ui::button(("dk-run", i), t("docker.run"), 1).on_click(cx.listener(move |v, _, w, cx| v.open_create(&img, w, cx))) })
                .child(ui::button(("dk-rmi", i), t("ui.delete"), 2).on_click(cx.listener(move |v, _, _w, cx| {
                    if v.ep.read_only { v.error = Some(t("docker.read_only").into()); } else { v.ask = Some(Ask::RemoveImage(id.clone(), label.clone())); }
                    cx.notify();
                }))));
        }
        let mut pull = div().p_3().flex().flex_col().gap_2().border_b_1().border_color(crate::theme::divider())
            .child(div().flex().gap_2().child(div().flex_1().child(ui::text_field(&self.pull_in, false)))
                .child(ui::button("dk-pull", if self.pulling { t("docker.pulling") } else { t("docker.pull") }, 1).on_click(cx.listener(|v, _, _w, cx| { v.pull(cx); cx.notify(); }))));
        if !self.pull_log.is_empty() {
            let mut lg = div().id("dk-pull-log").max_h(px(140.)).overflow_y_scroll().p_2().rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).flex().flex_col();
            for l in self.pull_log.iter().rev().take(40) { lg = lg.child(mono(l.clone())); }
            pull = pull.child(lg);
        }
        div().flex_1().min_h_0().flex().flex_col().child(pull).child(list).into_any_element()
    }

    fn simple_el(&self, rows: Vec<(String, String)>) -> AnyElement {
        let th = cur();
        let mut list = div().id("dk-simple").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        for (a, b) in rows {
            list = list.child(div().min_h(px(32.)).px_3().flex().items_center().gap_3().border_b_1().border_color(crate::theme::divider())
                .child(div().w(px(320.)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(a))
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().child(small(b))));
        }
        list.into_any_element()
    }

    fn stats_el(&self) -> AnyElement {
        let th = cur();
        let mut list = div().id("dk-stats").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        let running: Vec<&Container> = self.containers.iter().filter(|c| c.state == "running").collect();
        if running.is_empty() && !self.loading { list = list.child(div().p_4().child(small(t("docker.no_running")))); }
        let bar = |frac: f64, col: gpui::Hsla| div().w(px(140.)).h(px(6.)).flex_shrink_0().rounded_full().bg(c(th.surface0)).child(div().h_full().rounded_full().bg(col).w(px((140. * frac.clamp(0., 1.)) as f32)));
        for ct in running {
            let st = self.stats.get(&ct.id).copied();
            let (cpu, mem) = match st { Some(s) => (format!("{:.1}%", s.cpu), if s.limit > 0 { format!("{} / {}", dock::human_size(s.mem as i64), dock::human_size(s.limit as i64)) } else { dock::human_size(s.mem as i64) }), None => ("…".into(), "…".into()) };
            let s = st.unwrap_or_default();
            let mf = if s.limit > 0 { s.mem as f64 / s.limit as f64 } else { 0. };
            list = list.child(div().min_h(px(40.)).px_3().flex().items_center().gap_3().border_b_1().border_color(crate::theme::divider())
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_body())).text_color(c(th.text)).child(ct.name.clone()))
                .child(small(t("docker.cpu"))).child(bar(s.cpu / 100., if s.cpu > 80. { c(th.error) } else { c(th.accent) })).child(div().w(px(64.)).child(mono(cpu)))
                .child(small(t("docker.mem"))).child(bar(mf, if mf > 0.85 { c(th.error) } else { c(th.success) })).child(div().w(px(170.)).child(mono(mem))));
        }
        list.into_any_element()
    }

    fn menu_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.menu.as_ref()?;
        let running = m.state == "running";
        let paused = m.state == "paused";
        let mut acts: Vec<(Action, bool)> = vec![(Action::Start, !running && !paused), (Action::Stop, running || paused), (Action::Restart, true), (Action::Pause, running), (Action::Unpause, paused), (Action::Kill, running || paused)];
        acts.push((Action::Remove, true));
        let mut pop = ui::popover().w(px(200.)).py_1();
        for (i, (a, on)) in acts.into_iter().enumerate() {
            let (id, name) = (m.id.clone(), m.name.clone());
            let ic = match a { Action::Start | Action::Unpause => IconName::Play, Action::Stop | Action::Kill => IconName::CircleStop, Action::Restart => IconName::RefreshCw, Action::Pause => IconName::Pause, Action::Remove => IconName::Delete };
            if a == Action::Remove { pop = pop.child(ui::menu_sep()); }
            pop = pop.child(ui::menu_item(("dk-m", i), Some(ic), act_label(a), on, matches!(a, Action::Kill | Action::Remove))
                .on_click(cx.listener(move |v, _, _w, cx| { if !on { return; } v.menu = None; v.act(id.clone(), name.clone(), a, cx); cx.notify(); })));
        }
        let (id, name) = (m.id.clone(), m.name.clone());
        let (lid, lname) = (m.id.clone(), m.name.clone());
        pop = pop.child(ui::menu_sep())
            .child(ui::menu_item("dk-m-exec", Some(IconName::SquareTerminal), t("docker.exec_once"), running, false)
                .on_click(cx.listener(move |v, _, w, cx| { if !running { return; } v.menu = None; v.open_exec(id.clone(), name.clone(), w, cx); })))
            .child(ui::menu_item("dk-m-logs", Some(IconName::FileText), t("docker.logs"), true, false)
                .on_click(cx.listener(move |v, _, _w, cx| { v.menu = None; cx.emit(DockEvent::Logs(lid.clone(), lname.clone())); cx.notify(); })));
        let at = m.at;
        Some(div().absolute().top_0().left_0().size_full()
            .child(div().id("dk-menu-bg").absolute().top_0().left_0().size_full().on_mouse_down(gpui::MouseButton::Left, cx.listener(|v, _, _w, cx| { v.menu = None; cx.notify(); }))
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(|v, _, _w, cx| { v.menu = None; cx.notify(); })))
            .child(gpui::deferred(gpui::anchored().position(at).snap_to_window().child(pop.occlude())))
            .into_any_element())
    }

    fn create_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.create.as_ref()?;
        let th = cur();
        let lbl = |k: &'static str| div().w(px(96.)).flex_shrink_0().pt(px(6.)).text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(t(k));
        let row = |k: &'static str, el: AnyElement| div().flex().items_start().gap_2().child(lbl(k)).child(div().flex_1().min_w_0().child(el));
        let me = cx.entity().downgrade();
        let pols: Vec<(String, String)> = [("no", "docker.rp_no"), ("always", "docker.rp_always"), ("unless-stopped", "docker.rp_unless"), ("on-failure", "docker.rp_failure")].iter().map(|(a, b)| (a.to_string(), t(b).to_string())).collect();
        let start = f.start;
        Some(ui::modal(ui::card(560.)
            .child(ui::dialog_header(Some(IconName::Container), t("docker.create"), None, None))
            .child(div().p(px(ui::SP_5)).flex().flex_col().gap(px(ui::SP_3))
                .child(row("docker.f_image", ui::text_field(&f.image, false).into_any_element()))
                .child(row("docker.f_name", ui::text_field(&f.name, false).into_any_element()))
                .child(row("docker.f_ports", Textarea::new(&f.ports).into_any_element()))
                .child(row("docker.f_env", Textarea::new(&f.env).into_any_element()))
                .child(row("docker.f_vols", Textarea::new(&f.vols).into_any_element()))
                .child(row("docker.f_restart", ui::segmented("dk-rp", &pols, &f.restart, move |s, _w, cx| { let _ = me.update(cx, |v, cx| { if let Some(f) = v.create.as_mut() { f.restart = s; } cx.notify(); }); }).into_any_element()))
                .child(row("docker.f_cmd", ui::text_field(&f.cmd, false).into_any_element()))
                .child(div().flex().items_center().gap_2().pl(px(104.))
                    .child(gpui_component::switch::Switch::new("dk-start").checked(start).label(t("docker.f_start")).small()
                        .on_click(cx.listener(|v, on: &bool, _, cx| { if let Some(f) = v.create.as_mut() { f.start = *on; } cx.notify(); }))))
                .children(f.err.clone().map(|e| ui::notice(ui::Notice::Error, e).mx_0())))
            .child(ui::dialog_footer(None, vec![])
                .child(ui::button("dk-cr-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.create = None; cx.notify(); })))
                .child(ui::button("dk-cr-yes", if f.busy { t("docker.creating") } else { t("docker.create") }, 1).on_click(cx.listener(|v, _, _w, cx| { if v.create.as_ref().is_some_and(|f| !f.busy) { v.submit_create(cx); } cx.notify(); })))))
            .into_any_element())
    }

    fn exec_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let b = self.exec_box.as_ref()?;
        let th = cur();
        let mut out = div().id("dk-exec-out").h(px(300.)).overflow_y_scroll().p_2().rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).flex().flex_col();
        if b.output.is_empty() { out = out.child(small(t("docker.exec_hint"))); }
        for l in b.output.lines().rev().take(2000).collect::<Vec<_>>().into_iter().rev() { out = out.child(mono(l.to_string()).whitespace_normal()); }
        Some(ui::modal(ui::card(640.)
            .child(ui::dialog_header(Some(IconName::SquareTerminal), tf("docker.exec_title", &[("name", &b.name)]), None, None)
                .when(self.production, |d| d.child(ui::tag(t("tag.production"), c(th.error)))))
            .child(div().p(px(ui::SP_5)).flex().flex_col().gap(px(ui::SP_3))
                .child(div().flex().gap_2().child(div().flex_1().child(ui::text_field(&b.input, false)))
                    .child(ui::button("dk-ex-run", if b.running { t("docker.running") } else { t("docker.exec_run") }, 1).on_click(cx.listener(|v, _, _w, cx| { v.submit_exec(cx); cx.notify(); }))))
                .child(out))
            .child(ui::dialog_footer(None, vec![])
                .child(ui::button("dk-ex-clear", t("docker.clear"), 0).on_click(cx.listener(|v, _, _w, cx| { if let Some(b) = v.exec_box.as_mut() { b.output.clear(); } cx.notify(); })))
                .child(ui::button("dk-ex-close", t("docker.close"), 0).on_click(cx.listener(|v, _, _w, cx| { v.exec_box = None; cx.notify(); })))))
            .into_any_element())
    }

    fn ask_el(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.ask.as_ref()?;
        let th = cur();
        let msg = match ask {
            Ask::Act(_, name, a) => tf("docker.confirm_act", &[("a", &act_label(*a)), ("name", name)]),
            Ask::RemoveImage(_, l) => tf("docker.confirm_rmi", &[("name", l)]),
            Ask::Compose(p, _, _, op) => tf("docker.confirm_compose", &[("a", &compose_label(*op)), ("name", p)]),
            Ask::Create(sp) => tf("docker.confirm_create", &[("name", &sp.image)]),
            Ask::Exec(_, cmd) => tf("docker.confirm_exec", &[("cmd", cmd)]),
        };
        let prod = self.production.then(|| ui::tag(t("tag.production"), c(th.error)));
        Some(ui::modal(ui::card(460.)
            .child(ui::dialog_header(Some(IconName::TriangleAlert), t("db.confirm_title"), None, None).children(prod))
            .child(div().p(px(ui::SP_5)).flex().flex_col().gap(px(ui::SP_3))
                .child(small(msg)))
            .child(ui::dialog_footer(None, vec![])
                .child(ui::button("dk-cf-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _w, cx| { v.ask = None; cx.notify(); })))
                .child(ui::button("dk-cf-yes", t("db.confirm_run"), 2).on_click(cx.listener(|v, _, _w, cx| {
                    match v.ask.take() {
                        Some(Ask::Act(id, name, a)) => v.run_act(id, name, a, cx),
                        Some(Ask::RemoveImage(id, _)) => v.remove_image(id, cx),
                        Some(Ask::Compose(p, wd, f, op)) => v.run_compose(p, wd, f, op, cx),
                        Some(Ask::Create(sp)) => v.run_create(sp, cx),
                        Some(Ask::Exec(id, cmd)) => v.run_exec(id, cmd, cx),
                        None => {}
                    }
                    cx.notify();
                }))))).into_any_element())
    }
}

impl Render for DockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        if let Some(s) = self.pending_inspect.take() { self.inspect_ed.update(cx, |e, cx| e.set_value(s, window, cx)); }
        let mut head = ui::toolbar()
            .child(icon(IconName::Container, ui::IC_SM, c(th.accent)))
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.ep.name.clone()))
            .child(small(self.dock.as_ref().map(|d| format!("{} · {}", d.via.clone().unwrap_or_else(|| t("docker.local").to_string()), d.version)).unwrap_or_default()));
        if self.production { head = head.child(ui::tag(t("tag.production"), c(th.error))); }
        if self.ep.read_only { head = head.child(ui::tag(t("db.read_only"), c(th.warning))); }
        head = head.child(div().flex_1());
        if self.sub == Sub::Containers && !self.ep.read_only {
            head = head.child(ui::button("dk-new", t("docker.create"), 1).on_click(cx.listener(|v, _, w, cx| v.open_create("", w, cx))));
        }
        if self.sub == Sub::Containers {
            head = head.child(ui::button("dk-all", if self.show_all { t("docker.show_running") } else { t("docker.show_all") }, 0).on_click(cx.listener(|v, _, _w, cx| { v.show_all = !v.show_all; cx.notify(); })));
        }
        head = head.child(ui::button("dk-refresh", t("redis.refresh"), 0).on_click(cx.listener(|v, _, _w, cx| { v.refresh(cx); cx.notify(); })));
        let subs: Vec<(String, String)> = [("c", t("docker.containers")), ("s", t("docker.stats")), ("i", t("docker.images")), ("v", t("docker.volumes")), ("n", t("docker.networks"))].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let cur_sub = match self.sub { Sub::Containers => "c", Sub::Stats => "s", Sub::Images => "i", Sub::Volumes => "v", Sub::Networks => "n" };
        let me = cx.entity().downgrade();
        head = head.child(ui::segmented("dk-sub", &subs, cur_sub, move |s, _w, cx| {
            let sub = match s.as_str() { "s" => Sub::Stats, "i" => Sub::Images, "v" => Sub::Volumes, "n" => Sub::Networks, _ => Sub::Containers };
            let _ = me.update(cx, |v, cx| { v.sub = sub; v.refresh(cx); if sub == Sub::Stats { v.start_stats(cx); } cx.notify(); });
        }));
        let body: AnyElement = if let Some(e) = &self.conn_err {
            div().flex_1().p_5().flex().flex_col().gap_2().child(ui::notice(ui::Notice::Error, t("ui.connection_failed")).mx_0()).child(mono(e.clone()).whitespace_normal()).into_any_element()
        } else if self.dock.is_none() {
            div().flex_1().flex().items_center().justify_center().child(small(t("ui.connecting"))).into_any_element()
        } else {
            match self.sub {
                Sub::Containers => self.containers_el(cx),
                Sub::Stats => self.stats_el(),
                Sub::Images => self.images_el(cx),
                Sub::Volumes => self.simple_el(self.volumes.iter().map(|v| (v.name.clone(), format!("{} · {}", v.driver, v.mountpoint))).collect()),
                Sub::Networks => self.simple_el(self.networks.iter().map(|n| (n.name.clone(), format!("{} · {} · {}", n.driver, n.scope, n.id.chars().take(12).collect::<String>()))).collect()),
            }
        };
        let err = self.error.clone().map(|e| ui::notice(ui::Notice::Error, e).mt(px(ui::SP_2)));
        let (menu, create, exec) = (self.menu_el(cx), self.create_el(cx), self.exec_el(cx));
        let ask = self.ask_el(cx);
        div().size_full().flex().flex_col().relative().bg(c(th.base)).child(head).children(err).child(body).children(menu).children(create).children(exec).children(ask)
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
        let mut out = div().id("dk-log").flex_1().min_h_0().overflow_y_scroll().p_2().bg(crate::theme::sub_bg()).flex().flex_col();
        if let Some(e) = &self.conn_err { out = out.child(mono(e.clone()).text_color(c(th.error)).whitespace_normal()); }
        for l in &shown[start..] { out = out.child(mono((*l).clone()).whitespace_normal()); }
        div().size_full().flex().flex_col().bg(c(th.base))
            .child(ui::toolbar()
                .child(icon(IconName::FileText, ui::IC_SM, c(th.accent)))
                .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.title.clone()))
                .child(small(tf("docker.log_lines", &[("n", &self.lines.len())])))
                .child(div().flex_1())
                .child(div().w(px(220.)).child(ui::search_field(&self.filter_in, false)))
                .child(ui::button("dk-log-ts", if self.timestamps { t("docker.hide_ts") } else { t("docker.show_ts") }, 0).on_click(cx.listener(|v, _, _w, cx| { v.timestamps = !v.timestamps; v.start(cx); cx.notify(); })))
                .child(ui::button("dk-log-follow", if self.follow { t("docker.unfollow") } else { t("docker.follow") }, 0).on_click(cx.listener(|v, _, _w, cx| { v.follow = !v.follow; v.start(cx); cx.notify(); })))
                .child(ui::button("dk-log-clear", t("docker.clear"), 0).on_click(cx.listener(|v, _, _w, cx| { v.lines.clear(); cx.notify(); }))))
            .child(out)
    }
}
