//! App-level actions: shown in the command palette, bindable in Settings → 快捷键.
use crate::ui::IconName;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    CommandPalette, OpenSettings, NewLocalTab, NewHost, SplitAuto, SplitRight, SplitDown, ClosePane,
    NextTab, PrevTab, CycleTheme, ThemeSystem, CyclePermissionMode, ModeAsk, ModePlan, ModeAuto,
    OpenSftp, OpenPortForward, OpenAi, OpenAudit, OpenAbout, ClearHistory, ToggleSidebar,
    SettingsPermissions, SettingsAppearance, SettingsTerminal, SettingsSsh, SettingsKeys,
}

pub struct Info { pub id: &'static str, pub label: &'static str, pub group: &'static str, pub key: &'static str, pub icon: IconName }

impl Action {
    pub const ALL: &'static [Action] = &[
        Action::CommandPalette, Action::OpenSettings, Action::OpenAbout, Action::NewLocalTab, Action::SplitAuto, Action::SplitRight,
        Action::SplitDown, Action::ClosePane, Action::NextTab, Action::PrevTab, Action::NewHost, Action::CycleTheme, Action::ThemeSystem,
        Action::ToggleSidebar, Action::CyclePermissionMode, Action::ModeAsk, Action::ModePlan, Action::ModeAuto, Action::OpenSftp, Action::OpenPortForward,
        Action::OpenAi, Action::OpenAudit, Action::ClearHistory,
        Action::SettingsPermissions, Action::SettingsAppearance, Action::SettingsTerminal, Action::SettingsSsh, Action::SettingsKeys,
    ];
    pub fn info(self) -> Info {
        use IconName as I;
        let i = |id, label, group, key, icon| Info { id, label, group, key, icon };
        match self {
            Action::CommandPalette => i("palette", "命令面板", "通用", "ctrl-shift-p", I::Command),
            Action::OpenSettings => i("settings", "打开设置", "通用", "ctrl-,", I::Settings),
            Action::NewLocalTab => i("new_tab", "新建本地终端标签", "终端", "ctrl-shift-n", I::SquareTerminal),
            Action::NewHost => i("new_host", "新建主机…", "主机", "", I::Plus),
            Action::SplitAuto => i("split", "自动分屏（按长边）", "终端", "ctrl-shift-d", I::LayoutGrid),
            Action::SplitRight => i("split_right", "左右分屏", "终端", "", I::LayoutGrid),
            Action::SplitDown => i("split_down", "上下分屏", "终端", "", I::LayoutGrid),
            Action::ClosePane => i("close_pane", "关闭当前分屏", "终端", "ctrl-shift-w", I::X),
            Action::NextTab => i("next_tab", "下一个标签", "终端", "ctrl-tab", I::ChevronRight),
            Action::PrevTab => i("prev_tab", "上一个标签", "终端", "ctrl-shift-tab", I::ChevronRight),
            Action::CycleTheme => i("cycle_theme", "切换主题", "外观", "ctrl-shift-t", I::Palette),
            Action::ThemeSystem => i("theme_system", "主题跟随系统", "外观", "", I::Monitor),
            Action::CyclePermissionMode => i("cycle_mode", "切换权限模式 Ask → Plan → Auto", "权限", "ctrl-shift-m", I::Shield),
            Action::ModeAsk => i("mode_ask", "权限模式：Ask（逐项询问）", "权限", "", I::Shield),
            Action::ModePlan => i("mode_plan", "权限模式：Plan（只读）", "权限", "", I::Shield),
            Action::ModeAuto => i("mode_auto", "权限模式：Auto（低风险自动）", "权限", "", I::Shield),
            Action::OpenSftp => i("sftp", "SFTP 文件", "工具", "", I::FolderSync),
            Action::OpenPortForward => i("port_forward", "端口转发", "工具", "", I::ArrowLeftRight),
            Action::OpenAi => i("ai_panel", "AI / Agent 面板", "工具", "ctrl-shift-a", I::Bot),
            Action::OpenAudit => i("audit", "审计日志", "工具", "ctrl-shift-l", I::ScrollText),
            Action::OpenAbout => i("about", "帮助与关于（第三方许可）", "通用", "f1", I::CircleQuestionMark),
            Action::ClearHistory => i("clear_history", "清空命令建议历史", "命令建议", "", I::Trash),
            Action::ToggleSidebar => i("toggle_sidebar", "显示/隐藏侧栏", "外观", "ctrl-b", I::Layers),
            Action::SettingsPermissions => i("settings_permissions", "设置：权限与安全", "设置", "", I::ShieldCheck),
            Action::SettingsAppearance => i("settings_appearance", "设置：外观与字体", "设置", "", I::Palette),
            Action::SettingsTerminal => i("settings_terminal", "设置：终端", "设置", "", I::Terminal),
            Action::SettingsSsh => i("settings_ssh", "设置：SSH", "设置", "", I::Server),
            Action::SettingsKeys => i("settings_keys", "设置：快捷键", "设置", "", I::Keyboard),
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
    let has = |m: &str| parts.iter().any(|p| *p == m);
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
        assert!(fuzzy("spl", "自动分屏 split").is_some());
        assert!(fuzzy("xyz", "split").is_none());
        assert!(fuzzy("set", "settings").unwrap() > fuzzy("set", "sftp external transfer").unwrap_or(-100));
        assert!(fuzzy("权限", "设置：权限与安全").is_some());
        assert_eq!(normalize("Ctrl+Shift+P"), "ctrl-shift-p");
        assert_eq!(normalize("shift-ctrl-p"), "ctrl-shift-p");
        assert_eq!(normalize("ctrl-comma"), "ctrl-,");
        assert!(!valid_binding("c") && !valid_binding("shift-x") && valid_binding("alt-x"));
    }
}
