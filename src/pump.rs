//! Passthrough byte pump for transparent I/O.
//!
//! This module handles bidirectional byte streaming between the user terminal
//! and the PTY during Passthrough mode, with marker detection spliced in.
//!
//! # Architecture
//!
//! The pump is the critical hot path for all command output. It uses poll-based
//! I/O to efficiently wait on both stdin and the PTY master, forwarding bytes
//! in both directions while the marker parser scans for OSC 777 sequences.
//!
//! ```text
//! ┌────────────┐        ┌──────────┐        ┌──────────┐
//! │ User stdin │───────▶│   Pump   │───────▶│ PTY      │
//! └────────────┘        │          │        └──────────┘
//!                       │          │              │
//! ┌────────────┐        │  Parser  │◀─────────────┘
//! │ User stdout│◀───────│          │
//! └────────────┘        └──────────┘
//! ```
//!
//! # Performance Characteristics
//!
//! - Zero-allocation in the hot path (normal passthrough)
//! - Fixed 4KB buffers balance syscall overhead vs memory usage
//! - Adaptive poll timeout: 50ms when buffering partial markers, infinite otherwise
//! - Marker parser uses `Cow<'a, [u8]>` for zero-copy in common case
//!
//! # Poll Timeout Strategy
//!
//! The pump uses adaptive timeouts based on parser state:
//! - **Infinite (-1)**: When no partial sequence is buffered. This is CPU-efficient
//!   as we block until I/O is ready.
//! - **50ms**: When the parser is mid-sequence. This allows us to flush stale
//!   sequences if no new data arrives (e.g., an ESC byte that wasn't a marker).
//!
//! # Safety Notes
//!
//! The `pty_fd` passed to `Pump::new()` must remain valid for the lifetime of
//! the pump. The PTY file descriptor is obtained from `Pty::master_fd()` and
//! is owned by the `Pty` struct - ensure the `Pty` outlives the `Pump`.

use std::os::unix::io::{BorrowedFd, RawFd};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::poll::{PollFd, PollFlags, poll};
use nix::unistd::{read, write};
use smallvec::SmallVec;

use crate::marker::{MarkerParser, ParseOutput};
use crate::types::MarkerEvent;

/// Result of a non-blocking PTY read for Edit mode buffering.
#[derive(Debug)]
pub struct EditModeReadResult {
    /// Bytes read from PTY (to be buffered, not written to stdout).
    pub bytes: Vec<u8>,
    /// Any markers detected during the read.
    pub markers: MarkerVec,
    /// Whether PTY EOF was detected.
    pub eof: bool,
}

/// Inline capacity for marker events in a single read.
/// Most reads contain 0-2 markers; this avoids heap allocation for common cases.
pub type MarkerVec = SmallVec<[MarkerEvent; 2]>;

/// Standard file descriptor for stdin.
const STDIN_FILENO: RawFd = 0;

/// Standard file descriptor for stdout.
const STDOUT_FILENO: RawFd = 1;

/// Buffer size for read operations.
///
/// 4KB balances syscall overhead (fewer calls for large outputs) with memory
/// usage (small enough to avoid cache thrashing). This is a common choice for
/// terminal I/O and matches typical pipe buffer sizes.
const BUFFER_SIZE: usize = 4096;

/// Poll timeout when mid-sequence (milliseconds).
///
/// When the parser has buffered a partial marker sequence, we use a short
/// timeout to detect stale sequences that aren't actually markers.
const MID_SEQUENCE_TIMEOUT_MS: i32 = 50;

/// Stale sequence flush threshold.
///
/// If no new data arrives within this duration while mid-sequence, we flush
/// the buffered bytes as passthrough data.
const STALE_SEQUENCE_THRESHOLD: Duration = Duration::from_millis(100);

/// Result of a single pump iteration.
///
/// The pump returns these values to allow the caller (state machine) to
/// handle state transitions appropriately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpResult {
    /// One or more valid markers were detected and stripped from output.
    /// The caller should handle each state transition in order.
    /// Multiple markers can occur in a single read when shell output
    /// contains adjacent markers (e.g., PRECMD followed immediately by PROMPT).
    ///
    /// Uses `SmallVec` for zero-allocation in the common 0-2 marker case.
    /// Includes captured bytes written to stdout for scrollback support.
    MarkerDetected {
        /// Markers detected in this read.
        markers: MarkerVec,
        /// Bytes written to stdout (for scrollback capture).
        captured_bytes: Vec<u8>,
    },

    /// Normal operation, no events. Continue pumping.
    /// Includes captured bytes written to stdout for scrollback support.
    Continue {
        /// Bytes written to stdout (for scrollback capture).
        /// Empty if no PTY data was read.
        captured_bytes: Vec<u8>,
    },

    /// The PTY returned EOF (child process terminated).
    /// The caller should initiate shutdown.
    PtyEof,
}

