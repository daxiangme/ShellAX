//! 扩展包：WebAssembly 模块，在 wasmi 解释器里运行，不提供 WASI，也不提供任何导入函数。
//! 宿主只负责把字节传进去、把字节取出来；界面由宿主按包里的工具描述原生渲染。
//!
//! ABI（版本 1），模块需导出：
//! - `memory`
//! - `shellax_alloc(len: i32) -> i32`：分配 `len` 字节，返回指针。
//! - `shellax_run(ptr: i32, len: i32) -> i64`：返回 `(ptr << 32) | len`。
//!
//! 请求与响应缓冲区格式相同：`[u32 LE json_len][json][附加字节]`。
//! 请求 JSON：`{"op":"manifest"|"run","lang":"zh-CN"|"en","tool":..,"args":{..},"files":[{"field","name","len"}]}`，
//! 文件内容按 `files` 顺序依次拼在 JSON 之后。`op=run` 时还带 `now_ms`（Unix 毫秒）与 `utc_offset`（秒），
//! 因为扩展包里没有时钟。
//! 响应 JSON：`op=manifest` 时为 [`PackManifest`]；`op=run` 时为 [`RunReply`]，附加字节是输出文件内容。
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::tools::{Args, Output, ToolSpec};

pub const ABI: u32 = 1;
/// 扩展索引：发布在 ext-index 版本下的 index.json。
pub const INDEX_URL: &str = "https://github.com/daxiangme/ShellAX/releases/download/ext-index/index.json";
const FUEL: u64 = 400_000_000_000;
const MEMORY_LIMIT: usize = 1 << 30;
const MAX_REPLY: usize = 512 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackManifest {
    #[serde(default)]
    pub abi: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunReply {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub color: Option<[u8; 4]>,
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Serialize)]
struct FileMeta<'a> { field: &'a str, name: String, len: usize }

/// 打包 `[u32 json_len][json][bytes]`。
pub fn frame(json: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + json.len() + bytes.len());
    v.extend_from_slice(&(json.len() as u32).to_le_bytes());
    v.extend_from_slice(json);
    v.extend_from_slice(bytes);
    v
}

/// 拆开 `[u32 json_len][json][bytes]`。
pub fn unframe(buf: &[u8]) -> Result<(&[u8], &[u8])> {
    if buf.len() < 4 { bail!("reply too short"); }
    let n = u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize;
    if 4 + n > buf.len() { bail!("bad reply frame"); }
    Ok((&buf[4..4 + n], &buf[4 + n..]))
}

fn lang() -> &'static str { if crate::i18n::current() == crate::i18n::Lang::ZhCn { "zh-CN" } else { "en" } }

/// 在全新实例里调用一次 `shellax_run`。每次调用都重新实例化，调用之间不共享状态。
pub fn call(wasm: &[u8], req: &[u8]) -> Result<Vec<u8>> { call_with(wasm, req, FUEL) }

