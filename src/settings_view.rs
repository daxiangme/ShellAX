//! Settings page: categories on the left (searchable), form on the right.
//! Every control reads/writes `config::get()` by dotted path and persists config.toml
//! immediately; the root view re-applies what can take effect live.
use ssh_term_proto::i18n::{t, tf};
use std::collections::HashMap;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FontWeight, SharedString, Subscription, Window};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::switch::Switch;
use gpui_component::Sizable;
use ssh_term_proto::policy::{self, Effect, Mode, Risk, Rule};
use ssh_term_proto::ai::{Kind, Provider};
use ssh_term_proto::models::{self, ModelInfo};
use ssh_term_proto::secrets::{self, Location};

use crate::actions::{self, Action};
use crate::config;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Cat { General, Appearance, Terminal, Suggest, Ssh, Credentials, Ai, Agents, Permissions, Audit, Session, Keys, Notify, About }

impl Cat {
    pub const ALL: [Cat; 14] = [Cat::General, Cat::Appearance, Cat::Terminal, Cat::Suggest, Cat::Ssh, Cat::Credentials, Cat::Ai, Cat::Agents,
        Cat::Permissions, Cat::Audit, Cat::Session, Cat::Keys, Cat::Notify, Cat::About];
    pub fn name(self) -> &'static str {
        match self {
            Cat::General => t("ui.general"), Cat::Appearance => t("ui.appearance"), Cat::Terminal => t("ui.terminal"), Cat::Suggest => t("ui.suggestions"), Cat::Ssh => "SSH",
            Cat::Credentials => t("ui.credentials_keys"), Cat::Ai => "AI", Cat::Agents => t("ui.external_agents"), Cat::Permissions => t("ui.permissions_security"),
            Cat::Audit => t("ui.audit"), Cat::Session => t("ui.sessions_layout"), Cat::Keys => t("ui.keyboard_shortcuts"), Cat::Notify => t("ui.notifications"), Cat::About => t("ui.about"),
        }
    }
    fn icon(self) -> IconName {
        match self {
            Cat::General => IconName::SlidersHorizontal, Cat::Appearance => IconName::Palette, Cat::Terminal => IconName::SquareTerminal,
            Cat::Suggest => IconName::Lightbulb, Cat::Ssh => IconName::Server, Cat::Credentials => IconName::KeyRound, Cat::Ai => IconName::Sparkles,
            Cat::Agents => IconName::Bot, Cat::Permissions => IconName::ShieldCheck, Cat::Audit => IconName::ScrollText, Cat::Session => IconName::Layers,
            Cat::Keys => IconName::Keyboard, Cat::Notify => IconName::Bell, Cat::About => IconName::Info,
        }
    }
    fn desc(self) -> &'static str {
        match self {
            Cat::General => t("ui.language_startup_behaviour_and_updates"),
            Cat::Appearance => t("ui.theme_fonts_and_window_appearance"),
            Cat::Terminal => t("ui.terminal_behaviour_cursor_copy_paste"),
            Cat::Suggest => t("ui.history_based_grey_command_suggestions"),
            Cat::Ssh => t("ui.ssh_defaults_and_host_key"),
            Cat::Credentials => t("ui.private_keys_ssh_agent_and"),
            Cat::Ai => t("ui.built_in_ai_bring_your"),
            Cat::Agents => t("ui.detect_claude_code_codex_grok"),
            Cat::Permissions => t("ui.permission_mode_and_rules_for"),
            Cat::Audit => t("ui.append_only_jsonl_audit_log"),
            Cat::Session => t("ui.persistent_sessions_layout_restore_and"),
            Cat::Keys => t("ui.app_shortcuts_type_e_g"),
            Cat::Notify => t("ui.desktop_notifications_and_sounds"),
            Cat::About => t("ui.version_information_and_third_party"),
        }
    }
}

enum Ctl { Toggle(&'static str), Choice(&'static str, Vec<(String, String)>), Text(&'static str, &'static str), Info(String) }

struct Row { cat: Cat, label: &'static str, desc: &'static str, ctl: Ctl, soon: bool }

fn opts(v: &[(&str, &str)]) -> Vec<(String, String)> { v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }

pub enum SettingsEvent { Changed(String), Close, OpenAudit }

