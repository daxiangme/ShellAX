#!/bin/sh
# Launch the release GUI on DISPLAY=:2 with an isolated config (used for final screenshots / CPU checks).
cd "$(dirname "$0")/.." || exit 1
pkill -f "target/release/ssh-term-proto|/tmp/base-bin" 2>/dev/null; sleep 0.5
mkdir -p /tmp/stp-gui/data
SSH_HOST=127.0.0.1 SSH_PORT=${SSH_PORT:-2222} SSH_USER=sshtest SSH_KEY=${SSH_KEY-$PWD/testenv/client_ed25519} \
  AGENTSHELL_CONFIG=/tmp/stp-gui/config.toml XDG_DATA_HOME=/tmp/stp-gui/data \
  AGENTSHELL_NO_UPDATE_CHECK=1 AGENTSHELL_NO_RESTORE=1 \
  AGENTSHELL_KNOWN_HOSTS=$PWD/testenv/known_hosts_demo AGENTSHELL_SSH_CONFIG=$PWD/testenv/ssh_config \
  PERF_LOG=${PERF_LOG:-} DISPLAY=:2 env ${RESTORE:+-u AGENTSHELL_NO_RESTORE} setsid -f nohup ${BIN:-./target/release/ssh-term-proto} >/tmp/stp-gui.log 2>&1 &
sleep 4
DISPLAY=:2 xdotool search --name "AgentShell|ssh-term" 2>/dev/null | head -1
