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

use super::command_edit::{
    CommandEditState, CommandToken, TokenType, compute_edit_mode_layout, render_edit_mode_shared,
};
use super::footer_bar::FooterEntry;
use super::glyphs::GlyphSet;
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

fn command_display_lines(command: &str, cmd_width: usize) -> Vec<String> {
    let max_width = cmd_width.max(1);
    let continuation_prefix = "  ";
    let mut lines: Vec<String> = command
        .split('\n')
        .enumerate()
        .map(|(idx, line)| {
            let display = if idx == 0 {
                line.to_string()
            } else {
                format!("{continuation_prefix}{line}")
            };

            if crate::ui::text_width::display_width(&display) > max_width {
                crate::ui::text_width::truncate_with_ellipsis(&display, max_width).into_owned()
            } else {
                display
            }
        })
        .collect();

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

#[derive(Debug, Clone)]
struct HeredocSpec {
    delimiter: String,
    strip_tabs: bool,
}

fn parse_heredoc_spec(command: &str) -> Option<HeredocSpec> {
    let bytes = command.as_bytes();
    let mut i = 0usize;

    while i + 1 < bytes.len() {
        if bytes[i] != b'<' || bytes[i + 1] != b'<' {
            i += 1;
            continue;
        }

        // Ignore here-strings (<<<)
        if i + 2 < bytes.len() && bytes[i + 2] == b'<' {
            i += 1;
            continue;
        }

        let mut j = i + 2;
        let strip_tabs = if j < bytes.len() && bytes[j] == b'-' {
            j += 1;
            true
        } else {
            false
        };

        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }

        if j >= bytes.len() {
            return None;
        }

        let delimiter = match bytes[j] {
            b'\'' | b'"' => {
                let quote = bytes[j];
                j += 1;
                let start = j;
                while j < bytes.len() && bytes[j] != quote {
                    j += 1;
                }
                if j <= start {
                    return None;
                }
                command[start..j].to_string()
            }
            _ => {
                let start = j;
                while j < bytes.len() {
                    let b = bytes[j];
                    if b.is_ascii_whitespace() || matches!(b, b';' | b'|' | b'&' | b')') {
                        break;
                    }
                    j += 1;
                }
                if j <= start {
                    return None;
                }
                command[start..j].to_string()
            }
        };

        if !delimiter.is_empty() {
            return Some(HeredocSpec {
                delimiter,
                strip_tabs,
            });
        }

        i += 1;
    }

    None
}

fn heredoc_line_matches_delimiter(line: &str, spec: &HeredocSpec) -> bool {
    if spec.strip_tabs {
        line.trim_start_matches('\t') == spec.delimiter
    } else {
        line == spec.delimiter
    }
}

fn try_merge_heredoc_records(records: &[HistoryRecord], idx: usize) -> Option<(String, usize)> {
    const MAX_HEREDOC_LINES: usize = 256;

    let record = records.get(idx)?;
    if record.command.contains('\n') {
        return None;
    }

    let spec = parse_heredoc_spec(&record.command)?;
    let mut joined = record.command.clone();
    let mut consumed = 1usize;

    for next in records.iter().skip(idx + 1).take(MAX_HEREDOC_LINES) {
        // Avoid recursive/ambiguous grouping when the source row
        // already contains newlines.
        if next.command.contains('\n') {
            break;
        }

        joined.push('\n');
        joined.push_str(&next.command);
        consumed += 1;

        if heredoc_line_matches_delimiter(&next.command, &spec) {
            return Some((joined, consumed));
        }
    }

    None
}

#[derive(Debug, Default, Clone, Copy)]
struct ContinuationState {
    open_single_quote: bool,
    open_double_quote: bool,
    open_backtick: bool,
    paren_depth: i32,
    brace_depth: i32,
    if_depth: i32,
    pending_then_depth: i32,
    do_depth: i32,
    pending_do_depth: i32,
    case_depth: i32,
    trailing_backslash: bool,
    trailing_operator: bool,
}

