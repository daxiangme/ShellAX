//! Small shared widgets drawn with plain GPUI divs + Lucide icons (gpui-kit-assets, ISC).
use gpui::{div, prelude::*, px, svg, AnyElement, Div, ElementId, FontWeight, Hsla, SharedString, Stateful};
pub use gpui_kit_assets::IconName;

use crate::theme::{c, cur};

// ---- design tokens (UI audit §b.1) ----
/// Type scale: caption / secondary / body / title / page title.
pub const FS_CAPTION: f32 = 11.;
pub const FS_SECONDARY: f32 = 12.;
pub const FS_BODY: f32 = 13.;
pub const FS_TITLE: f32 = 15.;
pub const FS_PAGE: f32 = 20.;
/// Corner radius scale: tag/kbd, button/input/row, card/popover/dialog.
pub const R_SM: f32 = 4.;
pub const R_MD: f32 = 6.;
pub const R_LG: f32 = 10.;
/// Control height scale: compact (segmented, tab, menu item), default (button, input), large (dialog CTA, search).
pub const H_COMPACT: f32 = 24.;
pub const H_DEFAULT: f32 = 28.;
pub const H_LARGE: f32 = 32.;

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
    div().id(id).h(px(H_DEFAULT)).px_3().rounded(px(R_MD)).flex().items_center().justify_center().gap_1().cursor_pointer()
        .bg(bg).text_color(fg).border_1().border_color(border).text_size(px(FS_SECONDARY)).font_weight(FontWeight::MEDIUM)
        .hover(move |s| s.opacity(0.85))
        // pressed state: one step darker than hover
        .active(move |s| s.opacity(0.7))
        .child(label.into())
}

/// Segmented choice control. Returns the row; each option gets an id `(base, index)`.
pub fn segmented(base: &'static str, options: &[(String, String)], selected: &str, on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> impl IntoElement {
    let th = cur();
    let mut row = div().w_full().flex().flex_wrap().items_center().p(px(2.)).gap(px(2.)).rounded_md().bg(c(th.mantle)).border_1().border_color(c(th.surface0));
    for (i, (val, label)) in options.iter().enumerate() {
        let active = val == selected;
        let v = val.clone();
        let on = on.clone();
        row = row.child(
            div().id((base, i)).h(px(H_COMPACT)).px(px(10.)).rounded(px(R_MD)).flex().items_center().cursor_pointer().text_size(px(FS_SECONDARY))
                .bg(if active { c(th.surface1) } else { gpui::transparent_black() })
                .text_color(if active { c(th.text) } else { c(th.overlay1) })
                .when(active, |d| d.font_weight(FontWeight::MEDIUM))
                .hover(|s| s.text_color(c(cur().text)))
                .active(|s| s.bg(c(cur().surface2)))
                .on_click(move |_, w, cx| on(v.clone(), w, cx))
                .child(label.clone()),
        );
    }
    row
}

/// Grid of selectable cards for long option lists (e.g. themes) where a segmented control would
/// overflow and squeeze the label column. Cards are fixed-width and wrap.
/// `swatch(value)` may return (bg, surface, text, accent) to draw a small theme thumbnail.
pub fn choice_grid(base: &'static str, options: &[(String, String)], selected: &str, swatch: impl Fn(&str) -> Option<[u32; 4]>, on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> impl IntoElement {
    let th = cur();
    let mut grid = div().w(px(308.)).flex_shrink_0().flex().flex_wrap().gap(px(8.));
    for (i, (val, label)) in options.iter().enumerate() {
        let active = val == selected;
        let v = val.clone();
        let on = on.clone();
        // 36×24 thumbnail: window bg, a sidebar strip, a text line and an accent dot
        let thumb = swatch(val).map(|[bg, surf, text, acc]| div().w(px(36.)).h(px(24.)).flex_shrink_0().rounded(px(R_SM)).overflow_hidden().border_1().border_color(c(th.surface2)).bg(c(bg)).flex()
            .child(div().w(px(10.)).h_full().bg(c(surf)))
            .child(div().flex_1().p(px(3.)).flex().flex_col().gap(px(3.))
                .child(div().h(px(2.)).w(px(16.)).rounded_full().bg(c(text)))
                .child(div().size(px(5.)).rounded_full().bg(c(acc)))));
        grid = grid.child(
            div().id((base, i)).w(px(150.)).h(px(40.)).px(px(8.)).rounded(px(R_MD)).flex().items_center().gap(px(8.)).cursor_pointer()
                .border_1().border_color(if active { c(th.accent) } else { c(th.surface1) })
                .bg(if active { c(th.accent).opacity(0.12) } else { c(th.mantle) })
                .hover(|s| s.bg(c(cur().surface0)))
                .active(|s| s.bg(c(cur().surface1)))
                .on_click(move |_, w, cx| on(v.clone(), w, cx))
                .children(thumb)
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(FS_SECONDARY))
                    .text_color(if active { c(th.text) } else { c(th.subtext) }).when(active, |d| d.font_weight(FontWeight::MEDIUM)).child(label.clone()))
                .when(active, |d| d.child(icon(IconName::Check, 13., c(th.accent)))),
        );
    }
    grid
}

/// Small pill tag.
pub fn tag(label: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div().px(px(6.)).py(px(1.)).rounded(px(R_SM)).bg(color.opacity(0.16)).text_color(color).text_size(px(FS_CAPTION)).font_weight(FontWeight::MEDIUM).child(label.into())
}


