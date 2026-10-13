# TEST_REPORT — AgentShell 原型对照 PRD v1.2（第 14 轮：5.21 SFTP 显示模式，2026-10-13）

标记：**通过** / **失败**（PRD 要求未实现或不达标）/ **未验证**（只因硬件或平台限制无法在本机验证，写明原因）。
证据：`T:` 自动化测试；`G:` 本轮在 DISPLAY=:2 实测（截图本轮逐张看过）；`P:` 前几轮 GUI 实测（PROGRESS.md 有记录，截图在仓库）；`M:` 手工测量。
代码基线：第 14 轮（本报告随同一 commit 提交）；第 0 节为本轮重跑结果，第 1–3 节沿用第 12 轮结论，第 5b 节为第 13 轮 5.20 清单，第 5c 节为本轮新增 5.21 清单。G: 截图都在 DISPLAY=:2（1920×1200）全屏截取，本轮逐张看过。

## 0. 构建与自动化测试

| 项 | 命令 | 结果 |
|---|---|---|
| release 构建 | `cargo build --release`（`~/.cargo/bin` 工具链） | **通过**，0 警告 |
| dist 构建 | `cargo build --profile dist`（fat LTO、cgu=1、panic=abort、strip） | **通过**，0 警告；二进制 42.8MB |
| clippy | `cargo clippy --release --all-targets` | **通过**，0 警告 0 错误 |
| 全量测试 ×3 | `source testenv/test.env; eval $(ssh-agent -s); ssh-add testenv/client_ed25519; cargo test --release -- --nocapture` | **通过**：3 轮均 152/152（lib 107 含新增 sftp_mode 3 + session_store sftp 1、agentshell 8、GUI bin 16、ctxmenu_ssh 1、daemon 1、headless 8、managed_policy 1、sftp_modes_ssh 1（新增）、ssh_features 9），0 失败 0 忽略 |
| 跳过检查 | 在 --nocapture 输出里 grep skip | 0 个跳过（sshd 2222 公钥 / 2223 密码+转发 / 2224 keyboard-interactive 都在运行，SSH_AUTH_SOCK 已设） |
| 外部 Agent hook 端到端 | `tools/e2e_hooks.sh`（真实 Claude Code / Codex / Grok CLI + mock LLM） | **通过** 7/7 |
| 许可证（第 13 轮） | 右键菜单全部自写（src/ctxmenu.rs、src/ctxmenu_view.rs），无新增依赖；未打开 research/ 下 shell-rs、oxideterm、warp，未读 Zed terminal/terminal_view/editor | 通过 |
| 许可证（第 12 轮） | 新增代码全部自写（Sixel/Kitty 解码、asciicast、目录同步、广播、快照均按公开规范自写）；本轮新增直接依赖 image 0.25（MIT OR Apache-2.0，原本已是 gpui 的传递依赖）已登记 THIRD_PARTY_NOTICES.md；未打开 research/ 下 shell-rs、oxideterm、warp，未读 Zed terminal/terminal_view/editor | 通过 |

## 1. 用户故事（PRD 2.2）

