//! Help bar content for scroll viewer modes.
//!
//! Provides context-sensitive help text displayed at the bottom of the screen.

use std::io::{self, Write};

use crate::chrome::theme::Theme;
use crate::scrollback::mode::ScrollViewMode;

/// Help bar content for different modes.
pub struct HelpBar;

impl HelpBar {
    /// Returns help text for the given mode.
    pub fn text_for_mode(mode: &ScrollViewMode) -> &'static str {
        match mode {
            ScrollViewMode::Normal => {
                "PgUp/Dn:scroll  Home/End:top/bot  Ctrl+S:search  Ctrl+F:filter  Ctrl+G:goto  Ctrl+L:lines  Ctrl+T:time  ?:hide"
            }
            ScrollViewMode::Search(_) => {
                "Enter:confirm  Esc:cancel  Up/Down:prev/next match  Ctrl+F:filter"
            }
            ScrollViewMode::Filter(_) => {
                "Enter:confirm  Esc:cancel  Ctrl+S:search  PgUp/Dn:scroll  Home/End:top/bot"
            }
            ScrollViewMode::Yank(_) => "v:toggle line  y/Enter:copy  Esc:cancel  Arrows:move",
            ScrollViewMode::GoToLine(_) => "Enter:go  Esc:cancel",
        }
    }

    /// Renders the help bar at the bottom row of the terminal.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `mode` - Current scroll viewer mode
    /// * `cols` - Terminal width
    /// * `rows` - Terminal height
    /// * `theme` - Theme for colors
    pub fn render<W: Write>(
        out: &mut W,
        mode: &ScrollViewMode,
        cols: u16,
        rows: u16,
        theme: &Theme,
    ) -> io::Result<()> {
        use crate::chrome::segments::{color_to_bg_ansi, color_to_fg_ansi};

        let text = Self::text_for_mode(mode);
        let bg = color_to_bg_ansi(theme.help_bar_bg);
        let fg = color_to_fg_ansi(theme.help_bar_fg);
        let key_fg = color_to_fg_ansi(theme.help_bar_key);
        let reset = "\x1b[0m";

        // Move to last row and clear
        write!(out, "\x1b[{};1H\x1b[2K", rows)?;

        // Apply background to entire row
        write!(out, "{}{:width$}{}", bg, "", reset, width = cols as usize)?;
        write!(out, "\x1b[{};1H{}", rows, bg)?;

        // Render help text with keys highlighted
        // Format: "Key:action  Key:action" - highlight the key part
        let mut chars = text.chars().peekable();
        let mut in_key = true;

        while let Some(c) = chars.next() {
            if c == ':' {
                write!(out, "{}{}", fg, c)?;
                in_key = false;
            } else if c == ' ' && chars.peek() == Some(&' ') {
                // Double space indicates separator between bindings
                write!(out, "  ")?;
                chars.next(); // consume second space
                in_key = true;
            } else if in_key {
                write!(out, "{}{}", key_fg, c)?;
            } else {
                write!(out, "{}{}", fg, c)?;
            }
        }

        write!(out, "{}", reset)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrollback::features::SearchState;

    #[test]
    fn test_help_text_for_modes() {
        let normal_help = HelpBar::text_for_mode(&ScrollViewMode::Normal);
        assert!(normal_help.contains("PgUp"));
        assert!(normal_help.contains("Ctrl+S"));

        let search_help = HelpBar::text_for_mode(&ScrollViewMode::Search(SearchState::default()));
        assert!(search_help.contains("Up/Down"));
        assert!(search_help.contains("Esc"));
    }
}
