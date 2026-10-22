//! PRD 5.20 integration against the local test sshd (TEST_SSH_PORT, key auth):
//! a context-menu command inserted into a real remote shell has no side effect until Enter,
//! and after Enter a zip with spaces + CJK in its name is really extracted on the remote.
use std::time::{Duration, Instant};

use ssh_term_proto::ctxmenu::{self, Act, Ctx, FileObj, Place, Target};
use ssh_term_proto::ssh::{self, SshTarget};
use ssh_term_proto::term::{Session, Size};

fn env(k: &str) -> Option<String> { std::env::var(k).ok().filter(|v| !v.is_empty()) }

fn wait_for(s: &Session, needle: &str, secs: u64) -> String {
    let t0 = Instant::now();
    loop {
        let txt = s.screen_text();
        if txt.contains(needle) || t0.elapsed() > Duration::from_secs(secs) { return txt; }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn insert_does_not_execute_until_enter_then_zip_extracts() {
    let (Some(port), Some(user), Some(key)) = (env("TEST_SSH_PORT"), env("TEST_SSH_USER"), env("TEST_SSH_KEY")) else { eprintln!("skip: TEST_SSH_* unset"); return };
    let kh = std::env::temp_dir().join(format!("stp-cm-kh-{}", std::process::id()));
    let target = SshTarget { host: "127.0.0.1".into(), port: port.parse().unwrap(), user, keys: vec![key.into()], known_hosts: kh, ..Default::default() };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (htx, hrx) = flume::unbounded::<ssh::HostKeyPrompt>();
    let (atx, _arx) = flume::unbounded::<ssh::AuthPrompt>();
    std::thread::spawn(move || while let Ok(p) = hrx.recv() { let _ = p.reply.send(ssh::HostKeyDecision::AcceptAndSave); });
    let prompts = ssh::Prompts { hostkey: htx, auth: atx };
    let conn = rt.block_on(ssh::connect(&target, &prompts)).expect("connect");
    let exec = |c: &str| { let (code, out) = rt.block_on(conn.exec(c, Duration::from_secs(20))).unwrap(); (code, String::from_utf8_lossy(&out).into_owned()) };

    // remote fixture: a zip named with a space and CJK characters
    let base = format!("/tmp/cm-it-{}", std::process::id());
    let zip = format!("{base}/my 报告.zip");
    let mk = format!("rm -rf {b} && mkdir -p {b} && python3 -c \"import zipfile,sys; z=zipfile.ZipFile(sys.argv[1],'w'); z.writestr('inner.txt','ctx-menu-ok\\n'); z.close()\" {z}", b = ctxmenu::quote(&base), z = ctxmenu::quote(&zip));
    assert_eq!(exec(&mk).0, Some(0), "fixture");

    // tool detection on the remote
    let tools = ctxmenu::parse_probe(&exec(&ctxmenu::probe_command()).1);
    assert!(tools.has("tar") && tools.has("bash"), "{tools:?}");
    if !tools.has("unzip") { eprintln!("skip: no unzip on the test host"); return; }

    // the menu for the remote zip: "extract to my 报告/"
    let menu = ctxmenu::build(&Target::Files(vec![FileObj { path: zip.clone(), is_dir: false, mode: 0o644 }]), &Ctx { host: "127.0.0.1", place: Place::RemotePanel, tools: Some(&tools), custom: &[], lang_en: true });
    let item = menu.items.iter().find(|i| i.id == "extract_to").expect("extract_to");
    assert!(item.enabled());
    let Act::Insert(cmd) = &item.act else { panic!() };
    assert_eq!(cmd, &format!("mkdir -p '{base}/my 报告' && unzip -o '{zip}' -d '{base}/my 报告'"));
    // tools missing on this host are greyed out with an install hint
    if !tools.has("7z") {
        let m7 = ctxmenu::build(&Target::Files(vec![FileObj { path: format!("{base}/x.7z"), is_dir: false, mode: 0 }]), &Ctx { host: "127.0.0.1", place: Place::RemotePanel, tools: Some(&tools), custom: &[], lang_en: true });
        let it = m7.items.iter().find(|i| i.id == "extract_here").unwrap();
        assert!(!it.enabled() && it.hint.as_deref().is_some_and(|h| h.contains("7zip") || h.contains("p7zip") || h.contains("sevenzip")), "{it:?}");
    }

    // interactive remote shell: insert (Ctrl+U + text, no newline)
    let s = ssh::spawn_ssh(rt.handle(), target.clone(), Size { cols: 200, rows: 30 }, prompts.clone());
    wait_for(&s, "$", 15);
    std::thread::sleep(Duration::from_millis(500));
    s.write("echo typed-garbage-not-run".to_string()); // something already on the line gets cleared
    std::thread::sleep(Duration::from_millis(300));
    s.write(ctxmenu::insert_bytes(cmd));
    let screen = wait_for(&s, "unzip -o", 10);
    assert!(screen.contains("unzip -o"), "command visible on the input line:\n{screen}");
    std::thread::sleep(Duration::from_secs(2));
    // no side effect yet
    let (code, out) = exec(&format!("test -e {} && echo EXISTS || echo ABSENT", ctxmenu::quote(&format!("{base}/my 报告"))));
    assert_eq!((code, out.trim()), (Some(0), "ABSENT"), "inserted command must not run before Enter");
    assert!(!s.screen_text().contains("typed-garbage-not-run\n"), "previous line content was cleared, not executed");
    // Enter → runs
    s.write("\r".to_string());
    let t0 = Instant::now();
    let content = loop {
        let (_, o) = exec(&format!("cat {} 2>/dev/null", ctxmenu::quote(&format!("{base}/my 报告/inner.txt"))));
        if o.contains("ctx-menu-ok") || t0.elapsed() > Duration::from_secs(15) { break o; }
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(content.trim(), "ctx-menu-ok", "zip extracted on the remote after Enter");
    let (code, out) = exec(&format!("ls {}", ctxmenu::quote(&base)));
    assert_eq!(code, Some(0));
    assert!(out.contains("my 报告"), "{out}");
    // the cleared garbage line never ran (it would only have echoed, but must not appear as output)
    let screen = s.screen_text();
    assert!(!screen.lines().any(|l| l.trim() == "typed-garbage-not-run"), "{screen}");
    exec(&format!("rm -rf {}", ctxmenu::quote(&base)));
}
