//! Settings page: categories on the left (searchable), form on the right.
//! Every control reads/writes `config::get()` by dotted path and persists config.toml
//! immediately; the root view re-applies what can take effect live.
use std::collections::HashMap;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FontWeight, SharedString, Subscription, Window};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::switch::Switch;
use gpui_component::Sizable;
use ssh_term_proto::policy::{self, Effect, Mode, Risk, Rule};

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
            Cat::General => "通用", Cat::Appearance => "外观", Cat::Terminal => "终端", Cat::Suggest => "命令建议", Cat::Ssh => "SSH",
            Cat::Credentials => "凭据与密钥", Cat::Ai => "AI", Cat::Agents => "外部 Agent", Cat::Permissions => "权限与安全",
            Cat::Audit => "审计", Cat::Session => "会话与布局", Cat::Keys => "快捷键", Cat::Notify => "通知", Cat::About => "关于",
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
            Cat::General => "语言、启动行为与更新。",
            Cat::Appearance => "主题、字体与窗口外观。字体与主题修改立即生效。",
            Cat::Terminal => "终端行为。光标、复制粘贴、鼠标与响铃立即生效；回滚行数与 Shell 对新建会话生效。",
            Cat::Suggest => "基于历史的灰色命令建议（类似 fish）。",
            Cat::Ssh => "SSH 连接默认值与主机密钥策略，对新连接生效。",
            Cat::Credentials => "私钥、ssh-agent 与系统钥匙串。",
            Cat::Ai => "内置 AI（自带 Key，BYOK）。内置 AI 尚未实现，此处设置仅保存。",
            Cat::Agents => "识别在终端里运行的 Claude Code / Codex / Grok 并显示状态。",
            Cat::Permissions => "AI 与外部 Agent 执行操作时的权限模式与规则（最后匹配生效）。",
            Cat::Audit => "追加写入的 JSONL 审计日志，带 SHA-256 哈希链防篡改。",
            Cat::Session => "持久会话、布局恢复与关闭确认。",
            Cat::Keys => "应用级快捷键。输入如 ctrl-shift-p，留空恢复默认。",
            Cat::Notify => "桌面通知与提示音。",
            Cat::About => "版本信息与第三方许可证。",
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
    _subs: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(cat: Cat, theme_names: Vec<(String, bool)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索设置…"));
        let mk = |ph: &str, v: &str, window: &mut Window, cx: &mut Context<Self>| { let (ph, v) = (ph.to_string(), v.to_string()); cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v)) };
        let new_rule = [mk("规则 ID，如 allow-make", "", window, cx), mk("主体 * / ai / claude", "*", window, cx), mk("命令 glob，如 make *", "", window, cx), mk("原因（可选）", "", window, cx)];
        let tester = mk("输入命令测试，例如 git push --force", "git push --force origin main", window, cx);
        let mut subs = vec![cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() })];
        subs.push(cx.subscribe_in(&tester, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
        Self { cat, search, inputs: HashMap::new(), errors: HashMap::new(), theme_names, new_rule, new_effect: Effect::Allow, tester, notice: None, _subs: subs }
    }

    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.focus(window, cx));
    }

    fn rows(&self) -> Vec<Row> {
        let themes: Vec<(String, String)> = std::iter::once(("system".to_string(), "跟随系统".to_string()))
            .chain(self.theme_names.iter().map(|n| (n.0.clone(), n.0.clone()))).collect();
        let light: Vec<(String, String)> = self.theme_names.iter().filter(|n| !n.1).map(|n| (n.0.clone(), n.0.clone())).collect();
        let dark: Vec<(String, String)> = self.theme_names.iter().filter(|n| n.1).map(|n| (n.0.clone(), n.0.clone())).collect();
        let r = |cat, label, desc, ctl, soon| Row { cat, label, desc, ctl, soon };
        use Cat::*;
        vec![
            r(General, "界面语言", "目前界面只有简体中文", Ctl::Choice("general.language", opts(&[("zh-CN", "简体中文"), ("en", "English")])), true),
            r(General, "启动时", "启动应用后打开的内容", Ctl::Choice("general.startup", opts(&[("local", "本地终端"), ("restore", "恢复布局（即将支持）"), ("blank", "空白")])), false),
            r(General, "自动检查更新", "", Ctl::Toggle("general.check_updates"), true),
            r(General, "更新通道", "", Ctl::Choice("general.update_channel", opts(&[("stable", "稳定版"), ("beta", "测试版")])), true),
            r(General, "配置文件", "所有设置保存在这里，可直接编辑", Ctl::Info(config::config_path().display().to_string()), false),

            r(Appearance, "主题", "选择“跟随系统”时按系统深浅色切换", Ctl::Choice("appearance.theme", themes), false),
            r(Appearance, "浅色主题", "跟随系统时，浅色模式使用", Ctl::Choice("appearance.light_theme", light), false),
            r(Appearance, "深色主题", "跟随系统时，深色模式使用", Ctl::Choice("appearance.dark_theme", dark), false),
            r(Appearance, "终端字体", "主字体（等宽）", Ctl::Text("font.family", "JetBrains Mono"), false),
            r(Appearance, "回退字体链", "逗号分隔，依次用于中文、Nerd Font 图标、Emoji", Ctl::Text("font.fallbacks", "Noto Sans Mono CJK SC, …"), false),
            r(Appearance, "字号", "单位 pt，8–32", Ctl::Text("font.size", "13.5"), false),
            r(Appearance, "行高", "字号的倍数，1.0–2.0", Ctl::Text("font.line_height", "1.4"), false),
            r(Appearance, "连字", "开启后 -> != 等显示为连字", Ctl::Toggle("font.ligatures"), false),
            r(Appearance, "窗口透明度", "0.5–1.0", Ctl::Text("appearance.opacity", "1.0"), true),
            r(Appearance, "界面字号", "侧栏、标签等界面文字", Ctl::Text("appearance.ui_font_size", "13"), true),

            r(Terminal, "默认 Shell", "留空使用 $SHELL；对新建本地标签生效", Ctl::Text("terminal.shell", "/bin/zsh"), false),
            r(Terminal, "回滚行数", "对新建会话生效", Ctl::Text("terminal.scrollback", "10000"), false),
            r(Terminal, "光标样式", "", Ctl::Choice("terminal.cursor_style", opts(&[("block", "方块"), ("bar", "竖线"), ("underline", "下划线")])), false),
            r(Terminal, "光标闪烁", "空闲 10 秒后自动停止闪烁以省电", Ctl::Toggle("terminal.cursor_blink"), false),
            r(Terminal, "响铃", "收到 BEL 时", Ctl::Choice("terminal.bell", opts(&[("none", "关闭"), ("visual", "闪烁边框"), ("sound", "声音（即将支持）")])), false),
            r(Terminal, "选中即复制", "松开鼠标时复制到剪贴板（Linux 始终写入主选区）", Ctl::Toggle("terminal.copy_on_select"), false),
            r(Terminal, "多行粘贴确认", "程序未开启括号粘贴时，粘贴多行文本前确认", Ctl::Toggle("terminal.paste_confirm"), false),
            r(Terminal, "鼠标上报", "允许 vim / htop 等程序接收鼠标事件（按住 Shift 始终本地选择）", Ctl::Toggle("terminal.mouse_reporting"), false),
            r(Terminal, "Shell 集成（OSC 133）", "为 bash / zsh 注入提示符标记，用于命令建议与 Agent 识别；对新建会话生效", Ctl::Choice("terminal.shell_integration", opts(&[("auto", "自动注入"), ("off", "关闭")])), false),

            r(Suggest, "启用命令建议", "", Ctl::Toggle("suggest.enabled"), false),
            r(Suggest, "接受建议的按键", "", Ctl::Choice("suggest.accept", opts(&[("both", "→ 或 Tab"), ("right", "仅 →"), ("tab", "仅 Tab")])), false),
            r(Suggest, "本地历史", "记录在本应用中输入的命令（~/.local/share/agentshell/history，权限 0600）", Ctl::Toggle("suggest.local_history"), false),
            r(Suggest, "远程历史", "SSH 连接后读取远端 ~/.bash_history 与 ~/.zsh_history", Ctl::Toggle("suggest.remote_history"), false),
            r(Suggest, "敏感命令过滤规则", "正则，逗号分隔；与内置规则（password、token、-p 密码等）一起生效", Ctl::Text("suggest.filter_patterns", "(?i)vault +login"), false),

            r(Ssh, "默认用户名", "主机未指定用户时使用", Ctl::Text("ssh.default_user", "$USER"), false),
            r(Ssh, "默认端口", "", Ctl::Text("ssh.default_port", "22"), false),
            r(Ssh, "保活间隔（秒）", "0 表示关闭", Ctl::Text("ssh.keepalive_secs", "30"), false),
            r(Ssh, "连接超时（秒）", "TCP 连接阶段", Ctl::Text("ssh.connect_timeout_secs", "15"), false),
            r(Ssh, "known_hosts 文件", "", Ctl::Text("ssh.known_hosts", "~/.ssh/known_hosts"), false),
            r(Ssh, "未知主机密钥", "首次连接时的处理方式", Ctl::Choice("ssh.host_key_policy", opts(&[("ask", "询问"), ("strict", "严格拒绝"), ("accept-new", "自动接受新主机")])), false),
            r(Ssh, "读取 ~/.ssh/config", "把其中的 Host 列入侧栏", Ctl::Toggle("ssh.read_ssh_config"), false),
            r(Ssh, "ssh_config 路径", "", Ctl::Text("ssh.ssh_config", "~/.ssh/config"), false),
            r(Ssh, "默认跳板机", "ProxyJump，如 bastion", Ctl::Text("ssh.jump_host", "user@bastion:22"), true),
            r(Ssh, "加密算法偏好", "逗号分隔，按优先级", Ctl::Text("ssh.ciphers", ""), true),
            r(Ssh, "Agent 转发", "", Ctl::Toggle("ssh.agent_forwarding"), true),

            r(Credentials, "使用系统钥匙串", "保存密钥口令与 API Key（macOS Keychain / Secret Service / Windows 凭据）", Ctl::Toggle("credentials.use_keychain"), true),
            r(Credentials, "使用 ssh-agent", "", Ctl::Toggle("credentials.use_ssh_agent"), true),
            r(Credentials, "额外私钥", "逗号分隔的路径，追加到默认 ~/.ssh/id_* 之后", Ctl::Text("credentials.extra_keys", "~/.ssh/work_ed25519"), true),

            r(Ai, "启用内置 AI", "", Ctl::Toggle("ai.enabled"), true),
            r(Ai, "服务商", "自带 Key（BYOK）", Ctl::Choice("ai.provider", opts(&[("openai", "OpenAI"), ("anthropic", "Anthropic"), ("gemini", "Gemini"), ("ollama", "Ollama"), ("compatible", "兼容接口")])), true),
            r(Ai, "模型", "", Ctl::Text("ai.model", "gpt-4.1"), true),
            r(Ai, "接口地址", "兼容接口 / Ollama 使用", Ctl::Text("ai.base_url", "http://localhost:11434"), true),
            r(Ai, "API Key", "只保存在系统钥匙串，从不写入 config.toml", Ctl::Info("未设置（钥匙串集成即将支持）".into()), true),
            r(Ai, "发送前脱敏", "去除密码、Token、私钥等再发送", Ctl::Toggle("ai.redact"), true),
            r(Ai, "上下文范围", "", Ctl::Choice("ai.context", opts(&[("selection", "仅选中"), ("command", "最近命令"), ("screen", "当前屏幕")])), true),
            r(Ai, "允许发送命令输出", "关闭时只发送命令本身", Ctl::Toggle("ai.send_output"), true),

            r(Agents, "识别 Claude Code", "", Ctl::Toggle("agents.detect_claude"), false),
            r(Agents, "识别 Codex", "", Ctl::Toggle("agents.detect_codex"), false),
            r(Agents, "识别 Grok", "", Ctl::Toggle("agents.detect_grok"), false),
            r(Agents, "状态变化通知", "Agent 等待确认时发送桌面通知", Ctl::Toggle("agents.notify_state"), true),
            r(Agents, "审批超时（秒）", "Agent 请求权限后等待的时长", Ctl::Text("agents.approval_timeout_secs", "120"), true),
            r(Agents, "超时处理", "", Ctl::Choice("agents.on_timeout", opts(&[("deny", "拒绝"), ("ask", "保持等待")])), true),

            r(Permissions, "默认模式", "状态栏也可点击切换；仅保存与显示，内置 AI 未实现前不拦截任何操作", Ctl::Choice("permissions.mode", opts(&[("ask", "Ask 询问"), ("plan", "Plan 只读"), ("auto", "Auto 自动")])), false),
            r(Permissions, "Auto 模式最高自动风险", "不高于此等级的操作在 Auto 模式下自动放行", Ctl::Choice("permissions.max_auto_risk", opts(&[("R0", "R0"), ("R1", "R1"), ("R2", "R2")])), false),
            r(Permissions, "生产主机写操作确认", "标记为“生产”的主机上 R1 以上操作总是询问", Ctl::Toggle("permissions.prod_confirm"), true),
            r(Permissions, "高危命令二次确认", "R4 操作即使有允许规则也要确认", Ctl::Toggle("permissions.dangerous_confirm"), false),

            r(Audit, "日志目录", "", Ctl::Text("audit.dir", "~/.local/share/agentshell/audit"), false),
            r(Audit, "保留天数", "", Ctl::Text("audit.retention_days", "90"), true),
            r(Audit, "记录手动输入的命令", "默认只记录连接、权限与设置变更", Ctl::Toggle("audit.log_commands"), true),

            r(Session, "持久会话", "断线后用远端 tmux 恢复", Ctl::Choice("session.persistent", opts(&[("off", "关闭"), ("tmux", "tmux")])), true),
            r(Session, "启动时恢复布局", "", Ctl::Toggle("session.restore_layout"), true),
            r(Session, "关闭前确认", "有程序在运行时关闭标签需确认", Ctl::Toggle("session.confirm_close"), true),
            r(Session, "自动重连", "", Ctl::Toggle("session.auto_reconnect"), true),

            r(Notify, "桌面通知", "", Ctl::Toggle("notify.desktop"), true),
            r(Notify, "Agent 等待确认时通知", "", Ctl::Toggle("notify.agent_blocked"), true),
            r(Notify, "长命令完成通知（秒）", "命令运行超过该时长且窗口不在前台时", Ctl::Text("notify.long_command_secs", "10"), true),
            r(Notify, "提示音", "", Ctl::Toggle("notify.sound"), true),
        ]
    }

    fn set(&mut self, path: &str, v: toml::Value, cx: &mut Context<Self>) {
        let p = path.to_string();
        let mut err = None;
        let save_err = config::update(|s| { if let Err(e) = s.set(&p, v) { err = Some(format!("无效的值：{e}")); } });
        match err.or(save_err) {
            Some(e) => { self.errors.insert(p.clone(), e); }
            None => { self.errors.remove(&p); validate(&p, &mut self.errors); }
        }
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
                    this.errors.insert(k.clone(), "需包含 Ctrl / Alt / Cmd，或使用 F1–F12".into());
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
        out = out.child(Self::section_title("权限模式")).child(modes);

        // rule table
        out = out.child(Self::section_title("规则（自上而下评估，最后一条匹配的规则生效）"));
        let header = div().flex().items_center().gap_2().px_3().py(px(6.)).text_size(px(11.)).text_color(c(th.overlay1)).bg(c(th.mantle)).rounded_t_lg()
            .child(div().w(px(24.)).child("#")).child(div().w(px(130.)).child("ID")).child(div().w(px(70.)).child("主体"))
            .child(div().w(px(60.)).child("动作")).child(div().flex_1().child("命令 glob")).child(div().w(px(176.)).child("效果")).child(div().w(px(56.)));
        let mut table = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0)).child(header);
        let tester_cmd = self.tester.read(cx).value().to_string();
        let hit = policy::evaluate(&s.permissions.rules, "ai", "exec", &tester_cmd).map(|(i, _)| i);
        for (i, rule) in s.permissions.rules.iter().enumerate() {
            let me = cx.entity().downgrade();
            let eff_opts = opts(&[("allow", "允许"), ("ask", "询问"), ("deny", "拒绝")]);
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
            .child(div().w(px(176.)).child(ui::segmented("new-eff", &opts(&[("allow", "允许"), ("ask", "询问"), ("deny", "拒绝")]), &new_eff, move |v, _w, cx| {
                let _ = me.update(cx, |this, cx| { this.new_effect = match v.as_str() { "allow" => Effect::Allow, "deny" => Effect::Deny, _ => Effect::Ask }; cx.notify(); });
            })))
            .child(div().w(px(56.)).child(ui::button("rule-add", "添加", 1).on_click(cx.listener(|this, _, window, cx| this.add_rule(window, cx))))));
        out = out.child(table);
        out = out.child(div().pt_2().flex().items_center().gap_2().child(div().w(px(300.)).child(Input::new(&self.new_rule[3]).small()))
            .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child("glob：* 匹配任意字符，? 匹配单个字符。新规则添加到末尾（优先级最高）。")));
        if let Some(n) = &self.notice { out = out.child(div().pt_1().text_size(px(11.5)).text_color(c(th.error)).child(n.clone())); }

        // tester
        let max_auto: Risk = s.permissions.max_auto_risk.parse().unwrap_or(Risk::R1);
        let (eff, why) = policy::decide(s.permissions.mode, max_auto, &s.permissions.rules, "ai", "exec", &tester_cmd, s.permissions.dangerous_confirm);
        let eff_col = match eff { Effect::Allow => c(th.success), Effect::Ask => c(th.warning), Effect::Deny => c(th.error) };
        out = out.child(Self::section_title("规则测试器"))
            .child(div().p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface0)).flex().flex_col().gap_2()
                .child(div().flex().items_center().gap_3().child(div().flex_1().child(Input::new(&self.tester)))
                    .child(div().flex().items_center().gap_2().child(ui::tag(policy::classify(&tester_cmd).label(), c(th.info))).child(ui::tag(eff.label(), eff_col))))
                .child(div().text_size(px(11.5)).text_color(c(th.subtext)).child(format!("主体 ai · 动作 exec · 当前模式 {} → {}：{}", s.permissions.mode.label(), eff.label(), why))));

        // risk levels
        out = out.child(Self::section_title("风险等级"));
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
        if vals[2].is_empty() { self.notice = Some("请填写命令 glob".into()); cx.notify(); return; }
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
        if keys.is_empty() { list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child("~/.ssh 下没有找到私钥（id_* 及对应 .pub）")); }
        for (i, (name, kind, fp)) in keys.iter().enumerate() {
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(8.)).when(i > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                .child(icon(IconName::KeyRound, 14., c(th.warning)))
                .child(div().flex_1().flex().flex_col().child(div().text_size(px(12.5)).text_color(c(th.text)).child(name.clone()))
                    .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(fp.clone())))
                .child(ui::tag(kind.clone(), c(th.info))));
        }
        let agent = std::env::var("SSH_AUTH_SOCK").ok().filter(|p| std::path::Path::new(p).exists());
        div().flex().flex_col()
            .child(Self::section_title("本机私钥（只读扫描 ~/.ssh）")).child(list)
            .child(div().pt_2().flex().gap_2()
                .child(ui::button("key-gen", "生成新密钥…", 0).opacity(0.5)).child(ui::button("key-import", "导入…", 0).opacity(0.5)).child(div().pt_1().child(ui::soon())))
            .child(Self::section_title("ssh-agent"))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(match agent {
                Some(p) => format!("检测到 SSH_AUTH_SOCK = {p}（连接时使用 agent 即将支持）"),
                None => "未检测到正在运行的 ssh-agent（SSH_AUTH_SOCK 未设置）".into(),
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
                .child(if *ok { ui::tag("已安装 hook", c(th.success)).into_any_element() } else { ui::tag("未安装 hook", c(th.overlay1)).into_any_element() })
                .child(ui::button(("hook-install", i), "一键安装", 0).opacity(0.5)));
        }
        div().flex().flex_col().child(Self::section_title("Hook 状态（只读检测；未安装时仍可通过屏幕/标题识别状态）")).child(list)
            .child(div().pt_2().flex().items_center().gap_2().text_size(px(11.5)).text_color(c(th.overlay1)).child("一键安装 hook").child(ui::soon()))
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
            let e = self.input(&key, if info.key.is_empty() { "未绑定" } else { info.key }, initial, window, cx);
            list = list.child(div().flex().items_center().gap_3().px_3().py(px(6.)).when(i > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                .child(icon(info.icon, 14., c(th.overlay1)))
                .child(div().flex_1().flex().flex_col().child(div().text_size(px(12.5)).text_color(c(th.text)).child(info.label))
                    .when_some(conflict, |d, other| d.child(div().text_size(px(11.)).text_color(c(th.error)).child(format!("与“{other}”冲突"))))
                    .when_some(self.errors.get(&key).cloned(), |d, e| d.child(div().text_size(px(11.)).text_color(c(th.error)).child(e))))
                .child(div().w(px(150.)).flex().justify_end().child(if b.is_empty() { div().text_size(px(11.)).text_color(c(th.overlay0)).child("—").into_any_element() } else { ui::kbd(&b) }))
                .child(div().w(px(170.)).child(Input::new(&e).small())));
        }
        div().flex().flex_col().child(Self::section_title("应用快捷键（终端内 Ctrl+Shift+C / V 复制粘贴固定）")).child(list).into_any_element()
    }

    fn audit_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let path = config::get().audit_path();
        let n = ssh_term_proto::audit::read_all(&path).len();
        div().flex().flex_col().child(Self::section_title("日志"))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(format!("{}（{n} 条记录）", path.display())))
            .child(div().pt_2().flex().gap_2()
                .child(ui::button("open-audit", "打开审计日志查看器", 1).on_click(cx.listener(|_, _, _w, cx| cx.emit(SettingsEvent::OpenAudit)))))
            .into_any_element()
    }

    fn suggest_extra(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        div().flex().flex_col().child(Self::section_title("历史数据"))
            .child(div().flex().items_center().gap_3()
                .child(ui::button("clear-hist", "清空命令历史", 2).on_click(cx.listener(|this, _, _w, cx| {
                    let dir = crate::data_dir().join("history");
                    let n = std::fs::read_dir(&dir).map(|d| d.filter_map(Result::ok).filter(|e| std::fs::write(e.path(), "").is_ok()).count()).unwrap_or(0);
                    this.notice = Some(format!("已清空 {n} 个历史文件"));
                    cx.emit(SettingsEvent::Changed("suggest.clear_history".into()));
                    cx.notify();
                })))
                .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(self.notice.clone().unwrap_or_else(|| "清空本应用保存的历史（不影响远端 shell 历史文件）".into()))))
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
                    .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child(format!("原型版本 {} · GPUI 0.3.9 · alacritty_terminal 0.26 · russh 0.64", env!("CARGO_PKG_VERSION"))))))
            .child(Self::section_title("快速上手"))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).flex().flex_col().gap_1()
                .child("Ctrl+Shift+P 打开命令面板，可搜索所有操作与主机").child("→ / Tab 接受灰色命令建议；Shift+PgUp 或滚轮回看")
                .child("Ctrl+Shift+C / V 复制粘贴；按住 Shift 可在 vim/htop 中本地选择文本"))
            .child(Self::section_title("第三方许可证（THIRD_PARTY_NOTICES.md）")).child(list)
            .into_any_element()
    }
}

