use super::*;

/// 生成一个 n 页、每页写着页码的 PDF，页面属性放在上级 Pages 节点（检验继承处理）。
fn sample(n: usize, title: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier" });
    let res = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let mut kids = vec![];
    for i in 1..=n {
        let c = doc.add_object(Stream::new(Dictionary::new(), format!("BT /F1 24 Tf 72 720 Td ({title} page {i}) Tj ET").into_bytes()));
        kids.push(Object::Reference(doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => c })));
    }
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => n as i64, "Resources" => res, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()] }));
    let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", cat);
    let info = doc.add_object(dictionary! { "Author" => Object::string_literal("Alice"), "Producer" => Object::string_literal("Test") });
    doc.trailer.set("Info", info);
    let mut v = vec![];
    doc.save_to(&mut v).unwrap();
    v
}

fn texts(data: &[u8]) -> Vec<String> {
    let d = load(data, false).unwrap();
    d.get_pages().keys().map(|p| d.extract_text(&[*p]).unwrap().trim().to_string()).collect()
}

fn roundtrip(mut d: Document) -> Vec<u8> { save(&mut d).unwrap() }

#[test]
fn merge_select_reorder() {
    let a = sample(2, "A");
    let b = sample(3, "B");
    let m = roundtrip(merge(&[&a, &b], false).unwrap());
    assert_eq!(texts(&m), ["A page 1", "A page 2", "B page 1", "B page 2", "B page 3"]);
    let r = roundtrip(select(&m, &[4, 0, 2], false).unwrap());
    assert_eq!(texts(&r), ["B page 3", "A page 1", "B page 1"]);
}

#[test]
fn rotate_mark_number_clean() {
    let a = sample(3, "A");
    let mut d = load(&a, false).unwrap();
    rotate(&mut d, &[1], 90);
    rotate(&mut d, &[1], 270);
    rotate(&mut d, &[2], 90);
    let r = roundtrip(d);
    let d = load(&r, false).unwrap();
    let rot: Vec<i64> = d.get_pages().values().map(|id| d.get_dictionary(*id).unwrap().get(b"Rotate").and_then(Object::as_i64).unwrap_or(0)).collect();
    assert_eq!(rot, [0, 0, 90]);

    let mut d = load(&a, false).unwrap();
    watermark(&mut d, &Mark { text: "DRAFT (x)", size: 40., rgb: [1., 0., 0.], opacity: 0.3, pos: "tile", angle: 45. }, false).unwrap();
    page_numbers(&mut d, "n_of", "center", 10., 1, false).unwrap();
    let w = roundtrip(d);
    let t = texts(&w);
    assert!(t[0].contains("A page 1") && t[0].contains("DRAFT") && t[0].contains("1 / 3"), "{t:?}");
    assert!(t[2].contains("3 / 3"));
    let mut d = load(&a, false).unwrap();
    assert!(watermark(&mut d, &Mark { text: "机密", size: 40., rgb: [0.; 3], opacity: 0.3, pos: "center", angle: 45. }, true).is_ok());

    let mut d = load(&a, false).unwrap();
    let removed = clean(&mut d);
    assert!(removed.iter().any(|s| s == "Author: Alice"), "{removed:?}");
    let c = roundtrip(d);
    assert!(load(&c, false).unwrap().trailer.get(b"Info").is_err());
}

#[test]
fn images() {
    let mut png = vec![];
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(40, 20, image::Rgba([10, 20, 30, 128]))).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut jpg = vec![];
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(30, 60, image::Rgb([200, 0, 0]))).write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg).unwrap();
    let p = roundtrip(images_to_pdf(&[&png, &jpg], "a4", 20., false).unwrap());
    let d = load(&p, false).unwrap();
    assert_eq!(d.get_pages().len(), 2);
    assert!(images_to_pdf(&[b"nope"], "fit", 0., false).is_err());
}

#[test]
fn via_abi() {
    let a = sample(4, "A");
    let json = format!(r#"{{"op":"run","lang":"en","tool":"pdf_split","args":{{"ranges":"1-2, 3-4"}},"files":[{{"field":"file","name":"x.pdf","len":{}}}]}}"#, a.len());
    let out = sdk::handle(&sdk::frame(json.as_bytes(), &a), manifest, run);
    let n = u32::from_le_bytes(out[..4].try_into().unwrap()) as usize;
    let j: Value = sdk::serde_json::from_slice(&out[4..4 + n]).unwrap();
    assert_eq!(j["file_name"], "x-split.zip", "{j}");
    assert_eq!(&out[4 + n..4 + n + 4], b"PK\x03\x04");
}

#[test]
fn cjk_watermark_and_page_numbers() {
    let mut d = load(&sample(2, "Doc"), true).unwrap();
    watermark(&mut d, &Mark { text: "内部资料 机密", size: 40., rgb: [1., 0., 0.], opacity: 0.3, pos: "center", angle: 45. }, true).unwrap();
    page_numbers(&mut d, "zh", "center", 10., 1, true).unwrap();
    let out = roundtrip(d);
    // 子集化后整个文件应远小于内置字体本身。
    assert!(out.len() < 200_000, "pdf too large: {}", out.len());
    let d = load(&out, true).unwrap();
    let fonts: Vec<_> = d.objects.values().filter_map(|o| o.as_dict().ok()).filter(|o| o.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) == Some(b"CIDFontType2")).collect();
    assert!(!fonts.is_empty());
    let t = texts(&out).join("\n");
    assert!(t.contains("机密") && t.contains("第 1 页，共 2 页"), "{t}");
}

#[test]
fn cjk_missing_glyph_reported() {
    let mut d = load(&sample(1, "Doc"), true).unwrap();
    let e = watermark(&mut d, &Mark { text: "𠀀", size: 40., rgb: [0.; 3], opacity: 0.3, pos: "center", angle: 0. }, true).unwrap_err();
    assert!(e.contains("𠀀"));
}