impl PumpResult {
    /// Returns captured bytes from this pump iteration.
    ///
    /// Returns an empty slice for PtyEof.
    #[inline]
    pub fn captured_bytes(&self) -> &[u8] {
        match self {
            PumpResult::MarkerDetected { captured_bytes, .. } => captured_bytes,
            PumpResult::Continue { captured_bytes } => captured_bytes,
            PumpResult::PtyEof => &[],
        }
    }

    /// Returns markers if any were detected.
    #[inline]
    pub fn markers(&self) -> Option<&MarkerVec> {
        match self {
            PumpResult::MarkerDetected { markers, .. } => Some(markers),
            _ => None,
        }
    }
}

/// Bidirectional byte pump with marker detection.
///
/// The pump handles transparent I/O forwarding between stdin/stdout and the
/// PTY while scanning for OSC 777 marker sequences. When a marker is detected,
/// it is stripped from the output and returned to the caller.
///
/// # Example
///
/// ```ignore
/// use std::os::unix::io::AsRawFd;
/// use wrashpty::pump::{Pump, PumpResult};
///
/// let pty_fd = pty.master_fd();
/// let token = session_token;
/// let signal_fd = signal_handler.as_raw_fd();
/// let mut pump = Pump::new(pty_fd, token, Some(signal_fd));
///
/// loop {
///     match pump.run_once()? {
///         PumpResult::MarkerDetected(events) => {
///             // Handle each state transition in order
///             for event in events {
///                 match event {
///                     MarkerEvent::Prompt => break, // Return to edit mode
///                     _ => {}
///                 }
///             }
///         }
///         PumpResult::PtyEof => break,
///         PumpResult::Continue => {
///             // Check for pending signals
///             signal_handler.check_signals();
///         }
///     }
/// }
/// ```
#[derive(Debug)]
pub struct Pump {
    /// PTY master file descriptor for poll operations.
    pty_fd: RawFd,

    /// Signal handler file descriptor for waking on signal delivery.
    /// When readable, signals are pending and the caller should process them.
    signal_fd: Option<RawFd>,

    /// Streaming marker parser instance.
    marker_parser: MarkerParser,

    /// Timestamp of last I/O activity for stale sequence detection.
    /// Only updated when bytes are actually read or written, not on poll timeout.
    last_activity_time: Instant,

    /// Whether stdin has been closed (EOF, HUP, or error).
    /// Once set, stdin is excluded from subsequent polls to avoid busy-looping.
    stdin_closed: bool,

    /// Whether stdin bytes should be intercepted rather than forwarded to PTY.
    /// When true, stdin bytes are stored in `stdin_buffer` for caller processing.
    stdin_intercept: bool,

    /// Buffer for intercepted stdin bytes (when `stdin_intercept` is true).
    stdin_buffer: Vec<u8>,
}

impl Pump {
    /// Creates a new pump for the given PTY and session token.
    ///
    /// # Arguments
    ///
    /// * `pty_fd` - The PTY master file descriptor from `Pty::master_fd()`
    /// * `session_token` - 16-byte session token for marker validation
    /// * `signal_fd` - Optional signal handler fd to wake on signal delivery
    ///
    /// # Safety
    ///
    /// The caller must ensure `pty_fd` and `signal_fd` (if provided) remain
    /// valid for the lifetime of the pump.
    #[must_use]
    pub fn new(pty_fd: RawFd, session_token: [u8; 16], signal_fd: Option<RawFd>) -> Self {
        Self {
            pty_fd,
            signal_fd,
            marker_parser: MarkerParser::new(session_token),
            last_activity_time: Instant::now(),
            stdin_closed: false,
            stdin_intercept: false,
            stdin_buffer: Vec::new(),
        }
    }

