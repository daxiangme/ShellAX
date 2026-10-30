//! Minimal i18n: flat `"key" = "text"` TOML files in locales/, embedded at build time.
//! Lookup order: current language -> English -> the key itself. `{name}` placeholders via `tf`.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang { En, ZhCn }

pub const EN_SRC: &str = include_str!("../locales/en.toml");
pub const ZH_SRC: &str = include_str!("../locales/zh-CN.toml");

static CUR: AtomicU8 = AtomicU8::new(0);

pub fn parse(src: &str) -> HashMap<String, String> {
    let v: toml::Table = toml::from_str(src).expect("locale file must be valid TOML");
    v.into_iter().filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string()))).collect()
}
fn table(l: Lang) -> &'static HashMap<String, String> {
    static EN: OnceLock<HashMap<String, String>> = OnceLock::new();
    static ZH: OnceLock<HashMap<String, String>> = OnceLock::new();
    match l { Lang::En => EN.get_or_init(|| parse(EN_SRC)), Lang::ZhCn => ZH.get_or_init(|| parse(ZH_SRC)) }
}

/// Resolve the "system" setting from LC_ALL / LC_MESSAGES / LANG (POSIX precedence).
pub fn system_lang() -> Lang {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        if let Ok(v) = std::env::var(var) {
            if v.is_empty() { continue; }
            return if v.to_lowercase().starts_with("zh") { Lang::ZhCn } else { Lang::En };
        }
    }
    Lang::En
}
pub fn resolve(setting: &str) -> Lang {
    match setting { "zh-CN" | "zh" => Lang::ZhCn, "en" => Lang::En, _ => system_lang() }
}
pub fn set_language(setting: &str) { CUR.store(resolve(setting) as u8, Ordering::Relaxed); }
pub fn current() -> Lang { if CUR.load(Ordering::Relaxed) == 1 { Lang::ZhCn } else { Lang::En } }

pub fn t(key: &str) -> &'static str {
    if let Some(s) = table(current()).get(key) { return s; }
    if let Some(s) = table(Lang::En).get(key) { return s; }
    // unknown key: leak once so callers can keep &'static str (keys are a small fixed set)
    static MISSING: OnceLock<std::sync::Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut m = MISSING.get_or_init(Default::default).lock().unwrap();
    m.entry(key.to_string()).or_insert_with(|| Box::leak(key.to_string().into_boxed_str()))
}
pub fn tf(key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut s = t(key).to_string();
    for (k, v) in args { s = s.replace(&format!("{{{k}}}"), &v.to_string()); }
    s
}

/// A connection failure in the current language. Known IO and auth errors become
/// a sentence; anything else is left as written (it may already be translated).
pub fn describe_io(raw: &str) -> String { describe_io_in(raw, current()) }

fn describe_io_in(raw: &str, lang: Lang) -> String {
    let Some(key) = io_key(raw) else { return raw.to_string() };
    table(lang).get(key).cloned().or_else(|| table(Lang::En).get(key).cloned()).unwrap_or_else(|| raw.to_string())
}

fn io_key(raw: &str) -> Option<&'static str> {
    let l = raw.to_ascii_lowercase();
    if l.contains("connection refused") || l.contains("actively refused") || l.contains("os error 61") || l.contains("os error 111") || l.contains("os error 10061") {
        Some("net.refused")
    } else if l.contains("timed out") || l.contains("timeout") || l.contains("deadline has elapsed") || l.contains("os error 60") || l.contains("os error 10060") {
        Some("net.timeout")
    } else if l.contains("no route to host") || l.contains("network is unreachable") || l.contains("host is unreachable") || l.contains("os error 65") || l.contains("os error 51") {
        Some("net.unreachable")
    } else if l.contains("nodename nor servname") || l.contains("name or service not known") || l.contains("failed to lookup") || l.contains("no such host") || l.contains("temporary failure in name resolution") {
        Some("net.dns")
    } else if l.contains("wrongpass") || l.contains("invalid username-password") || l.contains("invalid password") || l.contains("noauth") || l.contains("authentication required") {
        Some("net.auth")
    } else if l.contains("connection reset") || l.contains("os error 54") || l.contains("os error 104") {
        Some("net.reset")
    } else if l.contains("certificate") || l.contains("unknown issuer") || l.contains("tls") || l.contains("handshake") {
        Some("net.tls")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locales_have_identical_keys() {
        let en = parse(EN_SRC);
        let zh = parse(ZH_SRC);
        let mut only_en: Vec<_> = en.keys().filter(|k| !zh.contains_key(*k)).collect();
        let mut only_zh: Vec<_> = zh.keys().filter(|k| !en.contains_key(*k)).collect();
        only_en.sort(); only_zh.sort();
        assert!(only_en.is_empty() && only_zh.is_empty(), "missing in zh-CN: {only_en:?}\nmissing in en: {only_zh:?}");
        // placeholders must match too
        let re = regex::Regex::new(r"\{[a-z_]+\}").unwrap();
        for (k, v) in &en {
            let mut a: Vec<_> = re.find_iter(v).map(|m| m.as_str()).collect();
            let mut b: Vec<_> = re.find_iter(&zh[k]).map(|m| m.as_str()).collect();
            a.sort(); b.sort();
            assert_eq!(a, b, "placeholder mismatch for {k}");
        }
    }
    #[test]
    fn every_literal_key_in_source_exists() {
        let en = parse(EN_SRC);
        let re = regex::Regex::new(r#"\b(?:t|tf)\("([a-z0-9_.\-]+)""#).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing = vec![];
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().map(|x| x == "rs").unwrap_or(false) {
                let src = std::fs::read_to_string(&p).unwrap();
                for c in re.captures_iter(&src) {
                    if !en.contains_key(&c[1]) { missing.push(format!("{}: {}", p.display(), &c[1])); }
                }
            }
        }
        assert!(missing.is_empty(), "keys used in code but absent from en.toml:\n{}", missing.join("\n"));
    }
    #[test]
    fn connection_failures_follow_the_language() {
        let refused = "Connection refused (os error 61)";
        assert_eq!(describe_io_in(refused, Lang::ZhCn), "连接被拒绝。这个地址和端口上没有服务在听。");
        assert_eq!(describe_io_in(refused, Lang::En), "Connection refused. Nothing is listening on that address and port.");
        assert_eq!(describe_io_in("timeout: deadline has elapsed", Lang::ZhCn), "连接超时。服务器没有在时限内响应。");
        assert_eq!(describe_io_in("WRONGPASS invalid username-password pair", Lang::ZhCn), "认证失败。请检查用户名和密码。");
        assert_eq!(io_key("哨兵模式需要填写主节点名称"), None);
        assert_eq!(describe_io_in("哨兵模式需要填写主节点名称", Lang::ZhCn), "哨兵模式需要填写主节点名称");
    }
    #[test]
    fn fallback_to_english_then_key() {
        assert_eq!(resolve("en"), Lang::En);
        assert_eq!(resolve("zh-CN"), Lang::ZhCn);
        let k = String::from("no.such.key.for.test");
        assert_eq!(t(&k), "no.such.key.for.test");
    }
}
