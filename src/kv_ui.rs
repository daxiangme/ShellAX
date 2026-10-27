//! Redis and container tool windows: saved connections / endpoints, their edit dialog,
//! and opening Redis, container, log and exec tabs.
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FontWeight, SharedString, Window};
use gpui_component::input::InputState;
use gpui_component::switch::Switch;
use ssh_term_proto::assets::{self, DockerEndpoint};
use ssh_term_proto::i18n::t;

use crate::config;
use crate::dock_view::{DockEvent, DockView, LogView};
use crate::kv_view::KvView;
use crate::termview::{Embed, Spec, TermView};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{Node, Overlay, Root, Tab};

#[derive(Clone, Copy, PartialEq)]
pub enum AssetKind { Redis, Docker }

pub enum AssetEvent { Saved, Close }

/// New / edit dialog for a `[[redis]]` connection or a `[[docker]]` endpoint.
pub struct AssetDialog {
    kind: AssetKind,
    id: Option<String>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    nodes: Entity<InputState>,
    master: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    db: Entity<InputState>,
    via: Entity<crate::host_picker::HostPicker>,
    socket: Entity<InputState>,
    group: Entity<InputState>,
    mode: String,
    tls: String,
    production: bool,
    read_only: bool,
    hosts: Vec<String>,
    error: Option<String>,
}

impl EventEmitter<AssetEvent> for AssetDialog {}

impl AssetDialog {
    pub fn new(kind: AssetKind, id: Option<String>, hosts: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = config::get();
        let prod_tag = s.tags.production.clone();
        let r = id.as_ref().and_then(|i| s.redis.iter().find(|x| &x.id == i)).cloned().unwrap_or_default();
        let d = id.as_ref().and_then(|i| s.docker.iter().find(|x| &x.id == i)).cloned().unwrap_or_default();
        let mk = |ph: &str, v: String, masked: bool, window: &mut Window, cx: &mut Context<Self>| { let ph = ph.to_string(); cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(ph).default_value(v)) };
        let pw = if kind == AssetKind::Redis && id.is_some() { crate::secret_store().get(&r.password_secret()).map(|x| x.0).unwrap_or_default() } else { String::new() };
        let redis = kind == AssetKind::Redis;
        Self {
            name: mk(&t("ui.name"), if redis { r.name.clone() } else { d.name.clone() }, false, window, cx),
            host: mk("127.0.0.1", r.host.clone(), false, window, cx),
            port: mk("6379", r.port.map(|p| p.to_string()).unwrap_or_default(), false, window, cx),
            nodes: mk("10.0.0.2:6379, 10.0.0.3:6379", r.nodes.clone(), false, window, cx),
            master: mk("mymaster", r.master.clone(), false, window, cx),
            user: mk("default", r.user.clone(), false, window, cx),
            password: mk(&t("ui.password"), pw, true, window, cx),
            db: mk("0", if r.db == 0 { String::new() } else { r.db.to_string() }, false, window, cx),
            via: { let (v, h) = (if redis { r.ssh_via.clone() } else { d.ssh_host.clone() }, hosts.clone()); cx.new(|cx| crate::host_picker::HostPicker::new("kv-via", v, h, window, cx)) },
            socket: mk("/var/run/docker.sock", d.socket.clone(), false, window, cx),
            group: mk(&t("ui.e_g_prod_test"), (if redis { r.group.clone() } else { d.group.clone() }).unwrap_or_default(), false, window, cx),
            mode: if r.mode.is_empty() { "standalone".into() } else { r.mode.clone() },
            tls: if r.tls.is_empty() { "disable".into() } else { r.tls.clone() },
            production: (if redis { &r.tags } else { &d.tags }).contains(&prod_tag),
            read_only: if redis { r.read_only } else { d.read_only },
            hosts, error: None, kind, id,
        }
    }
    fn v(e: &Entity<InputState>, cx: &Context<Self>) -> String { e.read(cx).value().trim().to_string() }

    fn save(&mut self, cx: &mut Context<Self>) {
        let prod_tag = config::get().tags.production.clone();
        let group = Some(Self::v(&self.group, cx)).filter(|g| !g.is_empty());
        let via = self.via.read(cx).value.clone();
        if !via.is_empty() && !self.hosts.contains(&via) { self.error = Some(t("kv.err_host").into()); cx.notify(); return; }
        let id = self.id.clone().unwrap_or_else(assets::new_id);
        let set_tags = |tags: &mut Vec<String>, on: bool| { tags.retain(|x| x != &prod_tag); if on { tags.push(prod_tag.clone()); } };
        match self.kind {
            AssetKind::Redis => {
                let port = Self::v(&self.port, cx);
                let port = if port.is_empty() { None } else { match port.parse() { Ok(p) => Some(p), Err(_) => { self.error = Some(t("kv.err_port").into()); cx.notify(); return; } } };
                let db = Self::v(&self.db, cx).parse().unwrap_or(0);
                let mut r = config::get().redis.iter().find(|x| x.id == id).cloned().unwrap_or_default();
                r.id = id.clone(); r.name = Self::v(&self.name, cx); r.mode = self.mode.clone(); r.host = Self::v(&self.host, cx); r.port = port;
                r.nodes = Self::v(&self.nodes, cx); r.master = Self::v(&self.master, cx); r.user = Self::v(&self.user, cx); r.db = db; r.tls = self.tls.clone();
                r.ssh_via = via; r.group = group; r.read_only = self.read_only;
                set_tags(&mut r.tags, self.production);
                if r.host.is_empty() && r.nodes.is_empty() { self.error = Some(t("kv.err_host_empty").into()); cx.notify(); return; }
                let pw = self.password.read(cx).value().to_string();
                if pw.is_empty() { crate::secret_store().delete(&r.password_secret()); } else { let _ = crate::secret_store().set(&r.password_secret(), &pw); }
                let _ = config::update(|s| { match s.redis.iter_mut().find(|x| x.id == r.id) { Some(x) => *x = r.clone(), None => s.redis.push(r.clone()) } let (a, b) = (&mut s.redis, &mut s.docker); assets::normalize_kv_docker(a, b); });
            }
            AssetKind::Docker => {
                let mut d = config::get().docker.iter().find(|x| x.id == id).cloned().unwrap_or_default();
                d.id = id; d.name = Self::v(&self.name, cx); d.endpoint = if via.is_empty() { "local".into() } else { "ssh".into() }; d.ssh_host = via;
                d.socket = Self::v(&self.socket, cx); d.group = group; d.read_only = self.read_only;
                set_tags(&mut d.tags, self.production);
                let _ = config::update(|s| { match s.docker.iter_mut().find(|x| x.id == d.id) { Some(x) => *x = d.clone(), None => s.docker.push(d.clone()) } let (a, b) = (&mut s.redis, &mut s.docker); assets::normalize_kv_docker(a, b); });
            }
        }
        cx.emit(AssetEvent::Saved);
    }
    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.id.clone() else { return };
        let _ = config::update(|s| { s.redis.retain(|x| x.id != id); s.docker.retain(|x| x.id != id); });
        crate::secret_store().delete(&format!("redis:{id}:password"));
        cx.emit(AssetEvent::Saved);
    }
}