fn call_with(wasm: &[u8], req: &[u8], fuel: u64) -> Result<Vec<u8>> {
    use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};
    let mut cfg = Config::default();
    cfg.consume_fuel(true);
    let engine = Engine::new(&cfg);
    let module = Module::new(&engine, wasm).map_err(|e| anyhow!("invalid extension module: {e}"))?;
    if module.imports().len() > 0 { bail!("extension modules must not import anything"); }
    let limits = StoreLimitsBuilder::new().memory_size(MEMORY_LIMIT).instances(1).build();
    let mut store: Store<StoreLimits> = Store::new(&engine, limits);
    store.limiter(|l| l);
    store.set_fuel(fuel).map_err(|e| anyhow!("{e}"))?;
    let linker = Linker::<StoreLimits>::new(&engine);
    let inst = linker.instantiate_and_start(&mut store, &module).map_err(|e| anyhow!("instantiate: {e}"))?;
    let mem = inst.get_memory(&store, "memory").ok_or_else(|| anyhow!("extension does not export memory"))?;
    let alloc = inst.get_typed_func::<i32, i32>(&store, "shellax_alloc").map_err(|e| anyhow!("shellax_alloc: {e}"))?;
    let run = inst.get_typed_func::<(i32, i32), i64>(&store, "shellax_run").map_err(|e| anyhow!("shellax_run: {e}"))?;
    let len = i32::try_from(req.len()).context("request too large")?;
    let ptr = alloc.call(&mut store, len).map_err(|e| anyhow!("shellax_alloc: {e}"))?;
    mem.write(&mut store, ptr as u32 as usize, req).map_err(|e| anyhow!("write request: {e}"))?;
    let r = run.call(&mut store, (ptr, len)).map_err(|e| anyhow!("{e}"))? as u64;
    let (rp, rl) = ((r >> 32) as usize, (r & 0xffff_ffff) as usize);
    if rl > MAX_REPLY { bail!("reply too large"); }
    let mut out = vec![0u8; rl];
    mem.read(&store, rp, &mut out).map_err(|e| anyhow!("read reply: {e}"))?;
    Ok(out)
}

/// 读取包的清单（按当前语言），并校验 ABI 版本，给每个工具填上包 id。
pub fn manifest_of(wasm: &[u8]) -> Result<PackManifest> {
    let req = serde_json::json!({ "op": "manifest", "lang": lang() }).to_string();
    let out = call(wasm, &frame(req.as_bytes(), &[]))?;
    let (j, _) = unframe(&out)?;
    let mut m: PackManifest = serde_json::from_slice(j).context("bad manifest")?;
    if m.abi != ABI { bail!("unsupported extension ABI {} (expected {ABI})", m.abi); }
    if m.id.is_empty() || !m.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { bail!("bad extension id"); }
    for t in &mut m.tools { t.pack = m.id.clone(); t.heavy = true; }
    Ok(m)
}

/// 运行包里的一个工具。文件字段的内容由宿主读出后传入。
pub fn run_tool(wasm: &[u8], tool: &str, a: &Args) -> Result<Output> {
    let mut files = vec![];
    let mut bytes = vec![];
    for (field, paths) in &a.files {
        for p in paths {
            let data = std::fs::read(p).with_context(|| p.display().to_string())?;
            files.push(FileMeta { field, name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), len: data.len() });
            bytes.extend_from_slice(&data);
        }
    }
    let args: BTreeMap<_, _> = a.values.iter().collect();
    // 扩展包里没有时钟，当前时间与本地时区偏移由宿主传入。
    let now = chrono::Local::now();
    let req = serde_json::json!({ "op": "run", "lang": lang(), "tool": tool, "args": args, "files": files,
        "now_ms": now.timestamp_millis(), "utc_offset": now.offset().local_minus_utc() }).to_string();
    let out = call(wasm, &frame(req.as_bytes(), &bytes))?;
    let (j, rest) = unframe(&out)?;
    let r: RunReply = serde_json::from_slice(j).context("bad reply")?;
    if let Some(e) = r.error { bail!(e); }
    Ok(Output { text: r.text, color: r.color, file: r.file_name.map(|n| (n, rest.to_vec())), ..Default::default() })
}

// ---------- 本地安装 ----------

pub fn ext_dir() -> PathBuf { crate::platform::config_dir().join("ext") }

#[derive(Clone, Debug, PartialEq)]
pub struct Installed { pub manifest: PackManifest, pub wasm: PathBuf }

static INSTALLED: RwLock<Option<Vec<Installed>>> = RwLock::new(None);

fn scan(dir: &Path) -> Vec<Installed> {
    let mut v = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return v };
    for e in rd.flatten() {
        let p = e.path();
        let (mf, wasm) = (p.join("manifest.json"), p.join("pack.wasm"));
        if !wasm.is_file() { continue; }
        let m = std::fs::read(&mf).ok().and_then(|b| serde_json::from_slice::<PackManifest>(&b).ok());
        if let Some(m) = m { v.push(Installed { manifest: m, wasm }); }
    }
    v.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    v
}

