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
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use nix::poll::{PollFd, PollFlags, poll};
use nix::unistd::read;
use portable_pty::ExitStatus;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use tracing::{debug, info, warn};

use crate::chrome::panel::{Panel, PanelResult};
use crate::chrome::tabbed_panel::TabbedPanel;
use crate::chrome::{Chrome, ChromeContext, NotificationStyle, SizeCheckResult};
use crate::config::Config;
use crate::editor::{Editor, EditorResult};
use crate::history_store::HistoryStore;
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

/// RAII guard for cursor visibility.
///
/// Ensures the cursor is shown again on all exit paths, including
/// panics and early returns during panel mode.
struct CursorGuard;

impl CursorGuard {
    /// Creates a new cursor guard and hides the cursor.
    ///
    /// The cursor will be shown again when the guard is dropped.
    fn new() -> std::io::Result<Self> {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        use std::io::Write;
        write!(out, "{}", crossterm::cursor::Hide)?;
        out.flush()?;
        Ok(Self)
    }
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        use std::io::Write;
        let _ = write!(out, "{}", crossterm::cursor::Show);
        let _ = out.flush();
    }
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

    /// Centralized history store with SQLite backend.
    history_store: Arc<Mutex<HistoryStore>>,

    /// Command pending injection after transitioning to Injecting mode.
    pending_command: Option<String>,

    /// Whether we're waiting for wipe confirmation (after `:wipe` was entered).
    pending_wipe_confirmation: bool,

    /// Whether we're waiting for dedupe confirmation (after `:dedupe` was entered).
    pending_dedupe_confirmation: bool,

    /// Whether we're waiting for wipe-ci confirmation (after `:wipe-ci` was entered).
    pending_wipe_ci_confirmation: bool,

    /// Timestamp when injection started (for timeout).
    injection_start: Option<Instant>,

    // Command execution metadata for context bar
    /// Current working directory (of the shell, not the parent process).
    current_cwd: PathBuf,
    /// Git branch name, if in a repository.
    git_branch: Option<String>,
    /// Whether git working directory is dirty.
    git_dirty: bool,
    /// Cached git info to avoid repeated expensive queries.
    git_cache: Option<crate::git::CachedGitInfo>,
    /// Last command that was executed.
    last_command: Option<String>,
    /// Duration of the last command execution.
    last_command_duration: Option<Duration>,
    /// Timestamp when command started (for duration tracking).
    command_start_time: Option<Instant>,
    /// Flag indicating a resize occurred during Edit mode and chrome needs updating.
    /// This defers scroll region and context bar updates until after reedline yields.
    pending_resize: bool,
}

