//! Small shared widgets drawn with plain GPUI divs + Lucide icons (gpui-kit-assets, ISC).
// 公共组件按规格一次性建好，业务视图随后分阶段接入；全部接入后（收尾阶段）移除此处的允许标注。
#![allow(dead_code)]
use gpui::{div, prelude::*, px, svg, AnyElement, Div, ElementId, FontWeight, Hsla, SharedString, Stateful};
pub use gpui_kit_assets::IconName;

use crate::theme::{c, cur, divider, hover_bg, menu_sel_bg, selected_bg};

// ---- design tokens (界面规范 §3) ----
/// 字号：由「设置 › 外观 › 界面字号」B 推导（默认 13），业务视图只调用这些函数。
/// 徽标 / kbd / 计数等极短标注。
pub fn fs_caption() -> f32 { crate::theme::ui_size() - 2. }
/// 次要信息、状态栏、分区标题、chip / 紧凑按钮、表头。
pub fn fs_small() -> f32 { crate::theme::ui_size() - 1. }
/// 正文：列表主文本、标签页标题、菜单、按钮、输入框、面板与卡片标题。
pub fn fs_body() -> f32 { crate::theme::ui_size() }
/// 弹窗标题。
pub fn fs_title() -> f32 { crate::theme::ui_size() + 2. }
/// 设置页面大标题。
pub fn fs_page() -> f32 { crate::theme::ui_size() + 7. }
/// 列表 / 表格里的命令、SQL、路径、哈希（等宽字体）。
pub fn fs_mono() -> f32 { crate::theme::ui_size() - 1. }
/// 代码块、审批卡片里的命令、日志正文。
pub fn fs_mono_block() -> f32 { crate::theme::ui_size() }
/// 行高：字号的 1.4 倍取整，列表行内文本显式设置，避免默认行高撑高行。
pub fn lh(fs: f32) -> f32 { (fs * 1.4).round() }
/// 等宽场景统一使用终端字体。
pub fn mono_family() -> SharedString { SharedString::from(crate::config::get().font.family.clone()) }

/// 间距（4px 网格）。
pub const SP_1: f32 = 4.;
pub const SP_2: f32 = 8.;
pub const SP_3: f32 = 12.;
pub const SP_4: f32 = 16.;
pub const SP_5: f32 = 20.;
pub const SP_6: f32 = 24.;
/// 列表外侧留白：行的悬停底色距岛边 6px。
pub const LIST_INSET: f32 = 6.;
/// 行内左右内边距。
pub const ROW_PX: f32 = 8.;
/// 面板内容（标题、说明文字）左右内边距 = LIST_INSET + ROW_PX，保证标题、搜索图标、行文本左对齐。
pub const PANEL_PX: f32 = 14.;
/// 树每级缩进。
pub const INDENT: f32 = 16.;
/// 输入框左右内边距。
pub const INPUT_PX: f32 = 8.;

/// Corner radius scale: tag/kbd, button/input/row, sub-card, island/popover/dialog, chat bubble.
pub const R_SM: f32 = 4.;
pub const R_MD: f32 = 6.;
pub const R_SUB: f32 = 8.;
pub const R_LG: f32 = 10.;
pub const R_XL: f32 = 12.;
/// Control height scale: compact (chip, segmented), default (button, input, menu item), large (palette input, dialog CTA).
pub const H_COMPACT: f32 = 24.;
pub const H_DEFAULT: f32 = 28.;
pub const H_LARGE: f32 = 32.;
/// 列表分区标题行 / 岛标题行 / 内容岛工具栏 / 表头行 / 弹窗头部。
pub const SECTION_H: f32 = 24.;
pub const PANEL_HEAD_H: f32 = 36.;
pub const TOOLBAR_H: f32 = 36.;
pub const TABLE_HEAD_H: f32 = 28.;
pub const DIALOG_HEAD_H: f32 = 48.;
/// 单行列表 / 树行高：随字号增长，默认 28。
pub fn row_h() -> f32 { (crate::theme::ui_size() + 15.).max(28.) }
/// 双行列表行高：两行文本加 6px 内边距，按 4px 网格向下取整（默认 40）。
pub fn row2_h() -> f32 { let b = crate::theme::ui_size(); ((lh(b) + lh(b - 1.) + 6.) / 4.).floor() * 4. }

