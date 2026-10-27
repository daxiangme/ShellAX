//! 图片与条码：生成/识别二维码、生成条码、图片压缩、图片水印。
use std::io::Cursor;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use image::{DynamicImage, GrayImage, ImageFormat, Luma, Rgba, RgbaImage};
use shellax_ext_sdk::{self as sdk, serde_json::Value, Reply, Request, Result};

fn err<E: std::fmt::Display>(e: E) -> String { e.to_string() }

pub fn png(img: &DynamicImage) -> Result<Vec<u8>> {
    let mut v = vec![];
    img.write_to(&mut Cursor::new(&mut v), ImageFormat::Png).map_err(err)?;
    Ok(v)
}

pub fn load(data: &[u8], zh: bool) -> Result<DynamicImage> {
    image::load_from_memory(data).map_err(|e| if zh { format!("无法读取图片：{e}") } else { format!("Can't read image: {e}") })
}

// ---------- 二维码 ----------

pub fn qr_png(text: &str, ec: &str, scale: u32, margin: u32) -> Result<DynamicImage> {
    use qrcode::{Color, EcLevel, QrCode};
    let lvl = match ec { "L" => EcLevel::L, "Q" => EcLevel::Q, "H" => EcLevel::H, _ => EcLevel::M };
    let code = QrCode::with_error_correction_level(text.as_bytes(), lvl).map_err(err)?;
    let w = code.width() as u32;
    let colors = code.to_colors();
    let size = (w + margin * 2) * scale;
    let mut img = GrayImage::from_pixel(size, size, Luma([255]));
    for y in 0..w {
        for x in 0..w {
            if colors[(y * w + x) as usize] == Color::Dark {
                for dy in 0..scale { for dx in 0..scale { img.put_pixel((x + margin) * scale + dx, (y + margin) * scale + dy, Luma([0])); } }
            }
        }
    }
    Ok(DynamicImage::ImageLuma8(img))
}

pub fn qr_decode(img: &DynamicImage) -> Vec<String> {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let Some(src) = image024::RgbaImage::from_raw(w, h, rgba.into_raw()) else { return vec![] };
    let dec = bardecoder::default_decoder();
    let mut out: Vec<String> = dec.decode(&image024::DynamicImage::ImageRgba8(src)).into_iter().filter_map(|r| r.ok()).map(|s| latin1_to_utf8(&s)).collect();
    out.dedup();
    out
}

/// 识别库按 Latin-1 解出字节模式；内容实际多为 UTF-8，能还原时就还原。
fn latin1_to_utf8(s: &str) -> String {
    if s.chars().all(|c| (c as u32) < 256) {
        let b: Vec<u8> = s.chars().map(|c| c as u32 as u8).collect();
        if let Ok(u) = String::from_utf8(b) { return u; }
    }
    s.to_string()
}

// ---------- 条码 ----------

pub fn barcode_bars(kind: &str, data: &str) -> Result<Vec<u8>> {
    use barcoders::sym::*;
    let d = data.trim();
    Ok(match kind {
        "ean13" => ean13::EAN13::new(d).map_err(err)?.encode(),
        "ean8" => ean8::EAN8::new(d).map_err(err)?.encode(),
        "code39" => code39::Code39::new(d.to_uppercase()).map_err(err)?.encode(),
        "code93" => code93::Code93::new(d.to_uppercase()).map_err(err)?.encode(),
        "itf" => tf::TF::interleaved(d).map_err(err)?.encode(),
        "codabar" => codabar::Codabar::new(d).map_err(err)?.encode(),
        _ => code128::Code128::new(format!("Ɓ{d}")).map_err(err)?.encode(),
    })
}

pub fn barcode_png(bars: &[u8], module: u32, height: u32, label: &str) -> DynamicImage {
    let quiet = 10 * module;
    let text_h = if label.is_empty() { 0 } else { 8 * 2 + 6 };
    let w = bars.len() as u32 * module + quiet * 2;
    let mut img = RgbaImage::from_pixel(w, height + quiet + text_h, Rgba([255, 255, 255, 255]));
    for (i, b) in bars.iter().enumerate() {
        if *b == 0 { continue; }
        for x in 0..module { for y in quiet / 2..quiet / 2 + height { img.put_pixel(quiet + i as u32 * module + x, y, Rgba([0, 0, 0, 255])); } }
    }
    if !label.is_empty() {
        let tw = label.chars().count() as u32 * 16;
        draw_bitmap_text(&mut img, label, (w.saturating_sub(tw) / 2) as i64, (quiet / 2 + height + 4) as i64, 2, [0, 0, 0], 1.0);
    }
    DynamicImage::ImageRgba8(img)
}

