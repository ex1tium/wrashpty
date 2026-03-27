//! Autosuggestion hinter for reedline.
//!
//! This module implements fish-style autosuggestions by searching history
//! for commands matching the current input prefix.
//!
//! # Hinting Strategy
//!
//! The `HistoryHinter` provides inline suggestions based on command history:
//! - Hints are shown only when the cursor is at the end of the line
//! - The most recent matching history entry is suggested
//! - Suggestions appear as dimmed text that can be accepted with the right arrow key
//!
//! # When Hints Are Shown
//!
//! Hints are displayed when:
//! - The current line is not empty
//! - The cursor is at the end of the line
//! - There is a matching entry in history
//!
//! # Hinter Trait Implementation
//!
//! This module implements reedline's `Hinter` trait, which is called after each
//! keystroke to provide inline completion suggestions.

use nu_ansi_term::{Color, Style};
use reedline::{Hinter, History, StyledText};
use tracing::debug;

/// History-based hinter for fish-style autosuggestions.
///
/// Provides inline suggestions by searching command history for entries
/// that start with the current input. The suggestion is shown as dimmed
/// text following the cursor.
pub struct HistoryHinter {
    /// Style for the hint text (dimmed).
    style: Style,
    /// The most recent hint returned by `handle()`, used for accepting hints.
    last_hint: String,
}

impl HistoryHinter {
    /// Creates a new HistoryHinter with default dimmed style.
    pub fn new() -> Self {
        Self {
            style: Style::new().fg(Color::DarkGray),
            last_hint: String::new(),
        }
    }

    /// Creates a HistoryHinter with a custom style.
    pub fn with_style(style: Style) -> Self {
        Self {
            style,
            last_hint: String::new(),
        }
    }
}

impl Default for HistoryHinter {
    fn default() -> Self {
        Self::new()
    }
}

impl Hinter for HistoryHinter {
    fn handle(
        &mut self,
        line: &str,
        pos: usize,
        history: &dyn History,
        _use_ansi_coloring: bool,
        _cwd: &str,
    ) -> String {
        // Only show hints when cursor is at the end of the line
        if line.is_empty() || pos != line.len() {
            self.last_hint.clear();
            return String::new();
        }

        debug!(line = %line, pos = pos, "Searching history for hint");

        // Search history for matching entries
        // We search for entries that start with the current line
        let search_result = history.search(reedline::SearchQuery::last_with_prefix(
            line.to_string(),
            None, // No session filtering
        ));

        match search_result {
            Ok(results) if !results.is_empty() => {
                // Get the most recent match
                let entry = &results[0].command_line;

                // Return only the suffix (part after current input)
                if entry.len() > line.len() && entry.starts_with(line) {
                    let suffix = &entry[line.len()..];
                    debug!(hint = %suffix, "Found history hint");
                    self.last_hint = suffix.to_string();
                    self.last_hint.clone()
                } else {
                    self.last_hint.clear();
                    String::new()
                }
            }
            Ok(_) => {
                debug!("No history match found");
                self.last_hint.clear();
                String::new()
            }
            Err(e) => {
                debug!(error = %e, "History search failed");
                self.last_hint.clear();
                String::new()
            }
        }
    }

    fn complete_hint(&self) -> String {
        // Returns the full hint for accepting with right arrow
        self.last_hint.clone()
    }

    fn next_hint_token(&self) -> String {
        // Returns the next word of the hint for partial acceptance (e.g., Ctrl+Right)
        if self.last_hint.is_empty() {
            return String::new();
        }

        // Find the first whitespace boundary to get the next token
        let trimmed = self.last_hint.trim_start();
        if trimmed.is_empty() {
            return self.last_hint.clone();
        }

        // Find end of first word (next whitespace or end of string)
        let token_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());

        // Include leading whitespace from original hint plus the token
        let leading_ws_len = self.last_hint.len() - trimmed.len();
        self.last_hint[..leading_ws_len + token_end].to_string()
    }
}

/// Creates a styled hint text for display.
///
/// This is a utility function that wraps hint text in the appropriate
/// ANSI styling for dimmed display.
pub fn style_hint(hint: &str) -> StyledText {
    let style = Style::new().fg(Color::DarkGray);
    let mut styled = StyledText::new();
    styled.push((style, hint.to_string()));
    styled
}

#[cfg(test)]
mod tests {
    use super::*;
    use reedline::{FileBackedHistory, History, HistoryItem};
    use tempfile::tempdir;

    /// Creates a test history with some entries.
    fn create_test_history() -> Box<dyn History> {
        let dir = tempdir().unwrap();
        let history_path = dir.path().join("test_history");

        let mut history = FileBackedHistory::with_file(1000, history_path).unwrap();

        // Add some test entries
        let entries = vec![
            "echo hello world",
            "echo goodbye",
            "ls -la",
            "git status",
            "git commit -m 'test'",
            "cargo build",
            "cargo test",
        ];

        for entry in entries {
            let item = HistoryItem::from_command_line(entry);
            history.save(item).unwrap();
        }

        Box::new(history)
    }

