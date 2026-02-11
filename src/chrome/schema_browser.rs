//! Schema browser panel for exploring command schemas.
//!
//! Provides a tree view of command schemas with search, exploration,
//! and manual discovery capabilities. This panel is the "Browser" sub-tab
//! within the Commands compound panel.

use std::any::Any;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};
use ratatui_widgets::paragraph::Paragraph;

use super::footer_bar::FooterEntry;
use super::panel::{Panel, PanelResult};
use super::theme::Theme;
use crate::history_store::HistoryStore;

/// A node in the schema tree view.
#[derive(Debug, Clone)]
enum TreeNode {
    /// A top-level command.
    Command {
        name: String,
        description: Option<String>,
        flag_count: usize,
        subcommand_count: usize,
    },
    /// A subcommand nested under a command.
    Subcommand {
        name: String,
        description: Option<String>,
        depth: usize,
    },
    /// A flag nested under a command or subcommand.
    Flag {
        short: Option<String>,
        long: Option<String>,
        description: Option<String>,
        depth: usize,
    },
}

impl TreeNode {
    fn depth(&self) -> usize {
        match self {
            TreeNode::Command { .. } => 0,
            TreeNode::Subcommand { depth, .. } | TreeNode::Flag { depth, .. } => *depth,
        }
    }

    fn display_name(&self) -> String {
        match self {
            TreeNode::Command { name, .. } | TreeNode::Subcommand { name, .. } => name.clone(),
            TreeNode::Flag { short, long, .. } => match (short, long) {
                (Some(s), Some(l)) => format!("{s}, {l}"),
                (None, Some(l)) => l.clone(),
                (Some(s), None) => s.clone(),
                (None, None) => "(unnamed)".to_string(),
            },
        }
    }

    /// Returns the searchable text for filtering (name + description).
    fn matches_filter(&self, filter_lower: &str) -> bool {
        match self {
            TreeNode::Command {
                name, description, ..
            } => {
                name.to_lowercase().contains(filter_lower)
                    || description
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(filter_lower))
            }
            TreeNode::Subcommand {
                name, description, ..
            } => {
                name.to_lowercase().contains(filter_lower)
                    || description
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(filter_lower))
            }
            TreeNode::Flag {
                short,
                long,
                description,
                ..
            } => {
                short
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase().contains(filter_lower))
                    || long
                        .as_ref()
                        .is_some_and(|l| l.to_lowercase().contains(filter_lower))
                    || description
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(filter_lower))
            }
        }
    }

    /// Returns true if this node has expandable children.
    fn has_children(&self) -> bool {
        match self {
            TreeNode::Command {
                subcommand_count,
                flag_count,
                ..
            } => *subcommand_count > 0 || *flag_count > 0,
            _ => false,
        }
    }
}

/// Schema browser panel for exploring command schemas.
pub struct SchemaBrowserPanel {
    /// Flat list of tree nodes (expanded view).
    nodes: Vec<TreeNode>,
    /// Currently selected index.
    selection: usize,
    /// Scroll offset.
    scroll_offset: usize,
    /// Current filter/search text.
    filter: String,
    /// Filtered node indices.
    filtered: Vec<usize>,
    /// History store for schema provider access.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Theme.
    theme: &'static Theme,
    /// Status message shown in footer.
    status: Option<String>,
}

impl SchemaBrowserPanel {
    /// Creates a new schema browser panel.
    pub fn new(theme: &'static Theme) -> Self {
        Self {
            nodes: Vec::new(),
            selection: 0,
            scroll_offset: 0,
            filter: String::new(),
            filtered: Vec::new(),
            history_store: None,
            theme,
            status: None,
        }
    }

