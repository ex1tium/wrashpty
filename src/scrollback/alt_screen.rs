//! Alternate screen buffer detection.
//!
//! Detects when applications like vim or htop enter/exit the alternate
//! screen buffer, so we can suspend/resume scrollback capture accordingly.
//!
//! # Detected Sequences
//!
//! - `\x1b[?1049h` - DECSET: Enable alternate screen buffer (xterm)
//! - `\x1b[?1049l` - DECRST: Disable alternate screen buffer (xterm)
//! - `\x1b[?47h` - DECSET: Enable alternate screen buffer (older)
//! - `\x1b[?47l` - DECRST: Disable alternate screen buffer (older)

/// Events emitted when alternate screen buffer state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltScreenEvent {
    /// Application entered alternate screen buffer.
    Enter,
    /// Application exited alternate screen buffer.
    Exit,
}

/// State machine for CSI parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsiState {
    /// Normal byte processing.
    Normal,
    /// Just saw ESC (0x1b).
    EscSeen,
    /// Inside CSI body (after ESC [).
    CsiBody,
}

/// Streaming detector for alternate screen buffer sequences.
///
/// Feed bytes one at a time and check for enter/exit events.
/// This is O(1) per byte with no heap allocations.
#[derive(Debug)]
pub struct AltScreenDetector {
    /// Current parser state.
    state: CsiState,
    /// Buffer for CSI parameters (fixed-size, no allocation).
    param_buf: [u8; 16],
    /// Current position in param_buf.
    param_pos: usize,
    /// Whether we're currently in alternate screen.
    in_alt_screen: bool,
}

impl AltScreenDetector {
    /// Creates a new detector.
    pub fn new() -> Self {
        Self {
            state: CsiState::Normal,
            param_buf: [0; 16],
            param_pos: 0,
            in_alt_screen: false,
        }
    }

    /// Returns whether we're currently in alternate screen buffer.
    #[inline]
    pub fn is_in_alt_screen(&self) -> bool {
        self.in_alt_screen
    }

    /// Feeds a single byte and returns an event if state changed.
    ///
    /// This is the hot path - called for every byte of PTY output.
    /// Designed to be O(1) and inline-friendly.
    #[inline]
    pub fn feed_byte(&mut self, b: u8) -> Option<AltScreenEvent> {
        match self.state {
            CsiState::Normal => {
                if b == 0x1b {
                    self.state = CsiState::EscSeen;
                }
                None
            }
            CsiState::EscSeen => {
                if b == b'[' {
                    self.state = CsiState::CsiBody;
                    self.param_pos = 0;
                } else {
                    self.state = CsiState::Normal;
                }
                None
            }
            CsiState::CsiBody => self.process_csi_byte(b),
        }
    }

    /// Feeds multiple bytes and returns events.
    ///
    /// Convenience method for processing chunks.
    pub fn feed<'a>(&'a mut self, data: &'a [u8]) -> impl Iterator<Item = AltScreenEvent> + 'a {
        AltScreenIterator {
            detector: self,
            data,
            pos: 0,
        }
    }

    /// Processes a byte in CSI body.
    fn process_csi_byte(&mut self, b: u8) -> Option<AltScreenEvent> {
        // CSI parameter bytes: 0x30-0x3F (digits, semicolon, ?, etc.)
        // CSI intermediate bytes: 0x20-0x2F
        // CSI final bytes: 0x40-0x7E

        if (0x30..=0x3f).contains(&b) {
            // Parameter byte - accumulate
            if self.param_pos < self.param_buf.len() {
                self.param_buf[self.param_pos] = b;
                self.param_pos += 1;
            }
            None
        } else if (0x40..=0x7e).contains(&b) {
            // Final byte - check for our sequences
            let event = self.check_sequence(b);
            self.state = CsiState::Normal;
            event
        } else {
            // Intermediate byte or invalid - ignore and continue
            None
        }
    }

    /// Checks if the accumulated sequence is an alt-screen toggle.
    fn check_sequence(&mut self, final_byte: u8) -> Option<AltScreenEvent> {
        // We're looking for:
        // CSI ? 1049 h  (\x1b[?1049h) - enter
        // CSI ? 1049 l  (\x1b[?1049l) - exit
        // CSI ? 47 h    (\x1b[?47h)   - enter (older)
        // CSI ? 47 l    (\x1b[?47l)   - exit (older)

        let params = &self.param_buf[..self.param_pos];

        // Must start with ?
        if params.first() != Some(&b'?') {
            return None;
        }

        // Extract the number after ?
        let num_str = &params[1..];

        // Check for 1049 or 47
        let is_1049 = num_str == b"1049";
        let is_47 = num_str == b"47";

        if !is_1049 && !is_47 {
            return None;
        }

        match final_byte {
            b'h' => {
                // Set (enter alt screen)
                if !self.in_alt_screen {
                    self.in_alt_screen = true;
                    tracing::debug!(
                        "Alt screen entered ({})",
                        if is_1049 { "1049" } else { "47" }
                    );
                    return Some(AltScreenEvent::Enter);
                }
            }
            b'l' => {
                // Reset (exit alt screen)
                if self.in_alt_screen {
                    self.in_alt_screen = false;
                    tracing::debug!(
                        "Alt screen exited ({})",
                        if is_1049 { "1049" } else { "47" }
                    );
                    return Some(AltScreenEvent::Exit);
                }
            }
            _ => {}
        }

        None
    }