/// 图标：展开箭头与行内操作 / 标题行与工具栏 / 列表实体与菜单 / 活动栏 / 空状态插图。
pub const IC_XS: f32 = 12.;
pub const IC_SM: f32 = 14.;
pub const IC_MD: f32 = 16.;
pub const IC_LG: f32 = 18.;
pub const IC_XL: f32 = 28.;
/// 实体图标角上的小徽标（生产环境盾牌）。
pub const IC_BADGE: f32 = 9.;

/// 统一的单行输入框：高 28（`large` 时 32）、圆角 6、左右内边距 8、正文字号。
/// 聚焦时只把边框换成强调色（主题关闭了外扩的聚焦环），不会被父元素裁切。
pub fn text_field(state: &gpui::Entity<gpui_component::input::InputState>, large: bool) -> gpui_component::input::Input {
    gpui_component::input::Input::new(state)
        .w_full().h(px(if large { H_LARGE } else { H_DEFAULT }))
        .rounded(px(R_MD)).px(px(INPUT_PX)).text_size(px(fs_body()))
}

/// 统一的搜索框：在 `text_field` 基础上加放大镜图标和清除按钮。
pub fn search_field(state: &gpui::Entity<gpui_component::input::InputState>, large: bool) -> gpui_component::input::Input {
    text_field(state, large).prefix(icon(IconName::Search, IC_SM, c(cur().overlay1))).cleanable(true)
}

/// 面板顶部放搜索框的一行：左右留白与列表行的悬停底色对齐，不被压缩。
pub fn field_row() -> Div {
    div().w_full().flex_shrink_0().px(px(LIST_INSET))
}

pub fn icon(name: IconName, size: f32, color: Hsla) -> impl IntoElement {
    svg().path(name.path()).size(px(size)).flex_shrink_0().text_color(color)
}

/// 图标按钮档位：Sm 行内操作 / Md 岛标题行与工具栏 / Lg 标题栏 / Strip 活动栏与右工具条。
#[derive(Clone, Copy, PartialEq)]
pub enum IconBtn { Sm, Md, Lg, Strip }

/// 统一图标按钮：默认透明、图标 overlay1，悬停淡底 + 图标 text；必须带 tooltip。
pub fn icon_button(id: impl Into<ElementId>, ic: IconName, size: IconBtn, tip: impl Into<SharedString>) -> Stateful<Div> {
    icon_button_state(id, ic, size, tip, false, false)
}

/// 图标按钮的完整形态：`active` 为激活态（淡强调底 + 强调色图标），`danger` 为危险操作（悬停变红）。
pub fn icon_button_state(id: impl Into<ElementId>, ic: IconName, size: IconBtn, tip: impl Into<SharedString>, active: bool, danger: bool) -> Stateful<Div> {
    icon_button_svg(id, ic.path(), size, tip, active, danger)
}

/// 同 `icon_button_state`，图标换成自带的 SVG 资源路径（数据库引擎、工具箱等品牌图标）。
pub fn icon_button_svg(id: impl Into<ElementId>, path: impl Into<SharedString>, size: IconBtn, tip: impl Into<SharedString>, active: bool, danger: bool) -> Stateful<Div> {
    let th = cur();
    let id: ElementId = id.into();
    let group = SharedString::from(format!("ib-{id}"));
    let (boxed, glyph, radius) = match size {
        IconBtn::Sm => (20., IC_XS, R_SM),
        IconBtn::Md => (24., IC_SM, R_MD),
        IconBtn::Lg => (28., IC_MD, R_MD),
        IconBtn::Strip => (32., IC_LG, R_SUB),
    };
    let tip: SharedString = tip.into();
    let (rest, hot) = if active { (c(th.accent), c(th.accent)) } else if danger { (c(th.overlay1), c(th.error)) } else { (c(th.overlay1), c(th.text)) };
    div().id(id).group(group.clone()).size(px(boxed)).flex_shrink_0().rounded(px(radius)).flex().items_center().justify_center().cursor_pointer()
        .when(active, |d| d.bg(selected_bg()))
        .when(!active, |d| d.hover(move |s| s.bg(if danger { crate::theme::danger_bg() } else { hover_bg() })).active(|s| s.bg(selected_bg())))
        // 提示为空时不挂 tooltip（仅用于菜单展开期间避免遮住第一项）
        .when(!tip.is_empty(), |d| d.tooltip(move |w, cx| gpui_component::tooltip::Tooltip::new(tip.clone()).build(w, cx)))
        .child(svg().path(path).size(px(glyph)).flex_shrink_0().text_color(rest).group_hover(group, move |s| s.text_color(hot)))
}

