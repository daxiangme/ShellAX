//! Model discovery for AI providers: base-URL normalisation, listing via each API's public
//! "list models" endpoint, parsing, chat/non-chat filtering, a local cache with timestamps,
//! provider presets, and optional metadata from the models.dev catalog (MIT, fetched at
//! runtime and cached; not bundled). Written from the providers' public API docs.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai::{Kind, Provider};

// ---------------------------------------------------------------- presets
#[derive(Debug, Clone, Copy)]
pub struct Preset { pub id: &'static str, pub name: &'static str, pub kind: Kind, pub base_url: &'static str, pub model: &'static str, pub fast_model: &'static str, pub needs_key: bool }

pub const PRESETS: &[Preset] = &[
    Preset { id: "openai", name: "OpenAI", kind: Kind::OpenAi, base_url: "https://api.openai.com/v1", model: "gpt-4.1", fast_model: "gpt-4.1-mini", needs_key: true },
    Preset { id: "anthropic", name: "Anthropic", kind: Kind::Anthropic, base_url: "https://api.anthropic.com", model: "claude-sonnet-4-5", fast_model: "claude-haiku-4-5", needs_key: true },
    Preset { id: "openrouter", name: "OpenRouter", kind: Kind::OpenAi, base_url: "https://openrouter.ai/api/v1", model: "openai/gpt-4.1", fast_model: "openai/gpt-4.1-mini", needs_key: true },
    Preset { id: "deepseek", name: "DeepSeek", kind: Kind::OpenAi, base_url: "https://api.deepseek.com/v1", model: "deepseek-chat", fast_model: "deepseek-chat", needs_key: true },
    Preset { id: "qwen", name: "通义千问 DashScope", kind: Kind::OpenAi, base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus", fast_model: "qwen-turbo", needs_key: true },
    Preset { id: "kimi", name: "Kimi (Moonshot)", kind: Kind::OpenAi, base_url: "https://api.moonshot.cn/v1", model: "kimi-k2-0905-preview", fast_model: "moonshot-v1-8k", needs_key: true },
    Preset { id: "zhipu", name: "智谱 GLM", kind: Kind::OpenAi, base_url: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4.6", fast_model: "glm-4-flash", needs_key: true },
    Preset { id: "ollama", name: "Ollama (local)", kind: Kind::OpenAi, base_url: "http://localhost:11434/v1", model: "llama3.2", fast_model: "", needs_key: false },
    Preset { id: "lmstudio", name: "LM Studio (local)", kind: Kind::OpenAi, base_url: "http://localhost:1234/v1", model: "", fast_model: "", needs_key: false },
    Preset { id: "custom", name: "Custom (OpenAI-compatible)", kind: Kind::OpenAi, base_url: "", model: "", fast_model: "", needs_key: true },
];
pub fn preset(id: &str) -> Option<&'static Preset> { PRESETS.iter().find(|p| p.id == id) }

// ---------------------------------------------------------------- URL normalisation
/// Normalise a user-entered base URL into the API root that `/models`, `/chat/completions`
/// (OpenAI-compatible) or `/models`, `/messages` (Anthropic) are appended to.
/// - trims whitespace, trailing `/`, and a pasted endpoint suffix (`/chat/completions`, `/models`, `/messages`)
/// - OpenAI-compatible: a bare host (no path) gets `/v1`; an explicit path (`/api/v1`,
///   `/compatible-mode/v1`, `/api/paas/v4`, `/v1beta/openai`) is kept as is
/// - Anthropic: always `<host>/v1`
pub fn api_base(kind: Kind, url: &str) -> String {
    let mut u = url.trim().trim_end_matches('/').to_string();
    for suf in ["/chat/completions", "/completions", "/models", "/messages"] {
        if let Some(x) = u.strip_suffix(suf) { u = x.trim_end_matches('/').to_string(); }
    }
    if !u.contains("://") && !u.is_empty() { u = format!("https://{u}"); }
    let path_start = u.find("://").map(|i| i + 3).and_then(|i| u[i..].find('/').map(|j| i + j));
    match kind {
        Kind::Anthropic => {
            let host = path_start.map(|p| &u[..p]).unwrap_or(&u).to_string();
            format!("{host}/v1")
        }
        Kind::OpenAi => if path_start.is_none() { format!("{u}/v1") } else { u },
    }
}
/// `http://host:11434/v1` -> `http://host:11434` (for Ollama's native /api/tags).
fn host_root(base: &str) -> String {
    let i = base.find("://").map(|i| i + 3).unwrap_or(0);
    match base[i..].find('/') { Some(j) => base[..i + j].to_string(), None => base.to_string() }
}

// ---------------------------------------------------------------- model info
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Use { Chat, Embedding, Audio, Image, Moderation, Other }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)] pub name: String,
    #[serde(default)] pub context: Option<u64>,
    /// USD per 1M input / output tokens
    #[serde(default)] pub price_in: Option<f64>,
    #[serde(default)] pub price_out: Option<f64>,
    pub usage: Use,
}

