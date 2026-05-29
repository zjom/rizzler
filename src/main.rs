use crossterm::{
    cursor::{self, MoveTo},
    event::{
        self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        Event, KeyCode, KeyEvent,
    },
    execute,
    style::{Color, SetForegroundColor},
    terminal::{
        self, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};
use std::{
    io::{self, Write},
    time::Duration,
};

pub enum EditingMode {
    Insert,
    Normal,
    Visual,
    Command,
}

pub struct State<T: Write> {
    buf: String,
    mode: EditingMode,
    command_buf: String,
    quit: bool,
    w: T,
    size: (u16, u16),
}

impl<T: Write> State<T> {
    pub fn handle_command(&mut self) -> io::Result<()> {
        match self.command_buf.as_str() {
            "quit" | "q" => {
                self.quit = true;
            }
            _ => {}
        };
        self.command_buf.clear();
        Ok(())
    }
    pub fn handle_key_event(&mut self, event: KeyEvent) -> io::Result<()> {
        match self.mode {
            EditingMode::Command => match event.code {
                KeyCode::Enter => {
                    self.handle_command()?;
                }
                KeyCode::Char(c) => {
                    self.command_buf.push(c);
                }
                KeyCode::Backspace => {
                    self.command_buf.pop();
                }
                KeyCode::Esc => {
                    self.mode = EditingMode::Normal;
                    execute!(self.w, cursor::SetCursorStyle::SteadyBlock)?;
                }
                _ => {}
            },

            EditingMode::Insert => {
                match event.code {
                    KeyCode::Enter => {
                        self.buf.push('\n');
                    }
                    KeyCode::Char(c) => {
                        self.buf.push(c);
                    }
                    KeyCode::Backspace => {
                        self.buf.pop();
                    }
                    KeyCode::Esc => {
                        self.mode = EditingMode::Normal;
                        execute!(self.w, cursor::SetCursorStyle::SteadyBlock)?;
                    }
                    _ => {}
                }

                self.render()?;
            }
            EditingMode::Normal => match event.code {
                KeyCode::Char(':') => {
                    self.mode = EditingMode::Command;
                }
                KeyCode::Char('i') => {
                    self.mode = EditingMode::Insert;
                    execute!(self.w, cursor::SetCursorStyle::SteadyBar)?;
                }

                KeyCode::Char('j') => {
                    execute!(self.w, cursor::MoveDown(1))?;
                }

                KeyCode::Char('k') => {
                    execute!(self.w, cursor::MoveUp(1))?;
                }

                KeyCode::Char('h') => {
                    execute!(self.w, cursor::MoveLeft(1))?;
                }

                KeyCode::Char('l') => {
                    execute!(self.w, cursor::MoveRight(1))?;
                }
                _ => {}
            },
            _ => {}
        };
        Ok(())
    }

    fn render(&mut self) -> io::Result<()> {
        execute!(
            self.w,
            terminal::Clear(terminal::ClearType::All),
            MoveTo(0, 0),
            SetForegroundColor(Color::Blue)
        )?;
        let main_area = self.size.0 as usize * (self.size.1 as usize - 1);
        let (to_render, _) = self.buf.split_at(main_area.clamp(0, self.buf.len()));
        for c in to_render.as_bytes() {
            if *c == b'\n' {
                let cur_pos = cursor::position()?;
                execute!(self.w, MoveTo(0, cur_pos.1 + 1))?;
                continue;
            }

            write!(self.w, "{}", c)?;
        }

        let pos = cursor::position()?;

        execute!(self.w, cursor::MoveTo(0, self.size.0))?;
        match self.mode {
            EditingMode::Insert => self.w.write(b"i"),
            EditingMode::Normal => self.w.write(b"n"),
            EditingMode::Visual => self.w.write(b"v"),
            EditingMode::Command => self.w.write(b"c"),
        }?;

        let cmd_area = self.size.0 as usize - 1;
        let (cmd_buf, _) = self
            .command_buf
            .split_at(cmd_area.clamp(0, self.command_buf.len()));
        self.w.write_all(cmd_buf.as_bytes())?;

        execute!(self.w, MoveTo(pos.0, pos.1))?;
        self.w.flush()
    }
}

fn main() -> io::Result<()> {
    let stdout = io::stdout();
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        terminal::Clear(terminal::ClearType::All),
        EnableFocusChange,
        EnableMouseCapture
    )?;
    enable_raw_mode()?;
    let mut state = State {
        buf: String::with_capacity(1000),
        mode: EditingMode::Normal,
        quit: false,
        command_buf: String::new(),
        w: stdout,
        size: terminal::size()?,
    };

    loop {
        if state.quit {
            break;
        }

        if event::poll(Duration::from_millis(500))? {
            match event::read()? {
                Event::Key(key_event) => state.handle_key_event(key_event)?,
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableFocusChange,
        DisableMouseCapture
    )?;
    Ok(())
}
