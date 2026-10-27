//! 日期时间工具。
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

use super::Args;
use crate::i18n::{t, tf};

/// 解析常见日期时间写法（本地时区）。
pub fn parse_dt(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    if s.is_empty() { return None; }
    if let Ok(d) = DateTime::parse_from_rfc3339(s) { return Some(d.with_timezone(&Local)); }
    if let Ok(d) = DateTime::parse_from_rfc2822(s) { return Some(d.with_timezone(&Local)); }
    let s2 = s.replace(['/', '.'], "-").replace('T', " ").replace(['年', '月'], "-").replace('日', " ").trim().to_string();
    for f in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y%m%d %H%M%S", "%Y%m%d%H%M%S"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(&s2, f) { return Local.from_local_datetime(&n).earliest(); }
    }
    for f in ["%Y-%m-%d", "%Y%m%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(&s2, f) { return Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest(); }
    }
    None
}

pub fn parse_date(s: &str) -> Option<NaiveDate> { parse_dt(s).map(|d| d.date_naive()) }

/// 数字时间戳 → 时间：按位数判断秒 / 毫秒 / 微秒 / 纳秒。
pub fn from_ts(n: i64) -> Option<DateTime<Utc>> {
    let a = n.unsigned_abs();
    if a < 100_000_000_000 { Utc.timestamp_opt(n, 0).single() }
    else if a < 100_000_000_000_000 { Utc.timestamp_millis_opt(n).single() }
    else if a < 100_000_000_000_000_000 { Some(Utc.timestamp_nanos(n * 1000)) }
    else { Some(Utc.timestamp_nanos(n)) }
}

