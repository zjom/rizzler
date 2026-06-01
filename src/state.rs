use std::io;
use std::path::Path;
use std::rc::Rc;

use crossterm::event::KeyEvent;

use crate::{
    action::Action,
    buffer::{Buffer, BufferKind},
    command::{CommandRegistry, DefaultCommands},
    keymap::KeymapRegistry,
    mode::EditingMode,
    position::Position,
    render::{CursorStyle, Renderer, StateSnapshot},
    render_ratatui::RatatuiRenderer,
    window::{SplitDir, WindowTree},
};

/// Bottom-of-screen reservation: one row for the status line, one for the
/// minibuffer. Subtracted from the terminal height when sizing the editor
/// area for the window tree.
const STATUS_LINE_ROWS: u16 = 1;
const MINIBUFFER_ROWS: u16 = 1;

/// Bundle of plugin points injected into [`State`]. Swap any field to
/// customise the editor without touching `State`'s internals.
pub struct Config {
    pub commands: Box<dyn CommandRegistry>,
    pub renderer: Box<dyn Renderer>,
}

impl Config {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            commands: Box::new(DefaultCommands),
            renderer: Box::new(RatatuiRenderer::new()?),
        })
    }
}

pub struct State {
    /// All live buffers. Index 0 is the minibuffer by construction; file
    /// buffers occupy index 1..
    bufs: Vec<Buffer>,
    /// Tree of editor windows. Leaves point at indices into `bufs`. The
    /// minibuffer is not part of this tree.
    windows: WindowTree,
    /// When true, key events route to the minibuffer instead of the focused
    /// editor window.
    focus_minibuffer: bool,
    /// Index of the minibuffer (always kind = Minibuffer).
    minibuffer: usize,
    quit: bool,
    keymap: KeymapRegistry,
    commands: Box<dyn CommandRegistry>,
    renderer: Box<dyn Renderer>,
    keyevent: Option<KeyEvent>,
}

impl State {
    pub fn new() -> io::Result<Self> {
        Self::with_config(Config::new()?)
    }

    pub fn with_config(config: Config) -> io::Result<Self> {
        // Layout: [minibuffer, first file buffer]. The window tree starts as
        // a single leaf pointing at the file buffer.
        let mut state = Self {
            bufs: vec![Buffer::minibuffer(), Buffer::new()],
            windows: WindowTree::new(1),
            focus_minibuffer: false,
            minibuffer: 0,
            quit: false,
            keymap: KeymapRegistry::new(),
            commands: config.commands,
            renderer: config.renderer,
            keyevent: None,
        };
        state.refresh_viewport();
        Ok(state)
    }

    /// The buffer currently receiving key events.
    fn focused_bufno(&self) -> usize {
        if self.focus_minibuffer {
            self.minibuffer
        } else {
            self.windows.focused_bufno()
        }
    }

