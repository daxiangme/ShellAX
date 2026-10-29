//! 生成类工具（随机数来自系统安全随机源）。
use anyhow::{bail, Result};
use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::Rng;

use super::{history, Args, Output, Row, Tone};
use crate::i18n::{t, tf};

/// 一批密码及其参数。
struct Pw { items: Vec<String>, bits: f64, len: usize, sets: Vec<&'static str>, no_ambiguous: bool }

fn gen_passwords(a: &Args) -> Result<Pw> {
    let len = a.int("len", 4, 256)? as usize;
    let count = a.int("count", 1, 200)? as usize;
    let amb = a.on("no_ambiguous");
    let mut sets: Vec<Vec<char>> = vec![];
    let mut names: Vec<&'static str> = vec![];
    let f = |s: &str| -> Vec<char> { s.chars().filter(|c| !amb || !"Il1O0o|`'\"".contains(*c)).collect() };
    if a.on("upper") { sets.push(f("ABCDEFGHIJKLMNOPQRSTUVWXYZ")); names.push("A-Z"); }
    if a.on("lower") { sets.push(f("abcdefghijklmnopqrstuvwxyz")); names.push("a-z"); }
    if a.on("digit") { sets.push(f("0123456789")); names.push("0-9"); }
    if a.on("symbol") { sets.push(f("!@#$%^&*()-_=+[]{};:,.<>?/~")); names.push("!@#$"); }
    if sets.is_empty() { bail!(t("tools.err_charset")); }
    let all: Vec<char> = sets.concat();
    let mut rng = OsRng;
    let mut items = vec![];
    for _ in 0..count {
        // 每类至少一个字符
        let mut p: Vec<char> = sets.iter().take(len).map(|s| s[rng.gen_range(0..s.len())]).collect();
        while p.len() < len { p.push(all[rng.gen_range(0..all.len())]); }
        p.shuffle(&mut rng);
        items.push(p.into_iter().collect::<String>());
    }
    let bits = (len as f64) * (all.len() as f64).log2();
    Ok(Pw { items, bits, len, sets: names, no_ambiguous: amb })
}

/// 纯文本形式：密码逐行，末尾附一行熵值（保留给文本接口与测试）。
pub fn passwords(a: &Args) -> Result<String> {
    let p = gen_passwords(a)?;
    Ok(format!("{}\n\n{}", p.items.join("\n"), tf("tools.pw_entropy", &[("n", &(p.bits.round() as u32))])))
}

/// 强度等级：(文案键, 语气)。熵 < 40 弱，< 60 中，< 80 强，其余很强。
pub fn pw_strength(bits: f64) -> (&'static str, Tone) {
    if bits < 40. { ("tools.pw_weak", Tone::Danger) } else if bits < 60. { ("tools.pw_medium", Tone::Warn) } else if bits < 80. { ("tools.pw_strong", Tone::Good) } else { ("tools.pw_very_strong", Tone::Accent) }
}

/// 生成一批密码：每条一行，行尾带强度标签（悬停显示熵值），不再附加整批的熵值文字。
pub fn password_output(a: &Args) -> Result<Output> {
    let p = gen_passwords(a)?;
    let (key, tone) = pw_strength(p.bits);
    let hint = tf("tools.pw_entropy", &[("n", &(p.bits.round() as u32))]);
    let rows = p.items.iter().map(|s| Row::item(s.clone()).secret().tag(t(key), tone).hint(hint.clone())).collect::<Vec<_>>();
    let mut sets = p.sets.join(" ");
    if p.no_ambiguous { sets = format!("{sets} · {}", t("tools.o_no_ambiguous")); }
    Ok(Output::rows(rows, tf("tools.meta_pw", &[("len", &p.len), ("sets", &sets), ("n", &p.items.len())])))
}

pub fn uuids(count: usize, upper: bool, hyphen: bool) -> String {
    (0..count).map(|_| {
        let u = uuid::Uuid::new_v4();
        let s = if hyphen { u.hyphenated().to_string() } else { u.simple().to_string() };
        if upper { s.to_uppercase() } else { s }
    }).collect::<Vec<_>>().join("\n")
}

/// 生成一批 UUID：每条一行。
pub fn uuid_output(a: &Args) -> Result<Output> {
    let n = a.int("count", 1, history::MAX_ROWS as i64)? as usize;
    let (upper, hyphen) = (a.on("upper"), a.on("hyphen"));
    let rows = uuids(n, upper, hyphen).lines().map(Row::item).collect::<Vec<_>>();
    let case = if upper { t("tools.o_uppercase") } else { t("tools.meta_lower") };
    let hy = if hyphen { t("tools.o_hyphen") } else { t("tools.meta_no_hyphen") };
    Ok(Output::rows(rows, tf("tools.meta_uuid", &[("n", &n), ("case", &case), ("hy", &hy)])))
}

fn random_values(a: &Args, max_count: i64) -> Result<(Vec<String>, String)> {
    let (mut lo, mut hi) = (a.num("min")?, a.num("max")?);
    if lo > hi { std::mem::swap(&mut lo, &mut hi); }
    let count = a.int("count", 1, max_count)? as usize;
    let dec = a.int("decimals", 0, 10)? as i32;
    let unique = a.on("unique");
    let mut rng = OsRng;
    let scale = 10f64.powi(dec);
    let (ilo, ihi) = ((lo * scale).ceil() as i64, (hi * scale).floor() as i64);
    if ilo > ihi { bail!(t("tools.err_range")); }
    let span = (ihi - ilo + 1) as u64;
    if unique && (count as u64) > span { bail!(tf("tools.err_unique", &[("n", &span)])); }
    let mut v: Vec<i64> = if unique && (count as u64) * 2 > span {
        let mut all: Vec<i64> = (ilo..=ihi).collect(); all.shuffle(&mut rng); all.truncate(count); all
    } else {
        let mut seen = std::collections::HashSet::new();
        let mut v = vec![];
        while v.len() < count { let x = rng.gen_range(ilo..=ihi); if !unique || seen.insert(x) { v.push(x); } }
        v
    };
    if a.on("sort") { v.sort(); }
    let items = v.iter().map(|x| if dec == 0 { x.to_string() } else { format!("{:.*}", dec as usize, *x as f64 / scale) }).collect();
    let mut meta = tf("tools.meta_num", &[("lo", &super::num::fmt_num(lo)), ("hi", &super::num::fmt_num(hi)), ("n", &count)]);
    if unique { meta = format!("{meta} · {}", t("tools.o_unique")); }
    if a.on("sort") { meta = format!("{meta} · {}", t("tools.o_sort")); }
    Ok((items, meta))
}

pub fn random_numbers(a: &Args) -> Result<String> { Ok(random_values(a, 100_000)?.0.join("\n")) }

/// 生成一批随机数：每个数一行。
pub fn number_output(a: &Args) -> Result<Output> {
    let (items, meta) = random_values(a, history::MAX_ROWS as i64)?;
    Ok(Output::rows(items.into_iter().map(Row::item).collect(), meta))
}

/// 分组结果：抽中的人，或各组成员。
enum Grouped { Picked(String), Groups(Vec<Vec<String>>) }

fn group_core(s: &str, mode: &str, n: usize) -> Result<(Grouped, usize)> {
    let mut names: Vec<&str> = s.split(['\n', ',', '，', '、']).map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
    if names.is_empty() { bail!(t("tools.err_need_names")); }
    let mut rng = OsRng;
    names.shuffle(&mut rng);
    let total = names.len();
    if mode == "pick" { return Ok((Grouped::Picked(names[0].to_string()), total)); }
    let groups = if mode == "size" { names.len().div_ceil(n.max(1)) } else { n.clamp(1, names.len()) };
    let mut g: Vec<Vec<String>> = vec![vec![]; groups];
    for (i, x) in names.iter().enumerate() { g[i % groups].push(x.to_string()); }
    Ok((Grouped::Groups(g), total))
}

pub fn random_group(s: &str, mode: &str, n: usize) -> Result<String> {
    Ok(match group_core(s, mode, n)?.0 {
        Grouped::Picked(name) => tf("tools.grp_picked", &[("name", &name)]),
        Grouped::Groups(g) => g.iter().enumerate().map(|(i, m)| format!("{} ({}): {}", tf("tools.grp_n", &[("n", &(i + 1))]), m.len(), m.join("、"))).collect::<Vec<_>>().join("\n"),
    })
}

/// 随机分组 / 抽一个：分组每组一行；抽一个每次一行，进入历史。
pub fn group_output(a: &Args) -> Result<Output> {
    let n = a.int("n", 1, 10000)? as usize;
    let mode = a.get("mode");
    let (res, total) = group_core(a.get("input"), mode, n)?;
    Ok(match res {
        Grouped::Picked(name) => Output::rows(vec![Row::item(name).primary()], tf("tools.meta_pick", &[("total", &total)])),
        Grouped::Groups(g) => {
            let meta = if mode == "size" { tf("tools.meta_size", &[("n", &n), ("g", &g.len()), ("total", &total)]) } else { tf("tools.meta_groups", &[("g", &g.len()), ("total", &total)]) };
            let rows = g.iter().enumerate().map(|(i, m)| Row::kv(tf("tools.grp_n", &[("n", &(i + 1))]), m.join("、")).tag(tf("tools.grp_count", &[("n", &m.len())]), Tone::Muted)).collect();
            Output::rows(rows, meta)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gens() {
        let mut a = Args::default();
        a.set("len", "12").set("count", "3").set("upper", "true").set("digit", "true").set("no_ambiguous", "true");
        let r = passwords(&a).unwrap();
        let pw: Vec<_> = r.lines().take(3).collect();
        assert!(pw.iter().all(|p| p.len() == 12 && p.chars().any(|c| c.is_ascii_digit()) && p.chars().any(|c| c.is_ascii_uppercase()) && !p.contains('O') && !p.contains('0')));
        let u = uuids(3, true, false);
        assert!(u.lines().all(|l| l.len() == 32 && l.chars().all(|c| !c.is_lowercase())));
        a.set("min", "1").set("max", "5").set("count", "5").set("decimals", "0").set("unique", "true").set("sort", "true");
        assert_eq!(random_numbers(&a).unwrap(), "1\n2\n3\n4\n5");
        a.set("count", "6");
        assert!(random_numbers(&a).is_err());
        a.set("min", "0").set("max", "1").set("decimals", "2").set("unique", "false").set("count", "50");
        assert!(random_numbers(&a).unwrap().lines().all(|l| { let v: f64 = l.parse().unwrap(); (0. ..=1.).contains(&v) && l.len() == 4 }));
        let g = random_group("a\nb\nc\nd\ne", "groups", 2).unwrap();
        assert_eq!(g.lines().count(), 2);
        let g = random_group("a,b,c,d,e", "size", 2).unwrap();
        assert_eq!(g.lines().count(), 3);
        assert!(random_group("", "pick", 1).is_err());
    }

    #[test]
    fn row_outputs() {
        let mut a = Args::default();
        a.set("len", "16").set("count", "5").set("upper", "true").set("lower", "true").set("digit", "true").set("symbol", "true");
        let o = password_output(&a).unwrap();
        assert_eq!(o.kind, super::super::OutKind::Rows);
        assert_eq!(o.rows.len(), 5);
        assert!(o.rows.iter().all(|r| r.value.len() == 16 && r.secret && r.tag.is_some() && !r.hint.is_empty()));
        // 每条一行，不含整批的熵值文字
        assert_eq!(o.text.lines().count(), 5);
        assert!(!o.text.contains("熵") && !o.text.to_lowercase().contains("entropy"));
        assert!(o.meta.contains("16") && o.meta.contains("A-Z"));
        assert_eq!(pw_strength(30.).1, Tone::Danger);
        assert_eq!(pw_strength(50.).1, Tone::Warn);
        assert_eq!(pw_strength(70.).1, Tone::Good);
        assert_eq!(pw_strength(104.).1, Tone::Accent);
        a.set("count", "7").set("upper", "false");
        assert_eq!(uuid_output(&a).unwrap().rows.len(), 7);
        a.set("count", "100000");
        assert_eq!(uuid_output(&a).unwrap().rows.len(), history::MAX_ROWS);
        a.set("min", "1").set("max", "9").set("count", "4").set("decimals", "0").set("unique", "true");
        let o = number_output(&a).unwrap();
        assert_eq!(o.rows.len(), 4);
        let o = group_output(&{ let mut b = Args::default(); b.set("input", "a\nb\nc\nd\ne").set("mode", "pick").set("n", "1"); b }).unwrap();
        assert_eq!(o.rows.len(), 1);
        assert!(o.rows[0].primary);
        let o = group_output(&{ let mut b = Args::default(); b.set("input", "a,b,c,d,e").set("mode", "groups").set("n", "2"); b }).unwrap();
        assert_eq!(o.rows.len(), 2);
        assert!(o.rows.iter().all(|r| r.tag.is_some() && !r.label.is_empty()));
    }
}
