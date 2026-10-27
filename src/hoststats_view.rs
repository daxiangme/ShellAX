//! Host metrics at the right of the path bar (rings for CPU / memory / disk, load and latency as
//! small text) and the poller that feeds the shared sampler in `hoststats`.
use std::time::Duration;

use gpui::{div, prelude::*, px, AnyElement, Context, Window};
use ssh_term_proto::hoststats::{self, HostStats};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::ssh;

use crate::config;
use crate::termview::Spec;
use crate::theme::{self, c, cur};
use crate::ui;
use crate::Root;

/// Bar widths (px) at which more items fit: CPU + memory, plus latency, plus disk / load and labels.
const W_MIN: f32 = 260.;
const W_MID: f32 = 420.;
const W_FULL: f32 = 640.;

fn key_of(spec: &Spec) -> String {
    match spec { Spec::Local => hoststats::LOCAL_KEY.to_string(), Spec::Ssh(t) => t.pool_key() }
}

fn pct_text(frac: f32) -> String { format!("{:.0}%", (frac * 100.).round()) }

fn num(text: String, color: gpui::Hsla) -> gpui::Div {
    div().text_size(px(ui::fs_caption())).font_family(ui::mono_family()).text_color(color).whitespace_nowrap().child(text)
}

fn label(text: &'static str) -> gpui::Div {
    div().text_size(px(ui::fs_caption())).text_color(c(cur().overlay1)).whitespace_nowrap().child(t(text))
}

/// Optional caption, a ring and its number.
fn ring_item(caption: Option<&'static str>, frac: Option<f32>, text: String) -> gpui::Div {
    let th = cur();
    let col = frac.map_or(c(th.overlay1), theme::usage_color);
    div().flex().items_center().gap(px(ui::SP_1)).flex_shrink_0()
        .children(caption.map(label))
        .child(ui::ring(frac.unwrap_or(0.), col))
        .child(num(text, c(th.subtext)))
}

/// Multi-line hover text with the numbers the bar has no room for.
fn tooltip_text(spec: &Spec, s: &HostStats) -> String {
    let mut lines = vec![match spec { Spec::Local => t("hosttools.local").to_string(), Spec::Ssh(tg) => tg.label() }];
    if s.has_metrics() {
        let cpu = s.cpu_pct.map_or("—".to_string(), |p| format!("{p:.0}%"));
        lines.push(tf("hoststats.tip_cpu", &[("pct", &cpu), ("n", &s.cpus)]));
        if let Some(m) = s.mem_frac() {
            lines.push(tf("hoststats.tip_mem", &[("used", &hoststats::fmt_kb(s.mem_total_kb.saturating_sub(s.mem_avail_kb))), ("total", &hoststats::fmt_kb(s.mem_total_kb)), ("pct", &pct_text(m))]));
        }
        if let Some(d) = &s.disk {
            lines.push(tf("hoststats.tip_disk", &[("mount", &d.mount), ("used", &hoststats::fmt_kb(d.used_kb)), ("total", &hoststats::fmt_kb(d.size_kb)), ("pct", &format!("{}%", d.pct))]));
        }
        if let Some(l) = s.load {
            lines.push(tf("hoststats.tip_load", &[("a", &format!("{:.2}", l[0])), ("b", &format!("{:.2}", l[1])), ("c", &format!("{:.2}", l[2]))]));
        }
    } else {
        lines.push(t("hoststats.tip_nolinux").to_string());
    }
    if !s.local {
        let v = if s.timed_out { t("hoststats.timeout").to_string() } else { s.latency_ms.map_or("—".into(), |v| format!("{v}ms")) };
        lines.push(tf("hoststats.tip_latency", &[("v", &v)]));
    }
    lines.push(tf("hoststats.tip_updated", &[("t", &s.wall)]));
    lines.push(t("hoststats.tip_click").to_string());
    lines.join("\n")
}

