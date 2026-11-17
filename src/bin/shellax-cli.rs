//! `shellax-cli`: talks to the local policy service of a running shellax (or runs one
//! headless with `serve`), acts as the command hook for Claude Code / Codex / Grok Build, and
//! installs those hooks.
//!
//!   shellax-cli status
//!   shellax-cli check [--host H] [--principal P] -- <command…>     exit 0 allow / 1 ask / 2 deny
//!   shellax-cli hook <claude|codex|grok> [PreToolUse|PermissionRequest]   (stdin → stdout)
//!   shellax-cli install <claude|codex|grok|all> [--uninstall] [--dry-run] [--home DIR]
//!   shellax-cli serve [--bind 127.0.0.1:0]
//!   shellax-cli hosts [--json]
//!   shellax-cli exec <host> [--timeout S] -- <command…>          exit = remote exit code (126 denied)
//!   shellax-cli upload <host> <local> <remote> · download <host> <remote> <local>
#[allow(dead_code)]
#[path = "../config.rs"]
mod config;

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ssh_term_proto::hooks::{self, Agent};
use ssh_term_proto::policy::Effect;
use ssh_term_proto::policy_server as ps;

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(Into::into)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("shellax")
}
fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn usage() -> ! {
    eprintln!("usage:\n  shellax-cli status\n  shellax-cli check [--host H] [--principal P] -- <command…>\n  shellax-cli hook <claude|codex|grok> [event]\n  shellax-cli install <claude|codex|grok|all> [--uninstall] [--dry-run] [--home DIR]\n  shellax-cli serve [--bind ADDR]\n  shellax-cli hosts [--json]\n  shellax-cli exec <host> [--timeout S] -- <command…>\n  shellax-cli upload <host> <local> <remote>\n  shellax-cli download <host> <remote> <local>\n  shellax-cli pane <list|split|send|keys|read|close> [args…]\n  shellax-cli agent <start|wait|prompt> [args…]\n  shellax-cli skill\n  shellax-cli play <file.cast> [--speed N] [--idle S]");
    std::process::exit(64)
}

fn flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(i) = args.iter().position(|a| a == name) {
        args.remove(i);
        true
    } else {
        false
    }
}
fn opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.remove(i);
    if i < args.len() {
        Some(args.remove(i))
    } else {
        None
    }
}

fn main() {
    ssh_term_proto::platform::init_env();
    // behave like other CLI tools when piped into `head` etc. (no EPIPE panic)
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let _ = ssh_term_proto::managed::load();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage()
    }
    let cmd = args.remove(0);
    let (settings, _) = config::Settings::load();
    ssh_term_proto::i18n::set_language(&settings.general.language);
    match cmd.as_str() {
        "status" => status(&settings),
        "check" => check(args, &settings),
        "hook" => hook(args, &settings),
        "install" => install(args, &settings),
        #[cfg(unix)]
        "daemon" => {
            let sock = args
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| ssh_term_proto::daemon::socket_path(&data_dir()));
            if let Err(e) = ssh_term_proto::daemon::serve(&sock) {
                eprintln!("shellax-cli daemon: {e:#}");
                std::process::exit(1)
            }
        }
        "serve" => serve(args, settings),
        "hosts" | "exec" | "upload" | "download" => remote(&cmd, args),
        "pane" => pane_cmd(args),
        "agent" => agent_cmd(args),
        "skill" => {
            print!("{}", include_str!("../../assets/skills/shellax/SKILL.md"));
        }
        "play" => std::process::exit(play(args)),
        "-h" | "--help" | "help" => usage(),
        _ => usage(),
    }
}

