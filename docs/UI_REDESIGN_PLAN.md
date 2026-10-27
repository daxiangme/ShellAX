# ShellAX 界面样式统一优化方案（Island 风格 v2）

> 用途：交给实施模型执行的完整规格。完成后由审查方按「第 8 章 验收标准」逐项检验。
> 基线：`main` @ `42d3957`（0.3.0-beta.1）。
> 范围：**只改视觉与布局**，不改功能、交互逻辑、快捷键、i18n 文案含义、配置格式。

---

## 0. 一句话目标

保留现有「仿 IDEA Island」的整体骨架（左活动栏 + 左工具岛 + 中间内容岛 + 右侧卡片岛 + 右工具条 + 状态栏），把**字号、尺寸、间距、圆角、图标、颜色状态、组件**收敛成一套设计规范，并让所有模块（主机、SFTP、数据库、Redis、Docker、端口、工具、常用命令、历史命令、AI、主机工具、设置、弹窗、菜单）都只通过公共组件和 token 渲染，看起来像同一个产品。

---

## 1. 现状诊断（问题清单与证据）

以下数字均为 `src/` 下实测（基线提交 `42d3957`）。

### 1.1 Token 形同虚设，字号全部写死
- `src/ui.rs:8-21` 已定义 `FS_CAPTION/FS_SECONDARY/FS_BODY/FS_TITLE/FS_PAGE`、`R_*`、`H_*`，但：
  - 业务代码里 `text_size(px(数字))` 字面量 **453 处**，使用 token 的只有约 68 处。
  - 字号分布：12px ×192、11px ×131、13px ×110、15px ×9、**10px ×9**、14px ×3、20px ×3。
- **「设置 › 外观 › 界面字号」实际上基本无效**：`main.rs:2068` 根节点按 `ui_font_size` 设了字号，但几乎所有子元素都覆盖成了写死的 11/12/13，只有 gpui-component 的输入框跟随（`main.rs:1814`）。
- 主要文本层级过小：侧栏副标题、状态栏、面板标题、卡片内容大量使用 11px；主机工具里出现 10px 按钮与表头（`host_tools_view.rs:225,232,263-276`）。

### 1.2 同类元素尺寸各不相同
| 元素 | 现状（文件:行） |
|---|---|
| 左侧列表行高 | 主机分组 30（`sidebar.rs:89 ROW_H`）、主机 min 32（`sidebar.rs:244`）、本地终端/位置 min 34（`sidebar.rs:299,408`）、标签行 28（`sidebar.rs:468`）、数据库树 28（`db_tree.rs:20`）、Redis/Docker 树 26（`kv_tree.rs:175`）、工具列表 26（`tools_ui.rs:62`） |
| 左侧行字号 | 主机/数据库 13，工具 12（`tools_ui.rs:67`），KV 分组 12 |
| 树缩进 | 主机 10/级（`sidebar.rs:244`），KV 16/级（`kv_tree.rs:207`） |
| 面板标题行 | 左侧 `panel_title` 28 高、11px 灰色大写（`activity.rs:278-284`）；**主机面板没有标题行**；右侧卡片 40 高、13px 半粗、强调色图标（`panels.rs:114`）；底部面板 30 高（`activity.rs:216`） |
| 弹窗标题 | 13px（`panels.rs:17`、`kv_ui.rs:157`、`dock_view.rs:498`）与 15px（`main.rs:1094`、`sidebar.rs:648`）混用；头部高 48、底栏背景 mantle 与无背景混用 |
| 工具栏高度 | 30 / 32 / 34 / 36 / 40 五种（`dock_view.rs:346,396,570`、`db_view.rs:1005,1073`、`workspace.rs:130`、`doc_view.rs:403`、`kv_view.rs:659`、`tools_view.rs:258`） |
| 图标按钮 | 18/20/22/24/26/28/32 七种尺寸，各处手写闭包（`panels.rs:110`、`tabs_ui.rs:259`、`library.rs:284`、`activity.rs:224,228`、`tools_ui.rs:43,126`、`sidebar.rs:380`、`dock_view.rs:379` …），没有公共 `icon_button` |
| 图标尺寸 | 字面量 201 处：16×31、14×26、12×25、13×24、11×11、15×9、17、18… |
| 圆角 | 字面量 116 处 + `rounded_md/lg()` 67 处；出现 2/3/4/5/6/7/8/10/12/14/18/22 等值；聊天气泡 18、输入框 22（`chat_view.rs:530,576`）与全局 10 的体系不符 |

### 1.3 颜色与状态不统一
- 悬停：49 处用 `surface0/surface1` 作悬停底色，33 处用 `theme::hover_bg()`，两种深浅并存。
- 选中：列表用淡强调色（`selected_bg`），而菜单悬停（`ui.rs:191`）、命令面板选中（`palette.rs:73`）、设置导航选中（`settings_view.rs:1270`）是**实心强调色 + 白字**的 macOS 风格，与 Island 的淡底风格冲突。
- 设置导航每项是彩色方块图标（`settings_view.rs:1278`），与其余单色图标体系冲突。
- 左侧实体图标颜色杂乱：本地终端 `info` 蓝、环境主机 `accent`、位置 `warning` 黄、分组文件夹 `accent` 或标签色、主机灰色（`sidebar.rs:255,302,321,410,419`）。
- 连接状态点两种画法：主机是图标右上角小点，本地终端/环境主机是图标旁独立圆点（`sidebar.rs:254` vs `411`）。
- 卡片边框：右侧卡片有 `surface0` 边框（`panels.rs:113`），左侧面板、中间卡片、底部面板没有。卡片内的子卡片又是 `base` 底 + 边框，层次过多。

### 1.4 字体
- 等宽字体三种写法：`font_family("monospace")` 9 处（`dock_view.rs:88` 等）、`config::get().font.family` 多处、部分命令文本直接用 UI 字体（常用命令预览、历史命令列表 `library.rs:169,270`）。
- 4 处把中文/英文标签 `to_uppercase()`（`activity.rs:282`、`palette.rs:67`、`sidebar.rs:546,609`），对中文无效，英文时又显得突兀。

### 1.5 常用命令 / 历史命令卡片（重点）
- 每行常驻 4–5 个 22px 图标按钮（插入/复制/编辑/复制一份/删除），窄卡片里按钮挤占命令文本（`library.rs:170-186,272-279`）。
- 命令文本用 UI 比例字体，命令可读性差；历史行的元信息 11px `overlay0`，对比度过低。
- 「新建」「清除本机历史」按钮放在列表底部常驻（`library.rs:93-99,112-114`），占高度；「仅显示 xx 模块」提示 11px 单独一行（`library.rs:84`）。
- 分组筛选 chip 与「本机/全部主机」切换都是同一种 chip，后者应是二选一的分段控件。
- 行悬停用 `surface0`，与左侧列表的悬停不同。

---

## 2. 设计原则

1. **岛（Island）是唯一容器**：左工具岛、中间内容岛、每张右侧卡片、底部面板，用同一个函数生成：同圆角、同底色、无边框。岛之间只靠外框底色与 6px 间距区分。
2. **一套字号，跟随设置**：所有字号都由 `appearance.ui_font_size`（默认 13）推导，改设置即全局生效。
3. **一种悬停、一种选中**：悬停 = `hover_bg()`，持久选中 = `selected_bg()`，键盘高亮（菜单/面板）= `menu_sel_bg()`。不再使用实心强调色 + 白字（主按钮除外）。
4. **图标分两类**：通用功能图标（按钮、菜单、工具栏、数据库 / Redis 树的实体图标）保持单色 `overlay1`，颜色只用于传达状态（连接点、标签点、危险/警告）。**主机列表与 SFTP 里的“实体”用填充图标**（`file_icon.rs`，多层 SVG）：文件夹 / 位置 = 默认蓝色两层填充（`theme::folder_tone(None)`，深浅色各一档，分组展开用 `folder_open_*`）；SSH 主机 / 本地终端 = 浅中性机身（`theme::entity_body`）加强调色指示灯 / 屏幕（`theme::entity_accent`）。分组、位置、SSH 主机可经右键「更换图标」自定义，见 §5.2「自定义图标」。品牌图标（数据库引擎 SVG、文件类型 SVG）同样除外。
   - 工具列表行与工具页标题左侧各有一枚工具图标：单色描边（Lucide）、`IC_MD`、颜色 `overlay1`，选中行跟随选中文字色 `text`；图标在 `ToolSpec.icon` / `tools::icon_of(id)` 里按工具定义，扩展包可选声明，缺省用通用图标。分类标题不带图标。
