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

use super::command_knowledge::COMMAND_KNOWLEDGE;
use super::panel::{Panel, PanelResult};
use crate::history_store::{FilterMode, HistoryRecord, HistoryStore, SortMode};

/// Token type for semantic classification.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TokenType {
    Command,    // First token (ls, git, etc.)
    Subcommand, // Second token for compound commands (checkout, push)
    Flag,       // Starts with - or --
    Path,       // Contains / or starts with . or ~
    Url,        // Contains :// or looks like git@...
    Argument,   // Generic argument
}

/// Quote style for tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
enum QuoteStyle {
    None,
    Single, // 'text'
    Double, // "text"
}

/// A token from a shell command (word, flag, or quoted string).
#[derive(Debug, Clone)]
struct CommandToken {
    /// The text content of this token.
    text: String,
    /// Semantic type of this token.
    token_type: TokenType,
}

/// Returns a superscript digit for display (¹²³...²⁰).
fn superscript_digit(n: usize) -> &'static str {
    const SUPERSCRIPTS: [&str; 20] = [
        "¹", "²", "³", "⁴", "⁵", "⁶", "⁷", "⁸", "⁹", "¹⁰",
        "¹¹", "¹²", "¹³", "¹⁴", "¹⁵", "¹⁶", "¹⁷", "¹⁸", "¹⁹", "²⁰",
    ];
    if n >= 1 && n <= 20 {
        SUPERSCRIPTS[n - 1]
    } else {
        "·" // Fallback for >20 tokens
    }
}

/// Classifies a token based on its content and position.
fn classify_token(text: &str, position: usize, prev_token: Option<&str>) -> TokenType {
    if position == 0 {
        return TokenType::Command;
    }
    if text.starts_with('-') {
        return TokenType::Flag;
    }
    if text.contains("://") || text.starts_with("git@") {
        return TokenType::Url;
    }
    if text.contains('/') || text.starts_with('.') || text.starts_with('~') {
        return TokenType::Path;
    }
    // Check for subcommand (second token after known compound commands)
    if position == 1 {
        if let Some(cmd) = prev_token {
            if matches!(cmd, "git" | "docker" | "kubectl" | "cargo" | "npm" | "yarn" | "systemctl" | "journalctl") {
                return TokenType::Subcommand;
            }
        }
    }
    TokenType::Argument
}

/// Returns the style for a token based on its type.
fn token_type_style(token_type: TokenType) -> Style {
    match token_type {
        TokenType::Command => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        TokenType::Subcommand => Style::default().fg(Color::Cyan),
        TokenType::Flag => Style::default().fg(Color::Yellow),
        TokenType::Path => Style::default().fg(Color::Blue),
        TokenType::Url => Style::default().fg(Color::Magenta),
        TokenType::Argument => Style::default().fg(Color::White),
    }
}

/// Parses quote style from a token.
fn parse_quotes(text: &str) -> (String, QuoteStyle) {
    if text.starts_with('\'') && text.ends_with('\'') && text.len() >= 2 {
        (text[1..text.len() - 1].to_string(), QuoteStyle::Single)
    } else if text.starts_with('"') && text.ends_with('"') && text.len() >= 2 {
        (text[1..text.len() - 1].to_string(), QuoteStyle::Double)
    } else {
        (text.to_string(), QuoteStyle::None)
    }
}

/// Applies a quote style to text.
fn apply_quotes(text: &str, style: QuoteStyle) -> String {
    match style {
        QuoteStyle::None => text.to_string(),
        QuoteStyle::Single => format!("'{}'", text),
        QuoteStyle::Double => format!("\"{}\"", text),
    }
}

