//! Network calculations, a public-IP geolocation lookup, and a one-shot TCP latency probe.
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use super::{Args, Output, Row, RowFmt, Tone};
use crate::i18n::{t, tf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Public,
    Private,
    Loopback,
    LinkLocal,
    Multicast,
    Unspecified,
    Reserved,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Public => t("tools.net_kind_public"),
            Self::Private => t("tools.net_kind_private"),
            Self::Loopback => t("tools.net_kind_loopback"),
            Self::LinkLocal => t("tools.net_kind_link"),
            Self::Multicast => t("tools.net_kind_multicast"),
            Self::Unspecified => t("tools.net_kind_unspecified"),
            Self::Reserved => t("tools.net_kind_reserved"),
        }
    }
    fn queries_geo(self) -> bool {
        self == Self::Public
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Geo {
    country: String,
    region: String,
    city: String,
    isp: String,
}

static GEO_CACHE: Mutex<Option<HashMap<String, Geo>>> = Mutex::new(None);

fn kind_of(ip: IpAddr) -> Kind {
    match ip {
        IpAddr::V4(v) => kind_v4(v),
        IpAddr::V6(v) => kind_v6(v),
    }
}

fn kind_v4(v: Ipv4Addr) -> Kind {
    let o = v.octets();
    if v.is_unspecified() {
        Kind::Unspecified
    } else if v.is_loopback() {
        Kind::Loopback
    } else if v.is_private() || (o[0] == 100 && (o[1] & 0xc0) == 64) {
        Kind::Private
    } else if v.is_link_local() {
        Kind::LinkLocal
    } else if v.is_multicast() || v.is_broadcast() {
        Kind::Multicast
    } else if is_reserved_v4(o) {
        Kind::Reserved
    } else {
        Kind::Public
    }
}

fn is_reserved_v4(o: [u8; 4]) -> bool {
    // TEST-NET, benchmarking, IETF protocol assignments.
    (o[0] == 192 && o[1] == 0 && o[2] == 2)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        || (o[0] == 198 && o[1] >= 18 && o[1] <= 19)
        || o[0] == 0
        || o[0] >= 240
}

fn kind_v6(v: Ipv6Addr) -> Kind {
    let s = v.segments();
    if v.is_unspecified() {
        Kind::Unspecified
    } else if v.is_loopback() {
        Kind::Loopback
    } else if (s[0] & 0xfe00) == 0xfc00 {
        Kind::Private
    } else if (s[0] & 0xffc0) == 0xfe80 {
        Kind::LinkLocal
    } else if v.is_multicast() {
        Kind::Multicast
    } else if s[0] == 0x2001 && s[1] == 0x0db8 {
        Kind::Reserved
    } else {
        Kind::Public
    }
}

fn parse_ip(s: &str) -> Result<IpAddr> {
    let s = s.trim();
    if s.is_empty() {
        bail!(t("tools.net_need_ip"));
    }
    s.parse::<IpAddr>()
        .or_else(|_| {
            s.split_once('/')
                .map(|(ip, _)| ip)
                .unwrap_or(s)
                .parse::<IpAddr>()
        })
        .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_ip", &[("v", &s)])))
}

pub fn lookup_rows(a: &Args) -> Result<Output> {
    let mut rows = Vec::new();
    if a.on("mine") {
        let geo = fetch_geo("")?;
        rows.push(Row::kv(t("tools.net_address"), &geo.ip).primary());
        rows.push(Row::kv(t("tools.net_kind"), Kind::Public.label()).tag(t("tools.net_kind_public"), Tone::Good));
        push_geo(&mut rows, &geo.place);
        return Ok(Output::kv(rows, RowFmt::Colon));
    }
    let ip = parse_ip(a.get("input"))?;
    let kind = kind_of(ip);
    rows.push(Row::kv(t("tools.net_address"), ip.to_string()).primary());
    rows.push(Row::kv(t("tools.net_version"), if ip.is_ipv4() { "IPv4" } else { "IPv6" }));
    rows.push(Row::kv(t("tools.net_kind"), kind.label()));
    if kind.queries_geo() {
        match fetch_geo(&ip.to_string()) {
            Ok(g) => push_geo(&mut rows, &g.place),
            Err(e) => {
                rows.push(
                    Row::kv(t("tools.net_geo"), e.to_string()).tone(Tone::Warn),
                );
            }
        }
    }
    Ok(Output::kv(rows, RowFmt::Colon))
}

struct Fetched {
    ip: String,
    place: Geo,
}

fn push_geo(rows: &mut Vec<Row>, g: &Geo) {
    if !g.country.is_empty() {
        rows.push(Row::kv(t("tools.net_country"), &g.country));
    }
    if !g.region.is_empty() {
        rows.push(Row::kv(t("tools.net_region"), &g.region));
    }
    if !g.city.is_empty() {
        rows.push(Row::kv(t("tools.net_city"), &g.city));
    }
    if !g.isp.is_empty() {
        rows.push(Row::kv(t("tools.net_isp"), &g.isp));
    }
}

fn fetch_geo(ip: &str) -> Result<Fetched> {
    if !ip.is_empty() {
        if let Some(hit) = GEO_CACHE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|m| m.get(ip).cloned()))
        {
            return Ok(Fetched {
                ip: ip.to_string(),
                place: hit,
            });
        }
    }
    let url = if ip.is_empty() {
        "https://ipwho.is/".to_string()
    } else {
        format!("https://ipwho.is/{ip}")
    };
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(8))
        .build();
    let body = agent
        .get(&url)
        .call()
        .map_err(|e| anyhow::anyhow!(tf("tools.net_geo_failed", &[("e", &e.to_string())])))?
        .into_string()
        .map_err(|e| anyhow::anyhow!(tf("tools.net_geo_failed", &[("e", &e.to_string())])))?;
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!(tf("tools.net_geo_failed", &[("e", &e.to_string())])))?;
    if v.get("success").and_then(|s| s.as_bool()) == Some(false) {
        let msg = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("lookup failed");
        bail!(tf("tools.net_geo_failed", &[("e", &msg)]));
    }
    let place = Geo {
        country: text_of(&v, "country"),
        region: text_of(&v, "region"),
        city: text_of(&v, "city"),
        isp: v
            .pointer("/connection/isp")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
    };
    let got = v
        .get("ip")
        .and_then(|s| s.as_str())
        .unwrap_or(ip)
        .to_string();
    if !got.is_empty() {
        if let Ok(mut g) = GEO_CACHE.lock() {
            g.get_or_insert_with(HashMap::new)
                .insert(got.clone(), place.clone());
        }
    }
    Ok(Fetched { ip: got, place })
}

