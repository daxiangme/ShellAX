//! Outbound proxy for SSH connections (PRD 3.1 P1): `socks5://[user:pass@]host:port`
//! (RFC 1928 / RFC 1929, remote DNS) or `http://[user:pass@]host:port` (HTTP CONNECT).
//! Written from the RFCs; no third-party proxy code.
use anyhow::{anyhow, bail, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[derive(Debug, Clone, PartialEq)]
pub enum Kind { Socks5, Http }
#[derive(Debug, Clone, PartialEq)]
pub struct Proxy { pub kind: Kind, pub host: String, pub port: u16, pub user: Option<(String, String)> }

/// Global proxy URL (ssh.proxy). Empty = direct.
pub static URL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
/// Hosts that bypass the proxy (ssh.no_proxy, comma separated globs).
pub static NO_PROXY: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub fn parse(url: &str) -> Result<Proxy> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").ok_or_else(|| anyhow!("proxy URL needs a scheme (socks5:// or http://)"))?;
    let kind = match scheme.to_ascii_lowercase().as_str() { "socks5" | "socks5h" | "socks" => Kind::Socks5, "http" => Kind::Http, s => bail!("unsupported proxy scheme {s}") };
    let rest = rest.trim_end_matches('/');
    let (user, hp) = match rest.rsplit_once('@') { Some((u, hp)) => { let (a, b) = u.split_once(':').unwrap_or((u, "")); (Some((a.to_string(), b.to_string())), hp) } None => (None, rest) };
    let (host, port) = match hp.rsplit_once(':') { Some((h, p)) => (h.trim_matches(['[', ']']).to_string(), p.parse().map_err(|_| anyhow!("bad proxy port {p}"))?), None => (hp.to_string(), if kind == Kind::Http { 8080 } else { 1080 }) };
    if host.is_empty() { bail!("proxy host is empty"); }
    Ok(Proxy { kind, host, port, user })
}

fn bypass(host: &str) -> bool {
    let np = NO_PROXY.lock().unwrap().clone();
    np.split(',').map(str::trim).filter(|p| !p.is_empty()).any(|p| p == host || crate::policy::glob(p, host))
}

/// The proxy to use for an HTTP client (`host` of the request URL), if `ssh.proxy` is set.
pub fn for_host(host: &str) -> Result<Option<Proxy>> {
    let url = URL.lock().unwrap().clone();
    if url.trim().is_empty() || bypass(host) { return Ok(None); }
    parse(&url).map(Some)
}

/// SSH hop. `use_proxy` is the per-connection switch. An empty or invalid URL is an error so the
/// hop is not silently opened directly. `no_proxy` still means direct.
pub fn for_ssh(host: &str, use_proxy: bool) -> Result<Option<Proxy>> {
    if !use_proxy || bypass(host) { return Ok(None); }
    let url = URL.lock().unwrap().clone();
    if url.trim().is_empty() { bail!(crate::i18n::t("ssh.proxy_not_configured")); }
    parse(&url).map(Some)
}

pub fn format_url(p: &Proxy) -> String {
    let scheme = match p.kind { Kind::Socks5 => "socks5", Kind::Http => "http" };
    let auth = p.user.as_ref().map(|(u, pw)| format!("{u}:{pw}@")).unwrap_or_default();
    let host = if p.host.contains(':') { format!("[{}]", p.host) } else { p.host.clone() };
    format!("{scheme}://{auth}{host}:{}", p.port)
}

/// Where a detected proxy came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectSource { System, Env, Listen }

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub kind: Kind,
    pub host: String,
    pub port: u16,
    pub user: Option<(String, String)>,
    pub source: DetectSource,
}

#[derive(Debug, Clone, Default)]
pub struct Detected { pub candidates: Vec<Candidate>, pub bypass: String }

pub fn candidate_label(c: &Candidate) -> String {
    let scheme = match c.kind { Kind::Socks5 => "socks5", Kind::Http => "http" };
    let auth = c.user.as_ref().map(|(u, _)| format!("{u}@")).unwrap_or_default();
    let host = if c.host.contains(':') { format!("[{}]", c.host) } else { c.host.clone() };
    format!("{scheme}://{auth}{host}:{}", c.port)
}

