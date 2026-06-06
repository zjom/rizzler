//! Filesystem load/save for [`Buffer`]. Pulled out of `buffer.rs` so the
//! buffer itself stays in-memory-only and the only filesystem entrypoints
//! live in one place.

use ropey::Rope;
use std::io;
use std::path::Path;
use std::rc::Rc;

use crate::buffer::Buffer;

/// Read a buffer's contents from `r`. Other fields default.
pub fn from_reader(r: impl io::Read) -> io::Result<Buffer> {
    Ok(Buffer {
        buf: Rope::from_reader(r)?,
        ..Buffer::default()
    })
}

/// Construct a buffer associated with `path`. Attempts to read from disk; on
/// any read failure produces an empty buffer with `fs_path` still set so a
/// subsequent [`write`] will create the file.
pub fn with_path(path: Rc<Path>) -> Buffer {
    let mut buf = std::fs::File::open(&path)
        .and_then(from_reader)
        .unwrap_or_default();
    buf.fs_path = Some(path);
    buf
}

/// Write `buffer`'s contents to disk. Resolves the destination from `path`,
/// then falls back to `buffer.fs_path`. No-op (returns `Ok(())`) when both
/// are `None`. On success the resolved path is stored back on `buffer`.
pub fn write(buffer: &mut Buffer, path: Option<Rc<Path>>) -> io::Result<()> {
    let resolved = path.or_else(|| buffer.fs_path.take());
    if let Some(path) = resolved {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)?;
        buffer.buf.write_to(f)?;
        buffer.fs_path = Some(path);
    }
    Ok(())
}
