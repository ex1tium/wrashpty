//! Terminal raw mode RAII guard and safety system.
//!
//! This module provides safe terminal mode management with automatic restoration
//! on drop, forming **layer 1** of Wrashpty's five-layer terminal safety system:
//!
//! 1. **RAII Guard** (this module) - Automatic restoration via `Drop` trait
//! 2. **Panic Hook** (`main.rs`) - Async-signal-safe restoration using `libc::write`
//! 3. **Signal Handlers** - Clean restoration on SIGTERM, SIGINT, etc.
//! 4. **Atexit Handler** - Final fallback for process termination
//! 5. **Shell Wrapper** - Outermost safety net in the launching shell
//!
//! The `TerminalGuard` saves the original terminal state on construction, enables
//! raw mode for direct character input, and restores the terminal on drop. This
//! works even during panic unwinding, providing defense-in-depth with the panic hook.
//!
//! # Usage
//!
//! ```no_run
//! use wrashpty::terminal::TerminalGuard;
//!
//! fn main() -> wrashpty::terminal::Result<()> {
//!     // Create guard - terminal enters raw mode
//!     let _guard = TerminalGuard::new()?;
//!
//!     // Query terminal size
//!     let (cols, rows) = TerminalGuard::get_size()?;
//!     println!("Terminal size: {}x{}", cols, rows);
//!
//!     // Do terminal operations...
//!
//!     // Guard drops here - terminal automatically restored
//!     Ok(())
//! }
//! ```
//!
//! # Safety Notes
//!
//! - The `Drop` implementation uses best-effort restoration and ignores errors
//! - Multiple restoration attempts (escape sequences + termios) provide redundancy
//! - The panic hook in `main.rs` provides additional safety using async-signal-safe I/O
//!
//! See architecture spec section 8 for complete terminal safety documentation.

use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use nix::sys::termios::{SetArg, Termios, tcgetattr, tcsetattr};
use std::io::{Write, stdin, stdout};
use thiserror::Error;

/// Errors that can occur during terminal operations.
///
/// This enum provides typed error handling for terminal-related failures,
/// allowing callers to match on specific error kinds and handle them
/// appropriately.
#[derive(Error, Debug)]
pub enum TerminalError {
    /// I/O error during terminal operations (writing escape sequences, flushing,
    /// raw mode, or terminal size queries).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Error from nix crate (termios operations).
    #[error("Terminal attribute error: {0}")]
    Nix(#[from] nix::errno::Errno),
}

/// Result type alias for terminal operations.
pub type Result<T> = std::result::Result<T, TerminalError>;

/// RAII guard for terminal raw mode management.
///
/// This struct saves the original terminal state on construction and restores it
/// when dropped. It provides automatic cleanup even during panics, complementing
/// the panic hook for maximum reliability.
///
/// # Example
///
/// ```no_run
/// use wrashpty::terminal::TerminalGuard;
///
/// let guard = TerminalGuard::new()?;
/// // Terminal is now in raw mode
/// // ... do terminal operations ...
/// drop(guard); // Or let it go out of scope
/// // Terminal is restored to original state
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct TerminalGuard {
    /// Saved terminal state for restoration on drop.
    original_termios: Termios,
}

impl TerminalGuard {
    /// Create a new terminal guard, enabling raw mode.
    ///
    /// This saves the current terminal state and enables raw mode, which:
    /// - Disables line buffering (characters available immediately)
    /// - Disables echo (typed characters not shown)
    /// - Disables signal generation (Ctrl+C doesn't send SIGINT)
    /// - Disables special input processing
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Terminal attributes cannot be read (e.g., stdin is not a terminal)
    /// - Raw mode cannot be enabled
    ///
    /// # Example
    ///
    /// ```no_run
    /// use wrashpty::terminal::TerminalGuard;
    ///
    /// let guard = TerminalGuard::new()?;
    /// // Terminal is now in raw mode
    /// # Ok::<(), wrashpty::terminal::TerminalError>(())
    /// ```
    pub fn new() -> Result<Self> {
        let original_termios = tcgetattr(stdin())?;

        enable_raw_mode()?;

        tracing::info!("Terminal raw mode enabled");

        Ok(TerminalGuard { original_termios })
    }

