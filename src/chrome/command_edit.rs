//! Unified command editing state for browser panels.
//!
//! Provides a configurable token-based command editor with support for:
//! - Locked (non-editable) tokens (e.g., filename in file browser)
//! - Dangerous command detection and confirmation
//! - Quote style cycling
//! - Undo/redo stack
//! - Pluggable suggestion providers
//! - Intelligent suggestions from learned patterns

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::theme::Theme;
use ratatui_core::style::{Modifier, Style};

use crate::history_store::HistoryStore;
use crate::intelligence::FileContext;

// ============================================================================
// Token Types and Classification
// ============================================================================

/// Token type for semantic classification and styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    /// First token - the command (ls, git, etc.)
    Command,
    /// Second token for compound commands (checkout, push)
    Subcommand,
    /// Starts with - or --
    Flag,
    /// Contains / or starts with . or ~
    Path,
    /// Contains :// or looks like git@...
    Url,
    /// Generic argument
    Argument,
    /// Non-editable token (e.g., filename in file browser)
    Locked,
}

/// Quote style for tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteStyle {
    None,
    Single, // 'text'
    Double, // "text"
}

/// A token from a shell command.
#[derive(Debug, Clone)]
pub struct CommandToken {
    /// The text content of this token.
    pub text: String,
    /// Semantic type of this token.
    pub token_type: TokenType,
    /// Whether this token can be edited.
    pub locked: bool,
}

impl CommandToken {
    /// Creates a new editable token.
    pub fn new(text: impl Into<String>, token_type: TokenType) -> Self {
        Self {
            text: text.into(),
            token_type,
            locked: false,
        }
    }

    /// Creates a new locked (non-editable) token.
    pub fn locked(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            token_type: TokenType::Locked,
            locked: true,
        }
    }
}

/// Classifies a token based on its content and position.
pub fn classify_token(text: &str, position: usize, prev_token: Option<&str>) -> TokenType {
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
            // Use canonical implementation from tokenizer
            if crate::intelligence::tokenizer::is_compound_command(cmd) {
                return TokenType::Subcommand;
            }
        }
    }
    TokenType::Argument
}

/// Returns the style for a token based on its type.
pub fn token_type_style(token_type: TokenType, theme: &Theme) -> Style {
    match token_type {
        TokenType::Command => Style::default()
            .fg(theme.semantic_success)
            .add_modifier(Modifier::BOLD),
        TokenType::Subcommand => Style::default().fg(theme.header_fg),
        TokenType::Flag => Style::default().fg(theme.text_highlight),
        TokenType::Path => Style::default().fg(theme.semantic_info),
        TokenType::Url => Style::default().fg(theme.git_fg),
        TokenType::Argument => Style::default().fg(theme.text_primary),
        TokenType::Locked => Style::default().fg(theme.text_highlight),
    }
}

/// Returns a superscript representation of any positive number.
/// Converts each digit to its Unicode superscript equivalent.
pub fn superscript_number(n: usize) -> String {
    const SUPERSCRIPT_DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

    if n == 0 {
        return "⁰".to_string();
    }

    n.to_string()
        .chars()
        .map(|c| {
            c.to_digit(10)
                .map(|d| SUPERSCRIPT_DIGITS[d as usize])
                .unwrap_or(c)
        })
        .collect()
}

// ============================================================================
// Quote Handling
// ============================================================================

/// Splits a token into its inner text and detected quote style.
pub fn split_quotes(text: &str) -> (String, QuoteStyle) {
    if text.len() >= 2 {
        if text.starts_with('\'') && text.ends_with('\'') {
            return (text[1..text.len() - 1].to_string(), QuoteStyle::Single);
        }
        if text.starts_with('"') && text.ends_with('"') {
            return (text[1..text.len() - 1].to_string(), QuoteStyle::Double);
        }
    }
    (text.to_string(), QuoteStyle::None)
}

/// Applies a quote style to text.
pub fn apply_quotes(text: &str, style: QuoteStyle) -> String {
    match style {
        QuoteStyle::None => text.to_string(),
        QuoteStyle::Single => format!("'{}'", text),
        QuoteStyle::Double => format!("\"{}\"", text),
    }
}

