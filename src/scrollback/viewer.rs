//! Stateless scrollback rendering utilities for display output.
//! Scroll offset ownership stays in `App`; this module only renders views.

use std::collections::HashSet;
use std::io::{self, Write};

use super::boundaries::CommandRecord;
use super::buffer::ScrollbackBuffer;
use super::features::{FilterState, SearchState};
use super::separator::SeparatorRegistry;
use crate::chrome::{symbols::Symbols, theme::Theme};

/// Truncates raw byte content (potentially containing ANSI escape sequences)
/// to fit within `max_display_cols` terminal columns.
///
/// Returns the byte index to safely slice `content[..index]` such that:
/// - The visible (non-ANSI) display width does not exceed `max_display_cols`
/// - No multi-byte UTF-8 character is split
/// - ANSI escape sequences are not counted toward display width
fn ansi_aware_truncate(content: &[u8], max_display_cols: usize) -> usize {
    use super::ansi::{skip_csi, skip_osc, utf8_sequence_len};
    use unicode_width::UnicodeWidthChar;

    let mut width: usize = 0;
    let mut i = 0;

    while i < content.len() {
        let b = content[i];

        // ESC sequence start - skip entirely (zero display width)
        if b == 0x1b && i + 1 < content.len() {
            let next = content[i + 1];
            if next == b'[' {
                i = skip_csi(content, i + 2);
                continue;
            } else if next == b']' {
                i = skip_osc(content, i + 2);
                continue;
            } else {
                i += 2;
                continue;
            }
        }

        // Tab - assume 8-space tabs
        if b == 0x09 {
            let tab_width = 8 - (width % 8);
            if width + tab_width > max_display_cols {
                return i;
            }
            width += tab_width;
            i += 1;
            continue;
        }

        // Control characters (no width)
        if b < 0x20 {
            i += 1;
            continue;
        }

        // Decode UTF-8 character and measure display width
        let expected_len = utf8_sequence_len(b);
        if expected_len == 1 {
            if (b & 0b1100_0000) == 0b1000_0000 {
                if width + 1 > max_display_cols {
                    return i;
                }
                width += 1;
                i += 1;
                continue;
            }

            let c = b as char;
            let ch_w = UnicodeWidthChar::width(c).unwrap_or(0);
            if width + ch_w > max_display_cols {
                return i;
            }
            width += ch_w;
            i += 1;
        } else {
            let end = (i + expected_len).min(content.len());
            if let Ok(s) = std::str::from_utf8(&content[i..end]) {
                if let Some(c) = s.chars().next() {
                    let ch_w = UnicodeWidthChar::width(c).unwrap_or(0);
                    if width + ch_w > max_display_cols {
                        return i;
                    }
                    width += ch_w;
                    i += c.len_utf8();
                } else {
                    i += 1;
                }
            } else {
                // Invalid UTF-8 - treat as 1-width replacement glyph
                if width + 1 > max_display_cols {
                    return i;
                }
                width += 1;
                i += 1;
            }
        }
    }

    content.len()
}

/// Writes line content truncated to visible display width.
///
/// Appends a full style reset when truncation occurs so partially emitted ANSI
/// styles do not bleed into the rest of the render output.
fn write_truncated_content<W: Write>(
    out: &mut W,
    content: &[u8],
    max_display_cols: usize,
) -> io::Result<()> {
    let sanitized = super::ansi::sanitize_for_display(content);
    let safe_len = ansi_aware_truncate(&sanitized, max_display_cols);
    out.write_all(&sanitized[..safe_len])?;

    if safe_len < sanitized.len() {
        out.write_all(b"\x1b[0m")?;
    }

    Ok(())
}

/// Rendering configuration for the unified scrollback render pipeline.
///
/// Replaces the old `RenderOptions` and the per-method parameter explosion.
/// All optional features (search, filter, boundary markers) are configured here.
pub struct RenderConfig<'a> {
    /// Starting terminal row (1-indexed). Set to 2 when chrome reserves row 1.
    pub start_row: u16,
    /// Show line numbers in left gutter.
    pub show_line_numbers: bool,
    /// Show relative timestamps in left gutter.
    pub show_timestamps: bool,
    /// Show BEGIN/END boundary markers at scroll extremes.
    pub boundary_markers: bool,
    /// Sorted prompt line indices for command separator rendering.
    pub boundary_lines: &'a [usize],
    /// Rich command metadata keyed by prompt boundaries.
    pub records: &'a [CommandRecord],
    /// Search state for match highlighting. `None` = no highlighting.
    pub search: Option<&'a SearchState>,
    /// Filter state for showing only matching lines. `None` = show all.
    pub filter: Option<&'a FilterState>,
    /// Scroll offset within filtered view (only used when `filter` is `Some`).
    pub filter_offset: usize,
    /// Separator segment registry for rich command metadata display.
    pub separator_registry: Option<&'a SeparatorRegistry>,
    /// UI symbols for status/failure indicators in separator segments.
    pub symbols: Option<&'a Symbols>,
    /// Optional external collapsed command set (reserved for future use).
    pub collapsed_commands: Option<&'a HashSet<usize>>,
    /// Whether sticky command headers are enabled in normal mode.
    pub sticky_header: bool,
    /// Theme for styled rendering. `None` = dim ANSI fallback.
    pub theme: Option<&'a Theme>,
}