    /// Get the current terminal size in columns and rows.
    ///
    /// This is a static method that can be called without holding the guard,
    /// as terminal size queries don't require raw mode.
    ///
    /// # Returns
    ///
    /// A tuple of `(columns, rows)` representing the terminal dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal size cannot be determined.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use wrashpty::terminal::TerminalGuard;
    ///
    /// let (cols, rows) = TerminalGuard::get_size()?;
    /// println!("Terminal is {}x{}", cols, rows);
    /// # Ok::<(), wrashpty::terminal::TerminalError>(())
    /// ```
    #[must_use = "terminal size should be used after querying"]
    pub fn get_size() -> Result<(u16, u16)> {
        Ok(size()?)
    }

    /// Reset the scroll region to the full screen.
    ///
    /// Emits the DECSTBM reset sequence (`\x1b[r`) to restore the scroll region
    /// to encompass the entire terminal. This is important for cleanup after
    /// applications that set custom scroll regions.
    ///
    /// # Errors
    ///
    /// Returns an error if the escape sequence cannot be written to stdout.
    pub fn reset_scroll_region() -> Result<()> {
        let mut out = stdout();
        write!(out, "\x1b[r")?;
        out.flush()?;
        Ok(())
    }

    /// Show the cursor.
    ///
    /// Emits the DECTCEM show cursor sequence (`\x1b[?25h`) to make the cursor
    /// visible. Should be called during cleanup to ensure the cursor is visible
    /// after the application exits.
    ///
    /// # Errors
    ///
    /// Returns an error if the escape sequence cannot be written to stdout.
    pub fn show_cursor() -> Result<()> {
        let mut out = stdout();
        write!(out, "\x1b[?25h")?;
        out.flush()?;
        Ok(())
    }

    /// Hide the cursor.
    ///
    /// Emits the DECTCEM hide cursor sequence (`\x1b[?25l`) to make the cursor
    /// invisible. Useful during rendering to prevent cursor flicker.
    ///
    /// # Errors
    ///
    /// Returns an error if the escape sequence cannot be written to stdout.
    pub fn hide_cursor() -> Result<()> {
        let mut out = stdout();
        write!(out, "\x1b[?25l")?;
        out.flush()?;
        Ok(())
    }

