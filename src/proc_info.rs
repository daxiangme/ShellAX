//! Cross-platform foreground process & working directory inspection for local PTYs.
//!
//! Portions (marked [3p]) Copyright (c) herdr contributors. Licensed under the Apache License,
//! Version 2.0; modified by ShellAX. See THIRD_PARTY_NOTICES.md.

use std::path::PathBuf;

/// Normalize a raw command name + argv into the effective CLI tool name.
/// Unwraps script interpreters (`node`, `bun`, `deno`, `python*`) and strips login-shell `-`.
pub fn effective_command_name(comm: Option<&str>, argv: &[String]) -> Option<String> {
    let base = |s: &str| -> Option<String> {
        let b = s.rsplit(['/', '\\']).next().unwrap_or(s);
        let b = b.strip_prefix('-').unwrap_or(b).trim();
        (!b.is_empty()).then(|| b.to_string())
    };
    let strip_script_ext = |s: String| -> String {
        for ext in [".mjs", ".cjs", ".js", ".py", ".ts"] {
            if let Some(stem) = s.strip_suffix(ext) {
                if !stem.is_empty() {
                    return stem.to_string();
                }
            }
        }
        s
    };

    let argv0 = argv.first().and_then(|a| base(a));
    let head = argv0.or_else(|| comm.and_then(base))?;
    let lower = head.to_ascii_lowercase();
    if lower == "env" {
        // Skip `env` flags (`-S`, `-i`, `-u VAR`) and `KEY=VAL` assignments, then recurse.
        let mut i = 1;
        while i < argv.len() {
            let a = argv[i].as_str();
            if a == "-u" || a == "--unset" || a == "-C" || a == "--chdir" {
                i += 2;
                continue;
            }
            if a.starts_with('-') || a.contains('=') {
                i += 1;
                continue;
            }
            return effective_command_name(None, &argv[i..]);
        }
        return Some(head);
    }
    if matches!(
        lower.as_str(),
        "node"
            | "nodejs"
            | "bun"
            | "deno"
            | "python"
            | "python3"
            | "sh"
            | "bash"
            | "zsh"
            | "dash"
    ) {
        // Find the first positional script file argument (skip interpreter flags & `-c`/`-e` inline code).
        let mut skip_next = false;
        for arg in argv.iter().skip(1) {
            if skip_next {
                skip_next = false;
                continue;
            }
            if matches!(
                arg.as_str(),
                "-r" | "--require" | "--import" | "-e" | "-c" | "-o"
            ) {
                skip_next = true;
                continue;
            }
            if arg.starts_with('-') || arg.starts_with('+') {
                continue;
            }
            if let Some(script) = base(arg) {
                return Some(strip_script_ext(script));
            }
        }
    }
    Some(head)
}

/// Given a PTY foreground process group ID (e.g. from `tcgetpgrp`), return the effective foreground
/// command name. Checks all processes in the foreground group so wrapper scripts (`sh claude` ->
/// `node claude`) resolve to the inner CLI rather than `sh`.
pub fn fg_group_command(pgid: i32) -> Option<String> {
    if pgid <= 0 {
        return None;
    }
    let pgid = pgid as u32;
    #[cfg(target_os = "macos")]
    {
        return macos::fg_group_command(pgid);
    }
    #[cfg(target_os = "linux")]
    {
        return linux::fg_group_command(pgid);
    }
    #[allow(unreachable_code)]
    None
}

/// Read the foreground process group ID of a controlling terminal from its shell PID.
pub fn fg_pgid_of_shell(shell_pid: u32) -> Option<i32> {
    if shell_pid == 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        return macos::foreground_process_group_id(shell_pid).map(|p| p as i32);
    }
    #[cfg(target_os = "linux")]
    {
        return linux::foreground_process_group_id(shell_pid);
    }
    #[allow(unreachable_code)]
    None
}

