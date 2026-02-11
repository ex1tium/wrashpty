//! Chrome layer for status bar, scroll regions, and expandable panels.
//!
//! This module manages the visual chrome (context bar, panels) using terminal
//! scroll regions to reserve screen real estate outside the shell area.

pub mod buffer_convert;
pub mod command_edit;
pub mod command_palette;
pub mod commands_panel;
pub mod core;
pub mod file_browser;
pub mod file_tree;
pub mod footer_bar;
pub mod help_panel;
pub mod history_browser;
pub mod panel;
pub mod schema_browser;
pub mod segments;
pub mod symbols;
pub mod tabbed_panel;
pub mod theme;

// Re-export core types for convenience
pub use core::{Chrome, NotificationStyle, SizeCheckResult};
pub use segments::{GitInfo, ScrollInfo, TopbarState};

/// Test utilities for normalizing strings in assertions (e.g., ANSI stripping).
#[cfg(test)]
pub(crate) mod test_utils {
    /// Strip ANSI escape sequences for test assertions.
    pub(crate) fn strip_ansi_for_test(s: &str) -> String {
        let mut result = String::new();
        let mut chars = s.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                result.push(ch);
            }
        }
        result
    }
}