impl Root {
    /// The readout for the path bar, or `None` when it is off, has no sample yet, or no item fits.
    pub(crate) fn host_stats_el(&self, spec: &Spec, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !config::get().terminal.show_host_stats { return None; }
        let s = hoststats::latest(&key_of(spec))?;
        let w = self.center_w.get();
        // width unknown until the first layout pass: assume it fits
        let tier = if w <= 1. || w >= W_FULL { 3 } else if w >= W_MID { 2 } else if w >= W_MIN { 1 } else { 0 };
        let th = cur();
        let mut items: Vec<gpui::Div> = vec![];
        if s.has_metrics() && tier >= 1 {
            items.push(ring_item((tier >= 3).then_some("hoststats.cpu"), s.cpu_pct.map(|p| p / 100.), s.cpu_pct.map_or("—".into(), |p| format!("{p:.0}%"))));
            if let Some(m) = s.mem_frac() { items.push(ring_item((tier >= 3).then_some("hoststats.mem"), Some(m), pct_text(m))); }
            if tier >= 3 {
                if let Some(d) = &s.disk { items.push(ring_item(Some("hoststats.disk"), Some(d.pct as f32 / 100.), format!("{}%", d.pct))); }
                if let Some(l) = s.load {
                    let col = s.load_frac().map_or(c(th.subtext), theme::usage_color);
                    items.push(div().flex().items_center().gap(px(ui::SP_1)).flex_shrink_0().child(label("hoststats.load")).child(num(format!("{:.2}", l[0]), col)));
                }
            }
        }
        if !s.local && (tier >= 2 || (tier >= 1 && !s.has_metrics())) {
            let (text, col) = match (s.timed_out, s.latency_ms) {
                (false, Some(ms)) => (format!("{ms}ms"), theme::latency_color(Some(ms))),
                _ => ("—".to_string(), theme::latency_color(None)),
            };
            items.push(div().flex_shrink_0().child(num(text, col)));
        }
        if items.is_empty() { return None; }
        let tip = gpui::SharedString::from(tooltip_text(spec, &s));
        let stale = s.at.elapsed() > Duration::from_secs(hoststats::POLL_SECS * 5);
        Some(div().id("host-stats").h(px(24.)).px_1().flex_shrink_0().flex().items_center().gap(px(ui::SP_2)).rounded(px(ui::R_SM)).cursor_pointer()
            .when(stale, |d| d.opacity(0.5))
            .hover(|d| d.bg(theme::hover_bg()))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, w, cx| this.show_host_dock(w, cx)))
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .children(items).into_any_element())
    }

    /// Open the host tools card for the focused pane's host (local machine or SSH); keep it if it already shows that host.
    pub(crate) fn show_host_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let want = self.focused_pane(window, cx).and_then(|p| match &p.read(cx).spec { Spec::Ssh(t) => Some(t.pool_key()), _ => None });
        if let Some(v) = &self.dock_host {
            if v.read(cx).target.as_ref().map(|t| t.pool_key()) == want { return; }
        }
        self.open_host_dock(window, cx);
    }

    /// Once a second: when the window is active and the focused pane's host has no sample younger than
    /// `POLL_SECS`, take one. Only that one host is sampled; remote hosts use the existing pooled
    /// connection and are never reconnected for this.
    pub(crate) fn host_stats_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !config::get().terminal.show_host_stats || !window.is_window_active() { return; }
        let Some(pane) = self.focused_pane(window, cx) else { return };
        let spec = pane.read(cx).spec.clone();
        let key = key_of(&spec);
        if hoststats::in_flight(&key) { return; }
        if hoststats::latest(&key).is_some_and(|s| s.at.elapsed() < Duration::from_secs(hoststats::POLL_SECS)) { return; }
        let max_age = Duration::from_millis(hoststats::FRESH_MS);
        match spec {
            Spec::Local => {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().spawn(async move { hoststats::refresh_local(max_age); }).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                }).detach();
            }
            Spec::Ssh(tg) => {
                let (tx, rx) = flume::bounded::<()>(1);
                self.rt.handle().spawn(async move {
                    if let Some(conn) = ssh::pooled(&tg).await { let _ = hoststats::refresh(&key, &conn, max_age).await; }
                    let _ = tx.send(());
                });
                cx.spawn(async move |this, cx| {
                    let _ = rx.recv_async().await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                }).detach();
            }
        }
    }
}
