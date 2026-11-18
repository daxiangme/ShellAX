//! UI-thread dispatcher for `/v1/pane/*`, `/v1/agent/*`, and lifecycle hook reports (`PaneCmd`).

use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, Window};
use serde_json::json;
use ssh_term_proto::agent::{self, AgentState};
use ssh_term_proto::layout::Node;
use ssh_term_proto::policy_server::{PaneCmd, PaneInfo};
use ssh_term_proto::session_store::{TaskMeta, TaskState};

use crate::activity::Tool;
use crate::termview::{Spec, TermView};
use crate::{PaneInit, Root, Tab};

pub(crate) struct PaneWaiter {
    pub pane_id: String,
    pub until: Vec<AgentState>,
    pub deadline: Instant,
    pub reply: flume::Sender<Result<serde_json::Value, String>>,
}

fn encode_named_key(k: &str) -> Vec<u8> {
    match k.trim().to_ascii_lowercase().as_str() {
        "enter" | "return" | "cr" => b"\r".to_vec(),
        "esc" | "escape" => b"\x1b".to_vec(),
        "tab" => b"\t".to_vec(),
        "shift+tab" | "s-tab" | "backtab" => b"\x1b[Z".to_vec(),
        "space" => b" ".to_vec(),
        "backspace" | "bs" => b"\x7f".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "ctrl+c" | "c-c" => b"\x03".to_vec(),
        "ctrl+d" | "c-d" => b"\x04".to_vec(),
        "ctrl+z" | "c-z" => b"\x1a".to_vec(),
        "ctrl+l" | "c-l" => b"\x0c".to_vec(),
        "ctrl+u" | "c-u" => b"\x15".to_vec(),
        other => other.as_bytes().to_vec(),
    }
}

fn state_word(st: AgentState) -> &'static str {
    match st {
        AgentState::Idle => "idle",
        AgentState::Working => "working",
        AgentState::Blocked => "blocked",
        AgentState::Done => "done",
        AgentState::Unknown => "unknown",
    }
}

/// Wait asynchronously until the pane's shell prints its prompt (OSC 133/7) or up to `max_wait`,
/// then write `line` + `\r`.
pub(crate) fn send_when_shell_ready(pane: Entity<TermView>, cmd: String, cx: &mut Context<Root>) {
    let clean = cmd.trim().replace(['\r', '\n'], " ");
    if clean.is_empty() {
        return;
    }
    cx.spawn(async move |_, cx| {
        let start = Instant::now();
        loop {
            let sess_ready =
                pane.update(cx, |v, _| v.session.as_ref().map(|s| (s.clone(), s.is_shell_prompt_ready())));
            if let Some((s, prompt_ready)) = sess_ready {
                if prompt_ready || start.elapsed() >= Duration::from_millis(2200) {
                    if !prompt_ready {
                        cx.background_executor()
                            .timer(Duration::from_millis(120))
                            .await;
                    }
                    s.write(format!("{clean}\r").into_bytes());
                    return;
                }
            }
            if start.elapsed() >= Duration::from_secs(6) {
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(60))
                .await;
        }
    })
    .detach();
}