    /// Runs a single iteration of the pump loop.
    ///
    /// This method:
    /// 1. Checks for and flushes stale marker sequences
    /// 2. Polls stdin and PTY for readability
    /// 3. Forwards stdin bytes to PTY
    /// 4. Forwards PTY bytes to stdout (through marker parser)
    /// 5. Returns any detected markers or EOF
    ///
    /// # Returns
    ///
    /// - `Ok(PumpResult::Continue)` - Normal operation, no events
    /// - `Ok(PumpResult::MarkerDetected(events))` - One or more markers found, handle state changes
    /// - `Ok(PumpResult::PtyEof)` - Child process terminated
    /// - `Err(_)` - I/O error occurred
    ///
    /// # Errors
    ///
    /// Returns an error if poll, read, or write syscalls fail.
    pub fn run_once(&mut self) -> Result<PumpResult> {
        self.run_once_inner(None)
    }

    /// Runs a single iteration of the pump loop with a bounded wait time.
    ///
    /// This is similar to `run_once`, but accepts an optional maximum wait duration.
    /// When `max_wait` is `Some(duration)`, the poll will return after at most that
    /// duration even if no I/O is ready. This allows callers to implement their own
    /// timeout logic by periodically waking to check conditions.
    ///
    /// # Arguments
    ///
    /// * `max_wait` - Optional maximum duration to wait. If `None`, uses infinite
    ///   timeout (or mid-sequence timeout if buffering a partial marker).
    ///
    /// # Returns
    ///
    /// Same as `run_once`.
    ///
    /// # Errors
    ///
    /// Returns an error if poll, read, or write syscalls fail.
    pub fn run_once_with_timeout(&mut self, max_wait: Option<Duration>) -> Result<PumpResult> {
        self.run_once_inner(max_wait)
    }

