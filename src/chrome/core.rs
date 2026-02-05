//! Chrome layer for status bar and scroll regions.
//!
//! This module manages the visual chrome (context bar) using terminal
//! scroll regions to reserve screen real estate outside the shell area.
//!
//! # Architecture
//!
//! The chrome layer is orthogonal to the main Mode state machine. It can be
//! enabled or disabled independently, and auto-suspends when the terminal
//! is too small to display the bar meaningfully.
//!
//! # Strategic Snapshot Rendering
//!
//! The context bar is rendered once at state transitions (specifically when
//! entering Edit mode) before reedline takes control. This eliminates
//! flickering and cursor conflicts that would occur with continuous refresh.
//! The bar displays rich context: exit code, command duration, current
//! directory, git status, and timestamp.
//!
//! # Safety
//!
//! The most critical aspect is scroll region reset on Passthrough entry.
//! This happens **unconditionally** to prevent terminal corruption from
//! full-screen applications like vim and htop.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::Color;
use tracing::{debug, info, warn};
use unicode_width::UnicodeWidthStr;

use super::buffer_convert::buffer_to_ansi;
use super::symbols::Symbols;
use super::theme::Theme;
use crate::config::Config;
use crate::types::ChromeMode;

/// Minimum terminal columns for chrome to be active.
const MIN_COLS: u16 = 20;

/// Minimum terminal rows for chrome to be active.
const MIN_ROWS: u16 = 5;

/// Result of checking minimum terminal size.
///
/// Indicates whether chrome state changed during the size check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeCheckResult {
    /// No state change occurred.
    NoChange,
    /// Chrome was just suspended due to small terminal.
    Suspended,
    /// Chrome was just resumed after terminal grew.
    Resumed,
}

/// State of expandable panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelState {
    /// Normal 1-row context bar only.
    Collapsed,
    /// Panel visible with N rows reserved.
    Expanded { height: u16 },
}

/// Style for notification messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationStyle {
    /// Informational message (blue).
    Info,
    /// Success message (green).
    Success,
    /// Warning message (yellow).
    Warning,
    /// Error message (red).
    Error,
}

/// A notification to display in the context bar area.
#[derive(Debug, Clone)]
pub struct Notification {
    /// The notification message.
    pub message: String,
    /// The style/type of notification.
    pub style: NotificationStyle,
    /// When the notification expires.
    pub expires_at: Instant,
}

/// Alignment for context bar segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentAlign {
    /// Left-aligned segment.
    Left,
    /// Right-aligned segment (clock).
    Right,
}

/// A segment of the context bar with priority and styling.
struct ContextSegment {
    /// Formatted segment content (with ANSI codes).
    content: String,
    /// Display width (excluding ANSI codes).
    display_width: usize,
    /// Priority (lower = more important, kept first during truncation).
    priority: u8,
    /// Alignment within the bar.
    align: SegmentAlign,
}

/// Chrome layer manager for status bar, scroll regions, and panels.
///
/// The Chrome struct manages:
/// - Mode state (Headless vs Full)
/// - Auto-suspension when terminal is too small
/// - Scroll region setup/reset
/// - Context bar rendering
/// - Expandable panels
/// - Notifications
pub struct Chrome {
    /// Current chrome display mode.
    mode: ChromeMode,

    /// Whether chrome is temporarily suspended due to small terminal size.
    /// User preference (mode) is preserved and chrome resumes when terminal grows.
    suspended: bool,

    /// Current panel state (collapsed or expanded).
    panel_state: PanelState,

    /// Queue of active notifications.
    notifications: VecDeque<Notification>,

    /// Theme for rendering.
    theme: &'static Theme,

    /// Symbols for icons.
    symbols: &'static Symbols,

    /// Last rendered minute (0-59) for efficient clock updates.
    last_rendered_minute: Option<u8>,
}

/// Context information for rendering the chrome bar.
///
/// Contains all metadata needed to render a rich context bar showing
/// command results, current location, and git status.
pub struct ChromeContext<'a> {
    /// Current working directory.
    pub cwd: &'a Path,
    /// Git branch name, if in a repository.
    pub git_branch: Option<&'a str>,
    /// Whether git working directory is dirty.
    pub git_dirty: bool,
    /// Exit code of the last command.
    pub last_exit_code: i32,
    /// The last command that was executed.
    pub last_command: Option<&'a str>,
    /// Duration of the last command execution.
    pub last_duration: Option<Duration>,
    /// Current timestamp string (HH:MM format).
    pub timestamp: &'a str,
    /// Scroll position information (percentage from top, if scrolled).
    pub scroll_info: Option<ScrollInfo>,
}

/// Information about scroll position for display in context bar.
#[derive(Debug, Clone, Copy)]
pub struct ScrollInfo {
    /// Scroll percentage (0 = at bottom/live, 100 = at top/oldest).
    pub percentage: u8,
    /// Total lines in scrollback buffer.
    pub total_lines: usize,
    /// First visible line number (1-indexed from oldest).
    pub first_visible_line: usize,
}

