//! PDF 工具：合并、拆分、旋转、删页、重排、文字水印、页码、图片转 PDF、提取文本、清理元数据。
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use shellax_ext_sdk::{self as sdk, serde_json::Value, Reply, Request, Result};

pub mod cjk;

fn err<E: std::fmt::Display>(e: E) -> String { e.to_string() }

pub fn load(data: &[u8], zh: bool) -> Result<Document> {
    let d = Document::load_mem(data).map_err(|e| if zh { format!("无法读取 PDF：{e}") } else { format!("Can't read PDF: {e}") })?;
    if d.is_encrypted() { return Err((if zh { "不支持加密的 PDF" } else { "Encrypted PDFs aren't supported" }).into()); }
    Ok(d)
}

pub fn save(doc: &mut Document) -> Result<Vec<u8>> {
    doc.prune_objects();
    doc.renumber_objects();
    doc.compress();
    let mut v = vec![];
    doc.save_to(&mut v).map_err(err)?;
    Ok(v)
}

fn pages_root(doc: &Document) -> Result<ObjectId> {
    let cat = doc.catalog().map_err(err)?;
    cat.get(b"Pages").and_then(Object::as_reference).map_err(err)
}

const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// 把从上级 Pages 节点继承的属性复制到每一页上，之后才能安全地改动页面树。
pub fn materialize(doc: &mut Document) {
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    for pid in pages {
        let mut found: Vec<(&[u8], Object)> = vec![];
        let mut cur = doc.get_dictionary(pid).ok().and_then(|d| d.get(b"Parent").ok()).and_then(|p| p.as_reference().ok());
        let own: Vec<bool> = INHERITABLE.iter().map(|k| doc.get_dictionary(pid).map(|d| d.has(k)).unwrap_or(false)).collect();
        let mut guard = 0;
        while let Some(id) = cur {
            guard += 1;
            if guard > 64 { break; }
            let Ok(d) = doc.get_dictionary(id) else { break };
            for (i, k) in INHERITABLE.iter().enumerate() {
                if !own[i] && !found.iter().any(|f| f.0 == *k) { if let Ok(v) = d.get(k) { found.push((k, v.clone())); } }
            }
            cur = d.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
        }
        if let Ok(d) = doc.get_dictionary_mut(pid) { for (k, v) in found { d.set(k, v); } }
    }
}

/// 让根 Pages 节点直接按 `order` 列出这些页（其余页随后被清理）。
pub fn set_pages(doc: &mut Document, order: &[ObjectId]) -> Result<()> {
    let root = pages_root(doc)?;
    for id in order { if let Ok(d) = doc.get_dictionary_mut(*id) { d.set("Parent", Object::Reference(root)); } }
    let d = doc.get_dictionary_mut(root).map_err(err)?;
    d.set("Kids", Object::Array(order.iter().map(|i| Object::Reference(*i)).collect()));
    d.set("Count", Object::Integer(order.len() as i64));
    for k in INHERITABLE { d.remove(k); }
    Ok(())
}

pub fn page_ids(doc: &Document) -> Vec<ObjectId> { doc.get_pages().into_values().collect() }

pub fn merge(files: &[&[u8]], zh: bool) -> Result<Document> {
    let mut docs = vec![];
    for f in files { let mut d = load(f, zh)?; materialize(&mut d); docs.push(d); }
    let mut it = docs.into_iter();
    let mut main = it.next().ok_or("no files")?;
    let mut order = page_ids(&main);
    for mut d in it {
        d.renumber_objects_with(main.max_id + 1);
        main.max_id = d.max_id;
        order.extend(page_ids(&d));
        main.objects.extend(d.objects);
    }
    set_pages(&mut main, &order)?;
    Ok(main)
}

/// 只保留 `idx`（从 0 开始，按给定顺序）这些页。
pub fn select(data: &[u8], idx: &[usize], zh: bool) -> Result<Document> {
    let mut d = load(data, zh)?;
    materialize(&mut d);
    let ids = page_ids(&d);
    let order: Vec<ObjectId> = idx.iter().filter_map(|i| ids.get(*i).copied()).collect();
    set_pages(&mut d, &order)?;
    Ok(d)
}

