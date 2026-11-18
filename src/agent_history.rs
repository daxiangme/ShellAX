//! Local coding-agent conversation history scanner and incremental index cache.
//!
//! Scans Claude Code (`~/.claude/projects/*/*.jsonl`) and Codex (`~/.codex/sessions/**/*.jsonl`)
//! using bounded head/tail reads (never loading multi-MB transcripts into memory), caches metadata
//! by `(mtime, size)` in `<data_dir>/agent-history.json`, and groups sessions by project.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 128 * 1024;
const CACHE_VERSION: u32 = 3;

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
}

impl SessionMeta {
    /// Context window fill `(0.0..=1.0)` and compact label (`"42%"` / `"12k"`), if known.
    pub fn context_ring_info(&self) -> Option<(f32, String)> {
        let used = self.ctx_used.filter(|&u| u > 0)?;
        let mut win = self.ctx_window.unwrap_or(200_000).max(1);
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
        let label = if pct >= 1 {
            format!("{pct}%")
        } else {
            crate::agent::format_tokens(used)
        };
        Some((frac, label))
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

fn claude_model_context_window(model: &str) -> u64 {
    let ml = model.to_ascii_lowercase();
    if ml.contains("[2m]") || ml.contains("2m") {
        2_000_000
    } else if ml.contains("[1m]")
        || ml.contains("1m")
        || ml.contains("opus-4-6")
        || ml.contains("opus-4.6")
        || ml.contains("opus-4-5")
        || ml.contains("opus-4.5")
        || ml.contains("opus-5")
        || ml.contains("sonnet-4-6")
        || ml.contains("sonnet-4.6")
        || ml.contains("sonnet-4-5")
        || ml.contains("sonnet-4.5")
        || ml.contains("sonnet-5")
    {
        1_000_000
    } else {
        200_000
    }
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
        let mut ctx_used: Option<u64> = None;
        let mut ctx_window: Option<u64> = None;

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
                    if v.get("isMeta").and_then(Value::as_bool) != Some(true) {
                        if let Some(text) = extract_claude_message_text(&v)
                            .as_deref()
                            .and_then(clean_prompt_snippet)
                        {
                            if first_prompt.is_none() {
                                first_prompt = Some(text.clone());
                            }
                            last_prompt = Some(text);
                        }
                    }
                }
                Some("assistant") => {
                    sampled_msgs += 1;
                    if let Some(msg) = v.get("message") {
                        if let Some(model) = msg.get("model").and_then(Value::as_str) {
                            let mw = claude_model_context_window(model);
                            ctx_window = Some(ctx_window.unwrap_or(0).max(mw));
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
                                ctx_used = Some(sum);
                            }
                        }
                    }
                }
                _ => {}
            }
            true
        };

        for line in &head {
            if !inspect(line) {
                return None;
            }
        }
        for line in &tail {
            if !inspect(line) {
                return None;
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
            ctx_used,
            ctx_window,
        })
    }
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
        let mut ctx_used: Option<u64> = None;
        let mut ctx_window: Option<u64> = None;

        let mut inspect = |line: &str| {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                return;
            };
            if let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(parse_iso_secs) {
                if started.is_none_or(|s| ts < s) {
                    started = Some(ts);
                }
                if updated.is_none_or(|u| ts > u) {
                    updated = Some(ts);
                }
            }
            match v.get("type").and_then(Value::as_str) {
                Some("session_meta") => {
                    if let Some(p) = v.get("payload") {
                        if let Some(id) = p.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            session_id = Some(id.to_string());
                        }
                        if let Some(c) = p.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            cwd = Some(c.to_string());
                        }
                        if let Some(b) = p
                            .get("git")
                            .and_then(|g| g.get("branch"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            branch = Some(b.to_string());
                        }
                    }
                }
                Some("event_msg") => {
                    if let Some(p) = v.get("payload") {
                        match p.get("type").and_then(Value::as_str) {
                            Some("user_message") => {
                                sampled_msgs += 1;
                                if let Some(m) = p
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .and_then(clean_prompt_snippet)
                                {
                                    if first_prompt.is_none() {
                                        first_prompt = Some(m.clone());
                                    }
                                    last_prompt = Some(m);
                                }
                            }
                            Some("token_count") => {
                                if let Some(info) = p.get("info") {
                                    if let Some(win) = info
                                        .get("model_context_window")
                                        .and_then(Value::as_u64)
                                        .filter(|&w| w > 0)
                                    {
                                        ctx_window = Some(win);
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
                                        ctx_used = Some(u);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Some("response_item") => {
                    if let Some(p) = v.get("payload") {
                        let role = p.get("role").and_then(Value::as_str);
                        if matches!(role, Some("user" | "assistant")) {
                            sampled_msgs += 1;
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
                                                first_prompt = Some(clean.clone());
                                            }
                                            last_prompt = Some(clean);
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
                        session_id = v
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                    }
                    if cwd.is_none() {
                        cwd = v
                            .get("cwd")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                    }
                }
                _ => {}
            }
        };

        for line in &head {
            inspect(line);
        }
        for line in &tail {
            inspect(line);
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
            ctx_used,
            ctx_window,
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
    let pb_path = home.join("conversations").join(format!("{id}.pb"));
    let path = if pb_path.exists() {
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
        ctx_used: None,
        ctx_window: None,
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

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
