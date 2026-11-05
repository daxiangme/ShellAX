//! Agent state detection from terminal screen text + OSC title/progress, driven by TOML manifests.
//!
//! Portions (marked [3p]) Copyright (c) herdr contributors, Apache License 2.0; modified by ShellAX.
//! See THIRD_PARTY_NOTICES.md.

use crate::i18n::t;
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => t("agent.idle"),
            Self::Working => t("agent.working"),
            Self::Blocked => t("agent.blocked"),
            Self::Unknown => t("agent.unknown"),
        }
    }
}

/// Screen snapshot plus OSC-derived strings. [3p]
#[derive(Debug, Clone, Copy)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    pub osc_title: &'a str,
    pub osc_progress: &'a str,
}

#[derive(Debug, Deserialize, Clone)]
struct AgentManifest {
    id: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[allow(dead_code)]
    version: Option<String>,
    #[allow(dead_code)]
    min_engine_version: Option<u32>,
    #[allow(dead_code)]
    updated_at: Option<String>,
    rules: Vec<ManifestRule>,
}

// [3p] manifest rule / gate schema
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestRule {
    #[allow(dead_code)]
    id: String,
    state: Option<ManifestState>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestGate {
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ManifestState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl From<ManifestState> for AgentState {
    fn from(v: ManifestState) -> Self {
        match v {
            ManifestState::Idle => Self::Idle,
            ManifestState::Working => Self::Working,
            ManifestState::Blocked => Self::Blocked,
            ManifestState::Unknown => Self::Unknown,
        }
    }
}

fn default_region() -> String {
    "whole_recent".to_string()
}

/// A loaded, compiled manifest.
pub struct Manifest {
    pub id: String,
    pub aliases: Vec<String>,
    rules: Vec<(ManifestRule, CompiledGate)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub state: AgentState,
    pub rule: Option<String>,
    pub skip_state_update: bool,
    pub visible: bool,
}

impl Manifest {
    pub fn parse(src: &str) -> Result<Self, String> {
        let m: AgentManifest = toml::from_str(src).map_err(|e| e.to_string())?;
        let rules = m
            .rules
            .into_iter()
            .map(|r| {
                let g =
                    compile_gate(&gate_from_rule(&r)).map_err(|e| format!("rule {}: {e}", r.id))?;
                Ok((r, g))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            id: m.id,
            aliases: m.aliases,
            rules,
        })
    }

    /// Highest-priority matching rule wins (first one on ties). [3p]
    pub fn detect(&self, input: DetectionInput<'_>) -> Detection {
        let mut best: Option<&ManifestRule> = None;
        for (rule, gate) in &self.rules {
            let text = region(input, &rule.region);
            if !gate_matches(gate, text, &text.to_lowercase()) {
                continue;
            }
            match best {
                Some(p) if p.priority >= rule.priority => {}
                _ => best = Some(rule),
            }
        }
        match best {
            None => Detection {
                state: AgentState::Unknown,
                rule: None,
                skip_state_update: false,
                visible: false,
            },
            Some(r) => {
                let state = r.state.map(AgentState::from).unwrap_or(AgentState::Unknown);
                let visible = match state {
                    AgentState::Idle => r.visible_idle,
                    AgentState::Working => r.visible_working,
                    AgentState::Blocked => r.visible_blocker,
                    _ => false,
                };
                Detection {
                    state,
                    rule: Some(r.id.clone()),
                    skip_state_update: r.skip_state_update,
                    visible,
                }
            }
        }
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// Bundled manifests (see agents/*.toml).
pub fn bundled() -> Vec<Manifest> {
    [
        include_str!("../agents/claude.toml"),
        include_str!("../agents/codex.toml"),
        include_str!("../agents/grok.toml"),
    ]
    .iter()
    .filter_map(|s| {
        Manifest::parse(s)
            .map_err(|e| eprintln!("agent manifest: {e}"))
            .ok()
    })
    .collect()
}

pub fn display_name(id: &str) -> &str {
    match id {
        "claude" => "Claude",
        "codex" => "Codex",
        "grok" => "Grok",
        other => other,
    }
}

/// Identify which agent (manifest index) is running. Original shellax logic:
/// 1) local PTY foreground process name (e.g. "claude", "codex", "grok");
/// 2) first word of the last command the user submitted (works over SSH);
/// 3) an OSC title that names the agent.
pub fn identify(
    manifests: &[Manifest],
    fg_process: Option<&str>,
    last_command: Option<&str>,
    title: &str,
) -> Option<usize> {
    let norm = |s: &str| s.rsplit('/').next().unwrap_or(s).to_ascii_lowercase();
    let by_name = |name: &str| manifests.iter().position(|m| m.names().any(|n| n == name));
    if let Some(p) = fg_process {
        if let Some(i) = by_name(&norm(p)) {
            return Some(i);
        }
    }
    if let Some(cmd) = last_command {
        let mut words = cmd
            .split_whitespace()
            .skip_while(|w| w.contains('=') || *w == "sudo" || *w == "npx" || *w == "exec");
        if let Some(w) = words.next() {
            if let Some(i) = by_name(&norm(w)) {
                return Some(i);
            }
        }
    }
    let t = title.to_ascii_lowercase();
    manifests.iter().position(|m| {
        m.names().any(|n| {
            t.split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .any(|w| w == n)
        })
    })
}

// ------------------------------------------------------------------ [3p] gate compile/match

fn gate_from_rule(r: &ManifestRule) -> ManifestGate {
    ManifestGate {
        all: r.all.clone(),
        any: r.any.clone(),
        not_gate: r.not_gate.clone(),
        contains: r.contains.clone(),
        regex: r.regex.clone(),
        line_regex: r.line_regex.clone(),
    }
}

fn compile_gate(g: &ManifestGate) -> Result<CompiledGate, String> {
    let re = |v: &Vec<String>| {
        v.iter()
            .map(|p| Regex::new(p).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()
    };
    Ok(CompiledGate {
        all: g.all.iter().map(compile_gate).collect::<Result<_, _>>()?,
        any: g.any.iter().map(compile_gate).collect::<Result<_, _>>()?,
        not_gate: g
            .not_gate
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        contains: g.contains.iter().map(|n| n.to_lowercase()).collect(),
        regex: re(&g.regex)?,
        line_regex: re(&g.line_regex)?,
    })
}

fn gate_matches(g: &CompiledGate, text: &str, lower: &str) -> bool {
    g.contains.iter().all(|n| lower.contains(n.as_str()))
        && g.regex.iter().all(|r| r.is_match(text))
        && g.line_regex
            .iter()
            .all(|r| text.lines().any(|l| r.is_match(l)))
        && g.all.iter().all(|n| gate_matches(n, text, lower))
        && (g.any.is_empty() || g.any.iter().any(|n| gate_matches(n, text, lower)))
        && !g.not_gate.iter().any(|n| gate_matches(n, text, lower))
}

// ------------------------------------------------------------------ [3p] regions

fn region<'a>(input: DetectionInput<'a>, spec: &str) -> &'a str {
    let spec = spec.trim();
    match spec {
        "osc_title" => return input.osc_title,
        "osc_progress" => return input.osc_progress,
        _ => {}
    }
    let c = input.screen;
    match spec {
        "whole_recent" => c,
        "after_last_prompt_marker" => after_last_prompt_marker(c),
        "before_current_prompt_marker" => before_current_prompt_marker(c),
        "whole_recent_without_current_prompt_marker" => {
            if current_codex_prompt_index(&c.lines().collect::<Vec<_>>()).is_some() {
                ""
            } else {
                c
            }
        }
        "current_prompt_block_marker" => current_prompt_block_marker(c).unwrap_or(""),
        "after_current_prompt_block_marker" => after_current_prompt_block_marker(c).unwrap_or(""),
        "prompt_box_body" => prompt_box_body(c).unwrap_or(""),
        "above_prompt_box" => above_prompt_box(c),
        "last_non_empty_above_prompt_box" => last_non_empty_line(above_prompt_box(c)),
        "after_last_horizontal_rule" => after_last_horizontal_rule(c),
        _ => {
            if let Some(n) = region_count(spec, "bottom_lines") {
                return bottom_lines(c, n);
            }
            if let Some(n) = region_count(spec, "bottom_non_empty_lines") {
                return bottom_non_empty_lines(c, n);
            }
            if let Some(n) = region_count(spec, "top_non_empty_lines").filter(|n| *n > 0) {
                return top_non_empty_lines(c, n);
            }
            ""
        }
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')?
        .parse()
        .ok()
}
fn bottom_lines(c: &str, n: usize) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    slice_from_line(c, &lines, lines.len().saturating_sub(n))
}
fn bottom_non_empty_lines(c: &str, n: usize) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    let Some(start) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, l)| !l.trim().is_empty())
        .take(n)
        .last()
        .map(|(i, _)| i)
    else {
        return "";
    };
    slice_from_line(c, &lines, start)
}
fn top_non_empty_lines(c: &str, n: usize) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    let Some(end) = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .take(n)
        .last()
        .map(|(i, _)| i)
    else {
        return "";
    };
    &c[..line_start(c, &lines, end + 1)]
}
fn codex_prompt_line(l: &str) -> bool {
    l == "›" || l.starts_with("› ")
}
fn codex_block_marker_line(l: &str) -> bool {
    l.starts_with('•') || l.starts_with('■') || l.starts_with('✗') || l.starts_with('✓')
}
fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let i = lines.iter().rposition(|l| codex_prompt_line(l))?;
    if lines[i + 1..].iter().any(|l| codex_block_marker_line(l)) {
        None
    } else {
        Some(i)
    }
}
fn after_last_prompt_marker(c: &str) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    match lines.iter().rposition(|l| codex_prompt_line(l)) {
        Some(i) => slice_from_line(c, &lines, i + 1),
        None => c,
    }
}
fn before_current_prompt_marker(c: &str) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    match current_codex_prompt_index(&lines) {
        Some(i) => &c[..line_start(c, &lines, i)],
        None => c,
    }
}
fn current_prompt_block_marker(c: &str) -> Option<&str> {
    let lines: Vec<&str> = c.lines().collect();
    let p = current_codex_prompt_index(&lines)?;
    lines[..p]
        .iter()
        .rev()
        .find(|l| codex_block_marker_line(l))
        .copied()
}
fn after_current_prompt_block_marker(c: &str) -> Option<&str> {
    let lines: Vec<&str> = c.lines().collect();
    let p = current_codex_prompt_index(&lines)?;
    let b = lines[..p]
        .iter()
        .rposition(|l| codex_block_marker_line(l))?;
    Some(slice_from_line(c, &lines, b))
}
fn is_horizontal_rule(line: &str) -> bool {
    let t = line.trim();
    let n = t.chars().take_while(|&ch| ch == '─').count();
    if n == 0 {
        return false;
    }
    let bytes = t.char_indices().nth(n).map(|(i, _)| i).unwrap_or(t.len());
    t[bytes..].trim_start().is_empty() || n >= 3
}
fn prompt_box_top(lines: &[&str]) -> Option<usize> {
    let mut seen = 0;
    for i in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[i]) {
            seen += 1;
            if seen == 2 {
                return Some(i);
            }
        }
    }
    None
}
fn prompt_box_body(c: &str) -> Option<&str> {
    let lines: Vec<&str> = c.lines().collect();
    let top = prompt_box_top(&lines)?;
    let start = line_start(c, &lines, top + 1);
    let end_i = lines[top + 1..]
        .iter()
        .position(|l| is_horizontal_rule(l))
        .map(|r| top + 1 + r)
        .unwrap_or(lines.len());
    let end = line_start(c, &lines, end_i);
    Some(&c[start.min(c.len())..end.min(c.len())])
}
fn above_prompt_box(c: &str) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    match prompt_box_top(&lines) {
        Some(t) => &c[..line_start(c, &lines, t)],
        None => c,
    }
}
fn after_last_horizontal_rule(c: &str) -> &str {
    let (mut last, mut off) = (0usize, 0usize);
    for l in c.lines() {
        let next = off + l.len() + 1;
        if is_horizontal_rule(l) {
            last = next.min(c.len());
        }
        off = next;
    }
    &c[last..]
}
fn last_non_empty_line(c: &str) -> &str {
    c.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")
}
fn slice_from_line<'a>(c: &'a str, lines: &[&str], i: usize) -> &'a str {
    &c[line_start(c, lines, i).min(c.len())..]
}
fn line_start(c: &str, lines: &[&str], i: usize) -> usize {
    lines[..i.min(lines.len())]
        .iter()
        .map(|l| l.len() + 1)
        .sum::<usize>()
        .min(c.len())
}

