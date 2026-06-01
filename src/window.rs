#![allow(dead_code)]

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Orientation of a window split. `Horizontal` lays children side-by-side
/// (vim's `:vsplit`); `Vertical` stacks them top-to-bottom (vim's `:split`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

impl SplitDir {
    fn as_ratatui(self) -> Direction {
        match self {
            SplitDir::Horizontal => Direction::Horizontal,
            SplitDir::Vertical => Direction::Vertical,
        }
    }
}

/// One node in the window tree. Leaves point at a buffer index; splits
/// divide their rect proportionally among children by integer weights.
#[derive(Debug, Clone)]
pub enum Window {
    Leaf { bufno: usize },
    Split {
        dir: SplitDir,
        children: Vec<(u16, Window)>,
    },
}

impl Window {
    pub fn leaf(bufno: usize) -> Self {
        Self::Leaf { bufno }
    }
}

/// Index path from root through Split children to a Leaf. Empty path = the
/// root is itself a leaf.
pub type LeafPath = Vec<usize>;

/// Concrete placement of one leaf after layout.
#[derive(Debug, Clone)]
pub struct LeafLayout {
    pub path: LeafPath,
    pub bufno: usize,
    pub area: Rect,
}

/// Editor window tree plus focus tracking. The minibuffer is *not* part of
/// this tree — it's rendered separately by the host renderer.
#[derive(Debug, Clone)]
pub struct WindowTree {
    root: Window,
    focused: LeafPath,
}

impl WindowTree {
    pub fn new(bufno: usize) -> Self {
        Self {
            root: Window::leaf(bufno),
            focused: Vec::new(),
        }
    }

    pub fn focused_path(&self) -> &LeafPath {
        &self.focused
    }

    pub fn focused_bufno(&self) -> usize {
        match self.node_at(&self.focused) {
            Some(Window::Leaf { bufno }) => *bufno,
            _ => panic!("focused path must point to a leaf"),
        }
    }

    /// Point the focused leaf at a different buffer.
    pub fn set_focused_bufno(&mut self, bufno: usize) {
        let path = self.focused.clone();
        if let Some(Window::Leaf { bufno: b }) = self.node_at_mut(&path) {
            *b = bufno;
        }
    }

    /// Lay the tree out into `area`. Returns one entry per leaf, in tree
    /// order (left-to-right, top-to-bottom).
    pub fn layout(&self, area: Rect) -> Vec<LeafLayout> {
        let mut out = Vec::new();
        Self::layout_node(&self.root, area, &mut Vec::new(), &mut out);
        out
    }

    fn layout_node(node: &Window, area: Rect, path: &mut LeafPath, out: &mut Vec<LeafLayout>) {
        match node {
            Window::Leaf { bufno } => out.push(LeafLayout {
                path: path.clone(),
                bufno: *bufno,
                area,
            }),
            Window::Split { dir, children } => {
                let total: u32 = children.iter().map(|(w, _)| *w as u32).sum::<u32>().max(1);
                let constraints: Vec<Constraint> = children
                    .iter()
                    .map(|(w, _)| Constraint::Ratio(*w as u32, total))
                    .collect();
                let rects = Layout::default()
                    .direction(dir.as_ratatui())
                    .constraints(constraints)
                    .split(area);
                for (i, ((_, child), rect)) in children.iter().zip(rects.iter()).enumerate() {
                    path.push(i);
                    Self::layout_node(child, *rect, path, out);
                    path.pop();
                }
            }
        }
    }

    fn node_at(&self, path: &[usize]) -> Option<&Window> {
        let mut node = &self.root;
        for &i in path {
            node = match node {
                Window::Split { children, .. } => &children.get(i)?.1,
                Window::Leaf { .. } => return None,
            };
        }
        Some(node)
    }

    fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut Window> {
        let mut node = &mut self.root;
        for &i in path {
            node = match node {
                Window::Split { children, .. } => &mut children.get_mut(i)?.1,
                Window::Leaf { .. } => return None,
            };
        }
        Some(node)
    }

    /// Replace the focused leaf with a split; the existing buffer stays as
    /// the first child, the new buffer is placed as the second child and
    /// gains focus.
    pub fn split(&mut self, dir: SplitDir, new_bufno: usize) {
        let current_bufno = self.focused_bufno();
        let path = self.focused.clone();
        if let Some(leaf) = self.node_at_mut(&path) {
            *leaf = Window::Split {
                dir,
                children: vec![
                    (1, Window::leaf(current_bufno)),
                    (1, Window::leaf(new_bufno)),
                ],
            };
        }
        self.focused.push(1);
    }

