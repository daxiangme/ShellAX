//! Shared spacing for the main content card. Every module view (terminal, SFTP, database,
//! Redis, containers, documents, settings, empty states) sits in the same card; views must not
//! add their own outer padding or bottom status strips — pane status goes to the window status bar.
use gpui::{div, prelude::*, px, Div};

/// Height of a tab pill inside the title/tab row.
pub const TAB_PILL_H: f32 = 28.;
/// Gap between the tab row's top edge and the tab pills; the card keeps the same gap below the
/// tab row (implicitly, the row's lower half) and above the status bar.
pub const EDGE_GAP: f32 = (crate::chrome::TAB_H - TAB_PILL_H) / 2.;
/// Horizontal gap between cards (sidebar card, content card, right dock, icon strip).
pub const CARD_GAP: f32 = 6.;
/// Inner padding of the content card on all sides, and the gap between sub-cards inside it.
pub const CARD_PAD: f32 = 6.;
/// Corner radius of the content card and sub-cards.
pub const CARD_RADIUS: f32 = 10.;
pub const SUB_RADIUS: f32 = 8.;

/// The content card: rounded, card background, `CARD_PAD` on every side. The padding also keeps
/// child backgrounds off the rounded corners (GPUI clips to the rectangle only).
pub fn content_card(id: &'static str) -> gpui::Stateful<Div> {
    div().id(id).flex_1().min_w_0().min_h_0().flex().flex_col().rounded(px(CARD_RADIUS)).overflow_hidden()
        .bg(crate::theme::c(crate::theme::scheme().bg)).p(px(CARD_PAD))
}

/// A sub-card inside the content card (SQL editor / results …).
pub fn sub_card() -> Div {
    let th = crate::theme::cur();
    div().rounded(px(SUB_RADIUS)).border_1().border_color(crate::theme::c(th.surface0)).overflow_hidden().bg(crate::theme::c(th.base))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spacing_rules() {
        // gap above the card equals the gap above the tab pills, and the status-bar gap matches it
        assert_eq!(EDGE_GAP, 4.);
        assert!(EDGE_GAP + TAB_PILL_H * 1. + EDGE_GAP == crate::chrome::TAB_H);
        // inner padding equals the gap between sub-cards, and fits inside the corner radius
        assert_eq!(CARD_PAD, CARD_GAP);
        assert!(CARD_PAD * 2. >= CARD_RADIUS && SUB_RADIUS <= CARD_RADIUS);
    }
}