fn text_of(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

pub fn iface_rows() -> Output {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut rows = Vec::new();
    for (name, data) in &networks {
        let mac = data.mac_address().to_string();
        let ips = data.ip_networks();
        if ips.is_empty() {
            rows.push(Row::kv(name.to_string(), mac));
            continue;
        }
        for ip in ips {
            rows.push(
                Row::kv(name.to_string(), format!("{}/{}", ip.addr, ip.prefix)).hint(mac.clone()),
            );
        }
    }
    if rows.is_empty() {
        return Output::text(t("tools.net_no_iface").to_string());
    }
    Output::kv(rows, RowFmt::Colon)
}

#[derive(Clone, Copy)]
struct Net {
    start: u128,
    end: u128,
    prefix: u8,
    v6: bool,
}

fn parse_net(s: &str) -> Result<Net> {
    let s = s.trim();
    if s.is_empty() {
        bail!(t("tools.net_need_cidr"));
    }
    let (addr, pref) = match s.split_once('/') {
        Some((a, p)) => (a.trim(), p.trim()),
        None => bail!(tf("tools.net_bad_cidr", &[("v", &s)])),
    };
    let ip: IpAddr = addr
        .parse()
        .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_cidr", &[("v", &s)])))?;
    let prefix: u8 = pref
        .parse()
        .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_cidr", &[("v", &s)])))?;
    let (bits, width, v6) = match ip {
        IpAddr::V4(v) => (u32::from(v) as u128, 32u8, false),
        IpAddr::V6(v) => (u128::from(v), 128u8, true),
    };
    if prefix > width {
        bail!(tf("tools.net_bad_cidr", &[("v", &s)]));
    }
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (width - prefix)
    };
    let start = bits & mask;
    let end = start | !mask;
    // For IPv4 the unused high bits of the mask shift must stay zero above bit 32.
    let start = if v6 { start } else { start & 0xffff_ffff };
    let end = if v6 { end } else { end & 0xffff_ffff };
    Ok(Net {
        start,
        end,
        prefix,
        v6,
    })
}