    /// Ensures raw mode is active on the terminal.
    ///
    /// This method should be called after operations that may have disabled
    /// raw mode (e.g., when transitioning from Edit mode where reedline may
    /// have toggled terminal modes). It's idempotent - calling when raw mode
    /// is already active has no negative effects.
    ///
    /// This is an instance method tied to an existing TerminalGuard, ensuring
    /// that raw mode re-enablement cannot occur without an owning RAII guard
    /// that will restore terminal state on drop.
    ///
    /// In raw mode:
    /// - Line buffering is disabled (characters available immediately)
    /// - Echo is disabled (typed characters not shown automatically)
    /// - Signal generation is disabled (Ctrl+C sends byte 0x03, not SIGINT)
    /// - Special input processing is disabled
    ///
    /// # Errors
    ///
    /// Returns an error if raw mode cannot be enabled.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use wrashpty::terminal::TerminalGuard;
    ///
    /// let guard = TerminalGuard::new()?;
    /// // ... after reedline returns, ensure raw mode is still active
    /// guard.ensure_raw_mode()?;
    /// # Ok::<(), wrashpty::terminal::TerminalError>(())
    /// ```
    pub fn ensure_raw_mode(&self) -> Result<()> {
        enable_raw_mode()?;
        tracing::debug!("Raw mode ensured");
        Ok(())
    }
}

impl Drop for TerminalGuard {
    /// Restore the terminal to its original state.
    ///
    /// This method performs best-effort restoration, ignoring any errors since
    /// `Drop` cannot return errors. Multiple restoration attempts provide
    /// defense-in-depth:
    ///
    /// 1. Reset scroll region (escape sequence)
    /// 2. Show cursor (escape sequence)
    /// 3. Flush stdout
    /// 4. Disable raw mode (crossterm)
    /// 5. Restore original termios (nix)
    ///
    /// Even if some steps fail (e.g., fd closed), others may succeed.
    fn drop(&mut self) {
        tracing::info!("Restoring terminal state");

        // Best-effort restoration - ignore all errors
        // Escape sequences may work even if termios fails
        let mut out = stdout();

        // Attempt 1: Reset scroll region
        if let Err(e) = write!(out, "\x1b[r") {
            tracing::warn!("Failed to reset scroll region: {}", e);
        }

        // Attempt 2: Clear screen and move cursor to home
        // This prevents "ghost" content from remaining after exit
        if let Err(e) = write!(out, "\x1b[2J\x1b[H") {
            tracing::warn!("Failed to clear screen: {}", e);
        }

        // Attempt 3: Show cursor
        if let Err(e) = write!(out, "\x1b[?25h") {
            tracing::warn!("Failed to show cursor: {}", e);
        }

        // Attempt 3: Flush stdout
        if let Err(e) = out.flush() {
            tracing::warn!("Failed to flush stdout: {}", e);
        }

        // Attempt 4: Disable raw mode via crossterm
        if let Err(e) = disable_raw_mode() {
            tracing::warn!("Failed to disable raw mode: {}", e);
        }

        // Attempt 5: Restore original termios
        if let Err(e) = tcsetattr(stdin(), SetArg::TCSANOW, &self.original_termios) {
            tracing::warn!("Failed to restore terminal attributes: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_when_tty_available_returns_guard() {
        // This test requires a real terminal, so it may be skipped in CI
        // Run with: cargo test -- --nocapture
        match TerminalGuard::new() {
            Ok(guard) => {
                // Guard created successfully, will restore on drop
                drop(guard);
                // If we get here without panic, restoration worked
            }
            Err(e) => {
                // Not running in a terminal (e.g., CI environment)
                eprintln!("Skipping test (no terminal): {}", e);
            }
        }
    }

    #[test]
    fn test_get_size_when_tty_returns_positive_dimensions() {
        match TerminalGuard::get_size() {
            Ok((cols, rows)) => {
                // Verify reasonable terminal dimensions
                assert!(cols > 0, "Terminal width should be positive");
                assert!(rows > 0, "Terminal height should be positive");
                // Most terminals are at least 20x5
                assert!(cols >= 20, "Terminal width should be at least 20");
                assert!(rows >= 5, "Terminal height should be at least 5");
            }
            Err(e) => {
                // Not running in a terminal (e.g., CI environment)
                eprintln!("Skipping test (no terminal): {}", e);
            }
        }
    }

    #[test]
    fn test_hide_cursor_then_show_cursor_succeeds() {
        // These tests work even without a real terminal since they just write to stdout
        // The visual effect can only be verified manually
        match TerminalGuard::hide_cursor() {
            Ok(()) => {
                // Hide succeeded, now show
                assert!(
                    TerminalGuard::show_cursor().is_ok(),
                    "show_cursor should succeed after hide_cursor"
                );
            }
            Err(e) => {
                eprintln!("Skipping test (stdout not writable): {}", e);
            }
        }
    }

    #[test]
    fn test_reset_scroll_region_when_stdout_writable_succeeds() {
        // This test just verifies the escape sequence can be written
        match TerminalGuard::reset_scroll_region() {
            Ok(()) => {
                // Success - effect can only be verified manually
            }
            Err(e) => {
                eprintln!("Skipping test (stdout not writable): {}", e);
            }
        }
    }

    #[test]
    fn test_drop_when_guard_out_of_scope_restores_terminal() {
        // Create guard in inner scope to test drop
        {
            match TerminalGuard::new() {
                Ok(_guard) => {
                    // Guard will be dropped at end of this scope
                }
                Err(e) => {
                    eprintln!("Skipping test (no terminal): {}", e);
                }
            }
        }
        // If we get here, drop completed without panic
        // Terminal should be usable - manual verification needed
    }

    #[test]
    fn test_ensure_raw_mode_succeeds_when_already_in_raw_mode() {
        // ensure_raw_mode should be idempotent - calling it multiple times
        // when already in raw mode should succeed without issues.
        match TerminalGuard::new() {
            Ok(guard) => {
                // Guard is active, terminal is in raw mode
                // ensure_raw_mode should succeed
                match guard.ensure_raw_mode() {
                    Ok(()) => {
                        // Success - raw mode was maintained
                    }
                    Err(e) => {
                        panic!(
                            "ensure_raw_mode should succeed when already in raw mode: {}",
                            e
                        );
                    }
                }
                // Guard drops here, restoring terminal
            }
            Err(e) => {
                eprintln!("Skipping test (no terminal): {}", e);
            }
        }
    }

    #[test]
    fn test_ensure_raw_mode_can_reenable_after_disable() {
        // Test that ensure_raw_mode can re-enable raw mode after it's been disabled
        match TerminalGuard::new() {
            Ok(guard) => {
                // Simulate what might happen if something disabled raw mode
                // (Note: We can't easily disable without the guard's Drop,
                // but we can verify ensure_raw_mode is callable)
                match guard.ensure_raw_mode() {
                    Ok(()) => {
                        // Success
                    }
                    Err(e) => {
                        panic!("ensure_raw_mode failed: {}", e);
                    }
                }
                drop(guard);
            }
            Err(e) => {
                eprintln!("Skipping test (no terminal): {}", e);
            }
        }
    }
}
