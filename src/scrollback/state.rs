//! Unified state management for the scroll viewer.
//!
//! This module consolidates all scroll viewer state into a single struct,
//! making it easy to pass around and manage during the scroll view loop.

use super::boundaries::CommandBoundaries;
use super::mode::ScrollViewMode;

/// Display settings that persist across mode changes.
#[derive(Debug, Clone, Default)]
pub struct DisplaySettings {
    /// Show line numbers in left gutter (Ctrl+L toggle).
    pub line_numbers: bool,
    /// Show relative timestamps in gutter (Ctrl+T toggle).
    pub timestamps: bool,
    /// Show help bar at bottom of screen (? or F1 toggle).
    pub help_bar: bool,
    /// Enable URL highlighting (default: true).
    pub url_highlighting: bool,
}

impl DisplaySettings {
    /// Creates new settings with defaults.
    pub fn new() -> Self {
        Self {
            url_highlighting: true, // On by default
            ..Default::default()
        }
    }
}

/// Complete state for the scroll viewer.
///
/// This struct consolidates all scroll viewer state that was previously
/// scattered across multiple fields in App. It owns the modal state,
/// display settings, and optional command boundary index.
#[derive(Debug, Default)]
pub struct ViewerState {
    /// Current modal mode (Normal, Search, Filter, etc.).
    pub mode: ScrollViewMode,
    /// Display toggle settings.
    pub display: DisplaySettings,
    /// Command boundary index for Ctrl+P/N navigation.
    /// Lazily populated when markers are detected.
    pub boundaries: CommandBoundaries,
    /// Cached search result line indices from last search.
    /// Used by filter mode to show only search result lines.
    pub last_search_lines: Option<Vec<usize>>,
}

impl ViewerState {
    /// Creates a new viewer state with default settings.
    pub fn new() -> Self {
        Self {
            display: DisplaySettings::new(),
            ..Default::default()
        }
    }

    /// Resets mode to Normal (called when exiting special modes).
    pub fn reset_mode(&mut self) {
        self.mode = ScrollViewMode::Normal;
    }

    /// Returns true if currently showing line numbers.
    pub fn show_line_numbers(&self) -> bool {
        self.display.line_numbers
    }

    /// Toggles line number display.
    pub fn toggle_line_numbers(&mut self) {
        self.display.line_numbers = !self.display.line_numbers;
    }

    /// Returns true if currently showing timestamps.
    pub fn show_timestamps(&self) -> bool {
        self.display.timestamps
    }

    /// Toggles timestamp display.
    pub fn toggle_timestamps(&mut self) {
        self.display.timestamps = !self.display.timestamps;
    }

    /// Returns true if help bar is shown.
    pub fn show_help_bar(&self) -> bool {
        self.display.help_bar
    }

    /// Toggles help bar display.
    pub fn toggle_help_bar(&mut self) {
        self.display.help_bar = !self.display.help_bar;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_settings_default() {
        let settings = DisplaySettings::new();
        assert!(!settings.line_numbers);
        assert!(!settings.timestamps);
        assert!(!settings.help_bar);
        assert!(settings.url_highlighting); // On by default
    }

    #[test]
    fn test_viewer_state_toggles() {
        let mut state = ViewerState::new();

        assert!(!state.show_line_numbers());
        state.toggle_line_numbers();
        assert!(state.show_line_numbers());

        assert!(!state.show_timestamps());
        state.toggle_timestamps();
        assert!(state.show_timestamps());
    }

    #[test]
    fn test_reset_mode() {
        use super::super::features::SearchState;

        let mut state = ViewerState::new();
        state.mode = ScrollViewMode::Search(SearchState::default());

        state.reset_mode();
        assert!(matches!(state.mode, ScrollViewMode::Normal));
    }
}
