//! User-managed host tree (PRD 5.23): nestable groups ("a/b/c" paths) and connections,
//! manual order, Finder-style color tags on hosts and groups, ~/.ssh/config import.
//! Pure functions over the config lists so they are unit-tested without a UI.
use serde::{Deserialize, Serialize};

use crate::hosts::HostEntry;
use crate::outline::normalize;

/// The seven Finder tag colors, in menu order.
pub const COLORS: [&str; 7] = ["red", "orange", "yellow", "green", "blue", "purple", "grey"];

/// "#rrggbb" / "rrggbb" → 0xrrggbb.
pub fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    (h.len() == 6).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

pub fn color_rgb(color: &str) -> u32 {
    match color { "red" => 0xff3b30, "orange" => 0xff9500, "yellow" => 0xffcc00, "green" => 0x34c759, "blue" => 0x007aff, "purple" => 0xaf52de, _ => 0x8e8e93 }
}

/// `[[groups]]` in config.toml. Order of the list is the sidebar order among siblings.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct GroupEntry {
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 自定义图标：文件夹字形 id（`iconset::FOLDER_GLYPHS`），缺省为纯文件夹。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 自定义图标颜色 id（`iconset::COLORS`），缺省为蓝色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_color: Option<String>,
}

pub fn parent_of(path: &str) -> &str { path.rsplit_once('/').map(|(p, _)| p).unwrap_or("") }
pub fn leaf_of(path: &str) -> &str { path.rsplit_once('/').map(|(_, l)| l).unwrap_or(path) }
pub fn join(parent: &str, name: &str) -> String { let n = normalize(name).replace('/', "-"); if parent.is_empty() { n } else { format!("{parent}/{n}") } }
/// `path` equals `root` or lies below it.
pub fn under(path: &str, root: &str) -> bool { path == root || path.strip_prefix(root).is_some_and(|r| r.starts_with('/')) }
fn host_group(h: &HostEntry) -> String { h.group.as_deref().map(normalize).unwrap_or_default() }

/// Make sure every group referenced by a host (and every ancestor) exists in `groups`.
pub fn ensure_groups(hosts: &[HostEntry], groups: &mut Vec<GroupEntry>) {
    let mut want: Vec<String> = groups.iter().map(|g| normalize(&g.path)).collect();
    for g in groups.iter_mut() { g.path = normalize(&g.path); }
    groups.retain(|g| !g.path.is_empty());
    want.retain(|p| !p.is_empty());
    let add = |p: String, groups: &mut Vec<GroupEntry>| {
        let mut acc = String::new();
        for part in p.split('/') {
            acc = if acc.is_empty() { part.to_string() } else { format!("{acc}/{part}") };
            if !groups.iter().any(|g| g.path == acc) { groups.push(GroupEntry { path: acc.clone(), ..Default::default() }); }
        }
    };
    for p in want { add(p, groups); }
    for h in hosts { let g = host_group(h); if !g.is_empty() { add(g, groups); } }
}

fn unique(base: &str, taken: &dyn Fn(&str) -> bool) -> String {
    if !taken(base) { return base.to_string(); }
    (2..).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap()
}

/// New group `name` under `parent`; returns its path (made unique among siblings).
pub fn add_group(groups: &mut Vec<GroupEntry>, parent: &str, name: &str) -> String {
    let base = join(parent, if name.trim().is_empty() { "New group" } else { name });
    let path = unique(&base, &|p| groups.iter().any(|g| g.path == p));
    groups.push(GroupEntry { path: path.clone(), ..Default::default() });
    path
}

/// Rename the last path component of `path`; rewrites subgroups and hosts. Returns the new path.
pub fn rename_group(hosts: &mut [HostEntry], groups: &mut [GroupEntry], path: &str, name: &str) -> Option<String> {
    let new = join(parent_of(path), name);
    if new.is_empty() || new == path { return Some(path.to_string()); }
    if groups.iter().any(|g| g.path == new) { return None; }
    Some(reparent(hosts, groups, path, &new))
}

