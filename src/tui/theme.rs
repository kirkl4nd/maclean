use ratatui::style::{Color, Modifier, Style};

use crate::core::Safety;

/// Quiet palette: one accent, muted status colours, and plain text everywhere
/// else. Emphasis is spent on the row you are on and on things that can bite.
pub struct Theme;

const ACCENT: Color = Color::Rgb(94, 148, 176);
const GOOD: Color = Color::Rgb(106, 148, 108);
const WARN: Color = Color::Rgb(184, 142, 74);
const DANGER: Color = Color::Rgb(178, 94, 84);

/// Glyphs, kept in one place so the whole UI stays consistent.
pub mod glyph {
    pub const CURSOR: &str = "▌";
    pub const OPEN: &str = "▾";
    pub const CLOSED: &str = "▸";
    pub const BOX_EMPTY: &str = "○";
    pub const BOX_PARTIAL: &str = "◐";
    pub const BOX_FULL: &str = "●";
    pub const RULE: &str = "─";
    pub const OK: &str = "✓";
    pub const FAIL: &str = "✗";
    pub const DOT: &str = "·";
}

impl Theme {
    pub fn base() -> Style {
        Style::default()
    }

    pub fn strong() -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    pub fn muted() -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    pub fn accent() -> Style {
        Style::default().fg(ACCENT)
    }

    pub fn heading() -> Style {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    }

    /// The row the cursor is on: accent, not just bold, so it reads as selected.
    pub fn selected() -> Style {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    }

    pub fn selected_muted() -> Style {
        Style::default().fg(ACCENT)
    }

    pub fn ok() -> Style {
        Style::default().fg(GOOD)
    }

    pub fn warn() -> Style {
        Style::default().fg(WARN)
    }

    pub fn danger() -> Style {
        Style::default().fg(DANGER)
    }

    pub fn safety(safety: Safety) -> Style {
        match safety {
            Safety::Safe => Style::default().fg(GOOD),
            Safety::Caution => Style::default().fg(WARN),
            Safety::Destructive => Style::default().fg(DANGER),
            Safety::Info => Style::default().add_modifier(Modifier::DIM),
        }
    }
}
