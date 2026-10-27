//! 文本类工具。
use anyhow::{bail, Result};

use super::{rows_text, Row, RowFmt, Tone};
use crate::i18n::{t, tf};

pub fn dedupe(s: &str, ignore_case: bool, trim: bool, drop_empty: bool, sort: bool) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = vec![];
    let total = s.lines().count();
    for l in s.lines() {
        let l = if trim { l.trim().to_string() } else { l.to_string() };
        if drop_empty && l.trim().is_empty() { continue; }
        let key = if ignore_case { l.to_lowercase() } else { l.clone() };
        if seen.insert(key) { out.push(l); }
    }
    if sort { out.sort_by_key(|a| a.to_lowercase()); }
    let _ = total;
    out.join("\n")
}

pub fn trim(s: &str, mode: &str, drop_empty: bool) -> String {
    let ws = regex::Regex::new(r"[ \t\u{3000}]+").unwrap();
    s.lines().map(|l| match mode {
        "left" => l.trim_start().to_string(),
        "right" => l.trim_end().to_string(),
        "all" => l.chars().filter(|c| !c.is_whitespace()).collect(),
        "collapse" => ws.replace_all(l.trim(), " ").into_owned(),
        _ => l.trim().to_string(),
    }).filter(|l| !drop_empty || !l.is_empty()).collect::<Vec<_>>().join("\n")
}

fn title_word(w: &str) -> String {
    let mut c = w.chars();
    match c.next() { Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(), None => String::new() }
}

pub fn case(s: &str, mode: &str) -> String {
    match mode {
        "upper" => s.to_uppercase(),
        "lower" => s.to_lowercase(),
        "title" => {
            let small = ["a", "an", "the", "and", "but", "or", "for", "nor", "on", "at", "to", "by", "of", "in", "with"];
            s.lines().map(|l| {
                let mut first = true;
                l.split(' ').map(|w| { let r = if !first && small.contains(&w.to_lowercase().as_str()) { w.to_lowercase() } else { title_word(w) }; if !w.is_empty() { first = false; } r }).collect::<Vec<_>>().join(" ")
            }).collect::<Vec<_>>().join("\n")
        }
        _ => {
            // 句首大写，其余小写
            let mut out = String::with_capacity(s.len());
            let mut cap = true;
            for ch in s.to_lowercase().chars() {
                if cap && ch.is_alphabetic() { out.extend(ch.to_uppercase()); cap = false; } else { out.push(ch); }
                if matches!(ch, '.' | '!' | '?' | '\n' | '。' | '！' | '？') { cap = true; }
            }
            out
        }
    }
}

const IRREGULAR: &[(&str, &str)] = &[
    ("person", "people"), ("man", "men"), ("woman", "women"), ("child", "children"), ("tooth", "teeth"), ("foot", "feet"), ("mouse", "mice"), ("goose", "geese"),
    ("ox", "oxen"), ("datum", "data"), ("medium", "media"), ("criterion", "criteria"), ("phenomenon", "phenomena"), ("analysis", "analyses"), ("index", "indices"),
    ("matrix", "matrices"), ("vertex", "vertices"), ("cactus", "cacti"), ("focus", "foci"), ("fungus", "fungi"), ("nucleus", "nuclei"), ("syllabus", "syllabi"),
    ("thesis", "theses"), ("crisis", "crises"), ("die", "dice"), ("leaf", "leaves"), ("life", "lives"), ("knife", "knives"), ("wife", "wives"), ("half", "halves"),
    ("wolf", "wolves"), ("shelf", "shelves"), ("potato", "potatoes"), ("tomato", "tomatoes"), ("hero", "heroes"), ("echo", "echoes"), ("quiz", "quizzes"),
];
const UNCOUNTABLE: &[&str] = &["sheep", "fish", "deer", "series", "species", "money", "rice", "information", "equipment", "news", "software", "hardware", "feedback", "police", "aircraft"];

fn keep_case(src: &str, w: String) -> String {
    if src.len() > 1 && src.chars().all(|c| !c.is_lowercase()) { w.to_uppercase() } else if src.chars().next().is_some_and(|c| c.is_uppercase()) { title_word(&w) } else { w }
}

pub fn pluralize(word: &str) -> String {
    let w = word.to_lowercase();
    if w.is_empty() || UNCOUNTABLE.contains(&w.as_str()) { return word.to_string(); }
    if let Some((_, p)) = IRREGULAR.iter().find(|(s, _)| *s == w) { return keep_case(word, p.to_string()); }
    if IRREGULAR.iter().any(|(_, p)| *p == w) { return word.to_string(); }
    let v = |c: char| "aeiou".contains(c);
    let r = if w.ends_with('y') && w.len() > 1 && !v(w.chars().rev().nth(1).unwrap()) { format!("{}ies", &w[..w.len() - 1]) }
        else if ["s", "x", "z", "ch", "sh"].iter().any(|e| w.ends_with(e)) { format!("{w}es") }
        else if w.ends_with("fe") { format!("{}ves", &w[..w.len() - 2]) }
        else { format!("{w}s") };
    keep_case(word, r)
}

