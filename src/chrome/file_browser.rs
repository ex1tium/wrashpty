//! File browser panel for navigating the filesystem.

use std::any::Any;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};
use ratatui_widgets::paragraph::Paragraph;
use tracing::debug;

use super::command_knowledge::COMMAND_KNOWLEDGE;
use super::panel::{Panel, PanelResult};
use super::theme::Theme;
use crate::history_store::HistoryStore;
use crate::intelligence::FileContext;

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
    /// Unix permissions mode (e.g., 0o755).
    pub mode: u32,
}

/// Section being edited in file edit mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileEditSection {
    /// Command and subcommands before the filename.
    Prefix,
    /// The filename itself (non-editable, visual only).
    Filename,
    /// Additional arguments after the filename.
    Suffix,
}

/// State for file edit mode.
#[derive(Debug, Clone)]
struct FileEditModeState {
    /// The filename being operated on.
    filename: String,
    /// Full path to the file.
    filepath: PathBuf,
    /// Tokens before the filename (command, subcommands).
    prefix_tokens: Vec<String>,
    /// Tokens after the filename (additional arguments).
    suffix_tokens: Vec<String>,
    /// Currently selected section.
    selected_section: FileEditSection,
    /// Index within the current section's tokens.
    selected_index: usize,
    /// Current edit buffer.
    edit_buffer: String,
    /// Available suggestions for the current position.
    suggestions: Vec<String>,
    /// Index into suggestions (None = using custom value).
    suggestion_index: Option<usize>,
}

impl FileEditModeState {
    /// Creates a new file edit mode state.
    fn new(filename: String, filepath: PathBuf) -> Self {
        // Get file type recommendations as initial suggestions
        let suggestions: Vec<String> = COMMAND_KNOWLEDGE
            .commands_for_filetype(&filename)
            .iter()
            .map(|s| s.to_string())
            .collect();

        Self {
            filename,
            filepath,
            prefix_tokens: Vec::new(),
            suffix_tokens: Vec::new(),
            selected_section: FileEditSection::Prefix,
            selected_index: 0,
            edit_buffer: String::new(),
            suggestions,
            suggestion_index: None,
        }
    }

    /// Adds external suggestions (e.g., from intelligence) to the front.
    fn add_suggestions(&mut self, external: Vec<String>) {
        // Merge: external first (more relevant), then existing
        let mut merged = Vec::new();
        for sugg in external {
            if !merged.contains(&sugg) {
                merged.push(sugg);
            }
        }
        for sugg in &self.suggestions {
            if !merged.contains(sugg) {
                merged.push(sugg.clone());
            }
        }
        self.suggestions = merged;
    }

    /// Cycles through suggestions in the given direction.
    fn cycle_suggestion(&mut self, direction: i32) {
        if self.suggestions.is_empty() {
            return;
        }

        let new_index = match self.suggestion_index {
            None => {
                if direction > 0 { 0 } else { self.suggestions.len() - 1 }
            }
            Some(idx) => {
                let len = self.suggestions.len();
                if direction > 0 {
                    (idx + 1) % len
                } else {
                    (idx + len - 1) % len
                }
            }
        };

        self.suggestion_index = Some(new_index);
        self.edit_buffer = self.suggestions[new_index].clone();
    }

    /// Returns the previous suggestion for display.
    fn prev_suggestion(&self) -> Option<&str> {
        if self.suggestions.is_empty() {
            return None;
        }
        let idx = self.suggestion_index.unwrap_or(0);
        let len = self.suggestions.len();
        let prev_idx = if idx == 0 { len - 1 } else { idx - 1 };
        self.suggestions.get(prev_idx).map(|s| s.as_str())
    }

    /// Returns the next suggestion for display.
    fn next_suggestion(&self) -> Option<&str> {
        if self.suggestions.is_empty() {
            return None;
        }
        let idx = self.suggestion_index.unwrap_or(0);
        let len = self.suggestions.len();
        let next_idx = (idx + 1) % len;
        self.suggestions.get(next_idx).map(|s| s.as_str())
    }