impl ContinuationState {
    fn is_incomplete(self) -> bool {
        self.open_single_quote
            || self.open_double_quote
            || self.open_backtick
            || self.paren_depth > 0
            || self.brace_depth > 0
            || self.if_depth > 0
            || self.pending_then_depth > 0
            || self.do_depth > 0
            || self.pending_do_depth > 0
            || self.case_depth > 0
            || self.trailing_backslash
            || self.trailing_operator
    }
}

fn ends_with_unescaped_backslash(line: &str) -> bool {
    let trimmed = line.trim_end();
    if !trimmed.ends_with('\\') {
        return false;
    }
    let backslashes = trimmed
        .as_bytes()
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count();
    backslashes % 2 == 1
}

fn ends_with_continuation_operator(line: &str) -> bool {
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.ends_with("&&")
        || trimmed.ends_with("||")
        || trimmed.ends_with('|')
        || trimmed.ends_with('(')
        || trimmed.ends_with('{')
        || trimmed.ends_with('>')
        || trimmed.ends_with('<')
    {
        return true;
    }

    let last_word = trimmed
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .rev()
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let last_word = last_word.to_ascii_lowercase();

    matches!(last_word.as_str(), "then" | "do" | "in" | "elif" | "else")
}

fn flush_keyword_token(token: &mut String, state: &mut ContinuationState) {
    if token.is_empty() {
        return;
    }
    let lower = token.to_ascii_lowercase();
    token.clear();

    match lower.as_str() {
        "if" => {
            state.if_depth += 1;
            state.pending_then_depth += 1;
        }
        "elif" => state.pending_then_depth += 1,
        "then" => state.pending_then_depth = (state.pending_then_depth - 1).max(0),
        "fi" => state.if_depth = (state.if_depth - 1).max(0),
        "for" | "while" | "until" | "select" => state.pending_do_depth += 1,
        "do" => {
            state.pending_do_depth = (state.pending_do_depth - 1).max(0);
            state.do_depth += 1;
        }
        "done" => state.do_depth = (state.do_depth - 1).max(0),
        "case" => state.case_depth += 1,
        "esac" => state.case_depth = (state.case_depth - 1).max(0),
        _ => {}
    }
}

fn analyze_command_continuation(command: &str) -> ContinuationState {
    let mut state = ContinuationState::default();
    let mut token = String::new();

    let mut in_single = false;
    let mut in_double = false;
    let mut in_backtick = false;
    let mut escaped_in_double = false;
    let mut in_comment = false;
    let mut prev_was_whitespace = true;

    for ch in command.chars() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
                prev_was_whitespace = true;
            }
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            continue;
        }

        if in_double {
            if escaped_in_double {
                escaped_in_double = false;
                continue;
            }
            match ch {
                '\\' => escaped_in_double = true,
                '"' => in_double = false,
                _ => {}
            }
            continue;
        }

        if in_backtick {
            if ch == '`' {
                in_backtick = false;
            }
            continue;
        }

        match ch {
            '\'' => {
                flush_keyword_token(&mut token, &mut state);
                in_single = true;
                prev_was_whitespace = false;
            }
            '"' => {
                flush_keyword_token(&mut token, &mut state);
                in_double = true;
                prev_was_whitespace = false;
            }
            '`' => {
                flush_keyword_token(&mut token, &mut state);
                in_backtick = true;
                prev_was_whitespace = false;
            }
            '#' => {
                if prev_was_whitespace {
                    flush_keyword_token(&mut token, &mut state);
                    in_comment = true;
                } else {
                    flush_keyword_token(&mut token, &mut state);
                    prev_was_whitespace = false;
                }
            }
            '(' => {
                flush_keyword_token(&mut token, &mut state);
                state.paren_depth += 1;
                prev_was_whitespace = false;
            }
            ')' => {
                flush_keyword_token(&mut token, &mut state);
                state.paren_depth = (state.paren_depth - 1).max(0);
                prev_was_whitespace = false;
            }
            '{' => {
                flush_keyword_token(&mut token, &mut state);
                state.brace_depth += 1;
                prev_was_whitespace = false;
            }
            '}' => {
                flush_keyword_token(&mut token, &mut state);
                state.brace_depth = (state.brace_depth - 1).max(0);
                prev_was_whitespace = false;
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                token.push(c);
                prev_was_whitespace = false;
            }
            c if c.is_whitespace() => {
                flush_keyword_token(&mut token, &mut state);
                prev_was_whitespace = true;
            }
            _ => {
                flush_keyword_token(&mut token, &mut state);
                prev_was_whitespace = false;
            }
        }
    }

    flush_keyword_token(&mut token, &mut state);

    state.open_single_quote = in_single;
    state.open_double_quote = in_double;
    state.open_backtick = in_backtick;

    if let Some(last_line) = command.lines().rev().find(|line| !line.trim().is_empty()) {
        state.trailing_backslash = ends_with_unescaped_backslash(last_line);
        state.trailing_operator = ends_with_continuation_operator(last_line);
    }

    state
}