/// Read the effective command name of a specific PID.
pub fn process_command_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        return macos::process_command_name(pid);
    }
    #[cfg(target_os = "linux")]
    {
        return linux::process_command_name(pid);
    }
    #[allow(unreachable_code)]
    None
}

/// Read the current working directory of `pid`.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        macos::process_cwd(pid)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let mut sys = System::new();
        let p = Pid::from_u32(pid);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[p]),
            ProcessRefreshKind::new().with_cwd(UpdateKind::Always),
        );
        sys.process(p)
            .and_then(|pr| pr.cwd())
            .filter(|d| !d.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
    }
}

fn looks_like_version(name: &str) -> bool {
    let trimmed = name.trim();
    let body = trimmed.strip_prefix('v').unwrap_or(trimmed);
    !body.is_empty()
        && body.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && body
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_'))
}

/// Infer the effective CLI tool name from an executable path (e.g.
/// `~/.local/share/claude/versions/2.1.318` -> `"claude"`).
pub fn infer_command_from_exec_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts: Vec<&str> = trimmed
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let file = *parts.last()?;
    let clean_file = file
        .strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .or_else(|| file.strip_suffix(".cmd"))
        .or_else(|| file.strip_suffix(".bat"))
        .unwrap_or(file);
    if !looks_like_version(clean_file) {
        return Some(clean_file.to_string());
    }
    // Match `/.../<tool>/versions/<ver>` (and `releases` / `current` layouts).
    for (idx, seg) in parts.iter().enumerate() {
        if idx > 0
            && matches!(
                seg.to_ascii_lowercase().as_str(),
                "versions" | "releases" | "current"
            )
        {
            let parent = parts[idx - 1];
            if !looks_like_version(parent)
                && !matches!(
                    parent.to_ascii_lowercase().as_str(),
                    "bin" | "lib" | "libexec" | "share" | "local" | "usr" | "opt"
                )
            {
                return Some(parent.to_string());
            }
        }
    }
    None
}

fn is_wrapper_shell(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "dash" | "fish" | "env"
    )
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{effective_command_name, is_wrapper_shell};

    pub fn process_command_name(pid: u32) -> Option<String> {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let argv: Vec<String> = cmdline
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        effective_command_name(Some(comm.trim()), &argv)
    }

    pub fn foreground_process_group_id(pid: u32) -> Option<i32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &stat[stat.rfind(')')? + 2..];
        let tpgid: i32 = rest.split_whitespace().nth(5)?.parse().ok()?;
        (tpgid > 0).then_some(tpgid)
    }

    pub fn fg_group_command(pgid: u32) -> Option<String> {
        let leader = process_command_name(pgid);
        if let Some(ref name) = leader {
            if !is_wrapper_shell(name) {
                return leader;
            }
        }
        // Scan `/proc` for non-shell child in the same process group if leader is a wrapper shell.
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for ent in rd.flatten() {
                let Ok(pid) = ent.file_name().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                if pid == pgid {
                    continue;
                }
                let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                    continue;
                };
                let Some(idx) = stat.rfind(')') else {
                    continue;
                };
                let pgrp: Option<u32> = stat[idx + 2..]
                    .split_whitespace()
                    .nth(2)
                    .and_then(|s| s.parse().ok());
                if pgrp == Some(pgid) {
                    if let Some(name) = process_command_name(pid) {
                        if !is_wrapper_shell(&name) {
                            return Some(name);
                        }
                    }
                }
            }
        }
        leader
    }
}

// [3p] macOS libproc + sysctl(KERN_PROCARGS2) inspection, adapted from herdr src/platform/macos.rs
#[cfg(target_os = "macos")]
mod macos {
    use super::{effective_command_name, is_wrapper_shell};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    const PROC_PGRP_ONLY: u32 = 2;

