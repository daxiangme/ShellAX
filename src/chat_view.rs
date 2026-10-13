//! Built-in AI chat panel: streamed multi-turn conversation; each user turn can carry the
//! focused terminal's context (scope/redaction per AI settings). "Explain failure" starts here too.
use gpui::{div, prelude::*, px, Context, Entity, FontWeight, SharedString, WeakEntity, Window};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::switch::Switch;
use gpui_component::Sizable;
use ssh_term_proto::i18n::{t, tf};

use crate::config;
use crate::termview::TermView;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum ChatEvent { Close, Insert(String) }
impl gpui::EventEmitter<ChatEvent> for ChatView {}

#[derive(Clone)]
pub struct Msg { pub role: &'static str, pub text: String, pub note: Option<String> }

pub struct ChatView {
    pub msgs: Vec<Msg>,
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
        Self { msgs: vec![], input, source: None, with_context: true, busy: false, stop: Default::default(), _subs: subs }
    }
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) { self.input.update(cx, |s, cx| s.focus(window, cx)); }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let q = self.input.read(cx).value().trim().to_string();
        if q.is_empty() || self.busy { return; }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        let cfg = config::get();
        let (ctx, n_red, host) = match (self.with_context, self.source.as_ref().and_then(|w| w.upgrade())) {
            (true, Some(tv)) => tv.read(cx).ai_context(),
            _ => (String::new(), 0, String::new()),
        };
        let (q_sent, n2) = if cfg.ai.redact { ssh_term_proto::ai::redact(&q) } else { (q.clone(), 0) };
        let content = if ctx.is_empty() { q_sent.clone() } else { format!("Terminal context from {host} (redacted):\n```\n{ctx}\n```\n\n{q_sent}") };
        let note = (!ctx.is_empty()).then(|| tf("chat.ctx_note", &[("host", &host), ("n", &ctx.chars().count()), ("r", &(n_red + n2))]));
        self.msgs.push(Msg { role: "user", text: q, note });
        self.run(ssh_term_proto::ai::CHAT_SYSTEM_PROMPT, content, cx);
    }
    /// Explain a failed command (from the terminal's "Explain failure" button).
    pub fn explain(&mut self, cmd: String, code: i32, out: String, host: String, cx: &mut Context<Self>) {
        if self.busy { return; }
        self.msgs.push(Msg { role: "user", text: tf("chat.explain_q", &[("cmd", &cmd), ("code", &code)]), note: Some(tf("chat.ctx_note", &[("host", &host), ("n", &out.chars().count()), ("r", &"✓")])) });
        let prompt = ssh_term_proto::ai::build_explain_prompt(&cmd, code, &host, &out);
        self.run(ssh_term_proto::ai::EXPLAIN_SYSTEM_PROMPT, prompt, cx);
    }

    fn run(&mut self, system: &'static str, last_content: String, cx: &mut Context<Self>) {
        let cfg = config::get();
        let Some(provider) = cfg.ai.default_provider().cloned().filter(|_| cfg.ai.enabled) else {
            self.msgs.push(Msg { role: "assistant", text: t(if cfg.ai.enabled { "ai.no_provider" } else { "ai.disabled" }).into(), note: None });
            cx.notify(); return;
        };
        let key = crate::secret_store().get(&provider.secret_name()).map(|x| x.0).unwrap_or_default();
        // history: previous turns as plain text (only the latest user turn carries the context)
        let mut hist: Vec<(String, String)> = self.msgs[..self.msgs.len() - 1].iter().filter(|m| !m.text.is_empty()).map(|m| (m.role.to_string(), m.text.clone())).collect();
        hist.push(("user".into(), last_content.clone()));
        let _ = crate::audit_log().append("ai.chat", "user", &format!("provider={} model={} turns={} chars={}", provider.name, provider.model, hist.len(), last_content.chars().count()));
        self.msgs.push(Msg { role: "assistant", text: String::new(), note: Some(format!("{} · {}", provider.name, provider.model)) });
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
            list = list.child(div().flex().when(user, |d| d.justify_end()).child(
                div().max_w(px(640.)).p_3().rounded_lg().text_size(px(12.5)).text_color(c(th.text))
                    .bg(if user { c(th.accent).opacity(0.14) } else { c(th.mantle) }).border_1().border_color(c(th.surface0))
                    .child(body)
                    .when_some(m.note.clone(), |d, note| d.child(div().pt_1().text_size(px(10.5)).text_color(c(th.overlay0)).child(note)))
                    .when_some(code.filter(|_| !(self.busy && i + 1 == n)), |d, code| d.child(div().pt_2().flex().gap_2()
                        .child(ui::button(("chat-ins", i), t("chat.insert"), 0).h(px(22.)).on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatEvent::Insert(code.clone())))))))));
        }
        let src = self.source.as_ref().and_then(|w| w.upgrade()).map(|v| v.read(cx).title.to_string()).unwrap_or_else(|| t("chat.no_terminal").into());
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
            .child(list).child(bar)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn code_block() {
        assert_eq!(super::first_code_block("x\n```bash\nls -la\n```\n"), Some("ls -la".into()));
        assert_eq!(super::first_code_block("none"), None);
    }
}