fn show_addr(n: &Net, bits: u128) -> String {
    if n.v6 {
        Ipv6Addr::from(bits).to_string()
    } else {
        Ipv4Addr::from(bits as u32).to_string()
    }
}

pub fn cidr_rows(a: &Args) -> Result<Output> {
    let n = parse_net(a.get("input"))?;
    let width = if n.v6 { 128u8 } else { 32 };
    let mask_bits = if n.prefix == 0 {
        0
    } else {
        u128::MAX << (width - n.prefix)
    };
    let mask_bits = if n.v6 { mask_bits } else { mask_bits & 0xffff_ffff };
    let wild = (!mask_bits) & if n.v6 { u128::MAX } else { 0xffff_ffff };
    let size = if n.prefix == 0 && !n.v6 {
        1u128 << 32
    } else if n.prefix == 0 && n.v6 {
        // 2^128 does not fit. Show the exponent instead of a number.
        0
    } else {
        1u128 << (width - n.prefix)
    };
    let mut rows = vec![
        Row::kv(t("tools.net_network"), show_addr(&n, n.start)).primary(),
        Row::kv(t("tools.net_prefix"), n.prefix.to_string()),
    ];
    if !n.v6 {
        rows.push(Row::kv(t("tools.net_mask"), show_addr(&n, mask_bits)));
        rows.push(Row::kv(t("tools.net_wildcard"), show_addr(&n, wild)));
        rows.push(Row::kv(t("tools.net_broadcast"), show_addr(&n, n.end)));
    }
    let (first, last, hosts) = usable(&n, size);
    rows.push(Row::kv(t("tools.net_first"), first));
    rows.push(Row::kv(t("tools.net_last"), last));
    rows.push(Row::kv(
        t("tools.net_count"),
        if n.v6 && n.prefix == 0 {
            "2^128".into()
        } else {
            hosts.to_string()
        },
    ));
    Ok(Output::kv(rows, RowFmt::Colon))
}

fn usable(n: &Net, size: u128) -> (String, String, u128) {
    if n.v6 || n.prefix >= 31 {
        (show_addr(n, n.start), show_addr(n, n.end), size.max(1))
    } else if size >= 2 {
        (
            show_addr(n, n.start + 1),
            show_addr(n, n.end - 1),
            size - 2,
        )
    } else {
        (show_addr(n, n.start), show_addr(n, n.end), size)
    }
}

pub fn overlap_rows(a: &Args) -> Result<Output> {
    let x = parse_net(a.get("a"))?;
    let y = parse_net(a.get("b"))?;
    if x.v6 != y.v6 {
        bail!(t("tools.net_family"));
    }
    let overlap = x.start <= y.end && y.start <= x.end;
    let x_has_y = x.start <= y.start && x.end >= y.end;
    let y_has_x = y.start <= x.start && y.end >= x.end;
    let yes = t("tools.net_yes");
    let no = t("tools.net_no");
    let flag = |on| if on { yes } else { no };
    Ok(Output::kv(
        vec![
            Row::kv(t("tools.net_overlap"), flag(overlap)).primary(),
            Row::kv(t("tools.net_a_contains"), flag(x_has_y)),
            Row::kv(t("tools.net_b_contains"), flag(y_has_x)),
        ],
        RowFmt::Colon,
    ))
}

pub fn convert_rows(a: &Args) -> Result<Output> {
    let mode = a.get("mode");
    let input = a.get("input").trim();
    if input.is_empty() {
        bail!(t("tools.net_need_value"));
    }
    let rows = match mode {
        "mask_to_prefix" => {
            let ip: Ipv4Addr = input
                .parse()
                .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_mask", &[("v", &input)])))?;
            let prefix = prefix_of_mask(u32::from(ip))?;
            vec![Row::kv(t("tools.net_prefix"), prefix.to_string()).primary()]
        }
        "ipv4_to_int" => {
            let ip: Ipv4Addr = input
                .parse()
                .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_ip", &[("v", &input)])))?;
            let n = u32::from(ip);
            vec![
                Row::kv(t("tools.net_decimal"), n.to_string()).primary(),
                Row::kv(t("tools.net_hex"), format!("0x{n:08x}")),
            ]
        }
        "int_to_ipv4" => {
            let n = parse_u32(input)?;
            vec![Row::kv(t("tools.net_address"), Ipv4Addr::from(n).to_string()).primary()]
        }
        _ => {
            let prefix: u8 = input
                .parse()
                .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_prefix", &[("v", &input)])))?;
            if prefix > 32 {
                bail!(tf("tools.net_bad_prefix", &[("v", &input)]));
            }
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            vec![
                Row::kv(t("tools.net_mask"), Ipv4Addr::from(mask).to_string()).primary(),
                Row::kv(
                    t("tools.net_wildcard"),
                    Ipv4Addr::from(!mask).to_string(),
                ),
            ]
        }
    };
    Ok(Output::kv(rows, RowFmt::Colon))
}

