//! Clock segment showing current time.
//!
//! Right-aligned segment displaying the current time (HH:MM format).

use unicode_width::UnicodeWidthStr;

use super::{RenderedSegment, SegmentAlign, TopbarSegment, TopbarState, color_to_fg_ansi};
use crate::chrome::symbols::Symbols;
use crate::chrome::theme::Theme;

/// Segment displaying current time.
///
/// Always renders, right-aligned.
/// Format: HH:MM (24-hour)
pub struct ClockSegment;

impl TopbarSegment for ClockSegment {
    fn id(&self) -> &'static str {
        "clock"
    }

    fn render(
        &self,
        state: &TopbarState,
        theme: &Theme,
        symbols: &Symbols,
        separator: &str,
    ) -> Option<RenderedSegment> {
        let sep_fg = color_to_fg_ansi(theme.separator_fg);
        let clock_fg = color_to_fg_ansi(theme.clock_fg);
        let separator_width = separator.width();
        let clock_icon = symbols.clock;
        let clock_icon_width = clock_icon.width();

        let (icon_part, icon_display_width) = if !clock_icon.is_empty() {
            (format!("{} ", clock_icon), clock_icon_width + 1)
        } else {
            (String::new(), 0)
        };

        let content = format!(
            " {}{} {}{}{} ",
            sep_fg, separator, clock_fg, icon_part, state.timestamp
        );
        let display_width = separator_width + icon_display_width + state.timestamp.width() + 3;

        Some(RenderedSegment {
            content,
            display_width,
            priority: 1,
            align: SegmentAlign::Right,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SymbolSet, ThemePreset};

    fn test_state() -> TopbarState {
        TopbarState {
            timestamp: "14:32".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_clock_segment_id() {
        assert_eq!(ClockSegment.id(), "clock");
    }

    #[test]
    fn test_clock_segment_renders() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let state = test_state();

        let rendered = ClockSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert_eq!(segment.priority, 1);
        assert_eq!(segment.align, SegmentAlign::Right);
        assert!(segment.content.contains("14:32"));
    }
}
