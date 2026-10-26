//! Console helpers: split a command line like the Redis CLI does, render replies, and
//! classify commands by risk so dangerous ones can be confirmed or refused.
use redis::Value;

/// Split `SET "a b" 'c' d\x41` into arguments. Double quotes take `\n \t \" \\ \xHH`.
pub fn split_line(line: &str) -> Result<Vec<String>, String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut in_arg = false;
    let mut q: Option<char> = None;
    let mut it = line.chars().peekable();
    while let Some(ch) = it.next() {
        match (q, ch) {
            (None, c) if c.is_whitespace() => { if in_arg { out.push(std::mem::take(&mut cur)); in_arg = false; } }
            (None, '"') | (None, '\'') => { q = Some(ch); in_arg = true; }
            (None, c) => { cur.push(c); in_arg = true; }
            (Some(qc), c) if c == qc => { q = None; }
            (Some('"'), '\\') => match it.next() {
                Some('n') => cur.push('\n'), Some('r') => cur.push('\r'), Some('t') => cur.push('\t'),
                Some('x') => {
                    let h: String = [it.next(), it.next()].into_iter().flatten().collect();
                    match u8::from_str_radix(&h, 16) { Ok(b) => cur.push(b as char), Err(_) => return Err(format!("bad escape \\x{h}")) }
                }
                Some(o) => cur.push(o),
                None => return Err("unterminated escape".into()),
            },
            (Some(_), c) => cur.push(c),
        }
    }
    if q.is_some() { return Err("unbalanced quotes".into()); }
    if in_arg { out.push(cur); }
    Ok(out)
}

/// Bytes as text when valid UTF-8, else quoted with `\xHH` escapes.
pub fn bytes_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| if c.is_ascii_graphic() || c == b' ' { (c as char).to_string() } else { format!("\\x{c:02x}") }).collect(),
    }
}

/// Plain text of a scalar reply (None for nil).
pub fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::Nil => None,
        Value::Int(i) => Some(i.to_string()),
        Value::BulkString(b) => Some(bytes_text(b)),
        Value::SimpleString(s) => Some(s.clone()),
        Value::Okay => Some("OK".into()),
        Value::Double(d) => Some(d.to_string()),
        Value::Boolean(b) => Some(b.to_string()),
        Value::VerbatimString { text, .. } => Some(text.clone()),
        other => Some(format!("{other:?}")),
    }
}

/// Reply rendered like the CLI: numbered nested lists, `(nil)`, `(integer) n`.
pub fn render(v: &Value) -> String {
    let mut s = String::new();
    render_into(v, 0, &mut s);
    s.trim_end().to_string()
}

