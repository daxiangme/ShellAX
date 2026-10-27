//! New / edit host dialog (PRD 5.24). Saves into config.toml [[hosts]]; the password and key
//! passphrases go to the secret store (keychain / secrets.enc), never into config.toml.
//! Credentials: username + an ordered list of auth methods (password by default; private key,
//! certificate, ssh-agent can be added). "Test connection" runs the unsaved form step by step.
use ssh_term_proto::i18n::{t, tf};
use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, FontWeight, SharedString, Window};
use gpui_component::input::{InputState};
use gpui_component::switch::Switch;
use ssh_term_proto::hosts::HostEntry;
use ssh_term_proto::ssh;

use crate::config;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum HostEvent { Saved(String), Deleted(String), Close, OpenProxySettings }

#[derive(Clone, Copy, PartialEq, Debug)]
enum Method { Password, Key, Cert, Agent }
impl Method {
    fn id(self) -> &'static str { match self { Method::Password => "password", Method::Key => "key", Method::Cert => "cert", Method::Agent => "agent" } }
    fn parse(s: &str) -> Option<Self> { match s { "password" | "kbd" => Some(Method::Password), "key" => Some(Method::Key), "cert" => Some(Method::Cert), "agent" => Some(Method::Agent), _ => None } }
    fn label(self) -> &'static str { match self { Method::Password => t("ui.password"), Method::Key => t("ui.private_key"), Method::Cert => t("host.cert"), Method::Agent => "ssh-agent" } }
}

/// Per-step state of "Test connection".
#[derive(Clone, Default)]
struct TestRow { ok: Option<bool>, ms: u128, detail: String, started: bool }

pub struct HostDialog {
    editing: Option<String>,
    fields: Vec<(&'static str, &'static str, Entity<InputState>)>,
    password: Entity<InputState>,
    key_path: Entity<InputState>,
    passphrase: Entity<InputState>,
    cert_path: Entity<InputState>,
    methods: Vec<Method>,
    use_proxy: bool,
    add_menu: bool,
    /// Open the list of keys already in ~/.ssh.
    key_menu: bool,
    tags: Vec<String>,
    error: Option<String>,
    prompts: ssh::Prompts,
    rt: tokio::runtime::Handle,
    test: Option<[TestRow; 4]>,
    test_task: Option<tokio::task::JoinHandle<()>>,
}

impl EventEmitter<HostEvent> for HostDialog {}

/// Secret-store name of a host's password (same key the SSH layer reads).
fn password_name(e: &HostEntry) -> String {
    let s = config::get();
    ssh_term_proto::hosts::resolve(e, &s.known_hosts_path(), &s.all_hosts(), &s.ssh_opts()).password_secret()
}