| ID | 验收标准 | 结果 | 证据 |
|---|---|---|---|
| US-01 | 导入 ~/.ssh/config；可编辑；不存明文秘密 | 通过 | T: hosts::tests::parses_ssh_config / jump_chain_resolution，config::tests::api_key_never_written_to_config；G: screenshot-final-host-groups-os.png |
| US-02 | 未知 key 显示 SHA256 并确认；变更 key 默认拒绝 | 通过 | T: ssh_localhost_ls_and_known_hosts_tofu；P: screenshot-final-hostkey.png |
| US-03 | 多标签/分屏；**拖标签到边缘分屏**；vim/htop/tmux 正常 | 通过 | 本轮新增拖放：T: layout::tests::insert_subtree_at_edges；G: screenshot-final-tab-drag-zone.png（拖动时左边缘高亮）→ screenshot-final-tab-drag-split.png（标签并入当前标签左侧）；P: screenshot-v4-htop-vim.png |
| US-04 | 识别 claude/codex/grok，blocked 时通知 | 通过 | T: agent::tests::*、local_agent_identified_by_foreground_process；P: 7.1 |
| US-05 | 持久会话，重开自动 reattach | 通过 | T: daemon_keeps_shell_alive_across_detach；P: screenshot-final-restore-reattach.png |
| US-06 | 自然语言→命令（默认只插入） | 通过 | T: ai::tests::extracts_command；P: screenshot-v6-nl-card.png |
| US-07 | 一键解释失败命令（脱敏） | 通过 | T: ai::tests::redacts_common_secrets、suggest::tests::osc133_mark_wins；P: 6.2 |
| US-08 | 内置 AI 执行逐条过策略、审批、审计，结果回传 | 通过 | T: chat_view::tests::plan_steps_and_approvals；P: screenshot-v10-ai-chat-step-approval.png、screenshot-v11-ai-feedback.png |
| US-09 | prod 主机 R3+ 一律拒绝 | 通过 | T: policy::tests::production_is_stricter、policy_server::tests::production_host_header |
| US-10 | `agentshell exec/hosts/upload/download`，凭据只在 App 内 | 通过 | T: policy_server::tests::remote_ops_go_through_policy；P: screenshot-v10-agentshell-upload-approval.png |
| US-11 | 审计按主机/会话/决策过滤，导出 JSONL | 通过 | T: audit::tests::filter_by_host_session_decision；P: screenshot-v10-audit-filter-*.png |
| US-12 | SFTP 双栏拖拽、断点续传 | 通过 | T: sftp_ops_and_resumable_transfers；P: screenshot-final-sftp.png |
| US-13 | -L/-R/-D，随 App 启动 | 通过 | T: port_forwards_local_remote_socks；P: screenshot-final-port-forwarding.png |
| US-14 | Agent 总览排序、点击跳转 | 通过 | P: screenshot-final-agent-page.png |
| US-15 | 组织托管策略（只读、最高优先级） | 通过 | T: managed::tests::parse_and_rules、managed_policy_overrides_user_settings；P: screenshot-final-managed-policy.png |

## 2. 功能需求（PRD 第 3 章）

### 3.1 SSH
| 功能 | 结果 | 证据 |
|---|---|---|
| 主机管理：搜索、生产标记 | 通过 | P: screenshot-v5-sidebar |
| 主机分组任意嵌套 | 通过 | 本轮：组名写 `a/b/c` 即嵌套，祖先自动生成、折叠父组隐藏子组、计数含子组；T: outline::tests::nested_rows_and_collapse；G: screenshot-final-host-groups-os.png |
| 主机拖拽 | 通过 | 本轮：拖动配置主机到组标题即改组并写回 config（审计 hosts.move）；G: scratch 拖到 prod/db，config.toml 写入 `group = "prod/db"`（screenshot-final-host-drag-group.png） |
| OS 识别徽章 | 通过 | 本轮：连接后一次只读探测（uname + os-release ID，审计 hosts.os_probe），侧栏徽章 Ub/Db/Ma/Wi… 和“· Debian”；T: outline::tests::os_probe；G: 本机 sshd 显示 Db/Debian |
| 认证链 agent→publickey→password→keyboard-interactive | 通过 | T: agent_auth、passphrase_key、password_auth_prompt_save_and_reuse、keyboard_interactive_auth |
| Host key / known_hosts | 通过 | 同 US-02 |
| ProxyJump 多跳 | 通过 | T: proxyjump_and_remote_forward |
| HTTP/SOCKS5 出站代理（SSH、AI、更新检查） | 通过 | T: proxy::tests::socks5_and_http_connect、http_clients_use_proxy_and_no_proxy；P: screenshot-v10-proxy-settings.png |
| 连接复用、keepalive、重连 | 通过 | T: connection_reuse；P: 2.5 |
| SFTP 基础 | 通过 | 同 US-12 |
| SFTP 内置编辑 | 通过 | T: sftp_ops_and_resumable_transfers（往返、冲突、二进制/大小限制）；P: screenshot-final-sftp-editor.png、screenshot-final-sftp-editor-conflict.png |
| SFTP 目录同步（P1） | 通过 | T: dirsync::tests::plans、tests/ssh_features.rs::sftp_directory_sync（单向不删/带删除、双向较新者胜、mtime 保留后幂等）；G: SFTP「同步…」→ 差异预览（↑/+/✕ 着色）→ 有删除时按钮变为“确认删除 3 项并同步”需再点一次 → 传输完成、远端只剩本地文件（screenshot-final-sftp-sync-preview.png / -delete-confirm.png / -done.png，ssh 核对远端文件列表与内容） |
| 端口转发 | 通过 | 同 US-13 |
| 端口转发规则图示（P1） | 通过 | T: forward::tests::diagram_nodes；G: 每条规则显示「本机/远端监听 ══SSH 隧道══▶ SSH 服务器 ──转发──▶ 目标」，运行中连线变绿、出错灰色并显示错误（screenshot-final-port-forward-diagram.png：-D 运行中、-L 端口被占用） |
| 钥匙串凭据 | 通过 / 未验证 | T: secrets::tests::*（本机走 0600 加密文件回退）；macOS Keychain / Windows Credential Manager 无对应平台，未验证 |
| Agent 转发（默认关、按主机开） | 通过 | P: 2.4 |
| 凭据页 / 生成导入密钥 | 通过 | T: keygen::tests::generate_import_and_load |
| ssh:// sftp:// 链接、Xshell -url | 通过 | T: link::tests::parses_links；P: screenshot-v11-sftp-link.png / -ssh-link.png |
| 主机侧栏工具（监控/进程/服务/Docker） | 通过 | T: hosttools::tests::parsers_and_actions；P: screenshot-final-hosttools-monitor.png、screenshot-v11-hosttools-kill-ask.png |