/// Windows `ProxyServer` value, or a bare `host:port` (treated as HTTP).
pub fn parse_system_server(raw: &str) -> Vec<Candidate> {
    let raw = raw.trim();
    if raw.is_empty() { return vec![]; }
    let mut out = Vec::new();
    if raw.contains('=') {
        for part in raw.split([';', ' ']) {
            let Some((k, v)) = part.split_once('=') else { continue };
            let kind = match k.trim().to_ascii_lowercase().as_str() {
                "socks" | "socks5" => Kind::Socks5,
                "http" | "https" => Kind::Http,
                _ => continue,
            };
            if let Some(c) = host_port(v.trim(), kind, DetectSource::System) { out.push(c); }
        }
    } else if let Some(c) = host_port(raw, Kind::Http, DetectSource::System) {
        out.push(c);
    }
    dedup(out)
}

/// `ProxyOverride` / `NO_PROXY` → comma-separated globs. `<local>` becomes `localhost`.
pub fn windows_bypass_to_globs(raw: &str) -> String {
    let mut out = Vec::new();
    let mut local = false;
    for p in raw.split([';', ',']).map(str::trim).filter(|p| !p.is_empty()) {
        if p.eq_ignore_ascii_case("<local>") || p.eq_ignore_ascii_case("localhost") { local = true; continue; }
        if !out.iter().any(|x: &String| x == p) { out.push(p.to_string()); }
    }
    if local { out.insert(0, "localhost".into()); }
    out.join(", ")
}

fn host_port(s: &str, kind: Kind, source: DetectSource) -> Option<Candidate> {
    if s.contains("://") {
        return parse(s).ok().map(|p| Candidate { kind: p.kind, host: p.host, port: p.port, user: p.user, source });
    }
    let (host, port) = s.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host.trim().trim_matches(['[', ']']).to_string();
    if host.is_empty() || port == 0 { return None; }
    Some(Candidate { kind, host, port, user: None, source })
}

fn dedup(mut v: Vec<Candidate>) -> Vec<Candidate> {
    v.sort_by_key(|c| (c.host.to_ascii_lowercase(), c.port, match c.kind { Kind::Socks5 => 0, Kind::Http => 1 }, match c.source { DetectSource::System => 0, DetectSource::Env => 1, DetectSource::Listen => 2 }));
    v.dedup_by(|a, b| a.kind == b.kind && a.port == b.port && a.host.eq_ignore_ascii_case(&b.host));
    v
}

fn loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
}

fn from_env() -> (Vec<Candidate>, String) {
    let mut out = Vec::new();
    for key in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        let Ok(v) = std::env::var(key) else { continue };
        let Ok(p) = parse(v.trim()) else { continue };
        out.push(Candidate { kind: p.kind, host: p.host, port: p.port, user: p.user, source: DetectSource::Env });
    }
    let bypass = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")).unwrap_or_default();
    (dedup(out), windows_bypass_to_globs(&bypass))
}