impl HostDialog {
    /// `group`: preset group for a new connection ("New connection here").
    pub fn new(existing: Option<HostEntry>, group: Option<String>, prompts: ssh::Prompts, rt: tokio::runtime::Handle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut e = existing.clone().unwrap_or_default();
        if existing.is_none() { e.group = group; }
        let mk = |ph: &str, v: String, masked: bool, window: &mut Window, cx: &mut Context<Self>| { let ph = ph.to_string(); cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(ph).default_value(v)) };
        let fields = vec![
            (t("ui.name"), "name", mk(t("ui.e_g_prod_web_1"), e.name.clone(), false, window, cx)),
            (t("ui.address"), "host", mk(t("ui.hostname_or_ip"), e.host.clone(), false, window, cx)),
            (t("ui.port"), "port", mk("22", e.port.map(|p| p.to_string()).unwrap_or_default(), false, window, cx)),
            (t("ui.group"), "group", mk(t("ui.e_g_prod_test"), e.group.clone().unwrap_or_default(), false, window, cx)),
            (t("ui.jump_host"), "jump", mk(t("ui.jump_placeholder"), e.jump.clone().unwrap_or_default(), false, window, cx)),
            (t("host.username"), "user", mk(t("ui.empty_uses_the_default_user"), e.user.clone().unwrap_or_default(), false, window, cx)),
        ];
        fields[0].2.update(cx, |s, cx| s.focus(window, cx));
        let store = ssh::SECRETS.get();
        let saved_pw = (existing.is_some()).then(|| store.and_then(|s| s.get(&password_name(&e))).map(|x| x.0)).flatten().unwrap_or_default();
        let key = e.identity_file.clone().unwrap_or_default();
        let saved_pp = (!key.is_empty()).then(|| store.and_then(|s| s.get(&ssh::passphrase_secret(&ssh_term_proto::hosts::expand_tilde(&key)))).map(|x| x.0)).flatten().unwrap_or_default();
        let password = mk(t("ui.password"), saved_pw, true, window, cx);
        let key_path = mk("~/.ssh/id_ed25519", key.clone(), false, window, cx);
        let passphrase = mk(t("host.passphrase_optional"), saved_pp, true, window, cx);
        let cert_path = mk("~/.ssh/id_ed25519-cert.pub", e.cert.clone().unwrap_or_default(), false, window, cx);
        // auth order: explicit list, or a sensible list for the legacy single values
        let auth = e.auth.clone().unwrap_or_default();
        let mut methods: Vec<Method> = match auth.as_str() {
            "" | "auto" | "auto-noagent" => if key.is_empty() { vec![Method::Password] } else { vec![Method::Key, Method::Password] },
            a => a.split(',').filter_map(|m| Method::parse(m.trim())).collect(),
        };
        methods.dedup();
        if methods.is_empty() { methods.push(Method::Password); }
        Self { editing: existing.map(|h| h.name), fields, password, key_path, passphrase, cert_path, methods, use_proxy: e.use_proxy, add_menu: false, key_menu: false, tags: e.tags.clone(), error: None, prompts, rt, test: None, test_task: None }
    }

    fn val(&self, key: &str, cx: &Context<Self>) -> String {
        self.fields.iter().find(|f| f.1 == key).map(|f| f.2.read(cx).value().trim().to_string()).unwrap_or_default()
    }
    fn v(e: &Entity<InputState>, cx: &Context<Self>) -> String { e.read(cx).value().to_string() }

    /// HostEntry from the current form (not saved).
    fn entry(&mut self, cx: &mut Context<Self>) -> Option<HostEntry> {
        let name = self.val("name", cx);
        let host = self.val("host", cx);
        if name.is_empty() || host.is_empty() { self.error = Some(t("ui.name_and_address_are_required").into()); cx.notify(); return None; }
        let port = self.val("port", cx);
        let port = if port.is_empty() { None } else { match port.parse::<u16>() { Ok(p) if p > 0 => Some(p), _ => { self.error = Some(t("ui.port_must_be_1_65535").into()); cx.notify(); return None; } } };
        let opt = |s: String| { let s = s.trim().to_string(); (!s.is_empty()).then_some(s) };
        let uses = |m: Method| self.methods.contains(&m);
        let key = if uses(Method::Key) || uses(Method::Cert) { opt(Self::v(&self.key_path, cx)) } else { None };
        let cert = if uses(Method::Cert) { opt(Self::v(&self.cert_path, cx)) } else { None };
        let auth = self.methods.iter().map(|m| m.id()).collect::<Vec<_>>().join(",");
        // 图标由侧栏「更换图标」维护，编辑连接时原样保留
        let (icon, icon_color) = self.editing.as_ref().and_then(|n| config::get().hosts.iter().find(|h| &h.name == n).map(|h| (h.icon.clone(), h.icon_color.clone()))).unwrap_or_default();
        Some(HostEntry { icon, icon_color, name, host, port, user: opt(self.val("user", cx)), identity_file: key, cert,
            group: opt(self.val("group", cx)).map(|g| ssh_term_proto::outline::normalize(&g)).filter(|g| !g.is_empty()), auth: Some(auth), production: false, tags: self.tags.clone(), jump: opt(self.val("jump", cx)), use_proxy: self.use_proxy, source: "config" })
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(cx) else { return };
        let name = entry.name.clone();
        let editing = self.editing.clone();
        let s = config::get();
        if editing.as_deref() != Some(name.as_str()) && s.hosts.iter().any(|h| h.name == name) { self.error = Some(tf("host.exists", &[("name", &name)])); cx.notify(); return; }
        // secrets first (keychain / secrets.enc), then config.toml without them
        if let Some(store) = ssh::SECRETS.get() {
            let pw_name = password_name(&entry);
            let pw = Self::v(&self.password, cx);
            if self.methods.contains(&Method::Password) && !pw.is_empty() { if let Err(e) = store.set(&pw_name, &pw) { self.error = Some(format!("{e:#}")); cx.notify(); return; } }
            else if pw.is_empty() { store.delete(&pw_name); }
            if let Some(k) = &entry.identity_file {
                let pp = Self::v(&self.passphrase, cx);
                let pn = ssh::passphrase_secret(&ssh_term_proto::hosts::expand_tilde(k));
                if !pp.is_empty() { let _ = store.set(&pn, &pp); } else { store.delete(&pn); }
            }
        }
        if let Some(e) = config::update(|s| {
            match editing.and_then(|old| s.hosts.iter().position(|h| h.name == old)) { Some(i) => s.hosts[i] = entry, None => s.hosts.push(entry) }
            ssh_term_proto::hosttree::ensure_groups(&s.hosts, &mut s.groups);
        }) { self.error = Some(e); cx.notify(); return; }
        cx.emit(HostEvent::Saved(name));
    }

    /// DNS → TCP → SSH handshake (+ host key) → auth with the unsaved form values; 10 s per step.
    fn run_test(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = self.test_task.take() { h.abort(); }
        let Some(entry) = self.entry(cx) else { return };
        self.error = None;
        let s = config::get();
        let mut tg = ssh_term_proto::hosts::resolve(&entry, &s.known_hosts_path(), &s.all_hosts(), &s.ssh_opts());
        if let Some(msg) = tg.proxy_block_message() { self.error = Some(msg); cx.notify(); return; }
        tg.auth = entry.auth.clone().unwrap_or_default();
        let pw = Self::v(&self.password, cx);
        if self.methods.contains(&Method::Password) && !pw.is_empty() { tg.password_override = Some(pw); }
        let pp = Self::v(&self.passphrase, cx);
        if !pp.is_empty() { tg.passphrase_override = Some(pp); }
        self.test = Some(Default::default());
        let (tx, rx) = flume::unbounded::<ssh::TestStep>();
        let prompts = self.prompts.clone();
        self.test_task = Some(self.rt.spawn(async move { ssh::test_connection(&tg, &prompts, 10, tx).await; }));
        cx.spawn(async move |this, cx| {
            while let Ok(st) = rx.recv_async().await {
                if this.update(cx, |this, cx| {
                    if let Some(rows) = this.test.as_mut() { rows[st.idx as usize] = TestRow { ok: st.ok, ms: st.ms, detail: st.detail, started: true }; }
                    cx.notify();
                }).is_err() { break; }
            }
            let _ = this.update(cx, |this, cx| { this.test_task = None; cx.notify(); });
        }).detach();
        cx.notify();
    }
    fn cancel_test(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = self.test_task.take() { h.abort(); }
        if let Some(rows) = self.test.as_mut() { for r in rows.iter_mut().filter(|r| r.started && r.ok.is_none()) { r.ok = Some(false); r.detail = t("host.test_cancelled").into(); } }
        cx.notify();
    }
}