    pub fn fg_group_command(pgid: u32) -> Option<String> {
        // Check the foreground process-group leader first so an agent spawning short-lived
        // child tools (`git`, `cargo`, `sleep`) stays identified as the agent itself.
        let leader = process_command_name(pgid);
        if let Some(ref name) = leader {
            if !is_wrapper_shell(name) {
                return leader;
            }
        }
        let mut pids = process_group_pids(pgid);
        let mut fallback: Option<String> = leader;
        pids.sort_unstable();
        for &pid in pids.iter().rev() {
            if pid == pgid {
                continue;
            }
            let Some(info) = process_bsdinfo(pid) else {
                continue;
            };
            if info.pbi_pgid != pgid || info.pbi_status == libc::SZOMB {
                continue;
            }
            let comm = comm_from_bsdinfo(&info);
            let argv = process_argv(pid).unwrap_or_default();
            if let Some(name) = effective_command_name(comm.as_deref(), &argv) {
                if !is_wrapper_shell(&name) {
                    return Some(name);
                }
                if fallback.is_none() {
                    fallback = Some(name);
                }
            }
        }
        fallback
    }

    pub fn process_command_name(pid: u32) -> Option<String> {
        let info = process_bsdinfo(pid)?;
        if info.pbi_status == libc::SZOMB {
            return None;
        }
        let comm = comm_from_bsdinfo(&info);
        let argv = process_argv(pid).unwrap_or_default();
        let effective = effective_command_name(comm.as_deref(), &argv);
        if argv.is_empty()
            && effective
                .as_deref()
                .is_none_or(|n| super::looks_like_version(n) || n.len() >= 14)
        {
            if let Some(hint) = process_path(pid)
                .as_deref()
                .and_then(super::infer_command_from_exec_path)
            {
                return Some(hint);
            }
        }
        effective
    }

