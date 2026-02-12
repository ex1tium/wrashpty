//! Compound commands panel with inner tabs (Discover + Schema).
//!
//! Wraps the existing `CommandPalettePanel` (project command discovery) and the
//! new `SchemaBrowserPanel` (schema tree view) behind inner Tab/Shift-Tab tabs.
//! This panel replaces `CommandPalettePanel` at tab index 2 in `TabbedPanel`.

use std::any::Any;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::Paragraph;

use super::command_palette::CommandPalettePanel;
use super::footer_bar::FooterEntry;
use super::glyphs::{GlyphSet, GlyphTier};
use super::panel::{Panel, PanelResult};
use super::schema_browser::SchemaBrowserPanel;
use super::theme::Theme;
use crate::history_store::HistoryStore;

/// Inner sub-tab indices.
const SUB_DISCOVER: usize = 0;
const SUB_SCHEMA: usize = 1;
const SUB_COUNT: usize = 2;

/// Inner sub-tab labels.
const SUB_LABELS: [&str; SUB_COUNT] = ["Discover", "Browser"];

/// Compound commands panel with inner tabs.
pub struct CommandsPanel {
    /// The Discover sub-panel (existing CommandPalettePanel).
    discover: CommandPalettePanel,
    /// The Schema browser sub-panel.
    schema: SchemaBrowserPanel,
    /// Currently active sub-tab.
    active_sub: usize,
    /// Theme for rendering.
    theme: &'static Theme,
    /// Unified glyph set for the current tier.
    glyphs: &'static GlyphSet,
}

impl CommandsPanel {
    /// Creates a new commands panel.
    pub fn new(theme: &'static Theme, glyph_tier: GlyphTier) -> Self {
        Self {
            discover: CommandPalettePanel::new(theme),
            schema: SchemaBrowserPanel::new(theme, glyph_tier),
            active_sub: SUB_DISCOVER,
            theme,
            glyphs: GlyphSet::for_tier(glyph_tier),
        }
    }

    /// Loads project commands into the Discover sub-panel.
    pub fn load_commands(&mut self, cwd: &Path) {
        self.discover.load_commands(cwd);
    }

    /// Returns a reference to the discovered command items from the Discover sub-panel.
    pub fn discovered_items(&self) -> &[super::command_palette::CommandItem] {
        self.discover.items()
    }

    /// Sets the history store for the Schema browser sub-panel.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.schema.set_history_store(store);
    }

    /// Switches to the next inner sub-tab.
    fn next_sub(&mut self) {
        self.active_sub = (self.active_sub + 1) % SUB_COUNT;
    }

    /// Switches to the previous inner sub-tab.
    fn prev_sub(&mut self) {
        self.active_sub = if self.active_sub == 0 {
            SUB_COUNT - 1
        } else {
            self.active_sub - 1
        };
    }

    /// Renders the inner tab bar (labels only — hint goes on separator).
    fn render_inner_tabs(&self, buffer: &mut Buffer, area: Rect) {
        let mut spans = Vec::new();

        for (i, label) in SUB_LABELS.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(
                    " ",
                    Style::default().fg(self.theme.text_secondary),
                ));
            }

            let style = if i == self.active_sub {
                Style::default()
                    .fg(self.theme.tab_active_fg)
                    .bg(self.theme.tab_active_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(self.theme.tab_inactive_fg)
                    .bg(self.theme.tab_inactive_bg)
            };

            spans.push(Span::styled(format!(" {} ", label), style));
        }

        Paragraph::new(Line::from(spans)).render(area, buffer);
    }

    /// Renders a hint overlaid on the right side of a separator line.
    fn render_separator_hint(&self, buffer: &mut Buffer, sep_area: Rect, hint: &str) {
        let hint_display_width = crate::ui::text_width::display_width(hint) as u16;
        let hint_start = sep_area.x + sep_area.width.saturating_sub(hint_display_width + 2);
        let mut col: u16 = 0;

        for ch in hint.chars() {
            let ch_w = unicode_width::UnicodeWidthChar::width(ch)
                .unwrap_or(1)
                .max(1) as u16;
            let x = hint_start + col;
            if x + ch_w <= sep_area.x + sep_area.width {
                if let Some(cell) = buffer.cell_mut((x, sep_area.y)) {
                    cell.set_char(ch);
                    cell.set_style(Style::default().fg(self.theme.text_secondary));
                }
                if ch_w > 1 {
                    for i in 1..ch_w {
                        let trailing_x = x + i;
                        if trailing_x < sep_area.x + sep_area.width {
                            if let Some(cell) = buffer.cell_mut((trailing_x, sep_area.y)) {
                                cell.set_char(' ');
                                cell.set_style(Style::default().fg(self.theme.text_secondary));
                            }
                        }
                    }
                }
            }
            col += ch_w;
        }
    }
}