fn prefix_of_mask(mask: u32) -> Result<u8> {
    let inv = !mask;
    if inv & (inv + 1) != 0 {
        bail!(t("tools.net_mask_holes"));
    }
    Ok(mask.count_ones() as u8)
}

const LATENCY_TIMEOUT: Duration = Duration::from_secs(2);
const LATENCY_COUNT_MAX: i64 = 8;

/// 一次探测。拒绝也算主机有响应，超时和其他错误不计入平均。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Ok(Duration),
    Refused(Duration),
    Lost,
}

fn normalize_host(raw: &str) -> Result<&str> {
    let s = raw.trim();
    if s.is_empty() {
        bail!(t("tools.net_need_host"));
    }
    if s.contains('/') || s.contains(char::is_whitespace) {
        bail!(tf("tools.net_bad_host", &[("v", &s)]));
    }
    let s = s.trim_matches(|c| c == '[' || c == ']');
    if s.is_empty() {
        bail!(tf("tools.net_bad_host", &[("v", &raw.trim())]));
    }
    Ok(s)
}

fn port_of(a: &Args) -> Result<u16> {
    let raw = a.get("port").trim();
    if raw.is_empty() {
        return Ok(443);
    }
    let n = a.num("port")?.round() as i64;
    if (1..=65535).contains(&n) {
        Ok(n as u16)
    } else {
        bail!(tf("tools.net_bad_port", &[("v", &raw)]))
    }
}

fn probe_count(a: &Args) -> Result<usize> {
    if a.get("count").trim().is_empty() {
        return Ok(4);
    }
    Ok(a.int("count", 1, LATENCY_COUNT_MAX)? as usize)
}

fn resolve_once(host: &str, port: u16) -> Result<(SocketAddr, Duration)> {
    let started = Instant::now();
    let mut addrs = (host, port).to_socket_addrs().map_err(|e| {
        anyhow::anyhow!(tf(
            "tools.net_resolve_failed",
            &[("e", &e.to_string())],
        ))
    })?;
    let addr = addrs.next().ok_or_else(|| {
        anyhow::anyhow!(tf("tools.net_resolve_failed", &[("e", &host)]))
    })?;
    Ok((addr, started.elapsed()))
}

fn is_refused(err: &std::io::Error) -> bool {
    err.kind() == ErrorKind::ConnectionRefused
        || matches!(err.raw_os_error(), Some(61 | 111 | 10061))
}

fn probe_once(addr: SocketAddr, timeout: Duration) -> Probe {
    let started = Instant::now();
    match TcpStream::connect_timeout(&addr, timeout) {
        Ok(stream) => {
            let elapsed = started.elapsed();
            drop(stream);
            Probe::Ok(elapsed)
        }
        Err(e) if is_refused(&e) => Probe::Refused(started.elapsed()),
        Err(_) => Probe::Lost,
    }
}

fn fmt_ms(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 100.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.0} ms")
    }
}

fn average(samples: &[Duration]) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    let sum: u128 = samples.iter().map(|d| d.as_nanos()).sum();
    Some(Duration::from_nanos((sum / samples.len() as u128) as u64))
}