/// Checks if a command is potentially dangerous.
fn is_dangerous_command(command: &str) -> Option<&'static str> {
    let lower = command.to_lowercase();

    if lower.contains("rm -rf") || lower.contains("rm -fr") {
        return Some("Recursive force delete");
    }
    if lower.contains("dd if=") && lower.contains("of=/dev/") {
        return Some("Direct disk write");
    }
    if lower.contains("mkfs") {
        return Some("Filesystem format");
    }
    if lower.contains("> /dev/sd") || lower.contains(">/dev/sd") {
        return Some("Direct device write");
    }
    if lower.contains("chmod -r 777") || lower.contains("chmod 777 -r") {
        return Some("Overly permissive chmod");
    }
    if lower.contains(":(){ :|:& };:") {
        return Some("Fork bomb");
    }

    None
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
    /// Undo stack for reverting changes.
    undo_stack: Vec<Vec<CommandToken>>,
    /// Pending command awaiting confirmation (for dangerous commands).
    pending_confirm: Option<String>,
    /// Warning message for dangerous command.
    danger_warning: Option<&'static str>,
    /// Skip dangerous command checks (toggled with Ctrl+!)
    skip_danger_check: bool,
    /// Current suggestions for the selected token position.
    current_suggestions: Vec<String>,
    /// Index into current_suggestions (None = using custom/typed value).
    suggestion_index: Option<usize>,
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
            undo_stack: Vec::new(),
            pending_confirm: None,
            danger_warning: None,
            skip_danger_check: false,
            current_suggestions: Vec::new(),
            suggestion_index: None,
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
            // Reclassify after saving (text change may affect token types)
            self.reclassify_tokens();
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

    /// Saves current state to undo stack.
    fn save_undo(&mut self) {
        self.undo_stack.push(self.tokens.clone());
        // Limit stack size
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
    }

    /// Restores previous state from undo stack.
    fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.tokens = prev;
            self.selected = self.selected.min(self.tokens.len().saturating_sub(1));
            self.edit_buffer = self.tokens.get(self.selected)
                .map(|t| t.text.clone())
                .unwrap_or_default();
            self.reclassify_tokens();
        }
    }

    /// Deletes the currently selected token.
    fn delete_token(&mut self) {
        if self.tokens.len() <= 1 {
            return; // Don't delete last token
        }
        self.save_undo();
        self.tokens.remove(self.selected);
        if self.selected >= self.tokens.len() {
            self.selected = self.tokens.len() - 1;
        }
        self.edit_buffer = self.tokens[self.selected].text.clone();
        self.reclassify_tokens();
    }

    /// Inserts a new token after the current one.
    fn insert_token_after(&mut self) {
        self.save_undo();
        // Save current edit first
        if let Some(token) = self.tokens.get_mut(self.selected) {
            token.text = self.edit_buffer.clone();
        }
        let new_token = CommandToken {
            text: String::new(),
            token_type: TokenType::Argument,
        };
        self.tokens.insert(self.selected + 1, new_token);
        self.selected += 1;
        self.edit_buffer.clear();
        self.editing = true;
        self.reclassify_tokens();
    }

    /// Inserts a new token before the current one.
    fn insert_token_before(&mut self) {
        self.save_undo();
        // Save current edit first
        if let Some(token) = self.tokens.get_mut(self.selected) {
            token.text = self.edit_buffer.clone();
        }
        let new_token = CommandToken {
            text: String::new(),
            token_type: TokenType::Argument,
        };
        self.tokens.insert(self.selected, new_token);
        self.edit_buffer.clear();
        self.editing = true;
        self.reclassify_tokens();
    }

    /// Cycles through quote styles for current token.
    fn cycle_quote(&mut self) {
        let (inner, current_style) = parse_quotes(&self.edit_buffer);

        let new_style = match current_style {
            QuoteStyle::None => QuoteStyle::Single,
            QuoteStyle::Single => QuoteStyle::Double,
            QuoteStyle::Double => QuoteStyle::None,
        };

        self.edit_buffer = apply_quotes(&inner, new_style);
    }

    /// Clears any pending confirmation state.
    fn clear_confirm(&mut self) {
        self.pending_confirm = None;
        self.danger_warning = None;
    }

    /// Reclassifies all tokens based on their current text and positions.
    ///
    /// This should be called after any mutation that changes token positions
    /// (delete, insert) or token text (editing), to ensure token_type stays
    /// accurate for UI hints and styling.
    fn reclassify_tokens(&mut self) {
        for i in 0..self.tokens.len() {
            let prev_text = if i > 0 {
                Some(self.tokens[i - 1].text.as_str())
            } else {
                None
            };
            self.tokens[i].token_type = classify_token(&self.tokens[i].text, i, prev_text);
        }
    }

    /// Returns true if waiting for confirmation.
    fn is_confirming(&self) -> bool {
        self.pending_confirm.is_some()
    }

    /// Returns true if there are any unsaved changes.
    fn has_changes(&self) -> bool {
        // Build current command to compare
        let current: String = self.tokens.iter().enumerate().map(|(i, t)| {
            if i == self.selected {
                self.edit_buffer.clone()
            } else {
                t.text.clone()
            }
        }).collect::<Vec<_>>().join(" ");
        current != self.original
    }

    /// Reverts all changes back to the original command.
    fn revert(&mut self) {
        self.tokens = tokenize_command(&self.original);
        self.selected = 0;
        self.edit_buffer = self.tokens.first().map(|t| t.text.clone()).unwrap_or_default();
        self.undo_stack.clear();
    }

    /// Updates suggestions for the currently selected token position.
    fn update_suggestions(&mut self, history_store: Option<&HistoryStore>) {
        // Get preceding tokens
        let preceding: Vec<&str> = self.tokens[..self.selected]
            .iter()
            .map(|t| t.text.as_str())
            .collect();

        // Get static suggestions from command knowledge
        let static_suggestions: Vec<String> = COMMAND_KNOWLEDGE
            .suggestions_for_position(&preceding)
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Get history-based suggestions
        let mut history_suggestions = Vec::new();
        if let Some(store) = history_store {
            if let Ok(suggestions) = store.tokens_at_position(&preceding, 20) {
                history_suggestions = suggestions;
            }
        }

        // Merge suggestions: history first (more relevant), then static
        let mut merged = Vec::new();
        for hist_sugg in &history_suggestions {
            if !merged.contains(hist_sugg) {
                merged.push(hist_sugg.clone());
            }
        }
        for static_sugg in &static_suggestions {
            if !merged.contains(static_sugg) {
                merged.push(static_sugg.clone());
            }
        }

        self.current_suggestions = merged;
        self.suggestion_index = None;
    }

    /// Cycles through suggestions in the given direction.
    ///
    /// # Arguments
    ///
    /// * `direction` - Positive for forward (down), negative for backward (up)
    fn cycle_suggestion(&mut self, direction: i32) {
        if self.current_suggestions.is_empty() {
            return;
        }

        let new_index = match self.suggestion_index {
            None => {
                // First press: enter suggestion mode
                if direction > 0 {
                    0
                } else {
                    self.current_suggestions.len() - 1
                }
            }
            Some(idx) => {
                // Cycle with wrapping
                let len = self.current_suggestions.len();
                if direction > 0 {
                    (idx + 1) % len
                } else {
                    (idx + len - 1) % len
                }
            }
        };

        self.suggestion_index = Some(new_index);
        self.edit_buffer = self.current_suggestions[new_index].clone();
    }

    /// Returns the previous suggestion (for three-row display).
    fn prev_suggestion(&self) -> Option<&str> {
        if self.current_suggestions.is_empty() {
            return None;
        }
        let idx = self.suggestion_index.unwrap_or(0);
        let len = self.current_suggestions.len();
        let prev_idx = if idx == 0 { len - 1 } else { idx - 1 };
        self.current_suggestions.get(prev_idx).map(|s| s.as_str())
    }

    /// Returns the next suggestion (for three-row display).
    fn next_suggestion(&self) -> Option<&str> {
        if self.current_suggestions.is_empty() {
            return None;
        }
        let idx = self.suggestion_index.unwrap_or(0);
        let len = self.current_suggestions.len();
        let next_idx = (idx + 1) % len;
        self.current_suggestions.get(next_idx).map(|s| s.as_str())
    }
}

