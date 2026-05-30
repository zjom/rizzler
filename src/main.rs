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
use ropey::{Rope, RopeSlice};
use std::{
    io::{self, Write},
    time::Duration,
};

#[derive(Clone, Copy)]
pub enum EditingMode {
    Insert,
    Normal,
    Visual,
    Command,
}

trait PositionSealed {}
impl PositionSealed for u16 {}
impl PositionSealed for usize {}

#[derive(Clone, Copy)]
struct Position<T: PositionSealed> {
    row: T, // line number
    col: T, // idx of char in row
}

impl Position<usize> {
    pub fn new(col: usize, row: usize) -> Self {
        Self { row, col }
    }
}

impl Position<u16> {
    pub fn new(col: u16, row: u16) -> Self {
        Self { row, col }
    }
}

pub struct State<T: Write> {
    buf: Rope,
    mode: EditingMode,
    command_buf: String,
    quit: bool,
    w: T,
    size: Position<u16>,
    cursor_pos: Position<u16>,
    file_pos: Position<usize>,
}

impl<T: Write> State<T> {
    pub fn handle_command(&mut self) -> io::Result<()> {
        if matches!(self.command_buf.as_str(), "quit" | "q") {
            self.quit = true;
        }
        self.command_buf.clear();
        Ok(())
    }

    /// Helper to transition modes and update the terminal cursor style automatically
    fn set_mode(&mut self, mode: EditingMode) -> io::Result<()> {
        self.mode = mode;
        let cursor_style = match mode {
            EditingMode::Insert => cursor::SetCursorStyle::SteadyBar,
            _ => cursor::SetCursorStyle::SteadyBlock,
        };
        execute!(self.w, cursor_style)
    }

    fn insert_char(&mut self, c: char) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;

        self.buf.insert_char(cidx, c);