/// 已安装的包（缓存；安装、卸载、切换语言后调用 [`reload`]）。
pub fn installed() -> Vec<Installed> {
    if let Some(v) = INSTALLED.read().unwrap().as_ref() { return v.clone(); }
    let v = scan(&ext_dir());
    *INSTALLED.write().unwrap() = Some(v.clone());
    v
}

/// 重新扫描；清单按当前语言重新生成（失败时保留旧清单）。
pub fn reload() {
    let dir = ext_dir();
    for i in scan(&dir) {
        if let Ok(b) = std::fs::read(&i.wasm) {
            if let Ok(m) = manifest_of(&b) { if m != i.manifest { let _ = write_manifest(&dir.join(&m.id), &m); } }
        }
    }
    *INSTALLED.write().unwrap() = Some(scan(&dir));
}

fn write_manifest(dir: &Path, m: &PackManifest) -> Result<()> {
    std::fs::write(dir.join("manifest.json"), serde_json::to_vec_pretty(m)?)?;
    Ok(())
}

/// 已安装包的全部工具。
pub fn installed_tools() -> Vec<ToolSpec> { installed().into_iter().flat_map(|i| i.manifest.tools).collect() }

/// 运行已安装包里的工具。
pub fn run_installed(pack: &str, tool: &str, a: &Args) -> Result<Output> {
    let i = installed().into_iter().find(|i| i.manifest.id == pack).ok_or_else(|| anyhow!(crate::i18n::tf("ext.err_not_installed", &[("id", &pack)])))?;
    let wasm = std::fs::read(&i.wasm)?;
    run_tool(&wasm, tool, a)
}

/// 从本地 wasm 字节安装（下载完成后、或测试时调用）。返回清单。
pub fn install_bytes(dir: &Path, wasm: &[u8]) -> Result<PackManifest> {
    let m = manifest_of(wasm)?;
    let d = dir.join(&m.id);
    std::fs::create_dir_all(&d)?;
    let tmp = d.join("pack.wasm.new");
    std::fs::write(&tmp, wasm)?;
    std::fs::rename(&tmp, d.join("pack.wasm"))?;
    write_manifest(&d, &m)?;
    Ok(m)
}

/// 下载、校验 SHA-256 并安装（覆盖旧版本即为更新）。走系统代理，失败自动重试。
pub fn install(p: &IndexPack, prog: &crate::updater::Progress) -> Result<PackManifest> {
    let dir = ext_dir();
    std::fs::create_dir_all(&dir)?;
    let dl = dir.join(format!("{}.download", p.id));
    let sha = (!p.sha256.is_empty()).then_some(p.sha256.as_str());
    crate::updater::fetch_to(&p.url, &dl, sha, prog)?;
    let res = std::fs::read(&dl).map_err(anyhow::Error::from).and_then(|b| install_bytes(&dir, &b));
    let _ = std::fs::remove_file(&dl);
    let m = res?;
    if m.id != p.id { let _ = std::fs::remove_dir_all(dir.join(&m.id)); bail!("pack id mismatch ({} ≠ {})", m.id, p.id); }
    reload();
    Ok(m)
}

pub fn uninstall(id: &str) -> Result<()> {
    if id.is_empty() || id.contains(['/', '\\', '.']) { bail!("bad id"); }
    let d = ext_dir().join(id);
    if d.exists() { std::fs::remove_dir_all(&d)?; }
    reload();
    Ok(())
}

