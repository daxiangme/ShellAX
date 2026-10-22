# ShellAX

ShellAX 是用 Rust + GPUI 编写的跨平台终端 / SSH 客户端，内置面向 AI Agent 的权限管控与审计。

## 功能概览
- **终端**：本地 shell 与 SSH 会话，多标签、分屏（拖动标签到面板边缘分屏）、正则搜索、矩形选择、URL 识别、OSC 52、关键字高亮、Sixel / Kitty 图形协议、asciicast v2 录制回放、广播输入、布局快照。
- **SSH**：公钥 / 密码 / ssh-agent / keyboard-interactive（2FA）认证，known_hosts 校验，ProxyJump，HTTP CONNECT / SOCKS5 代理，端口转发，连接复用。
- **主机管理**：分组（`a/b/c` 嵌套）、颜色标签、生产主机标记、从 `~/.ssh/config` 导入、ssh:// / sftp:// 链接、主机工具（监控 / 进程 / 服务 / Docker）。
- **SFTP**：弹窗 / 标签页 / 分屏 / 独立窗口四种模式，传输队列与断点续传，内置编辑（冲突检测），目录同步（先预览差异）。
- **AI 与 Agent**：内置 AI 对话（命令逐条审批执行、输出脱敏、按主机禁读输出）；外部 Agent（Claude Code / Codex / Grok 等）状态识别；`shellax-cli` hook 与策略服务，所有执行都经过权限引擎并写入审计日志；支持组织托管策略 `/etc/shellax/managed-policy.yaml`。
- **界面**：亮 / 暗主题、主题导入（ShellAX TOML、iTerm2 `.itermcolors`、Alacritty 配色）、命令面板、右键上下文菜单（可自定义）、托盘、中英文界面、可选“减少动态效果”。

## 程序与目录
- GUI：`shellax`（Windows 为 `shellax.exe`）；CLI：`shellax-cli`（hook、check、serve、hosts、exec、upload、download、play 等子命令）。
- 配置：`~/.config/shellax/config.toml`（Windows：`%APPDATA%\ShellAX`）；数据：`~/.local/share/shellax`（Windows：`%LOCALAPPDATA%\shellax`）。
- 环境变量：`SHELLAX_CONFIG` 指定配置文件，`SHELLAX_NO_ANIM=1` 关闭动画，`SHELLAX_SERVER_DECORATIONS=1` 使用系统标题栏。

## 构建
需要 Rust ≥ 1.89。
```bash
cargo build --release
cargo build --profile dist        # 发布构建：LTO + strip
```
Windows 安装包：`tools/package_win.ps1`（Windows 本机）或 `tools/package_win.sh`（交叉编译）。

## 测试
```bash
testenv/setup.sh                  # 生成本地测试用的一次性密钥并填写路径
/usr/sbin/sshd -f $PWD/testenv/sshd_config   # 回环测试 sshd（127.0.0.1:2222）
source testenv/test.env
cargo test --release
```
测试账号、端口和密码只用于本机回环环境。

## 文档
- `docs/PRD.md`：产品需求文档
- `docs/DEVELOPMENT.md`：开发说明
- `docs/WINDOWS_TEST.md`：Windows 测试说明
- `docs/screenshots/`：截图

## 许可证
ShellAX 以 AGPL-3.0 发布，见 `LICENSE`。第三方组件许可见 `THIRD_PARTY_NOTICES.md` 与 `licenses/`。
