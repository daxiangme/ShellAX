//! Git worktree helpers for isolated per-task agent workspaces.
//!
//! Portions (marked [3p]) Copyright (c) herdr contributors. Licensed under the Apache License,
//! Version 2.0; modified by ShellAX. See THIRD_PARTY_NOTICES.md.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const DEFAULT_WORKTREE_PREFIX: &str = "agent";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_prunable: bool,
}

/// Generate a readable branch name when the user leaves the task name empty. [3p]
pub fn generated_branch_slug(seed: u64) -> String {
    let adjectives = [
        "brave", "calm", "clear", "green", "lucky", "quiet", "rapid", "silver",
    ];
    let nouns = [
        "river", "cloud", "field", "forest", "harbor", "meadow", "stone", "valley",
    ];
    let adjective = adjectives[(seed as usize) % adjectives.len()];
    let noun = nouns[((seed / adjectives.len() as u64) as usize) % nouns.len()];
    let suffix = seed & 0xffff;
    format!("{DEFAULT_WORKTREE_PREFIX}/{adjective}-{noun}-{suffix:04x}")
}

/// Convert an arbitrary task title or branch name into a filesystem-safe slug. [3p]
pub fn branch_to_path_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in branch.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed
    }
}

/// Convert a task title into a default branch name `agent/<slug>`.
pub fn branch_for_task(task_name: &str, seed: u64) -> String {
    let s = branch_to_path_slug(task_name);
    if s == "task" && !task_name.chars().any(|c| c.is_ascii_alphanumeric()) {
        generated_branch_slug(seed)
    } else {
        format!("{DEFAULT_WORKTREE_PREFIX}/{s}")
    }
}

/// Default worktree root `~/.shellax/worktrees`.
pub fn default_worktrees_root() -> PathBuf {
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    } else {
        std::env::var_os("HOME")
    };
    home.map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".shellax")
        .join("worktrees")
}

/// Default checkout directory `<root>/<repo_name>/<slug>`. [3p]
pub fn default_checkout_path(root: &Path, repo_name: &str, branch: &str) -> PathBuf {
    root.join(repo_name).join(branch_to_path_slug(branch))
}

/// Resolve the git repository toplevel for `dir`, if `dir` is inside a git worktree.
pub fn git_toplevel(dir: &Path) -> Option<PathBuf> {
    let out = git_cmd(dir, false)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// Read the current branch name (or short HEAD hash if detached) of `dir`.
pub fn git_current_branch(dir: &Path) -> Option<String> {
    let out = git_cmd(dir, false)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b.is_empty() {
        None
    } else if b == "HEAD" {
        let rev = git_cmd(dir, false)
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()?;
        let h = String::from_utf8_lossy(&rev.stdout).trim().to_string();
        (!h.is_empty()).then_some(h)
    } else {
        Some(b)
    }
}

/// Zero-subprocess git toplevel lookup for UI render paths (walks `.git` ancestors + 10s TTL cache).
/// Avoids `CreateProcessW("git.exe")` stalls on Windows during scrolling/rendering.
pub fn fast_git_toplevel(dir: &Path) -> Option<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Instant, Option<PathBuf>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((at, res)) = g.get(dir) {
            if at.elapsed() < Duration::from_secs(10) {
                return res.clone();
            }
        }
    }
    let found = find_dot_git_toplevel(dir);
    if let Ok(mut g) = cache.lock() {
        if g.len() > 512 {
            g.clear();
        }
        g.insert(dir.to_path_buf(), (Instant::now(), found.clone()));
    }
    found
}

/// Zero-subprocess git branch/short-HEAD reader for UI render paths (reads `.git/HEAD` directly + 3s TTL cache).
/// Supports both standard `.git/HEAD` directories and linked worktree `.git` gitfiles (`gitdir: ...`).
pub fn fast_git_branch(dir: &Path) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Instant, Option<String>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(g) = cache.lock() {
        if let Some((at, res)) = g.get(dir) {
            if at.elapsed() < Duration::from_secs(3) {
                return res.clone();
            }
        }
    }
    let found = read_head_branch_no_spawn(dir);
    if let Ok(mut g) = cache.lock() {
        if g.len() > 512 {
            g.clear();
        }
        g.insert(dir.to_path_buf(), (Instant::now(), found.clone()));
    }
    found
}

fn find_dot_git_toplevel(dir: &Path) -> Option<PathBuf> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        if d.join(".git").exists() {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

fn read_head_branch_no_spawn(dir: &Path) -> Option<String> {
    let top = find_dot_git_toplevel(dir)?;
    let dot_git = top.join(".git");
    let git_dir = if dot_git.is_file() {
        let raw = std::fs::read_to_string(&dot_git).ok()?;
        let line = raw.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
        let p = Path::new(line);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            top.join(p)
        }
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let line = head.lines().next()?.trim();
    if let Some(rest) = line.strip_prefix("ref:") {
        let r = rest.trim();
        let name = r.strip_prefix("refs/heads/").unwrap_or(r).trim();
        (!name.is_empty()).then(|| name.to_string())
    } else if line.len() >= 7 && line.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(line[..7].to_string())
    } else {
        None
    }
}

