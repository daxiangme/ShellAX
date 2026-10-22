//! Tab right-click menu, inline rename, closed-tab history, "move to new window" and the per-pane
//! hover header with split / close controls (PRD 5.23).
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::layout::Dir;

use crate::termview::TermView;
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};
use crate::{chrome, Root, Tab};

const CLOSED_MAX: usize = 10;

impl Root {
    /// Close tab `i` (remembered for "reopen closed tab").
    pub(crate) fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() { return; }
        let node = Self::save_node(&self.tabs[i].layout, cx);
        let name = self.tabs[i].name.clone();
        self.closed_tabs.push((node, name));
        if self.closed_tabs.len() > CLOSED_MAX { self.closed_tabs.remove(0); }
        let tab = self.tabs.remove(i);
        for p in tab.layout.leaves() { self.end_pane(&p, cx); }
        if self.active > i || self.active >= self.tabs.len() { self.active = self.active.saturating_sub(1); }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
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
        gpui::deferred(gpui::anchored().position(pos).snap_to_window_with_margin(px(8.)).child(chrome::pop_in("tab-menu-pop", list))).with_priority(3).into_any_element()
    }

    /// Small header at the top-right of a pane, visible while the pointer is over the pane:
    /// split right, split down, close pane — they act on that pane (PRD 5.23).
    pub(crate) fn pane_header(&self, p: &Entity<TermView>, group: gpui::SharedString, cx: &mut Context<Self>) -> AnyElement {
        let th = cur();
        let btn = |i: usize, ic: IconName, tip: &'static str| div().id((gpui::SharedString::from(format!("ph-{}", p.entity_id())), i)).size(px(22.)).rounded(px(6.)).flex().items_center().justify_center().cursor_pointer()
            .hover(|d| d.bg(c(cur().surface1)))
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip).build(w, cx))
            .child(icon(ic, 13., c(th.overlay1)));
        let focus = |this: &mut Root, p: &Entity<TermView>, w: &mut Window, cx: &mut Context<Root>| {
            if let Some(ti) = this.tabs.iter().position(|t| t.layout.leaves().iter().any(|x| x == p)) { this.active = ti; this.leave_settings(); this.tabs[ti].focused = p.clone(); }
            let fh = p.read(cx).focus.clone(); w.focus(&fh, cx);
        };
        let (a, b, d) = (p.clone(), p.clone(), p.clone());
        div().absolute().top(px(4.)).right(px(8.)).w(px(74.)).h(px(28.)).flex().items_center().justify_end().gap(px(2.)).rounded(px(6.))
            .invisible().group_hover(group.clone(), |s| s.visible()).hover(|s| s.visible())
            .bg(c(th.mantle).opacity(0.92)).border_1().border_color(c(th.surface1).opacity(0.7)).shadow(chrome::soft_shadow(false))
            .child(btn(0, IconName::Columns2, t("ui.split_left_right")).on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, w, cx| { cx.stop_propagation(); focus(this, &a, w, cx); this.split(Some(Dir::Vertical), w, cx); cx.notify(); })))
            .child(btn(1, IconName::Rows2, t("ui.split_top_bottom")).on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, w, cx| { cx.stop_propagation(); focus(this, &b, w, cx); this.split(Some(Dir::Horizontal), w, cx); cx.notify(); })))
            .child(btn(2, IconName::X, t("ui.close_pane")).on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, w, cx| { cx.stop_propagation(); focus(this, &d, w, cx); this.close_pane(w, cx); cx.notify(); })))
            .into_any_element()
    }
}
