//! Search state and logic for incremental search in scrollback.

use regex::{RegexBuilder, escape};

use crate::scrollback::buffer::ScrollbackBuffer;

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

    /// Returns true if a match is available.
    pub fn is_match_available(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Returns the current match if one is selected.
    pub fn current(&self) -> Option<&SearchMatch> {
        self.current_match.and_then(|idx| self.matches.get(idx))
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

    /// Performs search on the buffer and populates matches.
    ///
    /// This is called incrementally as the user types. It finds all
    /// occurrences of the query in the buffer and sets the current
    /// match to the one nearest to the given viewport position.
    ///
    /// Uses regex for case-insensitive matching to preserve correct byte offsets.
    ///
    /// # Arguments
    ///
    /// * `buffer` - The scrollback buffer to search
    /// * `near_line` - Line to find nearest match to (for initial selection)
    pub fn perform_search(&mut self, buffer: &ScrollbackBuffer, near_line: usize) {
        self.clear_matches();

        if self.query.is_empty() {
            return;
        }

        // Build regex pattern with escaped query for literal matching
        let escaped_query = escape(&self.query);
        let regex = match RegexBuilder::new(&escaped_query)
            .case_insensitive(!self.case_sensitive)
            .build()
        {
            Ok(r) => r,
            Err(_) => return, // Invalid regex (shouldn't happen with escaped query)
        };

        // Search all lines in buffer (sanitized to match rendered output)
        for (line_idx, line) in buffer.iter().enumerate() {
            let sanitized = super::super::ansi::sanitize_for_display(line.content());
            let content_str = String::from_utf8_lossy(&sanitized);

            for mat in regex.find_iter(&content_str) {
                self.matches.push(SearchMatch {
                    line: line_idx,
                    start: mat.start(),
                    end: mat.end(),
                });
            }
        }

        // Select nearest match to the viewport position
        if !self.matches.is_empty() {
            // Find match closest to near_line
            let nearest_idx = self
                .matches
                .iter()
                .enumerate()
                .min_by_key(|(_, m)| m.line.abs_diff(near_line))
                .map(|(idx, _)| idx)
                .unwrap_or(0);

            self.current_match = Some(nearest_idx);
        }
    }

    /// Returns matches on a specific line for highlighting.
    pub fn matches_on_line(&self, line_idx: usize) -> impl Iterator<Item = &SearchMatch> {
        self.matches.iter().filter(move |m| m.line == line_idx)
    }

    /// Jumps to the match nearest to the given line, preferring forward direction.
    pub fn jump_to_nearest(&mut self, from_line: usize) {
        if self.matches.is_empty() {
            return;
        }

        // Find first match at or after from_line
        let forward_idx = self.matches.iter().position(|m| m.line >= from_line);

        self.current_match = Some(forward_idx.unwrap_or(0));
    }

    /// Returns a sorted, deduplicated list of line indices that have matches.
    ///
    /// Used by filter mode to show only lines with search results.
    pub fn matched_line_indices(&self) -> Vec<usize> {
        let mut lines: Vec<usize> = self.matches.iter().map(|m| m.line).collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Performs search only within a subset of lines (e.g., filtered lines).
    ///
    /// This is used for filter+search combination where search only looks
    /// at lines that passed the filter.
    ///
    /// Uses regex for case-insensitive matching to preserve correct byte offsets.
    ///
    /// # Arguments
    ///
    /// * `buffer` - The scrollback buffer to search
    /// * `within_lines` - Line indices to search within
    /// * `near_line` - Line to find nearest match to (for initial selection)
    pub fn perform_search_within(
        &mut self,
        buffer: &ScrollbackBuffer,
        within_lines: &[usize],
        near_line: usize,
    ) {
        self.clear_matches();

        if self.query.is_empty() {
            return;
        }

        // Build regex pattern with escaped query for literal matching
        let escaped_query = escape(&self.query);
        let regex = match RegexBuilder::new(&escaped_query)
            .case_insensitive(!self.case_sensitive)
            .build()
        {
            Ok(r) => r,
            Err(_) => return, // Invalid regex (shouldn't happen with escaped query)
        };

        // Search only within the specified lines
        // Search within specified lines (sanitized to match rendered output)
        for &line_idx in within_lines {
            if let Some(line) = buffer.get(line_idx) {
                let sanitized = super::super::ansi::sanitize_for_display(line.content());
                let content_str = String::from_utf8_lossy(&sanitized);

                for mat in regex.find_iter(&content_str) {
                    self.matches.push(SearchMatch {
                        line: line_idx,
                        start: mat.start(),
                        end: mat.end(),
                    });
                }
            }
        }

        // Select nearest match to the viewport position
        if !self.matches.is_empty() {
            // Find match closest to near_line
            let nearest_idx = self
                .matches
                .iter()
                .enumerate()
                .min_by_key(|(_, m)| m.line.abs_diff(near_line))
                .map(|(idx, _)| idx)
                .unwrap_or(0);

            self.current_match = Some(nearest_idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_state_default() {
        let state = SearchState::default();
        assert!(state.query.is_empty());
        assert!(!state.is_match_available());
        assert!(state.current().is_none());
    }

    #[test]
    fn test_match_navigation() {
        let mut state = SearchState {
            query: "test".to_string(),
            matches: vec![
                SearchMatch {
                    line: 0,
                    start: 0,
                    end: 4,
                },
                SearchMatch {
                    line: 5,
                    start: 10,
                    end: 14,
                },
                SearchMatch {
                    line: 10,
                    start: 0,
                    end: 4,
                },
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

        state.matches.push(SearchMatch {
            line: 0,
            start: 0,
            end: 4,
        });
        state.current_match = Some(0);
        assert_eq!(state.status(), "1/1");
    }

    #[test]
    fn test_perform_search_basic() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"hello world".to_vec());
        buffer.push_line(b"another line".to_vec());
        buffer.push_line(b"hello again".to_vec());

        let mut state = SearchState::new();
        state.query = "hello".to_string();
        state.perform_search(&buffer, 0);

        assert_eq!(state.matches.len(), 2);
        assert_eq!(state.matches[0].line, 0);
        assert_eq!(state.matches[0].start, 0);
        assert_eq!(state.matches[0].end, 5);
        assert_eq!(state.matches[1].line, 2);
        assert!(state.current_match.is_some());
    }

    #[test]
    fn test_perform_search_case_insensitive() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"Hello World".to_vec());
        buffer.push_line(b"HELLO AGAIN".to_vec());

        let mut state = SearchState::new();
        state.query = "hello".to_string();
        state.case_sensitive = false; // default
        state.perform_search(&buffer, 0);

        assert_eq!(state.matches.len(), 2);
    }

    #[test]
    fn test_perform_search_case_sensitive() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"Hello World".to_vec());
        buffer.push_line(b"hello again".to_vec());

        let mut state = SearchState::new();
        state.query = "Hello".to_string();
        state.case_sensitive = true;
        state.perform_search(&buffer, 0);

        assert_eq!(state.matches.len(), 1);
        assert_eq!(state.matches[0].line, 0);
    }

    #[test]
    fn test_perform_search_multiple_matches_per_line() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"test test test".to_vec());

        let mut state = SearchState::new();
        state.query = "test".to_string();
        state.perform_search(&buffer, 0);

        assert_eq!(state.matches.len(), 3);
        assert_eq!(state.matches[0].start, 0);
        assert_eq!(state.matches[1].start, 5);
        assert_eq!(state.matches[2].start, 10);
    }

    #[test]
    fn test_perform_search_empty_query() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"hello world".to_vec());

        let mut state = SearchState::new();
        state.perform_search(&buffer, 0);

        assert!(state.matches.is_empty());
        assert!(state.current_match.is_none());
    }

    #[test]
    fn test_matches_on_line() {
        let mut state = SearchState::new();
        state.matches = vec![
            SearchMatch {
                line: 0,
                start: 0,
                end: 4,
            },
            SearchMatch {
                line: 0,
                start: 10,
                end: 14,
            },
            SearchMatch {
                line: 2,
                start: 5,
                end: 9,
            },
        ];

        let line0_matches: Vec<_> = state.matches_on_line(0).collect();
        assert_eq!(line0_matches.len(), 2);

        let line1_matches: Vec<_> = state.matches_on_line(1).collect();
        assert_eq!(line1_matches.len(), 0);

        let line2_matches: Vec<_> = state.matches_on_line(2).collect();
        assert_eq!(line2_matches.len(), 1);
    }

    #[test]
    fn test_nearest_match_selection() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        for i in 0..10 {
            buffer.push_line(format!("line {} test", i).into_bytes());
        }

        let mut state = SearchState::new();
        state.query = "test".to_string();
        state.perform_search(&buffer, 5); // Near line 5

        // Should select match closest to line 5
        assert!(state.current_match.is_some());
        let current = state.current().unwrap();
        assert_eq!(current.line, 5);
    }

    #[test]
    fn test_matched_line_indices() {
        let mut state = SearchState::new();
        // Add matches on lines 0, 0 (duplicate), 5, 10, 5 (duplicate)
        state.matches = vec![
            SearchMatch {
                line: 0,
                start: 0,
                end: 4,
            },
            SearchMatch {
                line: 0,
                start: 10,
                end: 14,
            },
            SearchMatch {
                line: 5,
                start: 0,
                end: 4,
            },
            SearchMatch {
                line: 10,
                start: 0,
                end: 4,
            },
            SearchMatch {
                line: 5,
                start: 10,
                end: 14,
            },
        ];

        let lines = state.matched_line_indices();
        // Should be sorted and deduplicated
        assert_eq!(lines, vec![0, 5, 10]);
    }

    #[test]
    fn test_perform_search_within() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"error: first timeout".to_vec()); // 0
        buffer.push_line(b"info: all good".to_vec()); // 1
        buffer.push_line(b"error: second failure".to_vec()); // 2
        buffer.push_line(b"warning: timeout warning".to_vec()); // 3
        buffer.push_line(b"error: third timeout".to_vec()); // 4

        // Only search within lines 0, 2, 4 (the error lines)
        let within_lines = vec![0, 2, 4];

        let mut state = SearchState::new();
        state.query = "timeout".to_string();
        state.perform_search_within(&buffer, &within_lines, 0);

        // Should find "timeout" only in lines 0 and 4 (not line 3 which isn't in within_lines)
        assert_eq!(state.matches.len(), 2);
        assert_eq!(state.matches[0].line, 0);
        assert_eq!(state.matches[1].line, 4);

        // Verify current match is set
        assert!(state.current_match.is_some());
    }
}
