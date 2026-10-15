#!/bin/sh
# Launch the release GUI on DISPLAY=:2 with an isolated config (used for final screenshots / CPU checks).
cd "$(dirname "$0")/.." || exit 1
pkill -f "target/release/shellax|/tmp/base-bin" 2>/dev/null; sleep 0.5
mkdir -p /tmp/stp-gui/data
SSH_HOST=127.0.0.1 SSH_PORT=${SSH_PORT:-2222} SSH_USER=sshtest SSH_KEY=${SSH_KEY-$PWD/testenv/client_ed25519} \
  SHELLAX_CONFIG=/tmp/stp-gui/config.toml XDG_DATA_HOME=/tmp/stp-gui/data \
  SHELLAX_NO_UPDATE_CHECK=1 SHELLAX_NO_RESTORE=1 \
  SHELLAX_KNOWN_HOSTS=$PWD/testenv/known_hosts_demo SHELLAX_SSH_CONFIG=$PWD/testenv/ssh_config \
  PERF_LOG=${PERF_LOG:-} DISPLAY=:2 env ${RESTORE:+-u SHELLAX_NO_RESTORE} setsid -f nohup ${BIN:-./target/release/shellax} >/tmp/stp-gui.log 2>&1 &
sleep 4
DISPLAY=:2 xdotool search --name "shellax|ssh-term" 2>/dev/null | head -1
