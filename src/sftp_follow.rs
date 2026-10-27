//! Pure logic behind SFTP "follow terminal cwd": mapping a terminal cwd to an absolute
//! directory and deciding when the SFTP pane should navigate.

/// Map a terminal cwd (OSC 7 path or prompt text, maybe `~`-relative) to an absolute directory.
pub fn follow_target(cwd: &str, home: &str) -> Option<String> {
    let c = cwd.trim();
    if c.is_empty() { return None; }
    let abs = if c == "~" { home.to_string() }
        else if let Some(r) = c.strip_prefix("~/") { if home.is_empty() { return None } format!("{}/{r}", home.trim_end_matches('/')) }
        else if c.starts_with('/') { c.to_string() }
        else { return None };
    if abs.is_empty() || !abs.starts_with('/') { return None; }
    let n = crate::ctxmenu::normalize(&abs);
    Some(if n.is_empty() { "/".into() } else { n })
}

/// Follow state: navigates only when the *terminal's* cwd changes, so the user can still browse
/// elsewhere in the SFTP pane until the shell moves again.
#[derive(Default, Debug, Clone)]
pub struct Follow { pub on: bool, last: Option<String> }
impl Follow {
    pub fn new(on: bool) -> Self { Self { on, last: None } }
    /// Returns the directory to navigate to, if any.
    pub fn step(&mut self, term_cwd: Option<&str>, home: &str, current: &str) -> Option<String> {
        if !self.on { return None; }
        let d = follow_target(term_cwd?, home)?;
        if self.last.as_deref() == Some(d.as_str()) { return None; }
        self.last = Some(d.clone());
        (d != current).then_some(d)
    }
    pub fn set(&mut self, on: bool) { self.on = on; self.last = None; }
}

/// Window title / tab title for an SFTP view.
pub fn title(host_label: &str) -> String { format!("SFTP · {host_label}") }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn follow_target_paths() {
        assert_eq!(follow_target("~", "/home/u").as_deref(), Some("/home/u"));
        assert_eq!(follow_target("~/a b/../c", "/home/u/").as_deref(), Some("/home/u/c"));
        assert_eq!(follow_target("/var/log/./nginx", "").as_deref(), Some("/var/log/nginx"));
        assert_eq!(follow_target("/", "").as_deref(), Some("/"));
        assert_eq!(follow_target("rel/dir", "/h"), None);
        assert_eq!(follow_target("~/x", ""), None);
        assert_eq!(follow_target("  ", "/h"), None);
        assert_eq!(follow_target("/数据/日志", "").as_deref(), Some("/数据/日志"));
    }
    #[test]
    fn follow_only_on_terminal_change() {
        let mut f = Follow::new(true);
        assert_eq!(f.step(Some("/tmp"), "/h", "/h").as_deref(), Some("/tmp"));
        // user browsed elsewhere; same terminal cwd -> no jump back
        assert_eq!(f.step(Some("/tmp"), "/h", "/etc"), None);
        assert_eq!(f.step(Some("~/src"), "/h", "/etc").as_deref(), Some("/h/src"));
        // already there -> remembered, no navigation
        assert_eq!(f.step(Some("/etc"), "/h", "/etc"), None);
        assert_eq!(f.step(None, "/h", "/etc"), None);
        f.set(false);
        assert_eq!(f.step(Some("/var"), "/h", "/etc"), None);
        f.set(true);
        assert_eq!(f.step(Some("/var"), "/h", "/etc").as_deref(), Some("/var"));
    }
}
