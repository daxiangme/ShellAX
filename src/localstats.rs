//! 本机的系统概览 / 进程 / 网络数据（Windows 与 Mac 没有 `/proc`，也没有 `sh`）。
//! 采集用 sysinfo，再整理成与 Linux 命令输出相同的文本，由 `hosttools` 的解析函数统一处理。
use std::fmt::Write as _;
use std::sync::{LazyLock, Mutex};

use sysinfo::{Disks, Networks, ProcessesToUpdate, System};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub cpus: usize,
    /// 全部核心的平均占用，0–100。
    pub cpu_pct: f32,
    pub mem_total: u64,
    pub mem_avail: u64,
    pub swap_total: u64,
    pub swap_free: u64,
    pub uptime_secs: u64,
    /// (名称, 总容量, 可用, 挂载点) 单位字节。
    pub disks: Vec<(String, u64, u64, String)>,
    /// (pid, 名称, cpu%, 内存字节)
    pub procs: Vec<(u32, String, f32, u64)>,
    /// (接口, 收到字节, 发出字节)
    pub nets: Vec<(String, u64, u64)>,
    /// 1 / 5 / 15 分钟负载；Windows 没有这个概念，全为 0。
    pub load: [f64; 3],
}

fn uptime_text(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    let mut parts = vec![];
    if d > 0 { parts.push(format!("{d} day{}", if d == 1 { "" } else { "s" })); }
    if h > 0 { parts.push(format!("{h} hour{}", if h == 1 { "" } else { "s" })); }
    parts.push(format!("{m} minute{}", if m == 1 { "" } else { "s" }));
    format!("up {}", parts.join(", "))
}

/// 与 `Tab::Monitor.read_command()` 输出同样的格式。
/// 负载一栏放“核心平均占用 × 核数”，界面按 负载 / 核数 显示成占用百分比。
pub fn monitor_text(s: &Snapshot) -> String {
    let load = s.cpu_pct / 100.0 * s.cpus as f32;
    let mut o = String::new();
    let _ = write!(o, "{load:.2} {load:.2} {load:.2} 1/1 1\n---\n");
    let _ = write!(o, "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n---\n", s.mem_total / 1024, s.mem_avail / 1024, s.swap_total / 1024, s.swap_free / 1024);
    o.push_str("Filesystem 1024-blocks Used Available Capacity Mounted on\n");
    for (name, total, avail, mount) in &s.disks {
        if *total == 0 { continue; }
        let used = total.saturating_sub(*avail);
        let pct = (used as f64 / *total as f64 * 100.0).round() as u64;
        let _ = writeln!(o, "{} {} {} {} {}% {}", name.replace(char::is_whitespace, "_"), total / 1024, used / 1024, avail / 1024, pct, mount);
    }
    let _ = write!(o, "---\n{}\n{}\n", s.cpus, uptime_text(s.uptime_secs));
    o
}

/// 与 `ps -eo pid=,user=,pcpu=,pmem=,rss=,comm=` 同样的格式（按 CPU 降序，前 40 个）。
pub fn ps_text(s: &Snapshot) -> String {
    let mut p = s.procs.clone();
    p.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal).then(b.3.cmp(&a.3)));
    let mut o = String::new();
    for (pid, name, cpu, mem) in p.into_iter().take(40) {
        let pm = if s.mem_total > 0 { mem as f64 / s.mem_total as f64 * 100.0 } else { 0.0 };
        let _ = writeln!(o, "{pid} - {cpu:.1} {pm:.1} {} {}", mem / 1024, name.trim());
    }
    o
}

/// 与 `/proc/net/dev` 同样的格式。
pub fn net_text(s: &Snapshot) -> String {
    let mut o = String::from("Inter-|   Receive |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n");
    for (name, rx, tx) in &s.nets { let _ = writeln!(o, "{}: {rx} 0 0 0 0 0 0 0 {tx} 0 0 0 0 0 0 0", name.replace([':', ' '], "_")); }
    o
}

struct Sampler { sys: System, nets: Networks }
static SAMPLER: LazyLock<Mutex<Sampler>> = LazyLock::new(|| Mutex::new(Sampler { sys: System::new(), nets: Networks::new() }));

/// 采一次样。CPU 占用靠两次采样之间的差值，所以第一次返回 0。
pub fn snapshot() -> Snapshot {
    let mut g = SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
    let Sampler { sys, nets } = &mut *g;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    sys.refresh_processes(ProcessesToUpdate::All);
    nets.refresh_list();
    nets.refresh();
    let disks = Disks::new_with_refreshed_list();
    Snapshot {
        cpus: sys.cpus().len().max(1),
        cpu_pct: sys.global_cpu_usage(),
        mem_total: sys.total_memory(),
        mem_avail: sys.available_memory(),
        swap_total: sys.total_swap(),
        swap_free: sys.free_swap(),
        uptime_secs: System::uptime(),
        disks: disks.iter().map(|d| (d.name().to_string_lossy().into_owned(), d.total_space(), d.available_space(), d.mount_point().display().to_string())).collect(),
        procs: sys.processes().iter().map(|(pid, p)| (pid.as_u32(), p.name().to_string_lossy().into_owned(), p.cpu_usage() / sys.cpus().len().max(1) as f32, p.memory())).collect(),
        nets: nets.iter().map(|(n, d)| (n.clone(), d.total_received(), d.total_transmitted())).collect(),
        load: load_avg(),
    }
}

