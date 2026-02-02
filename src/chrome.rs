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

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use tracing::{debug, info, warn};
use unicode_width::UnicodeWidthStr;

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

/// Chrome layer manager for status bar and scroll regions.
///
/// The Chrome struct manages:
/// - Mode state (Headless vs Full)
/// - Auto-suspension when terminal is too small
/// - Scroll region setup/reset
/// - Context bar rendering
pub struct Chrome {
    /// Current chrome display mode.
    mode: ChromeMode,

    /// Whether chrome is temporarily suspended due to small terminal size.
    /// User preference (mode) is preserved and chrome resumes when terminal grows.
    suspended: bool,
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
}

impl Chrome {
    /// Creates a new Chrome instance with the specified mode.
    ///
    /// # Arguments
    ///
    /// * `mode` - Initial chrome display mode (Headless or Full)
    pub fn new(mode: ChromeMode) -> Self {
        info!(mode = ?mode, "Chrome layer initialized");
        Self {
            mode,
            suspended: false,
        }
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

        debug!(
            top = 2,
            bottom = total_rows,
            "Scroll region configured"
        );
        Ok(())
    }

    /// Sets up the scroll region and positions cursor at bottom of region.
    ///
    /// This function sets up the scroll region (rows 2 to N) and positions
    /// the cursor at the bottom row of the scroll region. This ensures that
    /// subsequent command output will appear at the bottom and scroll naturally.
    ///
    /// **Important**: Reedline may leave the cursor outside the scroll region
    /// after accepting input. Simply restoring that position would cause output
    /// to go to the wrong place. By explicitly positioning the cursor inside
    /// the scroll region, we ensure proper scrolling behavior.
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

        // DECSTBM: Set scrolling region from row 2 to row total_rows
        // Row 1 is for context bar, rows 2-N are scroll region
        // This moves cursor to row 1 as a side effect.
        write!(out, "\x1b[2;{}r", bottom_row)?;

        // CRITICAL: Position cursor at bottom of scroll region (row N).
        // If cursor is outside scroll region, output won't scroll properly.
        // By positioning at the bottom of scroll region, subsequent output will
        // appear there and scroll naturally when newlines are encountered.
        write!(out, "\x1b[{};1H", bottom_row)?;

        out.flush()?;

        debug!(
            top = 2,
            bottom = bottom_row,
            cursor_row = bottom_row,
            "Scroll region configured, cursor at bottom"
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
    /// - Success/failure indicator (✓/✗)
    /// - Command duration
    /// - Current directory
    /// - Git branch and dirty status
    /// - Current time
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

        let content = self.format_context_bar(cols as usize, ctx);

        let stdout = io::stdout();
        let mut out = stdout.lock();

        // Save cursor, move to row 1, clear line, draw content, restore cursor
        write!(out, "\x1b[s")?; // Save cursor position
        write!(out, "\x1b[1;1H")?; // Move to row 1, column 1
        write!(out, "\x1b[K")?; // Clear line
        write!(out, "\x1b[7m{}\x1b[0m", content)?; // Reverse video for bar
        write!(out, "\x1b[u")?; // Restore cursor position
        out.flush()?;

        debug!("Context bar rendered");
        Ok(())
    }

    /// Formats the context bar content with rich information.
    ///
    /// Layout: [✓/✗] [duration] [cwd] [git:branch●] [time]
    fn format_context_bar(&self, max_width: usize, ctx: &ChromeContext) -> String {
        // Build components
        let status_icon = if ctx.last_exit_code == 0 { "✓" } else { "✗" };

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

        let cwd_str = ctx.cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/");

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrome_new_headless() {
        let chrome = Chrome::new(ChromeMode::Headless);
        assert_eq!(chrome.mode(), ChromeMode::Headless);
        assert!(!chrome.is_active());
    }

    #[test]
    fn test_chrome_new_full() {
        let chrome = Chrome::new(ChromeMode::Full);
        assert_eq!(chrome.mode(), ChromeMode::Full);
        assert!(chrome.is_active());
    }

    #[test]
    fn test_chrome_toggle() {
        let mut chrome = Chrome::new(ChromeMode::Headless);
        assert_eq!(chrome.mode(), ChromeMode::Headless);

        chrome.toggle();
        assert_eq!(chrome.mode(), ChromeMode::Full);

        chrome.toggle();
        assert_eq!(chrome.mode(), ChromeMode::Headless);
    }

    #[test]
    fn test_chrome_is_active_when_suspended() {
        let mut chrome = Chrome::new(ChromeMode::Full);
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
        let mut chrome = Chrome::new(ChromeMode::Full);

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
        let chrome = Chrome::new(ChromeMode::Full);
        let cwd = Path::new("/home/user/project");
        let ctx = ChromeContext {
            cwd,
            git_branch: Some("main"),
            git_dirty: false,
            last_exit_code: 0,
            last_command: Some("echo test"),
            last_duration: Some(Duration::from_millis(123)),
            timestamp: "14:32",
        };

        let result = chrome.format_context_bar(80, &ctx);

        assert!(result.contains("✓"));
        assert!(result.contains("0.1s"));
        assert!(result.contains("project"));
        assert!(result.contains("main"));
        assert!(result.contains("14:32"));
        assert_eq!(result.width(), 80);
    }

    #[test]
    fn test_format_context_bar_failure() {
        let chrome = Chrome::new(ChromeMode::Full);
        let cwd = Path::new("/tmp");
        let ctx = ChromeContext {
            cwd,
            git_branch: None,
            git_dirty: false,
            last_exit_code: 1,
            last_command: Some("false"),
            last_duration: Some(Duration::from_millis(50)),
            timestamp: "14:33",
        };

        let result = chrome.format_context_bar(80, &ctx);

        assert!(result.contains("✗"));
        assert!(!result.contains("git:"));
    }

    #[test]
    fn test_format_context_bar_with_dirty_git() {
        let chrome = Chrome::new(ChromeMode::Full);
        let cwd = Path::new("/home/user/project");
        let ctx = ChromeContext {
            cwd,
            git_branch: Some("feature"),
            git_dirty: true,
            last_exit_code: 0,
            last_command: None,
            last_duration: None,
            timestamp: "14:34",
        };

        let result = chrome.format_context_bar(80, &ctx);

        assert!(result.contains("feature●"));
    }

    #[test]
    fn test_format_context_bar_truncation() {
        let chrome = Chrome::new(ChromeMode::Full);
        let cwd = Path::new("/very/long/path/to/project/directory");
        let ctx = ChromeContext {
            cwd,
            git_branch: Some("feature/very-long-branch-name"),
            git_dirty: true,
            last_exit_code: 0,
            last_command: Some("very long command"),
            last_duration: Some(Duration::from_secs(123)),
            timestamp: "14:32",
        };

        let result = chrome.format_context_bar(40, &ctx);

        assert!(result.width() <= 40);
        assert!(result.contains("..."));
    }

    #[test]
    fn test_clear_content_area_noop_when_headless() {
        let chrome = Chrome::new(ChromeMode::Headless);
        // Should succeed without doing anything (headless mode)
        assert!(chrome.clear_content_area(24).is_ok());
    }

    #[test]
    fn test_clear_content_area_succeeds_when_active() {
        let chrome = Chrome::new(ChromeMode::Full);
        // Should succeed (writes escape sequences to stdout)
        assert!(chrome.clear_content_area(24).is_ok());
    }
}