    fn process_path(pid: u32) -> Option<String> {
        let mut buf = vec![0u8; 4096];
        let ret = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len() as u32,
            )
        };
        if ret <= 0 {
            return None;
        }
        let n = (ret as usize).min(buf.len());
        String::from_utf8(buf[..n].to_vec()).ok()
    }

    /// Read `e_tpgid` (foreground process group of the controlling terminal) for `pid`. [3p]
    pub fn foreground_process_group_id(pid: u32) -> Option<u32> {
        let info = process_bsdinfo(pid)?;
        let fg = info.e_tpgid;
        if fg > 0 {
            #[allow(clippy::unnecessary_cast)]
            Some(fg as u32)
        } else {
            None
        }
    }

    fn process_group_pids(process_group_id: u32) -> Vec<u32> {
        let mut capacity = 16usize;
        for _ in 0..8 {
            let mut pids = vec![0 as libc::pid_t; capacity];
            let buffer_bytes = pids.len() * std::mem::size_of::<libc::pid_t>();
            let returned_bytes = unsafe {
                libc::proc_listpids(
                    PROC_PGRP_ONLY,
                    process_group_id,
                    pids.as_mut_ptr() as *mut libc::c_void,
                    buffer_bytes as libc::c_int,
                )
            };
            if returned_bytes <= 0 {
                return Vec::new();
            }
            let returned_bytes = returned_bytes as usize;
            let count = returned_bytes / std::mem::size_of::<libc::pid_t>();
            if returned_bytes < buffer_bytes {
                return pids
                    .into_iter()
                    .take(count)
                    .filter(|&pid| pid > 0)
                    .map(|pid| pid as u32)
                    .collect();
            }
            capacity = capacity.saturating_mul(2);
        }
        Vec::new()
    }

    fn process_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let ret = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        (ret == size).then_some(info)
    }

    fn comm_from_bsdinfo(info: &libc::proc_bsdinfo) -> Option<String> {
        let end = info
            .pbi_comm
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(info.pbi_comm.len());
        if end == 0 {
            return None;
        }
        let bytes: Vec<u8> = info.pbi_comm[..end].iter().map(|&b| b as u8).collect();
        String::from_utf8(bytes).ok()
    }

    fn kern_procargs2(pid: u32) -> Option<Vec<u8>> {
        unsafe {
            let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
            let mut size: libc::size_t = 0;
            let ret = libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &mut size,
                std::ptr::null_mut(),
                0,
            );
            if ret != 0 || size == 0 {
                return None;
            }
            let mut buf = vec![0u8; size];
            let ret = libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            );
            if ret != 0 {
                return None;
            }
            buf.truncate(size);
            Some(buf)
        }
    }

    fn process_argv(pid: u32) -> Option<Vec<String>> {
        let buf = kern_procargs2(pid)?;
        if buf.len() < 4 {
            return None;
        }
        let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if argc < 1 {
            return None;
        }
        let rest = &buf[4..];
        let exec_end = rest.iter().position(|&b| b == 0)?;
        let mut current = exec_end;
        while current < rest.len() && rest[current] == 0 {
            current += 1;
        }
        if current >= rest.len() {
            return None;
        }
        let mut argv = Vec::with_capacity(argc as usize);
        for _ in 0..argc {
            if current >= rest.len() {
                break;
            }
            let end = rest[current..]
                .iter()
                .position(|&b| b == 0)
                .map(|off| current + off)
                .unwrap_or(rest.len());
            if end == current {
                break;
            }
            argv.push(String::from_utf8_lossy(&rest[current..end]).into_owned());
            current = end + 1;
        }
        (!argv.is_empty()).then_some(argv)
    }

    /// Read current working directory via `proc_pidinfo(PROC_PIDVNODEPATHINFO)`. [3p]
    pub fn process_cwd(pid: u32) -> Option<PathBuf> {
        let mut pathinfo: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        let ret = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                &mut pathinfo as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if ret != size {
            return None;
        }
        let vip_path = unsafe {
            std::slice::from_raw_parts(
                pathinfo.pvi_cdir.vip_path.as_ptr() as *const u8,
                libc::MAXPATHLEN as usize,
            )
        };
        let nul = vip_path.iter().position(|&b| b == 0)?;
        if nul == 0 {
            return None;
        }
        Some(PathBuf::from(OsStr::from_bytes(&vip_path[..nul])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_interpreters_and_login_shells() {
        assert_eq!(
            effective_command_name(Some("zsh"), &["-zsh".into()]),
            Some("zsh".into())
        );
        assert_eq!(
            effective_command_name(
                Some("node"),
                &["/opt/homebrew/bin/node".into(), "/opt/homebrew/bin/claude".into()]
            ),
            Some("claude".into())
        );
        assert_eq!(
            effective_command_name(
                Some("node"),
                &[
                    "node".into(),
                    "--no-warnings".into(),
                    "/usr/local/lib/node_modules/@openai/codex/bin/codex.js".into()
                ]
            ),
            Some("codex".into())
        );
    }

    #[test]
    fn self_cwd_matches_env_current_dir() {
        let expected = std::env::current_dir()
            .ok()
            .and_then(|p| std::fs::canonicalize(p).ok());
        let actual = process_cwd(std::process::id()).and_then(|p| std::fs::canonicalize(p).ok());
        assert_eq!(actual, expected);
    }

    #[cfg(unix)]
    #[test]
    fn child_sleep_command_detected() {
        let mut child = std::process::Command::new("sleep")
            .arg("2")
            .spawn()
            .expect("spawn sleep");
        let name = process_command_name(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(name.as_deref(), Some("sleep"));
    }

    #[test]
    fn infers_versioned_claude_binary_from_exec_path() {
        assert_eq!(
            infer_command_from_exec_path("/Users/afeng/.local/share/claude/versions/2.1.318"),
            Some("claude".into())
        );
        assert_eq!(
            infer_command_from_exec_path("/Users/afeng/.local/bin/agy"),
            Some("agy".into())
        );
        assert_eq!(
            infer_command_from_exec_path(r"C:\Users\u\AppData\Local\claude\versions\2.1.318.exe"),
            Some("claude".into())
        );
    }
}
