# ShellAX Multi-Agent Orchestration Skill

You are running inside a **ShellAX** managed terminal pane (`$SHELLAX_PANE`). You can inspect panes, split companion terminals, delegate subtasks to parallel coding agents, wait for them to finish or block, and read their redacted output through `shellax-cli`.

## Environment
- `SHELLAX_PANE`: Stable ID of your own terminal pane.
- `SHELLAX_URL` / `SHELLAX_TOKEN`: Local ShellAX policy & orchestration endpoint (automatically read by `shellax-cli`).

## Commands

```sh
# List open panes and agent states (idle | working | blocked | done)
shellax-cli pane list --json

# Split a companion pane (e.g. test watcher or dev server) in your tab
shellax-cli pane split --dir right --cwd "$PWD" -- cargo test

# Start a child coding agent in a new pane and send an opening prompt
child=$(shellax-cli agent start claude --cwd "$PWD" --prompt "Audit error handling in src/ssh.rs. Read-only." --json | jq -r '.pane_id')

# Wait until the child agent finishes its turn (done) or needs human input (blocked)
shellax-cli agent wait "$child" --until done,blocked --timeout 600

# Read the child pane's screen + scrollback (automatically redacted)
shellax-cli pane read "$child" --lines 150

# Send follow-up instructions to a child agent once idle/done
shellax-cli agent prompt "$child" "Also verify timeout edge cases."

# Send raw text or named keys (enter, esc, ctrl+c, tab, up, down)
shellax-cli pane send "$child" "y" --enter
shellax-cli pane keys "$child" esc

# Clean up a child pane when finished
shellax-cli pane close "$child"
```

## Permission & Trust Rules
1. **Child panes**: You may freely send prompts, keys, and close commands to panes you spawned yourself.
2. **Other panes**: Writing to or closing panes you did not create is governed by ShellAX's `write_to_pty` policy (defaults to interactive user approval).
3. **Readiness**: `agent start --prompt` and `agent prompt` wait up to 30s for the target agent to reach `idle` before pasting text. If `sent: false` is returned (e.g. a directory trust dialog is waiting), run `shellax-cli pane read <id>` first.
4. **Exit codes**: `0` ok, `1` timeout, `2` denied by policy, `3` pane not found.

## Common Workflows
- **Parallel Code Review**: Spawn two read-only reviewer agents (`claude` and `codex`), `agent wait` on each, read both summaries, and synthesize findings.
- **Test + Fix Loop**: Split a companion pane running tests (`pane split --dir right -- cargo test`), read failures with `pane read`, edit code, and re-run.
- **Cross-Agent Verification**: Implement a change, then spawn a fresh reviewer agent to verify the `git diff` without context bias.
