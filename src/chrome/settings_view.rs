//! Main settings configuration view.
//!
//! Provides a navigable list of settings organized by category.
//! Each setting uses an appropriate input widget for editing.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};

use super::footer_bar::FooterEntry;
use super::glyphs::{GlyphSet, GlyphTier};
use super::theme::Theme;
use crate::history_store::HistoryStore;
use crate::config::ThemePreset;
use crate::ui::input_widgets::{
    CarouselWidget, SelectWidget, SliderWidget, TextInputWidget, ToggleWidget, WidgetResult,
};
use crate::ui::scrollable_list::ScrollableList;

/// A single configurable setting.
struct SettingItem {
    /// Display name.
    name: &'static str,
    /// Short description.
    description: &'static str,
    /// Setting key for persistence.
    key: &'static str,
    /// The kind of value this setting holds.
    kind: SettingKind,
}

/// The type and current value of a setting.
enum SettingKind {
    Toggle(bool),
    Select {
        options: Vec<String>,
        selected: usize,
    },
    Text(String),
    Slider {
        value: usize,
        min: usize,
        max: usize,
        step: usize,
    },
}

/// A section of related settings.
struct SettingSection {
    title: &'static str,
    items: Vec<SettingItem>,
}

/// Active editing state.
enum EditState {
    Toggle(ToggleWidget),
    Select(SelectWidget),
    Carousel(CarouselWidget),
    Slider(SliderWidget),
    Text(TextInputWidget),
}

/// Runtime action produced when a setting changes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum SettingAction {
    /// Change the glyph rendering tier.
    SetGlyphTier(GlyphTier),
    /// Change the color theme.
    SetTheme(ThemePreset),
    /// Enable or disable scrollback capture.
    SetScrollbackEnabled(bool),
    /// Change the maximum scrollback line count.
    SetScrollbackMaxLines(usize),
    /// Change the maximum bytes per scrollback line.
    SetScrollbackMaxLineBytes(usize),
}

/// Settings view for the settings panel.
pub struct SettingsView {
    sections: Vec<SettingSection>,
    /// Flat index across all items (section-first ordering).
    list: ScrollableList,
    /// Total number of items across all sections.
    total_items: usize,
    /// Currently editing widget (if any).
    editing: Option<EditState>,
    /// History store for persistence.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Pending runtime actions from setting changes.
    pending_actions: Vec<SettingAction>,
    /// Theme reference.
    theme: &'static Theme,
    /// Glyph set reference.
    glyphs: &'static GlyphSet,
}

impl SettingsView {
    /// Creates a new `SettingsView` configured with the given theme and glyph tier.
    ///
    /// The view is initialized with built-in setting sections (Appearance, Scrollback).
    /// Persisted values are loaded later via [`set_history_store`](Self::set_history_store).
    pub fn new(theme: &'static Theme, glyph_tier: GlyphTier) -> Self {
        let glyphs = GlyphSet::for_tier(glyph_tier);

        let sections = vec![
            SettingSection {
                title: "Appearance",
                items: vec![
                    SettingItem {
                        name: "Glyph Tier",
                        description: "Character rendering level",
                        key: "glyph_tier",
                        kind: SettingKind::Select {
                            options: vec![
                                "ASCII".into(),
                                "Unicode".into(),
                                "Emoji".into(),
                                "NerdFont".into(),
                            ],
                            selected: match glyph_tier {
                                GlyphTier::Ascii => 0,
                                GlyphTier::Unicode => 1,
                                GlyphTier::Emoji => 2,
                                GlyphTier::NerdFont => 3,
                            },
                        },
                    },
                    SettingItem {
                        name: "Theme",
                        description: "Color scheme preset",
                        key: "theme",
                        kind: SettingKind::Select {
                            options: vec!["Amber".into(), "Terminal".into()],
                            selected: 0,
                        },
                    },
                ],
            },
            SettingSection {
                title: "Scrollback",
                items: vec![
                    SettingItem {
                        name: "Enabled",
                        description: "Enable scrollback buffer",
                        key: "scrollback_enabled",
                        kind: SettingKind::Toggle(true),
                    },
                    SettingItem {
                        name: "Max Lines",
                        description: "Maximum lines in scrollback",
                        key: "scrollback_max_lines",
                        kind: SettingKind::Slider {
                            value: 10_000,
                            min: 100,
                            max: 1_000_000,
                            step: 1000,
                        },
                    },
                    SettingItem {
                        name: "Max Line Bytes",
                        description: "Maximum bytes per line",
                        key: "scrollback_max_line_bytes",
                        kind: SettingKind::Slider {
                            value: 4096,
                            min: 256,
                            max: 65_536,
                            step: 256,
                        },
                    },
                ],
            },
        ];

        let total_items: usize = sections.iter().map(|s| s.items.len()).sum();

        Self {
            sections,
            list: ScrollableList::new(),
            total_items,
            editing: None,
            history_store: None,
            pending_actions: Vec::new(),
            theme,
            glyphs,
        }
    }

