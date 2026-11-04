//! Homebrew 的路径、JSON 和命令参数。这里不启动 `brew`，界面层负责执行。
use std::path::{Path, PathBuf};

use serde_json::Value;

/// 官方安装命令。只用于复制，不自动执行。
pub const INSTALL_CMD: &str = "/bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"";

/// 查询时关掉自动更新、颜色和交互，避免一打开面板就去拉索引。
pub const ENV: &[(&str, &str)] = &[
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
    ("HOMEBREW_NO_COLOR", "1"),
    ("HOMEBREW_NO_EMOJI", "1"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
    ("NONINTERACTIVE", "1"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Formula,
    Cask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvcAct {
    Start,
    Stop,
    Restart,
}

impl SvcAct {
    pub fn arg(self) -> &'static str {
        match self {
            SvcAct::Start => "start",
            SvcAct::Stop => "stop",
            SvcAct::Restart => "restart",
        }
    }
}

/// 一次交给队列的操作。`Refresh` 会连续跑清单和服务两条命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Refresh,
    Update,
    UpgradeAll,
    Upgrade { name: String, cask: bool },
    Uninstall { name: String, cask: bool },
    Pin { name: String, unpin: bool },
    Service { name: String, act: SvcAct },
    Cleanup,
    Search { query: String },
    Install { name: String, cask: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub args: Vec<String>,
    pub capture: bool,
}

impl Op {
    pub fn commands(&self) -> Vec<Cmd> {
        let cap = |args: &[&str]| Cmd {
            args: args.iter().map(|s| (*s).to_string()).collect(),
            capture: true,
        };
        let run = |args: Vec<String>| Cmd {
            args,
            capture: false,
        };
        let own = |a: &str, b: &str, name: &str| run(vec![a.into(), b.into(), name.into()]);
        match self {
            Op::Refresh => vec![
                cap(&["info", "--json=v2", "--installed"]),
                cap(&["services", "list", "--json"]),
            ],
            Op::Update => vec![run(vec!["update".into()])],
            Op::UpgradeAll => vec![run(vec!["upgrade".into()])],
            Op::Upgrade { name, cask } => vec![own(
                "upgrade",
                if *cask { "--cask" } else { "--formula" },
                name,
            )],
            Op::Uninstall { name, cask } => vec![own(
                "uninstall",
                if *cask { "--cask" } else { "--formula" },
                name,
            )],
            Op::Pin { name, unpin } => vec![run(vec![
                if *unpin { "unpin" } else { "pin" }.into(),
                name.clone(),
            ])],
            Op::Service { name, act } => {
                vec![run(vec!["services".into(), act.arg().into(), name.clone()])]
            }
            Op::Cleanup => vec![run(vec!["cleanup".into()])],
            Op::Search { query } => vec![Cmd {
                args: vec!["search".into(), query.clone()],
                capture: true,
            }],
            Op::Install { name, cask } => {
                vec![own(
                    "install",
                    if *cask { "--cask" } else { "--formula" },
                    name,
                )]
            }
        }
    }
    pub fn shell(&self) -> String {
        self.commands()
            .into_iter()
            .map(|c| format!("brew {}", c.args.join(" ")))
            .collect::<Vec<_>>()
            .join(" && ")
    }
    /// 卸载、全部升级、清理会改本机软件，先在界面里确认。
    pub fn needs_confirm(&self) -> bool {
        matches!(
            self,
            Op::UpgradeAll | Op::Uninstall { .. } | Op::Cleanup | Op::Install { .. }
        )
    }
    /// 成功后重新读清单。刷新和搜索不再排队下一次。
    pub fn follow_with_refresh(&self) -> bool {
        !matches!(self, Op::Refresh | Op::Search { .. })
    }
}

/// `brew search` 的一条结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub name: String,
    pub cask: bool,
}

/// 解析 `brew search` 的分段文本。配方在前，应用在 `Casks` 段之后。
pub fn parse_search(text: &str) -> Vec<Hit> {
    let mut cask = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("Error") || t.starts_with("Warning") {
            continue;
        }
        let low = t.to_ascii_lowercase();
        if t.starts_with("==>") || t.ends_with(':') {
            if low.contains("cask") {
                cask = true;
            } else if low.contains("formula") {
                cask = false;
            }
            continue;
        }
        out.push(Hit {
            name: t.to_string(),
            cask,
        });
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub kind: Kind,
    pub name: String,
    pub cmd: String,
    pub title: String,
    pub desc: String,
    pub homepage: String,
    pub installed: String,
    pub latest: String,
    pub outdated: bool,
    pub pinned: bool,
    pub dependencies: Vec<String>,
    pub caveats: String,
    pub tap: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub status: String,
    pub user: String,
}

pub fn package_id(kind: Kind, name: &str) -> String {
    match kind {
        Kind::Formula => format!("formula:{name}"),
        Kind::Cask => format!("cask:{name}"),
    }
}
pub fn service_id(name: &str) -> String {
    format!("service:{name}")
}

/// 行上的版本：可更新时写成「当前 → 最新」。
pub fn version_line(installed: &str, latest: &str, outdated: bool) -> String {
    if outdated && !latest.is_empty() && latest != installed && !installed.is_empty() {
        format!("{installed} → {latest}")
    } else if !installed.is_empty() {
        installed.to_string()
    } else {
        latest.to_string()
    }
}

/// 第三方 tap 用带斜杠的全名，避免和核心库重名。
pub fn command_name(full_name: &str, name: &str) -> String {
    let full = full_name.trim();
    if full.contains('/') {
        full.to_string()
    } else if !name.is_empty() {
        name.to_string()
    } else {
        full.to_string()
    }
}

pub fn service_label_key(status: &str) -> &'static str {
    match status {
        "started" => "brew.svc_started",
        "none" | "stopped" => "brew.svc_none",
        "error" | "exited" => "brew.svc_error",
        _ => "brew.svc_other",
    }
}