pub struct SettingsView {
    pub cat: Cat,
    search: Entity<InputState>,
    inputs: HashMap<String, Entity<InputState>>,
    errors: HashMap<String, String>,
    theme_names: Vec<(String, bool)>,
    // rule editor
    new_rule: [Entity<InputState>; 4],
    new_effect: Effect,
    tester: Entity<InputState>,
    notice: Option<String>,
    // AI providers
    ai_sel: Option<String>,
    ai_key: Entity<InputState>,
    /// provider id -> (masked key, location) ; None = no key stored. Cached: keychain calls are D-Bus round-trips.
    ai_keys: HashMap<String, Option<(String, Location)>>,
    ai_note: Option<(bool, String)>,
    ai_test: HashMap<String, (bool, String)>,
    ai_testing: Option<String>,
    lang_seen: u8,
    ai_models: HashMap<String, ModelState>,
    ai_dropdown: Option<&'static str>,
    ai_model_search: Entity<InputState>,
    ai_url_gen: u64,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(cat: Cat, theme_names: Vec<(String, bool)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.search_settings")));
        let mk = |ph: &str, v: &str, window: &mut Window, cx: &mut Context<Self>| { let (ph, v) = (ph.to_string(), v.to_string()); cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v)) };
        let new_rule = [mk(t("ui.rule_id_e_g_allow"), "", window, cx), mk(t("ui.principal_ai_claude"), "*", window, cx), mk(t("ui.command_glob_e_g_make"), "", window, cx), mk(t("ui.reason_optional"), "", window, cx)];
        let tester = mk(t("ui.type_a_command_to_test"), "git push --force origin main", window, cx);
        let mut subs = vec![cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() })];
        subs.push(cx.subscribe_in(&tester, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
        let ai_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(t("ai.key_placeholder")));
        let ai_model_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("models.search")));
        subs.push(cx.subscribe_in(&ai_model_search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
        let mut me = Self { cat, search, inputs: HashMap::new(), errors: HashMap::new(), theme_names, new_rule, new_effect: Effect::Allow, tester, notice: None,
            ai_sel: config::get().ai.default_provider().map(|p| p.id.clone()), ai_key, ai_keys: HashMap::new(), ai_note: None, ai_test: HashMap::new(), ai_testing: None, lang_seen: ssh_term_proto::i18n::current() as u8,
            ai_models: HashMap::new(), ai_dropdown: None, ai_model_search, ai_url_gen: 0, _subs: subs };
        if let Some(id) = me.ai_sel.clone() { me.load_models(&id, false, cx); }
        me.refresh_keys();
        me
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn rows(&self) -> Vec<Row> {
        let themes: Vec<(String, String)> = std::iter::once(("system".to_string(), t("ui.follow_system").to_string()))
            .chain(self.theme_names.iter().map(|n| (n.0.clone(), n.0.clone()))).collect();
        let light: Vec<(String, String)> = self.theme_names.iter().filter(|n| !n.1).map(|n| (n.0.clone(), n.0.clone())).collect();
        let dark: Vec<(String, String)> = self.theme_names.iter().filter(|n| n.1).map(|n| (n.0.clone(), n.0.clone())).collect();
        let r = |cat, label, desc, ctl, soon| Row { cat, label, desc, ctl, soon };
        use Cat::*;
        vec![
            r(General, t("ui.interface_language"), t("ui.applies_immediately_follow_system_reads"), Ctl::Choice("general.language", opts(&[("system", t("ui.follow_system")), ("zh-CN", "简体中文"), ("en", "English")])), false),
            r(General, t("ui.on_startup"), t("ui.what_to_open_when_the"), Ctl::Choice("general.startup", opts(&[("local", t("ui.local_terminal")), ("restore", t("ui.restore_layout_coming_soon")), ("blank", t("ui.blank"))])), false),
            r(General, t("ui.check_for_updates_automatically"), "", Ctl::Toggle("general.check_updates"), true),
            r(General, t("ui.update_channel"), "", Ctl::Choice("general.update_channel", opts(&[("stable", t("ui.stable")), ("beta", t("ui.beta"))])), true),
            r(General, t("ui.config_file"), t("ui.all_settings_are_stored_here"), Ctl::Info(config::config_path().display().to_string()), false),

            r(Appearance, t("ui.theme"), t("ui.follow_system_switches_with_the"), Ctl::Choice("appearance.theme", themes), false),
            r(Appearance, t("ui.light_theme"), t("ui.used_in_light_mode_when"), Ctl::Choice("appearance.light_theme", light), false),
            r(Appearance, t("ui.dark_theme"), t("ui.used_in_dark_mode_when"), Ctl::Choice("appearance.dark_theme", dark), false),
            r(Appearance, t("ui.terminal_font"), t("ui.primary_font_monospace"), Ctl::Text("font.family", "JetBrains Mono"), false),
            r(Appearance, t("ui.fallback_fonts"), t("ui.comma_separated_used_in_order"), Ctl::Text("font.fallbacks", "Noto Sans Mono CJK SC, …"), false),
            r(Appearance, t("ui.font_size"), t("ui.in_pt_8_32"), Ctl::Text("font.size", "13.5"), false),
            r(Appearance, t("ui.line_height"), t("ui.multiple_of_the_font_size"), Ctl::Text("font.line_height", "1.4"), false),
            r(Appearance, t("ui.ligatures"), t("ui.show_etc_as_ligatures"), Ctl::Toggle("font.ligatures"), false),
            r(Appearance, t("ui.window_opacity"), "0.5–1.0", Ctl::Text("appearance.opacity", "1.0"), true),
            r(Appearance, t("ui.ui_font_size"), t("ui.sidebar_tabs_and_other_ui"), Ctl::Text("appearance.ui_font_size", "13"), true),

            r(Terminal, t("ui.default_shell"), t("ui.empty_uses_shell_applies_to"), Ctl::Text("terminal.shell", "/bin/zsh"), false),
            r(Terminal, t("ui.scrollback_lines"), t("ui.applies_to_new_sessions"), Ctl::Text("terminal.scrollback", "10000"), false),
            r(Terminal, t("ui.cursor_style"), "", Ctl::Choice("terminal.cursor_style", opts(&[("block", t("ui.block")), ("bar", t("ui.bar")), ("underline", t("ui.underline"))])), false),
            r(Terminal, t("ui.cursor_blink"), t("ui.stops_blinking_after_10_s"), Ctl::Toggle("terminal.cursor_blink"), false),
            r(Terminal, t("ui.bell"), t("ui.on_bel"), Ctl::Choice("terminal.bell", opts(&[("none", t("ui.off")), ("visual", t("ui.flash_border")), ("sound", t("ui.sound_coming_soon"))])), false),
            r(Terminal, t("ui.copy_on_select"), t("ui.copy_to_the_clipboard_on"), Ctl::Toggle("terminal.copy_on_select"), false),
            r(Terminal, t("ui.confirm_multi_line_paste"), t("ui.confirm_before_pasting_multiple_lines"), Ctl::Toggle("terminal.paste_confirm"), false),
            r(Terminal, t("ui.mouse_reporting"), t("ui.let_vim_htop_etc_receive"), Ctl::Toggle("terminal.mouse_reporting"), false),
            r(Terminal, t("ui.shell_integration_osc_133"), t("ui.inject_prompt_marks_into_bash"), Ctl::Choice("terminal.shell_integration", opts(&[("auto", t("ui.auto_inject")), ("off", t("ui.off"))])), false),

            r(Suggest, t("ui.enable_suggestions"), "", Ctl::Toggle("suggest.enabled"), false),
            r(Suggest, t("ui.accept_key"), "", Ctl::Choice("suggest.accept", opts(&[("both", t("ui.or_tab")), ("right", t("ui.only")), ("tab", t("ui.tab_only"))])), false),
            r(Suggest, t("ui.local_history"), t("ui.record_commands_typed_in_this"), Ctl::Toggle("suggest.local_history"), false),
            r(Suggest, t("ui.remote_history"), t("ui.read_remote_bash_history_and"), Ctl::Toggle("suggest.remote_history"), false),
            r(Suggest, t("ui.sensitive_command_filters"), t("ui.regexes_comma_separated_combined_with"), Ctl::Text("suggest.filter_patterns", "(?i)vault +login"), false),

            r(Ssh, t("ui.default_user"), t("ui.used_when_a_host_has"), Ctl::Text("ssh.default_user", "$USER"), false),
            r(Ssh, t("ui.default_port"), "", Ctl::Text("ssh.default_port", "22"), false),
            r(Ssh, t("ui.keepalive_interval_s"), t("ui.0_disables"), Ctl::Text("ssh.keepalive_secs", "30"), false),
            r(Ssh, t("ui.connect_timeout_s"), t("ui.tcp_connect_phase"), Ctl::Text("ssh.connect_timeout_secs", "15"), false),
            r(Ssh, t("ui.known_hosts_file"), "", Ctl::Text("ssh.known_hosts", "~/.ssh/known_hosts"), false),
            r(Ssh, t("ui.unknown_host_keys"), t("ui.what_to_do_on_first"), Ctl::Choice("ssh.host_key_policy", opts(&[("ask", t("ui.ask")), ("strict", t("ui.strict_reject")), ("accept-new", t("ui.accept_new_hosts"))])), false),
            r(Ssh, t("ui.read_ssh_config"), t("ui.list_its_host_entries_in"), Ctl::Toggle("ssh.read_ssh_config"), false),
            r(Ssh, t("ui.ssh_config_path"), "", Ctl::Text("ssh.ssh_config", "~/.ssh/config"), false),
            r(Ssh, t("ui.default_jump_host"), t("ui.proxyjump_e_g_bastion"), Ctl::Text("ssh.jump_host", "user@bastion:22"), true),
            r(Ssh, t("ui.cipher_preference"), t("ui.comma_separated_by_priority"), Ctl::Text("ssh.ciphers", ""), true),
            r(Ssh, t("ui.agent_forwarding"), "", Ctl::Toggle("ssh.agent_forwarding"), true),

            r(Credentials, t("ui.use_system_keychain"), t("ui.store_key_passphrases_and_api"), Ctl::Toggle("credentials.use_keychain"), true),
            r(Credentials, t("ui.use_ssh_agent"), "", Ctl::Toggle("credentials.use_ssh_agent"), true),
            r(Credentials, t("ui.extra_private_keys"), t("ui.comma_separated_paths_tried_after"), Ctl::Text("credentials.extra_keys", "~/.ssh/work_ed25519"), true),

            r(Ai, t("ui.enable_built_in_ai"), t("ai.enable.d"), Ctl::Toggle("ai.enabled"), false),
            r(Ai, t("ui.redact_before_sending"), t("ui.strip_passwords_tokens_private_keys"), Ctl::Toggle("ai.redact"), false),
            r(Ai, t("ui.context_scope"), t("ai.context.d"), Ctl::Choice("ai.context", opts(&[("none", t("ai.ctx_none")), ("command", t("ui.last_command")), ("selection", t("ui.selection_only")), ("screen", t("ui.current_screen"))])), false),
            r(Ai, t("ai.context_lines"), t("ai.context_lines.d"), Ctl::Text("ai.context_lines", "40"), false),
            r(Ai, t("ui.allow_sending_command_output"), t("ui.when_off_only_the_command"), Ctl::Toggle("ai.send_output"), false),
            r(Ai, t("ai.nl_prefix"), t("ai.nl_prefix.d"), Ctl::Text("ai.nl_prefix", "#"), false),

            r(Agents, t("ui.detect_claude_code"), "", Ctl::Toggle("agents.detect_claude"), false),
            r(Agents, t("ui.detect_codex"), "", Ctl::Toggle("agents.detect_codex"), false),
            r(Agents, t("ui.detect_grok"), "", Ctl::Toggle("agents.detect_grok"), false),
            r(Agents, t("ui.notify_on_state_change"), t("ui.desktop_notification_when_an_agent"), Ctl::Toggle("agents.notify_state"), true),
            r(Agents, t("ui.approval_timeout_s"), t("ui.how_long_to_wait_after"), Ctl::Text("agents.approval_timeout_secs", "120"), true),
            r(Agents, t("ui.on_timeout"), "", Ctl::Choice("agents.on_timeout", opts(&[("deny", t("ui.deny")), ("ask", t("ui.keep_waiting"))])), true),

            r(Permissions, t("ui.default_mode"), t("ui.also_switchable_from_the_status"), Ctl::Choice("permissions.mode", opts(&[("ask", t("ui.ask_2")), ("plan", t("ui.plan_read_only")), ("auto", t("ui.auto"))])), false),
            r(Permissions, t("ui.max_auto_risk_in_auto"), t("ui.actions_at_or_below_this"), Ctl::Choice("permissions.max_auto_risk", opts(&[("R0", "R0"), ("R1", "R1"), ("R2", "R2")])), false),
            r(Permissions, t("ui.confirm_writes_on_production_hosts"), t("ui.always_ask_for_r1_actions"), Ctl::Toggle("permissions.prod_confirm"), false),
            r(Permissions, t("ui.double_confirm_dangerous_commands"), t("ui.r4_actions_need_confirmation_even"), Ctl::Toggle("permissions.dangerous_confirm"), false),

            r(Audit, t("ui.log_directory"), "", Ctl::Text("audit.dir", "~/.local/share/agentshell/audit"), false),
            r(Audit, t("ui.retention_days"), "", Ctl::Text("audit.retention_days", "90"), true),
            r(Audit, t("ui.log_manually_typed_commands"), t("ui.by_default_only_connections_permissions"), Ctl::Toggle("audit.log_commands"), true),

            r(Session, t("ui.persistent_sessions"), t("ui.resume_via_remote_tmux_after"), Ctl::Choice("session.persistent", opts(&[("off", t("ui.off")), ("tmux", "tmux")])), true),
            r(Session, t("ui.restore_layout_on_startup"), "", Ctl::Toggle("session.restore_layout"), true),
            r(Session, t("ui.confirm_before_closing"), t("ui.confirm_closing_a_tab_while"), Ctl::Toggle("session.confirm_close"), true),
            r(Session, t("ui.auto_reconnect"), "", Ctl::Toggle("session.auto_reconnect"), true),

            r(Notify, t("ui.desktop_notifications"), "", Ctl::Toggle("notify.desktop"), true),
            r(Notify, t("ui.notify_when_an_agent_waits"), "", Ctl::Toggle("notify.agent_blocked"), true),
            r(Notify, t("ui.long_command_notification_s"), t("ui.when_a_command_runs_longer"), Ctl::Text("notify.long_command_secs", "10"), true),
            r(Notify, t("ui.sound"), "", Ctl::Toggle("notify.sound"), true),
        ]
    }

    fn set(&mut self, path: &str, v: toml::Value, cx: &mut Context<Self>) {
        let p = path.to_string();
        let mut err = None;
        let save_err = config::update(|s| { if let Err(e) = s.set(&p, v) { err = Some(tf("settings.invalid_value", &[("e", &e)])); } });
        match err.or(save_err) {
            Some(e) => { self.errors.insert(p.clone(), e); }
            None => { self.errors.remove(&p); validate(&p, &mut self.errors); }
        }
        if p == "general.language" { self.inputs.clear(); }
        cx.emit(SettingsEvent::Changed(p));
        cx.notify();
    }

    fn input(&mut self, key: &str, placeholder: &str, initial: String, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(e) = self.inputs.get(key) { return e.clone(); }
        let ph = placeholder.to_string();
        let e = cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(initial));
        let k = key.to_string();
        self._subs.push(cx.subscribe_in(&e, window, move |this, st, ev: &InputEvent, _w, cx| {
            if !matches!(ev, InputEvent::Change) { return; }
            let v = st.read(cx).value().to_string();
            if let Some(id) = k.strip_prefix("keys.") {
                let id = id.to_string();
                if !v.trim().is_empty() && !actions::valid_binding(&v) {
                    this.errors.insert(k.clone(), t("ui.must_include_ctrl_alt_cmd").into());
                    cx.notify();
                    return;
                }
                this.errors.remove(&k);
                let err = config::update(|s| { if v.trim().is_empty() { s.keys.remove(&id); } else { s.keys.insert(id.clone(), actions::normalize(&v)); } });
                if let Some(e) = err { this.errors.insert(k.clone(), e); }
                cx.emit(SettingsEvent::Changed(k.clone()));
                cx.notify();
            } else {
                this.set(&k, toml::Value::String(v), cx);
            }
        }));
        self.inputs.insert(key.to_string(), e.clone());
        e
    }

    fn control(&mut self, ctl: &Ctl, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let s = config::get();
        let th = cur();
        match ctl {
            Ctl::Toggle(path) => {
                let p = *path;
                Switch::new(SharedString::from(format!("sw-{p}"))).checked(s.get_bool(p))
                    .on_click(cx.listener(move |this, checked: &bool, _w, cx| this.set(p, toml::Value::Boolean(*checked), cx)))
                    .into_any_element()
            }
            Ctl::Choice(path, options) => {
                let p = *path;
                let me = cx.entity().downgrade();
                ui::segmented(p, options, &s.get_str(p), move |v, _w, cx| { let _ = me.update(cx, |this, cx| this.set(p, toml::Value::String(v), cx)); }).into_any_element()
            }
            Ctl::Text(path, ph) => {
                let e = self.input(path, ph, s.get_str(path), window, cx);
                div().w(px(260.)).child(Input::new(&e)).into_any_element()
            }
            Ctl::Info(text) => div().max_w(px(320.)).text_size(px(12.)).text_color(c(th.subtext)).child(text.clone()).into_any_element(),
        }
    }

    fn row_el(&mut self, row: &Row, idx: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let ctl = self.control(&row.ctl, window, cx);
        let path = match &row.ctl { Ctl::Toggle(p) | Ctl::Choice(p, _) | Ctl::Text(p, _) => Some(*p), _ => None };
        let err = path.and_then(|p| self.errors.get(p).cloned());
        let mut left = div().flex_1().flex().flex_col().gap(px(2.)).min_w_0()
            .child(div().flex().items_center().gap_2().text_size(px(13.)).text_color(c(th.text)).child(row.label).when(row.soon, |d| d.child(ui::soon())));
        if !row.desc.is_empty() { left = left.child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(row.desc)); }
        if let Some(e) = err { left = left.child(div().text_size(px(11.5)).text_color(c(th.error)).child(e)); }
        div().id(("row", idx)).py(px(10.)).flex().items_center().gap_4().border_b_1().border_color(c(th.surface0))
            .child(left).child(div().flex_shrink_0().child(ctl)).into_any_element()
    }

    // ---------------------------------------------------------------- custom sections
    fn section_title(t: &str) -> impl IntoElement {
        let th = cur();
        div().pt_5().pb_2().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.subtext)).child(t.to_string())
    }

    fn permissions_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let mut out = div().flex().flex_col();
        // mode cards
        let mut modes = div().flex().gap_2();
        for m in Mode::ALL {
            let active = s.permissions.mode == m;
            modes = modes.child(div().id(("mode-card", m as usize)).flex_1().p_3().rounded_lg().cursor_pointer().border_1()
                .border_color(if active { c(th.accent) } else { c(th.surface0) }).bg(if active { c(th.accent).opacity(0.08) } else { c(th.mantle) })
                .on_click(cx.listener(move |this, _, _w, cx| this.set("permissions.mode", toml::Value::String(format!("{m:?}").to_lowercase()), cx)))
                .child(div().flex().items_center().gap_2().child(icon(IconName::Shield, 14., if active { c(th.accent) } else { c(th.overlay1) }))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(m.label())))
                .child(div().pt_1().text_size(px(11.5)).text_color(c(th.overlay1)).child(m.desc())));
        }
        out = out.child(Self::section_title(t("ui.permission_mode"))).child(modes);

        // rule table
        out = out.child(Self::section_title(t("ui.rules_evaluated_top_to_bottom")));
        let header = div().flex().items_center().gap_2().px_3().py(px(6.)).text_size(px(11.)).text_color(c(th.overlay1)).bg(c(th.mantle)).rounded_t_lg()
            .child(div().w(px(24.)).child("#")).child(div().w(px(130.)).child("ID")).child(div().w(px(70.)).child(t("ui.principal")))
            .child(div().w(px(60.)).child(t("ui.action"))).child(div().flex_1().child(t("ui.command_glob"))).child(div().w(px(176.)).child(t("ui.effect"))).child(div().w(px(56.)));
        let mut table = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0)).child(header);
        let tester_cmd = self.tester.read(cx).value().to_string();
        let hit = policy::evaluate(&s.permissions.rules, "ai", "exec", &tester_cmd).map(|(i, _)| i);
        for (i, rule) in s.permissions.rules.iter().enumerate() {
            let me = cx.entity().downgrade();
            let eff_opts = opts(&[("allow", t("ui.allow")), ("ask", t("ui.ask")), ("deny", t("ui.deny"))]);
            let cur_eff = format!("{:?}", rule.effect).to_lowercase();
            let seg_id: &'static str = Box::leak(format!("rule-eff-{i}").into_boxed_str());
            let eff_col = match rule.effect { Effect::Allow => c(th.success), Effect::Ask => c(th.warning), Effect::Deny => c(th.error) };
            table = table.child(div().flex().items_center().gap_2().px_3().py(px(6.)).border_t_1().border_color(c(th.surface0)).text_size(px(12.)).text_color(c(th.text))
                .when(hit == Some(i), |d| d.bg(c(th.accent).opacity(0.08)))
                .child(div().w(px(24.)).flex().items_center().child(div().size(px(6.)).rounded_full().bg(eff_col)).child(div().pl_1().text_color(c(th.overlay1)).child(format!("{}", i + 1))))
                .child(div().w(px(130.)).overflow_hidden().child(rule.id.clone()))
                .child(div().w(px(70.)).text_color(c(th.subtext)).child(rule.principal.clone()))
                .child(div().w(px(60.)).text_color(c(th.subtext)).child(rule.action.clone()))
                .child(div().flex_1().flex().flex_col().min_w_0()
                    .child(div().font_family(SharedString::from(s.font.family.clone())).text_size(px(12.)).child(rule.command.clone()))
                    .when(!rule.reason.is_empty(), |d| d.child(div().text_size(px(10.5)).text_color(c(th.overlay0)).child(rule.reason.clone()))))
                .child(div().w(px(176.)).child(ui::segmented(seg_id, &eff_opts, &cur_eff, move |v, _w, cx| {
                    let _ = me.update(cx, |this, cx| {
                        let e = match v.as_str() { "allow" => Effect::Allow, "deny" => Effect::Deny, _ => Effect::Ask };
                        let err = config::update(|s| s.permissions.rules[i].effect = e);
                        this.notice = err;
                        cx.emit(SettingsEvent::Changed("permissions.rules".into()));
                        cx.notify();
                    });
                })))
                .child(div().w(px(56.)).flex().gap_1()
                    .child(div().id(("rule-up", i)).size(px(22.)).rounded_md().flex().items_center().justify_center().cursor_pointer().text_color(c(th.overlay1)).hover(|d| d.bg(c(cur().surface0)))
                        .on_click(cx.listener(move |this, _, _w, cx| { if i > 0 { this.notice = config::update(|s| s.permissions.rules.swap(i, i - 1)); cx.emit(SettingsEvent::Changed("permissions.rules".into())); cx.notify(); } }))
                        .child("↑"))
                    .child(div().id(("rule-del", i)).size(px(22.)).rounded_md().flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().error).opacity(0.15)))
                        .on_click(cx.listener(move |this, _, _w, cx| { this.notice = config::update(|s| { s.permissions.rules.remove(i); }); cx.emit(SettingsEvent::Changed("permissions.rules".into())); cx.notify(); }))
                        .child(icon(IconName::Trash, 13., c(th.error))))));
        }
        // add-rule row
        let me = cx.entity().downgrade();
        let new_eff = format!("{:?}", self.new_effect).to_lowercase();
        table = table.child(div().flex().items_center().gap_2().px_3().py(px(8.)).border_t_1().border_color(c(th.surface0)).bg(c(th.mantle)).rounded_b_lg()
            .child(div().w(px(24.)).child(icon(IconName::Plus, 13., c(th.overlay1))))
            .child(div().w(px(130.)).child(Input::new(&self.new_rule[0]).small()))
            .child(div().w(px(70.)).child(Input::new(&self.new_rule[1]).small()))
            .child(div().w(px(60.)).text_size(px(12.)).text_color(c(th.subtext)).child("exec"))
            .child(div().flex_1().child(Input::new(&self.new_rule[2]).small()))
            .child(div().w(px(176.)).child(ui::segmented("new-eff", &opts(&[("allow", t("ui.allow")), ("ask", t("ui.ask")), ("deny", t("ui.deny"))]), &new_eff, move |v, _w, cx| {
                let _ = me.update(cx, |this, cx| { this.new_effect = match v.as_str() { "allow" => Effect::Allow, "deny" => Effect::Deny, _ => Effect::Ask }; cx.notify(); });
            })))
            .child(div().w(px(56.)).child(ui::button("rule-add", t("ui.add"), 1).on_click(cx.listener(|this, _, window, cx| this.add_rule(window, cx))))));
        out = out.child(table);
        out = out.child(div().pt_2().flex().items_center().gap_2().child(div().w(px(300.)).child(Input::new(&self.new_rule[3]).small()))
            .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(t("ui.glob_matches_anything_one_character"))));
        if let Some(n) = &self.notice { out = out.child(div().pt_1().text_size(px(11.5)).text_color(c(th.error)).child(n.clone())); }

        // tester
        let max_auto: Risk = s.permissions.max_auto_risk.parse().unwrap_or(Risk::R1);
        let (eff, why) = policy::decide(s.permissions.mode, max_auto, &s.permissions.rules, "ai", "exec", &tester_cmd, s.permissions.dangerous_confirm);
        let eff_col = match eff { Effect::Allow => c(th.success), Effect::Ask => c(th.warning), Effect::Deny => c(th.error) };
        out = out.child(Self::section_title(t("ui.rule_tester")))
            .child(div().p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface0)).flex().flex_col().gap_2()
                .child(div().flex().items_center().gap_3().child(div().flex_1().child(Input::new(&self.tester)))
                    .child(div().flex().items_center().gap_2().child(ui::tag(policy::classify(&tester_cmd).label(), c(th.info))).child(ui::tag(eff.label(), eff_col))))
                .child(div().text_size(px(11.5)).text_color(c(th.subtext)).child(tf("settings.tester_result", &[("mode", &s.permissions.mode.label()), ("effect", &eff.label()), ("why", &why)]))));

        // risk levels
        out = out.child(Self::section_title(t("ui.risk_levels")));
        let mut risks = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0));
        for (i, r) in [Risk::R0, Risk::R1, Risk::R2, Risk::R3, Risk::R4, Risk::Hard].into_iter().enumerate() {
            let col = match r { Risk::R0 => c(th.success), Risk::R1 => c(th.info), Risk::R2 => c(th.accent), Risk::R3 => c(th.warning), _ => c(th.error) };
            risks = risks.child(div().flex().items_center().gap_3().px_3().py(px(7.)).when(i > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                .child(div().w(px(44.)).flex().child(ui::tag(r.label(), col)))
                .child(div().text_size(px(12.)).text_color(c(th.text)).child(r.desc())));
        }
        out = out.child(risks);
        let _ = window;
        out.into_any_element()
    }

    fn add_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let vals: Vec<String> = self.new_rule.iter().map(|e| e.read(cx).value().trim().to_string()).collect();
        if vals[2].is_empty() { self.notice = Some(t("ui.please_enter_a_command_glob").into()); cx.notify(); return; }
        let id = if vals[0].is_empty() { format!("rule-{}", config::get().permissions.rules.len() + 1) } else { vals[0].clone() };
        let rule = Rule { id, principal: if vals[1].is_empty() { "*".into() } else { vals[1].clone() }, action: "exec".into(), command: vals[2].clone(), effect: self.new_effect, reason: vals[3].clone() };
        self.notice = config::update(|s| s.permissions.rules.push(rule));
        for (i, e) in self.new_rule.iter().enumerate() { if i != 1 { e.update(cx, |s, cx| s.set_value("", window, cx)); } }
        cx.emit(SettingsEvent::Changed("permissions.rules".into()));
        cx.notify();
    }

    fn credentials_extra(&self) -> AnyElement {
        let th = cur();
        let mut list = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0));
        let keys = scan_keys();
        if keys.is_empty() { list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("ui.no_private_keys_found_in"))); }
        for (i, (name, kind, fp)) in keys.iter().enumerate() {
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(8.)).when(i > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                .child(icon(IconName::KeyRound, 14., c(th.warning)))
                .child(div().flex_1().flex().flex_col().child(div().text_size(px(12.5)).text_color(c(th.text)).child(name.clone()))
                    .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(fp.clone())))
                .child(ui::tag(kind.clone(), c(th.info))));
        }
        let agent = std::env::var("SSH_AUTH_SOCK").ok().filter(|p| std::path::Path::new(p).exists());
        div().flex().flex_col()
            .child(Self::section_title(t("ui.local_private_keys_read_only"))).child(list)
            .child(div().pt_2().flex().gap_2()
                .child(ui::button("key-gen", t("ui.generate_new_key"), 0).opacity(0.5)).child(ui::button("key-import", t("ui.import"), 0).opacity(0.5)).child(div().pt_1().child(ui::soon())))
            .child(Self::section_title("ssh-agent"))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(match agent {
                Some(p) => tf("settings.agent_sock", &[("p", &p)]),
                None => t("ui.no_running_ssh_agent_detected").into(),
            }))
            .into_any_element()
    }

    fn agents_extra(&self) -> AnyElement {
        let th = cur();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
        let check = |rel: &str| std::fs::read_to_string(home.join(rel)).map(|b| b.contains("agentshell")).unwrap_or(false);
        let items = [("Claude Code", ".claude/settings.json", check(".claude/settings.json")), ("Codex", ".codex/config.toml", check(".codex/config.toml")), ("Grok", ".grok/settings.json", check(".grok/settings.json"))];
        let mut list = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0));
        for (i, (name, file, ok)) in items.iter().enumerate() {
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(8.)).when(i > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                .child(icon(IconName::Bot, 14., c(th.accent)))
                .child(div().flex_1().flex().flex_col().child(div().text_size(px(12.5)).text_color(c(th.text)).child(*name))
                    .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(format!("~/{file}"))))
                .child(if *ok { ui::tag(t("ui.hook_installed"), c(th.success)).into_any_element() } else { ui::tag(t("ui.hook_not_installed"), c(th.overlay1)).into_any_element() })
                .child(ui::button(("hook-install", i), t("ui.install"), 0).opacity(0.5)));
        }
        div().flex().flex_col().child(Self::section_title(t("ui.hook_status_read_only_check"))).child(list)
            .child(div().pt_2().flex().items_center().gap_2().text_size(px(11.5)).text_color(c(th.overlay1)).child(t("ui.install_hooks")).child(ui::soon()))
            .into_any_element()
    }

    fn keys_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0));
        let mut seen: HashMap<String, &'static str> = HashMap::new();
        for (i, a) in Action::ALL.iter().enumerate() {
            let info = a.info();
            let b = a.binding();
            let conflict = if b.is_empty() { None } else { seen.insert(actions::normalize(&b), info.label) };
            let key = format!("keys.{}", info.id);
            let initial = config::get().keys.get(info.id).cloned().unwrap_or_default();
            let e = self.input(&key, if info.key.is_empty() { t("ui.unbound") } else { info.key }, initial, window, cx);
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(6.)).when(i > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                .child(icon(info.icon, 14., c(th.overlay1)))
                .child(div().flex_1().flex().flex_col().child(div().text_size(px(12.5)).text_color(c(th.text)).child(info.label))
                    .when_some(conflict, |d, other| d.child(div().text_size(px(11.)).text_color(c(th.error)).child(tf("settings.key_conflict", &[("other", &other)]))))
                    .when_some(self.errors.get(&key).cloned(), |d, e| d.child(div().text_size(px(11.)).text_color(c(th.error)).child(e))))
                .child(div().w(px(150.)).flex().justify_end().child(if b.is_empty() { div().text_size(px(11.)).text_color(c(th.overlay0)).child("—").into_any_element() } else { ui::kbd(&b) }))
                .child(div().w(px(170.)).child(Input::new(&e).small())));
        }
        div().flex().flex_col().child(Self::section_title(t("ui.app_shortcuts_ctrl_shift_c"))).child(list).into_any_element()
    }

    fn audit_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let path = config::get().audit_path();
        let n = ssh_term_proto::audit::read_all(&path).len();
        div().flex().flex_col().child(Self::section_title(t("ui.log")))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(tf("settings.audit_count", &[("path", &path.display()), ("n", &n)])))
            .child(div().pt_2().flex().gap_2()
                .child(ui::button("open-audit", t("ui.open_audit_log_viewer"), 1).on_click(cx.listener(|_, _, _w, cx| cx.emit(SettingsEvent::OpenAudit)))))
            .into_any_element()
    }

    fn suggest_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        div().flex().flex_col().child(Self::section_title(t("ui.history_data")))
            .child(div().flex().items_center().gap_3()
                .child(ui::button("clear-hist", t("ui.clear_command_history"), 2).on_click(cx.listener(|this, _, _w, cx| {
                    let dir = crate::data_dir().join("history");
                    let n = std::fs::read_dir(&dir).map(|d| d.filter_map(Result::ok).filter(|e| std::fs::write(e.path(), "").is_ok()).count()).unwrap_or(0);
                    this.notice = Some(tf("settings.cleared_files", &[("n", &n)]));
                    cx.emit(SettingsEvent::Changed("suggest.clear_history".into()));
                    cx.notify();
                })))
                .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(self.notice.clone().unwrap_or_else(|| t("ui.clears_history_saved_by_this").into()))))
            .into_any_element()
    }

    fn about_extra(&self) -> AnyElement {
        let th = cur();
        let notices = include_str!("../THIRD_PARTY_NOTICES.md");
        let mut list = div().flex().flex_col().gap_1().p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface0));
        for line in notices.lines() {
            if line.starts_with("|---") || line.starts_with("| Our file") || line.trim().is_empty() { continue; }
            let cleaned = line.trim_end().replace("**", "").replace('`', "");
            let cleaned = if cleaned.starts_with('|') { cleaned.trim_matches('|').split(" | ").map(str::trim).collect::<Vec<_>>().join("  ·  ") } else { cleaned };
            let l = cleaned.as_str();
            let el = if let Some(h) = l.strip_prefix("### ") { div().pt_1().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(h.to_string()) }
                else if let Some(h) = l.strip_prefix("## ") { div().pt_2().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(h.to_string()) }
                else if let Some(h) = l.strip_prefix("# ") { div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(h.to_string()) }
                else { div().text_size(px(11.5)).text_color(c(th.subtext)).child(l.to_string()) };
            list = list.child(el);
        }
        div().flex().flex_col()
            .child(div().flex().items_center().gap_3().py_3()
                .child(div().size(px(40.)).rounded_lg().bg(c(th.accent).opacity(0.15)).flex().items_center().justify_center().child(icon(IconName::SquareTerminal, 22., c(th.accent))))
                .child(div().flex().flex_col().child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child("AgentShell"))
                    .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(tf("settings.version", &[("v", &env!("CARGO_PKG_VERSION"))])))))
            .child(Self::section_title(t("ui.quick_start")))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).flex().flex_col().gap_1()
                .child(t("ui.ctrl_shift_p_opens_the")).child(t("ui.tab_accepts_grey_suggestions_shift"))
                .child(t("ui.ctrl_shift_c_v_copy")))
            .child(Self::section_title(t("ui.third_party_licenses_third_party"))).child(list)
            .into_any_element()
    }
}

