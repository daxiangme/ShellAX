# ssh-term-proto

最小原型：一个 GPUI 窗口，左右分屏两个终端：
- 左：`portable-pty` 本地 shell
- 右：`russh` SSH 会话（公钥认证，PTY + shell）

两个终端都用 `alacritty_terminal` 解析，字符网格渲染是自己写的。

## 许可证与净室说明
- v4 起移植了少量 Apache/MIT 代码：herdr（Agent manifest 引擎和 3 份规则）、zellij（分屏方向规则）。来源、commit、修改说明见 `THIRD_PARTY_NOTICES.md`，许可证全文在 `licenses/`。
- 只用了 Apache/MIT 的依赖和官方示例：`gpui-pre`（Zed 的 GPUI 快照，Apache-2.0）的 `examples/`、russh 自带的 `examples/client_exec_interactive.rs`（Apache-2.0）、alacritty_terminal 公共 API。
- **没有读过** Zed `crates/terminal`、`terminal_view`、`editor`，也没有读过 shell-rs / oxideterm / warp 的源码。

## 选型（与 PRD v0.3 一致）
| 组件 | 版本 |
|---|---|
| GPUI | `gpui = { package = "gpui-pre", version = "=0.3.9" }`，`gpui_platform = { package = "gpui-pre-platform", version = "=0.3.9" }`（快照对应 zed@93d7456；版本与 gpui-component 0.7.3 的锁定一致） |
| 终端解析 | `alacritty_terminal` 0.26.0 |
| 本地 PTY | `portable-pty` 0.9.0 |
| SSH | `russh` 0.64.1，`default-features = false, features = ["aws-lc-rs", "flate2", "rsa"]` |
| 工具链 | Rust stable 1.99.0（2026-09-28） |

## 结构
```
src/
  term.rs    与后端无关的 Session：Term<Listener> + vte Processor 解析线程 + dirty 标志；
             write()/resize() 通过 flume 发给后端；Listener 把 PtyWrite（DA/DSR 应答）回写给后端；screen_text() 供测试用
  local.rs   portable-pty：读线程 → Session；写/resize 线程 ← Session
  ssh.rs     russh：client::connect → 公钥认证 → channel_open_session → request_pty → request_shell；
             channel.split()，一个 tokio 任务里用 select! 同时处理输入/resize(window_change) 和输出
  main.rs    GPUI 界面（v2）：侧栏 + 标签栏 + 分屏 + 状态栏；TermView 用自绘 canvas 画网格（见下文 v2 一节）；
            键盘 → 字节映射（Enter/Backspace/方向键 含 APP_CURSOR/Ctrl+字母/Alt 前缀）；
             点击面板切换焦点；根据窗口 viewport 计算 cols/rows 并 resize
  theme.rs   主题系统：TOML 主题加载、Alacritty 导入、ANSI 16/256/真彩色映射
  config.rs  ~/.config/agentshell/config.toml（字体族/回退链/字号/行高/连字、默认主题）
  suggest.rs 历史自动建议引擎：History（去重、敏感过滤、落盘）+ InputTracker（OSC 133 / 按键+回显校验）
  termview.rs（v4）终端面板：GridLayer（可缓存的网格层，行级 damage）+ CursorLayer（光标和建议）+ 输入/鼠标/剪贴板/滚动
  agent.rs   （v4）Agent 状态识别，移植自 herdr manifest 引擎；agents/*.toml 为 herdr 原样规则
  layout.rs  （v4）分屏二叉树；auto_direction 移植自 zellij
  hosts.rs   （v4）~/.ssh/config 解析和主机条目
  boxdraw.rs （v4）框线、方块、Powerline 字形的像素栅格化
themes/            内置主题：catppuccin-mocha/latte（MIT）、agentshell-midnight/daylight（自有）
scripts/fonts.sh   中文/emoji/Nerd 图标/Powerline/连字测试文本
scripts/colors.sh  打印 16/256/真彩色和粗体、暗淡、反色样例
tests/headless.rs  无界面测试（见下）
testenv/           本地测试 sshd 的配置和密钥（仅测试用，勿用于生产）
```

