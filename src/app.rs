//! Application state machine and main event loop.
//!
//! This module orchestrates all Wrashpty functionality by managing the
//! Mode state machine and dispatching events to appropriate handlers.
//!
//! # State Machine
//!
//! The application transitions through these modes:
//!
//! ```text
//! Initializing ──┬──────────────────────────────────┐
//!                │ PROMPT marker                    │ 10s timeout
//!                ▼                                  ▼
//!              Edit ◀───────────────────────── Passthrough
//!                │   PROMPT marker                  ▲
//!                │ command submit                   │ PREEXEC marker
//!                ▼                                  │
//!           Injecting ──────────────────────────────┘
//!                │
//!                ▼
//!           Terminating ──► exit(code)
//! ```
//!
//! # Event Processing
//!
//! The main loop processes:
//! - Pump results (markers, EOF)
//! - Signal events (SIGWINCH, SIGCHLD, shutdown signals)
//!
//! Markers from the pump can arrive in batches (multiple per read), and are
//! processed sequentially to handle transitions correctly.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use std::os::unix::io::AsRawFd;

use portable_pty::ExitStatus;

use crate::pty::Pty;
use crate::pump::{Pump, PumpResult};
use crate::signals::SignalHandler;
use crate::terminal::TerminalGuard;
use crate::types::{MarkerEvent, Mode, SignalEvent};

/// Extracts the actual exit code from an ExitStatus.
///
/// Returns the shell's exit code (0-255). On Unix, if the process was
/// terminated by a signal, the code is typically 128 + signal_number.
fn exit_code_from_status(status: &ExitStatus) -> i32 {
    status.exit_code() as i32
}

/// Timeout for initial prompt detection before falling back to passthrough.
const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for graceful child exit during termination.
const TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Sleep duration for stub edit mode to avoid busy loop.
const EDIT_STUB_DELAY: Duration = Duration::from_millis(100);

/// Main application struct coordinating all Wrashpty components.
///
/// `App` owns the PTY, pump, terminal guard, and signal handler, coordinating
/// them through a state machine that handles mode transitions based on markers
/// and signals.
pub struct App {
    /// Current operational mode.
    mode: Mode,

    /// PTY instance with spawned bash.
    pty: Pty,

    /// Byte pump with marker detection.
    pump: Pump,

    /// RAII terminal state guard.
    terminal_guard: TerminalGuard,

    /// Unix signal handler.
    signal_handler: SignalHandler,

    /// Exit code from last command execution.
    last_exit_code: i32,

    /// Timestamp when app started (for initialization timeout).
    startup_time: Instant,

    /// Session token for marker validation (16 hex chars as bytes).
    #[allow(dead_code)]
    session_token: [u8; 16],

    /// Path to the generated bashrc file (for cleanup).
    bashrc_path: String,
}

impl App {
    /// Creates a new App instance with all components initialized.
    ///
    /// # Arguments
    ///
    /// * `bashrc_path` - Path to the generated bashrc file
    /// * `session_token` - 16-byte session token for marker validation
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Terminal guard creation fails (raw mode)
    /// - Terminal size query fails
    /// - PTY spawn fails
    /// - Signal handler registration fails
    pub fn new(bashrc_path: &str, session_token: [u8; 16]) -> Result<Self> {
        // Create terminal guard first (enables raw mode)
        let terminal_guard =
            TerminalGuard::new().context("Failed to initialize terminal raw mode")?;

        // Get terminal size for PTY
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size")?;

        // Spawn PTY with bash
        let pty = Pty::spawn(bashrc_path, cols, rows).context("Failed to spawn PTY")?;

        // Create signal handler before pump so we can pass its fd
        let signal_handler = SignalHandler::new().context("Failed to initialize signal handler")?;

        // Create pump with PTY master fd, session token, and signal fd for wake-on-signal
        let pump = Pump::new(
            pty.master_fd(),
            session_token,
            Some(signal_handler.as_raw_fd()),
        );

        info!(mode = ?Mode::Initializing, "App starting");

        Ok(Self {
            mode: Mode::Initializing,
            pty,
            pump,
            terminal_guard,
            signal_handler,
            last_exit_code: 0,
            startup_time: Instant::now(),
            session_token,
            bashrc_path: bashrc_path.to_string(),
        })
    }

    /// Runs the main event loop until termination.
    ///
    /// This method processes events (pump results, signals) and dispatches
    /// to mode-specific handlers until the application enters Terminating mode.
    ///
    /// # Returns
    ///
    /// The exit code from the shell (or last executed command).
    ///
    /// # Errors
    ///
    /// Returns an error if any critical operation fails.
    pub fn run(&mut self) -> Result<i32> {
        loop {
            // Process pending signals
            self.handle_signals()?;

            // Check if we should shut down
            if self.should_shutdown() && self.mode != Mode::Terminating {
                self.transition_to_terminating();
            }

            // Dispatch to mode-specific handler
            match self.mode {
                Mode::Initializing => self.run_initializing()?,
                Mode::Edit => self.run_edit()?,
                Mode::Passthrough => self.run_passthrough()?,
                Mode::Injecting => self.run_injecting()?,
                Mode::Terminating => {
                    return self.run_terminating();
                }
            }
        }
    }

