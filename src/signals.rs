//! Signal handling for Wrashpty.
//!
//! This module provides async-signal-safe signal handling using the `signal-hook`
//! crate's self-pipe pattern. Signals are converted to [`SignalEvent`] variants
//! for processing by the main event loop, avoiding direct signal handler complexity.
//!
//! # Handled Signals
//!
//! - `SIGWINCH`: Terminal window resize
//! - `SIGCHLD`: Child process state change
//! - `SIGTERM`, `SIGINT`, `SIGHUP`: Termination requests
//!
//! # Safety
//!
//! All signal handling is async-signal-safe through `signal-hook`'s self-pipe
//! mechanism. The actual signal handlers only write a byte to a pipe; all
//! processing happens in the main thread via [`SignalHandler::check_signals`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info};
use signal_hook::consts::signal::{SIGCHLD, SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::types::SignalEvent;

/// Handles Unix signal delivery via async-signal-safe self-pipe pattern.
///
/// `SignalHandler` uses the `signal-hook` crate to safely capture Unix signals
/// and convert them to events for main loop processing. This avoids the
/// complexity and pitfalls of writing async-signal-safe code directly.
///
/// # Example
///
/// ```no_run
/// use wrashpty::signals::SignalHandler;
///
/// let mut handler = SignalHandler::new().expect("Failed to register signals");
///
/// // In main event loop:
/// for event in handler.check_signals() {
///     // Process signal events...
/// }
///
/// if handler.should_shutdown() {
///     // Begin graceful shutdown...
/// }
/// ```
pub struct SignalHandler {
    /// Signal iterator using self-pipe for async-signal-safe delivery.
    signals: Signals,

    /// Shutdown flag set when termination signal is received.
    /// Shared via Arc to allow checking from multiple locations.
    shutdown_flag: Arc<AtomicBool>,
}

impl SignalHandler {
    /// Creates a new signal handler and registers for relevant signals.
    ///
    /// Registers handlers for:
    /// - `SIGWINCH`: Terminal resize
    /// - `SIGCHLD`: Child process events
    /// - `SIGTERM`, `SIGINT`, `SIGHUP`: Termination signals
    ///
    /// # Errors
    ///
    /// Returns an error if signal registration fails, which can happen if:
    /// - The signal is already handled by another mechanism
    /// - System resources are exhausted
    pub fn new() -> Result<Self> {
        let signals = Signals::new([SIGWINCH, SIGCHLD, SIGTERM, SIGINT, SIGHUP])
            .context("Failed to register signal handlers")?;

        let shutdown_flag = Arc::new(AtomicBool::new(false));

        info!("Signal handlers registered");

        Ok(Self {
            signals,
            shutdown_flag,
        })
    }

    /// Checks for pending signals and returns them as events.
    ///
    /// This method is non-blocking and returns all signals that have been
    /// delivered since the last call. Each signal is converted to the
    /// appropriate [`SignalEvent`] variant.
    ///
    /// Termination signals (`SIGTERM`, `SIGINT`, `SIGHUP`) also set the
    /// internal shutdown flag, queryable via [`should_shutdown`](Self::should_shutdown).
    ///
    /// # Returns
    ///
    /// A vector of signal events, empty if no signals are pending.
    pub fn check_signals(&mut self) -> Vec<SignalEvent> {
        let mut events = Vec::new();

        for signal in self.signals.pending() {
            match signal {
                SIGWINCH => {
                    debug!("SIGWINCH received");
                    events.push(SignalEvent::WindowResize);
                }
                SIGCHLD => {
                    debug!("SIGCHLD received");
                    events.push(SignalEvent::ChildExit);
                }
                SIGTERM | SIGINT | SIGHUP => {
                    info!("Termination signal received: {}", signal);
                    self.shutdown_flag.store(true, Ordering::SeqCst);
                    events.push(SignalEvent::Shutdown);
                }
                _ => {
                    // Ignore unexpected signals (defensive programming)
                }
            }
        }

        events
    }

    /// Returns whether a shutdown signal has been received.
    ///
    /// This method is thread-safe and uses sequential consistency ordering
    /// to ensure visibility across threads.
    ///
    /// # Returns
    ///
    /// `true` if `SIGTERM`, `SIGINT`, or `SIGHUP` has been received.
    pub fn should_shutdown(&self) -> bool {
        self.shutdown_flag.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::getpid;
    use serial_test::serial;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_signal_handler_creation() {
        let handler = SignalHandler::new().unwrap();
        assert!(!handler.should_shutdown());
    }

    #[test]
    #[serial]
    fn test_sigwinch_detection() {
        let mut handler = SignalHandler::new().unwrap();

        // Send SIGWINCH to ourselves
        kill(getpid(), Signal::SIGWINCH).unwrap();

        // Allow time for signal delivery
        thread::sleep(Duration::from_millis(10));

        let events = handler.check_signals();
        assert!(events.contains(&SignalEvent::WindowResize));
    }

    #[test]
    #[serial]
    fn test_shutdown_signal() {
        let mut handler = SignalHandler::new().unwrap();

        // Send SIGTERM to ourselves
        kill(getpid(), Signal::SIGTERM).unwrap();

        // Allow time for signal delivery
        thread::sleep(Duration::from_millis(10));

        let events = handler.check_signals();
        assert!(events.contains(&SignalEvent::Shutdown));
        assert!(handler.should_shutdown());
    }
}