### 3.2 终端
| 功能 | 结果 | 证据 |
|---|---|---|
| 仿真（真彩、鼠标、bracketed paste、备用屏、OSC 8） | 通过 | T: termview::tests::*；P: v4-htop-vim |
| OSC 52（写 allow/ask/deny，读默认关） | 通过 | T: term::tests::osc52_policy / osc52_store_and_load_reach_listener |
| 本地 Shell bash/zsh | 通过 | T: local_pty_ls、local_zsh_integration_and_history |
| 多标签/分屏/拖拽比例/拖标签分屏 | 通过 | 同 US-03 |
| 正则搜索、矩形选择、URL 识别 | 通过 | T: term::tests::regex_search_smart_case_and_direction / block_selection_hit_test / url_detection；P: screenshot-final-terminal-search.png、screenshot-final-block-select-url.png |
| OSC 133 命令块 | 通过 | T: local_bash_osc133_prompt_mark |
| 通知 OSC 9/777/BEL | 通过 | T: term::tests::osc_scan；系统通知弹窗本机无通知守护进程，仅验证到调用与审计 |
| 字体/CJK/Nerd Font | 通过 | P: screenshot-v3-fonts |
| 主题 + Alacritty/iTerm2 导入 | 通过 | T: theme::tests::*、theme::iterm_tests::imports_itermcolors |
| 历史建议 + AI 灰字 | 通过 | T: suggest::tests::*；P: screenshot-final-ai-inline-suggest.png |
| 关键字高亮（着色） | 通过 | T: highlight::tests::rules_and_spans；G: screenshot-final-keyword-highlight.png（OK/PASS 绿、ERROR/failed 红、warn/deprecated 黄） |
| 关键字触发通知（P1，PRD 写“着色/通知”） | 通过 | T: notify_rules::tests::fires_and_rate_limits、headless::local_pty_graphics_record_notify；G: `[[notify.keywords]]`（默认 build/deploy failed/succeeded/finished、OOM/段错误/kernel panic），命中输出行弹出提示并发桌面通知（本机无通知守护，日志显示 D-Bus 不可用，界面提示可见），审计 notify.keyword 已脱敏，刚输入的命令回显行不触发，每规则 10 秒限流（screenshot-final-keyword-notify-recording.png 右下角） |
| IME 预编辑 | 未验证 | 代码已实现 EntityInputHandler；本机没有 fcitx/ibus 输入法程序 |
| 光标闪烁 CPU | 通过 / 未验证 | M: 软件渲染下光标常亮，空闲 0.4% CPU；硬件 GPU 下的闪烁路径没有真 GPU 可测 |
| 图形协议 Sixel/Kitty（P2） | 通过 | T: graphics::tests::sixel_and_kitty、headless::local_pty_graphics_record_notify、headless::graphics_in_single_chunk_keep_order；G: Sixel（RGB/HLS 调色、重复、换行）与 Kitty（a=T，f=24/32/100 PNG，分块 m=1）在网格中按位置显示并预留行，clear 后清除（screenshot-final-graphics-sixel-kitty.png）。未做：Kitty 的放置/删除命令（a=p/d）、文件/共享内存传输、动画 |
| 录制回放 asciicast（P2） | 通过 | T: asciicast::tests::record_redact_and_replay（跨读分片的密钥也脱敏、关回显段写“[input hidden]”、空闲压缩）；G: Ctrl+Shift+R 开始/停止（面板右上“● 录制中（已脱敏）”），命令面板“回放录制”列出文件，点开在新标签里用 `agentshell play` 按时间回放，DEMO_TOKEN 显示为 [REDACTED]，图片也能回放（screenshot-final-recordings-list.png / -asciicast-playback.png） |
| 广播输入（P2） | 通过 | T: broadcast::tests::gating；G: Ctrl+Shift+B 对当前标签全部面板广播，面板红框+底部红条“广播输入中 · 同时发送到 N 个面板”，本地与 SSH 面板同时收到 `echo hello-from-broadcast`（screenshot-final-broadcast.png）；含生产主机时先弹确认框（screenshot-final-broadcast-prod-confirm.png），审计 broadcast.start/confirm/stop |

