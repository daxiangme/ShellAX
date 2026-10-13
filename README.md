# ssh-term-proto

最小原型：一个 GPUI 窗口，左右分屏两个终端：
- 左：`portable-pty` 本地 shell
- 右：`russh` SSH 会话（公钥认证，PTY + shell）

两个终端都用 `alacritty_terminal` 解析，字符网格渲染是自己写的。

## 许可证与净室说明
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
  suggest.rs 历史自动建议引擎：History（去重、敏感过滤）+ InputTracker（OSC 133 / 按键+回显校验）
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
