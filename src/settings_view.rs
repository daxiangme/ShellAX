//! Settings page: categories on the left (searchable), form on the right.
//! Every control reads/writes `config::get()` by dotted path and persists config.toml
//! immediately; the root view re-applies what can take effect live.
use ssh_term_proto::i18n::{t, tf};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FontWeight, SharedString, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};
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
pub enum Cat { General, Appearance, Terminal, Suggest, Ssh, Proxy, Credentials, Ai, Agents, Permissions, Tags, Audit, Session, Keys, Notify, ContextMenu, Extensions, About }

impl Cat {
    pub const ALL: [Cat; 18] = [Cat::General, Cat::Appearance, Cat::Terminal, Cat::Suggest, Cat::Ssh, Cat::Proxy, Cat::Credentials, Cat::Ai, Cat::Agents,
        Cat::Permissions, Cat::Tags, Cat::Audit, Cat::Session, Cat::Keys, Cat::Notify, Cat::ContextMenu, Cat::Extensions, Cat::About];
    pub fn name(self) -> &'static str {
        match self {
            Cat::General => t("ui.general"), Cat::Appearance => t("ui.appearance"), Cat::Terminal => t("ui.terminal"), Cat::Suggest => t("ui.suggestions"), Cat::Ssh => "SSH",
            Cat::Proxy => t("ui.proxy"),
            Cat::Credentials => t("ui.credentials_keys"), Cat::Ai => "AI", Cat::Agents => t("ui.external_agents"), Cat::Permissions => t("ui.permissions_security"), Cat::Tags => t("tag.tags"),
            Cat::Audit => t("ui.audit"), Cat::Session => t("ui.sessions_layout"), Cat::Keys => t("ui.keyboard_shortcuts"), Cat::Notify => t("ui.notifications"), Cat::ContextMenu => t("cm.settings_title"), Cat::Extensions => t("ext.title"), Cat::About => t("ui.about"),
        }
    }
    fn icon(self) -> IconName {
        match self {
            Cat::General => IconName::SlidersHorizontal, Cat::Appearance => IconName::Palette, Cat::Terminal => IconName::SquareTerminal,
            Cat::Suggest => IconName::Lightbulb, Cat::Ssh => IconName::Server, Cat::Proxy => IconName::Globe, Cat::Credentials => IconName::KeyRound, Cat::Ai => IconName::Sparkles,
            Cat::Agents => IconName::Bot, Cat::Permissions => IconName::ShieldCheck, Cat::Tags => IconName::Tag, Cat::Audit => IconName::ScrollText, Cat::Session => IconName::Layers,
            Cat::Keys => IconName::Keyboard, Cat::Notify => IconName::Bell, Cat::ContextMenu => IconName::FileText, Cat::Extensions => IconName::Puzzle, Cat::About => IconName::Info,
        }
    }
    fn desc(self) -> &'static str {
        match self {
            Cat::General => t("ui.language_startup_behaviour_and_updates"),
            Cat::Appearance => t("ui.theme_fonts_and_window_appearance"),
            Cat::Terminal => t("ui.terminal_behaviour_cursor_copy_paste"),
            Cat::Suggest => t("ui.history_based_grey_command_suggestions"),
            Cat::Ssh => t("ui.ssh_defaults_and_host_key"),
            Cat::Proxy => t("ui.proxy_desc"),
            Cat::Credentials => t("ui.private_keys_ssh_agent_and"),
            Cat::Ai => t("ui.built_in_ai_bring_your"),
            Cat::Agents => t("ui.detect_claude_code_codex_grok"),
            Cat::Permissions => t("ui.permission_mode_and_rules_for"),
            Cat::Tags => t("tag.settings_desc"),
            Cat::Audit => t("ui.append_only_jsonl_audit_log"),
            Cat::Session => t("ui.persistent_sessions_layout_restore_and"),
            Cat::Keys => t("ui.app_shortcuts_type_e_g"),
            Cat::Notify => t("ui.desktop_notifications_and_sounds"),
            Cat::ContextMenu => t("cm.settings_desc"),
            Cat::Extensions => t("ext.settings_desc"),
            Cat::About => t("ui.version_information_and_third_party"),
        }
    }
}

