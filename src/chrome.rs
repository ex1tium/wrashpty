//! Chrome layer for status bars and scroll regions.
//!
//! This module manages the visual chrome (top bar, footer) using terminal
//! scroll regions to reserve screen real estate outside the shell area.
//!
//! # Architecture
//!
//! The chrome layer is orthogonal to the main Mode state machine. It can be
//! enabled or disabled independently, and auto-suspends when the terminal
//! is too small to display bars meaningfully.
//!
//! # Chrome Refresh
//!
//! Because reedline/crossterm may overwrite the footer during initialization,
//! the `ChromeRefreshGuard` provides a background thread that periodically
//! redraws the chrome bars during Edit mode. This ensures the bars remain
//! visible even when reedline clears or repaints the terminal.
//!
//! # Safety
//!
//! The most critical aspect is scroll region reset on Passthrough entry.
//! This happens **unconditionally** to prevent terminal corruption from
//! full-screen applications like vim and htop.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing::{debug, info, trace, warn};
use unicode_width::UnicodeWidthStr;

use crate::types::ChromeMode;

/// Refresh interval for chrome bars during Edit mode (30fps).
const CHROME_REFRESH_INTERVAL: Duration = Duration::from_millis(33);

/// Initial delay before first refresh to let reedline initialize.
const CHROME_REFRESH_INITIAL_DELAY: Duration = Duration::from_millis(50);

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

/// Chrome layer manager for status bars and scroll regions.
///
/// The Chrome struct manages:
/// - Mode state (Headless vs Full)
/// - Auto-suspension when terminal is too small
/// - Scroll region setup/reset
/// - Top bar and footer rendering
pub struct Chrome {
    /// Current chrome display mode.
    mode: ChromeMode,

    /// Whether chrome is temporarily suspended due to small terminal size.
    /// User preference (mode) is preserved and chrome resumes when terminal grows.
    suspended: bool,
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
    /// Emits DECSTBM sequence to set scroll region from row 2 to row N-1,
    /// reserving row 1 for top bar and row N for footer.
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
        // DECSTBM: Set scrolling region from row 2 to row (total_rows - 1)
        // Row 1 is for top bar, row total_rows is for footer
        write!(out, "\x1b[2;{}r", total_rows - 1)?;
        out.flush()?;

