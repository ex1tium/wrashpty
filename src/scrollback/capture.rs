//! PTY output capture and line parsing.
//!
//! This module provides streaming line parsing for terminal output.
//! It handles CR, LF, CRLF line endings and terminal line wrapping.

/// State machine for parsing terminal output into lines.
///
/// Handles:
/// - CR (\r) - Carriage return, resets column position
/// - LF (\n) - Line feed, completes a line
/// - CRLF (\r\n) - Windows-style line ending
/// - Line wrapping when column exceeds terminal width
/// - ANSI escape sequences (passed through to line content)
#[derive(Debug)]
pub struct CaptureState {
    /// Accumulator for the current incomplete line.
    partial_line: Vec<u8>,
    /// Current column position (0-based).
    column: u16,
    /// Terminal width for wrap detection.
    terminal_width: u16,
    /// State for escape sequence parsing.
    escape_state: EscapeState,
}

/// State for tracking escape sequence parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeState {
    /// Normal character processing.
    Normal,
    /// Just saw ESC (0x1b).
    EscSeen,
    /// Inside CSI sequence (ESC [).
    CsiBody,
    /// Inside OSC sequence (ESC ]).
    OscBody,
}

impl CaptureState {
    /// Creates a new capture state with the given terminal width.
    pub fn new(terminal_width: u16) -> Self {
        Self {
            partial_line: Vec::with_capacity(256),
            column: 0,
            terminal_width: terminal_width.max(1),
            escape_state: EscapeState::Normal,
        }
    }

    /// Updates the terminal width for wrap detection.
    pub fn set_terminal_width(&mut self, width: u16) {
        self.terminal_width = width.max(1);
    }

    /// Returns the current terminal width.
    #[inline]
    pub fn terminal_width(&self) -> u16 {
        self.terminal_width
    }

    /// Feeds bytes and yields completed lines.
    ///
    /// Call this with each chunk of PTY output. Returns an iterator
    /// over completed lines. Lines may be completed by:
    /// - LF or CRLF line ending
    /// - Line wrap (column exceeds terminal width)
    ///
    /// # Arguments
    ///
    /// * `data` - Raw bytes from PTY output
    ///
    /// # Returns
    ///
    /// Iterator over completed lines (as Vec<u8>).
    pub fn feed<'a>(&'a mut self, data: &'a [u8]) -> impl Iterator<Item = Vec<u8>> + 'a {
        CaptureIterator {
            state: self,
            data,
            pos: 0,
        }
    }

