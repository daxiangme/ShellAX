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
    ("shellax/files/folder_open_back.svg", include_bytes!("../assets/icons/files/folder_open_back.svg")),
    ("shellax/files/folder_open_front.svg", include_bytes!("../assets/icons/files/folder_open_front.svg")),
    ("shellax/files/monitor_body.svg", include_bytes!("../assets/icons/files/monitor_body.svg")),
    ("shellax/files/monitor_screen.svg", include_bytes!("../assets/icons/files/monitor_screen.svg")),
    ("shellax/files/laptop_body.svg", include_bytes!("../assets/icons/files/laptop_body.svg")),
    ("shellax/files/laptop_screen.svg", include_bytes!("../assets/icons/files/laptop_screen.svg")),
    ("shellax/files/server_body.svg", include_bytes!("../assets/icons/files/server_body.svg")),
    ("shellax/files/server_leds.svg", include_bytes!("../assets/icons/files/server_leds.svg")),
    ("shellax/files/doc_page.svg", include_bytes!("../assets/icons/files/doc_page.svg")),
    ("shellax/files/doc_edge.svg", include_bytes!("../assets/icons/files/doc_edge.svg")),
    ("shellax/files/doc_fold.svg", include_bytes!("../assets/icons/files/doc_fold.svg")),
    ("shellax/files/mark_text.svg", include_bytes!("../assets/icons/files/mark_text.svg")),
    ("shellax/files/mark_code.svg", include_bytes!("../assets/icons/files/mark_code.svg")),
    ("shellax/files/mark_archive.svg", include_bytes!("../assets/icons/files/mark_archive.svg")),
    ("shellax/files/mark_image.svg", include_bytes!("../assets/icons/files/mark_image.svg")),
    ("shellax/files/mark_exec.svg", include_bytes!("../assets/icons/files/mark_exec.svg")),
    ("shellax/db/docker.svg", include_bytes!("../assets/icons/db/docker.svg")),
    ("shellax/db/redis.svg", include_bytes!("../assets/icons/db/redis.svg")),
    ("shellax/db/toolbox.svg", include_bytes!("../assets/icons/db/toolbox.svg")),
    ("shellax/db/generic.svg", include_bytes!("../assets/icons/db/generic.svg")),
    ("shellax/db/mysql.svg", include_bytes!("../assets/icons/db/mysql.svg")),
    ("shellax/db/postgres.svg", include_bytes!("../assets/icons/db/postgres.svg")),
    ("shellax/db/sqlite.svg", include_bytes!("../assets/icons/db/sqlite.svg")),
    ("shellax/db/sqlserver.svg", include_bytes!("../assets/icons/db/sqlserver.svg")),
    ("shellax/db/clickhouse.svg", include_bytes!("../assets/icons/db/clickhouse.svg")),
    ("shellax/db/duckdb.svg", include_bytes!("../assets/icons/db/duckdb.svg")),
    ("shellax/db/mongodb.svg", include_bytes!("../assets/icons/db/mongodb.svg")),
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
        return folder_icon(size, false);
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

/// 填充文件夹（SFTP 文件列表与主机列表共用）：`open` 为展开态。颜色来自 `theme::folder_colors`。
pub fn folder_icon(size: f32, open: bool) -> AnyElement {
    let (back, front) = crate::theme::folder_colors();
    let (b, f) = if open { ("shellax/files/folder_open_back.svg", "shellax/files/folder_open_front.svg") } else { ("shellax/files/folder_back.svg", "shellax/files/folder_front.svg") };
    div().relative().size(px(size)).flex_shrink_0().child(layer(b, size, back)).child(layer(f, size, front)).into_any_element()
}

/// 填充服务器：浅中性机身加强调色指示灯。
pub fn server_icon(size: f32) -> AnyElement {
    div().relative().size(px(size)).flex_shrink_0()
        .child(layer("shellax/files/server_body.svg", size, crate::theme::entity_body()))
        .child(layer("shellax/files/server_leds.svg", size, crate::theme::entity_accent())).into_any_element()
}

