//! Application state machine and main event loop.
//!
//! This module orchestrates all Wrashpty functionality by managing the
//! Mode state machine and dispatching events to appropriate handlers.

use anyhow::Result;
use tracing::{debug, info};

use crate::signals::SignalHandler;
use crate::types::SignalEvent;

/// Main application struct coordinating all Wrashpty components.
///
/// `App` owns the signal handler and will own the state machine, PTY,
/// editor, and other components in future implementation phases.
pub struct App {
    /// Signal handler for Unix signal events.
    signal_handler: SignalHandler,
    // TODO: Add state machine fields in future tickets
}

impl App {
    /// Creates a new App instance with all components initialized.
    ///
    /// # Errors
    ///
    /// Returns an error if signal handler registration fails.
    pub fn new() -> Result<Self> {
        let signal_handler = SignalHandler::new()?;
        // TODO: Initialize other components in future tickets

        Ok(Self { signal_handler })
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
                SignalEvent::Shutdown => self.shutdown()?,
            }
        }

        Ok(())
    }

    /// Handles terminal window resize signal.
    fn handle_sigwinch(&mut self) -> Result<()> {
        debug!("SIGWINCH handler called");
        // TODO: Implement in state machine ticket
        // - In Edit mode: delegate to reedline
        // - In Passthrough mode: propagate to PTY
        Ok(())
    }

    /// Handles child process exit signal.
    fn handle_sigchld(&mut self) -> Result<()> {
        debug!("SIGCHLD handler called");
        // TODO: Implement in state machine ticket
        // - Check if shell has exited
        // - Begin shutdown if shell is gone
        Ok(())
    }

    /// Initiates graceful shutdown.
    fn shutdown(&mut self) -> Result<()> {
        info!("Shutdown initiated");
        // TODO: Implement in state machine ticket
        // - Transition to Terminating mode
        // - Clean up resources
        // - Restore terminal state
        Ok(())
    }

    /// Returns whether the application should shut down.
    pub fn should_shutdown(&self) -> bool {
        self.signal_handler.should_shutdown()
    }
}
