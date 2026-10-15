//! Permission policy model (PRD 6.3–6.5): modes, risk levels, allow/ask/deny rules
//! with last-match-wins evaluation. Original code. Enforcement for the built-in AI /
//! agent bridge is not implemented yet; the settings UI edits and tests these rules.
use crate::i18n::{t, tf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode { #[default] Ask, Plan, Auto }

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Ask, Mode::Plan, Mode::Auto];
    pub fn label(self) -> &'static str { match self { Mode::Ask => "Ask", Mode::Plan => "Plan", Mode::Auto => "Auto" } }
    pub fn desc(self) -> &'static str {
        match self {
            Mode::Ask => t("ui.ask_before_every_write_command"),
            Mode::Plan => t("ui.read_only_view_and_plan"),
            Mode::Auto => t("ui.allow_actions_up_to_max"),
        }
    }
    pub fn next(self) -> Mode { match self { Mode::Ask => Mode::Plan, Mode::Plan => Mode::Auto, Mode::Auto => Mode::Ask } }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Effect { Allow, #[default] Ask, Deny }

impl Effect {
    pub fn label(self) -> &'static str { match self { Effect::Allow => t("ui.allow"), Effect::Ask => t("ui.ask"), Effect::Deny => t("ui.deny") } }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Rule {
    pub id: String,
    /// "*" | "ai" | "claude" | "codex" | "grok" ...
    #[serde(default = "star")]
    pub principal: String,
    /// "exec" | "read" | "write" | "pty_write" | "*"
    #[serde(default = "star")]
    pub action: String,
    /// glob over the command line: `*` any run of chars, `?` one char
    #[serde(default = "star")]
    pub command: String,
    pub effect: Effect,
    #[serde(default)]
    pub reason: String,
}
fn star() -> String { "*".into() }

pub fn glob(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) { pi += 1; ti += 1; }
        else if pi < p.len() && p[pi] == '*' { star = Some(pi); mark = ti; pi += 1; }
        else if let Some(sp) = star { pi = sp + 1; mark += 1; ti = mark; }
        else { return false; }
    }
    while pi < p.len() && p[pi] == '*' { pi += 1; }
    pi == p.len()
}

/// Last matching rule wins (PRD 6.5).
pub fn evaluate<'a>(rules: &'a [Rule], principal: &str, action: &str, command: &str) -> Option<(usize, &'a Rule)> {
    rules.iter().enumerate().rev().find(|(_, r)| glob(&r.principal, principal) && glob(&r.action, action) && glob(&r.command, command.trim()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk { R0, R1, R2, R3, R4, Hard }

impl Risk {
    pub fn label(self) -> &'static str { match self { Risk::R0 => "R0", Risk::R1 => "R1", Risk::R2 => "R2", Risk::R3 => "R3", Risk::R4 => "R4", Risk::Hard => "HARD" } }
    pub fn desc(self) -> &'static str {
        match self {
            Risk::R0 => t("ui.read_only_no_side_effects"),
            Risk::R1 => t("ui.local_reversible_writes_create_modify"),
            Risk::R2 => t("ui.affects_running_state_but_recoverable"),
            Risk::R3 => t("ui.destructive_or_affects_others_delete"),
            Risk::R4 => t("ui.dangerous_irreversible_rm_rf_drop"),
            Risk::Hard => t("ui.hard_blocked_no_mode_or"),
        }
    }
}

/// Command risk classification. The command line is parsed with tree-sitter-bash and every
/// simple command (in pipelines, lists, subshells, `$( )`, `bash -c '…'`, `find -exec`, `xargs`,
/// behind `sudo`/`env`/`nohup`/…) is classified separately; redirections are classified too.
/// The overall risk is the maximum. Unknown commands are R2; unparsable input is R3.
pub fn classify(cmd: &str) -> Risk { classify_depth(cmd, 0) }

fn classify_depth(cmd: &str, depth: u8) -> Risk {
    let c = cmd.trim();
    if c.is_empty() { return Risk::R0; }
    if depth > 4 { return Risk::R3; }
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_bash::LANGUAGE.into()).is_err() { return Risk::R3; }
    let Some(tree) = parser.parse(c, None) else { return Risk::R3 };
    let root = tree.root_node();
    let mut risk = if root.has_error() { Risk::R2 } else { Risk::R0 };
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "command" => {
                let words = command_words(n, c);
                risk = risk.max(classify_words(&words, depth));
            }
            "file_redirect" => risk = risk.max(classify_redirect(n, c)),
            "function_definition" => risk = risk.max(Risk::R2),
            _ => {}
        }
        if risk == Risk::Hard { return risk; }
        let mut cur = n.walk();
        for ch in n.children(&mut cur) { stack.push(ch); }
    }
    risk
}

