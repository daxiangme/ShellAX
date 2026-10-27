//! 应用内下载并更新：下载安装包、按 SHA256 校验、保存下载记录、退出后安装。
//! 下载目录为 `<数据目录>/updates/<版本号>/`，记录文件为 `<数据目录>/updates/pending.json`
//! （只含版本号、文件路径、大小、时间与来源地址，不含任何凭据）。
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::update::{is_newer, Manifest, CURRENT};

/// 下载进度（后台线程写，界面读）。`total` 为 0 表示服务器没给长度。
#[derive(Default)]
pub struct Progress { pub done: AtomicU64, pub total: AtomicU64, pub cancel: AtomicBool }

/// 已下载、已校验的安装包。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub version: String,
    pub file: PathBuf,
    pub size: u64,
    /// 下载完成时间（Unix 秒）。
    pub downloaded_at: i64,
}

pub fn updates_dir(data_dir: &Path) -> PathBuf { data_dir.join("updates") }
fn record_path(data_dir: &Path) -> PathBuf { updates_dir(data_dir).join("pending.json") }

/// 本平台要下载的地址：GitHub 的安装包地址里，dmg 换成 zip（zip 便于直接替换 .app）。
pub fn download_url(m: &Manifest) -> String {
    if cfg!(target_os = "macos") && m.url.contains("/releases/download/") && m.url.ends_with(".dmg") {
        return format!("{}.zip", m.url.trim_end_matches(".dmg"));
    }
    m.url.clone()
}

/// 下载地址能否在应用内下载（指向文件，而不是 release 网页）。
pub fn downloadable(m: &Manifest) -> bool {
    let u = download_url(m);
    let name = file_name(&u);
    (u.starts_with("https://") || u.starts_with("http://")) && [".exe", ".dmg", ".zip", ".msi", ".pkg"].iter().any(|x| name.to_ascii_lowercase().ends_with(x))
}

fn file_name(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string()
}

/// 同一 release 下的 SHA256SUMS.txt 地址（只对 GitHub 的 releases/download 地址有效）。
pub fn sums_url(asset_url: &str) -> Option<String> {
    if !asset_url.contains("/releases/download/") { return None; }
    let (dir, _) = asset_url.rsplit_once('/')?;
    Some(format!("{dir}/SHA256SUMS.txt"))
}

/// 从 SHA256SUMS.txt 里找某个文件名的哈希（`<hex>  <name>` 或 `<hex> *<name>`）。
pub fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let (h, n) = (it.next()?, it.next()?);
        (n.trim_start_matches('*') == name && h.len() == 64).then(|| h.to_ascii_lowercase())
    })
}

fn agent(url: &str, timeout: Duration) -> Result<ureq::Agent> {
    Ok(crate::proxy::ureq_builder_sys(url, ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(timeout).redirects(10))?.build())
}

/// 网络请求最多尝试 3 次（间隔 1s、3s）；取消或校验失败不重试。
fn retry<T>(p: Option<&Progress>, mut f: impl FnMut() -> Result<T>) -> Result<T> {
    let mut last = None;
    for (i, wait) in [0u64, 1, 3].into_iter().enumerate() {
        if wait > 0 { std::thread::sleep(Duration::from_secs(wait)); }
        if p.is_some_and(|p| p.cancel.load(Ordering::Relaxed)) { bail!("cancelled"); }
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let s = format!("{e:#}");
                if s.contains("cancelled") || s.contains("checksum mismatch") || s.contains("no entry for") || s.contains("HTTP 404") { return Err(e); }
                eprintln!("update: attempt {} failed: {s}", i + 1);
                last = Some(e);
            }
        }
    }
    Err(last.unwrap())
}

fn http_err(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, _) => anyhow!("HTTP {code}"),
        ureq::Error::Transport(t) => anyhow!("{t}"),
    }
}

