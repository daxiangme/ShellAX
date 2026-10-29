//! 中文语言包：汉字转拼音、简繁转换。
use pinyin::ToPinyin;
use shellax_ext_sdk::{self as sdk, serde_json::Value, Reply, Request, Result};

fn manifest(q: &Request) -> Value {
    let tr = |a, b| q.tr(a, b);
    sdk::manifest("lang-zh", tr("中文语言包", "Chinese Language"), env!("CARGO_PKG_VERSION"), tr("汉字转拼音、简繁转换", "Pinyin and Simplified/Traditional conversion"), vec![
        sdk::tool("pinyin", "lang", tr("汉字转拼音", "Chinese to Pinyin"), tr("汉字转拼音，可带声调或只取首字母", "Convert Chinese characters to pinyin, with tones or initials"), "pinyin hanzi pin yin",
            vec![sdk::multiline("input", tr("输入", "Input"))],
            vec![sdk::select("style", tr("格式", "Style"), &[("tone", tr("带声调", "With tones")), ("plain", tr("不带声调", "Without tones")), ("num", tr("数字声调", "Tone numbers")), ("initial", tr("首字母", "Initials"))]),
                 sdk::select("sep", tr("分隔", "Separator"), &[(" ", tr("空格", "Space")), ("", tr("无", "None")), ("-", "-")]),
                 sdk::toggle("keep", tr("保留非汉字", "Keep other characters"), true)]),
        sdk::tool("zh_convert", "lang", tr("简繁转换", "Simplified ⇄ Traditional"), tr("简体与繁体中文互相转换", "Convert between Simplified and Traditional Chinese"), "jianfan simplified traditional fanti",
            vec![sdk::multiline("input", tr("输入", "Input"))],
            vec![sdk::select("dir", tr("方向", "Direction"), &[("s2t", tr("简体 → 繁体", "Simplified → Traditional")), ("t2s", tr("繁体 → 简体", "Traditional → Simplified")),
                ("s2twp", tr("简体 → 台湾正体（含用词）", "Simplified → Taiwan (with phrases)")), ("tw2sp", tr("台湾正体 → 简体（含用词）", "Taiwan → Simplified (with phrases)")),
                ("s2hk", tr("简体 → 香港繁体", "Simplified → Hong Kong")), ("hk2s", tr("香港繁体 → 简体", "Hong Kong → Simplified"))])]),
    ])
}

pub fn pinyin(s: &str, style: &str, sep: &str, keep: bool) -> String {
    let mut out = String::new();
    let mut last_py = false;
    for (ch, p) in s.chars().zip(s.to_pinyin()) {
        match p {
            Some(p) => {
                if last_py || (!out.is_empty() && !out.ends_with(char::is_whitespace) && !sep.is_empty() && keep) { out.push_str(sep); }
                out.push_str(match style { "plain" => p.plain(), "num" => p.with_tone_num_end(), "initial" => p.first_letter(), _ => p.with_tone() });
                last_py = true;
            }
            None => {
                if !keep { continue; }
                if last_py && !sep.is_empty() && !ch.is_whitespace() && !ch.is_ascii_punctuation() && !"，。！？；：、".contains(ch) { out.push_str(sep); }
                out.push(ch);
                last_py = false;
            }
        }
    }
    out
}

pub fn convert(s: &str, dir: &str) -> String {
    match dir {
        "t2s" => hanconv::t2s(s), "s2twp" => hanconv::s2twp(s), "tw2sp" => hanconv::tw2sp(s),
        "s2hk" => hanconv::s2hk(s), "hk2s" => hanconv::hk2s(s), _ => hanconv::s2t(s),
    }
}

fn run(q: &Request) -> Result<Reply> {
    Ok(Reply::Text(match q.tool.as_str() {
        "pinyin" => q.get("input").lines().map(|l| pinyin(l, q.get("style"), q.get("sep"), q.on("keep"))).collect::<Vec<_>>().join("\n"),
        "zh_convert" => convert(q.get("input"), q.get("dir")),
        t => return Err(format!("unknown tool {t}")),
    }))
}

sdk::export!(manifest, run);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn py() {
        assert_eq!(pinyin("中国", "tone", " ", true), "zhōng guó");
        assert_eq!(pinyin("中国", "initial", "", true), "zg");
        assert_eq!(pinyin("中国人", "num", " ", true), "zhong1 guo2 ren2");
        assert_eq!(pinyin("我爱Rust", "plain", " ", true), "wo ai Rust");
        assert_eq!(pinyin("我爱Rust", "plain", " ", false), "wo ai");
    }
    #[test]
    fn conv() {
        assert_eq!(convert("汉字", "s2t"), "漢字");
        assert_eq!(convert("漢字", "t2s"), "汉字");
        assert_eq!(convert("软件", "s2twp"), "軟體");
    }
}
