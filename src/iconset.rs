//! 自定义图标的 id 表（侧栏「更换图标」）：分组 / 位置的文件夹颜色与字形，主机的图标与强调色。
//! 只存 id 字符串（配置里写 `icon` / `icon_color`）；id → 内置 SVG 图层的映射在这里，颜色 token 在 `theme.rs`
//! 的 `ICON_TONES`，渲染在 `file_icon.rs`。不接受用户导入的图标：未知 id 一律当作默认。

/// 色板（文件夹颜色与主机强调色共用），第一项是默认色（SFTP 文件夹蓝）。
pub const COLORS: [&str; 8] = ["blue", "green", "orange", "red", "purple", "cyan", "gray", "yellow"];
/// 文件夹字形：第一项 `plain` 是默认（无字形）。
pub const FOLDER_GLYPHS: [&str; 12] = ["plain", "code", "branch", "doc", "database", "cloud", "star", "lock", "home", "download", "image", "terminal"];
/// 主机图标：前 8 个是设备，后 3 个是系统标记（简单几何图形，不是官方标志）；第一项 `server` 是默认。
pub const HOST_ICONS: [&str; 11] = ["server", "cloud", "laptop", "desktop", "router", "dbserver", "container", "board", "linux", "windows", "mac"];

pub fn is_color(id: &str) -> bool { COLORS.contains(&id) }
pub fn is_folder_glyph(id: &str) -> bool { FOLDER_GLYPHS.contains(&id) }
pub fn is_host_icon(id: &str) -> bool { HOST_ICONS.contains(&id) }

/// 文件夹字形层的 SVG 路径；`plain`、空与未知 id 没有字形层。
pub fn folder_glyph_path(id: &str) -> Option<&'static str> {
    Some(match id {
        "code" => "shellax/files/glyph_code.svg",
        "branch" => "shellax/files/glyph_branch.svg",
        "doc" => "shellax/files/glyph_doc.svg",
        "database" => "shellax/files/glyph_database.svg",
        "cloud" => "shellax/files/glyph_cloud.svg",
        "star" => "shellax/files/glyph_star.svg",
        "lock" => "shellax/files/glyph_lock.svg",
        "home" => "shellax/files/glyph_home.svg",
        "download" => "shellax/files/glyph_download.svg",
        "image" => "shellax/files/glyph_image.svg",
        "terminal" => "shellax/files/glyph_terminal.svg",
        _ => return None,
    })
}

/// 主机图标的 `(机身层, 强调层)` SVG 路径；未知 id 返回 `None`（调用方回退到默认服务器）。
pub fn host_layers(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "server" => ("shellax/files/server_body.svg", "shellax/files/server_leds.svg"),
        "cloud" => ("shellax/files/host_cloud_body.svg", "shellax/files/host_cloud_accent.svg"),
        "laptop" => ("shellax/files/laptop_body.svg", "shellax/files/laptop_screen.svg"),
        "desktop" => ("shellax/files/monitor_body.svg", "shellax/files/monitor_screen.svg"),
        "router" => ("shellax/files/host_router_body.svg", "shellax/files/host_router_accent.svg"),
        "dbserver" => ("shellax/files/host_dbserver_body.svg", "shellax/files/host_dbserver_accent.svg"),
        "container" => ("shellax/files/host_container_body.svg", "shellax/files/host_container_accent.svg"),
        "board" => ("shellax/files/host_board_body.svg", "shellax/files/host_board_accent.svg"),
        "linux" => ("shellax/files/os_linux_body.svg", "shellax/files/os_linux_accent.svg"),
        "windows" => ("shellax/files/os_windows_body.svg", "shellax/files/os_windows_accent.svg"),
        "mac" => ("shellax/files/os_mac_body.svg", "shellax/files/os_mac_accent.svg"),
        _ => return None,
    })
}

/// 所有内置图层路径（资源表与测试共用）。
pub fn all_layer_paths() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = FOLDER_GLYPHS.iter().filter_map(|g| folder_glyph_path(g)).collect();
    for h in HOST_ICONS { if let Some((a, b)) = host_layers(h) { v.push(a); v.push(b); } }
    v
}

/// 字段值是否为有效且非默认的 id（`ok` 判定有效性）；用于渲染：无效或默认都回到默认外观。
pub fn pick<'a>(v: &'a Option<String>, ok: fn(&str) -> bool) -> Option<&'a str> { v.as_deref().filter(|s| ok(s)) }

/// 写入配置：默认值与未知 id 存成 `None`（不写入文件）。
pub fn store(id: &str, default: &str, ok: fn(&str) -> bool) -> Option<String> { (id != default && ok(id)).then(|| id.to_string()) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_are_unique_and_defaults_first() {
        for list in [&COLORS[..], &FOLDER_GLYPHS[..], &HOST_ICONS[..]] {
            let mut v = list.to_vec(); v.sort(); v.dedup();
            assert_eq!(v.len(), list.len());
        }
        assert_eq!((COLORS[0], FOLDER_GLYPHS[0], HOST_ICONS[0]), ("blue", "plain", "server"));
        assert_eq!((COLORS.len(), FOLDER_GLYPHS.len(), HOST_ICONS.len()), (8, 12, 11));
    }
    #[test]
    fn every_id_maps_to_layers() {
        for g in FOLDER_GLYPHS { assert_eq!(folder_glyph_path(g).is_some(), g != "plain", "{g}"); }
        for h in HOST_ICONS { assert!(host_layers(h).is_some(), "{h}"); }
        assert!(host_layers("apple").is_none() && folder_glyph_path("../x").is_none());
        // 每个图层文件都存在于 assets/icons/files 且是 SVG
        for p in all_layer_paths() {
            let f = format!("{}/assets/icons/{}", env!("CARGO_MANIFEST_DIR"), p.trim_start_matches("shellax/"));
            let body = std::fs::read_to_string(&f).unwrap_or_else(|_| panic!("missing {f}"));
            assert!(body.starts_with("<svg") && body.contains("currentColor"), "{f}");
        }
    }
    #[test]
    fn labels_exist_in_both_locales() {
        let (en, zh) = (crate::i18n::parse(crate::i18n::EN_SRC), crate::i18n::parse(crate::i18n::ZH_SRC));
        let keys = COLORS.iter().map(|c| format!("icon.color.{c}")).chain(FOLDER_GLYPHS.iter().map(|g| format!("icon.glyph.{g}"))).chain(HOST_ICONS.iter().map(|h| format!("icon.host.{h}")));
        for k in keys { assert!(en.contains_key(&k) && zh.contains_key(&k), "{k}"); }
    }
    #[test]
    fn store_and_pick() {
        assert_eq!(store("green", "blue", is_color), Some("green".into()));
        assert_eq!(store("blue", "blue", is_color), None);
        assert_eq!(store("nope", "blue", is_color), None);
        assert_eq!(pick(&Some("nope".into()), is_color), None);
        assert_eq!(pick(&Some("red".into()), is_color), Some("red"));
    }
}