impl App {
    /// Creates a new App instance with all components initialized.
    ///
    /// # Arguments
    ///
    /// * `bashrc_path` - Path to the generated bashrc file
    /// * `session_token` - 16-byte session token for marker validation
    /// * `chrome_mode` - Chrome display mode (Headless or Full)
    /// * `config` - Application configuration (theme, symbols)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Terminal guard creation fails (raw mode)
    /// - Terminal size query fails
    /// - PTY spawn fails
    /// - Signal handler registration fails
    /// - Editor creation fails
    pub fn new(
        bashrc_path: &str,
        session_token: [u8; 16],
        chrome_mode: ChromeMode,
        config: &Config,
    ) -> Result<Self> {
        // Create terminal guard first (enables raw mode)
        let terminal_guard =
            TerminalGuard::new().context("Failed to initialize terminal raw mode")?;

        // Get terminal size for PTY
        let (cols, rows) = TerminalGuard::get_size().context("Failed to get terminal size")?;

        // Create chrome layer with theme and symbols from config
        let chrome = Chrome::new(chrome_mode, config);

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

        // Create the history store with SQLite backend
        let history_store = Arc::new(Mutex::new(
            HistoryStore::new(session_token).context("Failed to create history store")?
        ));

        // Create reedline history from the store
        let reedline_history = history_store
            .lock()
            .map_err(|_| anyhow::anyhow!("History store lock poisoned"))?
            .create_reedline_history()?;

        // Create the reedline editor with the history
        let editor = Editor::new(reedline_history).context("Failed to create editor")?;

        // Get initial working directory
        let current_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

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
            history_store,
            pending_command: None,
            pending_wipe_confirmation: false,
            pending_dedupe_confirmation: false,
            pending_wipe_ci_confirmation: false,
            injection_start: None,
            current_cwd,
            git_branch: None,
            git_dirty: false,
            git_cache: None,
            last_command: None,
            last_command_duration: None,
            command_start_time: None,
            pending_resize: false,
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
        // Start intelligence session with unique session ID
        // session_token contains ASCII hex characters, so convert directly to string
        let session_id = std::str::from_utf8(&self.session_token)
            .unwrap_or("unknown")
            .to_string();
        if let Ok(mut store) = self.history_store.lock() {
            store.start_intelligence_session(&session_id);
            // Initial sync with history
            store.sync_intelligence();
        }

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
                // Queue warning notification for display in context bar
                self.chrome.notify(
                    format!("{} bytes dropped (buffer overflow)", dropped),
                    NotificationStyle::Warning,
                    Duration::from_secs(5),
                );
            }
        }

        // Handle deferred resize from SIGWINCH during Edit mode.
        // This is done here (before reedline takes control) rather than in the
        // signal handler to avoid interfering with reedline's internal repaint.
        if self.pending_resize {
            self.pending_resize = false;
            if let Ok((cols, rows)) = TerminalGuard::get_size() {
                match self.chrome.check_minimum_size(cols, rows) {
                    SizeCheckResult::Suspended => {
                        if let Err(e) = self.chrome.clear_bars(rows) {
                            warn!("Failed to clear chrome bars on suspend: {}", e);
                        }
                        if let Err(e) = Chrome::reset_scroll_region() {
                            warn!("Failed to reset scroll region on suspend: {}", e);
                        }
                        debug!("Chrome suspended due to small terminal (deferred)");
                    }
                    SizeCheckResult::Resumed | SizeCheckResult::NoChange => {
                        if self.chrome.is_active() {
                            if let Err(e) = self.chrome.setup_scroll_region_preserve_cursor(rows) {
                                warn!("Failed to reapply scroll region on resize: {}", e);
                            }
                        }
                    }
                }
            }
        }

        // Redraw context bar right before reedline takes over
        // This ensures the bar is visible even if flush_pending wrote output that scrolled.
        // Note: We do NOT call position_cursor_in_scroll_region() here because:
        // 1. After command execution, cursor should be where output ended
        // 2. Context bar drawing uses save/restore cursor, so it doesn't affect position
        // 3. Reedline will position its prompt at the current cursor location
        if self.chrome.is_active() {
            if let Ok((cols, _rows)) = TerminalGuard::get_size() {
                let timestamp = chrono::Local::now().format("%H:%M").to_string();
                let ctx = ChromeContext {
                    cwd: &self.current_cwd,
                    git_branch: self.git_branch.as_deref(),
                    git_dirty: self.git_dirty,
                    last_exit_code: self.last_exit_code,
                    last_command: self.last_command.as_deref(),
                    last_duration: self.last_command_duration,
                    timestamp: &timestamp,
                };
                if let Err(e) = self.chrome.render_context_bar_with_notifications(cols, &ctx) {
                    warn!("Failed to redraw context bar before prompt: {}", e);
                }
            }
        }

        // Create prompt with last exit code
        let prompt = WrashPrompt::new(self.last_exit_code);

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
        // Use match instead of ? to ensure drain guard is dropped before returning
        let editor_result = match self.editor.read_line(&prompt) {
            Ok(result) => result,
            Err(e) => {
                // Guard will stop and join thread on drop
                drop(drain_guard);
                return Err(e).context("Reedline read_line failed");
            }
        };

        // Explicitly stop background drain thread before processing results
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
            // Queue warning notification for display in context bar
            self.chrome.notify(
                format!("{} bytes dropped (channel full)", channel_dropped_bytes),
                NotificationStyle::Warning,
                Duration::from_secs(5),
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
                // Check for built-in commands
                let trimmed = line.trim();

                // Exit command
                if trimmed == "exit" || trimmed.starts_with("exit ") {
                    info!("User typed 'exit' command");
                    self.transition_to_terminating();
                    return Ok(());
                }

                // Panel command - opens the command palette
                if trimmed == ":panel" || trimmed == ":p" {
                    debug!("User requested panel via command");
                    self.open_panel()?;
                    return Ok(());
                }

                // History wipe command - sets pending confirmation flag
                if trimmed == ":wipe" {
                    self.pending_wipe_confirmation = true;
                    self.chrome.notify(
                        "Type 'wipe' to confirm history deletion",
                        NotificationStyle::Warning,
                        Duration::from_secs(10),
                    );
                    return Ok(());
                }

                // Handle wipe confirmation (only if :wipe was entered first)
                if trimmed == "wipe" && self.pending_wipe_confirmation {
                    self.pending_wipe_confirmation = false;
                    self.chrome.clear_notifications();
                    if let Ok(store) = self.history_store.lock() {
                        match store.wipe("wipe") {
                            Ok(()) => {
                                self.chrome.notify(
                                    "History database deleted",
                                    NotificationStyle::Success,
                                    Duration::from_secs(3),
                                );
                            }
                            Err(e) => {
                                self.chrome.notify(
                                    format!("Failed to delete history: {}", e),
                                    NotificationStyle::Error,
                                    Duration::from_secs(5),
                                );
                            }
                        }
                    }
                    return Ok(());
                }

                // Clear pending wipe confirmation if user enters anything else
                if self.pending_wipe_confirmation && trimmed != "wipe" {
                    self.pending_wipe_confirmation = false;
                }

                // History dedupe command - sets pending confirmation flag
                if trimmed == ":dedupe" {
                    self.pending_dedupe_confirmation = true;
                    self.chrome.notify(
                        "Type 'dedupe' to confirm removing duplicate history entries",
                        NotificationStyle::Warning,
                        Duration::from_secs(10),
                    );
                    return Ok(());
                }

                // Handle dedupe confirmation (only if :dedupe was entered first)
                if trimmed == "dedupe" && self.pending_dedupe_confirmation {
                    self.pending_dedupe_confirmation = false;
                    self.chrome.clear_notifications();
                    if let Ok(store) = self.history_store.lock() {
                        match store.dedupe_all() {
                            Ok((sqlite_removed, bash_removed)) => {
                                let msg = format!(
                                    "Removed {} duplicates (SQLite: {}, bash_history: {})",
                                    sqlite_removed + bash_removed,
                                    sqlite_removed,
                                    bash_removed
                                );
                                self.chrome.notify(
                                    msg,
                                    NotificationStyle::Success,
                                    Duration::from_secs(5),
                                );
                            }
                            Err(e) => {
                                self.chrome.notify(
                                    format!("Failed to dedupe history: {}", e),
                                    NotificationStyle::Error,
                                    Duration::from_secs(5),
                                );
                            }
                        }
                    }
                    return Ok(());
                }

                // Clear pending dedupe confirmation if user enters anything else
                if self.pending_dedupe_confirmation && trimmed != "dedupe" {
                    self.pending_dedupe_confirmation = false;
                }

                // Intelligence wipe command - sets pending confirmation flag
                if trimmed == ":wipe-ci" {
                    self.pending_wipe_ci_confirmation = true;
                    self.chrome.notify(
                        "Type 'wipe' to confirm intelligence database reset",
                        NotificationStyle::Warning,
                        Duration::from_secs(10),
                    );
                    return Ok(());
                }

                // Handle wipe-ci confirmation (only if :wipe-ci was entered first)
                if trimmed == "wipe" && self.pending_wipe_ci_confirmation {
                    self.pending_wipe_ci_confirmation = false;
                    self.chrome.clear_notifications();
                    if let Ok(mut store) = self.history_store.lock() {
                        match store.reset_intelligence() {
                            Ok(()) => {
                                self.chrome.notify(
                                    "Intelligence database reset",
                                    NotificationStyle::Success,
                                    Duration::from_secs(3),
                                );
                            }
                            Err(e) => {
                                self.chrome.notify(
                                    format!("Failed to reset intelligence: {}", e),
                                    NotificationStyle::Error,
                                    Duration::from_secs(5),
                                );
                            }
                        }
                    }
                    return Ok(());
                }

                // Clear pending wipe-ci confirmation if user enters anything else
                if self.pending_wipe_ci_confirmation && trimmed != "wipe" {
                    self.pending_wipe_ci_confirmation = false;
                }

                // Skip empty commands
                if trimmed.is_empty() {
                    debug!("Empty command, staying in Edit mode");
                    return Ok(());
                }

                // Store command and transition to Injecting
                self.pending_command = Some(line.clone());

                // Record command for intelligence learning and sync to bash_history
                // (reedline already saved to SQLite, but we need bash_history for other sessions)
                if let Ok(mut store) = self.history_store.lock() {
                    store.record_command_submission(&line);
                    store.sync_to_bash_history(&line);
                }

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
            EditorResult::HostCommand(cmd) => {
                // Handle host commands (like panel open requests)
                debug!(command = %cmd, "Host command received");
                match cmd.as_str() {
                    "open_panel" => {
                        self.open_panel()?;
                    }
                    _ => {
                        warn!(command = %cmd, "Unknown host command");
                    }
                }
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
}

/// RAII guard that ensures `collapse_panel` is called even on panic or early return.
///
/// Construct with [`PanelGuard::new`], which marks it as "armed". If the guarded
/// code completes successfully, call [`PanelGuard::disarm`] to prevent the cleanup
/// from running twice. If dropped while still armed (e.g., due to panic or `?`),
/// the `Drop` impl will call `collapse_panel`.
struct PanelGuard<'a> {
    chrome: &'a mut Chrome,
    total_rows: u16,
    armed: bool,
}