5. **安静的默认态**：行内操作按钮默认隐藏，悬停/选中时出现；标题不大写；次要信息用颜色而不是更小的字号区分。
6. **4px 网格**：间距只用 2/4/6/8/12/16/20/24/32。

---

## 3. 设计规范（Token）

全部集中在 `src/ui.rs`（尺寸/字号/组件）与 `src/theme.rs`（颜色）。业务文件**不得**再出现字号、圆角、图标尺寸字面量（见第 8 章 lint）。

### 3.1 字号（动态）

`B = appearance.ui_font_size.clamp(10, 20)`，默认 13。

| 函数 | 公式 | 默认值 | 用途 |
|---|---|---|---|
| `ui::fs_caption()` | B − 2 | 11 | **仅**用于：徽标/标签 `tag`、`kbd`、计数数字、状态点旁的极短标注 |
| `ui::fs_small()` | B − 1 | 12 | 次要信息（副标题、元信息、时间）、状态栏、分区标题、chip/紧凑按钮、表头 |
| `ui::fs_body()` | B | 13 | 正文：列表主文本、标签页标题、菜单、按钮、输入框、表单标签、面板/卡片标题（半粗） |
| `ui::fs_title()` | B + 2 | 15 | 弹窗标题 |
| `ui::fs_page()` | B + 7 | 20 | 设置页面大标题 |
| `ui::fs_mono()` | B − 1 | 12 | 列表/表格中的命令、SQL、路径、哈希（等宽字体） |
| `ui::fs_mono_block()` | B | 13 | 代码块、审批卡片里的命令、日志正文 |

- 禁止 10px 及以下；`fs_caption` 不得用于需要阅读的句子。
- 行高：`ui::lh(fs) = (fs * 1.4).round()`（13→18，12→17，11→15）。列表行内文本显式设置行高，避免 GPUI 默认 1.618 行高撑高行。
- 字重：正文 NORMAL；选中标签页、分组名 MEDIUM；面板/卡片/弹窗标题 SEMIBOLD；不使用 BOLD。
- 实现：`theme.rs` 增加 `static UI_SIZE: AtomicU32`（存 f32 bits，默认 13.0）与 `set_ui_size(f32)` / `ui_size()`；在 `main.rs:1814` 现有 `ui_size_seen` 分支中调用 `set_ui_size`，并 `cx.refresh_windows()`（或对所有窗口 notify），使 SFTP 独立窗口等也同步。原 `FS_*` 常量删除，所有引用改为函数。

### 3.2 文本颜色

| 层级 | 颜色 | 用途 |
|---|---|---|
| 主文本 | `th.text` | 列表主文本、标题、输入值 |
| 次文本 | `th.subtext` | 副标题、元信息、分区标题、表头、状态栏文字 |
| 提示文本 | `th.overlay1` | 空状态说明、占位提示、图标默认色 |
| 禁用 | `th.overlay0` | 仅禁用态 |

`overlay0` 不再用于可读的元信息（现 `library.rs:168,271` 等改为 `subtext`）。

### 3.3 间距与布局常量（`src/ui.rs`）

```rust
pub const SP_1: f32 = 4.;  pub const SP_2: f32 = 8.;  pub const SP_3: f32 = 12.;
pub const SP_4: f32 = 16.; pub const SP_5: f32 = 20.; pub const SP_6: f32 = 24.;
/// 列表外侧留白：行的悬停底色距岛边 6px。
pub const LIST_INSET: f32 = 6.;
/// 行内左右内边距。
pub const ROW_PX: f32 = 8.;
/// 面板内容（标题、说明文字）左右内边距 = LIST_INSET + ROW_PX，保证标题、搜索图标、行文本左对齐。
pub const PANEL_PX: f32 = 14.;
/// 树每级缩进。
pub const INDENT: f32 = 16.;
pub const INPUT_PX: f32 = 8.;
```

对齐关系（必须满足）：岛左边 → 6px → 行悬停底色左边 = 搜索框左边；→ 再 8px → 行文本/图标 = 搜索框放大镜图标 = 面板标题文字。即搜索行 `field_row()` 左右内边距改为 `LIST_INSET`。

### 3.4 尺寸（高度）

| 名称 | 值 | 用途 |
|---|---|---|
| `H_COMPACT` | 24 | chip、分段控件、紧凑按钮 `button_sm` |
| `H_DEFAULT` | 28 | 按钮、输入框、菜单项 |
| `H_LARGE` | 32 | 命令面板输入、弹窗主按钮 |
| `ui::row_h()` | `max(28, B + 15)` | 单行列表/树行 |
| `ui::row2_h()` | `lh(B) + lh(B−1) + 6`（默认 41→取整 40） | 双行列表行（主机、常用命令、历史命令、位置） |
| `SECTION_H` | 24 | 列表分区标题行（本地/连接/标签/收藏/最近…） |
| `PANEL_HEAD_H` | 36 | 所有岛的标题行（左工具岛、右侧卡片、底部面板） |
| `TOOLBAR_H` | 36 | 内容岛内部工具栏（数据库、Docker、Redis、文档、工具页） |
| `TABLE_HEAD_H` | 28 | 表头行 |
| `DIALOG_HEAD_H` | 48 | 弹窗头部 |
| `chrome::TAB_H` | 36 | 标签行（不变） |
| `TAB_PILL_H` | 28 | 标签胶囊（不变） |
| 状态栏 | 28 | 不变 |

行使用 `min_h(row_h())` 而不是 `h(...)` 时，需保证在 B=13 时视觉高度恰好为规定值；B 增大时行随之增高，不裁切文字。

### 3.5 圆角

| 名称 | 值 | 用途 |
|---|---|---|
| `R_SM` | 4 | kbd、tag、行内小图标按钮（20px） |
| `R_MD` | 6 | 列表行、按钮、输入框、图标按钮、菜单项、chip 以外的小块 |
| `R_SUB` | 8 | 岛内子卡片（SQL 编辑器、主机工具分卡、表单分组） |
| `R_LG` | 10 | 岛、弹出菜单、弹窗、命令面板、toast |
| `R_XL` | 12 | AI 聊天气泡、AI 输入框 |
| `rounded_full()` | — | 状态点、chip、头像、进度条 |

`card_layout::CARD_RADIUS`/`SUB_RADIUS` 改为引用 `R_LG`/`R_SUB`（`card_layout.rs` 的单元测试保持通过）。

### 3.6 图标

| 名称 | 值 | 用途 |
|---|---|---|
| `IC_XS` | 12 | 展开箭头、行内操作按钮图标、状态栏图标 |
| `IC_SM` | 14 | 标题行/工具栏图标按钮、标签页类型图标、面板标题图标 |
| `IC_MD` | 16 | 列表实体图标（主机、文件夹、表…）、菜单项图标、弹窗标题图标 |
| `IC_LG` | 18 | 活动栏/右工具条图标 |
| `IC_XL` | 28 | 空状态插图 |

**图标按钮**（新组件 `ui::icon_button`）：

| 档位 | 盒子 | 图标 | 圆角 | 用途 |
|---|---|---|---|---|
| `IconBtn::Sm` | 20 | 12 | R_SM | 列表行内操作（悬停显示） |
| `IconBtn::Md` | 24 | 14 | R_MD | 岛标题行、工具栏、pane 悬浮头 |
| `IconBtn::Lg` | 28 | 16 | R_MD | 标题栏（侧栏开关、新建标签、设置齿轮） |
| `IconBtn::Strip` | 32 | 18 | R_SUB | 左活动栏、右工具条 |

统一状态：默认透明、图标 `overlay1`；悬停 `hover_bg()` + 图标 `text`；按下/激活 `selected_bg()` + 图标 `accent`；危险操作（删除/清空）悬停时图标 `error`、底色 `error.opacity(0.12)`；必须带 tooltip。

