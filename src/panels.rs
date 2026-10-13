//! Side tools opened from the sidebar function area: audit log viewer, SFTP, port forwarding,
//! AI / Agent overview. Audit and the agent overview are real; SFTP remote side and port
//! forwarding are UI frames only (marked 即将支持).
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
        let records = audit::read_all(&path);
        let mono = SharedString::from(config::get().font.family.clone());
        let mut list = div().id("audit-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        list = list.child(div().flex().items_center().gap_3().px_4().py(px(6.)).bg(c(th.mantle)).text_size(px(11.)).text_color(c(th.overlay1))
            .child(div().w(px(40.)).child("#")).child(div().w(px(150.)).child("时间 (UTC+8)")).child(div().w(px(130.)).child("类型"))
            .child(div().w(px(70.)).child("主体")).child(div().flex_1().child("详情")).child(div().w(px(90.)).child("hash")));
        if records.is_empty() { list = list.child(div().p_5().text_size(px(12.5)).text_color(c(th.overlay1)).child("暂无审计记录")); }
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
            Some(Ok(n)) => ui::tag(format!("✓ 哈希链完整，{n} 条"), c(th.success)).into_any_element(),
            Some(Err((line, why))) => ui::tag(format!("✗ 第 {line} 行：{why}"), c(th.error)).into_any_element(),
            None => div().into_any_element(),
        };
        let toolbar = div().px_4().py_3().flex().items_center().gap_2().border_b_1().border_color(c(th.surface0))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(format!("{}  ·  共 {} 条", path.display(), records.len())))
            .child(div().flex_1()).child(verify_el)
            .child(ui::button("audit-verify", "校验哈希链", 0).on_click(cx.listener(|this, _, _w, cx| {
                this.audit_verify = Some(audit::verify(&config::get().audit_path()));
                cx.notify();
            })))
            .child(ui::button("audit-export", "导出", 0).on_click(cx.listener(|this, _, _w, cx| {
                let src = config::get().audit_path();
                let dst = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join(format!("agentshell-audit-{}.jsonl", chrono::Local::now().format("%Y%m%d-%H%M%S")));
                this.toast(match std::fs::copy(&src, &dst) { Ok(_) => format!("已导出到 {}", dst.display()), Err(e) => format!("导出失败：{e}") }, cx);
            })));
        ui::modal(panel_shell("审计日志", IconName::ScrollText, "追加写入 · SHA-256 哈希链", cx).child(toolbar).child(list)).into_any_element()
    }

    pub(crate) fn sftp_panel(&mut self, cx: &mut Context<Root>) -> AnyElement {
        let th = cur();
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
        let mut entries: Vec<(String, bool, u64)> = std::fs::read_dir(&home).map(|d| d.filter_map(Result::ok)
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| { let md = e.metadata().ok(); (e.file_name().to_string_lossy().to_string(), md.as_ref().is_some_and(|m| m.is_dir()), md.map(|m| m.len()).unwrap_or(0)) }).collect()).unwrap_or_default();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let pane = |title: String, body: AnyElement| div().flex_1().h_full().flex().flex_col().border_1().border_color(c(th.surface0)).rounded_lg().overflow_hidden()
            .child(div().h(px(32.)).px_3().flex().items_center().gap_2().bg(c(th.mantle)).text_size(px(12.)).text_color(c(th.subtext)).child(title))
            .child(body);
        let mut local = div().id("sftp-local").flex_1().overflow_y_scroll().flex().flex_col();
        for (i, (name, dir, size)) in entries.iter().enumerate().take(200) {
            local = local.child(div().id(("lf", i)).h(px(28.)).px_3().flex().items_center().gap_2().text_size(px(12.)).text_color(c(th.text)).hover(|d| d.bg(c(cur().surface0)))
                .child(icon(if *dir { IconName::Folder } else { IconName::FileText }, 14., if *dir { c(th.warning) } else { c(th.overlay1) }))
                .child(div().flex_1().child(name.clone()))
                .child(div().text_size(px(11.)).text_color(c(th.overlay0)).child(if *dir { "—".to_string() } else { human(*size) })));
        }
        let target = self.focused_ssh_label(cx).unwrap_or_else(|| "（当前标签不是 SSH 会话）".into());
        let remote = div().flex_1().flex().flex_col().items_center().justify_center().gap_2()
            .child(icon(IconName::FolderSync, 28., c(th.overlay0)))
            .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child("远端文件浏览（复用当前 SSH 连接的 SFTP 子系统）"))
            .child(ui::soon());
        let body = div().flex_1().min_h_0().p_4().flex().gap_3()
            .child(pane(format!("本机 · {}", home.display()), local.into_any_element()))
            .child(div().w(px(36.)).flex().flex_col().items_center().justify_center().gap_2().opacity(0.4)
                .child(icon(IconName::Upload, 16., c(th.overlay1))).child(icon(IconName::Download, 16., c(th.overlay1))))
            .child(pane(format!("远端 · {target}"), remote.into_any_element()));
        ui::modal(panel_shell("SFTP 文件", IconName::FolderSync, "本机列表为真实数据；远端浏览与传输即将支持", cx).child(body)).into_any_element()
    }

    pub(crate) fn port_panel(&mut self, cx: &mut Context<Root>) -> AnyElement {
        let th = cur();
        let field = |label: &str, ph: &str| div().flex().flex_col().gap_1()
            .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(label.to_string()))
            .child(div().h(px(30.)).w(px(150.)).px_2().rounded_md().border_1().border_color(c(th.surface1)).bg(c(th.mantle)).flex().items_center()
                .text_size(px(12.)).text_color(c(th.overlay0)).child(ph.to_string()));
        let kinds = vec![("L".to_string(), "本地 -L".to_string()), ("R".to_string(), "远程 -R".to_string()), ("D".to_string(), "动态 SOCKS -D".to_string())];
        let form = div().p_4().m_4().rounded_lg().border_1().border_color(c(th.surface0)).flex().flex_col().gap_3().opacity(0.6)
            .child(div().flex().items_center().gap_2().child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child("新建转发")).child(ui::soon()))
            .child(ui::segmented("pf-kind", &kinds, "L", |_, _, _| {}))
            .child(div().flex().gap_3().child(field("本地地址", "127.0.0.1")).child(field("本地端口", "8080")).child(field("远端主机", "localhost")).child(field("远端端口", "80")))
            .child(div().flex().gap_2().child(ui::button("pf-add", "添加并启动", 1)));
        let empty = div().flex_1().flex().flex_col().items_center().justify_center().gap_2()
            .child(icon(IconName::ArrowLeftRight, 28., c(th.overlay0)))
            .child(div().text_size(px(12.5)).text_color(c(th.subtext)).child("暂无端口转发"))
            .child(div().text_size(px(11.5)).text_color(c(th.overlay1)).child("转发规则将按主机保存，并登记到端口注册表（PRD 5.8）避免冲突"));
        ui::modal(panel_shell("端口转发", IconName::ArrowLeftRight, "界面框架 · 后端即将支持", cx).child(form).child(empty)).into_any_element()
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
                            .child(div().text_size(px(11.)).text_color(c(th.overlay1)).child(format!("标签 {} · {}", ti + 1, v.title))))
                        .child(ui::tag(st.label_zh(), col)));
                    n += 1;
                    let _ = AgentState::Idle;
                }
            }
        }
        if n == 0 { agents = agents.child(div().p_3().text_size(px(12.)).text_color(c(th.overlay1)).child("当前没有检测到运行中的 Claude Code / Codex / Grok")); }
        let ai = div().p_3().rounded_lg().bg(c(th.mantle)).border_1().border_color(c(th.surface0)).flex().flex_col().gap_2()
            .child(div().flex().items_center().gap_2().child(icon(IconName::Sparkles, 15., c(th.accent)))
                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child("内置 AI 助手")).child(ui::soon()))
            .child(div().text_size(px(12.)).text_color(c(th.subtext)).child(format!("服务商 {} · 模型 {} · 脱敏 {} · 权限模式 {}", s.ai.provider, s.ai.model, if s.ai.redact { "开" } else { "关" }, s.permissions.mode.label())))
            .child(div().h(px(64.)).px_3().rounded_md().border_1().border_color(c(th.surface1)).flex().items_center().text_size(px(12.)).text_color(c(th.overlay0))
                .child("向 AI 提问，或让它解释选中的输出…（即将支持）"));
        let body = div().flex_1().min_h_0().p_4().flex().flex_col().gap_3()
            .child(ai)
            .child(div().pt_2().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(c(th.subtext)).child("外部 Agent（实时检测）"))
            .child(agents)
            .child(div().flex().gap_2().child(ui::button("ai-settings", "AI 设置", 0).on_click(cx.listener(|this, _, w, cx| this.open_settings(crate::settings_view::Cat::Ai, w, cx))))
                .child(ui::button("perm-settings", "权限规则", 0).on_click(cx.listener(|this, _, w, cx| this.open_settings(crate::settings_view::Cat::Permissions, w, cx)))));
        ui::modal(panel_shell("AI / Agent", IconName::Bot, "Agent 状态为真实检测；内置 AI 对话即将支持", cx).child(body)).into_any_element()
    }
}

fn human(n: u64) -> String {
    if n < 1024 { format!("{n} B") } else if n < 1 << 20 { format!("{:.1} KB", n as f64 / 1024.0) } else if n < 1 << 30 { format!("{:.1} MB", n as f64 / 1048576.0) } else { format!("{:.1} GB", n as f64 / 1073741824.0) }
}

#[allow(dead_code)]
fn _unused(_: Overlay) {}
