use crate::i18n::{t, tf};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use russh::keys::{load_secret_key, HashAlg, PrivateKeyWithHashAlg};
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
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
    /// "auto" (agent → keys → password), "key", "agent" or "password".
    pub auth: String,
    /// ProxyJump: connect through this host first (direct-tcpip channel).
    pub jump: Option<Box<SshTarget>>,
    /// Request ssh-agent forwarding on the shell channel.
    pub agent_forward: bool,
    /// Preferred key-exchange / cipher algorithm names (empty = russh defaults).
    pub kex: Vec<String>,
    pub ciphers: Vec<String>,
    /// Reverse-forward the local policy service so agent hooks on the remote host can reach it.
    pub remote_hooks: bool,
}
impl SshTarget {
    pub fn label(&self) -> String { format!("{}@{}:{}", self.user, self.host, self.port) }
    /// Pool key: same user/host/port/jump chain → same connection.
    pub fn pool_key(&self) -> String {
        match &self.jump { Some(j) => format!("{}>{}", j.pool_key(), self.label()), None => self.label() }
    }
    pub fn password_secret(&self) -> String { format!("ssh-password:{}", self.label()) }
}
pub fn passphrase_secret(path: &std::path::Path) -> String { format!("ssh-passphrase:{}", path.display()) }

/// Asked of the UI when a password or key passphrase is needed. Reply `Some((secret, save))`
/// or `None` to cancel. The secret stays inside this process (never in config, logs, audit,
/// or anything an agent can read); with `save` it goes to the OS keychain (or encrypted file).
pub struct AuthPrompt { pub label: String, pub kind: AuthKind, pub reply: flume::Sender<Option<(String, bool)>> }
#[derive(Debug, Clone, PartialEq)]
pub enum AuthKind { Password, Passphrase(PathBuf) }

/// Channels to the UI for interactive questions during connect.
#[derive(Clone)]
pub struct Prompts { pub hostkey: flume::Sender<HostKeyPrompt>, pub auth: flume::Sender<AuthPrompt> }
impl Prompts {
    /// Prompts that auto-reject (headless / tests).
    pub fn reject_all() -> Self { let (a, _) = flume::unbounded(); let (b, _) = flume::unbounded(); Self { hostkey: a, auth: b } }
}

/// Secret store used for saved SSH passwords / passphrases (set once by the app).
pub static SECRETS: std::sync::OnceLock<crate::secrets::SecretStore> = std::sync::OnceLock::new();

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

/// Remote (-R) forwards: server-side bound port → local destination.
pub type ForwardMap = Arc<StdMutex<HashMap<u32, (String, u16)>>>;

pub struct Handler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    prompt: flume::Sender<HostKeyPrompt>,
    /// Filled when verification fails, so the error shown is meaningful.
    failure: Arc<std::sync::Mutex<Option<String>>>,
    forwards: ForwardMap,
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
                if self.prompt.send_async(HostKeyPrompt {
                    host: self.host.clone(), port: self.port, algorithm: key.algorithm().to_string(), fingerprint: fp, reply: tx,
                }).await.is_err() { *self.failure.lock().unwrap() = Some(t("ssh.user_rejected").into()); return Ok(false); }
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
    async fn server_channel_open_forwarded_tcpip(&mut self, channel: russh::Channel<client::Msg>, _ca: &str, connected_port: u32, _oa: &str, _op: u32, reply: client::ChannelOpenHandle, _s: &mut client::Session) -> Result<(), Self::Error> {
        let dest = self.forwards.lock().unwrap().get(&connected_port).cloned();
        let Some((h, p)) = dest else { drop(reply); return Ok(()) };
        reply.accept().await;
        tokio::spawn(async move {
            if let Ok(mut tcp) = tokio::net::TcpStream::connect((h.as_str(), p)).await {
                let mut st = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut st).await;
            }
        });
        Ok(())
    }
    async fn server_channel_open_agent_forward(&mut self, channel: russh::Channel<client::Msg>, reply: client::ChannelOpenHandle, _s: &mut client::Session) -> Result<(), Self::Error> {
        #[cfg(unix)]
        if let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") {
            reply.accept().await;
            tokio::spawn(async move {
                if let Ok(mut u) = tokio::net::UnixStream::connect(sock).await {
                    let mut st = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut u, &mut st).await;
                }
            });
            return Ok(());
        }
        drop(reply);
        Ok(())
    }
}

