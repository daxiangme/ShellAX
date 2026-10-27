//! 扩展包开发辅助：处理宿主 ABI（版本 1）、请求解析、回复打包、工具描述与简单的 zip 打包。
//!
//! 宿主调用 `shellax_alloc(len)` 拿到缓冲区，写入 `[u32 json_len][json][文件字节]`，
//! 再调用 `shellax_run(ptr, len)`，得到 `(ptr << 32) | len` 指向的同格式回复。
use std::collections::BTreeMap;

use serde_json::{json, Value};

pub use serde_json;

pub const ABI: u32 = 1;

pub struct File { pub field: String, pub name: String, pub data: Vec<u8> }

pub struct Request {
    pub op: String,
    pub lang: String,
    pub tool: String,
    pub args: BTreeMap<String, String>,
    pub files: Vec<File>,
    /// 宿主当前时间（Unix 毫秒）与本地时区偏移（秒）。
    pub now_ms: i64,
    pub utc_offset: i32,
}

impl Request {
    pub fn zh(&self) -> bool { self.lang.starts_with("zh") }
    /// 按语言选文案。
    pub fn tr<'a>(&self, zh: &'a str, en: &'a str) -> &'a str { if self.zh() { zh } else { en } }
    pub fn get(&self, k: &str) -> &str { self.args.get(k).map(|s| s.as_str()).unwrap_or("") }
    pub fn on(&self, k: &str) -> bool { self.get(k) == "true" }
    pub fn int(&self, k: &str, default: i64, min: i64, max: i64) -> i64 {
        self.get(k).trim().parse::<f64>().map(|v| v.round() as i64).unwrap_or(default).clamp(min, max)
    }
    pub fn num(&self, k: &str, default: f64) -> f64 { self.get(k).trim().parse().unwrap_or(default) }
    pub fn files(&self, field: &str) -> Vec<&File> { self.files.iter().filter(|f| f.field == field).collect() }
    pub fn file(&self, field: &str) -> Result<&File> {
        self.files.iter().find(|f| f.field == field).ok_or_else(|| self.tr("请先选择文件", "Choose a file first").to_string())
    }
}

/// 工具运行结果。
pub enum Reply { Text(String), File { name: String, data: Vec<u8>, text: String } }

pub type Result<T> = std::result::Result<T, String>;

pub fn frame(json: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + json.len() + bytes.len());
    v.extend_from_slice(&(json.len() as u32).to_le_bytes());
    v.extend_from_slice(json);
    v.extend_from_slice(bytes);
    v
}

pub fn parse(buf: &[u8]) -> Result<Request> {
    if buf.len() < 4 { return Err("short request".into()); }
    let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let j: Value = serde_json::from_slice(buf.get(4..4 + n).ok_or("bad frame")?).map_err(|e| e.to_string())?;
    let mut rest = &buf[4 + n..];
    let s = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut files = vec![];
    for f in j.get("files").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let len = f.get("len").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if len > rest.len() { return Err("bad file length".into()); }
        let (a, b) = rest.split_at(len);
        files.push(File { field: f.get("field").and_then(|v| v.as_str()).unwrap_or("").into(), name: f.get("name").and_then(|v| v.as_str()).unwrap_or("").into(), data: a.to_vec() });
        rest = b;
    }
    let args = j.get("args").and_then(|v| v.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))).collect()).unwrap_or_default();
    Ok(Request { op: s("op"), lang: s("lang"), tool: s("tool"), args, files,
        now_ms: j.get("now_ms").and_then(|v| v.as_i64()).unwrap_or(0), utc_offset: j.get("utc_offset").and_then(|v| v.as_i64()).unwrap_or(0) as i32 })
}

/// 处理一次调用，返回回复缓冲区。
pub fn handle(input: &[u8], manifest: fn(&Request) -> Value, run: fn(&Request) -> Result<Reply>) -> Vec<u8> {
    let req = match parse(input) { Ok(r) => r, Err(e) => return frame(json!({ "error": e }).to_string().as_bytes(), &[]) };
    if req.op == "manifest" {
        let mut m = manifest(&req);
        m["abi"] = json!(ABI);
        return frame(m.to_string().as_bytes(), &[]);
    }
    match run(&req) {
        Ok(Reply::Text(t)) => frame(json!({ "text": t }).to_string().as_bytes(), &[]),
        Ok(Reply::File { name, data, text }) => frame(json!({ "text": text, "file_name": name }).to_string().as_bytes(), &data),
        Err(e) => frame(json!({ "error": e }).to_string().as_bytes(), &[]),
    }
}

