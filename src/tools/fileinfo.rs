//! 文件结果的描述：大小、图片尺寸、PDF 页数、压缩包文件数，以及可在界面里预览的 PNG / JPEG。
//! 只做轻量的字节扫描，不解码整个文档。
use std::io::Cursor;

/// 文件结果的大类。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Pdf,
    Zip,
    #[default]
    Other,
}

/// 预览图的格式（界面直接从内存加载）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewFmt { Png, Jpeg }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
    pub kind: FileKind,
    /// 图片像素尺寸。
    pub dims: Option<(u32, u32)>,
    /// PDF 页数（能数出来时）。
    pub pages: Option<usize>,
    /// 压缩包里的文件数。
    pub entries: Option<usize>,
    /// 可预览的图片：文件本身（PNG / JPEG），或压缩包里的第一张 PNG。
    pub preview: Option<(PreviewFmt, Vec<u8>)>,
    /// 预览图的像素尺寸。
    pub preview_dims: Option<(u32, u32)>,
}

fn ext_of(name: &str) -> String { name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default() }

/// 分析一个输出文件。
pub fn inspect(name: &str, data: &[u8]) -> FileInfo {
    let mut f = FileInfo { name: name.to_string(), size: data.len() as u64, ..Default::default() };
    match ext_of(name).as_str() {
        "png" | "jpg" | "jpeg" => {
            f.kind = FileKind::Image;
            f.dims = image::ImageReader::new(Cursor::new(data)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
            f.preview_dims = f.dims;
            f.preview = Some((if data.starts_with(&[0x89, b'P', b'N', b'G']) { PreviewFmt::Png } else { PreviewFmt::Jpeg }, data.to_vec()));
        }
        "pdf" => { f.kind = FileKind::Pdf; f.pages = pdf_pages(data); }
        "zip" => {
            f.kind = FileKind::Zip;
            f.entries = zip_entries(data);
            f.preview = zip_first_png(data).map(|b| (PreviewFmt::Png, b));
            f.preview_dims = f.preview.as_ref().and_then(|(_, b)| image::ImageReader::new(Cursor::new(b)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok()));
        }
        _ => {}
    }
    f
}

/// 数 `/Type /Page` 对象（不含 `/Pages`）。压缩在对象流里的 PDF 数不出来，返回 `None`。
pub fn pdf_pages(data: &[u8]) -> Option<usize> {
    let mut n = 0;
    let mut i = 0;
    while let Some(k) = find(data, b"/Type", i) {
        let mut j = k + 5;
        while j < data.len() && data[j].is_ascii_whitespace() { j += 1; }
        if data[j..].starts_with(b"/Page") && !data[j + 5..].starts_with(b"s") { n += 1; }
        i = k + 5;
    }
    (n > 0).then_some(n)
}

fn find(h: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= h.len() { return None; }
    h[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// 从末尾的“中央目录结束记录”读文件数。
pub fn zip_entries(data: &[u8]) -> Option<usize> {
    let sig = [0x50, 0x4b, 0x05, 0x06];
    let start = data.len().saturating_sub(22 + 65535);
    let pos = data[start..].windows(4).rposition(|w| w == sig)? + start;
    let b = data.get(pos + 10..pos + 12)?;
    Some(u16::from_le_bytes([b[0], b[1]]) as usize)
}

/// 压缩包里第一个以 `.png` 结尾的、未压缩（stored）条目的内容；扩展包的 zip 都是 stored。
pub fn zip_first_png(data: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    while data.get(i..i + 4)? == [0x50, 0x4b, 0x03, 0x04] {
        let h = data.get(i..i + 30)?;
        let method = u16::from_le_bytes([h[8], h[9]]);
        let size = u32::from_le_bytes([h[18], h[19], h[20], h[21]]) as usize;
        let (nl, el) = (u16::from_le_bytes([h[26], h[27]]) as usize, u16::from_le_bytes([h[28], h[29]]) as usize);
        let name = data.get(i + 30..i + 30 + nl)?;
        let body = i + 30 + nl + el;
        if method != 0 { return None; }
        if name.to_ascii_lowercase().ends_with(b".png") { return data.get(body..body + size).map(|b| b.to_vec()); }
        i = body + size;
    }
    None
}

/// 字节数的可读写法（1.4 MB）。
pub fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024. && u < 3 { v /= 1024.; u += 1; }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[u]) }
}

/// 相对压缩前的变化百分比：变小为正（节省），变大为负。
pub fn change_pct(before: u64, after: u64) -> f64 {
    if before == 0 { 0. } else { (before as f64 - after as f64) / before as f64 * 100. }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = vec![];
        let mut cd = vec![];
        for (name, data) in files {
            let off = out.len() as u32;
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            cd.extend_from_slice(&off.to_le_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(7, 5, image::Rgba([1, 2, 3, 255]));
        let mut b = vec![];
        img.write_to(&mut Cursor::new(&mut b), image::ImageFormat::Png).unwrap();
        b
    }

    #[test]
    fn image_pdf_zip() {
        let p = png_bytes();
        let f = inspect("a.png", &p);
        assert_eq!((f.kind, f.dims), (FileKind::Image, Some((7, 5))));
        assert_eq!(f.preview.as_ref().unwrap().0, PreviewFmt::Png);
        let pdf = b"%PDF-1.4\n1 0 obj<</Type /Pages /Count 2>>endobj 2 0 obj<</Type/Page>>endobj 3 0 obj<< /Type /Page >>endobj";
        assert_eq!(inspect("x.pdf", pdf).pages, Some(2));
        let z = stored_zip(&[("a.txt", b"hi"), ("p-1.png", &p), ("p-2.png", &p)]);
        let f = inspect("x.zip", &z);
        assert_eq!(f.entries, Some(3));
        assert_eq!(f.preview_dims, Some((7, 5)));
        assert_eq!(f.preview.unwrap().1, p);
        assert_eq!(inspect("x.bin", b"abc").kind, FileKind::Other);
    }

    #[test]
    fn sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(3 << 20), "3.0 MB");
        assert_eq!(change_pct(1000, 250), 75.);
        assert_eq!(change_pct(1000, 1100), -10.);
        assert_eq!(change_pct(0, 5), 0.);
    }
}