### 3.3 外部 Agent
| 功能 | 结果 | 证据 |
|---|---|---|
| 识别（进程名 + 屏幕 manifest） | 通过 | T: agent::tests::* |
| 状态 UI、通知、总览 | 通过 | P: v4-agents、7.1 |
| 会话恢复 resume | 通过 | P: 5.4（grok `--resume` 参数未在本机核实） |
| 权限钩子 Claude/Codex/Grok | 通过 | T: hooks::tests::*、policy_server::tests::*；e2e 7/7 |
| 远端 hook 回传 | 通过 | P: 1.6 |
| 凭据桥 agentshell exec/hosts/upload/download | 通过 | 同 US-10 |
| 快速启动 | 通过 | T: config::ai_output_tests::quick_launch_and_new_defaults；P: screenshot-v11-agent-quick-launch*.png；G: Ctrl+Shift+O「快速启动：选目录 + 预设布局」——目录候选（焦点面板 cwd、HOME、快照里的目录、浏览…）+ 预设 单面板/左右/主+两侧/2×2/Agent + 日志 tail -f（screenshot-final-quickstart-dialog.png、-grid4.png：4 个面板 pwd 均为所选目录；-agent-tail.png：左侧 Agent 被识别、右侧 tail -F 最新 app.log）。“浏览…”用系统目录选择门户，本机无 xdg-desktop-portal 未实测 |
| write_to_pty 权限 | 通过 | T: policy::pty_tests::write_to_pty_modes；P: screenshot-v11-write-to-pty-ask.png |

### 3.4 内置 AI
| 功能 | 结果 | 证据 |
|---|---|---|
| inline 建议 / 自然语言→命令 / 错误解释 / 脱敏 | 通过 | T: ai::tests::* |
| 对话面板（流式、停止、插入） | 通过 | T: ai::stream_tests::*、chat_view::tests::code_block |
| Agentic 执行逐条审批 + 结果回传 | 通过 | 同 US-08 |
| 首次发送提示、按主机禁读输出 | 通过 | T: config::ai_output_tests::no_output_hosts；P: screenshot-v10-ai-first-send-notice.png |
| AI 请求走代理 | 通过 | T: proxy::tests::http_clients_use_proxy_and_no_proxy |
| BYOK：OpenAI 兼容 / Anthropic / Ollama / OpenRouter | 通过 / 未验证 | T: models::tests::*、ai::tests::anthropic_headers；只用本地 mock 验证，没有真实模型 key，真实服务商未验证 |

