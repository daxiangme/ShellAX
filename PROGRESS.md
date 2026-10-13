# PROGRESS — 按 PRD v0.8 补齐剩余功能

状态：☐ 未开始 / ◐ 进行中 / ☑ 完成（附 commit）/ ✗ 本环境无法完成（附原因）
规则：每完成一项更新本文件并 git commit；中断后从第一个 ☐/◐ 继续。

## 1 权限引擎真拦截（PRD 第 6 章，P0）
- ☑ 1.1 风险分级改为 tree-sitter-bash 分段：管道、&&/||/;、子 shell、$( )、find -exec、xargs、重定向、sudo/env/nohup 前缀，取最高风险
- ☑ 1.2 生产主机加严：`policy::decide_ctx(.., prod)`；Auto 只放行 R0，R2+ 的允许规则改为询问；AI 执行已接入
- ☑ 1.3 本地策略服务 `src/policy_server.rs`（127.0.0.1 HTTP + Bearer 令牌，端点文件 0600）+ CLI `src/bin/agentshell.rs`（status/check/hook/install/serve）
- ☑ 1.4 Hook 安装（`src/hooks.rs`，写前备份、可卸载、幂等）；真实 CLI 验证：Claude Code 2.1.296 / Codex 0.162.1 / Grok 1.0.52 + tools/mock_agent_llm.py，`tools/e2e_hooks.sh` 7/7 通过
- ☑ 1.5 界面审批卡（screenshot-v8-approval-card-en.png，点"允许一次"后 hook 返回 allow）、超时（agents.on_timeout）、CLI 默认 fail-closed、审计 hook.request/decision/approval
- ☑ 1.6 远端 Agent hook 回传：SSH 反向端口转发到本地策略服务（reverse forward + 远端端点文件 + AGENTSHELL_HOST，GUI 实测 allow/deny 并审计）

## 2 SSH（P0/P1）
- ☑ 2.1 密码认证（钥匙串存储）、ssh-agent 认证、密钥口令（tests/ssh_features.rs）
- ☑ 2.2 生成/导入密钥（src/keygen.rs，设置页凭据按钮）
- ☑ 2.3 ProxyJump 跳板机（多级链，测试通过）
- ☑ 2.4 算法偏好（kex/cipher 设置行）、agent 转发
- ☑ 2.5 keepalive、断线重连、连接复用

## 3 SFTP（P1）
- ☑ 3.1 双栏浏览、新建/重命名/删除/权限（删除需确认）：src/sftp.rs + src/sftp_view.rs，复用连接池；GUI 实测 chmod 600、确认后删除（screenshot-final-sftp*.png）
- ☑ 3.2 上传下载、队列与进度、暂停/继续、断点续传（按目标已有大小续传）、两栏拖放 + 系统文件拖入远端栏；tests/ssh_features.rs::sftp_ops_and_resumable_transfers；GUI 拖拽上传 40MB sha256 一致

## 4 端口转发（P1）
- ☑ 4.1 本地 / 远程 / 动态 SOCKS5；列表启停、状态与计数、按主机保存 forwards.toml、主机连接时自动启动（src/forward.rs + src/port_view.rs；测试 port_forwards_local_remote_socks；GUI 实测 L 走 ssh、D 走 curl --socks5、R 远端 curl、重启后自动启动、停止后端口关闭）

## 5 会话持久化（P1）
- ☑ 5.1 布局保存与启动恢复（src/session_store.rs，layout.json 每 5 秒与关闭时写入；含分屏比例、cwd、主机、tmux 名、daemon id、resume）；恢复时 Shell 在原 cwd 重新启动、不重跑命令；退出确认对话框（screenshot-final-restore-*.png、退出确认实测）
- ☑ 5.2 远端 tmux 持久会话：session.persistent=tmux 时 SSH 面板执行 `tmux new-session -A -s agentshell-xxxx`，退出后远端继续运行，重启 App 自动 reattach（实测 export 的变量仍在）；无 tmux 时回退普通 shell
- ☑ 5.3 本地后台保活：`agentshell daemon`（src/daemon.rs，Unix socket 0600，512KB 输出回放，同机 /proc 探测前台进程与 cwd）；session.local_daemon 开启后本地面板由 daemon 持有，关窗口后 Agent 继续运行，重开自动接回（tests/daemon.rs + GUI 实测）；关闭面板会结束 daemon 中对应 shell
- ☑ 5.4 Agent resume：快照记录面板最近识别到的 Agent → `claude --continue` / `codex resume --last` / `grok --resume`（grok 参数未在本机核实）；进程丢失后恢复布局时显示“恢复 Agent”按钮，点击才运行；daemon 仍持有时不提示

