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
    fn fallback_to_english_then_key() {
        assert_eq!(resolve("en"), Lang::En);
        assert_eq!(resolve("zh-CN"), Lang::ZhCn);
        let k = String::from("no.such.key.for.test");
        assert_eq!(t(&k), "no.such.key.for.test");
    }
}