/// One conversation shown in Claude's `agents` screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedSession {
    pub title: String,
    pub status: String,
    pub ago: String,
    pub needs: bool,
}

/// Claude's agents overview: sessions grouped later by `cwd`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentsBoard {
    pub cwd: Option<String>,
    pub sessions: Vec<ListedSession>,
    pub state: AgentState,
}

/// Parse the Claude Code agents overview. None when this screen is a normal shell or chat.
pub fn parse_claude_agents(screen: &str) -> Option<AgentsBoard> {
    if !is_agents_overview(screen) {
        return None;
    }
    let mut section = Section::Other;
    let mut sessions = Vec::new();
    for line in screen.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if eq_heading(trimmed, "needs input") {
            section = Section::Needs;
            continue;
        }
        if eq_heading(trimmed, "working") {
            section = Section::Working;
            continue;
        }
        if eq_heading(trimmed, "completed") {
            section = Section::Done;
            continue;
        }
        if let Some((title, status, ago)) = session_line(trimmed) {
            sessions.push(ListedSession {
                title,
                status,
                ago,
                needs: section == Section::Needs,
            });
        }
    }
    Some(AgentsBoard {
        cwd: directory_in(screen),
        sessions,
        state: rollup_state(screen),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Other,
    Needs,
    Working,
    Done,
}

fn is_agents_overview(screen: &str) -> bool {
    if screen.to_lowercase().contains("awaiting input") {
        return true;
    }
    screen.lines().any(|line| {
        let line = line.trim();
        eq_heading(line, "needs input")
            || eq_heading(line, "working")
            || eq_heading(line, "completed")
    })
}

fn eq_heading(line: &str, heading: &str) -> bool {
    line.trim_matches(|c: char| c == '*' || c == '#' || c.is_whitespace())
        .eq_ignore_ascii_case(heading)
}

fn session_line(line: &str) -> Option<(String, String, String)> {
    let rest = strip_bullet(line)?.trim();
    if rest.len() < 2 || rest.to_lowercase().starts_with("sessions ") {
        return None;
    }
    let (title, status) = split_columns(rest);
    let (title, ago) = split_ago(&title);
    let (status, ago) = if ago.is_empty() {
        split_ago(&status)
    } else {
        (status, ago)
    };
    if title.is_empty() {
        return None;
    }
    Some((title, status, ago))
}

fn split_columns(text: &str) -> (String, String) {
    let mut parts = text.split("  ").filter(|p| !p.trim().is_empty());
    let title = parts.next().unwrap_or("").trim().to_string();
    let status = parts.map(str::trim).collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        (text.trim().to_string(), String::new())
    } else {
        (title, status)
    }
}

