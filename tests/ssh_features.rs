//! SSH feature integration tests against the local test sshds (see testenv/):
//!   TEST_SSH_PORT=2222 (key only)  TEST_SSH_PORT2=2223 (password + forwarding)
//!   TEST_SSH_USER, TEST_SSH_KEY, TEST_SSH_PASSWORD, TEST_SSH_PP_KEY + TEST_SSH_PP (passphrase key)
//! Skipped when the variables are not set.
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ssh_term_proto::secrets::SecretStore;
use ssh_term_proto::ssh::{self, AuthKind, Prompts, SshTarget};

fn env(k: &str) -> Option<String> { std::env::var(k).ok().filter(|v| !v.is_empty()) }

fn kh() -> PathBuf {
    let p = std::env::temp_dir().join(format!("stp-feat-kh-{}", std::process::id()));
    p
}

fn base(port: &str) -> SshTarget {
    SshTarget { host: "127.0.0.1".into(), port: port.parse().unwrap(), user: env("TEST_SSH_USER").unwrap(), known_hosts: kh(), ..Default::default() }
}

/// Accept host keys; answer auth prompts from `answers` (counting them).
fn prompts(answers: Vec<(AuthKind, String, bool)>) -> (Prompts, Arc<std::sync::atomic::AtomicUsize>) {
    let (htx, hrx) = flume::unbounded::<ssh::HostKeyPrompt>();
    let (atx, arx) = flume::unbounded::<ssh::AuthPrompt>();
    std::thread::spawn(move || while let Ok(p) = hrx.recv() { let _ = p.reply.send(ssh::HostKeyDecision::AcceptAndSave); });
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || while let Ok(p) = arx.recv() {
        n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let a = answers.iter().find(|(k, _, _)| std::mem::discriminant(k) == std::mem::discriminant(&p.kind));
        let _ = p.reply.send(a.map(|(_, v, save)| (v.clone(), *save)));
    });
    (Prompts { hostkey: htx, auth: atx }, n)
}

fn secrets_init() {
    let dir = std::env::temp_dir().join(format!("stp-feat-sec-{}", std::process::id()));
    let _ = ssh::SECRETS.set(SecretStore::file_only(dir));
}

#[test]
fn password_auth_prompt_save_and_reuse() {
    let (Some(port2), Some(pw)) = (env("TEST_SSH_PORT2"), env("TEST_SSH_PASSWORD")) else { eprintln!("skip"); return };
    secrets_init();
    ssh::REUSE.store(false, std::sync::atomic::Ordering::Relaxed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "password".into(), ..base(&port2) };
    // wrong first, then right → 2 prompts, saved
    let (p, n) = prompts(vec![(AuthKind::Password, pw.clone(), true)]);
    let c = rt.block_on(ssh::connect(&t, &p)).expect("password auth");
    let (code, out) = rt.block_on(c.exec("echo pw_ok", Duration::from_secs(5))).unwrap();
    assert_eq!((code, String::from_utf8_lossy(&out).trim()), (Some(0), "pw_ok"));
    assert_eq!(n.load(std::sync::atomic::Ordering::SeqCst), 1);
    // saved in the secret store → no prompt next time
    let (p2, n2) = prompts(vec![]);
    rt.block_on(ssh::connect(&t, &p2)).expect("saved password");
    assert_eq!(n2.load(std::sync::atomic::Ordering::SeqCst), 0);
    // wrong password, user cancels → clean auth error
    ssh::SECRETS.get().unwrap().delete(&t.password_secret());
    let (p3, _) = prompts(vec![(AuthKind::Password, "wrong".into(), false)]);
    let e = rt.block_on(ssh::connect(&t, &p3)).err().expect("must fail");
    assert!(format!("{e:#}").contains("password") || format!("{e:#}").contains("密码"), "{e:#}");
}

