//! Search state and logic for incremental search in scrollback.

/// Direction for search navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchDirection {
    #[default]
    Forward,
    Backward,
}

/// Match location in the scrollback buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// Line index in buffer (0-indexed).
    pub line: usize,
    /// Byte offset start within line content.
    pub start: usize,
    /// Byte offset end within line content.
    pub end: usize,
}

/// State for incremental search mode.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchState {
    /// Current search query.
    pub query: String,
    /// Cursor position in query input.
    pub cursor: usize,
    /// All match positions in buffer.
    pub matches: Vec<SearchMatch>,
    /// Current match index for navigation (if any matches exist).
    pub current_match: Option<usize>,
    /// Search direction for Ctrl+N/P navigation.
    pub direction: SearchDirection,
    /// Whether search is case-sensitive.
    pub case_sensitive: bool,
}

impl SearchState {
    /// Creates a new search state with empty query.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if there are any matches.
    pub fn has_matches(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Returns the current match if one is selected.
    pub fn current(&self) -> Option<&SearchMatch> {
        self.current_match
            .and_then(|idx| self.matches.get(idx))
    }

    /// Moves to the next match in current direction.
    pub fn next_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }

        let count = self.matches.len();
        self.current_match = Some(match self.current_match {
            Some(idx) => match self.direction {
                SearchDirection::Forward => (idx + 1) % count,
                SearchDirection::Backward => (idx + count - 1) % count,
            },
            None => 0,
        });
    }

    /// Moves to the previous match (opposite of current direction).
    pub fn prev_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }

        let count = self.matches.len();
        self.current_match = Some(match self.current_match {
            Some(idx) => match self.direction {
                SearchDirection::Forward => (idx + count - 1) % count,
                SearchDirection::Backward => (idx + 1) % count,
            },
            None => count - 1,
        });
    }

    /// Returns match status string for display (e.g., "3/47").
    pub fn status(&self) -> String {
        if self.matches.is_empty() {
            if self.query.is_empty() {
                String::new()
            } else {
                "No matches".to_string()
            }
        } else {
            match self.current_match {
                Some(idx) => format!("{}/{}", idx + 1, self.matches.len()),
                None => format!("0/{}", self.matches.len()),
            }
        }
    }

    /// Clears matches (called when query changes, before re-searching).
    pub fn clear_matches(&mut self) {
        self.matches.clear();
        self.current_match = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_state_default() {
        let state = SearchState::default();
        assert!(state.query.is_empty());
        assert!(!state.has_matches());
        assert!(state.current().is_none());
    }

    #[test]
    fn test_match_navigation() {
        let mut state = SearchState {
            query: "test".to_string(),
            matches: vec![
                SearchMatch { line: 0, start: 0, end: 4 },
                SearchMatch { line: 5, start: 10, end: 14 },
                SearchMatch { line: 10, start: 0, end: 4 },
            ],
            current_match: Some(0),
            ..Default::default()
        };

        state.next_match();
        assert_eq!(state.current_match, Some(1));

        state.next_match();
        assert_eq!(state.current_match, Some(2));

        state.next_match();
        assert_eq!(state.current_match, Some(0)); // Wraps

        state.prev_match();
        assert_eq!(state.current_match, Some(2)); // Wraps back
    }

    #[test]
    fn test_status_display() {
        let mut state = SearchState::default();
        assert_eq!(state.status(), "");

        state.query = "test".to_string();
        assert_eq!(state.status(), "No matches");

        state.matches.push(SearchMatch { line: 0, start: 0, end: 4 });
        state.current_match = Some(0);
        assert_eq!(state.status(), "1/1");
    }
}
