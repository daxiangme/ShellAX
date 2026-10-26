use super::*;

const MANIFEST: &str = r#"{"abi":1,"id":"demo","name":"Demo","version":"1.0.0","tools":[{"id":"echo","cat":"text","name":"Echo","inputs":[{"id":"file","kind":"file"}]}]}"#;
const REPLY: &str = r#"{"text":"ok","file_name":"out.bin"}"#;


/// 手写的测试模块：请求 JSON 很短时返回清单，否则返回固定回复并把附加字节原样带回。
fn demo() -> Vec<u8> {
    let m = frame(MANIFEST.as_bytes(), &[]);
    let r = frame(REPLY.as_bytes(), &[]);
    let src = format!(r#"(module
      (memory (export "memory") 2)
      (global $top (mut i32) (i32.const 16384))
      (data (i32.const 1024) "{m}")
      (data (i32.const 8192) "{r}")
      (func (export "shellax_alloc") (param $n i32) (result i32)
        (local $p i32) (local.set $p (global.get $top))
        (global.set $top (i32.add (global.get $top) (local.get $n))) (local.get $p))
      (func (export "shellax_run") (param $p i32) (param $n i32) (result i64)
        (local $j i32) (local $rest i32) (local $out i32)
        (local.set $j (i32.load (local.get $p)))
        (if (i32.lt_u (local.get $j) (i32.const 40))
          (then (return (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {ml})))))
        (local.set $rest (i32.sub (local.get $n) (i32.add (local.get $j) (i32.const 4))))
        (local.set $out (global.get $top))
        (memory.copy (local.get $out) (i32.const 8192) (i32.const {rl}))
        (memory.copy (i32.add (local.get $out) (i32.const {rl})) (i32.add (i32.add (local.get $p) (i32.const 4)) (local.get $j)) (local.get $rest))
        (i64.or (i64.shl (i64.extend_i32_u (local.get $out)) (i64.const 32)) (i64.extend_i32_u (i32.add (local.get $rest) (i32.const {rl}))))))"#,
        m = m.iter().map(|b| format!("\\{b:02x}")).collect::<String>(), ml = m.len(),
        r = r.iter().map(|b| format!("\\{b:02x}")).collect::<String>(), rl = r.len());
    wat::parse_str(src).unwrap()
}

#[test]
fn frame_roundtrip() {
    let f = frame(b"{}", b"abc");
    assert_eq!(unframe(&f).unwrap(), (&b"{}"[..], &b"abc"[..]));
    assert!(unframe(&[1, 0]).is_err());
    assert!(unframe(&[9, 0, 0, 0, b'{']).is_err());
}

#[test]
fn manifest_and_run() {
    let w = demo();
    let m = manifest_of(&w).unwrap();
    assert_eq!(m.id, "demo");
    assert_eq!(m.tools[0].pack, "demo");
    assert!(m.tools[0].heavy);
    let dir = std::env::temp_dir().join(format!("shellax-ext-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("in.bin");
    std::fs::write(&f, b"\x00\x01binary payload").unwrap();
    let mut a = Args::default();
    a.set("mode", "x".repeat(50));
    a.files.insert("file".into(), vec![f]);
    let out = run_tool(&w, "echo", &a).unwrap();
    assert_eq!(out.text, "ok");
    assert_eq!(out.file, Some(("out.bin".into(), b"\x00\x01binary payload".to_vec())));
    // 安装到临时目录后能被扫描到
    let m2 = install_bytes(&dir, &w).unwrap();
    assert_eq!(m2, m);
    let s = scan(&dir);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].manifest.id, "demo");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rejects_imports_and_runaway() {
    let imp = wat::parse_str(r#"(module (import "env" "f" (func)) (memory (export "memory") 1))"#).unwrap();
    assert!(call(&imp, b"").unwrap_err().to_string().contains("import"));
    let spin = wat::parse_str(r#"(module (memory (export "memory") 1)
        (func (export "shellax_alloc") (param i32) (result i32) (i32.const 0))
        (func (export "shellax_run") (param i32 i32) (result i64) (loop $l (br $l)) (i64.const 0)))"#).unwrap();
    assert!(call_with(&spin, b"xxxx", 1_000_000).is_err());
    let big = wat::parse_str(r#"(module (memory (export "memory") 1)
        (func (export "shellax_alloc") (param i32) (result i32) (i32.const 0))
        (func (export "shellax_run") (param i32 i32) (result i64) (drop (memory.grow (i32.const 20000))) (i64.const 0x7fffffff)))"#).unwrap();
    assert!(call(&big, b"xxxx").is_err(), "memory beyond limit must not be readable");
}

#[test]
fn rows_merge() {
    let idx = Index { packs: vec![IndexPack { id: "pdf".into(), version: "1.2.0".into(), url: "u".into(), ..Default::default() }, IndexPack { id: "new".into(), version: "0.1.0".into(), url: "u".into(), ..Default::default() }] };
    let inst = vec![Installed { manifest: PackManifest { abi: 1, id: "pdf".into(), name: "PDF".into(), version: "1.1.0".into(), description: String::new(), tools: vec![] }, wasm: PathBuf::new() }];
    let r = rows(Some(&idx), &inst);
    assert_eq!(r.len(), KNOWN.len() + 1);
    let pdf = r.iter().find(|x| x.id == "pdf").unwrap();
    assert!(pdf.has_update());
    assert!(!r.iter().find(|x| x.id == "image").unwrap().has_update());
    let ph = placeholders(&inst);
    assert!(ph.iter().all(|t| t.pack != "pdf"));
    assert!(ph.iter().any(|t| t.id == "qr_gen" && t.pack == "image"));
}

#[test]
fn locale_keys() {
    let en = include_str!("../../locales/en.toml");
    let zh = include_str!("../../locales/zh-CN.toml");
    let mut keys: Vec<String> = KNOWN.iter().map(|k| format!("ext.{}", k.0)).collect();
    for (_, _, tools) in KNOWN { for t in *tools { keys.push(format!("tools.{t}")); keys.push(format!("tools.{t}_desc")); } }
    let src = concat!(include_str!("mod.rs"), include_str!("../ext_ui.rs"));
    for part in src.split("\"ext.").skip(1) {
        let Some(end) = part.find('"') else { continue };
        let k = &part[..end];
        if !k.is_empty() && !k.contains(['{', ' ', '<']) { keys.push(format!("ext.{k}")); }
    }
    let missing: Vec<_> = keys.iter().filter(|k| !en.contains(&format!("\"{k}\" =")) || !zh.contains(&format!("\"{k}\" ="))).collect();
    assert!(missing.is_empty(), "missing: {missing:?}");
}
