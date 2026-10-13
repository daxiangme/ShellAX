//! Inline graphics (own code): Sixel (DEC DCS `ESC P … q … ESC \`) and the Kitty graphics
//! protocol (APC `ESC _G key=val,…;base64 ESC \`, transmit+display, f=24/32/100, chunked m=1).
//! `Extractor` pulls these sequences out of the PTY byte stream (the rest goes to the
//! terminal emulator unchanged) and yields decoded images: RGBA pixels, or PNG bytes.

#[derive(Clone, Debug, PartialEq)]
pub enum Pixels { Rgba { w: u32, h: u32, data: Vec<u8> }, Png(Vec<u8>) }
#[derive(Clone, Debug, PartialEq)]
pub struct Image { pub pixels: Pixels, pub proto: &'static str, /// Offset in the passthrough bytes of this feed where the image belongs.
    pub at: usize }

#[derive(Default)]
pub struct Extractor { state: St, buf: Vec<u8>, kitty_chunks: Vec<u8>, kitty_keys: String }
#[derive(Default, PartialEq, Clone, Copy)]
enum St { #[default] Ground, Esc, Dcs, Apc, DcsEsc, ApcEsc }

impl Image {
    /// Pixel size (PNG: read from the IHDR chunk).
    pub fn size(&self) -> (u32, u32) {
        match &self.pixels { Pixels::Rgba { w, h, .. } => (*w, *h), Pixels::Png(b) if b.len() >= 24 => (u32::from_be_bytes([b[16], b[17], b[18], b[19]]), u32::from_be_bytes([b[20], b[21], b[22], b[23]])), Pixels::Png(_) => (0, 0) }
    }
}
impl Extractor {
    /// Inside a sequence that spans feeds.
    pub fn busy(&self) -> bool { self.state != St::Ground }
    /// Returns (passthrough bytes, completed images).
    pub fn feed(&mut self, data: &[u8]) -> (Vec<u8>, Vec<Image>) {
        let mut out = Vec::with_capacity(data.len()); let mut imgs = vec![];
        for &b in data {
            match self.state {
                St::Ground => if b == 0x1b { self.state = St::Esc } else { out.push(b) },
                St::Esc => match b {
                    b'P' => { self.state = St::Dcs; self.buf.clear(); }
                    b'_' => { self.state = St::Apc; self.buf.clear(); }
                    _ => { out.push(0x1b); out.push(b); self.state = if b == 0x1b { out.pop(); St::Esc } else { St::Ground }; }
                },
                St::Dcs | St::Apc => {
                    if b == 0x1b { self.state = if self.state == St::Dcs { St::DcsEsc } else { St::ApcEsc }; }
                    else if b == 0x9c || b == 0x07 && self.state == St::Apc { self.finish(self.state == St::Dcs, &mut out, &mut imgs); }
                    else if self.buf.len() < 64 << 20 { self.buf.push(b) }
                }
                St::DcsEsc | St::ApcEsc => {
                    let dcs = self.state == St::DcsEsc;
                    if b == b'\\' { self.finish(dcs, &mut out, &mut imgs); } else { self.buf.push(0x1b); self.buf.push(b); self.state = if dcs { St::Dcs } else { St::Apc }; }
                }
            }
        }
        (out, imgs)
    }
    fn finish(&mut self, dcs: bool, out: &mut Vec<u8>, imgs: &mut Vec<Image>) {
        self.state = St::Ground;
        let body = std::mem::take(&mut self.buf);
        if dcs {
            // Sixel: params then 'q'. Anything else is passed through untouched.
            let q = body.iter().position(|c| !(c.is_ascii_digit() || *c == b';'));
            if q.is_some_and(|i| body[i] == b'q') { if let Some(p) = decode_sixel(&body[q.unwrap() + 1..]) { imgs.push(Image { pixels: p, proto: "sixel", at: out.len() }); } return; }
            out.extend_from_slice(b"\x1bP"); out.extend_from_slice(&body); out.extend_from_slice(b"\x1b\\");
        } else if body.first() == Some(&b'G') {
            let s = String::from_utf8_lossy(&body[1..]).into_owned();
            let (keys, payload) = s.split_once(';').unwrap_or((&s, ""));
            if self.kitty_chunks.is_empty() { self.kitty_keys = keys.to_string(); }
            self.kitty_chunks.extend_from_slice(payload.as_bytes());
            if kv(keys, "m") == Some("1") { return; }
            let keys = std::mem::take(&mut self.kitty_keys); let b64 = std::mem::take(&mut self.kitty_chunks);
            if let Some(p) = decode_kitty(&keys, &b64) { imgs.push(Image { pixels: p, proto: "kitty", at: out.len() }); }
        } // other APC strings are dropped (as xterm does)
    }
}
fn kv<'a>(keys: &'a str, k: &str) -> Option<&'a str> { keys.split(',').find_map(|p| p.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v)) }

