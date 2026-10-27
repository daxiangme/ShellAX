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
pub mod fileinfo;
pub mod history;
pub mod num;
pub mod text;
pub mod time;
pub mod pinyin;
pub mod ws;

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
    /// 所属扩展包 id；内置工具为空。
    #[serde(default)]
    pub pack: String,
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

/// 结果的呈现形态。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutKind {
    /// 一段文本，用只读编辑器显示（文本进文本出的工具）。
    #[default]
    Text,
    /// 生成类：每次运行是一批“每条一行”的结果，旧批次保留为历史。
    Rows,
    /// 多格式键值：一个输入对应多行“标签 值”，每次运行替换上一次。
    Kv,
}

/// 标签 / 数值的语气，界面映射到主题色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone { Muted, Danger, Warn, Good, Accent }

/// 结果里的一行。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    /// 左侧标签（可空）。
    pub label: String,
    /// 内容；复制按钮复制的就是它。
    pub value: String,
    /// 行尾小标签（如强度、人数）。
    pub tag: Option<(String, Tone)>,
    /// 悬停提示（如熵值说明）。
    pub hint: String,
    /// 主结果：加大加粗显示。
    pub primary: bool,
    /// 敏感内容（密码）：可被“隐藏”遮住。
    pub secret: bool,
    /// 分组标题行：不可复制。
    pub head: bool,
    /// 内容文字的语气颜色。
    pub tone: Option<Tone>,
}

impl Row {
    pub fn item(value: impl Into<String>) -> Row { Row { value: value.into(), ..Default::default() } }
    pub fn kv(label: impl Into<String>, value: impl Into<String>) -> Row { Row { label: label.into(), value: value.into(), ..Default::default() } }
    pub fn head(title: impl Into<String>) -> Row { Row { value: title.into(), head: true, ..Default::default() } }
    pub fn primary(mut self) -> Row { self.primary = true; self }
    pub fn secret(mut self) -> Row { self.secret = true; self }
    pub fn tag(mut self, text: impl Into<String>, tone: Tone) -> Row { self.tag = Some((text.into(), tone)); self }
    pub fn hint(mut self, h: impl Into<String>) -> Row { self.hint = h.into(); self }
    pub fn tone(mut self, tone: Tone) -> Row { self.tone = Some(tone); self }
}

/// 行转文本的排版。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowFmt {
    /// `标签: 内容`；没有标签时只有内容。
    Colon,
    /// 标签左对齐补到 n 个字符宽，再空一格接内容。
    Pad(usize),
    /// `内容 标签`（单位换算：`1000 m`）。
    Suffix,
}

/// 把行拼成多行文本（复制全部、保存、发送到终端用）。分组标题只产生一个空行分隔。
pub fn rows_text(rows: &[Row], fmt: RowFmt) -> String {
    let mut out: Vec<String> = vec![];
    for r in rows {
        if r.head { if !out.is_empty() { out.push(String::new()); } continue; }
        out.push(if r.label.is_empty() { r.value.clone() } else { match fmt { RowFmt::Colon => format!("{}: {}", r.label, r.value), RowFmt::Pad(n) => format!("{:<n$} {}", r.label, r.value), RowFmt::Suffix => format!("{} {}", r.value, r.label) } });
    }
    out.join("\n")
}

/// 判定横幅：通过 / 失败 / 提示，界面画在输出卡片顶部。
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict { pub tone: Tone, pub text: String }

/// 原文预览：高亮其中的若干字节区间（正则匹配位置）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Source { pub text: String, pub marks: Vec<(usize, usize)> }

