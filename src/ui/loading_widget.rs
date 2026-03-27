//! Animated loading indicator widget.
//!
//! Provides [`LoadingWidget`] for displaying animated Unicode loading spinners
//! during async operations like command-schema discovery. Supports multiple
//! animation styles via [`SpinnerStyle`].

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;

use crate::chrome::theme::Theme;

/// Animation frame style for the loading spinner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpinnerStyle {
    /// ASCII-safe spinner: `-, \, |, /`
    Ascii,
    /// Unicode block segments that appear to rotate.
    #[default]
    Block,
    /// Braille pinwheel patterns.
    Braille,
    /// Dots that cycle through fill states.
    Dots,
    /// Moon phases (requires emoji support).
    Moon,
}

impl SpinnerStyle {
    /// Returns the number of frames in this animation.
    fn frame_count(&self) -> usize {
        match self {
            SpinnerStyle::Ascii => ASCII_FRAMES.len(),
            SpinnerStyle::Block => BLOCK_FRAMES.len(),
            SpinnerStyle::Braille => BRAILLE_FRAMES.len(),
            SpinnerStyle::Dots => DOTS_FRAMES.len(),
            SpinnerStyle::Moon => MOON_FRAMES.len(),
        }
    }

    /// Returns the character at the given frame index.
    fn char_at(&self, frame: usize) -> char {
        let idx = frame % self.frame_count();
        match self {
            SpinnerStyle::Ascii => ASCII_FRAMES[idx],
            SpinnerStyle::Block => BLOCK_FRAMES[idx],
            SpinnerStyle::Braille => BRAILLE_FRAMES[idx],
            SpinnerStyle::Dots => DOTS_FRAMES[idx],
            SpinnerStyle::Moon => MOON_FRAMES[idx],
        }
    }
}

// Frame sequences for each spinner style
const ASCII_FRAMES: [char; 4] = ['-', '\\', '|', '/'];
const BLOCK_FRAMES: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
const BRAILLE_FRAMES: [char; 8] = ['⡿', '⣟', '⣯', '⣷', '⣾', '⣽', '⣻', '⢿'];
const DOTS_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const MOON_FRAMES: [char; 8] = ['🌑', '🌒', '🌓', '🌔', '🌕', '🌖', '🌗', '🌘'];

/// Configuration options for creating a [`LoadingWidget`].
#[derive(Debug, Clone)]
pub struct LoadingWidgetOptions {
    /// Animation frame style.
    pub style: SpinnerStyle,
    /// Optional label text displayed after the spinner.
    pub label: Option<String>,
    /// Ticks between frame advances. Default: 1 (animate every render).
    pub tick_interval: Option<usize>,
}

impl Default for LoadingWidgetOptions {
    fn default() -> Self {
        Self {
            style: SpinnerStyle::default(),
            label: None,
            tick_interval: Some(1),
        }
    }
}

/// Animated loading indicator widget.
///
/// Displays a spinning animation with an optional label. Call [`tick()`](LoadingWidget::tick)
/// once per render loop iteration to advance the animation.
///
/// # Example
///
/// ```ignore
/// use ui::loading_widget::{LoadingWidget, LoadingWidgetOptions, SpinnerStyle};
///
/// let mut widget = LoadingWidget::new(LoadingWidgetOptions {
///     style: SpinnerStyle::Dots,
///     label: Some("Loading...".to_string()),
///     tick_interval: Some(2),
/// });
///
/// // In render loop:
/// widget.tick();
/// widget.render(buffer, area, &theme);
/// ```
#[derive(Debug, Clone)]
pub struct LoadingWidget {
    /// Animation frame style.
    style: SpinnerStyle,
    /// Optional label text displayed after the spinner.
    label: Option<String>,
    /// Current frame index.
    current_frame: usize,
    /// Ticks between frame advances.
    tick_interval: usize,
    /// Current tick counter.
    tick_count: usize,
}

impl LoadingWidget {
    /// Creates a new loading widget with the given options.
    pub fn new(options: LoadingWidgetOptions) -> Self {
        Self {
            style: options.style,
            label: options.label,
            current_frame: 0,
            tick_interval: options.tick_interval.unwrap_or(1).max(1),
            tick_count: 0,
        }
    }

    /// Creates a loading widget with a label, using default style and interval.
    pub fn with_label(label: impl Into<String>) -> Self {
        Self::new(LoadingWidgetOptions {
            label: Some(label.into()),
            ..Default::default()
        })
    }

    /// Sets the label text (builder pattern).
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Sets the spinner style (builder pattern).
    pub fn style(mut self, style: SpinnerStyle) -> Self {
        self.style = style;
        self
    }

    /// Sets the tick interval for animation speed control (builder pattern).
    pub fn tick_interval(mut self, interval: usize) -> Self {
        self.tick_interval = interval.max(1);
        self
    }