/// Wait up to 30s for `pane` to report an idle/ready agent state, then send `prompt` + `\r`.
fn send_prompt_when_agent_ready(
    pane: Entity<TermView>,
    pane_id: String,
    prompt: String,
    wait_agent_boot: bool,
    reply: flume::Sender<Result<serde_json::Value, String>>,
    cx: &mut Context<Root>,
) {
    cx.spawn(async move |_, cx| {
        let start = Instant::now();
        if wait_agent_boot {
            // Let the shell launch the agent binary first.
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
        }
        while start.elapsed() < Duration::from_secs(30) {
            let (st, sess) = pane.update(cx, |v, _| {
                let st = v.agent.map(|(_, s)| s);
                let sess = v.session.clone();
                (st, sess)
            });
            let Some(sess) = sess else {
                let _ = reply.send(Err("pane closed".into()));
                return;
            };
            if matches!(st, Some(AgentState::Idle | AgentState::Done)) {
                let clean = prompt.trim();
                if !clean.is_empty() {
                    sess.write(clean.as_bytes().to_vec());
                    cx.background_executor()
                        .timer(Duration::from_millis(120))
                        .await;
                    sess.write(b"\r".to_vec());
                }
                let _ = reply.send(Ok(json!({
                    "pane_id": pane_id,
                    "sent": true,
                })));
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
        }
        let _ = reply.send(Ok(json!({
            "pane_id": pane_id,
            "sent": false,
            "hint": "agent did not reach idle within 30s (a trust prompt may be waiting); inspect with `shellax-cli pane read`"
        })));
    })
    .detach();
}

impl Root {
    /// Locate a pane by its stable `pane_id` across all open tabs.
    pub(crate) fn pane_by_id(&self, id: &str, cx: &App) -> Option<(usize, Entity<TermView>)> {
        if id.is_empty() {
            return None;
        }
        for (ti, tab) in self.tabs.iter().enumerate() {
            for p in tab.layout.leaves() {
                if p.read(cx).pane_id == id {
                    return Some((ti, p));
                }
            }
        }
        None
    }

    /// Close a specific pane entity regardless of whether it currently holds focus.
    pub(crate) fn close_pane_entity(
        &mut self,
        target: &Entity<TermView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ti) = self.tabs.iter().position(|t| {
            t.layout
                .leaves()
                .iter()
                .any(|p| p.entity_id() == target.entity_id())
        }) else {
            return false;
        };
        let closed_mod = Self::tab_module(&self.tabs[ti], cx);
        self.end_pane(target, cx);
        if !self.tabs[ti].layout.remove(target) {
            self.tabs.remove(ti);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            }
            self.after_close(closed_mod, ti, cx);
        } else {
            self.tabs[ti].focused = self.tabs[ti].layout.leaves()[0].clone();
        }
        self.focus_active(window, cx);
        cx.notify();
        true
    }

    pub(crate) fn check_pane_waiters(&mut self, cx: &App) {
        if self.pane_waiters.is_empty() {
            return;
        }
        let now = Instant::now();
        let waiters = std::mem::take(&mut self.pane_waiters);
        let mut remaining = Vec::with_capacity(waiters.len());
        for w in waiters {
            if w.reply.is_disconnected() {
                continue;
            }
            match self.pane_by_id(&w.pane_id, cx) {
                None => {
                    let _ = w.reply.send(Err("pane not found".into()));
                }
                Some((_, pane)) => {
                    let st = pane.read(cx).agent.map(|(_, s)| s);
                    if let Some(s) = st.filter(|s| w.until.contains(s)) {
                        let _ = w.reply.send(Ok(json!({
                            "pane_id": w.pane_id,
                            "state": state_word(s),
                        })));
                    } else if now >= w.deadline {
                        let _ = w.reply.send(Err("timeout".into()));
                    } else {
                        remaining.push(w);
                    }
                }
            }
        }
        self.pane_waiters = remaining;
    }

    pub(crate) fn handle_pane_cmd(
        &mut self,
        cmd: PaneCmd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match cmd {
            PaneCmd::List { reply } => {
                let mut out = Vec::new();
                for (ti, tab) in self.tabs.iter().enumerate() {
                    let task_id = tab.task.as_ref().map(|t| t.id.clone());
                    for p in tab.layout.leaves() {
                        let v = p.read(cx);
                        if v.embedded() {
                            continue;
                        }
                        out.push(PaneInfo {
                            pane_id: v.pane_id.clone(),
                            tab: ti,
                            title: tab
                                .name
                                .clone()
                                .unwrap_or_else(|| v.title.to_string()),
                            cwd: v.shell_cwd().unwrap_or_default(),
                            agent: v.agent_id(),
                            state: v.agent.map(|(_, s)| s),
                            session_id: v.agent_session_id.clone(),
                            parent: v.parent_pane_id.clone(),
                            task: task_id.clone(),
                        });
                    }
                }
                let _ = reply.send(out);
            }
            PaneCmd::IsChild {
                caller_pane,
                target_pane,
                reply,
            } => {
                let ok = self
                    .pane_by_id(&target_pane, cx)
                    .is_some_and(|(_, p)| p.read(cx).parent_pane_id.as_deref() == Some(&caller_pane));
                let _ = reply.send(ok);
            }
            PaneCmd::Split {
                caller_pane,
                from,
                dir,
                cmd,
                cwd,
                focus,
                reply,
            } => {
                let anchor_id = from.or_else(|| caller_pane.clone());
                let resolved = anchor_id
                    .as_deref()
                    .and_then(|id| self.pane_by_id(id, cx))
                    .or_else(|| {
                        self.tabs
                            .get(self.active)
                            .map(|t| (self.active, t.focused.clone()))
                    });
                let Some((ti, target)) = resolved else {
                    let _ = reply.send(Err("no active tab to split".into()));
                    return;
                };
                let inherit_cwd = cwd.or_else(|| target.read(cx).shell_cwd());
                let inherit_ws = target.read(cx).ws;
                let pane = self.open_pane_with(
                    Spec::Local,
                    PaneInit {
                        cwd: inherit_cwd,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                let new_id = pane.read(cx).pane_id.clone();
                pane.update(cx, |v, _| {
                    v.parent_pane_id = caller_pane;
                    v.ws = inherit_ws;
                });
                if let Some(tab) = self.tabs.get_mut(ti) {
                    tab.layout.split(&target, pane.clone(), dir);
                    if focus {
                        tab.focused = pane.clone();
                        self.active = ti;
                        let fh = pane.read(cx).focus.clone();
                        window.focus(&fh, cx);
                    }
                }
                if let Some(c) = cmd {
                    send_when_shell_ready(pane, c, cx);
                }
                cx.notify();
                let _ = reply.send(Ok(new_id));
            }
            PaneCmd::SendText {
                pane_id,
                text,
                enter,
                reply,
            } => {
                let Some((_, pane)) = self.pane_by_id(&pane_id, cx) else {
                    let _ = reply.send(Err("pane not found".into()));
                    return;
                };
                let Some(sess) = pane.read(cx).session.clone() else {
                    let _ = reply.send(Err("pane has no terminal session".into()));
                    return;
                };
                let mut payload = text.into_bytes();
                if enter {
                    payload.push(b'\r');
                }
                sess.write(payload);
                let _ = reply.send(Ok(()));
            }
            PaneCmd::SendKeys {
                pane_id,
                keys,
                reply,
            } => {
                let Some((_, pane)) = self.pane_by_id(&pane_id, cx) else {
                    let _ = reply.send(Err("pane not found".into()));
                    return;
                };
                let Some(sess) = pane.read(cx).session.clone() else {
                    let _ = reply.send(Err("pane has no terminal session".into()));
                    return;
                };
                let mut bytes = Vec::new();
                for k in &keys {
                    bytes.extend_from_slice(&encode_named_key(k));
                }
                sess.write(bytes);
                let _ = reply.send(Ok(()));
            }
            PaneCmd::Read {
                pane_id,
                lines,
                reply,
            } => {
                let Some((_, pane)) = self.pane_by_id(&pane_id, cx) else {
                    let _ = reply.send(Err("pane not found".into()));
                    return;
                };
                let Some(sess) = pane.read(cx).session.clone() else {
                    let _ = reply.send(Err("pane has no terminal session".into()));
                    return;
                };
                let _ = reply.send(Ok(sess.recent_text(lines)));
            }
            PaneCmd::Close { pane_id, reply } => {
                let Some((_, pane)) = self.pane_by_id(&pane_id, cx) else {
                    let _ = reply.send(Err("pane not found".into()));
                    return;
                };
                self.close_pane_entity(&pane, window, cx);
                let _ = reply.send(Ok(()));
            }
            PaneCmd::AgentStart {
                caller_pane,
                agent: agent_name,
                cwd,
                prompt,
                task,
                reply,
            } => {
                let raw_cli = agent::default_cli_command(&agent_name);
                let (cli, pre_sid) = agent::prepare_fresh_launch(&agent_name, raw_cli);
                let caller_loc = caller_pane
                    .as_deref()
                    .and_then(|id| self.pane_by_id(id, cx));
                let spawn_cwd = cwd.or_else(|| {
                    caller_loc
                        .as_ref()
                        .and_then(|(_, p)| p.read(cx).shell_cwd())
                });
                let pane = self.open_pane_with(
                    Spec::Local,
                    PaneInit {
                        cwd: spawn_cwd.clone(),
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                let new_id = pane.read(cx).pane_id.clone();
                let auto_t = task.is_none();
                pane.update(cx, |v, _| {
                    v.parent_pane_id = caller_pane;
                    v.ws = Some(Tool::Agents);
                    v.auto_title = auto_t;
                    v.set_launched_agent(&agent_name);
                    if let Some(sid) = pre_sid {
                        v.agent_session_id = Some(sid);
                    }
                });

                if let (Some((ti, target)), None) = (caller_loc, task.as_ref()) {
                    // Split right inside the caller's tab when no standalone task tab was requested.
                    let target_ws = target.read(cx).ws;
                    pane.update(cx, |v, _| v.ws = target_ws);
                    if let Some(tab) = self.tabs.get_mut(ti) {
                        tab.layout
                            .split(&target, pane.clone(), ssh_term_proto::layout::Dir::Vertical);
                    }
                } else {
                    let repo = spawn_cwd
                        .as_deref()
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                    let task_meta = task.map(|name| TaskMeta {
                        id: ssh_term_proto::session_store::uuid_like(),
                        name: name.clone(),
                        repo,
                        worktree: None,
                        branch: None,
                        base: None,
                        agent: agent_name.clone(),
                        created: ssh_term_proto::agent_history::now_unix_secs(),
                        state: TaskState::Active,
                    });
                    let tab_name = task_meta.as_ref().map(|m| m.name.clone());
                    self.tabs.push(Tab {
                        layout: Node::Leaf(pane.clone()),
                        focused: pane.clone(),
                        name: tab_name,
                        task: task_meta,
                    });
                }
                send_when_shell_ready(pane.clone(), cli, cx);
                cx.notify();

                if let Some(p) = prompt.filter(|s| !s.trim().is_empty()) {
                    send_prompt_when_agent_ready(pane, new_id, p, true, reply, cx);
                } else {
                    let _ = reply.send(Ok(json!({
                        "pane_id": new_id,
                        "sent": false,
                    })));
                }
            }
            PaneCmd::AgentPrompt {
                pane_id,
                prompt,
                reply,
            } => {
                let Some((_, pane)) = self.pane_by_id(&pane_id, cx) else {
                    let _ = reply.send(Err("pane not found".into()));
                    return;
                };
                send_prompt_when_agent_ready(pane, pane_id, prompt, false, reply, cx);
            }
            PaneCmd::Wait {
                pane_id,
                until,
                timeout,
                reply,
            } => {
                let Some((_, pane)) = self.pane_by_id(&pane_id, cx) else {
                    let _ = reply.send(Err("pane not found".into()));
                    return;
                };
                if let Some(s) = pane
                    .read(cx)
                    .agent
                    .map(|(_, s)| s)
                    .filter(|s| until.contains(s))
                {
                    let _ = reply.send(Ok(json!({
                        "pane_id": pane_id,
                        "state": state_word(s),
                    })));
                    return;
                }
                self.pane_waiters.push(PaneWaiter {
                    pane_id,
                    until,
                    deadline: Instant::now() + timeout,
                    reply,
                });
            }
            PaneCmd::ReportAgent {
                pane_id,
                agent: agent_id,
                event: _,
                session_id,
                transcript_path,
                state,
            } => {
                if let Some((_, pane)) = self.pane_by_id(&pane_id, cx) {
                    pane.update(cx, |v, cx| {
                        v.apply_hook_report(&agent_id, session_id, transcript_path, state, cx);
                    });
                    self.check_pane_waiters(cx);
                    cx.notify();
                }
            }
        }
    }
}
