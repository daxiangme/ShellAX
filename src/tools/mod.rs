//! 工具箱核心：内置小工具的描述（输入、选项）与纯函数实现，不依赖界面。
//! 界面层按 `ToolSpec` 渲染输入卡片、选项与输出卡片；扩展包（WebAssembly）用同一套描述。
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::i18n::t;

pub mod crypto;
pub mod dev;
pub mod gen;
pub mod num;
pub mod text;
pub mod time;
pub mod pinyin;

/// 字段类型：输入与选项共用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind { Text, Multiline, File, Files, Select, Toggle, Number }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub default: String,
    /// Select 的 (值, 显示文本)。
    #[serde(default)]
    pub choices: Vec<(String, String)>,
    #[serde(default)]
    pub placeholder: String,
}

impl Field {
    fn new(id: &str, kind: FieldKind, label: &str) -> Field { Field { id: id.into(), label: label.into(), kind, default: String::new(), choices: vec![], placeholder: String::new() } }
    pub fn text(id: &str, label: &str) -> Field { Field::new(id, FieldKind::Text, label) }
    pub fn multiline(id: &str, label: &str) -> Field { Field::new(id, FieldKind::Multiline, label) }
    pub fn file(id: &str, label: &str) -> Field { Field::new(id, FieldKind::File, label) }
    pub fn number(id: &str, label: &str, default: &str) -> Field { Field::new(id, FieldKind::Number, label).with_default(default) }
    pub fn toggle(id: &str, label: &str, on: bool) -> Field { Field::new(id, FieldKind::Toggle, label).with_default(if on { "true" } else { "false" }) }
    /// 下拉；`choices` 为 (值, 文案键)，文案键经 `t()` 翻译，找不到时原样显示。
    pub fn select(id: &str, label: &str, choices: &[(&str, &str)]) -> Field {
        let ch: Vec<(String, String)> = choices.iter().map(|(v, l)| (v.to_string(), t(l).to_string())).collect();
        let d = ch.first().map(|c| c.0.clone()).unwrap_or_default();
        Field { choices: ch, ..Field::new(id, FieldKind::Select, label) }.with_default(&d)
    }
    pub fn with_default(mut self, d: &str) -> Field { self.default = d.into(); self }
    pub fn ph(mut self, p: &str) -> Field { self.placeholder = p.into(); self }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub id: String,
    pub cat: String,
    pub name: String,
    #[serde(default)]
    pub desc: String,
    /// 额外的搜索词（英文名、别名）。
    #[serde(default)]
    pub keywords: String,
    #[serde(default)]
    pub inputs: Vec<Field>,
    #[serde(default)]
    pub options: Vec<Field>,
    /// 交互式工具（如 WebSocket）由界面层单独实现。
    #[serde(default)]
    pub interactive: bool,
    /// 运行较慢（读大文件等），放到后台执行。
    #[serde(default)]
    pub heavy: bool,
    /// 输入变化时自动运行。
    #[serde(default)]
    pub live: bool,
}

/// 运行参数：字段 id → 文本值；文件字段为路径列表。
#[derive(Clone, Debug, Default)]
pub struct Args { pub values: HashMap<String, String>, pub files: HashMap<String, Vec<PathBuf>> }

impl Args {
    pub fn get(&self, id: &str) -> &str { self.values.get(id).map(|s| s.as_str()).unwrap_or("") }
    pub fn on(&self, id: &str) -> bool { self.get(id) == "true" }
    pub fn num(&self, id: &str) -> Result<f64> {
        let s = self.get(id).trim().replace([',', '，', '_'], "");
        if s.is_empty() { bail!(crate::i18n::tf("tools.err_need_number", &[("f", &id)])); }
        s.parse::<f64>().map_err(|_| anyhow::anyhow!(crate::i18n::tf("tools.err_bad_number", &[("v", &s)])))
    }
    pub fn int(&self, id: &str, min: i64, max: i64) -> Result<i64> { Ok((self.num(id)?.round() as i64).clamp(min, max)) }
    pub fn file(&self, id: &str) -> Option<&PathBuf> { self.files.get(id).and_then(|v| v.first()) }
    pub fn set(&mut self, id: &str, v: impl Into<String>) -> &mut Self { self.values.insert(id.into(), v.into()); self }
}

