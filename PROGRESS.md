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
- ☐ 3.1 双栏浏览、新建/重命名/删除/权限（删除需确认）
- ☐ 3.2 上传下载、队列与进度、断点续传、拖放

## 4 端口转发（P1）
- ☐ 4.1 本地 / 远程 / 动态 SOCKS；列表启停、状态、自动启动

## 5 会话持久化（P1）
- ☐ 5.1 布局保存与启动恢复，恢复时不自动重跑，退出确认
- ☐ 5.2 远端 tmux 持久会话适配
- ☐ 5.3 本地后台保活（server/client）
- ☐ 5.4 Agent resume

## 6 内置 AI（P1）
- ☐ 6.1 对话面板（流式，带脱敏后的终端上下文）
- ☐ 6.2 命令失败后一键解释
- ☐ 6.3 AI 命令建议接入灰字

## 7 其余（P2 / 3.7 设置全表）
- ☐ 7.1 桌面通知（Agent 等待输入 / 需确认）
- ☐ 7.2 IME 预编辑
- ☐ 7.3 分屏拖拽比例
- ☐ 7.4 标签标题随语言
- ☐ 7.5 iTerm2 主题导入
- ☐ 7.6 更新检查
- ☐ 7.7 审计保留期
- ☐ 7.8 配置写回保留注释（toml_edit）
- ☐ 7.9 光标闪烁 CPU
- ☐ 7.10 "即将支持"占位清零（当前 32 处）

## 8 整体测试
- ☐ release 无警告、clippy、全部测试；TEST_REPORT.md；screenshot-final-*.png；PRD v1.0、README