## 6 内置 AI（P1）
- ☑ 6.1 对话面板（src/chat_view.rs；ai::chat_stream 支持 OpenAI SSE / Anthropic 流；多轮；附带聚焦终端上下文（按 ai.context 范围、发送前脱敏，mock 日志确认 hunter2 未发出）；停止；代码块“插入终端”只插入不执行；审计 ai.chat*）
- ☑ 6.2 命令失败一键解释：shell 集成新增 OSC 133;D;$?，非零退出时面板标题出现“解释失败（退出码 N）”，命令与 200 行尾部输出脱敏后流式解释
- ☑ 6.3 AI 命令建议灰字：suggest.ai 开启且历史无匹配时停顿 600ms 请求命令模型（fast_model），复用灰字 UI 与 →/Tab 接受；含密钥/敏感命令的行不发送

## 7 其余（P2 / 3.7 设置全表）
- ☑ 7.1 桌面通知：Agent 状态变化（等待输入/完成，agents.notify_state）、长命令结束（notify.long_command_secs，基于 OSC 133;D）、notify.sound 与 bell=sound（canberra/paplay/afplay，尽力而为）；可见且聚焦的窗格不打扰；写审计 notify
- ☑ 7.2 IME 预编辑：TermView 实现 EntityInputHandler，预编辑文本在光标处带下划线绘制、不发送，提交后写入 PTY；候选框定位到光标单元格（本机无 IME 守护进程，预编辑路径未实测）
- ☑ 7.3 分屏拖拽比例（分隔条拖动，Node::Split.ratio 0.1–0.9，随布局保存）
- ☑ 7.4 标签/窗格标题随语言即时切换（shown_title 按当前语言重渲染“本地 · bash”）
- ☑ 7.5 iTerm2 主题导入：.itermcolors plist 解析（自写，无第三方代码）、设置-外观“导入主题”复制到 ~/.config/agentshell/themes 并即时出现在主题列表；Alacritty/自有 TOML 同入口
- ☑ 7.6 更新检查：src/update.rs 拉取通道清单 JSON（stable/beta，general.update_url 或 AGENTSHELL_UPDATE_URL 覆盖）、版本比较；启动时后台检查并提示，设置-通用“立即检查”。只提示不自动安装（下载+minisign 校验留给发布流水线）；默认清单域名无真实服务器
- ☑ 7.7 审计保留期：AuditLog::prune 按 retention_days 删除旧记录，首条保留记录的 prev 写入 audit.jsonl.anchor，verify 从锚点校验，剪除本身记一条 audit.prune；audit.log_commands 打开后记录提交的命令（脱敏）
- ☑ 7.8 配置写回保留注释：Settings::save 用 toml_edit 合并到已有文件（保留注释、空行、键顺序、行尾注释），解析失败回退整写；测试 config::tests::save_preserves_comments
- ☑ 7.9 光标闪烁 CPU：实测本机 llvmpipe 每次闪烁是整窗软件重绘（约 110ms/帧，闪烁期间约 23% CPU）；检测 window.gpu_specs().is_software_emulated，软件渲染时光标常亮（AGENTSHELL_FORCE_BLINK 可强制闪烁），实测输入后 8 秒 CPU 0.4%；硬件 GPU 保持原逻辑（聚焦+10 秒内活动才闪，只重绘光标层）
- ☑ 7.10 "即将支持"占位清零：透明度（窗口透明背景 + 终端区域 alpha）、界面字号（gpui-component 主题字号 + 根元素默认字号）实现；删除 Row.soon/ui::soon 与 5 个废弃键，修正 settings.agent_sock 文案；`rg "即将支持|coming soon|Soon" src locales` 为 0