/// Short `git diff --stat` summary for task completion review.
pub fn git_diff_stat(dir: &Path, base: Option<&str>) -> String {
    let mut cmd = git_cmd(dir, false);
    cmd.arg("diff").arg("--stat");
    if let Some(b) = base.filter(|b| !b.trim().is_empty()) {
        cmd.arg(b);
    }
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn git_args(repo_root: &Path, trust_repository: bool) -> Vec<String> {
    let mut args = Vec::new();
    if trust_repository {
        args.push("-c".into());
        args.push(format!("safe.directory={}", repo_root.display()));
    }
    args.push("-C".into());
    args.push(repo_root.display().to_string());
    args
}

fn git_cmd(repo_root: &Path, trust_repository: bool) -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.args(git_args(repo_root, trust_repository));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Check whether `path` has uncommitted or untracked changes. [3p]
pub fn checkout_has_dirty_files(path: &Path, trust_repository: bool) -> Result<bool, String> {
    let output = git_cmd(path, trust_repository)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(!output.stdout.is_empty());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("git status failed with status {}", output.status)
    })
}

/// Build `git worktree add -b <branch> <path> <base>`. [3p]
pub fn build_worktree_add_new_branch_command(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    trust_repository: bool,
) -> WorktreeCommand {
    let mut args = git_args(repo_root, trust_repository);
    args.extend([
        "worktree".into(),
        "add".into(),
        "-b".into(),
        branch.into(),
        path.display().to_string(),
        base.into(),
    ]);
    WorktreeCommand {
        program: "git".into(),
        args,
    }
}

/// Build `git worktree add <path> <branch>`. [3p]
pub fn build_worktree_add_existing_branch_command(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    trust_repository: bool,
) -> WorktreeCommand {
    let mut args = git_args(repo_root, trust_repository);
    args.extend([
        "worktree".into(),
        "add".into(),
        path.display().to_string(),
        branch.into(),
    ]);
    WorktreeCommand {
        program: "git".into(),
        args,
    }
}

/// Build `git worktree remove [--force] <path>`. [3p]
pub fn build_worktree_remove_command(
    repo_root: &Path,
    path: &Path,
    force: bool,
    trust_repository: bool,
) -> WorktreeCommand {
    let mut args = git_args(repo_root, trust_repository);
    args.extend(["worktree".into(), "remove".into()]);
    if force {
        args.push("--force".into());
    }
    args.push(path.display().to_string());
    WorktreeCommand {
        program: "git".into(),
        args,
    }
}

pub fn local_branch_exists(
    repo_root: &Path,
    branch: &str,
    trust_repository: bool,
) -> Result<bool, String> {
    let output = git_cmd(repo_root, trust_repository)
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("git show-ref failed with status {}", output.status)
    } else {
        stderr
    })
}

/// Create a worktree at `path` for `branch` (reusing `branch` if it already exists locally). [3p]
pub fn create_worktree(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    trust_repository: bool,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let command = if local_branch_exists(repo_root, branch, trust_repository)? {
        build_worktree_add_existing_branch_command(repo_root, path, branch, trust_repository)
    } else {
        build_worktree_add_new_branch_command(repo_root, path, branch, base, trust_repository)
    };
    run_worktree_command(&command)
}

pub fn run_worktree_command(command: &WorktreeCommand) -> Result<(), String> {
    let mut cmd = Command::new(&command.program);
    cmd.env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(&command.args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(if message.is_empty() {
        format!("{} failed with status {}", command.program, output.status)
    } else {
        message
    })
}

pub fn is_not_working_tree_remove_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("is not a working tree") || lower.contains("is not a worktree")
}

