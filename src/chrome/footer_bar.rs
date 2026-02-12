//! Footer bar widget system for panel keybind hints.
//!
//! Mirrors the topbar segment architecture (`TopbarSegment`, `RenderedSegment`,
//! `TopbarRegistry`) using ratatui primitives instead of raw ANSI.
//!
//! # Architecture
//!
//! - `FooterEntry`: Pure data — mirrors `RenderedSegment` (kind + priority)
//! - `FooterKind`: Styling variant enum — determines how `FooterBar` renders the entry
//! - `FooterBar`: Compositor `Widget` — mirrors `TopbarRegistry::render()` with
//!   priority-based overflow truncation
//! - `BorderLine`: Standalone `Widget` — DRYs the `─` border + optional right-aligned info

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::Paragraph;

use super::theme::Theme;

// ============================================================================
// Data types (mirrors RenderedSegment)
// ============================================================================

/// Styling variant for a footer entry. Determines how `FooterBar` renders it.
///
/// Analogous to `RenderedSegment::align` determining layout behavior in the
/// topbar segment system.
#[derive(Debug, Clone)]
pub enum FooterKind {
    /// A key-label action (e.g., "Enter Run").
    Action {
        key: &'static str,
        label: &'static str,
    },
    /// A togglable state — highlighted when active (e.g., "^D Dedupe").
    Toggle {
        key: &'static str,
        label: &'static str,
        active: bool,
    },
    /// A key-value display (e.g., "^S Frecency").
    Value { key: &'static str, value: String },
    /// An informational message (no key).
    Message(String),
}

/// A single footer entry. Pure data — no rendering logic.
///
/// Mirrors `RenderedSegment { content, display_width, priority, align }` from
/// the topbar segment system. Panels produce these; `FooterBar` renders them.
#[derive(Debug, Clone)]
pub struct FooterEntry {
    /// The variant determining how `FooterBar` styles this entry.
    pub kind: FooterKind,
    /// Priority for overflow truncation (0 = critical, higher = drop first).
    /// Mirrors `RenderedSegment::priority`.
    pub priority: u8,
}

impl FooterEntry {
    /// Creates an action entry (key + label). Default priority 1.
    pub fn action(key: &'static str, label: &'static str) -> Self {
        Self {
            kind: FooterKind::Action { key, label },
            priority: 1,
        }
    }

    /// Creates a toggle entry (highlighted when active). Default priority 2.
    pub fn toggle(key: &'static str, label: &'static str, active: bool) -> Self {
        Self {
            kind: FooterKind::Toggle { key, label, active },
            priority: 2,
        }
    }

    /// Creates a value display entry (key + dynamic value). Default priority 2.
    pub fn value(key: &'static str, value: String) -> Self {
        Self {
            kind: FooterKind::Value { key, value },
            priority: 2,
        }
    }

    /// Creates an informational message entry. Default priority 3.
    pub fn message(msg: impl Into<String>) -> Self {
        Self {
            kind: FooterKind::Message(msg.into()),
            priority: 3,
        }
    }

    /// Overrides the default priority (builder pattern).
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Calculates the display width of this entry (excluding inter-entry gaps).
    fn display_width(&self) -> u16 {
        use crate::ui::text_width::display_width;
        match &self.kind {
            FooterKind::Action { key, label } => {
                // "key label"
                display_width(key) as u16 + 1 + display_width(label) as u16
            }
            FooterKind::Toggle { key, label, .. } => {
                // "key label"
                display_width(key) as u16 + 1 + display_width(label) as u16
            }
            FooterKind::Value { key, value } => {
                // "key value"
                display_width(key) as u16 + 1 + display_width(value) as u16
            }
            FooterKind::Message(msg) => display_width(msg) as u16,
        }
    }
}

// ============================================================================
// FooterBar widget (mirrors TopbarRegistry::render())
// ============================================================================

/// Gap width between footer entries (matches existing codebase convention).
const ENTRY_GAP: u16 = 2;

/// Footer bar widget. Renders `FooterEntry` items into a ratatui `Buffer`.
///
/// Mirrors `TopbarRegistry::render()` — handles priority-based overflow
/// truncation when entries exceed available width. Composable standalone
/// `Widget` — not coupled to `Panel` or `TabbedPanel`.
pub struct FooterBar<'a> {
    entries: &'a [FooterEntry],
    theme: &'static Theme,
}

impl<'a> FooterBar<'a> {
    /// Creates a new footer bar widget.
    pub fn new(entries: &'a [FooterEntry], theme: &'static Theme) -> Self {
        Self { entries, theme }
    }

    /// Calculates total display width including inter-entry gaps.
    fn total_width(widths: &[u16]) -> u16 {
        let content: u16 = widths.iter().sum();
        let gaps = widths.len().saturating_sub(1) as u16 * ENTRY_GAP;
        content + gaps
    }

