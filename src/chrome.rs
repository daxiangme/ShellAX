//! macOS-style window chrome (PRD 5.22): title bar merged with the tab row, traffic-light
//! buttons (native on macOS, drawn elsewhere), window drag / zoom / resize for client decorations.
use gpui::{div, prelude::*, px, AnimationExt, AnyElement, Bounds, Pixels, ResizeEdge, Window, WindowBounds, WindowDecorations, WindowOptions};

use crate::theme::{c, cur, Appearance};

/// Native traffic lights (macOS draws them into the transparent title bar).
pub const NATIVE_LIGHTS: bool = cfg!(target_os = "macos");
/// Width reserved at the left of the title row for the traffic lights.
pub const LIGHTS_W: f32 = 76.;
/// Title bar / tab row height (8pt grid).
pub const TITLE_H: f32 = 40.;

pub fn window_options(bounds: Bounds<Pixels>, title: Option<String>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions { title: title.map(Into::into), appears_transparent: true, traffic_light_position: Some(gpui::point(px(14.), px(13.))) }),
        window_decorations: Some(if std::env::var_os("AGENTSHELL_SERVER_DECORATIONS").is_some() { WindowDecorations::Server } else { WindowDecorations::Client }),
        window_background: if cfg!(target_os = "macos") { gpui::WindowBackgroundAppearance::Blurred } else { gpui::WindowBackgroundAppearance::Opaque },
        ..Default::default()
    }
}

/// Window-control style (PRD 5.23): macOS traffic lights on the left, or Windows-style
/// minimize / maximize / close on the right (Windows and Linux). `AGENTSHELL_CONTROLS=mac|win`
/// overrides (screenshots / testing).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Style { Mac, Win }
pub fn style() -> Style {
    match std::env::var("AGENTSHELL_CONTROLS").as_deref() {
        Ok("mac") => Style::Mac,
        Ok("win") => Style::Win,
        _ => if cfg!(target_os = "macos") { Style::Mac } else { Style::Win },
    }
}

/// True when we draw our own title bar controls (no server title bar).
pub fn drawn_controls(window: &Window) -> bool { !NATIVE_LIGHTS && matches!(window.window_decorations(), gpui::Decorations::Client { .. }) }

/// Empty title-bar space that moves the window. On Windows it is a caption hit-test area
/// (WM_NCHITTEST -> HTCAPTION), so dragging, Aero Snap and double-click maximize are native.
pub fn drag_spacer(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div().id(id).flex_1().h_full().min_w(px(24.)).window_control_area(gpui::WindowControlArea::Drag)
}

/// Windows-style caption buttons (46×TITLE_H): minimize, maximize/restore, close (red hover).
/// On Windows they are native hit-test areas (HTMINBUTTON / HTMAXBUTTON / HTCLOSE: Snap Layouts
/// flyout on maximize); elsewhere they are plain click targets.
pub fn window_controls(id: &'static str, window: &Window, on_close: impl Fn(&mut Window, &mut gpui::App) + 'static) -> Option<AnyElement> {
    if style() != Style::Win || !drawn_controls(window) { return None; }
    let th = cur();
    let fg = c(th.text).opacity(if window.is_window_active() { 0.85 } else { 0.45 });
    let maxed = window.is_maximized();
    let btn = |i: usize, area: gpui::WindowControlArea| div().id((id, i)).w(px(46.)).h_full().flex().items_center().justify_center()
        .window_control_area(area).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let line = |w: f32, h: f32| div().w(px(w)).h(px(h)).bg(fg);
    let square = |sz: f32| div().size(px(sz)).border_1().border_color(fg).rounded(px(1.5));
    let max_glyph = if maxed {
        div().relative().size(px(10.)).child(square(8.).absolute().top(px(2.)).left_0().bg(c(th.crust))).child(div().absolute().top_0().left(px(2.)).size(px(8.)).border_t_1().border_r_1().border_color(fg).rounded_tr(px(1.5)))
    } else { div().child(square(10.)) };
    let mut min = btn(0, gpui::WindowControlArea::Min).hover(|d| d.bg(c(cur().surface1).opacity(0.7))).child(line(10., 1.));
    let mut max = btn(1, gpui::WindowControlArea::Max).hover(|d| d.bg(c(cur().surface1).opacity(0.7))).child(max_glyph);
    let gid = gpui::SharedString::from(format!("{id}-close"));
    let mut close = btn(2, gpui::WindowControlArea::Close).group(gid.clone()).hover(|d| d.bg(c(0xc42b1c)))
        .child(div().text_color(fg).group_hover(gid, |s| s.text_color(gpui::white())).child(crate::ui::icon(crate::ui::IconName::X, 15., fg)));
    // Windows routes these clicks through the non-client hit test; elsewhere handle them here
    if !cfg!(windows) {
        min = min.on_click(|_, w, _| w.minimize_window());
        max = max.on_click(|_, w, _| w.zoom_window());
        close = close.on_click(move |_, w, cx| on_close(w, cx));
    } else { let _ = on_close; }
    Some(div().flex().h_full().flex_shrink_0().child(min).child(max).child(close).into_any_element())
}

