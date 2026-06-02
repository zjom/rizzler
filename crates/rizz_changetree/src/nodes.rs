use std::{cmp::Ordering, collections::BinaryHeap, rc::Rc, time::Instant};

/// line number, and changed line
pub type Delta = (usize, Rc<str>);

/// identifier of nodes.
///
/// two nodes are PartialEq if their ids are equal
/// no two nodes should have the same ids
///
/// Ordering prioritizes recency over ID
#[derive(Debug, Clone, Copy)]
pub struct Meta {
    pub id: usize,
    pub timestamp: Instant,
}
impl Meta {
    pub fn new(id: usize) -> Self {
        let timestamp = Instant::now();
        Self::with_timestamp(id, timestamp)
    }

    pub fn with_timestamp(id: usize, timestamp: Instant) -> Self {
        Self { id, timestamp }
    }
}

#[derive(Debug, Clone)]
pub struct Trunk {
    pub meta: Meta,
    pub children: BinaryHeap<Meta>,
}

impl Trunk {
    pub fn new(meta: Meta) -> Self {
        Self::with_children(meta, BinaryHeap::new())
    }
    pub fn with_children(meta: Meta, children: BinaryHeap<Meta>) -> Self {
        Self { meta, children }
    }
    pub fn meta(&self) -> &Meta {
        &self.meta
    }
}

#[derive(Debug, Clone)]
pub struct Leaf {
    pub meta: Meta,
    pub children: BinaryHeap<Meta>,
    pub delta: Delta,
    pub parent: usize,
}

impl Leaf {
    pub fn new(meta: Meta, parent: usize, delta: Delta) -> Self {
        Self::with_children(meta, parent, delta, BinaryHeap::new())
    }
    pub fn with_children(
        meta: Meta,
        parent: usize,
        delta: Delta,
        children: BinaryHeap<Meta>,
    ) -> Self {
        Self {
            meta,
            children,
            delta,
            parent,
        }
    }
    pub fn meta(&self) -> &Meta {
        &self.meta
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Root(Trunk),
    Leaf(Leaf),
}

impl Node {
    pub fn as_leaf(&self) -> Option<&Leaf> {
        match self {
            Node::Leaf(leaf) => Some(leaf),
            Node::Root(_) => None,
        }
    }

    pub fn as_root(&self) -> Option<&Trunk> {
        match self {
            Node::Leaf(_) => None,
            Node::Root(root) => Some(root),
        }
    }

    pub fn meta(&self) -> &Meta {
        match self {
            Node::Leaf(n) => n.meta(),
            Node::Root(n) => n.meta(),
        }
    }

    pub fn children(&self) -> &BinaryHeap<Meta> {
        match self {
            Node::Root(n) => &n.children,
            Node::Leaf(n) => &n.children,
        }
    }

    pub fn children_mut(&mut self) -> &mut BinaryHeap<Meta> {
        match self {
            Node::Root(n) => &mut n.children,
            Node::Leaf(n) => &mut n.children,
        }
    }
}

impl PartialEq for Meta {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Meta {}

impl PartialOrd for Meta {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Meta {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.timestamp.cmp(&other.timestamp) {
            Ordering::Less => return Ordering::Less,
            Ordering::Greater => return Ordering::Greater,
            Ordering::Equal => {}
        };
        self.id.cmp(&other.id)
    }
}

impl PartialEq for Leaf {
    fn eq(&self, other: &Self) -> bool {
        self.meta.eq(&other.meta)
    }
}
impl PartialOrd for Leaf {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.meta.partial_cmp(&other.meta)
    }
}

impl PartialEq for Trunk {
    fn eq(&self, other: &Self) -> bool {
        self.meta.eq(&other.meta)
    }
}
impl PartialOrd for Trunk {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.meta.partial_cmp(&other.meta)
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.meta().eq(other.meta())
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.meta().partial_cmp(other.meta())
    }
}
