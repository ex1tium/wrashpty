//! Reusable frame-based horizontal marquee text primitive.
//!
//! This utility is independent from any specific widget and can be reused
//! by border rows, status lines, and other narrow single-line UI surfaces.

use unicode_segmentation::UnicodeSegmentation;

/// Reusable horizontal marquee primitive for overflow text in narrow UI regions.
#[derive(Debug, Clone, Copy)]
pub struct ScrollingText<'a> {
    text: &'a str,
    gap_cols: usize,
    hold_frames: u64,
}

impl<'a> ScrollingText<'a> {
    /// Creates a new scrolling text primitive with sensible defaults.
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            gap_cols: 6,
            hold_frames: 8,
        }
    }

    /// Sets the gap (in columns) inserted between wrap-around cycles.
    pub fn gap_cols(mut self, gap_cols: usize) -> Self {
        self.gap_cols = gap_cols;
        self
    }

    /// Sets how many frames to hold the text at offset 0 before scrolling.
    pub fn hold_frames(mut self, hold_frames: u64) -> Self {
        self.hold_frames = hold_frames;
        self
    }

    /// Returns true when the text exceeds the viewport width.
    pub fn is_overflowing(&self, viewport_cols: usize) -> bool {
        crate::ui::text_width::display_width(self.text) > viewport_cols
    }

    /// Renders a frame for the given viewport and frame index.
    ///
    /// The returned string is padded to exactly `viewport_cols` columns.
    pub fn frame_text(&self, viewport_cols: usize, frame: u64) -> String {
        if viewport_cols == 0 {
            return String::new();
        }

        if !self.is_overflowing(viewport_cols) {
            return crate::ui::text_width::truncate_to_width(self.text, viewport_cols).into_owned();
        }

        let mut units: Vec<&str> = self.text.graphemes(true).collect();
        for _ in 0..self.gap_cols {
            units.push(" ");
        }

        if units.is_empty() {
            return String::new();
        }

        let start = if frame < self.hold_frames {
            0
        } else {
            ((frame - self.hold_frames) as usize) % units.len()
        };

        let mut out = String::new();
        let mut used_cols = 0usize;
        let mut idx = start;
        let mut safety = 0usize;

        while used_cols < viewport_cols && safety < units.len().saturating_mul(4).max(1) {
            let unit = units[idx];
            let unit_cols = crate::ui::text_width::display_width(unit);

            if unit_cols > 0 && used_cols + unit_cols <= viewport_cols {
                out.push_str(unit);
                used_cols += unit_cols;
            }

            idx = (idx + 1) % units.len();
            safety += 1;
        }

        crate::ui::text_width::pad_to_width(&out, viewport_cols)
    }
}

#[cfg(test)]
mod tests {
    use super::ScrollingText;

    #[test]
    fn test_is_overflowing_with_short_text_returns_false() {
        let scroller = ScrollingText::new("ok");
        assert!(!scroller.is_overflowing(4));
    }

    #[test]
    fn test_is_overflowing_with_long_text_returns_true() {
        let scroller = ScrollingText::new("very long status");
        assert!(scroller.is_overflowing(6));
    }

    #[test]
    fn test_frame_text_with_no_overflow_returns_truncated_static_text() {
        let scroller = ScrollingText::new("status");
        assert_eq!(scroller.frame_text(6, 99), "status");
    }

    #[test]
    fn test_frame_text_with_overflow_and_initial_hold_keeps_start() {
        let scroller = ScrollingText::new("abcdef").hold_frames(3).gap_cols(2);
        let first = scroller.frame_text(4, 0);
        let second = scroller.frame_text(4, 2);
        assert_eq!(first, second);
    }

    #[test]
    fn test_frame_text_with_overflow_and_advanced_frame_scrolls() {
        let scroller = ScrollingText::new("abcdef").hold_frames(1).gap_cols(2);
        let held = scroller.frame_text(4, 0);
        let moved = scroller.frame_text(4, 3);
        assert_ne!(held, moved);
    }
}
