//! 文件列表的共用部分：排序、行、骨架、路径。SFTP 双栏和右侧「目录」卡片都走这里，
//! 行高、选中底色和列宽与原来的 SFTP 列表一致。
use gpui::{div, prelude::*, px, AnimationExt, AnyElement};
use ssh_term_proto::i18n::{t, tf};
use ssh_term_proto::sftp::{self, Entry};
use ssh_term_proto::sftp_follow::{follow_target, Follow};

use crate::theme::{c, cur};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey { Name, Mtime, Size, Kind }

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Sort { pub key: SortKey, pub desc: bool }

impl Default for Sort {
    fn default() -> Self { Self { key: SortKey::Name, desc: false } }
}

impl Sort {
    /// 点同一列翻转方向；时间和大小第一次点击从新到旧、从大到小。
    pub fn toggle(&mut self, key: SortKey) {
        if self.key == key { self.desc = !self.desc; }
        else { *self = Self { key, desc: matches!(key, SortKey::Mtime | SortKey::Size) }; }
    }
}

/// 目录排在文件前面。同一组里按所选列排，相等时保持原下标，避免来回抖动。
pub fn sorted_indices(entries: &[Entry], sort: Sort) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ea, eb) = (&entries[a], &entries[b]);
        let dir = match (ea.is_dir, eb.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        };
        let ord = match sort.key {
            SortKey::Name => ea.name.to_lowercase().cmp(&eb.name.to_lowercase()),
            SortKey::Mtime => ea.mtime.cmp(&eb.mtime),
            SortKey::Size => ea.size.cmp(&eb.size),
            SortKey::Kind => ext_of(ea).cmp(&ext_of(eb)).then(ea.name.to_lowercase().cmp(&eb.name.to_lowercase())),
        };
        dir.then(if sort.desc { ord.reverse() } else { ord }).then(a.cmp(&b))
    });
    idx
}

pub fn human(n: u64) -> String {
    if n < 1024 { format!("{n} B") }
    else if n < 1 << 20 { format!("{:.1} KB", n as f64 / 1024.0) }
    else if n < 1 << 30 { format!("{:.1} MB", n as f64 / 1048576.0) }
    else { format!("{:.1} GB", n as f64 / 1073741824.0) }
}

fn fmt_mtime(t: u64) -> String {
    if t == 0 { return "—".into(); }
    chrono::DateTime::from_timestamp(t as i64, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

fn ext_of(e: &Entry) -> String {
    if e.is_dir { return String::new(); }
    match e.name.rsplit_once('.') {
        Some((stem, x)) if !stem.is_empty() && !x.is_empty() => x.to_lowercase(),
        _ => String::new(),
    }
}

fn kind_label(e: &Entry) -> String {
    if e.is_dir { return t("sftp.type_folder").to_string(); }
    let x = ext_of(e);
    if x.is_empty() { t("sftp.type_plain").to_string() } else { tf("sftp.type_file", &[("x", &x.to_uppercase())]) }
}

/// 过滤后的文件数、文件夹数、文件字节（文件夹不计入大小）。
pub fn tally(entries: &[Entry], query: &str) -> (usize, usize, u64) {
    let q = query.trim().to_lowercase();
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut bytes = 0u64;
    for e in entries {
        if !q.is_empty() && !e.name.to_lowercase().contains(&q) { continue; }
        if e.is_dir { dirs += 1; } else { files += 1; bytes = bytes.saturating_add(e.size); }
    }
    (files, dirs, bytes)
}

#[derive(Clone, Copy)]
pub struct Cols { pub mtime: Option<f32>, pub size: Option<f32>, pub kind: Option<f32> }

pub fn cols_wide() -> Cols { Cols { mtime: Some(132.), size: Some(76.), kind: Some(88.) } }
pub fn cols_narrow() -> Cols { Cols { mtime: None, size: Some(76.), kind: None } }

fn meta(w: f32, text: String, ellipsis: bool) -> gpui::Div {
    let th = cur();
    div().w(px(w)).flex_shrink_0().px_1().overflow_hidden().whitespace_nowrap()
        .when(ellipsis, |d| d.text_ellipsis())
        .text_size(px(crate::ui::fs_small())).text_color(c(th.overlay0)).child(text)
}

/// 一行文件。调用方再挂点击、右键和拖拽。选中是淡强调底 + 正文色，和 SFTP 列表相同。
pub fn entry_row(id: impl Into<gpui::ElementId>, e: &Entry, selected: bool, dark: bool, cols: Cols) -> gpui::Stateful<gpui::Div> {
    let th = cur();
    let hidden = e.name.starts_with('.');
    let mut row = div().id(id).h(px(26.)).flex_shrink_0().px_3().flex().items_center().gap_2()
        .text_size(px(crate::ui::fs_body())).text_color(c(th.text)).cursor_pointer()
        .when(hidden, |d| d.opacity(0.72))
        .when(selected, |d| d.bg(c(th.accent).opacity(0.2)))
        .when(!selected, |d| d.hover(|d| d.bg(crate::theme::hover_bg())))
        .child(crate::file_icon::entry_icon(&e.name, e.is_dir, e.mode, crate::ui::IC_LG, dark))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.name.clone()));
    if let Some(w) = cols.mtime { row = row.child(meta(w, fmt_mtime(e.mtime), false)); }
    if let Some(w) = cols.size { row = row.child(meta(w, if e.is_dir { "—".into() } else { human(e.size) }, false)); }
    if let Some(w) = cols.kind { row = row.child(meta(w, kind_label(e), true)); }
    row
}

