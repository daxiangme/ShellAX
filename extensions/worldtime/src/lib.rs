//! 世界时间：世界时钟、时区换算。扩展包没有时钟，当前时间由宿主传入。
use chrono::{DateTime, FixedOffset, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use shellax_ext_sdk::{self as sdk, serde_json::Value, Reply, Request, Result};

/// 常用城市：(别名们, 时区, 中文名, 英文名)。
const CITIES: &[(&str, &str, &str, &str)] = &[
    ("beijing shanghai china 北京 上海 中国", "Asia/Shanghai", "北京", "Beijing"),
    ("hongkong hk 香港", "Asia/Hong_Kong", "香港", "Hong Kong"),
    ("taipei 台北", "Asia/Taipei", "台北", "Taipei"),
    ("tokyo japan 东京 日本", "Asia/Tokyo", "东京", "Tokyo"),
    ("seoul korea 首尔 韩国", "Asia/Seoul", "首尔", "Seoul"),
    ("singapore 新加坡", "Asia/Singapore", "新加坡", "Singapore"),
    ("bangkok 曼谷", "Asia/Bangkok", "曼谷", "Bangkok"),
    ("kolkata india mumbai delhi 印度 孟买 新德里", "Asia/Kolkata", "新德里", "New Delhi"),
    ("dubai 迪拜", "Asia/Dubai", "迪拜", "Dubai"),
    ("moscow 莫斯科", "Europe/Moscow", "莫斯科", "Moscow"),
    ("berlin germany 柏林 德国", "Europe/Berlin", "柏林", "Berlin"),
    ("paris france 巴黎 法国", "Europe/Paris", "巴黎", "Paris"),
    ("london uk 伦敦 英国", "Europe/London", "伦敦", "London"),
    ("utc gmt", "UTC", "UTC", "UTC"),
    ("saopaulo brazil 圣保罗 巴西", "America/Sao_Paulo", "圣保罗", "São Paulo"),
    ("newyork ny nyc 纽约", "America/New_York", "纽约", "New York"),
    ("chicago 芝加哥", "America/Chicago", "芝加哥", "Chicago"),
    ("denver 丹佛", "America/Denver", "丹佛", "Denver"),
    ("losangeles la sanfrancisco sf seattle 洛杉矶 旧金山 西雅图", "America/Los_Angeles", "洛杉矶", "Los Angeles"),
    ("honolulu hawaii 夏威夷", "Pacific/Honolulu", "夏威夷", "Honolulu"),
    ("sydney australia 悉尼 澳大利亚", "Australia/Sydney", "悉尼", "Sydney"),
    ("auckland newzealand 奥克兰 新西兰", "Pacific/Auckland", "奥克兰", "Auckland"),
];

const DEFAULT_ZONES: &str = "北京\n东京\n伦敦\n纽约\n洛杉矶";
const DEFAULT_ZONES_EN: &str = "Beijing\nTokyo\nLondon\nNew York\nLos Angeles";

/// 解析一个时区：IANA 名、城市别名或 UTC±h。返回 (显示名, 时区)。
pub enum Zone { Named(Tz), Fixed(FixedOffset) }

pub fn zone(s: &str, zh: bool, local: i32) -> std::result::Result<(String, Zone), String> {
    let s = s.trim();
    let key: String = s.to_lowercase().chars().filter(|c| !c.is_whitespace() && *c != '_' && *c != '-').collect();
    if key == "local" || s == "本地" { return Ok((if zh { "本地" } else { "Local" }.into(), Zone::Fixed(FixedOffset::east_opt(local).unwrap_or(Utc.fix())))); }
    for (alias, tz, z, e) in CITIES {
        if alias.split(' ').any(|a| a == key) || z == &s || e.to_lowercase().replace(' ', "") == key {
            return Ok(((if zh { *z } else { *e }).into(), Zone::Named(tz.parse().unwrap())));
        }
    }
    if let Ok(tz) = s.parse::<Tz>() { return Ok((s.into(), Zone::Named(tz))); }
    if let Some(tz) = chrono_tz::TZ_VARIANTS.iter().find(|t| t.name().to_lowercase().replace('_', "").ends_with(&format!("/{key}"))) { return Ok((tz.name().into(), Zone::Named(*tz))); }
    let up = key.to_uppercase();
    if let Some(rest) = up.strip_prefix("UTC").or_else(|| up.strip_prefix("GMT")) {
        let (sign, num) = match rest.chars().next() { Some('+') => (1, &rest[1..]), Some('-') => (-1, &rest[1..]), _ => (1, rest) };
        let (h, m) = num.split_once(':').unwrap_or((num, "0"));
        if let (Ok(h), Ok(m)) = (h.parse::<i32>(), m.parse::<i32>()) {
            if let Some(o) = FixedOffset::east_opt(sign * (h * 3600 + m * 60)) { return Ok((s.to_uppercase(), Zone::Fixed(o))); }
        }
    }
    Err(if zh { format!("无法识别的时区：{s}") } else { format!("Unknown time zone: {s}") })
}

fn fmt_off(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    if a % 3600 == 0 { format!("UTC{sign}{}", a / 3600) } else { format!("UTC{sign}{}:{:02}", a / 3600, a % 3600 / 60) }
}

fn show(utc: DateTime<Utc>, z: &Zone, h12: bool) -> (String, i32) {
    let (naive, off) = match z {
        Zone::Named(tz) => { let d = utc.with_timezone(tz); (d.naive_local(), d.offset().fix().local_minus_utc()) }
        Zone::Fixed(o) => { let d = utc.with_timezone(o); (d.naive_local(), o.local_minus_utc()) }
    };
    (naive.format(if h12 { "%Y-%m-%d %a %I:%M %p" } else { "%Y-%m-%d %a %H:%M" }).to_string(), off)
}

fn table(rows: Vec<(String, String, i32)>, base: i32, zh: bool) -> String {
    let w = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0);
    rows.into_iter().map(|(n, t, off)| {
        let diff = (off - base) as f64 / 3600.;
        let d = if diff == 0. { String::new() } else if zh { format!("  ({}{} 小时)", if diff > 0. { "+" } else { "" }, diff) } else { format!("  ({}{}h)", if diff > 0. { "+" } else { "" }, diff) };
        format!("{n}{}  {t}  {}{d}", " ".repeat(w - n.chars().count()), fmt_off(off))
    }).collect::<Vec<_>>().join("\n")
}