    /// Commits the current edit buffer to the appropriate token list.
    fn commit_edit(&mut self) {
        if self.edit_buffer.is_empty() {
            return;
        }

        match self.selected_section {
            FileEditSection::Prefix => {
                if self.selected_index < self.prefix_tokens.len() {
                    self.prefix_tokens[self.selected_index] = self.edit_buffer.clone();
                } else {
                    self.prefix_tokens.push(self.edit_buffer.clone());
                }
            }
            FileEditSection::Suffix => {
                if self.selected_index < self.suffix_tokens.len() {
                    self.suffix_tokens[self.selected_index] = self.edit_buffer.clone();
                } else {
                    self.suffix_tokens.push(self.edit_buffer.clone());
                }
            }
            FileEditSection::Filename => {}
        }
    }

    /// Moves to the next section.
    fn next_section(&mut self) {
        self.commit_edit();
        self.selected_section = match self.selected_section {
            FileEditSection::Prefix => FileEditSection::Suffix,
            FileEditSection::Filename => FileEditSection::Suffix,
            FileEditSection::Suffix => FileEditSection::Prefix,
        };
        self.selected_index = 0;
        self.edit_buffer.clear();
        self.suggestion_index = None;
    }

    /// Moves to the previous section.
    fn prev_section(&mut self) {
        self.commit_edit();
        self.selected_section = match self.selected_section {
            FileEditSection::Prefix => FileEditSection::Suffix,
            FileEditSection::Filename => FileEditSection::Prefix,
            FileEditSection::Suffix => FileEditSection::Prefix,
        };
        self.selected_index = 0;
        self.edit_buffer.clear();
        self.suggestion_index = None;
    }

    /// Builds the complete command from all parts.
    fn build_command(&mut self) -> String {
        self.commit_edit();
        let mut parts = Vec::new();
        parts.extend(self.prefix_tokens.iter().cloned());
        // Shell-quote the filepath to handle spaces and special characters
        parts.push(shell_quote(&self.filepath.to_string_lossy()));
        parts.extend(self.suffix_tokens.iter().cloned());
        parts.join(" ")
    }

    /// Deletes the current token.
    fn delete_token(&mut self) {
        match self.selected_section {
            FileEditSection::Prefix => {
                if self.selected_index < self.prefix_tokens.len() {
                    self.prefix_tokens.remove(self.selected_index);
                    if self.selected_index >= self.prefix_tokens.len() && self.selected_index > 0 {
                        self.selected_index -= 1;
                    }
                }
            }
            FileEditSection::Suffix => {
                if self.selected_index < self.suffix_tokens.len() {
                    self.suffix_tokens.remove(self.selected_index);
                    if self.selected_index >= self.suffix_tokens.len() && self.selected_index > 0 {
                        self.selected_index -= 1;
                    }
                }
            }
            FileEditSection::Filename => {}
        }
        self.edit_buffer.clear();
    }

    /// Inserts a new token after the current position.
    fn insert_token_after(&mut self) {
        self.commit_edit();
        match self.selected_section {
            FileEditSection::Prefix => {
                self.prefix_tokens.insert(self.selected_index + 1, String::new());
                self.selected_index += 1;
            }
            FileEditSection::Suffix => {
                let idx = if self.suffix_tokens.is_empty() {
                    0
                } else {
                    (self.selected_index + 1).min(self.suffix_tokens.len())
                };
                self.suffix_tokens.insert(idx, String::new());
                self.selected_index = idx;
            }
            FileEditSection::Filename => {
                // Insert into suffix when on filename
                self.suffix_tokens.insert(0, String::new());
                self.selected_section = FileEditSection::Suffix;
                self.selected_index = 0;
            }
        }
        self.edit_buffer.clear();
        self.suggestion_index = None;
    }