### 3.7 颜色与状态（`src/theme.rs` 新增）

```rust
/// 弹出菜单 / 命令面板 / 下拉的键盘高亮行：强调色与卡片底色 30% 混合的不透明色，文字保持 text。
pub fn menu_sel_bg() -> Hsla
/// 统一分隔线 / 发丝边框。
pub fn divider() -> Hsla        // c(cur().surface1).opacity(0.6)
/// 岛内子卡片底色（比卡片略深/略亮一层）。
pub fn sub_bg() -> Hsla          // c(cur().text).opacity(0.035) 叠在 card_bg 上
/// 危险操作悬停底色。
pub fn danger_bg() -> Hsla       // c(cur().error).opacity(0.12)
```

- 悬停只用 `hover_bg()`；持久选中只用 `selected_bg()`（文字保持 `text`，可加 MEDIUM）。
- 菜单项悬停、命令面板选中、新建菜单选中、历史下拉选中、选择框下拉选中 → `menu_sel_bg()` + `text`，图标 `text`。危险菜单项悬停 → `danger_bg()` + `error`。
- 主按钮（`button` kind=1）仍为实心强调色，这是唯一允许的大面积强调色。
- 分隔线、表格行线、弹窗头尾分隔统一 `divider()`。
- 岛（左/中/右/底）**都不加边框**；子卡片 `sub_bg()` + `divider()` 边框。

### 3.8 字体

- `ui::mono_family() -> SharedString`：返回 `config::get().font.family`（终端字体），所有等宽场景统一使用，删除 `font_family("monospace")`。
- UI 字体保持 `theme::ui_font()`（macOS `.SystemUIFont`）。
- 所有面向用户的标签不做 `to_uppercase()`。

---

## 4. 公共组件（`src/ui.rs` / `src/card_layout.rs`）

实施模型须先完成本章组件，再改业务视图。签名可按 GPUI 实际类型微调，但能力与规格必须一致。每个组件附一行中文注释说明用途，风格与 `ui.rs` 现有注释一致。

| 组件 | 规格 |
|---|---|
| `card_layout::island(id) -> Stateful<Div>` | `rounded(R_LG)`、`bg(card_bg())`、`overflow_hidden`、`flex_col`、无边框。左工具岛（`sb_frame`）、中间内容岛（`content_card`）、右侧每张卡片（`dock_frame`）、底部面板（`bottom_panel`）全部改用它。`content_card` 本身不带内边距（通栏条如终端路径栏可贴满左右边缘）；模块内容区自带 `CARD_PAD` 间距（`main.rs` 的内容包裹层、`tools_view` 的工具卡片）。 |
| `ui::panel_header(title, subtitle: Option, icon: Option, actions) -> Div` | 高 `PANEL_HEAD_H`，左右 `PANEL_PX`（右侧为 `PANEL_PX − 4` 以便图标按钮视觉对齐），标题 `fs_body` SEMIBOLD `text`，可选图标 `IC_SM` `overlay1`（非强调色），可选副标题 `fs_small` `subtext` 跟在标题后（`·` 分隔），右侧 actions 为 `IconBtn::Md`，间距 2。替代 `activity::panel_title`、`dock_frame` 头部、`bottom_panel` 头部。 |
| `ui::section_header(id, label, count: Option<usize>, collapsed: Option<bool>) -> Stateful<Div>` | 高 `SECTION_H`，`mx(LIST_INSET)`、`px(ROW_PX)`，可选展开箭头 `IC_XS`，文字 `fs_small` SEMIBOLD `subtext`，右侧计数 `fs_caption` `overlay1`。返回 Stateful 以便调用方挂点击/拖放。替代 `sidebar::section_header`、`tools_ui` 的 `head`、`kv_tree.rs:175` 分组头。 |
| `ui::list_row(id, depth, selected) -> Stateful<Div>` | `min_h(row_h())`、`mx(LIST_INSET)`、`pl(ROW_PX + INDENT*depth)`、`pr(4)`、`rounded(R_MD)`、`gap(6)`、`items_center`、`cursor_pointer`；`selected` → `selected_bg()`，否则悬停 `hover_bg()`。双行行用 `.min_h(row2_h())` 覆盖。 |
| `ui::row_title(text) / ui::row_meta(text) -> Div` | 单行省略文本：title `fs_body`/`lh` `text`；meta `fs_small`/`lh` `subtext`。替代各处重复的 `div().text_size(..).line_height(..).whitespace_nowrap().overflow_hidden().text_ellipsis()` 链。 |
| `ui::row_actions(group) -> Div` | 行尾操作容器：默认 `invisible()`，`group_hover(group)` 时可见；选中行常显。内部为 `IconBtn::Sm`。 |
| `ui::icon_button(id, icon, size: IconBtn, tip) -> Stateful<Div>` | 见 3.6。另提供 `.active(bool)` / `danger` 变体（参数或 builder 均可）。替代所有手写图标按钮（含 `Root::strip_btn`、`library::row_icon`、`tabs_ui::pane_header` 的 `btn`、`panels::dock_frame` 的 `btn`、`activity` 底部面板按钮、`sidebar` 的 `new-host`、`tools_ui` 的扩展入口与返回、标题栏 `sb-collapse`/`new-tab`/`title-settings`、`dock_view` 的 `dk-more`）。 |
| `ui::button(id, label, kind)` | 保留；高 `H_DEFAULT`，文字改 `fs_body` MEDIUM（原 12）。 |
| `ui::button_sm(id, label, kind)` | 新增；高 `H_COMPACT`、`px(8)`、`fs_small`。替代所有 `ui::button(..).h(px(18|22|24)).text_size(px(10|11))` 的写法（`host_tools_view.rs`、`panels.rs:179`、`panels.rs:58`）。 |
| `ui::chip(id, label, on) -> Stateful<Div>` | 高 `H_COMPACT`、`px(10)`、`rounded_full`、`fs_small`；off：`hover_bg()` 底 + `subtext`；on：`selected_bg()` + `accent` 文字 MEDIUM；悬停加深一层。替代 `library::chip`、审计面板的 chip（`panels.rs:58`）。 |
| `ui::segmented` | 保留；文字 `fs_small`，选中项底色改为 `card_bg()` + 轻阴影（与标签胶囊一致），容器底 `hover_bg()`、无边框。 |
| `ui::tag` | 保留；`fs_caption`、`R_SM`。标签页上的 Agent 状态徽标改用它（`main.rs:1897`）。 |
| `ui::toolbar() -> Div` | 高 `TOOLBAR_H`、`px(8)`、`gap(4)`、`items_center`、`flex_shrink_0`、底部 `divider()` 发丝线、**无底色**。替代 Docker/Redis/数据库/文档/工具页里的 30/32/34/36/40 高、`mantle` 底色头部。 |
| `ui::table_header() -> Div` | 高 `TABLE_HEAD_H`、`fs_small` SEMIBOLD `subtext`、底 `hover_bg()`、底部 `divider()`。替代审计列表头、Docker 容器分组头、进程表头等。 |
| `ui::empty_state(icon, title, desc: Option, actions: Vec<AnyElement>) -> Div` | 垂直居中列：图标 `IC_XL` `overlay0`，标题 `fs_body` MEDIUM `subtext`，说明 `fs_small` `overlay1` `max_w(260)` 自动换行，按钮纵向全宽排列（窄面板）或横向（宽区域，参数控制）。替代侧栏空状态卡片（`sidebar.rs:430`）、`activity::empty_tool`、各列表 "empty" 文本、底部面板空态、`ws_empty`、`panels::sftp_panel` 空态。 |
| `ui::notice(kind, text) -> Div` | 行内提示条：`mx(LIST_INSET)`、`px(ROW_PX)`、`py(6)`、`R_MD`、`fs_small`；kind=info/warn/error → 底色 `info/warning/error.opacity(0.10)`、文字对应色。替代 `lib_notice`、"需要终端" 提示、Docker/DB 的错误条。 |
| `ui::dialog_header(icon, icon_color, title, subtitle, on_close)` / `ui::dialog_footer(children)` | 头部高 `DIALOG_HEAD_H`、`px(SP_5)`、图标 `IC_MD`（颜色由 `icon_color` 传入：默认 `accent`，确认类 `warning`，删除类 `error`）、标题 `fs_title` SEMIBOLD、副标题 `fs_small` `subtext`、右侧关闭 `IconBtn::Lg`、底部 `divider()`；底栏 `px(SP_5) py(SP_3)`、顶部 `divider()`、**无底色**、按钮右对齐 gap 8、危险按钮靠左。替代 `panels::panel_shell`、`host_dialog.rs:319`、`kv_ui.rs:155`、`db_dialog.rs:253`、`port_view.rs:133`、`sftp_view.rs:1359`、`chat_view.rs:614`、`dock_view` 的创建/exec/确认卡片、`main.rs` 的 hostkey/auth/approval/quit/update 卡片、`sidebar::sb_modal_el`。 |
| `ui::swatch_row(base, colors, selected, on)` / `ui::icon_grid(base, cells, selected, cols, on)` | 更换图标对话框的共用件：一排圆形色板（选中描边）、等宽图标网格（选中底色）；不含业务逻辑，选中值由调用方传入。 |
| `ui::menu_item` / `ui::menu_sep` / `ui::menu_label(text)` | 菜单项悬停改 `menu_sel_bg()`（见 3.7），文字 `fs_body`，图标 `IC_MD`；新增 `menu_label` 作为菜单内分组小标题（`fs_small` SEMIBOLD `subtext`，不大写），替代 `sidebar.rs:546,609` 的大写标签。 |

