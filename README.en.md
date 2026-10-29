# ShellAX

[中文](README.md)

ShellAX is a cross-platform terminal and SSH client written in Rust and GPUI. Besides the terminal and host list, one window also holds SFTP, databases (including Dameng), Redis, Docker, port forwarding, and everyday utilities, with permission control and an audit log for AI agents.

Platforms: Windows x64 and Mac (Apple Silicon). There is no Linux release build yet; build it from source.

## Features

### Modules on the left
The activity bar switches modules. Each module has its own tabs, and the last tab you used is remembered. Click the current module again to collapse its tool window.

| Module | What it does |
|---|---|
| **Hosts / Terminal** | Host tree and terminals: a local shell and SSH sessions. The path bar and CPU, memory, disk, load, and latency appear only in this module |
| **SFTP** | Two-pane file manager. Each side is this computer or any saved host |
| **Database** | Connections, table browsing, and a SQL editor, plus a document browser |
| **Redis** | Key tree, typed viewers and editors, and a console |
| **Docker** | The local engine, or an engine reached through SSH |
| **Ports** | Port forwarding |
| **Tools** | 44 built-in utilities, plus downloadable extension packs |

The bottom tool window has two tabs: **Transfers** and **Execution log** (what each module has run).

### Terminal and SSH
- **Terminal**: local shell and SSH sessions, tabs, splits (drag a tab to a pane edge), regex search, rectangular selection, URL detection, OSC 52, keyword highlighting, Sixel / Kitty graphics, asciicast v2 recording and playback (recordings are redacted by default and skip hidden input), broadcast input (confirm when a target is a production host), layout snapshots and rollback, and restoring the last layout on startup.
- **Suggestions and history**: gray ghost text from history (Right arrow or Tab to accept). While typing at the prompt, matching history can list under the cursor; Up/Down to choose, Enter or Tab to fill the line, Esc to close (this can be turned off). History can be kept per host or read from the remote shell. Commands that contain secrets are filtered; the rules are editable.
- **SSH**: public key, password, ssh-agent, and keyboard-interactive (2FA); known_hosts checks; ProxyJump; HTTP CONNECT and SOCKS5 proxies; connection reuse; automatic reconnect; optional remote tmux to keep the session.
- **Hosts**: nested groups (`a/b/c`), color tags, a production mark, and a menu to change the group, place, or icon (8 colors, 12 folder glyphs, 11 host icons). Import from `~/.ssh/config`, open `ssh://` and `sftp://` links, and generate or import keys. Key passphrases and API keys go into the OS keychain, or an encrypted file if the keychain is unavailable. They are not written into the config file.

### SFTP
SFTP opens in its own tab. Each side is this computer or any saved host, so you can copy between local and remote or between two remotes. The transfer queue can resume. There is a text editor with conflict detection, and directory sync (one way or both ways, with a preview; deletes must be confirmed).

### Database
- Connections: MySQL, MariaDB, StarRocks, OceanBase, PostgreSQL, openGauss, Kingbase, Dameng, SQLite, SQL Server, ClickHouse, and MongoDB (its own document tab). Connect directly or through an SSH tunnel. A connection can be marked read-only or production. The tree search matches connection names and names of tables you already opened, and highlights the match.
- Tables: server-side paging, filter, and sort; staged edits (submitted after you confirm); structure and DDL views; table design (columns, indexes, foreign keys).
- SQL editor: highlighting, completion, multiple result tabs, query history, and saved queries.
- Export CSV / JSON / SQL INSERT. Import CSV / JSON.
- MongoDB: database and collection tree, paged documents with JSON filter / sort / projection, tree or JSON view, edit / insert / delete, indexes, a basic aggregation pipeline editor, and JSON export.
- DuckDB is an optional compile feature (`--features duckdb`) and is not in release builds.
- **Database MCP**: a local MCP server on `127.0.0.1`, currently for MySQL and Dameng. Access is granted per database and table. Writes must be confirmed and are audited. The port is chosen once and kept; it changes only when you refresh it. You can also set a fixed port. Copy the token and client config from the right-hand card into Cursor, Claude Code, or Codex.