/// Text of a node with one level of quotes removed.
fn unquote(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && ((t.starts_with('\'') && t.ends_with('\'')) || (t.starts_with('"') && t.ends_with('"'))) {
        return t[1..t.len() - 1].to_string();
    }
    t.replace('\\', "")
}

fn command_words(n: tree_sitter::Node, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = n.walk();
    for ch in n.children(&mut cur) {
        match ch.kind() {
            "command_name" | "word" | "string" | "raw_string" | "concatenation" | "number" | "simple_expansion" | "expansion" | "ansi_c_string" => {
                out.push(unquote(&src[ch.byte_range()]));
            }
            _ => {}
        }
    }
    out
}

fn classify_redirect(n: tree_sitter::Node, src: &str) -> Risk {
    let text = &src[n.byte_range()];
    let t = text.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    if t.starts_with('<') { return Risk::R0; }
    let target = unquote(t.trim_start_matches(['>', '|', '&']).trim());
    if target == "/dev/null" || target == "/dev/stdout" || target == "/dev/stderr" || target.starts_with("&") || target.chars().all(|c| c.is_ascii_digit()) { return Risk::R0; }
    if target.starts_with("/dev/sd") || target.starts_with("/dev/nvme") || target.starts_with("/dev/disk") { return Risk::Hard; }
    if ["/etc/", "/boot/", "/usr/", "/bin/", "/sbin/", "/lib", "/var/lib/", "/sys/", "/proc/"].iter().any(|p| target.starts_with(p))
        || target.contains(".ssh/") || target.ends_with(".bashrc") || target.ends_with(".zshrc") || target.ends_with(".profile") {
        return Risk::R3;
    }
    Risk::R1
}

const WRAPPERS: &[&str] = &["nohup", "time", "nice", "ionice", "stdbuf", "timeout", "command", "builtin", "exec", "env", "caffeinate", "chronic"];

fn is_root_path(a: &str) -> bool {
    matches!(a, "/" | "/*" | "~" | "~/" | "~/*" | "$HOME" | "$HOME/" | "${HOME}" | "." | "./*" | "*" | "/." | "..")
}