/// Heuristic usage from the model id (APIs rarely say whether a model can chat).
pub fn classify(id: &str) -> Use {
    let s = id.to_lowercase();
    let has = |xs: &[&str]| xs.iter().any(|x| s.contains(x));
    if has(&["embed", "bge-", "e5-", "gte-", "nomic-embed"]) { Use::Embedding }
    else if has(&["rerank"]) { Use::Other }
    else if has(&["whisper", "tts", "transcribe", "audio", "speech", "realtime"]) { Use::Audio }
    else if has(&["dall-e", "gpt-image", "image", "flux", "stable-diffusion", "sdxl", "imagen", "midjourney", "sora", "veo"]) { Use::Image }
    else if has(&["moderation", "omni-moderation", "guard"]) { Use::Moderation }
    else { Use::Chat }
}

fn per_million(v: Option<&Value>) -> Option<f64> {
    let x = match v? { Value::String(s) => s.parse::<f64>().ok()?, Value::Number(n) => n.as_f64()?, _ => return None };
    (x >= 0.0).then_some(x * 1_000_000.0)
}

/// Parse an OpenAI-style list (`{"data":[{"id":..}]}`), OpenRouter extras (`name`,
/// `context_length`, `pricing.prompt/completion` per token), a bare array, or Ollama's
/// `/api/tags` (`{"models":[{"name":..}]}`).
pub fn parse_openai_list(v: &Value) -> Result<Vec<ModelInfo>> {
    let items = v.get("data").and_then(|d| d.as_array())
        .or_else(|| v.get("models").and_then(|d| d.as_array()))
        .or_else(|| v.as_array())
        .ok_or_else(|| anyhow!("response has no `data` / `models` array"))?;
    let mut out: Vec<ModelInfo> = items.iter().filter_map(|m| {
        let id = m.get("id").or_else(|| m.get("model")).or_else(|| m.get("name")).and_then(|x| x.as_str())?.to_string();
        let name = m.get("name").and_then(|x| x.as_str()).filter(|n| *n != id).unwrap_or("").to_string();
        let context = m.get("context_length").or_else(|| m.get("context_window")).or_else(|| m.get("max_context_length")).and_then(|x| x.as_u64());
        let pr = m.get("pricing");
        Some(ModelInfo { usage: classify(&id), price_in: per_million(pr.and_then(|p| p.get("prompt"))), price_out: per_million(pr.and_then(|p| p.get("completion"))), id, name, context })
    }).collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Ok(out)
}

