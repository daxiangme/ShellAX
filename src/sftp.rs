//! SFTP backend: remote/local listings, file operations and a resumable transfer queue.
//! Used by the dual-pane SFTP panel; the remote side runs over the shared SSH `Conn`.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry { pub name: String, pub is_dir: bool, pub size: u64, pub mode: u32, pub mtime: u64 }

fn sort(v: &mut [Entry]) { v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))); }

pub fn join(dir: &str, name: &str) -> String {
    if name == ".." { return parent(dir); }
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}
pub fn parent(p: &str) -> String {
    let t = p.trim_end_matches('/');
    match t.rfind('/') { Some(0) | None => "/".into(), Some(i) => t[..i].into() }
}

pub fn list_local(dir: &Path, hidden: bool) -> Result<Vec<Entry>> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !hidden && name.starts_with('.') { continue; }
        let md = std::fs::metadata(e.path()).or_else(|_| e.metadata());
        let (is_dir, size, mode, mtime) = match md {
            Ok(m) => {
                #[cfg(unix)] let mode = { use std::os::unix::fs::PermissionsExt; m.permissions().mode() & 0o7777 };
                #[cfg(not(unix))] let mode = 0;
                (m.is_dir(), m.len(), mode, m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0))
            }
            Err(_) => (false, 0, 0, 0),
        };
        v.push(Entry { name, is_dir, size, mode, mtime });
    }
    sort(&mut v);
    Ok(v)
}

pub async fn list_remote(s: &SftpSession, dir: &str, hidden: bool) -> Result<Vec<Entry>> {
    let mut v = Vec::new();
    for e in s.read_dir(dir).await? {
        let name = e.file_name();
        if name == "." || name == ".." || (!hidden && name.starts_with('.')) { continue; }
        let mut m = e.metadata();
        if m.file_type().is_symlink() { if let Ok(t) = s.metadata(join(dir, &name)).await { m = t; } }
        v.push(Entry { name, is_dir: m.is_dir(), size: m.size.unwrap_or(0), mode: m.permissions.unwrap_or(0) & 0o7777, mtime: m.mtime.unwrap_or(0) as u64 });
    }
    sort(&mut v);
    Ok(v)
}

/// Which side an operation targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side { Local, Remote }

pub async fn mkdir(s: Option<&SftpSession>, side: Side, path: &str) -> Result<()> {
    match side { Side::Local => Ok(std::fs::create_dir(path)?), Side::Remote => Ok(s.ok_or_else(no_sftp)?.create_dir(path).await?) }
}
pub async fn rename(s: Option<&SftpSession>, side: Side, from: &str, to: &str) -> Result<()> {
    match side { Side::Local => Ok(std::fs::rename(from, to)?), Side::Remote => Ok(s.ok_or_else(no_sftp)?.rename(from, to).await?) }
}
pub async fn chmod(s: Option<&SftpSession>, side: Side, path: &str, mode: u32) -> Result<()> {
    match side {
        Side::Local => {
            #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?; }
            Ok(())
        }
        Side::Remote => {
            let attrs = FileAttributes { size: None, uid: None, user: None, gid: None, group: None, permissions: Some(mode), atime: None, mtime: None };
            Ok(s.ok_or_else(no_sftp)?.set_metadata(path, attrs).await?)
        }
    }
}
/// Delete a file or a directory tree.
pub fn remove<'a>(s: Option<&'a SftpSession>, side: Side, path: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        match side {
            Side::Local => { let p = Path::new(path); if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(p)? } else { std::fs::remove_file(p)? } Ok(()) }
            Side::Remote => {
                let s = s.ok_or_else(no_sftp)?;
                let m = s.symlink_metadata(path).await?;
                if m.is_dir() {
                    for e in s.read_dir(path).await? {
                        let n = e.file_name();
                        if n == "." || n == ".." { continue; }
                        remove(Some(s), side, &join(path, &n)).await?;
                    }
                    s.remove_dir(path).await?;
                } else { s.remove_file(path).await?; }
                Ok(())
            }
        }
    })
}
fn no_sftp() -> anyhow::Error { anyhow!("no SFTP session") }

/// "rwxr-xr-x" style rendering.
/// Largest file the built-in editor opens (PRD 3.5 P1 "内置编辑").
pub const EDIT_MAX: u64 = 2 << 20;

/// Remote file version used to detect concurrent changes: (size, mtime).
pub type Version = (u64, u64);

async fn version(s: &SftpSession, path: &str) -> Result<Version> {
    let m = s.metadata(path.to_string()).await?;
    Ok((m.size.unwrap_or(0), m.mtime.unwrap_or(0) as u64))
}