/// An authenticated SSH connection, shareable by several terminals, SFTP and forwards.
pub struct Conn {
    pub handle: client::Handle<Handler>,
    pub label: String,
    pub forwards: ForwardMap,
    /// Keeps the jump connection alive.
    _jump: Option<Arc<Conn>>,
}

impl Conn {
    pub fn is_closed(&self) -> bool { self.handle.is_closed() }
    /// Run a command; returns (exit status, stdout+stderr). Bounded by `timeout`.
    pub async fn exec(&self, cmd: &str, timeout: Duration) -> Result<(Option<u32>, Vec<u8>)> {
        let mut ch = self.handle.channel_open_session().await?;
        ch.exec(true, cmd).await?;
        let mut body = Vec::new();
        let mut code = None;
        let fut = async {
            while let Some(m) = ch.wait().await {
                match m {
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => body.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                    ChannelMsg::Eof => {}
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
        };
        let _ = tokio::time::timeout(timeout, fut).await;
        Ok((code, body))
    }
    /// Write a file on the remote host through `cat` with a private umask.
    pub async fn write_file(&self, path: &str, content: &[u8]) -> Result<()> {
        let mut ch = self.handle.channel_open_session().await?;
        let dir = path.rsplit_once('/').map(|x| x.0).unwrap_or(".");
        ch.exec(true, format!("umask 077; mkdir -p {d} && cat > {p}", d = sh_quote(dir), p = sh_quote(path))).await?;
        ch.data(content).await?;
        ch.eof().await?;
        let mut code = None;
        let _ = tokio::time::timeout(Duration::from_secs(10), async { while let Some(m) = ch.wait().await { if let ChannelMsg::ExitStatus { exit_status } = m { code = Some(exit_status); } if matches!(m, ChannelMsg::Close) { break; } } }).await;
        if code.unwrap_or(1) != 0 { bail!("remote write {path} failed"); }
        Ok(())
    }
    /// -R: ask the server to listen on `bind:port` (0 = any free port) and forward to `to`.
    pub async fn remote_forward(&self, bind: &str, port: u32, to_host: &str, to_port: u16) -> Result<u32> {
        let got = self.handle.tcpip_forward(bind, port).await?;
        let actual = if port == 0 { got } else { port };
        self.forwards.lock().unwrap().insert(actual, (to_host.to_string(), to_port));
        Ok(actual)
    }
    pub async fn cancel_remote_forward(&self, bind: &str, port: u32) -> Result<()> {
        self.forwards.lock().unwrap().remove(&port);
        self.handle.cancel_tcpip_forward(bind, port).await?;
        Ok(())
    }
    /// Open a direct-tcpip channel as a byte stream.
    pub async fn direct(&self, host: &str, port: u16) -> Result<russh::ChannelStream<client::Msg>> {
        let ch = self.handle.channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0).await?;
        Ok(ch.into_stream())
    }
    /// SFTP session on a new channel.
    pub async fn sftp(&self) -> Result<russh_sftp::client::SftpSession> {
        let ch = self.handle.channel_open_session().await?;
        ch.request_subsystem(true, "sftp").await?;
        Ok(russh_sftp::client::SftpSession::new(ch.into_stream()).await?)
    }
}

pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:@,".contains(c)) { s.into() } else { format!("'{}'", s.replace('\'', "'\\''")) }
}

