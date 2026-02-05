//! Scrollback viewer rendering.
//!
//! Stateless rendering functions for displaying scrollback buffer content.
//! The scroll offset is owned by App, keeping this module simple.

use std::io::{self, Write};

use super::buffer::ScrollbackBuffer;
use super::features::{FilterState, SearchState};
use crate::chrome::theme::Theme;

/// Rendering options for scrollback viewer.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Starting row for content (1-indexed). Default: 1.
    /// Set to 2 to preserve row 1 for topbar.
    pub start_row: u16,
    /// Whether to show line numbers in left gutter.
    pub show_line_numbers: bool,
    /// Whether to show relative timestamps in left gutter.
    pub show_timestamps: bool,
    /// Show "END" marker when at the bottom of the buffer.
    pub show_end_marker: bool,
    /// Show "BEGIN" marker when at the top of the buffer.
    pub show_begin_marker: bool,
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
        theme: Option<&Theme>,
    ) -> io::Result<RenderStats> {
        let rows = rows as usize;
        let start_row = options.start_row.max(1) as usize;
        let total = buffer.len();

        // Calculate boundary conditions
        let max_offset = Self::max_offset(total, rows);
        let is_at_bottom = offset == 0;
        let is_at_top = offset >= max_offset && total > 0;

        // Determine if we need to show markers
        let show_end = options.show_end_marker && is_at_bottom && total > 0;
        let show_begin = options.show_begin_marker && is_at_top && total > 0;

        // Calculate how many content rows available
        // Markers only "cost" a row when buffer would otherwise fill the viewport
        let buffer_fills_viewport = total >= rows;
        let end_cost = if show_end && buffer_fills_viewport { 1 } else { 0 };
        let begin_cost = if show_begin && buffer_fills_viewport { 1 } else { 0 };
        let content_rows = rows.saturating_sub(end_cost).saturating_sub(begin_cost);

        // Get lines to display and calculate first visible line number
        // When at top (BEGIN showing), fetch from line 1 using get_range
        // Otherwise use offset-based get_from_bottom
        let (first_visible_line, lines): (usize, Vec<_>) = if show_begin {
            (1, buffer.get_range(0, content_rows).collect())
        } else {
            let first = total
                .saturating_sub(offset)
                .saturating_sub(content_rows)
                .saturating_add(1)
                .max(1);
            (first, buffer.get_from_bottom(offset, content_rows).collect())
        };

        // Hide cursor during render
        write!(out, "\x1b[?25l")?;

        // Calculate gutter width for line numbers
        // Format: "  42 │ " = num_width + 3 (space + │ + space)
        let line_num_width = if options.show_line_numbers {
            let max_line = total;
            let num_width =
                if max_line == 0 { 1 } else { (max_line as f64).log10().floor() as usize + 1 };
            num_width.max(4) + 3 // +3 for " │ " (space + separator + space)
        } else {
            0
        };

        // Calculate timestamp gutter width
        // Format: " 5m │ " = 6 chars minimum (time + unit + space + │ + space)
        let timestamp_width = if options.show_timestamps { 7 } else { 0 };

        let gutter_width = line_num_width + timestamp_width;
        let content_cols = (cols as usize).saturating_sub(gutter_width);

        // Get current time for relative timestamp calculation
        let now = std::time::Instant::now();

        let mut current_row = start_row;
        let mut rendered = 0;

        // Render BEGIN marker if at top
        if show_begin {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            Self::render_boundary_marker_styled(out, cols as usize, gutter_width, "BEGIN", theme)?;
            current_row += 1;
        }

        for (i, line) in lines.iter().enumerate() {
            let screen_row = current_row;
            let line_number = first_visible_line + i;

            // Move to line position
            write!(out, "\x1b[{};1H", screen_row)?;

            // Clear line
            write!(out, "\x1b[2K")?;

            // Render timestamp gutter if enabled
            // Format: " 5m │ " showing relative time since capture
            if options.show_timestamps {
                let elapsed = now.duration_since(line.timestamp());
                let time_str = Self::format_relative_time(elapsed);
                write!(out, "\x1b[2m")?; // Dim
                write!(out, "{:>4} │ ", time_str)?;
                write!(out, "\x1b[22m")?; // Reset dim
            }

            // Render line number gutter if enabled
            // Format: "  42 │ " with spaces around separator for easier copy-paste
            if options.show_line_numbers {
                write!(out, "\x1b[2m")?; // Dim
                write!(out, "{:>width$} │ ", line_number, width = line_num_width - 3)?;
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

            current_row += 1;
            rendered += 1;
        }

        // Render END marker if at bottom
        if show_end {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            Self::render_boundary_marker_styled(out, cols as usize, gutter_width, "END", theme)?;
            current_row += 1;
        }

        // Clear any remaining rows
        while current_row < start_row + rows {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            current_row += 1;
        }

        // Show cursor
        write!(out, "\x1b[?25h")?;

        out.flush()?;
        Ok(RenderStats {
            lines_rendered: rendered,
            first_visible_line,
        })
    }

    /// Formats a duration as a relative time string.
    ///
    /// Returns strings like "0s", "5s", "2m", "1h", "3d".
    fn format_relative_time(duration: std::time::Duration) -> String {
        let secs = duration.as_secs();
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else if secs < 86400 {
            format!("{}h", secs / 3600)
        } else {
            format!("{}d", secs / 86400)
        }
    }

    /// Renders a centered boundary marker (BEGIN/END).
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `cols` - Terminal width
    /// * `gutter_width` - Width of line number/timestamp gutter
    /// * `label` - Marker text ("BEGIN" or "END")
    /// * `theme` - Optional theme for colored markers (uses dim if None)
    fn render_boundary_marker<W: Write>(
        out: &mut W,
        cols: usize,
        gutter_width: usize,
        label: &str,
    ) -> io::Result<()> {
        Self::render_boundary_marker_styled(out, cols, gutter_width, label, None)
    }

    /// Renders a centered boundary marker with optional theme styling.
    fn render_boundary_marker_styled<W: Write>(
        out: &mut W,
        cols: usize,
        gutter_width: usize,
        label: &str,
        theme: Option<&Theme>,
    ) -> io::Result<()> {
        use crate::chrome::segments::color_to_fg_ansi;

        let content_cols = cols.saturating_sub(gutter_width);

        // Build marker: "─────── LABEL ───────"
        let label_with_spaces = format!(" {} ", label);
        let label_len = label_with_spaces.len();
        let dashes_total = content_cols.saturating_sub(label_len);
        let dashes_left = dashes_total / 2;
        let dashes_right = dashes_total - dashes_left;

        // Render gutter spacer if needed
        if gutter_width > 0 {
            write!(out, "{:width$}", "", width = gutter_width)?;
        }

        // Apply color - either themed or dim
        if let Some(theme) = theme {
            let fg = color_to_fg_ansi(theme.marker_fg);
            write!(out, "{}", fg)?;
        } else {
            write!(out, "\x1b[2m")?; // Dim fallback
        }

        // Render the marker line
        for _ in 0..dashes_left {
            write!(out, "─")?;
        }
        write!(out, "{}", label_with_spaces)?;
        for _ in 0..dashes_right {
            write!(out, "─")?;
        }

        // Reset styling
        if theme.is_some() {
            write!(out, "\x1b[39m")?; // Reset fg
        } else {
            write!(out, "\x1b[22m")?; // Reset dim
        }

        Ok(())
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
    /// * `show_timestamps` - Whether to show relative timestamp gutter
    /// * `show_boundary_markers` - Whether to show BEGIN/END markers at buffer boundaries
    /// * `theme` - Optional theme for styled boundary markers
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_chrome<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        offset: usize,
        cols: u16,
        rows: u16,
        show_line_numbers: bool,
        show_timestamps: bool,
        show_boundary_markers: bool,
        theme: Option<&Theme>,
    ) -> io::Result<RenderStats> {
        let content_rows = rows.saturating_sub(1); // Reserve row 1 for topbar
        let options = RenderOptions {
            start_row: 2,
            show_line_numbers,
            show_timestamps,
            show_end_marker: show_boundary_markers,
            show_begin_marker: show_boundary_markers,
        };
        Self::render(out, buffer, offset, cols, content_rows, &options, theme)
    }

    /// Renders scrollback content with search highlighting.
    ///
    /// This extends `render_with_chrome` by adding visual highlights for
    /// search matches. The current match is highlighted more prominently
    /// than other matches.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `buffer` - The scrollback buffer
    /// * `offset` - Scroll offset (lines from bottom)
    /// * `cols` - Terminal width
    /// * `rows` - Total terminal rows
    /// * `show_line_numbers` - Whether to show line number gutter
    /// * `show_timestamps` - Whether to show relative timestamp gutter
    /// * `show_boundary_markers` - Whether to show BEGIN/END markers
    /// * `search` - Search state with matches to highlight
    /// * `theme` - Theme for highlight colors
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_search<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        offset: usize,
        cols: u16,
        rows: u16,
        show_line_numbers: bool,
        show_timestamps: bool,
        show_boundary_markers: bool,
        search: &SearchState,
        theme: &Theme,
    ) -> io::Result<RenderStats> {
        let content_rows = rows.saturating_sub(1) as usize; // Reserve row 1 for topbar
        let total = buffer.len();

        // Calculate boundary conditions
        let max_offset = Self::max_offset(total, content_rows);
        let is_at_bottom = offset == 0;
        let is_at_top = offset >= max_offset && total > 0;

        // Determine if we need to show markers
        let show_end = show_boundary_markers && is_at_bottom && total > 0;
        let show_begin = show_boundary_markers && is_at_top && total > 0;

        // Calculate marker row costs
        let buffer_fills_viewport = total >= content_rows;
        let end_cost = if show_end && buffer_fills_viewport { 1 } else { 0 };
        let begin_cost = if show_begin && buffer_fills_viewport { 1 } else { 0 };
        let available_rows = content_rows.saturating_sub(end_cost).saturating_sub(begin_cost);

        // Get lines to display and calculate first visible line number
        let (first_visible_line, lines): (usize, Vec<_>) = if show_begin {
            (1, buffer.get_range(0, available_rows).collect())
        } else {
            let first = total
                .saturating_sub(offset)
                .saturating_sub(available_rows)
                .saturating_add(1)
                .max(1);
            (first, buffer.get_from_bottom(offset, available_rows).collect())
        };

        // Calculate gutter widths
        let line_num_width = if show_line_numbers {
            let max_line = total;
            let num_width =
                if max_line == 0 { 1 } else { (max_line as f64).log10().floor() as usize + 1 };
            num_width.max(4) + 3
        } else {
            0
        };
        let timestamp_width = if show_timestamps { 7 } else { 0 };
        let gutter_width = line_num_width + timestamp_width;
        let content_cols = (cols as usize).saturating_sub(gutter_width);

        // Get current time for relative timestamp calculation
        let now = std::time::Instant::now();

        // Hide cursor during render
        write!(out, "\x1b[?25l")?;

        let mut current_row = 2; // Start at row 2 (after topbar)
        let mut rendered = 0;

        // Render BEGIN marker if at top (with theme styling)
        if show_begin {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            Self::render_boundary_marker_styled(out, cols as usize, gutter_width, "BEGIN", Some(theme))?;
            current_row += 1;
        }

        // Get current match line for special highlighting
        let current_match = search.current();

        for (i, line) in lines.iter().enumerate() {
            let line_index = first_visible_line + i - 1; // Convert to 0-indexed
            let line_number = first_visible_line + i;

            // Move to line position
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;

            // Render timestamp gutter if enabled
            if show_timestamps {
                let elapsed = now.duration_since(line.timestamp());
                let time_str = Self::format_relative_time(elapsed);
                write!(out, "\x1b[2m{:>4} │ \x1b[22m", time_str)?;
            }

            // Render line number gutter if enabled
            if show_line_numbers {
                write!(out, "\x1b[2m{:>width$} │ \x1b[22m", line_number, width = line_num_width - 3)?;
            }

            // Get matches for this line
            let line_matches: Vec<_> = search.matches_on_line(line_index).collect();

            if line_matches.is_empty() {
                // No matches - render line normally
                let content = line.content();
                if content.len() <= content_cols {
                    out.write_all(content)?;
                } else {
                    out.write_all(&content[..content_cols])?;
                }
            } else {
                // Render line with search highlights
                Self::render_line_with_highlights(
                    out,
                    line.content(),
                    content_cols,
                    &line_matches,
                    current_match.map(|m| m.line == line_index && m.start == line_matches[0].start),
                    theme,
                )?;
            }

            // If line was truncated in storage, show indicator
            if line.is_truncated() {
                write!(out, "\x1b[7m>\x1b[27m")?;
            }

            current_row += 1;
            rendered += 1;
        }

        // Render END marker if at bottom (with theme styling)
        if show_end {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            Self::render_boundary_marker_styled(out, cols as usize, gutter_width, "END", Some(theme))?;
            current_row += 1;
        }

        // Clear remaining rows
        let start_row = 2;
        while current_row < start_row + content_rows {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            current_row += 1;
        }

        // Show cursor
        write!(out, "\x1b[?25h")?;
        out.flush()?;

        Ok(RenderStats {
            lines_rendered: rendered,
            first_visible_line,
        })
    }

    /// Renders a line with search match highlighting.
    fn render_line_with_highlights<W: Write>(
        out: &mut W,
        content: &[u8],
        max_cols: usize,
        matches: &[&super::features::SearchMatch],
        is_current: Option<bool>,
        theme: &Theme,
    ) -> io::Result<()> {
        use crate::chrome::segments::color_to_bg_ansi;

        let content_str = String::from_utf8_lossy(content);
        let mut pos = 0;

        // Sort matches by start position
        let mut sorted_matches = matches.to_vec();
        sorted_matches.sort_by_key(|m| m.start);

        for search_match in sorted_matches {
            // Write text before match
            if search_match.start > pos {
                let before = &content_str[pos..search_match.start.min(content_str.len())];
                write!(out, "{}", before)?;
            }

            // Determine highlight color based on whether this is the current match
            let is_current_match = is_current.unwrap_or(false);
            let bg_color = if is_current_match {
                theme.search_current_bg
            } else {
                theme.search_other_bg
            };
            let bg_ansi = color_to_bg_ansi(bg_color);

            // Write highlighted match
            let match_end = search_match.end.min(content_str.len());
            let match_text = &content_str[search_match.start.min(content_str.len())..match_end];
            write!(out, "{}{}\x1b[49m", bg_ansi, match_text)?; // 49 resets bg

            pos = match_end;
        }

        // Write remaining text after last match
        if pos < content_str.len() {
            let remaining = &content_str[pos..];
            // Truncate if needed
            if remaining.len() <= max_cols.saturating_sub(pos) {
                write!(out, "{}", remaining)?;
            }
        }

        Ok(())
    }

    /// Renders scrollback with filter mode active (showing only matching lines).
    ///
    /// This extends render_with_chrome by only displaying lines that match
    /// the filter pattern. Original line numbers are preserved.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `buffer` - The scrollback buffer
    /// * `filter` - Filter state with matching lines
    /// * `filter_offset` - Scroll offset within filtered lines
    /// * `cols` - Terminal width
    /// * `rows` - Total terminal rows
    /// * `show_line_numbers` - Whether to show line number gutter
    /// * `show_timestamps` - Whether to show relative timestamp gutter
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_filter<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        filter: &FilterState,
        filter_offset: usize,
        cols: u16,
        rows: u16,
        show_line_numbers: bool,
        show_timestamps: bool,
    ) -> io::Result<RenderStats> {
        let content_rows = rows.saturating_sub(1) as usize; // Reserve row 1 for topbar
        let _filtered_total = filter.match_count();

        // Calculate gutter widths based on original buffer
        let total = buffer.len();
        let line_num_width = if show_line_numbers {
            let max_line = total;
            let num_width =
                if max_line == 0 { 1 } else { (max_line as f64).log10().floor() as usize + 1 };
            num_width.max(4) + 3
        } else {
            0
        };
        let timestamp_width = if show_timestamps { 7 } else { 0 };
        let gutter_width = line_num_width + timestamp_width;
        let content_cols = (cols as usize).saturating_sub(gutter_width);

        // Get current time for relative timestamp calculation
        let now = std::time::Instant::now();

        // Hide cursor during render
        write!(out, "\x1b[?25l")?;

        let mut current_row = 2; // Start at row 2 (after topbar)
        let mut rendered = 0;

        // Get filtered lines for the current viewport
        let lines = filter.get_filtered_range(buffer, filter_offset, content_rows);

        // Track first visible line
        let first_visible_line = lines.first().map(|(idx, _)| *idx + 1).unwrap_or(1);

        for (original_idx, line) in &lines {
            let line_number = *original_idx + 1; // Convert to 1-indexed

            // Move to line position and clear
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;

            // Render timestamp gutter if enabled
            if show_timestamps {
                let elapsed = now.duration_since(line.timestamp());
                let time_str = Self::format_relative_time(elapsed);
                write!(out, "\x1b[2m{:>4} │ \x1b[22m", time_str)?;
            }

            // Render line number gutter if enabled
            if show_line_numbers {
                write!(out, "\x1b[2m{:>width$} │ \x1b[22m", line_number, width = line_num_width - 3)?;
            }

            // Write line content
            let content = line.content();
            if content.len() <= content_cols {
                out.write_all(content)?;
            } else {
                out.write_all(&content[..content_cols])?;
            }

            // If line was truncated in storage, show indicator
            if line.is_truncated() {
                write!(out, "\x1b[7m>\x1b[27m")?;
            }

            current_row += 1;
            rendered += 1;
        }

        // Clear remaining rows
        let start_row = 2;
        while current_row < start_row + content_rows {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            current_row += 1;
        }

        // Show cursor
        write!(out, "\x1b[?25h")?;
        out.flush()?;

        Ok(RenderStats {
            lines_rendered: rendered,
            first_visible_line,
        })
    }

    /// Renders scrollback with filter AND search active (filtered lines with highlights).
    ///
    /// This combines filter mode (showing only matching lines) with search
    /// highlighting (visual highlights for search matches within filtered lines).
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to
    /// * `buffer` - The scrollback buffer
    /// * `filter` - Filter state with matching lines
    /// * `filter_offset` - Scroll offset within filtered lines
    /// * `cols` - Terminal width
    /// * `rows` - Total terminal rows
    /// * `show_line_numbers` - Whether to show line number gutter
    /// * `show_timestamps` - Whether to show relative timestamp gutter
    /// * `search` - Search state with matches to highlight
    /// * `theme` - Theme for highlight colors
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_filter_and_search<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        filter: &FilterState,
        filter_offset: usize,
        cols: u16,
        rows: u16,
        show_line_numbers: bool,
        show_timestamps: bool,
        search: &SearchState,
        theme: &Theme,
    ) -> io::Result<RenderStats> {
        let content_rows = rows.saturating_sub(1) as usize; // Reserve row 1 for topbar

        // Calculate gutter widths based on original buffer
        let total = buffer.len();
        let line_num_width = if show_line_numbers {
            let max_line = total;
            let num_width =
                if max_line == 0 { 1 } else { (max_line as f64).log10().floor() as usize + 1 };
            num_width.max(4) + 3
        } else {
            0
        };
        let timestamp_width = if show_timestamps { 7 } else { 0 };
        let gutter_width = line_num_width + timestamp_width;
        let content_cols = (cols as usize).saturating_sub(gutter_width);

        // Get current time for relative timestamp calculation
        let now = std::time::Instant::now();

        // Hide cursor during render
        write!(out, "\x1b[?25l")?;

        let mut current_row = 2; // Start at row 2 (after topbar)
        let mut rendered = 0;

        // Get filtered lines for the current viewport
        let lines = filter.get_filtered_range(buffer, filter_offset, content_rows);

        // Track first visible line
        let first_visible_line = lines.first().map(|(idx, _)| *idx + 1).unwrap_or(1);

        // Get current search match for special highlighting
        let current_match = search.current();

        for (original_idx, line) in &lines {
            let line_number = *original_idx + 1; // Convert to 1-indexed

            // Move to line position and clear
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;

            // Render timestamp gutter if enabled
            if show_timestamps {
                let elapsed = now.duration_since(line.timestamp());
                let time_str = Self::format_relative_time(elapsed);
                write!(out, "\x1b[2m{:>4} │ \x1b[22m", time_str)?;
            }

            // Render line number gutter if enabled
            if show_line_numbers {
                write!(out, "\x1b[2m{:>width$} │ \x1b[22m", line_number, width = line_num_width - 3)?;
            }

            // Get search matches for this line
            let line_matches: Vec<_> = search.matches_on_line(*original_idx).collect();

            if line_matches.is_empty() {
                // No matches - render line normally
                let content = line.content();
                if content.len() <= content_cols {
                    out.write_all(content)?;
                } else {
                    out.write_all(&content[..content_cols])?;
                }
            } else {
                // Render line with search highlights
                Self::render_line_with_highlights(
                    out,
                    line.content(),
                    content_cols,
                    &line_matches,
                    current_match.map(|m| m.line == *original_idx && m.start == line_matches[0].start),
                    theme,
                )?;
            }

            // If line was truncated in storage, show indicator
            if line.is_truncated() {
                write!(out, "\x1b[7m>\x1b[27m")?;
            }

            current_row += 1;
            rendered += 1;
        }

        // Clear remaining rows
        let start_row = 2;
        while current_row < start_row + content_rows {
            write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
            current_row += 1;
        }

        // Show cursor
        write!(out, "\x1b[?25h")?;
        out.flush()?;

        Ok(RenderStats {
            lines_rendered: rendered,
            first_visible_line,
        })
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

        let stats = ScrollViewer::render(&mut output, &buffer, 0, 80, 3, &options, None).unwrap();
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
        let stats = ScrollViewer::render(&mut output, &buffer, 2, 80, 2, &options, None).unwrap();
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
        let stats =
            ScrollViewer::render_with_chrome(&mut output, &buffer, 0, 80, 5, false, false, false, None)
                .unwrap();
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

        let stats =
            ScrollViewer::render_with_chrome(&mut output, &buffer, 0, 80, 5, true, false, false, None)
                .unwrap();
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
        let stats =
            ScrollViewer::render_with_chrome(&mut output, &buffer, 0, 80, 4, false, false, false, None)
                .unwrap();
        assert_eq!(stats.first_visible_line, 8);

        // Scrolled up 3 lines: should show lines 5,6,7
        output.clear();
        let stats =
            ScrollViewer::render_with_chrome(&mut output, &buffer, 3, 80, 4, false, false, false, None)
                .unwrap();
        assert_eq!(stats.first_visible_line, 5);
    }

    #[test]
    fn test_boundary_markers() {
        let buffer = create_test_buffer(&["line1", "line2"]);
        let mut output = Vec::new();

        // Buffer smaller than viewport (2 lines in 5 rows), at bottom - should show END
        let stats =
            ScrollViewer::render_with_chrome(&mut output, &buffer, 0, 80, 5, false, false, true, None)
                .unwrap();
        assert_eq!(stats.lines_rendered, 2);

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("END"), "Should contain END marker");

        // At top (max offset), should show BEGIN
        output.clear();
        let max_off = ScrollViewer::max_offset(2, 4); // 0 since buffer < viewport
        let _stats =
            ScrollViewer::render_with_chrome(&mut output, &buffer, max_off, 80, 5, false, false, true, None)
                .unwrap();

        let output_str = String::from_utf8_lossy(&output);
        // When buffer is smaller than viewport, BEGIN should show at top
        assert!(output_str.contains("BEGIN"), "Should contain BEGIN marker");
    }
}