        if c == '\n' {
            self.cursor_pos.row = self.cursor_pos.row.saturating_add(1);
            self.cursor_pos.col = 0; // Reset column to 0 when moving to a new line
        } else {
            self.cursor_pos.col = self.cursor_pos.col.saturating_add(1);
        }
    }

    fn delete_char(&mut self) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;

        if cidx == 0 {
            return;
        }

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
    }

    fn cur_line(&self) -> RopeSlice<'_> {
        self.buf.line(self.cur_lnum())
    }

    /// The line number (row in the rope) the cursor is currently on.
    fn cur_lnum(&self) -> usize {
        self.cursor_pos.row as usize + self.file_pos.row
    }

    /// Char index of the first character of the current line.
    fn cur_line_start(&self) -> usize {
        self.buf.line_to_char(self.cur_lnum())
    }

    fn move_cursor(&mut self, dx: i16, dy: i16) {
        if dx > 0 {
            self.cursor_pos.col = self.cursor_pos.col.saturating_add(dx as u16);
        }
        if dx < 0 {
            self.cursor_pos.col = self.cursor_pos.col.saturating_sub((-dx) as u16);
        }
        if dy > 0 {
            self.cursor_pos.row = self.cursor_pos.row.saturating_add(dy as u16);
        }
        if dy < 0 {
            self.cursor_pos.row = self.cursor_pos.row.saturating_sub((-dy) as u16);
        }
    }

    fn clamp_cursor(&mut self) {
        let last_line = self.buf.len_lines().saturating_sub(1);
        let abs_row = (self.cursor_pos.row as usize + self.file_pos.row).min(last_line);
        self.cursor_pos.row = abs_row.saturating_sub(self.file_pos.row) as u16;

        let line = self.buf.line(abs_row);
        let mut line_len = line.len_chars();
        // don't count the trailing newline as a landable column
        if line_len > 0 && line.char(line_len - 1) == '\n' {
            line_len -= 1;
        }
        let abs_col = (self.cursor_pos.col as usize + self.file_pos.col).min(line_len);
        self.cursor_pos.col = abs_col.saturating_sub(self.file_pos.col) as u16;
    }

    pub fn handle_key_event(&mut self, event: KeyEvent) -> io::Result<()> {
        match self.mode {
            EditingMode::Command => match event.code {
                KeyCode::Enter => self.handle_command()?,
                KeyCode::Char(c) => self.command_buf.push(c),
                KeyCode::Backspace => {
                    self.command_buf.pop();
                }
                KeyCode::Esc => self.set_mode(EditingMode::Normal)?,
                _ => {}
            },
            EditingMode::Insert => match event.code {
                KeyCode::Enter => self.insert_char('\n'),
                KeyCode::Char(c) => self.insert_char(c),
                KeyCode::Backspace => self.delete_char(),
                KeyCode::Esc => self.set_mode(EditingMode::Normal)?,
                _ => {}
            },
            EditingMode::Normal => match event.code {
                KeyCode::Char(':') => self.set_mode(EditingMode::Command)?,
                KeyCode::Char('i') => self.set_mode(EditingMode::Insert)?,
                KeyCode::Char('j') | KeyCode::Down => self.move_cursor(0, 1),
                KeyCode::Char('k') | KeyCode::Up => self.move_cursor(0, -1),
                KeyCode::Char('h') | KeyCode::Left => self.move_cursor(-1, 0),
                KeyCode::Char('l') | KeyCode::Right => self.move_cursor(1, 0),
                _ => {}
            },
            EditingMode::Visual => {
                todo!()
            }
        };

        // Keep the cursor valid, then render once per handled input.
        self.clamp_cursor();
        self.render()
    }

    fn render(&mut self) -> io::Result<()> {
        execute!(
            self.w,
            terminal::Clear(terminal::ClearType::All),
            MoveTo(0, 0),
            SetForegroundColor(Color::Blue)
        )?;

        let start = self.file_pos.row.min(self.buf.len_lines());
        let lines = self.buf.lines_at(start);

        let view_height = self.size.row.saturating_sub(1);
        for (lnum, line) in (0u16..view_height).zip(lines) {
            // strip the trailing newline; MoveTo handles row positioning
            let text = line.to_string();
            write!(self.w, "{}", text.trim_end_matches(['\n', '\r']))?;
            execute!(self.w, MoveTo(0, lnum + 1))?;
        }

        execute!(self.w, cursor::MoveTo(0, self.size.row.saturating_sub(1)))?;
        match self.mode {
            EditingMode::Insert => self.w.write_all(b"i"),
            EditingMode::Normal => self.w.write_all(b"n"),
            EditingMode::Visual => self.w.write_all(b"v"),
            EditingMode::Command => self.w.write_all(b"c"),
        }?;

        let cmd_area = (self.size.col as usize).saturating_sub(1);
        let (cmd_buf, _) = self
            .command_buf
            .split_at(cmd_area.min(self.command_buf.len()));
        self.w.write_all(cmd_buf.as_bytes())?;

        execute!(self.w, MoveTo(self.cursor_pos.col, self.cursor_pos.row))?;
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
    let (cols, rows) = terminal::size()?;

    let mut state = State {
        buf: Rope::new(),
        mode: EditingMode::Normal,
        quit: false,
        command_buf: String::new(),
        w: stdout,
        size: Position::<u16>::new(cols, rows),
        cursor_pos: Position::<u16>::new(0, 0),
        file_pos: Position::<usize>::new(0, 0),
    };

    loop {
        if state.quit {
            break;
        }

        if event::poll(Duration::from_millis(500))? {
            if let Event::Key(key_event) = event::read()? {
                state.handle_key_event(key_event)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    fn mk(text: &str) -> State<Vec<u8>> {
        State {
            buf: Rope::from_str(text),
            mode: EditingMode::Normal,
            quit: false,
            command_buf: String::new(),
            w: Vec::new(),
            size: Position::<u16>::new(80, 24),
            cursor_pos: Position::<u16>::new(0, 0),
            file_pos: Position::<usize>::new(0, 0),
        }
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

    #[test]
    fn insert_on_second_line_uses_correct_offset() {
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(1, 1); // between 'c' and 'd'
        s.insert_char('X');
        assert_eq!(s.buf.to_string(), "ab\ncXd");
    }

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
        assert_eq!(s.cursor_pos.col, 2); // end of "cd"
    }

    #[test]
    fn render_does_not_panic_on_empty_buffer() {
        let mut s = mk("");
        s.render().unwrap();
    }
}