/// Drawn traffic lights (Linux / Windows); glyphs appear while hovered. On macOS this is just
/// the space the native buttons occupy.
pub fn traffic_lights(id: &'static str, window: &Window, on_close: impl Fn(&mut Window, &mut gpui::App) + 'static) -> AnyElement {
    if !drawn_controls(window) || style() != Style::Mac { return div().w(px(if NATIVE_LIGHTS { LIGHTS_W } else { 8. })).flex_shrink_0().into_any_element(); }
    let active = window.is_window_active();
    let grey = if cur().appearance == Appearance::Dark { 0x4a4a4d } else { 0xd0d0d3 };
    let group = gpui::SharedString::from(format!("{id}-tl"));
    let dot = |i: usize, col: u32, glyph: &'static str| {
        let g = group.clone();
        div().id((id, i)).size(px(12.)).rounded_full().flex().items_center().justify_center()
            .bg(c(if active { col } else { grey })).border_1().border_color(gpui::black().opacity(0.12))
            .child(div().invisible().group_hover(g, |s| s.visible()).text_size(px(9.)).line_height(px(9.)).font_weight(gpui::FontWeight::BOLD).text_color(gpui::black().opacity(0.55)).child(glyph))
    };
    div().id(id).group(group.clone()).w(px(LIGHTS_W)).flex_shrink_0().h_full().pl(px(14.)).flex().items_center().gap(px(8.))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(dot(0, 0xff5f57, "×").on_click(move |_, w, cx| on_close(w, cx)))
        .child(dot(1, 0xfebc2e, "−").on_click(|_, w, _| w.minimize_window()))
        .child(dot(2, 0x28c840, "+").on_click(|_, w, _| w.zoom_window()))
        .into_any_element()
}

/// Make an element a window drag region (title bar): drag moves, double-click zooms.
pub fn drag_region<E: InteractiveElement + StatefulInteractiveElement>(el: E) -> E {
    el.on_mouse_down(gpui::MouseButton::Left, |ev, w, _| { if ev.click_count >= 2 { w.titlebar_double_click(); } else { w.start_window_move(); } })
}

/// Invisible resize handles along the window edges (client decorations on Linux).
/// Not used on Windows: there the frameless window answers WM_NCHITTEST with the resize borders
/// itself (gpui-windows), which keeps native Aero Snap / edge-resize behaviour.
pub fn resize_edges(window: &Window) -> Option<AnyElement> {
    if cfg!(windows) || !drawn_controls(window) || window.is_maximized() { return None; }
    let e = |edge: ResizeEdge| div().absolute().on_mouse_down(gpui::MouseButton::Left, move |_, w, cx| { cx.stop_propagation(); w.start_window_resize(edge); });
    const T: f32 = 5.;
    Some(div().absolute().top_0().left_0().size_full()
        .child(e(ResizeEdge::Top).top_0().left(px(T)).right(px(T)).h(px(T)).cursor_ns_resize())
        .child(e(ResizeEdge::Bottom).bottom_0().left(px(T)).right(px(T)).h(px(T)).cursor_ns_resize())
        .child(e(ResizeEdge::Left).left_0().top(px(T)).bottom(px(T)).w(px(T)).cursor_ew_resize())
        .child(e(ResizeEdge::Right).right_0().top(px(T)).bottom(px(T)).w(px(T)).cursor_ew_resize())
        .child(e(ResizeEdge::TopLeft).top_0().left_0().size(px(T)).cursor_nwse_resize())
        .child(e(ResizeEdge::TopRight).top_0().right_0().size(px(T)).cursor_nesw_resize())
        .child(e(ResizeEdge::BottomLeft).bottom_0().left_0().size(px(T)).cursor_nesw_resize())
        .child(e(ResizeEdge::BottomRight).bottom_0().right_0().size(px(T)).cursor_nwse_resize())
        .into_any_element())
}

/// Soft layered shadow (contact + ambient), PRD 5.22.
pub fn soft_shadow(strong: bool) -> Vec<gpui::BoxShadow> {
    let k = if cur().appearance == Appearance::Dark { 2.0 } else { 1.0 };
    let sh = |a: f32, y: f32, blur: f32| gpui::BoxShadow { color: gpui::black().opacity(a * k), offset: gpui::point(px(0.), px(y)), blur_radius: px(blur), spread_radius: px(0.), inset: false };
    if strong { vec![sh(0.06, 1., 2.), sh(0.10, 8., 24.), sh(0.08, 24., 48.)] } else { vec![sh(0.05, 1., 2.), sh(0.08, 4., 12.)] }
}

/// Fade + rise-in for popups (GPUI divs have no scale transform; 6px rise + fade instead).
pub fn pop_in(id: impl Into<gpui::ElementId>, el: impl IntoElement + 'static) -> AnyElement {
    if std::env::var_os("AGENTSHELL_NO_ANIM").is_some() { return el.into_any_element(); }
    div().child(el).with_animation(id, gpui::Animation::new(std::time::Duration::from_millis(160)).with_easing(gpui::ease_out_quint()),
        |d, t| d.opacity(t).mt(px((1. - t) * 6.))).into_any_element()
}
