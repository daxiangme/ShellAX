use crate::i18n::{t, tf};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use russh::keys::{load_secret_key, HashAlg, PrivateKeyWithHashAlg};
use russh::{client, ChannelMsg};

use crate::term::{BackendMsg, Session, Size};

#[derive(Clone, Debug, Default)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Identity files to try in order.
    pub keys: Vec<PathBuf>,
    /// known_hosts file used for verification and for learning new keys.
    pub known_hosts: PathBuf,
    /// Host is marked production (stricter permission policy, red tag).
    pub production: bool,
}
impl SshTarget {
    pub fn label(&self) -> String { format!("{}@{}:{}", self.user, self.host, self.port) }
}

/// Asked of the UI when the server key is not in known_hosts (TOFU).
pub struct HostKeyPrompt {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    pub reply: flume::Sender<HostKeyDecision>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyDecision { AcceptAndSave, AcceptOnce, Reject }

#[derive(Debug, PartialEq, Eq)]
pub enum KnownHost { Match, Unknown, Changed { line: usize } }

/// Check `key` for host:port against a known_hosts file (missing file = unknown).
pub fn check_known_host(host: &str, port: u16, key: &russh::keys::PublicKey, path: &std::path::Path) -> Result<KnownHost> {
    if !path.exists() { return Ok(KnownHost::Unknown); }
    match russh::keys::check_known_hosts_path(host, port, key, path) {
        Ok(true) => Ok(KnownHost::Match),
        Ok(false) => Ok(KnownHost::Unknown),
        Err(russh::keys::Error::KeyChanged { line }) => Ok(KnownHost::Changed { line }),
        Err(e) => Err(e.into()),
    }
}

struct Handler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    prompt: flume::Sender<HostKeyPrompt>,
    /// Filled when verification fails, so the error shown is meaningful.
    failure: Arc<std::sync::Mutex<Option<String>>>,
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = k.public_key();
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        match check_known_host(&self.host, self.port, &key, &self.known_hosts)? {
            KnownHost::Match => Ok(true),
            KnownHost::Changed { line } => {
                *self.failure.lock().unwrap() = Some(tf("ssh.hostkey_changed", &[("host", &self.host), ("port", &self.port), ("fp", &fp), ("file", &self.known_hosts.display()), ("line", &line)]));
                Ok(false)
            }
            KnownHost::Unknown if HOST_KEY_POLICY.load(std::sync::atomic::Ordering::Relaxed) == 1 => {
                *self.failure.lock().unwrap() = Some(tf("ssh.strict_unknown", &[("host", &self.host), ("port", &self.port), ("fp", &fp)]));
                Ok(false)
            }
            KnownHost::Unknown if HOST_KEY_POLICY.load(std::sync::atomic::Ordering::Relaxed) == 2 => {
                russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)?;
                Ok(true)
            }
            KnownHost::Unknown => {
                let (tx, rx) = flume::bounded(1);
                self.prompt.send_async(HostKeyPrompt {
                    host: self.host.clone(), port: self.port, algorithm: key.algorithm().to_string(), fingerprint: fp, reply: tx,
                }).await.map_err(|_| anyhow!("UI gone"))?;
                match rx.recv_async().await.unwrap_or(HostKeyDecision::Reject) {
                    HostKeyDecision::AcceptAndSave => {
                        russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)?;
                        Ok(true)
                    }
                    HostKeyDecision::AcceptOnce => Ok(true),
                    HostKeyDecision::Reject => { *self.failure.lock().unwrap() = Some(t("ssh.user_rejected").into()); Ok(false) }
                }
            }
        }
    }
}

/// Runtime knobs set from the settings page (ssh.keepalive_secs / connect_timeout_secs / host_key_policy).
pub static KEEPALIVE_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(30);
pub static CONNECT_TIMEOUT_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(15);
/// 0 = ask on unknown keys (TOFU prompt), 1 = strict (reject unknown), 2 = accept-new (save without asking).
pub static HOST_KEY_POLICY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Remote history fetch: bash and zsh history, newest 2000 lines each.
const HISTORY_CMD: &str = "for f in ~/.bash_history ~/.zsh_history; do [ -r \"$f\" ] && tail -n 2000 \"$f\"; done 2>/dev/null";

