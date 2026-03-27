//! Selection ribbon widget for displaying checked tree items as a command preview.
//!
//! Renders selected items (command, subcommands, flags) in a compact 1-3 row
//! display between the tree area and footer, with optional scrolling marquee
//! for overflow.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::Paragraph;

use crate::chrome::theme::Theme;
use crate::ui::scrolling_text::ScrollingText;

/// Prompt prefix rendered at the start of each ribbon row (" $ ")
const PROMPT_PREFIX: &str = " $ ";
/// Width of the prompt prefix in display columns
const PROMPT_PREFIX_WIDTH: usize = 3;

/// A single item in the selection ribbon.
#[derive(Debug, Clone)]
pub struct RibbonItem {
    /// Display text (command name, flag, subcommand, etc.)
    pub text: String,
    /// Whether this item was auto-implied (parent of a selected child).
    pub auto_implied: bool,
}

/// Renders a selection ribbon showing checked items as a command preview.
pub struct SelectionRibbon;

impl SelectionRibbon {
    /// Computes the number of rows needed to display all items.
    ///
    /// Returns 0 if there are no items, 1-3 based on content width.
    /// Caps at `max_rows` (typically 3).
    pub fn compute_height(items: &[RibbonItem], viewport_width: u16, max_rows: u16) -> u16 {
        if items.is_empty() || viewport_width < 4 {
            return 0;
        }

        let usable_width = viewport_width.saturating_sub(PROMPT_PREFIX_WIDTH as u16) as usize;
        let total_text_width = Self::total_text_width(items);

        if total_text_width <= usable_width {
            1
        } else if total_text_width <= usable_width * 2 {
            2.min(max_rows)
        } else {
            3.min(max_rows)
        }
    }