pub fn rotate(doc: &mut Document, idx: &[usize], angle: i64) {
    materialize(doc);
    let ids = page_ids(doc);
    for i in idx {
        if let Some(d) = ids.get(*i).and_then(|id| doc.get_dictionary_mut(*id).ok()) {
            let cur = d.get(b"Rotate").and_then(Object::as_i64).unwrap_or(0);
            d.set("Rotate", Object::Integer((cur + angle).rem_euclid(360)));
        }
    }
}

fn media_box(doc: &Document, id: ObjectId) -> [f32; 4] {
    let d = doc.get_dictionary(id).ok();
    let arr = d.and_then(|d| d.get(b"CropBox").or_else(|_| d.get(b"MediaBox")).ok()).and_then(|o| match o { Object::Reference(r) => doc.get_object(*r).ok(), o => Some(o) }).and_then(|o| o.as_array().ok());
    let n = |o: &Object| o.as_float().or_else(|_| o.as_i64().map(|i| i as f32)).unwrap_or(0.);
    match arr { Some(a) if a.len() == 4 => [n(&a[0]), n(&a[1]), n(&a[2]), n(&a[3])], _ => [0., 0., 595., 842.] }
}

/// 给页面资源加上字体与透明度设置，返回 (字体名, 透明度名)。
/// 资源名带上对象号，多次叠加（水印后再加页码）互不覆盖。
fn add_resources(doc: &mut Document, page: ObjectId, font: ObjectId, gs: ObjectId) -> Result<(String, String)> {
    let res = doc.get_dictionary(page).map_err(err)?.get(b"Resources").ok().cloned();
    let res_id = match res {
        Some(Object::Reference(r)) => r,
        Some(Object::Dictionary(d)) => { let id = doc.add_object(d); doc.get_dictionary_mut(page).map_err(err)?.set("Resources", Object::Reference(id)); id }
        _ => { let id = doc.add_object(Dictionary::new()); doc.get_dictionary_mut(page).map_err(err)?.set("Resources", Object::Reference(id)); id }
    };
    let (fname, gname) = (format!("SxF{}", font.0), format!("SxG{}", gs.0));
    for (cat, name, id) in [(&b"Font"[..], fname.as_bytes(), font), (b"ExtGState", gname.as_bytes(), gs)] {
        let sub = doc.get_dictionary(res_id).map_err(err)?.get(cat).ok().cloned();
        match sub {
            Some(Object::Reference(r)) => { doc.get_dictionary_mut(r).map_err(err)?.set(name, Object::Reference(id)); }
            Some(Object::Dictionary(mut d)) => { d.set(name, Object::Reference(id)); doc.get_dictionary_mut(res_id).map_err(err)?.set(cat, Object::Dictionary(d)); }
            _ => { doc.get_dictionary_mut(res_id).map_err(err)?.set(cat, dictionary! { name => Object::Reference(id) }); }
        }
    }
    Ok((fname, gname))
}

/// 在页面最上层叠加内容（原内容包进 q/Q，避免其图形状态影响叠加层）。
fn overlay(doc: &mut Document, page: ObjectId, ops: String) -> Result<()> {
    let q = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
    let mine = doc.add_object(Stream::new(Dictionary::new(), format!("Q\nq\n{ops}\nQ\n").into_bytes()));
    let d = doc.get_dictionary(page).map_err(err)?;
    let mut contents: Vec<Object> = match d.get(b"Contents") {
        Ok(Object::Reference(r)) => match doc.get_object(*r) { Ok(Object::Array(a)) => a.clone(), _ => vec![Object::Reference(*r)] },
        Ok(Object::Array(a)) => a.clone(),
        _ => vec![],
    };
    contents.insert(0, Object::Reference(q));
    contents.push(Object::Reference(mine));
    doc.get_dictionary_mut(page).map_err(err)?.set("Contents", Object::Array(contents));
    Ok(())
}

/// PDF 字符串字面量（仅 Latin-1；内置字体没有其他字形）。
fn pdf_str(s: &str) -> Option<String> {
    let mut o = String::from("(");
    for c in s.chars() {
        let b = c as u32;
        if b > 255 { return None; }
        match c { '(' | ')' | '\\' => { o.push('\\'); o.push(c); } _ if b < 32 || b > 126 => o.push_str(&format!("\\{b:03o}")), c => o.push(c) }
    }
    o.push(')');
    Some(o)
}

