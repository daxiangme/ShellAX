use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg};
use russh::{client, ChannelMsg};

use crate::term::{BackendMsg, Session, Size};

pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key: PathBuf,
}

struct Handler;
impl client::Handler for Handler {
    type Error = russh::Error;
    // PROTOTYPE ONLY: accepts any host key. The product must verify known_hosts / TOFU.
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Connect, authenticate with a key, open a PTY shell and wire it to a new Session.
/// Runs on the given tokio runtime; returns once the shell is open.
pub fn spawn_ssh(rt: &tokio::runtime::Handle, target: SshTarget, size: Size) -> Result<Arc<Session>> {
    let (session, out_tx, from_ui) = Session::new(size);
    let session_for_hist = session.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    rt.spawn(async move {
        let res: Result<()> = async {
            let cfg = Arc::new(client::Config { inactivity_timeout: None, keepalive_interval: Some(Duration::from_secs(30)), ..Default::default() });
            let mut h = client::connect(cfg, (target.host.as_str(), target.port), Handler).await?;
            let key = load_secret_key(&target.key, None)?;
            let hash = h.best_supported_rsa_hash().await?.flatten();
            let auth = h.authenticate_publickey(&target.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?;
            if !auth.success() { bail!("public key authentication failed"); }
            // fetch remote ~/.bash_history on a separate exec channel (read-only, best effort)
            if let Ok(mut hc) = h.channel_open_session().await {
                if hc.exec(true, "cat ~/.bash_history 2>/dev/null | tail -n 2000").await.is_ok() {
                    let mut body = Vec::new();
                    while let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(3), hc.wait()).await {
                        match m { ChannelMsg::Data { data } => body.extend_from_slice(&data), ChannelMsg::Eof | ChannelMsg::Close => break, _ => {} }
                    }
                    *session_for_hist.history_seed.lock().unwrap() = Some(String::from_utf8_lossy(&body).into_owned());
                }
            }
            let ch = h.channel_open_session().await?;
            ch.request_pty(false, "xterm-256color", size.cols as u32, size.rows as u32, 0, 0, &[]).await?;
            ch.request_shell(false).await?;
            let (mut rd, wr) = ch.split();
            let _ = ready_tx.send(Ok(()));
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
            let msg = format!("\r\n[ssh error] {e:#}\r\n");
            let _ = out_tx.send(msg.into_bytes());
            let _ = ready_tx.send(Err(e));
        }
    });
    match ready_rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(())) => Ok(session),
        Ok(Err(e)) => Err(e),
        Err(_) => bail!("ssh connect timed out"),
    }
}
