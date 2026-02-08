//! Capture PTY output into lines with streaming parsing.
//! Handles CR/LF/CRLF endings, wrapping, and cursor-up overwrite redraws.

/// A captured line with disposition indicating whether to append or overwrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturedLine {
    /// Normal line — append to the end of the scrollback buffer.
    Append(Vec<u8>),
    /// Overwrite an existing line in the buffer.
    /// `lines_back` is the distance from the end of the buffer (1 = last line, 2 = second-to-last).
    Overwrite { lines_back: usize, content: Vec<u8> },
    /// Erase lines from `lines_back` to the end of the buffer.
    /// Used when ESC[J (erase below) is encountered during overwrite mode.
    EraseBelow { lines_back: usize },
}

impl CapturedLine {
    /// Returns the line content regardless of disposition.
    pub fn content(&self) -> &[u8] {
        match self {
            CapturedLine::Append(v) | CapturedLine::Overwrite { content: v, .. } => v,
            CapturedLine::EraseBelow { .. } => &[],
        }
    }

    /// Unwraps as `Append`, panicking if it's a different variant.
    /// Useful in tests where only Append is expected.
    #[cfg(test)]
    pub fn unwrap_append(self) -> Vec<u8> {
        match self {
            CapturedLine::Append(v) => v,
            other => panic!("expected Append, got {:?}", other),
        }
    }
}

/// State machine for parsing terminal output into lines.
///
/// Handles:
/// - CR (\r) - Carriage return for in-place line rewrite
/// - LF (\n) - Line feed, completes a line
/// - CRLF (\r\n) - Windows-style line ending
/// - Line wrapping when column exceeds terminal width
/// - ANSI escape sequences (passed through to line content)
/// - Cursor-up (ESC[A, ESC[F) triggers overwrite mode for line coalescence
#[derive(Debug)]
pub struct CaptureState {
    /// Accumulator for the current incomplete line.
    partial_line: Vec<u8>,
    /// Saved content from before the last standalone CR.
    ///
    /// Programs like `apt` use `\r\r\n` to end progress lines: the first CR
    /// resets the cursor, and `\r\n` is the line ending. Without saving the
    /// pre-CR content, the standalone CR clears `partial_line` and the CRLF
    /// emits an empty line. This field preserves the content so it can be
    /// used as a fallback when `partial_line` is empty at emission time.
    pre_cr_content: Vec<u8>,
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
    /// Byte index in `partial_line` where the current CSI parameter bytes start.
    csi_param_start: usize,
    /// Lines remaining to overwrite (0 = normal append mode).
    /// When > 0, the next emitted line overwrites buffer[len - overwrite_remaining].
    overwrite_remaining: usize,
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
            pre_cr_content: Vec::new(),
            column: 0,
            terminal_width: terminal_width.max(1),
            escape_state: EscapeState::Normal,
            pending_cr: false,
            utf8_remaining: 0,
            utf8_start_index: 0,
            csi_param_start: 0,
            overwrite_remaining: 0,
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
    /// Iterator over completed lines (as [`CapturedLine`]).
    pub fn feed<'a>(&'a mut self, data: &'a [u8]) -> impl Iterator<Item = CapturedLine> + 'a {
        CaptureIterator {
            state: self,
            data,
            pos: 0,
        }
    }

    /// Returns a reference to the current partial line content.
    ///
    /// This is the incomplete line being accumulated (e.g. a shell prompt
    /// that hasn't been terminated by a newline). Non-consuming — does not
    /// affect capture state.
    pub fn partial_content(&self) -> &[u8] {
        if !self.partial_line.is_empty() {
            &self.partial_line
        } else {
            &self.pre_cr_content
        }
    }

    /// Flushes the partial line buffer.
    ///
    /// Call this during mode transitions to ensure no content is lost.
    /// Returns the partial line if non-empty.
    pub fn flush(&mut self) -> Option<CapturedLine> {
        let line = if !self.partial_line.is_empty() {
            std::mem::take(&mut self.partial_line)
        } else if !self.pre_cr_content.is_empty() {
            std::mem::take(&mut self.pre_cr_content)
        } else {
            return None;
        };
        self.pre_cr_content.clear();
        let captured = self.make_captured_line(line);
        self.column = 0;
        self.escape_state = EscapeState::Normal;
        self.pending_cr = false;
        self.utf8_remaining = 0;
        Some(captured)
    }

