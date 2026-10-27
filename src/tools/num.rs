//! 单位换算与财务计算。
use anyhow::{anyhow, bail, Result};

use super::Args;
use crate::i18n::{t, tf};

/// (种类 id, 文案键)
pub const UNIT_KINDS: &[(&str, &str)] = &[
    ("length", "tools.u_length"), ("mass", "tools.u_mass"), ("temp", "tools.u_temp"), ("data", "tools.u_data"),
    ("time", "tools.u_time"), ("area", "tools.u_area"), ("volume", "tools.u_volume"), ("speed", "tools.u_speed"),
];

/// (单位符号, 换算到基准单位的系数)。温度单独处理。
fn table(kind: &str) -> &'static [(&'static str, f64)] {
    match kind {
        "length" => &[("nm", 1e-9), ("μm", 1e-6), ("mm", 1e-3), ("cm", 1e-2), ("m", 1.), ("km", 1e3), ("in", 0.0254), ("ft", 0.3048), ("yd", 0.9144), ("mi", 1609.344), ("nmi", 1852.), ("里", 500.), ("丈", 10. / 3.), ("尺", 1. / 3.), ("寸", 1. / 30.)],
        "mass" => &[("mg", 1e-6), ("g", 1e-3), ("kg", 1.), ("t", 1e3), ("oz", 0.028349523125), ("lb", 0.45359237), ("斤", 0.5), ("两", 0.05), ("ct", 0.0002)],
        "data" => &[("bit", 0.125), ("B", 1.), ("KB", 1e3), ("MB", 1e6), ("GB", 1e9), ("TB", 1e12), ("PB", 1e15), ("KiB", 1024.), ("MiB", 1048576.), ("GiB", 1073741824.), ("TiB", 1099511627776.)],
        "time" => &[("ns", 1e-9), ("μs", 1e-6), ("ms", 1e-3), ("s", 1.), ("min", 60.), ("h", 3600.), ("d", 86400.), ("wk", 604800.), ("yr", 31536000.)],
        "area" => &[("mm²", 1e-6), ("cm²", 1e-4), ("m²", 1.), ("ha", 1e4), ("km²", 1e6), ("in²", 0.00064516), ("ft²", 0.09290304), ("acre", 4046.8564224), ("亩", 2000. / 3.)],
        "volume" => &[("ml", 1e-6), ("l", 1e-3), ("m³", 1.), ("cm³", 1e-6), ("gal", 0.003785411784), ("qt", 0.000946352946), ("pt", 0.000473176473), ("fl oz", 0.0000295735295625)],
        "speed" => &[("m/s", 1.), ("km/h", 1. / 3.6), ("mph", 0.44704), ("kn", 1852. / 3600.), ("ft/s", 0.3048)],
        _ => &[],
    }
}

fn norm_unit(u: &str) -> String {
    let u = u.trim();
    match u.to_lowercase().as_str() { "um" => "μm".into(), "us" => "μs".into(), "c" | "℃" | "°c" => "°C".into(), "f" | "℉" | "°f" => "°F".into(), "k" if u == "K" || u == "k" => "K".into(), "m2" => "m²".into(), "km2" => "km²".into(), "m3" => "m³".into(), "cm3" => "cm³".into(), _ => u.into() }
}

