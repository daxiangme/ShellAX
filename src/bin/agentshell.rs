//! `agentshell` CLI: talks to the local policy service of a running AgentShell (or runs one
//! headless with `serve`), acts as the command hook for Claude Code / Codex / Grok Build, and
//! installs those hooks.
//!
//!   agentshell status
//!   agentshell check [--host H] [--principal P] -- <command…>     exit 0 allow / 1 ask / 2 deny
//!   agentshell hook <claude|codex|grok> [PreToolUse|PermissionRequest]   (stdin → stdout)
//!   agentshell install <claude|codex|grok|all> [--uninstall] [--dry-run] [--home DIR]
//!   agentshell serve [--bind 127.0.0.1:0]
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
    std::env::var_os("XDG_DATA_HOME").map(Into::into)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"))
        .join("agentshell")
}
fn home() -> PathBuf { PathBuf::from(std::env::var("HOME").unwrap_or_default()) }

fn usage() -> ! {
    eprintln!("usage:\n  agentshell status\n  agentshell check [--host H] [--principal P] -- <command…>\n  agentshell hook <claude|codex|grok> [event]\n  agentshell install <claude|codex|grok|all> [--uninstall] [--dry-run] [--home DIR]\n  agentshell serve [--bind ADDR]");
    std::process::exit(64)
}

fn flag(args: &mut Vec<String>, name: &str) -> bool { if let Some(i) = args.iter().position(|a| a == name) { args.remove(i); true } else { false } }
fn opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.remove(i);
    if i < args.len() { Some(args.remove(i)) } else { None }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() { usage() }
    let cmd = args.remove(0);
    let (settings, _) = config::Settings::load();
    ssh_term_proto::i18n::set_language(&settings.general.language);
    match cmd.as_str() {
        "status" => status(&settings),
        "check" => check(args, &settings),
        "hook" => hook(args, &settings),
        "install" => install(args, &settings),
        #[cfg(unix)]
        "daemon" => { let sock = args.first().map(PathBuf::from).unwrap_or_else(|| ssh_term_proto::daemon::socket_path(&data_dir())); if let Err(e) = ssh_term_proto::daemon::serve(&sock) { eprintln!("agentshell daemon: {e:#}"); std::process::exit(1) } }
        "serve" => serve(args, settings),
        "-h" | "--help" | "help" => usage(),
        _ => usage(),
    }
}