enum Ctl { Toggle(&'static str), Choice(&'static str, Vec<(String, String)>), Text(&'static str, &'static str), Font(&'static str), Info(String) }

struct Row { cat: Cat, label: &'static str, desc: &'static str, ctl: Ctl }

fn opts(v: &[(&str, &str)]) -> Vec<(String, String)> { v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }

pub enum SettingsEvent { Changed(String), Close, OpenAudit, ThemesChanged, ApplyUpdate(ssh_term_proto::updater::Record) }

pub struct SettingsView {
    pub cat: Cat,
    search: Entity<InputState>,
    inputs: HashMap<String, Entity<InputState>>,
    errors: HashMap<String, String>,
    theme_names: Vec<(String, bool)>,
    /// Theme thumbnails for the appearance card grid: name -> (bg, surface, text, accent).
    theme_swatch: HashMap<String, [u32; 4]>,
    scheme_names: Vec<(String, bool)>,
    // rule editor
    new_rule: [Entity<InputState>; 4],
    new_effect: Effect,
    tester: Entity<InputState>,
    notice: Option<String>,
    /// Newer release found by "Check now".
    update_found: Option<ssh_term_proto::update::Manifest>,
    update_err_open: bool,
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
    key_pass: Entity<InputState>,
    key_import: Entity<InputState>,
    theme_import: Entity<InputState>,
    /// context_menu.toml editor (created when the section is first shown) and last save result.
    cm_editor: Option<Entity<gpui_component::input::EditorState>>,
    cm_note: Option<(bool, String)>,
    proxy_http: bool,
    proxy_auth: bool,
    proxy_host: Entity<InputState>,
    proxy_port: Entity<InputState>,
    proxy_user: Entity<InputState>,
    proxy_pass: Entity<InputState>,
    proxy_candidates: Vec<ssh_term_proto::proxy::Candidate>,
    proxy_bypass_suggest: String,
    proxy_detecting: bool,
    proxy_started: bool,
    proxy_note: Option<(bool, String)>,
    font_dropdown: bool,
    font_search: Entity<InputState>,
    font_show_all: bool,
    system_term_font: Option<ssh_term_proto::platform::DetectedTerminalFont>,
    ai_add_dropdown: bool,
    ai_template_dropdown: bool,
    /// Window-space bounds of the add / template buttons, recorded during prepaint.
    /// The menu anchors to these so it can flip above the button instead of being
    /// clipped by the settings sheet.
    ai_add_anchor: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    ai_tpl_anchor: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// Click point used only when the button has not been measured yet.
    ai_menu_at: Option<gpui::Point<gpui::Pixels>>,
    ai_advanced: bool,
    /// 扩展管理（由主窗口创建后注入，与工具面板共用）。
    pub ext: Option<Entity<crate::ext_ui::ExtManager>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(cat: Cat, theme_names: Vec<(String, bool)>, scheme_names: Vec<(String, bool)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.search_settings")));
        let mk = |ph: &str, v: &str, window: &mut Window, cx: &mut Context<Self>| { let (ph, v) = (ph.to_string(), v.to_string()); cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v)) };
        let new_rule = [mk(t("ui.rule_id_e_g_allow"), "", window, cx), mk(t("ui.principal_ai_claude"), "*", window, cx), mk(t("ui.command_glob_e_g_make"), "", window, cx), mk(t("ui.reason_optional"), "", window, cx)];
        let tester = mk(t("ui.type_a_command_to_test"), "git push --force origin main", window, cx);
        let mut subs = vec![cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() })];
        subs.push(cx.subscribe_in(&tester, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
        let ai_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(t("ai.key_placeholder")));
        let ai_model_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("models.search")));
        subs.push(cx.subscribe_in(&ai_model_search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
        let key_pass = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(t("ui.key_passphrase_optional")));
        let key_import = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.key_import_path")));
        let theme_import = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.theme_import_path")));
        let font_search = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.search_fonts")));
        subs.push(cx.subscribe_in(&font_search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
        let installed_fonts = cx.text_system().all_font_names();
        let system_term_font = ssh_term_proto::platform::detect_system_terminal_font(&installed_fonts);
        let saved = ssh_term_proto::proxy::parse(config::get().ssh.proxy.trim()).ok();
        let proxy_http = matches!(saved.as_ref().map(|p| &p.kind), Some(ssh_term_proto::proxy::Kind::Http));
        let proxy_auth = saved.as_ref().is_some_and(|p| p.user.is_some());
        let proxy_host = mk(t("proxy.addr"), saved.as_ref().map(|p| p.host.as_str()).unwrap_or("127.0.0.1"), window, cx);
        let proxy_port = mk("7890", &saved.as_ref().map(|p| p.port.to_string()).unwrap_or_default(), window, cx);
        let proxy_user = mk(t("proxy.user"), &saved.as_ref().and_then(|p| p.user.as_ref()).map(|u| u.0.as_str()).unwrap_or(""), window, cx);
        let proxy_pass = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(t("proxy.pass")).default_value(saved.as_ref().and_then(|p| p.user.as_ref()).map(|u| u.1.clone()).unwrap_or_default()));
        let mut me = Self { cm_editor: None, cm_note: None, key_pass, key_import, theme_import, cat, search, inputs: HashMap::new(), errors: HashMap::new(), theme_swatch: crate::theme::load_all().iter().map(|t| (t.name.clone(), [t.base, t.crust, t.text, t.accent])).collect(), theme_names, scheme_names, new_rule, new_effect: Effect::Allow, tester, notice: None, update_found: None, update_err_open: false,
            ai_sel: config::get().ai.default_provider().map(|p| p.id.clone()), ai_key, ai_keys: HashMap::new(), ai_note: None, ai_test: HashMap::new(), ai_testing: None, lang_seen: ssh_term_proto::i18n::current() as u8,
            ai_models: HashMap::new(), ai_dropdown: None, ai_model_search, ai_url_gen: 0,
            proxy_http, proxy_auth, proxy_host, proxy_port, proxy_user, proxy_pass, proxy_candidates: Vec::new(), proxy_bypass_suggest: String::new(), proxy_detecting: false, proxy_started: false, proxy_note: None,
            font_dropdown: false, font_search, font_show_all: false, system_term_font,
            ai_add_dropdown: false, ai_template_dropdown: false,
            ai_add_anchor: Rc::new(Cell::new(None)), ai_tpl_anchor: Rc::new(Cell::new(None)), ai_menu_at: None,
            ai_advanced: false, ext: None, _subs: subs };
        if let Some(id) = me.ai_sel.clone() { me.load_models(&id, false, cx); }
        me.refresh_keys();
        if cat == Cat::Proxy { me.start_proxy_detect(false, cx); }
        me
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Switch category in place when the settings tab is already open (no second page, no lost
    /// search/category state).
    pub fn select_category(&mut self, cat: Cat, window: &mut Window, cx: &mut Context<Self>) {
        self.cat = cat;
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        if cat == Cat::Proxy { self.start_proxy_detect(false, cx); }
        self.focus_search(window, cx);
        cx.notify();
    }

    fn rows(&self) -> Vec<Row> {
        let themes: Vec<(String, String)> = std::iter::once(("system".to_string(), t("ui.follow_system").to_string()))
            .chain(self.theme_names.iter().map(|n| (n.0.clone(), crate::theme::display_name(&n.0)))).collect();
        let light: Vec<(String, String)> = self.theme_names.iter().filter(|n| !n.1).map(|n| (n.0.clone(), crate::theme::display_name(&n.0))).collect();
        let dark: Vec<(String, String)> = self.theme_names.iter().filter(|n| n.1).map(|n| (n.0.clone(), crate::theme::display_name(&n.0))).collect();
        let r = |cat, label, desc, ctl| Row { cat, label, desc, ctl };
        use Cat::*;
        vec![
            r(General, t("ui.interface_language"), t("ui.applies_immediately_follow_system_reads"), Ctl::Choice("general.language", opts(&[("system", t("ui.follow_system")), ("zh-CN", "简体中文"), ("en", "English")]))),
            r(General, t("ui.on_startup"), t("ui.what_to_open_when_the"), Ctl::Choice("general.startup", opts(&[("local", t("ui.local_terminal")), ("restore", t("ui.restore_layout")), ("blank", t("ui.blank"))]))),
            r(General, t("ui.check_for_updates_automatically"), "", Ctl::Toggle("general.check_updates")),
            r(General, t("update.auto_download"), "", Ctl::Toggle("general.auto_download")),
            r(General, t("ui.update_channel"), "", Ctl::Choice("general.update_channel", opts(&[("stable", t("ui.stable")), ("beta", t("ui.beta"))]))),
            r(General, t("ui.tray"), t("ui.tray_desc"), Ctl::Toggle("general.tray")),
            r(General, t("ui.config_file"), t("ui.all_settings_are_stored_here"), Ctl::Info(config::config_path().display().to_string())),

            r(Appearance, t("ui.theme"), t("ui.follow_system_switches_with_the"), Ctl::Choice("appearance.theme", themes)),
            r(Appearance, t("ui.light_theme"), t("ui.used_in_light_mode_when"), Ctl::Choice("appearance.light_theme", light)),
            r(Appearance, t("ui.dark_theme"), t("ui.used_in_dark_mode_when"), Ctl::Choice("appearance.dark_theme", dark)),
            r(Appearance, t("ui.reduce_motion"), t("ui.reduce_motion_desc"), Ctl::Toggle("appearance.reduce_motion")),
            r(Appearance, t("ui.dock_multi"), t("ui.dock_multi_desc"), Ctl::Toggle("sidebar.dock_multi")),
            r(Appearance, t("ui.terminal_font"), t("ui.primary_font_monospace"), Ctl::Font("font.family")),
            r(Appearance, t("ui.fallback_fonts"), t("ui.comma_separated_used_in_order"), Ctl::Text("font.fallbacks", "Noto Sans Mono CJK SC, …")),
            r(Appearance, t("ui.font_size"), t("ui.in_pt_8_32"), Ctl::Text("font.size", "13.5")),
            r(Appearance, t("ui.line_height"), t("ui.multiple_of_the_font_size"), Ctl::Text("font.line_height", "1.4")),
            r(Appearance, t("ui.ligatures"), t("ui.show_etc_as_ligatures"), Ctl::Toggle("font.ligatures")),
            r(Appearance, t("ui.window_opacity"), "0.5–1.0", Ctl::Text("appearance.opacity", "1.0")),
            r(Appearance, t("ui.ui_font_size"), t("ui.sidebar_tabs_and_other_ui"), Ctl::Text("appearance.ui_font_size", "13")),

            r(Terminal, t("ui.default_shell"), t("ui.empty_uses_shell_applies_to"), Ctl::Text("terminal.shell", "/bin/zsh")),
            r(Terminal, t("ui.term_padding"), t("ui.term_padding_desc"), Ctl::Text("terminal.padding", "8")),
            r(Terminal, t("ui.show_cwd"), t("ui.show_cwd_desc"), Ctl::Toggle("terminal.show_cwd")),
            r(Terminal, t("ui.style_optimize"), t("ui.style_optimize_desc"), Ctl::Toggle("terminal.style")),
            r(Terminal, t("ui.scrollback_lines"), t("ui.applies_to_new_sessions"), Ctl::Text("terminal.scrollback", "10000")),
            r(Terminal, t("ui.cursor_style"), "", Ctl::Choice("terminal.cursor_style", opts(&[("block", t("ui.block")), ("bar", t("ui.bar")), ("underline", t("ui.underline"))]))),
            r(Terminal, t("ui.cursor_blink"), t("ui.stops_blinking_after_10_s"), Ctl::Toggle("terminal.cursor_blink")),
            r(Terminal, t("ui.bell"), t("ui.on_bel"), Ctl::Choice("terminal.bell", opts(&[("none", t("ui.off")), ("visual", t("ui.flash_border")), ("sound", t("ui.sound"))]))),
            r(Terminal, t("ui.block_select"), t("ui.block_select_desc"), Ctl::Choice("terminal.block_select", opts(&[("alt", "Alt"), ("ctrl", "Ctrl")]))),
            r(Terminal, t("ui.copy_on_select"), t("ui.copy_to_the_clipboard_on"), Ctl::Toggle("terminal.copy_on_select")),
            r(Terminal, t("ui.confirm_multi_line_paste"), t("ui.confirm_before_pasting_multiple_lines"), Ctl::Toggle("terminal.paste_confirm")),
            r(Terminal, t("ui.mouse_reporting"), t("ui.let_vim_htop_etc_receive"), Ctl::Toggle("terminal.mouse_reporting")),
            r(Terminal, t("ui.shell_integration_osc_133"), t("ui.inject_prompt_marks_into_bash"), Ctl::Choice("terminal.shell_integration", opts(&[("auto", t("ui.auto_inject")), ("off", t("ui.off"))]))),

            r(Suggest, t("ui.enable_suggestions"), "", Ctl::Toggle("suggest.enabled")),
            r(Suggest, t("ui.hist_popup"), t("ui.hist_popup.d"), Ctl::Toggle("suggest.popup")),
            r(Suggest, t("ui.accept_key"), "", Ctl::Choice("suggest.accept", opts(&[("both", t("ui.or_tab")), ("right", t("ui.only")), ("tab", t("ui.tab_only"))]))),
            r(Suggest, t("ui.local_history"), t("ui.record_commands_typed_in_this"), Ctl::Toggle("suggest.local_history")),
            r(Suggest, t("ui.remote_history"), t("ui.read_remote_bash_history_and"), Ctl::Toggle("suggest.remote_history")),
            r(Suggest, t("ui.ai_suggest"), t("ui.ai_suggest.d"), Ctl::Toggle("suggest.ai")),
            r(Suggest, t("ui.sensitive_command_filters"), t("ui.regexes_comma_separated_combined_with"), Ctl::Text("suggest.filter_patterns", "(?i)vault +login")),

            r(Proxy, t("ui.proxy"), t("ui.proxy_desc"), Ctl::Info(match ssh_term_proto::proxy::parse(config::get().ssh.proxy.trim()) {
                Ok(p) => ssh_term_proto::proxy::candidate_label(&ssh_term_proto::proxy::Candidate { kind: p.kind, host: p.host, port: p.port, user: p.user, source: ssh_term_proto::proxy::DetectSource::System }),
                Err(_) => t("proxy.current_none").into(),
            })),
            r(Ssh, t("ui.default_user"), t("ui.used_when_a_host_has"), Ctl::Text("ssh.default_user", "$USER")),
            r(Ssh, t("ui.default_port"), "", Ctl::Text("ssh.default_port", "22")),
            r(Ssh, t("ui.keepalive_interval_s"), t("ui.0_disables"), Ctl::Text("ssh.keepalive_secs", "30")),
            r(Ssh, t("ui.connect_timeout_s"), t("ui.tcp_connect_phase"), Ctl::Text("ssh.connect_timeout_secs", "15")),
            r(Ssh, t("ui.known_hosts_file"), "", Ctl::Text("ssh.known_hosts", "~/.ssh/known_hosts")),
            r(Ssh, t("ui.unknown_host_keys"), t("ui.what_to_do_on_first"), Ctl::Choice("ssh.host_key_policy", opts(&[("ask", t("ui.ask")), ("strict", t("ui.strict_reject")), ("accept-new", t("ui.accept_new_hosts"))]))),
            r(Ssh, t("ui.read_ssh_config"), t("ui.list_its_host_entries_in"), Ctl::Toggle("ssh.read_ssh_config")),
            r(Ssh, t("ui.ssh_config_path"), "", Ctl::Text("ssh.ssh_config", "~/.ssh/config")),
            r(Ssh, t("ui.default_jump_host"), t("ui.proxyjump_e_g_bastion"), Ctl::Text("ssh.jump_host", "user@bastion:22")),
            r(Ssh, t("ui.cipher_preference"), t("ui.comma_separated_by_priority"), Ctl::Text("ssh.ciphers", "")),
            r(Ssh, t("ui.kex_preference"), t("ui.comma_separated_by_priority"), Ctl::Text("ssh.kex", "curve25519-sha256")),
            r(Ssh, t("ui.agent_forwarding"), "", Ctl::Toggle("ssh.agent_forwarding")),
            r(Ssh, t("ssh.clip_write"), t("ssh.clip_write.d"), Ctl::Choice("ssh.osc52_write", opts(&[("allow", t("ui.allow")), ("ask", t("ui.ask")), ("deny", t("ui.deny"))]))),
            r(Ssh, t("ssh.clip_read"), t("ssh.clip_read.d"), Ctl::Toggle("ssh.osc52_read")),

            r(Credentials, t("ui.use_system_keychain"), t("ui.store_key_passphrases_and_api"), Ctl::Toggle("credentials.use_keychain")),
            r(Credentials, t("ui.use_ssh_agent"), "", Ctl::Toggle("credentials.use_ssh_agent")),
            r(Credentials, t("ui.extra_private_keys"), t("ui.comma_separated_paths_tried_after"), Ctl::Text("credentials.extra_keys", "~/.ssh/work_ed25519")),

            r(Ai, t("ui.enable_built_in_ai"), t("ai.enable.d"), Ctl::Toggle("ai.enabled")),
            r(Ai, t("ui.redact_before_sending"), t("ui.strip_passwords_tokens_private_keys"), Ctl::Toggle("ai.redact")),
            r(Ai, t("ui.context_scope"), t("ai.context.d"), Ctl::Choice("ai.context", opts(&[("none", t("ai.ctx_none")), ("command", t("ui.last_command")), ("selection", t("ui.selection_only")), ("screen", t("ui.current_screen"))]))),
            r(Ai, t("ai.context_lines"), t("ai.context_lines.d"), Ctl::Text("ai.context_lines", "40")),
            r(Ai, t("ui.allow_sending_command_output"), t("ui.when_off_only_the_command"), Ctl::Toggle("ai.send_output")),
            r(Terminal, t("ui.keyword_highlight"), t("ui.keyword_highlight_desc"), Ctl::Toggle("terminal.keyword_highlight")),
            r(Ai, t("ui.ai_no_output_hosts"), t("ui.ai_no_output_hosts_desc"), Ctl::Text("ai.no_output_hosts", "prod-*, db-*")),
            r(Ai, t("ui.ai_feed_back"), t("ui.ai_feed_back_desc"), Ctl::Toggle("ai.feed_back_results")),
            r(Ai, t("ui.ai_first_send_ack"), t("ui.ai_first_send_ack_desc"), Ctl::Toggle("ai.first_send_ack")),
            r(Ai, t("ai.nl_prefix"), t("ai.nl_prefix.d"), Ctl::Text("ai.nl_prefix", "#")),

            r(Agents, t("ui.detect_claude_code"), "", Ctl::Toggle("agents.detect_claude")),
            r(Agents, t("ui.detect_codex"), "", Ctl::Toggle("agents.detect_codex")),
            r(Agents, t("ui.detect_grok"), "", Ctl::Toggle("agents.detect_grok")),
            r(Agents, t("ui.notify_on_state_change"), t("ui.desktop_notification_when_an_agent"), Ctl::Toggle("agents.notify_state")),
            r(Agents, t("ui.approval_timeout_s"), t("ui.how_long_to_wait_after"), Ctl::Text("agents.approval_timeout_secs", "120")),
            r(Agents, t("ui.on_timeout"), "", Ctl::Choice("agents.on_timeout", opts(&[("deny", t("ui.deny")), ("ask", t("ui.keep_waiting"))]))),
            r(Agents, t("ui.write_to_pty"), t("ui.write_to_pty_desc"), Ctl::Choice("agents.write_to_pty", opts(&[("ask", "ask"), ("ask_first", "ask_first"), ("allow", "allow"), ("deny", "deny")]))),

            r(Permissions, t("ui.default_mode"), t("ui.also_switchable_from_the_status"), Ctl::Choice("permissions.mode", opts(&[("ask", t("ui.ask_2")), ("plan", t("ui.plan_read_only")), ("auto", t("ui.auto"))]))),
            r(Permissions, t("ui.max_auto_risk_in_auto"), t("ui.actions_at_or_below_this"), Ctl::Choice("permissions.max_auto_risk", opts(&[("R0", "R0"), ("R1", "R1"), ("R2", "R2")]))),
            r(Permissions, t("ui.confirm_writes_on_production_hosts"), t("ui.always_ask_for_r1_actions"), Ctl::Toggle("permissions.prod_confirm")),
            r(Permissions, t("ui.double_confirm_dangerous_commands"), t("ui.r4_actions_need_confirmation_even"), Ctl::Toggle("permissions.dangerous_confirm")),

            r(Audit, t("ui.log_directory"), "", Ctl::Text("audit.dir", "~/.local/share/shellax/audit")),
            r(Audit, t("ui.retention_days"), "", Ctl::Text("audit.retention_days", "90")),
            r(Audit, t("ui.log_manually_typed_commands"), t("ui.by_default_only_connections_permissions"), Ctl::Toggle("audit.log_commands")),

            r(Session, t("ui.persistent_sessions"), t("ui.resume_via_remote_tmux_after"), Ctl::Choice("session.persistent", opts(&[("off", t("ui.off")), ("tmux", "tmux")]))),
            r(Session, t("sftp.follow_cwd"), t("sftp.follow_cwd.d"), Ctl::Toggle("session.sftp_follow_cwd")),
            r(Session, t("ui.local_daemon"), t("ui.local_daemon.d"), Ctl::Toggle("session.local_daemon")),
            r(Session, t("ui.restore_layout_on_startup"), "", Ctl::Toggle("session.restore_layout")),
            r(Session, t("ui.prompt_resume_agent"), t("ui.prompt_resume_agent.d"), Ctl::Toggle("session.prompt_resume_agent")),
            r(Session, t("sb.confirm_new_tab_setting"), t("sb.confirm_new_tab_desc"), Ctl::Toggle("session.confirm_new_tab")),
            r(Session, t("ui.confirm_before_closing"), t("ui.confirm_closing_a_tab_while"), Ctl::Toggle("session.confirm_close")),
            r(Session, t("ui.auto_reconnect"), "", Ctl::Toggle("session.auto_reconnect")),

            r(Notify, t("ui.desktop_notifications"), "", Ctl::Toggle("notify.desktop")),
            r(Notify, t("ui.notify_when_an_agent_waits"), "", Ctl::Toggle("notify.agent_blocked")),
            r(Notify, t("ui.long_command_notification_s"), t("ui.when_a_command_runs_longer"), Ctl::Text("notify.long_command_secs", "10")),
            r(Notify, t("ui.sound"), "", Ctl::Toggle("notify.sound")),
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
            if let Some(rest) = k.strip_prefix("tagdef.") {
                // tagdef.<name|color>.<id>
                let (field, id) = rest.split_once('.').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
                let builtin = ssh_term_proto::hosttree::COLORS.contains(&id.as_str());
                if field == "color" && ssh_term_proto::hosttree::parse_hex(&v).is_none() { this.errors.insert(k.clone(), t("tag.bad_hex").into()); cx.notify(); return; }
                this.errors.remove(&k);
                let err = config::update(|s| {
                    if builtin { if v.trim().is_empty() { s.tags.names.remove(&id); } else { s.tags.names.insert(id.clone(), v.trim().to_string()); } }
                    else if let Some(d) = s.tags.custom.iter_mut().find(|d| d.id == id) {
                        if field == "color" { d.color = format!("#{}", v.trim().trim_start_matches('#').to_lowercase()); } else { d.name = v.trim().to_string(); }
                    }
                });
                if let Some(e) = err { this.errors.insert(k.clone(), e); }
                cx.emit(SettingsEvent::Changed("tags".into()));
                cx.notify();
            } else if let Some(id) = k.strip_prefix("keys.") {
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
                let on = move |v: String, _w: &mut Window, cx: &mut gpui::App| { let _ = me.update(cx, |this, cx| this.set(p, toml::Value::String(v), cx)); };
                // long lists (themes) become a wrapping card grid instead of an overflowing segmented control
                if options.len() > 4 {
                    let sw = self.theme_swatch.clone();
                    ui::choice_grid(p, options, &s.get_str(p), move |v| sw.get(v).copied(), on).into_any_element()
                }
                else { ui::segmented(p, options, &s.get_str(p), on).into_any_element() }
            }
            Ctl::Text(path, ph) => {
                let e = self.input(path, ph, s.get_str(path), window, cx);
                // width follows content: numbers 80, lists 320, other text 260
                let w = match *path { "font.size" | "font.line_height" | "appearance.opacity" | "appearance.ui_font_size" => 80., "font.fallbacks" => 320., _ => 260. };
                div().w(px(w)).flex_shrink_0().child(ui::text_field(&e, false)).into_any_element()
            }
            Ctl::Font(path) => self.font_picker(path, window, cx).into_any_element(),
            Ctl::Info(text) => div().max_w(px(360.)).text_size(px(ui::fs_small())).text_color(c(th.subtext)).text_align(gpui::TextAlign::Right).child(text.clone()).into_any_element(),
        }
    }

    fn font_picker(&mut self, path: &'static str, _window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let s = config::get();
        let current = s.get_str(path);
        let current_display = if current.trim().is_empty() { "Cascadia Mono".to_string() } else { current.clone() };
        let open = self.font_dropdown;
        let installed = cx.text_system().all_font_names();
        let query = self.font_search.read(cx).value().trim().to_lowercase();
        let show_all = self.font_show_all;

        let is_mono = |name: &str| -> bool {
            let l = name.to_lowercase();
            l.contains("mono") || l.contains("nerd font") || l.contains("code") || l.contains("consolas")
                || l.contains("courier") || l.contains("fixed") || l.contains("terminal") || l.contains("hack")
                || l.contains("fira") || l.contains("jetbrains") || l.contains("cascadia") || l.contains("inconsolata")
                || l.contains("menlo") || l.contains("source code") || l.ends_with(" nf") || l.ends_with(" nfm")
                || l.ends_with(" nfp")
        };

        let mut filtered: Vec<String> = installed.into_iter()
            .filter(|f| show_all || is_mono(f))
            .filter(|f| query.is_empty() || f.to_lowercase().contains(&query))
            .collect();
        filtered.sort_by(|a, b| {
            let a_mono = is_mono(a);
            let b_mono = is_mono(b);
            if a_mono != b_mono { b_mono.cmp(&a_mono) } else { a.cmp(b) }
        });
        filtered.dedup();

        let mut trigger = div().relative().w(px(260.));
        let button = div()
            .id("font-select-btn")
            .w_full()
            .h(px(32.))
            .px_3()
            .rounded(px(ui::R_MD))
            .bg(c(th.surface0).opacity(0.85))
            .border_1()
            .border_color(if open { c(th.accent) } else { c(th.surface1) })
            .flex()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .hover(|d| d.bg(c(th.surface1)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.font_dropdown = !this.font_dropdown;
                if this.font_dropdown {
                    this.font_search.update(cx, |s, cx| { s.set_value("", window, cx); s.focus(window, cx); });
                }
                cx.notify();
            }))
            .child(
                div().flex().items_center().gap_2().min_w_0()
                    .child(icon(IconName::SquareTerminal, ui::IC_SM, c(th.accent)))
                    .child(
                        div().text_size(px(ui::fs_body())).text_color(c(th.text))
                            .overflow_hidden().text_ellipsis().whitespace_nowrap()
                            .child(current_display)
                    )
            )
            .child(icon(if open { IconName::ChevronUp } else { IconName::ChevronDown }, ui::IC_XS, c(th.overlay1)));
        trigger = trigger.child(button);

        if open {
            let mut popover = ui::popover()
                .id("font-picker-popover")
                .occlude()
                .absolute()
                .top(px(36.))
                .right_0()
                .w(px(340.))
                .max_h(px(360.))
                .p(px(8.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.font_dropdown = false;
                    cx.notify();
                }));

            // Search input
            popover = popover.child(
                div().w_full().child(ui::search_field(&self.font_search, false))
            );

            // Filter bar: Monospace toggle
            let show_all_val = self.font_show_all;
            popover = popover.child(
                div().flex().items_center().justify_between().px_1().py(px(2.))
                    .child(div().text_size(px(ui::fs_mono())).text_color(c(th.subtext)).child(t("ui.monospace_only")))
                    .child(Switch::new("font-mono-only").small().checked(!show_all_val).on_click(cx.listener(|this, on: &bool, _, cx| {
                        this.font_show_all = !*on;
                        cx.notify();
                    })))
            );

            // Detected system terminal font suggestion if available
            if let Some(det) = &self.system_term_font {
                let det_fam = det.family.clone();
                let det_src = det.source.clone();
                let is_current = det.family.eq_ignore_ascii_case(&current);
                popover = popover.child(
                    div().id("font-detected-item")
                        .px_2().py(px(5.)).rounded(px(ui::R_MD))
                        .bg(crate::theme::selected_bg())
                        .border_1().border_color(c(th.accent).opacity(0.35))
                        .flex().items_center().justify_between().cursor_pointer()
                        .hover(|d| d.bg(c(th.accent).opacity(0.2)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set(path, toml::Value::String(det_fam.clone()), cx);
                            this.font_dropdown = false;
                            cx.notify();
                        }))
                        .child(
                            div().flex().flex_col().gap(px(1.)).min_w_0()
                                .child(
                                    div().flex().items_center().gap_1()
                                        .child(icon(IconName::Sparkles, ui::IC_XS, c(th.accent)))
                                        .child(div().text_size(px(ui::fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.accent)).child(format!("{} ({})", t("ui.system_terminal_font"), det_src)))
                                )
                                .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(det.family.clone()))
                        )
                        .child(
                            if is_current {
                                icon(IconName::Check, ui::IC_SM, c(th.accent)).into_any_element()
                            } else {
                                ui::tag(t("ui.use_detected"), c(th.accent)).into_any_element()
                            }
                        )
                );
            }

            // Font list
            let mut list = div().id("font-list-scroll").max_h(px(200.)).overflow_y_scroll().flex().flex_col().gap(px(2.));
            if filtered.is_empty() {
                list = list.child(div().p_3().text_center().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ui.no_matching_fonts")));
            } else {
                for (i, font_name) in filtered.into_iter().take(200).enumerate() {
                    let fam = font_name.clone();
                    let is_active = font_name.eq_ignore_ascii_case(&current);
                    let is_nerd = font_name.to_lowercase().contains("nerd font") || font_name.ends_with(" NF") || font_name.ends_with(" NFM");
                    list = list.child(
                        div().id(("font-opt", i))
                            .px_2().py(px(5.)).rounded(px(ui::R_MD))
                            .flex().items_center().justify_between().cursor_pointer()
                            .when(is_active, |d| d.bg(crate::theme::selected_bg()))
                            .hover(|d| d.bg(crate::theme::hover_bg()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set(path, toml::Value::String(fam.clone()), cx);
                                this.font_dropdown = false;
                                cx.notify();
                            }))
                            .child(
                                div().flex().items_center().gap_2().min_w_0()
                                    .child(
                                        div().text_size(px(ui::fs_body())).text_color(if is_active { c(th.accent) } else { c(th.text) })
                                            .overflow_hidden().text_ellipsis().whitespace_nowrap()
                                            .child(font_name)
                                    )
                                    .when(is_nerd, |d| d.child(ui::tag("NF", c(th.info))))
                            )
                            .when(is_active, |d| d.child(icon(IconName::Check, ui::IC_XS, c(th.accent))))
                    );
                }
            }
            popover = popover.child(list);

            // Option to use the typed custom name if query is not empty
            if !query.is_empty() {
                let custom_q = self.font_search.read(cx).value().trim().to_string();
                let custom_label = tf("ui.use_custom_font_name", &[("name", &custom_q)]);
                popover = popover.child(
                    div().id("font-custom-use")
                        .px_2().py(px(4.)).rounded(px(ui::R_MD))
                        .border_t_1().border_color(crate::theme::divider())
                        .flex().items_center().gap_2().cursor_pointer()
                        .hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set(path, toml::Value::String(custom_q.clone()), cx);
                            this.font_dropdown = false;
                            cx.notify();
                        }))
                        .child(icon(IconName::Plus, ui::IC_XS, c(th.subtext)))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(custom_label))
                );
            }

            trigger = trigger.child(gpui::deferred(popover).with_priority(3));
        }

        trigger
    }

    fn row_el(&mut self, row: &Row, idx: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let ctl = self.control(&row.ctl, window, cx);
        let path = match &row.ctl {
            Ctl::Toggle(p) | Ctl::Choice(p, _) | Ctl::Text(p, _) | Ctl::Font(p) => Some(*p),
            _ => None,
        };
        let err = path.and_then(|p| self.errors.get(p).cloned());
        let mut left = div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).whitespace_normal().child(row.label));
        if !row.desc.is_empty() {
            left = left.child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).whitespace_normal().child(row.desc));
        }
        if let Some(e) = err {
            left = left.child(div().text_size(px(ui::fs_body())).text_color(c(th.error)).whitespace_normal().child(e));
        }
        div().id(("row", idx))
            .px(px(16.))
            .py(px(10.))
            .min_h(px(48.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .child(left)
            .child(div().flex_shrink_0().flex().items_center().justify_end().child(ctl))
            .into_any_element()
    }
    /// Grouped inset card (System Settings): rows separated by inset hairlines.
    fn group_card(rows: Vec<AnyElement>) -> gpui::Div {
        let mut card = div().mt_2().rounded(px(ui::R_LG)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider()).flex().flex_col().overflow_hidden();
        let n = rows.len();
        for (i, r) in rows.into_iter().enumerate() {
            card = card.child(r);
            if i + 1 < n { card = card.child(div().mx(px(12.)).h(px(1.)).bg(crate::theme::divider())); }
        }
        card
    }

    // ---------------------------------------------------------------- custom sections
    fn section_title(t: &str) -> impl IntoElement {
        let th = cur();
        div().pt_5().pb_1().px(px(12.)).text_size(px(ui::fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.subtext)).child(t.to_string())
    }

/// Tag definitions (PRD 5.23): built-in color tags (rename / delete) and custom tags
    /// (name + "#rrggbb" color with preset swatches; add / delete). Deleting removes the tag from
    /// every host and group.
    fn tags_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let mut out = div().flex().flex_col().gap(px(6.)).mt_2();
        for (id, name) in s.tags.all() {
            let builtin = ssh_term_proto::hosttree::COLORS.contains(&id.as_str());
            let used = s.hosts.iter().filter(|h| h.tags.contains(&id)).count() + s.groups.iter().filter(|g| g.tags.contains(&id)).count();
            let name_in = self.input(&format!("tagdef.name.{id}"), &name, if builtin { s.tags.names.get(&id).cloned().unwrap_or_default() } else { name.clone() }, window, cx);
            let mut row = div().flex().items_center().gap_2().px_3().py(px(6.)).rounded(px(ui::R_MD)).bg(crate::theme::sub_bg())
                .child(div().size(px(14.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(&id))))
                .child(div().w(px(180.)).child(ui::text_field(&name_in, false)));
            if !builtin {
                let cur_hex = s.tags.custom.iter().find(|d| d.id == id).map(|d| d.color.clone()).unwrap_or_default();
                let hex_in = self.input(&format!("tagdef.color.{id}"), "#rrggbb", cur_hex, window, cx);
                row = row.child(div().w(px(96.)).child(ui::text_field(&hex_in, false)));
                // preset swatches: click to set the color
                for (k, rgb) in [0xff3b30u32, 0xff9500, 0xffcc00, 0x34c759, 0x00c7be, 0x5ac8fa, 0x007aff, 0x5856d6, 0xaf52de, 0xff2d55, 0xa2845e, 0x8e8e93].into_iter().enumerate() {
                    let idc = id.clone();
                    row = row.child(div().id(SharedString::from(format!("sw-{id}-{k}"))).size(px(14.)).flex_shrink_0().rounded_full().cursor_pointer().bg(c(rgb)).hover(|d| d.opacity(0.75))
                        .on_click(cx.listener(move |this, _, _w, cx| {
                            let hex = format!("#{rgb:06x}");
                            this.notice = config::update(|s| if let Some(d) = s.tags.custom.iter_mut().find(|d| d.id == idc) { d.color = hex.clone(); });
                            this.inputs.remove(&format!("tagdef.color.{idc}"));
                            cx.emit(SettingsEvent::Changed("tags".into())); cx.notify();
                        })));
                }
            }
            let idd = id.clone();
            row = row.child(div().flex_1()).child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(tf("tag.used_n", &[("n", &used)])))
                .when(id != s.tags.production, |r| r.child(ui::button(SharedString::from(format!("tagdel-{id}")), t("sb.delete"), 2).on_click(cx.listener(move |this, _, _w, cx| {
                    let idd = idd.clone();
                    this.notice = config::update(|s| {
                        for h in s.hosts.iter_mut() { h.tags.retain(|x| *x != idd); }
                        for g in s.groups.iter_mut() { g.tags.retain(|x| *x != idd); }
                        if ssh_term_proto::hosttree::COLORS.contains(&idd.as_str()) { if !s.tags.hidden.contains(&idd) { s.tags.hidden.push(idd.clone()); } }
                        else { s.tags.custom.retain(|d| d.id != idd); }
                    });
                    cx.emit(SettingsEvent::Changed("tags".into())); cx.notify();
                }))));
            out = out.child(row);
        }
        let hidden = !s.tags.hidden.is_empty();
        out.child(div().flex().gap_2().mt_1()
            .child(ui::button("tag-add", t("tag.add"), 1).on_click(cx.listener(|this, _, _w, cx| {
                this.notice = config::update(|s| {
                    let n = (1..).find(|n| !s.tags.custom.iter().any(|d| d.id == format!("tag{n}"))).unwrap_or(1);
                    s.tags.custom.push(config::TagDef { id: format!("tag{n}"), name: format!("{} {n}", t("tag.new_name")), color: "#5ac8fa".into() });
                });
                cx.emit(SettingsEvent::Changed("tags".into())); cx.notify();
            })))
            .when(hidden, |d| d.child(ui::button("tag-restore", t("tag.restore_builtin"), 0).on_click(cx.listener(|this, _, _w, cx| {
                this.notice = config::update(|s| s.tags.hidden.clear());
                cx.emit(SettingsEvent::Changed("tags".into())); cx.notify();
            })))))
            .into_any_element()
    }

    fn permissions_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let mut out = div().flex().flex_col();
        // mode cards
        let mut modes = div().flex().gap_2();
        for m in Mode::ALL {
            let active = s.permissions.mode == m;
            modes = modes.child(div().id(("mode-card", m as usize)).flex_1().p_3().rounded(px(ui::R_SUB)).cursor_pointer().border_1()
                .border_color(if active { c(th.accent) } else { crate::theme::divider() }).bg(if active { crate::theme::selected_bg() } else { crate::theme::sub_bg() })
                .on_click(cx.listener(move |this, _, _w, cx| this.set("permissions.mode", toml::Value::String(format!("{m:?}").to_lowercase()), cx)))
                .child(div().flex().items_center().gap_2().child(icon(IconName::Shield, ui::IC_SM, if active { c(th.accent) } else { c(th.overlay1) }))
                    .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(m.label())))
                .child(div().pt_1().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(m.desc())));
        }
        out = out.child(Self::section_title(t("ui.permission_mode"))).child(modes);

        // rule table
        out = out.child(Self::section_title(t("ui.rules_evaluated_top_to_bottom")));
        let header = ui::table_header().gap_2().px_3()
            .child(div().w(px(24.)).child("#")).child(div().w(px(130.)).child("ID")).child(div().w(px(70.)).child(t("ui.principal")))
            .child(div().w(px(60.)).child(t("ui.action"))).child(div().flex_1().child(t("ui.command_glob"))).child(div().w(px(176.)).child(t("ui.effect"))).child(div().w(px(56.)));
        let mut table = div().flex().flex_col().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider()).overflow_hidden().child(header);
        let tester_cmd = self.tester.read(cx).value().to_string();
        let hit = policy::evaluate(&s.permissions.rules, "ai", "exec", &tester_cmd).map(|(i, _)| i);
        for (i, rule) in s.permissions.rules.iter().enumerate() {
            let me = cx.entity().downgrade();
            let eff_opts = opts(&[("allow", t("ui.allow")), ("ask", t("ui.ask")), ("deny", t("ui.deny"))]);
            let cur_eff = format!("{:?}", rule.effect).to_lowercase();
            let seg_id: &'static str = Box::leak(format!("rule-eff-{i}").into_boxed_str());
            let eff_col = match rule.effect { Effect::Allow => c(th.success), Effect::Ask => c(th.warning), Effect::Deny => c(th.error) };
            table = table.child(div().flex().items_center().gap_2().px_3().py(px(6.)).border_t_1().border_color(crate::theme::divider()).text_size(px(ui::fs_body())).text_color(c(th.text))
                .when(hit == Some(i), |d| d.bg(c(th.accent).opacity(0.08)))
                .child(div().w(px(24.)).flex().items_center().child(div().size(px(6.)).rounded_full().bg(eff_col)).child(div().pl_1().text_color(c(th.overlay1)).child(format!("{}", i + 1))))
                .child(div().w(px(130.)).overflow_hidden().child(rule.id.clone()))
                .child(div().w(px(70.)).text_color(c(th.subtext)).child(rule.principal.clone()))
                .child(div().w(px(60.)).text_color(c(th.subtext)).child(rule.action.clone()))
                .child(div().flex_1().flex().flex_col().min_w_0()
                    .child(div().font_family(SharedString::from(s.font.family.clone())).text_size(px(ui::fs_mono())).child(rule.command.clone()))
                    .when(!rule.reason.is_empty(), |d| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(rule.reason.clone()))))
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
                    .child(div().id(("rule-up", i)).size(px(22.)).rounded(px(ui::R_MD)).flex().items_center().justify_center().cursor_pointer().text_color(c(th.overlay1)).hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |this, _, _w, cx| { if i > 0 { this.notice = config::update(|s| s.permissions.rules.swap(i, i - 1)); cx.emit(SettingsEvent::Changed("permissions.rules".into())); cx.notify(); } }))
                        .child("↑"))
                    .child(div().id(("rule-del", i)).size(px(22.)).rounded(px(ui::R_MD)).flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().error).opacity(0.15)))
                        .on_click(cx.listener(move |this, _, _w, cx| { this.notice = config::update(|s| { s.permissions.rules.remove(i); }); cx.emit(SettingsEvent::Changed("permissions.rules".into())); cx.notify(); }))
                        .child(icon(IconName::Trash, ui::IC_SM, c(th.error))))));
        }
        // add-rule row
        let me = cx.entity().downgrade();
        let new_eff = format!("{:?}", self.new_effect).to_lowercase();
        table = table.child(div().flex().items_center().gap_2().px_3().py(px(8.)).border_t_1().border_color(crate::theme::divider()).bg(crate::theme::sub_bg())
            .child(div().w(px(24.)).child(icon(IconName::Plus, ui::IC_SM, c(th.overlay1))))
            .child(div().w(px(130.)).child(ui::text_field(&self.new_rule[0], false)))
            .child(div().w(px(70.)).child(ui::text_field(&self.new_rule[1], false)))
            .child(div().w(px(60.)).text_size(px(ui::fs_small())).text_color(c(th.subtext)).child("exec"))
            .child(div().flex_1().child(ui::text_field(&self.new_rule[2], false)))
            .child(div().w(px(176.)).child(ui::segmented("new-eff", &opts(&[("allow", t("ui.allow")), ("ask", t("ui.ask")), ("deny", t("ui.deny"))]), &new_eff, move |v, _w, cx| {
                let _ = me.update(cx, |this, cx| { this.new_effect = match v.as_str() { "allow" => Effect::Allow, "deny" => Effect::Deny, _ => Effect::Ask }; cx.notify(); });
            })))
            .child(div().w(px(56.)).child(ui::button("rule-add", t("ui.add"), 1).on_click(cx.listener(|this, _, window, cx| this.add_rule(window, cx))))));
        out = out.child(table);
        out = out.child(div().pt_2().flex().items_center().gap_2().child(div().w(px(300.)).child(ui::text_field(&self.new_rule[3], false)))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(t("ui.glob_matches_anything_one_character"))));
        if let Some(n) = &self.notice { out = out.child(div().pt_1().text_size(px(ui::fs_body())).text_color(c(th.error)).child(n.clone())); }

        // tester
        let max_auto: Risk = s.permissions.max_auto_risk.parse().unwrap_or(Risk::R1);
        let (eff, why) = policy::decide(s.permissions.mode, max_auto, &s.permissions.rules, "ai", "exec", &tester_cmd, s.permissions.dangerous_confirm);
        let eff_col = match eff { Effect::Allow => c(th.success), Effect::Ask => c(th.warning), Effect::Deny => c(th.error) };
        out = out.child(Self::section_title(t("ui.rule_tester")))
            .child(div().p_3().rounded(px(ui::R_SUB)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider()).flex().flex_col().gap_2()
                .child(div().flex().items_center().gap_3().child(div().flex_1().child(ui::text_field(&self.tester, false)))
                    .child(div().flex().items_center().gap_2().child(ui::tag(policy::classify(&tester_cmd).label(), c(th.info))).child(ui::tag(eff.label(), eff_col))))
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(tf("settings.tester_result", &[("mode", &s.permissions.mode.label()), ("effect", &eff.label()), ("why", &why)]))));

        // risk levels
        out = out.child(Self::section_title(t("ui.risk_levels")));
        let mut risks = div().flex().flex_col().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider());
        for (i, r) in [Risk::R0, Risk::R1, Risk::R2, Risk::R3, Risk::R4, Risk::Hard].into_iter().enumerate() {
            let col = match r { Risk::R0 => c(th.success), Risk::R1 => c(th.info), Risk::R2 => c(th.accent), Risk::R3 => c(th.warning), _ => c(th.error) };
            risks = risks.child(div().flex().items_center().gap_3().px_3().py(px(7.)).when(i > 0, |d| d.border_t_1().border_color(crate::theme::divider()))
                .child(div().w(px(44.)).flex().child(ui::tag(r.label(), col)))
                .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(r.desc())));
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

    fn credentials_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().flex().flex_col().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider());
        let keys = scan_keys();
        if keys.is_empty() { list = list.child(div().p_3().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ui.no_private_keys_found_in"))); }
        for (i, (name, kind, fp)) in keys.iter().enumerate() {
            let copy_name = name.clone();
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(8.)).when(i > 0, |d| d.border_t_1().border_color(crate::theme::divider()))
                .child(icon(IconName::KeyRound, ui::IC_SM, c(th.warning)))
                .child(div().flex_1().min_w_0().flex().flex_col().child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(name.clone()))
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(fp.clone())))
                .child(ui::button(("key-copy", i), t("settings.copy_pubkey"), 0).on_click(cx.listener(move |this, _, _, cx| {
                    let stem = copy_name.rsplit(['/', '\\']).next().unwrap_or(copy_name.as_str()).to_string();
                    let path = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".ssh").join(format!("{stem}.pub"));
                    this.notice = Some(match std::fs::read_to_string(&path) {
                        Ok(text) => {
                            let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
                            if line.is_empty() { t("settings.pubkey_empty").into() }
                            else { cx.write_to_clipboard(gpui::ClipboardItem::new_string(line)); t("settings.pubkey_copied").into() }
                        }
                        Err(e) => tf("settings.key_failed", &[("e", &e)]),
                    });
                    cx.notify();
                })))
                .child(ui::tag(kind.clone(), c(th.info))));
        }
        let agent = std::env::var("SSH_AUTH_SOCK").ok().filter(|p| std::path::Path::new(p).exists());
        let ssh_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".ssh");
        let (d1, d2) = (ssh_dir.clone(), ssh_dir);
        div().flex().flex_col()
            .child(Self::section_title(t("ui.local_private_keys"))).child(list)
            .child(Self::section_title(t("ui.generate_new_key")))
            .child(div().flex().items_center().gap_2()
                .child(div().w(px(260.)).child(ui::text_field(&self.key_pass, false)))
                .child(ui::button("key-gen", t("ui.generate_ed25519"), 1).on_click(cx.listener(move |this, _, window, cx| {
                    let pass = this.key_pass.read(cx).value().to_string();
                    let path = ssh_term_proto::keygen::free_path(&d1, "shellax_ed25519");
                    let comment = format!("{}@shellax", std::env::var("USER").unwrap_or_default());
                    this.notice = Some(match ssh_term_proto::keygen::generate(&path, &comment, Some(&pass)) {
                        Ok((p, _, fp)) => { let _ = crate::audit_log().append("ssh.keygen", "user", &format!("{} {fp} encrypted={}", p.display(), !pass.is_empty())); tf("settings.key_generated", &[("path", &p.display()), ("fp", &fp)]) }
                        Err(e) => tf("settings.key_failed", &[("e", &e)]),
                    });
                    this.key_pass.update(cx, |i, cx| i.set_value("", window, cx));
                    cx.notify();
                }))))
            .child(Self::section_title(t("ui.import")))
            .child(div().flex().items_center().gap_2()
                .child(div().flex_1().child(ui::text_field(&self.key_import, false)))
                .child(ui::button("key-import", t("ui.import"), 0).on_click(cx.listener(move |this, _, window, cx| {
                    let src = ssh_term_proto::hosts::expand_tilde(this.key_import.read(cx).value().trim());
                    this.notice = Some(match ssh_term_proto::keygen::import(&src, &d2) {
                        Ok((p, fp)) => { let _ = crate::audit_log().append("ssh.keyimport", "user", &format!("{} -> {} {fp}", src.display(), p.display())); tf("settings.key_imported", &[("path", &p.display()), ("fp", &fp)]) }
                        Err(e) => tf("settings.key_failed", &[("e", &format!("{e:#}"))]),
                    });
                    this.key_import.update(cx, |i, cx| i.set_value("", window, cx));
                    cx.notify();
                }))))
            .children(self.notice.clone().map(|n| div().pt_2().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(n)))
            .child(Self::section_title("ssh-agent"))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(match agent {
                Some(p) => tf("settings.agent_sock", &[("p", &p)]),
                None => t("ui.no_running_ssh_agent_detected").into(),
            }))
            .into_any_element()
    }

    fn agents_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        use ssh_term_proto::hooks::{self, Agent};
        let th = cur();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
        let mut list = div().flex().flex_col().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider());
        for (i, (agent, name)) in [(Agent::Claude, "Claude Code"), (Agent::Codex, "Codex"), (Agent::Grok, "Grok Build")].into_iter().enumerate() {
            let ok = hooks::installed(agent, &home);
            let path = hooks::config_path(agent, &home);
            let h2 = home.clone();
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(8.)).when(i > 0, |d| d.border_t_1().border_color(crate::theme::divider()))
                .child(icon(IconName::Bot, ui::IC_SM, c(th.accent)))
                .child(div().flex_1().flex().flex_col().child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(name))
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(path.display().to_string())))
                .child(if ok { ui::tag(t("ui.hook_installed"), c(th.success)).into_any_element() } else { ui::tag(t("ui.hook_not_installed"), c(th.overlay1)).into_any_element() })
                .child(ui::button(("hook-install", i), if ok { t("ui.uninstall") } else { t("ui.install") }, if ok { 0 } else { 1 }).on_click(cx.listener(move |this, _, _w, cx| {
                    let cli = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(format!("shellax-cli{}", std::env::consts::EXE_SUFFIX)))).filter(|p| p.exists()).map(|p| p.display().to_string()).unwrap_or_else(|| "shellax-cli".into());
                    let timeout = config::get().agents.approval_timeout_secs + 15;
                    this.notice = Some(match hooks::install(agent, &h2, &cli, timeout, ok, false) {
                        Ok(r) => {
                            let _ = crate::audit_log().append(if ok { "hook.uninstall" } else { "hook.install" }, "user", &format!("{} {} backup={:?}", agent.id(), r.path.display(), r.backup));
                            tf(if ok { "settings.hook_removed" } else { "settings.hook_installed" }, &[("agent", &name), ("path", &r.path.display()), ("backup", &r.backup.map(|b| b.display().to_string()).unwrap_or_else(|| "-".into()))])
                        }
                        Err(e) => tf("settings.hook_failed", &[("agent", &name), ("e", &e)]),
                    });
                    cx.notify();
                }))));
        }
        let ep = crate::data_dir().join("policy-endpoint.json");
        div().flex().flex_col().child(Self::section_title(t("ui.agent_hooks"))).child(list)
            .children(self.notice.clone().map(|n| div().pt_2().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(n)))
            .child(div().pt_2().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(tf("settings.hooks_note", &[("ep", &ep.display())])))
            .into_any_element()
    }

    fn keys_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mut list = div().flex().flex_col().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider());
        let mut seen: HashMap<String, &'static str> = HashMap::new();
        for (i, a) in Action::ALL.iter().enumerate() {
            let info = a.info();
            let b = a.binding();
            let conflict = if b.is_empty() { None } else { seen.insert(actions::normalize(&b), info.label) };
            let key = format!("keys.{}", info.id);
            let initial = config::get().keys.get(info.id).cloned().unwrap_or_default();
            let e = self.input(&key, if info.key.is_empty() { t("ui.unbound") } else { info.key }, initial, window, cx);
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(6.)).when(i > 0, |d| d.border_t_1().border_color(crate::theme::divider()))
                .child(icon(info.icon, ui::IC_SM, c(th.overlay1)))
                .child(div().flex_1().flex().flex_col().child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(info.label))
                    .when_some(conflict, |d, other| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.error)).child(tf("settings.key_conflict", &[("other", &other)]))))
                    .when_some(self.errors.get(&key).cloned(), |d, e| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.error)).child(e))))
                .child(div().w(px(150.)).flex().justify_end().child(if b.is_empty() { div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child("—").into_any_element() } else { ui::kbd(&b) }))
                .child(div().w(px(170.)).child(ui::text_field(&e, false))));
        }
        div().flex().flex_col().child(Self::section_title(t("ui.app_shortcuts_ctrl_shift_c"))).child(list).into_any_element()
    }

    fn general_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        use ssh_term_proto::updater::Phase;
        use std::sync::atomic::Ordering;
        let th = cur();
        let g = config::get().general.clone();
        let card = || div().w_full().p_4().rounded(px(ui::R_LG)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider()).flex().flex_col().gap_2();
        let muted = |s: String| div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(s);
        let channel = if g.update_channel == "beta" { t("update.channel_beta") } else { t("update.channel_stable") };
        let checked = crate::update_ui::last_checked().map(crate::update_ui::when).map_or_else(|| t("update.never_checked").to_string(), |w| tf("update.last_checked", &[("t", &w)]));
        let source = if g.custom_update_url().is_some() { g.update_source() } else { "GitHub Releases".into() };
        // 版本卡片
        let version_card = card()
            .child(div().flex().items_center().gap_2()
                .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(tf("update.current", &[("v", &ssh_term_proto::update::CURRENT)])))
                .child(div().px_2().py(px(1.)).rounded_full().bg(c(th.surface0)).text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(channel))
                .child(div().flex_1())
                .child(ui::button("check-update", t("ui.check_now"), 0).on_click(cx.listener(|this, _, _w, cx| {
                    this.notice = Some(t("settings.update_checking").into());
                    this.update_found = None;
                    let g = config::get().general.clone();
                    cx.spawn(async move |this, cx| {
                        let r = cx.background_executor().spawn(async move { g.check_update() }).await;
                        crate::update_ui::mark_checked();
                        let _ = this.update(cx, |v, cx| {
                            match r {
                                Ok(Some(m)) => { v.notice = None; v.update_found = Some(m); }
                                Ok(None) => v.notice = Some(tf("settings.update_latest", &[("v", &ssh_term_proto::update::CURRENT)])),
                                Err(e) => v.notice = Some(tf("settings.update_failed", &[("e", &crate::update_ui::friendly(&format!("{e:#}")))])),
                            }
                            cx.notify();
                        });
                    }).detach();
                    cx.notify();
                }))))
            .child(muted(checked))
            .child(muted(tf("update.source", &[("s", &source)])))
            .children(self.notice.clone().map(|n| div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).whitespace_normal().child(n)));

        // 新版本卡片
        let phase = crate::update_ui::phase();
        let found = self.update_found.clone().or_else(|| crate::update_ui::offered()).or_else(|| match &phase {
            Phase::Downloading { .. } | Phase::Failed { .. } => crate::update_ui::last(), _ => None });
        let new_card = found.map(|m| {
            let notes_url = format!("https://github.com/{}/releases/tag/v{}", ssh_term_proto::update::REPO, m.version.trim_start_matches('v'));
            let browser_url = if m.url.is_empty() { notes_url.clone() } else { m.url.clone() };
            let mut body = card()
                .child(div().flex().items_center().gap_2()
                    .child(ui::icon(ui::IconName::Download, ui::IC_MD, c(th.accent)))
                    .child(div().px_2().py(px(1.)).rounded_full().bg(crate::theme::selected_bg()).text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.accent)).child(tf("update.new_badge", &[("v", &m.version)])))
                    .child(div().flex_1())
                    .child(div().id("upd-notes").text_size(px(ui::fs_body())).text_color(c(th.accent)).cursor_pointer().hover(|s| s.underline()).child(t("update.view_notes")).on_click(move |_, _w, cx| cx.open_url(&notes_url))));
            let ready = crate::update_ui::ready_for(&m.version);
            let mut action = div().flex().items_center().gap_3();
            match (&phase, ready) {
                (_, Some(rec)) => {
                    action = action.child(ui::button("dl-install", t("update.quit_and_update"), 1).on_click(cx.listener(move |_, _, _w, cx| cx.emit(SettingsEvent::ApplyUpdate(rec.clone())))));
                }
                (Phase::Downloading { version, progress }, _) if *version == m.version => {
                    let (done, total) = (progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
                    let frac = if total > 0 { done as f32 / total as f32 } else { 0. };
                    let pct = if total > 0 { format!("{}%", done * 100 / total) } else { "…".into() };
                    let size = if total > 0 { format!("{} / {}", crate::update_ui::human(done), crate::update_ui::human(total)) } else { crate::update_ui::human(done) };
                    let spd = crate::update_ui::speed(done).map(|b| format!(" · {}/s", crate::update_ui::human(b))).unwrap_or_default();
                    body = body.child(div().flex().items_center().gap_2()
                        .child(div().flex_1().h(px(6.)).rounded_full().bg(c(th.surface1)).child(div().h_full().rounded_full().bg(c(th.accent)).w(gpui::relative(frac.clamp(0., 1.)))))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(format!("{pct} · {size}{spd}"))));
                    action = action.child(ui::button("dl-cancel", t("ui.cancel"), 0).on_click(|_, _, _| crate::update_ui::cancel()));
                }
                (Phase::Failed { version, error }, _) if *version == m.version => {
                    let open = self.update_err_open;
                    body = body.child(div().flex().items_start().gap_2()
                        .child(ui::icon(ui::IconName::TriangleAlert, ui::IC_SM, c(th.error)))
                        .child(div().flex_1().min_w_0().text_size(px(ui::fs_body())).text_color(c(th.error)).whitespace_normal().child(crate::update_ui::friendly(error)))
                        .child(div().id("upd-err-toggle").text_size(px(ui::fs_small())).text_color(c(th.subtext)).cursor_pointer().hover(|s| s.underline())
                            .child(if open { t("update.hide_details") } else { t("update.details") })
                            .on_click(cx.listener(|this, _, _w, cx| { this.update_err_open = !this.update_err_open; cx.notify(); }))));
                    if open {
                        let full = error.clone();
                        body = body.child(div().id("upd-err-full").w_full().max_h(px(160.)).overflow_y_scroll().p_2().rounded(px(ui::R_MD)).bg(c(th.base)).border_1().border_color(crate::theme::divider())
                            .text_size(px(ui::fs_small())).font_family(ui::mono_family()).text_color(c(th.subtext)).whitespace_normal().child(full.clone()))
                            .child(div().flex().child(div().id("upd-err-copy").text_size(px(ui::fs_body())).text_color(c(th.accent)).cursor_pointer().hover(|s| s.underline()).child(t("update.copy_details"))
                                .on_click(move |_, _w, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(full.clone())))));
                    }
                    let mm = m.clone();
                    action = action.child(ui::button("dl-retry", t("update.retry"), 1).on_click(cx.listener(move |this, _, _w, cx| { this.update_err_open = false; crate::update_ui::start(mm.clone(), true); cx.notify(); })));
                }
                _ if ssh_term_proto::updater::downloadable(&m) => {
                    let mm = m.clone();
                    action = action.child(ui::button("dl-update", t("update.download"), 1).on_click(cx.listener(move |_, _, _w, cx| { crate::update_ui::start(mm.clone(), true); cx.notify(); })));
                }
                _ => {}
            }
            action = action.child(div().id("upd-browser").text_size(px(ui::fs_small())).text_color(c(th.subtext)).cursor_pointer().hover(|s| s.underline().text_color(c(th.accent)))
                .child(t("update.open_in_browser")).on_click(move |_, _w, cx| cx.open_url(&browser_url)));
            body.child(action)
        });

        // 已下载的安装包
        let history = match &phase {
            Phase::Ready(rec) => {
                let (r1, r2) = (rec.clone(), rec.file.clone());
                Some(div().flex().flex_col().gap_1()
                    .child(div().pt_2().text_size(px(ui::fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.subtext)).child(t("update.downloaded_title")))
                    .child(div().w_full().px_3().py_2().rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider()).flex().items_center().gap_3()
                        .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(format!("v{}", rec.version.trim_start_matches('v'))))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(crate::update_ui::human(rec.size)))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(crate::update_ui::when(rec.downloaded_at)))
                        .child(div().flex_1())
                        .child(ui::button("dl-reveal", t("update.show_in_folder"), 3).on_click(move |_, _, _| { let _ = ssh_term_proto::updater::reveal(&r2); }))
                        .child(ui::button("dl-install-h", t("update.install_now"), 0).on_click(cx.listener(move |_, _, _w, cx| cx.emit(SettingsEvent::ApplyUpdate(r1.clone())))))))
            }
            _ => None,
        };

        div().flex().flex_col().gap_3().child(Self::section_title(t("ui.updates")))
            .child(version_card)
            .children(new_card)
            .children(history)
            .into_any_element()
    }

    fn scheme_preview(&self) -> AnyElement {
        let sc = crate::theme::scheme();
        let mut dots = div().flex().items_center().gap(px(3.));
        for (i, col) in sc.ansi.iter().enumerate() {
            dots = dots.child(div().id(("scheme-dot", i)).size(px(12.)).rounded_full().bg(c(*col)));
        }
        div().mb_2().p(px(10.)).rounded(px(ui::R_MD)).bg(c(sc.bg)).flex().items_center().gap_3().overflow_hidden()
            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(sc.fg)).child(t("ui.scheme_preview")))
            .child(div().text_size(px(ui::fs_body())).text_color(c(sc.fg)).child(crate::theme::display_name(&sc.name)))
            .child(div().flex_1())
            .child(dots)
            .into_any_element()
    }

    fn appearance_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        div().flex().flex_col().child(Self::section_title(t("ui.theme_import")))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).pb_2().child(t("ui.theme_import_desc")))
            .child(div().flex().items_center().gap_2()
                .child(div().flex_1().child(ui::text_field(&self.theme_import, false)))
                .child(ui::button("theme-import", t("ui.import"), 0).on_click(cx.listener(move |this, _, window, cx| {
                    let src = ssh_term_proto::hosts::expand_tilde(this.theme_import.read(cx).value().trim());
                    this.notice = Some(match crate::theme::import_file(&src) {
                        Ok(crate::theme::Imported::Theme { dest, name, dark }) => {
                            let _ = crate::audit_log().append("theme.import", "user", &format!("{} -> {}", src.display(), dest.display()));
                            if !this.theme_names.iter().any(|n| n.0 == name) { this.theme_names.push((name.clone(), dark)); }
                            cx.emit(SettingsEvent::ThemesChanged);
                            tf("settings.theme_imported", &[("name", &name), ("path", &dest.display())])
                        }
                        Ok(crate::theme::Imported::Scheme { dest, name, dark }) => {
                            let _ = crate::audit_log().append("scheme.import", "user", &format!("{} -> {}", src.display(), dest.display()));
                            if let Some(n) = this.scheme_names.iter_mut().find(|n| n.0.eq_ignore_ascii_case(&name)) { n.1 = dark; }
                            else { this.scheme_names.push((name.clone(), dark)); }
                            cx.emit(SettingsEvent::ThemesChanged);
                            tf("settings.scheme_imported", &[("name", &name), ("path", &dest.display())])
                        }
                        Err(e) => tf("settings.theme_import_failed", &[("e", &format!("{e:#}"))]),
                    });
                    this.theme_import.update(cx, |i, cx| i.set_value("", window, cx));
                    cx.notify();
                }))))
            .children(self.notice.clone().map(|n| div().pt_2().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(n)))
            .into_any_element()
    }

    fn audit_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let path = config::get().audit_path();
        let n = ssh_term_proto::audit::read_all(&path).len();
        div().flex().flex_col().child(Self::section_title(t("ui.log")))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(tf("settings.audit_count", &[("path", &path.display()), ("n", &n)])))
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
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(self.notice.clone().unwrap_or_else(|| t("ui.clears_history_saved_by_this").into()))))
            .into_any_element()
    }

    /// Settings → Context menu: edit context_menu.toml in place (validated on save, hot-reloaded).
    fn ctxmenu_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use ssh_term_proto::ctxmenu;
        let th = cur();
        let path = ctxmenu::custom_path();
        let ed = match &self.cm_editor { Some(e) => e.clone(), None => {
            let text = std::fs::read_to_string(&path).unwrap_or_else(|_| ctxmenu::CUSTOM_EXAMPLE.to_string());
            let e = cx.new(|cx| gpui_component::input::EditorState::new(window, cx).line_number(true).default_value(text));
            self.cm_editor = Some(e.clone()); e
        } };
        let (_, errs) = ctxmenu::load_custom();
        let mut col = div().pt_3().flex().flex_col().gap_2().text_size(px(ui::fs_body())).text_color(c(th.text))
            .child(div().text_color(c(th.overlay1)).child(tf("cm.settings_file", &[("path", &path.display())])))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("cm.settings_help")))
            .child(div().h(px(360.)).border_1().border_color(crate::theme::divider()).rounded(px(ui::R_SUB)).overflow_hidden().child(gpui_component::input::Editor::new(&ed).h_full()))
            .child(div().flex().gap_2()
                .child(ui::button("cm-save", t("cm.settings_save"), 1).on_click(cx.listener(|v, _, _w, cx| v.ctxmenu_save(cx))))
                .child(ui::button("cm-example", t("cm.settings_example"), 0).on_click(cx.listener(|v, _, w, cx| {
                    if let Some(e) = v.cm_editor.clone() { e.update(cx, |s, cx| { let cur = s.value().to_string(); s.set_value(format!("{}{}{}", cur.trim_end(), if cur.trim().is_empty() { "" } else { "\n\n" }, ssh_term_proto::ctxmenu::CUSTOM_EXAMPLE), w, cx) }); }
                    cx.notify();
                })))
                .child(ui::button("cm-clear-tools", t("cm.settings_clear_tools"), 3).on_click(cx.listener(|v, _, _w, cx| { ssh_term_proto::ctxmenu::clear_tool_cache(); v.cm_note = Some((true, t("cm.settings_tools_cleared").into())); cx.notify(); }))));
        if let Some((ok, msg)) = &self.cm_note { col = col.child(div().text_color(if *ok { c(th.success) } else { c(th.error) }).child(msg.clone())); }
        for e in errs.iter() { col = col.child(div().text_color(c(th.error)).child(format!("• {e}"))); }
        col.into_any_element()
    }
    fn ctxmenu_save(&mut self, cx: &mut Context<Self>) {
        use ssh_term_proto::ctxmenu;
        let Some(ed) = self.cm_editor.clone() else { return };
        let text = ed.read(cx).value().to_string();
        let (items, errs) = ctxmenu::parse_custom(&text);
        if let Err(e) = toml::from_str::<toml::Table>(&text) { self.cm_note = Some((false, tf("cm.err_toml", &[("e", &e.message().trim())]))); cx.notify(); return; }
        let path = ctxmenu::custom_path();
        if let Some(d) = path.parent() { let _ = std::fs::create_dir_all(d); }
        match std::fs::write(&path, &text) {
            Ok(()) => {
                let _ = crate::audit_log().append("settings.change", "user", &format!("context_menu.toml items={} errors={}", items.len(), errs.len()));
                self.cm_note = Some((errs.is_empty(), tf("cm.settings_saved", &[("n", &items.len())])));
            }
            Err(e) => self.cm_note = Some((false, format!("{}: {e}", path.display()))),
        }
        cx.notify();
    }
    fn about_extra(&self) -> AnyElement {
        let th = cur();
        let notices = include_str!("../THIRD_PARTY_NOTICES.md");
        let mut list = div().flex().flex_col().gap_1().p_3().rounded(px(ui::R_SUB)).bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider());
        for line in notices.lines() {
            if line.starts_with("|---") || line.starts_with("| Our file") || line.trim().is_empty() { continue; }
            let cleaned = line.trim_end().replace("**", "").replace('`', "");
            let cleaned = if cleaned.starts_with('|') { cleaned.trim_matches('|').split(" | ").map(str::trim).collect::<Vec<_>>().join("  ·  ") } else { cleaned };
            let l = cleaned.as_str();
            let el = if let Some(h) = l.strip_prefix("### ") { div().pt_1().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(h.to_string()) }
                else if let Some(h) = l.strip_prefix("## ") { div().pt_2().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(h.to_string()) }
                else if let Some(h) = l.strip_prefix("# ") { div().text_size(px(ui::fs_body())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(h.to_string()) }
                else { div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(l.to_string()) };
            list = list.child(el);
        }
        div().flex().flex_col()
            .child(div().flex().items_center().gap_3().py_3()
                .child(div().size(px(40.)).rounded(px(ui::R_SUB)).bg(crate::theme::selected_bg()).flex().items_center().justify_center().child(icon(IconName::SquareTerminal, ui::IC_XL, c(th.accent))))
                .child(div().flex().flex_col().child(div().text_size(px(ui::fs_title())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child("shellax"))
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(tf("settings.version", &[("v", &env!("CARGO_PKG_VERSION"))])))))
            .child(Self::section_title(t("ui.quick_start")))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).flex().flex_col().gap_1()
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
        if n.ends_with(".pub") || n.starts_with("known_hosts") || n == "config" || n == "authorized_keys" { continue; }
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
        // The settings view is a tab page, so it fills the tab body instead of a fixed-size card.
        // `compact` keeps the category rail from squeezing the form in a narrow window.
        let compact = f32::from(window.viewport_size().width) < 980.0;
        // One opaque surface for the page and both columns, so the rounded corners match the body.
        let sheet = if th.appearance == crate::theme::Appearance::Dark { c(th.base) } else { c(th.mantle) };
        let query = self.search.read(cx).value().to_string().to_lowercase();
        let mut rows = self.rows();
        // light / dark theme pickers only matter when following the system appearance
        if !config::get().general.check_updates { rows.retain(|r| !matches!(r.ctl, Ctl::Toggle("general.auto_download"))); }
        if config::get().appearance.theme != "system" { rows.retain(|r| !matches!(r.ctl, Ctl::Choice("appearance.light_theme", _) | Ctl::Choice("appearance.dark_theme", _))); }

        // ---- left: search + categories
        let mut nav = div().id("settings-nav").w(px(if compact { 168. } else { 224. })).flex_shrink_0().h_full().overflow_y_scroll().flex().flex_col().gap(px(1.)).p(px(10.)).bg(sheet).border_r_1().border_color(crate::theme::divider())
            .child(div().pb_2().child(ui::search_field(&self.search, false)));
        for (i, cat) in Cat::ALL.iter().copied().enumerate() {
            let active = query.is_empty() && cat == self.cat;
            nav = nav.child(div().id(("cat", i)).h(px(30.)).px(px(8.)).rounded(px(ui::R_MD)).flex().items_center().gap(px(8.)).cursor_pointer()
                .when(active, |d| d.bg(crate::theme::selected_bg()).font_weight(FontWeight::MEDIUM))
                .when(!active, |d| d.hover(|d| d.bg(crate::theme::hover_bg())))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.cat = cat;
                    this.search.update(cx, |s, cx| s.set_value("", window, cx));
                    if cat == Cat::Proxy { this.start_proxy_detect(false, cx); }
                    cx.notify();
                }))
                .child(icon(cat.icon(), ui::IC_MD, if active { c(th.accent) } else { c(th.overlay1) }))
                .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(cat.name())));
        }

        // ---- right: form
        let mut body = div().flex().flex_col();
        if query.is_empty() {
            body = body.child(div().text_size(px(ui::fs_page())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(self.cat.name()))
                .child(div().pt_1().pb_2().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(self.cat.desc()));
            let cur_cat = self.cat;
            if cur_cat == Cat::Appearance { body = body.child(self.scheme_preview()); }
            if cur_cat == Cat::Proxy {
                body = body.child(self.proxy_extra(window, cx));
            } else {
            let els: Vec<AnyElement> = rows.iter().enumerate().filter(|(_, r)| r.cat == cur_cat).map(|(i, row)| self.row_el(row, i, window, cx)).collect();
            if !els.is_empty() { body = body.child(Self::group_card(els)); }
            let extra = match self.cat {
                Cat::Permissions => Some(self.permissions_extra(window, cx)),
                Cat::Tags => Some(self.tags_extra(window, cx)),
                Cat::Credentials => Some(self.credentials_extra(cx)),
                Cat::Agents => Some(self.agents_extra(cx)),
                Cat::Keys => Some(self.keys_extra(window, cx)),
                Cat::Audit => Some(self.audit_extra(cx)),
                Cat::Suggest => Some(self.suggest_extra(cx)),
                Cat::About => Some(self.about_extra()),
                Cat::ContextMenu => Some(self.ctxmenu_extra(window, cx)),
                Cat::Extensions => self.ext.clone().map(|e| e.into_any_element()),
                Cat::Appearance => Some(self.appearance_extra(cx)),
                Cat::General => Some(self.general_extra(cx)),
                Cat::Ai => Some(self.ai_extra(window, cx)),
                _ => None,
            };
            if let Some(e) = extra { body = body.child(e); }
            }
        } else {
            let hits: Vec<(usize, &Row)> = rows.iter().enumerate().filter(|(_, r)| {
                format!("{} {} {} {}", r.label, r.desc, r.cat.name(), match &r.ctl { Ctl::Toggle(p) | Ctl::Choice(p, _) | Ctl::Text(p, _) | Ctl::Font(p) => *p, _ => "" }).to_lowercase().contains(&query)
            }).collect();
            body = body.child(div().text_size(px(ui::fs_page())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(tf("settings.search_results", &[("n", &hits.len())])));
            let mut last = None;
            let mut group: Vec<AnyElement> = vec![];
            for (i, row) in hits {
                if last != Some(row.cat) {
                    if !group.is_empty() { body = body.child(Self::group_card(std::mem::take(&mut group))); }
                    body = body.child(Self::section_title(row.cat.name())); last = Some(row.cat);
                }
                group.push(self.row_el(row, i, window, cx));
            }
            if !group.is_empty() { body = body.child(Self::group_card(group)); }
        }

        let header = ui::dialog_header(Some(IconName::Settings), c(cur().accent), t("ui.settings"), (!compact).then(|| t("ui.changes_are_saved_to_config").into()), None);
        // The tab strip carries the close affordance; this footer keeps a visible "Done" even when
        // many tabs push the settings tab out of view.
        let footer = ui::dialog_footer(None, vec![ui::button("settings-done", t("ui.done"), 1)
            .on_click(cx.listener(|_, _, _w, cx| cx.emit(SettingsEvent::Close))).into_any_element()]);
        div().size_full().min_w_0().flex().flex_col().bg(sheet)
            .child(header)
            .child(div().flex_1().min_h_0().flex().flex_row().bg(sheet)
                .child(nav)
                .child(div().id("settings-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().px(px(ui::SP_6)).py_4().bg(sheet).child(body.w_full().max_w(px(760.)))))
            .child(footer)
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
    fn start_proxy_detect(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.proxy_detecting || (self.proxy_started && !force) { return; }
        self.proxy_started = true;
        self.proxy_detecting = true;
        let (tx, rx) = flume::bounded(1);
        std::thread::spawn(move || { let _ = tx.send(ssh_term_proto::proxy::detect()); });
        cx.spawn(async move |this, cx| {
            let found = rx.recv_async().await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.proxy_detecting = false;
                this.proxy_candidates = found.candidates;
                this.proxy_bypass_suggest = found.bypass;
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    fn apply_proxy_candidate(&mut self, c: &ssh_term_proto::proxy::Candidate, window: &mut Window, cx: &mut Context<Self>) {
        self.proxy_http = matches!(c.kind, ssh_term_proto::proxy::Kind::Http);
        self.proxy_auth = c.user.is_some();
        let host = c.host.clone();
        let port = c.port.to_string();
        let (user, pass) = c.user.clone().unwrap_or_default();
        self.proxy_host.update(cx, |s, cx| s.set_value(host, window, cx));
        self.proxy_port.update(cx, |s, cx| s.set_value(port, window, cx));
        self.proxy_user.update(cx, |s, cx| s.set_value(user, window, cx));
        self.proxy_pass.update(cx, |s, cx| s.set_value(pass, window, cx));
        self.save_proxy_form(cx);
    }

    fn save_proxy_form(&mut self, cx: &mut Context<Self>) {
        let host = self.proxy_host.read(cx).value().trim().to_string();
        let port_s = self.proxy_port.read(cx).value().trim().to_string();
        if host.is_empty() { self.proxy_note = Some((false, t("proxy.bad_host").into())); cx.notify(); return; }
        let port: u16 = match port_s.parse::<u16>() { Ok(p) if p > 0 => p, _ => { self.proxy_note = Some((false, t("proxy.bad_port").into())); cx.notify(); return; } };
        let user = if self.proxy_auth {
            let u = self.proxy_user.read(cx).value().trim().to_string();
            let pw = self.proxy_pass.read(cx).value().to_string();
            (!u.is_empty()).then(|| (u, pw))
        } else { None };
        let kind = if self.proxy_http { ssh_term_proto::proxy::Kind::Http } else { ssh_term_proto::proxy::Kind::Socks5 };
        let url = ssh_term_proto::proxy::format_url(&ssh_term_proto::proxy::Proxy { kind, host: host.clone(), port, user });
        let Ok(back) = ssh_term_proto::proxy::parse(&url) else { self.proxy_note = Some((false, t("proxy.bad_url").into())); cx.notify(); return; };
        if back.host != host || back.port != port { self.proxy_note = Some((false, t("proxy.bad_url").into())); cx.notify(); return; }
        let err = config::update(|s| s.ssh.proxy = url);
        self.proxy_note = Some(match err { Some(e) => (false, e), None => (true, t("proxy.saved").into()) });
        cx.emit(SettingsEvent::Changed("ssh.proxy".into()));
        cx.notify();
    }

    fn clear_proxy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.proxy_http = false;
        self.proxy_auth = false;
        self.proxy_host.update(cx, |s, cx| s.set_value("127.0.0.1", window, cx));
        self.proxy_port.update(cx, |s, cx| s.set_value("", window, cx));
        self.proxy_user.update(cx, |s, cx| s.set_value("", window, cx));
        self.proxy_pass.update(cx, |s, cx| s.set_value("", window, cx));
        let err = config::update(|s| s.ssh.proxy.clear());
        self.proxy_note = Some(match err { Some(e) => (false, e), None => (true, t("proxy.cleared").into()) });
        cx.emit(SettingsEvent::Changed("ssh.proxy".into()));
        cx.notify();
    }

    fn proxy_extra(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use ssh_term_proto::proxy::{candidate_label, Candidate, DetectSource};
        let th = cur();
        let card = |body: gpui::Div| body.rounded(px(ui::R_LG)).p(px(12.)).flex().flex_col().gap(px(8.))
            .bg(crate::theme::sub_bg()).border_1().border_color(crate::theme::divider());
        let lab = |text: &str| div().w(px(72.)).flex_shrink_0().text_size(px(ui::fs_body())).text_color(c(cur().subtext)).child(text.to_string());
        let saved = config::get().ssh.proxy.clone();
        let current = match ssh_term_proto::proxy::parse(saved.trim()) {
            Ok(p) => tf("proxy.current", &[("url", &candidate_label(&Candidate { kind: p.kind, host: p.host, port: p.port, user: p.user, source: DetectSource::System }))]),
            Err(_) => t("proxy.current_none").into(),
        };
        let para = |s: &str| div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).whitespace_normal().child(s.to_string());
        let guide = card(div())
            .child(div().flex().items_center().gap_2()
                .child(icon(IconName::Info, ui::IC_SM, c(th.accent)))
                .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("proxy.howto_title"))))
            .child(para(t("proxy.howto_1")))
            .child(para(t("proxy.howto_2")))
            .child(para(t("proxy.howto_3")));
        let mut detect = card(div()).child(div().flex().items_center().gap_2()
            .child(div().flex_1().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("proxy.detected")))
            .child(ui::button("proxy-redetect", t("proxy.redetect"), 0).on_click(cx.listener(|this, _, _, cx| this.start_proxy_detect(true, cx)))));
        if self.proxy_detecting {
            detect = detect.child(div().flex().items_center().gap_2().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(icon(IconName::LoaderCircle, ui::IC_SM, c(th.accent))).child(t("proxy.detecting")));
        } else if self.proxy_candidates.is_empty() {
            detect = detect.child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).whitespace_normal().child(t("proxy.none")));
        } else {
            for (i, cnd) in self.proxy_candidates.clone().into_iter().enumerate() {
                let src = match cnd.source { DetectSource::System => t("proxy.source_system"), DetectSource::Env => t("proxy.source_env"), DetectSource::Listen => t("proxy.source_listen") };
                let label = candidate_label(&cnd);
                let chosen = cnd.clone();
                detect = detect.child(div().flex().items_center().gap_2()
                    .child(ui::tag(src, c(th.info)))
                    .child(div().flex_1().min_w_0().text_size(px(ui::fs_body())).text_color(c(th.text)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(label))
                    .child(ui::button(SharedString::from(format!("proxy-use-{i}")), t("proxy.use"), 1).on_click(cx.listener(move |this, _, w, cx| this.apply_proxy_candidate(&chosen, w, cx)))));
            }
        }
        if !self.proxy_bypass_suggest.is_empty() && config::get().ssh.no_proxy.trim().is_empty() {
            let hint = self.proxy_bypass_suggest.clone();
            detect = detect.child(div().flex().items_center().gap_2()
                .child(div().flex_1().min_w_0().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).whitespace_normal().child(tf("proxy.bypass_suggest", &[("list", &hint)])))
                .child(ui::button("proxy-bypass", t("proxy.use_bypass"), 0).on_click(cx.listener(move |this, _, w, cx| {
                    let list = this.proxy_bypass_suggest.clone();
                    if let Some(e) = this.inputs.get("ssh.no_proxy").cloned() { e.update(cx, |s, cx| s.set_value(list, w, cx)); }
                    else { let _ = config::update(|s| s.ssh.no_proxy = list); cx.emit(SettingsEvent::Changed("ssh.no_proxy".into())); }
                    cx.notify();
                }))));
        }
        let kinds = vec![("socks5".to_string(), t("proxy.socks5").to_string()), ("http".to_string(), t("proxy.http").to_string())];
        let me = cx.entity().downgrade();
        let mut form = card(div())
            .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(current))
            .child(div().flex().items_center().gap_3().child(lab(t("proxy.kind"))).child(div().flex_1().child(ui::segmented("proxy-kind", &kinds, if self.proxy_http { "http" } else { "socks5" }, move |v, _, cx| { let _ = me.update(cx, |this, cx| { this.proxy_http = v == "http"; cx.notify(); }); }))))
            .child(div().flex().items_center().gap_3().child(lab(t("proxy.addr"))).child(div().flex_1().child(ui::text_field(&self.proxy_host, false))).child(lab(t("proxy.port"))).child(div().w(px(88.)).child(ui::text_field(&self.proxy_port, false))))
            .child(div().flex().items_center().gap_3().child(lab(t("proxy.auth"))).child(Switch::new("proxy-auth").checked(self.proxy_auth).on_click(cx.listener(|this, on: &bool, _, cx| { this.proxy_auth = *on; cx.notify(); }))));
        if self.proxy_auth {
            form = form.child(div().flex().items_center().gap_3().child(lab(t("proxy.user"))).child(div().flex_1().child(ui::text_field(&self.proxy_user, false))).child(div().flex_1().child(ui::text_field(&self.proxy_pass, false).mask_toggle())));
        }
        let no_proxy = self.input("ssh.no_proxy", "10.*, 192.168.*, localhost", config::get().ssh.no_proxy.clone(), window, cx);
        form = form.child(div().flex().items_start().gap_3().child(lab(t("proxy.no_proxy"))).child(div().flex_1().flex().flex_col().gap(px(2.))
            .child(ui::text_field(&no_proxy, false))
            .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).whitespace_normal().child(t("proxy.no_proxy_desc")))));
        form = form.child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).whitespace_normal().child(t("proxy.ai_note")));
        form = form.child(div().flex().items_center().gap_2()
            .child(ui::button("proxy-save", t("proxy.save"), 1).on_click(cx.listener(|this, _, _, cx| this.save_proxy_form(cx))))
            .child(ui::button("proxy-clear", t("proxy.clear"), 0).on_click(cx.listener(|this, _, w, cx| this.clear_proxy(w, cx)))));
        if let Some((ok, msg)) = &self.proxy_note {
            let col = if *ok { c(th.success) } else { c(th.error) };
            form = form.child(div().text_size(px(ui::fs_body())).text_color(col).whitespace_normal().child(msg.clone()));
        }
        div().flex().flex_col().gap_3().mt_1().child(guide).child(detect).child(form).into_any_element()
    }

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
        let mut out = div().flex().flex_col().gap_3();

        // Storage backend banner
        let probe = crate::secret_store();
        let backend = self.ai_keys.values().flatten().next().map(|(_, l)| location_label(l))
            .unwrap_or_else(|| tf("ai.loc_pending", &[("path", &probe.file_path().display())]));
        out = out.child(
            div().flex().items_center().gap_2().p_2().rounded(px(ui::R_MD)).bg(crate::theme::sub_bg()).text_size(px(ui::fs_small())).text_color(c(th.subtext))
                .child(icon(IconName::KeyRound, ui::IC_SM, c(th.accent))).child(tf("ai.key_storage", &[("where", &backend)]))
        );

        // Provider list header
        let list_header = div().flex().items_center().justify_between().pt_2()
            .child(Self::section_title(t("ai.providers")))
            .child(self.preset_menu("add", window, cx));
        out = out.child(list_header);

        // Provider list
        let mut list = div().flex().flex_col().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider()).bg(crate::theme::sub_bg()).overflow_hidden();
        if s.ai.providers.is_empty() {
            list = list.child(div().p_4().text_center().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.no_providers")));
        }
        for (i, p) in s.ai.providers.iter().enumerate() {
            let sel = self.ai_sel.as_deref() == Some(p.id.as_str());
            let is_def = default_id.as_deref() == Some(p.id.as_str());
            let key = self.ai_keys.get(&p.id).cloned().flatten();
            let pid = p.id.clone();
            let pid2 = p.id.clone();
            let pid3 = p.id.clone();
            list = list.child(div().id(("aip", i)).flex().items_center().gap_3().px_3().py(px(9.)).cursor_pointer()
                .when(i > 0, |d| d.border_t_1().border_color(crate::theme::divider()))
                .when(sel, |d| d.bg(crate::theme::selected_bg()))
                .hover(|d| d.bg(crate::theme::hover_bg()))
                .on_click(cx.listener(move |this, _, _w, cx| { this.ai_sel = Some(pid.clone()); this.ai_key_reset(cx); this.ai_dropdown = None; this.load_models(&pid, false, cx); cx.notify(); }))
                .child(icon(if p.kind == Kind::Anthropic { IconName::Bot } else { IconName::Sparkles }, ui::IC_MD, if sel { c(th.accent) } else { c(th.overlay1) }))
                .child(div().flex_1().min_w_0().flex().flex_col()
                    .child(div().flex().items_center().gap_2().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(if p.name.is_empty() { p.id.clone() } else { p.name.clone() })
                        .child(ui::tag(if p.kind == Kind::Anthropic { "Anthropic" } else { t("ai.kind_openai") }, c(th.overlay1)))
                        .when(is_def, |d| d.child(ui::tag(t("ai.default"), c(th.accent)))))
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(format!("{} · {}", p.model, p.base_url))))
                .child(div().text_size(px(ui::fs_small())).font_family(SharedString::from(s.font.family.clone()))
                    .text_color(if key.is_some() { c(th.success) } else { c(th.warning) })
                    .child(key.as_ref().map(|k| k.0.clone()).unwrap_or_else(|| t("ai.no_key").into())))
                .when(!is_def, |d| d.child(ui::button(SharedString::from(format!("aip-def-{i}")), t("ai.make_default"), 0)
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        this.notice = config::update(|s| s.ai.default_provider = pid2.clone());
                        cx.emit(SettingsEvent::Changed("ai.default_provider".into())); cx.notify();
                    }))))
                .child(div().id(("aip-del", i)).size(px(24.)).rounded(px(ui::R_MD)).flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().error).opacity(0.15)))
                    .on_click(cx.listener(move |this, _, _w, cx| {
                        let name = config::get().ai.providers.iter().find(|p| p.id == pid3).map(|p| p.secret_name());
                        if let Some(n) = name { crate::secret_store().delete(&n); }
                        models::ModelCache::new(&crate::data_dir()).remove(&pid3);
                        this.notice = config::update(|s| { s.ai.providers.retain(|p| p.id != pid3); if s.ai.default_provider == pid3 { s.ai.default_provider = s.ai.providers.first().map(|p| p.id.clone()).unwrap_or_default(); } });
                        if this.ai_sel.as_deref() == Some(pid3.as_str()) { this.ai_sel = config::get().ai.providers.first().map(|p| p.id.clone()); }
                        this.refresh_keys();
                        cx.emit(SettingsEvent::Changed("ai.providers".into())); cx.notify();
                    }))
                    .child(icon(IconName::Trash, ui::IC_SM, c(th.error)))));
        }
        out = out.child(list);

        // Editor for the selected provider
        let Some(p) = self.ai_sel.as_ref().and_then(|id| s.ai.providers.iter().find(|p| &p.id == id)).cloned() else { return out.into_any_element() };

        let edit_title = div().flex().items_center().justify_between().pt_4()
            .child(Self::section_title(t("ai.edit_provider")))
            .child(self.preset_menu("apply", window, cx));
        out = out.child(edit_title);

        let mut editor_card = div().flex().flex_col().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider()).bg(crate::theme::sub_bg()).overflow_hidden();

        // 1. Name
        let name_input = self.provider_input(&p.id, "name", "OpenAI", p.name.clone(), window, cx);
        editor_card = editor_card.child(
            div().px_4().py(px(10.)).flex().items_center().justify_between().gap_4().border_b_1().border_color(crate::theme::divider())
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
                    .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("ui.name")))
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.name_desc"))))
                .child(div().w(px(280.)).flex_shrink_0().child(ui::text_field(&name_input, false)))
        );

        // 2. Base URL
        let url_input = self.provider_input(&p.id, "base_url", "https://api.openai.com/v1", p.base_url.clone(), window, cx);
        editor_card = editor_card.child(
            div().px_4().py(px(10.)).flex().items_center().justify_between().gap_4().border_b_1().border_color(crate::theme::divider())
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
                    .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("ui.base_url")))
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.base_url_desc"))))
                .child(div().w(px(320.)).flex_shrink_0().child(ui::text_field(&url_input, false)))
        );

        // 3. API Key & Test Connection
        let key = self.ai_keys.get(&p.id).cloned().flatten();
        let (sid, sid2, sid3) = (p.id.clone(), p.id.clone(), p.id.clone());
        let testing = self.ai_testing.as_deref() == Some(p.id.as_str());
        let res = self.ai_test.get(&p.id).cloned();

        let mut key_right = div().flex().items_center().gap_2();
        key_right = key_right.child(div().w(px(200.)).child(ui::text_field(&self.ai_key, false)));
        key_right = key_right.child(ui::button("aip-key-save", t("ui.save"), 1).on_click(cx.listener(move |this, _, window, cx| {
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
        })));
        key_right = key_right.child(ui::button("aip-test", if testing { t("ai.testing") } else { t("ai.test") }, 0).on_click(cx.listener(move |this, _, _w, cx| {
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
        })));
        if key.is_some() {
            key_right = key_right.child(ui::button("aip-key-del", t("ui.delete"), 2).on_click(cx.listener(move |this, _, _w, cx| {
                let name = config::get().ai.providers.iter().find(|p| p.id == sid2).map(|p| p.secret_name());
                if let Some(n) = name { crate::secret_store().delete(&n); }
                this.ai_note = Some((true, t("ai.key_deleted").into()));
                this.refresh_keys(); cx.notify();
            })));
        }

        let key_desc = match &key {
            Some((m, l)) => tf("ai.key_saved", &[("mask", m), ("where", &location_label(l))]),
            None => t("ai.key_none").to_string(),
        };

        editor_card = editor_card.child(
            div().px_4().py(px(10.)).flex().flex_col().gap(px(6.)).border_b_1().border_color(crate::theme::divider())
                .child(
                    div().flex().items_center().justify_between().gap_4()
                        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
                            .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child("API Key"))
                            .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(key_desc)))
                        .child(key_right)
                )
                .when_some(res, |d, (ok, msg)| d.child(div().text_size(px(ui::fs_body())).text_color(if ok { c(th.success) } else { c(th.error) }).child(msg)))
                .when_some(self.ai_note.clone(), |d, (ok, n)| d.child(div().text_size(px(ui::fs_small())).text_color(if ok { c(th.subtext) } else { c(th.warning) }).child(n)))
        );

        // 4. Model picker
        editor_card = editor_card.child(
            div().px_4().py(px(8.)).border_b_1().border_color(crate::theme::divider())
                .child(self.model_picker(&p, "model", window, cx))
        );

        // 5. Collapsible Advanced Settings
        let adv_open = self.ai_advanced;
        let adv_header = div().id("ai-adv-toggle").px_4().py(px(10.)).cursor_pointer()
            .flex().items_center().justify_between()
            .hover(|d| d.bg(crate::theme::hover_bg()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.ai_advanced = !this.ai_advanced;
                cx.notify();
            }))
            .child(
                // Title and hint share one shrinking column, so a narrow pane ellipsizes the hint
                // instead of pushing 展开 / 收起 off the card.
                div().flex_1().min_w_0().flex().items_center().gap_2()
                    .child(icon(if adv_open { IconName::ChevronDown } else { IconName::ChevronRight }, ui::IC_SM, c(th.overlay1)))
                    .child(div().flex_shrink_0().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("ai.advanced_settings")))
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.advanced_settings_desc")))
            )
            .child(div().flex_shrink_0().text_size(px(ui::fs_body())).text_color(c(th.accent)).child(if adv_open { t("ai.collapse") } else { t("ai.expand") }));
        editor_card = editor_card.child(adv_header);

        if adv_open {
            let pid = p.id.clone();
            let me = cx.entity().downgrade();
            let kind_opts = opts(&[("openai", t("ai.kind_openai")), ("anthropic", "Anthropic")]);

            // Protocol kind
            editor_card = editor_card.child(
                div().px_4().py(px(10.)).flex().items_center().justify_between().gap_4().border_t_1().border_color(crate::theme::divider())
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
                        .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("ai.kind")))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.kind.d"))))
                    // Right-hand column like every settings row: the control keeps its own width, so
                    // the segmented control's `w_full` cannot starve the label of its column.
                    .child(div().flex_shrink_0().flex().items_center().justify_end()
                        .child(ui::segmented("aip-kind", &kind_opts, p.kind.id(), move |v, _w, cx| {
                            let pid = pid.clone();
                            let _ = me.update(cx, |this, cx| this.update_provider(&pid, |p| {
                                let k = Kind::from_id(&v);
                                if k != p.kind && k == Kind::Anthropic && p.base_url.contains("openai.com") {
                                    p.base_url = "https://api.anthropic.com".into();
                                    p.model = "claude-sonnet-4-5".into();
                                }
                                p.kind = k;
                            }, cx));
                        })))
            );

            // Fast / Command Model
            editor_card = editor_card.child(
                div().px_4().py(px(8.)).border_t_1().border_color(crate::theme::divider())
                    .child(self.model_picker(&p, "fast_model", window, cx))
            );

            // Timeout
            let timeout_in = self.provider_input(&p.id, "timeout_secs", "30", p.timeout_secs.to_string(), window, cx);
            editor_card = editor_card.child(
                div().px_4().py(px(10.)).flex().items_center().justify_between().gap_4().border_t_1().border_color(crate::theme::divider())
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
                        .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("ai.timeout")))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.timeout.d"))))
                    .child(div().w(px(200.)).flex_shrink_0().child(ui::text_field(&timeout_in, false)))
            );

            // Temperature
            let temp_in = self.provider_input(&p.id, "temperature", "0.2", p.temperature.to_string(), window, cx);
            editor_card = editor_card.child(
                div().px_4().py(px(10.)).flex().items_center().justify_between().gap_4().border_t_1().border_color(crate::theme::divider())
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
                        .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("ai.temperature")))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.temperature.d"))))
                    .child(div().w(px(200.)).flex_shrink_0().child(ui::text_field(&temp_in, false)))
            );

            // Max Tokens
            let tokens_in = self.provider_input(&p.id, "max_tokens", "512", p.max_tokens.to_string(), window, cx);
            editor_card = editor_card.child(
                div().px_4().py(px(10.)).flex().items_center().justify_between().gap_4().border_t_1().border_color(crate::theme::divider())
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.))
                        .child(div().text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.text)).child(t("ai.max_tokens")))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(t("ai.max_tokens.d"))))
                    .child(div().w(px(200.)).flex_shrink_0().child(ui::text_field(&tokens_in, false)))
            );
        }

        out = out.child(editor_card);
        out.into_any_element()
    }

    fn ai_key_reset(&mut self, _cx: &mut Context<Self>) { self.ai_note = None; }

    fn preset_menu(&mut self, mode: &'static str, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let th = cur();
        let open = if mode == "add" { self.ai_add_dropdown } else { self.ai_template_dropdown };
        let cur_preset = self.ai_sel.as_ref().and_then(|id| config::get().ai.providers.iter().find(|p| &p.id == id).map(|p| p.preset.clone()));
        let cur_name = cur_preset.as_deref().and_then(|id| models::PRESETS.iter().find(|p| p.id == id)).map(|p| p.name).unwrap_or(t("models.preset_custom"));

        let btn = if mode == "add" {
            div().id("ai-add-btn").px_3().py(px(6.)).rounded(px(ui::R_MD)).cursor_pointer()
                .bg(c(th.accent)).text_color(gpui::white()).text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM)
                .flex().items_center().gap_1()
                .hover(|d| d.opacity(0.9))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    this.ai_menu_at = Some(ev.position());
                    this.ai_add_dropdown = !this.ai_add_dropdown;
                    this.ai_template_dropdown = false;
                    cx.notify();
                }))
                .child(icon(IconName::Plus, ui::IC_SM, gpui::white()))
                .child(t("ai.add_provider"))
                .child(icon(if open { IconName::ChevronUp } else { IconName::ChevronDown }, ui::IC_XS, gpui::white()))
        } else {
            div().id("ai-tpl-btn").px_3().py(px(5.)).rounded(px(ui::R_MD)).cursor_pointer()
                .bg(c(th.surface0)).border_1().border_color(crate::theme::divider())
                .text_color(c(th.text)).text_size(px(ui::fs_body())).font_weight(FontWeight::MEDIUM)
                .flex().items_center().gap_2()
                .hover(|d| d.bg(c(th.surface1)))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    this.ai_menu_at = Some(ev.position());
                    this.ai_template_dropdown = !this.ai_template_dropdown;
                    this.ai_add_dropdown = false;
                    cx.notify();
                }))
                .child(div().text_color(c(th.overlay1)).text_size(px(ui::fs_small())).child(t("models.template")))
                .child(div().child(cur_name))
                .child(icon(if open { IconName::ChevronUp } else { IconName::ChevronDown }, ui::IC_XS, c(th.overlay1)))
        };
        let anchor_slot = if mode == "add" { self.ai_add_anchor.clone() } else { self.ai_tpl_anchor.clone() };
        let record = anchor_slot.clone();
        let mut trigger = div().relative().child(
            div().on_children_prepainted(move |bounds, _, _| {
                if let Some(b) = bounds.first().copied() { record.set(Some(b)); }
            }).child(btn)
        );

        if open {
            let trigger_bounds = anchor_slot.get().unwrap_or_else(|| {
                let p = self.ai_menu_at.unwrap_or_else(|| gpui::point(px(80.), px(80.)));
                gpui::Bounds::new(p, gpui::size(px(1.), px(28.)))
            });
            let (anchor, pos, max_h) = place_preset_menu(trigger_bounds, window.viewport_size());
            let mut popover = ui::popover().id(SharedString::from(format!("pop-preset-{mode}")))
                .occlude().w(px(PRESET_MENU_W)).max_h(px(max_h)).overflow_y_scroll()
                .p(px(6.)).flex().flex_col().gap(px(2.))
                .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    if mode == "add" { this.ai_add_dropdown = false; } else { this.ai_template_dropdown = false; }
                    cx.notify();
                }));

            for (i, pr) in models::PRESETS.iter().enumerate() {
                let active = mode == "apply" && cur_preset.as_deref() == Some(pr.id);
                let label = if pr.id == "custom" { t("models.preset_custom") } else { pr.name };
                let is_anthropic = pr.kind == Kind::Anthropic;
                popover = popover.child(
                    div().id(SharedString::from(format!("pr-opt-{mode}-{i}")))
                        .flex_shrink_0()
                        .px_2().py(px(6.)).rounded(px(ui::R_MD)).cursor_pointer()
                        .flex().items_center().justify_between()
                        .when(active, |d| d.bg(crate::theme::selected_bg()))
                        .hover(|d| d.bg(crate::theme::hover_bg()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let pr = &models::PRESETS[i];
                            if mode == "add" {
                                this.ai_add_dropdown = false;
                                let id = format!("p{}", chrono::Local::now().format("%m%d%H%M%S%3f"));
                                let p = Provider {
                                    id: id.clone(),
                                    name: if pr.id == "custom" { t("models.preset_custom").to_string() } else { pr.name.to_string() },
                                    kind: pr.kind,
                                    base_url: pr.base_url.into(),
                                    model: pr.model.into(),
                                    fast_model: pr.fast_model.into(),
                                    preset: pr.id.into(),
                                    ..Default::default()
                                };
                                this.notice = config::update(|s| {
                                    if s.ai.default_provider.is_empty() { s.ai.default_provider = id.clone(); }
                                    s.ai.providers.push(p);
                                });
                                this.ai_sel = Some(id.clone());
                                this.refresh_keys();
                                this.ai_key_reset(cx);
                                if !pr.needs_key { this.load_models(&id, true, cx); }
                            } else if let Some(id) = this.ai_sel.clone() {
                                this.ai_template_dropdown = false;
                                this.update_provider(&id, |p| {
                                    p.preset = pr.id.into();
                                    p.kind = pr.kind;
                                    if !pr.base_url.is_empty() { p.base_url = pr.base_url.into(); }
                                    if !pr.model.is_empty() { p.model = pr.model.into(); }
                                    p.fast_model = pr.fast_model.into();
                                }, cx);
                                this.inputs.retain(|k, _| !k.starts_with(&format!("aip.{id}.")));
                                let _ = window;
                                this.load_models(&id, true, cx);
                            }
                            cx.emit(SettingsEvent::Changed("ai.providers".into()));
                            cx.notify();
                        }))
                        .child(
                            div().flex().items_center().gap_2().min_w_0()
                                .child(icon(if is_anthropic { IconName::Bot } else { IconName::Sparkles }, ui::IC_SM, if active { c(th.accent) } else { c(th.overlay1) }))
                                .child(div().min_w_0().text_size(px(ui::fs_body())).whitespace_nowrap().overflow_hidden().text_ellipsis()
                                    .text_color(if active { c(th.accent) } else { c(th.text) }).child(label))
                        )
                        .when(active, |d| d.child(icon(IconName::Check, ui::IC_XS, c(th.accent))))
                );
            }
            trigger = trigger.child(
                gpui::deferred(
                    gpui::anchored().anchor(anchor).position(pos).snap_to_window_with_margin(px(8.)).child(popover)
                ).with_priority(3)
            );
        }

        trigger
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
        let mut col = div().py(px(8.)).flex().flex_col().gap_1().border_b_1().border_color(crate::theme::divider())
            .child(div().flex().items_center().gap_3()
                .child(div().flex_1().min_w_0().flex().flex_col().text_size(px(ui::fs_body())).text_color(c(th.text)).child(label)
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(desc)))
                .child(div().w(px(240.)).flex_shrink_0().child(ui::text_field(&e, false)))
                .child(ui::button(SharedString::from(format!("mdl-open-{field}")), if open { t("models.close_list") } else { t("models.pick") }, 0)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ai_dropdown = if this.ai_dropdown == Some(field) { None } else { Some(field) };
                        this.ai_model_search.update(cx, |s, cx| { s.set_value("", window, cx); s.focus(window, cx); });
                        cx.notify();
                    })))
                .when(field == "model", |d| d.child(ui::button("mdl-refresh", if st.loading { t("models.loading_short") } else { t("models.refresh") }, 1)
                    .on_click(cx.listener(move |this, _, _w, cx| this.load_models(&pid, true, cx))))))
            .when(field == "model", |d| d.child(div().flex().items_center().gap_2().text_size(px(ui::fs_body()))
                .child(div().flex_1().min_w_0().text_color(status.0).child(status.1))
                .child(div().text_color(c(th.overlay1)).child(t("models.chat_only")))
                .child(Switch::new("mdl-chat-only").small().checked(chat_only).on_click(cx.listener(|this, v: &bool, _w, cx| this.set("ai.chat_models_only", toml::Value::Boolean(*v), cx))))));
        if open {
            let q = self.ai_model_search.read(cx).value().to_string();
            let hits = models::search(&shown, &q);
            let mut list = div().id(SharedString::from(format!("mdl-list-{field}"))).max_h(px(260.)).overflow_y_scroll().flex().flex_col();
            if hits.is_empty() {
                list = list.child(div().p_2().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(if st.list.is_empty() { t("models.empty_manual") } else { t("models.no_match") }));
            }
            for (i, m) in hits.iter().take(300).enumerate() {
                let id = m.id.clone();
                let pid = p.id.clone();
                let sel = m.id == val;
                let mut meta = vec![];
                if let Some(c) = m.context { meta.push(if c >= 1000 { format!("{}K ctx", c / 1000) } else { format!("{c} ctx") }); }
                if let (Some(a), Some(b)) = (m.price_in, m.price_out) { meta.push(format!("${a:.2} / ${b:.2} per 1M")); }
                if m.usage != models::Use::Chat { meta.push(format!("{:?}", m.usage).to_lowercase()); }
                list = list.child(div().id(("mdl", i)).flex_shrink_0().px_2().py(px(5.)).flex().items_center().gap_2().cursor_pointer().rounded(px(ui::R_MD))
                    .when(sel, |d| d.bg(crate::theme::selected_bg()))
                    .hover(|d| d.bg(crate::theme::hover_bg()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let v = id.clone();
                        this.update_provider(&pid, |p| if field == "model" { p.model = v.clone() } else { p.fast_model = v.clone() }, cx);
                        if let Some(inp) = this.inputs.get(&format!("aip.{pid}.{field}")).cloned() { inp.update(cx, |s, cx| s.set_value(id.clone(), window, cx)); }
                        this.ai_dropdown = None;
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w_0().flex().flex_col()
                        .child(div().text_size(px(ui::fs_body())).text_color(c(th.text)).child(m.id.clone()))
                        .when(!m.name.is_empty(), |d| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(m.name.clone()))))
                    .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(meta.join(" · "))));
            }
            col = col.child(div().mt_1().p_2().rounded(px(ui::R_SUB)).border_1().border_color(crate::theme::divider()).bg(crate::theme::sub_bg()).flex().flex_col().gap_1()
                .child(ui::search_field(&self.ai_model_search, false))
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(tf("models.list_hint", &[("n", &hits.len())])))
                .child(list));
        }
        col
    }
}

