//! Local terminal prompt theme detection and built-in zero-dependency prompt presets.
//!
//! When an older Windows host or fresh macOS machine has no prompt framework configured
//! (Starship, Oh My Posh, Oh My Zsh, Powerlevel10k, custom PS1/PROMPT), ShellAX offers
//! 4 ANSI-16-color presets injected at session level without editing user dotfiles.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

const MAX_SCAN_BYTES: usize = 64 * 1024;
const CMD_PREFIX: &str = "$E]9;9;$P$E\\";

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptStyle {
    #[default]
    System = 0,
    Pure = 1,
    Capsule = 2,
    DevOps = 3,
    Classic = 4,
}

impl PromptStyle {
    pub const ALL: [PromptStyle; 5] = [
        Self::System,
        Self::Pure,
        Self::Capsule,
        Self::DevOps,
        Self::Classic,
    ];

    pub const PRESETS: [PromptStyle; 4] = [
        Self::Pure,
        Self::Capsule,
        Self::DevOps,
        Self::Classic,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Pure => "pure",
            Self::Capsule => "capsule",
            Self::DevOps => "devops",
            Self::Classic => "classic",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "pure" => Self::Pure,
            "capsule" => Self::Capsule,
            "devops" => Self::DevOps,
            "classic" => Self::Classic,
            _ => Self::System,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Pure,
            2 => Self::Capsule,
            3 => Self::DevOps,
            4 => Self::Classic,
            _ => Self::System,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
    Pwsh,
    PowerShell,
    Cmd,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptCapability {
    Full,
    StaticAnsi,
    Unsupported,
}

impl ShellKind {
    pub fn from_path(shell_path: &str) -> Self {
        let base = shell_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let stem = base.strip_suffix(".exe").unwrap_or(&base);
        match stem {
            "zsh" => Self::Zsh,
            "bash" => Self::Bash,
            "pwsh" => Self::Pwsh,
            "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            _ => Self::Other,
        }
    }

    pub fn capability(self) -> PromptCapability {
        match self {
            Self::Zsh | Self::Bash | Self::Pwsh | Self::PowerShell => PromptCapability::Full,
            Self::Cmd => PromptCapability::StaticAnsi,
            Self::Other => PromptCapability::Unsupported,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPrompt {
    pub name: String,
    pub source_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptDetection {
    pub shell: ShellKind,
    pub capability: PromptCapability,
    pub detected: Option<DetectedPrompt>,
}

/// Configured prompt style (synced from `terminal.prompt_style`).
pub static STYLE: AtomicU8 = AtomicU8::new(PromptStyle::System as u8);
/// Whether built-in style overrides an already-configured user prompt theme.
pub static OVERRIDE_EXISTING: AtomicBool = AtomicBool::new(false);

pub fn set_runtime(style: PromptStyle, override_existing: bool) {
    STYLE.store(style as u8, Ordering::Relaxed);
    OVERRIDE_EXISTING.store(override_existing, Ordering::Relaxed);
}

pub fn current_style() -> PromptStyle {
    PromptStyle::from_u8(STYLE.load(Ordering::Relaxed))
}

pub fn override_existing() -> bool {
    OVERRIDE_EXISTING.load(Ordering::Relaxed)
}

/// Effective style for spawning `shell_path`: yields to any detected user theme unless overridden.
pub fn resolve_effective_style(shell_path: &str) -> PromptStyle {
    let configured = current_style();
    if configured == PromptStyle::System {
        return PromptStyle::System;
    }
    let det = detect_for_shell(shell_path);
    if det.capability == PromptCapability::Unsupported {
        return PromptStyle::System;
    }
    if det.detected.is_some() && !override_existing() {
        return PromptStyle::System;
    }
    configured
}

pub fn detect_default_shell(configured_shell: &str) -> PromptDetection {
    let sh = if configured_shell.trim().is_empty() {
        crate::local::default_shell_path()
    } else {
        configured_shell.trim().to_string()
    };
    detect_for_shell(&sh)
}

pub fn detect_for_shell(shell_path: &str) -> PromptDetection {
    let shell = ShellKind::from_path(shell_path);
    let docs = windows_docs_dirs();
    detect_with_env(shell, |k| std::env::var(k).ok(), &docs)
}

/// Testable detection entry point that avoids process-global env mutations.
fn detect_with_env(
    shell: ShellKind,
    env: impl Fn(&str) -> Option<String>,
    docs_dirs: &[PathBuf],
) -> PromptDetection {
    let capability = shell.capability();
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from);

    let detected = match shell {
        ShellKind::Zsh => detect_zsh(&env, home.as_deref()),
        ShellKind::Bash => detect_bash(home.as_deref()),
        ShellKind::Pwsh => detect_powershell(true, home.as_deref(), docs_dirs),
        ShellKind::PowerShell => detect_powershell(false, home.as_deref(), docs_dirs),
        ShellKind::Cmd => detect_cmd(&env),
        ShellKind::Other => None,
    };

    PromptDetection {
        shell,
        capability,
        detected,
    }
}

fn read_capped(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; MAX_SCAN_BYTES];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn active_lines(content: &str, cmd_comments: bool) -> String {
    let mut out = String::with_capacity(content.len());
    for line in content.lines() {
        let t = line.trim_start();
        if t.is_empty() {
            continue;
        }
        if cmd_comments {
            let low = t.to_ascii_lowercase();
            if t.starts_with("::") || low == "rem" || low.starts_with("rem ") {
                continue;
            }
        } else if t.starts_with('#') {
            continue;
        }
        out.push_str(t);
        out.push('\n');
    }
    out
}

fn detect_zsh(env: &impl Fn(&str) -> Option<String>, home: Option<&Path>) -> Option<DetectedPrompt> {
    let zdotdir = env("SHELLAX_ORIG_ZDOTDIR")
        .or_else(|| env("ZDOTDIR"))
        .filter(|s| !s.trim().is_empty() && !s.contains("shellax-zsh-"))
        .map(PathBuf::from);
    let base = zdotdir.as_deref().or(home)?;
    let zshrc = base.join(".zshrc");
    let raw = read_capped(&zshrc)?;
    let active = active_lines(&raw, false);
    match_zsh_theme(&active).map(|name| DetectedPrompt {
        name: name.into(),
        source_file: zshrc,
    })
}

fn match_zsh_theme(active: &str) -> Option<&'static str> {
    let low = active.to_ascii_lowercase();
    if low.contains("starship init") {
        return Some("Starship");
    }
    if low.contains("oh-my-posh") {
        return Some("Oh My Posh");
    }
    if low.contains("powerlevel10k") || low.contains("p10k.zsh") {
        return Some("Powerlevel10k");
    }
    if low.contains("oh-my-zsh") || has_nonempty_var(&low, "zsh_theme=") {
        return Some("Oh My Zsh");
    }
    for kw in ["zinit", "zimfw", "antidote", "sheldon", "prezto", "zplug"] {
        if has_token(&low, kw) {
            return Some("Zsh Framework");
        }
    }
    if low.contains("prompt pure")
        || low.contains("spaceship")
        || low.contains("autoload -uz promptinit")
    {
        return Some("Zsh Prompt");
    }
    if has_assignment(&low, "prompt=")
        || has_assignment(&low, "ps1=")
        || has_assignment(&low, "rprompt=")
    {
        return Some("Custom PROMPT");
    }
    None
}

fn detect_bash(home: Option<&Path>) -> Option<DetectedPrompt> {
    let home = home?;
    for name in [".bashrc", ".bash_profile"] {
        let path = home.join(name);
        let Some(raw) = read_capped(&path) else {
            continue;
        };
        let active = active_lines(&raw, false);
        if let Some(theme) = match_bash_theme(&active) {
            return Some(DetectedPrompt {
                name: theme.into(),
                source_file: path,
            });
        }
    }
    None
}

fn match_bash_theme(active: &str) -> Option<&'static str> {
    let low = active.to_ascii_lowercase();
    if low.contains("starship init") {
        return Some("Starship");
    }
    if low.contains("oh-my-posh") {
        return Some("Oh My Posh");
    }
    if low.contains("oh-my-bash") || has_nonempty_var(&low, "osh_theme=") {
        return Some("Oh My Bash");
    }
    for kw in ["liquidprompt", "bash-it", "bash_it", "powerline"] {
        if low.contains(kw) {
            return Some("Bash Theme");
        }
    }
    for line in low.lines() {
        if line.contains("update_terminal_cwd") || line.contains("debian_chroot") {
            continue;
        }
        if has_assignment(line, "ps1=") {
            return Some("Custom PS1");
        }
    }
    None
}

fn detect_powershell(
    is_pwsh: bool,
    home: Option<&Path>,
    docs_dirs: &[PathBuf],
) -> Option<DetectedPrompt> {
    let folder = if is_pwsh {
        "PowerShell"
    } else {
        "WindowsPowerShell"
    };
    let mut candidates = Vec::new();
    for docs in docs_dirs {
        candidates.push(docs.join(folder).join("Microsoft.PowerShell_profile.ps1"));
        candidates.push(docs.join(folder).join("profile.ps1"));
    }
    if is_pwsh {
        if let Some(h) = home {
            candidates.push(h.join(".config/powershell/Microsoft.PowerShell_profile.ps1"));
            candidates.push(h.join(".config/powershell/profile.ps1"));
        }
    }
    for path in candidates {
        let Some(raw) = read_capped(&path) else {
            continue;
        };
        let active = active_lines(&raw, false);
        if let Some(theme) = match_powershell_theme(&active) {
            return Some(DetectedPrompt {
                name: theme.into(),
                source_file: path,
            });
        }
    }
    None
}

fn match_powershell_theme(active: &str) -> Option<&'static str> {
    let low = active.to_ascii_lowercase();
    if low.contains("oh-my-posh") {
        return Some("Oh My Posh");
    }
    if low.contains("starship") {
        return Some("Starship");
    }
    if low.contains("posh-git") {
        return Some("posh-git");
    }
    if low.contains("function prompt")
        || low.contains("function global:prompt")
        || low.contains("set-item function:prompt")
    {
        return Some("Custom Prompt");
    }
    None
}

fn detect_cmd(env: &impl Fn(&str) -> Option<String>) -> Option<DetectedPrompt> {
    #[cfg(windows)]
    if let Some(autorun) = crate::proxy::read_reg_sz(
        r"Software\Microsoft\Command Processor",
        "AutoRun",
    ) {
        if autorun.to_ascii_lowercase().contains("clink") {
            return Some(DetectedPrompt {
                name: "Clink".into(),
                source_file: PathBuf::from(r"HKCU\Software\Microsoft\Command Processor\AutoRun"),
            });
        }
    }
    if let Some(p) = env("PROMPT") {
        let trimmed = p.trim();
        let stripped = trimmed.strip_prefix(CMD_PREFIX).unwrap_or(trimmed).trim();
        if !stripped.is_empty() && !stripped.eq_ignore_ascii_case("$P$G") {
            return Some(DetectedPrompt {
                name: "Custom PROMPT".into(),
                source_file: PathBuf::from("PROMPT"),
            });
        }
    }
    None
}

fn has_nonempty_var(low: &str, prefix: &str) -> bool {
    for line in low.lines() {
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        if let Some(rest) = line.strip_prefix(prefix) {
            let v = rest.trim().trim_matches('"').trim_matches('\'').trim();
            if !v.is_empty() {
                return true;
            }
        }
    }
    false
}

fn has_assignment(low: &str, key_eq: &str) -> bool {
    for line in low.lines() {
        for seg in line.split([';', '&']) {
            let s = seg.trim_start();
            let s = s.strip_prefix("export ").unwrap_or(s).trim_start();
            if s.starts_with(key_eq) {
                return true;
            }
        }
    }
    false
}

fn has_token(low: &str, kw: &str) -> bool {
    low.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .any(|tok| tok == kw)
}

fn windows_docs_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut dirs = Vec::new();
        if let Some(reg) = crate::proxy::read_reg_sz(
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders",
            "Personal",
        ) {
            let expanded = expand_win_env(&reg);
            if !expanded.is_empty() {
                dirs.push(PathBuf::from(expanded));
            }
        }
        if let Some(up) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            for cand in [up.join("Documents"), up.join("OneDrive").join("Documents")] {
                if !dirs.contains(&cand) {
                    dirs.push(cand);
                }
            }
        }
        return dirs;
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
fn expand_win_env(raw: &str) -> String {
    let mut out = raw.to_string();
    for var in ["USERPROFILE", "OneDrive", "OneDriveConsumer", "HOME"] {
        let pat = format!("%{var}%");
        if out.to_ascii_lowercase().contains(&pat.to_ascii_lowercase()) {
            if let Ok(val) = std::env::var(var) {
                if let Some(idx) = out.to_ascii_lowercase().find(&pat.to_ascii_lowercase()) {
                    out.replace_range(idx..idx + pat.len(), &val);
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Cross-platform zero-dependency prompt script builders
// ---------------------------------------------------------------------------

const ZSH_GIT_FN: &str = r#"
__as_git_branch() {
  local d="$PWD" head line dir
  local -i n=0
  while [ -n "$d" ] && [ $n -lt 32 ]; do
    if [ -d "$d/.git" ]; then
      head="$d/.git/HEAD"
      break
    elif [ -f "$d/.git" ]; then
      IFS= read -r line < "$d/.git" 2>/dev/null || return
      case "$line" in
        gitdir:\ *)
          dir="${line#gitdir: }"
          case "$dir" in /*) ;; *) dir="$d/$dir" ;; esac
          head="$dir/HEAD"
          break
          ;;
      esac
    fi
    [ "$d" = "/" ] && break
    d="${d%/*}"
    [ -z "$d" ] && d="/"
    n=$((n + 1))
  done
  [ -n "$head" ] && [ -r "$head" ] || return
  IFS= read -r line < "$head" 2>/dev/null || return
  case "$line" in
    ref:\ refs/heads/*) print -r -- "${line#ref: refs/heads/}" ;;
    *) print -r -- "@${line[1,7]}" ;;
  esac
}
"#;

pub fn zsh_zshrc(integrate: bool, style_aliases: bool, style: PromptStyle, color_aliases: &str) -> String {
    if style == PromptStyle::System {
        let mut body = if integrate {
            crate::local::ZSH_ZSHRC.to_string()
        } else {
            "[ -f \"$HOME/.zshrc\" ] && . \"$HOME/.zshrc\"\n".into()
        };
        if style_aliases {
            body.push_str(color_aliases);
        }
        return body;
    }

    let mut s = String::from(
        r#"__as_orig="${SHELLAX_ORIG_ZDOTDIR:-$HOME}"
ZDOTDIR="$__as_orig"
[ -f "$__as_orig/.zshrc" ] && . "$__as_orig/.zshrc"
: ${HISTFILE:=$__as_orig/.zsh_history}
"#,
    );
    s.push_str(ZSH_GIT_FN);

    let osc_pre = if integrate {
        "  print -n \"\\e]133;D;$__ec\\a\"\n  print -n \"\\e]7;file://${HOST}${PWD}\\a\"\n  print -n '\\e]133;A\\a'\n"
    } else {
        ""
    };
    let osc_suf = if integrate { "%{\\e]133;B\\a%}" } else { "" };

    let body = match style {
        PromptStyle::Pure => format!(
            r#"__as_precmd() {{
  local __ec=$?
{osc_pre}  local __git="$(__as_git_branch)"
  local __sym='%F{{magenta}}❯%f'
  [ "$__ec" -ne 0 ] && __sym='%F{{red}}❯%f'
  local __g=''
  [ -n "$__git" ] && __g=" %F{{8}}$__git%f"
  PS1="%B%F{{blue}}%(4~|%-1~/…/%2~|%~)%f%b${{__g}} ${{__sym}} {osc_suf}"
}}
"#
        ),
        PromptStyle::Capsule => format!(
            r#"__as_precmd() {{
  local __ec=$?
{osc_pre}  local __git="$(__as_git_branch)"
  local __dir='%K{{blue}}%F{{15}} %(4~|%-1~/…/%2~|%~) %k%f'
  local __lead='%F{{blue}}%f'
  local __tail='%F{{blue}}%f'
  local __g=''
  if [ -n "$__git" ]; then
    __tail='%K{{cyan}}%F{{blue}}%f%F{{0}}  '"$__git"' %k%f%F{{cyan}}%f'
  fi
  local __err=''
  if [ "$__ec" -ne 0 ]; then
    __err=' %K{{red}}%F{{15}} ✘ '"$__ec"' %k%f%F{{red}}%f'
  fi
  PS1="${{__lead}}${{__dir}}${{__tail}}${{__err}} ❯ {osc_suf}"
}}
"#
        ),
        PromptStyle::DevOps => format!(
            r#"__as_precmd() {{
  local __ec=$?
{osc_pre}  local __git="$(__as_git_branch)"
  local __g=''
  [ -n "$__git" ] && __g=" %F{{8}}on%f %F{{green}} $__git%f"
  local __env="${{VIRTUAL_ENV:+${{VIRTUAL_ENV:t}}}}"
  [ -z "$__env" ] && __env="${{CONDA_DEFAULT_ENV}}"
  local __e=''
  [ -n "$__env" ] && __e=" %F{{8}}via%f %F{{yellow}}$__env%f"
  local __sym='%F{{green}}❯%f'
  [ "$__ec" -ne 0 ] && __sym='%F{{red}}❯%f'
  PS1="%B%F{{cyan}}%(4~|%-1~/…/%2~|%~)%f%b${{__g}}${{__e}} %F{{8}}%D{{%H:%M}}%f"$'\n'"${{__sym}} {osc_suf}"
}}
"#
        ),
        PromptStyle::Classic => format!(
            r#"__as_precmd() {{
  local __ec=$?
{osc_pre}  local __git="$(__as_git_branch)"
  local __g=''
  [ -n "$__git" ] && __g=" %F{{yellow}}($__git)%f"
  local __sym='%F{{cyan}}$%f'
  [ "$__ec" -ne 0 ] && __sym='%F{{red}}$%f'
  PS1="%F{{8}}[%f%B%F{{green}}%(4~|%-1~/…/%2~|%~)%f%b${{__g}}%F{{8}}]%f ${{__sym}} {osc_suf}"
}}
"#
        ),
        PromptStyle::System => unreachable!(),
    };
    s.push_str(&body);
    if integrate {
        s.push_str(
            "__as_preexec() { print -n '\\e]133;C\\a' }\nautoload -Uz add-zsh-hook\nadd-zsh-hook precmd __as_precmd\nadd-zsh-hook preexec __as_preexec\n",
        );
    } else {
        s.push_str("autoload -Uz add-zsh-hook\nadd-zsh-hook precmd __as_precmd\n");
    }
    if style_aliases {
        s.push_str(color_aliases);
    }
    s
}

const BASH_HELPERS: &str = r#"
__as_short_pwd() {
  local p="$PWD"
  case "$p" in
    "$HOME") printf '%s' '~'; return ;;
    "$HOME"/*) p="~${p#$HOME}" ;;
  esac
  local IFS=/
  set -f
  set -- $p
  set +f
  if [ $# -gt 4 ]; then
    local first="$1"
    [ -z "$first" ] && first=""
    shift $(($# - 2))
    printf '%s/…/%s/%s' "$first" "$1" "$2"
  else
    printf '%s' "$p"
  fi
}
__as_git_branch() {
  local d="$PWD" head="" line="" dir="" n=0
  while [ -n "$d" ] && [ $n -lt 32 ]; do
    if [ -d "$d/.git" ]; then
      head="$d/.git/HEAD"
      break
    elif [ -f "$d/.git" ]; then
      IFS= read -r line < "$d/.git" 2>/dev/null || return
      case "$line" in
        gitdir:\ *)
          dir="${line#gitdir: }"
          case "$dir" in /*) ;; *) dir="$d/$dir" ;; esac
          head="$dir/HEAD"
          break
          ;;
      esac
    fi
    [ "$d" = "/" ] && break
    d="${d%/*}"
    [ -z "$d" ] && d="/"
    n=$((n + 1))
  done
  [ -n "$head" ] && [ -r "$head" ] || return
  IFS= read -r line < "$head" 2>/dev/null || return
  case "$line" in
    ref:\ refs/heads/*) printf '%s' "${line#ref: refs/heads/}" ;;
    *) printf '@%s' "${line:0:7}" ;;
  esac
}
"#;

pub fn bash_rc(integrate: bool, style_aliases: bool, style: PromptStyle, color_aliases: &str) -> String {
    if style == PromptStyle::System {
        let mut body = if integrate {
            crate::local::BASH_INTEGRATION.to_string()
        } else {
            "[ -f ~/.bashrc ] && . ~/.bashrc\n".into()
        };
        if style_aliases {
            body.push_str(color_aliases);
        }
        return body;
    }

    let mut s = String::from("[ -f ~/.bashrc ] && . ~/.bashrc\n");
    s.push_str(BASH_HELPERS);
    let osc_pre = if integrate {
        r#"\[\e]133;D;${__ec}\a\e]7;file://${HOSTNAME}${PWD}\a\e]133;A\a\]"#
    } else {
        ""
    };
    let osc_suf = if integrate { r#"\[\e]133;B\a\]"# } else { "" };

    let ps1_build = match style {
        PromptStyle::Pure => format!(
            r#"  local __sym='\[\e[35m\]❯\[\e[0m\]'
  [ "$__ec" -ne 0 ] && __sym='\[\e[31m\]❯\[\e[0m\]'
  local __g=""
  [ -n "$__git" ] && __g=" \[\e[90m\]${{__git}}\[\e[0m\]"
  PS1="{osc_pre}\[\e[1;34m\]${{__dir}}\[\e[0m\]${{__g}} ${{__sym}} {osc_suf}"
"#
        ),
        PromptStyle::Capsule => format!(
            r#"  local __lead='\[\e[34m\]\[\e[0m\]'
  local __seg='\[\e[44;97m\] '"$__dir"' \[\e[0m\]'
  local __tail='\[\e[34m\]\[\e[0m\]'
  if [ -n "$__git" ]; then
    __tail='\[\e[46;34m\]\[\e[30m\]  '"$__git"' \[\e[0;36m\]\[\e[0m\]'
  fi
  local __err=""
  [ "$__ec" -ne 0 ] && __err=' \[\e[41;97m\] ✘ '"$__ec"' \[\e[0;31m\]\[\e[0m\]'
  PS1="{osc_pre}${{__lead}}${{__seg}}${{__tail}}${{__err}} ❯ {osc_suf}"
"#
        ),
        PromptStyle::DevOps => format!(
            r#"  local __g=""
  [ -n "$__git" ] && __g=" \[\e[90m\]on\[\e[0m\] \[\e[32m\] ${{__git}}\[\e[0m\]"
  local __env="${{VIRTUAL_ENV##*/}}"
  [ -z "$__env" ] && __env="${{CONDA_DEFAULT_ENV}}"
  local __e=""
  [ -n "$__env" ] && __e=" \[\e[90m\]via\[\e[0m\] \[\e[33m\]${{__env}}\[\e[0m\]"
  local __sym='\[\e[32m\]❯\[\e[0m\]'
  [ "$__ec" -ne 0 ] && __sym='\[\e[31m\]❯\[\e[0m\]'
  PS1="{osc_pre}\[\e[1;36m\]${{__dir}}\[\e[0m\]${{__g}}${{__e}} \[\e[90m\]\A\[\e[0m\]\n${{__sym}} {osc_suf}"
"#
        ),
        PromptStyle::Classic => format!(
            r#"  local __g=""
  [ -n "$__git" ] && __g=" \[\e[33m\](${{__git}})\[\e[0m\]"
  local __sym='\[\e[36m\]$\[\e[0m\]'
  [ "$__ec" -ne 0 ] && __sym='\[\e[31m\]$\[\e[0m\]'
  PS1="{osc_pre}\[\e[90m\][\[\e[1;32m\]${{__dir}}\[\e[0m\]${{__g}}\[\e[90m\]]\[\e[0m\] ${{__sym}} {osc_suf}"
"#
        ),
        PromptStyle::System => unreachable!(),
    };

    s.push_str(&format!(
        r#"__as_prompt_cmd() {{
  local __ec=$?
  local __dir="$(__as_short_pwd)"
  local __git="$(__as_git_branch)"
{ps1_build}}}
PROMPT_COMMAND=__as_prompt_cmd
"#
    ));
    if integrate {
        s.push_str(
            "if [ -z \"${__as_ps0_done:-}\" ]; then PS0='\\e]133;C\\a'\"$PS0\"; __as_ps0_done=1; fi\n",
        );
    }
    if style_aliases {
        s.push_str(color_aliases);
    }
    s
}