## 运行
```bash
cargo build
# 测试用 sshd（已在本机配置：用户 sshtest，端口 2222，仅监听 127.0.0.1）
sudo /usr/sbin/sshd -f $PWD/testenv/sshd_config -E $PWD/testenv/sshd.log
SSH_HOST=127.0.0.1 SSH_PORT=2222 SSH_USER=sshtest SSH_KEY=$PWD/testenv/client_ed25519 \
  DISPLAY=:2 ./target/debug/ssh-term-proto
```
环境变量：`SSH_HOST`（默认 127.0.0.1）、`SSH_PORT`（22）、`SSH_USER`（$USER）、`SSH_KEY`（~/.ssh/id_ed25519）、`TERM_FONT`（DejaVu Sans Mono）。

Linux 构建依赖（Debian 13 上实际装的）：`libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb1-dev libvulkan-dev mesa-vulkan-drivers libfontconfig1-dev libfreetype6-dev pkg-config clang`，以及 C 编译器（aws-lc-sys 需要）。

## 验证（2026-10-12 在本 Linux 机器上实测）
1. `cargo build` 通过。
2. 无界面测试：
   ```bash
   TEST_SSH_PORT=2222 TEST_SSH_USER=sshtest TEST_SSH_KEY=$PWD/testenv/client_ed25519 cargo test --test headless -- --nocapture
   ```
   - `local_pty_ls`：在临时目录建 `marker_local_file.txt`，向 PTY 输入 `cd … && ls`，断言 alacritty 网格里出现该文件名；同时断言 resize 后尺寸为 120x30。
   - `ssh_localhost_ls`：SSH 到本机 sshd，resize 到 90x25，输入 `stty size; ls`，断言网格里出现 `25 90`（说明 window_change 生效）和 `hello_from_sshd.txt`。
   - 结果：2 passed。
3. 界面：在 X 显示 `:2`（Xvfb + 桌面，Mesa lavapipe 软件 Vulkan）上运行，用 xdotool 在左侧输入 `ls`、点击右侧后输入 `ls -la`，两边都显示输出，截图 `screenshot.png`。再用 xdotool 把窗口高度改小，标题从 81x42 变为 81x36，远端 `stty size` 输出 `36 81`，说明窗口 resize 一路传到了远端 PTY。

**没验证的**：macOS、Windows、Wayland、真实 GPU；IME/中文输入；宽字符（CJK）对齐；大量输出时的性能。

## 遇到的坑
- crates.io 上的 `gpui` 0.2.2 是 2025-10 发布的，之后再没更新；gpui-component 0.7.x 用的是 `gpui-pre` 快照（每周发布、`=` 精确锁定）。要和 gpui-component 兼容，就得用同一个 `gpui-pre` 版本。
- `gpui-pre` 把平台层拆到了 `gpui-pre-platform`，入口是 `gpui_platform::application()`，不是旧的 `gpui::Application::new()`。
- 系统自带 Rust 1.85 太旧（russh 0.64 要求 ≥1.89）；装了 rustup stable 1.99。注意 `/workspace/research/Eugeny_russh/rust-toolchain.toml` 会把该目录下的工具链覆盖成 1.91。
- `alacritty_terminal::term::test::TermSize` 只是测试辅助，自己实现 `Dimensions` 更稳。
- russh 的 `Channel::wait()` 要 `&mut`，写入要 `&`，同一个 select! 里没法同时借用，所以要先 `channel.split()`。
- `useradd` 新建的用户密码字段是 `!`（锁定）；`UsePAM no` 时 sshd 会拒绝锁定账号的公钥登录，要 `usermod -p '*'`。
- 窗口宽度在这个 1920 宽的桌面上被窗口管理器限制在约 1024px，所以只验证了高度方向的 resize 生效；宽度方向是同一段代码，但没在界面上单独验证。
- **原型的安全缺口**：`check_server_key` 接受所有 host key（只在原型里这样做），产品必须做 known_hosts/TOFU。
- 渲染是「每行若干 div」的简单做法，每帧重建元素，只适合原型；产品应改成自定义 Element，批量 shape 文本并画背景矩形。

## v2 界面（2026-10-12）
截图：`screenshot-v2.png`（DISPLAY=:2 实测：左边本地 `ls --color=always` + `scripts/colors.sh`，右边 SSH `ls --color=always -la /` + `colors.sh`，开了两个标签）。

