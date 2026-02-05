//! Modular topbar segment system.
//!
//! This module provides a trait-based architecture for topbar segments,
//! allowing segments to be independently developed, tested, and configured.
//!
//! # Architecture
//!
//! - `TopbarSegment` trait: Interface for all segment types
//! - `TopbarState`: Unified state that segments react to
//! - `TopbarRegistry`: Holds enabled segments and orchestrates rendering
//! - Individual segment modules: status, duration, cwd, git, clock, scroll

mod clock;
mod cwd;
mod duration;
mod git;
mod scroll;
mod status;

pub use clock::ClockSegment;
pub use cwd::CwdSegment;
pub use duration::DurationSegment;
pub use git::GitSegment;
pub use scroll::ScrollSegment;
pub use status::StatusSegment;

use std::path::PathBuf;
use std::time::Duration;

use ratatui_core::style::Color;

use super::symbols::Symbols;
use super::theme::Theme;

/// Alignment for topbar segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentAlign {
    /// Left-aligned segment (default).
    #[default]
    Left,
    /// Right-aligned segment (e.g., clock).
    Right,
}

/// A rendered segment ready for display.
#[derive(Debug, Clone)]
pub struct RenderedSegment {
    /// Formatted content with ANSI escape codes.
    pub content: String,
    /// Display width excluding ANSI codes.
    pub display_width: usize,
    /// Priority for truncation (0 = critical, higher = drop first).
    pub priority: u8,
    /// Alignment within the bar.
    pub align: SegmentAlign,
}

impl RenderedSegment {
    /// Creates a new rendered segment.
    pub fn new(content: String, priority: u8, align: SegmentAlign) -> Self {
        // Calculate display width by stripping ANSI codes
        let display_width = strip_ansi_width(&content);
        Self {
            content,
            display_width,
            priority,
            align,
        }
    }

    /// Creates a left-aligned segment with the given priority.
    pub fn left(content: String, priority: u8) -> Self {
        Self::new(content, priority, SegmentAlign::Left)
    }

    /// Creates a right-aligned segment with the given priority.
    pub fn right(content: String, priority: u8) -> Self {
        Self::new(content, priority, SegmentAlign::Right)
    }
}

/// Calculates display width of a string, excluding ANSI escape codes.
fn strip_ansi_width(s: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;

    for ch in s.chars() {
        if ch == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
        } else {
            width += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }

    width
}

/// Information about scroll position for display.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollInfo {
    /// Scroll percentage (0 = at bottom/live, 100 = at top/oldest).
    pub percentage: u8,
    /// Total lines in scrollback buffer.
    pub total_lines: usize,
    /// First visible line number (1-indexed from oldest).
    pub first_visible_line: usize,
}

/// Git repository information for display.
#[derive(Debug, Clone, Default)]
pub struct GitInfo {
    /// Branch name, if in a git repository.
    pub branch: Option<String>,
    /// Whether the working directory has uncommitted changes.
    pub dirty: bool,
}

/// Unified state for all segments to react to.
///
/// Segments query this state to decide whether and how to render.
/// This separation allows state to be computed once and shared.
#[derive(Debug, Clone, Default)]
pub struct TopbarState {
    /// Current working directory.
    pub cwd: PathBuf,
    /// Git repository information.
    pub git: GitInfo,
    /// Exit code of the last command.
    pub exit_code: i32,
    /// Duration of the last command execution.
    pub last_duration: Option<Duration>,
    /// Current timestamp string (HH:MM format).
    pub timestamp: String,
    /// Scroll information (Some when in scroll mode).
    pub scroll: Option<ScrollInfo>,
}

/// Trait for self-contained topbar segments.
///
/// Each segment is responsible for:
/// - Deciding whether to render based on state
/// - Formatting its content with appropriate styling
/// - Specifying its priority and alignment
pub trait TopbarSegment: Send + Sync {
    /// Unique identifier for this segment (e.g., "status", "git", "clock").
    fn id(&self) -> &'static str;

    /// Renders the segment given current state.
    ///
    /// Returns `None` to hide the segment (e.g., git segment when not in repo).
    fn render(
        &self,
        state: &TopbarState,
        theme: &Theme,
        symbols: &Symbols,
        separator: &str,
    ) -> Option<RenderedSegment>;
}

/// Registry holding enabled segments and orchestrating rendering.
pub struct TopbarRegistry {
    segments: Vec<Box<dyn TopbarSegment>>,
}

