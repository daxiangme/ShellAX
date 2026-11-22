//! Agent Workbench (`Tool::Agents`): dedicated left-panel session history browser, attention
//! queue, running-agent overview, task + git worktree launcher (with multi-agent fan-out),
//! safe worktree teardown, and Agent-mode status bar.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClipboardItem, Context, Entity, FontWeight, Hsla,
    KeyDownEvent, Point, Pixels, ScrollHandle, SharedString, WeakEntity, Window,
};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::agent::{self, AgentState};
use ssh_term_proto::agent_history::{
    self, ContextBreakdown, ContextCategoryKind, SessionMeta,
};
use ssh_term_proto::agent_resume::{self, AgentSessionRef};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::layout::{Dir, Node};
use ssh_term_proto::policy_server::Answer;
use ssh_term_proto::session_store::{TaskMeta, TaskState};
use ssh_term_proto::worktree;

use crate::activity::{self, Tool};
use crate::attention::{self, AttentionKind};
use crate::config;
use crate::pane_api::send_when_shell_ready;
use crate::settings_view::Cat;
use crate::termview::{Spec, TermView};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{data_dir, PaneInit, Root, Tab, ToastKind};

#[derive(Clone)]
pub(crate) enum AgentMenuTarget {
    History(SessionMeta),
    Running {
        pane: Entity<TermView>,
        tab: usize,
        has_worktree: bool,
        session_id: Option<String>,
    },
}

pub(crate) struct NewTaskDialog {
    pub repo_input: Entity<InputState>,
    pub name_input: Entity<InputState>,
    pub base_input: Entity<InputState>,
    pub prompt_input: Entity<InputState>,
    pub use_worktree: bool,
    pub companion_shell: bool,
    pub selected_agents: Vec<String>,
    pub available_agents: Vec<(String, String, String)>, // (id, label, command)
    pub error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct FinishTaskDialog {
    pub tab_idx: usize,
    pub task: TaskMeta,
    pub dirty: bool,
    pub diff_stat: String,
    pub force: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ContextPopover {
    pub pos: Point<Pixels>,
    pub anchor_bottom: bool,
    pub agent: String,
    pub session_id: Option<String>,
    pub path: Option<PathBuf>,
    pub title: String,
    pub fallback_used: Option<u64>,
    pub fallback_window: Option<u64>,
    pub fallback_frac: f32,
    pub fallback_label: String,
    pub loading: bool,
    pub breakdown: Option<ContextBreakdown>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SessionGroupStatus {
    NeedsAttention,
    Running,
    Done,
    Open,
    Idle,
    Archived,
}

impl SessionGroupStatus {
    const ALL: [Self; 6] = [
        Self::NeedsAttention,
        Self::Running,
        Self::Done,
        Self::Open,
        Self::Idle,
        Self::Archived,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::NeedsAttention => t("ag.sec_attention"),
            Self::Running => t("ag.sec_running"),
            Self::Done => t("ag.sec_done"),
            Self::Open => t("ag.sec_open"),
            Self::Idle => t("ag.sec_idle"),
            Self::Archived => t("ag.sec_archived"),
        }
    }
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ArchiveState {
    #[serde(default)]
    pub archived: HashSet<String>,
    #[serde(default)]
    pub unarchived: HashSet<String>,
}

pub(crate) struct AgentHub {
    pub sessions: Vec<SessionMeta>,
    pub scanning: bool,
    pub started: bool,
    pub search: Option<Entity<InputState>>,
    pub filter: String,
    pub agent_menu: bool,
    pub agent_menu_pos: Option<Point<Pixels>>,
    pub collapsed_projects: HashSet<String>,
    pub collapsed_proj_groups: HashSet<(String, SessionGroupStatus)>,
    pub expanded_archived_projects: HashSet<String>,
    pub group_limits: HashMap<(String, SessionGroupStatus), usize>,
    pub archive: ArchiveState,
    pub kbd_active: bool,
    pub scroll: ScrollHandle,
    pub(crate) list_view: Option<Entity<AgentListView>>,
    pub(crate) cached_list: Option<Arc<AgentListSnapshot>>,
    pub(crate) sessions_rev: u64,
    pub(crate) layout_rev: u64,
    pub(crate) reset_scroll_pending: bool,
    pub selected_session_idx: usize,
    pub menu: Option<(AgentMenuTarget, Point<Pixels>)>,
    pub new_task: Option<NewTaskDialog>,
    pub finish_task: Option<FinishTaskDialog>,
    pub ctx_popover: Option<ContextPopover>,
    pub last_auto_scan: Option<std::time::Instant>,
    pub follow_up_scan_scheduled: bool,
    pub active_file_stamps: HashMap<PathBuf, (i64, u64)>,
}

impl AgentHub {
    pub(crate) fn invalidate_sessions(&mut self) {
        self.sessions_rev = self.sessions_rev.wrapping_add(1);
    }

    pub(crate) fn invalidate_layout(&mut self) {
        self.layout_rev = self.layout_rev.wrapping_add(1);
    }

    pub(crate) fn scroll_to_top_reset(&mut self) {
        self.reset_scroll_pending = true;
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
    }

    pub(crate) fn is_group_collapsed(&self, proj_key: &str, status: SessionGroupStatus) -> bool {
        if status == SessionGroupStatus::Archived {
            !self.expanded_archived_projects.contains(proj_key)
        } else {
            self.collapsed_proj_groups
                .contains(&(proj_key.to_string(), status))
        }
    }

    pub(crate) fn toggle_group_collapsed(&mut self, proj_key: &str, status: SessionGroupStatus) {
        if status == SessionGroupStatus::Archived {
            if !self.expanded_archived_projects.remove(proj_key) {
                self.expanded_archived_projects.insert(proj_key.to_string());
            }
        } else {
            let k = (proj_key.to_string(), status);
            if !self.collapsed_proj_groups.remove(&k) {
                self.collapsed_proj_groups.insert(k);
            }
        }
        self.invalidate_layout();
    }
}

impl Default for AgentHub {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            scanning: false,
            started: false,
            search: None,
            filter: "all".into(),
            agent_menu: false,
            agent_menu_pos: None,
            collapsed_projects: HashSet::new(),
            collapsed_proj_groups: HashSet::new(),
            expanded_archived_projects: HashSet::new(),
            group_limits: HashMap::new(),
            archive: ArchiveState::default(),
            kbd_active: false,
            scroll: ScrollHandle::new(),
            list_view: None,
            cached_list: None,
            sessions_rev: 0,
            layout_rev: 0,
            reset_scroll_pending: false,
            selected_session_idx: 0,
            menu: None,
            new_task: None,
            finish_task: None,
            ctx_popover: None,
            last_auto_scan: None,
            follow_up_scan_scheduled: false,
            active_file_stamps: HashMap::new(),
        }
    }
}

fn cache_file_path() -> PathBuf {
    data_dir().join("agent-history.json")
}

fn archive_file_path() -> PathBuf {
    data_dir().join("agent-archived.json")
}

fn archive_key(agent_id: &str, session_id: &str) -> String {
    format!("{agent_id}:{session_id}")
}

fn load_archive_state() -> ArchiveState {
    std::fs::read(archive_file_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<ArchiveState>(&b).ok())
        .unwrap_or_default()
}

fn save_archive_state(state: &ArchiveState) {
    let path = archive_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(state) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

fn cached_home_dir() -> Option<&'static str> {
    static HOME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let raw = if cfg!(windows) {
            std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"))
        } else {
            std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
        }
        .ok()?;
        let trimmed = raw.trim_end_matches(['/', '\\']).to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
    .as_deref()
}

fn dir_exists_cached(path: &str) -> bool {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, bool)>>> = OnceLock::new();
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((at, ok)) = g.get(trimmed) {
            if at.elapsed() < Duration::from_secs(15) {
                return *ok;
            }
        }
    }
    let ok = Path::new(trimmed).is_dir();
    if let Ok(mut g) = cache.lock() {
        if g.len() > 512 {
            g.clear();
        }
        g.insert(trimmed.to_string(), (Instant::now(), ok));
    }
    ok
}

fn abbreviate_home(path: &str) -> String {
    if let Some(h) = cached_home_dir() {
        if path == h {
            return "~".into();
        }
        if let Some(rest) = path.strip_prefix(h) {
            if rest.starts_with('/') || rest.starts_with('\\') {
                return format!("~{rest}");
            }
        }
        if cfg!(windows) && path.len() >= h.len() {
            let norm_h = h.replace('\\', "/");
            let norm_p = path.replace('\\', "/");
            if norm_p.eq_ignore_ascii_case(&norm_h) {
                return "~".into();
            }
            if norm_p.len() > norm_h.len()
                && norm_p[..norm_h.len()].eq_ignore_ascii_case(&norm_h)
                && norm_p.as_bytes()[norm_h.len()] == b'/'
            {
                return format!("~{}", &path[h.len()..]);
            }
        }
    }
    path.to_string()
}

fn agent_badge_color(agent_id: &str) -> Hsla {
    let th = cur();
    match agent_id {
        "claude" => c(th.warning),
        "codex" => c(th.success),
        "gemini" | "antigravity" | "agy" => c(th.info),
        "grok" => c(th.subtext),
        "opencode" => c(th.accent),
        _ => c(th.overlay1),
    }
}

