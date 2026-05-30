use std::io::{self, Write};

use crossterm::{cursor::SetCursorStyle, event::KeyEvent, execute};

use crate::{
    action::Action,
    buffer::Buffer,
    command::{CommandRegistry, DefaultCommands},
    keymap::{DefaultKeymap, Keymap},
    mode::EditingMode,
    position::Position,
    render::{DefaultRenderer, Renderer, StateSnapshot},
};

/// Bundle of plugin points injected into [`State`]. Swap any field to
/// customise the editor without touching `State`'s internals.
pub struct Config {
    pub keymap: Box<dyn Keymap>,
    pub commands: Box<dyn CommandRegistry>,
    pub renderer: Box<dyn Renderer>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keymap: Box::new(DefaultKeymap),
            commands: Box::new(DefaultCommands),
            renderer: Box::new(DefaultRenderer),
        }
    }
}

pub struct State<W: Write> {
    bufs: Vec<Buffer>,
    bufno: usize,
    mode: EditingMode,
    command_buf: String,
    quit: bool,
    w: W,
    size: Position<u16>,
    keymap: Box<dyn Keymap>,
    commands: Box<dyn CommandRegistry>,
    renderer: Box<dyn Renderer>,
}

impl<W: Write> State<W> {
    pub fn new(w: W, cols: u16, rows: u16) -> io::Result<Self> {
        Self::with_config(w, cols, rows, Config::default())
    }

    pub fn with_config(w: W, cols: u16, rows: u16, config: Config) -> io::Result<Self> {
        Ok(Self {
            bufs: vec![Buffer::new()],
            bufno: 0,
            mode: EditingMode::Normal,
            command_buf: String::new(),
            quit: false,
            w,
            size: Position::new(cols, rows),
            keymap: config.keymap,
            commands: config.commands,
            renderer: config.renderer,
        })
    }

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    pub fn handle_key_event(&mut self, event: KeyEvent) -> io::Result<()> {
        let action = self.keymap.resolve(self.mode, event);
        self.apply(action)?;
        self.bufs[self.bufno].clamp_cursor();
        self.render()
    }

    pub fn apply(&mut self, action: Action) -> io::Result<()> {
        match action {
            Action::Noop => {}
            Action::Quit => self.quit = true,
            Action::SetMode(m) => self.set_mode(m)?,
            Action::InsertChar(c) => self.bufs[self.bufno].insert_char(c),
            Action::InsertNewline => self.bufs[self.bufno].insert_char('\n'),
            Action::DeleteChar => self.bufs[self.bufno].delete_char(),
            Action::MoveCursor(dx, dy) => self.bufs[self.bufno].move_cursor(dx, dy),
            Action::CommandPush(c) => self.command_buf.push(c),
            Action::CommandPop => {
                self.command_buf.pop();
            }
            Action::CommandSubmit => {
                let next = self.commands.parse(&self.command_buf);
                self.command_buf.clear();
                self.set_mode(EditingMode::Normal)?;
                self.apply(next)?;
            }
            Action::CommandCancel => {
                self.command_buf.clear();
                self.set_mode(EditingMode::Normal)?;
            }
            Action::BufCreate => self.create_buf(),
            Action::BufDelete => self.delete_buf(self.bufno),
            Action::BufNext => self.next_buffer(),
            Action::BufPrev => self.previous_buffer(),
        }
        Ok(())
    }

    fn set_mode(&mut self, mode: EditingMode) -> io::Result<()> {
        self.mode = mode;
        let style = match mode {
            EditingMode::Insert => SetCursorStyle::SteadyBar,
            _ => SetCursorStyle::SteadyBlock,
        };
        execute!(self.w, style)
    }

    fn create_buf(&mut self) {
        self.bufs.push(Buffer::new());
        self.bufno = self.bufs.len() - 1;
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
        // Destructure so the immutable borrow of buffer/mode/command_buf and
        // the mutable borrow of `w` can coexist (renderer needs both).
        let Self {
            bufs,
            mode,
            command_buf,
            bufno,
            size,
            w,
            renderer,
            ..
        } = self;
        let snap = StateSnapshot {
            buffer: &bufs[*bufno],
            mode: *mode,
            command_buf: command_buf.as_str(),
            bufno: *bufno,
            size: *size,
        };
        renderer.render(w, snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_does_not_panic_on_empty_buffer() {
        let mut s = State::new(Vec::new(), 10, 10).unwrap();
        s.render().unwrap();
    }
}