impl Default for RenderConfig<'_> {
    fn default() -> Self {
        Self {
            start_row: 1,
            show_line_numbers: false,
            show_timestamps: false,
            boundary_markers: false,
            boundary_lines: &[],
            records: &[],
            search: None,
            filter: None,
            filter_offset: 0,
            separator_registry: None,
            symbols: None,
            collapsed_commands: None,
            sticky_header: false,
            theme: None,
        }
    }
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
/// Provides a single unified rendering function for scrollback content.
/// All state (scroll offset, buffer) is owned externally.
pub struct ScrollViewer;

impl ScrollViewer {
    /// Renders scrollback content to the terminal.
    ///
    /// This is the single entry point for all scrollback rendering: normal view,
    /// search highlighting, filter mode, boundary markers, and command separators
    /// are all controlled via `RenderConfig` fields.
    ///
    /// # Arguments
    ///
    /// * `out` - Writer to render to (usually stdout)
    /// * `buffer` - The scrollback buffer to read from
    /// * `offset` - Lines from bottom (0 = most recent visible at bottom).
    ///   Ignored when `config.filter` is `Some` (uses `config.filter_offset`).
    /// * `cols` - Terminal width in columns
    /// * `rows` - Number of rows available for content (viewport height)
    /// * `config` - Rendering configuration
    pub fn render<W: Write>(
        out: &mut W,
        buffer: &ScrollbackBuffer,
        offset: usize,
        cols: u16,
        rows: u16,
        config: &RenderConfig,
    ) -> io::Result<RenderStats> {
        let total = buffer.len();
        let start_row = config.start_row.max(1) as usize;
        let content_rows = rows as usize;
        let is_filter_mode = config.filter.is_some();

        // Calculate gutter widths (once)
        let (line_num_width, _timestamp_width, gutter_width) =
            Self::gutter_widths(config.show_line_numbers, config.show_timestamps, total);
        let content_cols = (cols as usize).saturating_sub(gutter_width);

        let now = std::time::Instant::now();

        // Hide cursor during render
        write!(out, "\x1b[?25l")?;

        let mut current_row = start_row;
        let max_row = start_row + content_rows;
        let mut rendered = 0;

        if is_filter_mode {
            // ─── Filter mode: show only matching lines ───────────────
            let filter = config.filter.unwrap();
            let lines = filter.get_filtered_range(buffer, config.filter_offset, content_rows);
            let first_visible_line = lines.first().map(|(idx, _)| *idx + 1).unwrap_or(1);

            // Get search state for optional highlighting within filter
            let current_match = config.search.and_then(|s| s.current().copied());

            for (original_idx, line) in &lines {
                let line_number = *original_idx + 1;

                write!(out, "\x1b[{};1H\x1b[2K", current_row)?;

                Self::render_gutter(
                    out,
                    line_number,
                    line.timestamp(),
                    now,
                    config.show_timestamps,
                    config.show_line_numbers,
                    line_num_width,
                )?;

                // Content: with search highlights if available, otherwise plain
                if let Some(search) = config.search {
                    if let Some(theme) = config.theme {
                        let line_matches: Vec<_> =
                            search.matches_on_line(*original_idx).collect();
                        if !line_matches.is_empty() {
                            Self::render_line_with_highlights(
                                out,
                                line.content(),
                                content_cols,
                                *original_idx,
                                &line_matches,
                                current_match,
                                theme,
                            )?;
                        } else {
                            write_truncated_content(out, line.content(), content_cols)?;
                        }
                    } else {
                        write_truncated_content(out, line.content(), content_cols)?;
                    }
                } else {
                    write_truncated_content(out, line.content(), content_cols)?;
                }

                if line.is_truncated() {
                    write!(out, "\x1b[7m>\x1b[27m")?;
                }

                current_row += 1;
                rendered += 1;
            }

            // Clear remaining rows
            while current_row < max_row {
                write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
                current_row += 1;
            }

            write!(out, "\x1b[?25h")?;
            out.flush()?;

            Ok(RenderStats {
                lines_rendered: rendered,
                first_visible_line,
            })
        } else {
            // ─── Normal mode: full buffer with markers and separators ─
            let visible_line_indices = Self::collect_visible_line_indices(
                total,
                config.records,
                config.collapsed_commands,
            );
            let visible_total = visible_line_indices.len();
            let max_offset = Self::max_offset(visible_total, content_rows);
            let is_at_bottom = offset == 0;
            let is_at_top = offset >= max_offset && visible_total > 0;

            let show_end = config.boundary_markers && is_at_bottom && visible_total > 0;
            let show_begin = config.boundary_markers && is_at_top && visible_total > 0;

            let mut available_rows = content_rows
                .saturating_sub(show_begin as usize)
                .saturating_sub(show_end as usize);

            let mut line_positions = Self::visible_line_positions(
                &visible_line_indices,
                offset,
                available_rows,
                show_begin,
                config.boundary_lines,
            );
            let mut first_visible_idx = line_positions
                .first()
                .and_then(|pos| visible_line_indices.get(*pos))
                .copied()
                .unwrap_or(0);
            let mut sticky_record = None;

            if config.sticky_header && available_rows > 0 {
                if let Some(record) = Self::record_for_line(config.records, first_visible_idx) {
                    let separator_visible = line_positions.iter().any(|pos| {
                        visible_line_indices.get(*pos).copied() == Some(record.output_start)
                    });
                    if first_visible_idx > record.output_start && !separator_visible {
                        sticky_record = Some(record);
                    }
                }
            }

            if sticky_record.is_some() && available_rows > 0 {
                available_rows = available_rows.saturating_sub(1);
                line_positions = Self::visible_line_positions(
                    &visible_line_indices,
                    offset,
                    available_rows,
                    show_begin,
                    config.boundary_lines,
                );
                first_visible_idx = line_positions
                    .first()
                    .and_then(|pos| visible_line_indices.get(*pos))
                    .copied()
                    .unwrap_or(0);

                if let Some(record) = sticky_record {
                    let separator_visible = line_positions.iter().any(|pos| {
                        visible_line_indices.get(*pos).copied() == Some(record.output_start)
                    });
                    if separator_visible || first_visible_idx <= record.output_start {
                        sticky_record = None;
                    }
                }
            }

            let first_visible_line = if visible_total == 0 {
                1
            } else {
                first_visible_idx.saturating_add(1)
            };

            // Get search state for optional highlighting
            let current_match = config.search.and_then(|s| s.current().copied());

            // BEGIN marker
            if show_begin {
                write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
                Self::render_boundary_marker_styled(
                    out,
                    cols as usize,
                    gutter_width,
                    "BEGIN",
                    config.theme,
                )?;
                current_row += 1;
            }

            if let Some(record) = sticky_record {
                if current_row < max_row {
                    write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
                    Self::render_command_separator(
                        out,
                        cols as usize,
                        gutter_width,
                        config.theme,
                        Some(record),
                        config.separator_registry,
                        config.symbols,
                    )?;
                    current_row += 1;
                }
            }

            for line_pos in line_positions {
                let line_index = visible_line_indices[line_pos];
                let line_number = line_index + 1;

                if config.boundary_lines.binary_search(&line_index).is_ok() {
                    if current_row >= max_row {
                        break;
                    }
                    write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
                    Self::render_command_separator(
                        out,
                        cols as usize,
                        gutter_width,
                        config.theme,
                        Self::record_for_line(config.records, line_index),
                        config.separator_registry,
                        config.symbols,
                    )?;
                    current_row += 1;
                }

                if current_row >= max_row {
                    break;
                }

                let Some(line) = buffer.get(line_index) else {
                    continue;
                };

                write!(out, "\x1b[{};1H\x1b[2K", current_row)?;

                Self::render_gutter(
                    out,
                    line_number,
                    line.timestamp(),
                    now,
                    config.show_timestamps,
                    config.show_line_numbers,
                    line_num_width,
                )?;

                // Content: with search highlights if available
                if let Some(search) = config.search {
                    if let Some(theme) = config.theme {
                        let line_matches: Vec<_> =
                            search.matches_on_line(line_index).collect();
                        if !line_matches.is_empty() {
                            Self::render_line_with_highlights(
                                out,
                                line.content(),
                                content_cols,
                                line_index,
                                &line_matches,
                                current_match,
                                theme,
                            )?;
                        } else {
                            write_truncated_content(out, line.content(), content_cols)?;
                        }
                    } else {
                        write_truncated_content(out, line.content(), content_cols)?;
                    }
                } else {
                    write_truncated_content(out, line.content(), content_cols)?;
                }

                if line.is_truncated() {
                    write!(out, "\x1b[7m>\x1b[27m")?;
                }

                current_row += 1;
                rendered += 1;
            }

            // END marker
            if show_end && current_row < max_row {
                write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
                Self::render_boundary_marker_styled(
                    out,
                    cols as usize,
                    gutter_width,
                    "END",
                    config.theme,
                )?;
                current_row += 1;
            }

            // Clear remaining rows
            while current_row < max_row {
                write!(out, "\x1b[{};1H\x1b[2K", current_row)?;
                current_row += 1;
            }

            write!(out, "\x1b[?25h")?;
            out.flush()?;

            Ok(RenderStats {
                lines_rendered: rendered,
                first_visible_line,
            })
        }
    }