/// Start connecting in the background and return the Session immediately (non-blocking).
/// Connection progress/errors are shown in the terminal and in `Session::conn_error`.
pub fn spawn_ssh(rt: &tokio::runtime::Handle, target: SshTarget, size: Size, prompt: flume::Sender<HostKeyPrompt>) -> Arc<Session> {
    let (session, out_tx, from_ui) = Session::new(size);
    let s = session.clone();
    let _ = out_tx.send(format!("{}\r\n", tf("ssh.connecting", &[("target", &target.label())])).into_bytes());
    rt.spawn(async move {
        let failure = Arc::new(std::sync::Mutex::new(None));
        let res: Result<()> = async {
            use std::sync::atomic::Ordering::Relaxed;
            let ka = KEEPALIVE_SECS.load(Relaxed);
            let cfg = Arc::new(client::Config { inactivity_timeout: None, keepalive_interval: (ka > 0).then(|| Duration::from_secs(ka)), ..Default::default() });
            let handler = Handler { host: target.host.clone(), port: target.port, known_hosts: target.known_hosts.clone(), prompt, failure: failure.clone() };
            // TCP connect is bounded by the configured timeout; the handshake (which may wait on the
            // host-key prompt) gets a separate generous bound.
            let secs = CONNECT_TIMEOUT_SECS.load(Relaxed).max(1);
            let tcp = tokio::time::timeout(Duration::from_secs(secs), tokio::net::TcpStream::connect((target.host.as_str(), target.port))).await
                .map_err(|_| anyhow!(tf("ssh.connect_timeout", &[("secs", &secs)])))??;
            let mut h = tokio::time::timeout(Duration::from_secs(300), client::connect_stream(cfg, tcp, handler)).await
                .map_err(|_| anyhow!(t("ssh.handshake_timeout")))??;
            let mut authed = false;
            let mut last_err = String::from(t("ssh.no_key"));
            for kp in &target.keys {
                let key = match load_secret_key(kp, None) { Ok(k) => k, Err(e) => { last_err = format!("{}: {e}", kp.display()); continue } };
                let hash = h.best_supported_rsa_hash().await?.flatten();
                if h.authenticate_publickey(&target.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?.success() { authed = true; break; }
                last_err = tf("ssh.key_rejected", &[("key", &kp.display())]);
            }
            if !authed { bail!(tf("ssh.pubkey_failed", &[("err", &last_err)])); }
            // remote history on a separate exec channel (read-only, best effort)
            if let Ok(mut hc) = h.channel_open_session().await {
                if hc.exec(true, HISTORY_CMD).await.is_ok() {
                    let mut body = Vec::new();
                    while let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(3), hc.wait()).await {
                        match m { ChannelMsg::Data { data } => body.extend_from_slice(&data), ChannelMsg::Eof | ChannelMsg::Close => break, _ => {} }
                    }
                    *s.history_seed.lock().unwrap() = Some(String::from_utf8_lossy(&body).into_owned());
                }
            }
            let ch = h.channel_open_session().await?;
            let sz = s.size();
            ch.request_pty(false, "xterm-256color", sz.cols as u32, sz.rows as u32, 0, 0, &[]).await?;
            ch.request_shell(false).await?;
            let (mut rd, wr) = ch.split();
            s.connected.store(true, Ordering::Release);
            let _ = out_tx.send(b"\x1b[2K\r".to_vec());
            loop {
                tokio::select! {
                    m = from_ui.recv_async() => match m {
                        Ok(BackendMsg::Input(b)) => wr.data_bytes(b).await?,
                        Ok(BackendMsg::Resize { cols, rows }) => wr.window_change(cols as u32, rows as u32, 0, 0).await?,
                        Err(_) => break,
                    },
                    m = rd.wait() => match m {
                        Some(ChannelMsg::Data { data }) => { let _ = out_tx.send(data.to_vec()); }
                        Some(ChannelMsg::ExtendedData { data, .. }) => { let _ = out_tx.send(data.to_vec()); }
                        Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                        _ => {}
                    },
                }
            }
            drop(h);
            Ok(())
        }
        .await;
        if let Err(e) = res {
            let msg = failure.lock().unwrap().clone().unwrap_or_else(|| format!("{e:#}"));
            let _ = out_tx.send(format!("\r\n\x1b[31m[ssh] {msg}\x1b[0m\r\n").into_bytes());
            *s.conn_error.lock().unwrap() = Some(msg);
            s.poke();
        }
        s.connected.store(false, Ordering::Release);
        // dropping out_tx ends the parser thread -> exited
    });
    session
}
