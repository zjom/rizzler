//! Buffer registry owned by `State`. A `SlotMap` keyed by `BufferId` keeps
//! handles stable across removals — window leaves, popups, and `BufferView`
//! widgets can hold a `BufferId` without ever needing reindex.
//!
//! The registry also keeps a parallel ordered list of *file* buffers (the
//! minibuffer is excluded) so the `:bn`/`:bp` cycle has a deterministic next
//! buffer.

use std::ops::{Index, IndexMut};
use std::path::Path;
use std::rc::Rc;

use rizz_core::EditingMode;
use rizz_text::{Buffer, BufferId, io as buffer_io};
use slotmap::SlotMap;

pub struct BufferList {
    bufs: SlotMap<BufferId, Buffer>,
    minibuffer: BufferId,
    /// File buffers in creation order. Used by `cycle` and `first_file_buf`.
    /// The minibuffer and panel-backing buffers are not in this list.
    file_order: Vec<BufferId>,
}

impl BufferList {
    pub fn new() -> Self {
        let mut bufs = SlotMap::with_key();
        let mut minibuf = Buffer::new();
        minibuf.set_mode(EditingMode::Command);
        let minibuffer = bufs.insert(minibuf);
        let first = bufs.insert(Buffer::new());
        Self {
            bufs,
            minibuffer,
            file_order: vec![first],
        }
    }

    pub fn len(&self) -> usize {
        self.bufs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bufs.is_empty()
    }

    /// Iterate every buffer in the registry. Order is unspecified.
    pub fn iter(&self) -> slotmap::basic::Iter<'_, BufferId, Buffer> {
        self.bufs.iter()
    }

    /// The underlying slot map. Handed to the renderer / precompute pass so
    /// they can look up buffers by `BufferId` without going through
    /// `BufferList` (which lives one crate up).
    pub fn raw(&self) -> &SlotMap<BufferId, Buffer> {
        &self.bufs
    }

    /// Iterate every buffer mutably. Order is unspecified.
    pub fn iter_mut(&mut self) -> slotmap::basic::IterMut<'_, BufferId, Buffer> {
        self.bufs.iter_mut()
    }

    /// File buffers in stable insertion order.
    pub fn file_ids(&self) -> &[BufferId] {
        &self.file_order
    }

    pub fn get(&self, id: BufferId) -> Option<&Buffer> {
        self.bufs.get(id)
    }

    pub fn get_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.bufs.get_mut(id)
    }

    pub fn contains(&self, id: BufferId) -> bool {
        self.bufs.contains_key(id)
    }

    pub fn minibuffer_id(&self) -> BufferId {
        self.minibuffer
    }

    pub fn minibuffer(&self) -> &Buffer {
        &self.bufs[self.minibuffer]
    }

    pub fn minibuffer_mut(&mut self) -> &mut Buffer {
        &mut self.bufs[self.minibuffer]
    }

    /// Append a file buffer (one that participates in `:bn`/`:bp` cycling
    /// and counts toward `:bd`'s last-file-buffer safeguard) and return its
    /// `BufferId`.
    pub fn push_file(&mut self, buf: Buffer) -> BufferId {
        let id = self.bufs.insert(buf);
        self.file_order.push(id);
        id
    }

    /// Append a panel-backing buffer (the buffer behind an overlay panel).
    /// Not in the file cycle, not counted toward file-buf safeguards.
    pub fn push_panel(&mut self, buf: Buffer) -> BufferId {
        self.bufs.insert(buf)
    }

    /// True if `id` is a file buffer (in the cycle order). Use this in
    /// place of asking the buffer about itself — file-ness is a property of
    /// how the buffer was registered, not of the buffer's content.
    pub fn is_file_buf(&self, id: BufferId) -> bool {
        self.file_order.iter().any(|&i| i == id)
    }

    /// Remove the buffer at `id`. Returns true if the buffer existed.
    pub fn remove(&mut self, id: BufferId) -> bool {
        if self.bufs.remove(id).is_none() {
            return false;
        }
        self.file_order.retain(|&i| i != id);
        true
    }

    /// Replace the buffer at `id` with a fresh scratch buffer in place. Keeps
    /// the same `BufferId` so window leaves pointing at it stay correct.
    pub fn reset(&mut self, id: BufferId) {
        if let Some(b) = self.bufs.get_mut(id) {
            *b = Buffer::new();
        }
    }

    pub fn find_by_path(&self, path: &Path) -> Option<BufferId> {
        self.bufs
            .iter()
            .find(|(_, b)| b.fs_path().as_deref() == Some(path))
            .map(|(id, _)| id)
    }

    /// Either clone the existing buffer for `path` or open it fresh from disk.
    pub fn buffer_for_path(&self, path: Rc<Path>) -> Buffer {
        self.bufs
            .iter()
            .find(|(_, b)| b.fs_path().as_deref() == Some(path.as_ref()))
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| buffer_io::with_path(path))
    }

    pub fn file_buf_count(&self) -> usize {
        self.file_order.len()
    }

    /// `BufferId` of the first non-minibuffer buffer. Panics when none exist —
    /// callers maintain the invariant by refusing to delete the last file
    /// buffer (`delete_buf` resets it in place instead).
    pub fn first_file_buf(&self) -> BufferId {
        *self
            .file_order
            .first()
            .expect("at least one file buffer always exists")
    }

    /// 1-based position of `id` in the file cycle order, or `None` if `id`
    /// isn't a file buffer. Used by status-line code as a human-friendly
    /// buffer label, since the underlying `BufferId` is an opaque handle.
    pub fn file_display_index(&self, id: BufferId) -> Option<usize> {
        self.file_order.iter().position(|&i| i == id).map(|p| p + 1)
    }

    /// Cycle through file buffers starting from `from`. Returns the next/prev
    /// file buffer's id, or `None` if there's only one file buffer (or none).
    pub fn cycle(&self, from: BufferId, dir: CycleDir) -> Option<BufferId> {
        let n = self.file_order.len();
        if n < 2 {
            return None;
        }
        let cur = self.file_order.iter().position(|&i| i == from);
        let start = cur.unwrap_or(0);
        let next = match dir {
            CycleDir::Next => (start + 1) % n,
            CycleDir::Prev => (start + n - 1) % n,
        };
        Some(self.file_order[next])
    }
}

impl Default for BufferList {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone, Debug)]
pub enum CycleDir {
    Next,
    Prev,
}

impl Index<BufferId> for BufferList {
    type Output = Buffer;
    fn index(&self, id: BufferId) -> &Buffer {
        &self.bufs[id]
    }
}

impl IndexMut<BufferId> for BufferList {
    fn index_mut(&mut self, id: BufferId) -> &mut Buffer {
        &mut self.bufs[id]
    }
}