    /// Updates suggestions based on current context.
    ///
    /// In suffix section, if the current token or preceding tokens contain a pipe,
    /// suggest pipeable commands instead of file-type commands.
    fn update_suggestions(&mut self) {
        if self.selected_section == FileEditSection::Suffix {
            // Check if current edit buffer starts after a pipe, or if any suffix token is just "|"
            let has_pipe_before = self.suffix_tokens.iter()
                .take(self.selected_index)
                .any(|t| t == "|" || t.ends_with('|'));

            // Also check if we're immediately after typing a pipe
            let editing_after_pipe = if self.selected_index > 0 {
                self.suffix_tokens.get(self.selected_index.saturating_sub(1))
                    .map(|t| t == "|")
                    .unwrap_or(false)
            } else {
                false
            };

            if has_pipe_before || editing_after_pipe {
                self.suggestions = COMMAND_KNOWLEDGE
                    .pipeable_commands()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                return;
            }
        }

        // Default: use file-type suggestions for prefix section
        if self.selected_section == FileEditSection::Prefix {
            self.suggestions = COMMAND_KNOWLEDGE
                .commands_for_filetype(&self.filename)
                .iter()
                .map(|s| s.to_string())
                .collect();
        } else {
            // For suffix without pipe, clear suggestions or provide generic ones
            self.suggestions.clear();
        }
    }
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
    /// Edit mode state (None when not in edit mode).
    edit_mode: Option<FileEditModeState>,
    /// Theme for rendering.
    theme: &'static Theme,
    /// Reference to the history store for intelligent suggestions.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
}

