//! Local keep-alive daemon: a shell survives the client detaching, output is replayed on reattach.
#![cfg(unix)]
use std::time::{Duration, Instant};
use alacritty_terminal::grid::Dimensions;
use ssh_term_proto::daemon;
use ssh_term_proto::term::{Session, Size};

fn screen(s: &Session) -> String {
    let t = s.term.lock().unwrap();
    let g = t.grid();
    let mut out = String::new();
    for l in 0..g.screen_lines() { for c in 0..g.columns() { out.push(g[alacritty_terminal::index::Line(l as i32)][alacritty_terminal::index::Column(c)].c); } out.push('\n'); }
    out
}
fn wait(s: &Session, needle: &str) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(8) { if screen(s).contains(needle) { return true; } std::thread::sleep(Duration::from_millis(100)); }
    eprintln!("{}", screen(s));
    false
}

#[test]
fn daemon_keeps_shell_alive_across_detach() {
    let dir = std::env::temp_dir().join(format!("stp-daemon-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = daemon::socket_path(&dir);
    let s2 = sock.clone();
    std::thread::spawn(move || daemon::serve(&s2).unwrap());
    for _ in 0..50 { if std::os::unix::net::UnixStream::connect(&sock).is_ok() { break } std::thread::sleep(Duration::from_millis(50)); }
    let size = Size { cols: 80, rows: 24 };
    let a = daemon::attach(&sock, "p1", size, "/bin/bash", dir.to_str().unwrap()).unwrap();
    a.write(b"export MARK=keepalive_$((6*7)); pwd\n".to_vec());
    assert!(wait(&a, "stp-daemon-"));
    a.close(); // window closed: detach
    std::thread::sleep(Duration::from_millis(300));
    let l = daemon::list(&sock).unwrap();
    assert_eq!(l.len(), 1);
    assert!(l[0].alive && !l[0].attached);
    assert!(l[0].cwd.contains("stp-daemon-"));
    let b = daemon::attach(&sock, "p1", size, "", "").unwrap();
    assert!(wait(&b, "stp-daemon-"), "replayed output");
    b.write(b"echo $MARK\n".to_vec());
    assert!(wait(&b, "keepalive_42"), "same shell process (env survived)");
    daemon::kill(&sock, "p1").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(daemon::list(&sock).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
