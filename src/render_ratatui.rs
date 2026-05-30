use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
};

use crate::{
    components::{Component, EditorView, StatusLine},
    render::{CursorStyle, Renderer, StateSnapshot},
};

pub struct RatatuiRenderer {
    term: Terminal<CrosstermBackend<Stdout>>,
    components: Vec<Box<dyn Component>>,
}

impl RatatuiRenderer {
    pub fn new() -> io::Result<Self> {
        Self::with_components(vec![
            Box::new(EditorView::default()),
            Box::new(StatusLine::default()),
        ])
    }

    pub fn with_components(components: Vec<Box<dyn Component>>) -> io::Result<Self> {
        let backend = CrosstermBackend::new(io::stdout());
        Ok(Self {
            term: Terminal::new(backend)?,
            components,
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

        self.term.draw(|f| {
            let constraints: Vec<Constraint> =
                self.components.iter().map(|c| c.constraint()).collect();
            let rects = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(f.area());

            // Render each component into its assigned rect; pick up the
            // cursor from whichever component owns it (last `Some` wins, so
            // a floating component drawn on top can override).
            let mut cursor = None;
            for (c, area) in self.components.iter().zip(rects.iter()) {
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
