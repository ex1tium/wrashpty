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
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use tracing::debug;

use super::command_edit::{CommandEditState, CommandToken, TokenType, superscript_number, token_type_style};
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
    /// Uses the same CommandEditState as history browser for consistent UX.
    edit_mode: Option<CommandEditState>,
    /// Filename being edited (stored separately for suggestions).
    edit_filename: Option<String>,
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
            edit_filename: None,
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

                // Create CommandEditState with locked filepath token
                // Token 0: Command (editable)
                // Token 1: Filepath (locked, non-editable)
                let filepath_str = shell_quote(&entry.path.to_string_lossy());
                let mut edit_state = CommandEditState::for_file(&entry.name, &filepath_str);

                // Set intelligence context
                if let Some(store) = &self.history_store {
                    edit_state.set_history_store(store.clone());
                }
                edit_state.set_cwd(self.current_dir.clone());
                edit_state.set_file_context(FileContext::new(&entry.name, entry.is_dir));

                // Add file-type specific suggestions for the command position
                let file_suggestions: Vec<String> = COMMAND_KNOWLEDGE
                    .commands_for_filetype(&entry.name)
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                edit_state.add_suggestions(file_suggestions);

                self.edit_filename = Some(entry.name.clone());
                self.edit_mode = Some(edit_state);
            }
        }
    }

    /// Exits edit mode.
    fn exit_edit_mode(&mut self) {
        self.edit_mode = None;
        self.edit_filename = None;
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

    /// Updates suggestions based on file context.
    ///
    /// For command position (first token): suggests file-type appropriate commands.
    /// After pipe: suggests pipeable commands.
    /// Other positions: uses intelligent suggestions from history.
    fn update_suggestions_with_file_context(&mut self) {
        let Some(edit_state) = &mut self.edit_mode else { return };
        let filename = self.edit_filename.clone().unwrap_or_default();

        // Check if we're editing after a pipe
        let editing_after_pipe = if edit_state.selected > 0 {
            edit_state.tokens.get(edit_state.selected.saturating_sub(1))
                .map(|t| t.text == "|")
                .unwrap_or(false)
        } else {
            false
        };

        if editing_after_pipe {
            // After pipe: suggest pipeable commands
            edit_state.suggestions = COMMAND_KNOWLEDGE
                .pipeable_commands()
                .iter()
                .map(|s| s.to_string())
                .collect();
            edit_state.suggestion_index = None;
            return;
        }

        // Check if we're at the command position (first non-locked token)
        let is_command_position = edit_state.selected == 0 ||
            (edit_state.selected > 0 && edit_state.tokens.iter().take(edit_state.selected).all(|t| t.locked));

        if is_command_position {
            // Command position: suggest file-type appropriate commands
            edit_state.suggestions = COMMAND_KNOWLEDGE
                .commands_for_filetype(&filename)
                .iter()
                .map(|s| s.to_string())
                .collect();
            edit_state.suggestion_index = None;
            return;
        }

        // Other positions: use intelligent suggestions
        edit_state.update_suggestions();
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
    /// Renders the file edit mode UI using unified token strip (same as history browser).
    fn render_file_edit_mode(&self, buffer: &mut Buffer, area: Rect, edit_state: &CommandEditState) {
        // Layout matching history browser
        let chunks = Layout::vertical([
            Constraint::Length(1), // 0: Title with filename
            Constraint::Length(1), // 1: Separator
            Constraint::Length(1), // 2: Previous suggestion row (dim)
            Constraint::Length(1), // 3: Current token strip (highlighted)
            Constraint::Length(1), // 4: Next suggestion row (dim)
            Constraint::Length(1), // 5: Spacer
            Constraint::Length(1), // 6: Edit input line
            Constraint::Length(1), // 7: Spacer before result
            Constraint::Min(2),    // 8: Result preview (wraps to multiple lines)
            Constraint::Length(1), // 9: Border
            Constraint::Length(1), // 10: Keybind hints
        ])
        .split(area);

        // Title with filename and suggestion count
        let filename = self.edit_filename.as_deref().unwrap_or("file");
        let mut title_spans = vec![
            Span::styled(" Edit Command for: ", Style::default().fg(self.theme.header_fg)),
            Span::styled(filename, Style::default().fg(self.theme.text_highlight).add_modifier(Modifier::BOLD)),
        ];
        if !edit_state.suggestions.is_empty() {
            let sugg_count = format!(" [{} suggestions]", edit_state.suggestions.len());
            title_spans.push(Span::styled(sugg_count, Style::default().fg(self.theme.text_secondary)));
        }
        let title = Line::from(title_spans);
        Paragraph::new(title).render(chunks[0], buffer);

        // Separator
        let border_style = Style::default().fg(self.theme.panel_border);
        for x in chunks[1].x..chunks[1].x + chunks[1].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[1].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Calculate the x-position where the selected token starts and ends
        // Token format: "   " + for each token: superscript(n digits) + "⟦" + text + "⟧" + "   "
        let mut selected_x_start: usize = 3; // Initial padding
        let mut selected_x_end: usize = 3;
        for (i, token) in edit_state.tokens.iter().enumerate() {
            let display_text = if i == edit_state.selected {
                if edit_state.edit_buffer.is_empty() { "_" } else { &edit_state.edit_buffer }
            } else if token.text.is_empty() {
                "_"
            } else {
                &token.text
            };
            // superscript (n digits) + ⟦ (1) + text + ⟧ (1) + spacing (3)
            let slot_num = i + 1;
            let superscript_len = slot_num.to_string().len();
            let token_width = superscript_len + 1 + display_text.chars().count() + 1 + 3;

            if i == edit_state.selected {
                selected_x_end = selected_x_start + superscript_len + 1 + display_text.chars().count() + 1;
                break;
            }
            selected_x_start += token_width;
        }
        // Add superscript + opening bracket to get to content start
        let superscript_len = (edit_state.selected + 1).to_string().len();
        let selected_x_offset = selected_x_start + superscript_len + 1;

        // Calculate horizontal scroll offset to keep selected token visible
        let viewport_width = chunks[3].width as usize;
        let left_context = viewport_width / 3; // Show ~1/3 of viewport with previous tokens
        let right_margin = 8; // Small margin on right edge
        let scroll_offset = if selected_x_end > viewport_width.saturating_sub(right_margin) {
            // Selected token is past right edge - scroll right, keeping previous tokens visible
            selected_x_start.saturating_sub(left_context)
        } else {
            0
        };

        // Previous suggestion row (dim, aligned under selected token, accounting for scroll)
        if let Some(prev_sugg) = edit_state.prev_suggestion() {
            let adjusted_offset = selected_x_offset.saturating_sub(scroll_offset);
            let padding = " ".repeat(adjusted_offset);
            let prev_line = Line::from(vec![
                Span::styled(padding, Style::default()),
                Span::styled(prev_sugg, Style::default().fg(self.theme.text_secondary)),
            ]);
            Paragraph::new(prev_line).render(chunks[2], buffer);
        }

        // Current token strip with double brackets and superscript numbers
        let mut spans = Vec::new();
        spans.push(Span::styled("   ", Style::default()));

        let bracket_style = Style::default().fg(self.theme.text_secondary);
        let bracket_selected_style = Style::default().fg(self.theme.header_fg);

        for (i, token) in edit_state.tokens.iter().enumerate() {
            let is_selected = i == edit_state.selected;
            let slot_num = i + 1;

            // Superscript number
            let num_style = if is_selected {
                Style::default().fg(self.theme.text_highlight).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.text_secondary)
            };
            spans.push(Span::styled(superscript_number(slot_num), num_style));

            // Opening bracket
            let bstyle = if is_selected { bracket_selected_style } else { bracket_style };
            spans.push(Span::styled("⟦", bstyle));

            // Token text with type-aware styling
            let base_style = token_type_style(token.token_type, self.theme);
            let token_style = if is_selected {
                base_style.add_modifier(Modifier::BOLD)
            } else {
                base_style
            };

            // Show edit buffer for selected token, original text for others
            let display_text = if is_selected {
                if edit_state.edit_buffer.is_empty() {
                    "_".to_string()
                } else {
                    edit_state.edit_buffer.clone()
                }
            } else if token.text.is_empty() {
                "_".to_string()
            } else {
                token.text.clone()
            };
            spans.push(Span::styled(display_text, token_style));

            // Closing bracket
            spans.push(Span::styled("⟧", bstyle));

            // Spacing between tokens
            spans.push(Span::raw("   "));
        }

        let token_line = Line::from(spans);
        Paragraph::new(token_line)
            .scroll((0, scroll_offset as u16))
            .render(chunks[3], buffer);

        // Next suggestion row (dim, aligned under selected token, accounting for scroll)
        if let Some(next_sugg) = edit_state.next_suggestion() {
            let adjusted_offset = selected_x_offset.saturating_sub(scroll_offset);
            let padding = " ".repeat(adjusted_offset);
            let next_line = Line::from(vec![
                Span::styled(padding, Style::default()),
                Span::styled(next_sugg, Style::default().fg(self.theme.text_secondary)),
            ]);
            Paragraph::new(next_line).render(chunks[4], buffer);
        }

        // Edit input line with type hint and cycling indicator
        let type_hint = edit_state.type_hint();
        let cycling_indicator = if edit_state.suggestion_index.is_some() {
            format!(" [{}/{}]",
                edit_state.suggestion_index.unwrap_or(0) + 1,
                edit_state.suggestions.len())
        } else {
            String::new()
        };
        let edit_label = format!("   {} {} > ", superscript_number(edit_state.selected + 1), type_hint);
        let edit_line = Line::from(vec![
            Span::styled(edit_label, Style::default().fg(self.theme.git_fg)),
            Span::styled(&edit_state.edit_buffer, Style::default().fg(self.theme.text_primary).add_modifier(Modifier::BOLD)),
            Span::styled("█", Style::default().fg(self.theme.header_fg)),
            Span::styled(cycling_indicator, Style::default().fg(self.theme.text_secondary)),
        ]);
        Paragraph::new(edit_line).render(chunks[6], buffer);

        // Build and show result preview
        let result_preview: String = edit_state.tokens.iter().enumerate().map(|(i, t)| {
            if i == edit_state.selected {
                edit_state.edit_buffer.clone()
            } else {
                t.text.clone()
            }
        }).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");

        let preview_changed = result_preview != edit_state.original;
        let preview_style = if preview_changed {
            Style::default().fg(self.theme.semantic_success)
        } else {
            Style::default().fg(self.theme.text_primary)
        };
        let preview_line = Line::from(vec![
            Span::styled("  Result: ", Style::default().fg(self.theme.text_secondary)),
            Span::styled(&result_preview, preview_style),
        ]);
        Paragraph::new(preview_line)
            .wrap(Wrap { trim: false })
            .render(chunks[8], buffer);

        // Border
        for x in chunks[9].x..chunks[9].x + chunks[9].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[9].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Keybindings (matching history browser)
        let key_style = Style::default().fg(self.theme.text_highlight);
        let label_style = Style::default().fg(self.theme.text_secondary);
        let hints = Line::from(vec![
            Span::styled("↑↓", key_style),
            Span::styled(" Cycle", label_style),
            Span::raw("  "),
            Span::styled("←→", key_style),
            Span::styled(" Nav", label_style),
            Span::raw("  "),
            Span::styled("^A", key_style),
            Span::styled(" Add", label_style),
            Span::raw("  "),
            Span::styled("^D", key_style),
            Span::styled(" Del", label_style),
            Span::raw("  "),
            Span::styled("^Z", key_style),
            Span::styled(" Undo", label_style),
            Span::raw("  "),
            Span::styled("Enter", key_style),
            Span::styled(" Run", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Back", label_style),
        ]);
        Paragraph::new(hints).render(chunks[10], buffer);
    }

    /// Handles input in file edit mode (mirrors history browser).
    fn handle_file_edit_input(&mut self, key: KeyEvent) -> Option<PanelResult> {
        let edit_state = self.edit_mode.as_mut()?;

        // Handle Ctrl+key commands
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('u') => {
                    edit_state.undo();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('d') => {
                    edit_state.delete_token();
                    self.update_suggestions_with_file_context();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('a') => {
                    edit_state.insert_token_after();
                    self.update_suggestions_with_file_context();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('i') => {
                    edit_state.insert_token_before();
                    self.update_suggestions_with_file_context();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('q') => {
                    edit_state.cycle_quote();
                    return Some(PanelResult::Continue);
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                // Three-stage Esc:
                // 1. If current token edit differs from saved token → revert token
                // 2. If command differs from original → revert entire command
                // 3. Exit edit mode
                let token_text = edit_state.tokens.get(edit_state.selected)
                    .map(|t| t.text.as_str())
                    .unwrap_or("");

                if edit_state.edit_buffer != token_text {
                    edit_state.edit_buffer = token_text.to_string();
                    Some(PanelResult::Continue)
                } else if edit_state.is_changed() {
                    edit_state.revert();
                    Some(PanelResult::Continue)
                } else {
                    self.exit_edit_mode();
                    Some(PanelResult::Continue)
                }
            }
            KeyCode::Enter => {
                let command = edit_state.build_command();
                self.exit_edit_mode();
                Some(PanelResult::Execute(command))
            }
            KeyCode::Left => {
                edit_state.prev();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Right => {
                edit_state.next();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Home => {
                edit_state.select(0);
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::End => {
                let last = edit_state.token_count().saturating_sub(1);
                edit_state.select(last);
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Up => {
                edit_state.cycle_suggestion(-1);
                Some(PanelResult::Continue)
            }
            KeyCode::Down => {
                edit_state.cycle_suggestion(1);
                Some(PanelResult::Continue)
            }
            KeyCode::Tab => {
                edit_state.next();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::BackTab => {
                edit_state.prev();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Char('|') => {
                // Pipe: commit current token, add pipe, start new token with pipeable suggestions
                if !edit_state.edit_buffer.is_empty() {
                    // Save current edit to current token
                    if let Some(token) = edit_state.tokens.get_mut(edit_state.selected) {
                        if !token.locked {
                            token.text = edit_state.edit_buffer.clone();
                        }
                    }
                }
                // Add the pipe as its own token
                let pipe_pos = edit_state.selected + 1;
                edit_state.tokens.insert(pipe_pos, CommandToken::new("|", TokenType::Argument));
                // Point to new empty token after pipe
                let empty_pos = pipe_pos + 1;
                edit_state.tokens.insert(empty_pos, CommandToken::new("", TokenType::Argument));
                edit_state.selected = empty_pos;
                edit_state.edit_buffer.clear();
                edit_state.suggestion_index = None;
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Char(c) => {
                edit_state.type_char(c);
                Some(PanelResult::Continue)
            }
            KeyCode::Backspace => {
                edit_state.backspace();
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
