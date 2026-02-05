//! Mode transitions and signal handling for the App state machine.
//!
//! This module handles:
//! - Mode transitions (Edit, Passthrough, Injecting, Terminating)
//! - Signal handling (SIGWINCH, SIGCHLD, shutdown)
//! - Chrome toggle and related utilities

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::terminal::TerminalGuard;
use crate::types::{Mode, SignalEvent};

use super::{App, exit_code_from_status};

impl App {
    // =========================================================================
    // Mode transitions
    // =========================================================================

    /// Transitions to Edit mode.
    ///
    /// Sets up scroll region and renders context bar if chrome is active.
    /// Resizes PTY to effective rows (accounting for top bar).
    ///
    /// When coming from Passthrough, the scroll region is already set and cursor
    /// is at the end of command output. We preserve cursor position so the prompt
    /// appears after the output (natural shell flow).
    pub(super) fn transition_to_edit(&mut self) {
        let from_mode = self.mode;
        info!(from = ?from_mode, to = ?Mode::Edit, "Mode transition");

        // Calculate command duration if coming from command execution
        if let Some(start) = self.command_start_time.take() {
            self.last_command_duration = Some(start.elapsed());

            // Update history metadata with exit status, duration, and cwd
            // Also learn from the command completion for intelligence
            if let Ok(mut store) = self.history_store.lock() {
                let exit_status = Some(self.last_exit_code);
                let cwd = Some(self.current_cwd.clone());
                if let Err(e) =
                    store.update_last_command(exit_status, self.last_command_duration, cwd)
                {
                    warn!("Failed to update history metadata: {}", e);
                }
                // Learn from the command completion
                store.learn_command_completion(exit_status);
            }
        }

        // Update context information using shell's cwd (not parent process)
        self.current_cwd = self.get_shell_cwd();
        self.update_git_info();

        // Query terminal size for chrome setup
        let (cols, rows) = match TerminalGuard::get_size() {
            Ok(size) => size,
            Err(e) => {
                warn!("Failed to get terminal size for Edit mode: {}", e);
                self.mode = Mode::Edit;
                return;
            }
        };

        // Check minimum size and auto-suspend chrome if needed
        let _ = self.chrome.check_minimum_size(cols, rows);

        let coming_from_passthrough = from_mode == Mode::Passthrough;

        if self.chrome.is_active() {
            if coming_from_passthrough {
                // Coming from Passthrough: cursor is at end of command output.
                // Re-establish scroll region using cursor-preserving variant.
                if let Err(e) = self.chrome.setup_scroll_region_preserve_cursor(rows) {
                    warn!("Failed to restore scroll region: {}", e);
                }
            } else {
                // Initial startup: set up scroll region
                if let Err(e) = self.chrome.enter_edit_mode(rows) {
                    warn!("Failed to set up chrome scroll region: {}", e);
                }
            }

            // SINGLE RENDER POINT: Render context bar BEFORE reedline starts
            let timestamp = chrono::Local::now().format("%H:%M").to_string();
            let state = self.topbar_state(&timestamp);

            if let Err(e) = self.chrome.render_context_bar(cols, &state) {
                warn!("Failed to render context bar: {}", e);
            }

            // Only position cursor for initial startup
            if !coming_from_passthrough {
                if let Err(e) = self.chrome.position_cursor_in_scroll_region() {
                    warn!("Failed to position cursor in scroll region: {}", e);
                }
            }
        }

        // Calculate effective rows for PTY (subtract 1 for top bar if chrome active)
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(1)
        } else {
            rows
        };

        // Resize PTY to effective rows
        if let Err(e) = self.pty.resize(cols, effective_rows) {
            warn!("Failed to resize PTY for Edit mode: {}", e);
        }
        debug!(
            cols,
            effective_rows,
            chrome_active = self.chrome.is_active(),
            "PTY resized for Edit mode"
        );