/// Keyboard shortcut chip, e.g. "ctrl-shift-p" -> Ctrl Shift P.
pub fn kbd(binding: &str) -> AnyElement {
    let th = cur();
    let mut row = div().flex().gap(px(3.));
    for part in binding.split('-').filter(|p| !p.is_empty()) {
        let label = match part { "ctrl" => "Ctrl".to_string(), "shift" => "Shift".into(), "alt" => "Alt".into(), "cmd" | "super" => "⌘".into(),
            "comma" | "," => ",".into(), "tab" => "Tab".into(), "escape" => "Esc".into(), p => p.to_uppercase() };
        row = row.child(div().px(px(5.)).h(px(18.)).flex().items_center().rounded(px(4.)).bg(c(th.surface0)).border_1().border_color(c(th.surface1))
            .text_size(px(FS_CAPTION)).text_color(c(th.subtext)).child(label));
    }
    row.into_any_element()
}

/// Modal backdrop + centered card.
pub fn modal(card: impl IntoElement + 'static) -> Div {
    // occlude: the backdrop swallows mouse/scroll events so the terminal below stays inert
    div().absolute().top_0().left_0().size_full().occlude().flex().items_center().justify_center().bg(gpui::black().opacity(0.28)).p(px(16.)).child(crate::chrome::pop_in("modal-pop", card))
}

/// Translucent popover surface (menus, palette): radius 10, hairline border, soft layered shadow.
pub fn popover() -> Div {
    let th = cur();
    div().rounded(px(10.)).bg(c(th.base)).border_1().border_color(c(th.surface2).opacity(0.45)).shadow(crate::chrome::soft_shadow(true))
}

pub fn card(w: f32) -> Div {
    let th = cur();
    div().w(px(w)).occlude().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).rounded(px(10.)).bg(c(th.base).opacity(0.97)).border_1().border_color(c(th.surface2).opacity(0.45)).shadow(crate::chrome::soft_shadow(true)).flex().flex_col().overflow_hidden()
}

/// Finder tag dots (PRD 5.23), overlapping slightly like Finder's list view.
/// Single Finder-style tag dot (the first tag of a host / group), or nothing.
pub fn tag_dot(tag: Option<&String>) -> Option<AnyElement> {
    tag.map(|t| div().size(px(7.)).flex_shrink_0().rounded_full().bg(c(crate::config::get().tags.rgb(t))).into_any_element())
}


/// Row of the seven color tags; selected ones get a check. `names`: (color, display name).
pub fn tag_picker(base: &'static str, selected: &[String], names: &[(String, String)], on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> AnyElement {
    let mut row = div().flex().flex_wrap().items_center().gap(px(6.));
    for (i, (color, name)) in names.iter().enumerate() {
        let on = on.clone();
        let col = color.clone();
        let sel = selected.iter().any(|t| t == color);
        let tip = name.clone();
        row = row.child(div().id((base, i)).size(px(18.)).rounded_full().cursor_pointer().flex().items_center().justify_center()
            .bg(c(crate::config::get().tags.rgb(color)))
            .border_2().border_color(if sel { c(cur().text).opacity(0.55) } else { gpui::transparent_black() })
            .hover(|d| d.opacity(0.85))
            .tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, w, cx| on(col.clone(), w, cx))
            .when(sel, |d| d.child(icon(IconName::Check, 11., gpui::white()))));
    }
    row.into_any_element()
}

/// Multi-select tag chips (dot + name), wrapping onto more lines; selected chips are highlighted.
pub fn tag_chips(base: &'static str, selected: &[String], names: &[(String, String)], on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> AnyElement {
    let th = cur();
    let mut row = div().w_full().flex().flex_wrap().gap(px(6.));
    for (i, (id, name)) in names.iter().enumerate() {
        let on = on.clone();
        let idc = id.clone();
        let sel = selected.iter().any(|t| t == id);
        let col = c(crate::config::get().tags.rgb(id));
        row = row.child(div().id((base, i)).h(px(24.)).px(px(9.)).rounded_full().flex().items_center().gap(px(6.)).cursor_pointer()
            .border_1().border_color(if sel { col } else { c(th.surface1) })
            .bg(if sel { col.opacity(0.16) } else { gpui::transparent_black() })
            .hover(|d| d.bg(c(cur().surface0)))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, w, cx| on(idc.clone(), w, cx))
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(col))
            .child(div().text_size(px(12.)).whitespace_nowrap().text_color(c(th.text)).child(name.clone()))
            .when(sel, |d| d.child(icon(IconName::Check, 11., col))));
    }
    row.into_any_element()
}

/// Menu row (context menus, popover menus): icon + label (+ optional right text), 26px.
pub fn menu_item(id: impl Into<ElementId>, ic: Option<IconName>, label: impl Into<SharedString>, enabled: bool, danger: bool) -> Stateful<Div> {
    let th = cur();
    let col = if !enabled { c(th.overlay0) } else if danger { c(th.error) } else { c(th.text) };
    // 28px row, 16px icon + 8px gap, 13px label; danger items red (icon + text)
    div().id(id).h(px(H_DEFAULT)).min_w(px(200.)).px(px(10.)).rounded(px(R_MD)).flex().items_center().gap_2().text_size(px(FS_BODY)).text_color(col)
        .when(enabled, |d| d.cursor_pointer().hover(move |d| if danger { d.bg(c(cur().error)).text_color(gpui::white()) } else { d.bg(c(cur().accent)).text_color(gpui::white()) }).active(|d| d.opacity(0.85)))
        .child(match ic { Some(i) => icon(i, 16., if danger { c(th.error) } else { c(th.overlay1) }).into_any_element(), None => div().w(px(16.)).into_any_element() })
        .child(div().flex_1().whitespace_nowrap().child(label.into()))
}

pub fn menu_sep() -> Div { div().mx(px(8.)).my(px(4.)).h(px(1.)).bg(c(cur().surface1).opacity(0.6)) }
