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
    let part = dir.join(format!("{name}.part"));
    let expected = expected_sha(m, &url)?;
    let res = retry(Some(p), || {
        let r = agent(&url, Duration::from_secs(60))?.get(&url).set("User-Agent", &format!("shellax/{CURRENT}")).call().map_err(http_err)?;
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
        if exp != got {
            let _ = std::fs::remove_file(&part);
            bail!("checksum mismatch (expected {exp}, got {got})");
        }
    }
    std::fs::rename(&part, &dest)?;
    let rec = Record { version: m.version.clone(), file: dest, size, downloaded_at: chrono::Utc::now().timestamp() };
    save_record(data_dir, Some(&rec))?;
    Ok(rec)
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
    Revealed,
}

/// 启动安装：Windows 安装版静默安装并在结束后重启；便携版只显示文件。
/// 另一平台解压 zip，用脚本等待本进程退出后替换 .app、去掉隔离属性并重新打开；
/// 没有写入权限或解压失败时只显示安装包。
pub fn apply(rec: &Record) -> Result<Apply> {
    if !rec.file.is_file() { bail!("installer not found: {}", rec.file.display()); }
    let exe = std::env::current_exe()?;
    #[cfg(windows)]
    {
        let dir = exe.parent().ok_or_else(|| anyhow!("no exe dir"))?;
        let installed = dir.join("Uninstall.exe").is_file();
        let is_setup = rec.file.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"));
        if installed && is_setup {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            std::process::Command::new(&rec.file).args(["/S", "/RESTART"])
                .creation_flags(DETACHED_PROCESS).spawn().context("start installer")?;
            return Ok(Apply::QuitNow);
        }
        reveal(&rec.file)?;
        return Ok(Apply::Revealed);
    }
    #[cfg(target_os = "macos")]
    {
        let app = exe.ancestors().find(|a| a.extension().is_some_and(|x| x == "app")).map(Path::to_path_buf);
        let fallback = |rec: &Record| -> Result<Apply> {
            if rec.file.extension().is_some_and(|x| x == "dmg") { std::process::Command::new("open").arg(&rec.file).spawn()?; } else { reveal(&rec.file)?; }
            Ok(Apply::Revealed)
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
        Ok(Apply::Revealed)
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
    fn sums_and_urls() {
        let s = "abc  x\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789ABCDEF  ShellAX-Setup-x64.exe\n";
        assert_eq!(sum_for(s, "ShellAX-Setup-x64.exe").unwrap(), "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        assert!(sum_for(s, "x").is_none());
        assert_eq!(sums_url("https://github.com/a/b/releases/download/v1/ShellAX-Setup-x64.exe").unwrap(), "https://github.com/a/b/releases/download/v1/SHA256SUMS.txt");
        assert!(sums_url("https://example.com/x.exe").is_none());
    }
}