/// `$HOMEBREW_PREFIX/bin/brew`，然后是 Apple 芯片和 Intel 的默认位置。
pub fn candidates(prefix: Option<&str>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let mut push = |p: PathBuf| {
        if !v.iter().any(|e| e == &p) {
            v.push(p);
        }
    };
    if let Some(p) = prefix.map(str::trim).filter(|s| !s.is_empty()) {
        push(PathBuf::from(p).join("bin").join("brew"));
    }
    push(PathBuf::from("/opt/homebrew/bin/brew"));
    push(PathBuf::from("/usr/local/bin/brew"));
    v
}

pub fn find_brew(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.is_file()).cloned()
}

/// 把 `brew` 所在目录放到子进程 PATH 前面。从 Dock 启动时经常没有这个目录。
pub fn child_path(brew: &Path, current: &str) -> String {
    let Some(bin) = brew.parent() else {
        return current.to_string();
    };
    let bin = bin.to_string_lossy().to_string();
    if bin.is_empty() {
        return current.to_string();
    }
    let sep = if cfg!(windows) { ';' } else { ':' };
    if current.split(sep).any(|p| p == bin) {
        return current.to_string();
    }
    if current.is_empty() {
        bin
    } else {
        format!("{bin}{sep}{current}")
    }
}

pub fn parse_installed(text: &str) -> Result<Vec<Package>, String> {
    let v: Value = serde_json::from_str(json_body(text)).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("formulae").and_then(|x| x.as_array()) {
        for f in arr {
            if let Some(p) = parse_formula(f) {
                out.push(p);
            }
        }
    }
    if let Some(arr) = v.get("casks").and_then(|x| x.as_array()) {
        for c in arr {
            if let Some(p) = parse_cask(c) {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.kind_ord().cmp(&b.kind_ord()))
    });
    Ok(out)
}

