//! SFTP follow-cwd integration against the local test sshd (TEST_SSH_PORT, key auth):
//! follow-terminal-cwd (OSC 7 and prompt fallback) drives the SFTP listing, and a saved
//! layout with SFTP tabs restores to the same host + directory.
use std::time::{Duration, Instant};

use ssh_term_proto::ctxmenu;
use ssh_term_proto::session_store::{self, LayoutSave, NodeSave, PaneSave};
use ssh_term_proto::sftp;
use ssh_term_proto::sftp_follow::Follow;
use ssh_term_proto::ssh::{self, SshTarget};
use ssh_term_proto::term::{Session, Size};

fn env(k: &str) -> Option<String> { std::env::var(k).ok().filter(|v| !v.is_empty()) }
fn wait<T>(secs: u64, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() { return Some(v); }
        if t0.elapsed() > Duration::from_secs(secs) { return None; } std::thread::sleep(Duration::from_millis(100));
    }
}
fn prompt_line(s: &Session) -> Option<String> { s.screen_text().lines().rev().find(|l| l.trim_end().ends_with('$')).map(str::to_string) }

#[test]
fn follow_cwd_and_restore_against_sshd() {
    let (Some(port), Some(user), Some(key)) = (env("TEST_SSH_PORT"), env("TEST_SSH_USER"), env("TEST_SSH_KEY")) else { eprintln!("skip: TEST_SSH_* unset"); return };
    let kh = std::env::temp_dir().join(format!("stp-sm-kh-{}", std::process::id()));
    let target = SshTarget { host: "127.0.0.1".into(), port: port.parse().unwrap(), user, keys: vec![key.into()], known_hosts: kh, ..Default::default() };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (htx, hrx) = flume::unbounded::<ssh::HostKeyPrompt>();
    let (atx, _arx) = flume::unbounded::<ssh::AuthPrompt>();
    std::thread::spawn(move || while let Ok(p) = hrx.recv() { let _ = p.reply.send(ssh::HostKeyDecision::AcceptAndSave); });
    let prompts = ssh::Prompts { hostkey: htx, auth: atx };
    let conn = rt.block_on(ssh::connect(&target, &prompts)).expect("connect");
    let exec = |c: &str| rt.block_on(conn.exec(c, Duration::from_secs(20))).unwrap().0;
    let sf = rt.block_on(conn.sftp()).expect("sftp");
    let home = rt.block_on(sf.canonicalize(".")).unwrap();

    let base = format!("/tmp/sm-it-{}", std::process::id());
    let (d1, d2) = (format!("{base}/osc 七"), format!("{base}/prompt"));
    assert_eq!(exec(&format!("rm -rf {b} && mkdir -p {a} {c} && touch {a}/f-osc7 {c}/f-prompt", b = ctxmenu::quote(&base), a = ctxmenu::quote(&d1), c = ctxmenu::quote(&d2))), Some(0));

    // 1) OSC 7 from the remote shell -> follow -> SFTP lists that directory
    let s = ssh::spawn_ssh(rt.handle(), target.clone(), Size { cols: 200, rows: 30 }, prompts.clone());
    wait(15, || prompt_line(&s)).expect("prompt");
    s.write(format!("cd {} && printf '\\033]7;file://h%s\\007' \"$PWD\"\r", ctxmenu::quote(&d1)));
    let cwd = wait(10, || s.osc7_cwd.lock().unwrap().clone()).expect("osc7 cwd");
    let mut f = Follow::new(true);
    let dir = f.step(Some(&cwd), &home, &home).expect("navigate");
    assert_eq!(dir, d1);
    let ls = rt.block_on(sftp::list_remote(&sf, &dir, false)).unwrap();
    assert!(ls.iter().any(|e| e.name == "f-osc7"), "{ls:?}");
    // same cwd again (user browsed elsewhere) -> no jump back
    assert_eq!(f.step(Some(&cwd), &home, "/etc"), None);

    // 2) no OSC 7: prompt fallback (user@host:~/dir$ or absolute path)
    s.write(format!("cd {}\r", ctxmenu::quote(&d2)));
    let pc = wait(10, || prompt_line(&s).and_then(|l| ctxmenu::prompt_cwd(&l)).filter(|c| c.ends_with("prompt"))).expect("prompt cwd");
    let dir2 = f.step(Some(&pc), &home, &dir).expect("navigate 2");
    assert_eq!(dir2, d2);
    assert!(rt.block_on(sftp::list_remote(&sf, &dir2, false)).unwrap().iter().any(|e| e.name == "f-prompt"));
    s.write("cd ~\r".to_string());
    let pc = wait(10, || prompt_line(&s).and_then(|l| ctxmenu::prompt_cwd(&l)).filter(|c| c == "~" || *c == home)).expect("home prompt");
    assert_eq!(f.step(Some(&pc), &home, &dir2).as_deref(), Some(home.as_str()));

    // 3) restore: layout with an SFTP tab and an SFTP split reopens the same host + dirs
    let l = LayoutSave { version: 1, saved_at: "t".into(), active: 0, names: vec![], tabs: vec![
        NodeSave::Leaf { pane: PaneSave::Sftp { target: target.clone(), local_dir: "/tmp".into(), remote_dir: d1.clone(), follow: false, panes: vec![] } },
        NodeSave::Split { vertical: true, ratio: 0.5, a: Box::new(NodeSave::Leaf { pane: PaneSave::Ssh { target: target.clone(), resume: None } }),
            b: Box::new(NodeSave::Leaf { pane: PaneSave::Sftp { target: target.clone(), local_dir: String::new(), remote_dir: d2.clone(), follow: true, panes: vec![] } }) },
    ] };
    let p = std::env::temp_dir().join(format!("stp-sm-layout-{}.json", std::process::id()));
    session_store::save(&p, &l).unwrap();
    let back = session_store::load(&p).unwrap();
    let mut seen = vec![];
    fn walk(n: &NodeSave, out: &mut Vec<PaneSave>) { match n { NodeSave::Leaf { pane } => out.push(pane.clone()), NodeSave::Split { a, b, .. } => { walk(a, out); walk(b, out); } } }
    for t in &back.tabs { walk(t, &mut seen); }
    let sftps: Vec<_> = seen.iter().filter_map(|p| match p { PaneSave::Sftp { target, remote_dir, follow, .. } => Some((target.clone(), remote_dir.clone(), *follow)), _ => None }).collect();
    assert_eq!(sftps.len(), 2);
    for (t, rd, _) in &sftps {
        let (c2, _) = rt.block_on(ssh::connect_shared(t, &prompts)).unwrap();
        let s2 = rt.block_on(c2.sftp()).unwrap();
        assert_eq!(rt.block_on(s2.canonicalize(rd.clone())).unwrap(), *rd);
        assert!(!rt.block_on(sftp::list_remote(&s2, rd, false)).unwrap().is_empty());
    }
    assert!(sftps[1].2 && !sftps[0].2, "follow flag persisted per pane");
    let _ = std::fs::remove_file(p);
    s.close();
    exec(&format!("rm -rf {}", ctxmenu::quote(&base)));
}
