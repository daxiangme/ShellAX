# ShellAX

[English](README.en.md)

ShellAX 是用 Rust + GPUI 编写的跨平台终端 / SSH 客户端。除终端和主机管理外，还把 SFTP、数据库（含达梦）、Redis、Docker、端口转发和一组日常小工具放进同一个窗口，并为 AI Agent 提供权限管控与审计。

支持平台：Windows x64、Mac（Apple Silicon）。Linux 暂无发布包，可从源码构建。

## 功能概览

### 左侧模块
左侧功能条按模块切换，每个模块有自己的标签页，最近使用的标签会被记住；再次点击当前模块可收起它的工具窗口。

| 模块 | 内容 |
|---|---|
| **主机 / 终端** | 主机树与终端：本地 shell、SSH 会话。底部路径栏和 CPU、内存、磁盘、负载、延迟只在这个模块显示 |
| **SFTP** | 双栏文件管理，两侧各自选择“本机”或任一已保存主机 |
| **数据库** | 连接管理、表浏览与 SQL 编辑器，另有文档型数据库浏览 |
| **Redis** | 键树、按类型查看与编辑、控制台 |
| **Docker** | 本机或经 SSH 访问的容器引擎 |
| **端口** | 端口转发管理 |
| **工具** | 44 个内置小工具与可下载的扩展包 |

窗口底部的工具窗口有两个标签：**传输队列**与**执行日志**（记录各模块执行过的操作）。

### 终端与 SSH
- **终端**：本地 shell 与 SSH 会话，多标签、分屏（拖动标签到面板边缘分屏）、正则搜索、矩形选择、URL 识别、OSC 52、关键字高亮、Sixel / Kitty 图形协议、asciicast v2 录制与回放（录制默认脱敏、不记录无回显输入）、广播输入（目标含生产主机时需确认）、布局快照与回滚、启动时恢复上次布局。
- **命令建议与历史下拉**：基于历史的灰色命令建议（→ / Tab 接受）；在提示符输入时，光标下方可列出匹配的历史命令，上下键选择，回车或 Tab 填入当前行，Esc 关闭（可在设置中关闭）。历史可按主机记录，也可读取远端 shell 历史，含密钥等敏感命令会被过滤，过滤规则可自定义。
- **SSH**：公钥 / 密码 / ssh-agent / keyboard-interactive（2FA）认证，known_hosts 校验，ProxyJump，HTTP CONNECT / SOCKS5 代理，连接复用，断线自动重连，可选用远端 tmux 保持会话。
- **主机管理**：分组（`a/b/c` 嵌套）、颜色标签、生产主机标记、右键更换分组 / 位置 / 主机图标（8 色、12 个文件夹字形、11 个主机图标）、从 `~/.ssh/config` 导入、`ssh://` / `sftp://` 链接、SSH 密钥生成与导入；密钥口令与 API Key 存入系统钥匙串，不可用时退回加密文件，不写入配置文件。

### SFTP
SFTP 在独立标签页中打开，两侧各自选择“本机”或任一已保存主机，本机与远端、远端与远端之间均可传输；传输队列支持断点续传；内置文本编辑（带冲突检测）；目录同步（单向或双向，先预览差异，删除需确认）。

### 数据库
- 连接：MySQL、MariaDB、StarRocks、OceanBase、PostgreSQL、openGauss、Kingbase、达梦、SQLite、SQL Server、ClickHouse，以及 MongoDB（独立的文档标签）。可直连，也可经 SSH 隧道；连接可标记为只读或生产。左侧树可按连接名和已打开的表名搜索，匹配文字会高亮。
- 表浏览：服务端分页、筛选、排序，暂存式编辑（确认后提交），结构与 DDL 视图，表设计（列、索引、外键变更）。
- SQL 编辑器：语法高亮、补全、多结果标签、查询历史、已保存查询。
- 导出 CSV / JSON / SQL INSERT，导入 CSV / JSON。
- MongoDB：库与集合树，带 JSON 筛选 / 排序 / 投影的分页文档浏览，树形或 JSON 视图，编辑 / 插入 / 删除，索引，基础聚合管道编辑，JSON 导出。
- DuckDB 为可选编译特性（`--features duckdb`），发布包不含。
- **数据库 MCP**：在本机 `127.0.0.1` 提供 MCP 服务，目前支持 MySQL 与达梦。可按库、表授权，写操作需要确认并记入审计。端口默认自动分配并记住，只有手动刷新才会更换；也可以指定固定端口。令牌和接入配置可从右侧卡片复制到 Cursor、Claude Code 或 Codex。