impl Render for AssetDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let label = |s: &str| div().w(px(96.)).flex_shrink_0().text_size(px(13.)).text_color(c(th.subtext)).child(s.to_string());
        let line = |l: &str, el: AnyElement| div().flex().items_center().gap_3().child(label(l)).child(div().flex_1().min_w_0().child(el));
        let mut form = div().p_5().flex().flex_col().gap_3().max_h(px(600.)).id("kv-form").overflow_y_scroll();
        form = form.child(line(&t("ui.name"), ui::text_field(&self.name, false).into_any_element()));
        if self.kind == AssetKind::Redis {
            let modes: Vec<(String, String)> = ["standalone", "cluster", "sentinel"].iter().map(|m| (m.to_string(), t(&format!("kv.mode_{m}")).to_string())).collect();
            let me = cx.entity().downgrade();
            form = form.child(line(&t("kv.mode"), ui::segmented("kv-mode", &modes, &self.mode, move |v, _w, cx| { let _ = me.update(cx, |d, cx| { d.mode = v; cx.notify(); }); }).into_any_element()));
            form = form.child(div().flex().items_center().gap_3().child(label(&t("ui.address")))
                .child(div().flex_1().min_w_0().child(ui::text_field(&self.host, false))).child(div().w(px(90.)).child(ui::text_field(&self.port, false))));
            if self.mode != "standalone" { form = form.child(line(&t("kv.nodes"), ui::text_field(&self.nodes, false).into_any_element())); }
            if self.mode == "sentinel" { form = form.child(line(&t("kv.master"), ui::text_field(&self.master, false).into_any_element())); }
            form = form.child(line(&t("kv.acl_user"), ui::text_field(&self.user, false).into_any_element()))
                .child(line(&t("ui.password"), ui::text_field(&self.password, false).into_any_element()))
                .child(line(&t("kv.db_index"), ui::text_field(&self.db, false).into_any_element()));
            let tls: Vec<(String, String)> = ["disable", "require", "insecure"].iter().map(|m| (m.to_string(), t(&format!("kv.tls_{m}")).to_string())).collect();
            let me = cx.entity().downgrade();
            form = form.child(line("TLS", ui::segmented("kv-tls", &tls, &self.tls, move |v, _w, cx| { let _ = me.update(cx, |d, cx| { d.tls = v; cx.notify(); }); }).into_any_element()));
        } else {
            form = form.child(line(&t("kv.socket"), ui::text_field(&self.socket, false).into_any_element()));
        }
        form = form.child(line(&t("kv.via"), self.via.clone().into_any_element()));
        if self.hosts.is_empty() { form = form.child(div().pl(px(108.)).text_size(px(11.)).text_color(c(th.overlay0)).child(t("kv.no_hosts"))); }
        if self.kind == AssetKind::Redis && self.mode != "standalone" { form = form.child(div().pl(px(108.)).text_size(px(11.)).text_color(c(th.warning)).child(t("redis.err_cluster_ssh"))); }
        form = form.child(line(&t("ui.group"), ui::text_field(&self.group, false).into_any_element()))
            .child(div().flex().items_center().gap_3().child(label(&t("tag.production"))).child(Switch::new("kv-prod").checked(self.production).on_click(cx.listener(|d, on: &bool, _, cx| { d.production = *on; cx.notify(); }))))
            .child(div().flex().items_center().gap_3().child(label(&t("db.read_only"))).child(Switch::new("kv-ro").checked(self.read_only).on_click(cx.listener(|d, on: &bool, _, cx| { d.read_only = *on; cx.notify(); }))));
        if let Some(e) = &self.error { form = form.child(div().text_size(px(12.)).text_color(c(th.error)).child(e.clone())); }
        let mut footer = div().px_5().py_3().flex().items_center().gap_2().border_t_1().border_color(c(th.surface0)).bg(c(th.mantle)).child(div().flex_1());
        if self.id.is_some() { footer = footer.child(ui::button("kv-d-del", t("ui.delete"), 2).on_click(cx.listener(|d, _, _w, cx| d.delete(cx)))); }
        footer = footer.child(ui::button("kv-d-cancel", t("ui.cancel"), 0).on_click(cx.listener(|_, _, _w, cx| cx.emit(AssetEvent::Close))))
            .child(ui::button("kv-d-save", t("ui.save"), 1).on_click(cx.listener(|d, _, _w, cx| d.save(cx))));
        let title: SharedString = match self.kind { AssetKind::Redis => t("kv.redis_conn").into(), AssetKind::Docker => t("kv.docker_ep").into() };
        ui::modal(ui::card(580.)
            .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(if self.kind == AssetKind::Redis { IconName::Database } else { IconName::Container }, 16., c(th.accent)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title)))
            .child(form).child(footer))
    }
}

