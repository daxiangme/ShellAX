//! Unified global Attention Queue (M3): aggregates pending policy approvals, blocked agents,
//! and finished-unread agents across all open tabs, ordered by urgency and wait duration.

use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{App, Context, Entity, Window};
use ssh_term_proto::agent::AgentState;
use ssh_term_proto::i18n::t;

use crate::termview::TermView;
use crate::Root;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttentionKind {
    Approval(u64),
    Blocked,
    DoneUnread,
}

impl AttentionKind {
    fn rank(&self) -> u8 {
        match self {
            Self::Approval(_) => 0,
            Self::Blocked => 1,
            Self::DoneUnread => 2,
        }
    }

    pub(crate) fn is_urgent(&self) -> bool {
        matches!(self, Self::Approval(_) | Self::Blocked)
    }
}

#[derive(Clone)]
pub(crate) struct AttentionItem {
    pub kind: AttentionKind,
    pub pane: Option<Entity<TermView>>,
    pub agent: String,
    pub title: String,
    pub subtitle: String,
    pub wait_secs: u64,
}

/// Collect all items currently needing human attention, sorted by priority then longest wait.
pub(crate) fn collect(root: &Root, cx: &App) -> Vec<AttentionItem> {
    let mut items = Vec::new();

    for ap in &root.approvals {
        let elapsed = ap
            .deadline
            .checked_duration_since(std::time::Instant::now())
            .map(|rem| {
                config::approval_total_secs().saturating_sub(rem.as_secs())
            })
            .unwrap_or(0);
        items.push(AttentionItem {
            kind: AttentionKind::Approval(ap.id),
            pane: None,
            agent: ap.principal.clone(),
            title: ap.req.command.chars().take(100).collect(),
            subtitle: t("ag.needs_approval").to_string(),
            wait_secs: elapsed,
        });
    }

    for tab in &root.tabs {
        for pane in tab.layout.leaves() {
            let v = pane.read(cx);
            let Some((agent_name, st)) = v.agent_label() else {
                continue;
            };
            let kind = match st {
                AgentState::Blocked => AttentionKind::Blocked,
                AgentState::Done => AttentionKind::DoneUnread,
                _ => continue,
            };
            let title = tab
                .name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| {
                    let cap = v.agent_caption();
                    if cap.is_empty() {
                        agent_name.clone()
                    } else {
                        cap
                    }
                });
            let subtitle = match &kind {
                AttentionKind::Blocked => t("ag.needs_input").to_string(),
                AttentionKind::DoneUnread => t("ag.done_unread").to_string(),
                AttentionKind::Approval(_) => String::new(),
            };
            items.push(AttentionItem {
                kind,
                pane: Some(pane.clone()),
                agent: agent_name,
                title,
                subtitle,
                wait_secs: v.agent_state_since.elapsed().as_secs(),
            });
        }
    }

    items.sort_by(|a, b| {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then_with(|| b.wait_secs.cmp(&a.wait_secs))
    });
    items
}

mod config {
    pub fn approval_total_secs() -> u64 {
        crate::config::get().agents.approval_timeout_secs
    }
}

/// Returns `(attention_count, has_urgent_blocked_or_approval)` and updates the OS Dock badge.
pub(crate) fn summary_and_sync_badge(root: &Root, cx: &App) -> (usize, bool) {
    let items = collect(root, cx);
    let count = items.len();
    let urgent = items.iter().any(|i| i.kind.is_urgent());
    sync_dock_badge(count);
    (count, urgent)
}

static LAST_BADGE: AtomicUsize = AtomicUsize::new(0);

#[allow(unexpected_cfgs)]
pub(crate) fn sync_dock_badge(count: usize) {
    if LAST_BADGE.swap(count, Ordering::Relaxed) == count {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        unsafe {
            let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return;
            }
            let tile: *mut Object = msg_send![app, dockTile];
            if tile.is_null() {
                return;
            }
            let label: *mut Object = if count == 0 {
                std::ptr::null_mut()
            } else if let Ok(c) = std::ffi::CString::new(count.to_string()) {
                msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()]
            } else {
                return;
            };
            let _: () = msg_send![tile, setBadgeLabel: label];
        }
    }
}

impl Root {
    /// Jump to the next (or previous) agent pane needing attention (`Ctrl+Shift+J`).
    pub(crate) fn jump_attention(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let items = collect(self, cx);
        let pane_items: Vec<&AttentionItem> = items.iter().filter(|i| i.pane.is_some()).collect();
        if pane_items.is_empty() {
            if !items.is_empty() {
                // Approval modal is already front-and-center; switch to Agents panel if desired.
                self.select_tool(crate::activity::Tool::Agents, window, cx);
            } else {
                self.toast(t("ag.no_attention").to_string(), cx);
            }
            return;
        }
        let cur_pane = self.tabs.get(self.active).map(|t| t.focused.entity_id());
        let cur_pos = cur_pane.and_then(|id| {
            pane_items
                .iter()
                .position(|it| it.pane.as_ref().is_some_and(|p| p.entity_id() == id))
        });
        let n = pane_items.len();
        let next_idx = match cur_pos {
            Some(pos) => {
                if forward {
                    (pos + 1) % n
                } else {
                    (pos + n - 1) % n
                }
            }
            None => 0,
        };
        if let Some(pane) = pane_items[next_idx].pane.clone() {
            self.focus_pane_anywhere(&pane, window, cx);
        }
    }

    /// Switch module + tab + leaf focus to bring `pane` to the front.
    pub(crate) fn focus_pane_anywhere(
        &mut self,
        pane: &Entity<TermView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target_id = pane.entity_id();
        let Some((ti, _)) = self.tabs.iter().enumerate().find(|(_, t)| {
            t.layout.leaves().iter().any(|p| p.entity_id() == target_id)
        }) else {
            return;
        };
        let module = Self::tab_module(&self.tabs[ti], cx);
        if self.tool.workspace() != module {
            self.tool = module;
            let _ = crate::config::update(|s| s.sidebar.tool = module.id().into());
        }
        self.active = ti;
        self.tabs[ti].focused = pane.clone();
        self.leave_settings();
        self.overlay = crate::Overlay::None;
        let fh = pane.read(cx).focus.clone();
        window.focus(&fh, cx);
        cx.notify();
    }
}