        debug!(
            top = 2,
            bottom = total_rows - 1,
            "Scroll region configured"
        );
        Ok(())
    }

    /// Sets up the scroll region while preserving cursor position.
    ///
    /// Unlike `setup_scroll_region`, this version saves the cursor position
    /// before setting the scroll region and restores it afterward. This is
    /// important when returning from Passthrough mode where command output
    /// should remain visible.
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

        let mut out = io::stdout();
        // Save cursor position before DECSTBM (which resets cursor to home)
        write!(out, "\x1b[s")?;
        // DECSTBM: Set scrolling region from row 2 to row (total_rows - 1)
        write!(out, "\x1b[2;{}r", total_rows - 1)?;
        // Restore cursor position
        write!(out, "\x1b[u")?;
        out.flush()?;

        debug!(
            top = 2,
            bottom = total_rows - 1,
            "Scroll region configured (cursor preserved)"
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
        let mut out = io::stdout();
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

    /// Draws the top bar at row 1.
    ///
    /// Saves cursor position, draws bar content, and restores cursor.
    /// Returns early if chrome is not active.
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn draw_top_bar(&self, cols: u16) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        let mut out = io::stdout();
        let content = self.render_top_bar_content(cols as usize);

        // Save cursor, move to row 1, clear line, draw content, restore cursor
        write!(out, "\x1b[s")?; // Save cursor position
        write!(out, "\x1b[1;1H")?; // Move to row 1, column 1
        write!(out, "\x1b[K")?; // Clear line
        write!(out, "\x1b[7m{}\x1b[0m", content)?; // Reverse video for bar
        write!(out, "\x1b[u")?; // Restore cursor position
        out.flush()?;

        debug!("Top bar drawn");
        Ok(())
    }

    /// Draws the footer bar at the last row.
    ///
    /// Saves cursor position, draws bar content, and restores cursor.
    /// Returns early if chrome is not active.
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn draw_footer(&self, cols: u16, total_rows: u16) -> io::Result<()> {
        if !self.is_active() {
            return Ok(());
        }

        let mut out = io::stdout();
        let content = self.render_footer_content(cols as usize);

        // Save cursor, move to last row, clear line, draw content, restore cursor
        write!(out, "\x1b[s")?; // Save cursor position
        write!(out, "\x1b[{};1H", total_rows)?; // Move to last row, column 1
        write!(out, "\x1b[K")?; // Clear line
        write!(out, "\x1b[7m{}\x1b[0m", content)?; // Reverse video for bar
        write!(out, "\x1b[u")?; // Restore cursor position
        out.flush()?;

        debug!("Footer drawn");
        Ok(())
    }

    /// Renders the top bar content with proper width handling.
    ///
    /// Displays the project name (current directory name) padded to fill
    /// the available width. Truncates with "..." if too long.
    fn render_top_bar_content(&self, max_width: usize) -> String {
        let project_name = Self::get_project_name();
        let label = format!(" {} ", project_name);
        let label_width = label.width();

        if label_width > max_width {
            // Truncate and add ellipsis
            let truncated = Self::truncate_to_width(&label, max_width.saturating_sub(3));
            format!("{}...", truncated)
        } else {
            // Pad to full width
            let padding = max_width.saturating_sub(label_width);
            format!("{}{}", label, " ".repeat(padding))
        }
    }

    /// Renders the footer content with keybinding hints.
    ///
    /// Displays keyboard shortcuts padded to fill the available width.
    /// Truncates if too long for the terminal.
    fn render_footer_content(&self, max_width: usize) -> String {
        let hints = " Tab: complete | Ctrl+R: search | Ctrl+C: clear | Ctrl+D: exit ";
        let hints_width = hints.width();

        if hints_width > max_width {
            // Truncate to fit
            let truncated = Self::truncate_to_width(hints, max_width);
            truncated.to_string()
        } else {
            // Pad to full width
            let padding = max_width.saturating_sub(hints_width);
            format!("{}{}", hints, " ".repeat(padding))
        }
    }

    /// Gets the project name from the current directory.
    ///
    /// Falls back to "wrashpty" if current directory cannot be determined.
    fn get_project_name() -> String {
        std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "wrashpty".to_string())
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

    /// Clears the content area (between chrome bars) and positions cursor.
    ///
    /// Clears rows 2 to N-1 (the scroll region area) and positions the cursor
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

        let mut out = io::stdout();

        // Clear each row in the content area (rows 2 to N-1)
        for row in 2..total_rows {
            write!(out, "\x1b[{};1H", row)?; // Move to row
            write!(out, "\x1b[K")?; // Clear line
        }

        // Position cursor at start of content area
        write!(out, "\x1b[2;1H")?;
        out.flush()?;

        debug!("Content area cleared, cursor at row 2");
        Ok(())
    }

    /// Clears the chrome bars from the terminal.
    ///
    /// **Note**: This function moves the cursor. The caller should save/restore
    /// cursor position if needed.
    ///
    /// # Arguments
    ///
    /// * `total_rows` - Total terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if escape sequences cannot be written to stdout.
    pub fn clear_bars(&self, total_rows: u16) -> io::Result<()> {
        let mut out = io::stdout();

        // Clear top bar
        write!(out, "\x1b[1;1H")?; // Move to row 1
        write!(out, "\x1b[K")?; // Clear line

        // Clear footer
        write!(out, "\x1b[{};1H", total_rows)?; // Move to last row
        write!(out, "\x1b[K")?; // Clear line

        out.flush()?;
        debug!("Chrome bars cleared");
        Ok(())
    }

    /// Toggles chrome mode with full terminal update.
    ///
    /// Handles all visual updates when switching between Headless and Full:
    /// - Enabling: Sets up scroll region and draws bars
    /// - Disabling: Clears bars and resets scroll region
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
                // Just check size - we proceed to draw if is_active() regardless of transition
                let _ = self.check_minimum_size(cols, rows);

                if self.is_active() {
                    self.setup_scroll_region(rows)?;
                    self.draw_top_bar(cols)?;
                    self.draw_footer(cols, rows)?;
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

    /// Redraws chrome bars if active.
    ///
    /// Called on SIGWINCH to update bars after terminal resize.
    ///
    /// # Arguments
    ///
    /// * `cols` - Terminal width in columns
    /// * `rows` - Terminal height in rows
    ///
    /// # Errors
    ///
    /// Returns an error if drawing operations fail.
    pub fn redraw_if_active(&self, cols: u16, rows: u16) -> io::Result<()> {
        if self.is_active() {
            self.draw_top_bar(cols)?;
            self.draw_footer(cols, rows)?;
        }
        Ok(())
    }
}

