//! Reusable input widget components for settings panels.
//!
//! Provides composable primitives: [`ToggleWidget`], [`SelectWidget`],
//! [`MultiSelectWidget`], [`CarouselWidget`], [`SliderWidget`], and
//! [`TextInputWidget`]. Each widget handles key input through its own
//! `handle_input` method and returns a [`WidgetResult`] to indicate
//! whether the edit was confirmed, cancelled, or still in progress.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};

use crate::chrome::glyphs::GlyphSet;
use crate::chrome::theme::Theme;
use crate::ui::scrollable_list::ScrollableList;

/// Result of widget input handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetResult {
    /// Widget consumed the input, continue editing.
    Continue,
    /// User confirmed the value.
    Confirmed,
    /// User cancelled editing.
    Cancelled,
}

// ============================================================================
// ToggleWidget
// ============================================================================

/// Boolean on/off toggle.
pub struct ToggleWidget {
    value: bool,
}

impl ToggleWidget {
    pub fn new(value: bool) -> Self {
        Self { value }
    }

    pub fn value(&self) -> bool {
        self.value
    }

    pub fn set_value(&mut self, value: bool) {
        self.value = value;
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> WidgetResult {
        match key.code {
            KeyCode::Char(' ') | KeyCode::Enter => {
                self.value = !self.value;
                WidgetResult::Confirmed
            }
            KeyCode::Esc => WidgetResult::Cancelled,
            _ => WidgetResult::Continue,
        }
    }

    pub fn render(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &GlyphSet) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let label = if self.value {
            glyphs.indicator.check_box
        } else {
            glyphs.indicator.empty_box
        };
        let text = if self.value { "ON" } else { "OFF" };
        let style = if self.value {
            Style::default()
                .fg(theme.semantic_success)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_secondary)
        };

        let spans = vec![
            Span::styled(label, style),
            Span::styled(" ", Style::default()),
            Span::styled(text, style),
        ];
        ratatui_widgets::paragraph::Paragraph::new(Line::from(spans)).render(area, buffer);
    }

    /// Returns a display string for the current value (for non-editing display).
    pub fn display_value(&self, glyphs: &GlyphSet) -> String {
        let label = if self.value {
            glyphs.indicator.check_box
        } else {
            glyphs.indicator.empty_box
        };
        let text = if self.value { "ON" } else { "OFF" };
        format!("{} {}", label, text)
    }
}

// ============================================================================
// SelectWidget
// ============================================================================

/// Single selection from a list of options.
pub struct SelectWidget {
    options: Vec<String>,
    list: ScrollableList,
}

impl SelectWidget {
    pub fn new(options: Vec<String>, selected: usize) -> Self {
        let mut list = ScrollableList::new();
        list.set_selection(selected, options.len());
        Self { options, list }
    }

    pub fn selected(&self) -> usize {
        self.list.selection()
    }

    pub fn selected_label(&self) -> &str {
        self.options
            .get(self.list.selection())
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    pub fn set_selected(&mut self, index: usize) {
        self.list.set_selection(index, self.options.len());
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> WidgetResult {
        match key.code {
            KeyCode::Up => {
                self.list.up(self.options.len());
                WidgetResult::Continue
            }
            KeyCode::Down => {
                self.list.down(self.options.len());
                WidgetResult::Continue
            }
            KeyCode::Enter => WidgetResult::Confirmed,
            KeyCode::Esc => WidgetResult::Cancelled,
            _ => WidgetResult::Continue,
        }
    }

    pub fn render(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, _glyphs: &GlyphSet) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let viewport_height = area.height as usize;
        let mut list_state = self.list.clone();
        list_state.ensure_visible(viewport_height);

        let items: Vec<ListItem> = self
            .options
            .iter()
            .enumerate()
            .skip(list_state.scroll_offset())
            .take(viewport_height)
            .map(|(i, opt)| {
                let style = if i == self.list.selection() {
                    Style::default()
                        .fg(theme.selection_fg)
                        .bg(theme.selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_primary)
                };
                let prefix = if i == self.list.selection() {
                    "> "
                } else {
                    "  "
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{}{}", prefix, opt),
                    style,
                )))
            })
            .collect();