#[cfg(windows)]
fn system_proxy() -> (Vec<Candidate>, String) {
    let enabled = read_reg_dword(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings", "ProxyEnable").unwrap_or(0) != 0;
    if !enabled { return (vec![], String::new()); }
    let server = read_reg_sz(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings", "ProxyServer").unwrap_or_default();
    let over = read_reg_sz(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings", "ProxyOverride").unwrap_or_default();
    (parse_system_server(&server), windows_bypass_to_globs(&over))
}

#[cfg(target_os = "macos")]
fn system_proxy() -> (Vec<Candidate>, String) {
    let out = std::process::Command::new("/usr/sbin/scutil").arg("--proxy").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    (parse_scutil(&out), String::new())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system_proxy() -> (Vec<Candidate>, String) { (vec![], String::new()) }

/// 解析 `scutil --proxy` 输出：依次取已启用的 HTTPS、SOCKS、HTTP 代理。
pub fn parse_scutil(out: &str) -> Vec<Candidate> {
    let kv: std::collections::HashMap<&str, &str> = out.lines().filter_map(|l| l.split_once(" : ")).map(|(k, v)| (k.trim(), v.trim())).collect();
    let mut v = Vec::new();
    for (pre, kind) in [("HTTPS", Kind::Http), ("SOCKS", Kind::Socks5), ("HTTP", Kind::Http)] {
        if kv.get(format!("{pre}Enable").as_str()) != Some(&"1") { continue; }
        let (Some(h), Some(p)) = (kv.get(format!("{pre}Proxy").as_str()), kv.get(format!("{pre}Port").as_str()).and_then(|p| p.parse().ok())) else { continue };
        v.push(Candidate { kind, host: h.to_string(), port: p, user: None, source: DetectSource::System });
    }
    v
}

/// 应用未配置代理时使用的系统代理（系统设置，其次环境变量），ureq 语法。不探测端口。
pub fn system_proxy_url() -> Option<String> {
    let (sys, _) = system_proxy();
    let (env, _) = from_env();
    let c = sys.into_iter().chain(env).next()?;
    let host = if c.host.contains(':') { format!("[{}]", c.host) } else { c.host };
    Some(format!("{}://{host}:{}", match c.kind { Kind::Socks5 => "socks5", Kind::Http => "http" }, c.port))
}

/// 更新检查和下载用：应用代理优先，未设置时回退到系统代理。
pub fn ureq_builder_sys(url: &str, b: ureq::AgentBuilder) -> Result<ureq::AgentBuilder> {
    if URL.lock().unwrap().trim().is_empty() {
        if let Some(u) = system_proxy_url() { if let Ok(p) = ureq::Proxy::new(&u) { return Ok(b.proxy(p)); } }
        return Ok(b);
    }
    ureq_builder(url, b)
}

#[cfg(windows)]
pub(crate) fn read_reg_sz(sub: &str, name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let sub: Vec<u16> = sub.encode_utf16().chain(std::iter::once(0)).collect();
    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 2048];
    let mut len = (buf.len() * 2) as u32;
    let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, sub.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _, &mut len) };
    if status != 0 || len < 2 { return None; }
    let n = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n]))
}

#[cfg(windows)]
fn read_reg_dword(sub: &str, name: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let sub: Vec<u16> = sub.encode_utf16().chain(std::iter::once(0)).collect();
    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut val = 0u32;
    let mut len = 4u32;
    let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, sub.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut val as *mut u32 as *mut _, &mut len) };
    (status == 0).then_some(val)
}

const COMMON_PORTS: &[u16] = &[7890, 7891, 7897, 1080, 10808, 10809, 20171, 1081];

fn probe_port(port: u16) -> Option<Kind> {
    if socks5_alive(port) { return Some(Kind::Socks5); }
    if http_proxy_alive(port) { return Some(Kind::Http); }
    None
}

fn socks5_alive(port: u16) -> bool {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)) else { return false };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let _ = s.set_write_timeout(Some(std::time::Duration::from_millis(200)));
    if s.write_all(&[5, 1, 0]).is_err() { return false; }
    let mut b = [0u8; 2];
    s.read_exact(&mut b).is_ok() && b[0] == 5
}

fn http_proxy_alive(port: u16) -> bool {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)) else { return false };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let _ = s.set_write_timeout(Some(std::time::Duration::from_millis(200)));
    if s.write_all(b"CONNECT 127.0.0.1:1 HTTP/1.1\r\nHost: 127.0.0.1:1\r\n\r\n").is_err() { return false; }
    let mut buf = [0u8; 16];
    let Ok(n) = s.read(&mut buf) else { return false };
    n >= 5 && buf.starts_with(b"HTTP/")
}

