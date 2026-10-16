//! How a file or folder should look in a file list: icon, theme color, and whether
//! it gets a tinted chip. Colors come from the active terminal palette.
use crate::ui::IconName;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub icon: IconName,
    pub color: u32,
    pub chip: bool,
}

pub fn glyph(name: &str, is_dir: bool, mode: u32, ansi: &[u32; 16], dark: bool, muted: u32) -> Glyph {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    if is_dir {
        return dir_glyph(&lower, ansi, dark, muted);
    }
    let kind = file_kind(&lower, mode);
    let (icon, idx, chip) = match kind {
        Kind::Image => (IconName::FileImage, 5, true),
        Kind::Video => (IconName::FilePlay, 5, true),
        Kind::Audio => (IconName::FileMusic, 6, true),
        Kind::Archive => (IconName::FileArchive, 1, true),
        Kind::Code => (IconName::FileCode, 4, true),
        Kind::Data => (IconName::FileBraces, 6, true),
        Kind::Sheet => (IconName::FileSpreadsheet, 2, true),
        Kind::Doc => (IconName::FileText, 4, true),
        Kind::Config => (IconName::FileCog, 3, true),
        Kind::Secret => (IconName::FileKey, 1, true),
        Kind::Exec => (IconName::FileTerminal, 2, true),
        Kind::Plain => (IconName::FileText, 7, false),
    };
    let color = if kind == Kind::Plain { muted } else { tone(ansi, dark, idx) };
    Glyph { icon, color, chip }
}

fn dir_glyph(lower: &str, ansi: &[u32; 16], dark: bool, muted: u32) -> Glyph {
    if lower == ".git" || lower == ".github" || lower == ".svn" || lower == ".hg" {
        return Glyph { icon: IconName::FolderGit, color: tone(ansi, dark, 4), chip: true };
    }
    if matches!(lower, "node_modules" | "target" | "dist" | "build" | "out" | "__pycache__" | ".cache" | "vendor") {
        return Glyph { icon: IconName::Folder, color: muted, chip: false };
    }
    Glyph { icon: IconName::Folder, color: tone(ansi, dark, 3), chip: true }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind { Image, Video, Audio, Archive, Code, Data, Sheet, Doc, Config, Secret, Exec, Plain }

fn file_kind(lower: &str, mode: u32) -> Kind {
    if is_secret_name(lower) { return Kind::Secret; }
    if lower.ends_with(".tar.gz") || lower.ends_with(".tar.bz2") || lower.ends_with(".tar.xz") || lower.ends_with(".tar.zst") {
        return Kind::Archive;
    }
    if matches!(lower, "dockerfile" | "makefile" | "jenkinsfile" | "vagrantfile" | "gemfile" | "rakefile") {
        return Kind::Code;
    }
    if matches!(lower, "license" | "readme" | "changelog" | "authors" | "copying") || lower.starts_with("readme.") || lower.starts_with("license.") {
        return Kind::Doc;
    }
    let ext = lower.rsplit_once('.').and_then(|(stem, ext)| (!stem.is_empty()).then_some(ext)).unwrap_or("");
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "avif" | "heic" | "tif" | "tiff" => Kind::Image,
        "mp4" | "mkv" | "mov" | "webm" | "avi" | "m4v" => Kind::Video,
        "mp3" | "wav" | "flac" | "ogg" | "aac" | "m4a" => Kind::Audio,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => Kind::Archive,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "cs" | "rb" | "php" | "swift" | "kt" | "kts" | "lua" | "sql" | "vue" | "css" | "scss" | "html" | "htm" | "xml" | "sh" | "bash" | "zsh" | "ps1" | "bat" | "cmd" => Kind::Code,
        "json" | "jsonc" | "yaml" | "yml" => Kind::Data,
        "csv" | "tsv" | "xls" | "xlsx" | "ods" => Kind::Sheet,
        "md" | "txt" | "pdf" | "doc" | "docx" | "rtf" | "rst" => Kind::Doc,
        "ini" | "conf" | "cfg" | "toml" | "properties" => Kind::Config,
        "pem" | "key" | "p12" | "pfx" | "crt" | "cer" => Kind::Secret,
        "exe" | "msi" | "dll" | "so" | "dylib" | "bin" | "appimage" => Kind::Exec,
        _ if mode & 0o111 != 0 && ext.is_empty() => Kind::Exec,
        _ => Kind::Plain,
    }
}

fn is_secret_name(lower: &str) -> bool {
    lower == ".env" || lower.starts_with(".env.") || lower.starts_with("id_rsa") || lower.starts_with("id_ed25519") || lower.starts_with("id_ecdsa")
}

fn tone(ansi: &[u32; 16], dark: bool, idx: usize) -> u32 {
    ansi[idx.min(7) + if dark { 8 } else { 0 }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ansi() -> [u32; 16] {
        let mut a = [0u32; 16];
        for i in 0..16 { a[i] = i as u32; }
        a
    }
    fn g(name: &str, dir: bool, mode: u32) -> Glyph { glyph(name, dir, mode, &ansi(), true, 99) }

    #[test]
    fn folders_and_types() {
        assert_eq!(g("src", true, 0).icon, IconName::Folder);
        assert!(g("src", true, 0).chip);
        assert_eq!(g("src", true, 0).color, 3 + 8);
        assert_eq!(g(".git", true, 0).icon, IconName::FolderGit);
        assert!(!g("node_modules", true, 0).chip);
        assert_eq!(g("photo.PNG", false, 0).icon, IconName::FileImage);
        assert_eq!(g("main.rs", false, 0).icon, IconName::FileCode);
        assert_eq!(g("app.tar.gz", false, 0).icon, IconName::FileArchive);
        assert_eq!(g("notes.pdf", false, 0).icon, IconName::FileText);
        assert_eq!(g("data.json", false, 0).icon, IconName::FileBraces);
        assert_eq!(g("Cargo.toml", false, 0).icon, IconName::FileCog);
        assert_eq!(g(".env", false, 0).icon, IconName::FileKey);
        assert_eq!(g("id_ed25519", false, 0).icon, IconName::FileKey);
        assert_eq!(g("tool", false, 0o755).icon, IconName::FileTerminal);
        assert_eq!(g("weird.xyz", false, 0).icon, IconName::FileText);
        assert!(!g("weird.xyz", false, 0).chip);
        assert_eq!(g("weird.xyz", false, 0).color, 99);
        assert_eq!(g("readme.md", false, 0).icon, IconName::FileText);
        assert!(g("readme.md", false, 0).chip);
    }
}