/// Tokenizes a shell command into words, respecting quotes.
fn tokenize_command(command: &str) -> Vec<CommandToken> {
    let mut raw_tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;

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
                    raw_tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }

    // Don't forget the last token
    if !current.is_empty() {
        raw_tokens.push(current);
    }

    // Now classify each token
    let mut tokens = Vec::new();
    for (i, text) in raw_tokens.iter().enumerate() {
        let prev = if i > 0 { Some(raw_tokens[i - 1].as_str()) } else { None };
        let token_type = classify_token(text, i, prev);
        tokens.push(CommandToken {
            text: text.clone(),
            token_type,
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
            let mut edit_state = EditModeState::new(cmd);

            // Initialize suggestions with history store
            let history_store = self.history_store.as_ref().and_then(|s| s.lock().ok());
            edit_state.update_suggestions(history_store.as_deref());
            drop(history_store);

            self.edit_mode = Some(edit_state);
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

    /// Renders the edit mode UI with three-row depth display.
    fn render_edit_mode(&self, buffer: &mut Buffer, area: Rect) {
        let Some(edit_state) = &self.edit_mode else { return };

        // Check if showing danger confirmation
        if edit_state.is_confirming() {
            self.render_danger_confirm(buffer, area, edit_state);
            return;
        }

        // Layout with three-row depth UI - 12 rows total
        let chunks = Layout::vertical([
            Constraint::Length(1), // Title
            Constraint::Length(1), // Original command (dim)
            Constraint::Length(1), // Previous suggestion row (dim)
            Constraint::Length(1), // Current token strip (highlighted)
            Constraint::Length(1), // Next suggestion row (dim)
            Constraint::Length(1), // Edit input line
            Constraint::Length(1), // Result preview
            Constraint::Min(1),    // Flexible spacer
            Constraint::Length(1), // Border
            Constraint::Length(1), // Keybind hints
        ])
        .split(area);

        // Title with optional unsafe mode indicator and suggestion count
        let mut title_spans = vec![
            Span::styled(" Edit Command", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ];
        if !edit_state.current_suggestions.is_empty() {
            let sugg_count = format!(" [{} suggestions]", edit_state.current_suggestions.len());
            title_spans.push(Span::styled(sugg_count, Style::default().fg(Color::DarkGray)));
        }
        if edit_state.skip_danger_check {
            title_spans.push(Span::styled(" [UNSAFE]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
        }
        let title = Line::from(title_spans);
        Paragraph::new(title).render(chunks[0], buffer);

        // Original command (dimmed reference)
        let original_line = Line::from(vec![
            Span::styled(" Original: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&edit_state.original, Style::default().fg(Color::DarkGray)),
        ]);
        Paragraph::new(original_line).render(chunks[1], buffer);

        // Calculate the x-position where the selected token starts
        // We'll align prev/next suggestions under the selected token
        let mut selected_x_offset: usize = 3; // Initial padding
        for (i, token) in edit_state.tokens.iter().enumerate() {
            if i == edit_state.selected {
                break;
            }
            // Add superscript + bracket + token + bracket + spacing
            selected_x_offset += 1 + 1 + token.text.len() + 1 + 3;
        }
        // Add superscript + opening bracket for selected token
        selected_x_offset += 1 + 1;

        // Previous suggestion row (dim, aligned under selected token)
        if let Some(prev_sugg) = edit_state.prev_suggestion() {
            let padding = " ".repeat(selected_x_offset);
            let prev_line = Line::from(vec![
                Span::styled(padding, Style::default()),
                Span::styled(prev_sugg, Style::default().fg(Color::DarkGray)),
            ]);
            Paragraph::new(prev_line).render(chunks[2], buffer);
        }

        // Current token strip with double brackets and superscript numbers
        let mut spans = Vec::new();
        spans.push(Span::styled("   ", Style::default()));

        let bracket_style = Style::default().fg(Color::DarkGray);
        let bracket_selected_style = Style::default().fg(Color::Cyan);

        for (i, token) in edit_state.tokens.iter().enumerate() {
            let is_selected = i == edit_state.selected;
            let slot_num = i + 1;

            // Superscript number
            let num_style = if is_selected {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(superscript_digit(slot_num), num_style));

            // Opening bracket
            let bstyle = if is_selected { bracket_selected_style } else { bracket_style };
            spans.push(Span::styled("⟦", bstyle));

            // Token text with type-aware styling
            let base_style = token_type_style(token.token_type);
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
        Paragraph::new(token_line).render(chunks[3], buffer);

        // Next suggestion row (dim, aligned under selected token)
        if let Some(next_sugg) = edit_state.next_suggestion() {
            let padding = " ".repeat(selected_x_offset);
            let next_line = Line::from(vec![
                Span::styled(padding, Style::default()),
                Span::styled(next_sugg, Style::default().fg(Color::DarkGray)),
            ]);
            Paragraph::new(next_line).render(chunks[4], buffer);
        }

        // Edit input line with type hint and cycling indicator
        let type_hint = match edit_state.tokens.get(edit_state.selected).map(|t| t.token_type) {
            Some(TokenType::Command) => "cmd",
            Some(TokenType::Subcommand) => "sub",
            Some(TokenType::Flag) => "flag",
            Some(TokenType::Path) => "path",
            Some(TokenType::Url) => "url",
            Some(TokenType::Argument) | None => "arg",
        };
        let cycling_indicator = if edit_state.suggestion_index.is_some() {
            format!(" [{}/{}]",
                edit_state.suggestion_index.unwrap_or(0) + 1,
                edit_state.current_suggestions.len())
        } else {
            String::new()
        };
        let edit_label = format!("   {} {} > ", superscript_digit(edit_state.selected + 1), type_hint);
        let edit_line = Line::from(vec![
            Span::styled(edit_label, Style::default().fg(Color::Magenta)),
            Span::styled(&edit_state.edit_buffer, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::styled("█", Style::default().fg(Color::Cyan)),
            Span::styled(cycling_indicator, Style::default().fg(Color::DarkGray)),
        ]);
        Paragraph::new(edit_line).render(chunks[5], buffer);

        // Build and show result preview
        let result_preview: String = edit_state.tokens.iter().enumerate().map(|(i, t)| {
            if i == edit_state.selected {
                edit_state.edit_buffer.clone()
            } else {
                t.text.clone()
            }
        }).collect::<Vec<_>>().join(" ");

        let preview_changed = result_preview != edit_state.original;
        let preview_style = if preview_changed {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::White)
        };
        let preview_line = Line::from(vec![
            Span::styled("  Result: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&result_preview, preview_style),
        ]);
        Paragraph::new(preview_line).render(chunks[6], buffer);

        // Border
        let border_style = Style::default().fg(Color::DarkGray);
        for x in chunks[8].x..chunks[8].x + chunks[8].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[8].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Keybind hints - updated to show Up/Down for cycling
        let key_style = Style::default().fg(Color::Yellow);
        let label_style = Style::default().fg(Color::DarkGray);

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
        Paragraph::new(hints).render(chunks[9], buffer);
    }

    /// Renders the danger confirmation dialog.
    fn render_danger_confirm(&self, buffer: &mut Buffer, area: Rect, edit_state: &EditModeState) {
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
        let header = Line::from(vec![
            Span::styled(" ⚠ WARNING ", Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]);
        Paragraph::new(header).render(chunks[0], buffer);

        // Warning message
        let warning_msg = edit_state.danger_warning.unwrap_or("Potentially dangerous command");
        let warning_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(warning_msg, Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        ]);
        Paragraph::new(warning_line).render(chunks[1], buffer);

        // Command
        let cmd = edit_state.pending_confirm.as_deref().unwrap_or("");
        let cmd_line = Line::from(vec![
            Span::styled(" Command: ", Style::default().fg(Color::DarkGray)),
            Span::styled(cmd, Style::default().fg(Color::White)),
        ]);
        Paragraph::new(cmd_line).render(chunks[2], buffer);

        // Border
        let border_style = Style::default().fg(Color::DarkGray);
        for x in chunks[4].x..chunks[4].x + chunks[4].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[4].y)) {
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        // Keybind hints
        let key_style = Style::default().fg(Color::Yellow);
        let label_style = Style::default().fg(Color::DarkGray);
        let hints = Line::from(vec![
            Span::styled("Enter", key_style),
            Span::styled(" Confirm & Run", label_style),
            Span::raw("  "),
            Span::styled("Esc", key_style),
            Span::styled(" Cancel", label_style),
        ]);
        Paragraph::new(hints).render(chunks[5], buffer);
    }

    /// Handles input in edit mode. Returns Some(PanelResult) if handled.
    fn handle_edit_input(&mut self, key: KeyEvent) -> Option<PanelResult> {
        let edit_state = self.edit_mode.as_mut()?;

        // Handle danger confirmation mode
        if edit_state.is_confirming() {
            return match key.code {
                KeyCode::Enter => {
                    // User confirmed - execute the dangerous command
                    let command = edit_state.pending_confirm.take().unwrap_or_default();
                    self.exit_edit_mode();
                    Some(PanelResult::Execute(command))
                }
                KeyCode::Esc => {
                    // Cancel confirmation, go back to editing
                    edit_state.clear_confirm();
                    Some(PanelResult::Continue)
                }
                _ => Some(PanelResult::Continue),
            };
        }

        // Handle Ctrl+key commands (don't interfere with text editing)
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('u') => {
                    edit_state.undo();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('d') => {
                    edit_state.delete_token();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('a') => {
                    edit_state.insert_token_after();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('i') => {
                    edit_state.insert_token_before();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('q') => {
                    edit_state.cycle_quote();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('!') => {
                    edit_state.skip_danger_check = !edit_state.skip_danger_check;
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
                    // Stage 1: Revert current token to its saved value
                    edit_state.edit_buffer = token_text.to_string();
                    Some(PanelResult::Continue)
                } else if edit_state.has_changes() {
                    // Stage 2: Revert entire command to original
                    edit_state.revert();
                    Some(PanelResult::Continue)
                } else {
                    // Stage 3: Exit edit mode
                    self.exit_edit_mode();
                    Some(PanelResult::Continue)
                }
            }
            KeyCode::Enter => {
                // Build the edited command
                let command = edit_state.build_command();

                // Check for dangerous patterns (unless bypassed)
                if !edit_state.skip_danger_check {
                    if let Some(warning) = is_dangerous_command(&command) {
                        edit_state.pending_confirm = Some(command);
                        edit_state.danger_warning = Some(warning);
                        return Some(PanelResult::Continue);
                    }
                }
                self.exit_edit_mode();
                Some(PanelResult::Execute(command))
            }
            KeyCode::Left => {
                edit_state.prev();
                // Update suggestions for new token position
                let history_store = self.history_store.as_ref().and_then(|s| s.lock().ok());
                if let Some(ref mut state) = self.edit_mode {
                    state.update_suggestions(history_store.as_deref());
                }
                Some(PanelResult::Continue)
            }
            KeyCode::Right => {
                edit_state.next();
                // Update suggestions for new token position
                let history_store = self.history_store.as_ref().and_then(|s| s.lock().ok());
                if let Some(ref mut state) = self.edit_mode {
                    state.update_suggestions(history_store.as_deref());
                }
                Some(PanelResult::Continue)
            }
            KeyCode::Home => {
                edit_state.select(0);
                // Update suggestions for new token position
                let history_store = self.history_store.as_ref().and_then(|s| s.lock().ok());
                if let Some(ref mut state) = self.edit_mode {
                    state.update_suggestions(history_store.as_deref());
                }
                Some(PanelResult::Continue)
            }
            KeyCode::End => {
                let last = edit_state.token_count().saturating_sub(1);
                edit_state.select(last);
                // Update suggestions for new token position
                let history_store = self.history_store.as_ref().and_then(|s| s.lock().ok());
                if let Some(ref mut state) = self.edit_mode {
                    state.update_suggestions(history_store.as_deref());
                }
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
                // Update suggestions for new token position
                let history_store = self.history_store.as_ref().and_then(|s| s.lock().ok());
                if let Some(ref mut state) = self.edit_mode {
                    state.update_suggestions(history_store.as_deref());
                }
                Some(PanelResult::Continue)
            }
            KeyCode::BackTab => {
                edit_state.prev();
                // Update suggestions for new token position
                let history_store = self.history_store.as_ref().and_then(|s| s.lock().ok());
                if let Some(ref mut state) = self.edit_mode {
                    state.update_suggestions(history_store.as_deref());
                }
                Some(PanelResult::Continue)
            }
            KeyCode::Char(c) => {
                edit_state.edit_buffer.push(c);
                edit_state.editing = true;
                Some(PanelResult::Continue)
            }
            KeyCode::Backspace => {
                edit_state.edit_buffer.pop();
                // If buffer becomes empty, reset editing state
                if edit_state.edit_buffer.is_empty() {
                    edit_state.editing = false;
                }
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
        // Use char-based truncation to avoid panicking on UTF-8 boundaries
        let cmd_display = if record.command.chars().count() > cmd_width {
            let truncated: String = record.command
                .chars()
                .take(cmd_width.saturating_sub(3))
                .collect();
            format!("{}...", truncated)
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
        // Enhanced edit mode needs 12 rows, list mode needs 15
        if self.edit_mode.is_some() {
            12
        } else {
            15
        }
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
        let mut filter_spans = vec![
            Span::styled(" > ", Style::default().fg(Color::Magenta)),
            filter_text,
            Span::styled(
                format!("  [{} entries]", self.records.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        // Show current directory when filtering by it
        if self.filter_mode.current_dir_only {
            if let Some(ref cwd) = self.current_cwd {
                let path_str = cwd.to_string_lossy();
                filter_spans.push(Span::styled(
                    format!("  in {}", path_str),
                    Style::default().fg(Color::Cyan),
                ));
            }
        }
        Paragraph::new(Line::from(filter_spans)).render(chunks[0], buffer);

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
            KeyCode::Tab => {
                // Insert command into buffer without executing
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
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].text, "-la");
        assert_eq!(tokens[1].token_type, TokenType::Flag);
        assert_eq!(tokens[2].text, "/tmp");
        assert_eq!(tokens[2].token_type, TokenType::Path);
    }

    #[test]
    fn test_tokenize_quoted_string() {
        let tokens = tokenize_command("echo \"hello world\"");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "echo");
        assert_eq!(tokens[1].text, "\"hello world\"");
        assert_eq!(tokens[1].token_type, TokenType::Argument);
    }

    #[test]
    fn test_tokenize_single_quoted() {
        let tokens = tokenize_command("grep 'pattern with spaces' file.txt");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].text, "'pattern with spaces'");
        assert_eq!(tokens[1].token_type, TokenType::Argument);
    }

    #[test]
    fn test_tokenize_git_command() {
        let tokens = tokenize_command("git checkout -b feature/new-branch origin/main");
        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[0].text, "git");
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].text, "checkout");
        assert_eq!(tokens[1].token_type, TokenType::Subcommand);
        assert_eq!(tokens[2].text, "-b");
        assert_eq!(tokens[2].token_type, TokenType::Flag);
        assert_eq!(tokens[3].text, "feature/new-branch");
        assert_eq!(tokens[3].token_type, TokenType::Path);
        assert_eq!(tokens[4].text, "origin/main");
        assert_eq!(tokens[4].token_type, TokenType::Path);
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize_command("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokenize_url() {
        let tokens = tokenize_command("git remote add origin git@github.com:user/repo.git");
        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[4].text, "git@github.com:user/repo.git");
        assert_eq!(tokens[4].token_type, TokenType::Url);
    }

    // =========================================================================
    // Token Type Tests
    // =========================================================================

    #[test]
    fn test_classify_command() {
        assert_eq!(classify_token("ls", 0, None), TokenType::Command);
        assert_eq!(classify_token("git", 0, None), TokenType::Command);
    }

    #[test]
    fn test_classify_subcommand() {
        assert_eq!(classify_token("checkout", 1, Some("git")), TokenType::Subcommand);
        assert_eq!(classify_token("build", 1, Some("cargo")), TokenType::Subcommand);
        // Not a known compound command
        assert_eq!(classify_token("something", 1, Some("echo")), TokenType::Argument);
    }

    #[test]
    fn test_classify_flag() {
        assert_eq!(classify_token("-la", 1, Some("ls")), TokenType::Flag);
        assert_eq!(classify_token("--help", 2, Some("checkout")), TokenType::Flag);
    }

    #[test]
    fn test_classify_path() {
        assert_eq!(classify_token("/tmp", 1, Some("ls")), TokenType::Path);
        assert_eq!(classify_token("./file.txt", 2, Some("-la")), TokenType::Path);
        assert_eq!(classify_token("~/Documents", 1, Some("cd")), TokenType::Path);
    }

    #[test]
    fn test_classify_url() {
        assert_eq!(classify_token("https://example.com", 1, Some("curl")), TokenType::Url);
        assert_eq!(classify_token("git@github.com:user/repo.git", 4, Some("origin")), TokenType::Url);
    }

    // =========================================================================
    // Quote Handling Tests
    // =========================================================================

    #[test]
    fn test_parse_quotes_none() {
        let (inner, style) = parse_quotes("hello");
        assert_eq!(inner, "hello");
        assert_eq!(style, QuoteStyle::None);
    }

    #[test]
    fn test_parse_quotes_single() {
        let (inner, style) = parse_quotes("'hello world'");
        assert_eq!(inner, "hello world");
        assert_eq!(style, QuoteStyle::Single);
    }

    #[test]
    fn test_parse_quotes_double() {
        let (inner, style) = parse_quotes("\"hello world\"");
        assert_eq!(inner, "hello world");
        assert_eq!(style, QuoteStyle::Double);
    }

    #[test]
    fn test_apply_quotes() {
        assert_eq!(apply_quotes("hello", QuoteStyle::None), "hello");
        assert_eq!(apply_quotes("hello", QuoteStyle::Single), "'hello'");
        assert_eq!(apply_quotes("hello", QuoteStyle::Double), "\"hello\"");
    }

    // =========================================================================
    // Dangerous Command Tests
    // =========================================================================

    #[test]
    fn test_dangerous_rm_rf() {
        assert!(is_dangerous_command("rm -rf /").is_some());
        assert!(is_dangerous_command("sudo rm -rf /tmp/build").is_some());
        assert!(is_dangerous_command("rm -fr ~/").is_some());
    }

    #[test]
    fn test_dangerous_dd() {
        assert!(is_dangerous_command("dd if=/dev/zero of=/dev/sda").is_some());
    }

    #[test]
    fn test_safe_commands() {
        assert!(is_dangerous_command("ls -la").is_none());
        assert!(is_dangerous_command("git push origin main").is_none());
        assert!(is_dangerous_command("rm file.txt").is_none()); // No -rf
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
        assert!(state.undo_stack.is_empty());
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

    #[test]
    fn test_edit_mode_delete_token() {
        let mut state = EditModeState::new("git push origin main");
        assert_eq!(state.token_count(), 4);

        state.select(2); // Select "origin"
        state.delete_token();

        assert_eq!(state.token_count(), 3);
        assert_eq!(state.build_command(), "git push main");
    }

    #[test]
    fn test_edit_mode_insert_token() {
        let mut state = EditModeState::new("git push");
        assert_eq!(state.token_count(), 2);

        state.select(1); // Select "push"
        state.insert_token_after();
        state.edit_buffer = "origin".to_string();

        assert_eq!(state.token_count(), 3);
        assert_eq!(state.build_command(), "git push origin");
    }

    #[test]
    fn test_edit_mode_undo() {
        let mut state = EditModeState::new("git push origin main");
        assert_eq!(state.token_count(), 4);

        state.select(2);
        state.delete_token();
        assert_eq!(state.token_count(), 3);

        state.undo();
        assert_eq!(state.token_count(), 4);
        assert_eq!(state.build_command(), "git push origin main");
    }

    #[test]
    fn test_edit_mode_quote_cycling() {
        let mut state = EditModeState::new("echo hello");
        state.select(1);

        // Start with no quotes
        assert_eq!(state.edit_buffer, "hello");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "'hello'");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "\"hello\"");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "hello");
    }

    #[test]
    fn test_edit_mode_danger_confirm() {
        let mut state = EditModeState::new("rm -rf /");
        assert!(!state.is_confirming());

        state.pending_confirm = Some("rm -rf /".to_string());
        state.danger_warning = Some("Test warning");

        assert!(state.is_confirming());

        state.clear_confirm();
        assert!(!state.is_confirming());
    }

    #[test]
    fn test_edit_mode_has_changes() {
        let mut state = EditModeState::new("echo hello");
        assert!(!state.has_changes()); // No changes yet

        state.select(1);
        state.edit_buffer = "world".to_string();
        assert!(state.has_changes()); // Now has changes
    }

    #[test]
    fn test_edit_mode_revert() {
        let mut state = EditModeState::new("echo hello");
        state.select(1);
        state.edit_buffer = "world".to_string();
        assert!(state.has_changes());

        state.revert();
        assert!(!state.has_changes());
        assert_eq!(state.selected, 0);
        assert_eq!(state.edit_buffer, "echo");
    }

    // =========================================================================
    // Helper Function Tests
    // =========================================================================

    #[test]
    fn test_superscript_digit() {
        assert_eq!(superscript_digit(1), "¹");
        assert_eq!(superscript_digit(10), "¹⁰");
        assert_eq!(superscript_digit(20), "²⁰");
        assert_eq!(superscript_digit(21), "·"); // Fallback
        assert_eq!(superscript_digit(0), "·"); // Fallback
    }

    #[test]
    fn test_edit_mode_reclassify_after_delete() {
        // When deleting first token, second token should become Command
        let mut state = EditModeState::new("sudo git push");
        assert_eq!(state.tokens[0].token_type, TokenType::Command);
        assert_eq!(state.tokens[1].token_type, TokenType::Argument); // "git" at position 1 is argument

        state.select(0); // Select "sudo"
        state.delete_token();

        // After deleting "sudo", "git" is now position 0 and should be Command
        assert_eq!(state.tokens[0].text, "git");
        assert_eq!(state.tokens[0].token_type, TokenType::Command);
        // "push" is now position 1 after "git", so it's a Subcommand
        assert_eq!(state.tokens[1].text, "push");
        assert_eq!(state.tokens[1].token_type, TokenType::Subcommand);
    }

    #[test]
    fn test_edit_mode_reclassify_after_insert() {
        let mut state = EditModeState::new("push origin");
        // Initially "push" is Command (position 0)
        assert_eq!(state.tokens[0].token_type, TokenType::Command);

        // Insert "git" before "push"
        state.select(0);
        state.insert_token_before();
        state.edit_buffer = "git".to_string();
        // Commit by selecting another token
        state.select(1);

        // Now "git" is Command, "push" is Subcommand
        assert_eq!(state.tokens[0].text, "git");
        assert_eq!(state.tokens[0].token_type, TokenType::Command);
        assert_eq!(state.tokens[1].text, "push");
        assert_eq!(state.tokens[1].token_type, TokenType::Subcommand);
    }

    #[test]
    fn test_edit_mode_reclassify_after_text_change() {
        let mut state = EditModeState::new("ls origin");
        // "ls" is Command, "origin" is Argument
        assert_eq!(state.tokens[0].token_type, TokenType::Command);
        assert_eq!(state.tokens[1].token_type, TokenType::Argument);

        // Change "ls" to "git"
        state.select(0);
        state.edit_buffer = "git".to_string();
        state.select(1); // Commit the change

        // Now "origin" after "git" should be reclassified as Subcommand
        assert_eq!(state.tokens[0].text, "git");
        assert_eq!(state.tokens[0].token_type, TokenType::Command);
        assert_eq!(state.tokens[1].text, "origin");
        // Note: "origin" isn't a recognized git subcommand, so it stays Argument
        // Let's change to a better test...
    }

    #[test]
    fn test_edit_mode_reclassify_git_subcommand() {
        let mut state = EditModeState::new("ls push");
        assert_eq!(state.tokens[1].token_type, TokenType::Argument);

        // Change "ls" to "git"
        state.select(0);
        state.edit_buffer = "git".to_string();
        state.select(1);

        // "push" after "git" should become Subcommand
        assert_eq!(state.tokens[1].text, "push");
        assert_eq!(state.tokens[1].token_type, TokenType::Subcommand);
    }
}