- **布局**：左侧主机侧栏（字母图标徽章、名称/副标题、在线状态点，点击打开新标签）；顶部圆角标签栏（激活态底色和紫色圆点、× 关闭、+ 新建本地标签）；分屏之间 1px 分隔线，焦点面板顶部有 2px 紫色边框，标题变亮；底部状态栏（连接状态点、主机、尺寸 cols×rows、UTF-8、「AI: Ask 模式」徽章占位）。
- **终端渲染（自写，未参考 Zed）**：`canvas` 元素。prepaint 里实测 cell 宽度，按元素大小算 cols/rows 并 resize；paint 里每行先把相同背景的连续单元格合并成一个矩形（`paint_quad`），再把整行文字按「同前景色 + 同粗细」合并成 `TextRun`，一次 `shape_line(…, force_width = cell 宽)` 后绘制，保证网格对齐。替代了原来「每行多个 div」的做法。
- **颜色**：ANSI 16 色用 Catppuccin Mocha 官方终端配色；256 色用 xterm 6×6×6 色块 + 24 级灰阶；真彩色直接用；支持 粗体 / 暗淡 / 反色 / 隐藏。
- **光标**：块状，约 530ms 闪烁，有输出时立即重新显示；失焦时只画空心框。
- **选中**：鼠标拖动选择（按行连续），用 Surface2 色高亮；按键后清除。（暂不支持复制到剪贴板。）
- **字体**：终端用系统已装的 JetBrains Mono（可用 `TERM_FONT` 覆盖），界面文字用 Inter。CJK 字符由系统字体回退显示。

### 许可
- 配色：Catppuccin（MIT License，https://github.com/catppuccin/catppuccin）。只用了色值，发布时在第三方声明里写一行署名。
- 字体：JetBrains Mono 和 Inter 都是 SIL Open Font License 1.1。原型**没有打包字体文件**，用的是系统已装字体；产品如果打包，要附带 OFL 许可文本，而且不能单独售卖字体。

### v2 已知问题
- JetBrains Mono 默认开启连字；终端字体已用 `FontFeatures::disable_ligatures()` 关掉 `calt`，`->` 等会按原字符显示。
- 宽字符（CJK）按单个 cell 宽画，`force_width` 下会被压窄或重叠，没处理 `WIDE_CHAR`。
- 没有滚动回看（scrollback）、复制粘贴、下划线/删除线、超链接；每帧重新 shape 每一行，没有做行缓存。
- 侧栏、标签栏都是手写 div，没有用 gpui-component（省去依赖和 API 学习成本），所以没有它的主题切换能力。
- 新建标签只能开本地 shell；侧栏主机列表是写死的两项；标签内不能新建或拖动分屏。
- 只在 Linux X11 + 软件 Vulkan 上看过效果。

## v3（2026-10-13）：字体、主题、历史建议
截图（DISPLAY=:2 实测）：`screenshot-v3-fonts.png`、`screenshot-v3-theme-light.png`、`screenshot-v3-suggest.png`。

### 字体
- 回退链：`Font.fallbacks`，Linux 默认 `JetBrains Mono → Noto Sans Mono CJK SC → Symbols Nerd Font Mono → Noto Color Emoji → DejaVu Sans Mono`；macOS、Windows 默认值见 `config.rs`（未实测）。
- 连字：`calt/liga/dlig = 0`，`->`、`=>`、`!=` 按原字符显示。
- 宽字符：`WIDE_CHAR` 单独排版，在 2 格槽位内居中；`WIDE_CHAR_SPACER` 跳过；零宽字符拼到前一个字形。
- 配置示例（`~/.config/agentshell/config.toml`）：
  ```toml
  theme = "system"            # 或主题名
  [font]
  family = "JetBrains Mono"
  fallbacks = ["Noto Sans Mono CJK SC", "Symbols Nerd Font Mono", "Noto Color Emoji"]
  size = 13.5
  line_height = 1.4
  ligatures = false
  ```
  环境变量 `TERM_FONT`、`TERM_FONT_SIZE`、`THEME` 优先。
- 本机为测试装了 Symbols Nerd Font（`~/.local/share/fonts/nerd/`，MIT 字体，内含 CC BY 4.0 等来源的图标）；没有打包进原型。

### 主题
- `themes/*.toml` 内置 4 个（Catppuccin Mocha/Latte 为 MIT；AgentShell Midnight/Daylight 为本项目原创）。用户主题放 `~/.config/agentshell/themes/`，原生格式或 Alacritty 配色（自动识别）都行。
- 切换：Ctrl+Shift+T 循环、点状态栏主题名循环、点「系统」跟随系统明暗（`observe_window_appearance`，运行时切换在 Xvfb 下无法实测）。

