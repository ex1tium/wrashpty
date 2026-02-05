//! Scrollback viewer rendering.
//!
//! Stateless rendering functions for displaying scrollback buffer content.
//! The scroll offset is owned by App, keeping this module simple.

use std::io::{self, Write};

use super::buffer::ScrollbackBuffer;

/// Stateless scrollback viewer.
///
/// Provides rendering functions for scrollback content. All state (scroll offset,
/// buffer) is owned externally - this struct just handles rendering logic.
pub struct ScrollViewer;

impl ScrollViewer {
    /// Renders scrollback content to the terminal.
    ///
    /// This renders the visible portion of the scrollback buffer based on
    /// the current offset and viewport dimensions.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to (usually stdout)
    /// * `buffer` - The scrollback buffer to read from
    /// * `offset` - Lines from bottom (0 = most recent visible at bottom)
    /// * `cols` - Terminal width in columns
    /// * `rows` - Number of rows to render (viewport height)
    ///
    /// # Returns
    ///
    /// The number of lines actually rendered.
    pub fn render<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        offset: usize,
        cols: u16,
        rows: u16,
    ) -> io::Result<usize> {
        let rows = rows as usize;

        // Hide cursor during render
        write!(out, "\x1b[?25l")?;

        // Move to top-left of viewport
        write!(out, "\x1b[H")?;

        // Get lines to display
        let lines: Vec<_> = buffer.get_from_bottom(offset, rows).collect();
        let mut rendered = 0;

        for (i, line) in lines.iter().enumerate() {
            // Move to line position
            write!(out, "\x1b[{};1H", i + 1)?;

            // Clear line
            write!(out, "\x1b[2K")?;

            // Write line content
            let content = line.content();
            if content.len() <= cols as usize {
                out.write_all(content)?;
            } else {
                // Truncate to terminal width (simplistic - doesn't account for ANSI)
                out.write_all(&content[..cols as usize])?;
            }

            // If line was truncated in storage, show indicator
            if line.is_truncated() {
                write!(out, "\x1b[7m>\x1b[27m")?; // Reverse video '>'
            }

            rendered += 1;
        }

        // Clear remaining rows if we didn't fill the viewport
        for i in rendered..rows {
            write!(out, "\x1b[{};1H\x1b[2K", i + 1)?;
        }

        // Show cursor
        write!(out, "\x1b[?25h")?;

        out.flush()?;
        Ok(rendered)
    }

    /// Renders a scroll position indicator bar.
    ///
    /// Shows a bar at the bottom of the screen indicating scroll position.
    /// Format: "──────── SCROLL 45% ────────"
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `offset` - Current scroll offset (lines from bottom)
    /// * `total_lines` - Total lines in buffer
    /// * `viewport_rows` - Number of visible rows
    /// * `cols` - Terminal width
    /// * `row` - Row to render the indicator on (1-indexed)
    pub fn render_indicator<W: Write>(
        out: &mut W,
        offset: usize,
        total_lines: usize,
        viewport_rows: usize,
        cols: u16,
        row: u16,
    ) -> io::Result<()> {
        // Calculate percentage (how far from bottom)
        // offset 0 = at bottom (0%)
        // offset = total - viewport = at top (100%)
        let max_offset = total_lines.saturating_sub(viewport_rows);
        let percentage = if max_offset == 0 {
            0
        } else {
            (offset * 100) / max_offset
        };

        // Build indicator string
        let indicator = format!(" SCROLL {}% ", percentage);
        let indicator_len = indicator.len();

        // Calculate padding
        let cols = cols as usize;
        let available = cols.saturating_sub(indicator_len);
        let left_pad = available / 2;
        let right_pad = available - left_pad;

        // Move to position and render
        write!(out, "\x1b[{};1H", row)?;
        write!(out, "\x1b[2K")?; // Clear line

        // Use dim color for the bar
        write!(out, "\x1b[2m")?; // Dim

        // Left dashes
        for _ in 0..left_pad {
            write!(out, "─")?;
        }

        // Indicator (highlighted)
        write!(out, "\x1b[22;7m")?; // Normal intensity, reverse video
        write!(out, "{}", indicator)?;
        write!(out, "\x1b[27;2m")?; // No reverse, dim again

        // Right dashes
        for _ in 0..right_pad {
            write!(out, "─")?;
        }

        write!(out, "\x1b[0m")?; // Reset attributes

        out.flush()
    }

    /// Renders both content and indicator together.
    ///
    /// This is a convenience method that renders scrollback content and
    /// the position indicator in one call.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `buffer` - The scrollback buffer
    /// * `offset` - Scroll offset (lines from bottom)
    /// * `cols` - Terminal width
    /// * `rows` - Total viewport rows (indicator uses last row)
    pub fn render_with_indicator<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        offset: usize,
        cols: u16,
        rows: u16,
    ) -> io::Result<usize> {
        if rows < 2 {
            // Not enough space for content + indicator
            return Ok(0);
        }

        // Reserve last row for indicator
        let content_rows = rows - 1;

        // Render content
        let rendered = Self::render(out, buffer, offset, cols, content_rows)?;

        // Render indicator on last row
        Self::render_indicator(out, offset, buffer.len(), content_rows as usize, cols, rows)?;

        Ok(rendered)
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

        let rendered = ScrollViewer::render(&mut output, &buffer, 0, 80, 3).unwrap();
        assert_eq!(rendered, 3);

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

        // Offset 2 from bottom, show 2 lines
        let rendered = ScrollViewer::render(&mut output, &buffer, 2, 80, 2).unwrap();
        assert_eq!(rendered, 2);

        let output_str = String::from_utf8_lossy(&output);
        // Should show line2 and line3 (offset 2 from bottom with 2 rows)
        assert!(output_str.contains("line2"));
        assert!(output_str.contains("line3"));
    }

    #[test]
    fn test_render_indicator() {
        let mut output = Vec::new();
        ScrollViewer::render_indicator(&mut output, 50, 100, 10, 80, 10).unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("SCROLL"));
        // 50 out of 90 max = ~55%
        assert!(output_str.contains("55%") || output_str.contains("56%"));
    }
}
