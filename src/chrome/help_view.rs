//! Enhanced help view with fuzzy search.
//!
//! Extends the existing `HelpPanel` concept with `/`-activated filter search
//! and auto-population from the `CommandRegistry`.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};

use super::command_palette::{CommandItem, CommandSource};
use super::footer_bar::FooterEntry;
use super::glyphs::{GlyphSet, GlyphTier};
use super::help_panel::HelpSection;
use super::theme::Theme;
use crate::ui::filter_input::FilterInput;
use crate::ui::scrollable_list::ScrollableList;

/// A single entry in the filtered results.
#[derive(Debug, Clone)]
struct FilteredEntry {
    /// Section index in the source sections list.
    section_idx: usize,
    /// Entry index within the section.
    entry_idx: usize,
    /// Fuzzy match score (higher is better).
    score: u32,
}

/// Fuzzy subsequence match with scoring.
///
/// Returns `Some(score)` if every character in `query` appears in `target`
/// in order (case-insensitive). Score rewards:
/// - Consecutive character matches (bonus per streak)
/// - Matches at word boundaries (after space, `-`, `_`, `/`)
/// - Match at the very start of target
///
/// Returns `None` if the query is not a subsequence of the target.
fn fuzzy_score(query: &str, target: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }

    let query_lower: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    let target_lower: Vec<char> = target
        .chars()
        .flat_map(|c| c.to_lowercase())
        .collect();

    // Quick check: query can't match if longer than target
    if query_lower.len() > target_lower.len() {
        return None;
    }

    let target_chars: Vec<char> = target.chars().collect();
    let mut score: u32 = 0;
    let mut qi = 0; // index into query_lower
    let mut prev_match_idx: Option<usize> = None;

    for (ti, &tc) in target_lower.iter().enumerate() {
        if qi < query_lower.len() && tc == query_lower[qi] {
            // Base score per matched character
            score += 1;

            // Bonus: consecutive match
            if let Some(prev) = prev_match_idx {
                if ti == prev + 1 {
                    score += 3;
                }
            }

            // Bonus: match at word boundary
            if ti == 0 {
                score += 5; // Start of string
            } else {
                let prev_ch = target_chars.get(ti.wrapping_sub(1)).copied().unwrap_or(' ');
                if prev_ch == ' ' || prev_ch == '-' || prev_ch == '_' || prev_ch == '/' {
                    score += 3; // Word boundary
                }
            }

            prev_match_idx = Some(ti);
            qi += 1;
        }
    }

    if qi == query_lower.len() {
        Some(score)
    } else {
        None // Not all query chars matched
    }
}

/// Returns the best fuzzy score across multiple candidate strings.
fn best_fuzzy_score(query: &str, candidates: &[&str]) -> Option<u32> {
    candidates
        .iter()
        .filter_map(|c| fuzzy_score(query, c))
        .max()
}

/// Enhanced help view with fuzzy search.
pub struct HelpView {
    /// All help sections.
    sections: Vec<HelpSection>,
    /// Filter input for search.
    filter: FilterInput,
    /// Filtered result indices.
    filtered: Vec<FilteredEntry>,
    /// Scrollable list for navigation.
    list: ScrollableList,
    /// Scroll offset for unfiltered display.
    scroll_offset: usize,
    /// Total lines for scrolling (unfiltered mode).
    total_lines: usize,
    /// Theme reference.
    theme: &'static Theme,
    /// Glyph set reference.
    glyphs: &'static GlyphSet,
}

