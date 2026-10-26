//! 工具模块的左侧面板（搜索、收藏、最近使用、分类）与标签页打开逻辑。
use std::collections::HashSet;

use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, SharedString, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::i18n::t;
use ssh_term_proto::tools::{self, ToolSpec};

use crate::termview::Embed;
use crate::theme::{c, cur};
use crate::tools_view::{ToolEvent, ToolView};
use crate::ui::{self, icon, IconName};
use crate::{config, Root};

const RECENT_MAX: usize = 8;

#[derive(Default)]
pub struct ToolsPanel { search: Option<Entity<InputState>>, collapsed: HashSet<String> }

/// 某个工具当前可用的描述（内置）。
pub fn spec_of(id: &str) -> Option<ToolSpec> { tools::builtin().into_iter().find(|s| s.id == id) }

impl Root {
    pub(crate) fn tools_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
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
        let all = tools::builtin();
        let s = config::get();
        let favs = s.sidebar.tools_fav.clone();
        let recent = s.sidebar.tools_recent.clone();
        let open_ids: HashSet<String> = self.tabs.iter().filter_map(|t| match &t.layout.leaves()[0].read(cx).embed { Some(Embed::Tool(v)) => Some(v.read(cx).spec.id.clone()), _ => None }).collect();
        let row = |this: &mut Self, key: String, sp: &ToolSpec, fav: bool, cx: &mut Context<Self>| -> AnyElement {
            let id = sp.id.clone();
            let id2 = sp.id.clone();
            let open = open_ids.contains(&sp.id);
            let _ = this;
            div().id(SharedString::from(key)).group("tool-row").h(px(26.)).mx_2().px_2().rounded(px(6.)).flex().items_center().gap_2().cursor_pointer()
                .when(open, |d| d.bg(crate::theme::selected_bg()))
                .when(!open, |d| d.hover(|s| s.bg(crate::theme::hover_bg())))
                .tooltip({ let d = sp.desc.clone(); move |w, cx| gpui_component::tooltip::Tooltip::new(d.clone()).build(w, cx) })
                .on_click(cx.listener(move |this, _, w, cx| this.open_tool(&id, None, w, cx)))
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::FS_SECONDARY)).text_color(c(cur().text)).child(sp.name.clone()))
                .child(div().id(SharedString::from(format!("fav-{}", sp.id))).size(px(18.)).flex().items_center().justify_center().rounded(px(4.))
                    .when(!fav, |d| d.invisible().group_hover("tool-row", |s| s.visible()))
                    .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(if fav { t("tools.ui_fav_remove") } else { t("tools.ui_fav_add") }).build(w, cx))
                    .on_click(cx.listener(move |this, _, _w, cx| { cx.stop_propagation(); this.toggle_tool_fav(&id2, cx); }))
                    .child(icon(if fav { IconName::Star } else { IconName::StarOff }, 12., if fav { c(cur().warning) } else { c(cur().overlay1) })))
                .into_any_element()
        };
        let head = |label: String, key: Option<String>, collapsed: bool, cx: &mut Context<Self>| {
            let k = key.clone();
            div().id(SharedString::from(format!("th-{}", key.clone().unwrap_or(label.clone())))).h(px(24.)).mx_2().px_1().mt_1().flex().items_center().gap_1().cursor_pointer()
                .text_size(px(ui::FS_CAPTION)).font_weight(FontWeight::SEMIBOLD).text_color(c(cur().overlay1))
                .when_some(k, |d, k| d.on_click(cx.listener(move |this, _, _w, cx| { if !this.toolsp.collapsed.remove(&k) { this.toolsp.collapsed.insert(k.clone()); } cx.notify(); })))
                .child(icon(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }, 12., c(cur().overlay1)))
                .child(label)
        };
        let mut list = div().id("tools-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().pb_2();
        if !q.trim().is_empty() {
            let hits: Vec<&ToolSpec> = all.iter().filter(|s| tools::matches(s, &q)).collect();
            if hits.is_empty() { list = list.child(div().p_3().text_size(px(ui::FS_SECONDARY)).text_color(c(th.overlay1)).child(t("tools.ui_no_match"))); }
            for sp in hits { let f = favs.contains(&sp.id); list = list.child(row(self, format!("q-{}", sp.id), sp, f, cx)); }
        } else {
            let sections: [(&str, Vec<String>); 2] = [("fav", favs.clone()), ("recent", recent.iter().filter(|r| !favs.contains(r)).cloned().collect())];
            for (key, ids) in sections {
                let items: Vec<&ToolSpec> = ids.iter().filter_map(|i| all.iter().find(|s| &s.id == i)).collect();
                if items.is_empty() { continue; }
                let collapsed = self.toolsp.collapsed.contains(key);
                list = list.child(head(if key == "fav" { t("tools.ui_fav") } else { t("tools.ui_recent") }.to_string(), Some(key.to_string()), collapsed, cx));
                if !collapsed { for sp in items { let f = favs.contains(&sp.id); list = list.child(row(self, format!("{key}-{}", sp.id), sp, f, cx)); } }
            }
            for (cat, _) in tools::CATEGORIES {
                let items: Vec<&ToolSpec> = all.iter().filter(|s| s.cat == *cat).collect();
                if items.is_empty() { continue; }
                let collapsed = self.toolsp.collapsed.contains(*cat);
                list = list.child(head(format!("{} · {}", tools::cat_label(cat), items.len()), Some(cat.to_string()), collapsed, cx));
                if !collapsed { for sp in items { let f = favs.contains(&sp.id); list = list.child(row(self, format!("c-{}", sp.id), sp, f, cx)); } }
            }
        }
        div().flex_1().min_h_0().flex().flex_col()
            .child(crate::activity::panel_title(t("tool.tools"), None))
            .child(ui::field_row().pb_1().child(ui::search_field(&search, false)))
            .child(list)
            .into_any_element()
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
        let v = cx.new(|cx| ToolView::new(spec, rt, window, cx));
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
