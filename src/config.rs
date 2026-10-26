//! Settings: `~/.config/shellax/config.toml` (override path with SHELLAX_CONFIG).
//! Every key is optional; missing keys take the defaults below. The settings page edits
//! a copy and writes the whole file back (comments in a hand-edited file are not kept).
//! Generic access by dotted path ("terminal.copy_on_select") goes through toml::Value.
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use ssh_term_proto::hosts::HostEntry;
use ssh_term_proto::policy::{Effect, Mode, Rule};

macro_rules! d { ($name:ident, $t:ty, $v:expr) => { fn $name() -> $t { $v } }; }
d!(t, bool, true);
d!(s_restore, String, "local".into());
d!(s_stable, String, "stable".into());
d!(s_system, String, "system".into());
d!(s_block, String, "block".into());
d!(s_visual, String, "visual".into());
d!(s_auto, String, "auto".into());
d!(s_both, String, "both".into());
d!(s_ask, String, "ask".into());
d!(s_kh, String, "~/.ssh/known_hosts".into());
d!(s_sc, String, "~/.ssh/config".into());
d!(s_screen, String, "screen".into());
d!(s_deny, String, "deny".into());
d!(s_r1, String, "R1".into());
d!(s_off, String, "off".into());
d!(s_auditdir, String, "~/.local/share/shellax/audit".into());
d!(f_size, f32, 13.5);
d!(f_lh, f32, 1.0);
d!(f_pad, f32, 8.0);
d!(f_one, f32, 1.0);
d!(f_ui, f32, 13.0);
d!(u_scroll, usize, 10_000);
d!(u_22, u16, 22);
d!(u_30, u64, 30);
d!(u_15, u64, 15);
d!(u_120, u64, 120);
d!(u_90, u64, 90);
d!(u_40, u64, 40);
d!(u_10, u64, 10);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General { pub language: String, pub startup: String, pub check_updates: bool, pub update_channel: String, pub update_url: String,
    /// 发现新版本后在后台自动下载（需开启自动检查）
    pub auto_download: bool,
    /// system tray icon with the running-agent count (Linux StatusNotifierItem)
    pub tray: bool }