fn text_width(s: &str, size: f32) -> f32 { s.chars().map(|c| if c.is_ascii_uppercase() || c == 'm' || c == 'w' { 0.68 } else if " il.,:;!'|".contains(c) { 0.28 } else { 0.55 }).sum::<f32>() * size }

fn std_font(doc: &mut Document, bold: bool) -> ObjectId {
    doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => if bold { "Helvetica-Bold" } else { "Helvetica" }, "Encoding" => "WinAnsiEncoding" })
}

/// 文字画笔：西文用标准字体，含中日韩等字符时用内置 CID 字体。
enum TextPen { Std, Cjk(cjk::Shaper) }

impl TextPen {
    fn new(doc: &mut Document, texts: &[&str], bold: bool, zh: bool) -> Result<(TextPen, ObjectId)> {
        if !texts.iter().any(|t| cjk::needs_cjk(t)) { return Ok((TextPen::Std, std_font(doc, bold))); }
        let sh = cjk::Shaper::new(texts)?;
        if !sh.missing().is_empty() {
            let m: String = sh.missing().iter().take(10).collect();
            return Err(if zh { format!("内置字体缺少这些字：{m}") } else { format!("The built-in font lacks these characters: {m}") });
        }
        let id = sh.embed(doc)?;
        Ok((TextPen::Cjk(sh), id))
    }
    fn enc(&self, s: &str, size: f32) -> (String, f32) {
        match self { TextPen::Std => (pdf_str(s).unwrap_or_default(), text_width(s, size)), TextPen::Cjk(sh) => sh.encode(s, size) }
    }
}

pub struct Mark<'a> { pub text: &'a str, pub size: f32, pub rgb: [f32; 3], pub opacity: f32, pub pos: &'a str, pub angle: f32 }

pub fn watermark(doc: &mut Document, m: &Mark, zh: bool) -> Result<()> {
    materialize(doc);
    let (pen, font) = TextPen::new(doc, &[m.text], true, zh)?;
    let gs = doc.add_object(dictionary! { "Type" => "ExtGState", "ca" => Object::Real(m.opacity), "CA" => Object::Real(m.opacity) });
    let (lit, w) = pen.enc(m.text, m.size);
    for page in page_ids(doc) {
        let (fname, gname) = add_resources(doc, page, font, gs)?;
        let b = media_box(doc, page);
        let (pw, ph) = (b[2] - b[0], b[3] - b[1]);
        let [r, g, bl] = m.rgb;
        let mut ops = format!("/{gname} gs {r} {g} {bl} rg BT /{fname} {} Tf\n", m.size);
        let place = |x: f32, y: f32, a: f32| {
            let (s, c) = a.to_radians().sin_cos();
            // 以文字中心为旋转中心
            let (dx, dy) = (-w / 2. * c + m.size * 0.35 * s, -w / 2. * s - m.size * 0.35 * c);
            format!("{c:.4} {s:.4} {:.4} {c:.4} {:.2} {:.2} Tm {lit} Tj\n", -s, x + dx, y + dy)
        };
        match m.pos {
            "tile" => {
                let (sx, sy) = (w + m.size * 4., m.size * 6.);
                let mut y = b[1] + sy / 2.;
                let mut row = 0;
                while y < b[3] + sy {
                    let mut x = b[0] + if row % 2 == 0 { 0. } else { sx / 2. };
                    while x < b[2] + sx { ops += &place(x, y, m.angle); x += sx; }
                    y += sy;
                    row += 1;
                }
            }
            "br" => ops += &place(b[2] - w / 2. - 24., b[1] + m.size + 12., 0.),
            "top" => ops += &place(b[0] + pw / 2., b[3] - m.size - 12., 0.),
            _ => ops += &place(b[0] + pw / 2., b[1] + ph / 2., m.angle),
        }
        ops += "ET";
        overlay(doc, page, ops)?;
    }
    Ok(())
}