    // ─── Private helpers ─────────────────────────────────────────────────

    /// Calculates gutter widths for line numbers and timestamps.
    ///
    /// Returns `(line_num_width, timestamp_width, total_gutter_width)`.
    fn gutter_widths(
        show_line_numbers: bool,
        show_timestamps: bool,
        total_lines: usize,
    ) -> (usize, usize, usize) {
        let line_num_width = if show_line_numbers {
            let num_width = if total_lines == 0 {
                1
            } else {
                (total_lines as f64).log10().floor() as usize + 1
            };
            num_width.max(4) + 3 // +3 for " │ "
        } else {
            0
        };
        let timestamp_width = if show_timestamps { 7 } else { 0 };
        (
            line_num_width,
            timestamp_width,
            line_num_width + timestamp_width,
        )
    }

    /// Renders the gutter (timestamp + line number) for a single row.
    fn render_gutter<W: Write>(
        out: &mut W,
        line_number: usize,
        timestamp: std::time::Instant,
        now: std::time::Instant,
        show_timestamps: bool,
        show_line_numbers: bool,
        line_num_width: usize,
    ) -> io::Result<()> {
        if show_timestamps {
            let elapsed = now.duration_since(timestamp);
            let time_str = Self::format_relative_time(elapsed);
            write!(out, "\x1b[2m{:>4} │ \x1b[22m", time_str)?;
        }
        if show_line_numbers {
            write!(
                out,
                "\x1b[2m{:>width$} │ \x1b[22m",
                line_number,
                width = line_num_width - 3
            )?;
        }
        Ok(())
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

        let label_with_spaces = format!(" {} ", label);
        let label_width = crate::ui::text_width::display_width(&label_with_spaces);
        let dashes_total = content_cols.saturating_sub(label_width);
        let dashes_left = dashes_total / 2;
        let dashes_right = dashes_total - dashes_left;

        if gutter_width > 0 {
            write!(out, "{:width$}", "", width = gutter_width)?;
        }

        if let Some(theme) = theme {
            let fg = color_to_fg_ansi(theme.marker_fg);
            write!(out, "{}", fg)?;
        } else {
            write!(out, "\x1b[2m")?;
        }

        for _ in 0..dashes_left {
            write!(out, "─")?;
        }
        write!(out, "{}", label_with_spaces)?;
        for _ in 0..dashes_right {
            write!(out, "─")?;
        }

        if theme.is_some() {
            write!(out, "\x1b[39m")?;
        } else {
            write!(out, "\x1b[22m")?;
        }

        Ok(())
    }

