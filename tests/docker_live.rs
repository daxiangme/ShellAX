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

#[test]
#[ignore]
fn docker_create_exec_stats_compose() {
    if env("SHELLAX_TEST_DOCKER").is_none() { eprintln!("SHELLAX_TEST_DOCKER not set"); return }
    use ssh_term_proto::dock::{ComposeOp, CreateSpec};
    rt().block_on(async {
        let d = Dock::open(&DockerEndpoint { endpoint: "local".into(), ..Default::default() }, None).await.unwrap();
        let _ = docker(&["rm", "-f", "sx-dk-create"]);
        let spec = CreateSpec { image: "alpine:latest".into(), name: "sx-dk-create".into(), ports: vec!["127.0.0.1:18089:80".into()], env: vec!["SXV=hello world".into()],
            volumes: vec!["sx-dk-vol:/data".into()], restart: "unless-stopped".into(), command: "sh -c 'sleep 600'".into(), start: true };
        let id = d.create(&spec).await.unwrap();
        let out = d.exec_once(&id, "sh -c 'echo $SXV; ls /data >/dev/null && echo ok'").await.unwrap();
        assert!(out.contains("hello world") && out.contains("ok"), "{out}");
        assert!(d.exec_once(&id, "sh -c 'exit 3'").await.unwrap().contains("[exit 3]"));
        let s = d.stats(&id).await.unwrap();
        assert!(s.mem > 0 && s.limit > 0, "{s:?}");
        assert!(docker(&["inspect", "-f", "{{.HostConfig.RestartPolicy.Name}} {{(index (index .HostConfig.PortBindings \"80/tcp\") 0).HostPort}}", &id]) == "unless-stopped 18089");
        d.act(&id, Action::Remove).await.unwrap();
        let _ = docker(&["volume", "rm", "sx-dk-vol"]);
        // compose by project name (the CLI must be present)
        if std::process::Command::new("docker").args(["compose", "version"]).output().is_ok_and(|o| o.status.success()) {
            let dir = std::env::temp_dir().join("sx-compose-test");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("compose.yml"), "services:\n  app:\n    image: alpine:latest\n    command: sleep 600\n").unwrap();
            let f = dir.join("compose.yml").to_string_lossy().to_string();
            let w = dir.to_string_lossy().to_string();
            d.compose("sxcomp", Some(&w), Some(&f), ComposeOp::Up, None).await.unwrap();
            let running = |cs: &[ssh_term_proto::dock::Container]| cs.iter().filter(|c| c.project.as_deref() == Some("sxcomp") && c.state == "running").count();
            let cs = d.containers().await.unwrap();
            assert_eq!(running(&cs), 1);
            let c = cs.iter().find(|c| c.project.as_deref() == Some("sxcomp")).unwrap();
            assert!(c.workdir.is_some() && c.config_files.is_some());
            d.compose("sxcomp", None, None, ComposeOp::Stop, None).await.unwrap();
            assert_eq!(running(&d.containers().await.unwrap()), 0);
            d.compose("sxcomp", c.workdir.as_deref(), c.config_files.as_deref(), ComposeOp::Up, None).await.unwrap();
            d.compose("sxcomp", None, None, ComposeOp::Restart, None).await.unwrap();
            d.compose("sxcomp", None, None, ComposeOp::Down, None).await.unwrap();
            assert!(!d.containers().await.unwrap().iter().any(|c| c.project.as_deref() == Some("sxcomp")));
        } else { eprintln!("docker compose not available: compose part skipped"); }
        d.close();
    });
}
