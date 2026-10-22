//! Port forwarding manager: saved forwards (data_dir/forwards.toml), add/start/stop/delete,
//! live state and counters, auto-start when an SSH session to that host opens.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use gpui::{div, prelude::*, px, Context, Entity, FontWeight, Window};
use gpui_component::input::{Input, InputState};
use gpui_component::switch::Switch;
use gpui_component::Sizable;
use ssh_term_proto::forward::{self, Kind, Running, Spec, State};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh::{self, SshTarget};

use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub enum PortEvent { Close }
impl gpui::EventEmitter<PortEvent> for PortView {}

pub struct PortView {
    rt: tokio::runtime::Handle,
    prompts: ssh::Prompts,
    pub target: Option<SshTarget>,
    pub items: Vec<Arc<Running>>,
    kind: Kind,
    auto: bool,
    fields: [Entity<InputState>; 4],
    error: Option<String>,
    ticking: bool,
}

fn store_path() -> std::path::PathBuf { crate::data_dir().join("forwards.toml") }

impl PortView {
    pub fn new(rt: tokio::runtime::Handle, prompts: ssh::Prompts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mk = |ph: &str, v: &str, window: &mut Window, cx: &mut Context<Self>| { let (ph, v) = (ph.to_string(), v.to_string()); cx.new(|cx| InputState::new(window, cx).placeholder(ph).default_value(v)) };
        let fields = [mk("127.0.0.1", "127.0.0.1", window, cx), mk("8080", "", window, cx), mk("localhost", "localhost", window, cx), mk("80", "", window, cx)];
        let items = forward::load(&store_path()).into_iter().map(Running::new).collect();
        Self { rt, prompts, target: None, items, kind: Kind::L, auto: false, fields, error: None, ticking: false }
    }
    fn save(&self) { let _ = forward::save(&store_path(), &self.items.iter().map(|r| r.spec.clone()).collect::<Vec<_>>()); }
    fn val(&self, i: usize, cx: &Context<Self>) -> String { self.fields[i].read(cx).value().trim().to_string() }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tg) = self.target.clone() else { self.error = Some(t("pf.need_ssh").into()); cx.notify(); return };
        let port = |s: String| s.parse::<u16>().ok();
        let bp = self.val(1, cx);
        let spec = Spec { host: tg.label(), kind: self.kind, bind_addr: self.val(0, cx), bind_port: if bp.is_empty() && self.kind == Kind::R { 0 } else { port(bp).unwrap_or(0) },
            dest_host: self.val(2, cx), dest_port: port(self.val(3, cx)).unwrap_or(0), auto_start: self.auto };
        if let Err(e) = spec.validate() { self.error = Some(format!("{e}")); cx.notify(); return; }
        self.error = None;
        for i in [1, 3] { self.fields[i].update(cx, |s, cx| s.set_value("", window, cx)); }
        let r = Running::new(spec);
        self.items.push(r.clone());
        self.save();
        self.start(r, tg, cx);
    }
    pub fn start(&mut self, r: Arc<Running>, tg: SshTarget, cx: &mut Context<Self>) {
        let p = self.prompts.clone();
        *r.state.lock().unwrap() = State::Starting;
        self.rt.spawn(async move {
            match ssh::connect_shared(&tg, &p).await {
                Ok((conn, _)) => { let _ = r.start(conn).await; }
                Err(e) => *r.state.lock().unwrap() = State::Error(format!("{e:#}")),
            }
        });
        self.tick(cx);
    }
    fn stop(&mut self, r: Arc<Running>, cx: &mut Context<Self>) { self.rt.spawn(async move { r.stop().await }); self.tick(cx); }
    /// Called when an SSH session to `tg` opens: start its auto-start forwards.
    pub fn auto_start(&mut self, tg: &SshTarget, cx: &mut Context<Self>) {
        if self.target.is_none() { self.target = Some(tg.clone()); }
        let label = tg.label();
        let todo: Vec<_> = self.items.iter().filter(|r| r.spec.auto_start && r.spec.host == label && !r.is_on()).cloned().collect();
        for r in todo { self.start(r, tg.clone(), cx); }
    }
    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.ticking { return; }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            for _ in 0..20 { cx.background_executor().timer(std::time::Duration::from_millis(250)).await; if this.update(cx, |_, cx| cx.notify()).is_err() { return; } }
            let _ = this.update(cx, |v, _| v.ticking = false);
        }).detach();
    }
}

