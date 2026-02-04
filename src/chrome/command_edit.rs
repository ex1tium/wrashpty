//! Shared command editing state for history and file browser panels.
//!
//! Provides a unified token-based command editor with support for locked
//! (non-editable) tokens like filenames.

use super::command_knowledge::COMMAND_KNOWLEDGE;
use super::theme::Theme;
use ratatui_core::style::{Modifier, Style};

/// Token type for semantic classification.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenType {
    Command,    // First token (ls, git, etc.)
    Subcommand, // Second token for compound commands (checkout, push)
    Flag,       // Starts with - or --
    Path,       // Contains / or starts with . or ~
    Url,        // Contains :// or looks like git@...
    Argument,   // Generic argument
    Locked,     // Non-editable token (e.g., filename in file browser)
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
    pub fn new(text: String, token_type: TokenType) -> Self {
        Self {
            text,
            token_type,
            locked: false,
        }
    }

    /// Creates a new locked (non-editable) token.
    pub fn locked(text: String) -> Self {
        Self {
            text,
            token_type: TokenType::Locked,
            locked: true,
        }
    }
}

/// Returns a superscript digit for display (¹²³...²⁰).
pub fn superscript_digit(n: usize) -> &'static str {
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
            if matches!(cmd, "git" | "docker" | "kubectl" | "cargo" | "npm" | "yarn" | "systemctl" | "journalctl") {
                return TokenType::Subcommand;
            }
        }
    }
    TokenType::Argument
}

/// Returns the style for a token based on its type and theme.
pub fn token_type_style(token_type: TokenType, theme: &Theme) -> Style {
    match token_type {
        TokenType::Command => Style::default().fg(theme.semantic_success).add_modifier(Modifier::BOLD),
        TokenType::Subcommand => Style::default().fg(theme.header_fg),
        TokenType::Flag => Style::default().fg(theme.text_highlight),
        TokenType::Path => Style::default().fg(theme.semantic_info),
        TokenType::Url => Style::default().fg(theme.git_fg),
        TokenType::Argument => Style::default().fg(theme.text_primary),
        TokenType::Locked => Style::default().fg(theme.text_highlight),
    }
}

/// Shared state for command editing.
#[derive(Debug, Clone)]
pub struct CommandEditState {
    /// The original command (for revert).
    pub original: String,
    /// Tokenized parts of the command.
    pub tokens: Vec<CommandToken>,
    /// Index of the currently selected token (0-based).
    pub selected: usize,
    /// Current edit buffer for the selected token.
    pub edit_buffer: String,
    /// Undo stack for reverting changes.
    pub undo_stack: Vec<Vec<CommandToken>>,
    /// Current suggestions for the selected token position.
    pub suggestions: Vec<String>,
    /// Index into suggestions (None = using custom/typed value).
    pub suggestion_index: Option<usize>,
}

impl CommandEditState {
    /// Creates a new edit state from a list of tokens.
    pub fn new(tokens: Vec<CommandToken>) -> Self {
        let original = tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        let edit_buffer = tokens.first().map(|t| t.text.clone()).unwrap_or_default();
        Self {
            original,
            tokens,
            selected: 0,
            edit_buffer,
            undo_stack: Vec::new(),
            suggestions: Vec::new(),
            suggestion_index: None,
        }
    }

    /// Creates a new edit state from a command string.
    pub fn from_command(command: &str) -> Self {
        let mut tokens = tokenize_command(command);
        // Ensure tokens is never empty to prevent panics
        if tokens.is_empty() {
            tokens.push(CommandToken::new(String::new(), TokenType::Command));
        }
        Self::new(tokens)
    }

    /// Creates a new edit state for file editing with a locked filename token.
    pub fn for_file(filename: &str, filepath: &str) -> Self {
        let suggestions: Vec<String> = COMMAND_KNOWLEDGE
            .commands_for_filetype(filename)
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Start with an empty command token, then the locked filename
        let tokens = vec![
            CommandToken::new(String::new(), TokenType::Command),
            CommandToken::locked(filepath.to_string()),
        ];

        let mut state = Self::new(tokens);
        state.suggestions = suggestions;
        state
    }

    /// Returns the number of tokens.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Returns true if the selected token is locked.
    pub fn is_selected_locked(&self) -> bool {
        self.tokens.get(self.selected).map(|t| t.locked).unwrap_or(false)
    }

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
            // Save current edit first
            self.save_current_edit();
            // Reclassify after saving
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

