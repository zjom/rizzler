pub use nodes::{Delta, Leaf, Meta, Node, Trunk};
use std::collections::HashMap;

mod nodes;

#[derive(Debug, Clone)]
pub struct ChangeTree {
    trunk: usize,
    nodes: HashMap<usize, Node>,
    cur: usize,
    largest_assigned_id: usize,
    /// parent_id -> child_id to redo into (the branch we last undid from)
    redo_target: HashMap<usize, usize>,
    /// Next leaf id `walk_back_edit` will visit. `None` means start fresh
    /// from `cur`. Reset on every new tracked edit so a fresh `g;` lands on
    /// the edit you just made.
    walk_id: Option<usize>,
}

impl ChangeTree {
    pub fn new() -> Self {
        let id = 0;
        let root = Node::Root(Trunk::new(Meta::new(id)));
        let mut nodes = HashMap::new();
        nodes.insert(id, root);
        Self {
            trunk: id,
            nodes,
            cur: id,
            largest_assigned_id: id,
            redo_target: HashMap::new(),
            walk_id: None,
        }
    }

    pub fn track_change(&mut self, delta: Delta) {
        let newnode_id = self.gen_id();
        let meta = Meta::new(newnode_id);
        let newnode = Node::Leaf(Leaf::new(meta, self.cur, delta));

        // fresh id => insert must NOT collide
        let clash = self.nodes.insert(newnode_id, newnode);
        assert!(
            clash.is_none(),
            "ids are monotonically increasing and must never clash"
        );

        // works for both Root and Leaf parents
        self.nodes
            .get_mut(&self.cur)
            .expect("self.cur must refer to a valid node")
            .children_mut()
            .push(meta);

        // a new change becomes the canonical redo target for this parent
        self.redo_target.insert(self.cur, newnode_id);
        self.cur = newnode_id;
        self.walk_id = None;
    }

    /// Mutate the current leaf's delta in place. Used to coalesce a run of
    /// adjacent edits (e.g. typed characters in insert mode) into a single
    /// tree node so undo reverts the whole run. Returns `false` when the
    /// current node is the root, in which case the caller should fall back
    /// to [`Self::track_change`].
    pub fn extend_current(&mut self, f: impl FnOnce(&mut Delta)) -> bool {
        match self
            .nodes
            .get_mut(&self.cur)
            .expect("self.cur must refer to a valid node")
        {
            Node::Leaf(leaf) => {
                f(&mut leaf.delta);
                true
            }
            Node::Root(_) => false,
        }
    }

    pub fn undo(&mut self) -> Option<Delta> {
        if self.cur == self.trunk {
            return None;
        }

        let child_id = self.cur;
        let (parent, delta) = {
            let cur = self
                .nodes
                .get(&self.cur)
                .expect("self.cur must refer to a valid node");
            match cur {
                Node::Leaf(leaf) => (leaf.parent, leaf.delta.clone()),
                Node::Root(_) => unreachable!("trunk handled by the guard above"),
            }
        };

        // remember where to redo, and move up. children heap is left intact:
        // membership is unchanged by undo, so the heap stays consistent.
        self.redo_target.insert(parent, child_id);
        self.cur = parent;
        Some(delta)
    }

    pub fn redo(&mut self) -> Option<Delta> {
        // pick the branch we last undid from; if none recorded, fall back to
        // the most-recent child via the heap (peek, don't mutate).
        let child_id = match self.redo_target.get(&self.cur) {
            Some(&id) => id,
            None => {
                self.nodes
                    .get(&self.cur)
                    .expect("self.cur must be valid")
                    .children()
                    .peek()?
                    .id
            }
        };

        let delta = match self
            .nodes
            .get(&child_id)
            .expect("child id from redo target must be valid")
        {
            Node::Leaf(leaf) => leaf.delta.clone(),
            Node::Root(_) => unreachable!("a root is never a child"),
        };

        self.cur = child_id;
        Some(delta)
    }

    /// Vim `g;` — walk `n` steps back along the parent chain from the change
    /// list cursor and return the `cursor_after` of the leaf we land on. The
    /// walk position advances by one step on each call, so repeated `g;`
    /// presses keep moving backward through the history. Returns `None` when
    /// the walk has reached the root (no further edits to visit).
    pub fn walk_back_edit(&mut self, n: u32) -> Option<(usize, usize)> {
        let mut id = self.walk_id.unwrap_or(self.cur);
        let steps = n.max(1);
        for _ in 1..steps {
            let leaf = self.nodes.get(&id)?.as_leaf()?;
            id = leaf.parent;
        }
        let leaf = self.nodes.get(&id)?.as_leaf()?;
        let cursor = leaf.delta.cursor_after;
        self.walk_id = Some(leaf.parent);
        Some(cursor)
    }

    fn gen_id(&mut self) -> usize {
        self.largest_assigned_id += 1;
        self.largest_assigned_id
    }
}

impl Default for ChangeTree {
    fn default() -> Self {
        Self::new()
    }
}