fn classify_words(words: &[String], depth: u8) -> Risk {
    let Some(first_raw) = words.first() else { return Risk::R0 };
    // leading VAR=value assignments are separate nodes, so words[0] is the program
    let first = first_raw.rsplit('/').next().unwrap_or(first_raw).to_string();
    let args = &words[1..];
    let joined = words.join(" ");
    let lower = joined.to_ascii_lowercase();
    let rest_after_opts = |skip_val: &[&str]| -> Vec<String> {
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--" { i += 1; break; }
            if a.starts_with('-') { if skip_val.contains(&a.as_str()) { i += 1; } i += 1; continue; }
            if first == "env" && a.contains('=') { i += 1; continue; }
            if first == "timeout" && a.chars().next().is_some_and(|c| c.is_ascii_digit()) { i += 1; break; }
            break;
        }
        args[i.min(args.len())..].to_vec()
    };
    match first.as_str() {
        "sudo" | "doas" | "su" | "pkexec" => {
            if first == "su" { return Risk::R3; }
            let inner = rest_after_opts(&["-u", "-g", "-C", "-p", "-h", "-U"]);
            if inner.is_empty() { return Risk::R3; }
            let r = classify_words(&inner, depth);
            // privilege escalation: at least R2 and one step higher than the inner command
            return match r { Risk::R0 | Risk::R1 => Risk::R2, Risk::R2 => Risk::R3, Risk::R3 => Risk::R4, x => x };
        }
        w if WRAPPERS.contains(&w) => {
            let inner = rest_after_opts(&["-n", "-c", "-s", "-k", "-u", "-o", "-e", "-i", "-S"]);
            return if inner.is_empty() { Risk::R0 } else { classify_words(&inner, depth) };
        }
        "xargs" | "parallel" => {
            let inner = rest_after_opts(&["-I", "-n", "-P", "-d", "-L", "-s", "-E", "-a", "-j"]);
            let r = if inner.is_empty() { Risk::R0 } else { classify_words(&inner, depth) };
            return r.max(Risk::R1);
        }
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "fish" => {
            if let Some(i) = args.iter().position(|a| a == "-c" || a == "-lc" || a == "-ec") {
                return args.get(i + 1).map(|s| classify_depth(s, depth + 1)).unwrap_or(Risk::R2).max(Risk::R1);
            }
            return Risk::R2;
        }
        "eval" => return classify_depth(&args.join(" "), depth + 1).max(Risk::R2),
        "find" | "fd" => {
            let mut r = Risk::R0;
            let mut i = 0;
            while i < args.len() {
                let a = args[i].as_str();
                if matches!(a, "-exec" | "-execdir" | "-ok" | "-okdir" | "-x" | "--exec" | "-X" | "--exec-batch") {
                    let mut j = i + 1;
                    let mut sub = Vec::new();
                    while j < args.len() && args[j] != ";" && args[j] != "+" && args[j] != "\\;" { sub.push(args[j].clone()); j += 1; }
                    r = r.max(classify_words(&sub, depth).max(Risk::R1));
                    i = j;
                } else if a == "-delete" { r = r.max(Risk::R3); }
                else if a == "-fprint" || a == "-fprintf" || a == "-fls" { r = r.max(Risk::R1); }
                i += 1;
            }
            return r;
        }
        _ => {}
    }
    // hard blocks
    if first == "rm" {
        let flags: String = args.iter().filter(|a| a.starts_with('-') && !a.starts_with("--")).map(|a| a.trim_start_matches('-')).collect();
        let recursive = flags.contains('r') || flags.contains('R') || args.iter().any(|a| a == "--recursive");
        let force = flags.contains('f') || args.iter().any(|a| a == "--force");
        let targets: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
        if recursive && targets.iter().any(|t| is_root_path(t) || matches!(t.as_str(), "/etc" | "/usr" | "/var" | "/home" | "/boot" | "/bin" | "/lib")) { return Risk::Hard; }
        if args.iter().any(|a| a == "--no-preserve-root") { return Risk::Hard; }
        if recursive || force { return Risk::R4; }
        return Risk::R3;
    }
    if first.starts_with("mkfs") || first == "wipefs" || first == "fdisk" || first == "parted" || first == "sfdisk" { return Risk::Hard; }
    if first == "dd" && args.iter().any(|a| a.starts_with("of=/dev/")) { return Risk::Hard; }
    if first == "dd" { return Risk::R3; }
    if first.contains("(){") || joined.contains(":(){") { return Risk::Hard; }
    if (first == "chmod" || first == "chown") && args.iter().any(|a| a == "-R" || a == "--recursive") && args.iter().any(|a| a == "/") { return Risk::Hard; }
    if first == "shred" { return Risk::R4; }
    // R4
    if first == "git" && args.first().is_some_and(|a| a == "push") && args.iter().any(|a| a == "--force" || a == "-f" || a.starts_with("--force-with-lease") || a.starts_with('+')) { return Risk::R4; }
    if first == "git" && args.first().is_some_and(|a| a == "reset") && args.iter().any(|a| a == "--hard") { return Risk::R3; }
    if first == "git" && args.first().is_some_and(|a| a == "clean") { return Risk::R3; }
    if lower.contains("drop table") || lower.contains("drop database") || lower.contains("truncate table") { return Risk::R4; }
    if matches!(first.as_str(), "shutdown" | "reboot" | "halt" | "poweroff" | "init" | "telinit") { return Risk::R4; }
    if first == "kubectl" && args.first().is_some_and(|a| a == "delete") { return Risk::R4; }
    if first == "terraform" && args.first().is_some_and(|a| a == "destroy") { return Risk::R4; }
    // R3
    if matches!(first.as_str(), "chmod" | "chown" | "chgrp" | "kill" | "pkill" | "killall" | "iptables" | "nft" | "ufw" | "userdel" | "useradd" | "usermod" | "passwd" | "crontab" | "visudo" | "mount" | "umount" | "swapoff" | "sysctl" | "unlink" | "rmdir" | "truncate") { return Risk::R3; }
    if first == "systemctl" && args.iter().any(|a| matches!(a.as_str(), "stop" | "disable" | "mask" | "restart" | "kill")) { return Risk::R3; }
    if first == "docker" && args.iter().any(|a| matches!(a.as_str(), "rm" | "rmi" | "prune" | "kill")) { return Risk::R3; }
    if (first == "curl" || first == "wget") && args.iter().any(|a| a == "-X" || a == "--request" || a == "-d" || a == "--data" || a == "-T" || a == "--upload-file" || a == "-F") { return Risk::R2; }
    if first == "sed" && args.iter().any(|a| a == "-i" || a.starts_with("-i") || a == "--in-place") { return Risk::R1; }
    // R0 read-only
    let ro = ["ls", "cat", "less", "more", "head", "tail", "grep", "egrep", "fgrep", "rg", "ag", "pwd", "whoami", "id", "df", "du", "ps", "top", "htop", "uptime", "echo", "printf", "which", "type", "stat", "wc", "file", "date", "hostname", "uname", "free", "lsblk", "lscpu", "tree", "sort", "uniq", "cut", "tr", "awk", "jq", "yq", "diff", "cmp", "md5sum", "sha256sum", "basename", "dirname", "realpath", "readlink", "history", "true", "false", "test", "[", "netstat", "ss", "ip", "ifconfig", "ping", "dig", "nslookup", "host", "lsof", "vmstat", "iostat", "w", "who", "last", "groups", "nproc", "seq", "sleep", "cd", "man", "help", "column", "nl", "fold", "tac", "rev", "strings", "hexdump", "xxd", "od", "zcat", "bat"];
    if ro.contains(&first.as_str()) {
        if first == "awk" && joined.contains("system(") { return Risk::R2; }
        return Risk::R0;
    }
    if first == "sed" { return Risk::R0; }
    if (first == "curl" || first == "wget") && !args.iter().any(|a| a == "-o" || a == "-O" || a == "--output") { return Risk::R0; }
    if first == "git" && args.first().is_some_and(|a| matches!(a.as_str(), "status" | "log" | "diff" | "show" | "branch" | "remote" | "fetch" | "blame" | "rev-parse" | "describe" | "ls-files" | "grep")) { return Risk::R0; }
    if first == "systemctl" && args.first().is_some_and(|a| matches!(a.as_str(), "status" | "is-active" | "list-units" | "show" | "cat")) { return Risk::R0; }
    if first == "journalctl" || first == "dmesg" { return Risk::R0; }
    if first == "docker" && args.first().is_some_and(|a| matches!(a.as_str(), "ps" | "images" | "logs" | "inspect" | "version" | "info")) { return Risk::R0; }
    if first == "kubectl" && args.first().is_some_and(|a| matches!(a.as_str(), "get" | "describe" | "logs" | "version" | "top" | "explain")) { return Risk::R0; }
    // R1 local reversible writes
    if matches!(first.as_str(), "touch" | "mkdir" | "cp" | "mv" | "ln" | "vim" | "vi" | "nano" | "emacs" | "code" | "tee" | "tar" | "zip" | "unzip" | "gzip" | "gunzip" | "sed" | "curl" | "wget") { return Risk::R1; }
    if first == "git" && args.first().is_some_and(|a| matches!(a.as_str(), "add" | "commit" | "checkout" | "switch" | "stash" | "pull" | "merge" | "rebase" | "tag" | "restore" | "clone" | "init")) { return Risk::R1; }
    if first == "git" && args.first().is_some_and(|a| a == "push") { return Risk::R2; }
    Risk::R2
}