fn try_merge_general_multiline_records(
    records: &[HistoryRecord],
    idx: usize,
) -> Option<(String, usize)> {
    const MAX_GENERAL_LINES: usize = 64;

    let record = records.get(idx)?;
    if record.command.contains('\n') {
        return None;
    }

    let initial = analyze_command_continuation(&record.command);
    if !initial.is_incomplete() {
        return None;
    }

    let mut joined = record.command.clone();
    let mut consumed = 1usize;

    for next in records.iter().skip(idx + 1).take(MAX_GENERAL_LINES) {
        if next.command.contains('\n') {
            break;
        }

        joined.push('\n');
        joined.push_str(&next.command);
        consumed += 1;

        if !analyze_command_continuation(&joined).is_incomplete() {
            return Some((joined, consumed));
        }
    }

    None
}

fn merge_split_multiline_records(records: Vec<HistoryRecord>) -> Vec<HistoryRecord> {
    let mut merged = Vec::with_capacity(records.len());
    let mut idx = 0usize;

    while idx < records.len() {
        let mut record = records[idx].clone();
        let mut consumed = 1usize;

        if !record.command.contains('\n') {
            if let Some((joined, n)) = try_merge_heredoc_records(&records, idx) {
                record.command = joined;
                consumed = n;
            } else if let Some((joined, n)) = try_merge_general_multiline_records(&records, idx) {
                record.command = joined;
                consumed = n;
            }
        }

        // Normalize metadata across merged fragments so non-recency modes
        // keep stable counters/recency when lines have different aggregates.
        if consumed > 1 {
            for extra in records.iter().skip(idx + 1).take(consumed - 1) {
                if let Some(extra_ts) = extra.timestamp {
                    if record.timestamp.is_none_or(|ts| extra_ts > ts) {
                        record.timestamp = Some(extra_ts);
                    }
                }
                if record.cwd.is_none() {
                    record.cwd = extra.cwd.clone();
                }
                if record.exit_status.is_none() {
                    record.exit_status = extra.exit_status;
                }
                if record.duration.is_none() {
                    record.duration = extra.duration;
                }
                record.execution_count = record.execution_count.min(extra.execution_count);
                record.frecency_score = record.frecency_score.min(extra.frecency_score);
            }
        }

        merged.push(record);
        idx += consumed;
    }

    merged
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
    /// Unified glyph set for the current tier.
    glyphs: &'static GlyphSet,
}

