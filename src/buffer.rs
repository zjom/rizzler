use rizz_changetree::{ChangeTree, Delta};
use ropey::{Rope, RopeSlice, iter::Lines};
use std::{path::Path, rc::Rc, str::FromStr};

use crate::{
    mode::EditingMode,
    position::Position,
    ui::{
        props::PropStore,
        wrap::{WrapMap, WrapMode},
    },
};

/// What sort of buffer this is. Drives default mode and gates operations like
/// BufDelete/BufNext — the minibuffer participates in everything a file
/// buffer does but is excluded from user-visible buffer cycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BufferKind {
    #[default]
    File,
    Minibuffer,
    /// Backing buffer of a [`crate::ui::popup::Popup`]. Excluded from
    /// user-visible buffer cycling and from `BufDelete`.
    Popup,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum MoveKind {
    LineStart,
    /// First non-blank character on the current line (vim `^`).
    LineFirstNonBlank,
    LineEnd,
    FileStart,
    FileEnd,
    /// Vim `b` — start of the word at/before the cursor. Word chars and
    /// punctuation form separate words; traverses newlines as whitespace.
    WordStart,
    /// Vim `w` — start of the next word. Word chars and punctuation form
    /// separate words; traverses newlines as whitespace.
    WordForward,
    /// Vim `e` — end of the word at/after the cursor. Word chars and
    /// punctuation form separate words; traverses newlines as whitespace.
    WordEnd,
    /// Vim `ge` — end of the previous word. Word chars and punctuation form
    /// separate words; traverses newlines as whitespace.
    WordBackEnd,
    /// Vim `B` — start of the WORD at/before the cursor. Whitespace is the
    /// only separator; traverses newlines.
    BigWordStart,
    /// Vim `W` — start of the next WORD. Whitespace is the only separator;
    /// traverses newlines.
    BigWordForward,
    /// Vim `E` — end of the WORD at/after the cursor. Whitespace is the only
    /// separator; traverses newlines.
    BigWordEnd,
    /// Vim `gE` — end of the previous WORD. Whitespace is the only
    /// separator; traverses newlines.
    BigWordBackEnd,
    Relative(Position<i16>),   // up, down, left, right of cursor
    Absolute(Position<usize>), // position in file
    LineNum(usize),
    HalfPageDown,
    HalfPageUp,
    /// Vim's `zz` — re-center the viewport on the cursor without moving it.
    Center,
}

