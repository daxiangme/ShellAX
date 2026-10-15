#!/bin/sh
# Launch the GUI on DISPLAY=:2 against the local test sshd (key auth).
cd "$(dirname "$0")/.." || exit 1
pkill -f "target/(debug|release)/shellax" 2>/dev/null; sleep 0.5
SSH_HOST=127.0.0.1 SSH_PORT=${SSH_PORT:-2222} SSH_USER=sshtest SSH_KEY=$PWD/testenv/client_ed25519 \
  DISPLAY=:2 nohup ./target/debug/shellax >/tmp/stp-gui.log 2>&1 &
sleep 4
DISPLAY=:2 xdotool search --name "shellax|ssh-term" 2>/dev/null | head -1