fn validate(path: &str, errors: &mut HashMap<String, String>) {
    let s = config::get();
    let msg = match path {
        "font.size" if !(8.0..=32.0).contains(&s.font.size) => Some("字号应在 8–32 之间"),
        "font.line_height" if !(1.0..=2.0).contains(&s.font.line_height) => Some("行高应在 1.0–2.0 之间"),
        "terminal.scrollback" if s.terminal.scrollback > 1_000_000 => Some("最多 1,000,000 行"),
        "suggest.filter_patterns" => {
            let bad: Vec<_> = s.suggest.filter_patterns.iter().filter(|p| regex::Regex::new(p).is_err()).cloned().collect();
            if !bad.is_empty() { errors.insert(path.into(), format!("无效的正则：{}", bad.join(", "))); }
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
        let fp = russh::keys::PublicKey::from_openssh(pubs.trim()).map(|k| k.fingerprint(russh::keys::HashAlg::Sha256).to_string()).unwrap_or_else(|_| "无法解析公钥".into());
        out.push((format!("~/.ssh/{n}"), kind, fp));
    }
    out
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                _ => None,
            };
            if let Some(e) = extra { body = body.child(e); }
        } else {
            let hits: Vec<(usize, &Row)> = rows.iter().enumerate().filter(|(_, r)| {
                format!("{} {} {} {}", r.label, r.desc, r.cat.name(), match &r.ctl { Ctl::Toggle(p) | Ctl::Choice(p, _) | Ctl::Text(p, _) => *p, _ => "" }).to_lowercase().contains(&query)
            }).collect();
            body = body.child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(format!("搜索结果（{}）", hits.len())));
            let mut last = None;
            for (i, row) in hits {
                if last != Some(row.cat) { body = body.child(Self::section_title(row.cat.name())); last = Some(row.cat); }
                body = body.child(self.row_el(row, i, window, cx));
            }
        }

        let header = div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
            .child(icon(IconName::Settings, 16., c(th.accent)))
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child("设置"))
            .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child("修改即保存到 config.toml"))
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
