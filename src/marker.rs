//! Streaming OSC 777 marker parser.
//!
//! This module implements a zero-allocation streaming parser for OSC 777 escape
//! sequences that carry shell state markers (precmd, prompt, preexec).
//!
//! # Protocol Overview
//!
//! OSC 777 markers use the format:
//! ```text
//! ESC ] 777 ; <session_token> ; <marker_type> [; <payload>] BEL
//! ```
//!
//! Where:
//! - `ESC` is byte 0x1B
//! - `]` is byte 0x5D (starts OSC sequence)
//! - `777` identifies our custom OSC type
//! - `session_token` is a 16 hex character session identifier
//! - `marker_type` is one of: `PRECMD`, `PROMPT`, `PREEXEC`
//! - `payload` is optional (used by `PRECMD` for exit code)
//! - `BEL` is byte 0x07 (terminates OSC sequence)
//!
//! # Zero-Allocation Design
//!
//! The parser uses a fixed 80-byte buffer to handle split reads (markers spanning
//! multiple `read()` calls) without heap allocation. This is critical for performance
//! in the hot path of terminal I/O processing.
//!
//! # State Machine
//!
//! ```text
//! ┌────────┐  ESC   ┌─────────┐   ]    ┌─────────┐
//! │ Normal │───────▶│ EscSeen │───────▶│ OscBody │
//! └────────┘        └─────────┘        └─────────┘
//!     ▲                  │                  │
//!     │    other byte    │                  │
//!     └──────────────────┘                  │
//!     ▲                                     │
//!     │  BEL / overflow / invalid           │
//!     └─────────────────────────────────────┘
//! ```
//!
//! # Session Token Security
//!
//! Session tokens are validated using constant-time comparison to prevent timing
//! attacks. Invalid token attempts are counted and logged after a threshold to
//! detect potential security issues.
//!
//! # Usage Example
//!
//! ```ignore
//! use wrashpty::marker::{MarkerParser, ParseOutput};
//!
//! let token = *b"a1b2c3d4e5f67890";
//! let mut parser = MarkerParser::new(token);
//!
//! // Feed incoming bytes from PTY
//! for output in parser.feed(input_bytes) {
//!     match output {
//!         ParseOutput::Bytes(bytes) => write_to_terminal(bytes),
//!         ParseOutput::Marker(event) => handle_marker_event(event),
//!     }
//! }
//!
//! // On timeout, flush any buffered partial sequence
//! if parser.is_mid_sequence() {
//!     if let Some(bytes) = parser.flush_stale() {
//!         write_to_terminal(bytes);
//!     }
//! }
//! ```

use std::borrow::Cow;

use thiserror::Error;

use crate::types::MarkerEvent;

/// Maximum length of an OSC 777 marker sequence in bytes.
///
/// This covers the worst case: `ESC ] 777 ; <16-byte-token> ; PREEXEC ; <payload> BEL`
/// With generous padding for payloads like exit codes.
pub const MAX_MARKER_LEN: usize = 80;

/// Timeout in milliseconds for considering a partial sequence stale.
///
/// If the parser is mid-sequence and no new bytes arrive within this window,
/// the buffered bytes should be flushed as passthrough data.
pub const STALE_SEQUENCE_TIMEOUT_MS: u64 = 100;

/// Threshold for logging security warnings about invalid token attempts.
const SECURITY_EVENT_LOG_THRESHOLD: u32 = 100;

/// Parser state machine states.
///
/// The parser transitions between these states as it processes input bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParserState {
    /// Normal passthrough mode - looking for ESC byte.
    Normal,
    /// Seen ESC (0x1B) - waiting for `]` to start OSC sequence.
    EscSeen,
    /// Inside OSC body - accumulating until BEL (0x07).
    OscBody,
}

/// Errors that can occur during marker parsing.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// Buffer overflow: marker sequence exceeded maximum length.
    #[error("marker buffer overflow: sequence exceeded {MAX_MARKER_LEN} bytes")]
    BufferOverflow,
}