## 8 整体测试
- ☑ release 0 警告、clippy 0 警告、cargo test --release 94/94 无跳过（testenv/test.env，新增 sshd3 :2224 keyboard-interactive）、e2e_hooks 7/7；TEST_REPORT.md（通过 55 / 失败 12 / 未能验证 7）；新增截图 approval-card / hostkey / password-prompt / keyboard-interactive / credentials / agents-settings / agent-page；PRD v1.0（v0.8 备份 PRD-v0.8.md）、README
- ☑ 8.1 本轮额外修复：SSH keyboard-interactive 认证（PRD 3.1 P0）、agentshell CLI EPIPE、透明度绘制到网格背景

# 第 10 轮（按 PRD v1.0 补齐）
- ☑ 10.1 审计按主机/会话/决策过滤：audit::field/Record::{host,session,decision}/Filter/facets；hook.request/decision/approval、ai.exec* 记录写入 host= session=；审计面板“决策/主机/会话”筛选条（全部 N 条中显示 M 条）；测试 audit::tests::filter_by_host_session_decision；截图 screenshot-v10-audit-filter-host.png / -deny.png / screenshot-v10-approval-card-buttons.png
- ☑ 10.2 agentshell hosts/exec/upload/download（US-10）：src/broker.rs（Ops trait + SshOps，复用 ssh::connect_shared 连接池与 App 的主机/凭据，CLI 只传主机名）；策略服务 /v1/hosts|exec|upload|download 走 decide()+审批卡，审计 agentshell.*（含 host/session）；远端回传调用者禁止 upload/download（403）；exec 退出码透传、拒绝 126；测试 policy_server::tests::remote_ops_go_through_policy；GUI 实测 hosts、exec uname、上传/下载 300KB sha256 一致、R2 无应答超时拒绝；截图 screenshot-v10-agentshell-upload-approval.png
- ☑ 10.3 内置 AI 对话中发起执行并逐条审批（US-08）：chat_view::Plan（code_commands 提取全部代码块命令→逐条经 TermView::ai_policy 判定；Allow 自动执行、Ask 停下显示“允许此步/跳过/停止”、Deny 显示原因并继续下一条）；在来源终端执行并审计 ai.plan / ai.exec.decision(plan=1) / ai.exec / ai.exec.rejected；测试 chat_view::tests::plan_steps_and_approvals；GUI 实测 df -h 自动、systemctl restart 询问→跳过、uptime 自动；截图 screenshot-v10-ai-chat-step-approval.png / -plan-done.png
- ☑ 10.4 AI 首次发送前提示（ai.first_send_ack；对话面板横幅/终端 AI 卡 Consent：服务商与地址、上下文范围、是否含输出、脱敏状态；确认后持久化并审计 ai.consent，设置可重置）、按主机禁止读取输出（ai.no_output_hosts 通配；TermView::ai_output_allowed 用于对话/命令生成上下文，解释失败也不发输出）；测试 config::ai_output_tests::no_output_hosts；GUI 实测本地输出发送、127.0.0.1 主机输出 SECRET_SSH_OUT 未出现在 mock 日志；截图 screenshot-v10-ai-first-send-notice.png / -no-output-host.png
- ☑ 10.5 P1
  - ☑ 10.5a HTTP/SOCKS5 出站代理：src/proxy.rs（按 RFC 1928/1929 与 HTTP CONNECT 自写，支持用户名密码、远端 DNS），ssh.proxy / ssh.no_proxy 设置行，SSH 首跳经代理（跳板链随之）；测试 proxy::tests（SOCKS5 经 forward.rs 服务端、HTTP CONNECT+407）；GUI 实测 tools/mock_proxy.py 日志 `CONNECT 127.0.0.1:2222` 且 SSH 面板已连接；截图 screenshot-v10-proxy-settings.png
  - ☑ 10.5b（第 11 轮完成，见 11.B*） 托盘、Agent 快速启动、write_to_pty 权限、关键词高亮、SFTP 内置编辑