/// Shared state for the chrome refresh thread.
///
/// Contains atomic values that can be safely accessed from both the main
/// thread and the refresh thread.
struct RefreshState {
    /// Flag to signal the refresh thread to stop.
    stop: AtomicBool,
    /// Current terminal width (updated on resize).
    cols: AtomicU16,
    /// Current terminal height (updated on resize).
    rows: AtomicU16,
    /// Whether chrome is currently active.
    active: AtomicBool,
}

/// RAII guard for the chrome refresh background thread.
///
/// While this guard is alive, a background thread periodically redraws the
/// chrome bars (top bar and footer). This compensates for reedline/crossterm
/// potentially overwriting the bars during terminal operations.
///
/// The thread is automatically stopped when the guard is dropped.
///
/// # Usage
///
/// ```ignore
/// // Start refresh before reedline takes control
/// let refresh_guard = ChromeRefreshGuard::start(cols, rows, chrome.is_active());
///
/// // reedline.read_line() blocks here...
/// // Background thread keeps footer visible
///
/// // Guard dropped, thread stops
/// drop(refresh_guard);
/// ```
pub struct ChromeRefreshGuard {
    /// Shared state with the refresh thread.
    state: Arc<RefreshState>,
    /// Handle to the refresh thread.
    handle: Option<JoinHandle<()>>,
}

impl ChromeRefreshGuard {
    /// Starts the chrome refresh background thread.
    ///
    /// The thread will periodically redraw the chrome bars at approximately
    /// 30fps while `active` is true. It respects terminal dimensions and
    /// will stop when the guard is dropped.
    ///
    /// # Arguments
    ///
    /// * `cols` - Initial terminal width
    /// * `rows` - Initial terminal height
    /// * `active` - Whether chrome is currently active
    pub fn start(cols: u16, rows: u16, active: bool) -> Self {
        let state = Arc::new(RefreshState {
            stop: AtomicBool::new(false),
            cols: AtomicU16::new(cols),
            rows: AtomicU16::new(rows),
            active: AtomicBool::new(active),
        });

        let state_clone = Arc::clone(&state);
        let handle = thread::spawn(move || {
            chrome_refresh_loop(state_clone);
        });

        debug!("Chrome refresh thread started");

        Self {
            state,
            handle: Some(handle),
        }
    }

    /// Updates the terminal dimensions for the refresh thread.
    ///
    /// Call this when the terminal is resized (SIGWINCH) to ensure the
    /// refresh thread draws bars at the correct positions.
    pub fn update_size(&self, cols: u16, rows: u16) {
        self.state.cols.store(cols, Ordering::Relaxed);
        self.state.rows.store(rows, Ordering::Relaxed);
    }

