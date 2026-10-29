//! Agent state detection from terminal screen text + OSC title/progress, driven by TOML manifests.
//!
//! Portions (marked [3p]) Copyright (c) herdr contributors, Apache License 2.0; modified by ShellAX.
//! See THIRD_PARTY_NOTICES.md.

use crate::i18n::t;
use regex::Regex;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState { Idle, Working, Blocked, Unknown }

impl AgentState {
    pub fn label(self) -> &'static str {
        match self { Self::Idle => t("agent.idle"), Self::Working => t("agent.working"), Self::Blocked => t("agent.blocked"), Self::Unknown => t("agent.unknown") }
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
    #[serde(default)] visible_idle: bool,
    #[serde(default)] visible_blocker: bool,
    #[serde(default)] visible_working: bool,
    #[serde(default)] skip_state_update: bool,
    #[serde(default)] all: Vec<ManifestGate>,
    #[serde(default)] any: Vec<ManifestGate>,
    #[serde(default, rename = "not")] not_gate: Vec<ManifestGate>,
    #[serde(default)] contains: Vec<String>,
    #[serde(default)] regex: Vec<String>,
    #[serde(default)] line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestGate {
    #[serde(default)] all: Vec<ManifestGate>,
    #[serde(default)] any: Vec<ManifestGate>,
    #[serde(default, rename = "not")] not_gate: Vec<ManifestGate>,
    #[serde(default)] contains: Vec<String>,
    #[serde(default)] regex: Vec<String>,
    #[serde(default)] line_regex: Vec<String>,
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
enum ManifestState { Idle, Working, Blocked, Unknown }

impl From<ManifestState> for AgentState {
    fn from(v: ManifestState) -> Self {
        match v { ManifestState::Idle => Self::Idle, ManifestState::Working => Self::Working, ManifestState::Blocked => Self::Blocked, ManifestState::Unknown => Self::Unknown }
    }
}

fn default_region() -> String { "whole_recent".to_string() }

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
        let rules = m.rules.into_iter().map(|r| {
            let g = compile_gate(&gate_from_rule(&r)).map_err(|e| format!("rule {}: {e}", r.id))?;
            Ok((r, g))
        }).collect::<Result<Vec<_>, String>>()?;
        Ok(Self { id: m.id, aliases: m.aliases, rules })
    }

    /// Highest-priority matching rule wins (first one on ties). [3p]
    pub fn detect(&self, input: DetectionInput<'_>) -> Detection {
        let mut best: Option<&ManifestRule> = None;
        for (rule, gate) in &self.rules {
            let text = region(input, &rule.region);
            if !gate_matches(gate, text, &text.to_lowercase()) { continue; }
            match best { Some(p) if p.priority >= rule.priority => {}, _ => best = Some(rule) }
        }
        match best {
            None => Detection { state: AgentState::Unknown, rule: None, skip_state_update: false, visible: false },
            Some(r) => {
                let state = r.state.map(AgentState::from).unwrap_or(AgentState::Unknown);
                let visible = match state { AgentState::Idle => r.visible_idle, AgentState::Working => r.visible_working, AgentState::Blocked => r.visible_blocker, _ => false };
                Detection { state, rule: Some(r.id.clone()), skip_state_update: r.skip_state_update, visible }
            }
        }
    }

    fn names(&self) -> impl Iterator<Item = &str> { std::iter::once(self.id.as_str()).chain(self.aliases.iter().map(String::as_str)) }
}

/// Bundled manifests (see agents/*.toml).
pub fn bundled() -> Vec<Manifest> {
    [include_str!("../agents/claude.toml"), include_str!("../agents/codex.toml"), include_str!("../agents/grok.toml")]
        .iter().filter_map(|s| Manifest::parse(s).map_err(|e| eprintln!("agent manifest: {e}")).ok()).collect()
}

pub fn display_name(id: &str) -> &str {
    match id { "claude" => "Claude", "codex" => "Codex", "grok" => "Grok", other => other }
}