fn status(s: &config::Settings) {
    let dd = data_dir();
    match ps::read_endpoint(&dd) {
        Some(ep) => {
            let up = ps::post(
                &ep.url,
                "/v1/check",
                &ep.token,
                None,
                r#"{"command":"true","interactive":false}"#,
                Duration::from_secs(3),
            )
            .map(|r| r.0 == 200)
            .unwrap_or(false);
            println!(
                "policy service: {} ({})",
                ep.url,
                if up { "reachable" } else { "NOT reachable" }
            );
        }
        None => println!(
            "policy service: not running (no {})",
            ps::endpoint_path(&dd).display()
        ),
    }
    println!(
        "mode: {} · max auto risk: {} · rules: {}",
        s.permissions.mode.label(),
        s.permissions.max_auto_risk,
        s.permissions.rules.len()
    );
    for a in Agent::ALL {
        println!(
            "{:7} hook: {} ({})",
            a.id(),
            if hooks::installed(a, &home()) {
                "installed"
            } else {
                "not installed"
            },
            hooks::config_path(a, &home()).display()
        );
    }
}

fn check(mut args: Vec<String>, s: &config::Settings) {
    let host = opt(&mut args, "--host")
        .or_else(|| std::env::var("SHELLAX_HOST").ok())
        .or_else(|| {
            ps::read_endpoint(&data_dir())
                .map(|e| e.host)
                .filter(|h| !h.is_empty())
        });
    let principal = opt(&mut args, "--principal").unwrap_or_else(|| "cli".into());
    if args.first().is_some_and(|a| a == "--") {
        args.remove(0);
    }
    let command = args.join(" ");
    if command.is_empty() {
        usage()
    }
    let body = serde_json::json!({"principal": principal, "command": command, "host": host.clone().unwrap_or_default(), "interactive": false}).to_string();
    let (eff, risk, reason) = match ps::read_endpoint(&data_dir()).and_then(|ep| {
        ps::post(
            &ep.url,
            "/v1/check",
            &ep.token,
            None,
            &body,
            Duration::from_secs(10),
        )
        .ok()
    }) {
        Some((200, b, _)) => {
            let v: serde_json::Value = serde_json::from_str(&b).unwrap_or_default();
            let e = match v["decision"].as_str() {
                Some("allow") => Effect::Allow,
                Some("deny") => Effect::Deny,
                _ => Effect::Ask,
            };
            (
                e,
                v["risk"].as_str().unwrap_or("?").to_string(),
                v["reason"].as_str().unwrap_or("").to_string(),
            )
        }
        _ => {
            // no running app: evaluate locally with the same engine (nothing is audited here)
            let c = s.policy_ctx();
            let prod = c.prod_confirm
                && host.as_deref().is_some_and(|h| {
                    c.prod_hosts
                        .iter()
                        .any(|p| p == h || ssh_term_proto::policy::glob(p, h))
                });
            let (e, r) = ssh_term_proto::policy::decide_host(
                c.mode,
                c.max_auto,
                &c.rules,
                &principal,
                "exec",
                &command,
                c.dangerous_confirm,
                prod,
                host.as_deref().unwrap_or(""),
            );
            (
                e,
                ssh_term_proto::policy::classify(&command)
                    .label()
                    .to_string(),
                format!("{r} (offline)"),
            )
        }
    };
    let d = match eff {
        Effect::Allow => "allow",
        Effect::Ask => "ask",
        Effect::Deny => "deny",
    };
    println!("{d}\t{risk}\t{reason}");
    std::process::exit(match eff {
        Effect::Allow => 0,
        Effect::Ask => 1,
        Effect::Deny => 2,
    });
}

