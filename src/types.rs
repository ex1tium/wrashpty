//! Shared types used across all Wrashpty modules.
//!
//! This module defines the core enums and types that form the foundation of the
//! application's state machine and event handling. By centralizing these types,
//! we prevent circular dependencies between modules.

// These types are foundational; usage comes in future implementation phases.
#![allow(dead_code)]

/// The main operational mode of Wrashpty.
///
/// The application transitions between these modes based on user input and
/// shell events. This forms the core state machine driving all behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Initial startup phase before the shell is ready.
    /// Terminal setup and bashrc injection occur in this mode.
    Initializing,

    /// Interactive line editing mode using reedline.
    /// User is composing a command with full editing capabilities.
    Edit,

    /// Transparent passthrough mode during command execution.
    /// All input/output flows directly between user and shell.
    Passthrough,

    /// Injecting a command into the shell.
    /// Used when submitting an edited command line to bash.
    Injecting,

    /// Graceful shutdown in progress.
    /// Cleaning up resources and restoring terminal state.
    Terminating,
}

/// Chrome display mode controlling the UI overlay.
///
/// Chrome (status bars, scroll regions) is orthogonal to the main Mode
/// state machine. It can be toggled independently of the editing state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeMode {
    /// No chrome displayed - full terminal passthrough.
    /// Used for maximum compatibility or user preference.
    Headless,

    /// Full chrome with top status bar and footer.
    /// Provides visual context and application branding.
    Full,
}

/// Events parsed from OSC 777 markers in shell output.
///
/// These markers are injected via the generated bashrc and signal
/// shell state transitions that drive the Wrashpty state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerEvent {
    /// Shell prompt is about to be displayed.
    /// Contains the exit code of the previous command.
    Precmd {
        /// Exit code from the last executed command (0 = success).
        exit_code: i32,
    },

    /// Prompt string has been fully rendered.
    /// Signals transition from Passthrough to Edit mode.
    Prompt,

    /// Command is about to be executed.
    /// Signals transition from Edit/Injecting to Passthrough mode.
    Preexec,
}
