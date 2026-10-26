//! 开发类工具：格式化、类型生成、cron、命名、颜色、进制。
use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use crate::i18n::{t, tf};

fn json_err(e: serde_json::Error) -> anyhow::Error { anyhow!(tf("tools.err_json", &[("line", &e.line()), ("col", &e.column()), ("msg", &e)])) }

fn sort_value(v: Value) -> Value {
    match v {
        Value::Object(m) => { let mut e: Vec<_> = m.into_iter().collect(); e.sort_by(|a, b| a.0.cmp(&b.0)); Value::Object(e.into_iter().map(|(k, v)| (k, sort_value(v))).collect()) }
        Value::Array(a) => Value::Array(a.into_iter().map(sort_value).collect()),
        v => v,
    }
}

pub fn json(s: &str, minify: bool, indent: &str, sort: bool) -> Result<String> {
    if s.trim().is_empty() { return Ok(String::new()); }
    let mut v: Value = serde_json::from_str(s).map_err(json_err)?;
    if sort { v = sort_value(v); }
    if minify { return Ok(serde_json::to_string(&v)?); }
    let ind: Vec<u8> = match indent { "4" => b"    ".to_vec(), "tab" => b"\t".to_vec(), _ => b"  ".to_vec() };
    let mut buf = vec![];
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::with_indent(&ind));
    serde::Serialize::serialize(&v, &mut ser)?;
    Ok(String::from_utf8(buf)?)
}

fn pascal(s: &str) -> String {
    let p = split_words(s).iter().map(|w| cap(w)).collect::<String>();
    if p.is_empty() { "Item".into() } else if p.chars().next().unwrap().is_ascii_digit() { format!("T{p}") } else { p }
}
fn cap(w: &str) -> String { let mut c = w.chars(); match c.next() { Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(), None => String::new() } }

/// JSON 样例 → TypeScript interface。
pub fn json_ts(s: &str, root: &str) -> Result<String> {
    if s.trim().is_empty() { return Ok(String::new()); }
    let v: Value = serde_json::from_str(s).map_err(json_err)?;
    let mut defs: Vec<(String, String)> = vec![];
    fn ty(v: &Value, name: &str, defs: &mut Vec<(String, String)>) -> String {
        match v {
            Value::Null => "null".into(), Value::Bool(_) => "boolean".into(), Value::Number(_) => "number".into(), Value::String(_) => "string".into(),
            Value::Array(a) => {
                if a.is_empty() { return "unknown[]".into(); }
                // 对象数组：合并字段
                if a.iter().all(|x| x.is_object()) {
                    let mut merged = serde_json::Map::new();
                    for x in a { for (k, v) in x.as_object().unwrap() { merged.entry(k.clone()).or_insert(v.clone()); } }
                    let item = ty(&Value::Object(merged), &format!("{name}Item"), defs);
                    return format!("{item}[]");
                }
                let mut ts: Vec<String> = a.iter().map(|x| ty(x, &format!("{name}Item"), defs)).collect();
                ts.dedup(); ts.sort(); ts.dedup();
                if ts.len() == 1 { format!("{}[]", ts[0]) } else { format!("({})[]", ts.join(" | ")) }
            }
            Value::Object(m) => {
                let name = pascal(name);
                let mut body = String::new();
                for (k, v) in m {
                    let key = if k.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$') && !k.starts_with(|c: char| c.is_ascii_digit()) { k.clone() } else { format!("{k:?}") };
                    body.push_str(&format!("  {key}: {};\n", ty(v, &format!("{name}{}", pascal(k)), defs)));
                }
                let mut nm = name.clone(); let mut i = 2;
                while defs.iter().any(|d| d.0 == nm) { nm = format!("{name}{i}"); i += 1; }
                defs.push((nm.clone(), body));
                nm
            }
        }
    }
    let top = ty(&v, if root.trim().is_empty() { "Root" } else { root.trim() }, &mut defs);
    let mut out: Vec<String> = defs.iter().rev().map(|(n, b)| format!("export interface {n} {{\n{b}}}")).collect();
    if !v.is_object() { out.push(format!("export type {} = {top};", pascal(if root.trim().is_empty() { "Root" } else { root }))); }
    Ok(out.join("\n\n"))
}

