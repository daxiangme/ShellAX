//! A live database connection: the driver plus, for connections that go through an SSH
//! host, the local tunnel (direct-tcpip over the shared SSH connection) it rides on.
use std::sync::Arc;

use anyhow::{Context, Result};

use super::Driver;
use crate::assets::DbConn;
use crate::forward::{Kind, Running, Spec};
use crate::ssh::{self, Prompts, SshTarget};

pub struct Live {
    pub driver: Driver,
    pub version: String,
    /// `user@host:port` of the SSH host when tunnelled.
    pub via: Option<String>,
    tunnel: Option<Arc<Running>>,
}

impl Live {
    pub async fn close(&self) {
        let _ = self.driver.close().await;
        if let Some(t) = &self.tunnel { t.stop().await; }
    }
}

/// Connect `c`. With `via`, an SSH connection to that host is opened (or reused) and the
/// database is reached through a 127.0.0.1 tunnel on a free port.
pub async fn open(c: &DbConn, password: Option<String>, via: Option<(SshTarget, Prompts)>) -> Result<Live> {
    let mut tunnel = None;
    let mut endpoint = None;
    let mut via_label = None;
    if let Some((t, prompts)) = via.filter(|_| !c.kind.is_file()) {
        let (r, port) = tunnel_to(&t, &prompts, &c.host, c.effective_port()).await?;
        endpoint = Some(("127.0.0.1".to_string(), port));
        via_label = Some(t.label());
        tunnel = Some(r);
    }
    let driver = match super::driver::connect(c, password, endpoint).await {
        Ok(d) => d,
        Err(e) => { if let Some(t) = &tunnel { t.stop().await; } return Err(e); }
    };
    let version = driver.server_version().await.unwrap_or_default();
    Ok(Live { driver, version, via: via_label, tunnel })
}

/// Local 127.0.0.1 port forwarded through the (shared) SSH connection of `t` to `host:port`.
pub async fn tunnel_to(t: &SshTarget, prompts: &Prompts, host: &str, port: u16) -> Result<(Arc<Running>, u16)> {
    let (conn, _) = ssh::connect_shared(t, prompts).await.with_context(|| format!("SSH {}", t.label()))?;
    let r = Running::new(Spec { host: t.label(), kind: Kind::L, bind_addr: "127.0.0.1".into(), bind_port: 0, dest_host: host.to_string(), dest_port: port, auto_start: false });
    let p = r.start_ephemeral(conn).await.context("SSH tunnel")?;
    Ok((r, p))
}