---

## 5. 各区域详细规格

### 5.1 外框（窗口骨架）—— 保持结构，只统一细节

```
┌ 左列 ───────────────────────┬ 右列 ──────────────────────────────────────────────┐
│ [●●● ShellAX]   (TAB_H 36)  │ [⊟][tab][tab][+]            ……拖动区……   [⚙][窗口按钮] │
├────┬────────────────────────┼─────────────────────────────────┬──────────────┬────┤
│活动│ 左工具岛 island         │ 中间内容岛 island                │ 右侧卡片岛 ×N │右工│
│栏40│  panel_header 36        │  (终端/SFTP/DB/…)                │ panel_header │具条│
│    │  搜索                   │                                  │ …            │ 40 │
│    │  列表                   ├──────────────────────────────────┴──────────────┤    │
│    │                        │ 底部面板 island（传输/执行日志）                    │    │
├────┴────────────────────────┴──────────────────────────────────────────────────┴────┤
│ 状态栏 28                                                                           │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

- **标签行**（`main.rs:1836-1960`）：标签文字 12 → `fs_body`；类型图标 13 → `IC_SM`；关闭按钮保持 18 盒子、图标 `IC_XS`；Agent 徽标用 `ui::tag`；`sb-collapse`、`new-tab`、`title-settings` 用 `IconBtn::Lg`。设置标签页同步。
- **左上品牌行**（`main.rs:2031-2034`）：保持；「ShellAX」`fs_body` SEMIBOLD。
- **活动栏 / 右工具条**（`activity.rs:131-160`、`sidebar.rs:495-526`）：`strip_btn` 内部改为 `icon_button(.., IconBtn::Strip, ..)`；图标 17 → `IC_LG`；激活态 `selected_bg()` + `accent` 图标；右工具条上下两组之间加一条 16px 宽的 `divider()` 短线。
- **状态栏**（`main.rs:2005-2027`）：文字 11 → `fs_small`；图标 12 → `IC_XS`；可点项高 20 → 22、`R_MD`；项之间 gap 12 不变；颜色保持 `muted_text()`（= `subtext` 或 `overlay1`，二者择一并在 `theme.rs` 注释说明）。
- **岛**：`sb_frame`、`content_card`、`dock_frame`、`bottom_panel` 全部基于 `card_layout::island`，去掉右侧卡片的 `surface0` 边框。拖动调整宽度/高度的手柄、动画逻辑原样保留。

### 5.2 左工具岛（所有模块统一结构）

```
island
├─ panel_header：模块名（与活动栏 tooltip 同名）+ 右侧操作按钮（IconBtn::Md）
├─ 搜索行：field_row()（左右 LIST_INSET）+ search_field，下方 8px
└─ 可滚动列表（pb 8）：section_header / list_row …
```

#### 主机 / SFTP（`sidebar.rs`）
- 新增 `panel_header("主机"/"SFTP")`，原搜索框右侧的「+」移到标题行 actions（`IconBtn::Md`，`Plus`，点击仍弹出原菜单）。搜索框独占一行。
- 分区：`本地`、`连接`、`标签` 用 `ui::section_header`；`连接`/`标签` 可折叠（折叠状态可只存内存，不改配置格式）；`连接` 右侧显示主机总数。拖放到「连接」标题移到顶层的逻辑保留。
- **主机行**：`list_row` + `min_h(row2_h())`；图标 `file_icon::host_icon(IC_MD, icon, color)`（默认填充服务器：浅中性机身 + 强调色指示灯；可自定义）；状态点 7px 位于图标右上角、带 `card_bg()` 描边（不是 `crust`；指示灯在机身左侧，不被状态点遮住）；生产环境盾牌保留在图标右下；第一行 `row_title(name)` + 最多 3 个标签点（6px）+ `+N`（`fs_caption`）；第二行 `row_meta("user@host:port · OS")`；编辑按钮放入 `row_actions`（`IconBtn::Sm`）。缩进 `INDENT` 16/级，展开箭头占 16px 宽槽位以对齐。
- **本地终端行 / 环境变量主机行**：与主机行完全同构（图标 `file_icon::local_icon`（macOS 笔记本 / 其他平台显示器）/ `server_icon` 填充图标 + 右上角状态点），去掉独立圆点与 `info`/`accent` 图标色。
- **分组行**：`list_row` 单行；箭头 `IC_XS`；文件夹图标 `file_icon::folder_icon_styled(IC_MD, open, color, glyph)`（默认蓝色填充，折叠闭合、展开用打开态；不再按标签着色；可自定义颜色与字形）；名称 `fs_body` MEDIUM；有标签时名称后跟 8px 标签点；右侧计数 `fs_caption` `overlay1`。
- **位置行（places）**：双行同主机行；图标用同一个填充文件夹（闭合态，无徽标，默认蓝色，可自定义颜色与字形），靠第二行路径与分组行区分。
- **自定义图标（更换图标）**：分组行、位置行、SSH 主机行的右键菜单各有「更换图标…」（`IconName::Palette`），打开小对话框 `SbModal::ChangeIcon`：顶部实时预览（`IC_XL` 图标 + 名称）、一排色板 `ui::swatch_row`（`SWATCH` 22px）、图标网格 `ui::icon_grid`（6 列，`ICON_CELL` 40px）、左下「恢复默认」、右下「完成」。每次点选立即写入配置并刷新侧栏。
  - 文件夹（分组 / 位置）：8 色（蓝默认、绿、橙、红、紫、青、灰、黄），色值在 `theme::ICON_TONES`（每色浅 / 深主题各一对 back / front + 字形色），经 `theme::folder_tone` 取用；12 个字形（默认纯文件夹、代码、分支、文档、数据库、云、星标、锁、主页、下载、图片、终端），作为第三层 SVG（`assets/icons/files/glyph_*.svg`）叠在文件夹前片中央；分组仍保留展开 / 折叠两态。
  - SSH 主机：11 个图标（服务器默认、云主机、笔记本、台式机、路由 / 网关、数据库服务器、容器、单板机，以及 Linux、Windows、Mac 三个自绘几何标记，非官方标志）；同为两层填充（机身 `entity_body` + 强调部分 `entity_accent`）；色板只改强调部分（`theme::host_accent`），不选即用主题强调色。
  - 存储：`GroupEntry`、`Place`、`HostEntry` 各新增可选字段 `icon`、`icon_color`（serde 默认 + 为空不写出），旧配置无需迁移；未知 id 渲染回默认。id 与标签表在 `iconset.rs`，中英文名称键为 `icon.color.*`、`icon.glyph.*`、`icon.host.*`。
  - 入口除右键「更换图标…」外，还有三处编辑对话框里的「图标」栏（共用 `ui::icon_picker`：预览 + 色板 + 图标网格 + 恢复默认）：连接编辑对话框（`host_dialog`）、新建 / 重命名分组、重命名主机 / 位置与保存位置（`SbModal::Prompt` 的暂存 `icon`）。对话框里的选择先暂存，点「保存」时与名称一起写入；右键入口仍是即时保存。
  - 不变：状态点、生产盾牌、标签色点；不支持导入自定义图标。仅左侧栏行套用，SFTP 主机选择器与标签页图标仍用默认图标。
- **标签行**：`list_row` 单行；8px 色点 + 名称 + 生产盾牌 + 计数。
- 空状态改 `ui::empty_state(Server, "还没有连接", 说明, [新建连接(主), 导入 ~/.ssh/config])`。无匹配时 `empty_state(Search, "没有匹配的主机", None, [])`。
- 右键菜单内「移动到」小标题改 `ui::menu_label`，不大写。

#### 数据库（`db_ui.rs`、`db_tree.rs`）
- `panel_title` → `panel_header`；搜索行同规范。
- `db_tree.rs:20-22`：`ROW_H` → `ui::row_h()`，`TEXT` → `fs_body`，`ICON` 18 → `IC_MD`；缩进统一 `INDENT`；引擎 SVG 图标保留彩色/灰色逻辑；计数、加载中、错误等说明文字用 `fs_small` `subtext`/`error`。

#### Redis / Docker（`kv_ui.rs`、`kv_tree.rs`）
- `panel_title` → `panel_header`；行高 26 → `row_h()`；`kv_tree.rs:175` 分组头 → `section_header`；`note` 行（`kv_tree.rs:207`）→ `fs_small` + `INDENT` 缩进；图标 14/15 → `IC_MD`。

#### 端口（`activity::empty_tool`）
- 改为 `panel_header` + `empty_state(ArrowLeftRight, 描述, [打开端口转发(主)])`。Docker 的同类入口同理。

#### 工具（`tools_ui.rs`）
- `panel_title` → `panel_header`（扩展入口、返回按钮用 `IconBtn::Md`）。
- 行高 26 → `row_h()`；文字 `FS_SECONDARY` → `fs_body`；收藏星标按钮 → `row_actions` 内 `IconBtn::Sm`（已收藏时常显）；分组头 `head` → `section_header`；未安装扩展的 ghost 行文字用 `overlay1`，不再叠加 `opacity(0.6)`，下载按钮用 `button_sm` kind=3。

#### 工具结果列表（`result_list.rs`、`tools/history.rs`）
- 结果以“行”为单位，不再整块塞进一个编辑器：`result_list::result_row`（标签 / 内容 / 徽标 / 逐条复制 / 悬停删除，高度 `row_h()`，间距走 `LIST_INSET`、`ROW_PX`）、`result_list::batch_head`（`SECTION_H` 高，含折叠、遮住、复制本批、删除本批）。
- 生成类工具（密码、UUID、随机数、随机分组）的结果按批次保存在 `Root.tool_hist`（`HashMap<工具 id, History>`），会话级、不写盘，关闭标签后仍在；每个工具最多 20 批 / 500 行，超出丢最旧的。
- 多格式结果（单位换算、哈希、进制、颜色等）使用键值行（`RowMode::Kv`），主结果行用 `fs_title` 加粗，分组标题行用 `section_header`。
- “清空输入”与“清空历史”是两个按钮，都不重置选项；生成类改选项不会自动重新生成。

#### 校验类工具的判定横幅（`result_list::verdict_banner`、`tools::Verdict`）
- 校验类工具（身份证、正则、Cron、JSON）的结果顶部是一条横幅：淡底色块（`tone.opacity(0.10)`）+ 状态图标 `IC_SM` + 一句话结论（`fs_body` MEDIUM），颜色走 `tone_color`：通过 `success`、失败 `error`、无匹配等提示 `warning`；下面是明细键值行，不再重复一个“结果”主行。
- 失败不再冒泡成顶部错误条：表达式写错、JSON 语法错误、格式不对都用 `Output::fail(横幅, 明细行)`；只有输入还没填时才走错误条。
- 正则结果分“匹配 / 替换”两页（`ui::segmented`）：匹配页是原文预览（`mono_family`，匹配片段 `accent` 底高亮，最多取前 4000 字节）+ 逐处匹配行（带位置标签）与捕获组行（最多 200 处）；替换页是替换后的全文，没填替换内容时给出提示。复制 / 保存取当前页。
- JSON 错误给出“第 L 行第 C 列”，并把出错行（标红）及前后各一行作为上下文行；超长行只截取出错位置附近。

#### 文件结果卡片与图片预览（`result_list::file_card`、`tools::fileinfo`）
- 工具输出是文件时（PDF、压缩包、图片），输出区不再是文本编辑器，而是：文件卡片（类型图标 `IC_LG`、文件名 `fs_body` MEDIUM、说明 `fs_small`：大小 · 像素尺寸 · 页数 · 压缩包文件数）+ 右侧“保存”（主按钮）和“在文件夹中显示”（保存之后才出现，`button_sm`）。
- 图片结果（二维码、条形码、图片压缩、水印）直接从内存预览：`gpui::Image::from_bytes` + `gpui::img`，不落盘；在剩余空间里等比缩放（`ObjectFit::Contain`），框的宽高再以图片原始像素为上限，所以小图不会被放大；放在 `R_MD` 圆角、发丝线边框、`hover_bg` 底的框里。PDF 转图片是 zip，预览取包内第一张 PNG 并注明。
- 图片压缩在卡片下给出“压缩前 → 压缩后，节省 x%”（变大时用 `warning` 色）；大小、尺寸、页数、文件数都由宿主从输出文件自身读出（`tools::fileinfo::inspect`），扩展包无需改动。
- Markdown 转 HTML 分“预览 / 源码”两页：预览用 `gpui_component::text::TextView::markdown` 渲染输入，源码页是生成的 HTML，复制与保存始终是 HTML。

#### 文本类工具的输入输出辅助
- 多行输入框的标签行右侧、输出卡片标题行里各显示“N 行 · M 字符”（`fs_caption`，`overlay1`）；输出为文本（非文件、非按行结果）时标题行多一个“用结果替换输入”按钮（`IconName::ArrowLeftRight`），把当前结果放回第一个输入框并重新运行，方便加密后再解密这类来回操作；Markdown 转 HTML 不显示。
- 加解密（AES / DES / RC4 / Rabbit）改为手动运行（`live = false`），输入过程中不再逐字符报“密钥不对”之类的错误；已有结果时改选项仍会自动重算。
- HMAC、人民币大写、批量时间转换使用键值行（可逐条复制，批量时间最多 300 行，超出退回纯文本；复制全部仍是只含结果的逐行文本）。

#### 工具箱输入 / 输出的高度（`tools_layout.rs`、`tools_view.rs`、`tools::InSize`）
目标：输入区只占它需要的高度，剩下的给输出；大文本进、大文本出的工具才对半分，并且分配可以拖动。

**输入高度（`Field::size()`）**
| 形态 | 条件 | 高度 |
|---|---|---|
| `Line` | 单行字段（`Text` / `Number`） | `ui::text_field` 的固定行高 |
| `Auto(min, max)` | 多行字段带 `rows` 提示 | `TextareaState::auto_grow(min, max)`，软换行也计入，超出 max 行后框内滚动 |
| `Fill` | 多行字段没有 `rows`（大文本，包括旧扩展包） | 占剩余空间，卡片最小高度由 `tools_layout::in_min_h` 按字段算出 |
| `Zone` | `File` / `Files` | 拖放区最小 `FILE_ZONE_MIN_H`（64），文件多了随列表增高、到 `FILE_ZONE_MAX_H`（160）后区内滚动 |
- 内置工具：短输入用 `Field::auto(min, max)`——哈希、HMAC、加解密（AES / DES / RC4 / Rabbit）、求和列表、随机分组为 3–10 行，命名转换 2–8 行，工作日的节假日 / 调休 2–6 行；JSON、SQL、文本转换、Base64 等保持 `Fill`。
- 扩展包：清单字段可选 `"rows": [min, max]`（SDK：`with_rows`），旧宿主忽略。已发布而未重新打包的包由宿主按 (工具 id, 字段 id) 补充：`tools::PACK_ROWS`（二维码内容 2–8 行，世界时钟 / 时区换算 3–10 行，拼音与繁简转换保持大文本）。清单自带的 `rows` 优先于这张表。

**输出高度**
- 占满剩余空间：文本、图片预览（带预览的文件）、历史列表、批量时间列表、正则结果；输出卡片最小 `OUT_FILL_MIN_H`（120）。
- 贴合内容（`tools::out_fit`）：键值行结果、没有预览的文件卡片（PDF 类）。卡片高度随内容，放不下时卡片内部滚动（最小 `fit_min_h()`），下方留白；**出结果前不画输出卡片**（出错时仍显示错误条）。
- 图片预览框占满输出卡片剩余高度，图片等比缩放且不超过原始像素。

**输入与输出共用高度**
- 输入有 `Fill` 字段时，输出一定占满剩余空间，两者按 `Sidebar.tools_split` 分配（输入占的份额，默认 0.5，范围 0.2–0.8，所有工具共用一个值）。
- 上下排列时分隔条是输入卡片与下一张卡片之间的 `CARD_PAD` 间隙（`tools_layout::handle`）：悬停 `accent` 35% 高亮、光标 `cursor_row_resize`、拖动换算 `split_ratio`（指针位置减去选项卡片与间隙后的比例）、双击复位 0.5；拖动中使用本地值，松手后保存到配置（做法同右侧卡片的 `dock_split`）。
- 工具区宽度 ≥ `SIDE_MIN_W`（880）且两边都是大文本时左右并排：选项卡片通栏在上，下面左输入右输出，中间是竖向分隔条（`cursor_col_resize`），两栏最小 `SIDE_PANE_MIN_W`；宽度由画布测量，跨过阈值才重新渲染。
- 窗口矮到放不下各区最小高度时，整个工具区（`tool-body`）纵向滚动，而不是继续压缩。
- 新工具的做法：多行输入默认是大文本；输入通常只有几行的就加 `.auto(min, max)`；输出是键值行的把 id 加进 `tools::out_fit`。尺寸只用 `tools_layout` 的常量与 `ui` / `card_layout` 的 token，不写字面量。

### 5.3 右侧卡片岛

#### 卡片框架（`panels.rs::dock_frame`）
- 头部改 `panel_header(title, subtitle, icon, actions)`：高 40 → 36；图标 15 强调色 → `IC_SM` `overlay1`；折叠/关闭按钮 `IconBtn::Md`；折叠态整行可点击展开。
- 去掉边框（与其他岛一致）。卡片间距、拖动分隔、动画保持。

#### 常用命令卡片（`library.rs`，snippets_mode = true）

```
┌ ⌘ 常用命令                                [+] [⌄] [×] ┐  ← panel_header；无副标题：列表在所有模块下相同，不按模块过滤
│ [🔍 搜索…                                         ]   │  ← field_row
│ (全部) (部署) (排障) (未分组)  → 单行，横向滚动         │  ← ui::chip，不换行
│ ⓘ 需要先打开终端                                       │  ← ui::notice(info)，仅 !ready 时
│ ┌──────────────────────────────────────────────────┐ │
│ │ 重启 nginx            部署                [↵][⧉][⋯]│ │  ← 行：row2_h；名称 fs_body MEDIUM；分组 ui::tag
│ │ sudo systemctl restart nginx                       │ │  ← 预览：mono_family + fs_mono + subtext
│ └──────────────────────────────────────────────────┘ │
│ …                                                     │
└───────────────────────────────────────────────────────┘
```
- 「新建」按钮移到标题行（`Plus`，tooltip「新建」）；底部按钮行删除。列表为空时显示 `empty_state(Code, "还没有常用命令", 说明, [新建(主), 添加示例])`；示例是终端 / SQL / Redis / Docker 合并的一组，仅在整份列表为空时出现。插入位置由焦点 pane 决定（SQL 编辑器 / Redis 控制台 / 就绪终端），都没有时复制到剪贴板并提示。
- 行内操作：`row_actions`，默认隐藏，悬停显示。顺序：插入 `CornerDownLeft`、复制 `Copy`、更多 `Ellipsis`。「更多」弹出小菜单（`ui::popover` + `menu_item`）：编辑、复制一份、分隔、删除（danger）。
  - 如果实现菜单成本过高，可退而求其次：5 个按钮全部放进 `row_actions`（仍悬停才显示）。须在提交说明里写明选择了哪种。
- 点击行 = 执行（保持现有行为）；行悬停 `hover_bg()`（替换 `surface0`）。
- 编辑表单（`snippet_editor`）：`px(PANEL_PX)`、字段间距 12、标签 `fs_small` `subtext`、正文 `Textarea` 使用 `mono_family()`、最小高度 120；按钮行右对齐（取消 / 保存）。
- 变量填写表单、确认表单：同上排版；确认原因列表用 `ui::notice(warn)` 包裹。
- `lib_notice` → `ui::notice(warn)`。

#### 历史命令卡片（`library.rs`，snippets_mode = false）

```
┌ 🕘 历史命令                                   [🗑] [⌄] [×] ┐  ← 清除本机历史移到标题行（危险图标按钮，tooltip 原文案）
│ [🔍 搜索…                                         ]     │
│ [   本机   |  全部主机   ]                               │  ← ui::segmented，占满宽度
│ ┌────────────────────────────────────────────────────┐ │
│ │ kubectl get pods -n prod                 [↵][⧉][⋯] │ │  ← 命令：mono_family + fs_mono + text，单行省略
│ │ prod-web · 2026-10-26 18:22                         │ │  ← 元信息：fs_small + subtext（不再是 overlay0/11px）
│ └────────────────────────────────────────────────────┘ │
└────────────────────────────────────────────────────────┘
```
- 「更多」菜单：保存为常用命令、分隔、删除（danger）。
- 时间格式保持；在「本机」模式下只显示时间。
- 空态 `empty_state(Clock, "暂无历史命令", None, [])`。

#### AI 卡片（`panels.rs::ai_dock`、`chat_view.rs`）
- Agent 状态条：文字 11 → `fs_small`；快捷启动按钮改 `button_sm` kind=0。
- 聊天气泡圆角 18 → `R_XL`；用户气泡底 `selected_bg()`，助手气泡底 `sub_bg()`；正文 `fs_body`，备注 `fs_small` `subtext`。
- 输入框（composer）圆角 22 → `R_XL`，底色 `sub_bg()`，边框 `divider()`；上下文 chip、快捷 chip 改 `ui::chip`；发送按钮 32 圆形保留（允许 `rounded_full`）。
- 计划步骤块：底色 `sub_bg()`、`R_MD`、命令 `mono_family` `fs_mono`。
- `chat_view.rs:614` 独立弹窗模式头部 → `dialog_header`。

#### 主机工具卡片（`host_tools_view.rs`）
- 子卡片 `card()`：`sub_card()` 改为 `sub_bg()` + `divider()` 边框 + `R_SUB`；标题 `fs_small` SEMIBOLD → `fs_body` SEMIBOLD；标题图标 `IC_SM` `overlay1`。
- 所有 11px/10px 文本 → `fs_small`（数值）/`fs_caption`（仅表头单位、百分号列）；进程表用 `mono_family` + `fs_mono`；表头用 `table_header` 风格。
- 行内操作按钮（kill/日志/重启/停止）`h(18) + 10px` → `button_sm`。
- 分页 tab（`ht-tab`）→ `ui::segmented`。

### 5.4 中间内容岛

- **pane 悬浮头**（`tabs_ui.rs:257-280`）：按钮 → `IconBtn::Md`；容器底 `card_bg()`、边框 `divider()`、`R_SUB`。
- **路径栏 cwd bar**（`main.rs:1705`）：高 28 保持；文字 `fs_small`；分隔线 `divider()`。每段用共用的 `ui::crumb` / `ui::crumb_sep`（SFTP 路径栏同一套）：高 `H_COMPACT`，图标（`IC_XS`）与文字同行居中、间距 `SP_1`；文字走 `ui::aligned_text`（显式行高 `lh(fs_small)`，并按真实字体度量上提 `ui::text_lift`，补偿 GPUI 把 ascent + descent 整体居中造成的字形偏低）；分隔箭头在同高居中盒里；根目录只画磁盘图标（`HardDrive`），不显示「/」，其余段用线框文件夹图标。本地面板始终显示路径栏：目录取 OSC 7 / OSC 9;9（PowerShell、cmd 提示符注入）→ 进程工作目录（Linux `/proc`，Mac / Windows sysinfo）→ 主目录；右侧显示本机 CPU / 内存 / 磁盘（Windows 无负载）。通栏贴满内容岛左右边缘，底部圆角 `CARD_RADIUS`。
- **空工作区**（`ws_empty`）→ `empty_state`（横向按钮）。
- **数据库视图**（`db_view.rs`）：工具栏 36/34 → `toolbar()`；结果表头 `table_header`；单元格 `mono_family` + `fs_mono`；SQL 编辑器/结果子卡片 `R_SUB`；错误条 `notice(error)`。
- **Redis 视图**（`kv_view.rs:659`）、**Docker 视图**（`dock_view.rs:346,396,570,679`）、**文档视图**（`doc_view.rs:403`）、**工具页**（`tools_view.rs:258`）、**工作区头**（`workspace.rs:130`）：头部统一 `toolbar()`；分组头 `table_header`；`small()/mono()` 辅助函数改为调用 `ui` 组件；弹出表单 → `ui::card` + `dialog_header/footer`。
- **SFTP**（`sftp_view.rs`）：工具栏 `toolbar()`；行高与文件图标尺寸保持（26/18 已单独调过，改为引用 `row_h()` 与 `IC_MD` 时若视觉变化须在提交说明注明）；弹窗头 `dialog_header`。
  - 左右留白只来自工作区包裹层的 `CARD_PAD`，SFTP 自身不再叠加内边距：标题栏（`dialog_header(...).px_0()`）、双栏面板卡片、传输队列、状态行、编辑/确认/同步条的左右边缘都落在同一条竖线上，标题图标左边缘 = 面板卡片左边缘，右侧同理。
- **终端内浮层**（`termview.rs:1623,2252-2380`）：连接中卡片、错误卡片、AI 建议卡片里的手写按钮 `btn` 闭包 → `ui::button` / `button_sm`；卡片 → `ui::card` 风格（`R_LG`、`divider()` 边框、`soft_shadow`）；标题 `fs_title`、正文 `fs_small`；命令 `mono_family` `fs_mono_block`。历史下拉（`termview.rs:1096`）选中行改 `menu_sel_bg()`。

### 5.5 浮层

- **右键/下拉菜单**（`ui::menu_item`、`ctxmenu_view.rs`、`sidebar::sb_menu_el`、`tabs_ui` 标签菜单、新建菜单）：见 3.7；菜单最小宽 200 保持；内边距 4；`ui::popover()` 圆角 `R_LG`。
- **命令面板**（`palette.rs`）：输入 `H_LARGE`；行高 32；选中 `menu_sel_bg()` + `text`（不再白字），快捷键 `kbd`；分组标题 `menu_label`（不大写）。
- **弹窗**：全部使用 `dialog_header/footer`；宽度保持各自原值；遮罩不变。
- **Toast**（`main.rs:2265` 附近）：`R_LG`、`fs_small`→`fs_body`、左侧色条保留。
- **审计日志面板**（`panels.rs::audit_panel`）：`panel_shell` → `dialog_header`；表头 `table_header`；筛选 chip → `ui::chip`；行文字 `fs_small`，哈希列 `mono_family` `fs_caption`。

### 5.6 设置页（`settings_view.rs`）

- 左侧导航：项高 28 → 30；图标改为**单色** `IC_MD` `overlay1`（删除彩色方块 `cat.color()` 的使用，保留方法以免影响其他引用；如无其他引用可删除）；选中 `selected_bg()` + `text` MEDIUM + 图标 `accent`；悬停 `hover_bg()`。
- 右侧内容区：`max_w(760)`，在宽窗口中靠左并留 `SP_6` 内边距（不要居中漂浮）；页标题 `fs_page` SEMIBOLD；页说明 `fs_small` `subtext`。
- 分组卡片 `group_card`：`R_LG`、底色 `sub_bg()`、边框 `divider()`；行内分隔 `divider()`；行 `min_h 48`、`px 16`、标签 `fs_body` MEDIUM、说明 `fs_small` `subtext`、错误 `fs_small` `error`。
- `section_title`：11 → `fs_small` SEMIBOLD `subtext`。
- 所有 `text_size(px(..))` 共 122 处全部换为 token；所有内嵌表格（权限规则、快捷键、凭据等）表头用 `table_header`。
- `choice_grid` 主题缩略卡：`R_MD`，选中描边 `accent`、底 `selected_bg()`。

---

## 6. 实施步骤（按阶段提交）

每个阶段一个（或少量）提交，提交信息用中文、风格参照现有 git log。每阶段结束必须：`cargo check --all-targets` 无错误、`cargo test` 全部通过、`sh tools/ui_lint.sh` 中已完成阶段涉及的文件无违规。

| 阶段 | 内容 | 主要文件 |
|---|---|---|
| P0 基础 | 第 3 章全部 token、`theme.rs` 新颜色函数与 `UI_SIZE`、第 4 章全部组件、`tools/ui_lint.sh`（第 8.1 节原样创建）；`main.rs` 中接入 `set_ui_size` | `ui.rs` `theme.rs` `card_layout.rs` `main.rs` `tools/ui_lint.sh` |
| P1 外框 | 标签行、品牌行、活动栏、右工具条、状态栏、四类岛统一 | `main.rs` `activity.rs` `sidebar.rs`(tool_rail/sb_frame) `panels.rs`(dock_frame) |
| P2 左工具岛 | 主机/SFTP、数据库、Redis/Docker、端口、工具、扩展管理 | `sidebar.rs` `db_ui.rs` `db_tree.rs` `kv_ui.rs` `kv_tree.rs` `tools_ui.rs` `ext_ui.rs` `activity.rs` |
| P3 右侧卡片 | 常用命令、历史命令、AI、主机工具 | `library.rs` `panels.rs` `chat_view.rs` `host_tools_view.rs` |
| P4 中间内容 | pane 头、路径栏、空工作区、数据库/Redis/Docker/文档/工具/SFTP/工作区视图、终端浮层 | `tabs_ui.rs` `main.rs` `db_view.rs` `db_editor.rs` `kv_view.rs` `dock_view.rs` `doc_view.rs` `tools_view.rs` `sftp_view.rs` `workspace.rs` `termview.rs` `port_view.rs` `update_ui.rs` |
| P5 浮层与设置 | 菜单、右键菜单、命令面板、所有弹窗、toast、审计面板、设置页 | `ui.rs` `ctxmenu_view.rs` `palette.rs` `host_dialog.rs` `host_picker.rs` `db_dialog.rs` `setup_view.rs` `panels.rs` `settings_view.rs` `sidebar.rs`(sb_modal_el/sb_menu_el) `main.rs`(render_modal 等) |
| P6 收尾 | 清零 lint、删除不再使用的旧函数/常量（`activity::panel_title`、`library::chip`、`library::row_icon`、旧 `FS_*` 等）、自查第 8 章清单 | 全部 |

---

## 7. 约束（实施模型必须遵守）

1. **不改功能**：点击/双击/右键/拖放/快捷键/动画时长/配置字段/i18n key 含义全部不变。唯一允许的交互调整：常用/历史命令行内操作收进「更多」菜单、底部按钮移到标题行、主机面板「+」移到标题行、分区可折叠（状态不落盘）。
2. 新增 UI 文案（如 empty_state 标题、「更多」tooltip）必须同时加到 `locales/zh-CN.toml` 与 `locales/en.toml`，用 `t()` 取。
3. 不改 `vendor/`、终端网格渲染（终端字号/行高仍由 `font.size`/`font.line_height` 决定）、配色方案文件（`themes/`、`schemes/`）的颜色值。
4. 代码风格与周边一致：GPUI 链式调用、单行较长的 builder 写法、中文注释、注释密度不高于现有代码。
5. `card_layout.rs` 现有单元测试必须通过（`EDGE_GAP == 4`、`CARD_PAD == CARD_GAP` 等约束不能破坏）。
6. 不得用 `// ui-lint: allow` 规避大面积违规；允许的例外仅限：窗口按钮配色（`chrome.rs` 已排除）、品牌/文件类型/数据库引擎 SVG 尺寸、终端网格相关、发送按钮等确需 `rounded_full` 以外特殊值的个别元素。全项目例外总数 ≤ 15，每处写明原因。
7. 每阶段完成后在提交说明里列出：改了哪些区域、哪些地方与本方案有偏差及原因。