/// Final decision for a proposed command (used by the rule tester in Settings; the AI/agent
/// bridge will call the same function once it exists).
pub fn decide(mode: Mode, max_auto: Risk, rules: &[Rule], principal: &str, action: &str, command: &str, dangerous_confirm: bool) -> (Effect, String) {
    decide_ctx(mode, max_auto, rules, principal, action, command, dangerous_confirm, false)
}

/// Like [`decide`], with the production-host flag. On a production host (when
/// `permissions.prod_confirm` is on, the caller passes `prod = true`): Auto mode never auto-allows
/// anything above R0, and an Allow rule for an R2+ command becomes Ask.
#[allow(clippy::too_many_arguments)]
pub fn decide_ctx(mode: Mode, max_auto: Risk, rules: &[Rule], principal: &str, action: &str, command: &str, dangerous_confirm: bool, prod: bool) -> (Effect, String) {
    decide_host(mode, max_auto, rules, principal, action, command, dangerous_confirm, prod, "")
}

/// [`decide_ctx`] plus the host name, for organization host rules. The managed policy
/// (`/etc/shellax/managed-policy.yaml`) is applied first and cannot be loosened by user rules.
#[allow(clippy::too_many_arguments)]
pub fn decide_host(mode: Mode, max_auto: Risk, rules: &[Rule], principal: &str, action: &str, command: &str, dangerous_confirm: bool, prod: bool, host: &str) -> (Effect, String) {
    let Some(m) = crate::managed::get() else { return decide_user(mode, max_auto, rules, principal, action, command, dangerous_confirm, prod) };
    if let Some(p) = m.hard_deny_hit(command) { return (Effect::Deny, tf("policy.org_hard_deny", &[("p", &p)])); }
    let risk = classify(command);
    if let Some(min) = m.host_deny_from(host, prod) { if risk >= min && risk != Risk::Hard { return (Effect::Deny, tf("policy.org_host_deny", &[("risk", &risk.label()), ("min", &min.label())])); } }
    let eff_mode = m.clamp_mode(mode);
    let (e, why) = decide_user(eff_mode, max_auto, rules, principal, action, command, dangerous_confirm, prod);
    if eff_mode != mode { return (e, format!("{why} · {}", tf("policy.org_mode", &[("m", &eff_mode.label())]))); }
    (e, why)
}

