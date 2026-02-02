//! Wrashpty - A readline wrapper for bash with modern line editing.
//!
//! This library provides the core components for terminal I/O, PTY management,
//! and shell integration. The binary crate (`main.rs`) uses these components
//! to build the full application.

// These modules are foundational; full usage comes in future implementation phases.
#![allow(dead_code)]

pub mod marker;
pub mod pty;
pub mod pump;
pub mod safety;
pub mod terminal;
pub mod types;

// Application modules - public for binary crate access
pub mod app;
pub mod bashrc;
mod chrome;
mod complete;
pub mod editor;
pub mod git;
pub mod history;
pub mod prompt;
pub mod signals;
mod suggest;

// Re-export ChromeContext for use in app.rs
pub use chrome::ChromeContext;