---

## 8. 验收标准

### 8.1 自动检查脚本（P0 原样创建 `tools/ui_lint.sh`）

```sh
#!/bin/sh
# 界面规范检查：业务视图不得直接写字号、圆角、图标尺寸、悬停色等字面量，统一走 ui / theme 中的 token 与组件。
# 个别确需例外的行，在行尾加 `// ui-lint: allow <原因>`。
cd "$(dirname "$0")/.." || exit 1
fail=0
# 定义 token 与组件本身的文件不检查
TOKENS='^src/(ui|theme|card_layout|chrome)\.rs:'
check() {
  name=$1; re=$2; skip=$3
  hits=$(grep -rnE --include='*.rs' "$re" src | grep -vE "$skip" | grep -v 'ui-lint: allow')
  n=$(printf '%s' "$hits" | grep -c .)
  if [ "$n" -gt 0 ]; then echo "✗ ${name}：${n} 处"; printf '%s\n' "$hits" | head -15; fail=1; else echo "✓ ${name}"; fi
}
check "字号字面量 text_size(px(数字))"      'text_size\(px\([0-9]'                                   "$TOKENS"
check "圆角字面量"                          'rounded(_t[lr]|_b[lr])?\(px\([0-9]|rounded_(sm|md|lg|xl|2xl)\(\)' "$TOKENS"
check "图标尺寸字面量"                      'icon\(.*, [0-9]+\.[0-9]*, '                               "$TOKENS"
check "SVG 尺寸字面量"                      'svg\(\).*\.size\(px\([0-9]'                               "$TOKENS|^src/(file_icon|db_tree)\.rs:"
check "悬停底色使用 surface"                'hover\(.*bg\(c\(cur\(\)\.surface[0-2]\)\)'                "$TOKENS"
check "monospace 字面量"                    'font_family\("monospace"\)'                               '^$'
check "界面标签大写"                        'child\(.*to_uppercase\(\)\)'                              "$TOKENS|^src/(tools|db)/"
check "10px 及以下字号"                     'text_size\(px\((10|[1-9])(\.|\))'                         '^$'
allow=$(grep -rn --include='*.rs' 'ui-lint: allow' src | wc -l | tr -d ' ')
echo "例外标注：${allow} 处（上限 15）"
[ "$allow" -le 15 ] || fail=1
exit $fail
```

基线（实施前，脚本实测）：字号字面量 451、圆角字面量 174、图标尺寸字面量 196、SVG 尺寸 1、surface 悬停 49、monospace 9、大写标签 4、10px 字号 9。
**验收要求：脚本退出码 0。**

### 8.2 构建与测试
- `cargo check --all-targets` 无错误、无新增 warning。
- `cargo test`（不含需要外部服务的 live 测试）全部通过。
- `cargo build --bin shellax` 可启动。

### 8.3 代码审查项（审查方逐条检查）
1. `ui.rs` 存在第 4 章全部组件，`theme.rs` 存在 `menu_sel_bg/divider/sub_bg/danger_bg/set_ui_size/ui_size`。
2. `activity::panel_title`、`library::chip`、`library::row_icon`、`FS_*` 常量、`Root::strip_btn_with` 内的手写按钮样式已删除或改为调用公共组件。
3. 四类岛都通过 `card_layout::island` 生成；`dock_frame` 无边框。
4. 所有左工具岛都有 `panel_header`，主机面板包含。
5. 列表行均通过 `ui::list_row`；主机、本地终端、环境主机、位置行结构一致（单色图标 + 角标状态点）。
6. 常用/历史命令：命令文本等宽；行内操作默认隐藏；底部按钮已移到标题行；本机/全部主机为分段控件。
7. 菜单、命令面板、设置导航不再出现实心强调色 + 白字（`text_color(gpui::white())` 仅允许出现在主按钮、窗口关闭按钮悬停、标签色点勾选、危险确认按钮）。
8. 所有弹窗使用 `dialog_header/dialog_footer`。
9. 新增文案在 `zh-CN.toml` 与 `en.toml` 中都存在。

### 8.4 视觉验收（截图核对）
在 1440×900 窗口下，分别用 **ShellAX Midnight（暗）** 与 **ShellAX Daylight（亮）** 截取以下画面；另外把「界面字号」改为 15 再截取 ①②⑤ 三张：

1. 主界面：主机面板（含分组、主机、位置、标签分区）+ 终端分屏 + 状态栏
2. 主界面 + 右侧同时打开 AI、常用命令、历史命令三张卡片
3. 数据库模块：左侧连接树展开 + SQL 标签页
4. Redis 模块与 Docker 模块各一张
5. 工具模块（含收藏/最近分区）
6. 主机工具卡片（概览 + 进程）
7. 右键菜单、标签页菜单、命令面板（有选中项）
8. 新建主机弹窗、审计日志面板
9. 设置页：外观分类、权限分类
10. 空状态：无主机时的主机面板、无常用命令时的卡片

核对要点：
- 同类元素同尺寸：所有岛的标题行等高、标题字号一致；所有列表单行行高一致；所有图标按钮按档位一致。
- 左对齐线：面板标题文字、搜索框放大镜、列表行图标三者左边缘在一条竖线上。
- 无实心强调色选中（主按钮除外）；悬停深浅全局一致。
- 字号 15 时所有文字同步放大、无裁切、无重叠。
- 亮/暗主题下岛与外框底色可区分，岛无边框，子卡片层次清晰。

---

## 附：关键文件索引

| 区域 | 文件 |
|---|---|
| Token / 组件 | `src/ui.rs`、`src/theme.rs`、`src/card_layout.rs`、`src/chrome.rs`、`src/motion.rs` |
| 窗口骨架 / 标签行 / 状态栏 | `src/main.rs`（`render` 约 1793–2140 行） |
| 活动栏 / 底部面板 / 面板标题 | `src/activity.rs` |
| 主机侧栏 / 右工具条 | `src/sidebar.rs` |
| 右侧卡片框架 / AI / 审计 | `src/panels.rs`、`src/chat_view.rs` |
| 常用命令 / 历史命令 | `src/library.rs` |
| 主机工具 | `src/host_tools_view.rs` |
| 数据库 | `src/db_ui.rs`、`src/db_tree.rs`、`src/db_view.rs`、`src/db_editor.rs`、`src/db_dialog.rs` |
| Redis / Docker | `src/kv_ui.rs`、`src/kv_tree.rs`、`src/kv_view.rs`、`src/dock_view.rs` |
| 工具 / 扩展 | `src/tools_ui.rs`、`src/tools_view.rs`、`src/tools_layout.rs`、`src/ext_ui.rs` |
| SFTP / 端口 / 文档 / 工作区 | `src/sftp_view.rs`、`src/port_view.rs`、`src/doc_view.rs`、`src/workspace.rs` |
| 菜单 / 命令面板 / 弹窗 | `src/ctxmenu_view.rs`、`src/palette.rs`、`src/tabs_ui.rs`、`src/host_dialog.rs`、`src/host_picker.rs`、`src/setup_view.rs`、`src/update_ui.rs` |
| 设置 | `src/settings_view.rs` |
| 文案 | `locales/zh-CN.toml`、`locales/en.toml` |
