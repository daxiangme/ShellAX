//! Host-side tools (PRD 3.8 P2): monitor, processes, services and Docker on the focused host.
//! Reads run fixed, read-only commands; every write (kill, service / container actions) is
//! built from validated arguments and goes through the permission engine before it runs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab { Monitor, Processes, Services, Docker }

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Monitor, Tab::Processes, Tab::Services, Tab::Docker];
    /// Read-only command producing this tab's data.
    pub fn read_command(self) -> &'static str {
        match self {
            Tab::Monitor => "cat /proc/loadavg; echo ---; cat /proc/meminfo; echo ---; df -P -k -x tmpfs -x devtmpfs -x overlay 2>/dev/null || df -P -k; echo ---; nproc 2>/dev/null; uptime -p 2>/dev/null || true",
            Tab::Processes => "ps -eo pid=,user=,pcpu=,pmem=,rss=,comm= --sort=-pcpu 2>/dev/null | head -n 40",
            Tab::Services => "systemctl list-units --type=service --all --no-pager --no-legend --plain 2>&1 | head -n 200",
            Tab::Docker => "docker ps -a --format '{{.ID}}\\t{{.Names}}\\t{{.Image}}\\t{{.State}}\\t{{.Status}}' 2>&1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Monitor { pub load: [f32; 3], pub cpus: u32, pub mem_total_kb: u64, pub mem_avail_kb: u64, pub swap_total_kb: u64, pub swap_free_kb: u64, pub disks: Vec<Disk>, pub uptime: String }
#[derive(Debug, Clone, PartialEq)]
pub struct Disk { pub mount: String, pub size_kb: u64, pub used_kb: u64, pub pct: u8 }

pub fn parse_monitor(out: &str) -> Monitor {
    let mut parts = out.split("---\n");
    let mut m = Monitor::default();
    if let Some(l) = parts.next() { for (i, v) in l.split_whitespace().take(3).enumerate() { m.load[i] = v.parse().unwrap_or(0.0); } }
    if let Some(mi) = parts.next() {
        for line in mi.lines() {
            let mut it = line.split_whitespace();
            let (k, v) = (it.next().unwrap_or(""), it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0));
            match k { "MemTotal:" => m.mem_total_kb = v, "MemAvailable:" => m.mem_avail_kb = v, "SwapTotal:" => m.swap_total_kb = v, "SwapFree:" => m.swap_free_kb = v, _ => {} }
        }
    }
    if let Some(df) = parts.next() {
        for line in df.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 6 { if let (Ok(size), Ok(used)) = (f[1].parse(), f[2].parse()) {
                m.disks.push(Disk { mount: f[5..].join(" "), size_kb: size, used_kb: used, pct: f[4].trim_end_matches('%').parse().unwrap_or(0) });
            } }
        }
    }
    if let Some(rest) = parts.next() { let mut l = rest.lines(); m.cpus = l.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0); m.uptime = l.next().unwrap_or("").trim().to_string(); }
    m
}

#[derive(Debug, Clone, PartialEq)]
pub struct Proc { pub pid: u32, pub user: String, pub cpu: f32, pub mem: f32, pub rss_kb: u64, pub comm: String }
pub fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines().filter_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 6 { return None; }
        Some(Proc { pid: f[0].parse().ok()?, user: f[1].into(), cpu: f[2].parse().unwrap_or(0.0), mem: f[3].parse().unwrap_or(0.0), rss_kb: f[4].parse().unwrap_or(0), comm: f[5..].join(" ") })
    }).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Service { pub unit: String, pub load: String, pub active: String, pub sub: String, pub desc: String }
/// `systemctl … --plain --no-legend`; `None` when systemd is not available on the host.
pub fn parse_services(out: &str) -> Option<Vec<Service>> {
    if out.contains("System has not been booted with systemd") || out.contains("command not found") || out.contains("Failed to connect to bus") { return None; }
    Some(out.lines().filter_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() >= 4 && f[0].ends_with(".service")).then(|| Service { unit: f[0].into(), load: f[1].into(), active: f[2].into(), sub: f[3].into(), desc: f[4..].join(" ") })
    }).collect())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Container { pub id: String, pub name: String, pub image: String, pub state: String, pub status: String }
/// `None` when Docker is not installed / not reachable (the error text is shown instead).
pub fn parse_docker(out: &str) -> Result<Vec<Container>, String> {
    let t = out.trim();
    if t.contains("command not found") || t.contains("Cannot connect to the Docker daemon") || t.contains("permission denied") { return Err(t.lines().next().unwrap_or("").to_string()); }
    Ok(t.lines().filter_map(|l| { let f: Vec<&str> = l.split('\t').collect(); (f.len() >= 5).then(|| Container { id: f[0].into(), name: f[1].into(), image: f[2].into(), state: f[3].into(), status: f[4].into() }) }).collect())
}

/// Write actions. Arguments are validated so the command line cannot be extended.
#[derive(Debug, Clone, PartialEq)]
pub enum Action { Kill { pid: u32, force: bool }, Service { verb: &'static str, unit: String }, Docker { verb: &'static str, id: String } }