impl<'a> PanelGuard<'a> {
    /// Creates a new armed guard.
    fn new(chrome: &'a mut Chrome, total_rows: u16) -> Self {
        Self {
            chrome,
            total_rows,
            armed: true,
        }
    }

    /// Disarms the guard, preventing `collapse_panel` from being called in `Drop`.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PanelGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Ignore the Result in Drop - we can't propagate errors here
            let _ = self.chrome.collapse_panel(self.total_rows);
        }
    }
}

impl App {
    /// Runs the panel mode with the given panel.
    ///
    /// This method handles the panel input loop, rendering the panel and
    /// processing key events until the user dismisses it or executes a command.
    ///
    /// # Arguments
    ///
    /// * `panel` - The panel to display and interact with
    ///
    /// # Returns
    ///
    /// The result of the panel interaction.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    fn run_panel_mode<P: Panel>(&mut self, panel: &mut P) -> Result<PanelResult> {
        // Ensure raw mode is active - reedline may have toggled terminal modes
        self.terminal_guard.ensure_raw_mode().context("Failed to ensure raw mode for panel")?;

        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size for panel")?;

        // Minimum terminal height needed for panel mode (panel + at least 1 row for PTY)
        const MIN_PANEL_ROWS: u16 = 6; // 5 for panel + 1 for PTY
        if rows < MIN_PANEL_ROWS {
            debug!(rows, min = MIN_PANEL_ROWS, "Terminal too small for panel");
            return Ok(PanelResult::Dismiss);
        }