impl Drop for HostDialog { fn drop(&mut self) { if let Some(h) = self.test_task.take() { h.abort(); } } }

fn same_key(a: &str, b: &str) -> bool {
    let norm = |s: &str| ssh_term_proto::hosts::expand_tilde(s.trim()).display().to_string().replace('\\', "/").to_lowercase();
    !a.trim().is_empty() && norm(a) == norm(b)
}

impl Render for HostDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let label = |t: &str| div().w(px(76.)).flex_shrink_0().text_size(px(ui::fs_body())).text_color(c(th.subtext)).child(t.to_string());
        let mut form = div().p_5().flex().flex_col().gap_3().max_h(px(560.)).id("host-form").overflow_y_scroll();
        for (l, key, e) in self.fields.iter() {
            if *key == "user" { break; }
            form = form.child(div().flex().items_center().gap_3().child(label(l)).child(div().flex_1().child(ui::text_field(e, false))));
        }
        // ---- credentials card
        let user = self.fields.iter().find(|f| f.1 == "user").map(|f| f.2.clone()).unwrap();
        let mut cred = div().flex().flex_col().gap_2().p_3().rounded(px(ui::R_SUB)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider())
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("host.credentials")))
            .child(div().flex().items_center().gap_3().child(label(t("host.username"))).child(div().flex_1().child(ui::text_field(&user, false))));
        let n = self.methods.len();
        for (i, m) in self.methods.clone().into_iter().enumerate() {
            let small = |id: String, ic: IconName| div().id(SharedString::from(id)).size(px(22.)).rounded(px(ui::R_MD)).flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(crate::theme::hover_bg())).child(icon(ic, ui::IC_XS, c(th.overlay1)));
            let head = div().flex().items_center().gap_2()
                .child(div().w(px(18.)).text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(format!("{}.", i + 1)))
                .child(div().flex_1().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(m.label()))
                .when(i > 0, |d| d.child(small(format!("am-up-{i}"), IconName::ChevronUp).on_click(cx.listener(move |this, _, _w, cx| { this.methods.swap(i, i - 1); cx.notify(); }))))
                .when(i + 1 < n, |d| d.child(small(format!("am-dn-{i}"), IconName::ChevronDown).on_click(cx.listener(move |this, _, _w, cx| { this.methods.swap(i, i + 1); cx.notify(); }))))
                .when(n > 1, |d| d.child(small(format!("am-rm-{i}"), IconName::X).on_click(cx.listener(move |this, _, _w, cx| { this.methods.remove(i); cx.notify(); }))));
            let mut block = div().flex().flex_col().gap(px(6.)).pt_2().border_t_1().border_color(crate::theme::divider()).child(head);
            match m {
                Method::Password => block = block.child(ui::text_field(&self.password, false).mask_toggle()),
                Method::Key | Method::Cert => {
                    if m == Method::Key || !self.methods.contains(&Method::Key) {
                        let kp = self.key_path.clone();
                        let current = self.key_path.read(cx).value().trim().to_string();
                        let keys = crate::settings_view::scan_keys();
                        let mut pick = div().relative().flex_shrink_0().child(ui::button(SharedString::from(format!("am-pick-{i}")), t("host.pick_key"), 0).on_click(cx.listener(|this, _, _w, cx| { this.key_menu = !this.key_menu; cx.notify(); })));
                        if self.key_menu {
                            let mut menu = ui::popover().id("key-pick").occlude().absolute().top(px(32.)).right_0().w(px(420.)).max_h(px(260.)).overflow_y_scroll().p(px(5.)).flex().flex_col()
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.key_menu = false; cx.notify(); }));
                            if keys.is_empty() {
                                menu = menu.child(div().px_2().py_2().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(t("host.no_keys")));
                            }
                            for (k, (path, kind, fp)) in keys.iter().enumerate() {
                                let chosen = path.clone();
                                let on = same_key(&current, path);
                                menu = menu.child(div().id(("keypick", k)).px_2().py_1().rounded(px(ui::R_MD)).flex().items_center().gap_2().cursor_pointer()
                                    .when(on, |d| d.bg(crate::theme::selected_bg()))
                                    .hover(|d| d.bg(crate::theme::menu_sel_bg()))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.key_menu = false;
                                        let path = chosen.clone();
                                        this.key_path.update(cx, |s, cx| s.set_value(path, w, cx));
                                        cx.notify();
                                    }))
                                    .child(icon(IconName::KeyRound, ui::IC_SM, c(th.warning)))
                                    .child(div().flex_1().min_w_0().flex().flex_col()
                                        .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(path.clone()))
                                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(fp.clone())))
                                    .child(ui::tag(kind.clone(), c(th.info)))
                                    .when(on, |d| d.child(icon(IconName::Check, ui::IC_XS, c(th.accent)))));
                            }
                            pick = pick.child(gpui::deferred(menu).with_priority(2));
                        }
                        block = block.child(div().flex().gap_2().child(div().flex_1().child(ui::text_field(&self.key_path, false)))
                            .child(pick)
                            .child(ui::button(SharedString::from(format!("am-browse-{i}")), t("host.browse"), 0).on_click(cx.listener(move |_this, _, w, cx| {
                                let rx = cx.prompt_for_paths(gpui::PathPromptOptions { files: true, directories: false, multiple: false, prompt: None });
                                let kp = kp.clone();
                                cx.spawn_in(w, async move |_this, cx| { if let Ok(Ok(Some(p))) = rx.await { if let Some(f) = p.first() { let f = f.display().to_string(); let _ = cx.update(|w, cx| kp.update(cx, |s, cx| s.set_value(f, w, cx))); } } }).detach();
                            }))))
                            .child(ui::text_field(&self.passphrase, false).mask_toggle());
                    }
                    if m == Method::Cert { block = block.child(ui::text_field(&self.cert_path, false)); }
                }
                Method::Agent => block = block.child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("host.agent_hint"))),
            }
            cred = cred.child(block);
        }
        // "Add auth method" menu
        let missing: Vec<Method> = [Method::Password, Method::Key, Method::Cert, Method::Agent].into_iter().filter(|m| !self.methods.contains(m)).collect();
        if !missing.is_empty() {
            let mut add = div().relative().child(ui::button("am-add", format!("+ {}", t("host.add_method")), 3).on_click(cx.listener(|this, _, _w, cx| { this.add_menu = !this.add_menu; cx.notify(); })));
            if self.add_menu {
                let mut menu = ui::popover().id("am-menu").occlude().absolute().top(px(30.)).left_0().min_w(px(200.)).p(px(5.)).flex().flex_col()
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| { this.add_menu = false; cx.notify(); }));
                for (k, m) in missing.into_iter().enumerate() {
                    let ic = match m { Method::Password => IconName::KeyRound, Method::Key => IconName::Key, Method::Cert => IconName::FileBadge, Method::Agent => IconName::Bot };
                    menu = menu.child(ui::menu_item(("am-mi", k), Some(ic), m.label(), true, false).on_click(cx.listener(move |this, _, _w, cx| { this.methods.push(m); this.add_menu = false; cx.notify(); })));
                }
                add = add.child(gpui::deferred(menu).with_priority(2));
            }
            cred = cred.child(add);
        }
        let proxy_missing = self.use_proxy && ssh_term_proto::proxy::parse(config::get().ssh.proxy.trim()).is_err();
        form = form.child(div().flex().items_start().gap_3()
            .child(label(t("host.use_proxy")))
            .child(Switch::new("host-use-proxy").checked(self.use_proxy).on_click(cx.listener(|this, on: &bool, _, cx| { this.use_proxy = *on; cx.notify(); })))
            .child(div().flex_1().min_w_0().pt(px(2.)).text_size(px(ui::fs_small())).text_color(c(th.overlay1)).whitespace_normal().child(t("host.use_proxy_desc"))));
        if proxy_missing {
            form = form.child(div().p_3().rounded(px(ui::R_MD)).bg(c(th.warning).opacity(0.14)).flex().flex_col().gap_2()
                .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).whitespace_normal().child(t("host.proxy_missing")))
                .child(div().flex().child(ui::button("host-proxy-settings", t("host.proxy_settings"), 1).on_click(cx.listener(|_, _, _, cx| cx.emit(HostEvent::OpenProxySettings))))));
        }
        form = form.child(cred);
        // Finder-style color tags; the production tag drives the permission engine
        let tags_cfg = config::get().tags.clone();
        let me = cx.entity().downgrade();
        form = form.child(div().flex().items_start().gap_3().child(label(t("tag.tags")))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(6.))
                .child(crate::ui::tag_chips("host-tags", &self.tags, &tags_cfg.all(), move |color, _w, cx| { let _ = me.update(cx, |this, cx| { ssh_term_proto::hosttree::toggle(&mut this.tags, &color); cx.notify(); }); }))
                .child(div().w_full().whitespace_normal().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(tf("tag.prod_hint", &[("name", &tags_cfg.name(&tags_cfg.production))])))));
        // ---- test results
        if let Some(rows) = &self.test {
            let names = [t("host.test_dns"), t("host.test_tcp"), t("host.test_ssh"), t("host.test_auth")];
            let mut box_ = div().flex().flex_col().gap(px(4.)).p_3().rounded(px(ui::R_SUB)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider());
            for (i, r) in rows.iter().enumerate() {
                let (ic, col) = match (r.started, r.ok) { (false, _) => (IconName::Circle, c(th.overlay0)), (true, None) => (IconName::LoaderCircle, c(th.accent)), (true, Some(true)) => (IconName::CircleCheck, c(th.success)), (true, Some(false)) => (IconName::CircleX, c(th.error)) };
                box_ = box_.child(div().flex().items_start().gap_2()
                    .child(icon(ic, ui::IC_SM, col))
                    .child(div().w(px(110.)).flex_shrink_0().text_size(px(ui::fs_body())).text_color(c(th.text)).child(names[i]))
                    .child(div().flex_1().min_w_0().whitespace_normal().text_size(px(ui::fs_small())).text_color(if r.ok == Some(false) { c(th.error) } else { c(th.subtext) }).child(r.detail.clone()))
                    .when(r.ok.is_some(), |d| d.child(div().flex_shrink_0().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(format!("{} ms", r.ms)))));
            }
            form = form.child(box_);
        }
        if let Some(e) = &self.error { form = form.child(div().text_size(px(ui::fs_body())).text_color(c(th.error)).child(e.clone())); }

        let title = if self.editing.is_some() { t("ui.edit_host") } else { t("ui.new_host") };
        let testing = self.test_task.is_some();
        let test_btn = if testing { ui::button("host-test", t("host.test_cancel"), 0).on_click(cx.listener(|this, _, _w, cx| this.cancel_test(cx))) }
                       else { ui::button("host-test", t("host.test"), 0).on_click(cx.listener(|this, _, _w, cx| this.run_test(cx))) };
        let mut btns: Vec<gpui::AnyElement> = Vec::new();
        if let Some(name) = self.editing.clone() {
            btns.push(ui::button("host-del", t("ui.delete"), 2).on_click(cx.listener(move |_, _, _w, cx| {
                let n = name.clone();
                let _ = config::update(|s| s.hosts.retain(|h| h.name != n));
                cx.emit(HostEvent::Deleted(name.clone()));
            })).into_any_element());
        }
        btns.push(ui::button("host-cancel", t("ui.cancel"), 0).on_click(cx.listener(|_, _, _w, cx| cx.emit(HostEvent::Close))).into_any_element());
        btns.push(ui::button("host-save", t("ui.save"), 1).on_click(cx.listener(|this, _, _w, cx| this.save(cx))).into_any_element());
        let footer = ui::dialog_footer(Some(test_btn.into_any_element()), btns);
        ui::modal(ui::card(580.)
            .child(ui::dialog_header(Some(IconName::Server), c(cur().accent), title, None, None))
            .child(form).child(footer))
    }
}