    /// Handles the Initializing mode: waiting for first PROMPT marker.
    ///
    /// In this mode, we wait for bash to emit its first PROMPT marker,
    /// indicating it's ready for input. If no marker arrives within the
    /// timeout, we fall back to degraded passthrough mode.
    fn run_initializing(&mut self) -> Result<()> {
        match self.pump.run_once()? {
            PumpResult::MarkerDetected(markers) => {
                for marker in markers {
                    match marker {
                        MarkerEvent::Precmd { exit_code } => {
                            self.last_exit_code = exit_code;
                            debug!(exit_code, "Received PRECMD in Initializing");
                        }
                        MarkerEvent::Prompt => {
                            self.transition_to_edit();
                            return Ok(());
                        }
                        MarkerEvent::Preexec => {
                            // Unexpected in Initializing, ignore
                            debug!("Unexpected PREEXEC in Initializing");
                        }
                    }
                }
            }
            PumpResult::PtyEof => {
                info!("PTY EOF during initialization");
                self.transition_to_terminating();
            }
            PumpResult::Continue => {
                // Check for initialization timeout
                if self.startup_time.elapsed() > INITIALIZATION_TIMEOUT {
                    warn!(
                        "Initialization timeout ({:?}) - entering degraded passthrough mode",
                        INITIALIZATION_TIMEOUT
                    );
                    self.transition_to_passthrough()?;
                }
            }
        }
        Ok(())
    }

    /// Handles the Passthrough mode: transparent I/O with marker detection.
    ///
    /// In this mode, the pump forwards all I/O between the terminal and PTY,
    /// watching for markers. When a PROMPT marker arrives, we transition to
    /// Edit mode.
    fn run_passthrough(&mut self) -> Result<()> {
        match self.pump.run_once()? {
            PumpResult::MarkerDetected(markers) => {
                for marker in markers {
                    match marker {
                        MarkerEvent::Precmd { exit_code } => {
                            self.last_exit_code = exit_code;
                            debug!(exit_code, "Received PRECMD");
                        }
                        MarkerEvent::Prompt => {
                            self.transition_to_edit();
                            return Ok(());
                        }
                        MarkerEvent::Preexec => {
                            // Already in passthrough, this is expected
                            debug!("Received PREEXEC (already in Passthrough)");
                        }
                    }
                }
            }
            PumpResult::PtyEof => {
                info!("PTY EOF in Passthrough");
                self.transition_to_terminating();
            }
            PumpResult::Continue => {}
        }
        Ok(())
    }

    /// Handles the Edit mode: interactive line editing.
    ///
    /// This is a stub implementation that will be replaced with reedline
    /// integration in a future ticket. Currently it just sleeps briefly
    /// and returns to Passthrough.
    ///
    /// # Future Implementation
    ///
    /// When command submission is implemented, this method should:
    /// 1. Accept user input via reedline
    /// 2. On command submit: call `transition_to_injecting()`, inject the command
    ///    into the PTY, then return to let the main loop handle `run_injecting()`
    /// 3. `run_injecting()` waits for PREEXEC marker before transitioning to Passthrough
    fn run_edit(&mut self) -> Result<()> {
        debug!("Edit mode (stub) - auto-transitioning to Passthrough");

        // Stub: sleep briefly to avoid busy loop, then return to passthrough
        thread::sleep(EDIT_STUB_DELAY);

        // TODO: When command submission is implemented, the flow should be:
        //   1. Get command from reedline
        //   2. self.transition_to_injecting();
        //   3. self.pty.write_command(&command)?;
        //   4. return Ok(()) to let main loop call run_injecting()
        //
        // For now, bypass Injecting mode since there's no command to inject.
        self.transition_to_passthrough()?;
        Ok(())
    }

    /// Handles the Injecting mode: waiting for command execution to start.
    ///
    /// In this mode, we've injected a command and are waiting for the shell
    /// to emit a PREEXEC marker indicating the command is about to execute.
    /// Once PREEXEC is received, we transition to Passthrough mode.
    fn run_injecting(&mut self) -> Result<()> {
        match self.pump.run_once()? {
            PumpResult::MarkerDetected(markers) => {
                for marker in markers {
                    match marker {
                        MarkerEvent::Precmd { exit_code } => {
                            self.last_exit_code = exit_code;
                            debug!(exit_code, "Received PRECMD in Injecting");
                        }
                        MarkerEvent::Prompt => {
                            // Unexpected - command should execute before next prompt
                            debug!("Unexpected PROMPT in Injecting");
                        }
                        MarkerEvent::Preexec => {
                            debug!("Received PREEXEC - command executing");
                            self.transition_to_passthrough()?;
                            return Ok(());
                        }
                    }
                }
            }
            PumpResult::PtyEof => {
                info!("PTY EOF in Injecting");
                self.transition_to_terminating();
            }
            PumpResult::Continue => {}
        }
        Ok(())
    }