        List::new(items).render(area, buffer);
    }
}

// ============================================================================
// MultiSelectWidget
// ============================================================================

/// Multiple selections from a list of options.
pub struct MultiSelectWidget {
    options: Vec<(String, bool)>,
    list: ScrollableList,
}

impl MultiSelectWidget {
    pub fn new(options: Vec<(String, bool)>) -> Self {
        let len = options.len();
        Self {
            options,
            list: ScrollableList::new(),
        }
        .with_count(len)
    }

    fn with_count(mut self, count: usize) -> Self {
        self.list.set_selection(0, count);
        self
    }

    pub fn selected_indices(&self) -> Vec<usize> {
        self.options
            .iter()
            .enumerate()
            .filter(|(_, (_, checked))| *checked)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> WidgetResult {
        match key.code {
            KeyCode::Up => {
                self.list.up(self.options.len());
                WidgetResult::Continue
            }
            KeyCode::Down => {
                self.list.down(self.options.len());
                WidgetResult::Continue
            }
            KeyCode::Char(' ') => {
                let idx = self.list.selection();
                if let Some((_, checked)) = self.options.get_mut(idx) {
                    *checked = !*checked;
                }
                WidgetResult::Continue
            }
            KeyCode::Enter => WidgetResult::Confirmed,
            KeyCode::Esc => WidgetResult::Cancelled,
            _ => WidgetResult::Continue,
        }
    }

    pub fn render(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &GlyphSet) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let viewport_height = area.height as usize;
        let mut list_state = self.list.clone();
        list_state.ensure_visible(viewport_height);

        let items: Vec<ListItem> = self
            .options
            .iter()
            .enumerate()
            .skip(list_state.scroll_offset())
            .take(viewport_height)
            .map(|(i, (label, checked))| {
                let check = if *checked {
                    glyphs.indicator.check_box
                } else {
                    glyphs.indicator.empty_box
                };
                let style = if i == self.list.selection() {
                    Style::default()
                        .fg(theme.selection_fg)
                        .bg(theme.selection_bg)
                } else {
                    Style::default().fg(theme.text_primary)
                };
                ListItem::new(Line::from(Span::styled(
                    format!("{} {}", check, label),
                    style,
                )))
            })
            .collect();

        List::new(items).render(area, buffer);
    }
}

// ============================================================================
// TextInputWidget
// ============================================================================

/// Single-line text input with cursor.
pub struct TextInputWidget {
    value: String,
    cursor: usize,
}

impl TextInputWidget {
    pub fn new(value: String) -> Self {
        let cursor = value.len();
        Self { value, cursor }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: String) {
        self.cursor = value.len();
        self.value = value;
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> WidgetResult {
        match key.code {
            KeyCode::Char(c) => {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                WidgetResult::Continue
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // Find the previous char boundary
                    let prev = self.value[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.value.remove(prev);
                    self.cursor = prev;
                }
                WidgetResult::Continue
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = self.value[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                WidgetResult::Continue
            }
            KeyCode::Right => {
                if self.cursor < self.value.len() {
                    self.cursor = self.value[self.cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor + i)
                        .unwrap_or(self.value.len());
                }
                WidgetResult::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                WidgetResult::Continue
            }
            KeyCode::End => {
                self.cursor = self.value.len();
                WidgetResult::Continue
            }
            KeyCode::Enter => WidgetResult::Confirmed,
            KeyCode::Esc => WidgetResult::Cancelled,
            _ => WidgetResult::Continue,
        }
    }

    pub fn render(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &GlyphSet) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let text_style = Style::default()
            .fg(theme.header_fg)
            .add_modifier(Modifier::BOLD);
        let cursor_style = Style::default().fg(theme.text_highlight);

        // Build the display: text before cursor, cursor char, text after cursor
        let before = &self.value[..self.cursor];
        let after = &self.value[self.cursor..];
        let cursor_ch = after.chars().next();

        let mut spans = vec![Span::styled(before, text_style)];

        if let Some(ch) = cursor_ch {
            spans.push(Span::styled(
                String::from(ch),
                cursor_style.add_modifier(Modifier::REVERSED),
            ));
            let rest_start = self.cursor + ch.len_utf8();
            if rest_start < self.value.len() {
                spans.push(Span::styled(&self.value[rest_start..], text_style));
            }
        } else {
            // Cursor at end — show block cursor
            spans.push(Span::styled(
                String::from(glyphs.progress.block_full),
                cursor_style,
            ));
        }

        ratatui_widgets::paragraph::Paragraph::new(Line::from(spans)).render(area, buffer);
    }
}

// ============================================================================
// CarouselWidget
// ============================================================================

/// Layout orientation for the carousel widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CarouselOrientation {
    /// Prev/next shown above and below current. Navigates with Up/Down.
    #[default]
    Vertical,
    /// Prev/next shown left and right of current. Navigates with Left/Right.
    Horizontal,
}