/// 「..」行：次要文字色，双击由调用方处理。
pub fn parent_row(id: impl Into<gpui::ElementId>, dark: bool) -> gpui::Stateful<gpui::Div> {
    let th = cur();
    div().id(id).h(px(26.)).flex_shrink_0().px_3().flex().items_center().gap_2()
        .text_size(px(crate::ui::fs_body())).text_color(c(th.subtext)).cursor_pointer()
        .hover(|d| d.bg(crate::theme::hover_bg()))
        .child(crate::file_icon::entry_icon("..", true, 0, crate::ui::IC_LG, dark))
        .child("..")
}

/// 表头的排序格子。调用方加上点击和列宽。
pub fn sort_cell(id: impl Into<gpui::ElementId>, label: impl Into<gpui::SharedString>, active: bool, desc: bool) -> gpui::Stateful<gpui::Div> {
    let th = cur();
    let mark = if !active { String::new() } else if desc { " ↓".to_string() } else { " ↑".to_string() };
    let label = label.into();
    div().id(id).h(px(crate::ui::H_DEFAULT)).px_1().flex().items_center().rounded(px(crate::ui::R_SM)).cursor_pointer()
        .overflow_hidden().whitespace_nowrap()
        .text_size(px(crate::ui::fs_small())).text_color(if active { c(th.accent) } else { c(th.overlay1) })
        .hover(|d| d.bg(crate::theme::hover_bg()))
        .child(format!("{label}{mark}"))
}

/// 表头：左侧留出与行图标对齐的 16px。
pub fn sort_header() -> gpui::Div {
    crate::ui::table_header().px_3().gap_2().child(div().size(px(16.)).flex_shrink_0())
}

/// 列表加载骨架。`prefix` 同时用作动画 id 和子元素 id 前缀，避免两处列表同时出现时 id 撞车。
pub fn listing_skeleton(prefix: &'static str) -> AnyElement {
    let th = cur();
    let widths = [148., 96., 188., 120., 164., 84., 210., 132.];
    let mut col = div().flex().flex_col();
    for (i, w) in widths.into_iter().enumerate() {
        col = col.child(div().h(px(32.)).px_3().flex().items_center().gap(px(10.))
            .child(div().id((prefix, i * 3)).size(px(16.)).flex_shrink_0().rounded(px(crate::ui::R_SM)).bg(c(th.surface1)))
            .child(div().id((prefix, i * 3 + 1)).h(px(12.)).w(px(w)).rounded(px(crate::ui::R_SM)).bg(c(th.surface1)))
            .child(div().flex_1())
            .child(div().id((prefix, i * 3 + 2)).h(px(10.)).w(px(64.)).rounded(px(crate::ui::R_SM)).bg(c(th.surface0))));
    }
    if crate::motion::reduced() { return col.into_any_element(); }
    col.with_animation(
        prefix,
        gpui::Animation::new(std::time::Duration::from_millis(1100)).repeat_synced().with_max_fps(30.).with_easing(|t| (t * std::f32::consts::PI).sin()),
        |d, t| d.opacity(0.35 + 0.65 * t),
    ).into_any_element()
}

