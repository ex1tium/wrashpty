//! History browser panel for browsing command history with table view.
//!
//! Includes an edit mode for modifying commands before execution.

use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::{Paragraph, Wrap};
use tracing::{debug, warn};

use super::command_edit::{
    CommandEditState, CommandToken, TokenType, superscript_number, token_type_style,
};
use super::panel::{Panel, PanelResult};
use super::theme::Theme;
use crate::history_store::{FilterMode, HistoryRecord, HistoryStore, SortMode};

/// Column widths for the table view
struct ColumnWidths {
    command: u16,  // Command (flexible, first column)
    time: u16,     // Relative time
    duration: u16, // Command duration
    count: u16,    // Execution count (optional)
    status: u16,   // Exit status indicator
}

impl ColumnWidths {
    fn calculate(total_width: u16, show_count: bool) -> Self {
        let time = 6;
        let duration = 8;
        let count = if show_count { 5 } else { 0 };
        let status = 2;
        let separators = if show_count { 4 } else { 3 };
        let fixed = time + duration + count + status + separators;
        let command = total_width.saturating_sub(fixed);

        Self {
            command,
            time,
            duration,
            count,
            status,
        }
    }
}

/// History browser panel with table view.
pub struct HistoryBrowserPanel {
    /// History records from the store.
    records: Vec<HistoryRecord>,
    /// Currently selected index.
    selection: usize,
    /// Scroll offset for display.
    scroll_offset: usize,
    /// Current filter text.
    filter: String,
    /// Filter mode settings.
    filter_mode: FilterMode,
    /// Sort mode.
    sort_mode: SortMode,
    /// Current working directory for "here" filter.
    current_cwd: Option<PathBuf>,
    /// Reference to the history store.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Edit mode state (None when not in edit mode).
    edit_mode: Option<CommandEditState>,
    /// Theme for rendering.
    theme: &'static Theme,
}

impl HistoryBrowserPanel {
    /// Creates a new empty history browser.
    pub fn new(theme: &'static Theme) -> Self {
        Self {
            records: Vec::new(),
            selection: 0,
            scroll_offset: 0,
            filter: String::new(),
            filter_mode: FilterMode::default(),
            sort_mode: SortMode::default(),
            current_cwd: None,
            history_store: None,
            edit_mode: None,
            theme,
        }
    }

