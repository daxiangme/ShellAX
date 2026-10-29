#!/bin/sh
# Prepare the local loopback test environment: generate throwaway keys and fill in the repo path.
set -e
cd "$(dirname "$0")/.."
REPO=$PWD
for k in host_ed25519 client_ed25519; do [ -f testenv/$k ] || ssh-keygen -q -t ed25519 -N '' -f testenv/$k; done
[ -f testenv/client_pp_ed25519 ] || ssh-keygen -q -t ed25519 -N 'agentshell-pp-test' -f testenv/client_pp_ed25519
sed -i "s#@REPO@#$REPO#g" testenv/sshd_config testenv/sshd2_config testenv/sshd3_config testenv/ssh_config
echo "testenv ready (keys generated, paths set to $REPO)"
