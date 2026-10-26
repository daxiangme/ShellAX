//! Docker access (v0.2 PRD §6.5): local socket or a remote daemon reached over SSH
//! (`direct-streamlocal` to the docker socket, falling back to `docker system dial-stdio`).
//! Containers, logs, inspect, images (pull/remove), volumes, networks, compose grouping.
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use bollard::query_parameters as qp;
use bollard::Docker;
use futures_util::StreamExt;
use tokio::net::TcpListener;

use crate::assets::DockerEndpoint;
use crate::ssh::{Conn, Prompts, SshTarget};

pub struct Dock {
    pub api: Docker,
    pub version: String,
    pub via: Option<String>,
    /// SSH relay: dropped on close.
    relay: Option<tokio::task::JoinHandle<()>>,
    pub ssh: Option<Arc<Conn>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// created | running | paused | restarting | exited | removing | dead
    pub state: String,
    pub status: String,
    pub ports: String,
    pub created: i64,
    pub project: Option<String>,
    pub service: Option<String>,
    /// compose working dir and config files (labels), used by `compose up`
    pub workdir: Option<String>,
    pub config_files: Option<String>,
}

/// What the create-container dialog collects. Multi-value fields are one entry per line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CreateSpec {
    pub image: String,
    pub name: String,
    /// `host:container[/proto]` or `container[/proto]`
    pub ports: Vec<String>,
    /// `KEY=value`
    pub env: Vec<String>,
    /// `host_path_or_volume:container_path[:ro]`
    pub volumes: Vec<String>,
    /// no | always | unless-stopped | on-failure
    pub restart: String,
    /// shell-like words; empty = image default
    pub command: String,
    pub start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats { pub cpu: f64, pub mem: u64, pub limit: u64 }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ComposeOp { Up, Stop, Restart, Down }
impl ComposeOp {
    pub fn arg(self) -> &'static str { match self { ComposeOp::Up => "up -d", ComposeOp::Stop => "stop", ComposeOp::Restart => "restart", ComposeOp::Down => "down" } }
    pub fn dangerous(self) -> bool { matches!(self, ComposeOp::Stop | ComposeOp::Down | ComposeOp::Restart) }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Image { pub id: String, pub tags: Vec<String>, pub size: i64, pub created: i64, pub containers: i64 }
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VolumeRow { pub name: String, pub driver: String, pub mountpoint: String }
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetworkRow { pub id: String, pub name: String, pub driver: String, pub scope: String }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action { Start, Stop, Restart, Pause, Unpause, Kill, Remove }

/// Compose label keys.
const PROJECT: &str = "com.docker.compose.project";
const SERVICE: &str = "com.docker.compose.service";

fn enum_str<T: serde::Serialize>(v: &T) -> String { serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default() }

impl Dock {
    /// Connect. `via` is the SSH target for `endpoint = "ssh"`.
    pub async fn open(e: &DockerEndpoint, via: Option<(SshTarget, Prompts)>) -> Result<Dock> {
        let (api, via_label, relay, ssh) = match via {
            Some((t, prompts)) if e.is_ssh() => {
                let (conn, _) = crate::ssh::connect_shared(&t, &prompts).await?;
                let lis = TcpListener::bind("127.0.0.1:0").await?;
                let port = lis.local_addr()?.port();
                let sock = e.socket_path().to_string();
                let c2 = conn.clone();
                let relay = tokio::spawn(async move {
                    while let Ok((mut tcp, _)) = lis.accept().await {
                        let c = c2.clone();
                        let sock = sock.clone();
                        tokio::spawn(async move {
                            match c.handle.channel_open_direct_streamlocal(sock.clone()).await {
                                Ok(ch) => { let mut s = ch.into_stream(); let _ = tokio::io::copy_bidirectional(&mut tcp, &mut s).await; }
                                Err(_) => {
                                    // sshd without streamlocal forwarding: let the docker CLI relay
                                    let Ok(ch) = c.handle.channel_open_session().await else { return };
                                    let cmd = format!("DOCKER_HOST=unix://{sock} docker system dial-stdio");
                                    if ch.exec(true, cmd.as_bytes()).await.is_err() { return; }
                                    let mut s = ch.into_stream();
                                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut s).await;
                                }
                            }
                        });
                    }
                });
                let api = Docker::connect_with_http(&format!("http://127.0.0.1:{port}"), 120, bollard::API_DEFAULT_VERSION)?;
                (api, Some(t.label()), Some(relay), Some(conn))
            }
            _ => {
                let api = if e.socket.trim().is_empty() { Docker::connect_with_local_defaults()? } else { Docker::connect_with_socket(e.socket.trim(), 120, bollard::API_DEFAULT_VERSION)? };
                (api, None, None, None)
            }
        };
        let v = match tokio::time::timeout(std::time::Duration::from_secs(15), api.version()).await.context("timeout")? {
            Ok(v) => v,
            Err(err) => { if let Some(r) = &relay { r.abort(); } return Err(err.into()); }
        };
        Ok(Dock { api, version: v.version.unwrap_or_default(), via: via_label, relay, ssh })
    }

