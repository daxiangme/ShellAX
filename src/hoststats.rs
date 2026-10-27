//! Live host metrics shared by the terminal path bar and the host tools panel.
//!
//! One sampler per host (keyed by the SSH pool key): a single exec reads `/proc/stat`, the
//! Monitor tab's data (load, memory, disks, cores, uptime), plus a tiny round-trip exec for latency.
//! Both consumers call [`refresh`]; a result younger than `max_age` is reused and a refresh that is
//! already running is awaited, so a host is never sampled twice at once. Local panes use
//! [`refresh_local`] (sysinfo). Non-Linux remotes only get latency.
use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::hosttools::{self, Tab};
use crate::ssh::Conn;

/// Seconds between samples of the active pane's host.
pub const POLL_SECS: u64 = 3;
/// A sample younger than this is reused instead of running the command again.
pub const FRESH_MS: u64 = 2500;
/// A latency probe slower than this counts as a timeout.
pub const LATENCY_TIMEOUT_SECS: u64 = 3;
/// Key of the local machine's entry.
pub const LOCAL_KEY: &str = "local";

/// `/proc/stat` first line, then exactly the Monitor tab's command (so the panel can parse the rest).
pub fn stats_command() -> String { format!("head -n1 /proc/stat 2>/dev/null; echo ---; {}", Tab::Monitor.read_command()) }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuTimes { pub total: u64, pub idle: u64 }

/// `cpu  user nice system idle iowait irq softirq steal guest guest_nice` (jiffies).
pub fn parse_cpu_line(line: &str) -> Option<CpuTimes> {
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" { return None; }
    let v: Vec<u64> = it.take(8).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 5 { return None; }
    Some(CpuTimes { total: v.iter().sum(), idle: v[3] + v[4] })
}

/// Busy share between two samples, 0–100. `None` when no time passed (or the counters went back).
pub fn cpu_pct(prev: CpuTimes, cur: CpuTimes) -> Option<f32> {
    let dt = cur.total.checked_sub(prev.total)?;
    if dt == 0 { return None; }
    let di = cur.idle.saturating_sub(prev.idle).min(dt);
    Some((100.0 * (1.0 - di as f64 / dt as f64)) as f32)
}

/// Split the combined output into the `/proc/stat` line and the Monitor text.
pub fn parse_stats(out: &str) -> (Option<CpuTimes>, &str) {
    match out.split_once("---\n") {
        Some((cpu, rest)) => (parse_cpu_line(cpu.trim()), rest),
        None => (None, ""),
    }
}