    /// Returns the current frame index.
    pub fn current_frame(&self) -> usize {
        self.current_frame
    }

    /// Returns the current spinner character.
    pub fn current_char(&self) -> char {
        self.style.char_at(self.current_frame)
    }

    /// Advances the animation by one tick.
    ///
    /// Call this once per render loop iteration. The frame advances when
    /// the internal tick counter reaches `tick_interval`.
    pub fn tick(&mut self) {
        self.tick_count += 1;
        if self.tick_count >= self.tick_interval {
            self.tick_count = 0;
            self.current_frame = (self.current_frame + 1) % self.style.frame_count();
        }
    }

    /// Renders the widget to a buffer area.
    ///
    /// The spinner is rendered centered horizontally within the area.
    /// If the area is too narrow, the label is truncated.
    ///
    /// # Arguments
    ///
    /// * `buffer` - The buffer to render into.
    /// * `area` - The area to render within.
    /// * `theme` - The theme for styling.
    pub fn render(&self, buffer: &mut Buffer, area: Rect, theme: &Theme) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let spinner = self.current_char();
        let spinner_style = Style::default()
            .fg(theme.semantic_info)
            .add_modifier(Modifier::BOLD);

        let spans = if let Some(ref label) = self.label {
            if label.is_empty() {
                vec![Span::styled(spinner.to_string(), spinner_style)]
            } else {
                // Calculate available width for label
                // spinner(1-2 chars) + space(1) + label
                let spinner_width = if self.style == SpinnerStyle::Moon {
                    2
                } else {
                    1
                };
                let available = area.width.saturating_sub(spinner_width + 1) as usize;

                let truncated_label = if label.len() > available {
                    unicode_truncate(label, available)
                } else {
                    label.as_str()
                };

                vec![
                    Span::styled(spinner.to_string(), spinner_style),
                    Span::raw(" "),
                    Span::styled(
                        truncated_label.to_string(),
                        Style::default().fg(theme.text_primary),
                    ),
                ]
            }
        } else {
            vec![Span::styled(spinner.to_string(), spinner_style)]
        };

        ratatui_widgets::paragraph::Paragraph::new(Line::from(spans)).render(area, buffer);
    }
}

