//! Filter state for showing only matching lines in scrollback.

use crate::scrollback::buffer::ScrollbackBuffer;

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

    /// Performs filtering on the buffer and populates matching_lines.
    ///
    /// This is called incrementally as the user types. It finds all
    /// lines that contain the filter pattern.
    ///
    /// # Arguments
    ///
    /// * `buffer` - The scrollback buffer to filter
    pub fn perform_filter(&mut self, buffer: &ScrollbackBuffer) {
        self.clear_matches();

        if self.pattern.is_empty() {
            return;
        }

        // Convert pattern for case-insensitive matching if needed
        let pattern_lower = if self.case_sensitive {
            None
        } else {
            Some(self.pattern.to_lowercase())
        };
        let search_pattern = pattern_lower.as_deref().unwrap_or(&self.pattern);

        // Find all matching lines
        for (line_idx, line) in buffer.iter().enumerate() {
            let content = line.content();
            let content_str = String::from_utf8_lossy(content);
            let search_str = if self.case_sensitive {
                content_str.to_string()
            } else {
                content_str.to_lowercase()
            };

            if search_str.contains(search_pattern) {
                self.matching_lines.push(line_idx);
            }
        }
    }

    /// Performs filtering within a subset of lines (e.g., search results).
    ///
    /// This is used for filter+search combination where we only filter
    /// lines that were found by a previous search.
    ///
    /// # Arguments
    ///
    /// * `buffer` - The scrollback buffer
    /// * `base_lines` - Line indices to filter within
    pub fn perform_filter_within(&mut self, buffer: &ScrollbackBuffer, base_lines: &[usize]) {
        self.clear_matches();

        if self.pattern.is_empty() {
            // If pattern is empty, show all base lines
            self.matching_lines = base_lines.to_vec();
            return;
        }

        // Convert pattern for case-insensitive matching if needed
        let pattern_lower = if self.case_sensitive {
            None
        } else {
            Some(self.pattern.to_lowercase())
        };
        let search_pattern = pattern_lower.as_deref().unwrap_or(&self.pattern);

        // Find matching lines within the base set
        for &line_idx in base_lines {
            if let Some(line) = buffer.get(line_idx) {
                let content = line.content();
                let content_str = String::from_utf8_lossy(content);
                let search_str = if self.case_sensitive {
                    content_str.to_string()
                } else {
                    content_str.to_lowercase()
                };

                if search_str.contains(search_pattern) {
                    self.matching_lines.push(line_idx);
                }
            }
        }
    }

    /// Returns an iterator of (line_index, line) for filtered lines.
    ///
    /// Use this to render only the matching lines while preserving
    /// their original line numbers.
    pub fn filtered_lines<'a>(
        &'a self,
        buffer: &'a ScrollbackBuffer,
    ) -> impl Iterator<Item = (usize, &'a crate::scrollback::buffer::ScrollLine)> {
        self.matching_lines
            .iter()
            .filter_map(move |&idx| buffer.get(idx).map(|line| (idx, line)))
    }

    /// Returns visible lines within a viewport range for filtered display.
    ///
    /// # Arguments
    ///
    /// * `buffer` - The scrollback buffer
    /// * `offset` - Lines from bottom (in filtered view)
    /// * `count` - Number of lines to retrieve
    ///
    /// # Returns
    ///
    /// Vector of (original_line_index, ScrollLine) pairs.
    pub fn get_filtered_range<'a>(
        &'a self,
        buffer: &'a ScrollbackBuffer,
        offset: usize,
        count: usize,
    ) -> Vec<(usize, &'a crate::scrollback::buffer::ScrollLine)> {
        let total = self.matching_lines.len();
        if offset >= total {
            return Vec::new();
        }

        let end_from_start = total.saturating_sub(offset);
        let start_from_start = end_from_start.saturating_sub(count);

        self.matching_lines
            .iter()
            .skip(start_from_start)
            .take(count.min(end_from_start - start_from_start))
            .filter_map(|&idx| buffer.get(idx).map(|line| (idx, line)))
            .collect()
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

    #[test]
    fn test_perform_filter_basic() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"error: something failed".to_vec());
        buffer.push_line(b"info: all good".to_vec());
        buffer.push_line(b"error: another failure".to_vec());
        buffer.push_line(b"warning: maybe".to_vec());

        let mut state = FilterState::new();
        state.pattern = "error".to_string();
        state.perform_filter(&buffer);

        assert_eq!(state.match_count(), 2);
        assert!(state.line_visible(0));
        assert!(!state.line_visible(1));
        assert!(state.line_visible(2));
        assert!(!state.line_visible(3));
    }

    #[test]
    fn test_perform_filter_case_insensitive() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"ERROR: uppercase".to_vec());
        buffer.push_line(b"error: lowercase".to_vec());
        buffer.push_line(b"Error: mixed".to_vec());

        let mut state = FilterState::new();
        state.pattern = "error".to_string();
        state.case_sensitive = false; // default
        state.perform_filter(&buffer);

        assert_eq!(state.match_count(), 3);
    }

    #[test]
    fn test_perform_filter_case_sensitive() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"ERROR: uppercase".to_vec());
        buffer.push_line(b"error: lowercase".to_vec());

        let mut state = FilterState::new();
        state.pattern = "ERROR".to_string();
        state.case_sensitive = true;
        state.perform_filter(&buffer);

        assert_eq!(state.match_count(), 1);
        assert!(state.line_visible(0));
        assert!(!state.line_visible(1));
    }

    #[test]
    fn test_perform_filter_empty_pattern() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"line 1".to_vec());
        buffer.push_line(b"line 2".to_vec());

        let mut state = FilterState::new();
        state.perform_filter(&buffer);

        assert!(state.matching_lines.is_empty());
    }

    #[test]
    fn test_get_filtered_range() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        for i in 0..10 {
            let content = if i % 2 == 0 {
                format!("line {} match", i)
            } else {
                format!("line {} no", i)
            };
            buffer.push_line(content.into_bytes());
        }

        let mut state = FilterState::new();
        state.pattern = "match".to_string();
        state.perform_filter(&buffer);

        // Should have 5 matching lines: 0, 2, 4, 6, 8
        assert_eq!(state.match_count(), 5);

        // Get last 2 lines (offset 0, count 2)
        let lines = state.get_filtered_range(&buffer, 0, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0, 6); // Original line index
        assert_eq!(lines[1].0, 8);

        // Get first 2 lines (max offset)
        let lines = state.get_filtered_range(&buffer, 3, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].0, 0);
        assert_eq!(lines[1].0, 2);
    }

    #[test]
    fn test_perform_filter_within() {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        buffer.push_line(b"error: first failure".to_vec()); // 0
        buffer.push_line(b"info: all good".to_vec()); // 1
        buffer.push_line(b"error: second failure".to_vec()); // 2
        buffer.push_line(b"warning: something".to_vec()); // 3
        buffer.push_line(b"error: third failure".to_vec()); // 4

        // Base set: only lines 0, 2, 4 (the error lines)
        let base_lines = vec![0, 2, 4];

        let mut state = FilterState::new();

        // Empty pattern should return all base lines
        state.pattern = String::new();
        state.perform_filter_within(&buffer, &base_lines);
        assert_eq!(state.matching_lines, vec![0, 2, 4]);

        // Filter within base lines for "second"
        state.pattern = "second".to_string();
        state.perform_filter_within(&buffer, &base_lines);
        assert_eq!(state.matching_lines, vec![2]);

        // Filter within base lines for "failure" (all match)
        state.pattern = "failure".to_string();
        state.perform_filter_within(&buffer, &base_lines);
        assert_eq!(state.matching_lines, vec![0, 2, 4]);

        // Filter within base lines for "not found"
        state.pattern = "not found".to_string();
        state.perform_filter_within(&buffer, &base_lines);
        assert!(state.matching_lines.is_empty());
    }
}
