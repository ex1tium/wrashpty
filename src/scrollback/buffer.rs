//! Ring buffer for scrollback line storage.
//!
//! Stores captured terminal output lines in a fixed-capacity ring buffer.
//! When full, oldest lines are dropped to make room for new ones.

use std::collections::VecDeque;
use std::time::Instant;

/// Default maximum number of lines to store.
pub const DEFAULT_MAX_LINES: usize = 10_000;

/// Default maximum bytes per line before truncation.
pub const DEFAULT_MAX_LINE_BYTES: usize = 4096;

/// A single captured line from terminal output.
#[derive(Debug, Clone)]
pub struct ScrollLine {
    /// Raw bytes including ANSI escape codes.
    content: Vec<u8>,
    /// Display width in columns (accounts for ANSI codes).
    display_width: u16,
    /// True if line was truncated at max_line_bytes.
    truncated: bool,
    /// When this line was captured.
    timestamp: Instant,
}

impl ScrollLine {
    /// Creates a new scroll line from raw bytes.
    ///
    /// # Arguments
    ///
    /// * `content` - Raw bytes (may include ANSI codes)
    /// * `max_bytes` - Maximum bytes before truncation
    /// * `terminal_width` - Terminal width for display width calculation
    pub fn new(content: Vec<u8>, max_bytes: usize, terminal_width: u16) -> Self {
        let truncated = content.len() > max_bytes;
        let content = if truncated {
            content[..max_bytes].to_vec()
        } else {
            content
        };

        let display_width = Self::calculate_display_width(&content, terminal_width);

        Self {
            content,
            display_width,
            truncated,
            timestamp: Instant::now(),
        }
    }

    /// Returns the raw content bytes.
    #[inline]
    pub fn content(&self) -> &[u8] {
        &self.content
    }

    /// Returns the display width in columns.
    #[inline]
    pub fn display_width(&self) -> u16 {
        self.display_width
    }

    /// Returns true if the line was truncated.
    #[inline]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Returns when the line was captured.
    #[inline]
    pub fn timestamp(&self) -> Instant {
        self.timestamp
    }