impl Chrome {
    /// Creates a new Chrome instance with the specified mode and configuration.
    ///
    /// # Arguments
    ///
    /// * `mode` - Initial chrome display mode (Headless or Full)
    /// * `config` - Application configuration for theme and symbol selection
    pub fn new(mode: ChromeMode, config: &Config) -> Self {
        let theme = Theme::for_preset(config.theme);
        let symbols = Symbols::for_set(config.symbol_set);
        info!(mode = ?mode, theme = ?config.theme, symbols = ?config.symbol_set, "Chrome layer initialized");
        Self {
            mode,
            suspended: false,
            panel_state: PanelState::Collapsed,
            notifications: VecDeque::new(),
            theme,
            symbols,
            last_rendered_minute: None,
        }
    }

    /// Returns the theme used by this Chrome instance.
    pub fn theme(&self) -> &'static Theme {
        self.theme
    }

    /// Returns the symbols used by this Chrome instance.
    pub fn symbols(&self) -> &'static Symbols {
        self.symbols
    }

    /// Checks if the clock should be updated based on the current minute.
    ///
    /// Returns true if the minute has changed since last render.
    pub fn should_update_clock(&self, minute: u8) -> bool {
        self.last_rendered_minute != Some(minute)
    }

    /// Marks the current minute as rendered.
    pub fn mark_minute_rendered(&mut self, minute: u8) {
        self.last_rendered_minute = Some(minute);
    }

    /// Returns true if chrome is actively displaying bars.
    ///
    /// Chrome is active when:
    /// - Mode is Full AND
    /// - Not suspended due to small terminal size
    pub fn is_active(&self) -> bool {
        self.mode == ChromeMode::Full && !self.suspended
    }

    /// Returns the current chrome mode.
    pub fn mode(&self) -> ChromeMode {
        self.mode
    }

    /// Toggles between Headless and Full modes.
    ///
    /// This changes the user preference but doesn't update the terminal.
    /// Use `toggle_with_terminal_update()` for a complete toggle with visual updates.
    pub fn toggle(&mut self) {
        self.mode = match self.mode {
            ChromeMode::Headless => ChromeMode::Full,
            ChromeMode::Full => ChromeMode::Headless,
        };
        info!(mode = ?self.mode, "Chrome mode toggled");
    }

    /// Checks if terminal meets minimum size requirements.
    ///
    /// Auto-suspends chrome if terminal is too small, auto-resumes when it grows.
    /// Logs warnings/info on state changes.
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    /// * `rows` - Terminal height in rows
    ///
    /// # Returns
    ///
    /// A `SizeCheckResult` indicating whether chrome state changed:
    /// - `NoChange` - No state transition occurred
    /// - `Suspended` - Chrome was just suspended (caller should clear bars and reset scroll region)
    /// - `Resumed` - Chrome was just resumed (caller should set up scroll region and draw bars)
    pub fn check_minimum_size(&mut self, cols: u16, rows: u16) -> SizeCheckResult {
        let meets_minimum = cols >= MIN_COLS && rows >= MIN_ROWS;

        if !meets_minimum && !self.suspended && self.mode == ChromeMode::Full {
            warn!(
                cols,
                rows,
                min_cols = MIN_COLS,
                min_rows = MIN_ROWS,
                "Terminal too small for chrome, suspending"
            );
            self.suspended = true;
            SizeCheckResult::Suspended
        } else if meets_minimum && self.suspended {
            info!(cols, rows, "Terminal size restored, resuming chrome");
            self.suspended = false;
            SizeCheckResult::Resumed
        } else {
            SizeCheckResult::NoChange
        }
    }

    /// Called when entering Passthrough mode.
    ///
    /// **MANDATORY**: Always emits scroll region reset (`\x1b[r`) regardless of
    /// chrome state. This is critical for preventing terminal corruption from
    /// full-screen applications like vim and htop.
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequence cannot be written to stdout.
    pub fn enter_passthrough_mode(&self) -> io::Result<()> {
        // ALWAYS reset scroll region on Passthrough entry - defense against corruption
        Self::reset_scroll_region()?;
        debug!("Scroll region reset for Passthrough mode");
        Ok(())
    }

    /// Called when entering Edit mode.
    ///
    /// Sets up scroll region if chrome is active to reserve space for bars.
    ///
    /// # Arguments
    ///
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequence cannot be written to stdout.
    pub fn enter_edit_mode(&mut self, total_rows: u16) -> io::Result<()> {
        if self.is_active() {
            self.setup_scroll_region(total_rows)?;
            debug!(total_rows, "Scroll region set for Edit mode with chrome");
        }
        Ok(())
    }

    /// Sets up the scroll region for chrome display.
    ///
    /// Emits DECSTBM sequence to set scroll region from row 2 to row N,
    /// reserving row 1 for the context bar.
    ///
    /// **Note**: DECSTBM resets the cursor to the home position (top-left of
    /// scroll region). Use `setup_scroll_region_preserve_cursor` if you need
    /// to preserve the cursor position after command output.
    ///
    /// # Arguments
    ///
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequence cannot be written to stdout.
    pub fn setup_scroll_region(&self, total_rows: u16) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        let mut out = io::stdout();
        // DECSTBM: Set scrolling region from row 2 to row total_rows
        // Row 1 is for context bar, rows 2-N are scroll region
        write!(out, "\x1b[2;{}r", total_rows)?;
        out.flush()?;

        debug!(top = 2, bottom = total_rows, "Scroll region configured");
        Ok(())
    }

    /// Sets up the scroll region while preserving cursor position.
    ///
    /// This function sets up the scroll region (rows 2 to N) and preserves
    /// the cursor position. This allows natural top-to-bottom terminal flow
    /// where output appears immediately after the previous content.
    ///
    /// **Note**: DECSTBM resets cursor to home position as a side effect.
    /// We save and restore the cursor to maintain the natural flow.
    ///
    /// # Arguments
    ///
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequence cannot be written to stdout.
    pub fn setup_scroll_region_preserve_cursor(&self, total_rows: u16) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        let bottom_row = total_rows;

        // Lock stdout for atomic writes - prevents interleaving with other threads.
        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Save cursor position before DECSTBM resets it
        write!(out, "\x1b[s")?;

        // DECSTBM: Set scrolling region from row 2 to row total_rows
        // Row 1 is for context bar, rows 2-N are scroll region
        // This moves cursor to row 1 as a side effect.
        write!(out, "\x1b[2;{}r", bottom_row)?;

        // Restore cursor to its original position
        // This preserves natural top-to-bottom flow
        write!(out, "\x1b[u")?;

        out.flush()?;

        debug!(
            top = 2,
            bottom = bottom_row,
            "Scroll region configured, cursor preserved"
        );
        Ok(())
    }

    /// Resets the scroll region to full screen.
    ///
    /// Emits DECSTBM reset sequence (`\x1b[r`) to restore scroll region
    /// to encompass the entire terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequence cannot be written to stdout.
    pub fn reset_scroll_region() -> io::Result<()> {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        write!(out, "\x1b[r")?;
        out.flush()?;
        debug!("Scroll region reset to full screen");
        Ok(())
    }

    /// Positions the cursor at the start of the scroll region.
    ///
    /// Moves cursor to row 2, column 1 (the first line of the content area
    /// when chrome is active). This should be called after drawing bars
    /// to ensure subsequent output appears in the scroll region.
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequence cannot be written to stdout.
    pub fn position_cursor_in_scroll_region(&self) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        let mut out = io::stdout();
        write!(out, "\x1b[2;1H")?; // Move to row 2, column 1
        out.flush()?;
        debug!("Cursor positioned at scroll region start (row 2)");
        Ok(())
    }

    /// Renders the context bar with rich command and environment information.
    ///
    /// This is the single render point for chrome - called once at transition
    /// to Edit mode before reedline takes control. The bar shows:
    /// - Success/failure indicator (✓/✗) - green/red
    /// - Command duration - yellow if >= 0.5s
    /// - Current directory - cyan
    /// - Git branch and dirty status - magenta (bold if dirty)
    /// - Current time - dim
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    /// * `ctx` - Context information to display
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn render_context_bar(&self, cols: u16, ctx: &ChromeContext) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        let content = self.format_context_bar_colored(cols as usize, ctx);
        let bar_bg = Self::color_to_bg_ansi(self.theme.bar_bg);

        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Save cursor, move to row 1, clear entire line, draw content, restore cursor
        write!(out, "\x1b[s")?; // Save cursor position
        write!(out, "\x1b[1;1H")?; // Move to row 1, column 1
        write!(out, "\x1b[2K")?; // Clear ENTIRE line (not just to end)
        // Use theme background for the bar
        write!(out, "{}{}\x1b[0m", bar_bg, content)?;
        write!(out, "\x1b[u")?; // Restore cursor position
        out.flush()?;

        debug!("Context bar rendered");
        Ok(())
    }

    /// Converts a ratatui Color to ANSI foreground escape sequence.
    fn color_to_fg_ansi(color: Color) -> String {
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
    fn color_to_bg_ansi(color: Color) -> String {
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

    /// Formats the context bar content with ANSI colors.
    ///
    /// Uses priority-based truncation: when the bar is too wide, segments
    /// with higher priority numbers are removed first.
    ///
    /// Layout: [status]▶[duration]▶[cwd]▶[git]▶ ... ▶[clock]
    ///         └──────────── LEFT ───────────┘       └─RIGHT─┘
    fn format_context_bar_colored(&self, max_width: usize, ctx: &ChromeContext) -> String {
        let bar_bg = Self::color_to_bg_ansi(self.theme.bar_bg);
        let sep_fg = Self::color_to_fg_ansi(self.theme.separator_fg);
        let separator = self.symbols.separator_right;
        let separator_width = separator.width();

        let mut left_segments: Vec<ContextSegment> = Vec::new();
        let mut right_segments: Vec<ContextSegment> = Vec::new();

        // === LEFT SEGMENTS ===

        // Status icon: priority 0 (always shown)
        let (status_icon, status_color) = if ctx.last_exit_code == 0 {
            (self.symbols.success, Self::color_to_fg_ansi(self.theme.success_fg))
        } else {
            (self.symbols.failure, Self::color_to_fg_ansi(self.theme.failure_fg))
        };
        let status_width = status_icon.width();
        left_segments.push(ContextSegment {
            content: format!(" {}{}{} ", status_color, status_icon, bar_bg),
            display_width: status_width + 2, // space + icon + space
            priority: 0,
            align: SegmentAlign::Left,
        });

        // Scroll indicator: priority 0 (always shown when active)
        if let Some(scroll_info) = ctx.scroll_info {
            let scroll_fg = Self::color_to_fg_ansi(self.theme.separator_fg);
            // Format: ▶ SCROLL | line/total | pct%
            let scroll_content = format!(
                "▶ SCROLL | {}/{} | {}%",
                scroll_info.first_visible_line,
                scroll_info.total_lines,
                scroll_info.percentage
            );
            let scroll_width = scroll_content.width();
            // Insert at position 0 to show before status icon
            left_segments.insert(0, ContextSegment {
                content: format!(" {}{} ", scroll_fg, scroll_content),
                display_width: scroll_width + 2, // " content "
                priority: 0, // Always show
                align: SegmentAlign::Left,
            });
        }

        // Duration: priority 3, shown only if >= 0.5s to avoid clutter
        if let Some(dur) = ctx.last_duration {
            let secs = dur.as_secs_f64();
            if secs >= 0.5 {
                let stopwatch = self.symbols.stopwatch;
                let stopwatch_width = stopwatch.width();
                let duration_str = if secs >= 60.0 {
                    let mins = (secs / 60.0).floor() as u32;
                    let remaining_secs = secs % 60.0;
                    format!("{}m{:.0}s", mins, remaining_secs)
                } else {
                    format!("{:.1}s", secs)
                };
                let color = if secs >= 5.0 {
                    Self::color_to_fg_ansi(self.theme.duration_slow_fg)
                } else {
                    Self::color_to_fg_ansi(self.theme.duration_fg)
                };
                let icon_part = if !stopwatch.is_empty() {
                    format!("{} ", stopwatch)
                } else {
                    String::new()
                };
                let icon_width = if !stopwatch.is_empty() { stopwatch_width + 1 } else { 0 };
                left_segments.push(ContextSegment {
                    content: format!(" {}{} {}{}{} ", sep_fg, separator, color, icon_part, duration_str),
                    display_width: separator_width + icon_width + duration_str.len() + 3, // +3 for " ▶ " + trailing space
                    priority: 3,
                    align: SegmentAlign::Left,
                });
            }
        }

        // CWD: priority 2
        let cwd_str = ctx.cwd.file_name().and_then(|n| n.to_str()).unwrap_or("/");
        let folder = self.symbols.folder;
        let folder_width = folder.width();
        let cwd_fg = Self::color_to_fg_ansi(self.theme.cwd_fg);
        let cwd_icon_part = if !folder.is_empty() {
            format!("{} ", folder)
        } else {
            String::new()
        };
        let cwd_icon_width = if !folder.is_empty() { folder_width + 1 } else { 0 };
        left_segments.push(ContextSegment {
            content: format!(" {}{} {}{}{} ", sep_fg, separator, cwd_fg, cwd_icon_part, cwd_str),
            display_width: separator_width + cwd_icon_width + cwd_str.width() + 3, // +3 for " ▶ " + trailing space
            priority: 2,
            align: SegmentAlign::Left,
        });

        // Git: priority 4
        if let Some(branch) = ctx.git_branch {
            let git_branch_icon = self.symbols.git_branch;
            let git_branch_width = git_branch_icon.width();
            let dirty_icon = self.symbols.git_dirty;
            let dirty_width = if ctx.git_dirty { dirty_icon.width() } else { 0 };

            let (git_fg, dirty_part) = if ctx.git_dirty {
                (
                    Self::color_to_fg_ansi(self.theme.git_dirty_fg),
                    dirty_icon,
                )
            } else {
                (Self::color_to_fg_ansi(self.theme.git_fg), "")
            };

            let icon_part = if !git_branch_icon.is_empty() {
                format!("{} ", git_branch_icon)
            } else {
                String::new()
            };
            let icon_width = if !git_branch_icon.is_empty() { git_branch_width + 1 } else { 0 };

            left_segments.push(ContextSegment {
                content: format!(" {}{} {}{}{}{} ", sep_fg, separator, git_fg, icon_part, branch, dirty_part),
                display_width: separator_width + icon_width + branch.len() + dirty_width + 3, // +3 for " ▶ " + trailing space
                priority: 4,
                align: SegmentAlign::Left,
            });
        }

        // === RIGHT SEGMENTS ===

        // Clock: priority 1, right-aligned
        let clock_icon = self.symbols.clock;
        let clock_icon_width = clock_icon.width();
        let clock_fg = Self::color_to_fg_ansi(self.theme.clock_fg);
        let clock_icon_part = if !clock_icon.is_empty() {
            format!("{} ", clock_icon)
        } else {
            String::new()
        };
        let clock_icon_display_width = if !clock_icon.is_empty() { clock_icon_width + 1 } else { 0 };
        right_segments.push(ContextSegment {
            content: format!(" {}{} {}{}{} ", sep_fg, separator, clock_fg, clock_icon_part, ctx.timestamp),
            display_width: separator_width + clock_icon_display_width + ctx.timestamp.len() + 3, // +3 for " ▶ " + trailing space
            priority: 1,
            align: SegmentAlign::Right,
        });

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

    /// Formats the context bar content with rich information (plain version).
    ///
    /// Layout: [✓/✗] [duration] [cwd] [git:branch●] [time]
    #[allow(dead_code)]
    fn format_context_bar(&self, max_width: usize, ctx: &ChromeContext) -> String {
        // Build components
        let status_icon = if ctx.last_exit_code == 0 {
            "✓"
        } else {
            "✗"
        };

        let duration_str = if let Some(dur) = ctx.last_duration {
            let secs = dur.as_secs_f64();
            if secs < 1.0 {
                format!("{:.1}s", secs)
            } else {
                format!("{:.1}s", secs)
            }
        } else {
            String::new()
        };

        let cwd_str = ctx.cwd.file_name().and_then(|n| n.to_str()).unwrap_or("/");

        let git_str = if let Some(branch) = ctx.git_branch {
            if ctx.git_dirty {
                format!("{}●", branch)
            } else {
                branch.to_string()
            }
        } else {
            String::new()
        };

        // Assemble bar content
        let mut parts = Vec::new();
        parts.push(format!(" {} ", status_icon));

        if !duration_str.is_empty() {
            parts.push(format!("{} ", duration_str));
        }

        parts.push(format!("{} ", cwd_str));

        if !git_str.is_empty() {
            parts.push(format!("git:{} ", git_str));
        }

        parts.push(format!("{} ", ctx.timestamp));

        let content = parts.join("");
        let content_width = content.width();

        if content_width > max_width {
            // Truncate with ellipsis
            let truncated = Self::truncate_to_width(&content, max_width.saturating_sub(3));
            format!("{}...", truncated)
        } else {
            // Pad to full width
            let padding = max_width.saturating_sub(content_width);
            format!("{}{}", content, " ".repeat(padding))
        }
    }

    /// Truncates a string to fit within the specified display width.
    ///
    /// Handles Unicode characters correctly by using display width rather
    /// than byte or character count.
    fn truncate_to_width(s: &str, max_width: usize) -> &str {
        let mut current_width = 0;
        let mut last_valid_idx = 0;

        for (idx, ch) in s.char_indices() {
            let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width + ch_width > max_width {
                break;
            }
            current_width += ch_width;
            last_valid_idx = idx + ch.len_utf8();
        }

        &s[..last_valid_idx]
    }

    /// Clears the content area (the scroll region) and positions cursor.
    ///
    /// Clears rows 2 through N (the scroll region area) and positions the cursor
    /// at row 2, column 1. This provides a clean slate after fullscreen apps
    /// or commands, ensuring predictable prompt positioning.
    ///
    /// Only performs clearing when chrome is active. When chrome is not active,
    /// this is a no-op.
    ///
    /// # Arguments
    ///
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn clear_content_area(&self, total_rows: u16) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        // Lock stdout for atomic writes - prevents interleaving with other threads.
        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Clear each row in the content area (rows 2 through total_rows inclusive)
        for row in 2..=total_rows {
            write!(out, "\x1b[{};1H", row)?; // Move to row
            write!(out, "\x1b[K")?; // Clear line
        }

        // Position cursor at start of content area
        write!(out, "\x1b[2;1H")?;
        out.flush()?;

        debug!("Content area cleared, cursor at row 2");
        Ok(())
    }

    /// Clears the context bar from the terminal.
    ///
    /// **Note**: This function moves the cursor. The caller should save/restore
    /// cursor position if needed.
    ///
    /// # Arguments
    ///
    /// * `_total_rows` - Total terminal height (unused, kept for API compatibility)
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn clear_bars(&self, _total_rows: u16) -> io::Result<()> {
        // Lock stdout for atomic writes - prevents interleaving with other threads.
        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Clear top bar (context bar)
        write!(out, "\x1b[1;1H")?; // Move to row 1
        write!(out, "\x1b[K")?; // Clear line

        out.flush()?;
        debug!("Context bar cleared");
        Ok(())
    }

    /// Toggles chrome mode with full terminal update.
    ///
    /// Handles all visual updates when switching between Headless and Full:
    /// - Enabling: Sets up scroll region (caller should render context bar)
    /// - Disabling: Clears bar and resets scroll region
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    /// * `rows` - Terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    pub fn toggle_with_terminal_update(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        match self.mode {
            ChromeMode::Headless => {
                // Enabling chrome
                self.mode = ChromeMode::Full;
                // Just check size - we proceed to setup if is_active() regardless of transition
                let _ = self.check_minimum_size(cols, rows);

                if self.is_active() {
                    self.setup_scroll_region(rows)?;
                    // Note: Caller should render context bar with proper context
                }
                info!("Chrome enabled");
            }
            ChromeMode::Full => {
                // Disabling chrome
                if self.is_active() {
                    self.clear_bars(rows)?;
                }
                self.mode = ChromeMode::Headless;
                Self::reset_scroll_region()?;
                info!("Chrome disabled");
            }
        }
        Ok(())
    }

    // =========================================================================
    // Panel Lifecycle Methods
    // =========================================================================

    /// Expands the panel area to the specified height.
    ///
    /// Updates the scroll region to reserve space for the panel at the top
    /// of the terminal.
    ///
    /// # Arguments
    ///
    /// * `height` - The height of the panel area (in rows)
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn expand_panel(&mut self, height: u16, total_rows: u16) -> io::Result<()> {
        self.panel_state = PanelState::Expanded { height };

        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Set scroll region from panel_height + 1 to total_rows
        // Panels occupy rows 1 through height, content is height+1 to total_rows
        let top_row = height + 1;
        write!(out, "\x1b[{};{}r", top_row, total_rows)?;

        // Position cursor at bottom of scroll region
        write!(out, "\x1b[{};1H", total_rows)?;

        out.flush()?;

        debug!(
            panel_height = height,
            scroll_top = top_row,
            scroll_bottom = total_rows,
            "Panel expanded"
        );

        Ok(())
    }

    /// Collapses the panel back to the normal context bar.
    ///
    /// Restores the scroll region for normal chrome operation.
    ///
    /// # Arguments
    ///
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn collapse_panel(&mut self, total_rows: u16) -> io::Result<()> {
        let old_height = match self.panel_state {
            PanelState::Expanded { height } => height,
            PanelState::Collapsed => return Ok(()),
        };

        self.panel_state = PanelState::Collapsed;

        // Lock stdout for atomic writes - prevents interleaving with other threads.
        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Reset scroll region to full screen first (required to clear rows outside
        // the panel's scroll region which was height+1 to total_rows)
        write!(out, "\x1b[0m")?;   // Reset attributes
        write!(out, "\x1b[r")?;    // Reset scroll region to full screen

        // Clear each panel row
        for row in 1..=old_height {
            write!(out, "\x1b[{};1H\x1b[2K", row)?;
        }

        out.flush()?;
        drop(out); // Release lock before calling methods that also lock

        // Restore scroll region for chrome mode
        self.setup_scroll_region(total_rows)?;

        // Position cursor at row 2 for subsequent output
        self.position_cursor_in_scroll_region()?;

        debug!(old_height, "Panel collapsed");

        Ok(())
    }

    /// Returns the current panel height.
    ///
    /// Returns 1 if collapsed (just context bar), otherwise returns the
    /// expanded panel height.
    pub fn panel_height(&self) -> u16 {
        match self.panel_state {
            PanelState::Collapsed => 1,
            PanelState::Expanded { height } => height,
        }
    }

    /// Returns the current panel state.
    pub fn panel_state(&self) -> PanelState {
        self.panel_state
    }

    /// Renders a ratatui buffer to the terminal.
    ///
    /// Converts the buffer to ANSI sequences and writes them to stdout.
    ///
    /// # Arguments
    ///
    /// * `buffer` - The ratatui buffer to render
    /// * `area` - The area of the buffer to render
    ///
    /// # Errors
    ///
    /// Returns an error if writing to stdout fails.
    pub fn render_panel_buffer(&self, buffer: &Buffer, area: Rect) -> io::Result<()> {
        let ansi = buffer_to_ansi(buffer, area);

        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Save cursor, write buffer content, restore cursor
        write!(out, "\x1b[s")?;
        write!(out, "{}", ansi)?;
        write!(out, "\x1b[u")?;
        out.flush()?;

        Ok(())
    }

    // =========================================================================
    // Notification Methods
    // =========================================================================

    /// Adds a notification to the queue.
    ///
    /// The notification will be displayed instead of the normal context bar
    /// until it expires.
    ///
    /// # Arguments
    ///
    /// * `message` - The notification message
    /// * `style` - The notification style/type
    /// * `duration` - How long the notification should be displayed
    pub fn notify(
        &mut self,
        message: impl Into<String>,
        style: NotificationStyle,
        duration: Duration,
    ) {
        let notification = Notification {
            message: message.into(),
            style,
            expires_at: Instant::now() + duration,
        };
        self.notifications.push_back(notification);
        debug!("Notification added");
    }

    /// Removes expired notifications from the queue.
    fn expire_notifications(&mut self) {
        let now = Instant::now();
        while let Some(notif) = self.notifications.front() {
            if notif.expires_at <= now {
                self.notifications.pop_front();
            } else {
                break;
            }
        }
    }

    /// Renders a notification to the context bar area.
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    /// * `notif` - The notification to render
    ///
    /// # Errors
    ///
    /// Returns an error if writing to stdout fails.
    fn render_notification(&self, cols: u16, notif: &Notification) -> io::Result<()> {
        // Map style to theme colors and symbols
        let (bg_color, fg_color, icon) = match notif.style {
            NotificationStyle::Info => (
                self.theme.semantic_info,
                self.theme.bar_bg,
                self.symbols.notif_info,
            ),
            NotificationStyle::Success => (
                self.theme.semantic_success,
                self.theme.bar_bg,
                self.symbols.success,
            ),
            NotificationStyle::Warning => (
                self.theme.semantic_warning,
                self.theme.bar_bg,
                self.symbols.notif_warning,
            ),
            NotificationStyle::Error => (
                self.theme.semantic_error,
                self.theme.bar_bg,
                self.symbols.failure,
            ),
        };
        let bg = Self::color_to_bg_ansi(bg_color);
        let fg = Self::color_to_fg_ansi(fg_color);

        // Format: " icon  message  icon "
        let prefix = format!(" {} ", icon);
        let suffix = format!(" {} ", icon);
        let decoration_width = prefix.width() + suffix.width();

        // Truncate and pad message to fit within available space
        let max_len = cols as usize;
        let available_for_msg = max_len.saturating_sub(decoration_width);
        let msg_display = if notif.message.width() > available_for_msg {
            let truncated = Self::truncate_to_width(&notif.message, available_for_msg.saturating_sub(3));
            format!("{}...", truncated)
        } else {
            notif.message.clone()
        };

        // Build full notification with padding
        let content = format!("{}{}{}", prefix, msg_display, suffix);
        let padding = max_len.saturating_sub(content.width());
        let display_msg = format!("{}{}", content, " ".repeat(padding));

        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Save cursor, draw notification, restore cursor
        write!(out, "\x1b[s")?;
        write!(out, "\x1b[1;1H")?;
        write!(out, "\x1b[2K")?; // Clear ENTIRE line
        write!(out, "{}{}{}\x1b[0m", bg, fg, display_msg)?;
        write!(out, "\x1b[u")?;
        out.flush()?;

        Ok(())
    }

    /// Renders the context bar, checking for notifications first.
    ///
    /// If there's an active notification, it's displayed instead of the
    /// normal context bar.
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    /// * `ctx` - Context information for normal context bar
    ///
    /// # Errors
    ///
    /// Returns an error if writing to stdout fails.
    pub fn render_context_bar_with_notifications(
        &mut self,
        cols: u16,
        ctx: &ChromeContext,
    ) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        // Expire old notifications
        self.expire_notifications();

        // Check for active notification
        if let Some(notif) = self.notifications.front().cloned() {
            self.render_notification(cols, &notif)
        } else {
            self.render_context_bar(cols, ctx)
        }
    }

    /// Returns whether there are active notifications.
    pub fn has_notifications(&self) -> bool {
        !self.notifications.is_empty()
    }

    /// Clears all notifications.
    pub fn clear_notifications(&mut self) {
        self.notifications.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a default config for tests.
    fn test_config() -> Config {
        Config::default()
    }

    #[test]
    fn test_chrome_new_headless() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Headless, &config);
        assert_eq!(chrome.mode(), ChromeMode::Headless);
        assert!(!chrome.is_active());
    }

    #[test]
    fn test_chrome_new_full() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Full, &config);
        assert_eq!(chrome.mode(), ChromeMode::Full);
        assert!(chrome.is_active());
    }

    #[test]
    fn test_chrome_toggle() {
        let config = test_config();
        let mut chrome = Chrome::new(ChromeMode::Headless, &config);
        assert_eq!(chrome.mode(), ChromeMode::Headless);

        chrome.toggle();
        assert_eq!(chrome.mode(), ChromeMode::Full);

        chrome.toggle();
        assert_eq!(chrome.mode(), ChromeMode::Headless);
    }

    #[test]
    fn test_chrome_is_active_when_suspended() {
        let config = test_config();
        let mut chrome = Chrome::new(ChromeMode::Full, &config);
        assert!(chrome.is_active());

        // Suspend by reporting small terminal
        let result = chrome.check_minimum_size(10, 3);
        assert_eq!(result, SizeCheckResult::Suspended);
        assert!(!chrome.is_active());
        assert_eq!(chrome.mode(), ChromeMode::Full); // Mode preserved

        // Resume by reporting adequate size
        let result = chrome.check_minimum_size(80, 24);
        assert_eq!(result, SizeCheckResult::Resumed);
        assert!(chrome.is_active());

        // No change when already active and size is adequate
        let result = chrome.check_minimum_size(80, 24);
        assert_eq!(result, SizeCheckResult::NoChange);
        assert!(chrome.is_active());
    }

    #[test]
    fn test_chrome_minimum_size_constants() {
        assert!(MIN_COLS > 0);
        assert!(MIN_ROWS > 0);
        assert!(MIN_COLS <= 80);
        assert!(MIN_ROWS <= 24);
    }

    #[test]
    fn test_check_minimum_size_boundary() {
        let config = test_config();
        let mut chrome = Chrome::new(ChromeMode::Full, &config);

        // Exactly at minimum should be fine (no change, still active)
        let result = chrome.check_minimum_size(MIN_COLS, MIN_ROWS);
        assert_eq!(result, SizeCheckResult::NoChange);
        assert!(chrome.is_active());

        // One below minimum should suspend
        let result = chrome.check_minimum_size(MIN_COLS - 1, MIN_ROWS);
        assert_eq!(result, SizeCheckResult::Suspended);
        assert!(!chrome.is_active());
    }

    #[test]
    fn test_truncate_to_width_ascii() {
        let s = "Hello, World!";
        assert_eq!(Chrome::truncate_to_width(s, 5), "Hello");
        assert_eq!(Chrome::truncate_to_width(s, 100), s);
        assert_eq!(Chrome::truncate_to_width(s, 0), "");
    }

    #[test]
    fn test_truncate_to_width_unicode() {
        // CJK characters have width 2
        let s = "Hello\u{4E2D}\u{6587}"; // "Hello中文"
        assert_eq!(Chrome::truncate_to_width(s, 5), "Hello");
        assert_eq!(Chrome::truncate_to_width(s, 7), "Hello\u{4E2D}");
        assert_eq!(Chrome::truncate_to_width(s, 6), "Hello"); // Can't fit half a CJK char
    }

    #[test]
    fn test_format_context_bar_success() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Full, &config);
        let cwd = Path::new("/home/user/project");
        let ctx = ChromeContext {
            cwd,
            git_branch: Some("main"),
            git_dirty: false,
            last_exit_code: 0,
            last_command: Some("echo test"),
            last_duration: Some(Duration::from_millis(123)),
            timestamp: "14:32",
            scroll_info: None,
        };

        let result = chrome.format_context_bar(80, &ctx);

        assert!(result.contains("✓"));
        // Duration < 0.5s not shown in new layout
        assert!(result.contains("project"));
        assert!(result.contains("main"));
        assert!(result.contains("14:32"));
        assert_eq!(result.width(), 80);
    }

    #[test]
    fn test_format_context_bar_failure() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Full, &config);
        let cwd = Path::new("/tmp");
        let ctx = ChromeContext {
            cwd,
            git_branch: None,
            git_dirty: false,
            last_exit_code: 1,
            last_command: Some("false"),
            last_duration: Some(Duration::from_millis(50)),
            timestamp: "14:33",
            scroll_info: None,
        };

        let result = chrome.format_context_bar(80, &ctx);

        assert!(result.contains("✗"));
    }

    #[test]
    fn test_format_context_bar_with_dirty_git() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Full, &config);
        let cwd = Path::new("/home/user/project");
        let ctx = ChromeContext {
            cwd,
            git_branch: Some("feature"),
            git_dirty: true,
            last_exit_code: 0,
            last_command: None,
            last_duration: None,
            timestamp: "14:34",
            scroll_info: None,
        };

        let result = chrome.format_context_bar(80, &ctx);

        // In fallback mode, dirty indicator is ●
        assert!(result.contains("feature"));
    }

    #[test]
    fn test_format_context_bar_truncation() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Full, &config);
        let cwd = Path::new("/very/long/path/to/project/directory");
        let ctx = ChromeContext {
            cwd,
            git_branch: Some("feature/very-long-branch-name"),
            git_dirty: true,
            last_exit_code: 0,
            last_command: Some("very long command"),
            last_duration: Some(Duration::from_secs(123)),
            timestamp: "14:32",
            scroll_info: None,
        };

        let result = chrome.format_context_bar(40, &ctx);

        assert!(result.width() <= 40);
        // May or may not contain ellipsis depending on truncation strategy
    }

    #[test]
    fn test_clear_content_area_noop_when_headless() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Headless, &config);
        // Should succeed without doing anything (headless mode)
        assert!(chrome.clear_content_area(24).is_ok());
    }

    #[test]
    fn test_clear_content_area_succeeds_when_active() {
        let config = test_config();
        let chrome = Chrome::new(ChromeMode::Full, &config);
        // Should succeed (writes escape sequences to stdout)
        assert!(chrome.clear_content_area(24).is_ok());
    }
}