/// Open a remote text file for editing: UTF-8 only, at most [`EDIT_MAX`] bytes.
pub async fn read_text(s: &SftpSession, path: &str) -> Result<(String, Version)> {
    let v = version(s, path).await?;
    if v.0 > EDIT_MAX { return Err(anyhow!("file is {} bytes; the built-in editor opens files up to {} bytes", v.0, EDIT_MAX)); }
    let mut f = s.open_with_flags(path.to_string(), OpenFlags::READ).await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;
    if buf.contains(&0) { return Err(anyhow!("binary file (NUL bytes); not opened in the editor")); }
    let text = String::from_utf8(buf).map_err(|_| anyhow!("not valid UTF-8; not opened in the editor"))?;
    Ok((text, v))
}

/// Save edited text. With `expect`, refuses when the file changed on the server since it was
/// opened (size or mtime differ), so another writer's change is not silently overwritten.
pub async fn write_text(s: &SftpSession, path: &str, text: &str, expect: Option<Version>) -> Result<Version> {
    if let Some(e) = expect {
        if let Ok(now) = version(s, path).await { if now != e { return Err(anyhow!("{path} changed on the server since it was opened (size/mtime {:?} → {:?}); reopen or save again to overwrite", e, now)); } }
    }
    let mut f = s.open_with_flags(path.to_string(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE).await?;
    f.write_all(text.as_bytes()).await?;
    f.shutdown().await?;
    drop(f);
    version(s, path).await
}

pub fn mode_string(m: u32) -> String {
    let b = |bit: u32, ch: char| if m & bit != 0 { ch } else { '-' };
    [b(0o400,'r'),b(0o200,'w'),b(0o100,'x'),b(0o40,'r'),b(0o20,'w'),b(0o10,'x'),b(0o4,'r'),b(0o2,'w'),b(0o1,'x')].iter().collect()
}
pub fn parse_mode(s: &str) -> Option<u32> { let s = s.trim(); (!s.is_empty() && s.len() <= 4).then(|| u32::from_str_radix(s, 8).ok()).flatten().filter(|m| *m <= 0o7777) }

// ---------------------------------------------------------------- transfer queue

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir { Upload, Download }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum JobState { Queued = 0, Running = 1, Done = 2, Failed = 3, Paused = 4 }

pub struct Job {
    pub id: u64,
    pub dir: Dir,
    pub local: PathBuf,
    pub remote: String,
    pub total: AtomicU64,
    pub done: AtomicU64,
    /// Bytes already present when the job started (resumed from).
    pub resumed_from: AtomicU64,
    state: AtomicU8,
    cancel: AtomicBool,
    pub error: Mutex<Option<String>>,
}
impl Job {
    pub fn state(&self) -> JobState { match self.state.load(Ordering::Acquire) { 0 => JobState::Queued, 1 => JobState::Running, 2 => JobState::Done, 3 => JobState::Failed, _ => JobState::Paused } }
    fn set(&self, s: JobState) { self.state.store(s as u8, Ordering::Release); }
    pub fn name(&self) -> String { self.local.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }
    pub fn progress(&self) -> f32 { let t = self.total.load(Ordering::Relaxed); if t == 0 { if self.state() == JobState::Done { 1.0 } else { 0.0 } } else { self.done.load(Ordering::Relaxed) as f32 / t as f32 } }
}

const CHUNK: usize = 64 * 1024;

/// Copy with resume: if the destination already has `n` bytes (n <= source size), continue at n.
pub async fn run_job(s: &SftpSession, job: &Job) -> Result<()> {
    job.cancel.store(false, Ordering::Release);
    job.set(JobState::Running);
    let r = async {
        let mut buf = vec![0u8; CHUNK];
        match job.dir {
            Dir::Upload => {
                let mut src = tokio::fs::File::open(&job.local).await?;
                let total = src.metadata().await?.len();
                job.total.store(total, Ordering::Relaxed);
                let have = s.metadata(job.remote.clone()).await.ok().and_then(|m| m.size).unwrap_or(0);
                let off = if have <= total { have } else { 0 };
                let mut dst = if off > 0 { s.open_with_flags(job.remote.clone(), OpenFlags::WRITE).await? } else { s.open_with_flags(job.remote.clone(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE).await? };
                src.seek(std::io::SeekFrom::Start(off)).await?;
                dst.seek(std::io::SeekFrom::Start(off)).await?;
                job.resumed_from.store(off, Ordering::Relaxed);
                job.done.store(off, Ordering::Relaxed);
                loop {
                    if job.cancel.load(Ordering::Acquire) { dst.shutdown().await.ok(); return Ok(false); }
                    let n = src.read(&mut buf).await?;
                    if n == 0 { break; }
                    dst.write_all(&buf[..n]).await?;
                    job.done.fetch_add(n as u64, Ordering::Relaxed);
                }
                dst.shutdown().await?;
                // keep the source mtime so later syncs see the pair as unchanged
                if let Some(mt) = std::fs::metadata(&job.local).ok().and_then(|m| m.modified().ok()).and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()) {
                    let t = mt.as_secs() as u32;
                    let _ = s.set_metadata(job.remote.clone(), FileAttributes { size: None, uid: None, user: None, gid: None, group: None, permissions: None, atime: Some(t), mtime: Some(t) }).await;
                }
            }
            Dir::Download => {
                let mut src = s.open_with_flags(job.remote.clone(), OpenFlags::READ).await?;
                let total = src.metadata().await?.size.unwrap_or(0);
                job.total.store(total, Ordering::Relaxed);
                let have = std::fs::metadata(&job.local).map(|m| m.len()).unwrap_or(0);
                let off = if have <= total { have } else { 0 };
                let mut dst = tokio::fs::OpenOptions::new().write(true).create(true).truncate(off == 0).open(&job.local).await?;
                src.seek(std::io::SeekFrom::Start(off)).await?;
                dst.seek(std::io::SeekFrom::Start(off)).await?;
                job.resumed_from.store(off, Ordering::Relaxed);
                job.done.store(off, Ordering::Relaxed);
                loop {
                    if job.cancel.load(Ordering::Acquire) { dst.flush().await.ok(); return Ok(false); }
                    let n = src.read(&mut buf).await?;
                    if n == 0 { break; }
                    dst.write_all(&buf[..n]).await?;
                    job.done.fetch_add(n as u64, Ordering::Relaxed);
                }
                dst.flush().await?;
                drop(dst);
                if let Some(mt) = s.metadata(job.remote.clone()).await.ok().and_then(|m| m.mtime) {
                    if let Ok(f) = std::fs::File::options().write(true).open(&job.local) { let _ = f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mt as u64)); }
                }
            }
        }
        Ok::<bool, anyhow::Error>(true)
    }.await;
    match r {
        Ok(true) => { job.set(JobState::Done); Ok(()) }
        Ok(false) => { job.set(JobState::Paused); Ok(()) }
        Err(e) => { *job.error.lock().unwrap() = Some(format!("{e:#}")); job.set(JobState::Failed); Err(e) }
    }
}