#[allow(clippy::too_many_arguments)]
fn decide_user(mode: Mode, max_auto: Risk, rules: &[Rule], principal: &str, action: &str, command: &str, dangerous_confirm: bool, prod: bool) -> (Effect, String) {
    let risk = classify(command);
    if risk == Risk::Hard { return (Effect::Deny, tf("policy.hard", &[("risk", &risk.label())])); }
    if mode == Mode::Plan && risk > Risk::R0 { return (Effect::Deny, tf("policy.plan_deny", &[("risk", &risk.label())])); }
    if let Some((i, r)) = evaluate(rules, principal, action, command) {
        if r.effect == Effect::Allow && prod && risk >= Risk::R2 {
            return (Effect::Ask, tf("policy.prod_rule_ask", &[("n", &(i + 1)), ("id", &r.id), ("risk", &risk.label())]));
        }
        if r.effect == Effect::Allow && dangerous_confirm && risk >= Risk::R4 {
            return (Effect::Ask, tf("policy.rule_but_dangerous", &[("n", &(i + 1)), ("id", &r.id), ("risk", &risk.label())]));
        }
        return (r.effect, tf("policy.rule_hit", &[("n", &(i + 1)), ("id", &r.id), ("risk", &risk.label())]));
    }
    match mode {
        Mode::Auto if prod && risk > Risk::R0 => (Effect::Ask, tf("policy.prod_ask", &[("risk", &risk.label())])),
        Mode::Auto if risk <= max_auto => (Effect::Allow, tf("policy.auto_allow", &[("risk", &risk.label()), ("max", &max_auto.label())])),
        _ if risk == Risk::R0 => (Effect::Allow, t("ui.no_matching_rule_r0_read").into()),
        _ => (Effect::Ask, tf("policy.no_rule_ask", &[("mode", &mode.label()), ("risk", &risk.label())])),
    }
}