    /// Renders a command boundary separator line.
    fn render_command_separator<W: Write>(
        out: &mut W,
        cols: usize,
        gutter_width: usize,
        theme: Option<&Theme>,
        record: Option<&CommandRecord>,
        separator_registry: Option<&SeparatorRegistry>,
        symbols: Option<&Symbols>,
    ) -> io::Result<()> {
        if let (Some(theme), Some(registry), Some(symbols)) = (theme, separator_registry, symbols) {
            let separator = registry.render(record, cols, gutter_width, theme, symbols);
            write!(out, "{}", separator)?;
            return Ok(());
        }

        use crate::chrome::segments::color_to_fg_ansi;

        let content_cols = cols.saturating_sub(gutter_width);

        if gutter_width > 0 {
            write!(out, "{:width$}", "", width = gutter_width)?;
        }

        if let Some(theme) = theme {
            let fg = color_to_fg_ansi(theme.marker_fg);
            write!(out, "{}", fg)?;
        } else {
            write!(out, "\x1b[2m")?;
        }

        for _ in 0..content_cols {
            write!(out, "╌")?;
        }

        if theme.is_some() {
            write!(out, "\x1b[39m")?;
        } else {
            write!(out, "\x1b[22m")?;
        }

        Ok(())
    }

    fn record_for_line(records: &[CommandRecord], line_idx: usize) -> Option<&CommandRecord> {
        Self::record_for_line_with_index(records, line_idx).map(|(_, record)| record)
    }

    fn record_for_line_with_index(
        records: &[CommandRecord],
        line_idx: usize,
    ) -> Option<(usize, &CommandRecord)> {
        let pos = records.partition_point(|record| record.output_start <= line_idx);
        if pos == 0 {
            return None;
        }
        let idx = pos - 1;
        let record = records.get(idx)?;
        let end = record.prompt_line.unwrap_or(usize::MAX);
        if line_idx < end {
            Some((idx, record))
        } else {
            None
        }
    }

