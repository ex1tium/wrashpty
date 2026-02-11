//! Tabbed panel system for organizing multiple panels.

use std::any::Any;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::tabs::Tabs;

use super::commands_panel::CommandsPanel;
use super::file_browser::FileBrowserPanel;
use super::footer_bar::{BorderLine, FooterBar};
use super::history_browser::HistoryBrowserPanel;
use super::panel::{Panel, PanelResult};
use super::settings_panel::SettingsPanel;
use super::settings_view::SettingAction;
use super::theme::Theme;
use super::glyphs::{GlyphSet, GlyphTier};
use crate::history_store::HistoryStore;

// Tab indices for type-based access
const TAB_HISTORY_BROWSER: usize = 0;
const TAB_FILE_BROWSER: usize = 1;
const TAB_COMMAND_PALETTE: usize = 2;
const TAB_SETTINGS: usize = 3;

/// A tabbed container for multiple panels.
pub struct TabbedPanel {
    /// Panel instances.
    tabs: Vec<Box<dyn Panel>>,
    /// Currently selected tab index.
    active_tab: usize,
    /// Reference to history store for settings persistence.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Pending runtime actions from settings changes.
    pending_actions: Vec<SettingAction>,
    /// Theme for rendering.
    theme: &'static Theme,
    /// Unified glyph set for the current tier.
    glyphs: &'static GlyphSet,
}

impl TabbedPanel {
    /// Creates a new tabbed panel with all panel types.
    pub fn new(theme: &'static Theme, glyph_tier: GlyphTier) -> Self {
        let glyphs = GlyphSet::for_tier(glyph_tier);
        let tabs: Vec<Box<dyn Panel>> = vec![
            Box::new(HistoryBrowserPanel::new(theme, glyphs)),
            Box::new(FileBrowserPanel::new(theme, glyph_tier)),
            Box::new(CommandsPanel::new(theme, glyph_tier)),
            Box::new(SettingsPanel::new(theme, glyph_tier)),
        ];

        Self {
            tabs,
            active_tab: 0,
            history_store: None,
            pending_actions: Vec::new(),
            theme,
            glyphs,
        }
    }

    /// Sets the history store for panels that need it.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        // Store reference for settings persistence
        self.history_store = Some(Arc::clone(&store));

        // Load last active tab from settings
        if let Ok(guard) = store.lock() {
            if let Ok(Some(tab_str)) = guard.get_setting("last_active_tab") {
                if let Ok(tab_idx) = tab_str.parse::<usize>() {
                    if tab_idx < self.tabs.len() {
                        self.active_tab = tab_idx;
                    }
                }
            }
        }

        // Pass store to history browser panel
        if let Some(panel) = self.tabs.get_mut(TAB_HISTORY_BROWSER) {
            if let Some(hist_panel) = panel.as_any_mut().downcast_mut::<HistoryBrowserPanel>() {
                hist_panel.set_history_store(Arc::clone(&store));
            }
        }

        // Pass store to file browser panel for intelligent suggestions
        if let Some(panel) = self.tabs.get_mut(TAB_FILE_BROWSER) {
            if let Some(file_panel) = panel.as_any_mut().downcast_mut::<FileBrowserPanel>() {
                file_panel.set_history_store(Arc::clone(&store));
            }
        }

        // Pass store to commands panel (for schema browser)
        if let Some(panel) = self.tabs.get_mut(TAB_COMMAND_PALETTE) {
            if let Some(cmd_panel) = panel.as_any_mut().downcast_mut::<CommandsPanel>() {
                cmd_panel.set_history_store(Arc::clone(&store));
            }
        }