impl Root {
    pub(crate) fn open_asset_dialog(&mut self, kind: AssetKind, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let hosts: Vec<String> = config::get().all_hosts().iter().map(|h| h.name.clone()).collect();
        let d = cx.new(|cx| AssetDialog::new(kind, id, hosts, window, cx));
        self._subs.push(cx.subscribe_in(&d, window, |this, _, ev: &AssetEvent, window, cx| {
            if let AssetEvent::Saved = ev { this.log("asset.save", "redis/docker"); }
            this.close_overlay(window, cx);
        }));
        self.overlay = Overlay::Asset(d);
        cx.notify();
    }

    pub(crate) fn push_pane(&mut self, pane: Entity<TermView>, window: &mut Window, cx: &mut Context<Self>) {
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(Tab { layout: Node::Leaf(pane.clone()), focused: pane, name: None });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        self.overlay = Overlay::None;
        window.focus(&fh, cx);
        cx.notify();
    }
    pub(crate) fn embed_pane(&mut self, e: Embed, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let (h, m) = (self.history_for("local"), self.manifests.clone());
        cx.new(|cx| TermView::new_embed(e, h, m, window, cx))
    }
    fn push_embed(&mut self, e: Embed, window: &mut Window, cx: &mut Context<Self>) { let p = self.embed_pane(e, window, cx); self.push_pane(p, window, cx); }