impl Panel for CommandsPanel {
    fn preferred_height(&self) -> u16 {
        // Active sub-panel height + 2 for inner tab bar + separator
        let sub_height = match self.active_sub {
            SUB_DISCOVER => self.discover.preferred_height(),
            SUB_SCHEMA => self.schema.preferred_height(),
            _ => 10,
        };
        sub_height + 2
    }

    fn title(&self) -> &str {
        "Commands"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 4 || area.width < 10 {
            return;
        }

        // Layout: inner tab bar (1 line), separator (1 line), content (rest)
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

        // Render inner tab bar
        self.render_inner_tabs(buffer, chunks[0]);

        // Render separator with hint
        for x in chunks[1].x..chunks[1].x + chunks[1].width {
            if let Some(cell) = buffer.cell_mut((x, chunks[1].y)) {
                cell.set_char(self.glyphs.border.horizontal);
                cell.set_style(Style::default().fg(self.theme.panel_border));
            }
        }
        self.render_separator_hint(buffer, chunks[1], "Tab/S-Tab");

        // Delegate to active sub-panel
        match self.active_sub {
            SUB_DISCOVER => self.discover.render(buffer, chunks[2]),
            SUB_SCHEMA => self.schema.render(buffer, chunks[2]),
            _ => {}
        }
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // Tab / Shift+Tab switch inner sub-tabs (unless schema browser is in edit mode)
        let in_edit = self.active_sub == SUB_SCHEMA && self.schema.in_edit_mode();
        if !in_edit {
            match key.code {
                KeyCode::Tab => {
                    self.next_sub();
                    return PanelResult::Continue;
                }
                KeyCode::BackTab => {
                    self.prev_sub();
                    return PanelResult::Continue;
                }
                _ => {}
            }
        }

        // Delegate to active sub-panel
        match self.active_sub {
            SUB_DISCOVER => self.discover.handle_input(key),
            SUB_SCHEMA => self.schema.handle_input(key),
            _ => PanelResult::Continue,
        }
    }

    fn footer_entries(&self) -> Vec<FooterEntry> {
        match self.active_sub {
            SUB_DISCOVER => self.discover.footer_entries(),
            SUB_SCHEMA => self.schema.footer_entries(),
            _ => Vec::new(),
        }
    }

    fn border_info(&self) -> Option<String> {
        match self.active_sub {
            SUB_DISCOVER => self.discover.border_info(),
            SUB_SCHEMA => self.schema.border_info(),
            _ => None,
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_glyph_tier(&mut self, tier: super::glyphs::GlyphTier) {
        self.glyphs = super::glyphs::GlyphSet::for_tier(tier);
        self.schema.set_glyph_tier(tier);
    }

    fn theme(&self) -> &'static super::theme::Theme {
        self.theme
    }

    fn set_theme(&mut self, theme: &'static super::theme::Theme) {
        self.theme = theme;
        self.discover.set_theme(theme);
        self.schema.set_theme(theme);
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    fn test_commands_panel_new_initializes_active_sub_and_title() {
        let panel = CommandsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.active_sub, SUB_DISCOVER);
        assert_eq!(panel.title(), "Commands");
    }

    #[test]
    fn test_commands_panel_next_prev_sub_wraps_correctly() {
        let mut panel = CommandsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.active_sub, SUB_DISCOVER);

        panel.next_sub();
        assert_eq!(panel.active_sub, SUB_SCHEMA);

        panel.next_sub();
        assert_eq!(panel.active_sub, SUB_DISCOVER); // Wraps

        panel.prev_sub();
        assert_eq!(panel.active_sub, SUB_SCHEMA); // Wraps back
    }

    #[test]
    fn test_commands_panel_handle_input_tab_and_backtab_switches_subs() {
        let mut panel = CommandsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.active_sub, SUB_DISCOVER);

        let tab_key = KeyEvent::from(KeyCode::Tab);
        let result = panel.handle_input(tab_key);
        assert!(matches!(result, PanelResult::Continue));
        assert_eq!(panel.active_sub, SUB_SCHEMA);

        let backtab_key = KeyEvent::from(KeyCode::BackTab);
        let result = panel.handle_input(backtab_key);
        assert!(matches!(result, PanelResult::Continue));
        assert_eq!(panel.active_sub, SUB_DISCOVER);
    }

    #[test]
    fn test_commands_panel_preferred_height_includes_subpanel_and_tabs() {
        let panel = CommandsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        // Should be sub-panel height + 2 (tab bar + separator)
        let discover_height = panel.discover.preferred_height();
        assert_eq!(panel.preferred_height(), discover_height + 2);
    }
}