impl Default for General { fn default() -> Self { Self { language: s_system(), startup: s_restore(), check_updates: t(), update_channel: s_stable(), update_url: String::new(), auto_download: false, tray: t() } } }
impl General {
    /// Custom manifest URL template (SHELLAX_UPDATE_URL > general.update_url); None = GitHub Releases.
    pub fn custom_update_url(&self) -> Option<String> {
        std::env::var("SHELLAX_UPDATE_URL").ok().filter(|s| !s.is_empty()).or_else(|| (!self.update_url.is_empty()).then(|| self.update_url.clone()))
    }
    /// Where updates come from, for display.
    pub fn update_source(&self) -> String {
        match self.custom_update_url() { Some(u) => ssh_term_proto::update::url_for(&u, &self.update_channel), None => ssh_term_proto::update::DEFAULT_SOURCE.to_string() }
    }
    /// Run the update check for the configured channel and source (blocking).
    pub fn check_update(&self) -> anyhow::Result<Option<ssh_term_proto::update::Manifest>> {
        ssh_term_proto::update::check_source(self.custom_update_url().as_deref(), &self.update_channel)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub theme: String, pub light_theme: String, pub dark_theme: String,
    /// Kept so older config files still parse. Terminal colors follow the active UI theme.
    pub scheme: String, pub light_scheme: String, pub dark_scheme: String,
    pub opacity: f32, pub ui_font_size: f32,
    /// Skip non-essential animations (menus, dialogs, toasts, skeleton pulses).
    pub reduce_motion: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: s_system(), light_theme: "ShellAX Daylight".into(), dark_theme: "ShellAX Midnight".into(),
            scheme: s_system(), light_scheme: "Catppuccin Latte".into(), dark_scheme: "Catppuccin Mocha".into(),
            opacity: f_one(), ui_font_size: f_ui(), reduce_motion: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Font { pub family: String, pub fallbacks: Vec<String>, pub size: f32, pub line_height: f32, pub ligatures: bool }
impl Default for Font {
    fn default() -> Self {
        let (family, fallbacks): (&str, &[&str]) = if cfg!(target_os = "macos") {
            ("Menlo", &["PingFang SC", "Symbols Nerd Font Mono", "Apple Color Emoji"])
        } else if cfg!(target_os = "windows") {
            ("Cascadia Mono", &["Microsoft YaHei UI", "Symbols Nerd Font Mono", "Segoe UI Symbol", "Segoe Fluent Icons", "Segoe MDL2 Assets", "Segoe UI Emoji"])
        } else {
            ("JetBrains Mono", &["Noto Sans Mono CJK SC", "Symbols Nerd Font Mono", "Noto Color Emoji", "DejaVu Sans Mono"])
        };
        Self { family: family.into(), fallbacks: fallbacks.iter().map(|s| s.to_string()).collect(), size: f_size(), line_height: f_lh(), ligatures: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Terminal {
    /// empty = $SHELL
    pub shell: String, pub scrollback: usize, pub cursor_style: String, pub cursor_blink: bool,
    pub bell: String, pub copy_on_select: bool, pub paste_confirm: bool, pub mouse_reporting: bool,
    /// auto = inject OSC 133 hooks for bash/zsh; off = never
    pub shell_integration: String,
    /// Kept so older config files still parse. The app no longer reads this:
    /// program clipboard writes are always allowed (over 1 MiB refused).
    pub osc52_write: String,
    /// Kept so older config files still parse. Program clipboard reads are always ignored.
    pub osc52_read: bool,
    /// modifier for rectangular selection drag: alt (default) | ctrl (for desktops whose window
    /// manager grabs Alt+drag, e.g. Xfwm / many Linux WMs)
    pub block_select: String,
    /// Keyword highlighting on/off and the rules (regex, colour role or #rrggbb, bold).
    pub keyword_highlight: bool,
    pub highlights: Vec<HighlightRule>,
    /// Space between the pane edge and the text grid, in px (all sides).
    pub padding: f32,
    /// Clickable current-directory bar at the bottom of the terminal card.
    pub show_cwd: bool,
    /// On connect, turn on ls/grep colors for this session only. Does not edit remote shell rc files.
    #[serde(default = "t")]
    pub style: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct HighlightRule { pub pattern: String, pub color: String, pub bold: bool }
fn default_highlights() -> Vec<HighlightRule> { ssh_term_proto::highlight::DEFAULTS.iter().map(|(p, c, b)| HighlightRule { pattern: p.to_string(), color: c.to_string(), bold: *b }).collect() }
impl Default for Terminal { fn default() -> Self { Self { padding: f_pad(), show_cwd: t(), style: t(), keyword_highlight: t(), highlights: default_highlights(), shell: String::new(), scrollback: u_scroll(), cursor_style: s_block(), cursor_blink: t(), bell: s_visual(), copy_on_select: false, paste_confirm: t(), mouse_reporting: t(), shell_integration: s_auto(), osc52_write: "allow".into(), osc52_read: false, block_select: "alt".into() } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Suggest { pub enabled: bool, pub accept: String, pub local_history: bool, pub remote_history: bool, pub filter_patterns: Vec<String>, pub ai: bool }
impl Default for Suggest { fn default() -> Self { Self { enabled: t(), accept: s_both(), local_history: t(), remote_history: t(), filter_patterns: vec!["(?i)vault +login".into()], ai: false } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ssh {
    pub default_user: String, pub default_port: u16, pub keepalive_secs: u64, pub connect_timeout_secs: u64,
    pub known_hosts: String, pub host_key_policy: String, pub jump_host: String, pub ciphers: Vec<String>,
    pub read_ssh_config: bool, pub ssh_config: String, pub agent_forwarding: bool,
    pub kex: Vec<String>, pub reconnect: bool, pub reuse_connections: bool, pub remote_agent_hooks: bool,
    /// Remote program writes to this computer's clipboard: allow | ask | deny. Ask by default
    /// so an SSH session cannot silently replace what the user just copied.
    pub osc52_write: String,
    /// Remote program may read this computer's clipboard (so vim/tmux can paste). On by default.
    pub osc52_read: bool,
    /// Outbound proxy URL (`socks5://` or `http://`). Empty = none.
    /// SSH uses it only for connections that opt in. AI requests and update checks use it whenever it is set.
    pub proxy: String,
    /// Hosts that bypass the proxy (globs, comma separated)
    pub no_proxy: String,
}
impl Default for Ssh {
    fn default() -> Self {
        Self { default_user: String::new(), default_port: u_22(), keepalive_secs: u_30(), connect_timeout_secs: u_15(), known_hosts: s_kh(),
            host_key_policy: s_ask(), jump_host: String::new(), ciphers: vec!["chacha20-poly1305@openssh.com".into(), "aes256-gcm@openssh.com".into(), "aes128-ctr".into()],
            read_ssh_config: t(), ssh_config: s_sc(), agent_forwarding: false,
            kex: vec![], reconnect: t(), reuse_connections: t(), remote_agent_hooks: t(), osc52_write: "ask".into(), osc52_read: true, proxy: String::new(), no_proxy: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Credentials { pub use_keychain: bool, pub use_ssh_agent: bool, pub extra_keys: Vec<String> }
impl Default for Credentials { fn default() -> Self { Self { use_keychain: t(), use_ssh_agent: t(), extra_keys: vec![] } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ai {
    pub enabled: bool,
    /// id of the default provider in `providers`
    pub default_provider: String,
    /// API keys are NOT here: they live in the OS keychain (or the encrypted fallback file)
    pub providers: Vec<ssh_term_proto::ai::Provider>,
    pub redact: bool, pub context: String, pub context_lines: u64, pub send_output: bool,
    /// natural-language prefix typed at the prompt, e.g. "# list big files"
    pub nl_prefix: String,
    /// hide embedding / audio / image models in the model picker
    pub chat_models_only: bool,
    /// model list cache lifetime
    pub models_cache_hours: u64,
    /// The user acknowledged the "data leaves this machine" notice before the first AI request.
    pub first_send_ack: bool,
    /// Hosts (names / addresses, glob, comma separated) whose terminal output is never sent to
    /// AI, even with send_output on: only typed commands are included.
    pub no_output_hosts: String,
    /// After an AI plan finishes, send each step's exit code (and redacted output, unless the
    /// host may not share output) back to the model as the next turn.
    pub feed_back_results: bool,
}
impl Default for Ai { fn default() -> Self { Self { enabled: t(), default_provider: String::new(), providers: vec![], redact: t(), context: s_screen(), context_lines: u_40(), send_output: false, nl_prefix: "#".into(), chat_models_only: t(), models_cache_hours: 24, first_send_ack: false, no_output_hosts: String::new(), feed_back_results: true } } }
impl Ai {
    /// May terminal output of this host (any of the given names) be sent to AI?
    pub fn output_allowed(&self, names: &[&str]) -> bool {
        self.send_output && !self.no_output_hosts.split(',').map(str::trim).filter(|p| !p.is_empty())
            .any(|p| names.iter().any(|n| !n.is_empty() && (p == *n || ssh_term_proto::policy::glob(p, n))))
    }
    pub fn default_provider(&self) -> Option<&ssh_term_proto::ai::Provider> {
        self.providers.iter().find(|p| p.id == self.default_provider).or(self.providers.first())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Agents { pub detect_claude: bool, pub detect_codex: bool, pub detect_grok: bool, pub notify_state: bool, pub approval_timeout_secs: u64, pub on_timeout: String, pub policy_port: u16,
    /// AI typing into a pane running an external agent: ask | ask_first | allow | deny
    pub write_to_pty: String,
    /// Agent quick launch entries (palette, AI / Agent panel): name + command typed into a new pane
    pub quick_launch: Vec<QuickLaunch> }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct QuickLaunch { pub name: String, pub command: String }
pub fn default_quick_launch() -> Vec<QuickLaunch> { [("Claude Code", "claude"), ("Codex", "codex"), ("Grok", "grok")].iter().map(|(n, c)| QuickLaunch { name: n.to_string(), command: c.to_string() }).collect() }
impl Default for Agents { fn default() -> Self { Self { detect_claude: t(), detect_codex: t(), detect_grok: t(), notify_state: t(), approval_timeout_secs: u_120(), on_timeout: s_deny(), policy_port: 0, write_to_pty: "ask".into(), quick_launch: default_quick_launch() } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Permissions { pub mode: Mode, pub max_auto_risk: String, pub prod_confirm: bool, pub dangerous_confirm: bool, pub rules: Vec<Rule> }
impl Default for Permissions {
    fn default() -> Self {
        let r = |id: &str, cmd: &str, e: Effect, why: &str| Rule { id: id.into(), principal: "*".into(), action: "exec".into(), command: cmd.into(), effect: e, reason: why.into() };
        Self { mode: Mode::Ask, max_auto_risk: s_r1(), prod_confirm: t(), dangerous_confirm: t(), rules: vec![
            r("read-only", "ls*", Effect::Allow, "只读命令"),
            r("git-read", "git status*", Effect::Allow, "只读 git"),
            r("no-force-push", "git push*--force*", Effect::Deny, "禁止强推"),
            r("pkg-install", "*install*", Effect::Ask, "安装依赖需确认"),
        ] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Audit { pub dir: String, pub retention_days: u64, pub log_commands: bool }
impl Default for Audit { fn default() -> Self { Self { dir: s_auditdir(), retention_days: u_90(), log_commands: false } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionCfg { pub persistent: String, pub restore_layout: bool, pub confirm_close: bool, pub auto_reconnect: bool, pub local_daemon: bool,
    /// Default SFTP open mode (PRD 5.21): popup | tab | split | window.
    pub sftp_mode: String,
    /// SFTP split pane follows the neighbouring terminal's cwd (OSC 7 / prompt).
    pub sftp_follow_cwd: bool,
    /// Double-click on a sidebar host asks before opening another tab (PRD 5.24).
    pub confirm_new_tab: bool,
    /// After a layout restore, ask before re-running the last Agent command. Off by default.
    pub prompt_resume_agent: bool }
impl Default for SessionCfg { fn default() -> Self { Self { confirm_new_tab: t(), persistent: s_off(), restore_layout: t(), confirm_close: t(), auto_reconnect: t(), local_daemon: false, sftp_mode: "popup".into(), sftp_follow_cwd: t(), prompt_resume_agent: false } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Notify { pub desktop: bool, pub agent_blocked: bool, pub long_command_secs: u64, pub sound: bool,
    /// Keyword-triggered notifications: regex matched against each output line (rate-limited 10s per rule).
    pub keywords: Vec<KeywordNotify> }
impl Default for Notify { fn default() -> Self { Self { desktop: t(), agent_blocked: t(), long_command_secs: u_10(), sound: false, keywords: default_keywords() } } }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct KeywordNotify { pub pattern: String, pub title: String }
fn default_keywords() -> Vec<KeywordNotify> {
    vec![KeywordNotify { pattern: r"(?i)\b(build|deploy|deployment) (failed|succeeded|finished|done)\b".into(), title: "Build/Deploy".into() },
         KeywordNotify { pattern: r"(?i)\b(out of memory|segmentation fault|kernel panic)\b".into(), title: "Crash".into() }]
}
/// Recording (asciicast v2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Recording { pub redact: bool, pub dir: String }
impl Default for Recording { fn default() -> Self { Self { redact: true, dir: String::new() } } }


/// Finder-style color tags (PRD 5.23): custom names per color; one color means "production"
/// for the permission engine (stricter confirmation, `env:prod` rules).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tags {
    pub production: String,
    pub names: std::collections::BTreeMap<String, String>,
    /// User-defined tags (id, name, "#rrggbb").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<TagDef>,
    /// Built-in color tags the user deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct TagDef { pub id: String, pub name: String, pub color: String }
impl Default for Tags { fn default() -> Self { Self { production: "red".into(), names: Default::default(), custom: vec![], hidden: vec![] } } }
impl Tags {
    /// Display name: custom name, else "production" for the production color, else the color name.
    pub fn name(&self, id: &str) -> String {
        if let Some(d) = self.custom.iter().find(|d| d.id == id) { return d.name.clone(); }
        if let Some(n) = self.names.get(id).filter(|n| !n.trim().is_empty()) { return n.clone(); }
        if id == self.production { return ssh_term_proto::i18n::t("tag.production").to_string(); }
        ssh_term_proto::i18n::t(&format!("tag.{id}")).to_string()
    }
    /// Dot color of a tag id.
    pub fn rgb(&self, id: &str) -> u32 {
        self.custom.iter().find(|d| d.id == id).and_then(|d| ssh_term_proto::hosttree::parse_hex(&d.color)).unwrap_or_else(|| ssh_term_proto::hosttree::color_rgb(id))
    }
    /// Defined tags in display order: built-in colors (minus deleted ones), then custom tags.
    pub fn all(&self) -> Vec<(String, String)> {
        ssh_term_proto::hosttree::COLORS.iter().filter(|c| !self.hidden.iter().any(|h| h == *c)).map(|c| (c.to_string(), self.name(c)))
            .chain(self.custom.iter().map(|d| (d.id.clone(), d.name.clone()))).collect()
    }
}

/// Sidebar state persisted across runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Sidebar {
    pub collapsed: Vec<String>,
    /** px, 180–480 */ pub width: f32,
    /** px, 280–560; missing config uses AI_DOCK_W */ #[serde(default = "default_ai_width")] pub ai_width: f32,
    /// Active tool window of the left activity strip (`hosts`, `sftp`, `database`, …).
    pub tool: String,
    /// Bottom tool window (transfers / logs) open, its height in px and active tab.
    pub bottom_open: bool,
    pub bottom_height: f32,
    pub bottom_tab: String,
}
impl Default for Sidebar { fn default() -> Self { Self { collapsed: vec![], width: SIDEBAR_W, ai_width: AI_DOCK_W, tool: "hosts".into(), bottom_open: false, bottom_height: BOTTOM_H, bottom_tab: "transfers".into() } } }
/// Default bottom tool window height.
pub const BOTTOM_H: f32 = 220.0;
/// Default sidebar width (double-click on its edge resets to this).
pub const SIDEBAR_W: f32 = 248.0;
/// Default AI dock width (double-click on its edge resets to this).
pub const AI_DOCK_W: f32 = 380.0;
fn default_ai_width() -> f32 { AI_DOCK_W }

/// A directory the user wants one click away. `host` empty means the local shell;
/// otherwise it is the sidebar host name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Place {
    pub name: String,
    pub path: String,
    pub host: String,
    /// Sidebar group path. Empty means the top level of Connections.
    pub group: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub general: General,
    pub recording: Recording,
    pub appearance: Appearance,
    pub font: Font,
    pub terminal: Terminal,
    pub suggest: Suggest,
    pub ssh: Ssh,
    pub credentials: Credentials,
    pub ai: Ai,
    pub agents: Agents,
    pub permissions: Permissions,
    pub audit: Audit,
    pub session: SessionCfg,
    /// action id -> keystroke, e.g. "split" = "ctrl-shift-d"; missing = default
    pub keys: std::collections::BTreeMap<String, String>,
    pub notify: Notify,
    pub hosts: Vec<HostEntry>,
    /// Host tree groups (PRD 5.23), sidebar order.
    pub groups: Vec<ssh_term_proto::hosttree::GroupEntry>,
    pub tags: Tags,
    pub sidebar: Sidebar,
    /// Saved directories opened from the sidebar.
    pub places: Vec<Place>,
    /// Database connections (`[[db]]`, v0.2): share the host group tree and tags.
    pub db: Vec<ssh_term_proto::assets::DbConn>,
    /// legacy top-level keys (v0.5 files); folded into sections on load
    #[serde(skip_serializing)] pub theme: Option<String>,
    #[serde(skip_serializing)] pub known_hosts: Option<String>,
    #[serde(skip_serializing)] pub ssh_config: Option<String>,
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SHELLAX_CONFIG") { return p.into(); }
    ssh_term_proto::platform::config_dir().join("config.toml")
}

impl Settings {
    pub fn parse(src: &str) -> anyhow::Result<Settings> {
        let mut s: Settings = toml::from_str(src)?;
        if let Some(v) = s.theme.take() { s.appearance.theme = v; }
        if let Some(v) = s.known_hosts.take() { s.ssh.known_hosts = v; }
        if let Some(v) = s.ssh_config.take() { s.ssh.ssh_config = v; }
        for h in &mut s.hosts { h.source = "config"; }
        // PRD 5.23: production flag -> production color tag; groups referenced by hosts exist
        let prod = s.tags.production.clone();
        ssh_term_proto::hosttree::migrate_production(&mut s.hosts, &prod);
        // font.line_height is now a multiplier of the font's own line metrics (ascent + descent);
        // the old default 1.4 (a multiplier of the point size) maps to 1.0
        if (s.font.line_height - 1.4).abs() < 1e-4 { s.font.line_height = 1.0; }
        // built-in themes renamed with the product (AgentShell → shellax)
        for t in [&mut s.appearance.theme, &mut s.appearance.light_theme, &mut s.appearance.dark_theme] {
            for old in ["AgentShell ", "coossh "] { if let Some(rest) = t.strip_prefix(old) { *t = format!("ShellAX {rest}"); } }
            if let Some(rest) = t.strip_prefix("macOS ") { *t = rest.to_string(); }
        }
        ssh_term_proto::hosttree::ensure_groups(&s.hosts, &mut s.groups);
        ssh_term_proto::assets::normalize_dbs(&mut s.db);
        ssh_term_proto::assets::ensure_asset_groups(&s.db, &mut s.groups);
        if s.sidebar.tool.is_empty() { s.sidebar.tool = "hosts".into(); }
        Ok(s)
    }
    pub fn load() -> (Settings, Option<String>) {
        match std::fs::read_to_string(config_path()) {
            Ok(src) => match Settings::parse(&src) { Ok(s) => (s, None), Err(e) => (Settings::default(), Some(format!("config.toml 解析失败：{e}"))) },
            Err(_) => (Settings::default(), None),
        }
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let p = config_path();
        if let Some(d) = p.parent() { std::fs::create_dir_all(d)?; }
        let fresh = tidy_floats(&toml::to_string_pretty(self)?);
        // keep the user's comments, key order and formatting: merge into the existing document
        let body = match std::fs::read_to_string(&p) {
            Ok(old) => merge_preserving(&old, &fresh).unwrap_or_else(|_| format!("# ShellAX settings — written by the settings page\n{fresh}")),
            Err(_) => format!("# ShellAX settings — written by the settings page\n{fresh}"),
        };
        let tmp = p.with_extension("toml.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    // ---- generic dotted-path access used by the settings page
    pub fn get(&self, path: &str) -> Option<toml::Value> {
        let v = toml::Value::try_from(self).ok()?;
        path.split('.').try_fold(v, |v, k| v.get(k).cloned())
    }
    pub fn set(&mut self, path: &str, val: toml::Value) -> anyhow::Result<()> {
        let mut root = toml::Value::try_from(&*self)?;
        let mut cur = &mut root;
        let keys: Vec<&str> = path.split('.').collect();
        for k in &keys[..keys.len() - 1] { cur = cur.get_mut(*k).ok_or_else(|| anyhow::anyhow!("no key {k}"))?; }
        let tbl = cur.as_table_mut().ok_or_else(|| anyhow::anyhow!("not a table"))?;
        let last = keys[keys.len() - 1];
        // keep the existing type: parse strings into numbers where the field is numeric
        let val = match (tbl.get(last), val) {
            (Some(toml::Value::Integer(_)), toml::Value::String(s)) => toml::Value::Integer(s.trim().parse()?),
            (Some(toml::Value::Float(_)), toml::Value::String(s)) => toml::Value::Float(s.trim().parse()?),
            (Some(toml::Value::Array(_)), toml::Value::String(s)) => toml::Value::Array(s.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).map(|x| toml::Value::String(x.into())).collect()),
            (_, v) => v,
        };
        tbl.insert(last.into(), val);
        let hosts = std::mem::take(&mut self.hosts);
        let groups = std::mem::take(&mut self.groups);
        let mut new: Settings = root.try_into()?;
        new.hosts = hosts;
        new.groups = groups;
        *self = new;
        Ok(())
    }
    pub fn get_str(&self, path: &str) -> String {
        match self.get(path) {
            Some(toml::Value::String(s)) => s,
            Some(toml::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "),
            Some(toml::Value::Float(f)) => format!("{}", (f * 100.0).round() / 100.0),
            Some(v) => v.to_string(),
            None => String::new(),
        }
    }
    pub fn get_bool(&self, path: &str) -> bool { matches!(self.get(path), Some(toml::Value::Boolean(true))) }

    /// Production = carries the production tag itself or through a group.
    pub fn host_is_production(&self, h: &HostEntry) -> bool { ssh_term_proto::hosttree::is_production(h, &self.groups, &self.tags.production) }
    /// Hosts of ~/.ssh/config (for import; they are no longer listed automatically).
    pub fn ssh_config_hosts(&self) -> Vec<HostEntry> {
        use ssh_term_proto::hosts::{expand_tilde, load_ssh_config};
        let path = std::env::var("SHELLAX_SSH_CONFIG").unwrap_or_else(|_| self.ssh.ssh_config.clone());
        load_ssh_config(&expand_tilde(&path))
    }
    /// Hosts for connection resolution (jump hosts, agents broker): config.toml entries first,
    /// then ~/.ssh/config (if enabled), defaults applied, production computed from tags.
    /// The sidebar shows only `self.hosts` (PRD 5.23).
    pub fn all_hosts(&self) -> Vec<HostEntry> {
        use ssh_term_proto::hosts::{expand_tilde, load_ssh_config};
        let mut hosts: Vec<HostEntry> = self.hosts.clone();
        for h in &mut hosts { h.production = self.host_is_production(h); }
        let path = std::env::var("SHELLAX_SSH_CONFIG").unwrap_or_else(|_| self.ssh.ssh_config.clone());
        if self.ssh.read_ssh_config {
            for h in load_ssh_config(&expand_tilde(&path)) { if !hosts.iter().any(|x| x.name == h.name) { hosts.push(h); } }
        }
        for h in &mut hosts {
            if h.user.is_none() && !self.ssh.default_user.is_empty() { h.user = Some(self.ssh.default_user.clone()); }
            if h.port.is_none() && self.ssh.default_port != 22 { h.port = Some(self.ssh.default_port); }
        }
        hosts
    }
    pub fn known_hosts_path(&self) -> PathBuf {
        ssh_term_proto::hosts::expand_tilde(&std::env::var("SHELLAX_KNOWN_HOSTS").unwrap_or_else(|_| self.ssh.known_hosts.clone()))
    }
    pub fn ssh_opts(&self) -> ssh_term_proto::hosts::SshOpts {
        ssh_term_proto::hosts::SshOpts {
            default_user: self.ssh.default_user.clone(), default_port: self.ssh.default_port, agent_forward: self.ssh.agent_forwarding,
            kex: self.ssh.kex.clone(), ciphers: self.ssh.ciphers.clone(), jump: self.ssh.jump_host.clone(), remote_hooks: self.ssh.remote_agent_hooks,
            no_agent: !self.credentials.use_ssh_agent, extra_keys: self.credentials.extra_keys.clone(),
        }
    }
    /// Audit log path. The default directory follows `$XDG_DATA_HOME/shellax/audit`
    /// (same base as the GUI's data dir); an explicitly configured dir is used as is.
    pub fn audit_path(&self) -> PathBuf { audit_dir(&self.audit.dir).join("audit.jsonl") }
}

impl Settings {
    /// Inputs for the local policy service.
    pub fn policy_ctx(&self) -> ssh_term_proto::policy_server::PolicyCtx {
        use ssh_term_proto::policy::Risk;
        let mut prod_hosts = Vec::new();
        for h in self.hosts.iter().filter(|h| self.host_is_production(h)) {
            prod_hosts.push(h.name.clone());
            if !h.host.is_empty() { prod_hosts.push(h.host.clone()); }
        }
        ssh_term_proto::policy_server::PolicyCtx {
            mode: self.permissions.mode,
            max_auto: self.permissions.max_auto_risk.parse().unwrap_or(Risk::R1),
            rules: self.permissions.rules.clone(),
            dangerous_confirm: self.permissions.dangerous_confirm,
            prod_confirm: self.permissions.prod_confirm,
            prod_hosts,
            approval_timeout: std::time::Duration::from_secs(self.agents.approval_timeout_secs.clamp(5, 3600)),
            timeout_deny: self.agents.on_timeout != "ask",
        }
    }
}

// ---- process-wide current settings (read from paint code, written by the settings page)
static CURRENT: RwLock<Option<Arc<Settings>>> = RwLock::new(None);
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn get() -> Arc<Settings> {
    if let Some(s) = CURRENT.read().unwrap().as_ref() { return s.clone(); }
    let s = Arc::new(Settings::default());
    *CURRENT.write().unwrap() = Some(s.clone());
    s
}
/// Bumped whenever settings change; paint caches include it in their key.
pub fn generation() -> u64 { GEN.load(std::sync::atomic::Ordering::Relaxed) }

pub fn install(mut s: Settings) {
    if let Ok(v) = std::env::var("THEME") { s.appearance.theme = v; }
    if let Ok(v) = std::env::var("TERM_FONT") { s.font.family = v; }
    if let Some(v) = std::env::var("TERM_FONT_SIZE").ok().and_then(|v| v.parse().ok()) { s.font.size = v; }
    apply_runtime(&s);
    *CURRENT.write().unwrap() = Some(Arc::new(s));
    GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Push settings into library-level knobs (new sessions / connections pick them up).
fn apply_runtime(s: &Settings) {
    ssh_term_proto::i18n::set_language(&s.general.language);
    use std::sync::atomic::Ordering::Relaxed;
    ssh_term_proto::term::SCROLLBACK.store(s.terminal.scrollback.clamp(100, 1_000_000), Relaxed);
    ssh_term_proto::ssh::KEEPALIVE_SECS.store(s.ssh.keepalive_secs, Relaxed);
    ssh_term_proto::ssh::CONNECT_TIMEOUT_SECS.store(s.ssh.connect_timeout_secs, Relaxed);
    ssh_term_proto::ssh::RECONNECT.store(s.ssh.reconnect && s.session.auto_reconnect, Relaxed);
    ssh_term_proto::ssh::REUSE.store(s.ssh.reuse_connections, Relaxed);
    *ssh_term_proto::proxy::URL.lock().unwrap() = s.ssh.proxy.clone();
    *ssh_term_proto::proxy::NO_PROXY.lock().unwrap() = s.ssh.no_proxy.clone();
    ssh_term_proto::ssh::HOST_KEY_POLICY.store(match s.ssh.host_key_policy.as_str() { "strict" => 1, "accept-new" => 2, _ => 0 }, Relaxed);
    ssh_term_proto::local::INTEGRATE.store(if s.terminal.shell_integration == "off" { 0 } else if s.terminal.shell.is_empty() { 1 } else { 2 }, Relaxed);
    ssh_term_proto::ssh::STYLE.store(s.terminal.style, Relaxed);
    let _ = ssh_term_proto::suggest::set_extra_filters(&s.suggest.filter_patterns);
}

/// Edit, persist and publish. Returns an error message for the UI when saving failed.
pub fn update(f: impl FnOnce(&mut Settings)) -> Option<String> {
    let mut s = (*get()).clone();
    f(&mut s);
    let err = s.save().err().map(|e| format!("保存失败：{e}"));
    apply_runtime(&s);
    *CURRENT.write().unwrap() = Some(Arc::new(s));
    GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    err
}

/// f32 settings serialize with binary noise (0.82 → 0.8199999928474426): write the shortest
/// decimal that round-trips through f32 instead.
pub fn tidy_floats(text: &str) -> String {
    use toml_edit::{DocumentMut, Item, Value};
    let Ok(mut doc) = text.parse::<DocumentMut>() else { return text.to_string() };
    fn fix_v(v: &mut Value) {
        match v {
            Value::Float(f) => { let x = *f.value(); let short: f64 = format!("{}", x as f32).parse().unwrap_or(x); if short != x { *v = Value::from(short); } }
            Value::Array(a) => a.iter_mut().for_each(fix_v),
            Value::InlineTable(t) => t.iter_mut().for_each(|(_, v)| fix_v(v)),
            _ => {}
        }
    }
    fn fix(i: &mut Item) {
        match i {
            Item::Value(v) => fix_v(v),
            Item::Table(t) => t.iter_mut().for_each(|(_, i)| fix(i)),
            Item::ArrayOfTables(a) => a.iter_mut().for_each(|t| t.iter_mut().for_each(|(_, i)| fix(i))),
            Item::None => {}
        }
    }
    fix(doc.as_item_mut());
    doc.to_string()
}

/// Write `fresh` (a full serialization of the settings) into the existing TOML text `old`, keeping
/// comments, blank lines and key order of `old`. Changed values keep their trailing comment;
/// keys no longer present are dropped; new keys are appended to their table.
pub fn merge_preserving(old: &str, fresh: &str) -> anyhow::Result<String> {
    use toml_edit::{DocumentMut, Item};
    let mut doc: DocumentMut = old.parse()?;
    let new: DocumentMut = fresh.parse()?;
    fn same(a: &Item, b: &Item) -> bool {
        match (a.as_value(), b.as_value()) {
            (Some(x), Some(y)) => { let (mut x, mut y) = (x.clone(), y.clone()); x.decor_mut().clear(); y.decor_mut().clear(); x.to_string().trim() == y.to_string().trim() }
            _ => a.to_string() == b.to_string(),
        }
    }
    fn merge(dst: &mut toml_edit::Table, src: &toml_edit::Table) {
        let gone: Vec<String> = dst.iter().map(|(k, _)| k.to_string()).filter(|k| !src.contains_key(k)).collect();
        for k in gone { dst.remove(&k); }
        for (k, v) in src.iter() {
            match (dst.get_mut(k), v) {
                (Some(Item::Table(dt)), Item::Table(st)) => merge(dt, st),
                (Some(old), new) if same(old, new) => {}
                (Some(old), new) => {
                    let mut new = new.clone();
                    if let (Some(ov), Some(nv)) = (old.as_value(), new.as_value_mut()) { *nv.decor_mut() = ov.decor().clone(); }
                    *old = new;
                }
                (None, new) => { dst.insert(k, new.clone()); }
            }
        }
    }
    merge(doc.as_table_mut(), new.as_table());
    Ok(doc.to_string())
}

#[cfg(test)]
mod ai_output_tests {
    #[test]
    fn quick_launch_and_new_defaults() {
        let a = super::Agents::default();
        assert_eq!(a.quick_launch.iter().map(|q| q.command.as_str()).collect::<Vec<_>>(), ["claude", "codex", "grok"]);
        assert_eq!(a.write_to_pty, "ask");
        let parsed: super::Agents = toml::from_str("[[quick_launch]]\nname = \"Aider\"\ncommand = \"aider --yes\"\n").unwrap();
        assert_eq!(parsed.quick_launch.len(), 1);
        assert!(super::Terminal::default().keyword_highlight && !super::Terminal::default().highlights.is_empty());
        assert!(super::Ai::default().feed_back_results);
    }
    #[test]
    fn no_output_hosts() {
        let mut a = super::Ai { send_output: true, no_output_hosts: "prod-*, 10.0.0.9".into(), ..Default::default() };
        assert!(a.output_allowed(&["local"]));
        assert!(!a.output_allowed(&["prod-web", "deploy@10.20.0.5:22"]));
        assert!(!a.output_allowed(&["x", "10.0.0.9"]));
        a.send_output = false; assert!(!a.output_allowed(&["local"]));
        assert!(!super::Ai::default().first_send_ack);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_legacy_and_sections() {
        let src = "theme = \"system\"\nknown_hosts = \"~/kh\"\n[font]\nsize = 14.0\n[terminal]\ncopy_on_select = true\n[[hosts]]\nname = \"prod\"\nhost = \"10.0.0.5\"\nuser = \"deploy\"\nport = 2200\nproduction = true\n";
        let s = Settings::parse(src).unwrap();
        assert_eq!(s.ssh.known_hosts, "~/kh");
        assert_eq!(s.font.size, 14.0);
        assert!(s.terminal.copy_on_select && s.terminal.mouse_reporting);
        assert_eq!((s.hosts[0].name.as_str(), s.hosts[0].port, s.host_is_production(&s.hosts[0])), ("prod", Some(2200), true));
        // PRD 5.23: legacy production flag migrated to the production tag
        assert_eq!((s.hosts[0].production, s.hosts[0].tags.as_slice()), (false, &["red".to_string()][..]));
        assert!(s.all_hosts()[0].production && s.policy_ctx().prod_hosts.contains(&"prod".to_string()));
    }
    #[test]
    fn v01_config_upgrades_to_asset_model() {
        // a v0.1 file: hosts, groups and the old sidebar section only
        let src = "[[hosts]]\nname = \"web\"\nhost = \"10.0.0.1\"\ngroup = \"prod\"\n[[groups]]\npath = \"prod\"\ntags = [\"red\"]\n[sidebar]\ncollapsed = [\"prod\"]\nwidth = 300.0\n";
        let s = Settings::parse(src).unwrap();
        assert!(s.db.is_empty());
        assert_eq!((s.sidebar.tool.as_str(), s.sidebar.width, s.sidebar.ai_width, s.sidebar.bottom_open), ("hosts", 300.0, AI_DOCK_W, false));
        assert_eq!(s.sidebar.collapsed, vec!["prod".to_string()]);
        // writing it back and reading again is lossless and adds no [[db]] noise
        let text = toml::to_string_pretty(&s).unwrap();
        assert!(!text.contains("[[db]]"));
        assert_eq!(Settings::parse(&text).unwrap(), s);
    }
    #[test]
    fn db_assets_share_groups() {
        let src = "[[groups]]\npath = \"prod\"\ntags = [\"red\"]\n[[db]]\nname = \"orders\"\nkind = \"mysql\"\nhost = \"10.0.3.12\"\nuser = \"app\"\ngroup = \"prod/east\"\nssh_via = \"web\"\n[[db]]\nkind = \"sqlite\"\nfile = \"/tmp/a.db\"\n";
        let s = Settings::parse(src).unwrap();
        assert_eq!(s.db.len(), 2);
        assert!(s.groups.iter().any(|g| g.path == "prod/east"));
        assert!(!s.db[0].id.is_empty() && s.db[0].id != s.db[1].id);
        assert_eq!(s.db[1].name, "a.db");
        assert!(ssh_term_proto::assets::db_is_production(&s.db[0], &s.groups, &s.tags.production));
        // ids are persisted so secrets stay attached after a reload
        let back = Settings::parse(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back.db, s.db);
    }
    #[test]
    fn dotted_get_set_roundtrip() {
        let mut s = Settings::default();
        s.set("terminal.copy_on_select", toml::Value::Boolean(true)).unwrap();
        s.set("font.size", toml::Value::String("15.5".into())).unwrap();
        s.set("terminal.scrollback", toml::Value::String("5000".into())).unwrap();
        s.set("permissions.mode", toml::Value::String("plan".into())).unwrap();
        s.set("font.fallbacks", toml::Value::String("A, B".into())).unwrap();
        assert!(s.get_bool("terminal.copy_on_select"));
        assert_eq!((s.font.size, s.terminal.scrollback, s.permissions.mode), (15.5, 5000, Mode::Plan));
        assert_eq!(s.font.fallbacks, vec!["A".to_string(), "B".to_string()]);
        assert!(!s.session.prompt_resume_agent);
        s.set("session.prompt_resume_agent", toml::Value::Boolean(true)).unwrap();
        assert!(s.get_bool("session.prompt_resume_agent"));
        let old = Settings::parse("[session]\nrestore_layout = true\n").unwrap();
        assert!(!old.session.prompt_resume_agent);
        assert!(s.set("font.size", toml::Value::String("abc".into())).is_err());
        let back = Settings::parse(&toml::to_string_pretty(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn api_key_never_written_to_config() {
        let dir = std::env::temp_dir().join(format!("as-cfg-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Settings::default();
        let p = ssh_term_proto::ai::Provider { id: "p1".into(), name: "Mock".into(), base_url: "http://127.0.0.1:18080/v1".into(), ..Default::default() };
        let store = ssh_term_proto::secrets::SecretStore::file_only(&dir);
        store.set(&p.secret_name(), "sk-NEVER-IN-CONFIG-4242").unwrap();
        s.ai.default_provider = "p1".into();
        s.ai.providers.push(p);
        let text = toml::to_string_pretty(&s).unwrap();
        assert!(text.contains("[[ai.providers]]") && text.contains("127.0.0.1:18080"));
        assert!(!text.contains("NEVER-IN-CONFIG") && !text.contains("4242"));
        assert!(!text.to_lowercase().contains("api_key"));
        // and the audit log written through the real writer contains no key either
        let log = ssh_term_proto::audit::AuditLog::new(dir.join("audit.jsonl"));
        log.append("ai.request", "user", "provider=Mock model=gpt request=\"list files\" redactions=0").unwrap();
        let audit = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        assert!(!audit.contains("NEVER-IN-CONFIG"));
        let enc = std::fs::read_to_string(dir.join("secrets.enc")).unwrap();
        assert!(!enc.contains("NEVER-IN-CONFIG"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn floats_written_without_f32_noise() {
        let mut s = Settings::default();
        s.set("appearance.opacity", toml::Value::Float(0.82f32 as f64)).unwrap();
        let out = tidy_floats(&toml::to_string_pretty(&s).unwrap());
        assert!(out.contains("opacity = 0.82\n"), "{out}");
        assert!(out.contains("line_height = 1.4\n"), "{out}");
        assert_eq!(Settings::parse(&out).unwrap(), s);
    }

    #[test]
    fn host_tree_round_trip() {
        // groups + tags + collapsed state survive a save (merge) / reload; legacy key dropped
        let old = "# mine\n[general]\nlanguage = \"en\"\n\n[[hosts]]\nname = \"db\"\nhost = \"10.0.0.9\"\ngroup = \"prod/db\"\nproduction = true\n";
        let mut s = super::Settings::parse(old).unwrap();
        assert_eq!(s.groups.iter().map(|g| g.path.as_str()).collect::<Vec<_>>(), ["prod", "prod/db"]);
        s.groups[0].tags.push("orange".into());
        s.sidebar.collapsed.push("prod".into());
        s.tags.names.insert("orange".into(), "预发".into());
        let body = super::merge_preserving(old, &toml::to_string_pretty(&s).unwrap()).unwrap();
        assert!(body.starts_with("# mine") && !body.contains("production = true"), "{body}");
        let r = super::Settings::parse(&body).unwrap();
        assert_eq!((r.groups[0].tags.clone(), r.sidebar.collapsed.clone(), r.tags.name("orange")), (vec!["orange".to_string()], vec!["prod".to_string()], "预发".to_string()));
        assert!(r.host_is_production(&r.hosts[0]));
        // production mapped to another color: red no longer means production
        let mut r2 = r.clone();
        r2.tags.production = "orange".into();
        r2.hosts[0].tags.clear();
        assert!(r2.host_is_production(&r2.hosts[0]), "inherits orange from group prod");
    }
    #[test]
    fn save_preserves_comments() {
        let old = "# my settings\n\n[font]\n# I like big text\nsize = 15.0 # bigger\nfamily = \"JetBrains Mono\"\n\n[general]\nlanguage = \"zh-CN\"\n";
        let mut s = Settings::parse(old).unwrap();
        s.set("font.size", toml::Value::Float(16.0)).unwrap();
        let fresh = toml::to_string_pretty(&s).unwrap();
        let out = merge_preserving(old, &fresh).unwrap();
        assert!(out.contains("# my settings\n\n[font]"), "{out}");
        assert!(out.contains("# I like big text\nsize = 16.0 # bigger"), "{out}");
        assert!(out.contains("language = \"zh-CN\""));
        // round-trips to the same settings
        assert_eq!(Settings::parse(&out).unwrap(), s);
    }
}

/// Resolve the audit dir: the built-in default honours XDG_DATA_HOME.
pub fn audit_dir(dir: &str) -> PathBuf {
    if dir.is_empty() || dir == s_auditdir() {
        if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|x| !x.is_empty()) { return PathBuf::from(x).join("shellax/audit"); }
    }
    ssh_term_proto::hosts::expand_tilde(dir)
}
#[cfg(test)]
mod audit_dir_tests {
    #[test]
    fn default_follows_xdg() {
        std::env::set_var("XDG_DATA_HOME", "/tmp/xdgx");
        assert_eq!(super::audit_dir(&super::s_auditdir()), std::path::PathBuf::from("/tmp/xdgx/shellax/audit"));
        assert_eq!(super::audit_dir("/var/aud"), std::path::PathBuf::from("/var/aud"));
    }
}