/// 填充显示器：浅中性机身加强调色屏幕。
pub fn monitor_icon(size: f32) -> AnyElement {
    div().relative().size(px(size)).flex_shrink_0()
        .child(layer("shellax/files/monitor_body.svg", size, crate::theme::entity_body()))
        .child(layer("shellax/files/monitor_screen.svg", size, crate::theme::entity_accent())).into_any_element()
}

/// 填充笔记本（Mac 本机用）：屏幕盖板 + 更宽的底座为机身色，屏幕为强调色；不含任何品牌标志。
pub fn laptop_icon(size: f32) -> AnyElement {
    div().relative().size(px(size)).flex_shrink_0()
        .child(layer("shellax/files/laptop_body.svg", size, crate::theme::entity_body()))
        .child(layer("shellax/files/laptop_screen.svg", size, crate::theme::entity_accent())).into_any_element()
}

/// 「本机」图标：macOS 用笔记本，Windows / Linux 用显示器。主机列表本地终端行与 SFTP 本地来源共用此入口。
pub fn local_icon(size: f32) -> AnyElement {
    if cfg!(target_os = "macos") { laptop_icon(size) } else { monitor_icon(size) }
}

/// 文件列表里一行文件 / 文件夹的图标：本机与远程窗格、「..」行共用同一个入口，样式只在这里定义。
pub fn entry_icon(name: &str, is_dir: bool, mode: u32, size: f32, dark: bool) -> AnyElement {
    file_icon(class(name, is_dir, mode), size, dark)
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

/// Engine glyph path (our own single-color drawings under assets/icons/db).
pub fn db_glyph_path(kind: &str) -> &'static str {
    match kind {
        "redis" => "shellax/db/redis.svg",
        "mysql" | "mariadb" | "starrocks" | "oceanbase" => "shellax/db/mysql.svg",
        "postgres" | "opengauss" | "kingbase" => "shellax/db/postgres.svg",
        "sqlite" => "shellax/db/sqlite.svg",
        "sqlserver" => "shellax/db/sqlserver.svg",
        "clickhouse" => "shellax/db/clickhouse.svg",
        "duckdb" => "shellax/db/duckdb.svg",
        "mongodb" => "shellax/db/mongodb.svg",
        _ => "shellax/db/generic.svg",
    }
}

/// Tint used for an engine glyph while its connection is open.
pub fn db_tint(kind: &str) -> gpui::Hsla {
    rgb(match kind {
        "redis" => 0xd9453a,
        "mysql" | "mariadb" | "starrocks" | "oceanbase" => 0x2f8fb0,
        "postgres" | "opengauss" | "kingbase" => 0x4a78b5,
        "sqlite" => 0x3c95d6,
        "sqlserver" => 0xc8463d,
        "clickhouse" => 0xd9a816,
        "duckdb" => 0xd6b31c,
        "mongodb" => 0x4f9f45,
        _ => 0x7c8aa5,
    }).into()
}

/// Engine glyph element.
pub fn db_glyph(kind: &str, size: f32, color: gpui::Hsla) -> gpui::Svg {
    svg().path(db_glyph_path(kind)).size(px(size)).flex_shrink_0().text_color(color)
}

#[cfg(test)]
mod db_glyph_tests {
    use super::*;
    #[test]
    fn every_kind_has_an_embedded_glyph() {
        for k in ssh_term_proto::assets::DbKind::ALL.iter().map(|k| k.id()).chain(["redis"]) {
            let p = db_glyph_path(k);
            assert!(FILES.iter().any(|(f, _)| *f == p), "{k}");
        }
        for (p, b) in FILES { assert!(std::str::from_utf8(b).unwrap().starts_with("<svg"), "{p}"); }
    }
}
