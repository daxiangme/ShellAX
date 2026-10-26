//! Searchable dropdown of saved SSH hosts ("" = direct / local), used by connection dialogs.
use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, Window};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::i18n::t;

use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

pub struct HostChanged;

pub struct HostPicker {
    pub value: String,
    hosts: Vec<String>,
    open: bool,
    search: Entity<InputState>,
    id: &'static str,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<HostChanged> for HostPicker {}

/// Hosts whose name contains `q` (case-insensitive), direct option first when `q` is empty.
pub fn filter_hosts(hosts: &[String], q: &str) -> Vec<String> {
    let q = q.trim().to_lowercase();
    let mut v = if q.is_empty() { vec![String::new()] } else { vec![] };
    v.extend(hosts.iter().filter(|h| q.is_empty() || h.to_lowercase().contains(&q)).cloned());
    v
}

impl HostPicker {
    pub fn new(id: &'static str, value: String, hosts: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t("kv.search_hosts")));
        let subs = vec![cx.subscribe_in(&search, window, |this: &mut Self, st, ev: &InputEvent, _w, cx| {
            match ev {
                InputEvent::PressEnter { .. } => { let q = st.read(cx).value().to_string(); if let Some(h) = filter_hosts(&this.hosts, &q).into_iter().next() { this.pick(h, cx); } }
                _ => cx.notify(),
            }
        })];
        Self { value, hosts, open: false, search, id, _subs: subs }
    }
    fn pick(&mut self, h: String, cx: &mut Context<Self>) { self.value = h; self.open = false; cx.emit(HostChanged); cx.notify(); }
}

impl Render for HostPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let label = if self.value.is_empty() { t("db.via_direct").to_string() } else { self.value.clone() };
        let mut root = div().relative().w_full()
            .child(div().id(self.id).h(px(28.)).px_2().rounded(px(6.)).flex().items_center().gap_2().cursor_pointer().bg(c(th.mantle)).border_1().border_color(c(th.surface1))
                .on_click(cx.listener(|this, _, w, cx| { this.open = !this.open; if this.open { this.search.update(cx, |s, cx| { s.set_value("", w, cx); s.focus(w, cx); }); } cx.notify(); }))
                .child(icon(if self.value.is_empty() { IconName::Plug } else { IconName::Waypoints }, 13., c(th.overlay1)))
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(12.)).child(label))
                .child(icon(IconName::ChevronDown, 12., c(th.overlay0))));
        if self.open {
            let q = self.search.read(cx).value().to_string();
            let m = ui::popover().absolute().top(px(32.)).left_0().w_full().p_1().flex().flex_col().gap_1()
                .child(ui::search_field(&self.search, false));
            let mut list = div().id("hp-list").max_h(px(220.)).overflow_y_scroll().flex().flex_col();
            let opts = filter_hosts(&self.hosts, &q);
            if opts.is_empty() { list = list.child(div().p_2().text_size(px(12.)).text_color(c(th.overlay0)).child(t("kv.no_match"))); }
            for (i, h) in opts.into_iter().enumerate() {
                let lbl = if h.is_empty() { t("db.via_direct").to_string() } else { h.clone() };
                let sel = h == self.value;
                list = list.child(ui::menu_item(("hp-o", i), None, lbl, true, false).when(sel, |d| d.bg(c(th.surface0))).on_click(cx.listener(move |this, _, _w, cx| this.pick(h.clone(), cx))));
            }
            root = root.child(gpui::deferred(m.child(list)).with_priority(3));
            let _ = window;
        }
        root
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn filters() {
        let h = vec!["web-1".to_string(), "DB".to_string()];
        assert_eq!(super::filter_hosts(&h, ""), vec!["", "web-1", "DB"]);
        assert_eq!(super::filter_hosts(&h, "db"), vec!["DB"]);
    }
}