/// PowerShell command string passed via `-Command`. Must never contain `"` so Windows command
/// line parsing cannot break it. All icons are built with `[char]` codepoints.
pub fn pwsh_script(integrate: bool, style: PromptStyle) -> Option<String> {
    if style == PromptStyle::System {
        return integrate.then(|| crate::local::PWSH_INTEGRATION.to_string());
    }

    let helpers = concat!(
        "function global:__as_dir($p){",
        "$h=$HOME; if($h -and $p.StartsWith($h,[System.StringComparison]::OrdinalIgnoreCase)){",
        "$p='~'+$p.Substring($h.Length)",
        "} ",
        "$a=$p -split '[\\\\/]'; ",
        "if($a.Count -gt 4){ $a[0]+'\\...\\'+$a[-2]+'\\'+$a[-1] }else{ $p }",
        "} ",
        "function global:__as_git($p){",
        "$d=$p; for($i=0;$i -lt 32 -and $d;$i++){",
        "$g=Join-Path $d '.git'; $hd=$null; ",
        "if([System.IO.Directory]::Exists($g)){ $hd=Join-Path $g 'HEAD' }",
        "elseif([System.IO.File]::Exists($g)){",
        "$l=([System.IO.File]::ReadAllText($g) -split '[\\r\\n]+')[0]; ",
        "if($l -match '^gitdir:\\s*(.+)$'){ $gd=$Matches[1].Trim(); ",
        "if(-not [System.IO.Path]::IsPathRooted($gd)){ $gd=Join-Path $d $gd } $hd=Join-Path $gd 'HEAD' }",
        "} ",
        "if($hd -and [System.IO.File]::Exists($hd)){",
        "$r=([System.IO.File]::ReadAllText($hd) -split '[\\r\\n]+')[0].Trim(); ",
        "if($r.StartsWith('ref: refs/heads/')){ return $r.Substring(16) } ",
        "if($r.Length -ge 7){ return '@'+$r.Substring(0,7) } ",
        "return $null",
        "} ",
        "$par=[System.IO.Path]::GetDirectoryName($d); if($par -eq $d){ break } $d=$par",
        "} ",
        "$null",
        "} ",
    );

    let osc_hdr = if integrate {
        concat!(
            "$code=if($ok){0}elseif($ec){$ec}else{1}; ",
            "$osc=$e+']133;D;'+$code+[char]7; ",
            "if($l.Provider.Name -eq 'FileSystem'){",
            "$osc+=($e+']9;9;'+$dp+$e+'\\')+($e+']7;file://localhost/'+($dp -replace '\\\\','/')+$e+'\\')",
            "} ",
            "$osc+=$e+']133;A'+[char]7; ",
            "$end=$e+']133;B'+[char]7; ",
        )
    } else {
        "$osc=''; $end=''; "
    };

    let body = match style {
        PromptStyle::Pure => concat!(
            "$g=if($dp){__as_git $dp}else{$null}; ",
            "$sc=if($ok){'35'}else{'31'}; ",
            "$gs=if($g){' '+$e+'[90m'+$g+$e+'[0m'}else{''}; ",
            "$out=$e+'[1;34m'+$sd+$e+'[0m'+$gs+' '+$e+'['+$sc+'m'+[char]0x276F+$e+'[0m '; ",
        ),
        PromptStyle::Capsule => concat!(
            "$g=if($dp){__as_git $dp}else{$null}; ",
            "$lead=$e+'[34m'+[char]0xE0B6+$e+'[0m'; ",
            "$seg=$e+'[44;97m '+$sd+' '+$e+'[0m'; ",
            "$tail=if($g){$e+'[46;34m'+[char]0xE0B0+$e+'[30m '+[char]0xE0A0+' '+$g+' '+$e+'[0;36m'+[char]0xE0B0+$e+'[0m'}else{$e+'[34m'+[char]0xE0B0+$e+'[0m'}; ",
            "$err=if(-not $ok){$c=if($ec){$ec}else{1}; ' '+$e+'[41;97m '+[char]0x2718+' '+$c+' '+$e+'[0;31m'+[char]0xE0B0+$e+'[0m'}else{''}; ",
            "$out=$lead+$seg+$tail+$err+' '+[char]0x276F+' '; ",
        ),
        PromptStyle::DevOps => concat!(
            "$g=if($dp){__as_git $dp}else{$null}; ",
            "$gs=if($g){' '+$e+'[90mon'+$e+'[0m '+$e+'[32m'+[char]0xE0A0+' '+$g+$e+'[0m'}else{''}; ",
            "$ve=$env:VIRTUAL_ENV; $ev=if($ve){Split-Path $ve -Leaf}elseif($env:CONDA_DEFAULT_ENV){$env:CONDA_DEFAULT_ENV}else{''}; ",
            "$es=if($ev){' '+$e+'[90mvia'+$e+'[0m '+$e+'[33m'+$ev+$e+'[0m'}else{''}; ",
            "$tm=(Get-Date -Format 'HH:mm'); ",
            "$sc=if($ok){'32'}else{'31'}; ",
            "$out=$e+'[1;36m'+$sd+$e+'[0m'+$gs+$es+' '+$e+'[90m'+$tm+$e+'[0m'+[Environment]::NewLine+$e+'['+$sc+'m'+[char]0x276F+$e+'[0m '; ",
        ),
        PromptStyle::Classic => concat!(
            "$g=if($dp){__as_git $dp}else{$null}; ",
            "$gs=if($g){' '+$e+'[33m('+$g+')'+$e+'[0m'}else{''}; ",
            "$sc=if($ok){'36'}else{'31'}; ",
            "$out=$e+'[90m['+$e+'[1;32m'+$sd+$e+'[0m'+$gs+$e+'[90m]'+$e+'[0m '+$e+'['+$sc+'m>'+$e+'[0m '; ",
        ),
        PromptStyle::System => unreachable!(),
    };

    let prompt_fn = format!(
        "{helpers}function global:prompt {{ $ok=$?; $ec=$global:LASTEXITCODE; $e=[char]27; $l=Get-Location; $dp=if($l.Provider.Name -eq 'FileSystem'){{$l.ProviderPath}}else{{''}}; $sd=if($dp){{__as_dir $dp}}else{{$l.Path}}; {osc_hdr}{body}$global:LASTEXITCODE=$ec; $osc+$out+$end }}"
    );
    Some(prompt_fn)
}