/// 弹窗 / 浮层右上角的关闭按钮。
pub fn dialog_close(id: impl Into<ElementId>, on: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static) -> AnyElement {
    icon_button(id, IconName::X, IconBtn::Lg, ssh_term_proto::i18n::t("ui.close")).on_click(move |_, w, cx| on(w, cx)).into_any_element()
}

/// 面板 / 岛的标题行：高 36，标题半粗，可选图标与副标题，右侧为 `IconBtn::Md` 操作。
pub fn panel_header(title: impl Into<SharedString>, subtitle: Option<SharedString>, ic: Option<IconName>, actions: Vec<AnyElement>) -> Div {
    let th = cur();
    let lead = ic.map(|i| icon(i, IC_SM, c(th.overlay1)).into_any_element()).into_iter()
        .chain([div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_body())).line_height(px(lh(fs_body()))).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title.into()).into_any_element()])
        .chain(subtitle.map(|s| div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_small())).text_color(c(th.subtext)).child(format!("· {s}")).into_any_element()))
        .collect();
    panel_header_with(lead, actions)
}

/// 标题行的通用形态：左侧放任意前导元素（标题、页签 chip），右侧为 `IconBtn::Md` 操作。
pub fn panel_header_with(lead: Vec<AnyElement>, actions: Vec<AnyElement>) -> Div {
    div().w_full().h(px(PANEL_HEAD_H)).flex_shrink_0().pl(px(PANEL_PX)).pr(px(PANEL_PX - 4.)).flex().items_center().gap(px(6.))
        .children(lead)
        .child(div().flex_1())
        .child(div().flex().items_center().gap(px(2.)).children(actions))
}

/// 列表分区标题行：可选展开箭头与计数；返回 Stateful 以便调用方挂点击 / 拖放。
pub fn section_header(id: impl Into<ElementId>, label: impl Into<SharedString>, count: Option<usize>, collapsed: Option<bool>) -> Stateful<Div> {
    let th = cur();
    div().id(id).h(px(SECTION_H)).flex_shrink_0().mx(px(LIST_INSET)).px(px(ROW_PX)).flex().items_center().gap(px(4.)).rounded(px(R_MD))
        .when(collapsed.is_some(), |d| d.cursor_pointer())
        .children(collapsed.map(|c0| icon(if c0 { IconName::ChevronRight } else { IconName::ChevronDown }, IC_XS, c(th.overlay1))))
        .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.subtext)).child(label.into()))
        .children(count.map(|n| div().text_size(px(fs_caption())).text_color(c(th.overlay1)).child(n.to_string())))
}

/// 列表 / 树的一行：悬停淡底、选中淡强调底；`depth` 为树层级，双行行用 `.min_h(row2_h())` 覆盖高度。
pub fn list_row(id: impl Into<ElementId>, depth: usize, selected: bool) -> Stateful<Div> {
    div().id(id).min_h(px(row_h())).mx(px(LIST_INSET)).pl(px(ROW_PX + INDENT * depth as f32)).pr(px(4.)).rounded(px(R_MD)).flex().items_center().gap(px(6.)).cursor_pointer()
        .when(selected, |d| d.bg(selected_bg()))
        .when(!selected, |d| d.hover(|s| s.bg(hover_bg())))
}

/// 行内单行省略文本：主文本。
pub fn row_title(text: impl Into<SharedString>) -> Div {
    div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_body())).line_height(px(lh(fs_body()))).text_color(c(cur().text)).child(text.into())
}

/// 行内单行省略文本：次要信息。
pub fn row_meta(text: impl Into<SharedString>) -> Div {
    div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_small())).line_height(px(lh(fs_small()))).text_color(c(cur().subtext)).child(text.into())
}

/// 行尾操作容器：默认隐藏，悬停所在 group 时显示（选中行由调用方再调 `.visible()`），内部放 `IconBtn::Sm`。
pub fn row_actions(group: impl Into<SharedString>) -> Div {
    div().flex_shrink_0().flex().items_center().gap(px(2.)).invisible().group_hover(group, |s| s.visible())
}

/// Standard button. `kind`: 0 = secondary, 1 = primary, 2 = danger, 3 = ghost.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: u8) -> Stateful<Div> {
    button_base(id, label, kind, H_DEFAULT, SP_3, fs_body())
}

