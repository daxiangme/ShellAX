//! Command palette (Ctrl+Shift+P): fuzzy search over app actions and hosts.
use ssh_term_proto::i18n::{t, tf};
use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, FontWeight, KeyDownEvent, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};

use crate::actions::{fuzzy, Action};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

#[derive(Clone)]
pub enum Item { Act(Action), Host(usize, String, String) }

pub enum PaletteEvent { Run(Item), Close }

pub struct Palette {
    query: Entity<InputState>,
    hosts: Vec<(String, String)>,
    selected: usize,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    pub fn new(hosts: Vec<(String, String)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.type_a_command_or_host")));
        let subs = vec![cx.subscribe_in(&query, window, |this: &mut Self, _, ev: &InputEvent, _, cx| {
            match ev { InputEvent::Change => { this.selected = 0; cx.notify(); } InputEvent::PressEnter { .. } => this.run_selected(cx), _ => {} }
        })];
        query.update(cx, |q, cx| q.focus(window, cx));
        Self { query, hosts, selected: 0, _subs: subs }
    }


    fn items(&self, cx: &Context<Self>) -> Vec<(i32, Item)> {
        let q = self.query.read(cx).value().to_string();
        let mut v: Vec<(i32, Item)> = Action::ALL.iter().filter_map(|a| {
            let i = a.info();
            fuzzy(&q, &format!("{} {} {}", i.label, i.group, i.id)).map(|s| (s, Item::Act(*a)))
        }).collect();
        for (idx, (name, detail)) in self.hosts.iter().enumerate() {
            if let Some(s) = fuzzy(&q, &tf("palette.connect_search", &[("name", name), ("detail", detail)])) { v.push((s - 2, Item::Host(idx, name.clone(), detail.clone()))); }
        }
        if !q.trim().is_empty() { v.sort_by_key(|x| std::cmp::Reverse(x.0)); }
        v
    }

    fn run_selected(&mut self, cx: &mut Context<Self>) {
        if let Some((_, it)) = self.items(cx).into_iter().nth(self.selected) { cx.emit(PaletteEvent::Run(it)); }
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        let items = self.items(cx);
        if self.selected >= items.len().max(1) { self.selected = items.len().saturating_sub(1); }
        let n = items.len();
        let mut list = div().id("palette-list").flex().flex_col().p_2().max_h(px(420.)).overflow_y_scroll();
        let mut last_group = "";
        for (i, (_, it)) in items.iter().enumerate().take(60) {
            let (ic, label, group, binding) = match it {
                Item::Act(a) => { let inf = a.info(); (inf.icon, inf.label.to_string(), inf.group, a.binding()) }
                Item::Host(_, name, detail) => (IconName::Server, tf("palette.connect", &[("name", name), ("detail", detail)]), t("ui.host"), String::new()),
            };
            if group != last_group && self.query.read(cx).value().is_empty() {
                list = list.child(ui::menu_label(group).flex_shrink_0());
                last_group = group;
            }
            let sel = i == self.selected;
            // 选中行：menu_sel_bg + 常规文字
            list = list.child(div().id(("pal", i)).flex_shrink_0().h(px(ui::H_LARGE)).px(px(10.)).rounded(px(ui::R_MD)).flex().items_center().gap_3().cursor_pointer()
                .bg(if sel { crate::theme::menu_sel_bg() } else { gpui::transparent_black() })
                .when(!sel, |d| d.hover(|d| d.bg(crate::theme::menu_sel_bg())))
                .on_click(cx.listener(move |this, _, _w, cx| { this.selected = i; this.run_selected(cx); }))
                .child(icon(ic, ui::IC_MD, c(th.overlay1)))
                .child(div().flex_1().text_size(px(ui::fs_body())).text_color(c(th.text)).child(label))
                .when(!self.query.read(cx).value().is_empty(), |d| d.child(div().text_size(px(ui::fs_small())).text_color(c(th.overlay0)).child(group)))
                .when(!binding.is_empty(), |d| d.child(ui::kbd(&binding))));
        }
        if n == 0 { list = list.child(div().p_4().text_size(px(ui::fs_body())).text_color(c(th.overlay1)).child(t("ui.no_matching_commands"))); }
        div().absolute().top_0().left_0().size_full().occlude().flex().flex_col().items_center().pt(px(96.)).bg(gpui::black().opacity(0.12))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|_, _, _w, cx| cx.emit(PaletteEvent::Close)))
            .child(crate::chrome::pop_in("palette-pop", ui::popover().w(px(640.)).occlude().flex().flex_col().overflow_hidden()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                    let n = this.items(cx).len();
                    match ev.keystroke.key.as_str() {
                        "down" => { if n > 0 { this.selected = (this.selected + 1) % n; } }
                        "up" => { if n > 0 { this.selected = (this.selected + n - 1) % n; } }
                        "escape" => cx.emit(PaletteEvent::Close),
                        _ => return,
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(div().p_3().border_b_1().border_color(crate::theme::divider()).child(ui::search_field(&self.query, true).cleanable(false)))
                .child(list)
                .child(div().h(px(ui::H_DEFAULT)).px_3().flex().items_center().gap_3().border_t_1().border_color(crate::theme::divider())
                    .text_size(px(ui::fs_small())).text_color(c(th.overlay0))
                    .child(tf("palette.count", &[("n", &n)])).child(div().flex_1()).child(t("ui.select")).child(t("ui.enter_run")).child(t("ui.esc_close"))
                    .font_weight(FontWeight::NORMAL))))
    }
}
