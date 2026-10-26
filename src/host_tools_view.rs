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

#[allow(dead_code)]
pub enum HostToolsEvent { Close }
impl gpui::EventEmitter<HostToolsEvent> for HostToolsView {}

pub struct HostToolsView {
    pub target: Option<SshTarget>,
    rt: tokio::runtime::Handle,
    prompts: ssh::Prompts,
    /// Extra list in the bottom card (services or containers); fetched on demand.
    tab: Tab,
    out: Option<Result<String, String>>,
    busy: bool,
    mon: Option<Result<String, String>>,
    ps: Option<Result<String, String>>,
    /// Previous `/proc/net/dev` sample for per-second rates.
    net: Option<(std::time::Instant, Vec<hosttools::NetIf>)>,
    net_rate: Vec<(String, f64, f64)>,
    stats_busy: bool,
    /// Write waiting for confirmation: (action, command, reason).
    pending: Option<(Action, String, String)>,
    note: Option<(String, bool)>,
}

impl HostToolsView {
    pub fn new(target: Option<SshTarget>, rt: tokio::runtime::Handle, prompts: ssh::Prompts, cx: &mut Context<Self>) -> Self {
        let mut me = Self { target, rt, prompts, tab: Tab::Services, out: None, busy: false, mon: None, ps: None, net: None, net_rate: vec![], stats_busy: false, pending: None, note: None };
        me.refresh_stats(cx);
        // live while the card is open: the loop ends once the view is dropped
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            if this.update(cx, |v, cx| v.refresh_stats(cx)).is_err() { break; }
        }).detach();
        me
    }
    fn host(&self) -> String { self.target.as_ref().map(|t| t.host.clone()).unwrap_or_else(|| "local".into()) }
    pub fn label(&self) -> String { self.target.as_ref().map(|t| t.label()).unwrap_or_else(|| t("hosttools.local").into()) }
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
    /// Overview / CPU / memory / disk / network / processes.
    pub fn refresh_stats(&mut self, cx: &mut Context<Self>) {
        if self.stats_busy { return; }
        self.stats_busy = true;
        let m = self.run(Tab::Monitor.read_command().to_string(), Duration::from_secs(20));
        let p = self.run(Tab::Processes.read_command().to_string(), Duration::from_secs(20));
        let n = self.run(hosttools::NET_COMMAND.to_string(), Duration::from_secs(20));
        cx.spawn(async move |this, cx| {
            let m = m.recv_async().await.unwrap_or_else(|_| Err("cancelled".into()));
            let p = p.recv_async().await.unwrap_or_else(|_| Err("cancelled".into()));
            let n = n.recv_async().await.unwrap_or_else(|_| Err("cancelled".into()));
            let _ = this.update(cx, |v, cx| {
                v.stats_busy = false;
                let (m, p) = (Some(m.map(|x| x.1)), Some(p.map(|x| x.1)));
                let old_rate = std::mem::take(&mut v.net_rate);
                let mut changed = v.mon != m || v.ps != p;
                v.mon = m;
                v.ps = p;
                let now = std::time::Instant::now();
                let cur = n.map(|x| hosttools::parse_netdev(&x.1)).unwrap_or_default();
                v.net_rate = match &v.net {
                    Some((t0, prev)) => { let dt = now.duration_since(*t0).as_secs_f64().max(0.5);
                        cur.iter().filter_map(|i| prev.iter().find(|p| p.name == i.name).map(|p| (i.name.clone(), i.rx.saturating_sub(p.rx) as f64 / dt, i.tx.saturating_sub(p.tx) as f64 / dt))).collect() }
                    None => cur.iter().map(|i| (i.name.clone(), 0., 0.)).collect(),
                };
                v.net = if cur.is_empty() { None } else { Some((now, cur)) };
                // displayed rates are rounded, so compare what is shown
                let shown = |r: &Vec<(String, f64, f64)>| r.iter().map(|(n, a, b)| format!("{n}{}{}", rate(*a), rate(*b))).collect::<Vec<_>>();
                changed |= shown(&old_rate) != shown(&v.net_rate);
                if changed { cx.notify(); }
            });
        }).detach();
        // nothing visible changes until the data arrives
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
    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) { if self.tab != tab || self.out.is_none() { self.tab = tab; self.out = None; self.pending = None; self.busy = false; self.refresh(cx); } }

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
                v.refresh_stats(cx);
                if v.out.is_some() { v.refresh(cx); }
            });
        }).detach();
        cx.notify();
    }
}