    /// Resets the detector state.
    ///
    /// Call this on mode transitions or errors.
    pub fn reset(&mut self) {
        self.state = CsiState::Normal;
        self.param_pos = 0;
        // Note: We don't reset in_alt_screen - that tracks actual terminal state
    }
}

impl Default for AltScreenDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator adapter for feeding multiple bytes.
struct AltScreenIterator<'a> {
    detector: &'a mut AltScreenDetector,
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for AltScreenIterator<'a> {
    type Item = AltScreenEvent;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            self.pos += 1;
            if let Some(event) = self.detector.feed_byte(b) {
                return Some(event);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_not_in_alt_screen() {
        let detector = AltScreenDetector::new();
        assert!(!detector.is_in_alt_screen());
    }

    #[test]
    fn test_detect_1049h_enter() {
        let mut detector = AltScreenDetector::new();
        let events: Vec<_> = detector.feed(b"\x1b[?1049h").collect();
        assert_eq!(events, vec![AltScreenEvent::Enter]);
        assert!(detector.is_in_alt_screen());
    }

    #[test]
    fn test_detect_1049l_exit() {
        let mut detector = AltScreenDetector::new();
        // First enter
        detector.feed(b"\x1b[?1049h").for_each(drop);
        assert!(detector.is_in_alt_screen());

        // Then exit
        let events: Vec<_> = detector.feed(b"\x1b[?1049l").collect();
        assert_eq!(events, vec![AltScreenEvent::Exit]);
        assert!(!detector.is_in_alt_screen());
    }

    #[test]
    fn test_detect_47h_enter() {
        let mut detector = AltScreenDetector::new();
        let events: Vec<_> = detector.feed(b"\x1b[?47h").collect();
        assert_eq!(events, vec![AltScreenEvent::Enter]);
        assert!(detector.is_in_alt_screen());
    }

    #[test]
    fn test_detect_47l_exit() {
        let mut detector = AltScreenDetector::new();
        detector.feed(b"\x1b[?47h").for_each(drop);
        let events: Vec<_> = detector.feed(b"\x1b[?47l").collect();
        assert_eq!(events, vec![AltScreenEvent::Exit]);
        assert!(!detector.is_in_alt_screen());
    }

    #[test]
    fn test_no_duplicate_enter() {
        let mut detector = AltScreenDetector::new();
        detector.feed(b"\x1b[?1049h").for_each(drop);

        // Second enter should not emit event
        let events: Vec<_> = detector.feed(b"\x1b[?1049h").collect();
        assert!(events.is_empty());
    }

    #[test]
    fn test_no_duplicate_exit() {
        let mut detector = AltScreenDetector::new();
        // Exit without enter should not emit
        let events: Vec<_> = detector.feed(b"\x1b[?1049l").collect();
        assert!(events.is_empty());
    }

    #[test]
    fn test_sequence_in_middle_of_data() {
        let mut detector = AltScreenDetector::new();
        let events: Vec<_> = detector
            .feed(b"hello\x1b[?1049hworld")
            .collect();
        assert_eq!(events, vec![AltScreenEvent::Enter]);
    }

    #[test]
    fn test_split_sequence() {
        let mut detector = AltScreenDetector::new();

        // Feed sequence in parts
        let events1: Vec<_> = detector.feed(b"\x1b[?10").collect();
        assert!(events1.is_empty());

        let events2: Vec<_> = detector.feed(b"49h").collect();
        assert_eq!(events2, vec![AltScreenEvent::Enter]);
    }

    #[test]
    fn test_other_csi_ignored() {
        let mut detector = AltScreenDetector::new();
        // Color sequence should be ignored
        let events: Vec<_> = detector.feed(b"\x1b[31m").collect();
        assert!(events.is_empty());
        assert!(!detector.is_in_alt_screen());
    }

    #[test]
    fn test_osc_ignored() {
        let mut detector = AltScreenDetector::new();
        // OSC title sequence should not confuse the parser
        let events: Vec<_> = detector.feed(b"\x1b]0;title\x07").collect();
        assert!(events.is_empty());
    }

    #[test]
    fn test_reset_preserves_alt_screen_state() {
        let mut detector = AltScreenDetector::new();
        detector.feed(b"\x1b[?1049h").for_each(drop);
        assert!(detector.is_in_alt_screen());

        detector.reset();
        // in_alt_screen should still be true
        assert!(detector.is_in_alt_screen());
    }
}
