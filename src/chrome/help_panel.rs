//! Help panel for displaying keybindings and usage information.

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};

use super::panel::{Panel, PanelResult};
use super::theme::Theme;

/// A section of help content.
#[derive(Debug, Clone)]
pub struct HelpSection {
    /// Section title.
    pub title: String,
    /// Key-description pairs.
    pub entries: Vec<(String, String)>,
}

/// Help panel.
pub struct HelpPanel {
    /// Help sections.
    sections: Vec<HelpSection>,
    /// Scroll offset for display.
    scroll_offset: usize,
    /// Total number of lines (for scrolling).
    total_lines: usize,
    /// Theme for rendering.
    theme: &'static Theme,
}

impl HelpPanel {
    /// Creates a new help panel with documentation.
    pub fn new(theme: &'static Theme) -> Self {
        let sections = vec![
            HelpSection {
                title: "Panel Navigation".to_string(),
                entries: vec![
                    ("Ctrl+Space".to_string(), "Open panels".to_string()),
                    ("Tab".to_string(), "Next tab".to_string()),
                    ("Shift+Tab".to_string(), "Previous tab".to_string()),
                    ("Esc".to_string(), "Close panels".to_string()),
                ],
            },
            HelpSection {
                title: "Command Palette".to_string(),
                entries: vec![
                    ("Type".to_string(), "Filter commands".to_string()),
                    ("Up/Down".to_string(), "Navigate list".to_string()),
                    ("Enter".to_string(), "Execute selected".to_string()),
                    ("Backspace".to_string(), "Clear filter".to_string()),
                ],
            },
            HelpSection {
                title: "File Browser".to_string(),
                entries: vec![
                    (
                        "Enter".to_string(),
                        "Open directory / insert path".to_string(),
                    ),
                    (
                        "Backspace".to_string(),
                        "Go to parent directory".to_string(),
                    ),
                    ("Up/Down".to_string(), "Navigate files".to_string()),
                    ("Ctrl+H or .".to_string(), "Toggle hidden files".to_string()),
                ],
            },
            HelpSection {
                title: "History Browser".to_string(),
                entries: vec![
                    ("Type".to_string(), "Filter history".to_string()),
                    ("Up/Down".to_string(), "Navigate history".to_string()),
                    ("Enter".to_string(), "Execute selected".to_string()),
                ],
            },
            HelpSection {
                title: "Edit Mode Keybindings".to_string(),
                entries: vec![
                    (
                        "Ctrl+A".to_string(),
                        "Move to beginning of line".to_string(),
                    ),
                    ("Ctrl+E".to_string(), "Move to end of line".to_string()),
                    ("Ctrl+K".to_string(), "Kill to end of line".to_string()),
                    (
                        "Ctrl+U".to_string(),
                        "Kill to beginning of line".to_string(),
                    ),
                    ("Ctrl+W".to_string(), "Kill previous word".to_string()),
                    ("Ctrl+Y".to_string(), "Yank killed text".to_string()),
                    ("Ctrl+R".to_string(), "Reverse history search".to_string()),
                    ("Ctrl+C".to_string(), "Clear line".to_string()),
                    ("Ctrl+D".to_string(), "Exit (on empty line)".to_string()),
                    ("Tab".to_string(), "Tab completion".to_string()),
                    ("Up/Down".to_string(), "History navigation".to_string()),
                ],
            },
        ];

        // Calculate total lines
        let total_lines: usize = sections
            .iter()
            .map(|s| 1 + s.entries.len() + 1) // title + entries + blank line
            .sum();

        Self {
            sections,
            scroll_offset: 0,
            total_lines,
            theme,
        }
    }
}

// Note: Default is removed since HelpPanel now requires a theme parameter

impl Panel for HelpPanel {
    fn preferred_height(&self) -> u16 {
        15
    }

    fn title(&self) -> &str {
        "Help"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        let visible_height = area.height as usize;

        // Build all lines
        let mut lines: Vec<ListItem> = Vec::new();

        for section in &self.sections {
            // Section title
            lines.push(ListItem::new(Line::from(vec![Span::styled(
                &section.title,
                Style::default()
                    .fg(self.theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            )])));

            // Entries
            for (key, desc) in &section.entries {
                let key_width = 15;
                let padded_key = crate::ui::text_width::pad_to_width(key, key_width);
                lines.push(ListItem::new(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(padded_key, Style::default().fg(self.theme.text_highlight)),
                    Span::styled(desc, Style::default().fg(self.theme.text_primary)),
                ])));
            }

            // Blank line after section
            lines.push(ListItem::new(Line::from("")));
        }

        // Clamp scroll_offset to prevent scrolling past content
        let max_offset = self.total_lines.saturating_sub(visible_height);
        self.scroll_offset = self.scroll_offset.min(max_offset);

        // Apply scroll offset
        let visible_lines: Vec<ListItem> = lines
            .into_iter()
            .skip(self.scroll_offset)
            .take(visible_height)
            .collect();

        let list = List::new(visible_lines);
        list.render(area, buffer);
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        match key.code {
            KeyCode::Esc => PanelResult::Dismiss,
            KeyCode::Up => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
                PanelResult::Continue
            }
            KeyCode::Down => {
                if self.scroll_offset + 1 < self.total_lines {
                    self.scroll_offset += 1;
                }
                PanelResult::Continue
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                PanelResult::Continue
            }
            KeyCode::PageDown => {
                self.scroll_offset =
                    (self.scroll_offset + 10).min(self.total_lines.saturating_sub(1));
                PanelResult::Continue
            }
            _ => PanelResult::Continue,
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    fn test_help_panel_new() {
        let panel = HelpPanel::new(&AMBER_THEME);
        assert!(!panel.sections.is_empty());
        assert_eq!(panel.scroll_offset, 0);
    }

    #[test]
    fn test_help_panel_sections() {
        let panel = HelpPanel::new(&AMBER_THEME);
        assert!(panel.sections.iter().any(|s| s.title == "Panel Navigation"));
        assert!(panel.sections.iter().any(|s| s.title == "Command Palette"));
        assert!(panel.sections.iter().any(|s| s.title == "File Browser"));
    }
}