/// 运行结果：文本，及可选的颜色色块、二进制文件。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output { pub text: String, pub color: Option<[u8; 4]>, pub file: Option<(String, Vec<u8>)> }
impl Output { pub fn text(s: impl Into<String>) -> Output { Output { text: s.into(), ..Default::default() } } }
impl From<String> for Output { fn from(s: String) -> Output { Output::text(s) } }

/// 分类：(id, 文案键, 图标名)。扩展包分类也在这里，以便未安装时也能显示占位。
pub const CATEGORIES: &[(&str, &str)] = &[
    ("unit", "tools.cat_unit"), ("text", "tools.cat_text"), ("dev", "tools.cat_dev"), ("crypto", "tools.cat_crypto"),
    ("time", "tools.cat_time"), ("gen", "tools.cat_gen"), ("lang", "tools.cat_lang"), ("image", "tools.cat_image"), ("pdf", "tools.cat_pdf"),
];

pub fn cat_label(id: &str) -> String {
    CATEGORIES.iter().find(|c| c.0 == id).map(|c| t(c.1).to_string()).unwrap_or_else(|| id.to_string())
}

fn spec(id: &str, cat: &str, kw: &str, inputs: Vec<Field>, options: Vec<Field>) -> ToolSpec {
    ToolSpec { id: id.into(), cat: cat.into(), name: t(&format!("tools.{id}")).to_string(), desc: t(&format!("tools.{id}_desc")).to_string(), keywords: kw.into(), inputs, options, interactive: false, heavy: false, live: true }
}

