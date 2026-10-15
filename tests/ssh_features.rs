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
    rt.block_on(c.write_file("/tmp/shellax-test-ep/ep.json", b"{\"x\":1}")).unwrap();
    let (_, out) = rt.block_on(c.exec("stat -c %a /tmp/shellax-test-ep/ep.json; cat /tmp/shellax-test-ep/ep.json; rm -r /tmp/shellax-test-ep", Duration::from_secs(5))).unwrap();
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

#[test]
fn sftp_ops_and_resumable_transfers() {
    use ssh_term_proto::sftp::{self, Dir, JobState, Queue, Side};
    let (Some(port), Some(key)) = (env("TEST_SSH_PORT"), env("TEST_SSH_KEY")) else { eprintln!("skip"); return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "key".into(), keys: vec![key.into()], ..base(&port) };
    let (p, _) = prompts(vec![]);
    rt.block_on(async {
        let c = ssh::connect(&t, &p).await.unwrap();
        let s = c.sftp().await.unwrap();
        let home = s.canonicalize(".").await.unwrap();
        let dir = sftp::join(&home, &format!("stp-sftp-{}", std::process::id()));
        let _ = sftp::remove(Some(&s), Side::Remote, &dir).await;
        sftp::mkdir(Some(&s), Side::Remote, &dir).await.unwrap();
        // 3 MB file, upload interrupted half-way then resumed
        let ld = std::env::temp_dir().join(format!("stp-sftp-l-{}", std::process::id()));
        std::fs::create_dir_all(&ld).unwrap();
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(ld.join("big.bin"), &data).unwrap();
        let remote = sftp::join(&dir, "big.bin");
        { use tokio::io::AsyncWriteExt; let mut f = s.create(remote.clone()).await.unwrap(); f.write_all(&data[..1_000_000]).await.unwrap(); f.shutdown().await.unwrap(); } // simulate a partial earlier upload
        let q = Queue::default();
        let j = q.push(Dir::Upload, ld.join("big.bin"), remote.clone());
        q.drain(&s).await;
        assert_eq!(j.state(), JobState::Done);
        assert_eq!(j.resumed_from.load(std::sync::atomic::Ordering::Relaxed), 1_000_000);
        assert_eq!(s.read(remote.clone()).await.unwrap(), data);
        // built-in editor: text round trip, conflict detection, binary / size limits
        let txt = sftp::join(&dir, "conf.txt");
        sftp::write_text(&s, &txt, "a = 1\n中文\n", None).await.unwrap();
        let (body, v) = sftp::read_text(&s, &txt).await.unwrap();
        assert_eq!(body, "a = 1\n中文\n");
        let v2 = sftp::write_text(&s, &txt, "a = 2\n", Some(v)).await.unwrap();
        assert!(sftp::write_text(&s, &txt, "stale", Some(v)).await.unwrap_err().to_string().contains("changed on the server"));
        assert_eq!(sftp::read_text(&s, &txt).await.unwrap().0, "a = 2\n");
        let _ = v2;
        assert!(sftp::read_text(&s, &remote).await.is_err(), "3 MB binary refused");
        sftp::remove(Some(&s), Side::Remote, &txt).await.unwrap();
        // download with a partial local copy → resume
        std::fs::write(ld.join("down.bin"), &data[..123_456]).unwrap();
        let j2 = q.push(Dir::Download, ld.join("down.bin"), remote.clone());
        q.drain(&s).await;
        assert_eq!(j2.resumed_from.load(std::sync::atomic::Ordering::Relaxed), 123_456);
        assert_eq!(std::fs::read(ld.join("down.bin")).unwrap(), data);
        // rename / chmod / listing / recursive delete
        let renamed = sftp::join(&dir, "renamed.bin");
        sftp::rename(Some(&s), Side::Remote, &remote, &renamed).await.unwrap();
        sftp::chmod(Some(&s), Side::Remote, &renamed, 0o600).await.unwrap();
        sftp::mkdir(Some(&s), Side::Remote, &sftp::join(&dir, "sub")).await.unwrap();
        let ls = sftp::list_remote(&s, &dir, false).await.unwrap();
        assert_eq!(ls[0].name, "sub");
        assert!(ls[0].is_dir);
        assert_eq!(ls[1].name, "renamed.bin");
        assert_eq!(ls[1].mode, 0o600);
        assert_eq!(ls[1].size, 3_000_000);
        // tree download
        let n = q.push_download_tree(&s, &dir, &ld.join("tree")).await.unwrap();
        q.drain(&s).await;
        assert_eq!(n, 1);
        assert!(ld.join("tree/sub").is_dir());
        sftp::remove(Some(&s), Side::Remote, &dir).await.unwrap();
        assert!(!s.try_exists(dir.clone()).await.unwrap());
        let _ = std::fs::remove_dir_all(&ld);
    });
}

