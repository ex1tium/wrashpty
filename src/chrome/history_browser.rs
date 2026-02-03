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
use ratatui_widgets::paragraph::Paragraph;
use tracing::{debug, warn};

use super::panel::{Panel, PanelResult};
use crate::history_store::{FilterMode, HistoryRecord, HistoryStore, SortMode};

/// A token from a shell command (word, flag, or quoted string).
#[derive(Debug, Clone)]
struct CommandToken {
    /// The text content of this token.
    text: String,
    /// Whether this token is likely editable (not a flag or command).
    editable: bool,
}

/// State for edit mode.
#[derive(Debug, Clone)]
struct EditModeState {
    /// The original command being edited.
    original: String,
    /// Tokenized parts of the command.
    tokens: Vec<CommandToken>,
    /// Index of the currently selected token (0-based).
    selected: usize,
    /// Current edit buffer for the selected token.
    edit_buffer: String,
    /// Whether we're actively editing the current token.
    editing: bool,
}

impl EditModeState {
    /// Creates a new edit mode state from a command string.
    fn new(command: &str) -> Self {
        let tokens = tokenize_command(command);
        let edit_buffer = tokens.first().map(|t| t.text.clone()).unwrap_or_default();
        Self {
            original: command.to_string(),
            tokens,
            selected: 0,
            edit_buffer,
            editing: false,
        }
    }

    /// Returns the number of tokens.
    fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Selects a token by index.
    fn select(&mut self, index: usize) {
        if index < self.tokens.len() {
            // Save current edit
            if let Some(token) = self.tokens.get_mut(self.selected) {
                token.text = self.edit_buffer.clone();
            }
            self.selected = index;
            self.edit_buffer = self.tokens[index].text.clone();
            self.editing = true;
        }
    }

    /// Moves to the next token.
    fn next(&mut self) {
        if self.selected + 1 < self.tokens.len() {
            self.select(self.selected + 1);
        }
    }

    /// Moves to the previous token.
    fn prev(&mut self) {
        if self.selected > 0 {
            self.select(self.selected - 1);
        }
    }