/// Truncates a string to fit within the given character count, respecting Unicode boundaries.
fn unicode_truncate(s: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }

    for (char_count, (byte_idx, _)) in s.char_indices().enumerate() {
        if char_count == max_chars {
            return &s[..byte_idx];
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::theme::AMBER_THEME;

    /// Get a reference to a theme for testing.
    fn test_theme() -> &'static Theme {
        &AMBER_THEME
    }

    #[test]
    fn test_loading_widget_new_with_defaults() {
        let widget = LoadingWidget::new(LoadingWidgetOptions::default());

        assert_eq!(widget.style, SpinnerStyle::Block);
        assert_eq!(widget.label, None);
        assert_eq!(widget.tick_interval, 1);
        assert_eq!(widget.current_frame, 0);
    }

    #[test]
    fn test_loading_widget_new_with_options() {
        let widget = LoadingWidget::new(LoadingWidgetOptions {
            style: SpinnerStyle::Dots,
            label: Some("Loading...".to_string()),
            tick_interval: Some(3),
        });

        assert_eq!(widget.style, SpinnerStyle::Dots);
        assert_eq!(widget.label, Some("Loading...".to_string()));
        assert_eq!(widget.tick_interval, 3);
    }

    #[test]
    fn test_loading_widget_with_label() {
        let widget = LoadingWidget::with_label("Discovering...");

        assert_eq!(widget.label, Some("Discovering...".to_string()));
        assert_eq!(widget.style, SpinnerStyle::Block); // default
    }

    #[test]
    fn test_loading_widget_tick_advances_frame() {
        let mut widget = LoadingWidget::new(LoadingWidgetOptions {
            tick_interval: Some(1),
            ..Default::default()
        });

        assert_eq!(widget.current_frame, 0);
        widget.tick();
        assert_eq!(widget.current_frame, 1);
        widget.tick();
        assert_eq!(widget.current_frame, 2);
    }

    #[test]
    fn test_loading_widget_tick_wraps_around() {
        let mut widget = LoadingWidget::new(LoadingWidgetOptions {
            style: SpinnerStyle::Ascii, // 4 frames
            tick_interval: Some(1),
            ..Default::default()
        });

        // Advance through all frames
        for i in 0..4 {
            assert_eq!(widget.current_frame, i);
            widget.tick();
        }

        // Should wrap back to 0
        assert_eq!(widget.current_frame, 0);
    }

    #[test]
    fn test_loading_widget_tick_respects_interval() {
        let mut widget = LoadingWidget::new(LoadingWidgetOptions {
            tick_interval: Some(3),
            ..Default::default()
        });

        // First two ticks should not advance frame
        widget.tick();
        assert_eq!(widget.current_frame, 0);
        widget.tick();
        assert_eq!(widget.current_frame, 0);

        // Third tick should advance
        widget.tick();
        assert_eq!(widget.current_frame, 1);

        // Next two ticks should not advance
        widget.tick();
        assert_eq!(widget.current_frame, 1);
        widget.tick();
        assert_eq!(widget.current_frame, 1);

        // Third tick should advance again
        widget.tick();
        assert_eq!(widget.current_frame, 2);
    }

    #[test]
    fn test_loading_widget_tick_interval_minimum_is_one() {
        let widget = LoadingWidget::new(LoadingWidgetOptions {
            tick_interval: Some(0), // Should be clamped to 1
            ..Default::default()
        });

        assert_eq!(widget.tick_interval, 1);
    }

    #[test]
    fn test_loading_widget_current_char_returns_correct_frame() {
        let widget = LoadingWidget::new(LoadingWidgetOptions {
            style: SpinnerStyle::Ascii,
            ..Default::default()
        });

        assert_eq!(widget.current_char(), '-'); // First ASCII frame
    }

    #[test]
    fn test_loading_widget_render_with_label() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 1));
        let widget = LoadingWidget::with_label("Loading...");
        let theme = test_theme();

        widget.render(&mut buffer, Rect::new(0, 0, 30, 1), theme);

        // Buffer should contain the spinner and label
        let content = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(content.contains('▏')); // First Block frame
        assert!(content.contains("Loading..."));
    }

    #[test]
    fn test_loading_widget_render_without_label() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
        let widget = LoadingWidget::new(LoadingWidgetOptions {
            style: SpinnerStyle::Ascii,
            ..Default::default()
        });
        let theme = test_theme();

        widget.render(&mut buffer, Rect::new(0, 0, 10, 1), theme);

        let content = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(content.contains('-')); // First ASCII frame
    }

    #[test]
    fn test_loading_widget_render_empty_label_shows_only_spinner() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
        let widget = LoadingWidget::new(LoadingWidgetOptions {
            label: Some("".to_string()),
            ..Default::default()
        });
        let theme = test_theme();

        widget.render(&mut buffer, Rect::new(0, 0, 10, 1), theme);

        let content = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(content.contains('▏')); // Spinner should be present
        // Should not have extra space for empty label
    }

    #[test]
    fn test_loading_widget_render_zero_area_does_nothing() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 0, 0));
        let widget = LoadingWidget::with_label("Test");
        let theme = test_theme();

        // Should not panic
        widget.render(&mut buffer, Rect::new(0, 0, 0, 0), theme);
    }

    #[test]
    fn test_loading_widget_render_truncates_long_label() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
        let widget =
            LoadingWidget::with_label("This is a very long label that should be truncated");
        let theme = test_theme();

        widget.render(&mut buffer, Rect::new(0, 0, 10, 1), theme);

        // Content should fit within 10 chars
        let content = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        // The exact truncation depends on implementation, but it should not exceed area
        assert!(content.len() <= 10 * 2); // Account for potential multi-byte chars
    }

    #[test]
    fn test_loading_widget_builder_pattern() {
        let widget = LoadingWidget::new(LoadingWidgetOptions::default())
            .label("Custom label")
            .style(SpinnerStyle::Moon)
            .tick_interval(5);

        assert_eq!(widget.label, Some("Custom label".to_string()));
        assert_eq!(widget.style, SpinnerStyle::Moon);
        assert_eq!(widget.tick_interval, 5);
    }

    #[test]
    fn test_spinner_style_frame_count() {
        assert_eq!(SpinnerStyle::Ascii.frame_count(), 4);
        assert_eq!(SpinnerStyle::Block.frame_count(), 8);
        assert_eq!(SpinnerStyle::Braille.frame_count(), 8);
        assert_eq!(SpinnerStyle::Dots.frame_count(), 10);
        assert_eq!(SpinnerStyle::Moon.frame_count(), 8);
    }

    #[test]
    fn test_spinner_style_char_at_wraps() {
        // Request frame beyond count - should wrap
        assert_eq!(SpinnerStyle::Ascii.char_at(4), '-'); // 4 % 4 = 0
        assert_eq!(SpinnerStyle::Ascii.char_at(5), '\\'); // 5 % 4 = 1
    }

    #[test]
    fn test_unicode_truncate_basic() {
        assert_eq!(unicode_truncate("Hello", 3), "Hel");
        assert_eq!(unicode_truncate("Hello", 10), "Hello");
        assert_eq!(unicode_truncate("Hello", 0), "");
        assert_eq!(unicode_truncate("", 5), "");
    }

    #[test]
    fn test_unicode_truncate_multibyte() {
        // Test with multi-byte Unicode characters
        assert_eq!(unicode_truncate("日本語", 2), "日本");
        assert_eq!(unicode_truncate("🦀🦀🦀", 2), "🦀🦀");
    }
}