fn latency_output(addr: SocketAddr, dns: Duration, probes: &[Probe]) -> Output {
    let responded: Vec<Duration> = probes
        .iter()
        .filter_map(|p| match p {
            Probe::Ok(d) | Probe::Refused(d) => Some(*d),
            Probe::Lost => None,
        })
        .collect();
    let ok_n = probes.iter().filter(|p| matches!(p, Probe::Ok(_))).count();
    let refused_n = probes
        .iter()
        .filter(|p| matches!(p, Probe::Refused(_)))
        .count();
    let lost_n = probes.len() - ok_n - refused_n;
    let mut rows = vec![
        Row::kv(t("tools.net_latency_resolved"), addr.to_string()),
        Row::kv(t("tools.net_latency_dns"), fmt_ms(dns)),
    ];
    if let Some(avg) = average(&responded) {
        rows.push(Row::kv(t("tools.net_latency_avg"), fmt_ms(avg)).primary());
        rows.push(Row::kv(
            t("tools.net_latency_min"),
            fmt_ms(responded.iter().copied().min().unwrap_or(avg)),
        ));
        rows.push(Row::kv(
            t("tools.net_latency_max"),
            fmt_ms(responded.iter().copied().max().unwrap_or(avg)),
        ));
    }
    rows.push(Row::kv(t("tools.net_latency_ok"), ok_n.to_string()));
    if refused_n > 0 {
        rows.push(Row::kv(
            t("tools.net_latency_refused"),
            refused_n.to_string(),
        ));
    }
    rows.push(Row::kv(t("tools.net_latency_timeout"), lost_n.to_string()));
    for (i, probe) in probes.iter().enumerate() {
        let label = tf("tools.net_latency_probe", &[("n", &(i + 1))]);
        rows.push(match probe {
            Probe::Ok(d) => Row::kv(label, fmt_ms(*d)).tone(Tone::Good),
            Probe::Refused(d) => Row::kv(label, fmt_ms(*d))
                .tag(t("tools.net_latency_port_closed"), Tone::Warn)
                .tone(Tone::Warn),
            Probe::Lost => Row::kv(label, t("tools.net_latency_lost")).tone(Tone::Danger),
        });
    }
    let (tone, banner) = if probes.is_empty() || lost_n == probes.len() {
        (Tone::Danger, t("tools.net_latency_none"))
    } else if lost_n > 0 {
        (Tone::Warn, t("tools.net_latency_partial"))
    } else if ok_n == 0 {
        (Tone::Good, t("tools.net_latency_refused_all"))
    } else if refused_n > 0 {
        (Tone::Good, t("tools.net_latency_responded"))
    } else {
        (Tone::Good, t("tools.net_latency_all_ok"))
    };
    Output::kv(rows, RowFmt::Colon).verdict(tone, banner)
}

/// 对一个地址的一个端口做几次 TCP 连接，汇总最小、平均、最大耗时。
pub fn latency_rows(a: &Args) -> Result<Output> {
    let host = normalize_host(a.get("input"))?;
    let port = port_of(a)?;
    let count = probe_count(a)?;
    let (addr, dns) = resolve_once(host, port)?;
    let probes: Vec<Probe> = (0..count)
        .map(|_| probe_once(addr, LATENCY_TIMEOUT))
        .collect();
    Ok(latency_output(addr, dns, &probes))
}

