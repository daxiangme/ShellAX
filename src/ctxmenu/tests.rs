use super::*;

fn sh_roundtrip(q: &str) -> String {
    // let a real POSIX shell parse the quoted word back
    let o = std::process::Command::new("sh").arg("-c").arg(format!("printf '%s' {q}")).output().unwrap();
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn quoting_spaces_quotes_cjk_unicode() {
    assert_eq!(quote("plain.txt"), "plain.txt");
    assert_eq!(quote("/a/b-c_d.tar.gz"), "/a/b-c_d.tar.gz");
    assert_eq!(quote("my file.zip"), "'my file.zip'");
    assert_eq!(quote("it's"), r"'it'\''s'");
    assert_eq!(quote(""), "''");
    assert_eq!(quote("-rf"), "'-rf'");
    for s in ["my file.zip", "it's \"quoted\"", "报告 2026.tar.gz", "emoji 🚀.txt", "$(rm -rf ~)", "a`b`c", "tab\there", "new\nline", "ends\\", "'", "日本語/ファイル", "ü ñ é"] {
        assert_eq!(sh_roundtrip(&quote(s)), s, "roundtrip {s:?}");
    }
    assert_eq!(quote_path("~/my dir/a.zip"), "~/'my dir/a.zip'");
    assert_eq!(quote_path("~/x"), "~/x");
    assert_eq!(quote_path("-weird"), "./-weird");
    assert_eq!(quote_path("/tmp/中文 目录"), "'/tmp/中文 目录'");
}

#[test]
fn type_detection() {
    assert_eq!(kinds("a.zip", false, 0o644), vec![Kind::Archive(ArcKind::Zip)]);
    assert_eq!(kinds("A.TAR.GZ", false, 0)[0], Kind::Archive(ArcKind::TarGz));
    assert_eq!(kinds("x.tgz", false, 0)[0], Kind::Archive(ArcKind::TarGz));
    assert_eq!(kinds("x.tar.xz", false, 0)[0], Kind::Archive(ArcKind::TarXz));
    assert_eq!(kinds("x.tar.bz2", false, 0)[0], Kind::Archive(ArcKind::TarBz2));
    assert_eq!(kinds("x.tar", false, 0)[0], Kind::Archive(ArcKind::Tar));
    assert_eq!(kinds("x.7z", false, 0)[0], Kind::Archive(ArcKind::SevenZ));
    assert_eq!(kinds("x.rar", false, 0)[0], Kind::Archive(ArcKind::Rar));
    assert_eq!(kinds("x.gz", false, 0)[0], Kind::Archive(ArcKind::Gz));
    assert_eq!(kinds(".zip", false, 0), vec![Kind::File]);
    assert_eq!(kinds("src", true, 0o755), vec![Kind::Dir]);
    assert_eq!(kinds("app.log", false, 0o644), vec![Kind::Log, Kind::Text]);
    assert_eq!(kinds("app.log.3", false, 0)[0], Kind::Log);
    assert_eq!(kinds("deploy.sh", false, 0o644), vec![Kind::Script, Kind::Text]);
    assert_eq!(kinds("deploy.sh", false, 0o755), vec![Kind::Script, Kind::Executable, Kind::Text]);
    assert_eq!(kinds("server", false, 0o755), vec![Kind::Executable]);
    assert_eq!(kinds("nginx.conf", false, 0o644), vec![Kind::Text]);
    assert_eq!(kinds("配置.yaml", false, 0), vec![Kind::Text]);
    assert_eq!(kinds("Makefile", false, 0), vec![Kind::Text]);
    assert_eq!(kinds("photo.png", false, 0o644), vec![Kind::File]);
    assert_eq!(ext_of("a.tar.gz"), "tar.gz");
    assert_eq!(stem_of("a.tar.gz"), "a");
    assert_eq!(stem_of("my.backup.zip"), "my.backup");
    assert_eq!(stem_of(".bashrc"), ".bashrc");
    assert_eq!(ext_of("noext"), "");
}

fn path_at(line: &str, needle: &str) -> Option<Found> {
    let col = line[..line.find(needle).unwrap()].chars().count() + 1;
    detect_at(line, col).map(|d| d.found)
}

#[test]
fn detection_in_terminal_output() {
    let p = |s: &str| Some(Found::Path { path: s.into(), line: None });
    assert_eq!(path_at("error: /var/log/syslog: permission denied", "/var"), p("/var/log/syslog"));
    assert_eq!(path_at("  --> src/main.rs:42:5", "src"), Some(Found::Path { path: "src/main.rs".into(), line: Some(42) }));
    assert_eq!(path_at("drwxr-xr-x 2 u u 4096 docs/", "docs"), p("docs/"));
    assert_eq!(path_at("see ./build/out.tar.gz.", "./build"), p("./build/out.tar.gz"));
    assert_eq!(path_at("cd ../up/dir", "../up"), p("../up/dir"));
    assert_eq!(path_at("saved ~/下载/报告.zip ok", "~/"), p("~/下载/报告.zip"));
    assert_eq!(path_at("-rw-r--r-- 1 u u 10 Oct 13 build.log", "build"), p("build.log"));
    assert_eq!(path_at("配置.yaml 已更新", "配置"), p("配置.yaml"));
    assert_eq!(path_at("main.rs:17 warning", "main"), Some(Found::Path { path: "main.rs".into(), line: Some(17) }));
    assert_eq!(path_at("visit https://example.com/a?b=1 now", "https"), Some(Found::Url("https://example.com/a?b=1".into())));
    assert_eq!(path_at("(see https://x.io/p).", "https"), Some(Found::Url("https://x.io/p".into())));
    assert_eq!(path_at("listening on 10.0.0.5:8080", "10.0"), Some(Found::HostPort { host: "10.0.0.5".into(), port: Some(8080) }));
    assert_eq!(path_at("ping 192.168.1.1 ok", "192"), Some(Found::HostPort { host: "192.168.1.1".into(), port: None }));
    assert_eq!(path_at("bind [::1]:22 done", "[::1]"), Some(Found::HostPort { host: "::1".into(), port: Some(22) }));
    assert_eq!(path_at("dev server localhost:3000", "localhost"), Some(Found::HostPort { host: "localhost".into(), port: Some(3000) }));
    // version numbers are neither IPs nor files
    assert_eq!(path_at("version 1.2.3.4.5 here", "1.2"), None);
    assert_eq!(path_at("took 1.25 seconds", "1.25"), None);
    assert_eq!(detect_at("hello world", 2), None);
    // column outside the match
    assert_eq!(detect_at("ab /tmp/x", 0), None);
}

#[test]
fn resolve_relative_with_cwd() {
    assert_eq!(resolve("build.log", Some("/home/u/proj"), None), ("/home/u/proj/build.log".into(), true));
    assert_eq!(resolve("../x/./y", Some("/home/u/proj"), None), ("/home/u/x/y".into(), true));
    assert_eq!(resolve("~/a", None, Some("/home/u")), ("/home/u/a".into(), true));
    assert_eq!(resolve("/etc/../etc/hosts", None, None), ("/etc/hosts".into(), true));
    assert_eq!(resolve("rel/a.txt", None, None), ("rel/a.txt".into(), false));
    assert_eq!(normalize("/../.."), "/");
    assert_eq!(prompt_cwd("sshtest@box:~/work dir$ "), Some("~/work dir".into()));
    assert_eq!(prompt_cwd("root@srv:/var/log# ls"), Some("/var/log".into()));
    assert_eq!(prompt_cwd("plain text"), None);
    assert_eq!(resolve("a.zip", Some("~/w"), None), ("~/w/a.zip".into(), true));
    assert_eq!(parse_osc7("file://host/home/u/my%20dir"), Some("/home/u/my dir".into()));
    assert_eq!(parse_osc7("file://h/tmp/%E4%B8%AD"), Some("/tmp/中".into()));
    assert_eq!(parse_osc7("http://x"), None);
}

#[test]
fn template_expansion() {
    let v = vars_for(&["/srv/my app/data.tar.gz".to_string()], "prod-1");
    assert_eq!(expand("tar -tzf {path}", &v).unwrap(), "tar -tzf '/srv/my app/data.tar.gz'");
    assert_eq!(expand("{dir}|{name}|{stem}|{ext}|{host}", &v).unwrap(), "'/srv/my app'|data.tar.gz|data|tar.gz|prod-1");
    assert_eq!(expand("echo {{literal}} {name}", &v).unwrap(), "echo {literal} data.tar.gz");
    assert!(expand("x {nope}", &v).is_err());
    assert!(expand("x {path", &v).is_err());
    let v = vars_for(&["/tmp/it's 报告.md".to_string(), "/tmp/b.md".to_string()], "local");
    let out = expand("cat {path}", &v).unwrap();
    assert_eq!(out, r"cat '/tmp/it'\''s 报告.md' /tmp/b.md");
    assert_eq!(placeholders("a {path} {{x}} {host}").unwrap(), vec!["path", "host"]);
}

#[test]
fn tool_probe_parsing_and_hints() {
    let t = parse_probe("T:tar\nT:gzip\nT:7za\nU:Linux\nO:ubuntu\nO:debian\n");
    assert!(t.has("tar") && t.has("7z") && !t.has("unrar"));
    assert_eq!(t.pm, Pm::Apt);
    assert_eq!(t.os, "ubuntu");
    assert_eq!(parse_probe("U:Darwin\n").pm, Pm::Brew);
    assert_eq!(parse_probe("U:Linux\nO:rocky\nO:rhel centos fedora\n").pm, Pm::Yum);
    assert_eq!(parse_probe("O:fedora\n").pm, Pm::Dnf);
    assert_eq!(parse_probe("O:alpine\n").pm, Pm::Apk);
    assert_eq!(parse_probe("O:arch\n").pm, Pm::Pacman);
    assert_eq!(parse_probe("O:opensuse-leap\n").pm, Pm::Zypper);
    assert_eq!(parse_probe("U:FreeBSD\n").pm, Pm::Unknown);
    assert_eq!(install_hint("unrar", Pm::Apt), "sudo apt install -y unrar");
    assert_eq!(install_hint("7z", Pm::Apt), "sudo apt install -y p7zip-full");
    assert_eq!(install_hint("7z", Pm::Yum), "sudo yum install -y p7zip p7zip-plugins");
    assert_eq!(install_hint("7z", Pm::Brew), "brew install sevenzip");
    assert_eq!(install_hint("xz", Pm::Apt), "sudo apt install -y xz-utils");
    assert_eq!(install_hint("unzip", Pm::Apk), "sudo apk add unzip");
    assert!(install_hint("unzip", Pm::Unknown).contains("unzip"));
    // the probe script runs in a real shell and finds sh-level tools here
    let local = probe_local();
    assert!(local.has("tar") && local.has("bash"), "{local:?}");
    // cache
    clear_tool_cache();
    assert!(begin_probe("h1"));
    assert!(!begin_probe("h1"));
    assert_eq!(cached_tools("h1"), None);
    store_tools("h1", t.clone());
    assert_eq!(cached_tools("h1"), Some(t));
}

#[test]
fn toml_parsing_and_validation() {
    let (items, errs) = parse_custom(CUSTOM_EXAMPLE);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].extensions, vec!["json"]);
    let src = r#"