/// System proxy, environment variables, then common local proxy ports. Does not scan the LAN.
pub fn detect() -> Detected {
    let (mut candidates, sys_bypass) = system_proxy();
    let (env, env_bypass) = from_env();
    candidates.extend(env);
    let mut ports: Vec<u16> = COMMON_PORTS.to_vec();
    for c in &candidates {
        if loopback(&c.host) { ports.push(c.port); }
    }
    ports.sort();
    ports.dedup();
    let probed = std::thread::scope(|sc| {
        let mut jobs = Vec::new();
        for port in ports {
            jobs.push(sc.spawn(move || probe_port(port).map(|kind| Candidate { kind, host: "127.0.0.1".into(), port, user: None, source: DetectSource::Listen })));
        }
        jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect::<Vec<_>>()
    });
    candidates.extend(probed);
    let bypass = if !sys_bypass.is_empty() { sys_bypass } else { env_bypass };
    Detected { candidates: dedup(candidates), bypass }
}

/// Host part of an http(s) URL (for no_proxy matching).
pub fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let hp = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hp = hp.rsplit_once('@').map(|x| x.1).unwrap_or(hp);
    if let Some(v6) = hp.strip_prefix('[') { return v6.split(']').next().unwrap_or("").to_string(); }
    hp.split(':').next().unwrap_or("").to_ascii_lowercase()
}

/// Proxy URL (ureq syntax) to use for an outbound HTTP request to `url` (AI providers, update
/// checks), honouring ssh.no_proxy. `None` = connect directly.
pub fn ureq_proxy_for(url: &str) -> Result<Option<String>> {
    let Some(p) = for_host(&url_host(url))? else { return Ok(None) };
    let auth = p.user.as_ref().map(|(u, pw)| format!("{u}:{pw}@")).unwrap_or_default();
    let host = if p.host.contains(':') { format!("[{}]", p.host) } else { p.host.clone() };
    Ok(Some(format!("{}://{auth}{host}:{}", match p.kind { Kind::Socks5 => "socks5", Kind::Http => "http" }, p.port)))
}

/// Apply the configured proxy to a ureq agent builder for requests to `url`.
pub fn ureq_builder(url: &str, b: ureq::AgentBuilder) -> Result<ureq::AgentBuilder> {
    Ok(match ureq_proxy_for(url)? { Some(u) => b.proxy(ureq::Proxy::new(u).map_err(|e| anyhow!("proxy: {e}"))?), None => b })
}

fn b64(d: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 { o.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' }); }
    }
    o
}

/// SOCKS5 CONNECT packet. IP literals use the IPv4/IPv6 address type so a local proxy in rule
/// mode routes them as addresses. Sending an IP as a domain makes rule mode try a DNS lookup
/// and drop the tunnel; global mode skips that lookup, which is why only global mode worked.
fn socks5_connect_packet(host: &str, port: u16) -> Result<Vec<u8>> {
    let mut req = vec![5, 1, 0];
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        req.push(1);
        req.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        req.push(4);
        req.extend_from_slice(&ip.octets());
    } else {
        if host.len() > 255 { bail!("host name too long"); }
        req.push(3);
        req.push(host.len() as u8);
        req.extend_from_slice(host.as_bytes());
    }
    req.extend_from_slice(&port.to_be_bytes());
    Ok(req)
}

