//! 工具模块的左侧面板（搜索、收藏、最近使用、分类）与标签页打开逻辑。
use std::collections::HashSet;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, SharedString, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::tools::{self, ToolSpec};

use crate::ext_ui::{ExtEvent, ExtManager};
use crate::termview::Embed;
use crate::theme::{c, cur};
use crate::tools_view::{ToolEvent, ToolView};
use crate::ui::{self, IconName};
use crate::{config, Root};

const RECENT_MAX: usize = 8;

#[derive(Default)]
pub struct ToolsPanel { search: Option<Entity<InputState>>, collapsed: HashSet<String>, manage: bool }

/// 内置工具加上已安装扩展包里的工具。
pub fn all_tools() -> Vec<ToolSpec> { let mut v = tools::builtin(); v.extend(ssh_term_proto::ext::installed_tools()); v }

/// 某个工具当前可用的描述（内置或已安装的扩展包）。
pub fn spec_of(id: &str) -> Option<ToolSpec> { all_tools().into_iter().find(|s| s.id == id) }

impl Root {
    pub(crate) fn tools_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let search = match &self.toolsp.search {
            Some(s) => s.clone(),
            None => {
                let s = cx.new(|cx| InputState::new(window, cx).placeholder(t("tools.ui_search")));
                self._subs.push(cx.subscribe_in(&s, window, |_, _, ev: &InputEvent, _, cx| if matches!(ev, InputEvent::Change) { cx.notify() }));
                self.toolsp.search = Some(s.clone());
                s
            }
        };
        let q = search.read(cx).value().to_string();
        let all = all_tools();
        let extm = self.ext_manager(cx);
        if self.toolsp.manage {
            let back = ui::icon_button("tools-ext-back", IconName::ChevronLeft, ui::IconBtn::Md, t("ext.back_to_tools"))
                .on_click(cx.listener(|this, _, _w, cx| { this.toolsp.manage = false; cx.notify(); }));
            return div().flex_1().min_h_0().flex().flex_col()
                .child(ui::panel_header(t("ext.title"), None, None, vec![back.into_any_element()]))
                .child(div().id("tools-ext").flex_1().min_h_0().overflow_y_scroll().px(px(ui::LIST_INSET)).pb(px(ui::SP_2)).child(extm))
                .into_any_element();
        }
        let placeholders = ssh_term_proto::ext::placeholders(&ssh_term_proto::ext::installed());
        let s = config::get();
        let favs = s.sidebar.tools_fav.clone();
        let recent = s.sidebar.tools_recent.clone();
        let open_ids: HashSet<String> = self.tabs.iter().filter_map(|t| match &t.layout.leaves()[0].read(cx).embed { Some(Embed::Tool(v)) => Some(v.read(cx).spec.id.clone()), _ => None }).collect();
        let row = |this: &mut Self, key: String, sp: &ToolSpec, fav: bool, cx: &mut Context<Self>| -> AnyElement {
            let id = sp.id.clone();
            let id2 = sp.id.clone();
            let open = open_ids.contains(&sp.id);
            let _ = this;
            let gid = SharedString::from(format!("tr-{key}"));
            ui::list_row(SharedString::from(key), 0, open).group(gid.clone())
                .tooltip({ let d = sp.desc.clone(); move |w, cx| gpui_component::tooltip::Tooltip::new(d.clone()).build(w, cx) })
                .on_click(cx.listener(move |this, _, w, cx| this.open_tool(&id, None, w, cx)))
                .child(ui::row_title(sp.name.clone()))
                .child(div().flex_1())
                .child(ui::row_actions(gid).when(fav || open, |d| d.visible())
                    .child(ui::icon_button_state(SharedString::from(format!("fav-{}", sp.id)), if fav { IconName::Star } else { IconName::StarOff }, ui::IconBtn::Sm, if fav { t("tools.ui_fav_remove") } else { t("tools.ui_fav_add") }, fav, false)
                        .on_click(cx.listener(move |this, _, _w, cx| { cx.stop_propagation(); this.toggle_tool_fav(&id2, cx); }))))
                .into_any_element()
        };
        let ghost = |this: &Self, key: String, sp: &ToolSpec, cx: &mut Context<Self>| -> AnyElement {
            let pack = sp.pack.clone();
            let m = this.extm.as_ref().map(|e| e.read(cx));
            let prog = m.and_then(|m| m.progress(&pack));
            let busy = m.is_some_and(|m| m.busy(&pack));
            let tip = tf("ext.needs_pack", &[("p", &ssh_term_proto::ext::pack_name(&pack))]);
            let label = match prog { Some(p) => format!("{:.0}%", p * 100.), None if busy => t("ext.preparing").to_string(), None => t("ext.download").to_string() };
            // 未安装的扩展工具：只读行，文字用提示色，右侧是下载按钮
            div().id(SharedString::from(key)).min_h(px(ui::row_h())).mx(px(ui::LIST_INSET)).pl(px(ui::ROW_PX)).pr(px(4.)).rounded(px(ui::R_MD)).flex().items_center().gap(px(6.))
                .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
                .child(ui::row_title(sp.name.clone()).text_color(c(cur().overlay1)))
                .child(div().flex_1())
                .child(ui::button_sm(SharedString::from(format!("dl-{}-{}", sp.pack, sp.id)), label, 3)
                    .on_click(cx.listener(move |this, _, _w, cx| { cx.stop_propagation(); if let Some(e) = this.extm.clone() { e.update(cx, |m, cx| m.install(&pack, cx)); } })))
                .into_any_element()
        };
        let head = |label: String, key: String, count: Option<usize>, collapsed: bool, cx: &mut Context<Self>| {
            let k = key.clone();
            ui::section_header(SharedString::from(format!("th-{key}")), label, count, Some(collapsed)).mt(px(ui::SP_1))
                .on_click(cx.listener(move |this, _, _w, cx| { if !this.toolsp.collapsed.remove(&k) { this.toolsp.collapsed.insert(k.clone()); } cx.notify(); }))
        };
        let mut list = div().id("tools-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().pb(px(ui::SP_2));
        if !q.trim().is_empty() {
            let hits: Vec<&ToolSpec> = all.iter().filter(|s| tools::matches(s, &q)).collect();
            if hits.is_empty() { list = list.child(ui::empty_state(IconName::Search, t("tools.ui_no_match"), None, vec![], false)); }
            for sp in hits { let f = favs.contains(&sp.id); list = list.child(row(self, format!("q-{}", sp.id), sp, f, cx)); }
            for sp in placeholders.iter().filter(|s| tools::matches(s, &q)) { list = list.child(ghost(self, format!("qg-{}", sp.id), sp, cx)); }
        } else {
            let sections: [(&str, Vec<String>); 2] = [("fav", favs.clone()), ("recent", recent.iter().filter(|r| !favs.contains(r)).cloned().collect())];
            for (key, ids) in sections {
                let items: Vec<&ToolSpec> = ids.iter().filter_map(|i| all.iter().find(|s| &s.id == i)).collect();
                if items.is_empty() { continue; }
                let collapsed = self.toolsp.collapsed.contains(key);
                list = list.child(head(if key == "fav" { t("tools.ui_fav") } else { t("tools.ui_recent") }.to_string(), key.to_string(), None, collapsed, cx));
                if !collapsed { for sp in items { let f = favs.contains(&sp.id); list = list.child(row(self, format!("{key}-{}", sp.id), sp, f, cx)); } }
            }
            for (cat, _) in tools::CATEGORIES {
                let items: Vec<&ToolSpec> = all.iter().filter(|s| s.cat == *cat).collect();
                let ghosts: Vec<&ToolSpec> = placeholders.iter().filter(|s| s.cat == *cat).collect();
                if items.is_empty() && ghosts.is_empty() { continue; }
                let collapsed = self.toolsp.collapsed.contains(*cat);
                list = list.child(head(tools::cat_label(cat).to_string(), cat.to_string(), Some(items.len() + ghosts.len()), collapsed, cx));
                if !collapsed {
                    for sp in items { let f = favs.contains(&sp.id); list = list.child(row(self, format!("c-{}", sp.id), sp, f, cx)); }
                    for sp in ghosts { list = list.child(ghost(self, format!("cg-{}", sp.id), sp, cx)); }
                }
            }
        }
        div().flex_1().min_h_0().flex().flex_col()
            .child(ui::panel_header(t("tool.tools"), None, None, vec![
                ui::icon_button("tools-ext-open", IconName::Puzzle, ui::IconBtn::Md, t("ext.title"))
                    .on_click(cx.listener(|this, _, _w, cx| { this.toolsp.manage = true; cx.notify(); })).into_any_element(),
            ]))
            .child(ui::field_row().pb(px(ui::SP_2)).child(ui::search_field(&search, false)))
            .child(list)
            .into_any_element()
    }

