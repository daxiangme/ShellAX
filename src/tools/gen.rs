//! 生成类工具（随机数来自系统安全随机源）。
use anyhow::{bail, Result};
use rand::rngs::OsRng;
use rand::seq::SliceRandom;
use rand::Rng;

use super::Args;
use crate::i18n::{t, tf};

pub fn passwords(a: &Args) -> Result<String> {
    let len = a.int("len", 4, 256)? as usize;
    let count = a.int("count", 1, 200)? as usize;
    let amb = a.on("no_ambiguous");
    let mut sets: Vec<Vec<char>> = vec![];
    let f = |s: &str| -> Vec<char> { s.chars().filter(|c| !amb || !"Il1O0o|`'\"".contains(*c)).collect() };
    if a.on("upper") { sets.push(f("ABCDEFGHIJKLMNOPQRSTUVWXYZ")); }
    if a.on("lower") { sets.push(f("abcdefghijklmnopqrstuvwxyz")); }
    if a.on("digit") { sets.push(f("0123456789")); }
    if a.on("symbol") { sets.push(f("!@#$%^&*()-_=+[]{};:,.<>?/~")); }
    if sets.is_empty() { bail!(t("tools.err_charset")); }
    let all: Vec<char> = sets.concat();
    let mut rng = OsRng;
    let mut out = vec![];
    for _ in 0..count {
        // 每类至少一个字符
        let mut p: Vec<char> = sets.iter().take(len).map(|s| s[rng.gen_range(0..s.len())]).collect();
        while p.len() < len { p.push(all[rng.gen_range(0..all.len())]); }
        p.shuffle(&mut rng);
        out.push(p.into_iter().collect::<String>());
    }
    let bits = (len as f64) * (all.len() as f64).log2();
    Ok(format!("{}\n\n{}", out.join("\n"), tf("tools.pw_entropy", &[("n", &(bits.round() as u32))])))
}

pub fn uuids(count: usize, upper: bool, hyphen: bool) -> String {
    (0..count).map(|_| {
        let u = uuid::Uuid::new_v4();
        let s = if hyphen { u.hyphenated().to_string() } else { u.simple().to_string() };
        if upper { s.to_uppercase() } else { s }
    }).collect::<Vec<_>>().join("\n")
}

pub fn random_numbers(a: &Args) -> Result<String> {
    let (mut lo, mut hi) = (a.num("min")?, a.num("max")?);
    if lo > hi { std::mem::swap(&mut lo, &mut hi); }
    let count = a.int("count", 1, 100_000)? as usize;
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
    Ok(v.iter().map(|x| if dec == 0 { x.to_string() } else { format!("{:.*}", dec as usize, *x as f64 / scale) }).collect::<Vec<_>>().join("\n"))
}

pub fn random_group(s: &str, mode: &str, n: usize) -> Result<String> {
    let mut names: Vec<&str> = s.split(['\n', ',', '，', '、']).map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
    if names.is_empty() { bail!(t("tools.err_need_names")); }
    let mut rng = OsRng;
    names.shuffle(&mut rng);
    if mode == "pick" { return Ok(tf("tools.grp_picked", &[("name", &names[0])])); }
    let groups = if mode == "size" { names.len().div_ceil(n.max(1)) } else { n.clamp(1, names.len()) };
    let mut g: Vec<Vec<&str>> = vec![vec![]; groups];
    for (i, x) in names.iter().enumerate() { g[i % groups].push(x); }
    Ok(g.iter().enumerate().map(|(i, m)| format!("{} ({}): {}", tf("tools.grp_n", &[("n", &(i + 1))]), m.len(), m.join("、"))).collect::<Vec<_>>().join("\n"))
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
}