/// Recursive remote listing for directory sync (relative paths; symlinks skipped).
pub async fn scan_remote(s: &SftpSession, root: &str) -> Result<crate::dirsync::Tree> {
    let mut t = crate::dirsync::Tree::new(); let mut stack = vec![(root.to_string(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in s.read_dir(dir.clone()).await? {
            let name = e.file_name();
            if name == "." || name == ".." { continue; }
            let m = e.metadata();
            if m.file_type().is_symlink() { continue; }
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            t.insert(r.clone(), crate::dirsync::FileInfo { size: if m.is_dir() { 0 } else { m.size.unwrap_or(0) }, mtime: m.mtime.unwrap_or(0) as u64, is_dir: m.is_dir() });
            if m.is_dir() { stack.push((join(&dir, &name), r)); }
            if t.len() > crate::dirsync::MAX_ENTRIES { return Err(anyhow!("more than {} entries under {root}; pick a smaller directory", crate::dirsync::MAX_ENTRIES)); }
        }
    }
    Ok(t)
}
/// Execute a sync plan: mkdirs and deletes run now; transfers go to `queue` (destination
/// file removed first so the resumable queue never appends to stale content). Conflicts are skipped.
/// Returns (queued transfers, deletes, skipped conflicts).
pub async fn execute_sync(s: &SftpSession, queue: &Queue, local_root: &Path, remote_root: &str, ops: &[crate::dirsync::Op]) -> Result<(usize, usize, usize)> {
    use crate::dirsync::Op;
    let (mut q, mut d, mut c) = (0, 0, 0);
    for op in ops {
        let (lp, rp) = (local_root.join(op.path()), join(remote_root, op.path()));
        match op {
            Op::MkdirRemote(_) => { let _ = s.create_dir(rp).await; }
            Op::MkdirLocal(_) => std::fs::create_dir_all(&lp)?,
            Op::Upload(_) => { let _ = s.remove_file(rp.clone()).await; queue.push(Dir::Upload, lp, rp); q += 1; }
            Op::Download(_) => { if let Some(p) = lp.parent() { std::fs::create_dir_all(p)?; } let _ = std::fs::remove_file(&lp); queue.push(Dir::Download, lp, rp); q += 1; }
            Op::DeleteRemote(_) => { remove(Some(s), Side::Remote, &rp).await?; d += 1; }
            Op::DeleteLocal(_) => { remove(None, Side::Local, &lp.to_string_lossy()).await?; d += 1; }
            Op::Conflict(_) => c += 1,
        }
    }
    Ok((q, d, c))
}

/// Sequential transfer queue on one SFTP session. Paused/failed jobs can be resumed.
#[derive(Clone, Default)]
pub struct Queue { pub jobs: Arc<Mutex<Vec<Arc<Job>>>>, next: Arc<AtomicU64>, wake: Arc<tokio::sync::Notify>, started: Arc<AtomicBool> }

impl Queue {
    pub fn push(&self, dir: Dir, local: PathBuf, remote: String) -> Arc<Job> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let j = Arc::new(Job { id, dir, local, remote, total: 0.into(), done: 0.into(), resumed_from: 0.into(), state: AtomicU8::new(0), cancel: false.into(), error: Mutex::new(None) });
        self.jobs.lock().unwrap().push(j.clone());
        self.wake.notify_one();
        j
    }
    /// Recursively queue a local directory (upload) — creates remote dirs first.
    pub async fn push_upload_tree(&self, s: &SftpSession, local: &Path, remote: &str) -> Result<usize> {
        if !local.is_dir() { self.push(Dir::Upload, local.into(), remote.into()); return Ok(1); }
        let _ = s.create_dir(remote).await;
        let mut n = 0;
        for e in std::fs::read_dir(local)?.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            n += Box::pin(self.push_upload_tree(s, &e.path(), &join(remote, &name))).await?;
        }
        Ok(n)
    }
    pub async fn push_download_tree(&self, s: &SftpSession, remote: &str, local: &Path) -> Result<usize> {
        if !s.metadata(remote).await?.is_dir() { self.push(Dir::Download, local.into(), remote.into()); return Ok(1); }
        std::fs::create_dir_all(local)?;
        let mut n = 0;
        for e in s.read_dir(remote).await? {
            let name = e.file_name();
            if name == "." || name == ".." { continue; }
            n += Box::pin(self.push_download_tree(s, &join(remote, &name), &local.join(&name))).await?;
        }
        Ok(n)
    }
    pub fn pause(&self, id: u64) { if let Some(j) = self.jobs.lock().unwrap().iter().find(|j| j.id == id) { j.cancel.store(true, Ordering::Release); if j.state() == JobState::Queued { j.set(JobState::Paused); } } }
    pub fn resume(&self, id: u64) { if let Some(j) = self.jobs.lock().unwrap().iter().find(|j| j.id == id) { if matches!(j.state(), JobState::Paused | JobState::Failed) { *j.error.lock().unwrap() = None; j.set(JobState::Queued); self.wake.notify_one(); } } }
    pub fn clear_done(&self) { self.jobs.lock().unwrap().retain(|j| j.state() != JobState::Done); }
    fn next_job(&self) -> Option<Arc<Job>> { self.jobs.lock().unwrap().iter().find(|j| j.state() == JobState::Queued).cloned() }
    /// Run until all queued jobs are done (for tests / one-shot use).
    pub async fn drain(&self, s: &SftpSession) { while let Some(j) = self.next_job() { let _ = run_job(s, &j).await; } }
    /// Start the background worker (idempotent).
    pub fn start(&self, rt: &tokio::runtime::Handle, s: Arc<SftpSession>, poke: impl Fn() + Send + 'static) {
        if self.started.swap(true, Ordering::AcqRel) { return; }
        let q = self.clone();
        rt.spawn(async move {
            loop {
                match q.next_job() {
                    Some(j) => { let _ = run_job(&s, &j).await; poke(); }
                    None => q.wake.notified().await,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths() {
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(join("/", "b"), "/b");
        assert_eq!(join("/a/b", ".."), "/a");
        assert_eq!(parent("/a"), "/");
        assert_eq!(mode_string(0o754), "rwxr-xr--");
        assert_eq!(parse_mode("644"), Some(0o644));
        assert_eq!(parse_mode("9"), None);
    }
    #[test]
    fn local_list_sorted() {
        let d = std::env::temp_dir().join(format!("stp-sftp-{}", std::process::id()));
        let _ = std::fs::create_dir_all(d.join("zdir"));
        std::fs::write(d.join("a.txt"), "x").unwrap();
        let v = list_local(&d, false).unwrap();
        assert_eq!(v[0].name, "zdir");
        assert!(v[0].is_dir && !v[1].is_dir);
        let _ = std::fs::remove_dir_all(&d);
    }
}