/// 运行结果：文本，及可选的颜色色块、二进制文件、按行结果。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    /// 全文（复制全部、保存、发送到终端用）；按行结果时由行拼出。
    pub text: String,
    pub color: Option<[u8; 4]>,
    pub file: Option<(String, Vec<u8>)>,
    pub kind: OutKind,
    pub rows: Vec<Row>,
    /// 批次的参数摘要（`Rows` 形态用）；扩展包的键值回复里也可带一行说明。
    pub meta: String,
    /// 判定横幅（有效 / 无效、匹配数……）。
    pub verdict: Option<Verdict>,
    /// 原文预览与高亮（正则）。
    pub source: Option<Source>,
    /// 第二个结果页的文本（正则的“替换”页）。
    pub alt: Option<String>,
    /// 文件结果的描述（名称、大小、页数、压缩包文件数、预览）。
    pub info: Option<fileinfo::FileInfo>,
    /// 输入文件的总字节数（压缩前后对比用）。
    pub in_size: Option<u64>,
}
impl Output {
    pub fn text(s: impl Into<String>) -> Output { Output { text: s.into(), ..Default::default() } }
    /// 生成类结果：一批，每条一行。
    pub fn rows(rows: Vec<Row>, meta: impl Into<String>) -> Output { Output { text: rows_text(&rows, RowFmt::Colon), kind: OutKind::Rows, rows, meta: meta.into(), ..Default::default() } }
}
impl Output {
    /// 多格式结果：键值行（可有一个主结果）。没有行时退回空文本。
    pub fn kv(rows: Vec<Row>, fmt: RowFmt) -> Output {
        if rows.is_empty() { return Output::default(); }
        Output { text: rows_text(&rows, fmt), kind: OutKind::Kv, rows, ..Default::default() }
    }
}
impl Output {
    /// 带判定横幅；键值形态的横幅文字同时作为全文第一行，复制时也带上。
    pub fn verdict(mut self, tone: Tone, text: impl Into<String>) -> Output {
        let text = text.into();
        // 文本形态的内容就是结果本身（如格式化后的 JSON），不混入横幅
        if self.kind != OutKind::Text { self.text = if self.text.is_empty() { text.clone() } else { format!("{text}\n{}", self.text) }; }
        self.verdict = Some(Verdict { tone, text });
        self
    }
    /// 校验失败：红色横幅 + 若干说明行（没有输出文本）。
    pub fn fail(text: impl Into<String>, rows: Vec<Row>) -> Output {
        let text = text.into();
        Output { text: format!("{text}\n{}", rows_text(&rows, RowFmt::Colon)).trim_end().to_string(), kind: OutKind::Kv, rows, verdict: Some(Verdict { tone: Tone::Danger, text }), ..Default::default() }
    }
}
impl From<String> for Output { fn from(s: String) -> Output { Output::text(s) } }

/// 带会话历史的工具（生成类）：每次运行追加一批，不覆盖旧结果。
pub fn has_history(id: &str) -> bool { matches!(id, "password" | "uuid" | "random_num" | "random_group") }

/// 分类：(id, 文案键, 图标名)。扩展包分类也在这里，以便未安装时也能显示占位。
pub const CATEGORIES: &[(&str, &str)] = &[
    ("unit", "tools.cat_unit"), ("text", "tools.cat_text"), ("dev", "tools.cat_dev"), ("crypto", "tools.cat_crypto"),
    ("time", "tools.cat_time"), ("gen", "tools.cat_gen"), ("lang", "tools.cat_lang"), ("image", "tools.cat_image"), ("pdf", "tools.cat_pdf"),
];

pub fn cat_label(id: &str) -> String {
    CATEGORIES.iter().find(|c| c.0 == id).map(|c| t(c.1).to_string()).unwrap_or_else(|| id.to_string())
}