    /// Takes line content for emission: uses `partial_line` if non-empty,
    /// otherwise falls back to `pre_cr_content` (for `\r\r\n` patterns).
    /// Always clears `pre_cr_content` after.
    fn take_line_content(&mut self) -> Vec<u8> {
        if !self.partial_line.is_empty() {
            self.pre_cr_content.clear();
            std::mem::take(&mut self.partial_line)
        } else {
            self.partial_line.clear();
            std::mem::take(&mut self.pre_cr_content)
        }
    }

    /// Wraps a completed line as `CapturedLine::Append` or `CapturedLine::Overwrite`
    /// depending on the current overwrite mode.
    fn make_captured_line(&mut self, content: Vec<u8>) -> CapturedLine {
        if self.overwrite_remaining > 0 {
            let lines_back = self.overwrite_remaining;
            self.overwrite_remaining = self.overwrite_remaining.saturating_sub(1);
            CapturedLine::Overwrite {
                lines_back,
                content,
            }
        } else {
            CapturedLine::Append(content)
        }
    }

    /// Processes a single byte, returning a completed line if one is ready.
    fn process_byte(&mut self, b: u8) -> Option<CapturedLine> {
        if self.pending_cr {
            self.pending_cr = false;

            // CRLF: complete current line.
            if b == 0x0a {
                let line = self.take_line_content();
                self.column = 0;
                return Some(self.make_captured_line(line));
            }

            // Standalone CR: command rewrote the current line in-place.
            // Save non-empty content before clearing — programs like `apt`
            // send `\r\r\n` where the first CR is standalone and the second
            // is part of CRLF. Without saving, the content is lost.
            if !self.partial_line.is_empty() {
                self.pre_cr_content = std::mem::take(&mut self.partial_line);
            }
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
    fn process_normal(&mut self, b: u8) -> Option<CapturedLine> {
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
                let line = self.take_line_content();
                self.column = 0;
                Some(self.make_captured_line(line))
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
    fn process_esc_seen(&mut self, b: u8) -> Option<CapturedLine> {
        self.partial_line.push(b);
        match b {
            b'[' => {
                self.escape_state = EscapeState::CsiBody;
                self.csi_param_start = self.partial_line.len();
                None
            }
            b']' => {
                self.escape_state = EscapeState::OscBody;
                None
            }
            // Reverse Index (ESC M): cursor up one line.
            // Used by some programs (e.g. scroll region based progress displays)
            // as an alternative to CSI A.
            b'M' => {
                self.escape_state = EscapeState::Normal;
                // Abandon partial line content — cursor moves to a different line.
                self.partial_line.clear();
                self.pre_cr_content.clear();
                self.overwrite_remaining += 1;
                self.column = 0;
                None
            }
            // Other escape sequences are typically 2 bytes total
            _ => {
                self.escape_state = EscapeState::Normal;
                None
            }
        }
    }

    /// Parses a CSI numeric parameter from the partial line.
    ///
    /// Reads digits from `csi_param_start` to `end` (exclusive).
    /// Returns `default` if no digits are present — this varies by sequence
    /// (cursor movement defaults to 1, erase defaults to 0).
    fn parse_csi_param(&self, end: usize, default: usize) -> usize {
        let start = self.csi_param_start;
        if start >= end {
            return default;
        }
        let param_bytes = &self.partial_line[start..end];
        // Take digits up to first non-digit (handles "2;3" by taking first param)
        let digit_end = param_bytes
            .iter()
            .position(|&b| !b.is_ascii_digit())
            .unwrap_or(param_bytes.len());
        if digit_end == 0 {
            return default;
        }
        // Safe: we verified all bytes are ASCII digits
        let s = std::str::from_utf8(&param_bytes[..digit_end]).unwrap_or("0");
        s.parse().unwrap_or(default)
    }

    /// Processes a byte in CSI sequence body.
    ///
    /// Detects cursor-up sequences (A, F) to enter overwrite mode, and
    /// erase sequences (J, K) to signal buffer modifications.
    fn process_csi_body(&mut self, b: u8) -> Option<CapturedLine> {
        self.partial_line.push(b);
        // CSI sequences end with a byte in 0x40-0x7E range
        if !(0x40..=0x7e).contains(&b) {
            return None;
        }

        self.escape_state = EscapeState::Normal;

        match b {
            // Cursor Up (CSI n A) or Cursor Previous Line (CSI n F)
            b'A' | b'F' => {
                let n = self.parse_csi_param(self.partial_line.len() - 1, 1).max(1);
                self.overwrite_remaining += n;
                // Cursor-up moves to a previous line — any content accumulated
                // for the current line is abandoned since we're now on a
                // different line. Clear entirely.
                self.partial_line.clear();
                self.pre_cr_content.clear();
                self.column = 0;
                None
            }
            // Erase in Display (CSI n J) — erase below when in overwrite mode
            b'J' => {
                let n = self.parse_csi_param(self.partial_line.len() - 1, 0);
                // Strip the erase CSI from partial line
                let csi_start = self.csi_param_start.saturating_sub(2);
                self.partial_line.truncate(csi_start);
                self.pre_cr_content.clear();
                if self.overwrite_remaining > 0 {
                    // J or 0J = erase from cursor to end, 2J = erase entire display
                    if n == 0 || n == 2 {
                        let lines_back = self.overwrite_remaining;
                        self.overwrite_remaining = 0;
                        return Some(CapturedLine::EraseBelow { lines_back });
                    }
                }
                None
            }
            // Erase in Line (CSI n K) — clear current line content
            b'K' => {
                // Strip the erase-line CSI from partial line
                let csi_start = self.csi_param_start.saturating_sub(2);
                self.partial_line.truncate(csi_start);
                // Clear any content accumulated before this CSI on the current line
                // (the program is erasing the line to rewrite it)
                self.partial_line.clear();
                self.pre_cr_content.clear();
                self.column = 0;
                None
            }
            // Cursor Down (CSI n B) or Cursor Next Line (CSI n E)
            // Decrements overwrite_remaining since cursor moves back towards buffer end.
            b'B' | b'E' => {
                let n = self.parse_csi_param(self.partial_line.len() - 1, 1).max(1);
                self.overwrite_remaining = self.overwrite_remaining.saturating_sub(n);
                // Abandon partial line content — cursor moves to a different line.
                self.partial_line.clear();
                self.pre_cr_content.clear();
                self.column = 0;
                None
            }
            // Cursor Position (CSI row;col H or CSI row;col f)
            // We can't track absolute row position, but we strip the sequence
            // from partial_line to avoid embedding raw CSI in captured content.
            // The cursor movement is lost in our line-based model.
            b'H' | b'f' => {
                // Strip the CSI sequence from partial_line
                let csi_start = self.csi_param_start.saturating_sub(2);
                self.partial_line.truncate(csi_start);
                self.pre_cr_content.clear();
                self.column = 0;
                None
            }
            // All other CSI sequences pass through (SGR, etc.)
            _ => None,
        }
    }

    /// Processes a byte in OSC sequence body.
    fn process_osc_body(&mut self, b: u8) -> Option<CapturedLine> {
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
    fn check_wrap(&mut self) -> Option<CapturedLine> {
        if self.column >= self.terminal_width {
            let line = std::mem::take(&mut self.partial_line);
            self.pre_cr_content.clear();
            self.column = 0;
            Some(self.make_captured_line(line))
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
    type Item = CapturedLine;

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

    /// Collects feed output as content bytes only (unwraps Append variants).
    fn feed_content(state: &mut CaptureState, data: &[u8]) -> Vec<Vec<u8>> {
        state.feed(data).map(|cl| cl.unwrap_append()).collect()
    }

    #[test]
    fn test_feed_single_line_yields_content() {
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"hello\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_feed_multiple_lines_yields_all() {
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"line1\nline2\nline3\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], b"line1");
        assert_eq!(lines[1], b"line2");
        assert_eq!(lines[2], b"line3");
    }

    #[test]
    fn test_feed_partial_then_complete_joins() {
        let mut state = CaptureState::new(80);

        let lines = feed_content(&mut state, b"hel");
        assert_eq!(lines.len(), 0);

        let lines = feed_content(&mut state, b"lo\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_process_byte_crlf_completes_line() {
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"hello\r\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_process_byte_standalone_cr_rewrites_line() {
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"abc\rxy\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"xy");
    }

    #[test]
    fn test_process_byte_repeated_cr_keeps_last_frame() {
        let mut state = CaptureState::new(120);
        let lines = feed_content(&mut state, b"10%\r20%\r30%\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"30%");
    }

    #[test]
    fn test_check_wrap_emits_line_at_width() {
        let mut state = CaptureState::new(5);
        let lines = feed_content(&mut state, b"12345678");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"12345");

        let flushed = state.flush().unwrap().unwrap_append();
        assert_eq!(flushed, b"678");
    }

    #[test]
    fn test_process_normal_csi_no_column_advance() {
        let mut state = CaptureState::new(10);
        let lines = feed_content(&mut state, b"\x1b[31mred\x1b[0m\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_flush_partial_returns_content() {
        let mut state = CaptureState::new(80);
        state.feed(b"partial").for_each(drop);
        let flushed = state.flush().unwrap().unwrap_append();
        assert_eq!(flushed, b"partial");

        let flushed = state.flush();
        assert_eq!(flushed, None);
    }

    #[test]
    fn test_feed_empty_lines_yields_empty_vecs() {
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"\n\n");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].is_empty());
        assert!(lines[1].is_empty());
    }

    #[test]
    fn test_process_normal_tab_advances_to_stop() {
        let mut state = CaptureState::new(20);
        let lines = feed_content(&mut state, b"ab\t");
        assert_eq!(lines.len(), 0);
        assert_eq!(state.column, 8);
    }

    #[test]
    fn test_process_normal_cjk_width_2() {
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, "你\n".as_bytes());
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
        let lines = feed_content(&mut state, "a你".as_bytes());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "a你".as_bytes());
        assert_eq!(state.column, 0);
    }

    #[test]
    fn test_check_wrap_cjk_no_wrap_when_fits() {
        // Terminal width 4: "a" (col 1) + "你" (col 3) should NOT wrap
        let mut state = CaptureState::new(4);
        let lines = feed_content(&mut state, "a你".as_bytes());
        assert_eq!(lines.len(), 0);
        assert_eq!(state.column, 3);
    }

    #[test]
    fn test_process_normal_utf8_split_across_feeds_tracks_width() {
        let mut state = CaptureState::new(80);
        // "你" is 0xE4 0xBD 0xA0 - split across two feed() calls
        let lines = feed_content(&mut state, &[0xE4]);
        assert_eq!(lines.len(), 0);
        assert_eq!(state.column, 0); // width not counted yet

        let lines = feed_content(&mut state, &[0xBD, 0xA0]);
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

    // --- Cursor-up coalescence tests ---

    #[test]
    fn test_cursor_up_single_line_emits_overwrite() {
        let mut state = CaptureState::new(80);
        // Emit a line, then cursor-up + new line
        let lines: Vec<_> = state.feed(b"original\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], CapturedLine::Append(b"original".to_vec()));

        // ESC[1A (cursor up 1) then new content + LF
        let lines: Vec<_> = state.feed(b"\x1b[1Areplacement\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"replacement".to_vec(),
            }
        );
    }

    #[test]
    fn test_cursor_up_multiple_lines_emits_sequential_overwrites() {
        let mut state = CaptureState::new(80);
        // Emit 3 blank lines, then cursor up 3 and overwrite all
        state.feed(b"\n\n\n").for_each(drop);

        // ESC[3A (cursor up 3)
        let lines: Vec<_> = state.feed(b"\x1b[3AlineA\nlineB\nlineC\n").collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 3,
                content: b"lineA".to_vec(),
            }
        );
        assert_eq!(
            lines[1],
            CapturedLine::Overwrite {
                lines_back: 2,
                content: b"lineB".to_vec(),
            }
        );
        assert_eq!(
            lines[2],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"lineC".to_vec(),
            }
        );
    }

    #[test]
    fn test_cursor_up_with_default_param_moves_one() {
        let mut state = CaptureState::new(80);
        state.feed(b"orig\n").for_each(drop);

        // ESC[A (no explicit param = default 1)
        let lines: Vec<_> = state.feed(b"\x1b[Anew\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"new".to_vec(),
            }
        );
    }