### 3.5 权限
| 功能 | 结果 | 证据 |
|---|---|---|
| Ask/Plan/Auto、规则 last-match-wins、R0–R4、分段 | 通过 | T: policy::tests::* |
| 主机 env 标签 | 通过 | T: policy::tests::production_is_stricter |
| 审批卡（允许一次 / 本会话总是允许 / 拒绝并说明原因）、超时 | 通过 | T: policy_server::tests::approvals_and_timeout / session_allow_and_deny_with_reason；G: screenshot-final-approval-withdraw-before.png（四个按钮） |
| **调用方断开自动撤销审批卡、不执行、写审计** | 通过 | 1658b6a 已实现。T: policy_server::tests::approval_withdrawn_when_caller_disconnects（exec 与 hook 两路；迟到的“允许”不执行）。G: 在本地面板运行 `agentshell exec sshtest-local -- touch /tmp/late_marker_remote` → 弹卡（screenshot-final-approval-withdraw-before.png）→ kill -9 CLI → 卡片 1 秒内消失（screenshot-final-approval-withdraw-after.png）；远端 /tmp/late_marker_remote 不存在；审计 seq 272 `hook.decision … effect=Deny reason="… caller disconnected, approval withdrawn"` |
| 审计 JSONL + 哈希链 + 保留期 + 过滤 | 通过 | T: audit::tests::* |
| 组织托管策略 | 通过 | 同 US-15 |

### 3.6 会话与布局
| 功能 | 结果 | 证据 |
|---|---|---|
| 布局持久化/恢复 | 通过 | T: session_store::tests::roundtrip |
| tmux 远端持久 / 本地 daemon | 通过 | 同 US-05 |
| 断线重连 | 通过 | P: 2.5 |
| 托盘（Agent 数、显示/新建/退出） | 通过 / 未验证 | T: tray::tests::summary_and_labels；G: 私有 dbus-daemon + examples/sni_watcher，busctl 读到 Title “AgentShell · 没有运行中的 Agent”、Status Active、菜单 显示窗口/新建本地终端/退出（screenshot-final-tray.png，D-Bus 输出截图）。本机没有桌面面板，图标的真实显示未验证；macOS/Windows 托盘未实现也无平台可测 |
| 布局快照 N 份可回滚（P2） | 通过 | T: snapshot::tests::ring_and_roundtrip；G: 保存布局时自动快照（去重，最多 10 份）+ 命令面板“保存布局快照”，“回滚布局快照…”列表点选后先快照当前布局再重建（screenshot-final-layout-snapshots.png → screenshot-final-layout-rollback.png，3 标签回到 2 标签），审计 layout.snapshot/rollback |

### 3.7 设置全表
| 项 | 结果 | 证据 |
|---|---|---|
| 所有设置有实际效果（无“即将支持”） | 通过 | rg 0 残留 |
| 配置写回保留注释 | 通过 | T: config::tests::save_preserves_comments |
| 窗口透明度 | 未验证 | llvmpipe 下 wgpu 回退为不透明，需真 GPU |
| 界面字号 | 通过（部分） | 自绘文本仍是固定 px |
| 更新检查（走代理） | 通过 | T: update::tests::version_compare、proxy::tests::http_clients_use_proxy_and_no_proxy |

## 3. 非功能需求（本机软件渲染）
| 指标 | 目标 | 实测 | 结果 |
|---|---|---|---|
| 冷启动 | < 400ms | 进程到窗口 62ms | 通过 |
| 空闲内存 | < 80MB | RSS 169MB，大部分是 llvmpipe | 未验证（需真 GPU） |
| 空闲 CPU | 低 | 0.4% | 通过 |
| 安装包 | < 60MB | dist 二进制 45.2MB，gzip 17.8MB | 通过 |
| 跨平台 | macOS/Windows/Linux | 只测了 Linux X11 | 未验证（无 macOS/Windows） |

## 4. 本轮截图（DISPLAY=:2 全屏，都看过）
- screenshot-final-graphics-sixel-kitty.png：Sixel 彩条 + Kitty PNG，文字在图片下方续排
- screenshot-final-keyword-notify-recording.png：录制中标记 + 关键字通知提示
- screenshot-final-recordings-list.png / screenshot-final-asciicast-playback.png：录制列表；回放（令牌已脱敏）
- screenshot-final-broadcast.png / screenshot-final-broadcast-prod-confirm.png：广播输入；含生产主机确认
- screenshot-final-layout-snapshots.png / screenshot-final-layout-rollback.png：快照列表；回滚后
- screenshot-final-quickstart-dialog.png / -grid4.png / -agent-tail.png：快速启动
- screenshot-final-port-forward-diagram.png：转发图示
- screenshot-final-sftp-sync-preview.png / -delete-confirm.png / -done.png：目录同步
- screenshot-final-late-allow-1-card.png / -2-ignored.png：审批撤销后迟到的允许
- 前几轮 screenshot-final-*.png 保留