// ---------- 文字绘制 ----------

fn blend(img: &mut RgbaImage, x: i64, y: i64, rgb: [u8; 3], a: f32) {
    if x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 || a <= 0. { return; }
    let p = img.get_pixel_mut(x as u32, y as u32);
    for i in 0..3 { p.0[i] = (p.0[i] as f32 * (1. - a) + rgb[i] as f32 * a).round() as u8; }
    p.0[3] = p.0[3].max((a * 255.) as u8);
}

/// 内置 8×8 点阵（仅 ASCII），按 `scale` 放大。
fn draw_bitmap_text(img: &mut RgbaImage, s: &str, x0: i64, y0: i64, scale: u32, rgb: [u8; 3], a: f32) {
    for (i, ch) in s.chars().enumerate() {
        let g = font8x8::legacy::BASIC_LEGACY.get(ch as usize).copied().unwrap_or([0; 8]);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) == 0 { continue; }
                for dy in 0..scale { for dx in 0..scale {
                    blend(img, x0 + (i as u32 * 8 * scale + col * scale + dx) as i64, y0 + (row as u32 * scale + dy) as i64, rgb, a);
                } }
            }
        }
    }
}

/// 文字的绘制方式：用户提供的字体文件，或内置点阵。
pub enum Pen { Font(FontVec, f32), Bitmap(u32) }

impl Pen {
    pub fn measure(&self, s: &str) -> (u32, u32) {
        match self {
            Pen::Bitmap(k) => (s.chars().count() as u32 * 8 * k, 8 * k),
            Pen::Font(f, px) => {
                let sf = f.as_scaled(PxScale::from(*px));
                let w: f32 = s.chars().map(|c| sf.h_advance(f.glyph_id(c))).sum();
                (w.ceil() as u32, (sf.ascent() - sf.descent()).ceil() as u32)
            }
        }
    }
    pub fn draw(&self, img: &mut RgbaImage, s: &str, x: i64, y: i64, rgb: [u8; 3], a: f32) {
        match self {
            Pen::Bitmap(k) => draw_bitmap_text(img, s, x, y, *k, rgb, a),
            Pen::Font(f, px) => {
                let sf = f.as_scaled(PxScale::from(*px));
                let mut cx = x as f32;
                for c in s.chars() {
                    let id = f.glyph_id(c);
                    let g = id.with_scale_and_position(PxScale::from(*px), ab_glyph::point(cx, y as f32 + sf.ascent()));
                    if let Some(o) = f.outline_glyph(g) {
                        let b = o.px_bounds();
                        o.draw(|gx, gy, cov| blend(img, b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64, rgb, cov * a));
                    }
                    cx += sf.h_advance(id);
                }
            }
        }
    }
}

/// 内置中文字体（思源黑体常用字子集，SIL OFL 1.1）。
pub static CJK_FONT: &[u8] = include_bytes!("../../assets/NotoSansSC-common.ttf");

/// 用内置中文字体的画笔；字库里没有的字会报错而不是画成空框。
pub fn cjk_pen(text: &str, px: f32, zh: bool) -> Result<Pen> {
    let f = FontVec::try_from_vec(CJK_FONT.to_vec()).map_err(err)?;
    let miss: String = text.chars().filter(|c| !c.is_whitespace() && f.glyph_id(*c).0 == 0).take(10).collect();
    if !miss.is_empty() { return Err(if zh { format!("内置字体缺少这些字：{miss}；可选择其他字体文件") } else { format!("The built-in font lacks: {miss}; choose a font file") }); }
    Ok(Pen::Font(f, px))
}

