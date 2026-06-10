//! Buffer and window management: focused-buffer accessors, file open/edit/
//! write/delete, window split/close, and the `:bn`/`:bp` cycle.

use std::path::Path;
use std::rc::Rc;

use rizz_text::{Buffer, BufferId, io as buffer_io};
use tracing::{debug, info, instrument, trace, warn};

use crate::buffer_list::CycleDir;

use super::State;

pub use rizz_core::SplitDir;

impl State {
    pub fn focused_buf(&self) -> &Buffer {
        let id = self.focused_buf_id();
        &self.bufs[id]
    }

    pub fn focused_buf_mut(&mut self) -> &mut Buffer {
        let id = self.focused_buf_id();
        &mut self.bufs[id]
    }

    pub fn nbufs(&self) -> usize {
        self.bufs.len()
    }

    pub fn minibuffer_id(&self) -> BufferId {
        self.bufs.minibuffer_id()
    }

    pub fn buf_exists(&self, id: BufferId) -> bool {
        self.bufs.contains(id)
    }

    /// 1-based display index of `id` among file buffers, or `None` for the
    /// minibuffer / popup-backing buffers / unknown ids.
    pub fn buf_display_index(&self, id: BufferId) -> Option<usize> {
        self.bufs.file_display_index(id)
    }

    pub fn set_buffer_contents(&mut self, buf: BufferId, msg: &str) {
        if let Some(b) = self.bufs.get_mut(buf) {
            b.clear_with(msg);
        }
    }

    /// Read the current minibuffer text and leave the minibuffer.
    pub fn take_minibuffer_command(&mut self) -> String {
        let cmd = self.bufs.minibuffer().text();
        self.exit_minibuffer();
        cmd
    }

    /// The substring of the minibuffer token that ends at the cursor — what
    /// candidate completions must `starts_with`. Always operates on the
    /// minibuffer regardless of which buffer currently has focus, since a
    /// completion popup may have stolen focus while the cmd line is still up.
    pub fn minibuffer_completion_prefix(&self) -> String {
        let mb = self.bufs.minibuffer();
        crate::completion::prefix_at(&mb.text(), mb.abs_col())
    }

    /// Replace the token under the minibuffer cursor with `replacement`,
    /// landing the cursor at the end of the inserted text. Falls back to a
    /// plain insert when the cursor isn't on a token. Operates on the
    /// minibuffer directly — see [`Self::minibuffer_completion_prefix`].
    pub fn apply_minibuffer_completion(&mut self, replacement: &str) {
        let (text, cursor) = {
            let mb = self.bufs.minibuffer();
            (mb.text(), mb.abs_col())
        };
        let (start, end) = crate::completion::token_bounds(&text, cursor);
        let mb = self.bufs.minibuffer_mut();
        if start < end {
            mb.delete_range(start, end);
        }
        mb.insert_many(replacement);
    }

    /// Replace the entire minibuffer line with `text`, landing the cursor at
    /// the end. Used by command-history recall.
    fn set_minibuffer_text(&mut self, text: &str) {
        let mb = self.bufs.minibuffer_mut();
        mb.clear();
        mb.insert_many(text);
    }

    /// Forget the command-history recall position so the next `<up>` starts
    /// again from the newest entry. Called on every (re)entry into command
    /// mode.
    pub(super) fn reset_cmd_history_nav(&mut self) {
        self.cmd_history_nav = Default::default();
    }

    /// `<up>` in command mode: pull the previous (older) submitted command
    /// into the minibuffer. The first press stashes the current draft so it
    /// can be restored by walking back down past the newest entry.
    pub fn command_history_prev(&mut self) {
        let n = self.journal.command_count();
        if n == 0 {
            return;
        }
        let next = match self.cmd_history_nav.pos {
            None => {
                self.cmd_history_nav.draft = self.bufs.minibuffer().text();
                n - 1
            }
            Some(0) => return, // already at the oldest entry
            Some(i) => i - 1,
        };
        self.cmd_history_nav.pos = Some(next);
        if let Some(cmd) = self.journal.command(next) {
            let cmd = cmd.to_string();
            self.set_minibuffer_text(&cmd);
        }
    }

    /// `<down>` in command mode: pull the next (newer) command in, or restore
    /// the stashed draft once we step past the newest entry. A no-op while the
    /// user is still on their own un-recalled line.
    pub fn command_history_next(&mut self) {
        let Some(i) = self.cmd_history_nav.pos else {
            return;
        };
        let n = self.journal.command_count();
        if i + 1 < n {
            self.cmd_history_nav.pos = Some(i + 1);
            if let Some(cmd) = self.journal.command(i + 1) {
                let cmd = cmd.to_string();
                self.set_minibuffer_text(&cmd);
            }
        } else {
            self.cmd_history_nav.pos = None;
            let draft = std::mem::take(&mut self.cmd_history_nav.draft);
            self.set_minibuffer_text(&draft);
        }
    }