impl FileBrowserPanel {
    /// Creates a new file browser at the current directory.
    pub fn new(theme: &'static Theme) -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut panel = Self {
            current_dir: current_dir.clone(),
            entries: Vec::new(),
            selection: 0,
            scroll_offset: 0,
            show_hidden: false,
            edit_mode: None,
            theme,
            history_store: None,
        };
        let _ = panel.refresh();
        panel
    }

    /// Sets the history store for intelligent suggestions.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.history_store = Some(store);
    }

    /// Enters edit mode for the selected file.
    fn enter_edit_mode(&mut self) {
        if let Some(entry) = self.selected_entry().cloned() {
            if !entry.is_dir {
                debug!(file = %entry.name, "Entering file edit mode");
                let mut edit_state = FileEditModeState::new(entry.name.clone(), entry.path.clone());

                // Try to get intelligent suggestions
                if let Some(store) = &self.history_store {
                    if let Ok(store) = store.lock() {
                        if store.has_intelligence() {
                            let file_context = FileContext::new(&entry.name, entry.is_dir);
                            let intelligent_suggestions = store.intelligent_suggest(
                                &[], // No preceding tokens yet
                                "",  // No partial text
                                Some(self.current_dir.clone()),
                                Some(file_context),
                                None, // No last command context
                            );

                            // Convert Suggestion objects to strings
                            let suggestions: Vec<String> = intelligent_suggestions
                                .into_iter()
                                .map(|s| s.text)
                                .collect();

                            if !suggestions.is_empty() {
                                edit_state.add_suggestions(suggestions);
                            }
                        }
                    }
                }

                self.edit_mode = Some(edit_state);
            }
        }
    }

    /// Exits edit mode.
    fn exit_edit_mode(&mut self) {
        self.edit_mode = None;
    }

    /// Returns true if in edit mode.
    fn in_edit_mode(&self) -> bool {
        self.edit_mode.is_some()
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
            let modified = metadata.as_ref().and_then(|m| m.modified().ok());

            // Extract Unix permissions
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata
                    .as_ref()
                    .map(|m| m.permissions().mode())
                    .unwrap_or(if is_dir { 0o755 } else { 0o644 })
            };
            #[cfg(not(unix))]
            let mode = if is_dir { 0o755 } else { 0o644 };

            self.entries.push(DirEntry {
                name,
                path,
                is_dir,
                size,
                modified,
                mode,
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

// Note: Default is removed since FileBrowserPanel now requires a theme parameter

/// Shell-quotes a string to safely handle spaces and special characters.
///
/// Uses single quotes with proper escaping for embedded single quotes.
/// Example: "file name.txt" -> "'file name.txt'"
/// Example: "it's here" -> "'it'\\''s here'"
pub fn shell_quote(s: &str) -> String {
    // If the string contains no special characters, return as-is
    let needs_quoting = s.chars().any(|c| {
        matches!(c, ' ' | '\t' | '\n' | '"' | '\'' | '\\' | '$' | '`' | '!' | '*' | '?' | '[' | ']' | '{' | '}' | '(' | ')' | '<' | '>' | '|' | '&' | ';' | '#' | '~')
    });

    if !needs_quoting && !s.is_empty() {
        return s.to_string();
    }

    // Single-quote the string, escaping embedded single quotes
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
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

/// Formats Unix permissions as 3-digit octal.
fn format_permissions(mode: u32) -> String {
    format!("{:03o}", mode & 0o777)
}

/// Formats a date in compact form (Today, Yesterday, or Mon DD).
fn format_date_compact(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "-".to_string();
    };

    let Ok(duration) = time.elapsed() else {
        return "-".to_string();
    };

    let secs = duration.as_secs();
    let days = secs / 86400;

    if days == 0 {
        "Today".to_string()
    } else if days == 1 {
        "Yday".to_string()
    } else if days < 7 {
        format!("{}d", days)
    } else if days < 365 {
        // Format as "Mon DD" using rough month calculation
        let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun",
                      "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        // Rough approximation - get day of year and convert
        let day_of_year = (days % 365) as usize;
        let month_idx = (day_of_year / 30).min(11);
        let day = (day_of_year % 30) + 1;
        format!("{} {:2}", months[month_idx], day)
    } else {
        format!("{}y", days / 365)
    }
}

impl FileBrowserPanel {
    /// Renders the file edit mode UI.
    fn render_file_edit_mode(&self, buffer: &mut Buffer, area: Rect, state: &FileEditModeState) {
        // Layout: 12 rows (with spacer before Result for consistency with history browser)
        let chunks = Layout::vertical([
            Constraint::Length(1), // 0: Title with filename
            Constraint::Length(1), // 1: Separator
            Constraint::Length(1), // 2: Previous suggestion (dim)
            Constraint::Length(1), // 3: Command preview row
            Constraint::Length(1), // 4: Next suggestion (dim)
            Constraint::Length(1), // 5: Spacer after suggestions
            Constraint::Length(1), // 6: Edit input
            Constraint::Length(1), // 7: Spacer before Result
            Constraint::Length(1), // 8: Result preview
            Constraint::Min(1),    // 9: Flexible spacer
            Constraint::Length(1), // 10: Border
            Constraint::Length(1), // 11: Keybindings
        ])
        .split(area);

        // Title with filename
        let title = Line::from(vec![
            Span::styled(" Edit Command for: ", Style::default().fg(self.theme.header_fg)),
            Span::styled(&state.filename, Style::default().fg(self.theme.text_highlight).add_modifier(Modifier::BOLD)),
            if !state.suggestions.is_empty() {
                Span::styled(format!(" [{} suggestions]", state.suggestions.len()), Style::default().fg(self.theme.text_secondary))
            } else {
                Span::raw("")
            },
        ]);
        Paragraph::new(title).render(chunks[0], buffer);

        // Separator
        let border_style = Style::default().fg(self.theme.panel_border);
        for x in chunks[1].x..chunks[1].x + chunks[1].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[1].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Calculate prefix display string and its length for alignment
        let prefix_display = if state.prefix_tokens.is_empty() && state.selected_section == FileEditSection::Prefix {
            if state.edit_buffer.is_empty() {
                "[command]".to_string()
            } else {
                format!("⟦{}⟧", state.edit_buffer)
            }
        } else {
            let tokens: Vec<&str> = state.prefix_tokens.iter().map(|s| s.as_str()).collect();
            if tokens.is_empty() {
                "[command]".to_string()
            } else {
                tokens.join(" ")
            }
        };

        // Calculate suggestion alignment offset and selected position for scrolling
        let (suggestion_offset, selected_x_start, selected_x_end) = match state.selected_section {
            FileEditSection::Prefix => {
                let start = 3; // Initial "   " padding
                let end = start + prefix_display.chars().count();
                (start, start, end)
            }
            FileEditSection::Filename => {
                let start = 3 + prefix_display.chars().count() + 1; // After prefix + space
                let end = start + state.filename.chars().count();
                (start, start, end)
            }
            FileEditSection::Suffix => {
                // Start after filename
                let mut offset = 3 + prefix_display.chars().count() + 1 + state.filename.chars().count() + 1;
                let start = offset;
                // Add width of suffix tokens before the selected position
                for (i, token) in state.suffix_tokens.iter().enumerate() {
                    if i < state.selected_index {
                        if i == state.selected_index {
                            offset += 1; // "⟦" before edit buffer
                        }
                        offset += token.chars().count() + 1; // token + space (or ⟦token⟧ + space)
                    }
                }
                // Calculate end position for the selected token/edit slot
                let edit_text = if state.edit_buffer.is_empty() { "_" } else { &state.edit_buffer };
                let end = offset + 1 + edit_text.chars().count() + 1; // ⟦ + text + ⟧
                // Add opening bracket for the edit slot
                offset += 1; // "⟦"
                (offset, start, end)
            }
        };

        // Calculate horizontal scroll offset to keep selected content visible
        let viewport_width = chunks[3].width as usize;
        let left_context = viewport_width / 3; // Show ~1/3 of viewport with previous tokens
        let right_margin = 8; // Small margin on right edge
        let scroll_offset = if selected_x_end > viewport_width.saturating_sub(right_margin) {
            // Selected content is past right edge - scroll right, keeping previous tokens visible
            selected_x_start.saturating_sub(left_context)
        } else {
            0
        };

        // Adjust suggestion padding for scroll offset
        let adjusted_suggestion_offset = suggestion_offset.saturating_sub(scroll_offset);
        let suggestion_padding = " ".repeat(adjusted_suggestion_offset);

        // Previous suggestion (dim)
        if let Some(prev_sugg) = state.prev_suggestion() {
            let prev_line = Line::from(vec![
                Span::styled(&suggestion_padding, Style::default()),
                Span::styled(prev_sugg, Style::default().fg(self.theme.text_secondary)),
            ]);
            Paragraph::new(prev_line).render(chunks[2], buffer);
        }

        // Command preview row with three parts
        let mut spans = Vec::new();
        spans.push(Span::styled("   ", Style::default()));

        // Prefix section
        let prefix_style = if state.selected_section == FileEditSection::Prefix {
            Style::default().fg(self.theme.semantic_success).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.theme.semantic_success)
        };
        spans.push(Span::styled(format!("{} ", prefix_display), prefix_style));

        // Filename (non-editable)
        let filename_style = Style::default().fg(self.theme.text_highlight);
        spans.push(Span::styled(format!("{} ", state.filename), filename_style));

        // Suffix section - show individual tokens with edit slot
        let suffix_style = Style::default().fg(self.theme.git_fg);
        let suffix_selected_style = Style::default().fg(self.theme.git_fg).add_modifier(Modifier::BOLD);

        if state.selected_section == FileEditSection::Suffix {
            // Show suffix tokens individually with edit slot at selected_index
            if state.suffix_tokens.is_empty() && state.edit_buffer.is_empty() {
                spans.push(Span::styled("[args]", suffix_style));
            } else {
                for (i, token) in state.suffix_tokens.iter().enumerate() {
                    if i == state.selected_index {
                        // This token is being edited
                        spans.push(Span::styled(format!("⟦{}⟧ ", state.edit_buffer), suffix_selected_style));
                    } else {
                        spans.push(Span::styled(format!("{} ", token), suffix_style));
                    }
                }
                // If selected_index is at or beyond tokens, show edit slot for new token
                if state.selected_index >= state.suffix_tokens.len() {
                    if state.edit_buffer.is_empty() {
                        spans.push(Span::styled("⟦_⟧", suffix_selected_style));
                    } else {
                        spans.push(Span::styled(format!("⟦{}⟧", state.edit_buffer), suffix_selected_style));
                    }
                }
            }
        } else {
            // Not in suffix section - just show tokens or placeholder
            if state.suffix_tokens.is_empty() {
                spans.push(Span::styled("[args]", suffix_style));
            } else {
                let tokens: Vec<&str> = state.suffix_tokens.iter().map(|s| s.as_str()).collect();
                spans.push(Span::styled(tokens.join(" "), suffix_style));
            }
        }

        let command_line = Line::from(spans);
        Paragraph::new(command_line)
            .scroll((0, scroll_offset as u16))
            .render(chunks[3], buffer);

        // Next suggestion (dim)
        if let Some(next_sugg) = state.next_suggestion() {
            let next_line = Line::from(vec![
                Span::styled(&suggestion_padding, Style::default()),
                Span::styled(next_sugg, Style::default().fg(self.theme.text_secondary)),
            ]);
            Paragraph::new(next_line).render(chunks[4], buffer);
        }

        // Edit input line
        let section_label = match state.selected_section {
            FileEditSection::Prefix => "prefix",
            FileEditSection::Filename => "file",
            FileEditSection::Suffix => "suffix",
        };
        let cycling_indicator = if state.suggestion_index.is_some() {
            format!(" [{}/{}]",
                state.suggestion_index.unwrap_or(0) + 1,
                state.suggestions.len())
        } else {
            String::new()
        };
        let edit_line = Line::from(vec![
            Span::styled(format!("   {} > ", section_label), Style::default().fg(self.theme.header_fg)),
            Span::styled(&state.edit_buffer, Style::default().fg(self.theme.text_primary).add_modifier(Modifier::BOLD)),
            Span::styled("█", Style::default().fg(self.theme.header_fg)),
            Span::styled(cycling_indicator, Style::default().fg(self.theme.text_secondary)),
        ]);
        Paragraph::new(edit_line).render(chunks[6], buffer);

        // Result preview - build the full command
        let mut parts = Vec::new();
        if !state.prefix_tokens.is_empty() {
            parts.extend(state.prefix_tokens.iter().cloned());
        } else if !state.edit_buffer.is_empty() && state.selected_section == FileEditSection::Prefix {
            parts.push(state.edit_buffer.clone());
        }
        // Use shell_quote for consistency with build_command
        parts.push(shell_quote(&state.filepath.to_string_lossy()));
        if !state.suffix_tokens.is_empty() {
            parts.extend(state.suffix_tokens.iter().cloned());
        } else if !state.edit_buffer.is_empty() && state.selected_section == FileEditSection::Suffix {
            parts.push(state.edit_buffer.clone());
        }
        let result_preview = parts.join(" ");

        let preview_line = Line::from(vec![
            Span::styled("  Result: ", Style::default().fg(self.theme.text_secondary)),
            Span::styled(&result_preview, Style::default().fg(self.theme.text_primary)),
        ]);
        Paragraph::new(preview_line).render(chunks[8], buffer);

        // Border
        for x in chunks[10].x..chunks[10].x + chunks[10].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[10].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Keybindings
        let key_style = Style::default().fg(self.theme.text_highlight);
        let label_style = Style::default().fg(self.theme.text_secondary);
        let hints = Line::from(vec![
            Span::styled("↑↓", key_style),
            Span::styled(" Cycle", label_style),
            Span::raw("  "),
            Span::styled("Tab", key_style),
            Span::styled(" Section", label_style),
            Span::raw("  "),
            Span::styled("^A", key_style),
            Span::styled(" Add", label_style),
            Span::raw("  "),
            Span::styled("^D", key_style),
            Span::styled(" Del", label_style),
            Span::raw("  "),
            Span::styled("Enter", key_style),
            Span::styled(" Run", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Back", label_style),
        ]);
        Paragraph::new(hints).render(chunks[11], buffer);
    }

    /// Handles input in file edit mode.
    fn handle_file_edit_input(&mut self, key: KeyEvent) -> Option<PanelResult> {
        let state = self.edit_mode.as_mut()?;

        // Handle Ctrl+key commands
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('a') => {
                    state.insert_token_after();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('d') => {
                    state.delete_token();
                    return Some(PanelResult::Continue);
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                self.exit_edit_mode();
                Some(PanelResult::Continue)
            }
            KeyCode::Enter => {
                let command = state.build_command();
                self.exit_edit_mode();
                Some(PanelResult::Execute(command))
            }
            KeyCode::Tab => {
                state.next_section();
                state.update_suggestions();
                Some(PanelResult::Continue)
            }
            KeyCode::BackTab => {
                state.prev_section();
                state.update_suggestions();
                Some(PanelResult::Continue)
            }
            KeyCode::Up => {
                state.cycle_suggestion(-1);
                Some(PanelResult::Continue)
            }
            KeyCode::Down => {
                state.cycle_suggestion(1);
                Some(PanelResult::Continue)
            }
            KeyCode::Char('|') if state.selected_section == FileEditSection::Suffix => {
                // Pipe in suffix: commit current token, add pipe, start new token with pipeable suggestions
                if !state.edit_buffer.is_empty() {
                    state.commit_edit();
                }
                // Add the pipe as its own token
                state.suffix_tokens.push("|".to_string());
                state.selected_index = state.suffix_tokens.len();
                state.edit_buffer.clear();
                state.suggestion_index = None;
                state.update_suggestions();
                Some(PanelResult::Continue)
            }
            KeyCode::Char(c) => {
                state.edit_buffer.push(c);
                state.suggestion_index = None;
                Some(PanelResult::Continue)
            }
            KeyCode::Backspace => {
                state.edit_buffer.pop();
                state.suggestion_index = None;
                Some(PanelResult::Continue)
            }
            _ => Some(PanelResult::Continue),
        }
    }
}

impl Panel for FileBrowserPanel {
    fn preferred_height(&self) -> u16 {
        if self.edit_mode.is_some() {
            13 // 12 rows including spacer before Result
        } else {
            12
        }
    }

    fn title(&self) -> &str {
        "Files"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // If in edit mode, render the edit UI
        if let Some(ref state) = self.edit_mode {
            self.render_file_edit_mode(buffer, area, &state.clone());
            return;
        }

        // Create layout: path header at top, list in middle, border + keybinds at bottom
        let chunks = Layout::vertical([
            Constraint::Length(1), // Path header
            Constraint::Min(1),    // File list
            Constraint::Length(1), // Border line
            Constraint::Length(1), // Keybind hints bar
        ])
        .split(area);

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
            Span::styled(" ", Style::default().fg(self.theme.header_fg)),
            Span::styled(
                truncated_path,
                Style::default()
                    .fg(self.theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            if self.show_hidden {
                Span::styled(" [H]", Style::default().fg(self.theme.text_secondary))
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
                    self.theme.dir_color
                } else {
                    self.theme.file_color
                };

                let name_style = if is_selected {
                    Style::default()
                        .fg(self.theme.selection_fg)
                        .add_modifier(Modifier::BOLD)
                } else if entry.is_dir {
                    Style::default().fg(self.theme.dir_color)
                } else {
                    Style::default().fg(self.theme.file_color)
                };

                // Format metadata columns
                let perms_str = format_permissions(entry.mode);
                let date_str = format_date_compact(entry.modified);
                let size_str = if entry.is_dir {
                    "     ".to_string()
                } else {
                    format!("{:>5}", format_size(entry.size))
                };

                // Calculate available width for name (total - metadata columns)
                // Format: icon(2) + name + perms(4) + date(6) + size(6) + spacing(6)
                let metadata_width = 22_usize;
                let available_for_name = (area.width as usize).saturating_sub(metadata_width);
                // Use char-aware truncation to avoid panic on UTF-8 multibyte boundaries
                let name_chars: usize = entry.name.chars().count();
                let display_name = if name_chars > available_for_name && available_for_name > 0 {
                    let truncated: String = entry.name
                        .chars()
                        .take(available_for_name.saturating_sub(1))
                        .collect();
                    format!("{}…", truncated)
                } else {
                    entry.name.clone()
                };
                let display_name_chars = display_name.chars().count();
                let name_padding = available_for_name.saturating_sub(display_name_chars);

                let line = Line::from(vec![
                    Span::styled(format!("{} ", icon), Style::default().fg(icon_color)),
                    Span::styled(display_name, name_style),
                    Span::styled(" ".repeat(name_padding), Style::default()),
                    Span::styled(format!(" {} ", perms_str), Style::default().fg(self.theme.permissions_color)),
                    Span::styled(format!("{:>5} ", date_str), Style::default().fg(self.theme.file_date_color)),
                    Span::styled(size_str, Style::default().fg(self.theme.file_size_color)),
                ]);

                if is_selected {
                    ListItem::new(line).style(Style::default().bg(self.theme.selection_bg))
                } else {
                    ListItem::new(line)
                }
            })
            .collect();

        let list = List::new(items);
        list.render(chunks[1], buffer);

        // Render border line above keybind bar
        let border_style = Style::default().fg(self.theme.panel_border);
        for x in chunks[2].x..chunks[2].x + chunks[2].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[2].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Render keybind bar
        let key_style = Style::default().fg(self.theme.text_highlight);
        let label_style = Style::default().fg(self.theme.text_secondary);
        let active_label = Style::default().fg(self.theme.semantic_success).add_modifier(Modifier::BOLD);
        let hints = Line::from(vec![
            Span::styled("^E", key_style),
            Span::styled(" Edit", label_style),
            Span::raw("  "),
            Span::styled(".", key_style),
            Span::styled(" Hidden", if self.show_hidden { active_label } else { label_style }),
            Span::raw("  "),
            Span::styled("⌫", key_style),
            Span::styled(" Parent", label_style),
            Span::raw("  "),
            Span::styled("Enter", key_style),
            Span::styled(" Open/Insert", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Close", label_style),
        ]);
        Paragraph::new(hints).render(chunks[3], buffer);
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // If in edit mode, delegate to edit handler
        if self.in_edit_mode() {
            if let Some(result) = self.handle_file_edit_input(key) {
                return result;
            }
        }

        // Handle Ctrl+key commands
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('e') => {
                    // Enter edit mode for the selected file
                    self.enter_edit_mode();
                    return PanelResult::Continue;
                }
                KeyCode::Char('h') => {
                    self.toggle_hidden();
                    return PanelResult::Continue;
                }
                _ => {}
            }
        }

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
    use super::super::theme::AMBER_THEME;

    #[test]
    fn test_file_browser_new() {
        let panel = FileBrowserPanel::new(&AMBER_THEME);
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
        let mut panel = FileBrowserPanel::new(&AMBER_THEME);
        assert!(!panel.show_hidden);
        panel.toggle_hidden();
        assert!(panel.show_hidden);
        panel.toggle_hidden();
        assert!(!panel.show_hidden);
    }

    #[test]
    fn test_shell_quote_no_special_chars() {
        assert_eq!(shell_quote("filename.txt"), "filename.txt");
        assert_eq!(shell_quote("path/to/file"), "path/to/file");
    }

    #[test]
    fn test_shell_quote_with_spaces() {
        assert_eq!(shell_quote("file name.txt"), "'file name.txt'");
        assert_eq!(shell_quote("path with spaces/file"), "'path with spaces/file'");
    }

    #[test]
    fn test_shell_quote_with_single_quote() {
        assert_eq!(shell_quote("it's here"), "'it'\\''s here'");
    }

    #[test]
    fn test_shell_quote_with_special_chars() {
        assert_eq!(shell_quote("file$var"), "'file$var'");
        assert_eq!(shell_quote("file*"), "'file*'");
        assert_eq!(shell_quote("file?"), "'file?'");
    }

    #[test]
    fn test_shell_quote_empty_string() {
        assert_eq!(shell_quote(""), "''");
    }
}
