//! Generic viewport and selection manager for scrollable lists.
//!
//! Extracts the common `selection` / `scroll_offset` / `ensure_visible` pattern
//! used across multiple panels into a single reusable struct. Does NOT own the
//! list items — it tracks indices only, with `item_count` provided at each call.

use std::ops::Range;

/// Manages selection and viewport scrolling for a list of items.
pub struct ScrollableList {
    /// Currently selected item (0-based).
    selection: usize,
    /// First visible item in the viewport.
    scroll_offset: usize,
}

impl ScrollableList {
    /// Creates a new list at position 0.
    pub fn new() -> Self {
        Self {
            selection: 0,
            scroll_offset: 0,
        }
    }

    /// Returns the current selection index.
    pub fn selection(&self) -> usize {
        self.selection
    }

    /// Returns the current scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Resets selection and scroll to 0.
    pub fn reset(&mut self) {
        self.selection = 0;
        self.scroll_offset = 0;
    }

    /// Sets selection, clamping to `item_count - 1`.
    pub fn set_selection(&mut self, index: usize, item_count: usize) {
        if item_count == 0 {
            self.selection = 0;
        } else {
            self.selection = index.min(item_count - 1);
        }
    }

    /// Moves selection up by 1, clamping at 0.
    pub fn up(&mut self, _item_count: usize) {
        self.selection = self.selection.saturating_sub(1);
    }

    /// Moves selection down by 1, clamping at `item_count - 1`.
    pub fn down(&mut self, item_count: usize) {
        if item_count > 0 && self.selection + 1 < item_count {
            self.selection += 1;
        }
    }

    /// Moves selection up by `page` items.
    pub fn page_up(&mut self, page: usize, _item_count: usize) {
        self.selection = self.selection.saturating_sub(page);
    }

    /// Moves selection down by `page` items.
    pub fn page_down(&mut self, page: usize, item_count: usize) {
        if item_count > 0 {
            self.selection = (self.selection + page).min(item_count - 1);
        }
    }

    /// Jumps to the first item.
    pub fn home(&mut self) {
        self.selection = 0;
    }

    /// Jumps to the last item.
    pub fn end(&mut self, item_count: usize) {
        if item_count > 0 {
            self.selection = item_count - 1;
        }
    }

    /// Adjusts `scroll_offset` so the current selection is visible within
    /// `viewport_height` rows.
    pub fn ensure_visible(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }
        if self.selection < self.scroll_offset {
            self.scroll_offset = self.selection;
        } else if self.selection >= self.scroll_offset + viewport_height {
            self.scroll_offset = self.selection.saturating_sub(viewport_height - 1);
        }
    }

    /// Returns the range of item indices visible in the current viewport.
    pub fn visible_range(&self, viewport_height: usize, item_count: usize) -> Range<usize> {
        let start = self.scroll_offset.min(item_count);
        let end = (self.scroll_offset + viewport_height).min(item_count);
        start..end
    }
}

impl Clone for ScrollableList {
    fn clone(&self) -> Self {
        Self {
            selection: self.selection,
            scroll_offset: self.scroll_offset,
        }
    }
}

impl Default for ScrollableList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_starts_at_zero() {
        let sl = ScrollableList::new();
        assert_eq!(sl.selection(), 0);
        assert_eq!(sl.scroll_offset(), 0);
    }

    #[test]
    fn test_up_at_zero_stays() {
        let mut sl = ScrollableList::new();
        sl.up(5);
        assert_eq!(sl.selection(), 0);
    }

    #[test]
    fn test_down_increments() {
        let mut sl = ScrollableList::new();
        sl.down(5);
        assert_eq!(sl.selection(), 1);
        sl.down(5);
        assert_eq!(sl.selection(), 2);
    }

    #[test]
    fn test_down_at_end_stays() {
        let mut sl = ScrollableList::new();
        sl.set_selection(4, 5);
        sl.down(5);
        assert_eq!(sl.selection(), 4);
    }

    #[test]
    fn test_down_empty_list() {
        let mut sl = ScrollableList::new();
        sl.down(0);
        assert_eq!(sl.selection(), 0);
    }

    #[test]
    fn test_page_down_clamps() {
        let mut sl = ScrollableList::new();
        sl.page_down(100, 5);
        assert_eq!(sl.selection(), 4);
    }

    #[test]
    fn test_page_up_clamps() {
        let mut sl = ScrollableList::new();
        sl.set_selection(2, 10);
        sl.page_up(100, 10);
        assert_eq!(sl.selection(), 0);
    }

    #[test]
    fn test_home_end() {
        let mut sl = ScrollableList::new();
        sl.set_selection(5, 10);
        sl.home();
        assert_eq!(sl.selection(), 0);
        sl.end(10);
        assert_eq!(sl.selection(), 9);
    }

    #[test]
    fn test_end_empty_list() {
        let mut sl = ScrollableList::new();
        sl.end(0);
        assert_eq!(sl.selection(), 0);
    }

    #[test]
    fn test_set_selection_clamps() {
        let mut sl = ScrollableList::new();
        sl.set_selection(100, 5);
        assert_eq!(sl.selection(), 4);
    }

    #[test]
    fn test_set_selection_empty() {
        let mut sl = ScrollableList::new();
        sl.set_selection(5, 0);
        assert_eq!(sl.selection(), 0);
    }

    #[test]
    fn test_reset() {
        let mut sl = ScrollableList::new();
        sl.set_selection(5, 10);
        sl.scroll_offset = 3;
        sl.reset();
        assert_eq!(sl.selection(), 0);
        assert_eq!(sl.scroll_offset(), 0);
    }

    #[test]
    fn test_ensure_visible_scrolls_down() {
        let mut sl = ScrollableList::new();
        sl.set_selection(15, 20);
        sl.ensure_visible(5);
        // Selection 15 should be visible: offset should be 11 (15 - 5 + 1)
        assert_eq!(sl.scroll_offset(), 11);
        assert!(sl.selection() >= sl.scroll_offset());
        assert!(sl.selection() < sl.scroll_offset() + 5);
    }

    #[test]
    fn test_ensure_visible_scrolls_up() {
        let mut sl = ScrollableList::new();
        sl.scroll_offset = 10;
        sl.set_selection(3, 20);
        sl.ensure_visible(5);
        assert_eq!(sl.scroll_offset(), 3);
    }

    #[test]
    fn test_ensure_visible_already_visible() {
        let mut sl = ScrollableList::new();
        sl.scroll_offset = 2;
        sl.set_selection(4, 20);
        sl.ensure_visible(5);
        // Selection 4 is already in viewport [2..7), offset unchanged
        assert_eq!(sl.scroll_offset(), 2);
    }

    #[test]
    fn test_ensure_visible_zero_viewport() {
        let mut sl = ScrollableList::new();
        sl.set_selection(5, 10);
        sl.ensure_visible(0);
        // Should not panic or change offset
        assert_eq!(sl.scroll_offset(), 0);
    }

    #[test]
    fn test_visible_range() {
        let mut sl = ScrollableList::new();
        sl.scroll_offset = 3;
        let range = sl.visible_range(5, 20);
        assert_eq!(range, 3..8);
    }

    #[test]
    fn test_visible_range_clamped() {
        let mut sl = ScrollableList::new();
        sl.scroll_offset = 18;
        let range = sl.visible_range(5, 20);
        assert_eq!(range, 18..20);
    }

    #[test]
    fn test_visible_range_empty() {
        let sl = ScrollableList::new();
        let range = sl.visible_range(5, 0);
        assert_eq!(range, 0..0);
    }
}