impl FromStr for MoveKind {
    type Err = &'static str;
    fn from_str(sym: &str) -> Result<Self, Self::Err> {
        use MoveKind as M;
        Ok(match sym {
            "down" => M::Relative(Position::new(0, 1)),
            "up" => M::Relative(Position::new(0, -1)),
            "left" => M::Relative(Position::new(-1, 0)),
            "right" => M::Relative(Position::new(1, 0)),
            "line-start" => M::LineStart,
            "line-first-non-blank" => M::LineFirstNonBlank,
            "line-end" => M::LineEnd,
            "file-start" => M::FileStart,
            "file-end" => M::FileEnd,
            "word-start" => M::WordStart,
            "word-forward" => M::WordForward,
            "word-end" => M::WordEnd,
            "word-back-end" => M::WordBackEnd,
            "big-word-start" => M::BigWordStart,
            "big-word-forward" => M::BigWordForward,
            "big-word-end" => M::BigWordEnd,
            "big-word-back-end" => M::BigWordBackEnd,
            "half-page-down" => M::HalfPageDown,
            "half-page-up" => M::HalfPageUp,
            "center" => M::Center,
            _ => return Err("unknown MoveKind"),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Buffer {
    pub(crate) buf: Rope,
    pub(crate) cursor_pos: Position<u16>,
    pub(crate) file_pos: Position<usize>,
    pub(crate) fs_path: Option<Rc<Path>>,
    /// Visible viewport size in cells. When `viewport.row > 0`, cursor
    /// movement scrolls `file_pos` to keep the cursor in view. Default zero
    /// means "no viewport" — scrolling is a no-op (useful in tests).
    pub(crate) viewport: Position<u16>,
    pub(crate) kind: BufferKind,
    pub(crate) mode: EditingMode,
    /// Stack of additional keymap modes layered on top of `mode`. Used to
    /// give a buffer extra named modes (e.g. a popup buffer activating
    /// `"popup"` and `"popup.files"`) without losing its base editing mode.
    /// Last element is the most recently pushed and shadows earlier layers
    /// during keymap resolution.
    pub(crate) mode_layers: Vec<Rc<str>>,
    /// Anchor (absolute file position) of the current visual selection.
    /// `Some` iff `mode` is one of the visual modes — managed by `set_mode`.
    pub(crate) selection_anchor: Option<Position<usize>>,
    /// Text properties and overlays. Built up by lisp via
    /// `put-text-property` / `overlay-create`; consumed by the precompute
    /// pass to emit decorator ranges.
    pub(crate) props: PropStore,
    /// Soft-wrap configuration. When `wrap.mode` is non-`None`, the
    /// precompute pass builds a `WrapMap` for this buffer and the renderer
    /// emits one visual row per `WrapMap` entry.
    pub(crate) wrap: crate::ui::wrap::WrapSettings,
    /// Cached visual-line layout from the most recent render. Movement code
    /// reads it to step in visual rows; `None` means "no recent render" or
    /// "wrap is off" — fall back to file-row movement.
    ///
    /// Edits (insert/delete) invalidate this; the next render rebuilds it.
    pub(crate) wrap_cache: Option<WrapMap>,
    pub(crate) changetree: ChangeTree,
}

impl Buffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct the editor's minibuffer — single-line, starts in Command mode,
    /// used as the destination for `:`-style command input.
    pub fn minibuffer() -> Self {
        Self {
            kind: BufferKind::Minibuffer,
            mode: EditingMode::Command,
            ..Self::default()
        }
    }

    /// Construct a popup's backing buffer. Same as a default buffer, just
    /// tagged so cycling/deletion code can skip it.
    pub fn popup() -> Self {
        Self {
            kind: BufferKind::Popup,
            ..Self::default()
        }
    }

    pub fn fs_path(&self) -> Option<Rc<Path>> {
        self.fs_path.clone()
    }

    pub fn props(&self) -> &PropStore {
        &self.props
    }

    pub fn props_mut(&mut self) -> &mut PropStore {
        &mut self.props
    }

    pub fn kind(&self) -> BufferKind {
        self.kind
    }

    pub fn mode(&self) -> EditingMode {
        self.mode
    }

    /// Push `name` to the top of the keymap mode stack. Idempotent: if
    /// already present, the existing entry is removed first so the layer
    /// ends up at the top.
    pub fn push_mode_layer(&mut self, name: Rc<str>) {
        self.mode_layers.retain(|m| m.as_ref() != name.as_ref());
        self.mode_layers.push(name);
    }

    /// Remove `name` from the mode stack. No-op when absent.
    pub fn remove_mode_layer(&mut self, name: &str) {
        self.mode_layers.retain(|m| m.as_ref() != name);
    }

    pub fn mode_layers(&self) -> &[Rc<str>] {
        &self.mode_layers
    }

    /// Active modes for keymap resolution, most-specific first. Stacked
    /// layers (most recent first) precede the buffer's base editing mode.
    pub fn active_modes(&self) -> Vec<Rc<str>> {
        let mut v: Vec<Rc<str>> = self.mode_layers.iter().rev().cloned().collect();
        v.push(self.mode.as_str().into());
        v
    }

    pub fn wrap_mode(&self) -> WrapMode {
        self.wrap.mode
    }

    pub fn set_wrap_mode(&mut self, m: WrapMode) {
        self.wrap.mode = m;
    }

    pub fn wrap_column(&self) -> Option<u16> {
        self.wrap.column
    }

    pub fn set_wrap_column(&mut self, col: Option<u16>) {
        self.wrap.column = col;
    }

    pub fn breakindent(&self) -> bool {
        self.wrap.breakindent
    }

    pub fn set_breakindent(&mut self, b: bool) {
        self.wrap.breakindent = b;
    }

    /// Drop the cached visual-line layout. Called by every edit so the next
    /// render builds a fresh map.
    pub(crate) fn invalidate_wrap_cache(&mut self) {
        self.wrap_cache = None;
    }

    /// Most recent visual-line layout (from the last render's precompute
    /// pass). Movement and scroll code reads this when wrap is on.
    pub fn wrap_cache(&self) -> Option<&WrapMap> {
        self.wrap_cache.as_ref()
    }

    pub(crate) fn set_wrap_cache(&mut self, map: Option<WrapMap>) {
        self.wrap_cache = map;
    }

    pub fn set_mode(&mut self, mode: EditingMode) {
        let was_visual = self.mode.is_visual();
        let is_visual = mode.is_visual();
        if is_visual && !was_visual {
            self.selection_anchor = Some(self.abs_pos());
        } else if !is_visual {
            self.selection_anchor = None;
        }
        self.mode = mode;
    }

    /// Anchor of the current visual selection (absolute file position).
    pub fn selection_anchor(&self) -> Option<Position<usize>> {
        self.selection_anchor
    }

    /// Text covered by the current visual selection. Inclusive on both ends;
    /// `VisualLine` includes the trailing newline of the last selected row,
    /// and `VisualBlock` joins each row's column slice with `\n`.
    pub fn selected_text(&self) -> Option<String> {
        let anchor = self.selection_anchor?;
        crate::selection::selected_text(&self.buf, self.mode, anchor, self.abs_pos())
    }

    /// Cursor's absolute file position (file_pos + cursor_pos).
    pub fn abs_pos(&self) -> Position<usize> {
        Position::new(
            self.file_pos.col + self.cursor_pos.col as usize,
            self.file_pos.row + self.cursor_pos.row as usize,
        )
    }

    /// Cursor's absolute file row — `file_pos.row + cursor_pos.row`.
    pub fn abs_row(&self) -> usize {
        self.file_pos.row + self.cursor_pos.row as usize
    }

    /// Cursor's absolute file column — `file_pos.col + cursor_pos.col`.
    pub fn abs_col(&self) -> usize {
        self.file_pos.col + self.cursor_pos.col as usize
    }

    /// Reset rope content and cursor — used when the minibuffer finishes
    /// processing a command and needs to be empty again.
    pub fn clear(&mut self) {
        self.buf = Rope::new();
        self.cursor_pos = Position::default();
        self.file_pos = Position::default();
        self.invalidate_wrap_cache();
    }

    pub fn clear_with(&mut self, text: &str) {
        self.buf = Rope::from_str(text);
        self.invalidate_wrap_cache();
        self.clamp_cursor();
    }

    /// Owned snapshot of the rope text — used by command parsing.
    pub fn text(&self) -> String {
        self.buf.to_string()
    }

    pub fn cursor_pos(&self) -> Position<u16> {
        self.cursor_pos
    }

    pub fn file_pos(&self) -> Position<usize> {
        self.file_pos
    }

    pub fn len_lines(&self) -> usize {
        self.buf.len_lines()
    }

    pub fn lines_at(&self, idx: usize) -> Lines<'_> {
        self.buf.lines_at(idx)
    }

    pub fn insert_char(&mut self, c: char) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        let start_line = self.abs_row();
        let abs_before = self.abs_pos();
        let before = snapshot_lines(&self.buf, start_line, 1);

        self.buf.insert_char(cidx, c);
        self.invalidate_wrap_cache();

        if c == '\n' {
            self.cursor_pos.row = self.cursor_pos.row.saturating_add(1);
            self.cursor_pos.col = 0;
        } else {
            self.cursor_pos.col = self.cursor_pos.col.saturating_add(1);
        }

        let after_lines = if c == '\n' { 2 } else { 1 };
        let after = snapshot_lines(&self.buf, start_line, after_lines);
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            start_line,
            before: before.into(),
            after: after.into(),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
    }

    pub fn delete_char(&mut self) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        if cidx == 0 {
            return;
        }

        // Snapshot before any rope mutation. Joining lines spans two lines;
        // an in-line delete spans one.
        let removed_is_nl = matches!(self.buf.get_char(cidx - 1), Some('\n'));
        let start_line = if removed_is_nl {
            self.abs_row().saturating_sub(1)
        } else {
            self.abs_row()
        };
        let before_lines = if removed_is_nl { 2 } else { 1 };
        let before = snapshot_lines(&self.buf, start_line, before_lines);
        let abs_before = self.abs_pos();

        match self.buf.get_char(cidx - 1) {
            Some('\n') => {
                self.cursor_pos.row = self.cursor_pos.row.saturating_sub(1);
                // length of the previous line *without* its trailing newline
                self.cursor_pos.col = self.cur_line().len_chars().saturating_sub(1) as u16;
            }
            Some(_) => self.cursor_pos.col = self.cursor_pos.col.saturating_sub(1),
            None => return,
        };

        _ = self.buf.try_remove(cidx - 1..cidx);
        self.invalidate_wrap_cache();

        let after = snapshot_lines(&self.buf, start_line, 1);
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            start_line,
            before: before.into(),
            after: after.into(),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
    }

