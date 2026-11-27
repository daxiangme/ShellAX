//! Local coding-agent conversation history scanner and incremental index cache.
//!
//! Scans Claude Code (`~/.claude/projects/*/*.jsonl`) and Codex (`~/.codex/sessions/**/*.jsonl`)
//! using bounded head/tail reads (never loading multi-MB transcripts into memory), caches metadata
//! by `(mtime, size)` in `<data_dir>/agent-history.json`, and groups sessions by project.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 128 * 1024;
const TAIL_EXPAND_BYTES: [u64; 2] = [512 * 1024, 2 * 1024 * 1024];
const CACHE_VERSION: u32 = 5;

/// One subscription quota window (e.g. 5-hour rolling window or 7-day weekly window).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotaWindow {
    /// Short window label (`"5h"`, `"7d"`, `"quota"`).
    pub label: String,
    /// Used percentage (`0..=100`).
    pub used_pct: u8,
    /// Optional unix timestamp (seconds) when this window resets.
    #[serde(default)]
    pub resets_at: Option<i64>,
}

impl QuotaWindow {
    pub fn frac(&self) -> f32 {
        (self.used_pct.min(100) as f32) / 100.0
    }
}

/// One model-group quota breakdown (e.g. Antigravity "Gemini Models" vs "Claude and GPT models").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotaGroup {
    pub name: String,
    pub windows: Vec<QuotaWindow>,
}

/// Structured subscription quota snapshot (e.g. Antigravity `/usage`, Codex `rate_limits`, or screen-scraped quota).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionQuota {
    #[serde(default)]
    pub plan: Option<String>,
    pub primary: QuotaWindow,
    #[serde(default)]
    pub secondary: Option<QuotaWindow>,
    #[serde(default)]
    pub groups: Vec<QuotaGroup>,
    #[serde(default)]
    pub updated_at: i64,
}

/// Aggregated local token usage over a rolling time window (fallback when CLI has no quota API).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalUsageWindow {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub turns: u32,
    pub sessions: u32,
}

impl LocalUsageWindow {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.output_tokens)
    }
}

/// Billing / auth mode detected for a coding-agent CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingMode {
    /// Consumer subscription plan (e.g. Antigravity consumer OAuth, Claude Pro/Max, Codex Plus/Pro).
    Subscription,
    /// Direct API key / custom base URL / third-party proxy (no subscription quota).
    Api,
    /// Unknown (only display quota if explicitly reported).
    Unknown,
}

fn has_non_empty_env(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
}

fn claude_settings_uses_api(v: &Value) -> bool {
    if v.get("apiKeyHelper")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
    {
        return true;
    }
    if let Some(env) = v.get("env").and_then(Value::as_object) {
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
        ] {
            if env
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
            {
                return true;
            }
        }
        for flag in [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ] {
            if env
                .get(flag)
                .and_then(Value::as_str)
                .is_some_and(|s| matches!(s.trim(), "1" | "true" | "TRUE"))
            {
                return true;
            }
        }
    }
    false
}

fn parse_codex_toml_uses_api(src: &str) -> bool {
    for line in src.lines() {
        let t = line.split('#').next().unwrap_or("").trim();
        if let Some((k, v)) = t.split_once('=') {
            let key = k.trim();
            let val = v.trim().trim_matches(['"', '\'']).trim();
            if key == "model_provider" && !val.is_empty() && !val.eq_ignore_ascii_case("openai") {
                return true;
            }
            if key == "base_url" && !val.is_empty() {
                return true;
            }
        }
    }
    false
}

fn parse_codex_toml_model(src: &str) -> Option<String> {
    let mut in_table = false;
    for line in src.lines() {
        let t = line.split('#').next().unwrap_or("").trim();
        if t.starts_with('[') {
            in_table = true;
            continue;
        }
        if in_table {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == "model" {
                let val = v.trim().trim_matches(['"', '\'']).trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// Detect whether `agent` is currently configured for subscription or direct API / proxy usage.
/// Cached by config file `(mtime, size)` so UI renders incur zero redundant disk I/O.
pub fn detect_billing_mode(agent: &str) -> BillingMode {
    use std::sync::{Mutex, OnceLock};
    type Stamp = Option<(i64, u64)>;
    static CACHE: OnceLock<Mutex<HashMap<String, ((Stamp, Stamp), BillingMode)>>> = OnceLock::new();

    let aid = crate::agent::normalize_id(agent);
    match aid.as_str() {
        "claude" => {
            if has_non_empty_env("ANTHROPIC_API_KEY")
                || has_non_empty_env("ANTHROPIC_AUTH_TOKEN")
                || has_non_empty_env("ANTHROPIC_BASE_URL")
                || has_non_empty_env("CLAUDE_CODE_USE_BEDROCK")
                || has_non_empty_env("CLAUDE_CODE_USE_VERTEX")
            {
                return BillingMode::Api;
            }
            let Some(home) = home_dir() else {
                return BillingMode::Subscription;
            };
            let base = std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"));
            let p1 = base.join("settings.json");
            let p2 = base.join("settings.local.json");
            let stamps = (file_stamp(&p1), file_stamp(&p2));
            let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
            if let Ok(g) = cache.lock() {
                if let Some((cs, mode)) = g.get(&aid) {
                    if *cs == stamps {
                        return *mode;
                    }
                }
            }
            let mut is_api = false;
            for p in [&p1, &p2] {
                if let Ok(text) = std::fs::read_to_string(p) {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if claude_settings_uses_api(&v) {
                            is_api = true;
                            break;
                        }
                    }
                }
            }
            let mode = if is_api {
                BillingMode::Api
            } else {
                BillingMode::Subscription
            };
            if let Ok(mut g) = cache.lock() {
                g.insert(aid, (stamps, mode));
            }
            mode
        }
        "agy" => {
            if has_non_empty_env("GEMINI_API_KEY") || has_non_empty_env("GOOGLE_API_KEY") {
                return BillingMode::Api;
            }
            let Some(home) = AntigravityProvider::default().agy_home() else {
                return BillingMode::Subscription;
            };
            let p1 = home.join("settings.json");
            let stamps = (file_stamp(&p1), None);
            let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
            if let Ok(g) = cache.lock() {
                if let Some((cs, mode)) = g.get(&aid) {
                    if *cs == stamps {
                        return *mode;
                    }
                }
            }
            let is_api = std::fs::read_to_string(&p1)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| {
                    v.get("authMethod")
                        .and_then(Value::as_str)
                        .map(|m| m.to_ascii_lowercase())
                })
                .is_some_and(|m| m.contains("api") || m.contains("vertex"));
            let mode = if is_api {
                BillingMode::Api
            } else {
                BillingMode::Subscription
            };
            if let Ok(mut g) = cache.lock() {
                g.insert(aid, (stamps, mode));
            }
            mode
        }
        "codex" => {
            if has_non_empty_env("OPENAI_API_KEY") || has_non_empty_env("OPENAI_BASE_URL") {
                return BillingMode::Api;
            }
            let Some(home) = CodexProvider::default().codex_home() else {
                return BillingMode::Subscription;
            };
            let p1 = home.join("config.toml");
            let stamps = (file_stamp(&p1), None);
            let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
            if let Ok(g) = cache.lock() {
                if let Some((cs, mode)) = g.get(&aid) {
                    if *cs == stamps {
                        return *mode;
                    }
                }
            }
            let is_api = std::fs::read_to_string(&p1)
                .ok()
                .is_some_and(|s| parse_codex_toml_uses_api(&s));
            let mode = if is_api {
                BillingMode::Api
            } else {
                BillingMode::Subscription
            };
            if let Ok(mut g) = cache.lock() {
                g.insert(aid, (stamps, mode));
            }
            mode
        }
        _ => BillingMode::Unknown,
    }
}

/// Read default model from `~/.claude/settings.json` / `settings.local.json` (cached by mtime/size)
/// so fresh Claude Code sessions before turn 1 display the configured model.
pub fn load_claude_settings_model() -> Option<String> {
    use std::sync::{Mutex, OnceLock};
    type Stamp = Option<(i64, u64)>;
    static CACHE: OnceLock<Mutex<Option<((Stamp, Stamp), Option<String>)>>> = OnceLock::new();

    let home = home_dir()?;
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let p1 = base.join("settings.json");
    let p2 = base.join("settings.local.json");
    let stamps = (file_stamp(&p1), file_stamp(&p2));
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((cs, m)) = g.as_ref() {
            if *cs == stamps {
                return m.clone();
            }
        }
    }
    let mut model: Option<String> = None;
    for p in [&p2, &p1] {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if let Some(m) = v
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    model = Some(m.to_string());
                    break;
                }
                if let Some(env) = v.get("env").and_then(Value::as_object) {
                    for k in [
                        "ANTHROPIC_MODEL",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
                        "ANTHROPIC_DEFAULT_OPUS_MODEL",
                        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
                        "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    ] {
                        if let Some(m) = env
                            .get(k)
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                        {
                            model = Some(m.to_string());
                            break;
                        }
                    }
                    if model.is_some() {
                        break;
                    }
                }
            }
        }
    }
    if let Ok(mut g) = cache.lock() {
        *g = Some((stamps, model.clone()));
    }
    model
}

/// Read default model from `~/.codex/config.toml` (cached by mtime/size).
pub fn load_codex_config_model() -> Option<String> {
    use std::sync::{Mutex, OnceLock};
    type Stamp = Option<(i64, u64)>;
    static CACHE: OnceLock<Mutex<Option<(Stamp, Option<String>)>>> = OnceLock::new();

    let home = CodexProvider::default().codex_home()?;
    let p1 = home.join("config.toml");
    let stamp = file_stamp(&p1);
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((cs, m)) = g.as_ref() {
            if *cs == stamp {
                return m.clone();
            }
        }
    }
    let model = std::fs::read_to_string(&p1)
        .ok()
        .and_then(|s| parse_codex_toml_model(&s));
    if let Ok(mut g) = cache.lock() {
        *g = Some((stamp, model.clone()));
    }
    model
}

/// Rolling 5h and 24h local usage summary aggregated from an agent's local transcripts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalUsageSummary {
    pub win_5h: LocalUsageWindow,
    pub win_24h: LocalUsageWindow,
}

/// Structured context-window usage derived from a session transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextDetail {
    /// Fill fraction (`0.0..=1.0`). `0.0` represents an explicitly cleared session.
    pub frac: f32,
    pub used: u64,
    pub window: u64,
    /// Compact label for narrow session cards (`"70%"`, `"0%"`, `"6k"`).
    pub short_label: String,
    /// Detailed label for the bottom status bar (`"70% · 701k/1M"`, `"0%"`).
    pub bar_label: String,
    /// Human-readable tooltip breakdown.
    pub tooltip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMeta {
    pub agent: String,
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub cwd: Option<String>,
    pub project: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub first_prompt: Option<String>,
    #[serde(default)]
    pub last_prompt: Option<String>,
    pub started: i64,
    pub updated: i64,
    pub messages: u32,
    #[serde(default)]
    pub ctx_used: Option<u64>,
    #[serde(default)]
    pub ctx_window: Option<u64>,
    /// True if the latest context-affecting event in the transcript was a `/clear` or `/compact`
    /// boundary with no subsequent assistant turn yet.
    #[serde(default)]
    pub ctx_reset: bool,
    /// Target session ID if the transcript ended with a `continued-in` handoff.
    #[serde(default)]
    pub continued_in: Option<String>,
    /// Latest model identifier observed in the transcript (e.g. `"gemini-4-neon-high"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Latest turn output throughput in tenths of tokens/sec (`645` = `64.5 tok/s`).
    #[serde(default)]
    pub last_speed_tps_x10: Option<u32>,
    /// Output token count of the turn used to compute `last_speed_tps_x10`.
    #[serde(default)]
    pub last_output_tokens: Option<u64>,
    /// Elapsed milliseconds of the turn used to compute `last_speed_tps_x10`.
    #[serde(default)]
    pub last_turn_ms: Option<u64>,
    /// Latest subscription quota snapshot observed in the transcript (e.g. Codex `rate_limits`).
    #[serde(default)]
    pub quota: Option<SubscriptionQuota>,
}

impl SessionMeta {
    /// Latest turn output throughput in tokens/sec, if available.
    pub fn speed_tps(&self) -> Option<f32> {
        self.last_speed_tps_x10
            .filter(|&v| v > 0)
            .map(|v| v as f32 / 10.0)
    }
    /// Rich context-window usage detail derived from transcript token accounting.
    pub fn context_detail(&self) -> Option<ContextDetail> {
        let used = self.ctx_used?;
        if used == 0 {
            if !self.ctx_reset {
                return None;
            }
            let win = self.ctx_window.unwrap_or(1_000_000).max(1);
            let tooltip = format!("ctx 0% (0 / {} tokens)", crate::agent::format_tokens(win));
            return Some(ContextDetail {
                frac: 0.0,
                used: 0,
                window: win,
                short_label: "0%".into(),
                bar_label: "0%".into(),
                tooltip,
            });
        }
        let mut win = self.ctx_window.unwrap_or(1_000_000).max(1);
        while used > win {
            win = if win < 1_000_000 {
                1_000_000
            } else {
                win.saturating_mul(2)
            };
        }
        let ratio = (used as f64 / win as f64).clamp(0.0, 1.0);
        let frac = ratio.max(0.04) as f32;
        let pct = (ratio * 100.0).round().min(100.0) as u32;
        let used_s = crate::agent::format_tokens(used);
        let win_s = crate::agent::format_tokens(win);
        let short_label = if pct >= 1 {
            format!("{pct}%")
        } else {
            used_s.clone()
        };
        let bar_label = if pct >= 1 {
            format!("{pct}% · {used_s}/{win_s}")
        } else {
            format!("{used_s}/{win_s}")
        };
        let tooltip = format!("ctx {pct}% ({used_s} / {win_s} tokens)");
        Some(ContextDetail {
            frac,
            used,
            window: win,
            short_label,
            bar_label,
            tooltip,
        })
    }

    /// Context window fill `(0.0..=1.0)` and compact label (`"42%"` / `"12k"` / `"0%"`), if known.
    pub fn context_ring_info(&self) -> Option<(f32, String)> {
        self.context_detail().map(|d| (d.frac, d.short_label))
    }
    /// Best human-readable title for list display.
    pub fn display_title(&self) -> &str {
        for cand in [
            self.title.as_deref(),
            self.last_prompt.as_deref(),
            self.first_prompt.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let t = cand.trim();
            if !t.is_empty() {
                return t;
            }
        }
        &self.id
    }

    /// Case-insensitive substring match across title, prompts, project, cwd, branch, and id.
    pub fn matches_query(&self, lower_query: &str) -> bool {
        if lower_query.is_empty() {
            return true;
        }
        let has = |opt: Option<&str>| {
            opt.is_some_and(|s| s.to_lowercase().contains(lower_query))
        };
        self.project.to_lowercase().contains(lower_query)
            || self.agent.to_lowercase().contains(lower_query)
            || self.id.to_lowercase().contains(lower_query)
            || has(self.title.as_deref())
            || has(self.first_prompt.as_deref())
            || has(self.last_prompt.as_deref())
            || has(self.branch.as_deref())
            || has(self.cwd.as_deref())
    }
}

pub trait HistoryProvider: Send + Sync {
    fn agent(&self) -> &'static str;
    fn roots(&self) -> Vec<PathBuf>;
    fn list_files(&self) -> Vec<PathBuf>;
    fn parse_file(&self, path: &Path, mtime: i64, size: u64) -> Option<SessionMeta>;
    fn extra_sessions(&self) -> Vec<SessionMeta> {
        Vec::new()
    }
}

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, CachedEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CachedEntry {
    mtime: i64,
    size: u64,
    /// `None` caches a negative result (e.g. sidechain or empty file) so we don't re-parse it.
    meta: Option<SessionMeta>,
}

