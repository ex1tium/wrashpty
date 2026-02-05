//! Yank/copy mode state for selecting and copying text.

/// Selection mode variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// Line-wise selection (V in vim).
    #[default]
    Line,
    /// Character-wise selection (v in vim) - future enhancement.
    Character,
}

/// State for yank/copy mode.
///
/// Allows visual selection of lines for copying to clipboard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct YankState {
    /// Anchor line where selection started (0-indexed buffer line).
    pub anchor_line: usize,
    /// Current line (selection extends from anchor to current).
    pub current_line: usize,
    /// Whether selection is active (V pressed).
    pub selection_active: bool,
    /// Selection mode (line or character).
    pub mode: SelectionMode,
}

impl YankState {
    /// Creates a new yank state starting at the given line.
    pub fn new(start_line: usize) -> Self {
        Self {
            anchor_line: start_line,
            current_line: start_line,
            selection_active: false,
            mode: SelectionMode::Line,
        }
    }

    /// Toggles selection on/off.
    pub fn toggle_selection(&mut self) {
        self.selection_active = !self.selection_active;
        if self.selection_active {
            // Reset anchor to current when starting selection
            self.anchor_line = self.current_line;
        }
    }

    /// Returns the selection range as (start, end) inclusive.
    /// Returns None if selection is not active.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        if !self.selection_active {
            return None;
        }

        let start = self.anchor_line.min(self.current_line);
        let end = self.anchor_line.max(self.current_line);
        Some((start, end))
    }

    /// Returns true if the given line is within the selection.
    pub fn line_selected(&self, line: usize) -> bool {
        match self.selection_range() {
            Some((start, end)) => line >= start && line <= end,
            None => false,
        }
    }

    /// Moves the current line up by one.
    pub fn move_up(&mut self) {
        self.current_line = self.current_line.saturating_sub(1);
    }

    /// Moves the current line down by one (capped at max_line).
    pub fn move_down(&mut self, max_line: usize) {
        if self.current_line < max_line {
            self.current_line += 1;
        }
    }

    /// Returns selection status for display.
    pub fn status(&self) -> String {
        match self.selection_range() {
            Some((start, end)) => {
                let count = end - start + 1;
                if count == 1 {
                    "1 line".to_string()
                } else {
                    format!("{} lines", count)
                }
            }
            None => "No selection".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yank_state_new() {
        let state = YankState::new(10);
        assert_eq!(state.anchor_line, 10);
        assert_eq!(state.current_line, 10);
        assert!(!state.selection_active);
    }

    #[test]
    fn test_toggle_selection() {
        let mut state = YankState::new(5);
        state.current_line = 10;

        state.toggle_selection();
        assert!(state.selection_active);
        assert_eq!(state.anchor_line, 10); // Anchor moves to current

        state.toggle_selection();
        assert!(!state.selection_active);
    }

    #[test]
    fn test_selection_range() {
        let mut state = YankState::new(5);
        state.selection_active = true;
        state.anchor_line = 5;
        state.current_line = 10;

        assert_eq!(state.selection_range(), Some((5, 10)));

        // Selection in reverse direction
        state.anchor_line = 10;
        state.current_line = 5;
        assert_eq!(state.selection_range(), Some((5, 10)));
    }

    #[test]
    fn test_line_selected() {
        let mut state = YankState::new(5);
        state.selection_active = true;
        state.anchor_line = 5;
        state.current_line = 10;

        assert!(state.line_selected(5));
        assert!(state.line_selected(7));
        assert!(state.line_selected(10));
        assert!(!state.line_selected(4));
        assert!(!state.line_selected(11));
    }
}
