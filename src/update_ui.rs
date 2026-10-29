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
/// 待用户处理的“发现新版本”通知（不自动消失）。
static OFFER: Mutex<Option<Manifest>> = Mutex::new(None);
/// 本次运行中用户选择“稍后”的版本，不再自动弹出。
static SNOOZED: Mutex<Option<String>> = Mutex::new(None);
pub fn offer(m: Manifest) {
    if SNOOZED.lock().unwrap().as_deref() == Some(m.version.as_str()) { return; }
    *OFFER.lock().unwrap() = Some(m); bump();
}
pub fn offered() -> Option<Manifest> { OFFER.lock().unwrap().clone() }
/// 关闭通知；`later` 为 true 时本次运行不再为该版本弹出。
pub fn dismiss_offer(later: bool) {
    if let Some(m) = OFFER.lock().unwrap().take() { if later { *SNOOZED.lock().unwrap() = Some(m.version); } }
    bump();
}
/// 已下载好的同版本安装包。
pub fn ready_for(v: &str) -> Option<Record> {
    match &*PHASE.lock().unwrap() { Phase::Ready(r) if r.version == v && r.file.is_file() => Some(r.clone()), _ => None }
}

static STARTED: Mutex<Option<std::time::Instant>> = Mutex::new(None);
static CHECKED: Mutex<Option<i64>> = Mutex::new(None);
/// 记录最近一次检查更新的时间。
pub fn mark_checked() { *CHECKED.lock().unwrap() = Some(chrono::Utc::now().timestamp()); }
pub fn last_checked() -> Option<i64> { *CHECKED.lock().unwrap() }
/// 下载速度（字节/秒）。
pub fn speed(done: u64) -> Option<u64> {
    let t = (*STARTED.lock().unwrap())?.elapsed().as_secs_f64();
    (t > 0.5).then(|| (done as f64 / t) as u64)
}

/// 把底层错误归类为一句简短的提示。
pub fn friendly(err: &str) -> String {
    let e = err.to_ascii_lowercase();
    let key = if e.contains("checksum mismatch") { "update.err_checksum" }
        else if e.contains("http 404") || e.contains("no entry for") { "update.err_missing" }
        else if e.contains("http 403") || e.contains("http 429") { "update.err_limited" }
        else if e.contains("no space") || e.contains("permission denied") || e.contains("os error 5") || e.contains("os error 112") { "update.err_disk" }
        else if e.contains("tls") || e.contains("connect") || e.contains("dns") || e.contains("timed out") || e.contains("timeout") || e.contains("resolve") || e.contains("os error 10060") || e.contains("proxy") || e.contains("incomplete") || e.contains("reset") { "update.err_network" }
        else { "update.err_generic" };
    ssh_term_proto::i18n::t(key).to_string()
}

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
        *STARTED.lock().unwrap() = Some(std::time::Instant::now());
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