/// Load cached sessions immediately for fast (<100ms) first paint.
pub fn load_cached(cache_path: &Path) -> Vec<SessionMeta> {
    let Ok(bytes) = std::fs::read(cache_path) else {
        return Vec::new();
    };
    let Ok(cf) = serde_json::from_slice::<CacheFile>(&bytes) else {
        return Vec::new();
    };
    if cf.version != CACHE_VERSION {
        return Vec::new();
    }
    let mut out: Vec<SessionMeta> = cf.entries.into_values().filter_map(|e| e.meta).collect();
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Default providers (Claude Code + Codex + Antigravity). Adding a new provider requires no UI changes.
pub fn default_providers() -> Vec<Box<dyn HistoryProvider>> {
    vec![
        Box::new(ClaudeProvider::default()),
        Box::new(CodexProvider::default()),
        Box::new(AntigravityProvider::default()),
    ]
}

/// Incrementally scan all providers, updating `cache_path` on disk if anything changed.
pub fn scan_all(cache_path: &Path, providers: &[Box<dyn HistoryProvider>]) -> Vec<SessionMeta> {
    let mut prev: CacheFile = std::fs::read(cache_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(|c: &CacheFile| c.version == CACHE_VERSION)
        .unwrap_or_else(|| CacheFile {
            version: CACHE_VERSION,
            entries: HashMap::new(),
        });

    let mut next_entries: HashMap<String, CachedEntry> = HashMap::new();
    let mut dirty = false;
    let mut repo_cache: HashMap<String, String> = HashMap::new();

    for provider in providers {
        for path in provider.list_files() {
            let Ok(md) = std::fs::metadata(&path) else {
                continue;
            };
            let size = md.len();
            if size == 0 {
                continue;
            }
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let key = path.to_string_lossy().into_owned();
            if let Some(existing) = prev.entries.remove(&key) {
                if existing.mtime == mtime && existing.size == size {
                    next_entries.insert(key, existing);
                    continue;
                }
            }
            dirty = true;
            let mut meta = provider.parse_file(&path, mtime, size);
            if let Some(m) = meta.as_mut() {
                if let Some(cwd) = m.cwd.as_deref() {
                    m.project = resolve_project_name(cwd, &mut repo_cache);
                }
            }
            next_entries.insert(key, CachedEntry { mtime, size, meta });
        }
    }

    if !prev.entries.is_empty() {
        dirty = true;
    }

    if dirty {
        let cf = CacheFile {
            version: CACHE_VERSION,
            entries: next_entries.clone(),
        };
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec(&cf) {
            let tmp = cache_path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(tmp, cache_path);
            }
        }
    }

    let mut out: Vec<SessionMeta> = next_entries.into_values().filter_map(|e| e.meta).collect();
    let mut seen: std::collections::HashSet<(String, String)> = out
        .iter()
        .map(|m| (m.agent.clone(), m.id.clone()))
        .collect();
    for provider in providers {
        for mut m in provider.extra_sessions() {
            if let Some(cwd) = m.cwd.as_deref() {
                m.project = resolve_project_name(cwd, &mut repo_cache);
            }
            if seen.insert((m.agent.clone(), m.id.clone())) {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Parse an optional bracketed context-window suffix such as `"claude-fable-5-2[1M]"`
/// or `"my-model[512k]"` into `(base_model_lower, window_tokens)`.
fn parse_bracket_window(raw: &str) -> Option<(String, u64)> {
    let start = raw.rfind('[')?;
    let rest = &raw[start + 1..];
    let end = rest.find(']')?;
    let inner = rest[..end].trim();
    let tokens = crate::agent::parse_scaled_number(inner)?.round() as u64;
    if tokens < 10_000 {
        return None;
    }
    let base = format!("{}{}", &raw[..start], &rest[end + 1..])
        .trim()
        .to_ascii_lowercase();
    Some((base, tokens))
}

fn extract_claude_settings_model_windows(v: &Value, out: &mut HashMap<String, u64>) {
    if let Some(m) = v.get("model").and_then(Value::as_str) {
        if let Some((base, win)) = parse_bracket_window(m) {
            if !base.is_empty() {
                out.insert(base, win);
            }
        }
    }
    if let Some(env) = v.get("env").and_then(Value::as_object) {
        for (k, val) in env {
            if !k.to_ascii_uppercase().contains("MODEL") {
                continue;
            }
            if let Some(s) = val.as_str() {
                if let Some((base, win)) = parse_bracket_window(s) {
                    if !base.is_empty() {
                        out.insert(base, win);
                    }
                }
            }
        }
    }
}

fn load_claude_settings_model_windows() -> HashMap<String, u64> {
    use std::sync::{Mutex, OnceLock};
    type Stamp = Option<(i64, u64)>;
    type Cache = ((Stamp, Stamp), HashMap<String, u64>);
    static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();

    let Some(home) = home_dir() else {
        return HashMap::new();
    };
    let p1 = home.join(".claude").join("settings.json");
    let p2 = home.join(".claude").join("settings.local.json");
    let stamps = (file_stamp(&p1), file_stamp(&p2));

    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((cs, map)) = g.as_ref() {
            if *cs == stamps {
                return map.clone();
            }
        }
    }

    let mut out = HashMap::new();
    for (k, v) in std::env::vars() {
        if !k.to_ascii_uppercase().contains("MODEL") {
            continue;
        }
        if let Some((base, win)) = parse_bracket_window(&v) {
            if !base.is_empty() {
                out.insert(base, win);
            }
        }
    }
    for p in [&p1, &p2] {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                extract_claude_settings_model_windows(&v, &mut out);
            }
        }
    }
    if let Ok(mut g) = cache.lock() {
        *g = Some((stamps, out.clone()));
    }
    out
}

fn claude_model_context_window_with_overrides(
    model: &str,
    overrides: &HashMap<String, u64>,
) -> u64 {
    if let Some((_, w)) = parse_bracket_window(model) {
        return w;
    }
    let ml = model.to_ascii_lowercase();
    if let Some(&w) = overrides.get(&ml) {
        return w;
    }
    if let Some((_, &w)) = overrides
        .iter()
        .find(|(k, _)| !k.is_empty() && ml.starts_with(k.as_str()))
    {
        return w;
    }
    if ml.contains("[2m]") || ml.contains("2m") {
        2_000_000
    } else if ml.contains("[1m]")
        || ml.contains("1m")
        || ml.contains("opus-4-7")
        || ml.contains("opus-4.7")
        || ml.contains("opus-4-6")
        || ml.contains("opus-4.6")
        || ml.contains("opus-4-5")
        || ml.contains("opus-4.5")
        || ml.contains("opus-5")
        || ml.contains("sonnet-4-7")
        || ml.contains("sonnet-4.7")
        || ml.contains("sonnet-4-6")
        || ml.contains("sonnet-4.6")
        || ml.contains("sonnet-4-5")
        || ml.contains("sonnet-4.5")
        || ml.contains("sonnet-5")
        || ml.contains("fable-5")
        || ml.contains("haiku-5")
    {
        1_000_000
    } else {
        200_000
    }
}

fn claude_model_context_window(model: &str) -> u64 {
    let overrides = load_claude_settings_model_windows();
    claude_model_context_window_with_overrides(model, &overrides)
}

fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

pub fn resolve_project_name_for_dir(cwd: &str) -> String {
    use std::sync::{Mutex, OnceLock};
    static GLOBAL_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let global = GLOBAL_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut map) = global.lock() {
        if map.len() > 1024 {
            map.clear();
        }
        return resolve_project_name(cwd, &mut map);
    }
    let mut fallback = HashMap::new();
    resolve_project_name(cwd, &mut fallback)
}

fn resolve_project_name(cwd: &str, cache: &mut HashMap<String, String>) -> String {
    if let Some(v) = cache.get(cwd) {
        return v.clone();
    }
    let p = Path::new(cwd);
    let mut cur = Some(p);
    let mut found: Option<String> = None;
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            found = dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty());
            break;
        }
        cur = dir.parent();
    }
    let name = found.unwrap_or_else(|| {
        cwd.trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(cwd)
            .to_string()
    });
    cache.insert(cwd.to_string(), name.clone());
    name
}

/// Read complete JSONL lines from the head and (if larger) the tail of `path`.
fn read_head_tail_lines(path: &Path, size: u64) -> Option<(Vec<String>, Vec<String>, bool)> {
    let mut f = File::open(path).ok()?;
    if size <= HEAD_BYTES + TAIL_BYTES {
        let mut buf = Vec::with_capacity(size as usize);
        f.read_to_end(&mut buf).ok()?;
        let lines = split_lines_lossy(&buf, false);
        return Some((lines, Vec::new(), true));
    }
    let mut head = vec![0u8; HEAD_BYTES as usize];
    f.read_exact(&mut head).ok()?;
    // Trim incomplete trailing line in head chunk.
    if let Some(last_nl) = head.iter().rposition(|&b| b == b'\n') {
        head.truncate(last_nl + 1);
    }
    f.seek(SeekFrom::Start(size - TAIL_BYTES)).ok()?;
    let mut tail = vec![0u8; TAIL_BYTES as usize];
    f.read_exact(&mut tail).ok()?;
    Some((
        split_lines_lossy(&head, false),
        split_lines_lossy(&tail, true),
        false,
    ))
}

/// Read complete JSONL lines from the last `tail_bytes` of `path`.
fn read_tail_lines(path: &Path, size: u64, tail_bytes: u64) -> Option<Vec<String>> {
    let mut f = File::open(path).ok()?;
    let take = size.min(tail_bytes);
    let skip_first = size > take;
    if skip_first {
        f.seek(SeekFrom::Start(size - take)).ok()?;
    }
    let mut buf = vec![0u8; take as usize];
    f.read_exact(&mut buf).ok()?;
    Some(split_lines_lossy(&buf, skip_first))
}

fn split_lines_lossy(bytes: &[u8], skip_first_partial: bool) -> Vec<String> {
    let start = if skip_first_partial {
        match bytes.iter().position(|&b| b == b'\n') {
            Some(i) => i + 1,
            None => return Vec::new(),
        }
    } else {
        0
    };
    bytes[start..]
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| {
            let l = l.strip_suffix(b"\r").unwrap_or(l);
            String::from_utf8_lossy(l).into_owned()
        })
        .collect()
}

fn clean_prompt_snippet(raw: &str) -> Option<String> {
    let mut cleaned = String::new();
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                cleaned.push(' ');
            }
            _ if !in_tag => cleaned.push(ch),
            _ => {}
        }
    }
    let one_line: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = one_line.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("[Request interrupted")
        || trimmed == "Warmup"
    {
        return None;
    }
    Some(trimmed.chars().take(160).collect())
}

fn parse_iso_secs(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp())
}

fn parse_iso_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn compute_tps_x10(output_tokens: u64, elapsed_ms: u64) -> Option<u32> {
    if output_tokens == 0 || !(200..=600_000).contains(&elapsed_ms) {
        return None;
    }
    let tps_x10 = ((output_tokens as f64) * 10_000.0 / (elapsed_ms as f64)).round();
    if !tps_x10.is_finite() || tps_x10 < 1.0 || tps_x10 > 50_000.0 {
        return None;
    }
    Some(tps_x10 as u32)
}

/// Format a raw model identifier into a compact, readable label for the bottom status bar.
///
/// Examples:
/// - `"gemini-4-neon-high"` -> `"Gemini 4 Neon (High)"`
/// - `"claude-opus-5-5-high"` -> `"Opus 5.5 (High)"`
/// - `"claude-sonnet-4-6-20251022"` -> `"Sonnet 4.6"`
/// - `"gpt-5.3-codex"` -> `"GPT-5.3 Codex"`
pub fn format_model_short(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Strip optional bracketed context window suffix like `[1M]`.
    let clean = match trimmed.find('[') {
        Some(i) => trimmed[..i].trim(),
        None => trimmed,
    };
    // Already human-formatted (contains space, e.g. `"Gemini 4 Neon (High)"`).
    if clean.contains(' ') {
        return clean.to_string();
    }
    let lower = clean.to_ascii_lowercase();
    let cap_effort = |tok: &str| -> Option<&'static str> {
        match tok {
            "low" => Some("Low"),
            "medium" | "med" => Some("Med"),
            "high" => Some("High"),
            "xhigh" => Some("XHigh"),
            "max" => Some("Max"),
            _ => None,
        }
    };
    let title_word = |w: &str| -> String {
        let mut ch = w.chars();
        match ch.next() {
            None => String::new(),
            Some(first) => first.to_uppercase().collect::<String>() + ch.as_str(),
        }
    };

    // Strip trailing YYYYMMDD date tag (e.g. `-20251022`).
    let mut parts: Vec<&str> = lower.split('-').filter(|s| !s.is_empty()).collect();
    if parts.len() > 2
        && parts
            .last()
            .is_some_and(|p| p.len() == 8 && p.chars().all(|c| c.is_ascii_digit()))
    {
        parts.pop();
    }
    let effort = parts.last().and_then(|p| cap_effort(p));
    if effort.is_some() && parts.len() > 1 {
        parts.pop();
    }

    let base = if parts.first() == Some(&"claude") && parts.len() >= 2 {
        // claude-opus-5-5 -> Opus 5.5
        let family = title_word(parts[1]);
        let ver_parts = &parts[2..];
        let ver = if ver_parts.len() == 2
            && ver_parts.iter().all(|s| s.chars().all(|c| c.is_ascii_digit()))
        {
            format!("{}.{}", ver_parts[0], ver_parts[1])
        } else {
            ver_parts.join(".")
        };
        if ver.is_empty() {
            family
        } else {
            format!("{family} {ver}")
        }
    } else if parts.first() == Some(&"gemini") {
        // gemini-4-neon -> Gemini 4 Neon, gemini-pro-orchestrator-neon -> Orchestrator (Neon)
        if parts.get(1) == Some(&"pro") && parts.get(2) == Some(&"orchestrator") {
            if let Some(sub) = parts.get(3) {
                format!("Orchestrator ({})", title_word(sub))
            } else {
                "Orchestrator".to_string()
            }
        } else {
            parts.iter().map(|p| title_word(p)).collect::<Vec<_>>().join(" ")
        }
    } else if parts.first() == Some(&"gpt") && parts.len() >= 2 {
        let rest = parts[1..]
            .iter()
            .map(|p| {
                if p.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    (*p).to_string()
                } else {
                    title_word(p)
                }
            })
            .collect::<Vec<_>>();
        if rest.len() >= 2 {
            format!("GPT-{} {}", rest[0], rest[1..].join(" "))
        } else {
            format!("GPT-{}", rest[0])
        }
    } else {
        clean.to_string()
    };

    match effort {
        Some(ef) => format!("{base} ({ef})"),
        None => base,
    }
}

#[derive(Default, Clone)]
struct CtxParseState {
    used: Option<u64>,
    window: Option<u64>,
    /// Explicit context window observed from `/context` markdown `(model_lower, window)` so
    /// subsequent assistant turns for the same API model do not downgrade it back to 200k.
    explicit_window: Option<(Option<String>, u64)>,
    reset: bool,
    continued_in: Option<String>,
    saw_event: bool,
    model: Option<String>,
    last_speed_tps_x10: Option<u32>,
    last_output_tokens: Option<u64>,
    last_turn_ms: Option<u64>,
    prev_ts_ms: Option<i64>,
    quota: Option<SubscriptionQuota>,
}

struct ClaudeContextMarkdown {
    used: Option<u64>,
    parsed_win: Option<u64>,
    model_win: Option<u64>,
    model_name: Option<String>,
}

fn parse_claude_context_usage_markdown(raw: &str) -> Option<ClaudeContextMarkdown> {
    if !raw.contains("## Context Usage") && !raw.to_ascii_lowercase().contains("## context usage") {
        return None;
    }
    let mut used = None;
    let mut parsed_win = None;
    let mut model_win = None;
    let mut model_name = None;
    for line in raw.lines() {
        let cleaned = line.trim().trim_start_matches(['-', '*', ' ']).replace("**", "");
        let lower = cleaned.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix("model:")
            .and_then(|_| cleaned.split_once(':').map(|(_, v)| v.trim()))
        {
            if !rest.is_empty() {
                model_name = Some(rest.to_string());
                model_win = Some(claude_model_context_window(rest));
            }
        } else if lower.starts_with("tokens:") {
            if let Some((_, val)) = cleaned.split_once(':') {
                if let Some((lhs, rhs)) = val.split_once('/') {
                    used = lhs
                        .split_whitespace()
                        .next()
                        .and_then(crate::agent::parse_scaled_number)
                        .map(|v| v.round() as u64);
                    parsed_win = rhs
                        .split_whitespace()
                        .next()
                        .and_then(crate::agent::parse_scaled_number)
                        .map(|v| v.round() as u64);
                }
            }
        }
    }
    (used.is_some() || parsed_win.is_some()).then_some(ClaudeContextMarkdown {
        used,
        parsed_win,
        model_win,
        model_name,
    })
}