    /// Internal implementation for run_once variants.
    fn run_once_inner(&mut self, max_wait: Option<Duration>) -> Result<PumpResult> {
        // Check for stale partial sequences that need flushing
        self.check_stale_sequence()?;

        // Calculate poll timeout based on parser state and caller-specified max wait
        // -1 means infinite timeout, positive value is milliseconds
        let poll_timeout_ms: i32 = if self.marker_parser.is_mid_sequence() {
            // Short timeout when buffering partial sequence
            // Use the minimum of mid-sequence timeout and caller's max_wait
            match max_wait {
                Some(d) => {
                    let max_ms = d.as_millis().min(i32::MAX as u128) as i32;
                    max_ms.min(MID_SEQUENCE_TIMEOUT_MS)
                }
                None => MID_SEQUENCE_TIMEOUT_MS,
            }
        } else {
            // Use caller's max_wait or block indefinitely
            match max_wait {
                Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
                None => -1,
            }
        };

        // SAFETY: These file descriptors remain valid for the duration of the poll call.
        // STDIN_FILENO (0) is always valid, pty_fd is owned by Pty which outlives Pump,
        // and signal_fd (if present) is owned by the signal handler which outlives Pump.
        let stdin_fd = unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) };
        let pty_fd = unsafe { BorrowedFd::borrow_raw(self.pty_fd) };
        let signal_fd = self
            .signal_fd
            .map(|fd| unsafe { BorrowedFd::borrow_raw(fd) });

        // Build poll array dynamically based on state.
        // Max 3 fds: stdin (optional), PTY (always), signal (optional).
        let mut pollfds: SmallVec<[PollFd<'_>; 3]> = SmallVec::new();

        // Add stdin if not closed (to avoid busy-looping on closed stdin)
        let stdin_idx = if !self.stdin_closed {
            pollfds.push(PollFd::new(&stdin_fd, PollFlags::POLLIN));
            Some(pollfds.len() - 1)
        } else {
            None
        };

        // PTY is always polled
        let pty_idx = pollfds.len();
        pollfds.push(PollFd::new(&pty_fd, PollFlags::POLLIN));

        // Add signal fd if present
        let signal_idx = signal_fd.as_ref().map(|fd| {
            pollfds.push(PollFd::new(fd, PollFlags::POLLIN));
            pollfds.len() - 1
        });

        // Wait for I/O events, retrying on EINTR (signal interruption)
        loop {
            match poll(&mut pollfds, poll_timeout_ms) {
                Ok(_) => break,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => return Err(e).context("Poll failed"),
            }
        }

        // Check signal fd first - return to let caller handle signals
        // But first, still process any ready I/O to avoid data loss
        if let Some(idx) = signal_idx {
            if let Some(revents) = pollfds[idx].revents() {
                if revents.contains(PollFlags::POLLIN) {
                    if let Some(stdin_idx) = stdin_idx {
                        self.process_stdin_events(&pollfds[stdin_idx])?;
                    }
                    if let Some(result) = self.process_pty_events(&pollfds[pty_idx])? {
                        return Ok(result);
                    }
                    return Ok(PumpResult::Continue {
                        captured_bytes: Vec::new(),
                    });
                }
            }
        }

        // Process stdin events (if stdin is in the poll array)
        if let Some(idx) = stdin_idx {
            self.process_stdin_events(&pollfds[idx])?;
        }

        // Process PTY events
        if let Some(result) = self.process_pty_events(&pollfds[pty_idx])? {
            return Ok(result);
        }

        Ok(PumpResult::Continue {
            captured_bytes: Vec::new(),
        })
    }

    /// Process stdin poll events.
    fn process_stdin_events(&mut self, pollfd: &PollFd) -> Result<()> {
        if let Some(revents) = pollfd.revents() {
            // Check for hang-up or error conditions (stdin closed)
            if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                self.stdin_closed = true;
                tracing::debug!("Stdin closed (HUP/ERR)");
            } else if revents.contains(PollFlags::POLLIN) {
                self.forward_stdin_to_pty()?;
            }
        }
        Ok(())
    }

    /// Process PTY poll events. Returns Some(result) if a state-changing event occurred.
    fn process_pty_events(&mut self, pollfd: &PollFd) -> Result<Option<PumpResult>> {
        if let Some(revents) = pollfd.revents() {
            // Check for hang-up or error conditions (child process terminated)
            if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                return Ok(Some(self.forward_pty_to_stdout()?));
            }
            if revents.contains(PollFlags::POLLIN) {
                return Ok(Some(self.forward_pty_to_stdout()?));
            }
        }
        Ok(None)
    }

    /// Checks for and flushes stale partial marker sequences.
    ///
    /// If the parser has been buffering a partial sequence for longer than
    /// the stale threshold (with no new I/O activity), we flush it as
    /// passthrough data. This handles the case where an ESC byte (or partial
    /// OSC) wasn't actually a marker.
    fn check_stale_sequence(&mut self) -> Result<()> {
        if self.marker_parser.is_mid_sequence()
            && self.last_activity_time.elapsed() > STALE_SEQUENCE_THRESHOLD
        {
            if let Some(stale_bytes) = self.marker_parser.flush_stale() {
                write_all(STDOUT_FILENO, stale_bytes)?;
                tracing::warn!("Flushed stale marker sequence");
            }
        }
        Ok(())
    }

    /// Forwards bytes from stdin to the PTY.
    ///
    /// Reads available data from stdin and writes it to the PTY master.
    /// This is the input path for user keystrokes during command execution.
    ///
    /// When `stdin_intercept` is enabled, bytes are buffered for caller processing
    /// instead of being forwarded to the PTY. This allows the caller to filter
    /// scroll keys before forwarding the remainder.
    #[inline]
    fn forward_stdin_to_pty(&mut self) -> Result<()> {
        let mut buf = [0u8; BUFFER_SIZE];

        match read(STDIN_FILENO, &mut buf) {
            Ok(0) => {
                // EOF on stdin - user closed input
                // Set flag to exclude stdin from future polls and prevent busy-looping
                // The PTY will continue running until child exits
                self.stdin_closed = true;
                tracing::debug!("Stdin closed (EOF)");
            }
            Ok(n) => {
                if self.stdin_intercept {
                    // Buffer for caller to process (scroll key filtering)
                    self.stdin_buffer.extend_from_slice(&buf[..n]);
                } else {
                    // Normal forwarding to PTY
                    write_all(self.pty_fd, &buf[..n])?;
                }
                // Update activity timestamp on successful I/O
                self.last_activity_time = Instant::now();
            }
            Err(nix::errno::Errno::EAGAIN) => {
                // No data available, ignore
            }
            Err(e) => {
                return Err(e).context("Failed to read stdin");
            }
        }

        Ok(())
    }

    /// Forwards bytes from PTY to stdout with marker detection.
    ///
    /// Reads available data from the PTY and processes it through the marker
    /// parser. Passthrough bytes are written to stdout; the first detected
    /// marker (if any) is returned for state machine handling.
    ///
    /// This method processes the entire read chunk before returning, ensuring
    /// no bytes after a marker are lost. Captured bytes are included in the
    /// result for scrollback support.
    #[inline]
    fn forward_pty_to_stdout(&mut self) -> Result<PumpResult> {
        let mut buf = [0u8; BUFFER_SIZE];

        match read(self.pty_fd, &mut buf) {
            Ok(0) => {
                // PTY EOF - child process terminated
                tracing::info!("PTY EOF detected");
                Ok(PumpResult::PtyEof)
            }
            Ok(n) => {
                let mut markers = MarkerVec::new();
                let mut captured_bytes = Vec::new();

                // Process through marker parser, accumulating all markers
                for output in self.marker_parser.feed(&buf[..n]) {
                    match output {
                        ParseOutput::Bytes(cow_bytes) => {
                            write_all(STDOUT_FILENO, &cow_bytes)?;
                            // Capture bytes for scrollback
                            captured_bytes.extend_from_slice(&cow_bytes);
                        }
                        ParseOutput::Marker(marker) => {
                            tracing::debug!(?marker, "Marker detected");
                            markers.push(marker);
                        }
                    }
                }

                // Update activity timestamp after successful PTY read and processing
                self.last_activity_time = Instant::now();

                if markers.is_empty() {
                    Ok(PumpResult::Continue { captured_bytes })
                } else {
                    Ok(PumpResult::MarkerDetected {
                        markers,
                        captured_bytes,
                    })
                }
            }
            Err(nix::errno::Errno::EAGAIN) => {
                // No data available, ignore
                Ok(PumpResult::Continue {
                    captured_bytes: Vec::new(),
                })
            }
            Err(nix::errno::Errno::EIO) => {
                // EIO on PTY read indicates child process exited - treat as EOF
                // This is common on Linux when the slave side of the PTY is closed
                tracing::info!("PTY EIO detected (child exited)");
                Ok(PumpResult::PtyEof)
            }
            Err(e) => Err(e).context("Failed to read PTY"),
        }
    }

    /// Non-blocking read from PTY for Edit mode background output buffering.
    ///
    /// This method polls the PTY with zero timeout and returns any available
    /// bytes and markers WITHOUT writing to stdout. The caller is responsible
    /// for buffering the bytes and handling markers.
    ///
    /// This is used during Edit mode to capture background job output without
    /// corrupting the reedline display.
    ///
    /// The method loops with zero-timeout polls to fully drain the PTY backlog
    /// in a single call, ensuring all pending output is captured.
    ///
    /// # Returns
    ///
    /// - `Ok(result)` - Contains bytes to buffer, markers to handle, and EOF flag
    /// - `Err(_)` - I/O error occurred
    pub fn poll_pty_nonblocking(&mut self) -> Result<EditModeReadResult> {
        // Check for stale partial sequences first
        let mut result = EditModeReadResult {
            bytes: Vec::new(),
            markers: MarkerVec::new(),
            eof: false,
        };

        // Flush any stale sequences
        if self.marker_parser.is_mid_sequence()
            && self.last_activity_time.elapsed() > STALE_SEQUENCE_THRESHOLD
        {
            if let Some(stale_bytes) = self.marker_parser.flush_stale() {
                result.bytes.extend_from_slice(stale_bytes);
                tracing::warn!("Flushed stale marker sequence during edit mode");
            }
        }

        // Loop to fully drain the PTY backlog
        loop {
            // SAFETY: pty_fd is owned by Pty which outlives Pump
            let pty_fd = unsafe { BorrowedFd::borrow_raw(self.pty_fd) };
            let mut pollfds = [PollFd::new(&pty_fd, PollFlags::POLLIN)];

            // Zero timeout for non-blocking poll
            match poll(&mut pollfds, 0) {
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue, // Interrupted, retry poll
                Err(e) => return Err(e).context("Non-blocking poll failed"),
            }

            // Check if PTY has data
            if let Some(revents) = pollfds[0].revents() {
                if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                    // PTY closed - read remaining data then signal EOF
                    let had_data = self.read_pty_to_buffer(&mut result)?;
                    if !had_data {
                        result.eof = true;
                    }
                    break;
                } else if revents.contains(PollFlags::POLLIN) {
                    let had_data = self.read_pty_to_buffer(&mut result)?;
                    if !had_data {
                        // No more data available (EAGAIN), stop draining
                        break;
                    }
                    // Continue loop to check for more data
                } else {
                    // No readable events, stop draining
                    break;
                }
            } else {
                // No events at all, stop draining
                break;
            }
        }

        Ok(result)
    }

    /// Processes pre-read bytes through the marker parser.
    ///
    /// This is used during Edit mode when a background thread has already
    /// read bytes from the PTY. The bytes are fed through the marker parser
    /// and results are returned without performing any PTY I/O.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw bytes previously read from the PTY
    /// * `is_eof` - Whether EOF was detected when reading these bytes
    ///
    /// # Returns
    ///
    /// An `EditModeReadResult` containing parsed bytes, markers, and EOF status.
    pub fn process_read_bytes(&mut self, bytes: &[u8], is_eof: bool) -> EditModeReadResult {
        let mut result = EditModeReadResult {
            bytes: Vec::new(),
            markers: MarkerVec::new(),
            eof: is_eof,
        };

        // Check for stale sequences first
        if self.marker_parser.is_mid_sequence()
            && self.last_activity_time.elapsed() > STALE_SEQUENCE_THRESHOLD
        {
            if let Some(stale_bytes) = self.marker_parser.flush_stale() {
                result.bytes.extend_from_slice(stale_bytes);
                tracing::warn!("Flushed stale marker sequence during background drain");
            }
        }

        // Process the provided bytes through the marker parser
        for output in self.marker_parser.feed(bytes) {
            match output {
                ParseOutput::Bytes(cow_bytes) => {
                    result.bytes.extend_from_slice(&cow_bytes);
                }
                ParseOutput::Marker(marker) => {
                    tracing::debug!(?marker, "Marker detected during background drain");
                    result.markers.push(marker);
                }
            }
        }

        if !bytes.is_empty() {
            self.last_activity_time = Instant::now();
        }

        result
    }

    /// Reads PTY data into a buffer result (for Edit mode).
    ///
    /// Unlike forward_pty_to_stdout, this collects bytes instead of writing them.
    /// Loops reading until `EAGAIN` or EOF to fully drain available data.
    ///
    /// # Returns
    ///
    /// - `Ok(true)` - Data was read successfully
    /// - `Ok(false)` - No data available (EAGAIN) or EOF reached
    fn read_pty_to_buffer(&mut self, result: &mut EditModeReadResult) -> Result<bool> {
        let mut buf = [0u8; BUFFER_SIZE];
        let mut read_any = false;

        // Loop reading until EAGAIN or EOF to fully drain the backlog
        loop {
            match read(self.pty_fd, &mut buf) {
                Ok(0) => {
                    tracing::info!("PTY EOF detected during edit mode read");
                    result.eof = true;
                    return Ok(read_any);
                }
                Ok(n) => {
                    read_any = true;
                    for output in self.marker_parser.feed(&buf[..n]) {
                        match output {
                            ParseOutput::Bytes(cow_bytes) => {
                                result.bytes.extend_from_slice(&cow_bytes);
                            }
                            ParseOutput::Marker(marker) => {
                                tracing::debug!(?marker, "Marker detected during edit mode");
                                result.markers.push(marker);
                            }
                        }
                    }
                    self.last_activity_time = Instant::now();
                    // Continue loop to read more data
                }
                Err(nix::errno::Errno::EAGAIN) => {
                    // No more data available, done draining
                    return Ok(read_any);
                }
                Err(nix::errno::Errno::EIO) => {
                    tracing::info!("PTY EIO detected during edit mode read");
                    result.eof = true;
                    return Ok(read_any);
                }
                Err(e) => return Err(e).context("Failed to read PTY in edit mode"),
            }
        }
    }

    // =========================================================================
    // Stdin interception for scroll key handling
    // =========================================================================

    /// Enables or disables stdin interception mode.
    ///
    /// When enabled, stdin bytes are buffered instead of being forwarded to the PTY.
    /// This allows the caller to filter scroll keys (PgUp/PgDown) before forwarding
    /// the remainder to the shell.
    ///
    /// # Arguments
    ///
    /// * `intercept` - `true` to intercept stdin, `false` for normal forwarding
    pub fn set_stdin_intercept(&mut self, intercept: bool) {
        self.stdin_intercept = intercept;
        if !intercept {
            // Clear buffer when disabling intercept
            self.stdin_buffer.clear();
        }
    }

    /// Returns whether stdin interception is currently enabled.
    #[inline]
    pub fn is_stdin_intercepted(&self) -> bool {
        self.stdin_intercept
    }

    /// Takes the buffered stdin bytes, clearing the internal buffer.
    ///
    /// Call this after `run_once()` when stdin interception is enabled to get
    /// the bytes that were read from stdin during this pump cycle.
    ///
    /// # Returns
    ///
    /// The intercepted stdin bytes. Empty if no stdin was read or interception is disabled.
    pub fn take_stdin_buffer(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.stdin_buffer)
    }

    /// Writes bytes to the PTY.
    ///
    /// Use this to forward filtered stdin bytes after scroll key processing.
    /// Only forwards non-scroll keys to the shell.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Bytes to write to the PTY
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_to_pty(&self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        write_all(self.pty_fd, bytes)
    }
}