fn strip_bullet(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    let first = chars.next()?;
    if is_marker(first) {
        Some(chars.as_str())
    } else {
        None
    }
}

/// Claude's agents list marks each conversation with a star (`*` or `✻`), not the window title.
fn is_marker(c: char) -> bool {
    matches!(
        c,
        '*' | '＊' | '﹡' | '∗' | '✱' | '•' | '·' | '●' | '◦' | '∙' | '⋅' | '▸' | '▶' | '►'
            | '▹' | '‣' | '⁃'
    ) || ('\u{2731}'..='\u{274B}').contains(&c)
}

fn is_ago(token: &str) -> bool {
    let bytes = token.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    let unit = bytes[bytes.len() - 1];
    matches!(unit, b's' | b'm' | b'h' | b'd' | b'S' | b'M' | b'H' | b'D')
        && bytes[..bytes.len() - 1].iter().all(|c| c.is_ascii_digit())
}

fn split_ago(text: &str) -> (String, String) {
    let trimmed = text.trim();
    if is_ago(trimmed) {
        return (String::new(), trimmed.to_string());
    }
    let bytes = text.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    let end = i;
    while i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        i -= 1;
    }
    if i < end && i > 0 && bytes[i - 1].is_ascii_whitespace() {
        let token = &text[i..end];
        let unit = token.as_bytes()[token.len() - 1];
        if matches!(unit, b's' | b'm' | b'h' | b'd' | b'S' | b'M' | b'H' | b'D')
            && token[..token.len() - 1]
                .chars()
                .all(|c| c.is_ascii_digit())
        {
            let title = text[..i].trim().to_string();
            if !title.is_empty() {
                return (title, token.to_string());
            }
        }
    }
    (text.trim().to_string(), String::new())
}