fn inspect_claude_ctx(v: &Value, st: &mut CtxParseState) {
    match v.get("type").and_then(Value::as_str) {
        Some("continued-in") => {
            if let Some(next_id) = v
                .get("continuedInSessionId")
                .or_else(|| v.get("newSessionId"))
                .or_else(|| v.get("targetSessionId"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                st.continued_in = Some(next_id.to_string());
            }
        }
        Some("system") => {
            let sub = v.get("subtype").and_then(Value::as_str);
            match sub {
                Some("compact_boundary" | "microcompact_boundary") => {
                    let post = v
                        .get("compactMetadata")
                        .and_then(|m| {
                            m.get("postTokens")
                                .or_else(|| m.get("post_tokens"))
                                .and_then(Value::as_u64)
                        })
                        .filter(|&n| n > 0);
                    st.used = post;
                    st.reset = true;
                    st.saw_event = true;
                }
                Some("local_command") => {
                    let cmd = v
                        .get("commandRun")
                        .and_then(|c| c.get("command"))
                        .and_then(Value::as_str);
                    let content = v.get("content").and_then(Value::as_str).unwrap_or("");
                    if cmd == Some("clear")
                        || content.contains("<command-name>/clear</command-name>")
                    {
                        st.used = Some(0);
                        st.reset = true;
                        st.saw_event = true;
                    } else if cmd == Some("compact")
                        || content.contains("<command-name>/compact</command-name>")
                    {
                        st.used = None;
                        st.reset = true;
                        st.saw_event = true;
                    }
                }
                _ => {}
            }
        }
        Some("user") => {
            if v.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
                st.used = None;
                st.reset = true;
                st.saw_event = true;
            } else if let Some(raw) = extract_claude_message_text(v) {
                if raw.contains("<command-name>/clear</command-name>") {
                    st.used = Some(0);
                    st.reset = true;
                    st.saw_event = true;
                } else if raw.contains("<command-name>/compact</command-name>") {
                    st.used = None;
                    st.reset = true;
                    st.saw_event = true;
                } else if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    if let Some(cm) = parse_claude_context_usage_markdown(&raw) {
                        if let Some(u) = cm.used {
                            st.used = Some(u);
                            st.saw_event = true;
                        }
                        if let Some(pw) = cm.parsed_win.filter(|&w| w > 0) {
                            st.window = Some(pw);
                            st.explicit_window =
                                Some((cm.model_name.map(|s| s.to_ascii_lowercase()), pw));
                        } else if let Some(mw) = cm.model_win {
                            st.window = Some(mw);
                        }
                    }
                }
            }
        }
        Some("assistant") => {
            if let Some(msg) = v.get("message") {
                let model = msg.get("model").and_then(Value::as_str);
                if model == Some("<synthetic>") {
                    return;
                }
                if let Some(model) = model.filter(|s| !s.is_empty()) {
                    st.model = Some(model.to_string());
                    let inferred = claude_model_context_window(model);
                    let ml = model.to_ascii_lowercase();
                    let kept_explicit = st.explicit_window.as_ref().and_then(|(em, ew)| {
                        em.as_deref()
                            .is_none_or(|m| m == ml)
                            .then_some((*ew).max(inferred))
                    });
                    if let Some(w) = kept_explicit {
                        st.window = Some(w);
                    } else {
                        st.explicit_window = None;
                        st.window = Some(inferred);
                    }
                }
                if let Some(u) = msg.get("usage").or_else(|| v.get("usage")) {
                    let input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let create = u
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let read = u
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let sum = input.saturating_add(create).saturating_add(read);
                    if sum > 0 {
                        st.used = Some(sum);
                        st.reset = false;
                        st.saw_event = true;
                    }
                    let out_tok = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                    if out_tok > 0 {
                        if let (Some(cur_ms), Some(prev_ms)) = (
                            v.get("timestamp").and_then(Value::as_str).and_then(parse_iso_ms),
                            st.prev_ts_ms,
                        ) {
                            if cur_ms > prev_ms {
                                let dt_ms = (cur_ms - prev_ms) as u64;
                                if let Some(tps_x10) = compute_tps_x10(out_tok, dt_ms) {
                                    st.last_speed_tps_x10 = Some(tps_x10);
                                    st.last_output_tokens = Some(out_tok);
                                    st.last_turn_ms = Some(dt_ms);
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    if let Some(ts_ms) = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_iso_ms)
    {
        st.prev_ts_ms = Some(ts_ms);
    }
}

fn parse_codex_quota_window(v: &Value, default_label: &str) -> Option<QuotaWindow> {
    let pct_raw = v.get("used_percent").and_then(Value::as_f64)?;
    if !pct_raw.is_finite() {
        return None;
    }
    let used_pct = pct_raw.round().clamp(0.0, 100.0) as u8;
    let label = match v.get("window_minutes").and_then(Value::as_u64) {
        Some(m) if m >= 24 * 60 => format!("{}d", (m + 720) / (24 * 60)),
        Some(m) if m >= 60 => format!("{}h", (m + 30) / 60),
        Some(m) if m > 0 => format!("{m}m"),
        _ => default_label.to_string(),
    };
    let resets_at = v
        .get("resets_at")
        .and_then(|r| {
            r.as_i64()
                .or_else(|| r.as_str().and_then(parse_iso_secs))
        });
    Some(QuotaWindow {
        label,
        used_pct,
        resets_at,
    })
}

fn inspect_codex_ctx(v: &Value, st: &mut CtxParseState) {
    let cur_ts_ms = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_iso_ms);
    match v.get("type").and_then(Value::as_str) {
        Some("compacted") => {
            st.used = None;
            st.reset = true;
            st.saw_event = true;
        }
        Some("turn_context") => {
            if let Some(m) = v
                .get("payload")
                .and_then(|p| p.get("model"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                st.model = Some(m.to_string());
            }
        }
        Some("event_msg") => {
            if let Some(p) = v.get("payload") {
                match p.get("type").and_then(Value::as_str) {
                    Some("context_compacted") => {
                        st.used = None;
                        st.reset = true;
                        st.saw_event = true;
                    }
                    Some("token_count") => {
                        if let Some(info) = p.get("info").filter(|x| !x.is_null()) {
                            if let Some(win) = info
                                .get("model_context_window")
                                .and_then(Value::as_u64)
                                .filter(|&w| w > 0)
                            {
                                st.window = Some(win);
                            }
                            if let Some(last_u) = info.get("last_token_usage") {
                                if let Some(u) = last_u
                                    .get("input_tokens")
                                    .or_else(|| last_u.get("total_tokens"))
                                    .and_then(Value::as_u64)
                                    .filter(|&n| n > 0)
                                {
                                    st.used = Some(u);
                                    st.reset = false;
                                    st.saw_event = true;
                                }
                                let out_tok = last_u
                                    .get("output_tokens")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(0);
                                if out_tok > 0 {
                                    if let (Some(c_ms), Some(p_ms)) = (cur_ts_ms, st.prev_ts_ms) {
                                        if c_ms > p_ms {
                                            let dt_ms = (c_ms - p_ms) as u64;
                                            if let Some(tps_x10) = compute_tps_x10(out_tok, dt_ms) {
                                                st.last_speed_tps_x10 = Some(tps_x10);
                                                st.last_output_tokens = Some(out_tok);
                                                st.last_turn_ms = Some(dt_ms);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(rl) = p.get("rate_limits").filter(|x| x.is_object()) {
                            let primary = rl
                                .get("primary")
                                .and_then(|w| parse_codex_quota_window(w, "5h"));
                            let secondary = rl
                                .get("secondary")
                                .and_then(|w| parse_codex_quota_window(w, "7d"));
                            if let Some(prim) = primary {
                                let plan = rl
                                    .get("plan_type")
                                    .and_then(Value::as_str)
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_string);
                                st.quota = Some(SubscriptionQuota {
                                    plan,
                                    primary: prim,
                                    secondary,
                                    groups: Vec::new(),
                                    updated_at: cur_ts_ms.map(|ms| ms / 1000).unwrap_or_else(now_unix_secs),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if let Some(ts_ms) = cur_ts_ms {
        st.prev_ts_ms = Some(ts_ms);
    }
}

// ============================================================================
// Claude Code Provider (`~/.claude/projects/<project>/<sessionId>.jsonl`)
// ============================================================================

#[derive(Default)]
pub struct ClaudeProvider {
    custom_root: Option<PathBuf>,
}

impl ClaudeProvider {
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            custom_root: Some(root),
        }
    }

    fn projects_dir(&self) -> Option<PathBuf> {
        if let Some(r) = &self.custom_root {
            return Some(r.clone());
        }
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|s| !s.is_empty()) {
            return Some(PathBuf::from(dir).join("projects"));
        }
        Some(home_dir()?.join(".claude").join("projects"))
    }
}

impl HistoryProvider for ClaudeProvider {
    fn agent(&self) -> &'static str {
        "claude"
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.projects_dir().into_iter().collect()
    }

    fn list_files(&self) -> Vec<PathBuf> {
        let Some(root) = self.projects_dir() else {
            return Vec::new();
        };
        let Ok(projects) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for proj in projects.flatten() {
            let p = proj.path();
            if !p.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&p) else {
                continue;
            };
            for f in files.flatten() {
                let fp = f.path();
                if fp.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    out.push(fp);
                }
            }
        }
        out
    }

    fn parse_file(&self, path: &Path, mtime: i64, size: u64) -> Option<SessionMeta> {
        let stem = path.file_stem()?.to_str()?.to_string();
        if stem.is_empty() || stem.starts_with("agent-") {
            return None;
        }
        let (head, tail, full) = read_head_tail_lines(path, size)?;
        let mut session_id = stem;
        let mut cwd: Option<String> = None;
        let mut branch: Option<String> = None;
        let mut title: Option<String> = None;
        let mut first_prompt: Option<String> = None;
        let mut last_prompt: Option<String> = None;
        let mut started: Option<i64> = None;
        let mut updated: Option<i64> = None;
        let mut sampled_msgs: u32 = 0;
        let mut ctx_st = CtxParseState::default();

        {
            let mut inspect = |line: &str| -> bool {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    return true;
                };
                if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                    return false;
                }
                if let Some(sid) = v.get("sessionId").and_then(Value::as_str) {
                    if !sid.is_empty() && sid.len() >= session_id.len() {
                        session_id = sid.to_string();
                    }
                }
                if cwd.is_none() {
                    cwd = v
                        .get("cwd")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
                if let Some(b) = v
                    .get("gitBranch")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && *s != "HEAD")
                {
                    branch = Some(b.to_string());
                }
                if let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(parse_iso_secs) {
                    if started.is_none_or(|s| ts < s) {
                        started = Some(ts);
                    }
                    if updated.is_none_or(|u| ts > u) {
                        updated = Some(ts);
                    }
                }
                match v.get("type").and_then(Value::as_str) {
                    Some("custom-title") => {
                        if let Some(t) = v
                            .get("customTitle")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                        {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("ai-title") => {
                        if let Some(t) = v.get("aiTitle").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("agent-name") if title.is_none() => {
                        if let Some(t) = v.get("agentName").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("summary") if title.is_none() => {
                        if let Some(t) = v.get("summary").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("last-prompt") => {
                        if let Some(lp) = v
                            .get("lastPrompt")
                            .and_then(Value::as_str)
                            .and_then(clean_prompt_snippet)
                        {
                            last_prompt = Some(lp);
                        }
                    }
                    Some("user") => {
                        sampled_msgs += 1;
                        if v.get("isMeta").and_then(Value::as_bool) != Some(true)
                            && v.get("isCompactSummary").and_then(Value::as_bool) != Some(true)
                        {
                            if let Some(raw) = extract_claude_message_text(&v) {
                                if !is_claude_local_cmd_wrapper(&raw) {
                                    if let Some(text) = clean_prompt_snippet(&raw) {
                                        if first_prompt.is_none() {
                                            first_prompt = Some(text.clone());
                                        }
                                        last_prompt = Some(text);
                                    }
                                }
                            }
                        }
                    }
                    Some("assistant") => {
                        sampled_msgs += 1;
                    }
                    _ => {}
                }
                inspect_claude_ctx(&v, &mut ctx_st);
                true
            };

            for line in &head {
                if !inspect(line) {
                    return None;
                }
            }
        }
        ctx_st.saw_event = false;
        {
            let mut inspect = |line: &str| -> bool {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    return true;
                };
                if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                    return false;
                }
                if let Some(sid) = v.get("sessionId").and_then(Value::as_str) {
                    if !sid.is_empty() && sid.len() >= session_id.len() {
                        session_id = sid.to_string();
                    }
                }
                if cwd.is_none() {
                    cwd = v
                        .get("cwd")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
                if let Some(b) = v
                    .get("gitBranch")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty() && *s != "HEAD")
                {
                    branch = Some(b.to_string());
                }
                if let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(parse_iso_secs) {
                    if started.is_none_or(|s| ts < s) {
                        started = Some(ts);
                    }
                    if updated.is_none_or(|u| ts > u) {
                        updated = Some(ts);
                    }
                }
                match v.get("type").and_then(Value::as_str) {
                    Some("custom-title") => {
                        if let Some(t) = v
                            .get("customTitle")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                        {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("ai-title") => {
                        if let Some(t) = v.get("aiTitle").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("agent-name") if title.is_none() => {
                        if let Some(t) = v.get("agentName").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("summary") if title.is_none() => {
                        if let Some(t) = v.get("summary").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                            title = Some(t.trim().to_string());
                        }
                    }
                    Some("last-prompt") => {
                        if let Some(lp) = v
                            .get("lastPrompt")
                            .and_then(Value::as_str)
                            .and_then(clean_prompt_snippet)
                        {
                            last_prompt = Some(lp);
                        }
                    }
                    Some("user") => {
                        sampled_msgs += 1;
                        if v.get("isMeta").and_then(Value::as_bool) != Some(true)
                            && v.get("isCompactSummary").and_then(Value::as_bool) != Some(true)
                        {
                            if let Some(raw) = extract_claude_message_text(&v) {
                                if !is_claude_local_cmd_wrapper(&raw) {
                                    if let Some(text) = clean_prompt_snippet(&raw) {
                                        if first_prompt.is_none() {
                                            first_prompt = Some(text.clone());
                                        }
                                        last_prompt = Some(text);
                                    }
                                }
                            }
                        }
                    }
                    Some("assistant") => {
                        sampled_msgs += 1;
                    }
                    _ => {}
                }
                inspect_claude_ctx(&v, &mut ctx_st);
                true
            };
            for line in &tail {
                if !inspect(line) {
                    return None;
                }
            }
        }
        // If a large trailing blob/snapshot pushed the last assistant usage or compact boundary
        // outside the default 128KB tail slice, expand the tail read so we never fall back to
        // stale turn-1 context from `head`.
        if !full && !ctx_st.saw_event {
            for &win_bytes in &TAIL_EXPAND_BYTES {
                if size <= TAIL_BYTES {
                    break;
                }
                let Some(expanded) = read_tail_lines(path, size, win_bytes) else {
                    break;
                };
                let mut probe = CtxParseState {
                    window: ctx_st.window,
                    explicit_window: ctx_st.explicit_window.clone(),
                    continued_in: ctx_st.continued_in.clone(),
                    ..Default::default()
                };
                for line in &expanded {
                    let Ok(v) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    inspect_claude_ctx(&v, &mut probe);
                }
                if probe.saw_event {
                    ctx_st = probe;
                    break;
                }
                if probe.continued_in.is_some() {
                    ctx_st.continued_in = probe.continued_in;
                }
                if win_bytes >= size {
                    break;
                }
            }
        }

        if sampled_msgs == 0 && title.is_none() && first_prompt.is_none() && last_prompt.is_none() {
            return None;
        }
        let messages = if full {
            sampled_msgs.max(1)
        } else {
            let ratio = (size as f64) / ((HEAD_BYTES + TAIL_BYTES) as f64);
            ((sampled_msgs as f64) * ratio).round().max(1.0) as u32
        };
        let project = cwd
            .as_deref()
            .and_then(|c| c.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next())
            .filter(|s| !s.is_empty())
            .unwrap_or("Claude")
            .to_string();
        let updated = updated.unwrap_or(mtime);
        let started = started.unwrap_or(updated);

        Some(SessionMeta {
            agent: "claude".into(),
            id: session_id,
            path: path.to_path_buf(),
            cwd,
            project,
            branch,
            title,
            first_prompt,
            last_prompt,
            started,
            updated,
            messages,
            ctx_used: ctx_st.used,
            ctx_window: ctx_st.window,
            ctx_reset: ctx_st.reset,
            continued_in: ctx_st.continued_in,
            model: ctx_st.model,
            last_speed_tps_x10: ctx_st.last_speed_tps_x10,
            last_output_tokens: ctx_st.last_output_tokens,
            last_turn_ms: ctx_st.last_turn_ms,
            quota: ctx_st.quota,
        })
    }
}

fn is_claude_local_cmd_wrapper(raw: &str) -> bool {
    let t = raw.trim_start();
    t.starts_with("<local-command-caveat>")
        || t.starts_with("<command-name>")
        || t.starts_with("<local-command-stdout>")
}

fn extract_claude_message_text(v: &Value) -> Option<String> {
    let msg = v.get("message")?;
    if let Some(s) = msg.get("content").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(arr) = msg.get("content").and_then(Value::as_array) {
        for item in arr {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    if !t.trim().is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
        }
    }
    None
}

// ============================================================================
// Codex Provider (`~/.codex/sessions/**/*.jsonl` + `archived_sessions/*.jsonl`)
// ============================================================================

#[derive(Default)]
pub struct CodexProvider {
    custom_home: Option<PathBuf>,
}

impl CodexProvider {
    pub fn with_home(home: PathBuf) -> Self {
        Self {
            custom_home: Some(home),
        }
    }

    fn codex_home(&self) -> Option<PathBuf> {
        if let Some(h) = &self.custom_home {
            return Some(h.clone());
        }
        if let Some(h) = std::env::var_os("CODEX_HOME").filter(|s| !s.is_empty()) {
            return Some(PathBuf::from(h));
        }
        Some(home_dir()?.join(".codex"))
    }

    fn thread_titles(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let Some(idx) = self.codex_home().map(|h| h.join("session_index.jsonl")) else {
            return map;
        };
        let Ok(body) = std::fs::read_to_string(idx) else {
            return map;
        };
        for line in body.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(id), Some(name)) = (
                v.get("id").and_then(Value::as_str),
                v.get("thread_name").and_then(Value::as_str),
            ) {
                if !id.is_empty() && !name.trim().is_empty() {
                    map.insert(id.to_string(), name.trim().to_string());
                }
            }
        }
        map
    }
}

fn collect_jsonl_recursive(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 5 || !dir.is_dir() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            collect_jsonl_recursive(&p, depth + 1, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

impl HistoryProvider for CodexProvider {
    fn agent(&self) -> &'static str {
        "codex"
    }

    fn roots(&self) -> Vec<PathBuf> {
        let Some(h) = self.codex_home() else {
            return Vec::new();
        };
        vec![h.join("sessions"), h.join("archived_sessions")]
    }

    fn list_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in self.roots() {
            collect_jsonl_recursive(&root, 0, &mut out);
        }
        out
    }

    fn parse_file(&self, path: &Path, mtime: i64, size: u64) -> Option<SessionMeta> {
        let (head, tail, full) = read_head_tail_lines(path, size)?;
        let titles = self.thread_titles();
        let mut session_id: Option<String> = None;
        let mut cwd: Option<String> = None;
        let mut branch: Option<String> = None;
        let mut first_prompt: Option<String> = None;
        let mut last_prompt: Option<String> = None;
        let mut started: Option<i64> = None;
        let mut updated: Option<i64> = None;
        let mut sampled_msgs: u32 = 0;
        let mut ctx_st = CtxParseState::default();

        let inspect_slice = |lines: &[String],
                             session_id: &mut Option<String>,
                             cwd: &mut Option<String>,
                             branch: &mut Option<String>,
                             first_prompt: &mut Option<String>,
                             last_prompt: &mut Option<String>,
                             started: &mut Option<i64>,
                             updated: &mut Option<i64>,
                             sampled_msgs: &mut u32,
                             ctx_st: &mut CtxParseState| {
            for line in lines {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(parse_iso_secs)
                {
                    if started.is_none_or(|s| ts < s) {
                        *started = Some(ts);
                    }
                    if updated.is_none_or(|u| ts > u) {
                        *updated = Some(ts);
                    }
                }
                match v.get("type").and_then(Value::as_str) {
                    Some("session_meta") => {
                        if let Some(p) = v.get("payload") {
                            if let Some(id) =
                                p.get("id").and_then(Value::as_str).filter(|s| !s.is_empty())
                            {
                                *session_id = Some(id.to_string());
                            }
                            if let Some(c) =
                                p.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty())
                            {
                                *cwd = Some(c.to_string());
                            }
                            if let Some(b) = p
                                .get("git")
                                .and_then(|g| g.get("branch"))
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                            {
                                *branch = Some(b.to_string());
                            }
                        }
                    }
                    Some("event_msg") => {
                        if let Some(p) = v.get("payload") {
                            if p.get("type").and_then(Value::as_str) == Some("user_message") {
                                *sampled_msgs += 1;
                                if let Some(m) = p
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .and_then(clean_prompt_snippet)
                                {
                                    if first_prompt.is_none() {
                                        *first_prompt = Some(m.clone());
                                    }
                                    *last_prompt = Some(m);
                                }
                            }
                        }
                    }
                    Some("response_item") => {
                        if let Some(p) = v.get("payload") {
                            let role = p.get("role").and_then(Value::as_str);
                            if matches!(role, Some("user" | "assistant")) {
                                *sampled_msgs += 1;
                            }
                            if role == Some("user") {
                                if let Some(arr) = p.get("content").and_then(Value::as_array) {
                                    for item in arr {
                                        if let Some(t) = item.get("text").and_then(Value::as_str) {
                                            let trimmed = t.trim_start();
                                            if trimmed.starts_with("<environment_context>")
                                                || trimmed.starts_with("# AGENTS.md")
                                                || trimmed.starts_with("<INSTRUCTIONS>")
                                            {
                                                continue;
                                            }
                                            if let Some(clean) = clean_prompt_snippet(trimmed) {
                                                if first_prompt.is_none() {
                                                    *first_prompt = Some(clean.clone());
                                                }
                                                *last_prompt = Some(clean);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        // Legacy flat header `{"id":"...","cwd":"..."}`
                        if session_id.is_none() {
                            *session_id = v
                                .get("id")
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string);
                        }
                        if cwd.is_none() {
                            *cwd = v
                                .get("cwd")
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string);
                        }
                    }
                    _ => {}
                }
                inspect_codex_ctx(&v, ctx_st);
            }
        };

        inspect_slice(
            &head,
            &mut session_id,
            &mut cwd,
            &mut branch,
            &mut first_prompt,
            &mut last_prompt,
            &mut started,
            &mut updated,
            &mut sampled_msgs,
            &mut ctx_st,
        );
        ctx_st.saw_event = false;
        inspect_slice(
            &tail,
            &mut session_id,
            &mut cwd,
            &mut branch,
            &mut first_prompt,
            &mut last_prompt,
            &mut started,
            &mut updated,
            &mut sampled_msgs,
            &mut ctx_st,
        );
        if !full && !ctx_st.saw_event {
            for &win_bytes in &TAIL_EXPAND_BYTES {
                if size <= TAIL_BYTES {
                    break;
                }
                let Some(expanded) = read_tail_lines(path, size, win_bytes) else {
                    break;
                };
                let mut probe = CtxParseState {
                    window: ctx_st.window,
                    ..Default::default()
                };
                for line in &expanded {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        inspect_codex_ctx(&v, &mut probe);
                    }
                }
                if probe.saw_event {
                    ctx_st = probe;
                    break;
                }
                if win_bytes >= size {
                    break;
                }
            }
        }

        let id = session_id.or_else(|| {
            let stem = path.file_stem()?.to_str()?;
            // rollout-2026-10-01T12-00-00-<uuid> -> extract trailing 36-char uuid if present
            if stem.len() >= 36 {
                let cand = &stem[stem.len() - 36..];
                if cand.chars().filter(|&c| c == '-').count() == 4 {
                    return Some(cand.to_string());
                }
            }
            Some(stem.to_string())
        })?;

        let title = titles.get(&id).cloned();
        if sampled_msgs == 0 && title.is_none() && first_prompt.is_none() {
            return None;
        }
        let messages = if full {
            sampled_msgs.max(1)
        } else {
            let ratio = (size as f64) / ((HEAD_BYTES + TAIL_BYTES) as f64);
            ((sampled_msgs as f64) * ratio).round().max(1.0) as u32
        };
        let project = cwd
            .as_deref()
            .and_then(|c| c.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next())
            .filter(|s| !s.is_empty())
            .unwrap_or("Codex")
            .to_string();
        let updated = updated.unwrap_or(mtime);
        let started = started.unwrap_or(updated);

        Some(SessionMeta {
            agent: "codex".into(),
            id,
            path: path.to_path_buf(),
            cwd,
            project,
            branch,
            title,
            first_prompt,
            last_prompt,
            started,
            updated,
            messages,
            ctx_used: ctx_st.used,
            ctx_window: ctx_st.window,
            ctx_reset: ctx_st.reset,
            continued_in: None,
            model: ctx_st.model,
            last_speed_tps_x10: ctx_st.last_speed_tps_x10,
            last_output_tokens: ctx_st.last_output_tokens,
            last_turn_ms: ctx_st.last_turn_ms,
            quota: ctx_st.quota,
        })
    }
}

// ============================================================================
// Antigravity CLI Provider (`~/.gemini/antigravity-cli/conversation_summaries.db`)
// ============================================================================

#[derive(Default)]
pub struct AntigravityProvider {
    custom_home: Option<PathBuf>,
}

impl AntigravityProvider {
    pub fn with_home(home: PathBuf) -> Self {
        Self {
            custom_home: Some(home),
        }
    }

    fn agy_home(&self) -> Option<PathBuf> {
        if let Some(h) = &self.custom_home {
            return Some(h.clone());
        }
        if let Some(h) = std::env::var_os("ANTIGRAVITY_CLI_HOME").filter(|s| !s.is_empty()) {
            return Some(PathBuf::from(h));
        }
        Some(home_dir()?.join(".gemini").join("antigravity-cli"))
    }
}

impl HistoryProvider for AntigravityProvider {
    fn agent(&self) -> &'static str {
        "agy"
    }

    fn roots(&self) -> Vec<PathBuf> {
        self.agy_home().into_iter().collect()
    }

    fn list_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn parse_file(&self, _path: &Path, _mtime: i64, _size: u64) -> Option<SessionMeta> {
        None
    }

    fn extra_sessions(&self) -> Vec<SessionMeta> {
        let Some(home) = self.agy_home() else {
            return Vec::new();
        };
        let db_path = home.join("conversation_summaries.db");
        let mut out = if db_path.is_file() {
            read_antigravity_summaries_db(&home, &db_path)
        } else {
            Vec::new()
        };
        if out.is_empty() {
            let meta_path = home.join("cache").join("conversation_metadata.json");
            if meta_path.is_file() {
                out = read_antigravity_metadata_cache(&home, &meta_path);
            }
        }
        out
    }
}

fn decode_workspace_uri(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let path_raw = trimmed.strip_prefix("file://").unwrap_or(trimmed);
    if path_raw.is_empty() {
        return None;
    }
    let bytes = path_raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex_str, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    let final_path = if cfg!(windows) && decoded.starts_with('/') && decoded.as_bytes().get(2) == Some(&b':') {
        decoded[1..].to_string()
    } else {
        decoded
    };
    (!final_path.is_empty()).then_some(final_path)
}

fn first_workspace_from_json_array(raw: &str) -> Option<String> {
    let arr = serde_json::from_str::<Vec<String>>(raw).ok()?;
    arr.first().and_then(|u| decode_workspace_uri(u))
}

struct AgyRawSummary<'a> {
    id: String,
    title: String,
    preview: String,
    step_count: i64,
    cwd: Option<String>,
    updated_raw: &'a str,
    started_raw: &'a str,
}

fn agy_model_context_window(model: &str) -> u64 {
    let ml = model.to_ascii_lowercase();
    if ml.contains("[2m]") || ml.contains("2m") {
        2_000_000
    } else if ml.contains("claude") {
        claude_model_context_window(model)
    } else if ml.contains("gpt-4o") || ml.contains("o1") || ml.contains("o3") {
        200_000
    } else {
        // Antigravity Gemini models default to a 1M context window.
        1_000_000
    }
}

fn agy_transcript_path(home: &Path, id: &str) -> PathBuf {
    home.join("brain")
        .join(id)
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl")
}

/// Telemetry parsed from an Antigravity CLI `transcript.jsonl` tail.
#[derive(Debug, Clone, Default)]
struct AgyTranscriptTelemetry {
    ctx_used: Option<u64>,
    ctx_window: Option<u64>,
    ctx_reset: bool,
    model: Option<String>,
    last_speed_tps_x10: Option<u32>,
    last_output_tokens: Option<u64>,
    last_turn_ms: Option<u64>,
}

/// Read default model from `~/.gemini/antigravity-cli/settings.json` (cached by mtime/size)
/// so fresh Antigravity sessions before turn 1 still display the configured model.
pub fn load_antigravity_settings_model() -> Option<String> {
    use std::sync::{Mutex, OnceLock};
    type Cache = (Option<(i64, u64)>, Option<String>);
    static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();

    let home = std::env::var_os("ANTIGRAVITY_CLI_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".gemini").join("antigravity-cli")))?;
    let path = home.join("settings.json");
    let stamp = file_stamp(&path);

    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((cs, m)) = g.as_ref() {
            if *cs == stamp {
                return m.clone();
            }
        }
    }

    let model = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("model")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
    if let Ok(mut g) = cache.lock() {
        *g = Some((stamp, model.clone()));
    }
    model
}

fn pick_antigravity_group_idx(groups: &[QuotaGroup], active_model: Option<&str>) -> usize {
    if groups.len() <= 1 {
        return 0;
    }
    let ml = active_model.unwrap_or("").to_ascii_lowercase();
    let want_3p = ml.contains("claude") || ml.contains("gpt") || ml.contains("opus") || ml.contains("sonnet");
    groups
        .iter()
        .position(|g| {
            let gl = g.name.to_ascii_lowercase();
            if want_3p {
                gl.contains("claude") || gl.contains("gpt")
            } else {
                gl.contains("gemini")
            }
        })
        .unwrap_or(0)
}

/// Re-select `primary` (5h) and `secondary` (7d) from `q.groups` for `active_model`.
pub fn select_antigravity_quota_for_model(
    q: &SubscriptionQuota,
    active_model: Option<&str>,
) -> SubscriptionQuota {
    if q.groups.is_empty() {
        return q.clone();
    }
    let idx = pick_antigravity_group_idx(&q.groups, active_model);
    let grp = &q.groups[idx];
    let primary = grp
        .windows
        .iter()
        .find(|w| w.label == "5h")
        .or_else(|| grp.windows.first())
        .cloned()
        .unwrap_or_else(|| q.primary.clone());
    let secondary = grp
        .windows
        .iter()
        .find(|w| w.label != primary.label)
        .cloned();
    SubscriptionQuota {
        plan: Some(grp.name.clone()),
        primary,
        secondary,
        groups: q.groups.clone(),
        updated_at: q.updated_at,
    }
}

/// Parse the structured JSON output of `agy -p "/usage" --output-format json`.
pub fn parse_antigravity_usage_json(
    raw: &str,
    active_model: Option<&str>,
) -> Option<SubscriptionQuota> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    let arr = v
        .pointer("/command/data/groups")
        .and_then(Value::as_array)?;
    let mut groups = Vec::new();
    for g in arr {
        let name = g
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Models")
            .to_string();
        let Some(buckets) = g.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        let mut windows = Vec::new();
        for b in buckets {
            let Some(rem) = b.get("remaining_fraction").and_then(Value::as_f64) else {
                continue;
            };
            if !rem.is_finite() {
                continue;
            }
            let used_pct = ((1.0 - rem.clamp(0.0, 1.0)) * 100.0).round().clamp(0.0, 100.0) as u8;
            let raw_win = b
                .get("window")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let label = match raw_win.as_str() {
                "5h" | "five_hour" => "5h".to_string(),
                "weekly" | "7d" => "7d".to_string(),
                other if !other.is_empty() => other.to_string(),
                _ => b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("quota")
                    .to_string(),
            };
            let resets_at = b
                .get("reset_time")
                .and_then(Value::as_str)
                .and_then(parse_iso_secs);
            windows.push(QuotaWindow {
                label,
                used_pct,
                resets_at,
            });
        }
        // Order 5h first, then 7d/weekly so callers have a predictable layout.
        windows.sort_by_key(|w| match w.label.as_str() {
            "5h" => 0,
            "7d" => 1,
            _ => 2,
        });
        if !windows.is_empty() {
            groups.push(QuotaGroup { name, windows });
        }
    }
    if groups.is_empty() {
        return None;
    }
    let idx = pick_antigravity_group_idx(&groups, active_model);
    let grp = &groups[idx];
    let primary = grp
        .windows
        .iter()
        .find(|w| w.label == "5h")
        .or_else(|| grp.windows.first())?
        .clone();
    let secondary = grp
        .windows
        .iter()
        .find(|w| w.label != primary.label)
        .cloned();
    Some(SubscriptionQuota {
        plan: Some(grp.name.clone()),
        primary,
        secondary,
        groups,
        updated_at: now_unix_secs(),
    })
}

fn find_agy_binary() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(h) = home_dir() {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".cargo/bin"));
        dirs.push(h.join(".npm-global/bin"));
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    for d in dirs {
        let p = d.join("agy");
        if p.is_file() {
            return Some(p);
        }
        #[cfg(windows)]
        for ext in ["exe", "cmd", "bat"] {
            let pw = d.join(format!("agy.{ext}"));
            if pw.is_file() {
                return Some(pw);
            }
        }
    }
    None
}

/// Query live Antigravity subscription quota via the documented read-only print-mode slash command
/// `agy -p "/usage" --output-format json` (zero turns, zero quota spend, no conversation created).
pub fn fetch_antigravity_quota(active_model: Option<&str>) -> Option<SubscriptionQuota> {
    use std::process::{Command, Stdio};
    let bin = find_agy_binary()?;
    let mut cmd = Command::new(bin);
    cmd.args(["-p", "/usage", "--output-format", "json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_antigravity_usage_json(&text, active_model)
}

/// Parse context usage, active model, and turn speed from an Antigravity CLI transcript.
/// Cached by `(mtime, size)` so repeated history scans incur zero redundant I/O.
fn parse_antigravity_transcript_telemetry(path: &Path) -> AgyTranscriptTelemetry {
    use std::sync::{Mutex, OnceLock};
    type Entry = (i64, u64, AgyTranscriptTelemetry);
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();

    let Ok(md) = std::fs::metadata(path) else {
        return AgyTranscriptTelemetry::default();
    };
    let size = md.len();
    if size == 0 {
        return AgyTranscriptTelemetry::default();
    }
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((cm, cs, tel)) = g.get(path) {
            if *cm == mtime && *cs == size {
                return tel.clone();
            }
        }
    }

    let mut out = AgyTranscriptTelemetry::default();

    for &win_bytes in &[TAIL_BYTES, TAIL_EXPAND_BYTES[0], TAIL_EXPAND_BYTES[1]] {
        let Some(lines) = read_tail_lines(path, size, win_bytes) else {
            break;
        };
        let mut saw = false;
        let mut u_cur: Option<u64> = None;
        let mut w_cur: Option<u64> = None;
        let mut r_cur = false;
        let mut m_cur: Option<String> = None;

        // Track latest PLANNER_RESPONSE with output_tokens > 0 and its following step timestamp.
        let mut pending_pr: Option<(i64, u64, bool)> = None; // (created_ms, out_tok, is_done)
        let mut best_speed: Option<(u32, u64, u64)> = None; // (tps_x10, out_tok, elapsed_ms)

        for line in &lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let step_ts_ms = v
                .get("created_at")
                .or_else(|| v.get("timestamp"))
                .and_then(Value::as_str)
                .and_then(parse_iso_ms);

            // If we have a pending PLANNER_RESPONSE and this subsequent step has a later timestamp,
            // compute throughput for that planner step.
            if let (Some((pr_ms, pr_out, _)), Some(next_ms)) = (pending_pr, step_ts_ms) {
                if next_ms > pr_ms {
                    let dt_ms = (next_ms - pr_ms) as u64;
                    if let Some(tps_x10) = compute_tps_x10(pr_out, dt_ms) {
                        best_speed = Some((tps_x10, pr_out, dt_ms));
                    }
                    pending_pr = None;
                }
            }

            match v.get("type").and_then(Value::as_str) {
                Some("PLANNER_RESPONSE") => {
                    if let Some(m) = v
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        m_cur = Some(m.to_string());
                        w_cur = Some(agy_model_context_window(m));
                    }
                    let input = v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let cache_read = v
                        .get("cache_read_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let sum = input.saturating_add(cache_read);
                    if sum > 0 {
                        u_cur = Some(sum);
                        r_cur = false;
                        saw = true;
                    }
                    let out_tok = v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                    if out_tok > 0 {
                        if let Some(ts_ms) = step_ts_ms {
                            let is_done = v
                                .get("status")
                                .and_then(Value::as_str)
                                .is_none_or(|s| s.eq_ignore_ascii_case("DONE"));
                            pending_pr = Some((ts_ms, out_tok, is_done));
                        }
                    }
                }
                Some("CHECKPOINT") => {
                    // Compaction checkpoint: discard pre-compact prompt tokens and keep the
                    // compacted summary size (`output_tokens`) until the next planner turn.
                    let out = v
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .filter(|&n| n > 0);
                    u_cur = out;
                    r_cur = true;
                    saw = true;
                    pending_pr = None;
                }
                _ => {}
            }
        }

        // Fallback for the final PLANNER_RESPONSE at the end of a turn (no subsequent GENERIC step):
        // use transcript file mtime when the step is marked DONE.
        if let Some((pr_ms, pr_out, true)) = pending_pr {
            let mtime_ms = mtime.saturating_mul(1000);
            if mtime_ms > pr_ms {
                let dt_ms = (mtime_ms - pr_ms) as u64;
                if let Some(tps_x10) = compute_tps_x10(pr_out, dt_ms) {
                    best_speed = Some((tps_x10, pr_out, dt_ms));
                }
            }
        }

        if saw || win_bytes >= size {
            out.ctx_used = u_cur;
            out.ctx_window = w_cur;
            out.ctx_reset = r_cur;
            out.model = m_cur;
            if let Some((tps_x10, out_tok, dt_ms)) = best_speed {
                out.last_speed_tps_x10 = Some(tps_x10);
                out.last_output_tokens = Some(out_tok);
                out.last_turn_ms = Some(dt_ms);
            }
            break;
        }
    }

    if let Ok(mut g) = cache.lock() {
        if g.len() > 1024 {
            g.clear();
        }
        g.insert(path.to_path_buf(), (mtime, size, out.clone()));
    }
    out
}

fn build_antigravity_meta(
    home: &Path,
    fallback_path: &Path,
    raw: AgyRawSummary<'_>,
) -> Option<SessionMeta> {
    let AgyRawSummary {
        id,
        title,
        preview,
        step_count,
        cwd,
        updated_raw,
        started_raw,
    } = raw;
    if id.is_empty() {
        return None;
    }
    let title_clean = clean_prompt_snippet(&title);
    let preview_clean = clean_prompt_snippet(&preview);
    if title_clean.is_none() && preview_clean.is_none() && step_count <= 0 {
        return None;
    }
    let transcript = agy_transcript_path(home, &id);
    let tel = if transcript.is_file() {
        parse_antigravity_transcript_telemetry(&transcript)
    } else {
        AgyTranscriptTelemetry::default()
    };
    let pb_path = home.join("conversations").join(format!("{id}.pb"));
    let path = if transcript.is_file() {
        transcript
    } else if pb_path.exists() {
        pb_path
    } else {
        fallback_path.to_path_buf()
    };
    let updated = parse_iso_secs(updated_raw).unwrap_or_else(now_unix_secs);
    let started = parse_iso_secs(started_raw).unwrap_or(updated);
    let project = cwd
        .as_deref()
        .and_then(|c| c.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next())
        .filter(|s| !s.is_empty())
        .unwrap_or("Antigravity")
        .to_string();
    Some(SessionMeta {
        agent: "agy".into(),
        id,
        path,
        cwd,
        project,
        branch: None,
        title: title_clean,
        first_prompt: preview_clean.clone(),
        last_prompt: preview_clean,
        started,
        updated,
        messages: (step_count.max(1)) as u32,
        ctx_used: tel.ctx_used,
        ctx_window: tel.ctx_window,
        ctx_reset: tel.ctx_reset,
        continued_in: None,
        model: tel.model,
        last_speed_tps_x10: tel.last_speed_tps_x10,
        last_output_tokens: tel.last_output_tokens,
        last_turn_ms: tel.last_turn_ms,
        quota: None,
    })
}

fn read_antigravity_summaries_db(home: &Path, db_path: &Path) -> Vec<SessionMeta> {
    struct DbGuard(*mut libsqlite3_sys::sqlite3);
    impl Drop for DbGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    libsqlite3_sys::sqlite3_close(self.0);
                }
            }
        }
    }
    struct StmtGuard(*mut libsqlite3_sys::sqlite3_stmt);
    impl Drop for StmtGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    libsqlite3_sys::sqlite3_finalize(self.0);
                }
            }
        }
    }

    let Ok(cpath) = std::ffi::CString::new(db_path.to_string_lossy().as_bytes()) else {
        return Vec::new();
    };
    let mut db: *mut libsqlite3_sys::sqlite3 = std::ptr::null_mut();
    let rc = unsafe {
        libsqlite3_sys::sqlite3_open_v2(
            cpath.as_ptr(),
            &mut db,
            libsqlite3_sys::SQLITE_OPEN_READONLY | libsqlite3_sys::SQLITE_OPEN_NOMUTEX,
            std::ptr::null(),
        )
    };
    let _db_guard = DbGuard(db);
    if rc != libsqlite3_sys::SQLITE_OK || db.is_null() {
        return Vec::new();
    }
    unsafe {
        libsqlite3_sys::sqlite3_busy_timeout(db, 150);
    }

    let sql = c"SELECT conversation_id, title, preview, step_count, workspace_uris, last_modified_time, last_user_input_time FROM conversation_summaries WHERE app_data_dir = 'antigravity-cli' AND (step_count > 0 OR title != '' OR preview != '') ORDER BY last_modified_time DESC LIMIT 500";
    let mut stmt: *mut libsqlite3_sys::sqlite3_stmt = std::ptr::null_mut();
    let prep = unsafe {
        libsqlite3_sys::sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut())
    };
    let _stmt_guard = StmtGuard(stmt);
    if prep != libsqlite3_sys::SQLITE_OK || stmt.is_null() {
        return Vec::new();
    }

    let col_str = |stmt: *mut libsqlite3_sys::sqlite3_stmt, col: i32| -> String {
        unsafe {
            let ptr = libsqlite3_sys::sqlite3_column_text(stmt, col);
            if ptr.is_null() {
                return String::new();
            }
            let len = libsqlite3_sys::sqlite3_column_bytes(stmt, col).max(0) as usize;
            let slice = std::slice::from_raw_parts(ptr, len);
            String::from_utf8_lossy(slice).into_owned()
        }
    };

    let mut out = Vec::new();
    loop {
        let step = unsafe { libsqlite3_sys::sqlite3_step(stmt) };
        if step != libsqlite3_sys::SQLITE_ROW {
            break;
        }
        let id = col_str(stmt, 0);
        let title = col_str(stmt, 1);
        let preview = col_str(stmt, 2);
        let steps = unsafe { libsqlite3_sys::sqlite3_column_int64(stmt, 3) };
        let workspaces_json = col_str(stmt, 4);
        let updated_raw = col_str(stmt, 5);
        let started_raw = col_str(stmt, 6);
        let cwd = first_workspace_from_json_array(&workspaces_json);
        if let Some(meta) = build_antigravity_meta(
            home,
            db_path,
            AgyRawSummary {
                id,
                title,
                preview,
                step_count: steps,
                cwd,
                updated_raw: &updated_raw,
                started_raw: &started_raw,
            },
        ) {
            out.push(meta);
        }
    }
    out
}

fn read_antigravity_metadata_cache(home: &Path, cache_path: &Path) -> Vec<SessionMeta> {
    let Ok(bytes) = std::fs::read(cache_path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    let Some(convs) = v.get("conversations").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, entry) in convs {
        let Some(sum) = entry.get("summary") else {
            continue;
        };
        if sum.get("AppDataDir").and_then(Value::as_str) != Some("antigravity-cli") {
            continue;
        }
        if sum.get("Internal").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let title = sum.get("Title").and_then(Value::as_str).unwrap_or("");
        let preview = sum.get("Preview").and_then(Value::as_str).unwrap_or("");
        let steps = sum.get("NumSteps").and_then(Value::as_i64).unwrap_or(0);
        let updated = sum.get("UpdatedAt").and_then(Value::as_str).unwrap_or("");
        let cwd = sum
            .get("WorkspaceURIs")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(Value::as_str)
            .and_then(decode_workspace_uri);
        if let Some(meta) = build_antigravity_meta(
            home,
            cache_path,
            AgyRawSummary {
                id: id.clone(),
                title: title.to_string(),
                preview: preview.to_string(),
                step_count: steps,
                cwd,
                updated_raw: updated,
                started_raw: updated,
            },
        ) {
            out.push(meta);
        }
    }
    out
}

/// Recent prompt entries `(conversation_id, prompt_display, workspace_opt, timestamp_secs)` from
/// Antigravity CLI's `history.jsonl` (tail slice) for live pane-to-conversation association.
pub fn lookup_antigravity_history_tail() -> Vec<(String, String, Option<String>, i64)> {
    let Some(home) = std::env::var_os("ANTIGRAVITY_CLI_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".gemini").join("antigravity-cli")))
    else {
        return Vec::new();
    };
    let hist_path = home.join("history.jsonl");
    let Ok(md) = std::fs::metadata(&hist_path) else {
        return Vec::new();
    };
    let size = md.len();
    if size == 0 {
        return Vec::new();
    }
    let Ok(mut f) = File::open(&hist_path) else {
        return Vec::new();
    };
    let tail_start = size.saturating_sub(64 * 1024);
    if f.seek(SeekFrom::Start(tail_start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let lines = split_lines_lossy(&buf, tail_start > 0);
    let mut out = Vec::new();
    for line in lines.into_iter().rev().take(120) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(cid) = v
            .get("conversationId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let display = v
            .get("display")
            .and_then(Value::as_str)
            .and_then(clean_prompt_snippet)
            .unwrap_or_default();
        if display.is_empty() {
            continue;
        }
        let ws = v
            .get("workspace")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let ts_ms = v.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        let ts_secs = if ts_ms > 10_000_000_000 {
            ts_ms / 1000
        } else {
            ts_ms
        };
        out.push((cid.to_string(), display, ws, ts_secs));
    }
    out
}

/// Return `(mtime_secs, size_bytes)` for a transcript file on disk, if readable.
pub fn file_stamp(path: &Path) -> Option<(i64, u64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((mtime, md.len()))
}

/// Fast single-session refresh for an active live pane without scanning all history directories.
pub fn refresh_single_session(
    agent: &str,
    path: &Path,
    existing: Option<&SessionMeta>,
) -> Option<SessionMeta> {
    let (mtime, size) = file_stamp(path)?;
    if size == 0 {
        return None;
    }
    let aid = crate::agent::normalize_id(agent);
    match aid.as_str() {
        "claude" => {
            let mut m = ClaudeProvider::default().parse_file(path, mtime, size)?;
            if let Some(cwd) = m.cwd.as_deref() {
                m.project = resolve_project_name_for_dir(cwd);
            }
            Some(m)
        }
        "codex" => {
            let mut m = CodexProvider::default().parse_file(path, mtime, size)?;
            if let Some(cwd) = m.cwd.as_deref() {
                m.project = resolve_project_name_for_dir(cwd);
            }
            Some(m)
        }
        "agy" => {
            let mut m = existing?.clone();
            let tel = parse_antigravity_transcript_telemetry(path);
            m.ctx_used = tel.ctx_used;
            m.ctx_window = tel.ctx_window;
            m.ctx_reset = tel.ctx_reset;
            if tel.model.is_some() {
                m.model = tel.model;
            }
            if tel.last_speed_tps_x10.is_some() {
                m.last_speed_tps_x10 = tel.last_speed_tps_x10;
                m.last_output_tokens = tel.last_output_tokens;
                m.last_turn_ms = tel.last_turn_ms;
            }
            m.updated = m.updated.max(mtime);
            Some(m)
        }
        _ => None,
    }
}

/// Summarize local token usage over the rolling 5-hour and 24-hour windows for `agent`.
/// Cached for 30s per agent so bottom-bar renders stay O(1).
pub fn summarize_recent_usage(agent: &str, now_secs: i64) -> Option<LocalUsageSummary> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, (i64, LocalUsageSummary)>>> = OnceLock::new();

    let aid = crate::agent::normalize_id(agent);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((at, sum)) = g.get(&aid) {
            if now_secs.saturating_sub(*at) < 30 {
                return (sum.win_24h.turns > 0).then(|| sum.clone());
            }
        }
    }

    let files: Vec<PathBuf> = match aid.as_str() {
        "agy" => {
            let Some(home) = AntigravityProvider::default().agy_home() else {
                return None;
            };
            let brain = home.join("brain");
            let Ok(rd) = std::fs::read_dir(&brain) else {
                return None;
            };
            rd.flatten()
                .map(|e| {
                    e.path()
                        .join(".system_generated")
                        .join("logs")
                        .join("transcript.jsonl")
                })
                .filter(|p| p.is_file())
                .collect()
        }
        "claude" => ClaudeProvider::default().list_files(),
        "codex" => CodexProvider::default().list_files(),
        _ => return None,
    };

    let sum = summarize_usage_files(&aid, &files, now_secs);
    if let Ok(mut g) = cache.lock() {
        g.insert(aid, (now_secs, sum.clone()));
    }
    (sum.win_24h.turns > 0).then_some(sum)
}

fn summarize_usage_files(agent: &str, files: &[PathBuf], now_secs: i64) -> LocalUsageSummary {
    let cutoff_5h = now_secs.saturating_sub(5 * 3600);
    let cutoff_24h = now_secs.saturating_sub(24 * 3600);
    let mut sum = LocalUsageSummary::default();

    for path in files {
        let Some((mtime, size)) = file_stamp(path) else {
            continue;
        };
        if size == 0 || mtime < cutoff_24h {
            continue;
        }
        // Bounded tail read (512 KB) keeps aggregation fast even for huge sessions.
        let Some(lines) = read_tail_lines(path, size, 512 * 1024) else {
            continue;
        };
        let mut hit_5h = false;
        let mut hit_24h = false;

        for line in &lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let (ts_opt, inp, cache_r, out) = match agent {
                "agy" => {
                    if v.get("type").and_then(Value::as_str) != Some("PLANNER_RESPONSE") {
                        continue;
                    }
                    let ts = v
                        .get("created_at")
                        .or_else(|| v.get("timestamp"))
                        .and_then(Value::as_str)
                        .and_then(parse_iso_secs);
                    let i = v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let c = v
                        .get("cache_read_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let o = v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                    (ts, i, c, o)
                }
                "claude" => {
                    if v.get("type").and_then(Value::as_str) != Some("assistant") {
                        continue;
                    }
                    let Some(msg) = v.get("message") else {
                        continue;
                    };
                    if msg.get("model").and_then(Value::as_str) == Some("<synthetic>") {
                        continue;
                    }
                    let Some(u) = msg.get("usage").or_else(|| v.get("usage")) else {
                        continue;
                    };
                    let ts = v
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_iso_secs);
                    let i = u
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        .saturating_add(
                            u.get("cache_creation_input_tokens")
                                .and_then(Value::as_u64)
                                .unwrap_or(0),
                        );
                    let c = u
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let o = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                    (ts, i, c, o)
                }
                "codex" => {
                    if v.get("type").and_then(Value::as_str) != Some("event_msg") {
                        continue;
                    }
                    let Some(p) = v.get("payload") else {
                        continue;
                    };
                    if p.get("type").and_then(Value::as_str) != Some("token_count") {
                        continue;
                    }
                    let Some(u) = p
                        .get("info")
                        .and_then(|inf| inf.get("last_token_usage"))
                    else {
                        continue;
                    };
                    let ts = v
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .and_then(parse_iso_secs);
                    let i = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let c = u
                        .get("cached_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let o = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
                    (ts, i, c, o)
                }
                _ => continue,
            };
            if inp == 0 && cache_r == 0 && out == 0 {
                continue;
            }
            let ts = ts_opt.unwrap_or(mtime);
            if ts >= cutoff_24h {
                sum.win_24h.input_tokens = sum.win_24h.input_tokens.saturating_add(inp);
                sum.win_24h.cache_read_tokens = sum.win_24h.cache_read_tokens.saturating_add(cache_r);
                sum.win_24h.output_tokens = sum.win_24h.output_tokens.saturating_add(out);
                sum.win_24h.turns = sum.win_24h.turns.saturating_add(1);
                hit_24h = true;
            }
            if ts >= cutoff_5h {
                sum.win_5h.input_tokens = sum.win_5h.input_tokens.saturating_add(inp);
                sum.win_5h.cache_read_tokens = sum.win_5h.cache_read_tokens.saturating_add(cache_r);
                sum.win_5h.output_tokens = sum.win_5h.output_tokens.saturating_add(out);
                sum.win_5h.turns = sum.win_5h.turns.saturating_add(1);
                hit_5h = true;
            }
        }
        if hit_5h {
            sum.win_5h.sessions = sum.win_5h.sessions.saturating_add(1);
        }
        if hit_24h {
            sum.win_24h.sessions = sum.win_24h.sessions.saturating_add(1);
        }
    }
    sum
}

/// Semantic category inside a session's context window (for segmented bar & breakdown popover).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCategoryKind {
    SystemPrompt,
    ToolDefinitions,
    Rules,
    Skills,
    McpTools,
    Subagents,
    Conversation,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCategory {
    pub kind: ContextCategoryKind,
    /// Optional concise qualifier (`"15 tools"`, `"2 files"`, `"21 skills"`, `"12 turns · tool I/O ~180K"`).
    pub detail: Option<String>,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBreakdown {
    pub agent: String,
    pub model: Option<String>,
    pub used_tokens: u64,
    pub window_tokens: u64,
    pub is_estimated: bool,
    pub cleared: bool,
    pub compacted_summary_only: bool,
    pub categories: Vec<ContextCategory>,
    pub autocompact_buffer: Option<u64>,
}

/// Fast CJK-aware token estimator (`~3.5` ASCII bytes/tok, `~1.25` tokens per non-ASCII char).
pub fn estimate_text_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let mut ascii = 0u64;
    let mut non_ascii_chars = 0u64;
    for &b in text.as_bytes() {
        if b < 0x80 {
            ascii += 1;
        } else if (b & 0xC0) != 0x80 {
            non_ascii_chars += 1;
        }
    }
    ((ascii as f64 / 3.5) + (non_ascii_chars as f64 * 1.25)).ceil() as u64
}

fn estimate_json_tokens(v: &Value) -> u64 {
    match v {
        Value::Null => 0,
        Value::String(s) => estimate_text_tokens(s),
        _ => serde_json::to_string(v)
            .map(|s| estimate_text_tokens(&s))
            .unwrap_or(0),
    }
}

fn calibrate_breakdown_categories(
    fixed: Vec<(ContextCategoryKind, Option<String>, u64)>,
    conv_raw: u64,
    conv_detail: Option<String>,
    used_tokens: u64,
) -> Vec<ContextCategory> {
    if used_tokens == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let fixed_sum: u64 = fixed.iter().map(|(_, _, t)| *t).sum();
    if fixed_sum == 0 {
        out.push(ContextCategory {
            kind: ContextCategoryKind::Conversation,
            detail: conv_detail,
            tokens: used_tokens,
        });
        return out;
    }
    if fixed_sum <= used_tokens {
        for (kind, detail, tokens) in fixed {
            if tokens > 0 {
                out.push(ContextCategory {
                    kind,
                    detail,
                    tokens,
                });
            }
        }
        let conv_tokens = used_tokens.saturating_sub(fixed_sum);
        if conv_tokens > 0 {
            out.push(ContextCategory {
                kind: ContextCategoryKind::Conversation,
                detail: conv_detail,
                tokens: conv_tokens,
            });
        }
        return out;
    }

    // If raw estimation slightly overshoots `used_tokens` (e.g. turn 1 or right after compaction),
    // proportionally scale all non-zero buckets so the sum equals `used_tokens` exactly.
    let total_raw = fixed_sum.saturating_add(conv_raw).max(1);
    let mut allocated = 0u64;
    let mut items: Vec<(ContextCategoryKind, Option<String>, u64)> = fixed
        .into_iter()
        .filter(|(_, _, t)| *t > 0)
        .collect();
    if conv_raw > 0 {
        items.push((ContextCategoryKind::Conversation, conv_detail, conv_raw));
    }
    let last_idx = items.len().saturating_sub(1);
    for (idx, (kind, detail, raw)) in items.into_iter().enumerate() {
        let tok = if idx == last_idx {
            used_tokens.saturating_sub(allocated)
        } else {
            let scaled = ((raw as f64 / total_raw as f64) * used_tokens as f64).round() as u64;
            scaled.min(used_tokens.saturating_sub(allocated))
        };
        allocated = allocated.saturating_add(tok);
        if tok > 0 {
            out.push(ContextCategory {
                kind,
                detail,
                tokens: tok,
            });
        }
    }
    out
}

fn format_conv_detail(turns: u32, tool_io_share: u64) -> Option<String> {
    let mut parts = Vec::new();
    if turns > 0 {
        parts.push(crate::i18n::tf("ag.ctx_turns_n", &[("n", &turns)]));
    }
    if tool_io_share >= 1_000 {
        let tok_s = crate::agent::format_tokens_precise(tool_io_share);
        parts.push(crate::i18n::tf("ag.ctx_tool_io", &[("t", &tok_s)]));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn normalize_window_for_used(used: u64, win_opt: Option<u64>) -> u64 {
    let mut win = win_opt.unwrap_or(1_000_000).max(1);
    while used > win {
        win = if win < 1_000_000 {
            1_000_000
        } else {
            win.saturating_mul(2)
        };
    }
    win
}

/// On-demand, `(mtime, size)`-cached detailed context breakdown for one session transcript.
/// Never called during background list scans so idle/scroll performance stays O(1).
pub fn analyze_context_breakdown(agent: &str, path: &Path) -> Option<ContextBreakdown> {
    use std::sync::{Mutex, OnceLock};
    type Entry = (i64, u64, Option<ContextBreakdown>);
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();

    let aid = crate::agent::normalize_id(agent);
    let effective_path = if aid == "agy" && path.file_name().and_then(|n| n.to_str()) == Some("transcript.jsonl") {
        let full = path.with_file_name("transcript_full.jsonl");
        if full.is_file() {
            full
        } else {
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    };

    let (mtime, size) = file_stamp(&effective_path)?;
    if size == 0 {
        return None;
    }

    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((cm, cs, res)) = g.get(&effective_path) {
            if *cm == mtime && *cs == size {
                return res.clone();
            }
        }
    }

    let res = match aid.as_str() {
        "claude" => parse_claude_context_breakdown(&effective_path),
        "agy" => parse_agy_context_breakdown(&effective_path),
        "codex" => parse_codex_context_breakdown(&effective_path),
        _ => None,
    };

    if let Ok(mut g) = cache.lock() {
        if g.len() > 64 {
            g.clear();
        }
        g.insert(effective_path, (mtime, size, res.clone()));
    }
    res
}

fn parse_claude_context_breakdown(path: &Path) -> Option<ContextBreakdown> {
    let f = File::open(path).ok()?;
    let reader = BufReader::with_capacity(64 * 1024, f);

    #[derive(Default, Clone, Copy)]
    struct ShapeCounts {
        system: u64,
        builtin_tools: u64,
        builtin_count: usize,
        mcp_tools: u64,
        mcp_count: usize,
    }

    let mut shapes: HashMap<String, ShapeCounts> = HashMap::new();
    let mut last_shape_hash: Option<String> = None;
    let mut instructions: HashMap<String, u64> = HashMap::new();
    let mut skills_toks = 0u64;
    let mut skills_count = 0usize;
    let mut subagents_toks = 0u64;
    let mut deferred_mcp_toks = 0u64;
    let mut env_att_toks: HashMap<String, u64> = HashMap::new();

    let mut conv_user_text = 0u64;
    let mut conv_asst_text = 0u64;
    let mut conv_tool_io = 0u64;
    let mut conv_att = 0u64;
    let mut turns = 0u32;

    let mut last_model: Option<String> = None;
    let mut last_window: Option<u64> = None;
    let mut explicit_window: Option<(Option<String>, u64)> = None;
    let mut last_used: Option<u64> = None;
    let mut cleared = false;
    let mut compacted_only = false;

    for line_res in reader.lines() {
        let Ok(line) = line_res else {
            break;
        };
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        match v.get("type").and_then(Value::as_str) {
            Some("api-request-shape") => {
                let Some(hash) = v.get("shapeHash").and_then(Value::as_str) else {
                    continue;
                };
                if shapes.contains_key(hash) {
                    continue;
                }
                let Some(sh) = v.get("shape") else {
                    continue;
                };
                let mut sc = ShapeCounts::default();
                if let Some(sys_arr) = sh.get("system").and_then(Value::as_array) {
                    for item in sys_arr {
                        if let Some(txt) = item.get("text").and_then(Value::as_str) {
                            sc.system += estimate_text_tokens(txt);
                        } else {
                            sc.system += estimate_json_tokens(item);
                        }
                    }
                }
                if let Some(tools) = sh.get("tools").and_then(Value::as_array) {
                    for tool in tools {
                        let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                        let toks = estimate_json_tokens(tool);
                        if name.starts_with("mcp__") {
                            sc.mcp_tools += toks;
                            sc.mcp_count += 1;
                        } else {
                            sc.builtin_tools += toks;
                            sc.builtin_count += 1;
                        }
                    }
                }
                shapes.insert(hash.to_string(), sc);
            }
            Some("api-request") => {
                if let Some(h) = v.get("shapeHash").and_then(Value::as_str) {
                    last_shape_hash = Some(h.to_string());
                }
            }
            Some("attachment") => {
                let Some(att) = v.get("attachment") else {
                    continue;
                };
                let at = att.get("type").and_then(Value::as_str).unwrap_or("");
                let rendered_toks = v.get("rendered").map(estimate_json_tokens).unwrap_or(0);
                match at {
                    "instructions" => {
                        if let Some(files) = att.get("files").and_then(Value::as_array) {
                            for (idx, fi) in files.iter().enumerate() {
                                let key = fi
                                    .get("path")
                                    .or_else(|| fi.get("filePath"))
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                                    .unwrap_or_else(|| format!("rule-{idx}"));
                                let t = fi
                                    .get("content")
                                    .and_then(Value::as_str)
                                    .map(estimate_text_tokens)
                                    .unwrap_or(0);
                                if t > 0 {
                                    instructions.insert(key, t);
                                }
                            }
                        } else if rendered_toks > 0 {
                            instructions.insert("instructions".into(), rendered_toks);
                        }
                    }
                    "skill_listing" => {
                        if rendered_toks > 0 {
                            skills_toks = rendered_toks;
                        }
                        if let Some(n) = att.get("skillCount").and_then(Value::as_u64) {
                            skills_count = n as usize;
                        }
                    }
                    "agent_listing_delta" => {
                        if rendered_toks > 0 {
                            subagents_toks = rendered_toks;
                        }
                    }
                    "deferred_tools_delta" | "deferred_tools_record" => {
                        if rendered_toks > 0 {
                            deferred_mcp_toks = deferred_mcp_toks.saturating_add(rendered_toks);
                        }
                    }
                    "environment" | "session_context" | "date" | "model" | "auto_mode" => {
                        if rendered_toks > 0 {
                            env_att_toks.insert(at.to_string(), rendered_toks);
                        }
                    }
                    "edited_text_file"
                    | "task_reminder"
                    | "hook_additional_context"
                    | "queued_command"
                    | "silent_turn_reminder" => {
                        conv_att = conv_att.saturating_add(rendered_toks);
                    }
                    _ => {}
                }
            }
            Some("system") => {
                let sub = v.get("subtype").and_then(Value::as_str);
                match sub {
                    Some("compact_boundary" | "microcompact_boundary") => {
                        conv_user_text = 0;
                        conv_asst_text = 0;
                        conv_tool_io = 0;
                        conv_att = 0;
                        turns = 0;
                        last_used = v
                            .get("compactMetadata")
                            .and_then(|m| {
                                m.get("postTokens")
                                    .or_else(|| m.get("post_tokens"))
                                    .and_then(Value::as_u64)
                            })
                            .filter(|&n| n > 0);
                        compacted_only = true;
                        cleared = false;
                    }
                    Some("local_command") => {
                        let cmd = v
                            .get("commandRun")
                            .and_then(|c| c.get("command"))
                            .and_then(Value::as_str);
                        let content = v.get("content").and_then(Value::as_str).unwrap_or("");
                        if cmd == Some("clear")
                            || content.contains("<command-name>/clear</command-name>")
                        {
                            conv_user_text = 0;
                            conv_asst_text = 0;
                            conv_tool_io = 0;
                            conv_att = 0;
                            turns = 0;
                            instructions.clear();
                            skills_toks = 0;
                            skills_count = 0;
                            subagents_toks = 0;
                            deferred_mcp_toks = 0;
                            env_att_toks.clear();
                            last_used = Some(0);
                            cleared = true;
                            compacted_only = false;
                        } else if cmd == Some("compact")
                            || content.contains("<command-name>/compact</command-name>")
                        {
                            conv_user_text = 0;
                            conv_asst_text = 0;
                            conv_tool_io = 0;
                            conv_att = 0;
                            turns = 0;
                            last_used = None;
                            compacted_only = true;
                            cleared = false;
                        }
                    }
                    _ => {}
                }
            }
            Some("user") => {
                if v.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
                    if !compacted_only {
                        conv_user_text = 0;
                        conv_asst_text = 0;
                        conv_tool_io = 0;
                        conv_att = 0;
                        turns = 0;
                        last_used = None;
                        compacted_only = true;
                    }
                    if let Some(msg) = v.get("message") {
                        if let Some(c) = msg.get("content") {
                            conv_user_text = conv_user_text.saturating_add(estimate_json_tokens(c));
                        }
                    }
                    continue;
                }
                if let Some(raw) = extract_claude_message_text(&v) {
                    if raw.contains("<command-name>/clear</command-name>") {
                        conv_user_text = 0;
                        conv_asst_text = 0;
                        conv_tool_io = 0;
                        conv_att = 0;
                        turns = 0;
                        instructions.clear();
                        skills_toks = 0;
                        skills_count = 0;
                        subagents_toks = 0;
                        deferred_mcp_toks = 0;
                        env_att_toks.clear();
                        last_used = Some(0);
                        cleared = true;
                        compacted_only = false;
                        continue;
                    }
                    if raw.contains("<command-name>/compact</command-name>") {
                        conv_user_text = 0;
                        conv_asst_text = 0;
                        conv_tool_io = 0;
                        conv_att = 0;
                        turns = 0;
                        last_used = None;
                        compacted_only = true;
                        cleared = false;
                        continue;
                    }
                    if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                        if let Some(cm) = parse_claude_context_usage_markdown(&raw) {
                            if let Some(u) = cm.used {
                                last_used = Some(u);
                            }
                            if let Some(m) = cm.model_name.as_ref() {
                                last_model = Some(m.clone());
                            }
                            if let Some(pw) = cm.parsed_win.filter(|&w| w > 0) {
                                last_window = Some(pw);
                                explicit_window =
                                    Some((cm.model_name.map(|s| s.to_ascii_lowercase()), pw));
                            } else if let Some(mw) = cm.model_win {
                                last_window = Some(mw);
                            }
                            continue;
                        }
                    }
                    if is_claude_local_cmd_wrapper(&raw) {
                        continue;
                    }
                }
                if let Some(c) = v.get("message").and_then(|m| m.get("content")) {
                    match c {
                        Value::String(s) => {
                            conv_user_text = conv_user_text.saturating_add(estimate_text_tokens(s));
                            if v.get("isMeta").and_then(Value::as_bool) != Some(true) {
                                turns += 1;
                            }
                        }
                        Value::Array(arr) => {
                            let mut has_user_text = false;
                            for item in arr {
                                if item.get("type").and_then(Value::as_str) == Some("tool_result") {
                                    conv_tool_io = conv_tool_io.saturating_add(
                                        item.get("content").map(estimate_json_tokens).unwrap_or(0),
                                    );
                                } else {
                                    has_user_text = true;
                                    conv_user_text =
                                        conv_user_text.saturating_add(estimate_json_tokens(item));
                                }
                            }
                            if has_user_text && v.get("isMeta").and_then(Value::as_bool) != Some(true)
                            {
                                turns += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("assistant") => {
                let Some(msg) = v.get("message") else {
                    continue;
                };
                let model = msg.get("model").and_then(Value::as_str);
                if model == Some("<synthetic>") {
                    continue;
                }
                if let Some(m) = model.filter(|s| !s.is_empty()) {
                    last_model = Some(m.to_string());
                    let inferred = claude_model_context_window(m);
                    let ml = m.to_ascii_lowercase();
                    let kept_explicit = explicit_window.as_ref().and_then(|(em, ew)| {
                        em.as_deref()
                            .is_none_or(|em| em == ml)
                            .then_some((*ew).max(inferred))
                    });
                    if let Some(w) = kept_explicit {
                        last_window = Some(w);
                    } else {
                        explicit_window = None;
                        last_window = Some(inferred);
                    }
                }
                if let Some(c) = msg.get("content") {
                    match c {
                        Value::String(s) => {
                            conv_asst_text = conv_asst_text.saturating_add(estimate_text_tokens(s));
                        }
                        Value::Array(arr) => {
                            for item in arr {
                                match item.get("type").and_then(Value::as_str) {
                                    Some("tool_use") => {
                                        let name_tok = item
                                            .get("name")
                                            .and_then(Value::as_str)
                                            .map(estimate_text_tokens)
                                            .unwrap_or(0);
                                        let inp_tok =
                                            item.get("input").map(estimate_json_tokens).unwrap_or(0);
                                        conv_tool_io = conv_tool_io
                                            .saturating_add(name_tok)
                                            .saturating_add(inp_tok);
                                    }
                                    Some("thinking") => {
                                        let t = item
                                            .get("thinking")
                                            .and_then(Value::as_str)
                                            .map(estimate_text_tokens)
                                            .unwrap_or(0);
                                        conv_asst_text = conv_asst_text.saturating_add(t);
                                    }
                                    Some("text") => {
                                        let t = item
                                            .get("text")
                                            .and_then(Value::as_str)
                                            .map(estimate_text_tokens)
                                            .unwrap_or(0);
                                        conv_asst_text = conv_asst_text.saturating_add(t);
                                    }
                                    _ => {
                                        conv_asst_text = conv_asst_text
                                            .saturating_add(estimate_json_tokens(item));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(u) = msg.get("usage").or_else(|| v.get("usage")) {
                    let input = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let create = u
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let read = u
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let sum = input.saturating_add(create).saturating_add(read);
                    if sum > 0 {
                        last_used = Some(sum);
                        cleared = false;
                        compacted_only = false;
                    }
                }
            }
            _ => {}
        }
    }

    let used_tokens = last_used.unwrap_or(0);
    let window_tokens = normalize_window_for_used(used_tokens, last_window);
    if cleared || used_tokens == 0 {
        return Some(ContextBreakdown {
            agent: "claude".into(),
            model: last_model,
            used_tokens: 0,
            window_tokens,
            is_estimated: true,
            cleared,
            compacted_summary_only: compacted_only,
            categories: Vec::new(),
            autocompact_buffer: None,
        });
    }

    let shape = last_shape_hash
        .as_deref()
        .and_then(|h| shapes.get(h))
        .copied()
        .unwrap_or_default();
    let inst_toks: u64 = instructions.values().sum();
    let env_toks: u64 = env_att_toks.values().sum();
    let rules_toks = inst_toks.saturating_add(env_toks);
    let mcp_toks = shape.mcp_tools.saturating_add(deferred_mcp_toks);

    let mut fixed = Vec::new();
    if shape.system > 0 {
        fixed.push((ContextCategoryKind::SystemPrompt, None, shape.system));
    }
    if shape.builtin_tools > 0 {
        let d = (shape.builtin_count > 0).then(|| {
            crate::i18n::tf("ag.ctx_tools_n", &[("n", &shape.builtin_count)])
        });
        fixed.push((ContextCategoryKind::ToolDefinitions, d, shape.builtin_tools));
    }
    if rules_toks > 0 {
        let d = (!instructions.is_empty()).then(|| {
            crate::i18n::tf("ag.ctx_files_n", &[("n", &instructions.len())])
        });
        fixed.push((ContextCategoryKind::Rules, d, rules_toks));
    }
    if skills_toks > 0 {
        let d = (skills_count > 0)
            .then(|| crate::i18n::tf("ag.ctx_skills_n", &[("n", &skills_count)]));
        fixed.push((ContextCategoryKind::Skills, d, skills_toks));
    }
    if mcp_toks > 0 {
        let d = (shape.mcp_count > 0)
            .then(|| crate::i18n::tf("ag.ctx_tools_n", &[("n", &shape.mcp_count)]));
        fixed.push((ContextCategoryKind::McpTools, d, mcp_toks));
    }
    if subagents_toks > 0 {
        fixed.push((ContextCategoryKind::Subagents, None, subagents_toks));
    }

    let conv_raw = conv_user_text
        .saturating_add(conv_asst_text)
        .saturating_add(conv_tool_io)
        .saturating_add(conv_att);
    let fixed_sum: u64 = fixed.iter().map(|(_, _, t)| *t).sum();
    let eff_conv = if fixed_sum < used_tokens {
        used_tokens.saturating_sub(fixed_sum)
    } else {
        conv_raw
    };
    let tool_io_share = if conv_raw > 0 && conv_tool_io > 0 {
        ((conv_tool_io as f64 / conv_raw as f64) * eff_conv as f64).round() as u64
    } else {
        0
    };
    let conv_detail = if compacted_only {
        Some(crate::i18n::t("ag.ctx_compacted").to_string())
    } else {
        format_conv_detail(turns, tool_io_share)
    };
    let categories = calibrate_breakdown_categories(fixed, conv_raw, conv_detail, used_tokens);

    let autocompact_buffer = (window_tokens >= 200_000).then(|| {
        // Claude Code reserves ~3.3% (e.g. ~33k on 1M, ~13k-33k) for autocompact buffer.
        ((window_tokens as f64 * 0.033).round() as u64).min(45_000)
    });

    Some(ContextBreakdown {
        agent: "claude".into(),
        model: last_model,
        used_tokens,
        window_tokens,
        is_estimated: true,
        cleared: false,
        compacted_summary_only: compacted_only,
        categories,
        autocompact_buffer,
    })
}

fn parse_agy_context_breakdown(path: &Path) -> Option<ContextBreakdown> {
    let f = File::open(path).ok()?;
    let reader = BufReader::with_capacity(64 * 1024, f);

    let mut first_sys_tools_overhead: Option<u64> = None;
    let mut pre_first_visible = 0u64;
    let mut saw_first_pr = false;

    let mut sys_msg_toks = 0u64;
    let mut conv_user_toks = 0u64;
    let mut conv_asst_toks = 0u64;
    let mut conv_tool_io = 0u64;
    let mut turns = 0u32;

    let mut last_model: Option<String> = None;
    let mut last_window: Option<u64> = None;
    let mut last_used: Option<u64> = None;
    let mut compacted_only = false;

    for line_res in reader.lines() {
        let Ok(line) = line_res else {
            break;
        };
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("CHECKPOINT") => {
                sys_msg_toks = 0;
                conv_user_toks = 0;
                conv_asst_toks = 0;
                conv_tool_io = 0;
                turns = 0;
                let out = v
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .filter(|&n| n > 0);
                last_used = out;
                compacted_only = true;
                if let Some(o) = out {
                    conv_asst_toks = o;
                }
            }
            Some("SYSTEM_MESSAGE") => {
                let t = v
                    .get("content")
                    .and_then(Value::as_str)
                    .map(estimate_text_tokens)
                    .unwrap_or(0);
                sys_msg_toks = sys_msg_toks.saturating_add(t);
                if !saw_first_pr {
                    pre_first_visible = pre_first_visible.saturating_add(t);
                }
            }
            Some("USER_INPUT") => {
                let t = v
                    .get("content")
                    .and_then(Value::as_str)
                    .map(estimate_text_tokens)
                    .unwrap_or(0);
                conv_user_toks = conv_user_toks.saturating_add(t);
                turns += 1;
                if !saw_first_pr {
                    pre_first_visible = pre_first_visible.saturating_add(t);
                }
            }
            Some("GENERIC") => {
                let t = v
                    .get("content")
                    .and_then(Value::as_str)
                    .map(estimate_text_tokens)
                    .unwrap_or(0);
                conv_tool_io = conv_tool_io.saturating_add(t);
                if !saw_first_pr {
                    pre_first_visible = pre_first_visible.saturating_add(t);
                }
            }
            Some("PLANNER_RESPONSE") => {
                if let Some(m) = v.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    last_model = Some(m.to_string());
                    last_window = Some(agy_model_context_window(m));
                }
                let input = v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                let cache_read = v
                    .get("cache_read_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let sum = input.saturating_add(cache_read);
                if sum > 0 {
                    if !saw_first_pr {
                        saw_first_pr = true;
                        first_sys_tools_overhead = Some(sum.saturating_sub(pre_first_visible));
                    }
                    last_used = Some(sum);
                    compacted_only = false;
                }
                let txt_tok = v
                    .get("content")
                    .and_then(Value::as_str)
                    .map(estimate_text_tokens)
                    .unwrap_or(0);
                let think_tok = v
                    .get("thinking")
                    .and_then(Value::as_str)
                    .map(estimate_text_tokens)
                    .unwrap_or(0);
                let tc_tok = v.get("tool_calls").map(estimate_json_tokens).unwrap_or(0);
                conv_asst_toks = conv_asst_toks
                    .saturating_add(txt_tok)
                    .saturating_add(think_tok);
                conv_tool_io = conv_tool_io.saturating_add(tc_tok);
            }
            _ => {}
        }
    }

    let used_tokens = last_used.unwrap_or(0);
    let window_tokens = normalize_window_for_used(used_tokens, last_window.or(Some(1_000_000)));
    if used_tokens == 0 {
        return None;
    }

    let mut fixed = Vec::new();
    let base_overhead = first_sys_tools_overhead.unwrap_or(0);
    if !compacted_only && base_overhead > 0 {
        fixed.push((ContextCategoryKind::SystemPrompt, None, base_overhead));
    }
    if sys_msg_toks > 0 {
        fixed.push((ContextCategoryKind::Rules, None, sys_msg_toks));
    }
    let conv_raw = conv_user_toks
        .saturating_add(conv_asst_toks)
        .saturating_add(conv_tool_io);
    let fixed_sum: u64 = fixed.iter().map(|(_, _, t)| *t).sum();
    let eff_conv = if fixed_sum < used_tokens {
        used_tokens.saturating_sub(fixed_sum)
    } else {
        conv_raw
    };
    let tool_io_share = if conv_raw > 0 && conv_tool_io > 0 {
        ((conv_tool_io as f64 / conv_raw as f64) * eff_conv as f64).round() as u64
    } else {
        0
    };
    let conv_detail = if compacted_only {
        Some(crate::i18n::t("ag.ctx_compacted").to_string())
    } else {
        format_conv_detail(turns, tool_io_share)
    };
    let categories = calibrate_breakdown_categories(fixed, conv_raw, conv_detail, used_tokens);

    Some(ContextBreakdown {
        agent: "agy".into(),
        model: last_model,
        used_tokens,
        window_tokens,
        is_estimated: true,
        cleared: false,
        compacted_summary_only: compacted_only,
        categories,
        autocompact_buffer: None,
    })
}

fn parse_codex_context_breakdown(path: &Path) -> Option<ContextBreakdown> {
    let f = File::open(path).ok()?;
    let reader = BufReader::with_capacity(64 * 1024, f);

    let mut sys_rules_toks = 0u64;
    let mut conv_msg_toks = 0u64;
    let mut conv_tool_io = 0u64;
    let mut turns = 0u32;
    let mut last_window: Option<u64> = None;
    let mut last_used: Option<u64> = None;
    let mut compacted_only = false;

    for line_res in reader.lines() {
        let Ok(line) = line_res else {
            break;
        };
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("compacted") => {
                sys_rules_toks = 0;
                conv_msg_toks = 0;
                conv_tool_io = 0;
                turns = 0;
                last_used = None;
                compacted_only = true;
            }
            Some("event_msg") => {
                if let Some(p) = v.get("payload") {
                    match p.get("type").and_then(Value::as_str) {
                        Some("context_compacted") => {
                            sys_rules_toks = 0;
                            conv_msg_toks = 0;
                            conv_tool_io = 0;
                            turns = 0;
                            last_used = None;
                            compacted_only = true;
                        }
                        Some("token_count") => {
                            if let Some(info) = p.get("info").filter(|x| !x.is_null()) {
                                if let Some(win) = info
                                    .get("model_context_window")
                                    .and_then(Value::as_u64)
                                    .filter(|&w| w > 0)
                                {
                                    last_window = Some(win);
                                }
                                if let Some(u) = info
                                    .get("last_token_usage")
                                    .and_then(|t| {
                                        t.get("input_tokens")
                                            .or_else(|| t.get("total_tokens"))
                                            .and_then(Value::as_u64)
                                    })
                                    .filter(|&n| n > 0)
                                {
                                    last_used = Some(u);
                                    compacted_only = false;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("response_item") => {
                if let Some(p) = v.get("payload") {
                    let ptype = p.get("type").and_then(Value::as_str).unwrap_or("");
                    if matches!(
                        ptype,
                        "function_call" | "function_call_output" | "custom_tool_call" | "custom_tool_call_output"
                    ) {
                        conv_tool_io = conv_tool_io.saturating_add(estimate_json_tokens(p));
                        continue;
                    }
                    let role = p.get("role").and_then(Value::as_str);
                    if matches!(role, Some("developer" | "system")) {
                        sys_rules_toks = sys_rules_toks.saturating_add(
                            p.get("content").map(estimate_json_tokens).unwrap_or(0),
                        );
                    } else if role == Some("user") {
                        if let Some(arr) = p.get("content").and_then(Value::as_array) {
                            let mut has_user = false;
                            for item in arr {
                                if let Some(t) = item.get("text").and_then(Value::as_str) {
                                    let trimmed = t.trim_start();
                                    let tok = estimate_text_tokens(t);
                                    if trimmed.starts_with("<environment_context>")
                                        || trimmed.starts_with("# AGENTS.md")
                                        || trimmed.starts_with("<INSTRUCTIONS>")
                                    {
                                        sys_rules_toks = sys_rules_toks.saturating_add(tok);
                                    } else {
                                        has_user = true;
                                        conv_msg_toks = conv_msg_toks.saturating_add(tok);
                                    }
                                } else {
                                    has_user = true;
                                    conv_msg_toks =
                                        conv_msg_toks.saturating_add(estimate_json_tokens(item));
                                }
                            }
                            if has_user {
                                turns += 1;
                            }
                        }
                    } else if role == Some("assistant") {
                        conv_msg_toks = conv_msg_toks.saturating_add(
                            p.get("content").map(estimate_json_tokens).unwrap_or(0),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    let used_tokens = last_used.filter(|&u| u > 0)?;
    let window_tokens = normalize_window_for_used(used_tokens, last_window);
    let mut fixed = Vec::new();
    if sys_rules_toks > 0 {
        fixed.push((ContextCategoryKind::Rules, None, sys_rules_toks));
    }
    let conv_raw = conv_msg_toks.saturating_add(conv_tool_io);
    let fixed_sum: u64 = fixed.iter().map(|(_, _, t)| *t).sum();
    let eff_conv = if fixed_sum < used_tokens {
        used_tokens.saturating_sub(fixed_sum)
    } else {
        conv_raw
    };
    let tool_io_share = if conv_raw > 0 && conv_tool_io > 0 {
        ((conv_tool_io as f64 / conv_raw as f64) * eff_conv as f64).round() as u64
    } else {
        0
    };
    let conv_detail = format_conv_detail(turns, tool_io_share);
    let categories = calibrate_breakdown_categories(fixed, conv_raw, conv_detail, used_tokens);

    Some(ContextBreakdown {
        agent: "codex".into(),
        model: None,
        used_tokens,
        window_tokens,
        is_estimated: true,
        cleared: false,
        compacted_summary_only: compacted_only,
        categories,
        autocompact_buffer: None,
    })
}

/// Format a unix timestamp as a compact relative age (`2m`, `3h`, `昨天`, `11-09`).
pub fn relative_time_short(unix_secs: i64, now_secs: i64) -> String {
    let diff = (now_secs - unix_secs).max(0);
    if diff < 60 {
        "now".into()
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86_400 {
        format!("{}h", diff / 3600)
    } else if diff < 86_400 * 7 {
        format!("{}d", diff / 86_400)
    } else {
        chrono::DateTime::from_timestamp(unix_secs, 0)
            .map(|dt| dt.format("%m-%d").to_string())
            .unwrap_or_else(|| format!("{}d", diff / 86_400))
    }
}

pub fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_head_tail_and_skips_sidechains() {
        let tmp = std::env::temp_dir().join(format!("shellax-hist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let proj = tmp.join("projects").join("-Users-test-ShellAX");
        std::fs::create_dir_all(&proj).unwrap();

        let main_file = proj.join("11111111-2222-3333-4444-555555555555.jsonl");
        let mut content = String::new();
        content.push_str(r#"{"type":"user","sessionId":"11111111-2222-3333-4444-555555555555","cwd":"/Users/test/ShellAX","gitBranch":"feat/ui","timestamp":"2026-11-17T01:00:00Z","isSidechain":false,"message":{"role":"user","content":"修复 SFTP 断点续传"}}"#);
        content.push('\n');
        // Pad > 250KB of multibyte CJK lines in the middle to exercise head/tail slicing safely.
        let filler = format!(r#"{{"type":"assistant","message":{{"role":"assistant","content":"{}"}}}}"#, "中文测试内容".repeat(40));
        for _ in 0..900 {
            content.push_str(&filler);
            content.push('\n');
        }
        content.push_str(r#"{"type":"ai-title","aiTitle":"SFTP resume fix","sessionId":"11111111-2222-3333-4444-555555555555"}"#);
        content.push('\n');
        content.push_str(r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5-5","usage":{"input_tokens":12000,"cache_read_input_tokens":686000}}}"#);
        content.push('\n');
        content.push_str(r#"{"type":"last-prompt","lastPrompt":"再加上单元测试","timestamp":"2026-11-17T02:00:00Z"}"#);
        content.push('\n');
        std::fs::write(&main_file, &content).unwrap();

        let side_file = proj.join("99999999-2222-3333-4444-555555555555.jsonl");
        std::fs::write(
            &side_file,
            r#"{"type":"user","sessionId":"99999999","isSidechain":true,"message":{"role":"user","content":"subtask"}}"#,
        )
        .unwrap();

        let codex_home = tmp.join("codex");
        let codex_day = codex_home.join("sessions/2026/11/17");
        std::fs::create_dir_all(&codex_day).unwrap();
        std::fs::write(
            codex_home.join("session_index.jsonl"),
            "{\"id\":\"019a0000-0000-7000-8000-000000000001\",\"thread_name\":\"Add IP ping latency\"}\n",
        )
        .unwrap();
        std::fs::write(
            codex_day.join("rollout-2026-11-17-019a0000-0000-7000-8000-000000000001.jsonl"),
            concat!(
                r#"{"timestamp":"2026-11-17T03:00:00Z","type":"session_meta","payload":{"id":"019a0000-0000-7000-8000-000000000001","cwd":"/Users/test/ShellAX","git":{"branch":"master"}}}"#,
                "\n",
                r#"{"timestamp":"2026-11-17T03:01:00Z","type":"event_msg","payload":{"type":"user_message","message":"增加 IP 延迟测试"}}"#,
                "\n"
            ),
        )
        .unwrap();

        let providers: Vec<Box<dyn HistoryProvider>> = vec![
            Box::new(ClaudeProvider::with_root(tmp.join("projects"))),
            Box::new(CodexProvider::with_home(codex_home)),
        ];
        let cache = tmp.join("agent-history.json");
        let list = scan_all(&cache, &providers);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].agent, "codex");
        assert_eq!(list[0].display_title(), "Add IP ping latency");
        assert_eq!(list[0].branch.as_deref(), Some("master"));

        assert_eq!(list[1].agent, "claude");
        assert_eq!(list[1].display_title(), "SFTP resume fix");
        assert_eq!(list[1].first_prompt.as_deref(), Some("修复 SFTP 断点续传"));
        assert_eq!(list[1].last_prompt.as_deref(), Some("再加上单元测试"));
        assert_eq!(list[1].branch.as_deref(), Some("feat/ui"));
        assert_eq!(list[1].ctx_used, Some(698_000));
        assert_eq!(list[1].ctx_window, Some(1_000_000));
        assert_eq!(list[1].context_ring_info(), Some((0.698, "70%".into())));
        assert!(list[1].matches_query("断点续传"));

        let cached = load_cached(&cache);
        assert_eq!(cached.len(), 2);

        let agy_home = tmp.join("agy");
        std::fs::create_dir_all(agy_home.join("cache")).unwrap();
        let agy_log_dir = agy_home.join("brain/c1/.system_generated/logs");
        std::fs::create_dir_all(&agy_log_dir).unwrap();
        std::fs::write(
            agy_log_dir.join("transcript.jsonl"),
            concat!(
                r#"{"step_index":10,"type":"PLANNER_RESPONSE","model":"gemini-4-hydrogen","input_tokens":6000,"cache_read_tokens":510000,"output_tokens":900}"#,
                "\n",
                r#"{"step_index":11,"type":"CHECKPOINT","input_tokens":6400,"cache_read_tokens":510858,"output_tokens":12972}"#,
                "\n"
            ),
        )
        .unwrap();
        std::fs::write(
            agy_home.join("cache/conversation_metadata.json"),
            r#"{"conversations":{"c1":{"summary":{"ID":"c1","Title":"Greeting And Hello","Preview":"你好","NumSteps":4,"UpdatedAt":"2026-11-17T10:43:06Z","WorkspaceURIs":["file:///Users/test/ShellAX"],"AppDataDir":"antigravity-cli"}},"c2":{"summary":{"ID":"c2","Title":"IDE Chat","NumSteps":10,"UpdatedAt":"2026-11-17T10:00:00Z","WorkspaceURIs":["file:///Users/test/ShellAX"],"AppDataDir":"antigravity"}}}}"#,
        )
        .unwrap();
        let agy_provider = AntigravityProvider::with_home(agy_home);
        let agy_list = agy_provider.extra_sessions();
        assert_eq!(agy_list.len(), 1);
        assert_eq!(agy_list[0].agent, "agy");
        assert_eq!(agy_list[0].display_title(), "Greeting And Hello");
        assert_eq!(agy_list[0].cwd.as_deref(), Some("/Users/test/ShellAX"));
        // After CHECKPOINT, context drops from 516k down to the compacted summary output (12,972).
        assert_eq!(agy_list[0].ctx_used, Some(12_972));
        assert!(agy_list[0].ctx_reset);

        // Next PLANNER_RESPONSE replaces checkpoint estimate with exact post-compact input+cache.
        std::fs::write(
            agy_log_dir.join("transcript.jsonl"),
            concat!(
                r#"{"step_index":10,"type":"PLANNER_RESPONSE","model":"gemini-4-hydrogen","input_tokens":6000,"cache_read_tokens":510000,"output_tokens":900}"#,
                "\n",
                r#"{"step_index":11,"type":"CHECKPOINT","input_tokens":6400,"cache_read_tokens":510858,"output_tokens":12972}"#,
                "\n",
                r#"{"step_index":12,"type":"PLANNER_RESPONSE","model":"gemini-4-hydrogen","input_tokens":24839,"cache_read_tokens":0,"output_tokens":335}"#,
                "\n"
            ),
        )
        .unwrap();
        let agy_list2 = agy_provider.extra_sessions();
        assert_eq!(agy_list2[0].ctx_used, Some(24_839));
        assert_eq!(agy_list2[0].ctx_window, Some(1_000_000));
        assert!(!agy_list2[0].ctx_reset);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn claude_and_codex_respect_compact_clear_continued_in_and_large_trailing_blobs() {
        let tmp = std::env::temp_dir().join(format!(
            "shellax-hist-ctx-{}-{}",
            std::process::id(),
            now_unix_secs()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let proj = tmp.join("projects/p1");
        std::fs::create_dir_all(&proj).unwrap();

        // 1. Claude compact_boundary clears stale 700k pre-compact usage until next assistant turn.
        let compact_file = proj.join("compacted-session.jsonl");
        std::fs::write(
            &compact_file,
            concat!(
                r#"{"type":"user","sessionId":"compacted-session","cwd":"/repo","message":{"role":"user","content":"start"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5-5","usage":{"input_tokens":10000,"cache_read_input_tokens":690000}}}"#,
                "\n",
                r#"{"type":"system","subtype":"compact_boundary","compactMetadata":{"trigger":"manual","preTokens":700000}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","model":"<synthetic>","usage":{"input_tokens":0}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let cp = ClaudeProvider::with_root(tmp.join("projects"));
        let md = std::fs::metadata(&compact_file).unwrap();
        let m = cp.parse_file(&compact_file, 100, md.len()).unwrap();
        assert_eq!(m.ctx_used, None);
        assert!(m.ctx_reset);
        assert!(m.context_detail().is_none());

        // 2. Claude `/clear` resets usage to 0% (`Some(0)` + `ctx_reset`) and does not pollute prompts.
        let cleared_file = proj.join("cleared-session.jsonl");
        std::fs::write(
            &cleared_file,
            concat!(
                r#"{"type":"user","sessionId":"cleared-session","cwd":"/repo","message":{"role":"user","content":"real task"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5-5","usage":{"input_tokens":50000,"cache_read_input_tokens":450000}}}"#,
                "\n",
                r#"{"type":"user","sessionId":"cleared-session","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
                "\n",
                r#"{"type":"system","subtype":"local_command","commandRun":{"command":"clear","args":""}}"#,
                "\n",
                r#"{"type":"continued-in","sessionId":"cleared-session","continuedInSessionId":"next-session-id"}"#,
                "\n"
            ),
        )
        .unwrap();
        let md = std::fs::metadata(&cleared_file).unwrap();
        let m = cp.parse_file(&cleared_file, 100, md.len()).unwrap();
        assert_eq!(m.ctx_used, Some(0));
        assert!(m.ctx_reset);
        assert_eq!(m.first_prompt.as_deref(), Some("real task"));
        assert_eq!(m.last_prompt.as_deref(), Some("real task"));
        assert_eq!(m.continued_in.as_deref(), Some("next-session-id"));
        let det = m.context_detail().expect("cleared detail");
        assert_eq!(det.frac, 0.0);
        assert_eq!(det.short_label, "0%");
        assert_eq!(det.bar_label, "0%");

        // 3. Large trailing blob (>128KB) must NOT cause fallback to turn-1 `head` usage.
        let blob_file = proj.join("blob-session.jsonl");
        let mut body = String::new();
        body.push_str(r#"{"type":"user","sessionId":"blob-session","cwd":"/repo","message":{"role":"user","content":"big session"}}"#);
        body.push('\n');
        body.push_str(r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5-5","usage":{"input_tokens":5000,"cache_read_input_tokens":5000}}}"#);
        body.push('\n');
        // Later turn with 420k context
        body.push_str(r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5-5","usage":{"input_tokens":20000,"cache_read_input_tokens":400000}}}"#);
        body.push('\n');
        // >160KB trailing non-assistant blob so 128KB tail misses the 420k assistant line.
        let huge = format!(r#"{{"type":"api-request-blob","data":"{}"}}"#, "x".repeat(170_000));
        body.push_str(&huge);
        body.push('\n');
        std::fs::write(&blob_file, &body).unwrap();
        let md = std::fs::metadata(&blob_file).unwrap();
        let m = cp.parse_file(&blob_file, 100, md.len()).unwrap();
        assert_eq!(m.ctx_used, Some(420_000));
        assert_eq!(m.ctx_window, Some(1_000_000));
        assert!(!m.ctx_reset);

        // 4. Codex `compacted` / `context_compacted` clears stale token_count until next turn.
        let codex_home = tmp.join("codex");
        let codex_file = codex_home.join("sessions/rollout-1.jsonl");
        std::fs::create_dir_all(codex_file.parent().unwrap()).unwrap();
        std::fs::write(
            &codex_file,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"cx-1","cwd":"/repo"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":200000,"last_token_usage":{"input_tokens":150000}}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"context_compacted"}}"#,
                "\n"
            ),
        )
        .unwrap();
        let xp = CodexProvider::with_home(codex_home);
        let md = std::fs::metadata(&codex_file).unwrap();
        let xm = xp.parse_file(&codex_file, 100, md.len()).unwrap();
        assert_eq!(xm.ctx_used, None);
        assert!(xm.ctx_reset);
        assert_eq!(xm.ctx_window, Some(200_000));

        // 5. Context breakdown calibration across Claude, Antigravity, and Codex.
        let bd_file = proj.join("breakdown-claude.jsonl");
        std::fs::write(
            &bd_file,
            concat!(
                r#"{"type":"api-request-shape","shapeHash":"h1","shape":{"system":[{"type":"text","text":"You are Claude Code."}],"tools":[{"name":"Bash","description":"Run shell"},{"name":"mcp__github__get_issue","description":"Read issue"}]}}"#,
                "\n",
                r#"{"type":"api-request","shapeHash":"h1"}"#,
                "\n",
                r#"{"type":"attachment","attachment":{"type":"instructions","files":[{"path":"/repo/AGENTS.md","content":"Keep code clean.\n保持代码整洁。"}]},"rendered":[]}"#,
                "\n",
                r#"{"type":"attachment","attachment":{"type":"skill_listing","skillCount":3},"rendered":["skill-a","skill-b","skill-c"]}"#,
                "\n",
                r#"{"type":"attachment","attachment":{"type":"agent_listing_delta"},"rendered":["subagent-research"]}"#,
                "\n",
                r#"{"type":"user","sessionId":"bd1","message":{"role":"user","content":"Analyze the repo"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}],"usage":{"input_tokens":1000,"cache_read_input_tokens":19000}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let bd = analyze_context_breakdown("claude", &bd_file).expect("claude breakdown");
        assert_eq!(bd.used_tokens, 20_000);
        assert_eq!(bd.window_tokens, 1_000_000);
        assert_eq!(
            bd.categories.iter().map(|c| c.tokens).sum::<u64>(),
            bd.used_tokens
        );
        assert!(bd
            .categories
            .iter()
            .any(|c| c.kind == ContextCategoryKind::SystemPrompt));
        assert!(bd
            .categories
            .iter()
            .any(|c| c.kind == ContextCategoryKind::ToolDefinitions));
        assert!(bd
            .categories
            .iter()
            .any(|c| c.kind == ContextCategoryKind::McpTools));
        assert!(bd
            .categories
            .iter()
            .any(|c| c.kind == ContextCategoryKind::Rules));
        assert!(bd
            .categories
            .iter()
            .any(|c| c.kind == ContextCategoryKind::Skills));
        assert!(bd
            .categories
            .iter()
            .any(|c| c.kind == ContextCategoryKind::Subagents));
        assert!(bd
            .categories
            .iter()
            .any(|c| c.kind == ContextCategoryKind::Conversation));

        // 6. Antigravity breakdown prefers transcript_full.jsonl and sums exactly to used_tokens.
        let agy_bd_dir = tmp.join("agy_bd/brain/c9/.system_generated/logs");
        std::fs::create_dir_all(&agy_bd_dir).unwrap();
        std::fs::write(agy_bd_dir.join("transcript.jsonl"), "{}\n").unwrap();
        std::fs::write(
            agy_bd_dir.join("transcript_full.jsonl"),
            concat!(
                r#"{"step_index":0,"type":"USER_INPUT","content":"你好，帮我看下项目"}"#,
                "\n",
                r#"{"step_index":1,"type":"PLANNER_RESPONSE","model":"gemini-4-hydrogen","input_tokens":15000,"cache_read_tokens":0,"thinking":"let me inspect","tool_calls":[{"name":"run_command"}]}"#,
                "\n",
                r#"{"step_index":2,"type":"GENERIC","content":"src/main.rs\nsrc/agent.rs"}"#,
                "\n",
                r#"{"step_index":3,"type":"PLANNER_RESPONSE","model":"gemini-4-hydrogen","input_tokens":3000,"cache_read_tokens":15000,"content":"查看完成"}"#,
                "\n"
            ),
        )
        .unwrap();
        let agy_bd =
            analyze_context_breakdown("agy", &agy_bd_dir.join("transcript.jsonl")).expect("agy bd");
        assert_eq!(agy_bd.used_tokens, 18_000);
        assert_eq!(agy_bd.window_tokens, 1_000_000);
        assert_eq!(
            agy_bd.categories.iter().map(|c| c.tokens).sum::<u64>(),
            agy_bd.used_tokens
        );

        // 6. Claude `/clear` + `/context` (`isMeta: true` markdown) reports context before first assistant turn,
        // and subsequent `assistant.usage` supersedes it cleanly.
        let ctx_cmd_file = proj.join("post-clear-context.jsonl");
        std::fs::write(
            &ctx_cmd_file,
            concat!(
                r#"{"type":"user","sessionId":"post-clear-context","cwd":"/repo","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
                "\n",
                r###"{"type":"user","isMeta":true,"sessionId":"post-clear-context","cwd":"/repo","message":{"role":"user","content":"## Context Usage\n\n**Model:** claude-opus-5-5  \n**Tokens:** 25.5k / 1m (3%)\n\n### Messages\n"}}"###,
                "\n"
            ),
        )
        .unwrap();
        let md = std::fs::metadata(&ctx_cmd_file).unwrap();
        let cm = cp.parse_file(&ctx_cmd_file, 100, md.len()).unwrap();
        assert_eq!(cm.ctx_used, Some(25_500));
        assert_eq!(cm.ctx_window, Some(1_000_000));
        assert!(cm.ctx_reset);
        let cdet = cm.context_detail().expect("context command detail");
        assert_eq!(cdet.short_label, "3%");
        assert_eq!(cdet.used, 25_500);
        assert_eq!(cdet.window, 1_000_000);

        std::fs::write(
            &ctx_cmd_file,
            concat!(
                r#"{"type":"user","sessionId":"post-clear-context","cwd":"/repo","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
                "\n",
                r###"{"type":"user","isMeta":true,"sessionId":"post-clear-context","cwd":"/repo","message":{"role":"user","content":"## Context Usage\n\n**Model:** claude-opus-5-5  \n**Tokens:** 25.5k / 1m (3%)"}}"###,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-5-5","usage":{"input_tokens":2,"cache_creation_input_tokens":22713,"cache_read_input_tokens":22933}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let md2 = std::fs::metadata(&ctx_cmd_file).unwrap();
        let cm2 = cp.parse_file(&ctx_cmd_file, 101, md2.len()).unwrap();
        assert_eq!(cm2.ctx_used, Some(45_648));
        assert!(!cm2.ctx_reset);

        // 7. Claude API settings `[1M]` / `[2M]` overrides resolve without running `/context`,
        // and explicit `/context` 1M window is preserved across subsequent assistant turns for
        // unknown custom API models.
        let settings_val: Value = serde_json::from_str(
            r#"{
                "model": "custom-top[2M]",
                "env": {
                    "ANTHROPIC_DEFAULT_FABLE_MODEL": "claude-fable-5-2[1M]",
                    "ANTHROPIC_MODEL": "my-api-proxy[1M]"
                }
            }"#,
        )
        .unwrap();
        let mut overrides = HashMap::new();
        extract_claude_settings_model_windows(&settings_val, &mut overrides);
        assert_eq!(
            claude_model_context_window_with_overrides("claude-fable-5-2", &overrides),
            1_000_000
        );
        assert_eq!(
            claude_model_context_window_with_overrides("my-api-proxy-20260501", &overrides),
            1_000_000
        );
        assert_eq!(
            claude_model_context_window_with_overrides("custom-top", &overrides),
            2_000_000
        );
        assert_eq!(
            claude_model_context_window_with_overrides("claude-fable-5-2", &HashMap::new()),
            1_000_000
        );

        let api_file = proj.join("api-custom-model.jsonl");
        std::fs::write(
            &api_file,
            concat!(
                r#"{"type":"user","sessionId":"api-custom-model","cwd":"/repo","message":{"role":"user","content":"hi"}}"#,
                "\n",
                r###"{"type":"user","isMeta":true,"sessionId":"api-custom-model","cwd":"/repo","message":{"role":"user","content":"## Context Usage\n\n**Model:** custom-proxy-v1  \n**Tokens:** 25.5k / 1m (3%)"}}"###,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","model":"custom-proxy-v1","usage":{"input_tokens":10000,"cache_read_input_tokens":35000}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let md3 = std::fs::metadata(&api_file).unwrap();
        let cm3 = cp.parse_file(&api_file, 102, md3.len()).unwrap();
        assert_eq!(cm3.ctx_used, Some(45_000));
        assert_eq!(cm3.ctx_window, Some(1_000_000));
        let bd3 = analyze_context_breakdown("claude", &api_file).expect("api bd");
        assert_eq!(bd3.window_tokens, 1_000_000);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn telemetry_model_speed_quota_and_local_usage() {
        assert_eq!(
            format_model_short("gemini-4-neon-high"),
            "Gemini 4 Neon (High)"
        );
        assert_eq!(
            format_model_short("gemini-4-neon-medium"),
            "Gemini 4 Neon (Med)"
        );
        assert_eq!(
            format_model_short("claude-opus-5-5-high"),
            "Opus 5.5 (High)"
        );
        assert_eq!(
            format_model_short("claude-sonnet-4-6-20251022"),
            "Sonnet 4.6"
        );
        assert_eq!(format_model_short("gpt-5.3-codex"), "GPT-5.3 Codex");
        assert_eq!(
            format_model_short("Gemini 4 Neon (High)"),
            "Gemini 4 Neon (High)"
        );

        let tmp = std::env::temp_dir().join(format!(
            "shellax_tel_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let agy_dir = tmp.join("agy/brain/s1/.system_generated/logs");
        std::fs::create_dir_all(&agy_dir).unwrap();
        let agy_tr = agy_dir.join("transcript.jsonl");
        std::fs::write(
            &agy_tr,
            concat!(
                r#"{"step_index":0,"type":"USER_INPUT","created_at":"2026-11-27T09:59:19Z","content":"hi"}"#,
                "\n",
                r#"{"step_index":1,"type":"PLANNER_RESPONSE","created_at":"2026-11-27T09:59:19Z","model":"gemini-4-neon-high","input_tokens":20000,"cache_read_tokens":5000,"output_tokens":529,"status":"DONE"}"#,
                "\n",
                r#"{"step_index":2,"type":"GENERIC","created_at":"2026-11-27T09:59:27Z","status":"DONE"}"#,
                "\n"
            ),
        )
        .unwrap();
        let tel = parse_antigravity_transcript_telemetry(&agy_tr);
        assert_eq!(tel.model.as_deref(), Some("gemini-4-neon-high"));
        assert_eq!(tel.ctx_used, Some(25_000));
        assert_eq!(tel.ctx_window, Some(1_000_000));
        assert_eq!(tel.last_output_tokens, Some(529));
        assert_eq!(tel.last_turn_ms, Some(8_000));
        // 529 / 8.0 = 66.125 -> 661 tenths
        assert_eq!(tel.last_speed_tps_x10, Some(661));

        // Local 5h / 24h aggregation
        let now_secs = parse_iso_secs("2026-11-27T10:00:00Z").unwrap();
        let sum = summarize_usage_files("agy", std::slice::from_ref(&agy_tr), now_secs);
        assert_eq!(sum.win_5h.turns, 1);
        assert_eq!(sum.win_5h.sessions, 1);
        assert_eq!(sum.win_5h.total_tokens(), 25_529);
        assert_eq!(sum.win_24h.total_tokens(), 25_529);

        // Codex rate_limits + model + speed
        let codex_home = tmp.join("codex");
        let codex_file = codex_home.join("sessions/2026/11/27/rollout-test.jsonl");
        std::fs::create_dir_all(codex_file.parent().unwrap()).unwrap();
        std::fs::write(
            &codex_file,
            concat!(
                r#"{"timestamp":"2026-11-27T09:59:00Z","type":"session_meta","payload":{"id":"cx-q1","cwd":"/repo"}}"#,
                "\n",
                r#"{"timestamp":"2026-11-27T09:59:01Z","type":"turn_context","payload":{"model":"gpt-5.3-codex"}}"#,
                "\n",
                r#"{"timestamp":"2026-11-27T09:59:01Z","type":"event_msg","payload":{"type":"user_message","message":"analyze"}}"#,
                "\n",
                r#"{"timestamp":"2026-11-27T09:59:11Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":256000,"last_token_usage":{"input_tokens":30000,"output_tokens":500}},"rate_limits":{"plan_type":"plus","primary":{"used_percent":38.4,"window_minutes":300,"resets_at":1795780000},"secondary":{"used_percent":14.0,"window_minutes":10080,"resets_at":1796300000}}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let cp = CodexProvider::with_home(codex_home);
        let md = std::fs::metadata(&codex_file).unwrap();
        let cm = cp.parse_file(&codex_file, 100, md.len()).unwrap();
        assert_eq!(cm.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(cm.last_speed_tps_x10, Some(500)); // 500 tok / 10s = 50.0 tok/s
        let q = cm.quota.expect("codex quota");
        assert_eq!(q.plan.as_deref(), Some("plus"));
        assert_eq!(q.primary.label, "5h");
        assert_eq!(q.primary.used_pct, 38);
        assert_eq!(q.primary.resets_at, Some(1795780000));
        let sec = q.secondary.expect("codex 7d quota");
        assert_eq!(sec.label, "7d");
        assert_eq!(sec.used_pct, 14);

        // Antigravity `agy -p "/usage" --output-format json` parsing + model-group selection
        let agy_usage_json = r#"{"conversation_id":"","status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.5801,"reset_time":"2026-12-03T07:01:59Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":0.8221,"reset_time":"2026-11-27T14:35:09Z"}]},{"name":"Claude and GPT models","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.4761,"reset_time":"2026-12-03T01:57:30Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":0.9927,"reset_time":"2026-11-27T14:31:21Z"}]}]}}}"#;
        let aq_gem = parse_antigravity_usage_json(agy_usage_json, Some("gemini-4-neon-high"))
            .expect("agy gemini quota");
        assert_eq!(aq_gem.groups.len(), 2);
        assert_eq!(aq_gem.plan.as_deref(), Some("Gemini Models"));
        assert_eq!(aq_gem.primary.label, "5h");
        assert_eq!(aq_gem.primary.used_pct, 18); // 82% left
        assert_eq!(aq_gem.secondary.as_ref().unwrap().label, "7d");
        assert_eq!(aq_gem.secondary.as_ref().unwrap().used_pct, 42); // 58% left

        let aq_claude = select_antigravity_quota_for_model(&aq_gem, Some("claude-opus-5-5-high"));
        assert_eq!(aq_claude.plan.as_deref(), Some("Claude and GPT models"));
        assert_eq!(aq_claude.primary.used_pct, 1); // 99% left
        assert_eq!(aq_claude.secondary.as_ref().unwrap().used_pct, 52); // 48% left

        // BillingMode detection helpers (API vs Subscription)
        let claude_api: Value = serde_json::json!({
            "env": { "ANTHROPIC_BASE_URL": "https://proxy.example.com", "ANTHROPIC_AUTH_TOKEN": "tok" }
        });
        assert!(claude_settings_uses_api(&claude_api));
        let claude_sub: Value = serde_json::json!({ "model": "claude-opus-5-5" });
        assert!(!claude_settings_uses_api(&claude_sub));
        assert!(parse_codex_toml_uses_api("model = \"gpt-5.3-codex\"\nbase_url = \"https://custom.api\"\n"));
        assert!(!parse_codex_toml_uses_api("model = \"gpt-5.3-codex\"\n"));
        assert_eq!(
            parse_codex_toml_model("model = \"gpt-5.3-codex\"\n[mcp]\nmodel = \"other\"\n").as_deref(),
            Some("gpt-5.3-codex")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