    /// Styles a single entry into spans.
    fn style_entry(&self, entry: &FooterEntry) -> Vec<Span<'static>> {
        let key_style = Style::default().fg(self.theme.text_highlight);
        let label_style = Style::default().fg(self.theme.text_secondary);
        let active_style = Style::default()
            .fg(self.theme.semantic_success)
            .add_modifier(Modifier::BOLD);
        let value_style = Style::default().fg(self.theme.header_fg);

        match &entry.kind {
            FooterKind::Action { key, label } => {
                vec![
                    Span::styled(*key, key_style),
                    Span::styled(format!(" {}", label), label_style),
                ]
            }
            FooterKind::Toggle { key, label, active } => {
                let style = if *active { active_style } else { label_style };
                vec![
                    Span::styled(*key, key_style),
                    Span::styled(format!(" {}", label), style),
                ]
            }
            FooterKind::Value { key, value } => {
                vec![
                    Span::styled(*key, key_style),
                    Span::styled(format!(" {}", value), value_style),
                ]
            }
            FooterKind::Message(msg) => {
                vec![Span::styled(msg.clone(), label_style)]
            }
        }
    }
}

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.entries.is_empty() {
            return;
        }

        // Calculate display widths for all entries
        let mut entries: Vec<(usize, u16)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.display_width()))
            .collect();

        // Priority-based truncation (mirrors TopbarRegistry algorithm):
        // Drop entries with highest priority number first until we fit.
        while Self::total_width(&entries.iter().map(|(_, w)| *w).collect::<Vec<_>>()) > area.width
            && entries.len() > 1
        {
            // Find entry with highest priority number (least critical)
            let max_idx = entries
                .iter()
                .enumerate()
                .max_by_key(|(_, (orig_idx, _))| self.entries[*orig_idx].priority)
                .map(|(i, _)| i);

            if let Some(idx) = max_idx {
                entries.remove(idx);
            } else {
                break;
            }
        }

        // Build spans from surviving entries
        let mut all_spans: Vec<Span<'static>> = Vec::new();
        for (i, (orig_idx, _)) in entries.iter().enumerate() {
            if i > 0 {
                all_spans.push(Span::raw("  "));
            }
            all_spans.extend(self.style_entry(&self.entries[*orig_idx]));
        }

        Paragraph::new(Line::from(all_spans)).render(area, buf);
    }
}

// ============================================================================
// BorderLine widget
// ============================================================================

/// Horizontal border line with optional right-aligned info text.
///
/// DRYs the `─` border rendering pattern duplicated across 5+ sites in the
/// codebase. Standalone `Widget` — usable by `TabbedPanel`, individual panels,
/// or any future compositor.
pub struct BorderLine<'a> {
    theme: &'static Theme,
    info: Option<&'a str>,
    border_char: char,
}

impl<'a> BorderLine<'a> {
    /// Creates a new border line widget.
    pub fn new(theme: &'static Theme, border_char: char) -> Self {
        Self {
            theme,
            info: None,
            border_char,
        }
    }

    /// Sets right-aligned info text to overlay on the border.
    pub fn with_info(mut self, info: &'a str) -> Self {
        self.info = Some(info);
        self
    }
}

