# TEST_REPORT — AgentShell 原型 对照 PRD v1.0（2026-10-13）

结论标记：**通过** / **失败**（PRD 要求未实现或不达标）/ **未能验证**（本机条件不允许，附原因）。
证据：`T:` 自动化测试名；`G:` 在 DISPLAY=:2 上的 GUI 实测（截图）；`P:` PROGRESS.md 中前几轮已记录的 GUI 实测（本轮未逐项重做）；`M:` 本轮手工测量。

## 0. 构建与自动化测试（本轮，HEAD 见 git log）

| 项 | 命令 | 结果 |
|---|---|---|
| release 构建 | `cargo build --release`（rustc 1.99.0，`~/.cargo/bin` 工具链；系统 /usr/bin 的 1.85 不满足依赖 MSRV） | **通过**，0 警告 |
| clippy | `cargo clippy --release --all-targets` | **通过**，0 警告 0 错误（本轮修掉 21 条） |
| 全部测试 | `source testenv/test.env; eval $(ssh-agent -s); ssh-add testenv/client_ed25519; cargo test --release` | **通过**：94 个（lib 64、agentshell 4、GUI bin 11、daemon 1、headless 6、ssh_features 8），0 失败 0 忽略 |
| 跳过检查 | `--nocapture` 下 grep `skip` | 0 个跳过。所需变量：TEST_SSH_PORT=2222、TEST_SSH_PORT2=2223（密码+转发）、TEST_SSH_PORT3=2224（keyboard-interactive/PAM）、TEST_SSH_USER=sshtest、TEST_SSH_KEY、TEST_SSH_PASSWORD、TEST_SSH_PP_KEY、TEST_SSH_PP、SSH_AUTH_SOCK（见 testenv/test.env） |
| 外部 Agent hook 端到端 | `tools/e2e_hooks.sh`（真实 Claude Code / Codex / Grok CLI + mock LLM） | **通过** 7/7 |
| “即将支持”残留 | `rg "即将支持|[Cc]oming soon|\bSoon\b|_soon|soon\(" src locales` | **通过**，0 处 |

## 1. 用户故事（PRD 2.2）

| ID | 验收标准 | 结果 | 证据 / 原因 |
|---|---|---|---|
| US-01 | 导入 ~/.ssh/config（Host/HostName/User/Port/IdentityFile/ProxyJump），可编辑，不存明文秘密 | 通过 | T: hosts::tests::parses_ssh_config / jump_chain_resolution / glob_works，config::tests::api_key_never_written_to_config；G: 侧栏“~/.ssh/config”分组（screenshot-final-*）；P: 新建/编辑主机对话框。分组“任意嵌套 / 拖拽 / OS 徽章”未实现（见第 6 节） |
| US-02 | 未知 key 显示 SHA256 指纹并确认；变更 key 默认拒绝 | 通过 | T: ssh_localhost_ls_and_known_hosts_tofu；G: screenshot-final-hostkey.png（2223 首次连接，拒绝/仅本次/接受并保存）；变更 key 拒绝走 Handler 校验（P: screenshot-v4-hostkey.png） |
| US-03 | 多标签/分屏，vim/htop/tmux 正常 | 通过 | P: screenshot-v4-htop-vim.png、v4-split-scroll；7.3 拖拽分屏比例；G: 本轮所有截图为本地+SSH 左右分屏。“拖标签到边缘分屏”未实现（用按钮/快捷键分屏） |
| US-04 | 识别 claude/codex/grok 并显示状态，blocked 时通知 | 通过 | T: agent::tests::*、local_agent_identified_by_foreground_process；P: screenshot-v4-agents.png、7.1 通知；G: screenshot-final-agent-page.png（总览页，本轮无运行中的 Agent） |
| US-05 | 持久会话：App 退出/断网后远端进程继续，重开自动 reattach | 通过 | P: 5.2 tmux 持久（screenshot-final-restore-reattach.png，reattach 后 export 的变量仍在）；5.3 本地 daemon（T: daemon_keeps_shell_alive_across_detach） |
| US-06 | 自然语言→命令：命令+解释+风险，默认只插入 | 通过 | T: ai::tests::extracts_command；P: screenshot-v6-nl-card.png / nl-inserted.png |
| US-07 | 失败命令“解释错误”，输出脱敏 | 通过 | T: ai::tests::redacts_common_secrets、suggest::tests::osc133_mark_wins；P: 6.2（OSC 133;D 退出码 → “解释失败”按钮） |
| US-08 | 内置 AI 执行命令逐条过策略引擎、写审计 | 通过（部分） | T: policy::tests::*；P: v6 “执行…”按钮按模式判定 + 审计。PRD 中“AI 面板多轮对话中由 AI 发起 run_command 工具调用并逐条审批”**未实现**（对话面板只能“插入终端”），见第 6 节 |
| US-09 | env=prod 时 R3+ 一律拒绝，always 不可放开 | 通过 | T: policy::tests::production_is_stricter、policy_server::tests::production_host_header |
| US-10 | `agentshell exec <host> "cmd"`，凭据只在 App 内，经策略引擎 | **失败** | `agentshell` 只有 status/check/hook/install/serve/daemon，没有 exec/hosts/upload/download（凭据桥未实现）。外部 Agent 的 hook 判定已可用（e2e 7/7），但“借 App 的连接在保存主机上执行”不存在 |
| US-11 | 审计面板按主机/会话/决策过滤，导出 JSONL | 通过（部分） | T: audit::tests::chain_detects_tampering / prune_keeps_chain_verifiable；P: screenshot-v5-audit.png（列表、校验、导出）。按主机/会话/决策的**过滤器未实现**（只有文本列表） |
| US-12 | SFTP 双栏拖拽、断点续传、共享认证 | 通过 | T: sftp_ops_and_resumable_transfers；P: screenshot-final-sftp*.png，40MB 拖拽上传 sha256 一致 |
| US-13 | -L/-R/-D，随 App 启动 | 通过 | T: port_forwards_local_remote_socks、forward::tests::*；P: screenshot-final-port-forwarding.png。“规则图示”未做（列表形式） |
| US-14 | Agent 总览按状态排序、点击跳转 | 通过 | P: screenshot-v5-ai-agent.png；G: screenshot-final-agent-page.png |
| US-15 | 组织级托管策略（P2） | 未能验证 | P2，未实现，不在本轮范围 |