/// 全部内置工具，按分类顺序。每次调用按当前语言生成文案（只在打开工具面板时调用）。
pub fn builtin() -> Vec<ToolSpec> {
    use Field as F;
    let input = || F::multiline("input", t("tools.f_input"));
    let enc_out = [("base64", "Base64"), ("hex", "Hex")];
    let dir = [("enc", "tools.o_encrypt"), ("dec", "tools.o_decrypt")];
    let keymode = [("pass", "tools.o_key_pass"), ("utf8", "tools.o_key_utf8"), ("hex", "tools.o_key_hex")];
    let mut v = vec![
        // 单位与财务
        spec("units", "unit", "unit convert length mass temperature bytes", vec![F::text("input", t("tools.f_value")).with_default("1")],
            vec![F::select("kind", t("tools.f_kind"), &num::UNIT_KINDS.iter().map(|k| (k.0, k.1)).collect::<Vec<_>>()), F::text("from", t("tools.f_from_unit")).with_default("m")]),
        spec("en_amount", "unit", "english amount words money check", vec![F::text("input", t("tools.f_amount"))], vec![F::select("currency", t("tools.f_currency"), &[("USD", "USD"), ("RMB", "RMB"), ("EUR", "EUR"), ("HKD", "HKD"), ("", "tools.o_none")])]),
        spec("rmb", "unit", "rmb uppercase daxie", vec![F::text("input", t("tools.f_amount"))], vec![]),
        spec("sum_list", "unit", "sum average total list", vec![input()], vec![]),
        spec("loan", "unit", "loan mortgage monthly payment", vec![],
            vec![F::number("principal", t("tools.f_principal"), "1000000"), F::number("rate", t("tools.f_rate"), "3.5"), F::number("years", t("tools.f_years"), "30"),
                 F::select("method", t("tools.f_method"), &[("annuity", "tools.o_annuity"), ("linear", "tools.o_linear")])]),
        // 文本
        spec("dedupe", "text", "dedupe unique lines", vec![input()], vec![F::toggle("ignore_case", t("tools.o_ignore_case"), false), F::toggle("trim", t("tools.o_trim"), true), F::toggle("drop_empty", t("tools.o_drop_empty"), true), F::toggle("sort", t("tools.o_sort"), false)]),
        spec("trim", "text", "trim whitespace spaces", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("both", "tools.o_trim_both"), ("left", "tools.o_trim_left"), ("right", "tools.o_trim_right"), ("all", "tools.o_trim_all"), ("collapse", "tools.o_trim_collapse")]), F::toggle("drop_empty", t("tools.o_drop_empty"), false)]),
        spec("case", "text", "upper lower title sentence case", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("upper", "tools.o_upper"), ("lower", "tools.o_lower"), ("title", "tools.o_title"), ("sentence", "tools.o_sentence")])]),
        spec("plural", "text", "plural singular english", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("plural", "tools.o_to_plural"), ("singular", "tools.o_to_singular")])]),
        spec("cjk_space", "text", "spacing chinese english", vec![input()], vec![]),
        spec("regex", "text", "regex regexp match replace", vec![F::text("pattern", t("tools.f_pattern")), input(), F::text("replace", t("tools.f_replace"))],
            vec![F::toggle("i", t("tools.o_ignore_case"), false), F::toggle("m", t("tools.o_multiline"), false), F::toggle("s", t("tools.o_dotall"), false)]),
        spec("id_card", "text", "id card shenfenzheng", vec![F::text("input", t("tools.f_id"))], vec![]),
        spec("emoji", "text", "emoji remove clean", vec![input()], vec![]),
        spec("md_html", "text", "markdown html", vec![input()], vec![]),
        // 开发
        spec("json", "dev", "json format minify validate", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("pretty", "tools.o_pretty"), ("minify", "tools.o_minify")]), F::select("indent", t("tools.f_indent"), &[("2", "2"), ("4", "4"), ("tab", "Tab")]), F::toggle("sort", t("tools.o_sort_keys"), false)]),
        spec("json_ts", "dev", "json typescript interface types", vec![input()], vec![F::text("root", t("tools.f_root")).with_default("Root")]),
        spec("go_json", "dev", "golang struct json", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("json2go", "JSON → Go"), ("go2json", "Go → JSON")]), F::text("root", t("tools.f_root")).with_default("Root")]),
        spec("sql", "dev", "sql format minify", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("pretty", "tools.o_pretty"), ("minify", "tools.o_minify")]), F::toggle("upper", t("tools.o_upper_kw"), true)]),
        spec("css", "dev", "css format minify", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("pretty", "tools.o_pretty"), ("minify", "tools.o_minify")])]),
        spec("html", "dev", "html format minify", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("pretty", "tools.o_pretty"), ("minify", "tools.o_minify")])]),
        spec("cron", "dev", "crontab cron schedule next", vec![F::text("input", t("tools.f_cron")).with_default("*/5 * * * *")], vec![F::number("count", t("tools.f_count"), "10")]),
        spec("naming", "dev", "camel snake kebab pascal naming case", vec![input()], vec![]),
        spec("color", "dev", "color hex rgb hsl", vec![F::text("input", t("tools.f_color")).with_default("#3b82f6")], vec![]),
        spec("radix", "dev", "binary octal hex decimal base radix", vec![F::text("input", t("tools.f_value"))], vec![F::select("from", t("tools.f_from_base"), &[("auto", "tools.o_auto"), ("2", "2"), ("8", "8"), ("10", "10"), ("16", "16"), ("36", "36")])]),
        ToolSpec { interactive: true, live: false, ..spec("websocket", "dev", "websocket ws wss test", vec![], vec![]) },
        // 加密编码
        spec("md5", "crypto", "md5 hash", vec![input()], vec![]),
        ToolSpec { heavy: true, live: false, ..spec("file_md5", "crypto", "file md5 sha checksum hash", vec![F::file("file", t("tools.f_file"))], vec![F::select("algo", t("tools.f_algo"), &[("md5", "MD5"), ("sha1", "SHA-1"), ("sha256", "SHA-256"), ("sha512", "SHA-512")])]) },
        spec("sha", "crypto", "sha1 sha256 sha512 hash", vec![input()], vec![F::toggle("upper", t("tools.o_uppercase"), false)]),
        spec("hmac", "crypto", "hmac hash", vec![input(), F::text("key", t("tools.f_key"))], vec![F::select("algo", t("tools.f_algo"), &[("sha256", "SHA-256"), ("sha1", "SHA-1"), ("sha512", "SHA-512"), ("sha384", "SHA-384"), ("sha224", "SHA-224"), ("md5", "MD5")])]),
        spec("aes", "crypto", "aes encrypt decrypt", vec![input(), F::text("key", t("tools.f_key")), F::text("iv", t("tools.f_iv"))],
            vec![F::select("dir", t("tools.f_dir"), &dir), F::select("keymode", t("tools.f_keymode"), &keymode), F::select("mode", t("tools.f_cipher_mode"), &[("cbc", "CBC"), ("ecb", "ECB"), ("ctr", "CTR"), ("cfb", "CFB"), ("ofb", "OFB")]),
                 F::select("pad", t("tools.f_padding"), &[("pkcs7", "PKCS7"), ("zero", "ZeroPadding"), ("none", "NoPadding")]), F::select("enc", t("tools.f_cipher_enc"), &enc_out)]),
        spec("des", "crypto", "des 3des tripledes encrypt decrypt", vec![input(), F::text("key", t("tools.f_key")), F::text("iv", t("tools.f_iv"))],
            vec![F::select("algo", t("tools.f_algo"), &[("des", "DES"), ("3des", "3DES")]), F::select("dir", t("tools.f_dir"), &dir), F::select("keymode", t("tools.f_keymode"), &keymode), F::select("mode", t("tools.f_cipher_mode"), &[("cbc", "CBC"), ("ecb", "ECB")]),
                 F::select("pad", t("tools.f_padding"), &[("pkcs7", "PKCS7"), ("zero", "ZeroPadding"), ("none", "NoPadding")]), F::select("enc", t("tools.f_cipher_enc"), &enc_out)]),
        spec("rc4", "crypto", "rc4 arc4 encrypt decrypt", vec![input(), F::text("key", t("tools.f_key"))], vec![F::select("dir", t("tools.f_dir"), &dir), F::select("keymode", t("tools.f_keymode"), &keymode), F::select("enc", t("tools.f_cipher_enc"), &enc_out)]),
        spec("rabbit", "crypto", "rabbit stream cipher encrypt decrypt", vec![input(), F::text("key", t("tools.f_key")), F::text("iv", t("tools.f_iv"))], vec![F::select("dir", t("tools.f_dir"), &dir), F::select("keymode", t("tools.f_keymode"), &keymode), F::select("enc", t("tools.f_cipher_enc"), &enc_out)]),
        spec("base64", "crypto", "base64 encode decode", vec![input()], vec![F::select("dir", t("tools.f_dir"), &[("enc", "tools.o_encode"), ("dec", "tools.o_decode")]), F::toggle("url", t("tools.o_url_safe"), false)]),
        spec("url", "crypto", "url encode decode percent", vec![input()], vec![F::select("dir", t("tools.f_dir"), &[("enc", "tools.o_encode"), ("dec", "tools.o_decode"), ("enc_uri", "tools.o_encode_uri")])]),
        spec("unicode", "crypto", "unicode escape chinese", vec![input()], vec![F::select("dir", t("tools.f_dir"), &[("enc", "tools.o_encode"), ("dec", "tools.o_decode")]), F::toggle("ascii", t("tools.o_escape_ascii"), false)]),
        // 日期时间
        spec("timestamp", "time", "timestamp unix epoch date", vec![F::text("input", t("tools.f_time_or_ts")).ph(t("tools.ph_now"))], vec![]),
        spec("date_calc", "time", "date add subtract diff interval", vec![F::text("a", t("tools.f_date_a")), F::text("b", t("tools.f_date_b"))],
            vec![F::select("mode", t("tools.f_mode"), &[("diff", "tools.o_diff"), ("add", "tools.o_add")]), F::number("n", t("tools.f_amount_n"), "7"), F::select("unit", t("tools.f_unit"), &[("day", "tools.o_day"), ("week", "tools.o_week"), ("month", "tools.o_month"), ("year", "tools.o_year"), ("hour", "tools.o_hour")])]),
        spec("workday", "time", "workday business day holiday", vec![F::text("start", t("tools.f_start_date")), F::multiline("holidays", t("tools.f_holidays")), F::multiline("workdays", t("tools.f_extra_workdays"))], vec![F::number("n", t("tools.f_workdays_n"), "5")]),
        spec("batch_time", "time", "batch timestamp convert lines log", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("auto", "tools.o_auto"), ("to_date", "tools.o_to_date"), ("to_ts", "tools.o_to_ts")]), F::select("unit", t("tools.f_ts_unit"), &[("s", "tools.o_seconds"), ("ms", "tools.o_millis")])]),
        // 生成
        ToolSpec { live: false, ..spec("password", "gen", "password random generate", vec![], vec![F::number("len", t("tools.f_length"), "16"), F::number("count", t("tools.f_count"), "5"), F::toggle("upper", "A-Z", true), F::toggle("lower", "a-z", true), F::toggle("digit", "0-9", true), F::toggle("symbol", "!@#$", true), F::toggle("no_ambiguous", t("tools.o_no_ambiguous"), false)]) },
        ToolSpec { live: false, ..spec("uuid", "gen", "uuid guid generate", vec![], vec![F::number("count", t("tools.f_count"), "10"), F::toggle("upper", t("tools.o_uppercase"), false), F::toggle("hyphen", t("tools.o_hyphen"), true)]) },
        ToolSpec { live: false, ..spec("random_num", "gen", "random number", vec![], vec![F::number("min", t("tools.f_min"), "1"), F::number("max", t("tools.f_max"), "100"), F::number("count", t("tools.f_count"), "10"), F::number("decimals", t("tools.f_decimals"), "0"), F::toggle("unique", t("tools.o_unique"), false), F::toggle("sort", t("tools.o_sort"), false)]) },
        ToolSpec { live: false, ..spec("random_group", "gen", "random group shuffle pick name", vec![input()], vec![F::select("mode", t("tools.f_mode"), &[("groups", "tools.o_by_groups"), ("size", "tools.o_by_size"), ("pick", "tools.o_pick_one")]), F::number("n", t("tools.f_n"), "2")]) },
    ];
    for s in &mut v { if s.id == "units" { s.options[1].placeholder = "m, km, kg, °C, MB …".into(); } }
    v
}

