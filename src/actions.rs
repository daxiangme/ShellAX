//! App-level actions: shown in the command palette, bindable in Settings → 快捷键.
use ssh_term_proto::i18n::t;
use crate::ui::IconName;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    CommandPalette, OpenSettings, NewLocalTab, NewHost, SplitAuto, SplitRight, SplitDown, ClosePane,
    NextTab, PrevTab, CycleTheme, ThemeSystem, CyclePermissionMode, ModeAsk, ModePlan, ModeAuto,
    OpenSftp, OpenPortForward, OpenHostTools, OpenSnippets, OpenHistory, OpenAi, AiCommand, OpenAudit, OpenAbout, ClearHistory, ToggleSidebar,
    SettingsPermissions, SettingsAppearance, SettingsTerminal, SettingsSsh, SettingsKeys,
    LaunchAgent1, LaunchAgent2, LaunchAgent3,
    ToggleBroadcast, ToggleRecording, PlayRecording, SnapshotLayout, RollbackLayout, QuickStart,
    ToolHosts, ToolSftp, ToolDatabase, ToolRedis, ToolDocker, ToolPorts, ToggleBottomPanel, NewDbConnection,
    Quit,
}

pub struct Info { pub id: &'static str, pub label: &'static str, pub group: &'static str, pub key: &'static str, pub icon: IconName }