    pub fn close(&self) { if let Some(r) = &self.relay { r.abort(); } }

    pub async fn containers(&self) -> Result<Vec<Container>> {
        let list = self.api.list_containers(Some(qp::ListContainersOptions { all: true, ..Default::default() })).await?;
        let mut out: Vec<Container> = list.into_iter().map(|c| {
            let labels = c.labels.unwrap_or_default();
            let mut ports: Vec<String> = c.ports.unwrap_or_default().iter().map(|p| match p.public_port {
                Some(pp) => format!("{}:{pp}->{}/{}", p.ip.clone().unwrap_or_default(), p.private_port, p.typ.as_ref().map(enum_str).unwrap_or_default()),
                None => format!("{}/{}", p.private_port, p.typ.as_ref().map(enum_str).unwrap_or_default()),
            }).collect();
            ports.sort(); ports.dedup();
            Container {
                id: c.id.unwrap_or_default(),
                name: c.names.unwrap_or_default().first().map(|n| n.trim_start_matches('/').to_string()).unwrap_or_default(),
                image: c.image.unwrap_or_default(),
                state: c.state.as_ref().map(enum_str).unwrap_or_default(),
                status: c.status.unwrap_or_default(),
                ports: ports.join(", "),
                created: c.created.unwrap_or(0),
                project: labels.get(PROJECT).cloned(),
                service: labels.get(SERVICE).cloned(),
                workdir: labels.get("com.docker.compose.project.working_dir").cloned(),
                config_files: labels.get("com.docker.compose.project.config_files").cloned(),
            }
        }).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub async fn act(&self, id: &str, a: Action) -> Result<()> {
        match a {
            Action::Start => self.api.start_container(id, None::<qp::StartContainerOptions>).await?,
            Action::Stop => self.api.stop_container(id, Some(qp::StopContainerOptions { t: Some(10), ..Default::default() })).await?,
            Action::Restart => self.api.restart_container(id, Some(qp::RestartContainerOptions { t: Some(10), ..Default::default() })).await?,
            Action::Pause => self.api.pause_container(id).await?,
            Action::Unpause => self.api.unpause_container(id).await?,
            Action::Kill => self.api.kill_container(id, None::<qp::KillContainerOptions>).await?,
            Action::Remove => self.api.remove_container(id, Some(qp::RemoveContainerOptions { force: true, ..Default::default() })).await?,
        }
        Ok(())
    }

    /// `docker inspect` as pretty JSON.
    pub async fn inspect(&self, id: &str) -> Result<String> {
        let v = self.api.inspect_container(id, None::<qp::InspectContainerOptions>).await?;
        Ok(serde_json::to_string_pretty(&v)?)
    }

    /// Stream log lines into `tx` (last `tail` lines, then follow while `follow`).
    /// Ends when the container stops, the stream errors, or the receiver is dropped.
    pub fn logs(&self, id: &str, tail: usize, follow: bool, timestamps: bool, tx: flume::Sender<String>) -> tokio::task::JoinHandle<()> {
        let api = self.api.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            let mut s = api.logs(&id, Some(qp::LogsOptions { follow, stdout: true, stderr: true, timestamps, tail: tail.to_string(), ..Default::default() }));
            let mut part = String::new();
            while let Some(item) = s.next().await {
                let Ok(o) = item else { let _ = tx.send_async(format!("[{}]", item.err().map(|e| e.to_string()).unwrap_or_default())).await; break };
                part.push_str(&String::from_utf8_lossy(&o.into_bytes()));
                while let Some(i) = part.find('\n') {
                    let line: String = part.drain(..=i).collect();
                    if tx.send_async(line.trim_end_matches(['\r', '\n']).to_string()).await.is_err() { return; }
                }
            }
            if !part.is_empty() { let _ = tx.send_async(part).await; }
        })
    }

    pub async fn images(&self) -> Result<Vec<Image>> {
        let mut v: Vec<Image> = self.api.list_images(Some(qp::ListImagesOptions { all: false, ..Default::default() })).await?.into_iter().map(|i| Image {
            id: i.id.trim_start_matches("sha256:").chars().take(12).collect(),
            tags: i.repo_tags.into_iter().filter(|t| t != "<none>:<none>").collect(),
            size: i.size, created: i.created, containers: i.containers,
        }).collect();
        v.sort_by(|a, b| b.created.cmp(&a.created));
        Ok(v)
    }

    /// Pull `reference` (`name[:tag]`), sending progress lines to `tx`.
    pub async fn pull(&self, reference: &str, tx: Option<flume::Sender<String>>) -> Result<()> {
        let (name, tag) = split_ref(reference);
        let mut s = self.api.create_image(Some(qp::CreateImageOptions { from_image: Some(name), tag: Some(tag), ..Default::default() }), None, None);
        while let Some(i) = s.next().await {
            let i = i?;
            if let Some(tx) = &tx {
                let line = [i.id.unwrap_or_default(), i.status.unwrap_or_default(), i.progress_detail.and_then(|p| Some(format!("{}/{}", p.current?, p.total?))).unwrap_or_default()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                let _ = tx.send_async(line).await;
            }
        }
        Ok(())
    }

    pub async fn remove_image(&self, id: &str, force: bool) -> Result<()> {
        self.api.remove_image(id, Some(qp::RemoveImageOptions { force, ..Default::default() }), None).await?;
        Ok(())
    }

    pub async fn volumes(&self) -> Result<Vec<VolumeRow>> {
        let mut v: Vec<VolumeRow> = self.api.list_volumes(None::<qp::ListVolumesOptions>).await?.volumes.unwrap_or_default().into_iter().map(|v| VolumeRow { name: v.name, driver: v.driver, mountpoint: v.mountpoint }).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    pub async fn networks(&self) -> Result<Vec<NetworkRow>> {
        let mut v: Vec<NetworkRow> = self.api.list_networks(None::<qp::ListNetworksOptions>).await?.into_iter().map(|n| NetworkRow {
            id: n.id.unwrap_or_default().chars().take(12).collect(), name: n.name.unwrap_or_default(), driver: n.driver.unwrap_or_default(), scope: n.scope.unwrap_or_default(),
        }).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }
}

impl Dock {
    /// Create (and optionally start) a container; returns its id.
    pub async fn create(&self, spec: &CreateSpec) -> Result<String> {
        let body = create_body(spec)?;
        let body: bollard::models::ContainerCreateBody = serde_json::from_value(body)?;
        let opts = (!spec.name.trim().is_empty()).then(|| qp::CreateContainerOptions { name: Some(spec.name.trim().to_string()), ..Default::default() });
        let r = self.api.create_container(opts, body).await?;
        if spec.start { self.api.start_container(&r.id, None::<qp::StartContainerOptions>).await?; }
        Ok(r.id)
    }

    /// Run one command in a running container and collect stdout+stderr (capped).
    pub async fn exec_once(&self, id: &str, cmd: &str) -> Result<String> {
        let argv = split_words(cmd);
        if argv.is_empty() { anyhow::bail!("empty command"); }
        let cfg: bollard::models::ExecConfig = serde_json::from_value(serde_json::json!({ "AttachStdout": true, "AttachStderr": true, "Cmd": argv }))?;
        let ex = self.api.create_exec(id, cfg).await?;
        let mut out = String::new();
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = self.api.start_exec(&ex.id, None).await? {
            let fut = async { while let Some(Ok(m)) = output.next().await { out.push_str(&m.to_string()); if out.len() > 256 * 1024 { break; } } };
            let _ = tokio::time::timeout(std::time::Duration::from_secs(60), fut).await;
        }
        if let Ok(i) = self.api.inspect_exec(&ex.id).await { if let Some(code) = i.exit_code.filter(|c| *c != 0) { out.push_str(&format!("\n[exit {code}]")); } }
        Ok(out)
    }

    /// One CPU / memory sample (the daemon waits ~1s to have a previous CPU reading).
    pub async fn stats(&self, id: &str) -> Result<Stats> {
        let mut st = self.api.stats(id, Some(qp::StatsOptions { stream: false, one_shot: false }));
        let s = st.next().await.context("no stats")??;
        Ok(parse_stats(&serde_json::to_value(&s)?))
    }

    /// `docker compose` for a project, through the CLI where the daemon lives (local or SSH host).
    pub async fn compose(&self, project: &str, workdir: Option<&str>, files: Option<&str>, op: ComposeOp, socket: Option<&str>) -> Result<String> {
        let cmd = compose_command(project, workdir, files, op, socket);
        match &self.ssh {
            Some(conn) => {
                let (code, out) = conn.exec(&cmd, std::time::Duration::from_secs(300)).await?;
                let out = String::from_utf8_lossy(&out).to_string();
                if code.unwrap_or(1) != 0 { anyhow::bail!("{}", out.trim()); }
                Ok(out)
            }
            None => {
                let mut c = if cfg!(windows) { let mut c = tokio::process::Command::new("cmd"); c.arg("/C").arg(&cmd); c } else { let mut c = tokio::process::Command::new("sh"); c.arg("-c").arg(&cmd); c };
                let o = tokio::time::timeout(std::time::Duration::from_secs(300), c.output()).await.context("timeout")??;
                let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                if !o.status.success() { anyhow::bail!("{}", out.trim()); }
                Ok(out)
            }
        }
    }
}

/// Docker API JSON for a create request.
pub fn create_body(spec: &CreateSpec) -> Result<serde_json::Value> {
    use serde_json::{json, Map, Value};
    let image = spec.image.trim();
    if image.is_empty() { anyhow::bail!("image is required"); }
    let mut exposed = Map::new();
    let mut bindings = Map::new();
    for p in spec.ports.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let (host, inner) = match p.rsplit_once(':') { Some((h, i)) => (Some(h), i), None => (None, p) };
        let inner = if inner.contains('/') { inner.to_string() } else { format!("{inner}/tcp") };
        let port_ok = inner.split('/').next().is_some_and(|n| n.parse::<u16>().is_ok());
        if !port_ok { anyhow::bail!("bad port: {p}"); }
        exposed.insert(inner.clone(), json!({}));
        if let Some(h) = host {
            let (ip, hp) = match h.rsplit_once(':') { Some((ip, hp)) => (ip.to_string(), hp.to_string()), None => (String::new(), h.to_string()) };
            bindings.entry(inner).or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({ "HostIp": ip, "HostPort": hp }));
        }
    }
    let env: Vec<&str> = spec.env.iter().map(|e| e.trim()).filter(|e| !e.is_empty()).collect();
    if let Some(bad) = env.iter().find(|e| !e.contains('=')) { anyhow::bail!("bad env: {bad}"); }
    let binds: Vec<&str> = spec.volumes.iter().map(|v| v.trim()).filter(|v| !v.is_empty()).collect();
    if let Some(bad) = binds.iter().find(|v| !v.contains(':')) { anyhow::bail!("bad volume: {bad}"); }
    let restart = match spec.restart.trim() { "" | "no" => "no", r @ ("always" | "unless-stopped" | "on-failure") => r, r => anyhow::bail!("bad restart policy: {r}") };
    let mut host = json!({ "PortBindings": Value::Object(bindings), "Binds": binds, "RestartPolicy": { "Name": restart } });
    if restart == "on-failure" { host["RestartPolicy"]["MaximumRetryCount"] = json!(3); }
    let mut body = json!({ "Image": image, "Env": env, "ExposedPorts": Value::Object(exposed), "HostConfig": host });
    let cmd = split_words(&spec.command);
    if !cmd.is_empty() { body["Cmd"] = json!(cmd); }
    Ok(body)
}

