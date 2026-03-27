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
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, MouseButton, MouseEventKind};
use portable_pty::ExitStatus;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use tracing::{debug, info, warn};

use crate::chrome::panel::{Panel, PanelResult};
use crate::chrome::settings_view::SettingAction;
use crate::chrome::tabbed_panel::TabbedPanel;
use crate::chrome::{Chrome, NotificationStyle};
use crate::config::Config;
use crate::editor::{Editor, EditorResult};
use crate::history_store::HistoryStore;
use crate::prompt::WrashPrompt;
use crate::pty::{EchoGuard, Pty};
use crate::pump::{Pump, PumpResult};
use crate::signals::SignalHandler;
use crate::terminal::TerminalGuard;
use crate::types::{ChromeMode, MarkerEvent, Mode};

pub mod commands;
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

/// Creates a secure raw-capture file when `WRASHPTY_CAPTURE_RAW` is enabled.
fn create_raw_capture_file() -> Result<(std::fs::File, PathBuf)> {
    let temp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();

    for attempt in 0..16 {
        let path = temp_dir.join(format!("wrashpty-raw-{pid}-{seed}-{attempt}.bin"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("Failed to create raw capture file at {}", path.display())
                });
            }
        }
    }

    anyhow::bail!(
        "Failed to create unique raw capture file in {}",
        temp_dir.display()
    );
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

/// RAII guard that exits the alternate screen buffer on drop.
///
/// Ensures the main screen is restored even on error, panic, or early return
/// from the panel input loop while in fullscreen mode.
struct AltScreenGuard {
    active: bool,
}

impl AltScreenGuard {
    fn new() -> Self {
        Self { active: false }
    }
}

impl Drop for AltScreenGuard {
    fn drop(&mut self) {
        if self.active {
            let _ =
                crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
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
    session_token: [u8; 16],

    /// Path to the generated bashrc file (for cleanup).
    bashrc_path: String,

    /// Reedline-based line editor.
    editor: Editor,

    /// Centralized history store with SQLite backend.
    history_store: Arc<Mutex<HistoryStore>>,

    /// Command pending injection after transitioning to Injecting mode.
    pending_command: Option<String>,

    /// Built-in colon command registry and dispatcher.
    command_registry: commands::CommandRegistry,

    /// Timestamp when injection started (for timeout).
    injection_start: Option<Instant>,
    /// Echo suppression guard held during Injecting mode to prevent duplicated
    /// command echos from being captured into scrollback.
    injection_echo_guard: Option<EchoGuard>,

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
    /// Optional raw byte dump file for debugging capture issues.
    /// Enabled by setting WRASHPTY_CAPTURE_RAW=1 environment variable.
    raw_capture_fd: Option<std::fs::File>,
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
        let raw_capture_fd = if std::env::var_os("WRASHPTY_CAPTURE_RAW").is_some() {
            let (raw_capture_fd, raw_capture_path) =
                create_raw_capture_file().context("Failed to enable raw capture file creation")?;
            info!(
                raw_capture_path = %raw_capture_path.display(),
                "Raw capture enabled"
            );
            Some(raw_capture_fd)
        } else {
            None
        };

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
            command_registry: commands::CommandRegistry::new(),
            injection_start: None,
            injection_echo_guard: None,
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
            raw_capture_fd,
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

            // Restore persisted glyph tier preference
            if let Ok(Some(tier_str)) = store.get_setting("glyph_tier") {
                if let Some(tier) = crate::chrome::glyphs::GlyphTier::try_from_label(&tier_str) {
                    self.chrome.set_glyph_tier(tier);
                }
            }

            // Restore persisted theme preference
            if let Ok(Some(theme_str)) = store.get_setting("theme") {
                if let Some(preset) = crate::config::ThemePreset::from_label(&theme_str) {
                    let theme = crate::chrome::theme::Theme::for_preset(preset);
                    self.chrome.set_theme(theme);
                }
            }
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
                            buffer_lines = self.scrollback_buffer.len(),
                            is_scroll_allowed = self.is_scroll_allowed(),
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
                            buffer_lines = self.scrollback_buffer.len(),
                            is_scroll_allowed = self.is_scroll_allowed(),
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
                self.sync_chrome_for_terminal_size(cols, rows, true);
            }
        }

        self.command_registry
            .expire_pending_confirmation(&mut self.chrome);

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
            // Keep scrollback capture in sync with output drained during Edit mode.
            self.capture_for_scrollback(&all_bytes);
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
                let trimmed = line.trim();

                // Exit command (not a colon command — shell built-in)
                if trimmed == "exit" || trimmed.starts_with("exit ") {
                    info!("User typed 'exit' command");
                    self.transition_to_terminating();
                    return Ok(());
                }