    /// Sets the history store for schema provider access.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.history_store = Some(store);
        self.load_schemas();
    }

    /// Loads all schemas from the provider into the tree view.
    fn load_schemas(&mut self) {
        self.nodes.clear();

        // Clone the Arc to avoid borrowing self.history_store while mutating self
        let store = match self.history_store.clone() {
            Some(s) => s,
            None => {
                self.apply_filter();
                return;
            }
        };

        let guard = match store.lock() {
            Ok(g) => g,
            Err(_) => {
                self.apply_filter();
                return;
            }
        };

        let provider = match guard.schema_provider() {
            Some(p) => p,
            None => {
                drop(guard);
                self.apply_filter();
                return;
            }
        };

        // Collect all command names, sort alphabetically
        let mut commands: Vec<&str> = provider.commands().collect();
        commands.sort_unstable();

        for cmd_name in commands {
            if let Some(schema) = provider.get(cmd_name) {
                self.nodes.push(TreeNode::Command {
                    name: schema.command.clone(),
                    description: schema.description.clone(),
                    flag_count: schema.global_flags.len(),
                    subcommand_count: schema.subcommands.len(),
                });

                // Add global flags
                for flag in &schema.global_flags {
                    self.nodes.push(TreeNode::Flag {
                        short: flag.short.clone(),
                        long: flag.long.clone(),
                        description: flag.description.clone(),
                        depth: 1,
                    });
                }

                // Add subcommands and their flags recursively
                Self::add_subcommands_recursive(&mut self.nodes, &schema.subcommands, 1);
            }
        }

        let count = provider.schema_count();
        let bundled = if provider.is_bundled_available() {
            " (bundled)"
        } else {
            ""
        };
        self.status = Some(format!("{count} schemas{bundled}"));

        drop(guard);
        self.apply_filter();
    }

    /// Discovers a command schema from its --help output.
    ///
    /// Uses the current filter text as the command name to discover.
    fn discover_command(&mut self) {
        let command = self.filter.trim().to_string();
        if command.is_empty() {
            return;
        }

        let store = match self.history_store.clone() {
            Some(s) => s,
            None => return,
        };

        let mut guard = match store.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        match guard.discover_schema(&command) {
            Ok(()) => {
                drop(guard);
                self.load_schemas();
            }
            Err(e) => {
                self.status = Some(format!("Discovery failed: {e}"));
            }
        }
    }

    /// Recursively adds subcommands and their flags to the node list.
    fn add_subcommands_recursive(
        nodes: &mut Vec<TreeNode>,
        subcommands: &[command_schema_core::SubcommandSchema],
        depth: usize,
    ) {
        for sub in subcommands {
            nodes.push(TreeNode::Subcommand {
                name: sub.name.clone(),
                description: sub.description.clone(),
                depth,
            });

            for flag in &sub.flags {
                nodes.push(TreeNode::Flag {
                    short: flag.short.clone(),
                    long: flag.long.clone(),
                    description: flag.description.clone(),
                    depth: depth + 1,
                });
            }

            // Recurse into nested subcommands
            if !sub.subcommands.is_empty() {
                Self::add_subcommands_recursive(nodes, &sub.subcommands, depth + 1);
            }
        }
    }

    /// Applies the current filter text.
    ///
    /// When a filter is active, searches across all node types (commands,
    /// subcommands, and flags) by name and description. Matching child nodes
    /// also pull in their parent command for context.
    fn apply_filter(&mut self) {
        self.filtered.clear();

        if self.filter.is_empty() {
            // Show only top-level commands when no filter
            for (i, node) in self.nodes.iter().enumerate() {
                if matches!(node, TreeNode::Command { .. }) {
                    self.filtered.push(i);
                }
            }
        } else {
            let filter_lower = self.filter.to_lowercase();
            // Track the current parent command index for child matches
            let mut current_cmd_idx: Option<usize> = None;
            let mut cmd_already_added = false;

            for (i, node) in self.nodes.iter().enumerate() {
                match node {
                    TreeNode::Command { .. } => {
                        current_cmd_idx = Some(i);
                        if node.matches_filter(&filter_lower) {
                            self.filtered.push(i);
                            cmd_already_added = true;
                        } else {
                            cmd_already_added = false;
                        }
                    }
                    TreeNode::Subcommand { .. } | TreeNode::Flag { .. } => {
                        if cmd_already_added {
                            // Parent command matched — include all children
                            self.filtered.push(i);
                        } else if node.matches_filter(&filter_lower) {
                            // Child matched — pull in parent command first if not yet added
                            if let Some(cmd_i) = current_cmd_idx {
                                if self.filtered.last() != Some(&cmd_i) {
                                    self.filtered.push(cmd_i);
                                }
                            }
                            self.filtered.push(i);
                        }
                    }
                }
            }
        }

        self.selection = 0;
        self.scroll_offset = 0;
    }

    /// Ensures the selection is visible.
    fn ensure_visible(&mut self, visible_count: usize) {
        if visible_count == 0 {
            return;
        }
        if self.selection < self.scroll_offset {
            self.scroll_offset = self.selection;
        } else if self.selection >= self.scroll_offset + visible_count {
            self.scroll_offset = self.selection.saturating_sub(visible_count - 1);
        }
    }

    /// Returns true if the selected command is currently expanded.
    fn is_expanded(&self) -> bool {
        let Some(&node_idx) = self.filtered.get(self.selection) else {
            return false;
        };
        if !matches!(self.nodes[node_idx], TreeNode::Command { .. }) {
            return false;
        }
        self.filtered
            .get(self.selection + 1)
            .is_some_and(|&next_idx| self.nodes[next_idx].depth() > 0)
    }

    /// Expands the selected command node (no-op if already expanded or not a command).
    fn expand(&mut self) {
        if self.is_expanded() {
            return;
        }
        self.toggle_expand();
    }

    /// Collapses the selected command node (no-op if already collapsed or not a command).
    fn collapse(&mut self) {
        if !self.is_expanded() {
            return;
        }
        self.toggle_expand();
    }

    /// Toggles expansion of the selected command node.
    fn toggle_expand(&mut self) {
        let Some(&node_idx) = self.filtered.get(self.selection) else {
            return;
        };

        // Only commands can be expanded/collapsed
        if !matches!(self.nodes[node_idx], TreeNode::Command { .. }) {
            return;
        }

        // Check if children are currently shown
        if self.is_expanded() {
            // Collapse: remove children from filtered
            let remove_start = self.selection + 1;
            let remove_end = self.filtered[remove_start..]
                .iter()
                .position(|&idx| matches!(self.nodes[idx], TreeNode::Command { .. }))
                .map(|pos| remove_start + pos)
                .unwrap_or(self.filtered.len());
            self.filtered.drain(remove_start..remove_end);
        } else {
            // Expand: add children after current position using splice for O(n) performance
            let children: Vec<usize> = ((node_idx + 1)..self.nodes.len())
                .take_while(|&i| !matches!(self.nodes[i], TreeNode::Command { .. }))
                .collect();
            let insert_pos = self.selection + 1;
            self.filtered.splice(insert_pos..insert_pos, children);
        }
    }

    /// Returns the insert text for the currently selected node.
    fn selected_insert_text(&self) -> Option<String> {
        let &node_idx = self.filtered.get(self.selection)?;
        match &self.nodes[node_idx] {
            TreeNode::Command { name, .. } | TreeNode::Subcommand { name, .. } => {
                Some(name.clone())
            }
            TreeNode::Flag { long, short, .. } => long.clone().or_else(|| short.clone()),
        }
    }
}

