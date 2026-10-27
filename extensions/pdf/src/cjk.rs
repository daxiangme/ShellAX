//! 中日韩文字：内置思源黑体（Noto Sans SC）常用字子集，按文档再次子集化后以 CID 字体（Type0 / CIDFontType2，Identity-H）嵌入。
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use std::collections::BTreeMap;
use subsetter::GlyphRemapper;

pub static FONT: &[u8] = include_bytes!("../../assets/NotoSansSC-common.ttf");

/// 文本里是否有标准字体（WinAnsi）画不出的字符。
pub fn needs_cjk(s: &str) -> bool { s.chars().any(|c| c as u32 > 255) }

/// 为一组字符串准备字形：字符 -> (新 CID, 宽度/1000)。
pub struct Shaper { map: BTreeMap<char, (u16, f32)>, remap: GlyphRemapper, missing: Vec<char> }

impl Shaper {
    pub fn new(texts: &[&str]) -> Result<Shaper, String> {
        let face = ttf_parser::Face::parse(FONT, 0).map_err(|e| e.to_string())?;
        let upem = face.units_per_em() as f32;
        let mut remap = GlyphRemapper::new();
        let mut map = BTreeMap::new();
        let mut missing = vec![];
        for c in texts.iter().flat_map(|t| t.chars()) {
            if map.contains_key(&c) { continue; }
            let gid = face.glyph_index(c);
            if gid.is_none() && !c.is_whitespace() && !missing.contains(&c) { missing.push(c); }
            let gid = gid.unwrap_or(ttf_parser::GlyphId(0));
            let cid = remap.remap(gid.0);
            let w = face.glyph_hor_advance(gid).unwrap_or(0) as f32 / upem;
            map.insert(c, (cid, w));
        }
        Ok(Shaper { map, remap, missing })
    }
    /// 字库里没有的字符（会显示为空框）。
    pub fn missing(&self) -> &[char] { &self.missing }
    /// 编码为十六进制字符串 `<...>`，并返回字号为 size 时的宽度。
    pub fn encode(&self, s: &str, size: f32) -> (String, f32) {
        let mut hex = String::from("<");
        let mut w = 0.;
        for c in s.chars() {
            if let Some(&(cid, cw)) = self.map.get(&c) { hex += &format!("{cid:04X}"); w += cw; }
        }
        hex.push('>');
        (hex, w * size)
    }
    /// 写入字体对象，返回 Type0 字体的对象 id。
    pub fn embed(&self, doc: &mut Document) -> Result<ObjectId, String> {
        let data = subsetter::subset(FONT, 0, &self.remap).map_err(|e| format!("font subset: {e:?}"))?;
        let mut file = Stream::new(dictionary! { "Length1" => data.len() as i64 }, data);
        let _ = file.compress();
        let file = doc.add_object(file);
        // 子集字体名需要 6 位大写前缀，取字符集的简单散列。
        let h = self.map.keys().fold(5381u32, |h, &c| h.wrapping_mul(33) ^ c as u32);
        let tag: String = (0..6).map(|i| (b'A' + ((h >> (i * 5)) % 26) as u8) as char).collect();
        let name = format!("{tag}+NotoSansSC-Regular");
        let desc = doc.add_object(dictionary! {
            "Type" => "FontDescriptor", "FontName" => Object::Name(name.clone().into_bytes()), "Flags" => 4,
            "FontBBox" => vec![(-1000).into(), (-300).into(), 1100.into(), 1100.into()], "ItalicAngle" => 0,
            "Ascent" => 880, "Descent" => -120, "CapHeight" => 733, "StemV" => 80, "FontFile2" => Object::Reference(file),
        });
        let mut ws: Vec<(u16, f32)> = self.map.values().copied().collect();
        ws.sort_by_key(|x| x.0);
        ws.dedup_by_key(|x| x.0);
        let mut w_arr = vec![];
        for (cid, w) in &ws { w_arr.push(Object::Integer(*cid as i64)); w_arr.push(Object::Array(vec![Object::Integer((w * 1000.).round() as i64)])); }
        let cid_font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => Object::Name(name.clone().into_bytes()),
            "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 },
            "FontDescriptor" => Object::Reference(desc), "DW" => 1000, "W" => w_arr, "CIDToGIDMap" => "Identity",
        });
        // ToUnicode：便于复制、搜索文字。
        let mut cmap = String::from("/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");
        let pairs: Vec<_> = self.map.iter().collect();
        for chunk in pairs.chunks(100) {
            cmap += &format!("{} beginbfchar\n", chunk.len());
            for (c, (cid, _)) in chunk {
                let mut u = [0u16; 2];
                let hex: String = c.encode_utf16(&mut u).iter().map(|x| format!("{x:04X}")).collect();
                cmap += &format!("<{cid:04X}> <{hex}>\n");
            }
            cmap += "endbfchar\n";
        }
        cmap += "endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
        let tu = doc.add_object(Stream::new(dictionary! {}, cmap.into_bytes()));
        Ok(doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type0", "BaseFont" => Object::Name(name.into_bytes()), "Encoding" => "Identity-H",
            "DescendantFonts" => vec![Object::Reference(cid_font)], "ToUnicode" => Object::Reference(tu),
        }))
    }
}
