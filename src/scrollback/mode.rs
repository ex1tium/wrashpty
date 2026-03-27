//! Context enum for scroll viewer help bar text.
//!
//! Search/Filter/GoToLine use local state in nested loops, so there is no
//! global modal state machine.  This enum purely selects the help text shown
//! at the bottom of the screen.

/// Context for selecting help bar content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HelpContext {
    /// Normal scrollback navigation.
    #[default]
    Normal,
    /// Incremental search mode.
    Search,
    /// Filter mode (only matching lines visible).
    Filter,
    /// Go-to-line prompt.
    GoToLine,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_returns_normal() {
        assert_eq!(HelpContext::default(), HelpContext::Normal);
    }
}