### Redis
Standalone, sentinel, or cluster, with ACL users and TLS, through an SSH tunnel if you want. The key tree uses SCAN (not KEYS). View and edit string, hash, list, set, zset, and stream values. TTL, rename, delete, server info, slow log, and a command console that warns on dangerous commands.

### Docker
The local engine, or a remote engine through SSH. Containers are grouped by Compose project. Start, stop, restart, pause, remove, inspect, logs, and exec (each in its own tab), plus Compose actions, images (pull / remove), volumes, and networks.

### Port forwarding
Local (`-L`), remote (`-R`), and dynamic SOCKS5 (`-D`). Saved forwards can be started by hand, or started when you connect to that host. The list shows status, connection count, and traffic.

### Tools and extension packs
44 built-in tools in 9 groups: units and money, text, development, crypto and encoding, date and time, generators, Chinese and time zones, images and barcodes, and PDF. Favorite them and see recent ones. Multiple results stay as separate rows with history. Input and output grow with the content; a large block can be split with a drag handle.

Some tools need an extension pack. Packs are WebAssembly modules downloaded to `<config dir>/ext/<id>/` and run in a sandbox (no imports, limited instructions and memory, a new instance per call). See `docs/EXTENSIONS.md`.

| Pack | Tools |
|---|---|
| `lang-zh` | Pinyin, simplified / traditional Chinese |
| `worldtime` | World clock, time-zone conversion |
| `image` | QR codes, barcodes, image compression, watermarks |
| `pdf` | Merge, split, rotate, delete pages, reorder, watermark, page numbers, images to PDF, extract text, strip metadata |
| `pdf-render` | PDF to images |

### Right-hand column
Six cards: **Host tools**, **AI assistant**, **Snippets**, **History**, **Directory**, and **Database MCP**. In Settings → Appearance you can allow several cards open at once, or only one (opening a card replaces the current one). While Database MCP is listening, a dot stays on its icon after the card is collapsed or closed.
- **Host tools**: monitoring, processes, systemd services, and Docker containers for the current machine (local or SSH), plus kill, service, and container actions. Writes go through the permission engine and the audit log. On a local Windows machine only the overview and processes are available.
- **AI assistant**: streaming multi-turn chat, with optional context from the current terminal. After a failed command, “explain this failure” is one click. Database, Redis, and Docker views have their own context and prompts. A generated command is inserted (no Enter) or run, and the permission mode decides whether it is allowed.
- **Snippets**: one list for every module, grouped, with `{{variables}}` filled in before insert or run. Focus in the SQL editor, Redis console, or terminal inserts there (the Redis console runs only after a click). Otherwise the text is copied. Multi-line scripts ask first. An empty list can be filled with terminal, SQL, Redis, and Docker examples.
- **History**: this host or all hosts, with search. Insert, copy, save as a snippet, delete one row, or clear this host.
- **Directory**: the current directory of a terminal tab. Local paths are read from disk. SSH uses the SFTP session that is already open.
- **Database MCP**: see above. The card holds the token, listen address, timeout, max rows, and what each table may do.

### AI and agent control
- **Built-in AI (your own key)**: OpenAI-compatible and Anthropic APIs, several providers, model list and selection, and a connection test. The API key stays in the OS keychain. At a prompt, `# request` then Enter (or Ctrl+Shift+G) drafts a command from plain language.
- **Privacy**: secrets and tokens are redacted before anything is sent. Choose how much context to send (nothing / last command / selection / current screen) and whether to include command output. Host-name patterns can block output. The first send tells you what will leave the machine.
- **Permission engine**: three modes (ask / read-only plan / auto), risk levels, and allow / ask / deny rules. Writes on production hosts and dangerous commands can require a confirmation. Ctrl+Shift+M cycles the mode.
- **External agents**: ShellAX notices command-line agents in the terminal (idle / working / waiting for approval) and can notify on a state change. An approval can be denied on timeout, or left waiting.
- **`shellax-cli` and the local policy server**: install command hooks so every command is judged by the permission engine. An agent can also `exec`, `upload`, and `download` on a saved host by name. Credentials and keys stay inside ShellAX.
- **Audit**: append-only JSONL with a SHA-256 hash chain. Retention is configurable. Ctrl+Shift+L opens the viewer.
- **Managed policy**: `/etc/shellax/managed-policy.yaml` (read-only, highest priority) can force hard denies, cap the permission mode, limit risk on production hosts, and set a minimum audit retention.