#[doc(hidden)]
pub fn alloc(len: i32) -> i32 {
    let mut v = Vec::<u8>::with_capacity(len.max(0) as usize);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p as usize as i32
}

#[doc(hidden)]
/// # Safety
/// `ptr` 必须来自 `alloc(len)`，且宿主已写满 `len` 字节。
pub unsafe fn run_raw(ptr: i32, len: i32, manifest: fn(&Request) -> Value, run: fn(&Request) -> Result<Reply>) -> i64 {
    let input = Vec::from_raw_parts(ptr as usize as *mut u8, len as usize, len as usize);
    let out = handle(&input, manifest, run).into_boxed_slice();
    let n = out.len();
    let p = Box::into_raw(out) as *mut u8 as usize;
    ((p as i64) << 32) | n as i64
}

/// 导出 ABI 入口：`export!(manifest, run);`
#[macro_export]
macro_rules! export {
    ($m:path, $r:path) => {
        const _: (fn(&$crate::Request) -> $crate::serde_json::Value, fn(&$crate::Request) -> $crate::Result<$crate::Reply>) = ($m, $r);
        // 只在 wasm 目标导出，避免本机测试时多个包链接在一起符号重名。
        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn shellax_alloc(len: i32) -> i32 { $crate::alloc(len) }
        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub unsafe extern "C" fn shellax_run(ptr: i32, len: i32) -> i64 { $crate::run_raw(ptr, len, $m, $r) }
    };
}

// ---------- 工具描述 ----------

/// 一个工具的描述，对应宿主的 ToolSpec。
pub fn tool(id: &str, cat: &str, name: &str, desc: &str, keywords: &str, inputs: Vec<Value>, options: Vec<Value>) -> Value {
    json!({ "id": id, "cat": cat, "name": name, "desc": desc, "keywords": keywords, "inputs": inputs, "options": options, "live": false })
}
fn field(id: &str, kind: &str, label: &str, default: &str) -> Value { json!({ "id": id, "kind": kind, "label": label, "default": default }) }
pub fn text(id: &str, label: &str, default: &str) -> Value { field(id, "text", label, default) }
pub fn multiline(id: &str, label: &str) -> Value { field(id, "multiline", label, "") }
pub fn file(id: &str, label: &str) -> Value { field(id, "file", label, "") }
pub fn files(id: &str, label: &str) -> Value { field(id, "files", label, "") }
pub fn number(id: &str, label: &str, default: &str) -> Value { field(id, "number", label, default) }
pub fn toggle(id: &str, label: &str, on: bool) -> Value { field(id, "toggle", label, if on { "true" } else { "false" }) }
pub fn select(id: &str, label: &str, choices: &[(&str, &str)]) -> Value {
    let mut f = field(id, "select", label, choices.first().map(|c| c.0).unwrap_or(""));
    f["choices"] = json!(choices.iter().map(|(v, l)| [v, l]).collect::<Vec<_>>());
    f
}
pub fn with_placeholder(mut f: Value, ph: &str) -> Value { f["placeholder"] = json!(ph); f }

pub fn manifest(id: &str, name: &str, version: &str, description: &str, tools: Vec<Value>) -> Value {
    json!({ "id": id, "name": name, "version": version, "description": description, "tools": tools })
}

// ---------- 工具函数 ----------