impl Widget for BorderLine<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let border_style = Style::default().fg(self.theme.panel_border);

        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_char(self.border_char);
                cell.set_style(border_style);
            }
        }

        // Overlay right-aligned info text if present
        if let Some(info) = self.info {
            let info_display_width = crate::ui::text_width::display_width(info) as u16;
            let padded_width = info_display_width + 2; // " info "
            let info_start = area.x + area.width.saturating_sub(padded_width + 1);
            let info_style = Style::default().fg(self.theme.text_secondary);

            let mut col: u16 = 0;
            // Leading space
            if let Some(cell) = buf.cell_mut((info_start, area.y)) {
                cell.set_char(' ');
                cell.set_style(info_style);
            }
            col += 1;

            for ch in info.chars() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch)
                    .unwrap_or(1)
                    .max(1) as u16;
                let x = info_start + col;
                if x + ch_w <= area.x + area.width {
                    if let Some(cell) = buf.cell_mut((x, area.y)) {
                        cell.set_char(ch);
                        cell.set_style(info_style);
                    }
                    // Clear continuation cells for wide chars
                    if ch_w > 1 {
                        for i in 1..ch_w {
                            let trailing_x = x + i;
                            if trailing_x < area.x + area.width {
                                if let Some(cell) = buf.cell_mut((trailing_x, area.y)) {
                                    cell.set_char(' ');
                                    cell.set_style(info_style);
                                }
                            }
                        }
                    }
                }
                col += ch_w;
            }

            // Trailing space
            let trail_x = info_start + col;
            if trail_x < area.x + area.width {
                if let Some(cell) = buf.cell_mut((trail_x, area.y)) {
                    cell.set_char(' ');
                    cell.set_style(info_style);
                }
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::theme::AMBER_THEME;

    #[test]
    fn test_footer_entry_action_defaults() {
        let entry = FooterEntry::action("Enter", "Run");
        assert_eq!(entry.priority, 1);
        assert!(matches!(
            entry.kind,
            FooterKind::Action {
                key: "Enter",
                label: "Run"
            }
        ));
    }

    #[test]
    fn test_footer_entry_toggle_defaults() {
        let entry = FooterEntry::toggle("^D", "Dedupe", true);
        assert_eq!(entry.priority, 2);
        assert!(matches!(
            entry.kind,
            FooterKind::Toggle {
                key: "^D",
                label: "Dedupe",
                active: true,
            }
        ));
    }

    #[test]
    fn test_footer_entry_value_defaults() {
        let entry = FooterEntry::value("^S", "Frecency".into());
        assert_eq!(entry.priority, 2);
    }

    #[test]
    fn test_footer_entry_message_defaults() {
        let entry = FooterEntry::message("hello");
        assert_eq!(entry.priority, 3);
    }

    #[test]
    fn test_footer_entry_with_priority_override() {
        let entry = FooterEntry::action("Esc", "Close").with_priority(0);
        assert_eq!(entry.priority, 0);
    }

    #[test]
    fn test_footer_entry_display_width() {
        let action = FooterEntry::action("^E", "Edit");
        // "^E" (2) + " " (1) + "Edit" (4) = 7
        assert_eq!(action.display_width(), 7);

        let toggle = FooterEntry::toggle("^D", "Dedupe", false);
        // "^D" (2) + " " (1) + "Dedupe" (6) = 9
        assert_eq!(toggle.display_width(), 9);

        let msg = FooterEntry::message("hello world");
        assert_eq!(msg.display_width(), 11);
    }

    #[test]
    fn test_footer_bar_renders_into_buffer() {
        let entries = vec![
            FooterEntry::action("Enter", "Run"),
            FooterEntry::action("Esc", "Close"),
        ];
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);

        FooterBar::new(&entries, &AMBER_THEME).render(area, &mut buf);

        // Check that content was written
        let content: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(content.contains("Enter"));
        assert!(content.contains("Run"));
        assert!(content.contains("Esc"));
        assert!(content.contains("Close"));
    }

    #[test]
    fn test_footer_bar_empty_entries_renders_nothing() {
        let entries: Vec<FooterEntry> = vec![];
        let area = Rect::new(0, 0, 40, 1);
        let mut buf = Buffer::empty(area);

        FooterBar::new(&entries, &AMBER_THEME).render(area, &mut buf);

        // Buffer should remain empty (spaces)
        let content: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(content.trim().is_empty());
    }

    #[test]
    fn test_footer_bar_priority_truncation_drops_highest_priority_number() {
        // Create entries that exceed 20 columns
        let entries = vec![
            FooterEntry::action("Enter", "Run").with_priority(0), // 9 cols, critical
            FooterEntry::toggle("^D", "Dedupe", true).with_priority(2), // 9 cols
            FooterEntry::action("Esc", "Close").with_priority(1), // 9 cols
        ];
        // Total: 9 + 2 + 9 + 2 + 9 = 31 cols

        let area = Rect::new(0, 0, 22, 1);
        let mut buf = Buffer::empty(area);

        FooterBar::new(&entries, &AMBER_THEME).render(area, &mut buf);

        let content: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();

        // Priority 2 (Dedupe) should be dropped first
        assert!(content.contains("Enter"));
        assert!(content.contains("Esc"));
        assert!(!content.contains("Dedupe"));
    }

    #[test]
    fn test_border_line_renders_horizontal_line() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);

        BorderLine::new(&AMBER_THEME, '─').render(area, &mut buf);

        for x in 0..20 {
            let cell = buf.cell((x, 0)).unwrap();
            assert_eq!(cell.symbol(), "─");
        }
    }

    #[test]
    fn test_border_line_with_info_overlays_right_aligned() {
        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);

        BorderLine::new(&AMBER_THEME, '─')
            .with_info("sort:name")
            .render(area, &mut buf);

        let content: String = (0..area.width)
            .filter_map(|x| buf.cell((x, 0)).map(|c| c.symbol().to_string()))
            .collect();

        // Left side should be ─, right side should contain info
        assert!(content.contains("sort:name"));
        assert!(content.starts_with('─'));
    }

    #[test]
    fn test_border_line_no_info_is_all_border() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);

        BorderLine::new(&AMBER_THEME, '─').render(area, &mut buf);

        for x in 0..10 {
            assert_eq!(buf.cell((x, 0)).unwrap().symbol(), "─");
        }
    }

    #[test]
    fn test_total_width_calculation() {
        // 3 entries of width 5 each: 5 + 2 + 5 + 2 + 5 = 19
        assert_eq!(FooterBar::total_width(&[5, 5, 5]), 19);

        // 1 entry: no gaps
        assert_eq!(FooterBar::total_width(&[10]), 10);

        // Empty
        assert_eq!(FooterBar::total_width(&[]), 0);
    }
}