- ☑ 10.6（第 11 轮完成，见 11.C*） P2：组织策略文件、ssh:// sftp:// 链接、主机侧工具
- ☑ 10.7 收尾（第 11 轮完成，见 11.D）

# 第 11 轮（安全修复 + 剩余 P1/P2 + 收尾）
- ☑ 11.A1 调用方断开自动撤销审批卡：policy_server 等待审批时每 200ms 探测 TCP 对端（非阻塞 peek=EOF），断开即丢弃应答通道（界面 1 秒内移除卡片）、判拒绝并审计 `hook.approval id=N withdrawn caller-disconnected`；/v1/exec 等远端操作在批准后再次检查调用方，已断开则不执行（审计 effect=Cancelled）；测试 policy_server::tests::approval_withdrawn_when_caller_disconnects（exec 与 hook 两路，迟到的“允许”不执行）
- ☑ 11.A2 代理覆盖 AI 请求与更新检查：proxy::ureq_builder（ssh.proxy 的 http:// / socks5://，遵守 ssh.no_proxy 通配）用于 ai::chat / chat_stream / update::check；ureq 开启 socks-proxy（socks 0.3.4，MIT/Apache，已登记）；测试 proxy::tests::http_clients_use_proxy_and_no_proxy；tools/mock_proxy.py 支持绝对 URI 的明文 HTTP
- ☑ 11.A3 AI 逐条执行结果回传：步骤串行执行，OSC 133;D 到达后截取该命令输出（最后 200 行，脱敏；ai.send_output 关闭或主机在 ai.no_output_hosts 时只回传退出码、标注 withheld），计划结束后把每步退出码/输出/拒绝/跳过汇总作为下一轮 user 消息发给 AI（ai.feed_back_results，默认开，设置-AI 可关）；审计 ai.exec.result / ai.exec.feedback；无 shell 集成时 120 秒超时；测试 chat_view::tests::plan_steps_and_approvals；GUI 实测 df -h 自动→systemctl 跳过→uptime 自动→mock 日志收到汇总并回复；截图 screenshot-v11-ai-feedback.png
- ☑ 11.B1 关键词高亮：src/highlight.rs（正则规则、颜色=主题角色或 #rrggbb、粗体、先到先得不重叠、宽字符跳过占位格以匹配中文）；terminal.keyword_highlight（默认开）+ [[terminal.highlights]]，默认 error/failed/denied 红、warn/deprecated 黄、OK/PASS 绿；渲染时按行着色（选区/反显不覆盖），随配置代次重编译；测试 highlight::tests::rules_and_spans；截图 screenshot-v11-keyword-highlight.png
- ☑ 11.B2 write_to_pty 权限：policy::write_to_pty（ask / ask_first / allow / deny，默认 ask；无 Agent 的普通 shell 面板不受此限）；内置 AI 的插入、执行、逐条计划、对话面板“插入终端”向正在运行 Agent 的面板写入前都经此闸门，ask 时面板底部显示“AI 想向 Claude 输入并回车：…[允许][拒绝]”，ask_first 每面板首次批准后放行；审计 write_to_pty ask/approved/rejected/allow/deny；设置-外部 Agent 新增选项；测试 policy::pty_tests::write_to_pty_modes；GUI 实测假 claude 进程面板弹卡→允许后收到 "df -h"；截图 screenshot-v11-write-to-pty-ask.png
- ☑ 11.B3 Agent 快速启动：[[agents.quick_launch]]（默认 Claude Code=claude / Codex=codex / Grok=grok）；命令面板 “在分屏中启动 Agent 1-3”（Ctrl+Alt+1/2/3）与 AI / Agent 面板“快速启动”按钮：按焦点面板的主机（本地或同一 SSH 主机）分屏并在 shell 就绪后输入命令；审计 agent.launch；测试 config::ai_output_tests::quick_launch_and_new_defaults；GUI 实测启动后 Agent 被识别（Claude 标签）；截图 screenshot-v11-agent-quick-launch*.png
- ☑ 11.B4 SFTP 内置编辑：sftp::read_text / write_text（UTF-8、≤2MB、含 NUL 视为二进制拒绝；保存前比对打开时的 size+mtime，被他人修改则拒绝并提示“仍要覆盖”）；SFTP 工具栏“编辑”→ gpui-component EditorState（行号）编辑远端文件，保存写回并刷新，审计 sftp.edit.open/save/conflict；测试 tests/ssh_features.rs::sftp_ops_and_resumable_transfers（往返、冲突、二进制/大小限制）；GUI 实测编辑 aa-edit-me.conf 保存后远端内容一致，远端改动后保存出现冲突提示；截图 screenshot-v11-sftp-editor.png / -conflict.png
- ☑ 11.B5 托盘：src/tray.rs（ksni 0.3.6 StatusNotifierItem，Unlicense，已登记；zbus MIT）；标题/提示显示“N 个 Agent 运行中（M 个等待输入）”，有等待时 Status=NeedsAttention；菜单 显示窗口 / 新建本地终端 / 退出（退出走退出确认）；general.tray（默认开，AGENTSHELL_NO_TRAY 关闭）；无会话总线时只记日志。验证：本机无桌面面板，用 examples/sni_watcher.rs（测试用 Watcher）+ 私有 dbus-daemon（tools/tray_check.sh）：busctl 读到 Title “AgentShell · 1 个 Agent 运行中”，GetLayout 菜单 6 项，Event(4,clicked) 后窗口新建本地终端标签；测试 tray::tests::summary_and_labels。真实面板（GNOME/KDE/XFCE）上图标显示未验证；macOS/Windows 托盘未实现
- ☑ 11.C1 组织托管策略：src/managed.rs 读取 /etc/agentshell/managed-policy.yaml（AGENTSHELL_MANAGED_POLICY 可改路径，serde_yaml MIT/Apache 已登记），App 与 agentshell CLI 启动时加载，只读、永不写回；policy::decide_host 在用户规则之前应用：hard_deny 通配（整条或管道/;/&& 任一段）→ 拒绝、mode.allowed 限制模式（不允许的模式按最严格允许模式评估，切换模式被拒并提示，循环切换跳过）、hosts[].deny_risk_at_or_above（tags env:prod=生产主机 / names 通配）、audit.retention_days 作为保留期下限、pty_write 上限；解析失败按失败关闭（仅 Plan）；状态栏显示“· 组织策略”，审计 policy.managed；测试 managed::tests::parse_and_rules、tests/managed_policy.rs（用户 Auto+全部允许规则仍被拒、经策略服务 /v1/check 拒绝、文件未被修改、坏文件失败关闭）；截图 screenshot-v11-managed-policy.png
- ☑ 11.C2 ssh:// sftp:// 链接启动：src/link.rs（user[;fingerprint=…][:password]@host[:port][/path]、IPv6、百分号解码、Xshell `-url` 兼容；URL 中的密码一律丢弃并提示；主机名含空白/引号/; 拒绝）；命令行带链接时若已有实例运行则经本地策略服务 /v1/open（令牌认证、仅本机调用者，审计 link.open）交给已运行实例并退出，否则自身启动后打开；匹配已配置主机（保留认证/跳板/生产标记）否则临时目标，主机密钥照常校验；sftp:// 直接打开 SFTP 面板到给定目录；packaging/agentshell.desktop 注册 x-scheme-handler/ssh、sftp；测试 link::tests::parses_links；GUI 实测第二实例 `-url sftp://sshtest@127.0.0.1:2222/home/sshtest/bin` 打开到 bin 目录、`ssh://sshtest:secret@127.0.0.1:2222` 新开 SSH 标签（密码被忽略）；截图 screenshot-v11-sftp-link.png / -ssh-link.png
- ☑ 11.C3 主机侧工具：src/hosttools.rs（只读采集命令 + 解析：/proc 负载/内存/df、ps、systemctl list-units、docker ps；写操作 Action 白名单构造并校验参数：kill PID、sudo -n systemctl restart/stop/start 单元、docker restart/stop/logs 容器 ID）；src/host_tools_view.rs 面板（侧栏「主机工具」/命令面板，目标为当前焦点窗格：SSH 复用连接池 exec，本地 sh -c）；写操作经 policy::decide_host（含组织策略）→ Allow 直接执行 / Ask 内联确认条 / Deny 显示原因，审计 hosttools.decision/approval/exec；测试 hosttools::tests::parsers_and_actions；GUI 实测 127.0.0.1:2222 监控/进程/Docker 不可用提示、结束进程触发 R3 询问并拒绝（审计 seq 263/264）；截图 screenshot-v11-hosttools-monitor.png / -kill-ask.png / -docker-missing.png
- ☑ 11.D1 未提交修改（managed.rs 文档空行、proxy.rs 测试拆行、termview.rs 去掉 Copy 类型的 clone）为 clippy 清理，已提交 2fcec7a
- ☑ 11.D2 PRD 3.1 主机管理补齐：src/outline.rs（嵌套分组 a/b/c、折叠、计数、OS 探测解析与徽章）；侧栏树形分组、拖主机到分组标题改组并写回（审计 hosts.move）、连接后只读探测 OS（审计 hosts.os_probe）；测试 outline::tests::*；截图 screenshot-final-host-groups-os.png / host-drag-group.png
- ☑ 11.D3 US-03 拖标签到面板边缘分屏：Node::insert_at，拖动时面板显示左/右/上/下放置区；测试 layout::tests::insert_subtree_at_edges；截图 screenshot-final-tab-drag-zone.png / -split.png
- ☑ 11.D4 收尾：release / dist 0 警告（dist 45MB）、clippy --all-targets 0 警告；cargo test --release 3 轮 123/123 无跳过；e2e_hooks 7/7；GUI 复验审批撤销（kill CLI 后卡片消失、远端文件未创建、审计 seq 272）；截图 screenshot-final-{approval-withdraw-before,approval-withdraw-after,tray,keyword-highlight,sftp-editor,sftp-editor-conflict,hosttools-monitor,managed-policy}.png；TEST_REPORT 重写（通过 75 / 失败 7 / 未验证 4）；PRD v1.0 同步（3.7 新行、5.18、Q19–Q23）；README
- ☑ 11.E 剩余未实现项已在第 12 轮完成（见 12.*）

