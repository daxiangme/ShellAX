//! Per-module context for the built-in AI assistant: what the assistant may read about the
//! database / Redis / container view the user is looking at, and how its answers are used.
//! Everything here is read-only; generated SQL / commands are only inserted, never run.
use gpui::{App, WeakEntity};

use crate::db_view::DbView;
use crate::dock_view::DockView;
use crate::kv_view::KvView;

/// The view the assistant currently talks about.
#[derive(Clone, Default)]
pub enum Target {
    /// terminal module: the terminal context path (agent hints, plans) applies
    #[default]
    Terminal,
    Db(WeakEntity<DbView>),
    Redis(WeakEntity<KvView>),
    Docker(WeakEntity<DockView>),
    /// a module without assistant context (files, documents, …)
    Other,
}

impl Target {
    pub fn is_terminal(&self) -> bool { matches!(self, Target::Terminal) }
    pub fn id(&self) -> Option<gpui::EntityId> {
        match self { Target::Db(w) => Some(w.entity_id()), Target::Redis(w) => Some(w.entity_id()), Target::Docker(w) => Some(w.entity_id()), _ => None }
    }
    pub fn same(&self, o: &Target) -> bool { std::mem::discriminant(self) == std::mem::discriminant(o) && self.id() == o.id() }
    pub fn system_prompt(&self) -> &'static str {
        match self {
            Target::Db(_) => DB_SYSTEM_PROMPT,
            Target::Redis(_) => REDIS_SYSTEM_PROMPT,
            Target::Docker(_) => DOCKER_SYSTEM_PROMPT,
            _ => ssh_term_proto::ai::CHAT_SYSTEM_PROMPT,
        }
    }
    /// Short label for the context chip, or None when the view is gone.
    pub fn label(&self, cx: &App) -> Option<String> {
        match self {
            Target::Db(w) => w.upgrade().map(|v| v.read(cx).ai_label()),
            Target::Redis(w) => w.upgrade().map(|v| v.read(cx).ai_label()),
            Target::Docker(w) => w.upgrade().map(|v| v.read(cx).ai_label()),
            _ => None,
        }
    }
    /// Start collecting the context (`deep` = the analysis agent's extra probes).
    pub fn gather(&self, deep: bool, cx: &App) -> Option<tokio::task::JoinHandle<String>> {
        match self {
            Target::Db(w) => w.upgrade().and_then(|v| v.read(cx).ai_context(deep, cx)),
            Target::Redis(w) => w.upgrade().and_then(|v| v.read(cx).ai_context(deep)),
            Target::Docker(w) => w.upgrade().and_then(|v| v.read(cx).ai_context(deep)),
            _ => None,
        }
    }
    /// Quick actions: (button label key, question key, deep).
    pub fn quick(&self) -> &'static [(&'static str, &'static str, bool)] {
        match self {
            Target::Db(_) => &[("aictx.db_crud", "aictx.db_crud_q", false), ("aictx.db_perf", "aictx.db_perf_q", true)],
            Target::Redis(_) => &[("aictx.kv_analyze", "aictx.kv_analyze_q", true)],
            Target::Docker(_) => &[("aictx.dk_diagnose", "aictx.dk_diagnose_q", true), ("aictx.dk_compose", "aictx.dk_compose_q", false)],
            _ => &[],
        }
    }
}

const RULES: &str = " Reply in plain sentences without bold, headings or numbered lists. Nothing you write is executed automatically: \
the user reviews and runs it. Say so when an action is destructive, and be extra careful when the context says production.";
pub const DB_SYSTEM_PROMPT: &str = "You are the shellax database assistant. The user message carries the schema of the current connection (truncated) \
and, for performance questions, EXPLAIN output, index lists and slow-query statistics. Write SQL for the stated database engine in ```sql fenced blocks, \
one block per statement group, using only tables and columns that exist in the schema. For performance work explain the plan, then suggest concrete \
indexes (CREATE INDEX statements) and query rewrites. Never claim you ran anything.";
pub const REDIS_SYSTEM_PROMPT: &str = "You are the shellax Redis assistant. The user message carries INFO excerpts, key counts and a sample of keys with \
types, and for analysis requests memory usage of sampled keys and the slow log. Write Redis commands one per line in ```redis fenced blocks. \
Never suggest KEYS or FLUSHALL on large or production instances; prefer SCAN and UNLINK. For analysis name big or hot keys, memory risks and slow commands, with fixes.";
pub const DOCKER_SYSTEM_PROMPT: &str = "You are the shellax container assistant. The user message carries the container list with states, and for \
diagnosis CPU/memory samples and recent logs of failing or restarting containers. Write docker CLI commands in ```sh fenced blocks and compose files in \
```yaml fenced blocks. For diagnosis state the most likely cause from the logs first, then one fix.";

pub fn full_prompt(base: &str) -> String { format!("{base}{RULES}") }

/// Keep a context block within `max` chars (on a char boundary), noting the cut.
pub fn cap(mut s: String, max: usize) -> String {
    if s.chars().count() > max {
        s = s.chars().take(max).collect();
        s.push_str("\n…(truncated)");
    }
    s
}

/// Plain-text table of a result set (first `rows` rows).
pub fn table_text(rs: &ssh_term_proto::db::ResultSet, rows: usize) -> String {
    let mut out = rs.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" | ");
    for r in rs.rows.iter().take(rows) {
        out.push('\n');
        out.push_str(&r.iter().map(|v| v.as_deref().unwrap_or("NULL").replace('\n', " ")).collect::<Vec<_>>().join(" | "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cap_and_prompts() {
        assert_eq!(cap("abcdef".into(), 10), "abcdef");
        assert!(cap("é".repeat(20), 5).starts_with("ééééé\n…"));
        assert!(full_prompt(DB_SYSTEM_PROMPT).contains("Nothing you write is executed automatically"));
        assert!(Target::Terminal.is_terminal() && Target::Other.quick().is_empty());
    }
}