/// JSON 样例 → Go struct。
pub fn json_to_go(s: &str, root: &str) -> Result<String> {
    if s.trim().is_empty() { return Ok(String::new()); }
    let v: Value = serde_json::from_str(s).map_err(json_err)?;
    let mut defs: Vec<(String, String)> = vec![];
    fn go_name(k: &str) -> String {
        split_words(k).iter().map(|w| { let l = w.to_lowercase(); if ["id", "url", "http", "api", "json", "uuid", "ip", "sql"].contains(&l.as_str()) { l.to_uppercase() } else { cap(w) } }).collect::<String>()
    }
    fn ty(v: &Value, name: &str, defs: &mut Vec<(String, String)>) -> String {
        match v {
            Value::Null => "interface{}".into(), Value::Bool(_) => "bool".into(), Value::String(_) => "string".into(),
            Value::Number(n) => if n.is_f64() { "float64".into() } else { "int64".into() },
            Value::Array(a) => match a.first() {
                None => "[]interface{}".into(),
                Some(Value::Object(_)) => {
                    let mut merged = serde_json::Map::new();
                    for x in a { if let Some(o) = x.as_object() { for (k, v) in o { merged.entry(k.clone()).or_insert(v.clone()); } } }
                    format!("[]{}", ty(&Value::Object(merged), name, defs))
                }
                Some(f) => format!("[]{}", ty(f, name, defs)),
            },
            Value::Object(m) => {
                let name = pascal(name);
                let w = m.keys().map(|k| go_name(k).len()).max().unwrap_or(0);
                let mut body = String::new();
                for (k, v) in m {
                    let f = { let g = go_name(k); if g.is_empty() || g.starts_with(|c: char| c.is_ascii_digit()) { format!("F{g}") } else { g } };
                    let tt = ty(v, &f, defs);
                    body.push_str(&format!("\t{f:w$} {tt} `json:\"{k}\"`\n"));
                }
                defs.push((name.clone(), body));
                name
            }
        }
    }
    let r = if root.trim().is_empty() { "Root" } else { root.trim() };
    let top = ty(&v, r, &mut defs);
    let mut out: Vec<String> = defs.iter().rev().map(|(n, b)| format!("type {n} struct {{\n{b}}}")).collect();
    if !v.is_object() { out.insert(0, format!("type {} {top}", pascal(r))); }
    Ok(out.join("\n\n"))
}