pub fn page_numbers(doc: &mut Document, fmt: &str, pos: &str, size: f32, start: i64, zh: bool) -> Result<()> {
    materialize(doc);
    let gs = doc.add_object(dictionary! { "Type" => "ExtGState", "ca" => Object::Real(1.0), "CA" => Object::Real(1.0) });
    let ids = page_ids(doc);
    let total = ids.len() as i64 + start - 1;
    let labels: Vec<String> = (0..ids.len() as i64).map(|i| {
        let n = i + start;
        match fmt { "n_of" => format!("{n} / {total}"), "dash" => format!("- {n} -"), "page_of" => format!("Page {n} of {total}"), "zh" => format!("第 {n} 页，共 {total} 页"), _ => n.to_string() }
    }).collect();
    let (pen, font) = TextPen::new(doc, &labels.iter().map(|s| s.as_str()).collect::<Vec<_>>(), false, zh)?;
    for (page, s) in ids.into_iter().zip(&labels) {
        let (fname, gname) = add_resources(doc, page, font, gs)?;
        let b = media_box(doc, page);
        let (lit, w) = pen.enc(s, size);
        let x = match pos { "right" => b[2] - 36. - w, "left" => b[0] + 36., _ => b[0] + (b[2] - b[0] - w) / 2. };
        overlay(doc, page, format!("/{gname} gs 0 0 0 rg BT /{fname} {size} Tf {x:.2} {:.2} Td {lit} Tj ET", b[1] + 24.))?;
    }
    Ok(())
}

pub fn images_to_pdf(imgs: &[&[u8]], page: &str, margin: f32, zh: bool) -> Result<Document> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = vec![];
    for data in imgs {
        let fmt = image::guess_format(data).map_err(|e| if zh { format!("无法识别的图片：{e}") } else { format!("Unknown image: {e}") })?;
        let img = image::load_from_memory(data).map_err(err)?;
        let (w, h) = (img.width(), img.height());
        let stream = if fmt == image::ImageFormat::Jpeg && !matches!(img.color(), image::ColorType::La8 | image::ColorType::Rgba8) {
            let cs = if img.color().channel_count() == 1 { "DeviceGray" } else { "DeviceRGB" };
            Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64, "ColorSpace" => cs, "BitsPerComponent" => 8, "Filter" => "DCTDecode" }, data.to_vec())
        } else {
            // 透明部分铺白底
            let rgba = img.to_rgba8();
            let mut rgb = Vec::with_capacity((w * h * 3) as usize);
            for p in rgba.pixels() { let a = p[3] as u32; for c in 0..3 { rgb.push(((p[c] as u32 * a + 255 * (255 - a)) / 255) as u8); } }
            let mut s = Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 }, rgb);
            let _ = s.compress();
            s
        };
        let img_id = doc.add_object(stream);
        let (pw, ph) = match page { "a4" => if w > h { (842., 595.) } else { (595., 842.) }, "letter" => if w > h { (792., 612.) } else { (612., 792.) }, _ => (w as f32 * 0.75 + margin * 2., h as f32 * 0.75 + margin * 2.) };
        let (aw, ah) = (pw - margin * 2., ph - margin * 2.);
        let k = (aw / w as f32).min(ah / h as f32);
        let (dw, dh) = (w as f32 * k, h as f32 * k);
        let (x, y) = ((pw - dw) / 2., (ph - dh) / 2.);
        let content = doc.add_object(Stream::new(Dictionary::new(), format!("q {dw:.2} 0 0 {dh:.2} {x:.2} {y:.2} cm /Im0 Do Q").into_bytes()));
        let p = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), Object::Real(pw), Object::Real(ph)], "Contents" => content,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => img_id } },
        });
        kids.push(Object::Reference(p));
    }
    let n = kids.len() as i64;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => n }));
    let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", cat);
    Ok(doc)
}

pub fn text(doc: &Document) -> String {
    let pages: Vec<u32> = doc.get_pages().into_keys().collect();
    pages.iter().map(|p| doc.extract_text(&[*p]).unwrap_or_default().trim_end().to_string()).collect::<Vec<_>>().join("\n\n\u{c}\n\n")
}

