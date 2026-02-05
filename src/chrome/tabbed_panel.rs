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

use super::command_palette::CommandPalettePanel;
use super::file_browser::FileBrowserPanel;
use super::help_panel::HelpPanel;
use super::history_browser::HistoryBrowserPanel;
use super::panel::{Panel, PanelResult};
use super::theme::Theme;
use crate::history_store::HistoryStore;

// Tab indices for type-based access
const TAB_HISTORY_BROWSER: usize = 0;
const TAB_FILE_BROWSER: usize = 1;
const TAB_COMMAND_PALETTE: usize = 2;
#[allow(dead_code)]
const TAB_HELP: usize = 3;

/// A tabbed container for multiple panels.
pub struct TabbedPanel {
    /// Panel instances.
    tabs: Vec<Box<dyn Panel>>,
    /// Currently selected tab index.
    active_tab: usize,
    /// Reference to history store for settings persistence.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Theme for rendering.
    theme: &'static Theme,
}

impl TabbedPanel {
    /// Creates a new tabbed panel with all panel types.
    pub fn new(theme: &'static Theme) -> Self {
        let tabs: Vec<Box<dyn Panel>> = vec![
            Box::new(HistoryBrowserPanel::new(theme)),
            Box::new(FileBrowserPanel::new(theme)),
            Box::new(CommandPalettePanel::new(theme)),
            Box::new(HelpPanel::new(theme)),
        ];

        Self {
            tabs,
            active_tab: 0,
            history_store: None,
            theme,
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
                file_panel.set_history_store(store);
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
        // Load commands for command palette
        if let Some(panel) = self.tabs.get_mut(TAB_COMMAND_PALETTE) {
            if let Some(cmd_panel) = panel.as_any_mut().downcast_mut::<CommandPalettePanel>() {
                cmd_panel.load_commands(cwd);
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
}

// Note: Default is removed since TabbedPanel now requires a theme parameter

impl Panel for TabbedPanel {
    fn preferred_height(&self) -> u16 {
        // Active panel height + 4 for tab bar and outer border
        self.tabs
            .get(self.active_tab)
            .map(|p| p.preferred_height() + 4)
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
                    cell.set_char('─');
                    cell.set_style(Style::default().fg(self.theme.panel_border));
                }
            }
            // Add hint for tab switching at the right side
            let hint = "Ctrl+←→ switch tabs";
            let hint_start = sep_area.x + sep_area.width.saturating_sub(hint.len() as u16 + 2);
            for (i, ch) in hint.chars().enumerate() {
                let x = hint_start + i as u16;
                if x < sep_area.x + sep_area.width {
                    if let Some(cell) = buffer.cell_mut((x, sep_area.y)) {
                        cell.set_char(ch);
                        cell.set_style(Style::default().fg(self.theme.text_secondary));
                    }
                }
            }
        }

        // Render active panel content
        if let Some(panel) = self.tabs.get_mut(self.active_tab) {
            panel.render(buffer, chunks[1]);
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
        if let Some(panel) = self.tabs.get_mut(self.active_tab) {
            panel.handle_input(key)
        } else {
            PanelResult::Dismiss
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    fn test_tabbed_panel_new() {
        let panel = TabbedPanel::new(&AMBER_THEME);
        assert_eq!(panel.tab_count(), 4);
        assert_eq!(panel.active_tab(), 0);
    }

    #[test]
    fn test_tabbed_panel_next_tab() {
        let mut panel = TabbedPanel::new(&AMBER_THEME);
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
        let mut panel = TabbedPanel::new(&AMBER_THEME);
        // Should wrap to last
        panel.prev_tab();
        assert_eq!(panel.active_tab(), 3);
        panel.prev_tab();
        assert_eq!(panel.active_tab(), 2);
    }
}