pub fn world_clock(now_ms: i64, local: i32, zones: &str, h12: bool, zh: bool) -> std::result::Result<String, String> {
    let utc = Utc.timestamp_millis_opt(now_ms).single().ok_or("bad time")?;
    let mut rows = vec![];
    for l in zones.lines().filter(|l| !l.trim().is_empty()) {
        let (n, z) = zone(l, zh, local)?;
        let (t, off) = show(utc, &z, h12);
        rows.push((n, t, off));
    }
    Ok(table(rows, local, zh))
}

pub fn tz_convert(input: &str, from: &str, to: &str, now_ms: i64, local: i32, h12: bool, zh: bool) -> std::result::Result<String, String> {
    let (_, fz) = zone(if from.trim().is_empty() { "local" } else { from }, zh, local)?;
    let s = input.trim();
    let utc: DateTime<Utc> = if s.is_empty() || s == "now" || s == "现在" {
        Utc.timestamp_millis_opt(now_ms).single().ok_or("bad time")?
    } else {
        let mut naive = None;
        for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y/%m/%d %H:%M:%S", "%Y/%m/%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
            if let Ok(n) = NaiveDateTime::parse_from_str(s, f) { naive = Some(n); break; }
        }
        if naive.is_none() {
            // 只有时刻：用源时区的今天
            let today = show(Utc.timestamp_millis_opt(now_ms).single().ok_or("bad time")?, &fz, false).0[..10].to_string();
            for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] { if let Ok(n) = NaiveDateTime::parse_from_str(&format!("{today} {s}"), f) { naive = Some(n); break; } }
        }
        let n = naive.ok_or_else(|| if zh { format!("无法解析时间：{s}（例如 2026-10-26 09:00 或 09:00）") } else { format!("Can't parse time: {s} (e.g. 2026-10-26 09:00 or 09:00)") })?;
        match &fz {
            Zone::Named(tz) => tz.from_local_datetime(&n).earliest().ok_or(if zh { "该时刻在源时区不存在（夏令时切换）" } else { "That time doesn't exist in the source zone (DST change)" })?.with_timezone(&Utc),
            Zone::Fixed(o) => o.from_local_datetime(&n).single().ok_or("bad time")?.with_timezone(&Utc),
        }
    };
    let base = match &fz { Zone::Named(tz) => utc.with_timezone(tz).offset().fix().local_minus_utc(), Zone::Fixed(o) => o.local_minus_utc() };
    let mut rows = vec![];
    let to = if to.trim().is_empty() { if zh { DEFAULT_ZONES } else { DEFAULT_ZONES_EN } } else { to };
    for l in to.lines().filter(|l| !l.trim().is_empty()) {
        let (n, z) = zone(l, zh, local)?;
        let (t, off) = show(utc, &z, h12);
        rows.push((n, t, off));
    }
    Ok(table(rows, base, zh))
}