        // Pass store to settings panel (for persistence)
        if let Some(panel) = self.tabs.get_mut(TAB_SETTINGS) {
            if let Some(settings_panel) = panel.as_any_mut().downcast_mut::<SettingsPanel>() {
                settings_panel.set_history_store(store);
            }
        }
    }

    /// Saves the current active tab to settings.
    fn save_active_tab(&self) {
        if let Some(store) = &self.history_store {
            if let Ok(guard) = store.lock() {
                let _ = guard.set_setting("last_active_tab", &self.active_tab.to_string());
            }
        }
    }

    /// Loads context for all panels based on the current working directory.
    pub fn load_context(&mut self, cwd: &Path) {
        // Load commands for commands panel (Discover sub-tab) and collect discovered items
        let mut discovered = Vec::new();
        if let Some(panel) = self.tabs.get_mut(TAB_COMMAND_PALETTE) {
            if let Some(cmd_panel) = panel.as_any_mut().downcast_mut::<CommandsPanel>() {
                cmd_panel.load_commands(cwd);
                discovered = cmd_panel.discovered_items().to_vec();
            }
        }

        // Auto-load colon command docs and project commands into Settings>Help
        if let Some(panel) = self.tabs.get_mut(TAB_SETTINGS) {
            if let Some(settings_panel) = panel.as_any_mut().downcast_mut::<SettingsPanel>() {
                let cmd_list = crate::app::commands::CommandRegistry::new().command_list();
                settings_panel.load_command_docs(cmd_list);
                if !discovered.is_empty() {
                    settings_panel.load_project_commands(&discovered);
                }
            }
        }

        // Set cwd for file browser
        if let Some(panel) = self.tabs.get_mut(TAB_FILE_BROWSER) {
            if let Some(file_panel) = panel.as_any_mut().downcast_mut::<FileBrowserPanel>() {
                let _ = file_panel.navigate_to(cwd);
            }
        }

        // Load history for history browser with cwd context
        if let Some(panel) = self.tabs.get_mut(TAB_HISTORY_BROWSER) {
            if let Some(hist_panel) = panel.as_any_mut().downcast_mut::<HistoryBrowserPanel>() {
                hist_panel.set_cwd(cwd.to_path_buf());
                hist_panel.load_history();
            }
        }
    }

    /// Loads command documentation into the settings panel's help view.
    pub fn load_command_docs(&mut self, commands: Vec<(&str, &[&str], &str)>) {
        if let Some(panel) = self.tabs.get_mut(TAB_SETTINGS) {
            if let Some(settings_panel) = panel.as_any_mut().downcast_mut::<SettingsPanel>() {
                settings_panel.load_command_docs(commands);
            }
        }
    }

    /// Switches to the Settings tab on the Help subtab.
    pub fn switch_to_settings_help(&mut self) {
        self.active_tab = TAB_SETTINGS;
        self.save_active_tab();
        if let Some(panel) = self.tabs.get_mut(TAB_SETTINGS) {
            if let Some(settings_panel) = panel.as_any_mut().downcast_mut::<SettingsPanel>() {
                settings_panel.switch_to_help();
            }
        }
    }

    /// Takes any pending runtime actions from settings changes, draining the queue.
    pub fn take_pending_actions(&mut self) -> Vec<SettingAction> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Returns the number of tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Returns the active tab index.
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    /// Switches to the next tab.
    fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
            self.save_active_tab();
        }
    }

    /// Switches to the previous tab.
    fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
            self.save_active_tab();
        }
    }

    fn render_tab_hint(&self, buffer: &mut Buffer, sep_area: Rect, hint: &str) {
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

                // Explicitly clear continuation cells so separator glyphs don't leak
                // into the visual width of wide characters.
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

// Note: Default is removed since TabbedPanel now requires a theme parameter

impl Panel for TabbedPanel {
    fn preferred_height(&self) -> u16 {
        // Active panel height + 2 for tab bar/separator
        // + 2 for border/footer if panel has footer entries
        self.tabs
            .get(self.active_tab)
            .map(|p| {
                let footer_rows = if p.footer_entries().is_empty() {
                    0
                } else {
                    2
                };
                p.preferred_height() + 2 + footer_rows
            })
            .unwrap_or(14)
    }

    fn title(&self) -> &str {
        "Panels"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // Create layout: tab bar at top (2 lines for visibility), content below
        let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(area);

        // Render tab bar with theme colors
        let titles: Vec<Line> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let style = if i == self.active_tab {
                    Style::default()
                        .fg(self.theme.tab_active_fg)
                        .bg(self.theme.tab_active_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(self.theme.tab_inactive_fg)
                        .bg(self.theme.tab_inactive_bg)
                };
                Line::from(Span::styled(format!(" {} ", tab.title()), style))
            })
            .collect();

        let tabs_widget = Tabs::new(titles)
            .select(self.active_tab)
            .style(Style::default().fg(self.theme.text_primary))
            .highlight_style(
                Style::default()
                    .fg(self.theme.tab_active_fg)
                    .bg(self.theme.tab_active_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .divider(Span::styled(" ", Style::default()));

        tabs_widget.render(chunks[0], buffer);

        // Render a separator line with tab switch hint
        if chunks[0].height > 1 {
            let sep_area = Rect::new(chunks[0].x, chunks[0].y + 1, chunks[0].width, 1);
            for x in sep_area.x..sep_area.x + sep_area.width {
                if let Some(cell) = buffer.cell_mut((x, sep_area.y)) {
                    cell.set_char(self.glyphs.border.horizontal);
                    cell.set_style(Style::default().fg(self.theme.panel_border));
                }
            }
            // Add hint for tab switching at the right side
            let hint = "Ctrl+←→ switch tabs";
            self.render_tab_hint(buffer, sep_area, hint);
        }

        // Render active panel content + footer (compositor role)
        if let Some(panel) = self.tabs.get_mut(self.active_tab) {
            let entries = panel.footer_entries();

            if entries.is_empty() {
                panel.render(buffer, chunks[1]);
            } else {
                let content_chunks = Layout::vertical([
                    Constraint::Min(1),    // Panel content
                    Constraint::Length(1), // Border
                    Constraint::Length(1), // Footer
                ])
                .split(chunks[1]);

                panel.render(buffer, content_chunks[0]);

                // Compose border widget with optional info
                let border_info = panel.border_info();
                let border = match border_info {
                    Some(ref info) => BorderLine::new(self.theme, self.glyphs.border.horizontal).with_info(info),
                    None => BorderLine::new(self.theme, self.glyphs.border.horizontal),
                };
                border.render(content_chunks[1], buffer);

                // Compose footer widget
                FooterBar::new(&entries, self.theme).render(content_chunks[2], buffer);
            }
        }
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // Panel tab switching with Ctrl+Left/Right (frees Tab for inner panel use)
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Left => {
                    self.prev_tab();
                    return PanelResult::Continue;
                }
                KeyCode::Right => {
                    self.next_tab();
                    return PanelResult::Continue;
                }
                _ => {}
            }
        }

        // Delegate all other keys to active panel
        let result = if let Some(panel) = self.tabs.get_mut(self.active_tab) {
            panel.handle_input(key)
        } else {
            PanelResult::Dismiss
        };

        // Drain setting actions from SettingsPanel and apply visual changes immediately
        if let Some(panel) = self.tabs.get_mut(TAB_SETTINGS) {
            if let Some(settings_panel) = panel.as_any_mut().downcast_mut::<SettingsPanel>() {
                for action in settings_panel.take_pending_actions() {
                    match &action {
                        SettingAction::SetGlyphTier(tier) => {
                            // Apply glyph tier to all tabs immediately for visual feedback
                            self.glyphs = GlyphSet::for_tier(*tier);
                            for tab in &mut self.tabs {
                                tab.set_glyph_tier(*tier);
                            }
                        }
                        SettingAction::SetTheme(preset) => {
                            // Apply theme to all tabs immediately for visual feedback
                            let theme = super::theme::Theme::for_preset(*preset);
                            self.theme = theme;
                            for tab in &mut self.tabs {
                                tab.set_theme(theme);
                            }
                        }
                        _ => {}
                    }
                    self.pending_actions.push(action);
                }
            }
        }

        result
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_glyph_tier(&mut self, tier: GlyphTier) {
        self.glyphs = GlyphSet::for_tier(tier);
        for tab in &mut self.tabs {
            tab.set_glyph_tier(tier);
        }
    }

    fn set_theme(&mut self, theme: &'static Theme) {
        self.theme = theme;
        for tab in &mut self.tabs {
            tab.set_theme(theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    fn test_tabbed_panel_new() {
        let panel = TabbedPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.tab_count(), 4);
        assert_eq!(panel.active_tab(), 0);
    }

    #[test]
    fn test_tabbed_panel_next_tab() {
        let mut panel = TabbedPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert_eq!(panel.active_tab(), 0);
        panel.next_tab();
        assert_eq!(panel.active_tab(), 1);
        panel.next_tab();
        assert_eq!(panel.active_tab(), 2);
        panel.next_tab();
        assert_eq!(panel.active_tab(), 3);
        // Wrap around
        panel.next_tab();
        assert_eq!(panel.active_tab(), 0);
    }

    #[test]
    fn test_tabbed_panel_prev_tab() {
        let mut panel = TabbedPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        // Should wrap to last
        panel.prev_tab();
        assert_eq!(panel.active_tab(), 3);
        panel.prev_tab();
        assert_eq!(panel.active_tab(), 2);
    }

    #[test]
    fn test_render_tab_hint_clears_wide_char_continuation_cell() {
        let panel = TabbedPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        let sep_area = Rect::new(0, 0, 12, 1);
        let mut buffer = Buffer::empty(sep_area);

        for x in sep_area.x..sep_area.x + sep_area.width {
            if let Some(cell) = buffer.cell_mut((x, sep_area.y)) {
                cell.set_char('─');
            }
        }

        let hint = "📁a";
        panel.render_tab_hint(&mut buffer, sep_area, hint);

        let hint_display_width = crate::ui::text_width::display_width(hint) as u16;
        let hint_start = sep_area.x + sep_area.width.saturating_sub(hint_display_width + 2);

        let lead = buffer.cell((hint_start, sep_area.y)).unwrap();
        assert_eq!(lead.symbol(), "📁");

        // Trailing continuation cell must be cleared, not left as the separator glyph.
        let trailing = buffer.cell((hint_start + 1, sep_area.y)).unwrap();
        assert_eq!(trailing.symbol(), " ");
    }
}