## 4b. 本轮另修
| 项 | 结果 | 证据 |
|---|---|---|
| 策略服务审计路径遵守 XDG_DATA_HOME（与 GUI 一致） | 通过 | config::audit_dir：默认目录改为 `$XDG_DATA_HOME/agentshell/audit`（未设时仍是 ~/.local/share/agentshell/audit），显式配置的目录原样使用；GUI 与 agentshell serve/CLI 共用；T: config::audit_dir_tests::default_follows_xdg（GUI bin 与 agentshell bin 各跑一次）；G: 隔离环境 XDG_DATA_HOME=/tmp/stp-gui/data 下 agentshell exec 的审计写入 /tmp/stp-gui/data/agentshell/audit/audit.jsonl |
| OS 徽章缓存键按主机条目 | 通过 | 缓存键由 user@host:port 改为 SshTarget::pool_key（含跳板链），经不同跳板到同名地址的条目不再串用徽章 |
| GUI 验证“审批撤销后迟到允许不执行” | 通过 | 测试钩子 `AGENTSHELL_TEST_SLOW_WITHDRAW=1`：调用方断开后卡片仍留在界面。G: `agentshell exec prod-db -- touch /tmp/late-allow-proof` 弹卡 → kill CLI → 2.5 秒后点“允许一次” → 提示“该审批已被撤销（调用方已断开），迟到的应答未执行”；审计 seq 72 `withdrawn caller-disconnected`、seq 74 `late-answer-ignored`；ssh 检查远端文件不存在（screenshot-final-late-allow-1-card.png / -2-ignored.png）。另有 T: policy_server::tests::approval_withdrawn_when_caller_disconnects |

## 5. 统计
第 1–3 节逐行计 86 行：**通过 82**（其中 4 行“通过 / 未验证”：钥匙串真实平台、硬件 GPU 光标闪烁、真实模型服务商、托盘真实面板；1 行“通过（部分）”：界面字号），**失败 0**，**未验证 4**（IME、透明度、空闲内存、跨平台——都是本机硬件/平台限制：没有输入法程序、没有真 GPU、只有 Linux X11）。
第 0 节、4b 节全部通过。

## 5b. 第 13 轮：PRD 5.20 右键上下文菜单

