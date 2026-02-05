//! Go-to-line state for jumping to specific line numbers.

/// State for go-to-line mode.
///
/// Shows a mini-input prompt for entering a line number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoToLineState {
    /// Line number input buffer.
    pub input: String,
    /// Cursor position in input.
    pub cursor: usize,
}

impl GoToLineState {
    /// Creates a new go-to-line state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses the input as a line number.
    /// Returns None if input is empty or invalid.
    pub fn line_number(&self) -> Option<usize> {
        if self.input.is_empty() {
            return None;
        }
        self.input.parse::<usize>().ok()
    }

    /// Returns true if the input is valid (empty or valid number).
    pub fn is_valid(&self) -> bool {
        self.input.is_empty() || self.line_number().is_some()
    }

    /// Appends a character to input (only digits allowed).
    pub fn push_char(&mut self, c: char) {
        if c.is_ascii_digit() {
            self.input.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    /// Removes character before cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.input.remove(self.cursor);
        }
    }

    /// Clears the input.
    pub fn clear(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_goto_state_default() {
        let state = GoToLineState::default();
        assert!(state.input.is_empty());
        assert_eq!(state.line_number(), None);
    }

    #[test]
    fn test_line_number_parsing() {
        let mut state = GoToLineState::default();

        state.input = "42".to_string();
        assert_eq!(state.line_number(), Some(42));

        state.input = "abc".to_string();
        assert_eq!(state.line_number(), None);

        state.input = "".to_string();
        assert_eq!(state.line_number(), None);
    }

    #[test]
    fn test_push_char() {
        let mut state = GoToLineState::default();

        state.push_char('1');
        state.push_char('2');
        state.push_char('3');
        assert_eq!(state.input, "123");

        // Non-digits ignored
        state.push_char('a');
        assert_eq!(state.input, "123");
    }

    #[test]
    fn test_backspace() {
        let mut state = GoToLineState {
            input: "123".to_string(),
            cursor: 3,
        };

        state.backspace();
        assert_eq!(state.input, "12");
        assert_eq!(state.cursor, 2);

        state.backspace();
        state.backspace();
        assert_eq!(state.input, "");
        assert_eq!(state.cursor, 0);

        // Backspace on empty does nothing
        state.backspace();
        assert_eq!(state.cursor, 0);
    }
}