pub fn cmd_prompt(existing: Option<&str>, integrate: bool, style: PromptStyle) -> String {
    let prefix = if integrate { CMD_PREFIX } else { "" };
    let body = match style {
        PromptStyle::System => existing
            .filter(|s| !s.is_empty())
            .unwrap_or("$P$G")
            .to_string(),
        PromptStyle::Pure => "$E[1;34m$P$E[0m $E[35m>$E[0m ".into(),
        PromptStyle::Capsule => format!(
            "$E[34m\u{E0B6}$E[44;97m $P $E[0;34m\u{E0B0}$E[0m > "
        ),
        PromptStyle::DevOps => "$E[1;36m$P$E[0m $E[90m$T$H$H$H$H$H$H$E[0m$_$E[32m>$E[0m ".into(),
        PromptStyle::Classic => "$E[90m[$E[1;32m$P$E[0;90m]$E[0m $E[36m>$E[0m ".into(),
    };
    format!("{prefix}{body}")
}

// ---------------------------------------------------------------------------
// UI preview metadata (uses ANSI 16 palette indices 0..=15 so themes auto-adapt)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    DefaultFg,
    Palette(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSpan {
    pub text: &'static str,
    pub fg: AnsiColor,
    pub bg: Option<u8>,
    pub bold: bool,
}

impl PreviewSpan {
    const fn plain(text: &'static str) -> Self {
        Self {
            text,
            fg: AnsiColor::DefaultFg,
            bg: None,
            bold: false,
        }
    }
    const fn fg(text: &'static str, c: u8, bold: bool) -> Self {
        Self {
            text,
            fg: AnsiColor::Palette(c),
            bg: None,
            bold,
        }
    }
    const fn chip(text: &'static str, fg: u8, bg: u8) -> Self {
        Self {
            text,
            fg: AnsiColor::Palette(fg),
            bg: Some(bg),
            bold: false,
        }
    }
}

/// Returns up to 2 lines of preview spans for the prompt style + command sample.
pub fn preview_lines(style: PromptStyle, shell: ShellKind) -> Vec<Vec<PreviewSpan>> {
    let is_cmd = shell == ShellKind::Cmd;
    let is_win = matches!(shell, ShellKind::Pwsh | ShellKind::PowerShell | ShellKind::Cmd);
    let dir = if is_win {
        r"~\work\app"
    } else {
        "~/work/app"
    };
    let cmd = PreviewSpan::plain("git status");

    match (style, is_cmd) {
        (PromptStyle::System, true) => vec![vec![
            PreviewSpan::plain(r"C:\Users\dev\app> "),
            cmd,
        ]],
        (PromptStyle::System, false) => vec![vec![
            PreviewSpan::plain(if is_win {
                r"PS C:\Users\dev\app> "
            } else {
                "user@host app % "
            }),
            cmd,
        ]],
        (PromptStyle::Pure, true) => vec![vec![
            PreviewSpan::fg(dir, 4, true),
            PreviewSpan::plain(" "),
            PreviewSpan::fg(">", 5, false),
            PreviewSpan::plain(" "),
            cmd,
        ]],
        (PromptStyle::Pure, false) => vec![vec![
            PreviewSpan::fg(dir, 4, true),
            PreviewSpan::plain(" "),
            PreviewSpan::fg("main", 8, false),
            PreviewSpan::plain(" "),
            PreviewSpan::fg("❯", 5, false),
            PreviewSpan::plain(" "),
            cmd,
        ]],
        (PromptStyle::Capsule, true) => vec![vec![
            PreviewSpan::fg("\u{E0B6}", 4, false),
            PreviewSpan::chip(" ~\\work\\app ", 15, 4),
            PreviewSpan::fg("\u{E0B0}", 4, false),
            PreviewSpan::plain(" > "),
            cmd,
        ]],
        (PromptStyle::Capsule, false) => vec![vec![
            PreviewSpan::fg("\u{E0B6}", 4, false),
            PreviewSpan::chip(if is_win { " ~\\work\\app " } else { " ~/work/app " }, 15, 4),
            PreviewSpan::chip("\u{E0B0}", 4, 6),
            PreviewSpan::chip(" \u{E0A0} main ", 0, 6),
            PreviewSpan::fg("\u{E0B0}", 6, false),
            PreviewSpan::plain(" ❯ "),
            cmd,
        ]],
        (PromptStyle::DevOps, true) => vec![
            vec![
                PreviewSpan::fg(dir, 6, true),
                PreviewSpan::plain(" "),
                PreviewSpan::fg("14:30", 8, false),
            ],
            vec![PreviewSpan::fg(">", 2, false), PreviewSpan::plain(" "), cmd],
        ],
        (PromptStyle::DevOps, false) => vec![
            vec![
                PreviewSpan::fg(dir, 6, true),
                PreviewSpan::fg(" on ", 8, false),
                PreviewSpan::fg("\u{E0A0} main", 2, false),
                PreviewSpan::fg(" via ", 8, false),
                PreviewSpan::fg("venv", 3, false),
                PreviewSpan::fg(" 14:30", 8, false),
            ],
            vec![PreviewSpan::fg("❯", 2, false), PreviewSpan::plain(" "), cmd],
        ],
        (PromptStyle::Classic, true) => vec![vec![
            PreviewSpan::fg("[", 8, false),
            PreviewSpan::fg(dir, 2, true),
            PreviewSpan::fg("]", 8, false),
            PreviewSpan::plain(" "),
            PreviewSpan::fg(">", 6, false),
            PreviewSpan::plain(" "),
            cmd,
        ]],
        (PromptStyle::Classic, false) => vec![vec![
            PreviewSpan::fg("[", 8, false),
            PreviewSpan::fg(dir, 2, true),
            PreviewSpan::plain(" "),
            PreviewSpan::fg("(main)", 3, false),
            PreviewSpan::fg("]", 8, false),
            PreviewSpan::plain(" "),
            PreviewSpan::fg(if is_win { ">" } else { "$" }, 6, false),
            PreviewSpan::plain(" "),
            cmd,
        ]],
    }
}

/// Optional install commands for external prompt engines, filtered by package managers on PATH.
pub fn external_tool_commands() -> Vec<(&'static str, &'static str)> {
    let on_path = |bin: &str| {
        std::env::var_os("PATH").is_some_and(|p| {
            std::env::split_paths(&p).any(|d| d.join(bin).is_file())
        })
    };
    #[cfg(windows)]
    {
        if on_path("winget.exe") {
            return vec![
                ("Starship", "winget install --id Starship.Starship"),
                ("Oh My Posh", "winget install JanDeDobbeleer.OhMyPosh"),
            ];
        }
        if on_path("scoop.cmd") || on_path("scoop.ps1") {
            return vec![
                ("Starship", "scoop install starship"),
                ("Oh My Posh", "scoop install oh-my-posh"),
            ];
        }
        vec![
            ("Starship", "winget install --id Starship.Starship"),
            ("Oh My Posh", "winget install JanDeDobbeleer.OhMyPosh"),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        if on_path("brew") {
            return vec![
                ("Starship", "brew install starship"),
                ("Oh My Posh", "brew install jandedobbeleer/oh-my-posh/oh-my-posh"),
            ];
        }
        vec![
            ("Starship", "curl -sS https://starship.rs/install.sh | sh"),
            ("Oh My Posh", "brew install jandedobbeleer/oh-my-posh/oh-my-posh"),
        ]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = on_path;
        vec![
            ("Starship", "curl -sS https://starship.rs/install.sh | sh"),
            ("Oh My Posh", "curl -s https://ohmyposh.dev/install.sh | bash -s"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shellax-prompt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn parses_shell_kinds_and_capabilities() {
        assert_eq!(ShellKind::from_path("/bin/zsh"), ShellKind::Zsh);
        assert_eq!(ShellKind::from_path("/usr/bin/bash"), ShellKind::Bash);
        assert_eq!(
            ShellKind::from_path(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            ShellKind::Pwsh
        );
        assert_eq!(
            ShellKind::from_path(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            ShellKind::PowerShell
        );
        assert_eq!(ShellKind::from_path(r"C:\Windows\System32\cmd.exe"), ShellKind::Cmd);
        assert_eq!(ShellKind::from_path("/usr/bin/fish"), ShellKind::Other);
        assert_eq!(ShellKind::Cmd.capability(), PromptCapability::StaticAnsi);
        assert_eq!(ShellKind::Other.capability(), PromptCapability::Unsupported);
    }

    #[test]
    fn detects_zsh_frameworks_and_ignores_comments_and_empty_theme() {
        let d = temp_dir("zsh");
        let home = d.to_string_lossy().to_string();
        let env = |k: &str| match k {
            "HOME" => Some(home.clone()),
            _ => None,
        };

        // Bare zshrc -> None
        std::fs::write(d.join(".zshrc"), "# eval \"$(starship init zsh)\"\nZSH_THEME=\"\"\nalias ll='ls -la'\n").unwrap();
        assert_eq!(detect_with_env(ShellKind::Zsh, env, &[]).detected, None);

        // Oh My Zsh
        std::fs::write(d.join(".zshrc"), "ZSH_THEME=\"robbyrussell\"\nsource $ZSH/oh-my-zsh.sh\n").unwrap();
        let det = detect_with_env(ShellKind::Zsh, env, &[]).detected.unwrap();
        assert_eq!(det.name, "Oh My Zsh");

        // Starship
        std::fs::write(d.join(".zshrc"), "eval \"$(starship init zsh)\"\n").unwrap();
        let det = detect_with_env(ShellKind::Zsh, env, &[]).detected.unwrap();
        assert_eq!(det.name, "Starship");

        // Custom PROMPT
        std::fs::write(d.join(".zshrc"), "PROMPT='%~ %# '\n").unwrap();
        let det = detect_with_env(ShellKind::Zsh, env, &[]).detected.unwrap();
        assert_eq!(det.name, "Custom PROMPT");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn detects_bash_and_ignores_apple_cwd_helper() {
        let d = temp_dir("bash");
        let home = d.to_string_lossy().to_string();
        let env = |k: &str| match k {
            "HOME" => Some(home.clone()),
            _ => None,
        };

        std::fs::write(
            d.join(".bash_profile"),
            "# PS1='custom'\nPROMPT_COMMAND=\"update_terminal_cwd; $PROMPT_COMMAND\"\n",
        )
        .unwrap();
        assert_eq!(detect_with_env(ShellKind::Bash, env, &[]).detected, None);

        std::fs::write(d.join(".bashrc"), "PS1='\\u@\\h:\\w\\$ '\n").unwrap();
        let det = detect_with_env(ShellKind::Bash, env, &[]).detected.unwrap();
        assert_eq!(det.name, "Custom PS1");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn detects_powershell_profiles_and_ignores_commented_out_init() {
        let d = temp_dir("ps");
        let ps_dir = d.join("WindowsPowerShell");
        std::fs::create_dir_all(&ps_dir).unwrap();
        let prof = ps_dir.join("Microsoft.PowerShell_profile.ps1");
        let env = |_: &str| None;

        std::fs::write(&prof, "# oh-my-posh init pwsh | Invoke-Expression\nSet-PSReadLineOption -EditMode Emacs\n").unwrap();
        assert_eq!(
            detect_with_env(ShellKind::PowerShell, env, std::slice::from_ref(&d)).detected,
            None
        );

        std::fs::write(&prof, "oh-my-posh init pwsh --config ~/theme.omp.json | Invoke-Expression\n").unwrap();
        let det = detect_with_env(ShellKind::PowerShell, env, std::slice::from_ref(&d))
            .detected
            .unwrap();
        assert_eq!(det.name, "Oh My Posh");

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn detects_cmd_prompt_unless_default_or_shellax_osc_only() {
        let def_env = |k: &str| match k {
            "PROMPT" => Some(format!("{CMD_PREFIX}$P$G")),
            _ => None,
        };
        assert_eq!(detect_cmd(&def_env), None);

        let custom_env = |k: &str| match k {
            "PROMPT" => Some(format!("{CMD_PREFIX}$E[32m$P$G")),
            _ => None,
        };
        assert_eq!(detect_cmd(&custom_env).unwrap().name, "Custom PROMPT");
    }

    #[test]
    fn powershell_scripts_never_contain_double_quotes_and_preserve_osc() {
        for style in PromptStyle::ALL {
            for integrate in [true, false] {
                if let Some(script) = pwsh_script(integrate, style) {
                    assert!(
                        !script.contains('"'),
                        "style {style:?} integrate={integrate} contained double quote: {script}"
                    );
                    if integrate {
                        assert!(script.contains("9;9;"), "{style:?}");
                    }
                    if style != PromptStyle::System {
                        assert!(script.contains("LASTEXITCODE"), "{style:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn zsh_and_bash_scripts_include_osc133_when_integrated() {
        for style in PromptStyle::PRESETS {
            let z = zsh_zshrc(true, true, style, "");
            assert!(z.contains("133;B"), "zsh {style:?}");
            assert!(z.contains("133;D;"), "zsh {style:?}");
            assert!(z.contains("__as_git_branch"), "zsh {style:?}");

            let b = bash_rc(true, true, style, "");
            assert!(b.contains("133;B"), "bash {style:?}");
            assert!(b.contains("133;C"), "bash {style:?}");
            assert!(b.contains("PROMPT_COMMAND=__as_prompt_cmd"), "bash {style:?}");
        }
    }

    #[test]
    fn cmd_prompt_preserves_osc9_9_across_styles() {
        for style in PromptStyle::ALL {
            let p = cmd_prompt(None, true, style);
            assert!(p.starts_with(CMD_PREFIX), "{style:?}: {p}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_scripts_pass_shell_syntax_check() {
        use std::process::Command;
        let dir = temp_dir("syntax");
        for (bin, flag_ext, is_zsh) in [("zsh", "zsh", true), ("bash", "sh", false)] {
            if Command::new(bin).arg("--version").output().is_err() {
                continue;
            }
            for style in PromptStyle::ALL {
                for integrate in [true, false] {
                    let body = if is_zsh {
                        zsh_zshrc(integrate, true, style, "")
                    } else {
                        bash_rc(integrate, true, style, "")
                    };
                    let f = dir.join(format!("{}-{}.{flag_ext}", style.as_str(), integrate));
                    std::fs::write(&f, &body).unwrap();
                    let out = Command::new(bin).arg("-n").arg(&f).output().unwrap();
                    assert!(
                        out.status.success(),
                        "{bin} -n failed for {style:?} integrate={integrate}: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