impl HelpView {
    pub fn new(theme: &'static Theme, glyph_tier: GlyphTier) -> Self {
        let glyphs = GlyphSet::for_tier(glyph_tier);

        let sections = vec![
            HelpSection {
                title: "Panel Navigation".to_string(),
                entries: vec![
                    ("Ctrl+Space".into(), "Open panels".into()),
                    ("Ctrl+\u{2190}\u{2192}".into(), "Switch tabs".into()),
                    ("Tab/S-Tab".into(), "Switch sub-tabs".into()),
                    ("Esc".into(), "Close panels".into()),
                ],
            },
            HelpSection {
                title: "Command Palette".to_string(),
                entries: vec![
                    ("Type".into(), "Filter commands".into()),
                    ("Up/Down".into(), "Navigate list".into()),
                    ("Enter".into(), "Execute selected".into()),
                    ("Backspace".into(), "Clear filter".into()),
                ],
            },
            HelpSection {
                title: "File Browser".to_string(),
                entries: vec![
                    ("Enter".into(), "Open directory / insert path".into()),
                    ("Backspace".into(), "Go to parent directory".into()),
                    ("Up/Down".into(), "Navigate files".into()),
                    ("Ctrl+H or .".into(), "Toggle hidden files".into()),
                ],
            },
            HelpSection {
                title: "History Browser".to_string(),
                entries: vec![
                    ("Type".into(), "Filter history".into()),
                    ("Up/Down".into(), "Navigate history".into()),
                    ("Enter".into(), "Execute selected".into()),
                ],
            },
            HelpSection {
                title: "Edit Mode Keybindings".to_string(),
                entries: vec![
                    ("Ctrl+A".into(), "Move to beginning of line".into()),
                    ("Ctrl+E".into(), "Move to end of line".into()),
                    ("Ctrl+K".into(), "Kill to end of line".into()),
                    ("Ctrl+U".into(), "Kill to beginning of line".into()),
                    ("Ctrl+W".into(), "Kill previous word".into()),
                    ("Ctrl+Y".into(), "Yank killed text".into()),
                    ("Ctrl+R".into(), "Reverse history search".into()),
                    ("Ctrl+C".into(), "Clear line".into()),
                    ("Ctrl+D".into(), "Exit (on empty line)".into()),
                    ("Tab".into(), "Tab completion".into()),
                    ("Up/Down".into(), "History navigation".into()),
                ],
            },
            HelpSection {
                title: "Scroll Viewer".to_string(),
                entries: vec![
                    ("Ctrl+\u{2191}\u{2193}".into(), "Scroll up/down".into()),
                    ("Page Up/Down".into(), "Page scroll".into()),
                    ("/".into(), "Search in scrollback".into()),
                    ("n/N".into(), "Next/prev match".into()),
                    ("y".into(), "Yank mode".into()),
                    ("q or Esc".into(), "Exit scroll view".into()),
                ],
            },
        ];

        let total_lines: usize = sections
            .iter()
            .map(|s| 1 + s.entries.len() + 1) // title + entries + blank
            .sum();

        Self {
            sections,
            filter: FilterInput::new(),
            filtered: Vec::new(),
            list: ScrollableList::new(),
            scroll_offset: 0,
            total_lines,
            theme,
            glyphs,
        }
    }

    /// Loads command documentation from the command registry.
    pub fn load_command_docs(&mut self, commands: Vec<(&str, &[&str], &str)>) {
        let entries: Vec<(String, String)> = commands
            .iter()
            .map(|(name, aliases, desc)| {
                let alias_str = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", aliases.join(", "))
                };
                (format!(":{}{}", name, alias_str), desc.to_string())
            })
            .collect();