fn truncate_tab_title(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Truncate a label by visual width units (ASCII = 1, non-ASCII = 2) so compact tab project
/// badges show only a few characters without cramping the session title.
fn compact_display_units(name: &str, max_units: usize, keep_units: usize) -> String {
    let char_w = |ch: char| if ch.is_ascii() { 1 } else { 2 };
    let total: usize = name.chars().map(char_w).sum();
    if total <= max_units {
        return name.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in name.chars() {
        let w = char_w(ch);
        if used + w > keep_units {
            break;
        }
        out.push(ch);
        used += w;
    }
    if out.is_empty() {
        if let Some(ch) = name.chars().next() {
            out.push(ch);
        }
    }
    out.push('…');
    out
}

pub(crate) fn compact_project_label(dir: &str) -> String {
    let trimmed = dir.trim();
    if abbreviate_home(trimmed) == "~" && !Path::new(trimmed).join(".git").exists() {
        return "~".into();
    }
    let name = agent_history::resolve_project_name_for_dir(trimmed);
    compact_display_units(&name, 8, 6)
}

/// Unified context usage resolved across transcript accounting (primary) and bottom-footer
/// terminal scraping (fallback when no transcript exists yet or over remote SSH).
#[derive(Debug, Clone, PartialEq)]
struct ResolvedContext {
    frac: f32,
    short_label: String,
    bar_label: String,
    tooltip: String,
    used_tokens: Option<u64>,
    window_tokens: Option<u64>,
}

fn resolve_context(
    meta: Option<&SessionMeta>,
    screen: Option<&agent::ScreenContextUsage>,
) -> Option<ResolvedContext> {
    if let Some(m) = meta {
        if let Some(d) = m.context_detail() {
            return Some(ResolvedContext {
                frac: d.frac,
                short_label: d.short_label,
                bar_label: d.bar_label,
                tooltip: d.tooltip,
                used_tokens: Some(d.used),
                window_tokens: Some(d.window),
            });
        }
        // If the transcript explicitly recorded a `/compact` or `/clear` boundary without a
        // subsequent turn yet, do NOT resurrect stale numbers from terminal scrollback.
        if m.ctx_reset {
            return None;
        }
    }
    screen.map(|s| ResolvedContext {
        frac: s.frac,
        short_label: s.short_label.clone(),
        bar_label: s.bar_label.clone(),
        tooltip: format!("ctx {}", s.bar_label),
        used_tokens: s.used_tokens,
        window_tokens: s.window_tokens,
    })
}

/// Compute non-overflowing pixel widths for each segment inside a fixed `bar_w` capsule.
fn segmented_bar_widths(tokens: &[u64], window: u64, bar_w: f32, gap: f32) -> Vec<f32> {
    let n = tokens.len();
    if n == 0 || window == 0 || bar_w <= 0.0 {
        return Vec::new();
    }
    let gap_total = gap * (n.saturating_sub(1) as f32);
    let avail = (bar_w - gap_total).max(0.0);
    let sum_tok: u64 = tokens.iter().sum();
    let total_fill = ((sum_tok as f64 / window as f64).clamp(0.0, 1.0) as f32) * avail;
    if total_fill <= 0.0 {
        return vec![0.0; n];
    }
    let mut widths: Vec<f32> = tokens
        .iter()
        .map(|&t| {
            if t == 0 {
                0.0
            } else {
                ((t as f64 / window as f64) as f32 * avail).max(3.0)
            }
        })
        .collect();
    let cur_sum: f32 = widths.iter().sum();
    if cur_sum > total_fill && cur_sum > 0.0 {
        // Trim excess proportionally from segments larger than the 3px floor so the total fill
        // stays faithful while keeping tiny categories visible.
        let excess = cur_sum - total_fill;
        let slack: f32 = widths.iter().map(|&w| (w - 3.0).max(0.0)).sum();
        if slack > 0.0 {
            for w in &mut widths {
                let s = (*w - 3.0).max(0.0);
                if s > 0.0 {
                    let take = (excess * (s / slack)).min(s);
                    *w -= take;
                }
            }
        } else {
            let scale = total_fill / cur_sum;
            for w in &mut widths {
                *w *= scale;
            }
        }
    }
    let final_sum: f32 = widths.iter().sum();
    if final_sum > avail && final_sum > 0.0 {
        let scale = avail / final_sum;
        for w in &mut widths {
            *w *= scale;
        }
    }
    widths
}

fn render_ctx_ring(ctx_frac: Option<f32>, ctx_label: &str, in_bar: bool) -> AnyElement {
    let th = cur();
    let raw = ctx_frac.unwrap_or(0.0);
    let frac = if !raw.is_finite() || raw <= 0.0 {
        0.0
    } else {
        raw.clamp(0.04, 1.0)
    };
    let tone = if frac >= 0.85 {
        c(th.error)
    } else if frac >= 0.65 {
        c(th.warning)
    } else if frac == 0.0 {
        c(th.overlay1)
    } else {
        c(th.accent)
    };
    let short_lbl = if in_bar {
        format!("ctx {ctx_label}")
    } else {
        ctx_label.strip_suffix(" tok").unwrap_or(ctx_label).to_string()
    };
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(if in_bar { ui::SP_1 } else { 3. }))
        .child(ui::ring(frac, tone))
        .child(
            div()
                .text_size(px(ui::fs_caption()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tone)
                .child(short_lbl),
        )
        .into_any_element()
}

fn working_pulse_dot(id: impl Into<gpui::ElementId>, color: Hsla) -> AnyElement {
    use gpui::AnimationExt as _;
    let core = div().size(px(6.)).rounded_full().bg(color);
    let ring: AnyElement = if crate::motion::reduced() {
        div().into_any_element()
    } else {
        div()
            .rounded_full()
            .border_1()
            .border_color(color)
            .with_animation(
                id,
                gpui::Animation::new(crate::motion::ms(crate::motion::BREATHE_MS))
                    .repeat()
                    .with_max_fps(30.0),
                |d, t| {
                    let sz = 6. + 6. * t;
                    d.size(px(sz)).opacity(0.72 * (1. - t))
                },
            )
            .into_any_element()
    };
    div()
        .relative()
        .size(px(12.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(ring)
        .child(div().absolute().child(core))
        .into_any_element()
}

fn agent_mark_tile(agent_id: &str) -> AnyElement {
    let col = agent_badge_color(agent_id);
    let glyph: AnyElement = match crate::file_icon::agent_glyph_path(agent_id) {
        Some(path) => ui::icon_svg(path, ui::IC_SM, col).into_any_element(),
        None => icon(IconName::Terminal, ui::IC_SM, col).into_any_element(),
    };
    div()
        .size(px(26.))
        .flex_shrink_0()
        .rounded(px(ui::R_MD))
        .bg(col.opacity(0.13))
        .border_1()
        .border_color(col.opacity(0.24))
        .flex()
        .items_center()
        .justify_center()
        .child(glyph)
        .into_any_element()
}

fn state_icon_and_color(st: AgentState) -> (IconName, Hsla) {
    let th = cur();
    match st {
        AgentState::Blocked => (IconName::CircleAlert, c(th.error)),
        AgentState::Done => (IconName::CircleCheck, c(th.success)),
        AgentState::Working => (IconName::LoaderCircle, c(th.accent)),
        AgentState::Idle => (IconName::Circle, c(th.overlay1)),
        AgentState::Unknown => (IconName::Circle, c(th.overlay0)),
    }
}

fn format_wait(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

fn available_agent_choices() -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (id, label, cmd) in agent::installed_agent_clis() {
        if seen.insert(cmd.to_string()) {
            out.push((id.into(), label.into(), cmd.into()));
        }
    }
    for ql in &config::get().agents.quick_launch {
        let cmd = ql.command.trim();
        if cmd.is_empty() {
            continue;
        }
        let first = cmd.split_whitespace().next().unwrap_or(cmd);
        if seen.insert(first.to_string()) {
            let id = first.to_ascii_lowercase();
            out.push((id, ql.name.clone(), cmd.to_string()));
        }
    }
    if out.is_empty() {
        out.push(("claude".into(), "Claude Code".into(), "claude".into()));
        out.push(("codex".into(), "Codex".into(), "codex".into()));
    }
    out
}

fn configured_worktrees_root() -> PathBuf {
    let raw = config::get().agents.worktree_root.trim().to_string();
    if raw.is_empty() {
        worktree::default_worktrees_root()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(rest)
    } else {
        PathBuf::from(raw)
    }
}

fn remember_recent_project(dir: &str) {
    let d = dir.trim();
    if d.is_empty() {
        return;
    }
    let _ = config::update(|s| {
        s.agents.recent_projects.retain(|x| x != d);
        s.agents.recent_projects.insert(0, d.to_string());
        s.agents.recent_projects.truncate(12);
    });
}

impl Root {
    pub(crate) fn ensure_agent_hub_started(&mut self, cx: &mut Context<Self>) {
        if self.agent_hub.started {
            return;
        }
        self.agent_hub.started = true;
        self.agent_hub.archive = load_archive_state();
        // Fast <10ms synchronous load from cache for instant first paint.
        self.agent_hub.sessions = agent_history::load_cached(&cache_file_path());
        self.agent_hub.invalidate_sessions();
        self.refresh_agent_history(cx);

        // Periodic background refresh (every 15s while open) + manifest auto-update.
        cx.spawn(async move |this, cx| {
            // Optional manifest update 25s after launch.
            cx.background_executor()
                .timer(Duration::from_secs(25))
                .await;
            if config::get().agents.manifest_update {
                let dd = data_dir();
                let url = config::get().agents.manifest_catalog_url.clone();
                let updated = cx
                    .background_executor()
                    .spawn(async move {
                        let custom = (!url.trim().is_empty()).then_some(url.as_str());
                        agent::refresh_remote_manifests(&dd, custom).ok()
                    })
                    .await;
                if updated.unwrap_or(0) > 0 {
                    let fresh = Arc::new(agent::load_manifests(&data_dir()));
                    let _ = this.update(cx, |r, cx| {
                        r.manifests = fresh;
                        cx.notify();
                    });
                }
            }
            let mut tick: u32 = 0;
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(3))
                    .await;
                tick = tick.wrapping_add(1);
                let active = this
                    .update(cx, |r, cx| {
                        if r.tool == Tool::Agents {
                            // Fast single-file stat/re-parse of open live panes so compact/clear
                            // and live turn token counts update within 1-3s even while idle.
                            r.refresh_active_pane_sessions(cx);
                            if !r.agent_hub.scanning {
                                let has_active_turn = r.tabs.iter().any(|t| {
                                    Root::tab_module(t, cx) == Tool::Agents
                                        && t.layout.leaves().iter().any(|p| {
                                            let v = p.read(cx);
                                            v.agent.is_some_and(|(_, st)| st == AgentState::Working)
                                                || (v.agent.is_some()
                                                    && v.agent_session_id.is_none())
                                        })
                                });
                                if has_active_turn || tick.is_multiple_of(5) {
                                    r.refresh_agent_history(cx);
                                }
                            }
                        }
                    })
                    .is_ok();
                if !active {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn refresh_agent_history(&mut self, cx: &mut Context<Self>) {
        if self.agent_hub.scanning {
            return;
        }
        self.agent_hub.scanning = true;
        cx.notify();
        let cache = cache_file_path();
        cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move {
                    let providers = agent_history::default_providers();
                    agent_history::scan_all(&cache, &providers)
                })
                .await;
            let _ = this.update(cx, |r, cx| {
                let changed = r.agent_hub.sessions != list;
                let was_empty = r.agent_hub.sessions.is_empty();
                if changed {
                    r.agent_hub.sessions = list;
                    r.agent_hub.invalidate_sessions();
                }
                r.agent_hub.scanning = false;
                r.agent_hub.last_auto_scan = Some(std::time::Instant::now());
                let bound = r.sync_live_agent_sessions(cx);
                if changed || was_empty || bound {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Lightweight single-file refresh for active live agent panes: stats each bound pane's
    /// transcript file and re-parses ONLY the files whose `(mtime, size)` actually changed,
    /// keeping live context rings and the bottom `ctx` readout accurate within 1-3s.
    pub(crate) fn refresh_active_pane_sessions(&mut self, cx: &mut Context<Self>) {
        let mut jobs: Vec<(String, PathBuf, (i64, u64), Option<SessionMeta>)> = Vec::new();
        let mut seen_paths: HashSet<PathBuf> = HashSet::new();

        for tab in &self.tabs {
            for pane in tab.layout.leaves() {
                let v = pane.read(cx);
                let Some(raw_aid) = v.agent_id() else {
                    continue;
                };
                let aid = agent::normalize_id(&raw_aid);
                let existing = v
                    .agent_session_id
                    .as_deref()
                    .and_then(|sid| self.agent_hub.sessions.iter().find(|m| m.id == sid))
                    .cloned();
                let path_opt = v
                    .agent_transcript_path
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| existing.as_ref().map(|m| m.path.clone()));
                let Some(path) = path_opt else {
                    continue;
                };
                if !seen_paths.insert(path.clone()) {
                    continue;
                }
                let Some(stamp) = agent_history::file_stamp(&path) else {
                    continue;
                };
                if self.agent_hub.active_file_stamps.get(&path) == Some(&stamp) {
                    continue;
                }
                self.agent_hub.active_file_stamps.insert(path.clone(), stamp);
                jobs.push((aid, path, stamp, existing));
            }
        }
        if jobs.is_empty() {
            return;
        }
        if self.agent_hub.active_file_stamps.len() > 256 {
            self.agent_hub.active_file_stamps.clear();
        }
        cx.spawn(async move |this, cx| {
            let updated: Vec<SessionMeta> = cx
                .background_executor()
                .spawn(async move {
                    jobs.into_iter()
                        .filter_map(|(aid, path, _stamp, existing)| {
                            agent_history::refresh_single_session(&aid, &path, existing.as_ref())
                        })
                        .collect()
                })
                .await;
            if updated.is_empty() {
                return;
            }
            let _ = this.update(cx, |r, cx| {
                let mut changed = false;
                for m in updated {
                    if let Some(slot) = r
                        .agent_hub
                        .sessions
                        .iter_mut()
                        .find(|s| s.agent == m.agent && s.id == m.id)
                    {
                        if *slot != m {
                            *slot = m;
                            changed = true;
                        }
                    } else {
                        r.agent_hub.sessions.push(m);
                        changed = true;
                    }
                }
                if changed {
                    r.agent_hub
                        .sessions
                        .sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
                    r.agent_hub.invalidate_sessions();
                    r.sync_live_agent_sessions(cx);
                    r.refresh_open_context_popover(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn refresh_open_context_popover(&mut self, cx: &mut Context<Self>) {
        let Some(ref pop) = self.agent_hub.ctx_popover else {
            return;
        };
        let Some(path) = pop.path.clone() else {
            return;
        };
        let agent = pop.agent.clone();
        let sid = pop.session_id.clone();
        cx.spawn(async move |this, cx| {
            let bd = cx
                .background_executor()
                .spawn(async move { agent_history::analyze_context_breakdown(&agent, &path) })
                .await;
            let _ = this.update(cx, |r, cx| {
                if let Some(ref mut cur_pop) = r.agent_hub.ctx_popover {
                    if cur_pop.session_id == sid {
                        cur_pop.breakdown = bd;
                        cur_pop.loading = false;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn open_context_popover(
        &mut self,
        pos: Point<Pixels>,
        anchor_bottom: bool,
        agent: String,
        session_id: Option<String>,
        path: Option<PathBuf>,
        title: String,
        fallback_used: Option<u64>,
        fallback_window: Option<u64>,
        fallback_frac: f32,
        fallback_label: String,
        cx: &mut Context<Self>,
    ) {
        self.agent_hub.menu = None;
        self.agent_hub.agent_menu = false;
        let has_path = path.is_some();
        self.agent_hub.ctx_popover = Some(ContextPopover {
            pos,
            anchor_bottom,
            agent: agent.clone(),
            session_id: session_id.clone(),
            path: path.clone(),
            title,
            fallback_used,
            fallback_window,
            fallback_frac,
            fallback_label,
            loading: has_path,
            breakdown: None,
        });
        cx.notify();
        if let Some(p) = path {
            cx.spawn(async move |this, cx| {
                let bd = cx
                    .background_executor()
                    .spawn(async move { agent_history::analyze_context_breakdown(&agent, &p) })
                    .await;
                let _ = this.update(cx, |r, cx| {
                    if let Some(ref mut cur_pop) = r.agent_hub.ctx_popover {
                        if cur_pop.session_id == session_id {
                            cur_pop.breakdown = bd;
                            cur_pop.loading = false;
                            cx.notify();
                        }
                    }
                });
            })
            .detach();
        }
    }

    fn toggle_context_popover_from_bar(
        &mut self,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let v = tab.focused.read(cx);
        let sid = v.agent_session_id.clone();
        let bound_meta = sid
            .as_deref()
            .and_then(|id| self.agent_hub.sessions.iter().find(|m| m.id == id))
            .cloned();
        let agent = bound_meta
            .as_ref()
            .map(|m| m.agent.clone())
            .or_else(|| v.agent_id().map(|a| agent::normalize_id(&a)))
            .unwrap_or_else(|| "claude".into());
        let path = v
            .agent_transcript_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| bound_meta.as_ref().map(|m| m.path.clone()));
        if let Some(ref existing) = self.agent_hub.ctx_popover {
            if existing.anchor_bottom && existing.session_id == sid && existing.path == path {
                self.agent_hub.ctx_popover = None;
                cx.notify();
                return;
            }
        }
        let screen_ctx = v
            .session
            .as_ref()
            .and_then(|s| agent::screen_context_usage(&s.screen_text()));
        let rc = resolve_context(bound_meta.as_ref(), screen_ctx.as_ref());
        let title = bound_meta
            .as_ref()
            .map(|m| m.display_title().to_string())
            .or_else(|| self.resolve_pane_auto_title(v, true))
            .unwrap_or_else(|| agent::display_name(&agent).to_string());
        let (fu, fw, ff, fl) = match rc {
            Some(r) => (r.used_tokens, r.window_tokens, r.frac, r.bar_label),
            None => (None, None, 0.0, "0%".into()),
        };
        self.open_context_popover(
            gpui::point(pos.x, pos.y - px(12.)),
            true,
            agent,
            sid,
            path,
            title,
            fu,
            fw,
            ff,
            fl,
            cx,
        );
    }

    fn open_context_popover_for_meta(
        &mut self,
        meta: &SessionMeta,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let d = meta.context_detail();
        let (fu, fw, ff, fl) = match d {
            Some(det) => (
                Some(det.used),
                Some(det.window),
                det.frac,
                det.bar_label,
            ),
            None => (meta.ctx_used, meta.ctx_window, 0.0, "0%".into()),
        };
        self.open_context_popover(
            pos,
            false,
            meta.agent.clone(),
            Some(meta.id.clone()),
            Some(meta.path.clone()),
            meta.display_title().to_string(),
            fu,
            fw,
            ff,
            fl,
            cx,
        );
    }

    fn open_context_popover_for_running_pane(
        &mut self,
        pane: &Entity<TermView>,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let v = pane.read(cx);
        let sid = v.agent_session_id.clone();
        let bound_meta = sid
            .as_deref()
            .and_then(|id| self.agent_hub.sessions.iter().find(|m| m.id == id))
            .cloned();
        if let Some(ref meta) = bound_meta {
            self.open_context_popover_for_meta(meta, pos, cx);
            return;
        }
        let agent = v
            .agent_id()
            .map(|a| agent::normalize_id(&a))
            .unwrap_or_else(|| "claude".into());
        let path = v
            .agent_transcript_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let screen_ctx = v
            .session
            .as_ref()
            .and_then(|s| agent::screen_context_usage(&s.screen_text()));
        let rc = resolve_context(None, screen_ctx.as_ref());
        let title = self
            .resolve_pane_auto_title(v, true)
            .unwrap_or_else(|| agent::display_name(&agent).to_string());
        let (fu, fw, ff, fl) = match rc {
            Some(r) => (r.used_tokens, r.window_tokens, r.frac, r.bar_label),
            None => (None, None, 0.0, "0%".into()),
        };
        self.open_context_popover(pos, false, agent, sid, path, title, fu, fw, ff, fl, cx);
    }

    /// Resolve a pane's auto-generated title from its bound history summary or visible prompt.
    fn resolve_pane_auto_title(&self, v: &TermView, allow_auto: bool) -> Option<String> {
        if !allow_auto {
            return None;
        }
        if !(v.agent.is_some() || v.agent_session_id.is_some() || v.ws == Some(Tool::Agents)) {
            return None;
        }
        if let Some(sid) = v.agent_session_id.as_deref() {
            if let Some(meta) = self.agent_hub.sessions.iter().find(|m| m.id == sid) {
                if meta.title.is_some()
                    || meta.first_prompt.is_some()
                    || meta.last_prompt.is_some()
                {
                    return Some(truncate_tab_title(meta.display_title(), 36));
                }
            }
        }
        v.meaningful_agent_caption()
    }

    /// Resolve an automatic tab title when the tab name was left unset or auto-generated.
    pub(crate) fn resolve_auto_tab_title(&self, tab: &Tab, cx: &App) -> Option<String> {
        let v = tab.focused.read(cx);
        let allow_auto = tab.name.is_none() || v.auto_title;
        if let Some(t) = self.resolve_pane_auto_title(v, allow_auto) {
            return Some(t);
        }
        if tab.name.is_none() {
            for leaf in tab.layout.leaves() {
                if leaf.entity_id() == tab.focused.entity_id() {
                    continue;
                }
                let lv = leaf.read(cx);
                if let Some(t) = self.resolve_pane_auto_title(lv, true) {
                    return Some(t);
                }
            }
        }
        None
    }

    /// Resolve `(short_label, full_project_dir)` for an Agent tab prefix badge and hover tooltip.
    pub(crate) fn agent_tab_project(&self, tab: &Tab, cx: &App) -> Option<(String, String)> {
        let panes = tab.layout.leaves();
        let bound_cwd = |v: &TermView| -> Option<String> {
            let sid = v.agent_session_id.as_deref()?;
            self.agent_hub
                .sessions
                .iter()
                .find(|m| m.id == sid)
                .and_then(|m| m.cwd.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let pane_cwd_of = |v: &TermView| -> Option<String> {
            v.shell_cwd()
                .or_else(|| bound_cwd(v))
                .or_else(|| v.initial_cwd.clone())
        };

        let task_repo = tab
            .task
            .as_ref()
            .map(|tk| tk.repo.display().to_string())
            .filter(|s| !s.trim().is_empty());
        let has_wt = tab
            .task
            .as_ref()
            .and_then(|tk| tk.worktree.as_ref())
            .is_some();

        let pane_cwd = pane_cwd_of(tab.focused.read(cx))
            .or_else(|| panes.iter().find_map(|p| pane_cwd_of(p.read(cx))));

        let raw_dir = if has_wt {
            task_repo.or(pane_cwd)
        } else {
            pane_cwd.or(task_repo)
        }?;

        let dir = raw_dir.trim();
        if dir.is_empty() {
            return None;
        }
        Some((compact_project_label(dir), dir.to_string()))
    }

    /// Trigger a throttled background scan on agent state changes and schedule one follow-up
    /// scan so delayed AI summary titles / newly created JSONL logs appear automatically.
    pub(crate) fn maybe_auto_refresh_agent_history(
        &mut self,
        urgent: bool,
        cx: &mut Context<Self>,
    ) {
        let now = std::time::Instant::now();
        let min_gap = if urgent {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(6)
        };
        let can_scan_now = !self.agent_hub.scanning
            && self
                .agent_hub
                .last_auto_scan
                .is_none_or(|t| now.duration_since(t) >= min_gap);
        if can_scan_now {
            self.agent_hub.last_auto_scan = Some(now);
            self.refresh_agent_history(cx);
        }

        if urgent && !self.agent_hub.follow_up_scan_scheduled {
            let needs_summary = self.tabs.iter().any(|t| {
                t.layout.leaves().iter().any(|p| {
                    let v = p.read(cx);
                    let is_auto = t.name.is_none() || v.auto_title;
                    if v.agent.is_none() {
                        return false;
                    }
                    if matches!(v.agent, Some((_, AgentState::Working))) {
                        return true;
                    }
                    if !is_auto {
                        return false;
                    }
                    match v.agent_session_id.as_deref() {
                        Some(sid) => !self
                            .agent_hub
                            .sessions
                            .iter()
                            .any(|m| m.id == sid && m.title.is_some()),
                        None => true,
                    }
                })
            });
            if needs_summary {
                self.agent_hub.follow_up_scan_scheduled = true;
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(1400))
                        .await;
                    let _ = this.update(cx, |r, cx| {
                        r.agent_hub.follow_up_scan_scheduled = false;
                        let bound = r.sync_live_agent_sessions(cx);
                        if !r.agent_hub.scanning {
                            r.agent_hub.last_auto_scan = Some(std::time::Instant::now());
                            r.refresh_agent_history(cx);
                        } else if bound {
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// Bind unbound live agent panes (`claude`, `codex`, `agy`, etc.) to their active
    /// history session ID so live panes merge with disk history and titles stay in sync.
    /// Returns `true` if any pane received a new session ID binding.
    pub(crate) fn sync_live_agent_sessions(&mut self, cx: &mut Context<Self>) -> bool {
        fn norm_cwd(p: &str) -> String {
            let t = p.trim().trim_end_matches(['/', '\\']);
            if cfg!(windows) {
                t.replace('/', "\\").to_ascii_lowercase()
            } else {
                t.to_string()
            }
        }
        fn compact_lower(s: &str) -> String {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        }

        struct UnboundPane {
            pane: Entity<TermView>,
            agent_id: String,
            cwd: Option<String>,
            started_unix: i64,
            screen_compact: String,
            osc_clean: String,
        }
        struct Cand {
            agent_id: String,
            cid: String,
            workspace: Option<String>,
            first_ts: i64,
            last_ts: i64,
            hints: Vec<String>,
        }

        let mut bound_sids: HashSet<String> = HashSet::new();
        let mut unbound: Vec<UnboundPane> = Vec::new();
        let mut rebound_count = 0usize;
        for tab in &self.tabs {
            let task_repo = tab
                .task
                .as_ref()
                .map(|t| t.repo.display().to_string())
                .filter(|s| !s.is_empty());
            for pane in tab.layout.leaves() {
                let (cur_sid, cur_tp) = {
                    let v = pane.read(cx);
                    (v.agent_session_id.clone(), v.agent_transcript_path.clone())
                };
                if let Some(sid) = cur_sid {
                    // Follow any `continued-in` handoff chain so a pane automatically tracks the
                    // successor session after context continuation.
                    let mut final_sid = sid.clone();
                    for _ in 0..8 {
                        let next = self
                            .agent_hub
                            .sessions
                            .iter()
                            .find(|m| m.id == final_sid)
                            .and_then(|m| m.continued_in.clone());
                        match next {
                            Some(ns) if ns != final_sid => final_sid = ns,
                            _ => break,
                        }
                    }
                    let matched_path = self
                        .agent_hub
                        .sessions
                        .iter()
                        .find(|m| m.id == final_sid)
                        .map(|m| m.path.display().to_string());
                    if final_sid != sid || (cur_tp.is_none() && matched_path.is_some()) {
                        let new_sid = final_sid.clone();
                        let new_tp = matched_path;
                        pane.update(cx, |v, _| {
                            v.agent_session_id = Some(new_sid);
                            if let Some(tp) = new_tp {
                                v.agent_transcript_path = Some(tp);
                            }
                        });
                        if final_sid != sid {
                            rebound_count += 1;
                        }
                    }
                    bound_sids.insert(final_sid);
                    continue;
                }
                let v = pane.read(cx);
                if let Some(raw_aid) = v.agent_id() {
                    let agent_id = agent::normalize_id(&raw_aid);
                    let cwd = v
                        .shell_cwd()
                        .or_else(|| task_repo.clone())
                        .map(|d| norm_cwd(&d))
                        .filter(|s| !s.is_empty());
                    let screen_compact = v
                        .session
                        .as_ref()
                        .map(|s| compact_lower(&s.screen_text()))
                        .unwrap_or_default();
                    let osc_clean = v
                        .session
                        .as_ref()
                        .map(|s| crate::termview::clean_osc_agent_title(&s.title()))
                        .filter(|t| !crate::termview::is_generic_title(t, None))
                        .map(|t| compact_lower(&t))
                        .unwrap_or_default();
                    unbound.push(UnboundPane {
                        pane: pane.clone(),
                        agent_id,
                        cwd,
                        started_unix: v.agent_started_unix,
                        screen_compact,
                        osc_clean,
                    });
                }
            }
        }
        if unbound.is_empty() {
            if rebound_count > 0 {
                cx.notify();
                return true;
            }
            return false;
        }

        let now_secs = agent_history::now_unix_secs();
        let min_pane_start = unbound
            .iter()
            .filter_map(|u| (u.started_unix > 0).then_some(u.started_unix))
            .min()
            .unwrap_or(now_secs)
            .saturating_sub(30);
        let content_match_scopes: Vec<(&str, Option<&str>)> = unbound
            .iter()
            .filter(|u| !u.osc_clean.is_empty() || !u.screen_compact.is_empty())
            .map(|u| (u.agent_id.as_str(), u.cwd.as_deref()))
            .collect();

        let mut by_key: HashMap<(String, String), Cand> = HashMap::new();
        for m in &self.agent_hub.sessions {
            if bound_sids.contains(&m.id) {
                continue;
            }
            let aid = agent::normalize_id(&m.agent);
            let ws = m.cwd.as_deref().map(norm_cwd).filter(|s| !s.is_empty());
            let recent = m.updated >= min_pane_start;
            let scope_match = !recent
                && content_match_scopes.iter().any(|(ua, uc)| {
                    *ua == aid
                        && match (*uc, ws.as_deref()) {
                            (Some(pc), Some(cw)) => pc == cw,
                            _ => true,
                        }
                });
            if !recent && !scope_match {
                continue;
            }
            let mut hints = Vec::new();
            for cand in [
                m.title.as_deref(),
                m.first_prompt.as_deref(),
                m.last_prompt.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                let c = cand.trim();
                if !c.is_empty() && !hints.iter().any(|x| x == c) {
                    hints.push(c.to_string());
                }
            }
            if !recent && hints.is_empty() {
                continue;
            }
            by_key.insert(
                (aid.clone(), m.id.clone()),
                Cand {
                    agent_id: aid,
                    cid: m.id.clone(),
                    workspace: ws,
                    first_ts: m.started,
                    last_ts: m.updated,
                    hints,
                },
            );
        }

        if unbound.iter().any(|u| u.agent_id == "agy") {
            for (cid, display, workspace, ts) in agent_history::lookup_antigravity_history_tail() {
                if bound_sids.contains(&cid) {
                    continue;
                }
                let norm_ws = workspace
                    .as_deref()
                    .map(norm_cwd)
                    .filter(|s| !s.is_empty());
                let entry = by_key
                    .entry(("agy".to_string(), cid.clone()))
                    .or_insert_with(|| Cand {
                        agent_id: "agy".to_string(),
                        cid,
                        workspace: norm_ws.clone(),
                        first_ts: ts,
                        last_ts: ts,
                        hints: Vec::new(),
                    });
                entry.first_ts = entry.first_ts.min(ts);
                entry.last_ts = entry.last_ts.max(ts);
                if entry.workspace.is_none() {
                    entry.workspace = norm_ws;
                }
                let p = display.trim();
                if !p.is_empty() && !p.starts_with('/') && !entry.hints.iter().any(|x| x == p) {
                    entry.hints.push(p.to_string());
                }
            }
        }

        let mut cands: Vec<Cand> = by_key.into_values().collect();
        if cands.is_empty() {
            return false;
        }
        cands.sort_by_key(|c| std::cmp::Reverse(c.last_ts));

        let mut group_counts: HashMap<(String, Option<String>), usize> = HashMap::new();
        for up in &unbound {
            *group_counts
                .entry((up.agent_id.clone(), up.cwd.clone()))
                .or_insert(0) += 1;
        }

        let mut used: HashSet<(String, String)> = HashSet::new();
        let mut need_scan = false;
        let mut bound_count = 0usize;

        for up in &unbound {
            let ref_ts = if up.started_unix > 0 {
                up.started_unix
            } else {
                now_secs
            };
            let same_group = group_counts
                .get(&(up.agent_id.clone(), up.cwd.clone()))
                .copied()
                .unwrap_or(1);
            let mut matching_cands = 0usize;
            for cand in &cands {
                if cand.agent_id != up.agent_id
                    || used.contains(&(cand.agent_id.clone(), cand.cid.clone()))
                    || cand.last_ts < ref_ts.saturating_sub(15)
                {
                    continue;
                }
                if let (Some(pc), Some(cw)) = (up.cwd.as_deref(), cand.workspace.as_deref()) {
                    if pc != cw {
                        continue;
                    }
                }
                if cand.first_ts >= ref_ts.saturating_sub(25) && !cand.hints.is_empty() {
                    matching_cands += 1;
                }
            }

            let mut best: Option<(i32, i64, String)> = None;
            for cand in &cands {
                if cand.agent_id != up.agent_id
                    || used.contains(&(cand.agent_id.clone(), cand.cid.clone()))
                {
                    continue;
                }
                if let (Some(pc), Some(cw)) = (up.cwd.as_deref(), cand.workspace.as_deref()) {
                    if pc != cw {
                        continue;
                    }
                }
                let recent = cand.last_ts >= ref_ts.saturating_sub(15);
                let min_osc = if recent { 2 } else { 4 };
                let min_prompt = if recent { 3 } else { 8 };
                let osc_hit = !up.osc_clean.is_empty()
                    && up.osc_clean.chars().count() >= min_osc
                    && cand.hints.iter().any(|h| {
                        let hc = compact_lower(h);
                        hc.chars().count() >= min_osc
                            && (hc == up.osc_clean
                                || hc.contains(&up.osc_clean)
                                || up.osc_clean.contains(&hc))
                    });
                let prompt_hit = cand
                    .hints
                    .iter()
                    .filter_map(|h| {
                        let hc = compact_lower(h);
                        let n = hc.chars().count();
                        if n < min_prompt {
                            return None;
                        }
                        if up.screen_compact.contains(&hc) {
                            Some(n as i32)
                        } else if n > 24 {
                            let head: String = hc.chars().take(24).collect();
                            up.screen_compact.contains(&head).then_some(24)
                        } else {
                            None
                        }
                    })
                    .max();
                let score = if osc_hit {
                    2000 + up.osc_clean.chars().count() as i32
                } else if let Some(len) = prompt_hit {
                    1000 + len
                } else if recent
                    && cand.first_ts >= ref_ts.saturating_sub(25)
                    && same_group == 1
                    && matching_cands == 1
                {
                    100
                } else {
                    0
                };
                if score > 0
                    && best
                        .as_ref()
                        .is_none_or(|(bs, bt, _)| (score, cand.last_ts) > (*bs, *bt))
                {
                    best = Some((score, cand.last_ts, cand.cid.clone()));
                }
            }
            if let Some((_, _, cid)) = best {
                used.insert((up.agent_id.clone(), cid.clone()));
                bound_count += 1;
                let matched = self.agent_hub.sessions.iter().find(|m| m.id == cid);
                let has_titled_meta = matched.is_some_and(|m| m.title.is_some());
                let matched_path = matched.map(|m| m.path.display().to_string());
                if !has_titled_meta {
                    need_scan = true;
                }
                up.pane.update(cx, |v, _| {
                    v.agent_session_id = Some(cid);
                    if let Some(tp) = matched_path {
                        v.agent_transcript_path = Some(tp);
                    }
                });
            }
        }
        if bound_count > 0 || rebound_count > 0 {
            if need_scan {
                self.maybe_auto_refresh_agent_history(true, cx);
            }
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Wake any restored local Agent panes in `tab_idx` that were queued for lazy auto-resume.
    pub(crate) fn wake_pending_agent_resumes_in_tab(
        &mut self,
        tab_idx: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let leaves = tab.layout.leaves();
        let mut woke = false;
        for pane in leaves {
            let pending = pane.update(cx, |v, _| v.pending_agent_resume.take());
            let Some(pending) = pending else {
                continue;
            };
            let transcript = pending.transcript_path.clone().or_else(|| {
                self.agent_hub
                    .sessions
                    .iter()
                    .find(|m| m.id == pending.session_id)
                    .map(|m| m.path.display().to_string())
            });
            pane.update(cx, |v, _| {
                v.agent_session_id = Some(pending.session_id.clone());
                if let Some(tp) = transcript {
                    v.agent_transcript_path = Some(tp);
                }
                v.agent_started_unix = agent_history::now_unix_secs();
                v.set_launched_agent(&pending.agent_id);
                v.resume_offer = None;
            });
            send_when_shell_ready(pane, pending.cmd, cx);
            woke = true;
        }
        if woke {
            self.agent_hub.invalidate_sessions();
            cx.notify();
        }
        woke
    }

    /// Wake the currently active Agent tab if it contains any restored panes awaiting lazy resume.
    pub(crate) fn wake_active_agent_tab_if_needed(&mut self, cx: &mut Context<Self>) {
        if self.settings_active()
            || self.tool.workspace() != Tool::Agents
            || !self.active_in_ws(cx)
        {
            return;
        }
        let has_pending = self.tabs.get(self.active).is_some_and(|t| {
            t.layout
                .leaves()
                .iter()
                .any(|p| p.read(cx).pending_agent_resume.is_some())
        });
        if has_pending {
            self.wake_pending_agent_resumes_in_tab(self.active, cx);
        }
    }

    /// Resume a historical coding session (or jump to its live pane if already open).
    pub(crate) fn resume_history_session(
        &mut self,
        meta: &SessionMeta,
        in_new_worktree: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // De-duplicate: if an open pane already owns this session ID, jump straight to it
        // (waking lazy-restored panes or re-launching exited shells in place).
        if !in_new_worktree {
            let existing = self.tabs.iter().flat_map(|t| t.layout.leaves()).find(|p| {
                p.read(cx)
                    .agent_session_id
                    .as_deref()
                    .is_some_and(|sid| sid == meta.id)
            });
            if let Some(pane) = existing {
                self.focus_pane_anywhere(&pane, window, cx);
                if pane.read(cx).pending_agent_resume.is_some() {
                    self.wake_pending_agent_resumes_in_tab(self.active, cx);
                } else {
                    let needs_relaunch = {
                        let v = pane.read(cx);
                        v.agent.is_none()
                            && v.ws == Some(Tool::Agents)
                            && matches!(v.spec, Spec::Local)
                    };
                    if needs_relaunch {
                        if let Some(cmd_line) =
                            agent_resume::build_validated_resume_line(&meta.agent, &meta.id)
                        {
                            let tp = meta.path.display().to_string();
                            let agent_name = meta.agent.clone();
                            pane.update(cx, |v, _| {
                                v.agent_transcript_path = Some(tp);
                                v.agent_started_unix = agent_history::now_unix_secs();
                                v.set_launched_agent(&agent_name);
                                v.resume_offer = None;
                            });
                            send_when_shell_ready(pane, cmd_line, cx);
                            cx.notify();
                        }
                    }
                }
                return;
            }
        }

        let Some(sref) = AgentSessionRef::id(meta.id.clone()) else {
            return;
        };
        let Some(plan) = agent_resume::plan(&meta.agent, &sref) else {
            return;
        };
        let Some(cmd_line) = agent_resume::shell_line(&plan.argv) else {
            return;
        };

        let orig_dir = meta.cwd.as_deref().map(PathBuf::from);
        let (spawn_dir, mut task_meta) = if in_new_worktree {
            if let Some(repo) = orig_dir
                .as_deref()
                .filter(|p| p.is_dir())
                .and_then(worktree::git_toplevel)
            {
                let short: String = meta.id.chars().take(8).collect();
                let branch = format!("agent/resume-{}", worktree::branch_to_path_slug(&short));
                let repo_name = repo
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "repo".into());
                let wt_path =
                    worktree::default_checkout_path(&configured_worktrees_root(), &repo_name, &branch);
                let base = worktree::git_current_branch(&repo).unwrap_or_else(|| "HEAD".into());
                match worktree::create_worktree(&repo, &wt_path, &branch, &base, false) {
                    Ok(()) => {
                        activity::log_push(
                            &self.exec_log,
                            "worktree",
                            &format!("git worktree add {} ({branch})", wt_path.display()),
                            true,
                            String::new(),
                        );
                        (
                            Some(wt_path.display().to_string()),
                            Some(TaskMeta {
                                id: ssh_term_proto::session_store::uuid_like(),
                                name: meta.display_title().chars().take(32).collect(),
                                repo,
                                worktree: Some(wt_path),
                                branch: Some(branch),
                                base: Some(base),
                                agent: meta.agent.clone(),
                                created: agent_history::now_unix_secs(),
                                state: TaskState::Active,
                            }),
                        )
                    }
                    Err(e) => {
                        self.toast_kind(
                            ToastKind::Error,
                            tf("ag.task_wt_failed", &[("e", &e)]),
                            cx,
                        );
                        return;
                    }
                }
            } else {
                (meta.cwd.clone(), None)
            }
        } else {
            match &orig_dir {
                Some(d) if d.is_dir() => (Some(d.display().to_string()), None),
                Some(_) => {
                    self.toast_kind(ToastKind::Warn, t("ag.cwd_missing"), cx);
                    (None, None)
                }
                None => (None, None),
            }
        };

        if let Some(ref d) = spawn_dir {
            remember_recent_project(d);
        }

        let pane = self.open_pane_with(
            Spec::Local,
            PaneInit {
                cwd: spawn_dir.clone(),
                ..Default::default()
            },
            window,
            cx,
        );
        let sid = meta.id.clone();
        let tp = meta.path.display().to_string();
        let auto_t = meta.title.is_none();
        let agent_name = meta.agent.clone();
        pane.update(cx, |v, _| {
            v.ws = Some(Tool::Agents);
            v.agent_session_id = Some(sid);
            v.agent_transcript_path = Some(tp);
            v.auto_title = auto_t;
            v.agent_started_unix = agent_history::now_unix_secs();
            v.set_launched_agent(&agent_name);
        });
        let tab_title: String = meta.display_title().chars().take(28).collect();
        if task_meta.is_none() {
            if let Some(ref d) = spawn_dir {
                task_meta = Some(TaskMeta {
                    id: ssh_term_proto::session_store::uuid_like(),
                    name: tab_title.clone(),
                    repo: PathBuf::from(d),
                    worktree: None,
                    branch: meta.branch.clone(),
                    base: None,
                    agent: meta.agent.clone(),
                    created: agent_history::now_unix_secs(),
                    state: TaskState::Active,
                });
            }
        }
        let fh = pane.read(cx).focus.clone();
        self.tool = Tool::Agents;
        let _ = config::update(|s| s.sidebar.tool = Tool::Agents.id().into());
        self.tabs.push(Tab {
            layout: Node::Leaf(pane.clone()),
            focused: pane.clone(),
            name: (!tab_title.is_empty()).then_some(tab_title),
            task: task_meta,
        });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        self.overlay = crate::Overlay::None;
        window.focus(&fh, cx);
        send_when_shell_ready(pane, cmd_line, cx);
        cx.notify();
    }

    /// Quick-launch a single coding agent in a new Agent tab.
    pub(crate) fn quick_launch_agent_in_dir(
        &mut self,
        agent_id: &str,
        cmd: &str,
        dir: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = dir.or_else(|| self.default_agent_project_dir(cx));
        if let Some(ref d) = dir {
            remember_recent_project(d);
        }
        let (launch_cmd, pre_sid) = agent::prepare_fresh_launch(agent_id, cmd);
        let pane = self.open_pane_with(
            Spec::Local,
            PaneInit {
                cwd: dir.clone(),
                ..Default::default()
            },
            window,
            cx,
        );
        pane.update(cx, |v, _| {
            v.ws = Some(Tool::Agents);
            v.auto_title = true;
            v.agent_started_unix = agent_history::now_unix_secs();
            v.set_launched_agent(agent_id);
            if let Some(sid) = pre_sid {
                v.agent_session_id = Some(sid);
            }
        });
        let fh = pane.read(cx).focus.clone();
        self.tool = Tool::Agents;
        let _ = config::update(|s| s.sidebar.tool = Tool::Agents.id().into());
        self.tabs.push(Tab {
            layout: Node::Leaf(pane.clone()),
            focused: pane.clone(),
            name: None,
            task: None,
        });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        self.overlay = crate::Overlay::None;
        window.focus(&fh, cx);
        send_when_shell_ready(pane, launch_cmd, cx);
        cx.notify();
    }

    fn default_agent_project_dir(&self, cx: &App) -> Option<String> {
        if let Some(tab) = self.tabs.get(self.active) {
            if let Some(cwd) = tab.focused.read(cx).shell_cwd() {
                if dir_exists_cached(&cwd) {
                    if let Some(top) = worktree::fast_git_toplevel(Path::new(&cwd)) {
                        return Some(top.display().to_string());
                    }
                    return Some(cwd);
                }
            }
        }
        if let Some(recent) = config::get()
            .agents
            .recent_projects
            .iter()
            .find(|p| dir_exists_cached(p))
        {
            return Some(recent.clone());
        }
        if let Some(s) = self
            .agent_hub
            .sessions
            .iter()
            .find_map(|m| m.cwd.as_ref().filter(|c| dir_exists_cached(c)))
        {
            return Some(s.clone());
        }
        cached_home_dir().map(str::to_string)
    }

    /// Open the "New Agent Task" modal (worktree isolation + optional multi-agent fan-out).
    pub(crate) fn open_new_task_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_task_dialog_in(None, window, cx);
    }

    pub(crate) fn open_new_task_dialog_in(
        &mut self,
        dir_override: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_agent_hub_started(cx);
        let init_dir = dir_override
            .filter(|d| !d.trim().is_empty() && Path::new(d).is_dir())
            .or_else(|| self.default_agent_project_dir(cx))
            .unwrap_or_default();
        let choices = available_agent_choices();
        let first_agent = choices
            .first()
            .map(|(id, _, _)| vec![id.clone()])
            .unwrap_or_default();

        let repo_input = cx.new(|cx| {
            let mut st = InputState::new(window, cx);
            st.set_value(init_dir, window, cx);
            st
        });
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder(t("ag.task_name_ph")));
        let base_input = cx.new(|cx| InputState::new(window, cx).placeholder(t("ag.task_base_ph")));
        let prompt_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(t("ag.task_prompt_ph")));
        name_input.update(cx, |i, cx| i.focus(window, cx));

        self.agent_hub.new_task = Some(NewTaskDialog {
            repo_input,
            name_input,
            base_input,
            prompt_input,
            use_worktree: false,
            companion_shell: false,
            selected_agents: first_agent,
            available_agents: choices,
            error: None,
        });
        cx.notify();
    }

    fn submit_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dlg) = self.agent_hub.new_task.as_ref() else {
            return;
        };
        let raw_dir = dlg.repo_input.read(cx).value().trim().to_string();
        let raw_name = dlg.name_input.read(cx).value().trim().to_string();
        let raw_base = dlg.base_input.read(cx).value().trim().to_string();
        let raw_prompt = dlg.prompt_input.read(cx).value().trim().to_string();
        let use_wt = dlg.use_worktree;
        let companion = dlg.companion_shell;
        let selected = dlg.selected_agents.clone();
        let choices = dlg.available_agents.clone();

        let dir_path = PathBuf::from(&raw_dir);
        if raw_dir.is_empty() || !dir_path.is_dir() {
            self.toast_kind(ToastKind::Error, t("ag.task_err_dir"), cx);
            return;
        }
        if selected.is_empty() {
            self.toast_kind(ToastKind::Error, t("ag.task_err_agent"), cx);
            return;
        }

        remember_recent_project(&raw_dir);
        let seed = agent_history::now_unix_secs() as u64;
        let repo_top = worktree::git_toplevel(&dir_path);
        let base_branch = if !raw_base.is_empty() {
            raw_base
        } else {
            repo_top
                .as_deref()
                .and_then(worktree::git_current_branch)
                .unwrap_or_else(|| "HEAD".into())
        };
        let is_auto_name = raw_name.is_empty();
        let will_create_wt = use_wt && repo_top.is_some();
        let base_branch_name = worktree::branch_for_task(&raw_name, seed);
        let display_task_name = if !is_auto_name {
            raw_name.clone()
        } else if will_create_wt {
            base_branch_name
                .strip_prefix("agent/")
                .unwrap_or(&base_branch_name)
                .to_string()
        } else {
            selected
                .iter()
                .map(|id| agent::display_name(id))
                .collect::<Vec<_>>()
                .join(" + ")
        };

        let fan_out = selected.len() > 1;
        let mut created_panes: Vec<Entity<TermView>> = Vec::new();
        let mut first_wt: Option<PathBuf> = None;
        let mut first_branch: Option<String> = None;

        for aid in &selected {
            let raw_cmd = choices
                .iter()
                .find(|(id, _, _)| id == aid)
                .map(|(_, _, c)| c.clone())
                .unwrap_or_else(|| agent::default_cli_command(aid).to_string());
            let (cmd, pre_sid) = agent::prepare_fresh_launch(aid, &raw_cmd);

            let (pane_cwd, wt_opt, br_opt) = if use_wt {
                if let Some(ref repo) = repo_top {
                    let branch = if fan_out {
                        format!("{base_branch_name}-{aid}")
                    } else {
                        base_branch_name.clone()
                    };
                    let repo_name = repo
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "repo".into());
                    let wt_path = worktree::default_checkout_path(
                        &configured_worktrees_root(),
                        &repo_name,
                        &branch,
                    );
                    if let Err(e) =
                        worktree::create_worktree(repo, &wt_path, &branch, &base_branch, false)
                    {
                        if let Some(d) = self.agent_hub.new_task.as_mut() {
                            d.error = Some(tf("ag.task_wt_failed", &[("e", &e)]));
                        }
                        cx.notify();
                        return;
                    }
                    activity::log_push(
                        &self.exec_log,
                        "worktree",
                        &format!("git worktree add {} ({branch})", wt_path.display()),
                        true,
                        String::new(),
                    );
                    let s = wt_path.display().to_string();
                    (s, Some(wt_path), Some(branch))
                } else {
                    (raw_dir.clone(), None, None)
                }
            } else {
                (raw_dir.clone(), None, None)
            };

            if first_wt.is_none() {
                first_wt = wt_opt;
                first_branch = br_opt;
            }

            let pane = self.open_pane_with(
                Spec::Local,
                PaneInit {
                    cwd: Some(pane_cwd),
                    ..Default::default()
                },
                window,
                cx,
            );
            pane.update(cx, |v, _| {
                v.ws = Some(Tool::Agents);
                v.auto_title = is_auto_name;
                v.agent_started_unix = seed as i64;
                v.set_launched_agent(aid);
                if let Some(sid) = pre_sid.clone() {
                    v.agent_session_id = Some(sid);
                }
            });
            send_when_shell_ready(pane.clone(), cmd, cx);
            if !raw_prompt.is_empty() {
                let pid = pane.read(cx).pane_id.clone();
                let prompt_copy = raw_prompt.clone();
                let (rtx, _rrx) = flume::bounded(1);
                // Reuse the 30s agent-idle readiness sender via PaneCmd::AgentPrompt after boot delay.
                let pane_clone = pane.clone();
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(800))
                        .await;
                    let _ = this.update_in(cx, |r, w, cx| {
                        r.handle_pane_cmd(
                            ssh_term_proto::policy_server::PaneCmd::AgentPrompt {
                                pane_id: pid,
                                prompt: prompt_copy,
                                reply: rtx,
                            },
                            w,
                            cx,
                        );
                    });
                })
                .detach();
                let _ = pane_clone;
            }
            created_panes.push(pane);
        }

        let Some(first_pane) = created_panes.first().cloned() else {
            return;
        };
        let mut layout = Node::Leaf(first_pane.clone());
        for extra in created_panes.iter().skip(1) {
            let leaves = layout.leaves();
            let last = leaves.last().unwrap().clone();
            layout.split(&last, extra.clone(), Dir::Vertical);
        }
        if companion && !fan_out {
            let comp_cwd = first_wt
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| raw_dir.clone());
            let comp = self.open_pane_with(
                Spec::Local,
                PaneInit {
                    cwd: Some(comp_cwd),
                    ..Default::default()
                },
                window,
                cx,
            );
            comp.update(cx, |v, _| v.ws = Some(Tool::Agents));
            layout.split(&first_pane, comp, Dir::Vertical);
        }

        let task_meta = TaskMeta {
            id: ssh_term_proto::session_store::uuid_like(),
            name: display_task_name.clone(),
            repo: repo_top.unwrap_or(dir_path),
            worktree: first_wt,
            branch: first_branch,
            base: Some(base_branch),
            agent: selected.join("+"),
            created: agent_history::now_unix_secs(),
            state: TaskState::Active,
        };
        let fh = first_pane.read(cx).focus.clone();
        self.agent_hub.new_task = None;
        self.tool = Tool::Agents;
        let _ = config::update(|s| s.sidebar.tool = Tool::Agents.id().into());
        self.tabs.push(Tab {
            layout,
            focused: first_pane,
            name: (!is_auto_name).then_some(raw_name),
            task: Some(task_meta),
        });
        self.active = self.tabs.len() - 1;
        self.leave_settings();
        self.overlay = crate::Overlay::None;
        window.focus(&fh, cx);
        cx.notify();
    }

    pub(crate) fn open_finish_task_dialog(&mut self, tab_idx: usize, cx: &mut Context<Self>) {
        let Some(task) = self.tabs.get(tab_idx).and_then(|t| t.task.clone()) else {
            return;
        };
        let (dirty, diff_stat) = if let Some(ref wt) = task.worktree {
            let d = worktree::checkout_has_dirty_files(wt, false).unwrap_or(false);
            let stat = worktree::git_diff_stat(wt, task.base.as_deref());
            (d, stat)
        } else {
            (false, String::new())
        };
        self.agent_hub.finish_task = Some(FinishTaskDialog {
            tab_idx,
            task,
            dirty,
            diff_stat,
            force: false,
            error: None,
        });
        cx.notify();
    }

    fn execute_finish_task(
        &mut self,
        remove_wt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dlg) = self.agent_hub.finish_task.clone() else {
            return;
        };
        if remove_wt {
            if let Some(ref wt) = dlg.task.worktree {
                match worktree::remove_worktree(&dlg.task.repo, wt, dlg.force, false) {
                    Ok(()) => {
                        let p = wt.display().to_string();
                        activity::log_push(
                            &self.exec_log,
                            "worktree",
                            &format!("git worktree remove {p}"),
                            true,
                            String::new(),
                        );
                        self.toast_kind(
                            ToastKind::Success,
                            tf("ag.finish_removed", &[("p", &p)]),
                            cx,
                        );
                    }
                    Err(e) => {
                        self.toast_kind(ToastKind::Error, e, cx);
                        return;
                    }
                }
            }
        }
        self.agent_hub.finish_task = None;
        if dlg.tab_idx < self.tabs.len() {
            self.close_tab(dlg.tab_idx, window, cx);
        }
        cx.notify();
    }

    fn recent_project_dirs(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut seen = HashSet::new();
        let mut push = |p: &str| {
            let trimmed = p.trim();
            if trimmed.is_empty() || !Path::new(trimmed).is_dir() {
                return;
            }
            if !seen.insert(trimmed.to_string()) {
                return;
            }
            let name = Path::new(trimmed)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| trimmed.to_string());
            out.push((name, trimmed.to_string()));
        };
        let recent = config::get().agents.recent_projects.clone();
        for p in &recent {
            push(p);
        }
        for m in &self.agent_hub.sessions {
            if let Some(c) = m.cwd.as_deref() {
                push(c);
            }
        }
        out.truncate(6);
        out
    }

    fn is_meta_archived(&self, meta: &SessionMeta) -> bool {
        let key = archive_key(&meta.agent, &meta.id);
        if self.agent_hub.archive.unarchived.contains(&key) {
            return false;
        }
        if self.agent_hub.archive.archived.contains(&key) {
            return true;
        }
        meta.agent == "codex"
            && meta
                .path
                .components()
                .any(|c| c.as_os_str() == "archived_sessions")
    }

    fn toggle_archive_session(&mut self, meta: &SessionMeta, cx: &mut Context<Self>) {
        let key = archive_key(&meta.agent, &meta.id);
        let currently = self.is_meta_archived(meta);
        if currently {
            self.agent_hub.archive.archived.remove(&key);
            self.agent_hub.archive.unarchived.insert(key);
        } else {
            self.agent_hub.archive.unarchived.remove(&key);
            self.agent_hub.archive.archived.insert(key);
        }
        self.agent_hub.invalidate_layout();
        save_archive_state(&self.agent_hub.archive);
        let title_short: String = meta.display_title().chars().take(28).collect();
        if !currently {
            if self
                .agent_hub
                .is_group_collapsed(&meta.project, SessionGroupStatus::Archived)
            {
                self.toast_kind(
                    ToastKind::Info,
                    tf("ag.archived_done", &[("name", &title_short)]),
                    cx,
                );
            }
        } else {
            self.toast_kind(
                ToastKind::Info,
                tf("ag.unarchived_done", &[("name", &title_short)]),
                cx,
            );
        }
        cx.notify();
    }

    fn pick_project_directory(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(picked) = paths.into_iter().next() else {
                return;
            };
            let dir = picked.display().to_string();
            let _ = this.update(cx, |this, cx| {
                remember_recent_project(&dir);
                let name = agent_history::resolve_project_name_for_dir(&dir);
                this.agent_hub.collapsed_projects.remove(&name);
                this.agent_hub.agent_menu = false;
                this.agent_hub.menu = None;
                this.agent_hub.selected_session_idx = 0;
                this.agent_hub.kbd_active = false;
                this.agent_hub.invalidate_layout();
                this.agent_hub.scroll_to_top_reset();
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_directory_for_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(picked) = paths.into_iter().next() else {
                return;
            };
            let dir = picked.display().to_string();
            let _ = this.update_in(cx, |this, window, cx| {
                remember_recent_project(&dir);
                if let Some(d) = this.agent_hub.new_task.as_mut() {
                    let ri = d.repo_input.clone();
                    if worktree::fast_git_toplevel(Path::new(&dir)).is_none() {
                        d.use_worktree = false;
                    }
                    ri.update(cx, |inp, cx| inp.set_value(dir, window, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Collect `(total_count, [(agent_id, count)])` across disk history and unbound live panes.
    fn agent_filter_summary(&self, cx: &App) -> (usize, Vec<(String, usize)>) {
        if let Some(ref snap) = self.agent_hub.cached_list {
            return (snap.filter_total, snap.filter_counts.clone());
        }
        let running = self.collect_running_agent_rows(cx);
        self.agent_filter_summary_from(&running)
    }

    fn agent_filter_summary_from(&self, running: &[RunningRow]) -> (usize, Vec<(String, usize)>) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut total = self.agent_hub.sessions.len();
        let known_sids: HashSet<&str> = self
            .agent_hub
            .sessions
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        for m in &self.agent_hub.sessions {
            *counts.entry(m.agent.clone()).or_insert(0) += 1;
        }
        for row in running {
            if row
                .session_id
                .as_deref()
                .is_some_and(|sid| known_sids.contains(sid))
            {
                continue;
            }
            if let Some(ref aid) = row.agent_id {
                *counts.entry(aid.clone()).or_insert(0) += 1;
                total += 1;
            }
        }
        if self.agent_hub.filter != "all" {
            counts.entry(self.agent_hub.filter.clone()).or_insert(0);
        }
        let mut out: Vec<(String, usize)> = counts.into_iter().collect();
        out.sort_by(|(a_id, a_n), (b_id, b_n)| {
            b_n.cmp(a_n)
                .then_with(|| agent::display_name(a_id).cmp(agent::display_name(b_id)))
        });
        (total, out)
    }

    fn status_group_from_state(st: AgentState) -> SessionGroupStatus {
        match st {
            AgentState::Blocked => SessionGroupStatus::NeedsAttention,
            AgentState::Working => SessionGroupStatus::Running,
            AgentState::Done => SessionGroupStatus::Done,
            AgentState::Idle | AgentState::Unknown => SessionGroupStatus::Open,
        }
    }

    fn state_urgency(st: AgentState) -> u8 {
        match st {
            AgentState::Blocked => 4,
            AgentState::Working => 3,
            AgentState::Done => 2,
            AgentState::Idle => 1,
            AgentState::Unknown => 0,
        }
    }

    fn is_worktree_path(dir: &str) -> bool {
        dir.contains("/.shellax/worktrees/") || dir.contains("\\.shellax\\worktrees\\")
    }

    fn prefer_primary_cwd(slot: &mut String, cand: Option<&str>) {
        let Some(c) = cand.map(str::trim).filter(|s| !s.is_empty()) else {
            return;
        };
        if slot.is_empty() || (Self::is_worktree_path(slot) && !Self::is_worktree_path(c)) {
            *slot = c.to_string();
        }
    }

    fn build_project_buckets_from(
        &self,
        running: &[RunningRow],
        query: &str,
        now_secs: i64,
        cx: &App,
    ) -> Vec<ProjectBucket> {
        struct Accum {
            name: String,
            cwd: String,
            total_count: usize,
            has_attention: bool,
            has_running: bool,
            has_open: bool,
            most_recent: i64,
            buckets: HashMap<SessionGroupStatus, Vec<GroupedItem>>,
        }

        let mut pane_by_sid: HashMap<String, LivePaneInfo> = HashMap::new();
        for row in running {
            if let Some(ref sid) = row.session_id {
                let entry = pane_by_sid.entry(sid.clone()).or_insert_with(|| LivePaneInfo {
                    state: row.state,
                    selected: row.selected,
                    ctx_frac: row.ctx_frac,
                    ctx_label: row.ctx_label.clone(),
                    live_title: (!row.title.is_empty()).then(|| row.title.clone()),
                });
                if Self::state_urgency(row.state) > Self::state_urgency(entry.state) {
                    entry.state = row.state;
                }
                if row.selected {
                    entry.selected = true;
                }
                if entry.ctx_label.is_none() && row.ctx_label.is_some() {
                    entry.ctx_frac = row.ctx_frac;
                    entry.ctx_label = row.ctx_label.clone();
                }
                if entry.live_title.is_none() && !row.title.is_empty() {
                    entry.live_title = Some(row.title.clone());
                }
            }
        }

        let all_known_sids: HashSet<&str> = self
            .agent_hub
            .sessions
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        let filter = self.agent_hub.filter.as_str();

        let mut by_proj: HashMap<String, Accum> = HashMap::new();

        for row in running {
            let sid_known = row
                .session_id
                .as_deref()
                .is_some_and(|sid| all_known_sids.contains(sid));
            if sid_known {
                continue;
            }
            if filter != "all" && row.agent_id.as_deref() != Some(filter) {
                continue;
            }
            if !query.is_empty() {
                let hit = row.title.to_lowercase().contains(query)
                    || row.sub.to_lowercase().contains(query)
                    || row
                        .cwd
                        .as_deref()
                        .is_some_and(|c| c.to_lowercase().contains(query));
                if !hit {
                    continue;
                }
            }
            let proj_name = row
                .project_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Local".into());
            let entry = by_proj.entry(proj_name.clone()).or_insert_with(|| Accum {
                name: proj_name,
                cwd: String::new(),
                total_count: 0,
                has_attention: false,
                has_running: false,
                has_open: false,
                most_recent: now_secs,
                buckets: HashMap::new(),
            });
            Self::prefer_primary_cwd(&mut entry.cwd, row.cwd.as_deref());
            let status = Self::status_group_from_state(row.state);
            match status {
                SessionGroupStatus::NeedsAttention => entry.has_attention = true,
                SessionGroupStatus::Running => entry.has_running = true,
                SessionGroupStatus::Done | SessionGroupStatus::Open => entry.has_open = true,
                _ => {}
            }
            entry.most_recent = entry.most_recent.max(now_secs);
            entry.total_count += 1;
            entry
                .buckets
                .entry(status)
                .or_default()
                .push(GroupedItem::LiveOnly(row.clone()));
        }

        let focused_sid: Option<String> = self
            .tabs
            .get(self.active)
            .filter(|t| Self::tab_module(t, cx) == Tool::Agents)
            .and_then(|t| t.focused.read(cx).agent_session_id.clone());

        for meta in self
            .agent_hub
            .sessions
            .iter()
            .filter(|m| (filter == "all" || m.agent == filter) && m.matches_query(query))
        {
            let proj_name = if meta.project.trim().is_empty() {
                "Local".to_string()
            } else {
                meta.project.clone()
            };
            let entry = by_proj.entry(proj_name.clone()).or_insert_with(|| Accum {
                name: proj_name,
                cwd: String::new(),
                total_count: 0,
                has_attention: false,
                has_running: false,
                has_open: false,
                most_recent: 0,
                buckets: HashMap::new(),
            });
            Self::prefer_primary_cwd(&mut entry.cwd, meta.cwd.as_deref());

            let archived = self.is_meta_archived(meta);
            let live = pane_by_sid.get(&meta.id);
            let active_open = focused_sid.as_deref() == Some(meta.id.as_str())
                || live.is_some_and(|l| l.selected);
            let live_state = live.map(|l| l.state);
            let status = if archived {
                SessionGroupStatus::Archived
            } else if let Some(st) = live_state {
                Self::status_group_from_state(st)
            } else {
                SessionGroupStatus::Idle
            };
            match status {
                SessionGroupStatus::NeedsAttention => entry.has_attention = true,
                SessionGroupStatus::Running => entry.has_running = true,
                SessionGroupStatus::Done | SessionGroupStatus::Open => entry.has_open = true,
                _ => {}
            }
            entry.most_recent = entry.most_recent.max(meta.updated);
            entry.total_count += 1;
            entry.buckets.entry(status).or_default().push(GroupedItem::Session {
                meta: meta.clone(),
                live_state,
                active_open,
                archived,
                ctx_frac: live.and_then(|l| l.ctx_frac),
                ctx_label: live.and_then(|l| l.ctx_label.clone()),
                live_title: live.and_then(|l| l.live_title.clone()),
            });
        }

        if filter == "all" && query.is_empty() {
            for dir in &config::get().agents.recent_projects {
                let trimmed = dir.trim();
                if trimmed.is_empty() || !dir_exists_cached(trimmed) {
                    continue;
                }
                let proj_name = agent_history::resolve_project_name_for_dir(trimmed);
                let entry = by_proj.entry(proj_name.clone()).or_insert_with(|| Accum {
                    name: proj_name,
                    cwd: trimmed.to_string(),
                    total_count: 0,
                    has_attention: false,
                    has_running: false,
                    has_open: false,
                    most_recent: 0,
                    buckets: HashMap::new(),
                });
                Self::prefer_primary_cwd(&mut entry.cwd, Some(trimmed));
            }
        }

        let mut out: Vec<ProjectBucket> = by_proj
            .into_iter()
            .map(|(key, mut acc)| {
                let groups = SessionGroupStatus::ALL
                    .into_iter()
                    .filter_map(|st| {
                        let mut items = acc.buckets.remove(&st)?;
                        if items.is_empty() {
                            return None;
                        }
                        items.sort_by(|a, b| match (a, b) {
                            (GroupedItem::LiveOnly(ra), GroupedItem::LiveOnly(rb)) => {
                                rb.selected.cmp(&ra.selected).then_with(|| ra.title.cmp(&rb.title))
                            }
                            (GroupedItem::LiveOnly(_), _) => std::cmp::Ordering::Less,
                            (_, GroupedItem::LiveOnly(_)) => std::cmp::Ordering::Greater,
                            (
                                GroupedItem::Session {
                                    meta: ma,
                                    active_open: oa,
                                    ..
                                },
                                GroupedItem::Session {
                                    meta: mb,
                                    active_open: ob,
                                    ..
                                },
                            ) => ob
                                .cmp(oa)
                                .then_with(|| mb.updated.cmp(&ma.updated))
                                .then_with(|| ma.id.cmp(&mb.id)),
                        });
                        Some((st, items))
                    })
                    .collect();
                ProjectBucket {
                    key,
                    name: acc.name,
                    cwd: acc.cwd,
                    total_count: acc.total_count,
                    has_attention: acc.has_attention,
                    has_running: acc.has_running,
                    has_open: acc.has_open,
                    most_recent: acc.most_recent,
                    groups,
                }
            })
            .collect();

        out.sort_by(|a, b| {
            b.has_attention
                .cmp(&a.has_attention)
                .then_with(|| b.has_running.cmp(&a.has_running))
                .then_with(|| b.has_open.cmp(&a.has_open))
                .then_with(|| b.most_recent.cmp(&a.most_recent))
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    fn search_query_lower(&self, cx: &App) -> String {
        self.agent_hub
            .search
            .as_ref()
            .map(|s| s.read(cx).value().trim().to_lowercase())
            .unwrap_or_default()
    }

    fn ensure_agent_list_snapshot(&mut self, cx: &App) -> Arc<AgentListSnapshot> {
        let running = self.collect_running_agent_rows(cx);
        let query = self.search_query_lower(cx);
        let focused_sid: Option<String> = self
            .tabs
            .get(self.active)
            .filter(|t| Self::tab_module(t, cx) == Tool::Agents)
            .and_then(|t| t.focused.read(cx).agent_session_id.clone());
        let now_secs = agent_history::now_unix_secs();
        let live_sig: Vec<LiveRowSig> = running
            .iter()
            .map(|r| LiveRowSig {
                pane_id: r.pane.entity_id(),
                agent_id: r.agent_id.clone(),
                state: r.state,
                selected: r.selected,
                session_id: r.session_id.clone(),
                title: r.title.clone(),
                ctx_label: r.ctx_label.clone(),
                ctx_permille: r.ctx_frac.map(|f| (f * 1000.0) as i32),
                cwd: r.cwd.clone(),
            })
            .collect();
        let key = AgentListCacheKey {
            sessions_rev: self.agent_hub.sessions_rev,
            layout_rev: self.agent_hub.layout_rev,
            query: query.clone(),
            filter: self.agent_hub.filter.clone(),
            focused_sid,
            time_minute: now_secs / 60,
            recent_projects: config::get().agents.recent_projects.clone(),
            live: live_sig,
        };

        if let Some(ref existing) = self.agent_hub.cached_list {
            if existing.key == key {
                return existing.clone();
            }
        }

        let (filter_total, filter_counts) = self.agent_filter_summary_from(&running);
        let running_active_count = running
            .iter()
            .filter(|r| matches!(r.state, AgentState::Working | AgentState::Blocked))
            .count();
        let archived_total = self
            .agent_hub
            .sessions
            .iter()
            .filter(|m| self.is_meta_archived(m))
            .count();
        let active_total = self.agent_hub.sessions.len().saturating_sub(archived_total);

        let project_buckets = self.build_project_buckets_from(&running, &query, now_secs, cx);
        let mut projects: Vec<ProjectHeaderMeta> = Vec::with_capacity(project_buckets.len());
        let mut rows: Vec<AgentFlatRow> = Vec::new();
        let mut row_tops: Vec<f32> = Vec::new();
        let mut row_ends: Vec<f32> = Vec::new();
        let mut cur_y: f32 = 0.0;
        let mut nav_row_indices: Vec<usize> = Vec::new();
        let mut nav_sessions: Vec<SessionMeta> = Vec::new();
        let mut nav_cursor: usize = 0;
        let mut run_idx: usize = 0;

        fn push_row(
            rows: &mut Vec<AgentFlatRow>,
            row_tops: &mut Vec<f32>,
            row_ends: &mut Vec<f32>,
            cur_y: &mut f32,
            row: AgentFlatRow,
            h: f32,
        ) {
            row_tops.push(*cur_y);
            *cur_y += h;
            row_ends.push(*cur_y);
            rows.push(row);
        }

        for (p_idx, proj) in project_buckets.into_iter().enumerate() {
            let proj_collapsed = self.agent_hub.collapsed_projects.contains(&proj.key);
            let hdr_top_y = cur_y;
            let card_top_y = if p_idx == 0 {
                hdr_top_y
            } else {
                hdr_top_y + ui::SP_2
            };
            let short_cwd = if proj.cwd.is_empty() {
                SharedString::from("")
            } else {
                SharedString::from(abbreviate_home(&proj.cwd))
            };
            let toggle_tip = {
                let action = if proj_collapsed {
                    t("ag.expand_sessions")
                } else {
                    t("ag.collapse_sessions")
                };
                SharedString::from(if !short_cwd.is_empty() {
                    format!("{short_cwd}\n{action}")
                } else {
                    action.to_string()
                })
            };
            let hdr_meta = ProjectHeaderMeta {
                key: proj.key.clone(),
                name: SharedString::from(proj.name),
                cwd: proj.cwd.clone(),
                short_cwd,
                toggle_tip,
                total_count: proj.total_count,
                has_attention: proj.has_attention,
                has_running: proj.has_running,
                collapsed: proj_collapsed,
                hdr_top_y,
                card_top_y,
            };
            projects.push(hdr_meta);
            let hdr_h = if p_idx == 0 {
                ROW_H_PROJ_FIRST
            } else {
                ROW_H_PROJ_NEXT
            };
            push_row(
                &mut rows,
                &mut row_tops,
                &mut row_ends,
                &mut cur_y,
                AgentFlatRow::ProjectHeader { proj_idx: p_idx },
                hdr_h,
            );

            if proj_collapsed {
                continue;
            }

            if proj.groups.is_empty() {
                let launch_dir = (!proj.cwd.is_empty()).then_some(proj.cwd);
                push_row(
                    &mut rows,
                    &mut row_tops,
                    &mut row_ends,
                    &mut cur_y,
                    AgentFlatRow::EmptyProject {
                        proj_idx: p_idx,
                        cwd: launch_dir,
                    },
                    ROW_H_EMPTY_PROJ,
                );
                continue;
            }

            for (status, items) in proj.groups {
                let count = items.len();
                let grp_collapsed = self.agent_hub.is_group_collapsed(&proj.key, status);
                push_row(
                    &mut rows,
                    &mut row_tops,
                    &mut row_ends,
                    &mut cur_y,
                    AgentFlatRow::StatusHeader {
                        proj_idx: p_idx,
                        proj_key: proj.key.clone(),
                        status,
                        count: SharedString::from(count.to_string()),
                        collapsed: grp_collapsed,
                    },
                    ROW_H_STATUS_HDR,
                );

                if grp_collapsed {
                    continue;
                }

                let limit = self
                    .agent_hub
                    .group_limits
                    .get(&(proj.key.clone(), status))
                    .copied()
                    .unwrap_or(80);
                let visible_count = items.len().min(limit);
                let remaining = items.len().saturating_sub(visible_count);

                for item in items.into_iter().take(limit) {
                    match item {
                        GroupedItem::LiveOnly(row) => {
                            push_row(
                                &mut rows,
                                &mut row_tops,
                                &mut row_ends,
                                &mut cur_y,
                                AgentFlatRow::Running { run_idx, row },
                                ROW_H_SESSION,
                            );
                            run_idx += 1;
                        }
                        GroupedItem::Session {
                            meta,
                            live_state,
                            active_open,
                            archived,
                            ctx_frac,
                            ctx_label,
                            live_title,
                        } => {
                            let row_ix = rows.len();
                            nav_row_indices.push(row_ix);
                            nav_sessions.push(meta.clone());
                            let prepared = prepare_session_row(
                                nav_cursor,
                                meta,
                                live_state,
                                active_open,
                                archived,
                                ctx_frac,
                                ctx_label,
                                live_title,
                                now_secs,
                            );
                            push_row(
                                &mut rows,
                                &mut row_tops,
                                &mut row_ends,
                                &mut cur_y,
                                AgentFlatRow::Session(prepared),
                                ROW_H_SESSION,
                            );
                            nav_cursor += 1;
                        }
                    }
                }

                if remaining > 0 {
                    push_row(
                        &mut rows,
                        &mut row_tops,
                        &mut row_ends,
                        &mut cur_y,
                        AgentFlatRow::LoadMore {
                            proj_idx: p_idx,
                            proj_key: proj.key.clone(),
                            status,
                            remaining,
                        },
                        ROW_H_LOAD_MORE,
                    );
                }
            }
        }

        let content_height = cur_y;
        let total_height = content_height + ui::SP_2;

        let snap = Arc::new(AgentListSnapshot {
            key,
            filter_total,
            filter_counts,
            running_active_count,
            archived_total,
            active_total,
            projects,
            rows,
            row_tops,
            row_ends,
            content_height,
            total_height,
            nav_row_indices,
            nav_sessions,
        });

        let reset_scroll = std::mem::take(&mut self.agent_hub.reset_scroll_pending);
        self.agent_hub.cached_list = Some(snap.clone());
        if reset_scroll {
            self.agent_hub.scroll.set_offset(gpui::point(px(0.), px(0.)));
        }

        snap
    }

    fn visible_nav_sessions(&mut self, cx: &App) -> Vec<SessionMeta> {
        self.ensure_agent_list_snapshot(cx).nav_sessions.clone()
    }

    /// Left tool window when `self.tool == Tool::Agents`.
    pub(crate) fn agents_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_agent_hub_started(cx);
        let th = cur();

        if self.agent_hub.search.is_none() {
            let inp = cx.new(|cx| InputState::new(window, cx).placeholder(t("ag.search")));
            self._subs.push(cx.subscribe_in(
                &inp,
                window,
                |this, _, ev: &InputEvent, w, cx| match ev {
                    InputEvent::Change => {
                        this.agent_hub.selected_session_idx = 0;
                        this.agent_hub.kbd_active = false;
                        this.agent_hub.scroll_to_top_reset();
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        let list = this.visible_nav_sessions(cx);
                        if let Some(meta) = list.get(this.agent_hub.selected_session_idx).cloned() {
                            this.resume_history_session(&meta, false, w, cx);
                        }
                    }
                    _ => {}
                },
            ));
            self.agent_hub.search = Some(inp);
        }
        let search_inp = self.agent_hub.search.clone().unwrap();

        // 1. Header bar
        let header = ui::panel_header_with(
            vec![
                ui::icon_svg("shellax/agent.svg", ui::IC_SM, c(th.accent)).into_any_element(),
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(ui::fs_body()))
                    .line_height(px(ui::lh(ui::fs_body())))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(th.text))
                    .child(t("tool.agents"))
                    .into_any_element(),
            ],
            vec![
                ui::button_sm("ag-new-task", t("ag.new_short"), 1)
                    .gap(px(ui::SP_1))
                    .tooltip(ui::tip(crate::sidebar::action_tip(
                        t("ag.new_task"),
                        &crate::actions::Action::NewAgentTask.binding(),
                    )))
                    .on_click(cx.listener(|this, _, w, cx| this.open_new_task_dialog(w, cx)))
                    .into_any_element(),
                ui::icon_button(
                    "ag-pick-dir",
                    IconName::FolderOpen,
                    ui::IconBtn::Md,
                    t("ag.browse_dir"),
                )
                .on_click(cx.listener(|this, _, _, cx| this.pick_project_directory(cx)))
                .into_any_element(),
                ui::icon_button(
                    "ag-refresh",
                    IconName::RefreshCw,
                    ui::IconBtn::Md,
                    t("ag.refresh"),
                )
                .on_click(cx.listener(|this, _, _, cx| this.refresh_agent_history(cx)))
                .into_any_element(),
                ui::icon_button(
                    "ag-settings",
                    IconName::Settings,
                    ui::IconBtn::Md,
                    t("ag.settings"),
                )
                .on_click(cx.listener(|this, _, w, cx| this.open_settings(Cat::Agents, w, cx)))
                .into_any_element(),
            ],
        );

        // 2. Build/reuse cached flattened session snapshot once per render, then feed filter/list/footer.
        let snap = self.ensure_agent_list_snapshot(cx);
        let all_count = snap.filter_total;
        let filter_counts = &snap.filter_counts;
        let cur_filter = self.agent_hub.filter.clone();
        let active_filter_count = if cur_filter == "all" {
            all_count
        } else {
            filter_counts
                .iter()
                .find(|(id, _)| id == &cur_filter)
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        let filter_active = cur_filter != "all";
        let filter_label = if !filter_active {
            t("ag.filter_all").to_string()
        } else {
            agent::display_name(&cur_filter).to_string()
        };
        let agent_filter_btn = div()
            .id("ag-filter-btn")
            .h(px(ui::H_DEFAULT))
            .max_w(px(124.))
            .flex_shrink_0()
            .px(px(ui::SP_2))
            .rounded(px(ui::R_MD))
            .bg(if filter_active {
                c(th.accent).opacity(0.14)
            } else {
                crate::theme::sub_bg()
            })
            .border_1()
            .border_color(if filter_active {
                c(th.accent).opacity(0.45)
            } else {
                crate::theme::divider()
            })
            .cursor_pointer()
            .hover(|s| s.bg(crate::theme::hover_bg()))
            .tooltip(ui::tip(t("ag.filter_agent")))
            .flex()
            .items_center()
            .gap(px(ui::SP_1))
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                cx.stop_propagation();
                this.agent_hub.menu = None;
                this.agent_hub.agent_menu = !this.agent_hub.agent_menu;
                this.agent_hub.agent_menu_pos = Some(ev.position());
                cx.notify();
            }))
            .child(if filter_active {
                match crate::file_icon::agent_glyph_path(&cur_filter) {
                    Some(path) => ui::icon_svg(path, ui::IC_XS, agent_badge_color(&cur_filter))
                        .into_any_element(),
                    None => div()
                        .size(px(6.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(agent_badge_color(&cur_filter))
                        .into_any_element(),
                }
            } else {
                icon(IconName::ListFilter, ui::IC_XS, c(th.overlay1)).into_any_element()
            })
            .child(
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(ui::fs_small()))
                    .font_weight(if filter_active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .text_color(c(th.text))
                    .child(filter_label),
            )
            .when(active_filter_count > 0 || filter_active, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(ui::fs_caption()))
                        .text_color(c(th.overlay0))
                        .child(active_filter_count.to_string()),
                )
            })
            .child(icon(IconName::ChevronDown, ui::IC_XS, c(th.overlay1)));

        // 3. Urgent policy approvals (kept pinned above directory bar so Allow/Deny are never hidden)
        let att_items = attention::collect(self, cx);
        let att_count = att_items.len();
        let urgent_approvals: Vec<_> = att_items
            .into_iter()
            .filter(|i| matches!(i.kind, AttentionKind::Approval(_)))
            .collect();
        let mut approval_banner = div().flex_shrink_0().flex().flex_col().gap(px(ui::SP_1));
        if !urgent_approvals.is_empty() {
            approval_banner = approval_banner.pb(px(ui::SP_1));
            for (idx, item) in urgent_approvals.into_iter().take(4).enumerate() {
                approval_banner = approval_banner.child(self.render_attention_row(idx, item, cx));
            }
        }

        // 4. Per-project collapsible virtualized session tree hosted in an isolated GPUI Entity
        // so mouse-wheel / touchpad scrolling only re-renders `AgentListView` instead of `Root`.
        let total_sessions_in_view = snap.nav_row_indices.len();
        if self.agent_hub.selected_session_idx >= total_sessions_in_view.max(1) {
            self.agent_hub.selected_session_idx = total_sessions_in_view.saturating_sub(1);
        }

        let sel_idx = self.agent_hub.selected_session_idx;
        let kbd_active = self.agent_hub.kbd_active;
        let list_body: AnyElement = if self.agent_hub.scanning && snap.rows.is_empty() {
            div()
                .flex_1()
                .child(ui::skeleton_rows(6))
                .into_any_element()
        } else if snap.rows.is_empty() {
            let msg = if self.agent_hub.sessions.is_empty() {
                t("ag.empty_history")
            } else {
                t("ag.empty_match")
            };
            ui::empty_state(
                IconName::BrainCircuit,
                msg,
                None,
                vec![ui::button_sm("ag-empty-pick-dir", t("ag.browse_dir"), 0)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_project_directory(cx)))
                    .into_any_element()],
                true,
            )
            .into_any_element()
        } else {
            let root_weak = cx.entity().downgrade();
            let shared_scroll = self.agent_hub.scroll.clone();
            let lv = self
                .agent_hub
                .list_view
                .get_or_insert_with(|| {
                    cx.new(|_| AgentListView {
                        root: root_weak,
                        scroll: shared_scroll,
                        snap: None,
                        sel_idx: 0,
                        kbd_active: false,
                    })
                })
                .clone();
            lv.update(cx, |v, cx| {
                let snap_changed = v
                    .snap
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(old, &snap));
                if snap_changed || v.sel_idx != sel_idx || v.kbd_active != kbd_active {
                    v.snap = Some(snap.clone());
                    v.sel_idx = sel_idx;
                    v.kbd_active = kbd_active;
                    cx.notify();
                }
            });
            lv.into_any_element()
        };

        // 5. Footer summary bar
        let running_count = snap.running_active_count;
        let archived_total = snap.archived_total;
        let active_total = snap.active_total;
        let footer_text = if att_count > 0 || running_count > 0 {
            tf(
                "ag.summary",
                &[("w", &att_count), ("r", &running_count), ("h", &active_total)],
            )
        } else if archived_total > 0 {
            tf(
                "ag.summary_archived",
                &[("h", &active_total), ("a", &archived_total)],
            )
        } else {
            tf("ag.summary_idle", &[("h", &active_total)])
        };
        let footer = div()
            .h(px(ui::H_COMPACT + 4.))
            .flex_shrink_0()
            .px(px(ui::PANEL_PX))
            .border_t_1()
            .border_color(crate::theme::divider())
            .flex()
            .items_center()
            .gap(px(ui::SP_2))
            .child(if att_count > 0 {
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(c(th.warning))
                    .into_any_element()
            } else if running_count > 0 {
                working_pulse_dot("ag-footer-run", c(th.accent))
            } else {
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(c(th.overlay0))
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(ui::fs_caption()))
                    .text_color(c(th.subtext))
                    .child(footer_text),
            );

        let nav_total = snap.nav_row_indices.len();
        let nav_snap = snap.clone();
        div()
            .id("agents-left-panel")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .capture_key_down(cx.listener(move |this, ev: &KeyDownEvent, _w, cx| {
                if nav_total == 0 {
                    return;
                }
                let next_idx = match ev.keystroke.key.as_str() {
                    "down" => Some((this.agent_hub.selected_session_idx + 1) % nav_total),
                    "up" => Some((this.agent_hub.selected_session_idx + nav_total - 1) % nav_total),
                    _ => None,
                };
                if let Some(new_sel) = next_idx {
                    this.agent_hub.kbd_active = true;
                    this.agent_hub.selected_session_idx = new_sel;
                    if let Some(&list_ix) = nav_snap.nav_row_indices.get(new_sel) {
                        reveal_virtual_row(&this.agent_hub.scroll, &nav_snap, list_ix);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(header)
            .child(
                ui::field_row()
                    .pb(px(ui::SP_2))
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_1))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(ui::search_field(&search_inp, false)),
                    )
                    .child(agent_filter_btn),
            )
            .child(approval_banner)
            .child(list_body)
            .child(footer)
            .into_any_element()
    }

fn render_project_header_card_el(
    root: &Entity<Root>,
    id_prefix: &'static str,
    p_idx: usize,
    meta: &ProjectHeaderMeta,
    is_sticky: bool,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let _ = cx;
    let th = cur();
    let has_short = !meta.short_cwd.is_empty();
    let launch_dir = (!meta.cwd.is_empty()).then(|| meta.cwd.clone());
    let toggle_key = meta.key.clone();
    let hdr_top_y = meta.hdr_top_y;

    let card = div()
        .id((id_prefix, p_idx))
        .w_full()
        .flex_shrink_0()
        .pl(px(ui::SP_2))
        .pr(px(ui::SP_1))
        .h(px(30.))
        .rounded(px(ui::R_MD))
        .bg(crate::theme::sub_bg())
        .flex()
        .items_center()
        .gap(px(ui::SP_1))
        .cursor_pointer()
        .hover(|s| s.bg(crate::theme::hover_bg()))
        .tooltip(ui::tip(meta.toggle_tip.clone()))
        .on_click(window.listener_for(root, move |this, _, _, cx| {
            cx.stop_propagation();
            this.agent_hub.agent_menu = false;
            this.agent_hub.menu = None;
            if !this.agent_hub.collapsed_projects.remove(&toggle_key) {
                this.agent_hub
                    .collapsed_projects
                    .insert(toggle_key.clone());
                if is_sticky {
                    this.agent_hub
                        .scroll
                        .set_offset(gpui::point(px(0.), px(-hdr_top_y)));
                }
            }
            this.agent_hub.invalidate_layout();
            cx.notify();
        }))
        .child(icon(
            if meta.collapsed {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            },
            ui::IC_XS,
            c(th.overlay1),
        ))
        .child(icon(IconName::FolderGit, ui::IC_SM, c(th.accent)))
        .child(
            div()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .text_size(px(ui::fs_small()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(th.text))
                .child(meta.name.clone()),
        )
        .when(has_short, |d| {
            d.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(ui::fs_caption()))
                    .text_color(c(th.overlay0))
                    .child(meta.short_cwd.clone()),
            )
        })
        .when(!has_short, |d| d.child(div().flex_1()))
        .when(meta.collapsed && (meta.has_running || meta.has_attention), |d| {
            let dot_col = if meta.has_attention {
                c(th.error)
            } else {
                c(th.accent)
            };
            d.child(working_pulse_dot(("ag-proj-dot", p_idx), dot_col))
        })
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(ui::fs_caption()))
                .text_color(c(th.overlay0))
                .child(meta.total_count.to_string()),
        )
        .child(
            ui::icon_button(
                (
                    if is_sticky {
                        "ag-proj-new-sticky"
                    } else {
                        "ag-proj-new"
                    },
                    p_idx,
                ),
                IconName::Plus,
                ui::IconBtn::Sm,
                t("ag.start_in_project"),
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(window.listener_for(root, move |this, _, w, cx| {
                cx.stop_propagation();
                this.open_new_task_dialog_in(launch_dir.clone(), w, cx);
            })),
        );

    div()
        .w_full()
        .flex_shrink_0()
        .px(px(ui::LIST_INSET))
        .when(!is_sticky && p_idx == 0, |d| {
            d.h(px(ROW_H_PROJ_FIRST)).pb(px(2.))
        })
        .when(!is_sticky && p_idx > 0, |d| {
            d.h(px(ROW_H_PROJ_NEXT)).pt(px(ui::SP_2)).pb(px(2.))
        })
        .child(card)
        .into_any_element()
}

fn render_status_group_header_el(
    root: &Entity<Root>,
    proj_idx: usize,
    proj_key: String,
    status: SessionGroupStatus,
    count: SharedString,
    collapsed: bool,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let _ = cx;
    let th = cur();
    let badge: AnyElement = match status {
        SessionGroupStatus::NeedsAttention => {
            icon(IconName::CircleAlert, ui::IC_XS, c(th.error)).into_any_element()
        }
        SessionGroupStatus::Running => {
            working_pulse_dot(("ag-grp-run-dot", proj_idx), c(th.accent))
        }
        SessionGroupStatus::Done => {
            icon(IconName::CircleCheck, ui::IC_XS, c(th.success)).into_any_element()
        }
        SessionGroupStatus::Open => div()
            .size(px(6.))
            .flex_shrink_0()
            .rounded_full()
            .bg(c(th.success))
            .into_any_element(),
        SessionGroupStatus::Idle => div()
            .size(px(6.))
            .flex_shrink_0()
            .rounded_full()
            .bg(c(th.overlay0))
            .into_any_element(),
        SessionGroupStatus::Archived => {
            icon(IconName::Archive, ui::IC_XS, c(th.overlay0)).into_any_element()
        }
    };

    div()
        .w_full()
        .h(px(ROW_H_STATUS_HDR))
        .flex_shrink_0()
        .px(px(ui::LIST_INSET))
        .pb(px(2.))
        .child(
            div()
                .id(("ag-grp-hdr", proj_idx * 16 + status as usize))
                .w_full()
                .h(px(ui::SECTION_H))
                .flex_shrink_0()
                .px(px(ui::ROW_PX))
                .rounded(px(ui::R_MD))
                .cursor_pointer()
                .hover(|s| s.bg(crate::theme::hover_bg()))
                .flex()
                .items_center()
                .gap(px(ui::SP_1))
                .on_click(window.listener_for(root, move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.agent_hub.toggle_group_collapsed(&proj_key, status);
                    cx.notify();
                }))
                .child(icon(
                    if collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    },
                    ui::IC_XS,
                    c(th.overlay1),
                ))
                .child(badge)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(ui::fs_small()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(th.subtext))
                        .child(status.label()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(ui::fs_caption()))
                        .text_color(c(th.overlay0))
                        .child(count),
                ),
        )
        .into_any_element()
}

fn render_grouped_session_row_el(
    root: &Entity<Root>,
    row: &PreparedSessionRow,
    kbd_sel: bool,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let _ = cx;
    let th = cur();
    let live = row.live_state.is_some();
    let nav_idx = row.nav_idx;
    let active_open = row.active_open;
    let archived = row.archived;
    let meta_click = row.meta.clone();
    let meta_wt = row.meta.clone();
    let meta_arch = row.meta.clone();
    let meta_ctx = row.meta.clone();
    let meta_ring = row.meta.clone();

    let status_badge: Option<AnyElement> = row.live_state.map(|st| match st {
        AgentState::Working => ui::spinning_icon(
            row.spin_id.clone(),
            IconName::LoaderCircle,
            ui::IC_XS,
            c(th.accent),
        ),
        AgentState::Blocked => div()
            .size(px(6.))
            .flex_shrink_0()
            .rounded_full()
            .bg(c(th.error))
            .into_any_element(),
        AgentState::Done | AgentState::Idle | AgentState::Unknown => div()
            .size(px(6.))
            .flex_shrink_0()
            .rounded_full()
            .bg(c(th.success))
            .into_any_element(),
    });

    let card = div()
        .id(row.row_id.clone())
        .relative()
        .group(row.row_gid.clone())
        .w_full()
        .h_full()
        .px(px(ui::SP_2))
        .rounded(px(ui::R_MD))
        .flex()
        .items_center()
        .gap(px(ui::SP_2))
        .cursor_pointer()
        .when(archived && !active_open, |d| d.opacity(0.74))
        .when(active_open, |d| d.bg(crate::theme::selected_bg()))
        .when(
            !active_open && row.live_state == Some(AgentState::Blocked),
            |d| {
                d.bg(c(th.error).opacity(0.07))
                    .hover(|s| s.bg(c(cur().error).opacity(0.11)))
            },
        )
        .when(
            !active_open && row.live_state != Some(AgentState::Blocked) && kbd_sel,
            |d| d.bg(crate::theme::hover_bg()),
        )
        .when(
            !active_open && row.live_state != Some(AgentState::Blocked) && !kbd_sel,
            |d| d.hover(|s| s.bg(crate::theme::hover_bg())),
        )
        .when(active_open || kbd_sel, |d| {
            d.child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(6.))
                    .bottom(px(6.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(if active_open {
                        c(th.accent)
                    } else {
                        c(th.accent).opacity(0.55)
                    }),
            )
        })
        .tooltip(ui::tip(row.tip_text.clone()))
        .on_click(window.listener_for(root, move |this, _, w, cx| {
            this.agent_hub.selected_session_idx = nav_idx;
            this.agent_hub.kbd_active = false;
            this.resume_history_session(&meta_click, false, w, cx);
        }))
        .on_mouse_down(
            gpui::MouseButton::Right,
            window.listener_for(root, move |this, ev: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.agent_hub.menu = Some((
                    AgentMenuTarget::History(meta_ctx.clone()),
                    ev.position,
                ));
                cx.notify();
            }),
        )
        .child(agent_mark_tile(&row.meta.agent))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(0.))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(ui::SP_1))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(ui::fs_body()))
                                .font_weight(if active_open || live {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(c(th.text))
                                .child(row.title.clone()),
                        )
                        .children(status_badge)
                        .when_some(row.eff_ctx_label.as_ref(), |d, lbl| {
                            let mr = meta_ring.clone();
                            let frac = row.eff_ctx_frac;
                            d.child(
                                div()
                                    .id(("ag-s-ctx", nav_idx))
                                    .px(px(3.))
                                    .py(px(1.))
                                    .rounded(px(ui::R_SM))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(crate::theme::hover_bg()))
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(window.listener_for(
                                        root,
                                        move |this, ev: &gpui::ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.open_context_popover_for_meta(
                                                &mr,
                                                ev.position(),
                                                cx,
                                            );
                                        },
                                    ))
                                    .child(render_ctx_ring(frac, lbl.as_ref(), false)),
                            )
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(ui::fs_caption()))
                                .text_color(c(th.overlay0))
                                .child(row.age.clone()),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(ui::SP_1))
                        .child(icon(IconName::GitBranch, ui::IC_XS, c(th.overlay0)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(ui::fs_caption()))
                                .text_color(c(th.overlay1))
                                .child(row.sub_line.clone()),
                        )
                        .child(
                            ui::row_actions(row.row_gid.clone())
                                .child(
                                    ui::icon_button(
                                        row.arch_id.clone(),
                                        if archived {
                                            IconName::RotateCcw
                                        } else {
                                            IconName::Archive
                                        },
                                        ui::IconBtn::Sm,
                                        if archived {
                                            t("ag.unarchive")
                                        } else {
                                            t("ag.archive")
                                        },
                                    )
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(window.listener_for(root, move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_archive_session(&meta_arch, cx);
                                    })),
                                )
                                .child(
                                    ui::icon_button(
                                        row.wt_id.clone(),
                                        IconName::FolderGit,
                                        ui::IconBtn::Sm,
                                        t("ag.resume_worktree"),
                                    )
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(window.listener_for(root, move |this, _, w, cx| {
                                        cx.stop_propagation();
                                        this.resume_history_session(&meta_wt, true, w, cx);
                                    })),
                                ),
                        ),
                ),
        );

    div()
        .w_full()
        .h(px(ROW_H_SESSION))
        .flex_shrink_0()
        .px(px(ui::LIST_INSET))
        .py(px(1.))
        .child(card)
        .into_any_element()
}

    fn render_attention_row(
        &mut self,
        idx: usize,
        item: crate::attention::AttentionItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let th = cur();
        let row_h = 46.0_f32;
        match item.kind {
            AttentionKind::Approval(ap_id) => {
                div()
                    .w_full()
                    .px(px(ui::LIST_INSET))
                    .child(
                        ui::list_row(("ag-att-ap", idx), 0, false)
                            .mx_0()
                            .w_full()
                            .h(px(row_h))
                            .min_h(px(row_h))
                            .child(icon(IconName::ShieldAlert, ui::IC_MD, c(th.error)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .child(ui::row_title(item.title))
                                    .child(ui::row_meta(format!("{} · {}", item.agent, item.subtitle))),
                            )
                            .child(
                                ui::button_sm(("ag-ap-ok", idx), t("ag.allow"), 1).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.answer_approval_by_id(ap_id, Answer::Once, cx);
                                    },
                                )),
                            )
                            .child(
                                ui::button_sm(("ag-ap-no", idx), t("ag.deny"), 2).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.answer_approval_by_id(ap_id, Answer::Deny(String::new()), cx);
                                    },
                                )),
                            ),
                    )
                    .into_any_element()
            }
            AttentionKind::Blocked | AttentionKind::DoneUnread => {
                let (ic, col) = if matches!(item.kind, AttentionKind::Blocked) {
                    (IconName::CircleAlert, c(th.error))
                } else {
                    (IconName::CircleCheck, c(th.success))
                };
                let pane = item.pane.clone();
                let wait = format_wait(item.wait_secs);
                div()
                    .w_full()
                    .px(px(ui::LIST_INSET))
                    .child(
                        ui::list_row(("ag-att-p", idx), 0, false)
                            .mx_0()
                            .w_full()
                            .h(px(row_h))
                            .min_h(px(row_h))
                            .on_click(cx.listener(move |this, _, w, cx| {
                                if let Some(ref p) = pane {
                                    this.focus_pane_anywhere(p, w, cx);
                                }
                            }))
                            .child(icon(ic, ui::IC_MD, col))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .child(ui::row_title(item.title))
                                    .child(ui::row_meta(format!(
                                        "{} · {} · {wait}",
                                        item.agent, item.subtitle
                                    ))),
                            ),
                    )
                    .into_any_element()
            }
        }
    }

    pub(crate) fn answer_approval_by_id(
        &mut self,
        id: u64,
        ans: Answer,
        cx: &mut Context<Self>,
    ) {
        if let Some(pos) = self.approvals.iter().position(|a| a.id == id) {
            let ap = self.approvals.remove(pos);
            let _ = ap.reply.send(ans);
            self.deny_reason = None;
            cx.notify();
        }
    }

    fn collect_running_agent_rows(&self, cx: &App) -> Vec<RunningRow> {
        let mut out = Vec::new();
        let active_pane = self.tabs.get(self.active).map(|t| t.focused.entity_id());
        for (ti, tab) in self.tabs.iter().enumerate() {
            let has_wt = tab.task.as_ref().and_then(|t| t.worktree.as_ref()).is_some();
            let branch = tab.task.as_ref().and_then(|t| t.branch.clone());
            let task_repo = tab
                .task
                .as_ref()
                .map(|t| t.repo.display().to_string())
                .filter(|s| !s.is_empty());
            for pane in tab.layout.leaves() {
                let v = pane.read(cx);
                let pane_cwd = v
                    .shell_cwd()
                    .or_else(|| task_repo.clone())
                    .or_else(|| v.initial_cwd.clone());
                let project_name = pane_cwd
                    .as_deref()
                    .map(agent_history::resolve_project_name_for_dir);
                let resolved_live = v.agent_label().or_else(|| {
                    let aid = v
                        .pending_agent_resume
                        .as_ref()
                        .map(|p| p.agent_id.clone())
                        .or_else(|| {
                            (v.agent_session_id.is_some() && v.ws == Some(Tool::Agents))
                                .then(|| v.resolved_agent_id())
                                .flatten()
                        })?;
                    Some((agent::display_name(&aid).to_string(), AgentState::Idle))
                });
                let Some((agent_name, st)) = resolved_live else {
                    if v.ws != Some(Tool::Agents) {
                        continue;
                    }
                    let title = tab
                        .name
                        .clone()
                        .unwrap_or_else(|| v.title.to_string());
                    out.push(RunningRow {
                        pane: pane.clone(),
                        tab: ti,
                        agent: "shell".into(),
                        agent_id: None,
                        state: AgentState::Idle,
                        title,
                        sub: branch
                            .clone()
                            .or_else(|| pane_cwd.clone())
                            .unwrap_or_default(),
                        selected: Some(pane.entity_id()) == active_pane,
                        has_worktree: has_wt,
                        session_id: v.agent_session_id.clone(),
                        cwd: pane_cwd,
                        project_name,
                        ctx_frac: None,
                        ctx_label: None,
                    });
                    continue;
                };
                let title = self
                    .resolve_pane_auto_title(v, tab.name.is_none() || v.auto_title)
                    .unwrap_or_else(|| {
                        tab.name
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| {
                                let cap = v.agent_caption();
                                if cap.is_empty() {
                                    agent_name.clone()
                                } else {
                                    cap
                                }
                            })
                    });
                let bound_meta = v
                    .agent_session_id
                    .as_deref()
                    .and_then(|sid| self.agent_hub.sessions.iter().find(|m| m.id == sid));
                let screen_ctx = v
                    .session
                    .as_ref()
                    .and_then(|s| agent::screen_context_usage(&s.screen_text()));
                let resolved_ctx = resolve_context(bound_meta, screen_ctx.as_ref());
                let ctx_frac = resolved_ctx.as_ref().map(|c| c.frac);
                let ctx_label = resolved_ctx.map(|c| c.short_label);
                let cwd_proj = pane_cwd
                    .as_deref()
                    .and_then(|c| {
                        c.trim_end_matches(['/', '\\'])
                            .rsplit(['/', '\\'])
                            .next()
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                let mut sub_parts = vec![agent_name.clone()];
                if let Some(ref b) = branch {
                    sub_parts.push(b.clone());
                } else if !cwd_proj.is_empty() {
                    sub_parts.push(cwd_proj);
                }
                out.push(RunningRow {
                    pane: pane.clone(),
                    tab: ti,
                    agent: agent_name,
                    agent_id: v.agent_id().or_else(|| v.resolved_agent_id()),
                    state: st,
                    title,
                    sub: sub_parts.join(" · "),
                    selected: Some(pane.entity_id()) == active_pane,
                    has_worktree: has_wt,
                    session_id: v.agent_session_id.clone(),
                    cwd: pane_cwd,
                    project_name,
                    ctx_frac,
                    ctx_label,
                });
            }
        }
        out
    }

fn render_running_row_el(
    root: &Entity<Root>,
    idx: usize,
    row: RunningRow,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let _ = cx;
    let th = cur();
    let (_, col) = state_icon_and_color(row.state);
    let tone = match row.state {
        AgentState::Blocked => c(th.error),
        AgentState::Working => c(th.accent),
        AgentState::Done | AgentState::Idle => c(th.success),
        AgentState::Unknown => c(th.overlay1),
    };
    let pane_click = row.pane.clone();
    let pane_ctx = row.pane.clone();
    let pane_ring = row.pane.clone();
    let tab_idx = row.tab;
    let has_wt = row.has_worktree;
    let sid = row.session_id.clone();
    let pane_close = row.pane.clone();
    let gid = SharedString::from(format!("ag-rrg-{idx}"));

    let left_tile = if let Some(ref aid) = row.agent_id {
        agent_mark_tile(aid)
    } else {
        div()
            .size(px(26.))
            .flex_shrink_0()
            .rounded(px(ui::R_MD))
            .bg(col.opacity(0.13))
            .border_1()
            .border_color(col.opacity(0.24))
            .flex()
            .items_center()
            .justify_center()
            .child(icon(IconName::Terminal, ui::IC_SM, col))
            .into_any_element()
    };

    let is_working = row.state == AgentState::Working;
    let status_badge: AnyElement = if is_working {
        ui::spinning_icon(
            ("ag-rspin", idx),
            IconName::LoaderCircle,
            ui::IC_XS,
            tone,
        )
    } else {
        div()
            .size(px(6.))
            .flex_shrink_0()
            .rounded_full()
            .bg(tone)
            .into_any_element()
    };

    let sub_text = format!("{} · {}", row.state.label(), row.sub);
    let ctx_tip = row
        .ctx_label
        .as_deref()
        .map(|l| format!("  ·  ctx {l}"))
        .unwrap_or_default();
    let tip_text = format!("{}\n{}{}", row.title, sub_text, ctx_tip);

    let card = div()
        .id(("ag-run", idx))
        .relative()
        .group(gid.clone())
        .w_full()
        .h_full()
        .px(px(ui::SP_2))
        .rounded(px(ui::R_MD))
        .flex()
        .items_center()
        .gap(px(ui::SP_2))
        .cursor_pointer()
        .when(row.selected, |d| {
            d.bg(crate::theme::selected_bg()).child(
                div()
                    .absolute()
                    .left(px(0.))
                    .top(px(6.))
                    .bottom(px(6.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(c(th.accent)),
            )
        })
        .when(!row.selected && row.state == AgentState::Blocked, |d| {
            d.bg(c(th.error).opacity(0.07))
                .hover(|s| s.bg(c(cur().error).opacity(0.11)))
        })
        .when(!row.selected && row.state != AgentState::Blocked, |d| {
            d.hover(|s| s.bg(crate::theme::hover_bg()))
        })
        .tooltip(ui::tip(tip_text))
        .on_click(window.listener_for(root, move |this, _, w, cx| {
            this.focus_pane_anywhere(&pane_click, w, cx);
        }))
        .on_mouse_down(
            gpui::MouseButton::Right,
            window.listener_for(root, move |this, ev: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.agent_hub.menu = Some((
                    AgentMenuTarget::Running {
                        pane: pane_ctx.clone(),
                        tab: tab_idx,
                        has_worktree: has_wt,
                        session_id: sid.clone(),
                    },
                    ev.position,
                ));
                cx.notify();
            }),
        )
        .child(left_tile)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(0.))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(ui::SP_1))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(ui::fs_body()))
                                .font_weight(if row.selected {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(c(th.text))
                                .child(row.title),
                        )
                        .child(status_badge)
                        .when_some(row.ctx_label.as_ref(), |d, lbl| {
                            let pr = pane_ring.clone();
                            let frac = row.ctx_frac;
                            d.child(
                                div()
                                    .id(("ag-r-ctx", idx))
                                    .px(px(3.))
                                    .py(px(1.))
                                    .rounded(px(ui::R_SM))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(crate::theme::hover_bg()))
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(window.listener_for(
                                        root,
                                        move |this, ev: &gpui::ClickEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.open_context_popover_for_running_pane(
                                                &pr,
                                                ev.position(),
                                                cx,
                                            );
                                        },
                                    ))
                                    .child(render_ctx_ring(frac, lbl, false)),
                            )
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(ui::SP_1))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(ui::fs_caption()))
                                .text_color(c(th.overlay1))
                                .child(sub_text),
                        )
                        .child(
                            ui::row_actions(gid).child(
                                ui::icon_button(
                                    ("ag-r-x", idx),
                                    IconName::X,
                                    ui::IconBtn::Sm,
                                    t("ag.close_pane"),
                                )
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(window.listener_for(root, move |this, _, w, cx| {
                                    cx.stop_propagation();
                                    this.close_pane_entity(&pane_close, w, cx);
                                })),
                            ),
                        ),
                ),
        );

    div()
        .w_full()
        .h(px(ROW_H_SESSION))
        .flex_shrink_0()
        .px(px(ui::LIST_INSET))
        .py(px(1.))
        .child(card)
        .into_any_element()
}

    /// Empty state in the center area when `Tool::Agents` has no open tabs.
    pub(crate) fn agents_ws_empty(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_agent_hub_started(cx);
        let th = cur();
        let target_dir = self.default_agent_project_dir(cx);
        let target_short = target_dir
            .as_deref()
            .map(abbreviate_home)
            .unwrap_or_else(|| "~".into());

        let choices = available_agent_choices();
        let mut launch_btns = div().flex().flex_wrap().items_center().gap(px(ui::SP_2));
        launch_btns = launch_btns.child(
            ui::button("ag-empty-task", t("ag.new_task"), 1)
                .gap(px(ui::SP_1))
                .on_click(cx.listener({
                    let td = target_dir.clone();
                    move |this, _, w, cx| this.open_new_task_dialog_in(td.clone(), w, cx)
                })),
        );
        for (i, (id, label, cmd)) in choices.into_iter().take(4).enumerate() {
            let td = target_dir.clone();
            launch_btns = launch_btns.child(
                ui::button(
                    ("ag-empty-launch", i),
                    tf("ag.quick_start", &[("name", &label)]),
                    0,
                )
                .on_click(cx.listener(move |this, _, w, cx| {
                    this.quick_launch_agent_in_dir(&id, &cmd, td.clone(), w, cx);
                })),
            );
        }

        let launch_card = div()
            .w_full()
            .p(px(ui::SP_4))
            .rounded(px(ui::R_LG))
            .bg(crate::theme::sub_bg())
            .border_1()
            .border_color(crate::theme::divider())
            .flex()
            .flex_col()
            .gap(px(ui::SP_3))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .child(icon(IconName::FolderGit, ui::IC_SM, c(th.accent)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_size(px(ui::fs_small()))
                            .font_family(ui::mono_family())
                            .text_color(c(th.subtext))
                            .child(target_short),
                    )
                    .child(
                        ui::button_sm("ag-empty-browse", t("ag.browse_dir"), 0)
                            .gap(px(ui::SP_1))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.pick_project_directory(cx);
                            })),
                    ),
            )
            .child(launch_btns);

        let now_secs = agent_history::now_unix_secs();
        let query = self.search_query_lower(cx);
        let filter = self.agent_hub.filter.as_str();
        let recents: Vec<SessionMeta> = self
            .agent_hub
            .sessions
            .iter()
            .filter(|m| {
                (filter == "all" || m.agent == filter)
                    && m.matches_query(&query)
                    && !self.is_meta_archived(m)
            })
            .take(6)
            .cloned()
            .collect();
        let has_recents = !recents.is_empty();
        let mut recent_col = div().flex().flex_col().gap(px(ui::SP_1)).w_full();
        for (i, meta) in recents.into_iter().enumerate() {
            let age = agent_history::relative_time_short(meta.updated, now_secs);
            let m2 = meta.clone();
            let m_wt = meta.clone();
            let sub = match meta.branch.as_deref().filter(|s| !s.is_empty()) {
                Some(b) => format!(
                    "{} · {} · {} · {age}",
                    meta.project,
                    b,
                    tf("ag.msgs", &[("n", &meta.messages)])
                ),
                None => format!(
                    "{} · {} · {age}",
                    meta.project,
                    tf("ag.msgs", &[("n", &meta.messages)])
                ),
            };
            let gid = SharedString::from(format!("ag-er-{i}"));
            recent_col = recent_col.child(
                div()
                    .id(("ag-empty-rec", i))
                    .group(gid.clone())
                    .w_full()
                    .min_h(px(48.))
                    .px(px(ui::SP_3))
                    .py(px(ui::SP_2))
                    .rounded(px(ui::R_SUB))
                    .bg(crate::theme::sub_bg())
                    .border_1()
                    .border_color(crate::theme::divider())
                    .hover(|s| s.bg(crate::theme::hover_bg()))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.resume_history_session(&m2, false, w, cx);
                    }))
                    .child(agent_mark_tile(&meta.agent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(ui::row_title(meta.display_title().to_string()))
                            .child(ui::row_meta(sub)),
                    )
                    .child(
                        ui::row_actions(gid).child(
                            ui::icon_button(
                                ("ag-empty-wt", i),
                                IconName::FolderGit,
                                ui::IconBtn::Sm,
                                t("ag.resume_worktree"),
                            )
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(cx.listener(move |this, _, w, cx| {
                                cx.stop_propagation();
                                this.resume_history_session(&m_wt, true, w, cx);
                            })),
                        ),
                    ),
            );
        }

        let mut inner = div()
            .w_full()
            .max_w(px(640.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(ui::SP_4))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .child(
                        div()
                            .size(px(44.))
                            .rounded(px(ui::R_LG))
                            .bg(crate::theme::accent_bg())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::icon_svg("shellax/agent.svg", ui::IC_LG + 4., c(th.accent))),
                    )
                    .child(
                        div()
                            .text_size(px(ui::fs_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.text))
                            .child(t("ws.agents_title")),
                    )
                    .child(
                        div()
                            .max_w(px(460.))
                            .text_size(px(ui::fs_small()))
                            .text_color(c(th.overlay1))
                            .text_center()
                            .child(t("ws.agents_hint")),
                    ),
            )
            .child(launch_card);

        if has_recents {
            inner = inner.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(ui::SP_2))
                    .child(
                        div()
                            .px(px(ui::SP_1))
                            .text_size(px(ui::fs_small()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(th.subtext))
                            .child(t("ag.recent_sessions")),
                    )
                    .child(recent_col),
            );
        }

        div()
            .id("ag-ws-empty")
            .flex_1()
            .size_full()
            .overflow_y_scroll()
            .px(px(ui::SP_6))
            .py(px(ui::SP_6))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(inner)
            .into_any_element()
    }

    /// Context menu & task modals hosted over the window for the Agent workbench.
    pub(crate) fn render_agent_overlays(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if self.agent_hub.agent_menu {
            if let Some(pos) = self.agent_hub.agent_menu_pos {
                out.push(self.render_agent_filter_popover(pos, cx));
            }
        }
        if let Some((target, pos)) = self.agent_hub.menu.clone() {
            out.push(self.render_agent_context_menu(target, pos, cx));
        }
        if self.agent_hub.new_task.is_some() {
            out.push(self.render_new_task_modal(cx));
        }
        if self.agent_hub.finish_task.is_some() {
            out.push(self.render_finish_task_modal(cx));
        }
        if self.agent_hub.ctx_popover.is_some() {
            out.push(self.render_context_popover(cx));
        }
        out
    }

    fn render_context_popover(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let pop = self.agent_hub.ctx_popover.clone().unwrap();
        let dark = th.appearance == crate::theme::Appearance::Dark;
        let cat_color = |kind: ContextCategoryKind| -> Hsla {
            match kind {
                ContextCategoryKind::SystemPrompt => c(th.overlay1),
                ContextCategoryKind::ToolDefinitions => gpui::hsla(
                    264. / 360.,
                    0.66,
                    if dark { 0.64 } else { 0.52 },
                    1.0,
                ),
                ContextCategoryKind::Rules => c(th.success),
                ContextCategoryKind::Skills => c(th.warning),
                ContextCategoryKind::McpTools => gpui::hsla(
                    330. / 360.,
                    0.72,
                    if dark { 0.58 } else { 0.48 },
                    1.0,
                ),
                ContextCategoryKind::Subagents => c(th.info),
                ContextCategoryKind::Conversation => gpui::hsla(
                    14. / 360.,
                    0.82,
                    if dark { 0.56 } else { 0.48 },
                    1.0,
                ),
                ContextCategoryKind::Other => c(th.subtext),
            }
        };
        let cat_label = |kind: ContextCategoryKind| -> &'static str {
            match kind {
                ContextCategoryKind::SystemPrompt => t("ag.ctx_system_prompt"),
                ContextCategoryKind::ToolDefinitions => t("ag.ctx_tool_defs"),
                ContextCategoryKind::Rules => t("ag.ctx_rules"),
                ContextCategoryKind::Skills => t("ag.ctx_skills"),
                ContextCategoryKind::McpTools => t("ag.ctx_mcp_tools"),
                ContextCategoryKind::Subagents => t("ag.ctx_subagents"),
                ContextCategoryKind::Conversation => t("ag.ctx_conversation"),
                ContextCategoryKind::Other => t("ag.ctx_other"),
            }
        };

        let (used_opt, win_opt, model_opt, is_est, cleared, compacted_only, categories, auto_buf) =
            match pop.breakdown.as_ref() {
                Some(bd) => (
                    Some(bd.used_tokens),
                    Some(bd.window_tokens),
                    bd.model.clone(),
                    bd.is_estimated,
                    bd.cleared,
                    bd.compacted_summary_only,
                    bd.categories.clone(),
                    bd.autocompact_buffer,
                ),
                None => (
                    pop.fallback_used,
                    pop.fallback_window,
                    None,
                    false,
                    false,
                    false,
                    Vec::new(),
                    None,
                ),
            };

        let ratio = match (used_opt, win_opt) {
            (Some(u), Some(w)) if w > 0 => (u as f64 / w as f64).clamp(0.0, 1.0) as f32,
            _ => pop.fallback_frac.clamp(0.0, 1.0),
        };
        let pct = (ratio * 100.0).round().min(100.0) as u32;
        let pct_tone = if pct >= 85 {
            c(th.error)
        } else if pct >= 65 {
            c(th.warning)
        } else {
            c(th.text)
        };

        let total_right = match (used_opt, win_opt) {
            (Some(u), Some(w)) if w > 0 => {
                let prefix = if is_est && u > 0 { "~" } else { "" };
                format!(
                    "{prefix}{} / {} {}",
                    agent::format_tokens_precise(u),
                    agent::format_tokens(w),
                    t("ag.ctx_tokens")
                )
            }
            _ => pop.fallback_label.clone(),
        };

        let card_w = 344.0_f32;
        let inner_w = card_w - 28.0; // px(14.) * 2
        let gap_w = 1.5_f32;

        // Build segmented progress bar items: either detailed categories or single active fill.
        let mut bar_segs: Vec<(Hsla, f32, String)> = Vec::new();
        if !categories.is_empty() {
            let win = win_opt.unwrap_or(1_000_000).max(1);
            let toks: Vec<u64> = categories.iter().map(|c| c.tokens).collect();
            let widths = segmented_bar_widths(&toks, win, inner_w, gap_w);
            for (cat, w) in categories.iter().zip(widths.into_iter()) {
                if w > 0.0 {
                    let lbl = cat_label(cat.kind);
                    let p_pct = (cat.tokens as f64 / win as f64) * 100.0;
                    let tip = format!(
                        "{lbl}: {} ({p_pct:.1}%)",
                        agent::format_tokens_precise(cat.tokens)
                    );
                    bar_segs.push((cat_color(cat.kind), w, tip));
                }
            }
        } else if ratio > 0.0 {
            let w = (inner_w * ratio).clamp(4.0, inner_w);
            bar_segs.push((c(th.accent), w, total_right.clone()));
        }

        let mut bar = div()
            .w(px(inner_w))
            .h(px(8.))
            .rounded_full()
            .bg(c(th.text).opacity(0.11))
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(gap_w));
        for (i, (col, w, tip)) in bar_segs.into_iter().enumerate() {
            bar = bar.child(
                div()
                    .id(("ag-ctx-seg", i))
                    .h_full()
                    .w(px(w))
                    .flex_shrink_0()
                    .bg(col)
                    .tooltip(ui::tip(tip)),
            );
        }

        let mut rows_col = div().w_full().flex().flex_col().gap(px(6.));
        if !categories.is_empty() {
            let win = win_opt.unwrap_or(1_000_000).max(1);
            for (i, cat) in categories.iter().enumerate() {
                let col = cat_color(cat.kind);
                let name = cat_label(cat.kind);
                let val_str = agent::format_tokens_precise(cat.tokens);
                let p_pct = (cat.tokens as f64 / win as f64) * 100.0;
                let tip = format!("{name}: {} tokens ({p_pct:.1}%)", cat.tokens);
                rows_col = rows_col.child(
                    div()
                        .id(("ag-ctx-row", i))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(px(ui::SP_2))
                        .tooltip(ui::tip(tip))
                        .child(
                            div()
                                .size(px(10.))
                                .flex_shrink_0()
                                .rounded(px(ui::R_SM))
                                .bg(col),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_baseline()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(ui::fs_small()))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(c(th.text))
                                        .child(name),
                                )
                                .when_some(cat.detail.as_ref(), |d, det| {
                                    d.child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_size(px(ui::fs_caption()))
                                            .text_color(c(th.overlay1))
                                            .child(det.clone()),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(ui::fs_small()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(c(th.subtext))
                                .child(val_str),
                        ),
                );
            }
        } else if cleared || used_opt == Some(0) {
            rows_col = rows_col.child(
                div()
                    .py_1()
                    .text_size(px(ui::fs_small()))
                    .text_color(c(th.overlay1))
                    .child(t("ag.ctx_empty")),
            );
        } else if let Some(u) = used_opt.filter(|&u| u > 0) {
            rows_col = rows_col.child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_2))
                    .child(
                        div()
                            .size(px(10.))
                            .flex_shrink_0()
                            .rounded(px(ui::R_SM))
                            .bg(c(th.accent)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(ui::fs_small()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c(th.text))
                            .child(t("ag.ctx_active")),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(ui::fs_small()))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c(th.subtext))
                            .child(agent::format_tokens_precise(u)),
                    ),
            );
        }

        let mut footer_notes = Vec::new();
        if let (Some(u), Some(w)) = (used_opt, win_opt) {
            let free = w.saturating_sub(u).saturating_sub(auto_buf.unwrap_or(0));
            if free > 0 && w > 0 {
                footer_notes.push(format!(
                    "{}: {}",
                    t("ag.ctx_free"),
                    agent::format_tokens_precise(free)
                ));
            }
        }
        if let Some(buf) = auto_buf {
            footer_notes.push(format!(
                "{}: {}",
                t("ag.ctx_buffer"),
                agent::format_tokens_precise(buf)
            ));
        }
        let sub_note = if pop.loading {
            None
        } else if pop.breakdown.is_none() && !cleared && pop.path.is_none() {
            Some(t("ag.ctx_screen_note").to_string())
        } else if compacted_only {
            Some(t("ag.ctx_compacted").to_string())
        } else if is_est && !categories.is_empty() {
            Some(t("ag.ctx_est_note").to_string())
        } else {
            None
        };

        let card = div()
            .id("ag-ctx-pop-card")
            .w(px(card_w))
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .rounded(px(ui::R_LG))
            .bg(crate::theme::overlay_card_bg(1.0))
            .border_1()
            .border_color(crate::theme::overlay_border())
            .shadow(crate::chrome::soft_shadow(true))
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(10.))
            // Header row
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(ui::SP_2))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(px(ui::fs_body()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(c(th.text))
                                    .child(t("ag.ctx_title")),
                            )
                            .when_some(model_opt, |d, m| {
                                d.child(
                                    div()
                                        .px(px(6.))
                                        .py(px(1.))
                                        .rounded(px(ui::R_SM))
                                        .bg(crate::theme::sub_bg())
                                        .border_1()
                                        .border_color(crate::theme::divider())
                                        .text_size(px(ui::fs_caption()))
                                        .text_color(c(th.overlay1))
                                        .child(m),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                ui::icon_button(
                                    "ag-ctx-pop-x",
                                    IconName::X,
                                    ui::IconBtn::Sm,
                                    t("ui.cancel"),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.agent_hub.ctx_popover = None;
                                    cx.notify();
                                })),
                            ),
                    )
                    .when(!pop.title.is_empty(), |d| {
                        d.child(
                            div()
                                .w_full()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(ui::fs_caption()))
                                .text_color(c(th.overlay1))
                                .child(pop.title.clone()),
                        )
                    }),
            )
            // Summary row: "27% Full"  +  "~273.0K / 1M Tokens"
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .gap(px(ui::SP_2))
                    .child(
                        div()
                            .text_size(px(ui::fs_page() - 2.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(pct_tone)
                            .child(tf("ag.ctx_full", &[("p", &pct)])),
                    )
                    .child(
                        div()
                            .text_size(px(ui::fs_small()))
                            .text_color(c(th.subtext))
                            .child(total_right),
                    ),
            )
            // Segmented bar
            .child(bar)
            // Category rows
            .child(rows_col)
            // Optional footer
            .when(!footer_notes.is_empty() || sub_note.is_some(), |d| {
                d.child(
                    div()
                        .w_full()
                        .pt(px(6.))
                        .border_t_1()
                        .border_color(crate::theme::divider())
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .when(!footer_notes.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(px(ui::fs_caption()))
                                    .text_color(c(th.subtext))
                                    .child(footer_notes.join("  ·  ")),
                            )
                        })
                        .when_some(sub_note, |d, sn| {
                            d.child(
                                div()
                                    .text_size(px(ui::fs_caption()))
                                    .text_color(c(th.overlay0))
                                    .child(sn),
                            )
                        }),
                )
            });

        let anchor = if pop.anchor_bottom {
            gpui::Anchor::BottomRight
        } else {
            gpui::Anchor::TopLeft
        };
        div()
            .id("ag-ctx-pop-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.agent_hub.ctx_popover = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.agent_hub.ctx_popover = None;
                    cx.notify();
                }),
            )
            .child(
                gpui::deferred(
                    gpui::anchored()
                        .anchor(anchor)
                        .position(pop.pos)
                        .snap_to_window_with_margin(px(ui::EDGE_MARGIN))
                        .child(crate::chrome::pop_in("ag-ctx-pop-anim", card)),
                )
                .with_priority(4),
            )
            .into_any_element()
    }

    fn render_agent_filter_popover(
        &mut self,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let th = cur();
        let (total, counts) = self.agent_filter_summary(cx);
        let cur_sel = self.agent_hub.filter.clone();

        let mut list = div()
            .id("ag-af-scroll")
            .max_h(px(320.))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        list = list.child(
            ui::menu_item(
                "ag-af-all",
                Some(if cur_sel == "all" {
                    IconName::Check
                } else {
                    IconName::Layers
                }),
                t("ag.filter_all"),
                true,
                false,
            )
            .when(cur_sel == "all", |d| d.font_weight(FontWeight::SEMIBOLD))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(ui::fs_caption()))
                    .text_color(c(th.overlay0))
                    .child(total.to_string()),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.agent_hub.filter = "all".into();
                this.agent_hub.agent_menu = false;
                this.agent_hub.selected_session_idx = 0;
                this.agent_hub.kbd_active = false;
                this.agent_hub.invalidate_layout();
                this.agent_hub.scroll_to_top_reset();
                cx.notify();
            })),
        );
        if !counts.is_empty() {
            list = list.child(ui::menu_sep());
            for (idx, (agent_id, count)) in counts.into_iter().enumerate() {
                let active = cur_sel == agent_id;
                let id_clone = agent_id.clone();
                let mut item = ui::menu_item(
                    ("ag-af-item", idx),
                    if active { Some(IconName::Check) } else { None },
                    agent::display_name(&agent_id).to_string(),
                    true,
                    false,
                )
                .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
                .child(match crate::file_icon::agent_glyph_path(&agent_id) {
                    Some(path) => ui::icon_svg(path, ui::IC_XS, agent_badge_color(&agent_id))
                        .into_any_element(),
                    None => div()
                        .size(px(6.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(agent_badge_color(&agent_id))
                        .into_any_element(),
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(ui::fs_caption()))
                        .text_color(c(th.overlay0))
                        .child(count.to_string()),
                );
                item = item.on_click(cx.listener(move |this, _, _, cx| {
                    this.agent_hub.filter = id_clone.clone();
                    this.agent_hub.agent_menu = false;
                    this.agent_hub.selected_session_idx = 0;
                    this.agent_hub.kbd_active = false;
                    this.agent_hub.invalidate_layout();
                    this.agent_hub.scroll_to_top_reset();
                    cx.notify();
                }));
                list = list.child(item);
            }
        }

        let menu = ui::popover().w(px(230.)).p(px(ui::SP_1)).child(list);

        div()
            .id("ag-af-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.agent_hub.agent_menu = false;
                    cx.notify();
                }),
            )
            .child(
                gpui::anchored()
                    .snap_to_window_with_margin(px(ui::EDGE_MARGIN))
                    .position(pos)
                    .child(crate::chrome::pop_in("ag-af-pop", menu)),
            )
            .into_any_element()
    }

    fn render_agent_context_menu(
        &mut self,
        target: AgentMenuTarget,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut menu = ui::popover().w(px(230.)).p(px(ui::SP_1)).flex().flex_col();
        match target {
            AgentMenuTarget::History(meta) => {
                let is_archived = self.is_meta_archived(&meta);
                let (m1, m2, m3, m4, m5, m6, m7) = (
                    meta.clone(),
                    meta.clone(),
                    meta.clone(),
                    meta.clone(),
                    meta.clone(),
                    meta.clone(),
                    meta.clone(),
                );
                menu = menu
                    .child(
                        ui::menu_item("ag-m-res", Some(IconName::Play), t("ag.resume"), true, false)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.agent_hub.menu = None;
                                this.resume_history_session(&m1, false, w, cx);
                            })),
                    )
                    .child(
                        ui::menu_item(
                            "ag-m-wt",
                            Some(IconName::FolderGit),
                            t("ag.resume_worktree"),
                            true,
                            false,
                        )
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.agent_hub.menu = None;
                            this.resume_history_session(&m2, true, w, cx);
                        })),
                    )
                    .child(
                        ui::menu_item(
                            "ag-m-ctx",
                            Some(IconName::Gauge),
                            t("ag.ctx_view"),
                            true,
                            false,
                        )
                        .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                            this.agent_hub.menu = None;
                            this.open_context_popover_for_meta(&m7, ev.position(), cx);
                        })),
                    )
                    .child(
                        ui::menu_item(
                            "ag-m-arch",
                            Some(if is_archived {
                                IconName::RotateCcw
                            } else {
                                IconName::Archive
                            }),
                            if is_archived {
                                t("ag.unarchive")
                            } else {
                                t("ag.archive")
                            },
                            true,
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.agent_hub.menu = None;
                            this.toggle_archive_session(&m6, cx);
                        })),
                    )
                    .child(ui::menu_sep())
                    .child(
                        ui::menu_item("ag-m-cid", Some(IconName::Copy), t("ag.copy_id"), true, false)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.agent_hub.menu = None;
                                cx.write_to_clipboard(ClipboardItem::new_string(m3.id.clone()));
                                this.toast_kind(ToastKind::Success, t("ag.copied_id"), cx);
                            })),
                    )
                    .when(m4.cwd.is_some(), |d| {
                        d.child(
                            ui::menu_item(
                                "ag-m-dir",
                                Some(IconName::SquareTerminal),
                                t("ag.open_dir"),
                                true,
                                false,
                            )
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.agent_hub.menu = None;
                                let pane = this.open_pane_with(
                                    Spec::Local,
                                    PaneInit {
                                        cwd: m4.cwd.clone(),
                                        ..Default::default()
                                    },
                                    w,
                                    cx,
                                );
                                this.tool = Tool::Hosts;
                                let fh = pane.read(cx).focus.clone();
                                this.tabs.push(Tab {
                                    layout: Node::Leaf(pane.clone()),
                                    focused: pane,
                                    name: None,
                                    task: None,
                                });
                                this.active = this.tabs.len() - 1;
                                w.focus(&fh, cx);
                                cx.notify();
                            })),
                        )
                    })
                    .child(
                        ui::menu_item(
                            "ag-m-rev",
                            Some(IconName::FolderOpen),
                            t("ag.reveal_file"),
                            true,
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.agent_hub.menu = None;
                            reveal_in_file_manager(&m5.path);
                            cx.notify();
                        })),
                    );
            }
            AgentMenuTarget::Running {
                pane,
                tab,
                has_worktree,
                session_id,
            } => {
                let (p1, p2, p3) = (pane.clone(), pane.clone(), pane.clone());
                menu = menu
                    .child(
                        ui::menu_item(
                            "ag-m-int",
                            Some(IconName::CircleStop),
                            t("ag.interrupt"),
                            true,
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.agent_hub.menu = None;
                            if let Some(s) = p1.read(cx).session.clone() {
                                s.write(b"\x1b".to_vec());
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        ui::menu_item(
                            "ag-m-rctx",
                            Some(IconName::Gauge),
                            t("ag.ctx_view"),
                            true,
                            false,
                        )
                        .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                            this.agent_hub.menu = None;
                            this.open_context_popover_for_running_pane(&p3, ev.position(), cx);
                        })),
                    )
                    .when_some(session_id, |d, sid| {
                        d.child(
                            ui::menu_item(
                                "ag-m-rcid",
                                Some(IconName::Copy),
                                t("ag.copy_id"),
                                true,
                                false,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.agent_hub.menu = None;
                                cx.write_to_clipboard(ClipboardItem::new_string(sid.clone()));
                                this.toast_kind(ToastKind::Success, t("ag.copied_id"), cx);
                            })),
                        )
                    })
                    .when(has_worktree, |d| {
                        d.child(
                            ui::menu_item(
                                "ag-m-fin",
                                Some(IconName::CheckCheck),
                                t("ag.finish_task"),
                                true,
                                false,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.agent_hub.menu = None;
                                this.open_finish_task_dialog(tab, cx);
                            })),
                        )
                    })
                    .child(ui::menu_sep())
                    .child(
                        ui::menu_item(
                            "ag-m-cls",
                            Some(IconName::Trash),
                            t("ag.close_pane"),
                            true,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.agent_hub.menu = None;
                            this.close_pane_entity(&p2, w, cx);
                        })),
                    );
            }
        }

        div()
            .id("ag-ctx-backdrop")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.agent_hub.menu = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                gpui::MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.agent_hub.menu = None;
                    cx.notify();
                }),
            )
            .child(
                gpui::anchored().snap_to_window_with_margin(px(ui::EDGE_MARGIN))
                    .position(pos)
                    .child(crate::chrome::pop_in("ag-ctx-pop", menu)),
            )
            .into_any_element()
    }

    fn render_new_task_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let dlg = self.agent_hub.new_task.as_ref().unwrap();
        let repo_inp = dlg.repo_input.clone();
        let name_inp = dlg.name_input.clone();
        let base_inp = dlg.base_input.clone();
        let prompt_inp = dlg.prompt_input.clone();
        let use_wt = dlg.use_worktree;
        let companion = dlg.companion_shell;
        let selected = dlg.selected_agents.clone();
        let choices = dlg.available_agents.clone();
        let err = dlg.error.clone();

        let field_label = |s: &'static str| {
            div()
                .text_size(px(ui::fs_small()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(c(th.subtext))
                .child(s)
        };

        let mut agent_chips = div().flex().flex_wrap().gap(px(ui::SP_1));
        for (i, (id, label, _)) in choices.iter().enumerate() {
            let on = selected.iter().any(|x| x == id);
            let id_c = id.clone();
            agent_chips = agent_chips.child(ui::chip(("ag-nt-ag", i), label.clone(), on).on_click(
                cx.listener(move |this, _, _, cx| {
                    if let Some(d) = this.agent_hub.new_task.as_mut() {
                        if let Some(pos) = d.selected_agents.iter().position(|x| x == &id_c) {
                            if d.selected_agents.len() > 1 {
                                d.selected_agents.remove(pos);
                            }
                        } else {
                            d.selected_agents.push(id_c.clone());
                        }
                    }
                    cx.notify();
                }),
            ));
        }

        let recent_dirs = self.recent_project_dirs();
        let cur_repo_val = repo_inp.read(cx).value().trim().to_string();
        let mut recent_row = div().flex().flex_wrap().gap(px(ui::SP_1));
        for (ri, (rname, rpath)) in recent_dirs.into_iter().take(4).enumerate() {
            let active = cur_repo_val == rpath;
            let ri_inp = repo_inp.clone();
            let rp_c = rpath.clone();
            recent_row = recent_row.child(
                ui::chip(("ag-nt-proj", ri), rname, active)
                    .tooltip(ui::tip(abbreviate_home(&rpath)))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        let rp2 = rp_c.clone();
                        ri_inp.update(cx, |inp, cx| inp.set_value(rp2, w, cx));
                        if let Some(d) = this.agent_hub.new_task.as_mut() {
                            if worktree::fast_git_toplevel(Path::new(&rp_c)).is_none() {
                                d.use_worktree = false;
                            }
                        }
                        cx.notify();
                    })),
            );
        }

        let toggle_row = |id: &'static str, checked: bool, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(ui::SP_2))
                .cursor_pointer()
                .child(
                    div()
                        .size(px(16.))
                        .rounded(px(ui::R_SM))
                        .border_1()
                        .border_color(if checked {
                            c(th.accent)
                        } else {
                            crate::theme::divider()
                        })
                        .bg(if checked {
                            c(th.accent)
                        } else {
                            gpui::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(checked, |d| {
                            d.child(icon(IconName::Check, ui::IC_XS, c(th.base)))
                        }),
                )
                .child(
                    div()
                        .text_size(px(ui::fs_body()))
                        .text_color(c(th.text))
                        .child(label),
                )
        };

        let close_btn = ui::icon_button("ag-nt-x", IconName::X, ui::IconBtn::Md, t("ui.cancel"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.agent_hub.new_task = None;
                cx.notify();
            }))
            .into_any_element();

        ui::modal(
            ui::card(520.)
                .child(ui::dialog_header(
                    Some(IconName::Workflow),
                    c(th.accent),
                    t("ag.task_title"),
                    None,
                    Some(close_btn),
                ))
                .child(
                    div()
                        .id("ag-nt-form")
                        .p(px(ui::SP_5))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(ui::SP_3))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(ui::SP_1))
                                .child(field_label(t("ag.task_repo")))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(ui::SP_2))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(ui::text_field(&repo_inp, false)),
                                        )
                                        .child(
                                            ui::button("ag-nt-browse", t("ag.browse_dir"), 0)
                                                .gap(px(ui::SP_1))
                                                .on_click(cx.listener(|this, _, w, cx| {
                                                    this.pick_directory_for_new_task(w, cx);
                                                })),
                                        ),
                                )
                                .child(recent_row),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(ui::SP_1))
                                .child(field_label(t("ag.task_name")))
                                .child(ui::text_field(&name_inp, false)),
                        )
                        .child(
                            toggle_row("ag-nt-wt", use_wt, t("ag.task_worktree")).on_click(
                                cx.listener(|this, _, _, cx| {
                                    if let Some(d) = this.agent_hub.new_task.as_mut() {
                                        d.use_worktree = !d.use_worktree;
                                    }
                                    cx.notify();
                                }),
                            ),
                        )
                        .when(use_wt, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(ui::SP_1))
                                    .child(field_label(t("ag.task_base")))
                                    .child(ui::text_field(&base_inp, false)),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(ui::SP_1))
                                .child(field_label(t("ag.task_agents")))
                                .child(agent_chips),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(ui::SP_1))
                                .child(field_label(t("ag.task_prompt")))
                                .child(ui::text_field(&prompt_inp, true)),
                        )
                        .when(selected.len() <= 1, |d| {
                            d.child(
                                toggle_row("ag-nt-comp", companion, t("ag.task_companion"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(d) = this.agent_hub.new_task.as_mut() {
                                            d.companion_shell = !d.companion_shell;
                                        }
                                        cx.notify();
                                    })),
                            )
                        })
                        .when_some(err, |d, e| d.child(ui::notice(ui::Notice::Error, e))),
                )
                .child(
                    ui::dialog_footer(None, vec![])
                        .child(ui::button("ag-nt-cancel", t("ui.cancel"), 0).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.agent_hub.new_task = None;
                                cx.notify();
                            }),
                        ))
                        .child(ui::button("ag-nt-ok", t("ag.task_create"), 1).on_click(
                            cx.listener(|this, _, w, cx| this.submit_new_task(w, cx)),
                        )),
                ),
        )
        .into_any_element()
    }

    fn render_finish_task_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let dlg = self.agent_hub.finish_task.clone().unwrap();
        let wt_str = dlg
            .task
            .worktree
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let br_str = dlg.task.branch.clone().unwrap_or_else(|| "-".into());
        let wt_for_diff = dlg.task.worktree.clone();

        let close_btn = ui::icon_button("ag-ft-x", IconName::X, ui::IconBtn::Md, t("ui.cancel"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.agent_hub.finish_task = None;
                cx.notify();
            }))
            .into_any_element();

        ui::modal(
            ui::card(520.)
                .child(ui::dialog_header(
                    Some(IconName::CheckCheck),
                    c(th.accent),
                    format!("{} · {}", t("ag.finish_title"), dlg.task.name),
                    None,
                    Some(close_btn),
                ))
                .child(
                    div()
                        .id("ag-ft-form")
                        .p(px(ui::SP_5))
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(ui::SP_3))
                        .child(ui::small(tf("ag.finish_branch", &[("b", &br_str)])))
                        .when(!wt_str.is_empty(), |d| {
                            d.child(ui::mono(tf("ag.finish_wt", &[("p", &wt_str)])))
                        })
                        .child(if dlg.dirty {
                            ui::notice(ui::Notice::Warn, t("ag.finish_dirty"))
                        } else {
                            ui::notice(ui::Notice::Info, t("ag.finish_clean"))
                        })
                        .when(!dlg.diff_stat.is_empty(), |d| {
                            d.child(
                                div()
                                    .p(px(ui::SP_2))
                                    .rounded(px(ui::R_MD))
                                    .bg(crate::theme::sub_bg())
                                    .max_h(px(120.))
                                    .id("ag-ft-diffstat")
                                    .overflow_y_scroll()
                                    .child(ui::mono(dlg.diff_stat.clone())),
                            )
                        })
                        .when(dlg.dirty, |d| {
                            d.child(
                                div()
                                    .id("ag-ft-force")
                                    .flex()
                                    .items_center()
                                    .gap(px(ui::SP_2))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if let Some(f) = this.agent_hub.finish_task.as_mut() {
                                            f.force = !f.force;
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .size(px(16.))
                                            .rounded(px(ui::R_SM))
                                            .border_1()
                                            .border_color(if dlg.force {
                                                c(th.error)
                                            } else {
                                                crate::theme::divider()
                                            })
                                            .bg(if dlg.force {
                                                c(th.error)
                                            } else {
                                                gpui::transparent_black()
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(dlg.force, |el| {
                                                el.child(icon(
                                                    IconName::Check,
                                                    ui::IC_XS,
                                                    c(th.base),
                                                ))
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(ui::fs_small()))
                                            .text_color(c(th.error))
                                            .child(t("ag.finish_force")),
                                    ),
                            )
                        })
                        .when_some(dlg.error, |d, e| d.child(ui::notice(ui::Notice::Error, e))),
                )
                .child(
                    ui::dialog_footer(
                        wt_for_diff.map(|wt| {
                            ui::button("ag-ft-diff", t("ag.finish_diff"), 0)
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.agent_hub.finish_task = None;
                                    let pane = this.open_pane_with(
                                        Spec::Local,
                                        PaneInit {
                                            cwd: Some(wt.display().to_string()),
                                            ..Default::default()
                                        },
                                        w,
                                        cx,
                                    );
                                    pane.update(cx, |v, _| v.ws = Some(Tool::Agents));
                                    if let Some(tab) = this.tabs.get_mut(dlg.tab_idx) {
                                        let anchor = tab.focused.clone();
                                        tab.layout.split(&anchor, pane.clone(), Dir::Vertical);
                                        tab.focused = pane.clone();
                                    }
                                    send_when_shell_ready(pane, "git diff".into(), cx);
                                    cx.notify();
                                }))
                                .into_any_element()
                        }),
                        vec![],
                    )
                    .child(
                        ui::button("ag-ft-keep", t("ag.finish_close_only"), 0).on_click(
                            cx.listener(|this, _, w, cx| this.execute_finish_task(false, w, cx)),
                        ),
                    )
                    .child(if dlg.dirty && !dlg.force {
                        ui::button_disabled("ag-ft-rm", t("ag.finish_remove_wt"), 2)
                    } else {
                        ui::button("ag-ft-rm", t("ag.finish_remove_wt"), 2).on_click(
                            cx.listener(|this, _, w, cx| this.execute_finish_task(true, w, cx)),
                        )
                    }),
                ),
        )
        .into_any_element()
    }

    /// Lightweight count of agent/workbench panes for center footer (avoids screen text extraction).
    fn count_running_agent_panes(&self, cx: &App) -> usize {
        let mut count = 0usize;
        for tab in &self.tabs {
            for pane in tab.layout.leaves() {
                let v = pane.read(cx);
                if v.agent_label().is_some() || v.ws == Some(Tool::Agents) {
                    count += 1;
                }
            }
        }
        count
    }

    /// Bottom bar inside the center editor card when `self.tool == Tool::Agents`.
    pub(crate) fn agent_center_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let (att_count, att_urgent) = attention::summary_and_sync_badge(self, cx);
        let run_count = self.count_running_agent_panes(cx);
        let total_hist = self.agent_hub.sessions.len();

        let (focus_label, focus_working, branch_label, ctx_info, has_wt_tab, active_tab_idx) = self
            .tabs
            .get(self.active)
            .map(|tab| {
                let v = tab.focused.read(cx);
                let ag_lbl = v.agent_label();
                let working = ag_lbl
                    .as_ref()
                    .is_some_and(|(_, st)| *st == AgentState::Working);
                let ag = ag_lbl
                    .map(|(n, st)| format!("{n} · {}", st.label()))
                    .unwrap_or_default();
                let br = tab
                    .task
                    .as_ref()
                    .and_then(|tk| {
                        let b = tk.branch.as_deref()?;
                        Some(if tk.worktree.is_some() {
                            format!("{b} [worktree]")
                        } else {
                            b.to_string()
                        })
                    })
                    .or_else(|| {
                        v.shell_cwd()
                            .as_deref()
                            .map(Path::new)
                            .and_then(worktree::fast_git_branch)
                    })
                    .unwrap_or_default();
                let bound_meta = v
                    .agent_session_id
                    .as_deref()
                    .and_then(|sid| self.agent_hub.sessions.iter().find(|m| m.id == sid));
                let screen_ctx = v
                    .session
                    .as_ref()
                    .and_then(|s| agent::screen_context_usage(&s.screen_text()));
                let ctx_info = resolve_context(bound_meta, screen_ctx.as_ref());
                let wt = tab.task.as_ref().and_then(|t| t.worktree.as_ref()).is_some();
                (ag, working, br, ctx_info, wt, self.active)
            })
            .unwrap_or_default();

        div()
            .id("ag-center-bar")
            .h(px(26.))
            .w_full()
            .flex_shrink_0()
            .px(px(ui::SP_3))
            .bg(crate::theme::bar_bg())
            .border_t_1()
            .border_color(crate::theme::divider())
            .rounded_bl(px(crate::card_layout::CARD_RADIUS))
            .rounded_br(px(crate::card_layout::CARD_RADIUS))
            .flex()
            .items_center()
            .gap(px(ui::SP_2))
            .text_size(px(ui::fs_small()))
            .text_color(c(th.subtext))
            .child(
                div()
                    .id("ag-bar-summary")
                    .flex()
                    .items_center()
                    .gap(px(ui::SP_1))
                    .when(att_count > 0, |d| {
                        d.cursor_pointer()
                            .px(px(ui::SP_1))
                            .rounded(px(ui::R_SM))
                            .hover(|s| s.bg(crate::theme::hover_bg()))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.jump_attention(true, w, cx);
                            }))
                    })
                    .child(ui::icon_svg(
                        "shellax/agent.svg",
                        ui::IC_SM,
                        if att_count > 0 {
                            if att_urgent {
                                c(th.error)
                            } else {
                                c(th.success)
                            }
                        } else {
                            c(th.accent)
                        },
                    ))
                    .child(tf(
                        "ag.summary",
                        &[("w", &att_count), ("r", &run_count), ("h", &total_hist)],
                    )),
            )
            .child(div().flex_1())
            .when(!branch_label.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(ui::SP_1))
                        .child(icon(IconName::FolderGit, ui::IC_XS, c(th.overlay1)))
                        .child(ui::mono(branch_label)),
                )
            })
            .when_some(ctx_info.as_ref(), |d, rc| {
                let tip = format!("{} · {}", rc.tooltip, t("ag.ctx_view"));
                d.child(
                    div()
                        .id("ag-bar-ctx")
                        .cursor_pointer()
                        .px(px(ui::SP_1 + 2.))
                        .py(px(2.))
                        .rounded(px(ui::R_SM))
                        .hover(|s| s.bg(crate::theme::hover_bg()))
                        .tooltip(ui::tip(tip))
                        .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                            this.toggle_context_popover_from_bar(ev.position(), cx);
                        }))
                        .child(render_ctx_ring(Some(rc.frac), &rc.bar_label, true)),
                )
            })
            .when(focus_working, |d| {
                d.child(ui::spinning_icon(
                    "ag-bar-spin",
                    IconName::LoaderCircle,
                    ui::IC_XS,
                    c(th.accent),
                ))
            })
            .when(!focus_label.is_empty(), |d| d.child(focus_label))
            .when(has_wt_tab, |d| {
                d.child(
                    ui::button_sm("ag-bar-finish", t("ag.finish_short"), 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_finish_task_dialog(active_tab_idx, cx);
                        })),
                )
            })
            .child(
                ui::button_sm("ag-bar-new", t("ag.new_task"), 3)
                    .on_click(cx.listener(|this, _, w, cx| this.open_new_task_dialog(w, cx))),
            )
            .into_any_element()
    }
}

