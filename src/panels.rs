//! Side tools opened from the sidebar function area: audit log viewer, SFTP, port forwarding,
//! AI / Agent overview. SFTP and port forwarding live in sftp_view.rs / port_view.rs.
use ssh_term_proto::i18n::{t, tf};
use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString};
use ssh_term_proto::audit;

use crate::config;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{Overlay, Root};

fn panel_shell(title: &str, ic: IconName, sub: &str, cx: &mut Context<Root>) -> gpui::Div {
    let th = cur();
    ui::card(980.).h(px(640.))
        .child(div().h(px(48.)).px_5().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
            .child(icon(ic, 16., c(th.accent)))
            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title.to_string()))
            .child(div().pl_2().text_size(px(11.)).text_color(c(th.overlay0)).child(sub.to_string()))
            .child(div().flex_1())
            .child(div().id("panel-close").size(px(28.)).rounded_md().flex().items_center().justify_center().cursor_pointer()
                .hover(|d| d.bg(c(cur().surface0))).on_click(cx.listener(|this, _, w, cx| this.close_overlay(w, cx)))
                .child(icon(IconName::X, 16., c(th.overlay1)))))
}

impl Root {
    pub(crate) fn audit_panel(&mut self, cx: &mut Context<Root>) -> AnyElement {
        let th = cur();
        let path = config::get().audit_path();
        let all = audit::read_all(&path);
        let (hosts, sessions) = audit::facets(&all);
        let filter = self.audit_filter.clone();
        let records: Vec<audit::Record> = filter.apply(&all).into_iter().cloned().collect();
        let mono = SharedString::from(config::get().font.family.clone());
        let mut list = div().id("audit-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        list = list.child(div().flex().items_center().gap_3().px_4().py(px(6.)).bg(c(th.mantle)).text_size(px(11.)).text_color(c(th.overlay1))
            .child(div().w(px(40.)).child("#")).child(div().w(px(150.)).child(t("ui.time_utc_8"))).child(div().w(px(130.)).child(t("ui.kind")))
            .child(div().w(px(70.)).child(t("ui.principal"))).child(div().flex_1().child(t("ui.detail"))).child(div().w(px(90.)).child("hash")));
        if records.is_empty() { list = list.child(div().p_5().text_size(px(13.)).text_color(c(th.overlay1)).child(t("ui.no_audit_records_yet"))); }
        for r in records.iter().rev().take(500) {
            let col = if r.kind.starts_with("permission") { c(th.warning) } else if r.kind.starts_with("hostkey") { c(th.error) } else if r.kind.starts_with("ssh") { c(th.info) } else { c(th.accent) };
            list = list.child(div().flex().items_center().gap_3().px_4().py(px(6.)).border_t_1().border_color(c(th.surface0)).text_size(px(12.)).text_color(c(th.text))
                .child(div().w(px(40.)).text_color(c(th.overlay1)).child(r.seq.to_string()))
                .child(div().w(px(150.)).text_color(c(th.subtext)).child(r.ts.replace('T', " ").chars().take(19).collect::<String>()))
                .child(div().w(px(130.)).flex().child(ui::tag(r.kind.clone(), col)))
                .child(div().w(px(70.)).text_color(c(th.subtext)).child(r.actor.clone()))
                .child(div().flex_1().min_w_0().overflow_hidden().child(r.detail.clone()))
                .child(div().w(px(90.)).font_family(mono.clone()).text_size(px(11.)).text_color(c(th.overlay0)).child(r.hash[..10].to_string())));
        }
        let verify_el = match &self.audit_verify {
            Some(Ok(n)) => ui::tag(tf("panels.chain_ok", &[("n", &n)]), c(th.success)).into_any_element(),
            Some(Err((line, why))) => ui::tag(tf("panels.chain_bad", &[("line", &line), ("why", &why)]), c(th.error)).into_any_element(),
            None => div().into_any_element(),
        };
        let toolbar = div().px_4().py_3().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(tf("panels.audit_total", &[("path", &path.display()), ("n", &records.len())])))
            .child(div().text_size(px(12.)).text_color(c(th.overlay0)).child(tf("panels.audit_of", &[("n", &all.len())])))
            .child(div().flex_1()).child(verify_el)
            .child(ui::button("audit-verify", t("ui.verify_hash_chain"), 0).on_click(cx.listener(|this, _, _w, cx| {
                this.audit_verify = Some(audit::verify(&config::get().audit_path()));
                cx.notify();
            })))
            .child(ui::button("audit-export", t("ui.export"), 0).on_click(cx.listener(|this, _, _w, cx| {
                let src = config::get().audit_path();
                let dst = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join(format!("shellax-audit-{}.jsonl", chrono::Local::now().format("%Y%m%d-%H%M%S")));
                this.toast(match std::fs::copy(&src, &dst) { Ok(_) => tf("panels.exported", &[("path", &dst.display())]), Err(e) => tf("panels.export_failed", &[("e", &e)]) }, cx);
            })));
        // filter chips: decision / host / session (PRD 6.x audit filtering)
        let chip = |id: (&'static str, usize), label: String, on: bool| ui::button(gpui::ElementId::from((SharedString::from(id.0), id.1)), label, if on { 1 } else { 3 }).h(px(24.));
        let mut frow = div().px_4().py_2().flex().flex_wrap().items_center().gap_1().border_b_1().border_color(c(th.surface0)).text_size(px(11.))
            .child(div().text_color(c(th.overlay1)).pr_1().child(t("panels.filter_decision").to_string()));
        for (i, (val, key)) in [(None, "panels.filter_all"), (Some("allow"), "panels.filter_allow"), (Some("ask"), "panels.filter_ask"), (Some("deny"), "panels.filter_deny")].into_iter().enumerate() {
            let on = filter.decision.as_deref() == val;
            frow = frow.child(chip(("audit-dec", i), t(key).to_string(), on).on_click(cx.listener(move |this, _, _w, cx| { this.audit_filter.decision = val.map(String::from); cx.notify(); })));
        }
        for (label, base, vals, is_host) in [(t("panels.filter_host"), "audit-host", hosts, true), (t("panels.filter_session"), "audit-sess", sessions, false)] {
            if vals.is_empty() { continue; }
            frow = frow.child(div().w(px(10.))).child(div().text_color(c(th.overlay1)).pr_1().child(label.to_string()));
            let cur_v = if is_host { filter.host.clone() } else { filter.session.clone() };
            frow = frow.child(chip((base, 0), t("panels.filter_all").to_string(), cur_v.is_none()).on_click(cx.listener(move |this, _, _w, cx| { if is_host { this.audit_filter.host = None } else { this.audit_filter.session = None }; cx.notify(); })));
            for (i, v) in vals.into_iter().take(12).enumerate() {
                let on = cur_v.as_deref() == Some(v.as_str());
                let shown: String = if v.chars().count() > 18 { format!("{}…", v.chars().take(17).collect::<String>()) } else { v.clone() };
                frow = frow.child(chip((base, i + 1), shown, on).on_click(cx.listener(move |this, _, _w, cx| { let nv = if on { None } else { Some(v.clone()) }; if is_host { this.audit_filter.host = nv } else { this.audit_filter.session = nv }; cx.notify(); })));
            }
        }
        ui::modal(panel_shell(t("ui.audit_log"), IconName::ScrollText, t("ui.append_only_sha_256_hash"), cx).child(toolbar).child(frow).child(list)).into_any_element()
    }

