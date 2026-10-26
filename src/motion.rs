//! Shared motion timings (UI audit §c). All UI animations read their durations and the
//! reduced-motion switch from here so the whole app stays consistent.
use std::time::Duration;

/// Popover / menu / palette enter.
pub const ENTER_MS: u64 = 140;
/// Popover / menu / toast exit.
pub const EXIT_MS: u64 = 90;
/// Modal dialog enter.
pub const MODAL_ENTER_MS: u64 = 180;
/// Toast enter.
pub const TOAST_ENTER_MS: u64 = 200;
/// Toast exit.
pub const TOAST_EXIT_MS: u64 = 150;
/// Breathing indicators (connecting dot), one full cycle.
pub const BREATHE_MS: u64 = 1200;
/// Sidebar collapse / expand.
pub const SIDEBAR_MS: u64 = 200;
/// Tab / pane appear.
pub const APPEAR_MS: u64 = 160;
/// Delay before a connecting card is shown, to avoid flashing on fast connects.
pub const PROGRESS_DELAY_MS: u64 = 300;
/// Rise distance for enter animations, in px.
pub const RISE_PX: f32 = 4.0;

/// True when animations should be skipped: `SHELLAX_NO_ANIM` or Settings › Appearance › Reduce motion.
pub fn reduced() -> bool {
    std::env::var_os("SHELLAX_NO_ANIM").is_some() || crate::config::get().appearance.reduce_motion
}

pub fn ms(v: u64) -> Duration { Duration::from_millis(v) }

/// Panel slide-in (left panel, right column, bottom panel).
pub const PANEL_OPEN_MS: u64 = 170;
/// Panel slide-out.
pub const PANEL_CLOSE_MS: u64 = 140;
/// Trailing quiet time before a terminal applies a new size (window / drag resizes).
pub const RESIZE_DEBOUNCE_MS: u64 = 60;

static HOLD_UNTIL: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Freeze terminal sizes for `ms` (panel animations, splitter drags). Terminals keep their
/// grid and apply the final size once, after the hold ends.
pub fn hold(ms: u64) {
    let until = std::time::Instant::now() + Duration::from_millis(ms);
    let mut h = HOLD_UNTIL.lock().unwrap();
    if h.is_none_or(|u| u < until) { *h = Some(until); }
}
/// True while a layout animation or drag is in progress.
pub fn layout_busy() -> bool { HOLD_UNTIL.lock().unwrap().is_some_and(|u| std::time::Instant::now() < u) }

/// Ease-out cubic.
pub fn ease_out(t: f32) -> f32 { 1. - (1. - t).powi(3) }

/// Trailing debounce for a terminal size: returns the size to apply now, if any.
#[derive(Default)]
pub struct SizeGate { pending: Option<((usize, usize), std::time::Instant)>, applied: bool }
impl SizeGate {
    /// `want` = size the current bounds give, `cur` = size the terminal has. The first size is
    /// applied immediately; later changes wait for `RESIZE_DEBOUNCE_MS` of quiet and no hold.
    pub fn poll(&mut self, want: (usize, usize), cur: (usize, usize), busy: bool, now: std::time::Instant) -> Option<(usize, usize)> {
        if !self.applied { self.applied = true; self.pending = None; return Some(want); }
        if want == cur { self.pending = None; return None; }
        match self.pending {
            Some((p, t)) if p == want => {
                if !busy && now.duration_since(t) >= Duration::from_millis(RESIZE_DEBOUNCE_MS) { self.pending = None; Some(want) } else { None }
            }
            _ => { self.pending = Some((want, now)); None }
        }
    }
    /// A size is still waiting to be applied (keep drawing frames until it is).
    pub fn waiting(&self) -> bool { self.pending.is_some() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn size_gate_debounces() {
        let t0 = std::time::Instant::now();
        let mut g = SizeGate::default();
        assert_eq!(g.poll((80, 24), (0, 0), false, t0), Some((80, 24)));
        assert_eq!(g.poll((90, 24), (80, 24), false, t0), None);
        assert!(g.waiting());
        assert_eq!(g.poll((90, 24), (80, 24), false, t0 + Duration::from_millis(30)), None);
        // held: never applied while busy
        assert_eq!(g.poll((90, 24), (80, 24), true, t0 + Duration::from_millis(100)), None);
        assert_eq!(g.poll((90, 24), (80, 24), false, t0 + Duration::from_millis(100)), Some((90, 24)));
        assert!(!g.waiting());
        assert_eq!(g.poll((90, 24), (90, 24), false, t0), None);
    }
}

/// Which edge a panel slides in from.
#[derive(Clone, Copy)]
pub enum Edge { Left, Right, Bottom }

/// Slide a panel in/out inside a clipping box. The box keeps its full size for the whole
/// animation, so nothing next to it is laid out again per frame; only the panel's paint offset
/// moves. Callers add / remove the box (and resize terminals) once, at the start or end.
pub fn slide(outer: gpui::Div, id: impl Into<gpui::ElementId>, el: gpui::AnyElement, dist: f32, edge: Edge, show: bool) -> gpui::AnyElement {
    use gpui::{div, prelude::*, px, AnimationExt};
    let dur = if show { PANEL_OPEN_MS } else { PANEL_CLOSE_MS };
    let inner = match edge { Edge::Bottom => div().relative().w_full(), _ => div().relative().h_full() };
    outer.overflow_hidden().child(inner.child(el).with_animation(id, gpui::Animation::new(ms(dur)).with_easing(ease_out), move |d, t| {
        let off = (if show { 1. - t } else { t }) * dist;
        match edge { Edge::Left => d.left(px(-off)), Edge::Right => d.left(px(off)), Edge::Bottom => d.top(px(off)) }
    })).into_any_element()
}
