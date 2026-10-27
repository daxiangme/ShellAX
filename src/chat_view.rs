//! Built-in AI chat panel: streamed multi-turn conversation; each user turn can carry the
//! focused terminal's context (scope/redaction per AI settings). "Explain failure" starts here too.
use gpui::{div, prelude::*, px, relative, Animation, AnimationExt, Context, Entity, FontWeight, ScrollHandle, SharedString, WeakEntity, Window};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use gpui_component::text::{SelectionFormat, TextView};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::policy::Effect;

use crate::config;
use crate::termview::{StepOutput, TermView};
use crate::theme::{c, cur, divider, selected_bg, sub_bg};
use crate::ui::{self, icon, IconName};

pub enum ChatEvent { Close, Insert(String) }

static DOCK_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_dock_open(open: bool) { DOCK_OPEN.store(open, std::sync::atomic::Ordering::Relaxed); }
pub fn dock_open() -> bool { DOCK_OPEN.load(std::sync::atomic::Ordering::Relaxed) }
impl gpui::EventEmitter<ChatEvent> for ChatView {}

#[derive(Clone)]
pub struct Msg { pub role: &'static str, pub text: String, pub note: Option<String>, pub plan: Option<Plan> }

/// US-08: commands proposed in a reply, executed one by one. Every step is evaluated by the
/// permission engine when it becomes current; "ask" steps wait for the user's per-step approval.
#[derive(Clone, Debug, PartialEq)]
pub enum StepState { Pending, Ask(String), Ran(&'static str), Denied(String), Skipped }
#[derive(Clone, Debug, PartialEq)]
pub struct Step { pub cmd: String, pub risk: String, pub state: StepState, pub result: Option<StepOutput> }
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan { pub steps: Vec<Step>, pub running: bool, pub reported: bool }

impl Plan {
    pub fn new(cmds: Vec<String>) -> Self { Self { steps: cmds.into_iter().map(|cmd| Step { cmd, risk: String::new(), state: StepState::Pending, result: None }).collect(), running: false, reported: false } }
    pub fn current(&self) -> Option<usize> { self.steps.iter().position(|s| matches!(s.state, StepState::Pending | StepState::Ask(_))) }
    pub fn done(&self) -> bool { self.current().is_none() }
    /// Evaluate pending steps in order; returns the command to run now (auto-allowed), stopping
    /// at the first step that needs the user's approval. One step runs at a time: nothing is
    /// evaluated while a step is still running (its output may change what should happen next).
    pub fn advance(&mut self, decide: impl Fn(&str) -> (Effect, String, String)) -> Vec<String> {
        let mut run = vec![];
        if self.running { return run; }
        while let Some(i) = self.current() {
            let st = &mut self.steps[i];
            if matches!(st.state, StepState::Ask(_)) { break; }
            let (eff, risk, reason) = decide(&st.cmd);
            st.risk = risk;
            match eff {
                Effect::Allow => { st.state = StepState::Ran("policy"); run.push(st.cmd.clone()); self.running = true; break; }
                Effect::Ask => { st.state = StepState::Ask(reason); break; }
                Effect::Deny => { st.state = StepState::Denied(reason); }
            }
        }
        run
    }
    /// User answer for the current "ask" step. Returns the command when approved.
    pub fn answer(&mut self, allow: bool) -> Option<String> {
        let i = self.current()?;
        let st = &mut self.steps[i];
        if !matches!(st.state, StepState::Ask(_)) { return None; }
        if allow { st.state = StepState::Ran("user"); self.running = true; Some(st.cmd.clone()) } else { st.state = StepState::Skipped; None }
    }
    /// Record the result of the running step.
    pub fn finish(&mut self, out: StepOutput) {
        self.running = false;
        if let Some(st) = self.steps.iter_mut().rev().find(|s| matches!(s.state, StepState::Ran(_)) && s.result.is_none()) { st.result = Some(out); }
    }
    /// Message reporting executed steps back to the model (once, when the plan is done).
    pub fn report(&mut self) -> Option<String> {
        if !self.done() || self.running || self.reported || self.steps.iter().all(|s| s.result.is_none() && !matches!(s.state, StepState::Denied(_) | StepState::Skipped)) { return None; }
        self.reported = true;
        let mut m = String::from("Results of the commands from your previous reply (output redacted by shellax):\n");
        for s in &self.steps {
            match (&s.state, &s.result) {
                (_, Some(r)) => {
                    m.push_str(&format!("\n$ {}\nexit: {}\n", r.cmd, r.exit.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())));
                    if r.withheld { m.push_str("output: [withheld — output from this host is not shared with AI]\n"); }
                    else { m.push_str(&format!("```\n{}\n```\n", r.output)); }
                }
                (StepState::Denied(why), _) => m.push_str(&format!("\n$ {}\nnot run: denied by policy ({why})\n", crate_redact(&s.cmd))),
                (StepState::Skipped, _) => m.push_str(&format!("\n$ {}\nnot run: skipped by the user\n", crate_redact(&s.cmd))),
                _ => {}
            }
        }
        Some(m)
    }
    /// Stop the rest of the plan.
    pub fn cancel(&mut self) { for s in &mut self.steps { if matches!(s.state, StepState::Pending | StepState::Ask(_)) { s.state = StepState::Skipped; } } }
}

fn crate_redact(s: &str) -> String { ssh_term_proto::ai::redact(s).0 }

fn command_line(line: &str) -> Option<String> {
    let l = line.trim().trim_start_matches("$ ").trim();
    if l.is_empty() || l.starts_with('#') { None } else { Some(l.to_string()) }
}

/// Every command line in every fenced code block of a reply (comments and blanks skipped).
pub fn code_commands(s: &str) -> Vec<String> {
    let mut out = vec![]; let mut in_code = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") { in_code = !in_code; continue; }
        if in_code { if let Some(l) = command_line(line) { out.push(l); } }
    }
    out
}

/// One command per fence, so each command can carry its own insert control.
/// While `streaming`, an unclosed fence stays plain text until its closing marker arrives.
pub fn split_command_fences(src: &str, streaming: bool) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut buf = String::new();
    let mut lang = String::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_code {
                let cmds: Vec<String> = buf.lines().filter_map(command_line).collect();
                if cmds.is_empty() {
                    out.push_str("```");
                    out.push_str(&lang);
                    out.push('\n');
                    out.push_str(&buf);
                    if !buf.ends_with('\n') && !buf.is_empty() { out.push('\n'); }
                    out.push_str("```\n");
                } else {
                    for c in cmds {
                        out.push_str("```sh\n");
                        out.push_str(&c);
                        out.push_str("\n```\n");
                    }
                }
                buf.clear();
                lang.clear();
                in_code = false;
            } else {
                in_code = true;
                lang = trimmed.trim_start_matches('`').trim().to_string();
            }
            continue;
        }
        if in_code { buf.push_str(line); buf.push('\n'); }
        else { out.push_str(line); out.push('\n'); }
    }
    if in_code {
        if streaming {
            out.push_str(&buf);
        } else {
            out.push_str("```");
            out.push_str(&lang);
            out.push('\n');
            out.push_str(&buf);
        }
    }
    out
}