/// Carousel-style single-select with wrapping navigation.
///
/// Renders prev/current/next options with directional arrows.
/// Vertical mode uses 3 lines (Up/Down); horizontal uses a single line (Left/Right).
/// All four arrow keys are accepted in both orientations for convenience.
pub struct CarouselWidget {
    options: Vec<String>,
    selected: usize,
    orientation: CarouselOrientation,
}

impl CarouselWidget {
    /// Creates a new vertical carousel (default orientation).
    pub fn new(options: Vec<String>, selected: usize) -> Self {
        let selected = if options.is_empty() {
            0
        } else {
            selected.min(options.len() - 1)
        };
        Self {
            options,
            selected,
            orientation: CarouselOrientation::Vertical,
        }
    }

    /// Creates a new horizontal carousel.
    pub fn horizontal(options: Vec<String>, selected: usize) -> Self {
        let selected = if options.is_empty() {
            0
        } else {
            selected.min(options.len() - 1)
        };
        Self {
            options,
            selected,
            orientation: CarouselOrientation::Horizontal,
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_label(&self) -> &str {
        self.options
            .get(self.selected)
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> WidgetResult {
        if self.options.is_empty() {
            return match key.code {
                KeyCode::Esc => WidgetResult::Cancelled,
                KeyCode::Enter => WidgetResult::Confirmed,
                _ => WidgetResult::Continue,
            };
        }
        match key.code {
            KeyCode::Up | KeyCode::Left => {
                let len = self.options.len();
                self.selected = (self.selected + len - 1) % len;
                WidgetResult::Continue
            }
            KeyCode::Down | KeyCode::Right => {
                self.selected = (self.selected + 1) % self.options.len();
                WidgetResult::Continue
            }
            KeyCode::Enter => WidgetResult::Confirmed,
            KeyCode::Esc => WidgetResult::Cancelled,
            _ => WidgetResult::Continue,
        }
    }

    pub fn render(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &GlyphSet) {
        if area.width == 0 || area.height == 0 || self.options.is_empty() {
            return;
        }

        match self.orientation {
            CarouselOrientation::Vertical => self.render_vertical(buffer, area, theme, glyphs),
            CarouselOrientation::Horizontal => self.render_horizontal(buffer, area, theme, glyphs),
        }
    }

    fn render_vertical(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &GlyphSet) {
        let len = self.options.len();
        let prev_idx = (self.selected + len - 1) % len;
        let next_idx = (self.selected + 1) % len;

        let dimmed = Style::default().fg(theme.text_secondary);
        let active = Style::default()
            .fg(theme.selection_fg)
            .bg(theme.selection_bg)
            .add_modifier(Modifier::BOLD);

        if area.height >= 3 {
            // 3-line layout: prev, current, next
            let prev_row = Rect::new(area.x, area.y, area.width, 1);
            let curr_row = Rect::new(area.x, area.y + 1, area.width, 1);
            let next_row = Rect::new(area.x, area.y + 2, area.width, 1);

            ratatui_widgets::paragraph::Paragraph::new(Line::from(vec![
                Span::styled(format!("  {} ", glyphs.nav.arrow_up), dimmed),
                Span::styled(&self.options[prev_idx], dimmed),
            ]))
            .render(prev_row, buffer);

            ratatui_widgets::paragraph::Paragraph::new(Line::from(vec![
                Span::styled(format!("  {} ", glyphs.nav.chevron_right), active),
                Span::styled(&self.options[self.selected], active),
            ]))
            .render(curr_row, buffer);

            ratatui_widgets::paragraph::Paragraph::new(Line::from(vec![
                Span::styled(format!("  {} ", glyphs.nav.arrow_down), dimmed),
                Span::styled(&self.options[next_idx], dimmed),
            ]))
            .render(next_row, buffer);
        } else {
            // Fallback: single-line current only
            let row = Rect::new(area.x, area.y, area.width, 1);
            let active = Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD);
            ratatui_widgets::paragraph::Paragraph::new(Line::from(vec![
                Span::styled(format!("{} ", glyphs.nav.chevron_right), active),
                Span::styled(&self.options[self.selected], active),
            ]))
            .render(row, buffer);
        }
    }

    fn render_horizontal(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &GlyphSet) {
        let len = self.options.len();
        let prev_idx = (self.selected + len - 1) % len;
        let next_idx = (self.selected + 1) % len;

        let dimmed = Style::default().fg(theme.text_secondary);
        let active = Style::default()
            .fg(theme.selection_fg)
            .bg(theme.selection_bg)
            .add_modifier(Modifier::BOLD);

        let row = Rect::new(area.x, area.y, area.width, 1);

        // Build spans: ← Prev   ❯ Current   Next →
        let mut spans = Vec::new();

        // Prev with left arrow
        spans.push(Span::styled(
            format!("{} {} ", glyphs.nav.arrow_left, &self.options[prev_idx]),
            dimmed,
        ));
        // Spacer
        spans.push(Span::styled("  ", Style::default()));
        // Current with chevron
        spans.push(Span::styled(
            format!(
                "{} {}",
                glyphs.nav.chevron_right, &self.options[self.selected]
            ),
            active,
        ));
        // Spacer
        spans.push(Span::styled("  ", Style::default()));
        // Next with right arrow
        spans.push(Span::styled(
            format!("{} {}", &self.options[next_idx], glyphs.nav.arrow_right),
            dimmed,
        ));

        ratatui_widgets::paragraph::Paragraph::new(Line::from(spans)).render(row, buffer);
    }
}

// ============================================================================
// SliderWidget
// ============================================================================

/// Numeric range slider with visual bar.
pub struct SliderWidget {
    value: usize,
    min: usize,
    max: usize,
    step: usize,
}

impl SliderWidget {
    pub fn new(value: usize, min: usize, max: usize, step: usize) -> Self {
        Self {
            value: value.clamp(min, max),
            min,
            max,
            step: step.max(1),
        }
    }

    pub fn value(&self) -> usize {
        self.value
    }

    pub fn handle_input(&mut self, key: KeyEvent) -> WidgetResult {
        match key.code {
            KeyCode::Left => {
                self.value = self.value.saturating_sub(self.step).max(self.min);
                WidgetResult::Continue
            }
            KeyCode::Right => {
                self.value = (self.value + self.step).min(self.max);
                WidgetResult::Continue
            }
            KeyCode::Home => {
                self.value = self.min;
                WidgetResult::Continue
            }
            KeyCode::End => {
                self.value = self.max;
                WidgetResult::Continue
            }
            KeyCode::Enter => WidgetResult::Confirmed,
            KeyCode::Esc => WidgetResult::Cancelled,
            _ => WidgetResult::Continue,
        }
    }

    pub fn render(&self, buffer: &mut Buffer, area: Rect, theme: &Theme, glyphs: &GlyphSet) {
        if area.width < 10 || area.height == 0 {
            return;
        }

        let value_str = self.value.to_string();
        // Layout: [bar] value
        // Reserve space for brackets (2), space (1), and value text
        let value_display_width = value_str.len() + 1; // space + digits
        let bar_width = (area.width as usize).saturating_sub(2 + value_display_width);

        if bar_width == 0 {
            // Too narrow for bar, just show value
            let row = Rect::new(area.x, area.y, area.width, 1);
            ratatui_widgets::paragraph::Paragraph::new(Line::from(Span::styled(
                &value_str,
                Style::default()
                    .fg(theme.text_highlight)
                    .add_modifier(Modifier::BOLD),
            )))
            .render(row, buffer);
            return;
        }

        let range = self.max - self.min;
        let fill_ratio = if range == 0 {
            1.0
        } else {
            (self.value - self.min) as f64 / range as f64
        };
        let filled = ((bar_width as f64) * fill_ratio).round() as usize;
        let empty = bar_width.saturating_sub(filled);

        let fill_char = glyphs.progress.bar[7]; // full block
        let empty_char = glyphs.progress.shade_light;

        let bar_fill: String = std::iter::repeat_n(fill_char, filled).collect();
        let bar_empty: String = std::iter::repeat_n(empty_char, empty).collect();

        let bar_style = Style::default().fg(theme.semantic_success);
        let empty_style = Style::default().fg(theme.text_secondary);
        let bracket_style = Style::default().fg(theme.panel_border);
        let value_style = Style::default()
            .fg(theme.text_highlight)
            .add_modifier(Modifier::BOLD);

        let row = Rect::new(area.x, area.y, area.width, 1);
        ratatui_widgets::paragraph::Paragraph::new(Line::from(vec![
            Span::styled("[", bracket_style),
            Span::styled(bar_fill, bar_style),
            Span::styled(bar_empty, empty_style),
            Span::styled("]", bracket_style),
            Span::styled(format!(" {}", value_str), value_style),
        ]))
        .render(row, buffer);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- ToggleWidget ---

    #[test]
    fn test_toggle_new_and_value() {
        let t = ToggleWidget::new(false);
        assert!(!t.value());
        let t = ToggleWidget::new(true);
        assert!(t.value());
    }

    #[test]
    fn test_toggle_space_toggles() {
        let mut t = ToggleWidget::new(false);
        let result = t.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert!(t.value());
        assert_eq!(result, WidgetResult::Confirmed);
    }

    #[test]
    fn test_toggle_enter_toggles() {
        let mut t = ToggleWidget::new(true);
        let result = t.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(!t.value());
        assert_eq!(result, WidgetResult::Confirmed);
    }

    #[test]
    fn test_toggle_esc_cancels() {
        let mut t = ToggleWidget::new(false);
        let result = t.handle_input(KeyEvent::from(KeyCode::Esc));
        assert_eq!(result, WidgetResult::Cancelled);
    }

    // --- SelectWidget ---

    #[test]
    fn test_select_new_and_selected() {
        let s = SelectWidget::new(vec!["A".into(), "B".into(), "C".into()], 1);
        assert_eq!(s.selected(), 1);
        assert_eq!(s.selected_label(), "B");
    }

    #[test]
    fn test_select_navigation() {
        let mut s = SelectWidget::new(vec!["A".into(), "B".into(), "C".into()], 0);
        s.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(s.selected(), 1);
        s.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(s.selected(), 2);
        s.handle_input(KeyEvent::from(KeyCode::Up));
        assert_eq!(s.selected(), 1);
    }

    #[test]
    fn test_select_enter_confirms() {
        let mut s = SelectWidget::new(vec!["A".into(), "B".into()], 0);
        let result = s.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, WidgetResult::Confirmed);
    }

    #[test]
    fn test_select_esc_cancels() {
        let mut s = SelectWidget::new(vec!["A".into(), "B".into()], 0);
        let result = s.handle_input(KeyEvent::from(KeyCode::Esc));
        assert_eq!(result, WidgetResult::Cancelled);
    }

    // --- MultiSelectWidget ---

    #[test]
    fn test_multiselect_toggle_with_space() {
        let mut m = MultiSelectWidget::new(vec![
            ("A".into(), false),
            ("B".into(), true),
            ("C".into(), false),
        ]);
        assert_eq!(m.selected_indices(), vec![1]);

        // Toggle item 0
        m.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(m.selected_indices(), vec![0, 1]);
    }

    #[test]
    fn test_multiselect_navigation() {
        let mut m = MultiSelectWidget::new(vec![("A".into(), false), ("B".into(), false)]);
        m.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(m.list.selection(), 1);
    }

    // --- TextInputWidget ---

    #[test]
    fn test_text_input_typing() {
        let mut t = TextInputWidget::new(String::new());
        t.handle_input(KeyEvent::from(KeyCode::Char('h')));
        t.handle_input(KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(t.value(), "hi");
    }

    #[test]
    fn test_text_input_backspace() {
        let mut t = TextInputWidget::new("abc".into());
        t.handle_input(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(t.value(), "ab");
    }

    #[test]
    fn test_text_input_cursor_movement() {
        let mut t = TextInputWidget::new("hello".into());
        t.handle_input(KeyEvent::from(KeyCode::Home));
        assert_eq!(t.cursor, 0);
        t.handle_input(KeyEvent::from(KeyCode::End));
        assert_eq!(t.cursor, 5);
        t.handle_input(KeyEvent::from(KeyCode::Left));
        assert_eq!(t.cursor, 4);
        t.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(t.cursor, 5);
    }

    #[test]
    fn test_text_input_enter_confirms() {
        let mut t = TextInputWidget::new("test".into());
        let result = t.handle_input(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, WidgetResult::Confirmed);
    }

    #[test]
    fn test_text_input_esc_cancels() {
        let mut t = TextInputWidget::new("test".into());
        let result = t.handle_input(KeyEvent::from(KeyCode::Esc));
        assert_eq!(result, WidgetResult::Cancelled);
    }

    #[test]
    fn test_text_input_insert_at_cursor() {
        let mut t = TextInputWidget::new("ac".into());
        t.handle_input(KeyEvent::from(KeyCode::Left)); // cursor at 'c'
        t.handle_input(KeyEvent::from(KeyCode::Char('b')));
        assert_eq!(t.value(), "abc");
    }

    // --- CarouselWidget ---

    #[test]
    fn test_carousel_new_and_selected() {
        let c = CarouselWidget::new(vec!["A".into(), "B".into(), "C".into()], 1);
        assert_eq!(c.selected(), 1);
        assert_eq!(c.selected_label(), "B");
    }

    #[test]
    fn test_carousel_down_wraps() {
        let mut c = CarouselWidget::new(vec!["A".into(), "B".into(), "C".into()], 2);
        c.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(c.selected(), 0); // wraps to first
    }

    #[test]
    fn test_carousel_up_wraps() {
        let mut c = CarouselWidget::new(vec!["A".into(), "B".into(), "C".into()], 0);
        c.handle_input(KeyEvent::from(KeyCode::Up));
        assert_eq!(c.selected(), 2); // wraps to last
    }

    #[test]
    fn test_carousel_enter_confirms() {
        let mut c = CarouselWidget::new(vec!["A".into(), "B".into()], 0);
        assert_eq!(
            c.handle_input(KeyEvent::from(KeyCode::Enter)),
            WidgetResult::Confirmed
        );
    }

    #[test]
    fn test_carousel_esc_cancels() {
        let mut c = CarouselWidget::new(vec!["A".into(), "B".into()], 0);
        assert_eq!(
            c.handle_input(KeyEvent::from(KeyCode::Esc)),
            WidgetResult::Cancelled
        );
    }

    #[test]
    fn test_carousel_navigation_cycle() {
        let mut c = CarouselWidget::new(vec!["A".into(), "B".into(), "C".into()], 0);
        c.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(c.selected_label(), "B");
        c.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(c.selected_label(), "C");
        c.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(c.selected_label(), "A"); // full cycle
    }

    #[test]
    fn test_carousel_horizontal_new() {
        let c = CarouselWidget::horizontal(vec!["A".into(), "B".into(), "C".into()], 1);
        assert_eq!(c.selected(), 1);
        assert_eq!(c.orientation, CarouselOrientation::Horizontal);
    }

    #[test]
    fn test_carousel_horizontal_left_right_navigation() {
        let mut c = CarouselWidget::horizontal(vec!["A".into(), "B".into(), "C".into()], 0);
        c.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(c.selected_label(), "B");
        c.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(c.selected_label(), "C");
        c.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(c.selected_label(), "A"); // wraps

        c.handle_input(KeyEvent::from(KeyCode::Left));
        assert_eq!(c.selected_label(), "C"); // wraps back
    }

    #[test]
    fn test_carousel_vertical_accepts_left_right() {
        let mut c = CarouselWidget::new(vec!["A".into(), "B".into(), "C".into()], 0);
        assert_eq!(c.orientation, CarouselOrientation::Vertical);
        c.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(c.selected_label(), "B");
        c.handle_input(KeyEvent::from(KeyCode::Left));
        assert_eq!(c.selected_label(), "A");
    }

    // --- SliderWidget ---

    #[test]
    fn test_slider_new_and_value() {
        let s = SliderWidget::new(500, 0, 1000, 100);
        assert_eq!(s.value(), 500);
    }

    #[test]
    fn test_slider_right_increments_by_step() {
        let mut s = SliderWidget::new(500, 0, 1000, 100);
        s.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(s.value(), 600);
    }

    #[test]
    fn test_slider_left_decrements_by_step() {
        let mut s = SliderWidget::new(500, 0, 1000, 100);
        s.handle_input(KeyEvent::from(KeyCode::Left));
        assert_eq!(s.value(), 400);
    }

    #[test]
    fn test_slider_right_clamps_at_max() {
        let mut s = SliderWidget::new(950, 0, 1000, 100);
        s.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(s.value(), 1000);
    }

    #[test]
    fn test_slider_left_clamps_at_min() {
        let mut s = SliderWidget::new(50, 0, 1000, 100);
        s.handle_input(KeyEvent::from(KeyCode::Left));
        assert_eq!(s.value(), 0);
    }

    #[test]
    fn test_slider_home_sets_min() {
        let mut s = SliderWidget::new(500, 100, 1000, 100);
        s.handle_input(KeyEvent::from(KeyCode::Home));
        assert_eq!(s.value(), 100);
    }

    #[test]
    fn test_slider_end_sets_max() {
        let mut s = SliderWidget::new(500, 100, 1000, 100);
        s.handle_input(KeyEvent::from(KeyCode::End));
        assert_eq!(s.value(), 1000);
    }

    #[test]
    fn test_slider_enter_confirms_esc_cancels() {
        let mut s = SliderWidget::new(500, 0, 1000, 100);
        assert_eq!(
            s.handle_input(KeyEvent::from(KeyCode::Enter)),
            WidgetResult::Confirmed
        );
        let mut s2 = SliderWidget::new(500, 0, 1000, 100);
        assert_eq!(
            s2.handle_input(KeyEvent::from(KeyCode::Esc)),
            WidgetResult::Cancelled
        );
    }

    #[test]
    fn test_slider_clamps_initial_value() {
        let s = SliderWidget::new(2000, 0, 1000, 100);
        assert_eq!(s.value(), 1000);
    }
}
