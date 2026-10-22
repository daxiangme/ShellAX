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

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
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
    /// Persistent session: run the shell inside `tmux new -A -s <name>` on the remote.
    pub tmux: Option<String>,
    /// OpenSSH certificate (`*-cert.pub`) used with the first identity file ("cert" auth step).
    pub cert: Option<PathBuf>,
    /// This hop's TCP connection goes through the configured outbound proxy.
    pub use_proxy: bool,
    /// Unsaved password / passphrase from the host dialog ("Test connection"); never persisted.
    #[serde(skip)]
    pub password_override: Option<String>,
    #[serde(skip)]
    pub passphrase_override: Option<String>,
}
impl SshTarget {
    pub fn label(&self) -> String { format!("{}@{}:{}", self.user, self.host, self.port) }
    /// Pool key: same user/host/port/jump chain → same connection.
    pub fn pool_key(&self) -> String {
        let via = if self.use_proxy { "+proxy" } else { "" };
        match &self.jump { Some(j) => format!("{}>{}{via}", j.pool_key(), self.label()), None => format!("{}{via}", self.label()) }
    }
    /// Outermost hop: the TCP connection that leaves this machine.
    pub fn first_hop(&self) -> &SshTarget {
        let mut cur = self;
        while let Some(j) = cur.jump.as_ref() { cur = j; }
        cur
    }
    /// `Some` when this connection asked for a proxy and none is configured (or the URL is invalid).
    /// Matching `ssh.no_proxy` is not a block: that hop connects directly on purpose.
    pub fn proxy_block_message(&self) -> Option<String> {
        let hop = self.first_hop();
        match crate::proxy::for_ssh(&hop.host, hop.use_proxy) {
            Err(e) => Some(format!("{e:#}")),
            Ok(_) => None,
        }
    }
    pub fn password_secret(&self) -> String { format!("ssh-password:{}", self.label()) }
}
pub fn passphrase_secret(path: &std::path::Path) -> String { format!("ssh-passphrase:{}", path.display()) }

/// Asked of the UI when a password or key passphrase is needed. Reply `Some((secret, save))`
/// or `None` to cancel. The secret stays inside this process (never in config, logs, audit,
/// or anything an agent can read); with `save` it goes to the OS keychain (or encrypted file).
pub struct AuthPrompt { pub label: String, pub kind: AuthKind, pub reply: flume::Sender<Option<(String, bool)>> }
#[derive(Debug, Clone, PartialEq)]
pub enum AuthKind { Password, Passphrase(PathBuf), /// keyboard-interactive (2FA / OTP / PAM) prompt; never saved
    Interactive { instructions: String, prompt: String, echo: bool } }

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
    /// "algorithm SHA256:…" of the server key, recorded for "Test connection".
    seen: Arc<std::sync::Mutex<Option<String>>>,
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = k.public_key();
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        *self.seen.lock().unwrap() = Some(format!("{} {fp}", key.algorithm()));
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
        #[cfg(not(unix))]
        let _ = (&channel, &reply);
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
        let _ = tokio::time::timeout(Duration::from_secs(10), async { while let Some(m) = ch.wait().await { if let ChannelMsg::ExitStatus { exit_status } = m { code = Some(exit_status); } else if matches!(m, ChannelMsg::Close) { break; } } }).await;
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

