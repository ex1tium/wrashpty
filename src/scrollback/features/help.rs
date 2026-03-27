//! Legend bar content for scroll viewer modes.
//!
//! Provides context-sensitive key legends displayed on the bottom row.

use std::io::{self, Write};

use crate::chrome::theme::Theme;
use crate::scrollback::mode::HelpContext;
use crate::scrollback::state::DisplaySettings;
use crate::ui::text_width;

/// One legend binding entry.
pub struct LegendEntry {
    /// Key chord text, e.g. "^S", "PgUp/Dn", "Esc".
    pub key: &'static str,
    /// Human label, e.g. "Search", "Scroll", "Exit".
    pub label: &'static str,
    /// Optional toggle state.
    pub active: Option<bool>,
}

/// Legend bar for different viewer contexts.
pub struct LegendBar;

impl LegendBar {
    fn entries_for_context(ctx: HelpContext, display: &DisplaySettings) -> Vec<LegendEntry> {
        match ctx {
            HelpContext::Normal => vec![
                LegendEntry {
                    key: "PgUp/Dn",
                    label: "Scroll",
                    active: None,
                },
                LegendEntry {
                    key: "^P/N",
                    label: "Cmd",
                    active: None,
                },
                LegendEntry {
                    key: "^S",
                    label: "Search",
                    active: None,
                },
                LegendEntry {
                    key: "^F",
                    label: "Filter",
                    active: None,
                },
                LegendEntry {
                    key: "^G",
                    label: "Goto",
                    active: None,
                },
                LegendEntry {
                    key: "^L",
                    label: "Lines",
                    active: Some(display.line_numbers),
                },
                LegendEntry {
                    key: "^T",
                    label: "Time",
                    active: Some(display.timestamps),
                },
                LegendEntry {
                    key: "^B",
                    label: "Sep",
                    active: Some(display.command_separators),
                },
                LegendEntry {
                    key: "z",
                    label: "Fold",
                    active: None,
                },
                LegendEntry {
                    key: "x",
                    label: "Unfold",
                    active: None,
                },
                LegendEntry {
                    key: "X",
                    label: "UnfoldAll",
                    active: None,
                },
                LegendEntry {
                    key: "?",
                    label: "Help",
                    active: Some(display.help_bar),
                },
                LegendEntry {
                    key: "Esc",
                    label: "Exit",
                    active: None,
                },
            ],
            HelpContext::Search => vec![
                LegendEntry {
                    key: "Enter",
                    label: "Confirm",
                    active: None,
                },
                LegendEntry {
                    key: "Esc",
                    label: "Cancel",
                    active: None,
                },
                LegendEntry {
                    key: "↑/↓",
                    label: "Prev/Next",
                    active: None,
                },
                LegendEntry {
                    key: "^F",
                    label: "Filter",
                    active: None,
                },
            ],
            HelpContext::Filter => vec![
                LegendEntry {
                    key: "Enter",
                    label: "Confirm",
                    active: None,
                },
                LegendEntry {
                    key: "Esc",
                    label: "Cancel",
                    active: None,
                },
                LegendEntry {
                    key: "^S",
                    label: "Search",
                    active: None,
                },
                LegendEntry {
                    key: "PgUp/Dn",
                    label: "Scroll",
                    active: None,
                },
            ],
            HelpContext::GoToLine => vec![
                LegendEntry {
                    key: "Enter",
                    label: "Go",
                    active: None,
                },
                LegendEntry {
                    key: "Esc",
                    label: "Cancel",
                    active: None,
                },
            ],
        }
    }

    /// Renders the legend bar at the bottom row of the terminal.
    pub fn render<W: Write>(
        out: &mut W,
        ctx: HelpContext,
        display: &DisplaySettings,
        cols: u16,
        rows: u16,
        theme: &Theme,
    ) -> io::Result<()> {
        use crate::chrome::segments::{color_to_bg_ansi, color_to_fg_ansi};

        let entries = Self::entries_for_context(ctx, display);
        let bg = color_to_bg_ansi(theme.help_bar_bg);
        let fg = color_to_fg_ansi(theme.help_bar_fg);
        let key_fg = color_to_fg_ansi(theme.help_bar_key);
        let active_fg = color_to_fg_ansi(theme.semantic_success);
        let reset = "\x1b[0m";

        write!(out, "\x1b[{};1H\x1b[2K", rows)?;
        write!(out, "{}{:width$}{}", bg, "", reset, width = cols as usize)?;
        write!(out, "\x1b[{};1H{}{}", rows, bg, fg)?;

        let mut used = 0usize;
        let max_cols = cols as usize;
        for entry in entries {
            let entry_width =
                text_width::display_width(entry.key) + 1 + text_width::display_width(entry.label);
            let separator_width = if used == 0 { 0 } else { 2 };
            if used + separator_width + entry_width > max_cols {
                break;
            }

            if separator_width > 0 {
                write!(out, "  ")?;
                used += separator_width;
            }

            write!(out, "{}{} ", key_fg, entry.key)?;
            match entry.active {
                Some(true) => write!(out, "{}\x1b[1m{}\x1b[22m{}", active_fg, entry.label, fg)?,
                _ => write!(out, "{}{}", fg, entry.label)?,
            }
            used += entry_width;
        }

        write!(out, "{}", reset)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entries_for_context_when_normal_contains_expected_bindings() {
        let display = DisplaySettings::new();
        let entries = LegendBar::entries_for_context(HelpContext::Normal, &display);
        assert!(
            entries
                .iter()
                .any(|entry| entry.key == "^S" && entry.label == "Search")
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.key == "z" && entry.label == "Fold")
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.key == "x" && entry.label == "Unfold")
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.key == "X" && entry.label == "UnfoldAll")
        );
    }

    #[test]
    fn test_entries_for_context_when_search_contains_prev_next_binding() {
        let display = DisplaySettings::new();
        let entries = LegendBar::entries_for_context(HelpContext::Search, &display);
        assert!(entries.iter().any(|entry| entry.key == "↑/↓"));
    }
}