    /// Updates whether chrome is active.
    ///
    /// When inactive, the refresh thread will skip drawing.
    pub fn set_active(&self, active: bool) {
        self.state.active.store(active, Ordering::Relaxed);
    }

    /// Stops the refresh thread and waits for it to finish.
    pub fn stop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
            debug!("Chrome refresh thread stopped");
        }
    }
}

impl Drop for ChromeRefreshGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Background loop that periodically redraws chrome bars.
///
/// This function runs in a dedicated thread and redraws the footer
/// at regular intervals to compensate for reedline potentially
/// overwriting it.
fn chrome_refresh_loop(state: Arc<RefreshState>) {
    // Initial delay to let reedline do its setup/clearing
    thread::sleep(CHROME_REFRESH_INITIAL_DELAY);

    while !state.stop.load(Ordering::Relaxed) {
        if state.active.load(Ordering::Relaxed) {
            // Query current terminal size to handle resizes correctly.
            // This adds one syscall per refresh but ensures correct positioning.
            let (cols, rows) = match crossterm::terminal::size() {
                Ok((c, r)) => (c, r),
                Err(_) => {
                    // Fall back to stored values if query fails
                    (
                        state.cols.load(Ordering::Relaxed),
                        state.rows.load(Ordering::Relaxed),
                    )
                }
            };

            // Redraw footer (the part most likely to be overwritten)
            if let Err(e) = draw_footer_static(cols, rows) {
                trace!("Chrome refresh failed: {}", e);
            }
        }

        thread::sleep(CHROME_REFRESH_INTERVAL);
    }
}

/// Draws the footer without requiring a Chrome instance.
///
/// This is a static version used by the refresh thread which doesn't
/// have access to the Chrome struct.
fn draw_footer_static(cols: u16, total_rows: u16) -> io::Result<()> {
    let hints = " Tab: complete | Ctrl+R: search | Ctrl+C: clear | Ctrl+D: exit ";
    let hints_width = hints.width();
    let max_width = cols as usize;

    let content = if hints_width > max_width {
        // Truncate to fit
        truncate_to_width_static(hints, max_width).to_string()
    } else {
        // Pad to full width
        let padding = max_width.saturating_sub(hints_width);
        format!("{}{}", hints, " ".repeat(padding))
    };

    let mut out = io::stdout();
    write!(out, "\x1b[s")?; // Save cursor position
    write!(out, "\x1b[{};1H", total_rows)?; // Move to last row
    write!(out, "\x1b[K")?; // Clear line
    write!(out, "\x1b[7m{}\x1b[0m", content)?; // Reverse video
    write!(out, "\x1b[u")?; // Restore cursor position
    out.flush()?;

    Ok(())
}

/// Static version of truncate_to_width for the refresh thread.
fn truncate_to_width_static(s: &str, max_width: usize) -> &str {
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
    fn test_get_project_name_returns_string() {
        // Should return some string, either current dir name or fallback
        let name = Chrome::get_project_name();
        assert!(!name.is_empty());
    }

    #[test]
    fn test_render_top_bar_content_fits() {
        let chrome = Chrome::new(ChromeMode::Full);
        let content = chrome.render_top_bar_content(80);
        assert_eq!(content.width(), 80);
    }

    #[test]
    fn test_render_top_bar_content_truncates() {
        let chrome = Chrome::new(ChromeMode::Full);
        let content = chrome.render_top_bar_content(10);
        // Should be truncated to fit
        assert!(content.width() <= 10);
        assert!(content.ends_with("...") || content.width() == 10);
    }

    #[test]
    fn test_render_footer_content_fits() {
        let chrome = Chrome::new(ChromeMode::Full);
        let content = chrome.render_footer_content(80);
        assert_eq!(content.width(), 80);
    }

    #[test]
    fn test_render_footer_content_truncates() {
        let chrome = Chrome::new(ChromeMode::Full);
        let content = chrome.render_footer_content(20);
        assert!(content.width() <= 20);
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
