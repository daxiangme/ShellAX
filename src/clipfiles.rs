//! 读取系统剪贴板中的文件/文件夹（资源管理器 / Finder 复制），并格式化为可粘贴到终端的路径文本。

use std::path::PathBuf;

/// 剪贴板中复制的文件路径；没有文件时返回 None。
pub fn read() -> Option<Vec<PathBuf>> {
    let v = imp::read()?;
    if v.is_empty() { None } else { Some(v) }
}

/// 多个路径以空格分隔；`windows_shell` 为 true 时使用双引号，否则使用 POSIX 单引号。
pub fn format(paths: &[PathBuf], windows_shell: bool) -> String {
    paths.iter().map(|p| quote(&p.to_string_lossy(), windows_shell)).collect::<Vec<_>>().join(" ")
}

pub fn quote(s: &str, windows_shell: bool) -> String {
    if windows_shell {
        let special = |c: char| c.is_whitespace() || "&()[]{}^=;!'+,`~$@#%".contains(c);
        if s.chars().any(special) { format!("\"{s}\"") } else { s.to_string() }
    } else {
        let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+,:@%=".contains(c) || (!c.is_ascii() && !c.is_whitespace());
        if !s.is_empty() && s.chars().all(safe) { s.to_string() } else { format!("'{}'", s.replace('\'', r"'\''")) }
    }
}

/// file:// URL 转本地路径（含百分号解码）。
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]);
        i += 1;
    }
    let mut s = String::from_utf8(out).ok()?;
    if s.len() > 1 && s.ends_with('/') { s.pop(); }
    Some(PathBuf::from(s))
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::DataExchange::{OpenClipboard, CloseClipboard, GetClipboardData, IsClipboardFormatAvailable};
    use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};
    const CF_HDROP: u32 = 15;

    pub fn read() -> Option<Vec<PathBuf>> {
        unsafe {
            if IsClipboardFormatAvailable(CF_HDROP) == 0 { return None; }
            if OpenClipboard(std::ptr::null_mut()) == 0 { return None; }
            let h = GetClipboardData(CF_HDROP);
            let mut out = Vec::new();
            if !h.is_null() {
                let drop = h as HDROP;
                let n = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
                for i in 0..n {
                    let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0);
                    let mut buf = vec![0u16; len as usize + 1];
                    let got = DragQueryFileW(drop, i, buf.as_mut_ptr(), buf.len() as u32);
                    buf.truncate(got as usize);
                    out.push(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
                }
            }
            CloseClipboard();
            Some(out)
        }
    }
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
mod imp {
    use std::path::PathBuf;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    unsafe fn nsstring(s: &str) -> *mut Object {
        let c = std::ffi::CString::new(s).unwrap();
        msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()]
    }
    unsafe fn to_string(o: *mut Object) -> Option<String> {
        if o.is_null() { return None; }
        let p: *const std::os::raw::c_char = msg_send![o, UTF8String];
        if p.is_null() { return None; }
        Some(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
    }

    pub fn read() -> Option<Vec<PathBuf>> {
        unsafe {
            let pb: *mut Object = msg_send![class!(NSPasteboard), generalPasteboard];
            if pb.is_null() { return None; }
            let items: *mut Object = msg_send![pb, pasteboardItems];
            if items.is_null() { return None; }
            let n: usize = msg_send![items, count];
            let ty = nsstring("public.file-url");
            let mut out = Vec::new();
            for i in 0..n {
                let item: *mut Object = msg_send![items, objectAtIndex: i];
                let s: *mut Object = msg_send![item, stringForType: ty];
                if let Some(p) = to_string(s).and_then(|u| super::file_url_to_path(&u)) { out.push(p); }
            }
            if out.is_empty() {
                // 旧式类型：NSFilenamesPboardType，内容为路径字符串数组
                let arr: *mut Object = msg_send![pb, propertyListForType: nsstring("NSFilenamesPboardType")];
                if !arr.is_null() {
                    let is_arr: bool = msg_send![arr, isKindOfClass: class!(NSArray)];
                    if is_arr {
                        let n: usize = msg_send![arr, count];
                        for i in 0..n {
                            let s: *mut Object = msg_send![arr, objectAtIndex: i];
                            if let Some(p) = to_string(s) { out.push(PathBuf::from(p)); }
                        }
                    }
                }
            }
            Some(out)
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    pub fn read() -> Option<Vec<std::path::PathBuf>> { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoting() {
        assert_eq!(quote("/a/b", false), "/a/b");
        assert_eq!(quote("/a b/it's", false), r"'/a b/it'\''s'");
        assert_eq!(quote(r"C:\a b", true), r#""C:\a b""#);
        assert_eq!(quote(r"C:\ab", true), r"C:\ab");
        assert_eq!(format(&[PathBuf::from("/x"), PathBuf::from("/y z")], false), "/x '/y z'");
        assert_eq!(file_url_to_path("file:///Users/a/My%20Dir/").unwrap(), PathBuf::from("/Users/a/My Dir"));
    }
}