fn load_avg() -> [f64; 3] {
    if cfg!(windows) { return [0.0; 3]; }
    let l = System::load_average();
    [l.one, l.five, l.fifteen]
}

/// 只采 CPU、内存、磁盘（路径栏每几秒一次，不扫进程和网卡）。与 `snapshot` 共用同一份 CPU 基线。
pub fn snapshot_light() -> Snapshot {
    let mut g = SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
    let Sampler { sys, .. } = &mut *g;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let disks = Disks::new_with_refreshed_list();
    Snapshot {
        cpus: sys.cpus().len().max(1),
        cpu_pct: sys.global_cpu_usage(),
        mem_total: sys.total_memory(),
        mem_avail: sys.available_memory(),
        swap_total: sys.total_swap(),
        swap_free: sys.free_swap(),
        uptime_secs: System::uptime(),
        disks: disks.iter().map(|d| (d.name().to_string_lossy().into_owned(), d.total_space(), d.available_space(), d.mount_point().display().to_string())).collect(),
        load: load_avg(),
        ..Default::default()
    }
}

/// 系统盘 / 根卷：(挂载点, 总容量, 已用)，单位字节。Windows 取系统盘，Mac 优先数据卷，其余取 `/`。
pub fn root_disk(s: &Snapshot) -> Option<(String, u64, u64)> {
    let want: Vec<String> = if cfg!(windows) {
        vec![format!("{}\\", std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into())), "C:\\".into()]
    } else if cfg!(target_os = "macos") {
        vec!["/System/Volumes/Data".into(), "/".into()]
    } else { vec!["/".into()] };
    let pick = want.iter().find_map(|m| s.disks.iter().find(|d| d.3.eq_ignore_ascii_case(m) && d.1 > 0));
    let d = pick.or_else(|| s.disks.iter().filter(|d| d.1 > 0).max_by_key(|d| d.1))?;
    Some((d.3.clone(), d.1, d.1.saturating_sub(d.2)))
}

static LAST: Mutex<Option<(std::time::Instant, Snapshot)>> = Mutex::new(None);

/// 概览、进程、网络三次读取共用一次采样（两秒内复用），避免间隔过短导致 CPU 占用恒为 0。
pub fn snapshot_cached() -> Snapshot {
    let mut g = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, s)) = g.as_ref() { if t.elapsed() < std::time::Duration::from_secs(2) { return s.clone(); } }
    let s = snapshot();
    *g = Some((std::time::Instant::now(), s.clone()));
    s
}

/// 本机读取命令对应的原生实现；不是概览 / 进程 / 网络时返回 None。
pub fn native_output(cmd: &str) -> Option<String> {
    use crate::hosttools::{Tab, NET_COMMAND};
    if cmd == Tab::Monitor.read_command() { Some(monitor_text(&snapshot_cached())) }
    else if cmd == Tab::Processes.read_command() { Some(ps_text(&snapshot_cached())) }
    else if cmd == NET_COMMAND { Some(net_text(&snapshot_cached())) }
    else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosttools::{parse_monitor, parse_netdev, parse_ps};

    fn sample() -> Snapshot {
        Snapshot {
            cpus: 8, cpu_pct: 25.0, mem_total: 16 << 30, mem_avail: 6 << 30, swap_total: 2 << 30, swap_free: 1 << 30, uptime_secs: 90_000,
            disks: vec![("Local Disk".into(), 500 << 30, 100 << 30, "C:\\".into()), ("empty".into(), 0, 0, "Z:\\".into())],
            procs: vec![(4, "System".into(), 0.5, 1 << 20), (900, "Code Helper.exe".into(), 12.0, 800 << 20)],
            nets: vec![("Wi-Fi".into(), 1000, 2000)],
            load: [0.0; 3],
        }
    }
    #[test]
    fn text_round_trips_through_the_linux_parsers() {
        let s = sample();
        let m = parse_monitor(&monitor_text(&s));
        assert_eq!((m.cpus, m.mem_total_kb, m.mem_avail_kb, m.swap_total_kb), (8, 16 << 20, 6 << 20, 2 << 20));
        assert!((m.load[0] - 2.0).abs() < 0.01);
        assert_eq!(m.disks.len(), 1);
        assert_eq!((m.disks[0].mount.as_str(), m.disks[0].pct), ("C:\\", 80));
        assert_eq!(m.uptime, "up 1 day, 1 hour, 0 minutes");
        let p = parse_ps(&ps_text(&s));
        assert_eq!((p[0].pid, p[0].comm.as_str()), (900, "Code Helper.exe"));
        assert_eq!(p.len(), 2);
        let n = parse_netdev(&net_text(&s));
        assert_eq!((n[0].name.as_str(), n[0].rx, n[0].tx), ("Wi-Fi", 1000, 2000));
    }
    #[test]
    fn root_disk_prefers_the_system_volume() {
        let mut s = sample();
        s.disks = vec![("data".into(), 900 << 30, 800 << 30, "/data".into()), ("root".into(), 100 << 30, 40 << 30, if cfg!(windows) { "C:\\".into() } else { "/".into() })];
        let (_, total, used) = root_disk(&s).unwrap();
        if !cfg!(target_os = "macos") { assert_eq!((total, used), (100 << 30, 60 << 30)); }
        s.disks.clear();
        assert!(root_disk(&s).is_none());
    }
    #[test]
    fn live_snapshot_has_something() {
        let s = snapshot();
        assert!(s.cpus >= 1 && s.mem_total > 0);
    }
}