fn human(n: u64) -> String { if n < 1024 { format!("{n} B") } else if n < 1 << 20 { format!("{:.1} KB", n as f64 / 1024.0) } else { format!("{:.1} MB", n as f64 / 1048576.0) } }

impl Render for PortView {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let labels = match self.kind { Kind::L => [t("ui.local_address"), t("ui.local_port"), t("ui.remote_host"), t("ui.remote_port")], Kind::R => [t("pf.remote_bind"), t("pf.remote_port_0"), t("pf.local_dest"), t("pf.local_dest_port")], Kind::D => [t("ui.local_address"), t("ui.local_port"), "", ""] };
        let mut inputs = div().flex().gap_3();
        for (i, l) in labels.iter().enumerate() {
            if l.is_empty() { continue; }
            inputs = inputs.child(div().flex().flex_col().gap_1().w(px(170.)).child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(l.to_string())).child(Input::new(&self.fields[i]).small()));
        }
        let kinds = vec![("L".to_string(), t("ui.local_l").to_string()), ("R".to_string(), t("ui.remote_r").to_string()), ("D".to_string(), t("ui.dynamic_socks_d").to_string())];
        let sel = match self.kind { Kind::L => "L", Kind::R => "R", Kind::D => "D" };
        let me = cx.entity().downgrade();
        let form = div().p_4().m_4().rounded_lg().border_1().border_color(c(th.surface0)).flex().flex_col().gap_3()
            .child(div().flex().items_center().gap_2().child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.new_forward")))
                .child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(self.target.as_ref().map(|t| tf("pf.on_host", &[("h", &t.label())])).unwrap_or_else(|| t("pf.need_ssh").into()))))
            .child(ui::segmented("pf-kind", &kinds, sel, move |v, _, cx| { let _ = me.update(cx, |p, cx| { p.kind = match v.as_str() { "R" => Kind::R, "D" => Kind::D, _ => Kind::L }; cx.notify() }); }))
            .child(inputs)
            .child(div().flex().items_center().gap_3()
                .child(Switch::new("pf-auto").checked(self.auto).label(t("pf.auto_start")).on_click(cx.listener(|v, on: &bool, _, cx| { v.auto = *on; cx.notify() })))
                .child(ui::button("pf-add", t("ui.add_and_start"), 1).on_click(cx.listener(|v, _, w, cx| v.add(w, cx))))
                .children(self.error.clone().map(|e| div().text_size(px(12.)).text_color(c(th.error)).child(e))));
        let mut list = div().id("pf-list").mx_4().flex_1().min_h_0().overflow_y_scroll().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0));
        if self.items.is_empty() {
            list = list.child(div().p_5().flex().flex_col().items_center().gap_2().child(icon(IconName::ArrowLeftRight, 24., c(th.overlay0)))
                .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(t("ui.no_port_forwards"))).child(div().text_size(px(12.)).text_color(c(th.overlay1)).child(t("ui.forwards_will_be_saved_per"))));
        }
        for (i, r) in self.items.iter().enumerate() {
            let st = r.state();
            let (label, col) = match &st { State::Stopped => (t("pf.stopped").to_string(), c(th.overlay1)), State::Starting => (t("pf.starting").to_string(), c(th.info)), State::Running { port } => (tf("pf.running", &[("p", port)]), c(th.success)), State::Error(_) => (t("pf.error").to_string(), c(th.error)) };
            let on = r.is_on();
            let (r1, r2) = (r.clone(), r.clone());
            let stats = tf("pf.stats", &[("a", &r.stats.active.load(Ordering::Relaxed)), ("t", &r.stats.total.load(Ordering::Relaxed)), ("b", &human(r.stats.bytes.load(Ordering::Relaxed)))]);
            list = list.child(div().px_3().py(px(8.)).flex().items_center().gap_3().when(i > 0, |d| d.border_t_1().border_color(c(th.surface0))).text_size(px(12.)).text_color(c(th.text))
                .child(icon(IconName::ArrowLeftRight, 14., col))
                .child(div().flex_1().flex().flex_col()
                    .child(div().child(r.spec.describe()))
                    .child(diagram(&r.spec, match &st { State::Running { port } => Some(*port), _ => None }, on && matches!(st, State::Running { .. })))
                    .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(format!("{} · {}{}", r.spec.host, stats, if r.spec.auto_start { format!(" · {}", t("pf.auto_start")) } else { String::new() })))
                    .when_some(match &st { State::Error(e) => Some(e.clone()), _ => None }, |d, e| d.child(div().text_size(px(11.)).text_color(c(th.error)).child(e))))
                .child(ui::tag(label, col))
                .child(if on { ui::button(("pf-stop", i), t("pf.stop"), 0).on_click(cx.listener(move |v, _, _, cx| v.stop(r1.clone(), cx))) }
                    else { ui::button(("pf-start", i), t("pf.start"), 1).on_click(cx.listener(move |v, _, _, cx| { match v.target.clone() { Some(tg) if tg.label() == r1.spec.host => v.start(r1.clone(), tg, cx), _ => { v.error = Some(tf("pf.open_host_first", &[("h", &r1.spec.host)])); cx.notify() } } })) })
                .child(ui::button(("pf-del", i), t("sftp.delete"), 2).on_click(cx.listener(move |v, _, _, cx| { let r = r2.clone(); v.stop(r.clone(), cx); v.items.retain(|x| !Arc::ptr_eq(x, &r)); v.save(); cx.notify() }))));
        }
        ui::card(980.).h(px(640.))
            .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
                .child(icon(IconName::ArrowLeftRight, 16., c(th.accent)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.port_forwarding")))
                .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child(t("pf.subtitle")))
                .child(div().flex_1())
                .child(div().id("pf-close").size(px(28.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                    .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(|_, _, _, cx| cx.emit(PortEvent::Close)))
                    .child(icon(IconName::X, 16., c(th.overlay1)))))
            .child(form).child(list).child(div().h(px(16.)))
    }
}