impl HistoryBrowserPanel {
    /// Creates a new empty history browser.
    pub fn new(theme: &'static Theme, glyphs: &'static GlyphSet) -> Self {
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
            glyphs,
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
                    Ok(records) => {
                        self.records = merge_split_multiline_records(records);
                    }
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
    fn ensure_visible(&mut self, visible_count: usize, cmd_width: usize) {
        if self.records.is_empty() || visible_count == 0 {
            self.scroll_offset = 0;
            return;
        }

        let mut selected_start = 0usize;
        let mut selected_height = 1usize;
        let mut cumulative = 0usize;

        for (idx, record) in self.records.iter().enumerate() {
            let height = command_display_lines(&record.command, cmd_width).len();
            if idx == self.selection {
                selected_start = cumulative;
                selected_height = height.max(1);
                break;
            }
            cumulative += height.max(1);
        }

        let selected_end = selected_start + selected_height;

        if selected_start < self.scroll_offset {
            self.scroll_offset = selected_start;
        } else if selected_end > self.scroll_offset + visible_count {
            self.scroll_offset = selected_end.saturating_sub(visible_count);
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

        // Compute adaptive layout (returns None if area too small)
        let Some(layout) = compute_edit_mode_layout(area) else {
            return;
        };

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
        Paragraph::new(Line::from(title_spans)).render(layout.title, buffer);

        // Separator with original command hint
        let border_style = Style::default().fg(self.theme.panel_border);
        let orig_hint = format!(" Original: {} ", edit_state.original);
        let max_hint_width = (area.width as usize).saturating_sub(4);
        let truncated_hint: String =
            crate::ui::text_width::truncate_to_width(&orig_hint, max_hint_width).into_owned();
        let hint_chars: Vec<char> = truncated_hint.chars().collect();

        let mut hint_idx = 0;
        let mut col_offset: usize = 0;
        for x in layout.separator.x..layout.separator.x + layout.separator.width {
            if let Some(cell) = buffer.cell_mut((x, layout.separator.y)) {
                if col_offset > 0 {
                    col_offset -= 1;
                    continue;
                }

                if hint_idx < hint_chars.len() {
                    let ch = hint_chars[hint_idx];
                    let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(self.theme.text_secondary));
                    hint_idx += 1;
                    col_offset = ch_w.saturating_sub(1);
                } else {
                    cell.set_char(self.glyphs.border.horizontal);
                    cell.set_style(border_style);
                }
            }
        }

        // Render shared elements (token strip, suggestions, edit input, result preview)
        render_edit_mode_shared(buffer, self.theme, self.glyphs, edit_state, &layout);
    }

    /// Renders the danger confirmation dialog.
    fn render_danger_confirm(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        edit_state: &CommandEditState,
    ) {
        // Border and keybind hints are rendered externally by TabbedPanel's
        // footer compositor — footer_entries() returns confirm-mode entries.
        let chunks = Layout::vertical([
            Constraint::Length(1), // Warning header
            Constraint::Length(1), // Warning message
            Constraint::Length(1), // Command
            Constraint::Min(1),    // Spacer
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

    /// Renders a single visual line for a history record.
    fn render_row_line(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        record: &HistoryRecord,
        command_text: &str,
        cols: &ColumnWidths,
        is_selected: bool,
        show_metadata: bool,
    ) {
        let base_style = if is_selected {
            Style::default().bg(self.theme.selection_bg)
        } else {
            Style::default()
        };
        let dim = Style::default().fg(self.theme.text_secondary);
        // Keep multiline rendering stable: continuation lines never draw
        // column separators, regardless of selection state.
        let hide_separators = !show_metadata;

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
        {
            let mut col: u16 = 0;
            for ch in command_text.chars() {
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
            cell.set_char(if hide_separators { ' ' } else { '|' });
            cell.set_style(if is_selected {
                base_style.fg(self.theme.text_secondary)
            } else {
                dim
            });
        }
        x += 1;

        // When column
        let time_text = if show_metadata {
            self.format_relative_time(record)
        } else {
            String::new()
        };
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
            cell.set_char(if hide_separators { ' ' } else { '|' });
            cell.set_style(if is_selected {
                base_style.fg(self.theme.text_secondary)
            } else {
                dim
            });
        }
        x += 1;

        // Duration column
        let dur_text = if show_metadata {
            self.format_duration(record)
        } else {
            String::new()
        };
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
            cell.set_char(if hide_separators { ' ' } else { '|' });
            cell.set_style(if is_selected {
                base_style.fg(self.theme.text_secondary)
            } else {
                dim
            });
        }
        x += 1;

        // Count column (only in dedupe/frequency modes)
        if cols.count > 0 {
            let count_text = if show_metadata {
                format!("{:>4}", record.execution_count)
            } else {
                String::new()
            };
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
                cell.set_char(if hide_separators { ' ' } else { '|' });
                cell.set_style(if is_selected {
                    base_style.fg(self.theme.text_secondary)
                } else {
                    dim
                });
            }
            x += 1;
        }

        // Status column (last)
        let (status_text, status_color) = if show_metadata {
            self.format_exit_status(record)
        } else {
            ("", self.theme.text_secondary)
        };
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
        13
    }

    fn title(&self) -> &str {
        "History"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 6 || area.width < 20 {
            return;
        }

        // If in edit mode, render the edit UI instead
        if self.in_edit_mode() {
            self.render_edit_mode(buffer, area);
            return;
        }

        // Layout: filter (1) + header (1) + separator (1) + list (n)
        // Border + keybinds are rendered externally by TabbedPanel's footer compositor.
        let chunks = Layout::vertical([
            Constraint::Length(1), // Filter input
            Constraint::Length(1), // Table header
            Constraint::Length(1), // Separator line
            Constraint::Min(1),    // Table body
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
                cell.set_char(self.glyphs.border.horizontal);
                cell.set_style(sep_style);
            }
        }

        // Render table body
        let visible_height = chunks[3].height as usize;
        let cmd_width = cols.command.saturating_sub(1) as usize;
        self.ensure_visible(visible_height, cmd_width);

        let viewport_start = self.scroll_offset;
        let viewport_end = self.scroll_offset + visible_height;
        let mut virtual_line = 0usize;
        let mut y = chunks[3].y;
        let y_end = chunks[3].y + chunks[3].height;

        for (record_idx, record) in self.records.iter().enumerate() {
            let display_lines = command_display_lines(&record.command, cmd_width);
            let record_height = display_lines.len().max(1);
            let record_start = virtual_line;
            let record_end = record_start + record_height;
            virtual_line = record_end;

            if record_end <= viewport_start {
                continue;
            }
            if record_start >= viewport_end || y >= y_end {
                break;
            }

            let first_visible = viewport_start.saturating_sub(record_start);
            let is_selected = record_idx == self.selection;

            for (line_idx, command_line) in display_lines.iter().enumerate().skip(first_visible) {
                if y >= y_end {
                    break;
                }

                let row_area = Rect::new(chunks[3].x, y, chunks[3].width, 1);
                self.render_row_line(
                    buffer,
                    row_area,
                    record,
                    command_line,
                    &cols,
                    is_selected,
                    line_idx == 0,
                );
                y += 1;
            }
        }
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

    fn footer_entries(&self) -> Vec<FooterEntry> {
        if let Some(ref edit_state) = self.edit_mode {
            if edit_state.is_confirming() {
                return vec![
                    FooterEntry::action("Enter", "Confirm & Run"),
                    FooterEntry::action("Esc", "Cancel"),
                ];
            }
            return vec![
                FooterEntry::action("←→", "Nav"),
                FooterEntry::action("↑↓", "Cycle"),
                FooterEntry::action("^D", "Del"),
                FooterEntry::action("^A/I", "Ins"),
                FooterEntry::action("^Q", "Quote"),
                FooterEntry::action("Enter", "Run"),
                FooterEntry::action("Esc", "Back"),
            ];
        }
        vec![
            FooterEntry::action("^E", "Edit"),
            FooterEntry::toggle("^D", "Dedupe", self.filter_mode.dedupe),
            FooterEntry::toggle("^G", "CurDir", self.filter_mode.current_dir_only),
            FooterEntry::toggle("^X", "Failed", self.filter_mode.failed_only),
            FooterEntry::value("^S", self.sort_mode.name().to_string()),
            FooterEntry::action("Enter", "Run"),
            FooterEntry::action("Esc", "Close"),
        ]
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_glyph_tier(&mut self, tier: super::glyphs::GlyphTier) {
        self.glyphs = super::glyphs::GlyphSet::for_tier(tier);
    }

    fn theme(&self) -> &'static super::theme::Theme {
        self.theme
    }

    fn set_theme(&mut self, theme: &'static super::theme::Theme) {
        self.theme = theme;
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

    fn history_record_for_command(command: &str) -> HistoryRecord {
        HistoryRecord {
            command: command.to_string(),
            timestamp: None,
            cwd: None,
            exit_status: None,
            duration: None,
            frecency_score: 0.0,
            execution_count: 1,
        }
    }

    #[test]
    fn test_history_browser_new_constructs_empty_records() {
        let panel = HistoryBrowserPanel::new(
            &AMBER_THEME,
            crate::chrome::glyphs::GlyphSet::for_tier(crate::chrome::glyphs::GlyphTier::Unicode),
        );
        assert!(panel.records.is_empty());
        assert_eq!(panel.selection, 0);
    }

    #[test]
    fn test_filter_mode_default_flags_false() {
        let panel = HistoryBrowserPanel::new(
            &AMBER_THEME,
            crate::chrome::glyphs::GlyphSet::for_tier(crate::chrome::glyphs::GlyphTier::Unicode),
        );
        assert!(!panel.filter_mode.dedupe);
        assert!(!panel.filter_mode.current_dir_only);
        assert!(!panel.filter_mode.failed_only);
    }

    #[test]
    fn test_sort_mode_cycle_through_variants() {
        let mut panel = HistoryBrowserPanel::new(
            &AMBER_THEME,
            crate::chrome::glyphs::GlyphSet::for_tier(crate::chrome::glyphs::GlyphTier::Unicode),
        );
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

    #[test]
    fn test_parse_heredoc_spec_with_quoted_delimiter_returns_spec() {
        let spec =
            parse_heredoc_spec("sudo tee /tmp/file >/dev/null <<'EOF'").expect("heredoc spec");
        assert_eq!(spec.delimiter, "EOF");
        assert!(!spec.strip_tabs);
    }

    #[test]
    fn test_parse_heredoc_spec_with_dash_strip_tabs_returns_spec() {
        let spec = parse_heredoc_spec("cat <<-EOF").expect("heredoc spec");
        assert_eq!(spec.delimiter, "EOF");
        assert!(spec.strip_tabs);
    }

    #[test]
    fn test_analyze_command_continuation_with_trailing_backslash_reports_incomplete() {
        let state = analyze_command_continuation("echo hello \\");
        assert!(state.is_incomplete());
        assert!(state.trailing_backslash);
    }

    #[test]
    fn test_analyze_command_continuation_with_complete_single_line_reports_complete() {
        let state = analyze_command_continuation("echo hello");
        assert!(!state.is_incomplete());
    }

    #[test]
    fn test_merge_split_multiline_records_with_heredoc_rows_collapses_into_one_record() {
        let records = vec![
            history_record_for_command("sudo tee /tmp/demo <<'EOF'"),
            history_record_for_command("line 1"),
            history_record_for_command("line 2"),
            history_record_for_command("EOF"),
            history_record_for_command("echo done"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].command,
            "sudo tee /tmp/demo <<'EOF'\nline 1\nline 2\nEOF"
        );
        assert_eq!(merged[1].command, "echo done");
    }

    #[test]
    fn test_merge_split_multiline_records_without_delimiter_keeps_original_rows() {
        let records = vec![
            history_record_for_command("cat <<EOF"),
            history_record_for_command("line 1"),
            history_record_for_command("echo done"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].command, "cat <<EOF");
        assert_eq!(merged[1].command, "line 1");
        assert_eq!(merged[2].command, "echo done");
    }

    #[test]
    fn test_merge_split_multiline_records_with_backslash_continuation_collapses_rows() {
        let records = vec![
            history_record_for_command("echo first \\"),
            history_record_for_command("second"),
            history_record_for_command("echo done"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].command, "echo first \\\nsecond");
        assert_eq!(merged[1].command, "echo done");
    }

    #[test]
    fn test_merge_split_multiline_records_with_trailing_pipe_collapses_rows() {
        let records = vec![
            history_record_for_command("cat /etc/passwd |"),
            history_record_for_command("grep root"),
            history_record_for_command("echo done"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].command, "cat /etc/passwd |\ngrep root");
        assert_eq!(merged[1].command, "echo done");
    }

    #[test]
    fn test_merge_split_multiline_records_with_unclosed_quote_collapses_rows() {
        let records = vec![
            history_record_for_command("echo \"hello"),
            history_record_for_command("world\""),
            history_record_for_command("echo done"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].command, "echo \"hello\nworld\"");
        assert_eq!(merged[1].command, "echo done");
    }

    #[test]
    fn test_merge_split_multiline_records_with_if_block_collapses_rows() {
        let records = vec![
            history_record_for_command("if [ -f /tmp/demo ]; then"),
            history_record_for_command("echo yes"),
            history_record_for_command("fi"),
            history_record_for_command("echo done"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].command, "if [ -f /tmp/demo ]; then\necho yes\nfi");
        assert_eq!(merged[1].command, "echo done");
    }

    #[test]
    fn test_merge_split_multiline_records_with_for_do_done_collapses_rows() {
        let records = vec![
            history_record_for_command("for x in a b"),
            history_record_for_command("do"),
            history_record_for_command("echo $x"),
            history_record_for_command("done"),
            history_record_for_command("echo done"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].command, "for x in a b\ndo\necho $x\ndone");
        assert_eq!(merged[1].command, "echo done");
    }

    #[test]
    fn test_merge_split_multiline_records_with_unclosed_quote_without_terminator_keeps_rows() {
        let records = vec![
            history_record_for_command("echo \"hello"),
            history_record_for_command("next-command"),
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].command, "echo \"hello");
        assert_eq!(merged[1].command, "next-command");
    }

    #[test]
    fn test_merge_split_multiline_records_with_frequency_like_metadata_normalizes_fields() {
        let t0 = chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("timestamp t0");
        let t1 = chrono::DateTime::from_timestamp(1_700_000_100, 0).expect("timestamp t1");
        let records = vec![
            HistoryRecord {
                command: "cat <<'EOF'".to_string(),
                timestamp: Some(t0),
                cwd: None,
                exit_status: None,
                duration: None,
                frecency_score: 42.0,
                execution_count: 9,
            },
            HistoryRecord {
                command: "line 1".to_string(),
                timestamp: Some(t1),
                cwd: Some(PathBuf::from("/tmp")),
                exit_status: Some(0),
                duration: Some(std::time::Duration::from_millis(1200)),
                frecency_score: 3.0,
                execution_count: 2,
            },
            HistoryRecord {
                command: "EOF".to_string(),
                timestamp: Some(t0),
                cwd: None,
                exit_status: None,
                duration: None,
                frecency_score: 7.0,
                execution_count: 5,
            },
        ];

        let merged = merge_split_multiline_records(records);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].command, "cat <<'EOF'\nline 1\nEOF");
        assert_eq!(merged[0].timestamp, Some(t1));
        assert_eq!(merged[0].cwd.as_deref(), Some(std::path::Path::new("/tmp")));
        assert_eq!(merged[0].exit_status, Some(0));
        assert_eq!(
            merged[0].duration,
            Some(std::time::Duration::from_millis(1200))
        );
        assert_eq!(merged[0].execution_count, 2);
        assert_eq!(merged[0].frecency_score, 3.0);
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
        let mut panel = HistoryBrowserPanel::new(
            &AMBER_THEME,
            crate::chrome::glyphs::GlyphSet::for_tier(crate::chrome::glyphs::GlyphTier::Unicode),
        );
        panel.edit_mode = Some(CommandEditState::from_command("echo 你好"));

        let area = Rect::new(0, 0, 60, 12);
        let mut buffer = Buffer::empty(area);
        panel.render(&mut buffer, area);

        let ansi = buffer_to_ansi(&buffer, area);
        let visible = crate::chrome::test_utils::strip_ansi_for_test(&ansi);
        assert!(
            visible.contains("Original: echo 你好"),
            "expected wide chars in original hint, got: {visible:?}"
        );
    }
}