pub fn decode_kitty(keys: &str, b64: &[u8]) -> Option<Pixels> {
    let a = kv(keys, "a").unwrap_or("t");
    if !matches!(a, "T" | "t" | "p") || kv(keys, "t").is_some_and(|t| t != "d") || kv(keys, "o").is_some() { return None; }
    let raw = base64(b64)?;
    match kv(keys, "f").unwrap_or("32") {
        "100" => raw.starts_with(b"\x89PNG").then_some(Pixels::Png(raw)),
        f @ ("24" | "32") => {
            let (w, h): (u32, u32) = (kv(keys, "s")?.parse().ok()?, kv(keys, "v")?.parse().ok()?);
            let bpp = if f == "24" { 3 } else { 4 };
            if raw.len() < (w * h) as usize * bpp || w * h > 16 << 20 { return None; }
            let data = if bpp == 4 { raw[..(w * h * 4) as usize].to_vec() } else { raw.chunks(3).take((w * h) as usize).flat_map(|c| [c[0], c[1], c[2], 255]).collect() };
            Some(Pixels::Rgba { w, h, data })
        }
        _ => None,
    }
}
pub fn base64(s: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| Some(match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' => 62, b'/' => 63, _ => return None });
    let mut out = vec![]; let (mut acc, mut n) = (0u32, 0);
    for &c in s.iter().filter(|c| !c.is_ascii_whitespace() && **c != b'=') { acc = acc << 6 | val(c)? as u32; n += 6; if n >= 8 { n -= 8; out.push((acc >> n) as u8); } }
    Some(out)
}

/// Decode sixel data (after 'q') into RGBA. Supports raster attributes, color registers
/// (RGB % and HLS), repeat, carriage return and newline.
pub fn decode_sixel(d: &[u8]) -> Option<Pixels> {
    let mut pal: Vec<[u8; 3]> = (0..256).map(|i| { let v = [0u8, 205, 0, 205, 0, 205, 0, 229][i % 8]; [v, v, v] }).collect();
    let mut color = 0usize; let (mut x, mut y) = (0usize, 0usize);
    let mut px: std::collections::HashMap<(usize, usize), usize> = Default::default();
    let (mut w, mut h) = (0usize, 0usize); let mut i = 0;
    let num = |i: &mut usize| -> Vec<usize> { let mut v = vec![]; let mut cur: Option<usize> = None; while *i < d.len() && (d[*i].is_ascii_digit() || d[*i] == b';') { if d[*i] == b';' { v.push(cur.take().unwrap_or(0)); } else { cur = Some(cur.unwrap_or(0).saturating_mul(10).saturating_add((d[*i] - b'0') as usize)); } *i += 1; } if let Some(c) = cur { v.push(c) } v };
    let put = |x: usize, y: usize, bits: u8, c: usize, px: &mut std::collections::HashMap<(usize, usize), usize>, w: &mut usize, h: &mut usize| { for b in 0..6 { if bits >> b & 1 == 1 && x < 4096 && y + b < 4096 { px.insert((x, y + b), c); *w = (*w).max(x + 1); *h = (*h).max(y + b + 1); } } };
    while i < d.len() {
        let c = d[i]; i += 1;
        match c {
            b'"' => { let v = num(&mut i); if v.len() >= 4 { w = w.max(v[2].min(4096)); h = h.max(v[3].min(4096)); } }
            b'#' => { let v = num(&mut i); if let Some(&r) = v.first() { color = r % 256; if v.len() >= 5 { let s = |p: usize| (p.min(100) * 255 / 100) as u8; pal[color] = if v[1] == 2 { [s(v[2]), s(v[3]), s(v[4])] } else { hls(v[2], v[3], v[4]) }; } } }
            b'!' => { let n = num(&mut i).first().copied().unwrap_or(1).min(4096); if i < d.len() && (0x3f..=0x7e).contains(&d[i]) { let bits = d[i] - 0x3f; i += 1; for _ in 0..n { put(x, y, bits, color, &mut px, &mut w, &mut h); x += 1; } } }
            b'$' => x = 0,
            b'-' => { x = 0; y += 6; }
            0x3f..=0x7e => { put(x, y, c - 0x3f, color, &mut px, &mut w, &mut h); x += 1; }
            _ => {}
        }
    }
    if w == 0 || h == 0 { return None; }
    let mut data = vec![0u8; w * h * 4];
    for ((x, y), c) in px { if x < w && y < h { let o = (y * w + x) * 4; data[o..o + 3].copy_from_slice(&pal[c]); data[o + 3] = 255; } }
    Some(Pixels::Rgba { w: w as u32, h: h as u32, data })
}
fn hls(hd: usize, l: usize, s: usize) -> [u8; 3] {
    let (h, l, s) = (((hd + 240) % 360) as f32 / 360., l.min(100) as f32 / 100., s.min(100) as f32 / 100.);
    if s == 0. { let v = (l * 255.) as u8; return [v, v, v]; }
    let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s }; let p = 2. * l - q;
    let f = |mut t: f32| { if t < 0. { t += 1. } else if t > 1. { t -= 1. } (255. * if t < 1. / 6. { p + (q - p) * 6. * t } else if t < 0.5 { q } else if t < 2. / 3. { p + (q - p) * (2. / 3. - t) * 6. } else { p }) as u8 };
    [f(h + 1. / 3.), f(h), f(h - 1. / 3.)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sixel_and_kitty() {
        let mut e = Extractor::default();
        // 2x6 red sixel block between text, split across feeds.
        let (o1, i1) = e.feed(b"hi\x1bPq\"1;1;2;6#1;2;100;0;0#1!2");
        let (o2, i2) = e.feed(b"~\x1b\\there\x1b[0m");
        assert_eq!([o1, o2].concat(), b"hithere\x1b[0m".to_vec());
        assert!(i1.is_empty() && i2.len() == 1 && i2[0].at == 0);
        let Pixels::Rgba { w, h, data } = &i2[0].pixels else { panic!() };
        assert_eq!((*w, *h), (2, 6)); assert_eq!(&data[..4], &[255, 0, 0, 255]);
        // Kitty: 1x1 RGB pixel in two chunks.
        let (o, im) = e.feed(b"a\x1b_Ga=T,f=24,s=1,v=1,m=1;AP8\x1b\\\x1b_Gm=0;A\x1b\\b");
        assert_eq!(o, b"ab"); assert_eq!(im[0].at, 1); assert_eq!(im[0].pixels, Pixels::Rgba { w: 1, h: 1, data: vec![0, 255, 0, 255] });
        assert_eq!(base64(b"aGk=").unwrap(), b"hi");
        // Non-sixel DCS passes through.
        let (o, im) = e.feed(b"\x1bP+q544e\x1b\\"); assert!(im.is_empty() && o == b"\x1bP+q544e\x1b\\");
    }
}
