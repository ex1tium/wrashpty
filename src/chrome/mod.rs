//! Chrome layer for status bar, scroll regions, and expandable panels.
//!
//! This module manages the visual chrome (context bar, panels) using terminal
//! scroll regions to reserve screen real estate outside the shell area.

pub mod buffer_convert;
pub mod command_palette;
pub mod core;
pub mod file_browser;
pub mod help_panel;
pub mod history_browser;
pub mod panel;
pub mod tabbed_panel;

// Re-export core types for backward compatibility
pub use core::{Chrome, ChromeContext, NotificationStyle, SizeCheckResult};