    pub fn delete_char_at(&mut self, Position { col, row }: Position<usize>) {
        if row >= self.buf.len_lines() {
            return;
        }
        let line_start = self.buf.line_to_char(row);
        let mut line_len = self.buf.line(row).len_chars();
        if line_len > 0 && self.buf.char(line_start + line_len - 1) == '\n' {
            line_len -= 1;
        }
        if col >= line_len {
            return;
        }
        let before = snapshot_lines(&self.buf, row, 1);
        let abs_before = self.abs_pos();
        let cidx = line_start + col;
        _ = self.buf.try_remove(cidx..cidx + 1);
        self.invalidate_wrap_cache();
        self.clamp_cursor();
        let after = snapshot_lines(&self.buf, row, 1);
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            start_line: row,
            before: before.into(),
            after: after.into(),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
    }

    /// Reverse the most recent tracked edit and return whether anything
    /// happened. Cursor lands where it was just before that edit.
    pub fn undo(&mut self) -> bool {
        let Some(delta) = self.changetree.undo() else {
            return false;
        };
        let after_lines = rope_line_count(&delta.after);
        replace_lines(&mut self.buf, delta.start_line, after_lines, &delta.before);
        self.invalidate_wrap_cache();
        let (row, col) = delta.cursor_before;
        self.land_cursor_at(row, col);
        true
    }

    /// Reapply the most recently undone edit, if any. Cursor lands where it
    /// ended up the first time around.
    pub fn redo(&mut self) -> bool {
        let Some(delta) = self.changetree.redo() else {
            return false;
        };
        let before_lines = rope_line_count(&delta.before);
        replace_lines(&mut self.buf, delta.start_line, before_lines, &delta.after);
        self.invalidate_wrap_cache();
        let (row, col) = delta.cursor_after;
        self.land_cursor_at(row, col);
        true
    }

    fn land_cursor_at(&mut self, row: usize, col: usize) {
        let row = row.min(self.buf.len_lines().saturating_sub(1));
        if row < self.file_pos.row {
            self.file_pos.row = row;
        }
        self.cursor_pos.row = (row - self.file_pos.row) as u16;
        if col < self.file_pos.col {
            self.file_pos.col = col;
        }
        self.cursor_pos.col = (col - self.file_pos.col) as u16;
        self.clamp_cursor();
    }

    /// Apply `m` `count` times. For [`MoveKind::Relative`] the count
    /// multiplies the delta in one shot; for every other variant we just
    /// loop. `count == 0` is treated as 1 so a bare bind without a numeric
    /// prefix still works.
    pub fn move_cursor_n(&mut self, m: MoveKind, count: u32) {
        let n = count.max(1);
        if let MoveKind::Relative(Position { col, row }) = m {
            let scaled = MoveKind::Relative(Position::new(
                (col as i32)
                    .saturating_mul(n as i32)
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (row as i32)
                    .saturating_mul(n as i32)
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            ));
            self.move_cursor(scaled);
            return;
        }
        for _ in 0..n {
            self.move_cursor(m);
        }
    }

