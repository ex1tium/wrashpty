//! PTY output capture and line parsing.
//!
//! This module provides streaming line parsing for terminal output.
//! It handles CR, LF, CRLF line endings and terminal line wrapping.

/// State machine for parsing terminal output into lines.
///
/// Handles:
/// - CR (\r) - Carriage return for in-place line rewrite
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
    /// Whether the previous byte was CR and needs disambiguation from CRLF.
    pending_cr: bool,
    /// Number of UTF-8 continuation bytes still expected for the current character.
    utf8_remaining: u8,
    /// Byte index in `partial_line` where the current multi-byte UTF-8 char starts.
    utf8_start_index: usize,
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
            pending_cr: false,
            utf8_remaining: 0,
            utf8_start_index: 0,
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
            self.pending_cr = false;
            self.utf8_remaining = 0;
            Some(line)
        }
    }

    /// Processes a single byte, returning a completed line if one is ready.
    fn process_byte(&mut self, b: u8) -> Option<Vec<u8>> {
        if self.pending_cr {
            self.pending_cr = false;

            // CRLF: complete current line.
            if b == 0x0a {
                let line = std::mem::take(&mut self.partial_line);
                self.column = 0;
                return Some(line);
            }

            // Standalone CR: command rewrote the current line in-place.
            // Keep only the latest frame to avoid progress-line artifacts.
            self.partial_line.clear();
            self.column = 0;
            self.utf8_remaining = 0;
        }

        match self.escape_state {
            EscapeState::Normal => self.process_normal(b),
            EscapeState::EscSeen => self.process_esc_seen(b),
            EscapeState::CsiBody => self.process_csi_body(b),
            EscapeState::OscBody => self.process_osc_body(b),
        }
    }

    /// Processes a byte in normal mode.
    fn process_normal(&mut self, b: u8) -> Option<Vec<u8>> {
        // If we're mid-UTF-8 sequence and this byte is NOT a continuation byte,
        // the sequence is broken. Account for the incomplete bytes as width 1
        // (replacement character) and reset state before processing this byte.
        if self.utf8_remaining > 0 && (b & 0xC0) != 0x80 {
            self.utf8_remaining = 0;
            self.column = self.column.saturating_add(1);
            // Don't return - fall through to process this byte normally
        }

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
                // Defer handling until we see the next byte:
                // - if next is LF => CRLF newline
                // - otherwise => standalone CR (in-place rewrite)
                self.pending_cr = true;
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
                let seq_len = super::ansi::utf8_sequence_len(b);
                self.utf8_start_index = self.partial_line.len();
                self.partial_line.push(b);
                if seq_len <= 1 {
                    // Invalid start byte treated as width 1
                    self.column = self.column.saturating_add(1);
                    self.check_wrap()
                } else {
                    self.utf8_remaining = (seq_len - 1) as u8;
                    None // wait for continuation bytes
                }
            }
            // UTF-8 continuation bytes or control chars
            _ => {
                self.partial_line.push(b);
                if self.utf8_remaining > 0 && (b & 0xC0) == 0x80 {
                    self.utf8_remaining -= 1;
                    if self.utf8_remaining == 0 {
                        // Complete UTF-8 sequence: decode and measure display width
                        let start = self.utf8_start_index;
                        let w = if let Ok(s) = std::str::from_utf8(&self.partial_line[start..]) {
                            s.chars()
                                .next()
                                .and_then(unicode_width::UnicodeWidthChar::width)
                                .unwrap_or(1) as u16
                        } else {
                            1 // invalid UTF-8 fallback
                        };
                        self.column = self.column.saturating_add(w);
                        return self.check_wrap();
                    }
                }
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
    fn test_feed_single_line_yields_content() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"hello\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_feed_multiple_lines_yields_all() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"line1\nline2\nline3\n").collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], b"line1");
        assert_eq!(lines[1], b"line2");
        assert_eq!(lines[2], b"line3");
    }

    #[test]
    fn test_feed_partial_then_complete_joins() {
        let mut state = CaptureState::new(80);

        let lines: Vec<_> = state.feed(b"hel").collect();
        assert_eq!(lines.len(), 0);

        let lines: Vec<_> = state.feed(b"lo\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_process_byte_crlf_completes_line() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"hello\r\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_process_byte_standalone_cr_rewrites_line() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"abc\rxy\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"xy");
    }

    #[test]
    fn test_process_byte_repeated_cr_keeps_last_frame() {
        let mut state = CaptureState::new(120);
        let lines: Vec<_> = state.feed(b"10%\r20%\r30%\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"30%");
    }

    #[test]
    fn test_check_wrap_emits_line_at_width() {
        let mut state = CaptureState::new(5);
        let lines: Vec<_> = state.feed(b"12345678").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"12345");

        let flushed = state.flush();
        assert_eq!(flushed, Some(b"678".to_vec()));
    }

    #[test]
    fn test_process_normal_csi_no_column_advance() {
        let mut state = CaptureState::new(10);
        let lines: Vec<_> = state.feed(b"\x1b[31mred\x1b[0m\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_flush_partial_returns_content() {
        let mut state = CaptureState::new(80);
        state.feed(b"partial").for_each(drop);
        let flushed = state.flush();
        assert_eq!(flushed, Some(b"partial".to_vec()));

        let flushed = state.flush();
        assert_eq!(flushed, None);
    }

    #[test]
    fn test_feed_empty_lines_yields_empty_vecs() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed(b"\n\n").collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].is_empty());
        assert!(lines[1].is_empty());
    }

    #[test]
    fn test_process_normal_tab_advances_to_stop() {
        let mut state = CaptureState::new(20);
        let lines: Vec<_> = state.feed(b"ab\t").collect();
        assert_eq!(lines.len(), 0);
        assert_eq!(state.column, 8);
    }

    #[test]
    fn test_process_normal_cjk_width_2() {
        let mut state = CaptureState::new(80);
        let lines: Vec<_> = state.feed("你\n".as_bytes()).collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "你".as_bytes());
    }

    #[test]
    fn test_process_normal_cjk_column_tracks_width_2() {
        let mut state = CaptureState::new(80);
        // "a" (width 1) + "你" (width 2) = column 3
        state.feed("a你".as_bytes()).for_each(drop);
        assert_eq!(state.column, 3);
    }

    #[test]
    fn test_check_wrap_cjk_wraps_at_exact_width() {
        // Terminal width 3: "a" (col 1) + "你" (col 3) should trigger wrap
        let mut state = CaptureState::new(3);
        let lines: Vec<_> = state.feed("a你".as_bytes()).collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "a你".as_bytes());
        assert_eq!(state.column, 0);
    }

    #[test]
    fn test_check_wrap_cjk_no_wrap_when_fits() {
        // Terminal width 4: "a" (col 1) + "你" (col 3) should NOT wrap
        let mut state = CaptureState::new(4);
        let lines: Vec<_> = state.feed("a你".as_bytes()).collect();
        assert_eq!(lines.len(), 0);
        assert_eq!(state.column, 3);
    }

    #[test]
    fn test_process_normal_utf8_split_across_feeds_tracks_width() {
        let mut state = CaptureState::new(80);
        // "你" is 0xE4 0xBD 0xA0 - split across two feed() calls
        let lines: Vec<_> = state.feed(&[0xE4]).collect();
        assert_eq!(lines.len(), 0);
        assert_eq!(state.column, 0); // width not counted yet

        let lines: Vec<_> = state.feed(&[0xBD, 0xA0]).collect();
        assert_eq!(lines.len(), 0);
        assert_eq!(state.column, 2); // now width 2 is counted
    }

    #[test]
    fn test_process_normal_two_byte_utf8_width_1() {
        let mut state = CaptureState::new(80);
        // "é" is U+00E9, 2-byte UTF-8: 0xC3 0xA9, display width 1
        state.feed("é".as_bytes()).for_each(drop);
        assert_eq!(state.column, 1);
    }

    #[test]
    fn test_process_normal_broken_utf8_resets_and_counts_replacement() {
        let mut state = CaptureState::new(80);
        // Start a 3-byte UTF-8 sequence (0xE4) but follow with ASCII 'x' instead
        // of continuation bytes. The broken sequence should count as width 1,
        // then 'x' should count as width 1.
        state.feed(&[0xE4, b'x']).for_each(drop);
        assert_eq!(state.column, 2); // 1 (replacement) + 1 ('x')
        assert_eq!(state.utf8_remaining, 0);
    }
}