fn kb(n: u64) -> String { let g = n as f64 / 1048576.0; if g >= 1.0 { format!("{g:.1} GB") } else { format!("{:.0} MB", n as f64 / 1024.0) } }
fn rate(b: f64) -> String { if b >= 1048576. { format!("{:.1} MB/s", b / 1048576.) } else if b >= 1024. { format!("{:.0} KB/s", b / 1024.) } else { format!("{b:.0} B/s") } }

impl HostToolsView {
    fn bar(pct: f32, col: gpui::Hsla) -> AnyElement {
        let th = cur();
        div().flex_1().min_w(px(60.)).h(px(6.)).rounded_full().bg(c(th.surface0)).child(div().h_full().rounded_full().bg(col).w(gpui::relative(pct.clamp(0., 1.)))).into_any_element()
    }
    /// A titled sub-card in the dock column (same radius / border / padding as the AI panel's cards).
    fn card(title: impl Into<SharedString>, ic: IconName, right: Option<String>) -> gpui::Div {
        let th = cur();
        crate::card_layout::sub_card().flex_shrink_0().p(px(crate::card_layout::CARD_PAD + 4.)).flex().flex_col().gap(px(crate::card_layout::CARD_PAD))
            .child(div().flex().items_center().gap_2().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text))
                .child(icon(ic, 13., c(th.accent))).child(div().flex_1().child(title.into()))
                .children(right.map(|r| div().text_size(px(11.)).font_weight(FontWeight::NORMAL).text_color(c(th.overlay1)).child(r))))
    }
    fn meter(label: impl Into<SharedString>, pct: f32, value: String) -> gpui::Div {
        let th = cur();
        let col = if pct > 0.9 { c(th.error) } else if pct > 0.7 { c(th.warning) } else { c(th.success) };
        div().flex().items_center().gap_2().text_size(px(11.)).text_color(c(th.subtext))
            .child(div().w(px(64.)).flex_shrink_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(label.into()))
            .child(Self::bar(pct, col))
            .child(div().flex_shrink_0().text_color(c(th.text)).child(value))
    }
    fn muted(s: impl Into<SharedString>) -> AnyElement { div().text_size(px(11.)).text_color(c(cur().overlay1)).child(s.into()).into_any_element() }

    fn stat_cards(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let th = cur();
        let mono = SharedString::from(config::get().font.family.clone());
        let mut v: Vec<AnyElement> = vec![];
        match &self.mon {
            None => v.push(Self::card(t("hosttools.overview"), IconName::Monitor, None).child(Self::muted(t("hosttools.loading"))).into_any_element()),
            Some(Err(e)) => v.push(Self::card(t("hosttools.overview"), IconName::Monitor, None).child(div().text_size(px(11.)).text_color(c(th.error)).child(e.clone())).into_any_element()),
            Some(Ok(out)) => {
                let m = hosttools::parse_monitor(out);
                let used = m.mem_total_kb.saturating_sub(m.mem_avail_kb);
                let mp = if m.mem_total_kb > 0 { used as f32 / m.mem_total_kb as f32 } else { 0. };
                let lp = if m.cpus > 0 { m.load[0] / m.cpus as f32 } else { 0. };
                let kv = |k: SharedString, val: String| div().flex().gap_2().text_size(px(11.)).child(div().w(px(64.)).text_color(c(th.overlay1)).child(k)).child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().text_color(c(th.text)).child(val));
                v.push(Self::card(t("hosttools.overview"), IconName::Monitor, None)
                    .child(kv(t("hosttools.host").into(), self.label()))
                    .when(!m.uptime.is_empty(), |d| d.child(kv(t("hosttools.uptime").into(), m.uptime.trim_start_matches("up ").to_string())))
                    .child(kv("CPU".into(), tf("hosttools.cores", &[("n", &m.cpus)])))
                    .child(kv(t("hosttools.mem").into(), kb(m.mem_total_kb))).into_any_element());
                v.push(Self::card("CPU", IconName::Cpu, Some(format!("{:.2} / {:.2} / {:.2}", m.load[0], m.load[1], m.load[2])))
                    .child(Self::meter(t("hosttools.load"), lp, format!("{:.0}%", lp * 100.)))
                    .child(Self::muted(t("hosttools.load_hint"))).into_any_element());
                let mut mem = Self::card(t("hosttools.mem"), IconName::MemoryStick, Some(format!("{:.0}%", mp * 100.)))
                    .child(Self::meter("RAM", mp, format!("{} / {}", kb(used), kb(m.mem_total_kb))));
                if m.swap_total_kb > 0 {
                    let su = m.swap_total_kb.saturating_sub(m.swap_free_kb);
                    mem = mem.child(Self::meter("Swap", su as f32 / m.swap_total_kb as f32, format!("{} / {}", kb(su), kb(m.swap_total_kb))));
                }
                v.push(mem.into_any_element());
                if !m.disks.is_empty() {
                    let mut d = Self::card(t("hosttools.disk"), IconName::HardDrive, None);
                    for k in &m.disks { d = d.child(Self::meter(k.mount.clone(), k.pct as f32 / 100., format!("{} / {}", kb(k.used_kb), kb(k.size_kb)))); }
                    v.push(d.into_any_element());
                }
            }
        }
        if !self.net_rate.is_empty() {
            let mut d = Self::card(t("hosttools.network"), IconName::Network, None);
            for (name, rx, tx) in self.net_rate.iter().take(6) {
                d = d.child(div().flex().items_center().gap_2().text_size(px(11.)).text_color(c(th.text))
                    .child(div().w(px(64.)).overflow_hidden().text_ellipsis().text_color(c(th.subtext)).child(name.clone()))
                    .child(div().flex_1().child(format!("↓ {}", rate(*rx)))).child(div().flex_1().child(format!("↑ {}", rate(*tx)))));
            }
            v.push(d.into_any_element());
        }
        if let Some(Ok(out)) = &self.ps {
            let mut d = Self::card(t("hosttools.processes"), IconName::List, None)
                .child(div().flex().gap_2().text_size(px(10.)).text_color(c(th.overlay1)).child(div().w(px(52.)).child("PID")).child(div().flex_1().child(t("hosttools.command"))).child(div().w(px(40.)).child("CPU%")).child(div().w(px(40.)).child("MEM%")).child(div().w(px(36.))));
            for (i, p) in hosttools::parse_ps(out).into_iter().take(10).enumerate() {
                let pid = p.pid;
                d = d.child(div().flex().items_center().gap_2().text_size(px(11.)).text_color(c(th.text)).font_family(mono.clone())
                    .child(div().w(px(52.)).text_color(c(th.subtext)).child(p.pid.to_string()))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(p.comm))
                    .child(div().w(px(40.)).child(format!("{:.1}", p.cpu))).child(div().w(px(40.)).child(format!("{:.1}", p.mem)))
                    .child(div().w(px(36.)).child(ui::button(("ht-kill", i), t("hosttools.kill"), 2).h(px(18.)).px_1().text_size(px(10.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Kill { pid, force: false }, cx))))));
            }
            v.push(d.into_any_element());
        }
        v
    }

    /// Services / containers: loaded only when the user picks one.
    fn extra_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let mono = SharedString::from(config::get().font.family.clone());
        let mut head = div().flex().items_center().gap_1();
        for (i, tb) in [Tab::Services, Tab::Docker].into_iter().enumerate() {
            let label = if tb == Tab::Services { t("hosttools.services") } else { "Docker" };
            head = head.child(ui::button(("ht-tab", i), label, if self.out.is_some() && tb == self.tab { 1 } else { 0 }).h(px(22.)).text_size(px(11.)).on_click(cx.listener(move |v, _, _, cx| v.set_tab(tb, cx))));
        }
        let mut d = Self::card(t("hosttools.more"), IconName::Settings2, None).child(head);
        let row = || div().flex().items_center().gap_2().text_size(px(11.)).text_color(c(th.text));
        match &self.out {
            None if self.busy => d = d.child(Self::muted(t("hosttools.loading"))),
            None => {}
            Some(Err(e)) => d = d.child(div().text_size(px(11.)).text_color(c(th.error)).child(e.clone())),
            Some(Ok(out)) => match self.tab {
                Tab::Docker => match hosttools::parse_docker(out) {
                    Err(e) => d = d.child(Self::muted(tf("hosttools.no_docker", &[("out", &e)]))),
                    Ok(cs) => {
                        if cs.is_empty() { d = d.child(Self::muted(t("hosttools.no_containers"))); }
                        for (i, k) in cs.into_iter().enumerate() {
                            let (a, b, l) = (k.id.clone(), k.id.clone(), k.id.clone());
                            let col = if k.state == "running" { c(th.success) } else { c(th.overlay1) };
                            d = d.child(row().child(div().size(px(6.)).rounded_full().bg(col)).child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(k.name))
                                .child(ui::button(("ht-dl", i), t("hosttools.logs"), 3).h(px(18.)).px_1().text_size(px(10.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Docker { verb: "logs", id: l.clone() }, cx))))
                                .child(ui::button(("ht-dr", i), t("hosttools.restart"), 0).h(px(18.)).px_1().text_size(px(10.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Docker { verb: "restart", id: a.clone() }, cx))))
                                .child(ui::button(("ht-ds", i), t("hosttools.stop"), 2).h(px(18.)).px_1().text_size(px(10.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Docker { verb: "stop", id: b.clone() }, cx)))));
                        }
                    }
                },
                _ => match hosttools::parse_services(out) {
                    None => d = d.child(Self::muted(tf("hosttools.no_systemd", &[("out", &out.lines().next().unwrap_or(""))]))),
                    Some(svcs) => for (i, s) in svcs.into_iter().enumerate().take(60) {
                        let (u1, u2) = (s.unit.clone(), s.unit.clone());
                        let col = if s.active == "active" { c(th.success) } else if s.active == "failed" { c(th.error) } else { c(th.overlay1) };
                        d = d.child(row().font_family(mono.clone()).child(div().size(px(6.)).rounded_full().bg(col)).child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.unit))
                            .child(ui::button(("ht-rs", i), t("hosttools.restart"), 0).h(px(18.)).px_1().text_size(px(10.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Service { verb: "restart", unit: u1.clone() }, cx))))
                            .child(ui::button(("ht-st", i), t("hosttools.stop"), 2).h(px(18.)).px_1().text_size(px(10.)).on_click(cx.listener(move |v, _, _, cx| v.act(Action::Service { verb: "stop", unit: u2.clone() }, cx)))));
                    },
                },
            },
        }
        d.into_any_element()
    }
}

