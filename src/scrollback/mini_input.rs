//! Minimal input widget for modal prompts in the scroll viewer.
//!
//! This provides a lightweight text input field rendered in the topbar
//! area for search, filter, and go-to-line prompts.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::{self, Write};

/// Result of handling a key in the mini-input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniInputResult {
    /// Continue editing, no re-render needed.
    Continue,
    /// Content changed, may need incremental update (e.g., search).
    Changed,
    /// User pressed Enter to submit.
    Submit,
    /// User pressed Esc to cancel.
    Cancel,
}

/// Minimal input widget for modal prompts.
///
/// Rendered inline in the topbar, replacing the normal topbar content
/// while the prompt is active.
#[derive(Debug, Clone)]
pub struct MiniInput {
    /// Input buffer.
    pub buffer: String,
    /// Cursor position (byte offset).
    pub cursor: usize,
    /// Prompt label (e.g., "Search", "Filter", "Go to line").
    pub label: &'static str,
    /// Optional hint text shown when buffer is empty.
    pub hint: Option<&'static str>,
}

impl MiniInput {
    fn sanitized_cursor(&self) -> usize {
        let mut cursor = self.cursor.min(self.buffer.len());
        while cursor > 0 && !self.buffer.is_char_boundary(cursor) {
            cursor -= 1;
        }
        cursor
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self.sanitized_cursor();
    }

