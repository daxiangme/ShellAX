//! Agent-facing remote operations (PRD US-10: `shellax-cli exec / hosts / upload / download`).
//! The policy service calls an [`Ops`] implementation provided by the App, so the agent only
//! ever names a host from the App's host list: credentials, keys and passwords stay inside the
//! App, and the App's connection pool is reused (one SSH connection per user@host:port).
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::ssh::{self, Prompts, SshTarget};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostInfo { pub name: String, pub user: String, pub host: String, pub port: u16, pub production: bool }

/// Operations the policy service may perform after the permission engine allowed them.
pub trait Ops: Send + Sync {
    fn hosts(&self) -> Vec<HostInfo>;
    /// Run a command; returns exit status and combined stdout/stderr.
    fn exec(&self, host: &str, command: &str, timeout: Duration) -> Result<(Option<u32>, Vec<u8>)>;
    /// Copy a local file to the remote path; returns bytes transferred.
    fn upload(&self, host: &str, local: &Path, remote: &str) -> Result<u64>;
    fn download(&self, host: &str, remote: &str, local: &Path) -> Result<u64>;
}

pub type TargetsFn = Arc<dyn Fn() -> Vec<(String, SshTarget)> + Send + Sync>;

/// SSH-backed [`Ops`] over the shared connection pool.
pub struct SshOps { pub rt: tokio::runtime::Handle, pub targets: TargetsFn, pub prompts: Prompts }

impl SshOps {
    /// Host name lookup: the configured host name, or the target's host/address.
    pub fn target(&self, name: &str) -> Result<SshTarget> {
        let all = (self.targets)();
        all.iter().find(|(n, _)| n == name).or_else(|| all.iter().find(|(_, t)| t.host == name || t.label() == name))
            .map(|(_, t)| t.clone()).ok_or_else(|| anyhow!("unknown host {name:?} (see `shellax-cli hosts`)"))
    }
    fn conn(&self, name: &str) -> Result<Arc<ssh::Conn>> {
        let t = self.target(name)?;
        let p = self.prompts.clone();
        self.rt.block_on(async move { ssh::connect_shared(&t, &p).await.map(|(c, _)| c) })
    }
    fn transfer(&self, host: &str, dir: crate::sftp::Dir, local: &Path, remote: &str) -> Result<u64> {
        let c = self.conn(host)?;
        let q = crate::sftp::Queue::default();
        let job = q.push(dir, local.to_path_buf(), remote.to_string());
        self.rt.block_on(async move {
            let s = c.sftp().await?;
            crate::sftp::run_job(&s, &job).await?;
            Ok(job.done.load(std::sync::atomic::Ordering::Relaxed))
        })
    }
}

impl Ops for SshOps {
    fn hosts(&self) -> Vec<HostInfo> {
        (self.targets)().into_iter().map(|(name, t)| HostInfo { name, user: t.user, host: t.host, port: t.port, production: t.production }).collect()
    }
    fn exec(&self, host: &str, command: &str, timeout: Duration) -> Result<(Option<u32>, Vec<u8>)> {
        let c = self.conn(host)?;
        let cmd = command.to_string();
        self.rt.block_on(async move { c.exec(&cmd, timeout).await })
    }
    fn upload(&self, host: &str, local: &Path, remote: &str) -> Result<u64> {
        if !local.is_file() { return Err(anyhow!("{}: not a file", local.display())); }
        self.transfer(host, crate::sftp::Dir::Upload, local, remote)
    }
    fn download(&self, host: &str, remote: &str, local: &Path) -> Result<u64> { self.transfer(host, crate::sftp::Dir::Download, local, remote) }
}