    /// Calculates display width accounting for ANSI escape sequences.
    ///
    /// This is a simplified calculation that:
    /// - Skips ANSI CSI sequences (ESC [ ... final byte)
    /// - Skips ANSI OSC sequences (ESC ] ... ST or BEL)
    /// - Counts printable ASCII as width 1
    /// - Treats wide characters (CJK) as width 2 (simplified)
    fn calculate_display_width(content: &[u8], terminal_width: u16) -> u16 {
        let mut width: u16 = 0;
        let mut i = 0;

        while i < content.len() {
            let b = content[i];

            // ESC sequence start
            if b == 0x1b && i + 1 < content.len() {
                let next = content[i + 1];
                if next == b'[' {
                    // CSI sequence: skip until final byte (0x40-0x7E)
                    i += 2;
                    while i < content.len() && !(0x40..=0x7E).contains(&content[i]) {
                        i += 1;
                    }
                    if i < content.len() {
                        i += 1; // Skip final byte
                    }
                    continue;
                } else if next == b']' {
                    // OSC sequence: skip until BEL (0x07) or ST (ESC \)
                    i += 2;
                    while i < content.len() {
                        if content[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if content[i] == 0x1b && i + 1 < content.len() && content[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                } else {
                    // Other escape sequence, skip ESC and next byte
                    i += 2;
                    continue;
                }
            }

            // Printable ASCII
            if (0x20..=0x7E).contains(&b) {
                width = width.saturating_add(1);
            }
            // Tab - assume 8-space tabs for simplicity
            else if b == 0x09 {
                width = width.saturating_add(8 - (width % 8));
            }
            // UTF-8 continuation bytes don't add width
            else if (0x80..=0xBF).contains(&b) {
                // Skip continuation bytes
            }
            // UTF-8 start bytes - simplified: assume width 1 for most, 2 for CJK range
            else if b >= 0xC0 {
                // This is a simplification; proper handling would need full Unicode width tables
                width = width.saturating_add(1);
            }

            i += 1;
        }

        width.min(terminal_width)
    }
}

/// Ring buffer for storing scrollback lines.
///
/// Lines are stored oldest-first (index 0 = oldest line).
/// When capacity is reached, oldest lines are dropped.
#[derive(Debug)]
pub struct ScrollbackBuffer {
    /// Lines stored oldest-first.
    lines: VecDeque<ScrollLine>,
    /// Maximum number of lines to store.
    max_lines: usize,
    /// Maximum bytes per line before truncation.
    max_line_bytes: usize,
    /// Current terminal width for display calculations.
    terminal_width: u16,
    /// Total count of lines dropped due to capacity.
    dropped_count: u64,
    /// Whether capture is currently active (false during alt-screen).
    capture_active: bool,
}

impl ScrollbackBuffer {
    /// Creates a new scrollback buffer with default settings.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_LINES, DEFAULT_MAX_LINE_BYTES)
    }

    /// Creates a new scrollback buffer with specified capacity.
    ///
    /// # Arguments
    ///
    /// * `max_lines` - Maximum number of lines to store
    /// * `max_line_bytes` - Maximum bytes per line before truncation
    pub fn with_capacity(max_lines: usize, max_line_bytes: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(max_lines.min(1000)), // Pre-allocate reasonably
            max_lines,
            max_line_bytes,
            terminal_width: 80, // Default, will be updated
            dropped_count: 0,
            capture_active: true,
        }
    }

    /// Sets the terminal width for display calculations.
    pub fn set_terminal_width(&mut self, width: u16) {
        self.terminal_width = width;
    }

    /// Returns the current terminal width.
    #[inline]
    pub fn terminal_width(&self) -> u16 {
        self.terminal_width
    }

    /// Returns the number of stored lines.
    #[inline]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Returns true if the buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Returns the maximum number of lines.
    #[inline]
    pub fn max_lines(&self) -> usize {
        self.max_lines
    }

    /// Returns the total count of dropped lines.
    #[inline]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count
    }

    /// Returns whether capture is currently active.
    #[inline]
    pub fn is_capture_active(&self) -> bool {
        self.capture_active
    }

    /// Suspends capture (called when entering alt-screen).
    pub fn suspend_capture(&mut self) {
        self.capture_active = false;
        tracing::debug!("Scrollback capture suspended (alt-screen)");
    }

    /// Resumes capture (called when exiting alt-screen).
    pub fn resume_capture(&mut self) {
        self.capture_active = true;
        tracing::debug!("Scrollback capture resumed");
    }

    /// Adds a line to the buffer.
    ///
    /// If capture is suspended, the line is silently discarded.
    /// If the buffer is full, the oldest line is dropped.
    ///
    /// # Arguments
    ///
    /// * `content` - Raw line bytes (may include ANSI codes)
    ///
    /// # Returns
    ///
    /// Number of lines dropped to make room (0 or 1).
    pub fn push_line(&mut self, content: Vec<u8>) -> usize {
        if !self.capture_active {
            return 0;
        }

        let mut dropped = 0;

        // Drop oldest if at capacity
        if self.lines.len() >= self.max_lines {
            self.lines.pop_front();
            self.dropped_count += 1;
            dropped = 1;
        }

        let line = ScrollLine::new(content, self.max_line_bytes, self.terminal_width);
        self.lines.push_back(line);

        dropped
    }

    /// Gets a line by index (0 = oldest).
    #[inline]
    pub fn get(&self, index: usize) -> Option<&ScrollLine> {
        self.lines.get(index)
    }