/// CPU % (of all cores, like `docker stats`) and memory from a stats JSON.
pub fn parse_stats(v: &serde_json::Value) -> Stats {
    let u = |p: &str| v.pointer(p).and_then(|x| x.as_u64()).unwrap_or(0);
    let cpu_d = u("/cpu_stats/cpu_usage/total_usage").saturating_sub(u("/precpu_stats/cpu_usage/total_usage")) as f64;
    let sys_d = u("/cpu_stats/system_cpu_usage").saturating_sub(u("/precpu_stats/system_cpu_usage")) as f64;
    let n = match u("/cpu_stats/online_cpus") { 0 => v.pointer("/cpu_stats/cpu_usage/percpu_usage").and_then(|x| x.as_array()).map(|a| a.len() as u64).unwrap_or(1).max(1), n => n } as f64;
    let cpu = if sys_d > 0. && cpu_d > 0. { cpu_d / sys_d * n * 100. } else { 0. };
    // like the CLI: page cache does not count as used
    let cache = { let a = u("/memory_stats/stats/inactive_file"); if a > 0 { a } else { u("/memory_stats/stats/total_inactive_file") } };
    Stats { cpu, mem: u("/memory_stats/usage").saturating_sub(cache), limit: u("/memory_stats/limit") }
}

pub fn compose_command(project: &str, workdir: Option<&str>, files: Option<&str>, op: ComposeOp, socket: Option<&str>) -> String {
    use crate::ssh::sh_quote as q;
    let host = socket.map(|s| format!("DOCKER_HOST=unix://{} ", q(s))).unwrap_or_default();
    let mut c = format!("{host}docker compose -p {}", q(project));
    if op == ComposeOp::Up {
        if let Some(w) = workdir.filter(|w| !w.is_empty()) { c.push_str(&format!(" --project-directory {}", q(w))); }
        for f in files.unwrap_or_default().split(',').map(str::trim).filter(|f| !f.is_empty()) { c.push_str(&format!(" -f {}", q(f))); }
    }
    c.push(' ');
    c.push_str(op.arg());
    c
}