### Redis
单机 / 哨兵 / 集群连接，ACL 用户，TLS，可经 SSH 隧道。键树基于 SCAN（不使用 KEYS），按类型（string、hash、list、set、zset、stream）查看与编辑，TTL、重命名、删除，服务器信息、慢日志与命令控制台（按风险提示危险命令）。

### Docker
连接本机引擎，或经 SSH 连接远端引擎。容器按 compose 项目分组，启停 / 重启 / 暂停 / 删除等操作、详情、日志与 exec（各自打开标签）、compose 操作，以及镜像（拉取 / 删除）、卷、网络。

### 端口转发
本地（-L）、远程（-R）与动态 SOCKS5（-D）转发；保存的转发可手动启停，也可在连接到对应主机时自动启动；显示实时状态、连接数与流量。

### 工具与扩展包
内置 44 个小工具，分 9 类：单位与财务、文本、开发、加密编码、日期时间、生成、中文与时区、图片与条码、PDF。支持收藏与最近使用；多条结果逐条显示并保留历史；输入与输出区高度随内容自适应，大文本输入输出之间可拖动分隔条。

其中一部分需要下载扩展包后使用。扩展包是 WebAssembly 模块，下载到 `<配置目录>/ext/<id>/`，在沙箱中运行（无任何导入、限制指令与内存、每次调用新建实例）。详见 `docs/EXTENSIONS.md`。

| 扩展包 | 工具 |
|---|---|
| `lang-zh` | 汉字转拼音、简繁转换 |
| `worldtime` | 世界时钟、时区换算 |
| `image` | 二维码生成 / 识别、条形码、图片压缩、图片水印 |
| `pdf` | PDF 合并、拆分、旋转、删页、重排、水印、页码、图片转 PDF、提取文本、清理元数据 |
| `pdf-render` | PDF 转图片 |

### 右侧栏
右侧栏有六张卡片：**主机工具**、**AI 助手**、**常用命令**、**历史命令**、**目录**、**数据库 MCP**。设置 → 外观中可选择允许多张卡片同时展开，或同一时间只显示一张（打开新卡片会替换当前卡片）。数据库 MCP 仍在监听时，卡片收起或关闭后，右侧图标上会有一个圆点。
- **主机工具**：对当前主机（本机或 SSH）查看系统监控、进程、服务（systemd）与 Docker 容器，并可执行结束进程、服务 / 容器操作；所有写操作先经权限引擎判定并记入审计。本机为 Windows 时只支持系统概览与进程。
- **AI 助手**：多轮流式对话，可按设置附带当前终端的上下文；命令失败后可一键“解释失败”；数据库、Redis、Docker 视图有各自的上下文与快捷提问。生成的命令只“插入”（不回车），或“执行”——是否放行由权限模式与规则决定。
- **常用命令**：所有模块共用同一份列表，按分组保存命令，支持 `{{变量}}` 填写后再插入或执行：焦点在 SQL 编辑器、Redis 控制台或终端时分别插入到对应位置（Redis 控制台需点击才会执行），没有可插入位置时复制到剪贴板；多行脚本执行前需确认；列表为空时可一键添加终端、SQL、Redis、Docker 示例。
- **历史命令**：按当前主机或全部主机查看、搜索，可插入、复制、存为常用命令、删除单条或清空当前主机。
- **目录**：浏览某个终端标签的当前目录。本机直接读磁盘，SSH 走已经建立的 SFTP。
- **数据库 MCP**：见上文。卡片里管理令牌、监听地址、超时、最大行数，以及每张表的可见范围。

### AI 与 Agent 管控
- **内置 AI（自带 Key）**：支持 OpenAI 兼容接口与 Anthropic 接口，可配置多个服务商、列出并选择模型、测试连接；API Key 存入系统钥匙串。在提示符输入 `# 需求` 回车（或 Ctrl+Shift+G）用自然语言生成命令。
- **隐私**：发送前脱敏密钥与令牌；可选择上下文范围（不发送 / 上一条命令 / 选中内容 / 当前屏幕）、是否带命令输出；可按主机名模式禁止发送输出；首次发送前提示将离开本机的内容。
- **权限引擎**：三种模式（询问 / 只读计划 / 自动）、风险等级与允许 / 询问 / 拒绝规则，生产主机写操作与危险命令可强制确认；Ctrl+Shift+M 循环切换模式。
- **外部 Agent**：识别运行在终端中的命令行 Agent 的状态（空闲 / 工作中 / 等待审批），可在状态变化时发桌面通知；审批超时可自动拒绝或继续等待。
- **`shellax-cli` 与本地策略服务**：为外部 Agent 安装命令钩子，所有命令经权限引擎判定；Agent 也可通过 `exec` / `upload` / `download` 在已保存的主机上操作，只需给出主机名，凭据与密钥始终留在 ShellAX 内。
- **审计**：追加式 JSONL 日志，带 SHA-256 哈希链，保留天数可配置；Ctrl+Shift+L 打开审计查看。
- **组织托管策略**：`/etc/shellax/managed-policy.yaml`（只读、优先级最高）可强制硬拒绝规则、限定权限模式、对生产主机限制风险等级、设定审计保留下限。

