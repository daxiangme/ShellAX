#!/usr/bin/env bash
# End-to-end hook check with the real Claude Code / Codex / Grok Build CLIs driven by
# tools/mock_agent_llm.py (no API keys). Everything lives under an isolated $AH home.
# Prereq: CLIs installed in /tmp/agents (npm i @anthropic-ai/claude-code @openai/codex; grok via x.ai installer).
set -u
AH=${AH:-/tmp/agenthome}; BIN=/tmp/agents/bin; NM=/tmp/agents/node_modules/.bin
export HOME=$AH XDG_DATA_HOME=$AH/.local/share PATH=$BIN:$PATH MOCK_KEY=x GROK_DISABLE_AUTOUPDATER=1
mkdir -p $AH/.config/shellax $AH/.codex $AH/.grok /tmp/agentwork
cat > $AH/.config/shellax/config.toml <<'T'
[[permissions.rules]]
id = "deny-touch-test"
principal = "*"
action = "exec"
command = "touch *"
effect = "deny"
reason = "test rule"
T
printf 'model = "mock-model"\nmodel_provider = "mock"\n[model_providers.mock]\nname = "mock"\nbase_url = "http://127.0.0.1:18090/v1"\nwire_api = "responses"\nenv_key = "MOCK_KEY"\n' > $AH/.codex/config.toml
printf '[model.mock]\nmodel = "mock-model"\nbase_url = "http://127.0.0.1:18090/v1"\nname = "Mock"\nenv_key = "MOCK_KEY"\napi_backend = "messages"\n[models]\ndefault = "mock"\n' > $AH/.grok/config.toml
pgrep -f mock_agent_llm >/dev/null || (python3 "$(dirname "$0")/mock_agent_llm.py" 18090 >/dev/null 2>&1 &)
pgrep -f "shellax-cli serve" >/dev/null || ($BIN/shellax-cli serve >/tmp/as-serve.log 2>&1 &)
sleep 1
$BIN/shellax-cli install all >/dev/null
cd /tmp/agentwork && rm -f denied.txt
pass=0; fail=0
chk() { if echo "$2" | grep -q "$3"; then echo "PASS $1"; pass=$((pass+1)); else echo "FAIL $1: $2" | head -c 400; echo; fail=$((fail+1)); fi; }
for agent in claude codex grok; do
  for c in "ls /tmp/agentwork" "touch denied.txt"; do
    case $agent in
      claude) out=$(ANTHROPIC_BASE_URL=http://127.0.0.1:18090 ANTHROPIC_API_KEY=sk-ant-fake timeout 90 $NM/claude -p "RUN: $c" --output-format json 2>&1);;
      codex)  out=$(timeout 90 $NM/codex exec --skip-git-repo-check --dangerously-bypass-hook-trust "RUN: $c" </dev/null 2>&1);;
      grok)   out=$(timeout 90 $BIN/grok -p "RUN: $c" -m mock --always-approve </dev/null 2>&1);;
    esac
    if [ "$c" = "touch denied.txt" ]; then chk "$agent deny" "$out" "deny-touch-test"; else chk "$agent allow" "$out" "MOCK-DONE"; fi
  done
done
[ -e /tmp/agentwork/denied.txt ] && { echo "FAIL denied.txt exists"; fail=$((fail+1)); } || { echo "PASS denied.txt not created"; pass=$((pass+1)); }
echo "pass=$pass fail=$fail"; [ $fail -eq 0 ]