/// Split a command line on whitespace, honouring '…' and "…" quotes and backslash escapes.
pub fn split_words(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut have = false;
    let mut q: Option<char> = None;
    let mut it = s.chars();
    while let Some(ch) = it.next() {
        match (q, ch) {
            (None, c) if c.is_whitespace() => { if have { out.push(std::mem::take(&mut cur)); have = false; } }
            (None, '\\') => { if let Some(n) = it.next() { cur.push(n); have = true; } }
            (None, '\'' | '"') => { q = Some(ch); have = true; }
            (Some('"'), '\\') => { if let Some(n) = it.next() { cur.push(n); } }
            (Some(qc), c) if c == qc => q = None,
            (_, c) => { cur.push(c); have = true; }
        }
    }
    if have { out.push(cur); }
    out
}

/// `name[:tag]` (a `:` inside a registry host:port is not a tag).
pub fn split_ref(r: &str) -> (String, String) {
    let r = r.trim();
    let r = r.split('@').next().unwrap_or(r);
    match r.rfind(':') {
        Some(i) if !r[i + 1..].contains('/') => (r[..i].to_string(), r[i + 1..].to_string()),
        _ => (r.to_string(), "latest".to_string()),
    }
}

/// Compose projects (sorted) with their containers; containers outside any project go under `None`.
pub fn group(cs: &[Container]) -> Vec<(Option<String>, Vec<Container>)> {
    let mut m: BTreeMap<Option<String>, Vec<Container>> = BTreeMap::new();
    for c in cs { m.entry(c.project.clone()).or_default().push(c.clone()); }
    let mut v: Vec<_> = m.into_iter().collect();
    // projects first, loose containers last
    v.sort_by_key(|(k, _)| (k.is_none(), k.clone()));
    v
}