    pub fn move_cursor(&mut self, m: MoveKind) {
        use MoveKind as MK;
        match m {
            MK::Relative(Position { col: dx, row: dy }) => {
                let abs = self.abs_pos();

                // Wrap-aware vertical step when the cache has us covered;
                // otherwise fall through to file-row math (the next render
                // will rebuild the cache around the new scroll position).
                let visual_target = if dy != 0 {
                    crate::ui::scroll::visual_step(self.wrap_cache.as_ref(), abs.row, abs.col, dy)
                } else {
                    None
                };

                // Compute the absolute target. We can't use saturating_add_signed
                // on cursor_pos directly because clamping to u16::0 would erase
                // any "wanted to scroll up by N" overshoot; clamp_cursor then
                // could never observe the up-scroll intent.
                let (abs_row, abs_col) = match visual_target {
                    Some((r, c)) => {
                        // Apply any horizontal delta on top of the visual landing.
                        let c = (c as isize + dx as isize).max(0) as usize;
                        (r, c)
                    }
                    None => {
                        let r = (self.cursor_pos.row as isize)
                            .saturating_add(self.file_pos.row as isize)
                            .saturating_add(dy as isize)
                            .max(0) as usize;
                        let c = (self.cursor_pos.col as isize)
                            .saturating_add(self.file_pos.col as isize)
                            .saturating_add(dx as isize)
                            .max(0) as usize;
                        (r, c)
                    }
                };

                // Up/left scrolling lives here; down/right scrolling is left
                // to clamp_cursor, which also knows the viewport bounds.
                if abs_row < self.file_pos.row {
                    self.file_pos.row = abs_row;
                }
                self.cursor_pos.row = (abs_row - self.file_pos.row) as u16;
                if abs_col < self.file_pos.col {
                    self.file_pos.col = abs_col;
                }
                self.cursor_pos.col = (abs_col - self.file_pos.col) as u16;
            }
            MK::LineStart => {
                self.cursor_pos.col = 0;
            }
            MK::LineFirstNonBlank => {
                let line = self.cur_line();
                let len = line.len_chars();
                let effective = if len > 0 && line.char(len - 1) == '\n' {
                    len - 1
                } else {
                    len
                };
                let mut i = 0;
                while i < effective && line.char(i).is_ascii_whitespace() {
                    i += 1;
                }
                // All-blank line: stay at col 0, matching vim's `^`.
                self.cursor_pos.col = if i == effective { 0 } else { i as u16 };
            }
            MK::LineEnd => self.cursor_pos.col = self.cur_line().len_chars() as u16,
            MK::FileStart => {
                self.cursor_pos = Position::default();
                self.file_pos = Position::default();
            }
            MK::FileEnd => {
                let last_line = self.buf.len_lines().saturating_sub(1);
                self.file_pos.row = 0;
                self.cursor_pos.row = last_line as u16;
            }
            MK::WordStart => self.apply_motion(crate::motions::word_back_start, false),
            MK::WordForward => self.apply_motion(crate::motions::word_forward, false),
            MK::WordEnd => self.apply_motion(crate::motions::word_end, false),
            MK::WordBackEnd => self.apply_motion(crate::motions::word_back_end, false),
            MK::BigWordStart => self.apply_motion(crate::motions::word_back_start, true),
            MK::BigWordForward => self.apply_motion(crate::motions::word_forward, true),
            MK::BigWordEnd => self.apply_motion(crate::motions::word_end, true),
            MK::BigWordBackEnd => self.apply_motion(crate::motions::word_back_end, true),
            MK::Absolute(Position { row, col }) => {
                self.file_pos = Position::new(col, row);
                self.cursor_pos = Position::default();
            }
            MK::LineNum(n) => {
                let last_line = self.buf.len_lines().saturating_sub(1);
                self.file_pos.row = 0;
                self.cursor_pos.row = n.min(last_line) as u16;
            }
            MK::HalfPageDown => self.half_page(1),
            MK::HalfPageUp => self.half_page(-1),
            MK::Center => {
                let abs_row = self.abs_row();
                self.center_on(abs_row);
            }
        }

        self.clamp_cursor();
    }

    /// Move the cursor by half the viewport height and re-center the
    /// viewport on the new cursor row (matches vim's C-d / C-u + zz fusion).
    /// "Half" is counted in *visual* rows when wrap is on so half-page over
    /// a tall wrapped paragraph stays inside the paragraph.
    fn half_page(&mut self, direction: i16) {
        if self.viewport.row == 0 {
            return;
        }
        let abs = self.abs_pos();
        let (tgt_row, tgt_col) = crate::ui::scroll::half_page_target(
            self.viewport.row,
            self.wrap_cache.as_ref(),
            abs.row,
            abs.col,
            direction,
        );
        self.center_on(tgt_row);
        if let Some(col) = tgt_col {
            if col < self.file_pos.col {
                self.file_pos.col = col;
            }
            self.cursor_pos.col = (col - self.file_pos.col) as u16;
        }
    }

    /// Place `abs_row` at the vertical middle of the viewport. clamp_cursor
    /// applies the EOF cap and per-line column clamping afterwards.
    fn center_on(&mut self, abs_row: usize) {
        if self.viewport.row == 0 {
            return;
        }
        self.file_pos.row = crate::ui::scroll::centered_top(self.viewport.row, abs_row);
        self.cursor_pos.row = (abs_row - self.file_pos.row) as u16;
    }

    pub fn clamp_cursor(&mut self) {
        let last_line = self.buf.len_lines().saturating_sub(1);
        let abs_row = self.abs_row().min(last_line);

        // Vertical scroll. Skipped when viewport.row is 0 (e.g. tests without
        // a known terminal size) so pre-scroll behaviour is preserved.
        if self.viewport.row > 0 {
            let abs_col_now = self.abs_col();
            self.file_pos.row = crate::ui::scroll::clamp_scroll_top(
                self.viewport.row,
                self.wrap_cache.as_ref(),
                self.file_pos.row,
                abs_row,
                abs_col_now,
                last_line,
            );
        }
        self.cursor_pos.row = abs_row.saturating_sub(self.file_pos.row) as u16;

        let line = self.buf.line(abs_row);
        let len = line.len_chars();
        let has_trailing_nl = len > 0 && line.char(len - 1) == '\n';
        // Number of non-newline chars on this line.
        let chars = if has_trailing_nl { len - 1 } else { len };
        // In Normal mode the cursor sits ON a character, so it cannot move past
        // the last non-newline char. In all other modes it may sit just after.
        let max_col = match self.mode {
            EditingMode::Normal => chars.saturating_sub(1),
            EditingMode::Insert
            | EditingMode::Command
            | EditingMode::Visual
            | EditingMode::VisualLine
            | EditingMode::VisualBlock => chars,
        };
        let abs_col = self.abs_col().min(max_col);
        self.cursor_pos.col = abs_col.saturating_sub(self.file_pos.col) as u16;
    }

    /// Resolve `motion` against the rope from the cursor's current absolute
    /// char index and move there. Pairs with [`crate::motions`] free fns.
    fn apply_motion(&mut self, motion: fn(&Rope, usize, bool) -> usize, big: bool) {
        let abs = self.abs_pos();
        let cidx = self.buf.line_to_char(abs.row) + abs.col;
        let new = motion(&self.buf, cidx, big);
        self.set_abs_char(new);
    }

    /// Place the cursor at absolute char index `cidx` in the rope. Adjusts
    /// `file_pos` upward when needed; `clamp_cursor` (run by the caller)
    /// handles down/right scrolling.
    fn set_abs_char(&mut self, cidx: usize) {
        let row = self.buf.char_to_line(cidx);
        let col = cidx - self.buf.line_to_char(row);
        if row < self.file_pos.row {
            self.file_pos.row = row;
        }
        self.cursor_pos.row = (row - self.file_pos.row) as u16;
        if col < self.file_pos.col {
            self.file_pos.col = col;
        }
        self.cursor_pos.col = (col - self.file_pos.col) as u16;
    }