    /// Builds the final command from the edited tokens.
    fn build_command(&mut self) -> String {
        // Save current edit buffer
        if let Some(token) = self.tokens.get_mut(self.selected) {
            token.text = self.edit_buffer.clone();
        }
        self.tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// Tokenizes a shell command into words, respecting quotes.
fn tokenize_command(command: &str) -> Vec<CommandToken> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;
    let mut is_first = true;

    for ch in command.chars() {
        if escape_next {
            current.push(ch);
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if !in_single_quote => {
                current.push(ch);
                escape_next = true;
            }
            '\'' if !in_double_quote => {
                current.push(ch);
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                current.push(ch);
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    let editable = !is_first && !current.starts_with('-');
                    tokens.push(CommandToken {
                        text: current.clone(),
                        editable,
                    });
                    current.clear();
                    is_first = false;
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }

    // Don't forget the last token
    if !current.is_empty() {
        let editable = !is_first && !current.starts_with('-');
        tokens.push(CommandToken {
            text: current,
            editable,
        });
    }

    tokens
}

/// Column widths for the table view
struct ColumnWidths {
    command: u16,   // Command (flexible, first column)
    time: u16,      // Relative time
    duration: u16,  // Command duration
    count: u16,     // Execution count (optional)
    status: u16,    // Exit status indicator
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

        Self { command, time, duration, count, status }
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
    edit_mode: Option<EditModeState>,
}

impl HistoryBrowserPanel {
    /// Creates a new empty history browser.
    pub fn new() -> Self {
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
            self.edit_mode = Some(EditModeState::new(cmd));
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

    /// Renders the edit mode UI.
    fn render_edit_mode(&self, buffer: &mut Buffer, area: Rect) {
        let Some(edit_state) = &self.edit_mode else { return };

        // Layout: title (1) + command display (2) + separator (1) + edit area (2) + border (1) + keybinds (1)
        let chunks = Layout::vertical([
            Constraint::Length(1), // Title
            Constraint::Length(2), // Command with tokens
            Constraint::Length(1), // Separator
            Constraint::Length(2), // Edit input area
            Constraint::Min(1),    // Spacer
            Constraint::Length(1), // Border
            Constraint::Length(1), // Keybind hints
        ])
        .split(area);

        // Title
        let title = Line::from(vec![
            Span::styled(" Edit Command ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("- modify tokens and press Enter to run", Style::default().fg(Color::DarkGray)),
        ]);
        Paragraph::new(title).render(chunks[0], buffer);

        // Render tokenized command with slot numbers
        let mut spans = Vec::new();
        spans.push(Span::raw(" "));

        for (i, token) in edit_state.tokens.iter().enumerate() {
            let is_selected = i == edit_state.selected;
            let slot_num = i + 1; // 1-indexed for display

            // Slot number indicator (visual reference for position)
            let num_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(format!("{}", slot_num), num_style));

            // Token text
            let token_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else if token.editable {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            // Show edit buffer for selected token, original text for others
            let display_text = if is_selected {
                &edit_state.edit_buffer
            } else {
                &token.text
            };
            spans.push(Span::styled(display_text.clone(), token_style));
            spans.push(Span::raw(" "));
        }

        let cmd_line = Line::from(spans);
        Paragraph::new(cmd_line).render(chunks[1], buffer);

        // Separator
        let sep_style = Style::default().fg(Color::DarkGray);
        for x in chunks[2].x..chunks[2].x + chunks[2].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[2].y)) {
                cell.set_char('─');
                cell.set_style(sep_style);
            }
        }

        // Edit area - show current token being edited
        let edit_label = format!(" Editing slot {}: ", edit_state.selected + 1);
        let edit_line = Line::from(vec![
            Span::styled(edit_label, Style::default().fg(Color::Magenta)),
            Span::styled(&edit_state.edit_buffer, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled("█", Style::default().fg(Color::White)), // Cursor
        ]);
        Paragraph::new(edit_line).render(chunks[3], buffer);

        // Border
        let border_style = Style::default().fg(Color::DarkGray);
        for x in chunks[5].x..chunks[5].x + chunks[5].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[5].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Keybind hints for edit mode
        let key_style = Style::default().fg(Color::Yellow);
        let label_style = Style::default().fg(Color::DarkGray);

        let hints = Line::from(vec![
            Span::styled("←→/Tab", key_style),
            Span::styled(" Navigate", label_style),
            Span::raw("  "),
            Span::styled("Home/End", key_style),
            Span::styled(" First/Last", label_style),
            Span::raw("  "),
            Span::styled("Enter", key_style),
            Span::styled(" Run", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Back", label_style),
        ]);
        Paragraph::new(hints).render(chunks[6], buffer);
    }

    /// Handles input in edit mode. Returns Some(PanelResult) if handled.
    fn handle_edit_input(&mut self, key: KeyEvent) -> Option<PanelResult> {
        let edit_state = self.edit_mode.as_mut()?;

        match key.code {
            KeyCode::Esc => {
                self.exit_edit_mode();
                Some(PanelResult::Continue)
            }
            KeyCode::Enter => {
                // Build and execute the edited command
                let command = edit_state.build_command();
                self.exit_edit_mode();
                // Execute directly since reedline doesn't support buffer injection
                Some(PanelResult::Execute(command))
            }
            KeyCode::Left => {
                edit_state.prev();
                Some(PanelResult::Continue)
            }
            KeyCode::Right => {
                edit_state.next();
                Some(PanelResult::Continue)
            }
            KeyCode::Home => {
                edit_state.select(0);
                Some(PanelResult::Continue)
            }
            KeyCode::End => {
                let last = edit_state.token_count().saturating_sub(1);
                edit_state.select(last);
                Some(PanelResult::Continue)
            }
            KeyCode::Tab => {
                // Tab moves to next token (more intuitive than right arrow for some)
                edit_state.next();
                Some(PanelResult::Continue)
            }
            KeyCode::BackTab => {
                // Shift+Tab moves to previous token
                edit_state.prev();
                Some(PanelResult::Continue)
            }
            KeyCode::Char(c) => {
                edit_state.edit_buffer.push(c);
                edit_state.editing = true;
                Some(PanelResult::Continue)
            }
            KeyCode::Backspace => {
                edit_state.edit_buffer.pop();
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
            Some(0) => ("ok", Color::Green),
            Some(code) => {
                if code > 128 {
                    ("!!", Color::Red)  // Signal
                } else {
                    ("!!", Color::Yellow)  // Non-zero exit
                }
            }
            None => ("  ", Color::DarkGray),
        }
    }

    /// Renders the table header row.
    fn render_header(&self, buffer: &mut Buffer, area: Rect, cols: &ColumnWidths) {
        let style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
        let dim = Style::default().fg(Color::DarkGray);

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
    fn render_row(&self, buffer: &mut Buffer, area: Rect, record: &HistoryRecord, cols: &ColumnWidths, is_selected: bool) {
        let base_style = if is_selected {
            Style::default().bg(Color::DarkGray)
        } else {
            Style::default()
        };
        let dim = Style::default().fg(Color::DarkGray);

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
            base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(Color::White)
        };
        let cmd_width = cols.command.saturating_sub(1) as usize;
        let cmd_display = if record.command.len() > cmd_width {
            format!("{}...", &record.command[..cmd_width.saturating_sub(3)])
        } else {
            record.command.clone()
        };
        for (i, ch) in cmd_display.chars().enumerate() {
            if (i as u16) < cols.command {
                if let Some(cell) = buffer.cell_mut((x + (i as u16), area.y)) {
                    cell.set_char(ch);
                    cell.set_style(cmd_style);
                }
            }
        }
        x += cols.command;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(if is_selected { base_style.fg(Color::DarkGray) } else { dim });
        }
        x += 1;

        // When column
        let time_text = self.format_relative_time(record);
        let time_style = base_style.fg(Color::Blue);
        for (i, ch) in format!("{:>5}", time_text).chars().take(cols.time as usize - 1).enumerate() {
            if let Some(cell) = buffer.cell_mut((x + i as u16, area.y)) {
                cell.set_char(ch);
                cell.set_style(time_style);
            }
        }
        x += cols.time;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(if is_selected { base_style.fg(Color::DarkGray) } else { dim });
        }
        x += 1;

        // Duration column
        let dur_text = self.format_duration(record);
        let dur_style = base_style.fg(Color::Magenta);
        for (i, ch) in format!("{:>7}", dur_text).chars().take(cols.duration as usize - 1).enumerate() {
            if let Some(cell) = buffer.cell_mut((x + i as u16, area.y)) {
                cell.set_char(ch);
                cell.set_style(dur_style);
            }
        }
        x += cols.duration;

        // Separator
        if let Some(cell) = buffer.cell_mut((x, area.y)) {
            cell.set_char('|');
            cell.set_style(if is_selected { base_style.fg(Color::DarkGray) } else { dim });
        }
        x += 1;

        // Count column (only in dedupe/frequency modes)
        if cols.count > 0 {
            let count_text = format!("{:>4}", record.execution_count);
            let count_style = if record.execution_count > 10 {
                base_style.fg(Color::Yellow)
            } else if record.execution_count > 1 {
                base_style.fg(Color::White)
            } else {
                base_style.fg(Color::DarkGray)
            };
            for (i, ch) in count_text.chars().take(cols.count as usize - 1).enumerate() {
                if let Some(cell) = buffer.cell_mut((x + i as u16, area.y)) {
                    cell.set_char(ch);
                    cell.set_style(count_style);
                }
            }
            x += cols.count;

            // Separator
            if let Some(cell) = buffer.cell_mut((x, area.y)) {
                cell.set_char('|');
                cell.set_style(if is_selected { base_style.fg(Color::DarkGray) } else { dim });
            }
            x += 1;
        }

        // Status column (last)
        let (status_text, status_color) = self.format_exit_status(record);
        let status_style = base_style.fg(status_color);
        for (i, ch) in status_text.chars().take(cols.status as usize).enumerate() {
            if let Some(cell) = buffer.cell_mut((x + i as u16, area.y)) {
                cell.set_char(ch);
                cell.set_style(status_style);
            }
        }
    }
}

impl Default for HistoryBrowserPanel {
    fn default() -> Self {
        Self::new()
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
        let show_count = self.filter_mode.dedupe || matches!(self.sort_mode, SortMode::Frequency | SortMode::Frecency);
        let cols = ColumnWidths::calculate(area.width, show_count);

        // Render filter input
        let filter_text = if self.filter.is_empty() {
            Span::styled("Type to filter...", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled(&self.filter, Style::default().fg(Color::White))
        };
        let filter_line = Line::from(vec![
            Span::styled(" > ", Style::default().fg(Color::Magenta)),
            filter_text,
            Span::styled(
                format!("  [{} entries]", self.records.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        Paragraph::new(filter_line).render(chunks[0], buffer);

        // Render table header
        self.render_header(buffer, chunks[1], &cols);

        // Render separator line
        let sep_style = Style::default().fg(Color::DarkGray);
        for x in chunks[2].x..chunks[2].x + chunks[2].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[2].y)) {
                cell.set_char('─');
                cell.set_style(sep_style);
            }
        }

        // Render table body
        let visible_height = chunks[3].height as usize;
        self.ensure_visible(visible_height);

        for (display_idx, record) in self.records
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
        let border_style = Style::default().fg(Color::DarkGray);
        for x in chunks[4].x..chunks[4].x + chunks[4].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[4].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Render keybind bar
        let key_style = Style::default().fg(Color::Yellow);
        let label_style = Style::default().fg(Color::DarkGray);
        let active_label = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
        let sort_style = Style::default().fg(Color::Cyan);

        let hints = Line::from(vec![
            Span::styled("^E", key_style),
            Span::styled(" Edit", label_style),
            Span::raw("  "),
            Span::styled("^D", key_style),
            Span::styled(" Dedupe", if self.filter_mode.dedupe { active_label } else { label_style }),
            Span::raw("  "),
            Span::styled("^G", key_style),
            Span::styled(" CurDir", if self.filter_mode.current_dir_only { active_label } else { label_style }),
            Span::raw("  "),
            Span::styled("^X", key_style),
            Span::styled(" Failed", if self.filter_mode.failed_only { active_label } else { label_style }),
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
    use super::*;

    #[test]
    fn test_history_browser_new() {
        let panel = HistoryBrowserPanel::new();
        assert!(panel.records.is_empty());
        assert_eq!(panel.selection, 0);
    }

    #[test]
    fn test_filter_mode_default() {
        let panel = HistoryBrowserPanel::new();
        assert!(!panel.filter_mode.dedupe);
        assert!(!panel.filter_mode.current_dir_only);
        assert!(!panel.filter_mode.failed_only);
    }

    #[test]
    fn test_sort_mode_cycle() {
        let mut panel = HistoryBrowserPanel::new();
        assert_eq!(panel.sort_mode, SortMode::Recency);
        panel.sort_mode = panel.sort_mode.next();
        assert_eq!(panel.sort_mode, SortMode::Frequency);
        panel.sort_mode = panel.sort_mode.next();
        assert_eq!(panel.sort_mode, SortMode::Frecency);
        panel.sort_mode = panel.sort_mode.next();
        assert_eq!(panel.sort_mode, SortMode::Recency);
    }

    #[test]
    fn test_column_widths() {
        let cols = ColumnWidths::calculate(80, true);
        assert_eq!(cols.status, 2);
        assert_eq!(cols.count, 5);
        assert_eq!(cols.time, 6);
        assert_eq!(cols.duration, 8);
        // command should get remaining space
        assert!(cols.command > 0);
    }

    // =========================================================================
    // Tokenizer Tests
    // =========================================================================

    #[test]
    fn test_tokenize_simple_command() {
        let tokens = tokenize_command("ls -la /tmp");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].text, "ls");
        assert!(!tokens[0].editable); // First token (command)
        assert_eq!(tokens[1].text, "-la");
        assert!(!tokens[1].editable); // Flag
        assert_eq!(tokens[2].text, "/tmp");
        assert!(tokens[2].editable); // Path argument
    }

    #[test]
    fn test_tokenize_quoted_string() {
        let tokens = tokenize_command("echo \"hello world\"");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "echo");
        assert_eq!(tokens[1].text, "\"hello world\"");
        assert!(tokens[1].editable);
    }

    #[test]
    fn test_tokenize_single_quoted() {
        let tokens = tokenize_command("grep 'pattern with spaces' file.txt");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].text, "'pattern with spaces'");
        assert!(tokens[1].editable);
    }

    #[test]
    fn test_tokenize_git_command() {
        let tokens = tokenize_command("git checkout -b feature/new-branch origin/main");
        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[0].text, "git");
        assert!(!tokens[0].editable);
        assert_eq!(tokens[1].text, "checkout");
        assert!(tokens[1].editable);
        assert_eq!(tokens[2].text, "-b");
        assert!(!tokens[2].editable);
        assert_eq!(tokens[3].text, "feature/new-branch");
        assert!(tokens[3].editable);
        assert_eq!(tokens[4].text, "origin/main");
        assert!(tokens[4].editable);
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize_command("");
        assert!(tokens.is_empty());
    }

    // =========================================================================
    // Edit Mode Tests
    // =========================================================================

    #[test]
    fn test_edit_mode_state_new() {
        let state = EditModeState::new("echo hello");
        assert_eq!(state.original, "echo hello");
        assert_eq!(state.token_count(), 2);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_edit_mode_navigation() {
        let mut state = EditModeState::new("git push origin main");
        assert_eq!(state.selected, 0);

        state.next();
        assert_eq!(state.selected, 1);

        state.next();
        assert_eq!(state.selected, 2);

        state.prev();
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn test_edit_mode_build_command() {
        let mut state = EditModeState::new("echo hello");
        state.select(1);
        state.edit_buffer = "world".to_string();

        let result = state.build_command();
        assert_eq!(result, "echo world");
    }
}