impl std::str::FromStr for Risk {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s { "R0" => Risk::R0, "R1" => Risk::R1, "R2" => Risk::R2, "R3" => Risk::R3, "R4" => Risk::R4, _ => return Err(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn r(id: &str, cmd: &str, e: Effect) -> Rule { Rule { id: id.into(), principal: "*".into(), action: "exec".into(), command: cmd.into(), effect: e, reason: String::new() } }
    #[test]
    fn last_match_wins_and_glob() {
        let rules = vec![r("a", "*", Effect::Ask), r("b", "git *", Effect::Allow), r("c", "git push*", Effect::Deny)];
        assert_eq!(evaluate(&rules, "claude", "exec", "git status").unwrap().1.id, "b");
        assert_eq!(evaluate(&rules, "claude", "exec", "git push origin main").unwrap().1.id, "c");
        assert_eq!(evaluate(&rules, "ai", "exec", "ls").unwrap().1.id, "a");
        assert!(evaluate(&rules, "ai", "read", "ls").is_none());
        assert!(glob("a?c*", "abcdef") && !glob("a?c", "abcd"));
    }
    #[test]
    fn decisions() {
        let rules = vec![r("git", "git *", Effect::Allow)];
        assert_eq!(decide(Mode::Ask, Risk::R1, &rules, "ai", "exec", "git status", true).0, Effect::Allow);
        assert_eq!(decide(Mode::Plan, Risk::R1, &rules, "ai", "exec", "git commit -m x", true).0, Effect::Deny);
        assert_eq!(decide(Mode::Ask, Risk::R1, &rules, "ai", "exec", "git push --force", true).0, Effect::Ask);
        assert_eq!(decide(Mode::Auto, Risk::R1, &[], "ai", "exec", "touch a", true).0, Effect::Allow);
        assert_eq!(decide(Mode::Auto, Risk::R1, &[], "ai", "exec", "npm install", true).0, Effect::Ask);
        assert_eq!(decide(Mode::Auto, Risk::R4, &rules, "ai", "exec", "rm -rf /", false).0, Effect::Deny);
    }
    #[test]
    fn segmented_risk() {
        assert_eq!(classify("find . -name '*.log' -exec rm -rf {} +"), Risk::R4);
        assert_eq!(classify("find . -name x -delete"), Risk::R3);
        assert_eq!(classify("find . -name x"), Risk::R0);
        assert_eq!(classify("ls | xargs rm"), Risk::R3);
        assert_eq!(classify("cat a | grep b | wc -l"), Risk::R0);
        assert_eq!(classify("echo hi > /etc/hosts"), Risk::R3);
        assert_eq!(classify("echo hi > out.txt"), Risk::R1);
        assert_eq!(classify("ls 2>/dev/null"), Risk::R0);
        assert_eq!(classify("sudo ls"), Risk::R2);
        assert_eq!(classify("sudo systemctl restart nginx"), Risk::R4);
        assert_eq!(classify("ls && rm -rf build"), Risk::R4);
        assert_eq!(classify("echo $(rm -rf ~)"), Risk::Hard);
        assert_eq!(classify("bash -c 'rm -rf /'"), Risk::Hard);
        assert_eq!(classify("nohup env A=1 rm x"), Risk::R3);
        assert_eq!(classify("echo 'rm -rf /'"), Risk::R0);
        assert_eq!(classify("dd if=/dev/zero of=/dev/sda"), Risk::Hard);
        assert_eq!(classify("cat x > /dev/sda"), Risk::Hard);
        assert_eq!(classify("git push origin +main"), Risk::R4);
        assert_eq!(classify("(cd /tmp; rm a)"), Risk::R3);
    }
    #[test]
    fn production_is_stricter() {
        let rules = vec![r("npm", "npm *", Effect::Allow)];
        assert_eq!(decide_ctx(Mode::Auto, Risk::R2, &[], "ai", "exec", "touch a", true, false).0, Effect::Allow);
        assert_eq!(decide_ctx(Mode::Auto, Risk::R2, &[], "ai", "exec", "touch a", true, true).0, Effect::Ask);
        assert_eq!(decide_ctx(Mode::Auto, Risk::R2, &[], "ai", "exec", "ls", true, true).0, Effect::Allow);
        assert_eq!(decide_ctx(Mode::Ask, Risk::R1, &rules, "ai", "exec", "npm install", true, false).0, Effect::Allow);
        assert_eq!(decide_ctx(Mode::Ask, Risk::R1, &rules, "ai", "exec", "npm install", true, true).0, Effect::Ask);
    }
    #[test]
    fn risk_levels() {
        assert_eq!(classify("ls -la"), Risk::R0);
        assert_eq!(classify("git commit -m x"), Risk::R1);
        assert_eq!(classify("npm install"), Risk::R2);
        assert_eq!(classify("rm foo.txt"), Risk::R3);
        assert_eq!(classify("rm -rf build"), Risk::R4);
        assert_eq!(classify("sudo rm -rf /"), Risk::Hard);
        assert_eq!(classify("rm -rf /tmp/x"), Risk::R4);
    }
}

/// PRD 6.x `write_to_pty`: built-in AI (or another agent) typing into a pane where an external
/// agent is running. Modes: `ask` (always ask), `ask_first` (ask once per pane, then allow),
/// `allow`, `deny`. Panes without an agent are ordinary shells and are not gated here.
pub fn write_to_pty(mode: &str, agent_running: bool, approved_before: bool) -> Effect {
    if !agent_running { return Effect::Allow; }
    match mode {
        "allow" | "always_allow" => Effect::Allow,
        "deny" => Effect::Deny,
        "ask_first" | "ask_on_first_write" => if approved_before { Effect::Allow } else { Effect::Ask },
        _ => Effect::Ask,
    }
}

#[cfg(test)]
mod pty_tests {
    use super::*;
    #[test]
    fn write_to_pty_modes() {
        assert_eq!(write_to_pty("ask", false, false), Effect::Allow);
        assert_eq!(write_to_pty("ask", true, true), Effect::Ask);
        assert_eq!(write_to_pty("ask_first", true, false), Effect::Ask);
        assert_eq!(write_to_pty("ask_first", true, true), Effect::Allow);
        assert_eq!(write_to_pty("allow", true, false), Effect::Allow);
        assert_eq!(write_to_pty("deny", true, true), Effect::Deny);
        assert_eq!(write_to_pty("bogus", true, false), Effect::Ask);
    }
}
