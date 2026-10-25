# ShellAX 开发与调试说明

## 1. 环境
- Rust stable ≥ 1.85（`rustup default stable`）
- Windows 本机开发：Visual Studio 2022 Build Tools（勾选“使用 C++ 的桌面开发”，含 Windows 10/11 SDK）。
  GPUI 的 Windows 后端在 **release** 构建时会用 SDK 里的 `fxc.exe` 编译 HLSL 着色器；找不到时可设置环境变量
  `GPUI_FXC_PATH=C:\Program Files (x86)\Windows Kits\10\bin\<版本>\x64\fxc.exe`。debug 构建不需要 fxc。
- Linux：`libxkbcommon-dev libwayland-dev libvulkan-dev libfontconfig-dev libssl-dev clang`
- 最低运行环境：Windows 10 1903+。

## 2. 目录
- `src/main.rs` GUI 入口（二进制 `shellax`），`src/bin/shellax-cli.rs` → CLI `shellax-cli`
- `src/` 其余模块：`ssh.rs`(russh)、`local.rs`(portable-pty)、`config.rs`、`ctxmenu.rs`、权限/审计/AI 等
- `vendor/gpui-pre-windows/` GPUI Windows 后端的补丁副本（Apache-2.0），只为在 Linux 上交叉编译时使用预编译着色器；Windows 本机构建照常走 fxc
- `vendor/gpui-pre-macos/` GPUI macOS 后端的补丁副本（Apache-2.0）。剪贴板改用系统字符串写入，避免中文复制到其他软件后按 MacRoman 变成乱码
- `packaging/windows/installer.nsi` NSIS 安装包脚本；`tools/package_win.sh` 打包脚本
- `docs/` PRD、测试报告、截图

## 3. 构建产物与体积
| 命令 | 用途 | 产物大约 |
|---|---|---|
| `cargo run` | 日常调试 | 现在 ~100–200 MB（已在 Cargo.toml 限制调试信息） |
| `cargo build --release` | 性能测试 | ~60 MB |
| `cargo build --profile dist` | 发布（fat LTO、strip、panic=abort） | ~29 MB exe，安装包 ~10 MB |

调试版 900 MB 是因为默认 `[profile.dev]` 给所有依赖（GPUI、russh、aws-lc 等）都生成完整调试信息，这是正常现象，不是问题。
仓库的 `Cargo.toml` 已加：
```toml
[profile.dev]
debug = "line-tables-only"      # 本项目只保留行号，崩溃栈仍能定位到行
[profile.dev.package."*"]
debug = false                   # 依赖不生成调试信息
opt-level = 1                   # 依赖轻度优化，界面不卡
```
需要逐行断点调试时，临时把 `debug` 改回 `true`（或 `2`）。