/// Remote command for a persistent tmux pane (status bar hidden so it looks like a plain shell).
pub fn tmux_command(name: &str) -> String {
    let n = sh_quote(name);
    format!("if command -v tmux >/dev/null 2>&1; then exec tmux new-session -A -s {n} \\; set-option status off; else echo '[shellax] tmux not found, plain shell'; exec \"$SHELL\" -l; fi")
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
        let handler = Handler { host: tg.host.clone(), port: tg.port, known_hosts: tg.known_hosts.clone(), prompt: prompts.hostkey.clone(), failure: failure.clone(), forwards: forwards.clone(), seen: Default::default() };
        let secs = CONNECT_TIMEOUT_SECS.load(Relaxed).max(1);
        let res: Result<(client::Handle<Handler>, Option<Arc<Conn>>)> = async {
            if let Some(j) = &tg.jump {
                let jc = connect(j, prompts).await.map_err(|e| anyhow!(tf("ssh.jump_failed", &[("jump", &j.label()), ("err", &format!("{e:#}"))])))?;
                let stream = tokio::time::timeout(Duration::from_secs(secs), jc.direct(&tg.host, tg.port)).await.map_err(|_| anyhow!(tf("ssh.connect_timeout", &[("secs", &secs)])))??;
                let h = tokio::time::timeout(Duration::from_secs(300), client::connect_stream(cfg, stream, handler)).await.map_err(|_| anyhow!(t("ssh.handshake_timeout")))??;
                Ok((h, Some(jc)))
            } else {
                // Opt-in proxy: HTTP CONNECT / SOCKS5 for this hop. Jump hosts ride the first hop's tunnel.
                let tcp = match crate::proxy::for_ssh(&tg.host, tg.use_proxy)? {
                    Some(p) => tokio::time::timeout(Duration::from_secs(secs), crate::proxy::connect(&p, &tg.host, tg.port)).await
                        .map_err(|_| anyhow!(tf("ssh.connect_timeout", &[("secs", &secs)])))??,
                    None => tokio::time::timeout(Duration::from_secs(secs), tokio::net::TcpStream::connect((tg.host.as_str(), tg.port))).await
                        .map_err(|_| anyhow!(tf("ssh.connect_timeout", &[("secs", &secs)])))??,
                };
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

type DynAgent = russh::keys::agent::client::AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin>>;

/// Unix: $SSH_AUTH_SOCK. Windows: $SSH_AUTH_SOCK or the Win32-OpenSSH pipe, then Pageant.
async fn connect_agent() -> Result<DynAgent, russh::keys::Error> {
    use russh::keys::agent::client::AgentClient;
    #[cfg(unix)]
    { Ok(AgentClient::connect_env().await?.dynamic()) }
    #[cfg(windows)]
    {
        let pipe = std::env::var_os("SSH_AUTH_SOCK").unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
        match AgentClient::connect_named_pipe(&pipe).await {
            Ok(a) => Ok(a.dynamic()),
            Err(e) => AgentClient::connect_pageant().await.map(|a| a.dynamic()).map_err(|_| e),
        }
    }
}

async fn authenticate(h: &mut client::Handle<Handler>, tg: &SshTarget, prompts: &Prompts) -> Result<()> {
    let (mode, no_agent) = match tg.auth.as_str() { "" => ("auto", false), "auto-noagent" => ("auto", true), m => (m, false) };
    let mut errors: Vec<String> = Vec::new();
    // ask the server which methods it offers ("none" probe), so we never prompt for a password
    // the server would not accept (e.g. a PAM / 2FA-only host)
    let offered: Option<Vec<russh::MethodKind>> = match h.authenticate_none(&tg.user).await? {
        russh::client::AuthResult::Success => return Ok(()),
        russh::client::AuthResult::Failure { remaining_methods, .. } => Some(remaining_methods.to_vec()),
    };
    let offers = |m: russh::MethodKind| offered.as_ref().is_none_or(|v| v.is_empty() || v.contains(&m));
    // methods in the configured order ("password,key,agent" …); "auto" = agent → keys → password → kbd
    let order: Vec<&str> = if mode == "auto" { vec!["agent", "key", "password"] } else { mode.split(',').map(str::trim).filter(|m| !m.is_empty()).collect() };
    let single = order.len() == 1;
    for step in order.iter().copied() {
    match step {
    "agent" if !no_agent => {
        match connect_agent().await {
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
            Err(e) => if single || mode != "auto" { errors.push(tf("ssh.agent_unavailable", &[("err", &e)])); },
        }
    }
    "cert" => {
        // OpenSSH certificate + its private key (first identity file)
        let (Some(cp), Some(kp)) = (tg.cert.as_ref(), tg.keys.first()) else { errors.push(t("ssh.cert_missing").into()); continue };
        let cert = match russh::keys::Certificate::read_file(cp) { Ok(c) => c, Err(e) => { errors.push(format!("{}: {e}", cp.display())); continue } };
        let pp = tg.passphrase_override.clone().or_else(|| SECRETS.get().and_then(|s| s.get(&passphrase_secret(kp))).map(|x| x.0));
        let key = match load_secret_key(kp, pp.as_deref()) { Ok(k) => k, Err(e) => { errors.push(format!("{}: {e}", kp.display())); continue } };
        if h.authenticate_openssh_cert(&tg.user, Arc::new(key), cert).await?.success() { return Ok(()); }
        errors.push(tf("ssh.key_rejected", &[("key", &cp.display())]));
    }
    "key" => {
        for kp in &tg.keys {
            if !kp.exists() { if mode != "auto" { errors.push(format!("{}: not found", kp.display())); } continue; }
            let key = match load_secret_key(kp, None) {
                Ok(k) => k,
                Err(russh::keys::Error::KeyIsEncrypted) => {
                    let saved = tg.passphrase_override.clone().or_else(|| SECRETS.get().and_then(|s| s.get(&passphrase_secret(kp))).map(|x| x.0));
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
    "password" if offers(russh::MethodKind::Password) => {
        let name = tg.password_secret();
        if let Some(pw) = tg.password_override.clone() {
            if h.authenticate_password(&tg.user, pw).await?.success() { return Ok(()); }
            errors.push(t("ssh.password_rejected").into());
            continue;
        }
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
    _ => {}
    }
    }
    // 4. keyboard-interactive (2FA / OTP / PAM): each server prompt goes to the UI, answers are never stored
    if (mode == "auto" || order.iter().any(|m| matches!(*m, "password" | "kbd"))) && tg.password_override.is_none() && offers(russh::MethodKind::KeyboardInteractive) {
        use russh::client::KeyboardInteractiveAuthResponse as R;
        let mut r = h.authenticate_keyboard_interactive_start(&tg.user, None::<String>).await?;
        let mut rounds = 0;
        loop {
            match r {
                R::Success => return Ok(()),
                R::Failure { .. } => { if rounds > 0 { errors.push(t("ssh.kbd_rejected").into()); } break }
                R::InfoRequest { name, instructions, prompts: ps } => {
                    rounds += 1;
                    if rounds > 6 { break }
                    let head = [name, instructions].into_iter().filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join("\n");
                    let mut answers = Vec::with_capacity(ps.len());
                    for p in ps {
                        let Some((a, _)) = ask(prompts, tg.label(), AuthKind::Interactive { instructions: head.clone(), prompt: p.prompt.trim().to_string(), echo: p.echo }).await else {
                            bail!(tf("ssh.auth_failed", &[("err", &t("ssh.kbd_cancelled"))]))
                        };
                        answers.push(a);
                    }
                    r = h.authenticate_keyboard_interactive_respond(answers).await?;
                }
            }
        }
    }
    if errors.is_empty() { errors.push(t("ssh.no_key").into()); }
    bail!(tf("ssh.auth_failed", &[("err", &errors.join("; "))]))
}

/// One step of "Test connection": 0 DNS, 1 TCP, 2 SSH handshake + host key, 3 authentication.
/// `ok: None` = running.
#[derive(Clone, Debug)]
pub struct TestStep { pub idx: u8, pub ok: Option<bool>, pub ms: u128, pub detail: String }

/// Diagnose a target step by step (no pooling, no jump/proxy shortcuts: the first hop is tested
/// directly). Each step is bounded by `secs`. Host-key trust and auth prompts go through `prompts`.
pub async fn test_connection(tg: &SshTarget, prompts: &Prompts, secs: u64, tx: flume::Sender<TestStep>) {
    use std::time::Instant;
    let send = |idx: u8, ok: Option<bool>, ms: u128, detail: String| { let _ = tx.send(TestStep { idx, ok, ms, detail }); };
    let lim = Duration::from_secs(secs.max(1));
    let to = |what: &str| tf("ssh.test_timeout", &[("what", &what), ("secs", &secs)]);
    // 1. DNS. A proxied hop lets the proxy resolve the name, so a local lookup failure is not fatal.
    send(0, None, 0, String::new());
    let t0 = Instant::now();
    let via = match crate::proxy::for_ssh(&tg.host, tg.use_proxy) {
        Err(e) => return send(0, Some(false), 0, format!("{e:#}")),
        Ok(v) => v,
    };
    let direct_addr = if via.is_some() {
        let via_detail = if tg.host.parse::<std::net::IpAddr>().is_ok() { t("ssh.proxy_ip") } else { t("ssh.proxy_dns") };
        send(0, Some(true), t0.elapsed().as_millis(), via_detail.into());
        None
    } else {
        let addrs = match tokio::time::timeout(lim, tokio::net::lookup_host((tg.host.as_str(), tg.port))).await {
            Err(_) => return send(0, Some(false), t0.elapsed().as_millis(), to("DNS")),
            Ok(Err(e)) => return send(0, Some(false), t0.elapsed().as_millis(), format!("{e}")),
            Ok(Ok(a)) => a.collect::<Vec<_>>(),
        };
        let Some(addr) = addrs.first().copied() else { return send(0, Some(false), t0.elapsed().as_millis(), t("ssh.test_no_address").into()) };
        send(0, Some(true), t0.elapsed().as_millis(), addr.ip().to_string());
        Some(addr)
    };
    // 2. TCP
    send(1, None, 0, String::new());
    let t1 = Instant::now();
    let tcp = match via {
        Some(p) => tokio::time::timeout(lim, crate::proxy::connect(&p, &tg.host, tg.port)).await.map(|r| r.map_err(|e| format!("{e:#}"))),
        None => tokio::time::timeout(lim, tokio::net::TcpStream::connect(direct_addr.unwrap())).await.map(|r| r.map_err(|e| match e.kind() {
            std::io::ErrorKind::ConnectionRefused => t("ssh.test_refused").to_string(),
            _ => e.to_string(),
        })),
    };
    let tcp = match tcp {
        Err(_) => return send(1, Some(false), t1.elapsed().as_millis(), to("TCP")),
        Ok(Err(e)) => return send(1, Some(false), t1.elapsed().as_millis(), e),
        Ok(Ok(s)) => s,
    };
    let _ = tcp.set_nodelay(true);
    send(1, Some(true), t1.elapsed().as_millis(), direct_addr.map(|a| a.to_string()).unwrap_or_else(|| format!("{}:{}", tg.host, tg.port)));
    // 3. SSH handshake + host key (an unknown key is offered for trust through the normal prompt)
    send(2, None, 0, String::new());
    let t2 = Instant::now();
    let failure = Arc::new(std::sync::Mutex::new(None));
    let seen: Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let cfg = Arc::new(client::Config { inactivity_timeout: None, preferred: preferred(tg), ..Default::default() });
    let handler = Handler { host: tg.host.clone(), port: tg.port, known_hosts: tg.known_hosts.clone(), prompt: prompts.hostkey.clone(), failure: failure.clone(), forwards: Default::default(), seen: seen.clone() };
    // the host-key prompt may wait for the user: allow it beyond the step limit
    let mut h = match tokio::time::timeout(Duration::from_secs(120), client::connect_stream(cfg, tcp, handler)).await {
        Err(_) => return send(2, Some(false), t2.elapsed().as_millis(), to("SSH")),
        Ok(Err(e)) => return send(2, Some(false), t2.elapsed().as_millis(), failure.lock().unwrap().clone().unwrap_or_else(|| format!("{e}"))),
        Ok(Ok(h)) => h,
    };
    send(2, Some(true), t2.elapsed().as_millis(), seen.lock().unwrap().clone().unwrap_or_default());
    // 4. authentication
    send(3, None, 0, String::new());
    let t3 = Instant::now();
    match tokio::time::timeout(Duration::from_secs(120), authenticate(&mut h, tg, prompts)).await {
        Err(_) => send(3, Some(false), t3.elapsed().as_millis(), to("auth")),
        Ok(Err(e)) => send(3, Some(false), t3.elapsed().as_millis(), format!("{e:#}")),
        Ok(Ok(())) => send(3, Some(true), t3.elapsed().as_millis(), tg.user.clone()),
    }
    let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
}

/// Connection reuse (PRD: multiplexing): one SSH connection per user@host:port (+jump chain).
static POOL: std::sync::LazyLock<tokio::sync::Mutex<HashMap<String, std::sync::Weak<Conn>>>> = std::sync::LazyLock::new(Default::default);
pub static REUSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// Session-only ls/grep colors. Does not rewrite remote shell rc files.
pub static STYLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Remote command that starts an interactive shell with color aliases for this session only.
pub fn style_shell_command() -> String {
    r#"if [ -n "$ZSH_VERSION" ] || [ "$(basename "${SHELL:-}")" = zsh ]; then
  d=$(mktemp -d /tmp/shellax-style.XXXXXX 2>/dev/null) || d=$(mktemp -d) || exec "${SHELL:-/bin/sh}" -l
  printf '%s\n' '[ -f "$HOME/.zshenv" ] && . "$HOME/.zshenv"' > "$d/.zshenv"
  cat > "$d/.zshrc" << 'END'
[ -f "$HOME/.zshrc" ] && . "$HOME/.zshrc"
if command -v dircolors >/dev/null 2>&1; then eval "$(dircolors -b 2>/dev/null)"; fi
alias ls='ls --color=auto'
alias ll='ls -alF --color=auto'
alias grep='grep --color=auto'
alias egrep='egrep --color=auto'
alias fgrep='fgrep --color=auto'
END
  ZDOTDIR="$d" exec zsh -i
fi
f=$(mktemp /tmp/shellax-style.XXXXXX 2>/dev/null) || f=$(mktemp) || exec "${SHELL:-/bin/bash}" -l
cat > "$f" << 'END'
[ -f /etc/profile ] && . /etc/profile
[ -f ~/.bashrc ] && . ~/.bashrc
if command -v dircolors >/dev/null 2>&1; then eval "$(dircolors -b 2>/dev/null)"; fi
alias ls='ls --color=auto'
alias ll='ls -alF --color=auto'
alias grep='grep --color=auto'
alias egrep='egrep --color=auto'
alias fgrep='fgrep --color=auto'
END
exec bash --rcfile "$f" -i
"#.to_string()
}

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
                loop { tokio::select! { _ = &mut sleep => break, m = from_ui.recv_async() => if m.is_err() || matches!(m, Ok(BackendMsg::Close)) { gone = true; break } } }
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
                    let ok = conn.write_file(".local/share/shellax/policy-endpoint.json", body.as_bytes()).await.is_ok();
                    let _ = out_tx.send(format!("\x1b[2m[shellax] {}\x1b[0m\r\n", tf(if ok { "ssh.remote_hooks_on" } else { "ssh.remote_hooks_write_failed" }, &[("port", &rport)])).into_bytes());
                }
                Err(e) => { let _ = out_tx.send(format!("\x1b[33m[shellax] {}\x1b[0m\r\n", tf("ssh.remote_hooks_failed", &[("err", &e)])).into_bytes()); }
            }
        }
    }
    let ch = conn.handle.channel_open_session().await?;
    if target.agent_forward { let _ = ch.agent_forward(false).await; }
    if target.remote_hooks { let _ = ch.set_env(false, "SHELLAX_HOST", target.host.clone()).await; }
    let sz = s.size();
    ch.request_pty(false, "xterm-256color", sz.cols as u32, sz.rows as u32, 0, 0, &[]).await?;
    match &target.tmux {
        // reattach (or create) the persistent session; fall back to a login shell without tmux
        Some(name) => ch.exec(false, tmux_command(name)).await?,
        None if STYLE.load(Ordering::Relaxed) => ch.exec(false, style_shell_command()).await?,
        None => ch.request_shell(false).await?,
    }
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
                Ok(BackendMsg::Close) | Err(_) => return Ok(ShellEnd::UiGone),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn style_command_colors_ls_without_editing_bashrc() {
        let c = style_shell_command();
        assert!(c.contains("alias ls='ls --color=auto'"));
        assert!(c.contains("alias ll='ls -alF --color=auto'"));
        assert!(!c.contains(">>"), "must not append to the remote shell rc");
        assert!(c.contains(". ~/.bashrc"));
    }
}