### 界面与设置
- 亮 / 暗主题与跟随系统，内置多套主题与终端配色，可导入配色（ShellAX TOML、`.itermcolors` 与通用 TOML 配色文件）；中英文界面；可选“减少动态效果”；系统托盘（Linux，显示运行中的 Agent 数量）；命令面板（Ctrl+Shift+P）；可自定义右键上下文菜单；快捷键可在设置中修改。
- 设置分为：通用、外观、终端、命令建议、SSH、代理、凭据与密钥、AI、外部 Agent、权限与安全、标签、审计、会话与布局、快捷键、通知、右键菜单、扩展、关于。

### 更新
- 默认从 GitHub Releases 检查更新，可选“稳定版 / 测试版”通道；也可指定自定义清单地址（`general.update_url` 或 `SHELLAX_UPDATE_URL`）。
- 可手动下载或开启自动下载；下载后按 SHA256 校验，退出后安装。Windows 安装版使用安装包静默更新并重启；Mac 在可写入时直接替换应用，否则打开安装包所在位置；便携版会在文件夹中显示安装包。

## 截图

界面截图稍后补充。

## 安装

### Windows
从发布页下载 `ShellAX-Setup-x64.exe`（安装包）或 `ShellAX-portable-x64.zip`（便携版）。安装包未做代码签名，SmartScreen 可能提示“已保护你的电脑”，点“更多信息 → 仍要运行”。

### Mac
打开 `ShellAX-macos-aarch64.dmg`（或使用同名 `.zip`），把 ShellAX 拖到 Applications。应用仅做 ad-hoc 签名、未经公证，首次打开若提示“无法验证开发者”：
- 在“应用程序”中右键 ShellAX →“打开”；或
- “系统设置 → 隐私与安全性”中点击“仍要打开”；或
- 终端执行 `xattr -dr com.apple.quarantine /Applications/ShellAX.app`。

## 程序与目录
- GUI：`shellax`（Windows 为 `shellax.exe`）。
- CLI：`shellax-cli`，子命令：
  - `status`：查看本地策略服务状态
  - `check`：询问某条命令的权限结果（退出码 0 允许 / 1 询问 / 2 拒绝）
  - `hook`、`install`：外部 Agent 的命令钩子及其安装 / 卸载
  - `serve`：无界面运行策略服务
  - `hosts`、`exec`、`upload`、`download`：在已保存的主机上列出、执行、传输
  - `play`：播放 asciicast 录制
  - `daemon`：本地常驻进程，窗口关闭后 shell 仍保留（需在设置的“会话与布局”中开启；仅 Linux / Mac）
- 配置：`~/.config/shellax/config.toml`（Windows：`%APPDATA%\ShellAX`）；数据：`~/.local/share/shellax`（Windows：`%LOCALAPPDATA%\shellax`）。
- 环境变量：`SHELLAX_CONFIG` 指定配置文件，`SHELLAX_NO_ANIM=1` 关闭动画，`SHELLAX_SERVER_DECORATIONS=1` 使用系统标题栏，`SHELLAX_UPDATE_URL` 指定自定义更新清单。

## 构建
使用 Rust stable 工具链（CI 与发布均使用 stable）。平台依赖与调试方法见 `docs/DEVELOPMENT.md`。
```bash
cargo build --release
cargo build --profile dist        # 发布构建：LTO + strip
```
Windows 安装包：`tools/package_win.ps1`（Windows 本机）或 `tools/package_win.sh`（交叉编译）。扩展包是独立工作区，构建与发布见 `docs/EXTENSIONS.md`。

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
- `docs/EXTENSIONS.md`：扩展包格式、现有扩展包与发布
- `docs/UI_REDESIGN_PLAN.md`：界面设计规则
- `docs/WINDOWS_TEST.md`：Windows 测试说明
- `docs/screenshots/`：截图

## 许可证
ShellAX 以 AGPL-3.0 发布，见 `LICENSE`。第三方组件许可见 `THIRD_PARTY_NOTICES.md` 与 `licenses/`。
