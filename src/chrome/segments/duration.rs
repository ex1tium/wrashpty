//! Duration segment showing command execution time.
//!
//! Only displays when duration >= 0.5s to avoid visual clutter.

use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use super::{RenderedSegment, SegmentAlign, TopbarSegment, TopbarState, color_to_fg_ansi};
use crate::chrome::symbols::Symbols;
use crate::chrome::theme::Theme;

/// Segment displaying command execution duration.
///
/// Only renders when:
/// - A duration is present
/// - Duration >= 0.5s (to avoid clutter on fast commands)
///
/// Uses slower color (orange) when duration >= 5s.
pub struct DurationSegment;

impl TopbarSegment for DurationSegment {
    fn id(&self) -> &'static str {
        "duration"
    }

    fn render(
        &self,
        state: &TopbarState,
        theme: &Theme,
        symbols: &Symbols,
        separator: &str,
    ) -> Option<RenderedSegment> {
        let dur = state.last_duration?;
        let secs = dur.as_secs_f64();

        // Only show if >= 0.5s
        if secs < 0.5 {
            return None;
        }

        let sep_fg = color_to_fg_ansi(theme.separator_fg);
        let stopwatch = symbols.stopwatch;
        let stopwatch_width = stopwatch.width();
        let separator_width = separator.width();

        let duration_str = format_duration(dur);
        let color = if secs >= 5.0 {
            color_to_fg_ansi(theme.duration_slow_fg)
        } else {
            color_to_fg_ansi(theme.duration_fg)
        };

        let (icon_part, icon_width) = if !stopwatch.is_empty() {
            (format!("{} ", stopwatch), stopwatch_width + 1)
        } else {
            (String::new(), 0)
        };

        let content = format!(
            " {}{} {}{}{} ",
            sep_fg, separator, color, icon_part, duration_str
        );
        let display_width = separator_width + icon_width + duration_str.width() + 3;

        Some(RenderedSegment {
            content,
            display_width,
            priority: 3,
            align: SegmentAlign::Left,
        })
    }
}

/// Formats a duration into a human-readable string.
pub(crate) fn format_duration(dur: Duration) -> String {
    // Round total seconds first to avoid "1m60s" from floating point edge cases
    let total_secs = dur.as_secs_f64().round() as u64;
    if total_secs >= 60 {
        let mins = total_secs / 60;
        let remaining_secs = total_secs % 60;
        format!("{}m{}s", mins, remaining_secs)
    } else {
        format!("{:.1}s", dur.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SymbolSet, ThemePreset};

    fn test_state() -> TopbarState {
        TopbarState::default()
    }

    #[test]
    fn test_duration_id_returns_expected() {
        assert_eq!(DurationSegment.id(), "duration");
    }

    #[test]
    fn test_duration_render_when_fast_returns_none() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let mut state = test_state();
        state.last_duration = Some(Duration::from_millis(100));

        let rendered = DurationSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_none());
    }

    #[test]
    fn test_duration_render_when_slow_shows_segment() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let mut state = test_state();
        state.last_duration = Some(Duration::from_secs(1));

        let rendered = DurationSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert_eq!(segment.priority, 3);
        assert!(segment.content.contains("1.0s"));
    }

    #[test]
    fn test_duration_render_when_none_returns_none() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let state = test_state();

        let rendered = DurationSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_none());
    }

    #[test]
    fn test_format_duration_with_seconds_formats_decimal() {
        assert_eq!(format_duration(Duration::from_millis(500)), "0.5s");
        assert_eq!(format_duration(Duration::from_secs(5)), "5.0s");
    }

    #[test]
    fn test_format_duration_with_minutes_formats_min_sec() {
        assert_eq!(format_duration(Duration::from_secs(90)), "1m30s");
        assert_eq!(format_duration(Duration::from_secs(120)), "2m0s");
    }
}