pub fn fmt_num(v: f64) -> String {
    if v == 0. { return "0".into(); }
    let a = v.abs();
    if !(1e-6..1e15).contains(&a) { return format!("{v:e}"); }
    let s = format!("{v:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub fn convert(kind: &str, v: f64, from: &str) -> Result<Vec<(String, f64)>> {
    let from = norm_unit(from);
    if kind == "temp" {
        let c = match from.as_str() { "°C" => v, "°F" => (v - 32.) * 5. / 9., "K" => v - 273.15, _ => bail!(tf("tools.err_unit", &[("u", &from)])) };
        return Ok(vec![("°C".into(), c), ("°F".into(), c * 9. / 5. + 32.), ("K".into(), c + 273.15)]);
    }
    let tb = table(kind);
    let f = tb.iter().find(|(u, _)| *u == from).or_else(|| tb.iter().find(|(u, _)| u.eq_ignore_ascii_case(&from))).ok_or_else(|| anyhow!(tf("tools.err_unit", &[("u", &from)])))?.1;
    let base = v * f;
    Ok(tb.iter().map(|(u, k)| (u.to_string(), base / k)).collect())
}

pub fn units(a: &Args) -> Result<String> {
    let v = a.num("input")?;
    let kind = a.get("kind");
    let from = if a.get("from").trim().is_empty() { if kind == "temp" { "°C" } else { table(kind).first().map(|x| x.0).unwrap_or("") } } else { a.get("from") };
    let rows = convert(kind, v, from).or_else(|e| {
        // 单位不属于当前种类时，自动找一个包含它的种类
        UNIT_KINDS.iter().find_map(|k| convert(k.0, v, from).ok()).ok_or(e)
    })?;
    Ok(rows.into_iter().map(|(u, x)| format!("{} {u}", fmt_num(x))).collect::<Vec<_>>().join("\n"))
}

const ONES: [&str; 20] = ["Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten", "Eleven", "Twelve", "Thirteen", "Fourteen", "Fifteen", "Sixteen", "Seventeen", "Eighteen", "Nineteen"];
const TENS: [&str; 10] = ["", "", "Twenty", "Thirty", "Forty", "Fifty", "Sixty", "Seventy", "Eighty", "Ninety"];

fn under_1000(n: u64) -> String {
    let mut parts = vec![];
    if n >= 100 { parts.push(format!("{} Hundred", ONES[(n / 100) as usize])); }
    let r = n % 100;
    if r > 0 {
        if r < 20 { parts.push(ONES[r as usize].to_string()); } else if r % 10 == 0 { parts.push(TENS[(r / 10) as usize].to_string()); } else { parts.push(format!("{}-{}", TENS[(r / 10) as usize], ONES[(r % 10) as usize])); }
    }
    parts.join(" ")
}

pub fn en_words(mut n: u64) -> String {
    if n == 0 { return "Zero".into(); }
    let scales = ["", "Thousand", "Million", "Billion", "Trillion", "Quadrillion"];
    let mut out = vec![];
    let mut i = 0;
    while n > 0 {
        let g = n % 1000;
        if g > 0 { out.push(if scales[i].is_empty() { under_1000(g) } else { format!("{} {}", under_1000(g), scales[i]) }); }
        n /= 1000; i += 1;
    }
    out.reverse();
    out.join(" ")
}

fn parse_money(s: &str) -> Result<(bool, u64, u64)> {
    let s: String = s.trim().chars().filter(|c| !matches!(c, ',' | '，' | ' ' | '¥' | '$' | '￥' | '€' | '_')).collect();
    if s.is_empty() { bail!(t("tools.err_empty")); }
    let (neg, s) = match s.strip_prefix('-') { Some(r) => (true, r.to_string()), None => (false, s) };
    let (i, f) = s.split_once('.').unwrap_or((&s, ""));
    if i.is_empty() && f.is_empty() || !i.chars().all(|c| c.is_ascii_digit()) || !f.chars().all(|c| c.is_ascii_digit()) { bail!(tf("tools.err_bad_number", &[("v", &s)])); }
    if i.len() > 16 { bail!(t("tools.err_too_large")); }
    let int: u64 = if i.is_empty() { 0 } else { i.parse()? };
    // 四舍五入到分
    let mut cents: u64 = format!("{:0<2}", &f[..f.len().min(2)]).parse().unwrap_or(0);
    let mut int = int;
    if f.len() > 2 && f.as_bytes()[2] >= b'5' { cents += 1; if cents == 100 { cents = 0; int += 1; } }
    Ok((neg, int, cents))
}

pub fn en_amount(s: &str, currency: &str) -> Result<String> {
    let (neg, int, cents) = parse_money(s)?;
    let (major, minor) = match currency { "USD" => ("Dollars", "Cents"), "EUR" => ("Euros", "Cents"), "HKD" => ("Hong Kong Dollars", "Cents"), "RMB" => ("Yuan", "Fen"), _ => ("", "") };
    let mut w = en_words(int);
    if !major.is_empty() { w = format!("{w} {major}"); }
    let words = if cents > 0 { if major.is_empty() { format!("{w} and {cents:02}/100") } else { format!("{w} and {} {minor}", en_words(cents)) } } else { w };
    let words = if neg { format!("Minus {words}") } else { words };
    let check = format!("{} {} AND {:02}/100", if currency.is_empty() { "" } else { currency }, en_words(int).to_uppercase(), cents).trim().to_string();
    Ok(format!("{words}\n{}\nSAY {} ONLY\n{}", words.to_uppercase(), words.to_uppercase(), check))
}

pub fn rmb(s: &str) -> Result<String> {
    const D: [&str; 10] = ["零", "壹", "贰", "叁", "肆", "伍", "陆", "柒", "捌", "玖"];
    const U: [&str; 4] = ["", "拾", "佰", "仟"];
    const G: [&str; 4] = ["", "万", "亿", "万"];
    let (neg, int, cents) = parse_money(s)?;
    let mut out = String::new();
    if int > 0 {
        let ds: Vec<u8> = int.to_string().bytes().map(|b| b - b'0').collect();
        let n = ds.len();
        let mut zero = false;
        let mut group_has = false;
        for (i, &d) in ds.iter().enumerate() {
            let pos = n - 1 - i;
            if d == 0 { zero = true; } else {
                if zero { out.push_str("零"); zero = false; }
                out.push_str(D[d as usize]); out.push_str(U[pos % 4]);
                group_has = true;
            }
            if pos % 4 == 0 {
                if group_has || pos == 8 && n > 9 { out.push_str(G[(pos / 4) % 4]); }
                group_has = false;
            }
        }
        out.push('元');
    }
    let (j, f) = (cents / 10, cents % 10);
    if j == 0 && f == 0 { if int == 0 { out.push_str("零元"); } out.push('整'); } else {
        if j > 0 { out.push_str(D[j as usize]); out.push('角'); } else if int > 0 { out.push('零'); }
        if f > 0 { out.push_str(D[f as usize]); out.push('分'); } else { out.push('整'); }
    }
    Ok(if neg { format!("负{out}") } else { out })
}

pub fn parse_numbers(s: &str) -> Vec<f64> {
    s.split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | ';' | '；' | '、' | '|'))
        .filter_map(|w| { let w: String = w.chars().filter(|c| !matches!(c, '¥' | '$' | '￥' | '€' | '元')).collect(); w.parse::<f64>().ok() })
        .filter(|v| v.is_finite()).collect()
}