pub fn parse_color(s: &str) -> [u8; 3] {
    let h = s.trim().trim_start_matches('#');
    if h.len() == 6 { if let Ok(v) = u32::from_str_radix(h, 16) { return [(v >> 16) as u8, (v >> 8) as u8, v as u8]; } }
    if h.len() == 3 { if let Ok(v) = u32::from_str_radix(h, 16) { let f = |n: u32| (n * 17) as u8; return [f(v >> 8 & 15), f(v >> 4 & 15), f(v & 15)]; } }
    [255, 255, 255]
}

pub fn watermark(img: &DynamicImage, text: &str, pen: &Pen, pos: &str, rgb: [u8; 3], opacity: f32) -> DynamicImage {
    let mut out = img.to_rgba8();
    let (w, h) = out.dimensions();
    let (tw, th) = pen.measure(text);
    let m = (w.min(h) / 40).max(8) as i64;
    let place = |x: i64, y: i64, o: &mut RgbaImage| pen.draw(o, text, x, y, rgb, opacity);
    match pos {
        "tile" => {
            let (sx, sy) = ((tw as i64 + m * 6).max(1), (th as i64 + m * 6).max(1));
            let mut row = 0;
            let mut y = m;
            while y < h as i64 {
                let mut x = if row % 2 == 0 { m } else { m + sx / 2 } - sx;
                while x < w as i64 { place(x, y, &mut out); x += sx; }
                y += sy;
                row += 1;
            }
        }
        "center" => place((w as i64 - tw as i64) / 2, (h as i64 - th as i64) / 2, &mut out),
        "tl" => place(m, m, &mut out),
        "tr" => place(w as i64 - tw as i64 - m, m, &mut out),
        "bl" => place(m, h as i64 - th as i64 - m, &mut out),
        _ => place(w as i64 - tw as i64 - m, h as i64 - th as i64 - m, &mut out),
    }
    DynamicImage::ImageRgba8(out)
}

// ---------- 压缩 ----------

pub fn compress(img: &DynamicImage, max_side: u32, fmt: &str, quality: u8) -> Result<(Vec<u8>, &'static str)> {
    let img = if max_side > 0 && img.width().max(img.height()) > max_side { img.resize(max_side, max_side, image::imageops::FilterType::Lanczos3) } else { img.clone() };
    let mut v = vec![];
    match fmt {
        "png" => {
            use image::codecs::png::{CompressionType, FilterType, PngEncoder};
            img.write_with_encoder(PngEncoder::new_with_quality(&mut v, CompressionType::Best, FilterType::Adaptive)).map_err(err)?;
            Ok((v, "png"))
        }
        _ => {
            let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
            rgb.write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut v, quality.clamp(10, 100))).map_err(err)?;
            Ok((v, "jpg"))
        }
    }
}

fn human(n: usize) -> String { if n >= 1 << 20 { format!("{:.2} MB", n as f64 / 1048576.) } else { format!("{:.1} KB", n as f64 / 1024.) } }

// ---------- 清单与入口 ----------