fn directory_in(screen: &str) -> Option<String> {
    for line in screen.lines().take(12) {
        if let Some(path) = windows_path(line).or_else(|| unix_path(line)) {
            return Some(path);
        }
        if line.to_lowercase().contains("awaiting input") {
            break;
        }
    }
    None
}

fn windows_path(line: &str) -> Option<String> {
    let b = line.as_bytes();
    let mut i = 0;
    while i + 2 < b.len() {
        if b[i].is_ascii_alphabetic() && b[i + 1] == b':' && (b[i + 2] == b'\\' || b[i + 2] == b'/')
        {
            let rest = &line[i..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '·')
                .unwrap_or(rest.len());
            return Some(rest[..end].trim_end_matches(['\\', '/']).to_string());
        }
        i += 1;
    }
    None
}

fn unix_path(line: &str) -> Option<String> {
    let start = line.find(" /")?;
    let rest = line[start + 1..].trim_start();
    if !rest.starts_with('/') {
        return None;
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '·')
        .unwrap_or(rest.len());
    let path = rest[..end].trim_end_matches('/');
    (path.len() > 1).then(|| path.to_string())
}

fn rollup_state(screen: &str) -> AgentState {
    let lower = screen.to_lowercase();
    let awaiting = number_before(&lower, "awaiting input").unwrap_or(0);
    let working = number_before(&lower, "working").unwrap_or(0);
    if awaiting > 0 {
        AgentState::Blocked
    } else if working > 0 {
        AgentState::Working
    } else {
        AgentState::Idle
    }
}