    pub(crate) fn production_of(tags: &[String], group: &str) -> bool { let s = config::get(); assets::is_production(tags, group, &s.groups, &s.tags.production) }
    /// SSH route to a saved host name ("" = none); Err when the host is gone.
    pub(crate) fn via_for(&self, host: &str) -> Result<Option<(ssh_term_proto::ssh::SshTarget, ssh_term_proto::ssh::Prompts)>, String> {
        if host.is_empty() { return Ok(None); }
        self.target_for(host).map(|t| Some((t, self.prompts.clone()))).ok_or_else(|| ssh_term_proto::i18n::tf("db.via_missing", &[("host", &host)]))
    }
    /// Placeholder leaf for a saved tab whose connection was deleted.
    pub(crate) fn gone_pane(&mut self, what: String, save: Option<ssh_term_proto::session_store::PaneSave>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        use ssh_term_proto::session_store::PaneSave as P;
        let module = match &save { Some(P::Redis { .. }) => crate::activity::Tool::Redis, Some(P::Docker { .. } | P::DockerLogs { .. }) => crate::activity::Tool::Docker, Some(P::Mongo { .. } | P::Db { .. }) => crate::activity::Tool::Database, Some(P::Sftp { .. }) => crate::activity::Tool::Sftp, _ => crate::activity::Tool::Hosts };
        let g = cx.new(|_| GoneView { what, save, module });
        self.embed_pane(Embed::Gone(g), window, cx)
    }