## 2. 功能需求（PRD 第 3 章）

### 3.1 SSH
| 功能 | 结果 | 证据 / 原因 |
|---|---|---|
| 主机管理（搜索、分组、生产标记） | 通过 | P: screenshot-v5-sidebar / new-host |
| 认证链 agent → publickey → password → keyboard-interactive | 通过 | T: agent_auth、passphrase_key、password_auth_prompt_save_and_reuse、**keyboard_interactive_auth（本轮新增，sshd3 PAM :2224）**；G: screenshot-final-password-prompt.png、screenshot-final-keyboard-interactive.png（本轮实测登录成功）。先做 none 探测，服务器不提供的方式不再弹窗 |
| Host key（独立 known_hosts） | 通过 | 同 US-02 |
| ProxyJump 多跳 | 通过 | T: proxyjump_and_remote_forward、hosts::tests::jump_chain_resolution |
| HTTP/SOCKS5 出站代理（P1） | 失败 | 未实现 |
| 连接复用 | 通过 | T: connection_reuse |
| SFTP | 通过 | 见 US-12；内置编辑/同步（P1）未实现 |
| 端口转发 | 通过 | 见 US-13 |
| 钥匙串凭据 | 通过 | T: secrets::tests::*；Secret Service 不可用时回退 0600 加密文件（本机走回退路径；真实 Keychain/Credential Manager 未能验证：无 macOS/Windows） |
| Agent 转发（默认关、按主机开） | 通过 | P: 2.4（GUI 实测）；T: agent_auth 覆盖 agent 认证 |
| 凭据页 / 生成导入密钥 | 通过 | T: keygen::tests::generate_import_and_load；G: screenshot-final-credentials.png |
| 堡垒机 ssh:// 拉起、主机侧栏工具（P2） | 未能验证 | P2，未实现 |

### 3.2 终端
| 功能 | 结果 | 证据 / 原因 |
|---|---|---|
| 仿真（256 色/真彩、鼠标、bracketed paste、备用屏、OSC 8） | 通过 | T: termview::tests::mouse_encoding / bracketed_paste_strips_esc；P: v4-htop-vim |
| OSC 52 剪贴板 | 失败 | 未实现 |
| 本地 Shell（bash/zsh） | 通过 | T: local_pty_ls、local_zsh_integration_and_history |
| 多标签/分屏/拖拽比例 | 通过 | T: layout::tests::*；P: 7.3 |
| 搜索（正则）/ 矩形选择 / URL 识别 | 失败 | 终端内搜索与矩形选择未实现；OSC 8 链接可 Ctrl+点击，纯文本 URL 识别未做 |
| 命令块 OSC 133 A/B/C/D | 通过 | T: local_bash_osc133_prompt_mark、suggest::tests::osc133_mark_wins |
| 通知（OSC 9/777/BEL → toast + 系统通知） | 通过 | T: term::tests::osc_scan；P: 7.1（notify-rust，本机无通知守护进程弹窗只验证到调用与审计） |
| 字体/回退/CJK/Nerd Font | 通过 | P: screenshot-v3-fonts |
| 主题（内置、跟随系统、Alacritty/iTerm2 导入） | 通过 | T: theme::tests::*、theme::iterm_tests::imports_itermcolors；P: screenshot-final-iterm-theme.png |
| 历史自动建议 + AI 灰字 | 通过 | T: suggest::tests::*（9 个）；P: screenshot-final-ai-inline-suggest.png |
| IME 预编辑 | 未能验证 | 代码已实现 EntityInputHandler；本机无 IME 守护进程（fcitx/ibus），无法输入预编辑 |
| 光标闪烁 CPU | 通过 | M: llvmpipe 下闪烁时约 23% CPU（每帧整窗软件重绘约 110ms）→ 检测软件渲染器时光标常亮，输入后 8 秒 CPU 0.4%；硬件 GPU 维持“聚焦 + 10 秒内有输入才闪、只重绘光标层”（本机无硬件 GPU，未能复测） |
| 关键字高亮 / 图形协议 / 录制 / 广播（P1/P2） | 失败（P1）/ 未能验证（P2） | 未实现 |