fn safe_name(s: &str) -> bool { !s.is_empty() && s.len() <= 256 && s.chars().all(|c| c.is_ascii_alphanumeric() || "@._:-".contains(c)) && !s.starts_with('-') }

impl Action {
    pub fn command(&self) -> Result<String, String> {
        match self {
            Action::Kill { pid, force } if *pid > 1 => Ok(format!("kill {}{pid}", if *force { "-9 " } else { "" })),
            Action::Kill { .. } => Err("refusing to signal pid 0/1".into()),
            Action::Service { verb, unit } if ["start", "stop", "restart", "reload"].contains(verb) && safe_name(unit) => Ok(format!("sudo -n systemctl {verb} {unit}")),
            Action::Docker { verb, id } if ["start", "stop", "restart", "rm"].contains(verb) && safe_name(id) => Ok(format!("docker {verb} {id}")),
            Action::Docker { verb: "logs", id } if safe_name(id) => Ok(format!("docker logs --tail 200 {id}")),
            _ => Err("invalid action".into()),
        }
    }
    /// Reads (docker logs) are R0 and do not need approval.
    pub fn is_write(&self) -> bool { !matches!(self, Action::Docker { verb: "logs", .. }) }
}


/// Read-only network counters (Linux `/proc/net/dev`).
pub const NET_COMMAND: &str = "cat /proc/net/dev 2>/dev/null";
#[derive(Debug, Clone, PartialEq)]
pub struct NetIf { pub name: String, pub rx: u64, pub tx: u64 }
/// Interfaces with their byte counters, loopback skipped.
pub fn parse_netdev(out: &str) -> Vec<NetIf> {
    out.lines().filter_map(|l| {
        let (name, rest) = l.split_once(':')?;
        let name = name.trim();
        if name.is_empty() || name == "lo" || name.contains('|') { return None; }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if f.len() < 9 { return None; }
        Some(NetIf { name: name.into(), rx: f[0], tx: f[8] })
    }).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn netdev_counters() {
        let out = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 100 1 0 0 0 0 0 0 100 1 0 0 0 0 0 0\n  eth0: 123456 10 0 0 0 0 0 0 654321 9 0 0 0 0 0 0\n";
        assert_eq!(super::parse_netdev(out), vec![super::NetIf { name: "eth0".into(), rx: 123456, tx: 654321 }]);
    }

    use super::*;
    #[test]
    fn parsers_and_actions() {
        let m = parse_monitor("0.52 0.40 0.31 1/234 999\n---\nMemTotal:       16000000 kB\nMemAvailable:    8000000 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n---\nFilesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 1000 430 570 43% /\n/dev/sdb1 2000 1000 1000 50% /data disk\n---\n8\nup 3 days\n");
        assert_eq!(m.load, [0.52, 0.40, 0.31]); assert_eq!((m.mem_total_kb, m.mem_avail_kb, m.cpus), (16000000, 8000000, 8));
        assert_eq!(m.disks.len(), 2); assert_eq!(m.disks[1].mount, "/data disk"); assert_eq!(m.uptime, "up 3 days");
        let p = parse_ps("  101 root  12.5  1.0 20480 nginx: worker\n bad line\n");
        assert_eq!(p.len(), 1); assert_eq!((p[0].pid, p[0].comm.as_str()), (101, "nginx: worker"));
        let s = parse_services("ssh.service loaded active running OpenBSD Secure Shell server\ncron.service loaded inactive dead Regular background program\n").unwrap();
        assert_eq!(s.len(), 2); assert_eq!(s[1].active, "inactive");
        assert!(parse_services("System has not been booted with systemd as init system (PID 1). Can't operate.").is_none());
        let d = parse_docker("abc\tweb\tnginx:1\trunning\tUp 2 hours\n").unwrap();
        assert_eq!(d[0].name, "web");
        assert!(parse_docker("bash: docker: command not found").is_err());
        assert_eq!(Action::Kill { pid: 4242, force: false }.command().unwrap(), "kill 4242");
        assert!(Action::Kill { pid: 1, force: true }.command().is_err());
        assert_eq!(Action::Service { verb: "restart", unit: "nginx.service".into() }.command().unwrap(), "sudo -n systemctl restart nginx.service");
        assert!(Action::Service { verb: "restart", unit: "x; rm -rf /".into() }.command().is_err());
        assert!(Action::Docker { verb: "exec", id: "abc".into() }.command().is_err());
        assert!(!Action::Docker { verb: "logs", id: "abc".into() }.is_write());
        // writes are classified as risky by the permission engine (so they ask / are denied in Plan)
        for a in [Action::Kill { pid: 4242, force: false }, Action::Service { verb: "restart", unit: "nginx.service".into() }, Action::Docker { verb: "rm", id: "abc".into() }] {
            assert!(crate::policy::classify(&a.command().unwrap()) >= crate::policy::Risk::R2, "{a:?}");
        }
        assert_eq!(crate::policy::classify(&Action::Docker { verb: "logs", id: "abc".into() }.command().unwrap()), crate::policy::Risk::R0);
    }
}
