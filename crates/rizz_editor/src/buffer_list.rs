//! Buffer collection owned by `State`. Wraps the `Vec<Buffer>` and the
//! minibuffer index together so the reindex-on-removal invariant stays in
//! one place. Other indexed views (window leaves, popup `bufno`s) live
//! outside; their reindex helpers run alongside `BufferList::remove`.

use std::ops::{Index, IndexMut};
use std::path::Path;
use std::rc::Rc;

use rizz_text::{Buffer, BufferKind, io as buffer_io};

pub struct BufferList {
    bufs: Vec<Buffer>,
    minibuffer: usize,
}

impl BufferList {
    /// Initial layout: `[minibuffer, first file buffer]`.
    pub fn new() -> Self {
        Self {
            bufs: vec![Buffer::minibuffer(), Buffer::new()],
            minibuffer: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.bufs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bufs.is_empty()
    }

    pub fn as_slice(&self) -> &[Buffer] {
        &self.bufs
    }

    pub fn as_mut_slice(&mut self) -> &mut [Buffer] {
        &mut self.bufs
    }

    pub fn get_mut(&mut self, i: usize) -> Option<&mut Buffer> {
        self.bufs.get_mut(i)
    }

    pub fn minibuffer_index(&self) -> usize {
        self.minibuffer
    }

    pub fn minibuffer(&self) -> &Buffer {
        &self.bufs[self.minibuffer]
    }

    pub fn minibuffer_mut(&mut self) -> &mut Buffer {
        &mut self.bufs[self.minibuffer]
    }

    /// Append a buffer and return its bufno.
    pub fn push(&mut self, buf: Buffer) -> usize {
        self.bufs.push(buf);
        self.bufs.len() - 1
    }

    /// Remove the buffer at `bufno`. Keeps the minibuffer index in sync.
    /// Callers must separately reindex their own bufno references (windows,
    /// popups).
    pub fn remove(&mut self, bufno: usize) -> bool {
        if bufno >= self.bufs.len() {
            return false;
        }
        self.bufs.remove(bufno);
        if self.minibuffer > bufno {
            self.minibuffer -= 1;
        }
        true
    }

    /// Replace the buffer at `bufno` with a fresh scratch buffer.
    pub fn reset(&mut self, bufno: usize) {
        self.bufs[bufno] = Buffer::new();
    }

    pub fn find_by_path(&self, path: &Path) -> Option<usize> {
        self.bufs
            .iter()
            .position(|b| b.fs_path().as_deref() == Some(path))
    }

    /// Either clone the existing buffer for `path` or open it fresh from disk.
    pub fn buffer_for_path(&self, path: Rc<Path>) -> Buffer {
        self.bufs
            .iter()
            .find(|b| b.fs_path().as_deref() == Some(path.as_ref()))
            .cloned()
            .unwrap_or_else(|| buffer_io::with_path(path))
    }

    pub fn file_buf_count(&self) -> usize {
        self.bufs
            .iter()
            .filter(|b| b.kind() != BufferKind::Minibuffer)
            .count()
    }

    /// Bufno of the first non-minibuffer buffer. Panics when none exist —
    /// callers maintain the invariant by refusing to delete the last file
    /// buffer (`delete_buf` resets it in place instead).
    pub fn first_file_buf(&self) -> usize {
        self.bufs
            .iter()
            .position(|b| b.kind() != BufferKind::Minibuffer)
            .expect("at least one file buffer always exists")
    }

    /// Cycle direction for [`Self::cycle`].
    pub fn cycle(&self, from: usize, dir: CycleDir) -> Option<usize> {
        let n = self.bufs.len();
        if n == 0 {
            return None;
        }
        let mut i = from;
        for _ in 0..n {
            i = match dir {
                CycleDir::Next => {
                    if i + 1 >= n {
                        0
                    } else {
                        i + 1
                    }
                }
                CycleDir::Prev => {
                    if i == 0 {
                        n - 1
                    } else {
                        i - 1
                    }
                }
            };
            if self.bufs[i].kind() != BufferKind::Minibuffer {
                return Some(i);
            }
        }
        None
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

impl Index<usize> for BufferList {
    type Output = Buffer;
    fn index(&self, i: usize) -> &Buffer {
        &self.bufs[i]
    }
}

impl IndexMut<usize> for BufferList {
    fn index_mut(&mut self, i: usize) -> &mut Buffer {
        &mut self.bufs[i]
    }
}
