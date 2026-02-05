//! Scrollback viewer rendering.
//!
//! Stateless rendering functions for displaying scrollback buffer content.
//! The scroll offset is owned by App, keeping this module simple.

use std::io::{self, Write};

use super::buffer::ScrollbackBuffer;

/// Rendering options for scrollback viewer.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Starting row for content (1-indexed). Default: 1.
    /// Set to 2 to preserve row 1 for topbar.
    pub start_row: u16,
    /// Whether to show line numbers in left gutter.
    pub show_line_numbers: bool,
}

/// Statistics from a render operation.
#[derive(Debug, Clone)]
pub struct RenderStats {
    /// Number of lines rendered.
    pub lines_rendered: usize,
    /// First visible line number (1-indexed from buffer start).
    pub first_visible_line: usize,
}

/// Stateless scrollback viewer.
///
/// Provides rendering functions for scrollback content. All state (scroll offset,
/// buffer) is owned externally - this struct just handles rendering logic.
pub struct ScrollViewer;

impl ScrollViewer {
    /// Renders scrollback content to the terminal.
    ///
    /// This renders the visible portion of the scrollback buffer based on
    /// the current offset and viewport dimensions. Use `RenderOptions` to
    /// configure starting row and line numbers.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to (usually stdout)
    /// * `buffer` - The scrollback buffer to read from
    /// * `offset` - Lines from bottom (0 = most recent visible at bottom)
    /// * `cols` - Terminal width in columns
    /// * `rows` - Number of rows to render (viewport height)
    /// * `options` - Rendering options (start_row, line_numbers)
    ///
    /// # Returns
    ///
    /// RenderStats containing number of lines rendered and first visible line.
    pub fn render<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        offset: usize,
        cols: u16,
        rows: u16,
        options: &RenderOptions,
    ) -> io::Result<RenderStats> {
        let rows = rows as usize;
        let start_row = options.start_row.max(1) as usize;
        let total = buffer.len();

        // Calculate first visible line number (1-indexed)
        let first_visible_line = total
            .saturating_sub(offset)
            .saturating_sub(rows)
            .saturating_add(1)
            .max(1);

        // Hide cursor during render
        write!(out, "\x1b[?25l")?;

        // Get lines to display
        let lines: Vec<_> = buffer.get_from_bottom(offset, rows).collect();
        let mut rendered = 0;

        // Calculate gutter width if showing line numbers
        let gutter_width = if options.show_line_numbers {
            // Width needed for largest line number + separator
            let max_line = total;
            let num_width = if max_line == 0 { 1 } else { (max_line as f64).log10().floor() as usize + 1 };
            num_width.max(4) + 1 // +1 for │ separator, min 4 digits
        } else {
            0
        };
        let content_cols = (cols as usize).saturating_sub(gutter_width);

        for (i, line) in lines.iter().enumerate() {
            let screen_row = start_row + i;
            let line_number = first_visible_line + i;

            // Move to line position
            write!(out, "\x1b[{};1H", screen_row)?;

            // Clear line
            write!(out, "\x1b[2K")?;

            // Render line number gutter if enabled
            if options.show_line_numbers {
                write!(out, "\x1b[2m")?; // Dim
                write!(out, "{:>width$}│", line_number, width = gutter_width - 1)?;
                write!(out, "\x1b[22m")?; // Reset dim
            }

            // Write line content
            let content = line.content();
            if content.len() <= content_cols {
                out.write_all(content)?;
            } else {
                // Truncate to available width (simplistic - doesn't account for ANSI)
                out.write_all(&content[..content_cols])?;
            }

            // If line was truncated in storage, show indicator
            if line.is_truncated() {
                write!(out, "\x1b[7m>\x1b[27m")?; // Reverse video '>'
            }

            rendered += 1;
        }

        // Clear remaining rows if we didn't fill the viewport
        for i in rendered..rows {
            let screen_row = start_row + i;
            write!(out, "\x1b[{};1H\x1b[2K", screen_row)?;
        }

        // Show cursor
        write!(out, "\x1b[?25h")?;

        out.flush()?;
        Ok(RenderStats {
            lines_rendered: rendered,
            first_visible_line,
        })
    }

    /// Renders scrollback content preserving the topbar (starts at row 2).
    ///
    /// This is a convenience method for the common case of rendering scrollback
    /// while keeping the chrome context bar visible on row 1.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `buffer` - The scrollback buffer
    /// * `offset` - Scroll offset (lines from bottom)
    /// * `cols` - Terminal width
    /// * `rows` - Total terminal rows (content will use rows 2..rows)
    /// * `show_line_numbers` - Whether to show line number gutter
    pub fn render_with_chrome<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        offset: usize,
        cols: u16,
        rows: u16,
        show_line_numbers: bool,
    ) -> io::Result<RenderStats> {
        let content_rows = rows.saturating_sub(1); // Reserve row 1 for topbar
        let options = RenderOptions {
            start_row: 2,
            show_line_numbers,
        };
        Self::render(out, buffer, offset, cols, content_rows, &options)
    }

    /// Calculates the maximum valid scroll offset.
    ///
    /// # Arguments
    ///
    /// * `total_lines` - Total lines in buffer
    /// * `viewport_rows` - Number of visible rows
    ///
    /// # Returns
    ///
    /// Maximum offset value (0 if buffer smaller than viewport).
    #[inline]
    pub fn max_offset(total_lines: usize, viewport_rows: usize) -> usize {
        total_lines.saturating_sub(viewport_rows)
    }

    /// Clamps a scroll offset to valid range.
    ///
    /// # Arguments
    ///
    /// * `offset` - Proposed offset
    /// * `total_lines` - Total lines in buffer
    /// * `viewport_rows` - Number of visible rows
    ///
    /// # Returns
    ///
    /// Clamped offset value.
    #[inline]
    pub fn clamp_offset(offset: usize, total_lines: usize, viewport_rows: usize) -> usize {
        offset.min(Self::max_offset(total_lines, viewport_rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_buffer(lines: &[&str]) -> ScrollbackBuffer {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        for line in lines {
            buffer.push_line(line.as_bytes().to_vec());
        }
        buffer
    }

    #[test]
    fn test_max_offset_empty_buffer() {
        assert_eq!(ScrollViewer::max_offset(0, 10), 0);
    }

    #[test]
    fn test_max_offset_buffer_smaller_than_viewport() {
        assert_eq!(ScrollViewer::max_offset(5, 10), 0);
    }

    #[test]
    fn test_max_offset_buffer_larger_than_viewport() {
        assert_eq!(ScrollViewer::max_offset(100, 10), 90);
    }

    #[test]
    fn test_clamp_offset() {
        // Within range
        assert_eq!(ScrollViewer::clamp_offset(50, 100, 10), 50);
        // Above max
        assert_eq!(ScrollViewer::clamp_offset(95, 100, 10), 90);
        // At max
        assert_eq!(ScrollViewer::clamp_offset(90, 100, 10), 90);
    }

    #[test]
    fn test_render_to_buffer() {
        let buffer = create_test_buffer(&["line1", "line2", "line3"]);
        let mut output = Vec::new();
        let options = RenderOptions::default();

        let stats = ScrollViewer::render(&mut output, &buffer, 0, 80, 3, &options).unwrap();
        assert_eq!(stats.lines_rendered, 3);

        // Output should contain the lines
        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("line1"));
        assert!(output_str.contains("line2"));
        assert!(output_str.contains("line3"));
    }

    #[test]
    fn test_render_with_offset() {
        let buffer = create_test_buffer(&["line1", "line2", "line3", "line4", "line5"]);
        let mut output = Vec::new();
        let options = RenderOptions::default();

        // Offset 2 from bottom, show 2 lines
        let stats = ScrollViewer::render(&mut output, &buffer, 2, 80, 2, &options).unwrap();
        assert_eq!(stats.lines_rendered, 2);

        let output_str = String::from_utf8_lossy(&output);
        // Should show line2 and line3 (offset 2 from bottom with 2 rows)
        assert!(output_str.contains("line2"));
        assert!(output_str.contains("line3"));
    }

    #[test]
    fn test_render_with_chrome() {
        let buffer = create_test_buffer(&["line1", "line2", "line3", "line4", "line5"]);
        let mut output = Vec::new();

        // 5 total rows, row 1 reserved for topbar, so 4 content rows
        let stats = ScrollViewer::render_with_chrome(&mut output, &buffer, 0, 80, 5, false).unwrap();
        assert_eq!(stats.lines_rendered, 4);
        assert_eq!(stats.first_visible_line, 2); // lines 2,3,4,5 visible (offset 0)

        let output_str = String::from_utf8_lossy(&output);
        // Should start at row 2
        assert!(output_str.contains("\x1b[2;1H"));
    }

    #[test]
    fn test_render_with_line_numbers() {
        let buffer = create_test_buffer(&["hello", "world"]);
        let mut output = Vec::new();

        let stats = ScrollViewer::render_with_chrome(&mut output, &buffer, 0, 80, 5, true).unwrap();
        assert_eq!(stats.lines_rendered, 2);

        let output_str = String::from_utf8_lossy(&output);
        // Should contain line numbers with dim formatting
        assert!(output_str.contains("│")); // Gutter separator
    }

    #[test]
    fn test_first_visible_line_calculation() {
        let buffer = create_test_buffer(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]);
        let mut output = Vec::new();

        // At bottom (offset 0), viewing 3 lines: should show lines 8,9,10
        let stats = ScrollViewer::render_with_chrome(&mut output, &buffer, 0, 80, 4, false).unwrap();
        assert_eq!(stats.first_visible_line, 8);

        // Scrolled up 3 lines: should show lines 5,6,7
        output.clear();
        let stats = ScrollViewer::render_with_chrome(&mut output, &buffer, 3, 80, 4, false).unwrap();
        assert_eq!(stats.first_visible_line, 5);
    }
}
