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
- ☐ 7.6 更新检查
- ☐ 7.7 审计保留期
- ☐ 7.8 配置写回保留注释（toml_edit）
- ☐ 7.9 光标闪烁 CPU
- ☐ 7.10 "即将支持"占位清零（当前 32 处）

## 8 整体测试
- ☐ release 无警告、clippy、全部测试；TEST_REPORT.md；screenshot-final-*.png；PRD v1.0、README