    fn is_line_folded(
        line_idx: usize,
        records: &[CommandRecord],
        collapsed_commands: Option<&HashSet<usize>>,
    ) -> bool {
        let Some((record_idx, record)) = Self::record_for_line_with_index(records, line_idx) else {
            return false;
        };
        let externally_folded = collapsed_commands
            .map(|set| set.contains(&record_idx))
            .unwrap_or(false);
        if !(record.folded || externally_folded) {
            return false;
        }

        let prompt_line = record.prompt_line.unwrap_or(usize::MAX);
        line_idx > record.output_start && line_idx < prompt_line
    }

    fn collect_visible_line_indices(
        total: usize,
        records: &[CommandRecord],
        collapsed_commands: Option<&HashSet<usize>>,
    ) -> Vec<usize> {
        let mut indices = Vec::with_capacity(total);
        for line_idx in 0..total {
            if !Self::is_line_folded(line_idx, records, collapsed_commands) {
                indices.push(line_idx);
            }
        }
        indices
    }

    fn visible_line_positions(
        visible_indices: &[usize],
        offset: usize,
        requested_rows: usize,
        show_begin: bool,
        boundary_lines: &[usize],
    ) -> Vec<usize> {
        if visible_indices.is_empty() || requested_rows == 0 {
            return Vec::new();
        }

        if show_begin {
            let mut rows = 0usize;
            let mut positions = Vec::new();
            for (pos, line_idx) in visible_indices.iter().enumerate() {
                let separator_rows =
                    usize::from(*line_idx > 0 && boundary_lines.binary_search(line_idx).is_ok());
                let needed = 1 + separator_rows;
                if rows + needed > requested_rows {
                    break;
                }
                rows += needed;
                positions.push(pos);
            }
            return positions;
        }

        let end_pos = visible_indices
            .len()
            .saturating_sub(offset.min(visible_indices.len()));
        if end_pos == 0 {
            return Vec::new();
        }

        let mut start_pos = end_pos;
        let mut rows = 0usize;
        while start_pos > 0 {
            let line_idx = visible_indices[start_pos - 1];
            let separator_rows =
                usize::from(line_idx > 0 && boundary_lines.binary_search(&line_idx).is_ok());
            let needed = 1 + separator_rows;
            if rows + needed > requested_rows {
                break;
            }
            rows += needed;
            start_pos -= 1;
        }

        (start_pos..end_pos).collect()
    }

    /// Renders a line with search match highlighting.
    ///
    /// Uses safe string slicing via `.get()` to handle potential index mismatches.
    fn render_line_with_highlights<W: Write>(
        out: &mut W,
        content: &[u8],
        max_cols: usize,
        line_index: usize,
        matches: &[&super::features::SearchMatch],
        current_match: Option<super::features::SearchMatch>,
        theme: &Theme,
    ) -> io::Result<()> {
        use crate::chrome::segments::color_to_bg_ansi;

        let sanitized = super::ansi::sanitize_for_display(content);
        let truncate_len = ansi_aware_truncate(&sanitized, max_cols);
        let was_truncated = truncate_len < sanitized.len();
        let truncated = &sanitized[..truncate_len];
        let content_str = String::from_utf8_lossy(truncated);
        let mut pos = 0;

        let mut sorted_matches = matches.to_vec();
        sorted_matches.sort_by_key(|m| m.start);

        for search_match in sorted_matches {
            let match_start = search_match.start.min(truncated.len());
            let match_end = search_match.end.min(truncated.len());

            if match_start > pos {
                if let Some(before) = content_str.get(pos..match_start) {
                    write!(out, "{}", before)?;
                }
            }

            let is_current_match = current_match
                .map(|m| {
                    m.line == line_index
                        && m.start == search_match.start
                        && m.end == search_match.end
                })
                .unwrap_or(false);
            let bg_color = if is_current_match {
                theme.search_current_bg
            } else {
                theme.search_other_bg
            };
            let bg_ansi = color_to_bg_ansi(bg_color);

            if let Some(match_text) = content_str.get(match_start..match_end) {
                write!(out, "{}{}\x1b[49m", bg_ansi, match_text)?;
            }

            pos = match_end;
        }

        if let Some(remaining) = content_str.get(pos..) {
            write!(out, "{}", remaining)?;
        }

        if was_truncated {
            write!(out, "\x1b[0m")?;
        }

        Ok(())
    }

    // ─── Public utilities ────────────────────────────────────────────────

    /// Calculates the maximum valid scroll offset.
    #[inline]
    pub fn max_offset(total_lines: usize, viewport_rows: usize) -> usize {
        total_lines.saturating_sub(viewport_rows)
    }

