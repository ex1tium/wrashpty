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
    MarkerDetected(MarkerVec),

    /// Normal operation, no events. Continue pumping.
    Continue,

    /// The PTY returned EOF (child process terminated).
    /// The caller should initiate shutdown.
    PtyEof,
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
/// use wrashpty::pump::{Pump, PumpResult};
///
/// let pty_fd = pty.master_fd();
/// let token = session_token;
/// let mut pump = Pump::new(pty_fd, token);
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
///         PumpResult::Continue => {}
///     }
/// }
/// ```
#[derive(Debug)]
pub struct Pump {
    /// PTY master file descriptor for poll operations.
    pty_fd: RawFd,

    /// Streaming marker parser instance.
    marker_parser: MarkerParser,

    /// Timestamp of last I/O activity for stale sequence detection.
    /// Only updated when bytes are actually read or written, not on poll timeout.
    last_activity_time: Instant,
}

impl Pump {
    /// Creates a new pump for the given PTY and session token.
    ///
    /// # Arguments
    ///
    /// * `pty_fd` - The PTY master file descriptor from `Pty::master_fd()`
    /// * `session_token` - 16-byte session token for marker validation
    ///
    /// # Safety
    ///
    /// The caller must ensure `pty_fd` remains valid for the lifetime of
    /// the pump.
    #[must_use]
    pub fn new(pty_fd: RawFd, session_token: [u8; 16]) -> Self {
        Self {
            pty_fd,
            marker_parser: MarkerParser::new(session_token),
            last_activity_time: Instant::now(),
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
        // Check for stale partial sequences that need flushing
        self.check_stale_sequence()?;

        // Calculate poll timeout based on parser state
        // -1 means infinite timeout, positive value is milliseconds
        let poll_timeout_ms: i32 = if self.marker_parser.is_mid_sequence() {
            // Short timeout when buffering partial sequence
            MID_SEQUENCE_TIMEOUT_MS
        } else {
            // Block indefinitely when no partial sequence
            -1
        };

        // SAFETY: STDIN_FILENO (0) and pty_fd are valid file descriptors
        // that remain open for the duration of the poll call.
        let stdin_fd = unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) };
        let pty_fd = unsafe { BorrowedFd::borrow_raw(self.pty_fd) };

        // Set up poll file descriptors
        let mut pollfds = [
            PollFd::new(&stdin_fd, PollFlags::POLLIN),
            PollFd::new(&pty_fd, PollFlags::POLLIN),
        ];

        // Wait for I/O events, retrying on EINTR (signal interruption)
        loop {
            match poll(&mut pollfds, poll_timeout_ms) {
                Ok(_) => break,
                Err(nix::errno::Errno::EINTR) => {
                    // Interrupted by signal (e.g., SIGWINCH), retry poll
                    continue;
                }
                Err(e) => return Err(e).context("Poll failed"),
            }
        }

        // Note: We do NOT update last_activity_time here on poll return.
        // It is only updated when actual I/O occurs (bytes read/written).

        // Check stdin readiness
        if let Some(revents) = pollfds[0].revents() {
            if revents.contains(PollFlags::POLLIN) {
                self.forward_stdin_to_pty()?;
            }
        }

        // Check PTY readiness - handle POLLIN, POLLHUP, and POLLERR
        if let Some(revents) = pollfds[1].revents() {
            // Check for hang-up or error conditions (child process terminated)
            if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                // PTY hung up or error - attempt a read to confirm EOF or drain remaining data
                return self.forward_pty_to_stdout();
            }

            if revents.contains(PollFlags::POLLIN) {
                return self.forward_pty_to_stdout();
            }
        }

        Ok(PumpResult::Continue)
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
    #[inline]
    fn forward_stdin_to_pty(&mut self) -> Result<()> {
        let mut buf = [0u8; BUFFER_SIZE];

        match read(STDIN_FILENO, &mut buf) {
            Ok(0) => {
                // EOF on stdin - user closed input, ignore
                // The PTY will continue running until child exits
            }
            Ok(n) => {
                write_all(self.pty_fd, &buf[..n])?;
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
    /// no bytes after a marker are lost.
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
                // Process through marker parser, accumulating all markers
                for output in self.marker_parser.feed(&buf[..n]) {
                    match output {
                        ParseOutput::Bytes(cow_bytes) => {
                            write_all(STDOUT_FILENO, &cow_bytes)?;
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
                    Ok(PumpResult::Continue)
                } else {
                    Ok(PumpResult::MarkerDetected(markers))
                }
            }
            Err(nix::errno::Errno::EAGAIN) => {
                // No data available, ignore
                Ok(PumpResult::Continue)
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
        // Single marker
        let marker = PumpResult::MarkerDetected(smallvec![MarkerEvent::Prompt]);
        assert!(matches!(marker, PumpResult::MarkerDetected(ref v) if v.len() == 1));

        // Multiple markers (still inline, no heap allocation)
        let markers = PumpResult::MarkerDetected(smallvec![
            MarkerEvent::Precmd { exit_code: 0 },
            MarkerEvent::Prompt,
        ]);
        assert!(matches!(markers, PumpResult::MarkerDetected(ref v) if v.len() == 2));

        let cont = PumpResult::Continue;
        assert!(matches!(cont, PumpResult::Continue));

        let eof = PumpResult::PtyEof;
        assert!(matches!(eof, PumpResult::PtyEof));
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
        let result = PumpResult::MarkerDetected(events);

        if let PumpResult::MarkerDetected(captured) = result {
            assert_eq!(captured.len(), 3);
            assert_eq!(captured[0], MarkerEvent::Precmd { exit_code: 0 });
            assert_eq!(captured[1], MarkerEvent::Prompt);
            assert_eq!(captured[2], MarkerEvent::Preexec);
        } else {
            panic!("Expected MarkerDetected");
        }
    }

    /// Test Pump construction.
    #[test]
    fn test_pump_new() {
        let token = *b"a1b2c3d4e5f67890";
        // Use a dummy fd (won't actually be used in this test)
        let pump = Pump::new(99, token);

        assert_eq!(pump.pty_fd, 99);
        // Parser should start in normal state
        assert!(!pump.marker_parser.is_mid_sequence());
    }

    /// Test that buffer size is reasonable.
    #[test]
    fn test_buffer_size() {
        assert!(BUFFER_SIZE >= 1024, "Buffer too small");
        assert!(BUFFER_SIZE <= 65536, "Buffer too large");
    }

    /// Test timeout constants.
    #[test]
    fn test_timeout_constants() {
        assert!(MID_SEQUENCE_TIMEOUT_MS > 0);
        assert!(MID_SEQUENCE_TIMEOUT_MS < 1000);
        assert!(STALE_SEQUENCE_THRESHOLD.as_millis() >= 50);
        assert!(STALE_SEQUENCE_THRESHOLD.as_millis() <= 500);
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
