//! New / edit host dialog. Saves into config.toml [[hosts]].
use ssh_term_proto::i18n::{t, tf};
use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, FontWeight, Window};
use gpui_component::input::{Input, InputState};
use gpui_component::switch::Switch;
use ssh_term_proto::hosts::HostEntry;

use crate::config;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum HostEvent { Saved(String), Deleted(String), Close }

pub struct HostDialog {
    editing: Option<String>,
    fields: Vec<(&'static str, &'static str, Entity<InputState>)>,
    auth: String,
    production: bool,
    error: Option<String>,
}

impl EventEmitter<HostEvent> for HostDialog {}

impl HostDialog {
    pub fn new(existing: Option<HostEntry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let e = existing.clone().unwrap_or_default();
        let mk = |ph: &str, v: String, window: &mut Window, cx: &mut Context<Self>| { let ph = ph.to_string(); cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v)) };
        let fields = vec![
            (t("ui.name"), "name", mk(t("ui.e_g_prod_web_1"), e.name.clone(), window, cx)),
            (t("ui.address"), "host", mk(t("ui.hostname_or_ip"), e.host.clone(), window, cx)),
            (t("ui.port"), "port", mk("22", e.port.map(|p| p.to_string()).unwrap_or_default(), window, cx)),
            (t("ui.user"), "user", mk(t("ui.empty_uses_the_default_user"), e.user.clone().unwrap_or_default(), window, cx)),
            (t("ui.private_key"), "key", mk(t("ui.ssh_id_ed25519_empty_tries"), e.identity_file.clone().unwrap_or_default(), window, cx)),
            (t("ui.group"), "group", mk(t("ui.e_g_prod_test"), e.group.clone().unwrap_or_default(), window, cx)),
            (t("ui.jump_host"), "jump", mk(t("ui.user_bastion_22_coming_soon"), e.jump.clone().unwrap_or_default(), window, cx)),
        ];
        fields[0].2.update(cx, |s, cx| s.focus(window, cx));
        Self { editing: existing.map(|h| h.name), fields, auth: e.auth.unwrap_or_else(|| "key".into()), production: e.production, error: None }
    }

    fn val(&self, key: &str, cx: &Context<Self>) -> String {
        self.fields.iter().find(|f| f.1 == key).map(|f| f.2.read(cx).value().trim().to_string()).unwrap_or_default()
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.val("name", cx);
        let host = self.val("host", cx);
        if name.is_empty() || host.is_empty() { self.error = Some(t("ui.name_and_address_are_required").into()); cx.notify(); return; }
        let port = self.val("port", cx);
        let port = if port.is_empty() { None } else { match port.parse::<u16>() { Ok(p) if p > 0 => Some(p), _ => { self.error = Some(t("ui.port_must_be_1_65535").into()); cx.notify(); return; } } };
        let opt = |s: String| (!s.is_empty()).then_some(s);
        let entry = HostEntry { name: name.clone(), host, port, user: opt(self.val("user", cx)), identity_file: opt(self.val("key", cx)),
            group: opt(self.val("group", cx)), auth: Some(self.auth.clone()), production: self.production, jump: opt(self.val("jump", cx)), source: "config" };
        let editing = self.editing.clone();
        let s = config::get();
        if editing.as_deref() != Some(name.as_str()) && s.hosts.iter().any(|h| h.name == name) { self.error = Some(tf("host.exists", &[("name", &name)])); cx.notify(); return; }
        if let Some(e) = config::update(|s| {
            match editing.and_then(|old| s.hosts.iter().position(|h| h.name == old)) { Some(i) => s.hosts[i] = entry, None => s.hosts.push(entry) }
        }) { self.error = Some(e); cx.notify(); return; }
        cx.emit(HostEvent::Saved(name));
    }
}

impl Render for HostDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let label = |t: &str| div().w(px(70.)).text_size(px(12.5)).text_color(c(th.subtext)).child(t.to_string());
        let mut form = div().p_5().flex().flex_col().gap_3();
        for (i, (l, key, e)) in self.fields.iter().enumerate() {
            let mut row = div().flex().items_center().gap_3().child(label(l)).child(div().flex_1().child(Input::new(e)));
            if *key == "jump" { row = row.child(ui::soon()); }
            form = form.child(row);
            if i == 4 {
                let me = cx.entity().downgrade();
                let opts = vec![("key".to_string(), t("ui.private_key").to_string()), ("agent".to_string(), "ssh-agent".to_string()), ("password".to_string(), t("ui.password").to_string())];
                form = form.child(div().flex().items_center().gap_3().child(label(t("ui.authentication")))
                    .child(ui::segmented("auth", &opts, &self.auth, move |v, _w, cx| { let _ = me.update(cx, |this, cx| { this.auth = v; cx.notify(); }); }))
                    .when(self.auth != "key", |d| d.child(ui::soon())));
            }
        }
        form = form.child(div().flex().items_center().gap_3().child(label(t("ui.production")))
            .child(Switch::new("prod").checked(self.production).on_click(cx.listener(|this, v: &bool, _w, cx| { this.production = *v; cx.notify(); })))
            .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(t("ui.shows_a_red_prod_tag"))));
        if let Some(e) = &self.error { form = form.child(div().text_size(px(12.)).text_color(c(th.error)).child(e.clone())); }

        let title = if self.editing.is_some() { t("ui.edit_host") } else { t("ui.new_host") };
        let mut footer = div().px_5().py_3().flex().items_center().gap_2().border_t_1().border_color(c(th.surface0)).bg(c(th.mantle))
            .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(tf("host.save_to", &[("path", &config::config_path().display())])))
            .child(div().flex_1());
        if let Some(name) = self.editing.clone() {
            footer = footer.child(ui::button("host-del", t("ui.delete"), 2).on_click(cx.listener(move |_, _, _w, cx| {
                let n = name.clone();
                let _ = config::update(|s| s.hosts.retain(|h| h.name != n));
                cx.emit(HostEvent::Deleted(name.clone()));
            })));
        }
        footer = footer.child(ui::button("host-cancel", t("ui.cancel"), 0).on_click(cx.listener(|_, _, _w, cx| cx.emit(HostEvent::Close))))
            .child(ui::button("host-save", t("ui.save"), 1).on_click(cx.listener(|this, _, _w, cx| this.save(cx))));
        ui::modal(ui::card(560.)
            .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(IconName::Server, 16., c(th.accent)))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title)))
            .child(form).child(footer))
    }
}
