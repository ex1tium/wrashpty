//! Internal scrollback buffer system for terminal-emulator-independent scrolling.
//!
//! This module provides scrollback functionality that works regardless of the
//! terminal emulator being used. It captures PTY output into a ring buffer and
//! allows seamless PgUp/PgDown scrolling without explicit mode toggling.
//!
//! # Architecture
//!
//! The scrollback system is orthogonal to the main Mode state machine, similar
//! to ChromeMode. It consists of:
//!
//! - [`ScrollbackBuffer`] - Ring buffer storing captured terminal lines
//! - [`CaptureState`] - Streaming line parser for PTY output
//! - [`AltScreenDetector`] - Detects alternate screen buffer (vim, htop) to suspend capture
//! - [`ScrollViewer`] - Stateless renderer for scrollback content
//! - [`ViewerState`] - Consolidated state for scroll viewer modes and display settings
//! - [`CommandBoundaries`] - Index for command boundary navigation (Ctrl+P/N)
//!
//! # Usage
//!
//! The scrollback system integrates at two points:
//!
//! 1. **Capture**: PTY output is fed to `CaptureState` which parses lines
//!    and stores them in `ScrollbackBuffer`
//! 2. **Viewing**: PgUp/PgDown keys trigger scrollback rendering via `ScrollViewer`
//!
//! When the user presses any non-scroll key while scrolled back, the system
//! returns to live view and forwards the key to the shell.

mod alt_screen;
mod boundaries;
mod buffer;
mod capture;
pub mod features;
mod mini_input;
mod mode;
mod state;
mod viewer;

pub use alt_screen::{AltScreenDetector, AltScreenEvent};
pub use boundaries::CommandBoundaries;
pub use buffer::{ScrollLine, ScrollbackBuffer};
pub use capture::CaptureState;
pub use mini_input::{MiniInput, MiniInputResult};
pub use mode::ScrollViewMode;
pub use state::{DisplaySettings, ViewerState};
pub use viewer::{RenderOptions, RenderStats, ScrollViewer};
