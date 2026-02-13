//! File browser panel with tree view, git integration, and inline filter.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};
use ratatui_widgets::paragraph::Paragraph;
use tracing::debug;

use super::command_edit::{
    CommandEditState, CommandToken, TokenType, compute_edit_mode_layout, render_edit_mode_shared,
};
use super::file_tree::{FileTreeState, FlatEntry};
use super::footer_bar::FooterEntry;
use super::glyphs::{GlyphSet, GlyphTier};
use super::panel::{Panel, PanelResult};
use super::theme::Theme;
use crate::git::{CachedGitRepoStatus, GitFileStatus, get_git_repo_status_cached};
use crate::history_store::HistoryStore;
use crate::intelligence::FileContext;
use crate::ui::filter_input::FilterInput;
use crate::ui::focus_style::apply_focus;
use crate::ui::scrollable_list::ScrollableList;
use crate::ui::tree_view::tree_prefix;

/// File browser panel with tree view.
pub struct FileBrowserPanel {
    /// Tree state manager.
    tree: FileTreeState,
    /// Scrollable list for viewport management.
    scroll: ScrollableList,
    /// Inline filter input.
    filter: FilterInput,
    /// Indices into tree.entries() that match the current filter.
    filtered_indices: Vec<usize>,
    /// Edit mode state (None when not in edit mode).
    edit_mode: Option<CommandEditState>,
    /// Filename being edited (stored separately for suggestions).
    edit_filename: Option<String>,
    /// Theme for rendering.
    theme: &'static Theme,
    /// Unified glyph set for the current tier.
    glyphs: &'static GlyphSet,
    /// Reference to the history store for intelligent suggestions.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Cached git repo status.
    git_cache: Option<CachedGitRepoStatus>,
}

impl FileBrowserPanel {
    /// Creates a new file browser at the current directory.
    pub fn new(theme: &'static Theme, glyph_tier: GlyphTier) -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let tree = FileTreeState::new(current_dir);
        let glyphs = GlyphSet::for_tier(glyph_tier);

        Self {
            tree,
            scroll: ScrollableList::new(),
            filter: FilterInput::new(),
            filtered_indices: Vec::new(),
            edit_mode: None,
            edit_filename: None,
            theme,
            glyphs,
            history_store: None,
            git_cache: None,
        }
    }

