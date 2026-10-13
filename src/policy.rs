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

/// Heuristic command risk classification (conservative; unknown commands are R2).
pub fn classify(cmd: &str) -> Risk {
    let c = cmd.trim();
    let hard = ["rm -rf /*", "rm -rf / ", "rm -rf ~ ", "mkfs", "dd if=", ":(){", "> /dev/sd", "chmod -R 777 / "];
    let padded = format!("{c} ");
    if hard.iter().any(|h| padded.contains(h)) && !c.starts_with("echo ") { return Risk::Hard; }
    let first = c.split_whitespace().next().unwrap_or("");
    let first = first.rsplit('/').next().unwrap_or(first);
    if c.contains("rm -rf") || c.contains("rm -fr") || c.contains("push --force") || c.contains("push -f")
        || c.to_ascii_lowercase().contains("drop table") || c.to_ascii_lowercase().contains("drop database")
        || matches!(first, "shutdown" | "reboot" | "halt" | "poweroff") { return Risk::R4; }
    if matches!(first, "rm" | "chmod" | "chown" | "kill" | "pkill" | "killall" | "iptables" | "userdel" | "crontab") || c.contains(" > /etc/") { return Risk::R3; }
    if matches!(first, "ls" | "cat" | "less" | "head" | "tail" | "grep" | "rg" | "find" | "pwd" | "whoami" | "df" | "du" | "ps" | "top" | "htop" | "uptime" | "echo" | "which" | "env" | "stat" | "wc")
        || c.starts_with("git status") || c.starts_with("git log") || c.starts_with("git diff") || c.starts_with("systemctl status") || c.starts_with("journalctl") { return Risk::R0; }
    if matches!(first, "touch" | "mkdir" | "cp" | "mv" | "vim" | "nano" | "sed") || c.starts_with("git commit") || c.starts_with("git add") { return Risk::R1; }
    Risk::R2
}

/// Final decision for a proposed command (used by the rule tester in Settings; the AI/agent
/// bridge will call the same function once it exists).
pub fn decide(mode: Mode, max_auto: Risk, rules: &[Rule], principal: &str, action: &str, command: &str, dangerous_confirm: bool) -> (Effect, String) {
    let risk = classify(command);
    if risk == Risk::Hard { return (Effect::Deny, tf("policy.hard", &[("risk", &risk.label())])); }
    if mode == Mode::Plan && risk > Risk::R0 { return (Effect::Deny, tf("policy.plan_deny", &[("risk", &risk.label())])); }
    if let Some((i, r)) = evaluate(rules, principal, action, command) {
        if r.effect == Effect::Allow && dangerous_confirm && risk >= Risk::R4 {
            return (Effect::Ask, tf("policy.rule_but_dangerous", &[("n", &(i + 1)), ("id", &r.id), ("risk", &risk.label())]));
        }
        return (r.effect, tf("policy.rule_hit", &[("n", &(i + 1)), ("id", &r.id), ("risk", &risk.label())]));
    }
    match mode {
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