/// Writes all bytes to a file descriptor, handling partial writes and EINTR.
///
/// This function loops until all bytes are written, retrying on EINTR
/// (interrupted by signal) and advancing the buffer on partial writes.
///
/// # Arguments
///
/// * `fd` - File descriptor to write to
/// * `buf` - Bytes to write
///
/// # Errors
///
/// Returns an error if write fails or returns 0 (broken pipe).
#[inline]
fn write_all(fd: RawFd, mut buf: &[u8]) -> Result<()> {
    while !buf.is_empty() {
        match write(fd, buf) {
            Ok(0) => {
                anyhow::bail!("Write returned 0 (broken pipe)");
            }
            Ok(n) => {
                buf = &buf[n..];
            }
            Err(nix::errno::Errno::EINTR) => {
                // Interrupted by signal (e.g., SIGWINCH), retry
                continue;
            }
            // EAGAIN and EWOULDBLOCK are the same on Linux; handle both for portability
            #[allow(unreachable_patterns)]
            Err(nix::errno::Errno::EAGAIN) | Err(nix::errno::Errno::EWOULDBLOCK) => {
                // Would block - retry the write (mirrors read-path behavior)
                continue;
            }
            Err(e) => {
                return Err(e).context("Write failed");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    /// Test that PumpResult variants are correctly constructed.
    #[test]
    fn test_pump_result_variants() {
        // Single marker with captured bytes
        let marker = PumpResult::MarkerDetected {
            markers: smallvec![MarkerEvent::Prompt],
            captured_bytes: vec![b'h', b'i'],
        };
        assert!(
            matches!(marker, PumpResult::MarkerDetected { ref markers, .. } if markers.len() == 1)
        );
        assert_eq!(marker.captured_bytes(), b"hi");

        // Multiple markers (still inline, no heap allocation)
        let markers = PumpResult::MarkerDetected {
            markers: smallvec![MarkerEvent::Precmd { exit_code: 0 }, MarkerEvent::Prompt,],
            captured_bytes: vec![],
        };
        assert!(
            matches!(markers, PumpResult::MarkerDetected { markers: ref m, .. } if m.len() == 2)
        );

        let cont = PumpResult::Continue {
            captured_bytes: vec![b'x'],
        };
        assert!(matches!(cont, PumpResult::Continue { .. }));
        assert_eq!(cont.captured_bytes(), b"x");

        let eof = PumpResult::PtyEof;
        assert!(matches!(eof, PumpResult::PtyEof));
        assert_eq!(eof.captured_bytes(), b"");
    }

    /// Test that multiple markers in a single read are all captured.
    #[test]
    fn test_multiple_markers_accumulated() {
        // This tests the invariant that no markers are lost when multiple
        // appear in a single PTY read chunk.
        // Note: 3 markers exceeds inline capacity (2), triggers heap allocation
        let events: MarkerVec = smallvec![
            MarkerEvent::Precmd { exit_code: 0 },
            MarkerEvent::Prompt,
            MarkerEvent::Preexec,
        ];
        let result = PumpResult::MarkerDetected {
            markers: events,
            captured_bytes: b"test output".to_vec(),
        };

        if let PumpResult::MarkerDetected {
            markers: captured,
            captured_bytes,
        } = result
        {
            assert_eq!(captured.len(), 3);
            assert_eq!(captured[0], MarkerEvent::Precmd { exit_code: 0 });
            assert_eq!(captured[1], MarkerEvent::Prompt);
            assert_eq!(captured[2], MarkerEvent::Preexec);
            assert_eq!(captured_bytes, b"test output");
        } else {
            panic!("Expected MarkerDetected");
        }
    }

    /// Test PumpResult helper methods.
    #[test]
    fn test_pump_result_helpers() {
        let result = PumpResult::MarkerDetected {
            markers: smallvec![MarkerEvent::Prompt],
            captured_bytes: b"hello".to_vec(),
        };
        assert!(result.markers().is_some());
        assert_eq!(result.markers().unwrap().len(), 1);
        assert_eq!(result.captured_bytes(), b"hello");

        let cont = PumpResult::Continue {
            captured_bytes: b"world".to_vec(),
        };
        assert!(cont.markers().is_none());
        assert_eq!(cont.captured_bytes(), b"world");

        let eof = PumpResult::PtyEof;
        assert!(eof.markers().is_none());
        assert!(eof.captured_bytes().is_empty());
    }

    /// Test Pump construction.
    #[test]
    fn test_pump_new() {
        let token = *b"a1b2c3d4e5f67890";
        // Use a dummy fd (won't actually be used in this test)
        let pump = Pump::new(99, token, None);

        assert_eq!(pump.pty_fd, 99);
        assert!(pump.signal_fd.is_none());
        // Parser should start in normal state
        assert!(!pump.marker_parser.is_mid_sequence());
    }

    /// Test Pump construction with signal fd.
    #[test]
    fn test_pump_new_with_signal_fd() {
        let token = *b"a1b2c3d4e5f67890";
        let pump = Pump::new(99, token, Some(42));

        assert_eq!(pump.pty_fd, 99);
        assert_eq!(pump.signal_fd, Some(42));
    }

    /// Test that buffer size is reasonable.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_buffer_size() {
        assert!(BUFFER_SIZE >= 1024, "Buffer too small");
        assert!(BUFFER_SIZE <= 65536, "Buffer too large");
    }

    /// Test timeout constants.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_timeout_constants() {
        assert!(MID_SEQUENCE_TIMEOUT_MS > 0);
        assert!(MID_SEQUENCE_TIMEOUT_MS < 1000);
        assert!(STALE_SEQUENCE_THRESHOLD.as_millis() >= 50);
        assert!(STALE_SEQUENCE_THRESHOLD.as_millis() <= 500);
    }

    /// Test that run_once_with_timeout accepts Duration parameter.
    /// This is a structural test; full I/O testing is in integration.rs.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_run_once_with_timeout_exists() {
        // Verify the method signature compiles and accepts Duration
        let _: fn(&mut Pump, Option<Duration>) -> Result<PumpResult> = Pump::run_once_with_timeout;

        // Verify constants used in timeout calculation are reasonable
        assert!(MID_SEQUENCE_TIMEOUT_MS > 0);
        assert!(MID_SEQUENCE_TIMEOUT_MS <= 100);
    }

    // Note: Full integration testing of the pump requires a real PTY and is
    // performed in tests/integration.rs. These unit tests verify basic structure
    // and invariants that don't require actual I/O.
}