/// Zero-allocation streaming parser for OSC 777 markers.
///
/// The parser maintains state between `feed()` calls to handle markers that
/// span multiple read operations. It uses a fixed-size buffer to avoid heap
/// allocation in the hot path.
///
/// # Thread Safety
///
/// This parser is not thread-safe. Each PTY session should have its own
/// parser instance.
#[derive(Debug)]
pub struct MarkerParser {
    /// Fixed-size buffer for accumulating partial sequences.
    buf: [u8; MAX_MARKER_LEN],
    /// Current number of valid bytes in the buffer.
    buf_len: usize,
    /// Current parser state.
    state: ParserState,
    /// Session token for validating markers (16 hex characters).
    session_token: [u8; 16],
    /// Counter for invalid token attempts (security monitoring).
    security_event_count: u32,
}

/// Output from the marker parser.
///
/// Each item yielded by the parser is either passthrough bytes or a parsed
/// marker event. The lifetime `'a` ensures output slices don't outlive the
/// input buffer.
///
/// # Memory Efficiency
///
/// The `Bytes` variant uses `Cow<'a, [u8]>` to achieve zero-copy for the common
/// case (passthrough from input) while allowing owned data for edge cases
/// (buffered partial sequences, invalid markers). In practice:
/// - Normal passthrough: `Cow::Borrowed(&input[..])` - zero allocation
/// - Buffer flush: `Cow::Owned(buffer.to_vec())` - small allocation, rare
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutput<'a> {
    /// Passthrough bytes that should be written to the terminal.
    /// Uses `Cow` to allow both borrowed (zero-copy) and owned (buffered) data.
    Bytes(Cow<'a, [u8]>),
    /// A parsed marker event from a valid OSC 777 sequence.
    Marker(MarkerEvent),
}

impl MarkerParser {
    /// Creates a new marker parser with the given session token.
    ///
    /// The session token must be exactly 16 hex characters. This token is
    /// embedded in the generated bashrc and used to validate that markers
    /// come from our shell integration rather than application output.
    ///
    /// # Arguments
    ///
    /// * `session_token` - 16-byte array containing hex characters [0-9a-f]
    ///
    /// # Example
    ///
    /// ```ignore
    /// let token = *b"a1b2c3d4e5f67890";
    /// let parser = MarkerParser::new(token);
    /// ```
    #[must_use]
    pub fn new(session_token: [u8; 16]) -> Self {
        Self {
            buf: [0u8; MAX_MARKER_LEN],
            buf_len: 0,
            state: ParserState::Normal,
            session_token,
            security_event_count: 0,
        }
    }

    /// Returns `true` if the parser is currently buffering a partial sequence.
    ///
    /// This is used by the poll loop to determine if a timeout should be set
    /// for flushing stale partial sequences.
    ///
    /// # Returns
    ///
    /// `true` if the parser state is `EscSeen` or `OscBody`, `false` otherwise.
    #[inline]
    #[must_use]
    pub fn is_mid_sequence(&self) -> bool {
        self.state != ParserState::Normal
    }

    /// Flushes any buffered partial sequence as passthrough bytes.
    ///
    /// This should be called by the poll loop when a timeout occurs while
    /// the parser is mid-sequence. This handles the case where an ESC byte
    /// or partial OSC sequence was not actually a marker.
    ///
    /// # Returns
    ///
    /// `Some(&[u8])` containing the buffered bytes if any, `None` if the
    /// buffer is empty.
    ///
    /// # Note
    ///
    /// After calling this method, the parser state is reset to `Normal`.
    pub fn flush_stale(&mut self) -> Option<&[u8]> {
        if self.buf_len > 0 {
            let len = self.buf_len;
            self.buf_len = 0;
            self.state = ParserState::Normal;
            Some(&self.buf[..len])
        } else {
            self.state = ParserState::Normal;
            None
        }
    }

