use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
};

use crate::{
    components::{Component, EditorView, MinibufferLine, StatusLine},
    render::{CursorStyle, Renderer, StateSnapshot},
};

/// Bottom-of-screen strip components, rendered in order below the editor
/// window tree. The renderer always reserves one row per component here.
pub struct RatatuiRenderer {
    term: Terminal<CrosstermBackend<Stdout>>,
    editor: EditorView,
    bottom: Vec<Box<dyn Component>>,
}

impl RatatuiRenderer {
    pub fn new() -> io::Result<Self> {
        Self::with_parts(
            EditorView::default(),
            vec![Box::new(StatusLine::default()), Box::new(MinibufferLine)],
        )
    }

    pub fn with_parts(
        editor: EditorView,
        bottom: Vec<Box<dyn Component>>,
    ) -> io::Result<Self> {
        let backend = CrosstermBackend::new(io::stdout());
        Ok(Self {
            term: Terminal::new(backend)?,
            editor,
            bottom,
        })
    }
}

impl Renderer for RatatuiRenderer {
    fn render(&mut self, snap: StateSnapshot<'_>) -> io::Result<()> {
        // Cursor style is a terminal escape, not a ratatui widget — emit it
        // out-of-band before the frame draw.
        let style = match snap.cursor_style {
            CursorStyle::Bar => SetCursorStyle::SteadyBar,
            CursorStyle::Block => SetCursorStyle::SteadyBlock,
        };
        execute!(io::stdout(), style)?;

        let editor = &self.editor;
        let bottom = &self.bottom;

        self.term.draw(|f| {
            // Vertical layout: window-tree area on top, then one row per
            // bottom component.
            let mut constraints = vec![Constraint::Min(1)];
            for c in bottom {
                constraints.push(c.constraint());
            }
            let rects = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(f.area());
            let editor_area = rects[0];

            // Walk the window tree, render each leaf with EditorView. The
            // focused leaf publishes the cursor (unless focus is in the
            // minibuffer, in which case MinibufferLine will override below).
            let mut cursor = None;
            let focused_path = snap.windows.focused_path();
            for leaf in snap.windows.layout(editor_area) {
                let buf = match snap.bufs.get(leaf.bufno) {
                    Some(b) => b,
                    None => continue,
                };
                editor.render(buf, leaf.area, f);
                if !snap.focus_minibuffer && &leaf.path == focused_path {
                    cursor = Some(editor.cursor(buf, leaf.area));
                }
            }

            // Bottom strip — each component gets its assigned row.
            for (c, area) in bottom.iter().zip(rects.iter().skip(1)) {
                c.render(*area, &snap, f);
                if let Some(pos) = c.cursor(*area, &snap) {
                    cursor = Some(pos);
                }
            }
            if let Some((x, y)) = cursor {
                f.set_cursor_position((x, y));
            }
        })?;
        Ok(())
    }
}

