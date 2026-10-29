//! SFTP directory sync planning (own code): compare two recursive listings and produce a
//! previewable plan. One-way (local→remote or remote→local) or two-way (newer wins).
//! Deletions are only planned when `delete` is set and are flagged so the UI asks first.
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct FileInfo { pub size: u64, pub mtime: u64, pub is_dir: bool }
/// Relative path ("a/b.txt") → info.
pub type Tree = BTreeMap<String, FileInfo>;
/// Safety cap for one sync scan (each side).
pub const MAX_ENTRIES: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { LocalToRemote, RemoteToLocal, TwoWay }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op { Upload(String), Download(String), DeleteRemote(String), DeleteLocal(String), MkdirRemote(String), MkdirLocal(String), Conflict(String) }
impl Op {
    pub fn path(&self) -> &str { match self { Op::Upload(p) | Op::Download(p) | Op::DeleteRemote(p) | Op::DeleteLocal(p) | Op::MkdirRemote(p) | Op::MkdirLocal(p) | Op::Conflict(p) => p } }
    pub fn is_delete(&self) -> bool { matches!(self, Op::DeleteRemote(_) | Op::DeleteLocal(_)) }
    /// Short symbol for the preview list.
    pub fn sign(&self) -> &'static str { match self { Op::Upload(_) => "↑", Op::Download(_) => "↓", Op::DeleteRemote(_) | Op::DeleteLocal(_) => "✕", Op::MkdirRemote(_) | Op::MkdirLocal(_) => "+", Op::Conflict(_) => "!" } }
}

fn differs(a: &FileInfo, b: &FileInfo) -> bool { !a.is_dir && (a.size != b.size || a.mtime.abs_diff(b.mtime) > 2) }

/// Build the plan. Directories are created before their files; deletes come last, deepest first.
pub fn plan(local: &Tree, remote: &Tree, mode: Mode, delete: bool) -> Vec<Op> {
    let mut out = vec![]; let mut dels = vec![];
    let push_copy = |out: &mut Vec<Op>, p: &str, i: &FileInfo, up: bool| out.push(match (i.is_dir, up) { (true, true) => Op::MkdirRemote(p.into()), (true, false) => Op::MkdirLocal(p.into()), (false, true) => Op::Upload(p.into()), (false, false) => Op::Download(p.into()) });
    for (p, l) in local {
        match remote.get(p) {
            None => match mode {
                Mode::LocalToRemote | Mode::TwoWay => push_copy(&mut out, p, l, true),
                Mode::RemoteToLocal => if delete { dels.push(Op::DeleteLocal(p.clone())) },
            },
            Some(r) if l.is_dir != r.is_dir => out.push(Op::Conflict(p.clone())),
            Some(r) if differs(l, r) => match mode {
                Mode::LocalToRemote => out.push(Op::Upload(p.clone())),
                Mode::RemoteToLocal => out.push(Op::Download(p.clone())),
                Mode::TwoWay => out.push(if l.mtime.abs_diff(r.mtime) <= 2 { Op::Conflict(p.clone()) } else if l.mtime > r.mtime { Op::Upload(p.clone()) } else { Op::Download(p.clone()) }),
            },
            _ => {}
        }
    }
    for (p, r) in remote.iter().filter(|(p, _)| !local.contains_key(*p)) {
        match mode {
            Mode::RemoteToLocal | Mode::TwoWay => push_copy(&mut out, p, r, false),
            Mode::LocalToRemote => if delete { dels.push(Op::DeleteRemote(p.clone())) },
        }
    }
    out.sort_by_key(|o| (!matches!(o, Op::MkdirLocal(_) | Op::MkdirRemote(_)), o.path().matches('/').count(), o.path().to_string()));
    dels.sort_by_key(|o| std::cmp::Reverse(o.path().matches('/').count()));
    out.extend(dels);
    out
}

/// Recursive local listing (relative paths, '/' separators). Symlinks are not followed.
pub fn scan_local(root: &std::path::Path) -> std::io::Result<Tree> {
    let mut t = Tree::new(); let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?; let md = std::fs::symlink_metadata(e.path())?; if md.file_type().is_symlink() { continue; }
            let name = e.file_name().to_string_lossy().to_string();
            let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let mtime = md.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            t.insert(r.clone(), FileInfo { size: if md.is_dir() { 0 } else { md.len() }, mtime, is_dir: md.is_dir() });
            if t.len() > MAX_ENTRIES { return Err(std::io::Error::other(format!("more than {MAX_ENTRIES} entries under {}; pick a smaller directory", root.display()))); }
            if md.is_dir() { stack.push((e.path(), r)); }
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn f(size: u64, mtime: u64) -> FileInfo { FileInfo { size, mtime, is_dir: false } }
    fn d() -> FileInfo { FileInfo { size: 0, mtime: 0, is_dir: true } }
    #[test]
    fn plans() {
        let l: Tree = [("a.txt".into(), f(1, 100)), ("dir".into(), d()), ("dir/n.txt".into(), f(3, 100)), ("same".into(), f(5, 50)), ("new".into(), f(1, 900))].into();
        let r: Tree = [("a.txt".into(), f(2, 200)), ("old".into(), f(1, 1)), ("same".into(), f(5, 51)), ("new".into(), f(2, 100))].into();
        let p = plan(&l, &r, Mode::LocalToRemote, false);
        assert_eq!(p[0], Op::MkdirRemote("dir".into()));
        assert!(p.contains(&Op::Upload("a.txt".into())) && p.contains(&Op::Upload("dir/n.txt".into())) && !p.iter().any(|o| o.is_delete()));
        let p = plan(&l, &r, Mode::LocalToRemote, true);
        assert_eq!(p.last(), Some(&Op::DeleteRemote("old".into())));
        let p = plan(&l, &r, Mode::TwoWay, true);
        assert!(p.contains(&Op::Download("a.txt".into())) && p.contains(&Op::Upload("new".into())) && p.contains(&Op::Download("old".into())));
        assert!(!p.iter().any(|o| o.is_delete() || o.path() == "same"));
        let p = plan(&l, &r, Mode::RemoteToLocal, true);
        assert!(p.contains(&Op::DeleteLocal("dir/n.txt".into())) && p.contains(&Op::Download("a.txt".into())));
        let i = p.iter().position(|o| o == &Op::DeleteLocal("dir/n.txt".into())).unwrap();
        let j = p.iter().position(|o| o == &Op::DeleteLocal("dir".into())).unwrap();
        assert!(i < j, "deepest deleted first");
    }
}