fn manifest(q: &Request) -> Value {
    let tr = |a, b| q.tr(a, b);
    let pos = [("br", tr("右下角", "Bottom right")), ("center", tr("居中", "Center")), ("tile", tr("平铺", "Tiled")), ("bl", tr("左下角", "Bottom left")), ("tr", tr("右上角", "Top right")), ("tl", tr("左上角", "Top left"))];
    sdk::manifest("image", tr("图片与条码", "Images & Barcodes"), env!("CARGO_PKG_VERSION"), tr("二维码、条码、图片压缩与水印", "QR codes, barcodes, image compression and watermarks"), vec![
        sdk::tool("qr_gen", "image", tr("生成二维码", "QR Code Generator"), tr("把文本或网址生成二维码图片", "Turn text or a URL into a QR code image"), "qr qrcode erweima generate",
            vec![sdk::multiline("input", tr("内容", "Content"))],
            vec![sdk::select("ec", tr("纠错等级", "Error correction"), &[("M", "M 15%"), ("L", "L 7%"), ("Q", "Q 25%"), ("H", "H 30%")]), sdk::number("scale", tr("每格像素", "Pixels per module"), "8"), sdk::number("margin", tr("边距（格）", "Margin (modules)"), "4")]),
        sdk::tool("qr_decode", "image", tr("识别二维码", "QR Code Reader"), tr("从图片中读取二维码内容", "Read the content of a QR code in an image"), "qr qrcode decode scan read",
            vec![sdk::file("file", tr("图片", "Image"))], vec![]),
        sdk::tool("barcode", "image", tr("生成条码", "Barcode Generator"), tr("生成 Code128、EAN-13 等一维条码", "Create Code128, EAN-13 and other barcodes"), "barcode code128 ean13 ean upc code39",
            vec![sdk::text("input", tr("内容", "Content"), "")],
            vec![sdk::select("kind", tr("码制", "Symbology"), &[("code128", "Code 128"), ("ean13", "EAN-13"), ("ean8", "EAN-8"), ("code39", "Code 39"), ("code93", "Code 93"), ("itf", "ITF"), ("codabar", "Codabar")]),
                 sdk::number("module", tr("线宽（像素）", "Bar width (px)"), "3"), sdk::number("height", tr("高度（像素）", "Height (px)"), "120"), sdk::toggle("label", tr("显示文字", "Show text"), true)]),
        sdk::tool("img_compress", "image", tr("图片压缩", "Image Compressor"), tr("缩小图片尺寸与体积，可转为 JPEG/PNG", "Shrink image size and file size, convert to JPEG/PNG"), "image compress resize jpeg png tinypng",
            vec![sdk::file("file", tr("图片", "Image"))],
            vec![sdk::select("format", tr("输出格式", "Output format"), &[("jpeg", "JPEG"), ("png", "PNG")]), sdk::number("quality", tr("JPEG 质量", "JPEG quality"), "80"), sdk::number("max", tr("最长边（像素，0 为不缩放）", "Longest side (px, 0 = keep)"), "1920")]),
        sdk::tool("img_watermark", "image", tr("图片水印", "Image Watermark"), tr("给图片加文字水印", "Add a text watermark to an image"), "image watermark text",
            vec![sdk::file("file", tr("图片", "Image")), sdk::text("text", tr("水印文字", "Watermark text"), ""), sdk::file("font", tr("字体文件（可选）", "Font file (optional)"))],
            vec![sdk::select("pos", tr("位置", "Position"), &pos), sdk::number("size", tr("字号（像素）", "Size (px)"), "32"), sdk::text("color", tr("颜色", "Color"), "#ffffff"), sdk::number("opacity", tr("不透明度 %", "Opacity %"), "50")]),
    ])
}

