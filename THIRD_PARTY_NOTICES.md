# Third-Party Notices — ssh-term-proto (AgentShell prototype)

AgentShell is closed source. It **ports code only from Apache-2.0 or MIT projects**. Every ported
file keeps the upstream copyright, says what was changed, and is listed here.
It does **not** read or copy GPL/AGPL code: shell-rs, oxideterm, warp, or Zed's
terminal/terminal_view/editor.

## 1. Ported source code (copied or adapted into this repository)

| Our file | Source (repository @ commit / path) | License | What we did |
|---|---|---|---|
| `src/agent.rs` (functions marked `[herdr]`) | herdr @ dc6b6e5f · `src/detect/manifest.rs` | Apache-2.0 | Ported the manifest schema (rules/gates), gate compilation and matching (`contains`/`regex`/`line_regex`/`all`/`any`/`not`), highest-priority rule selection, and all region selectors (`osc_title`, `bottom_non_empty_lines(N)`, `prompt_box_body`, …). **Removed** remote manifest updates, local overrides, explain/diagnostics output and version gating. Agents are keyed by string id instead of an enum. `identify()` and the state labels are our own code. |
| `agents/claude.toml`, `agents/codex.toml`, `agents/grok.toml` | herdr @ dc6b6e5f · `distribution/agent-detection/*.toml` | Apache-2.0 | Copied verbatim. We only added a 3-line header naming the source and license. |
| `src/layout.rs` (`auto_direction`, `DEFAULT_CURSOR_HEIGHT_WIDTH_RATIO`) | zellij @ a6c3dba · `zellij-server/src/panes/tiled_panes/tiled_pane_grid.rs` (`TiledPaneGrid::split_pane`) | MIT | Reduced to a pure function of (rows, cols). The split tree in the same file is our own code. |

License texts: `licenses/herdr-LICENSE-Apache-2.0.txt`, `licenses/zellij-LICENSE-MIT.md`.

### herdr
Copyright (c) herdr contributors. Licensed under the Apache License, Version 2.0 (the "License");
you may not use these files except in compliance with the License. You may obtain a copy at
http://www.apache.org/licenses/LICENSE-2.0. Unless required by applicable law or agreed to in
writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
WARRANTIES OR CONDITIONS OF ANY KIND. herdr ships no NOTICE file (checked at dc6b6e5f).

### zellij
MIT License — Copyright (c) 2020 Zellij contributors. The full permission notice is in
`licenses/zellij-LICENSE-MIT.md` and must ship with any distribution that contains `src/layout.rs`.

### Wave Terminal (waveterm) — NOTICE registered, no code ported yet
Wave is Apache-2.0 and has a NOTICE file. **No Wave code has been ported in this round.** We register
the NOTICE now so that any later port (e.g. connection or block-layout ideas) has it from day one.
Apache-2.0 §4(d) requires redistributing the NOTICE content alongside any derivative work:

```
Copyright 2025, Command Line Inc.
```
(full texts: `licenses/waveterm-NOTICE.txt`, `licenses/waveterm-LICENSE-Apache-2.0.txt`)

## 2. Libraries linked as dependencies (not copied)

| Crate / asset | License |
|---|---|
| gpui-pre / gpui-pre-platform 0.3.9 (Zed's GPUI, published snapshot) | Apache-2.0 |
| alacritty_terminal 0.26.0 | Apache-2.0 |
| russh 0.64.1 (+ aws-lc-rs backend) | russh: Apache-2.0；aws-lc-rs: "ISC AND (Apache-2.0 OR ISC)"；aws-lc-sys: ISC AND Apache-2.0 AND MIT AND BSD-3-Clause 等组合（发布前需单独核对） |
| portable-pty 0.9.0 | MIT |
| regex 1.x, serde, toml, tokio, flume, anyhow, libc | MIT OR Apache-2.0 |
| Catppuccin Mocha / Latte palettes (`themes/catppuccin-*.toml`) | MIT, © Catppuccin |

Fonts are **not** bundled (see README "字体许可").