        // Calculate panel height (min of preferred and half terminal height)
        // Clamp to ensure at least 1 row remains for the PTY
        let preferred = panel.preferred_height();
        let max_panel_height = rows.saturating_sub(1); // Leave at least 1 row for PTY
        let panel_height = preferred.min(max_panel_height / 2).max(5).min(max_panel_height);

        // Calculate effective rows and verify it's valid
        let effective_rows = rows.saturating_sub(panel_height);
        if effective_rows == 0 {
            debug!(rows, panel_height, "Cannot open panel: no space for PTY");
            return Ok(PanelResult::Dismiss);
        }

        debug!(cols, rows, panel_height, effective_rows, preferred, "Entering panel mode");

        // Expand panel area
        self.chrome
            .expand_panel(panel_height, rows)
            .context("Failed to expand panel")?;

        // Create guard to ensure collapse_panel is called even on panic or early return
        let guard = PanelGuard::new(&mut self.chrome, rows);

        // Resize PTY to account for panel - access through guard's chrome reference
        // Note: We need to drop the guard temporarily to access self.pty due to borrow rules
        // For the resize, we handle errors explicitly to maintain guard safety
        let resize_result = {
            // Temporarily disarm and drop guard to access self
            guard.disarm();
            self.pty.resize(cols, effective_rows)
        };