impl Panel for SchemaBrowserPanel {
    fn preferred_height(&self) -> u16 {
        8
    }

    fn title(&self) -> &str {
        "Browser"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // Layout: filter (1 line), list (flexible)
        // Status is shown via border_info(), rendered externally by TabbedPanel.
        let chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);

        // Render filter input
        let filter_text = if self.filter.is_empty() {
            Span::styled(
                "Type to search schemas...",
                Style::default().fg(self.theme.text_secondary),
            )
        } else {
            Span::styled(&self.filter, Style::default().fg(self.theme.text_primary))
        };
        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(self.theme.text_highlight)),
            filter_text,
        ]);
        Paragraph::new(filter_line).render(chunks[0], buffer);

        // Render tree list
        if self.filtered.is_empty() {
            if self.nodes.is_empty() {
                let secondary = Style::default().fg(self.theme.text_secondary);
                let highlight = Style::default()
                    .fg(self.theme.text_highlight)
                    .add_modifier(Modifier::BOLD);
                let lines = vec![
                    Line::from(""),
                    Line::from(Span::styled("No command schemas loaded.", secondary)),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Schemas are added by: ", secondary),
                        Span::styled("^D", highlight),
                        Span::styled(" to scan a command's --help,", secondary),
                    ]),
                    Line::from(vec![
                        Span::styled("or compile with ", secondary),
                        Span::styled("bundled-schemas", highlight),
                        Span::styled(" feature for 975 built-in.", secondary),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Type a command name above, then press ", secondary),
                        Span::styled("^D", highlight),
                        Span::styled(" to discover.", secondary),
                    ]),
                ];
                Paragraph::new(lines).render(chunks[1], buffer);
            } else {
                let help = Line::from(Span::styled(
                    "No matching schemas.",
                    Style::default().fg(self.theme.text_secondary),
                ));
                Paragraph::new(vec![Line::from(""), help]).render(chunks[1], buffer);
            }
        } else {
            let visible_height = chunks[1].height as usize;
            self.ensure_visible(visible_height);

            let items: Vec<ListItem> = self
                .filtered
                .iter()
                .skip(self.scroll_offset)
                .take(visible_height)
                .enumerate()
                .map(|(display_idx, &node_idx)| {
                    let node = &self.nodes[node_idx];
                    let actual_idx = self.scroll_offset + display_idx;
                    let is_selected = actual_idx == self.selection;

                    let indent = "  ".repeat(node.depth());
                    let prefix = match node {
                        TreeNode::Command {
                            subcommand_count,
                            flag_count,
                            ..
                        } => {
                            let is_expanded = self
                                .filtered
                                .get(actual_idx + 1)
                                .is_some_and(|&next| self.nodes[next].depth() > 0);
                            if *subcommand_count > 0 || *flag_count > 0 {
                                if is_expanded {
                                    "▼ "
                                } else {
                                    "▶ "
                                }
                            } else {
                                "  "
                            }
                        }
                        TreeNode::Subcommand { .. } => "├─ ",
                        TreeNode::Flag { .. } => "│  ",
                    };

                    let name = node.display_name();
                    let desc = match node {
                        TreeNode::Command { description, .. }
                        | TreeNode::Subcommand { description, .. }
                        | TreeNode::Flag { description, .. } => {
                            description.as_deref().unwrap_or("")
                        }
                    };

                    let name_style = if is_selected {
                        Style::default()
                            .fg(self.theme.selection_fg)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        match node {
                            TreeNode::Command { .. } => {
                                Style::default().fg(self.theme.text_primary)
                            }
                            TreeNode::Subcommand { .. } => {
                                Style::default().fg(self.theme.text_highlight)
                            }
                            TreeNode::Flag { .. } => {
                                Style::default().fg(self.theme.text_secondary)
                            }
                        }
                    };

                    let mut spans = vec![
                        Span::styled(
                            format!("{indent}{prefix}"),
                            Style::default().fg(self.theme.panel_border),
                        ),
                        Span::styled(name, name_style),
                    ];

                    if !desc.is_empty() {
                        spans.push(Span::styled(
                            format!("  {desc}"),
                            Style::default().fg(self.theme.text_secondary),
                        ));
                    }

                    let line = Line::from(spans);
                    if is_selected {
                        ListItem::new(line).style(Style::default().bg(self.theme.selection_bg))
                    } else {
                        ListItem::new(line)
                    }
                })
                .collect();

            List::new(items).render(chunks[1], buffer);
        }

        // Render status line
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // Ctrl+ keybinds — checked before the main match so they don't fall
        // through to the filter's Char(c) handler.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => {
                    self.discover_command();
                    return PanelResult::Continue;
                }
                KeyCode::Char('r') => {
                    self.load_schemas();
                    return PanelResult::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => PanelResult::Dismiss,
            KeyCode::Up => {
                if self.selection > 0 {
                    self.selection -= 1;
                }
                PanelResult::Continue
            }
            KeyCode::Down => {
                if self.selection + 1 < self.filtered.len() {
                    self.selection += 1;
                }
                PanelResult::Continue
            }
            KeyCode::PageUp => {
                self.selection = self.selection.saturating_sub(10);
                PanelResult::Continue
            }
            KeyCode::PageDown => {
                self.selection =
                    (self.selection + 10).min(self.filtered.len().saturating_sub(1));
                PanelResult::Continue
            }
            KeyCode::Home => {
                self.selection = 0;
                PanelResult::Continue
            }
            KeyCode::End => {
                self.selection = self.filtered.len().saturating_sub(1);
                PanelResult::Continue
            }
            KeyCode::Enter => {
                // Toggle expand/collapse for commands, insert text for leaves
                let is_command = self
                    .filtered
                    .get(self.selection)
                    .is_some_and(|&idx| matches!(self.nodes[idx], TreeNode::Command { .. }));

                if is_command {
                    self.toggle_expand();
                    PanelResult::Continue
                } else if let Some(text) = self.selected_insert_text() {
                    PanelResult::InsertText(text)
                } else {
                    PanelResult::Continue
                }
            }
            KeyCode::Right => {
                // Expand selected command (standard tree: Right = expand only, not toggle)
                if let Some(&idx) = self.filtered.get(self.selection) {
                    if matches!(self.nodes[idx], TreeNode::Command { .. }) {
                        if self.is_expanded() {
                            // Already expanded: move to first child
                            if self.selection + 1 < self.filtered.len() {
                                self.selection += 1;
                            }
                        } else {
                            self.expand();
                        }
                    }
                }
                PanelResult::Continue
            }
            KeyCode::Left => {
                // Collapse: jump to parent command if on a child node
                if let Some(&node_idx) = self.filtered.get(self.selection) {
                    if self.nodes[node_idx].depth() > 0 {
                        // Find parent command
                        for i in (0..self.selection).rev() {
                            if let Some(&parent_idx) = self.filtered.get(i) {
                                if matches!(self.nodes[parent_idx], TreeNode::Command { .. }) {
                                    self.selection = i;
                                    break;
                                }
                            }
                        }
                    } else {
                        // Already on command: collapse it
                        self.collapse();
                    }
                }
                PanelResult::Continue
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.apply_filter();
                PanelResult::Continue
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.apply_filter();
                PanelResult::Continue
            }
            _ => PanelResult::Continue,
        }
    }

    fn footer_entries(&self) -> Vec<FooterEntry> {
        vec![
            FooterEntry::action("^D", "Discover"),
            FooterEntry::action("^R", "Refresh"),
            FooterEntry::action("Enter", "Expand"),
            FooterEntry::action("Esc", "Close"),
        ]
    }

    fn border_info(&self) -> Option<String> {
        self.status.clone()
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;

    use super::super::theme::AMBER_THEME;
    use super::*;

    /// Creates a panel pre-populated with test nodes for interactive tests.
    fn panel_with_nodes() -> SchemaBrowserPanel {
        let mut panel = SchemaBrowserPanel::new(&AMBER_THEME);
        panel.nodes = vec![
            TreeNode::Command {
                name: "git".into(),
                description: Some("Distributed VCS".into()),
                flag_count: 1,
                subcommand_count: 2,
            },
            TreeNode::Flag {
                short: Some("-v".into()),
                long: Some("--verbose".into()),
                description: Some("Be verbose".into()),
                depth: 1,
            },
            TreeNode::Subcommand {
                name: "commit".into(),
                description: Some("Record changes".into()),
                depth: 1,
            },
            TreeNode::Flag {
                short: Some("-m".into()),
                long: Some("--message".into()),
                description: Some("Commit message".into()),
                depth: 2,
            },
            TreeNode::Subcommand {
                name: "push".into(),
                description: Some("Push to remote".into()),
                depth: 1,
            },
            TreeNode::Command {
                name: "cargo".into(),
                description: Some("Rust package manager".into()),
                flag_count: 0,
                subcommand_count: 1,
            },
            TreeNode::Subcommand {
                name: "build".into(),
                description: Some("Compile the project".into()),
                depth: 1,
            },
        ];
        panel.apply_filter();
        panel
    }

    #[test]
    fn test_schema_browser_new_initial_state() {
        let panel = SchemaBrowserPanel::new(&AMBER_THEME);
        assert!(panel.nodes.is_empty());
        assert!(panel.filtered.is_empty());
        assert_eq!(panel.selection, 0);
        assert!(panel.filter.is_empty());
    }

    #[test]
    fn test_schema_browser_title() {
        let panel = SchemaBrowserPanel::new(&AMBER_THEME);
        assert_eq!(panel.title(), "Browser");
    }

    #[test]
    fn test_schema_browser_preferred_height() {
        let panel = SchemaBrowserPanel::new(&AMBER_THEME);
        assert_eq!(panel.preferred_height(), 8);
    }

    #[test]
    fn test_tree_node_display_name_flag_variants() {
        let both = TreeNode::Flag {
            short: Some("-v".into()),
            long: Some("--verbose".into()),
            description: None,
            depth: 1,
        };
        assert_eq!(both.display_name(), "-v, --verbose");

        let long_only = TreeNode::Flag {
            short: None,
            long: Some("--help".into()),
            description: None,
            depth: 1,
        };
        assert_eq!(long_only.display_name(), "--help");

        let short_only = TreeNode::Flag {
            short: Some("-h".into()),
            long: None,
            description: None,
            depth: 1,
        };
        assert_eq!(short_only.display_name(), "-h");
    }

    #[test]
    fn test_tree_node_depth() {
        let cmd = TreeNode::Command {
            name: "git".into(),
            description: None,
            flag_count: 0,
            subcommand_count: 0,
        };
        assert_eq!(cmd.depth(), 0);

        let sub = TreeNode::Subcommand {
            name: "commit".into(),
            description: None,
            depth: 1,
        };
        assert_eq!(sub.depth(), 1);

        let flag = TreeNode::Flag {
            short: None,
            long: Some("--message".into()),
            description: None,
            depth: 2,
        };
        assert_eq!(flag.depth(), 2);
    }

    #[test]
    fn test_tree_node_has_children() {
        let with = TreeNode::Command {
            name: "git".into(),
            description: None,
            flag_count: 1,
            subcommand_count: 2,
        };
        assert!(with.has_children());

        let without = TreeNode::Command {
            name: "ls".into(),
            description: None,
            flag_count: 0,
            subcommand_count: 0,
        };
        assert!(!without.has_children());
    }

    #[test]
    fn test_apply_filter_no_filter_shows_commands_only() {
        let panel = panel_with_nodes();
        // No filter → only top-level commands
        assert_eq!(panel.filtered.len(), 2);
        assert_eq!(panel.nodes[panel.filtered[0]].display_name(), "git");
        assert_eq!(panel.nodes[panel.filtered[1]].display_name(), "cargo");
    }

    #[test]
    fn test_apply_filter_by_command_name() {
        let mut panel = panel_with_nodes();
        panel.filter = "git".into();
        panel.apply_filter();
        // "git" matches → shows git + all children
        assert!(panel.filtered.len() >= 2);
        assert_eq!(panel.nodes[panel.filtered[0]].display_name(), "git");
    }

    #[test]
    fn test_apply_filter_by_subcommand_name() {
        let mut panel = panel_with_nodes();
        panel.filter = "build".into();
        panel.apply_filter();
        // "build" subcommand under cargo → should show cargo parent + build child
        assert_eq!(panel.filtered.len(), 2);
        assert_eq!(panel.nodes[panel.filtered[0]].display_name(), "cargo");
        assert_eq!(panel.nodes[panel.filtered[1]].display_name(), "build");
    }

    #[test]
    fn test_apply_filter_by_flag_name() {
        let mut panel = panel_with_nodes();
        panel.filter = "--verbose".into();
        panel.apply_filter();
        // --verbose flag under git → should show git parent + the flag
        assert!(panel.filtered.len() >= 2);
        assert_eq!(panel.nodes[panel.filtered[0]].display_name(), "git");
    }

    #[test]
    fn test_apply_filter_no_match() {
        let mut panel = panel_with_nodes();
        panel.filter = "zzzznotfound".into();
        panel.apply_filter();
        assert!(panel.filtered.is_empty());
    }

    #[test]
    fn test_toggle_expand_collapse() {
        let mut panel = panel_with_nodes();
        assert_eq!(panel.filtered.len(), 2); // git, cargo (collapsed)

        // Expand git
        panel.selection = 0;
        panel.toggle_expand();
        // git + its children (flag, commit, commit-flag, push) = 5, plus cargo = 6
        assert!(panel.filtered.len() > 2);
        let first_child = panel.filtered[1];
        assert!(panel.nodes[first_child].depth() > 0);

        // Collapse git
        panel.selection = 0;
        panel.toggle_expand();
        assert_eq!(panel.filtered.len(), 2); // back to commands only
    }

    #[test]
    fn test_expand_only_does_not_collapse() {
        let mut panel = panel_with_nodes();
        panel.selection = 0;
        panel.expand();
        let expanded_len = panel.filtered.len();
        assert!(expanded_len > 2);

        // expand again should be a no-op (not toggle)
        panel.selection = 0;
        panel.expand();
        assert_eq!(panel.filtered.len(), expanded_len);
    }

    #[test]
    fn test_collapse_only_does_not_expand() {
        let mut panel = panel_with_nodes();
        panel.selection = 0;
        // collapse when already collapsed → no-op
        panel.collapse();
        assert_eq!(panel.filtered.len(), 2);
    }

    #[test]
    fn test_right_key_expands_then_moves_to_child() {
        let mut panel = panel_with_nodes();
        assert_eq!(panel.selection, 0);

        // First Right: expand
        let key = KeyEvent::from(KeyCode::Right);
        panel.handle_input(key);
        assert!(panel.filtered.len() > 2); // expanded
        assert_eq!(panel.selection, 0); // still on git

        // Second Right: move to first child
        panel.handle_input(key);
        assert_eq!(panel.selection, 1); // moved to first child
    }

    #[test]
    fn test_left_key_jumps_to_parent() {
        let mut panel = panel_with_nodes();
        panel.selection = 0;
        panel.expand();
        panel.selection = 2; // on "commit" subcommand

        let key = KeyEvent::from(KeyCode::Left);
        panel.handle_input(key);
        assert_eq!(panel.selection, 0); // jumped back to "git"
    }

    #[test]
    fn test_left_key_collapses_on_command() {
        let mut panel = panel_with_nodes();
        panel.selection = 0;
        panel.expand();
        assert!(panel.filtered.len() > 2);

        // Left on the command itself → collapse
        panel.selection = 0;
        let key = KeyEvent::from(KeyCode::Left);
        panel.handle_input(key);
        assert_eq!(panel.filtered.len(), 2); // collapsed
    }

    #[test]
    fn test_page_up_down_home_end() {
        let mut panel = panel_with_nodes();
        // Expand both to have more items
        panel.selection = 0;
        panel.expand();
        panel.selection = panel.filtered.len() - 1; // go to cargo (last command)
        // Find cargo in filtered
        for (i, &idx) in panel.filtered.iter().enumerate() {
            if panel.nodes[idx].display_name() == "cargo" {
                panel.selection = i;
                break;
            }
        }
        panel.expand();

        let total = panel.filtered.len();
        assert!(total > 3);

        // Home
        panel.selection = total / 2;
        panel.handle_input(KeyEvent::from(KeyCode::Home));
        assert_eq!(panel.selection, 0);

        // End
        panel.handle_input(KeyEvent::from(KeyCode::End));
        assert_eq!(panel.selection, total - 1);

        // PageDown from start
        panel.selection = 0;
        panel.handle_input(KeyEvent::from(KeyCode::PageDown));
        assert!(panel.selection > 0);

        // PageUp from end
        panel.selection = total - 1;
        panel.handle_input(KeyEvent::from(KeyCode::PageUp));
        assert!(panel.selection < total - 1);
    }

    #[test]
    fn test_selected_insert_text() {
        let mut panel = panel_with_nodes();
        // Select "git" command
        assert_eq!(panel.selected_insert_text(), Some("git".into()));

        // Expand and select a flag
        panel.expand();
        panel.selection = 1; // --verbose flag
        let text = panel.selected_insert_text().unwrap();
        assert!(text == "--verbose" || text == "-v");
    }

    #[test]
    fn test_enter_on_leaf_returns_insert_text() {
        let mut panel = panel_with_nodes();
        panel.selection = 0;
        panel.expand();
        panel.selection = 1; // flag node

        let result = panel.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(result, PanelResult::InsertText(_)));
    }

    #[test]
    fn test_enter_on_command_toggles_expand() {
        let mut panel = panel_with_nodes();
        let initial = panel.filtered.len();

        let result = panel.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(result, PanelResult::Continue));
        assert!(panel.filtered.len() > initial); // expanded
    }

    #[test]
    fn test_matches_filter_searches_all_node_types() {
        let cmd = TreeNode::Command {
            name: "docker".into(),
            description: Some("Container runtime".into()),
            flag_count: 0,
            subcommand_count: 0,
        };
        assert!(cmd.matches_filter("docker"));
        assert!(cmd.matches_filter("container"));
        assert!(!cmd.matches_filter("zzz"));

        let sub = TreeNode::Subcommand {
            name: "compose".into(),
            description: Some("Multi-container apps".into()),
            depth: 1,
        };
        assert!(sub.matches_filter("compose"));
        assert!(sub.matches_filter("multi"));

        let flag = TreeNode::Flag {
            short: Some("-d".into()),
            long: Some("--detach".into()),
            description: Some("Run in background".into()),
            depth: 1,
        };
        assert!(flag.matches_filter("detach"));
        assert!(flag.matches_filter("background"));
        assert!(flag.matches_filter("-d"));
    }

    #[test]
    fn test_ensure_visible_scrolls_down() {
        let mut panel = panel_with_nodes();
        panel.selection = 0;
        panel.expand();
        // Simulate small viewport
        panel.selection = panel.filtered.len() - 1;
        panel.ensure_visible(3);
        assert!(panel.scroll_offset > 0);
    }

    #[test]
    fn test_ensure_visible_scrolls_up() {
        let mut panel = panel_with_nodes();
        panel.scroll_offset = 5;
        panel.selection = 0;
        panel.ensure_visible(3);
        assert_eq!(panel.scroll_offset, 0);
    }
}
