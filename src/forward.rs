//! Port forwarding over a shared SSH `Conn`: local (-L), remote (-R) and dynamic SOCKS5 (-D).
//! Each running forward has a live status (state, active connections, bytes) and can be stopped.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::ssh::Conn;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Kind { #[default] L, R, D }

/// A saved forward (persisted per host in forwards.toml).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Spec {
    /// Host label (user@host:port) the forward belongs to.
    pub host: String,
    pub kind: Kind,
    pub bind_addr: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
    pub auto_start: bool,
}
impl Spec {
    pub fn describe(&self) -> String {
        match self.kind {
            Kind::L => format!("L {}:{} → {}:{}", self.bind_addr, self.bind_port, self.dest_host, self.dest_port),
            Kind::R => format!("R remote {}:{} → {}:{}", self.bind_addr, self.bind_port, self.dest_host, self.dest_port),
            Kind::D => format!("D SOCKS5 {}:{}", self.bind_addr, self.bind_port),
        }
    }
    /// Diagram nodes left→right: (role, title, address). Traffic flows from the first node to the
    /// last; `listen` is the index of the listening side. The SSH tunnel sits between node 0/1 and the server.
    pub fn diagram(&self, port: Option<u16>) -> (Vec<(&'static str, String, String)>, usize) {
        let bp = port.filter(|p| *p != 0).unwrap_or(self.bind_port);
        let ssh = self.host.rsplit('@').next().unwrap_or(&self.host).to_string();
        match self.kind {
            Kind::L => (vec![("client", "local".into(), format!("{}:{bp}", self.bind_addr)), ("ssh", "ssh".into(), ssh), ("dest", "remote-side".into(), format!("{}:{}", self.dest_host, self.dest_port))], 0),
            Kind::R => (vec![("client", "remote".into(), format!("{}:{bp}", self.bind_addr)), ("ssh", "ssh".into(), ssh), ("dest", "local-side".into(), format!("{}:{}", self.dest_host, self.dest_port))], 0),
            Kind::D => (vec![("client", "local".into(), format!("SOCKS5 {}:{bp}", self.bind_addr)), ("ssh", "ssh".into(), ssh), ("dest", "any".into(), "*:*".into())], 0),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.bind_addr.trim().is_empty() { bail!("bind address is empty"); }
        if self.kind != Kind::R && self.bind_port == 0 { bail!("port must be 1-65535"); }
        if self.kind != Kind::D && (self.dest_host.trim().is_empty() || self.dest_port == 0) { bail!("destination host/port required"); }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State { Stopped, Starting, Running { port: u16 }, Error(String) }

#[derive(Default)]
pub struct Stats { pub active: AtomicU64, pub total: AtomicU64, pub bytes: AtomicU64 }

pub struct Running {
    pub spec: Spec,
    pub state: Mutex<State>,
    pub stats: Stats,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    conn: Mutex<Option<Arc<Conn>>>,
}
impl Running {
    pub fn new(spec: Spec) -> Arc<Self> { Arc::new(Self { spec, state: Mutex::new(State::Stopped), stats: Stats::default(), task: Mutex::new(None), conn: Mutex::new(None) }) }
    pub fn state(&self) -> State { self.state.lock().unwrap().clone() }
    pub fn is_on(&self) -> bool { matches!(self.state(), State::Running { .. } | State::Starting) }

    /// Start on `conn`. Returns the bound port (useful when bind_port = 0).
    pub async fn start(self: &Arc<Self>, conn: Arc<Conn>) -> Result<u16> { self.start_checked(conn, true).await }
    /// Internal local tunnel: binds `bind_addr` on a port chosen by the OS (`bind_port` 0),
    /// used by database connections that go through an SSH host.
    pub async fn start_ephemeral(self: &Arc<Self>, conn: Arc<Conn>) -> Result<u16> {
        anyhow::ensure!(self.spec.kind == Kind::L && self.spec.bind_port == 0, "ephemeral tunnels are local forwards on port 0");
        self.start_checked(conn, false).await
    }
    async fn start_checked(self: &Arc<Self>, conn: Arc<Conn>, check: bool) -> Result<u16> {
        self.stop().await;
        *self.state.lock().unwrap() = State::Starting;
        let r = self.start_inner(conn.clone(), check).await;
        match &r {
            Ok(p) => { *self.state.lock().unwrap() = State::Running { port: *p }; *self.conn.lock().unwrap() = Some(conn); }
            Err(e) => *self.state.lock().unwrap() = State::Error(format!("{e:#}")),
        }
        r
    }
    async fn start_inner(self: &Arc<Self>, conn: Arc<Conn>, check: bool) -> Result<u16> {
        let s = &self.spec;
        if check { s.validate()?; }
        match s.kind {
            Kind::R => {
                let p = conn.remote_forward(&s.bind_addr, s.bind_port as u32, &s.dest_host, s.dest_port).await?;
                Ok(p as u16)
            }
            Kind::L | Kind::D => {
                let l = TcpListener::bind((s.bind_addr.as_str(), s.bind_port)).await?;
                let port = l.local_addr()?.port();
                let me = self.clone();
                let h = tokio::spawn(async move {
                    loop {
                        let Ok((tcp, _)) = l.accept().await else { break };
                        if conn.is_closed() { *me.state.lock().unwrap() = State::Error("SSH connection closed".into()); break; }
                        let me2 = me.clone();
                        let c2 = conn.clone();
                        tokio::spawn(async move {
                            me2.stats.active.fetch_add(1, Ordering::Relaxed);
                            me2.stats.total.fetch_add(1, Ordering::Relaxed);
                            let _ = me2.serve(tcp, c2).await;
                            me2.stats.active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                });
                *self.task.lock().unwrap() = Some(h);
                Ok(port)
            }
        }
    }
    async fn serve(&self, mut tcp: TcpStream, conn: Arc<Conn>) -> Result<()> {
        let _ = tcp.set_nodelay(true);
        let (host, port) = match self.spec.kind {
            Kind::D => socks5_handshake(&mut tcp).await?,
            _ => (self.spec.dest_host.clone(), self.spec.dest_port),
        };
        let mut ch = match conn.direct(&host, port).await {
            Ok(c) => { if self.spec.kind == Kind::D { tcp.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?; } c }
            Err(e) => { if self.spec.kind == Kind::D { let _ = tcp.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await; } return Err(e); }
        };
        let (a, b) = tokio::io::copy_bidirectional(&mut tcp, &mut ch).await?;
        self.stats.bytes.fetch_add(a + b, Ordering::Relaxed);
        Ok(())
    }
    pub async fn stop(&self) {
        let h = self.task.lock().unwrap().take();
        // wait for the aborted task so the listener is really closed when stop() returns
        if let Some(h) = h { h.abort(); let _ = h.await; }
        let c = self.conn.lock().unwrap().take();
        if let (Some(c), State::Running { port }) = (c, self.state()) {
            if self.spec.kind == Kind::R { let _ = c.cancel_remote_forward(&self.spec.bind_addr, port as u32).await; }
        }
        *self.state.lock().unwrap() = State::Stopped;
    }
}

/// Minimal SOCKS5 server side (RFC 1928): no-auth, CONNECT, IPv4/IPv6/domain.
pub async fn socks5_handshake(s: &mut TcpStream) -> Result<(String, u16)> {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).await?;
    if h[0] != 5 { bail!("not SOCKS5"); }
    let mut m = vec![0u8; h[1] as usize];
    s.read_exact(&mut m).await?;
    if !m.contains(&0) { s.write_all(&[5, 0xff]).await?; bail!("no acceptable auth method"); }
    s.write_all(&[5, 0]).await?;
    let mut r = [0u8; 4];
    s.read_exact(&mut r).await?;
    if r[1] != 1 { s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("only CONNECT supported"); }
    let host = match r[3] {
        1 => { let mut a = [0u8; 4]; s.read_exact(&mut a).await?; std::net::Ipv4Addr::from(a).to_string() }
        4 => { let mut a = [0u8; 16]; s.read_exact(&mut a).await?; std::net::Ipv6Addr::from(a).to_string() }
        3 => { let mut l = [0u8; 1]; s.read_exact(&mut l).await?; let mut d = vec![0u8; l[0] as usize]; s.read_exact(&mut d).await?; String::from_utf8_lossy(&d).into_owned() }
        _ => bail!("bad address type"),
    };
    let mut p = [0u8; 2];
    s.read_exact(&mut p).await?;
    Ok((host, u16::from_be_bytes(p)))
}

// ---------------------------------------------------------------- persistence

#[derive(Serialize, Deserialize, Default)]
struct File { #[serde(default)] forward: Vec<Spec> }

pub fn load(path: &std::path::Path) -> Vec<Spec> {
    std::fs::read_to_string(path).ok().and_then(|s| toml::from_str::<File>(&s).ok()).map(|f| f.forward).unwrap_or_default()
}
pub fn save(path: &std::path::Path, specs: &[Spec]) -> Result<()> {
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    std::fs::write(path, toml::to_string(&File { forward: specs.to_vec() })?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn diagram_nodes() {
        let s = super::Spec { host: "u@db:22".into(), kind: super::Kind::R, bind_addr: "0.0.0.0".into(), bind_port: 0, dest_host: "127.0.0.1".into(), dest_port: 3000, ..Default::default() };
        let (n, l) = s.diagram(Some(41234));
        assert_eq!(l, 0); assert_eq!(n[0].2, "0.0.0.0:41234"); assert_eq!(n[1].2, "db:22"); assert_eq!(n[2].2, "127.0.0.1:3000");
    }
    use super::*;
    #[test]
    fn spec_roundtrip_and_validate() {
        let s = Spec { host: "u@h:22".into(), kind: Kind::L, bind_addr: "127.0.0.1".into(), bind_port: 8080, dest_host: "localhost".into(), dest_port: 80, auto_start: true };
        s.validate().unwrap();
        assert!(Spec { dest_port: 0, ..s.clone() }.validate().is_err());
        assert!(Spec { kind: Kind::D, dest_host: String::new(), dest_port: 0, ..s.clone() }.validate().is_ok());
        let p = std::env::temp_dir().join(format!("stp-fwd-{}.toml", std::process::id()));
        save(&p, std::slice::from_ref(&s)).unwrap();
        assert_eq!(load(&p), vec![s]);
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn socks5_parse() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let a = l.local_addr().unwrap();
            let c = tokio::spawn(async move {
                let mut c = TcpStream::connect(a).await.unwrap();
                c.write_all(&[5, 1, 0]).await.unwrap();
                let mut r = [0u8; 2]; c.read_exact(&mut r).await.unwrap();
                assert_eq!(r, [5, 0]);
                c.write_all(&[5, 1, 0, 3, 9]).await.unwrap(); c.write_all(b"localhost").await.unwrap(); c.write_all(&80u16.to_be_bytes()).await.unwrap();
            });
            let (mut s, _) = l.accept().await.unwrap();
            assert_eq!(socks5_handshake(&mut s).await.unwrap(), ("localhost".into(), 80));
            c.await.unwrap();
        });
    }
}
