//! Headless checks: type `ls` into each backend and look for its output in the alacritty grid.
use std::time::{Duration, Instant};
use ssh_term_proto::{local, ssh, term::{Session, Size}};

fn wait_for(s: &Session, needle: &str, secs: u64) -> String {
    let t0 = Instant::now();
    loop {
        let txt = s.screen_text();
        if txt.contains(needle) || t0.elapsed() > Duration::from_secs(secs) { return txt; }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn local_pty_ls() {
    let dir = std::env::temp_dir().join(format!("stp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker_local_file.txt"), b"x").unwrap();
    let s = local::spawn_local(Size { cols: 100, rows: 30 }, Some("/bin/bash")).unwrap();
    s.resize(Size { cols: 120, rows: 30 });
    s.write(format!("cd {} && ls\r", dir.display()));
    let txt = wait_for(&s, "marker_local_file.txt", 10);
    println!("{txt}");
    assert!(txt.lines().any(|l| l.trim() == "marker_local_file.txt"), "ls output not in grid");
    assert_eq!(s.size(), Size { cols: 120, rows: 30 });
}

fn test_target(known_hosts: std::path::PathBuf) -> Option<ssh::SshTarget> {
    let port = std::env::var("TEST_SSH_PORT").ok()?;
    Some(ssh::SshTarget {
        host: "127.0.0.1".into(),
        port: port.parse().unwrap(),
        user: std::env::var("TEST_SSH_USER").unwrap(),
        keys: vec![std::env::var("TEST_SSH_KEY").unwrap().into()],
        known_hosts,
        ..Default::default()
    })
}

#[test]
fn ssh_localhost_ls_and_known_hosts_tofu() {
    let kh = std::env::temp_dir().join(format!("stp-kh-{}", std::process::id()));
    let _ = std::fs::remove_file(&kh);
    let Some(target) = test_target(kh.clone()) else { eprintln!("TEST_SSH_PORT unset, skipping"); return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (tx, rx) = flume::unbounded::<ssh::HostKeyPrompt>();
    let (atx, _arx) = flume::unbounded::<ssh::AuthPrompt>();
    let prompts = ssh::Prompts { hostkey: tx.clone(), auth: atx };
    let nprompt = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let p2 = nprompt.clone();
    std::thread::spawn(move || while let Ok(p) = rx.recv() {
        assert!(p.fingerprint.starts_with("SHA256:"));
        p2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = p.reply.send(ssh::HostKeyDecision::AcceptAndSave);
    });
    // 1st connection: unknown host -> prompt -> saved
    let s = ssh::spawn_ssh(rt.handle(), target.clone(), Size { cols: 100, rows: 30 }, prompts.clone());
    let txt = wait_for(&s, "$", 10);
    s.resize(Size { cols: 90, rows: 25 });
    s.write("stty size; ls\r");
    let txt2 = wait_for(&s, "hello_from_sshd.txt", 10);
    println!("{txt}\n{txt2}");
    assert!(txt2.contains("hello_from_sshd.txt"), "remote ls output not in grid");
    assert!(txt2.contains("25 90"), "window_change not applied on remote pty");
    assert_eq!(nprompt.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(std::fs::read_to_string(&kh).unwrap().contains("ssh-ed25519"));
    // 2nd connection: known -> no prompt
    let s2 = ssh::spawn_ssh(rt.handle(), target.clone(), Size { cols: 80, rows: 24 }, prompts.clone());
    s2.write("echo second_ok\r");
    assert!(wait_for(&s2, "second_ok", 10).contains("second_ok"));
    assert_eq!(nprompt.load(std::sync::atomic::Ordering::SeqCst), 1);
    // changed key: record a different ed25519 key for this host -> refused, no prompt
    let other = std::fs::read_to_string(std::env::var("TEST_SSH_KEY").unwrap() + ".pub").unwrap();
    let mut parts = other.split_whitespace();
    let line = format!("[127.0.0.1]:{} {} {}\n", target.port, parts.next().unwrap(), parts.next().unwrap());
    std::fs::write(&kh, line).unwrap();
    ssh::REUSE.store(false, std::sync::atomic::Ordering::Relaxed); // force a fresh handshake
    let s3 = ssh::spawn_ssh(rt.handle(), target, Size { cols: 80, rows: 24 }, prompts);
    let t0 = Instant::now();
    while s3.conn_error.lock().unwrap().is_none() && t0.elapsed() < Duration::from_secs(10) { std::thread::sleep(Duration::from_millis(100)); }
    let err = s3.conn_error.lock().unwrap().clone().expect("changed key must be refused");
    assert!(err.contains("主机密钥已变更") || err.contains("HOST KEY CHANGED"), "{err}");
    assert_eq!(nprompt.load(std::sync::atomic::Ordering::SeqCst), 1);
    let _ = std::fs::remove_file(&kh);
}

#[test]
fn scrollback_keeps_history() {
    let s = local::spawn_local(Size { cols: 80, rows: 10 }, Some("/bin/bash")).unwrap();
    s.write("seq 1 300\r");
    wait_for(&s, "300", 10);
    let mut t = s.term.lock().unwrap();
    use alacritty_terminal::grid::{Dimensions, Scroll};
    assert!(t.grid().history_size() >= 290, "history {}", t.grid().history_size());
    t.scroll_display(Scroll::Delta(5));
    assert_eq!(t.grid().display_offset(), 5);
}

#[test]
fn local_zsh_integration_and_history() {
    if !std::path::Path::new("/usr/bin/zsh").exists() { eprintln!("no zsh, skipping"); return; }
    std::env::set_var("SHELLAX_INTEGRATE", "1");
    let s = local::spawn_local(Size { cols: 100, rows: 30 }, Some("/usr/bin/zsh")).unwrap();
    let t0 = Instant::now();
    while s.prompt_mark.lock().unwrap().is_none() && t0.elapsed() < Duration::from_secs(10) {
        if t0.elapsed() > Duration::from_secs(2) { s.write("q"); } // dismiss zsh-newuser-install if it shows up
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(s.prompt_mark.lock().unwrap().is_some(), "zsh integration should emit OSC 133;B\n{}", s.screen_text());
}

#[cfg(unix)]
#[test]
fn local_agent_identified_by_foreground_process() {
    let dir = std::env::temp_dir().join(format!("stp-agent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("grok");
    std::fs::write(&fake, "#!/bin/bash\nprintf '\\033]0;\\342\\240\\231 Working - grok\\007'\nsleep 5\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let s = local::spawn_local(Size { cols: 100, rows: 30 }, Some("/bin/bash")).unwrap();
    s.write(format!("{}\r", fake.display()));
    let t0 = Instant::now();
    while s.foreground_process().as_deref() != Some("grok") && t0.elapsed() < Duration::from_secs(5) { std::thread::sleep(Duration::from_millis(100)); }
    assert_eq!(s.foreground_process().as_deref(), Some("grok"));
    std::thread::sleep(Duration::from_millis(300));
    let m = ssh_term_proto::agent::bundled();
    let i = ssh_term_proto::agent::identify(&m, s.foreground_process().as_deref(), None, &s.title()).unwrap();
    let d = m[i].detect(ssh_term_proto::agent::DetectionInput { screen: &s.screen_text(), osc_title: &s.title(), osc_progress: "" });
    assert_eq!(d.state, ssh_term_proto::agent::AgentState::Working, "title={:?}", s.title());
}

#[test]
fn local_bash_osc133_prompt_mark() {
    std::env::set_var("SHELL", "/bin/bash");
    let s = local::spawn_local(Size { cols: 100, rows: 30 }, None).unwrap();
    let t0 = Instant::now();
    while s.prompt_mark.lock().unwrap().is_none() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let mark = s.prompt_mark.lock().unwrap().expect("bash integration should emit OSC 133;B");
    let cur = s.term.lock().unwrap().grid().cursor.point;
    assert_eq!((mark.0, mark.1), (cur.line.0, cur.column.0), "mark = input start = cursor at idle prompt");
}

#[test]
fn local_pty_graphics_record_notify() {
    let s = local::spawn_local(Size { cols: 80, rows: 24 }, Some("/bin/bash")).unwrap();
    let cast = std::env::temp_dir().join(format!("stp-rec-{}.cast", std::process::id()));
    s.start_recording(&cast, "t", true).unwrap();
    *s.notifier.lock().unwrap() = Some(ssh_term_proto::notify_rules::Notifier::new(vec![("DEPLOY_DONE".into(), "部署".into())]));
    // 4x12 sixel image, then a kitty 1x1 PNG-less RGB, then text after them.
    s.write("printf '\\033Pq\"1;1;4;12#1;2;0;0;100#1!4~-!4~\\033\\\\'; printf '\\033_Ga=T,f=24,s=1,v=1;AP8A\\033\\\\'; echo AFTER_IMG; echo TOKEN=sk-abcdefghijklmnopqrstuvwxyz; echo DEPLOY_DONE\r");
    let txt = wait_for(&s, "DEPLOY_DONE\n", 10);
    std::thread::sleep(Duration::from_millis(300));
    let imgs = s.images.lock().unwrap().clone();
    assert_eq!(imgs.iter().map(|i| i.image.proto).collect::<Vec<_>>(), vec!["sixel", "kitty"], "{txt}");
    assert_eq!(imgs[0].image.size(), (4, 12));
    assert!(imgs[1].row > imgs[0].row, "second image placed below first");
    assert!(s.notices.lock().unwrap().iter().any(|(t, l)| t == "部署" && l.contains("DEPLOY_DONE")));
    assert!(s.stop_recording());
    let rec = std::fs::read_to_string(&cast).unwrap();
    assert!(rec.contains("AFTER_IMG") && !rec.contains("sk-abcdefghijklmnopqrstuvwxyz"));
    let c = ssh_term_proto::asciicast::parse(&rec).unwrap();
    assert_eq!((c.width, c.height), (80, 24));
}

#[test]
fn graphics_in_single_chunk_keep_order() {
    let (s, out, _rx) = ssh_term_proto::term::Session::new(Size { cols: 40, rows: 20 });
    out.send(b"A\r\n\x1bPq\"1;1;2;6#1!2~\x1b\\B\r\n\x1b_Ga=T,f=24,s=1,v=1;AP8A\x1b\\C\r\n".to_vec()).unwrap();
    let txt = wait_for(&s, "C", 5);
    let imgs = s.images.lock().unwrap().clone();
    let line_of = |w: &str| txt.lines().position(|l| l.trim() == w).unwrap() as i64;
    assert_eq!(imgs.len(), 2);
    assert!(imgs[0].row > line_of("A") && imgs[0].row < line_of("B"), "{txt}");
    assert!(imgs[1].row > line_of("B") && imgs[1].row < line_of("C"), "{txt}");
}