        if !entries.is_empty() {
            // Check if we already have a "Colon Commands" section and replace it
            if let Some(pos) = self.sections.iter().position(|s| s.title == "Colon Commands") {
                self.sections[pos] = HelpSection {
                    title: "Colon Commands".to_string(),
                    entries,
                };
            } else {
                self.sections.push(HelpSection {
                    title: "Colon Commands".to_string(),
                    entries,
                });
            }
            self.recalculate_total_lines();
        }
    }

    /// Loads discovered project commands as help sections, grouped by source.
    pub fn load_project_commands(&mut self, items: &[CommandItem]) {
        if items.is_empty() {
            return;
        }

        // Group items by source, preserving order of first appearance
        let source_order: &[(CommandSource, &str)] = &[
            (CommandSource::CargoToml, "Cargo Commands"),
            (CommandSource::PackageJson, "npm Scripts"),
            (CommandSource::Makefile, "Makefile Targets"),
            (CommandSource::JustFile, "Just Recipes"),
            (CommandSource::Script, "Project Scripts"),
        ];

        for &(source, title) in source_order {
            let entries: Vec<(String, String)> = items
                .iter()
                .filter(|item| item.source == source)
                .map(|item| (item.command.clone(), item.description.clone()))
                .collect();

            if entries.is_empty() {
                continue;
            }

            // Replace existing section with this title, or append
            if let Some(pos) = self.sections.iter().position(|s| s.title == title) {
                self.sections[pos] = HelpSection {
                    title: title.to_string(),
                    entries,
                };
            } else {
                self.sections.push(HelpSection {
                    title: title.to_string(),
                    entries,
                });
            }
        }

        self.recalculate_total_lines();
    }

    fn recalculate_total_lines(&mut self) {
        self.total_lines = self
            .sections
            .iter()
            .map(|s| 1 + s.entries.len() + 1)
            .sum();
    }

    /// Rebuilds filtered results based on current filter text using fuzzy matching.
    fn rebuild_filter(&mut self) {
        self.filtered.clear();

        if !self.filter.has_filter() {
            return;
        }

        let query = self.filter.text();

        for (si, section) in self.sections.iter().enumerate() {
            for (ei, (key, desc)) in section.entries.iter().enumerate() {
                // Fuzzy match against section title, key, and description
                if let Some(score) =
                    best_fuzzy_score(query, &[&section.title, key.as_str(), desc.as_str()])
                {
                    self.filtered.push(FilteredEntry {
                        section_idx: si,
                        entry_idx: ei,
                        score,
                    });
                }
            }
        }

        // Sort by score descending (best matches first)
        self.filtered.sort_by(|a, b| b.score.cmp(&a.score));

        // Reset selection to the beginning of filtered results
        self.list.set_selection(0, self.filtered.len());
    }

    /// Handles key input. Returns true if consumed.
    pub fn handle_input(&mut self, key: KeyEvent) -> bool {
        // Filter active: handle filter input
        if self.filter.is_active() {
            match key.code {
                KeyCode::Esc => {
                    if self.filter.has_filter() {
                        self.filter.clear_and_deactivate();
                        self.filtered.clear();
                    } else {
                        self.filter.deactivate();
                    }
                    return true;
                }
                KeyCode::Backspace => {
                    if self.filter.backspace() {
                        self.filtered.clear();
                    } else {
                        self.rebuild_filter();
                    }
                    return true;
                }
                KeyCode::Char(c) => {
                    self.filter.type_char(c);
                    self.rebuild_filter();
                    return true;
                }
                KeyCode::Up => {
                    self.list.up(self.filtered.len());
                    return true;
                }
                KeyCode::Down => {
                    self.list.down(self.filtered.len());
                    return true;
                }
                KeyCode::Enter => {
                    self.filter.deactivate();
                    return true;
                }
                _ => return true,
            }
        }

        // Filter has results: navigate them
        if self.filter.has_filter() {
            match key.code {
                KeyCode::Up => {
                    self.list.up(self.filtered.len());
                    return true;
                }
                KeyCode::Down => {
                    self.list.down(self.filtered.len());
                    return true;
                }
                KeyCode::Esc => {
                    self.filter.clear_and_deactivate();
                    self.filtered.clear();
                    return true;
                }
                KeyCode::Char('/') => {
                    self.filter.activate();
                    return true;
                }
                _ => return true,
            }
        }

        // Normal mode
        match key.code {
            KeyCode::Char('/') => {
                self.filter.activate();
                true
            }
            KeyCode::Up => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
                true
            }
            KeyCode::Down => {
                if self.scroll_offset + 1 < self.total_lines {
                    self.scroll_offset += 1;
                }
                true
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
                true
            }
            KeyCode::PageDown => {
                self.scroll_offset =
                    (self.scroll_offset + 10).min(self.total_lines.saturating_sub(1));
                true
            }
            _ => false,
        }
    }

    /// Renders the help view.
    pub fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 2 || area.width < 10 {
            return;
        }

        if self.filter.is_active() || self.filter.has_filter() {
            // Filter mode: show filter bar + filtered results
            let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
            self.render_filter_bar(buffer, chunks[0]);
            self.render_filtered(buffer, chunks[1]);
        } else {
            // Normal mode: show all sections
            self.render_all_sections(buffer, area);
        }
    }

    fn render_filter_bar(&self, buffer: &mut Buffer, area: Rect) {
        let spans = self.filter.render_spans(self.theme, self.glyphs);
        let count_str = format!(" ({} matches)", self.filtered.len());
        let mut all_spans = spans;
        all_spans.push(Span::styled(
            count_str,
            Style::default().fg(self.theme.text_secondary),
        ));
        ratatui_widgets::paragraph::Paragraph::new(Line::from(all_spans)).render(area, buffer);
    }

    fn render_filtered(&self, buffer: &mut Buffer, area: Rect) {
        let viewport_height = area.height as usize;
        let mut render_list = self.list.clone();
        render_list.ensure_visible(viewport_height);

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .skip(render_list.scroll_offset())
            .take(viewport_height)
            .map(|(i, entry)| {
                let section = &self.sections[entry.section_idx];
                let (key, desc) = &section.entries[entry.entry_idx];
                let is_selected = i == self.list.selection();

                let key_style = if is_selected {
                    Style::default()
                        .fg(self.theme.selection_fg)
                        .bg(self.theme.selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(self.theme.text_highlight)
                };

                let desc_style = if is_selected {
                    Style::default()
                        .fg(self.theme.selection_fg)
                        .bg(self.theme.selection_bg)
                } else {
                    Style::default().fg(self.theme.text_primary)
                };

                let section_style = Style::default().fg(self.theme.text_secondary);

                let padded_key = crate::ui::text_width::pad_to_width(key, 15);
                ListItem::new(Line::from(vec![
                    Span::styled(format!("[{}] ", section.title), section_style),
                    Span::styled(padded_key, key_style),
                    Span::styled(desc, desc_style),
                ]))
            })
            .collect();

        List::new(items).render(area, buffer);
    }

    fn render_all_sections(&self, buffer: &mut Buffer, area: Rect) {
        let visible_height = area.height as usize;

        let mut lines: Vec<ListItem> = Vec::new();

        for section in &self.sections {
            lines.push(ListItem::new(Line::from(vec![Span::styled(
                &section.title,
                Style::default()
                    .fg(self.theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            )])));

            for (key, desc) in &section.entries {
                let padded_key = crate::ui::text_width::pad_to_width(key, 15);
                lines.push(ListItem::new(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(padded_key, Style::default().fg(self.theme.text_highlight)),
                    Span::styled(desc, Style::default().fg(self.theme.text_primary)),
                ])));
            }

            lines.push(ListItem::new(Line::from("")));
        }

        let max_offset = self.total_lines.saturating_sub(visible_height);
        let clamped_offset = self.scroll_offset.min(max_offset);

        let visible_lines: Vec<ListItem> = lines
            .into_iter()
            .skip(clamped_offset)
            .take(visible_height)
            .collect();

        List::new(visible_lines).render(area, buffer);
    }

    /// Returns footer entries.
    pub fn footer_entries(&self) -> Vec<FooterEntry> {
        if self.filter.is_active() {
            vec![
                FooterEntry::action("Type", "Search"),
                FooterEntry::action("Esc", "Clear"),
            ]
        } else if self.filter.has_filter() {
            vec![
                FooterEntry::action("/", "Edit search"),
                FooterEntry::action("\u{2191}\u{2193}", "Navigate"),
                FooterEntry::action("Esc", "Clear"),
            ]
        } else {
            vec![
                FooterEntry::action("/", "Search"),
                FooterEntry::action("\u{2191}\u{2193}", "Scroll"),
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
    fn test_help_view_new_has_sections() {
        let view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        assert!(!view.sections.is_empty());
        assert!(view.total_lines > 0);
    }

    #[test]
    fn test_help_view_load_command_docs() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        let initial_count = view.sections.len();
        view.load_command_docs(vec![
            ("panel", &["p"] as &[&str], "Open panel"),
            ("help", &["h", "?"] as &[&str], "Show help"),
        ]);
        assert_eq!(view.sections.len(), initial_count + 1);
        assert_eq!(view.sections.last().unwrap().title, "Colon Commands");
        assert_eq!(view.sections.last().unwrap().entries.len(), 2);
    }

    #[test]
    fn test_filter_activation() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        assert!(!view.filter.is_active());
        view.handle_input(KeyEvent::from(KeyCode::Char('/')));
        assert!(view.filter.is_active());
    }

    #[test]
    fn test_filter_typing_and_matching() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.handle_input(KeyEvent::from(KeyCode::Char('/')));
        view.handle_input(KeyEvent::from(KeyCode::Char('C')));
        view.handle_input(KeyEvent::from(KeyCode::Char('t')));
        view.handle_input(KeyEvent::from(KeyCode::Char('r')));
        view.handle_input(KeyEvent::from(KeyCode::Char('l')));
        assert!(view.filter.has_filter());
        assert!(!view.filtered.is_empty());
    }

    #[test]
    fn test_filter_esc_clears() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.handle_input(KeyEvent::from(KeyCode::Char('/')));
        view.handle_input(KeyEvent::from(KeyCode::Char('a')));
        view.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(!view.filter.has_filter());
        assert!(!view.filter.is_active());
    }

    #[test]
    fn test_normal_scroll() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        assert_eq!(view.scroll_offset, 0);
        view.handle_input(KeyEvent::from(KeyCode::Down));
        assert_eq!(view.scroll_offset, 1);
        view.handle_input(KeyEvent::from(KeyCode::Up));
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn test_footer_entries_change_with_filter_state() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        let normal = view.footer_entries();
        assert!(normal.iter().any(|e| matches!(&e.kind, super::super::footer_bar::FooterKind::Action { key: "/", .. })));

        view.handle_input(KeyEvent::from(KeyCode::Char('/')));
        let active = view.footer_entries();
        assert!(active.iter().any(|e| matches!(&e.kind, super::super::footer_bar::FooterKind::Action { key: "Type", .. })));
    }

    // --- Fuzzy scoring ---

    #[test]
    fn test_fuzzy_score_exact_match() {
        assert!(fuzzy_score("Ctrl", "Ctrl+Space").is_some());
        let score = fuzzy_score("Ctrl", "Ctrl+Space").unwrap();
        assert!(score > 0);
    }

    #[test]
    fn test_fuzzy_score_subsequence_match() {
        // "crl" is a subsequence of "Ctrl" (c-r-l, skipping t)
        assert!(fuzzy_score("crl", "Ctrl+Space").is_some());
    }

    #[test]
    fn test_fuzzy_score_no_match() {
        assert!(fuzzy_score("xyz", "Ctrl+Space").is_none());
    }

    #[test]
    fn test_fuzzy_score_empty_query() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn test_fuzzy_score_case_insensitive() {
        let upper = fuzzy_score("ctrl", "Ctrl+Space");
        let lower = fuzzy_score("CTRL", "Ctrl+Space");
        assert!(upper.is_some());
        assert!(lower.is_some());
        assert_eq!(upper, lower);
    }

    #[test]
    fn test_fuzzy_score_consecutive_bonus() {
        // "abc" consecutive in "abcdef" should score higher than spread across "axbxcx"
        let consecutive = fuzzy_score("abc", "abcdef").unwrap();
        let spread = fuzzy_score("abc", "axbxcx").unwrap();
        assert!(consecutive > spread);
    }

    #[test]
    fn test_fuzzy_score_start_bonus() {
        // Match at start of string should score higher
        let at_start = fuzzy_score("pan", "Panel Navigation").unwrap();
        let mid = fuzzy_score("pan", "Open panels").unwrap();
        assert!(at_start > mid);
    }

    #[test]
    fn test_best_fuzzy_score_picks_max() {
        let score = best_fuzzy_score("ctrl", &["Ctrl+Space", "something", "ctrl"]);
        assert!(score.is_some());
    }

    // --- Project commands integration ---

    fn make_test_items() -> Vec<CommandItem> {
        vec![
            CommandItem {
                name: "build".to_string(),
                description: "Compile the current package".to_string(),
                command: "cargo build".to_string(),
                source: CommandSource::CargoToml,
                frecency_score: 70.0,
            },
            CommandItem {
                name: "test".to_string(),
                description: "Run the tests".to_string(),
                command: "cargo test".to_string(),
                source: CommandSource::CargoToml,
                frecency_score: 70.0,
            },
            CommandItem {
                name: "all".to_string(),
                description: "Makefile target".to_string(),
                command: "make all".to_string(),
                source: CommandSource::Makefile,
                frecency_score: 50.0,
            },
        ]
    }

    #[test]
    fn test_load_project_commands_adds_sections() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        let initial_count = view.sections.len();
        view.load_project_commands(&make_test_items());
        // Should add "Cargo Commands" and "Makefile Targets" sections
        assert_eq!(view.sections.len(), initial_count + 2);
    }

    #[test]
    fn test_load_project_commands_groups_by_source() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.load_project_commands(&make_test_items());
        let cargo_section = view
            .sections
            .iter()
            .find(|s| s.title == "Cargo Commands")
            .expect("Cargo Commands section missing");
        assert_eq!(cargo_section.entries.len(), 2);

        let make_section = view
            .sections
            .iter()
            .find(|s| s.title == "Makefile Targets")
            .expect("Makefile Targets section missing");
        assert_eq!(make_section.entries.len(), 1);
    }

    #[test]
    fn test_load_project_commands_uses_command_as_key() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.load_project_commands(&make_test_items());
        let cargo_section = view
            .sections
            .iter()
            .find(|s| s.title == "Cargo Commands")
            .unwrap();
        assert_eq!(cargo_section.entries[0].0, "cargo build");
        assert_eq!(cargo_section.entries[0].1, "Compile the current package");
    }

    #[test]
    fn test_load_project_commands_empty_is_noop() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        let initial_count = view.sections.len();
        view.load_project_commands(&[]);
        assert_eq!(view.sections.len(), initial_count);
    }

    #[test]
    fn test_load_project_commands_replaces_on_reload() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.load_project_commands(&make_test_items());
        let count_after_first = view.sections.len();
        // Reload same items
        view.load_project_commands(&make_test_items());
        assert_eq!(view.sections.len(), count_after_first);
    }

    #[test]
    fn test_load_project_commands_searchable() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.load_project_commands(&make_test_items());
        // Activate search and type "cargo"
        view.handle_input(KeyEvent::from(KeyCode::Char('/')));
        for c in "cargo".chars() {
            view.handle_input(KeyEvent::from(KeyCode::Char(c)));
        }
        // Should find matches in the Cargo Commands section
        assert!(
            view.filtered.len() >= 2,
            "Expected at least 2 matches for 'cargo', got {}",
            view.filtered.len()
        );
    }

    #[test]
    fn test_fuzzy_filter_results_sorted_by_score() {
        let mut view = HelpView::new(&AMBER_THEME, GlyphTier::Unicode);
        view.handle_input(KeyEvent::from(KeyCode::Char('/')));
        // Type "esc" - should match "Esc" entries, ranked by fuzzy score
        view.handle_input(KeyEvent::from(KeyCode::Char('e')));
        view.handle_input(KeyEvent::from(KeyCode::Char('s')));
        view.handle_input(KeyEvent::from(KeyCode::Char('c')));
        assert!(!view.filtered.is_empty());
        // Verify descending score order
        for window in view.filtered.windows(2) {
            assert!(window[0].score >= window[1].score);
        }
    }
}