### 历史自动建议
- 输入时光标后显示灰色建议；**→ 或 Tab** 接受（有建议时拦截，没有建议时透传给 shell）。
- 当前输入识别：本地 bash 注入 `--rcfile` 发 OSC 133 A/B/C，解析线程旁路扫描并记录 `133;B` 的光标位置；SSH 或无集成时用「按键缓冲 + 屏幕回显校验」。
- 历史来源：本面板 Enter 的命令（按主机分开）+ 本地 `~/.bash_history` + 远端 `~/.bash_history`（独立 exec channel 读取）。
- 不记录、不建议：备用屏（vim/htop）、不回显的输入（密码）、以空格开头的命令、含 password/token/secret 等的敏感命令。
- 测试：`cargo test` → lib 单元测试 7 个（建议引擎）、主题单元测试 2 个、headless 3 个（新增本地 bash OSC 133 标记测试），全部通过。

### v3 坑
- Read 工具按路径缓存图片，同名截图覆盖后看到的是旧图，要复制成新文件名再看（只影响自查截图，不影响程序）。
- `import -window <id>` 截 GPUI/Vulkan 窗口偶尔拿到旧帧；改成先截整个 root 再按 `xwininfo` 坐标裁剪。
- 远端 locale 不是 UTF-8 时，`printf '\uXXXX'` 不输出字符，测试脚本改用 UTF-8 字面量。
- 快速输入时 Enter 早于远端回显到达，严格的回显校验会漏记命令；提交时放宽为「屏幕已显示至少一半」。

### v3 已知问题
- 建议超过行尾会被截断，不折行；OSC 133 标记遇到滚屏会失效，退回按键兜底。
- 框线字符和 Powerline 用字体绘制，有细小缝隙；产品应自绘。
- zsh/fish 集成、zsh_history 格式、历史落盘都没做；远端 OSC 133 注入没做。
- iTerm2 `.itermcolors` 导入没做（PRD 5.11 写了方案）。
- 空闲时约 45% CPU（软件 Vulkan + 每 16ms 轮询 + 光标闪烁全量重绘），产品要改成事件驱动并做行缓存。

## v4（2026-10-13）：性能与能力补齐

截图（DISPLAY=:2 实跑，release 构建，都逐张看过）：
- `screenshot-v4-hostkey.png`：首次连接弹出主机密钥确认（指纹和 `ssh-keygen -lf testenv/host_ed25519.pub` 一致）
- `screenshot-v4-split-scroll.png`：同一标签里三个分屏；两个面板向上回看并显示滚动条；左下是下划线/双线/波浪线/删除线/斜体/OSC 8 和自绘框线、方块、Powerline（`scripts/deco.sh`）
- `screenshot-v4-htop-vim.png`：htop 点 MEM% 表头排序、点行选中；vim `mouse=a` 滚轮滚动并点击定位（状态行 `cursor at line 94 col 20`）
- `screenshot-v4-agents.png`：面板头和标签上的 Agent 状态（Claude 需要确认 / Grok 运行中 / Codex 运行中，Codex 在 SSH 侧）。**三个 Agent 都是 `/tmp/fakebin` 里的模拟脚本**，只模仿标题和提示文字
- `screenshot-v4-suggest-wrap.png`：建议灰字超过行尾后折到下面两行

### 运行（v4 新增变量）
```bash
cargo build --release
SSH_PORT=2222 SSH_USER=sshtest SSH_KEY=$PWD/testenv/client_ed25519 \
  AGENTSHELL_KNOWN_HOSTS=$PWD/testenv/known_hosts_demo \
  AGENTSHELL_SSH_CONFIG=$PWD/testenv/ssh_config \
  PERF_LOG=1 DISPLAY=:2 ./target/release/ssh-term-proto
```
- `AGENTSHELL_KNOWN_HOSTS`：known_hosts 路径（默认 `~/.ssh/known_hosts`）。删掉这个文件就会再次弹出确认框。
- `AGENTSHELL_SSH_CONFIG`：ssh_config 路径（默认 `~/.ssh/config`）。`testenv/ssh_config` 是测试用的。
- `PERF_LOG=1`：每秒在 stderr 输出 `grid_paints/s rows_built/s cursor_paints/s`，全为 0 时不输出。
- `NO_SSH=1`：启动时只开本地面板。
- config.toml 新增顶层键 `known_hosts`、`ssh_config` 和 `[[hosts]]` 数组（字段 name/host/user/port/identity_file），见 `src/config.rs` 文件头。

