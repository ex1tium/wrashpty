//! History browser panel for browsing command history.

use std::any::Any;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};
use ratatui_widgets::paragraph::Paragraph;
use tracing::debug;

use super::panel::{Panel, PanelResult};

/// A history entry.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// The command that was executed.
    pub command: String,
    /// Duration of execution, if known.
    pub duration: Option<Duration>,
    /// Exit code, if known.
    pub exit_code: Option<i32>,
    /// Timestamp of execution, if known.
    pub timestamp: Option<SystemTime>,
    /// Working directory, if known.
    pub cwd: Option<PathBuf>,
}

/// History browser panel.
pub struct HistoryBrowserPanel {
    /// All history entries.
    entries: Vec<HistoryEntry>,
    /// Indices of filtered entries.
    filtered: Vec<usize>,
    /// Currently selected index in filtered list.
    selection: usize,
    /// Scroll offset for display.
    scroll_offset: usize,
    /// Current filter text.
    filter: String,
}

impl HistoryBrowserPanel {
    /// Creates a new empty history browser.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            filtered: Vec::new(),
            selection: 0,
            scroll_offset: 0,
            filter: String::new(),
        }
    }

    /// Loads history from the history file.
    pub fn load_history(&mut self) {
        self.entries.clear();

        // Load from wrashpty or bash history
        let history_entries = crate::history::load_history().unwrap_or_else(|e| {
            debug!("Failed to load history: {}", e);
            Vec::new()
        });

        // Convert to HistoryEntry, newest first
        self.entries = history_entries
            .into_iter()
            .rev()
            .map(|cmd| HistoryEntry {
                command: cmd,
                duration: None,
                exit_code: None,
                timestamp: None,
                cwd: None,
            })
            .collect();

        self.apply_filter();

        debug!("Loaded {} history entries", self.entries.len());
    }

    /// Applies the current filter to the entry list.
    fn apply_filter(&mut self) {
        self.filtered.clear();

        if self.filter.is_empty() {
            self.filtered = (0..self.entries.len()).collect();
        } else {
            let filter_lower = self.filter.to_lowercase();
            for (i, entry) in self.entries.iter().enumerate() {
                if entry.command.to_lowercase().contains(&filter_lower) {
                    self.filtered.push(i);
                }
            }
        }

        self.selection = 0;
        self.scroll_offset = 0;
    }

    /// Ensures the selection is visible in the scroll window.
    fn ensure_visible(&mut self, visible_count: usize) {
        if self.selection < self.scroll_offset {
            self.scroll_offset = self.selection;
        } else if self.selection >= self.scroll_offset + visible_count {
            self.scroll_offset = self.selection.saturating_sub(visible_count - 1);
        }
    }

    /// Returns the currently selected command, if any.
    fn selected_command(&self) -> Option<&str> {
        self.filtered
            .get(self.selection)
            .and_then(|&i| self.entries.get(i))
            .map(|e| e.command.as_str())
    }
}

impl Default for HistoryBrowserPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl Panel for HistoryBrowserPanel {
    fn preferred_height(&self) -> u16 {
        10
    }

    fn title(&self) -> &str {
        "History"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // Create layout: filter input at top, list below
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);

        // Render filter input
        let filter_text = if self.filter.is_empty() {
            Span::styled("Type to filter...", Style::default().fg(Color::DarkGray))
        } else {
            Span::raw(&self.filter)
        };
        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Magenta)),
            filter_text,
            Span::styled(
                format!(" ({} matches)", self.filtered.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        Paragraph::new(filter_line).render(chunks[0], buffer);

        // Calculate visible items
        let visible_height = chunks[1].height as usize;
        self.ensure_visible(visible_height);

        // Render entry list
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .skip(self.scroll_offset)
            .take(visible_height)
            .enumerate()
            .map(|(display_idx, &entry_idx)| {
                let entry = &self.entries[entry_idx];
                let actual_idx = self.scroll_offset + display_idx;
                let is_selected = actual_idx == self.selection;

                // Truncate command if too long
                let max_cmd_len = area.width.saturating_sub(4) as usize;
                let cmd_display = if entry.command.len() > max_cmd_len {
                    format!("{}...", &entry.command[..max_cmd_len.saturating_sub(3)])
                } else {
                    entry.command.clone()
                };

                let cmd_style = if is_selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let line = Line::from(vec![Span::styled(cmd_display, cmd_style)]);

                if is_selected {
                    ListItem::new(line).style(Style::default().bg(Color::DarkGray))
                } else {
                    ListItem::new(line)
                }
            })
            .collect();

        let list = List::new(items);
        list.render(chunks[1], buffer);
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        match key.code {
            KeyCode::Esc => PanelResult::Dismiss,
            KeyCode::Enter => {
                if let Some(cmd) = self.selected_command() {
                    PanelResult::Execute(cmd.to_string())
                } else {
                    PanelResult::Dismiss
                }
            }
            KeyCode::Up => {
                if self.selection > 0 {
                    self.selection -= 1;
                }
                PanelResult::Continue
            }
            KeyCode::Down => {
                if self.selection + 1 < self.filtered.len() {
                    self.selection += 1;
                }
                PanelResult::Continue
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.apply_filter();
                PanelResult::Continue
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.apply_filter();
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
    use super::*;

    #[test]
    fn test_history_browser_new() {
        let panel = HistoryBrowserPanel::new();
        assert!(panel.entries.is_empty());
        assert!(panel.filtered.is_empty());
        assert_eq!(panel.selection, 0);
    }

    #[test]
    fn test_apply_filter_empty() {
        let mut panel = HistoryBrowserPanel::new();
        panel.entries.push(HistoryEntry {
            command: "echo hello".to_string(),
            duration: None,
            exit_code: None,
            timestamp: None,
            cwd: None,
        });
        panel.apply_filter();
        assert_eq!(panel.filtered.len(), 1);
    }

    #[test]
    fn test_apply_filter_match() {
        let mut panel = HistoryBrowserPanel::new();
        panel.entries.push(HistoryEntry {
            command: "cargo build".to_string(),
            duration: None,
            exit_code: None,
            timestamp: None,
            cwd: None,
        });
        panel.entries.push(HistoryEntry {
            command: "cargo test".to_string(),
            duration: None,
            exit_code: None,
            timestamp: None,
            cwd: None,
        });
        panel.entries.push(HistoryEntry {
            command: "ls -la".to_string(),
            duration: None,
            exit_code: None,
            timestamp: None,
            cwd: None,
        });
        panel.filter = "cargo".to_string();
        panel.apply_filter();
        assert_eq!(panel.filtered.len(), 2);
    }
}
