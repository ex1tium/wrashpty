//! Command boundary tracking for navigating between command outputs.
//!
//! This module tracks where commands start and end in the scrollback buffer,
//! enabling Ctrl+P/N navigation to jump between command outputs.

use crate::types::MarkerEvent;

/// Index of command boundaries for Ctrl+P/N navigation.
///
/// Tracks line indices where commands start (after Preexec) and where
/// prompts appear (after Precmd). This enables jumping between command
/// outputs - a unique feature not found in standard terminal emulators.
#[derive(Debug, Clone, Default)]
pub struct CommandBoundaries {
    /// Line indices where command output starts (right after Preexec marker).
    /// These mark the beginning of a command's output.
    pub command_starts: Vec<usize>,

    /// Line indices where prompts appear (after Precmd/Prompt markers).
    /// These mark the end of a command's output and the start of the next prompt.
    pub prompt_lines: Vec<usize>,
}

impl CommandBoundaries {
    /// Creates a new empty boundary index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a marker event at the given buffer line index.
    ///
    /// Call this from the PTY pump loop when a marker is detected.
    /// The line_index should be the current buffer length at the time
    /// the marker is received.
    pub fn record_marker(&mut self, event: &MarkerEvent, line_index: usize) {
        match event {
            MarkerEvent::Preexec => {
                // Command is about to execute - next lines are command output
                self.command_starts.push(line_index);
            }
            MarkerEvent::Precmd { .. } | MarkerEvent::Prompt => {
                // Command finished - prompt is about to appear
                self.prompt_lines.push(line_index);
            }
        }
    }

    /// Finds the previous command start before the given line.
    ///
    /// Returns the line index of the most recent command output start
    /// that is before `from_line`. Used for Ctrl+P navigation.
    pub fn prev_command(&self, from_line: usize) -> Option<usize> {
        self.command_starts
            .iter()
            .rev()
            .find(|&&line| line < from_line)
            .copied()
    }

    /// Finds the next command start after the given line.
    ///
    /// Returns the line index of the next command output start
    /// that is after `from_line`. Used for Ctrl+N navigation.
    pub fn next_command(&self, from_line: usize) -> Option<usize> {
        self.command_starts
            .iter()
            .find(|&&line| line > from_line)
            .copied()
    }

    /// Finds the previous prompt line before the given line.
    pub fn prev_prompt(&self, from_line: usize) -> Option<usize> {
        self.prompt_lines
            .iter()
            .rev()
            .find(|&&line| line < from_line)
            .copied()
    }

    /// Finds the next prompt line after the given line.
    pub fn next_prompt(&self, from_line: usize) -> Option<usize> {
        self.prompt_lines
            .iter()
            .find(|&&line| line > from_line)
            .copied()
    }

    /// Returns the total count of recorded command starts.
    pub fn command_count(&self) -> usize {
        self.command_starts.len()
    }

    /// Returns true if there are any command boundaries recorded.
    pub fn has_boundaries(&self) -> bool {
        !self.command_starts.is_empty()
    }

    /// Clears all recorded boundaries.
    /// Call this when the scrollback buffer is cleared.
    pub fn clear(&mut self) {
        self.command_starts.clear();
        self.prompt_lines.clear();
    }

    /// Adjusts all indices after a buffer trim operation.
    ///
    /// When the scrollback buffer drops old lines, call this with
    /// the number of lines dropped to keep indices valid.
    pub fn adjust_for_dropped_lines(&mut self, dropped_count: usize) {
        // Remove boundaries that are now out of range
        self.command_starts.retain(|&line| line >= dropped_count);
        self.prompt_lines.retain(|&line| line >= dropped_count);

        // Adjust remaining indices
        for line in &mut self.command_starts {
            *line -= dropped_count;
        }
        for line in &mut self.prompt_lines {
            *line -= dropped_count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_markers() {
        let mut bounds = CommandBoundaries::new();

        bounds.record_marker(&MarkerEvent::Preexec, 10);
        bounds.record_marker(&MarkerEvent::Precmd { exit_code: 0 }, 20);
        bounds.record_marker(&MarkerEvent::Preexec, 25);
        bounds.record_marker(&MarkerEvent::Precmd { exit_code: 1 }, 50);

        assert_eq!(bounds.command_starts, vec![10, 25]);
        assert_eq!(bounds.prompt_lines, vec![20, 50]);
    }

    #[test]
    fn test_prev_next_command() {
        let mut bounds = CommandBoundaries::new();
        bounds.command_starts = vec![10, 30, 60, 100];

        // Previous command from middle
        assert_eq!(bounds.prev_command(50), Some(30));
        assert_eq!(bounds.prev_command(30), Some(10));
        assert_eq!(bounds.prev_command(10), None);

        // Next command from middle
        assert_eq!(bounds.next_command(20), Some(30));
        assert_eq!(bounds.next_command(60), Some(100));
        assert_eq!(bounds.next_command(100), None);
    }

    #[test]
    fn test_adjust_for_dropped_lines() {
        let mut bounds = CommandBoundaries::new();
        bounds.command_starts = vec![10, 30, 60];
        bounds.prompt_lines = vec![20, 50];

        // Drop first 25 lines
        bounds.adjust_for_dropped_lines(25);

        // 10 and 20 should be removed, others adjusted
        assert_eq!(bounds.command_starts, vec![5, 35]); // 30-25=5, 60-25=35
        assert_eq!(bounds.prompt_lines, vec![25]); // 50-25=25
    }

    #[test]
    fn test_clear() {
        let mut bounds = CommandBoundaries::new();
        bounds.command_starts = vec![10, 30];
        bounds.prompt_lines = vec![20];

        bounds.clear();
        assert!(bounds.command_starts.is_empty());
        assert!(bounds.prompt_lines.is_empty());
    }
}