    /// Clamps a scroll offset to valid range.
    #[inline]
    pub fn clamp_offset(offset: usize, total_lines: usize, viewport_rows: usize) -> usize {
        offset.min(Self::max_offset(total_lines, viewport_rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SymbolSet, ThemePreset};
    use crate::scrollback::{CommandRecord, SeparatorRegistry};
    use std::path::PathBuf;
    use std::time::Duration;

    fn create_test_buffer(lines: &[&str]) -> ScrollbackBuffer {
        let mut buffer = ScrollbackBuffer::with_capacity(100, 1000);
        for line in lines {
            buffer.push_line(line.as_bytes().to_vec());
        }
        buffer
    }

    #[test]
    fn test_max_offset_empty_buffer_returns_zero() {
        assert_eq!(ScrollViewer::max_offset(0, 10), 0);
    }

    #[test]
    fn test_max_offset_buffer_smaller_than_viewport_returns_zero() {
        assert_eq!(ScrollViewer::max_offset(5, 10), 0);
    }

    #[test]
    fn test_max_offset_buffer_larger_than_viewport_returns_difference() {
        assert_eq!(ScrollViewer::max_offset(100, 10), 90);
    }

    #[test]
    fn test_clamp_offset_various_behaviors_returns_expected() {
        assert_eq!(ScrollViewer::clamp_offset(50, 100, 10), 50);
        assert_eq!(ScrollViewer::clamp_offset(95, 100, 10), 90);
        assert_eq!(ScrollViewer::clamp_offset(90, 100, 10), 90);
    }

    #[test]
    fn test_render_to_buffer_with_three_lines_renders_three() {
        let buffer = create_test_buffer(&["line1", "line2", "line3"]);
        let mut output = Vec::new();

        let stats =
            ScrollViewer::render(&mut output, &buffer, 0, 80, 3, &RenderConfig::default()).unwrap();
        assert_eq!(stats.lines_rendered, 3);

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("line1"));
        assert!(output_str.contains("line2"));
        assert!(output_str.contains("line3"));
    }

    #[test]
    fn test_render_with_offset_offset2_shows_expected_lines() {
        let buffer = create_test_buffer(&["line1", "line2", "line3", "line4", "line5"]);
        let mut output = Vec::new();

        let stats =
            ScrollViewer::render(&mut output, &buffer, 2, 80, 2, &RenderConfig::default()).unwrap();
        assert_eq!(stats.lines_rendered, 2);

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("line2"));
        assert!(output_str.contains("line3"));
    }