    /// Flushes the partial line buffer.
    ///
    /// Call this during mode transitions to ensure no content is lost.
    /// Returns the partial line if non-empty.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.partial_line.is_empty() {
            None
        } else {
            let line = std::mem::take(&mut self.partial_line);
            self.column = 0;
            self.escape_state = EscapeState::Normal;
            Some(line)
        }
    }

    /// Processes a single byte, returning a completed line if one is ready.
    fn process_byte(&mut self, b: u8) -> Option<Vec<u8>> {
        match self.escape_state {
            EscapeState::Normal => self.process_normal(b),
            EscapeState::EscSeen => self.process_esc_seen(b),
            EscapeState::CsiBody => self.process_csi_body(b),
            EscapeState::OscBody => self.process_osc_body(b),
        }
    }

    /// Processes a byte in normal mode.
    fn process_normal(&mut self, b: u8) -> Option<Vec<u8>> {
        match b {
            // ESC - start escape sequence
            0x1b => {
                self.partial_line.push(b);
                self.escape_state = EscapeState::EscSeen;
                None
            }
            // LF - complete line
            0x0a => {
                let line = std::mem::take(&mut self.partial_line);
                self.column = 0;
                Some(line)
            }
            // CR - carriage return
            0x0d => {
                // CR typically resets column, we include it in the line
                // but don't count it towards display width
                self.partial_line.push(b);
                self.column = 0;
                None
            }
            // TAB - advance column to next tab stop
            0x09 => {
                self.partial_line.push(b);
                let tab_width = 8 - (self.column % 8);
                self.column = self.column.saturating_add(tab_width);
                self.check_wrap()
            }
            // BEL - bell, no width
            0x07 => {
                self.partial_line.push(b);
                None
            }
            // Backspace - move column back
            0x08 => {
                self.partial_line.push(b);
                self.column = self.column.saturating_sub(1);
                None
            }
            // Printable ASCII
            b if (0x20..=0x7e).contains(&b) => {
                self.partial_line.push(b);
                self.column = self.column.saturating_add(1);
                self.check_wrap()
            }
            // UTF-8 start bytes
            b if b >= 0xc0 => {
                self.partial_line.push(b);
                // Simplified: count as width 1, actual width depends on character
                self.column = self.column.saturating_add(1);
                self.check_wrap()
            }
            // UTF-8 continuation bytes or control chars
            _ => {
                self.partial_line.push(b);
                // Continuation bytes don't add width
                None
            }
        }
    }

    /// Processes a byte after seeing ESC.
    fn process_esc_seen(&mut self, b: u8) -> Option<Vec<u8>> {
        self.partial_line.push(b);
        match b {
            b'[' => {
                self.escape_state = EscapeState::CsiBody;
                None
            }
            b']' => {
                self.escape_state = EscapeState::OscBody;
                None
            }
            // Other escape sequences are typically 2 bytes total
            _ => {
                self.escape_state = EscapeState::Normal;
                None
            }
        }
    }

    /// Processes a byte in CSI sequence body.
    fn process_csi_body(&mut self, b: u8) -> Option<Vec<u8>> {
        self.partial_line.push(b);
        // CSI sequences end with a byte in 0x40-0x7E range
        if (0x40..=0x7e).contains(&b) {
            self.escape_state = EscapeState::Normal;
        }
        None
    }

    /// Processes a byte in OSC sequence body.
    fn process_osc_body(&mut self, b: u8) -> Option<Vec<u8>> {
        self.partial_line.push(b);
        // OSC sequences end with BEL (0x07) or ST (ESC \)
        if b == 0x07 {
            self.escape_state = EscapeState::Normal;
        } else if b == 0x1b {
            // Might be ST, but we need to see the next byte
            // For simplicity, we'll handle this by staying in OscBody
            // and checking for backslash
        } else if b == b'\\' && self.partial_line.len() >= 2 {
            // Check if previous was ESC
            let len = self.partial_line.len();
            if self.partial_line[len - 2] == 0x1b {
                self.escape_state = EscapeState::Normal;
            }
        }
        None
    }

    /// Checks if line should wrap and returns completed line if so.
    fn check_wrap(&mut self) -> Option<Vec<u8>> {
        if self.column >= self.terminal_width {
            let line = std::mem::take(&mut self.partial_line);
            self.column = 0;
            Some(line)
        } else {
            None
        }
    }
}

/// Iterator adapter for feeding bytes and yielding lines.
struct CaptureIterator<'a> {
    state: &'a mut CaptureState,
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for CaptureIterator<'a> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            if let Some(line) = self.state.process_byte(b) {
                return Some(line);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_line() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"hello\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_multiple_lines() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"line1\nline2\nline3\n").collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], b"line1");
        assert_eq!(lines[1], b"line2");
        assert_eq!(lines[2], b"line3");
    }

    #[test]
    fn test_partial_then_complete() {
        let mut state = CaptureState::new(80);

        let lines: Vec<_> = state.feed(b"hel").collect();
        assert_eq!(lines.len(), 0);

        let lines: Vec<_> = state.feed(b"lo\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_crlf() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"hello\r\n").collect();
        assert_eq!(lines.len(), 1);
        // CR is included in the line content
        assert_eq!(lines[0], b"hello\r");
    }

    #[test]
    fn test_line_wrap() {
        let mut state = CaptureState::new(5);
        let lines: Vec<_> = state.feed(b"12345678").collect();
        // Should wrap at column 5
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"12345");

        // Rest is still in buffer
        let flushed = state.flush();
        assert_eq!(flushed, Some(b"678".to_vec()));
    }

    #[test]
    fn test_csi_sequence_no_wrap() {
        let mut state = CaptureState::new(10);
        // CSI sequence should not count towards column width
        let lines: Vec<_> = state.feed(b"\x1b[31mred\x1b[0m\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_flush_partial() {
        let mut state = CaptureState::new(80);
        state.feed(b"partial").for_each(drop);
        let flushed = state.flush();
        assert_eq!(flushed, Some(b"partial".to_vec()));

        // Flush again should return None
        let flushed = state.flush();
        assert_eq!(flushed, None);
    }

    #[test]
    fn test_empty_line() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"\n\n").collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].is_empty());
        assert!(lines[1].is_empty());
    }

    #[test]
    fn test_tab_width() {
        let mut state = CaptureState::new(20);
        // Tab should advance to next 8-column boundary
        let lines: Vec<_> = state.feed(b"ab\t").collect();
        assert_eq!(lines.len(), 0);
        // 2 chars + 6 spaces = 8 columns
        assert_eq!(state.column, 8);
    }
}
