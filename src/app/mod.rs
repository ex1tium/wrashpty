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
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use portable_pty::ExitStatus;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use tracing::{debug, info, warn};

use crate::chrome::panel::{Panel, PanelResult};
use crate::chrome::tabbed_panel::TabbedPanel;
use crate::chrome::{Chrome, NotificationStyle, SizeCheckResult};
use crate::config::Config;
use crate::editor::{Editor, EditorResult};
use crate::history_store::HistoryStore;
use crate::prompt::WrashPrompt;
use crate::pty::Pty;
use crate::pump::{Pump, PumpResult};
use crate::signals::SignalHandler;
use crate::terminal::TerminalGuard;
use crate::types::{ChromeMode, MarkerEvent, Mode};

mod drain;
mod scroll_view;
mod transitions;

use drain::{DRAIN_CHANNEL_CAPACITY, DrainGuard, DrainResult, pty_drain_loop};

/// Extracts the actual exit code from an ExitStatus.
///
/// Returns the shell's exit code (0-255). On Unix, if the process was
/// terminated by a signal, the code is typically 128 + signal_number.
pub(super) fn exit_code_from_status(status: &ExitStatus) -> i32 {
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

    // Scrollback system
    /// Configuration for scrollback behavior (cached from Config).
    scrollback_config: crate::config::ScrollbackConfig,
    /// Ring buffer storing captured terminal output for scrollback.
    scrollback_buffer: crate::scrollback::ScrollbackBuffer,
    /// State machine for parsing PTY output into lines.
    capture_state: crate::scrollback::CaptureState,
    /// Detector for alternate screen buffer (vim, htop).
    alt_screen_detector: crate::scrollback::AltScreenDetector,
    /// Current scroll state (Live or Scrolled with offset).
    scroll_state: crate::types::ScrollState,
    /// Viewer state for scroll view modes and display settings.
    viewer_state: crate::scrollback::ViewerState,
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
            HistoryStore::new(session_token).context("Failed to create history store")?,
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

        // Initialize scrollback system
        let scrollback_buffer = if config.scrollback.enabled {
            crate::scrollback::ScrollbackBuffer::with_capacity(
                config.scrollback.max_lines,
                config.scrollback.max_line_bytes,
            )
        } else {
            // Create a minimal buffer that won't actually store anything
            crate::scrollback::ScrollbackBuffer::with_capacity(0, 0)
        };
        let capture_state = crate::scrollback::CaptureState::new(cols);
        let alt_screen_detector = crate::scrollback::AltScreenDetector::new();

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
            scrollback_config: config.scrollback,
            scrollback_buffer,
            capture_state,
            alt_screen_detector,
            scroll_state: crate::types::ScrollState::Live,
            viewer_state: crate::scrollback::ViewerState::new(),
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
    /// Handles the Initializing mode: waiting for first PROMPT marker.
    ///
    /// # Marker Batching
    ///
    /// Multiple markers can arrive in a single read batch during shell startup.
    /// We process ALL markers in the batch, handling state transitions correctly.
    fn run_initializing(&mut self) -> Result<()> {
        match self.pump.run_once()? {
            PumpResult::MarkerDetected {
                markers,
                captured_bytes,
            } => {
                // Feed captured bytes to scrollback (during init, captures shell startup)
                self.capture_for_scrollback(&captured_bytes);
                // Process ALL markers in the batch, updating state as we go.
                for marker in markers {
                    match self.mode {
                        Mode::Initializing => {
                            match marker {
                                MarkerEvent::Precmd { exit_code } => {
                                    self.last_exit_code = exit_code;
                                    debug!(exit_code, "Received PRECMD in Initializing");
                                }
                                MarkerEvent::Prompt => {
                                    self.transition_to_edit();
                                    // Continue processing remaining markers in Edit mode context
                                }
                                MarkerEvent::Preexec => {
                                    // Unexpected in Initializing, ignore
                                    debug!("Unexpected PREEXEC in Initializing");
                                }
                            }
                        }
                        Mode::Edit => {
                            // We transitioned mid-batch, handle remaining markers
                            match marker {
                                MarkerEvent::Precmd { exit_code } => {
                                    self.last_exit_code = exit_code;
                                    debug!(
                                        exit_code,
                                        "Received PRECMD in Edit (batched during init)"
                                    );
                                }
                                MarkerEvent::Prompt => {
                                    debug!(
                                        "Received duplicate PROMPT in Edit (batched during init)"
                                    );
                                }
                                MarkerEvent::Preexec => {
                                    warn!("Unexpected PREEXEC in Edit (batched during init)");
                                }
                            }
                        }
                        _ => {
                            // Other modes shouldn't happen here
                            break;
                        }
                    }
                }
            }
            PumpResult::PtyEof => {
                info!("PTY EOF during initialization");
                self.transition_to_terminating();
            }
            PumpResult::Continue { captured_bytes } => {
                // Feed captured bytes to scrollback
                self.capture_for_scrollback(&captured_bytes);
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
    ///
    /// # Marker Batching
    ///
    /// When multiple commands are pasted rapidly, markers from multiple command
    /// cycles can arrive in a single read batch. We process ALL markers in the
    /// batch, handling state transitions correctly to avoid losing markers.
    fn run_passthrough(&mut self) -> Result<()> {
        // Enable stdin interception when scrollback is available and not in alt-screen
        // This allows filtering scroll keys (PgUp/PgDown) before forwarding to PTY
        let should_intercept =
            self.scrollback_config.enabled && !self.alt_screen_detector.is_in_alt_screen();

        if should_intercept != self.pump.is_stdin_intercepted() {
            self.pump.set_stdin_intercept(should_intercept);
        }

        match self.pump.run_once()? {
            PumpResult::MarkerDetected {
                markers,
                captured_bytes,
            } => {
                // Feed captured bytes to scrollback
                self.capture_for_scrollback(&captured_bytes);

                // Process intercepted stdin (if any) for scroll keys
                if should_intercept {
                    let stdin_bytes = self.pump.take_stdin_buffer();
                    if !stdin_bytes.is_empty() {
                        debug!(
                            stdin_len = stdin_bytes.len(),
                            stdin_hex = %format!("{:02x?}", &stdin_bytes),
                            buffer_lines = self.scrollback_buffer.len(),
                            can_scroll = self.can_scroll(),
                            "Processing stdin for scroll (MarkerDetected)"
                        );
                        let to_forward = self.process_stdin_for_scroll(&stdin_bytes);
                        if !to_forward.is_empty() {
                            self.pump.write_to_pty(&to_forward)?;
                        }
                    }
                }

                // Process ALL markers in the batch, updating state as we go.
                // This handles rapid command sequences where markers batch together.
                for marker in markers {
                    match self.mode {
                        Mode::Passthrough => {
                            // Record boundary for navigation before processing
                            self.record_command_boundary(&marker);

                            match marker {
                                MarkerEvent::Precmd { exit_code } => {
                                    self.last_exit_code = exit_code;
                                    debug!(exit_code, "Received PRECMD");
                                }
                                MarkerEvent::Prompt => {
                                    self.transition_to_edit();
                                    // Continue processing remaining markers in Edit mode context
                                }
                                MarkerEvent::Preexec => {
                                    // Already in passthrough, this is expected
                                    debug!("Received PREEXEC (already in Passthrough)");
                                }
                            }
                        }
                        Mode::Edit => {
                            // We transitioned mid-batch, handle remaining markers
                            // These are markers that arrived after PROMPT but before
                            // we processed them (e.g., from rapid pasting)
                            match marker {
                                MarkerEvent::Precmd { exit_code } => {
                                    self.last_exit_code = exit_code;
                                    debug!(exit_code, "Received PRECMD in Edit (batched)");
                                }
                                MarkerEvent::Prompt => {
                                    // Already at prompt, ignore
                                    debug!("Received duplicate PROMPT in Edit (batched)");
                                }
                                MarkerEvent::Preexec => {
                                    // This shouldn't happen - PREEXEC only comes after
                                    // user submits a command from Edit mode
                                    warn!("Unexpected PREEXEC in Edit (batched)");
                                }
                            }
                        }
                        _ => {
                            // Other modes shouldn't happen here
                            break;
                        }
                    }
                }
            }
            PumpResult::PtyEof => {
                info!("PTY EOF in Passthrough");
                self.transition_to_terminating();
            }
            PumpResult::Continue { captured_bytes } => {
                // Feed captured bytes to scrollback
                self.capture_for_scrollback(&captured_bytes);

                // Process intercepted stdin (if any) for scroll keys
                if should_intercept {
                    let stdin_bytes = self.pump.take_stdin_buffer();
                    if !stdin_bytes.is_empty() {
                        debug!(
                            stdin_len = stdin_bytes.len(),
                            stdin_hex = %format!("{:02x?}", &stdin_bytes),
                            buffer_lines = self.scrollback_buffer.len(),
                            can_scroll = self.can_scroll(),
                            "Processing stdin for scroll (Continue)"
                        );
                        let to_forward = self.process_stdin_for_scroll(&stdin_bytes);
                        if !to_forward.is_empty() {
                            self.pump.write_to_pty(&to_forward)?;
                        }
                    }
                }
            }
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
                let state = self.topbar_state(&timestamp);
                if let Err(e) = self
                    .chrome
                    .render_context_bar_with_notifications(cols, &state)
                {
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
                    "scroll_up" | "scroll_down" | "scroll_line_up" | "scroll_line_down" => {
                        // Enter scroll view at the bottom (offset 0) - user can scroll from there
                        if self.can_scroll() {
                            self.scroll_state = crate::types::ScrollState::scrolled_at(0);
                            self.run_scroll_view()?;
                        }
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
        self.terminal_guard
            .ensure_raw_mode()
            .context("Failed to ensure raw mode for panel")?;

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
        let panel_height = preferred
            .min(max_panel_height / 2)
            .max(5)
            .min(max_panel_height);

        // Calculate effective rows and verify it's valid
        let effective_rows = rows.saturating_sub(panel_height);
        if effective_rows == 0 {
            debug!(rows, panel_height, "Cannot open panel: no space for PTY");
            return Ok(PanelResult::Dismiss);
        }

        debug!(
            cols,
            rows, panel_height, effective_rows, preferred, "Entering panel mode"
        );

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
        let (cols, rows) =
            TerminalGuard::get_size().context("Failed to get terminal size after panel")?;

        // Re-establish scroll region for chrome
        if self.chrome.is_active() {
            self.chrome.setup_scroll_region(rows)?;
        }

        // Redraw context bar
        let timestamp = chrono::Local::now().format("%H:%M").to_string();
        let state = self.topbar_state(&timestamp);

        if let Err(e) = self
            .chrome
            .render_context_bar_with_notifications(cols, &state)
        {
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
    ///
    /// # Marker Batching
    ///
    /// When commands fail quickly (e.g., "command not found"), multiple markers
    /// can arrive in a single read: [PREEXEC, PRECMD, PROMPT]. We process ALL
    /// markers in the batch, transitioning state as we go, to avoid losing
    /// the PROMPT marker that would otherwise leave us stuck in Passthrough.
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
            PumpResult::MarkerDetected {
                markers,
                captured_bytes,
            } => {
                // Feed captured bytes to scrollback
                self.capture_for_scrollback(&captured_bytes);
                // Process ALL markers in the batch, updating state as we go.
                // This handles the case where [PREEXEC, PRECMD, PROMPT] arrive
                // together when a command fails quickly.
                for marker in markers {
                    // Record boundary for navigation
                    self.record_command_boundary(&marker);

                    match self.mode {
                        Mode::Injecting => {
                            match marker {
                                MarkerEvent::Precmd { exit_code } => {
                                    self.last_exit_code = exit_code;
                                    debug!(exit_code, "Received PRECMD in Injecting");
                                }
                                MarkerEvent::Prompt => {
                                    // Command completed before we saw PREEXEC - unusual but possible
                                    debug!("Received PROMPT in Injecting - transitioning to Edit");
                                    self.injection_start = None;
                                    self.transition_to_edit();
                                    // Continue processing remaining markers in Edit mode
                                }
                                MarkerEvent::Preexec => {
                                    debug!("Received PREEXEC - command executing");
                                    self.injection_start = None;
                                    self.transition_to_passthrough()?;
                                    // Continue processing remaining markers in Passthrough mode
                                }
                            }
                        }
                        Mode::Passthrough => {
                            // We transitioned mid-batch, handle remaining markers
                            match marker {
                                MarkerEvent::Precmd { exit_code } => {
                                    self.last_exit_code = exit_code;
                                    debug!(exit_code, "Received PRECMD in Passthrough (batched)");
                                }
                                MarkerEvent::Prompt => {
                                    debug!(
                                        "Received PROMPT in Passthrough (batched) - transitioning to Edit"
                                    );
                                    self.transition_to_edit();
                                    // We're now in Edit mode, done with this batch
                                    return Ok(());
                                }
                                MarkerEvent::Preexec => {
                                    debug!("Received PREEXEC in Passthrough (batched)");
                                }
                            }
                        }
                        Mode::Edit => {
                            // Already in Edit, we're done
                            return Ok(());
                        }
                        _ => {
                            // Other modes shouldn't happen here
                            break;
                        }
                    }
                }
            }
            PumpResult::PtyEof => {
                info!("PTY EOF in Injecting");
                self.injection_start = None;
                self.transition_to_terminating();
            }
            PumpResult::Continue { captured_bytes } => {
                // Feed captured bytes to scrollback
                self.capture_for_scrollback(&captured_bytes);
            }
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

    // Scrollback capture and viewing methods are in scroll_view.rs
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
mod tests;
