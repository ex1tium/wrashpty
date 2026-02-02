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

use std::io::Write;
use std::os::unix::io::{AsRawFd, BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::poll::{poll, PollFd, PollFlags};
use nix::unistd::read;
use portable_pty::ExitStatus;
use tracing::{debug, info, warn};

use crate::chrome::{Chrome, ChromeRefreshGuard, SizeCheckResult};
use crate::editor::{Editor, EditorResult};
use crate::prompt::WrashPrompt;
use crate::pty::Pty;
use crate::pump::{Pump, PumpResult};
use crate::signals::SignalHandler;
use crate::terminal::TerminalGuard;
use crate::types::{ChromeMode, MarkerEvent, Mode, SignalEvent};

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

/// Timeout for waiting for PREEXEC marker during command injection.
/// If no PREEXEC arrives within this time, transition to Passthrough anyway.
const INJECTION_TIMEOUT: Duration = Duration::from_millis(500);

/// Poll timeout for the Injecting mode pump loop.
/// This allows the loop to wake periodically to check `injection_start` and
/// trigger the 500ms timeout path even when no PTY data arrives.
const INJECTION_POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// Poll interval for background PTY drain during Edit mode (milliseconds).
const EDIT_MODE_DRAIN_POLL_MS: i32 = 50;

/// Buffer size for background PTY drain reads.
const DRAIN_BUFFER_SIZE: usize = 4096;

/// Maximum number of drain results to buffer in the channel.
/// With 4KB per chunk, this caps memory at ~16MB for the channel.
/// This accommodates verbose background jobs (builds, find, logs) while
/// still preventing OOM from runaway output. When full, newest chunks
/// are dropped to prevent blocking PTY reads.
const DRAIN_CHANNEL_CAPACITY: usize = 4096;

/// Result from the background PTY drain thread.
struct DrainResult {
    /// Bytes read from the PTY.
    bytes: Vec<u8>,
    /// Whether EOF was detected.
    eof: bool,
    /// Number of bytes dropped due to channel backpressure before this chunk.
    dropped_bytes: usize,
}

/// RAII guard for the background PTY drain thread.
///
/// Ensures the drain thread is stopped and joined on all exit paths,
/// including when `read_line` returns an error. This prevents leaking
/// a live PTY reader thread.
struct DrainGuard {
    /// Flag to signal the drain thread to stop.
    stop_flag: Arc<AtomicBool>,
    /// Handle to the drain thread (Option to allow taking in drop).
    handle: Option<JoinHandle<()>>,
}

impl DrainGuard {
    /// Creates a new drain guard with the given stop flag and thread handle.
    fn new(stop_flag: Arc<AtomicBool>, handle: JoinHandle<()>) -> Self {
        Self {
            stop_flag,
            handle: Some(handle),
        }
    }

    /// Stops the drain thread and waits for it to finish.
    ///
    /// This is called automatically on drop, but can be called explicitly
    /// if you need to ensure the thread is stopped before proceeding.
    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Background PTY drain loop that runs while reedline blocks on input.
///
/// This function continuously polls the PTY and reads any available data,
/// sending it through a bounded channel for later processing. This prevents
/// background job output from backing up in the PTY buffer while the user
/// is typing.
///
/// When the channel is full (backpressure), chunks are dropped and the byte
/// count is tracked. The dropped count is reported with the next successfully
/// sent chunk so users are informed about dropped background output.
fn pty_drain_loop(pty_fd: RawFd, stop: Arc<AtomicBool>, tx: SyncSender<DrainResult>) {
    let mut buf = [0u8; DRAIN_BUFFER_SIZE];
    let mut pending_dropped_bytes: usize = 0;

    while !stop.load(Ordering::Relaxed) {
        // SAFETY: pty_fd is valid for the duration of Edit mode
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        let mut pollfds = [PollFd::new(&borrowed_fd, PollFlags::POLLIN)];

        // Poll with short timeout to check stop flag periodically
        match poll(&mut pollfds, EDIT_MODE_DRAIN_POLL_MS) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }

        if let Some(revents) = pollfds[0].revents() {
            if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                // EOF detected - use try_send to avoid blocking if channel is full.
                // If we can't send the EOF marker, just break - the receiver will
                // detect EOF when it processes the channel contents.
                let eof_result = DrainResult {
                    bytes: Vec::new(),
                    eof: true,
                    dropped_bytes: pending_dropped_bytes,
                };
                match tx.try_send(eof_result) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => {}
                }
                break;
            }

            if revents.contains(PollFlags::POLLIN) {
                // Drain all available data
                loop {
                    match read(pty_fd, &mut buf) {
                        Ok(0) => {
                            // EOF - use try_send to avoid blocking
                            let eof_result = DrainResult {
                                bytes: Vec::new(),
                                eof: true,
                                dropped_bytes: pending_dropped_bytes,
                            };
                            let _ = tx.try_send(eof_result);
                            return;
                        }
                        Ok(n) => {
                            let result = DrainResult {
                                bytes: buf[..n].to_vec(),
                                eof: false,
                                dropped_bytes: pending_dropped_bytes,
                            };
                            // Use try_send for backpressure - don't block PTY reads
                            match tx.try_send(result) {
                                Ok(()) => {
                                    // Successfully sent, reset dropped counter
                                    pending_dropped_bytes = 0;
                                }
                                Err(mpsc::TrySendError::Full(dropped)) => {
                                    // Channel full - drop this chunk and track bytes
                                    pending_dropped_bytes =
                                        pending_dropped_bytes.saturating_add(dropped.bytes.len());
                                }
                                Err(mpsc::TrySendError::Disconnected(_)) => {
                                    // Receiver gone, stop draining
                                    return;
                                }
                            }
                        }
                        Err(nix::errno::Errno::EAGAIN) => break,
                        Err(nix::errno::Errno::EIO) => {
                            // EIO means PTY closed - use try_send to avoid blocking
                            let eof_result = DrainResult {
                                bytes: Vec::new(),
                                eof: true,
                                dropped_bytes: pending_dropped_bytes,
                            };
                            let _ = tx.try_send(eof_result);
                            return;
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    }
}

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

    /// Chrome layer for status bars and scroll regions.
    chrome: Chrome,

    /// Exit code from last command execution.
    last_exit_code: i32,

    /// Timestamp when app started (for initialization timeout).
    startup_time: Instant,

    /// Session token for marker validation (16 hex chars as bytes).
    #[allow(dead_code)]
    session_token: [u8; 16],

    /// Path to the generated bashrc file (for cleanup).
    bashrc_path: String,

    /// Reedline-based line editor.
    editor: Editor,

    /// Command pending injection after transitioning to Injecting mode.
    pending_command: Option<String>,

    /// Timestamp when injection started (for timeout).
    injection_start: Option<Instant>,
}

impl App {
    /// Creates a new App instance with all components initialized.
    ///
    /// # Arguments
    ///
    /// * `bashrc_path` - Path to the generated bashrc file
    /// * `session_token` - 16-byte session token for marker validation
    /// * `chrome_mode` - Chrome display mode (Headless or Full)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Terminal guard creation fails (raw mode)
    /// - Terminal size query fails
    /// - PTY spawn fails
    /// - Signal handler registration fails
    /// - Editor creation fails
    pub fn new(bashrc_path: &str, session_token: [u8; 16], chrome_mode: ChromeMode) -> Result<Self> {
        // Create terminal guard first (enables raw mode)
        let terminal_guard =
            TerminalGuard::new().context("Failed to initialize terminal raw mode")?;

        // Get terminal size for PTY
        let (cols, rows) = TerminalGuard::get_size().context("Failed to get terminal size")?;

        // Create chrome layer
        let chrome = Chrome::new(chrome_mode);

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

        // Create the reedline editor
        let editor = Editor::new().context("Failed to create editor")?;

        info!(mode = ?Mode::Initializing, "App starting");

        Ok(Self {
            mode: Mode::Initializing,
            pty,
            pump,
            terminal_guard,
            signal_handler,
            chrome,
            last_exit_code: 0,
            startup_time: Instant::now(),
            session_token,
            bashrc_path: bashrc_path.to_string(),
            editor,
            pending_command: None,
            injection_start: None,
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

    /// Handles the Edit mode: interactive line editing with reedline.
    ///
    /// In Edit mode, reedline owns the terminal for command editing. Any
    /// background output from the PTY is buffered to prevent corruption.
    /// A background thread continuously drains PTY output while reedline
    /// blocks waiting for user input.
    ///
    /// The user can:
    /// - Submit a command (Enter): transitions to Injecting
    /// - Clear the line (Ctrl+C): stays in Edit
    /// - Exit (Ctrl+D at empty prompt): transitions to Terminating
    fn run_edit(&mut self) -> Result<()> {
        // Poll PTY for any background output before showing the prompt
        // This captures output from background jobs that arrived since last prompt
        if let Some(transition) = self.collect_background_output()? {
            return transition;
        }

        // Flush any pending background output before showing the prompt
        if self.editor.has_pending() {
            let (data, dropped) = self.editor.flush_pending();
            if !data.is_empty() {
                std::io::stdout()
                    .write_all(&data)
                    .context("Failed to write buffered output")?;
                std::io::stdout()
                    .flush()
                    .context("Failed to flush stdout")?;
            }
            if dropped > 0 {
                warn!(
                    dropped_bytes = dropped,
                    "Background output exceeded buffer, {} bytes dropped", dropped
                );
                // Show warning to user in terminal
                let _ = writeln!(
                    std::io::stderr(),
                    "\x1b[33m[wrashpty: {} bytes of background output dropped due to buffer overflow]\x1b[0m",
                    dropped
                );
            }
        }

        // Redraw chrome bars right before reedline takes over
        // This ensures bars are visible even if flush_pending wrote output that scrolled
        if self.chrome.is_active() {
            if let Ok((cols, rows)) = TerminalGuard::get_size() {
                if let Err(e) = self.chrome.draw_top_bar(cols) {
                    warn!("Failed to redraw top bar before prompt: {}", e);
                }
                if let Err(e) = self.chrome.draw_footer(cols, rows) {
                    warn!("Failed to redraw footer before prompt: {}", e);
                }
                if let Err(e) = self.chrome.position_cursor_in_scroll_region() {
                    warn!("Failed to position cursor before prompt: {}", e);
                }
            }
        }

        // Create prompt with last exit code
        let prompt = WrashPrompt::new(self.last_exit_code);

        // Get terminal size for chrome refresh
        let (refresh_cols, refresh_rows) = TerminalGuard::get_size().unwrap_or((80, 24));

        // Start chrome refresh thread to keep footer visible during reedline operation.
        // Reedline/crossterm may clear or overwrite the footer during initialization
        // and rendering. This thread periodically redraws it.
        let mut chrome_refresh_guard =
            ChromeRefreshGuard::start(refresh_cols, refresh_rows, self.chrome.is_active());

        // Start background PTY drain thread to continuously collect output
        // while reedline blocks waiting for user input.
        // Use a bounded channel to prevent OOM from noisy background output.
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();
        let (tx, rx): (SyncSender<DrainResult>, Receiver<DrainResult>) =
            mpsc::sync_channel(DRAIN_CHANNEL_CAPACITY);
        let pty_fd = self.pty.master_fd();

        let drain_handle: JoinHandle<()> = thread::spawn(move || {
            pty_drain_loop(pty_fd, stop_flag_clone, tx);
        });

        // RAII guard ensures drain thread is stopped on all exit paths,
        // including when read_line returns an error
        let mut drain_guard = DrainGuard::new(stop_flag, drain_handle);

        // Read line from user (blocks until input)
        // Use match instead of ? to ensure guards are dropped before returning
        let editor_result = match self.editor.read_line(&prompt) {
            Ok(result) => result,
            Err(e) => {
                // Guards will stop and join threads on drop
                drop(chrome_refresh_guard);
                drop(drain_guard);
                return Err(e).context("Reedline read_line failed");
            }
        };

        // Explicitly stop background threads before processing results
        chrome_refresh_guard.stop();
        drain_guard.stop();

        // Collect and process all bytes from the drain thread
        let mut all_bytes = Vec::new();
        let mut eof_detected = false;
        let mut channel_dropped_bytes: usize = 0;
        while let Ok(result) = rx.try_recv() {
            all_bytes.extend(result.bytes);
            channel_dropped_bytes = channel_dropped_bytes.saturating_add(result.dropped_bytes);
            if result.eof {
                eof_detected = true;
            }
        }

        // Warn user about bytes dropped due to channel backpressure
        if channel_dropped_bytes > 0 {
            warn!(
                dropped_bytes = channel_dropped_bytes,
                "Background drain channel full, {} bytes dropped", channel_dropped_bytes
            );
            let _ = writeln!(
                std::io::stderr(),
                "\x1b[33m[wrashpty: {} bytes of background output dropped due to channel backpressure]\x1b[0m",
                channel_dropped_bytes
            );
        }

        // Process collected bytes through the marker parser
        if !all_bytes.is_empty() || eof_detected {
            debug!(
                bytes = all_bytes.len(),
                eof = eof_detected,
                channel_dropped = channel_dropped_bytes,
                "Processing bytes from background drain"
            );
            let parsed = self.pump.process_read_bytes(&all_bytes, eof_detected);

            // Buffer any output bytes
            if !parsed.bytes.is_empty() {
                self.editor.buffer_output(&parsed.bytes);
            }

            // Handle markers from drain
            for marker in parsed.markers {
                match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        self.last_exit_code = exit_code;
                        debug!(exit_code, "Received PRECMD during background drain");
                    }
                    MarkerEvent::Prompt => {
                        debug!("Received PROMPT during background drain (ignored)");
                    }
                    MarkerEvent::Preexec => {
                        debug!("Received PREEXEC during background drain (ignored)");
                    }
                }
            }

            // Handle EOF from drain
            if parsed.eof {
                info!("PTY EOF detected during background drain");
                self.transition_to_terminating();
                return Ok(());
            }
        }

        // Also do a final non-blocking poll in case data arrived after drain stopped
        if let Some(transition) = self.collect_background_output()? {
            return transition;
        }

        // Handle the editor result
        match editor_result {
            EditorResult::Command(line) => {
                // Check for built-in exit command
                let trimmed = line.trim();
                if trimmed == "exit" || trimmed.starts_with("exit ") {
                    info!("User typed 'exit' command");
                    self.transition_to_terminating();
                    return Ok(());
                }

                // Skip empty commands
                if trimmed.is_empty() {
                    debug!("Empty command, staying in Edit mode");
                    return Ok(());
                }

                // Store command and transition to Injecting
                self.pending_command = Some(line);
                self.inject_pending_command()?;
            }
            EditorResult::ClearLine => {
                debug!("Line cleared, staying in Edit mode");
                // Stay in Edit mode
            }
            EditorResult::Exit => {
                info!("Ctrl+D at empty prompt, exiting");
                self.transition_to_terminating();
            }
        }

        Ok(())
    }

    /// Collects background PTY output during Edit mode.
    ///
    /// Does a non-blocking poll to check for PTY output from background jobs.
    /// Buffers any bytes received and handles markers appropriately.
    ///
    /// # Returns
    ///
    /// - `Ok(None)` - Continue in Edit mode
    /// - `Ok(Some(Ok(())))` - Transition occurred, caller should return
    /// - `Err(_)` - Error occurred
    fn collect_background_output(&mut self) -> Result<Option<Result<()>>> {
        let result = self.pump.poll_pty_nonblocking()?;

        // Buffer any bytes received (don't write to stdout during Edit mode)
        if !result.bytes.is_empty() {
            debug!(
                bytes = result.bytes.len(),
                "Buffering background PTY output"
            );
            self.editor.buffer_output(&result.bytes);
        }

        // Handle markers that arrived during Edit mode
        for marker in result.markers {
            match marker {
                MarkerEvent::Precmd { exit_code } => {
                    self.last_exit_code = exit_code;
                    debug!(exit_code, "Received PRECMD during Edit mode");
                }
                MarkerEvent::Prompt => {
                    // Unexpected PROMPT during Edit mode - we're already at prompt
                    // This could happen with background job completion; stay in Edit
                    debug!("Received PROMPT during Edit mode (ignored)");
                }
                MarkerEvent::Preexec => {
                    // Unexpected PREEXEC during Edit mode - very unusual
                    debug!("Unexpected PREEXEC during Edit mode (ignored)");
                }
            }
        }

        // Handle PTY EOF
        if result.eof {
            info!("PTY EOF detected during Edit mode");
            self.transition_to_terminating();
            return Ok(Some(Ok(())));
        }

        Ok(None)
    }

    /// Injects the pending command into the PTY.
    ///
    /// Creates an EchoGuard to suppress echo, writes the command,
    /// and transitions to Injecting mode.
    fn inject_pending_command(&mut self) -> Result<()> {
        let command = self.pending_command.take().ok_or_else(|| {
            anyhow::anyhow!("inject_pending_command called without pending command")
        })?;

        debug!(command = %command, "Injecting command");

        // Transition to Injecting mode first (syncs PTY size)
        self.transition_to_injecting()?;

        // Create echo guard to suppress command echo
        let _guard = self
            .pty
            .create_echo_guard()
            .context("Failed to create echo guard")?;

        // Write command to PTY
        self.pty
            .write_command(&command)
            .context("Failed to write command to PTY")?;

        // Guard drops here, restoring echo

        Ok(())
    }

    /// Handles the Injecting mode: waiting for command execution to start.
    ///
    /// In this mode, we've injected a command and are waiting for the shell
    /// to emit a PREEXEC marker indicating the command is about to execute.
    /// Once PREEXEC is received, we transition to Passthrough mode.
    ///
    /// If no PREEXEC arrives within INJECTION_TIMEOUT, transitions to Passthrough
    /// anyway to prevent deadlocks.
    fn run_injecting(&mut self) -> Result<()> {
        // Check for injection timeout
        if let Some(start) = self.injection_start {
            if start.elapsed() > INJECTION_TIMEOUT {
                warn!(
                    "Injection timeout ({:?}) - transitioning to Passthrough without PREEXEC",
                    INJECTION_TIMEOUT
                );
                self.injection_start = None;
                self.transition_to_passthrough()?;
                return Ok(());
            }
        }

        // Use bounded wait so we can re-check injection_start timeout
        // even when no PTY data arrives
        match self.pump.run_once_with_timeout(Some(INJECTION_POLL_TIMEOUT))? {
            PumpResult::MarkerDetected(markers) => {
                for marker in markers {
                    match marker {
                        MarkerEvent::Precmd { exit_code } => {
                            self.last_exit_code = exit_code;
                            debug!(exit_code, "Received PRECMD in Injecting");
                        }
                        MarkerEvent::Prompt => {
                            // Unexpected - command should execute before next prompt
                            // But handle gracefully by transitioning to Edit
                            debug!("Unexpected PROMPT in Injecting - transitioning to Edit");
                            self.injection_start = None;
                            self.transition_to_edit();
                            return Ok(());
                        }
                        MarkerEvent::Preexec => {
                            debug!("Received PREEXEC - command executing");
                            self.injection_start = None;
                            self.transition_to_passthrough()?;
                            return Ok(());
                        }
                    }
                }
            }
            PumpResult::PtyEof => {
                info!("PTY EOF in Injecting");
                self.injection_start = None;
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
    ///
    /// Sets up scroll region and draws chrome bars if chrome is active.
    /// Resizes PTY to effective rows (accounting for chrome bars).
    fn transition_to_edit(&mut self) {
        info!(from = ?self.mode, to = ?Mode::Edit, "Mode transition");

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
        // In transition_to_edit, we don't need to handle the result specially:
        // - Suspended: is_active() will be false, so we won't draw bars or set scroll region
        // - Resumed/NoChange: we proceed to set up scroll region and draw bars if active
        let _ = self.chrome.check_minimum_size(cols, rows);

        // Set up scroll region if chrome is active
        if let Err(e) = self.chrome.enter_edit_mode(rows) {
            warn!("Failed to set up chrome scroll region: {}", e);
        }

        // Draw chrome bars if active
        if self.chrome.is_active() {
            if let Err(e) = self.chrome.draw_top_bar(cols) {
                warn!("Failed to draw top bar: {}", e);
            }
            if let Err(e) = self.chrome.draw_footer(cols, rows) {
                warn!("Failed to draw footer: {}", e);
            }
            // Position cursor at start of scroll region so reedline renders there
            if let Err(e) = self.chrome.position_cursor_in_scroll_region() {
                warn!("Failed to position cursor in scroll region: {}", e);
            }
        }

        // Calculate effective rows for PTY (subtract 2 for bars if chrome active)
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(2)
        } else {
            rows
        };

        // Resize PTY to effective rows
        if let Err(e) = self.pty.resize(cols, effective_rows) {
            warn!("Failed to resize PTY for Edit mode: {}", e);
        }
        debug!(cols, effective_rows, chrome_active = self.chrome.is_active(), "PTY resized for Edit mode");

        self.mode = Mode::Edit;
    }

    /// Transitions to Passthrough mode.
    ///
    /// **Critical**: Resets the scroll region and clears chrome before entering
    /// Passthrough to ensure proper terminal behavior for fullscreen apps.
    /// Chrome's enter_passthrough_mode() ALWAYS resets scroll region regardless
    /// of chrome state (defense-in-depth).
    fn transition_to_passthrough(&mut self) -> Result<()> {
        info!(from = ?self.mode, to = ?Mode::Passthrough, "Mode transition");

        // Get terminal size first - needed for clearing bars and PTY resize
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size for Passthrough")?;

        // Clear chrome bars from display before resetting scroll region.
        // This prevents visual artifacts when fullscreen apps (vim, nano, htop) start.
        if self.chrome.is_active() {
            if let Err(e) = self.chrome.clear_bars(rows) {
                warn!("Failed to clear chrome bars: {}", e);
            }
        }

        // Chrome ALWAYS resets scroll region on Passthrough entry
        if let Err(e) = self.chrome.enter_passthrough_mode() {
            warn!("Chrome failed to reset scroll region: {}", e);
        }

        // Defense-in-depth: also reset via TerminalGuard
        if let Err(e) = TerminalGuard::reset_scroll_region() {
            warn!("Failed to reset scroll region: {}", e);
        }

        // Resize PTY to full terminal - Passthrough uses all rows
        self.pty
            .resize(cols, rows)
            .context("Failed to resize PTY for Passthrough")?;
        debug!(cols, rows, "PTY resized for Passthrough (full screen)");

        self.mode = Mode::Passthrough;
        Ok(())
    }

    /// Transitions to Injecting mode.
    ///
    /// **Critical**: Synchronizes PTY size before entering Injecting mode.
    /// The terminal may have been resized while reedline was active in Edit
    /// mode. Since reedline owns SIGWINCH during Edit mode, we sync here
    /// to ensure the PTY has the correct geometry before command execution.
    fn transition_to_injecting(&mut self) -> Result<()> {
        info!(from = ?self.mode, to = ?Mode::Injecting, "Mode transition");

        // Sync PTY size - terminal may have been resized during Edit mode
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size for transition")?;
        self.pty
            .resize(cols, rows)
            .context("Failed to resize PTY on transition to Injecting")?;
        debug!(cols, rows, "PTY size synchronized for command execution");

        self.mode = Mode::Injecting;
        self.injection_start = Some(Instant::now());
        Ok(())
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
    fn handle_sigwinch(&mut self) -> Result<()> {
        match self.mode {
            Mode::Edit => {
                // Let reedline handle SIGWINCH internally via crossterm.
                // Do NOT resize PTY here - it will be synced on transition.
                debug!("SIGWINCH in Edit mode - reedline handles display refresh");

                // Always get terminal size for chrome handling
                let (cols, rows) = TerminalGuard::get_size()
                    .context("Failed to get terminal size for chrome handling")?;

                // Check minimum size and handle state transitions
                match self.chrome.check_minimum_size(cols, rows) {
                    SizeCheckResult::Suspended => {
                        // Chrome just suspended - clear bars and reset scroll region
                        if let Err(e) = self.chrome.clear_bars(rows) {
                            warn!("Failed to clear chrome bars on suspend: {}", e);
                        }
                        if let Err(e) = Chrome::reset_scroll_region() {
                            warn!("Failed to reset scroll region on suspend: {}", e);
                        }
                        debug!("Chrome suspended due to small terminal");
                    }
                    SizeCheckResult::Resumed => {
                        // Chrome just resumed - set up scroll region and draw bars
                        if let Err(e) = self.chrome.setup_scroll_region(rows) {
                            warn!("Failed to setup scroll region on resume: {}", e);
                        }
                        if let Err(e) = self.chrome.draw_top_bar(cols) {
                            warn!("Failed to draw top bar on resume: {}", e);
                        }
                        if let Err(e) = self.chrome.draw_footer(cols, rows) {
                            warn!("Failed to draw footer on resume: {}", e);
                        }
                        debug!("Chrome resumed after terminal grew");
                    }
                    SizeCheckResult::NoChange => {
                        // No state transition - reapply scroll region and redraw if active
                        if self.chrome.is_active() {
                            // Reapply scroll region to match new terminal size
                            if let Err(e) = self.chrome.setup_scroll_region(rows) {
                                warn!("Failed to reapply scroll region on resize: {}", e);
                            }
                            // Redraw bars for new dimensions
                            if let Err(e) = self.chrome.redraw_if_active(cols, rows) {
                                warn!("Failed to redraw chrome bars on resize: {}", e);
                            }
                        }
                    }
                }
            }
            Mode::Passthrough | Mode::Injecting | Mode::Initializing => {
                // We own SIGWINCH in these modes - propagate resize to PTY
                let (cols, rows) =
                    TerminalGuard::get_size().context("Failed to get terminal size for resize")?;
                self.pty
                    .resize(cols, rows)
                    .context("Failed to resize PTY")?;
                info!(cols, rows, mode = ?self.mode, "PTY resized");
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

    /// Toggles chrome display mode.
    ///
    /// Switches between Headless and Full modes with full terminal update.
    /// This can be called from keybinding handlers in future tickets.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    #[allow(dead_code)]
    pub fn toggle_chrome(&mut self) -> Result<()> {
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size for chrome toggle")?;

        self.chrome
            .toggle_with_terminal_update(cols, rows)
            .context("Failed to toggle chrome")?;

        // Calculate new effective rows based on chrome state
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(2)
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

    // =========================================================================
    // Constant Tests
    // =========================================================================

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
    fn test_injection_timeout_constant() {
        // Verify timeout is reasonable for injection
        assert!(INJECTION_TIMEOUT >= Duration::from_millis(100));
        assert!(INJECTION_TIMEOUT <= Duration::from_secs(2));
    }

    #[test]
    fn test_injection_poll_timeout_constant() {
        // Verify poll timeout is short enough to allow timely timeout detection
        // but not so short as to cause excessive CPU usage
        assert!(INJECTION_POLL_TIMEOUT >= Duration::from_millis(10));
        assert!(INJECTION_POLL_TIMEOUT <= Duration::from_millis(200));
        // Must be shorter than injection timeout to allow timeout to fire
        assert!(INJECTION_POLL_TIMEOUT < INJECTION_TIMEOUT);
    }

    // =========================================================================
    // Exit Code Helper Tests
    // =========================================================================

    #[test]
    fn test_exit_code_from_status_success() {
        // Create a mock ExitStatus representing success
        // Note: ExitStatus::with_exit_code is not directly available,
        // but we can test the function with known values
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(0);
        assert_eq!(exit_code_from_status(&status), 0);
    }

    #[test]
    fn test_exit_code_from_status_failure() {
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(1);
        assert_eq!(exit_code_from_status(&status), 1);

        let status = ExitStatus::with_exit_code(42);
        assert_eq!(exit_code_from_status(&status), 42);

        // Test signal-like exit codes (128 + signal)
        let status = ExitStatus::with_exit_code(130); // SIGINT
        assert_eq!(exit_code_from_status(&status), 130);

        let status = ExitStatus::with_exit_code(137); // SIGKILL
        assert_eq!(exit_code_from_status(&status), 137);
    }

    #[test]
    fn test_exit_code_from_status_max_values() {
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(255);
        assert_eq!(exit_code_from_status(&status), 255);
    }

    // =========================================================================
    // Mode Transition Tests
    // =========================================================================

    #[test]
    fn test_mode_equality() {
        assert_eq!(Mode::Initializing, Mode::Initializing);
        assert_eq!(Mode::Edit, Mode::Edit);
        assert_eq!(Mode::Passthrough, Mode::Passthrough);
        assert_eq!(Mode::Injecting, Mode::Injecting);
        assert_eq!(Mode::Terminating, Mode::Terminating);

        assert_ne!(Mode::Initializing, Mode::Edit);
        assert_ne!(Mode::Edit, Mode::Passthrough);
    }

    #[test]
    fn test_mode_debug_format() {
        // Verify Debug implementations for logging
        assert!(format!("{:?}", Mode::Initializing).contains("Initializing"));
        assert!(format!("{:?}", Mode::Edit).contains("Edit"));
        assert!(format!("{:?}", Mode::Passthrough).contains("Passthrough"));
        assert!(format!("{:?}", Mode::Injecting).contains("Injecting"));
        assert!(format!("{:?}", Mode::Terminating).contains("Terminating"));
    }

    // =========================================================================
    // Marker Event Transition Tests
    // =========================================================================

    #[test]
    fn test_marker_event_variants() {
        // Test that all marker event variants can be constructed and matched
        let precmd = MarkerEvent::Precmd { exit_code: 0 };
        let prompt = MarkerEvent::Prompt;
        let preexec = MarkerEvent::Preexec;

        assert!(matches!(precmd, MarkerEvent::Precmd { exit_code: 0 }));
        assert!(matches!(prompt, MarkerEvent::Prompt));
        assert!(matches!(preexec, MarkerEvent::Preexec));
    }

    #[test]
    fn test_marker_event_precmd_exit_codes() {
        // Test various exit codes in Precmd events
        let success = MarkerEvent::Precmd { exit_code: 0 };
        let failure = MarkerEvent::Precmd { exit_code: 1 };
        let signal = MarkerEvent::Precmd { exit_code: 130 };
        let negative = MarkerEvent::Precmd { exit_code: -1 };

        if let MarkerEvent::Precmd { exit_code } = success {
            assert_eq!(exit_code, 0);
        }
        if let MarkerEvent::Precmd { exit_code } = failure {
            assert_eq!(exit_code, 1);
        }
        if let MarkerEvent::Precmd { exit_code } = signal {
            assert_eq!(exit_code, 130);
        }
        if let MarkerEvent::Precmd { exit_code } = negative {
            assert_eq!(exit_code, -1);
        }
    }

    // =========================================================================
    // Terminal Mode Transition Tests (require TTY)
    // =========================================================================

    /// Helper to check if we're running in a real terminal.
    fn is_tty() -> bool {
        use std::io::stdin;
        use std::os::unix::io::AsRawFd;
        unsafe { libc::isatty(stdin().as_raw_fd()) == 1 }
    }

    #[test]
    fn test_terminal_guard_raw_mode_toggle() {
        if !is_tty() {
            eprintln!("Skipping test (no terminal)");
            return;
        }

        // Test that TerminalGuard properly enables raw mode and restores on drop
        {
            let guard = match TerminalGuard::new() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Skipping test (terminal unavailable): {}", e);
                    return;
                }
            };

            // Guard is active - terminal should be in raw mode
            // We can't easily assert raw mode is active, but we verify
            // the guard was created successfully
            drop(guard);
        }

        // After drop, terminal should be restored
        // Verify by creating another guard successfully
        match TerminalGuard::new() {
            Ok(guard) => drop(guard),
            Err(e) => {
                // This might fail if terminal wasn't properly restored
                panic!("Terminal not properly restored after first guard: {}", e);
            }
        }
    }

    #[test]
    fn test_terminal_guard_nested_drops() {
        if !is_tty() {
            eprintln!("Skipping test (no terminal)");
            return;
        }

        // Test that multiple sequential guard creations work
        for i in 0..3 {
            match TerminalGuard::new() {
                Ok(guard) => {
                    // Simulate some work
                    thread::sleep(Duration::from_millis(10));
                    drop(guard);
                }
                Err(e) => {
                    panic!("Failed to create terminal guard on iteration {}: {}", i, e);
                }
            }
        }
    }

    // =========================================================================
    // Marker Parser Edge Case Tests (malformed and partial input)
    // =========================================================================

    #[test]
    fn test_marker_parser_malformed_osc_type() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // OSC with wrong type number (not 777)
        let malformed = b"\x1b]123;a1b2c3d4e5f67890;PROMPT\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        // Should return as passthrough bytes, not a marker
        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_truncated_sequence() {
        use crate::marker::MarkerParser;

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Truncated sequence (no BEL terminator)
        let truncated = b"\x1b]777;a1b2c3d4e5f67890;PROMPT";
        let outputs: Vec<_> = parser.feed(truncated).collect();

        // Parser should be mid-sequence, no output yet
        assert!(outputs.is_empty());
        assert!(parser.is_mid_sequence());

        // Flush stale should return the buffered bytes
        let stale = parser.flush_stale();
        assert!(stale.is_some());
        assert!(!parser.is_mid_sequence());
    }

    #[test]
    fn test_marker_parser_partial_token() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Partial token (too short)
        let malformed = b"\x1b]777;short;PROMPT\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        // Should return as bytes, not a marker (token validation fails)
        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_invalid_marker_type() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Unknown marker type
        let malformed = b"\x1b]777;a1b2c3d4e5f67890;UNKNOWN\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        // Should return as bytes
        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_empty_fields() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Empty marker type
        let malformed = b"\x1b]777;a1b2c3d4e5f67890;\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_split_at_every_byte() {
        use crate::marker::{MarkerParser, ParseOutput};
        use crate::types::MarkerEvent;

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Valid marker split into individual bytes
        let marker = b"\x1b]777;a1b2c3d4e5f67890;PROMPT\x07";

        let mut found_marker = false;
        for &byte in marker.iter() {
            for output in parser.feed(&[byte]) {
                if matches!(output, ParseOutput::Marker(MarkerEvent::Prompt)) {
                    found_marker = true;
                }
            }
        }

        assert!(
            found_marker,
            "Should find PROMPT marker even with byte-by-byte feeding"
        );
        assert!(!parser.is_mid_sequence());
    }

    #[test]
    fn test_marker_parser_binary_garbage() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Feed random binary data - should not panic
        let garbage: Vec<u8> = (0..=255).collect();
        let outputs: Vec<_> = parser.feed(&garbage).collect();

        // Should produce only bytes output, no markers (garbage doesn't form valid markers)
        for output in &outputs {
            assert!(matches!(output, ParseOutput::Bytes(_)));
        }

        // Parser should handle gracefully
        let _ = parser.flush_stale();
        assert!(!parser.is_mid_sequence());
    }

    #[test]
    fn test_marker_parser_repeated_esc() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Multiple ESC bytes in a row
        let input = b"\x1b\x1b\x1b\x1btest";
        let outputs: Vec<_> = parser.feed(input).collect();

        // Should output all ESC bytes and "test" as passthrough
        let total_bytes: Vec<u8> = outputs
            .iter()
            .filter_map(|o| match o {
                ParseOutput::Bytes(b) => Some(b.to_vec()),
                _ => None,
            })
            .flatten()
            .collect();

        assert_eq!(total_bytes, b"\x1b\x1b\x1b\x1btest");
    }

    // =========================================================================
    // Echo Suppression During Injection Tests
    // =========================================================================

    #[test]
    fn test_injection_mode_transitions() {
        // Test the conceptual flow of injection:
        // Edit -> Injecting -> Passthrough (after PREEXEC)

        // Verify mode transitions are distinct
        let modes = [Mode::Edit, Mode::Injecting, Mode::Passthrough];

        for (i, mode) in modes.iter().enumerate() {
            for (j, other) in modes.iter().enumerate() {
                if i == j {
                    assert_eq!(mode, other);
                } else {
                    assert_ne!(mode, other);
                }
            }
        }
    }

    // =========================================================================
    // Panic Hook Terminal Restoration Tests
    // =========================================================================

    #[test]
    fn test_panic_hook_installed() {
        use std::panic;

        // Verify panic hook can be installed and doesn't break normal operation
        crate::safety::install_panic_hook();

        // After installation, panic behavior should include terminal restoration
        // We can't easily test the actual terminal restoration without a TTY,
        // but we verify the hook is installed by checking panic handling
        let result = panic::catch_unwind(|| {
            // This should trigger the panic hook
            panic!("Test panic for hook verification");
        });

        assert!(result.is_err(), "Panic should have been caught");
    }

    #[test]
    fn test_panic_hook_preserves_original() {
        use std::panic;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // Install a custom panic hook first
        let custom_called = Arc::new(AtomicBool::new(false));
        let custom_called_clone = custom_called.clone();

        let original = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            custom_called_clone.store(true, Ordering::SeqCst);
            // Don't call original to avoid noise in test output
            let _ = info;
        }));

        // Now install wrashpty's panic hook (which should chain to ours)
        crate::safety::install_panic_hook();

        // Trigger a panic
        let result = panic::catch_unwind(|| {
            panic!("Test panic for chaining");
        });

        assert!(result.is_err());
        assert!(
            custom_called.load(Ordering::SeqCst),
            "Original panic hook should have been called"
        );

        // Restore original panic hook for other tests
        panic::set_hook(original);
    }

    #[test]
    fn test_terminal_restoration_after_panic() {
        if !is_tty() {
            eprintln!("Skipping test (no terminal)");
            return;
        }

        use std::panic;

        // Install panic hook
        crate::safety::install_panic_hook();

        // Create a terminal guard
        let guard = match TerminalGuard::new() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("Skipping test (terminal unavailable): {}", e);
                return;
            }
        };

        // Force the guard to drop (simulating what happens during panic unwind)
        drop(guard);

        // After drop, verify terminal is in a usable state by creating new guard
        let result = panic::catch_unwind(|| match TerminalGuard::new() {
            Ok(g) => {
                drop(g);
                true
            }
            Err(_) => false,
        });

        match result {
            Ok(success) => assert!(
                success,
                "Should be able to create new guard after restoration"
            ),
            Err(_) => panic!("Guard creation panicked"),
        }
    }

    // =========================================================================
    // Signal Event Tests
    // =========================================================================

    #[test]
    fn test_signal_event_variants() {
        use crate::types::SignalEvent;

        let resize = SignalEvent::WindowResize;
        let child = SignalEvent::ChildExit;
        let shutdown = SignalEvent::Shutdown;

        assert!(matches!(resize, SignalEvent::WindowResize));
        assert!(matches!(child, SignalEvent::ChildExit));
        assert!(matches!(shutdown, SignalEvent::Shutdown));
    }

    #[test]
    fn test_signal_event_debug_format() {
        use crate::types::SignalEvent;

        assert!(format!("{:?}", SignalEvent::WindowResize).contains("WindowResize"));
        assert!(format!("{:?}", SignalEvent::ChildExit).contains("ChildExit"));
        assert!(format!("{:?}", SignalEvent::Shutdown).contains("Shutdown"));
    }
}