/// Open a TCP tunnel to `host:port` through the proxy.
pub async fn connect(p: &Proxy, host: &str, port: u16) -> Result<TcpStream> {
    let mut s = TcpStream::connect((p.host.as_str(), p.port)).await.map_err(|e| anyhow!("proxy {}:{}: {e}", p.host, p.port))?;
    let _ = s.set_nodelay(true);
    match p.kind {
        Kind::Socks5 => {
            let methods: &[u8] = if p.user.is_some() { &[0, 2] } else { &[0] };
            let mut hello = vec![5, methods.len() as u8]; hello.extend_from_slice(methods);
            s.write_all(&hello).await?;
            let mut r = [0u8; 2]; s.read_exact(&mut r).await?;
            if r[0] != 5 { bail!("proxy is not SOCKS5"); }
            match r[1] {
                0 => {}
                2 => {
                    let (u, pw) = p.user.clone().unwrap_or_default();
                    let mut a = vec![1, u.len() as u8]; a.extend_from_slice(u.as_bytes()); a.push(pw.len() as u8); a.extend_from_slice(pw.as_bytes());
                    s.write_all(&a).await?;
                    let mut r = [0u8; 2]; s.read_exact(&mut r).await?;
                    if r[1] != 0 { bail!("SOCKS5 proxy rejected the username/password"); }
                }
                _ => bail!("SOCKS5 proxy accepts none of our auth methods"),
            }
            let req = socks5_connect_packet(host, port)?;
            s.write_all(&req).await?;
            let mut h = [0u8; 4]; s.read_exact(&mut h).await?;
            if h[1] != 0 { bail!("SOCKS5 proxy: connect to {host}:{port} failed (code {})", h[1]); }
            let skip = match h[3] { 1 => 4, 4 => 16, 3 => { let mut l = [0u8; 1]; s.read_exact(&mut l).await?; l[0] as usize } _ => bail!("bad SOCKS5 reply") };
            let mut rest = vec![0u8; skip + 2]; s.read_exact(&mut rest).await?;
            Ok(s)
        }
        Kind::Http => {
            let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
            let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
            if let Some((u, pw)) = &p.user {
                req.push_str(&format!("Proxy-Authorization: Basic {}\r\n", b64(format!("{u}:{pw}").as_bytes())));
            }
            req.push_str("\r\n");
            s.write_all(req.as_bytes()).await?;
            // read the response head byte-wise so no tunnel bytes are swallowed
            let mut br = BufReader::with_capacity(1, &mut s);
            let mut status = String::new();
            br.read_line(&mut status).await?;
            let code: u16 = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
            loop { let mut l = String::new(); if br.read_line(&mut l).await? == 0 || l == "\r\n" || l == "\n" { break; } }
            if code != 200 { bail!("HTTP proxy refused CONNECT {target}: {}", status.trim()); }
            Ok(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static CFG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[test]
    fn parse_urls() {
        assert_eq!(parse("socks5://127.0.0.1:1080").unwrap(), Proxy { kind: Kind::Socks5, host: "127.0.0.1".into(), port: 1080, user: None });
        assert_eq!(parse("http://u:p@proxy.corp:3128/").unwrap(), Proxy { kind: Kind::Http, host: "proxy.corp".into(), port: 3128, user: Some(("u".into(), "p".into())) });
        assert!(parse("ftp://x:1").is_err() && parse("x:1").is_err());
        assert_eq!(b64(b"u:p"), "dTpw"); assert_eq!(b64(b"user:pass1"), "dXNlcjpwYXNzMQ==");
    }
    #[tokio::test]
    async fn socks5_and_http_connect() {
        use tokio::net::TcpListener;
        // echo target
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap(); let eport = echo.local_addr().unwrap().port();
        tokio::spawn(async move { loop { let (mut c, _) = echo.accept().await.unwrap(); tokio::spawn(async move { let mut b = [0u8; 64]; let n = c.read(&mut b).await.unwrap(); c.write_all(&b[..n]).await.unwrap(); }); } });
        // SOCKS5 proxy: the port-forward module's server side
        let sp = TcpListener::bind("127.0.0.1:0").await.unwrap(); let sport = sp.local_addr().unwrap().port();
        tokio::spawn(async move { loop { let (mut c, _) = sp.accept().await.unwrap(); tokio::spawn(async move {
            let (h, p) = crate::forward::socks5_handshake(&mut c).await.unwrap();
            let mut up = TcpStream::connect((if h == "localhost" { "127.0.0.1" } else { h.as_str() }, p)).await.unwrap();
            c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut c, &mut up).await;
        }); } });
        // HTTP CONNECT proxy
        let hp = TcpListener::bind("127.0.0.1:0").await.unwrap(); let hport = hp.local_addr().unwrap().port();
        tokio::spawn(async move { loop { let (c, _) = hp.accept().await.unwrap(); tokio::spawn(async move {
            let mut br = BufReader::new(c); let mut l = String::new(); br.read_line(&mut l).await.unwrap();
            let mut auth = false;
            loop { let mut h = String::new(); br.read_line(&mut h).await.unwrap(); if h.starts_with("Proxy-Authorization: Basic dTpw") { auth = true; }
            if h == "\r\n" { break; } }
            let mut c = br.into_inner();
            if !auth { c.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await.unwrap(); return; }
            let t = l.split_whitespace().nth(1).unwrap().to_string();
            let mut up = TcpStream::connect(t.replace("localhost", "127.0.0.1")).await.unwrap();
            c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut c, &mut up).await;
        }); } });
        for url in [format!("socks5://127.0.0.1:{sport}"), format!("http://u:p@127.0.0.1:{hport}")] {
            let mut s = connect(&parse(&url).unwrap(), "localhost", eport).await.unwrap();
            s.write_all(b"ping").await.unwrap();
            let mut b = [0u8; 4]; s.read_exact(&mut b).await.unwrap();
            assert_eq!(&b, b"ping", "{url}");
        }
        let mut s = connect(&parse(&format!("socks5://127.0.0.1:{sport}")).unwrap(), "127.0.0.1", eport).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let mut b = [0u8; 4]; s.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"ping");
        assert!(connect(&parse(&format!("http://127.0.0.1:{hport}")).unwrap(), "localhost", eport).await.unwrap_err().to_string().contains("407"));
        let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *URL.lock().unwrap() = "socks5://1.2.3.4:1".into(); *NO_PROXY.lock().unwrap() = "10.*, localhost".into();
        assert!(for_host("10.0.0.1").unwrap().is_none() && for_host("example.com").unwrap().is_some());
        URL.lock().unwrap().clear();
    }

    /// AI requests and update checks go through ssh.proxy (HTTP CONNECT here) and honour no_proxy.
    #[test]
    fn http_clients_use_proxy_and_no_proxy() {
        use std::io::{BufRead, Read as _, Write as _};
        let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // origin: serves an update manifest and an OpenAI-style reply
        let origin = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let oport = origin.local_addr().unwrap().port();
        std::thread::spawn(move || for c in origin.incoming().flatten() {
            let mut r = std::io::BufReader::new(c.try_clone().unwrap()); let mut line = String::new(); r.read_line(&mut line).unwrap();
            let mut len = 0; loop { let mut h = String::new(); r.read_line(&mut h).unwrap(); if h.trim().is_empty() { break; }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") { len = v.trim().parse().unwrap(); } }
            let mut b = vec![0; len]; r.read_exact(&mut b).unwrap();
            let body = if line.contains("manifest") { r#"{"version":"99.0.0","url":"x","notes":""}"#.to_string() } else { r#"{"choices":[{"message":{"content":"ok"}}]}"#.to_string() };
            let mut c = c; let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        });
        // CONNECT proxy that records tunnels
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let pl = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let pport = pl.local_addr().unwrap().port();
        let seen2 = seen.clone();
        std::thread::spawn(move || for c in pl.incoming().flatten() {
            let seen = seen2.clone();
            std::thread::spawn(move || {
                let mut r = std::io::BufReader::new(c.try_clone().unwrap()); let mut line = String::new(); r.read_line(&mut line).unwrap();
                let mut hdrs = String::new();
                loop { let mut h = String::new(); r.read_line(&mut h).unwrap(); if h.trim().is_empty() { break; } hdrs.push_str(&h); }
                seen.lock().unwrap().push(line.trim().to_string());
                let mut c2 = c.try_clone().unwrap();
                let mut up = if line.starts_with("CONNECT ") {
                    let up = std::net::TcpStream::connect(line.split_whitespace().nth(1).unwrap()).unwrap();
                    c2.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").unwrap(); up
                } else { // absolute-form: GET http://h:p/path HTTP/1.1
                    let mut it = line.split_whitespace(); let (m, u) = (it.next().unwrap(), it.next().unwrap());
                    let rest = u.strip_prefix("http://").unwrap(); let (hp, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
                    let mut up = std::net::TcpStream::connect(hp).unwrap();
                    write!(up, "{m} {} HTTP/1.1\r\n{hdrs}\r\n", if path.is_empty() { "/" } else { path }).unwrap(); up
                };
                let _ = &mut up;
                let (mut a, mut b) = (up.try_clone().unwrap(), c2);
                std::thread::spawn(move || { let _ = std::io::copy(&mut a, &mut b); });
                let (mut a, mut b) = (r, up); let _ = std::io::copy(&mut a, &mut b);
            });
        });
        *URL.lock().unwrap() = format!("http://127.0.0.1:{pport}"); NO_PROXY.lock().unwrap().clear();
        let m = crate::update::check(&format!("http://127.0.0.1:{oport}/manifest.json")).unwrap();
        assert!(m.is_some());
        let p = crate::ai::Provider { base_url: format!("http://127.0.0.1:{oport}/v1"), ..Default::default() };
        let r = crate::ai::chat(&p, "k", "s", "u", Some(5)).unwrap();
        assert!(r.text.contains("ok"));
        assert_eq!(seen.lock().unwrap().len(), 2, "{:?}", seen.lock().unwrap());
        assert!(seen.lock().unwrap().iter().all(|l| l.contains(&format!("127.0.0.1:{oport}"))));
        // no_proxy: direct
        *NO_PROXY.lock().unwrap() = "127.0.0.*".into();
        assert!(crate::update::check(&format!("http://127.0.0.1:{oport}/manifest.json")).unwrap().is_some());
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!(url_host("https://user@API.example.com:443/v1"), "api.example.com");
        URL.lock().unwrap().clear(); NO_PROXY.lock().unwrap().clear();
    }

    #[test]
    fn ssh_proxy_is_opt_in_and_system_strings_parse() {
        let _g = CFG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *URL.lock().unwrap() = "socks5://127.0.0.1:1080".into();
        *NO_PROXY.lock().unwrap() = "10.*".into();
        assert!(for_ssh("example.com", false).unwrap().is_none());
        assert!(for_ssh("10.1.1.1", true).unwrap().is_none());
        assert_eq!(for_ssh("example.com", true).unwrap().unwrap().port, 1080);
        URL.lock().unwrap().clear();
        assert!(for_ssh("example.com", true).is_err());
        URL.lock().unwrap().clear();
        NO_PROXY.lock().unwrap().clear();
        let v = parse_system_server("http=127.0.0.1:7890;socks=127.0.0.1:7891");
        assert!(v.iter().any(|c| c.kind == Kind::Socks5 && c.port == 7891));
        assert!(v.iter().any(|c| c.kind == Kind::Http && c.port == 7890));
        assert_eq!(parse_system_server("127.0.0.1:7890")[0].kind, Kind::Http);
        assert_eq!(windows_bypass_to_globs("localhost;<local>;10.*"), "localhost, 10.*");
        let ip = socks5_connect_packet("136.0.57.118", 22).unwrap();
        assert_eq!(&ip[..4], &[5, 1, 0, 1]);
        assert_eq!(&ip[4..8], &[136, 0, 57, 118]);
        assert_eq!(&ip[8..], &22u16.to_be_bytes());
        let name = socks5_connect_packet("example.com", 22).unwrap();
        assert_eq!(name[3], 3);
        assert_eq!(name[4] as usize, "example.com".len());
        let url = format_url(&Proxy { kind: Kind::Socks5, host: "127.0.0.1".into(), port: 7890, user: Some(("u".into(), "p".into())) });
        assert_eq!(parse(&url).unwrap().port, 7890);
    }
}

#[cfg(test)]
mod scutil_tests {
    #[test]
    fn scutil() {
        let out = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 7890\n  HTTPProxy : 127.0.0.1\n  HTTPSEnable : 1\n  HTTPSPort : 7890\n  HTTPSProxy : 127.0.0.1\n  SOCKSEnable : 0\n}";
        let v = super::parse_scutil(out);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].host.as_str(), v[0].port), ("127.0.0.1", 7890));
    }
}
