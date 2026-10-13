//! Built-in AI chat panel: streamed multi-turn conversation; each user turn can carry the
//! focused terminal's context (scope/redaction per AI settings). "Explain failure" starts here too.
use gpui::{div, prelude::*, px, Context, Entity, FontWeight, SharedString, WeakEntity, Window};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::switch::Switch;
use gpui_component::Sizable;
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::policy::Effect;

use crate::config;
use crate::termview::{StepOutput, TermView};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum ChatEvent { Close, Insert(String) }
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
        let mut m = String::from("Results of the commands from your previous reply (output redacted by AgentShell):\n");
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

/// Every command line in every fenced code block of a reply (comments and blanks skipped).
pub fn code_commands(s: &str) -> Vec<String> {
    let mut out = vec![]; let mut in_code = false;
    for line in s.lines() {
        if line.trim_start().starts_with("```") { in_code = !in_code; continue; }
        let l = line.trim().trim_start_matches("$ ").trim();
        if in_code && !l.is_empty() && !l.starts_with('#') { out.push(l.to_string()); }
    }
    out
}

/// A request held back until the user acknowledges the first-send notice.
#[derive(Clone)]
enum Pending { Ask(String), Explain(String, i32, String, String) }

pub struct ChatView {
    pub msgs: Vec<Msg>,
    pending: Option<Pending>,
    input: Entity<InputState>,
    pub source: Option<WeakEntity<TermView>>,
    with_context: bool,
    busy: bool,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _subs: Vec<gpui::Subscription>,
}

impl ChatView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(t("chat.placeholder")));
        let subs = vec![cx.subscribe_in(&input, window, |v, _, ev: &InputEvent, w, cx| if matches!(ev, InputEvent::PressEnter { .. }) { v.send(w, cx) })];
        Self { msgs: vec![], pending: None, input, source: None, with_context: true, busy: false, stop: Default::default(), _subs: subs }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) { self.input.update(cx, |s, cx| s.focus(window, cx)); }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let q = self.input.read(cx).value().trim().to_string();
        if q.is_empty() || self.busy { return; }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        self.send_text(q, cx);
    }
    fn send_text(&mut self, q: String, cx: &mut Context<Self>) {
        let cfg = config::get();
        if cfg.ai.enabled && !cfg.ai.first_send_ack { self.pending = Some(Pending::Ask(q)); cx.notify(); return; }
        let (ctx, n_red, host) = match (self.with_context, self.source.as_ref().and_then(|w| w.upgrade())) {
            (true, Some(tv)) => tv.read(cx).ai_context(),
            _ => (String::new(), 0, String::new()),
        };
        let (q_sent, n2) = if cfg.ai.redact { ssh_term_proto::ai::redact(&q) } else { (q.clone(), 0) };
        let content = if ctx.is_empty() { q_sent.clone() } else { format!("Terminal context from {host} (redacted):\n```\n{ctx}\n```\n\n{q_sent}") };
        let note = (!ctx.is_empty()).then(|| tf("chat.ctx_note", &[("host", &host), ("n", &ctx.chars().count()), ("r", &(n_red + n2))]));
        self.msgs.push(Msg { role: "user", text: q, note, plan: None });
        self.run(ssh_term_proto::ai::CHAT_SYSTEM_PROMPT, content, cx);
    }
    /// Explain a failed command (from the terminal's "Explain failure" button).
    pub fn explain(&mut self, cmd: String, code: i32, out: String, host: String, cx: &mut Context<Self>) {
        if self.busy { return; }
        let cfg = config::get();
        if cfg.ai.enabled && !cfg.ai.first_send_ack { self.pending = Some(Pending::Explain(cmd, code, out, host)); cx.notify(); return; }
        self.msgs.push(Msg { role: "user", text: tf("chat.explain_q", &[("cmd", &cmd), ("code", &code)]), note: Some(tf("chat.ctx_note", &[("host", &host), ("n", &out.chars().count()), ("r", &"✓")])), plan: None });
        let prompt = ssh_term_proto::ai::build_explain_prompt(&cmd, code, &host, &out);
        self.run(ssh_term_proto::ai::EXPLAIN_SYSTEM_PROMPT, prompt, cx);
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
        self.run(ssh_term_proto::ai::CHAT_SYSTEM_PROMPT, report, cx);
    }

    fn run(&mut self, system: &'static str, last_content: String, cx: &mut Context<Self>) {
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
        self.busy = true;
        self.stop.store(false, std::sync::atomic::Ordering::Relaxed);
        let stop = self.stop.clone();
        let (tx, rx) = flume::unbounded::<Result<String, String>>();
        std::thread::spawn(move || {
            let r = ssh_term_proto::ai::chat_stream(&provider, &key, system, &hist, |d| { let _ = tx.send(Ok(d.to_string())); !stop.load(std::sync::atomic::Ordering::Relaxed) });
            if let Err(e) = r { let _ = tx.send(Err(format!("{e:#}"))); }
        });
        cx.spawn(async move |this, cx| {
            while let Ok(m) = rx.recv_async().await {
                if this.update(cx, |v, cx| {
                    if let Some(last) = v.msgs.last_mut() { match m { Ok(d) => last.text.push_str(&d), Err(e) => { last.text = tf("chat.error", &[("e", &e)]); } } }
                    cx.notify();
                }).is_err() { return; }
            }
            let _ = this.update(cx, |v, cx| { v.busy = false; let _ = crate::audit_log().append("ai.chat.response", "ai", &format!("chars={}", v.msgs.last().map(|m| m.text.chars().count()).unwrap_or(0))); cx.notify(); });
        }).detach();
        cx.notify();
    }
}