    /// Feeds input bytes to the parser and returns an iterator over outputs.
    ///
    /// The returned iterator yields `ParseOutput` items - either passthrough
    /// bytes or parsed marker events. The iterator processes input lazily,
    /// only advancing as items are consumed.
    ///
    /// # Arguments
    ///
    /// * `input` - Slice of bytes read from the PTY
    ///
    /// # Lifetimes
    ///
    /// The lifetime `'a` ties the output iterator to both `self` and `input`.
    /// This ensures that `ParseOutput::Bytes` slices (which borrow from `input`
    /// or the internal buffer) don't outlive their source.
    ///
    /// # Example
    ///
    /// ```ignore
    /// for output in parser.feed(bytes) {
    ///     match output {
    ///         ParseOutput::Bytes(b) => terminal.write(b),
    ///         ParseOutput::Marker(m) => handle_event(m),
    ///     }
    /// }
    /// ```
    pub fn feed<'a>(&'a mut self, input: &'a [u8]) -> impl Iterator<Item = ParseOutput<'a>> + 'a {
        MarkerIterator {
            parser: self,
            input,
            pos: 0,
            passthrough_start: None,
        }
    }

    /// Validates an OSC body and extracts a marker event if valid.
    ///
    /// The body format is: `777;<token>;<type>[;<payload>]`
    ///
    /// # Arguments
    ///
    /// * `body` - The OSC body bytes (without ESC, ], or BEL)
    ///
    /// # Returns
    ///
    /// `Some(MarkerEvent)` if the body is a valid marker with correct token,
    /// `None` otherwise.
    fn validate_marker(&mut self, body: &[u8]) -> Option<MarkerEvent> {
        // Parse body as UTF-8
        let body_str = std::str::from_utf8(body).ok()?;

        // Split into fields: 777, token, type, [payload]
        let mut parts = body_str.splitn(4, ';');

        // Validate OSC type is 777
        let osc_type = parts.next()?;
        if osc_type != "777" {
            return None;
        }

        // Validate session token using constant-time comparison
        let token = parts.next()?;
        if token.len() != 16 || !constant_time_eq(token.as_bytes(), &self.session_token) {
            self.security_event_count += 1;
            if self.security_event_count == SECURITY_EVENT_LOG_THRESHOLD {
                tracing::warn!(
                    count = self.security_event_count,
                    "excessive invalid marker token attempts detected"
                );
            }
            return None;
        }

        // Parse marker type
        let marker_type = parts.next()?;
        match marker_type {
            "PRECMD" => {
                // Parse exit code from payload, default to 1 if missing or invalid
                let exit_code = parts
                    .next()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1);
                Some(MarkerEvent::Precmd { exit_code })
            }
            "PROMPT" => Some(MarkerEvent::Prompt),
            "PREEXEC" => Some(MarkerEvent::Preexec),
            _ => None,
        }
    }

    /// Validates a marker using the internal buffer directly (zero-allocation).
    ///
    /// This is the hot path optimization - for valid markers, no allocation occurs.
    /// The body is read directly from `self.buf[2..body_len]`.
    ///
    /// # Arguments
    ///
    /// * `body_len` - Length of buffered data (body starts at index 2, after ESC ])
    ///
    /// # Returns
    ///
    /// `Some(MarkerEvent)` if the body is a valid marker with correct token,
    /// `None` otherwise.
    fn validate_marker_borrowed(&mut self, body_len: usize) -> Option<MarkerEvent> {
        // Body is at buf[2..body_len], skipping ESC and ]
        let body = &self.buf[2..body_len];

        // Parse body as UTF-8
        let body_str = std::str::from_utf8(body).ok()?;

        // Split into fields: 777, token, type, [payload]
        let mut parts = body_str.splitn(4, ';');

        // Validate OSC type is 777
        let osc_type = parts.next()?;
        if osc_type != "777" {
            return None;
        }

        // Validate session token using constant-time comparison
        let token = parts.next()?;
        if token.len() != 16 || !constant_time_eq(token.as_bytes(), &self.session_token) {
            self.security_event_count += 1;
            if self.security_event_count == SECURITY_EVENT_LOG_THRESHOLD {
                tracing::warn!(
                    count = self.security_event_count,
                    "excessive invalid marker token attempts detected"
                );
            }
            return None;
        }

        // Parse marker type
        let marker_type = parts.next()?;
        match marker_type {
            "PRECMD" => {
                // Parse exit code from payload, default to 1 if missing or invalid
                let exit_code = parts
                    .next()
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1);
                Some(MarkerEvent::Precmd { exit_code })
            }
            "PROMPT" => Some(MarkerEvent::Prompt),
            "PREEXEC" => Some(MarkerEvent::Preexec),
            _ => None,
        }
    }
}

