//! Split-pane layout tree for one tab.
//!
//! `auto_direction` (marked [3p]): Copyright (c) 2020 Zellij contributors, MIT License; modified.
//! See THIRD_PARTY_NOTICES.md.

/// Horizontal = new pane below (stacked top/bottom), Vertical = new pane to the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir { Horizontal, Vertical }

/// [3p] terminal cells are roughly 4x taller than wide .
pub const DEFAULT_CURSOR_HEIGHT_WIDTH_RATIO: usize = 4;

/// [3p] split along the longer visual side; None when the pane is too small to split.
pub fn auto_direction(rows: usize, cols: usize) -> Option<Dir> {
    if rows * DEFAULT_CURSOR_HEIGHT_WIDTH_RATIO > cols && rows > 10 * 2 {
        Some(Dir::Horizontal)
    } else if cols > 30 * 2 {
        Some(Dir::Vertical)
    } else {
        None
    }
}

#[derive(Clone, Debug)]
pub enum Node<T> {
    Leaf(T),
    /// `ratio` = share of the first child (0.1–0.9), changed by dragging the divider.
    Split { dir: Dir, ratio: f32, a: Box<Node<T>>, b: Box<Node<T>> },
}

impl<T: Clone + PartialEq> Node<T> {
    pub fn leaves(&self) -> Vec<T> {
        match self { Node::Leaf(t) => vec![t.clone()], Node::Split { a, b, .. } => { let mut v = a.leaves(); v.extend(b.leaves()); v } }
    }
    /// Replace leaf `target` with a split of (target, new). Returns false if not found.
    pub fn split(&mut self, target: &T, new: T, dir: Dir) -> bool {
        match self {
            Node::Leaf(t) if t == target => {
                let old = Node::Leaf(t.clone());
                *self = Node::Split { dir, ratio: 0.5, a: Box::new(old), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, new.clone(), dir) || b.split(target, new, dir),
        }
    }
    /// Replace leaf `target` with a split of (target, `sub`); `before` puts `sub` first (left/top).
    /// Used when a whole tab is dragged onto an edge of a pane.
    pub fn insert_at(&mut self, target: &T, sub: Node<T>, dir: Dir, before: bool) -> bool {
        match self {
            Node::Leaf(t) if t == target => {
                let old = Node::Leaf(t.clone());
                let (a, b) = if before { (sub, old) } else { (old, sub) };
                *self = Node::Split { dir, ratio: 0.5, a: Box::new(a), b: Box::new(b) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => if a.leaves().contains(target) { a.insert_at(target, sub, dir, before) } else { b.insert_at(target, sub, dir, before) },
        }
    }
    /// Set the ratio of the split at `path` (false = first child, true = second). Clamped.
    pub fn set_ratio(&mut self, path: &[bool], r: f32) -> bool {
        match (self, path.split_first()) {
            (Node::Split { ratio, .. }, None) => { *ratio = r.clamp(0.1, 0.9); true }
            (Node::Split { a, b, .. }, Some((second, rest))) => if *second { b.set_ratio(rest, r) } else { a.set_ratio(rest, r) },
            _ => false,
        }
    }
    /// Swap leaf `target` for `new` in place (e.g. retrying a failed connection). Returns false if not found.
    pub fn replace(&mut self, target: &T, new: T) -> bool {
        match self {
            Node::Leaf(t) if t == target => { *t = new; true }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.replace(target, new.clone()) || b.replace(target, new),
        }
    }
    /// Remove leaf `target`; its sibling takes the parent's place. The last leaf can't be removed.
    pub fn remove(&mut self, target: &T) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                if matches!(&**a, Node::Leaf(t) if t == target) { *self = (**b).clone(); return true; }
                if matches!(&**b, Node::Leaf(t) if t == target) { *self = (**a).clone(); return true; }
                a.remove(target) || b.remove(target)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direction_rule() {
        assert_eq!(auto_direction(40, 200), Some(Dir::Vertical));
        assert_eq!(auto_direction(50, 100), Some(Dir::Horizontal));
        assert_eq!(auto_direction(15, 50), None);
    }
    #[test]
    fn insert_subtree_at_edges() {
        let mut n = Node::Leaf(1);
        let mut sub = Node::Leaf(5); sub.split(&5, 6, Dir::Horizontal);
        assert!(n.insert_at(&1, sub, Dir::Vertical, true));
        assert_eq!(n.leaves(), vec![5, 6, 1]);
        assert!(n.insert_at(&6, Node::Leaf(7), Dir::Horizontal, false));
        assert_eq!(n.leaves(), vec![5, 6, 7, 1]);
        assert!(!n.insert_at(&9, Node::Leaf(8), Dir::Vertical, false));
    }
    #[test]
    fn split_and_remove() {
        let mut n = Node::Leaf(1);
        assert!(n.split(&1, 2, Dir::Vertical));
        assert!(n.split(&2, 3, Dir::Horizontal));
        assert_eq!(n.leaves(), vec![1, 2, 3]);
        assert!(n.remove(&2));
        assert_eq!(n.leaves(), vec![1, 3]);
        assert!(n.remove(&1));
        assert_eq!(n.leaves(), vec![3]);
        assert!(!n.remove(&3));
        let mut m = Node::Leaf(1);
        m.split(&1, 2, Dir::Vertical);
        m.split(&2, 3, Dir::Horizontal);
        assert!(m.set_ratio(&[true], 0.95));
        assert!(matches!(&m, Node::Split { b, .. } if matches!(**b, Node::Split { ratio, .. } if ratio == 0.9)));
        assert!(!m.set_ratio(&[false], 0.3));
    }
}