fn render_into(v: &Value, indent: usize, out: &mut String) {
    let items: Option<Vec<&Value>> = match v {
        Value::Array(a) | Value::Set(a) => Some(a.iter().collect()),
        Value::Map(m) => Some(m.iter().flat_map(|(k, v)| [k, v]).collect()),
        Value::Push { data, .. } => Some(data.iter().collect()),
        _ => None,
    };
    match items {
        Some(a) if a.is_empty() => out.push_str("(empty array)\n"),
        Some(a) => {
            let w = a.len().to_string().len();
            for (i, x) in a.iter().enumerate() {
                if i > 0 { out.push_str(&" ".repeat(indent)); }
                let pre = format!("{:>w$}) ", i + 1);
                out.push_str(&pre);
                render_into(x, indent + pre.len(), out);
            }
        }
        None => {
            let line = match v {
                Value::Nil => "(nil)".to_string(),
                Value::Int(i) => format!("(integer) {i}"),
                Value::Double(d) => format!("(double) {d}"),
                Value::Boolean(b) => format!("(boolean) {b}"),
                Value::BulkString(b) => format!("\"{}\"", bytes_text(b).replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")),
                Value::ServerError(e) => format!("(error) {}", e.details().map(|d| format!("{} {d}", e.code())).unwrap_or_else(|| e.code().to_string())),
                other => scalar(other).unwrap_or_default(),
            };
            out.push_str(&line);
            out.push('\n');
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk { Read, Write, Dangerous }

const READS: &[&str] = &["GET", "MGET", "STRLEN", "GETRANGE", "EXISTS", "TYPE", "TTL", "PTTL", "EXPIRETIME", "SCAN", "HSCAN", "SSCAN", "ZSCAN", "HGET", "HMGET", "HGETALL", "HKEYS", "HVALS", "HLEN", "HEXISTS",
    "LRANGE", "LLEN", "LINDEX", "LPOS", "SMEMBERS", "SISMEMBER", "SCARD", "SRANDMEMBER", "SINTER", "SUNION", "SDIFF", "ZRANGE", "ZRANGEBYSCORE", "ZREVRANGE", "ZCARD", "ZSCORE", "ZRANK", "ZREVRANK", "ZCOUNT",
    "XRANGE", "XREVRANGE", "XLEN", "XINFO", "XPENDING", "XREAD", "INFO", "PING", "ECHO", "DBSIZE", "TIME", "MEMORY", "OBJECT", "SLOWLOG", "CLIENT", "COMMAND", "SELECT", "RANDOMKEY", "DUMP", "LASTSAVE",
    "ROLE", "LOLWUT", "PUBSUB", "BITCOUNT", "GETBIT", "BITPOS", "PFCOUNT", "GEOPOS", "GEODIST", "GEOSEARCH", "HSTRLEN", "ZMSCORE", "SMISMEMBER", "LCS", "TOUCH", "HELLO", "AUTH", "READONLY", "CLUSTER"];

/// Risk of one command. Dangerous: flushing, KEYS (blocks the server), CONFIG SET,
/// SHUTDOWN/DEBUG, script flushes, and deletes of more than one key.
pub fn classify(args: &[String]) -> Risk {
    let Some(c) = args.first().map(|s| s.to_ascii_uppercase()) else { return Risk::Read };
    let sub = args.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    match c.as_str() {
        "FLUSHALL" | "FLUSHDB" | "KEYS" | "SHUTDOWN" | "DEBUG" | "SWAPDB" | "MIGRATE" | "REPLICAOF" | "SLAVEOF" | "FAILOVER" | "MONITOR" => Risk::Dangerous,
        "DEL" | "UNLINK" if args.len() > 2 => Risk::Dangerous,
        "CONFIG" if sub != "GET" => Risk::Dangerous,
        "SCRIPT" | "FUNCTION" if sub == "FLUSH" => Risk::Dangerous,
        "CLIENT" if sub == "KILL" || sub == "PAUSE" => Risk::Dangerous,
        "CLUSTER" if ["RESET", "FAILOVER", "FORGET", "FLUSHSLOTS", "DELSLOTS"].contains(&sub.as_str()) => Risk::Dangerous,
        "CONFIG" | "SLOWLOG" if sub == "GET" => Risk::Read,
        "SLOWLOG" if sub == "RESET" => Risk::Write,
        "CLIENT" | "CLUSTER" | "MEMORY" | "OBJECT" | "XINFO" | "COMMAND" | "PUBSUB" => Risk::Read,
        c if READS.contains(&c) => Risk::Read,
        _ => Risk::Write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splitting() {
        assert_eq!(split_line(r#"SET "a b" 'c d' e\x"#).unwrap(), vec!["SET", "a b", "c d", "e\\x"]);
        assert_eq!(split_line(r#"SET k "x\n\x41""#).unwrap(), vec!["SET", "k", "x\nA"]);
        assert_eq!(split_line(r#"SET k """#).unwrap(), vec!["SET", "k", ""]);
        assert!(split_line("GET \"x").is_err());
    }
    #[test]
    fn risk() {
        let a = |s: &str| classify(&split_line(s).unwrap());
        assert_eq!(a("get x"), Risk::Read);
        assert_eq!(a("set x 1"), Risk::Write);
        assert_eq!(a("del x"), Risk::Write);
        assert_eq!(a("del x y"), Risk::Dangerous);
        assert_eq!(a("flushdb"), Risk::Dangerous);
        assert_eq!(a("KEYS *"), Risk::Dangerous);
        assert_eq!(a("config get maxmemory"), Risk::Read);
        assert_eq!(a("config set maxmemory 1"), Risk::Dangerous);
        assert_eq!(a("slowlog get 10"), Risk::Read);
    }
    #[test]
    fn rendering() {
        let v = Value::Array(vec![Value::BulkString(b"a".to_vec()), Value::Int(3), Value::Nil, Value::Array(vec![Value::Okay])]);
        assert_eq!(render(&v), "1) \"a\"\n2) (integer) 3\n3) (nil)\n4) 1) OK");
        assert_eq!(render(&Value::Array(vec![])), "(empty array)");
        assert_eq!(bytes_text(&[0x61, 0xff]), "a\\xff");
    }
}