const ROW_H_PROJ_FIRST: f32 = 32.0;
const ROW_H_PROJ_NEXT: f32 = 30.0 + ui::SP_2 + 2.0;
const ROW_H_EMPTY_PROJ: f32 = ui::H_DEFAULT + 2.0;
const ROW_H_STATUS_HDR: f32 = ui::SECTION_H + 2.0;
const ROW_H_SESSION: f32 = 42.0;
const ROW_H_LOAD_MORE: f32 = ui::SECTION_H + 2.0;
const VIRTUAL_OVERSCAN_PX: f32 = 220.0;

/// Compute `(start_ix, end_ix, top_pad_px, bottom_pad_px)` via binary search over prefix heights.
fn visible_virtual_range(
    row_tops: &[f32],
    row_ends: &[f32],
    content_height: f32,
    scroll_y: f32,
    viewport_h: f32,
) -> (usize, usize, f32, f32) {
    let n = row_tops.len();
    if n == 0 {
        return (0, 0, 0.0, 0.0);
    }
    let win_top = (scroll_y - VIRTUAL_OVERSCAN_PX).max(0.0);
    let win_bot = scroll_y + viewport_h + VIRTUAL_OVERSCAN_PX;
    let start_ix = row_ends.partition_point(|&end_y| end_y <= win_top).min(n);
    let end_ix = row_tops
        .partition_point(|&top_y| top_y < win_bot)
        .max(start_ix)
        .min(n);
    let top_pad = if start_ix < n {
        row_tops[start_ix]
    } else {
        content_height
    };
    let rendered_end = if end_ix > start_ix {
        row_ends[end_ix - 1]
    } else {
        top_pad
    };
    let bottom_pad = (content_height - rendered_end).max(0.0);
    (start_ix, end_ix, top_pad, bottom_pad)
}