快捷键：
- Ctrl+Shift+D：分屏（按 zellij 规则自动选方向）；Ctrl+Shift+W：关闭分屏；标签栏右侧也有「左右 / 上下 / 关闭分屏」按钮。
- Ctrl+Shift+C / Ctrl+Shift+V：复制 / 粘贴；Shift+Insert、中键也能粘贴。
- Shift+PgUp / Shift+PgDn：回看翻页；也可以用滚轮和滚动条。
- 按住 Shift 拖动：在 htop、vim 这类程序里也做本地选择。
- Ctrl+点击：打开 OSC 8 链接。

### 性能（前后对比，同一脚本 `/tmp/bench.sh` 各跑两次，软件 Vulkan）
| 指标 | v3 | v4 |
|---|---|---|
| 空闲 CPU（聚焦，10 s 无输入后） | 17.6% / 17.9% | **0.2% / 0.2%** |
| 光标闪烁期间 CPU | 17.4% / 19.1% | 17.1% / 18.3%（网格层没有重绘，开销来自 GPUI 整窗重绘 + llvmpipe） |
| `seq 1 2000000` | 0.91 / 1.10 s | 0.97 / 1.08 s |
| `cat` 38 MB | 2.56 / 2.58 s | 2.77 / 2.72 s |
| 大量输出时网格帧率 | 无计数器（60 Hz 轮询上限） | 31–35 帧/s，每帧重建约 37 行 |
| 打一个字符 | 整屏重排 | 1–2 行（alacritty damage） |

结论：
- 空闲功耗问题已解决。
- 吞吐没有变化，瓶颈在解析和 pty 回压。
- 闪烁的开销在软件渲染下没降下来，要在真 GPU 上复测。

### 测试
`TEST_SSH_PORT=2222 TEST_SSH_USER=sshtest TEST_SSH_KEY=$PWD/testenv/client_ed25519 cargo test` 结果：
- lib 单元测试 21 个：建议引擎、zsh 历史、落盘、OSC 扫描、herdr 规则、ssh_config、分屏树、框线；
- bin 单元测试 5 个：主题、bracketed paste、鼠标编码、config hosts；
- 无界面测试 6 个：
  - 本地 ls；
  - SSH 测试（ls、window_change、known_hosts 首次确认、再次连接不询问、密钥变更被拒）；
  - 回看；
  - bash OSC 133；
  - zsh OSC 133；
  - 用模拟的 `grok` 进程测前台进程识别和 Working 状态。

### v4 坑
- 截图被缩放显示：窗口 1440 宽，看图工具按 1024 宽显示，算 xdotool 坐标时要乘 1440/1024。
- 不要在模拟脚本还在 `read` 时修改它：bash 会接着从修改后的文件读，命令被吞。
- 框线最初直接按 `col*cw` 画，会出现 1 px 缝。改成用相邻单元格边缘取整后计算宽度。
- 第一次看到 Powerline 三角没显示，后来发现是测试脚本里漏了 PUA 字符，并不是渲染问题。最终仍改为按像素行栅格化成矩形（各后端一致、边缘清晰）；GPUI `PathBuilder` 路径能不能用没有单独验证。

### v4 已知问题
- 圆角框线画成直角；双线交叉处有 1 px 毛刺；斜线 U+2571–2573 仍用字体。
- 分屏比例不能拖拽。
- 滚动条点击/拖动和 Shift+PgUp 已实现，但没有实测；Ctrl+点击打开链接也没有实测。
- SSH 侧 Agent 识别依赖本面板里输入的命令首词或 OSC 标题，通过别名启动时识别不到。
- zsh 只测了本地（无界面测试），远端 zsh 只读取历史文件。
- IME 预编辑、macOS/Windows/Wayland 仍未验证。

## v5（2026-10-13）：设置页与功能入口