    // =========================================================================
    // HistoryHinter Tests
    // =========================================================================

    #[test]
    fn test_hinter_new() {
        let hinter = HistoryHinter::new();
        // Should create without panicking
        drop(hinter);
    }

    #[test]
    fn test_hinter_with_style() {
        let style = Style::new().fg(Color::Blue);
        let hinter = HistoryHinter::with_style(style);
        assert_eq!(hinter.style, style);
    }

    #[test]
    fn test_hinter_empty_line() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        let hint = hinter.handle("", 0, history.as_ref(), true, "");
        assert!(hint.is_empty(), "Empty line should not produce hint");
    }

    #[test]
    fn test_hinter_cursor_not_at_end() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // Cursor at position 2, but line is "echo" (length 4)
        let hint = hinter.handle("echo", 2, history.as_ref(), true, "");
        assert!(hint.is_empty(), "Cursor not at end should not produce hint");
    }

    #[test]
    fn test_hinter_finds_match() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // "echo h" should match "echo hello world"
        let hint = hinter.handle("echo h", 6, history.as_ref(), true, "");
        assert_eq!(hint, "ello world", "Should return suffix of matching entry");
    }

    #[test]
    fn test_hinter_finds_recent_match() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // "cargo " should match most recent cargo command ("cargo test")
        let hint = hinter.handle("cargo ", 6, history.as_ref(), true, "");
        assert_eq!(hint, "test", "Should return suffix of most recent match");
    }

    #[test]
    fn test_hinter_no_match() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // "xyz" should not match anything
        let hint = hinter.handle("xyz", 3, history.as_ref(), true, "");
        assert!(hint.is_empty(), "Non-matching input should return empty");
    }

    #[test]
    fn test_hinter_exact_match() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // Exact match should not produce hint (nothing to complete)
        let hint = hinter.handle("git status", 10, history.as_ref(), true, "");
        assert!(hint.is_empty(), "Exact match should return empty");
    }

    #[test]
    fn test_hinter_complete_hint_empty_when_no_hint() {
        let hinter = HistoryHinter::new();
        let result = hinter.complete_hint();
        assert!(result.is_empty(), "No hint should return empty");
    }

    #[test]
    fn test_hinter_complete_hint_returns_last_hint() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // Generate a hint first
        let hint = hinter.handle("echo h", 6, history.as_ref(), true, "");
        assert_eq!(hint, "ello world");

        // complete_hint should return the same hint
        let complete = hinter.complete_hint();
        assert_eq!(
            complete, "ello world",
            "complete_hint should return last hint"
        );
    }

    #[test]
    fn test_hinter_next_hint_token_empty_when_no_hint() {
        let hinter = HistoryHinter::new();
        let result = hinter.next_hint_token();
        assert!(result.is_empty(), "No hint should return empty");
    }

    #[test]
    fn test_hinter_next_hint_token_returns_first_word() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // Generate a hint with multiple words: "ello world"
        let hint = hinter.handle("echo h", 6, history.as_ref(), true, "");
        assert_eq!(hint, "ello world");

        // next_hint_token should return just the first word
        let token = hinter.next_hint_token();
        assert_eq!(token, "ello", "next_hint_token should return first word");
    }

    #[test]
    fn test_hinter_next_hint_token_single_word() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // Generate a hint with single word: "test"
        let hint = hinter.handle("cargo ", 6, history.as_ref(), true, "");
        assert_eq!(hint, "test");

        // next_hint_token should return the whole hint
        let token = hinter.next_hint_token();
        assert_eq!(token, "test", "Single word hint should return whole hint");
    }

    #[test]
    fn test_hinter_clears_hint_on_no_match() {
        let mut hinter = HistoryHinter::new();
        let history = create_test_history();

        // First generate a hint
        let hint = hinter.handle("echo h", 6, history.as_ref(), true, "");
        assert_eq!(hint, "ello world");
        assert!(!hinter.complete_hint().is_empty());

        // Now search for something with no match
        let hint = hinter.handle("xyz", 3, history.as_ref(), true, "");
        assert!(hint.is_empty());

        // last_hint should be cleared
        assert!(
            hinter.complete_hint().is_empty(),
            "Hint should be cleared on no match"
        );
    }

    // =========================================================================
    // style_hint Tests
    // =========================================================================

    #[test]
    fn test_style_hint() {
        let styled = style_hint("test hint");
        // Just verify it doesn't panic and creates styled text
        drop(styled);
    }

    #[test]
    fn test_style_hint_empty() {
        let styled = style_hint("");
        drop(styled);
    }
}