fn reveal_virtual_row(scroll: &ScrollHandle, snap: &AgentListSnapshot, list_ix: usize) {
    let Some(&row_top) = snap.row_tops.get(list_ix) else {
        return;
    };
    let Some(&row_bot) = snap.row_ends.get(list_ix) else {
        return;
    };
    let vp_h = f32::from(scroll.bounds().size.height);
    if vp_h <= 1.0 {
        return;
    }
    let cur_y = (-f32::from(scroll.offset().y)).max(0.0);
    let sticky_pad = if row_top > 34.0 { 34.0 } else { 0.0 };
    if row_top < cur_y + sticky_pad {
        scroll.set_offset(gpui::point(px(0.), px(-(row_top - sticky_pad).max(0.0))));
    } else if row_bot > cur_y + vp_h {
        let max_scroll = (snap.total_height - vp_h).max(0.0);
        scroll.set_offset(gpui::point(
            px(0.),
            px(-(row_bot - vp_h).clamp(0.0, max_scroll)),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_session_row(
    nav_idx: usize,
    meta: SessionMeta,
    live_state: Option<AgentState>,
    active_open: bool,
    archived: bool,
    ctx_frac: Option<f32>,
    ctx_label: Option<String>,
    live_title: Option<String>,
    now_secs: i64,
) -> PreparedSessionRow {
    let age = SharedString::from(agent_history::relative_time_short(meta.updated, now_secs));
    let title_str = if meta.title.is_some() || meta.first_prompt.is_some() {
        meta.display_title().to_string()
    } else if let Some(lt) = live_title.filter(|s| !s.is_empty()) {
        lt
    } else {
        meta.display_title().to_string()
    };

    let mut sub_parts = Vec::with_capacity(3);
    if let Some(st) = live_state {
        sub_parts.push(st.label().to_string());
    }
    if let Some(b) = meta.branch.as_deref().filter(|s| !s.is_empty()) {
        sub_parts.push(b.to_string());
    }
    if meta.messages > 0 {
        sub_parts.push(tf("ag.msgs", &[("n", &meta.messages)]));
    } else {
        sub_parts.push(agent::display_name(&meta.agent).to_string());
    }
    let sub_str = sub_parts.join(" · ");

    let (eff_ctx_frac, eff_ctx_label_str, eff_ctx_tip_str) =
        if let Some(d) = meta.context_detail() {
            (Some(d.frac), Some(d.short_label), Some(d.tooltip))
        } else if meta.ctx_reset {
            (None, None, None)
        } else if let Some(lbl) = ctx_label {
            let tip = format!("ctx {lbl}");
            (ctx_frac, Some(lbl), Some(tip))
        } else {
            (None, None, None)
        };

    let cwd_short = meta
        .cwd
        .as_deref()
        .map(abbreviate_home)
        .unwrap_or_else(|| "-".into());
    let short_id: String = meta.id.chars().take(12).collect();
    let ctx_tip = eff_ctx_tip_str
        .as_deref()
        .map(|l| format!("  ·  {l}"))
        .unwrap_or_default();
    let tip_text = SharedString::from(format!(
        "{title_str}\n{cwd_short}  ·  {sub_str}{ctx_tip}  ·  {age}  ({short_id}…)"
    ));
    let row_gid = SharedString::from(format!("ag-hrg-{}-{}", meta.agent, meta.id));
    let row_id = SharedString::from(format!("ag-hs-{}-{}", meta.agent, meta.id));
    let wt_id = SharedString::from(format!("ag-h-wt-{}-{}", meta.agent, meta.id));
    let arch_id = SharedString::from(format!("ag-h-ar-{}-{}", meta.agent, meta.id));
    let spin_id = SharedString::from(format!("ag-hspin-{}-{}", meta.agent, meta.id));

    PreparedSessionRow {
        nav_idx,
        meta,
        live_state,
        active_open,
        archived,
        eff_ctx_frac,
        eff_ctx_label: eff_ctx_label_str.map(SharedString::from),
        title: SharedString::from(title_str),
        sub_line: SharedString::from(sub_str),
        age,
        tip_text,
        row_id,
        row_gid,
        wt_id,
        arch_id,
        spin_id,
    }
}

pub(crate) struct AgentListView {
    pub root: WeakEntity<Root>,
    pub scroll: ScrollHandle,
    pub snap: Option<Arc<AgentListSnapshot>>,
    pub sel_idx: usize,
    pub kbd_active: bool,
}

impl gpui::Render for AgentListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root_ent) = self.root.upgrade() else {
            return div().size_full();
        };
        let Some(ref snap) = self.snap else {
            return div().size_full();
        };
        let vp = self.scroll.bounds();
        let vp_h = f32::from(vp.size.height);
        let eff_vp_h = if vp_h > 1.0 { vp_h } else { 900.0 };
        let max_scroll = (snap.total_height - eff_vp_h).max(0.0);
        let scroll_y = (-f32::from(self.scroll.offset().y)).clamp(0.0, max_scroll);

        let (start_ix, end_ix, top_pad, bottom_pad) = visible_virtual_range(
            &snap.row_tops,
            &snap.row_ends,
            snap.content_height,
            scroll_y,
            eff_vp_h,
        );

        let mut scroll_col = div()
            .id("ag-virtual-scroll")
            .track_scroll(&self.scroll)
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col();

        if top_pad > 0.0 {
            scroll_col = scroll_col.child(div().w_full().h(px(top_pad)).flex_shrink_0());
        }

        let sel_idx = self.sel_idx;
        let kbd_active = self.kbd_active;
        let app: &App = cx;

        for ix in start_ix..end_ix {
            let el: AnyElement = match &snap.rows[ix] {
                AgentFlatRow::ProjectHeader { proj_idx } => {
                    let meta = &snap.projects[*proj_idx];
                    Root::render_project_header_card_el(
                        &root_ent,
                        "ag-proj-hdr",
                        *proj_idx,
                        meta,
                        false,
                        window,
                        app,
                    )
                }
                AgentFlatRow::EmptyProject { proj_idx, cwd } => {
                    let ld = cwd.clone();
                    div()
                        .w_full()
                        .h(px(ROW_H_EMPTY_PROJ))
                        .flex_shrink_0()
                        .px(px(ui::LIST_INSET))
                        .pb(px(2.))
                        .child(
                            div()
                                .id(("ag-proj-empty", *proj_idx))
                                .w_full()
                                .h(px(ui::H_DEFAULT))
                                .px(px(ui::ROW_PX))
                                .rounded(px(ui::R_MD))
                                .cursor_pointer()
                                .hover(|s| s.bg(crate::theme::hover_bg()))
                                .flex()
                                .items_center()
                                .gap(px(ui::SP_1))
                                .on_click(window.listener_for(&root_ent, move |this, _, w, cx| {
                                    this.open_new_task_dialog_in(ld.clone(), w, cx);
                                }))
                                .child(icon(IconName::Plus, ui::IC_XS, c(cur().overlay1)))
                                .child(ui::row_meta(t("ag.start_in_project"))),
                        )
                        .into_any_element()
                }
                AgentFlatRow::StatusHeader {
                    proj_idx,
                    proj_key,
                    status,
                    count,
                    collapsed,
                } => Root::render_status_group_header_el(
                    &root_ent,
                    *proj_idx,
                    proj_key.clone(),
                    *status,
                    count.clone(),
                    *collapsed,
                    window,
                    app,
                ),
                AgentFlatRow::Running { run_idx, row } => {
                    Root::render_running_row_el(&root_ent, *run_idx, row.clone(), window, app)
                }
                AgentFlatRow::Session(prepared) => {
                    let kbd_sel =
                        kbd_active && prepared.nav_idx == sel_idx && !prepared.active_open;
                    Root::render_grouped_session_row_el(&root_ent, prepared, kbd_sel, window, app)
                }
                AgentFlatRow::LoadMore {
                    proj_idx,
                    proj_key,
                    status,
                    remaining,
                } => {
                    let pkey = proj_key.clone();
                    let st = *status;
                    div()
                        .w_full()
                        .h(px(ROW_H_LOAD_MORE))
                        .flex_shrink_0()
                        .px(px(ui::LIST_INSET))
                        .pb(px(2.))
                        .child(
                            div()
                                .id(("ag-grp-more", *proj_idx * 16 + *status as usize))
                                .w_full()
                                .h(px(ui::SECTION_H))
                                .px(px(ui::ROW_PX))
                                .rounded(px(ui::R_MD))
                                .cursor_pointer()
                                .hover(|s| s.bg(crate::theme::hover_bg()))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(ui::fs_caption()))
                                .text_color(c(cur().accent))
                                .child(tf("ag.load_more", &[("n", remaining)]))
                                .on_click(window.listener_for(&root_ent, move |this, _, _, cx| {
                                    let entry = this
                                        .agent_hub
                                        .group_limits
                                        .entry((pkey.clone(), st))
                                        .or_insert(80);
                                    *entry += 80;
                                    this.agent_hub.invalidate_layout();
                                    cx.notify();
                                })),
                        )
                        .into_any_element()
                }
            };
            scroll_col = scroll_col.child(el);
        }

        scroll_col = scroll_col.child(
            div()
                .w_full()
                .h(px(bottom_pad + ui::SP_2))
                .flex_shrink_0(),
        );

        let sticky_hdr: Option<AnyElement> =
            if vp_h > 1.0 && scroll_y > 0.5 && !snap.projects.is_empty() {
                let next_idx = snap
                    .projects
                    .partition_point(|p| p.card_top_y <= scroll_y + 0.5);
                if next_idx > 0 {
                    let pos = next_idx - 1;
                    let meta = &snap.projects[pos];
                    if meta.collapsed {
                        None
                    } else {
                        let push_y = snap
                            .projects
                            .get(pos + 1)
                            .map(|nxt| nxt.card_top_y - scroll_y)
                            .filter(|&nry| nry > 0.0 && nry < 34.0)
                            .map(|nry| nry - 34.0)
                            .unwrap_or(0.0);
                        Some(
                            div()
                                .id("ag-proj-sticky-wrap")
                                .absolute()
                                .top(px(push_y))
                                .left_0()
                                .right_0()
                                .flex()
                                .flex_col()
                                .bg(crate::theme::solid_panel_bg())
                                .pb(px(3.))
                                .occlude()
                                .child(Root::render_project_header_card_el(
                                    &root_ent,
                                    "ag-proj-sticky",
                                    pos,
                                    meta,
                                    true,
                                    window,
                                    app,
                                ))
                                .into_any_element(),
                        )
                    }
                } else {
                    None
                }
            } else {
                None
            };

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .size_full()
            .overflow_hidden()
            .child(scroll_col)
            .children(sticky_hdr)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LiveRowSig {
    pane_id: gpui::EntityId,
    agent_id: Option<String>,
    state: AgentState,
    selected: bool,
    session_id: Option<String>,
    title: String,
    ctx_label: Option<String>,
    ctx_permille: Option<i32>,
    cwd: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AgentListCacheKey {
    sessions_rev: u64,
    layout_rev: u64,
    query: String,
    filter: String,
    focused_sid: Option<String>,
    time_minute: i64,
    recent_projects: Vec<String>,
    live: Vec<LiveRowSig>,
}

pub(crate) struct PreparedSessionRow {
    nav_idx: usize,
    meta: SessionMeta,
    live_state: Option<AgentState>,
    active_open: bool,
    archived: bool,
    eff_ctx_frac: Option<f32>,
    eff_ctx_label: Option<SharedString>,
    title: SharedString,
    sub_line: SharedString,
    age: SharedString,
    tip_text: SharedString,
    row_id: SharedString,
    row_gid: SharedString,
    wt_id: SharedString,
    arch_id: SharedString,
    spin_id: SharedString,
}

pub(crate) enum AgentFlatRow {
    ProjectHeader {
        proj_idx: usize,
    },
    EmptyProject {
        proj_idx: usize,
        cwd: Option<String>,
    },
    StatusHeader {
        proj_idx: usize,
        proj_key: String,
        status: SessionGroupStatus,
        count: SharedString,
        collapsed: bool,
    },
    Running {
        run_idx: usize,
        row: RunningRow,
    },
    Session(PreparedSessionRow),
    LoadMore {
        proj_idx: usize,
        proj_key: String,
        status: SessionGroupStatus,
        remaining: usize,
    },
}

pub(crate) struct AgentListSnapshot {
    key: AgentListCacheKey,
    filter_total: usize,
    filter_counts: Vec<(String, usize)>,
    running_active_count: usize,
    archived_total: usize,
    active_total: usize,
    projects: Vec<ProjectHeaderMeta>,
    rows: Vec<AgentFlatRow>,
    row_tops: Vec<f32>,
    row_ends: Vec<f32>,
    content_height: f32,
    total_height: f32,
    nav_row_indices: Vec<usize>,
    nav_sessions: Vec<SessionMeta>,
}

pub(crate) struct ProjectHeaderMeta {
    key: String,
    name: SharedString,
    cwd: String,
    short_cwd: SharedString,
    toggle_tip: SharedString,
    total_count: usize,
    has_attention: bool,
    has_running: bool,
    collapsed: bool,
    hdr_top_y: f32,
    card_top_y: f32,
}

struct ProjectBucket {
    key: String,
    name: String,
    cwd: String,
    total_count: usize,
    has_attention: bool,
    has_running: bool,
    has_open: bool,
    most_recent: i64,
    groups: Vec<(SessionGroupStatus, Vec<GroupedItem>)>,
}

struct LivePaneInfo {
    state: AgentState,
    selected: bool,
    ctx_frac: Option<f32>,
    ctx_label: Option<String>,
    live_title: Option<String>,
}

enum GroupedItem {
    Session {
        meta: SessionMeta,
        live_state: Option<AgentState>,
        active_open: bool,
        archived: bool,
        ctx_frac: Option<f32>,
        ctx_label: Option<String>,
        live_title: Option<String>,
    },
    LiveOnly(RunningRow),
}

#[derive(Clone)]
pub(crate) struct RunningRow {
    pane: Entity<TermView>,
    tab: usize,
    #[allow(dead_code)]
    agent: String,
    agent_id: Option<String>,
    state: AgentState,
    title: String,
    sub: String,
    selected: bool,
    has_worktree: bool,
    session_id: Option<String>,
    cwd: Option<String>,
    project_name: Option<String>,
    ctx_frac: Option<f32>,
    ctx_label: Option<String>,
}

fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_hub_revs_and_group_collapse() {
        let mut hub = AgentHub::default();
        assert_eq!(hub.sessions_rev, 0);
        assert_eq!(hub.layout_rev, 0);

        hub.invalidate_sessions();
        assert_eq!(hub.sessions_rev, 1);

        // Regular groups default to expanded, Archived defaults to collapsed.
        assert!(!hub.is_group_collapsed("proj", SessionGroupStatus::Idle));
        assert!(hub.is_group_collapsed("proj", SessionGroupStatus::Archived));

        hub.toggle_group_collapsed("proj", SessionGroupStatus::Idle);
        assert!(hub.is_group_collapsed("proj", SessionGroupStatus::Idle));
        assert_eq!(hub.layout_rev, 1);

        hub.toggle_group_collapsed("proj", SessionGroupStatus::Archived);
        assert!(!hub.is_group_collapsed("proj", SessionGroupStatus::Archived));
        assert_eq!(hub.layout_rev, 2);

        hub.scroll_to_top_reset();
        assert!(hub.reset_scroll_pending);
    }

    #[test]
    fn visible_virtual_range_preserves_exact_total_height() {
        let mut tops = Vec::new();
        let mut ends = Vec::new();
        let mut cur = 0.0f32;
        for i in 0..500 {
            let h = match i {
                0 => ROW_H_PROJ_FIRST,
                1 | 251 => ROW_H_STATUS_HDR,
                250 => ROW_H_PROJ_NEXT,
                _ => ROW_H_SESSION,
            };
            tops.push(cur);
            cur += h;
            ends.push(cur);
        }
        let total = cur;
        for scroll_y in [0.0, 48.0, 950.0, 6400.0, 18000.0, total] {
            let (s, e, top_pad, bot_pad) =
                visible_virtual_range(&tops, &ends, total, scroll_y, 640.0);
            assert!(s <= e && e <= 500);
            let slice_h: f32 = (s..e).map(|ix| ends[ix] - tops[ix]).sum();
            assert!((top_pad + slice_h + bot_pad - total).abs() < 0.01);
            assert!(e - s <= 28, "rendered slice must stay small, got {}", e - s);
        }
    }

    #[test]
    fn resolve_context_prefers_transcript_and_honors_compact_or_clear_reset() {
        let base_meta = SessionMeta {
            agent: "claude".into(),
            id: "s1".into(),
            path: PathBuf::from("/tmp/s1.jsonl"),
            cwd: Some("/repo".into()),
            project: "repo".into(),
            branch: None,
            title: None,
            first_prompt: None,
            last_prompt: None,
            started: 1,
            updated: 2,
            messages: 4,
            ctx_used: Some(700_000),
            ctx_window: Some(1_000_000),
            ctx_reset: false,
            continued_in: None,
        };
        let noisy_screen = agent::ScreenContextUsage {
            frac: 0.15,
            short_label: "15%".into(),
            bar_label: "15%".into(),
            used_tokens: None,
            window_tokens: None,
        };

        // 1. Transcript always takes priority over screen scraping.
        let r = resolve_context(Some(&base_meta), Some(&noisy_screen)).expect("resolved");
        assert_eq!(r.short_label, "70%");
        assert_eq!(r.bar_label, "70% · 700k/1M");

        // 2. Right after `/compact` (`ctx_used = None, ctx_reset = true`), stale screen text is suppressed.
        let mut compacted = base_meta.clone();
        compacted.ctx_used = None;
        compacted.ctx_reset = true;
        assert_eq!(resolve_context(Some(&compacted), Some(&noisy_screen)), None);

        // 3. Right after `/clear` (`ctx_used = Some(0), ctx_reset = true`), displays `0%`.
        let mut cleared = base_meta.clone();
        cleared.ctx_used = Some(0);
        cleared.ctx_reset = true;
        let rc = resolve_context(Some(&cleared), Some(&noisy_screen)).expect("cleared");
        assert_eq!(rc.frac, 0.0);
        assert_eq!(rc.short_label, "0%");
        assert_eq!(rc.bar_label, "0%");

        // 4. Without transcript (e.g. remote SSH), falls back to strict screen footer.
        let rs = resolve_context(None, Some(&noisy_screen)).expect("screen fallback");
        assert_eq!(rs.short_label, "15%");
        assert_eq!(rs.bar_label, "15%");

        // 5. Segmented bar widths stay within total bar width even with multiple tiny segments.
        let segs = vec![2_000, 4_400, 6_100, 4_800, 7_900, 1_500, 473_300];
        let widths = segmented_bar_widths(&segs, 500_000, 312.0, 1.5);
        assert_eq!(widths.len(), segs.len());
        let sum_w: f32 = widths.iter().sum();
        assert!(sum_w + 1.5 * 6.0 <= 312.1, "bar must not overflow: {sum_w}");
        assert!(widths.iter().all(|&w| w >= 2.5), "tiny segments stay visible: {widths:?}");
    }

    #[test]
    fn compact_project_label_keeps_short_names_and_truncates_long_ones() {
        assert_eq!(compact_display_units("ShellAX", 8, 6), "ShellAX");
        assert_eq!(compact_display_units("frontend", 8, 6), "frontend");
        assert_eq!(compact_display_units("测试工程", 8, 6), "测试工程");
        assert_eq!(compact_display_units("VibeCoding", 8, 6), "VibeCo…");
        assert_eq!(compact_display_units("企业智能运维平台", 8, 6), "企业智…");
        assert_eq!(
            compact_project_label("/tmp/some-nonexistent-repo-name"),
            "some-n…"
        );
    }
}