/// One Anthropic page: (models, has_more, last_id).
pub fn parse_anthropic_page(v: &Value) -> Result<(Vec<ModelInfo>, bool, Option<String>)> {
    let items = v.get("data").and_then(|d| d.as_array()).ok_or_else(|| anyhow!("response has no `data` array"))?;
    let models = items.iter().filter_map(|m| {
        let id = m.get("id")?.as_str()?.to_string();
        Some(ModelInfo { usage: classify(&id), name: m.get("display_name").and_then(|x| x.as_str()).unwrap_or("").to_string(), id, context: None, price_in: None, price_out: None })
    }).collect();
    Ok((models, v.get("has_more").and_then(|x| x.as_bool()).unwrap_or(false), v.get("last_id").and_then(|x| x.as_str()).map(String::from)))
}

fn http_err(e: ureq::Error, key: &str) -> anyhow::Error {
    let scrub = |s: String| { let s: String = s.chars().take(240).collect(); if key.len() >= 4 { s.replace(key, "[key]") } else { s } };
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let hint = match code { 401 | 403 => crate::i18n::t("models.hint_auth"), 404 => crate::i18n::t("models.hint_404"), _ => "" };
            anyhow!("HTTP {code}{hint}: {}", scrub(body))
        }
        e => anyhow!("{}", scrub(e.to_string())),
    }
}

/// List models from the provider's API.
pub fn list_models(p: &Provider, key: &str) -> Result<Vec<ModelInfo>> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(p.timeout_secs.clamp(3, 60))).build();
    let base = api_base(p.kind, &p.base_url);
    match p.kind {
        Kind::OpenAi => {
            let mut req = agent.get(&format!("{base}/models"));
            if !key.is_empty() { req = req.set("authorization", &format!("Bearer {key}")); }
            match req.call() {
                Ok(r) => parse_openai_list(&r.into_json::<Value>().map_err(|e| anyhow!("bad JSON: {e}"))?),
                // older Ollama builds have no /v1/models: fall back to the native /api/tags
                Err(ureq::Error::Status(404, _)) if p.preset == "ollama" || base.contains(":11434") => {
                    let r = agent.get(&format!("{}/api/tags", host_root(&base))).call().map_err(|e| http_err(e, key))?;
                    parse_openai_list(&r.into_json::<Value>()?)
                }
                Err(e) => Err(http_err(e, key)),
            }
        }
        Kind::Anthropic => {
            let mut out = vec![];
            let mut after: Option<String> = None;
            for _ in 0..20 {
                let mut url = format!("{base}/models?limit=1000");
                if let Some(a) = &after { url.push_str(&format!("&after_id={a}")); }
                let r = agent.get(&url).set("x-api-key", key).set("anthropic-version", "2023-06-01").call().map_err(|e| http_err(e, key))?;
                let (mut m, more, last) = parse_anthropic_page(&r.into_json::<Value>()?)?;
                out.append(&mut m);
                if !more || last.is_none() { break; }
                after = last;
            }
            Ok(out)
        }
    }
}

pub fn filter_chat(models: &[ModelInfo], chat_only: bool) -> Vec<ModelInfo> {
    models.iter().filter(|m| !chat_only || m.usage == Use::Chat).cloned().collect()
}
/// Case-insensitive substring search on id and name.
pub fn search<'a>(models: &'a [ModelInfo], q: &str) -> Vec<&'a ModelInfo> {
    let q = q.trim().to_lowercase();
    models.iter().filter(|m| q.is_empty() || m.id.to_lowercase().contains(&q) || m.name.to_lowercase().contains(&q)).collect()
}

// ---------------------------------------------------------------- cache
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheEntry { pub fetched_at: i64, pub base_url: String, pub models: Vec<ModelInfo> }

