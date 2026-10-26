//! Live Docker tests. Ignored by default; run with `cargo test --test docker_live -- --ignored`
//! and SHELLAX_TEST_DOCKER=1 (local daemon; the test pulls `alpine` and starts a throwaway container).
//! The SSH case also needs TEST_SSH_PORT2 / TEST_SSH_USER / TEST_SSH_KEY (see testenv/).
use ssh_term_proto::assets::DockerEndpoint;
use ssh_term_proto::dock::{Action, Dock};

fn env(k: &str) -> Option<String> { std::env::var(k).ok().filter(|v| !v.is_empty()) }
fn rt() -> tokio::runtime::Runtime { tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap() }
fn docker(args: &[&str]) -> String { String::from_utf8(std::process::Command::new("docker").args(args).output().unwrap().stdout).unwrap().trim().to_string() }

#[test]
#[ignore]
fn docker_local() {
    if env("SHELLAX_TEST_DOCKER").is_none() { eprintln!("SHELLAX_TEST_DOCKER not set"); return }
    rt().block_on(async {
        let d = Dock::open(&DockerEndpoint { endpoint: "local".into(), ..Default::default() }, None).await.unwrap();
        assert!(!d.version.is_empty());
        let (tx, rx) = flume::unbounded();
        d.pull("alpine:latest", Some(tx)).await.unwrap();
        assert!(rx.drain().count() > 0);
        assert!(d.images().await.unwrap().iter().any(|i| i.tags.iter().any(|t| t == "alpine:latest")));
        let _ = docker(&["rm", "-f", "sx-dk-test"]);
        docker(&["run", "-d", "--name", "sx-dk-test", "--label", "com.docker.compose.project=sxproj", "--label", "com.docker.compose.service=app", "alpine", "sh", "-c", "trap 'exit 0' TERM; for i in 1 2 3; do echo line$i; done; sleep 600 & wait"]);
        let cs = d.containers().await.unwrap();
        let c = cs.iter().find(|c| c.name == "sx-dk-test").expect("container listed").clone();
        assert_eq!((c.state.as_str(), c.project.as_deref(), c.service.as_deref()), ("running", Some("sxproj"), Some("app")));
        assert!(ssh_term_proto::dock::group(&cs).iter().any(|(p, v)| p.as_deref() == Some("sxproj") && v.len() == 1));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let (tx, rx) = flume::unbounded();
        d.logs(&c.id, 100, false, false, tx).await.unwrap();
        assert_eq!(rx.drain().collect::<Vec<_>>(), vec!["line1", "line2", "line3"]);
        assert!(d.inspect(&c.id).await.unwrap().contains("\"sx-dk-test\"") || d.inspect(&c.id).await.unwrap().contains("/sx-dk-test"));
        d.act(&c.id, Action::Pause).await.unwrap();
        d.act(&c.id, Action::Unpause).await.unwrap();
        d.act(&c.id, Action::Stop).await.unwrap();
        let mut st = String::new();
        for _ in 0..20 { st = d.containers().await.unwrap().iter().find(|x| x.id == c.id).unwrap().state.clone(); if st == "exited" { break } tokio::time::sleep(std::time::Duration::from_millis(250)).await; }
        assert_eq!(st, "exited");
        d.act(&c.id, Action::Start).await.unwrap();
        d.act(&c.id, Action::Restart).await.unwrap();
        d.act(&c.id, Action::Remove).await.unwrap();
        assert!(!d.containers().await.unwrap().iter().any(|x| x.id == c.id));
        assert!(d.volumes().await.is_ok());
        assert!(d.networks().await.unwrap().iter().any(|n| n.name == "bridge"));
        d.close();
    });
}

#[test]
#[ignore]
fn docker_over_ssh() {
    use ssh_term_proto::ssh::{self, SshTarget};
    if env("SHELLAX_TEST_DOCKER").is_none() { return }
    let (Some(port), Some(user), Some(key)) = (env("TEST_SSH_PORT2"), env("TEST_SSH_USER"), env("TEST_SSH_KEY")) else { eprintln!("TEST_SSH_* not set"); return };
    let (htx, hrx) = flume::unbounded::<ssh::HostKeyPrompt>();
    std::thread::spawn(move || while let Ok(p) = hrx.recv() { let _ = p.reply.send(ssh::HostKeyDecision::AcceptOnce); });
    let (atx, _arx) = flume::unbounded::<ssh::AuthPrompt>();
    let kh = std::env::temp_dir().join(format!("sx-dk-kh-{}", std::process::id()));
    rt().block_on(async {
        let t = SshTarget { host: "127.0.0.1".into(), port: port.parse().unwrap(), user, auth: "key".into(), keys: vec![key.into()], known_hosts: kh, ..Default::default() };
        let d = Dock::open(&DockerEndpoint { endpoint: "ssh".into(), ..Default::default() }, Some((t, ssh::Prompts { hostkey: htx, auth: atx }))).await.unwrap();
        assert!(d.via.is_some() && !d.version.is_empty());
        assert!(!d.networks().await.unwrap().is_empty());
        d.containers().await.unwrap();
        d.close();
    });
}