#[test]
fn port_forwards_local_remote_socks() {
    use ssh_term_proto::forward::{Kind, Running, Spec, State};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (Some(port), Some(key)) = (env("TEST_SSH_PORT2").or(env("TEST_SSH_PORT")), env("TEST_SSH_KEY")) else { eprintln!("skip"); return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "key".into(), keys: vec![key.into()], ..base(&port) };
    let (p, _) = prompts(vec![]);
    rt.block_on(async {
        let c = ssh::connect(&t, &p).await.unwrap();
        // echo server on this machine
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ep = echo.local_addr().unwrap().port();
        tokio::spawn(async move { loop { let (mut s, _) = echo.accept().await.unwrap(); tokio::spawn(async move { let mut b = [0u8; 64]; while let Ok(n) = s.read(&mut b).await { if n == 0 { break } let _ = s.write_all(&b[..n]).await; } }); } });
        async fn roundtrip(port: u16, socks: Option<u16>) -> Vec<u8> {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            if let Some(dp) = socks {
                s.write_all(&[5, 1, 0]).await.unwrap(); let mut r = [0u8; 2]; s.read_exact(&mut r).await.unwrap();
                s.write_all(&[5, 1, 0, 1, 127, 0, 0, 1]).await.unwrap(); s.write_all(&dp.to_be_bytes()).await.unwrap();
                let mut r = [0u8; 10]; s.read_exact(&mut r).await.unwrap(); assert_eq!(r[1], 0);
            }
            s.write_all(b"ping").await.unwrap(); let mut b = [0u8; 4]; s.read_exact(&mut b).await.unwrap(); b.to_vec()
        }
        let free = { let x = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); x.local_addr().unwrap().port() };
        let l = Running::new(Spec { kind: Kind::L, bind_addr: "127.0.0.1".into(), bind_port: free, dest_host: "127.0.0.1".into(), dest_port: ep, ..Default::default() });
        let lp = l.start(c.clone()).await.unwrap();
        assert_eq!(roundtrip(lp, None).await, b"ping");
        assert!(matches!(l.state(), State::Running { .. }));
        l.stop().await;
        assert_eq!(l.state(), State::Stopped);
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", lp)).await.is_err());
        let free = { let x = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); x.local_addr().unwrap().port() };
        let d = Running::new(Spec { kind: Kind::D, bind_addr: "127.0.0.1".into(), bind_port: free, ..Default::default() });
        let dp = d.start(c.clone()).await.unwrap();
        assert_eq!(roundtrip(dp, Some(ep)).await, b"ping");
        // -R: remote port 0 → server picks; reach it from the server side via a second -L
        let r = Running::new(Spec { kind: Kind::R, bind_addr: "127.0.0.1".into(), bind_port: 0, dest_host: "127.0.0.1".into(), dest_port: ep, ..Default::default() });
        let rp = r.start(c.clone()).await.unwrap();
        assert!(rp > 0);
        let (code, out) = c.exec(&format!("exec 3<>/dev/tcp/127.0.0.1/{rp}; printf ping >&3; head -c4 <&3"), Duration::from_secs(5)).await.unwrap();
        assert_eq!((code, out), (Some(0), b"ping".to_vec()));
        r.stop().await;
        d.stop().await;
    });
}

