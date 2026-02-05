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
#[derive(Debug, Clone, PartialEq)]
#[derive(Default)]
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
        matches!(
            self,
            Self::Search(_) | Self::Filter(_) | Self::GoToLine(_)
        )
    }

    /// Returns true if in selection mode (Yank).
    pub fn is_selection_mode(&self) -> bool {
        matches!(self, Self::Yank(_))
    }

    /// Returns the mode name for display (e.g., in help bar).
    pub fn name(&self) -> &'static str {
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
    fn test_default_is_normal() {
        assert!(matches!(ScrollViewMode::default(), ScrollViewMode::Normal));
    }

    #[test]
    fn test_mode_predicates() {
        assert!(ScrollViewMode::Normal.is_normal());
        assert!(!ScrollViewMode::Normal.is_input_mode());

        let search = ScrollViewMode::Search(SearchState::default());
        assert!(!search.is_normal());
        assert!(search.is_input_mode());
    }

    #[test]
    fn test_mode_names() {
        assert_eq!(ScrollViewMode::Normal.name(), "NORMAL");
        assert_eq!(
            ScrollViewMode::Search(SearchState::default()).name(),
            "SEARCH"
        );
    }
}