    /// Renders the selection ribbon into the provided area.
    pub fn render(
        items: &[RibbonItem],
        buffer: &mut Buffer,
        area: Rect,
        theme: &Theme,
        frame: u64,
    ) {
        if items.is_empty() || area.height == 0 || area.width < 4 {
            return;
        }

        let bg = theme.help_bar_bg;
        let prompt_style = Style::default().fg(theme.git_fg).bg(bg);
        let cmd_style = Style::default()
            .fg(theme.text_primary)
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let flag_style = Style::default().fg(theme.text_highlight).bg(bg);
        let pipe_style = Style::default()
            .fg(theme.text_secondary)
            .bg(bg)
            .add_modifier(Modifier::BOLD);
        let bg_style = Style::default().bg(bg);

        // Paint background across all ribbon rows
        for row in 0..area.height {
            let row_area = Rect::new(area.x, area.y + row, area.width, 1);
            buffer.set_style(row_area, bg_style);
        }

        // Build the full command string for width calculations
        let full_text = Self::build_command_string(items);
        let usable_width = area.width.saturating_sub(PROMPT_PREFIX_WIDTH as u16) as usize;

        if area.height == 1 {
            // Single row — use marquee if overflowing
            let prefixed = format!("{}{}" , PROMPT_PREFIX, full_text);
            if crate::ui::text_width::display_width(&prefixed) <= area.width as usize {
                // Fits in one row: render with styles
                let spans =
                    Self::build_spans(items, prompt_style, cmd_style, flag_style, pipe_style);
                Paragraph::new(Line::from(spans)).render(area, buffer);
            } else {
                // Overflow: use scrolling marquee
                let scroller = ScrollingText::new(&prefixed).hold_frames(12).gap_cols(4);
                let text = scroller.frame_text(area.width as usize, frame);
                let line = Line::from(Span::styled(text, cmd_style));
                Paragraph::new(line).render(area, buffer);
            }
        } else {
            // Multi-row: wrap items across rows
            let rows = Self::wrap_into_rows(items, usable_width, area.height as usize);
            for (row_idx, row_items) in rows.iter().enumerate() {
                if row_idx >= area.height as usize {
                    break;
                }
                let row_area = Rect::new(area.x, area.y + row_idx as u16, area.width, 1);
                let is_last_row = row_idx == rows.len() - 1;
                let mut spans = Vec::new();
                if row_idx == 0 {
                    spans.push(Span::styled(PROMPT_PREFIX, prompt_style));
                } else {
                    spans.push(Span::styled("   ", bg_style));
                }

                // Check if last row overflows and needs marquee
                if is_last_row {
                    let row_text: String = row_items
                        .iter()
                        .map(|i| i.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let row_display_width =
                        crate::ui::text_width::display_width(&row_text) + PROMPT_PREFIX_WIDTH;
                    if row_display_width > area.width as usize {
                        // Last row overflows: use marquee for entire row
                        let marquee_text = format!("{}{}", PROMPT_PREFIX, row_text);
                        let scroller =
                            ScrollingText::new(&marquee_text).hold_frames(12).gap_cols(4);
                        let text = scroller.frame_text(area.width as usize, frame);
                        let line = Line::from(Span::styled(text, cmd_style));
                        Paragraph::new(line).render(row_area, buffer);
                        continue;
                    }
                }

                for (i, item) in row_items.iter().enumerate() {
                    if i > 0 {
                        spans.push(Span::styled(" ", bg_style));
                    }
                    let style = if item.text == "|" {
                        pipe_style
                    } else if item.text.starts_with('-') {
                        flag_style
                    } else {
                        cmd_style
                    };
                    spans.push(Span::styled(&item.text, style));
                }
                Paragraph::new(Line::from(spans)).render(row_area, buffer);
            }
        }
    }

    /// Builds the full command string from ribbon items.
    fn build_command_string(items: &[RibbonItem]) -> String {
        items
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Builds styled spans for a single-row ribbon.
    fn build_spans<'a>(
        items: &'a [RibbonItem],
        prompt_style: Style,
        cmd_style: Style,
        flag_style: Style,
        pipe_style: Style,
    ) -> Vec<Span<'a>> {
        let mut spans = vec![Span::styled(PROMPT_PREFIX, prompt_style)];
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
            }
            let style = if item.text == "|" {
                pipe_style
            } else if item.text.starts_with('-') {
                flag_style
            } else {
                cmd_style
            };
            spans.push(Span::styled(item.text.as_str(), style));
        }
        spans
    }

    /// Wraps items into rows that fit within the given width.
    /// The provided `usable_width` already accounts for the prompt prefix.
    fn wrap_into_rows(items: &[RibbonItem], usable_width: usize, max_rows: usize) -> Vec<Vec<&RibbonItem>> {
        let mut rows: Vec<Vec<&RibbonItem>> = vec![vec![]];
        let mut current_width = 0usize;

        for item in items {
            let item_width = crate::ui::text_width::display_width(&item.text);
            let needed = if current_width == 0 {
                item_width
            } else {
                item_width + 1 // +1 for space separator
            };

            if current_width + needed > usable_width && !rows.last().unwrap().is_empty() {
                if rows.len() >= max_rows {
                    // Can't add more rows; squeeze into last row
                    rows.last_mut().unwrap().push(item);
                    current_width += needed;
                } else {
                    rows.push(vec![item]);
                    current_width = item_width;
                }
            } else {
                current_width += needed;
                rows.last_mut().unwrap().push(item);
            }
        }

        rows
    }

    /// Total display width of all items joined by spaces.
    fn total_text_width(items: &[RibbonItem]) -> usize {
        if items.is_empty() {
            return 0;
        }
        let text_width: usize = items
            .iter()
            .map(|i| crate::ui::text_width::display_width(&i.text))
            .sum();
        text_width + items.len() - 1 // spaces between items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str) -> RibbonItem {
        RibbonItem {
            text: text.to_string(),
            auto_implied: false,
        }
    }

    fn implied(text: &str) -> RibbonItem {
        RibbonItem {
            text: text.to_string(),
            auto_implied: true,
        }
    }

    #[test]
    fn test_compute_height_empty() {
        assert_eq!(SelectionRibbon::compute_height(&[], 80, 3), 0);
    }

    #[test]
    fn test_compute_height_single_row() {
        let items = vec![item("git"), item("commit")];
        assert_eq!(SelectionRibbon::compute_height(&items, 80, 3), 1);
    }

    #[test]
    fn test_compute_height_multi_row() {
        let items: Vec<RibbonItem> = (0..20)
            .map(|i| item(&format!("--flag-{i}")))
            .collect();
        let h = SelectionRibbon::compute_height(&items, 40, 3);
        assert!(h > 1);
    }

    #[test]
    fn test_build_command_string() {
        let items = vec![item("git"), item("commit"), item("--verbose")];
        assert_eq!(
            SelectionRibbon::build_command_string(&items),
            "git commit --verbose"
        );
    }

    #[test]
    fn test_total_text_width() {
        let items = vec![item("git"), item("commit")];
        // "git" (3) + " " (1) + "commit" (6) = 10
        assert_eq!(SelectionRibbon::total_text_width(&items), 10);
    }

    #[test]
    fn test_wrap_into_rows_single() {
        let items = vec![item("git"), item("commit")];
        let rows = SelectionRibbon::wrap_into_rows(&items, 80, 3);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
    }

    #[test]
    fn test_wrap_into_rows_overflow() {
        let items = vec![item("git"), item("commit"), item("--verbose"), item("--message")];
        let rows = SelectionRibbon::wrap_into_rows(&items, 15, 3);
        assert!(rows.len() > 1);
    }

    #[test]
    fn test_implied_items() {
        let items = vec![implied("git"), implied("commit"), item("--verbose")];
        // Implied items still appear in the command string
        assert_eq!(
            SelectionRibbon::build_command_string(&items),
            "git commit --verbose"
        );
    }
}