/// 去掉文档信息字典与 XMP 元数据，返回被移除的信息条目。
pub fn clean(doc: &mut Document) -> Vec<String> {
    let mut removed = vec![];
    if let Ok(info) = doc.trailer.get(b"Info").cloned() {
        let d = match &info { Object::Reference(r) => doc.get_dictionary(*r).ok().cloned(), Object::Dictionary(d) => Some(d.clone()), _ => None };
        if let Some(d) = d {
            for (k, v) in d.iter() {
                let val = match v { Object::String(s, _) => decode_pdf_text(s), o => format!("{o:?}") };
                removed.push(format!("{}: {val}", String::from_utf8_lossy(k)));
            }
        }
        doc.trailer.remove(b"Info");
    }
    if let Ok(cat) = doc.catalog_mut() {
        if cat.remove(b"Metadata").is_some() { removed.push("XMP Metadata".into()); }
        cat.remove(b"PieceInfo");
    }
    for (_, o) in doc.objects.iter_mut() {
        if let Object::Dictionary(d) = o { if d.get(b"Type").and_then(Object::as_name).map(|n| n == b"Page").unwrap_or(false) { d.remove(b"PieceInfo"); d.remove(b"Metadata"); } }
    }
    removed
}

fn decode_pdf_text(b: &[u8]) -> String {
    if b.starts_with(&[0xfe, 0xff]) {
        let u: Vec<u16> = b[2..].chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
        return String::from_utf16_lossy(&u);
    }
    b.iter().map(|&c| c as char).collect()
}

// ---------- 清单与入口 ----------

fn manifest(q: &Request) -> Value {
    let tr = |a, b| q.tr(a, b);
    let pdf = || sdk::file("file", "PDF");
    let pages = |d: &str| sdk::with_placeholder(sdk::text("pages", tr("页码范围", "Pages"), d), tr("例如 1-3,5,8-（留空为全部）", "e.g. 1-3,5,8- (empty = all)"));
    sdk::manifest("pdf", tr("PDF 工具", "PDF Tools"), env!("CARGO_PKG_VERSION"), tr("合并、拆分、旋转、水印、页码等", "Merge, split, rotate, watermark, page numbers and more"), vec![
        sdk::tool("pdf_merge", "pdf", tr("PDF 合并", "Merge PDFs"), tr("把多个 PDF 按顺序合并为一个", "Combine several PDFs into one, in order"), "pdf merge combine join",
            vec![sdk::files("files", tr("PDF 文件（按顺序）", "PDF files (in order)"))], vec![]),
        sdk::tool("pdf_split", "pdf", tr("PDF 拆分", "Split PDF"), tr("按页码范围拆出新的 PDF", "Extract page ranges into new PDFs"), "pdf split extract",
            vec![pdf(), sdk::with_placeholder(sdk::text("ranges", tr("拆分范围", "Ranges"), ""), tr("例如 1-3,4-6；留空则每页一个文件", "e.g. 1-3,4-6; empty = one file per page"))], vec![]),
        sdk::tool("pdf_rotate", "pdf", tr("PDF 旋转", "Rotate PDF Pages"), tr("旋转全部或指定页面", "Rotate all or selected pages"), "pdf rotate",
            vec![pdf(), pages("")], vec![sdk::select("angle", tr("角度", "Angle"), &[("90", tr("顺时针 90°", "90° clockwise")), ("180", "180°"), ("270", tr("逆时针 90°", "90° counter-clockwise"))])]),
        sdk::tool("pdf_delete", "pdf", tr("PDF 删页", "Delete PDF Pages"), tr("删除指定页面", "Remove selected pages"), "pdf delete remove pages",
            vec![pdf(), sdk::with_placeholder(sdk::text("pages", tr("要删除的页", "Pages to delete"), ""), tr("例如 2,5-7", "e.g. 2,5-7"))], vec![]),
        sdk::tool("pdf_reorder", "pdf", tr("PDF 重排", "Reorder PDF Pages"), tr("按指定顺序重新排列页面", "Rearrange pages in a given order"), "pdf reorder arrange sort pages",
            vec![pdf(), sdk::with_placeholder(sdk::text("order", tr("新顺序", "New order"), ""), tr("例如 3,1,2,4-；“reverse” 为倒序", "e.g. 3,1,2,4-; “reverse” to reverse"))], vec![]),
        sdk::tool("pdf_watermark", "pdf", tr("PDF 水印", "PDF Watermark"), tr("在每一页加文字水印", "Add a text watermark to every page"), "pdf watermark stamp",
            vec![pdf(), sdk::text("text", tr("水印文字", "Watermark text"), tr("机密", "CONFIDENTIAL"))],
            vec![sdk::select("pos", tr("位置", "Position"), &[("center", tr("居中斜放", "Diagonal center")), ("tile", tr("平铺", "Tiled")), ("top", tr("顶部", "Top")), ("br", tr("右下角", "Bottom right"))]),
                 sdk::number("size", tr("字号", "Size"), "48"), sdk::text("color", tr("颜色", "Color"), "#ff0000"), sdk::number("opacity", tr("不透明度 %", "Opacity %"), "20")]),
        sdk::tool("pdf_pagenum", "pdf", tr("PDF 页码", "PDF Page Numbers"), tr("在页面底部加页码", "Add page numbers at the bottom of pages"), "pdf page numbers",
            vec![pdf()],
            vec![sdk::select("fmt", tr("格式", "Format"), &[("n", "1"), ("n_of", "1 / N"), ("dash", "- 1 -"), ("page_of", "Page 1 of N"), ("zh", "第 1 页，共 N 页")]),
                 sdk::select("pos", tr("位置", "Position"), &[("center", tr("居中", "Center")), ("right", tr("右侧", "Right")), ("left", tr("左侧", "Left"))]),
                 sdk::number("size", tr("字号", "Size"), "10"), sdk::number("start", tr("起始页码", "Start at"), "1")]),
        sdk::tool("img_to_pdf", "pdf", tr("图片转 PDF", "Images to PDF"), tr("把多张图片合成一个 PDF", "Combine images into one PDF"), "image jpg png to pdf",
            vec![sdk::files("files", tr("图片（按顺序）", "Images (in order)"))],
            vec![sdk::select("page", tr("页面大小", "Page size"), &[("fit", tr("与图片一致", "Same as image")), ("a4", "A4"), ("letter", "Letter")]), sdk::number("margin", tr("边距（pt）", "Margin (pt)"), "0")]),
        sdk::tool("pdf_text", "pdf", tr("PDF 转文本", "PDF to Text"), tr("提取 PDF 中的文字", "Extract the text of a PDF"), "pdf text extract",
            vec![pdf()], vec![]),
        sdk::tool("pdf_clean", "pdf", tr("PDF 元数据清理", "Clean PDF Metadata"), tr("移除作者、软件等文档信息", "Remove author, producer and other document info"), "pdf metadata clean privacy",
            vec![pdf()], vec![]),
    ])
}