/// 期望的 SHA256：自定义 manifest 的 sha256 字段优先，否则读同一 release 的 SHA256SUMS.txt。
/// 两者都没有时返回 None（不校验）。
fn expected_sha(m: &Manifest, url: &str) -> Result<Option<String>> {
    let s = m.sha256.trim().to_ascii_lowercase();
    if !s.is_empty() && download_url(m) == m.url { return Ok(Some(s)); }
    let Some(su) = sums_url(url) else { return Ok(None) };
    let body = retry(None, || Ok(agent(&su, Duration::from_secs(20))?.get(&su).set("User-Agent", &format!("shellax/{CURRENT}")).call().map_err(http_err)?.into_string()?))
        .with_context(|| "SHA256SUMS.txt")?;
    sum_for(&body, &file_name(url)).map(Some).ok_or_else(|| anyhow!("SHA256SUMS.txt: no entry for {}", file_name(url)))
}

/// 下载并校验。成功后写入下载记录并返回。取消时返回错误 "cancelled"，并删除未完成的文件。
pub fn download(m: &Manifest, data_dir: &Path, p: &Progress) -> Result<Record> {
    let url = download_url(m);
    let name = file_name(&url);
    if name.is_empty() { bail!("no installer for this platform"); }
    let dir = updates_dir(data_dir).join(&m.version);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&name);
    let expected = expected_sha(m, &url)?;
    let size = fetch_to(&url, &dest, expected.as_deref(), p)?;
    let rec = Record { version: m.version.clone(), file: dest, size, downloaded_at: chrono::Utc::now().timestamp() };
    save_record(data_dir, Some(&rec))?;
    Ok(rec)
}

/// 下载到 `dest`（先写 `.part`，校验通过后改名），带重试、进度、取消与可选 SHA-256 校验。返回字节数。
pub fn fetch_to(url: &str, dest: &Path, expected: Option<&str>, p: &Progress) -> Result<u64> {
    let part = dest.with_file_name(format!("{}.part", dest.file_name().and_then(|n| n.to_str()).unwrap_or("download")));
    let res = retry(Some(p), || {
        let r = agent(url, Duration::from_secs(60))?.get(url).set("User-Agent", &format!("shellax/{CURRENT}")).call().map_err(http_err)?;
        let total = r.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0u64);
        p.total.store(total, Ordering::Relaxed);
        p.done.store(0, Ordering::Relaxed);
        (|| -> Result<(u64, String)> {
            let mut f = std::fs::File::create(&part)?;
            let mut rd = r.into_reader();
            let mut h = Sha256::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut n = 0u64;
            loop {
                if p.cancel.load(Ordering::Relaxed) { bail!("cancelled"); }
                let k = rd.read(&mut buf)?;
                if k == 0 { break; }
                f.write_all(&buf[..k])?;
                h.update(&buf[..k]);
                n += k as u64;
                p.done.store(n, Ordering::Relaxed);
            }
            f.flush()?;
            if total > 0 && n != total { bail!("incomplete download ({n} / {total} bytes)"); }
            Ok((n, h.finalize().iter().map(|b| format!("{b:02x}")).collect()))
        })()
    });
    let (size, got) = match res { Ok(x) => x, Err(e) => { let _ = std::fs::remove_file(&part); return Err(e); } };
    if let Some(exp) = expected {
        let exp = exp.trim().to_ascii_lowercase();
        if exp != got {
            let _ = std::fs::remove_file(&part);
            bail!("checksum mismatch (expected {exp}, got {got})");
        }
    }
    std::fs::rename(&part, dest)?;
    Ok(size)
}

/// 小文件（索引 JSON 等）：走系统代理，最多重试 3 次。
pub fn fetch_text(url: &str) -> Result<String> {
    retry(None, || Ok(agent(url, Duration::from_secs(20))?.get(url).set("User-Agent", &format!("shellax/{CURRENT}")).call().map_err(http_err)?.into_string()?))
}

pub fn save_record(data_dir: &Path, rec: Option<&Record>) -> Result<()> {
    let p = record_path(data_dir);
    match rec {
        Some(r) => { std::fs::create_dir_all(updates_dir(data_dir))?; std::fs::write(&p, serde_json::to_vec_pretty(r)?)?; }
        None => { let _ = std::fs::remove_file(&p); }
    }
    Ok(())
}