### Interface and settings
- Light, dark, and follow-the-system themes, built-in themes and terminal palettes, and imported palettes (ShellAX TOML, `.itermcolors`, and generic TOML). Chinese and English UI. Optional reduced motion. A system tray on Linux shows how many agents are running. Command palette (Ctrl+Shift+P). The context menu and shortcuts can be edited in Settings.
- Settings: General, Appearance, Terminal, Suggestions, SSH, Proxy, Credentials and keys, AI, External agents, Permissions, Tags, Audit, Session and layout, Shortcuts, Notifications, Context menu, Extensions, About.

### Updates
- Updates come from GitHub Releases by default, on a stable or preview channel. A custom manifest URL is `general.update_url` or `SHELLAX_UPDATE_URL`.
- Download by hand or automatically. The file is checked with SHA-256, then installed after you quit. The Windows installer updates silently and restarts. On Mac, ShellAX replaces the app when the folder is writable; otherwise it reveals the package. A portable build shows the package in its folder.

## Screenshots

Screenshots will be added later.

## Install

### Windows
Download `ShellAX-Setup-x64.exe` (installer) or `ShellAX-portable-x64.zip` (portable) from the releases page. The installer is unsigned. SmartScreen may say it protected your PC; choose More info → Run anyway.

### Mac
Open `ShellAX-macos-aarch64.dmg` (or the `.zip` of the same name) and drag ShellAX to Applications. The app is ad-hoc signed and not notarized. If the first launch says the developer cannot be verified:
- In Applications, right-click ShellAX → Open; or
- System Settings → Privacy & Security → Open Anyway; or
- In a terminal: `xattr -dr com.apple.quarantine /Applications/ShellAX.app`.

## Programs and directories
- GUI: `shellax` (`shellax.exe` on Windows).
- CLI: `shellax-cli`:
  - `status`: local policy server status
  - `check`: permission result for one command (exit 0 allow / 1 ask / 2 deny)
  - `hook`, `install`: command hooks for external agents, and install / uninstall
  - `serve`: run the policy server without a window
  - `hosts`, `exec`, `upload`, `download`: list, run, and transfer on saved hosts
  - `play`: play an asciicast recording
  - `daemon`: keep local shells alive after the window closes (turn this on under Session and layout; Linux and Mac only)
- Config: `~/.config/shellax/config.toml` (Windows: `%APPDATA%\ShellAX`). Data: `~/.local/share/shellax` (Windows: `%LOCALAPPDATA%\shellax`).
- Environment: `SHELLAX_CONFIG` selects the config file, `SHELLAX_NO_ANIM=1` turns animation off, `SHELLAX_SERVER_DECORATIONS=1` uses the system title bar, `SHELLAX_UPDATE_URL` sets a custom update manifest.

## Build
Rust stable (CI and releases use stable). Platform dependencies and debugging are in `docs/DEVELOPMENT.md`.
```bash
cargo build --release
cargo build --profile dist        # release build: LTO + strip
```
Windows packages: `tools/package_win.ps1` (on Windows) or `tools/package_win.sh` (cross-compile). Extension packs are a separate workspace; see `docs/EXTENSIONS.md`.

## Tests
```bash
testenv/setup.sh                  # one-off local keys and paths
/usr/sbin/sshd -f $PWD/testenv/sshd_config   # loopback sshd (127.0.0.1:2222)
source testenv/test.env
cargo test --release
```
The test account, port, and password are only for the loopback environment.

## Docs
- `docs/PRD.md`: product requirements
- `docs/DEVELOPMENT.md`: development notes
- `docs/EXTENSIONS.md`: extension-pack format, current packs, and publishing
- `docs/UI_REDESIGN_PLAN.md`: interface rules
- `docs/WINDOWS_TEST.md`: Windows test notes
- `docs/screenshots/`: screenshots

## License
ShellAX is AGPL-3.0. See `LICENSE`. Third-party notices are in `THIRD_PARTY_NOTICES.md` and `licenses/`.