#[cfg(test)]
mod write_all_tests {
    use super::*;

    /// Test write_all with a pipe (simulates normal write).
    #[test]
    fn test_write_all_success() {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("pipe failed");

        let data = b"Hello, World!";
        write_all(write_fd, data).expect("write_all failed");

        // Read back and verify
        let mut buf = [0u8; 32];
        let n = read(read_fd, &mut buf).expect("read failed");
        assert_eq!(&buf[..n], data);

        // Clean up file descriptors
        nix::unistd::close(read_fd).ok();
        nix::unistd::close(write_fd).ok();
    }

    /// Test write_all with empty buffer (should succeed immediately).
    #[test]
    fn test_write_all_empty() {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("pipe failed");

        // Empty write should succeed without error
        write_all(write_fd, b"").expect("write_all failed for empty");

        // Clean up
        nix::unistd::close(read_fd).ok();
        nix::unistd::close(write_fd).ok();
    }

    /// Test write_all detects broken pipe (closed read end).
    #[test]
    fn test_write_all_broken_pipe() {
        let (read_fd, write_fd) = nix::unistd::pipe().expect("pipe failed");

        // Close read end to cause EPIPE on write
        nix::unistd::close(read_fd).expect("close read_fd failed");

        let result = write_all(write_fd, b"test");
        assert!(result.is_err(), "Expected error on broken pipe");

        nix::unistd::close(write_fd).ok();
    }

    /// Test write_all handles large data (forces multiple write calls due to pipe buffer limits).
    #[test]
    fn test_write_all_large_data() {
        use std::thread;

        let (read_fd, write_fd) = nix::unistd::pipe().expect("pipe failed");

        // 128KB of data - larger than typical pipe buffer (64KB on Linux)
        let large_data: Vec<u8> = (0..131072).map(|i| (i % 256) as u8).collect();
        let expected = large_data.clone();

        // Spawn reader thread to drain the pipe
        let reader = thread::spawn(move || {
            let mut result = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match read(read_fd, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => result.extend_from_slice(&buf[..n]),
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(e) => panic!("read error: {}", e),
                }
            }
            nix::unistd::close(read_fd).ok();
            result
        });

        // Write all data
        write_all(write_fd, &large_data).expect("write_all failed");
        nix::unistd::close(write_fd).expect("close write_fd failed");

        // Verify all data received
        let received = reader.join().expect("reader thread panicked");
        assert_eq!(received.len(), expected.len());
        assert_eq!(received, expected);
    }
}
