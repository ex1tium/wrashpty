//! Chrome layer for status bar, scroll regions, and expandable panels.
//!
//! This module manages the visual chrome (context bar, panels) using terminal
//! scroll regions to reserve screen real estate outside the shell area.

pub mod buffer_convert;
pub mod command_edit;
pub mod command_knowledge;
pub mod command_palette;
pub mod core;
pub mod file_browser;
pub mod help_panel;
pub mod history_browser;
pub mod panel;
pub mod symbols;
pub mod tabbed_panel;
pub mod theme;

// Re-export core types for convenience
pub use core::{Chrome, ChromeContext, NotificationStyle, SizeCheckResult};