// ---------- 远程索引 ----------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexTool { pub id: String, #[serde(default)] pub cat: String }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexPack {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub sha256: String,
    pub url: String,
    #[serde(default)]
    pub tools: Vec<IndexTool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Index { #[serde(default)] pub packs: Vec<IndexPack> }

fn index_cache() -> PathBuf { ext_dir().join("index.json") }

/// 上次成功获取的索引（离线时使用）。
pub fn cached_index() -> Option<Index> { std::fs::read(index_cache()).ok().and_then(|b| serde_json::from_slice(&b).ok()) }

/// 获取远程索引并缓存。
pub fn fetch_index() -> Result<Index> {
    let s = crate::updater::fetch_text(INDEX_URL)?;
    let idx: Index = serde_json::from_str(&s).context("bad extension index")?;
    let _ = std::fs::create_dir_all(ext_dir());
    let _ = std::fs::write(index_cache(), &s);
    Ok(idx)
}

/// 已知的扩展包与其工具（未安装、也还没获取索引时，用来显示灰色占位）。
/// (包 id, 分类, 工具 id 列表)；名称文案为 `ext.<包>` 与 `tools.<工具>`。
pub const KNOWN: &[(&str, &str, &[&str])] = &[
    ("lang-zh", "lang", &["pinyin", "zh_convert"]),
    ("worldtime", "time", &["world_clock", "tz_convert"]),
    ("image", "image", &["qr_gen", "qr_decode", "barcode", "img_compress", "img_watermark"]),
    ("pdf", "pdf", &["pdf_merge", "pdf_split", "pdf_rotate", "pdf_delete", "pdf_reorder", "pdf_watermark", "pdf_pagenum", "img_to_pdf", "pdf_text", "pdf_clean"]),
    ("pdf-render", "pdf", &["pdf_to_img"]),
];

pub fn pack_name(id: &str) -> String {
    let k = format!("ext.{id}");
    let s = crate::i18n::t(&k);
    if s == k { id.to_string() } else { s.to_string() }
}

/// 某个包的状态，供界面显示。
#[derive(Clone, Debug, PartialEq)]
pub struct PackRow { pub id: String, pub name: String, pub installed: Option<String>, pub latest: Option<IndexPack> }
impl PackRow {
    pub fn has_update(&self) -> bool {
        matches!((&self.installed, &self.latest), (Some(a), Some(l)) if crate::update::is_newer(&l.version, a))
    }
}

/// 合并已知列表、索引与本地安装，得到扩展管理器的行。
pub fn rows(index: Option<&Index>, inst: &[Installed]) -> Vec<PackRow> {
    let mut ids: Vec<String> = KNOWN.iter().map(|k| k.0.to_string()).collect();
    for p in index.map(|i| i.packs.as_slice()).unwrap_or(&[]) { if !ids.contains(&p.id) { ids.push(p.id.clone()); } }
    for i in inst { if !ids.contains(&i.manifest.id) { ids.push(i.manifest.id.clone()); } }
    ids.into_iter().map(|id| {
        let ins = inst.iter().find(|i| i.manifest.id == id);
        PackRow {
            name: ins.map(|i| i.manifest.name.clone()).unwrap_or_else(|| pack_name(&id)),
            installed: ins.map(|i| i.manifest.version.clone()),
            latest: index.and_then(|x| x.packs.iter().find(|p| p.id == id).cloned()),
            id,
        }
    }).collect()
}

/// 未安装包的工具占位：(包 id, 工具 spec)。spec 只有 id、分类与名称。
pub fn placeholders(inst: &[Installed]) -> Vec<ToolSpec> {
    let mut v = vec![];
    for (pack, cat, tools) in KNOWN {
        if inst.iter().any(|i| i.manifest.id == *pack) { continue; }
        for t in *tools {
            v.push(ToolSpec {
                id: t.to_string(), cat: cat.to_string(), name: crate::i18n::t(&format!("tools.{t}")).to_string(),
                desc: crate::i18n::t(&format!("tools.{t}_desc")).to_string(), keywords: String::new(), inputs: vec![], options: vec![],
                interactive: false, heavy: true, live: false, pack: pack.to_string(),
            });
        }
    }
    v
}

#[cfg(test)]
mod tests;