impl Action {
    pub const ALL: &'static [Action] = &[
        Action::CommandPalette, Action::OpenSettings, Action::OpenAbout, Action::NewLocalTab, Action::SplitAuto, Action::SplitRight,
        Action::SplitDown, Action::ClosePane, Action::NextTab, Action::PrevTab, Action::NewHost, Action::CycleTheme, Action::ThemeSystem,
        Action::ToggleSidebar, Action::CyclePermissionMode, Action::ModeAsk, Action::ModePlan, Action::ModeAuto, Action::OpenSftp, Action::OpenPortForward, Action::OpenHostTools, Action::OpenSnippets, Action::OpenHistory,
        Action::OpenAi, Action::AiCommand, Action::OpenAudit, Action::ClearHistory,
        Action::SettingsPermissions, Action::SettingsAppearance, Action::SettingsTerminal, Action::SettingsSsh, Action::SettingsKeys,
        Action::LaunchAgent1, Action::LaunchAgent2, Action::LaunchAgent3,
        Action::ToggleBroadcast, Action::ToggleRecording, Action::PlayRecording, Action::SnapshotLayout, Action::RollbackLayout, Action::QuickStart,
        Action::ToolHosts, Action::ToolSftp, Action::ToolDatabase, Action::ToolRedis, Action::ToolDocker, Action::ToolPorts, Action::ToggleBottomPanel, Action::NewDbConnection,
        Action::Quit,
    ];
    pub fn info(self) -> Info {
        use IconName as I;
        let i = |id, label, group, key, icon| Info { id, label, group, key, icon };
        match self {
            Action::CommandPalette => i("palette", t("ui.command_palette"), t("ui.general"), "ctrl-shift-p", I::Command),
            Action::OpenSettings => i("settings", t("ui.open_settings"), t("ui.general"), "ctrl-,", I::Settings),
            Action::NewLocalTab => i("new_tab", t("ui.new_local_terminal_tab"), t("ui.terminal"), "ctrl-shift-n", I::SquareTerminal),
            Action::NewHost => i("new_host", t("ui.new_host_2"), t("ui.host"), "", I::Plus),
            Action::SplitAuto => i("split", t("ui.auto_split_longer_side"), t("ui.terminal"), "ctrl-shift-d", I::LayoutGrid),
            Action::SplitRight => i("split_right", t("ui.split_left_right"), t("ui.terminal"), "", I::LayoutGrid),
            Action::SplitDown => i("split_down", t("ui.split_top_bottom"), t("ui.terminal"), "", I::LayoutGrid),
            Action::ClosePane => i("close_pane", t("ui.close_current_pane"), t("ui.terminal"), "ctrl-shift-w", I::X),
            Action::NextTab => i("next_tab", t("ui.next_tab"), t("ui.terminal"), "ctrl-tab", I::ChevronRight),
            Action::PrevTab => i("prev_tab", t("ui.previous_tab"), t("ui.terminal"), "ctrl-shift-tab", I::ChevronRight),
            Action::CycleTheme => i("cycle_theme", t("ui.toggle_theme"), t("ui.appearance"), "ctrl-shift-t", I::Palette),
            Action::ThemeSystem => i("theme_system", t("ui.theme_follows_system"), t("ui.appearance"), "", I::Monitor),
            Action::CyclePermissionMode => i("cycle_mode", t("ui.cycle_permission_mode_ask_plan"), t("ui.permissions"), "ctrl-shift-m", I::Shield),
            Action::ModeAsk => i("mode_ask", t("ui.permission_mode_ask_ask_each"), t("ui.permissions"), "", I::Shield),
            Action::ModePlan => i("mode_plan", t("ui.permission_mode_plan_read_only"), t("ui.permissions"), "", I::Shield),
            Action::ModeAuto => i("mode_auto", t("ui.permission_mode_auto_low_risk"), t("ui.permissions"), "", I::Shield),
            Action::OpenSftp => i("sftp", t("ui.sftp_files"), t("ui.tools"), "", I::FolderSync),
            Action::OpenHostTools => i("host_tools", t("hosttools.title"), t("ui.tools"), "", I::ChartPie),
            Action::OpenSnippets => i("snippets", t("lib.snippets"), t("ui.tools"), "alt-7", I::Code),
            Action::OpenHistory => i("history_panel", t("lib.history"), t("ui.tools"), "", I::Clock),
            Action::OpenPortForward => i("port_forward", t("ui.port_forwarding"), t("ui.tools"), "", I::ArrowLeftRight),
            Action::OpenAi => i("ai_panel", t("ui.ai_agent_panel"), t("ui.tools"), "ctrl-shift-a", I::Bot),
            Action::AiCommand => i("ai_command", t("ai.action_nl"), t("ui.tools"), "ctrl-shift-g", I::Sparkles),
            Action::OpenAudit => i("audit", t("ui.audit_log"), t("ui.tools"), "ctrl-shift-l", I::ScrollText),
            Action::OpenAbout => i("about", t("ui.help_about_third_party_licenses"), t("ui.general"), "f1", I::CircleQuestionMark),
            Action::ClearHistory => i("clear_history", t("ui.clear_suggestion_history"), t("ui.suggestions"), "", I::Trash),
            Action::ToggleSidebar => i("toggle_sidebar", t("ui.show_hide_sidebar"), t("ui.appearance"), "ctrl-b", I::Layers),
            Action::SettingsPermissions => i("settings_permissions", t("ui.settings_permissions_security"), t("ui.settings"), "", I::ShieldCheck),
            Action::SettingsAppearance => i("settings_appearance", t("ui.settings_appearance_fonts"), t("ui.settings"), "", I::Palette),
            Action::SettingsTerminal => i("settings_terminal", t("ui.settings_terminal"), t("ui.settings"), "", I::Terminal),
            Action::SettingsSsh => i("settings_ssh", t("ui.settings_ssh"), t("ui.settings"), "", I::Server),
            Action::SettingsKeys => i("settings_keys", t("ui.settings_keyboard_shortcuts"), t("ui.settings"), "", I::Keyboard),
            Action::LaunchAgent1 => i("launch_agent_1", t("ui.launch_agent_1"), t("ui.agents_group"), "ctrl-alt-1", I::Bot),
            Action::LaunchAgent2 => i("launch_agent_2", t("ui.launch_agent_2"), t("ui.agents_group"), "ctrl-alt-2", I::Bot),
            Action::LaunchAgent3 => i("launch_agent_3", t("ui.launch_agent_3"), t("ui.agents_group"), "ctrl-alt-3", I::Bot),
            Action::ToggleBroadcast => i("broadcast", t("bc.action"), t("ui.terminal"), "ctrl-shift-b", I::Radio),
            Action::ToggleRecording => i("record", t("rec.action"), t("ui.terminal"), "ctrl-shift-r", I::CircleDot),
            Action::PlayRecording => i("play_recording", t("rec.play_action"), t("ui.terminal"), "", I::Play),
            Action::SnapshotLayout => i("layout_snapshot", t("snap.action"), t("ui.terminal"), "", I::Camera),
            Action::RollbackLayout => i("layout_rollback", t("snap.rollback_action"), t("ui.terminal"), "", I::RotateCcw),
            Action::QuickStart => i("quick_start", t("qs.action"), t("ui.terminal"), "ctrl-shift-o", I::FolderOpen),
            Action::ToolHosts => i("tool_hosts", t("tool.hosts"), t("tool.group"), "alt-1", I::Server),
            Action::ToolSftp => i("tool_sftp", t("tool.sftp"), t("tool.group"), "alt-2", I::FolderSync),
            Action::ToolDatabase => i("tool_database", t("tool.database"), t("tool.group"), "alt-3", I::Database),
            Action::ToolRedis => i("tool_redis", t("tool.redis"), t("tool.group"), "alt-4", I::Layers),
            Action::ToolDocker => i("tool_docker", t("tool.docker"), t("tool.group"), "alt-5", I::Container),
            Action::ToolPorts => i("tool_ports", t("tool.ports"), t("tool.group"), "alt-6", I::ArrowLeftRight),
            Action::ToggleBottomPanel => i("toggle_bottom", t("tool.toggle_bottom"), t("tool.group"), "", I::PanelBottom),
            Action::NewDbConnection => i("new_db", t("db.new_connection"), t("tool.database"), "", I::DatabasePlus),
            // macOS 习惯是 ⌘Q；其他平台留空，避免占用 Ctrl+Q（终端里的流控键）。
            Action::Quit => i("quit", t("menu.quit"), t("ui.general"), if cfg!(target_os = "macos") { "cmd-q" } else { "" }, I::X),
        }
    }
    /// Effective binding (user override from settings.keys, else default; "" = unbound).
    pub fn binding(self) -> String {
        let info = self.info();
        crate::config::get().keys.get(info.id).cloned().unwrap_or_else(|| info.key.to_string())
    }
}

