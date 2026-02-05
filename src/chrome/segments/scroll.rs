//! Scroll mode segment.
//!
//! Displays scroll position information when in scroll mode.

use unicode_width::UnicodeWidthStr;

use super::{color_to_fg_ansi, RenderedSegment, SegmentAlign, TopbarSegment, TopbarState};
use crate::chrome::symbols::Symbols;
use crate::chrome::theme::Theme;

/// Segment displaying scroll mode information.
///
/// Only renders when `state.scroll` is `Some`.
/// Format: ▶ SCROLL | line/total | pct%
pub struct ScrollSegment;

impl TopbarSegment for ScrollSegment {
    fn id(&self) -> &'static str {
        "scroll"
    }

    fn render(
        &self,
        state: &TopbarState,
        theme: &Theme,
        _symbols: &Symbols,
        _separator: &str,
    ) -> Option<RenderedSegment> {
        let scroll_info = state.scroll.as_ref()?;

        let scroll_fg = color_to_fg_ansi(theme.separator_fg);
        let scroll_content = format!(
            "▶ SCROLL | {}/{} | {}%",
            scroll_info.first_visible_line, scroll_info.total_lines, scroll_info.percentage
        );
        let scroll_width = scroll_content.width();

        let content = format!(" {}{} ", scroll_fg, scroll_content);
        let display_width = scroll_width + 2;

        Some(RenderedSegment {
            content,
            display_width,
            priority: 0, // Always show when active
            align: SegmentAlign::Left,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::segments::ScrollInfo;
    use crate::config::{SymbolSet, ThemePreset};

    fn test_state() -> TopbarState {
        TopbarState::default()
    }

    #[test]
    fn test_scroll_segment_id() {
        assert_eq!(ScrollSegment.id(), "scroll");
    }

    #[test]
    fn test_scroll_segment_hidden_when_not_scrolled() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let state = test_state();

        let rendered = ScrollSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_none());
    }

    #[test]
    fn test_scroll_segment_renders_when_scrolled() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let mut state = test_state();
        state.scroll = Some(ScrollInfo {
            percentage: 45,
            total_lines: 1000,
            first_visible_line: 450,
        });

        let rendered = ScrollSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert_eq!(segment.priority, 0);
        assert!(segment.content.contains("SCROLL"));
        assert!(segment.content.contains("450/1000"));
        assert!(segment.content.contains("45%"));
    }
}