    pub(super) fn open_or_focus_file(&mut self, path: &Path) -> BufferId {
        let id = self.find_or_open_file(path);
        self.surface.windows.set_focused_buf(id);
        id
    }

    pub(super) fn find_or_open_file(&mut self, path: &Path) -> BufferId {
        let path: Rc<Path> = Rc::from(path);
        let ids: Vec<BufferId> = self.bufs.file_ids().to_vec();
        for id in ids {
            if self.bufs.get(id).and_then(|b| b.fs_path()).as_deref() == Some(&*path) {
                return id;
            }
        }
        let new_buf = buffer_io::with_path(path.clone());
        let new_id = self.bufs.push_file(new_buf);
        self.install_highlighter(new_id);
        self.install_lsp_client(new_id);
        new_id
    }

    #[instrument(skip(self))]
    pub(super) fn create_buf(&mut self, set_active: bool, path: Option<Rc<Path>>) -> BufferId {
        let buf = match path {
            Some(p) => self.bufs.buffer_for_path(p),
            None => Buffer::new(),
        };
        let id = self.bufs.push_file(buf);
        self.install_highlighter(id);
        self.install_lsp_client(id);
        if set_active {
            self.surface.windows.set_focused_buf(id);
        }
        info!(buf = ?id, set_active, "created buffer");
        id
    }

    #[instrument(skip(self))]
    pub(super) fn edit_buf(&mut self, path: Rc<Path>) -> BufferId {
        let id = match self.bufs.find_by_path(&path) {
            Some(id) => {
                debug!(buf = ?id, "edit_buf: reusing existing buffer");
                id
            }
            None => {
                let pushed = self.bufs.push_file(buffer_io::with_path(path));
                self.install_highlighter(pushed);
                self.install_lsp_client(pushed);
                info!(buf = ?pushed, "edit_buf: created new buffer");
                pushed
            }
        };
        self.surface.windows.set_focused_buf(id);
        id
    }

    /// Write the focused buffer to `path` (or its own file path). A failed
    /// write is a user-facing event, not a process error: it's surfaced via
    /// notify + journal and the editor keeps running.
    #[instrument(skip(self))]
    pub(super) fn write_buf(&mut self, path: Option<Rc<Path>>) {
        let editor = self.surface.windows.focused_buf();
        match buffer_io::write(&mut self.bufs[editor], path) {
            Ok(()) => info!(buf = ?editor, "wrote buffer"),
            Err(e) => {
                tracing::error!(buf = ?editor, error = %e, "write_buf failed");
                self.notify_via_lisp(&format!("write failed: {e}"));
            }
        }
    }

    #[instrument(skip(self))]
    pub(super) fn delete_buf(&mut self, buf: BufferId) {
        if !self.bufs.contains(buf) {
            warn!(?buf, "delete_buf: skipping (unknown id)");
            return;
        }
        if !self.bufs.is_file_buf(buf) {
            warn!(?buf, "delete_buf: skipping (not a file buffer)");
            return;
        }

        if self.bufs.file_buf_count() == 1 {
            debug!(?buf, "delete_buf: last file buffer -> resetting");
            self.bufs.reset(buf);
            self.surface.windows.for_each_leaf_mut(|b| *b = buf);
            return;
        }

        self.bufs.remove(buf);
        let first = self.bufs.first_file_buf();
        self.surface.windows.for_each_leaf_mut(|b| {
            if *b == buf {
                *b = first;
            }
        });
        info!(?buf, "deleted buffer");
    }

    pub(super) fn window_split(&mut self, dir: SplitDir) {
        let new_buf = self.bufs.push_file(Buffer::new());
        self.surface.windows.split(dir, new_buf);
        info!(?dir, ?new_buf, "window split");
    }

    pub(super) fn window_close(&mut self) {
        debug!("closing focused window");
        self.surface.windows.close_focused();
    }

    pub(super) fn cycle_buffer(&mut self, dir: CycleDir) {
        if let Some(id) = self.bufs.cycle(self.surface.windows.focused_buf(), dir) {
            debug!(?dir, buf = ?id, "cycled buffer");
            self.surface.windows.set_focused_buf(id);
        } else {
            trace!(?dir, "cycle_buffer: no cycle (single file buffer)");
        }
    }
}
