//! File browser panel for navigating the filesystem.

use std::any::Any;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};
use ratatui_widgets::paragraph::Paragraph;
use tracing::debug;

use super::panel::{Panel, PanelResult};

/// A directory entry in the file browser.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// File or directory name.
    pub name: String,
    /// Full path.
    pub path: PathBuf,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// File size in bytes.
    pub size: u64,
    /// Last modification time.
    pub modified: Option<SystemTime>,
}

/// File browser panel.
pub struct FileBrowserPanel {
    /// Current directory being browsed.
    current_dir: PathBuf,
    /// Directory entries.
    entries: Vec<DirEntry>,
    /// Currently selected index.
    selection: usize,
    /// Scroll offset for display.
    scroll_offset: usize,
    /// Whether to show hidden files.
    show_hidden: bool,
}

impl FileBrowserPanel {
    /// Creates a new file browser at the current directory.
    pub fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut panel = Self {
            current_dir: current_dir.clone(),
            entries: Vec::new(),
            selection: 0,
            scroll_offset: 0,
            show_hidden: false,
        };
        let _ = panel.refresh();
        panel
    }

    /// Navigates to the given path.
    pub fn navigate_to(&mut self, path: &Path) -> std::io::Result<()> {
        let canonical = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.current_dir.join(path)
        };

        if canonical.is_dir() {
            self.current_dir = canonical;
            self.refresh()?;
        }

        Ok(())
    }

    /// Refreshes the directory listing.
    fn refresh(&mut self) -> std::io::Result<()> {
        self.entries.clear();

        let read_dir = fs::read_dir(&self.current_dir)?;

        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files if not showing them
            if !self.show_hidden && name.starts_with('.') {
                continue;
            }

            let path = entry.path();
            let metadata = entry.metadata().ok();
            let is_dir = path.is_dir();
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = metadata.and_then(|m| m.modified().ok());

            self.entries.push(DirEntry {
                name,
                path,
                is_dir,
                size,
                modified,
            });
        }

        // Sort: directories first, then alphabetically
        self.entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });

        self.selection = 0;
        self.scroll_offset = 0;

        debug!(
            "Refreshed file browser: {} entries in {}",
            self.entries.len(),
            self.current_dir.display()
        );

        Ok(())
    }

    /// Navigates to the parent directory.
    fn go_parent(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            let parent_path = parent.to_path_buf();
            let _ = self.navigate_to(&parent_path);
        }
    }

    /// Toggles display of hidden files.
    fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        let _ = self.refresh();
    }

    /// Ensures the selection is visible in the scroll window.
    fn ensure_visible(&mut self, visible_count: usize) {
        if self.selection < self.scroll_offset {
            self.scroll_offset = self.selection;
        } else if self.selection >= self.scroll_offset + visible_count {
            self.scroll_offset = self.selection.saturating_sub(visible_count - 1);
        }
    }

    /// Returns the currently selected entry, if any.
    fn selected_entry(&self) -> Option<&DirEntry> {
        self.entries.get(self.selection)
    }
}

impl Default for FileBrowserPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Formats a file size in human-readable form.
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}M", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}K", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}

impl Panel for FileBrowserPanel {
    fn preferred_height(&self) -> u16 {
        12
    }

    fn title(&self) -> &str {
        "Files"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // Create layout: path header at top, list below
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);

        // Render path header
        let path_str = self.current_dir.to_string_lossy();
        let truncated_path = if path_str.len() > area.width as usize - 4 {
            format!(
                "...{}",
                &path_str[path_str.len() - (area.width as usize - 7)..]
            )
        } else {
            path_str.to_string()
        };
        let header = Line::from(vec![
            Span::styled(" ", Style::default().fg(Color::Cyan)),
            Span::styled(
                truncated_path,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            if self.show_hidden {
                Span::styled(" [H]", Style::default().fg(Color::DarkGray))
            } else {
                Span::raw("")
            },
        ]);
        Paragraph::new(header).render(chunks[0], buffer);

        // Calculate visible items
        let visible_height = chunks[1].height as usize;
        self.ensure_visible(visible_height);

        // Render entry list
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .skip(self.scroll_offset)
            .take(visible_height)
            .enumerate()
            .map(|(display_idx, entry)| {
                let actual_idx = self.scroll_offset + display_idx;
                let is_selected = actual_idx == self.selection;

                let icon = if entry.is_dir { "" } else { "" };
                let icon_color = if entry.is_dir {
                    Color::Blue
                } else {
                    Color::Gray
                };

                let name_style = if is_selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if entry.is_dir {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default().fg(Color::White)
                };

                let size_str = if entry.is_dir {
                    "     ".to_string()
                } else {
                    format!("{:>5}", format_size(entry.size))
                };

                let line = Line::from(vec![
                    Span::styled(format!("{} ", icon), Style::default().fg(icon_color)),
                    Span::styled(&entry.name, name_style),
                    Span::styled(
                        format!("  {}", size_str),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);

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
                if let Some(entry) = self.selected_entry().cloned() {
                    if entry.is_dir {
                        let _ = self.navigate_to(&entry.path);
                        PanelResult::Continue
                    } else {
                        // Insert the file path
                        PanelResult::InsertText(entry.path.to_string_lossy().to_string())
                    }
                } else {
                    PanelResult::Continue
                }
            }
            KeyCode::Backspace => {
                self.go_parent();
                PanelResult::Continue
            }
            KeyCode::Up => {
                if self.selection > 0 {
                    self.selection -= 1;
                }
                PanelResult::Continue
            }
            KeyCode::Down => {
                if self.selection + 1 < self.entries.len() {
                    self.selection += 1;
                }
                PanelResult::Continue
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_hidden();
                PanelResult::Continue
            }
            KeyCode::Char('.') => {
                self.toggle_hidden();
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
    fn test_file_browser_new() {
        let panel = FileBrowserPanel::new();
        assert!(!panel.show_hidden);
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(1024), "1.0K");
        assert_eq!(format_size(1536), "1.5K");
        assert_eq!(format_size(1048576), "1.0M");
        assert_eq!(format_size(1073741824), "1.0G");
    }

    #[test]
    fn test_toggle_hidden() {
        let mut panel = FileBrowserPanel::new();
        assert!(!panel.show_hidden);
        panel.toggle_hidden();
        assert!(panel.show_hidden);
        panel.toggle_hidden();
        assert!(!panel.show_hidden);
    }
}