截图（DISPLAY=:2，1440×860 窗口）：
- `screenshot-v5-sidebar.png`：侧栏，含主机搜索、新建按钮、分组（快速连接 / 测试 / 生产 / staging / ~/.ssh/config）、生产标记、悬停编辑按钮，以及左下功能区（SFTP、端口转发、AI/Agent、审计日志、命令面板、设置、帮助与关于）
- `screenshot-v5-settings-{appearance,terminal,ssh,keys,search}.png`：设置页的外观、终端、SSH、快捷键分类和搜索结果
- `screenshot-v5-permissions.png`：权限与安全：模式卡片、规则表（可改效果/上移/删除/新增）、规则测试器、风险等级
- `screenshot-v5-new-host.png`、`screenshot-v5-palette.png`、`screenshot-v5-audit.png`、`screenshot-v5-ai-agent.png`、`screenshot-v5-sftp.png`、`screenshot-v5-about.png`

新增文件：`src/config.rs`（重写：完整设置结构、点分路径读写、保存）、`src/settings_view.rs`、`src/palette.rs`、`src/host_dialog.rs`、`src/panels.rs`、`src/actions.rs`、`src/ui.rs`、`src/policy.rs`、`src/audit.rs`。新依赖：`gpui-component 0.7.3`、`gpui-kit-assets 0.7.3`（Lucide 图标，ISC）、`sha2`、`chrono`、`serde_json`。

**真正可用**
- 设置读写 `~/.config/agentshell/config.toml`（`AGENTSHELL_CONFIG` 可换路径），改动立即写回。立即生效：主题/跟随系统/浅深色主题、字体/回退链/字号/行高/连字、光标样式/闪烁、响铃（闪烁边框）、选中即复制、多行粘贴确认、鼠标上报、命令建议开关/接受键/过滤正则、Agent 识别开关、读取 ssh_config/默认用户端口（主机列表）、快捷键改绑。新会话生效：默认 Shell、回滚行数、Shell 集成开关、本地/远程历史、keepalive、连接超时、known_hosts 路径、未知主机密钥策略（询问/严格/自动接受）。
- 新建/编辑/删除主机，写入 `[[hosts]]`（含 group、production、auth、jump 字段）。
- 命令面板 Ctrl+Shift+P：28 个动作和所有主机的模糊搜索，↑↓/Enter/Esc。
- 状态栏权限模式按钮：点击循环 Ask → Plan → Auto，写配置并记审计。
- 权限规则编辑器 + 测试器（`policy::decide`，有单元测试）。
- 审计日志：SHA-256 哈希链 JSONL，查看器、校验、导出（导出到 `~/agentshell-audit-<时间>.jsonl`）。
- 凭据页：只读扫描 `~/.ssh/id_*` 并显示 SHA-256 指纹；检测 SSH_AUTH_SOCK。外部 Agent 页：只读检测 hook 配置。关于页：显示 THIRD_PARTY_NOTICES。

**只有界面（标“即将支持”）**：界面语言、更新、透明度、界面字号、跳板机、加密算法偏好、Agent 转发、钥匙串、ssh-agent 认证、生成/导入密钥、全部内置 AI 设置、Agent 通知/审批超时/一键安装 hook、生产主机确认、审计保留/记录命令、会话持久化/布局恢复/关闭确认/自动重连、通知、响铃声音、SFTP 远端、端口转发、密码/agent 认证。这些设置能改、会保存，但没有效果。权限模式和规则目前**不拦截任何操作**（还没有内置 AI 和 agent bridge）。

**测试**：`cargo test --release`：库 25 + 二进制 7 + 无界面 6（SSH 用例需 `TEST_SSH_PORT=2222 TEST_SSH_USER=sshtest TEST_SSH_KEY=testenv/client_ed25519`）全部通过；release 构建无警告。

**已知问题**：设置写回会丢掉 config.toml 里手写的注释；多行粘贴确认在 bash/zsh 提示符下不会出现（readline 开了括号粘贴，属预期）；本机 X 环境没有 xclip，粘贴确认没截图验证；首次点击窗口有时只是获得焦点。

## v6：界面多语言 + 内置 AI（2026-10-13）