pub fn parse_services(text: &str) -> Result<Vec<Service>, String> {
    let v: Value = serde_json::from_str(json_body(text)).map_err(|e| e.to_string())?;
    let arr = v.as_array().ok_or_else(|| "services".to_string())?;
    let mut out: Vec<Service> = arr
        .iter()
        .filter_map(|s| {
            let name = s
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                return None;
            }
            Some(Service {
                status: text_of(s.get("status")),
                user: text_of(s.get("user")),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

impl Package {
    fn kind_ord(&self) -> u8 {
        match self.kind {
            Kind::Formula => 0,
            Kind::Cask => 1,
        }
    }
}

fn parse_formula(v: &Value) -> Option<Package> {
    let name = text_of(v.get("name"));
    if name.is_empty() {
        return None;
    }
    let full = text_of(v.get("full_name"));
    let cmd = command_name(&full, &name);
    let (installed, _) = formula_installed(v);
    Some(Package {
        kind: Kind::Formula,
        id: package_id(Kind::Formula, &name),
        title: name.clone(),
        cmd,
        name,
        desc: text_of(v.get("desc")),
        homepage: text_of(v.get("homepage")),
        latest: text_of(v.get("versions").and_then(|x| x.get("stable"))),
        outdated: v.get("outdated").and_then(|x| x.as_bool()).unwrap_or(false),
        pinned: pinned_of(v),
        dependencies: string_list(v.get("dependencies")),
        caveats: text_of(v.get("caveats")),
        tap: text_of(v.get("tap")),
        installed,
    })
}

fn parse_cask(v: &Value) -> Option<Package> {
    let name = text_of(v.get("token"));
    if name.is_empty() {
        return None;
    }
    let title = {
        let n = text_of(v.get("name"));
        if n.is_empty() {
            name.clone()
        } else {
            n
        }
    };
    Some(Package {
        kind: Kind::Cask,
        id: package_id(Kind::Cask, &name),
        cmd: name.clone(),
        title,
        name,
        desc: text_of(v.get("desc")),
        homepage: text_of(v.get("homepage")),
        installed: cask_installed(v.get("installed")),
        latest: text_of(v.get("version")),
        outdated: v.get("outdated").and_then(|x| x.as_bool()).unwrap_or(false),
        pinned: pinned_of(v),
        dependencies: cask_deps(v.get("depends_on")),
        caveats: text_of(v.get("caveats")),
        tap: text_of(v.get("tap")),
    })
}

fn formula_installed(v: &Value) -> (String, bool) {
    let Some(arr) = v.get("installed").and_then(|x| x.as_array()) else {
        return (String::new(), false);
    };
    let versions: Vec<String> = arr
        .iter()
        .map(|i| text_of(i.get("version")))
        .filter(|s| !s.is_empty())
        .collect();
    let requested = arr.iter().any(|i| {
        i.get("installed_on_request")
            .and_then(|x| x.as_bool())
            .unwrap_or(false)
    });
    (versions.join(", "), requested)
}

fn cask_installed(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| {
                x.as_str().map(|s| s.to_string()).or_else(|| {
                    x.get("version")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
            })
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

fn cask_deps(v: Option<&Value>) -> Vec<String> {
    let Some(d) = v else { return vec![] };
    let mut out = Vec::new();
    for key in ["formula", "cask"] {
        match d.get(key) {
            Some(Value::String(s)) if !s.is_empty() => out.push(s.clone()),
            Some(Value::Array(a)) => out.extend(string_list(Some(&Value::Array(a.clone())))),
            _ => {}
        }
    }
    out
}

fn pinned_of(v: &Value) -> bool {
    if v.get("pinned").and_then(|x| x.as_bool()).unwrap_or(false) {
        return true;
    }
    matches!(v.get("pinned_version"), Some(Value::String(s)) if !s.is_empty())
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn text_of(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .find(|s| !s.is_empty())
            .unwrap_or("")
            .to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `brew` 偶尔会在 JSON 前面打一行警告。从第一个 `{` 或 `[` 开始解析。
fn json_body(text: &str) -> &str {
    let t = text.trim();
    t.find(['{', '[']).map(|i| &t[i..]).unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLED: &str = r#"
Warning: tapped
{
  "formulae": [{
    "name": "wget",
    "full_name": "wget",
    "desc": "Internet file retriever",
    "homepage": "https://www.gnu.org/software/wget/",
    "outdated": true,
    "pinned": false,
    "tap": "homebrew/core",
    "versions": {"stable": "1.25.0"},
    "dependencies": ["openssl@3", "libidn2"],
    "caveats": "Note this",
    "installed": [
      {"version": "1.24.5", "installed_on_request": true},
      {"version": "1.21.0", "installed_on_request": false}
    ]
  }, {
    "name": "foo",
    "full_name": "user/tap/foo",
    "desc": "",
    "outdated": false,
    "pinned": true,
    "installed": [{"version": "1.0"}]
  }],
  "casks": [{
    "token": "firefox",
    "name": ["Firefox"],
    "desc": "Browser",
    "homepage": "https://www.mozilla.org/firefox/",
    "outdated": false,
    "pinned": false,
    "pinned_version": null,
    "tap": "homebrew/cask",
    "version": "130.0",
    "installed": "130.0",
    "caveats": null,
    "depends_on": {"formula": ["something"]}
  }, {
    "token": "pinned-app",
    "name": [],
    "outdated": true,
    "pinned_version": "1.2.3",
    "version": "2.0",
    "installed": ["1.2.3"]
  }]
}
"#;

    #[test]
    fn parses_formula_array_and_cask_string() {
        let pkgs = parse_installed(INSTALLED).unwrap();
        let wget = pkgs.iter().find(|p| p.name == "wget").unwrap();
        assert_eq!(wget.kind, Kind::Formula);
        assert_eq!(wget.installed, "1.24.5, 1.21.0");
        assert_eq!(wget.latest, "1.25.0");
        assert!(wget.outdated && !wget.pinned);
        assert_eq!(
            wget.dependencies,
            vec!["openssl@3".to_string(), "libidn2".into()]
        );
        assert_eq!(wget.caveats, "Note this");
        assert_eq!(wget.cmd, "wget");
        assert_eq!(
            version_line(&wget.installed, &wget.latest, true),
            "1.24.5, 1.21.0 → 1.25.0"
        );
        let foo = pkgs.iter().find(|p| p.name == "foo").unwrap();
        assert_eq!(foo.cmd, "user/tap/foo");
        assert!(foo.pinned);
        let fx = pkgs.iter().find(|p| p.name == "firefox").unwrap();
        assert_eq!(fx.kind, Kind::Cask);
        assert_eq!(fx.title, "Firefox");
        assert_eq!(fx.installed, "130.0");
        assert_eq!(fx.dependencies, vec!["something".to_string()]);
        assert!(!fx.pinned);
        let pin = pkgs.iter().find(|p| p.name == "pinned-app").unwrap();
        assert!(pin.pinned);
        assert_eq!(pin.installed, "1.2.3");
        assert_eq!(pin.title, "pinned-app");
    }

    #[test]
    fn parses_services_and_rejects_object() {
        let sv = parse_services(
            r#"[{"name":"mysql@8.4","status":"started","user":"Enigma","exit_code":null}]"#,
        )
        .unwrap();
        assert_eq!(sv.len(), 1);
        assert_eq!(sv[0].name, "mysql@8.4");
        assert_eq!(sv[0].status, "started");
        assert_eq!(service_label_key("started"), "brew.svc_started");
        assert_eq!(service_label_key("none"), "brew.svc_none");
        assert!(parse_services(r#"{"name":"x"}"#).is_err());
        assert!(parse_installed("not json").is_err());
        assert!(parse_installed("{}").unwrap().is_empty());
    }

    #[test]
    fn command_args_match_the_operation() {
        assert_eq!(
            Op::Refresh.commands()[0].args,
            vec!["info", "--json=v2", "--installed"]
        );
        assert!(Op::Refresh.commands()[0].capture);
        assert_eq!(
            Op::Refresh.commands()[1].args,
            vec!["services", "list", "--json"]
        );
        assert_eq!(
            Op::Upgrade {
                name: "wget".into(),
                cask: false
            }
            .commands()[0]
                .args,
            vec!["upgrade", "--formula", "wget"]
        );
        assert_eq!(
            Op::Upgrade {
                name: "firefox".into(),
                cask: true
            }
            .commands()[0]
                .args,
            vec!["upgrade", "--cask", "firefox"]
        );
        assert_eq!(
            Op::Uninstall {
                name: "wget".into(),
                cask: false
            }
            .commands()[0]
                .args,
            vec!["uninstall", "--formula", "wget"]
        );
        assert_eq!(
            Op::Pin {
                name: "wget".into(),
                unpin: false
            }
            .commands()[0]
                .args,
            vec!["pin", "wget"]
        );
        assert_eq!(
            Op::Pin {
                name: "wget".into(),
                unpin: true
            }
            .commands()[0]
                .args,
            vec!["unpin", "wget"]
        );
        assert_eq!(
            Op::Service {
                name: "mysql".into(),
                act: SvcAct::Restart
            }
            .commands()[0]
                .args,
            vec!["services", "restart", "mysql"]
        );
        assert_eq!(Op::Cleanup.commands()[0].args, ["cleanup"]);
        assert_eq!(Op::Update.shell(), "brew update");
        assert_eq!(Op::UpgradeAll.shell(), "brew upgrade");
        assert!(Op::Uninstall {
            name: "a".into(),
            cask: true
        }
        .needs_confirm());
        assert!(Op::UpgradeAll.needs_confirm() && Op::Cleanup.needs_confirm());
        assert!(!Op::Upgrade {
            name: "a".into(),
            cask: false
        }
        .needs_confirm());
        assert!(!Op::Refresh.follow_with_refresh());
        assert!(!Op::Search {
            query: "wget".into()
        }
        .follow_with_refresh());
        assert!(Op::Install {
            name: "wget".into(),
            cask: false
        }
        .needs_confirm());
        assert!(Op::Update.follow_with_refresh());
        let found = parse_search("==> Formulae\nwget\nwgetpaste\n\n==> Casks\nfirefox\n");
        assert_eq!(
            found,
            vec![
                Hit {
                    name: "wget".into(),
                    cask: false
                },
                Hit {
                    name: "wgetpaste".into(),
                    cask: false
                },
                Hit {
                    name: "firefox".into(),
                    cask: true
                },
            ]
        );
        assert_eq!(command_name("wget", "wget"), "wget");
        assert_eq!(command_name("user/tap/foo", "foo"), "user/tap/foo");
    }

    #[test]
    fn brew_path_order_and_path_prefix() {
        let c = candidates(Some("/opt/homebrew"));
        assert_eq!(
            c,
            vec![
                PathBuf::from("/opt/homebrew/bin/brew"),
                PathBuf::from("/usr/local/bin/brew")
            ]
        );
        let custom = candidates(Some("/custom"));
        assert_eq!(custom[0], PathBuf::from("/custom/bin/brew"));
        assert_eq!(custom[1], PathBuf::from("/opt/homebrew/bin/brew"));
        let dir = std::env::temp_dir().join(format!("shellax-brew-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::write(&b, "").unwrap();
        assert_eq!(find_brew(&[a.clone(), b.clone()]), Some(b.clone()));
        std::fs::write(&a, "").unwrap();
        assert_eq!(find_brew(&[a.clone(), b]), Some(a));
        let _ = std::fs::remove_dir_all(&dir);
        let p = child_path(Path::new("/opt/homebrew/bin/brew"), "/usr/bin");
        assert!(p.starts_with("/opt/homebrew/bin"), "{p}");
        assert!(p.contains("/usr/bin"), "{p}");
        assert_eq!(child_path(Path::new("/opt/homebrew/bin/brew"), &p), p);
        assert_eq!(ENV.len(), 5);
        assert!(INSTALL_CMD.contains("brew.sh") || INSTALL_CMD.contains("Homebrew/install"));
    }
}
