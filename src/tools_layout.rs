//! 工具箱输入 / 输出区的高度规则与共用部件（规则见 `docs/UI_REDESIGN_PLAN.md` 的“工具箱输入输出高度”一节）。
//! 这里只放常量、纯函数与分隔条，不持有任何工具状态；全部尺寸来自 `ui` / `card_layout` 的 token。
use gpui::{div, prelude::*, px, Context, ElementId, IntoElement, Stateful, Window};

use crate::card_layout::CARD_PAD;
use crate::theme::{c, cur};
use crate::ui;

/// 输入与输出两个大文本区的分配比例范围；默认对半。
pub const SPLIT_MIN: f32 = 0.2;
pub const SPLIT_MAX: f32 = 0.8;
pub const SPLIT_DEFAULT: f32 = 0.5;
/// 工具面板至少这么宽时，两个大文本区左右并排。
pub const SIDE_MIN_W: f32 = 880.;
/// 大文本输入卡片内的编辑器最小高度，与输出卡片的最小高度。
pub const IN_FILL_MIN_H: f32 = 90.;
pub const OUT_FILL_MIN_H: f32 = 120.;
/// 并排时左右两栏的最小宽度。
pub const SIDE_PANE_MIN_W: f32 = 280.;
/// 文件拖放区：单个文件的最小高度，多个文件时随列表增高、到上限后滚动。
pub const FILE_ZONE_MIN_H: f32 = 64.;
pub const FILE_ZONE_MAX_H: f32 = 160.;
/// 输出文本编辑器的最小高度。
pub const OUT_EDITOR_MIN_H: f32 = 80.;
/// 图片预览框的最小高度。
pub const PREVIEW_MIN_H: f32 = 96.;

/// 贴合内容的输出卡片的最小高度：标题行加两行结果，窗口再矮也不会被压没（整个工具区改为滚动）。
pub fn fit_min_h() -> f32 { ui::H_COMPACT + 2. * ui::row_h() }

/// 含大文本框的输入卡片的最小高度：内边距 + 每个字段的标签行与最小高度（单行字段按默认行高，大文本按 `IN_FILL_MIN_H`，其余内容定高的不计）。
/// 卡片自身裁剪溢出，必须给出明确的下限，否则会被压到只剩一条边。
pub fn in_min_h(sizes: &[ssh_term_proto::tools::InSize]) -> f32 {
    use ssh_term_proto::tools::InSize;
    let label = ui::lh(ui::fs_small()) + ui::SP_1;
    let body: f32 = sizes.iter().map(|z| match z {
        InSize::Fill => IN_FILL_MIN_H,
        InSize::Line => label + ui::H_DEFAULT,
        InSize::Auto(min, _) => label + *min as f32 * ui::lh(ui::fs_mono()) + ui::SP_2 * 2.,
        InSize::Zone => label + FILE_ZONE_MIN_H,
    }).sum();
    2. * crate::card_layout::SUB_PAD + body + ui::SP_2 * sizes.len().saturating_sub(1) as f32
}

pub fn clamp_split(r: f32) -> f32 { if r.is_finite() { r.clamp(SPLIT_MIN, SPLIT_MAX) } else { SPLIT_DEFAULT } }

/// 面板够宽才并排。
pub fn side_by_side(width: f32) -> bool { width >= SIDE_MIN_W }

/// 由指针位置算输入区占的比例：`pos` 是指针沿拖动方向的坐标，`origin` 是容器起点，`free` 是两个可伸缩区合起来的长度
/// （容器总长减去选项卡片和间隙）；`gap` 是分隔条自身的厚度（指针落在它的中线）。
pub fn split_ratio(pos: f32, origin: f32, free: f32, gap: f32) -> f32 {
    clamp_split((pos - origin - gap / 2.) / free.max(1.))
}

/// 拖动分隔条的载荷。
#[derive(Clone)]
pub struct ToolSplit;
impl gpui::Render for ToolSplit {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement { div() }
}

/// 输入与输出之间的分隔条：厚度与卡片间隙相同，悬停高亮，横向（上下排列时）或纵向（并排时）拖动。
pub fn handle(id: impl Into<ElementId>, vertical: bool) -> Stateful<gpui::Div> {
    let h = div().id(id).flex_shrink_0().hover(|d| d.bg(c(cur().accent).opacity(0.35)))
        .on_drag(ToolSplit, |d: &ToolSplit, _, _, cx| cx.new(|_| d.clone()));
    if vertical { h.w(px(CARD_PAD)).h_full().cursor_col_resize() } else { h.h(px(CARD_PAD)).w_full().cursor_row_resize() }
}

/// 普通的卡片间隙（不可拖动）。
pub fn gap(vertical: bool) -> gpui::Div {
    if vertical { div().w(px(CARD_PAD)).h_full().flex_shrink_0() } else { div().h(px(CARD_PAD)).w_full().flex_shrink_0() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_rules() {
        assert_eq!(clamp_split(0.0), SPLIT_MIN);
        assert_eq!(clamp_split(1.0), SPLIT_MAX);
        assert_eq!(clamp_split(f32::NAN), SPLIT_DEFAULT);
        assert_eq!(clamp_split(0.5), 0.5);
        // 指针在容器正中、总长 400、分隔条 6：输入区高 197，可伸缩总长 394 → 约一半
        let r = split_ratio(200., 0., 394., 6.);
        assert!((r - 0.5).abs() < 0.01, "{r}");
        assert_eq!(split_ratio(-50., 0., 400., 6.), SPLIT_MIN);
        assert_eq!(split_ratio(900., 0., 400., 6.), SPLIT_MAX);
    }
    #[test]
    fn min_heights() {
        use ssh_term_proto::tools::InSize;
        // 只有大文本：内边距 + 最小高度；每多一个字段多一个间隙与标签行
        let one = in_min_h(&[InSize::Fill]);
        assert_eq!(one, 2. * crate::card_layout::SUB_PAD + IN_FILL_MIN_H);
        assert!(in_min_h(&[InSize::Line, InSize::Fill, InSize::Line]) > one + 2. * ui::H_DEFAULT);
        // 贴合内容的输出至少容下标题行和两行结果
        assert!(fit_min_h() >= ui::H_COMPACT + 2. * ui::H_DEFAULT);
    }
    #[test]
    fn width_threshold() {
        assert!(!side_by_side(879.9) && side_by_side(880.));
        assert!(SIDE_PANE_MIN_W * 2. + CARD_PAD < SIDE_MIN_W);
        assert!(FILE_ZONE_MIN_H < FILE_ZONE_MAX_H && IN_FILL_MIN_H < OUT_FILL_MIN_H);
    }
}