    /// 扩展管理实体（工具面板与设置页共用）。
    pub(crate) fn ext_manager(&mut self, cx: &mut Context<Self>) -> Entity<ExtManager> {
        if let Some(e) = &self.extm { return e.clone(); }
        let e = cx.new(|_| ExtManager::new());
        self._subs.push(cx.subscribe(&e, |_, _, _ev: &ExtEvent, cx| cx.notify()));
        self.extm = Some(e.clone());
        e
    }

    fn toggle_tool_fav(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        let _ = config::update(|s| { let f = &mut s.sidebar.tools_fav; if let Some(i) = f.iter().position(|x| *x == id) { f.remove(i); } else { f.push(id.clone()); } });
        cx.notify();
    }

    /// 打开（或切换到）某个工具的标签页；`preset` 放进第一个输入框。
    pub(crate) fn open_tool(&mut self, id: &str, preset: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let _ = config::update(|s| { let r = &mut s.sidebar.tools_recent; r.retain(|x| x != id); r.insert(0, id.to_string()); r.truncate(RECENT_MAX); });
        let existing = self.tabs.iter().position(|t| matches!(&t.layout.leaves()[0].read(cx).embed, Some(Embed::Tool(v)) if v.read(cx).spec.id == id));
        if let Some(i) = existing {
            self.active = i;
            self.leave_settings();
            if let (Some(p), Some(Embed::Tool(v))) = (preset, self.tabs[i].layout.leaves()[0].read(cx).embed.clone()) { v.update(cx, |v, cx| v.preset(p, window, cx)); }
            self.focus_active(window, cx);
            cx.notify();
            return;
        }
        let Some(spec) = spec_of(id) else { return };
        let rt = self.rt.handle().clone();
        let hist = self.tool_hist.entry(id.to_string()).or_default().clone();
        let v = cx.new(|cx| ToolView::new(spec, rt, hist, window, cx));
        self._subs.push(cx.subscribe_in(&v, window, |this: &mut Self, _, ev: &ToolEvent, w, cx| match ev { ToolEvent::SendToTerminal(s) => this.send_to_terminal(s.clone(), w, cx) }));
        if let Some(p) = preset { v.update(cx, |v, cx| v.preset(p, window, cx)); }
        let pane = self.embed_pane(Embed::Tool(v), window, cx);
        self.push_pane(pane, window, cx);
    }

    /// 把文本填入最近使用的终端输入行（不回车执行；多行合并为一行）。
    pub(crate) fn send_to_terminal(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let line = text.trim().lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        if line.is_empty() { return; }
        let last = self.ws_last.get("hosts").copied();
        let is_term = |p: &Entity<crate::termview::TermView>, cx: &gpui::App| { let v = p.read(cx); !v.embedded() && v.session.is_some() };
        let mut pick = None;
        for (i, tab) in self.tabs.iter().enumerate() {
            for p in tab.layout.leaves() {
                if !is_term(&p, cx) { continue; }
                if Some(tab.layout.leaves()[0].entity_id()) == last || pick.is_none() { pick = Some((i, p.clone())); }
            }
        }
        let Some((i, pane)) = pick else { self.toast(t("tools.ui_no_terminal").to_string(), cx); return };
        pane.update(cx, |v, cx| { v.insert_text(&line); cx.notify(); });
        self.active = i;
        self.leave_settings();
        self.focus_active(window, cx);
        cx.notify();
    }
}