/// Shell command that opens one Claude background session by its list name.
pub fn attach_command(title: &str, powershell: bool) -> Option<String> {
    let title = title.trim();
    if title.is_empty() || title.chars().any(|c| c.is_control()) {
        return None;
    }
    let quoted = if powershell {
        format!("'{}'", title.replace('\'', "''"))
    } else {
        format!("'{}'", title.replace('\'', "'\\''"))
    };
    Some(format!("claude attach {quoted}"))
}

/// Context already used by a Claude session, such as `5%`. Read from Claude's local
/// session record. None when this name has no record in `cwd`.
pub fn claude_context_label(name: &str, cwd: &str) -> Option<String> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    struct Slot {
        at: Instant,
        map: std::collections::HashMap<String, String>,
    }
    static CACHE: Mutex<Option<Slot>> = Mutex::new(None);
    let mut guard = CACHE.lock().ok()?;
    let fresh = guard
        .as_ref()
        .is_some_and(|s| s.at.elapsed() < Duration::from_secs(2));
    if !fresh {
        let map = load_context_map();
        *guard = Some(Slot {
            at: Instant::now(),
            map,
        });
    }
    let map = &guard.as_ref()?.map;
    let key = context_key(name, cwd);
    if let Some(label) = map.get(&key) {
        return Some(label.clone());
    }
    if cwd.trim().is_empty() {
        let prefix = format!("{}\n", name.trim());
        let mut hits = map.iter().filter(|(k, _)| k.starts_with(&prefix));
        let first = hits.next()?;
        if hits.next().is_none() {
            return Some(first.1.clone());
        }
    }
    None
}

fn context_key(name: &str, cwd: &str) -> String {
    format!("{}\n{}", name.trim(), norm_path(cwd))
}

fn norm_path(path: &str) -> String {
    let path = path.replace('\\', "/").trim_end_matches('/').to_string();
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path
    }
}

