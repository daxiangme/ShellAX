//! asciicast v2 recording and playback (format per the public asciinema spec; own code).
//! Recording redacts secrets by default (ai::redact) and drops output while the terminal
//! has echo off (password prompts), replacing it with a marker.
use std::io::Write;

pub struct Recorder { w: std::io::BufWriter<std::fs::File>, start: std::time::Instant, pub redact: bool, hidden: bool,
    /// Redacting: output is held until end of line so a secret split across reads is still caught.
    pending: String, pending_t: f64 }
impl Recorder {
    pub fn create(path: &std::path::Path, cols: u16, rows: u16, title: &str, redact: bool) -> std::io::Result<Self> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
        let mut w = std::io::BufWriter::new(std::fs::File::create(path)?);
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        writeln!(w, "{}", serde_json::json!({"version": 2, "width": cols, "height": rows, "timestamp": ts, "title": title, "env": {"TERM": "xterm-256color"}}))?;
        Ok(Self { w, start: std::time::Instant::now(), redact, hidden: false, pending: String::new(), pending_t: 0. })
    }
    /// `echo_off` = the pty is in no-echo mode (password entry): output is hidden.
    pub fn output(&mut self, data: &[u8], echo_off: bool) -> std::io::Result<()> {
        self.event_at(self.start.elapsed().as_secs_f64(), "o", data, echo_off)
    }
    pub fn event_at(&mut self, t: f64, kind: &str, data: &[u8], echo_off: bool) -> std::io::Result<()> {
        if echo_off && self.redact {
            if !self.hidden { self.hidden = true; writeln!(self.w, "{}", serde_json::json!([round(t), kind, "[input hidden]"]))?; }
            return Ok(());
        }
        self.hidden = false;
        let s = String::from_utf8_lossy(data);
        let s = if self.redact && kind == "o" {
            if self.pending.is_empty() { self.pending_t = t; }
            self.pending.push_str(&s);
            let cut = match self.pending.rfind('\n') { Some(i) => i + 1, None if self.pending.len() > 8192 => self.pending.len(), None => return Ok(()) };
            let line: String = self.pending.drain(..cut).collect();
            crate::ai::redact(&line).0
        } else if self.redact { crate::ai::redact(&s).0 } else { s.into_owned() };
        writeln!(self.w, "{}", serde_json::json!([round(t), kind, s]))?;
        // keep the file usable while recording (crash / live tail)
        self.w.flush()
    }
    pub fn resize(&mut self, cols: u16, rows: u16) -> std::io::Result<()> { writeln!(self.w, "{}", serde_json::json!([round(self.start.elapsed().as_secs_f64()), "r", format!("{cols}x{rows}")])) }
    pub fn finish(mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() { let p = crate::ai::redact(&std::mem::take(&mut self.pending)).0; writeln!(self.w, "{}", serde_json::json!([round(self.pending_t), "o", p]))?; }
        self.w.flush()
    }
}
fn round(t: f64) -> f64 { (t * 1e6).round() / 1e6 }

#[derive(Debug, Clone, PartialEq)]
pub struct Cast { pub width: u16, pub height: u16, pub title: String, pub events: Vec<(f64, String, String)> }
pub fn parse(text: &str) -> anyhow::Result<Cast> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let h: serde_json::Value = serde_json::from_str(lines.next().ok_or_else(|| anyhow::anyhow!("empty cast"))?)?;
    anyhow::ensure!(h["version"] == 2, "not asciicast v2");
    let mut events = vec![];
    for l in lines { let v: (f64, String, String) = serde_json::from_str(l)?; events.push(v); }
    Ok(Cast { width: h["width"].as_u64().unwrap_or(80) as u16, height: h["height"].as_u64().unwrap_or(24) as u16, title: h["title"].as_str().unwrap_or("").into(), events })
}
impl Cast {
    /// Output bytes due at playback time `t` seconds (speed applied by the caller), from index `from`.
    /// Returns (bytes, next index). Idle gaps longer than `max_idle` are compressed.
    pub fn due(&self, from: usize, t: f64, max_idle: f64) -> (String, usize) {
        let mut out = String::new(); let mut i = from;
        while i < self.events.len() && self.compressed_time(i, max_idle) <= t { if self.events[i].1 == "o" { out.push_str(&self.events[i].2); } i += 1; }
        (out, i)
    }
    pub fn compressed_time(&self, i: usize, max_idle: f64) -> f64 {
        let mut acc = 0.; let mut prev = 0.;
        for e in &self.events[..=i.min(self.events.len().saturating_sub(1))] { acc += (e.0 - prev).min(max_idle); prev = e.0; }
        acc
    }
    pub fn duration(&self, max_idle: f64) -> f64 { if self.events.is_empty() { 0. } else { self.compressed_time(self.events.len() - 1, max_idle) } }
}

#[cfg(test)]
mod tests {
    #[test]
    fn record_redact_and_replay() {
        let p = std::env::temp_dir().join(format!("cast-{}.cast", std::process::id()));
        let mut r = super::Recorder::create(&p, 80, 24, "t", true).unwrap();
        r.event_at(0.1, "o", b"$ export API_KEY=sk-abcdefghijklmnopqrstuvwx\r\n", false).unwrap();
        r.event_at(0.15, "o", b"DEMO_TOKEN=fa", false).unwrap();
        r.event_at(0.16, "o", b"ke-split-secret\r\n", false).unwrap();
        r.event_at(0.2, "o", b"Password: ", false).unwrap();
        r.event_at(0.3, "o", b"hunter2", true).unwrap();
        r.event_at(0.4, "o", b"\r\n", true).unwrap();
        r.event_at(30.0, "o", b"ok\r\n", false).unwrap();
        r.finish().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("sk-abcdefghijklmnopqrstuvwx") && !text.contains("hunter2") && !text.contains("split-secret"), "{text}");
        assert_eq!(text.matches("[input hidden]").count(), 1);
        let c = super::parse(&text).unwrap();
        assert_eq!((c.width, c.height, c.events.len()), (80, 24, 4));
        let (s, i) = c.due(0, 0.25, 2.0); assert!(s.contains("[REDACTED]") && i == 2, "{s}");
        assert!((c.duration(2.0) - 2.3).abs() < 1e-6, "idle compressed");
        let (s, i) = c.due(i, 10.0, 2.0); assert!(s.contains("[input hidden]") && s.contains("Password: [REDACTED]") && i == 4, "{s}");
        assert!(super::parse("{\"version\":1}").is_err());
    }
}