    /// Builds the final command from the edited tokens.
    pub fn build_command(&mut self) -> String {
        self.save_current_edit();
        self.tokens
            .iter()
            .filter(|t| !t.text.is_empty())
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Saves current state to undo stack.
    pub fn save_undo(&mut self) {
        self.undo_stack.push(self.tokens.clone());
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
    }

    /// Restores previous state from undo stack.
    pub fn undo(&mut self) {
        if let Some(prev) = self.undo_stack.pop() {
            self.tokens = prev;
            self.selected = self.selected.min(self.tokens.len().saturating_sub(1));
            self.edit_buffer = self.tokens.get(self.selected)
                .map(|t| t.text.clone())
                .unwrap_or_default();
            self.reclassify_tokens();
        }
    }

    /// Deletes the currently selected token (if not locked).
    pub fn delete_token(&mut self) {
        // Don't delete locked tokens
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
        self.edit_buffer = self.tokens.get(self.selected)
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

    /// Cycles through suggestions in the given direction.
    pub fn cycle_suggestion(&mut self, direction: i32) {
        if self.suggestions.is_empty() || self.is_selected_locked() {
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
        let idx = self.suggestion_index.unwrap_or(0);
        let len = self.suggestions.len();
        let next_idx = (idx + 1) % len;
        self.suggestions.get(next_idx).map(|s| s.as_str())
    }

    /// Updates suggestions based on current position.
    /// For file browser: show file-type commands at position 0, pipeable after |
    pub fn update_suggestions_for_file(&mut self, filename: &str) {
        if self.is_selected_locked() {
            self.suggestions.clear();
            return;
        }

        // Check if we're after a pipe
        let has_pipe_before = self.tokens[..self.selected]
            .iter()
            .any(|t| t.text == "|" || t.text.ends_with('|'));

        if has_pipe_before {
            self.suggestions = COMMAND_KNOWLEDGE
                .pipeable_commands()
                .iter()
                .map(|s| s.to_string())
                .collect();
        } else if self.selected == 0 {
            // First position: suggest commands for this file type
            self.suggestions = COMMAND_KNOWLEDGE
                .commands_for_filetype(filename)
                .iter()
                .map(|s| s.to_string())
                .collect();
        } else {
            self.suggestions.clear();
        }
        self.suggestion_index = None;
    }

    /// Updates suggestions based on preceding tokens (for history browser).
    pub fn update_suggestions_for_position(&mut self, preceding: &[&str]) {
        if self.is_selected_locked() {
            self.suggestions.clear();
            return;
        }

        self.suggestions = COMMAND_KNOWLEDGE
            .suggestions_for_position(preceding)
            .iter()
            .map(|s| s.to_string())
            .collect();
        self.suggestion_index = None;
    }

    /// Type a character into the edit buffer (if not locked).
    pub fn type_char(&mut self, c: char) {
        if !self.is_selected_locked() {
            self.edit_buffer.push(c);
            self.suggestion_index = None;
        }
    }

    /// Delete a character from the edit buffer (if not locked).
    pub fn backspace(&mut self) {
        if !self.is_selected_locked() {
            self.edit_buffer.pop();
            self.suggestion_index = None;
        }
    }

    /// Returns true if there are any changes from the original.
    pub fn has_changes(&self) -> bool {
        let current: String = self.tokens.iter().enumerate().map(|(i, t)| {
            if i == self.selected && !t.locked {
                self.edit_buffer.clone()
            } else {
                t.text.clone()
            }
        }).collect::<Vec<_>>().join(" ");
        current != self.original
    }

    /// Returns the type hint for the current token position.
    pub fn type_hint(&self) -> &'static str {
        match self.tokens.get(self.selected).map(|t| t.token_type) {
            Some(TokenType::Command) => "cmd",
            Some(TokenType::Subcommand) => "sub",
            Some(TokenType::Flag) => "flag",
            Some(TokenType::Path) => "path",
            Some(TokenType::Url) => "url",
            Some(TokenType::Locked) => "file",
            Some(TokenType::Argument) | None => "arg",
        }
    }
}

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
    let mut tokens = Vec::new();
    for (i, text) in raw_tokens.iter().enumerate() {
        let prev = if i > 0 { Some(raw_tokens[i - 1].as_str()) } else { None };
        let token_type = classify_token(text, i, prev);
        tokens.push(CommandToken::new(text.clone(), token_type));
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_simple() {
        let tokens = tokenize_command("ls -la /tmp");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].text, "ls");
        assert_eq!(tokens[1].text, "-la");
        assert_eq!(tokens[2].text, "/tmp");
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
    fn test_insert_after_locked() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        state.select(1); // Select the locked filename
        state.insert_token_after();
        assert_eq!(state.tokens.len(), 3);
        assert_eq!(state.selected, 2); // Should be on the new token
        assert!(!state.tokens[2].locked);
    }

    #[test]
    fn test_cannot_delete_locked() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        state.select(1); // Select the locked filename
        state.delete_token();
        assert_eq!(state.tokens.len(), 2); // Should not have deleted
        assert!(state.tokens[1].locked);
    }

    #[test]
    fn test_navigation() {
        let state = CommandEditState::from_command("git push origin main");
        assert_eq!(state.token_count(), 4);
    }
}
