//! Small shared widgets drawn with plain GPUI divs + Lucide icons (gpui-kit-assets, ISC).
use gpui::{div, prelude::*, px, svg, AnyElement, Div, ElementId, FontWeight, Hsla, SharedString, Stateful};
pub use gpui_kit_assets::IconName;

use crate::theme::{c, cur};

pub fn icon(name: IconName, size: f32, color: Hsla) -> impl IntoElement {
    svg().path(name.path()).size(px(size)).flex_shrink_0().text_color(color)
}

/// Standard button. `kind`: 0 = secondary, 1 = primary, 2 = danger, 3 = ghost.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: u8) -> Stateful<Div> {
    let th = cur();
    let (bg, fg, border) = match kind {
        1 => (c(th.accent), c(th.base), c(th.accent)),
        2 => (c(th.error).opacity(0.15), c(th.error), c(th.error).opacity(0.4)),
        3 => (gpui::transparent_black(), c(th.subtext), gpui::transparent_black()),
        _ => (c(th.surface0), c(th.text), c(th.surface1)),
    };
    div().id(id).h(px(28.)).px_3().rounded_md().flex().items_center().justify_center().gap_1().cursor_pointer()
        .bg(bg).text_color(fg).border_1().border_color(border).text_size(px(12.)).font_weight(FontWeight::MEDIUM)
        .hover(move |s| s.opacity(0.85))
        .child(label.into())
}

/// Segmented choice control. Returns the row; each option gets an id `(base, index)`.
pub fn segmented(base: &'static str, options: &[(String, String)], selected: &str, on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> impl IntoElement {
    let th = cur();
    let mut row = div().flex().items_center().p(px(2.)).gap(px(2.)).rounded_md().bg(c(th.mantle)).border_1().border_color(c(th.surface0));
    for (i, (val, label)) in options.iter().enumerate() {
        let active = val == selected;
        let v = val.clone();
        let on = on.clone();
        row = row.child(
            div().id((base, i)).h(px(24.)).px(px(10.)).rounded(px(5.)).flex().items_center().cursor_pointer().text_size(px(12.))
                .bg(if active { c(th.surface1) } else { gpui::transparent_black() })
                .text_color(if active { c(th.text) } else { c(th.overlay1) })
                .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                .hover(|s| s.text_color(c(cur().text)))
                .on_click(move |_, w, cx| on(v.clone(), w, cx))
                .child(label.clone()),
        );
    }
    row
}

/// Small pill tag.
pub fn tag(label: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div().px(px(6.)).py(px(1.)).rounded(px(4.)).bg(color.opacity(0.16)).text_color(color).text_size(px(10.5)).font_weight(FontWeight::MEDIUM).child(label.into())
}


/// Keyboard shortcut chip, e.g. "ctrl-shift-p" -> Ctrl Shift P.
pub fn kbd(binding: &str) -> AnyElement {
    let th = cur();
    let mut row = div().flex().gap(px(3.));
    for part in binding.split('-').filter(|p| !p.is_empty()) {
        let label = match part { "ctrl" => "Ctrl".to_string(), "shift" => "Shift".into(), "alt" => "Alt".into(), "cmd" | "super" => "⌘".into(),
            "comma" | "," => ",".into(), "tab" => "Tab".into(), "escape" => "Esc".into(), p => p.to_uppercase() };
        row = row.child(div().px(px(5.)).h(px(18.)).flex().items_center().rounded(px(4.)).bg(c(th.surface0)).border_1().border_color(c(th.surface1))
            .text_size(px(10.5)).text_color(c(th.subtext)).child(label));
    }
    row.into_any_element()
}

/// Modal backdrop + centered card.
pub fn modal(card: impl IntoElement) -> Div {
    // occlude: the backdrop swallows mouse/scroll events so the terminal below stays inert
    div().absolute().top_0().left_0().size_full().occlude().flex().items_center().justify_center().bg(gpui::black().opacity(0.5)).child(card)
}

pub fn card(w: f32) -> Div {
    let th = cur();
    div().w(px(w)).occlude().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).rounded_xl().bg(c(th.base)).border_1().border_color(c(th.surface1)).shadow_lg().flex().flex_col().overflow_hidden()
}