/// 运行内置工具。
pub fn run(id: &str, a: &Args) -> Result<Output> {
    Ok(match id {
        "units" => num::units(a)?.into(),
        "en_amount" => num::en_amount(a.get("input"), a.get("currency"))?.into(),
        "rmb" => num::rmb(a.get("input"))?.into(),
        "sum_list" => num::sum_list(a.get("input")).into(),
        "loan" => num::loan(a)?.into(),
        "dedupe" => text::dedupe(a.get("input"), a.on("ignore_case"), a.on("trim"), a.on("drop_empty"), a.on("sort")).into(),
        "trim" => text::trim(a.get("input"), a.get("mode"), a.on("drop_empty")).into(),
        "case" => text::case(a.get("input"), a.get("mode")).into(),
        "plural" => text::plural_lines(a.get("input"), a.get("mode") != "singular").into(),
        "cjk_space" => text::cjk_space(a.get("input")).into(),
        "regex" => text::regex_test(a.get("pattern"), a.get("input"), a.get("replace"), a.on("i"), a.on("m"), a.on("s"))?.into(),
        "id_card" => text::id_card(a.get("input"), chrono::Local::now().date_naive())?.into(),
        "emoji" => text::strip_emoji(a.get("input")).into(),
        "md_html" => text::md_html(a.get("input")).into(),
        "json" => dev::json(a.get("input"), a.get("mode") == "minify", a.get("indent"), a.on("sort"))?.into(),
        "json_ts" => dev::json_ts(a.get("input"), a.get("root"))?.into(),
        "go_json" => if a.get("mode") == "go2json" { dev::go_to_json(a.get("input"))? } else { dev::json_to_go(a.get("input"), a.get("root"))? }.into(),
        "sql" => dev::sql(a.get("input"), a.get("mode") == "minify", a.on("upper")).into(),
        "css" => dev::css(a.get("input"), a.get("mode") == "minify").into(),
        "html" => dev::html(a.get("input"), a.get("mode") == "minify").into(),
        "cron" => dev::cron_next(a.get("input"), a.int("count", 1, 100).unwrap_or(10) as usize, chrono::Local::now())?.into(),
        "naming" => dev::naming(a.get("input")).into(),
        "color" => { let (s, rgba) = dev::color(a.get("input"))?; Output { text: s, color: Some(rgba), file: None } }
        "radix" => dev::radix(a.get("input"), a.get("from"))?.into(),
        "md5" => crypto::md5_text(a.get("input")).into(),
        "file_md5" => { let p = a.file("file").ok_or_else(|| anyhow::anyhow!(t("tools.err_need_file")))?; crypto::file_hash(p, a.get("algo"))?.into() }
        "sha" => crypto::sha_all(a.get("input"), a.on("upper")).into(),
        "hmac" => crypto::hmac(a.get("input"), a.get("key"), a.get("algo"))?.into(),
        "aes" | "des" | "rc4" | "rabbit" => crypto::cipher(id, a)?.into(),
        "base64" => crypto::base64(a.get("input"), a.get("dir") == "dec", a.on("url"))?.into(),
        "url" => crypto::url(a.get("input"), a.get("dir"))?.into(),
        "unicode" => crypto::unicode(a.get("input"), a.get("dir") == "dec", a.on("ascii")).into(),
        "timestamp" => time::timestamp(a.get("input"), chrono::Local::now())?.into(),
        "date_calc" => time::date_calc(a)?.into(),
        "workday" => time::workday(a)?.into(),
        "batch_time" => time::batch(a.get("input"), a.get("mode"), a.get("unit") == "ms").into(),
        "password" => gen::passwords(a)?.into(),
        "uuid" => gen::uuids(a.int("count", 1, 1000)? as usize, a.on("upper"), a.on("hyphen")).into(),
        "random_num" => gen::random_numbers(a)?.into(),
        "random_group" => gen::random_group(a.get("input"), a.get("mode"), a.int("n", 1, 10000)? as usize)?.into(),
        _ => bail!(crate::i18n::tf("tools.err_unknown", &[("id", &id)])),
    })
}

