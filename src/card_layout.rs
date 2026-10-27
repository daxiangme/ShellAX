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
pub const CARD_RADIUS: f32 = crate::ui::R_LG;
pub const SUB_RADIUS: f32 = crate::ui::R_SUB;

/// 岛：所有面板容器（左工具岛、内容岛、右侧卡片、底部面板）的唯一生成函数：圆角、卡片底色、无边框、纵向排列。
pub fn island(id: impl Into<gpui::ElementId>) -> gpui::Stateful<Div> {
    div().id(id).flex().flex_col().rounded(px(CARD_RADIUS)).overflow_hidden().bg(crate::theme::card_bg())
}

/// The content card: an island with `CARD_PAD` on every side. The padding also keeps
/// child backgrounds off the rounded corners (GPUI clips to the rectangle only).
pub fn content_card(id: &'static str) -> gpui::Stateful<Div> {
    island(id).flex_1().min_w_0().min_h_0().p(px(CARD_PAD))
}

/// 子卡片内容区的内边距（输入、选项、输出卡片共用）；标题工具栏不加，保证分隔线通栏。
pub const SUB_PAD: f32 = crate::ui::SP_2;

/// A sub-card inside the content card (SQL editor / results …): faint fill + hairline border.
pub fn sub_card() -> Div {
    div().rounded(px(SUB_RADIUS)).border_1().border_color(crate::theme::divider()).overflow_hidden().bg(crate::theme::sub_bg())
}

/// 子卡片内容区：填满剩余空间并带 `SUB_PAD` 内边距，让编辑器等内容与卡片边框之间留出间距。
pub fn sub_card_body() -> Div {
    div().flex_1().min_h_0().p(px(SUB_PAD))
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
        // 子卡片内容区边距与工具栏水平边距（SP_2）一致，标题与内容左边缘对齐
        assert_eq!(SUB_PAD, crate::ui::SP_2);
    }
}