fn reparent(hosts: &mut [HostEntry], groups: &mut [GroupEntry], old: &str, new: &str) -> String {
    let re = |p: &str| -> String { format!("{new}{}", &p[old.len()..]) };
    for g in groups.iter_mut() { if under(&g.path, old) { g.path = re(&g.path); } }
    for h in hosts.iter_mut() { let g = host_group(h); if under(&g, old) { h.group = Some(re(&g)); } }
    new.to_string()
}

/// Move group `path` (with its contents) into `new_parent` ("" = top level), placed before
/// sibling `before` if given, else last. Refuses to move a group into itself or a descendant.
pub fn move_group(hosts: &mut [HostEntry], groups: &mut Vec<GroupEntry>, path: &str, new_parent: &str, before: Option<&str>) -> Option<String> {
    if under(new_parent, path) { return None; }
    let new = if parent_of(path) == new_parent { path.to_string() } else {
        let base = join(new_parent, leaf_of(path));
        let n = unique(&base, &|p| groups.iter().any(|g| g.path == p && !under(p, path)));
        reparent(hosts, groups, path, &n)
    };
    // reorder: take the moved subtree out and put it before `before` (or at the end)
    let (moved, mut rest): (Vec<GroupEntry>, Vec<GroupEntry>) = groups.drain(..).partition(|g| under(&g.path, &new));
    let at = before.and_then(|b| rest.iter().position(|g| g.path == b)).unwrap_or(rest.len());
    rest.splice(at..at, moved);
    *groups = rest;
    Some(new)
}

/// Delete a group. `keep_children`: subgroups and hosts move up to the parent; otherwise they
/// are deleted too. Returns the names of deleted hosts.
pub fn delete_group(hosts: &mut Vec<HostEntry>, groups: &mut Vec<GroupEntry>, path: &str, keep_children: bool) -> Vec<String> {
    let parent = parent_of(path).to_string();
    let mut gone = vec![];
    if keep_children {
        let children: Vec<String> = groups.iter().filter(|g| parent_of(&g.path) == path).map(|g| g.path.clone()).collect();
        groups.retain(|g| g.path != path);
        for c in children { move_group(hosts, groups, &c, &parent, None); }
        for h in hosts.iter_mut() { if host_group(h) == path { h.group = (!parent.is_empty()).then(|| parent.clone()); } }
    } else {
        groups.retain(|g| !under(&g.path, path));
        hosts.retain(|h| if under(&host_group(h), path) { gone.push(h.name.clone()); false } else { true });
    }
    gone
}

/// Move host `name` into `group`, before host `before` (same group) or last in that group.
pub fn move_host(hosts: &mut Vec<HostEntry>, name: &str, group: &str, before: Option<&str>) -> bool {
    let Some(i) = hosts.iter().position(|h| h.name == name) else { return false };
    let mut h = hosts.remove(i);
    let g = normalize(group);
    h.group = (!g.is_empty()).then_some(g.clone());
    let at = before.and_then(|b| hosts.iter().position(|x| x.name == b))
        .or_else(|| hosts.iter().rposition(|x| host_group(x) == g).map(|p| p + 1))
        .unwrap_or(hosts.len());
    hosts.insert(at, h);
    true
}

/// Copy of host `name` right after it, named "<name> copy" (unique). Returns the new name.
pub fn duplicate_host(hosts: &mut Vec<HostEntry>, name: &str) -> Option<String> {
    let i = hosts.iter().position(|h| h.name == name)?;
    let mut h = hosts[i].clone();
    h.name = unique(&format!("{name} copy"), &|n| hosts.iter().any(|x| x.name == n));
    let n = h.name.clone();
    hosts.insert(i + 1, h);
    Some(n)
}

pub fn rename_host(hosts: &mut [HostEntry], name: &str, new: &str) -> bool {
    let new = new.trim();
    if new.is_empty() || hosts.iter().any(|h| h.name == new) { return new == name; }
    match hosts.iter_mut().find(|h| h.name == name) { Some(h) => { h.name = new.to_string(); true } None => false }
}