    #[test]
    fn test_render_with_chrome_topbar_reserved_rows_renders_content() {
        let buffer = create_test_buffer(&["line1", "line2", "line3", "line4", "line5"]);
        let mut output = Vec::new();

        // 5 total rows, row 1 reserved for topbar, so 4 content rows
        let stats = ScrollViewer::render(
            &mut output,
            &buffer,
            0,
            80,
            4, // content rows (rows - 1 for topbar)
            &RenderConfig {
                start_row: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(stats.lines_rendered, 4);
        assert_eq!(stats.first_visible_line, 2);

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("\x1b[2;1H"));
    }

    #[test]
    fn test_render_with_line_numbers_shows_gutter() {
        let buffer = create_test_buffer(&["hello", "world"]);
        let mut output = Vec::new();

        let stats = ScrollViewer::render(
            &mut output,
            &buffer,
            0,
            80,
            4,
            &RenderConfig {
                start_row: 2,
                show_line_numbers: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(stats.lines_rendered, 2);

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("│"));
    }

    #[test]
    fn test_first_visible_line_calculation_bottom_and_scrolled_expected_values() {
        let buffer = create_test_buffer(&["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]);
        let mut output = Vec::new();

        // At bottom (offset 0), viewing 3 lines: should show lines 8,9,10
        let stats = ScrollViewer::render(
            &mut output,
            &buffer,
            0,
            80,
            3,
            &RenderConfig {
                start_row: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(stats.first_visible_line, 8);

        // Scrolled up 3 lines: should show lines 5,6,7
        output.clear();
        let stats = ScrollViewer::render(
            &mut output,
            &buffer,
            3,
            80,
            3,
            &RenderConfig {
                start_row: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(stats.first_visible_line, 5);
    }

    #[test]
    fn test_boundary_markers_begin_and_end_markers_present() {
        let buffer = create_test_buffer(&["line1", "line2"]);
        let mut output = Vec::new();

        // Buffer smaller than viewport (2 lines in 4 rows), at bottom - should show END
        let stats = ScrollViewer::render(
            &mut output,
            &buffer,
            0,
            80,
            4,
            &RenderConfig {
                start_row: 2,
                boundary_markers: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(stats.lines_rendered, 2);

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("END"), "Should contain END marker");

        // At top (max offset), should show BEGIN
        output.clear();
        let max_off = ScrollViewer::max_offset(2, 4);
        let _stats = ScrollViewer::render(
            &mut output,
            &buffer,
            max_off,
            80,
            4,
            &RenderConfig {
                start_row: 2,
                boundary_markers: true,
                ..Default::default()
            },
        )
        .unwrap();

        let output_str = String::from_utf8_lossy(&output);
        assert!(output_str.contains("BEGIN"), "Should contain BEGIN marker");
    }

    // ─── Unicode truncation regression tests ────────────────────────

    #[test]
    fn test_ansi_aware_truncate_ascii() {
        let content = b"hello world";
        assert_eq!(ansi_aware_truncate(content, 5), 5);
        assert_eq!(ansi_aware_truncate(content, 100), 11);
    }

    #[test]
    fn test_ansi_aware_truncate_skips_ansi_sequences() {
        let content = b"\x1b[31mhi\x1b[0m";
        assert_eq!(ansi_aware_truncate(content, 2), content.len());
        let len = ansi_aware_truncate(content, 1);
        assert_eq!(&content[..len], b"\x1b[31mh");
    }

    #[test]
    fn test_ansi_aware_truncate_wide_chars() {
        let content = "你好".as_bytes();
        assert_eq!(ansi_aware_truncate(content, 4), 6);
        assert_eq!(ansi_aware_truncate(content, 3), 3);
        assert_eq!(ansi_aware_truncate(content, 2), 3);
        assert_eq!(ansi_aware_truncate(content, 1), 0);
    }

    #[test]
    fn test_ansi_aware_truncate_mixed_ansi_and_wide() {
        let content = "\x1b[31m你\x1b[0ma".as_bytes();
        assert_eq!(ansi_aware_truncate(content, 3), content.len());
        assert_eq!(ansi_aware_truncate(content, 2), "\x1b[31m你\x1b[0m".len());
    }

    #[test]
    fn test_write_truncated_content_appends_reset_when_cut() {
        let mut out = Vec::new();
        write_truncated_content(&mut out, b"\x1b[31mhello\x1b[0m", 1).unwrap();
        assert_eq!(out, b"\x1b[31mh\x1b[0m");
    }

    #[test]
    fn test_render_line_with_highlights_resets_when_truncated() {
        let mut out = Vec::new();
        ScrollViewer::render_line_with_highlights(
            &mut out,
            b"\x1b[31mhello\x1b[0m",
            1,
            0,
            &[],
            None,
            &crate::chrome::theme::AMBER_THEME,
        )
        .unwrap();

        let rendered = String::from_utf8_lossy(&out);
        assert!(
            rendered.ends_with("\x1b[0m"),
            "truncated highlighted line should end with reset: {rendered:?}"
        );
    }

    #[test]
    fn test_render_line_with_highlights_marks_only_current_match() {
        use crate::chrome::segments::color_to_bg_ansi;
        use crate::scrollback::features::SearchMatch;

        let mut out = Vec::new();
        let first = SearchMatch {
            line: 42,
            start: 0,
            end: 3,
        };
        let second = SearchMatch {
            line: 42,
            start: 4,
            end: 7,
        };
        let matches = vec![&first, &second];

        ScrollViewer::render_line_with_highlights(
            &mut out,
            b"abc def",
            20,
            42,
            &matches,
            Some(second),
            &crate::chrome::theme::AMBER_THEME,
        )
        .unwrap();

        let rendered = String::from_utf8_lossy(&out);
        let current_bg = color_to_bg_ansi(crate::chrome::theme::AMBER_THEME.search_current_bg);
        let other_bg = color_to_bg_ansi(crate::chrome::theme::AMBER_THEME.search_other_bg);

        assert_ne!(current_bg, other_bg);
        assert_eq!(rendered.matches(&current_bg).count(), 1);
        assert_eq!(rendered.matches(&other_bg).count(), 1);
    }

    #[test]
    fn test_render_with_cjk_content_no_panic() {
        let buffer = create_test_buffer(&["hello", "你好世界", "mixed混合content"]);
        let mut output = Vec::new();

        let stats =
            ScrollViewer::render(&mut output, &buffer, 0, 10, 3, &RenderConfig::default()).unwrap();
        assert_eq!(stats.lines_rendered, 3);
    }

    #[test]
    fn test_gutter_widths_no_gutters() {
        let (ln, ts, total) = ScrollViewer::gutter_widths(false, false, 100);
        assert_eq!(ln, 0);
        assert_eq!(ts, 0);
        assert_eq!(total, 0);
    }

    #[test]
    fn test_gutter_widths_line_numbers_only() {
        let (ln, ts, total) = ScrollViewer::gutter_widths(true, false, 100);
        assert!(ln > 0);
        assert_eq!(ts, 0);
        assert_eq!(total, ln);
    }

    #[test]
    fn test_gutter_widths_both() {
        let (ln, ts, total) = ScrollViewer::gutter_widths(true, true, 100);
        assert!(ln > 0);
        assert_eq!(ts, 7);
        assert_eq!(total, ln + ts);
    }

    #[test]
    fn test_render_sticky_header_when_separator_offscreen_renders_separator_once() {
        let buffer = create_test_buffer(&[
            "l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "l9", "l10", "l11", "l12",
        ]);
        let mut output = Vec::new();
        let theme = crate::chrome::theme::Theme::for_preset(ThemePreset::Amber);
        let symbols = crate::chrome::symbols::Symbols::for_set(SymbolSet::Fallback);
        let registry = SeparatorRegistry::with_defaults();
        let records = vec![CommandRecord {
            output_start: 2,
            prompt_line: Some(10),
            command_text: Some("build".to_string()),
            exit_code: Some(0),
            duration: Some(Duration::from_secs(2)),
            cwd: Some(PathBuf::from("/tmp")),
            ..Default::default()
        }];

        let stats = ScrollViewer::render(
            &mut output,
            &buffer,
            2,
            80,
            4,
            &RenderConfig {
                boundary_lines: &[2],
                records: &records,
                sticky_header: true,
                separator_registry: Some(&registry),
                symbols: Some(symbols),
                theme: Some(theme),
                ..Default::default()
            },
        )
        .unwrap();

        let rendered = String::from_utf8_lossy(&output);
        assert_eq!(rendered.matches("build").count(), 1);
        assert!(rendered.contains('╌'));
        assert!(stats.first_visible_line > 2 && stats.first_visible_line < 10);
    }

    #[test]
    fn test_render_sticky_header_when_separator_visible_avoids_duplication() {
        let buffer = create_test_buffer(&[
            "l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "l9", "l10", "l11", "l12",
        ]);
        let mut output = Vec::new();
        let theme = crate::chrome::theme::Theme::for_preset(ThemePreset::Amber);
        let symbols = crate::chrome::symbols::Symbols::for_set(SymbolSet::Fallback);
        let registry = SeparatorRegistry::with_defaults();
        let records = vec![CommandRecord {
            output_start: 2,
            prompt_line: Some(10),
            command_text: Some("build".to_string()),
            exit_code: Some(0),
            ..Default::default()
        }];

        ScrollViewer::render(
            &mut output,
            &buffer,
            1,
            80,
            4,
            &RenderConfig {
                boundary_lines: &[2],
                records: &records,
                sticky_header: true,
                separator_registry: Some(&registry),
                symbols: Some(symbols),
                theme: Some(theme),
                ..Default::default()
            },
        )
        .unwrap();

        let rendered = String::from_utf8_lossy(&output);
        assert_eq!(rendered.matches("build").count(), 1);
    }

    #[test]
    fn test_render_folded_viewport_when_offset_clamped_keeps_lines_visible() {
        let buffer = create_test_buffer(&[
            "l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "l9", "l10", "l11", "l12",
        ]);
        let mut output = Vec::new();
        let records = vec![CommandRecord {
            output_start: 2,
            prompt_line: Some(10),
            folded: true,
            ..Default::default()
        }];

        let total = buffer.len();
        let folded_count = records[0]
            .prompt_line
            .unwrap_or(0)
            .saturating_sub(records[0].output_start.saturating_add(1));
        let visible_total = total.saturating_sub(folded_count);
        let clamped_offset = ScrollViewer::clamp_offset(8, visible_total, 4);

        let stats = ScrollViewer::render(
            &mut output,
            &buffer,
            clamped_offset,
            80,
            4,
            &RenderConfig {
                records: &records,
                ..Default::default()
            },
        )
        .unwrap();

        let rendered = String::from_utf8_lossy(&output);
        assert!(rendered.contains("l1"));
        assert!(rendered.contains("l2"));
        assert!(!rendered.contains("l4"));
        assert!(stats.lines_rendered > 0);
        assert_eq!(stats.first_visible_line, 1);
    }

    #[test]
    fn test_render_separator_when_duplicate_output_start_prefers_active_record_for_line() {
        let buffer = create_test_buffer(&["l1", "l2", "l3", "l4", "l5", "l6"]);
        let mut output = Vec::new();
        let theme = crate::chrome::theme::Theme::for_preset(ThemePreset::Amber);
        let symbols = crate::chrome::symbols::Symbols::for_set(SymbolSet::Fallback);
        let registry = SeparatorRegistry::with_defaults();
        let records = vec![
            CommandRecord {
                output_start: 2,
                prompt_line: Some(4),
                command_text: Some("older".to_string()),
                exit_code: Some(0),
                ..Default::default()
            },
            CommandRecord {
                output_start: 2,
                prompt_line: Some(6),
                command_text: Some("newer".to_string()),
                exit_code: Some(0),
                ..Default::default()
            },
        ];

        ScrollViewer::render(
            &mut output,
            &buffer,
            0,
            120,
            6,
            &RenderConfig {
                boundary_lines: &[2],
                records: &records,
                separator_registry: Some(&registry),
                symbols: Some(symbols),
                theme: Some(theme),
                ..Default::default()
            },
        )
        .unwrap();

        let rendered = String::from_utf8_lossy(&output);
        assert!(rendered.contains("newer"));
        assert!(!rendered.contains("older"));
    }
}
