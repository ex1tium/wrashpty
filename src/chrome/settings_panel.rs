//! Settings panel with inner subtabs (Settings + Help).
//!
//! Follows the `CommandsPanel` compound container pattern with inner Tab/Shift-Tab
//! tabs. This panel provides a unified settings and help interface.

use std::any::Any;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::Paragraph;

use super::footer_bar::FooterEntry;
use super::glyphs::{GlyphSet, GlyphTier};
use super::help_view::HelpView;
use super::panel::{Panel, PanelResult};
use super::settings_view::{SettingAction, SettingsView};
use super::theme::Theme;
use crate::history_store::HistoryStore;

/// Inner sub-tab indices.
const SUB_SETTINGS: usize = 0;
const SUB_HELP: usize = 1;
const SUB_COUNT: usize = 2;

/// Inner sub-tab labels.
const SUB_LABELS: [&str; SUB_COUNT] = ["Settings", "Help"];

/// Compound settings panel with inner tabs.
pub struct SettingsPanel {
    /// The Settings sub-view.
    settings_view: SettingsView,
    /// The Help sub-view.
    help_view: HelpView,
    /// Currently active sub-tab.
    active_sub: usize,
    /// Pending runtime actions from setting changes.
    pending_actions: Vec<SettingAction>,
    /// Theme for rendering.
    theme: &'static Theme,
    /// Unified glyph set for the current tier.
    glyphs: &'static GlyphSet,
}

impl SettingsPanel {
    /// Creates a new settings panel.
    pub fn new(theme: &'static Theme, glyph_tier: GlyphTier) -> Self {
        Self {
            settings_view: SettingsView::new(theme, glyph_tier),
            help_view: HelpView::new(theme, glyph_tier),
            active_sub: SUB_SETTINGS,
            pending_actions: Vec::new(),
            theme,
            glyphs: GlyphSet::for_tier(glyph_tier),
        }
    }

    /// Sets the history store for settings persistence.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.settings_view.set_history_store(store);
    }

    /// Loads command documentation into the help view.
    pub fn load_command_docs(&mut self, commands: Vec<(&str, &[&str], &str)>) {
        self.help_view.load_command_docs(commands);
    }

    /// Loads discovered project commands into the help view.
    pub fn load_project_commands(&mut self, items: &[super::command_palette::CommandItem]) {
        self.help_view.load_project_commands(items);
    }

    /// Switches to the help subtab.
    pub fn switch_to_help(&mut self) {
        self.active_sub = SUB_HELP;
    }

    /// Takes any pending runtime actions, draining the queue.
    pub fn take_pending_actions(&mut self) -> Vec<SettingAction> {
        std::mem::take(&mut self.pending_actions)
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

    /// Renders the inner tab bar.
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

impl Panel for SettingsPanel {
    fn preferred_height(&self) -> u16 {
        // Active sub-view height + 2 for inner tab bar + separator
        let sub_height = match self.active_sub {
            SUB_SETTINGS => 12,
            SUB_HELP => 15,
            _ => 12,
        };
        sub_height + 2
    }

    fn title(&self) -> &str {
        "Settings"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 4 || area.width < 10 {
            return;
        }

        // Layout: inner tab bar (1 line), separator (1 line), content (rest)
        let chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
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

        // Delegate to active sub-view
        match self.active_sub {
            SUB_SETTINGS => self.settings_view.render(buffer, chunks[2]),
            SUB_HELP => self.help_view.render(buffer, chunks[2]),
            _ => {}
        }
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // Don't allow tab switching when settings view is editing
        let in_edit = self.active_sub == SUB_SETTINGS && self.settings_view.is_editing();
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

        // Delegate to active sub-view
        let result = match self.active_sub {
            SUB_SETTINGS => {
                if self.settings_view.handle_input(key) {
                    PanelResult::Continue
                } else {
                    match key.code {
                        KeyCode::Esc => PanelResult::Dismiss,
                        _ => PanelResult::Continue,
                    }
                }
            }
            SUB_HELP => {
                if self.help_view.handle_input(key) {
                    PanelResult::Continue
                } else {
                    match key.code {
                        KeyCode::Esc => PanelResult::Dismiss,
                        _ => PanelResult::Continue,
                    }
                }
            }
            _ => PanelResult::Continue,
        };

        // Drain setting actions from the settings view and apply locally for visual feedback
        for action in self.settings_view.take_pending_actions() {
            match &action {
                SettingAction::SetGlyphTier(tier) => {
                    self.set_glyph_tier(*tier);
                }
                SettingAction::SetTheme(preset) => {
                    let theme = super::theme::Theme::for_preset(*preset);
                    self.set_theme(theme);
                }
                _ => {}
            }
            self.pending_actions.push(action);
        }

        result
    }

    fn footer_entries(&self) -> Vec<FooterEntry> {
        match self.active_sub {
            SUB_SETTINGS => self.settings_view.footer_entries(),
            SUB_HELP => self.help_view.footer_entries(),
            _ => Vec::new(),
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_glyph_tier(&mut self, tier: GlyphTier) {
        self.glyphs = GlyphSet::for_tier(tier);
        self.settings_view.set_glyph_tier(tier);
        self.help_view.set_glyph_tier(tier);
    }

    fn set_theme(&mut self, theme: &'static Theme) {
        self.theme = theme;
        self.settings_view.set_theme(theme);
        self.help_view.set_theme(theme);
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    fn test_settings_panel_new_initializes_correctly() {
        let panel = SettingsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.active_sub, SUB_SETTINGS);
        assert_eq!(panel.title(), "Settings");
    }

    #[test]
    fn test_settings_panel_next_prev_sub_wraps() {
        let mut panel = SettingsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.active_sub, SUB_SETTINGS);

        panel.next_sub();
        assert_eq!(panel.active_sub, SUB_HELP);

        panel.next_sub();
        assert_eq!(panel.active_sub, SUB_SETTINGS); // Wraps

        panel.prev_sub();
        assert_eq!(panel.active_sub, SUB_HELP); // Wraps back
    }

    #[test]
    fn test_settings_panel_tab_switches_subs() {
        let mut panel = SettingsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.active_sub, SUB_SETTINGS);

        let tab_key = KeyEvent::from(KeyCode::Tab);
        let result = panel.handle_input(tab_key);
        assert!(matches!(result, PanelResult::Continue));
        assert_eq!(panel.active_sub, SUB_HELP);

        let backtab_key = KeyEvent::from(KeyCode::BackTab);
        let result = panel.handle_input(backtab_key);
        assert!(matches!(result, PanelResult::Continue));
        assert_eq!(panel.active_sub, SUB_SETTINGS);
    }

    #[test]
    fn test_settings_panel_switch_to_help() {
        let mut panel = SettingsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        panel.switch_to_help();
        assert_eq!(panel.active_sub, SUB_HELP);
    }

    #[test]
    fn test_settings_panel_preferred_height_includes_tabs() {
        let panel = SettingsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert!(panel.preferred_height() >= 14); // 12 content + 2 tab bar
    }

    #[test]
    fn test_settings_panel_footer_entries_delegate() {
        let panel = SettingsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        let entries = panel.footer_entries();
        assert!(!entries.is_empty());
    }

    #[test]
    fn test_settings_panel_esc_dismisses_from_help() {
        let mut panel = SettingsPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        panel.switch_to_help();
        let result = panel.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(result, PanelResult::Dismiss));
    }
}