    pub(crate) fn sftp_panel(&mut self, cx: &mut Context<Root>) -> AnyElement {
        if let Some(v) = self.sftp_current.clone() { return ui::modal(v).into_any_element(); }
        let th = cur();
        let body = div().flex_1().flex().flex_col().items_center().justify_center().gap_2()
            .child(icon(IconName::FolderSync, 28., c(th.overlay0)))
            .child(div().text_size(px(13.)).text_color(c(th.subtext)).child(t("sftp.need_ssh")));
        ui::modal(panel_shell(t("ui.sftp_files"), IconName::FolderSync, t("ui.remote_browsing_reuses_the_sftp"), cx).child(body)).into_any_element()
    }

    pub(crate) fn port_panel(&mut self, _cx: &mut Context<Root>) -> AnyElement {
        match self.port_view.clone() { Some(v) => ui::modal(v).into_any_element(), None => div().into_any_element() }
    }

    /// One right-column card: rounded panel with a 40px title row (collapse + close).
    /// Width and the resize handle belong to the column (`dock_column`).
    pub(crate) fn dock_frame(&mut self, dock_id: &'static str, title: SharedString, ic: IconName, body: impl IntoElement, cx: &mut Context<Root>) -> AnyElement {
        let th = cur();
        let close_id = SharedString::from(format!("{dock_id}-close"));
        let min_id = SharedString::from(format!("{dock_id}-min"));
        let collapsed = self.dock_min.contains(dock_id);
        let btn = |id: SharedString| div().id(id).size(px(26.)).rounded_md().flex().items_center().justify_center().cursor_pointer().hover(|d| d.bg(c(cur().surface0)));
        let mut dock = div().id(dock_id).w_full().flex().flex_col().min_h_0().overflow_hidden()
            .rounded(px(crate::card_layout::CARD_RADIUS)).bg(crate::theme::card_bg());
        dock = dock.child(div().h(px(40.)).px_3().flex_shrink_0().flex().items_center().gap_2().when(!collapsed, |d| d.border_b_1()).border_color(c(th.surface0))
            .child(icon(ic, 15., c(th.accent)))
            .child(div().flex_1().min_w_0().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
            .child(btn(min_id).on_click(cx.listener(move |this, _, _w, cx| { if this.dock_min.remove(dock_id) { *this.dock_flip.entry(dock_id).or_default() += 1; } else { this.dock_min.insert(dock_id); } cx.notify(); }))
                .child(icon(if collapsed { IconName::ChevronDown } else { IconName::ChevronUp }, 15., c(th.overlay1))))
            .child(btn(close_id).on_click(cx.listener(move |this, _, w, cx| this.close_dock(dock_id, w, cx)))
                .child(icon(IconName::X, 16., c(th.overlay1)))));
        if collapsed { return dock.flex_shrink_0().into_any_element(); }
        let body = div().flex_1().min_h_0().flex().flex_col().child(body);
        match self.dock_flip.get(dock_id).copied() {
            // re-expanded: short ease-out reveal (offset only; the column size does not change)
            Some(n) if !crate::motion::reduced() => {
                use gpui::AnimationExt;
                dock.child(body.with_animation((dock_id, n as usize), gpui::Animation::new(crate::motion::ms(crate::motion::PANEL_CLOSE_MS)).with_easing(crate::motion::ease_out), |d, t| d.relative().top(px((1. - t) * -12.)))).into_any_element()
            }
            _ => dock.child(body).into_any_element(),
        }
    }

    /// Right column: host status, AI and the snippets/history card stacked top to bottom.
    /// Shared width (drag the left edge); the first two expanded cards split by `dock_split`.
    pub(crate) fn dock_column(&mut self, window: &mut gpui::Window, cx: &mut Context<Root>) -> Option<AnyElement> {
        let mut cards: Vec<(&'static str, AnyElement)> = vec![];
        if let Some(v) = self.dock_host.clone() {
            let title = SharedString::from(format!("{} · {}", t("hosttools.title"), v.read(cx).label()));
            // cached: its 5 s refresh repaints only this card; the window re-renders it only when it notifies
            let el = v.cached(gpui::StyleRefinement::default().size_full());
            cards.push(("host-dock", self.dock_frame("host-dock", title, IconName::Monitor, el, cx)));
        }
        if self.dock_ai { cards.push(("ai-dock", self.ai_dock(cx))); }
        if matches!(self.dock, crate::SideDock::Snippets | crate::SideDock::History) {
            let id = if self.dock == crate::SideDock::Snippets { "snip-dock" } else { "hist-dock" };
            cards.push((id, self.library_dock(window, cx)));
        }
        if cards.is_empty() { self.dock_shown = false; return None; }
        // first card in an empty column: slide the column in (terminal resizes once, at the end)
        if !self.dock_shown && !crate::motion::reduced() {
            let seq = self.dock_anim.map(|a| a.1 + 1).unwrap_or(1);
            self.dock_anim = Some((true, seq));
            crate::motion::hold(crate::motion::PANEL_OPEN_MS + 20);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(crate::motion::ms(crate::motion::PANEL_OPEN_MS)).await;
                let _ = this.update(cx, |r, cx| if r.dock_anim.is_some_and(|a| a == (true, seq)) { r.dock_anim = None; cx.notify(); });
            }).detach();
        }
        self.dock_shown = true;
        let open: Vec<usize> = cards.iter().enumerate().filter(|(_, (id, _))| !self.dock_min.contains(id)).map(|(i, _)| i).collect();
        let split = self.dock_split.clamp(0.15, 0.85);
        let first_open = open.first().copied();
        let n_open = open.len().max(1) as f32;
        let mut col = div().id("dock-col").relative().w(px(self.ai_width)).h_full().flex_shrink_0().flex().flex_col().min_h_0()
            .on_drag_move(cx.listener(|this, ev: &gpui::DragMoveEvent<crate::sidebar::AiResize>, _w, cx| {
                let w = f32::from(ev.bounds.right() - ev.event.position.x).clamp(280.0, 560.0);
                if (w - this.ai_width).abs() >= 1.0 { this.ai_width = w; cx.notify(); }
            }))
            .on_drag_move(cx.listener(|this, ev: &gpui::DragMoveEvent<DockSplit>, _w, cx| {
                let h = f32::from(ev.bounds.size.height).max(1.0);
                let r = (f32::from(ev.event.position.y - ev.bounds.top()) / h).clamp(0.15, 0.85);
                if (r - this.dock_split).abs() > 0.002 { this.dock_split = r; cx.notify(); }
            }));
        let total = cards.len();
        for (i, (id, el)) in cards.into_iter().enumerate() {
            let collapsed = self.dock_min.contains(id);
            let mut slot = div().w_full().min_h_0().flex().flex_col();
            slot = if collapsed { slot.flex_shrink_0() } else if open.len() >= 2 {
                let share = if Some(i) == first_open { split } else { (1.0 - split) / (n_open - 1.0) };
                slot.flex_basis(gpui::relative(share)).flex_grow(1.).flex_shrink(1.)
            } else { slot.flex_1() };
            col = col.child(slot.child(el));
            if i + 1 < total {
                // gap between cards; below the first expanded card it doubles as the split handle
                let handle = Some(i) == first_open && open.len() >= 2;
                col = col.child(div().id(("dock-gap", i)).h(px(crate::card_layout::CARD_GAP)).flex_shrink_0().w_full()
                    .when(handle, |d| d.cursor_row_resize().hover(|d| d.bg(c(cur().accent).opacity(0.35)))
                        .on_drag(DockSplit, |d: &DockSplit, _, _, cx| cx.new(|_| d.clone()))
                        .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| if ev.click_count() >= 2 { this.dock_split = 0.5; cx.notify(); }))));
            }
        }
        col = col.child(div().id("dock-col-resize").absolute().top_0().left_0().w(px(6.)).h_full().cursor_col_resize()
            .hover(|d| d.bg(c(cur().accent).opacity(0.35)))
            .on_drag(crate::sidebar::AiResize, |d: &crate::sidebar::AiResize, _, _, cx| cx.new(|_| d.clone()))
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _w, cx| if ev.click_count() >= 2 { this.ai_width = config::AI_DOCK_W; this.save_ai_width(); cx.notify(); })));
        Some(match self.dock_anim {
            Some((show, seq)) => crate::motion::slide(div().h_full().flex_shrink_0().w(px(self.ai_width)), ("dock-anim", seq as usize), col.into_any_element(), self.ai_width + 8., crate::motion::Edge::Right, show),
            None => col.into_any_element(),
        })
    }

    /// Right-hand AI tool window: conversation fills the column, terminal stays visible.
    /// What the built-in assistant talks about: the focused pane of the active tab.
    pub(crate) fn ai_target(&self, cx: &gpui::App) -> crate::ai_ctx::Target {
        use crate::ai_ctx::Target;
        let Some(p) = self.tabs.get(self.active).map(|t| t.focused.clone()) else { return Target::Other };
        let v = p.read(cx);
        if let Some(d) = &v.db { return Target::Db(d.downgrade()); }
        match &v.embed {
            Some(crate::termview::Embed::Kv(k)) => Target::Redis(k.downgrade()),
            Some(crate::termview::Embed::Dock(d)) => Target::Docker(d.downgrade()),
            Some(_) => Target::Other,
            None if v.sftp.is_some() => Target::Other,
            None => Target::Terminal,
        }
    }
    pub(crate) fn ai_dock(&mut self, cx: &mut Context<Root>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let target = self.ai_target(cx);
        let term = self.tool.workspace() == crate::activity::Tool::Hosts && target.is_terminal();
        if let Some(v) = self.chat_view.clone() { v.update(cx, |c, cx| c.set_target(target, cx)); }
        let mut strip = div().px_3().py(px(6.)).flex().flex_wrap().items_center().gap_1().border_b_1().border_color(c(th.surface0));
        let mut n = 0usize;
        for (ti, tab) in self.tabs.iter().enumerate() {
            for p in tab.layout.leaves() {
                let v = p.read(cx);
                if let Some((name, st)) = v.agent_label() {
                    let col = crate::state_color(st, &th);
                    strip = strip.child(ui::tag(format!("{name} · {}", tf("panels.tab_n", &[("n", &(ti + 1)), ("title", &v.title)])), col));
                    n += 1;
                }
            }
        }
        if n == 0 { strip = strip.child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(t("ui.no_running_claude_code_codex"))); }
        for (k, q) in s.agents.quick_launch.iter().enumerate().take(6) {
            if q.command.trim().is_empty() { continue; }
            strip = strip.child(ui::button(("aidock-ql", k), q.name.clone(), 0).h(px(24.)).px_2().text_size(px(11.))
                .on_click(cx.listener(move |this, _, w, cx| this.launch_agent(k, w, cx))));
        }
        let mut body = div().flex_1().min_h_0().flex().flex_col().when(term, |d| d.child(strip));
        if let Some(v) = self.chat_view.clone() { body = body.child(div().flex_1().min_h_0().child(v)); }
        self.dock_frame("ai-dock", t("ui.built_in_ai_assistant").into(), IconName::Sparkles, body, cx)
    }
}


#[allow(dead_code)]
fn _unused(_: Overlay) {}

/// Drag payload for the split between stacked right-column cards.
#[derive(Clone)]
pub(crate) struct DockSplit;
impl gpui::Render for DockSplit {
    fn render(&mut self, _w: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement { div() }
}
