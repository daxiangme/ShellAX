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

/// The proxy to use for `host`, if any.
pub fn for_host(host: &str) -> Result<Option<Proxy>> {
    let url = URL.lock().unwrap().clone();
    if url.trim().is_empty() { return Ok(None); }
    let np = NO_PROXY.lock().unwrap().clone();
    if np.split(',').map(str::trim).filter(|p| !p.is_empty()).any(|p| p == host || crate::policy::glob(p, host)) { return Ok(None); }
    parse(&url).map(Some)
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
            if host.len() > 255 { bail!("host name too long"); }
            let mut req = vec![5, 1, 0, 3, host.len() as u8]; req.extend_from_slice(host.as_bytes()); req.extend_from_slice(&port.to_be_bytes());
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
            let mut len = 0; loop { let mut h = String::new(); r.read_line(&mut h).unwrap(); if h.trim().is_empty() { break; } if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") { len = v.trim().parse().unwrap(); } }
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
}