# 第 12 轮（剩余 PRD 项 + 修复）
- ☑ 12.1 审计路径遵守 XDG_DATA_HOME：config::audit_dir（默认目录 → $XDG_DATA_HOME/agentshell/audit；显式目录不变），GUI 与 agentshell 共用；测试 config::audit_dir_tests
- ☑ 12.2 OS 徽章缓存键改为 SshTarget::pool_key（按主机条目含跳板链）
- ☑ 12.3 src/dirsync.rs（差异计划：单向/双向、删除仅在开启时且排最后、深者先删、2 万项上限）+ sftp::scan_remote / execute_sync（覆盖前删目标避免续传追加）+ 传输后保留 mtime；SFTP「同步…」面板：模式、删除开关、预览、有删除需二次确认，审计 sftp.sync；测试 dirsync::tests::plans、ssh_features::sftp_directory_sync；截图 screenshot-final-sftp-sync-*.png
- ☑ 12.4 端口转发图示：forward::Spec::diagram + port_view 每条规则画 监听 → SSH 服务器 → 目标（运行中绿色）；测试 forward::tests::diagram_nodes；截图 screenshot-final-port-forward-diagram.png
- ☑ 12.5 关键字触发通知：src/notify_rules.rs（正则、10 秒限流、跳过刚输入命令的回显行），[[notify.keywords]]；应用内提示 + 桌面通知 + 审计 notify.keyword（脱敏）
- ☑ 12.6 Sixel/Kitty：src/graphics.rs（自写解码器与流提取器，按流偏移放置，ED2/ED3/RIS 清除），term::Session::images，termview 用 gpui paint_image 绘制；依赖 image 0.25（MIT/Apache，已登记）；测试 graphics::tests、headless 两项；截图 screenshot-final-graphics-sixel-kitty.png
- ☑ 12.7 asciicast v2：src/asciicast.rs（默认脱敏、按行缓冲防分片泄露、关回显写 [input hidden]、空闲压缩）；Ctrl+Shift+R 录制、“回放录制”经 `agentshell play` 在新标签回放；[recording] redact/dir；截图 screenshot-final-recordings-list.png / -asciicast-playback.png
- ☑ 12.8 广播输入：src/broadcast.rs + termview 全局广播组（键入/IME/粘贴都扇出），红框+红条提示，生产主机先确认；Ctrl+Shift+B；审计 broadcast.*；截图 screenshot-final-broadcast*.png
- ☑ 12.9 布局快照：src/snapshot.rs（最近 10 份，去重）；保存布局时自动快照 + 手动保存；回滚列表（回滚前先快照当前）；截图 screenshot-final-layout-*.png
- ☑ 12.10 快速启动“选目录 + 预设布局”：Ctrl+Shift+O，目录候选/浏览，预设 单/左右/主+两侧/2×2/Agent+tail -F 最新日志；截图 screenshot-final-quickstart-*.png
- ☑ 12.11 审批撤销后迟到允许：测试钩子 AGENTSHELL_TEST_SLOW_WITHDRAW 保留卡片，迟到应答发送失败 → 审计 late-answer-ignored + 提示；GUI 实测远端文件未创建；截图 screenshot-final-late-allow-*.png
- ☑ 12.12 收尾：release / dist（46.0MB）0 警告、clippy --all-targets 0 警告；cargo test --release 3 轮 135/135 无跳过；e2e_hooks 7/7；TEST_REPORT 重写（通过 82 / 失败 0 / 未验证 4）；PRD v1.0、README 同步

