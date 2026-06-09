//! Editor window tree: splits and leaves, each leaf pointing at a buffer
//! via a stable `BufferId`. The minibuffer lives outside this tree — it's
//! a single-row strip rendered via [`crate::Widget::Minibuffer`].
//!
//! Direction enums ([`SplitDir`], [`FocusDir`]) live in `rizz_core` so
//! `rizz_actions` can reference them without depending on this crate.

#![allow(dead_code)]

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use rizz_text::BufferId;

pub use rizz_core::{FocusDir, SplitDir};

trait SplitDirExt {
    fn to_ratatui(self) -> Direction;
}

impl SplitDirExt for SplitDir {
    fn to_ratatui(self) -> Direction {
        match self {
            SplitDir::Horizontal => Direction::Horizontal,
            SplitDir::Vertical => Direction::Vertical,
        }
    }
}

trait FocusDirExt {
    fn split_axis(self) -> SplitDir;
    fn forward(self) -> bool;
}

impl FocusDirExt for FocusDir {
    fn split_axis(self) -> SplitDir {
        match self {
            FocusDir::Left | FocusDir::Right => SplitDir::Horizontal,
            FocusDir::Up | FocusDir::Down => SplitDir::Vertical,
        }
    }

    fn forward(self) -> bool {
        matches!(self, FocusDir::Right | FocusDir::Down)
    }
}

#[derive(Debug, Clone)]
pub enum Window {
    Leaf {
        buf: BufferId,
    },
    Split {
        dir: SplitDir,
        children: Vec<(u16, Window)>,
    },
}

impl Window {
    pub fn leaf(buf: BufferId) -> Self {
        Self::Leaf { buf }
    }
}

/// Index path from root through Split children to a Leaf.
pub type LeafPath = Vec<usize>;

#[derive(Debug, Clone)]
pub struct LeafLayout {
    pub path: LeafPath,
    pub buf: BufferId,
    pub area: Rect,
}

#[derive(Debug, Clone)]
pub struct WindowTree {
    root: Window,
    focused: LeafPath,
}

impl WindowTree {
    pub fn new(buf: BufferId) -> Self {
        Self {
            root: Window::leaf(buf),
            focused: Vec::new(),
        }
    }

    pub fn focused_path(&self) -> &LeafPath {
        &self.focused
    }

    pub fn focused_buf(&self) -> BufferId {
        match self.node_at(&self.focused) {
            Some(Window::Leaf { buf }) => *buf,
            _ => panic!("focused path must point to a leaf"),
        }
    }

    pub fn set_focused_buf(&mut self, buf: BufferId) {
        let path = self.focused.clone();
        if let Some(Window::Leaf { buf: b }) = self.node_at_mut(&path) {
            *b = buf;
        }
    }

    pub fn layout(&self, area: Rect) -> Vec<LeafLayout> {
        let mut out = Vec::new();
        Self::layout_node(&self.root, area, &mut Vec::new(), &mut out);
        out
    }

