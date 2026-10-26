//! File-list icons drawn from our own SVG layers (assets/icons/files): a filled light-blue
//! folder, and a white page with a colored mark for text / code / archive / image / executable.
//! GPUI paints an SVG as a single-color mask, so each icon is a few stacked layers.
use std::borrow::Cow;

use gpui::{div, prelude::*, px, rgb, svg, AnyElement, SharedString};


/// Embedded icon layers, served under `shellax/files/…`; everything else falls through to the
/// stock icon set.
pub struct Assets;
const FILES: &[(&str, &[u8])] = &[
    ("shellax/files/folder_back.svg", include_bytes!("../assets/icons/files/folder_back.svg")),
    ("shellax/files/folder_front.svg", include_bytes!("../assets/icons/files/folder_front.svg")),
    ("shellax/files/doc_page.svg", include_bytes!("../assets/icons/files/doc_page.svg")),
    ("shellax/files/doc_edge.svg", include_bytes!("../assets/icons/files/doc_edge.svg")),
    ("shellax/files/doc_fold.svg", include_bytes!("../assets/icons/files/doc_fold.svg")),
    ("shellax/files/mark_text.svg", include_bytes!("../assets/icons/files/mark_text.svg")),
    ("shellax/files/mark_code.svg", include_bytes!("../assets/icons/files/mark_code.svg")),
    ("shellax/files/mark_archive.svg", include_bytes!("../assets/icons/files/mark_archive.svg")),
    ("shellax/files/mark_image.svg", include_bytes!("../assets/icons/files/mark_image.svg")),
    ("shellax/files/mark_exec.svg", include_bytes!("../assets/icons/files/mark_exec.svg")),
];
impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, b)) = FILES.iter().find(|(p, _)| *p == path) { return Ok(Some(Cow::Borrowed(b))); }
        gpui_kit_assets::AllAssets.load(path)
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut v = gpui_kit_assets::AllAssets.list(path)?;
        v.extend(FILES.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)));
        Ok(v)
    }
}

/// The six looks a file-list entry can have (plus folders).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class { Folder, Text, Code, Archive, Image, Exec, Generic }

/// Icon class from the same name / mode rules as `filekind`.
pub fn class(name: &str, is_dir: bool, mode: u32) -> Class {
    use crate::ui::IconName as I;
    if is_dir { return Class::Folder; }
    let g = crate::filekind::glyph(name, false, mode, &[0; 16], false, 0);
    match g.icon {
        I::FileText | I::FileSpreadsheet if g.chip => Class::Text,
        I::FileCode | I::FileBraces | I::FileCog => Class::Code,
        I::FileArchive => Class::Archive,
        I::FileImage | I::FilePlay => Class::Image,
        I::FileTerminal => Class::Exec,
        _ => Class::Generic,
    }
}

fn layer(path: &'static str, size: f32, color: gpui::Hsla) -> gpui::Svg {
    svg().path(path).absolute().top_0().left_0().size(px(size)).text_color(color)
}

/// Stacked-layer icon of `size` px for a file-list row.
pub fn file_icon(cls: Class, size: f32, dark: bool) -> AnyElement {
    let h = |x: u32| gpui::Hsla::from(rgb(x));
    let mut d = div().relative().size(px(size)).flex_shrink_0();
    if cls == Class::Folder {
        let (back, front) = if dark { (0x3d8fd6, 0x62aef0) } else { (0x5aa9ec, 0x8cc8f8) };
        return d.child(layer("shellax/files/folder_back.svg", size, h(back))).child(layer("shellax/files/folder_front.svg", size, h(front))).into_any_element();
    }
    let (page, edge, fold) = if dark { (0xe9edf2, 0x8b95a3, 0xc5ccd6) } else { (0xffffff, 0xa7b0bd, 0xdfe4ea) };
    d = d.child(layer("shellax/files/doc_page.svg", size, h(page))).child(layer("shellax/files/doc_edge.svg", size, h(edge))).child(layer("shellax/files/doc_fold.svg", size, h(fold)));
    let mark = match cls {
        Class::Text => Some(("shellax/files/mark_text.svg", 0x7d8794)),
        Class::Code => Some(("shellax/files/mark_code.svg", 0x2f7de1)),
        Class::Archive => Some(("shellax/files/mark_archive.svg", 0x9a6b2f)),
        Class::Image => Some(("shellax/files/mark_image.svg", 0x2e9e6a)),
        Class::Exec => Some(("shellax/files/mark_exec.svg", 0x4b5563)),
        _ => None,
    };
    if let Some((p, col)) = mark { d = d.child(layer(p, size, h(col))); }
    d.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classes() {
        assert_eq!(class("src", true, 0), Class::Folder);
        assert_eq!(class("notes.md", false, 0), Class::Text);
        assert_eq!(class("main.rs", false, 0), Class::Code);
        assert_eq!(class("a.tar.gz", false, 0), Class::Archive);
        assert_eq!(class("p.png", false, 0), Class::Image);
        assert_eq!(class("tool", false, 0o755), Class::Exec);
        assert_eq!(class("weird.xyz", false, 0), Class::Generic);
    }
    #[test]
    fn layers_are_embedded() {
        use gpui::AssetSource;
        for (p, _) in FILES { assert!(Assets.load(p).unwrap().is_some()); }
    }
}
