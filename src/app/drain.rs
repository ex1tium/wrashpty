//! Background PTY drain thread for Edit mode.
//!
//! When reedline blocks waiting for user input, background jobs might produce
//! output that would otherwise back up in the PTY buffer. This module provides
//! a drain thread that continuously reads PTY output into a bounded channel.

use std::os::unix::io::{BorrowedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread::JoinHandle;

use nix::poll::{PollFd, PollFlags, poll};
use nix::unistd::read;
use tracing::warn;

/// Poll interval for background PTY drain during Edit mode (milliseconds).
pub(super) const EDIT_MODE_DRAIN_POLL_MS: i32 = 50;

/// Buffer size for background PTY drain reads.
pub(super) const DRAIN_BUFFER_SIZE: usize = 4096;

/// Maximum number of drain results to buffer in the channel.
/// With 4KB per chunk, this caps memory at ~16MB for the channel.
/// This accommodates verbose background jobs (builds, find, logs) while
/// still preventing OOM from runaway output. When full, newest chunks
/// are dropped to prevent blocking PTY reads.
pub(super) const DRAIN_CHANNEL_CAPACITY: usize = 4096;

/// Result from the background PTY drain thread.
pub(super) struct DrainResult {
    /// Bytes read from the PTY.
    pub bytes: Vec<u8>,
    /// Whether EOF was detected.
    pub eof: bool,
    /// Whether the drain loop stopped due to local stop flag (not PTY EOF).
    pub drain_stopped: bool,
    /// Number of bytes dropped due to channel backpressure before this chunk.
    pub dropped_bytes: usize,
}

/// RAII guard for the background PTY drain thread.
///
/// Ensures the drain thread is stopped and joined on all exit paths,
/// including when `read_line` returns an error. This prevents leaking
/// a live PTY reader thread.
pub(super) struct DrainGuard {
    /// Flag to signal the drain thread to stop.
    stop_flag: Arc<AtomicBool>,
    /// Handle to the drain thread (Option to allow taking in drop).
    handle: Option<JoinHandle<()>>,
}

impl DrainGuard {
    /// Creates a new drain guard with the given stop flag and thread handle.
    pub fn new(stop_flag: Arc<AtomicBool>, handle: JoinHandle<()>) -> Self {
        Self {
            stop_flag,
            handle: Some(handle),
        }
    }

    /// Stops the drain thread and waits for it to finish.
    ///
    /// This is called automatically on drop, but can be called explicitly
    /// if you need to ensure the thread is stopped before proceeding.
    pub fn stop(&mut self) {
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
pub(super) fn pty_drain_loop(pty_fd: RawFd, stop: Arc<AtomicBool>, tx: SyncSender<DrainResult>) {
    fn send_final_result(
        tx: &SyncSender<DrainResult>,
        dropped_bytes: usize,
        eof: bool,
        drain_stopped: bool,
    ) {
        let final_result = DrainResult {
            bytes: Vec::new(),
            eof,
            drain_stopped,
            dropped_bytes,
        };
        let _ = tx.try_send(final_result);
    }

    let mut buf = [0u8; DRAIN_BUFFER_SIZE];
    let mut pending_dropped_bytes: usize = 0;
    let mut final_result_sent = false;

    while !stop.load(Ordering::Relaxed) {
        // SAFETY: pty_fd is valid for the duration of Edit mode
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        let mut pollfds = [PollFd::new(&borrowed_fd, PollFlags::POLLIN)];

        // Poll with short timeout to check stop flag periodically
        match poll(&mut pollfds, EDIT_MODE_DRAIN_POLL_MS) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                warn!("Edit-mode PTY drain poll failed: {}", e);
                send_final_result(&tx, pending_dropped_bytes, true, false);
                final_result_sent = true;
                break;
            }
        }

        if let Some(revents) = pollfds[0].revents() {
            if revents.contains(PollFlags::POLLHUP) || revents.contains(PollFlags::POLLERR) {
                // EOF detected - notify receiver, then stop.
                send_final_result(&tx, pending_dropped_bytes, true, false);
                final_result_sent = true;
                break;
            }

            if revents.contains(PollFlags::POLLIN) {
                // Drain all available data
                loop {
                    match read(pty_fd, &mut buf) {
                        Ok(0) => {
                            // EOF - use try_send to avoid blocking
                            send_final_result(&tx, pending_dropped_bytes, true, false);
                            return;
                        }
                        Ok(n) => {
                            let result = DrainResult {
                                bytes: buf[..n].to_vec(),
                                eof: false,
                                drain_stopped: false,
                                dropped_bytes: pending_dropped_bytes,
                            };
                            // Use try_send for backpressure - don't block PTY reads
                            match tx.try_send(result) {
                                Ok(()) => {
                                    // Successfully sent, reset dropped counter
                                    pending_dropped_bytes = 0;
                                }
                                Err(TrySendError::Full(dropped)) => {
                                    // Channel full - drop this chunk and track bytes
                                    pending_dropped_bytes =
                                        pending_dropped_bytes.saturating_add(dropped.bytes.len());
                                }
                                Err(TrySendError::Disconnected(_)) => {
                                    // Receiver gone, stop draining
                                    return;
                                }
                            }
                        }
                        Err(nix::errno::Errno::EAGAIN) => break,
                        Err(nix::errno::Errno::EIO) => {
                            // EIO means PTY closed - use try_send to avoid blocking
                            send_final_result(&tx, pending_dropped_bytes, true, false);
                            return;
                        }
                        Err(e) => {
                            warn!("Edit-mode PTY drain read failed: {}", e);
                            send_final_result(&tx, pending_dropped_bytes, true, false);
                            return;
                        }
                    }
                }
            }
        }
    }

    // Stop-flag termination should still notify the receiver that draining ended.
    if !final_result_sent {
        send_final_result(&tx, pending_dropped_bytes, false, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::Duration;

    use nix::unistd::{close, pipe, write};

    #[test]
    fn test_pty_drain_loop_eof_on_pipe_close_returns_final_result() {
        let (read_fd, write_fd) = pipe().expect("pipe should be created");
        let (tx, rx) = mpsc::sync_channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            pty_drain_loop(read_fd.as_raw_fd(), stop_clone, tx);
        });

        close(write_fd).expect("write end should close");
        let result = rx
            .recv_timeout(Duration::from_millis((EDIT_MODE_DRAIN_POLL_MS * 4) as u64))
            .expect("expected drain result after EOF");
        assert!(result.eof);
        assert!(!result.drain_stopped);
        assert!(result.bytes.is_empty());

        stop.store(true, Ordering::Relaxed);
        handle.join().expect("drain thread should join");
    }

    #[test]
    fn test_pty_drain_loop_backpressure_reports_dropped_on_next_success() {
        let (read_fd, write_fd) = pipe().expect("pipe should be created");
        let (tx, rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            pty_drain_loop(read_fd.as_raw_fd(), stop_clone, tx);
        });

        let first = b"first";
        let dropped = b"second";
        let third = b"third";

        write(write_fd, first).expect("first write should succeed");
        thread::sleep(Duration::from_millis((EDIT_MODE_DRAIN_POLL_MS * 2) as u64));
        write(write_fd, dropped).expect("second write should succeed");
        thread::sleep(Duration::from_millis((EDIT_MODE_DRAIN_POLL_MS * 2) as u64));

        let first_result = rx
            .recv_timeout(Duration::from_millis((EDIT_MODE_DRAIN_POLL_MS * 4) as u64))
            .expect("first result should be delivered");
        assert_eq!(first_result.bytes, first);
        assert_eq!(first_result.dropped_bytes, 0);
        assert!(!first_result.eof);
        assert!(!first_result.drain_stopped);

        write(write_fd, third).expect("third write should succeed");
        thread::sleep(Duration::from_millis((EDIT_MODE_DRAIN_POLL_MS * 2) as u64));

        let next_result = rx
            .recv_timeout(Duration::from_millis((EDIT_MODE_DRAIN_POLL_MS * 4) as u64))
            .expect("next successful send should be delivered");
        assert_eq!(next_result.bytes, third);
        assert_eq!(next_result.dropped_bytes, dropped.len());
        assert!(!next_result.eof);
        assert!(!next_result.drain_stopped);

        close(write_fd).expect("write end should close");
        stop.store(true, Ordering::Relaxed);
        handle.join().expect("drain thread should join");
    }

    #[test]
    fn test_pty_drain_loop_stop_flag_sends_final_stop_result() {
        let (read_fd, _write_fd) = pipe().expect("pipe should be created");
        let (tx, rx) = mpsc::sync_channel(2);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            pty_drain_loop(read_fd.as_raw_fd(), stop_clone, tx);
        });

        stop.store(true, Ordering::Relaxed);

        let result = rx
            .recv_timeout(Duration::from_millis((EDIT_MODE_DRAIN_POLL_MS * 4) as u64))
            .expect("final stop result should be sent on stop");
        assert!(!result.eof);
        assert!(result.drain_stopped);
        assert!(result.bytes.is_empty());

        handle.join().expect("drain thread should join");

        match rx.recv_timeout(Duration::from_millis(20)) {
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
            Ok(_) => panic!("unexpected extra drain result"),
        }
    }
}