/// Shell command that opens an interactive shell in a container (bash when present).
pub fn exec_command(id: &str, socket: Option<&str>) -> String {
    let host = socket.map(|s| format!("DOCKER_HOST=unix://{} ", crate::ssh::sh_quote(s))).unwrap_or_default();
    format!("{host}docker exec -it {} sh -c 'command -v bash >/dev/null 2>&1 && exec bash || exec sh'", crate::ssh::sh_quote(id))
}

pub fn human_size(b: i64) -> String {
    let b = b.max(0) as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b; let mut i = 0;
    while v >= 1000. && i < units.len() - 1 { v /= 1000.; i += 1; }
    if i == 0 { format!("{} B", b as i64) } else { format!("{v:.1} {}", units[i]) }
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn create_and_words() {
        assert_eq!(split_words(r#"sh -c 'echo hi there' "a b" c\ d"#), vec!["sh", "-c", "echo hi there", "a b", "c d"]);
        let spec = CreateSpec { image: "nginx".into(), ports: vec!["8080:80".into(), "127.0.0.1:9000:9000/udp".into(), "443".into()], env: vec!["A=1".into()], volumes: vec!["/data:/d:ro".into()], restart: "always".into(), command: "nginx -g 'daemon off;'".into(), ..Default::default() };
        let b = create_body(&spec).unwrap();
        assert_eq!(b["HostConfig"]["PortBindings"]["80/tcp"][0]["HostPort"], "8080");
        assert_eq!(b["HostConfig"]["PortBindings"]["9000/udp"][0]["HostIp"], "127.0.0.1");
        assert!(b["ExposedPorts"]["443/tcp"].is_object() && b["HostConfig"]["PortBindings"]["443/tcp"].is_null());
        assert_eq!(b["Cmd"][2], "daemon off;");
        let _: bollard::models::ContainerCreateBody = serde_json::from_value(b).unwrap();
        assert!(create_body(&CreateSpec { image: "x".into(), env: vec!["NOEQ".into()], ..Default::default() }).is_err());
        assert!(create_body(&CreateSpec { image: "x".into(), ports: vec!["abc".into()], ..Default::default() }).is_err());
        assert!(create_body(&CreateSpec::default()).is_err());
    }
    #[test]
    fn stats_and_compose() {
        let v = serde_json::json!({"cpu_stats": {"cpu_usage": {"total_usage": 300}, "system_cpu_usage": 2000, "online_cpus": 2},
            "precpu_stats": {"cpu_usage": {"total_usage": 100}, "system_cpu_usage": 1000}, "memory_stats": {"usage": 500, "limit": 1000, "stats": {"inactive_file": 100}}});
        let s = parse_stats(&v);
        assert!((s.cpu - 40.).abs() < 1e-9 && s.mem == 400 && s.limit == 1000);
        assert_eq!(compose_command("web", Some("/srv/web"), Some("/srv/web/a.yml,/srv/web/b.yml"), ComposeOp::Up, None), "docker compose -p web --project-directory /srv/web -f /srv/web/a.yml -f /srv/web/b.yml up -d");
        assert_eq!(compose_command("web", Some("/x"), None, ComposeOp::Down, Some("/s.sock")), "DOCKER_HOST=unix:///s.sock docker compose -p web down");
    }
    #[test]
    fn refs() {
        assert_eq!(split_ref("redis"), ("redis".into(), "latest".into()));
        assert_eq!(split_ref("redis:7"), ("redis".into(), "7".into()));
        assert_eq!(split_ref("reg:5000/app"), ("reg:5000/app".into(), "latest".into()));
        assert_eq!(split_ref("reg:5000/app:1.2"), ("reg:5000/app".into(), "1.2".into()));
    }
    #[test]
    fn grouping_and_misc() {
        let c = |n: &str, p: Option<&str>| Container { name: n.into(), project: p.map(str::to_string), ..Default::default() };
        let g = group(&[c("x", None), c("a1", Some("web")), c("b1", Some("api")), c("a2", Some("web"))]);
        assert_eq!(g.iter().map(|(k, v)| (k.clone(), v.len())).collect::<Vec<_>>(), vec![(Some("api".into()), 1), (Some("web".into()), 2), (None, 1)]);
        assert_eq!(human_size(1_500_000), "1.5 MB");
        assert!(exec_command("abc", None).starts_with("docker exec -it abc sh -c"));
    }
}
