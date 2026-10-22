//! Sidebar host outline (PRD 3.1 主机管理): arbitrarily nested groups written as `a/b/c`,
//! moving a host into a group by drag-and-drop, and OS badges from a one-shot probe.
//! Pure functions so the tree logic is unit-tested without a window.

/// One group header row: full path (`a/b`), its last segment, and its nesting depth (0 = top level).
#[derive(Clone, Debug, PartialEq)]
pub struct GroupRow { pub path: String, pub name: String, pub depth: usize }

/// Normalise a user-entered group path: trim segments, drop empty ones (`" a// b "` → `a/b`).
pub fn normalize(path: &str) -> String {
    path.split('/').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

/// Header rows for the given group paths in first-appearance order, inserting implicit ancestors
/// (a host in `prod/web/eu` creates `prod`, `prod/web`, `prod/web/eu`) and keeping children
/// directly below their parent.
pub fn rows(paths: &[String]) -> Vec<GroupRow> {
    // build an ordered tree of segments
    #[derive(Default)]
    struct T { kids: Vec<(String, T)> }
    let mut root = T::default();
    for p in paths {
        let mut n = &mut root;
        for seg in p.split('/').filter(|s| !s.is_empty()) {
            let i = match n.kids.iter().position(|(k, _)| k == seg) { Some(i) => i, None => { n.kids.push((seg.to_string(), T::default())); n.kids.len() - 1 } };
            n = &mut n.kids[i].1;
        }
    }
    fn walk(t: &T, prefix: &str, depth: usize, out: &mut Vec<GroupRow>) {
        for (k, c) in &t.kids {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}/{k}") };
            out.push(GroupRow { path: path.clone(), name: k.clone(), depth });
            walk(c, &path, depth + 1, out);
        }
    }
    let mut out = vec![];
    walk(&root, "", 0, &mut out);
    out
}

/// Hidden when any ancestor (or the group itself) is collapsed.
pub fn hidden_by(path: &str, collapsed: &dyn Fn(&str) -> bool) -> bool {
    let mut acc = String::new();
    for seg in path.split('/') {
        if !acc.is_empty() { acc.push('/'); }
        acc.push_str(seg);
        if collapsed(&acc) { return true; }
    }
    false
}

/// Number of hosts in `path` and all its sub-groups.
pub fn count_under(path: &str, host_groups: &[String]) -> usize {
    host_groups.iter().filter(|g| *g == path || g.starts_with(&format!("{path}/"))).count()
}

/// Read-only probe run once per connected host (POSIX `uname` + os-release; Windows OpenSSH answers via `ver`).
pub const PROBE: &str = "uname -s 2>/dev/null; (. /etc/os-release 2>/dev/null && echo \"$ID\") || true; ver 2>/dev/null || true";

/// OS id from the probe output: distro id for Linux when known, else the kernel family.
pub fn os_from_probe(out: &str) -> Option<String> {
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let first = lines.first()?.to_ascii_lowercase();
    if first.contains("microsoft windows") { return Some("windows".into()); }
    match first.as_str() {
        "linux" => Some(lines.get(1).map(|s| s.to_ascii_lowercase()).filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !s.is_empty()).unwrap_or_else(|| "linux".into())),
        "darwin" => Some("macos".into()),
        "freebsd" | "openbsd" | "netbsd" => Some(first),
        s if s.starts_with("mingw") || s.starts_with("msys") || s.starts_with("cygwin") => Some("windows".into()),
        _ => None,
    }
}

/// Short badge text and display name for an OS id.
pub fn badge(os: &str) -> (&'static str, &'static str) {
    match os {
        "ubuntu" => ("Ub", "Ubuntu"), "debian" => ("Db", "Debian"), "fedora" => ("Fd", "Fedora"),
        "centos" => ("Ce", "CentOS"), "rhel" => ("RH", "RHEL"), "rocky" => ("Ro", "Rocky"), "almalinux" => ("Al", "AlmaLinux"),
        "alpine" => ("Ap", "Alpine"), "arch" => ("Ar", "Arch"), "opensuse-leap" | "opensuse-tumbleweed" | "sles" => ("Su", "SUSE"),
        "amzn" => ("Am", "Amazon Linux"), "macos" => ("Ma", "macOS"), "windows" => ("Wi", "Windows"),
        "freebsd" => ("Fb", "FreeBSD"), "openbsd" => ("Ob", "OpenBSD"), "netbsd" => ("Nb", "NetBSD"),
        _ => ("Lx", "Linux"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_rows_and_collapse() {
        let g = vec!["prod/web/eu".to_string(), "dev".into(), "prod/db".into(), "prod/web".into()];
        let r: Vec<(String, usize)> = rows(&g).into_iter().map(|r| (r.path, r.depth)).collect();
        assert_eq!(r, vec![("prod".into(), 0), ("prod/web".into(), 1), ("prod/web/eu".into(), 2), ("prod/db".into(), 1), ("dev".into(), 0)]);
        assert!(hidden_by("prod/web/eu", &|p| p == "prod"));
        assert!(!hidden_by("prod/db", &|p| p == "prod/web"));
        assert_eq!(count_under("prod", &g), 3);
        assert_eq!(count_under("prod/web", &g), 2);
        assert_eq!(normalize(" a// b /"), "a/b");
    }
    #[test]
    fn os_probe() {
        assert_eq!(os_from_probe("Linux\nubuntu\n").as_deref(), Some("ubuntu"));
        assert_eq!(os_from_probe("Linux\n").as_deref(), Some("linux"));
        assert_eq!(os_from_probe("Darwin\n").as_deref(), Some("macos"));
        assert_eq!(os_from_probe("\nMicrosoft Windows [Version 10.0]\n").as_deref(), Some("windows"));
        assert_eq!(os_from_probe("Linux\n$(rm -rf)\n").as_deref(), Some("linux"));
        assert_eq!(os_from_probe(""), None);
        assert_eq!(badge("ubuntu").0, "Ub");
        assert_eq!(badge("weird").1, "Linux");
    }
}
