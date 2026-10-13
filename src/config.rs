//! Settings: `~/.config/agentshell/config.toml` (override path with AGENTSHELL_CONFIG).
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
d!(s_auditdir, String, "~/.local/share/agentshell/audit".into());
d!(f_size, f32, 13.5);
d!(f_lh, f32, 1.4);
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
pub struct General { pub language: String, pub startup: String, pub check_updates: bool, pub update_channel: String }
impl Default for General { fn default() -> Self { Self { language: s_system(), startup: s_restore(), check_updates: t(), update_channel: s_stable() } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Appearance { pub theme: String, pub light_theme: String, pub dark_theme: String, pub opacity: f32, pub ui_font_size: f32 }
impl Default for Appearance { fn default() -> Self { Self { theme: s_system(), light_theme: "Catppuccin Latte".into(), dark_theme: "Catppuccin Mocha".into(), opacity: f_one(), ui_font_size: f_ui() } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Font { pub family: String, pub fallbacks: Vec<String>, pub size: f32, pub line_height: f32, pub ligatures: bool }
impl Default for Font {
    fn default() -> Self {
        let (family, fallbacks): (&str, &[&str]) = if cfg!(target_os = "macos") {
            ("Menlo", &["PingFang SC", "Symbols Nerd Font Mono", "Apple Color Emoji"])
        } else if cfg!(target_os = "windows") {
            ("Cascadia Mono", &["Microsoft YaHei UI", "Symbols Nerd Font Mono", "Segoe UI Emoji", "Segoe UI Symbol"])
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
}
impl Default for Terminal { fn default() -> Self { Self { shell: String::new(), scrollback: u_scroll(), cursor_style: s_block(), cursor_blink: t(), bell: s_visual(), copy_on_select: false, paste_confirm: t(), mouse_reporting: t(), shell_integration: s_auto() } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Suggest { pub enabled: bool, pub accept: String, pub local_history: bool, pub remote_history: bool, pub filter_patterns: Vec<String> }
impl Default for Suggest { fn default() -> Self { Self { enabled: t(), accept: s_both(), local_history: t(), remote_history: t(), filter_patterns: vec!["(?i)vault +login".into()] } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Ssh {
    pub default_user: String, pub default_port: u16, pub keepalive_secs: u64, pub connect_timeout_secs: u64,
    pub known_hosts: String, pub host_key_policy: String, pub jump_host: String, pub ciphers: Vec<String>,
    pub read_ssh_config: bool, pub ssh_config: String, pub agent_forwarding: bool,
}
impl Default for Ssh {
    fn default() -> Self {
        Self { default_user: String::new(), default_port: u_22(), keepalive_secs: u_30(), connect_timeout_secs: u_15(), known_hosts: s_kh(),
            host_key_policy: s_ask(), jump_host: String::new(), ciphers: vec!["chacha20-poly1305@openssh.com".into(), "aes256-gcm@openssh.com".into(), "aes128-ctr".into()],
            read_ssh_config: t(), ssh_config: s_sc(), agent_forwarding: false }
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
}
impl Default for Ai { fn default() -> Self { Self { enabled: t(), default_provider: String::new(), providers: vec![], redact: t(), context: s_screen(), context_lines: u_40(), send_output: false, nl_prefix: "#".into() } } }
impl Ai {
    pub fn default_provider(&self) -> Option<&ssh_term_proto::ai::Provider> {
        self.providers.iter().find(|p| p.id == self.default_provider).or(self.providers.first())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Agents { pub detect_claude: bool, pub detect_codex: bool, pub detect_grok: bool, pub notify_state: bool, pub approval_timeout_secs: u64, pub on_timeout: String }
impl Default for Agents { fn default() -> Self { Self { detect_claude: t(), detect_codex: t(), detect_grok: t(), notify_state: t(), approval_timeout_secs: u_120(), on_timeout: s_deny() } } }

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
pub struct SessionCfg { pub persistent: String, pub restore_layout: bool, pub confirm_close: bool, pub auto_reconnect: bool }
impl Default for SessionCfg { fn default() -> Self { Self { persistent: s_off(), restore_layout: t(), confirm_close: t(), auto_reconnect: t() } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Notify { pub desktop: bool, pub agent_blocked: bool, pub long_command_secs: u64, pub sound: bool }
impl Default for Notify { fn default() -> Self { Self { desktop: t(), agent_blocked: t(), long_command_secs: u_10(), sound: false } } }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub general: General,
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
    /// legacy top-level keys (v0.5 files); folded into sections on load
    #[serde(skip_serializing)] pub theme: Option<String>,
    #[serde(skip_serializing)] pub known_hosts: Option<String>,
    #[serde(skip_serializing)] pub ssh_config: Option<String>,
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTSHELL_CONFIG") { return p.into(); }
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/agentshell/config.toml")
}

impl Settings {
    pub fn parse(src: &str) -> anyhow::Result<Settings> {
        let mut s: Settings = toml::from_str(src)?;
        if let Some(v) = s.theme.take() { s.appearance.theme = v; }
        if let Some(v) = s.known_hosts.take() { s.ssh.known_hosts = v; }
        if let Some(v) = s.ssh_config.take() { s.ssh.ssh_config = v; }
        for h in &mut s.hosts { h.source = "config"; }
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
        let body = format!("# AgentShell settings — written by the settings page\n{}", toml::to_string_pretty(self)?);
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
        let mut new: Settings = root.try_into()?;
        new.hosts = hosts;
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

    /// All hosts: config.toml entries first, then ~/.ssh/config (if enabled), defaults applied.
    pub fn all_hosts(&self) -> Vec<HostEntry> {
        use ssh_term_proto::hosts::{expand_tilde, load_ssh_config};
        let mut hosts: Vec<HostEntry> = self.hosts.clone();
        let path = std::env::var("AGENTSHELL_SSH_CONFIG").unwrap_or_else(|_| self.ssh.ssh_config.clone());
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
        ssh_term_proto::hosts::expand_tilde(&std::env::var("AGENTSHELL_KNOWN_HOSTS").unwrap_or_else(|_| self.ssh.known_hosts.clone()))
    }
    pub fn audit_path(&self) -> PathBuf { ssh_term_proto::hosts::expand_tilde(&self.audit.dir).join("audit.jsonl") }
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
    ssh_term_proto::ssh::HOST_KEY_POLICY.store(match s.ssh.host_key_policy.as_str() { "strict" => 1, "accept-new" => 2, _ => 0 }, Relaxed);
    ssh_term_proto::local::INTEGRATE.store(if s.terminal.shell_integration == "off" { 0 } else if s.terminal.shell.is_empty() { 1 } else { 2 }, Relaxed);
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
        assert_eq!((s.hosts[0].name.as_str(), s.hosts[0].port, s.hosts[0].production), ("prod", Some(2200), true));
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
}
