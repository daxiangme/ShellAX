//! Split-pane layout tree for one tab.
//!
//! PORTED CODE NOTICE
//! `auto_direction` is derived from zellij `zellij-server/src/panes/tiled_panes/tiled_pane_grid.rs`
//! (`TiledPaneGrid::split_pane` and `DEFAULT_CURSOR_HEIGHT_WIDTH_RATIO`),
//! https://github.com/zellij-org/zellij (commit a6c3dba), Copyright (c) 2020 Zellij contributors, MIT License.
//! See THIRD_PARTY_NOTICES.md. Modifications (AgentShell, 2026-10-13): reduced to a pure function
//! over (rows, cols); minimum sizes taken from zellij's hard-coded 10*2 rows / 30*2 cols.
//! The binary tree itself (split/remove/leaves) is original AgentShell code.

/// Horizontal = new pane below (stacked top/bottom), Vertical = new pane to the right.
/// (zellij naming: the divider line is horizontal / vertical.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir { Horizontal, Vertical }

/// [zellij] terminal cells are roughly 4x taller than wide (in zellij's integer model).
pub const DEFAULT_CURSOR_HEIGHT_WIDTH_RATIO: usize = 4;

/// [zellij] split along the longer visual side; None when the pane is too small to split.
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
    Split { dir: Dir, a: Box<Node<T>>, b: Box<Node<T>> },
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
                *self = Node::Split { dir, a: Box::new(old), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, new.clone(), dir) || b.split(target, new, dir),
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
    fn zellij_direction_rule() {
        assert_eq!(auto_direction(40, 200), Some(Dir::Vertical));
        assert_eq!(auto_direction(50, 100), Some(Dir::Horizontal));
        assert_eq!(auto_direction(15, 50), None);
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
    }
}