| 项 | 结果 | 证据 |
|---|---|---|
| 类型判定（zip/tar/tar.gz/tgz/tar.xz/tar.bz2/7z/rar/gz、目录、文本/配置、脚本、可执行、日志） | 通过 | T: ctxmenu::tests::type_detection |
| 终端输出中的路径 / URL / IP:端口检测（绝对、~/、./、../、相对目录、带扩展名文件名、`:行号`、CJK、版本号不误判） | 通过 | T: ctxmenu::tests::detection_in_terminal_output |
| 相对路径用 cwd 解析（OSC 7 → 本地 /proc → `user@host:dir$` 提示符兜底；拿不到时保留相对路径并在菜单头提示） | 通过 | T: ctxmenu::tests::resolve_relative_with_cwd、term::tests::osc_scan（OSC 7）；G: screenshot-final-ctxmenu-terminal-path.png（tmux 内远端会话，经提示符解析为 ~/ctxdemo/backup.tar.gz） |
| 引号转义（空格、单双引号、`$()`、反引号、CJK、emoji、换行；`~/` 保持可展开；`-` 开头加 `./`） | 通过 | T: ctxmenu::tests::quoting_spaces_quotes_cjk_unicode（用真实 sh 往返校验） |
| 模板展开 {path} {name} {dir} {stem} {ext} {host}（全部转义，`{{`/`}}` 字面量，未知占位符报错） | 通过 | T: ctxmenu::tests::template_expansion |
| 工具探测与安装提示（apt/yum/dnf/apk/pacman/zypper/brew，按主机缓存 10 分钟） | 通过 | T: ctxmenu::tests::tool_probe_parsing_and_hints；集成：ctxmenu_ssh 在 sshd 上探测；G: screenshot-final-ctxmenu-missing-tool.png（远端无 7z：置灰 + `sudo apt install -y p7zip-full`）、screenshot-final-ctxmenu-missing-tool-terminal.png（远端无 less） |
| TOML 解析/校验（坏项跳过并报错、未知字段/类型/占位符、语法错误）、热加载（mtime） | 通过 | T: ctxmenu::tests::toml_parsing_and_validation / hot_reload_by_mtime / custom_items_match_and_expand；G: screenshot-final-ctxmenu-settings.png（设置分区编辑、保存后显示“第 4 项：未知占位符 {file}”） |
| 各类型菜单项与精确命令预览 | 通过 | T: ctxmenu::tests::menus_by_type_with_previews_and_missing_tools；G: screenshot-final-ctxmenu-filepanel-zip.png（`release 报告.zip`：解压到此处/同名目录/列出 + 通用项） |
| 点击只插入不回车；插入前 Ctrl+U 清空当前行 | 通过 | T: ctxmenu::tests::insert_bytes_never_submit；集成 ctxmenu_ssh：插入后 2 秒远端目标目录不存在、原先输入的行未执行；G: screenshot-final-ctxmenu-inserted.png |
| 回车后真实执行（远端 zip 解压） | 通过 | 集成 ctxmenu_ssh：回车后 `my 报告/inner.txt` 内容正确；G: 文件面板 → 插入 → 回车 → 权限确认 → 远端 `release 报告/readme.txt` 已解出 |
| 回车经权限引擎 + 审计 | 通过 | G: screenshot-final-ctxmenu-enter-permission.png（`rm -- ~/ctxdemo/deploy.sh` → “R3 · 无匹配规则；Ask 模式下 R3 需要询问”），审计 ctxmenu.pick / insert / decision / approval / exec |
| 文件面板插入到该主机会话（无则新开） | 通过 | G: screenshot-final-ctxmenu-filepanel-inserted.png（远端栏自定义项 sha256 插入到同主机 SSH 面板）；代码 Root::pane_for |
| 多选（Ctrl 切换 / Shift 范围）只显示通用项，两项文件显示 diff | 通过 | T: menus_by_type…（multi-select 段） |
| 选中文本：复制 / 终端搜索 / 发给 AI 解释 / 粘贴到输入行 | 通过 | G: screenshot-final-ctxmenu-selection.png；AI 解释走对话面板 send_text（首次发送提示与脱敏照常） |
| 自定义菜单项（匹配扩展名/glob/类型/主机，缺工具置灰） | 通过 | G: screenshot-final-ctxmenu-custom-item.png（config.json：jq 缺失置灰 + `sudo apt install -y jq`、sha256、备份） |
| i18n（内置项 zh/en） | 通过 | T: i18n::tests::locales_have_identical_keys / every_literal_key_in_source_exists |

本轮限制（不计失败）：
1. tmux / 全屏程序（alt screen）内无法跟踪对插入命令的手工修改，权限判定使用插入时的原命令；普通 shell 中使用编辑后的实际行。
2. 远端未发 OSC 7 且提示符不是 `user@host:dir$` 形式时，相对路径保持原样（命令插入同一 shell 仍按当前目录生效），菜单头标注“未知 cwd”。
3. 终端输出中检测到的路径无法知道是否为目录（以 `/` 结尾才按目录处理）及权限位；“运行”只对脚本扩展名给出。
4. “在内置编辑器打开”只对远端文件（经 SFTP 编辑器）；本地文件给出 `"${EDITOR:-vi}" 文件` 插入命令。
5. 测试 sshd 的远端 shell 未设置 UTF-8 locale，readline 把中文显示为 `____`（字节正确，执行结果正确，见 GUI 实测）。
6. 本机 llvmpipe 软件渲染，菜单弹出到屏幕更新约 3–6 秒（截图时等待）。

## 5c. 第 14 轮：PRD 5.21 SFTP 显示模式