        self.mode = Mode::Edit;
    }

    /// Updates git information for the current working directory.
    ///
    /// Uses a cache to avoid expensive git status queries on every transition.
    /// The cache is valid for a short duration and is invalidated when the
    /// working directory changes.
    pub(super) fn update_git_info(&mut self) {
        let git_info = crate::git::get_git_info_cached(&self.current_cwd, &mut self.git_cache);
        self.git_branch = git_info.branch;
        self.git_dirty = git_info.dirty;
    }

    /// Gets the shell's current working directory via /proc/<pid>/cwd.
    ///
    /// This reads the symlink at /proc/<pid>/cwd to determine the shell's
    /// actual working directory, which may differ from the parent process's
    /// cwd after `cd` commands.
    ///
    /// Falls back to the parent process's cwd if the shell's cwd cannot be read.
    pub(super) fn get_shell_cwd(&self) -> PathBuf {
        if let Some(pid) = self.pty.child_pid() {
            let proc_cwd = format!("/proc/{}/cwd", pid);
            if let Ok(cwd) = std::fs::read_link(&proc_cwd) {
                return cwd;
            }
        }
        // Fallback to parent process cwd
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    }

    /// Transitions to Passthrough mode.
    ///
    /// **Scroll Region**: Re-established in transition_to_injecting() which runs
    /// before this. We re-apply here as well for safety (belt and suspenders).
    ///
    /// **PTY Size**: Keeps PTY at effective_rows (total_rows - 2 when chrome is
    /// active) to match the scroll region. The shell sees the constrained size
    /// and formats output accordingly.
    ///
    /// **Raw Mode**: Ensures terminal raw mode is active so control characters
    /// are forwarded as bytes to the PTY rather than generating signals.
    pub(super) fn transition_to_passthrough(&mut self) -> Result<()> {
        info!(from = ?self.mode, to = ?Mode::Passthrough, "Mode transition");

        // Ensure raw mode is active - reedline may have toggled terminal modes.
        // This is critical for control character passthrough (Ctrl+C -> 0x03, not SIGINT).
        self.terminal_guard
            .ensure_raw_mode()
            .context("Failed to ensure raw mode for Passthrough")?;

        // Get terminal size
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size for Passthrough")?;

        // Safety: ensure scroll region is set before any output flows.
        // This should already be set by transition_to_injecting(), but we re-apply
        // here as a defensive measure in case we enter Passthrough from another path.
        if self.chrome.is_active() {
            if let Err(e) = self.chrome.setup_scroll_region_preserve_cursor(rows) {
                warn!("Failed to ensure scroll region for Passthrough: {}", e);
            }
        }

        // Calculate effective rows (matching Edit mode) to keep output constrained
        // Subtract 1 for top bar when chrome is active
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(1)
        } else {
            rows
        };

        // Keep PTY at effective_rows so command output stays within scroll region.
        self.pty
            .resize(cols, effective_rows)
            .context("Failed to resize PTY for Passthrough")?;
        debug!(
            cols,
            effective_rows, "PTY sized for Passthrough (within scroll region)"
        );

        self.mode = Mode::Passthrough;
        Ok(())
    }

    /// Transitions to Injecting mode.
    ///
    /// **Critical**: Synchronizes PTY size and scroll region before command execution.
    /// The terminal may have been resized while reedline was active in Edit mode.
    /// Since reedline owns SIGWINCH during Edit mode, we sync here to ensure
    /// the PTY has the correct geometry before command execution.
    ///
    /// **Scroll Region**: Reedline/crossterm RESETS the scroll region during Edit mode.
    /// We MUST re-establish it here BEFORE command output flows, otherwise output
    /// will not be constrained to the content area and will overflow to the footer row.
    ///
    /// **PTY Size**: Uses effective_rows (total_rows - 2 when chrome is active)
    /// to match the scroll region. This ensures command output stays constrained
    /// to the content area.
    ///
    /// **Raw Mode**: Ensures terminal raw mode is active. Reedline may toggle
    /// terminal modes during Edit mode, so we must explicitly re-enable raw mode
    /// here to ensure control characters are forwarded correctly to the PTY.
    pub(super) fn transition_to_injecting(&mut self) -> Result<()> {
        info!(from = ?self.mode, to = ?Mode::Injecting, "Mode transition");

        // Record command start time for duration tracking
        self.command_start_time = Some(Instant::now());

        // Ensure raw mode is active - reedline may have toggled terminal modes.
        // This is critical for control character passthrough during command injection.
        self.terminal_guard
            .ensure_raw_mode()
            .context("Failed to ensure raw mode for Injecting")?;

        // Sync PTY size - terminal may have been resized during Edit mode
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size for transition")?;

        // CRITICAL: Re-establish scroll region BEFORE command output flows.
        // Reedline/crossterm RESETS the scroll region during Edit mode.
        // Without this, command output will overflow to the footer row.
        if self.chrome.is_active() {
            if let Err(e) = self.chrome.setup_scroll_region_preserve_cursor(rows) {
                warn!("Failed to re-establish scroll region for Injecting: {}", e);
            }
        }

        // Use effective_rows to keep output within scroll region
        // Subtract 1 for top bar when chrome is active
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(1)
        } else {
            rows
        };

        self.pty
            .resize(cols, effective_rows)
            .context("Failed to resize PTY on transition to Injecting")?;
        debug!(
            cols,
            effective_rows, "PTY size synchronized for command execution"
        );

        self.mode = Mode::Injecting;
        self.injection_start = Some(Instant::now());
        Ok(())
    }

    /// Transitions to Terminating mode.
    pub(super) fn transition_to_terminating(&mut self) {
        info!(from = ?self.mode, to = ?Mode::Terminating, "Mode transition");

        // End intelligence session
        if let Ok(mut store) = self.history_store.lock() {
            store.end_intelligence_session();
        }

        self.mode = Mode::Terminating;
    }

    /// Processes all pending signal events.
    ///
    /// This method should be called regularly from the main event loop
    /// to handle Unix signals that have been delivered to the process.
    ///
    /// # Errors
    ///
    /// Returns an error if any signal handler fails.
    pub(super) fn handle_signals(&mut self) -> Result<()> {
        for event in self.signal_handler.check_signals() {
            match event {
                SignalEvent::WindowResize => self.handle_sigwinch()?,
                SignalEvent::ChildExit => self.handle_sigchld()?,
                SignalEvent::Shutdown => self.transition_to_terminating(),
            }
        }

        Ok(())
    }

    /// Handles terminal window resize signal (SIGWINCH).
    ///
    /// Behavior depends on current mode:
    /// - Edit mode: Let reedline handle SIGWINCH internally via crossterm.
    ///   PTY will be synchronized when transitioning out of Edit mode.
    ///   Chrome scroll region is reapplied and bars are redrawn if active.
    ///   On suspend: clears bars and resets scroll region.
    ///   On resume: sets up scroll region and draws bars.
    /// - Passthrough/Injecting/Initializing: Propagate resize to PTY
    /// - Terminating: Ignored
    ///
    /// # Why Edit Mode Doesn't Resize PTY
    ///
    /// Reedline has its own internal SIGWINCH handler (via crossterm). If the
    /// wrapper also handled the signal, both would attempt to manage terminal
    /// state, causing potential race conditions and corrupted rendering. By
    /// letting reedline own SIGWINCH during Edit mode, we avoid conflicts.
    /// The PTY is synchronized in `transition_to_injecting()` before command
    /// execution.
    pub(super) fn handle_sigwinch(&mut self) -> Result<()> {
        match self.mode {
            Mode::Edit => {
                // Let reedline handle SIGWINCH internally via crossterm.
                // Do NOT resize PTY here - it will be synced on transition.
                // Do NOT emit terminal sequences here - they interfere with reedline's
                // internal repaint cycle. Instead, set a flag to defer chrome updates
                // until the next run_edit() iteration, before reedline takes control.
                debug!("SIGWINCH in Edit mode - deferring chrome update");
                self.pending_resize = true;
            }
            Mode::Initializing => {
                // During initialization, chrome isn't fully active yet (scroll region
                // not set up). Use full terminal size until first PROMPT marker.
                let (cols, rows) =
                    TerminalGuard::get_size().context("Failed to get terminal size for resize")?;
                self.pty
                    .resize(cols, rows)
                    .context("Failed to resize PTY")?;
                info!(cols, rows, mode = ?self.mode, "PTY resized");
            }
            Mode::Passthrough | Mode::Injecting => {
                // We own SIGWINCH in these modes - propagate resize to PTY
                let (cols, rows) =
                    TerminalGuard::get_size().context("Failed to get terminal size for resize")?;

                // Calculate effective rows to match scroll region
                // Subtract 1 for top bar when chrome is active
                let effective_rows = if self.chrome.is_active() {
                    rows.saturating_sub(1)
                } else {
                    rows
                };

                // Resize PTY to effective rows (matching scroll region)
                self.pty
                    .resize(cols, effective_rows)
                    .context("Failed to resize PTY")?;

                // Update chrome for new terminal dimensions
                if self.chrome.is_active() {
                    // Reapply scroll region for new size (preserving cursor)
                    if let Err(e) = self.chrome.setup_scroll_region_preserve_cursor(rows) {
                        warn!("Failed to reapply scroll region on resize: {}", e);
                    }

                    // Redraw context bar for new dimensions
                    let timestamp = chrono::Local::now().format("%H:%M").to_string();
                    let state = self.topbar_state(&timestamp);

                    if let Err(e) = self
                        .chrome
                        .render_context_bar_with_notifications(cols, &state)
                    {
                        warn!("Failed to redraw context bar on resize: {}", e);
                    }
                }

                info!(cols, effective_rows, mode = ?self.mode, "PTY resized");
            }
            Mode::Terminating => {
                // Ignore resize during shutdown
                debug!("SIGWINCH in Terminating mode - ignored");
            }
        }
        Ok(())
    }

    /// Handles child process exit signal (SIGCHLD).
    ///
    /// Checks if the shell has exited and initiates shutdown if so.
    pub(super) fn handle_sigchld(&mut self) -> Result<()> {
        debug!("SIGCHLD handler called");

        if let Some(status) = self.pty.try_wait()? {
            let code = exit_code_from_status(&status);
            info!(exit_code = code, "Child exited (SIGCHLD)");
            self.last_exit_code = code;
            self.transition_to_terminating();
        }

        Ok(())
    }

    /// Returns whether the application should shut down.
    pub(super) fn should_shutdown(&self) -> bool {
        self.signal_handler.should_shutdown()
    }

    /// Toggles chrome display mode.
    ///
    /// Switches between Headless and Full modes with full terminal update.
    /// This can be called from keybinding handlers in future tickets.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    #[allow(dead_code)]
    pub(super) fn toggle_chrome(&mut self) -> Result<()> {
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size for chrome toggle")?;

        self.chrome
            .toggle_with_terminal_update(cols, rows)
            .context("Failed to toggle chrome")?;

        // Render context bar immediately after enabling chrome
        if self.chrome.is_active() {
            let timestamp = chrono::Local::now().format("%H:%M").to_string();
            let state = self.topbar_state(&timestamp);

            if let Err(e) = self
                .chrome
                .render_context_bar_with_notifications(cols, &state)
            {
                warn!("Failed to render context bar after toggle: {}", e);
            }
        }

        // Calculate new effective rows based on chrome state
        // Subtract 1 for top bar when chrome is active
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(1)
        } else {
            rows
        };

        // Resize PTY to new effective rows
        self.pty
            .resize(cols, effective_rows)
            .context("Failed to resize PTY after chrome toggle")?;

        debug!(
            cols,
            effective_rows,
            chrome_active = self.chrome.is_active(),
            "PTY resized after chrome toggle"
        );

        Ok(())
    }
}
