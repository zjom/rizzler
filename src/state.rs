use std::io;
use std::path::Path;
use std::rc::Rc;

use crossterm::event::KeyEvent;

use crate::{
    action::Action,
    buffer::Buffer,
    command::{CommandRegistry, DefaultCommands},
    keymap::KeymapRegistry,
    mode::EditingMode,
    render::{CursorStyle, Renderer, StateSnapshot},
    render_ratatui::RatatuiRenderer,
};

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
    bufs: Vec<Buffer>,
    bufno: usize,
    mode: EditingMode,
    command_buf: String,
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
        Ok(Self {
            bufs: vec![Buffer::new()],
            bufno: 0,
            mode: EditingMode::Normal,
            command_buf: String::new(),
            quit: false,
            keymap: KeymapRegistry::new(),
            commands: config.commands,
            renderer: config.renderer,
            keyevent: None,
        })
    }

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    pub fn handle_key_event(&mut self, event: KeyEvent) -> io::Result<()> {
        self.keyevent = Some(event);
        if let Some(action) = self.keymap.resolve(self.mode, event.into()) {
            self.apply(&action)?;
        }
        self.bufs[self.bufno].clamp_cursor();
        self.render()
    }

    pub fn apply(&mut self, actions: &[Rc<Action>]) -> io::Result<()> {
        for action in actions {
            match action.as_ref() {
                Action::Noop => {}
                Action::Quit => self.quit = true,
                Action::SetMode(m) => self.mode = *m,
                Action::InsertChar(c) => self.bufs[self.bufno].insert_char(*c),
                Action::InsertNewline => self.bufs[self.bufno].insert_char('\n'),
                Action::DeleteChar => self.bufs[self.bufno].delete_char(),
                Action::MoveCursor(m) => self.bufs[self.bufno].move_cursor(*m),
                Action::CommandPush(c) => self.command_buf.push(*c),
                Action::CommandPop => {
                    self.command_buf.pop();
                }
                Action::CommandSubmit => {
                    let next = self.commands.parse(&self.command_buf);
                    self.command_buf.clear();
                    self.mode = EditingMode::Normal;
                    self.apply(&[Rc::new(next)])?;
                }
                Action::CommandCancel => {
                    self.command_buf.clear();
                    self.mode = EditingMode::Normal;
                }
                Action::BufCreate { path, set_active } => {
                    self.create_buf(*set_active, path.clone())?;
                }
                Action::BufDelete => self.delete_buf(self.bufno),
                Action::BufNext => self.next_buffer(),
                Action::BufPrev => self.previous_buffer(),
                Action::BufEdit(path) => {
                    self.edit_buf(path.clone())?;
                }
                Action::BufWrite(path) => self.write_buf(path.clone())?,
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
            self.bufno = bufno;
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
                self.bufno = idx - 1;
                Ok(self.bufno)
            }
            None => {
                self.bufs.push(Buffer::with_path(path));
                self.bufno = self.bufs.len() - 1;
                Ok(self.bufno)
            }
        }
    }

    fn write_buf(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
        self.bufs[self.bufno].write(path)
    }

    fn delete_buf(&mut self, bufno: usize) {
        if bufno >= self.bufs.len() {
            return;
        }

        // Never drop the final buffer — just reset it to an empty one.
        if self.bufs.len() == 1 {
            self.bufs[0] = Buffer::new();
            self.bufno = 0;
            return;
        }

        self.bufs.remove(bufno);

        if self.bufno > bufno {
            self.bufno -= 1;
        } else if self.bufno >= self.bufs.len() {
            self.bufno = self.bufs.len() - 1;
        }
    }

    fn previous_buffer(&mut self) {
        if self.bufno == 0 {
            self.bufno = self.bufs.len() - 1;
            return;
        }
        self.bufno -= 1;
    }

    fn next_buffer(&mut self) {
        if self.bufno == self.bufs.len() - 1 {
            self.bufno = 0;
            return;
        }
        self.bufno += 1;
    }

    pub fn render(&mut self) -> io::Result<()> {
        let snap = StateSnapshot {
            buffer: &self.bufs[self.bufno],
            mode: self.mode,
            command_buf: self.command_buf.as_str(),
            bufno: self.bufno,
            keyevent: self.keyevent.map(|e| e.into()),
            cursor_style: match self.mode {
                EditingMode::Insert => CursorStyle::Bar,
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
}