fn spec(id: &str, cat: &str, kw: &str, inputs: Vec<Field>, options: Vec<Field>) -> ToolSpec {
    ToolSpec { id: id.into(), cat: cat.into(), name: t(&format!("tools.{id}")).to_string(), desc: t(&format!("tools.{id}_desc")).to_string(), keywords: kw.into(), inputs, options, interactive: false, heavy: false, live: true, pack: String::new() }
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
        "units" => Output::kv(num::units_rows(a)?, RowFmt::Suffix),
        "en_amount" => Output::kv(num::en_amount_rows(a.get("input"), a.get("currency"))?, RowFmt::Colon),
        "rmb" => Output::kv(vec![Row::item(num::rmb(a.get("input"))?).primary()], RowFmt::Colon),
        "sum_list" => { let r = num::sum_rows(a.get("input")); if r.is_empty() { num::sum_list(a.get("input")).into() } else { Output::kv(r, RowFmt::Colon) } }
        "loan" => Output::kv(num::loan_rows(a)?, RowFmt::Colon),
        "dedupe" => text::dedupe(a.get("input"), a.on("ignore_case"), a.on("trim"), a.on("drop_empty"), a.on("sort")).into(),
        "trim" => text::trim(a.get("input"), a.get("mode"), a.on("drop_empty")).into(),
        "case" => text::case(a.get("input"), a.get("mode")).into(),
        "plural" => text::plural_lines(a.get("input"), a.get("mode") != "singular").into(),
        "cjk_space" => text::cjk_space(a.get("input")).into(),
        "regex" => text::regex_run(a.get("pattern"), a.get("input"), a.get("replace"), a.on("i"), a.on("m"), a.on("s"))?,
        "id_card" => match text::id_card_check(a.get("input"), chrono::Local::now().date_naive()) {
            Ok((ok, rows)) => Output::kv(rows, RowFmt::Colon).verdict(if ok { Tone::Good } else { Tone::Danger }, if ok { t("tools.id_valid_banner") } else { t("tools.id_invalid_banner") }),
            Err(e) if a.get("input").trim().is_empty() => return Err(e),
            Err(e) => Output::fail(e.to_string(), vec![]),
        },
        "emoji" => text::strip_emoji(a.get("input")).into(),
        "md_html" => text::md_html(a.get("input")).into(),
        "json" => dev::json_output(a.get("input"), a.get("mode") == "minify", a.get("indent"), a.on("sort")),
        "json_ts" => dev::json_ts(a.get("input"), a.get("root"))?.into(),
        "go_json" => if a.get("mode") == "go2json" { dev::go_to_json(a.get("input"))? } else { dev::json_to_go(a.get("input"), a.get("root"))? }.into(),
        "sql" => dev::sql(a.get("input"), a.get("mode") == "minify", a.on("upper")).into(),
        "css" => dev::css(a.get("input"), a.get("mode") == "minify").into(),
        "html" => dev::html(a.get("input"), a.get("mode") == "minify").into(),
        "cron" => match dev::cron_rows(a.get("input"), a.int("count", 1, 100).unwrap_or(10) as usize, chrono::Local::now()) {
            Ok(rows) => Output::kv(rows, RowFmt::Pad(4)).verdict(Tone::Good, t("tools.cron_valid")),
            Err(e) if a.get("input").trim().is_empty() => return Err(e),
            Err(e) => Output::fail(e.to_string(), vec![]),
        },
        "naming" => Output::kv(dev::naming_rows(a.get("input")), RowFmt::Pad(16)),
        "color" => { let (rows, rgba) = dev::color_rows(a.get("input"))?; Output { color: Some(rgba), ..Output::kv(rows, RowFmt::Pad(5)) } }
        "radix" => Output::kv(dev::radix_rows(a.get("input"), a.get("from"))?, RowFmt::Pad(4)),
        "md5" => Output::kv(crypto::md5_rows(a.get("input")), RowFmt::Colon),
        "file_md5" => { let p = a.file("file").ok_or_else(|| anyhow::anyhow!(t("tools.err_need_file")))?; Output::kv(crypto::file_hash_rows(p, a.get("algo"))?, RowFmt::Colon) }
        "sha" => Output::kv(crypto::sha_rows(a.get("input"), a.on("upper")), RowFmt::Pad(8)),
        "hmac" => Output::kv(crypto::hmac_rows(a.get("input"), a.get("key"), a.get("algo"))?, RowFmt::Pad(7)),
        "aes" | "des" | "rc4" | "rabbit" => crypto::cipher(id, a)?.into(),
        "base64" => crypto::base64(a.get("input"), a.get("dir") == "dec", a.on("url"))?.into(),
        "url" => crypto::url(a.get("input"), a.get("dir"))?.into(),
        "unicode" => crypto::unicode(a.get("input"), a.get("dir") == "dec", a.on("ascii")).into(),
        "timestamp" => Output::kv(time::timestamp_rows(a.get("input"), chrono::Local::now())?, RowFmt::Colon),
        "date_calc" => Output::kv(time::date_calc_rows(a)?, RowFmt::Colon),
        "workday" => Output::kv(time::workday_rows(a)?, RowFmt::Colon),
        "batch_time" => match time::batch_rows(a.get("input"), a.get("mode"), a.get("unit") == "ms") {
            // 复制全部仍是只含结果的逐行文本
            Some(rows) => Output { text: time::batch(a.get("input"), a.get("mode"), a.get("unit") == "ms"), kind: OutKind::Kv, rows, ..Default::default() },
            None => time::batch(a.get("input"), a.get("mode"), a.get("unit") == "ms").into(),
        },
        "password" => gen::password_output(a)?,
        "uuid" => gen::uuid_output(a)?,
        "random_num" => gen::number_output(a)?,
        "random_group" => gen::group_output(a)?,
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
    #[test]
    fn kv_outputs() {
        let mut a = Args::default();
        // 颜色：主结果是小写 HEX，色块与全文都在
        a.set("input", "#3b82f6");
        let o = run("color", &a).unwrap();
        assert_eq!(o.kind, OutKind::Kv);
        assert_eq!(o.color, Some([0x3b, 0x82, 0xf6, 255]));
        assert!(o.rows[0].primary && o.rows[0].value == "#3b82f6" && o.text.starts_with("HEX   #3b82f6"), "{}", o.text);
        // 哈希：每行一个算法，复制的是纯哈希
        a.set("input", "abc");
        let o = run("sha", &a).unwrap();
        assert_eq!(o.rows.len(), 5);
        assert_eq!(o.rows[2].label, "SHA-256");
        assert_eq!(o.rows[2].value, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // 单位：全文保持“数值 单位”
        let mut u = Args::default();
        u.set("input", "1").set("kind", "length").set("from", "km");
        let o = run("units", &u).unwrap();
        assert!(o.text.contains("1000 m") && o.rows.iter().any(|r| r.label == "m" && r.value == "1000"), "{}", o.text);
        // 求和：合计为主结果；没有数字时退回文本提示
        a.set("input", "1 2 3");
        let o = run("sum_list", &a).unwrap();
        assert!(o.rows[0].primary && o.rows[0].value == "6");
        a.set("input", "abc");
        assert_eq!(run("sum_list", &a).unwrap().kind, OutKind::Text);
        // 命名：多行输入每组前有分组标题，单行没有
        a.set("input", "user name");
        assert!(!run("naming", &a).unwrap().rows.iter().any(|r| r.head));
        a.set("input", "user name\nfoo bar");
        let o = run("naming", &a).unwrap();
        assert_eq!(o.rows.iter().filter(|r| r.head).count(), 2);
        // 空进制输入没有行
        a.set("input", "").set("from", "auto");
        assert!(run("radix", &a).unwrap().rows.is_empty());
        // 身份证：无效时横幅为危险色，明细行仍在
        a.set("input", "110101199003070000");
        let o = run("id_card", &a).unwrap();
        assert_eq!(o.verdict.as_ref().unwrap().tone, Tone::Danger);
        assert!(!o.rows.is_empty());
    }
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

#[cfg(test)]
mod verdict_tests {
    use super::*;

    fn go(id: &str, kv: &[(&str, &str)]) -> Output {
        let mut a = Args::default();
        for (k, v) in kv { a.set(k, *v); }
        run(id, &a).unwrap()
    }

    #[test]
    fn regex_matches_and_marks() {
        let o = go("regex", &[("pattern", r"(\d+)-(?P<b>\d+)"), ("input", "1-2 x 30-40"), ("replace", "<$1>")]);
        assert_eq!(o.verdict.as_ref().unwrap().tone, Tone::Good);
        assert_eq!(o.kind, OutKind::Kv);
        // 每处匹配一行（主结果）+ 每个捕获组一行
        assert_eq!(o.rows.iter().filter(|r| r.primary).count(), 2);
        assert_eq!(o.rows.len(), 6);
        assert_eq!(o.source.as_ref().unwrap().marks, vec![(0, 3), (6, 11)]);
        assert_eq!(o.alt.as_deref(), Some("<1> x <30>"));
        // 没填替换内容时没有替换页文本
        assert!(go("regex", &[("pattern", "a"), ("input", "b")]).alt.is_none());
    }

    #[test]
    fn regex_no_match_and_invalid() {
        let o = go("regex", &[("pattern", "z"), ("input", "abc")]);
        assert_eq!(o.verdict.as_ref().unwrap().tone, Tone::Warn);
        assert!(o.rows.is_empty() && o.source.is_some());
        let o = go("regex", &[("pattern", "("), ("input", "abc")]);
        assert_eq!(o.verdict.as_ref().unwrap().tone, Tone::Danger);
        assert!(o.source.is_none() && !o.rows.is_empty());
        // 没填表达式仍是输入错误
        let mut a = Args::default();
        a.set("input", "x");
        assert!(run("regex", &a).is_err());
    }

    #[test]
    fn regex_marks_are_cut_at_preview() {
        let big = "ab".repeat(5000);
        let o = go("regex", &[("pattern", "a"), ("input", &big)]);
        let src = o.source.unwrap();
        assert!(src.text.len() <= 4000 && src.marks.iter().all(|m| m.1 <= src.text.len()));
        assert!(o.rows.iter().any(|r| r.value.contains("200")));
    }

    #[test]
    fn id_card_verdict() {
        let ok = go("id_card", &[("input", "11010519491231002X")]);
        assert_eq!(ok.verdict.as_ref().unwrap().tone, Tone::Good);
        assert!(ok.rows.iter().all(|r| !r.primary));
        let bad = go("id_card", &[("input", "110105194912310021")]);
        assert_eq!(bad.verdict.as_ref().unwrap().tone, Tone::Danger);
        assert!(!bad.rows.is_empty());
        let fmt = go("id_card", &[("input", "123")]);
        assert_eq!(fmt.verdict.as_ref().unwrap().tone, Tone::Danger);
        assert!(run("id_card", &Args::default()).is_err());
    }

    #[test]
    fn json_verdict_and_location() {
        let ok = go("json", &[("input", r#"{"a":1}"#), ("mode", "format"), ("indent", "2")]);
        assert_eq!(ok.verdict.as_ref().unwrap().tone, Tone::Good);
        assert!(!ok.text.contains(&ok.verdict.as_ref().unwrap().text), "复制内容不混入横幅");
        let bad = go("json", &[("input", "{\n  \"a\": 1,\n  \"b\": \n}"), ("mode", "format"), ("indent", "2")]);
        let v = bad.verdict.unwrap();
        assert_eq!(v.tone, Tone::Danger);
        assert!(v.text.contains('4'), "{}", v.text);
        // 出错行标红，前后行作为上下文
        assert!(bad.rows.iter().any(|r| r.tone == Some(Tone::Danger)));
        assert!(go("json", &[("input", "  ")]).verdict.is_none());
    }

    #[test]
    fn cron_verdict() {
        assert_eq!(go("cron", &[("input", "*/5 * * * *"), ("count", "3")]).verdict.unwrap().tone, Tone::Good);
        let bad = go("cron", &[("input", "99 * * * *")]);
        assert_eq!(bad.verdict.unwrap().tone, Tone::Danger);
        assert!(run("cron", &Args::default()).is_err());
    }
}