/// 紧凑按钮：高 24、左右内边距 8、次要字号；用于行内与表格内操作。
pub fn button_sm(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: u8) -> Stateful<Div> {
    button_base(id, label, kind, H_COMPACT, SP_2, fs_small())
}

fn button_base(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: u8, h: f32, pad: f32, fs: f32) -> Stateful<Div> {
    let th = cur();
    let (bg, fg, border) = match kind {
        1 => (c(th.accent), c(th.base), c(th.accent)),
        2 => (c(th.error).opacity(0.15), c(th.error), c(th.error).opacity(0.4)),
        3 => (gpui::transparent_black(), c(th.subtext), gpui::transparent_black()),
        _ => (c(th.surface0), c(th.text), c(th.surface1)),
    };
    div().id(id).h(px(h)).px(px(pad)).rounded(px(R_MD)).flex().items_center().justify_center().gap_1().cursor_pointer()
        .bg(bg).text_color(fg).border_1().border_color(border).text_size(px(fs)).font_weight(FontWeight::MEDIUM)
        .hover(move |s| s.opacity(0.85))
        // pressed state: one step darker than hover
        .active(move |s| s.opacity(0.7))
        .child(label.into())
}

/// 筛选 chip：高 24 的胶囊；未选淡底 + 次文本，选中淡强调底 + 强调色文字。
pub fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    let th = cur();
    div().id(id).h(px(H_COMPACT)).px(px(10.)).rounded_full().flex().items_center().flex_shrink_0().whitespace_nowrap().cursor_pointer().text_size(px(fs_small()))
        .bg(if on { selected_bg() } else { hover_bg() })
        .text_color(if on { c(th.accent) } else { c(th.subtext) })
        .when(on, |d| d.font_weight(FontWeight::MEDIUM))
        .hover(move |s| s.bg(if on { c(cur().accent).opacity(0.16) } else { c(cur().text).opacity(0.10) }))
        .child(label.into())
}

/// Segmented choice control. Returns the row; each option gets an id `(base, index)`.
pub fn segmented(base: &'static str, options: &[(String, String)], selected: &str, on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> impl IntoElement {
    segmented_with(base, options, selected, false, on)
}

/// 占满容器宽度的分段控件：各选项等分、文字居中（卡片内的二选一筛选）。
pub fn segmented_fill(base: &'static str, options: &[(String, String)], selected: &str, on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> impl IntoElement {
    segmented_with(base, options, selected, true, on)
}

fn segmented_with(base: &'static str, options: &[(String, String)], selected: &str, fill: bool, on: impl Fn(String, &mut gpui::Window, &mut gpui::App) + 'static + Clone) -> impl IntoElement {
    let th = cur();
    let mut row = div().w_full().flex().flex_wrap().items_center().p(px(2.)).gap(px(2.)).rounded(px(R_MD)).bg(hover_bg());
    for (i, (val, label)) in options.iter().enumerate() {
        let active = val == selected;
        let v = val.clone();
        let on = on.clone();
        row = row.child(
            div().id((base, i)).h(px(H_COMPACT)).px(px(10.)).rounded(px(R_MD)).flex().items_center().cursor_pointer().text_size(px(fs_small()))
                .when(fill, |d| d.flex_1().justify_center())
                .when(active, |d| d.bg(crate::theme::card_bg()).shadow_sm().font_weight(FontWeight::MEDIUM))
                .text_color(if active { c(th.text) } else { c(th.subtext) })
                .hover(|s| s.text_color(c(cur().text)))
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
                .border_1().border_color(if active { c(th.accent) } else { divider() })
                .bg(if active { selected_bg() } else { gpui::transparent_black() })
                .hover(|s| s.bg(hover_bg()))
                .active(|s| s.bg(selected_bg()))
                .on_click(move |_, w, cx| on(v.clone(), w, cx))
                .children(thumb)
                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_small()))
                    .text_color(if active { c(th.text) } else { c(th.subtext) }).when(active, |d| d.font_weight(FontWeight::MEDIUM)).child(label.clone()))
                .when(active, |d| d.child(icon(IconName::Check, IC_SM, c(th.accent)))),
        );
    }
    grid
}

/// Small pill tag.
pub fn tag(label: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    div().px(px(6.)).py(px(1.)).rounded(px(R_SM)).bg(color.opacity(0.16)).text_color(color).text_size(px(fs_caption())).font_weight(FontWeight::MEDIUM).child(label.into())
}