    /// Handles the Terminating mode: graceful shutdown.
    ///
    /// Attempts to cleanly exit the child process, waiting up to the
    /// termination timeout. Returns the exit code.
    fn run_terminating(&mut self) -> Result<i32> {
        info!("Entering Terminating mode");

        // Check if child is already gone
        if let Some(status) = self.pty.try_wait()? {
            let code = exit_code_from_status(&status);
            info!(exit_code = code, "Child already exited");
            return Ok(code);
        }

        // Try to send exit command
        debug!("Sending 'exit' command to PTY");
        if let Err(e) = self.pty.write_command("exit") {
            warn!("Failed to send exit command: {}", e);
        }

        // Wait for child to exit with timeout
        let deadline = Instant::now() + TERMINATION_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(status) = self.pty.try_wait()? {
                let code = exit_code_from_status(&status);
                info!(exit_code = code, "Child exited during termination");
                return Ok(code);
            }
            thread::sleep(Duration::from_millis(100));
        }

        // Timeout - child didn't exit gracefully
        warn!(
            "Child didn't exit within {:?} - returning last exit code",
            TERMINATION_TIMEOUT
        );

        Ok(self.last_exit_code)
    }

    /// Transitions to Edit mode.
    fn transition_to_edit(&mut self) {
        info!(from = ?self.mode, to = ?Mode::Edit, "Mode transition");
        self.mode = Mode::Edit;
    }

    /// Transitions to Passthrough mode.
    ///
    /// **Critical**: Resets the scroll region before entering Passthrough
    /// to ensure proper terminal behavior.
    fn transition_to_passthrough(&mut self) -> Result<()> {
        info!(from = ?self.mode, to = ?Mode::Passthrough, "Mode transition");

        // Reset scroll region before entering passthrough
        if let Err(e) = TerminalGuard::reset_scroll_region() {
            warn!("Failed to reset scroll region: {}", e);
        }

        self.mode = Mode::Passthrough;
        Ok(())
    }

    /// Transitions to Injecting mode.
    #[allow(dead_code)]
    fn transition_to_injecting(&mut self) {
        info!(from = ?self.mode, to = ?Mode::Injecting, "Mode transition");
        self.mode = Mode::Injecting;
    }

    /// Transitions to Terminating mode.
    fn transition_to_terminating(&mut self) {
        info!(from = ?self.mode, to = ?Mode::Terminating, "Mode transition");
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
    pub fn handle_signals(&mut self) -> Result<()> {
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
    /// - Edit mode: Ignored (reedline handles SIGWINCH internally)
    /// - Passthrough/Injecting/Initializing: Propagate resize to PTY
    /// - Terminating: Ignored
    fn handle_sigwinch(&mut self) -> Result<()> {
        match self.mode {
            Mode::Edit => {
                // Reedline handles SIGWINCH internally
                debug!("SIGWINCH in Edit mode - delegating to reedline (stub)");
            }
            Mode::Passthrough | Mode::Injecting | Mode::Initializing => {
                // Propagate resize to PTY
                let (cols, rows) = TerminalGuard::get_size()
                    .context("Failed to get terminal size for resize")?;
                self.pty
                    .resize(cols, rows)
                    .context("Failed to resize PTY")?;
                info!(cols, rows, "PTY resized");
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
    fn handle_sigchld(&mut self) -> Result<()> {
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
    pub fn should_shutdown(&self) -> bool {
        self.signal_handler.should_shutdown()
    }

    /// Gets the path to the generated bashrc file.
    pub fn bashrc_path(&self) -> &str {
        &self.bashrc_path
    }
}

impl Drop for App {
    fn drop(&mut self) {
        info!("App cleanup");

        // Attempt to remove the generated bashrc file
        if let Err(e) = std::fs::remove_file(&self.bashrc_path) {
            // Not critical if it fails (e.g., already removed)
            debug!("Failed to remove bashrc file: {}", e);
        }

        // Terminal guard will restore terminal state via its Drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialization_timeout_constant() {
        // Verify timeout is reasonable
        assert!(INITIALIZATION_TIMEOUT >= Duration::from_secs(5));
        assert!(INITIALIZATION_TIMEOUT <= Duration::from_secs(30));
    }

    #[test]
    fn test_termination_timeout_constant() {
        // Verify timeout is reasonable
        assert!(TERMINATION_TIMEOUT >= Duration::from_secs(1));
        assert!(TERMINATION_TIMEOUT <= Duration::from_secs(10));
    }

    #[test]
    fn test_edit_stub_delay_constant() {
        // Verify delay is reasonable for a stub
        assert!(EDIT_STUB_DELAY >= Duration::from_millis(10));
        assert!(EDIT_STUB_DELAY <= Duration::from_millis(500));
    }
}
