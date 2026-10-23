//! 应用内更新的界面状态：下载进度、失败原因、已下载的安装包，以及“下载完成”提示。
//! 下载在后台线程进行；主窗口定时刷新，设置页和提示框都从这里读取。
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use ssh_term_proto::update::Manifest;
use ssh_term_proto::updater::{self, Phase, Progress, Record};

static PHASE: LazyLock<Mutex<Phase>> = LazyLock::new(|| Mutex::new(Phase::Idle));
/// 最近一次要下载的版本信息（失败后重试用）。
static LAST: Mutex<Option<Manifest>> = Mutex::new(None);
/// 等待用户处理的“下载完成”提示。
static PROMPT: Mutex<Option<Record>> = Mutex::new(None);
/// 状态变化计数，主窗口据此决定是否刷新。
pub static GEN: AtomicU64 = AtomicU64::new(0);

fn bump() { GEN.fetch_add(1, Ordering::Relaxed); }

/// 启动时读取下载记录；已安装同版本或更新版本时清理旧安装包。
pub fn init() {
    if let Some(r) = updater::load_and_cleanup(&crate::data_dir()) { *PHASE.lock().unwrap() = Phase::Ready(r); bump(); }
}

pub fn phase() -> Phase { PHASE.lock().unwrap().clone() }
pub fn last() -> Option<Manifest> { LAST.lock().unwrap().clone() }
pub fn prompt() -> Option<Record> { PROMPT.lock().unwrap().clone() }
pub fn dismiss_prompt() { *PROMPT.lock().unwrap() = None; bump(); }
pub fn downloading() -> bool { matches!(*PHASE.lock().unwrap(), Phase::Downloading { .. }) }

/// 开始后台下载。正在下载或该版本已下载好时不重复下载。`prompt` 为 true 时完成后弹出提示。
pub fn start(m: Manifest, prompt: bool) {
    {
        let mut ph = PHASE.lock().unwrap();
        match &*ph {
            Phase::Downloading { .. } => return,
            Phase::Ready(r) if r.version == m.version && r.file.is_file() => {
                if prompt { *PROMPT.lock().unwrap() = Some(r.clone()); }
                bump();
                return;
            }
            _ => {}
        }
        let progress = Arc::new(Progress::default());
        *ph = Phase::Downloading { version: m.version.clone(), progress: progress.clone() };
        *LAST.lock().unwrap() = Some(m.clone());
        bump();
        std::thread::spawn(move || {
            let r = updater::download(&m, &crate::data_dir(), &progress);
            let mut ph = PHASE.lock().unwrap();
            *ph = match r {
                Ok(rec) => { if prompt { *PROMPT.lock().unwrap() = Some(rec.clone()); } Phase::Ready(rec) }
                Err(e) if progress.cancel.load(Ordering::Relaxed) => { let _ = e; Phase::Idle }
                Err(e) => Phase::Failed { version: m.version.clone(), error: format!("{e:#}") },
            };
            bump();
        });
    }
}

pub fn cancel() {
    if let Phase::Downloading { progress, .. } = &*PHASE.lock().unwrap() { progress.cancel.store(true, Ordering::Relaxed); }
}

pub fn retry() { if let Some(m) = last() { start(m, true); } }

/// 人类可读的字节数。
pub fn human(n: u64) -> String {
    let f = n as f64;
    if f >= 1024. * 1024. { format!("{:.1} MB", f / 1024. / 1024.) } else if f >= 1024. { format!("{:.0} KB", f / 1024.) } else { format!("{n} B") }
}

/// 下载时间（本地时区）。
pub fn when(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(ts, 0).single().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}