### 界面语言
- 设置 → 通用 → 界面语言：跟随系统 / 简体中文 / English，切换后立即生效，写入 `general.language`。
- 文本在 `locales/en.toml`、`locales/zh-CN.toml`（扁平 `"key" = "文本"`，`{name}` 占位符），编译时嵌入。缺键时先回退英文，再回退到 key 本身。
- `cargo test --lib i18n` 会检查：两份文件键集合相同、占位符相同、代码里所有 `t("…")` 都有英文。
- 不随语言变化的内容：已打开标签的标题（新开标签才会换）、用户数据（分组名、规则原因）、已经写进审计的文本。

### 内置 AI（自带 Key）
- 设置 → AI：可以添加多个服务商（类型为 OpenAI 兼容或 Anthropic；可填 Base URL、模型、超时、温度、最大 tokens），并设一个为默认。这些写入 `config.toml` 的 `[[ai.providers]]`。
- API Key **不写入 config.toml、日志或审计**：
  - 优先存系统钥匙串（keyring crate：Linux 用 Secret Service，macOS 用钥匙串，Windows 用凭据管理器）。服务名为 `agentshell`，账户为 `ai-provider:<id>`。
  - 钥匙串不可用时，存到 `$XDG_DATA_HOME/agentshell/secrets.enc`（ChaCha20-Poly1305 加密，密钥文件 `secrets.key`，两者权限都是 0600）。这个加密只防误泄露，不防同一用户的其他进程。
  - 界面只显示 Key 的后 4 位，并标出当前存储位置。
  - 设环境变量 `AGENTSHELL_NO_KEYRING=1` 可强制使用文件存储。
- “测试连接”会发一次真实的最小请求（max_tokens=1），并显示延迟或错误。
- 脱敏和上下文范围设置会作用于实际请求：
  - 上下文范围可选：不发送 / 最近命令 / 选中 / 当前屏幕。
  - 关闭“允许发送输出”后只发送最近一条命令。
  - 发送前脱敏会去掉密码、Token、私钥、URL 中的凭据等。
- 自然语言生成命令：在终端输入 `# 想做的事` 回车，或按 Ctrl+Shift+G，会弹出命令卡片。
  - **插入（→）**：只写入命令、不回车。
  - **执行…**：交给权限引擎按当前模式和规则判定，允许则运行，询问则弹出确认，拒绝则显示原因。
  - 请求、响应、插入、判定、执行都会写入审计（只记录脱敏后的请求文本和上下文字数）。
- 本地 mock（开发用）：`python3 tools/mock_ai.py` 监听 127.0.0.1:18080，提供 OpenAI 兼容接口，按关键词返回固定命令。**本轮没有用任何真实模型测试过。**

### 截图
`screenshot-v6-english-ui.png`、`screenshot-v6-english-settings.png`、`screenshot-v6-chinese-settings.png`、`screenshot-v6-chinese-ui-confirm.png`、`screenshot-v6-ai-provider-en.png`、`screenshot-v6-ai-provider-zh.png`、`screenshot-v6-nl-card.png`、`screenshot-v6-nl-inserted.png`、`screenshot-v6-nl-redaction-zh.png`、`screenshot-v6-nl-plan-denied.png`

## v7：AI 模型列表（2026-10-13）

- 服务商模板：OpenAI、Anthropic、OpenRouter、DeepSeek、通义 DashScope、Kimi、智谱、Ollama、LM Studio、自定义。选中后自动填类型和 Base URL。
- 模型列表：OpenAI 兼容服务商请求 `GET {base}/models`（Base URL 有没有 `/v1` 都可以），Ollama 在 404 时退回 `/api/tags`，Anthropic 请求 `/v1/models` 并翻页。OpenRouter 会显示上下文长度和价格。
- 设置 → AI：模型下拉框可搜索，有“刷新模型”按钮，显示加载中、数量或错误。保存 Key 或修改 URL 后会自动拉取。可以手填模型名，可以只显示聊天模型（`ai.chat_models_only`）。列表缓存在 `models-cache.json`，缓存时长由 `ai.models_cache_hours` 设置。
- 命令模型 `fast_model`：“# …”生成命令时用这个模型，留空则用主模型。
- 可选元数据来源 models.dev（MIT），运行时拉取。
- mock：`python3 tools/mock_ai.py` 新增 `/v1/models`、Anthropic 分页和 `/api/tags`。设置 `MOCK_MODELS_DELAY` 可以模拟慢响应。
- 真实接口验证：`cargo run --example models_probe` 请求 OpenRouter 公开接口，不需要 Key。
- 截图：`screenshot-v7-*.png`。
