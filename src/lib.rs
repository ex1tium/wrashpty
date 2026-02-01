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

// Internal modules - not part of the public API
mod app;
mod bashrc;
mod chrome;
mod complete;
mod editor;
mod history;
mod prompt;
mod signals;
mod suggest;