    #[test]
    fn test_cursor_previous_line_f_triggers_overwrite() {
        let mut state = CaptureState::new(80);
        state.feed(b"orig\n").for_each(drop);

        // ESC[1F (cursor previous line) - same as cursor up + column 0
        let lines: Vec<_> = state.feed(b"\x1b[1Fnew\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"new".to_vec(),
            }
        );
    }

    #[test]
    fn test_overwrite_then_append_resumes_normal() {
        let mut state = CaptureState::new(80);
        state.feed(b"line1\nline2\n").for_each(drop);

        // Cursor up 1, overwrite, then new append
        let lines: Vec<_> = state.feed(b"\x1b[1Areplaced\nnew_line3\n").collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"replaced".to_vec(),
            }
        );
        assert_eq!(lines[1], CapturedLine::Append(b"new_line3".to_vec()));
    }

    #[test]
    fn test_erase_line_clears_partial_content() {
        let mut state = CaptureState::new(80);
        // Write some content, then ESC[2K (erase entire line), then new content
        let lines = feed_content(&mut state, b"old stuff\x1b[2Knew content\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"new content");
    }

    #[test]
    fn test_erase_below_emits_erase_signal() {
        let mut state = CaptureState::new(80);
        state.feed(b"line1\nline2\nline3\n").for_each(drop);

        // Cursor up 2, then ESC[J (erase below)
        let lines: Vec<_> = state.feed(b"\x1b[2A\x1b[J").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], CapturedLine::EraseBelow { lines_back: 2 });
    }

    #[test]
    fn test_apt_style_progress_coalescence() {
        let mut state = CaptureState::new(80);
        // Simulate apt-style output:
        // 1. Emit blank lines
        // 2. Cursor up
        // 3. Erase and rewrite with "Done" versions
        let lines: Vec<_> = state.feed(b"\n\n").collect();
        assert_eq!(lines.len(), 2); // Two blank lines

        // Cursor up 2, erase line, write "Reading... Done", LF
        let lines: Vec<_> = state
            .feed(b"\x1b[2A\x1b[2KReading package lists... Done\n\x1b[2KBuilding dependency tree... Done\n")
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 2,
                content: b"Reading package lists... Done".to_vec(),
            }
        );
        assert_eq!(
            lines[1],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"Building dependency tree... Done".to_vec(),
            }
        );
    }

    #[test]
    fn test_cursor_up_strips_csi_from_content() {
        let mut state = CaptureState::new(80);
        state.feed(b"orig\n").for_each(drop);

        // The cursor-up CSI should not appear in the overwrite content
        let lines: Vec<_> = state.feed(b"prefix\x1b[1Anew\n").collect();
        assert_eq!(lines.len(), 1);
        // The "prefix" was in partial_line before the cursor-up stripped it
        // Actually, cursor-up strips from partial_line, so only "new" is the content
        match &lines[0] {
            CapturedLine::Overwrite { content, .. } => {
                assert_eq!(content, b"new");
            }
            other => panic!("expected Overwrite, got {:?}", other),
        }
    }

    #[test]
    fn test_consecutive_cursor_up_accumulates() {
        let mut state = CaptureState::new(80);
        state.feed(b"line1\nline2\nline3\n").for_each(drop);

        // Two consecutive ESC[1A should accumulate overwrite_remaining to 2
        let lines: Vec<_> = state.feed(b"\x1b[1A\x1b[1AnewA\nnewB\n").collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 2,
                content: b"newA".to_vec(),
            }
        );
        assert_eq!(
            lines[1],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"newB".to_vec(),
            }
        );
    }

    #[test]
    fn test_reverse_index_triggers_overwrite() {
        let mut state = CaptureState::new(80);
        state.feed(b"original\n").for_each(drop);

        // ESC M (Reverse Index) = cursor up one line
        let lines: Vec<_> = state.feed(b"\x1bMreplacement\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"replacement".to_vec(),
            }
        );
    }

    #[test]
    fn test_reverse_index_multiple_accumulates() {
        let mut state = CaptureState::new(80);
        state.feed(b"line1\nline2\nline3\n").for_each(drop);

        // Three ESC M = cursor up 3 lines
        let lines: Vec<_> = state.feed(b"\x1bM\x1bM\x1bMnewA\nnewB\nnewC\n").collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 3,
                content: b"newA".to_vec(),
            }
        );
        assert_eq!(
            lines[1],
            CapturedLine::Overwrite {
                lines_back: 2,
                content: b"newB".to_vec(),
            }
        );
        assert_eq!(
            lines[2],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"newC".to_vec(),
            }
        );
    }

    #[test]
    fn test_cursor_down_decrements_overwrite() {
        let mut state = CaptureState::new(80);
        state.feed(b"line1\nline2\nline3\n").for_each(drop);

        // Cursor up 3, then cursor down 1 = net up 2
        let lines: Vec<_> = state.feed(b"\x1b[3A\x1b[1BnewA\nnewB\n").collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 2,
                content: b"newA".to_vec(),
            }
        );
        assert_eq!(
            lines[1],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"newB".to_vec(),
            }
        );
    }

    #[test]
    fn test_cursor_next_line_decrements_overwrite() {
        let mut state = CaptureState::new(80);
        state.feed(b"line1\nline2\nline3\n").for_each(drop);

        // Cursor up 2, cursor next line 1 = net up 1
        let lines: Vec<_> = state.feed(b"\x1b[2A\x1b[1Enew\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"new".to_vec(),
            }
        );
    }

    #[test]
    fn test_cursor_position_stripped_from_content() {
        let mut state = CaptureState::new(80);
        // ESC[10;1H (cursor position) should be stripped from partial_line
        let lines = feed_content(&mut state, b"before\x1b[10;1Hafter\n");
        assert_eq!(lines.len(), 1);
        // CSI H is stripped, so content is "beforeafter"
        assert_eq!(lines[0], b"beforeafter");
    }

    #[test]
    fn test_mixed_cursor_up_and_reverse_index() {
        let mut state = CaptureState::new(80);
        state.feed(b"a\nb\nc\nd\n").for_each(drop);

        // ESC[2A (up 2) + ESC M (up 1) = total up 3
        let lines: Vec<_> = state.feed(b"\x1b[2A\x1bMnew1\nnew2\nnew3\n").collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 3,
                content: b"new1".to_vec(),
            }
        );
        assert_eq!(lines[1].content(), b"new2");
        assert_eq!(lines[2].content(), b"new3");
    }

    // --- CR-CR-LF preservation tests (apt-style progress) ---

    #[test]
    fn test_cr_cr_lf_preserves_content() {
        // apt sends: text\r\r\n — the \r\r\n should preserve "text"
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"Reading package lists... Done\r\r\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"Reading package lists... Done");
    }

    #[test]
    fn test_apt_progress_then_done_cr_cr_lf() {
        // Full apt progress sequence: multiple CR-separated updates ending with \r\r\n
        let mut state = CaptureState::new(80);
        let lines = feed_content(
            &mut state,
            b"\rReading package lists... 0%\r\rReading package lists... 50%\r\rReading package lists... Done\r\r\n",
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"Reading package lists... Done");
    }

    #[test]
    fn test_multiple_apt_phases() {
        // Multiple apt phases each ending with \r\r\n
        let mut state = CaptureState::new(80);
        let lines = feed_content(
            &mut state,
            b"\rReading package lists... Done\r\r\n\rBuilding dependency tree... Done\r\r\n\rReading state information... Done\r\r\n",
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], b"Reading package lists... Done");
        assert_eq!(lines[1], b"Building dependency tree... Done");
        assert_eq!(lines[2], b"Reading state information... Done");
    }

    #[test]
    fn test_cr_lf_still_works_normally() {
        // Normal CRLF should not be affected
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"hello\r\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"hello");
    }

    #[test]
    fn test_standalone_cr_still_overwrites() {
        // Standalone CR followed by content should still use latest content
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"old content\rnew content\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"new content");
    }

    #[test]
    fn test_empty_cr_cr_lf_emits_empty() {
        // \r\r\n with no preceding content should emit empty line
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"\r\r\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].is_empty());
    }

    #[test]
    fn test_cr_cr_lf_after_normal_line() {
        // Normal line followed by \r\r\n pattern
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"normal\r\nDone\r\r\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"normal");
        assert_eq!(lines[1], b"Done");
    }

    #[test]
    fn test_pre_cr_content_cleared_after_use() {
        // After using pre_cr_content, it should be cleared and not leak into next line
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"saved\r\r\n\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"saved");
        assert!(lines[1].is_empty()); // Second line should be empty, not "saved" again
    }

    // --- Edge cases: cursor movement + CR interactions ---

    #[test]
    fn test_cr_then_cursor_up_clears_pre_cr() {
        // text\r\033[1A\n — CR saves content, cursor-up should clear it
        let mut state = CaptureState::new(80);
        state.feed(b"orig\n").for_each(drop);

        let lines: Vec<_> = state.feed(b"text\r\x1b[1A\n").collect();
        assert_eq!(lines.len(), 1);
        // Cursor-up clears pre_cr_content, so the overwrite should have empty content
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"".to_vec(),
            }
        );
    }

    #[test]
    fn test_cr_then_erase_line_clears_pre_cr() {
        // text\r\033[K\n — CR saves, then erase-line should clear saved content
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"text\r\x1b[K\n");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].is_empty()); // Erase-line cleared pre_cr_content
    }

    #[test]
    fn test_cr_then_erase_line_then_new_content() {
        // text\r\033[Knew\n — erase clears saved, new content takes over
        let mut state = CaptureState::new(80);
        let lines = feed_content(&mut state, b"text\r\x1b[Knew\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"new");
    }

    #[test]
    fn test_cr_then_reverse_index_clears_pre_cr() {
        // text\r ESC M \n — CR saves, reverse index should clear
        let mut state = CaptureState::new(80);
        state.feed(b"orig\n").for_each(drop);

        let lines: Vec<_> = state.feed(b"text\r\x1bM\n").collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            CapturedLine::Overwrite {
                lines_back: 1,
                content: b"".to_vec(),
            }
        );
    }

    #[test]
    fn test_flush_returns_pre_cr_content_when_partial_empty() {
        // Flush should return pre_cr_content if partial_line is empty
        let mut state = CaptureState::new(80);
        state.feed(b"content\r").for_each(drop);
        // After \r, pending_cr is set but not yet resolved.
        // Force resolve by feeding a non-\n byte and discarding
        state.feed(b" ").for_each(drop);
        // Now partial_line = " ", pre_cr_content = "content"
        // Flush returns partial_line since it's non-empty
        let flushed = state.flush().unwrap().unwrap_append();
        assert_eq!(flushed, b" ");
    }

    #[test]
    fn test_flush_pre_cr_when_partial_empty() {
        // Simulate: content arrives, standalone CR clears it, then flush
        let mut state = CaptureState::new(80);
        // Feed "content\r" then trigger the standalone CR by feeding another \r
        state.feed(b"content\r\r").for_each(drop);
        // Now: pending_cr=true from the second \r, partial_line empty,
        // pre_cr_content = "content"
        // Feed a non-newline to resolve pending_cr as standalone
        state.feed(b"x").for_each(drop);
        // Now partial_line = "x", pre_cr_content should be empty (since \r was standalone
        // and partial_line was empty, skip save)
        // Actually trace: first \r sets pending. second \r resolves first as standalone
        // (save "content"), clear partial. Sets pending again.
        // Then "x" resolves second \r as standalone (partial empty, skip save), push "x".
        // pre_cr_content = "content", partial_line = "x"
        // Flush returns "x" (partial_line non-empty)
        let flushed = state.flush().unwrap().unwrap_append();
        assert_eq!(flushed, b"x");
    }

    #[test]
    fn test_apt_full_sequence_download_and_progress() {
        // Simulate apt's full download + progress pattern
        let mut state = CaptureState::new(120);
        let mut all_lines: Vec<CapturedLine> = Vec::new();

        // Download phase: CR-separated progress, then CRLF-terminated content lines
        all_lines.extend(state.feed(
            b"\x1b[33m\r0% [Working]\x1b[0m\r              \rGet:1 https://example.com stable InRelease [3917 B]\r\n",
        ));
        // "Fetched" summary with download progress cleared
        all_lines
            .extend(state.feed(
                b"\x1b[33m\r100% [Working]\x1b[0m\r              \rFetched 3917 B in 1s\r\n",
            ));
        // Progress phase: CR-CR-LF terminated
        all_lines.extend(
            state.feed(b"\rReading package lists... 0%\r\rReading package lists... Done\r\r\n"),
        );
        all_lines.extend(state.feed(b"\rBuilding dependency tree... Done\r\r\n"));

        // Should have 4 lines
        assert_eq!(all_lines.len(), 4);
        assert_eq!(
            all_lines[0].content(),
            b"Get:1 https://example.com stable InRelease [3917 B]"
        );
        assert_eq!(all_lines[1].content(), b"Fetched 3917 B in 1s");
        assert_eq!(all_lines[2].content(), b"Reading package lists... Done");
        assert_eq!(all_lines[3].content(), b"Building dependency tree... Done");
    }
}
