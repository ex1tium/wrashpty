//! Unified scroll view state centered on [`ViewerState`].
//! Keeps mode, display toggles, and command boundary metadata together.

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
    /// Show horizontal rule separators at command boundaries (Ctrl+B toggle).
    pub command_separators: bool,
}

impl DisplaySettings {
    /// Creates new settings with defaults.
    pub fn new() -> Self {
        Self {
            url_highlighting: true,   // On by default
            command_separators: true, // On by default
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

    /// Returns true if line numbers are currently shown.
    pub fn is_line_numbers_shown(&self) -> bool {
        self.display.line_numbers
    }

    /// Deprecated alias for [`Self::is_line_numbers_shown`].
    #[deprecated(note = "Use is_line_numbers_shown() instead.")]
    pub fn show_line_numbers(&self) -> bool {
        self.is_line_numbers_shown()
    }

    /// Toggles line number display.
    pub fn toggle_line_numbers(&mut self) {
        self.display.line_numbers = !self.display.line_numbers;
    }

    /// Returns true if timestamps are currently shown.
    pub fn is_timestamps_shown(&self) -> bool {
        self.display.timestamps
    }

    /// Deprecated alias for [`Self::is_timestamps_shown`].
    #[deprecated(note = "Use is_timestamps_shown() instead.")]
    pub fn show_timestamps(&self) -> bool {
        self.is_timestamps_shown()
    }

    /// Toggles timestamp display.
    pub fn toggle_timestamps(&mut self) {
        self.display.timestamps = !self.display.timestamps;
    }

    /// Returns true if the help bar is currently shown.
    pub fn is_help_bar_shown(&self) -> bool {
        self.display.help_bar
    }

    /// Deprecated alias for [`Self::is_help_bar_shown`].
    #[deprecated(note = "Use is_help_bar_shown() instead.")]
    pub fn show_help_bar(&self) -> bool {
        self.is_help_bar_shown()
    }

    /// Toggles help bar display.
    pub fn toggle_help_bar(&mut self) {
        self.display.help_bar = !self.display.help_bar;
    }

    /// Returns true if command separators are currently shown.
    pub fn is_command_separators_shown(&self) -> bool {
        self.display.command_separators
    }

    /// Deprecated alias for [`Self::is_command_separators_shown`].
    #[deprecated(note = "Use is_command_separators_shown() instead.")]
    pub fn show_command_separators(&self) -> bool {
        self.is_command_separators_shown()
    }

    /// Toggles command separator display.
    pub fn toggle_command_separators(&mut self) {
        self.display.command_separators = !self.display.command_separators;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_settings_default_values_are_expected() {
        let settings = DisplaySettings::new();
        assert!(!settings.line_numbers);
        assert!(!settings.timestamps);
        assert!(!settings.help_bar);
        assert!(settings.url_highlighting); // On by default
        assert!(settings.command_separators); // On by default
    }

    #[test]
    fn test_viewer_state_toggle_line_numbers_updates_state() {
        let mut state = ViewerState::new();

        assert!(!state.is_line_numbers_shown());
        state.toggle_line_numbers();
        assert!(state.is_line_numbers_shown());
    }

    #[test]
    fn test_viewer_state_toggle_timestamps_updates_state() {
        let mut state = ViewerState::new();

        assert!(!state.is_timestamps_shown());
        state.toggle_timestamps();
        assert!(state.is_timestamps_shown());
    }

    #[test]
    fn test_reset_mode_from_search_returns_normal() {
        use super::super::features::SearchState;

        let mut state = ViewerState::new();
        state.mode = ScrollViewMode::Search(SearchState::default());

        state.reset_mode();
        assert!(matches!(state.mode, ScrollViewMode::Normal));
    }
}