/// Identify which agent (manifest index) is running. Original shellax logic:
/// 1) local PTY foreground process name (e.g. "claude", "codex", "grok");
/// 2) first word of the last command the user submitted (works over SSH);
/// 3) an OSC title that names the agent.
pub fn identify(manifests: &[Manifest], fg_process: Option<&str>, last_command: Option<&str>, title: &str) -> Option<usize> {
    let norm = |s: &str| s.rsplit('/').next().unwrap_or(s).to_ascii_lowercase();
    let by_name = |name: &str| manifests.iter().position(|m| m.names().any(|n| n == name));
    if let Some(p) = fg_process { if let Some(i) = by_name(&norm(p)) { return Some(i); } }
    if let Some(cmd) = last_command {
        let mut words = cmd.split_whitespace().skip_while(|w| w.contains('=') || *w == "sudo" || *w == "npx" || *w == "exec");
        if let Some(w) = words.next() { if let Some(i) = by_name(&norm(w)) { return Some(i); } }
    }
    let t = title.to_ascii_lowercase();
    manifests.iter().position(|m| m.names().any(|n| t.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').any(|w| w == n)))
}

// ------------------------------------------------------------------ [3p] gate compile/match

fn gate_from_rule(r: &ManifestRule) -> ManifestGate {
    ManifestGate { all: r.all.clone(), any: r.any.clone(), not_gate: r.not_gate.clone(), contains: r.contains.clone(), regex: r.regex.clone(), line_regex: r.line_regex.clone() }
}

fn compile_gate(g: &ManifestGate) -> Result<CompiledGate, String> {
    let re = |v: &Vec<String>| v.iter().map(|p| Regex::new(p).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>();
    Ok(CompiledGate {
        all: g.all.iter().map(compile_gate).collect::<Result<_, _>>()?,
        any: g.any.iter().map(compile_gate).collect::<Result<_, _>>()?,
        not_gate: g.not_gate.iter().map(compile_gate).collect::<Result<_, _>>()?,
        contains: g.contains.iter().map(|n| n.to_lowercase()).collect(),
        regex: re(&g.regex)?,
        line_regex: re(&g.line_regex)?,
    })
}

fn gate_matches(g: &CompiledGate, text: &str, lower: &str) -> bool {
    g.contains.iter().all(|n| lower.contains(n.as_str()))
        && g.regex.iter().all(|r| r.is_match(text))
        && g.line_regex.iter().all(|r| text.lines().any(|l| r.is_match(l)))
        && g.all.iter().all(|n| gate_matches(n, text, lower))
        && (g.any.is_empty() || g.any.iter().any(|n| gate_matches(n, text, lower)))
        && !g.not_gate.iter().any(|n| gate_matches(n, text, lower))
}

// ------------------------------------------------------------------ [3p] regions

fn region<'a>(input: DetectionInput<'a>, spec: &str) -> &'a str {
    let spec = spec.trim();
    match spec { "osc_title" => return input.osc_title, "osc_progress" => return input.osc_progress, _ => {} }
    let c = input.screen;
    match spec {
        "whole_recent" => c,
        "after_last_prompt_marker" => after_last_prompt_marker(c),
        "before_current_prompt_marker" => before_current_prompt_marker(c),
        "whole_recent_without_current_prompt_marker" => if current_codex_prompt_index(&c.lines().collect::<Vec<_>>()).is_some() { "" } else { c },
        "current_prompt_block_marker" => current_prompt_block_marker(c).unwrap_or(""),
        "after_current_prompt_block_marker" => after_current_prompt_block_marker(c).unwrap_or(""),
        "prompt_box_body" => prompt_box_body(c).unwrap_or(""),
        "above_prompt_box" => above_prompt_box(c),
        "last_non_empty_above_prompt_box" => last_non_empty_line(above_prompt_box(c)),
        "after_last_horizontal_rule" => after_last_horizontal_rule(c),
        _ => {
            if let Some(n) = region_count(spec, "bottom_lines") { return bottom_lines(c, n); }
            if let Some(n) = region_count(spec, "bottom_non_empty_lines") { return bottom_non_empty_lines(c, n); }
            if let Some(n) = region_count(spec, "top_non_empty_lines").filter(|n| *n > 0) { return top_non_empty_lines(c, n); }
            ""
        }
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)?.strip_prefix('(')?.strip_suffix(')')?.parse().ok()
}
fn bottom_lines(c: &str, n: usize) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    slice_from_line(c, &lines, lines.len().saturating_sub(n))
}
fn bottom_non_empty_lines(c: &str, n: usize) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    let Some(start) = lines.iter().enumerate().rev().filter(|(_, l)| !l.trim().is_empty()).take(n).last().map(|(i, _)| i) else { return "" };
    slice_from_line(c, &lines, start)
}
fn top_non_empty_lines(c: &str, n: usize) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    let Some(end) = lines.iter().enumerate().filter(|(_, l)| !l.trim().is_empty()).take(n).last().map(|(i, _)| i) else { return "" };
    &c[..line_start(c, &lines, end + 1)]
}
fn codex_prompt_line(l: &str) -> bool { l == "›" || l.starts_with("› ") }
fn codex_block_marker_line(l: &str) -> bool { l.starts_with('•') || l.starts_with('■') || l.starts_with('✗') || l.starts_with('✓') }
fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let i = lines.iter().rposition(|l| codex_prompt_line(l))?;
    if lines[i + 1..].iter().any(|l| codex_block_marker_line(l)) { None } else { Some(i) }
}
fn after_last_prompt_marker(c: &str) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    match lines.iter().rposition(|l| codex_prompt_line(l)) { Some(i) => slice_from_line(c, &lines, i + 1), None => c }
}
fn before_current_prompt_marker(c: &str) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    match current_codex_prompt_index(&lines) { Some(i) => &c[..line_start(c, &lines, i)], None => c }
}
fn current_prompt_block_marker(c: &str) -> Option<&str> {
    let lines: Vec<&str> = c.lines().collect();
    let p = current_codex_prompt_index(&lines)?;
    lines[..p].iter().rev().find(|l| codex_block_marker_line(l)).copied()
}
fn after_current_prompt_block_marker(c: &str) -> Option<&str> {
    let lines: Vec<&str> = c.lines().collect();
    let p = current_codex_prompt_index(&lines)?;
    let b = lines[..p].iter().rposition(|l| codex_block_marker_line(l))?;
    Some(slice_from_line(c, &lines, b))
}
fn is_horizontal_rule(line: &str) -> bool {
    let t = line.trim();
    let n = t.chars().take_while(|&ch| ch == '─').count();
    if n == 0 { return false; }
    let bytes = t.char_indices().nth(n).map(|(i, _)| i).unwrap_or(t.len());
    t[bytes..].trim_start().is_empty() || n >= 3
}
fn prompt_box_top(lines: &[&str]) -> Option<usize> {
    let mut seen = 0;
    for i in (0..lines.len()).rev() { if is_horizontal_rule(lines[i]) { seen += 1; if seen == 2 { return Some(i); } } }
    None
}
fn prompt_box_body(c: &str) -> Option<&str> {
    let lines: Vec<&str> = c.lines().collect();
    let top = prompt_box_top(&lines)?;
    let start = line_start(c, &lines, top + 1);
    let end_i = lines[top + 1..].iter().position(|l| is_horizontal_rule(l)).map(|r| top + 1 + r).unwrap_or(lines.len());
    let end = line_start(c, &lines, end_i);
    Some(&c[start.min(c.len())..end.min(c.len())])
}
fn above_prompt_box(c: &str) -> &str {
    let lines: Vec<&str> = c.lines().collect();
    match prompt_box_top(&lines) { Some(t) => &c[..line_start(c, &lines, t)], None => c }
}
fn after_last_horizontal_rule(c: &str) -> &str {
    let (mut last, mut off) = (0usize, 0usize);
    for l in c.lines() { let next = off + l.len() + 1; if is_horizontal_rule(l) { last = next.min(c.len()); } off = next; }
    &c[last..]
}
fn last_non_empty_line(c: &str) -> &str { c.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("") }
fn slice_from_line<'a>(c: &'a str, lines: &[&str], i: usize) -> &'a str { &c[line_start(c, lines, i).min(c.len())..] }
fn line_start(c: &str, lines: &[&str], i: usize) -> usize { lines[..i.min(lines.len())].iter().map(|l| l.len() + 1).sum::<usize>().min(c.len()) }

