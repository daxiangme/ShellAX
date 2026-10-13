# TEST_REPORT — AgentShell 原型对照 PRD v1.0（第 12 轮收尾，2026-10-13）

标记：**通过** / **失败**（PRD 要求未实现或不达标）/ **未验证**（只因硬件或平台限制无法在本机验证，写明原因）。
证据：`T:` 自动化测试；`G:` 本轮在 DISPLAY=:2 实测（截图本轮逐张看过）；`P:` 前几轮 GUI 实测（PROGRESS.md 有记录，截图在仓库）；`M:` 手工测量。
代码基线：第 12 轮（2a7fbe7 之后，本报告提交前的最后一个代码 commit）。G: 截图都在 DISPLAY=:2（1920×1200）全屏截取，本轮逐张看过。

## 0. 构建与自动化测试

| 项 | 命令 | 结果 |
|---|---|---|
| release 构建 | `cargo build --release`（`~/.cargo/bin` 工具链） | **通过**，0 警告 |
| dist 构建 | `cargo build --profile dist`（fat LTO、cgu=1、panic=abort、strip） | **通过**，0 警告；二进制 46.0MB，gzip 18.2MB |
| clippy | `cargo clippy --release --all-targets` | **通过**，0 警告 0 错误 |
| 全量测试 ×3 | `source testenv/test.env; eval $(ssh-agent -s); ssh-add testenv/client_ed25519; cargo test --release -- --nocapture` | **通过**：3 轮均 135/135（lib 92、agentshell 8、GUI bin 16、daemon 1、headless 8、managed_policy 1、ssh_features 9），0 失败 0 忽略 |
| 跳过检查 | 在 --nocapture 输出里 grep skip | 0 个跳过（sshd 2222 公钥 / 2223 密码+转发 / 2224 keyboard-interactive 都在运行，SSH_AUTH_SOCK 已设） |
| 外部 Agent hook 端到端 | `tools/e2e_hooks.sh`（真实 Claude Code / Codex / Grok CLI + mock LLM） | **通过** 7/7 |
| 许可证 | 新增代码全部自写（Sixel/Kitty 解码、asciicast、目录同步、广播、快照均按公开规范自写）；本轮新增直接依赖 image 0.25（MIT OR Apache-2.0，原本已是 gpui 的传递依赖）已登记 THIRD_PARTY_NOTICES.md；未打开 research/ 下 shell-rs、oxideterm、warp，未读 Zed terminal/terminal_view/editor | 通过 |

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

## 6. 仍存问题（不计失败）
1. Kitty 图形只做传输即显示（a=T/t/p 直接数据），未做放置 ID 管理、删除、文件/共享内存传输、动画；图片按像素原尺寸绘制，不随字号缩放。
2. 录制只录输出流；脱敏开启时按整行写出（未换行的提示符会与下一行一起出现在回放里）；“[input hidden]”依赖调用方传入关回显状态，目前 GUI 只录输出，密码本身不会回显所以不会被录到。
3. 关键字通知的桌面通知需要会话 D-Bus 通知服务，本机没有，只验证了应用内提示与审计。
4. 快速启动“浏览…”依赖 xdg-desktop-portal，本机未实测；目录候选可直接点选。
5. 目录同步：冲突（同名一侧文件一侧目录、双向且 mtime 相同但大小不同）只列出不处理；单侧扫描上限 2 万项；符号链接跳过。
6. 回放录制中如含关键字，会再次触发关键字通知。