/// Go struct → JSON 样例（按字段类型给出示例值，读取 json 标签）。
pub fn go_to_json(s: &str) -> Result<String> {
    let st = regex::Regex::new(r"type\s+(\w+)\s+struct\s*\{").unwrap();
    let field = regex::Regex::new(r#"^\s*(\w+)\s+([\[\]\*\w\.\{\}]+)(?:\s+`([^`]*)`)?"#).unwrap();
    let tag = regex::Regex::new(r#"json:"([^"]*)""#).unwrap();
    // 解析所有结构体
    let mut structs: Vec<(String, Vec<(String, String)>)> = vec![];
    let mut cur: Option<(String, Vec<(String, String)>)> = None;
    for line in s.lines() {
        if let Some(c) = st.captures(line) { if let Some(x) = cur.take() { structs.push(x); } cur = Some((c[1].to_string(), vec![])); continue; }
        if line.trim_start().starts_with('}') { if let Some(x) = cur.take() { structs.push(x); } continue; }
        if let (Some((_, fields)), Some(c)) = (cur.as_mut(), field.captures(line)) {
            let (name, ty) = (c[1].to_string(), c[2].to_string());
            if name == "type" || name.starts_with("//") { continue; }
            let key = match c.get(3).and_then(|tg| tag.captures(tg.as_str())) {
                Some(j) => { let k = j[1].split(',').next().unwrap_or("").to_string(); if k == "-" { continue; } if k.is_empty() { name.clone() } else { k } }
                None => name.clone(),
            };
            if name.chars().next().is_some_and(|c| c.is_lowercase()) && c.get(3).is_none() { continue; }
            fields.push((key, ty));
        }
    }
    if let Some(x) = cur.take() { structs.push(x); }
    if structs.is_empty() { bail!(t("tools.err_no_struct")); }
    fn sample(ty: &str, structs: &[(String, Vec<(String, String)>)], depth: usize) -> Value {
        let ty = ty.trim_start_matches('*');
        if let Some(inner) = ty.strip_prefix("[]") { return Value::Array(vec![sample(inner, structs, depth)]); }
        if ty.starts_with("map[") { return Value::Object(Default::default()); }
        match ty {
            "string" => "".into(), "bool" => false.into(),
            "int" | "int8" | "int16" | "int32" | "int64" | "uint" | "uint8" | "uint16" | "uint32" | "uint64" | "byte" | "rune" => 0.into(),
            "float32" | "float64" => 0.0.into(),
            "time.Time" => "2006-01-02T15:04:05Z".into(),
            other => match structs.iter().find(|s| s.0 == other) {
                Some((_, fs)) if depth < 8 => Value::Object(fs.iter().map(|(k, t)| (k.clone(), sample(t, structs, depth + 1))).collect()),
                _ => Value::Null,
            },
        }
    }
    let v = sample(&structs[0].0, &structs, 0);
    Ok(serde_json::to_string_pretty(&v)?)
}

pub fn sql(s: &str, minify: bool, upper: bool) -> String {
    if minify {
        // 合并空白（保留字符串字面量），去掉注释
        let mut out = String::new();
        let cs: Vec<char> = s.chars().collect();
        let mut i = 0; let mut space = false;
        while i < cs.len() {
            let c = cs[i];
            if c == '\'' || c == '"' || c == '`' {
                if space && !out.is_empty() { out.push(' '); } space = false;
                out.push(c); i += 1;
                while i < cs.len() { out.push(cs[i]); if cs[i] == c { if i + 1 < cs.len() && cs[i + 1] == c { out.push(c); i += 2; continue; } break; } i += 1; }
                i += 1; continue;
            }
            if c == '-' && cs.get(i + 1) == Some(&'-') { while i < cs.len() && cs[i] != '\n' { i += 1; } space = true; continue; }
            if c == '/' && cs.get(i + 1) == Some(&'*') { i += 2; while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') { i += 1; } i += 2; space = true; continue; }
            if c.is_whitespace() { space = true; i += 1; continue; }
            if space && !out.is_empty() && !"(),;".contains(c) && !out.ends_with('(') { out.push(' '); }
            space = false; out.push(c); i += 1;
        }
        return out;
    }
    let opts = sqlformat::FormatOptions { uppercase: Some(upper), lines_between_queries: 2, ..Default::default() };
    sqlformat::format(s, &sqlformat::QueryParams::None, &opts)
}

/// CSS 格式化/压缩：按字符扫描，保留字符串与注释（压缩时去掉注释）。
pub fn css(s: &str, minify: bool) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut depth = 0usize;
    let mut i = 0;
    let ind = |d: usize| "  ".repeat(d);
    let mut pending_space = false;
    while i < cs.len() {
        let c = cs[i];
        if c == '"' || c == '\'' {
            if pending_space && !out.is_empty() && !out.ends_with(['\n', ' ', '(', '{', '}', ';', ':', ',']) { out.push(' '); } pending_space = false;
            out.push(c); i += 1;
            while i < cs.len() { out.push(cs[i]); if cs[i] == '\\' && i + 1 < cs.len() { out.push(cs[i + 1]); i += 2; continue; } if cs[i] == c { break; } i += 1; }
            i += 1; continue;
        }
        if c == '/' && cs.get(i + 1) == Some(&'*') {
            let start = i; i += 2;
            while i + 1 < cs.len() && !(cs[i] == '*' && cs[i + 1] == '/') { i += 1; }
            i = (i + 2).min(cs.len());
            if !minify { let cm: String = cs[start..i].iter().collect(); if !out.is_empty() && !out.ends_with('\n') { out.push('\n'); } out.push_str(&ind(depth)); out.push_str(&cm); out.push('\n'); }
            continue;
        }
        if c.is_whitespace() { pending_space = true; i += 1; continue; }
        match c {
            '{' => {
                let t = out.trim_end().len(); out.truncate(t);
                if minify { out.push('{'); depth += 1; } else { out.push_str(" {\n"); depth += 1; out.push_str(&ind(depth)); }
            }
            '}' => {
                let t = out.trim_end().len(); out.truncate(t);
                if minify { if out.ends_with(';') { out.pop(); } out.push('}'); depth = depth.saturating_sub(1); } else {
                    depth = depth.saturating_sub(1);
                    if !out.ends_with('{') && !out.ends_with(';') && !out.ends_with('}') && !out.ends_with("*/") { out.push(';'); }
                    out.push('\n'); out.push_str(&ind(depth)); out.push_str("}\n"); if depth == 0 { out.push('\n'); } out.push_str(&ind(depth));
                }
            }
            ';' => { let t = out.trim_end().len(); out.truncate(t); out.push(';'); if !minify { out.push('\n'); out.push_str(&ind(depth)); } }
            ':' if depth > 0 => { let t = out.trim_end().len(); out.truncate(t); out.push(':'); if !minify && depth > 0 { out.push(' '); } }
            ',' => { let t = out.trim_end().len(); out.truncate(t); out.push(','); if !minify { out.push(if depth == 0 { '\n' } else { ' ' }); } }
            _ => {
                if pending_space && !out.is_empty() && !out.ends_with(['\n', ' ', '{', '}', ';', ':', ',', '(']) { out.push(' '); }
                out.push(c);
            }
        }
        pending_space = false;
        i += 1;
    }
    let lines: Vec<String> = out.lines().map(|l| l.trim_end().to_string()).collect();
    let mut res = lines.join("\n").trim().to_string();
    while res.contains("\n\n\n") { res = res.replace("\n\n\n", "\n\n"); }
    res
}

const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr", "!doctype"];
const RAW: &[&str] = &["pre", "script", "style", "textarea"];
const INLINE: &[&str] = &["a", "b", "i", "em", "strong", "span", "code", "small", "sub", "sup", "u", "label", "abbr"];

/// HTML 格式化/压缩：标签级缩进，`pre/script/style/textarea` 内容原样保留。
pub fn html(s: &str, minify: bool) -> String {
    #[derive(Debug)]
    enum Tok { Open(String, String, bool), Close(String, String), Text(String), Comment(String), Raw(String) }
    let mut toks = vec![];
    let mut rest = s;
    while !rest.is_empty() {
        if rest.starts_with("<!--") {
            let end = rest.find("-->").map(|e| e + 3).unwrap_or(rest.len());
            toks.push(Tok::Comment(rest[..end].to_string())); rest = &rest[end..]; continue;
        }
        if rest.starts_with('<') && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!') {
            // 找到标签结束（忽略引号内的 >）
            let b = rest.as_bytes(); let mut i = 1; let mut q = 0u8;
            while i < b.len() { let c = b[i]; if q != 0 { if c == q { q = 0; } } else if c == b'"' || c == b'\'' { q = c; } else if c == b'>' { break; } i += 1; }
            let end = (i + 1).min(rest.len());
            let tag = &rest[..end];
            let name: String = tag.trim_start_matches(['<', '/']).chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/').collect::<String>().to_lowercase();
            if tag.starts_with("</") { toks.push(Tok::Close(name, tag.to_string())); rest = &rest[end..]; continue; }
            let self_close = tag.ends_with("/>") || VOID.contains(&name.as_str());
            toks.push(Tok::Open(name.clone(), tag.to_string(), self_close));
            rest = &rest[end..];
            if RAW.contains(&name.as_str()) {
                let close = format!("</{name}");
                let at = rest.to_lowercase().find(&close).unwrap_or(rest.len());
                if at > 0 { toks.push(Tok::Raw(rest[..at].to_string())); }
                rest = &rest[at..];
            }
            continue;
        }
        let next = rest[1..].find('<').map(|x| x + 1).unwrap_or(rest.len());
        toks.push(Tok::Text(rest[..next].to_string())); rest = &rest[next..];
    }
    let ws = regex::Regex::new(r"\s+").unwrap();
    let mut out = String::new();
    let mut depth = 0usize;
    let nl = |out: &mut String, d: usize| { if !out.is_empty() { out.push('\n'); } out.push_str(&"  ".repeat(d)); };
    for tk in toks {
        match tk {
            Tok::Comment(c) => if !minify { nl(&mut out, depth); out.push_str(&c); },
            Tok::Raw(r) => if minify { out.push_str(&r) } else if r.trim().is_empty() {} else { out.push_str(&r) },
            Tok::Text(x) => {
                let x = ws.replace_all(&x, " ");
                if minify { out.push_str(&x); } else { let tx = x.trim(); if !tx.is_empty() { nl(&mut out, depth); out.push_str(tx); } }
            }
            Tok::Open(name, raw, sc) => {
                if minify { out.push_str(&raw); } else { nl(&mut out, depth); out.push_str(&ws.replace_all(&raw, " ")); }
                if !sc && !name.starts_with('!') { depth += 1; }
                let _ = INLINE;
            }
            Tok::Close(name, raw) => {
                depth = depth.saturating_sub(1);
                if minify { out.push_str(&raw); } else if RAW.contains(&name.as_str()) { out.push_str(&raw); } else { nl(&mut out, depth); out.push_str(&raw); }
            }
        }
    }
    if minify { regex::Regex::new(r">\s+<").unwrap().replace_all(out.trim(), "><").into_owned() } else { out }
}

pub fn cron_next(expr: &str, count: usize, now: chrono::DateTime<chrono::Local>) -> Result<String> {
    use croner::parser::{CronParser, Seconds};
    let e = expr.trim();
    if e.is_empty() { bail!(t("tools.err_empty")); }
    let cron = CronParser::builder().seconds(Seconds::Optional).build().parse(e).map_err(|x| anyhow!(tf("tools.err_cron", &[("msg", &x)])))?;
    let mut lines = vec![format!("{}: {}", t("tools.cron_meaning"), cron.describe())];
    for (i, dt) in cron.iter_after(now).take(count).enumerate() { lines.push(format!("{:>3}. {}", i + 1, dt.format("%Y-%m-%d %H:%M:%S %a"))); }
    Ok(lines.join("\n"))
}

/// 拆词：驼峰、下划线、连字符、空格、点。
pub fn split_words(s: &str) -> Vec<String> {
    let mut words = vec![];
    let mut cur = String::new();
    let cs: Vec<char> = s.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        if !c.is_alphanumeric() { if !cur.is_empty() { words.push(std::mem::take(&mut cur)); } continue; }
        if c.is_uppercase() && !cur.is_empty() {
            let prev = cs[i - 1];
            let next_lower = cs.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) { words.push(std::mem::take(&mut cur)); }
        }
        cur.push(c);
    }
    if !cur.is_empty() { words.push(cur); }
    words
}