fn jobs_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(std::path::PathBuf::from(dir).join("jobs"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(std::path::PathBuf::from(home).join(".claude").join("jobs"))
}

fn load_context_map() -> std::collections::HashMap<String, String> {
    let Some(dir) = jobs_dir() else {
        return std::collections::HashMap::new();
    };
    context_from_jobs(&dir)
}

fn context_from_jobs(jobs: &std::path::Path) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Ok(rd) = std::fs::read_dir(jobs) else {
        return map;
    };
    for ent in rd.flatten() {
        let text = std::fs::read_to_string(ent.path().join("state.json")).ok();
        let Some(text) = text else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
        let Some(transcript) = v.get("linkScanPath").and_then(|x| x.as_str()) else {
            continue;
        };
        let Some(used) = last_context_used(std::path::Path::new(transcript)) else {
            continue;
        };
        if used == 0 {
            continue;
        }
        let window = context_window(&text).max(1);
        let pct = (used.saturating_mul(100) / window).min(100);
        let label = if pct == 0 {
            "<1%".to_string()
        } else {
            format!("{pct}%")
        };
        map.insert(context_key(name, cwd), label);
    }
    map
}

/// One Claude conversation stored on this machine.
#[derive(Clone, Debug)]
pub struct ClaudeRecord {
    pub name: String,
    pub cwd: String,
    pub status: String,
    pub working: bool,
    pub needs: bool,
    pub used: u64,
    pub window: u64,
    updated: String,
}

/// Conversations whose directory is `cwd`. Empty when this folder has no session records.
pub fn claude_records_for(cwd: &str) -> Vec<ClaudeRecord> {
    if cwd.trim().is_empty() {
        return Vec::new();
    }
    let want = norm_path(cwd);
    let mut rows: Vec<_> = cached_records()
        .into_iter()
        .filter(|r| norm_path(&r.cwd) == want)
        .collect();
    rows.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.name.cmp(&b.name)));
    rows
}

/// `53338` → `53k`, `1000000` → `1M`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        let v = n as f64 / 1_000_000.0;
        if (v - v.round()).abs() < 0.05 {
            format!("{}M", v.round() as u64)
        } else {
            format!("{v:.1}M")
        }
    } else if n >= 1_000 {
        format!("{}k", ((n as f64) / 1_000.0).round() as u64)
    } else {
        n.to_string()
    }
}

fn cached_records() -> Vec<ClaudeRecord> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    struct Slot {
        at: Instant,
        rows: Vec<ClaudeRecord>,
    }
    static CACHE: Mutex<Option<Slot>> = Mutex::new(None);
    let Ok(mut guard) = CACHE.lock() else {
        return Vec::new();
    };
    let fresh = guard
        .as_ref()
        .is_some_and(|s| s.at.elapsed() < Duration::from_secs(2));
    if !fresh {
        let rows = jobs_dir().map(|d| records_from_jobs(&d)).unwrap_or_default();
        *guard = Some(Slot {
            at: Instant::now(),
            rows,
        });
    }
    guard.as_ref().map(|s| s.rows.clone()).unwrap_or_default()
}

fn records_from_jobs(jobs: &std::path::Path) -> Vec<ClaudeRecord> {
    let mut rows = Vec::new();
    let Ok(rd) = std::fs::read_dir(jobs) else {
        return rows;
    };
    for ent in rd.flatten() {
        let Ok(text) = std::fs::read_to_string(ent.path().join("state.json")) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
        if name.is_empty() {
            continue;
        }
        let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let status = v
            .get("detail")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let tempo = v.get("tempo").and_then(|x| x.as_str()).unwrap_or("");
        let state = v.get("state").and_then(|x| x.as_str()).unwrap_or("");
        let tasks = v
            .pointer("/inFlight/tasks")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let working = tasks > 0
            || tempo == "working"
            || tempo == "active"
            || state == "working";
        let needs = tempo == "blocked" || state == "blocked";
        let (used, window) = v
            .get("linkScanPath")
            .and_then(|x| x.as_str())
            .and_then(|p| last_context_used(std::path::Path::new(p)))
            .filter(|n| *n > 0)
            .map(|used| (used, context_window(&text).max(1)))
            .unwrap_or((0, 0));
        let updated = v
            .get("updatedAt")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        rows.push(ClaudeRecord {
            name: name.to_string(),
            cwd,
            status,
            working,
            needs,
            used,
            window,
            updated,
        });
    }
    rows
}