### 3.3 外部 Agent
| 功能 | 结果 | 证据 / 原因 |
|---|---|---|
| 无缝运行、识别（进程名 + 屏幕 manifest） | 通过 | T: agent::tests::*、local_agent_identified_by_foreground_process |
| 状态与增强 UI、通知 | 通过 | P: v4-agents、7.1 |
| 会话恢复（resume 命令，点击才运行） | 通过 | P: 5.4（grok `--resume` 参数未在本机核实） |
| 外部 Agent 权限钩子（Claude/Codex/Grok） | 通过 | T: hooks::tests::*、policy_server::tests::*；e2e 7/7；G: screenshot-final-approval-card.png（本轮：hook 请求弹审批卡，点“拒绝”后 hook 输出 `permissionDecision: deny`）；G: screenshot-final-agents-settings.png |
| 远端 Agent hook 回传 | 通过 | P: 1.6（反向转发，GUI 实测 allow/deny） |
| 凭据桥 `agentshell exec/hosts/upload/download` | 失败 | 未实现（同 US-10） |
| 快捷启动 Agent 菜单、Agent 输入注入 write_to_pty（P1） | 失败 | 未实现 |

### 3.4 内置 AI
| 功能 | 结果 | 证据 / 原因 |
|---|---|---|
| inline 建议、自然语言→命令、错误解释、上下文脱敏 | 通过 | 见 US-06/07；T: ai::tests::*；P: mock 日志确认 hunter2 未发出 |
| 对话面板（流式、停止、插入终端） | 通过 | T: ai::stream_tests::*、chat_view::tests::code_block；P: screenshot-final-ai-chat.png |
| Agentic 执行（AI 发起 run_command 逐条审批卡） | 失败 | 未实现（见 US-08） |
| 模型 BYOK：OpenAI 兼容 / Anthropic / Ollama / OpenRouter 等 | 通过（部分） | T: models::tests::*、ai::tests::anthropic_headers；聊天只用本地 mock 验证 OpenAI 兼容，Anthropic 只有单元测试；Gemini 原生接口未实现（可走 OpenAI 兼容端点） |
| 隐私：首次发送远端输出前提示、按主机禁止 AI 读取输出 | 失败（部分） | 有全局 `ai.send_output`（默认 false）与脱敏；首次提示与按主机开关未实现 |

### 3.5 权限
| 功能 | 结果 | 证据 |
|---|---|---|
| Ask/Plan/Auto、状态栏切换 | 通过 | T: policy::tests::decisions；P: v5-permissions |
| allow/ask/deny 规则、last-match-wins | 通过 | T: policy::tests::last_match_wins_and_glob |
| R0–R4 + hard-deny、tree-sitter 分段 | 通过 | T: policy::tests::risk_levels / segmented_risk |
| 主机 env 标签 | 通过 | T: policy::tests::production_is_stricter |
| 审批卡、超时自动拒绝 | 通过 | T: policy_server::tests::approvals_and_timeout；G: screenshot-final-approval-card.png。审批卡只有“允许一次 / 拒绝”，PRD 的“本会话总是允许此前缀 / 拒绝并告诉 AI 原因”未实现 |
| 审计 JSONL + 哈希链 + 脱敏 + 保留期 | 通过 | T: audit::tests::*；7.7 |
| 托管策略（P2） | 未能验证 | P2 未实现 |

