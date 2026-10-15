//! Side tools opened from the sidebar function area: audit log viewer, SFTP, port forwarding,
//! AI / Agent overview. SFTP and port forwarding live in sftp_view.rs / port_view.rs.
use ssh_term_proto::i18n::{t, tf};
use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString};
use ssh_term_proto::agent::AgentState;
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
            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title.to_string()))
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
        if records.is_empty() { list = list.child(div().p_5().text_size(px(12.5)).text_color(c(th.overlay1)).child(t("ui.no_audit_records_yet"))); }
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
            .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child(t("sftp.need_ssh")));
        ui::modal(panel_shell(t("ui.sftp_files"), IconName::FolderSync, t("ui.remote_browsing_reuses_the_sftp"), cx).child(body)).into_any_element()
    }

    pub(crate) fn port_panel(&mut self, _cx: &mut Context<Root>) -> AnyElement {
        match self.port_view.clone() { Some(v) => ui::modal(v).into_any_element(), None => div().into_any_element() }
    }

    pub(crate) fn ai_panel(&mut self, cx: &mut Context<Root>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let mut agents = div().flex().flex_col().rounded_lg().border_1().border_color(c(th.surface0));
        let mut n = 0;
        for (ti, tab) in self.tabs.iter().enumerate() {
            for p in tab.layout.leaves() {
                let v = p.read(cx);
                if let Some((name, st)) = v.agent_label() {
                    let col = crate::state_color(st, &th);
                    agents = agents.child(div().flex().items_center().gap_3().px_3().py(px(8.)).when(n > 0, |d| d.border_t_1().border_color(c(th.surface0)))
                        .child(icon(IconName::Bot, 15., col))
                        .child(div().flex_1().flex().flex_col().child(div().text_size(px(12.5)).text_color(c(th.text)).child(name))
                            .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(tf("panels.tab_n", &[("n", &(ti + 1)), ("title", &v.title)]))))
                        .child(ui::tag(st.label(), col)));
                    n += 1;
                    let _ = AgentState::Idle;
                }
            }
        }
        if n == 0 { agents = agents.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child(t("ui.no_running_claude_code_codex"))); }
        let mut launch = div().flex().flex_wrap().items_center().gap_2().child(div().text_size(px(12.)).text_color(c(th.subtext)).child(t("ui.launch_agent_title")));
        for (k, q) in s.agents.quick_launch.iter().enumerate().take(9) {
            launch = launch.child(ui::button(("ql", k), tf("ui.launch_agent_btn", &[("name", &q.name)]), 0).on_click(cx.listener(move |this, _, w, cx| { this.overlay = Overlay::None; this.launch_agent(k, w, cx); })));
        }
        let ai = div().p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface0)).flex().flex_col().gap_2()
            .child(div().flex().items_center().gap_2().child(icon(IconName::Sparkles, 15., c(th.accent)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(t("ui.built_in_ai_assistant")))
                .child(div().flex_1())
                .child(ui::button("ai-chat-open", t("chat.open"), 1).on_click(cx.listener(|this, _, w, cx| { this.open_chat(w, cx); }))))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(tf("panels.ai_summary", &[("provider", &s.ai.default_provider().map(|p| p.name.clone()).unwrap_or_else(|| t("ai.none").into())), ("model", &s.ai.default_provider().map(|p| p.model.clone()).unwrap_or_default()), ("redact", &if s.ai.redact { t("ui.on") } else { t("ui.off_2") }), ("mode", &s.permissions.mode.label())])))
            .child(div().h(px(64.)).px_3().rounded_md().border_1().border_color(c(th.surface1)).flex().items_center().text_size(px(12.)).text_color(c(th.overlay0))
                .child(t("ui.in_a_terminal_type_what")));
        let body = div().flex_1().min_h_0().p_4().flex().flex_col().gap_3()
            .child(ai)
            .child(div().pt_2().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.subtext)).child(t("ui.external_agents_live_detection")))
            .child(agents)
            .child(launch)
            .child(div().flex().gap_2().child(ui::button("ai-settings", t("ui.ai_settings"), 0).on_click(cx.listener(|this, _, w, cx| this.open_settings(crate::settings_view::Cat::Ai, w, cx))))
                .child(ui::button("perm-settings", t("ui.permission_rules"), 0).on_click(cx.listener(|this, _, w, cx| this.open_settings(crate::settings_view::Cat::Permissions, w, cx)))));
        ui::modal(panel_shell("AI / Agent", IconName::Bot, t("ui.agent_state_is_detected_live"), cx).child(body)).into_any_element()
    }
}


#[allow(dead_code)]
fn _unused(_: Overlay) {}