    /// Returns an iterator over lines from oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &ScrollLine> {
        self.lines.iter()
    }

    /// Gets a slice of lines for rendering.
    ///
    /// # Arguments
    ///
    /// * `start` - Start index (0 = oldest)
    /// * `count` - Number of lines to retrieve
    ///
    /// # Returns
    ///
    /// Iterator over the requested lines.
    pub fn get_range(&self, start: usize, count: usize) -> impl Iterator<Item = &ScrollLine> {
        self.lines.iter().skip(start).take(count)
    }

    /// Gets lines from the end of the buffer (for scroll offset).
    ///
    /// # Arguments
    ///
    /// * `offset` - Lines from the bottom (0 = most recent)
    /// * `count` - Number of lines to retrieve
    ///
    /// # Returns
    ///
    /// Iterator over the requested lines, from oldest to newest in the range.
    pub fn get_from_bottom(
        &self,
        offset: usize,
        count: usize,
    ) -> impl Iterator<Item = &ScrollLine> {
        let total = self.lines.len();
        if offset >= total {
            // Offset beyond buffer - return empty
            return self.lines.iter().skip(total).take(0);
        }

        // Calculate start index from the beginning
        let end_from_start = total.saturating_sub(offset);
        let start_from_start = end_from_start.saturating_sub(count);

        self.lines
            .iter()
            .skip(start_from_start)
            .take(count.min(end_from_start - start_from_start))
    }

    /// Clears all stored lines.
    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

impl Default for ScrollbackBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_buffer_is_empty() {
        let buffer = ScrollbackBuffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
        assert_eq!(buffer.dropped_count(), 0);
    }

    #[test]
    fn test_push_line_increases_len() {
        let mut buffer = ScrollbackBuffer::new();
        buffer.push_line(b"hello".to_vec());
        assert_eq!(buffer.len(), 1);
        buffer.push_line(b"world".to_vec());
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn test_push_line_respects_capacity() {
        let mut buffer = ScrollbackBuffer::with_capacity(3, 100);
        buffer.push_line(b"line 1".to_vec());
        buffer.push_line(b"line 2".to_vec());
        buffer.push_line(b"line 3".to_vec());
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.dropped_count(), 0);

        // Push 4th line, should drop oldest
        let dropped = buffer.push_line(b"line 4".to_vec());
        assert_eq!(dropped, 1);
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.dropped_count(), 1);

        // Verify oldest was dropped
        assert_eq!(buffer.get(0).unwrap().content(), b"line 2");
    }

    #[test]
    fn test_suspend_capture_ignores_pushes() {
        let mut buffer = ScrollbackBuffer::new();
        buffer.push_line(b"before".to_vec());
        assert_eq!(buffer.len(), 1);

        buffer.suspend_capture();
        buffer.push_line(b"during".to_vec());
        assert_eq!(buffer.len(), 1); // Still 1

        buffer.resume_capture();
        buffer.push_line(b"after".to_vec());
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn test_get_from_bottom() {
        let mut buffer = ScrollbackBuffer::with_capacity(10, 100);
        for i in 0..5 {
            buffer.push_line(format!("line {}", i).into_bytes());
        }

        // Get 2 lines from offset 0 (most recent)
        let lines: Vec<_> = buffer.get_from_bottom(0, 2).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].content(), b"line 3");
        assert_eq!(lines[1].content(), b"line 4");

        // Get 2 lines from offset 2
        let lines: Vec<_> = buffer.get_from_bottom(2, 2).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].content(), b"line 1");
        assert_eq!(lines[1].content(), b"line 2");
    }

    #[test]
    fn test_line_truncation() {
        let mut buffer = ScrollbackBuffer::with_capacity(10, 5);
        buffer.push_line(b"hello world".to_vec());

        let line = buffer.get(0).unwrap();
        assert!(line.is_truncated());
        assert_eq!(line.content(), b"hello");
    }

    #[test]
    fn test_display_width_skips_csi_sequences() {
        let content = b"\x1b[31mred\x1b[0m";
        let width = ScrollLine::calculate_display_width(content, 80);
        assert_eq!(width, 3); // "red" only
    }

    #[test]
    fn test_display_width_handles_plain_text() {
        let content = b"hello world";
        let width = ScrollLine::calculate_display_width(content, 80);
        assert_eq!(width, 11);
    }

    #[test]
    fn test_terminal_width_clamping() {
        let mut buffer = ScrollbackBuffer::new();
        buffer.set_terminal_width(40);
        assert_eq!(buffer.terminal_width(), 40);
    }
}