pub fn singularize(word: &str) -> String {
    let w = word.to_lowercase();
    if w.is_empty() || UNCOUNTABLE.contains(&w.as_str()) { return word.to_string(); }
    if let Some((s, _)) = IRREGULAR.iter().find(|(_, p)| *p == w) { return keep_case(word, s.to_string()); }
    if IRREGULAR.iter().any(|(s, _)| *s == w) { return word.to_string(); }
    let r = if w.ends_with("ies") && w.len() > 3 { format!("{}y", &w[..w.len() - 3]) }
        else if w.ends_with("ves") { format!("{}fe", &w[..w.len() - 3]) }
        else if ["sses", "xes", "zes", "ches", "shes"].iter().any(|e| w.ends_with(e)) { w[..w.len() - 2].to_string() }
        else if w.ends_with("ss") || w.ends_with("us") || w.ends_with("is") { w.clone() }
        else if let Some(x) = w.strip_suffix('s') { x.to_string() }
        else { w.clone() };
    keep_case(word, r)
}

pub fn plural_lines(s: &str, plural: bool) -> String {
    let re = regex::Regex::new(r"[A-Za-z]+").unwrap();
    re.replace_all(s, |c: &regex::Captures| if plural { pluralize(&c[0]) } else { singularize(&c[0]) }).into_owned()
}

fn is_cjk(c: char) -> bool { matches!(c as u32, 0x2E80..=0x2FDF | 0x3040..=0x30FF | 0x3100..=0x312F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF) }
fn is_latin(c: char) -> bool { c.is_ascii_alphanumeric() || matches!(c, '@' | '$' | '%' | '#' | '&' | '+' | '`') }

/// 在中文与英文、数字之间补空格。
pub fn cjk_space(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut prev: Option<char> = None;
    for ch in s.chars() {
        if let Some(p) = prev {
            if (is_cjk(p) && is_latin(ch)) || (is_latin(p) && is_cjk(ch)) { out.push(' '); }
        }
        out.push(ch);
        prev = Some(ch);
    }
    out
}

pub fn regex_test(pat: &str, text: &str, replace: &str, i: bool, m: bool, s: bool) -> Result<String> {
    if pat.is_empty() { bail!(t("tools.err_need_pattern")); }
    let re = regex::RegexBuilder::new(pat).case_insensitive(i).multi_line(m).dot_matches_new_line(s).build()?;
    if !replace.is_empty() { return Ok(re.replace_all(text, replace).into_owned()); }
    let mut out = vec![];
    let names: Vec<Option<&str>> = re.capture_names().collect();
    for (n, c) in re.captures_iter(text).enumerate().take(1000) {
        let m0 = c.get(0).unwrap();
        out.push(format!("#{} [{}..{}] {}", n + 1, m0.start(), m0.end(), m0.as_str()));
        for (gi, g) in c.iter().enumerate().skip(1) {
            let label = names[gi].map(|s| s.to_string()).unwrap_or_else(|| gi.to_string());
            out.push(format!("    ${label}: {}", g.map(|x| x.as_str()).unwrap_or("—")));
        }
    }
    Ok(if out.is_empty() { t("tools.no_match").to_string() } else { format!("{}\n{}", tf("tools.n_matches", &[("n", &re.find_iter(text).count())]), out.join("\n")) })
}

/// 大陆 18 位身份证号校验。
pub fn id_card(s: &str, today: chrono::NaiveDate) -> Result<String> { Ok(rows_text(&id_card_rows(s, today)?, RowFmt::Colon)) }