fn describe(d: DateTime<Local>) -> String {
    let wd = ["tools.wd_mon", "tools.wd_tue", "tools.wd_wed", "tools.wd_thu", "tools.wd_fri", "tools.wd_sat", "tools.wd_sun"][d.weekday().num_days_from_monday() as usize];
    [format!("{}: {}", t("tools.ts_s"), d.timestamp()), format!("{}: {}", t("tools.ts_ms"), d.timestamp_millis()),
     format!("{}: {} {}", t("tools.ts_local"), d.format("%Y-%m-%d %H:%M:%S"), t(wd)), format!("UTC: {}", d.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S")),
     format!("ISO 8601: {}", d.to_rfc3339()), format!("RFC 2822: {}", d.to_rfc2822()),
     format!("{}: {}", t("tools.ts_week"), tf("tools.ts_week_v", &[("y", &d.iso_week().year()), ("w", &d.iso_week().week()), ("d", &d.ordinal())]))].join("\n")
}

pub fn timestamp(s: &str, now: DateTime<Local>) -> Result<String> {
    let s = s.trim();
    if s.is_empty() { return Ok(describe(now)); }
    if let Ok(n) = s.parse::<i64>() {
        let d = from_ts(n).ok_or_else(|| anyhow!(t("tools.err_ts")))?;
        return Ok(describe(d.with_timezone(&Local)));
    }
    if let Ok(f) = s.parse::<f64>() { if let Some(d) = Utc.timestamp_millis_opt((f * 1000.).round() as i64).single() { return Ok(describe(d.with_timezone(&Local))); } }
    parse_dt(s).map(describe).ok_or_else(|| anyhow!(tf("tools.err_date", &[("v", &s)])))
}

fn add_months(d: NaiveDateTime, m: i64) -> Option<NaiveDateTime> {
    let total = d.year() as i64 * 12 + d.month0() as i64 + m;
    let (y, mo) = (total.div_euclid(12) as i32, total.rem_euclid(12) as u32 + 1);
    let last = (28..=31).rev().find(|&x| NaiveDate::from_ymd_opt(y, mo, x).is_some())?;
    NaiveDate::from_ymd_opt(y, mo, d.day().min(last)).map(|x| x.and_time(d.time()))
}

pub fn date_calc(a: &Args) -> Result<String> {
    let now = Local::now();
    let da = if a.get("a").trim().is_empty() { now } else { parse_dt(a.get("a")).ok_or_else(|| anyhow!(tf("tools.err_date", &[("v", &a.get("a"))])))? };
    if a.get("mode") == "add" {
        let n = a.num("n")?.round() as i64;
        let base = da.naive_local();
        let r = match a.get("unit") {
            "week" => Some(base + Duration::weeks(n)), "month" => add_months(base, n), "year" => add_months(base, n * 12), "hour" => Some(base + Duration::hours(n)),
            _ => Some(base + Duration::days(n)),
        }.ok_or_else(|| anyhow!(t("tools.err_range")))?;
        let r = Local.from_local_datetime(&r).earliest().ok_or_else(|| anyhow!(t("tools.err_range")))?;
        return Ok(describe(r));
    }
    let db = if a.get("b").trim().is_empty() { now } else { parse_dt(a.get("b")).ok_or_else(|| anyhow!(tf("tools.err_date", &[("v", &a.get("b"))])))? };
    let (x, y) = if da <= db { (da, db) } else { (db, da) };
    let dur = y - x;
    // 整月数
    let mut months = (y.year() - x.year()) as i64 * 12 + y.month() as i64 - x.month() as i64;
    if add_months(x.naive_local(), months).is_some_and(|m| m > y.naive_local()) { months -= 1; }
    let workdays = (0..dur.num_days()).filter(|i| (x.date_naive() + Duration::days(*i)).weekday().num_days_from_monday() < 5).count();
    Ok([tf("tools.diff_days", &[("n", &dur.num_days())]), tf("tools.diff_weeks", &[("w", &(dur.num_days() / 7)), ("d", &(dur.num_days() % 7))]),
        tf("tools.diff_months", &[("n", &months)]), tf("tools.diff_hours", &[("n", &dur.num_hours())]), tf("tools.diff_minutes", &[("n", &dur.num_minutes())]),
        tf("tools.diff_seconds", &[("n", &dur.num_seconds())]), tf("tools.diff_workdays", &[("n", &workdays)])].join("\n"))
}

fn date_list(s: &str) -> Result<Vec<NaiveDate>> {
    let mut v = vec![];
    for w in s.split(|c: char| c.is_whitespace() || c == ',' || c == '，' || c == ';') {
        if w.is_empty() { continue; }
        // 支持区间 2026-10-01~2026-10-07
        if let Some((a, b)) = w.split_once(['~', '～']) {
            let (a, b) = (parse_date(a).ok_or_else(|| anyhow!(tf("tools.err_date", &[("v", &a)])))?, parse_date(b).ok_or_else(|| anyhow!(tf("tools.err_date", &[("v", &b)])))?);
            let mut d = a; while d <= b { v.push(d); d += Duration::days(1); }
        } else { v.push(parse_date(w).ok_or_else(|| anyhow!(tf("tools.err_date", &[("v", &w)])))?); }
    }
    Ok(v)
}

pub fn is_workday(d: NaiveDate, holidays: &[NaiveDate], extra: &[NaiveDate]) -> bool {
    if extra.contains(&d) { return true; }
    if holidays.contains(&d) { return false; }
    d.weekday().num_days_from_monday() < 5
}

pub fn workday(a: &Args) -> Result<String> {
    let start = if a.get("start").trim().is_empty() { Local::now().date_naive() } else { parse_date(a.get("start")).ok_or_else(|| anyhow!(tf("tools.err_date", &[("v", &a.get("start"))])))? };
    let n = a.num("n")?.round() as i64;
    if n.abs() > 100_000 { bail!(t("tools.err_range")); }
    let (hol, extra) = (date_list(a.get("holidays"))?, date_list(a.get("workdays"))?);
    let step = if n >= 0 { 1 } else { -1 };
    let mut d = start; let mut left = n.abs(); let mut skipped = 0;
    while left > 0 { d += Duration::days(step); if is_workday(d, &hol, &extra) { left -= 1; } else { skipped += 1; } }
    let wd = ["tools.wd_mon", "tools.wd_tue", "tools.wd_wed", "tools.wd_thu", "tools.wd_fri", "tools.wd_sat", "tools.wd_sun"][d.weekday().num_days_from_monday() as usize];
    Ok([format!("{}: {} {}", t("tools.wk_result"), d.format("%Y-%m-%d"), t(wd)), tf("tools.wk_calendar", &[("n", &(d - start).num_days().abs())]), tf("tools.wk_skipped", &[("n", &skipped)]),
        format!("{}: {}", t("tools.wk_start_is"), if is_workday(start, &hol, &extra) { t("tools.wk_workday") } else { t("tools.wk_restday") })].join("\n"))
}

pub fn batch(s: &str, mode: &str, ms: bool) -> String {
    let num = regex::Regex::new(r"^-?\d{9,19}$").unwrap();
    s.lines().map(|l| {
        let x = l.trim();
        if x.is_empty() { return String::new(); }
        let is_ts = num.is_match(x);
        if (mode == "to_date" || (mode == "auto" && is_ts)) && is_ts {
            return x.parse::<i64>().ok().and_then(from_ts).map(|d| d.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| format!("{x}  ✗"));
        }
        if mode == "to_date" { return format!("{x}  ✗"); }
        // 行内可能含日志前缀：取第一个能解析的日期时间片段
        let re = regex::Regex::new(r"\d{4}[-/.]\d{1,2}[-/.]\d{1,2}(?:[ T]\d{1,2}:\d{2}(?::\d{2}(?:\.\d+)?)?)?(?:Z|[+-]\d{2}:?\d{2})?").unwrap();
        let cand = re.find(x).map(|m| m.as_str()).unwrap_or(x);
        match parse_dt(cand) { Some(d) => if ms { d.timestamp_millis().to_string() } else { d.timestamp().to_string() }, None => format!("{x}  ✗") }
    }).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_and_ts() {
        assert!(parse_dt("2026-10-26 12:30:00").is_some());
        assert!(parse_dt("2026/10/26").is_some());
        assert!(parse_dt("2026年10月26日").is_some());
        assert!(parse_dt("2026-10-26T12:30:00Z").is_some());
        assert!(parse_dt("nope").is_none());
        assert_eq!(from_ts(1_700_000_000).unwrap().timestamp(), 1_700_000_000);
        assert_eq!(from_ts(1_700_000_000_123).unwrap().timestamp_millis(), 1_700_000_000_123);
        let r = timestamp("1700000000", Local::now()).unwrap();
        assert!(r.contains("1700000000000") && r.contains("2023-11-14 22:13:20"), "{r}");
        let r = timestamp("2023-11-14T22:13:20Z", Local::now()).unwrap();
        assert!(r.contains(": 1700000000\n"));
    }
    #[test]
    fn calc() {
        let mut a = Args::default();
        a.set("a", "2026-01-31").set("mode", "add").set("n", "1").set("unit", "month");
        assert!(date_calc(&a).unwrap().contains("2026-02-28"));
        a.set("mode", "diff").set("b", "2026-03-02");
        let r = date_calc(&a).unwrap();
        assert!(r.contains("30") && r.contains(&tf("tools.diff_months", &[("n", &1)])), "{r}");
    }
    #[test]
    fn workdays() {
        let mut a = Args::default();
        // 2026-10-23 是周五
        a.set("start", "2026-10-23").set("n", "1");
        assert!(workday(&a).unwrap().contains("2026-10-26"));
        a.set("holidays", "2026-10-26~2026-10-27").set("workdays", "2026-10-24");
        assert!(workday(&a).unwrap().contains("2026-10-24"));
        a.set("n", "2");
        assert!(workday(&a).unwrap().contains("2026-10-28"));
        a.set("n", "-1").set("holidays", "").set("workdays", "");
        assert!(workday(&a).unwrap().contains("2026-10-22"));
    }
    #[test]
    fn batches() {
        let r = batch("1700000000\n2023-11-14T22:13:20Z\n[INFO] 2023-11-14T22:13:20Z started\nxx", "auto", false);
        let l: Vec<_> = r.lines().collect();
        assert_eq!(l[1], "1700000000"); assert_eq!(l[2], "1700000000"); assert!(l[3].ends_with('✗'));
        assert!(l[0].starts_with("2023-11-1"));
    }
}