fn color(s: &str) -> [f32; 3] {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).unwrap_or(0xff0000);
    let v = if h.len() == 3 { let f = |n: u32| n * 17; f(v >> 8 & 15) << 16 | f(v >> 4 & 15) << 8 | f(v & 15) } else { v };
    [((v >> 16) & 255) as f32 / 255., ((v >> 8) & 255) as f32 / 255., (v & 255) as f32 / 255.]
}

fn run(q: &Request) -> Result<Reply> {
    let zh = q.zh();
    let out = |name: String, mut d: Document, text: String| -> Result<Reply> { Ok(Reply::File { name, data: save(&mut d)?, text }) };
    let n_pages = |d: &Document| d.get_pages().len();
    match q.tool.as_str() {
        "pdf_merge" => {
            let fs = q.files("files");
            if fs.len() < 2 { return Err(q.tr("请至少选择两个 PDF", "Choose at least two PDFs").into()); }
            let d = merge(&fs.iter().map(|f| f.data.as_slice()).collect::<Vec<_>>(), zh)?;
            let n = n_pages(&d);
            out("merged.pdf".into(), d, format!("{} → {n} {}", fs.len(), q.tr("页", "pages")))
        }
        "pdf_split" => {
            let f = q.file("file")?;
            let total = n_pages(&load(&f.data, zh)?);
            let ranges: Vec<String> = if q.get("ranges").trim().is_empty() { (1..=total).map(|p| p.to_string()).collect() } else { q.get("ranges").split([',', '，', ';', '；']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect() };
            let mut parts = vec![];
            for r in &ranges {
                let idx = sdk::parse_ranges(r, total)?;
                let mut d = select(&f.data, &idx, zh)?;
                parts.push((format!("{}-{}.pdf", sdk::stem(&f.name), r.replace(' ', "")), save(&mut d)?));
            }
            let text = format!("{} {}", parts.len(), q.tr("个文件", "files"));
            if parts.len() == 1 { let (name, data) = parts.pop().unwrap(); return Ok(Reply::File { name, data, text }); }
            Ok(Reply::File { name: format!("{}-split.zip", sdk::stem(&f.name)), data: sdk::zip_store(&parts), text })
        }
        "pdf_rotate" => {
            let f = q.file("file")?;
            let mut d = load(&f.data, zh)?;
            let idx = sdk::parse_ranges(q.get("pages"), n_pages(&d))?;
            rotate(&mut d, &idx, q.int("angle", 90, 0, 270));
            out(format!("{}-rotated.pdf", sdk::stem(&f.name)), d, format!("{} {}", idx.len(), q.tr("页已旋转", "pages rotated")))
        }
        "pdf_delete" => {
            let f = q.file("file")?;
            let total = n_pages(&load(&f.data, zh)?);
            if q.get("pages").trim().is_empty() { return Err(q.tr("请填写要删除的页", "Enter the pages to delete").into()); }
            let del = sdk::parse_ranges(q.get("pages"), total)?;
            let keep: Vec<usize> = (0..total).filter(|i| !del.contains(i)).collect();
            if keep.is_empty() { return Err(q.tr("不能删除全部页面", "Can't delete every page").into()); }
            let n = keep.len();
            out(format!("{}-edited.pdf", sdk::stem(&f.name)), select(&f.data, &keep, zh)?, format!("{total} → {n} {}", q.tr("页", "pages")))
        }
        "pdf_reorder" => {
            let f = q.file("file")?;
            let total = n_pages(&load(&f.data, zh)?);
            let o = q.get("order").trim();
            let mut idx = if o.eq_ignore_ascii_case("reverse") || o == "倒序" { (0..total).rev().collect() } else { sdk::parse_ranges(o, total)? };
            for i in 0..total { if !idx.contains(&i) { idx.push(i); } }
            out(format!("{}-reordered.pdf", sdk::stem(&f.name)), select(&f.data, &idx, zh)?, format!("{total} {}", q.tr("页", "pages")))
        }
        "pdf_watermark" => {
            let f = q.file("file")?;
            let mut d = load(&f.data, zh)?;
            let t = q.get("text");
            if t.trim().is_empty() { return Err(q.tr("请输入水印文字", "Enter the watermark text").into()); }
            watermark(&mut d, &Mark { text: t, size: q.num("size", 48.).clamp(4., 400.) as f32, rgb: color(q.get("color")), opacity: (q.num("opacity", 20.) / 100.).clamp(0.02, 1.) as f32, pos: q.get("pos"), angle: 45. }, zh)?;
            out(format!("{}-watermark.pdf", sdk::stem(&f.name)), d, String::new())
        }
        "pdf_pagenum" => {
            let f = q.file("file")?;
            let mut d = load(&f.data, zh)?;
            page_numbers(&mut d, q.get("fmt"), q.get("pos"), q.num("size", 10.).clamp(4., 72.) as f32, q.int("start", 1, -100000, 100000), zh)?;
            out(format!("{}-numbered.pdf", sdk::stem(&f.name)), d, String::new())
        }
        "img_to_pdf" => {
            let fs = q.files("files");
            if fs.is_empty() { return Err(q.tr("请先选择图片", "Choose images first").into()); }
            let d = images_to_pdf(&fs.iter().map(|f| f.data.as_slice()).collect::<Vec<_>>(), q.get("page"), q.num("margin", 0.).clamp(0., 200.) as f32, zh)?;
            out(format!("{}.pdf", sdk::stem(&fs[0].name)), d, format!("{} {}", fs.len(), q.tr("页", "pages")))
        }
        "pdf_text" => {
            let d = load(&q.file("file")?.data, zh)?;
            let t = text(&d);
            if t.trim().is_empty() { return Err(q.tr("没有提取到文字（可能是扫描件）", "No text found (it may be a scanned document)").into()); }
            Ok(Reply::Text(t))
        }
        "pdf_clean" => {
            let f = q.file("file")?;
            let mut d = load(&f.data, zh)?;
            let removed = clean(&mut d);
            let text = if removed.is_empty() { q.tr("没有发现元数据", "No metadata found").to_string() } else { format!("{}\n{}", q.tr("已移除：", "Removed:"), removed.join("\n")) };
            out(format!("{}-clean.pdf", sdk::stem(&f.name)), d, text)
        }
        t => Err(format!("unknown tool {t}")),
    }
}

sdk::export!(manifest, run);

#[cfg(test)]
mod tests;
