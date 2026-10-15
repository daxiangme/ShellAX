# coossh 开发与调试说明

## 1. 环境
- Rust stable ≥ 1.85（`rustup default stable`）
- Windows 本机开发：Visual Studio 2022 Build Tools（勾选“使用 C++ 的桌面开发”，含 Windows 10/11 SDK）。
  GPUI 的 Windows 后端在 **release** 构建时会用 SDK 里的 `fxc.exe` 编译 HLSL 着色器；找不到时可设置环境变量
  `GPUI_FXC_PATH=C:\Program Files (x86)\Windows Kits\10\bin\<版本>\x64\fxc.exe`。debug 构建不需要 fxc。
- Linux：`libxkbcommon-dev libwayland-dev libvulkan-dev libfontconfig-dev libssl-dev clang`
- 最低运行环境：Windows 10 1903+。

## 2. 目录
- `src/main.rs` GUI 入口（二进制 `coossh`），`src/bin/coossh-cli.rs` → CLI `coossh-cli`
- `src/` 其余模块：`ssh.rs`(russh)、`local.rs`(portable-pty)、`config.rs`、`ctxmenu.rs`、权限/审计/AI 等
- `vendor/gpui-pre-windows/` GPUI Windows 后端的补丁副本（Apache-2.0），只为在 Linux 上交叉编译时使用预编译着色器；Windows 本机构建照常走 fxc
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
cargo xwin build --profile dist --target x86_64-pc-windows-msvc --bin coossh --bin coossh-cli
tools/package_win.sh        # 生成 coossh-Setup-x64.exe / portable zip
```
Windows 本机发布构建直接：`cargo build --profile dist --bin coossh --bin coossh-cli`，产物在 `target\dist\`。
`.cargo/config.toml` 已为 MSVC 目标静态链接 C 运行库（无需 VC++ 运行库）。

## 4. 调试
- 日志：Windows 上 GUI 没有控制台（debug 版也是），`eprintln!` 和 panic 都写到 `%LOCALAPPDATA%\coossh\logs\coossh.log`，上一次运行的在 `coossh.old.log`。
  设置环境变量 `COOSSH_NO_LOGFILE=1` 后不写文件；Linux/macOS 直接输出到启动它的终端。
- 项目目前没有用 log/tracing 框架，调试信息就是 `eprintln!`，需要更多细节时自己在对应模块加 `eprintln!`。
- 配置：`%APPDATA%\coossh\config.toml`；密码和 Key 在 Windows 凭据管理器（keyring），无钥匙串时为加密的 `secrets.enc`。
- 用 VS Code + rust-analyzer + CodeLLDB（或 Windows 上用 `cppvsdbg`）断点调试，`launch.json` 程序指向 `target/debug/coossh.exe`。
- 环境变量前缀统一为 `COOSSH_`，例如 `COOSSH_SERVER_DECORATIONS=1` 改用系统标题栏（排查窗口按钮/拖动问题时有用）。

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
- 净室规则：不得阅读或复制 GPL/AGPL 项目（shell-rs、OxideTerm、Warp）和 Zed 的 terminal/editor crate 的源码；只可移植 MIT/Apache/ISC 代码并登记到 `THIRD_PARTY_NOTICES.md`。
- 密码、API Key 只能进钥匙串/`secrets.enc`，不得写入 config、日志、审计。
- 提交前 `cargo clippy --all-targets` 0 警告。