/// Port-forward diagram: [listener] ══ssh tunnel══ [ssh server] ──► [destination].
fn diagram(spec: &ssh_term_proto::forward::Spec, port: Option<u16>, live: bool) -> impl IntoElement {
    let th = cur();
    let (nodes, _) = spec.diagram(port);
    let role = |r: &str| match r { "local" => t("pf.node_local"), "remote" => t("pf.node_remote"), "ssh" => t("pf.node_ssh"), "remote-side" => t("pf.node_remote_side"), "local-side" => t("pf.node_local_side"), _ => t("pf.node_any") };
    let col = |i: usize| match i { 0 => c(th.accent), 1 => c(th.warning), _ => c(th.success) };
    let boxn = |i: usize, title: &str, addr: &str| div().px_2().py(px(3.)).rounded_md().border_1().border_color(col(i)).bg(col(i).opacity(0.08)).flex().flex_col()
        .child(div().text_size(px(11.)).text_color(col(i)).child(role(title).to_string())).child(div().text_size(px(12.)).text_color(c(th.text)).child(addr.to_string()));
    let line = if live { c(th.success) } else { c(th.overlay0) };
    let arrow = |label: String, thick: bool| div().flex().flex_col().items_center().min_w(px(90.))
        .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(label))
        .child(div().flex().items_center().w_full().child(div().flex_1().h(px(if thick { 3. } else { 1.5 })).bg(line)).child(div().text_size(px(11.)).text_color(line).child("▶")));
    let dir_label = match spec.kind { ssh_term_proto::forward::Kind::R => t("pf.tunnel_back").to_string(), _ => t("pf.tunnel").to_string() };
    div().mt_1().flex().items_center().gap_1()
        .child(boxn(0, &nodes[0].1, &nodes[0].2)).child(arrow(dir_label, true))
        .child(boxn(1, &nodes[1].1, &nodes[1].2)).child(arrow(t("pf.then").to_string(), false))
        .child(boxn(2, &nodes[2].1, &nodes[2].2))
}