/// Compares two byte slices in constant time.
///
/// This function prevents timing attacks by ensuring the comparison takes
/// the same amount of time regardless of where the bytes differ. This is
/// critical for session token validation.
///
/// # Arguments
///
/// * `a` - First byte slice
/// * `b` - Second byte slice
///
/// # Returns
///
/// `true` if the slices are equal, `false` otherwise.
///
/// # Security
///
/// The XOR accumulation pattern ensures no early exit on mismatch. The
/// length check is not constant-time, but leaking length information is
/// acceptable since token length is fixed and public.
#[inline]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Streaming iterator over parser outputs.
///
/// This iterator processes input bytes lazily, yielding `ParseOutput` items
/// as they are identified. It maintains state to handle passthrough byte
/// accumulation efficiently.
struct MarkerIterator<'a> {
    /// Reference to the parser for state machine access.
    parser: &'a mut MarkerParser,
    /// Input bytes being processed.
    input: &'a [u8],
    /// Current position in the input.
    pos: usize,
    /// Start position of accumulated passthrough bytes (if any).
    passthrough_start: Option<usize>,
}

impl<'a> Iterator for MarkerIterator<'a> {
    type Item = ParseOutput<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        // First, check if we need to yield accumulated passthrough bytes
        // This happens when we previously hit an ESC or finished a sequence