### 3.6 会话与布局
| 功能 | 结果 | 证据 |
|---|---|---|
| 布局持久化 / 启动恢复（不重跑命令） | 通过 | T: session_store::tests::roundtrip；P: screenshot-final-restore-layout.png |
| 远端持久（tmux） | 通过 | P: screenshot-final-restore-reattach.png |
| 本地持久（daemon） | 通过 | T: daemon_keeps_shell_alive_across_detach |
| 断线重连 / keepalive | 通过 | P: 2.5（GUI 实测重启 sshd 后重连） |
| 托盘图标、布局快照（P2） | 失败（托盘）/ 未能验证（P2） | 未实现 |

### 3.7 设置全表
| 项 | 结果 | 证据 |
|---|---|---|
| 所有设置都有实际效果（无“即将支持”） | 通过 | rg 0 残留；本轮补上 `appearance.opacity`、`appearance.ui_font_size` |
| 配置写回保留注释 | 通过 | T: config::tests::save_preserves_comments |
| 窗口透明度视觉效果 | 未能验证 | 代码：opacity<1 时窗口切到 Transparent 背景，终端区与根背景带 alpha；X 窗口深度 32、xfwm 合成已开，但截图像素不变（llvmpipe Vulkan surface 推测只提供 Opaque alpha 模式，wgpu 回退为不透明）。需在真实 GPU 上复测 |
| 界面字号 | 通过（部分） | G: 改为 15 后 gpui-component 控件（输入框、开关文字）与无显式字号的文本变大；大量自绘文本仍是固定 px，不随之缩放 |
| 更新检查 | 通过 | T: update::tests::version_compare；P: screenshot-final-update-check.png。只提示不自动安装，minisign 校验未做 |

## 3. 非功能需求（PRD 第 4 章，本机软件渲染）

| 指标 | 目标 | 实测 | 结果 |
|---|---|---|---|
| 冷启动 | < 400ms | 进程启动到 X 窗口创建 62ms（首帧时间未单独计量） | 通过（部分口径） |
| 空闲内存 | 单标签 < 80MB | RSS 169MB（含 llvmpipe/Mesa 软件渲染约百 MB 级） | 未能验证（软件渲染器下不可比，需真机） |
| 空闲 CPU | 低 | 聚焦空闲 0.4% | 通过 |
| 安装包 | < 60MB | release 二进制 82MB，strip 后 61MB，gzip 21MB | 失败（未 strip 时）；打包时需 strip + 压缩 |
| 跨平台 | macOS / Windows / Linux | 仅 Linux X11 实测 | 未能验证 |

## 4. 截图（本轮新增并逐张查看）
- screenshot-final-approval-card.png：Agent hook 审批卡（claude · local · Bash · R4，倒计时，拒绝/允许一次）
- screenshot-final-hostkey.png：首次连接主机密钥确认
- screenshot-final-password-prompt.png：SSH 密码对话框（掩码、保存到钥匙串开关）
- screenshot-final-keyboard-interactive.png：键盘交互（PAM “Password:” 提示，无保存开关）
- screenshot-final-credentials.png：设置-凭据与密钥
- screenshot-final-agents-settings.png：设置-外部 Agent（hook 安装状态）
- screenshot-final-agent-page.png：AI / Agent 总览页

此前已有：screenshot-final-ai-chat / ai-inline-suggest / iterm-theme / port-forwarding / restore-layout / restore-reattach / sftp / sftp-delete-confirm / update-check。

## 5. 统计
按第 1–3 节逐行计（共 74 行）：**通过 55**（其中“部分通过” 6 行），**失败 12**，**未能验证 7**。第 0 节构建/测试 6 项全部通过。

## 6. 失败项汇总（PRD 要求但未实现）
1. 凭据桥 `agentshell exec/hosts/upload/download`（US-10，P0）
2. 内置 AI Agentic 执行：对话中 AI 发起 run_command 并逐条审批（US-08 / 3.4，P0）
3. 审批卡“本会话总是允许此前缀”“拒绝并告诉 AI 原因”（3.4 交互原则 2）
4. 审计面板按主机/会话/决策过滤（US-11）
5. 终端搜索（正则）、矩形选择、纯文本 URL 识别（3.2，P0）
6. OSC 52（3.2，P0）
7. AI 隐私：首次发送提示、按主机禁止读取输出（3.4，P0）
8. HTTP/SOCKS5 出站代理（P1）、快捷启动 Agent / write_to_pty 权限（P1）、关键字高亮（P1）、托盘（P1）、SFTP 内置编辑（P1）
9. 主机分组任意嵌套 / 拖拽 / OS 徽章；标签拖到边缘分屏
10. 安装包体积（需 strip）

这些属于 PRD 范围但不在 PROGRESS.md 的补齐计划（1–7）里，工作量大，本轮未实现，已写入 PRD v1.0 第 9.2 节与 README“已知问题”。
