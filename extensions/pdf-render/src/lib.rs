//! PDF 渲染：把 PDF 页面渲染为 PNG 图片（多页时打包为 zip）。
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, PixmapSettings, RenderCache, RenderSettings};
use shellax_ext_sdk::{self as sdk, serde_json::Value, Reply, Request, Result};

fn manifest(q: &Request) -> Value {
    let tr = |a, b| q.tr(a, b);
    sdk::manifest("pdf-render", tr("PDF 渲染", "PDF Rendering"), env!("CARGO_PKG_VERSION"), tr("把 PDF 页面转成图片", "Turn PDF pages into images"), vec![
        sdk::tool("pdf_to_img", "pdf", tr("PDF 转图片", "PDF to Images"), tr("把 PDF 页面渲染为 PNG 图片", "Render PDF pages as PNG images"), "pdf image png render convert",
            vec![sdk::file("file", "PDF"), sdk::with_placeholder(sdk::text("pages", tr("页码范围", "Pages"), ""), tr("例如 1-3,5（留空为全部）", "e.g. 1-3,5 (empty = all)"))],
            vec![sdk::number("dpi", "DPI", "144")]),
    ])
}

/// 渲染指定页（0 基），返回 (文件名, PNG)。
pub fn render_pages(data: Vec<u8>, pages: &str, dpi: f32, stem: &str, zh: bool) -> Result<Vec<(String, Vec<u8>)>> {
    let pdf = Pdf::new(data).map_err(|e| format!("{}: {e:?}", if zh { "无法读取 PDF" } else { "Cannot read PDF" }))?;
    let all = pdf.pages();
    let idx = sdk::parse_ranges(pages, all.len())?;
    let scale = dpi / 72.0;
    let cache = RenderCache::new();
    let (is, rs) = (InterpreterSettings::default(), RenderSettings::default());
    let ps = PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE };
    let mut out = vec![];
    for i in idx {
        let page = &all[i];
        let (w, h) = page.render_dimensions();
        if w * scale > 16000.0 || h * scale > 16000.0 { return Err((if zh { "页面过大，请降低 DPI" } else { "Page too large; lower the DPI" }).into()); }
        let png = render(page, &cache, &is, &rs, &ps).into_png().map_err(|e| format!("png: {e:?}"))?;
        out.push((format!("{stem}-{}.png", i + 1), png));
    }
    Ok(out)
}

fn run(q: &Request) -> Result<Reply> {
    match q.tool.as_str() {
        "pdf_to_img" => {
            let f = q.file("file")?;
            let dpi = q.int("dpi", 144, 36, 600) as f32;
            let stem = sdk::stem(&f.name).to_string();
            let mut parts = render_pages(f.data.clone(), q.get("pages"), dpi, &stem, q.zh())?;
            let text = format!("{} {}", parts.len(), q.tr("页", "pages"));
            if parts.len() == 1 { let (name, data) = parts.pop().unwrap(); return Ok(Reply::File { name, data, text }); }
            Ok(Reply::File { name: format!("{stem}-png.zip"), data: sdk::zip_store(&parts), text })
        }
        t => Err(format!("unknown tool: {t}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_cjk_watermark() {
        let base = minimal();
        let mut d = ext_pdf::load(&base, true).unwrap();
        ext_pdf::watermark(&mut d, &ext_pdf::Mark { text: "机密", size: 40., rgb: [1., 0., 0.], opacity: 1., pos: "center", angle: 0. }, true).unwrap();
        let pdf = ext_pdf::save(&mut d).unwrap();
        let r = super::render_pages(pdf, "", 72.0, "t", true).unwrap();
        let img = image_png_red_pixels(&r[0].1);
        assert!(img > 50, "watermark glyphs not drawn: {img}");
    }

    /// 统计 PNG 里偏红的像素数（不引入解码库：渲染器自带 png 编码，这里用 hayro 的依赖解码）。
    fn image_png_red_pixels(png: &[u8]) -> usize {
        let img = image::load_from_memory(png).unwrap().to_rgba8();
        img.pixels().filter(|p| p[0] > 200 && p[1] < 120 && p[2] < 120).count()
    }

    #[test]
    fn renders_generated_pdf() {
        let pdf = minimal();
        let r = super::render_pages(pdf, "", 72.0, "t", true).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(&r[0].1[1..4], b"PNG");
    }

    fn minimal() -> Vec<u8> {
        // 最小 PDF：一页，画一个矩形。
        let content = b"0 0 1 rg 10 10 50 50 re f";
        let mut pdf = Vec::new();
        let mut offs = vec![];
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), std::str::from_utf8(content).unwrap()),
        ];
        for (i, o) in objs.iter().enumerate() { offs.push(pdf.len()); pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes()); }
        let x = pdf.len();
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for o in offs { pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()); }
        pdf.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF").as_bytes());
        pdf
    }
}

sdk::export!(manifest, run);