| 项 | 结果 | 证据 |
|---|---|---|
| 标签页模式：主标签栏独立标签，标题 `SFTP · host`、文件夹图标、双栏；同主机可多开 | 通过 | G: screenshot-final-sftp-tab.png；命令面板“在标签页中打开 SFTP”每次新建视图 |
| 分屏模式：贴在同主机终端旁 | 通过 | G: screenshot-final-sftp-split.png（宿主机右键菜单 / 命令面板“在分屏中打开”） |
| 分屏跟随终端目录（OSC 7 / 提示符） | 通过 | T: sftp_mode::tests::follow_*；集成 sftp_modes_ssh（远端 shell 发 OSC 7 → 列出该目录；无 OSC 7 时按提示符；`cd ~` 回到 home）；G: 终端 `cd ctxdemo` 后 SFTP 状态栏“已跟随终端 → /home/sshtest/ctxdemo” |
| 弹窗模式保留 | 通过 | G: screenshot-final-sftp-popup.png |
| 分离为独立窗口 / 挂回 | 通过 | G: screenshot-final-sftp-window.png（窗口标题 `SFTP · sshtest@127.0.0.1:2222`）；点“标签页”挂回，窗口关闭；关窗也挂回为标签 |
| 模式切换保留状态（目录、选中、传输队列） | 通过 | G: 同一视图依次 tab → split → window → tab → popup，远端目录 `/home/sshtest/ctxdemo`、选中 `release 报告.zip`、队列中已完成的下载任务一直保留（见四张截图）；实现上移动的是同一个实体，审计 sftp.mode from/to |
| 设置：默认打开方式、跟随开关 | 通过 | 设置 → 会话 `session.sftp_mode`、`session.sftp_follow_cwd`；T: i18n 键一致性 |
| 入口：命令面板 / 侧边栏 / 主机右键菜单 | 通过 | G: 命令面板 sftp_tab / sftp_split；侧边栏主机右键 → 新建终端标签页 / 在标签页 / 分屏 / 新窗口 / 弹窗中打开 SFTP |
| 布局快照与会话恢复 | 通过 | T: session_store::tests::sftp_tabs_roundtrip（含旧格式默认值）；集成 sftp_modes_ssh（保存的两个 SFTP 叶子在同一主机目录重新打开、follow 标记保留）；G: RESTORE=1 重启后 SFTP 标签回到 ctxdemo、分屏 SFTP 跟随开关为开 |
| 5.20 右键菜单在各模式可用 | 通过 | G: 分屏和独立窗口里右键 zip → “列出内容”插入到同主机终端（独立窗口时激活主窗口） |

本轮限制（不计失败）：
1. “模式切换保留状态”只有 GUI 实测，没有自动化测试（GPUI 实体放置逻辑在 bin 内，未接入 gpui 测试上下文）。
2. 远端 shell 默认不发 OSC 7，跟随依赖提示符含路径（`user@host:dir$` 形式）；tmux 内取当前屏幕的光标行。
3. 弹窗和独立窗口不写入布局；退出前未挂回的独立窗口在下次启动时不会恢复。
4. 分屏时窄终端（<90 列且 >30 行）改为上下分；三栏以上布局中 SFTP 栏较窄，权限列在分屏模式隐藏。
5. 独立窗口使用系统默认标题栏；拖拽标签到窗口外分离未实现（用模式按钮）。

## 6. 仍存问题（不计失败）
1. Kitty 图形只做传输即显示（a=T/t/p 直接数据），未做放置 ID 管理、删除、文件/共享内存传输、动画；图片按像素原尺寸绘制，不随字号缩放。
2. 录制只录输出流；脱敏开启时按整行写出（未换行的提示符会与下一行一起出现在回放里）；“[input hidden]”依赖调用方传入关回显状态，目前 GUI 只录输出，密码本身不会回显所以不会被录到。
3. 关键字通知的桌面通知需要会话 D-Bus 通知服务，本机没有，只验证了应用内提示与审计。
4. 快速启动“浏览…”依赖 xdg-desktop-portal，本机未实测；目录候选可直接点选。
5. 目录同步：冲突（同名一侧文件一侧目录、双向且 mtime 相同但大小不同）只列出不处理；单侧扫描上限 2 万项；符号链接跳过。
6. 回放录制中如含关键字，会再次触发关键字通知。