    /// Sets the history store for intelligent suggestions.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.history_store = Some(store);
    }

    /// Navigates to the given path (sets as tree root).
    pub fn navigate_to(&mut self, path: &Path) -> std::io::Result<()> {
        let canonical = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.tree.root().join(path)
        };

        if canonical.is_dir() {
            self.tree.set_root(canonical);
            self.scroll.reset();
            self.refresh_git();
            self.rebuild_filtered();
        }

        Ok(())
    }

    /// Refreshes git status for the current tree root.
    fn refresh_git(&mut self) {
        let root = self.tree.root().to_path_buf();
        let status = get_git_repo_status_cached(&root, &mut self.git_cache);
        self.tree.set_git_status(status);
    }

    /// Rebuilds the filtered indices list based on current filter text.
    fn rebuild_filtered(&mut self) {
        self.filtered_indices.clear();
        if self.filter.has_filter() {
            for (i, entry) in self.tree.entries().iter().enumerate() {
                if self.filter.matches(&entry.entry.name) {
                    self.filtered_indices.push(i);
                }
            }
        }
    }

    /// Returns the number of visible items (filtered or total).
    fn visible_count(&self) -> usize {
        if self.filter.has_filter() {
            self.filtered_indices.len()
        } else {
            self.tree.len()
        }
    }

    /// Maps a display index (in the scroll list) to a tree index.
    fn display_to_tree_index(&self, display_idx: usize) -> Option<usize> {
        if self.filter.has_filter() {
            self.filtered_indices.get(display_idx).copied()
        } else if display_idx < self.tree.len() {
            Some(display_idx)
        } else {
            None
        }
    }

    /// Returns the currently selected tree entry.
    fn selected_entry(&self) -> Option<&FlatEntry> {
        let tree_idx = self.display_to_tree_index(self.scroll.selection())?;
        self.tree.entry_at(tree_idx)
    }

    // ── Edit mode ────────────────────────────────────────────────────────────

    /// Enters edit mode for the selected file.
    fn enter_edit_mode(&mut self) {
        if let Some(entry) = self.selected_entry().cloned() {
            if !entry.entry.is_dir {
                debug!(file = %entry.entry.name, "Entering file edit mode");

                let raw_path = entry.entry.path.to_string_lossy();
                let mut edit_state = CommandEditState::for_file(&entry.entry.name, &raw_path);

                if let Some(store) = &self.history_store {
                    edit_state.set_history_store(store.clone());
                }
                edit_state.set_cwd(self.tree.root().to_path_buf());
                edit_state
                    .set_file_context(FileContext::new(&entry.entry.name, entry.entry.is_dir));
                edit_state.update_suggestions();

                self.edit_filename = Some(entry.entry.name.clone());
                self.edit_mode = Some(edit_state);
            }
        }
    }

    /// Exits edit mode.
    fn exit_edit_mode(&mut self) {
        self.edit_mode = None;
        self.edit_filename = None;
    }

    /// Returns true if in edit mode.
    fn in_edit_mode(&self) -> bool {
        self.edit_mode.is_some()
    }

    /// Updates suggestions from the intelligence pipeline.
    fn update_suggestions_with_file_context(&mut self) {
        if let Some(edit_state) = &mut self.edit_mode {
            edit_state.update_suggestions();
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Renders the file edit mode UI.
    fn render_file_edit_mode(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        edit_state: &CommandEditState,
    ) {
        let Some(layout) = compute_edit_mode_layout(area, edit_state.strip_vertical) else {
            return;
        };

        // Title
        let filename = self.edit_filename.as_deref().unwrap_or("file");
        let mut title_spans = vec![
            Span::styled(
                " Edit Command for: ",
                Style::default().fg(self.theme.header_fg),
            ),
            Span::styled(
                filename,
                Style::default()
                    .fg(self.theme.text_highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if !edit_state.suggestions.is_empty() {
            let sugg_count = format!(" [{} suggestions]", edit_state.suggestions.len());
            title_spans.push(Span::styled(
                sugg_count,
                Style::default().fg(self.theme.text_secondary),
            ));
        }
        Paragraph::new(Line::from(title_spans)).render(layout.title, buffer);

        // Separator
        let border_style = Style::default().fg(self.theme.panel_border);
        for x in layout.separator.x..layout.separator.x + layout.separator.width {
            if let Some(cell) = buffer.cell_mut((x, layout.separator.y)) {
                cell.set_char(self.glyphs.border.horizontal);
                cell.set_style(border_style);
            }
        }

        render_edit_mode_shared(buffer, self.theme, self.glyphs, edit_state, &layout);
    }

    /// Renders the path header with git summary.
    fn render_header(&self, buffer: &mut Buffer, area: Rect) {
        let path_str = self.tree.root().to_string_lossy();
        let max_path_width = (area.width as usize).saturating_sub(4);

        // Build git summary if available
        let git_summary = self.build_git_summary();
        let git_width = crate::ui::text_width::display_width(&git_summary);
        let path_budget = max_path_width.saturating_sub(git_width + 2);

        let truncated_path = if crate::ui::text_width::display_width(&path_str) > path_budget {
            let target_width = path_budget.saturating_sub(3);
            let mut width = 0;
            let mut start_idx = path_str.len();
            for (idx, ch) in path_str.char_indices().rev() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if width + ch_w > target_width {
                    break;
                }
                width += ch_w;
                start_idx = idx;
            }
            format!("...{}", &path_str[start_idx..])
        } else {
            path_str.to_string()
        };

        let show_hidden = self.tree.show_hidden();
        let truncated_path_width = crate::ui::text_width::display_width(&truncated_path);
        let mut spans = vec![
            Span::styled(" ", Style::default().fg(self.theme.header_fg)),
            Span::styled(
                truncated_path,
                Style::default()
                    .fg(self.theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            ),
        ];

        if show_hidden {
            spans.push(Span::styled(
                " [H]",
                Style::default().fg(self.theme.text_secondary),
            ));
        }

        // Add git summary at right side
        if !git_summary.is_empty() {
            // Calculate padding to right-align git info
            let used_width = 1 + truncated_path_width + if show_hidden { 4 } else { 0 };
            let padding = (area.width as usize).saturating_sub(used_width + git_width + 1);
            if padding > 0 {
                spans.push(Span::raw(" ".repeat(padding)));
            }
            spans.extend(self.build_git_summary_spans());
        }

        Paragraph::new(Line::from(spans)).render(area, buffer);
    }

    /// Builds a plain-text git summary string for width calculation.
    fn build_git_summary(&self) -> String {
        let Some(git_status) = self.tree.git_status() else {
            return String::new();
        };

        let summary = git_status.summary();
        if summary.is_empty() {
            return String::new();
        }

        let icons = &self.glyphs.icon;
        let mut parts = Vec::new();
        if let Some(&count) = summary.get(&GitFileStatus::Modified) {
            parts.push(format!("{}{}", icons.git_modified, count));
        }
        if let Some(&count) = summary.get(&GitFileStatus::Added) {
            parts.push(format!("{}{}", icons.git_added, count));
        }
        if let Some(&count) = summary.get(&GitFileStatus::Deleted) {
            parts.push(format!("{}{}", icons.git_deleted, count));
        }
        if let Some(&count) = summary.get(&GitFileStatus::Untracked) {
            parts.push(format!("{}{}", icons.git_untracked, count));
        }
        if let Some(&count) = summary.get(&GitFileStatus::Renamed) {
            parts.push(format!("{}{}", icons.git_renamed, count));
        }
        if let Some(&count) = summary.get(&GitFileStatus::Conflict) {
            parts.push(format!("{}{}", icons.git_conflict, count));
        }

        parts.join(" ")
    }

    /// Builds colored spans for the git summary.
    fn build_git_summary_spans(&self) -> Vec<Span<'static>> {
        let Some(git_status) = self.tree.git_status() else {
            return vec![];
        };

        let summary = git_status.summary();
        if summary.is_empty() {
            return vec![];
        }

        let mut spans = Vec::new();
        let mut first = true;

        let icons = &self.glyphs.icon;
        let entries: Vec<(GitFileStatus, &str, ratatui_core::style::Color)> = vec![
            (
                GitFileStatus::Modified,
                icons.git_modified,
                self.theme.git_modified_fg,
            ),
            (
                GitFileStatus::Added,
                icons.git_added,
                self.theme.git_added_fg,
            ),
            (
                GitFileStatus::Deleted,
                icons.git_deleted,
                self.theme.git_deleted_fg,
            ),
            (
                GitFileStatus::Untracked,
                icons.git_untracked,
                self.theme.git_untracked_fg,
            ),
            (
                GitFileStatus::Renamed,
                icons.git_renamed,
                self.theme.git_renamed_fg,
            ),
            (
                GitFileStatus::Conflict,
                icons.git_conflict,
                self.theme.git_conflict_fg,
            ),
        ];

        for (status, marker, color) in entries {
            if let Some(&count) = summary.get(&status) {
                if !first {
                    spans.push(Span::raw(" "));
                }
                first = false;
                spans.push(Span::styled(
                    format!("{}{}", marker, count),
                    Style::default().fg(color),
                ));
            }
        }

        spans
    }

    /// Renders the dimmed parent context line.
    fn render_parent_line(&self, buffer: &mut Buffer, area: Rect) {
        if self.tree.root() == Path::new("/") {
            return;
        }

        if let Some(parent) = self.tree.root().parent() {
            let parent_name = parent.to_string_lossy();
            let dim_style = Style::default()
                .fg(self.theme.text_secondary)
                .add_modifier(Modifier::DIM);

            let line = Line::from(vec![
                Span::styled(" \u{2934} ", dim_style), // ⤴
                Span::styled(format!("{}/", parent_name), dim_style),
            ]);
            Paragraph::new(line).render(area, buffer);
        }
    }

    /// Renders a single tree entry row.
    fn render_tree_entry(
        &self,
        flat_entry: &FlatEntry,
        is_selected: bool,
        area_width: u16,
    ) -> ListItem<'static> {
        let entry = &flat_entry.entry;

        // Tree prefix
        let prefix = tree_prefix(&flat_entry.tree_line, &self.glyphs.tree);
        let prefix_style = Style::default().fg(self.theme.panel_border);

        // Git status marker
        let (git_marker, git_color) = match flat_entry.git_status {
            Some(status) => {
                let marker = self.glyphs.icon.git_status_marker(status);
                let color = self.git_status_color(status);
                (format!("{} ", marker), color)
            }
            None => ("  ".to_string(), self.theme.text_secondary),
        };

        // Icon (from glyph tier)
        let icon = if entry.is_dir {
            self.glyphs.icon.folder
        } else {
            self.glyphs.icon.file
        };
        let icon_color = if entry.is_dir {
            self.theme.dir_color
        } else {
            self.theme.file_color
        };

        // Name with optional trailing slash for dirs
        let name_display = if entry.is_dir {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        };

        // Metadata
        let perms_str = format_permissions(entry.mode);
        let date_str = format_date_compact(entry.modified);
        let size_str = if entry.is_dir {
            "     ".to_string()
        } else {
            format!("{:>5}", format_size(entry.size))
        };

        // Calculate available width for name
        let prefix_width = crate::ui::text_width::display_width(&prefix);
        let git_marker_width = crate::ui::text_width::display_width(&git_marker);
        let icon_width = crate::ui::text_width::display_width(icon) + 1;
        let metadata_width = 20; // perms(4) + date(6) + size(6) + spacing(4)
        let fixed_width = prefix_width + git_marker_width + icon_width + metadata_width;
        let available_for_name = (area_width as usize).saturating_sub(fixed_width);

        let display_name = if crate::ui::text_width::display_width(&name_display)
            > available_for_name
            && available_for_name > 0
        {
            crate::ui::text_width::truncate_with_ellipsis(&name_display, available_for_name)
                .into_owned()
        } else {
            name_display.clone()
        };
        let name_width = crate::ui::text_width::display_width(&display_name);
        let name_padding = available_for_name.saturating_sub(name_width);

        // Determine style based on selection and spotlight
        let is_focused = flat_entry.in_focus_path;

        let name_style = if is_selected {
            Style::default()
                .fg(self.theme.selection_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            let base = if entry.is_dir {
                Style::default().fg(self.theme.dir_color)
            } else {
                Style::default().fg(self.theme.file_color)
            };
            apply_focus(base, is_focused)
        };

        let line = Line::from(vec![
            Span::styled(prefix, apply_focus(prefix_style, is_focused)),
            Span::styled(git_marker, Style::default().fg(git_color)),
            Span::styled(
                format!("{} ", icon),
                apply_focus(Style::default().fg(icon_color), is_focused),
            ),
            Span::styled(display_name, name_style),
            Span::styled(" ".repeat(name_padding), Style::default()),
            Span::styled(
                format!(" {} ", perms_str),
                apply_focus(
                    Style::default().fg(self.theme.permissions_color),
                    is_focused,
                ),
            ),
            Span::styled(
                format!("{:>5} ", date_str),
                apply_focus(Style::default().fg(self.theme.file_date_color), is_focused),
            ),
            Span::styled(
                size_str,
                apply_focus(Style::default().fg(self.theme.file_size_color), is_focused),
            ),
        ]);

        if is_selected {
            ListItem::new(line).style(Style::default().bg(self.theme.selection_bg))
        } else {
            ListItem::new(line)
        }
    }

    /// Returns the theme color for a git file status.
    fn git_status_color(&self, status: GitFileStatus) -> ratatui_core::style::Color {
        match status {
            GitFileStatus::Modified => self.theme.git_modified_fg,
            GitFileStatus::Added => self.theme.git_added_fg,
            GitFileStatus::Deleted => self.theme.git_deleted_fg,
            GitFileStatus::Untracked => self.theme.git_untracked_fg,
            GitFileStatus::Conflict => self.theme.git_conflict_fg,
            GitFileStatus::Renamed => self.theme.git_renamed_fg,
        }
    }

    // ── Input handlers ───────────────────────────────────────────────────────

    /// Handles input in file edit mode (mirrors history browser).
    fn handle_file_edit_input(&mut self, key: KeyEvent) -> Option<PanelResult> {
        let edit_state = self.edit_mode.as_mut()?;

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('u') => {
                    edit_state.undo();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('d') => {
                    edit_state.delete_token();
                    self.update_suggestions_with_file_context();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('a') => {
                    edit_state.insert_token_after();
                    self.update_suggestions_with_file_context();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('i') => {
                    edit_state.insert_token_before();
                    self.update_suggestions_with_file_context();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('q') => {
                    edit_state.cycle_quote();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('l') => {
                    edit_state.toggle_lock();
                    return Some(PanelResult::Continue);
                }
                KeyCode::Char('t') => {
                    edit_state.toggle_strip_mode();
                    return Some(PanelResult::Continue);
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                let token_text = edit_state
                    .tokens
                    .get(edit_state.selected)
                    .map(|t| t.text.as_str())
                    .unwrap_or("");

                if edit_state.edit_buffer != token_text {
                    edit_state.edit_buffer = token_text.to_string();
                    Some(PanelResult::Continue)
                } else if edit_state.is_changed() {
                    edit_state.revert();
                    Some(PanelResult::Continue)
                } else {
                    self.exit_edit_mode();
                    Some(PanelResult::Continue)
                }
            }
            KeyCode::Enter => {
                let command = edit_state.build_command();
                self.exit_edit_mode();
                Some(PanelResult::Execute(command))
            }
            KeyCode::Left => {
                edit_state.prev();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Right => {
                edit_state.next();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Home => {
                edit_state.select(0);
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::End => {
                let last = edit_state.token_count().saturating_sub(1);
                edit_state.select(last);
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Up => {
                if edit_state.strip_vertical {
                    edit_state.prev();
                    self.update_suggestions_with_file_context();
                } else {
                    edit_state.cycle_suggestion(-1);
                }
                Some(PanelResult::Continue)
            }
            KeyCode::Down => {
                if edit_state.strip_vertical {
                    edit_state.next();
                    self.update_suggestions_with_file_context();
                } else {
                    edit_state.cycle_suggestion(1);
                }
                Some(PanelResult::Continue)
            }
            KeyCode::Tab => {
                edit_state.next();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::BackTab => {
                edit_state.prev();
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Char('|') => {
                if !edit_state.edit_buffer.is_empty() {
                    if let Some(token) = edit_state.tokens.get_mut(edit_state.selected) {
                        if !token.locked {
                            token.text = edit_state.edit_buffer.clone();
                        }
                    }
                }
                let pipe_pos = edit_state.selected + 1;
                edit_state
                    .tokens
                    .insert(pipe_pos, CommandToken::new("|", TokenType::Pipe));
                let empty_pos = pipe_pos + 1;
                edit_state
                    .tokens
                    .insert(empty_pos, CommandToken::new("", TokenType::Argument));
                edit_state.selected = empty_pos;
                edit_state.edit_buffer.clear();
                edit_state.suggestion_index = None;
                self.update_suggestions_with_file_context();
                Some(PanelResult::Continue)
            }
            KeyCode::Char(c) => {
                edit_state.type_char(c);
                Some(PanelResult::Continue)
            }
            KeyCode::Backspace => {
                edit_state.backspace();
                Some(PanelResult::Continue)
            }
            _ => Some(PanelResult::Continue),
        }
    }

    /// Handles input in filter mode.
    fn handle_filter_input(&mut self, key: KeyEvent) -> PanelResult {
        match key.code {
            KeyCode::Esc => {
                self.filter.deactivate();
                PanelResult::Continue
            }
            KeyCode::Enter => {
                self.filter.deactivate();
                PanelResult::Continue
            }
            KeyCode::Char(c) => {
                self.filter.type_char(c);
                self.rebuild_filtered();
                self.scroll.reset();
                PanelResult::Continue
            }
            KeyCode::Backspace => {
                let empty = self.filter.backspace();
                if empty {
                    self.filter.deactivate();
                }
                self.rebuild_filtered();
                self.scroll.reset();
                PanelResult::Continue
            }
            _ => PanelResult::Continue,
        }
    }

    /// Handles Enter key: navigate into dir or insert file path.
    fn handle_enter(&mut self) -> PanelResult {
        if let Some(entry) = self.selected_entry().cloned() {
            if entry.entry.is_dir {
                let path = entry.entry.path.clone();
                let _ = self.navigate_to(&path);
                PanelResult::Continue
            } else {
                PanelResult::InsertText(entry.entry.path.to_string_lossy().to_string())
            }
        } else {
            PanelResult::Continue
        }
    }

    /// Handles Right/l: expand collapsed dir, or move to first child if expanded.
    fn handle_expand_or_child(&mut self) -> PanelResult {
        if let Some(tree_idx) = self.display_to_tree_index(self.scroll.selection()) {
            if let Some(entry) = self.tree.entry_at(tree_idx) {
                if entry.entry.is_dir {
                    if self.tree.is_expanded(tree_idx) {
                        // Move to first child
                        if let Some(child_idx) = self.tree.first_child_index(tree_idx) {
                            // Map child_idx back to display index
                            let display_idx = if self.filter.has_filter() {
                                self.filtered_indices.iter().position(|&i| i == child_idx)
                            } else {
                                Some(child_idx)
                            };
                            if let Some(di) = display_idx {
                                self.scroll.set_selection(di, self.visible_count());
                            }
                        }
                    } else {
                        self.tree.expand(tree_idx);
                        self.rebuild_filtered();
                    }
                }
            }
        }
        PanelResult::Continue
    }

    /// Handles Left/h: collapse expanded dir, or jump to parent entry.
    fn handle_collapse_or_parent(&mut self) -> PanelResult {
        if let Some(tree_idx) = self.display_to_tree_index(self.scroll.selection()) {
            if let Some(entry) = self.tree.entry_at(tree_idx) {
                if entry.entry.is_dir && self.tree.is_expanded(tree_idx) {
                    self.tree.collapse(tree_idx);
                    self.rebuild_filtered();
                } else if let Some(parent_idx) = self.tree.parent_index(tree_idx) {
                    // Jump to parent entry
                    let display_idx = if self.filter.has_filter() {
                        self.filtered_indices.iter().position(|&i| i == parent_idx)
                    } else {
                        Some(parent_idx)
                    };
                    if let Some(di) = display_idx {
                        self.scroll.set_selection(di, self.visible_count());
                    }
                }
            }
        }
        PanelResult::Continue
    }

    /// Handles Space: toggle expand/collapse.
    fn handle_toggle_expand(&mut self) -> PanelResult {
        if let Some(tree_idx) = self.display_to_tree_index(self.scroll.selection()) {
            self.tree.toggle_expand(tree_idx);
            self.rebuild_filtered();
        }
        PanelResult::Continue
    }
}

impl Panel for FileBrowserPanel {
    fn preferred_height(&self) -> u16 {
        if self.edit_mode.is_some() { 11 } else { 12 }
    }

    fn title(&self) -> &str {
        "Files"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // If in edit mode, render the edit UI
        if let Some(ref state) = self.edit_mode {
            self.render_file_edit_mode(buffer, area, state);
            return;
        }

        // Update focus path for spotlight dimming
        if let Some(tree_idx) = self.display_to_tree_index(self.scroll.selection()) {
            self.tree.update_focus(tree_idx);
        }

        // Build layout
        let at_root = self.tree.root() == Path::new("/");
        let filter_active = self.filter.is_active() || self.filter.has_filter();

        let mut constraints = vec![
            Constraint::Length(1), // Path header
        ];
        if !at_root {
            constraints.push(Constraint::Length(1)); // Parent context line
        }
        constraints.push(Constraint::Min(1)); // Tree view
        if filter_active {
            constraints.push(Constraint::Length(1)); // Filter bar
        }
        // Border + keybind hints are rendered externally by TabbedPanel's footer compositor.

        let chunks = Layout::vertical(constraints).split(area);
        let mut chunk_idx = 0;

        // Path header
        self.render_header(buffer, chunks[chunk_idx]);
        chunk_idx += 1;

        // Parent context line (only when not at /)
        if !at_root {
            self.render_parent_line(buffer, chunks[chunk_idx]);
            chunk_idx += 1;
        }

        // Tree view
        let tree_area = chunks[chunk_idx];
        chunk_idx += 1;

        let visible_height = tree_area.height as usize;
        let item_count = self.visible_count();
        self.scroll.ensure_visible(visible_height);
        let visible = self.scroll.visible_range(visible_height, item_count);

        let items: Vec<ListItem> = visible
            .map(|display_idx| {
                let is_selected = display_idx == self.scroll.selection();
                if let Some(tree_idx) = self.display_to_tree_index(display_idx) {
                    if let Some(flat_entry) = self.tree.entry_at(tree_idx) {
                        return self.render_tree_entry(flat_entry, is_selected, area.width);
                    }
                }
                ListItem::new(Line::raw(""))
            })
            .collect();

        let list = List::new(items);
        list.render(tree_area, buffer);

        // Filter bar
        if filter_active {
            let filter_area = chunks[chunk_idx];

            let filter_spans = self.filter.render_spans(self.theme, self.glyphs);
            let mut spans = Vec::new();
            spans.extend(filter_spans);
            // Show match count
            if self.filter.has_filter() {
                let count_str = format!("  ({}/{})", self.filtered_indices.len(), self.tree.len());
                spans.push(Span::styled(
                    count_str,
                    Style::default().fg(self.theme.text_secondary),
                ));
            }
            Paragraph::new(Line::from(spans)).render(filter_area, buffer);
        }
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // If in edit mode, delegate to edit handler
        if self.in_edit_mode() {
            if let Some(result) = self.handle_file_edit_input(key) {
                return result;
            }
        }

        // If filter is active, handle filter input
        if self.filter.is_active() {
            return self.handle_filter_input(key);
        }

        // Ctrl+key commands
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('e') => {
                    self.enter_edit_mode();
                    return PanelResult::Continue;
                }
                KeyCode::Char('h') => {
                    self.tree.toggle_hidden();
                    self.rebuild_filtered();
                    return PanelResult::Continue;
                }
                _ => {}
            }
        }

        let count = self.visible_count();

        match key.code {
            KeyCode::Esc => PanelResult::Dismiss,
            KeyCode::Enter => self.handle_enter(),
            KeyCode::Backspace => {
                // Go to parent directory (change root)
                if let Some(parent) = self.tree.root().parent() {
                    let parent_path = parent.to_path_buf();
                    let _ = self.navigate_to(&parent_path);
                }
                PanelResult::Continue
            }

            // Navigation
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll.up(count);
                PanelResult::Continue
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll.down(count);
                PanelResult::Continue
            }
            KeyCode::PageUp => {
                self.scroll.page_up(10, count);
                PanelResult::Continue
            }
            KeyCode::PageDown => {
                self.scroll.page_down(10, count);
                PanelResult::Continue
            }
            KeyCode::Home => {
                self.scroll.home();
                PanelResult::Continue
            }
            KeyCode::End => {
                self.scroll.end(count);
                PanelResult::Continue
            }

            // Tree operations
            KeyCode::Right | KeyCode::Char('l') => self.handle_expand_or_child(),
            KeyCode::Left | KeyCode::Char('h') => self.handle_collapse_or_parent(),
            KeyCode::Char(' ') => self.handle_toggle_expand(),

            // Filter
            KeyCode::Char('/') => {
                self.filter.activate();
                PanelResult::Continue
            }

            // Toggles
            KeyCode::Char('.') => {
                self.tree.toggle_hidden();
                self.rebuild_filtered();
                PanelResult::Continue
            }
            KeyCode::Char('s') => {
                self.tree.cycle_sort();
                self.rebuild_filtered();
                PanelResult::Continue
            }
            KeyCode::Char('d') => {
                self.tree.cycle_depth();
                self.rebuild_filtered();
                PanelResult::Continue
            }
            KeyCode::Char('m') => {
                self.tree.toggle_spotlight();
                PanelResult::Continue
            }
            KeyCode::Char('r') => {
                self.tree.refresh();
                self.refresh_git();
                self.rebuild_filtered();
                PanelResult::Continue
            }

            _ => PanelResult::Continue,
        }
    }

    fn footer_entries(&self) -> Vec<FooterEntry> {
        if self.edit_mode.is_some() {
            return vec![
                FooterEntry::action("↑↓", "Cycle"),
                FooterEntry::action("←→", "Nav"),
                FooterEntry::action("^A", "Add"),
                FooterEntry::action("^D", "Del"),
                FooterEntry::action("^Z", "Undo"),
                FooterEntry::action("^L", "Lock"),
                FooterEntry::action("^T", "View"),
                FooterEntry::action("Enter", "Run"),
                FooterEntry::action("Esc", "Back"),
            ];
        }
        vec![
            FooterEntry::action("^E", "Edit"),
            FooterEntry::action("Space", "Expand"),
            FooterEntry::action("/", "Filter"),
            FooterEntry::toggle(".", "Hidden", self.tree.show_hidden()),
            FooterEntry::action("s", "Sort"),
            FooterEntry::action("Enter", "Open"),
            FooterEntry::action("Esc", "Close"),
        ]
    }

    fn border_info(&self) -> Option<String> {
        if self.edit_mode.is_some() {
            return None;
        }
        let sort_label = self.tree.sort_mode().label();
        let depth_label = match self.tree.max_depth() {
            0 => "∞".to_string(),
            d => d.to_string(),
        };
        let spotlight = if self.tree.spotlight() { " ◉" } else { "" };
        Some(format!(
            "sort:{} depth:{}{}",
            sort_label, depth_label, spotlight
        ))
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_glyph_tier(&mut self, tier: super::glyphs::GlyphTier) {
        self.glyphs = super::glyphs::GlyphSet::for_tier(tier);
    }

    fn theme(&self) -> &'static super::theme::Theme {
        self.theme
    }

    fn set_theme(&mut self, theme: &'static super::theme::Theme) {
        self.theme = theme;
    }
}

// ── Formatting helpers ───────────────────────────────────────────────────────

/// Shell-quotes a string to safely handle spaces and special characters.
pub fn shell_quote(s: &str) -> String {
    let needs_quoting = s.chars().any(|c| {
        matches!(
            c,
            ' ' | '\t'
                | '\n'
                | '"'
                | '\''
                | '\\'
                | '$'
                | '`'
                | '!'
                | '*'
                | '?'
                | '['
                | ']'
                | '{'
                | '}'
                | '('
                | ')'
                | '<'
                | '>'
                | '|'
                | '&'
                | ';'
                | '#'
                | '~'
        )
    });

    if !needs_quoting && !s.is_empty() {
        return s.to_string();
    }

    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

/// Formats a file size in human-readable form.
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}M", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}K", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}

/// Formats Unix permissions as 3-digit octal.
fn format_permissions(mode: u32) -> String {
    format!("{:03o}", mode & 0o777)
}

/// Formats a date in compact form.
fn format_date_compact(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "-".to_string();
    };

    let Ok(duration) = time.elapsed() else {
        return "-".to_string();
    };

    let secs = duration.as_secs();
    let days = secs / 86400;

    if days == 0 {
        "Today".to_string()
    } else if days == 1 {
        "Yday".to_string()
    } else if days < 7 {
        format!("{}d", days)
    } else if days < 365 {
        let months = days / 30;
        if months < 1 {
            format!("{}d", days)
        } else {
            format!("{}mo", months)
        }
    } else {
        format!("{}y", days / 365)
    }
}

#[cfg(test)]
mod tests {
    use super::super::file_tree::SortMode;
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    #[allow(non_snake_case)]
    fn test_FileBrowserPanel_new_with_default_theme_and_nerdfont_shows_hidden_false_and_sorted_by_name()
     {
        let panel = FileBrowserPanel::new(&AMBER_THEME, GlyphTier::NerdFont);
        assert!(!panel.tree.show_hidden());
        assert_eq!(panel.tree.sort_mode(), SortMode::Name);
    }

    #[test]
    fn test_format_size_various_units_expected_strings() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(1024), "1.0K");
        assert_eq!(format_size(1536), "1.5K");
        assert_eq!(format_size(1048576), "1.0M");
        assert_eq!(format_size(1073741824), "1.0G");
    }

    #[test]
    fn test_shell_quote_plain_path_returns_unquoted() {
        assert_eq!(shell_quote("filename.txt"), "filename.txt");
        assert_eq!(shell_quote("path/to/file"), "path/to/file");
    }

    #[test]
    fn test_shell_quote_path_with_spaces_returns_single_quoted() {
        assert_eq!(shell_quote("file name.txt"), "'file name.txt'");
        assert_eq!(
            shell_quote("path with spaces/file"),
            "'path with spaces/file'"
        );
    }

    #[test]
    fn test_shell_quote_path_with_single_quote_returns_escaped() {
        assert_eq!(shell_quote("it's here"), "'it'\\''s here'");
    }

    #[test]
    fn test_shell_quote_path_with_special_chars_returns_quoted() {
        assert_eq!(shell_quote("file$var"), "'file$var'");
        assert_eq!(shell_quote("file*"), "'file*'");
        assert_eq!(shell_quote("file?"), "'file?'");
    }

    #[test]
    fn test_shell_quote_empty_returns_empty_quotes() {
        assert_eq!(shell_quote(""), "''");
    }
}