# 第 13 轮（PRD v1.1 5.20 右键上下文菜单）
- ☑ 13.0 PRD 5.20 写入，v1.1，备份 PRD-v1.0-final.md
- ☑ 13.1 src/ctxmenu.rs 核心（类型判定、路径/URL/IP 检测、引号、模板、工具探测、TOML、菜单构建）+ 单测
- ☑ 13.2 OSC 7 cwd（term.rs + bash/zsh 集成）
- ☑ 13.3 GUI：终端右键菜单、SFTP 右键与多选、插入到对应主机会话、设置分区
- ☑ 13.4 集成测试（sshd：插入不执行、回车后解压）
- ☑ 13.5 截图 screenshot-final-ctxmenu-{terminal-path,inserted,enter-permission,missing-tool,missing-tool-terminal,filepanel-zip,filepanel-inserted,custom-item,settings,selection}.png（逐张看过）
- ☑ 13.6 全量：release + dist（46.3MB）0 警告、clippy --all-targets 0 警告、cargo test --release 3 轮 147/147、e2e_hooks 7/7；TEST_REPORT 5b 节、README 第 13 轮

# 第 14 轮（PRD v1.2 5.21 SFTP 显示模式）
- ☑ 14.0 PRD 5.21 写入，v1.2，备份 PRD-v1.1-final.md
- ☑ 14.1 src/sftp_mode.rs（模式、跟随路径映射、Follow 状态）+ 单测；session_store PaneSave::Sftp
- ☑ 14.2 GUI：TermView 叶子承载 SftpView（标签/分屏），独立窗口 SftpWindow，弹窗保留；标题栏模式切换；跟随终端目录
- ☑ 14.3 设置 session.sftp_mode / sftp_follow_cwd；命令面板 4 项；侧边栏主机右键菜单
- ☑ 14.4 布局保存 / 快照 / 恢复 SFTP 叶子（follow 重新挂到同标签终端）
- ☑ 14.5 集成测试 tests/sftp_modes_ssh.rs；截图 screenshot-final-sftp-{tab,split,window,popup}.png
- ☑ 14.6 全量：release + dist（42.8MB）0 警告、clippy 0、cargo test --release 3 轮 152/152、e2e_hooks 7/7；TEST_REPORT 5c、README 第 14 轮
- 注：tools/gui_final.sh 增加 RESTORE=1（取消 AGENTSHELL_NO_RESTORE）以测试恢复