/// Width of the provider preset menu. Long enough for "通义千问 DashScope" and the custom label.
const PRESET_MENU_W: f32 = 280.;
/// Preferred height: all ten presets, without scrolling, when the window has the room.
const PRESET_MENU_WANT: f32 = 380.;

/// Anchor the preset menu to `trigger` so the whole list stays inside `viewport`.
/// Opens downward when that side can hold the list (or has more room); otherwise upward.
/// The returned height is the scrollport cap — shorter than the list, the menu scrolls.
fn place_preset_menu(trigger: gpui::Bounds<gpui::Pixels>, viewport: gpui::Size<gpui::Pixels>) -> (gpui::Anchor, gpui::Point<gpui::Pixels>, f32) {
    const MARGIN: f32 = 8.;
    const GAP: f32 = 4.;
    let vh = f32::from(viewport.height);
    let vw = f32::from(viewport.width);
    let top = f32::from(trigger.top());
    let bottom = f32::from(trigger.bottom());
    let right = f32::from(trigger.right());
    let below = (vh - bottom - GAP - MARGIN).max(0.);
    let above = (top - GAP - MARGIN).max(0.);
    let open_down = below + 0.5 >= PRESET_MENU_WANT || below >= above;
    let max_h = (if open_down { below } else { above }).min(PRESET_MENU_WANT).max(1.);
    let anchor = if open_down { gpui::Anchor::TopRight } else { gpui::Anchor::BottomRight };
    let mut x = right;
    if x > vw - MARGIN { x = (vw - MARGIN).max(MARGIN); }
    if x - PRESET_MENU_W < MARGIN { x = (MARGIN + PRESET_MENU_W).min((vw - MARGIN).max(MARGIN)); }
    let y = if open_down { bottom + GAP } else { top - GAP };
    (anchor, gpui::point(gpui::px(x), gpui::px(y)), max_h)
}