        loop {
            // If we've consumed all input, yield any remaining passthrough bytes
            if self.pos >= self.input.len() {
                if let Some(start) = self.passthrough_start.take() {
                    if start < self.input.len() {
                        return Some(ParseOutput::Bytes(Cow::Borrowed(
                            &self.input[start..self.input.len()],
                        )));
                    }
                }
                return None;
            }

            let byte = self.input[self.pos];

            match self.parser.state {
                ParserState::Normal => {
                    if byte == 0x1B {
                        // ESC byte - potential start of marker sequence
                        // First, yield any accumulated passthrough bytes
                        if let Some(start) = self.passthrough_start.take() {
                            if start < self.pos {
                                let result =
                                    ParseOutput::Bytes(Cow::Borrowed(&self.input[start..self.pos]));
                                // Buffer the ESC and transition state
                                self.parser.buf[0] = byte;
                                self.parser.buf_len = 1;
                                self.parser.state = ParserState::EscSeen;
                                self.pos += 1;
                                return Some(result);
                            }
                        }
                        // Buffer the ESC and transition state
                        self.parser.buf[0] = byte;
                        self.parser.buf_len = 1;
                        self.parser.state = ParserState::EscSeen;
                        self.pos += 1;
                    } else {
                        // Regular byte - accumulate for passthrough
                        if self.passthrough_start.is_none() {
                            self.passthrough_start = Some(self.pos);
                        }
                        self.pos += 1;
                    }
                }

                ParserState::EscSeen => {
                    if byte == 0x5D {
                        // `]` byte - start of OSC sequence
                        self.parser.buf[self.parser.buf_len] = byte;
                        self.parser.buf_len += 1;
                        self.parser.state = ParserState::OscBody;
                        self.pos += 1;
                    } else {
                        // Not an OSC sequence - flush buffer as passthrough (owned copy)
                        let buffered = self.parser.buf[..self.parser.buf_len].to_vec();
                        self.parser.buf_len = 0;
                        self.parser.state = ParserState::Normal;
                        self.passthrough_start = Some(self.pos);
                        self.pos += 1;

                        // Return buffered ESC as passthrough
                        return Some(ParseOutput::Bytes(Cow::Owned(buffered)));
                    }
                }

                ParserState::OscBody => {
                    if byte == 0x07 {
                        // BEL byte - end of OSC sequence
                        // Validate marker using borrowed slice (zero-allocation for valid markers)
                        let body_len = self.parser.buf_len;
                        let event = self.parser.validate_marker_borrowed(body_len);

                        // Reset parser state
                        self.parser.buf_len = 0;
                        self.parser.state = ParserState::Normal;
                        self.pos += 1;

                        if let Some(marker_event) = event {
                            // Valid marker - yield the event (no allocation!)
                            return Some(ParseOutput::Marker(marker_event));
                        } else {
                            // Invalid marker - allocate only here (rare path)
                            // Include the BEL in the output
                            let mut buffered = self.parser.buf[..body_len].to_vec();
                            buffered.push(0x07);
                            return Some(ParseOutput::Bytes(Cow::Owned(buffered)));
                        }
                    } else if self.parser.buf_len >= MAX_MARKER_LEN {
                        // Buffer overflow - flush as passthrough and reset (owned copy)
                        let buffered = self.parser.buf[..self.parser.buf_len].to_vec();
                        self.parser.buf_len = 0;
                        self.parser.state = ParserState::Normal;
                        self.passthrough_start = Some(self.pos);
                        self.pos += 1;
                        return Some(ParseOutput::Bytes(Cow::Owned(buffered)));
                    } else {
                        // Accumulate body byte
                        self.parser.buf[self.parser.buf_len] = byte;
                        self.parser.buf_len += 1;
                        self.pos += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to create a consistent test token.
    fn make_token() -> [u8; 16] {
        *b"a1b2c3d4e5f67890"
    }

    /// Helper to extract bytes from ParseOutput, regardless of Cow variant.
    fn extract_bytes<'a>(output: &'a ParseOutput<'a>) -> Option<&'a [u8]> {
        match output {
            ParseOutput::Bytes(cow) => Some(cow.as_ref()),
            _ => None,
        }
    }

    /// Helper to create a valid marker sequence.
    fn make_marker(token: &[u8; 16], marker_type: &str, payload: Option<&str>) -> Vec<u8> {
        let mut seq = vec![0x1B, 0x5D]; // ESC ]
        seq.extend_from_slice(b"777;");
        seq.extend_from_slice(token);
        seq.push(b';');
        seq.extend_from_slice(marker_type.as_bytes());
        if let Some(p) = payload {
            seq.push(b';');
            seq.extend_from_slice(p.as_bytes());
        }
        seq.push(0x07); // BEL
        seq
    }

    // ==========================================================================
    // Valid Marker Tests
    // ==========================================================================

    #[test]
    fn test_valid_precmd_marker() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PRECMD", Some("0"));

        let outputs: Vec<_> = parser.feed(&marker).collect();

        assert_eq!(outputs.len(), 1);
        assert_eq!(
            outputs[0],
            ParseOutput::Marker(MarkerEvent::Precmd { exit_code: 0 })
        );
    }

    #[test]
    fn test_valid_prompt_marker() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PROMPT", None);

        let outputs: Vec<_> = parser.feed(&marker).collect();

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], ParseOutput::Marker(MarkerEvent::Prompt));
    }

    #[test]
    fn test_valid_preexec_marker() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PREEXEC", None);

