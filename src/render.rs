use std::io::{self, Write};

use crossterm::{
    cursor::{self, MoveTo},
    execute,
    style::{Color, SetForegroundColor},
    terminal,
};

use crate::{buffer::Buffer, keymap::KeyEvent, mode::EditingMode, position::Position};

/// Read-only view of the editor passed to renderers. Decoupling this from
/// `State` means a renderer can be implemented without depending on the
/// editor's internal type, and `State` can hand out a snapshot without
/// exposing its private fields.
pub struct StateSnapshot<'a> {
    pub buffer: &'a Buffer,
    pub mode: EditingMode,
    pub command_buf: &'a str,
    pub bufno: usize,
    pub size: Position<u16>,
    pub keyevent: Option<KeyEvent>,
}

pub trait Renderer {
    fn render(&self, w: &mut dyn Write, snap: StateSnapshot<'_>) -> io::Result<()>;
}

pub struct DefaultRenderer;

impl Renderer for DefaultRenderer {
    fn render(&self, w: &mut dyn Write, snap: StateSnapshot<'_>) -> io::Result<()> {
        // Stage into a local buffer because `execute!` requires a `Sized`
        // writer (it calls `Write::by_ref`). Writing the whole frame in one
        // shot also avoids intra-frame flicker.
        let mut out: Vec<u8> = Vec::new();

        execute!(
            out,
            terminal::Clear(terminal::ClearType::All),
            MoveTo(0, 0),
            SetForegroundColor(Color::Blue),
        )?;

        let start = snap.buffer.file_pos().row.min(snap.buffer.len_lines());
        let lines = snap.buffer.lines_at(start);

        let view_height = snap.size.row.saturating_sub(1);
        for (lnum, line) in (0u16..view_height).zip(lines) {
            // strip the trailing newline; MoveTo handles row positioning
            let text = line.to_string();
            write!(out, "{}", text.trim_end_matches(['\n', '\r']))?;
            execute!(out, MoveTo(0, lnum + 1))?;
        }

        execute!(out, cursor::MoveTo(0, snap.size.row.saturating_sub(1)))?;
        match snap.mode {
            EditingMode::Insert => out.write_all(b"i")?,
            EditingMode::Normal => out.write_all(b"n")?,
            EditingMode::Visual => out.write_all(b"v")?,
            EditingMode::Command => out.write_all(b":")?,
        }

        let cmd_area = (snap.size.col as usize).saturating_sub(4);
        let (cmd_buf, _) = snap
            .command_buf
            .split_at(cmd_area.min(snap.command_buf.len()));
        out.write_all(cmd_buf.as_bytes())?;

        let rhs = format!(
            "{}  {}",
            snap.keyevent
                .map(|e| e.code.to_string())
                .unwrap_or("None".to_string()),
            snap.bufno
        );
        execute!(
            out,
            MoveTo(
                snap.size.col.saturating_sub(rhs.len() as u16),
                snap.size.row - 1
            )
        )?;
        out.write_all(rhs.as_bytes())?;

        let cur = snap.buffer.cursor_pos();
        execute!(out, MoveTo(cur.col, cur.row))?;

        w.write_all(&out)?;
        w.flush()
    }
}