fn parse_u32(s: &str) -> Result<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
            .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_int", &[("v", &s)])))
    } else {
        s.parse::<u32>()
            .map_err(|_| anyhow::anyhow!(tf("tools.net_bad_int", &[("v", &s)])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_without_a_lookup() {
        assert_eq!(kind_of("10.1.2.3".parse().unwrap()), Kind::Private);
        assert_eq!(kind_of("127.0.0.1".parse().unwrap()), Kind::Loopback);
        assert_eq!(kind_of("169.254.1.1".parse().unwrap()), Kind::LinkLocal);
        assert_eq!(kind_of("224.0.0.1".parse().unwrap()), Kind::Multicast);
        assert_eq!(kind_of("192.0.2.1".parse().unwrap()), Kind::Reserved);
        assert_eq!(kind_of("100.64.0.1".parse().unwrap()), Kind::Private);
        assert_eq!(kind_of("8.8.8.8".parse().unwrap()), Kind::Public);
        assert_eq!(kind_of("fe80::1".parse().unwrap()), Kind::LinkLocal);
        assert_eq!(kind_of("fd00::1".parse().unwrap()), Kind::Private);
        assert!(!Kind::Private.queries_geo());
        assert!(Kind::Public.queries_geo());
    }

    #[test]
    fn cidr_v4_usable_hosts() {
        let mut a = Args::default();
        a.set("input", "192.168.1.20/24");
        let o = cidr_rows(&a).unwrap();
        let val = |label: &str| {
            o.rows
                .iter()
                .find(|r| r.label == label)
                .map(|r| r.value.clone())
                .unwrap_or_default()
        };
        assert_eq!(val(t("tools.net_network")), "192.168.1.0");
        assert_eq!(val(t("tools.net_mask")), "255.255.255.0");
        assert_eq!(val(t("tools.net_wildcard")), "0.0.0.255");
        assert_eq!(val(t("tools.net_broadcast")), "192.168.1.255");
        assert_eq!(val(t("tools.net_first")), "192.168.1.1");
        assert_eq!(val(t("tools.net_last")), "192.168.1.254");
        assert_eq!(val(t("tools.net_count")), "254");
    }

    #[test]
    fn overlap_and_contain() {
        let mut a = Args::default();
        a.set("a", "10.0.0.0/8").set("b", "10.1.0.0/16");
        let o = overlap_rows(&a).unwrap();
        assert_eq!(o.rows[0].value, t("tools.net_yes"));
        assert_eq!(o.rows[1].value, t("tools.net_yes"));
        assert_eq!(o.rows[2].value, t("tools.net_no"));
        a.set("b", "11.0.0.0/8");
        let o = overlap_rows(&a).unwrap();
        assert_eq!(o.rows[0].value, t("tools.net_no"));
    }

    #[test]
    fn converts_prefix_mask_and_int() {
        let mut a = Args::default();
        a.set("mode", "prefix_to_mask").set("input", "24");
        let o = convert_rows(&a).unwrap();
        assert_eq!(o.rows[0].value, "255.255.255.0");
        a.set("mode", "mask_to_prefix").set("input", "255.255.255.0");
        assert_eq!(convert_rows(&a).unwrap().rows[0].value, "24");
        a.set("mode", "ipv4_to_int").set("input", "10.0.0.1");
        assert_eq!(convert_rows(&a).unwrap().rows[0].value, "167772161");
        a.set("mode", "int_to_ipv4").set("input", "167772161");
        assert_eq!(convert_rows(&a).unwrap().rows[0].value, "10.0.0.1");
        a.set("mode", "mask_to_prefix").set("input", "255.0.255.0");
        assert!(convert_rows(&a).is_err());
    }

    fn row<'a>(o: &'a Output, label: &str) -> &'a Row {
        o.rows
            .iter()
            .find(|r| r.label == label)
            .unwrap_or_else(|| panic!("missing {label}"))
    }

    #[test]
    fn latency_formats_milliseconds() {
        assert_eq!(fmt_ms(Duration::from_millis(10)), "10.0 ms");
        assert_eq!(fmt_ms(Duration::from_millis(100)), "100 ms");
        assert_eq!(fmt_ms(Duration::from_micros(400)), "0.4 ms");
    }

    #[test]
    fn latency_summary_mixes_success_refused_and_loss() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 443));
        let o = latency_output(
            addr,
            Duration::from_millis(5),
            &[
                Probe::Ok(Duration::from_millis(10)),
                Probe::Refused(Duration::from_millis(30)),
                Probe::Lost,
            ],
        );
        assert_eq!(o.verdict.as_ref().unwrap().tone, Tone::Warn);
        assert_eq!(o.verdict.as_ref().unwrap().text, t("tools.net_latency_partial"));
        assert_eq!(row(&o, t("tools.net_latency_resolved")).value, "127.0.0.1:443");
        assert_eq!(row(&o, t("tools.net_latency_dns")).value, "5.0 ms");
        assert!(row(&o, t("tools.net_latency_avg")).primary);
        assert_eq!(row(&o, t("tools.net_latency_avg")).value, "20.0 ms");
        assert_eq!(row(&o, t("tools.net_latency_min")).value, "10.0 ms");
        assert_eq!(row(&o, t("tools.net_latency_max")).value, "30.0 ms");
        assert_eq!(row(&o, t("tools.net_latency_ok")).value, "1");
        assert_eq!(row(&o, t("tools.net_latency_refused")).value, "1");
        assert_eq!(row(&o, t("tools.net_latency_timeout")).value, "1");
        let refused = o.rows.iter().find(|r| r.tag.is_some()).unwrap();
        assert_eq!(refused.value, "30.0 ms");
        assert_eq!(
            refused.tag.as_ref().unwrap().0,
            t("tools.net_latency_port_closed")
        );
        assert!(o.rows.iter().any(|r| r.value == t("tools.net_latency_lost")));
    }

    #[test]
    fn latency_summary_banner_follows_responses() {
        let addr = SocketAddr::from(([127, 0, 0, 1], 9));
        let all_ok = latency_output(
            addr,
            Duration::ZERO,
            &[Probe::Ok(Duration::from_millis(10)), Probe::Ok(Duration::from_millis(10))],
        );
        assert_eq!(all_ok.verdict.as_ref().unwrap().tone, Tone::Good);
        assert_eq!(all_ok.verdict.as_ref().unwrap().text, t("tools.net_latency_all_ok"));
        let refused = latency_output(addr, Duration::ZERO, &[Probe::Refused(Duration::from_millis(8))]);
        assert_eq!(refused.verdict.as_ref().unwrap().tone, Tone::Good);
        assert_eq!(
            refused.verdict.as_ref().unwrap().text,
            t("tools.net_latency_refused_all")
        );
        assert_eq!(row(&refused, t("tools.net_latency_avg")).value, "8.0 ms");
        let lost = latency_output(addr, Duration::ZERO, &[Probe::Lost, Probe::Lost]);
        assert_eq!(lost.verdict.as_ref().unwrap().tone, Tone::Danger);
        assert!(lost.rows.iter().all(|r| r.label != t("tools.net_latency_avg")));
    }

    #[test]
    fn latency_rejects_empty_cidr_and_bad_port_without_connecting() {
        assert_eq!(
            latency_rows(&Args::default()).unwrap_err().to_string(),
            t("tools.net_need_host")
        );
        let mut a = Args::default();
        a.set("input", "10.0.0.0/24");
        assert_eq!(
            latency_rows(&a).unwrap_err().to_string(),
            tf("tools.net_bad_host", &[("v", &"10.0.0.0/24")])
        );
        a.set("input", "127.0.0.1").set("port", "0");
        assert!(latency_rows(&a).is_err());
        a.set("port", "65536");
        assert!(latency_rows(&a).is_err());
        assert_eq!(port_of(&Args::default()).unwrap(), 443);
        a.set("count", "100");
        assert_eq!(probe_count(&a).unwrap(), 8);
        a.set("count", "");
        assert_eq!(probe_count(&a).unwrap(), 4);
        assert_eq!(normalize_host("[::1]").unwrap(), "::1");
    }

    #[test]
    fn latency_reaches_a_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut a = Args::default();
        a.set("input", "127.0.0.1")
            .set("port", port.to_string())
            .set("count", "1");
        let o = latency_rows(&a).unwrap();
        assert_eq!(
            o.verdict.as_ref().map(|v| v.tone),
            Some(Tone::Good),
            "{}",
            o.text
        );
        assert_eq!(o.verdict.as_ref().unwrap().text, t("tools.net_latency_all_ok"));
        assert_eq!(row(&o, t("tools.net_latency_ok")).value, "1");
        assert_eq!(row(&o, t("tools.net_latency_timeout")).value, "0");
        assert!(row(&o, t("tools.net_latency_resolved"))
            .value
            .starts_with("127.0.0.1:"));
    }

    #[test]
    fn latency_counts_a_closed_local_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        // Some Windows stacks drop the first SYN and refuse on the retransmit,
        // which can take about two seconds. Wait past that so the RST is visible.
        let probe = probe_once(addr, Duration::from_secs(4));
        assert!(matches!(probe, Probe::Refused(_)), "{probe:?}");
        let o = latency_output(addr, Duration::ZERO, &[probe]);
        assert_eq!(
            o.verdict.as_ref().unwrap().text,
            t("tools.net_latency_refused_all"),
            "{}",
            o.text
        );
        assert_eq!(row(&o, t("tools.net_latency_ok")).value, "0");
        assert_eq!(row(&o, t("tools.net_latency_timeout")).value, "0");
        assert_eq!(row(&o, t("tools.net_latency_refused")).value, "1");
        assert!(o.rows.iter().any(|r| {
            r.tag
                .as_ref()
                .is_some_and(|tag| tag.0 == t("tools.net_latency_port_closed"))
        }));
    }
}