[[item]]
label = "ok"
command = "md5sum {path}"
types = ["file"]
[[item]]
label = ""
command = "x"
[[item]]
label = "bad ph"
command = "echo {nope}"
[[item]]
label = "bad type"
command = "echo"
types = ["blob"]
[[item]]
label = "unknown field"
command = "echo"
colour = "red"
[[item]]
label = "unterminated"
command = "echo {path"
"#;
    let (items, errs) = parse_custom(src);
    assert_eq!(items.len(), 1, "{errs:?}");
    assert_eq!(errs.len(), 5, "{errs:?}");
    let (items, errs) = parse_custom("[[item]\nlabel=");
    assert!(items.is_empty() && errs.len() == 1);
    let (_, errs) = parse_custom("foo = 1\n");
    assert_eq!(errs.len(), 1);
}

fn ctx<'a>(place: Place, tools: Option<&'a Tools>, custom: &'a [CustomItem]) -> Ctx<'a> { Ctx { host: "web-1", place, tools, custom, lang_en: false } }
fn find<'a>(m: &'a Menu, id: &str) -> &'a MenuItem { m.items.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("no {id} in {:?}", m.items.iter().map(|i| &i.id).collect::<Vec<_>>())) }

#[test]
fn menus_by_type_with_previews_and_missing_tools() {
    let tools = parse_probe("T:tar\nT:gzip\nT:unzip\nT:less\nT:tail\nT:du\nT:grep\nT:diff\nT:bash\nU:Linux\nO:debian\n");
    let file = |p: &str, d: bool, m: u32| Target::Files(vec![FileObj { path: p.into(), is_dir: d, mode: m }]);
    let m = build(&file("/srv/my files/a b.zip", false, 0o644), &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "extract_here").act, Act::Insert("unzip -o '/srv/my files/a b.zip' -d '/srv/my files'".into()));
    assert_eq!(find(&m, "extract_to").act, Act::Insert("mkdir -p '/srv/my files/a b' && unzip -o '/srv/my files/a b.zip' -d '/srv/my files/a b'".into()));
    assert_eq!(find(&m, "list").act, Act::Insert("unzip -l '/srv/my files/a b.zip'".into()));
    assert!(find(&m, "extract_here").enabled());
    assert_eq!(find(&m, "download").act, Act::Download(vec!["/srv/my files/a b.zip".into()]));
    assert_eq!(find(&m, "copy_path").act, Act::Copy("/srv/my files/a b.zip".into()));
    assert_eq!(find(&m, "rm").act, Act::Insert("rm -- '/srv/my files/a b.zip'".into()));
    // rar without unrar → greyed out with apt hint
    let m = build(&file("/x/y.rar", false, 0o644), &ctx(Place::RemotePanel, Some(&tools), &[]));
    let it = find(&m, "extract_here");
    assert!(!it.enabled());
    assert_eq!(it.missing, vec!["unrar"]);
    assert_eq!(it.hint.as_deref(), Some("sudo apt install -y unrar"));
    // unknown tools (probe in flight) → enabled, marked unknown
    let m = build(&file("/x/y.7z", false, 0o644), &ctx(Place::RemotePanel, None, &[]));
    assert!(find(&m, "extract_here").enabled() && find(&m, "extract_here").unknown);
    assert_eq!(find(&m, "extract_here").act, Act::Insert("7z x -y /x/y.7z -o/x".into()));
    // tar variants
    let m = build(&file("/d/a.tar.xz", false, 0), &ctx(Place::LocalPanel, Some(&tools), &[]));
    assert_eq!(find(&m, "extract_here").act, Act::Insert("tar -xJf /d/a.tar.xz -C /d".into()));
    assert_eq!(find(&m, "extract_here").missing, vec!["xz"]);
    assert_eq!(find(&m, "upload").act, Act::Upload(vec!["/d/a.tar.xz".into()]));
    let m = build(&file("/d/a.gz", false, 0), &ctx(Place::LocalPanel, Some(&tools), &[]));
    assert_eq!(find(&m, "extract_here").act, Act::Insert("gunzip -k /d/a.gz".into()));
    // directory
    let m = build(&file("/home/u/项目 A", true, 0o755), &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "targz_dir").act, Act::Insert("tar -czf '/home/u/项目 A.tar.gz' -C /home/u '项目 A'".into()));
    assert_eq!(find(&m, "zip_dir").act, Act::Insert("cd /home/u && zip -qr '项目 A.zip' '项目 A'".into()));
    assert!(!find(&m, "zip_dir").enabled());
    assert_eq!(find(&m, "du").act, Act::Insert("du -sh -- '/home/u/项目 A'".into()));
    assert_eq!(find(&m, "cd_tab").act, Act::CdNewTab("/home/u/项目 A".into()));
    assert_eq!(find(&m, "rm").act, Act::Insert("rm -r -- '/home/u/项目 A'".into()));
    // text: remote → built-in editor; local → $EDITOR
    let m = build(&file("/etc/nginx/nginx.conf", false, 0o644), &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "edit").act, Act::OpenEditor("/etc/nginx/nginx.conf".into()));
    assert_eq!(find(&m, "tail_f").act, Act::Insert("tail -f -- /etc/nginx/nginx.conf".into()));
    assert_eq!(find(&m, "less").act, Act::Insert("less -- /etc/nginx/nginx.conf".into()));
    let m = build(&file("/tmp/a.md", false, 0o644), &ctx(Place::Terminal { remote: false }, Some(&tools), &[]));
    assert!(m.items.iter().all(|i| i.id != "edit"));
    // script / log
    let m = build(&file("/opt/run me.sh", false, 0o644), &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "run").act, Act::Insert("bash '/opt/run me.sh'".into()));
    assert_eq!(find(&m, "chmod_x").act, Act::Insert("chmod +x -- '/opt/run me.sh'".into()));
    let m = build(&file("/opt/x.py", false, 0o644), &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "run").missing, vec!["python3"]);
    let m = build(&file("/var/log/app.log", false, 0o644), &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "grep").act, Act::Insert("grep -n -- 'PATTERN' /var/log/app.log".into()));
    let m = build(&file("/usr/local/bin/srv", false, 0o755), &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "run_exec").act, Act::Insert("/usr/local/bin/srv".into()));
    // multi-select
    let multi = Target::Files(vec![FileObj { path: "/d/a.txt".into(), is_dir: false, mode: 0 }, FileObj { path: "/d/b c.txt".into(), is_dir: false, mode: 0 }]);
    let m = build(&multi, &ctx(Place::RemotePanel, Some(&tools), &[]));
    assert_eq!(find(&m, "diff2").act, Act::Insert("diff -u -- /d/a.txt '/d/b c.txt'".into()));
    assert_eq!(find(&m, "rm").act, Act::Insert("rm -- /d/a.txt '/d/b c.txt'".into()));
    assert_eq!(find(&m, "targz_sel").act, Act::Insert("tar -czf /d/archive.tar.gz -C /d a.txt 'b c.txt'".into()));
    assert!(m.items.iter().all(|i| i.id != "rename" && i.id != "extract_here"));
    // url / ip / selection
    let m = build(&Target::Url("https://ex.com/a b".into()), &ctx(Place::Terminal { remote: true }, Some(&tools), &[]));
    assert_eq!(find(&m, "curl_head").act, Act::Insert("curl -sSI -- 'https://ex.com/a b'".into()));
    assert!(!find(&m, "curl_head").enabled());
    let m = build(&Target::HostPort { host: "10.0.0.5".into(), port: Some(8080) }, &ctx(Place::Terminal { remote: true }, Some(&tools), &[]));
    assert_eq!(find(&m, "nc").act, Act::Insert("nc -zv 10.0.0.5 8080".into()));
    assert_eq!(find(&m, "copy").act, Act::Copy("10.0.0.5:8080".into()));
    let m = build(&Target::Selection("ls -la".into()), &ctx(Place::Terminal { remote: true }, Some(&tools), &[]));
    for id in ["copy", "search", "ai_explain", "paste_input"] { find(&m, id); }
    // no insert command ever contains a newline
    for it in &m.items { if let Act::Insert(c) = &it.act { assert!(!c.contains('\n')); } }
}