fn preferred(t: &SshTarget) -> russh::Preferred {
    let mut p = russh::Preferred::default();
    if !t.kex.is_empty() {
        let v: Vec<russh::kex::Name> = t.kex.iter().filter_map(|n| russh::kex::Name::try_from(n.as_str()).ok()).collect();
        if !v.is_empty() { p.kex = std::borrow::Cow::Owned(v); }
    }
    if !t.ciphers.is_empty() {
        let v: Vec<russh::cipher::Name> = t.ciphers.iter().filter_map(|n| russh::cipher::Name::try_from(n.as_str()).ok()).collect();
        if !v.is_empty() { p.cipher = std::borrow::Cow::Owned(v); }
    }
    p
}

async fn ask(prompts: &Prompts, label: String, kind: AuthKind) -> Option<(String, bool)> {
    let (tx, rx) = flume::bounded(1);
    prompts.auth.send_async(AuthPrompt { label, kind, reply: tx }).await.ok()?;
    rx.recv_async().await.ok().flatten()
}

/// Connect and authenticate (recursively through `jump`).
pub fn connect<'a>(tg: &'a SshTarget, prompts: &'a Prompts) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Arc<Conn>>> + Send + 'a>> {
    Box::pin(async move {
        use std::sync::atomic::Ordering::Relaxed;
        let failure = Arc::new(std::sync::Mutex::new(None));
        let ka = KEEPALIVE_SECS.load(Relaxed);
        let cfg = Arc::new(client::Config { inactivity_timeout: None, keepalive_interval: (ka > 0).then(|| Duration::from_secs(ka)), keepalive_max: 3, preferred: preferred(tg), ..Default::default() });
        let forwards: ForwardMap = Default::default();
        let handler = Handler { host: tg.host.clone(), port: tg.port, known_hosts: tg.known_hosts.clone(), prompt: prompts.hostkey.clone(), failure: failure.clone(), forwards: forwards.clone() };
        let secs = CONNECT_TIMEOUT_SECS.load(Relaxed).max(1);
        let res: Result<(client::Handle<Handler>, Option<Arc<Conn>>)> = async {
            if let Some(j) = &tg.jump {
                let jc = connect(j, prompts).await.map_err(|e| anyhow!(tf("ssh.jump_failed", &[("jump", &j.label()), ("err", &format!("{e:#}"))])))?;
                let stream = tokio::time::timeout(Duration::from_secs(secs), jc.direct(&tg.host, tg.port)).await.map_err(|_| anyhow!(tf("ssh.connect_timeout", &[("secs", &secs)])))??;
                let h = tokio::time::timeout(Duration::from_secs(300), client::connect_stream(cfg, stream, handler)).await.map_err(|_| anyhow!(t("ssh.handshake_timeout")))??;
                Ok((h, Some(jc)))
            } else {
                let tcp = tokio::time::timeout(Duration::from_secs(secs), tokio::net::TcpStream::connect((tg.host.as_str(), tg.port))).await
                    .map_err(|_| anyhow!(tf("ssh.connect_timeout", &[("secs", &secs)])))??;
                let _ = tcp.set_nodelay(true);
                let h = tokio::time::timeout(Duration::from_secs(300), client::connect_stream(cfg, tcp, handler)).await.map_err(|_| anyhow!(t("ssh.handshake_timeout")))??;
                Ok((h, None))
            }
        }.await;
        let (mut h, jump) = match res { Ok(x) => x, Err(e) => return Err(match failure.lock().unwrap().clone() { Some(f) => anyhow!(f), None => e }) };
        authenticate(&mut h, tg, prompts).await?;
        Ok(Arc::new(Conn { handle: h, label: tg.label(), forwards, _jump: jump }))
    })
}

