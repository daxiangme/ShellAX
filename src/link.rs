//! `ssh://` / `sftp://` links (PRD 3.1 P2 "堡垒机拉起"): `shellax-cli ssh://user@host:2222`,
//! `sftp://user@host/var/log`, and Xshell-style `-url ssh://…`. Passwords in URLs are never
//! used (dropped and reported); host keys are verified as for any other connection.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Ssh, Sftp }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link { pub kind: Kind, pub user: Option<String>, pub host: String, pub port: Option<u16>, pub path: Option<String>, pub dropped_password: bool }

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes(); let mut out = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse(url: &str) -> Result<Link, String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("not a link: {url}"))?;
    let kind = match scheme.to_ascii_lowercase().as_str() { "ssh" => Kind::Ssh, "sftp" | "scp" => Kind::Sftp, s => return Err(format!("unsupported scheme {s}://")) };
    let (auth_host, path) = match rest.find('/') { Some(i) => (&rest[..i], Some(&rest[i..])), None => (rest, None) };
    let (userinfo, hostport) = match auth_host.rsplit_once('@') { Some((u, h)) => (Some(u), h), None => (None, auth_host) };
    let (mut user, mut dropped) = (None, false);
    if let Some(u) = userinfo {
        // ssh URI (draft-ietf-secsh-scp-sftp-ssh-uri): user[;fingerprint=…][:password]
        let u = u.split(';').next().unwrap_or("");
        let (name, pw) = match u.split_once(':') { Some((n, _)) => (n, true), None => (u, false) };
        dropped = pw;
        if !name.is_empty() { user = Some(pct_decode(name)); }
    }
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (h, r) = v6.split_once(']').ok_or("bad IPv6 host")?;
        (h.to_string(), r.strip_prefix(':').map(|p| p.parse::<u16>().map_err(|_| format!("bad port {p}"))).transpose()?)
    } else {
        match hostport.rsplit_once(':') { Some((h, p)) if !p.is_empty() => (h.to_string(), Some(p.parse::<u16>().map_err(|_| format!("bad port {p}"))?)), Some((h, _)) => (h.to_string(), None), None => (hostport.to_string(), None) }
    };
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == ';') { return Err(format!("bad host in {url}")); }
    let path = path.map(pct_decode).filter(|p| p != "/" || kind == Kind::Sftp).map(|p| p.strip_prefix("/~").map(|r| format!("~{r}")).unwrap_or(p));
    Ok(Link { kind, user, host, port, path, dropped_password: dropped })
}

/// Links on a command line: bare `ssh://…` / `sftp://…` arguments and Xshell's `-url <link>`.
pub fn from_args(args: &[String]) -> Vec<String> {
    let mut out = vec![]; let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "-url" || a == "--url" { if let Some(u) = it.next() { out.push(u.clone()); } continue; }
        let l = a.to_ascii_lowercase();
        if l.starts_with("ssh://") || l.starts_with("sftp://") || l.starts_with("scp://") { out.push(a.clone()); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_links() {
        let l = parse("ssh://deploy@10.0.0.5:2222").unwrap();
        assert_eq!((l.kind, l.user.as_deref(), l.host.as_str(), l.port, l.path.clone()), (Kind::Ssh, Some("deploy"), "10.0.0.5", Some(2222), None));
        let l = parse("sftp://ops@bastion.example.com/var/log/nginx").unwrap();
        assert_eq!((l.kind, l.path.as_deref(), l.port), (Kind::Sftp, Some("/var/log/nginx"), None));
        let l = parse("ssh://root:hunter2@h").unwrap();
        assert!(l.dropped_password && l.user.as_deref() == Some("root"));
        let l = parse("ssh://u;fingerprint=ssh-ed25519-AAAA@[::1]:22").unwrap();
        assert_eq!((l.host.as_str(), l.port, l.user.as_deref()), ("::1", Some(22), Some("u")));
        assert_eq!(parse("sftp://u@h/~/conf").unwrap().path.as_deref(), Some("~/conf"));
        assert_eq!(parse("ssh://a%40corp@h").unwrap().user.as_deref(), Some("a@corp"));
        assert!(parse("http://x").is_err() && parse("ssh://h:99999").is_err() && parse("ssh://h;rm").is_err());
        let args: Vec<String> = ["app", "-url", "ssh://a@b", "sftp://c", "--other"].iter().map(|s| s.to_string()).collect();
        assert_eq!(from_args(&args), vec!["ssh://a@b", "sftp://c"]);
    }
}