#[cfg(test)]
mod tests {
    use super::*;
    fn inp<'a>(screen: &'a str, title: &'a str) -> DetectionInput<'a> { DetectionInput { screen, osc_title: title, osc_progress: "" } }

    #[test]
    fn bundled_manifests_compile() {
        let m = bundled();
        assert_eq!(m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["claude", "codex", "grok"]);
    }
    #[test]
    fn identify_by_process_command_and_title() {
        let m = bundled();
        assert_eq!(identify(&m, Some("claude"), None, ""), Some(0));
        assert_eq!(identify(&m, Some("bash"), Some("FOO=1 codex --full-auto"), ""), Some(1));
        assert_eq!(identify(&m, None, Some("ls -la"), "⠋ Thinking - grok"), Some(2));
        assert_eq!(identify(&m, Some("bash"), Some("vim a.txt"), "user@host: ~"), None);
    }
    #[test]
    fn grok_states_from_title() {
        let m = bundled();
        let g = &m[2];
        assert_eq!(g.detect(inp("", "⠙ Reading files - grok")).state, AgentState::Working);
        assert_eq!(g.detect(inp("", "grok")).state, AgentState::Idle);
        assert_eq!(g.detect(inp("", "⚠ Action Required - grok")).state, AgentState::Blocked);
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
}