fn run(q: &Request) -> Result<Reply> {
    let zh = q.zh();
    match q.tool.as_str() {
        "qr_gen" => {
            let s = q.get("input");
            if s.is_empty() { return Err(q.tr("请输入内容", "Enter some content").into()); }
            let img = qr_png(s, q.get("ec"), q.int("scale", 8, 1, 64) as u32, q.int("margin", 4, 0, 32) as u32)?;
            Ok(Reply::File { name: "qrcode.png".into(), text: format!("{}×{} PNG", img.width(), img.height()), data: png(&img)? })
        }
        "qr_decode" => {
            let img = load(&q.file("file")?.data, zh)?;
            let r = qr_decode(&img);
            if r.is_empty() { return Err(q.tr("没有找到二维码", "No QR code found").into()); }
            Ok(Reply::Text(r.join("\n\n")))
        }
        "barcode" => {
            let s = q.get("input").trim();
            if s.is_empty() { return Err(q.tr("请输入内容", "Enter some content").into()); }
            let bars = barcode_bars(q.get("kind"), s).map_err(|e| format!("{}{e}", q.tr("内容不符合该码制：", "Invalid for this symbology: ")))?;
            let img = barcode_png(&bars, q.int("module", 3, 1, 20) as u32, q.int("height", 120, 20, 2000) as u32, if q.on("label") && s.is_ascii() { s } else { "" });
            Ok(Reply::File { name: format!("barcode-{}.png", q.get("kind")), text: format!("{}×{} PNG", img.width(), img.height()), data: png(&img)? })
        }
        "img_compress" => {
            let f = q.file("file")?;
            let img = load(&f.data, zh)?;
            let (data, ext) = compress(&img, q.int("max", 1920, 0, 20000) as u32, q.get("format"), q.int("quality", 80, 10, 100) as u8)?;
            let pct = data.len() as f64 * 100. / f.data.len().max(1) as f64;
            let text = format!("{} → {} ({pct:.0}%)", human(f.data.len()), human(data.len()));
            Ok(Reply::File { name: format!("{}-min.{ext}", sdk::stem(&f.name)), data, text })
        }
        "img_watermark" => {
            let f = q.file("file")?;
            let text = q.get("text");
            if text.trim().is_empty() { return Err(q.tr("请输入水印文字", "Enter the watermark text").into()); }
            let size = q.num("size", 32.).clamp(6., 1000.);
            let pen = match q.files("font").first() {
                Some(ff) => Pen::Font(FontVec::try_from_vec(ff.data.clone()).map_err(|_| q.tr("无法读取字体文件", "Can't read the font file").to_string())?, size as f32),
                None if text.is_ascii() => Pen::Bitmap(((size / 8.).round() as u32).max(1)),
                None => cjk_pen(text, size as f32, zh)?,
            };
            let img = load(&f.data, zh)?;
            let out = watermark(&img, text, &pen, q.get("pos"), parse_color(q.get("color")), (q.num("opacity", 50.) / 100.).clamp(0.02, 1.) as f32);
            let is_jpg = f.name.to_lowercase().ends_with(".jpg") || f.name.to_lowercase().ends_with(".jpeg");
            let data = if is_jpg { compress(&out, 0, "jpeg", 92)?.0 } else { png(&out)? };
            Ok(Reply::File { name: format!("{}-watermark.{}", sdk::stem(&f.name), if is_jpg { "jpg" } else { "png" }), data, text: format!("{}×{}", out.width(), out.height()) })
        }
        t => Err(format!("unknown tool {t}")),
    }
}

sdk::export!(manifest, run);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qr_roundtrip() {
        let img = qr_png("https://example.com/你好", "M", 6, 4).unwrap();
        assert_eq!(qr_decode(&img), vec!["https://example.com/你好".to_string()]);
        let p = png(&img).unwrap();
        assert_eq!(&p[1..4], b"PNG");
    }
    #[test]
    fn barcodes() {
        for (k, d) in [("code128", "Hello-123"), ("ean13", "750103131130"), ("ean8", "9638507"), ("code39", "ABC123"), ("itf", "1234567890"), ("codabar", "A1234B"), ("code93", "TEST93")] {
            let b = barcode_bars(k, d).unwrap_or_else(|e| panic!("{k}: {e}"));
            assert!(b.len() > 20, "{k}");
            let img = barcode_png(&b, 2, 50, d);
            assert!(img.width() > 40);
        }
        assert!(barcode_bars("ean13", "abc").is_err());
    }
    #[test]
    fn compress_and_mark() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(400, 300, |x, y| Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])));
        let (j, ext) = compress(&img, 200, "jpeg", 70).unwrap();
        assert_eq!(ext, "jpg");
        let back = image::load_from_memory(&j).unwrap();
        assert_eq!((back.width(), back.height()), (200, 150));
        let marked = watermark(&img, "Hi", &Pen::Bitmap(2), "br", [255, 255, 255], 1.0);
        assert_ne!(marked.to_rgba8(), img.to_rgba8());
        let tiled = watermark(&img, "A", &Pen::Bitmap(1), "tile", [0, 0, 0], 0.5);
        assert_ne!(tiled.to_rgba8(), img.to_rgba8());
        assert_eq!(parse_color("#f00"), [255, 0, 0]);
    }

    #[test]
    fn cjk_watermark() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(300, 120, Rgba([255, 255, 255, 255])));
        let pen = cjk_pen("内部资料", 40., true).unwrap();
        let (w, _) = pen.measure("内部资料");
        assert!(w > 80, "width {w}");
        let out = watermark(&img, "内部资料", &pen, "center", [255, 0, 0], 1.0).to_rgba8();
        let red = out.pixels().filter(|p| p[0] > 200 && p[1] < 100).count();
        assert!(red > 300, "red pixels {red}");
        assert!(cjk_pen("𠀀", 40., true).err().unwrap().contains("𠀀"));
    }
}
