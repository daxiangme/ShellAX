//! Host tools panel: monitor / processes / services / Docker for the focused pane's host
//! (local or SSH via the pooled connection). Writes go through the permission engine.
use std::time::Duration;

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString, Window};
use ssh_term_proto::hosttools::{self, Action, Tab};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::policy::{self, Effect, Risk};
use ssh_term_proto::ssh::{self, SshTarget};

use crate::config;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum HostToolsEvent { Close }
impl gpui::EventEmitter<HostToolsEvent> for HostToolsView {}

pub struct HostToolsView {
    pub target: Option<SshTarget>,
    rt: tokio::runtime::Handle,
    prompts: ssh::Prompts,
    tab: Tab,
    out: Option<Result<String, String>>,
    busy: bool,
    /// Write waiting for confirmation: (action, command, reason).
    pending: Option<(Action, String, String)>,
    note: Option<(String, bool)>,
}

impl HostToolsView {
    pub fn new(target: Option<SshTarget>, rt: tokio::runtime::Handle, prompts: ssh::Prompts, cx: &mut Context<Self>) -> Self {
        let mut me = Self { target, rt, prompts, tab: Tab::Monitor, out: None, busy: false, pending: None, note: None };
        me.refresh(cx);
        me
    }
    fn host(&self) -> String { self.target.as_ref().map(|t| t.host.clone()).unwrap_or_else(|| "local".into()) }
    fn label(&self) -> String { self.target.as_ref().map(|t| t.label()).unwrap_or_else(|| t("hosttools.local").into()) }
    fn run(&self, cmd: String, timeout: Duration) -> flume::Receiver<Result<(Option<u32>, String), String>> {
        let (tx, rx) = flume::bounded(1);
        match self.target.clone() {
            Some(tg) => { let p = self.prompts.clone(); self.rt.spawn(async move {
                let r = async { let (c, _) = ssh::connect_shared(&tg, &p).await?; c.exec(&cmd, timeout).await }.await;
                let _ = tx.send(r.map(|(code, o)| (code, String::from_utf8_lossy(&o).into_owned())).map_err(|e| format!("{e:#}")));
            }); }
            None => { std::thread::spawn(move || {
                let r = std::process::Command::new("sh").arg("-c").arg(&cmd).output();
                let _ = tx.send(r.map(|o| (o.status.code().map(|c| c as u32), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))).map_err(|e| e.to_string()));
            }); }
        }
        rx
    }
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy { return; }
        self.busy = true;
        let rx = self.run(self.tab.read_command().to_string(), Duration::from_secs(20));
        let tab = self.tab;
        cx.spawn(async move |this, cx| {
            let r = rx.recv_async().await.unwrap_or_else(|_| Err("cancelled".into()));
            let _ = this.update(cx, |v, cx| { v.busy = false; if v.tab == tab { v.out = Some(r.map(|x| x.1)); } cx.notify(); });
        }).detach();
        cx.notify();
    }
    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) { if self.tab != tab { self.tab = tab; self.out = None; self.pending = None; self.busy = false; self.refresh(cx); } }

    /// Permission-engine verdict for a tool action on this host (user principal "hosttools").
    fn decide(&self, cmd: &str) -> (Effect, Risk, String) {
        let cfg = config::get();
        let max = match cfg.permissions.max_auto_risk.as_str() { "R0" => Risk::R0, "R2" => Risk::R2, _ => Risk::R1 };
        let prod = cfg.permissions.prod_confirm && self.target.as_ref().is_some_and(|t| t.production);
        let (e, why) = policy::decide_host(cfg.permissions.mode, max, &cfg.permissions.rules, "hosttools", "exec", cmd, cfg.permissions.dangerous_confirm, prod, &self.host());
        (e, policy::classify(cmd), why)
    }
    fn act(&mut self, a: Action, cx: &mut Context<Self>) {
        let cmd = match a.command() { Ok(c) => c, Err(e) => { self.note = Some((e, true)); cx.notify(); return } };
        let (eff, risk, why) = self.decide(&cmd);
        let _ = crate::audit_log().append("hosttools.decision", "policy", &format!("host={} risk={} effect={eff:?} command={cmd:?} reason={why:?}", self.host(), risk.label()));
        if !a.is_write() { self.execute(cmd, "read", cx); return; }
        match eff {
            Effect::Allow => self.execute(cmd, "policy", cx),
            Effect::Ask => { self.pending = Some((a, cmd, format!("{} · {why}", risk.label()))); cx.notify(); }
            Effect::Deny => { self.note = Some((tf("hosttools.denied", &[("cmd", &cmd), ("why", &why)]), true)); cx.notify(); }
        }
    }
    fn execute(&mut self, cmd: String, by: &'static str, cx: &mut Context<Self>) {
        self.pending = None;
        let host = self.host();
        let rx = self.run(cmd.clone(), Duration::from_secs(60));
        cx.spawn(async move |this, cx| {
            let r = rx.recv_async().await.unwrap_or_else(|_| Err("cancelled".into()));
            let _ = this.update(cx, |v, cx| {
                let (msg, err) = match &r { Ok((Some(0), o)) => (tf("hosttools.done", &[("cmd", &cmd), ("out", &o.lines().last().unwrap_or(""))]), false), Ok((code, o)) => (tf("hosttools.failed", &[("cmd", &cmd), ("code", &format!("{code:?}")), ("out", &o.lines().last().unwrap_or(""))]), true), Err(e) => (format!("{cmd}: {e}"), true) };
                let _ = crate::audit_log().append("hosttools.exec", by, &format!("host={host} command={cmd:?} ok={}", !err));
                if cmd.starts_with("docker logs") { if let Ok((_, o)) = &r { v.note = Some((o.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"), false)); } } else { v.note = Some((msg, err)); }
                v.refresh(cx);
            });
        }).detach();
        cx.notify();
    }
}

fn kb(n: u64) -> String { let g = n as f64 / 1048576.0; if g >= 1.0 { format!("{g:.1} GB") } else { format!("{:.0} MB", n as f64 / 1024.0) } }

impl HostToolsView {
    fn bar(pct: f32, col: gpui::Hsla) -> AnyElement {
        let th = cur();
        div().w(px(220.)).h(px(8.)).rounded_full().bg(c(th.surface0)).child(div().h_full().rounded_full().bg(col).w(gpui::relative(pct.clamp(0., 1.)))).into_any_element()
    }
    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mono = SharedString::from(config::get().font.family.clone());
        let Some(out) = &self.out else { return div().p_4().text_size(px(12.)).text_color(c(th.overlay1)).child(t("hosttools.loading")).into_any_element() };
        let out = match out { Ok(o) => o, Err(e) => return div().p_4().text_size(px(12.)).text_color(c(th.error)).child(e.clone()).into_any_element() };
        let row = || div().px_3().py(px(5.)).flex().items_center().gap_3().border_t_1().border_color(c(th.surface0)).text_size(px(12.)).text_color(c(th.text));
        let mut list = div().id("ht-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().border_1().border_color(c(th.surface0)).rounded_lg();
        match self.tab {
            Tab::Monitor => {
                let m = hosttools::parse_monitor(out);
                let used = m.mem_total_kb.saturating_sub(m.mem_avail_kb);
                let mp = if m.mem_total_kb > 0 { used as f32 / m.mem_total_kb as f32 } else { 0. };
                let lp = if m.cpus > 0 { m.load[0] / m.cpus as f32 } else { 0. };
                let col = |p: f32| if p > 0.9 { c(th.error) } else if p > 0.7 { c(th.warning) } else { c(th.success) };
                list = list.child(row().child(div().w(px(120.)).child(t("hosttools.load"))).child(Self::bar(lp, col(lp))).child(format!("{:.2} {:.2} {:.2} · {} CPU · {}", m.load[0], m.load[1], m.load[2], m.cpus, m.uptime)))
                    .child(row().child(div().w(px(120.)).child(t("hosttools.mem"))).child(Self::bar(mp, col(mp))).child(format!("{} / {} ({:.0}%)", kb(used), kb(m.mem_total_kb), mp * 100.)));
                for d in &m.disks { let p = d.pct as f32 / 100.; list = list.child(row().child(div().w(px(120.)).overflow_hidden().child(d.mount.clone())).child(Self::bar(p, col(p))).child(format!("{} / {} ({}%)", kb(d.used_kb), kb(d.size_kb), d.pct))); }
            }
            Tab::Processes => {
                list = list.child(row().text_color(c(th.overlay1)).child(div().w(px(70.)).child("PID")).child(div().w(px(90.)).child(t("hosttools.user"))).child(div().w(px(60.)).child("CPU%")).child(div().w(px(60.)).child("MEM%")).child(div().flex_1().child(t("hosttools.command"))));
                for (i, p) in hosttools::parse_ps(out).into_iter().enumerate() {
                    let pid = p.pid;
                    list = list.child(row().font_family(mono.clone()).child(div().w(px(70.)).child(p.pid.to_string())).child(div().w(px(90.)).overflow_hidden().child(p.user)).child(div().w(px(60.)).child(format!("{:.1}", p.cpu))).child(div().w(px(60.)).child(format!("{:.1}", p.mem))).child(div().flex_1().overflow_hidden().child(p.comm))
                        .child(ui::button(("ht-kill", i), t("hosttools.kill"), 2).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Kill { pid, force: false }, cx)))));
                }
            }
            Tab::Services => match hosttools::parse_services(out) {
                None => list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(tf("hosttools.no_systemd", &[("out", &out.lines().next().unwrap_or(""))]))),
                Some(svcs) => for (i, s) in svcs.into_iter().enumerate() {
                    let u1 = s.unit.clone(); let u2 = s.unit.clone();
                    let col = if s.active == "active" { c(th.success) } else if s.active == "failed" { c(th.error) } else { c(th.overlay1) };
                    list = list.child(row().child(div().w(px(260.)).overflow_hidden().font_family(mono.clone()).child(s.unit)).child(div().w(px(110.)).child(ui::tag(format!("{}/{}", s.active, s.sub), col))).child(div().flex_1().overflow_hidden().text_color(c(th.subtext)).child(s.desc))
                        .child(ui::button(("ht-rs", i), t("hosttools.restart"), 0).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Service { verb: "restart", unit: u1.clone() }, cx))))
                        .child(ui::button(("ht-st", i), t("hosttools.stop"), 2).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Service { verb: "stop", unit: u2.clone() }, cx)))));
                },
            },
            Tab::Docker => match hosttools::parse_docker(out) {
                Err(e) => list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(tf("hosttools.no_docker", &[("out", &e)]))),
                Ok(cs) => {
                    if cs.is_empty() { list = list.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("hosttools.no_containers"))); }
                    for (i, k) in cs.into_iter().enumerate() {
                        let (a, b, d) = (k.id.clone(), k.id.clone(), k.id.clone());
                        let col = if k.state == "running" { c(th.success) } else { c(th.overlay1) };
                        list = list.child(row().child(div().w(px(160.)).overflow_hidden().child(k.name)).child(div().w(px(200.)).overflow_hidden().text_color(c(th.subtext)).child(k.image)).child(div().flex_1().child(ui::tag(k.status, col)))
                            .child(ui::button(("ht-dl", i), t("hosttools.logs"), 3).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Docker { verb: "logs", id: d.clone() }, cx))))
                            .child(ui::button(("ht-dr", i), t("hosttools.restart"), 0).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Docker { verb: "restart", id: a.clone() }, cx))))
                            .child(ui::button(("ht-ds", i), t("hosttools.stop"), 2).h(px(20.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Docker { verb: "stop", id: b.clone() }, cx)))));
                    }
                }
            },
        }
        list.into_any_element()
    }
}