impl ChatView {
    fn plan_el(&self, i: usize, m: &Msg, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let plan = m.plan.as_ref()?;
        let th = cur();
        let mono = SharedString::from(config::get().font.family.clone());
        let mut col = div().mt_2().pt_2().border_t_1().border_color(c(th.surface0)).flex().flex_col().gap_1()
            .child(div().flex().gap_2().items_center().text_size(px(11.)).text_color(c(th.overlay1)).child(tf("chat.plan_title", &[("n", &plan.steps.len())]))
                .when(plan.done(), |d| d.child(ui::tag(t("chat.plan_done"), c(th.success)))));
        for (k, st) in plan.steps.iter().enumerate() {
            let (mark, col_c, why) = match &st.state {
                StepState::Pending => ("·", c(th.overlay1), String::new()),
                StepState::Ask(r) => ("?", c(th.warning), r.clone()),
                StepState::Ran(by) => ("✓", c(th.success), if *by == "user" { t("chat.step_user").to_string() } else { t("chat.step_auto").to_string() }),
                StepState::Denied(r) => ("✗", c(th.error), r.clone()),
                StepState::Skipped => ("–", c(th.overlay0), t("chat.step_skipped").to_string()),
            };
            let mut row = div().flex().flex_col().gap(px(2.)).p_2().rounded_md().bg(c(th.crust))
                .child(div().flex().items_center().gap_2()
                    .child(div().w(px(14.)).text_color(col_c).child(mark))
                    .child(div().flex_1().font_family(mono.clone()).text_size(px(12.)).child(st.cmd.clone()))
                    .when(!st.risk.is_empty(), |d| d.child(ui::tag(st.risk.clone(), col_c))));
            if !why.is_empty() { row = row.child(div().pl(px(22.)).text_size(px(10.5)).text_color(c(th.overlay1)).child(why)); }
            if matches!(st.state, StepState::Ask(_)) {
                row = row.child(div().pl(px(22.)).pt_1().flex().gap_2()
                    .child(ui::button(("plan-allow", i * 1000 + k), t("chat.step_allow"), 1).h(px(22.)).on_click(cx.listener(move |v, _, _, cx| v.step(i, Some(true), cx))))
                    .child(ui::button(("plan-skip", i * 1000 + k), t("chat.step_skip"), 0).h(px(22.)).on_click(cx.listener(move |v, _, _, cx| v.step(i, Some(false), cx))))
                    .child(ui::button(("plan-stop", i * 1000 + k), t("chat.step_stop"), 2).h(px(22.)).on_click(cx.listener(move |v, _, _, cx| { if let Some(p) = v.msgs.get_mut(i).and_then(|m| m.plan.as_mut()) { p.cancel(); } let _ = crate::audit_log().append("ai.plan.stop", "user", ""); cx.notify(); }))));
            }
            col = col.child(row);
        }
        Some(col.into_any_element())
    }
}