fn hook(mut args: Vec<String>, _s: &config::Settings) {
    if args.is_empty() {
        usage()
    }
    let Some(agent) = Agent::parse(&args.remove(0)) else {
        usage()
    };
    let event = args.first().cloned().unwrap_or_else(|| "PreToolUse".into());
    // Grok Build also reads ~/.claude/settings.json; when our Grok hook is installed it already
    // covers this call, so the Claude-format handler stays silent to avoid asking twice.
    if agent == Agent::Claude
        && std::env::var_os("GROK_HOOK_EVENT").is_some()
        && hooks::installed(Agent::Grok, &home())
    {
        std::process::exit(0);
    }
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let state_only = hooks::is_state_only_event(&event);
    let timeout = if state_only {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(3600)
    };
    let res = ps::read_endpoint(&data_dir())
        .ok_or_else(|| anyhow::anyhow!("shellax policy service is not running"))
        .and_then(|ep| {
            // on a remote host the endpoint file (written by shellax over SSH) names the host
            let host = std::env::var("SHELLAX_HOST")
                .ok()
                .filter(|h| !h.is_empty())
                .or_else(|| (!ep.host.is_empty()).then(|| ep.host.clone()));
            ps::post(
                &ep.url,
                &format!("/v1/hook/{}/{event}", agent.id()),
                &ep.token,
                host.as_deref(),
                &input,
                timeout,
            )
        });
    match res {
        Ok((200, body, exit)) => {
            print!("{body}");
            std::process::exit(exit.unwrap_or(0));
        }
        other => {
            if state_only {
                // Lifecycle state hooks must never block an agent when GUI is offline.
                std::process::exit(0);
            }
            let why = match other {
                Ok((code, b, _)) => format!("policy service answered {code}: {b}"),
                Err(e) => e.to_string(),
            };
            if std::env::var("SHELLAX_FAIL_OPEN").is_ok_and(|v| v == "1") {
                eprintln!("shellax-cli: {why} (fail-open)");
                std::process::exit(0);
            }
            // fail closed
            let (out, code) = hooks::render(
                agent,
                &event,
                Effect::Deny,
                &format!("{why}; failing closed"),
            );
            print!("{out}");
            eprintln!("shellax-cli: {why}; failing closed");
            std::process::exit(if code == 0 { 2 } else { code });
        }
    }
}

fn install(mut args: Vec<String>, s: &config::Settings) {
    let remove = flag(&mut args, "--uninstall");
    let dry = flag(&mut args, "--dry-run");
    let home = opt(&mut args, "--home")
        .map(PathBuf::from)
        .unwrap_or_else(home);
    let which = args.first().cloned().unwrap_or_else(|| usage());
    let agents: Vec<Agent> = if which == "all" {
        Agent::ALL.to_vec()
    } else {
        vec![Agent::parse(&which).unwrap_or_else(|| usage())]
    };
    let cli = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "shellax-cli".into());
    let timeout = s.agents.approval_timeout_secs + 15;
    let mut failed = false;
    for a in agents {
        match hooks::install(a, &home, &cli, timeout, remove, dry) {
            Ok(r) => {
                let verb = if remove {
                    "removed from"
                } else {
                    "installed in"
                };
                if dry {
                    println!(
                        "--- {} (dry run, nothing written)\n{}",
                        r.path.display(),
                        r.content
                    );
                } else if !r.changed {
                    println!("{}: already up to date ({})", a.id(), r.path.display());
                } else {
                    println!(
                        "{}: hook {verb} {}{}",
                        a.id(),
                        r.path.display(),
                        r.backup
                            .map(|b| format!(" (backup: {})", b.display()))
                            .unwrap_or_default()
                    );
                }
                if a == Agent::Codex && !remove && !dry {
                    println!("codex: open /hooks in Codex once to review and trust the new hook");
                }
            }
            Err(e) => {
                eprintln!("{}: {e}", a.id());
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1)
    }
}

fn serve(mut args: Vec<String>, s: config::Settings) {
    let bind = opt(&mut args, "--bind").unwrap_or_else(|| "127.0.0.1:0".into());
    config::install(s);
    let audit = Arc::new(ssh_term_proto::audit::AuditLog::new(
        config::get().audit_path(),
    ));
    // re-read config.toml on every request so edits apply without a restart
    let ctx: ps::CtxFn = Arc::new(|| config::Settings::load().0.policy_ctx());
    match ps::start(
        &bind,
        None,
        ctx,
        None,
        audit,
        Some(ps::endpoint_path(&data_dir())),
    ) {
        Ok(srv) => {
            println!("shellax-cli policy service on 127.0.0.1:{} (headless: \"ask\" is passed back to the agent)", srv.port);
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            eprintln!("serve: {e}");
            std::process::exit(1)
        }
    }
}