                // Dispatch through the command registry
                if let Some(action) =
                    self.command_registry
                        .dispatch(trimmed, &mut self.chrome, &self.history_store)
                {
                    match action {
                        commands::CommandAction::Handled => {
                            // Immediate re-render so the user sees the effect
                            // (notification or updated glyphs) without waiting
                            // for the next prompt cycle.
                            self.render_context_bar_now();
                            return Ok(());
                        }
                        commands::CommandAction::OpenPanel => {
                            self.open_panel()?;
                            return Ok(());
                        }
                        commands::CommandAction::OpenSettingsHelp => {
                            self.open_panel_settings_help()?;
                            return Ok(());
                        }
                    }
                }

                // Clear any pending confirmation if user typed something else
                self.command_registry.clear_pending();

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
                        if self.is_scroll_allowed() {
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
            // Keep scrollback capture in sync with non-blocking background output.
            self.capture_for_scrollback(&result.bytes);
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
            // Use current terminal size (may have changed during panel mode)
            let total_rows = TerminalGuard::get_size()
                .map(|(_, rows)| rows)
                .unwrap_or(self.total_rows);
            // Ignore the Result in Drop - we can't propagate errors here
            let _ = self.chrome.collapse_panel(total_rows);
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

        // Minimum terminal height needed for panel mode (panel + at least 2 rows for PTY)
        const MIN_PANEL_ROWS: u16 = 10;
        if rows < MIN_PANEL_ROWS {
            debug!(rows, min = MIN_PANEL_ROWS, "Terminal too small for panel");
            return Ok(PanelResult::Dismiss);
        }

        // Calculate panel height: use full preferred height, bounded by terminal size.
        // The panel occupies the top of the screen while the PTY is resized to fit below.
        // Leave at least 2 rows for the PTY so the shell prompt remains visible.
        let preferred = panel.preferred_height();
        let max_panel_height = rows.saturating_sub(2);
        let panel_height = preferred.min(max_panel_height).max(8);

        // Calculate effective rows for PTY
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

        // Flush capture state so scrollback buffer has all content up to this point
        if let Some(captured) = self.capture_state.flush() {
            self.apply_captured_line(captured);
        }

        // Remember how many visible rows the PTY had before panel (for scrollback repaint)
        let pre_panel_effective_rows = rows.saturating_sub(1) as usize; // minus chrome bar

        // Use catch_unwind for panic safety during panel_input_loop
        // Note: We don't use PanelGuard here because we need &mut self for panel_input_loop,
        // and the guard would hold a mutable borrow of self.chrome. The catch_unwind
        // provides panic safety, and we explicitly call collapse_panel after.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.panel_input_loop(panel, cols, panel_height, rows)
        }));

        // Get current terminal size (may have changed during panel mode due to resize)
        let (current_cols, current_rows) = TerminalGuard::get_size().unwrap_or((cols, rows));

        // Collapse panel - always runs after panel_input_loop completes or panics
        self.chrome
            .collapse_panel(current_rows)
            .context("Failed to collapse panel")?;

        // Repaint scrollback content into the freed rows to restore previous display
        self.repaint_after_panel(panel_height, pre_panel_effective_rows);

        // Restore PTY size (accounting for chrome bar if active)
        let effective_rows = if self.chrome.is_active() {
            current_rows.saturating_sub(1)
        } else {
            current_rows
        };
        self.pty
            .resize(current_cols, effective_rows)
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
        mut cols: u16,
        mut panel_height: u16,
        _total_rows: u16,
    ) -> Result<PanelResult> {
        // RAII guard ensures cursor is shown on all exit paths (including panics/errors)
        let _cursor_guard = CursorGuard::new().context("Failed to hide cursor for panel")?;

        // Clear the panel area first
        {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            use crossterm::cursor::MoveTo;
            use crossterm::terminal::{Clear, ClearType};
            for row in 1..=panel_height {
                crossterm::queue!(out, MoveTo(0, row - 1), Clear(ClearType::UntilNewLine))?;
            }
            out.flush()?;
        }

        // Note: We don't disable raw mode here as wrashpty needs it for PTY handling
        // The TerminalGuard manages the overall raw mode state
        // Cursor will be shown when _cursor_guard is dropped

        self.panel_input_loop_inner(panel, &mut cols, &mut panel_height)
    }

    /// Inner implementation of panel input loop.
    ///
    /// `cols` and `panel_height` are mutable so the resize handler can update them.
    fn panel_input_loop_inner<P: Panel>(
        &mut self,
        panel: &mut P,
        cols: &mut u16,
        panel_height: &mut u16,
    ) -> Result<PanelResult> {
        use ratatui_core::style::Style;
        use ratatui_core::widgets::Widget;
        use ratatui_widgets::block::Block;
        use ratatui_widgets::borders::Borders;

        // Track if we need to redraw - start with true for initial render
        let mut needs_redraw = true;

        // Fullscreen toggle state (F10).
        // AltScreenGuard ensures LeaveAlternateScreen on all exit paths
        // (error, panic, early return) so the main screen is always restored.
        let mut fullscreen = false;
        let mut normal_panel_height: u16 = *panel_height;
        let mut _alt_screen = AltScreenGuard::new();
        let mut left_mouse_selecting = false;

        loop {
            // Only render when needed (after input or on first draw)
            if needs_redraw {
                // Create buffer for panel area (starting at row 1, which is terminal row 1)
                // We use row 0 in buffer coordinates, which maps to terminal row 1
                let area = Rect::new(0, 0, *cols, *panel_height);
                let mut buffer = Buffer::empty(area);

                // Re-read theme from panel each frame so runtime theme
                // changes (via Settings) are reflected in the outer border.
                let theme = panel.theme();

                // Create a bordered block for the panel with theme colors
                let title = if fullscreen {
                    " Wrashpty Panel (Esc to close, F10 restore) "
                } else {
                    " Wrashpty Panel (Esc to close, F10 fullscreen) "
                };
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.panel_border))
                    .title(title)
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

            // Determine poll timeout based on animation state
            let poll_timeout = if panel.is_animating() {
                std::time::Duration::from_millis(50) // Faster updates for animation
            } else {
                std::time::Duration::from_millis(100) // Normal idle
            };

            // Poll for input with timeout
            match event::poll(poll_timeout) {
                Ok(true) => {
                    match event::read() {
                        Ok(Event::Key(key)) => {
                            // Skip key release events (only handle press)
                            if key.kind != crossterm::event::KeyEventKind::Press {
                                continue;
                            }

                            debug!(key = ?key.code, "Panel received key");

                            // F10: toggle fullscreen using alternate screen buffer.
                            // Alt screen saves/restores the main screen atomically,
                            // preserving PTY content and prompt without repaint logic.
                            if key.code == KeyCode::F(10) {
                                let (current_cols, total_rows) =
                                    TerminalGuard::get_size().unwrap_or((*cols, *panel_height));

                                fullscreen = !fullscreen;

                                if fullscreen {
                                    // Enter alternate screen (saves main screen + cursor)
                                    crossterm::execute!(
                                        std::io::stdout(),
                                        crossterm::terminal::EnterAlternateScreen
                                    )
                                    .context("Failed to enter alternate screen")?;
                                    _alt_screen.active = true;

                                    // Save normal height, go fullscreen
                                    normal_panel_height = *panel_height;
                                    *panel_height = total_rows;
                                    *cols = current_cols;

                                    self.chrome
                                        .resize_panel(total_rows, total_rows)
                                        .context("Failed to resize panel to fullscreen")?;
                                } else {
                                    // Restore normal height
                                    let max_ph = total_rows.saturating_sub(2);
                                    *panel_height = normal_panel_height.min(max_ph).max(8);
                                    *cols = current_cols;

                                    // Exit alternate screen (atomically restores main screen)
                                    crossterm::execute!(
                                        std::io::stdout(),
                                        crossterm::terminal::LeaveAlternateScreen
                                    )
                                    .context("Failed to leave alternate screen")?;
                                    _alt_screen.active = false;

                                    // Use expand_panel (not resize_panel) to set PanelState
                                    // and scroll region without clearing restored content.
                                    self.chrome
                                        .expand_panel(*panel_height, total_rows)
                                        .context("Failed to restore panel from fullscreen")?;

                                    // Resize PTY to fit below panel
                                    let effective = total_rows.saturating_sub(*panel_height);
                                    if effective > 0 {
                                        self.pty
                                            .resize(current_cols, effective)
                                            .context("Failed to resize PTY after fullscreen")?;
                                    }
                                }

                                // Clear panel area for clean redraw
                                {
                                    let stdout = std::io::stdout();
                                    let mut out = stdout.lock();
                                    use crossterm::cursor::MoveTo;
                                    use crossterm::terminal::{Clear, ClearType};
                                    for row in 1..=*panel_height {
                                        crossterm::queue!(
                                            out,
                                            MoveTo(0, row - 1),
                                            Clear(ClearType::UntilNewLine)
                                        )?;
                                    }
                                    out.flush()?;
                                }

                                debug!(
                                    fullscreen,
                                    panel_height = *panel_height,
                                    "Toggled fullscreen"
                                );
                                needs_redraw = true;
                                continue;
                            }

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

                            // If terminal is too small, dismiss
                            if new_rows < 10 {
                                return Ok(PanelResult::Dismiss);
                            }

                            if fullscreen {
                                // Stay fullscreen: panel takes entire terminal
                                *panel_height = new_rows;
                                *cols = new_cols;

                                self.chrome
                                    .resize_panel(new_rows, new_rows)
                                    .context("Failed to resize fullscreen panel")?;
                            } else {
                                // Recalculate panel height
                                let preferred = panel.preferred_height();
                                let max_ph = new_rows.saturating_sub(2);
                                let new_panel_height = preferred.min(max_ph).max(8);
                                let new_effective = new_rows.saturating_sub(new_panel_height);
                                if new_effective == 0 {
                                    return Ok(PanelResult::Dismiss);
                                }

                                // Resize panel scroll region
                                self.chrome
                                    .resize_panel(new_panel_height, new_rows)
                                    .context("Failed to resize panel")?;

                                // Resize PTY to fit below panel
                                self.pty
                                    .resize(new_cols, new_effective)
                                    .context("Failed to resize PTY during panel resize")?;

                                // Update tracked dimensions
                                *cols = new_cols;
                                *panel_height = new_panel_height;

                                // Also update normal_panel_height for correct restore
                                normal_panel_height = new_panel_height;
                            }

                            // Clear panel area for clean redraw
                            {
                                let stdout = std::io::stdout();
                                let mut out = stdout.lock();
                                use crossterm::cursor::MoveTo;
                                use crossterm::terminal::{Clear, ClearType};
                                for row in 1..=*panel_height {
                                    crossterm::queue!(
                                        out,
                                        MoveTo(0, row - 1),
                                        Clear(ClearType::UntilNewLine)
                                    )?;
                                }
                                out.flush()?;
                            }

                            needs_redraw = true;
                        }
                        Ok(Event::Mouse(mouse)) => {
                            match mouse.kind {
                                MouseEventKind::Down(MouseButton::Left)
                                | MouseEventKind::Drag(MouseButton::Left) => {
                                    left_mouse_selecting = true;
                                }
                                MouseEventKind::Up(MouseButton::Left) => {
                                    left_mouse_selecting = false;
                                }
                                _ => {}
                            }

                            // Pause marquee animation while selecting text with the mouse.
                            if let Some(tabbed_panel) =
                                panel.as_any_mut().downcast_mut::<TabbedPanel>()
                            {
                                tabbed_panel.set_border_info_animation_paused(left_mouse_selecting);
                            }

                            // Keep current frame while selecting; on release, allow redraw.
                            if !left_mouse_selecting {
                                needs_redraw = true;
                            }
                        }
                        Ok(_) => {
                            // Other events - ignore but don't redraw
                        }
                        Err(e) => {
                            warn!("Error reading event: {}", e);
                            return Ok(PanelResult::Dismiss);
                        }
                    }
                }
                Ok(false) => {
                    // Timeout expired - check if we need to redraw for animation
                    if panel.is_animating() {
                        // Fast path: when only border-info marquee is animating, redraw just that row.
                        if !left_mouse_selecting {
                            if let Some(tabbed_panel) =
                                panel.as_any_mut().downcast_mut::<TabbedPanel>()
                            {
                                let full_area = Rect::new(0, 0, *cols, *panel_height);
                                let title = if fullscreen {
                                    " Wrashpty Panel (Esc to close, F10 restore) "
                                } else {
                                    " Wrashpty Panel (Esc to close, F10 fullscreen) "
                                };
                                let block = Block::default()
                                    .borders(Borders::ALL)
                                    .border_style(
                                        Style::default().fg(tabbed_panel.theme().panel_border),
                                    )
                                    .title(title)
                                    .title_style(
                                        Style::default().fg(tabbed_panel.theme().header_fg),
                                    );
                                let inner_area = block.inner(full_area);

                                if tabbed_panel.supports_partial_animation_row_render() {
                                    let mut delta_buffer = Buffer::empty(full_area);
                                    if let Some(dirty_row) = tabbed_panel
                                        .render_border_info_animation_row(
                                            &mut delta_buffer,
                                            inner_area,
                                        )
                                    {
                                        self.chrome
                                            .render_panel_buffer(&delta_buffer, dirty_row)
                                            .context("Failed to render border info animation row")?;

                                        {
                                            use std::io::Write;
                                            std::io::stdout().flush()?;
                                        }

                                        continue;
                                    }
                                }
                            }
                        }

                        needs_redraw = true;
                    }
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

    /// Repaints scrollback content into the rows that were occupied by the panel.
    ///
    /// After the panel is collapsed, the rows it occupied are blank. This method
    /// writes scrollback buffer content to those rows to restore the previous display.
    fn repaint_after_panel(&self, panel_height: u16, pre_panel_visible_rows: usize) {
        use std::io::Write;

        // Row 1 is the chrome bar (restored by collapse_panel via setup_scroll_region).
        // We need to fill rows 2 through panel_height with scrollback content.
        let repaint_count = panel_height.saturating_sub(1) as usize;
        if repaint_count == 0 || self.scrollback_buffer.is_empty() {
            return;
        }

        // The PTY area below the panel still has its content intact.
        // We need to show the scrollback lines that were above that area before the panel.
        // Those lines are offset from the bottom of the scrollback buffer by the number of
        // PTY-visible rows that existed before the panel (minus the rows we're repainting).
        let offset_from_bottom = pre_panel_visible_rows.saturating_sub(repaint_count);
        let lines: Vec<_> = self
            .scrollback_buffer
            .get_from_bottom(offset_from_bottom, repaint_count)
            .collect();

        if lines.is_empty() {
            return;
        }

        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        use crossterm::cursor::MoveTo;
        use crossterm::style::{Attribute, SetAttribute};
        use crossterm::terminal::{Clear, ClearType};

        // Sanitize and write each line to the correct terminal row
        for (i, line) in lines.iter().enumerate() {
            let row = 2 + i as u16; // Row 1 is chrome bar
            if row > panel_height {
                break;
            }
            // Position cursor, reset attributes
            let _ = crossterm::queue!(out, MoveTo(0, row - 1), SetAttribute(Attribute::Reset));
            // Write sanitized line content (strip dangerous CSI, preserve colors)
            let sanitized = crate::scrollback::sanitize_for_display(line.content());
            let _ = out.write_all(&sanitized);
            // Clear rest of line
            let _ = crossterm::queue!(out, Clear(ClearType::UntilNewLine));
        }

        let _ = out.flush();
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
        let mut panel = TabbedPanel::new(self.chrome.theme(), self.chrome.glyph_tier());
        panel.set_history_store(Arc::clone(&self.history_store));
        panel.load_context(&self.current_cwd);

        match self.run_panel_mode(&mut panel)? {
            PanelResult::Execute(cmd) => {
                debug!(command = %cmd, "Panel executing command");
                // Use the same restore flow as Dismiss - this properly clears the panel
                // and restores terminal state. Then inject the command.
                self.apply_pending_setting_actions(&mut panel);
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
                self.apply_pending_setting_actions(&mut panel);
                self.restore_after_panel()?;
                // Insert the text into reedline's buffer before the next read_line call
                self.editor.prefill_buffer(&text);
            }
            PanelResult::Dismiss | PanelResult::Continue => {
                debug!("Panel dismissed, restoring chrome");
                self.apply_pending_setting_actions(&mut panel);
                // Return to editing - redraw context bar and restore terminal state
                self.restore_after_panel()?;
            }
        }

        Ok(())
    }

    /// Opens panels with the Settings tab on the Help subtab.
    pub fn open_panel_settings_help(&mut self) -> Result<()> {
        let mut panel = TabbedPanel::new(self.chrome.theme(), self.chrome.glyph_tier());
        panel.set_history_store(Arc::clone(&self.history_store));
        panel.load_context(&self.current_cwd);
        panel.switch_to_settings_help();

        match self.run_panel_mode(&mut panel)? {
            PanelResult::Execute(cmd) => {
                debug!(command = %cmd, "Panel executing command");
                self.apply_pending_setting_actions(&mut panel);
                self.restore_after_panel()?;
                if let Ok(mut store) = self.history_store.lock() {
                    if let Err(e) = store.save_command(&cmd, Some(&self.current_cwd)) {
                        warn!("Failed to save panel command to history: {}", e);
                    }
                }
                self.pending_command = Some(cmd);
                self.inject_pending_command()?;
            }
            PanelResult::InsertText(text) => {
                debug!(text = %text, "Panel requested text insertion");
                self.apply_pending_setting_actions(&mut panel);
                self.restore_after_panel()?;
                self.editor.prefill_buffer(&text);
            }
            PanelResult::Dismiss | PanelResult::Continue => {
                debug!("Panel dismissed, restoring chrome");
                self.apply_pending_setting_actions(&mut panel);
                self.restore_after_panel()?;
            }
        }

        Ok(())
    }

    /// Applies any pending setting actions from the panel to Chrome and app state.
    fn apply_pending_setting_actions(&mut self, panel: &mut TabbedPanel) {
        for action in panel.take_pending_actions() {
            match action {
                SettingAction::SetGlyphTier(tier) => {
                    debug!(?tier, "Applying glyph tier from settings panel");
                    self.chrome.set_glyph_tier(tier);
                }
                SettingAction::SetTheme(preset) => {
                    debug!(?preset, "Applying theme from settings panel");
                    let theme = crate::chrome::theme::Theme::for_preset(preset);
                    self.chrome.set_theme(theme);
                }
                SettingAction::SetScrollbackEnabled(enabled) => {
                    debug!(enabled, "Applying scrollback enabled from settings panel");
                    if enabled {
                        self.scrollback_buffer.resume_capture();
                    } else {
                        self.scrollback_buffer.suspend_capture();
                    }
                    self.scrollback_config.enabled = enabled;
                }
                SettingAction::SetScrollbackMaxLines(n) => {
                    debug!(
                        max_lines = n,
                        "Applying scrollback max lines from settings panel"
                    );
                    self.scrollback_buffer.set_max_lines(n);
                    self.scrollback_config.max_lines = n;
                }
                SettingAction::SetScrollbackMaxLineBytes(n) => {
                    debug!(
                        max_line_bytes = n,
                        "Applying scrollback max line bytes from settings panel"
                    );
                    self.scrollback_buffer.set_max_line_bytes(n);
                    self.scrollback_config.max_line_bytes = n;
                }
            }
        }
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

        // Position cursor at the BOTTOM of the scroll region where the shell prompt was.
        // This is critical: reedline uses Clear(FromCursorDown) when painting the prompt,
        // which wipes everything below the cursor. By placing the cursor at the bottom,
        // the repainted scrollback content and PTY content above are preserved.
        if self.chrome.is_active() {
            crossterm::execute!(std::io::stdout(), crossterm::cursor::MoveTo(0, rows - 1))?;
        }

        Ok(())
    }

    /// Immediately re-renders the context bar.
    ///
    /// Used after colon commands so the user sees the effect (notification
    /// or updated glyphs) without waiting for the next prompt cycle.
    fn render_context_bar_now(&mut self) {
        if self.chrome.is_active() {
            if let Ok((cols, _rows)) = TerminalGuard::get_size() {
                let timestamp = chrono::Local::now().format("%H:%M").to_string();
                let state = self.topbar_state(&timestamp);
                if let Err(e) = self
                    .chrome
                    .render_context_bar_with_notifications(cols, &state)
                {
                    warn!("Failed to re-render context bar after command: {}", e);
                }
            }
        }
    }

    /// Injects the pending command into the PTY.
    ///
    /// Creates an EchoGuard to suppress echo, writes the command,
    /// and keeps suppression active until execution starts.
    fn inject_pending_command(&mut self) -> Result<()> {
        let command = self.pending_command.take().ok_or_else(|| {
            anyhow::anyhow!("inject_pending_command called without pending command")
        })?;

        debug!(command = %command, "Injecting command");

        // Store command for context bar display
        self.last_command = Some(command.clone());
        self.viewer_state.boundaries.seed_record(
            self.scrollback_buffer.len(),
            Some(command.clone()),
            Some(self.current_cwd.clone()),
            Some(chrono::Local::now().naive_local()),
        );

        // Transition to Injecting mode first (syncs PTY size, records start time)
        self.transition_to_injecting()?;

        // Ensure no stale guard remains from a previous injection path.
        self.injection_echo_guard = None;

        // Create echo guard to suppress command echo and keep it alive until
        // we leave Injecting mode.
        let guard = self
            .pty
            .create_echo_guard()
            .context("Failed to create echo guard")?;

        // Write command to PTY
        self.pty
            .write_command(&command)
            .context("Failed to write command to PTY")?;

        // Keep guard alive across Injecting so late PTY echo doesn't leak.
        self.injection_echo_guard = Some(guard);

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
mod tests {
    use super::*;

    // =========================================================================
    // Constant Tests
    // =========================================================================

    #[test]
    fn test_initialization_timeout_default_within_expected_range() {
        // Verify timeout is reasonable
        assert!(INITIALIZATION_TIMEOUT >= Duration::from_secs(5));
        assert!(INITIALIZATION_TIMEOUT <= Duration::from_secs(30));
    }

    #[test]
    fn test_termination_timeout_default_within_expected_range() {
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
    fn test_injection_poll_timeout_within_bounds_and_less_than_injection_timeout() {
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
    fn test_exit_code_from_status_zero_returns_zero() {
        // Create a mock ExitStatus representing success
        // Note: ExitStatus::with_exit_code is not directly available,
        // but we can test the function with known values
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(0);
        assert_eq!(exit_code_from_status(&status), 0);
    }

    #[test]
    fn test_exit_code_from_status_nonzero_returns_same_code() {
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
    fn test_exit_code_from_status_255_returns_255() {
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(255);
        assert_eq!(exit_code_from_status(&status), 255);
    }

    // =========================================================================
    // Mode Transition Tests
    // =========================================================================

    #[test]
    fn test_mode_equality_variants_compare_equal_and_not_equal() {
        assert_eq!(Mode::Initializing, Mode::Initializing);
        assert_eq!(Mode::Edit, Mode::Edit);
        assert_eq!(Mode::Passthrough, Mode::Passthrough);
        assert_eq!(Mode::Injecting, Mode::Injecting);
        assert_eq!(Mode::Terminating, Mode::Terminating);

        assert_ne!(Mode::Initializing, Mode::Edit);
        assert_ne!(Mode::Edit, Mode::Passthrough);
    }

    #[test]
    fn test_mode_debug_format_contains_variant_names() {
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

    // =========================================================================
    // Marker Batching Tests
    //
    // These tests verify that when multiple markers arrive in a single read
    // batch (common when commands fail quickly), all markers are processed
    // and none are lost. This guards against regression of the batching bug
    // where early returns after state transitions would lose remaining markers.
    // =========================================================================

    /// Helper to create a valid marker sequence for testing.
    fn make_test_marker(token: &[u8; 16], marker_type: &str, payload: Option<&str>) -> Vec<u8> {
        let mut seq = vec![0x1B, 0x5D]; // ESC ]
        seq.extend_from_slice(b"777;");
        seq.extend_from_slice(token);
        seq.push(b';');
        seq.extend_from_slice(marker_type.as_bytes());
        if let Some(p) = payload {
            seq.push(b';');
            seq.extend_from_slice(p.as_bytes());
        }
        seq.push(0x07); // BEL
        seq
    }

    #[test]
    fn test_marker_batching_all_markers_parsed_from_single_chunk() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate a fast-failing command: PREEXEC, PRECMD, PROMPT all arrive together
        let mut batch = Vec::new();
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(make_test_marker(&token, "PRECMD", Some("127"))); // command not found
        batch.extend(make_test_marker(&token, "PROMPT", None));

        let outputs: Vec<_> = parser.feed(&batch).collect();

        // All three markers should be parsed
        let markers: Vec<_> = outputs
            .iter()
            .filter_map(|o| match o {
                ParseOutput::Marker(m) => Some(m.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(
            markers.len(),
            3,
            "All three markers should be parsed from batch"
        );
        assert!(matches!(markers[0], MarkerEvent::Preexec));
        assert!(matches!(markers[1], MarkerEvent::Precmd { exit_code: 127 }));
        assert!(matches!(markers[2], MarkerEvent::Prompt));
    }

    #[test]
    fn test_marker_batching_with_interleaved_output() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate markers with error output interleaved
        let mut batch = Vec::new();
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: foo: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        let outputs: Vec<_> = parser.feed(&batch).collect();

        // Count markers and bytes
        let mut marker_count = 0;
        let mut byte_chunks = 0;
        for output in &outputs {
            match output {
                ParseOutput::Marker(_) => marker_count += 1,
                ParseOutput::Bytes(_) => byte_chunks += 1,
            }
        }

        assert_eq!(marker_count, 3, "All three markers should be parsed");
        assert!(byte_chunks >= 1, "Error output should be passed through");
    }

    #[test]
    fn test_marker_batching_rapid_command_sequence() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate multiple rapid commands (like pasting garbage)
        // Each command cycle: PREEXEC -> PRECMD -> PROMPT
        let mut batch = Vec::new();

        // Command 1: "foo" (not found)
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: foo: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        // Command 2: "bar" (not found)
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: bar: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        // Command 3: "baz" (not found)
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: baz: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        let outputs: Vec<_> = parser.feed(&batch).collect();

        // Count markers by type
        let mut preexec_count = 0;
        let mut precmd_count = 0;
        let mut prompt_count = 0;

        for output in &outputs {
            if let ParseOutput::Marker(m) = output {
                match m {
                    MarkerEvent::Preexec => preexec_count += 1,
                    MarkerEvent::Precmd { .. } => precmd_count += 1,
                    MarkerEvent::Prompt => prompt_count += 1,
                }
            }
        }

        assert_eq!(preexec_count, 3, "All PREEXEC markers should be parsed");
        assert_eq!(precmd_count, 3, "All PRECMD markers should be parsed");
        assert_eq!(prompt_count, 3, "All PROMPT markers should be parsed");
    }

    #[test]
    fn test_injection_batched_marker_transitions() {
        // Test that the mode transition logic handles batched markers correctly.
        // This simulates what happens in run_injecting() when processing a batch.
        let mut mode = Mode::Injecting;
        let mut last_exit_code = 0;

        // Simulate receiving [PREEXEC, PRECMD, PROMPT] in one batch
        let markers = vec![
            MarkerEvent::Preexec,
            MarkerEvent::Precmd { exit_code: 127 },
            MarkerEvent::Prompt,
        ];

        // Process markers the same way run_injecting() does after the fix
        for marker in markers {
            match mode {
                Mode::Injecting => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        mode = Mode::Passthrough;
                    }
                },
                Mode::Passthrough => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        // Already in passthrough
                    }
                },
                Mode::Edit => {
                    // Already in edit, done processing
                    break;
                }
                _ => break,
            }
        }

        // After processing the batch, we should end up in Edit mode
        assert_eq!(mode, Mode::Edit, "Should end in Edit mode after PROMPT");
        assert_eq!(
            last_exit_code, 127,
            "Exit code should be captured from PRECMD"
        );
    }

    #[test]
    fn test_passthrough_batched_marker_transitions() {
        // Test that run_passthrough() logic handles batched markers correctly
        let mode = Mode::Passthrough;
        let mut last_exit_code = 0;

        // Simulate receiving [PRECMD, PROMPT, PREEXEC, PRECMD, PROMPT] in one batch
        // This represents: command ends, prompt shown, new command starts, fails, prompt shown
        let markers = vec![
            MarkerEvent::Precmd { exit_code: 0 },
            MarkerEvent::Prompt,
            MarkerEvent::Preexec,
            MarkerEvent::Precmd { exit_code: 1 },
            MarkerEvent::Prompt,
        ];

        let mut final_mode = mode;
        for marker in markers {
            match final_mode {
                Mode::Passthrough => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        final_mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        // Stay in passthrough
                    }
                },
                Mode::Edit => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        // Still update exit code for markers that arrive while in Edit
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        // Already at prompt
                    }
                    MarkerEvent::Preexec => {
                        // Unexpected in edit
                    }
                },
                _ => break,
            }
        }

        // Should end in Edit mode with the first PROMPT
        assert_eq!(final_mode, Mode::Edit);
        // With the batching fix, we continue processing markers in Edit mode,
        // so the final exit code is 1 from the second PRECMD
        assert_eq!(last_exit_code, 1);
    }

    #[test]
    fn test_initializing_mode_batched_markers() {
        // Test that run_initializing() logic handles batched markers
        let mut mode = Mode::Initializing;
        let mut last_exit_code = 0;

        // During initialization, we might see PRECMD and PROMPT together
        let markers = vec![MarkerEvent::Precmd { exit_code: 0 }, MarkerEvent::Prompt];

        for marker in markers {
            match mode {
                Mode::Initializing => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        // Unexpected
                    }
                },
                Mode::Edit => {
                    // Handle remaining markers in Edit context
                    if let MarkerEvent::Precmd { exit_code } = marker {
                        last_exit_code = exit_code;
                    }
                }
                _ => break,
            }
        }

        assert_eq!(mode, Mode::Edit);
        assert_eq!(last_exit_code, 0);
    }

    #[test]
    fn test_marker_batching_no_markers_lost_regression() {
        // Regression test: ensure we don't lose markers after state transitions.
        // This specifically tests the bug where returning early after PREEXEC
        // would lose subsequent PRECMD and PROMPT markers.
        use crate::marker::{MarkerParser, ParseOutput};
        use smallvec::SmallVec;

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Create the exact scenario that caused the bug:
        // Fast-failing command where all markers arrive in one read
        let mut batch = Vec::new();
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        // Parse all markers
        let mut markers: SmallVec<[MarkerEvent; 4]> = SmallVec::new();
        for output in parser.feed(&batch) {
            if let ParseOutput::Marker(m) = output {
                markers.push(m);
            }
        }

        // Simulate processing with the FIXED logic (continues after transitions)
        let mut mode = Mode::Injecting;
        let mut reached_edit = false;

        for marker in &markers {
            match mode {
                Mode::Injecting => match marker {
                    MarkerEvent::Preexec => {
                        mode = Mode::Passthrough;
                        // BUG FIX: Don't return here, continue processing
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                        reached_edit = true;
                    }
                    MarkerEvent::Precmd { .. } => {}
                },
                Mode::Passthrough => {
                    if marker == &MarkerEvent::Prompt {
                        mode = Mode::Edit;
                        reached_edit = true;
                    }
                }
                Mode::Edit => {
                    reached_edit = true;
                    break;
                }
                _ => {}
            }
        }

        // The critical assertion: we MUST reach Edit mode
        assert!(
            reached_edit,
            "Must reach Edit mode after processing batched markers"
        );
        assert_eq!(
            mode,
            Mode::Edit,
            "Final mode must be Edit after PROMPT marker"
        );
    }
}