    /// Sets the history store for queries.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.history_store = Some(store);
    }

    /// Sets the current working directory.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.current_cwd = Some(cwd);
    }

    /// Loads history from the store with current filters.
    pub fn load_history(&mut self) {
        self.records.clear();

        if let Some(store) = &self.history_store {
            if let Ok(store) = store.lock() {
                match store.query(
                    &self.filter,
                    &self.filter_mode,
                    &self.sort_mode,
                    self.current_cwd.as_ref(),
                    1000,
                ) {
                    Ok(records) => self.records = records,
                    Err(e) => warn!("Failed to query history: {}", e),
                }
            }
        }

        self.selection = 0;
        self.scroll_offset = 0;

        debug!(
            count = self.records.len(),
            filter = %self.filter,
            dedupe = self.filter_mode.dedupe,
            here = self.filter_mode.current_dir_only,
            failed = self.filter_mode.failed_only,
            sort = ?self.sort_mode,
            "Loaded history"
        );
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
        self.records.get(self.selection).map(|r| r.command.as_str())
    }

    /// Toggles the dedupe filter.
    fn toggle_dedupe(&mut self) {
        self.filter_mode.dedupe = !self.filter_mode.dedupe;
        self.load_history();
    }

    /// Toggles the current directory filter.
    fn toggle_current_dir(&mut self) {
        self.filter_mode.current_dir_only = !self.filter_mode.current_dir_only;
        self.load_history();
    }

    /// Toggles the failed commands filter.
    fn toggle_failed(&mut self) {
        self.filter_mode.failed_only = !self.filter_mode.failed_only;
        self.load_history();
    }

    /// Cycles through sort modes.
    fn cycle_sort(&mut self) {
        self.sort_mode = self.sort_mode.next();
        self.load_history();
    }

    /// Enters edit mode for the selected command.
    fn enter_edit_mode(&mut self) {
        if let Some(cmd) = self.selected_command() {
            debug!(command = %cmd, "Entering edit mode");
            let mut edit_state = CommandEditState::from_command(cmd);

            // Configure intelligence context if available
            if let Some(ref store) = self.history_store {
                edit_state.set_intelligence_context(
                    Arc::clone(store),
                    self.current_cwd.clone(),
                    None, // No file context in history browser
                    None, // Last command could be tracked separately
                );
            }

            self.edit_mode = Some(edit_state);
            self.update_suggestions_with_history();
        }
    }

    /// Exits edit mode without saving.
    fn exit_edit_mode(&mut self) {
        self.edit_mode = None;
    }

    /// Returns true if currently in edit mode.
    fn in_edit_mode(&self) -> bool {
        self.edit_mode.is_some()
    }

    /// Updates suggestions from the unified intelligence pipeline.
    fn update_suggestions_with_history(&mut self) {
        let Some(edit_state) = &mut self.edit_mode else {
            return;
        };

        edit_state.update_suggestions();
    }

    /// Renders the edit mode UI with three-row depth display.
    fn render_edit_mode(&self, buffer: &mut Buffer, area: Rect) {
        let Some(edit_state) = &self.edit_mode else {
            return;
        };

        // Check if showing danger confirmation
        if edit_state.is_confirming() {
            self.render_danger_confirm(buffer, area, edit_state);
            return;
        }

        // Layout with three-row depth UI - 12 rows to match file browser
        let chunks = Layout::vertical([
            Constraint::Length(1), // 0: Title with original command
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

        // Title with suggestion count and unsafe indicator
        let mut title_spans = vec![Span::styled(
            " Edit Command",
            Style::default()
                .fg(self.theme.header_fg)
                .add_modifier(Modifier::BOLD),
        )];
        if !edit_state.suggestions.is_empty() {
            let sugg_count = format!(" [{} suggestions]", edit_state.suggestions.len());
            title_spans.push(Span::styled(
                sugg_count,
                Style::default().fg(self.theme.text_secondary),
            ));
        }
        if edit_state.skip_danger_check {
            title_spans.push(Span::styled(
                " [UNSAFE]",
                Style::default()
                    .fg(self.theme.semantic_error)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let title = Line::from(title_spans);
        Paragraph::new(title).render(chunks[0], buffer);

        // Separator with original command hint
        let border_style = Style::default().fg(self.theme.panel_border);
        let orig_hint = format!(" Original: {} ", edit_state.original);
        let max_hint_width = (area.width as usize).saturating_sub(4);
        let truncated_hint: String =
            crate::ui::text_width::truncate_to_width(&orig_hint, max_hint_width).into_owned();
        // Collect chars for cell-by-cell rendering
        let hint_chars: Vec<char> = truncated_hint.chars().collect();

        // Track display column as we write hint chars into cells
        let mut hint_idx = 0;
        let mut col_offset: usize = 0;
        for x in chunks[1].x..chunks[1].x + chunks[1].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[1].y)) {
                if col_offset > 0 {
                    // Skip continuation cell(s) from previous wide char.
                    col_offset -= 1;
                    continue;
                }

                if hint_idx < hint_chars.len() {
                    let ch = hint_chars[hint_idx];
                    let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(self.theme.text_secondary));
                    hint_idx += 1;
                    // For wide characters, skip continuation cells.
                    col_offset = ch_w.saturating_sub(1);
                } else {
                    cell.set_char('─');
                    cell.set_style(border_style);
                }
            }
        }

        // Calculate the x-position where the selected token starts and ends
        // Token format: "   " + for each token: superscript(1-2 chars) + "⟦" + text + "⟧" + "   "
        let mut selected_x_start: usize = 3; // Initial padding
        let mut selected_x_end: usize = 3;
        for (i, token) in edit_state.tokens.iter().enumerate() {
            let display_text = if i == edit_state.selected {
                if edit_state.edit_buffer.is_empty() {
                    "_"
                } else {
                    &edit_state.edit_buffer
                }
            } else if token.text.is_empty() {
                "_"
            } else {
                &token.text
            };
            // superscript (n digits) + ⟦ (1) + text + ⟧ (1) + spacing (3)
            let slot_num = i + 1;
            let superscript_len = slot_num.to_string().len(); // Number of digits
            let text_display_width = crate::ui::text_width::display_width(display_text);
            let token_width = superscript_len + 1 + text_display_width + 1 + 3;

            if i == edit_state.selected {
                selected_x_end = selected_x_start + superscript_len + 1 + text_display_width + 1;
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
                Style::default()
                    .fg(self.theme.text_highlight)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.text_secondary)
            };
            spans.push(Span::styled(superscript_number(slot_num), num_style));

            // Opening bracket
            let bstyle = if is_selected {
                bracket_selected_style
            } else {
                bracket_style
            };
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
            format!(
                " [{}/{}]",
                edit_state.suggestion_index.unwrap_or(0) + 1,
                edit_state.suggestions.len()
            )
        } else {
            String::new()
        };
        let edit_label = format!(
            "   {} {} > ",
            superscript_number(edit_state.selected + 1),
            type_hint
        );
        let edit_line = Line::from(vec![
            Span::styled(edit_label, Style::default().fg(self.theme.git_fg)),
            Span::styled(
                &edit_state.edit_buffer,
                Style::default()
                    .fg(self.theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("█", Style::default().fg(self.theme.header_fg)),
            Span::styled(
                cycling_indicator,
                Style::default().fg(self.theme.text_secondary),
            ),
        ]);
        Paragraph::new(edit_line).render(chunks[6], buffer);

        // Build and show result preview
        let result_preview: String = edit_state
            .tokens
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == edit_state.selected {
                    edit_state.edit_buffer.clone()
                } else {
                    t.text.clone()
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

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

        // Keybind hints
        let key_style = Style::default().fg(self.theme.text_highlight);
        let label_style = Style::default().fg(self.theme.text_secondary);

        let hints = Line::from(vec![
            Span::styled("←→", key_style),
            Span::styled(" Nav", label_style),
            Span::raw("  "),
            Span::styled("↑↓", key_style),
            Span::styled(" Cycle", label_style),
            Span::raw("  "),
            Span::styled("^D", key_style),
            Span::styled(" Del", label_style),
            Span::raw("  "),
            Span::styled("^A/I", key_style),
            Span::styled(" Ins", label_style),
            Span::raw("  "),
            Span::styled("^Q", key_style),
            Span::styled(" Quote", label_style),
            Span::raw("  "),
            Span::styled("Enter", key_style),
            Span::styled(" Run", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Back", label_style),
        ]);
        Paragraph::new(hints).render(chunks[10], buffer);
    }

    /// Renders the danger confirmation dialog.
    fn render_danger_confirm(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        edit_state: &CommandEditState,
    ) {
        let chunks = Layout::vertical([
            Constraint::Length(1), // Warning header
            Constraint::Length(1), // Warning message
            Constraint::Length(1), // Command
            Constraint::Min(1),    // Spacer
            Constraint::Length(1), // Border
            Constraint::Length(1), // Keybind hints
        ])
        .split(area);

        // Warning header
        let header = Line::from(vec![Span::styled(
            " ⚠ WARNING ",
            Style::default()
                .fg(self.theme.bar_bg)
                .bg(self.theme.semantic_warning)
                .add_modifier(Modifier::BOLD),
        )]);
        Paragraph::new(header).render(chunks[0], buffer);

        // Warning message
        let warning_msg = edit_state
            .pending_confirm
            .as_ref()
            .map(|c| c.warning.message)
            .unwrap_or("Potentially dangerous command");
        let warning_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                warning_msg,
                Style::default()
                    .fg(self.theme.semantic_error)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        Paragraph::new(warning_line).render(chunks[1], buffer);

        // Command
        let cmd = edit_state
            .pending_confirm
            .as_ref()
            .map(|c| c.command.as_str())
            .unwrap_or("");
        let cmd_line = Line::from(vec![
            Span::styled(" Command: ", Style::default().fg(self.theme.text_secondary)),
            Span::styled(cmd, Style::default().fg(self.theme.text_primary)),
        ]);
        Paragraph::new(cmd_line).render(chunks[2], buffer);

        // Border
        let border_style = Style::default().fg(self.theme.panel_border);
        for x in chunks[4].x..chunks[4].x + chunks[4].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[4].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Keybind hints
        let key_style = Style::default().fg(self.theme.text_highlight);
        let label_style = Style::default().fg(self.theme.text_secondary);
        let hints = Line::from(vec![
            Span::styled("Enter", key_style),
            Span::styled(" Confirm & Run", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Cancel", label_style),
        ]);
        Paragraph::new(hints).render(chunks[5], buffer);
    }

    /// Handles input in edit mode.
    fn handle_edit_input(&mut self, key: KeyEvent) -> Option<PanelResult> {
        let edit_state = self.edit_mode.as_mut()?;

        // Handle danger confirmation mode
        if edit_state.is_confirming() {
            return match key.code {
                KeyCode::Enter => {
                    let result = match edit_state.confirm_dangerous() {
                        Some(cmd) if !cmd.is_empty() => PanelResult::Execute(cmd),
                        _ => PanelResult::Continue,
                    };
                    self.exit_edit_mode();
                    Some(result)
                }
                KeyCode::Esc => {
                    edit_state.cancel_confirm();
                    Some(PanelResult::Continue)
                }
                _ => Some(PanelResult::Continue),
            };
        }

        // Handle Ctrl+key commands
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('u') => {
                    edit_state.undo();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('d') => {
                    edit_state.delete_token();
                    self.update_suggestions_with_history();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('a') => {
                    edit_state.insert_token_after();
                    self.update_suggestions_with_history();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('i') => {
                    edit_state.insert_token_before();
                    self.update_suggestions_with_history();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('q') => {
                    edit_state.cycle_quote();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('!') => {
                    edit_state.toggle_danger_check();
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
                let token_text = edit_state
                    .tokens
                    .get(edit_state.selected)
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
                // Use check_and_prepare_execute for danger checking
                if let Some(command) = edit_state.check_and_prepare_execute() {
                    self.exit_edit_mode();
                    Some(PanelResult::Execute(command))
                } else {
                    // Danger confirmation needed - stay in edit mode
                    Some(PanelResult::Continue)
                }
            }
            KeyCode::Left => {
                edit_state.prev();
                self.update_suggestions_with_history();
                Some(PanelResult::Continue)
            }
            KeyCode::Right => {
                edit_state.next();
                self.update_suggestions_with_history();
                Some(PanelResult::Continue)
            }
            KeyCode::Home => {
                edit_state.select(0);
                self.update_suggestions_with_history();
                Some(PanelResult::Continue)
            }
            KeyCode::End => {
                let last = edit_state.token_count().saturating_sub(1);
                edit_state.select(last);
                self.update_suggestions_with_history();
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
                self.update_suggestions_with_history();
                Some(PanelResult::Continue)
            }
            KeyCode::BackTab => {
                edit_state.prev();
                self.update_suggestions_with_history();
                Some(PanelResult::Continue)
            }
            KeyCode::Char('|') => {
                // Pipe: commit current token, add pipe, start new token with pipeable suggestions
                // This mirrors the file browser's pipe handling behavior
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
                edit_state
                    .tokens
                    .insert(pipe_pos, CommandToken::new("|", TokenType::Argument));
                // Point to virtual "new token" position by creating an empty token
                let empty_pos = pipe_pos + 1;
                edit_state
                    .tokens
                    .insert(empty_pos, CommandToken::new("", TokenType::Argument));
                edit_state.selected = empty_pos;
                edit_state.edit_buffer.clear();
                edit_state.suggestion_index = None;
                self.update_suggestions_with_history();
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

    /// Formats a relative time string (e.g., "5m", "2h", "3d").
    fn format_relative_time(&self, record: &HistoryRecord) -> String {
        let Some(timestamp) = record.timestamp else {
            return "-".to_string();
        };

        let now = Utc::now();
        let duration = now.signed_duration_since(timestamp);

        if duration.num_seconds() < 60 {
            "now".to_string()
        } else if duration.num_minutes() < 60 {
            format!("{}m", duration.num_minutes())
        } else if duration.num_hours() < 24 {
            format!("{}h", duration.num_hours())
        } else if duration.num_days() < 30 {
            format!("{}d", duration.num_days())
        } else if duration.num_days() < 365 {
            format!("{}mo", duration.num_days() / 30)
        } else {
            format!("{}y", duration.num_days() / 365)
        }
    }

    /// Formats a duration string (e.g., "1.2s", "5m3s").
    fn format_duration(&self, record: &HistoryRecord) -> String {
        let Some(duration) = record.duration else {
            return "-".to_string();
        };

        let ms = duration.as_millis();
        if ms < 10 {
            return "-".to_string();
        }

        let secs = duration.as_secs();
        if secs == 0 {
            format!("{}ms", ms)
        } else if secs < 60 {
            format!("{:.1}s", duration.as_secs_f64())
        } else if secs < 3600 {
            format!("{}m{}s", secs / 60, secs % 60)
        } else {
            format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
        }
    }

    /// Formats the exit status indicator.
    fn format_exit_status(&self, record: &HistoryRecord) -> (&'static str, Color) {
        match record.exit_status {
            Some(0) => ("ok", self.theme.semantic_success),
            Some(code) => {
                if code > 128 {
                    ("!!", self.theme.semantic_error) // Signal
                } else {
                    ("!!", self.theme.semantic_warning) // Non-zero exit
                }
            }
            None => ("  ", self.theme.text_secondary),
        }
    }

    /// Renders the table header row.
    fn render_header(&self, buffer: &mut Buffer, area: Rect, cols: &ColumnWidths) {
        let style = Style::default()
            .fg(self.theme.header_fg)
            .add_modifier(Modifier::BOLD);
        let dim = Style::default().fg(self.theme.text_secondary);

        let mut x = area.x;

        // Command column (first)
        let cmd_text = "Command";
        for (i, ch) in cmd_text.chars().enumerate() {
            if x + (i as u16) < area.x + cols.command {
                if let Some(cell) = buffer.cell_mut((x + (i as u16), area.y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
        }
        x += cols.command;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(dim);
        }
        x += 1;

        // When column
        let time_text = "When";
        for (i, ch) in time_text.chars().enumerate() {
            if let Some(cell) = buffer.cell_mut((x + i as u16, area.y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
        x += cols.time;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(dim);
        }
        x += 1;

        // Duration column
        let dur_text = "Dur";
        for (i, ch) in dur_text.chars().enumerate() {
            if let Some(cell) = buffer.cell_mut((x + i as u16, area.y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }
        x += cols.duration;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(dim);
        }
        x += 1;

        // Count column (only in dedupe/frequency modes)
        if cols.count > 0 {
            let count_text = "#";
            for (i, ch) in count_text.chars().enumerate() {
                if let Some(cell) = buffer.cell_mut((x + i as u16, area.y)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
            }
            x += cols.count;

            // Separator
            if let Some(cell) = buffer.cell_mut((x, area.y)) {
                cell.set_char('|');
                cell.set_style(dim);
            }
            x += 1;
        }

        // Status column (last)
        let status_text = "St";
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char(status_text.chars().next().unwrap_or(' '));
            cell.set_style(style);
        }
        if let Some(cell) = buffer.cell_mut((x + 1, area.y)) {
            cell.set_char(status_text.chars().nth(1).unwrap_or(' '));
            cell.set_style(style);
        }
    }

    /// Renders a single table row.
    fn render_row(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        record: &HistoryRecord,
        cols: &ColumnWidths,
        is_selected: bool,
    ) {
        let base_style = if is_selected {
            Style::default().bg(self.theme.selection_bg)
        } else {
            Style::default()
        };
        let dim = Style::default().fg(self.theme.text_secondary);

        // Fill background for selected row
        if is_selected {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buffer.cell_mut((x, area.y)) {
                    cell.set_style(base_style);
                }
            }
        }

        let mut x = area.x;

        // Command column (first)
        let cmd_style = if is_selected {
            base_style
                .fg(self.theme.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(self.theme.text_primary)
        };
        let cmd_width = cols.command.saturating_sub(1) as usize;
        let cmd_display = if crate::ui::text_width::display_width(&record.command) > cmd_width {
            crate::ui::text_width::truncate_with_ellipsis(&record.command, cmd_width).into_owned()
        } else {
            record.command.clone()
        };
        {
            let mut col: u16 = 0;
            for ch in cmd_display.chars() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                if col + ch_w > cols.command {
                    break;
                }
                if let Some(cell) = buffer.cell_mut((x + col, area.y)) {
                    cell.set_char(ch);
                    cell.set_style(cmd_style);
                }
                col += ch_w;
            }
        }
        x += cols.command;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(if is_selected {
                base_style.fg(self.theme.text_secondary)
            } else {
                dim
            });
        }
        x += 1;

        // When column
        let time_text = self.format_relative_time(record);
        let time_style = base_style.fg(self.theme.semantic_info);
        let time_padded = crate::ui::text_width::pad_right_align(&time_text, 5);
        {
            let max_col = cols.time.saturating_sub(1) as usize;
            let mut col: u16 = 0;
            for ch in time_padded.chars() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                if (col + ch_w) as usize > max_col {
                    break;
                }
                if let Some(cell) = buffer.cell_mut((x + col, area.y)) {
                    cell.set_char(ch);
                    cell.set_style(time_style);
                }
                col += ch_w;
            }
        }
        x += cols.time;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(if is_selected {
                base_style.fg(self.theme.text_secondary)
            } else {
                dim
            });
        }
        x += 1;

        // Duration column
        let dur_text = self.format_duration(record);
        let dur_style = base_style.fg(self.theme.git_fg);
        let dur_padded = crate::ui::text_width::pad_right_align(&dur_text, 7);
        {
            let max_col = cols.duration.saturating_sub(1) as usize;
            let mut col: u16 = 0;
            for ch in dur_padded.chars() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                if (col + ch_w) as usize > max_col {
                    break;
                }
                if let Some(cell) = buffer.cell_mut((x + col, area.y)) {
                    cell.set_char(ch);
                    cell.set_style(dur_style);
                }
                col += ch_w;
            }
        }
        x += cols.duration;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(if is_selected {
                base_style.fg(self.theme.text_secondary)
            } else {
                dim
            });
        }
        x += 1;

        // Count column (only in dedupe/frequency modes)
        if cols.count > 0 {
            let count_text = format!("{:>4}", record.execution_count);
            let count_style = if record.execution_count > 10 {
                base_style.fg(self.theme.text_highlight)
            } else if record.execution_count > 1 {
                base_style.fg(self.theme.text_primary)
            } else {
                base_style.fg(self.theme.text_secondary)
            };
            {
                let max_col = cols.count.saturating_sub(1) as usize;
                let mut col: u16 = 0;
                for ch in count_text.chars() {
                    let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                    if (col + ch_w) as usize > max_col {
                        break;
                    }
                    if let Some(cell) = buffer.cell_mut((x + col, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(count_style);
                    }
                    col += ch_w;
                }
            }
            x += cols.count;

            // Separator
            if let Some(cell) = buffer.cell_mut((x, area.y)) {
                cell.set_char('|');
                cell.set_style(if is_selected {
                    base_style.fg(self.theme.text_secondary)
                } else {
                    dim
                });
            }
            x += 1;
        }

        // Status column (last)
        let (status_text, status_color) = self.format_exit_status(record);
        let status_style = base_style.fg(status_color);
        {
            let max_col = cols.status as usize;
            let mut col: u16 = 0;
            for ch in status_text.chars() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
                if (col + ch_w) as usize > max_col {
                    break;
                }
                if let Some(cell) = buffer.cell_mut((x + col, area.y)) {
                    cell.set_char(ch);
                    cell.set_style(status_style);
                }
                col += ch_w;
            }
        }
    }
}

impl Panel for HistoryBrowserPanel {
    fn preferred_height(&self) -> u16 {
        15
    }

    fn title(&self) -> &str {
        "History"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 6 || area.width < 30 {
            return;
        }

        // If in edit mode, render the edit UI instead
        if self.in_edit_mode() {
            self.render_edit_mode(buffer, area);
            return;
        }

        // Layout: filter (1) + header (1) + separator (1) + list (n) + border (1) + keybinds (1)
        let chunks = Layout::vertical([
            Constraint::Length(1), // Filter input
            Constraint::Length(1), // Table header
            Constraint::Length(1), // Separator line
            Constraint::Min(1),    // Table body
            Constraint::Length(1), // Border line
            Constraint::Length(1), // Keybind hints bar
        ])
        .split(area);

        // Determine if we need count column (dedupe or frequency modes)
        let show_count = self.filter_mode.dedupe
            || matches!(self.sort_mode, SortMode::Frequency | SortMode::Frecency);
        let cols = ColumnWidths::calculate(area.width, show_count);

        // Render filter input
        let filter_text = if self.filter.is_empty() {
            Span::styled(
                "Type to filter...",
                Style::default().fg(self.theme.text_secondary),
            )
        } else {
            Span::styled(&self.filter, Style::default().fg(self.theme.text_primary))
        };
        let mut filter_spans = vec![
            Span::styled(" > ", Style::default().fg(self.theme.git_fg)),
            filter_text,
            Span::styled(
                format!("  [{} entries]", self.records.len()),
                Style::default().fg(self.theme.text_secondary),
            ),
        ];
        if self.filter_mode.current_dir_only {
            if let Some(ref cwd) = self.current_cwd {
                let path_str = cwd.to_string_lossy();
                filter_spans.push(Span::styled(
                    format!("  in {}", path_str),
                    Style::default().fg(self.theme.header_fg),
                ));
            }
        }
        Paragraph::new(Line::from(filter_spans)).render(chunks[0], buffer);

        // Render table header
        self.render_header(buffer, chunks[1], &cols);

        // Render separator line
        let sep_style = Style::default().fg(self.theme.panel_border);
        for x in chunks[2].x..chunks[2].x + chunks[2].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[2].y)) {
                cell.set_char('─');
                cell.set_style(sep_style);
            }
        }

        // Render table body
        let visible_height = chunks[3].height as usize;
        self.ensure_visible(visible_height);

        for (display_idx, record) in self
            .records
            .iter()
            .skip(self.scroll_offset)
            .take(visible_height)
            .enumerate()
        {
            let actual_idx = self.scroll_offset + display_idx;
            let is_selected = actual_idx == self.selection;
            let row_area = Rect::new(
                chunks[3].x,
                chunks[3].y + display_idx as u16,
                chunks[3].width,
                1,
            );
            self.render_row(buffer, row_area, record, &cols, is_selected);
        }

        // Render border line above keybind bar
        let border_style = Style::default().fg(self.theme.panel_border);
        for x in chunks[4].x..chunks[4].x + chunks[4].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[4].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Render keybind bar
        let key_style = Style::default().fg(self.theme.text_highlight);
        let label_style = Style::default().fg(self.theme.text_secondary);
        let active_label = Style::default()
            .fg(self.theme.semantic_success)
            .add_modifier(Modifier::BOLD);
        let sort_style = Style::default().fg(self.theme.header_fg);

        let hints = Line::from(vec![
            Span::styled("^E", key_style),
            Span::styled(" Edit", label_style),
            Span::raw("  "),
            Span::styled("^D", key_style),
            Span::styled(
                " Dedupe",
                if self.filter_mode.dedupe {
                    active_label
                } else {
                    label_style
                },
            ),
            Span::raw("  "),
            Span::styled("^G", key_style),
            Span::styled(
                " CurDir",
                if self.filter_mode.current_dir_only {
                    active_label
                } else {
                    label_style
                },
            ),
            Span::raw("  "),
            Span::styled("^X", key_style),
            Span::styled(
                " Failed",
                if self.filter_mode.failed_only {
                    active_label
                } else {
                    label_style
                },
            ),
            Span::raw("  "),
            Span::styled("^S", key_style),
            Span::styled(format!(" {}", self.sort_mode.name()), sort_style),
            Span::raw("  "),
            Span::styled("Enter", key_style),
            Span::styled(" Run", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Close", label_style),
        ]);
        Paragraph::new(hints).render(chunks[5], buffer);
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // If in edit mode, delegate to edit handler
        if self.in_edit_mode() {
            if let Some(result) = self.handle_edit_input(key) {
                return result;
            }
        }

        // Check for Ctrl+key toggles first
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('e') => {
                    self.enter_edit_mode();
                    return PanelResult::Continue;
                }
                KeyCode::Char('d') => {
                    self.toggle_dedupe();
                    return PanelResult::Continue;
                }
                KeyCode::Char('g') => {
                    self.toggle_current_dir();
                    return PanelResult::Continue;
                }
                KeyCode::Char('x') => {
                    self.toggle_failed();
                    return PanelResult::Continue;
                }
                KeyCode::Char('s') => {
                    self.cycle_sort();
                    return PanelResult::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => PanelResult::Dismiss,
            KeyCode::Enter => {
                if let Some(cmd) = self.selected_command() {
                    PanelResult::Execute(cmd.to_string())
                } else {
                    PanelResult::Dismiss
                }
            }
            KeyCode::Tab => {
                if let Some(cmd) = self.selected_command() {
                    PanelResult::InsertText(cmd.to_string())
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
                if self.selection + 1 < self.records.len() {
                    self.selection += 1;
                }
                PanelResult::Continue
            }
            KeyCode::PageUp => {
                self.selection = self.selection.saturating_sub(10);
                PanelResult::Continue
            }
            KeyCode::PageDown => {
                self.selection = (self.selection + 10).min(self.records.len().saturating_sub(1));
                PanelResult::Continue
            }
            KeyCode::Home => {
                self.selection = 0;
                PanelResult::Continue
            }
            KeyCode::End => {
                self.selection = self.records.len().saturating_sub(1);
                PanelResult::Continue
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.load_history();
                PanelResult::Continue
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.load_history();
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
    use super::super::buffer_convert::buffer_to_ansi;
    use super::super::command_edit::{
        QuoteStyle, TokenType, apply_quotes, check_dangerous_command, split_quotes,
        tokenize_command,
    };
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    fn test_history_browser_new_constructs_empty_records() {
        let panel = HistoryBrowserPanel::new(&AMBER_THEME);
        assert!(panel.records.is_empty());
        assert_eq!(panel.selection, 0);
    }

    #[test]
    fn test_filter_mode_default_flags_false() {
        let panel = HistoryBrowserPanel::new(&AMBER_THEME);
        assert!(!panel.filter_mode.dedupe);
        assert!(!panel.filter_mode.current_dir_only);
        assert!(!panel.filter_mode.failed_only);
    }

    #[test]
    fn test_sort_mode_cycle_through_variants() {
        let mut panel = HistoryBrowserPanel::new(&AMBER_THEME);
        assert_eq!(panel.sort_mode, SortMode::Recency);
        panel.sort_mode = panel.sort_mode.next();
        assert_eq!(panel.sort_mode, SortMode::Frequency);
        panel.sort_mode = panel.sort_mode.next();
        assert_eq!(panel.sort_mode, SortMode::Frecency);
        panel.sort_mode = panel.sort_mode.next();
        assert_eq!(panel.sort_mode, SortMode::Recency);
    }

    #[test]
    fn test_column_widths_calculated_for_80_cols() {
        let cols = ColumnWidths::calculate(80, true);
        assert_eq!(cols.status, 2);
        assert_eq!(cols.count, 5);
        assert_eq!(cols.time, 6);
        assert_eq!(cols.duration, 8);
        assert!(cols.command > 0);
    }

    // Tokenizer tests (delegated to command_edit module)
    #[test]
    fn test_tokenize_command_simple_parses_command() {
        let tokens = tokenize_command("ls -la /tmp");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].text, "ls");
        assert_eq!(tokens[0].token_type, TokenType::Command);
    }

    #[test]
    fn test_tokenize_command_quoted_returns_quoted_token() {
        let tokens = tokenize_command("echo \"hello world\"");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[1].text, "\"hello world\"");
    }

    #[test]
    fn test_tokenize_command_git_subcommand_detected() {
        let tokens = tokenize_command("git checkout -b feature/new-branch");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[1].token_type, TokenType::Subcommand);
    }

    // Dangerous command tests
    #[test]
    fn test_check_dangerous_command_detects_rm_rf() {
        assert!(check_dangerous_command("rm -rf /").is_some());
        assert!(check_dangerous_command("sudo rm -rf /tmp/build").is_some());
    }

    #[test]
    fn test_check_dangerous_command_allows_safe_commands() {
        assert!(check_dangerous_command("ls -la").is_none());
        assert!(check_dangerous_command("git push origin main").is_none());
    }

    // Quote handling tests
    #[test]
    fn test_split_quotes_returns_style() {
        assert_eq!(
            split_quotes("hello"),
            ("hello".to_string(), QuoteStyle::None)
        );
        assert_eq!(
            split_quotes("'hello'"),
            ("hello".to_string(), QuoteStyle::Single)
        );
        assert_eq!(
            split_quotes("\"hello\""),
            ("hello".to_string(), QuoteStyle::Double)
        );
    }

    #[test]
    fn test_apply_quotes_wraps_correctly() {
        assert_eq!(apply_quotes("hello", QuoteStyle::None), "hello");
        assert_eq!(apply_quotes("hello", QuoteStyle::Single), "'hello'");
        assert_eq!(apply_quotes("hello", QuoteStyle::Double), "\"hello\"");
    }

    // Edit state tests (using shared module)
    #[test]
    fn test_command_edit_state_navigation_moves_selection() {
        let mut state = CommandEditState::from_command("git push origin main");
        assert_eq!(state.selected, 0);
        state.next();
        assert_eq!(state.selected, 1);
        state.prev();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_command_edit_state_insert_delete_updates_token_count() {
        let mut state = CommandEditState::from_command("git push");
        assert_eq!(state.token_count(), 2);
        state.select(1);
        state.insert_token_after();
        assert_eq!(state.token_count(), 3);
        state.delete_token();
        assert_eq!(state.token_count(), 2);
    }

    #[test]
    fn test_command_edit_state_undo_restores_tokens() {
        let mut state = CommandEditState::from_command("git push origin main");
        state.select(2);
        state.delete_token();
        assert_eq!(state.token_count(), 3);
        state.undo();
        assert_eq!(state.token_count(), 4);
    }

    #[test]
    fn test_command_edit_state_quote_cycling_cycles_quotes() {
        let mut state = CommandEditState::from_command("echo hello");
        state.select(1);
        state.cycle_quote();
        assert_eq!(state.edit_buffer, "'hello'");
        state.cycle_quote();
        assert_eq!(state.edit_buffer, "\"hello\"");
    }

    #[test]
    fn test_edit_mode_original_hint_preserves_wide_chars() {
        let mut panel = HistoryBrowserPanel::new(&AMBER_THEME);
        panel.edit_mode = Some(CommandEditState::from_command("echo 你好"));

        let area = Rect::new(0, 0, 60, 12);
        let mut buffer = Buffer::empty(area);
        panel.render(&mut buffer, area);

        let ansi = buffer_to_ansi(&buffer, area);
        let visible = strip_ansi_for_test(&ansi);
        assert!(
            visible.contains("Original: echo 你好"),
            "expected wide chars in original hint, got: {visible:?}"
        );
    }

    fn strip_ansi_for_test(s: &str) -> String {
        let mut result = String::new();
        let mut chars = s.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                result.push(ch);
            }
        }
        result
    }
}