/// US-10: remote operations through the running App (credentials never leave it).
fn remote(cmd: &str, mut args: Vec<String>) {
    let Some(ep) = ps::read_endpoint(&data_dir()) else {
        eprintln!("shellax-cli: shellax is not running (no policy endpoint)");
        std::process::exit(3)
    };
    let principal = opt(&mut args, "--principal")
        .or_else(|| std::env::var("SHELLAX_PRINCIPAL").ok())
        .unwrap_or_else(|| "cli".into());
    let session = std::env::var("SHELLAX_SESSION").unwrap_or_default();
    let json_out = flag(&mut args, "--json");
    let timeout: u64 = opt(&mut args, "--timeout")
        .and_then(|t| t.parse().ok())
        .unwrap_or(600);
    let abs = |p: &str| {
        std::fs::canonicalize(p)
            .or_else(|_| std::env::current_dir().map(|d| d.join(p)))
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| p.to_string())
    };
    let body = match cmd {
        "hosts" => serde_json::json!({"principal": principal}),
        "exec" => {
            if args.is_empty() {
                usage()
            }
            let host = args.remove(0);
            if args.first().is_some_and(|a| a == "--") {
                args.remove(0);
            }
            if args.is_empty() {
                usage()
            }
            serde_json::json!({"principal": principal, "session": session, "host": host, "command": args.join(" "), "timeout_secs": timeout})
        }
        "upload" if args.len() == 3 => {
            serde_json::json!({"principal": principal, "session": session, "host": args[0], "local": abs(&args[1]), "remote": args[2]})
        }
        "download" if args.len() == 3 => {
            serde_json::json!({"principal": principal, "session": session, "host": args[0], "remote": args[1], "local": abs(&args[2])})
        }
        _ => usage(),
    };
    // approval (up to agents.approval_timeout) + the operation itself
    let wait = Duration::from_secs(timeout + 300);
    let (status, b) = match ps::post(
        &ep.url,
        &format!("/v1/{cmd}"),
        &ep.token,
        None,
        &body.to_string(),
        wait,
    ) {
        Ok((s, b, _)) => (s, b),
        Err(e) => {
            eprintln!("shellax-cli: {e:#}");
            std::process::exit(3)
        }
    };
    let v: serde_json::Value = serde_json::from_str(&b).unwrap_or_default();
    if json_out || status != 200 && v.get("error").is_none() {
        println!("{b}");
    }
    if let Some(e) = v["error"].as_str() {
        eprintln!("shellax-cli: {e}");
        std::process::exit(if status == 502 { 4 } else { 3 })
    }
    if cmd == "hosts" {
        if !json_out {
            for h in v["hosts"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{}\t{}@{}:{}{}",
                    h["name"].as_str().unwrap_or(""),
                    h["user"].as_str().unwrap_or(""),
                    h["host"].as_str().unwrap_or(""),
                    h["port"],
                    if h["production"].as_bool() == Some(true) {
                        "\tproduction"
                    } else {
                        ""
                    }
                );
            }
        }
        return;
    }
    let d = v["decision"].as_str().unwrap_or("");
    if d != "allow" {
        eprintln!(
            "shellax-cli: {d} ({}) {}",
            v["risk"].as_str().unwrap_or("?"),
            v["reason"].as_str().unwrap_or("")
        );
        std::process::exit(126)
    }
    if json_out {
        return;
    }
    if cmd == "exec" {
        use std::io::Write;
        let _ = std::io::stdout().write_all(v["output"].as_str().unwrap_or("").as_bytes());
        std::process::exit(v["exit"].as_i64().map(|x| x as i32).unwrap_or(255));
    }
    println!("{} bytes", v["bytes"]);
}