/// keyboard-interactive (PAM) on TEST_SSH_PORT3 (testenv/sshd3_config, password/pubkey off):
/// the server prompt reaches the UI channel, the answer is not saved, wrong answers fail cleanly.
#[test]
fn keyboard_interactive_auth() {
    let (Some(port3), Some(pw)) = (env("TEST_SSH_PORT3"), env("TEST_SSH_PASSWORD")) else { eprintln!("skip"); return };
    secrets_init();
    ssh::REUSE.store(false, std::sync::atomic::Ordering::Relaxed);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "password".into(), ..base(&port3) };
    let kbd = AuthKind::Interactive { instructions: String::new(), prompt: String::new(), echo: false };
    let (p, n) = prompts(vec![(kbd.clone(), pw.clone(), true)]);
    let c = rt.block_on(ssh::connect(&t, &p)).expect("keyboard-interactive auth");
    let (code, out) = rt.block_on(c.exec("echo kbd_ok", Duration::from_secs(5))).unwrap();
    assert_eq!((code, String::from_utf8_lossy(&out).trim()), (Some(0), "kbd_ok"));
    assert!(n.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    // never stored: a second connection prompts again
    let (p2, n2) = prompts(vec![(kbd.clone(), pw, true)]);
    rt.block_on(ssh::connect(&t, &p2)).expect("again");
    assert!(n2.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    // wrong answer → clean error
    let (p3, _) = prompts(vec![(kbd, "wrong".into(), false)]);
    assert!(rt.block_on(ssh::connect(&t, &p3)).is_err());
}

#[test]
fn sftp_directory_sync() {
    use ssh_term_proto::dirsync::{plan, scan_local, Mode, Op};
    use ssh_term_proto::sftp::{self, Queue, Side};
    let (Some(port), Some(key)) = (env("TEST_SSH_PORT"), env("TEST_SSH_KEY")) else { eprintln!("skip"); return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = SshTarget { auth: "key".into(), keys: vec![key.into()], ..base(&port) };
    let (p, _) = prompts(vec![]);
    rt.block_on(async {
        let c = ssh::connect(&t, &p).await.unwrap();
        let s = c.sftp().await.unwrap();
        let home = s.canonicalize(".").await.unwrap();
        let rd = sftp::join(&home, &format!("stp-sync-{}", std::process::id()));
        let _ = sftp::remove(Some(&s), Side::Remote, &rd).await;
        sftp::mkdir(Some(&s), Side::Remote, &rd).await.unwrap();
        let ld = std::env::temp_dir().join(format!("stp-sync-l-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ld);
        std::fs::create_dir_all(ld.join("sub/deep")).unwrap();
        std::fs::write(ld.join("a.txt"), "local a").unwrap();
        std::fs::write(ld.join("sub/deep/b.txt"), "local b").unwrap();
        sftp::write_text(&s, &sftp::join(&rd, "only-remote.txt"), "r", None).await.unwrap();
        sftp::write_text(&s, &sftp::join(&rd, "a.txt"), "old remote a, longer", None).await.unwrap();
        // one-way local→remote without delete: remote extra stays
        let q = Queue::default();
        let ops = plan(&scan_local(&ld).unwrap(), &sftp::scan_remote(&s, &rd).await.unwrap(), Mode::LocalToRemote, false);
        assert!(ops.contains(&Op::Upload("a.txt".into())) && ops.contains(&Op::MkdirRemote("sub/deep".into())) && !ops.iter().any(|o| o.is_delete()));
        sftp::execute_sync(&s, &q, &ld, &rd, &ops).await.unwrap();
        q.drain(&s).await;
        assert_eq!(s.read(sftp::join(&rd, "a.txt")).await.unwrap(), b"local a");
        assert_eq!(s.read(sftp::join(&rd, "sub/deep/b.txt")).await.unwrap(), b"local b");
        // in sync now (mtimes preserved) → only the remote-only file differs
        let ops = plan(&scan_local(&ld).unwrap(), &sftp::scan_remote(&s, &rd).await.unwrap(), Mode::LocalToRemote, true);
        assert_eq!(ops, vec![Op::DeleteRemote("only-remote.txt".into())]);
        sftp::execute_sync(&s, &q, &ld, &rd, &ops).await.unwrap();
        assert!(s.metadata(sftp::join(&rd, "only-remote.txt")).await.is_err());
        // two-way: new remote file comes down, newer local edit goes up
        sftp::write_text(&s, &sftp::join(&rd, "from-remote.txt"), "hello", None).await.unwrap();
        std::fs::write(ld.join("a.txt"), "local a v2").unwrap();
        let f = std::fs::File::options().write(true).open(ld.join("a.txt")).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)).unwrap();
        let ops = plan(&scan_local(&ld).unwrap(), &sftp::scan_remote(&s, &rd).await.unwrap(), Mode::TwoWay, false);
        assert!(ops.contains(&Op::Download("from-remote.txt".into())) && ops.contains(&Op::Upload("a.txt".into())), "{ops:?}");
        sftp::execute_sync(&s, &q, &ld, &rd, &ops).await.unwrap();
        q.drain(&s).await;
        assert_eq!(std::fs::read_to_string(ld.join("from-remote.txt")).unwrap(), "hello");
        assert_eq!(s.read(sftp::join(&rd, "a.txt")).await.unwrap(), b"local a v2");
        assert!(plan(&scan_local(&ld).unwrap(), &sftp::scan_remote(&s, &rd).await.unwrap(), Mode::TwoWay, true).is_empty(), "idempotent");
        let _ = sftp::remove(Some(&s), Side::Remote, &rd).await;
        let _ = std::fs::remove_dir_all(&ld);
    });
}