fn is_drive_root(path: &str) -> bool {
    let s = path.replace('/', "\\");
    let t = s.trim_end_matches('\\');
    t.len() == 2 && t.as_bytes().first().is_some_and(|c| c.is_ascii_alphabetic()) && t.as_bytes().get(1) == Some(&b':')
}

fn win_abs(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// 去掉末尾斜杠（根目录和盘符根保留）。空串保持空串，避免被规范成 `.`。
pub fn tidy(path: &str, remote: bool) -> String {
    if path.is_empty() { return String::new(); }
    if remote {
        let n = ssh_term_proto::ctxmenu::normalize(path);
        return if n.is_empty() { "/".into() } else { n };
    }
    if is_drive_root(path) {
        let t = path.trim_end_matches(['\\', '/']);
        return format!("{t}\\");
    }
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return if path.contains('\\') && !path.contains('/') { "\\".into() } else { "/".into() };
    }
    trimmed.to_string()
}

/// 上一级。根目录、空路径和盘符根没有上一级（不显示「..」）。
pub fn parent_of(path: &str, remote: bool) -> Option<String> {
    if path.is_empty() { return None; }
    if remote {
        let p = tidy(path, true);
        if p.is_empty() || p == "/" { return None; }
        let up = sftp::parent(&p);
        return (up != p).then_some(up);
    }
    let p = tidy(path, false);
    if p.is_empty() || p == "/" || p == "\\" || is_drive_root(&p) { return None; }
    if p.contains('\\') {
        let t = p.trim_end_matches('\\');
        let i = t.rfind('\\')?;
        let up = if i == 2 && t.as_bytes().get(1) == Some(&b':') { format!("{}\\", &t[..2]) } else { t[..i].to_string() };
        let up = tidy(&up, false);
        return (up != p && !up.is_empty()).then_some(up);
    }
    let parent = std::path::Path::new(&p).parent()?;
    let s = parent.to_string_lossy();
    if s.is_empty() { return None; }
    let up = tidy(&s, false);
    (up != p && !up.is_empty()).then_some(up)
}

pub fn child_path(dir: &str, name: &str, remote: bool) -> String {
    if remote { sftp::join(dir, name) } else { tidy(&std::path::Path::new(dir).join(name).to_string_lossy(), false) }
}

/// 把终端 cwd 收成可列出的绝对路径。远程走 `follow_target`（只认 Unix 路径）；
/// 本地同时接受 `~` 和 Windows 盘符，所以不能直接用 `follow_target`。
pub fn resolve_cwd(cwd: &str, home: &str, remote: bool) -> Option<String> {
    let c = cwd.trim();
    if c.is_empty() { return None; }
    if remote {
        let d = follow_target(c, home)?;
        let t = tidy(&d, true);
        return (!t.is_empty()).then_some(t);
    }
    let expanded = if c == "~" {
        if home.is_empty() { return None; }
        home.to_string()
    } else if let Some(r) = c.strip_prefix("~/").or_else(|| c.strip_prefix("~\\")) {
        if home.is_empty() { return None; }
        let h = home.trim_end_matches(['/', '\\']);
        let r = r.trim_start_matches(['/', '\\']);
        if h.contains('\\') || r.contains('\\') { format!("{h}\\{r}") } else { format!("{h}/{r}") }
    } else {
        c.to_string()
    };
    let abs = std::path::Path::new(&expanded).is_absolute() || win_abs(&expanded);
    if !abs { return None; }
    let t = tidy(&expanded, false);
    (!t.is_empty()).then_some(t)
}