impl Render for HostToolsView {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let gap = px(crate::card_layout::CARD_PAD);
        let pending = self.pending.clone().map(|(_, cmd, why)| div().p_2().rounded(px(crate::card_layout::SUB_RADIUS)).bg(c(th.warning).opacity(0.1)).border_1().border_color(c(th.warning).opacity(0.6)).flex().flex_col().gap_2().text_size(px(12.)).text_color(c(th.text))
            .child(div().flex().gap_2().child(icon(IconName::ShieldCheck, 14., c(th.warning))).child(div().flex_1().child(tf("hosttools.confirm", &[("cmd", &cmd), ("why", &why)]))))
            .child(div().flex().gap_2().justify_end()
                .child(ui::button("ht-ok", t("ui.allow"), 1).on_click(cx.listener(move |v, _, _, cx| { let c2 = cmd.clone(); let _ = crate::audit_log().append("hosttools.approval", "user", &format!("allow command={c2:?}")); v.execute(c2, "user", cx); })))
                .child(ui::button("ht-no", t("ui.deny"), 3).on_click(cx.listener(|v, _, _, cx| { if let Some((_, c2, _)) = v.pending.take() { let _ = crate::audit_log().append("hosttools.approval", "user", &format!("deny command={c2:?}")); } cx.notify(); })))));
        let note = self.note.clone().map(|(m, err)| div().p_2().rounded(px(crate::card_layout::SUB_RADIUS)).bg(c(th.crust)).text_size(px(11.)).text_color(if err { c(th.error) } else { c(th.subtext) }).child(m));
        let cards = self.stat_cards(cx);
        let extra = self.extra_card(cx);
        div().id("ht-scroll").size_full().min_h_0().overflow_y_scroll().p(gap).flex().flex_col().gap(gap)
            .children(pending).children(note).children(cards).child(extra)
    }
}