impl Render for HostToolsView {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let mut tabs = div().px_4().pt_3().flex().items_center().gap_2();
        for (i, tb) in Tab::ALL.into_iter().enumerate() {
            let label = match tb { Tab::Monitor => t("hosttools.monitor"), Tab::Processes => t("hosttools.processes"), Tab::Services => t("hosttools.services"), Tab::Docker => "Docker" };
            tabs = tabs.child(ui::button(("ht-tab", i), label, if tb == self.tab { 1 } else { 0 }).on_click(cx.listener(move |v, _, _, cx| v.set_tab(tb, cx))));
        }
        tabs = tabs.child(div().flex_1()).child(ui::button("ht-refresh", if self.busy { t("hosttools.loading") } else { t("sftp.refresh") }, 0).on_click(cx.listener(|v, _, _, cx| v.refresh(cx))));
        let pending = self.pending.clone().map(|(_, cmd, why)| div().mx_4().mt_2().p_2().rounded_md().bg(c(th.warning).opacity(0.1)).border_1().border_color(c(th.warning).opacity(0.6)).flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text))
            .child(icon(IconName::ShieldCheck, 14., c(th.warning))).child(div().flex_1().child(tf("hosttools.confirm", &[("cmd", &cmd), ("why", &why)])))
            .child(ui::button("ht-ok", t("ui.allow"), 1).on_click(cx.listener(move |v, _, _, cx| { let c2 = cmd.clone(); let _ = crate::audit_log().append("hosttools.approval", "user", &format!("allow command={c2:?}")); v.execute(c2, "user", cx); })))
            .child(ui::button("ht-no", t("ui.deny"), 3).on_click(cx.listener(|v, _, _, cx| { if let Some((_, c2, _)) = v.pending.take() { let _ = crate::audit_log().append("hosttools.approval", "user", &format!("deny command={c2:?}")); } cx.notify(); }))));
        let note = self.note.clone().map(|(m, err)| div().mx_4().mt_2().p_2().rounded_md().bg(c(th.crust)).text_size(px(12.)).text_color(if err { c(th.error) } else { c(th.subtext) }).child(m));
        ui::card(1100.).h(px(700.))
            .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(IconName::ChartPie, 16., c(th.accent)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("hosttools.title")))
                .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child(self.label()))
                .child(div().flex_1())
                .child(div().id("ht-close").size(px(28.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                    .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(|_, _, _, cx| cx.emit(HostToolsEvent::Close)))
                    .child(icon(IconName::X, 16., c(th.overlay1)))))
            .child(tabs).children(pending).children(note)
            .child(div().flex_1().min_h_0().p_4().flex().flex_col().child(self.body(cx)))
    }
}