/// Keyboard shortcut chip, e.g. "ctrl-shift-p" -> Ctrl Shift P.
pub fn kbd(binding: &str) -> AnyElement {
    let th = cur();
    let mut row = div().flex().gap(px(3.));
    for part in binding.split('-').filter(|p| !p.is_empty()) {
        let label = match part { "ctrl" => "Ctrl".to_string(), "shift" => "Shift".into(), "alt" => "Alt".into(), "cmd" | "super" => "⌘".into(),
            "comma" | "," => ",".into(), "tab" => "Tab".into(), "escape" => "Esc".into(), p => p.to_uppercase() };
        row = row.child(div().px(px(5.)).h(px(18.)).flex().items_center().rounded(px(R_SM)).bg(c(th.surface0)).border_1().border_color(c(th.surface1))
            .text_size(px(fs_caption())).text_color(c(th.subtext)).child(label));
    }
    row.into_any_element()
}

/// 内容岛 / 面板顶部工具栏：高 36、底部发丝线、无底色。
pub fn toolbar() -> Div {
    div().w_full().h(px(TOOLBAR_H)).flex_shrink_0().px(px(SP_2)).gap(px(SP_1)).flex().items_center().border_b_1().border_color(divider())
}

/// 表头行：高 28、次要字号半粗、淡底、底部发丝线。
pub fn table_header() -> Div {
    div().w_full().h(px(TABLE_HEAD_H)).flex_shrink_0().flex().items_center().text_size(px(fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(cur().subtext)).bg(hover_bg()).border_b_1().border_color(divider())
}

/// 空状态：垂直居中的插图 + 标题 + 说明 + 按钮；`horizontal` 为真时按钮横排（宽区域），否则纵向全宽（窄面板）。
pub fn empty_state(ic: IconName, title: impl Into<SharedString>, desc: Option<SharedString>, actions: Vec<AnyElement>, horizontal: bool) -> Div {
    let th = cur();
    div().w_full().flex_1().min_h_0().px(px(PANEL_PX)).py(px(SP_6)).flex().flex_col().items_center().justify_center().gap(px(SP_2))
        .child(icon(ic, IC_XL, c(th.overlay0)))
        .child(div().text_size(px(fs_body())).font_weight(FontWeight::MEDIUM).text_color(c(th.subtext)).text_center().child(title.into()))
        .children(desc.map(|d| div().max_w(px(260.)).text_size(px(fs_small())).text_color(c(th.overlay1)).text_center().child(d)))
        .when(!actions.is_empty(), |d| d.child(
            div().mt(px(SP_1)).flex().gap(px(SP_2)).when(horizontal, |r| r.items_center()).when(!horizontal, |r| r.flex_col().w_full().max_w(px(260.))).children(actions)))
}

/// 行内提示条的类别。
#[derive(Clone, Copy, PartialEq)]
pub enum Notice { Info, Warn, Error }

/// 行内提示条：淡底色块 + 同色文字，用于提示、警告与错误。
pub fn notice(kind: Notice, text: impl Into<SharedString>) -> Div {
    let th = cur();
    let col = match kind { Notice::Info => c(th.info), Notice::Warn => c(th.warning), Notice::Error => c(th.error) };
    div().mx(px(LIST_INSET)).px(px(ROW_PX)).py(px(6.)).rounded(px(R_MD)).bg(col.opacity(0.10)).text_color(col).text_size(px(fs_small())).child(text.into())
}

/// 弹窗头部：高 48，可选强调色图标、标题 + 副标题、右侧关闭按钮（`dialog_close`）。
pub fn dialog_header(ic: Option<IconName>, title: impl Into<SharedString>, subtitle: Option<SharedString>, close: Option<AnyElement>) -> Div {
    let th = cur();
    div().w_full().h(px(DIALOG_HEAD_H)).flex_shrink_0().px(px(SP_5)).flex().items_center().gap(px(SP_2)).border_b_1().border_color(divider())
        .children(ic.map(|i| icon(i, IC_MD, c(th.accent))))
        .child(div().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_title())).font_weight(FontWeight::SEMIBOLD).text_color(c(th.text)).child(title.into()))
        .children(subtitle.map(|s| div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_size(px(fs_small())).text_color(c(th.subtext)).child(s)))
        .child(div().flex_1())
        .children(close)
}