fn validate(path: &str, errors: &mut HashMap<String, String>) {
    let s = config::get();
    let msg = match path {
        "font.size" if !(8.0..=32.0).contains(&s.font.size) => Some(t("ui.font_size_must_be_8")),
        "font.line_height" if !(1.0..=2.0).contains(&s.font.line_height) => Some(t("ui.line_height_must_be_1")),
        "terminal.scrollback" if s.terminal.scrollback > 1_000_000 => Some(t("ui.at_most_1_000_000")),
        "suggest.filter_patterns" => {
            let bad: Vec<_> = s.suggest.filter_patterns.iter().filter(|p| regex::Regex::new(p).is_err()).cloned().collect();
            if !bad.is_empty() { errors.insert(path.into(), tf("settings.bad_regex", &[("list", &bad.join(", "))])); }
            None
        }
        _ => None,
    };
    if let Some(m) = msg { errors.insert(path.into(), m.into()); }
}

/// (file name, key type, SHA-256 fingerprint) for ~/.ssh/id_* with a matching .pub.
pub fn scan_keys() -> Vec<(String, String, String)> {
    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".ssh");
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(&dir) else { return out };
    let mut names: Vec<String> = rd.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for n in names {
        if !n.starts_with("id_") || n.ends_with(".pub") { continue; }
        let Ok(pubs) = std::fs::read_to_string(dir.join(format!("{n}.pub"))) else { continue };
        let kind = pubs.split_whitespace().next().unwrap_or("?").trim_start_matches("ssh-").to_string();
        let fp = russh::keys::PublicKey::from_openssh(pubs.trim()).map(|k| k.fingerprint(russh::keys::HashAlg::Sha256).to_string()).unwrap_or_else(|_| t("ui.cannot_parse_public_key").into());
        out.push((format!("~/.ssh/{n}"), kind, fp));
    }
    out
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lang = ssh_term_proto::i18n::current() as u8;
        if self.lang_seen != lang {
            self.lang_seen = lang;
            let phs = [t("ui.rule_id_e_g_allow"), t("ui.principal_ai_claude"), t("ui.command_glob_e_g_make"), t("ui.reason_optional")];
            for (e, ph) in self.new_rule.clone().iter().zip(phs) { e.update(cx, |s, cx| s.set_placeholder(ph, window, cx)); }
            self.search.update(cx, |s, cx| s.set_placeholder(t("ui.search_settings"), window, cx));
            self.tester.update(cx, |s, cx| s.set_placeholder(t("ui.type_a_command_to_test"), window, cx));
            self.ai_key.update(cx, |s, cx| s.set_placeholder(t("ai.key_placeholder"), window, cx));
        }
        let th = cur();
        let query = self.search.read(cx).value().to_string().to_lowercase();
        let rows = self.rows();

        // ---- left: search + categories
        let mut nav = div().w(px(220.)).flex_shrink_0().h_full().flex().flex_col().gap(px(2.)).p_3().bg(c(th.mantle)).border_r_1().border_color(c(th.surface0))
            .child(div().pb_2().child(Input::new(&self.search).prefix(icon(IconName::Search, 14., c(th.overlay1))).cleanable(true)));
        for (i, cat) in Cat::ALL.iter().copied().enumerate() {
            let active = query.is_empty() && cat == self.cat;
            let n_soon = rows.iter().filter(|r| r.cat == cat).count();
            let _ = n_soon;
            nav = nav.child(div().id(("cat", i)).h(px(30.)).px_2().rounded_md().flex().items_center().gap_2().cursor_pointer()
                .bg(if active { c(th.surface0) } else { gpui::transparent_black() })
                .hover(|d| d.bg(c(cur().surface0)))
                .on_click(cx.listener(move |this, _, window, cx| { this.cat = cat; this.search.update(cx, |s, cx| s.set_value("", window, cx)); cx.notify(); }))
                .child(icon(cat.icon(), 15., if active { c(th.accent) } else { c(th.overlay1) }))
                .child(div().text_size(px(12.5)).text_color(if active { c(th.text) } else { c(th.subtext) }).child(cat.name())));
        }

        // ---- right: form
        let mut body = div().flex().flex_col();
        if query.is_empty() {
            body = body.child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.cat.name()))
                .child(div().pt_1().pb_2().text_size(px(12.)).text_color(c(th.overlay1)).child(self.cat.desc()));
            let cur_cat = self.cat;
            for (i, row) in rows.iter().enumerate().filter(|(_, r)| r.cat == cur_cat) {
                body = body.child(self.row_el(row, i, window, cx));
            }
            let extra = match self.cat {
                Cat::Permissions => Some(self.permissions_extra(window, cx)),
                Cat::Credentials => Some(self.credentials_extra()),
                Cat::Agents => Some(self.agents_extra()),
                Cat::Keys => Some(self.keys_extra(window, cx)),
                Cat::Audit => Some(self.audit_extra(cx)),
                Cat::Suggest => Some(self.suggest_extra(cx)),
                Cat::About => Some(self.about_extra()),
                Cat::Ai => Some(self.ai_extra(window, cx)),
                _ => None,
            };
            if let Some(e) = extra { body = body.child(e); }
        } else {
            let hits: Vec<(usize, &Row)> = rows.iter().enumerate().filter(|(_, r)| {
                format!("{} {} {} {}", r.label, r.desc, r.cat.name(), match &r.ctl { Ctl::Toggle(p) | Ctl::Choice(p, _) | Ctl::Text(p, _) => *p, _ => "" }).to_lowercase().contains(&query)
            }).collect();
            body = body.child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(tf("settings.search_results", &[("n", &hits.len())])));
            let mut last = None;
            for (i, row) in hits {
                if last != Some(row.cat) { body = body.child(Self::section_title(row.cat.name())); last = Some(row.cat); }
                body = body.child(self.row_el(row, i, window, cx));
            }
        }

        let header = div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
            .child(icon(IconName::Settings, 16., c(th.accent)))
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.settings")))
            .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child(t("ui.changes_are_saved_to_config")))
            .child(div().flex_1())
            .child(div().id("settings-close").size(px(28.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(|_, _, _w, cx| cx.emit(SettingsEvent::Close)))
                .child(icon(IconName::X, 16., c(th.overlay1))));
        ui::card(1120.).h(px(740.))
            .child(header)
            .child(div().flex_1().min_h_0().flex().flex_row()
                .child(nav)
                .child(div().id("settings-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().px_6().py_4().child(body.w_full())))
    }
}

#[derive(Default, Clone)]
pub struct ModelState { pub loading: bool, pub list: Vec<ModelInfo>, pub fetched_at: Option<i64>, pub error: Option<String>, pub from_cache: bool }

pub fn location_label(l: &Location) -> String {
    match l {
        Location::Keychain => t("ai.loc_keychain").to_string(),
        Location::File(p) => tf("ai.loc_file", &[("path", &p.display())]),
    }
}

// ---------------------------------------------------------------- AI providers
impl SettingsView {
    fn refresh_keys(&mut self) {
        let st = crate::secret_store();
        self.ai_keys = config::get().ai.providers.iter()
            .map(|p| (p.id.clone(), st.get(&p.secret_name()).map(|(k, l)| (secrets::mask(&k), l)))).collect();
    }
    fn update_provider(&mut self, id: &str, f: impl FnOnce(&mut Provider), cx: &mut Context<Self>) {
        let id = id.to_string();
        let err = config::update(|s| { if let Some(p) = s.ai.providers.iter_mut().find(|p| p.id == id) { f(p); } });
        self.notice = err;
        cx.emit(SettingsEvent::Changed("ai.providers".into()));
        cx.notify();
    }
    fn provider_input(&mut self, id: &str, field: &'static str, ph: &str, val: String, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let key = format!("aip.{id}.{field}");
        if let Some(e) = self.inputs.get(&key) { return e.clone(); }
        let ph = ph.to_string();
        let e = cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(val));
        let (pid, k2) = (id.to_string(), key.clone());
        self._subs.push(cx.subscribe_in(&e, window, move |this, st, ev: &InputEvent, _w, cx| {
            if !matches!(ev, InputEvent::Change) { return; }
            let v = st.read(cx).value().trim().to_string();
            let mut bad = false;
            let vv = v.clone();
            this.update_provider(&pid, |p| match field {
                "name" => p.name = vv,
                "base_url" => p.base_url = vv,
                "model" => p.model = vv,
                "timeout_secs" => match vv.parse() { Ok(n) if n > 0 => p.timeout_secs = n, _ => bad = true },
                "temperature" => match vv.parse::<f32>() { Ok(n) if (0.0..=2.0).contains(&n) => p.temperature = n, _ => bad = true },
                "max_tokens" => match vv.parse() { Ok(n) if n > 0 => p.max_tokens = n, _ => bad = true },
                _ => {}
            }, cx);
            if bad { this.errors.insert(k2.clone(), t("ai.bad_number").into()); } else { this.errors.remove(&k2); }
            if field == "base_url" { this.schedule_models(&pid, cx); }
        }));
        self.inputs.insert(key, e.clone());
        e
    }

    fn ai_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let default_id = s.ai.default_provider().map(|p| p.id.clone());
        let mut out = div().flex().flex_col();

        // storage backend banner
        let probe = crate::secret_store();
        let backend = self.ai_keys.values().flatten().next().map(|(_, l)| location_label(l))
            .unwrap_or_else(|| tf("ai.loc_pending", &[("path", &probe.file_path().display())]));
        out = out.child(Self::section_title(t("ai.providers")))
            .child(div().flex().items_center().gap_2().p_2().mb_2().rounded_md().bg(c(th.mantle)).text_size(px(11.5)).text_color(c(th.subtext))
                .child(icon(IconName::KeyRound, 13., c(th.accent))).child(tf("ai.key_storage", &[("where", &backend)])));

        // provider list
        let mut list = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0));
        if s.ai.providers.is_empty() {
            list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("ai.no_providers")));
        }
        for (i, p) in s.ai.providers.iter().enumerate() {
            let sel = self.ai_sel.as_deref() == Some(p.id.as_str());
            let is_def = default_id.as_deref() == Some(p.id.as_str());
            let key = self.ai_keys.get(&p.id).cloned().flatten();
            let pid = p.id.clone();
            let pid2 = p.id.clone();
            let pid3 = p.id.clone();
            list = list.child(div().id(("aip", i)).flex().items_center().gap_3().px_3().py(px(8.)).cursor_pointer()
                .when(i > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                .when(sel, |d| d.bg(c(th.accent).opacity(0.08)))
                .on_click(cx.listener(move |this, _, _w, cx| { this.ai_sel = Some(pid.clone()); this.ai_key_reset(cx); this.ai_dropdown = None; this.load_models(&pid, false, cx); cx.notify(); }))
                .child(icon(IconName::Sparkles, 14., if sel { c(th.accent) } else { c(th.overlay1) }))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(div().flex().items_center().gap_2().text_size(px(13.)).text_color(c(th.text)).child(if p.name.is_empty() { p.id.clone() } else { p.name.clone() })
                        .child(ui::tag(if p.kind == Kind::Anthropic { "Anthropic" } else { t("ai.kind_openai") }, c(th.overlay1)))
                        .when(is_def, |d| d.child(ui::tag(t("ai.default"), c(th.accent)))))
                    .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(format!("{} · {}", p.model, p.base_url))))
                .child(div().text_size(px(11.)).font_family(SharedString::from(s.font.family.clone()))
                    .text_color(if key.is_some() { c(th.success) } else { c(th.warning) })
                    .child(key.as_ref().map(|k| k.0.clone()).unwrap_or_else(|| t("ai.no_key").into())))
                .when(!is_def, |d| d.child(ui::button(SharedString::from(format!("aip-def-{i}")), t("ai.make_default"), 0)
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        this.notice = config::update(|s| s.ai.default_provider = pid2.clone());
                        cx.emit(SettingsEvent::Changed("ai.default_provider".into())); cx.notify();
                    }))))
                .child(div().id(("aip-del", i)).size(px(22.)).rounded_md().flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().error).opacity(0.15)))
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        let name = config::get().ai.providers.iter().find(|p| p.id == pid3).map(|p| p.secret_name());
                        if let Some(n) = name { crate::secret_store().delete(&n); }
                        models::ModelCache::new(&crate::data_dir()).remove(&pid3);
                        this.notice = config::update(|s| { s.ai.providers.retain(|p| p.id != pid3); if s.ai.default_provider == pid3 { s.ai.default_provider = s.ai.providers.first().map(|p| p.id.clone()).unwrap_or_default(); } });
                        if this.ai_sel.as_deref() == Some(pid3.as_str()) { this.ai_sel = config::get().ai.providers.first().map(|p| p.id.clone()); }
                        this.refresh_keys();
                        cx.emit(SettingsEvent::Changed("ai.providers".into())); cx.notify();
                    }))
                    .child(icon(IconName::Trash, 13., c(th.error)))));
        }
        out = out.child(list).child(div().pt_2().flex().gap_2()
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(t("ai.add_provider")))
            .child(self.preset_chips("add", cx)));

        // editor for the selected provider
        let Some(p) = self.ai_sel.as_ref().and_then(|id| s.ai.providers.iter().find(|p| &p.id == id)).cloned() else { return out.into_any_element() };
        out = out.child(Self::section_title(t("ai.edit_provider")));
        let field = |this: &mut Self, label: &str, f: &'static str, ph: &str, val: String, window: &mut Window, cx: &mut Context<Self>| {
            let e = this.provider_input(&p.id, f, ph, val, window, cx);
            let err = this.errors.get(&format!("aip.{}.{f}", p.id)).cloned();
            div().py(px(8.)).flex().items_center().gap_4().border_b_1().border_color(c(cur().surface0))
                .child(div().flex_1().flex().flex_col().text_size(px(13.)).text_color(c(cur().text)).child(label.to_string())
                    .when_some(err, |d, e| d.child(div().text_size(px(11.5)).text_color(c(cur().error)).child(e))))
                .child(div().w(px(320.)).child(Input::new(&e)))
        };
        let pid = p.id.clone();
        let me = cx.entity().downgrade();
        let kind_opts = opts(&[("openai", t("ai.kind_openai")), ("anthropic", "Anthropic")]);
        out = out
            .child(div().py(px(8.)).flex().items_center().gap_4().border_b_1().border_color(c(th.surface0))
                .child(div().w(px(140.)).flex_shrink_0().flex().flex_col().text_size(px(13.)).text_color(c(th.text)).child(t("models.template"))
                    .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(t("models.template.d"))))
                .child(div().flex_1().child(self.preset_chips("apply", cx))))
            .child(field(self, t("ui.name"), "name", "OpenAI", p.name.clone(), window, cx))
            .child(div().py(px(8.)).flex().items_center().gap_4().border_b_1().border_color(c(th.surface0))
                .child(div().flex_1().flex().flex_col().text_size(px(13.)).text_color(c(th.text)).child(t("ai.kind"))
                    .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(t("ai.kind.d"))))
                .child(ui::segmented("aip-kind", &kind_opts, p.kind.id(), move |v, _w, cx| {
                    let pid = pid.clone();
                    let _ = me.update(cx, |this, cx| this.update_provider(&pid, |p| {
                        let k = Kind::from_id(&v);
                        if k != p.kind && k == Kind::Anthropic && p.base_url.contains("openai.com") { p.base_url = "https://api.anthropic.com".into(); p.model = "claude-sonnet-4-5".into(); }
                        p.kind = k;
                    }, cx));
                })))
            .child(field(self, t("ui.base_url"), "base_url", "https://api.openai.com/v1", p.base_url.clone(), window, cx))
            .child(self.model_picker(&p, "model", window, cx))
            .child(self.model_picker(&p, "fast_model", window, cx))
            .child(field(self, t("ai.timeout"), "timeout_secs", "30", p.timeout_secs.to_string(), window, cx))
            .child(field(self, t("ai.temperature"), "temperature", "0.2", p.temperature.to_string(), window, cx))
            .child(field(self, t("ai.max_tokens"), "max_tokens", "512", p.max_tokens.to_string(), window, cx));

        // API key
        let key = self.ai_keys.get(&p.id).cloned().flatten();
        let (sid, sid2, sid3) = (p.id.clone(), p.id.clone(), p.id.clone());
        out = out.child(div().py(px(8.)).flex().items_center().gap_4().border_b_1().border_color(c(th.surface0))
            .child(div().flex_1().flex().flex_col().gap(px(2.)).min_w_0()
                .child(div().text_size(px(13.)).text_color(c(th.text)).child("API Key"))
                .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(match &key {
                    Some((m, l)) => tf("ai.key_saved", &[("mask", m), ("where", &location_label(l))]),
                    None => t("ai.key_none").to_string(),
                }))
                .when_some(self.ai_note.clone(), |d, (ok, n)| d.child(div().text_size(px(11.5)).text_color(if ok { c(th.subtext) } else { c(th.warning) }).child(n))))
            .child(div().w(px(200.)).child(Input::new(&self.ai_key)))
            .child(ui::button("aip-key-save", t("ui.save"), 1).on_click(cx.listener(move |this, _, window, cx| {
                let v = this.ai_key.read(cx).value().trim().to_string();
                if v.is_empty() { return; }
                let name = config::get().ai.providers.iter().find(|p| p.id == sid).map(|p| p.secret_name());
                if let Some(n) = name {
                    this.ai_note = Some(match crate::secret_store().set(&n, &v) {
                        Ok((Location::Keychain, _)) => (true, t("ai.saved_keychain").into()),
                        Ok((l, why)) => (false, tf("ai.saved_fallback", &[("where", &location_label(&l)), ("why", &why.unwrap_or_default().split(": ").last().unwrap_or("").chars().take(80).collect::<String>())])),
                        Err(e) => (false, format!("{e:#}")),
                    });
                }
                this.ai_key.update(cx, |s, cx| s.set_value("", window, cx));
                this.refresh_keys();
                this.load_models(&sid, true, cx);
                cx.notify();
            })))
            .when(key.is_some(), |d| d.child(ui::button("aip-key-del", t("ui.delete"), 2).on_click(cx.listener(move |this, _, _w, cx| {
                let name = config::get().ai.providers.iter().find(|p| p.id == sid2).map(|p| p.secret_name());
                if let Some(n) = name { crate::secret_store().delete(&n); }
                this.ai_note = Some((true, t("ai.key_deleted").into()));
                this.refresh_keys(); cx.notify();
            })))));

        // test connection
        let testing = self.ai_testing.as_deref() == Some(p.id.as_str());
        let res = self.ai_test.get(&p.id).cloned();
        out = out.child(div().py(px(10.)).flex().items_center().gap_3()
            .child(ui::button("aip-test", if testing { t("ai.testing") } else { t("ai.test") }, 1).on_click(cx.listener(move |this, _, _w, cx| {
                if this.ai_testing.is_some() { return; }
                let Some(p) = config::get().ai.providers.iter().find(|p| p.id == sid3).cloned() else { return };
                let key = crate::secret_store().get(&p.secret_name()).map(|x| x.0).unwrap_or_default();
                this.ai_testing = Some(p.id.clone());
                cx.notify();
                let task = cx.background_executor().spawn(async move { ssh_term_proto::ai::test_connection(&p, &key).map(|r| (r.latency, r.model)).map_err(|e| format!("{e:#}")) });
                let id = sid3.clone();
                cx.spawn(async move |this, cx| {
                    let r = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.ai_testing = None;
                        this.ai_test.insert(id, match r {
                            Ok((lat, model)) => (true, tf("ai.test_ok", &[("ms", &lat.as_millis()), ("model", &model)])),
                            Err(e) => (false, tf("ai.test_fail", &[("e", &e)])),
                        });
                        cx.notify();
                    });
                }).detach();
            })))
            .when_some(res, |d, (ok, msg)| d.child(div().flex_1().min_w_0().text_size(px(12.)).text_color(if ok { c(th.success) } else { c(th.error) }).child(msg)))
            .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(t("ai.test.d"))));
        out.into_any_element()
    }
    fn ai_key_reset(&mut self, _cx: &mut Context<Self>) { self.ai_note = None; }

    fn preset_chips(&mut self, mode: &'static str, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let cur_preset = self.ai_sel.as_ref().and_then(|id| config::get().ai.providers.iter().find(|p| &p.id == id).map(|p| p.preset.clone()));
        let mut row = div().flex().flex_wrap().gap_1();
        for (i, pr) in models::PRESETS.iter().enumerate() {
            let active = mode == "apply" && cur_preset.as_deref() == Some(pr.id);
            let label = if pr.id == "custom" { t("models.preset_custom") } else { pr.name };
            row = row.child(div().id(SharedString::from(format!("preset-{mode}-{i}"))).px_2().py(px(3.)).rounded_md().cursor_pointer().text_size(px(11.5))
                .border_1().border_color(if active { c(th.accent) } else { c(th.surface1) })
                .bg(if active { c(th.accent).opacity(0.12) } else { c(th.surface0) }).text_color(c(th.text))
                .hover(|d| d.border_color(c(cur().accent)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    let pr = &models::PRESETS[i];
                    if mode == "add" {
                        let id = format!("p{}", chrono::Local::now().format("%m%d%H%M%S%3f"));
                        let p = Provider { id: id.clone(), name: if pr.id == "custom" { t("models.preset_custom").to_string() } else { pr.name.to_string() }, kind: pr.kind,
                            base_url: pr.base_url.into(), model: pr.model.into(), fast_model: pr.fast_model.into(), preset: pr.id.into(), ..Default::default() };
                        this.notice = config::update(|s| { if s.ai.default_provider.is_empty() { s.ai.default_provider = id.clone(); } s.ai.providers.push(p); });
                        this.ai_sel = Some(id.clone());
                        this.refresh_keys(); this.ai_key_reset(cx);
                        if !pr.needs_key { this.load_models(&id, true, cx); }
                    } else if let Some(id) = this.ai_sel.clone() {
                        this.update_provider(&id, |p| {
                            p.preset = pr.id.into(); p.kind = pr.kind;
                            if !pr.base_url.is_empty() { p.base_url = pr.base_url.into(); }
                            if !pr.model.is_empty() { p.model = pr.model.into(); }
                            p.fast_model = pr.fast_model.into();
                        }, cx);
                        // re-create the bound inputs so they show the new values
                        this.inputs.retain(|k, _| !k.starts_with(&format!("aip.{id}.")));
                        let _ = window;
                        this.load_models(&id, true, cx);
                    }
                    cx.emit(SettingsEvent::Changed("ai.providers".into())); cx.notify();
                }))
                .child(label));
        }
        row
    }

    /// Show cached models at once; fetch when forced, missing or stale.
    fn load_models(&mut self, id: &str, force: bool, cx: &mut Context<Self>) {
        let cfg = config::get();
        let Some(p) = cfg.ai.providers.iter().find(|p| p.id == id).cloned() else { return };
        let base = models::api_base(p.kind, &p.base_url);
        let now = chrono::Utc::now().timestamp();
        let ttl = (cfg.ai.models_cache_hours.max(1) * 3600) as i64;
        let cache = models::ModelCache::new(&crate::data_dir());
        let cached = cache.get(id, &base);
        let st = self.ai_models.entry(id.to_string()).or_default();
        if let Some(e) = &cached { st.list = e.models.clone(); st.fetched_at = Some(e.fetched_at); st.from_cache = true; st.error = None; }
        else if force { st.list.clear(); st.fetched_at = None; }
        let stale = cached.as_ref().map(|e| !models::is_fresh(e, now, ttl)).unwrap_or(true);
        let key = crate::secret_store().get(&p.secret_name()).map(|x| x.0).unwrap_or_default();
        let can = !key.is_empty() || !models::preset(&p.preset).map(|x| x.needs_key).unwrap_or(true) || p.base_url.contains("127.0.0.1") || p.base_url.contains("localhost");
        if !(force || (stale && can)) || p.base_url.trim().is_empty() { cx.notify(); return; }
        st.loading = true; st.error = None;
        cx.notify();
        let dir = crate::data_dir();
        let task = cx.background_executor().spawn(async move {
            let mut list = models::list_models(&p, &key).map_err(|e| format!("{e:#}"))?;
            if let Some(meta) = models::models_dev(&dir, now, 86400) { models::enrich(&mut list, &p.preset, &meta); }
            Ok::<_, String>(list)
        });
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                let st = this.ai_models.entry(id.clone()).or_default();
                st.loading = false;
                match r {
                    Ok(list) => {
                        let _ = models::ModelCache::new(&crate::data_dir()).put(&id, &base, list.clone(), now);
                        st.list = list; st.fetched_at = Some(now); st.from_cache = false; st.error = None;
                    }
                    Err(e) => st.error = Some(e),
                }
                cx.notify();
            });
        }).detach();
    }
    /// Debounced refetch after the base URL was edited.
    fn schedule_models(&mut self, id: &str, cx: &mut Context<Self>) {
        self.ai_url_gen += 1;
        let gen = self.ai_url_gen;
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(900)).await;
            let _ = this.update(cx, |this, cx| if this.ai_url_gen == gen { this.load_models(&id, true, cx) });
        }).detach();
    }

    fn model_picker(&mut self, p: &Provider, field: &'static str, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let val = if field == "model" { p.model.clone() } else { p.fast_model.clone() };
        let e = self.provider_input(&p.id, field, if field == "model" { "gpt-4.1" } else { t("models.fast_ph") }, val.clone(), window, cx);
        let st = self.ai_models.get(&p.id).cloned().unwrap_or_default();
        let chat_only = config::get().ai.chat_models_only;
        let shown = models::filter_chat(&st.list, chat_only);
        let open = self.ai_dropdown == Some(field);
        let pid = p.id.clone();
        let status: (gpui::Hsla, String) = if st.loading { (c(th.accent), t("models.loading").into()) }
            else if let Some(err) = &st.error { (c(th.error), tf("models.error", &[("e", err)])) }
            else if let Some(ts) = st.fetched_at {
                let when = chrono::DateTime::from_timestamp(ts, 0).map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string()).unwrap_or_default();
                (c(th.overlay1), tf(if st.from_cache { "models.count_cached" } else { "models.count" }, &[("n", &shown.len()), ("total", &st.list.len()), ("when", &when)]))
            } else { (c(th.overlay1), t("models.none_yet").into()) };
        let label = if field == "model" { t("ui.model") } else { t("models.fast") };
        let desc = if field == "model" { t("models.model.d") } else { t("models.fast.d") };
        let mut col = div().py(px(8.)).flex().flex_col().gap_1().border_b_1().border_color(c(th.surface0))
            .child(div().flex().items_center().gap_3()
                .child(div().flex_1().flex().flex_col().text_size(px(13.)).text_color(c(th.text)).child(label)
                    .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(desc)))
                .child(div().w(px(240.)).child(Input::new(&e)))
                .child(ui::button(SharedString::from(format!("mdl-open-{field}")), if open { t("models.close_list") } else { t("models.pick") }, 0)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ai_dropdown = if this.ai_dropdown == Some(field) { None } else { Some(field) };
                        this.ai_model_search.update(cx, |s, cx| { s.set_value("", window, cx); s.focus(window, cx); });
                        cx.notify();
                    })))
                .when(field == "model", |d| d.child(ui::button("mdl-refresh", if st.loading { t("models.loading_short") } else { t("models.refresh") }, 1)
                    .on_click(cx.listener(move |this, _, _w, cx| this.load_models(&pid, true, cx))))))
            .when(field == "model", |d| d.child(div().flex().items_center().gap_2().text_size(px(11.5))
                .child(div().flex_1().min_w_0().text_color(status.0).child(status.1))
                .child(div().text_color(c(th.overlay1)).child(t("models.chat_only")))
                .child(Switch::new("mdl-chat-only").small().checked(chat_only).on_click(cx.listener(|this, v: &bool, _w, cx| this.set("ai.chat_models_only", toml::Value::Boolean(*v), cx))))));
        if open {
            let q = self.ai_model_search.read(cx).value().to_string();
            let hits = models::search(&shown, &q);
            let mut list = div().id(SharedString::from(format!("mdl-list-{field}"))).max_h(px(260.)).overflow_y_scroll().flex().flex_col();
            if hits.is_empty() {
                list = list.child(div().p_2().text_size(px(12.)).text_color(c(th.overlay1)).child(if st.list.is_empty() { t("models.empty_manual") } else { t("models.no_match") }));
            }
            for (i, m) in hits.iter().take(300).enumerate() {
                let id = m.id.clone();
                let pid = p.id.clone();
                let sel = m.id == val;
                let mut meta = vec![];
                if let Some(c) = m.context { meta.push(if c >= 1000 { format!("{}K ctx", c / 1000) } else { format!("{c} ctx") }); }
                if let (Some(a), Some(b)) = (m.price_in, m.price_out) { meta.push(format!("${a:.2} / ${b:.2} per 1M")); }
                if m.usage != models::Use::Chat { meta.push(format!("{:?}", m.usage).to_lowercase()); }
                list = list.child(div().id(("mdl", i)).flex_shrink_0().px_2().py(px(5.)).flex().items_center().gap_2().cursor_pointer().rounded_md()
                    .when(sel, |d| d.bg(c(th.accent).opacity(0.12)))
                    .hover(|d| d.bg(c(cur().surface0)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let v = id.clone();
                        this.update_provider(&pid, |p| if field == "model" { p.model = v.clone() } else { p.fast_model = v.clone() }, cx);
                        if let Some(inp) = this.inputs.get(&format!("aip.{pid}.{field}")).cloned() { inp.update(cx, |s, cx| s.set_value(id.clone(), window, cx)); }
                        this.ai_dropdown = None;
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w_0().flex().flex_col()
                        .child(div().text_size(px(12.5)).text_color(c(th.text)).child(m.id.clone()))
                        .when(!m.name.is_empty(), |d| d.child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(m.name.clone()))))
                    .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(meta.join(" · "))));
            }
            col = col.child(div().mt_1().p_2().rounded_lg().border_1().border_color(c(th.surface1)).bg(c(th.mantle)).flex().flex_col().gap_1()
                .child(Input::new(&self.ai_model_search).small().prefix(icon(IconName::Search, 13., c(th.overlay1))))
                .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(tf("models.list_hint", &[("n", &hits.len())])))
                .child(list));
        }
        col
    }
}