pub fn toggle(tags: &mut Vec<String>, color: &str) {
    if let Some(i) = tags.iter().position(|t| t == color) { tags.remove(i); } else { tags.push(color.to_string()); tags.sort_by_key(|t| COLORS.iter().position(|c| c == t).unwrap_or(COLORS.len())); }
}

/// Host is production: own tag, or any ancestor group carries the production tag (or legacy flag).
pub fn is_production(h: &HostEntry, groups: &[GroupEntry], prod_tag: &str) -> bool {
    if h.production || h.tags.iter().any(|t| t == prod_tag) { return true; }
    let g = host_group(h);
    !g.is_empty() && groups.iter().any(|x| under(&g, &x.path) && x.tags.iter().any(|t| t == prod_tag))
}

/// Legacy `production = true` hosts get the production tag (flag cleared). Returns how many.
pub fn migrate_production(hosts: &mut [HostEntry], prod_tag: &str) -> usize {
    let mut n = 0;
    for h in hosts.iter_mut().filter(|h| h.production) {
        if !h.tags.iter().any(|t| t == prod_tag) { toggle(&mut h.tags, prod_tag); }
        h.production = false;
        n += 1;
    }
    n
}

/// ~/.ssh/config hosts that are not in the list yet (by name).
pub fn importable(existing: &[HostEntry], ssh_config: &[HostEntry]) -> Vec<HostEntry> {
    ssh_config.iter().filter(|h| !existing.iter().any(|x| x.name == h.name)).cloned().collect()
}
/// Add the chosen ssh_config hosts to `group`. Returns the number added.
pub fn import(hosts: &mut Vec<HostEntry>, chosen: &[HostEntry], group: &str) -> usize {
    let g = normalize(group);
    let mut n = 0;
    for h in chosen {
        if hosts.iter().any(|x| x.name == h.name) { continue; }
        let mut h = h.clone();
        h.source = "config";
        h.group = (!g.is_empty()).then(|| g.clone());
        hosts.push(h);
        n += 1;
    }
    n
}

/// Search: plain words match name / address / user / group; `#name` or `#color` filter by tag
/// (host tag or inherited from a group). `tag_names`: (color, display name).
#[derive(Debug, Default, Clone)]
pub struct Query { pub words: Vec<String>, pub tags: Vec<String> }
pub fn parse_query(q: &str, tag_names: &[(String, String)]) -> Query {
    let mut out = Query::default();
    for w in q.split_whitespace() {
        match w.strip_prefix('#') {
            Some(t) if !t.is_empty() => {
                let t = t.to_lowercase();
                let color = tag_names.iter().find(|(c, n)| *c == t || n.to_lowercase() == t || n.to_lowercase().starts_with(&t)).map(|(c, _)| c.clone()).unwrap_or(t);
                out.tags.push(color);
            }
            _ => out.words.push(w.to_lowercase()),
        }
    }
    out
}
pub fn effective_tags(h: &HostEntry, groups: &[GroupEntry]) -> Vec<String> {
    let g = host_group(h);
    let mut v = h.tags.clone();
    for x in groups.iter().filter(|x| !g.is_empty() && under(&g, &x.path)) { for t in &x.tags { if !v.contains(t) { v.push(t.clone()); } } }
    v
}
pub fn host_matches(h: &HostEntry, groups: &[GroupEntry], q: &Query) -> bool {
    let hay = format!("{} {} {} {}", h.name, h.host, h.user.clone().unwrap_or_default(), host_group(h)).to_lowercase();
    let tags = effective_tags(h, groups);
    q.words.iter().all(|w| hay.contains(w)) && q.tags.iter().all(|t| tags.contains(t))
}

/// One visible sidebar row.
#[derive(Debug, Clone, PartialEq)]
pub enum Row { Group { path: String, name: String, depth: usize, count: usize, collapsed: bool }, Host { index: usize, depth: usize } }