/// Remove a worktree safely, refusing dirty checkouts unless `force` is true. [3p]
pub fn remove_worktree(
    repo_root: &Path,
    path: &Path,
    force: bool,
    trust_repository: bool,
) -> Result<(), String> {
    if !force && path.exists() && checkout_has_dirty_files(path, trust_repository)? {
        return Err(format!(
            "worktree '{}' has uncommitted or untracked changes",
            path.display()
        ));
    }
    let cmd = build_worktree_remove_command(repo_root, path, force, trust_repository);
    match run_worktree_command(&cmd) {
        Ok(()) => Ok(()),
        Err(err) if force && is_not_working_tree_remove_error(&err) => {
            let listed = list_existing_worktrees(repo_root, trust_repository)?;
            if listed.iter().any(|w| w.path == path) {
                return Err(err);
            }
            if path.exists() && leftover_matches_repo(repo_root, path, trust_repository) {
                std::fs::remove_dir_all(path).map_err(|e| format!("{err}; {e}"))?;
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn leftover_matches_repo(repo_root: &Path, path: &Path, trust_repository: bool) -> bool {
    let Ok(content) = std::fs::read_to_string(path.join(".git")) else {
        return false;
    };
    let Some(gitdir) = content.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let gitdir = PathBuf::from(gitdir.trim());
    let gitdir = if gitdir.is_absolute() {
        gitdir
    } else {
        path.join(gitdir)
    };
    let Ok(out) = git_cmd(repo_root, trust_repository)
        .args(["rev-parse", "--git-common-dir"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if common.is_empty() {
        return false;
    }
    let common = PathBuf::from(common);
    let worktrees = if common.is_absolute() {
        common
    } else {
        repo_root.join(common)
    }
    .join("worktrees");
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(&gitdir).starts_with(canon(&worktrees))
}

/// Parse `git worktree list --porcelain` output. [3p]
pub fn parse_worktree_list_porcelain(output: &str) -> Vec<ExistingWorktree> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = None;
    let mut is_bare = false;
    let mut is_detached = false;
    let mut is_prunable = false;

    let finish = |entries: &mut Vec<ExistingWorktree>,
                  path: &mut Option<PathBuf>,
                  branch: &mut Option<String>,
                  is_bare: &mut bool,
                  is_detached: &mut bool,
                  is_prunable: &mut bool| {
        if let Some(p) = path.take() {
            entries.push(ExistingWorktree {
                path: p,
                branch: branch.take(),
                is_bare: *is_bare,
                is_detached: *is_detached,
                is_prunable: *is_prunable,
            });
        }
        *is_bare = false;
        *is_detached = false;
        *is_prunable = false;
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            finish(
                &mut entries,
                &mut path,
                &mut branch,
                &mut is_bare,
                &mut is_detached,
                &mut is_prunable,
            );
            continue;
        }
        if let Some(v) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(v));
        } else if let Some(v) = line.strip_prefix("branch ") {
            branch = Some(v.strip_prefix("refs/heads/").unwrap_or(v).to_string());
        } else if line == "detached" {
            is_detached = true;
        } else if line == "bare" {
            is_bare = true;
        } else if line.starts_with("prunable") {
            is_prunable = true;
        }
    }
    finish(
        &mut entries,
        &mut path,
        &mut branch,
        &mut is_bare,
        &mut is_detached,
        &mut is_prunable,
    );
    entries
}

pub fn list_existing_worktrees(
    repo_root: &Path,
    trust_repository: bool,
) -> Result<Vec<ExistingWorktree>, String> {
    let output = git_cmd(repo_root, trust_repository)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(parse_worktree_list_porcelain(&String::from_utf8_lossy(
            &output.stdout,
        )));
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("git worktree list failed with status {}", output.status)
    } else {
        stderr
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_porcelain_parsing() {
        assert_eq!(branch_to_path_slug("feat/Login UI!"), "feat-login-ui");
        assert_eq!(branch_for_task("Fix SFTP resume", 1), "agent/fix-sftp-resume");
        let sample = "worktree /repo\nbranch refs/heads/master\n\nworktree /wt/a\nbranch refs/heads/agent/a\n";
        let list = parse_worktree_list_porcelain(sample);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].branch.as_deref(), Some("master"));
        assert_eq!(list[1].branch.as_deref(), Some("agent/a"));
    }

    #[test]
    fn fast_git_readers_handle_repo_and_worktree_file() {
        let tmp = std::env::temp_dir().join(format!("shellax-wt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let main_repo = tmp.join("repo");
        let sub_dir = main_repo.join("src/nested");
        let wt_gitdir = main_repo.join(".git/worktrees/feat-x");
        let linked_wt = tmp.join("wt-feat-x");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::create_dir_all(&linked_wt).unwrap();

        std::fs::write(main_repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(wt_gitdir.join("HEAD"), "ref: refs/heads/agent/feat-x\n").unwrap();
        std::fs::write(
            linked_wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        assert_eq!(find_dot_git_toplevel(&sub_dir).as_deref(), Some(main_repo.as_path()));
        assert_eq!(read_head_branch_no_spawn(&sub_dir).as_deref(), Some("main"));
        assert_eq!(
            read_head_branch_no_spawn(&linked_wt).as_deref(),
            Some("agent/feat-x")
        );

        std::fs::write(
            main_repo.join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(read_head_branch_no_spawn(&main_repo).as_deref(), Some("0123456"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