/// `models-cache.json`: provider id -> entry. Contains no secrets.
pub struct ModelCache { path: PathBuf }
impl ModelCache {
    pub fn new(dir: &Path) -> Self { Self { path: dir.join("models-cache.json") } }
    fn load(&self) -> HashMap<String, CacheEntry> {
        std::fs::read_to_string(&self.path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    /// Entry for this provider if it was fetched for the same base URL.
    pub fn get(&self, provider_id: &str, base_url: &str) -> Option<CacheEntry> {
        self.load().remove(provider_id).filter(|e| e.base_url == base_url)
    }
    pub fn put(&self, provider_id: &str, base_url: &str, models: Vec<ModelInfo>, now: i64) -> Result<()> {
        let mut m = self.load();
        m.insert(provider_id.into(), CacheEntry { fetched_at: now, base_url: base_url.into(), models });
        if let Some(d) = self.path.parent() { std::fs::create_dir_all(d)?; }
        std::fs::write(&self.path, serde_json::to_string(&m)?)?;
        Ok(())
    }
    pub fn remove(&self, provider_id: &str) {
        let mut m = self.load();
        if m.remove(provider_id).is_some() { let _ = std::fs::write(&self.path, serde_json::to_string(&m).unwrap_or_default()); }
    }
}
pub fn is_fresh(e: &CacheEntry, now: i64, ttl_secs: i64) -> bool { now - e.fetched_at < ttl_secs }

// ---------------------------------------------------------------- models.dev metadata
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// context window, $/1M input, $/1M output per model id
pub type ModelsDevMeta = HashMap<String, (Option<u64>, Option<f64>, Option<f64>)>;

/// model id -> (context, $/1M in, $/1M out) from models.dev's api.json
/// (`{provider: {models: {id: {limit:{context}, cost:{input,output}}}}}`, costs already per 1M).
pub fn parse_models_dev(v: &Value) -> ModelsDevMeta {
    let mut out = HashMap::new();
    let Some(provs) = v.as_object() else { return out };
    for (pid, prov) in provs {
        let Some(models) = prov.get("models").and_then(|m| m.as_object()) else { continue };
        for (mid, m) in models {
            let ctx = m.pointer("/limit/context").and_then(|x| x.as_u64()).filter(|c| *c > 0);
            let ci = m.pointer("/cost/input").and_then(|x| x.as_f64());
            let co = m.pointer("/cost/output").and_then(|x| x.as_f64());
            if ctx.is_none() && ci.is_none() { continue; }
            out.insert(format!("{pid}/{mid}"), (ctx, ci, co));
            out.entry(mid.clone()).or_insert((ctx, ci, co));
        }
    }
    out
}
/// Fill missing context / price from models.dev (by "<preset>/<id>", then by bare id).
pub fn enrich(models: &mut [ModelInfo], preset: &str, meta: &ModelsDevMeta) {
    for m in models {
        let hit = meta.get(&format!("{preset}/{}", m.id)).or_else(|| meta.get(&m.id))
            .or_else(|| m.id.split_once('/').and_then(|(_, b)| meta.get(b)));
        if let Some((c, i, o)) = hit {
            if m.context.is_none() { m.context = *c; }
            if m.price_in.is_none() { m.price_in = *i; }
            if m.price_out.is_none() { m.price_out = *o; }
        }
    }
}
/// Fetch models.dev (cached in `dir/models-dev.json` for `ttl_secs`).
pub fn models_dev(dir: &Path, now: i64, ttl_secs: i64) -> Option<ModelsDevMeta> {
    let path = dir.join("models-dev.json");
    let fresh = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| (now - d.as_secs() as i64) < ttl_secs).unwrap_or(false);
    if !fresh {
        if let Ok(r) = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).build().get(MODELS_DEV_URL).call() {
            if let Ok(body) = r.into_string() { let _ = std::fs::create_dir_all(dir); let _ = std::fs::write(&path, body); }
        }
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    Some(parse_models_dev(&v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_url_normalisation() {
        let o = |u| api_base(Kind::OpenAi, u);
        assert_eq!(o("https://api.openai.com"), "https://api.openai.com/v1");
        assert_eq!(o("https://api.openai.com/v1/"), "https://api.openai.com/v1");
        assert_eq!(o("https://api.openai.com/v1/chat/completions"), "https://api.openai.com/v1");
        assert_eq!(o("https://api.deepseek.com"), "https://api.deepseek.com/v1");
        assert_eq!(o("https://openrouter.ai/api/v1/models"), "https://openrouter.ai/api/v1");
        assert_eq!(o("https://dashscope.aliyuncs.com/compatible-mode/v1"), "https://dashscope.aliyuncs.com/compatible-mode/v1");
        assert_eq!(o("https://open.bigmodel.cn/api/paas/v4/"), "https://open.bigmodel.cn/api/paas/v4");
        assert_eq!(o("localhost:11434"), "https://localhost:11434/v1");
        assert_eq!(o("http://localhost:11434"), "http://localhost:11434/v1");
        let a = |u| api_base(Kind::Anthropic, u);
        assert_eq!(a("https://api.anthropic.com"), "https://api.anthropic.com/v1");
        assert_eq!(a("https://api.anthropic.com/v1/"), "https://api.anthropic.com/v1");
        assert_eq!(a("https://api.anthropic.com/v1/messages"), "https://api.anthropic.com/v1");
        assert_eq!(host_root("http://h:11434/v1"), "http://h:11434");
    }
    #[test]
    fn parses_openai_openrouter_and_ollama() {
        let v = json!({"object":"list","data":[{"id":"gpt-4.1","owned_by":"openai"},{"id":"text-embedding-3-small"},{"id":"gpt-4.1"}]});
        let m = parse_openai_list(&v).unwrap();
        assert_eq!(m.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["gpt-4.1", "text-embedding-3-small"]);
        assert_eq!(m[1].usage, Use::Embedding);
        let or = json!({"data":[{"id":"anthropic/claude-sonnet-4.5","name":"Anthropic: Claude Sonnet 4.5","context_length":1000000,"pricing":{"prompt":"0.000003","completion":"0.000015"}}]});
        let m = parse_openai_list(&or).unwrap();
        assert_eq!(m[0].context, Some(1_000_000));
        assert!((m[0].price_in.unwrap() - 3.0).abs() < 1e-9 && (m[0].price_out.unwrap() - 15.0).abs() < 1e-9);
        let ol = json!({"models":[{"name":"llama3.2:latest","model":"llama3.2:latest","size":2019393189}]});
        assert_eq!(parse_openai_list(&ol).unwrap()[0].id, "llama3.2:latest");
        assert!(parse_openai_list(&json!({"error":"x"})).is_err());
    }
    #[test]
    fn parses_anthropic_page() {
        let v = json!({"data":[{"type":"model","id":"claude-sonnet-4-5","display_name":"Claude Sonnet 4.5"}],"has_more":true,"first_id":"a","last_id":"claude-sonnet-4-5"});
        let (m, more, last) = parse_anthropic_page(&v).unwrap();
        assert_eq!((m[0].name.as_str(), more, last.as_deref()), ("Claude Sonnet 4.5", true, Some("claude-sonnet-4-5")));
    }
    #[test]
    fn classify_and_filter() {
        for (id, u) in [("gpt-4o-mini", Use::Chat), ("text-embedding-3-large", Use::Embedding), ("whisper-1", Use::Audio), ("tts-1-hd", Use::Audio),
                        ("dall-e-3", Use::Image), ("gpt-image-1", Use::Image), ("omni-moderation-latest", Use::Moderation), ("deepseek-chat", Use::Chat), ("nomic-embed-text:latest", Use::Embedding)] {
            assert_eq!(classify(id), u, "{id}");
        }
        let ms: Vec<ModelInfo> = ["gpt-4o", "whisper-1", "text-embedding-3-small"].iter().map(|i| ModelInfo { id: i.to_string(), name: String::new(), context: None, price_in: None, price_out: None, usage: classify(i) }).collect();
        assert_eq!(filter_chat(&ms, true).len(), 1);
        assert_eq!(filter_chat(&ms, false).len(), 3);
        assert_eq!(search(&ms, "EMBED").len(), 1);
    }
    #[test]
    fn cache_roundtrip_and_expiry() {
        let dir = std::env::temp_dir().join(format!("as-mcache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let c = ModelCache::new(&dir);
        let m = vec![ModelInfo { id: "m1".into(), name: String::new(), context: Some(8192), price_in: None, price_out: None, usage: Use::Chat }];
        c.put("p1", "https://x/v1", m.clone(), 1000).unwrap();
        let e = c.get("p1", "https://x/v1").unwrap();
        assert_eq!(e.models, m);
        assert!(is_fresh(&e, 1000 + 3599, 3600) && !is_fresh(&e, 1000 + 3600, 3600));
        assert!(c.get("p1", "https://other/v1").is_none(), "cache is per base URL");
        c.remove("p1");
        assert!(c.get("p1", "https://x/v1").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn models_dev_enrichment() {
        let v = json!({"deepseek":{"models":{"deepseek-chat":{"limit":{"context":128000},"cost":{"input":0.28,"output":0.42}}}},
                       "openai":{"models":{"gpt-4.1":{"limit":{"context":1047576},"cost":{"input":2.0,"output":8.0}}}}});
        let meta = parse_models_dev(&v);
        let mut ms = vec![ModelInfo { id: "deepseek-chat".into(), name: String::new(), context: None, price_in: None, price_out: None, usage: Use::Chat },
                          ModelInfo { id: "openai/gpt-4.1".into(), name: String::new(), context: Some(5), price_in: None, price_out: None, usage: Use::Chat }];
        enrich(&mut ms, "deepseek", &meta);
        assert_eq!((ms[0].context, ms[0].price_in), (Some(128000), Some(0.28)));
        assert_eq!((ms[1].context, ms[1].price_out), (Some(5), Some(8.0)), "existing values win, missing ones filled");
    }
    /// One-shot server returning `body` with `status`; yields the raw request.
    fn mock(status: u16, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 8192];
            let n = c.read(&mut buf).unwrap();
            let resp = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            c.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        (format!("http://{addr}"), h)
    }
    #[test]
    fn list_openai_sends_bearer_and_normalises() {
        let (base, h) = mock(200, r#"{"data":[{"id":"mock-fast"},{"id":"mock-embed-1"}]}"#);
        let p = Provider { id: "x".into(), base_url: base, ..Default::default() };
        let m = list_models(&p, "sk-LIST-1234").unwrap();
        assert_eq!(m.len(), 2);
        let req = h.join().unwrap().to_lowercase();
        assert!(req.starts_with("get /v1/models "), "{req}");
        assert!(req.contains("authorization: bearer sk-list-1234"));
    }
    #[test]
    fn list_anthropic_headers_and_errors() {
        let (base, h) = mock(200, r#"{"data":[{"id":"claude-x","display_name":"Claude X"}],"has_more":false,"last_id":"claude-x"}"#);
        let p = Provider { id: "a".into(), kind: Kind::Anthropic, base_url: base, ..Default::default() };
        assert_eq!(list_models(&p, "ak-9").unwrap()[0].id, "claude-x");
        let req = h.join().unwrap().to_lowercase();
        assert!(req.starts_with("get /v1/models?limit=1000") && req.contains("x-api-key: ak-9") && req.contains("anthropic-version: 2023-06-01"));
        let (base, h) = mock(401, r#"{"error":"bad key sk-SECRET-77"}"#);
        let p = Provider { id: "b".into(), base_url: base, ..Default::default() };
        let e = list_models(&p, "sk-SECRET-77").unwrap_err().to_string();
        h.join().unwrap();
        assert!(e.contains("401") && !e.contains("sk-SECRET-77"), "{e}");
    }
}