/// 读取下载记录，并清理：已安装同版本或更新版本时删掉安装包和记录；其他版本目录一并删除。
pub fn load_and_cleanup(data_dir: &Path) -> Option<Record> {
    let dir = updates_dir(data_dir);
    let rec: Option<Record> = std::fs::read(record_path(data_dir)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let rec = rec.filter(|r| is_newer(&r.version, CURRENT) && r.file.is_file());
    if rec.is_none() { let _ = std::fs::remove_file(record_path(data_dir)); }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let keep = rec.as_ref().is_some_and(|r| e.file_name().to_string_lossy() == r.version);
            if e.path().is_dir() && !keep { let _ = std::fs::remove_dir_all(e.path()); }
        }
    }
    rec
}

/// 在系统文件管理器里显示文件。
pub fn reveal(path: &Path) -> Result<()> {
    #[cfg(windows)]
    { std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()?; }
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg("-R").arg(path).spawn()?; }
    #[cfg(all(unix, not(target_os = "macos")))]
    { std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn()?; }
    Ok(())
}

/// 退出并更新的结果：调用方据此决定是否退出应用。
#[derive(Debug, PartialEq)]
pub enum Apply {
    /// 已启动安装或替换流程，调用方应立即退出应用。
    QuitNow,
    /// 无法自动安装，已在文件管理器中显示安装包（或已打开 dmg），应用继续运行。
    /// 带原因的语言键（`update.why_*`），界面显示在提示里。
    Revealed(&'static str),
}

/// Windows 上的更新方式（纯逻辑，便于测试）。
#[derive(Debug, PartialEq, Clone)]
pub enum WinPlan {
    /// 静默运行安装包，装到 `install_dir`。
    Setup { install_dir: PathBuf },
    /// 便携版：用 zip 里的 exe 替换 `dir` 下的同名文件。
    Portable { dir: PathBuf },
    /// 无法自动更新，原因为语言键。
    Reveal(&'static str),
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase().replace('/', "\\");
    n(a) == n(b)
}

/// 决定 Windows 更新方式。`reg_dir` 为注册表 `HKCU\Software\ShellAX\InstallDir`；
/// `has_file(dir, name)` 判断目录下是否有某文件。运行中的副本在安装目录里，或另有安装版存在时，
/// 安装包更新安装版；运行的是便携副本且下载的是 zip 时，替换便携副本里的 exe。
pub fn win_plan(file: &Path, exe: &Path, reg_dir: Option<&Path>, has_file: &dyn Fn(&Path, &str) -> bool) -> WinPlan {
    let ext = file.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let Some(exe_dir) = exe.parent() else { return WinPlan::Reveal("update.why_other") };
    let running_installed = has_file(exe_dir, "Uninstall.exe") || reg_dir.is_some_and(|r| same_dir(r, exe_dir));
    let other_install = reg_dir.filter(|r| has_file(r, "Uninstall.exe") || has_file(r, "shellax.exe")).map(Path::to_path_buf);
    match ext.as_str() {
        "exe" => {
            if running_installed { WinPlan::Setup { install_dir: exe_dir.to_path_buf() } }
            else if let Some(d) = other_install { WinPlan::Setup { install_dir: d } }
            else { WinPlan::Reveal("update.why_portable_exe") }
        }
        "zip" => WinPlan::Portable { dir: exe_dir.to_path_buf() },
        _ => WinPlan::Reveal("update.why_other"),
    }
}

/// 注册表里的安装目录（安装包写入）。
#[cfg(windows)]
pub fn registry_install_dir() -> Option<PathBuf> { crate::proxy::read_reg_sz("Software\\ShellAX", "InstallDir").filter(|s| !s.is_empty()).map(PathBuf::from) }

/// 更新记录文件：助手脚本每次追加一行 `时间\t状态\t版本\t说明`。
pub fn log_path(data_dir: &Path) -> PathBuf { updates_dir(data_dir).join("update.log") }

/// 上次更新失败（目标版本仍比当前新）时的说明；读取后把日志改名，只提示一次。
pub fn take_failure(data_dir: &Path) -> Option<String> {
    let p = log_path(data_dir);
    let text = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::rename(&p, p.with_file_name("update.last.log"));
    last_failure(&text, CURRENT)
}

/// 日志里最后一条状态为 FAIL 且目标版本比 `current` 新的说明。
pub fn last_failure(log: &str, current: &str) -> Option<String> {
    let last = log.lines().filter(|l| !l.trim().is_empty()).last()?;
    let mut it = last.splitn(4, '\t');
    let (_ts, st, ver, msg) = (it.next()?, it.next()?, it.next()?, it.next().unwrap_or(""));
    (st == "FAIL" && is_newer(ver, current)).then(|| msg.trim().to_string())
}

/// Windows 助手脚本（PowerShell，UTF-8 带 BOM 写入）：等应用进程退出 → 安装或替换文件 → 记录结果 → 重新启动。
pub const WIN_HELPER: &str = r#"param([int]$AppPid,[string]$Mode,[string]$Source,[string]$Dir,[string]$Exe,[string]$Log,[string]$Version,[string]$Extra)
$ErrorActionPreference = 'Stop'
function Note($st, $msg) { try { Add-Content -LiteralPath $Log -Encoding UTF8 -Value ("{0}`t{1}`t{2}`t{3}" -f (Get-Date -Format s), $st, $Version, ($msg -replace "[
]+", ' ')) } catch {} }
try {
  try { Wait-Process -Id $AppPid -Timeout 60 -ErrorAction SilentlyContinue } catch {}
  Start-Sleep -Milliseconds 500
  if ($Mode -eq 'setup') {
    $a = '/S'
    if ($Extra) { $a = $a + ' /D=' + $Extra }
    $p = Start-Process -FilePath $Source -ArgumentList $a -PassThru
    $p.WaitForExit()
    $code = $p.ExitCode
    $err = Join-Path $env:TEMP 'shellax-setup-error.log'
    $detail = ''
    if (Test-Path -LiteralPath $err) { $detail = (Get-Content -LiteralPath $err -Raw); Remove-Item -LiteralPath $err -ErrorAction SilentlyContinue }
    if ($code -ne 0) { Note 'FAIL' ("安装程序退出码 $code $detail"); exit 1 }
  } else {
    $work = Join-Path (Split-Path -Parent $Source) 'extracted'
    if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
    Expand-Archive -LiteralPath $Source -DestinationPath $work -Force
    $new = Get-ChildItem -LiteralPath $work -Recurse -Filter 'shellax.exe' | Select-Object -First 1
    if (-not $new) { Note 'FAIL' '压缩包里没有 shellax.exe'; exit 1 }
    foreach ($f in 'shellax.exe', 'shellax-cli.exe') {
      $src = Join-Path $new.DirectoryName $f
      if (-not (Test-Path -LiteralPath $src)) { continue }
      $ok = $false
      for ($i = 0; $i -lt 60 -and -not $ok; $i++) {
        try { Copy-Item -LiteralPath $src -Destination (Join-Path $Dir $f) -Force; $ok = $true } catch { Start-Sleep -Milliseconds 500 }
      }
      if (-not $ok) { Note 'FAIL' "无法替换 $f（文件被占用或没有写入权限）"; exit 1 }
    }
  }
  Note 'OK' '更新完成'
  if (Test-Path -LiteralPath $Exe) { Start-Process -FilePath $Exe }
} catch {
  Note 'FAIL' $_.Exception.Message
  exit 1
}
"#;

#[cfg(windows)]
fn spawn_win(mut cmd: std::process::Command, flags: u32) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // 应用运行在不允许脱离的作业里时，带 BREAKAWAY 会被拒绝（拒绝访问）：去掉后再试一次
    match cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn() {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(5) => { cmd.creation_flags(flags).spawn().map(|_| ()) }
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
fn apply_windows(rec: &Record, exe: &Path, data: &Path) -> Result<Apply> {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt;
    let reg = registry_install_dir();
    let plan = win_plan(&rec.file, exe, reg.as_deref(), &|d, n| d.join(n).is_file());
    let (mode, dir, extra) = match &plan {
        WinPlan::Reveal(why) => { reveal(&rec.file)?; return Ok(Apply::Revealed(*why)); }
        WinPlan::Setup { install_dir } => ("setup", install_dir.clone(), if reg.as_deref().is_some_and(|r| same_dir(r, install_dir)) { String::new() } else { install_dir.display().to_string() }),
        WinPlan::Portable { dir } => ("portable", dir.clone(), String::new()),
    };
    let work = updates_dir(data);
    std::fs::create_dir_all(&work)?;
    let script = work.join("update.ps1");
    let mut body = vec![0xEF, 0xBB, 0xBF];   // PowerShell 5.1 只在有 BOM 时按 UTF-8 读取脚本
    body.extend_from_slice(WIN_HELPER.as_bytes());
    std::fs::write(&script, body).context("write update helper")?;
    let target_exe = if mode == "setup" { dir.join("shellax.exe") } else { exe.to_path_buf() };
    let mut ps = std::process::Command::new("powershell.exe");
    ps.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"]).arg(&script)
        .arg("-AppPid").arg(std::process::id().to_string()).arg("-Mode").arg(mode)
        .arg("-Source").arg(&rec.file).arg("-Dir").arg(&dir).arg("-Exe").arg(&target_exe)
        .arg("-Log").arg(log_path(data)).arg("-Version").arg(&rec.version);
    if !extra.is_empty() { ps.arg("-Extra").arg(&extra); }
    // 助手独立于本进程：没有控制台窗口、自己的进程组、尽量脱离作业
    match spawn_win(ps, CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP) {
        Ok(()) => return Ok(Apply::QuitNow),
        Err(e) => eprintln!("update: helper did not start: {e}"),
    }
    // 助手起不来（PowerShell 被禁用等）：安装版直接运行安装包，便携版退回手动
    if mode == "setup" {
        let mut c = std::process::Command::new(&rec.file);
        c.arg("/S").arg("/RESTART");
        if !extra.is_empty() { c.raw_arg(format!("/D={extra}")); }
        spawn_win(c, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP).context("start installer")?;
        return Ok(Apply::QuitNow);
    }
    reveal(&rec.file)?;
    Ok(Apply::Revealed("update.why_helper"))
}

/// 启动安装：Windows 由 PowerShell 助手等应用退出后静默安装（或替换便携版 exe）并重启，结果写入 updates\update.log。
/// 另一平台解压 zip，用脚本等待本进程退出后替换 .app、去掉隔离属性并重新打开；
/// 没有写入权限或解压失败时只显示安装包。
pub fn apply(rec: &Record, data_dir: &Path) -> Result<Apply> {
    let _ = data_dir;
    if !rec.file.is_file() { bail!("installer not found: {}", rec.file.display()); }
    let exe = std::env::current_exe()?;
    #[cfg(windows)]
    { return apply_windows(rec, &exe, data_dir); }
    #[cfg(target_os = "macos")]
    {
        let app = exe.ancestors().find(|a| a.extension().is_some_and(|x| x == "app")).map(Path::to_path_buf);
        let fallback = |rec: &Record| -> Result<Apply> {
            if rec.file.extension().is_some_and(|x| x == "dmg") { std::process::Command::new("open").arg(&rec.file).spawn()?; } else { reveal(&rec.file)?; }
            Ok(Apply::Revealed("update.why_other"))
        };
        let Some(app) = app else { return fallback(rec) };
        if rec.file.extension().is_none_or(|x| x != "zip") { return fallback(rec); }
        let parent = app.parent().ok_or_else(|| anyhow!("no app dir"))?.to_path_buf();
        // 写入权限：在 .app 所在目录试建一个临时文件
        let probe = parent.join(format!(".shellax-update-{}", std::process::id()));
        if std::fs::write(&probe, b"").is_err() { return fallback(rec); }
        let _ = std::fs::remove_file(&probe);
        let work = rec.file.with_extension("extracted");
        let _ = std::fs::remove_dir_all(&work);
        let ok = std::process::Command::new("ditto").args(["-x", "-k"]).arg(&rec.file).arg(&work).status().is_ok_and(|s| s.success());
        let new_app = std::fs::read_dir(&work).ok().and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "app")));
        let Some(new_app) = new_app.filter(|_| ok) else { return fallback(rec) };
        let script = work.join("replace.sh");
        let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        let body = format!(r#"#!/bin/sh
PID={pid}; APP={app}; NEW={new}; OLD={app_old}
i=0; while kill -0 "$PID" 2>/dev/null && [ $i -lt 120 ]; do sleep 0.5; i=$((i+1)); done
rm -rf "$OLD"
if mv "$APP" "$OLD" && ditto "$NEW" "$APP"; then
  xattr -dr com.apple.quarantine "$APP" 2>/dev/null
  rm -rf "$OLD"
  open "$APP"
else
  [ -d "$OLD" ] && [ ! -d "$APP" ] && mv "$OLD" "$APP"
  open -R "$NEW"
fi
"#, pid = std::process::id(), app = q(&app), new = q(&new_app), app_old = q(&parent.join(format!(".{}.old", app.file_name().unwrap_or_default().to_string_lossy()))));
        std::fs::write(&script, body)?;
        std::process::Command::new("/bin/sh").arg(&script).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().context("start updater script")?;
        return Ok(Apply::QuitNow);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = exe;
        reveal(&rec.file)?;
        Ok(Apply::Revealed("update.why_other"))
    }
}

/// 共享给界面的下载状态。
#[derive(Clone)]
pub enum Phase {
    Idle,
    Downloading { version: String, progress: Arc<Progress> },
    Failed { version: String, error: String },
    Ready(Record),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn win_plans() {
        let has = |d: &Path, n: &str| { let d = d.to_string_lossy(); (d == "C:/Apps/ShellAX" && n == "Uninstall.exe") || (d == "D:/中文 目录/ShellAX" && n == "shellax.exe") };
        let setup = Path::new("C:/u/ShellAX-Setup-x64.exe");
        let zip = Path::new("C:/u/ShellAX-portable-x64.zip");
        let inst = Path::new("C:/Apps/ShellAX/shellax.exe");
        let port = Path::new("E:/tmp/shellax.exe");
        // 运行的是安装版
        assert_eq!(win_plan(setup, inst, None, &has), WinPlan::Setup { install_dir: "C:/Apps/ShellAX".into() });
        // 只有注册表指向运行目录（Uninstall.exe 丢了）
        assert_eq!(win_plan(setup, inst, Some(Path::new("c:/apps/shellax/")), &|_, _| false), WinPlan::Setup { install_dir: "C:/Apps/ShellAX".into() });
        // 运行的是别处的副本，但装过安装版（含中文和空格的目录）
        assert_eq!(win_plan(setup, port, Some(Path::new("D:/中文 目录/ShellAX")), &has), WinPlan::Setup { install_dir: "D:/中文 目录/ShellAX".into() });
        // 便携副本 + 安装包，没有安装版：只能手动
        assert_eq!(win_plan(setup, port, None, &has), WinPlan::Reveal("update.why_portable_exe"));
        // zip 替换运行副本所在目录
        assert_eq!(win_plan(zip, port, None, &has), WinPlan::Portable { dir: "E:/tmp".into() });
        assert_eq!(win_plan(Path::new("x.msi"), inst, None, &has), WinPlan::Reveal("update.why_other"));
    }
    #[test]
    fn failure_log() {
        let log = "2026-10-27T09:00:00\tOK\t0.2.2\t更新完成\n2026-10-27T09:05:00\tFAIL\t0.2.3\t安装程序退出码 2\n";
        assert_eq!(last_failure(log, "0.2.2").as_deref(), Some("安装程序退出码 2"));
        assert_eq!(last_failure(log, "0.2.3"), None);   // 版本已是目标版本
        assert_eq!(last_failure("2026\tOK\t0.2.3\tx\n", "0.2.2"), None);
        assert_eq!(last_failure("", "0.2.2"), None);
    }
    #[test]
    fn helper_script_has_no_unbalanced_braces() {
        assert_eq!(WIN_HELPER.matches('{').count(), WIN_HELPER.matches('}').count());
        assert!(WIN_HELPER.contains("Wait-Process"));
    }
    #[test]
    fn sums_and_urls() {
        let s = "abc  x\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789ABCDEF  ShellAX-Setup-x64.exe\n";
        assert_eq!(sum_for(s, "ShellAX-Setup-x64.exe").unwrap(), "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        assert!(sum_for(s, "x").is_none());
        assert_eq!(sums_url("https://github.com/a/b/releases/download/v1/ShellAX-Setup-x64.exe").unwrap(), "https://github.com/a/b/releases/download/v1/SHA256SUMS.txt");
        assert!(sums_url("https://example.com/x.exe").is_none());
    }
}
