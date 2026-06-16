//! Filesystem load/save for [`crate::Buffer`]. Kept out of the buffer module
//! so the buffer itself stays in-memory-only and the only filesystem
//! entrypoints live in one place.

use ropey::Rope;
use std::io;
use std::path::Path;
use std::rc::Rc;
use tracing::{debug, info, warn};

use crate::buffer::Buffer;

pub fn from_reader(r: impl io::Read) -> io::Result<Buffer> {
    Ok(Buffer {
        buf: Rope::from_reader(r)?,
        ..Buffer::default()
    })
}

/// Construct a buffer associated with `path`. Attempts to read from disk; on
/// any read failure produces an empty buffer with `fs_path` still set so a
/// subsequent [`write()`] will create the file.
pub fn with_path(path: Rc<Path>) -> Buffer {
    let mut buf = match std::fs::File::open(&path).and_then(from_reader) {
        Ok(b) => {
            info!(path = %path.display(), bytes = b.buf.len_bytes(), "loaded buffer from disk");
            b
        }
        Err(e) => {
            debug!(path = %path.display(), error = %e, "no on-disk file (or read failed) -> empty buffer");
            Buffer::default()
        }
    };
    buf.fs_path = Some(path);
    buf
}

/// Write `buffer`'s contents to disk. Resolves the destination from `path`,
/// then falls back to `buffer.fs_path`. No-op (returns `Ok(())`) when both
/// are `None`. On success the resolved path is stored back on `buffer`.
pub fn write(buffer: &mut Buffer, path: Option<Rc<Path>>) -> io::Result<()> {
    let resolved = path.or_else(|| buffer.fs_path.take());
    if let Some(path) = resolved {
        match std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
        {
            Ok(f) => {
                if let Err(e) = buffer.buf.write_to(f) {
                    warn!(path = %path.display(), error = %e, "rope write_to failed");
                    return Err(e);
                }
                info!(path = %path.display(), bytes = buffer.buf.len_bytes(), "wrote buffer to disk");
                buffer.fs_path = Some(path);
                buffer.mark_saved();
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "could not open file for write");
                return Err(e);
            }
        }
    } else {
        debug!("write: no path on buffer -> no-op");
    }
    Ok(())
}