pub fn naming(s: &str) -> String {
    s.lines().filter(|l| !l.trim().is_empty()).map(|l| {
        let w: Vec<String> = split_words(l).iter().map(|x| x.to_lowercase()).collect();
        if w.is_empty() { return String::new(); }
        let camel = w[0].clone() + &w[1..].iter().map(|x| cap(x)).collect::<String>();
        let pas: String = w.iter().map(|x| cap(x)).collect();
        [("camelCase", camel), ("PascalCase", pas), ("snake_case", w.join("_")), ("SCREAMING_SNAKE", w.join("_").to_uppercase()), ("kebab-case", w.join("-")),
         ("dot.case", w.join(".")), ("Title Case", w.iter().map(|x| cap(x)).collect::<Vec<_>>().join(" ")), ("path/case", w.join("/"))]
            .iter().map(|(k, v)| format!("{k:16} {v}")).collect::<Vec<_>>().join("\n")
    }).collect::<Vec<_>>().join("\n\n")
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let c = (1. - (2. * l - 1.).abs()) * s;
    let hp = (h.rem_euclid(360.)) / 60.;
    let x = c * (1. - (hp % 2. - 1.).abs());
    let (r, g, b) = match hp as u32 { 0 => (c, x, 0.), 1 => (x, c, 0.), 2 => (0., c, x), 3 => (0., x, c), 4 => (x, 0., c), _ => (c, 0., x) };
    let m = l - c / 2.;
    (r + m, g + m, b + m)
}
fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.;
    if mx == mn { return (0., 0., l); }
    let d = mx - mn;
    let s = if l > 0.5 { d / (2. - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r { (g - b) / d + if g < b { 6. } else { 0. } } else if mx == g { (b - r) / d + 2. } else { (r - g) / d + 4. };
    (h * 60., s, l)
}

/// 解析 HEX / rgb() / rgba() / hsl() / hsla()，输出各种格式与 RGBA 值。
pub fn color(s: &str) -> Result<(String, [u8; 4])> {
    let s = s.trim().to_lowercase();
    let bad = || anyhow!(tf("tools.err_color", &[("v", &s)]));
    let nums = |inner: &str| -> Vec<f64> { inner.split([',', ' ', '/']).filter(|x| !x.is_empty()).map(|x| { let p = x.ends_with('%'); let v: f64 = x.trim_end_matches(['%', 'g', 'e', 'd']).parse().unwrap_or(f64::NAN); if p { v / 100. } else { v } }).collect() };
    let (r, g, b, a): (f64, f64, f64, f64) = if let Some(h) = s.strip_prefix('#').or_else(|| if s.chars().all(|c| c.is_ascii_hexdigit()) && [3, 4, 6, 8].contains(&s.len()) { Some(s.as_str()) } else { None }) {
        let h: String = if h.len() <= 4 { h.chars().flat_map(|c| [c, c]).collect() } else { h.to_string() };
        if !(h.len() == 6 || h.len() == 8) || !h.chars().all(|c| c.is_ascii_hexdigit()) { return Err(bad()); }
        let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap() as f64;
        (p(0) / 255., p(2) / 255., p(4) / 255., if h.len() == 8 { p(6) / 255. } else { 1. })
    } else if let Some(inner) = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb(")).and_then(|x| x.strip_suffix(')')) {
        let raw: Vec<&str> = inner.split([',', ' ', '/']).filter(|x| !x.is_empty()).collect();
        let v = nums(inner);
        if v.len() < 3 || v.iter().any(|x| x.is_nan()) { return Err(bad()); }
        let ch = |i: usize| if raw[i].ends_with('%') { v[i] } else { v[i] / 255. };
        (ch(0), ch(1), ch(2), v.get(3).copied().unwrap_or(1.))
    } else if let Some(inner) = s.strip_prefix("hsla(").or_else(|| s.strip_prefix("hsl(")).and_then(|x| x.strip_suffix(')')) {
        let v = nums(inner);
        if v.len() < 3 || v.iter().any(|x| x.is_nan()) { return Err(bad()); }
        let (sat, l) = (if v[1] > 1. { v[1] / 100. } else { v[1] }, if v[2] > 1. { v[2] / 100. } else { v[2] });
        let (r, g, b) = hsl_to_rgb(v[0], sat, l);
        (r, g, b, v.get(3).copied().unwrap_or(1.))
    } else { return Err(bad()); };
    let c8 = |x: f64| (x.clamp(0., 1.) * 255.).round() as u8;
    let (r8, g8, b8, a8) = (c8(r), c8(g), c8(b), c8(a));
    let (h, sl, l) = rgb_to_hsl(r8 as f64 / 255., g8 as f64 / 255., b8 as f64 / 255.);
    let mx = (r8.max(g8).max(b8)) as f64 / 255.;
    let sv = if mx == 0. { 0. } else { (mx - (r8.min(g8).min(b8)) as f64 / 255.) / mx };
    let mut lines = vec![format!("HEX   #{r8:02x}{g8:02x}{b8:02x}"), format!("HEX   #{r8:02X}{g8:02X}{b8:02X}")];
    if a8 < 255 { lines.push(format!("HEXA  #{r8:02x}{g8:02x}{b8:02x}{a8:02x}")); }
    lines.push(format!("RGB   rgb({r8}, {g8}, {b8})"));
    if a8 < 255 { lines.push(format!("RGBA  rgba({r8}, {g8}, {b8}, {:.2})", a)); }
    lines.push(format!("HSL   hsl({}, {}%, {}%)", h.round(), (sl * 100.).round(), (l * 100.).round()));
    lines.push(format!("HSV   hsv({}, {}%, {}%)", h.round(), (sv * 100.).round(), (mx * 100.).round()));
    lines.push(format!("ARGB  0x{a8:02X}{r8:02X}{g8:02X}{b8:02X}"));
    Ok((lines.join("\n"), [r8, g8, b8, a8]))
}

fn to_base(mut n: u128, b: u32) -> String {
    if n == 0 { return "0".into(); }
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut v = vec![];
    while n > 0 { v.push(digits[(n % b as u128) as usize]); n /= b as u128; }
    v.reverse();
    String::from_utf8(v).unwrap()
}

pub fn radix(s: &str, from: &str) -> Result<String> {
    let raw: String = s.trim().chars().filter(|c| !matches!(c, '_' | ' ' | ',')).collect();
    if raw.is_empty() { return Ok(String::new()); }
    let (neg, body) = match raw.strip_prefix('-') { Some(r) => (true, r.to_string()), None => (false, raw.clone()) };
    let lower = body.to_lowercase();
    let (base, digits) = match from {
        "auto" | "" => if let Some(x) = lower.strip_prefix("0x") { (16, x.to_string()) } else if let Some(x) = lower.strip_prefix("0b") { (2, x.to_string()) } else if let Some(x) = lower.strip_prefix("0o") { (8, x.to_string()) } else { (10, lower.clone()) },
        b => { let b: u32 = b.parse().map_err(|_| anyhow!(tf("tools.err_bad_number", &[("v", &b)])))?; (b, lower.trim_start_matches("0x").trim_start_matches("0b").to_string()) }
    };
    let n = u128::from_str_radix(&digits, base).map_err(|_| anyhow!(tf("tools.err_radix", &[("v", &body), ("b", &base)])))?;
    let sg = if neg { "-" } else { "" };
    let mut lines = vec![];
    for (b, name) in [(2, "BIN"), (8, "OCT"), (10, "DEC"), (16, "HEX"), (32, "B32"), (36, "B36")] { lines.push(format!("{name:4} {sg}{}", to_base(n, b))); }
    lines.push(format!("HEX  {sg}0x{}", to_base(n, 16).to_uppercase()));
    if !neg && n <= u32::MAX as u128 { lines.push(format!("{} {}", t("tools.radix_bits"), format!("{:032b}", n as u32).as_bytes().chunks(8).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" "))); }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_fmt() {
        assert_eq!(json("{\"b\":1,\"a\":[1,2]}", true, "2", false).unwrap(), "{\"b\":1,\"a\":[1,2]}");
        assert_eq!(json("{\"b\":1,\"a\":2}", true, "2", true).unwrap(), "{\"a\":2,\"b\":1}");
        assert_eq!(json("{\"a\":1}", false, "4", false).unwrap(), "{\n    \"a\": 1\n}");
        let e = json("{\"a\":}", false, "2", false).unwrap_err().to_string();
        assert!(e.contains('1'), "{e}");
    }
    #[test]
    fn types() {
        let ts = json_ts(r#"{"id":1,"name":"x","tags":["a"],"owner":{"login":"y"},"items":[{"n":1},{"m":true}]}"#, "User").unwrap();
        assert!(ts.contains("export interface User {") && ts.contains("owner: UserOwner;") && ts.contains("tags: string[];"), "{ts}");
        assert!(ts.contains("n: number;") && ts.contains("m: boolean;"));
        let go = json_to_go(r#"{"user_id":1,"score":1.5,"tags":["a"],"profile":{"home_url":"x"}}"#, "Root").unwrap();
        assert!(go.contains("UserID") && go.contains("float64") && go.contains("[]string") && go.contains("HomeURL") && go.contains("`json:\"home_url\"`"), "{go}");
        let j = go_to_json("type A struct {\n Name string `json:\"name\"`\n Age int `json:\"age,omitempty\"`\n Skip int `json:\"-\"`\n B B\n}\ntype B struct {\n X []bool `json:\"x\"`\n}").unwrap();
        let v: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["name"], ""); assert_eq!(v["age"], 0); assert!(v.get("Skip").is_none()); assert_eq!(v["B"]["x"][0], false);
    }
    #[test]
    fn formatters() {
        let s = sql("select a,b from t where x=1", false, true);
        assert!(s.contains("SELECT") && s.contains('\n'));
        assert_eq!(sql("select  a ,\n b -- c\n from t where s = 'a  b'", true, false), "select a, b from t where s = 'a  b'");
        let c = css("a,b{color:red;margin:0 auto}/*x*/ .c{top:0}", false);
        assert!(c.contains("a,\nb {\n  color: red;\n  margin: 0 auto;\n}"), "{c}");
        assert_eq!(css("a { color : red ; }\n/* c */ .b { x: 'a b' }", true), "a{color:red}.b{x:'a b'}");
        let h = html("<div><p>Hi <b>x</b></p><br><pre>  a\n b</pre></div>", false);
        assert!(h.contains("<div>\n  <p>\n    Hi") && h.contains("<pre>  a\n b</pre>"), "{h}");
        assert_eq!(html("<div>\n  <p> a  b </p>\n</div><!-- c -->", true), "<div><p> a b </p></div>");
    }
    #[test]
    fn crons() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 1, 1, 0, 0, 30).unwrap();
        let r = cron_next("*/15 * * * *", 3, now).unwrap();
        assert!(r.contains("2026-01-01 00:15:00") && r.contains("2026-01-01 00:45:00"), "{r}");
        assert!(cron_next("61 * * * *", 3, now).is_err());
    }
    #[test]
    fn names_colors_radix() {
        assert_eq!(split_words("parseHTTPResponse_code"), ["parse", "HTTP", "Response", "code"]);
        let n = naming("user name");
        assert!(n.contains("userName") && n.contains("UserName") && n.contains("USER_NAME") && n.contains("user-name"));
        let (s, c) = color("#3b82f6").unwrap();
        assert_eq!(c, [0x3b, 0x82, 0xf6, 255]);
        assert!(s.contains("rgb(59, 130, 246)") && s.contains("hsl(217, 91%, 60%)"), "{s}");
        assert_eq!(color("rgb(255, 0, 0)").unwrap().1, [255, 0, 0, 255]);
        assert_eq!(color("hsl(120, 100%, 50%)").unwrap().1, [0, 255, 0, 255]);
        assert_eq!(color("#f008").unwrap().1, [255, 0, 0, 0x88]);
        assert!(color("nope").is_err());
        let r = radix("0xff", "auto").unwrap();
        assert!(r.contains("BIN  11111111") && r.contains("DEC  255") && r.contains("OCT  377"));
        assert!(radix("102", "2").is_err());
        assert!(radix("-10", "10").unwrap().contains("BIN  -1010"));
    }
}