    /// Sets the history store for persistence.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.history_store = Some(store.clone());
        // Load persisted values
        if let Ok(guard) = store.lock() {
            for section in &mut self.sections {
                for item in &mut section.items {
                    if let Ok(Some(val)) = guard.get_setting(item.key) {
                        match &mut item.kind {
                            SettingKind::Toggle(v) => {
                                *v = val == "true" || val == "1";
                            }
                            SettingKind::Select {
                                options,
                                selected,
                            } => {
                                if let Some(idx) =
                                    options.iter().position(|o| o.to_lowercase() == val.to_lowercase())
                                {
                                    *selected = idx;
                                }
                            }
                            SettingKind::Text(s) => {
                                *s = val;
                            }
                            SettingKind::Slider {
                                value, min, max, ..
                            } => {
                                if let Ok(parsed) = val.parse::<usize>() {
                                    *value = parsed.clamp(*min, *max);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Converts flat index to (section_idx, item_idx).
    fn flat_to_section(&self, flat: usize) -> Option<(usize, usize)> {
        let mut remaining = flat;
        for (si, section) in self.sections.iter().enumerate() {
            if remaining < section.items.len() {
                return Some((si, remaining));
            }
            remaining -= section.items.len();
        }
        None
    }

    /// Returns the item at the current selection.
    fn current_item(&self) -> Option<&SettingItem> {
        let (si, ii) = self.flat_to_section(self.list.selection())?;
        self.sections.get(si)?.items.get(ii)
    }

    /// Returns the item at the current selection (mutable).
    fn current_item_mut(&mut self) -> Option<&mut SettingItem> {
        let (si, ii) = self.flat_to_section(self.list.selection())?;
        self.sections.get_mut(si)?.items.get_mut(ii)
    }

    /// Starts editing the currently selected item.
    fn start_editing(&mut self) {
        let item = match self.current_item() {
            Some(item) => item,
            None => return,
        };
        self.editing = Some(match &item.kind {
            SettingKind::Toggle(val) => EditState::Toggle(ToggleWidget::new(*val)),
            SettingKind::Select { options, selected } => {
                if options.len() <= 8 {
                    EditState::Carousel(CarouselWidget::new(options.clone(), *selected))
                } else {
                    EditState::Select(SelectWidget::new(options.clone(), *selected))
                }
            }
            SettingKind::Text(val) => EditState::Text(TextInputWidget::new(val.clone())),
            SettingKind::Slider {
                value,
                min,
                max,
                step,
            } => EditState::Slider(SliderWidget::new(*value, *min, *max, *step)),
        });
    }

    /// Applies the confirmed edit value.
    fn apply_edit(&mut self) {
        let edit = match self.editing.take() {
            Some(e) => e,
            None => return,
        };

        let item = match self.current_item_mut() {
            Some(item) => item,
            None => return,
        };

        let persist_value: String;

        match (&mut item.kind, edit) {
            (SettingKind::Toggle(val), EditState::Toggle(w)) => {
                *val = w.value();
                persist_value = if *val { "true".into() } else { "false".into() };
            }
            (
                SettingKind::Select {
                    selected, ..
                },
                EditState::Select(w),
            ) => {
                *selected = w.selected();
                persist_value = w.selected_label().to_string();
            }
            (
                SettingKind::Select {
                    selected, ..
                },
                EditState::Carousel(w),
            ) => {
                *selected = w.selected();
                persist_value = w.selected_label().to_string();
            }
            (SettingKind::Text(val), EditState::Text(w)) => {
                *val = w.value().to_string();
                persist_value = val.clone();
            }
            (
                SettingKind::Slider { value, .. },
                EditState::Slider(w),
            ) => {
                *value = w.value();
                persist_value = value.to_string();
            }
            _ => return,
        }

        // Persist to history store
        let key = item.key;
        if let Some(store) = &self.history_store {
            if let Ok(guard) = store.lock() {
                let _ = guard.set_setting(key, &persist_value);
            }
        }

        // Emit runtime actions for settings that need immediate application
        match key {
            "glyph_tier" => {
                let tier = match persist_value.as_str() {
                    "ASCII" => GlyphTier::Ascii,
                    "Unicode" => GlyphTier::Unicode,
                    "Emoji" => GlyphTier::Emoji,
                    "NerdFont" => GlyphTier::NerdFont,
                    _ => return,
                };
                self.pending_actions.push(SettingAction::SetGlyphTier(tier));
            }
            "theme" => {
                let preset = match persist_value.as_str() {
                    "Amber" => ThemePreset::Amber,
                    "Terminal" => ThemePreset::Terminal,
                    _ => return,
                };
                self.pending_actions.push(SettingAction::SetTheme(preset));
            }
            "scrollback_enabled" => {
                let enabled = persist_value == "true";
                self.pending_actions
                    .push(SettingAction::SetScrollbackEnabled(enabled));
            }
            "scrollback_max_lines" => {
                if let Ok(n) = persist_value.parse::<usize>() {
                    self.pending_actions
                        .push(SettingAction::SetScrollbackMaxLines(n));
                }
            }
            "scrollback_max_line_bytes" => {
                if let Ok(n) = persist_value.parse::<usize>() {
                    self.pending_actions
                        .push(SettingAction::SetScrollbackMaxLineBytes(n));
                }
            }
            _ => {}
        }
    }

    /// Returns whether the view is in editing mode.
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Takes any pending runtime actions, draining the queue.
    pub fn take_pending_actions(&mut self) -> Vec<SettingAction> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Handles key input.
    pub fn handle_input(&mut self, key: KeyEvent) -> bool {
        // If editing, delegate to the active widget
        if let Some(ref mut edit) = self.editing {
            let result = match edit {
                EditState::Toggle(w) => w.handle_input(key),
                EditState::Select(w) => w.handle_input(key),
                EditState::Carousel(w) => w.handle_input(key),
                EditState::Slider(w) => w.handle_input(key),
                EditState::Text(w) => w.handle_input(key),
            };
            match result {
                WidgetResult::Confirmed => {
                    self.apply_edit();
                    return true;
                }
                WidgetResult::Cancelled => {
                    self.editing = None;
                    return true;
                }
                WidgetResult::Continue => return true,
            }
        }

        // Navigation mode
        match key.code {
            KeyCode::Up => {
                self.list.up(self.total_items);
                true
            }
            KeyCode::Down => {
                self.list.down(self.total_items);
                true
            }
            KeyCode::Enter => {
                self.start_editing();
                true
            }
            KeyCode::Home => {
                self.list.home();
                true
            }
            KeyCode::End => {
                self.list.end(self.total_items);
                true
            }
            _ => false,
        }
    }

    /// Renders the settings view.
    pub fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 2 || area.width < 20 {
            return;
        }

        // Two-column layout: items list (left 40%), detail/edit (right 60%)
        let chunks = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        self.render_item_list(buffer, chunks[0]);
        self.render_detail(buffer, chunks[1]);
    }

    /// Computes the display row offset for a given flat item index.
    ///
    /// Walks `self.sections` counting section headers (1 row each), item rows,
    /// and trailing blank lines (1 row each) to translate from a flat item
    /// index to the corresponding row index in the rendered list.
    fn row_offset_for_item(&self, flat_idx: usize) -> usize {
        let mut row = 0usize;
        let mut remaining = flat_idx;
        for section in &self.sections {
            if remaining < section.items.len() {
                // Header row + items before the target
                return row + 1 + remaining;
            }
            // header + items + blank spacer
            row += 1 + section.items.len() + 1;
            remaining -= section.items.len();
        }
        row
    }

    fn render_item_list(&self, buffer: &mut Buffer, area: Rect) {
        let viewport_height = area.height as usize;
        let mut render_list = self.list.clone();
        render_list.ensure_visible(viewport_height);

        let mut items: Vec<ListItem> = Vec::new();
        let mut flat_idx = 0usize;

        for section in &self.sections {
            // Section header
            items.push(ListItem::new(Line::from(Span::styled(
                section.title,
                Style::default()
                    .fg(self.theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            ))));

            for item in &section.items {
                let is_selected = flat_idx == self.list.selection();
                let style = if is_selected {
                    Style::default()
                        .fg(self.theme.selection_fg)
                        .bg(self.theme.selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(self.theme.text_primary)
                };

                let value_str = match &item.kind {
                    SettingKind::Toggle(v) => {
                        if *v { "ON" } else { "OFF" }.to_string()
                    }
                    SettingKind::Select {
                        options, selected, ..
                    } => options.get(*selected).cloned().unwrap_or_default(),
                    SettingKind::Text(v) => v.clone(),
                    SettingKind::Slider { value, .. } => value.to_string(),
                };

                let prefix = if is_selected {
                    self.glyphs.nav.chevron_right
                } else {
                    " "
                };
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", prefix), style),
                    Span::styled(
                        crate::ui::text_width::pad_to_width(item.name, 18),
                        style,
                    ),
                    Span::styled(value_str, Style::default().fg(self.theme.text_secondary)),
                ])));

                flat_idx += 1;
            }

            // Blank line between sections
            items.push(ListItem::new(Line::from("")));
        }

        // Translate the item-based scroll offset into a row offset that accounts
        // for section headers and blank spacer lines.
        let row_offset = self.row_offset_for_item(render_list.scroll_offset());
        let visible_items: Vec<ListItem> = items.into_iter().skip(row_offset).take(viewport_height).collect();

        let list = List::new(visible_items);
        list.render(area, buffer);
    }

    fn render_detail(&self, buffer: &mut Buffer, area: Rect) {
        if area.width < 5 || area.height < 2 {
            return;
        }

        // Show description and editing widget for selected item
        let item = match self.current_item() {
            Some(item) => item,
            None => return,
        };

        let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(area);

        // Description
        let desc_line = Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(item.name, Style::default().fg(self.theme.header_fg).add_modifier(Modifier::BOLD)),
            Span::styled(": ", Style::default().fg(self.theme.text_secondary)),
            Span::styled(item.description, Style::default().fg(self.theme.text_secondary)),
        ]);
        ratatui_widgets::paragraph::Paragraph::new(desc_line).render(chunks[0], buffer);

        // Editing widget or current value display
        let edit_area = Rect::new(chunks[1].x + 2, chunks[1].y, chunks[1].width.saturating_sub(4), chunks[1].height);

        if let Some(ref edit) = self.editing {
            match edit {
                EditState::Toggle(w) => w.render(buffer, edit_area, self.theme, self.glyphs),
                EditState::Select(w) => w.render(buffer, edit_area, self.theme, self.glyphs),
                EditState::Carousel(w) => w.render(buffer, edit_area, self.theme, self.glyphs),
                EditState::Slider(w) => w.render(buffer, edit_area, self.theme, self.glyphs),
                EditState::Text(w) => w.render(buffer, edit_area, self.theme, self.glyphs),
            }
        } else {
            // Show current value as read-only
            let value_str = match &item.kind {
                SettingKind::Toggle(v) => {
                    if *v { "ON" } else { "OFF" }.to_string()
                }
                SettingKind::Select {
                    options, selected, ..
                } => options.get(*selected).cloned().unwrap_or_default(),
                SettingKind::Text(v) => v.clone(),
                SettingKind::Slider { value, .. } => value.to_string(),
            };
            let line = Line::from(Span::styled(
                value_str,
                Style::default().fg(self.theme.text_primary),
            ));
            ratatui_widgets::paragraph::Paragraph::new(line).render(edit_area, buffer);
        }
    }

    /// Returns footer entries for the current state.
    pub fn footer_entries(&self) -> Vec<FooterEntry> {
        if self.editing.is_some() {
            vec![
                FooterEntry::action("Enter", "Confirm"),
                FooterEntry::action("Esc", "Cancel"),
            ]
        } else {
            vec![
                FooterEntry::action("\u{2191}\u{2193}", "Navigate"),
                FooterEntry::action("Enter", "Edit"),
                FooterEntry::action("Esc", "Close"),
            ]
        }
    }

    /// Updates glyph tier for runtime switching.
    pub fn set_glyph_tier(&mut self, tier: GlyphTier) {
        self.glyphs = GlyphSet::for_tier(tier);
    }

    /// Updates theme for runtime switching.
    pub fn set_theme(&mut self, theme: &'static Theme) {
        self.theme = theme;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::theme::AMBER_THEME;

    #[test]
    fn test_settings_view_new_has_sections() {
        let view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        assert!(!view.sections.is_empty());
        assert!(view.total_items > 0);
    }

    #[test]
    fn test_flat_to_section_mapping() {
        let view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // First section (Appearance) has 2 items
        assert_eq!(view.flat_to_section(0), Some((0, 0)));
        assert_eq!(view.flat_to_section(1), Some((0, 1)));
        // Second section (Scrollback) starts at index 2
        assert_eq!(view.flat_to_section(2), Some((1, 0)));
        assert_eq!(view.flat_to_section(3), Some((1, 1)));
        assert_eq!(view.flat_to_section(4), Some((1, 2)));
        // Out of range
        assert_eq!(view.flat_to_section(99), None);
    }

    #[test]
    fn test_navigation_up_down() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        assert_eq!(view.list.selection(), 0);

        view.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(view.list.selection(), 1);

        view.handle_input(KeyEvent::from(KeyCode::Up));
        assert_eq!(view.list.selection(), 0);
    }

    #[test]
    fn test_start_editing_toggle() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // Navigate to Scrollback Enabled (index 2)
        view.list.set_selection(2, view.total_items);
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(view.is_editing());
    }

    #[test]
    fn test_cancel_editing() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(view.is_editing());
        view.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(!view.is_editing());
    }