#[test]
fn passphrase_key() {
    let (Some(port), Some(k), Some(pp)) = (env("TEST_SSH_PORT"), env("TEST_SSH_PP_KEY"), env("TEST_SSH_PP")) else { eprintln!("skip"); return };
    secrets_init();
    ssh::REUSE.store(false, std::sync::atomic::Ordering::Relaxed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "key".into(), keys: vec![k.into()], ..base(&port) };
    let (p, n) = prompts(vec![(AuthKind::Passphrase(PathBuf::new()), pp, false)]);
    let c = rt.block_on(ssh::connect(&t, &p)).expect("passphrase key");
    assert_eq!(String::from_utf8_lossy(&rt.block_on(c.exec("echo pp_ok", Duration::from_secs(5))).unwrap().1).trim(), "pp_ok");
    assert_eq!(n.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (p2, _) = prompts(vec![]); // cancelled → fails
    assert!(rt.block_on(ssh::connect(&t, &p2)).is_err());
}

#[test]
fn agent_auth() {
    let (Some(port), Some(_sock)) = (env("TEST_SSH_PORT"), env("SSH_AUTH_SOCK")) else { eprintln!("skip (no SSH_AUTH_SOCK)"); return };
    ssh::REUSE.store(false, std::sync::atomic::Ordering::Relaxed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "agent".into(), ..base(&port) };
    let (p, _) = prompts(vec![]);
    let c = rt.block_on(ssh::connect(&t, &p)).expect("agent auth");
    assert_eq!(String::from_utf8_lossy(&rt.block_on(c.exec("echo agent_ok", Duration::from_secs(5))).unwrap().1).trim(), "agent_ok");
}

#[test]
fn proxyjump_and_remote_forward() {
    let (Some(port), Some(port2), Some(key)) = (env("TEST_SSH_PORT"), env("TEST_SSH_PORT2"), env("TEST_SSH_KEY")) else { eprintln!("skip"); return };
    ssh::REUSE.store(false, std::sync::atomic::Ordering::Relaxed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let jump = SshTarget { auth: "key".into(), keys: vec![key.clone().into()], ..base(&port) };
    let t = SshTarget { auth: "key".into(), keys: vec![key.into()], jump: Some(Box::new(jump)), ..base(&port2) };
    let (p, _) = prompts(vec![]);
    let c = rt.block_on(ssh::connect(&t, &p)).expect("via jump");
    let (_, out) = rt.block_on(c.exec("echo jumped; echo $SSH_CONNECTION", Duration::from_secs(5))).unwrap();
    let out = String::from_utf8_lossy(&out).to_string();
    assert!(out.starts_with("jumped"), "{out}");
    assert!(out.contains(" 2223"), "inner connection lands on port 2223: {out}");
    // -R: remote port → local TCP server
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let lport = l.local_addr().unwrap().port();
    std::thread::spawn(move || for s in l.incoming().flatten() { let mut s = s; let mut b = [0u8; 2]; let _ = s.read_exact(&mut b); let _ = s.write_all(format!("pong:{}", String::from_utf8_lossy(&b)).as_bytes()); });
    let rport = rt.block_on(c.remote_forward("127.0.0.1", 0, "127.0.0.1", lport)).expect("tcpip_forward");
    assert!(rport > 0);
    let cmd = format!("exec 3<>/dev/tcp/127.0.0.1/{rport}; printf hi >&3; head -c 7 <&3");
    let (_, out) = rt.block_on(c.exec(&cmd, Duration::from_secs(5))).unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "pong:hi");
    // write_file (used for the remote policy endpoint) is private
    rt.block_on(c.write_file("/tmp/agentshell-test-ep/ep.json", b"{\"x\":1}")).unwrap();
    let (_, out) = rt.block_on(c.exec("stat -c %a /tmp/agentshell-test-ep/ep.json; cat /tmp/agentshell-test-ep/ep.json; rm -r /tmp/agentshell-test-ep", Duration::from_secs(5))).unwrap();
    assert_eq!(String::from_utf8_lossy(&out), "600\n{\"x\":1}");
}

#[test]
fn connection_reuse() {
    let (Some(port), Some(key)) = (env("TEST_SSH_PORT"), env("TEST_SSH_KEY")) else { eprintln!("skip"); return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "key".into(), keys: vec![key.into()], ..base(&port) };
    let (p, _) = prompts(vec![]);
    // REUSE is a process-wide switch; this test only asserts when it is on
    ssh::REUSE.store(true, std::sync::atomic::Ordering::Relaxed);
    let (a, ra) = rt.block_on(ssh::connect_shared(&t, &p)).unwrap();
    let (b, rb) = rt.block_on(ssh::connect_shared(&t, &p)).unwrap();
    if ssh::REUSE.load(std::sync::atomic::Ordering::Relaxed) { assert!(!ra && rb && Arc::ptr_eq(&a, &b)); }
}