    /// Creates a new mini-input with the given label.
    pub fn new(label: &'static str) -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            label,
            hint: None,
        }
    }

    /// Creates a new mini-input with label and hint.
    pub fn with_hint(label: &'static str, hint: &'static str) -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            label,
            hint: Some(hint),
        }
    }

    /// Handles a key event, returning the result.
    pub fn handle_input(&mut self, key: KeyEvent) -> MiniInputResult {
        self.clamp_cursor();

        // Handle Ctrl combinations first
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') | KeyCode::Char('g') => MiniInputResult::Cancel,
                KeyCode::Char('u') => {
                    // Ctrl+U: clear to beginning of line
                    self.buffer.drain(..self.cursor);
                    self.cursor = 0;
                    MiniInputResult::Changed
                }
                KeyCode::Char('w') => {
                    // Ctrl+W: delete word backward
                    if self.cursor > 0 {
                        let before = &self.buffer[..self.cursor];
                        // Trim trailing whitespace first, then find previous word boundary.
                        // This matches common shell behavior for "word erase".
                        let trimmed_end = before
                            .rfind(|c: char| !c.is_whitespace())
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        let word_start = before[..trimmed_end]
                            .rfind(|c: char| c.is_whitespace())
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        self.buffer.drain(word_start..self.cursor);
                        self.cursor = word_start;
                        MiniInputResult::Changed
                    } else {
                        MiniInputResult::Continue
                    }
                }
                KeyCode::Char('a') => {
                    // Ctrl+A: move to beginning
                    self.cursor = 0;
                    MiniInputResult::Continue
                }
                KeyCode::Char('e') => {
                    // Ctrl+E: move to end
                    self.cursor = self.buffer.len();
                    MiniInputResult::Continue
                }
                _ => MiniInputResult::Continue,
            };
        }

        match key.code {
            KeyCode::Char(c) => {
                self.buffer.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                MiniInputResult::Changed
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // Find previous character boundary
                    let prev_char_boundary = self.buffer[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.buffer.drain(prev_char_boundary..self.cursor);
                    self.cursor = prev_char_boundary;
                    MiniInputResult::Changed
                } else {
                    MiniInputResult::Continue
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.buffer.len() {
                    // Find next character boundary
                    let next_char = self.buffer[self.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                    self.buffer.drain(self.cursor..self.cursor + next_char);
                    MiniInputResult::Changed
                } else {
                    MiniInputResult::Continue
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    // Move to previous character boundary
                    self.cursor = self.buffer[..self.cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                MiniInputResult::Continue
            }
            KeyCode::Right => {
                if self.cursor < self.buffer.len() {
                    // Move to next character boundary
                    self.cursor += self.buffer[self.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                }
                MiniInputResult::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                MiniInputResult::Continue
            }
            KeyCode::End => {
                self.cursor = self.buffer.len();
                MiniInputResult::Continue
            }
            KeyCode::Enter => MiniInputResult::Submit,
            KeyCode::Esc => MiniInputResult::Cancel,
            _ => MiniInputResult::Continue,
        }
    }

    /// Renders the mini-input to the topbar (row 1).
    ///
    /// Format: "Label: buffer|" with cursor positioned correctly.
    /// Uses simple ANSI styling without theme.
    pub fn render<W: Write>(&self, out: &mut W, cols: u16, status: Option<&str>) -> io::Result<()> {
        self.render_styled(out, cols, status, None, None, None)
    }

    /// Renders the mini-input with theme-consistent styling.
    ///
    /// # Arguments
    /// * `bg_ansi` - Background ANSI escape sequence (e.g., from color_to_bg_ansi)
    /// * `label_ansi` - Label foreground ANSI escape sequence
    /// * `text_ansi` - Text foreground ANSI escape sequence
    pub fn render_styled<W: Write>(
        &self,
        out: &mut W,
        cols: u16,
        status: Option<&str>,
        bg_ansi: Option<&str>,
        label_ansi: Option<&str>,
        text_ansi: Option<&str>,
    ) -> io::Result<()> {
        let bg = bg_ansi.unwrap_or("");
        let label_fg = label_ansi.unwrap_or("\x1b[2m"); // Default: dim
        let text_fg = text_ansi.unwrap_or("");
        let reset = "\x1b[0m";

        // Move to row 1, column 1 and clear line
        write!(out, "\x1b[1;1H\x1b[2K")?;

        // Apply background to entire row
        if !bg.is_empty() {
            // Fill the entire row with background color using spaces
            write!(out, "{}{:width$}{}", bg, "", reset, width = cols as usize)?;
            // Move back to start of row
            write!(out, "\x1b[1;1H")?;
            // Re-apply background for content
            write!(out, "{}", bg)?;
        }

        use crate::ui::text_width;

        // Build the prompt: "Label: "
        let prompt = format!("{}: ", self.label);
        let prompt_width = text_width::display_width(&prompt);

        // Render the label with styling
        write!(out, "{}{}{}", label_fg, prompt, reset)?;
        if !bg.is_empty() {
            write!(out, "{}", bg)?; // Re-apply background after reset
        }

        // Calculate available width for input (in display columns)
        let status_width = status.map(text_width::display_width).unwrap_or(0);
        let reserved_for_status = if status.is_some() {
            status_width + 1
        } else {
            0
        };
        let available = (cols as usize).saturating_sub(prompt_width + reserved_for_status);
        let mut view_start_col = 0usize;

        // Show buffer (or hint if empty)
        if self.buffer.is_empty() {
            if let Some(hint) = self.hint {
                // Hint in dim - truncate by display width
                let truncated = text_width::truncate_to_width(hint, available);
                write!(out, "\x1b[2m{}\x1b[22m", truncated)?;
            }
        } else {
            // Truncate if too long (show end of buffer if cursor is at end)
            // Use display width, not character count, to determine what fits
            let buf_width = text_width::display_width(&self.buffer);
            let display = if buf_width <= available {
                self.buffer.clone()
            } else {
                // Find cursor display column position
                let cursor = self.sanitized_cursor();
                let cursor_display_col = text_width::display_width(&self.buffer[..cursor]);

                if cursor_display_col >= available {
                    // Cursor is past the visible window – show window around cursor.
                    // Walk backwards from cursor to find the start byte that fits.
                    let target_start_col = cursor_display_col.saturating_sub(available / 2);
                    // Find byte offset corresponding to target_start_col
                    let mut start_byte = 0;
                    let mut start_col = 0;
                    let mut col = 0;
                    for (idx, ch) in self.buffer.char_indices() {
                        if col >= target_start_col {
                            start_byte = idx;
                            start_col = col;
                            break;
                        }
                        col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                        start_byte = idx + ch.len_utf8();
                        start_col = col;
                    }
                    view_start_col = start_col;
                    let window = &self.buffer[start_byte..];
                    text_width::truncate_to_width(window, available).into_owned()
                } else {
                    // Show from beginning, truncated to available width
                    text_width::truncate_to_width(&self.buffer, available).into_owned()
                }
            };
            write!(out, "{}{}", text_fg, display)?;
        }

        // Show status on the right if provided
        if let Some(status) = status {
            let col = cols
                .saturating_sub(text_width::display_width(status) as u16)
                .saturating_add(1);
            write!(out, "\x1b[1;{}H{}\x1b[2m{}\x1b[22m", col, bg, status)?;
        }

        // Reset styling before cursor positioning
        write!(out, "{}", reset)?;

        // Position cursor using display width and visible window offset.
        let cursor = self.sanitized_cursor();
        let cursor_display_col = text_width::display_width(&self.buffer[..cursor]);
        let cursor_in_view = cursor_display_col.saturating_sub(view_start_col);
        let cursor_col = prompt_width + cursor_in_view.min(available) + 1;
        write!(out, "\x1b[1;{}H", cursor_col)?;

        // Show cursor
        write!(out, "\x1b[?25h")?;

        out.flush()
    }

    /// Returns true if buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Returns the current buffer content.
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Clears the buffer and resets cursor.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn test_new_min_input_initializes_empty() {
        let input = MiniInput::new("Search");
        assert_eq!(input.label, "Search");
        assert!(input.buffer.is_empty());
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn test_handle_input_typing_changed() {
        let mut input = MiniInput::new("Search");

        assert_eq!(
            input.handle_input(key(KeyCode::Char('h'))),
            MiniInputResult::Changed
        );
        assert_eq!(
            input.handle_input(key(KeyCode::Char('i'))),
            MiniInputResult::Changed
        );
        assert_eq!(input.buffer, "hi");
        assert_eq!(input.cursor, 2);
    }

    #[test]
    fn test_handle_input_backspace_changed() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello".to_string();
        input.cursor = 5;

        assert_eq!(
            input.handle_input(key(KeyCode::Backspace)),
            MiniInputResult::Changed
        );
        assert_eq!(input.buffer, "hell");
        assert_eq!(input.cursor, 4);
    }

    #[test]
    fn test_handle_input_cursor_movement_positions_updated() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello".to_string();
        input.cursor = 5;

        input.handle_input(key(KeyCode::Left));
        assert_eq!(input.cursor, 4);

        input.handle_input(key(KeyCode::Home));
        assert_eq!(input.cursor, 0);

        input.handle_input(key(KeyCode::End));
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn test_handle_input_submit_and_cancel_results() {
        let mut input = MiniInput::new("Search");

        assert_eq!(
            input.handle_input(key(KeyCode::Enter)),
            MiniInputResult::Submit
        );
        assert_eq!(
            input.handle_input(key(KeyCode::Esc)),
            MiniInputResult::Cancel
        );
        assert_eq!(input.handle_input(ctrl_key('c')), MiniInputResult::Cancel);
    }

    #[test]
    fn test_handle_input_ctrl_u_clears_line_changed() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello world".to_string();
        input.cursor = 6; // After "hello "

        assert_eq!(input.handle_input(ctrl_key('u')), MiniInputResult::Changed);
        assert_eq!(input.buffer, "world");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn test_handle_input_ctrl_w_deletes_word_changed() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello world".to_string();
        input.cursor = 11; // At end

        assert_eq!(input.handle_input(ctrl_key('w')), MiniInputResult::Changed);
        assert_eq!(input.buffer, "hello ");
        assert_eq!(input.cursor, 6);
    }

    #[test]
    fn test_handle_input_ctrl_w_deletes_word_with_trailing_spaces() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello world   ".to_string();
        input.cursor = input.buffer.len(); // At end after trailing spaces

        assert_eq!(input.handle_input(ctrl_key('w')), MiniInputResult::Changed);
        assert_eq!(input.buffer, "hello ");
        assert_eq!(input.cursor, 6);
    }

    #[test]
    fn test_cjk_input_cursor_movement() {
        let mut input = MiniInput::new("Search");
        // Type "你好" (each char is 3 bytes)
        input.buffer = "你好".to_string();
        input.cursor = 6; // After both chars

        // Move left should go to byte 3 (after "你")
        input.handle_input(key(KeyCode::Left));
        assert_eq!(input.cursor, 3);

        // Move left again to byte 0
        input.handle_input(key(KeyCode::Left));
        assert_eq!(input.cursor, 0);

        // Move right to byte 3
        input.handle_input(key(KeyCode::Right));
        assert_eq!(input.cursor, 3);
    }

    #[test]
    fn test_backspace_on_multibyte_char() {
        let mut input = MiniInput::new("Search");
        input.buffer = "a你b".to_string(); // 1 + 3 + 1 = 5 bytes
        input.cursor = 4; // After "a你"

        input.handle_input(key(KeyCode::Backspace));
        assert_eq!(input.buffer, "ab");
        assert_eq!(input.cursor, 1);
    }

    #[test]
    fn test_delete_on_multibyte_char() {
        let mut input = MiniInput::new("Search");
        input.buffer = "a你b".to_string();
        input.cursor = 1; // After "a", before "你"

        input.handle_input(key(KeyCode::Delete));
        assert_eq!(input.buffer, "ab");
        assert_eq!(input.cursor, 1);
    }

    #[test]
    fn test_render_cursor_position_with_wide_chars() {
        let input = MiniInput::new("S");
        // Verify render doesn't panic with wide content
        let mut buf = Vec::new();
        // This should not panic even with very small cols
        let _ = input.render(&mut buf, 10, None);
    }

    #[test]
    fn test_render_cursor_tracks_windowed_view() {
        let mut input = MiniInput::new("S");
        input.buffer = "abcdefghijklmno".to_string();
        input.cursor = 12;

        let mut out = Vec::new();
        input.render(&mut out, 12, None).unwrap();
        let rendered = String::from_utf8_lossy(&out);

        // Prompt width is 3 ("S: "), cursor should be at column 8 in the scrolled window.
        assert!(
            rendered.ends_with("\x1b[1;8H\x1b[?25h"),
            "expected cursor at col 8 in windowed view, got: {rendered:?}"
        );
    }

    #[test]
    fn test_render_status_right_aligned_one_based_column() {
        let input = MiniInput::new("S");
        let mut out = Vec::new();
        input.render(&mut out, 20, Some("OK")).unwrap();
        let rendered = String::from_utf8_lossy(&out);

        // Width 2 status should start at column 19 in a 20-col terminal.
        assert!(
            rendered.contains("\x1b[1;19H"),
            "expected status at col 19, got: {rendered:?}"
        );
    }
}