fn context_window(text: &str) -> u64 {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    let mut window = 200_000u64;
    while i + 4 < bytes.len() {
        if bytes[i] == b'[' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > start
                && j + 1 < bytes.len()
                && matches!(bytes[j], b'm' | b'k')
                && bytes[j + 1] == b']'
            {
                if let Ok(n) = lower[start..j].parse::<u64>() {
                    let size = if bytes[j] == b'm' {
                        n.saturating_mul(1_000_000)
                    } else {
                        n.saturating_mul(1_000)
                    };
                    if size >= 8_000 {
                        window = size;
                    }
                }
            }
        }
        i += 1;
    }
    window
}

fn last_context_used(path: &std::path::Path) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    let take = len.min(512 * 1024);
    file.seek(SeekFrom::Start(len - take)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    let mut last = None;
    let mut from = 0;
    while let Some(rel) = buf[from..].find("\"input_tokens\"") {
        let at = from + rel;
        let start = at.saturating_sub(240);
        let rest = &buf[start..buf.len().min(at + 360)];
        if let Some(input) = number_after(rest, "\"input_tokens\"") {
            let read = number_after(rest, "\"cache_read_input_tokens\"").unwrap_or(0);
            let create = number_after(rest, "\"cache_creation_input_tokens\"").unwrap_or(0);
            last = Some(input + read + create);
        }
        from = at + 14;
    }
    last
}