    fn layout_node(node: &Window, area: Rect, path: &mut LeafPath, out: &mut Vec<LeafLayout>) {
        match node {
            Window::Leaf { buf } => out.push(LeafLayout {
                path: path.clone(),
                buf: *buf,
                area,
            }),
            Window::Split { dir, children } => {
                let total: u32 = children.iter().map(|(w, _)| *w as u32).sum::<u32>().max(1);
                let constraints: Vec<Constraint> = children
                    .iter()
                    .map(|(w, _)| Constraint::Ratio(*w as u32, total))
                    .collect();
                let rects = Layout::default()
                    .direction(dir.to_ratatui())
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

    pub fn split(&mut self, dir: SplitDir, new_buf: BufferId) {
        let current_buf = self.focused_buf();
        let path = self.focused.clone();
        if let Some(leaf) = self.node_at_mut(&path) {
            *leaf = Window::Split {
                dir,
                children: vec![(1, Window::leaf(current_buf)), (1, Window::leaf(new_buf))],
            };
        }
        self.focused.push(1);
    }

    pub fn focus_dir(&mut self, dir: FocusDir) {
        let axis = dir.split_axis();
        let forward = dir.forward();
        let mut path = self.focused.clone();
        while let Some(child_idx) = path.pop() {
            let Some(Window::Split {
                dir: split_dir,
                children,
            }) = self.node_at(&path)
            else {
                continue;
            };
            if *split_dir != axis {
                continue;
            }
            let sibling = if forward {
                (child_idx + 1 < children.len()).then_some(child_idx + 1)
            } else {
                (child_idx > 0).then(|| child_idx - 1)
            };
            let Some(sibling_idx) = sibling else { continue };
            path.push(sibling_idx);
            Self::descend_first_leaf(&self.root, &mut path);
            self.focused = path;
            return;
        }
    }

    fn descend_first_leaf(root: &Window, path: &mut LeafPath) {
        let mut node = root;
        for &i in path.iter() {
            node = match node {
                Window::Split { children, .. } => &children[i].1,
                Window::Leaf { .. } => return,
            };
        }
        while let Window::Split { children, .. } = node {
            path.push(0);
            node = &children[0].1;
        }
    }

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
        if children.len() == 1 {
            let (_, only) = children.remove(0);
            *parent = only;
        }

        self.focused = parent_path;
        loop {
            match self.node_at(&self.focused) {
                Some(Window::Leaf { .. }) | None => break,
                Some(Window::Split { .. }) => self.focused.push(0),
            }
        }
    }

    /// Visit every leaf's buffer id. Used to redirect leaves pointing at
    /// a buffer that's being removed to a fallback id.
    pub fn for_each_leaf_mut(&mut self, mut f: impl FnMut(&mut BufferId)) {
        Self::walk_mut(&mut self.root, &mut f);
    }

    fn walk_mut(node: &mut Window, f: &mut impl FnMut(&mut BufferId)) {
        match node {
            Window::Leaf { buf } => f(buf),
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
    use slotmap::{Key, KeyData};

    fn bid(n: u64) -> BufferId {
        BufferId::from(KeyData::from_ffi(n))
    }

    fn r(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn new_tree_is_single_leaf() {
        let id = bid(3);
        let t = WindowTree::new(id);
        assert_eq!(t.focused_buf(), id);
        let layout = t.layout(r(80, 24));
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].buf, id);
    }

    #[test]
    fn split_creates_two_leaves_and_focuses_new() {
        let a = bid(1);
        let b = bid(2);
        let mut t = WindowTree::new(a);
        t.split(SplitDir::Horizontal, b);
        let layout = t.layout(r(80, 24));
        assert_eq!(layout.len(), 2);
        assert_eq!(t.focused_buf(), b);
    }

    #[test]
    fn focus_dir_moves_across_horizontal_split() {
        let a = bid(1);
        let b = bid(2);
        let mut t = WindowTree::new(a);
        t.split(SplitDir::Horizontal, b);
        t.focus_dir(FocusDir::Left);
        assert_eq!(t.focused_buf(), a);
        t.focus_dir(FocusDir::Right);
        assert_eq!(t.focused_buf(), b);
    }

    #[test]
    fn close_focused_collapses_split() {
        let a = bid(1);
        let b = bid(2);
        let mut t = WindowTree::new(a);
        t.split(SplitDir::Vertical, b);
        t.close_focused();
        let layout = t.layout(r(80, 24));
        assert_eq!(layout.len(), 1);
        assert_eq!(t.focused_buf(), a);
    }

    #[test]
    fn for_each_leaf_mut_redirects() {
        let a = bid(2);
        let b = bid(3);
        let fallback = bid(1);
        let mut t = WindowTree::new(a);
        t.split(SplitDir::Horizontal, b);
        t.for_each_leaf_mut(|id| {
            if *id == b {
                *id = fallback;
            }
        });
        let layout = t.layout(r(10, 10));
        assert!(layout.iter().any(|l| l.buf == a));
        assert!(layout.iter().any(|l| l.buf == fallback));
        assert!(layout.iter().all(|l| l.buf != b));
    }

    fn _key_suppress() {
        let _ = bid(0).is_null();
    }
}
