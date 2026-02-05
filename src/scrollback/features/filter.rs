//! Filter state for showing only matching lines in scrollback.

/// State for filter mode.
///
/// When active, only lines matching the filter pattern are displayed.
/// Original line numbers are preserved (gaps visible).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilterState {
    /// Filter pattern (substring or regex).
    pub pattern: String,
    /// Cursor position in pattern input.
    pub cursor: usize,
    /// Indices of lines matching the filter (0-indexed).
    pub matching_lines: Vec<usize>,
    /// Whether filter is case-sensitive.
    pub case_sensitive: bool,
    /// Whether to use regex matching.
    pub use_regex: bool,
}

impl FilterState {
    /// Creates a new filter state with empty pattern.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if there are any matching lines.
    pub fn has_matches(&self) -> bool {
        !self.matching_lines.is_empty()
    }

    /// Returns the count of matching lines.
    pub fn match_count(&self) -> usize {
        self.matching_lines.len()
    }

    /// Returns true if the given line index passes the filter.
    pub fn line_visible(&self, line_index: usize) -> bool {
        self.matching_lines.binary_search(&line_index).is_ok()
    }

    /// Clears match results (called when pattern changes).
    pub fn clear_matches(&mut self) {
        self.matching_lines.clear();
    }

    /// Returns status string for display.
    pub fn status(&self) -> String {
        if self.pattern.is_empty() {
            String::new()
        } else if self.matching_lines.is_empty() {
            "No matches".to_string()
        } else {
            format!("{} lines", self.matching_lines.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_state_default() {
        let state = FilterState::default();
        assert!(state.pattern.is_empty());
        assert!(!state.has_matches());
    }

    #[test]
    fn test_line_visible() {
        let state = FilterState {
            matching_lines: vec![0, 5, 10, 15],
            ..Default::default()
        };

        assert!(state.line_visible(0));
        assert!(state.line_visible(5));
        assert!(!state.line_visible(3));
        assert!(!state.line_visible(100));
    }

    #[test]
    fn test_status() {
        let mut state = FilterState::default();
        assert_eq!(state.status(), "");

        state.pattern = "error".to_string();
        assert_eq!(state.status(), "No matches");

        state.matching_lines = vec![1, 5, 10];
        assert_eq!(state.status(), "3 lines");
    }
}
