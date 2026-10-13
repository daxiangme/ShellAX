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

#[test]
fn ssh_localhost_ls() {
    let Ok(port) = std::env::var("TEST_SSH_PORT") else { eprintln!("TEST_SSH_PORT unset, skipping"); return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let target = ssh::SshTarget {
        host: "127.0.0.1".into(),
        port: port.parse().unwrap(),
        user: std::env::var("TEST_SSH_USER").unwrap(),
        key: std::env::var("TEST_SSH_KEY").unwrap().into(),
    };
    let s = ssh::spawn_ssh(rt.handle(), target, Size { cols: 100, rows: 30 }).unwrap();
    s.resize(Size { cols: 90, rows: 25 });
    s.write("stty size; ls\r");
    let txt = wait_for(&s, "hello_from_sshd.txt", 10);
    println!("{txt}");
    assert!(txt.contains("hello_from_sshd.txt"), "remote ls output not in grid");
    assert!(txt.contains("25 90"), "window_change not applied on remote pty");
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