/// Normalize a GPUI keystroke to our binding syntax ("ctrl-shift-p").
pub fn keystroke_str(k: &gpui::Keystroke) -> String {
    let m = &k.modifiers;
    let mut s = String::new();
    if m.control { s.push_str("ctrl-"); }
    if m.alt { s.push_str("alt-"); }
    if m.platform { s.push_str("cmd-"); }
    if m.shift { s.push_str("shift-"); }
    s.push_str(&k.key.to_ascii_lowercase());
    s
}

pub fn normalize(binding: &str) -> String {
    let b = binding.trim().to_ascii_lowercase().replace('+', "-").replace(' ', "");
    let mut parts: Vec<&str> = b.split('-').filter(|p| !p.is_empty()).collect();
    if b.ends_with("--") { parts.push("-"); }
    let key = parts.pop().unwrap_or("");
    let has = |m: &str| parts.contains(&m);
    let mut s = String::new();
    if has("ctrl") || has("control") { s.push_str("ctrl-"); }
    if has("alt") || has("option") { s.push_str("alt-"); }
    if has("cmd") || has("super") || has("win") { s.push_str("cmd-"); }
    if has("shift") { s.push_str("shift-"); }
    s.push_str(match key { "comma" => ",", "esc" => "escape", k => k });
    s
}

/// A binding must use Ctrl/Alt/Cmd or be a function key, so it never steals plain typing.
pub fn valid_binding(b: &str) -> bool {
    let n = normalize(b);
    let key = n.rsplit('-').next().unwrap_or("");
    n.starts_with("ctrl-") || n.starts_with("alt-") || n.starts_with("cmd-") || n.contains("-ctrl-")
        || (key.len() >= 2 && key.starts_with('f') && key[1..].chars().all(|c| c.is_ascii_digit()))
}

pub fn find(ks: &str) -> Option<Action> {
    Action::ALL.iter().copied().find(|a| { let b = a.binding(); valid_binding(&b) && normalize(&b) == ks })
}

/// Fuzzy subsequence score (higher is better); None = no match. Case-insensitive,
/// rewards consecutive and word-start matches. Original code.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() { return Some(0); }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut qi, mut score, mut last) = (0usize, 0i32, None::<usize>);
    for (i, ch) in t.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            score += 10;
            if last == Some(i.wrapping_sub(1)) { score += 15; }
            if i == 0 || matches!(t[i - 1], ' ' | '-' | '_' | '：' | '/' | '（') { score += 8; }
            last = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then(|| score - t.len() as i32 / 4)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuzzy_and_bindings() {
        assert!(fuzzy("spl", t("ui.auto_split")).is_some());
        assert!(fuzzy("xyz", "split").is_none());
        assert!(fuzzy("set", "settings").unwrap() > fuzzy("set", "sftp external transfer").unwrap_or(-100));
        assert!(fuzzy(t("ui.permissions"), t("ui.settings_permissions_security")).is_some());
        assert_eq!(normalize("Ctrl+Shift+P"), "ctrl-shift-p");
        assert_eq!(normalize("shift-ctrl-p"), "ctrl-shift-p");
        assert_eq!(normalize("ctrl-comma"), "ctrl-,");
        assert!(!valid_binding("c") && !valid_binding("shift-x") && valid_binding("alt-x"));
    }
}
