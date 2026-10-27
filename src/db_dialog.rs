//! New / edit database connection dialog (v0.2 PRD §6.2). The connection goes to
//! config.toml `[[db]]`; its password goes to the secret store (keychain / secrets.enc).
//! A connection can go through any saved SSH host; "Test" runs the unsaved form.
use std::collections::HashMap;

use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, SharedString, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::switch::Switch;
use ssh_term_proto::assets::{self, DbConn, DbKind};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh::{Prompts, SshTarget};

use crate::config;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum DbDialogEvent { Saved(String), Deleted(String), Close }

const TLS_MODES: [&str; 5] = ["prefer", "disable", "require", "verify-ca", "verify-full"];

pub struct DbDialog {
    editing: Option<DbConn>,
    kind: DbKind,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    file: Entity<InputState>,
    group: Entity<InputState>,
    uri: Entity<InputState>,
    tls: String,
    read_only: bool,
    via: Entity<crate::host_picker::HostPicker>,
    targets: HashMap<String, SshTarget>,
    prompts: Prompts,
    rt: tokio::runtime::Handle,
    error: Option<String>,
    /// None = not run; Some(None) = running; Some(Some(Ok/Err)).
    test: Option<Option<Result<String, String>>>,
    test_task: Option<tokio::task::JoinHandle<()>>,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<DbDialogEvent> for DbDialog {}

impl DbDialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(existing: Option<DbConn>, group: Option<String>, hosts: Vec<String>, targets: HashMap<String, SshTarget>, prompts: Prompts, rt: tokio::runtime::Handle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut e = existing.clone().unwrap_or_default();
        if existing.is_none() { e.group = group; }
        let mk = |ph: String, v: String, masked: bool, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(ph).default_value(v));
        let saved_pw = existing.as_ref().and_then(|d| crate::secret_store().get(&d.password_secret())).map(|x| x.0).unwrap_or_default();
        let name = mk(t("db.name_placeholder").into(), e.name.clone(), false, window, cx);
        let host = mk("127.0.0.1".into(), e.host.clone(), false, window, cx);
        let port = mk(e.kind.default_port().map(|p| p.to_string()).unwrap_or_default(), e.port.map(|p| p.to_string()).unwrap_or_default(), false, window, cx);
        let user = mk("root".into(), e.user.clone(), false, window, cx);
        let password = mk(t("ui.password").into(), saved_pw, true, window, cx);
        let database = mk(t("db.database_placeholder").into(), e.database.clone(), false, window, cx);
        let file = mk("/path/to/data.db".into(), e.file.clone(), false, window, cx);
        let group_in = mk(t("ui.e_g_prod_test").into(), e.group.clone().unwrap_or_default(), false, window, cx);
        let uri = mk("mysql://user:pass@host:3306/db".into(), String::new(), false, window, cx);
        name.update(cx, |s, cx| s.focus(window, cx));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&uri, window, |this: &mut Self, _, ev: &InputEvent, window, cx| { if matches!(ev, InputEvent::Change) { this.apply_uri(window, cx); } }));
        let via = cx.new(|cx| crate::host_picker::HostPicker::new("db-via", e.ssh_via.clone(), hosts.clone(), window, cx));
        subs.push(cx.subscribe(&via, |this: &mut Self, _, _: &crate::host_picker::HostChanged, cx| { this.test = None; cx.notify(); }));
        let tls = if e.tls.is_empty() { "prefer".to_string() } else { e.tls.clone() };
        Self { kind: e.kind, name, host, port, user, password, database, file, group: group_in, uri, tls, read_only: e.read_only, via,
            targets, prompts, rt, error: None, test: None, test_task: None, editing: existing, _subs: subs }
    }

    fn v(e: &Entity<InputState>, cx: &Context<Self>) -> String { e.read(cx).value().trim().to_string() }

    /// A pasted `scheme://user:pass@host:port/db` fills the form.
    fn apply_uri(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = Self::v(&self.uri, cx);
        let set = |e: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| e.update(cx, |s, cx| s.set_value(v, window, cx));
        if let Some((uri, user, pass, db)) = ssh_term_proto::doc::split_uri(&raw) {
            // MongoDB keeps the whole URI (SRV, replica sets, options) in the host field.
            self.kind = DbKind::Mongodb;
            set(&self.host.clone(), uri, window, cx);
            set(&self.port.clone(), String::new(), window, cx);
            set(&self.user.clone(), user, window, cx);
            set(&self.database.clone(), db, window, cx);
            if let Some(p) = pass { set(&self.password.clone(), p, window, cx); }
            cx.notify();
            return;
        }
        let Some((c, pass)) = assets::parse_uri(&raw) else { return };
        self.kind = c.kind;
        let set = |e: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| e.update(cx, |s, cx| s.set_value(v, window, cx));
        if c.kind.is_file() { set(&self.file.clone(), c.file, window, cx); } else {
            set(&self.host.clone(), c.host.clone(), window, cx);
            set(&self.port.clone(), c.port.map(|p| p.to_string()).unwrap_or_default(), window, cx);
            set(&self.user.clone(), c.user, window, cx);
            set(&self.database.clone(), c.database, window, cx);
            if let Some(p) = pass { set(&self.password.clone(), p, window, cx); }
            if Self::v(&self.name, cx).is_empty() { set(&self.name.clone(), c.host, window, cx); }
        }
        cx.notify();
    }

    /// Connection from the form (not saved) plus the typed password.
    fn conn(&mut self, cx: &mut Context<Self>) -> Option<(DbConn, String)> {
        let mut d = self.editing.clone().unwrap_or_default();
        d.kind = self.kind;
        d.name = Self::v(&self.name, cx);
        let fail = |this: &mut Self, m: String, cx: &mut Context<Self>| { this.error = Some(m); cx.notify(); };
        if self.kind.is_file() {
            d.file = Self::v(&self.file, cx);
            if d.file.is_empty() { fail(self, t("db.err_file").into(), cx); return None; }
            d.host.clear(); d.port = None; d.user.clear(); d.ssh_via.clear();
            if d.name.is_empty() { d.name = std::path::Path::new(&d.file).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(); }
        } else {
            d.host = Self::v(&self.host, cx);
            if d.host.is_empty() { fail(self, t("db.err_host").into(), cx); return None; }
            let p = Self::v(&self.port, cx);
            d.port = if p.is_empty() { None } else { match p.parse::<u16>() { Ok(p) if p > 0 => Some(p), _ => { fail(self, t("ui.port_must_be_1_65535").into(), cx); return None; } } };
            d.user = Self::v(&self.user, cx);
            d.database = Self::v(&self.database, cx);
            d.file.clear();
            d.ssh_via = self.via.read(cx).value.clone();
            d.tls = if self.tls == "prefer" { String::new() } else { self.tls.clone() };
            if d.kind == DbKind::Mongodb && d.host.contains("://") {
                d.port = None;
                d.ssh_via.clear(); // a URI may name several hosts / SRV records; tunnels only work for one standalone host
            }
            if d.name.is_empty() { d.name = ssh_term_proto::doc::split_uri(&d.host).map(|x| x.0.split_once("://").map(|y| y.1.split(['/', '?']).next().unwrap_or("").to_string()).unwrap_or_default()).unwrap_or_else(|| d.host.clone()); }
        }
        let g = Self::v(&self.group, cx);
        d.group = (!g.is_empty()).then(|| ssh_term_proto::outline::normalize(&g)).filter(|g| !g.is_empty());
        d.read_only = self.read_only;
        if d.id.is_empty() { d.id = assets::new_id(); }
        let pw = self.password.read(cx).value().to_string();
        Some((d, pw))
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some((d, pw)) = self.conn(cx) else { return };
        let store = crate::secret_store();
        if pw.is_empty() { store.delete(&d.password_secret()); } else if let Err(e) = store.set(&d.password_secret(), &pw) { self.error = Some(format!("{e:#}")); cx.notify(); return; }
        let id = d.id.clone();
        let r = config::update(|s| {
            match s.db.iter_mut().find(|x| x.id == d.id) { Some(x) => *x = d.clone(), None => s.db.push(d.clone()) }
            assets::ensure_asset_groups(&s.db, &mut s.groups);
        });
        if let Some(e) = r { self.error = Some(e); cx.notify(); return; }
        cx.emit(DbDialogEvent::Saved(id));
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        let Some((d, pw)) = self.conn(cx) else { return };
        let via = if d.ssh_via.is_empty() { None } else {
            match self.targets.get(&d.ssh_via) { Some(t) => Some((t.clone(), self.prompts.clone())), None => { self.test = Some(Some(Err(tf("db.via_missing", &[("host", &d.ssh_via)])))); cx.notify(); return; } }
        };
        if !d.kind.dialog_ready() { self.test = Some(Some(Err(tf("db.kind_unsupported", &[("kind", &d.kind.label())])))); cx.notify(); return; }
        self.error = None;
        self.test = Some(None);
        let me = cx.entity().downgrade();
        let pw = (!pw.is_empty()).then_some(pw);
        let started = std::time::Instant::now();
        let h = self.rt.spawn(async move {
            if d.kind == DbKind::Mongodb {
                let m = ssh_term_proto::doc::Mongo::open(&d, pw, via).await?;
                let r = (m.version.clone(), m.via.clone());
                m.close().await;
                return Ok::<_, anyhow::Error>(r);
            }
            let l = ssh_term_proto::db::live::open(&d, pw, via).await?;
            let v = l.version.clone();
            let via = l.via.clone();
            l.close().await;
            Ok::<_, anyhow::Error>((v, via))
        });
        let task = cx.spawn(async move |_, cx| {
            let r = h.await.map_err(anyhow::Error::from).and_then(|r| r);
            let ms = started.elapsed().as_millis();
            let _ = me.update(cx, |this, cx| {
                this.test_task = None;
                this.test = Some(Some(match r {
                    Ok((v, via)) => Ok(match via { Some(h) => tf("db.test_ok_via", &[("version", &v), ("ms", &ms), ("host", &h)]), None => tf("db.test_ok", &[("version", &v), ("ms", &ms)]) }),
                    Err(e) => Err(format!("{e:#}")),
                }));
                cx.notify();
            });
        });
        task.detach();
        cx.notify();
    }
}