/// First fenced code block of a reply (for "Insert into terminal").
pub fn first_code_block(s: &str) -> Option<String> {
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
        let mono = SharedString::from(config::get().font.family.clone());
        let mut list = div().id("chat-list").flex_1().min_h_0().overflow_y_scroll().p_4().flex().flex_col().gap_3();
        if self.msgs.is_empty() { list = list.child(div().text_size(px(12.5)).text_color(c(th.overlay1)).child(t("chat.empty"))); }
        let n = self.msgs.len();
        for (i, m) in self.msgs.iter().enumerate() {
            let user = m.role == "user";
            let mut body = div().flex().flex_col().gap_1();
            // render fenced code blocks in mono
            let mut in_code = false;
            for line in m.text.lines() {
                if line.trim_start().starts_with("```") { in_code = !in_code; continue; }
                body = body.child(if in_code { div().px_2().py(px(1.)).bg(c(th.crust)).rounded_sm().font_family(mono.clone()).text_size(px(12.)).child(line.to_string()) } else { div().child(line.to_string()) });
            }
            if m.text.is_empty() { body = body.child(div().text_color(c(th.overlay1)).child("…")); }
            let code = (!user).then(|| first_code_block(&m.text)).flatten();
            let streaming = self.busy && i + 1 == n;
            let plan_el = (!user && !streaming).then(|| self.plan_el(i, m, cx)).flatten();
            list = list.child(div().flex().when(user, |d| d.justify_end()).child(
                div().max_w(px(640.)).p_3().rounded_lg().text_size(px(12.5)).text_color(c(th.text))
                    .bg(if user { c(th.accent).opacity(0.14) } else { c(th.mantle) }).border_1().border_color(c(th.surface0))
                    .child(body)
                    .when_some(m.note.clone(), |d, note| d.child(div().pt_1().text_size(px(10.5)).text_color(c(th.overlay0)).child(note)))
                    .when_some(code.filter(|_| !(self.busy && i + 1 == n)), |d, code| d.child(div().pt_2().flex().gap_2()
                        .child(ui::button(("chat-ins", i), t("chat.insert"), 0).h(px(22.)).on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::Insert(code.clone())))))
                        .when(m.plan.is_none(), |d| d.child(ui::button(("chat-plan", i), t("chat.run_steps"), 1).h(px(22.)).on_click(cx.listener(move |v, _, _, cx| v.start_plan(i, cx)))))))
                    .when_some(plan_el, |d, p| d.child(p))));
        }
        let src = self.source.as_ref().and_then(|w| w.upgrade()).map(|v| v.read(cx).title.to_string()).unwrap_or_else(|| t("chat.no_terminal").into());
        let consent = self.pending.is_some().then(|| {
            let text = self.source.as_ref().and_then(|w| w.upgrade()).map(|v| v.read(cx).ai_consent_text()).unwrap_or_default();
            div().mx_3().mb_2().p_3().rounded_lg().border_1().border_color(c(th.warning)).bg(c(th.warning).opacity(0.08)).flex().flex_col().gap_2().text_size(px(12.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ai.consent_title")))
                .child(div().text_color(c(th.subtext)).child(text))
                .child(div().flex().gap_2().justify_end()
                    .child(ui::button("chat-consent-no", t("ui.cancel"), 0).on_click(cx.listener(|v, _, _, cx| { v.pending = None; cx.notify() })))
                    .child(ui::button("chat-consent-ok", t("ai.consent_ok"), 1).on_click(cx.listener(|v, _, _, cx| {
                        crate::ai_consent_ack();
                        match v.pending.take() { Some(Pending::Ask(q)) => v.send_text(q, cx), Some(Pending::Explain(a, b, c2, d)) => v.explain(a, b, c2, d, cx), None => {} }
                    }))))
        });
        let bar = div().p_3().border_t_1().border_color(c(th.surface0)).flex().flex_col().gap_2()
            .child(div().flex().items_center().gap_3().text_size(px(11.5)).text_color(c(th.overlay1))
                .child(Switch::new("chat-ctx").small().checked(self.with_context).label(tf("chat.with_ctx", &[("t", &src), ("scope", &config::get().ai.context)])).on_click(cx.listener(|v, on: &bool, _, cx| { v.with_context = *on; cx.notify() })))
                .child(div().flex_1())
                .child(if config::get().ai.redact { t("chat.redact_on") } else { t("chat.redact_off") }))
            .child(div().flex().gap_2().child(div().flex_1().child(Input::new(&self.input)))
                .child(if self.busy { ui::button("chat-stop", t("chat.stop"), 2).on_click(cx.listener(|v, _, _, cx| { v.stop.store(true, std::sync::atomic::Ordering::Relaxed); cx.notify() })) }
                    else { ui::button("chat-send", t("chat.send"), 1).on_click(cx.listener(|v, _, w, cx| v.send(w, cx))) })
                .child(ui::button("chat-clear", t("chat.clear"), 3).on_click(cx.listener(|v, _, _, cx| { if !v.busy { v.msgs.clear(); } cx.notify() }))));
        ui::card(900.).h(px(680.))
            .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(IconName::Sparkles, 16., c(th.accent)))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.built_in_ai_assistant")))
                .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child(t("chat.subtitle")))
                .child(div().flex_1())
                .child(div().id("chat-close").size(px(28.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                    .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(|_, _, _, cx| cx.emit(ChatEvent::Close)))
                    .child(icon(IconName::X, 16., c(th.overlay1)))))
            .child(list).children(consent).child(bar)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn code_block() {
        assert_eq!(super::first_code_block("x\n```bash\nls -la\n```\n"), Some("ls -la".into()));
        assert_eq!(super::first_code_block("none"), None);
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