/// Median of the successful latencies.
pub fn median(vals: &[u32]) -> Option<u32> {
    if vals.is_empty() { return None; }
    let mut v = vals.to_vec();
    v.sort_unstable();
    Some(v[v.len() / 2])
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiskUse { pub mount: String, pub size_kb: u64, pub used_kb: u64, pub pct: u8 }

#[derive(Debug, Clone)]
pub struct HostStats {
    pub at: Instant,
    /// Local wall clock of the sample, `HH:MM:SS`.
    pub wall: String,
    pub local: bool,
    /// The host exposes `/proc` (or is the local machine): CPU / memory / disk / load are meaningful.
    pub linux: bool,
    pub cpu_pct: Option<f32>,
    pub cpus: u32,
    pub load: Option<[f32; 3]>,
    pub mem_total_kb: u64,
    pub mem_avail_kb: u64,
    pub disk: Option<DiskUse>,
    /// Median of the last three probes; `None` for local panes or when the last probe timed out.
    pub latency_ms: Option<u32>,
    pub timed_out: bool,
    /// Monitor tab text (load / memory / disks / cores / uptime) for the panel's parser.
    pub monitor_text: String,
}

impl HostStats {
    fn empty(local: bool) -> Self {
        Self { at: Instant::now(), wall: wall_clock(), local, linux: false, cpu_pct: None, cpus: 0, load: None, mem_total_kb: 0, mem_avail_kb: 0, disk: None, latency_ms: None, timed_out: false, monitor_text: String::new() }
    }
    pub fn has_metrics(&self) -> bool { self.local || self.linux }
    /// Memory in use as a 0–1 share.
    pub fn mem_frac(&self) -> Option<f32> {
        (self.mem_total_kb > 0).then(|| self.mem_total_kb.saturating_sub(self.mem_avail_kb) as f32 / self.mem_total_kb as f32)
    }
    /// 1-minute load per core as a 0–1+ share.
    pub fn load_frac(&self) -> Option<f32> { self.load.filter(|_| self.cpus > 0).map(|l| l[0] / self.cpus as f32) }
}

/// KiB as "1.5 GB" / "512 MB".
pub fn fmt_kb(n: u64) -> String { let g = n as f64 / 1048576.0; if g >= 1.0 { format!("{g:.1} GB") } else { format!("{:.0} MB", n as f64 / 1024.0) } }

fn wall_clock() -> String { chrono::Local::now().format("%H:%M:%S").to_string() }

#[derive(Default)]
struct Entry { cpu_prev: Option<CpuTimes>, lat: VecDeque<Option<u32>>, latest: Option<HostStats>, busy: bool }

static STATE: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(Default::default);

fn state() -> std::sync::MutexGuard<'static, HashMap<String, Entry>> { STATE.lock().unwrap_or_else(|e| e.into_inner()) }

/// Newest sample for `key`, if any.
pub fn latest(key: &str) -> Option<HostStats> { state().get(key).and_then(|e| e.latest.clone()) }

/// A refresh for `key` is running right now.
pub fn in_flight(key: &str) -> bool { state().get(key).is_some_and(|e| e.busy) }

/// Forget a host (its connection is gone).
pub fn clear(key: &str) { state().remove(key); }

/// Claim the right to sample `key`, or return a fresh enough sample. `Err(())` = another refresh is running.
fn claim(key: &str, max_age: Duration) -> Result<Option<HostStats>, ()> {
    let mut g = state();
    let e = g.entry(key.to_string()).or_default();
    if let Some(s) = &e.latest { if s.at.elapsed() < max_age { return Ok(Some(s.clone())); } }
    if e.busy { return Err(()); }
    e.busy = true;
    Ok(None)
}

fn release(key: &str) { if let Some(e) = state().get_mut(key) { e.busy = false; } }

/// Sample a remote host over its shared connection (or reuse a fresh sample).
pub async fn refresh(key: &str, conn: &Conn, max_age: Duration) -> Result<HostStats, String> {
    let waited = Instant::now();
    loop {
        match claim(key, max_age) {
            Ok(Some(s)) => return Ok(s),
            Ok(None) => break,
            Err(()) => {
                if waited.elapsed() > Duration::from_secs(20) { return Err("busy".into()); }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    let r = sample_remote(key, conn).await;
    release(key);
    r
}

async fn sample_remote(key: &str, conn: &Conn) -> Result<HostStats, String> {
    // latency: a tiny exec round trip, bounded
    let t0 = Instant::now();
    let probe = tokio::time::timeout(Duration::from_secs(LATENCY_TIMEOUT_SECS), conn.exec("echo 1", Duration::from_secs(LATENCY_TIMEOUT_SECS))).await;
    let ms = match probe {
        Ok(Ok(_)) => Some(t0.elapsed().as_millis().min(u32::MAX as u128) as u32),
        Ok(Err(e)) => return Err(format!("{e:#}")),
        Err(_) => None,
    };
    let (latency_ms, timed_out) = {
        let mut g = state();
        let e = g.entry(key.to_string()).or_default();
        e.lat.push_back(ms);
        while e.lat.len() > 3 { e.lat.pop_front(); }
        let ok: Vec<u32> = e.lat.iter().flatten().copied().collect();
        (if ms.is_some() { median(&ok) } else { None }, ms.is_none())
    };
    let prev = latest(key);
    if timed_out {
        // the link is stalled: keep the last numbers, flag the latency
        let mut s = prev.unwrap_or_else(|| HostStats::empty(false));
        s.at = Instant::now(); s.wall = wall_clock(); s.latency_ms = None; s.timed_out = true;
        store(key, s.clone(), None);
        return Ok(s);
    }
    let out = match tokio::time::timeout(Duration::from_secs(8), conn.exec(&stats_command(), Duration::from_secs(8))).await {
        Ok(Ok((_, o))) => String::from_utf8_lossy(&o).into_owned(),
        _ => String::new(),
    };
    let (cpu, mon_text) = parse_stats(&out);
    let mon = hosttools::parse_monitor(mon_text);
    let linux = cpu.is_some() && mon.mem_total_kb > 0;
    let cpu_prev = state().get(key).and_then(|e| e.cpu_prev);
    let pct = match (cpu_prev, cpu) { (Some(p), Some(c)) => cpu_pct(p, c), _ => None };
    let disk = mon.disks.iter().find(|d| d.mount == "/").or(mon.disks.first()).map(|d| DiskUse { mount: d.mount.clone(), size_kb: d.size_kb, used_kb: d.used_kb, pct: d.pct });
    let s = HostStats {
        at: Instant::now(), wall: wall_clock(), local: false, linux, cpu_pct: pct, cpus: mon.cpus,
        load: linux.then_some(mon.load), mem_total_kb: mon.mem_total_kb, mem_avail_kb: mon.mem_avail_kb, disk,
        latency_ms, timed_out: false, monitor_text: mon_text.to_string(),
    };
    store(key, s.clone(), cpu);
    Ok(s)
}

fn store(key: &str, s: HostStats, cpu: Option<CpuTimes>) {
    let mut g = state();
    let e = g.entry(key.to_string()).or_default();
    if cpu.is_some() { e.cpu_prev = cpu; }
    e.latest = Some(s);
}

/// Sample the local machine with sysinfo (blocking; run it off the UI thread).
pub fn refresh_local(max_age: Duration) -> HostStats {
    match claim(LOCAL_KEY, max_age) {
        Ok(Some(s)) => return s,
        Ok(None) => {}
        Err(()) => return latest(LOCAL_KEY).unwrap_or_else(|| HostStats::empty(true)),
    }
    let first = latest(LOCAL_KEY).is_none();
    let snap = crate::localstats::snapshot_light();
    let disk = crate::localstats::root_disk(&snap).map(|(mount, total, used)| DiskUse { mount, size_kb: total / 1024, used_kb: used / 1024, pct: (used as f64 / total.max(1) as f64 * 100.0).round().min(100.0) as u8 });
    let s = HostStats {
        at: Instant::now(), wall: wall_clock(), local: true, linux: false,
        cpu_pct: (!first).then_some(snap.cpu_pct.clamp(0.0, 100.0)), cpus: snap.cpus as u32,
        load: (!cfg!(windows)).then(|| [snap.load[0] as f32, snap.load[1] as f32, snap.load[2] as f32]),
        mem_total_kb: snap.mem_total / 1024, mem_avail_kb: snap.mem_avail / 1024, disk, latency_ms: None, timed_out: false, monitor_text: String::new(),
    };
    store(LOCAL_KEY, s.clone(), None);
    release(LOCAL_KEY);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "cpu  4705 150 1120 16250856 1000 0 100 0 0 0\n---\n0.52 0.40 0.31 1/234 999\n---\nMemTotal:       16000000 kB\nMemAvailable:    4000000 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n---\nFilesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sdb1 2000 1000 1000 50% /data\n/dev/sda1 1000 430 570 43% /\n---\n8\nup 3 days\n";

    #[test]
    fn cpu_line_and_delta() {
        let a = parse_cpu_line("cpu  100 0 100 700 100 0 0 0 0 0").unwrap();
        assert_eq!((a.total, a.idle), (1000, 800));
        let b = parse_cpu_line("cpu  150 0 150 1000 100 0 0 0 0 0").unwrap();
        // 400 more jiffies, 300 of them idle -> 25% busy
        assert!((cpu_pct(a, b).unwrap() - 25.0).abs() < 0.01);
        assert!(cpu_pct(a, a).is_none());
        assert!(cpu_pct(b, a).is_none());
        assert!(parse_cpu_line("cpu0 1 2 3").is_none());
        assert!(parse_cpu_line("").is_none());
    }

    #[test]
    fn combined_output_splits_into_stat_and_monitor() {
        let (cpu, mon) = parse_stats(OUT);
        assert_eq!(cpu.unwrap().total, 4705 + 150 + 1120 + 16250856 + 1000 + 100);
        let m = hosttools::parse_monitor(mon);
        assert_eq!((m.cpus, m.mem_total_kb, m.mem_avail_kb), (8, 16000000, 4000000));
        assert_eq!(m.load[0], 0.52);
        assert_eq!(m.disks.len(), 2);
        assert_eq!(m.uptime, "up 3 days");
    }

    #[test]
    fn non_linux_host_has_no_stat_line() {
        let (cpu, mon) = parse_stats("---\ncat: /proc/loadavg: No such file or directory\n---\n");
        assert!(cpu.is_none());
        assert_eq!(hosttools::parse_monitor(mon).mem_total_kb, 0);
        assert_eq!(parse_stats("garbage"), (None, ""));
    }

    #[test]
    fn median_of_last_probes() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[40]), Some(40));
        assert_eq!(median(&[90, 20, 40]), Some(40));
        assert_eq!(median(&[10, 300]), Some(300));
    }

    #[test]
    fn command_starts_with_stat_and_keeps_the_monitor_command() {
        let c = stats_command();
        assert!(c.starts_with("head -n1 /proc/stat"));
        assert!(c.ends_with(Tab::Monitor.read_command()));
    }

    #[test]
    fn fractions() {
        let mut s = HostStats::empty(false);
        s.mem_total_kb = 1000; s.mem_avail_kb = 250; s.cpus = 4; s.load = Some([2.0, 1.0, 1.0]);
        assert_eq!(s.mem_frac(), Some(0.75));
        assert_eq!(s.load_frac(), Some(0.5));
        s.cpus = 0;
        assert_eq!(s.load_frac(), None);
    }

    #[test]
    fn claim_reuses_fresh_samples_and_blocks_a_second_sampler() {
        let key = "test-claim";
        clear(key);
        assert!(matches!(claim(key, Duration::from_secs(5)), Ok(None)));
        assert!(in_flight(key));
        assert!(claim(key, Duration::from_secs(5)).is_err());
        store(key, HostStats::empty(false), None);
        release(key);
        assert!(matches!(claim(key, Duration::from_secs(5)), Ok(Some(_))));
        assert!(matches!(claim(key, Duration::ZERO), Ok(None)));
        release(key);
        clear(key);
    }
}