fn status(s: &config::Settings) {
    let dd = data_dir();
    match ps::read_endpoint(&dd) {
        Some(ep) => {
            let up = ps::post(&ep.url, "/v1/check", &ep.token, None, r#"{"command":"true","interactive":false}"#, Duration::from_secs(3)).map(|r| r.0 == 200).unwrap_or(false);
            println!("policy service: {} ({})", ep.url, if up { "reachable" } else { "NOT reachable" });
        }
        None => println!("policy service: not running (no {})", ps::endpoint_path(&dd).display()),
    }
    println!("mode: {} · max auto risk: {} · rules: {}", s.permissions.mode.label(), s.permissions.max_auto_risk, s.permissions.rules.len());
    for a in Agent::ALL {
        println!("{:7} hook: {} ({})", a.id(), if hooks::installed(a, &home()) { "installed" } else { "not installed" }, hooks::config_path(a, &home()).display());
    }
}

fn check(mut args: Vec<String>, s: &config::Settings) {
    let host = opt(&mut args, "--host").or_else(|| std::env::var("AGENTSHELL_HOST").ok())
        .or_else(|| ps::read_endpoint(&data_dir()).map(|e| e.host).filter(|h| !h.is_empty()));
    let principal = opt(&mut args, "--principal").unwrap_or_else(|| "cli".into());
    if args.first().is_some_and(|a| a == "--") { args.remove(0); }
    let command = args.join(" ");
    if command.is_empty() { usage() }
    let body = serde_json::json!({"principal": principal, "command": command, "host": host.clone().unwrap_or_default(), "interactive": false}).to_string();
    let (eff, risk, reason) = match ps::read_endpoint(&data_dir()).and_then(|ep| ps::post(&ep.url, "/v1/check", &ep.token, None, &body, Duration::from_secs(10)).ok()) {
        Some((200, b, _)) => {
            let v: serde_json::Value = serde_json::from_str(&b).unwrap_or_default();
            let e = match v["decision"].as_str() { Some("allow") => Effect::Allow, Some("deny") => Effect::Deny, _ => Effect::Ask };
            (e, v["risk"].as_str().unwrap_or("?").to_string(), v["reason"].as_str().unwrap_or("").to_string())
        }
        _ => {
            // no running app: evaluate locally with the same engine (nothing is audited here)
            let c = s.policy_ctx();
            let prod = c.prod_confirm && host.as_deref().is_some_and(|h| c.prod_hosts.iter().any(|p| p == h || ssh_term_proto::policy::glob(p, h)));
            let (e, r) = ssh_term_proto::policy::decide_ctx(c.mode, c.max_auto, &c.rules, &principal, "exec", &command, c.dangerous_confirm, prod);
            (e, ssh_term_proto::policy::classify(&command).label().to_string(), format!("{r} (offline)"))
        }
    };
    let d = match eff { Effect::Allow => "allow", Effect::Ask => "ask", Effect::Deny => "deny" };
    println!("{d}\t{risk}\t{reason}");
    std::process::exit(match eff { Effect::Allow => 0, Effect::Ask => 1, Effect::Deny => 2 });
}

fn hook(mut args: Vec<String>, _s: &config::Settings) {
    if args.is_empty() { usage() }
    let Some(agent) = Agent::parse(&args.remove(0)) else { usage() };
    let event = args.first().cloned().unwrap_or_else(|| "PreToolUse".into());
    // Grok Build also reads ~/.claude/settings.json; when our Grok hook is installed it already
    // covers this call, so the Claude-format handler stays silent to avoid asking twice.
    if agent == Agent::Claude && std::env::var_os("GROK_HOOK_EVENT").is_some() && hooks::installed(Agent::Grok, &home()) { std::process::exit(0); }
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let res = ps::read_endpoint(&data_dir()).ok_or_else(|| anyhow::anyhow!("AgentShell policy service is not running"))
        .and_then(|ep| {
            // on a remote host the endpoint file (written by AgentShell over SSH) names the host
            let host = std::env::var("AGENTSHELL_HOST").ok().filter(|h| !h.is_empty()).or_else(|| (!ep.host.is_empty()).then(|| ep.host.clone()));
            ps::post(&ep.url, &format!("/v1/hook/{}/{event}", agent.id()), &ep.token, host.as_deref(), &input, Duration::from_secs(3600))
        });
    match res {
        Ok((200, body, exit)) => { print!("{body}"); std::process::exit(exit.unwrap_or(0)); }
        other => {
            let why = match other { Ok((code, b, _)) => format!("policy service answered {code}: {b}"), Err(e) => e.to_string() };
            if std::env::var("AGENTSHELL_FAIL_OPEN").is_ok_and(|v| v == "1") { eprintln!("agentshell: {why} (fail-open)"); std::process::exit(0); }
            // fail closed
            let (out, code) = hooks::render(agent, &event, Effect::Deny, &format!("{why}; failing closed"));
            print!("{out}");
            eprintln!("agentshell: {why}; failing closed");
            std::process::exit(if code == 0 { 2 } else { code });
        }
    }
}

fn install(mut args: Vec<String>, s: &config::Settings) {
    let remove = flag(&mut args, "--uninstall");
    let dry = flag(&mut args, "--dry-run");
    let home = opt(&mut args, "--home").map(PathBuf::from).unwrap_or_else(home);
    let which = args.first().cloned().unwrap_or_else(|| usage());
    let agents: Vec<Agent> = if which == "all" { Agent::ALL.to_vec() } else { vec![Agent::parse(&which).unwrap_or_else(|| usage())] };
    let cli = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "agentshell".into());
    let timeout = s.agents.approval_timeout_secs + 15;
    let mut failed = false;
    for a in agents {
        match hooks::install(a, &home, &cli, timeout, remove, dry) {
            Ok(r) => {
                let verb = if remove { "removed from" } else { "installed in" };
                if dry { println!("--- {} (dry run, nothing written)\n{}", r.path.display(), r.content); }
                else if !r.changed { println!("{}: already up to date ({})", a.id(), r.path.display()); }
                else { println!("{}: hook {verb} {}{}", a.id(), r.path.display(), r.backup.map(|b| format!(" (backup: {})", b.display())).unwrap_or_default()); }
                if a == Agent::Codex && !remove && !dry { println!("codex: open /hooks in Codex once to review and trust the new hook"); }
            }
            Err(e) => { eprintln!("{}: {e}", a.id()); failed = true; }
        }
    }
    if failed { std::process::exit(1) }
}

fn serve(mut args: Vec<String>, s: config::Settings) {
    let bind = opt(&mut args, "--bind").unwrap_or_else(|| "127.0.0.1:0".into());
    config::install(s);
    let audit = Arc::new(ssh_term_proto::audit::AuditLog::new(config::get().audit_path()));
    // re-read config.toml on every request so edits apply without a restart
    let ctx: ps::CtxFn = Arc::new(|| config::Settings::load().0.policy_ctx());
    match ps::start(&bind, None, ctx, None, audit, Some(ps::endpoint_path(&data_dir()))) {
        Ok(srv) => { println!("agentshell policy service on 127.0.0.1:{} (headless: \"ask\" is passed back to the agent)", srv.port); loop { std::thread::park(); } }
        Err(e) => { eprintln!("serve: {e}"); std::process::exit(1) }
    }
}