/// Replay an asciicast v2 file to stdout with its timing (idle gaps capped).
fn play(args: Vec<String>) -> i32 {
    let mut file = None;
    let (mut speed, mut idle) = (1.0f64, 2.0f64);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--speed" => {
                speed = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1.0f64)
                    .max(0.01)
            }
            "--idle" => idle = it.next().and_then(|v| v.parse().ok()).unwrap_or(2.0),
            _ => file = Some(a),
        }
    }
    let Some(f) = file else { usage() };
    let cast = match std::fs::read_to_string(&f)
        .map_err(anyhow::Error::from)
        .and_then(|t| ssh_term_proto::asciicast::parse(&t))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("play: {e:#}");
            return 1;
        }
    };
    use std::io::Write;
    let mut out = std::io::stdout();
    let start = std::time::Instant::now();
    let mut i = 0;
    while i < cast.events.len() {
        let next = cast.compressed_time(i, idle) / speed;
        let el = start.elapsed().as_secs_f64();
        if next > el {
            std::thread::sleep(std::time::Duration::from_secs_f64(next - el));
        }
        let (s, j) = cast.due(i, start.elapsed().as_secs_f64() * speed, idle);
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
        i = j.max(i + 1);
    }
    let _ = writeln!(out, "\r\n\x1b[2m[playback finished: {}]\x1b[0m", cast.title);
    0
}

fn api_call(method: &str, path: &str, body: &str, timeout: Duration) -> (u16, serde_json::Value) {
    let Some(ep) = ps::read_endpoint(&data_dir()) else {
        eprintln!("shellax-cli: policy service is not running");
        std::process::exit(3);
    };
    let host = std::env::var("SHELLAX_HOST")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| (!ep.host.is_empty()).then(|| ep.host.clone()));
    let pane = std::env::var("SHELLAX_PANE").ok().filter(|s| !s.is_empty());
    match ps::request(
        method,
        &ep.url,
        path,
        &ep.token,
        host.as_deref(),
        pane.as_deref(),
        body,
        timeout,
    ) {
        Ok((code, text, _)) => {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            (code, v)
        }
        Err(e) => {
            eprintln!("shellax-cli: {e:#}");
            std::process::exit(1);
        }
    }
}

fn exit_for_status(code: u16, v: &serde_json::Value) -> ! {
    let err = v
        .get("error")
        .and_then(|x| x.as_str())
        .unwrap_or("request failed");
    eprintln!("shellax-cli: {err} (HTTP {code})");
    std::process::exit(match code {
        408 | 504 => 1,
        401 | 403 => 2,
        404 => 3,
        _ => 1,
    });
}

fn pane_cmd(mut args: Vec<String>) {
    let json_out = flag(&mut args, "--json");
    if args.is_empty() {
        usage();
    }
    let sub = args.remove(0);
    match sub.as_str() {
        "list" | "ls" => {
            let (code, v) = api_call("GET", "/v1/pane/list", "", Duration::from_secs(10));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
                return;
            }
            if let Some(arr) = v.get("panes").and_then(|x| x.as_array()) {
                for p in arr {
                    println!(
                        "{}\ttab:{}\t{}\t{}\t{}",
                        p["pane_id"].as_str().unwrap_or(""),
                        p["tab"].as_u64().unwrap_or(0),
                        p["agent"].as_str().unwrap_or("-"),
                        p["state"].as_str().unwrap_or("-"),
                        p["cwd"].as_str().unwrap_or("")
                    );
                }
            }
        }
        "split" => {
            let from = opt(&mut args, "--from");
            let dir = opt(&mut args, "--dir").unwrap_or_else(|| "right".into());
            let cwd = opt(&mut args, "--cwd");
            let focus = flag(&mut args, "--focus");
            let cmd_str = if let Some(pos) = args.iter().position(|a| a == "--") {
                let tail: Vec<String> = args.drain(pos + 1..).collect();
                args.pop();
                (!tail.is_empty()).then(|| tail.join(" "))
            } else {
                opt(&mut args, "--cmd")
            };
            let body = serde_json::json!({
                "from": from,
                "dir": dir,
                "cwd": cwd,
                "cmd": cmd_str,
                "focus": focus,
            })
            .to_string();
            let (code, v) = api_call("POST", "/v1/pane/split", &body, Duration::from_secs(15));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            } else {
                println!("{}", v["pane_id"].as_str().unwrap_or(""));
            }
        }
        "send" => {
            let enter = flag(&mut args, "--enter");
            if args.len() < 2 {
                usage();
            }
            let pane_id = args.remove(0);
            let text = args.join(" ");
            let body = serde_json::json!({"pane_id": pane_id, "text": text, "enter": enter}).to_string();
            let (code, v) = api_call("POST", "/v1/pane/send_text", &body, Duration::from_secs(120));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            }
        }
        "keys" => {
            if args.len() < 2 {
                usage();
            }
            let pane_id = args.remove(0);
            let body = serde_json::json!({"pane_id": pane_id, "keys": args}).to_string();
            let (code, v) = api_call("POST", "/v1/pane/send_keys", &body, Duration::from_secs(120));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            }
        }
        "read" => {
            let lines: usize = opt(&mut args, "--lines")
                .and_then(|s| s.parse().ok())
                .unwrap_or(200);
            let Some(pane_id) = args.first().cloned() else {
                usage();
            };
            let path = format!(
                "/v1/pane/read?pane_id={}&lines={lines}",
                percent_encoding::utf8_percent_encode(&pane_id, percent_encoding::NON_ALPHANUMERIC)
            );
            let (code, v) = api_call("GET", &path, "", Duration::from_secs(10));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            } else {
                let text = v["text"].as_str().unwrap_or("");
                print!("{text}");
                if !text.ends_with('\n') {
                    println!();
                }
            }
        }
        "close" => {
            let Some(pane_id) = args.first().cloned() else {
                usage();
            };
            let body = serde_json::json!({"pane_id": pane_id}).to_string();
            let (code, v) = api_call("POST", "/v1/pane/close", &body, Duration::from_secs(120));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            }
        }
        _ => usage(),
    }
}