/// POSIX-quotes a string if it contains shell metacharacters or spaces.
/// Returns the string unchanged if no quoting is needed.
pub fn quote_for_shell(text: &str) -> String {
    if text.is_empty() {
        return "''".to_string();
    }

    // Check if quoting is needed
    let needs_quoting = text.chars().any(|c| {
        matches!(
            c,
            ' ' | '\t'
                | '\n'
                | '"'
                | '\''
                | '\\'
                | '$'
                | '`'
                | '!'
                | '*'
                | '?'
                | '['
                | ']'
                | '('
                | ')'
                | '{'
                | '}'
                | '<'
                | '>'
                | '|'
                | '&'
                | ';'
                | '#'
                | '~'
        )
    });

    if !needs_quoting {
        return text.to_string();
    }

    // Use single quotes, escaping any embedded single quotes
    // In POSIX shell: 'foo'\''bar' produces foo'bar
    if text.contains('\'') {
        let escaped = text.replace('\'', "'\\''");
        format!("'{}'", escaped)
    } else {
        format!("'{}'", text)
    }
}

// ============================================================================
// Dangerous Command Detection
// ============================================================================

/// Result of dangerous command check.
#[derive(Debug, Clone)]
pub struct DangerWarning {
    /// Human-readable description of why this command is dangerous.
    pub message: &'static str,
}

/// Checks if a command is potentially dangerous.
pub fn check_dangerous_command(command: &str) -> Option<DangerWarning> {
    let lower = command.to_lowercase();

    if lower.contains("rm -rf") || lower.contains("rm -fr") {
        return Some(DangerWarning {
            message: "Recursive force delete",
        });
    }
    if lower.contains("dd if=") && lower.contains("of=/dev/") {
        return Some(DangerWarning {
            message: "Direct disk write",
        });
    }
    if lower.contains("mkfs") {
        return Some(DangerWarning {
            message: "Filesystem format",
        });
    }
    if lower.contains("> /dev/sd") || lower.contains(">/dev/sd") {
        return Some(DangerWarning {
            message: "Direct device write",
        });
    }
    if lower.contains("chmod -r 777") || lower.contains("chmod 777 -r") {
        return Some(DangerWarning {
            message: "Overly permissive chmod",
        });
    }
    if lower.contains(":(){ :|:& };:") {
        return Some(DangerWarning {
            message: "Fork bomb",
        });
    }

    None
}

// ============================================================================
// Tokenizer
// ============================================================================

/// Tokenizes a shell command into words, respecting quotes.
pub fn tokenize_command(command: &str) -> Vec<CommandToken> {
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

    if !current.is_empty() {
        raw_tokens.push(current);
    }

    // Classify each token
    raw_tokens
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let prev = if i > 0 {
                Some(raw_tokens[i - 1].as_str())
            } else {
                None
            };
            let token_type = classify_token(text, i, prev);
            CommandToken::new(text.clone(), token_type)
        })
        .collect()
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for command edit behavior.
#[derive(Debug, Clone)]
pub struct EditConfig {
    /// Enable dangerous command detection and confirmation.
    pub danger_check: bool,
    /// Enable undo/redo functionality.
    pub enable_undo: bool,
    /// Enable quote style cycling (Ctrl+Q).
    pub enable_quotes: bool,
    /// Maximum undo stack size.
    pub max_undo_size: usize,
}

impl Default for EditConfig {
    fn default() -> Self {
        Self {
            danger_check: true,
            enable_undo: true,
            enable_quotes: true,
            max_undo_size: 50,
        }
    }
}

impl EditConfig {
    /// Config for history browser (full features).
    pub fn for_history() -> Self {
        Self::default()
    }

    /// Config for file browser (minimal features).
    pub fn for_file() -> Self {
        Self {
            danger_check: false,
            enable_undo: true,
            enable_quotes: false,
            max_undo_size: 20,
        }
    }
}

// ============================================================================
// Confirmation State
// ============================================================================

/// State for dangerous command confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmState {
    /// The command awaiting confirmation.
    pub command: String,
    /// Warning message to display.
    pub warning: DangerWarning,
}

// ============================================================================
// Command Edit State
// ============================================================================