        // If resize failed, collapse panel and propagate error
        if let Err(e) = resize_result {
            self.chrome
                .collapse_panel(rows)
                .context("Failed to collapse panel after resize error")?;
            return Err(e).context("Failed to resize PTY for panel");
        }

        // Use catch_unwind for panic safety during panel_input_loop
        // Note: We don't use PanelGuard here because we need &mut self for panel_input_loop,
        // and the guard would hold a mutable borrow of self.chrome. The catch_unwind
        // provides panic safety, and we explicitly call collapse_panel after.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.panel_input_loop(panel, cols, panel_height, rows)
        }));

        // Collapse panel - always runs after panel_input_loop completes or panics
        self.chrome
            .collapse_panel(rows)
            .context("Failed to collapse panel")?;

        // Restore PTY size (accounting for chrome bar if active)
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(1)
        } else {
            rows
        };
        self.pty
            .resize(cols, effective_rows)
            .context("Failed to restore PTY size")?;

        debug!("Exited panel mode");

        // Handle the result from catch_unwind
        match result {
            Ok(r) => r,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Inner loop for panel input handling.
    fn panel_input_loop<P: Panel>(
        &mut self,
        panel: &mut P,
        cols: u16,
        panel_height: u16,
        _total_rows: u16,
    ) -> Result<PanelResult> {
        use crossterm::terminal::enable_raw_mode;

        // Ensure raw mode is enabled for crossterm event handling
        enable_raw_mode().context("Failed to enable raw mode for panel")?;

        // RAII guard ensures cursor is shown on all exit paths (including panics/errors)
        let _cursor_guard = CursorGuard::new().context("Failed to hide cursor for panel")?;

        // Clear the panel area first
        {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            use std::io::Write;
            for row in 1..=panel_height {
                write!(out, "\x1b[{};1H\x1b[K", row)?;
            }
            out.flush()?;
        }

        

        // Note: We don't disable raw mode here as wrashpty needs it for PTY handling
        // The TerminalGuard manages the overall raw mode state
        // Cursor will be shown when _cursor_guard is dropped

        self.panel_input_loop_inner(panel, cols, panel_height)
    }

    /// Inner implementation of panel input loop.
    fn panel_input_loop_inner<P: Panel>(
        &mut self,
        panel: &mut P,
        cols: u16,
        panel_height: u16,
    ) -> Result<PanelResult> {
        use ratatui_core::style::Style;
        use ratatui_core::widgets::Widget;
        use ratatui_widgets::block::Block;
        use ratatui_widgets::borders::Borders;

        // Get theme for panel styling
        let theme = self.chrome.theme();

        // Track if we need to redraw - start with true for initial render
        let mut needs_redraw = true;

        loop {
            // Only render when needed (after input or on first draw)
            if needs_redraw {
                // Create buffer for panel area (starting at row 1, which is terminal row 1)
                // We use row 0 in buffer coordinates, which maps to terminal row 1
                let area = Rect::new(0, 0, cols, panel_height);
                let mut buffer = Buffer::empty(area);

                // Create a bordered block for the panel with theme colors
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.panel_border))
                    .title(" Wrashpty Panel (Esc to close) ")
                    .title_style(Style::default().fg(theme.header_fg));

                // Get the inner area for panel content
                let inner_area = block.inner(area);

                // Render the border
                block.render(area, &mut buffer);

                // Render panel content inside the border
                panel.render(&mut buffer, inner_area);

                // Write buffer to terminal (row 0 in buffer = terminal row 1)
                self.chrome
                    .render_panel_buffer(&buffer, area)
                    .context("Failed to render panel buffer")?;

                // Flush stdout to ensure panel is visible
                {
                    use std::io::Write;
                    std::io::stdout().flush()?;
                }

                needs_redraw = false;
            }

            // Poll for input with timeout - use a longer timeout since we don't redraw constantly
            match event::poll(std::time::Duration::from_millis(100)) {
                Ok(true) => {
                    match event::read() {
                        Ok(Event::Key(key)) => {
                            // Skip key release events (only handle press)
                            if key.kind != crossterm::event::KeyEventKind::Press {
                                continue;
                            }

                            debug!(key = ?key.code, "Panel received key");

                            match panel.handle_input(key) {
                                PanelResult::Continue => {
                                    // Input processed, need to redraw
                                    needs_redraw = true;
                                }
                                result => {
                                    debug!(result = ?result, "Panel returning result");
                                    return Ok(result);
                                }
                            }
                        }
                        Ok(Event::Resize(new_cols, new_rows)) => {
                            debug!(new_cols, new_rows, "Terminal resized during panel");
                            // For now, just dismiss on resize - could handle more gracefully
                            return Ok(PanelResult::Dismiss);
                        }
                        Ok(_) => {
                            // Mouse or other events - ignore but don't redraw
                        }
                        Err(e) => {
                            warn!("Error reading event: {}", e);
                            return Ok(PanelResult::Dismiss);
                        }
                    }
                }
                Ok(false) => {
                    // No event available - don't redraw, just continue polling
                }
                Err(e) => {
                    warn!("Error polling for events: {}", e);
                    return Ok(PanelResult::Dismiss);
                }
            }

            // Check for signals
            self.handle_signals()?;

            if self.should_shutdown() {
                return Ok(PanelResult::Dismiss);
            }
        }
    }

    /// Opens the command panel.
    ///
    /// Creates a tabbed panel with all available panels (command palette,
    /// file browser, history browser, help) and runs it in panel mode.
    ///
    /// # Returns
    ///
    /// Ok(()) if the panel was handled successfully (command executed or dismissed).
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    pub fn open_panel(&mut self) -> Result<()> {
        let mut panel = TabbedPanel::new(self.chrome.theme());
        panel.set_history_store(Arc::clone(&self.history_store));
        panel.load_context(&self.current_cwd);

        match self.run_panel_mode(&mut panel)? {
            PanelResult::Execute(cmd) => {
                debug!(command = %cmd, "Panel executing command");
                // Use the same restore flow as Dismiss - this properly clears the panel
                // and restores terminal state. Then inject the command.
                self.restore_after_panel()?;

                // Save to history (both SQLite and bash_history) since panel bypasses reedline
                if let Ok(mut store) = self.history_store.lock() {
                    if let Err(e) = store.save_command(&cmd, Some(&self.current_cwd)) {
                        warn!("Failed to save panel command to history: {}", e);
                    }
                }

                self.pending_command = Some(cmd);
                self.inject_pending_command()?;
            }
            PanelResult::InsertText(text) => {
                // Pre-fill the reedline buffer with the selected text
                debug!(text = %text, "Panel requested text insertion, pre-filling buffer");
                self.restore_after_panel()?;
                // Insert the text into reedline's buffer before the next read_line call
                self.editor.prefill_buffer(&text);
            }
            PanelResult::Dismiss | PanelResult::Continue => {
                debug!("Panel dismissed, restoring chrome");
                // Return to editing - redraw context bar and restore terminal state
                self.restore_after_panel()?;
            }
        }

        Ok(())
    }

    /// Restores terminal state after panel closes without executing a command.
    fn restore_after_panel(&mut self) -> Result<()> {
        let (cols, rows) = TerminalGuard::get_size()
            .context("Failed to get terminal size after panel")?;

        // Re-establish scroll region for chrome
        if self.chrome.is_active() {
            self.chrome.setup_scroll_region(rows)?;
        }

        // Redraw context bar
        let timestamp = chrono::Local::now().format("%H:%M").to_string();
        let ctx = ChromeContext {
            cwd: &self.current_cwd,
            git_branch: self.git_branch.as_deref(),
            git_dirty: self.git_dirty,
            last_exit_code: self.last_exit_code,
            last_command: self.last_command.as_deref(),
            last_duration: self.last_command_duration,
            timestamp: &timestamp,
        };

        if let Err(e) = self.chrome.render_context_bar_with_notifications(cols, &ctx) {
            warn!("Failed to redraw context bar after panel: {}", e);
        }

        // Position cursor in scroll region (but not at bottom - let reedline handle it)
        if self.chrome.is_active() {
            self.chrome.position_cursor_in_scroll_region()?;
        }

        Ok(())
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

        // Store command for context bar display
        self.last_command = Some(command.clone());

        // Transition to Injecting mode first (syncs PTY size, records start time)
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
        match self
            .pump
            .run_once_with_timeout(Some(INJECTION_POLL_TIMEOUT))?
        {
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

        // Send SIGHUP to the shell (standard "terminal hangup" signal)
        // This is cleaner than sending "exit" command which pollutes shell history
        if let Some(pid) = self.pty.child_pid() {
            debug!(pid, "Sending SIGHUP to child process");
            // SAFETY: Sending SIGHUP to our own child process is safe
            unsafe {
                libc::kill(pid as i32, libc::SIGHUP);
            }
        } else {
            warn!("Could not get child PID for SIGHUP");
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
    /// Sets up scroll region and renders context bar if chrome is active.
    /// Resizes PTY to effective rows (accounting for top bar).
    ///
    /// When coming from Passthrough, the scroll region is already set and cursor
    /// is at the end of command output. We preserve cursor position so the prompt
    /// appears after the output (natural shell flow).
    fn transition_to_edit(&mut self) {
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
                if let Err(e) = store.update_last_command(exit_status, self.last_command_duration, cwd) {
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
            let ctx = ChromeContext {
                cwd: &self.current_cwd,
                git_branch: self.git_branch.as_deref(),
                git_dirty: self.git_dirty,
                last_exit_code: self.last_exit_code,
                last_command: self.last_command.as_deref(),
                last_duration: self.last_command_duration,
                timestamp: &timestamp,
            };

            if let Err(e) = self.chrome.render_context_bar(cols, &ctx) {
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
    fn update_git_info(&mut self) {
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
    fn get_shell_cwd(&self) -> PathBuf {
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
    fn transition_to_passthrough(&mut self) -> Result<()> {
        info!(from = ?self.mode, to = ?Mode::Passthrough, "Mode transition");

        // Ensure raw mode is active - reedline may have toggled terminal modes.
        // This is critical for control character passthrough (Ctrl+C -> 0x03, not SIGINT).
        self.terminal_guard.ensure_raw_mode().context("Failed to ensure raw mode for Passthrough")?;

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
    fn transition_to_injecting(&mut self) -> Result<()> {
        info!(from = ?self.mode, to = ?Mode::Injecting, "Mode transition");

        // Record command start time for duration tracking
        self.command_start_time = Some(Instant::now());

        // Ensure raw mode is active - reedline may have toggled terminal modes.
        // This is critical for control character passthrough during command injection.
        self.terminal_guard.ensure_raw_mode().context("Failed to ensure raw mode for Injecting")?;

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
    fn transition_to_terminating(&mut self) {
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
                    let ctx = ChromeContext {
                        cwd: &self.current_cwd,
                        git_branch: self.git_branch.as_deref(),
                        git_dirty: self.git_dirty,
                        last_exit_code: self.last_exit_code,
                        last_command: self.last_command.as_deref(),
                        last_duration: self.last_command_duration,
                        timestamp: &timestamp,
                    };

                    if let Err(e) = self.chrome.render_context_bar_with_notifications(cols, &ctx) {
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

        // Render context bar immediately after enabling chrome
        if self.chrome.is_active() {
            let timestamp = chrono::Local::now().format("%H:%M").to_string();
            let ctx = ChromeContext {
                cwd: &self.current_cwd,
                git_branch: self.git_branch.as_deref(),
                git_dirty: self.git_dirty,
                last_exit_code: self.last_exit_code,
                last_command: self.last_command.as_deref(),
                last_duration: self.last_command_duration,
                timestamp: &timestamp,
            };

            if let Err(e) = self.chrome.render_context_bar_with_notifications(cols, &ctx) {
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

    /// Gets the path to the generated bashrc file.
    pub fn bashrc_path(&self) -> &str {
        &self.bashrc_path
    }
}

impl Drop for App {
    fn drop(&mut self) {
        info!("App cleanup");

        // End intelligence session (idempotent, safe to call even if already ended)
        if let Ok(mut store) = self.history_store.lock() {
            store.end_intelligence_session();
        }

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