    /// Move focus to the next leaf in tree order; wraps around.
    pub fn focus_next(&mut self) {
        let leaves = self.layout(Rect::default());
        if leaves.is_empty() {
            return;
        }
        let cur = leaves
            .iter()
            .position(|l| l.path == self.focused)
            .unwrap_or(0);
        let next = (cur + 1) % leaves.len();
        self.focused = leaves[next].path.clone();
    }

    /// Close the focused window. The sibling absorbs the split's space and
    /// focus moves into it. No-op on a single-leaf tree.
    pub fn close_focused(&mut self) {
        if self.focused.is_empty() {
            return;
        }
        let mut path = self.focused.clone();
        let leaf_idx = path.pop().unwrap();
        let parent_path = path;

        let Some(parent) = self.node_at_mut(&parent_path) else {
            return;
        };
        let Window::Split { children, .. } = parent else {
            return;
        };
        if leaf_idx >= children.len() {
            return;
        }
        children.remove(leaf_idx);
        // Collapse one-child splits — the lone survivor replaces the split.
        if children.len() == 1 {
            let (_, only) = children.remove(0);
            *parent = only;
        }

        // Re-establish focus on the first leaf at or under parent_path.
        self.focused = parent_path;
        loop {
            match self.node_at(&self.focused) {
                Some(Window::Leaf { .. }) | None => break,
                Some(Window::Split { .. }) => self.focused.push(0),
            }
        }
    }

    /// Visit every leaf's bufno in-place. Use after the buffer Vec has been
    /// mutated (e.g. a removal shifts subsequent indices down by one).
    pub fn for_each_leaf_mut(&mut self, mut f: impl FnMut(&mut usize)) {
        Self::walk_mut(&mut self.root, &mut f);
    }

    fn walk_mut(node: &mut Window, f: &mut impl FnMut(&mut usize)) {
        match node {
            Window::Leaf { bufno } => f(bufno),
            Window::Split { children, .. } => {
                for (_, child) in children {
                    Self::walk_mut(child, f);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn new_tree_is_single_leaf() {
        let t = WindowTree::new(3);
        assert_eq!(t.focused_bufno(), 3);
        let layout = t.layout(r(80, 24));
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].bufno, 3);
    }

    #[test]
    fn split_creates_two_leaves_and_focuses_new() {
        let mut t = WindowTree::new(1);
        t.split(SplitDir::Horizontal, 2);
        let layout = t.layout(r(80, 24));
        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0].bufno, 1);
        assert_eq!(layout[1].bufno, 2);
        assert_eq!(t.focused_bufno(), 2);
    }

    #[test]
    fn focus_next_cycles() {
        let mut t = WindowTree::new(1);
        t.split(SplitDir::Horizontal, 2); // focus on 2
        t.focus_next();
        assert_eq!(t.focused_bufno(), 1);
        t.focus_next();
        assert_eq!(t.focused_bufno(), 2);
    }

    #[test]
    fn close_focused_collapses_split() {
        let mut t = WindowTree::new(1);
        t.split(SplitDir::Vertical, 2); // focus on 2
        t.close_focused();
        let layout = t.layout(r(80, 24));
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].bufno, 1);
        assert_eq!(t.focused_bufno(), 1);
    }

    #[test]
    fn close_focused_noop_on_single_leaf() {
        let mut t = WindowTree::new(5);
        t.close_focused();
        assert_eq!(t.focused_bufno(), 5);
    }

    #[test]
    fn nested_split_layouts() {
        let mut t = WindowTree::new(1);
        t.split(SplitDir::Horizontal, 2); // focus on 2
        t.split(SplitDir::Vertical, 3); // split 2 vertically, focus on 3
        let layout = t.layout(r(80, 24));
        assert_eq!(layout.len(), 3);
        // tree order: left half (bufno 1), then right-top (2), then right-bottom (3)
        assert_eq!(layout[0].bufno, 1);
        assert_eq!(layout[1].bufno, 2);
        assert_eq!(layout[2].bufno, 3);
    }

    #[test]
    fn for_each_leaf_mut_reindexes() {
        let mut t = WindowTree::new(2);
        t.split(SplitDir::Horizontal, 3);
        // Simulate: bufno 1 was removed from the buffer Vec — all indices
        // ≥ 1 shift down by one.
        t.for_each_leaf_mut(|b| {
            if *b > 1 {
                *b -= 1;
            }
        });
        let layout = t.layout(r(10, 10));
        assert_eq!(layout[0].bufno, 1);
        assert_eq!(layout[1].bufno, 2);
    }
}