/// Rows of the tree in display order: each group, then its subgroups, then its hosts; top-level
/// (ungrouped) hosts last. Collapsed groups hide their contents (not while searching). With a
/// query only matching hosts and their ancestor groups are shown.
pub fn rows(hosts: &[HostEntry], groups: &[GroupEntry], collapsed: &[String], q: &Query) -> Vec<Row> {
    let searching = !q.words.is_empty() || !q.tags.is_empty();
    let hit: Vec<bool> = hosts.iter().map(|h| !searching || host_matches(h, groups, q)).collect();
    let count = |p: &str| hosts.iter().zip(&hit).filter(|(h, ok)| **ok && under(&host_group(h), p)).count();
    let mut out = vec![];
    #[allow(clippy::too_many_arguments)]
    fn walk(parent: &str, depth: usize, hosts: &[HostEntry], groups: &[GroupEntry], collapsed: &[String], hit: &[bool], searching: bool, count: &dyn Fn(&str) -> usize, out: &mut Vec<Row>) {
        for g in groups.iter().filter(|g| parent_of(&g.path) == parent) {
            let n = count(&g.path);
            if searching && n == 0 { continue; }
            let col = !searching && collapsed.iter().any(|c| c == &g.path);
            out.push(Row::Group { path: g.path.clone(), name: leaf_of(&g.path).to_string(), depth, count: n, collapsed: col });
            if col { continue; }
            walk(&g.path, depth + 1, hosts, groups, collapsed, hit, searching, count, out);
            for (i, h) in hosts.iter().enumerate() { if hit[i] && host_group(h) == g.path { out.push(Row::Host { index: i, depth: depth + 1 }); } }
        }
    }
    walk("", 0, hosts, groups, collapsed, &hit, searching, &count, &mut out);
    for (i, h) in hosts.iter().enumerate() { if hit[i] && host_group(h).is_empty() { out.push(Row::Host { index: i, depth: 0 }); } }
    out
}