async fn authenticate(h: &mut client::Handle<Handler>, tg: &SshTarget, prompts: &Prompts) -> Result<()> {
    let (mode, no_agent) = match tg.auth.as_str() { "" => ("auto", false), "auto-noagent" => ("auto", true), m => (m, false) };
    let mut errors: Vec<String> = Vec::new();
    // 1. ssh-agent
    #[cfg(unix)]
    if matches!(mode, "auto" | "agent") && !no_agent {
        match russh::keys::agent::client::AgentClient::connect_env().await {
            Ok(mut agent) => {
                let ids = agent.request_identities().await.unwrap_or_default();
                if ids.is_empty() { errors.push(t("ssh.agent_empty").into()); }
                for id in ids {
                    let key = match &id { russh::keys::agent::AgentIdentity::PublicKey { key, .. } => key.clone(), _ => continue };
                    let hash = h.best_supported_rsa_hash().await?.flatten();
                    match h.authenticate_publickey_with(&tg.user, key, hash, &mut agent).await {
                        Ok(r) if r.success() => return Ok(()),
                        Ok(_) => {}
                        Err(e) => errors.push(format!("agent: {e}")),
                    }
                }
            }
            Err(e) => if mode == "agent" { errors.push(tf("ssh.agent_unavailable", &[("err", &e)])); },
        }
    }
    // 2. key files (passphrase from keychain or prompt)
    if matches!(mode, "auto" | "key") {
        for kp in &tg.keys {
            if !kp.exists() { if mode == "key" { errors.push(format!("{}: not found", kp.display())); } continue; }
            let key = match load_secret_key(kp, None) {
                Ok(k) => k,
                Err(russh::keys::Error::KeyIsEncrypted) => {
                    let saved = SECRETS.get().and_then(|s| s.get(&passphrase_secret(kp))).map(|x| x.0);
                    let mut loaded = None;
                    if let Some(p) = saved { loaded = load_secret_key(kp, Some(&p)).ok(); }
                    let mut tries = 0;
                    while loaded.is_none() && tries < 3 {
                        tries += 1;
                        let Some((p, save)) = ask(prompts, kp.display().to_string(), AuthKind::Passphrase(kp.clone())).await else { break };
                        if let Ok(k) = load_secret_key(kp, Some(&p)) {
                            if save { if let Some(s) = SECRETS.get() { let _ = s.set(&passphrase_secret(kp), &p); } }
                            loaded = Some(k);
                        }
                    }
                    match loaded { Some(k) => k, None => { errors.push(tf("ssh.passphrase_failed", &[("key", &kp.display())])); continue } }
                }
                Err(e) => { errors.push(format!("{}: {e}", kp.display())); continue }
            };
            let hash = h.best_supported_rsa_hash().await?.flatten();
            if h.authenticate_publickey(&tg.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?.success() { return Ok(()); }
            errors.push(tf("ssh.key_rejected", &[("key", &kp.display())]));
        }
    }
    // 3. password (keychain first, then prompt up to 3 times)
    if matches!(mode, "auto" | "password") {
        let name = tg.password_secret();
        if let Some((pw, _)) = SECRETS.get().and_then(|s| s.get(&name)) {
            if h.authenticate_password(&tg.user, pw).await?.success() { return Ok(()); }
            errors.push(t("ssh.saved_password_rejected").into());
        }
        for _ in 0..3 {
            let Some((pw, save)) = ask(prompts, tg.label(), AuthKind::Password).await else { break };
            if h.authenticate_password(&tg.user, pw.clone()).await?.success() {
                if save { if let Some(s) = SECRETS.get() { let _ = s.set(&name, &pw); } }
                return Ok(());
            }
            errors.push(t("ssh.password_rejected").into());
        }
    }
    if errors.is_empty() { errors.push(t("ssh.no_key").into()); }
    bail!(tf("ssh.auth_failed", &[("err", &errors.join("; "))]))
}

/// Connection reuse (PRD: multiplexing): one SSH connection per user@host:port (+jump chain).
static POOL: std::sync::LazyLock<tokio::sync::Mutex<HashMap<String, std::sync::Weak<Conn>>>> = std::sync::LazyLock::new(Default::default);
pub static REUSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub async fn connect_shared(t: &SshTarget, prompts: &Prompts) -> Result<(Arc<Conn>, bool)> {
    let key = t.pool_key();
    let mut pool = POOL.lock().await;
    if REUSE.load(Ordering::Relaxed) {
        if let Some(c) = pool.get(&key).and_then(|w| w.upgrade()) { if !c.is_closed() { return Ok((c, true)); } }
    }
    let c = connect(t, prompts).await?;
    pool.insert(key, Arc::downgrade(&c));
    Ok((c, false))
}

/// Runtime knobs set from the settings page (ssh.keepalive_secs / connect_timeout_secs / host_key_policy).
pub static KEEPALIVE_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(30);
pub static CONNECT_TIMEOUT_SECS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(15);
/// 0 = ask on unknown keys (TOFU prompt), 1 = strict (reject unknown), 2 = accept-new (save without asking).
pub static HOST_KEY_POLICY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Remote history fetch: bash and zsh history, newest 2000 lines each.
const HISTORY_CMD: &str = "for f in ~/.bash_history ~/.zsh_history; do [ -r \"$f\" ] && tail -n 2000 \"$f\"; done 2>/dev/null";

/// Auto-reconnect after an unexpected disconnect (ssh.reconnect).
pub static RECONNECT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// Policy service port on this machine (0 = none) and its token, for remote hook forwarding.
pub static POLICY_ENDPOINT: StdMutex<Option<(u16, String)>> = StdMutex::new(None);

/// Start connecting in the background and return the Session immediately (non-blocking).
/// Connection progress/errors are shown in the terminal and in `Session::conn_error`.
pub fn spawn_ssh(rt: &tokio::runtime::Handle, target: SshTarget, size: Size, prompts: Prompts) -> Arc<Session> {
    let (session, out_tx, from_ui) = Session::new(size);
    let s = session.clone();
    let _ = out_tx.send(format!("{}\r\n", tf("ssh.connecting", &[("target", &target.label())])).into_bytes());
    rt.spawn(async move {
        let mut attempt = 0u32;
        loop {
            let res = run_shell(&target, &prompts, &s, &out_tx, &from_ui, attempt > 0).await;
            s.connected.store(false, Ordering::Release);
            if matches!(res, Ok(ShellEnd::Exited) | Ok(ShellEnd::UiGone)) { break; }
            let retryable = match &res { Ok(_) => true, Err(e) => attempt > 0 && !is_fatal(e) };
            if retryable && RECONNECT.load(Ordering::Relaxed) && attempt < 5 {
                attempt += 1;
                let wait = 2u64.pow(attempt.min(4));
                if let Err(e) = &res { let _ = out_tx.send(format!("\r\n\x1b[33m[ssh] {e:#}\x1b[0m").into_bytes()); }
                let _ = out_tx.send(format!("\r\n\x1b[33m[ssh] {}\x1b[0m\r\n", tf("ssh.reconnecting", &[("secs", &wait), ("n", &attempt)])).into_bytes());
                s.poke();
                // wait, but stop if the UI closes the pane (keystrokes while waiting are dropped)
                let sleep = tokio::time::sleep(Duration::from_secs(wait));
                tokio::pin!(sleep);
                let mut gone = false;
                loop { tokio::select! { _ = &mut sleep => break, m = from_ui.recv_async() => if m.is_err() { gone = true; break } } }
                if gone { break; }
                continue;
            }
            let msg = match res { Ok(_) => t("ssh.connection_lost").to_string(), Err(e) => format!("{e:#}") };
            let _ = out_tx.send(format!("\r\n\x1b[31m[ssh] {msg}\x1b[0m\r\n").into_bytes());
            *s.conn_error.lock().unwrap() = Some(msg);
            s.poke();
            break;
        }
        // dropping out_tx ends the parser thread -> exited
    });
    session
}

fn is_fatal(e: &anyhow::Error) -> bool {
    let m = format!("{e:#}");
    // authentication / host-key problems will not fix themselves
    let auth = tf("ssh.auth_failed", &[("err", &"")]);
    m.contains(t("ssh.user_rejected")) || m.contains("known_hosts") || m.starts_with(auth.trim())
}

enum ShellEnd { Exited, Lost, UiGone }

async fn run_shell(target: &SshTarget, prompts: &Prompts, s: &Arc<Session>, out_tx: &flume::Sender<Vec<u8>>, from_ui: &flume::Receiver<BackendMsg>, reconnect: bool) -> Result<ShellEnd> {
    let (conn, reused) = connect_shared(target, prompts).await?;
    if reused { let _ = out_tx.send(format!("{}\r\n", t("ssh.reused")).into_bytes()); }
    if reconnect { let _ = out_tx.send(format!("\x1b[32m[ssh] {}\x1b[0m\r\n", t("ssh.reconnected")).into_bytes()); }
    // remote history on a separate exec channel (read-only, best effort)
    if s.history_seed.lock().unwrap().is_none() {
        if let Ok((_, body)) = conn.exec(HISTORY_CMD, Duration::from_secs(3)).await {
            *s.history_seed.lock().unwrap() = Some(String::from_utf8_lossy(&body).into_owned());
        }
    }
    // remote agent hooks → local policy service (reverse forward + endpoint file on the remote)
    if target.remote_hooks && !reused {
        let ep = POLICY_ENDPOINT.lock().unwrap().clone();
        if let Some((lport, token)) = ep {
            match conn.remote_forward("127.0.0.1", 0, "127.0.0.1", lport).await {
                Ok(rport) => {
                    let host = if target.host.is_empty() { target.label() } else { target.host.clone() };
                    let body = serde_json::json!({"url": format!("http://127.0.0.1:{rport}"), "token": token, "pid": 0, "host": host}).to_string();
                    let ok = conn.write_file(".local/share/agentshell/policy-endpoint.json", body.as_bytes()).await.is_ok();
                    let _ = out_tx.send(format!("\x1b[2m[agentshell] {}\x1b[0m\r\n", tf(if ok { "ssh.remote_hooks_on" } else { "ssh.remote_hooks_write_failed" }, &[("port", &rport)])).into_bytes());
                }
                Err(e) => { let _ = out_tx.send(format!("\x1b[33m[agentshell] {}\x1b[0m\r\n", tf("ssh.remote_hooks_failed", &[("err", &e)])).into_bytes()); }
            }
        }
    }
    let ch = conn.handle.channel_open_session().await?;
    if target.agent_forward { let _ = ch.agent_forward(false).await; }
    if target.remote_hooks { let _ = ch.set_env(false, "AGENTSHELL_HOST", target.host.clone()).await; }
    let sz = s.size();
    ch.request_pty(false, "xterm-256color", sz.cols as u32, sz.rows as u32, 0, 0, &[]).await?;
    ch.request_shell(false).await?;
    let (mut rd, wr) = ch.split();
    s.connected.store(true, Ordering::Release);
    *s.conn_error.lock().unwrap() = None;
    if !reconnect { let _ = out_tx.send(b"\x1b[2K\r".to_vec()); }
    let mut exited = false;
    loop {
        tokio::select! {
            m = from_ui.recv_async() => match m {
                Ok(BackendMsg::Input(b)) => { if wr.data_bytes(b).await.is_err() { return Ok(ShellEnd::Lost) } }
                Ok(BackendMsg::Resize { cols, rows }) => { let _ = wr.window_change(cols as u32, rows as u32, 0, 0).await; }
                Err(_) => return Ok(ShellEnd::UiGone),
            },
            m = rd.wait() => match m {
                Some(ChannelMsg::Data { data }) => { let _ = out_tx.send(data.to_vec()); }
                Some(ChannelMsg::ExtendedData { data, .. }) => { let _ = out_tx.send(data.to_vec()); }
                Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::ExitSignal { .. }) => exited = true,
                Some(ChannelMsg::Eof) => if exited { return Ok(ShellEnd::Exited) },
                Some(ChannelMsg::Close) => return Ok(if exited || !conn.is_closed() { ShellEnd::Exited } else { ShellEnd::Lost }),
                None => return Ok(if exited { ShellEnd::Exited } else { ShellEnd::Lost }),
                _ => {}
            },
        }
    }
}