    pub(crate) fn redis_pane(&mut self, id: &str, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let Some(conn) = config::get().redis.iter().find(|r| r.id == id).cloned() else { return self.gone_pane(format!("Redis {id}"), Some(ssh_term_proto::session_store::PaneSave::Redis { conn: id.to_string(), key }), window, cx) };
        let via = if conn.mode == "standalone" || conn.mode.is_empty() { self.via_for(&conn.ssh_via) } else { Ok(None) };
        let pw = crate::secret_store().get(&conn.password_secret()).map(|x| x.0);
        let prod = Self::production_of(&conn.tags, &conn.group_path());
        let (rt, log) = (self.rt.handle().clone(), self.exec_log.clone());
        let v = cx.new(|cx| KvView::new(conn, pw, via, key, prod, rt, log, window, cx));
        self.embed_pane(Embed::Kv(v), window, cx)
    }
    pub(crate) fn open_redis_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) { let p = self.redis_pane(id, None, window, cx); self.push_pane(p, window, cx); }
    /// Redis tab on logical database `db` (default: the connection's own).
    pub(crate) fn redis_pane_on(&mut self, id: &str, db: Option<u32>, key: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let Some(mut conn) = config::get().redis.iter().find(|r| r.id == id).cloned() else { return self.redis_pane(id, key, window, cx) };
        if let Some(n) = db { conn.db = n; }
        let via = if conn.mode == "standalone" || conn.mode.is_empty() { self.via_for(&conn.ssh_via) } else { Ok(None) };
        let pw = crate::secret_store().get(&conn.password_secret()).map(|x| x.0);
        let prod = Self::production_of(&conn.tags, &conn.group_path());
        let (rt, log) = (self.rt.handle().clone(), self.exec_log.clone());
        let v = cx.new(|cx| KvView::new(conn, pw, via, key, prod, rt, log, window, cx));
        self.embed_pane(Embed::Kv(v), window, cx)
    }

    /// MongoDB tab for a saved DB connection; `start` = (database, collection, filter, sort).
    pub(crate) fn mongo_pane(&mut self, id: &str, start: (String, String, String, String), window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        use ssh_term_proto::session_store::PaneSave;
        let Some(conn) = config::get().db.iter().find(|r| r.id == id).cloned() else {
            let (database, collection, filter, sort) = start;
            return self.gone_pane(format!("MongoDB {id}"), Some(PaneSave::Mongo { conn: id.to_string(), database, collection, filter, sort }), window, cx);
        };
        let uri = conn.host.contains("://");
        let via = if uri { Ok(None) } else { self.via_for(&conn.ssh_via) };
        let pw = crate::secret_store().get(&conn.password_secret()).map(|x| x.0);
        let prod = Self::production_of(&conn.tags, &conn.group_path());
        let (rt, log) = (self.rt.handle().clone(), self.exec_log.clone());
        let v = cx.new(|cx| crate::doc_view::DocView::new(conn, pw, via, prod, start, rt, log, window, cx));
        self.embed_pane(Embed::Mongo(v), window, cx)
    }
    pub(crate) fn open_mongo_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) { let p = self.mongo_pane(id, Default::default(), window, cx); self.push_pane(p, window, cx); }

    pub(crate) fn docker_pane(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let Some(ep) = config::get().docker.iter().find(|r| r.id == id).cloned() else { return self.gone_pane(format!("Docker {id}"), Some(ssh_term_proto::session_store::PaneSave::Docker { endpoint: id.to_string() }), window, cx) };
        let via = if ep.is_ssh() { self.via_for(&ep.ssh_host).and_then(|v| v.ok_or_else(|| t("kv.err_host").to_string())).map(Some) } else { Ok(None) };
        let prod = Self::production_of(&ep.tags, &ep.group_path());
        let (rt, log) = (self.rt.handle().clone(), self.exec_log.clone());
        let v = cx.new(|cx| DockView::new(ep.clone(), via, prod, rt, log, window, cx));
        self._subs.push(cx.subscribe_in(&v, window, move |this, v, ev: &DockEvent, window, cx| match ev {
            DockEvent::Logs(cid, name) => {
                let (d, rt) = (v.read(cx).dock.clone(), v.read(cx).rt());
                let (cid, name, eid) = (cid.clone(), name.clone(), ep.id.clone());
                let l = cx.new(|cx| LogView::new(d, None, Ok(None), eid, rt, cid, name, window, cx));
                this.push_embed(Embed::Log(l), window, cx);
            }
            DockEvent::Exec(cid) => this.docker_exec(&ep, cid, window, cx),
        }));
        self.embed_pane(Embed::Dock(v), window, cx)
    }
    pub(crate) fn open_docker_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) { let p = self.docker_pane(id, window, cx); self.push_pane(p, window, cx); }
    pub(crate) fn docker_logs_pane(&mut self, endpoint: &str, container: String, name: String, window: &mut Window, cx: &mut Context<Self>) -> Entity<TermView> {
        let ep = config::get().docker.iter().find(|r| r.id == endpoint).cloned();
        let via = match &ep { Some(e) if e.is_ssh() => self.via_for(&e.ssh_host), _ => Ok(None) };
        let rt = self.rt.handle().clone();
        let eid = endpoint.to_string();
        let l = cx.new(|cx| LogView::new(None, ep, via, eid, rt, container, name, window, cx));
        self.embed_pane(Embed::Log(l), window, cx)
    }

    /// Interactive shell in a container: a terminal tab (local or on the SSH host) running `docker exec -it`.
    fn docker_exec(&mut self, ep: &DockerEndpoint, cid: &str, window: &mut Window, cx: &mut Context<Self>) {
        let spec = if ep.is_ssh() { match self.target_for(&ep.ssh_host) { Some(t) => Spec::Ssh(t), None => { self.toast(t("kv.err_host").into(), cx); return; } } } else { Spec::Local };
        let socket = (!ep.socket.is_empty() && ep.socket != "/var/run/docker.sock").then_some(ep.socket.as_str());
        let cmd = ssh_term_proto::dock::exec_command(cid, socket);
        let pane = self.open_pane(spec, window, cx);
        pane.update(cx, |v, cx| { v.ws = Some(crate::activity::Tool::Docker); v.queue_cd(cmd, cx) });
        let fh = pane.read(cx).focus.clone();
        self.tabs.push(Tab { layout: Node::Leaf(pane.clone()), focused: pane, name: Some(format!("exec {}", cid.chars().take(12).collect::<String>())) });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        window.focus(&fh, cx);
        cx.notify();
    }

    /// Tool window body for Redis or containers: saved entries grouped by group path.
    pub(crate) fn asset_panel(&mut self, kind: AssetKind, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let items: Vec<(String, String, String, String, bool)> = match kind {
            AssetKind::Redis => s.redis.iter().map(|r| (r.id.clone(), r.name.clone(), format!("{}{}", r.address(), if r.ssh_via.is_empty() { String::new() } else { format!(" ⇢ {}", r.ssh_via) }), r.group_path(), Self::production_of(&r.tags, &r.group_path()))).collect(),
            AssetKind::Docker => s.docker.iter().map(|d| (d.id.clone(), d.name.clone(), if d.is_ssh() { format!("ssh ⇢ {}", d.ssh_host) } else { t("docker.local").to_string() }, d.group_path(), Self::production_of(&d.tags, &d.group_path()))).collect(),
        };
        let (title, ic) = match kind { AssetKind::Redis => (t("tool.redis"), IconName::Database), AssetKind::Docker => (t("tool.docker"), IconName::Container) };
        let add_tip = match kind { AssetKind::Redis => t("kv.add_redis"), AssetKind::Docker => t("kv.add_docker") };
        let add = ui::icon_button("kv-add", IconName::Plus, ui::IconBtn::Md, add_tip)
            .on_click(cx.listener(move |this, _, w, cx| this.open_asset_dialog(kind, None, w, cx))).into_any_element();
        let mut col = div().flex_1().min_h_0().flex().flex_col().child(ui::panel_header(title, None, None, vec![add]));
        if items.is_empty() {
            let (msg, btn) = match kind { AssetKind::Redis => (t("kv.redis_empty"), t("kv.redis_conn")), AssetKind::Docker => (t("kv.docker_empty"), t("kv.docker_ep")) };
            return col.child(ui::empty_state(ic, msg, None, vec![
                ui::button("kv-empty-add", btn, 1).w_full().on_click(cx.listener(move |this, _, w, cx| this.open_asset_dialog(kind, None, w, cx))).into_any_element(),
            ], false)).into_any_element();
        }
        if kind == AssetKind::Redis {
            col = col.child(self.kv_tree_list(cx));
            if let Some((k, pos)) = self.kvp.menu.clone() { col = col.child(self.kv_menu_el(k, pos, cx)); }
            return col.into_any_element();
        }
        let mut sorted = items;
        sorted.sort_by(|a, b| (a.3.clone(), a.1.to_lowercase()).cmp(&(b.3.clone(), b.1.to_lowercase())));
        let mut list = div().id("kv-list").flex_1().min_h_0().overflow_y_scroll().pb(px(ui::SP_2)).flex().flex_col();
        let mut last: Option<String> = None;
        for (i, (id, name, addr, group, prod)) in sorted.into_iter().enumerate() {
            if last.as_ref() != Some(&group) && !group.is_empty() {
                list = list.child(ui::section_header(SharedString::from(format!("kvg:{group}")), group.clone(), None, None));
            }
            last = Some(group.clone());
            let (oid, eid) = (id.clone(), id.clone());
            let gid = SharedString::from(format!("kv-it-g-{i}"));
            list = list.child(ui::list_row(("kv-it", i), 0, false).min_h(px(ui::row2_h())).group(gid.clone())
                .on_click(cx.listener(move |this, _, w, cx| match kind { AssetKind::Redis => this.open_redis_tab(&oid, w, cx), AssetKind::Docker => this.open_docker_tab(&oid, w, cx) }))
                .child(icon(ic, ui::IC_MD, c(if prod { th.error } else { th.overlay1 })))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(ui::row_title(name))
                    .child(ui::row_meta(addr)))
                .child(ui::row_actions(gid)
                    .child(ui::icon_button(("kv-edit", i), IconName::Pencil, ui::IconBtn::Sm, t("menu.edit"))
                        .on_click(cx.listener(move |this, _, w, cx| { cx.stop_propagation(); this.open_asset_dialog(kind, Some(eid.clone()), w, cx); })))));
        }
        col = col.child(list);
        col.into_any_element()
    }
}

/// Shown for a restored tab whose saved connection no longer exists.
/// Placeholder for a saved tab whose connection no longer exists; keeps the saved form so
/// the next layout save does not drop it.
pub struct GoneView { pub what: String, pub save: Option<ssh_term_proto::session_store::PaneSave>, pub module: crate::activity::Tool }
impl Render for GoneView {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        div().size_full().flex().flex_col().items_center().justify_center().gap_2().bg(c(th.base))
            .child(icon(IconName::TriangleAlert, 20., c(th.warning)))
            .child(div().text_size(px(13.)).text_color(c(th.text)).child(t("kv.gone")))
            .child(div().text_size(px(12.)).text_color(c(th.overlay0)).child(self.what.clone()))
    }
}
