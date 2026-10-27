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

/// 若 extensions/ 已构建出 wasm，则在宿主运行时里实际加载并跑一遍（未构建时跳过）。
#[test]
fn built_packs_run_in_host() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/target/wasm32-unknown-unknown/release");
    let load = |n: &str| std::fs::read(dir.join(format!("ext_{n}.wasm"))).ok();
    let run = |w: &[u8], tool: &str, kv: &[(&str, &str)]| {
        let mut a = Args::default();
        for (k, v) in kv { a.values.insert(k.to_string(), v.to_string()); }
        run_tool(w, tool, &a)
    };
    for (n, id) in [("lang_zh", "lang-zh"), ("worldtime", "worldtime"), ("image", "image"), ("pdf", "pdf"), ("pdf_render", "pdf-render")] {
        let Some(w) = load(n) else { continue };
        let m = manifest_of(&w).unwrap();
        assert_eq!(m.id, id);
        let known: Vec<&str> = KNOWN.iter().find(|k| k.0 == id).map(|k| k.2.to_vec()).unwrap_or_default();
        for t in &m.tools { assert!(known.contains(&t.id.as_str()), "{id}: {} not in KNOWN", t.id); }
    }
    if let Some(w) = load("lang_zh") {
        assert!(!run(&w, "pinyin", &[("input", "中文")]).unwrap().text.is_empty());
    }
    if let Some(w) = load("image") {
        let o = run(&w, "qr_gen", &[("input", "hello")]).unwrap();
        assert!(o.file.is_some());
    }
    // 中文水印：在 wasm 里完成字体子集化与 CID 嵌入。
    if let (Some(pdfw), Some(imgw)) = (load("pdf"), load("image")) {
        let png = run(&imgw, "qr_gen", &[("input", "x")]).unwrap().file.unwrap().1;
        let tmp = std::env::temp_dir().join(format!("shellax-ext-{}.png", std::process::id()));
        std::fs::write(&tmp, &png).unwrap();
        let mut a = Args::default();
        a.files.insert("files".into(), vec![tmp.clone()]);
        let pdf = run_tool(&pdfw, "img_to_pdf", &a).unwrap().file.unwrap().1;
        let tmp2 = tmp.with_extension("pdf");
        std::fs::write(&tmp2, &pdf).unwrap();
        let mut a = Args::default();
        a.files.insert("file".into(), vec![tmp2.clone()]);
        a.values.insert("text".into(), "内部资料".into());
        let out = run_tool(&pdfw, "pdf_watermark", &a).unwrap().file.unwrap().1;
        assert!(out.windows(12).any(|w| w == b"CIDFontType2"));
        let mut a = Args::default();
        a.files.insert("file".into(), vec![tmp.clone()]);
        a.values.insert("text".into(), "内部资料".into());
        assert!(run_tool(&imgw, "img_watermark", &a).unwrap().file.is_some());
        let _ = (std::fs::remove_file(tmp), std::fs::remove_file(tmp2));
    }
}

#[test]
fn reply_old_and_rows() {
    // 旧包的回复没有 rows / meta，仍是纯文本结果
    let old: RunReply = serde_json::from_str(r#"{"text":"3 页","file_name":"a.pdf"}"#).unwrap();
    let o = output_of(old, b"%PDF-1.4", 10);
    assert_eq!((o.kind, o.text.as_str()), (crate::tools::OutKind::Text, "3 页"));
    assert_eq!(o.file.as_ref().map(|f| f.0.as_str()), Some("a.pdf"));
    assert_eq!(o.info.as_ref().map(|i| i.kind), Some(crate::tools::fileinfo::FileKind::Pdf));
    assert_eq!(o.in_size, Some(10));
    // 新包带 rows：按键值行显示
    let new: RunReply = serde_json::from_str(r#"{"text":"","meta":"m","rows":[{"label":"a","value":"1","primary":true,"tone":"good","tag":"x"},{"value":"2"}]}"#).unwrap();
    let o = output_of(new, &[], 0);
    assert_eq!(o.kind, crate::tools::OutKind::Kv);
    assert_eq!(o.rows.len(), 2);
    assert!(o.rows[0].primary && o.rows[0].tone == Some(Tone::Good) && o.rows[0].tag.is_some());
    assert_eq!(o.text, "a: 1\n2");
    assert_eq!(o.meta, "m");
    assert!(o.in_size.is_none() && o.info.is_none());
}