impl Render for DbDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let label = |s: &str| div().w(px(84.)).flex_shrink_0().text_size(px(ui::fs_body())).text_color(c(th.subtext)).child(s.to_string());
        let line = |l: &str, el: gpui::AnyElement| div().flex().items_center().gap_3().child(label(l)).child(div().flex_1().min_w_0().child(el));
        let mut form = div().p_5().flex().flex_col().gap_3().max_h(px(600.)).id("db-form").overflow_y_scroll();

        // engine
        let kinds: Vec<(String, String)> = DbKind::ALL.iter().filter(|k| k.dialog_ready()).map(|k| (k.id().to_string(), k.label().to_string())).collect();
        let me = cx.entity().downgrade();
        form = form.child(div().flex().items_start().gap_3().child(label(t("db.kind")).pt_1())
            .child(div().flex_1().flex().flex_col().gap_1()
                .child(ui::segmented("db-kind", &kinds, self.kind.id(), move |v, _w, cx| { if let Some(k) = DbKind::from_id(&v) { let _ = me.update(cx, |this, cx| { this.kind = k; this.test = None; cx.notify(); }); } }))
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(t("db.kinds_more")))));
        form = form.child(line(t("db.uri"), ui::text_field(&self.uri, false).into_any_element()));
        form = form.child(div().h(px(1.)).bg(crate::theme::divider()));
        form = form.child(line(t("ui.name"), ui::text_field(&self.name, false).into_any_element()));
        if self.kind.is_file() {
            form = form.child(line(t("db.file"), ui::text_field(&self.file, false).into_any_element()));
        } else {
            form = form.child(div().flex().items_center().gap_3().child(label(t("ui.address")))
                .child(div().flex_1().min_w_0().child(ui::text_field(&self.host, false)))
                .child(div().w(px(90.)).flex_shrink_0().child(ui::text_field(&self.port, false))));
            form = form.child(line(t("host.username"), ui::text_field(&self.user, false).into_any_element()));
            form = form.child(line(t("ui.password"), ui::text_field(&self.password, false).into_any_element()));
            form = form.child(line(t("db.database"), ui::text_field(&self.database, false).into_any_element()));
            form = form.child(line(t("db.via"), self.via.clone().into_any_element()));
            let tls: Vec<(String, String)> = TLS_MODES.iter().map(|m| (m.to_string(), t(&format!("db.tls_{}", m.replace('-', "_"))).to_string())).collect();
            let me = cx.entity().downgrade();
            form = form.child(div().flex().items_center().gap_3().child(label("TLS"))
                .child(div().flex_1().child(ui::segmented("db-tls", &tls, &self.tls, move |v, _w, cx| { let _ = me.update(cx, |this, cx| { this.tls = v; cx.notify(); }); }))));
        }
        form = form.child(line(t("ui.group"), ui::text_field(&self.group, false).into_any_element()));
        form = form.child(div().flex().items_center().gap_3().child(label(t("db.read_only")))
            .child(Switch::new("db-ro").checked(self.read_only).on_click(cx.listener(|this, on: &bool, _, cx| { this.read_only = *on; cx.notify(); })))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(t("db.read_only_hint"))));
        if let Some(r) = &self.test {
            let (col, ic, msg) = match r { None => (th.overlay1, IconName::LoaderCircle, t("db.testing").to_string()), Some(Ok(m)) => (th.success, IconName::CircleCheck, m.clone()), Some(Err(e)) => (th.error, IconName::CircleX, e.clone()) };
            form = form.child(div().p_3().rounded(px(ui::R_SUB)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider()).flex().items_start().gap_2()
                .child(icon(ic, ui::IC_SM, c(col))).child(div().flex_1().min_w_0().whitespace_normal().text_size(px(ui::fs_body())).text_color(c(col)).child(msg)));
        }
        if let Some(e) = &self.error { form = form.child(div().text_size(px(ui::fs_body())).text_color(c(th.error)).child(e.clone())); }

        let testing = matches!(self.test, Some(None));
        let test_btn = ui::button("db-test", t("host.test"), 0).when(testing, |b| b.opacity(0.5)).on_click(cx.listener(move |this, _, _w, cx| if !testing { this.run_test(cx) }));
        let mut btns: Vec<gpui::AnyElement> = Vec::new();
        if let Some(d) = self.editing.clone() {
            btns.push(ui::button("db-del", t("ui.delete"), 2).on_click(cx.listener(move |_, _, _w, cx| {
                let id = d.id.clone();
                crate::secret_store().delete(&d.password_secret());
                let _ = config::update(|s| s.db.retain(|x| x.id != id));
                cx.emit(DbDialogEvent::Deleted(id));
            })).into_any_element());
        }
        btns.push(ui::button("db-cancel", t("ui.cancel"), 0).on_click(cx.listener(|_, _, _w, cx| cx.emit(DbDialogEvent::Close))).into_any_element());
        btns.push(ui::button("db-save", t("ui.save"), 1).on_click(cx.listener(|this, _, _w, cx| this.save(cx))).into_any_element());
        let footer = ui::dialog_footer(Some(test_btn.into_any_element()), btns);
        let title: SharedString = if self.editing.is_some() { t("db.edit_connection").into() } else { t("db.new_connection").into() };
        ui::modal(ui::card(600.)
            .child(ui::dialog_header(Some(IconName::Database), c(cur().accent), title, None, None))
            .child(form).child(footer))
    }
}