    #[test]
    fn test_footer_entries_change_with_editing_state() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        let normal_entries = view.footer_entries();
        assert!(normal_entries.len() >= 2);

        view.handle_input(KeyEvent::from(KeyCode::Enter));
        let edit_entries = view.footer_entries();
        assert!(edit_entries.len() >= 2);
    }

    #[test]
    fn test_select_uses_carousel_widget() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // Index 0 is "Glyph Tier" (Select with 4 options ≤ 8 → Carousel)
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(view.is_editing());
        assert!(matches!(view.editing, Some(EditState::Carousel(_))));
    }

    #[test]
    fn test_slider_editing_for_max_lines() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // Navigate to "Max Lines" (index 3)
        view.list.set_selection(3, view.total_items);
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(view.is_editing());
        assert!(matches!(view.editing, Some(EditState::Slider(_))));
    }

    #[test]
    fn test_carousel_confirm_emits_glyph_action() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // Index 0 is "Glyph Tier", start editing
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        // Default is Unicode (index 1), navigate down once to Emoji (index 2)
        view.handle_input(KeyEvent::from(KeyCode::Down));
        // Confirm
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        let actions = view.take_pending_actions();
        assert_eq!(actions, vec![SettingAction::SetGlyphTier(GlyphTier::Emoji)]);
    }

    #[test]
    fn test_theme_change_emits_action() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // Navigate to "Theme" (index 1)
        view.list.set_selection(1, view.total_items);
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        // Navigate to "Terminal" (down from Amber → Terminal)
        view.handle_input(KeyEvent::from(KeyCode::Down));
        // Confirm
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        let actions = view.take_pending_actions();
        assert_eq!(
            actions,
            vec![SettingAction::SetTheme(crate::config::ThemePreset::Terminal)]
        );
    }

    #[test]
    fn test_scrollback_toggle_emits_action() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // Navigate to "Enabled" (index 2)
        view.list.set_selection(2, view.total_items);
        // First Enter starts editing
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(view.is_editing());
        // Second Enter toggles (true→false) and confirms
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(!view.is_editing());
        let actions = view.take_pending_actions();
        assert_eq!(
            actions,
            vec![SettingAction::SetScrollbackEnabled(false)]
        );
    }

    #[test]
    fn test_slider_confirm_emits_scrollback_action() {
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        // Navigate to "Max Lines" (index 3)
        view.list.set_selection(3, view.total_items);
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        // Increase by one step (Right key)
        view.handle_input(KeyEvent::from(KeyCode::Right));
        // Confirm
        view.handle_input(KeyEvent::from(KeyCode::Enter));
        let actions = view.take_pending_actions();
        assert_eq!(
            actions,
            vec![SettingAction::SetScrollbackMaxLines(11_000)]
        );
    }

    #[test]
    fn test_set_theme_updates_theme_ref() {
        use crate::chrome::theme::TERMINAL_THEME;
        let mut view = SettingsView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.set_theme(&TERMINAL_THEME);
        assert!(std::ptr::eq(view.theme, &TERMINAL_THEME));
    }
}