    fn cur_line(&self) -> RopeSlice<'_> {
        self.buf.line(self.cur_lnum())
    }

    fn cur_lnum(&self) -> usize {
        self.abs_row()
    }

    fn cur_line_start(&self) -> usize {
        self.buf.line_to_char(self.cur_lnum())
    }
}

/// Read lines `[start..start+n_lines)` as a single string. Caps at EOF so the
/// snapshot is well-defined even when the requested range overshoots.
fn snapshot_lines(rope: &Rope, start: usize, n_lines: usize) -> String {
    let total = rope.len_lines();
    if start >= total {
        return String::new();
    }
    let start_char = rope.line_to_char(start);
    let end_line = (start + n_lines).min(total);
    let end_char = if end_line >= total {
        rope.len_chars()
    } else {
        rope.line_to_char(end_line)
    };
    rope.slice(start_char..end_char).to_string()
}

/// Replace lines `[start..start+n_lines)` with `text`. Used by undo/redo to
/// swap in the opposite snapshot of a recorded delta.
fn replace_lines(rope: &mut Rope, start: usize, n_lines: usize, text: &str) {
    let total = rope.len_lines();
    let start_char = if start >= total {
        rope.len_chars()
    } else {
        rope.line_to_char(start)
    };
    let end_line = (start + n_lines).min(total);
    let end_char = if end_line >= total {
        rope.len_chars()
    } else {
        rope.line_to_char(end_line)
    };
    rope.remove(start_char..end_char);
    rope.insert(start_char, text);
}

/// Count the number of ropey-style lines the snapshot occupies. A trailing
/// `\n` closes the final line; otherwise the dangling content counts as one
/// more line.
fn rope_line_count(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    let nls = s.bytes().filter(|b| *b == b'\n').count();
    if s.ends_with('\n') { nls } else { nls + 1 }
}

impl FromStr for Buffer {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            buf: Rope::from_str(s),
            ..Self::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(text: &str) -> Buffer {
        Buffer::from_str(text).expect("never fails")
    }

    fn cur_row(s: &Buffer) -> usize {
        s.cursor_pos.row as usize + s.file_pos.row
    }

    fn cur_col(s: &Buffer) -> usize {
        s.cursor_pos.col as usize + s.file_pos.col
    }

    // ---- insert_char --------------------------------------------------