#[test]
fn custom_items_match_and_expand() {
    let (custom, errs) = parse_custom(r#"
[[item]]
label = "用 jq 美化"
label_en = "jq pretty"
command = "jq . {path} | less"
extensions = ["JSON"]
tools = ["jq"]
[[item]]
label = "备份"
command = "cp -- {path} {dir}/{stem}.bak.{ext}"
glob = ["*.conf"]
hosts = ["web-*"]
[[item]]
label = "只在 db"
command = "echo {host}"
hosts = ["db-*"]
[[item]]
label = "whois"
command = "whois {path}"
types = ["ip"]
"#);
    assert!(errs.is_empty(), "{errs:?}");
    let tools = parse_probe("T:less\nO:debian\n");
    let file = |p: &str| Target::Files(vec![FileObj { path: p.into(), is_dir: false, mode: 0o644 }]);
    let m = build(&file("/data/x y.json"), &ctx(Place::RemotePanel, Some(&tools), &custom));
    let c: Vec<_> = m.items.iter().filter(|i| i.custom).collect();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].label, "用 jq 美化");
    assert_eq!(c[0].act, Act::Insert("jq . '/data/x y.json' | less".into()));
    assert_eq!(c[0].missing, vec!["jq"]);
    assert_eq!(c[0].hint.as_deref(), Some("sudo apt install -y jq"));
    let m = build(&file("/etc/app.conf"), &ctx(Place::RemotePanel, Some(&tools), &custom));
    let c: Vec<_> = m.items.iter().filter(|i| i.custom).collect();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].act, Act::Insert("cp -- /etc/app.conf /etc/app.bak.conf".into()));
    let mut cx = ctx(Place::RemotePanel, Some(&tools), &custom);
    cx.host = "local";
    assert!(build(&file("/etc/app.conf"), &cx).items.iter().all(|i| !i.custom));
    cx.lang_en = true; cx.host = "web-1";
    assert_eq!(build(&file("/a.json"), &cx).items.iter().find(|i| i.custom).unwrap().label, "jq pretty");
    let m = build(&Target::HostPort { host: "1.2.3.4".into(), port: None }, &ctx(Place::Terminal { remote: false }, Some(&tools), &custom));
    assert_eq!(m.items.iter().find(|i| i.custom).unwrap().act, Act::Insert("whois 1.2.3.4".into()));
}

#[test]
fn hot_reload_by_mtime() {
    let dir = std::env::temp_dir().join(format!("cm-hot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("context_menu.toml");
    std::env::set_var("SHELLAX_CONTEXT_MENU", &f);
    assert!(load_custom().0.is_empty());
    std::fs::write(&f, CUSTOM_EXAMPLE).unwrap();
    assert_eq!(load_custom().0.len(), 1);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&f, format!("{CUSTOM_EXAMPLE}\n[[item]]\nlabel = \"b\"\ncommand = \"echo {{name}}\"\n")).unwrap();
    assert_eq!(load_custom().0.len(), 2);
    std::env::remove_var("SHELLAX_CONTEXT_MENU");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn insert_bytes_never_submit() {
    let b = insert_bytes("echo a\nrm -rf /\r");
    assert_eq!(b[0], 0x15);
    assert!(!b.contains(&b'\n') && !b.contains(&b'\r'));
}