fn number_after(text: &str, key: &str) -> Option<u64> {
    let at = text.find(key)?;
    let rest = text[at + key.len()..].trim_start_matches([':', ' ', '"']);
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn number_before(text: &str, marker: &str) -> Option<u32> {
    let at = text.find(marker)?;
    let head = text[..at].trim_end();
    let digits: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inp<'a>(screen: &'a str, title: &'a str) -> DetectionInput<'a> {
        DetectionInput {
            screen,
            osc_title: title,
            osc_progress: "",
        }
    }

    #[test]
    fn claude_agents_screen_lists_sessions_by_directory() {
        let screen = "\
Claude Code v2.1.314
New sessions: deepseek-flash[1m] · high effort · D:\\Workspace\\qkchi\\air2
0 awaiting input · 0 working · 1 completed

Needs input
Sessions that have a question or need your decision land here

Working
Sessions Claude is actively working on

Completed
Finished sessions wait here for you to review
* greeting response 已回复问候，等待用户提出具体任务。          1s
";
        let board = parse_claude_agents(screen).unwrap();
        assert_eq!(board.cwd.as_deref(), Some(r"D:\Workspace\qkchi\air2"));
        assert_eq!(board.state, AgentState::Idle);
        assert_eq!(board.sessions.len(), 1);
        assert!(board.sessions[0].title.contains("greeting response"));
        assert_eq!(board.sessions[0].ago, "1s");
        assert!(!board.sessions[0].needs);
        let two = "\
Completed
* 回复 好的老板          待命中，等待下一步指令
* greeting response     已回复问候，等待用户提出具体任务。
";
        let board = parse_claude_agents(two).unwrap();
        assert_eq!(board.sessions.len(), 2);
        assert_eq!(board.sessions[0].title, "回复 好的老板");
        assert!(board.sessions[0].status.contains("待命中"));
        assert_eq!(board.sessions[1].title, "greeting response");
        assert!(board.sessions[1].status.contains("已回复问候"));
        assert!(parse_claude_agents("user@host:~$ ").is_none());
        let live = "\
 ▐▛███▛█   Claude Code v2.1.318
▝▜██████▀  New sessions: deepseek-flash[1m] · high effort · D:\\Workspace\\qkchi\\air2
 ▝▝   ▝▝   0 awaiting input · 0 working · 2 completed

Completed
✻ standby acknowledgment  待命中，等待下一步指令                                                             2s
✻ greeting response       已回复问候，等待用户提出具体任务。                                                 1s

❯ describe a task for a new session
  enter to open · space to reply
";
        let board = parse_claude_agents(live).unwrap();
        assert_eq!(board.cwd.as_deref(), Some(r"D:\Workspace\qkchi\air2"));
        assert_eq!(board.sessions.len(), 2);
        assert_eq!(board.sessions[0].title, "standby acknowledgment");
        assert!(board.sessions[0].status.contains("待命中"));
        assert_eq!(board.sessions[0].ago, "2s");
        assert_eq!(board.sessions[1].title, "greeting response");
        assert!(board.sessions[1].status.contains("已回复问候"));
        assert_eq!(board.sessions[1].ago, "1s");
        assert!(board.sessions.iter().all(|s| !s.title.contains("claude agents")));
        assert!(board.sessions.iter().all(|s| !s.title.contains("describe a task")));
    }

    #[test]
    fn bundled_manifests_compile() {
        let m = bundled();
        assert_eq!(
            m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["claude", "codex", "grok"]
        );
    }
    #[test]
    fn identify_by_process_command_and_title() {
        let m = bundled();
        assert_eq!(identify(&m, Some("claude"), None, ""), Some(0));
        assert_eq!(
            identify(&m, Some("bash"), Some("FOO=1 codex --full-auto"), ""),
            Some(1)
        );
        assert_eq!(
            identify(&m, None, Some("ls -la"), "⠋ Thinking - grok"),
            Some(2)
        );
        assert_eq!(
            identify(&m, Some("bash"), Some("vim a.txt"), "user@host: ~"),
            None
        );
    }
    #[test]
    fn grok_states_from_title() {
        let m = bundled();
        let g = &m[2];
        assert_eq!(
            g.detect(inp("", "⠙ Reading files - grok")).state,
            AgentState::Working
        );
        assert_eq!(g.detect(inp("", "grok")).state, AgentState::Idle);
        assert_eq!(
            g.detect(inp("", "⚠ Action Required - grok")).state,
            AgentState::Blocked
        );
    }
    #[test]
    fn claude_states_from_screen() {
        let m = bundled();
        let c = &m[0];
        assert_eq!(c.detect(inp("", "⠂ Fix tests")).state, AgentState::Working);
        let blocked = "Bash command\n  rm -rf build\nDo you want to proceed?\n❯ 1. Yes\n  2. No\n";
        assert_eq!(c.detect(inp(blocked, "")).state, AgentState::Blocked);
        assert_eq!(c.detect(inp("", "✳ Claude Code")).state, AgentState::Idle);
    }

    #[test]
    fn attach_quotes_the_session_name() {
        assert_eq!(
            attach_command("standby acknowledgment", true).as_deref(),
            Some("claude attach 'standby acknowledgment'")
        );
        assert_eq!(
            attach_command("it's mine", false).as_deref(),
            Some("claude attach 'it'\\''s mine'")
        );
        assert!(attach_command("a\nb", true).is_none());
    }

    #[test]
    fn context_percent_comes_from_the_session_record() {
        let dir = std::env::temp_dir().join(format!("shellax-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let job = dir.join("jobs").join("abc");
        std::fs::create_dir_all(&job).unwrap();
        let transcript = dir.join("session.jsonl");
        std::fs::write(
            &transcript,
            "{\"message\":{\"usage\":{\"input_tokens\":40000,\"cache_read_input_tokens\":10000,\"cache_creation_input_tokens\":0}}}\n",
        )
        .unwrap();
        let state = format!(
            "{{\"name\":\"greeting response\",\"cwd\":\"D:\\\\Workspace\\\\qkchi\\\\air2\",\"model\":\"deepseek-flash[1M]\",\"linkScanPath\":{}}}",
            serde_json::to_string(&transcript).unwrap()
        );
        std::fs::write(job.join("state.json"), state).unwrap();
        let map = context_from_jobs(&dir.join("jobs"));
        let key = context_key("greeting response", r"D:\Workspace\qkchi\air2");
        assert_eq!(map.get(&key).map(String::as_str), Some("5%"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_stay_listed_for_the_directory() {
        let dir = std::env::temp_dir().join(format!("shellax-recs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let job = dir.join("abc");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(
            job.join("state.json"),
            r#"{"name":"greeting response","cwd":"D:\\Workspace\\qkchi\\air2","detail":"已回复问候","tempo":"working","state":"working","updatedAt":"2026-11-05T08:00:00Z","inFlight":{"tasks":1}}"#,
        )
        .unwrap();
        let rows = records_from_jobs(&dir);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "greeting response");
        assert!(rows[0].working);
        assert_eq!(rows[0].status, "已回复问候");
        assert_eq!(format_tokens(53_338), "53k");
        assert_eq!(format_tokens(1_000_000), "1M");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