    #[test]
    fn insert_into_empty_buffer() {
        let mut s = mk("");
        s.insert_char('a');
        assert_eq!(s.buf.to_string(), "a");
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 1);
    }

    #[test]
    fn insert_on_second_line_uses_correct_offset() {
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(1, 1); // between 'c' and 'd'
        s.insert_char('X');
        assert_eq!(s.buf.to_string(), "ab\ncXd");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2);
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut s = mk("abcd");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('\n');
        assert_eq!(s.buf.to_string(), "ab\ncd");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn insert_at_end_of_buffer() {
        let mut s = mk("ab");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('c');
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(s.cursor_pos.col, 3);
    }

    // ---- delete_char --------------------------------------------------

    #[test]
    fn delete_at_file_start_is_noop() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.delete_char();
        assert_eq!(s.buf.to_string(), "hello");
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn delete_char_in_middle() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(3, 0);
        s.delete_char();
        assert_eq!(s.buf.to_string(), "helo");
        assert_eq!(s.cursor_pos.col, 2);
    }

    #[test]
    fn delete_only_character() {
        let mut s = mk("a");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.delete_char();
        assert_eq!(s.buf.to_string(), "");
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn delete_newline_at_line_start() {
        let mut s = mk("ab\ncd\nef");
        s.cursor_pos = Position::<u16>::new(0, 2); // start of "ef"
        s.delete_char();
        assert_eq!(s.buf.to_string(), "ab\ncdef");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2); // after "cd"
    }

    // ---- move_cursor: LineStart / LineEnd -----------------------------

    #[test]
    fn line_start_moves_to_col_zero() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(3, 1);
        s.move_cursor(MoveKind::LineStart);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn line_end_does_not_land_on_newline() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 3); // just past 'c', not on '\n'
    }
    #[test]
    fn line_end_lands_on_last_char_in_normal_mode() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Normal;
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 2); // on 'c'
    }

    #[test]
    fn line_end_on_last_line_without_newline() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2); // on 'f' (normal mode)
    }

    // ---- move_cursor: FileStart / FileEnd -----------------------------

    #[test]
    fn file_start_resets_row_and_col() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(3, 1);
        s.move_cursor(MoveKind::FileStart);
        assert_eq!(cur_row(&s), 0);
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn file_end_moves_to_last_line() {
        let mut s = mk("a\nb\nc"); // 3 lines, last line index = 2
        s.move_cursor(MoveKind::FileEnd);
        assert_eq!(cur_row(&s), 2);
    }

    // ---- move_cursor: LineNum -----------------------------------------

    #[test]
    fn line_num_moves_to_specified_line() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.move_cursor(MoveKind::LineNum(2));
        assert_eq!(cur_row(&s), 2);
    }

    #[test]
    fn line_num_zero_moves_to_first_line() {
        let mut s = mk("a\nb\nc");
        s.cursor_pos = Position::<u16>::new(0, 2);
        s.move_cursor(MoveKind::LineNum(0));
        assert_eq!(cur_row(&s), 0);
    }

    #[test]
    fn line_num_clamps_to_last_line() {
        let mut s = mk("a\nb\nc"); // valid line indices: 0, 1, 2
        s.move_cursor(MoveKind::LineNum(100));
        assert_eq!(cur_row(&s), 2);
    }

    // ---- move_cursor: WordStart / WordEnd -----------------------------

    #[test]
    fn word_end_lands_on_last_char_of_word() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 4); // 'o' of "hello"
    }

    #[test]
    fn word_end_from_middle_of_word() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 4); // 'o' of "hello"
    }

    #[test]
    fn word_end_jumps_to_next_word_when_already_at_end() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(4, 0); // 'o' of "hello"
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 10); // 'd' of "world"
    }

    #[test]
    fn word_start_goes_to_previous_word_start() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(8, 0); // 'r' of "world"
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 6); // 'w' of "world"
    }

    #[test]
    fn word_start_from_word_start_goes_back_one_word() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(6, 0); // 'w' of "world"
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 0); // 'h' of "hello"
    }

    // ---- move_cursor: WordForward (w) ---------------------------------

    #[test]
    fn word_forward_jumps_to_next_word_start() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 6); // 'w' of "world"
    }

    #[test]
    fn word_forward_from_middle_of_word_lands_on_next_word() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(2, 0); // 'l' of "hello"
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 6); // 'w' of "world"
    }

    #[test]
    fn word_forward_skips_over_multiple_spaces() {
        let mut s = mk("a    b");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 5); // 'b'
    }

    // ---- move_cursor: WordBackEnd (ge) --------------------------------

    #[test]
    fn word_back_end_from_inside_word_lands_on_prev_word_end() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(8, 0); // 'r' of "world"
        s.move_cursor(MoveKind::WordBackEnd);
        assert_eq!(s.cursor_pos.col, 4); // 'o' of "hello"
    }

    #[test]
    fn word_back_end_from_whitespace_lands_on_prev_word_end() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(5, 0); // space
        s.move_cursor(MoveKind::WordBackEnd);
        assert_eq!(s.cursor_pos.col, 4); // 'o' of "hello"
    }

    #[test]
    fn word_back_end_at_line_start_stays_put() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordBackEnd);
        assert_eq!(s.cursor_pos.col, 0);
    }

    // ---- vim-style w/b/e: punctuation as a separate word ---------------

    #[test]
    fn word_forward_splits_on_punctuation() {
        let mut s = mk("foo.bar");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 3); // '.'
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 4); // 'b'
    }

    #[test]
    fn big_word_forward_treats_punctuation_as_word_char() {
        let mut s = mk("foo.bar baz");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::BigWordForward);
        assert_eq!(s.cursor_pos.col, 8); // 'b' of "baz"
    }

    #[test]
    fn word_end_splits_on_punctuation() {
        let mut s = mk("foo.bar");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 2); // last 'o' of "foo"
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 3); // '.'
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 6); // 'r' of "bar"
    }

    #[test]
    fn word_start_splits_on_punctuation() {
        let mut s = mk("foo.bar");
        s.cursor_pos = Position::<u16>::new(6, 0); // 'r'
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 4); // 'b'
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 3); // '.'
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 0); // 'f'
    }

    // ---- vim-style w/b/e: cross-line traversal -------------------------

    #[test]
    fn word_forward_crosses_newline_to_next_line() {
        let mut s = mk("foo\nbar");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 0); // 'b' of "bar"
    }

    #[test]
    fn word_end_crosses_newline_to_next_line() {
        let mut s = mk("foo\nbar");
        s.cursor_pos = Position::<u16>::new(2, 0); // 'o', end of "foo"
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2); // 'r' of "bar"
    }

    #[test]
    fn word_back_start_crosses_newline_to_prev_line() {
        let mut s = mk("foo\nbar");
        s.cursor_pos = Position::<u16>::new(0, 1); // 'b' of "bar"
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0); // 'f' of "foo"
    }

    #[test]
    fn word_back_end_crosses_newline_to_prev_line() {
        let mut s = mk("foo\nbar");
        s.cursor_pos = Position::<u16>::new(0, 1); // 'b' of "bar"
        s.move_cursor(MoveKind::WordBackEnd);
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 2); // 'o', end of "foo"
    }

    // ---- move_cursor: LineFirstNonBlank (^) ---------------------------

    #[test]
    fn line_first_non_blank_skips_leading_whitespace() {
        let mut s = mk("    hello");
        s.cursor_pos = Position::<u16>::new(7, 0);
        s.move_cursor(MoveKind::LineFirstNonBlank);
        assert_eq!(s.cursor_pos.col, 4); // 'h'
    }

    #[test]
    fn line_first_non_blank_on_blank_line_stays_at_zero() {
        let mut s = mk("   \nabc");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.move_cursor(MoveKind::LineFirstNonBlank);
        assert_eq!(s.cursor_pos.col, 0);
    }

    // ---- move_cursor_n (count) ----------------------------------------

    #[test]
    fn move_cursor_n_scales_relative_delta() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor_n(MoveKind::Relative(Position::new(0, 1)), 3);
        assert_eq!(cur_row(&s), 3);
    }

    #[test]
    fn move_cursor_n_loops_for_non_relative_kinds() {
        let mut s = mk("aaa bbb ccc ddd");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor_n(MoveKind::WordForward, 2);
        assert_eq!(s.cursor_pos.col, 8); // 'c' of "ccc"
    }

    #[test]
    fn move_cursor_n_count_zero_runs_once() {
        let mut s = mk("a\nb\nc");
        s.move_cursor_n(MoveKind::Relative(Position::new(0, 1)), 0);
        assert_eq!(cur_row(&s), 1);
    }

    // ---- move_cursor: Relative / Absolute -----------------------------

    #[test]
    fn relative_move_within_bounds() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.move_cursor(MoveKind::Relative(Position::new(1, 1)));
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 3);
    }

    #[test]
    fn relative_move_clamped_at_top_left() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::Relative(Position::new(-5, -5)));
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn absolute_moves_to_file_position() {
        let mut s = mk("aaaa\nbbbb\ncccc\ndddd\neeee");
        s.cursor_pos = Position::<u16>::new(3, 2);
        s.move_cursor(MoveKind::Absolute(Position::new(0, 0)));
        assert_eq!(cur_row(&s), 0);
        assert_eq!(cur_col(&s), 0);
    }

    // ---- clamp_cursor / cur_line --------------------------------------

    #[test]
    fn cur_line_returns_the_right_line() {
        let mut s = mk("ab\ncd\nef");
        s.cursor_pos = Position::<u16>::new(0, 2);
        assert_eq!(s.cur_line().to_string(), "ef");
    }

    #[test]
    fn clamp_keeps_cursor_in_buffer() {
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(50, 50); // way out of bounds
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 1); // last line
        assert_eq!(s.cursor_pos.col, 1); // on 'd' (normal mode clamps to last char)
    }

    #[test]
    fn clamp_on_empty_buffer() {
        let mut s = mk("");
        s.cursor_pos = Position::<u16>::new(10, 10);
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn clamp_does_not_allow_landing_on_newline() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(10, 0);
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 3); // past 'c', not on '\n'
    }

    // ---- vertical scrolling -------------------------------------------

    #[test]
    fn move_down_within_viewport_does_not_scroll() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 3;
        s.move_cursor(MoveKind::Relative(Position::new(0, 2)));
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(s.file_pos.row, 0);
    }

    #[test]
    fn move_down_past_viewport_scrolls_file_pos() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 3;
        s.move_cursor(MoveKind::Relative(Position::new(0, 4)));
        // Absolute row 4 should sit at the last visible viewport row.
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(cur_row(&s), 4);
    }

    #[test]
    fn move_up_past_viewport_scrolls_file_pos() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 2;
        s.file_pos.row = 3;
        s.cursor_pos = Position::<u16>::new(0, 0); // cursor at abs row 3
        s.move_cursor(MoveKind::Relative(Position::new(0, -1))); // → abs row 2
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(s.cursor_pos.row, 0);
    }

    #[test]
    fn file_end_scrolls_to_bottom() {
        let mut s = mk("a\nb\nc\nd\ne"); // 5 lines, last_line = 4
        s.viewport.row = 2;
        s.move_cursor(MoveKind::FileEnd);
        // Cursor on abs row 4, viewport size 2 → top row is 3.
        assert_eq!(s.file_pos.row, 3);
        assert_eq!(s.cursor_pos.row, 1);
    }

    #[test]
    fn file_start_resets_scroll() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 2;
        s.file_pos.row = 3;
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::FileStart);
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }

    #[test]
    fn relative_up_clamps_at_top_when_already_at_origin() {
        // Cursor at top of file, no scroll possible — stays put.
        let mut s = mk("a\nb\nc");
        s.viewport.row = 2;
        s.move_cursor(MoveKind::Relative(Position::new(0, -5)));
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }

    // ---- HalfPageDown / HalfPageUp ------------------------------------

    #[test]
    fn half_page_down_centers_cursor() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4; // half = 2, center offset = 2
        s.cursor_pos = Position::<u16>::new(0, 1); // abs row 1
        s.move_cursor(MoveKind::HalfPageDown);
        // abs row → 3, centered: file_pos = 3 - 2 = 1, cursor at row 2.
        assert_eq!(s.file_pos.row, 1);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 3);
    }

    #[test]
    fn half_page_up_centers_cursor() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4;
        s.file_pos.row = 4;
        s.cursor_pos = Position::<u16>::new(0, 2); // abs row 6
        s.move_cursor(MoveKind::HalfPageUp);
        // abs row → 4, centered: file_pos = 2, cursor at row 2.
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 4);
    }

    // ---- Center (zz) --------------------------------------------------

    #[test]
    fn center_puts_cursor_in_middle_of_viewport() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 5; // center offset = 2
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.file_pos.row = 6; // abs row 6
        s.move_cursor(MoveKind::Center);
        assert_eq!(s.file_pos.row, 4);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 6);
    }

    #[test]
    fn center_near_top_does_not_scroll_past_origin() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 5;
        s.cursor_pos = Position::<u16>::new(0, 1); // abs row 1
        s.move_cursor(MoveKind::Center);
        // Want file_pos = 1 - 2 = -1 → clamped to 0; cursor stays at abs row 1.
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 1);
    }

    #[test]
    fn center_near_eof_pins_viewport_to_last_line() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4; // max_file_pos = 6
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.file_pos.row = 9; // abs row 9
        s.move_cursor(MoveKind::Center);
        // Center wants file_pos = 9 - 2 = 7, EOF cap pulls it back to 6.
        assert_eq!(s.file_pos.row, 6);
        assert_eq!(cur_row(&s), 9);
    }

    #[test]
    fn half_page_down_near_eof_pins_viewport_to_last_line() {
        // 10 lines (indices 0..=9), viewport 4, half = 2. From the bottom,
        // C-d shouldn't scroll past EOF (the last line stays in view).
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4;
        s.file_pos.row = 6;
        s.cursor_pos = Position::<u16>::new(0, 3); // abs row 9
        s.move_cursor(MoveKind::HalfPageDown);
        // Already at last line; viewport stays pinned with last_line at bottom.
        assert_eq!(s.file_pos.row, 6);
        assert_eq!(cur_row(&s), 9);
    }

    // ---- selected_text ------------------------------------------------

    #[test]
    fn selected_text_none_when_not_visual() {
        let s = mk("hello");
        assert_eq!(s.selected_text(), None);
    }

    #[test]
    fn selected_text_none_in_visual_without_anchor() {
        // Anchor is normally set by set_mode, but a directly-set Visual mode
        // with no anchor (e.g. mid-construction) must still return None.
        let mut s = mk("hello");
        s.mode = EditingMode::Visual;
        assert_eq!(s.selected_text(), None);
    }

    #[test]
    fn selected_text_visual_forward_single_line() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Visual); // anchor at (0,0)
        s.cursor_pos = Position::<u16>::new(2, 0);
        assert_eq!(s.selected_text().as_deref(), Some("hel"));
    }

    #[test]
    fn selected_text_visual_reverse_single_line() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(3, 0);
        s.set_mode(EditingMode::Visual); // anchor at col 3
        s.cursor_pos = Position::<u16>::new(1, 0);
        assert_eq!(s.selected_text().as_deref(), Some("ell"));
    }

    #[test]
    fn selected_text_visual_single_char() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Visual);
        assert_eq!(s.selected_text().as_deref(), Some("h"));
    }

    #[test]
    fn selected_text_visual_multiline() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::Visual); // anchor at (col=1, row=0)
        s.cursor_pos = Position::<u16>::new(1, 1);
        assert_eq!(s.selected_text().as_deref(), Some("bc\nde"));
    }

    #[test]
    fn selected_text_visual_clamps_at_eof() {
        let mut s = mk("ab");
        s.set_mode(EditingMode::Visual);
        s.cursor_pos = Position::<u16>::new(50, 0); // past end, no clamp called
        assert_eq!(s.selected_text().as_deref(), Some("ab"));
    }

    #[test]
    fn selected_text_visual_line_single_line() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.set_mode(EditingMode::VisualLine);
        assert_eq!(s.selected_text().as_deref(), Some("def\n"));
    }

    #[test]
    fn selected_text_visual_line_multiline() {
        let mut s = mk("abc\ndef\nghi");
        s.set_mode(EditingMode::VisualLine);
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert_eq!(s.selected_text().as_deref(), Some("abc\ndef\n"));
    }

    #[test]
    fn selected_text_visual_line_reverse() {
        // Anchor below cursor — start/end rows must swap.
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 2);
        s.set_mode(EditingMode::VisualLine);
        s.cursor_pos = Position::<u16>::new(0, 0);
        assert_eq!(s.selected_text().as_deref(), Some("abc\ndef\nghi"));
    }

    #[test]
    fn selected_text_visual_line_includes_last_line_without_newline() {
        let mut s = mk("abc\ndef");
        s.set_mode(EditingMode::VisualLine);
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert_eq!(s.selected_text().as_deref(), Some("abc\ndef"));
    }

    #[test]
    fn selected_text_visual_block_rectangle() {
        let mut s = mk("abcde\nfghij\nklmno");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(3, 2);
        assert_eq!(s.selected_text().as_deref(), Some("bcd\nghi\nlmn"));
    }

    #[test]
    fn selected_text_visual_block_reverse_columns() {
        let mut s = mk("abcde\nfghij");
        s.cursor_pos = Position::<u16>::new(3, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(1, 1);
        assert_eq!(s.selected_text().as_deref(), Some("bcd\nghi"));
    }

    #[test]
    fn selected_text_visual_block_truncates_short_lines() {
        let mut s = mk("ab\nfghij\nk");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(3, 2);
        // row 0 "ab"     → cols 1..3 capped to len 2 → "b"
        // row 1 "fghij"  → cols 1..4 → "ghi"
        // row 2 "k"      → cols capped to len 1, empty slice
        // Trailing newline after row 1 is emitted before the empty row 2.
        assert_eq!(s.selected_text().as_deref(), Some("b\nghi\n"));
    }

    // ---- undo / redo --------------------------------------------------

    #[test]
    fn undo_reverts_insert_char_and_restores_cursor() {
        let mut s = mk("ab");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('c');
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(cur_col(&s), 3);
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "ab");
        // Cursor returns to the pre-edit column, not col 0.
        assert_eq!(cur_col(&s), 2);
    }

    #[test]
    fn redo_reapplies_insert_char_and_restores_cursor() {
        let mut s = mk("ab");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('c');
        s.undo();
        assert!(s.redo());
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(cur_col(&s), 3);
    }

    #[test]
    fn undo_reverts_newline_split_with_cursor() {
        let mut s = mk("abcd");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('\n');
        assert_eq!(s.buf.to_string(), "ab\ncd");
        assert_eq!((cur_row(&s), cur_col(&s)), (1, 0));
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "abcd");
        assert_eq!((cur_row(&s), cur_col(&s)), (0, 2));
        assert!(s.redo());
        assert_eq!(s.buf.to_string(), "ab\ncd");
        assert_eq!((cur_row(&s), cur_col(&s)), (1, 0));
    }

    #[test]
    fn undo_reverts_delete_char_join_with_cursor() {
        // Backspacing across the newline merges two lines; undo must restore
        // the split AND drop the cursor back before 'c' on line 1.
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(0, 1); // before 'c'
        s.delete_char();
        assert_eq!(s.buf.to_string(), "abcd");
        assert_eq!((cur_row(&s), cur_col(&s)), (0, 2));
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "ab\ncd");
        assert_eq!((cur_row(&s), cur_col(&s)), (1, 0));
        assert!(s.redo());
        assert_eq!(s.buf.to_string(), "abcd");
        assert_eq!((cur_row(&s), cur_col(&s)), (0, 2));
    }

    #[test]
    fn undo_restores_column_mid_line() {
        // 'x' deletes a non-leading char; undo should put the cursor back on
        // the original column, not col 0.
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(3, 0); // on 'l'
        s.delete_char_at(Position::new(3, 0));
        assert_eq!(s.buf.to_string(), "helo");
        s.undo();
        assert_eq!(s.buf.to_string(), "hello");
        assert_eq!(cur_col(&s), 3);
    }

    #[test]
    fn undo_chain_then_new_edit_drops_redo() {
        let mut s = mk("");
        s.mode = EditingMode::Insert;
        s.insert_char('a');
        s.insert_char('b');
        s.insert_char('c');
        assert_eq!(s.buf.to_string(), "abc");
        s.undo();
        s.undo();
        assert_eq!(s.buf.to_string(), "a");
        // Cursor was at col 1 (just after 'a') before the 'b' insert, so undo
        // restored it there. 'Z' lands between 'a' and what was 'b'.
        assert_eq!(cur_col(&s), 1);
        s.insert_char('Z');
        assert_eq!(s.buf.to_string(), "aZ");
        assert!(!s.redo());
    }

    #[test]
    fn undo_on_fresh_buffer_is_noop() {
        let mut s = mk("hello");
        assert!(!s.undo());
        assert_eq!(s.buf.to_string(), "hello");
    }

    #[test]
    fn half_page_up_at_top_does_not_scroll_past_origin() {
        let mut s = mk("0\n1\n2\n3\n4\n5");
        s.viewport.row = 4;
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::HalfPageUp);
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }
}