impl Default for TopbarRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TopbarRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Creates a registry with the default segment set.
    ///
    /// Default order: [scroll?] [status] [duration?] [cwd] [git?] ... [clock]
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.add(Box::new(ScrollSegment));
        registry.add(Box::new(StatusSegment));
        registry.add(Box::new(DurationSegment));
        registry.add(Box::new(CwdSegment));
        registry.add(Box::new(GitSegment));
        registry.add(Box::new(ClockSegment));
        registry
    }

    /// Adds a segment to the registry.
    pub fn add(&mut self, segment: Box<dyn TopbarSegment>) {
        self.segments.push(segment);
    }

    /// Renders all segments into a formatted bar string.
    ///
    /// Handles priority-based truncation when content exceeds max_width.
    pub fn render(
        &self,
        state: &TopbarState,
        max_width: usize,
        theme: &Theme,
        symbols: &Symbols,
    ) -> String {
        let bar_bg = color_to_bg_ansi(theme.bar_bg);
        let separator = symbols.separator_right;

        // Render all segments
        let mut left_segments: Vec<RenderedSegment> = Vec::new();
        let mut right_segments: Vec<RenderedSegment> = Vec::new();

        for segment in &self.segments {
            if let Some(rendered) = segment.render(state, theme, symbols, separator) {
                match rendered.align {
                    SegmentAlign::Left => left_segments.push(rendered),
                    SegmentAlign::Right => right_segments.push(rendered),
                }
            }
        }

        // Calculate total width
        let left_width: usize = left_segments.iter().map(|s| s.display_width).sum();
        let right_width: usize = right_segments.iter().map(|s| s.display_width).sum();
        let mut total_width = left_width + right_width;

        // Truncate segments if needed (remove highest priority first from left)
        while total_width > max_width && left_segments.len() > 1 {
            let max_priority_idx = left_segments
                .iter()
                .enumerate()
                .max_by_key(|(_, s)| s.priority)
                .map(|(i, _)| i);

            if let Some(idx) = max_priority_idx {
                let removed = left_segments.remove(idx);
                total_width = total_width.saturating_sub(removed.display_width);
            } else {
                break;
            }
        }

        // Recalculate after truncation
        let left_width: usize = left_segments.iter().map(|s| s.display_width).sum();
        let right_width: usize = right_segments.iter().map(|s| s.display_width).sum();

        // Assemble result: left segments + gap + right segments
        let mut result = String::new();

        // Left segments
        for segment in &left_segments {
            result.push_str(&segment.content);
        }

        // Gap padding (fill middle with spaces)
        let gap = max_width.saturating_sub(left_width + right_width);
        if gap > 0 {
            result.push_str(&bar_bg);
            result.push_str(&" ".repeat(gap));
        }

        // Right segments
        for segment in &right_segments {
            result.push_str(&segment.content);
        }

        result
    }
}

/// Converts a ratatui Color to ANSI foreground escape sequence.
pub fn color_to_fg_ansi(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[39m".to_string(),
        Color::Black => "\x1b[30m".to_string(),
        Color::Red => "\x1b[31m".to_string(),
        Color::Green => "\x1b[32m".to_string(),
        Color::Yellow => "\x1b[33m".to_string(),
        Color::Blue => "\x1b[34m".to_string(),
        Color::Magenta => "\x1b[35m".to_string(),
        Color::Cyan => "\x1b[36m".to_string(),
        Color::Gray => "\x1b[37m".to_string(),
        Color::DarkGray => "\x1b[90m".to_string(),
        Color::LightRed => "\x1b[91m".to_string(),
        Color::LightGreen => "\x1b[92m".to_string(),
        Color::LightYellow => "\x1b[93m".to_string(),
        Color::LightBlue => "\x1b[94m".to_string(),
        Color::LightMagenta => "\x1b[95m".to_string(),
        Color::LightCyan => "\x1b[96m".to_string(),
        Color::White => "\x1b[97m".to_string(),
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{};{};{}m", r, g, b),
        Color::Indexed(i) => format!("\x1b[38;5;{}m", i),
    }
}

/// Converts a ratatui Color to ANSI background escape sequence.
pub fn color_to_bg_ansi(color: Color) -> String {
    match color {
        Color::Reset => "\x1b[49m".to_string(),
        Color::Black => "\x1b[40m".to_string(),
        Color::Red => "\x1b[41m".to_string(),
        Color::Green => "\x1b[42m".to_string(),
        Color::Yellow => "\x1b[43m".to_string(),
        Color::Blue => "\x1b[44m".to_string(),
        Color::Magenta => "\x1b[45m".to_string(),
        Color::Cyan => "\x1b[46m".to_string(),
        Color::Gray => "\x1b[47m".to_string(),
        Color::DarkGray => "\x1b[100m".to_string(),
        Color::LightRed => "\x1b[101m".to_string(),
        Color::LightGreen => "\x1b[102m".to_string(),
        Color::LightYellow => "\x1b[103m".to_string(),
        Color::LightBlue => "\x1b[104m".to_string(),
        Color::LightMagenta => "\x1b[105m".to_string(),
        Color::LightCyan => "\x1b[106m".to_string(),
        Color::White => "\x1b[107m".to_string(),
        Color::Rgb(r, g, b) => format!("\x1b[48;2;{};{};{}m", r, g, b),
        Color::Indexed(i) => format!("\x1b[48;5;{}m", i),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_width() {
        assert_eq!(strip_ansi_width("hello"), 5);
        assert_eq!(strip_ansi_width("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(strip_ansi_width("\x1b[38;2;255;0;0mrgb\x1b[0m"), 3);
    }

    #[test]
    fn test_rendered_segment_new() {
        let segment = RenderedSegment::new("hello".to_string(), 1, SegmentAlign::Left);
        assert_eq!(segment.display_width, 5);
        assert_eq!(segment.priority, 1);
        assert_eq!(segment.align, SegmentAlign::Left);
    }

    #[test]
    fn test_rendered_segment_with_ansi() {
        let segment = RenderedSegment::left("\x1b[32m✓\x1b[0m".to_string(), 0);
        assert_eq!(segment.display_width, 1); // Just the checkmark
    }

    #[test]
    fn test_registry_default() {
        let registry = TopbarRegistry::with_defaults();
        assert_eq!(registry.segments.len(), 6);
    }

    #[test]
    fn test_segment_align_default() {
        assert_eq!(SegmentAlign::default(), SegmentAlign::Left);
    }
}