/// Unified state for command editing.
///
/// This is the core editing state used by both history and file browsers.
/// Features can be enabled/disabled via `EditConfig`.
#[derive(Clone)]
pub struct CommandEditState {
    // --- Core State ---
    /// The original command (for revert and change detection).
    pub original: String,
    /// Tokenized parts of the command.
    pub tokens: Vec<CommandToken>,
    /// Index of the currently selected token (0-based).
    pub selected: usize,
    /// Current edit buffer for the selected token.
    pub edit_buffer: String,

    // --- Configuration ---
    /// Feature configuration.
    pub config: EditConfig,

    // --- Undo State ---
    /// Undo stack for reverting changes (tokens, edit_buffer, selected).
    undo_stack: Vec<(Vec<CommandToken>, String, usize)>,

    // --- Original State ---
    /// Original tokens for revert (preserves locked state).
    original_tokens: Vec<CommandToken>,

    // --- Suggestion State ---
    /// Current suggestions for the selected token position.
    pub suggestions: Vec<String>,
    /// Index into suggestions (None = using custom/typed value).
    pub suggestion_index: Option<usize>,

    // --- Confirmation State ---
    /// Pending dangerous command confirmation.
    pub pending_confirm: Option<ConfirmState>,
    /// Skip dangerous command checks (toggled with Ctrl+!).
    pub skip_danger_check: bool,

    // --- Context ---
    /// Optional context for suggestions (e.g., filename for file browser).
    context: Option<String>,

    // --- Intelligence Context ---
    /// History store for intelligent suggestions.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Current working directory for context-aware suggestions.
    cwd: Option<PathBuf>,
    /// File context for file-type specific suggestions.
    file_context: Option<FileContext>,
    /// Last executed command for session-based suggestions.
    last_command: Option<String>,
}

impl std::fmt::Debug for CommandEditState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEditState")
            .field("original", &self.original)
            .field("tokens", &self.tokens)
            .field("selected", &self.selected)
            .field("edit_buffer", &self.edit_buffer)
            .field("config", &self.config)
            .field("suggestions", &self.suggestions)
            .field("suggestion_index", &self.suggestion_index)
            .field("pending_confirm", &self.pending_confirm)
            .field("skip_danger_check", &self.skip_danger_check)
            .field("context", &self.context)
            .field(
                "history_store",
                &self.history_store.as_ref().map(|_| "<HistoryStore>"),
            )
            .field("cwd", &self.cwd)
            .field("file_context", &self.file_context)
            .field("last_command", &self.last_command)
            .finish()
    }
}

impl CommandEditState {
    // ========================================================================
    // Constructors
    // ========================================================================

    /// Creates a new edit state from a list of tokens.
    pub fn new(mut tokens: Vec<CommandToken>, config: EditConfig) -> Self {
        // Guard against empty tokens to avoid panics in methods like insert_token_after
        if tokens.is_empty() {
            tokens.push(CommandToken::new(String::new(), TokenType::Command));
        }
        let original = tokens
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let edit_buffer = tokens.first().map(|t| t.text.clone()).unwrap_or_default();
        let original_tokens = tokens.clone();
        Self {
            original,
            tokens,
            selected: 0,
            edit_buffer,
            config,
            undo_stack: Vec::new(),
            original_tokens,
            suggestions: Vec::new(),
            suggestion_index: None,
            pending_confirm: None,
            skip_danger_check: false,
            context: None,
            history_store: None,
            cwd: None,
            file_context: None,
            last_command: None,
        }
    }

    /// Creates edit state from a command string (for history browser).
    pub fn from_command(command: &str) -> Self {
        let mut tokens = tokenize_command(command);
        // Ensure tokens is never empty
        if tokens.is_empty() {
            tokens.push(CommandToken::new(String::new(), TokenType::Command));
        }
        Self::new(tokens, EditConfig::for_history())
    }

    /// Creates edit state for file editing with a locked filename token.
    pub fn for_file(filename: &str, filepath: &str) -> Self {
        let tokens = vec![
            CommandToken::new(String::new(), TokenType::Command),
            CommandToken::locked(filepath),
        ];
        let mut state = Self::new(tokens, EditConfig::for_file());
        state.context = Some(filename.to_string());
        state.update_suggestions();
        state
    }

    // ========================================================================
    // Intelligence Context Setters
    // ========================================================================

