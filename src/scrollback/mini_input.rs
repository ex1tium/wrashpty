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
                        let word_start = before
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
    pub fn render<W: Write>(
        &self,
        out: &mut W,
        cols: u16,
        status: Option<&str>,
    ) -> io::Result<()> {
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

        // Build the prompt: "Label: "
        let prompt = format!("{}: ", self.label);
        let prompt_len = prompt.len();

        // Render the label with styling
        write!(out, "{}{}{}", label_fg, prompt, reset)?;
        if !bg.is_empty() {
            write!(out, "{}", bg)?; // Re-apply background after reset
        }

        // Calculate available width for input
        let status_len = status.map(|s| s.len() + 2).unwrap_or(0); // " [status]"
        let available = (cols as usize).saturating_sub(prompt_len + status_len + 1);

        // Show buffer (or hint if empty)
        if self.buffer.is_empty() {
            if let Some(hint) = self.hint {
                // Hint in dim
                write!(out, "\x1b[2m{}\x1b[22m", &hint[..hint.len().min(available)])?;
            }
        } else {
            // Truncate if too long (show end of buffer if cursor is at end)
            let display = if self.buffer.len() <= available {
                &self.buffer
            } else if self.cursor >= available {
                // Show window around cursor
                let start = self.cursor.saturating_sub(available / 2);
                &self.buffer[start..self.buffer.len().min(start + available)]
            } else {
                &self.buffer[..available]
            };
            write!(out, "{}{}", text_fg, display)?;
        }

        // Show status on the right if provided
        if let Some(status) = status {
            let col = cols.saturating_sub(status.len() as u16);
            write!(out, "\x1b[1;{}H{}\x1b[2m{}\x1b[22m", col, bg, status)?;
        }

        // Reset styling before cursor positioning
        write!(out, "{}", reset)?;

        // Position cursor
        let cursor_col = prompt_len + self.cursor.min(available) + 1;
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
    fn test_new() {
        let input = MiniInput::new("Search");
        assert_eq!(input.label, "Search");
        assert!(input.buffer.is_empty());
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn test_typing() {
        let mut input = MiniInput::new("Search");

        assert_eq!(input.handle_input(key(KeyCode::Char('h'))), MiniInputResult::Changed);
        assert_eq!(input.handle_input(key(KeyCode::Char('i'))), MiniInputResult::Changed);
        assert_eq!(input.buffer, "hi");
        assert_eq!(input.cursor, 2);
    }

    #[test]
    fn test_backspace() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello".to_string();
        input.cursor = 5;

        assert_eq!(input.handle_input(key(KeyCode::Backspace)), MiniInputResult::Changed);
        assert_eq!(input.buffer, "hell");
        assert_eq!(input.cursor, 4);
    }

    #[test]
    fn test_cursor_movement() {
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
    fn test_submit_cancel() {
        let mut input = MiniInput::new("Search");

        assert_eq!(input.handle_input(key(KeyCode::Enter)), MiniInputResult::Submit);
        assert_eq!(input.handle_input(key(KeyCode::Esc)), MiniInputResult::Cancel);
        assert_eq!(input.handle_input(ctrl_key('c')), MiniInputResult::Cancel);
    }

    #[test]
    fn test_ctrl_u_clear_line() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello world".to_string();
        input.cursor = 6; // After "hello "

        assert_eq!(input.handle_input(ctrl_key('u')), MiniInputResult::Changed);
        assert_eq!(input.buffer, "world");
        assert_eq!(input.cursor, 0);
    }

    #[test]
    fn test_ctrl_w_delete_word() {
        let mut input = MiniInput::new("Search");
        input.buffer = "hello world".to_string();
        input.cursor = 11; // At end

        assert_eq!(input.handle_input(ctrl_key('w')), MiniInputResult::Changed);
        assert_eq!(input.buffer, "hello ");
        assert_eq!(input.cursor, 6);
    }
}
