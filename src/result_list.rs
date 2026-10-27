//! 工具结果的共享列表组件：结果行（逐条复制 / 删除 / 强度标签）、键值行、批次头。
//! 只负责样式；点击行为由调用方以回调传入，不持有状态。
use gpui::{div, prelude::*, px, AnyElement, App, ClickEvent, Div, FontWeight, Hsla, SharedString, Window};
use ssh_term_proto::i18n::t;
use ssh_term_proto::tools::{Row, Tone};

use crate::theme::{c, cur, hover_bg};
use crate::ui::{self, IconBtn, IconName};

pub type ClickFn = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// 行的版式：`List` 为生成类结果（标签窄），`Kv` 为多格式键值（标签宽）。
#[derive(Clone, Copy, PartialEq)]
pub enum RowMode { List, Kv }

/// 标签列宽：生成类 72，键值 120。
const LABEL_W_LIST: f32 = ui::SP_6 * 3.;
const LABEL_W_KV: f32 = ui::SP_6 * 5.;
/// 被遮住的内容最多画这么多个圆点。
const MASK_MAX: usize = 64;

pub fn tone_color(tone: Tone) -> Hsla {
    let th = cur();
    c(match tone { Tone::Muted => th.overlay1, Tone::Danger => th.error, Tone::Warn => th.warning, Tone::Good => th.success, Tone::Accent => th.accent })
}

/// 超过这个长度的整段内容（哈希等）也允许折行。
const WRAP_LEN: usize = 32;

/// 内容里有空白（句子、名单）时允许换行，否则单行省略并在悬停时显示全文。
fn wraps(v: &str) -> bool { v.contains(char::is_whitespace) || v.contains('、') || v.chars().count() > WRAP_LEN }

/// 一行结果：[标签] 内容 [标签徽标] [复制] [删除（悬停显示）]。
/// `masked` 时内容画成圆点（复制仍复制原文）；`copied` 时复制图标短暂变成对勾。
pub fn result_row(id: impl Into<SharedString>, r: &Row, mode: RowMode, masked: bool, copied: bool, on_copy: ClickFn, on_delete: Option<ClickFn>) -> AnyElement {
    let th = cur();
    let id: SharedString = id.into();
    if r.head { return ui::section_header(id, r.value.clone(), None, None).into_any_element(); }
    let gid = SharedString::from(format!("rr-{id}"));
    let shown = if masked { "•".repeat(r.value.chars().count().min(MASK_MAX)) } else { r.value.clone() };
    let wrap = wraps(&r.value);
    let vsize = if r.primary && r.value.chars().count() <= WRAP_LEN { ui::fs_title() } else { ui::fs_mono() };
    let vcol = r.tone.map(tone_color).unwrap_or_else(|| c(th.text));
    let value = div().id(SharedString::from(format!("{id}-v"))).flex_1().min_w_0().font_family(ui::mono_family()).text_size(px(vsize)).line_height(px(ui::lh(vsize)))
        .text_color(vcol).when(r.primary, |d| d.font_weight(FontWeight::SEMIBOLD))
        .when(wrap, |d| d.whitespace_normal())
        .when(!wrap, |d| d.whitespace_nowrap().overflow_hidden().text_ellipsis())
        .when(!wrap && !masked, |d| { let tip = r.value.clone(); d.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx)) })
        .child(shown);
    let label_w = if mode == RowMode::Kv { LABEL_W_KV } else { LABEL_W_LIST };
    let row = div().id(id.clone()).group(gid.clone()).min_h(px(ui::row_h())).mx(px(ui::LIST_INSET)).pl(px(ui::ROW_PX)).pr(px(4.)).rounded(px(ui::R_MD)).flex().items_center().gap(px(6.))
        .hover(|s| s.bg(hover_bg()))
        .when(!r.label.is_empty(), |d| d.child(div().w(px(label_w)).flex_shrink_0().whitespace_nowrap().overflow_hidden().text_ellipsis()
            .text_size(px(ui::fs_small())).text_color(c(th.subtext)).child(r.label.clone())))
        .child(value)
        .when_some(r.tag.clone(), |d, (text, tone)| {
            let hint = r.hint.clone();
            d.child(div().id(SharedString::from(format!("{id}-t"))).flex_shrink_0()
                .when(!hint.is_empty(), |d| d.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(hint.clone()).build(w, cx)))
                .child(ui::tag(text, tone_color(tone))))
        })
        .child(ui::icon_button(SharedString::from(format!("{id}-cp")), if copied { IconName::Check } else { IconName::Copy }, IconBtn::Sm, t("tools.ui_copy")).on_click(on_copy))
        .when_some(on_delete, |d, f| d.child(ui::row_actions(gid.clone()).child(ui::icon_button_state(SharedString::from(format!("{id}-del")), IconName::X, IconBtn::Sm, t("tools.hist_delete_row"), false, true).on_click(f))));
    row.into_any_element()
}

/// 批次头需要的回调与状态。
pub struct BatchHead {
    pub id: u64,
    pub time: String,
    pub meta: String,
    pub count: usize,
    pub folded: bool,
    /// `Some(是否已遮住)` 表示该批含敏感内容，显示“隐藏 / 显示”眼睛。
    pub masked: Option<bool>,
    pub on_fold: ClickFn,
    pub on_mask: ClickFn,
    pub on_copy: ClickFn,
    pub on_delete: ClickFn,
}

/// 批次头：[折叠箭头] 时间 参数摘要 ………… 条数 [眼睛] [复制本批] [删除本批]。
pub fn batch_head(h: BatchHead) -> Div {
    let th = cur();
    let id = h.id;
    div().h(px(ui::SECTION_H)).mx(px(ui::LIST_INSET)).pl(px(ui::ROW_PX)).pr(px(4.)).flex().items_center().gap(px(ui::SP_1))
        .child(div().id(SharedString::from(format!("bh-{id}"))).flex_1().min_w_0().h_full().flex().items_center().gap(px(ui::SP_1)).cursor_pointer().on_click(h.on_fold)
            .tooltip(|w, cx| gpui_component::tooltip::Tooltip::new(t("tools.hist_fold")).build(w, cx))
            .child(ui::icon(if h.folded { IconName::ChevronRight } else { IconName::ChevronDown }, ui::IC_XS, c(th.overlay1)))
            .child(div().flex_shrink_0().text_size(px(ui::fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.subtext)).child(h.time))
            .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(ui::fs_small())).text_color(c(th.overlay1)).child(h.meta))
            .child(div().flex_shrink_0().text_size(px(ui::fs_caption())).text_color(c(th.overlay1)).child(h.count.to_string())))
        .when_some(h.masked, |d, m| d.child(ui::icon_button(SharedString::from(format!("bh-{id}-eye")), if m { IconName::EyeOff } else { IconName::Eye }, IconBtn::Sm,
            if m { t("tools.hist_show") } else { t("tools.hist_hide") }).on_click(h.on_mask)))
        .child(ui::icon_button(SharedString::from(format!("bh-{id}-cp")), IconName::Copy, IconBtn::Sm, t("tools.hist_copy_batch")).on_click(h.on_copy))
        .child(ui::icon_button_state(SharedString::from(format!("bh-{id}-del")), IconName::Trash, IconBtn::Sm, t("tools.hist_delete_batch"), false, true).on_click(h.on_delete))
}
