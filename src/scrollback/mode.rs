//! Modal states for the scroll viewer.
//!
//! The scroll viewer supports multiple modes beyond simple navigation:
//! - Normal: Basic scrolling with PgUp/PgDown/Home/End
//! - Search: Incremental search with highlighting
//! - Filter: Show only matching lines
//! - Yank: Selection mode for copying text
//! - GoToLine: Jump to specific line number

use super::features::{FilterState, GoToLineState, SearchState, YankState};

/// Modal state for the scroll viewer.
///
/// Each mode has its own input handling and rendering behavior.
/// Transitions happen via keybindings (Ctrl+S for Search, etc.)
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ScrollViewMode {
    /// Normal scrollback navigation (default behavior).
    /// Keys: PgUp/PgDown, Home/End, Ctrl+U/D, arrows
    #[default]
    Normal,

    /// Incremental search with match highlighting.
    /// Enter via Ctrl+S. Shows search prompt in topbar.
    /// Ctrl+N/P navigate between matches.
    Search(SearchState),

    /// Filter mode showing only matching lines.
    /// Enter via Ctrl+F. Non-matching lines hidden.
    /// Original line numbers preserved.
    Filter(FilterState),

    /// Yank/copy mode with visual selection.
    /// Enter via Ctrl+Y. V toggles line selection.
    /// Y or Enter copies to clipboard.
    Yank(YankState),

    /// Go-to-line prompt.
    /// Enter via Ctrl+G. Shows line number input.
    GoToLine(GoToLineState),
}

impl ScrollViewMode {
    /// Returns true if currently in Normal mode.
    pub fn is_normal(&self) -> bool {
        matches!(self, Self::Normal)
    }

    /// Returns true if in any input mode (Search, Filter, GoToLine).
    pub fn is_input_mode(&self) -> bool {
        matches!(self, Self::Search(_) | Self::Filter(_) | Self::GoToLine(_))
    }

    /// Returns true if in selection mode (Yank).
    pub fn is_selection_mode(&self) -> bool {
        matches!(self, Self::Yank(_))
    }

    /// Returns the mode name for display (e.g., in help bar).
    pub fn current_mode_name(&self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Search(_) => "SEARCH",
            Self::Filter(_) => "FILTER",
            Self::Yank(_) => "YANK",
            Self::GoToLine(_) => "GOTO",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_default_returns_normal() {
        assert!(matches!(ScrollViewMode::default(), ScrollViewMode::Normal));
    }

    #[test]
    fn test_is_normal_on_normal_returns_true() {
        assert!(ScrollViewMode::Normal.is_normal());
    }

    #[test]
    fn test_is_input_mode_on_normal_returns_false() {
        assert!(!ScrollViewMode::Normal.is_input_mode());
    }

    #[test]
    fn test_is_normal_on_search_returns_false() {
        let search = ScrollViewMode::Search(SearchState::default());
        assert!(!search.is_normal());
    }

    #[test]
    fn test_is_input_mode_on_search_returns_true() {
        let search = ScrollViewMode::Search(SearchState::default());
        assert!(search.is_input_mode());
    }

    #[test]
    fn test_name_on_normal_returns_normal_uppercase() {
        assert_eq!(ScrollViewMode::Normal.current_mode_name(), "NORMAL");
    }

    #[test]
    fn test_name_on_search_returns_search_uppercase() {
        assert_eq!(
            ScrollViewMode::Search(SearchState::default()).current_mode_name(),
            "SEARCH"
        );
    }
}
