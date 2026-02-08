//! Scrollback subsystem for terminal-agnostic capture and rendering.
//! Provides ring-buffer storage, PTY capture parsing, and scroll-view rendering.
//! See `docs/scrollback.md` for architecture and usage details.

mod alt_screen;
mod ansi;
mod boundaries;
mod buffer;
mod capture;
pub mod features;
mod mini_input;
mod mode;
mod state;
mod viewer;

pub use alt_screen::{AltScreenDetector, AltScreenEvent};
pub use ansi::sanitize_for_display;
pub use boundaries::CommandBoundaries;
pub use buffer::{ScrollLine, ScrollbackBuffer};
pub use capture::{CaptureState, CapturedLine};
pub use mini_input::{MiniInput, MiniInputResult};
pub use mode::ScrollViewMode;
pub use state::{DisplaySettings, ViewerState};
pub use viewer::{RenderOptions, RenderStats, ScrollViewer};
