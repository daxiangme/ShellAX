//! Tab right-click menu, inline rename, closed-tab history, "move to new window" and the per-pane
//! hover header with split / close controls (PRD 5.23).
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::layout::Dir;

use crate::termview::TermView;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{chrome, config, Root, Spec, Tab};

const CLOSED_MAX: usize = 10;

impl Root {
    /// Close tab `i` (remembered for "reopen closed tab").
    pub(crate) fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() { return; }
        let node = Self::save_node(&self.tabs[i].layout, cx);
        let name = self.tabs[i].name.clone();
        self.closed_tabs.push((node, name));
        if self.closed_tabs.len() > CLOSED_MAX { self.closed_tabs.remove(0); }
        let module = Self::tab_module(&self.tabs[i], cx);
        let tab = self.tabs.remove(i);
        for p in tab.layout.leaves() { self.end_pane(&p, cx); }
        if self.active > i || self.active >= self.tabs.len() { self.active = self.active.saturating_sub(1); }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.after_close(module, i, cx);
        self.log("tab.close", format!("index={i}"));
        self.focus_active(window, cx);
        cx.notify();
    }
    fn close_others(&mut self, keep: usize, window: &mut Window, cx: &mut Context<Self>) {
        for i in (0..self.tabs.len()).rev() { if i != keep { self.close_tab(i, window, cx); } }
        self.active = 0;
    }
    fn close_right(&mut self, of: usize, window: &mut Window, cx: &mut Context<Self>) {
        for i in (of + 1..self.tabs.len()).rev() { self.close_tab(i, window, cx); }
    }
    pub(crate) fn reopen_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((node, name)) = self.closed_tabs.pop() else { return self.toast(t("tab.none_closed").into(), cx) };
        self.insert_tab_from(node, name, self.tabs.len(), window, cx);
        self.log("tab.reopen", "");
    }
    fn insert_tab_from(&mut self, node: ssh_term_proto::session_store::NodeSave, name: Option<String>, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.build_node(&node, window, cx);
        self.relink_follow(&layout, cx);
        let focused = layout.leaves()[0].clone();
        let at = at.min(self.tabs.len());
        self.tabs.insert(at, Tab { layout, focused, name });
        self.active = at;
        self.focus_active(window, cx);
        cx.notify();
    }
    /// Same panes (fresh shells in the same cwd / hosts), inserted right after `i`.
    fn duplicate_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(i) else { return };
        let node = Self::save_node(&tab.layout, cx);
        let name = tab.name.clone();
        self.insert_tab_from(node, name, i + 1, window, cx);
        self.log("tab.duplicate", format!("index={i}"));
    }
    /// Split the tab's focused pane (same host) right / down.
    fn split_tab(&mut self, i: usize, dir: Dir, window: &mut Window, cx: &mut Context<Self>) {
        self.active = i;
        if let Some(p) = self.tabs.get(i).map(|t| t.focused.clone()) { let fh = p.read(cx).focus.clone(); window.focus(&fh, cx); }
        self.split(Some(dir), window, cx);
    }
    /// Move a tab into a new window: a second shellax process restores it from its own layout
    /// file (shells start fresh in the same cwd / reconnect, like a restored layout).
    fn move_to_window(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(i) else { return };
        let l = ssh_term_proto::session_store::LayoutSave { version: 1, saved_at: chrono::Local::now().to_rfc3339(), active: 0, tabs: vec![Self::save_node(&tab.layout, cx)], names: vec![tab.name.clone()] };
        let dir = crate::data_dir().join("windows");
        let file = dir.join(format!("window-{}.json", ssh_term_proto::session_store::uuid_like()));
        let r = ssh_term_proto::session_store::save(&file, &l).map_err(|e| e.to_string()).and_then(|_| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            std::process::Command::new(exe).env("SHELLAX_LAYOUT_FILE", &file).env_remove("SHELLAX_NO_RESTORE").spawn().map(|_| ()).map_err(|e| e.to_string())
        });
        match r {
            Ok(()) => { self.log("tab.move_window", format!("file={}", file.display())); let t = self.tabs.remove(i); for p in t.layout.leaves() { self.end_pane(&p, cx); } self.active = self.active.min(self.tabs.len().saturating_sub(1)); self.focus_active(window, cx); }
            Err(e) => self.toast(tf("tab.move_failed", &[("e", &e)]), cx),
        }
        cx.notify();
    }
    pub(crate) fn start_rename(&mut self, i: usize, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        input.update(cx, |s, cx| { s.focus(window, cx); s.select_all(window, cx); });
        self._subs.push(cx.subscribe_in(&input, window, move |this, inp, ev: &InputEvent, w, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => { let v = inp.read(cx).value().trim().to_string(); this.finish_rename(i, Some(v), w, cx); }
            _ => {}
        }));
        self.tab_rename = Some((i, input));
        cx.notify();
    }
    /// `None` = cancel. An empty name goes back to the automatic title.
    pub(crate) fn finish_rename(&mut self, i: usize, v: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tab_rename.as_ref().is_some_and(|(j, _)| *j == i) { return; }
        self.tab_rename = None;
        if let (Some(v), Some(tab)) = (v, self.tabs.get_mut(i)) {
            tab.name = (!v.is_empty()).then_some(v.clone());
            self.log("tab.rename", format!("index={i} name={v:?}"));
            self.save_layout(cx);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(crate) fn open_new_menu(&mut self, pos: gpui::Point<gpui::Pixels>, w: &mut Window, cx: &mut Context<Self>) {
        self.new_menu = Some(pos);
        self.new_menu_at = std::time::Instant::now();
        self.new_menu_sel = 0;
        self.new_menu_search.update(cx, |s, cx| { s.set_value("", w, cx); s.focus(w, cx); });
        cx.notify();
    }

    fn close_new_menu(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        self.new_menu = None;
        self.new_menu_closed = std::time::Instant::now();
        self.focus_active(w, cx);
        cx.notify();
    }

    /// 菜单行：本地终端、分组 / 主机（与侧栏相同的目录树和顺序）、新建连接。
    fn new_menu_items(&self, cx: &Context<Self>) -> Vec<NewItem> {
        let s = config::get();
        let q = ssh_term_proto::hosttree::parse_query(&self.new_menu_search.read(cx).value(), &s.tags.all());
        let mut v = vec![NewItem::Local];
        for r in ssh_term_proto::hosttree::rows(&s.hosts, &s.groups, &self.new_menu_collapsed, &q) {
            v.push(match r {
                ssh_term_proto::hosttree::Row::Group { path, name, depth, count, collapsed } => NewItem::Group { path, name, depth, count, collapsed },
                ssh_term_proto::hosttree::Row::Host { index, depth } => NewItem::Host { index, depth },
            });
        }
        v.push(NewItem::NewHost);
        v
    }

    fn new_menu_activate(&mut self, item: NewItem, w: &mut Window, cx: &mut Context<Self>) {
        match item {
            NewItem::Local => { self.close_new_menu(w, cx); self.new_tab(Spec::Local, w, cx); }
            NewItem::Group { path, .. } => {
                if let Some(i) = self.new_menu_collapsed.iter().position(|c| *c == path) { self.new_menu_collapsed.remove(i); } else { self.new_menu_collapsed.push(path); }
                cx.notify();
            }
            NewItem::Host { index, .. } => {
                let name = config::get().hosts.get(index).map(|h| h.name.clone()).unwrap_or_default();
                self.close_new_menu(w, cx);
                match self.target_for(&name) { Some(t) => self.new_tab(Spec::Ssh(t), w, cx), None => self.toast(tf("sb.cannot_resolve", &[("name", &name)]), cx) }
            }
            NewItem::NewHost => { self.close_new_menu(w, cx); self.open_host_dialog(None, None, w, cx); }
        }
    }

    pub(crate) fn new_menu_el(&self, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let s = config::get();
        let items = self.new_menu_items(cx);
        let sel = self.new_menu_sel.min(items.len().saturating_sub(1));
        let searching = !self.new_menu_search.read(cx).value().trim().is_empty();
        let row = |i: usize| div().id(("newm", i)).h(px(ui::H_DEFAULT)).px(px(10.)).rounded(px(ui::R_MD)).flex().items_center().gap_2().text_size(px(ui::fs_body())).text_color(c(th.text)).cursor_pointer()
            .when(i == sel, |d| d.bg(crate::theme::menu_sel_bg()))
            .hover(|d| d.bg(crate::theme::menu_sel_bg()));
        let mut list = div().id("newm-list").max_h(px(360.)).overflow_y_scroll().flex().flex_col();
        let mut n_hosts = 0;
        for (i, it) in items.iter().enumerate() {
            let it2 = it.clone();
            let click = cx.listener(move |this, _, w, cx| this.new_menu_activate(it2.clone(), w, cx));
            match it {
                NewItem::Local => {
                    list = list.child(row(i).child(icon(IconName::SquareTerminal, ui::IC_MD, c(th.overlay1))).child(div().flex_1().child(t("newmenu.local")))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(if cfg!(target_os = "macos") { "⌘T" } else { "Ctrl+T" })).on_click(click));
                    if items.len() > 2 { list = list.child(ui::menu_sep()); }
                }
                NewItem::Group { name, depth, count, collapsed, .. } => {
                    let tags = s.groups.iter().find(|g| matches!(it, NewItem::Group { path, .. } if *path == g.path)).map(|g| g.tags.clone()).unwrap_or_default();
                    list = list.child(row(i).pl(px(10. + *depth as f32 * 14.)).text_color(c(th.subtext))
                        .child(icon(if *collapsed { IconName::ChevronRight } else { IconName::ChevronDown }, ui::IC_XS, c(th.overlay1)))
                        .child(icon(IconName::Folder, ui::IC_SM, c(th.accent)))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(gpui::FontWeight::MEDIUM).child(name.clone()))
                        .children(tags.iter().take(3).map(|t| div().size(px(6.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(t)))))
                        .child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(count.to_string()))
                        .on_click(click));
                }
                NewItem::Host { index, depth } => {
                    let Some(h) = s.hosts.get(*index) else { continue };
                    n_hosts += 1;
                    let addr = format!("{}{}", h.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default(), h.host);
                    list = list.child(row(i).pl(px(10. + *depth as f32 * 14. + if *depth > 0 { 12. } else { 0. })).tooltip(crate::ui::tip(addr.clone()))
                        .child(icon(IconName::Server, ui::IC_SM, c(th.overlay1)))
                        .child(div().flex_shrink_0().max_w(px(180.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(h.name.clone()))
                        .children(h.tags.iter().take(3).map(|t| div().size(px(6.)).flex_shrink_0().rounded_full().bg(c(s.tags.rgb(t)))))
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(addr))
                        .on_click(click));
                }
                NewItem::NewHost => {
                    if searching && n_hosts == 0 { list = list.child(div().px(px(10.)).py_2().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(t("newmenu.no_match"))); }
                    list = list.child(ui::menu_sep()).child(row(i).child(icon(IconName::Plus, ui::IC_MD, c(th.overlay1))).child(div().flex_1().child(t("newmenu.new_host"))).on_click(click));
                }
            }
        }
        let n = items.len();
        let pop = ui::popover().id("new-menu").occlude().w(px(340.)).p(px(5.)).flex().flex_col().gap_1()
            .on_mouse_down_out(cx.listener(|this, _, w, cx| {
                if this.new_menu_at.elapsed() < std::time::Duration::from_millis(250) { return; }
                this.close_new_menu(w, cx);
            }))
            .capture_key_down(cx.listener(move |this, ev: &gpui::KeyDownEvent, w, cx| {
                match ev.keystroke.key.as_str() {
                    "down" => { this.new_menu_sel = (this.new_menu_sel + 1) % n.max(1); }
                    "up" => { this.new_menu_sel = (this.new_menu_sel + n.max(1) - 1) % n.max(1); }
                    "enter" => {
                        let items = this.new_menu_items(cx);
                        if let Some(it) = items.get(this.new_menu_sel.min(items.len().saturating_sub(1))).cloned() { this.new_menu_activate(it, w, cx); }
                    }
                    "escape" => this.close_new_menu(w, cx),
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(div().p(px(3.)).child(ui::search_field(&self.new_menu_search, false)))
            .child(list);
        gpui::deferred(gpui::anchored().position(pos).snap_to_window_with_margin(px(ui::EDGE_MARGIN)).child(chrome::pop_in("new-menu-pop", pop))).with_priority(3).into_any_element()
    }

    pub(crate) fn tab_menu_el(&self, i: usize, pos: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let n = self.tabs.len();
        let mut list = ui::popover().id("tab-menu").occlude().min_w(px(220.)).p(px(5.)).flex().flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if this.tab_menu_at.elapsed() < std::time::Duration::from_millis(250) { return; }
                this.tab_menu = None;
                cx.notify();
            }));
        let mut k = 0usize;
        let mut id = || { k += 1; ("tabm", k) };
        macro_rules! item { ($ic:expr, $label:expr, $en:expr, $f:expr) => {{ let f = $f; let el = ui::menu_item(id(), Some($ic), $label, $en, false); if $en { el.on_click(cx.listener(move |this, _, w, cx| { this.tab_menu = None; f(this, w, cx); cx.notify(); })) } else { el } }} }
        let title = self.tabs.get(i).map(|t| t.name.clone().unwrap_or_else(|| crate::shown_title(t.focused.read(cx)).to_string())).unwrap_or_default();
        list = list
            .child(item!(IconName::TextCursorInput, t("tab.rename"), true, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.start_rename(i, title.clone(), w, cx)))
            .child(item!(IconName::CopyPlus, t("tab.duplicate"), true, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.duplicate_tab(i, w, cx)))
            .child(ui::menu_sep())
            .child(item!(IconName::Columns2, t("tab.split_right"), true, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.split_tab(i, Dir::Vertical, w, cx)))
            .child(item!(IconName::Rows2, t("tab.split_down"), true, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.split_tab(i, Dir::Horizontal, w, cx)))
            .child(item!(IconName::ExternalLink, t("tab.move_window"), true, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.move_to_window(i, w, cx)))
            .child(ui::menu_sep())
            .child(item!(IconName::X, t("tab.close"), true, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.close_tab(i, w, cx)))
            .child(item!(IconName::X, t("tab.close_others"), n > 1, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.close_others(i, w, cx)))
            .child(item!(IconName::X, t("tab.close_right"), i + 1 < n, move |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.close_right(i, w, cx)))
            .child(ui::menu_sep())
            .child(item!(IconName::RotateCcw, t("tab.reopen"), !self.closed_tabs.is_empty(), |this: &mut Root, w: &mut Window, cx: &mut Context<Root>| this.reopen_closed(w, cx)));
        gpui::deferred(gpui::anchored().position(pos).snap_to_window_with_margin(px(ui::EDGE_MARGIN)).child(chrome::pop_in("tab-menu-pop", list))).with_priority(3).into_any_element()
    }

    /// Small header at the top-right of a pane, visible while the pointer is over the pane:
    /// split right, split down, close pane — they act on that pane (PRD 5.23).
    pub(crate) fn pane_header(&self, p: &Entity<TermView>, group: gpui::SharedString, cx: &mut Context<Self>) -> AnyElement {
        let btn = |i: usize, ic: IconName, tip: &'static str| ui::icon_button((gpui::SharedString::from(format!("ph-{}", p.entity_id())), i), ic, ui::IconBtn::Md, tip);
        let focus = |this: &mut Root, p: &Entity<TermView>, w: &mut Window, cx: &mut Context<Root>| {
            if let Some(ti) = this.tabs.iter().position(|t| t.layout.leaves().iter().any(|x| x == p)) { this.active = ti; this.leave_settings(); this.tabs[ti].focused = p.clone(); }
            let fh = p.read(cx).focus.clone(); w.focus(&fh, cx);
        };
        let (a, b, d) = (p.clone(), p.clone(), p.clone());
        div().absolute().top(px(4.)).right(px(8.)).p(px(2.)).flex().items_center().justify_end().gap(px(2.)).rounded(px(ui::R_SUB))
            .invisible().group_hover(group.clone(), |s| s.visible()).hover(|s| s.visible())
            .bg(crate::theme::card_bg()).border_1().border_color(crate::theme::divider()).shadow(chrome::soft_shadow(false))
            .child(btn(0, IconName::Columns2, t("ui.split_left_right")).on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, w, cx| { cx.stop_propagation(); focus(this, &a, w, cx); this.split(Some(Dir::Vertical), w, cx); cx.notify(); })))
            .child(btn(1, IconName::Rows2, t("ui.split_top_bottom")).on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, w, cx| { cx.stop_propagation(); focus(this, &b, w, cx); this.split(Some(Dir::Horizontal), w, cx); cx.notify(); })))
            .child(btn(2, IconName::X, t("ui.close_pane")).on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, w, cx| { cx.stop_propagation(); focus(this, &d, w, cx); this.close_pane(w, cx); cx.notify(); })))
            .into_any_element()
    }
}

/// “+”新建菜单中的一项。
#[derive(Clone)]
pub(crate) enum NewItem {
    Local,
    Group { path: String, name: String, depth: usize, count: usize, collapsed: bool },
    Host { index: usize, depth: usize },
    NewHost,
}