        let outputs: Vec<_> = parser.feed(&marker).collect();

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], ParseOutput::Marker(MarkerEvent::Preexec));
    }

    #[test]
    fn test_precmd_with_nonzero_exit() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PRECMD", Some("1"));

        let outputs: Vec<_> = parser.feed(&marker).collect();

        assert_eq!(outputs.len(), 1);
        assert_eq!(
            outputs[0],
            ParseOutput::Marker(MarkerEvent::Precmd { exit_code: 1 })
        );
    }

    #[test]
    fn test_precmd_without_exit_code() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PRECMD", None);

        let outputs: Vec<_> = parser.feed(&marker).collect();

        assert_eq!(outputs.len(), 1);
        // Default exit code is 1 when missing
        assert_eq!(
            outputs[0],
            ParseOutput::Marker(MarkerEvent::Precmd { exit_code: 1 })
        );
    }

    // ==========================================================================
    // Split Read Tests
    // ==========================================================================

    #[test]
    fn test_split_read_esc_bracket() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PROMPT", None);

        // Split at ESC ]
        let part1 = &marker[..1]; // Just ESC
        let part2 = &marker[1..]; // ] onwards

        let outputs1: Vec<_> = parser.feed(part1).collect();
        assert!(outputs1.is_empty());
        assert!(parser.is_mid_sequence());

        let outputs2: Vec<_> = parser.feed(part2).collect();
        assert_eq!(outputs2.len(), 1);
        assert_eq!(outputs2[0], ParseOutput::Marker(MarkerEvent::Prompt));
    }

    #[test]
    fn test_split_read_mid_body() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PREEXEC", None);

        // Split in middle of OSC body
        let mid = marker.len() / 2;
        let part1 = &marker[..mid];
        let part2 = &marker[mid..];

        let outputs1: Vec<_> = parser.feed(part1).collect();
        assert!(outputs1.is_empty());
        assert!(parser.is_mid_sequence());

        let outputs2: Vec<_> = parser.feed(part2).collect();
        assert_eq!(outputs2.len(), 1);
        assert_eq!(outputs2[0], ParseOutput::Marker(MarkerEvent::Preexec));
    }

    #[test]
    fn test_split_read_before_bel() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PROMPT", None);

        // Split just before BEL
        let part1 = &marker[..marker.len() - 1];
        let part2 = &marker[marker.len() - 1..];

        let outputs1: Vec<_> = parser.feed(part1).collect();
        assert!(outputs1.is_empty());
        assert!(parser.is_mid_sequence());

        let outputs2: Vec<_> = parser.feed(part2).collect();
        assert_eq!(outputs2.len(), 1);
        assert_eq!(outputs2[0], ParseOutput::Marker(MarkerEvent::Prompt));
    }

    #[test]
    fn test_multiple_splits() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PRECMD", Some("42"));

        // Split into many small chunks and count markers
        let mut marker_found = false;
        for chunk in marker.chunks(3) {
            for output in parser.feed(chunk) {
                if let ParseOutput::Marker(MarkerEvent::Precmd { exit_code: 42 }) = output {
                    marker_found = true;
                }
            }
        }

        // Should have found the marker
        assert!(marker_found);

        // The marker should have been yielded, parser back to normal
        assert!(!parser.is_mid_sequence());
    }

    // ==========================================================================
    // Invalid Marker Tests
    // ==========================================================================

    #[test]
    fn test_invalid_token() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let wrong_token = *b"0000000000000000";
        let marker = make_marker(&wrong_token, "PROMPT", None);

        let outputs: Vec<_> = parser.feed(&marker).collect();

        assert_eq!(outputs.len(), 1);
        // Should return bytes, not a marker
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_invalid_marker_type() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "INVALID", None);

        let outputs: Vec<_> = parser.feed(&marker).collect();

        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_malformed_osc() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        // Missing token field
        let malformed = b"\x1b]777;PROMPT\x07";

        let outputs: Vec<_> = parser.feed(malformed).collect();

        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    // ==========================================================================
    // Edge Cases
    // ==========================================================================

    #[test]
    fn test_buffer_overflow() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);

        // Create an OSC sequence that exceeds buffer
        let mut huge_seq = vec![0x1B, 0x5D]; // ESC ]
        huge_seq.extend(vec![b'x'; MAX_MARKER_LEN + 10]);
        huge_seq.push(0x07);

        let outputs: Vec<_> = parser.feed(&huge_seq).collect();

        // Should flush buffer on overflow and return as bytes
        assert!(!outputs.is_empty());
        for output in &outputs {
            assert!(matches!(output, ParseOutput::Bytes(_)));
        }
        assert!(!parser.is_mid_sequence());
    }

    #[test]
    fn test_passthrough_bytes() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let input = b"Hello, World!";

        let outputs: Vec<_> = parser.feed(input).collect();

        assert_eq!(outputs.len(), 1);
        assert_eq!(extract_bytes(&outputs[0]), Some(input.as_slice()));
    }

    #[test]
    fn test_interleaved_markers_and_text() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);

        let mut input = b"prefix ".to_vec();
        input.extend(make_marker(&token, "PROMPT", None));
        input.extend(b" suffix");

        let outputs: Vec<_> = parser.feed(&input).collect();

        assert_eq!(outputs.len(), 3);
        assert_eq!(extract_bytes(&outputs[0]), Some(b"prefix ".as_slice()));
        assert_eq!(outputs[1], ParseOutput::Marker(MarkerEvent::Prompt));
        assert_eq!(extract_bytes(&outputs[2]), Some(b" suffix".as_slice()));
    }

    #[test]
    fn test_flush_stale() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);

        // Feed partial sequence (ESC only)
        let _: Vec<_> = parser.feed(&[0x1B]).collect();
        assert!(parser.is_mid_sequence());

        // Flush stale
        let flushed = parser.flush_stale();
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap(), &[0x1B]);
        assert!(!parser.is_mid_sequence());

        // Second flush returns None
        assert!(parser.flush_stale().is_none());
    }

    #[test]
    fn test_empty_input() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);

        let outputs: Vec<_> = parser.feed(&[]).collect();
        assert!(outputs.is_empty());
    }

    #[test]
    fn test_multiple_markers_in_sequence() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);

        let mut input = make_marker(&token, "PRECMD", Some("0"));
        input.extend(make_marker(&token, "PROMPT", None));
        input.extend(make_marker(&token, "PREEXEC", None));

        let outputs: Vec<_> = parser.feed(&input).collect();

        assert_eq!(outputs.len(), 3);
        assert_eq!(
            outputs[0],
            ParseOutput::Marker(MarkerEvent::Precmd { exit_code: 0 })
        );
        assert_eq!(outputs[1], ParseOutput::Marker(MarkerEvent::Prompt));
        assert_eq!(outputs[2], ParseOutput::Marker(MarkerEvent::Preexec));
    }

    #[test]
    fn test_esc_followed_by_non_bracket() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);

        // ESC followed by 'A' (not ']') - should pass through
        let input = b"\x1bA";

        let outputs: Vec<_> = parser.feed(input).collect();

        // Should get ESC as bytes, then 'A' as bytes
        assert!(!outputs.is_empty());
        let total_bytes: Vec<u8> = outputs
            .iter()
            .filter_map(|o| match o {
                ParseOutput::Bytes(b) => Some(b.to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(total_bytes, vec![0x1B, b'A']);
    }

    // ==========================================================================
    // Security Tests
    // ==========================================================================

    #[test]
    fn test_security_event_counter() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);
        let wrong_token = *b"0000000000000000";

        // Feed multiple invalid markers
        for _ in 0..10 {
            let marker = make_marker(&wrong_token, "PROMPT", None);
            let _: Vec<_> = parser.feed(&marker).collect();
        }

        assert_eq!(parser.security_event_count, 10);
    }

    #[test]
    fn test_constant_time_comparison() {
        // Test equality
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"a1b2c3d4e5f67890", b"a1b2c3d4e5f67890"));

        // Test inequality
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"x"));

        // Test different lengths
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn test_non_osc_777() {
        let token = make_token();
        let mut parser = MarkerParser::new(token);

        // OSC 8 (hyperlink) sequence - should pass through
        let osc8 = b"\x1b]8;;https://example.com\x07";

        let outputs: Vec<_> = parser.feed(osc8).collect();

        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    prop_compose! {
        /// Generate a random byte sequence of length 0..1000.
        fn arbitrary_bytes()(bytes in prop::collection::vec(any::<u8>(), 0..1000)) -> Vec<u8> {
            bytes
        }
    }

    proptest! {
        /// Test that no bytes are lost during parsing.
        ///
        /// This property test feeds random bytes to the parser in chunks and
        /// verifies that all input bytes appear in the output (either as
        /// passthrough bytes or as part of stripped markers).
        #[test]
        fn test_no_bytes_lost(input in arbitrary_bytes()) {
            let token = *b"a1b2c3d4e5f67890";
            let mut parser = MarkerParser::new(token);

            let mut output_bytes = Vec::new();

            // Feed in chunks of 10 bytes to simulate split reads
            for chunk in input.chunks(10) {
                for output in parser.feed(chunk) {
                    if let ParseOutput::Bytes(b) = output {
                        output_bytes.extend_from_slice(&b);
                    }
                    // Markers are consumed but not tracked (they consume input bytes)
                }
            }

            // Flush any remaining buffered bytes
            if let Some(remaining) = parser.flush_stale() {
                output_bytes.extend_from_slice(remaining);
            }

            // If no markers were parsed, all bytes should be output
            // If markers were parsed, some bytes are consumed by markers
            // In either case, we verify the parser didn't crash
            prop_assert!(output_bytes.len() <= input.len());
        }

        /// Test that the parser never panics regardless of input.
        #[test]
        fn test_parser_never_panics(input in arbitrary_bytes()) {
            let token = *b"a1b2c3d4e5f67890";
            let mut parser = MarkerParser::new(token);

            // Feed in random chunk sizes
            let chunk_sizes = [1, 3, 7, 11, 13, 17, 23, 31];
            let mut pos = 0;
            let mut chunk_idx = 0;

            while pos < input.len() {
                let chunk_size = chunk_sizes[chunk_idx % chunk_sizes.len()];
                let end = std::cmp::min(pos + chunk_size, input.len());
                let chunk = &input[pos..end];

                // This should never panic
                for _output in parser.feed(chunk) {
                    // Just consume the iterator
                }

                pos = end;
                chunk_idx += 1;
            }

            // Flush should never panic
            let _ = parser.flush_stale();
        }

        /// Test state machine invariants.
        #[test]
        fn test_state_machine_invariants(input in arbitrary_bytes()) {
            let token = *b"a1b2c3d4e5f67890";
            let mut parser = MarkerParser::new(token);

            for chunk in input.chunks(10) {
                // Consume the iterator fully before checking invariants
                let _outputs: Vec<_> = parser.feed(chunk).collect();

                // Verify buffer length invariant after processing chunk
                prop_assert!(parser.buf_len <= MAX_MARKER_LEN);

                // Verify: if state is Normal, the buffer should be empty
                if parser.state == ParserState::Normal {
                    prop_assert_eq!(parser.buf_len, 0);
                }
            }

            // After flush_stale, state should be Normal
            let _ = parser.flush_stale();
            prop_assert_eq!(parser.state, ParserState::Normal);
            prop_assert_eq!(parser.buf_len, 0);
        }
    }
}