#[cfg(test)]
mod preset_menu_tests {
    use super::*;
    use gpui::{px, size, Bounds, Point};

    fn box_at(x: f32, y: f32, w: f32, h: f32) -> Bounds<gpui::Pixels> {
        Bounds::new(Point::new(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn opens_down_when_the_list_fits_below() {
        let (anchor, pos, max_h) = place_preset_menu(box_at(800., 100., 140., 32.), size(px(1200.), px(800.)));
        assert_eq!(anchor, gpui::Anchor::TopRight);
        assert!((max_h - PRESET_MENU_WANT).abs() < 0.1, "{max_h}");
        assert!((f32::from(pos.y) - 136.).abs() < 0.1, "{}", f32::from(pos.y));
        assert!((f32::from(pos.x) - 940.).abs() < 0.1, "{}", f32::from(pos.x));
    }

    #[test]
    fn flips_up_when_the_window_would_clip_the_bottom() {
        // Button sits 40px above the bottom of an 800px window.
        let (anchor, pos, max_h) = place_preset_menu(box_at(800., 728., 140., 32.), size(px(1200.), px(800.)));
        assert_eq!(anchor, gpui::Anchor::BottomRight);
        assert!((max_h - PRESET_MENU_WANT).abs() < 0.1, "{max_h}");
        assert!((f32::from(pos.y) - 724.).abs() < 0.1, "{}", f32::from(pos.y));
    }

    #[test]
    fn shrinks_and_scrolls_when_neither_side_holds_the_full_list() {
        // 400px window, button in the middle: above is the larger side and still under WANT.
        let (anchor, _, max_h) = place_preset_menu(box_at(200., 200., 120., 32.), size(px(600.), px(400.)));
        assert_eq!(anchor, gpui::Anchor::BottomRight);
        assert!((max_h - 188.).abs() < 0.1, "{max_h}");
    }

    #[test]
    fn keeps_the_menu_inside_the_left_edge() {
        let (_, pos, _) = place_preset_menu(box_at(10., 100., 40., 32.), size(px(600.), px(800.)));
        assert!((f32::from(pos.x) - (8. + PRESET_MENU_W)).abs() < 0.1, "{}", f32::from(pos.x));
    }
}