/// 只存储（不压缩）的 zip，用于一次输出多个文件（PDF、PNG 本身已压缩）。
pub fn zip_store(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![];
    let mut central = vec![];
    for (name, data) in entries {
        let crc = crc32fast::hash(data);
        let off = out.len() as u32;
        let n = name.as_bytes();
        let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
        let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
        le32(&mut out, 0x0403_4b50); le16(&mut out, 20); le16(&mut out, 0x0800); le16(&mut out, 0); le16(&mut out, 0); le16(&mut out, 0x21);
        le32(&mut out, crc); le32(&mut out, data.len() as u32); le32(&mut out, data.len() as u32); le16(&mut out, n.len() as u16); le16(&mut out, 0);
        out.extend_from_slice(n); out.extend_from_slice(data);
        le32(&mut central, 0x0201_4b50); le16(&mut central, 20); le16(&mut central, 20); le16(&mut central, 0x0800); le16(&mut central, 0); le16(&mut central, 0); le16(&mut central, 0x21);
        le32(&mut central, crc); le32(&mut central, data.len() as u32); le32(&mut central, data.len() as u32); le16(&mut central, n.len() as u16);
        le16(&mut central, 0); le16(&mut central, 0); le16(&mut central, 0); le16(&mut central, 0); le32(&mut central, 0); le32(&mut central, off);
        central.extend_from_slice(n);
    }
    let (coff, clen) = (out.len() as u32, central.len() as u32);
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&clen.to_le_bytes());
    out.extend_from_slice(&coff.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// 解析页码范围，如 "1-3,5,8-"，返回从 0 开始的页序号（按出现顺序，去重）。
pub fn parse_ranges(s: &str, total: usize) -> Result<Vec<usize>> {
    let mut v = vec![];
    let s = s.trim();
    if s.is_empty() { return Ok((0..total).collect()); }
    for part in s.split([',', '，', ' ']).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part.trim(), part.trim()),
        };
        let pa = if a.is_empty() { 1 } else { a.parse::<usize>().map_err(|_| format!("bad page: {part}"))? };
        let pb = if b.is_empty() { total } else { b.parse::<usize>().map_err(|_| format!("bad page: {part}"))? };
        if pa == 0 || pa > total || pb == 0 || pb > total { return Err(format!("page out of range: {part} (1-{total})")); }
        let it: Box<dyn Iterator<Item = usize>> = if pa <= pb { Box::new(pa..=pb) } else { Box::new((pb..=pa).rev()) };
        for p in it { if !v.contains(&(p - 1)) { v.push(p - 1); } }
    }
    Ok(v)
}

pub fn stem(name: &str) -> &str { name.rsplit_once('.').map(|(a, _)| a).filter(|a| !a.is_empty()).unwrap_or(name) }

#[cfg(test)]
mod tests {
    use super::*;
    fn m(_: &Request) -> Value { manifest("t", "T", "1.0.0", "", vec![tool("a", "text", "A", "", "", vec![file("f", "F")], vec![])]) }
    fn r(q: &Request) -> Result<Reply> { Ok(Reply::File { name: "x".into(), data: q.file("f")?.data.clone(), text: q.get("k").into() }) }
    #[test]
    fn roundtrip() {
        let req = frame(br#"{"op":"run","tool":"a","args":{"k":"v"},"files":[{"field":"f","name":"n","len":3}]}"#, b"abc");
        let out = handle(&req, m, r);
        let n = u32::from_le_bytes(out[..4].try_into().unwrap()) as usize;
        let j: Value = serde_json::from_slice(&out[4..4 + n]).unwrap();
        assert_eq!(j["text"], "v");
        assert_eq!(&out[4 + n..], b"abc");
        let out = handle(&frame(br#"{"op":"manifest"}"#, b""), m, r);
        let j: Value = serde_json::from_slice(&out[4..]).unwrap();
        assert_eq!(j["abi"], 1);
    }
    #[test]
    fn ranges() {
        assert_eq!(parse_ranges("1-3,5", 6).unwrap(), vec![0, 1, 2, 4]);
        assert_eq!(parse_ranges("5-", 6).unwrap(), vec![4, 5]);
        assert_eq!(parse_ranges("3-1", 6).unwrap(), vec![2, 1, 0]);
        assert_eq!(parse_ranges("", 2).unwrap(), vec![0, 1]);
        assert!(parse_ranges("9", 6).is_err());
    }
    #[test]
    fn zip_layout() {
        let z = zip_store(&[("a.txt".into(), b"hi".to_vec())]);
        assert_eq!(&z[..4], b"PK\x03\x04");
        assert_eq!(&z[z.len() - 22..z.len() - 18], b"PK\x05\x06");
    }
}

/// wasm 里没有系统随机源。依赖只用随机数做哈希种子，这里给一个确定性的伪随机序列即可，
/// 扩展包不得把它用于任何安全用途。
#[cfg(target_arch = "wasm32")]
#[no_mangle]
unsafe extern "Rust" fn __getrandom_v03_custom(dest: *mut u8, len: usize) -> std::result::Result<(), getrandom::Error> {
    static mut STATE: u64 = 0x9e37_79b9_7f4a_7c15;
    for i in 0..len {
        let mut x = STATE;
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        STATE = x;
        *dest.add(i) = x as u8;
    }
    Ok(())
}
