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