    /// Sets the history store for intelligent suggestions.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.history_store = Some(store);
    }

    /// Sets the current working directory for context-aware suggestions.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = Some(cwd);
    }

    /// Sets the file context for file-type specific suggestions.
    pub fn set_file_context(&mut self, file_context: FileContext) {
        self.file_context = Some(file_context);
    }

    /// Sets the last executed command for session-based suggestions.
    pub fn set_last_command(&mut self, last_command: String) {
        self.last_command = Some(last_command);
    }

    /// Configures all intelligence context at once.
    pub fn set_intelligence_context(
        &mut self,
        history_store: Arc<Mutex<HistoryStore>>,
        cwd: Option<PathBuf>,
        file_context: Option<FileContext>,
        last_command: Option<String>,
    ) {
        self.history_store = Some(history_store);
        self.cwd = cwd;
        self.file_context = file_context;
        self.last_command = last_command;
    }

    // ========================================================================
    // Token Access
    // ========================================================================

    /// Returns the number of tokens.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Returns true if the selected token is locked.
    pub fn is_selected_locked(&self) -> bool {
        self.tokens
            .get(self.selected)
            .map(|t| t.locked)
            .unwrap_or(false)
    }

    /// Returns the currently selected token, if any.
    pub fn selected_token(&self) -> Option<&CommandToken> {
        self.tokens.get(self.selected)
    }

    /// Returns the type hint for the current token position.
    pub fn type_hint(&self) -> &'static str {
        match self.selected_token().map(|t| t.token_type) {
            Some(TokenType::Command) => "cmd",
            Some(TokenType::Subcommand) => "sub",
            Some(TokenType::Flag) => "flag",
            Some(TokenType::Path) => "path",
            Some(TokenType::Url) => "url",
            Some(TokenType::Locked) => "file",
            Some(TokenType::Argument) | None => "arg",
        }
    }

    // ========================================================================
    // Navigation
    // ========================================================================

    /// Saves current edit to the selected token.
    fn save_current_edit(&mut self) {
        if let Some(token) = self.tokens.get_mut(self.selected) {
            if !token.locked {
                token.text = self.edit_buffer.clone();
            }
        }
    }

    /// Selects a token by index.
    pub fn select(&mut self, index: usize) {
        if index < self.tokens.len() {
            self.save_current_edit();
            self.reclassify_tokens();
            self.selected = index;
            self.edit_buffer = self.tokens[index].text.clone();
            self.suggestion_index = None;
        }
    }

    /// Moves to the next token.
    pub fn next(&mut self) {
        if self.selected + 1 < self.tokens.len() {
            self.select(self.selected + 1);
        }
    }

    /// Moves to the previous token.
    pub fn prev(&mut self) {
        if self.selected > 0 {
            self.select(self.selected - 1);
        }
    }

    // ========================================================================
    // Editing
    // ========================================================================

    /// Types a character into the edit buffer (if not locked).
    pub fn type_char(&mut self, c: char) {
        if !self.is_selected_locked() {
            self.save_undo();
            self.edit_buffer.push(c);
            self.suggestion_index = None;
        }
    }

    /// Deletes a character from the edit buffer (if not locked).
    pub fn backspace(&mut self) {
        if !self.is_selected_locked() {
            self.save_undo();
            self.edit_buffer.pop();
            self.suggestion_index = None;
        }
    }

    /// Builds the final command from the edited tokens.
    /// Locked tokens (e.g., file paths) are POSIX-quoted if they contain spaces or special chars.
    pub fn build_command(&mut self) -> String {
        self.save_current_edit();
        self.tokens
            .iter()
            .filter(|t| !t.text.is_empty())
            .map(|t| {
                if t.locked {
                    quote_for_shell(&t.text)
                } else {
                    t.text.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Reclassifies all non-locked tokens based on their position.
    pub fn reclassify_tokens(&mut self) {
        for i in 0..self.tokens.len() {
            if self.tokens[i].locked {
                continue;
            }
            let prev_text = if i > 0 {
                Some(self.tokens[i - 1].text.as_str())
            } else {
                None
            };
            self.tokens[i].token_type = classify_token(&self.tokens[i].text, i, prev_text);
        }
    }

    // ========================================================================
    // Token Mutation
    // ========================================================================

    /// Saves current state to undo stack.
    fn save_undo(&mut self) {
        if !self.config.enable_undo {
            return;
        }
        self.undo_stack
            .push((self.tokens.clone(), self.edit_buffer.clone(), self.selected));
        if self.undo_stack.len() > self.config.max_undo_size {
            self.undo_stack.remove(0);
        }
    }

    /// Restores previous state from undo stack.
    pub fn undo(&mut self) {
        if let Some((tokens, edit_buffer, selected)) = self.undo_stack.pop() {
            self.tokens = tokens;
            self.selected = selected.min(self.tokens.len().saturating_sub(1));
            self.edit_buffer = edit_buffer;
            self.reclassify_tokens();
        }
    }

    /// Deletes the currently selected token (if not locked).
    pub fn delete_token(&mut self) {
        if self.is_selected_locked() {
            return;
        }
        // Don't delete if only one non-locked token remains
        let non_locked_count = self.tokens.iter().filter(|t| !t.locked).count();
        if non_locked_count <= 1 {
            return;
        }
        self.save_undo();
        self.tokens.remove(self.selected);
        if self.selected >= self.tokens.len() {
            self.selected = self.tokens.len().saturating_sub(1);
        }
        self.edit_buffer = self
            .tokens
            .get(self.selected)
            .map(|t| t.text.clone())
            .unwrap_or_default();
        self.reclassify_tokens();
        self.suggestion_index = None;
    }

    /// Inserts a new token after the current one.
    pub fn insert_token_after(&mut self) {
        self.save_undo();
        self.save_current_edit();
        let new_token = CommandToken::new(String::new(), TokenType::Argument);
        let insert_pos = self.selected + 1;
        self.tokens.insert(insert_pos, new_token);
        self.selected = insert_pos;
        self.edit_buffer.clear();
        self.reclassify_tokens();
        self.suggestion_index = None;
    }

    /// Inserts a new token before the current one.
    pub fn insert_token_before(&mut self) {
        self.save_undo();
        self.save_current_edit();
        let new_token = CommandToken::new(String::new(), TokenType::Argument);
        self.tokens.insert(self.selected, new_token);
        // selected now points to the new empty token
        self.edit_buffer.clear();
        self.reclassify_tokens();
        self.suggestion_index = None;
    }

    // ========================================================================
    // Quote Cycling
    // ========================================================================

    /// Cycles through quote styles for current token.
    pub fn cycle_quote(&mut self) {
        if !self.config.enable_quotes || self.is_selected_locked() {
            return;
        }
        self.save_undo();
        let (inner, current_style) = split_quotes(&self.edit_buffer);
        let new_style = match current_style {
            QuoteStyle::None => QuoteStyle::Single,
            QuoteStyle::Single => QuoteStyle::Double,
            QuoteStyle::Double => QuoteStyle::None,
        };
        self.edit_buffer = apply_quotes(&inner, new_style);
    }

    // ========================================================================
    // Suggestions
    // ========================================================================

    /// Cycles through suggestions in the given direction.
    pub fn cycle_suggestion(&mut self, direction: i32) {
        if self.suggestions.is_empty() || self.is_selected_locked() {
            return;
        }

        let new_index = match self.suggestion_index {
            None => {
                if direction > 0 {
                    0
                } else {
                    self.suggestions.len() - 1
                }
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
    pub fn prev_suggestion(&self) -> Option<&str> {
        if self.suggestions.is_empty() {
            return None;
        }
        let idx = self.suggestion_index.unwrap_or(0);
        let len = self.suggestions.len();
        let prev_idx = if idx == 0 { len - 1 } else { idx - 1 };
        self.suggestions.get(prev_idx).map(|s| s.as_str())
    }

    /// Returns the next suggestion for display.
    pub fn next_suggestion(&self) -> Option<&str> {
        if self.suggestions.is_empty() {
            return None;
        }
        let len = self.suggestions.len();
        // Treat None as one-before-start so (len - 1 + 1) % len = 0
        let idx = self.suggestion_index.unwrap_or(len - 1);
        let next_idx = (idx + 1) % len;
        self.suggestions.get(next_idx).map(|s| s.as_str())
    }

    /// Updates suggestions based on current context.
    ///
    /// When on a locked token, suggestions are preserved (not cleared) so
    /// the suggestion rows remain visible for context. Users can't cycle
    /// suggestions on locked tokens, but they can still see what was suggested.
    ///
    /// Suggestions are gathered from the learned command hierarchy, which includes
    /// bootstrapped knowledge for common commands. The hierarchy learns from user
    /// command history and provides position-aware token suggestions.
    pub fn update_suggestions(&mut self) {
        if self.is_selected_locked() {
            // Don't clear suggestions on locked tokens - preserve them for display
            return;
        }

        self.suggestions.clear();

        // Primary source: intelligent suggestions from learned hierarchy
        // The hierarchy includes bootstrapped data, so it handles all cases
        if let Some(ref store) = self.history_store {
            if let Ok(store) = store.lock() {
                if store.is_intelligence_enabled() {
                    let tokens = &self.tokens[..self.selected];

                    // Only apply prefix filter when user has modified the text
                    // If edit_buffer matches the original token, show all suggestions
                    let current_token_text = self
                        .tokens
                        .get(self.selected)
                        .map(|t| t.text.as_str())
                        .unwrap_or("");
                    let partial = if self.edit_buffer == current_token_text {
                        "" // Show all suggestions - user hasn't started typing
                    } else {
                        &self.edit_buffer // Filter by what user is typing
                    };

                    let intelligent_suggestions = store.intelligent_suggest(
                        tokens,
                        partial,
                        self.cwd.clone(),
                        self.file_context.clone(),
                        self.last_command.clone(),
                    );

                    self.suggestions = intelligent_suggestions
                        .into_iter()
                        .map(|s| s.text)
                        .collect();
                }
            }
        }

        self.suggestion_index = None;
    }

    /// Adds external suggestions (e.g., from history) to the front.
    pub fn add_suggestions(&mut self, external: Vec<String>) {
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

    // ========================================================================
    // Change Detection
    // ========================================================================

    /// Returns true if there are any changes from the original.
    pub fn is_changed(&self) -> bool {
        let current: String = self
            .tokens
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.selected && !t.locked {
                    self.edit_buffer.clone()
                } else {
                    t.text.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        current != self.original
    }

    /// Reverts all changes back to the original command.
    pub fn revert(&mut self) {
        self.tokens = self.original_tokens.clone();
        if self.tokens.is_empty() {
            self.tokens
                .push(CommandToken::new(String::new(), TokenType::Command));
        }
        self.selected = 0;
        self.edit_buffer = self
            .tokens
            .first()
            .map(|t| t.text.clone())
            .unwrap_or_default();
        self.undo_stack.clear();
        self.suggestion_index = None;
    }

    // ========================================================================
    // Dangerous Command Handling
    // ========================================================================

    /// Checks if current command is dangerous and needs confirmation.
    /// Returns the command if safe to execute, None if confirmation needed.
    pub fn check_and_prepare_execute(&mut self) -> Option<String> {
        let command = self.build_command();

        if !self.config.danger_check || self.skip_danger_check {
            return Some(command);
        }

        if let Some(warning) = check_dangerous_command(&command) {
            self.pending_confirm = Some(ConfirmState { command, warning });
            return None;
        }

        Some(command)
    }

    /// Confirms and returns the pending dangerous command.
    pub fn confirm_dangerous(&mut self) -> Option<String> {
        self.pending_confirm.take().map(|c| c.command)
    }

    /// Cancels the pending dangerous command confirmation.
    pub fn cancel_confirm(&mut self) {
        self.pending_confirm = None;
    }

    /// Returns true if waiting for confirmation.
    pub fn is_confirming(&self) -> bool {
        self.pending_confirm.is_some()
    }

    /// Toggles the skip_danger_check flag.
    pub fn toggle_danger_check(&mut self) {
        self.skip_danger_check = !self.skip_danger_check;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_command_simple_classifies_tokens() {
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
    fn test_tokenize_command_quoted_preserves_quotes() {
        let tokens = tokenize_command("echo \"hello world\"");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[1].text, "\"hello world\"");
    }

    #[test]
    fn test_tokenize_command_git_checkout_flags_identified() {
        let tokens = tokenize_command("git checkout -b feature");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].token_type, TokenType::Subcommand);
        assert_eq!(tokens[2].token_type, TokenType::Flag);
    }

    #[test]
    fn test_for_file_creates_locked_token() {
        let state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        assert_eq!(state.tokens.len(), 2);
        assert!(!state.tokens[0].locked);
        assert!(state.tokens[1].locked);
        assert_eq!(state.tokens[1].text, "/path/to/test.rs");
    }

    #[test]
    fn test_for_file_starts_with_empty_suggestions_without_intelligence() {
        // Without intelligence configured, for_file creates state but suggestions are empty
        // Suggestions require a HistoryStore with intelligence enabled
        let state = CommandEditState::for_file("test.rs", "/path/to/test.rs");

        // State is correctly initialized
        assert_eq!(state.tokens.len(), 2);
        assert_eq!(state.tokens[0].text, "");
        assert_eq!(state.tokens[1].text, "/path/to/test.rs");

        // Without intelligence, suggestions are empty (no static fallback)
        assert!(
            state.suggestions.is_empty(),
            "Without intelligence, suggestions should be empty"
        );
    }

    #[test]
    fn test_next_prev_suggestion_cycles_through_list() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        // Manually populate suggestions to test cycling mechanics
        state.suggestions = vec!["cat".to_string(), "less".to_string(), "vim".to_string()];

        // Should return suggestions for display
        assert!(
            state.next_suggestion().is_some(),
            "next_suggestion should return Some when suggestions exist"
        );
        assert!(
            state.prev_suggestion().is_some(),
            "prev_suggestion should return Some when suggestions exist"
        );

        // Verify cycling works correctly
        assert_eq!(state.next_suggestion(), Some("cat"));
        assert_eq!(state.prev_suggestion(), Some("vim"));
    }

    #[test]
    fn test_suggestions_preserved_on_locked_token() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        // Manually populate suggestions to test preservation behavior
        state.suggestions = vec!["cat".to_string(), "less".to_string(), "vim".to_string()];
        let initial_count = state.suggestions.len();

        // Move to locked token (filename)
        state.next();
        state.update_suggestions();

        // Suggestions should be preserved, not cleared (key behavior being tested)
        assert_eq!(
            state.suggestions.len(),
            initial_count,
            "Suggestions should be preserved when on locked token"
        );
        assert!(
            state.next_suggestion().is_some(),
            "next_suggestion should still work on locked token"
        );
    }

    #[test]
    fn test_history_browser_suggestions_cycling() {
        // Test suggestion cycling in history browser context
        let mut state =
            CommandEditState::from_command("git remote add origin git@github.com:user/repo.git");

        // Manually populate suggestions to test cycling mechanics
        state.suggestions = vec![
            "status".to_string(),
            "push".to_string(),
            "pull".to_string(),
            "commit".to_string(),
        ];

        // Both prev and next should return values
        let prev = state.prev_suggestion();
        let next = state.next_suggestion();

        assert!(
            prev.is_some(),
            "prev_suggestion should return Some when suggestions exist"
        );
        assert!(
            next.is_some(),
            "next_suggestion should return Some when suggestions exist"
        );

        // They should be different (cycling through list)
        assert_ne!(
            prev, next,
            "prev and next should show different suggestions"
        );

        // Test cycling updates edit_buffer
        state.cycle_suggestion(1);
        assert!(
            !state.edit_buffer.is_empty(),
            "edit_buffer should update after cycling"
        );
    }

    #[test]
    fn test_update_suggestions_clears_without_intelligence() {
        // Without intelligence configured, update_suggestions clears suggestions
        let mut state = CommandEditState::from_command("git status");

        // Manually populate some suggestions
        state.suggestions = vec!["push".to_string(), "pull".to_string()];

        // Call update_suggestions without history_store set
        state.update_suggestions();

        // Without intelligence, suggestions are cleared
        assert!(
            state.suggestions.is_empty(),
            "Without intelligence, update_suggestions should clear suggestions"
        );
    }

    #[test]
    fn test_file_browser_build_command_after_cycling() {
        // Simulate file browser edit mode flow
        let mut state = CommandEditState::for_file("test.txt", "/path/to/test.txt");

        // Initially token 0 is empty, token 1 is the locked filepath
        assert_eq!(state.tokens[0].text, "");
        assert_eq!(state.tokens[1].text, "/path/to/test.txt");
        assert!(state.tokens[1].locked);

        // Manually populate suggestions to test cycling behavior
        state.suggestions = vec!["cat".to_string(), "less".to_string(), "vim".to_string()];

        // User cycles through suggestions to select "cat"
        state.cycle_suggestion(1); // Cycle to first suggestion
        assert_eq!(
            state.edit_buffer, "cat",
            "Edit buffer should have 'cat' after cycling"
        );

        // User presses Enter - build_command should create the full command
        let command = state.build_command();

        assert!(!command.is_empty(), "Command should not be empty");
        assert!(
            command.starts_with("cat"),
            "Command should start with 'cat', got: {}",
            command
        );
        assert!(
            command.contains("/path/to/test.txt"),
            "Command should contain the filepath, got: {}",
            command
        );
        assert_eq!(
            command, "cat /path/to/test.txt",
            "Full command should be 'cat /path/to/test.txt'"
        );
    }

    #[test]
    fn test_insert_token_after_increments_count_and_selection() {
        let mut state = CommandEditState::from_command("git push");
        assert_eq!(state.token_count(), 2);
        state.select(1);
        state.insert_token_after();
        assert_eq!(state.token_count(), 3);
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn test_delete_token_locked_token_preserves_count() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        state.select(1); // Select locked token
        state.delete_token();
        assert_eq!(state.tokens.len(), 2); // Should not delete
    }

    #[test]
    fn test_undo_after_delete_restores_token_count() {
        let mut state = CommandEditState::from_command("git push origin main");
        assert_eq!(state.token_count(), 4);
        state.select(2);
        state.delete_token();
        assert_eq!(state.token_count(), 3);
        state.undo();
        assert_eq!(state.token_count(), 4);
    }

    #[test]
    fn test_cycle_quote_selected_token_updates_edit_buffer_and_quotes() {
        let mut state = CommandEditState::from_command("echo hello");
        state.select(1);
        assert_eq!(state.edit_buffer, "hello");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "'hello'");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "\"hello\"");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "hello");
    }

    #[test]
    fn test_check_dangerous_command_detects_rm_rf_and_allows_safe() {
        assert!(check_dangerous_command("rm -rf /").is_some());
        assert!(check_dangerous_command("sudo rm -rf /tmp").is_some());
        assert!(check_dangerous_command("ls -la").is_none());
    }

    #[test]
    fn test_is_changed_after_edit_returns_true() {
        let mut state = CommandEditState::from_command("echo hello");
        assert!(!state.is_changed());

        state.select(1);
        state.edit_buffer = "world".to_string();
        assert!(state.is_changed());
    }

    #[test]
    fn test_revert_after_edit_restores_original_token() {
        let mut state = CommandEditState::from_command("echo hello");
        state.select(1);
        state.edit_buffer = "world".to_string();
        state.save_current_edit();

        state.revert();
        assert!(!state.is_changed());
        assert_eq!(state.tokens[1].text, "hello");
    }

    #[test]
    fn test_edit_config_for_history_enables_danger_check_and_quotes() {
        let config = EditConfig::for_history();
        assert!(config.danger_check);
        assert!(config.enable_quotes);
    }

    #[test]
    fn test_edit_config_for_file_disables_danger_check_and_quotes() {
        let config = EditConfig::for_file();
        assert!(!config.danger_check);
        assert!(!config.enable_quotes);
    }

    #[test]
    fn test_superscript_number_formats_digits_correctly() {
        assert_eq!(superscript_number(0), "⁰");
        assert_eq!(superscript_number(1), "¹");
        assert_eq!(superscript_number(10), "¹⁰");
        assert_eq!(superscript_number(20), "²⁰");
        assert_eq!(superscript_number(21), "²¹");
        assert_eq!(superscript_number(99), "⁹⁹");
        assert_eq!(superscript_number(123), "¹²³");
    }

    #[test]
    fn test_split_quotes_returns_text_and_style() {
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
    fn test_apply_quotes_wraps_text_with_style() {
        assert_eq!(apply_quotes("hello", QuoteStyle::None), "hello");
        assert_eq!(apply_quotes("hello", QuoteStyle::Single), "'hello'");
        assert_eq!(apply_quotes("hello", QuoteStyle::Double), "\"hello\"");
    }
}