/// 默认参数（各字段的 default）。
pub fn default_args(s: &ToolSpec) -> Args {
    let mut a = Args::default();
    for f in s.inputs.iter().chain(&s.options) { a.values.insert(f.id.clone(), f.default.clone()); }
    a
}

/// 搜索匹配：名称、id、关键字、中文名拼音首字母。
pub fn matches(s: &ToolSpec, q: &str) -> bool {
    let q = q.trim().to_lowercase();
    if q.is_empty() { return true; }
    let hay = format!("{} {} {} {}", s.name.to_lowercase(), s.id.replace('_', "-"), s.keywords, s.desc.to_lowercase());
    if q.split_whitespace().all(|w| hay.contains(w)) { return true; }
    let ini = pinyin::initials(&s.name);
    !ini.is_empty() && ini.contains(&q.replace(' ', ""))
}

/// 终端右键"用工具打开"支持的工具，按选中文本猜一个最合适的放第一位。
pub fn terminal_targets(sel: &str) -> Vec<&'static str> {
    let s = sel.trim();
    let mut v = vec!["json", "timestamp", "base64", "url", "cron"];
    let first = if (s.starts_with('{') || s.starts_with('[')) && serde_json::from_str::<serde_json::Value>(s).is_ok() { "json" }
        else if s.len() >= 9 && s.len() <= 13 && s.chars().all(|c| c.is_ascii_digit()) { "timestamp" }
        else if s.contains('%') && s.contains(|c: char| c.is_ascii_hexdigit()) { "url" }
        else if (5..=7).contains(&s.split_whitespace().count()) && s.chars().all(|c| c.is_ascii_digit() || " */,-?LW#".contains(c) || c.is_ascii_uppercase()) { "cron" }
        else if s.len() >= 8 && s.chars().all(|c| c.is_ascii_alphanumeric() || "+/=-_".contains(c)) { "base64" }
        else { "json" };
    v.retain(|x| *x != first);
    v.insert(0, first);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_complete() {
        let all = builtin();
        assert_eq!(all.len(), 44);
        let mut ids: Vec<_> = all.iter().map(|s| s.id.clone()).collect();
        ids.sort(); ids.dedup();
        assert_eq!(ids.len(), 44, "ids unique");
        for s in &all {
            assert!(CATEGORIES.iter().any(|c| c.0 == s.cat), "{}", s.id);
            assert!(!s.name.starts_with("tools."), "missing name {}", s.id);
            if s.interactive { continue; }
            // 默认参数下运行不能 panic（可以报错）
            let _ = run(&s.id, &default_args(s));
        }
    }
    #[test]
    fn search() {
        let all = builtin();
        let json = all.iter().find(|s| s.id == "json").unwrap();
        assert!(matches(json, "JSON"));
        assert!(matches(json, "minify"));
        assert!(!matches(json, "uuid"));
        let rmb = all.iter().find(|s| s.id == "rmb").unwrap();
        assert!(matches(rmb, "rmb"));
    }
    #[test]
    fn terminal_guess() {
        assert_eq!(terminal_targets("{\"a\":1}")[0], "json");
        assert_eq!(terminal_targets("1700000000")[0], "timestamp");
        assert_eq!(terminal_targets("a%20b")[0], "url");
        assert_eq!(terminal_targets("*/5 * * * *")[0], "cron");
        assert_eq!(terminal_targets("aGVsbG8gd29ybGQ=")[0], "base64");
        assert_eq!(terminal_targets("x").len(), 5);
    }
}

#[cfg(test)]
mod i18n_tests {
    /// src/tools/ 下所有字面量 key 都必须在两种语言里存在。
    #[test]
    fn keys_exist() {
        let en = include_str!("../../locales/en.toml");
        let zh = include_str!("../../locales/zh-CN.toml");
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tools");
        let mut missing = vec![];
        for e in std::fs::read_dir(dir).unwrap() {
            let src = std::fs::read_to_string(e.unwrap().path()).unwrap();
            for part in src.split("\"tools.").skip(1) {
                let Some(end) = part.find('"') else { continue };
                let k = &part[..end];
                if k.is_empty() || k.contains(['{', ' ']) || k.ends_with('_') { continue; }
                let line = format!("\"tools.{k}\" =");
                if !en.contains(&line) || !zh.contains(&line) { missing.push(k.to_string()); }
            }
        }
        for s in super::builtin() {
            for k in [format!("\"tools.{}\" =", s.id), format!("\"tools.{}_desc\" =", s.id)] {
                if !en.contains(&k) { missing.push(k); }
            }
        }
        missing.sort(); missing.dedup();
        assert!(missing.is_empty(), "missing: {missing:?}");
    }
}