/// All host indices under group `path` (recursive), in display order — for "connect all".
pub fn hosts_under(hosts: &[HostEntry], path: &str) -> Vec<usize> {
    hosts.iter().enumerate().filter(|(_, h)| under(&host_group(h), path)).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(name: &str, g: &str) -> HostEntry { HostEntry { name: name.into(), host: format!("{name}.x"), group: (!g.is_empty()).then(|| g.into()), source: "config", ..Default::default() } }
    fn names(hosts: &[HostEntry], rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| match r { Row::Group { path, depth, .. } => format!("{}[{path}]", " ".repeat(*depth)), Row::Host { index, depth } => format!("{}{}", " ".repeat(*depth), hosts[*index].name) }).collect()
    }
    #[test]
    fn tree_rows_order_collapse_and_search() {
        let hosts = vec![h("web1", "prod"), h("db1", "prod/db"), h("dev", ""), h("web2", "prod")];
        let mut groups = vec![GroupEntry { path: "lab".into(), ..Default::default() }];
        ensure_groups(&hosts, &mut groups);
        assert_eq!(groups.iter().map(|g| g.path.as_str()).collect::<Vec<_>>(), ["lab", "prod", "prod/db"]);
        let r = rows(&hosts, &groups, &[], &Query::default());
        assert_eq!(names(&hosts, &r), ["[lab]", "[prod]", " [prod/db]", "  db1", " web1", " web2", "dev"]);
        let r = rows(&hosts, &groups, &["prod".into()], &Query::default());
        assert_eq!(names(&hosts, &r), ["[lab]", "[prod]", "dev"]);
        let q = parse_query("db", &[]);
        let r = rows(&hosts, &groups, &["prod".into()], &q);
        assert_eq!(names(&hosts, &r), ["[prod]", " [prod/db]", "  db1"]);
        assert!(matches!(r[0], Row::Group { count: 1, collapsed: false, .. }));
    }
    #[test]
    fn group_ops() {
        let mut hosts = vec![h("a", "g1"), h("b", "g1/sub"), h("c", "")];
        let mut groups = vec![];
        ensure_groups(&hosts, &mut groups);
        assert_eq!(add_group(&mut groups, "g1", "sub"), "g1/sub 2");
        assert_eq!(rename_group(&mut hosts, &mut groups, "g1", "Prod").as_deref(), Some("Prod"));
        assert_eq!(hosts[1].group.as_deref(), Some("Prod/sub"));
        assert!(groups.iter().any(|g| g.path == "Prod/sub 2"));
        // no cycles
        assert_eq!(move_group(&mut hosts, &mut groups, "Prod", "Prod/sub", None), None);
        let top = add_group(&mut groups, "", "Lab");
        assert_eq!(move_group(&mut hosts, &mut groups, "Prod/sub", &top, None).as_deref(), Some("Lab/sub"));
        assert_eq!(hosts[1].group.as_deref(), Some("Lab/sub"));
        // reorder top level: Lab before Prod
        move_group(&mut hosts, &mut groups, "Lab", "", Some("Prod"));
        assert_eq!(groups[0].path, "Lab");
        // delete keeping children -> moved up
        let gone = delete_group(&mut hosts, &mut groups, "Lab", true);
        assert!(gone.is_empty());
        assert_eq!(hosts[1].group.as_deref(), Some("sub"));
        assert!(groups.iter().any(|g| g.path == "sub"));
        // delete with children
        let gone = delete_group(&mut hosts, &mut groups, "Prod", false);
        assert_eq!(gone, ["a"]);
        assert!(!groups.iter().any(|g| under(&g.path, "Prod")));
    }
    #[test]
    fn host_ops() {
        let mut hosts = vec![h("a", "g"), h("b", "g"), h("c", "")];
        assert!(move_host(&mut hosts, "c", "g", Some("a")));
        assert_eq!(hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["c", "a", "b"]);
        assert!(move_host(&mut hosts, "c", "", None));
        assert_eq!(hosts[2].group, None);
        assert_eq!(duplicate_host(&mut hosts, "a").as_deref(), Some("a copy"));
        assert_eq!(duplicate_host(&mut hosts, "a").as_deref(), Some("a copy 2"));
        assert_eq!(hosts[1].name, "a copy 2");
        assert!(!rename_host(&mut hosts, "a", "b"));
        assert!(rename_host(&mut hosts, "a", "alpha"));
    }
    #[test]
    fn tags_production_and_filter() {
        let mut hosts = vec![h("a", "g"), h("b", ""), HostEntry { production: true, ..h("legacy", "") }];
        let mut groups = vec![GroupEntry { path: "g".into(), tags: vec!["red".into()], ..Default::default() }];
        assert_eq!(migrate_production(&mut hosts, "red"), 1);
        assert_eq!((hosts[2].production, hosts[2].tags.as_slice()), (false, &["red".to_string()][..]));
        assert!(is_production(&hosts[0], &groups, "red"));
        assert!(!is_production(&hosts[1], &groups, "red"));
        toggle(&mut hosts[1].tags, "blue");
        toggle(&mut hosts[1].tags, "red");
        assert_eq!(hosts[1].tags, ["red", "blue"]);
        toggle(&mut groups[0].tags, "red");
        assert!(!is_production(&hosts[0], &groups, "red"));
        let names = vec![("red".to_string(), "生产".to_string()), ("blue".to_string(), "Blue".to_string())];
        let q = parse_query("#生产", &names);
        assert_eq!(q.tags, ["red"]);
        assert!(host_matches(&hosts[1], &groups, &q) && !host_matches(&hosts[0], &groups, &q));
        assert_eq!(parse_query("#blu web", &names).tags, ["blue"]);
    }
    #[test]
    fn import_from_ssh_config() {
        let mut hosts = vec![h("a", "")];
        let sc = vec![HostEntry { source: "ssh_config", ..h("a", "") }, HostEntry { source: "ssh_config", ..h("gh", "") }];
        let cand = importable(&hosts, &sc);
        assert_eq!(cand.len(), 1);
        assert_eq!(import(&mut hosts, &cand, "Imported"), 1);
        assert_eq!((hosts[1].source, hosts[1].group.as_deref()), ("config", Some("Imported")));
        assert_eq!(hosts_under(&hosts, "Imported"), [1]);
    }
}