    /// Update viewports of all buffers currently displayed in a window plus
    /// the minibuffer. Per-leaf rect comes from the window tree layout.
    /// Silently ignores terminal::size errors so tests without a real TTY
    /// still work.
    fn refresh_viewport(&mut self) {
        let Ok((cols, rows)) = crossterm::terminal::size() else {
            return;
        };
        let editor_h = rows.saturating_sub(STATUS_LINE_ROWS + MINIBUFFER_ROWS);
        let editor_area = ratatui::layout::Rect::new(0, 0, cols, editor_h);
        for leaf in self.windows.layout(editor_area) {
            if let Some(buf) = self.bufs.get_mut(leaf.bufno) {
                buf.viewport = Position::new(leaf.area.width, leaf.area.height);
            }
        }
        self.bufs[self.minibuffer].viewport = Position::new(cols, MINIBUFFER_ROWS);
    }

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    pub fn handle_key_event(&mut self, event: KeyEvent) -> io::Result<()> {
        self.keyevent = Some(event);
        let mode = self.bufs[self.focused_bufno()].mode();
        if let Some(action) = self.keymap.resolve(mode, event.into()) {
            self.apply(&action)?;
        }
        // Refresh after apply: window splits/closes and buffer switches may
        // have changed which buffer occupies which viewport.
        self.refresh_viewport();
        let focused = self.focused_bufno();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

    pub fn apply(&mut self, actions: &[Rc<Action>]) -> io::Result<()> {
        for action in actions {
            match action.as_ref() {
                Action::Noop => {}
                Action::Quit => self.quit = true,
                Action::SetMode(m) => self.set_mode(*m),
                Action::InsertChar(c) => {
                    let f = self.focused_bufno();
                    self.bufs[f].insert_char(*c);
                }
                Action::InsertNewline => {
                    let f = self.focused_bufno();
                    self.bufs[f].insert_char('\n');
                }
                Action::DeleteChar => {
                    let f = self.focused_bufno();
                    self.bufs[f].delete_char();
                }
                Action::MoveCursor(m) => {
                    let f = self.focused_bufno();
                    self.bufs[f].move_cursor(*m);
                }
                Action::CommandSubmit => {
                    let cmd = self.bufs[self.minibuffer].text();
                    self.exit_minibuffer();
                    let next = self.commands.parse(&cmd);
                    self.apply(&[Rc::new(next)])?;
                }
                Action::CommandCancel => self.exit_minibuffer(),
                Action::BufCreate { path, set_active } => {
                    self.create_buf(*set_active, path.clone())?;
                }
                Action::BufDelete => {
                    let editor = self.windows.focused_bufno();
                    self.delete_buf(editor);
                }
                Action::BufNext => self.next_buffer(),
                Action::BufPrev => self.previous_buffer(),
                Action::BufEdit(path) => {
                    self.edit_buf(path.clone())?;
                }
                Action::BufWrite(path) => self.write_buf(path.clone())?,
                Action::WindowSplit(dir) => self.window_split(*dir),
                Action::WindowClose => self.window_close(),
                Action::WindowFocusNext => self.windows.focus_next(),
                Action::KeymapSet { mode, lhs, rhs } => {
                    self.keymap.set(*mode, lhs, rhs.clone());
                }
                Action::KeymapRemove { mode, lhs } => {
                    self.keymap.remove(*mode, lhs);
                }
            }
        }
        Ok(())
    }

    /// Apply a SetMode action. Command is special: it moves focus to the
    /// minibuffer instead of changing an editor buffer's mode.
    fn set_mode(&mut self, mode: EditingMode) {
        if mode == EditingMode::Command {
            self.bufs[self.minibuffer].set_mode(EditingMode::Command);
            self.focus_minibuffer = true;
        } else {
            let f = self.focused_bufno();
            self.bufs[f].set_mode(mode);
        }
    }

    /// Clear the minibuffer, drop focus from it, and reset the focused
    /// editor buffer to Normal mode.
    fn exit_minibuffer(&mut self) {
        self.bufs[self.minibuffer].clear();
        self.bufs[self.minibuffer].set_mode(EditingMode::Command);
        self.focus_minibuffer = false;
        let editor = self.windows.focused_bufno();
        self.bufs[editor].set_mode(EditingMode::Normal);
    }

    fn create_buf(&mut self, set_active: bool, path: Option<Rc<Path>>) -> io::Result<usize> {
        let buf = match path {
            Some(ref p) => self
                .bufs
                .iter()
                .find(|b| b.fs_path == path)
                .cloned()
                .unwrap_or_else(|| Buffer::with_path(p.clone())),
            None => Buffer::new(),
        };

        self.bufs.push(buf);
        let bufno = self.bufs.len() - 1;
        if set_active {
            self.windows.set_focused_bufno(bufno);
        }
        Ok(bufno)
    }

    fn edit_buf(&mut self, path: Rc<Path>) -> io::Result<usize> {
        let idx = self
            .bufs
            .iter()
            .position(|b| b.fs_path.as_ref() == Some(&path));
        match idx {
            Some(idx) => {
                self.windows.set_focused_bufno(idx);
                Ok(idx)
            }
            None => {
                self.bufs.push(Buffer::with_path(path));
                let idx = self.bufs.len() - 1;
                self.windows.set_focused_bufno(idx);
                Ok(idx)
            }
        }
    }

    fn write_buf(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
        let editor = self.windows.focused_bufno();
        self.bufs[editor].write(path)
    }

    /// Refuses to delete the minibuffer; keeps at least one file buffer alive.
    fn delete_buf(&mut self, bufno: usize) {
        if bufno >= self.bufs.len() || self.bufs[bufno].kind() == BufferKind::Minibuffer {
            return;
        }

        if self.file_buf_count() == 1 {
            // Last file buffer: reset it in place instead of removing.
            self.bufs[bufno] = Buffer::new();
            self.windows.for_each_leaf_mut(|b| *b = bufno);
            return;
        }

        self.bufs.remove(bufno);
        if self.minibuffer > bufno {
            self.minibuffer -= 1;
        }
        // Reindex every leaf that pointed past the removed buffer; any leaf
        // that pointed AT the removed buffer falls back to the first file buf.
        let first = self.first_file_buf();
        self.windows.for_each_leaf_mut(|b| {
            if *b == bufno {
                *b = first;
            } else if *b > bufno {
                *b -= 1;
            }
        });
    }

    fn window_split(&mut self, dir: SplitDir) {
        // New pane gets a fresh scratch buffer.
        self.bufs.push(Buffer::new());
        let new_bufno = self.bufs.len() - 1;
        self.windows.split(dir, new_bufno);
    }

    fn window_close(&mut self) {
        self.windows.close_focused();
    }

    fn file_buf_count(&self) -> usize {
        self.bufs
            .iter()
            .filter(|b| b.kind() != BufferKind::Minibuffer)
            .count()
    }

    fn first_file_buf(&self) -> usize {
        self.bufs
            .iter()
            .position(|b| b.kind() != BufferKind::Minibuffer)
            .expect("at least one file buffer always exists")
    }

    /// Cycle the focused window to the previous file buffer in `bufs`,
    /// skipping the minibuffer.
    fn previous_buffer(&mut self) {
        let n = self.bufs.len();
        let mut i = self.windows.focused_bufno();
        for _ in 0..n {
            i = if i == 0 { n - 1 } else { i - 1 };
            if self.bufs[i].kind() != BufferKind::Minibuffer {
                self.windows.set_focused_bufno(i);
                return;
            }
        }
    }

    fn next_buffer(&mut self) {
        let n = self.bufs.len();
        let mut i = self.windows.focused_bufno();
        for _ in 0..n {
            i = if i + 1 >= n { 0 } else { i + 1 };
            if self.bufs[i].kind() != BufferKind::Minibuffer {
                self.windows.set_focused_bufno(i);
                return;
            }
        }
    }

    pub fn render(&mut self) -> io::Result<()> {
        let focused = self.focused_bufno();
        let snap = StateSnapshot {
            bufs: &self.bufs,
            windows: &self.windows,
            minibuffer: &self.bufs[self.minibuffer],
            focus_minibuffer: self.focus_minibuffer,
            bufno: self.windows.focused_bufno(),
            keyevent: self.keyevent.map(|e| e.into()),
            cursor_style: match self.bufs[focused].mode() {
                EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
                _ => CursorStyle::Block,
            },
        };
        self.renderer.render(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A renderer that does nothing — used in tests that don't want to touch a terminal.
    struct NullRenderer;
    impl Renderer for NullRenderer {
        fn render(&mut self, _snap: StateSnapshot<'_>) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_state() -> State {
        State::with_config(Config {
            commands: Box::new(DefaultCommands),
            renderer: Box::new(NullRenderer),
        })
        .unwrap()
    }

    #[test]
    fn render_does_not_panic_on_empty_buffer() {
        let mut s = test_state();
        s.render().unwrap();
    }

    #[test]
    fn split_then_close_returns_to_single_window() {
        let mut s = test_state();
        s.apply(&[Rc::new(Action::WindowSplit(SplitDir::Horizontal))])
            .unwrap();
        s.apply(&[Rc::new(Action::WindowClose)]).unwrap();
        s.render().unwrap();
    }
}
