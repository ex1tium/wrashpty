//! Reusable inline filter widget for panels.
//!
//! Provides a simple text input for filtering list items by case-insensitive
//! substring match. Can be embedded in any panel that needs a `/`-activated
//! filter mode.

use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::Span;

use crate::chrome::theme::Theme;

/// State for an inline filter input.
pub struct FilterInput {
    /// Current filter text.
    text: String,
    /// Lowercased version of `text`, cached for efficient matching.
    text_lower: String,
    /// Whether the filter is currently active (accepting input).
    active: bool,
}

impl FilterInput {
    /// Creates a new inactive filter with no text.
    pub fn new() -> Self {
        Self {
            text: String::new(),
            text_lower: String::new(),
            active: false,
        }
    }

    /// Returns the current filter text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns true if the filter has any text.
    pub fn has_filter(&self) -> bool {
        !self.text.is_empty()
    }

    /// Returns true if the filter is active (accepting input).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Activates the filter input.
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// Deactivates the filter input, keeping the text.
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Deactivates and clears the filter text.
    pub fn clear_and_deactivate(&mut self) {
        self.text.clear();
        self.text_lower.clear();
        self.active = false;
    }

    /// Clears the filter text without changing active state.
    pub fn clear(&mut self) {
        self.text.clear();
        self.text_lower.clear();
    }

    /// Types a character into the filter.
    pub fn type_char(&mut self, c: char) {
        self.text.push(c);
        // Extend the cached lowercase with the lowercased char
        for lc in c.to_lowercase() {
            self.text_lower.push(lc);
        }
    }

    /// Deletes the last character. Returns true if text is now empty.
    pub fn backspace(&mut self) -> bool {
        self.text.pop();
        // Rebuild the cache (pop doesn't map 1:1 for multi-byte lowercase)
        self.text_lower = self.text.to_lowercase();
        self.text.is_empty()
    }

    /// Returns true if `text` matches the current filter.
    ///
    /// Uses case-insensitive substring matching. An empty filter matches everything.
    pub fn matches(&self, text: &str) -> bool {
        if self.text.is_empty() {
            return true;
        }
        text.to_lowercase().contains(&self.text_lower)
    }

    /// Returns styled spans for rendering the filter bar.
    ///
    /// When active: `" / pattern_"` (with cursor indicator).
    /// When inactive with text: `" / pattern"`.
    pub fn render_spans<'a>(&'a self, theme: &Theme) -> Vec<Span<'a>> {
        let key_style = Style::default().fg(theme.text_highlight);
        let text_style = Style::default()
            .fg(theme.header_fg)
            .add_modifier(Modifier::BOLD);

        let mut spans = vec![
            Span::styled(" / ", key_style),
            Span::styled(self.text.as_str(), text_style),
        ];

        if self.active {
            spans.push(Span::styled("█", Style::default().fg(theme.text_highlight)));
        }

        spans
    }
}

impl Default for FilterInput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state() {
        let f = FilterInput::new();
        assert!(!f.is_active());
        assert!(!f.has_filter());
        assert_eq!(f.text(), "");
    }

    #[test]
    fn test_activate_deactivate() {
        let mut f = FilterInput::new();
        f.activate();
        assert!(f.is_active());
        f.deactivate();
        assert!(!f.is_active());
    }

    #[test]
    fn test_type_and_backspace() {
        let mut f = FilterInput::new();
        f.type_char('a');
        f.type_char('b');
        assert_eq!(f.text(), "ab");
        assert!(f.has_filter());

        let empty = f.backspace();
        assert!(!empty);
        assert_eq!(f.text(), "a");

        let empty = f.backspace();
        assert!(empty);
        assert_eq!(f.text(), "");
        assert!(!f.has_filter());
    }

    #[test]
    fn test_clear_and_deactivate() {
        let mut f = FilterInput::new();
        f.activate();
        f.type_char('x');
        f.clear_and_deactivate();
        assert!(!f.is_active());
        assert!(!f.has_filter());
    }

    #[test]
    fn test_matches_empty_filter() {
        let f = FilterInput::new();
        assert!(f.matches("anything"));
        assert!(f.matches(""));
    }

    #[test]
    fn test_matches_case_insensitive() {
        let mut f = FilterInput::new();
        f.type_char('h');
        f.type_char('e');
        f.type_char('l');
        assert!(f.matches("Hello"));
        assert!(f.matches("HELLO"));
        assert!(f.matches("shell"));
        assert!(!f.matches("world"));
    }

    #[test]
    fn test_matches_substring() {
        let mut f = FilterInput::new();
        f.type_char('r');
        f.type_char('s');
        assert!(f.matches("main.rs"));
        assert!(f.matches("lib.rs"));
        assert!(!f.matches("main.py"));
    }

    #[test]
    fn test_backspace_on_empty() {
        let mut f = FilterInput::new();
        let empty = f.backspace();
        assert!(empty);
        assert_eq!(f.text(), "");
    }

    #[test]
    fn test_clear_keeps_active_state() {
        let mut f = FilterInput::new();
        f.activate();
        f.type_char('x');
        f.clear();
        assert!(f.is_active());
        assert!(!f.has_filter());
    }
}