/// A request held back until the user acknowledges the first-send notice.
#[derive(Clone)]
enum Pending { Ask(String), Explain(String, i32, String, String) }

pub struct ChatView {
    pub msgs: Vec<Msg>,
    pending: Option<Pending>,
    input: Entity<TextareaState>,
    pub source: Option<WeakEntity<TermView>>,
    with_context: bool,
    /// Module the assistant talks about (terminal, database, Redis, containers).
    pub target: crate::ai_ctx::Target,
    /// Fills the right-hand dock instead of a fixed-size dialog.
    pub docked: bool,
    busy: bool,
    /// Bumped on each request so a finished stream cannot clear a newer one.
    turn: u64,
    scroll: ScrollHandle,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _subs: Vec<gpui::Subscription>,
}

impl ChatView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx).placeholder(t("chat.placeholder")).auto_grow(1, 6).submit_on_enter(true));
        let subs = vec![cx.subscribe_in(&input, window, |v, _, ev: &InputEvent, w, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => v.send(w, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        })];
        Self { msgs: vec![], pending: None, input, source: None, with_context: true, target: Default::default(), docked: false, busy: false, turn: 0, scroll: ScrollHandle::new(), stop: Default::default(), _subs: subs }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) { self.input.update(cx, |s, cx| s.focus(window, cx)); }
    /// Attach later questions to this terminal. Turns context on if it was off.
    pub fn use_terminal(&mut self, src: WeakEntity<TermView>) { self.source = Some(src); self.with_context = true; }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let q = self.input.read(cx).value().trim().to_string();
        if q.is_empty() || self.busy { return; }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.send_text(q, cx);
    }
    /// Follow the active module; a different target starts from its own context.
    pub fn set_target(&mut self, t: crate::ai_ctx::Target, cx: &mut Context<Self>) {
        if !self.target.same(&t) { self.target = t; cx.notify(); }
    }
    /// Ask about the current non-terminal module; `deep` adds the analysis probes.
    pub fn ask_module(&mut self, q: String, deep: bool, cx: &mut Context<Self>) {
        let cfg = config::get();
        let Some(job) = self.with_context.then(|| self.target.gather(deep, cx)).flatten() else {
            let content = if cfg.ai.redact { ssh_term_proto::ai::redact(&q).0 } else { q.clone() };
            self.msgs.push(Msg { role: "user", text: q, note: None, plan: None });
            self.run(crate::ai_ctx::full_prompt(self.target.system_prompt()), content, cx);
            return;
        };
        let label = self.target.label(cx).unwrap_or_default();
        let system = crate::ai_ctx::full_prompt(self.target.system_prompt());
        self.msgs.push(Msg { role: "user", text: q.clone(), note: Some(tf("aictx.collecting", &[("t", &label)])), plan: None });
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let ctx = job.await.unwrap_or_default();
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                let cfg = config::get();
                let (ctx, n1) = if cfg.ai.redact { ssh_term_proto::ai::redact(&ctx) } else { (ctx, 0) };
                let (q_sent, n2) = if cfg.ai.redact { ssh_term_proto::ai::redact(&q) } else { (q.clone(), 0) };
                if let Some(m) = v.msgs.last_mut() { m.note = Some(tf("chat.ctx_note", &[("host", &label), ("n", &ctx.chars().count()), ("r", &(n1 + n2))])); }
                let content = format!("Context of {label} (redacted):\n```\n{ctx}\n```\n\n{q_sent}");
                v.run(system, content, cx);
            });
        }).detach();
    }
    fn send_text(&mut self, q: String, cx: &mut Context<Self>) {
        self.send_deep(q, false, cx)
    }
    pub fn send_deep(&mut self, q: String, deep: bool, cx: &mut Context<Self>) {
        let cfg = config::get();
        if cfg.ai.enabled && !cfg.ai.first_send_ack { self.pending = Some(Pending::Ask(q)); cx.notify(); return; }
        if !self.target.is_terminal() { return self.ask_module(q, deep, cx); }
        let (ctx, n_red, host) = match (self.with_context, self.source.as_ref().and_then(|w| w.upgrade())) {
            (true, Some(tv)) => { let (t, n, h, _) = tv.read(cx).chat_context(); (t, n, h) }
            _ => (String::new(), 0, String::new()),
        };
        let (q_sent, n2) = if cfg.ai.redact { ssh_term_proto::ai::redact(&q) } else { (q.clone(), 0) };
        let content = if ctx.is_empty() { q_sent.clone() } else { format!("Terminal context from {host} (redacted):\n```\n{ctx}\n```\n\n{q_sent}") };
        let note = (!ctx.is_empty()).then(|| tf("chat.ctx_note", &[("host", &host), ("n", &ctx.chars().count()), ("r", &(n_red + n2))]));
        self.msgs.push(Msg { role: "user", text: q, note, plan: None });
        self.run(ssh_term_proto::ai::CHAT_SYSTEM_PROMPT.to_string(), content, cx);
    }
    /// Explain selected terminal text (context menu "Explain with AI"); redaction / first-send notice apply.
    pub fn explain_text(&mut self, text: String, cx: &mut Context<Self>) {
        if self.busy { return; }
        self.send_text(tf("chat.explain_selection", &[("text", &text)]), cx);
    }
    /// Explain a failed command (from the terminal's "Explain failure" button).
    pub fn explain(&mut self, cmd: String, code: i32, out: String, host: String, cx: &mut Context<Self>) {
        if self.busy { return; }
        let cfg = config::get();
        if cfg.ai.enabled && !cfg.ai.first_send_ack { self.pending = Some(Pending::Explain(cmd, code, out, host)); cx.notify(); return; }
        self.msgs.push(Msg { role: "user", text: tf("chat.explain_q", &[("cmd", &cmd), ("code", &code)]), note: Some(tf("chat.ctx_note", &[("host", &host), ("n", &out.chars().count()), ("r", &"✓")])), plan: None });
        let prompt = ssh_term_proto::ai::build_explain_prompt(&cmd, code, &host, &out);
        self.run(ssh_term_proto::ai::EXPLAIN_SYSTEM_PROMPT.to_string(), prompt, cx);
    }

    /// Start step-by-step execution of the commands in message `i` (in the source terminal).
    pub fn start_plan(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(m) = self.msgs.get_mut(i) else { return };
        let cmds = code_commands(&m.text);
        if cmds.is_empty() { return; }
        let _ = crate::audit_log().append("ai.plan", "user", &format!("steps={}", cmds.len()));
        m.plan = Some(Plan::new(cmds));
        self.step(i, None, cx);
    }
    /// Advance plan `i`; `answer` is the user's per-step approval for the current ask step.
    pub fn step(&mut self, i: usize, answer: Option<bool>, cx: &mut Context<Self>) {
        let Some(tv) = self.source.as_ref().and_then(|w| w.upgrade()) else {
            if let Some(p) = self.msgs.get_mut(i).and_then(|m| m.plan.as_mut()) { p.cancel(); }
            cx.notify(); return;
        };
        let Some(plan) = self.msgs.get_mut(i).and_then(|m| m.plan.as_mut()) else { return };
        let mut run: Vec<(String, &'static str)> = vec![];
        if let Some(a) = answer {
            let cur = plan.current().map(|k| plan.steps[k].cmd.clone()).unwrap_or_default();
            match plan.answer(a) { Some(c) => run.push((c, "user")), None => { let _ = crate::audit_log().append("ai.exec.rejected", "user", &format!("host={} command={cur:?}", tv.read(cx).audit_host())); } }
        }
        let tvr = tv.read(cx);
        let host = tvr.audit_host();
        let auto = plan.advance(|c| { let (e, r, why) = tvr.ai_policy(c); let _ = crate::audit_log().append("ai.exec.decision", "policy", &format!("plan=1 host={host} risk={} effect={e:?} command={c:?} reason={why:?}", r.label())); (e, r.label().to_string(), why) });
        run.extend(auto.into_iter().map(|c| (c, "policy")));
        let report = plan.report();
        for (c, by) in run {
            let (tx, rx) = flume::bounded::<StepOutput>(1);
            tv.update(cx, |t, cx| t.ai_run_capture(c.clone(), by, tx, cx));
            cx.spawn(async move |this, cx| {
                // no shell integration (no OSC 133;D) → give up waiting after 120 s
                let (wtx, wrx) = flume::bounded(1);
                std::thread::spawn(move || { let _ = wtx.send(rx.recv_timeout(std::time::Duration::from_secs(120)).ok()); });
                let out = wrx.recv_async().await.ok().flatten();
                let out = out.unwrap_or(StepOutput { cmd: crate_redact(&c), exit: None, output: "[no completion signal from the shell within 120 s]".into(), withheld: false });
                let _ = this.update(cx, |v, cx| {
                    let _ = crate::audit_log().append("ai.exec.result", "policy", &format!("exit={:?} chars={} withheld={} command={:?}", out.exit, out.output.chars().count(), out.withheld, out.cmd));
                    if let Some(p) = v.msgs.get_mut(i).and_then(|m| m.plan.as_mut()) { p.finish(out); }
                    v.step(i, None, cx);
                });
            }).detach();
        }
        if let Some(r) = report { self.feed_back(r, cx); }
        cx.notify();
    }
    /// Send executed-step results back to the model as the next user turn.
    fn feed_back(&mut self, report: String, cx: &mut Context<Self>) {
        if self.busy || !config::get().ai.feed_back_results { return; }
        let _ = crate::audit_log().append("ai.exec.feedback", "user", &format!("chars={}", report.chars().count()));
        self.msgs.push(Msg { role: "user", text: report.clone(), note: Some(t("chat.feedback_note").to_string()), plan: None });
        self.run(ssh_term_proto::ai::CHAT_SYSTEM_PROMPT.to_string(), report, cx);
    }

    fn run(&mut self, system: String, last_content: String, cx: &mut Context<Self>) {
        let cfg = config::get();
        let Some(provider) = cfg.ai.default_provider().cloned().filter(|_| cfg.ai.enabled) else {
            self.msgs.push(Msg { role: "assistant", text: t(if cfg.ai.enabled { "ai.no_provider" } else { "ai.disabled" }).into(), note: None, plan: None });
            cx.notify(); return;
        };
        let key = crate::secret_store().get(&provider.secret_name()).map(|x| x.0).unwrap_or_default();
        // history: previous turns as plain text (only the latest user turn carries the context)
        let mut hist: Vec<(String, String)> = self.msgs[..self.msgs.len() - 1].iter().filter(|m| !m.text.is_empty()).map(|m| (m.role.to_string(), m.text.clone())).collect();
        hist.push(("user".into(), last_content.clone()));
        let _ = crate::audit_log().append("ai.chat", "user", &format!("provider={} model={} turns={} chars={}", provider.name, provider.model, hist.len(), last_content.chars().count()));
        self.msgs.push(Msg { role: "assistant", text: String::new(), note: Some(format!("{} · {}", provider.name, provider.model)), plan: None });
        self.turn += 1;
        let turn = self.turn;
        self.busy = true;
        self.stop.store(false, std::sync::atomic::Ordering::Relaxed);
        self.scroll.scroll_to_bottom();
        let stop = self.stop.clone();
        let (tx, rx) = flume::unbounded::<Result<String, String>>();
        std::thread::spawn(move || {
            let r = ssh_term_proto::ai::chat_stream(&provider, &key, &system, &hist, |d| { let _ = tx.send(Ok(d.to_string())); !stop.load(std::sync::atomic::Ordering::Relaxed) });
            if let Err(e) = r { let _ = tx.send(Err(format!("{e:#}"))); }
        });
        cx.spawn(async move |this, cx| {
            let mut buf = String::new();
            let mut failed = false;
            loop {
                if buf.is_empty() {
                    match rx.recv_async().await {
                        Ok(Ok(d)) => buf.push_str(&d),
                        Ok(Err(e)) => {
                            let _ = this.update(cx, |v, cx| {
                                if v.turn == turn { if let Some(last) = v.msgs.last_mut() { last.text = tf("chat.error", &[("e", &e)]); } v.busy = false; }
                                cx.notify();
                            });
                            return;
                        }
                        Err(_) => break,
                    }
                }
                loop {
                    match rx.try_recv() {
                        Ok(Ok(d)) => buf.push_str(&d),
                        Ok(Err(e)) => {
                            let _ = this.update(cx, |v, cx| {
                                if v.turn == turn { if let Some(last) = v.msgs.last_mut() { last.text = tf("chat.error", &[("e", &e)]); } v.busy = false; }
                                cx.notify();
                            });
                            return;
                        }
                        Err(flume::TryRecvError::Empty) => break,
                        Err(flume::TryRecvError::Disconnected) => { failed = true; break; }
                    }
                }
                cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
                loop {
                    match rx.try_recv() {
                        Ok(Ok(d)) => buf.push_str(&d),
                        Ok(Err(e)) => {
                            buf.clear();
                            let _ = this.update(cx, |v, cx| {
                                if v.turn == turn { if let Some(last) = v.msgs.last_mut() { last.text = tf("chat.error", &[("e", &e)]); } v.busy = false; }
                                cx.notify();
                            });
                            return;
                        }
                        Err(flume::TryRecvError::Empty) => break,
                        Err(flume::TryRecvError::Disconnected) => { failed = true; break; }
                    }
                }
                if !buf.is_empty() {
                    let chunk = std::mem::take(&mut buf);
                    if this.update(cx, |v, cx| {
                        if v.turn != turn { return; }
                        let follow = {
                            let off = v.scroll.offset().y;
                            let max = v.scroll.max_offset().y;
                            max == px(0.) || (off - max).abs() < px(80.)
                        };
                        if let Some(last) = v.msgs.last_mut() { last.text.push_str(&chunk); }
                        if follow { v.scroll.scroll_to_bottom(); }
                        cx.notify();
                    }).is_err() { return; }
                }
                if failed { break; }
            }
            let _ = this.update(cx, |v, cx| {
                if v.turn == turn {
                    v.busy = false;
                    v.scroll.scroll_to_bottom();
                    let _ = crate::audit_log().append("ai.chat.response", "ai", &format!("chars={}", v.msgs.last().map(|m| m.text.chars().count()).unwrap_or(0)));
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }
}

fn typing_dots() -> gpui::AnyElement {
    let th = cur();
    let mut row = div().flex().items_center().gap(px(5.)).h(px(18.));
    for i in 0..3usize {
        row = row.child(div().size(px(6.)).rounded_full().bg(c(th.overlay1)).with_animation(
            ("chat-dot", i),
            Animation::new(std::time::Duration::from_millis(900)).repeat().with_easing(move |t| {
                let t = (t + i as f32 * 0.18).fract();
                (t * std::f32::consts::PI).sin()
            }),
            |d, t| d.opacity(0.25 + 0.75 * t).mt(px((1.0 - t) * 3.)),
        ));
    }
    row.into_any_element()
}

fn stream_caret() -> gpui::AnyElement {
    let th = cur();
    div().w(px(2.)).h(px(14.)).rounded_full().bg(c(th.accent)).with_animation(
        "chat-caret",
        Animation::new(std::time::Duration::from_millis(800)).repeat().with_easing(|t| if t < 0.55 { 1. } else { 0.15 }),
        |d, t| d.opacity(t),
    ).into_any_element()
}

impl ChatView {
    fn plan_el(&self, i: usize, m: &Msg, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let plan = m.plan.as_ref()?;
        let th = cur();
        let mono = ui::mono_family();
        let mut col = div().mt(px(ui::SP_2)).pt(px(ui::SP_2)).border_t_1().border_color(divider()).flex().flex_col().gap(px(ui::SP_1))
            .child(div().flex().gap(px(ui::SP_2)).items_center().text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(tf("chat.plan_title", &[("n", &plan.steps.len())]))
                .when(plan.done(), |d| d.child(ui::tag(t("chat.plan_done"), c(th.success)))));
        for (k, st) in plan.steps.iter().enumerate() {
            let (mark, col_c, why) = match &st.state {
                StepState::Pending => ("·", c(th.overlay1), String::new()),
                StepState::Ask(r) => ("?", c(th.warning), r.clone()),
                StepState::Ran(by) => ("✓", c(th.success), if *by == "user" { t("chat.step_user").to_string() } else { t("chat.step_auto").to_string() }),
                StepState::Denied(r) => ("✗", c(th.error), r.clone()),
                StepState::Skipped => ("–", c(th.overlay0), t("chat.step_skipped").to_string()),
            };
            let mut row = div().flex().flex_col().gap(px(2.)).p(px(ui::SP_2)).rounded(px(ui::R_MD)).bg(sub_bg())
                .child(div().flex().items_center().gap(px(ui::SP_2))
                    .child(div().w(px(14.)).text_color(col_c).child(mark))
                    .child(div().flex_1().font_family(mono.clone()).text_size(px(ui::fs_mono())).child(st.cmd.clone()))
                    .when(!st.risk.is_empty(), |d| d.child(ui::tag(st.risk.clone(), col_c))));
            if !why.is_empty() { row = row.child(div().pl(px(22.)).text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(why)); }
            if matches!(st.state, StepState::Ask(_)) {
                row = row.child(div().pl(px(22.)).pt(px(ui::SP_1)).flex().gap(px(ui::SP_2))
                    .child(ui::button_sm(("plan-allow", i * 1000 + k), t("chat.step_allow"), 1).on_click(cx.listener(move |v, _, _, cx| v.step(i, Some(true), cx))))
                    .child(ui::button_sm(("plan-skip", i * 1000 + k), t("chat.step_skip"), 0).on_click(cx.listener(move |v, _, _, cx| v.step(i, Some(false), cx))))
                    .child(ui::button_sm(("plan-stop", i * 1000 + k), t("chat.step_stop"), 2).on_click(cx.listener(move |v, _, _, cx| { if let Some(p) = v.msgs.get_mut(i).and_then(|m| m.plan.as_mut()) { p.cancel(); } let _ = crate::audit_log().append("ai.plan.stop", "user", ""); cx.notify(); }))));
            }
            col = col.child(row);
        }
        Some(col.into_any_element())
    }
}

/// First line of the first fenced block. Kept for the parser tests.
#[cfg(test)]
fn first_code_block(s: &str) -> Option<String> {
    let a = s.find("```")?;
    let rest = &s[a + 3..];
    let rest = rest.split_once('\n').map(|x| x.1).unwrap_or(rest);
    let b = rest.find("```")?;
    let code = rest[..b].trim();
    (!code.is_empty()).then(|| code.lines().next().unwrap_or(code).to_string())
}

impl Render for ChatView {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let mut list = div().id("chat-list").track_scroll(&self.scroll).flex_1().min_h_0().overflow_y_scroll().p(px(ui::PANEL_PX)).flex().flex_col().gap(px(ui::SP_3));
        if self.msgs.is_empty() { list = list.child(div().text_size(px(ui::fs_body())).text_color(c(th.overlay1)).child(t("chat.empty"))); }
        let n = self.msgs.len();
        let view = cx.entity();
        let term = self.target.is_terminal();
        for (i, m) in self.msgs.iter().enumerate() {
            let user = m.role == "user";
            let cmds = if user || !term { Vec::new() } else { code_commands(&m.text) };
            let streaming = self.busy && i + 1 == n;
            let waiting = streaming && m.text.is_empty();
            let plan_el = (!user && !streaming).then(|| self.plan_el(i, m, cx)).flatten();
            let body = if waiting {
                typing_dots()
            } else if m.text.is_empty() {
                div().text_color(c(th.overlay1)).child("…").into_any_element()
            } else if user {
                div().whitespace_normal().child(m.text.clone()).into_any_element()
            } else {
                let shown = split_command_fences(&m.text, streaming);
                let view = view.clone();
                TextView::markdown(("chat-ai", i), shown)
                    .selectable(true)
                    .stream_fade(streaming)
                    .selection_format(SelectionFormat::Plain)
                    .code_block_actions(move |block, _, _| {
                        let cmd = if term {
                            let cmds: Vec<String> = block.code().lines().filter_map(command_line).collect();
                            (cmds.len() == 1).then(|| cmds.into_iter().next().unwrap())
                        } else { Some(block.code().trim_end().to_string()).filter(|s| !s.trim().is_empty()) };
                        let view = view.clone();
                        div().when_some(cmd, |d, cmd| d.child(
                            ui::icon_button("ins", IconName::SquareTerminal, ui::IconBtn::Sm, "")
                                .on_click(move |_, _, cx| { let cmd = cmd.clone(); let _ = view.update(cx, |_, cx| cx.emit(ChatEvent::Insert(cmd))); }),
                        )).into_any_element()
                    })
                    .into_any_element()
            };
            let one = cmds.len() == 1;
            let only = cmds.first().cloned();
            let mut bubble = div().max_w(relative(if user { 0.72 } else { 0.92 })).p(px(ui::SP_3)).rounded(px(ui::R_XL)).text_size(px(ui::fs_body())).text_color(c(th.text))
                .bg(if user { selected_bg() } else { sub_bg() }).border_1().border_color(divider());
            if !user { bubble = bubble.w_full(); }
            list = list.child(div().w_full().flex().when(user, |d| d.justify_end()).child(bubble
                    .child(body)
                    .when(streaming && !m.text.is_empty(), |d| d.child(div().pt(px(ui::SP_1)).child(stream_caret())))
                    .when(!waiting, |d| d.when_some(m.note.clone(), |d, note| d.child(div().pt(px(ui::SP_1)).text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(note))))
                    .when(one && !streaming, |d| d.child(div().pt(px(ui::SP_2)).flex().gap(px(ui::SP_2))
                        .child(ui::button_sm(("chat-ins", i), t("chat.insert"), 0).on_click(cx.listener({
                            let only = only.clone().unwrap_or_default();
                            move |_, _, _, cx| cx.emit(ChatEvent::Insert(only.clone()))
                        })))))
                    .when(cmds.len() > 1 && m.plan.is_none() && !streaming, |d| d.child(div().pt(px(ui::SP_2))
                        .child(ui::button_sm(("chat-plan", i), t("chat.run_steps"), 1).on_click(cx.listener(move |v, _, _, cx| v.start_plan(i, cx))))))
                    .when_some(plan_el, |d, p| d.child(p))));
        }
        let consent = self.pending.is_some().then(|| {
            let text = self.source.as_ref().and_then(|w| w.upgrade()).map(|v| v.read(cx).ai_consent_text()).unwrap_or_default();
            div().mx(px(ui::SP_3)).mb(px(ui::SP_2)).p(px(ui::SP_3)).rounded(px(ui::R_SUB)).border_1().border_color(c(th.warning)).bg(c(th.warning).opacity(0.08)).flex().flex_col().gap(px(ui::SP_2)).text_size(px(ui::fs_small()))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ai.consent_title")))
                .child(div().text_color(c(th.subtext)).child(text))
                .child(div().flex().gap(px(ui::SP_2)).justify_end()
                    .child(ui::button("chat-consent-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _, cx| { v.pending = None; cx.notify() })))
                    .child(ui::button("chat-consent-ok", t("ai.consent_ok"), 1).on_click(cx.listener(|v, _, _, cx| {
                        crate::ai_consent_ack();
                        match v.pending.take() { Some(Pending::Ask(q)) => v.send_text(q, cx), Some(Pending::Explain(a, b, c2, d)) => v.explain(a, b, c2, d, cx), None => {} }
                    }))))
        });
        let ctx_on = self.with_context;
        let attached = self.source.as_ref().and_then(|w| w.upgrade()).map(|v| v.read(cx).chat_context());
        let ctx_label: SharedString = if !ctx_on {
            t("chat.ctx_off").into()
        } else if !term {
            self.target.label(cx).map(|l| tf("aictx.ctx_label", &[("t", &l)])).unwrap_or_else(|| t("chat.ctx_empty").to_string()).into()
        } else {
            match attached.as_ref().map(|(_, _, host, kind)| (*kind, host.as_str())) {
                Some(("screen", host)) => tf("chat.ctx_screen", &[("t", &host)]).into(),
                Some(("selection", host)) => tf("chat.ctx_selection", &[("t", &host)]).into(),
                Some(("command", host)) => tf("chat.ctx_command", &[("t", &host)]).into(),
                _ => t("chat.ctx_empty").into(),
            }
        };
        let has_text = !self.input.read(cx).value().trim().is_empty();
        let can_send = has_text && !self.busy;
        let send_bg = if self.busy || can_send { c(th.accent) } else { crate::theme::hover_bg() };
        let send_fg = if self.busy || can_send { gpui::white() } else { c(th.overlay0) };
        let composer = div().mx(px(ui::SP_3)).mb(px(ui::SP_3)).rounded(px(ui::R_XL)).border_1().border_color(divider()).bg(sub_bg()).flex().flex_col()
            .child(div().px(px(ui::SP_3)).pt(px(ui::SP_2)).flex().items_center().gap(px(ui::SP_2)).min_w_0()
                .child(ui::chip("chat-ctx", ctx_label, ctx_on).max_w(px(240.)).overflow_hidden().text_ellipsis()
                    .on_click(cx.listener(|v, _, _, cx| { v.with_context = !v.with_context; cx.notify(); })))
                .child(div().flex_1())
                .child(div().text_size(px(ui::fs_small())).text_color(c(th.subtext)).whitespace_nowrap().child(if config::get().ai.redact { t("chat.redact_on") } else { t("chat.redact_off") })))
            .child(Textarea::new(&self.input).appearance(false).bordered(false))
            .child(div().px(px(ui::SP_2)).pb(px(ui::SP_2)).flex().items_center().gap(px(ui::SP_2))
                .when(!self.msgs.is_empty(), |d| d.child(ui::chip("chat-clear", t("chat.clear"), false)
                    .on_click(cx.listener(|v, _, _, cx| { if !v.busy { v.msgs.clear(); } cx.notify(); }))))
                .children(self.target.quick().iter().enumerate().map(|(k, (label, q, deep))| {
                    let (q, deep) = (*q, *deep);
                    ui::chip(("chat-quick", k), t(label), false)
                        .on_click(cx.listener(move |v, _, _, cx| { if !v.busy { v.send_deep(t(q).to_string(), deep, cx); } }))
                }))
                .child(div().flex_1())
                .child(div().id("chat-send").size(px(32.)).rounded_full().flex().items_center().justify_center()
                    .bg(send_bg)
                    .when(self.busy || can_send, |d| d.cursor_pointer())
                    .on_click(cx.listener(|v, _, w, cx| {
                        if v.busy { v.stop.store(true, std::sync::atomic::Ordering::Relaxed); v.busy = false; cx.notify(); }
                        else { v.send(w, cx); }
                    }))
                    .child(icon(if self.busy { IconName::Square } else { IconName::ArrowUp }, ui::IC_MD, send_fg))));
        let column = div().flex_1().min_h_0().flex().flex_col().child(list).children(consent).child(composer);
        if self.docked {
            column.size_full()
        } else {
            let me = cx.entity();
            ui::card(900.).h(px(680.))
                .child(ui::dialog_header(Some(IconName::Sparkles), t("ui.built_in_ai_assistant"), Some(t("chat.subtitle").into()),
                    Some(ui::dialog_close("chat-close", move |_, cx| { me.update(cx, |_, cx| cx.emit(ChatEvent::Close)); }))))
                .child(column)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn code_block() {
        assert_eq!(super::first_code_block("x\n```bash\nls -la\n```\n"), Some("ls -la".into()));
        assert_eq!(super::first_code_block("none"), None);
        let src = "看状态：\n```bash\ntop\nhtop\n```\n然后\n```\nuptime\n```\n";
        assert_eq!(super::code_commands(src), vec!["top", "htop", "uptime"]);
        let shown = super::split_command_fences(src, false);
        assert!(!super::split_command_fences("```sh\ntop", true).contains("```"));
        assert_eq!(shown.matches("```").count(), 6);
        assert!(shown.contains("```sh\ntop\n```") && shown.contains("```sh\nuptime\n```"));
        assert!(!super::split_command_fences("只有说明，没有命令。", false).contains("```"));
    }
    #[test]
    fn plan_steps_and_approvals() {
        use super::*;
        let cmds = code_commands("do:\n```bash\n# check\ndf -h\n$ systemctl restart nginx\n```\nthen\n```\nrm -rf /\nuptime\n```");
        assert_eq!(cmds, vec!["df -h", "systemctl restart nginx", "rm -rf /", "uptime"]);
        let decide = |c: &str| { let r = ssh_term_proto::policy::classify(c); let (e, why) = ssh_term_proto::policy::decide_ctx(ssh_term_proto::policy::Mode::Ask, ssh_term_proto::policy::Risk::R1, &[], "ai", "exec", c, true, false); (e, r.label().to_string(), why) };
        let mut p = Plan::new(cmds);
        let out = |c: &str, e: i32, o: &str| StepOutput { cmd: c.into(), exit: Some(e), output: o.into(), withheld: false };
        assert_eq!(p.advance(decide), vec!["df -h".to_string()]);
        assert_eq!(p.advance(decide), Vec::<String>::new(), "one step at a time");
        p.finish(out("df -h", 0, "/dev/sda1 50%"));
        assert_eq!(p.advance(decide), Vec::<String>::new());
        assert!(matches!(p.steps[1].state, StepState::Ask(_)));
        assert_eq!(p.advance(decide), Vec::<String>::new(), "waits for approval");
        assert_eq!(p.answer(true), Some("systemctl restart nginx".into()));
        assert!(p.report().is_none());
        p.finish(StepOutput { cmd: "systemctl restart nginx".into(), exit: Some(1), output: String::new(), withheld: true });
        // hard-denied step is skipped with its reason, then the next one asks or runs
        let next = p.advance(decide);
        assert!(matches!(p.steps[2].state, StepState::Denied(_)));
        assert_eq!(next, vec!["uptime".to_string()]);
        assert!(p.done() && p.report().is_none(), "uptime still running");
        p.finish(out("uptime", 0, "up 3 days"));
        let r = p.report().unwrap();
        assert!(r.contains("$ df -h\nexit: 0") && r.contains("/dev/sda1 50%") && r.contains("up 3 days"), "{r}");
        assert!(r.contains("exit: 1\noutput: [withheld"), "{r}");
        assert!(r.contains("rm -rf /\nnot run: denied by policy"), "{r}");
        assert!(p.report().is_none(), "reported once");
        let mut p = Plan::new(vec!["rm -r build".into(), "ls".into()]);
        p.advance(decide); assert_eq!(p.answer(false), None);
        assert_eq!(p.steps[0].state, StepState::Skipped);
        assert_eq!(p.advance(decide), vec!["ls".to_string()]);
        p.finish(out("ls", 0, "a\nb"));
        assert!(p.report().unwrap().contains("rm -r build\nnot run: skipped"));
        let mut p = Plan::new(vec!["rm -r build".into(), "ls".into()]);
        p.advance(decide); p.cancel(); assert!(p.done()); assert_eq!(p.steps[1].state, StepState::Skipped);
    }
}