/// 与 `Follow::step` 相同的节奏（只在终端 cwd 变化时跳转），但用 `resolve_cwd`，本地 Windows 路径也能跟上。
pub fn follow_step(follow: &mut Follow, cwd: Option<&str>, home: &str, current: &str, remote: bool) -> Option<String> {
    if !follow.on { return None; }
    let d = resolve_cwd(cwd?, home, remote)?;
    if follow.last.as_deref() == Some(d.as_str()) { return None; }
    follow.last = Some(d.clone());
    (d != current).then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, dir: bool, size: u64, mtime: u64) -> Entry {
        Entry { name: name.into(), is_dir: dir, size, mode: 0, mtime }
    }

    #[test]
    fn dirs_stay_above_files() {
        let v = vec![e("z.log", false, 10, 300), e("a", true, 0, 100), e("m.log", false, 99, 200), e("b", true, 0, 400), e("c.txt", false, 1, 1)];
        let names = |s: Sort| sorted_indices(&v, s).into_iter().map(|i| v[i].name.as_str()).collect::<Vec<_>>();
        assert_eq!(names(Sort { key: SortKey::Name, desc: false }), ["a", "b", "c.txt", "m.log", "z.log"]);
        assert_eq!(names(Sort { key: SortKey::Mtime, desc: true }), ["b", "a", "z.log", "m.log", "c.txt"]);
        assert_eq!(names(Sort { key: SortKey::Kind, desc: false }), ["a", "b", "m.log", "z.log", "c.txt"]);
    }

    #[test]
    fn tally_skips_dirs_and_filter() {
        let v = vec![e("a", true, 9, 0), e("b.txt", false, 10, 0), e("c.txt", false, 5, 0)];
        assert_eq!(tally(&v, ""), (2, 1, 15));
        assert_eq!(tally(&v, "b"), (1, 0, 10));
        assert_eq!(tally(&v, "a"), (0, 1, 0));
    }

    #[test]
    fn parent_of_roots() {
        assert_eq!(parent_of("", false), None);
        assert_eq!(parent_of("/", false), None);
        assert_eq!(parent_of("/tmp/", false).as_deref(), Some("/"));
        assert_eq!(parent_of("/a/b", false).as_deref(), Some("/a"));
        assert_eq!(parent_of("/", true), None);
        assert_eq!(parent_of("/a/b/", true).as_deref(), Some("/a"));
        assert_eq!(parent_of("/a", true).as_deref(), Some("/"));
        assert_eq!(parent_of(r"C:\", false), None);
        assert_eq!(parent_of(r"C:\Users", false).as_deref(), Some(r"C:\"));
        assert_eq!(parent_of(r"C:\Users\a", false).as_deref(), Some(r"C:\Users"));
    }

    #[test]
    fn resolve_cwd_remote_and_local() {
        assert_eq!(resolve_cwd("~", "/home/u", true).as_deref(), Some("/home/u"));
        assert_eq!(resolve_cwd("/home/u/src/", "", true).as_deref(), Some("/home/u/src"));
        assert_eq!(resolve_cwd("rel", "/h", true), None);
        assert_eq!(resolve_cwd("", "/h", false), None);
        assert_eq!(resolve_cwd("/tmp/", "", false).as_deref(), Some("/tmp"));
        assert_eq!(resolve_cwd("~/a", "/home/u", false).as_deref(), Some("/home/u/a"));
        assert_eq!(resolve_cwd("~/a", "", false), None);
        assert_eq!(resolve_cwd("rel", "/home/u", false), None);
        assert_eq!(resolve_cwd(r"C:\Users\a\", "", false).as_deref(), Some(r"C:\Users\a"));
    }

    #[test]
    fn follow_step_waits_for_terminal_move() {
        let mut f = Follow::new(true);
        f.last = resolve_cwd("/home/u", "", true);
        assert_eq!(follow_step(&mut f, Some("/home/u"), "", "/tmp", true), None);
        assert_eq!(follow_step(&mut f, Some("/var"), "", "/tmp", true).as_deref(), Some("/var"));
        assert_eq!(follow_step(&mut f, Some("/var/"), "", "/var", true), None);
        let mut w = Follow::new(true);
        assert_eq!(follow_step(&mut w, Some(r"C:\Users\a"), "", r"C:\", false).as_deref(), Some(r"C:\Users\a"));
        assert_eq!(follow_step(&mut w, Some(r"C:\Users\a\"), "", r"C:\Users\a", false), None);
    }
}
