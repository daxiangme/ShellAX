//! Command palette (Ctrl+Shift+P): fuzzy search over app actions and hosts.
use gpui::{
    div, prelude::*, px, Context, Entity, EventEmitter, FontWeight, KeyDownEvent, Subscription,
    Window,
};
use gpui_component::input::{InputEvent, InputState};
use ssh_term_proto::i18n::{t, tf};

use crate::actions::{fuzzy, Action};
use crate::theme::{c, cur};
use crate::ui::{self, icon, IconName};

#[derive(Clone)]
pub enum Item {
    Act(Action),
    Host(usize, String, String),
}

pub enum PaletteEvent {
    Run(Item),
    Close,
}

pub struct Palette {
    query: Entity<InputState>,
    hosts: Vec<(String, String)>,
    selected: usize,
    scroll: gpui::UniformListScrollHandle,
    cached: Vec<(i32, Item)>,
    cached_query: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    pub fn new(hosts: Vec<(String, String)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder(t("ui.type_a_command_or_host")));
        let subs = vec![cx.subscribe_in(
            &query,
            window,
            |this: &mut Self, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => {
                    this.selected = 0;
                    this.cached_query = None;
                    this.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.run_selected(cx),
                _ => {}
            },
        )];
        query.update(cx, |q, cx| q.focus(window, cx));
        Self {
            query,
            hosts,
            selected: 0,
            scroll: gpui::UniformListScrollHandle::new(),
            cached: Vec::new(),
            cached_query: None,
            _subs: subs,
        }
    }

    fn ensure_items(&mut self, cx: &gpui::App) {
        let q = self.query.read(cx).value().to_string();
        if self.cached_query.as_deref() == Some(q.as_str()) {
            return;
        }
        let mut v: Vec<(i32, Item)> = Action::ALL
            .iter()
            .filter(|a| a.shown())
            .filter_map(|a| {
                let i = a.info();
                fuzzy(&q, &format!("{} {} {}", i.label, i.group, i.id)).map(|s| (s, Item::Act(*a)))
            })
            .collect();
        for (idx, (name, detail)) in self.hosts.iter().enumerate() {
            if let Some(s) = fuzzy(
                &q,
                &tf(
                    "palette.connect_search",
                    &[("name", name), ("detail", detail)],
                ),
            ) {
                v.push((s - 2, Item::Host(idx, name.clone(), detail.clone())));
            }
        }
        if !q.trim().is_empty() {
            v.sort_by_key(|x| std::cmp::Reverse(x.0));
        }
        self.cached = v;
        self.cached_query = Some(q);
    }

    fn run_selected(&mut self, cx: &mut Context<Self>) {
        self.ensure_items(cx);
        if let Some((_, it)) = self.cached.get(self.selected).cloned() {
            cx.emit(PaletteEvent::Run(it));
        }
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = cur();
        self.ensure_items(cx);
        if self.selected >= self.cached.len().max(1) {
            self.selected = self.cached.len().saturating_sub(1);
        }
        let n = self.cached.len();
        let selected = self.selected;
        let query_non_empty = !self.query.read(cx).value().is_empty();
        let items_snapshot = std::rc::Rc::new(self.cached.clone());
        let entity = cx.entity().downgrade();

        let list = if n == 0 {
            ui::empty_state(
                IconName::Search,
                t("ui.no_matching_commands"),
                None,
                vec![],
                true,
            )
            .py(px(ui::SP_6))
            .into_any_element()
        } else {
            let row_h = ui::H_LARGE + 6.;
            let list_h = ((n as f32) * row_h + 16.).min(420.);
            div()
                .h(px(list_h))
                .p(px(ui::SP_2))
                .flex()
                .flex_col()
                .child(ui::virtual_list(
                    "palette-list",
                    n,
                    &self.scroll,
                    move |range, _w, _cx| {
                        let th = cur();
                        let mut rows = Vec::with_capacity(range.len());
                        for i in range {
                            let (_, it) = &items_snapshot[i];
                            let (ic, label, group, binding) = match it {
                                Item::Act(a) => {
                                    let inf = a.info();
                                    (inf.icon, inf.label.to_string(), inf.group, a.binding())
                                }
                                Item::Host(_, name, detail) => (
                                    IconName::Server,
                                    tf("palette.connect", &[("name", name), ("detail", detail)]),
                                    t("ui.host"),
                                    String::new(),
                                ),
                            };
                            let sel = i == selected;
                            let ent = entity.clone();
                            rows.push(
                                div()
                                    .id(("pal", i))
                                    .relative()
                                    .w_full()
                                    .h(px(row_h))
                                    .px(px(10.))
                                    .rounded(px(ui::R_MD))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .cursor_pointer()
                                    .bg(if sel {
                                        crate::theme::selected_bg()
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .when(!sel, |d| d.hover(|d| d.bg(crate::theme::hover_bg())))
                                    .on_click(move |_, _w, cx| {
                                        if let Some(e) = ent.upgrade() {
                                            e.update(cx, |this, cx| {
                                                this.selected = i;
                                                this.run_selected(cx);
                                            });
                                        }
                                    })
                                    .when(sel, |d| {
                                        d.child(
                                            div()
                                                .absolute()
                                                .left(px(0.))
                                                .top(px(8.))
                                                .bottom(px(8.))
                                                .w(px(2.))
                                                .rounded_full()
                                                .bg(c(th.accent)),
                                        )
                                    })
                                    .child(
                                        div()
                                            .size(px(22.))
                                            .flex_shrink_0()
                                            .rounded(px(ui::R_SM))
                                            .bg(if sel {
                                                crate::theme::accent_bg()
                                            } else {
                                                crate::theme::sub_bg()
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(icon(
                                                ic,
                                                ui::IC_SM,
                                                if sel { c(th.accent) } else { c(th.overlay1) },
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .text_size(px(ui::fs_body()))
                                            .when(sel, |d| d.font_weight(FontWeight::MEDIUM))
                                            .text_color(c(th.text))
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(px(ui::fs_caption()))
                                            .text_color(if sel {
                                                c(th.subtext)
                                            } else {
                                                c(th.overlay0)
                                            })
                                            .when(!query_non_empty && !sel && i > 0, |d| {
                                                d.opacity(0.82)
                                            })
                                            .child(group),
                                    )
                                    .when(!binding.is_empty(), |d| d.child(ui::kbd(&binding))),
                            );
                        }
                        rows
                    },
                ))
                .into_any_element()
        };
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(88.))
            .bg(crate::theme::modal_scrim_bg())
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|_, _, _w, cx| cx.emit(PaletteEvent::Close)),
            )
            .child(crate::chrome::pop_in(
                "palette-pop",
                ui::popover()
                    .w(px(640.))
                    .max_w(px(640.))
                    .rounded(px(ui::R_XL))
                    .occlude()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                        this.ensure_items(cx);
                        let n = this.cached.len();
                        let ctrl = ev.keystroke.modifiers.control
                            && !ev.keystroke.modifiers.alt
                            && !ev.keystroke.modifiers.platform;
                        match ev.keystroke.key.as_str() {
                            "down" => {
                                if n > 0 {
                                    this.selected = (this.selected + 1) % n;
                                    this.scroll
                                        .scroll_to_item(this.selected, gpui::ScrollStrategy::Nearest);
                                }
                            }
                            "n" if ctrl => {
                                if n > 0 {
                                    this.selected = (this.selected + 1) % n;
                                    this.scroll
                                        .scroll_to_item(this.selected, gpui::ScrollStrategy::Nearest);
                                }
                            }
                            "up" => {
                                if n > 0 {
                                    this.selected = (this.selected + n - 1) % n;
                                    this.scroll
                                        .scroll_to_item(this.selected, gpui::ScrollStrategy::Nearest);
                                }
                            }
                            "p" if ctrl => {
                                if n > 0 {
                                    this.selected = (this.selected + n - 1) % n;
                                    this.scroll
                                        .scroll_to_item(this.selected, gpui::ScrollStrategy::Nearest);
                                }
                            }
                            "escape" => cx.emit(PaletteEvent::Close),
                            _ => return,
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .h(px(48.))
                            .px(px(ui::SP_4))
                            .flex()
                            .items_center()
                            .gap(px(ui::SP_2))
                            .border_b_1()
                            .border_color(crate::theme::divider())
                            .child(icon(IconName::Search, ui::IC_MD, c(th.accent)))
                            .child(
                                div().flex_1().min_w_0().child(
                                    gpui_component::input::Input::new(&self.query)
                                        .appearance(false)
                                        .w_full()
                                        .text_size(px(ui::fs_title())),
                                ),
                            )
                            .child(
                                div()
                                    .px(px(7.))
                                    .h(px(20.))
                                    .rounded_full()
                                    .bg(crate::theme::sub_bg())
                                    .border_1()
                                    .border_color(crate::theme::divider())
                                    .flex()
                                    .items_center()
                                    .text_size(px(ui::fs_caption()))
                                    .text_color(c(th.subtext))
                                    .child(tf("palette.count", &[("n", &n)])),
                            ),
                    )
                    .child(list)
                    .child(
                        div()
                            .h(px(34.))
                            .px(px(ui::SP_4))
                            .bg(crate::theme::sub_bg())
                            .flex()
                            .items_center()
                            .gap(px(ui::SP_4))
                            .border_t_1()
                            .border_color(crate::theme::divider())
                            .text_size(px(ui::fs_caption()))
                            .text_color(c(th.subtext))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(ui::kbd("up-down"))
                                    .child(t("ui.select")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(ui::kbd("enter"))
                                    .child(t("ui.enter_run")),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(ui::kbd("escape"))
                                    .child(t("ui.esc_close")),
                            ),
                    ),
            ))
    }
}