pub fn sum_list(s: &str) -> String {
    let v = parse_numbers(s);
    if v.is_empty() { return t("tools.no_numbers").to_string(); }
    let sum: f64 = v.iter().sum();
    let mut sorted = v.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    let median = if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2. };
    let r = |x: f64| fmt_num((x * 1e8).round() / 1e8);
    format!("{}: {n}\n{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}", t("tools.s_count"), t("tools.s_sum"), r(sum), t("tools.s_avg"), r(sum / n as f64), t("tools.s_min"), r(sorted[0]), t("tools.s_max"), r(sorted[n - 1]), t("tools.s_median"), r(median))
}

pub fn loan(a: &Args) -> Result<String> {
    let p = a.num("principal")?;
    let rate = a.num("rate")? / 100. / 12.;
    let n = (a.num("years")? * 12.).round();
    if p <= 0. || n <= 0. || rate < 0. { bail!(t("tools.err_loan")); }
    let money = |x: f64| format!("{:.2}", x);
    if a.get("method") == "linear" {
        let base = p / n;
        let first = base + p * rate;
        let last = base + base * rate;
        let interest = p * rate * (n + 1.) / 2.;
        return Ok(format!("{}: {}\n{}: {}\n{}: {}\n{}: {}\n{}: {}", t("tools.l_first"), money(first), t("tools.l_last"), money(last), t("tools.l_decrease"), money(base * rate), t("tools.l_interest"), money(interest), t("tools.l_total"), money(p + interest)));
    }
    let m = if rate == 0. { p / n } else { p * rate * (1. + rate).powf(n) / ((1. + rate).powf(n) - 1.) };
    let total = m * n;
    Ok(format!("{}: {}\n{}: {}\n{}: {}\n{}: {n}", t("tools.l_monthly"), money(m), t("tools.l_interest"), money(total - p), t("tools.l_total"), money(total), t("tools.l_months")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rmb_upper() {
        assert_eq!(rmb("1234.56").unwrap(), "壹仟贰佰叁拾肆元伍角陆分");
        assert_eq!(rmb("100000").unwrap(), "壹拾万元整");
        assert_eq!(rmb("1001").unwrap(), "壹仟零壹元整");
        assert_eq!(rmb("100001").unwrap(), "壹拾万零壹元整");
        assert_eq!(rmb("0.5").unwrap(), "伍角整");
        assert_eq!(rmb("10.05").unwrap(), "壹拾元零伍分");
        assert_eq!(rmb("0").unwrap(), "零元整");
        assert_eq!(rmb("100000000").unwrap(), "壹亿元整");
        assert_eq!(rmb("1,0000,0001").unwrap(), "壹亿零壹元整");
        assert_eq!(rmb("-3").unwrap(), "负叁元整");
        assert!(rmb("abc").is_err());
    }
    #[test]
    fn english() {
        assert_eq!(en_words(123), "One Hundred Twenty-Three");
        assert_eq!(en_words(1_000_015), "One Million Fifteen");
        let s = en_amount("1234.5", "USD").unwrap();
        assert!(s.starts_with("One Thousand Two Hundred Thirty-Four Dollars and Fifty Cents"), "{s}");
        assert!(s.contains("USD ONE THOUSAND TWO HUNDRED THIRTY-FOUR AND 50/100"));
    }
    #[test]
    fn unit_conv() {
        let r = convert("length", 1., "km").unwrap();
        assert!(r.iter().any(|(u, v)| u == "m" && (*v - 1000.).abs() < 1e-9));
        let r = convert("temp", 100., "c").unwrap();
        assert!(r.iter().any(|(u, v)| u == "°F" && (*v - 212.).abs() < 1e-9));
        assert!(convert("mass", 1., "parsec").is_err());
        let mut a = Args::default();
        a.set("input", "1").set("kind", "length").set("from", "GiB");
        assert!(units(&a).unwrap().contains("1024 MiB"));
    }
    #[test]
    fn sums_and_loans() {
        let s = sum_list("1, 2\n3 ¥4 x");
        assert!(s.contains(": 4\n") && s.contains(": 10\n") && s.contains("2.5"));
        let mut a = Args::default();
        a.set("principal", "100000").set("rate", "6").set("years", "1").set("method", "annuity");
        assert!(loan(&a).unwrap().contains("8606.64"));
        a.set("method", "linear");
        assert!(loan(&a).unwrap().contains("8833.33"));
    }
}