发布包是我在 Linux 上交叉编译的：
```sh
cargo install cargo-xwin
cargo xwin build --profile dist --target x86_64-pc-windows-msvc --bin shellax --bin shellax-cli
tools/package_win.sh        # 生成 ShellAX-Setup-x64.exe / portable zip
```
Windows 本机发布构建直接：`cargo build --profile dist --bin shellax --bin shellax-cli`，产物在 `target\dist\`。

在 Windows 本机打包（推荐）：
```powershell
pwsh -File tools\package_win.ps1                              # 构建 + 打包，产物在 dist\
pwsh -File tools\package_win.ps1 -SkipBuild -Out D:\dist-win  # 不重新构建，换个输出目录
```
脚本从 `Cargo.toml` 读版本号，如果 `packaging/windows/installer.nsi` 或 `shellax.rc` 里的版本不一致就直接报错（避免打出旧版本号的包）；需要 `makensis.exe`，按 PATH、`SHELLAX_MAKENSIS`、`-Makensis` 的顺序查找。
exe 的图标和版本信息由 `build.rs` 在构建时写入：GNU 目标（本机默认 `x86_64-pc-windows-gnu`）调 MinGW 的 `windres`，MSVC 目标调 `llvm-rc` / `rc.exe`。找不到资源编译器时只打印警告，exe 会没有图标和版本信息。
`.cargo/config.toml` 已为 MSVC 目标静态链接 C 运行库（无需 VC++ 运行库）。

## 4. 调试
- 日志：Windows 上 GUI 没有控制台（debug 版也是），`eprintln!` 和 panic 都写到 `%LOCALAPPDATA%\shellax\logs\shellax.log`，上一次运行的在 `shellax.old.log`。
  设置环境变量 `SHELLAX_NO_LOGFILE=1` 后不写文件；Linux/macOS 直接输出到启动它的终端。
- 项目目前没有用 log/tracing 框架，调试信息就是 `eprintln!`，需要更多细节时自己在对应模块加 `eprintln!`。
- 配置：`%APPDATA%\ShellAX\config.toml`；密码和 Key 在 Windows 凭据管理器（keyring），无钥匙串时为加密的 `secrets.enc`。
- 用 VS Code + rust-analyzer + CodeLLDB（或 Windows 上用 `cppvsdbg`）断点调试，`launch.json` 程序指向 `target/debug/shellax.exe`。
- 环境变量前缀统一为 `SHELLAX_`，例如 `SHELLAX_SERVER_DECORATIONS=1` 改用系统标题栏（排查窗口按钮/拖动问题时有用）。

## 5. 测试
```sh
cargo check                       # 最快的编译检查
cargo clippy --all-targets        # 要求 0 警告
cargo test --release              # 单元 + 集成测试
```
部分集成测试需要本机 sshd：`testenv/` 里有测试用 sshd 配置（端口 2222/2224，测试账号仅本地用），参数见 `testenv/test.env`。
没有 sshd 时这些测试会失败，可用 `cargo test --release --lib` 只跑单元测试。
`tools/e2e_hooks.sh` 测 Claude Code / Codex 钩子，需要本机装好对应 CLI。

## 6. 约束（给 AI 助手）
- 密码、API Key 只能进钥匙串/`secrets.enc`，不得写入 config、日志、审计。
- 提交前 `cargo clippy --all-targets` 0 警告。

## 7. macOS（Apple Silicon，如 MacBook Pro M3）

> 注意：macOS 版代码已写好（原生红黄绿按钮、毛玻璃侧栏、Metal 渲染），但还**没有在真机上编译和运行过**，第一次编译可能需要修小问题。

### 需要安装
1. **Xcode**（App Store 安装完整版，约 10+ GB），装完执行：
   ```sh
   sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
   sudo xcodebuild -license accept
   xcodebuild -downloadComponent MetalToolchain   # Xcode 26 起 Metal 编译器需单独下载
   ```
   GPUI 在 macOS 上用 Metal 渲染，构建时要调用 `xcrun metal` 编译着色器，这个编译器只在完整 Xcode 里，
   只装 Command Line Tools（`xcode-select --install`）通常会报 `unable to find utility "metal"`。
2. **Rust**：`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`，目标默认就是 `aarch64-apple-darwin`。
3. **CMake**（aws-lc-rs 需要）：`brew install cmake`（没有 Homebrew 先装 https://brew.sh）。

### 构建与运行
```sh
cargo run                                          # 调试运行，日志直接打印在终端
cargo build --profile dist --bin shellax --bin shellax-cli   # 发布构建，产物在 target/dist/
```
- Intel Mac：`rustup target add x86_64-apple-darwin` 后加 `--target x86_64-apple-darwin`；
  通用包可用 `lipo -create -output ShellAX target/{aarch64,x86_64}-apple-darwin/dist/shellax`。
- 打成 `.app`：可用 `cargo install cargo-bundle && cargo bundle --profile dist`（需在 Cargo.toml 加 `[package.metadata.bundle]`，尚未配置）。
- 未签名的 app 首次打开会被 Gatekeeper 拦截：右键 → 打开，或 `xattr -dr com.apple.quarantine ShellAX.app`。

### 路径
- 配置：`~/.config/shellax/config.toml`（与 Linux 相同，`SHELLAX_CONFIG` 可改路径）；数据（审计、历史、会话快照）：`~/.local/share/shellax/`
- 密码 / Key：macOS 钥匙串（“钥匙串访问”里可见），不会写入配置文件
- 日志：直接输出到启动它的终端（stderr）；从 Finder 启动时不保存 stderr，需要日志请从终端启动 `target/debug/shellax`

### 调试
- VS Code + rust-analyzer + **CodeLLDB** 扩展，`launch.json` 程序指向 `target/debug/shellax`；或命令行 `lldb target/debug/shellax`。
- Metal / GPU 问题：Xcode → Debug → Attach to Process 选 ShellAX，可用 GPU Frame Capture 看渲染。
- 输入法、透明度、毛玻璃这几项只能在 macOS 真机验证。

## 发版

1. 改版本号，三处必须一致（Windows 打包脚本会校验）：
   - `Cargo.toml` 的 `version`
   - `packaging/windows/installer.nsi` 的 `!define VER` 和 `VIProductVersion`
   - `packaging/windows/shellax.rc` 的 `FILEVERSION` / `PRODUCTVERSION`
2. 提交并推送到 main，然后打标签：
   ```sh
   git tag v0.x.y && git push origin v0.x.y
   ```
   标签含 `-beta` / `-rc`（如 `v0.2.0-beta.1`）时发布为预发布版本，只有“测试版”更新通道的用户会收到。
3. GitHub Actions（`.github/workflows/release.yml`）会在 Windows 和 macOS（Apple Silicon）上构建，并把以下文件上传到对应的 Release：
   `ShellAX-Setup-x64.exe`、`ShellAX-portable-x64.zip`、`ShellAX-macos-aarch64.dmg`、`ShellAX-macos-aarch64.zip`、`SHA256SUMS.txt`。
   也可以在 Actions 页面手动运行（workflow_dispatch），填一个已存在的标签。
4. 应用内“检查更新”读取 GitHub Releases：正式版通道读 `releases/latest`，测试版通道读最新一条（含预发布）。
5. 应用内下载并更新（`src/updater.rs`、`src/update_ui.rs`）依赖上面这些文件名，改名时要同步修改代码：
   - 发现新版本后，设置 > 更新里点“下载更新”（或开启“自动下载更新”），后台经 `proxy::ureq_builder` 下载到
     `<数据目录>/updates/<版本号>/`：Windows 下载 `ShellAX-Setup-x64.exe`，macOS 下载 `ShellAX-macos-aarch64.zip`
     （release 地址里的 `.dmg` 自动换成 `.zip`，便于直接替换 .app）。
   - 下载完成后用同一个 release 的 `SHA256SUMS.txt` 校验；自定义 `update_url` 的 manifest 带 `sha256` 字段时用该字段。
     不一致会删除文件并提示原因。所以 **`SHA256SUMS.txt` 必须和安装包一起上传**。
   - 下载记录写在 `<数据目录>/updates/pending.json`（版本号、文件路径、大小、时间），启动时若当前版本已不低于记录里的版本，
     自动删除旧安装包和记录。
   - “退出并更新”：
     - Windows 安装版：以 `ShellAX-Setup-x64.exe /S /RESTART` 启动安装程序后退出。`installer.nsi` 会循环等待
       旧的 `shellax.exe` 可删除（约 30 秒），静默安装到原目录，带 `/RESTART` 时安装结束后重新启动应用。
       便携版（同目录没有 `Uninstall.exe`）只在资源管理器中显示安装包。
     - macOS：用 `ditto -x -k` 解压 zip，生成一个小脚本等待当前进程退出，把旧 .app 挪开、复制新 .app、
       `xattr -dr com.apple.quarantine`，再 `open` 新 .app；失败时恢复旧 .app 并在 Finder 中显示新 .app。
       .app 所在目录不可写或解压失败时，直接在 Finder 中显示安装包，由用户手动安装。
   - 只有从包含此功能的版本升级到更新版本时才会走这个流程。

注意：macOS 包未签名、未公证；Windows 安装包未做代码签名，首次运行可能出现 SmartScreen 提示。