fn agent_cmd(mut args: Vec<String>) {
    let json_out = flag(&mut args, "--json");
    if args.is_empty() {
        usage();
    }
    let sub = args.remove(0);
    match sub.as_str() {
        "start" | "spawn" => {
            let cwd = opt(&mut args, "--cwd");
            let prompt = opt(&mut args, "--prompt");
            let task = opt(&mut args, "--task");
            let agent = opt(&mut args, "--agent")
                .or_else(|| args.first().cloned())
                .unwrap_or_else(|| usage());
            let body = serde_json::json!({
                "agent": agent,
                "cwd": cwd,
                "prompt": prompt,
                "task": task,
            })
            .to_string();
            let (code, v) = api_call("POST", "/v1/agent/start", &body, Duration::from_secs(120));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            } else {
                println!("{}", v["pane_id"].as_str().unwrap_or(""));
            }
        }
        "wait" => {
            let until = opt(&mut args, "--until").unwrap_or_else(|| "done,blocked".into());
            let timeout: u64 = opt(&mut args, "--timeout")
                .and_then(|s| s.parse().ok())
                .unwrap_or(600);
            let Some(pane_id) = args.first().cloned() else {
                usage();
            };
            let path = format!(
                "/v1/agent/wait?pane_id={}&until={}&timeout={timeout}",
                percent_encoding::utf8_percent_encode(&pane_id, percent_encoding::NON_ALPHANUMERIC),
                percent_encoding::utf8_percent_encode(&until, percent_encoding::NON_ALPHANUMERIC),
            );
            let (code, v) = api_call("GET", &path, "", Duration::from_secs(timeout + 15));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            } else {
                println!(
                    "{}\t{}",
                    v["pane_id"].as_str().unwrap_or(&pane_id),
                    v["state"].as_str().unwrap_or("unknown")
                );
            }
        }
        "prompt" => {
            if args.len() < 2 {
                usage();
            }
            let pane_id = args.remove(0);
            let prompt = args.join(" ");
            let body = serde_json::json!({"pane_id": pane_id, "prompt": prompt}).to_string();
            let (code, v) = api_call("POST", "/v1/agent/prompt", &body, Duration::from_secs(120));
            if code != 200 {
                exit_for_status(code, &v);
            }
            if json_out {
                println!("{v}");
            }
        }
        _ => usage(),
    }
}
