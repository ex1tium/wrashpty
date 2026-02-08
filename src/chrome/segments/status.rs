//! Status segment showing command exit code.
//!
//! Displays a checkmark (✓) for success or X (✗) for failure.

use unicode_width::UnicodeWidthStr;

use super::{
    RenderedSegment, SegmentAlign, TopbarSegment, TopbarState, color_to_bg_ansi, color_to_fg_ansi,
};
use crate::chrome::symbols::Symbols;
use crate::chrome::theme::Theme;

/// Segment displaying command exit status.
///
/// Shows:
/// - ✓ (green) for exit code 0
/// - ✗ (red) for non-zero exit code
pub struct StatusSegment;

impl TopbarSegment for StatusSegment {
    fn id(&self) -> &'static str {
        "status"
    }

    fn render(
        &self,
        state: &TopbarState,
        theme: &Theme,
        symbols: &Symbols,
        _separator: &str,
    ) -> Option<RenderedSegment> {
        let bar_bg = color_to_bg_ansi(theme.bar_bg);

        let (icon, color) = if state.exit_code == 0 {
            (symbols.success, color_to_fg_ansi(theme.success_fg))
        } else {
            (symbols.failure, color_to_fg_ansi(theme.failure_fg))
        };

        let icon_width = icon.width();
        let content = format!(" {}{}{} ", color, icon, bar_bg);
        let display_width = icon_width + 2; // " icon "

        Some(RenderedSegment {
            content,
            display_width,
            priority: 0, // Always show
            align: SegmentAlign::Left,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SymbolSet, ThemePreset};

    fn test_state() -> TopbarState {
        TopbarState {
            exit_code: 0,
            ..Default::default()
        }
    }

    #[test]
    fn test_status_segment_id() {
        assert_eq!(StatusSegment.id(), "status");
    }

    #[test]
    fn test_status_segment_success() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let state = test_state();

        let rendered = StatusSegment.render(&state, theme, symbols, "");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert_eq!(segment.priority, 0);
        assert!(segment.content.contains(symbols.success));
    }

    #[test]
    fn test_status_segment_failure() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let mut state = test_state();
        state.exit_code = 1;

        let rendered = StatusSegment.render(&state, theme, symbols, "");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert!(segment.content.contains(symbols.failure));
    }
}
