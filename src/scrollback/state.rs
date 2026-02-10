//! Unified scroll view state centered on [`ViewerState`].
//! Keeps display toggles and command boundary metadata together.

use std::collections::HashSet;

use super::boundaries::CommandBoundaries;
use super::separator::SeparatorRegistry;

/// Display settings that persist across mode changes.
#[derive(Debug, Clone, Default)]
pub struct DisplaySettings {
    /// Show line numbers in left gutter (Ctrl+L toggle).
    pub line_numbers: bool,
    /// Show relative timestamps in gutter (Ctrl+T toggle).
    pub timestamps: bool,
    /// Show help bar at bottom of screen (? or F1 toggle).
    pub help_bar: bool,
    /// Show horizontal rule separators at command boundaries (Ctrl+B toggle).
    pub command_separators: bool,
    /// Show sticky command headers when scrolling through command output.
    pub sticky_headers: bool,
}

impl DisplaySettings {
    /// Creates new settings with defaults.
    pub fn new() -> Self {
        Self {
            command_separators: true, // On by default
            sticky_headers: true,
            ..Default::default()
        }
    }
}

/// Complete state for the scroll viewer.
///
/// This struct consolidates all scroll viewer state: display settings,
/// command boundary index, and cached search results for filter mode.
#[derive(Debug, Default)]
pub struct ViewerState {
    /// Display toggle settings.
    pub display: DisplaySettings,
    /// Command boundary index for Ctrl+P/N navigation.
    /// Lazily populated when markers are detected.
    pub boundaries: CommandBoundaries,
    /// Cached search result line indices from last search.
    /// Used by filter mode to show only search result lines.
    pub last_search_lines: Option<Vec<usize>>,
    /// Registry for rich command separator rendering.
    pub separator_registry: SeparatorRegistry,
    /// Collapsed command indices (reserved for future external fold controls).
    pub collapsed_commands: HashSet<usize>,
    /// Last rendered first visible buffer line index (0-based) in normal mode.
    pub last_first_visible_line_idx: Option<usize>,
}

impl ViewerState {
    /// Creates a new viewer state with default settings.
    pub fn new() -> Self {
        Self {
            display: DisplaySettings::new(),
            separator_registry: SeparatorRegistry::with_defaults(),
            ..Default::default()
        }
    }

    /// Returns true if line numbers are currently shown.
    pub fn is_line_numbers_shown(&self) -> bool {
        self.display.line_numbers
    }

    /// Toggles line number display.
    pub fn toggle_line_numbers(&mut self) {
        self.display.line_numbers = !self.display.line_numbers;
    }

    /// Returns true if timestamps are currently shown.
    pub fn is_timestamps_shown(&self) -> bool {
        self.display.timestamps
    }

    /// Toggles timestamp display.
    pub fn toggle_timestamps(&mut self) {
        self.display.timestamps = !self.display.timestamps;
    }

    /// Returns true if the help bar is currently shown.
    pub fn is_help_bar_shown(&self) -> bool {
        self.display.help_bar
    }

    /// Toggles help bar display.
    pub fn toggle_help_bar(&mut self) {
        self.display.help_bar = !self.display.help_bar;
    }

    /// Returns true if command separators are currently shown.
    pub fn is_command_separators_shown(&self) -> bool {
        self.display.command_separators
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
        assert!(settings.command_separators); // On by default
        assert!(settings.sticky_headers);
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
}