fn manifest(q: &Request) -> Value {
    let tr = |a, b| q.tr(a, b);
    let zones = if q.zh() { DEFAULT_ZONES } else { DEFAULT_ZONES_EN };
    let mut zl = sdk::multiline("zones", tr("城市或时区（每行一个）", "Cities or time zones (one per line)"));
    zl["default"] = zones.into();
    let mut to = sdk::multiline("to", tr("目标城市或时区（每行一个）", "Target cities or zones (one per line)"));
    to["default"] = zones.into();
    sdk::manifest("worldtime", tr("世界时间", "World Time"), env!("CARGO_PKG_VERSION"), tr("世界时钟与时区换算", "World clock and time zone conversion"), vec![
        sdk::tool("world_clock", "time", tr("世界时钟", "World Clock"), tr("同时查看多个城市的当前时间", "Current time in several cities at once"), "world clock timezone city",
            vec![zl], vec![sdk::toggle("h12", tr("12 小时制", "12-hour clock"), false)]),
        sdk::tool("tz_convert", "time", tr("时区换算", "Time Zone Converter"), tr("把某个时区的时间换算到其他时区", "Convert a time from one zone to others"), "timezone convert tz utc",
            vec![sdk::with_placeholder(sdk::text("input", tr("时间", "Time"), ""), "2026-10-26 09:00"), sdk::with_placeholder(sdk::text("from", tr("源时区", "From zone"), "local"), "local / Asia/Shanghai / UTC+8"), to],
            vec![sdk::toggle("h12", tr("12 小时制", "12-hour clock"), false)]),
    ])
}

fn run(q: &Request) -> Result<Reply> {
    Ok(Reply::Text(match q.tool.as_str() {
        "world_clock" => world_clock(q.now_ms, q.utc_offset, q.get("zones"), q.on("h12"), q.zh())?,
        "tz_convert" => tz_convert(q.get("input"), q.get("from"), q.get("to"), q.now_ms, q.utc_offset, q.on("h12"), q.zh())?,
        t => return Err(format!("unknown tool {t}")),
    }))
}

sdk::export!(manifest, run);

#[cfg(test)]
mod tests {
    use super::*;
    const T: i64 = 1_793_000_000_000; // 2026-10-26 07:33:20 UTC
    #[test]
    fn clock() {
        let s = world_clock(T, 8 * 3600, "北京\nNew York\nUTC+5:30\nEurope/Paris", false, true).unwrap();
        assert!(s.contains("北京") && s.contains("15:33") && s.contains("UTC+8"), "{s}");
        assert!(s.contains("纽约") && s.contains("03:33") && s.contains("UTC-4") && s.contains("-12 小时"), "{s}");
        assert!(s.contains("13:03") && s.contains("UTC+5:30"), "{s}");
        assert!(s.contains("Europe/Paris") && s.contains("08:33"), "{s}");
        assert!(world_clock(T, 0, "Mars", false, false).is_err());
    }
    #[test]
    fn convert() {
        let s = tz_convert("2026-10-26 09:00", "Asia/Shanghai", "London\nTokyo", T, 0, false, false).unwrap();
        assert!(s.contains("London") && s.contains("01:00") && s.contains("UTC+0"), "{s}");
        assert!(s.contains("Tokyo") && s.contains("10:00") && s.contains("+1h"), "{s}");
        let s = tz_convert("09:00", "UTC+8", "UTC", T, 0, true, true).unwrap();
        assert!(s.contains("01:00 AM"), "{s}");
    }
}
