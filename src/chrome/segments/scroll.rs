//! Scroll mode segment.
//!
//! Displays scroll position information when in scroll mode.

use unicode_width::UnicodeWidthStr;

use super::{RenderedSegment, SegmentAlign, TopbarSegment, TopbarState, color_to_fg_ansi};
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
        separator: &str,
    ) -> Option<RenderedSegment> {
        let scroll_info = state.scroll.as_ref()?;

        let scroll_fg = color_to_fg_ansi(theme.separator_fg);

        // Build mode indicators - all in one bracket pair, comma separated
        let mut modes: Vec<&str> = Vec::new();

        // Search and filter combined indicator uses +
        if scroll_info.search_active && scroll_info.filter_active {
            modes.push("S+F");
        } else if scroll_info.search_active {
            modes.push("S");
        } else if scroll_info.filter_active {
            modes.push("F");
        }

        // Display toggle indicators
        if scroll_info.timestamps_on {
            modes.push("T");
        }
        if scroll_info.line_numbers_on {
            modes.push("L");
        }

        let mode_indicators = if modes.is_empty() {
            String::new()
        } else {
            format!(" [{}]", modes.join(", "))
        };

        let scroll_content = format!(
            "{} SCROLL | {}/{} | {}%{}",
            separator,
            scroll_info.current_line,
            scroll_info.total_lines,
            scroll_info.percentage,
            mode_indicators
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
    fn test_scroll_segment_id_default_returns_scroll() {
        assert_eq!(ScrollSegment.id(), "scroll");
    }

    #[test]
    fn test_render_when_no_scroll_returns_none() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let state = test_state();

        let rendered = ScrollSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_none());
    }

    #[test]
    fn test_render_when_scrolled_returns_content_with_priority_and_fields() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let mut state = test_state();
        state.scroll = Some(ScrollInfo {
            percentage: 45,
            total_lines: 1000,
            current_line: 450,
            ..Default::default()
        });

        let rendered = ScrollSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert_eq!(segment.priority, 0);
        assert!(segment.content.contains("SCROLL"));
        assert!(segment.content.contains("450/1000"));
        assert!(segment.content.contains("45%"));
    }

    #[test]
    fn test_render_modes_various_indicators_present() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let mut state = test_state();

        // Test search only
        state.scroll = Some(ScrollInfo {
            percentage: 50,
            total_lines: 100,
            current_line: 50,
            search_active: true,
            filter_active: false,
            timestamps_on: false,
            line_numbers_on: false,
        });
        let rendered = ScrollSegment.render(&state, theme, symbols, "▶").unwrap();
        assert!(rendered.content.contains("[S]"));

        // Test filter only
        state.scroll = Some(ScrollInfo {
            search_active: false,
            filter_active: true,
            ..state.scroll.unwrap()
        });
        let rendered = ScrollSegment.render(&state, theme, symbols, "▶").unwrap();
        assert!(rendered.content.contains("[F]"));

        // Test search+filter combined
        state.scroll = Some(ScrollInfo {
            search_active: true,
            filter_active: true,
            ..state.scroll.unwrap()
        });
        let rendered = ScrollSegment.render(&state, theme, symbols, "▶").unwrap();
        assert!(rendered.content.contains("[S+F]"));

        // Test multiple modes with timestamps and line numbers
        state.scroll = Some(ScrollInfo {
            search_active: true,
            filter_active: false,
            timestamps_on: true,
            line_numbers_on: true,
            ..state.scroll.unwrap()
        });
        let rendered = ScrollSegment.render(&state, theme, symbols, "▶").unwrap();
        assert!(rendered.content.contains("[S, T, L]"));
    }
}