/// 校验结果（主结果，有效绿色/无效红色）+ 校验位、地区、生日、年龄、性别。
pub fn id_card_rows(s: &str, today: chrono::NaiveDate) -> Result<Vec<Row>> {
    use chrono::Datelike;
    let id: String = s.trim().to_uppercase().chars().filter(|c| !c.is_whitespace()).collect();
    if id.len() != 18 || !id[..17].chars().all(|c| c.is_ascii_digit()) || !id[17..].chars().all(|c| c.is_ascii_digit() || c == 'X') { bail!(t("tools.id_bad_format")); }
    const W: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const C: &[u8; 11] = b"10X98765432";
    let sum: u32 = id.bytes().take(17).zip(W).map(|(b, w)| (b - b'0') as u32 * w).sum();
    let expect = C[(sum % 11) as usize] as char;
    let ok = id.ends_with(expect);
    let birth = chrono::NaiveDate::parse_from_str(&id[6..14], "%Y%m%d").ok().filter(|d| *d <= today && d.year() >= 1900);
    let male = (id.as_bytes()[16] - b'0') % 2 == 1;
    let valid = ok && birth.is_some();
    let mut lines = vec![
        Row::kv(t("tools.id_result"), if valid { t("tools.id_valid") } else { t("tools.id_invalid") }).primary().tone(if valid { Tone::Good } else { Tone::Danger }),
        Row::kv(t("tools.id_check"), format!("{} ({})", if ok { "✓" } else { "✗" }, tf("tools.id_expect", &[("c", &expect)]))),
        Row::kv(t("tools.id_region"), &id[..6]),
    ];
    match birth {
        Some(b) => {
            let mut age = today.year() - b.year();
            if (today.month(), today.day()) < (b.month(), b.day()) { age -= 1; }
            lines.push(Row::kv(t("tools.id_birth"), b.format("%Y-%m-%d").to_string()));
            lines.push(Row::kv(t("tools.id_age"), age.to_string()));
        }
        None => lines.push(Row::kv(t("tools.id_birth"), "✗")),
    }
    lines.push(Row::kv(t("tools.id_gender"), if male { t("tools.id_male") } else { t("tools.id_female") }));
    Ok(lines)
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF | 0xFE00..=0xFE0F | 0x200D | 0x20E3 | 0xE0020..=0xE007F
        | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x2190..=0x21FF | 0x25A0..=0x25FF | 0x2934 | 0x2935 | 0x2122 | 0x2139 | 0x24C2)
}

pub fn strip_emoji(s: &str) -> String {
    let out: String = s.chars().filter(|c| !is_emoji(*c)).collect();
    // 去掉因删除产生的多余空格
    let re = regex::Regex::new(r"[ \t]{2,}").unwrap();
    out.lines().map(|l| re.replace_all(l, " ").trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

pub fn md_html(s: &str) -> String {
    let opts = pulldown_cmark::Options::ENABLE_TABLES | pulldown_cmark::Options::ENABLE_STRIKETHROUGH | pulldown_cmark::Options::ENABLE_TASKLISTS | pulldown_cmark::Options::ENABLE_FOOTNOTES;
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, pulldown_cmark::Parser::new_ext(s, opts));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lines() {
        assert_eq!(dedupe("a\nA\n b\n\na", true, true, true, false), "a\nb");
        assert_eq!(dedupe("b\na\nb", false, false, false, true), "a\nb");
        assert_eq!(trim("  a  b \n\n c", "collapse", true), "a b\nc");
        assert_eq!(trim(" a b ", "all", false), "ab");
        assert_eq!(trim(" a ", "left", false), "a ");
    }
    #[test]
    fn cases() {
        assert_eq!(case("hello world", "upper"), "HELLO WORLD");
        assert_eq!(case("the lord of the rings", "title"), "The Lord of the Rings");
        assert_eq!(case("HELLO. WORLD", "sentence"), "Hello. World");
    }
    #[test]
    fn plurals() {
        for (s, p) in [("box", "boxes"), ("city", "cities"), ("day", "days"), ("child", "children"), ("knife", "knives"), ("sheep", "sheep"), ("Fox", "Foxes"), ("CAT", "CATS")] {
            assert_eq!(pluralize(s), p);
            assert_eq!(singularize(p).to_lowercase(), s.to_lowercase());
        }
        assert_eq!(plural_lines("apple, box", true), "apples, boxes");
    }
    #[test]
    fn spacing() {
        assert_eq!(cjk_space("使用Rust编写的app有3个"), "使用 Rust 编写的 app 有 3 个");
        assert_eq!(cjk_space("已有 空格 ok"), "已有 空格 ok");
    }
    #[test]
    fn regexes() {
        let r = regex_test(r"(\d+)-(?P<b>\d+)", "1-2 x 30-40", "", false, false, false).unwrap();
        assert!(r.contains("#2 [6..11] 30-40") && r.contains("$b: 40"), "{r}");
        assert_eq!(regex_test("a", "AbA", "x", true, false, false).unwrap(), "xbx");
        assert!(regex_test("(", "", "", false, false, false).is_err());
    }
    #[test]
    fn id_cards() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        let r = id_card("11010519491231002X", today).unwrap();
        assert!(r.contains("1949-12-31") && r.contains(": 76") && r.contains("✓"), "{r}");
        let r = id_card("110105194912310021", today).unwrap();
        assert!(r.contains("✗"));
        assert!(id_card("123", today).is_err());
    }
    #[test]
    fn emoji_and_md() {
        assert_eq!(strip_emoji("你好😀 世界👍🏻 ok❤️"), "你好 世界 ok");
        assert_eq!(strip_emoji("👨‍👩‍👧 家"), " 家");
        assert!(md_html("# T\n\n| a |\n|---|\n| 1 |").contains("<h1>T</h1>"));
        assert!(md_html("| a |\n|---|\n| 1 |").contains("<table>"));
    }
}