/// 弹窗底栏：顶部发丝线、无底色；按钮右对齐，危险按钮（如有）靠左。
pub fn dialog_footer(danger: Option<AnyElement>, children: Vec<AnyElement>) -> Div {
    div().w_full().flex_shrink_0().px(px(SP_5)).py(px(SP_3)).flex().items_center().gap(px(SP_2)).border_t_1().border_color(divider())
        .children(danger)
        .child(div().flex_1())
        .children(children)
}

/// Modal backdrop + centered card.
pub fn modal(card: impl IntoElement + 'static) -> Div {
    // occlude: the backdrop swallows mouse/scroll events so the terminal below stays inert
    div().absolute().top_0().left_0().size_full().occlude().flex().items_center().justify_center().bg(gpui::black().opacity(0.28)).p(px(SP_4)).child(crate::chrome::pop_in("modal-pop", card))
}

/// 弹出面（菜单、命令面板）：圆角 10、发丝边框、柔和分层阴影。
pub fn popover() -> Div {
    let th = cur();
    div().rounded(px(R_LG)).bg(c(th.base)).border_1().border_color(divider()).shadow(crate::chrome::soft_shadow(true))
}

pub fn card(w: f32) -> Div {
    let th = cur();
    div().w(px(w)).occlude().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).rounded(px(R_LG)).bg(c(th.base).opacity(0.97)).border_1().border_color(divider()).shadow(crate::chrome::soft_shadow(true)).flex().flex_col().overflow_hidden()
}

/// Finder tag dots (PRD 5.23), overlapping slightly like Finder's list view.
/// Single Finder-style tag dot (the first tag of a host / group), or nothing.
pub fn tag_dot(tag: Option<&String>) -> Option<AnyElement> {
    tag.map(|t| div().size(px(8.)).flex_shrink_0().rounded_full().bg(c(crate::config::get().tags.rgb(t))).into_any_element())
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
            .when(sel, |d| d.child(icon(IconName::Check, IC_XS, gpui::white()))));
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
            .hover(|d| d.bg(hover_bg()))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_, w, cx| on(idc.clone(), w, cx))
            .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(col))
            .child(div().text_size(px(fs_small())).whitespace_nowrap().text_color(c(th.text)).child(name.clone()))
            .when(sel, |d| d.child(icon(IconName::Check, IC_XS, col))));
    }
    row.into_any_element()
}

/// Menu row (context menus, popover menus): icon + label, 28px; hover = keyboard-highlight color, danger items red.
pub fn menu_item(id: impl Into<ElementId>, ic: Option<IconName>, label: impl Into<SharedString>, enabled: bool, danger: bool) -> Stateful<Div> {
    let th = cur();
    let id: ElementId = id.into();
    let group = SharedString::from(format!("mi-{id}"));
    let col = if !enabled { c(th.overlay0) } else if danger { c(th.error) } else { c(th.text) };
    // 28px row, 16px icon + 8px gap, body label; hover: menu_sel_bg + normal text and icon (danger: danger_bg + error)
    div().id(id).group(group.clone()).h(px(H_DEFAULT)).min_w(px(200.)).px(px(10.)).rounded(px(R_MD)).flex().items_center().gap_2().text_size(px(fs_body())).text_color(col)
        .when(enabled, |d| d.cursor_pointer().hover(move |d| if danger { d.bg(crate::theme::danger_bg()) } else { d.bg(menu_sel_bg()) }).active(|d| d.opacity(0.85)))
        .child(match ic {
            Some(i) => svg().path(i.path()).size(px(IC_MD)).flex_shrink_0().text_color(if danger { c(th.error) } else { c(th.overlay1) }).group_hover(group, move |s| s.text_color(if danger || !enabled { col } else { c(cur().text) })).into_any_element(),
            None => div().w(px(IC_MD)).into_any_element(),
        })
        .child(div().flex_1().whitespace_nowrap().child(label.into()))
}

pub fn menu_sep() -> Div { div().mx(px(8.)).my(px(4.)).h(px(1.)).bg(divider()) }

/// 菜单内的分组小标题：次要字号半粗、不大写。
pub fn menu_label(text: impl Into<SharedString>) -> Div {
    div().h(px(H_COMPACT)).px(px(10.)).flex().items_center().text_size(px(fs_small())).font_weight(FontWeight::SEMIBOLD).text_color(c(cur().subtext)).child(text.into())
}
