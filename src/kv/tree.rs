//! Key namespace tree: keys split by a separator into folders with key counts. Built from
//! the keys loaded so far (SCAN batches), so counts are of loaded keys.
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Node {
    /// Child folders by segment.
    pub dirs: BTreeMap<String, Node>,
    /// Keys ending at this level: (leaf segment, full key).
    pub keys: Vec<(String, String)>,
    /// Keys under this node, recursively.
    pub count: usize,
}

impl Node {
    pub fn insert(&mut self, key: &str, sep: &str) {
        self.count += 1;
        if sep.is_empty() { self.keys.push((key.to_string(), key.to_string())); return; }
        let mut node = self;
        let parts: Vec<&str> = key.split(sep).collect();
        for p in &parts[..parts.len() - 1] {
            node = node.dirs.entry(p.to_string()).or_default();
            node.count += 1;
        }
        node.keys.push((parts[parts.len() - 1].to_string(), key.to_string()));
    }
    pub fn build<'a>(keys: impl IntoIterator<Item = &'a str>, sep: &str) -> Node {
        let mut n = Node::default();
        for k in keys { n.insert(k, sep); }
        n.sort();
        n
    }
    fn sort(&mut self) {
        self.keys.sort();
        for d in self.dirs.values_mut() { d.sort(); }
    }
}

/// One visible row of the tree with `open` folders (paths joined by `sep`, ending in `sep`).
#[derive(Debug, Clone, PartialEq)]
pub enum Row { Dir { path: String, name: String, depth: usize, count: usize, open: bool }, Key { key: String, name: String, depth: usize } }

pub fn rows(root: &Node, sep: &str, open: &std::collections::HashSet<String>) -> Vec<Row> {
    let mut out = vec![];
    walk(root, "", sep, 0, open, &mut out);
    out
}

fn walk(n: &Node, prefix: &str, sep: &str, depth: usize, open: &std::collections::HashSet<String>, out: &mut Vec<Row>) {
    for (name, d) in &n.dirs {
        let path = format!("{prefix}{name}{sep}");
        let is_open = open.contains(&path);
        out.push(Row::Dir { path: path.clone(), name: name.clone(), depth, count: d.count, open: is_open });
        if is_open { walk(d, &path, sep, depth + 1, open, out); }
    }
    for (name, key) in &n.keys { out.push(Row::Key { key: key.clone(), name: name.clone(), depth }); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree() {
        let n = Node::build(["user:1:profile", "user:1:cart", "user:2:profile", "plain", "a::b"], ":");
        assert_eq!(n.count, 5);
        assert_eq!(n.dirs["user"].count, 3);
        assert_eq!(n.dirs["user"].dirs["1"].keys, vec![("cart".to_string(), "user:1:cart".to_string()), ("profile".into(), "user:1:profile".into())]);
        assert_eq!(n.keys, vec![("plain".to_string(), "plain".to_string())]);
        assert_eq!(n.dirs["a"].dirs[""].keys[0].1, "a::b");
        let mut open = std::collections::HashSet::new();
        assert_eq!(rows(&n, ":", &open).len(), 3);
        open.insert("user:".to_string());
        let r = rows(&n, ":", &open);
        assert_eq!(r.len(), 5);
        assert!(matches!(&r[2], Row::Dir { path, depth: 1, count: 2, .. } if path == "user:1:"));
    }
}
