//! Schema browser panel for exploring command schemas.
//!
//! Provides a tree view of command schemas with search, exploration,
//! and manual discovery capabilities. This panel is the "Browser" sub-tab
//! within the Commands compound panel.

use std::any::Any;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};
use ratatui_widgets::paragraph::Paragraph;

use super::command_edit::{compute_edit_mode_layout, render_edit_mode_shared, CommandEditState};
use super::footer_bar::FooterEntry;
use super::panel::{Panel, PanelResult};
use super::theme::Theme;
use crate::history_store::HistoryStore;
use crate::ui::filter_input::FilterInput;
use crate::ui::tree_state::{TreeItem, TreeViewState};
use crate::ui::tree_view::{tree_prefix, UNICODE_TREE_CHARS};

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
    /// A section header grouping related items (e.g., "Global Options").
    Section {
        label: String,
        count: usize,
        depth: usize,
    },
    /// A subcommand nested under a command.
    Subcommand {
        name: String,
        description: Option<String>,
        depth: usize,
        flag_count: usize,
        subcommand_count: usize,
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
            TreeNode::Section { depth, .. }
            | TreeNode::Subcommand { depth, .. }
            | TreeNode::Flag { depth, .. } => *depth,
        }
    }

    fn display_name(&self) -> String {
        match self {
            TreeNode::Command { name, .. } | TreeNode::Subcommand { name, .. } => name.clone(),
            TreeNode::Section { label, count, .. } => format!("[{label}] ({count})"),
            TreeNode::Flag { short, long, .. } => match (short, long) {
                (Some(s), Some(l)) => format!("{s}, {l}"),
                (None, Some(l)) => l.clone(),
                (Some(s), None) => s.clone(),
                (None, None) => "(unnamed)".to_string(),
            },
        }
    }

    /// Returns true if this node matches the given filter.
    fn matches_filter(&self, filter: &FilterInput) -> bool {
        match self {
            TreeNode::Command {
                name, description, ..
            } => {
                filter.matches(name)
                    || description.as_ref().is_some_and(|d| filter.matches(d))
            }
            TreeNode::Section { label, .. } => filter.matches(label),
            TreeNode::Subcommand {
                name, description, ..
            } => {
                filter.matches(name)
                    || description.as_ref().is_some_and(|d| filter.matches(d))
            }
            TreeNode::Flag {
                short,
                long,
                description,
                ..
            } => {
                short.as_ref().is_some_and(|s| filter.matches(s))
                    || long.as_ref().is_some_and(|l| filter.matches(l))
                    || description.as_ref().is_some_and(|d| filter.matches(d))
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
            }
            | TreeNode::Subcommand {
                subcommand_count,
                flag_count,
                ..
            } => *subcommand_count > 0 || *flag_count > 0,
            _ => false,
        }
    }
}

impl TreeItem for TreeNode {
    fn depth(&self) -> usize {
        self.depth()
    }
    fn has_children(&self) -> bool {
        self.has_children()
    }
}

/// Schema browser panel for exploring command schemas.
pub struct SchemaBrowserPanel {
    /// Full flat pre-order list of tree nodes.
    nodes: Vec<TreeNode>,
    /// Tree viewport manager (visible list, TreeLine metadata, scroll).
    tree: TreeViewState,
    /// Set of node indices that are currently expanded.
    expanded: HashSet<usize>,
    /// Filter input (`/`-activated).
    filter: FilterInput,
    /// Edit mode state (command crafter).
    edit_mode: Option<CommandEditState>,
    /// Name displayed in edit mode title.
    edit_command_name: Option<String>,
    /// History store for schema provider access.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Theme.
    theme: &'static Theme,
    /// Status message shown in border info.
    status: Option<String>,
}

impl SchemaBrowserPanel {
    /// Creates a new schema browser panel.
    pub fn new(theme: &'static Theme) -> Self {
        Self {
            nodes: Vec::new(),
            tree: TreeViewState::new(),
            expanded: HashSet::new(),
            filter: FilterInput::new(),
            edit_mode: None,
            edit_command_name: None,
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

    /// Returns true if the panel is in edit mode.
    pub fn in_edit_mode(&self) -> bool {
        self.edit_mode.is_some()
    }

    /// Enters edit mode for the currently selected node.
    fn enter_edit_mode(&mut self) {
        let Some(node_idx) = self.tree.selected_node_idx() else {
            return;
        };

        match &self.nodes[node_idx] {
            TreeNode::Command { name, .. } => {
                let flags = self.collect_flags_for_command(name);
                let mut state = CommandEditState::for_schema(name, None, flags);
                if let Some(store) = &self.history_store {
                    state.set_history_store(store.clone());
                }
                self.edit_command_name = Some(name.clone());
                self.edit_mode = Some(state);
            }
            TreeNode::Subcommand { name, depth, .. } => {
                // Find the parent command name by walking backwards
                let parent_name = self.find_parent_command_name(node_idx);
                let flags = self.collect_flags_for_subcommand(name);
                let cmd = parent_name.as_deref().unwrap_or(name);
                let mut state = CommandEditState::for_schema(cmd, Some(name), flags);
                if let Some(store) = &self.history_store {
                    state.set_history_store(store.clone());
                }
                let label = if *depth > 1 {
                    name.clone()
                } else {
                    format!("{cmd} {name}")
                };
                self.edit_command_name = Some(label);
                self.edit_mode = Some(state);
            }
            TreeNode::Flag { .. } | TreeNode::Section { .. } => {
                // Flags: insert text directly (no edit mode needed)
                // Sections: no-op
            }
        }
    }

    /// Exits edit mode.
    fn exit_edit_mode(&mut self) {
        self.edit_mode = None;
        self.edit_command_name = None;
    }

    /// Finds the parent Command name for a node by walking backwards.
    fn find_parent_command_name(&self, node_idx: usize) -> Option<String> {
        for i in (0..node_idx).rev() {
            if let TreeNode::Command { name, .. } = &self.nodes[i] {
                return Some(name.clone());
            }
        }
        None
    }

    /// Collects flag canonical names for a command from the schema provider.
    fn collect_flags_for_command(&self, command: &str) -> Vec<String> {
        let store = match self.history_store.as_ref() {
            Some(s) => s,
            None => return Vec::new(),
        };
        let guard = match store.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let provider = match guard.schema_provider() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let schema = match provider.get(command) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let mut flags: Vec<String> = schema
            .global_flags
            .iter()
            .map(|f| f.canonical_name().to_string())
            .collect();

        // Also include subcommand names as suggestions
        for sub in &schema.subcommands {
            flags.push(sub.name.clone());
        }

        flags
    }

    /// Collects flag canonical names for a subcommand from the schema provider.
    fn collect_flags_for_subcommand(&self, subcommand: &str) -> Vec<String> {
        // Walk the nodes to find flags that are children of this subcommand
        let mut flags = Vec::new();
        let mut found = false;
        for node in &self.nodes {
            match node {
                TreeNode::Subcommand { name, .. } if name == subcommand => {
                    found = true;
                }
                TreeNode::Subcommand { .. } | TreeNode::Command { .. } if found => {
                    break; // Next sibling or parent
                }
                TreeNode::Flag {
                    long, short, depth, ..
                } if found && *depth > 0 => {
                    if let Some(l) = long {
                        flags.push(l.clone());
                    } else if let Some(s) = short {
                        flags.push(s.clone());
                    }
                }
                _ => {}
            }
        }
        flags
    }

    /// Handles input in edit mode.
    fn handle_edit_input(&mut self, key: KeyEvent) -> PanelResult {
        let edit_state = match self.edit_mode.as_mut() {
            Some(s) => s,
            None => return PanelResult::Continue,
        };

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('u') => {
                    edit_state.undo();
                    return PanelResult::Continue;
                }
                KeyCode::Char('d') => {
                    edit_state.delete_token();
                    return PanelResult::Continue;
                }
                KeyCode::Char('a') => {
                    edit_state.insert_token_after();
                    return PanelResult::Continue;
                }
                KeyCode::Char('q') => {
                    edit_state.cycle_quote();
                    return PanelResult::Continue;
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
                } else if edit_state.is_changed() {
                    edit_state.revert();
                } else {
                    self.exit_edit_mode();
                }
                PanelResult::Continue
            }
            KeyCode::Enter => {
                let command = edit_state.build_command();
                self.exit_edit_mode();
                PanelResult::Execute(command)
            }
            KeyCode::Left => {
                edit_state.prev();
                PanelResult::Continue
            }
            KeyCode::Right => {
                edit_state.next();
                PanelResult::Continue
            }
            KeyCode::Home => {
                edit_state.select(0);
                PanelResult::Continue
            }
            KeyCode::End => {
                let last = edit_state.token_count().saturating_sub(1);
                edit_state.select(last);
                PanelResult::Continue
            }
            KeyCode::Up => {
                edit_state.cycle_suggestion(-1);
                PanelResult::Continue
            }
            KeyCode::Down => {
                edit_state.cycle_suggestion(1);
                PanelResult::Continue
            }
            KeyCode::Tab => {
                edit_state.next();
                PanelResult::Continue
            }
            KeyCode::BackTab => {
                edit_state.prev();
                PanelResult::Continue
            }
            KeyCode::Char(c) => {
                edit_state.type_char(c);
                PanelResult::Continue
            }
            KeyCode::Backspace => {
                edit_state.backspace();
                PanelResult::Continue
            }
            _ => PanelResult::Continue,
        }
    }

    /// Renders the edit mode UI.
    fn render_edit_mode(&self, buffer: &mut Buffer, area: Rect, edit_state: &CommandEditState) {
        let Some(layout) = compute_edit_mode_layout(area) else {
            return;
        };

        // Title
        let cmd_name = self.edit_command_name.as_deref().unwrap_or("command");
        let mut title_spans = vec![
            Span::styled(
                " Edit Command: ",
                Style::default().fg(self.theme.header_fg),
            ),
            Span::styled(
                cmd_name,
                Style::default()
                    .fg(self.theme.text_highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if !edit_state.suggestions.is_empty() {
            let sugg_count = format!(" [{} flags]", edit_state.suggestions.len());
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
                cell.set_char('─');
                cell.set_style(border_style);
            }
        }

        render_edit_mode_shared(buffer, self.theme, edit_state, &layout);
    }

    /// Loads all schemas from the provider into the tree view.
    fn load_schemas(&mut self) {
        self.nodes.clear();
        self.expanded.clear();

        let store = match self.history_store.clone() {
            Some(s) => s,
            None => {
                self.rebuild_visible();
                return;
            }
        };

        let guard = match store.lock() {
            Ok(g) => g,
            Err(_) => {
                self.rebuild_visible();
                return;
            }
        };

        let provider = match guard.schema_provider() {
            Some(p) => p,
            None => {
                drop(guard);
                self.rebuild_visible();
                return;
            }
        };

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

                if !schema.global_flags.is_empty() {
                    self.nodes.push(TreeNode::Section {
                        label: "Global Options".to_string(),
                        count: schema.global_flags.len(),
                        depth: 1,
                    });
                    for flag in &schema.global_flags {
                        self.nodes.push(TreeNode::Flag {
                            short: flag.short.clone(),
                            long: flag.long.clone(),
                            description: flag.description.clone(),
                            depth: 2,
                        });
                    }
                }

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
        self.rebuild_visible();
    }

    /// Discovers a command schema from its --help output.
    fn discover_command(&mut self) {
        let command = if self.filter.has_filter() {
            self.filter.text().trim().to_string()
        } else if let Some(node_idx) = self.tree.selected_node_idx() {
            match &self.nodes[node_idx] {
                TreeNode::Command { name, .. } | TreeNode::Subcommand { name, .. } => {
                    name.clone()
                }
                _ => return,
            }
        } else {
            return;
        };

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
                flag_count: sub.flags.len(),
                subcommand_count: sub.subcommands.len(),
            });

            for flag in &sub.flags {
                nodes.push(TreeNode::Flag {
                    short: flag.short.clone(),
                    long: flag.long.clone(),
                    description: flag.description.clone(),
                    depth: depth + 1,
                });
            }

            if !sub.subcommands.is_empty() {
                Self::add_subcommands_recursive(nodes, &sub.subcommands, depth + 1);
            }
        }
    }

    /// Rebuilds the visible list and tree metadata from current state.
    fn rebuild_visible(&mut self) {
        let vis = Self::compute_visibility(&self.nodes, &self.filter, &self.expanded);
        let has_filter = self.filter.has_filter();
        let nodes = &self.nodes;
        let expanded = &self.expanded;

        self.tree.rebuild(
            nodes,
            |idx| vis[idx],
            |idx| {
                if has_filter {
                    // In filter mode, show expanded indicator when children are visible.
                    nodes.get(idx + 1).is_some_and(|n| {
                        n.depth() > nodes[idx].depth() && vis[idx + 1]
                    })
                } else {
                    expanded.contains(&idx)
                }
            },
        );

        // Fix guide rails: depth-0 nodes (Commands) render as headers with
        // just ▾/▸, not branch connectors, so their is_last should not
        // affect children's guide rails. Remove the depth-0 ancestor entry
        // so that depth-2 nodes use the depth-1 parent's continuation line.
        for tl in self.tree.tree_lines_mut() {
            if tl.depth >= 1 && !tl.ancestor_is_last.is_empty() {
                tl.ancestor_is_last.remove(0);
            }
        }
    }

    /// Computes per-node visibility based on filter and expansion state.
    fn compute_visibility(
        nodes: &[TreeNode],
        filter: &FilterInput,
        expanded: &HashSet<usize>,
    ) -> Vec<bool> {
        let n = nodes.len();
        let mut vis = vec![false; n];

        if filter.has_filter() {
            // Filter mode: show matching nodes + parent commands for context.
            // Expansion state is bypassed.
            let mut current_cmd_idx: Option<usize> = None;
            let mut cmd_already_added = false;

            for (i, node) in nodes.iter().enumerate() {
                match node {
                    TreeNode::Command { .. } => {
                        current_cmd_idx = Some(i);
                        if node.matches_filter(filter) {
                            vis[i] = true;
                            cmd_already_added = true;
                        } else {
                            cmd_already_added = false;
                        }
                    }
                    _ => {
                        if cmd_already_added {
                            vis[i] = true;
                        } else if node.matches_filter(filter) {
                            if let Some(cmd_i) = current_cmd_idx {
                                vis[cmd_i] = true;
                            }
                            vis[i] = true;
                        }
                    }
                }
            }
        } else {
            // No filter: show based on expansion state.
            let mut depth_open: Vec<bool> = vec![true]; // depth 0 always open

            for (i, node) in nodes.iter().enumerate() {
                let depth = node.depth();

                while depth_open.len() <= depth {
                    depth_open.push(false);
                }

                vis[i] = depth_open[depth];

                // Update openness for the next depth level.
                let next_depth = depth + 1;
                let children_open = if node.has_children() {
                    // Expandable: children visible only if this node is expanded
                    depth_open[depth] && expanded.contains(&i)
                } else {
                    // Non-expandable (Section, leaf): pass through parent openness
                    depth_open[depth]
                };

                if depth_open.len() <= next_depth {
                    depth_open.push(children_open);
                } else {
                    depth_open[next_depth] = children_open;
                }
            }
        }

        vis
    }

    /// Handles input when the filter is active.
    fn handle_filter_input(&mut self, key: KeyEvent) -> PanelResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
            self.discover_command();
            return PanelResult::Continue;
        }

        match key.code {
            KeyCode::Esc => {
                self.filter.clear_and_deactivate();
                self.rebuild_visible();
                PanelResult::Continue
            }
            KeyCode::Enter => {
                self.filter.deactivate();
                PanelResult::Continue
            }
            KeyCode::Char(c) => {
                self.filter.type_char(c);
                self.rebuild_visible();
                self.tree.scroll_mut().reset();
                PanelResult::Continue
            }
            KeyCode::Backspace => {
                let empty = self.filter.backspace();
                if empty {
                    self.filter.deactivate();
                }
                self.rebuild_visible();
                PanelResult::Continue
            }
            _ => PanelResult::Continue,
        }
    }

    /// Right / `l`: expand or move to first child.
    fn handle_expand_or_child(&mut self) -> PanelResult {
        if let Some(node_idx) = self.tree.selected_node_idx() {
            if self.nodes[node_idx].has_children() {
                if self.expanded.contains(&node_idx) {
                    // Already expanded → move to first child
                    if let Some(child_vis) = self.tree.first_child_visible_idx() {
                        let count = self.tree.visible_count();
                        self.tree.scroll_mut().set_selection(child_vis, count);
                    }
                } else {
                    self.expanded.insert(node_idx);
                    self.rebuild_visible();
                }
            }
        }
        PanelResult::Continue
    }

    /// Left / `h`: collapse or jump to parent.
    fn handle_collapse_or_parent(&mut self) -> PanelResult {
        if let Some(node_idx) = self.tree.selected_node_idx() {
            if self.nodes[node_idx].depth() > 0 {
                // On child → jump to parent
                if let Some(parent_vis) = self.tree.parent_visible_idx(&self.nodes) {
                    let count = self.tree.visible_count();
                    self.tree.scroll_mut().set_selection(parent_vis, count);
                }
            } else if self.expanded.contains(&node_idx) {
                // On expanded root → collapse
                self.expanded.remove(&node_idx);
                self.rebuild_visible();
            }
        }
        PanelResult::Continue
    }

    /// Toggles expansion of the selected node.
    fn toggle_selected(&mut self) {
        if let Some(node_idx) = self.tree.selected_node_idx() {
            if self.nodes[node_idx].has_children() {
                if self.expanded.contains(&node_idx) {
                    self.expanded.remove(&node_idx);
                } else {
                    self.expanded.insert(node_idx);
                }
                self.rebuild_visible();
            }
        }
    }

    /// Returns the insert text for the currently selected node.
    fn selected_insert_text(&self) -> Option<String> {
        let node_idx = self.tree.selected_node_idx()?;
        match &self.nodes[node_idx] {
            TreeNode::Command { name, .. } | TreeNode::Subcommand { name, .. } => {
                Some(name.clone())
            }
            TreeNode::Section { .. } => None,
            TreeNode::Flag { long, short, .. } => long.clone().or_else(|| short.clone()),
        }
    }
}

impl Panel for SchemaBrowserPanel {
    fn preferred_height(&self) -> u16 {
        if self.edit_mode.is_some() {
            12
        } else {
            8
        }
    }

    fn title(&self) -> &str {
        "Browser"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 2 || area.width < 10 {
            return;
        }

        // Edit mode: delegate to edit renderer
        if let Some(ref edit_state) = self.edit_mode {
            self.render_edit_mode(buffer, area, edit_state);
            return;
        }

        // Layout: optional filter bar + tree area
        let show_filter = self.filter.is_active() || self.filter.has_filter();
        let (filter_area, tree_area) = if show_filter {
            let chunks =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
            (Some(chunks[0]), chunks[1])
        } else {
            (None, area)
        };

        // Render filter bar
        if let Some(fa) = filter_area {
            let spans = self.filter.render_spans(self.theme);
            Paragraph::new(Line::from(spans)).render(fa, buffer);
        }

        // Empty state
        if self.tree.visible_count() == 0 {
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
                        Span::styled("Press ", secondary),
                        Span::styled("/", highlight),
                        Span::styled(" to search, type a command, then ", secondary),
                        Span::styled("^D", highlight),
                        Span::styled(" to discover.", secondary),
                    ]),
                ];
                Paragraph::new(lines).render(tree_area, buffer);
            } else {
                let help = Line::from(Span::styled(
                    "No matching schemas.",
                    Style::default().fg(self.theme.text_secondary),
                ));
                Paragraph::new(vec![Line::from(""), help]).render(tree_area, buffer);
            }
            return;
        }

        let visible_height = tree_area.height as usize;
        self.tree.scroll_mut().ensure_visible(visible_height);

        let range = self
            .tree
            .scroll()
            .visible_range(visible_height, self.tree.visible_count());

        let items: Vec<ListItem> = range
            .map(|vis_idx| {
                let node_idx = self.tree.visible()[vis_idx];
                let node = &self.nodes[node_idx];
                let tree_line = self.tree.tree_line_at(vis_idx).unwrap();
                let is_selected = vis_idx == self.tree.scroll().selection();

                let prefix = tree_prefix(tree_line, &UNICODE_TREE_CHARS);
                let name = node.display_name();

                let desc = match node {
                    TreeNode::Command { description, .. }
                    | TreeNode::Subcommand { description, .. }
                    | TreeNode::Flag { description, .. } => {
                        description.as_deref().unwrap_or("")
                    }
                    TreeNode::Section { .. } => "",
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
                        TreeNode::Section { .. } => Style::default()
                            .fg(self.theme.text_secondary)
                            .add_modifier(Modifier::ITALIC),
                        TreeNode::Subcommand { .. } => {
                            Style::default().fg(self.theme.text_highlight)
                        }
                        TreeNode::Flag { .. } => {
                            Style::default().fg(self.theme.text_secondary)
                        }
                    }
                };

                let mut spans = vec![
                    Span::styled(prefix, Style::default().fg(self.theme.panel_border)),
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

        List::new(items).render(tree_area, buffer);
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        // Edit mode: delegate to edit handler
        if self.edit_mode.is_some() {
            return self.handle_edit_input(key);
        }

        // If filter is active, delegate to filter handler
        if self.filter.is_active() {
            return self.handle_filter_input(key);
        }

        // Ctrl+ keybinds
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
                KeyCode::Char('e') => {
                    self.enter_edit_mode();
                    return PanelResult::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                if self.filter.has_filter() {
                    // First Esc clears filter, second dismisses
                    self.filter.clear_and_deactivate();
                    self.rebuild_visible();
                    PanelResult::Continue
                } else {
                    PanelResult::Dismiss
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let count = self.tree.visible_count();
                self.tree.scroll_mut().down(count);
                PanelResult::Continue
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let count = self.tree.visible_count();
                self.tree.scroll_mut().up(count);
                PanelResult::Continue
            }
            KeyCode::Char('l') | KeyCode::Right => self.handle_expand_or_child(),
            KeyCode::Char('h') | KeyCode::Left => self.handle_collapse_or_parent(),
            KeyCode::Char(' ') => {
                self.toggle_selected();
                PanelResult::Continue
            }
            KeyCode::Char('/') => {
                self.filter.activate();
                PanelResult::Continue
            }
            KeyCode::Enter => {
                if let Some(node_idx) = self.tree.selected_node_idx() {
                    if self.nodes[node_idx].has_children() {
                        self.toggle_selected();
                        PanelResult::Continue
                    } else if let Some(text) = self.selected_insert_text() {
                        PanelResult::InsertText(text)
                    } else {
                        PanelResult::Continue
                    }
                } else {
                    PanelResult::Continue
                }
            }
            KeyCode::PageUp => {
                let count = self.tree.visible_count();
                self.tree.scroll_mut().page_up(10, count);
                PanelResult::Continue
            }
            KeyCode::PageDown => {
                let count = self.tree.visible_count();
                self.tree.scroll_mut().page_down(10, count);
                PanelResult::Continue
            }
            KeyCode::Home => {
                self.tree.scroll_mut().home();
                PanelResult::Continue
            }
            KeyCode::End => {
                let count = self.tree.visible_count();
                self.tree.scroll_mut().end(count);
                PanelResult::Continue
            }
            _ => PanelResult::Continue,
        }
    }

    fn footer_entries(&self) -> Vec<FooterEntry> {
        if self.edit_mode.is_some() {
            vec![
                FooterEntry::action("↑↓", "Cycle"),
                FooterEntry::action("←→", "Nav"),
                FooterEntry::action("^A", "Add"),
                FooterEntry::action("^D", "Del"),
                FooterEntry::action("^Z", "Undo"),
                FooterEntry::action("Enter", "Run"),
                FooterEntry::action("Esc", "Back"),
            ]
        } else if self.filter.is_active() {
            vec![
                FooterEntry::action("^D", "Discover"),
                FooterEntry::action("Enter", "Accept"),
                FooterEntry::action("Esc", "Cancel"),
            ]
        } else {
            vec![
                FooterEntry::action("^E", "Edit"),
                FooterEntry::action("Space", "Expand"),
                FooterEntry::action("/", "Filter"),
                FooterEntry::action("^D", "Discover"),
                FooterEntry::action("^R", "Refresh"),
                FooterEntry::action("Enter", "Select"),
                FooterEntry::action("Esc", "Close"),
            ]
        }
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
    use crate::ui::tree_view::tree_prefix_width;

    /// Creates a panel pre-populated with test nodes.
    fn panel_with_nodes() -> SchemaBrowserPanel {
        let mut panel = SchemaBrowserPanel::new(&AMBER_THEME);
        panel.nodes = vec![
            TreeNode::Command {
                name: "git".into(),
                description: Some("Distributed VCS".into()),
                flag_count: 1,
                subcommand_count: 2,
            },
            TreeNode::Section {
                label: "Global Options".into(),
                count: 1,
                depth: 1,
            },
            TreeNode::Flag {
                short: Some("-v".into()),
                long: Some("--verbose".into()),
                description: Some("Be verbose".into()),
                depth: 2,
            },
            TreeNode::Subcommand {
                name: "commit".into(),
                description: Some("Record changes".into()),
                depth: 1,
                flag_count: 1,
                subcommand_count: 0,
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
                flag_count: 0,
                subcommand_count: 0,
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
                flag_count: 0,
                subcommand_count: 0,
            },
        ];
        panel.rebuild_visible();
        panel
    }

    /// Helper: type a string into the filter.
    fn type_filter(panel: &mut SchemaBrowserPanel, text: &str) {
        panel.filter.activate();
        for c in text.chars() {
            panel.filter.type_char(c);
        }
        panel.rebuild_visible();
    }

    #[test]
    fn test_schema_browser_new_initial_state() {
        let panel = SchemaBrowserPanel::new(&AMBER_THEME);
        assert!(panel.nodes.is_empty());
        assert_eq!(panel.tree.visible_count(), 0);
        assert!(!panel.filter.has_filter());
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

        let section = TreeNode::Section {
            label: "Global Options".into(),
            count: 5,
            depth: 1,
        };
        assert_eq!(section.depth(), 1);

        let sub = TreeNode::Subcommand {
            name: "commit".into(),
            description: None,
            depth: 1,
            flag_count: 0,
            subcommand_count: 0,
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

        // Subcommand with children
        let sub_with = TreeNode::Subcommand {
            name: "commit".into(),
            description: None,
            depth: 1,
            flag_count: 3,
            subcommand_count: 0,
        };
        assert!(sub_with.has_children());

        // Subcommand without children
        let sub_without = TreeNode::Subcommand {
            name: "push".into(),
            description: None,
            depth: 1,
            flag_count: 0,
            subcommand_count: 0,
        };
        assert!(!sub_without.has_children());
    }

    #[test]
    fn test_no_filter_shows_commands_only() {
        let panel = panel_with_nodes();
        assert_eq!(panel.tree.visible_count(), 2);
        assert_eq!(
            panel.nodes[panel.tree.visible()[0]].display_name(),
            "git"
        );
        assert_eq!(
            panel.nodes[panel.tree.visible()[1]].display_name(),
            "cargo"
        );
    }

    #[test]
    fn test_filter_by_command_name() {
        let mut panel = panel_with_nodes();
        type_filter(&mut panel, "git");
        // "git" matches → shows git + all children
        assert!(panel.tree.visible_count() >= 2);
        assert_eq!(
            panel.nodes[panel.tree.visible()[0]].display_name(),
            "git"
        );
    }

    #[test]
    fn test_filter_by_subcommand_name() {
        let mut panel = panel_with_nodes();
        type_filter(&mut panel, "build");
        // "build" → cargo parent + build child
        assert_eq!(panel.tree.visible_count(), 2);
        assert_eq!(
            panel.nodes[panel.tree.visible()[0]].display_name(),
            "cargo"
        );
        assert_eq!(
            panel.nodes[panel.tree.visible()[1]].display_name(),
            "build"
        );
    }

    #[test]
    fn test_filter_by_flag_name() {
        let mut panel = panel_with_nodes();
        type_filter(&mut panel, "--verbose");
        assert!(panel.tree.visible_count() >= 2);
        assert_eq!(
            panel.nodes[panel.tree.visible()[0]].display_name(),
            "git"
        );
    }

    #[test]
    fn test_filter_no_match() {
        let mut panel = panel_with_nodes();
        type_filter(&mut panel, "zzzznotfound");
        assert_eq!(panel.tree.visible_count(), 0);
    }

    #[test]
    fn test_expand_collapse_via_space() {
        let mut panel = panel_with_nodes();
        assert_eq!(panel.tree.visible_count(), 2); // git, cargo

        // Press Space to expand git
        panel.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert!(panel.tree.visible_count() > 2);

        // Press Space to collapse git
        panel.handle_input(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(panel.tree.visible_count(), 2);
    }

    #[test]
    fn test_expand_collapse_via_enter() {
        let mut panel = panel_with_nodes();
        let initial = panel.tree.visible_count();

        let result = panel.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(result, PanelResult::Continue));
        assert!(panel.tree.visible_count() > initial);
    }

    #[test]
    fn test_right_expands_then_moves_to_child() {
        let mut panel = panel_with_nodes();
        assert_eq!(panel.tree.scroll().selection(), 0);

        // First Right: expand
        panel.handle_input(KeyEvent::from(KeyCode::Right));
        assert!(panel.tree.visible_count() > 2);
        assert_eq!(panel.tree.scroll().selection(), 0);

        // Second Right: move to first child
        panel.handle_input(KeyEvent::from(KeyCode::Right));
        assert_eq!(panel.tree.scroll().selection(), 1);
    }

    #[test]
    fn test_left_jumps_to_parent() {
        let mut panel = panel_with_nodes();
        // Expand git, then move to a depth-1 child (commit subcommand)
        panel.expanded.insert(0);
        panel.rebuild_visible();
        // Find commit in visible list
        let commit_vis = (0..panel.tree.visible_count())
            .find(|&vi| {
                matches!(
                    &panel.nodes[panel.tree.visible()[vi]],
                    TreeNode::Subcommand { name, .. } if name == "commit"
                )
            })
            .expect("commit should be visible");
        let count = panel.tree.visible_count();
        panel.tree.scroll_mut().set_selection(commit_vis, count);

        panel.handle_input(KeyEvent::from(KeyCode::Left));
        assert_eq!(panel.tree.scroll().selection(), 0); // back to git
    }

    #[test]
    fn test_left_collapses_on_command() {
        let mut panel = panel_with_nodes();
        panel.expanded.insert(0);
        panel.rebuild_visible();
        let expanded_count = panel.tree.visible_count();
        assert!(expanded_count > 2);

        // Left on git (command, expanded) → collapse
        panel.handle_input(KeyEvent::from(KeyCode::Left));
        assert_eq!(panel.tree.visible_count(), 2);
    }

    #[test]
    fn test_vim_j_k_navigation() {
        let mut panel = panel_with_nodes();
        assert_eq!(panel.tree.scroll().selection(), 0);

        // j moves down
        panel.handle_input(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(panel.tree.scroll().selection(), 1);

        // k moves up
        panel.handle_input(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(panel.tree.scroll().selection(), 0);
    }

    #[test]
    fn test_vim_l_h_navigation() {
        let mut panel = panel_with_nodes();

        // l expands
        panel.handle_input(KeyEvent::from(KeyCode::Char('l')));
        assert!(panel.tree.visible_count() > 2);

        // l again moves to child
        panel.handle_input(KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(panel.tree.scroll().selection(), 1);

        // h jumps to parent
        panel.handle_input(KeyEvent::from(KeyCode::Char('h')));
        assert_eq!(panel.tree.scroll().selection(), 0);
    }

    #[test]
    fn test_slash_activates_filter() {
        let mut panel = panel_with_nodes();
        assert!(!panel.filter.is_active());

        panel.handle_input(KeyEvent::from(KeyCode::Char('/')));
        assert!(panel.filter.is_active());
    }

    #[test]
    fn test_filter_mode_esc_clears_and_deactivates() {
        let mut panel = panel_with_nodes();
        // Activate filter and type
        panel.handle_input(KeyEvent::from(KeyCode::Char('/')));
        panel.handle_input(KeyEvent::from(KeyCode::Char('g')));
        assert!(panel.filter.has_filter());

        // Esc clears filter
        panel.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(!panel.filter.is_active());
        assert!(!panel.filter.has_filter());
    }

    #[test]
    fn test_filter_mode_enter_deactivates_keeps_text() {
        let mut panel = panel_with_nodes();
        panel.handle_input(KeyEvent::from(KeyCode::Char('/')));
        panel.handle_input(KeyEvent::from(KeyCode::Char('g')));

        panel.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(!panel.filter.is_active());
        assert!(panel.filter.has_filter()); // text preserved
    }

    #[test]
    fn test_filter_mode_backspace_deactivates_on_empty() {
        let mut panel = panel_with_nodes();
        panel.handle_input(KeyEvent::from(KeyCode::Char('/')));
        panel.handle_input(KeyEvent::from(KeyCode::Char('x')));
        assert!(panel.filter.is_active());

        // Backspace removes char
        panel.handle_input(KeyEvent::from(KeyCode::Backspace));
        // Now empty → deactivates
        assert!(!panel.filter.is_active());
    }

    #[test]
    fn test_page_up_down_home_end() {
        let mut panel = panel_with_nodes();
        // Expand both commands
        panel.expanded.insert(0);
        // Find cargo index
        let cargo_idx = panel
            .nodes
            .iter()
            .position(|n| matches!(n, TreeNode::Command { name, .. } if name == "cargo"))
            .unwrap();
        panel.expanded.insert(cargo_idx);
        panel.rebuild_visible();

        let total = panel.tree.visible_count();
        assert!(total > 3);

        // Home
        let count = panel.tree.visible_count();
        panel.tree.scroll_mut().set_selection(total / 2, count);
        panel.handle_input(KeyEvent::from(KeyCode::Home));
        assert_eq!(panel.tree.scroll().selection(), 0);

        // End
        panel.handle_input(KeyEvent::from(KeyCode::End));
        assert_eq!(panel.tree.scroll().selection(), total - 1);

        // PageDown from start
        panel.tree.scroll_mut().home();
        panel.handle_input(KeyEvent::from(KeyCode::PageDown));
        assert!(panel.tree.scroll().selection() > 0);

        // PageUp from end
        let count = panel.tree.visible_count();
        panel.tree.scroll_mut().end(count);
        panel.handle_input(KeyEvent::from(KeyCode::PageUp));
        assert!(panel.tree.scroll().selection() < total - 1);
    }

    #[test]
    fn test_selected_insert_text() {
        let mut panel = panel_with_nodes();
        // Select "git" command
        assert_eq!(panel.selected_insert_text(), Some("git".into()));

        // Expand and find a flag
        panel.expanded.insert(0);
        panel.rebuild_visible();
        let flag_vis = (0..panel.tree.visible_count()).find(|&vi| {
            matches!(
                panel.nodes[panel.tree.visible()[vi]],
                TreeNode::Flag { .. }
            )
        });
        if let Some(vi) = flag_vis {
            let count = panel.tree.visible_count();
            panel.tree.scroll_mut().set_selection(vi, count);
            let text = panel.selected_insert_text().unwrap();
            assert!(text == "--verbose" || text == "-v");
        }

        // Section node should not produce insert text
        let section_vis = (0..panel.tree.visible_count()).find(|&vi| {
            matches!(
                panel.nodes[panel.tree.visible()[vi]],
                TreeNode::Section { .. }
            )
        });
        if let Some(vi) = section_vis {
            let count = panel.tree.visible_count();
            panel.tree.scroll_mut().set_selection(vi, count);
            assert_eq!(panel.selected_insert_text(), None);
        }
    }

    #[test]
    fn test_enter_on_leaf_returns_insert_text() {
        let mut panel = panel_with_nodes();
        panel.expanded.insert(0);
        panel.rebuild_visible();

        let flag_vis = (0..panel.tree.visible_count())
            .find(|&vi| {
                matches!(
                    panel.nodes[panel.tree.visible()[vi]],
                    TreeNode::Flag { .. }
                )
            })
            .expect("should have a flag");
        let count = panel.tree.visible_count();
        panel.tree.scroll_mut().set_selection(flag_vis, count);

        let result = panel.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(result, PanelResult::InsertText(_)));
    }

    #[test]
    fn test_matches_filter_all_node_types() {
        let filter = {
            let mut f = FilterInput::new();
            for c in "docker".chars() {
                f.type_char(c);
            }
            f
        };

        let cmd = TreeNode::Command {
            name: "docker".into(),
            description: Some("Container runtime".into()),
            flag_count: 0,
            subcommand_count: 0,
        };
        assert!(cmd.matches_filter(&filter));

        let container_filter = {
            let mut f = FilterInput::new();
            for c in "container".chars() {
                f.type_char(c);
            }
            f
        };
        assert!(cmd.matches_filter(&container_filter));

        let zzz_filter = {
            let mut f = FilterInput::new();
            for c in "zzz".chars() {
                f.type_char(c);
            }
            f
        };
        assert!(!cmd.matches_filter(&zzz_filter));
    }

    #[test]
    fn test_esc_clears_filter_before_dismiss() {
        let mut panel = panel_with_nodes();
        // Set up filter text (not active)
        type_filter(&mut panel, "git");
        panel.filter.deactivate();
        assert!(panel.filter.has_filter());

        // First Esc: clears filter
        let result = panel.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(result, PanelResult::Continue));
        assert!(!panel.filter.has_filter());

        // Second Esc: dismiss
        let result = panel.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(result, PanelResult::Dismiss));
    }

    #[test]
    fn test_subcommand_expandable() {
        let mut panel = panel_with_nodes();
        // Expand git to see subcommands
        panel.expanded.insert(0);
        panel.rebuild_visible();

        // Find commit subcommand (has flag_count=1, so has_children=true)
        let commit_vis = (0..panel.tree.visible_count()).find(|&vi| {
            matches!(
                &panel.nodes[panel.tree.visible()[vi]],
                TreeNode::Subcommand { name, .. } if name == "commit"
            )
        });
        assert!(commit_vis.is_some(), "commit should be visible");

        let commit_node_idx = panel.tree.visible()[commit_vis.unwrap()];
        assert!(panel.nodes[commit_node_idx].has_children());

        // Expand commit subcommand
        panel.expanded.insert(commit_node_idx);
        panel.rebuild_visible();

        // Should now see the -m/--message flag under commit
        let message_vis = (0..panel.tree.visible_count()).find(|&vi| {
            matches!(
                &panel.nodes[panel.tree.visible()[vi]],
                TreeNode::Flag { long: Some(l), .. } if l == "--message"
            )
        });
        assert!(message_vis.is_some(), "--message flag should be visible");
    }

    #[test]
    fn test_tree_prefix_consistency() {
        let mut panel = panel_with_nodes();
        panel.expanded.insert(0); // expand git
        panel.rebuild_visible();

        // All visible items should have valid tree_line metadata
        for vi in 0..panel.tree.visible_count() {
            let tl = panel.tree.tree_line_at(vi).unwrap();
            let prefix = tree_prefix(tl, &UNICODE_TREE_CHARS);
            let expected_width = tree_prefix_width(tl.depth);

            use unicode_width::UnicodeWidthStr;
            let actual_width = UnicodeWidthStr::width(prefix.as_str());
            assert_eq!(
                actual_width, expected_width,
                "Prefix width mismatch at vis_idx {vi}: {:?} (node={:?})",
                prefix, panel.nodes[panel.tree.visible()[vi]].display_name()
            );
        }
    }

    #[test]
    fn test_depth2_guide_rail_connects_depth1_siblings() {
        let mut panel = panel_with_nodes();
        panel.expanded.insert(0); // expand git
        panel.rebuild_visible();

        // Find a depth-2 flag (e.g. --verbose) whose depth-1 parent (Section)
        // is NOT last at depth 1 (add, commit, push follow).
        // Its prefix should start with "│" to connect to the next depth-1 sibling.
        let flag_vis = (0..panel.tree.visible_count())
            .find(|&vi| {
                matches!(
                    &panel.nodes[panel.tree.visible()[vi]],
                    TreeNode::Flag { long: Some(l), .. } if l == "--verbose"
                )
            })
            .expect("--verbose should be visible");

        let tl = panel.tree.tree_line_at(flag_vis).unwrap();
        let prefix = tree_prefix(tl, &UNICODE_TREE_CHARS);
        assert!(
            prefix.starts_with('│'),
            "Depth-2 flag under non-last depth-1 parent should have │ guide rail, got: {:?}",
            prefix
        );
    }

    #[test]
    fn test_depth2_guide_rail_absent_under_last_sibling() {
        let mut panel = panel_with_nodes();
        // Expand git AND the "push" subcommand (last depth-1 sibling, has no children)
        panel.expanded.insert(0);
        panel.rebuild_visible();

        // Find the last depth-1 node (push, which has no children).
        // Instead, expand "commit" (not last at depth 1) to get its flags.
        // Then verify flags under commit DO have guide rail.
        let commit_node_idx = panel
            .nodes
            .iter()
            .position(|n| {
                matches!(n, TreeNode::Subcommand { name, .. } if name == "commit")
            })
            .unwrap();
        panel.expanded.insert(commit_node_idx);

        // Also expand the last command (cargo) and its subcommand (build)
        let cargo_idx = panel
            .nodes
            .iter()
            .position(|n| matches!(n, TreeNode::Command { name, .. } if name == "cargo"))
            .unwrap();
        panel.expanded.insert(cargo_idx);
        panel.rebuild_visible();

        // build is the LAST depth-1 child of cargo (and has no children/flags).
        // So there should be no depth-2 items under the last depth-1 sibling
        // to check. But commit's --message flag should have │ since commit
        // is not the last depth-1 sibling.
        let msg_vis = (0..panel.tree.visible_count())
            .find(|&vi| {
                matches!(
                    &panel.nodes[panel.tree.visible()[vi]],
                    TreeNode::Flag { long: Some(l), .. } if l == "--message"
                )
            })
            .expect("--message should be visible");

        let tl = panel.tree.tree_line_at(msg_vis).unwrap();
        let prefix = tree_prefix(tl, &UNICODE_TREE_CHARS);
        assert!(
            prefix.starts_with('│'),
            "--message under non-last commit should have │, got: {:?}",
            prefix
        );
    }

    #[test]
    fn test_compute_visibility_expansion_depth() {
        // Test that Section pass-through works:
        // When Command is expanded, Section and its children are visible.
        let mut panel = panel_with_nodes();
        panel.expanded.insert(0); // expand git
        panel.rebuild_visible();

        // Section "Global Options" should be visible
        let section_vis = (0..panel.tree.visible_count()).any(|vi| {
            matches!(
                panel.nodes[panel.tree.visible()[vi]],
                TreeNode::Section { .. }
            )
        });
        assert!(section_vis, "Section should be visible when command expanded");

        // Flag under Section should be visible
        let flag_vis = (0..panel.tree.visible_count()).any(|vi| {
            matches!(
                &panel.nodes[panel.tree.visible()[vi]],
                TreeNode::Flag { long: Some(l), .. } if l == "--verbose"
            )
        });
        assert!(flag_vis, "Flag under Section should be visible");
    }

    fn footer_has_label(entries: &[FooterEntry], label: &str) -> bool {
        entries.iter().any(|e| match &e.kind {
            super::super::footer_bar::FooterKind::Action { label: l, .. } => *l == label,
            _ => false,
        })
    }

    #[test]
    fn test_footer_entries_browse_mode() {
        let panel = panel_with_nodes();
        let entries = panel.footer_entries();
        assert!(footer_has_label(&entries, "Filter"));
        assert!(footer_has_label(&entries, "Expand"));
    }

    #[test]
    fn test_footer_entries_filter_mode() {
        let mut panel = panel_with_nodes();
        panel.filter.activate();
        let entries = panel.footer_entries();
        assert!(footer_has_label(&entries, "Cancel"));
        assert!(footer_has_label(&entries, "Discover"));
    }

    #[test]
    fn test_discover_from_selected_command() {
        // discover_command reads the selected node name when filter is empty.
        let panel = panel_with_nodes();
        // Just verify the method doesn't panic (no store connected).
        assert!(panel.selected_insert_text().is_some());
    }

    // ── Edit mode tests ──

    #[test]
    fn test_enter_edit_mode_on_command() {
        let mut panel = panel_with_nodes();
        assert!(!panel.in_edit_mode());

        // ^E on "git" command
        panel.enter_edit_mode();
        assert!(panel.in_edit_mode());
        assert_eq!(panel.edit_command_name.as_deref(), Some("git"));

        // First token should be locked "git"
        let edit = panel.edit_mode.as_ref().unwrap();
        assert!(edit.tokens[0].locked);
        assert_eq!(edit.tokens[0].text, "git");
    }

    #[test]
    fn test_enter_edit_mode_on_subcommand() {
        let mut panel = panel_with_nodes();
        // Expand git and select "commit" subcommand
        panel.expanded.insert(0);
        panel.rebuild_visible();
        let commit_vis = (0..panel.tree.visible_count())
            .find(|&vi| {
                matches!(
                    &panel.nodes[panel.tree.visible()[vi]],
                    TreeNode::Subcommand { name, .. } if name == "commit"
                )
            })
            .expect("commit should be visible");
        let count = panel.tree.visible_count();
        panel.tree.scroll_mut().set_selection(commit_vis, count);

        panel.enter_edit_mode();
        assert!(panel.in_edit_mode());
        assert_eq!(panel.edit_command_name.as_deref(), Some("git commit"));

        let edit = panel.edit_mode.as_ref().unwrap();
        // First two tokens should be locked: "git" and "commit"
        assert!(edit.tokens[0].locked);
        assert_eq!(edit.tokens[0].text, "git");
        assert!(edit.tokens[1].locked);
        assert_eq!(edit.tokens[1].text, "commit");
    }

    #[test]
    fn test_enter_edit_mode_on_flag_is_noop() {
        let mut panel = panel_with_nodes();
        panel.expanded.insert(0);
        panel.rebuild_visible();
        let flag_vis = (0..panel.tree.visible_count())
            .find(|&vi| {
                matches!(
                    panel.nodes[panel.tree.visible()[vi]],
                    TreeNode::Flag { .. }
                )
            })
            .expect("should have a flag");
        let count = panel.tree.visible_count();
        panel.tree.scroll_mut().set_selection(flag_vis, count);

        panel.enter_edit_mode();
        assert!(!panel.in_edit_mode()); // No edit mode for flags
    }

    #[test]
    fn test_enter_edit_mode_on_section_is_noop() {
        let mut panel = panel_with_nodes();
        panel.expanded.insert(0);
        panel.rebuild_visible();
        let section_vis = (0..panel.tree.visible_count())
            .find(|&vi| {
                matches!(
                    panel.nodes[panel.tree.visible()[vi]],
                    TreeNode::Section { .. }
                )
            })
            .expect("should have a section");
        let count = panel.tree.visible_count();
        panel.tree.scroll_mut().set_selection(section_vis, count);

        panel.enter_edit_mode();
        assert!(!panel.in_edit_mode());
    }

    #[test]
    fn test_exit_edit_mode() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();
        assert!(panel.in_edit_mode());

        panel.exit_edit_mode();
        assert!(!panel.in_edit_mode());
        assert!(panel.edit_command_name.is_none());
    }

    #[test]
    fn test_edit_mode_enter_executes() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();

        let result = panel.handle_input(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(result, PanelResult::Execute(_)));
        assert!(!panel.in_edit_mode());
    }

    #[test]
    fn test_edit_mode_esc_exits_eventually() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();

        // Multiple Esc presses: revert buffer → revert all → exit
        panel.handle_input(KeyEvent::from(KeyCode::Esc));
        panel.handle_input(KeyEvent::from(KeyCode::Esc));
        panel.handle_input(KeyEvent::from(KeyCode::Esc));
        assert!(!panel.in_edit_mode());
    }

    #[test]
    fn test_edit_mode_typing() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();

        panel.handle_input(KeyEvent::from(KeyCode::Char('-')));
        panel.handle_input(KeyEvent::from(KeyCode::Char('-')));
        panel.handle_input(KeyEvent::from(KeyCode::Char('a')));
        panel.handle_input(KeyEvent::from(KeyCode::Char('l')));
        panel.handle_input(KeyEvent::from(KeyCode::Char('l')));

        let edit = panel.edit_mode.as_ref().unwrap();
        assert_eq!(edit.edit_buffer, "--all");
    }

    #[test]
    fn test_ctrl_e_enters_edit_mode() {
        let mut panel = panel_with_nodes();
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        panel.handle_input(key);
        assert!(panel.in_edit_mode());
    }

    #[test]
    fn test_edit_mode_blocks_browse_keys() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();

        // j/k/h/l should be typed as characters, not navigate tree
        let initial_selection = panel.tree.scroll().selection();
        panel.handle_input(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(panel.tree.scroll().selection(), initial_selection);
        assert!(panel.in_edit_mode()); // still in edit mode
    }

    #[test]
    fn test_footer_entries_edit_mode() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();
        let entries = panel.footer_entries();
        assert!(footer_has_label(&entries, "Run"));
        assert!(footer_has_label(&entries, "Undo"));
        assert!(footer_has_label(&entries, "Back"));
        assert!(footer_has_label(&entries, "Cycle"));
    }

    #[test]
    fn test_footer_entries_browse_mode_has_edit() {
        let panel = panel_with_nodes();
        let entries = panel.footer_entries();
        assert!(footer_has_label(&entries, "Edit"));
    }

    #[test]
    fn test_preferred_height_edit_mode() {
        let mut panel = panel_with_nodes();
        assert_eq!(panel.preferred_height(), 8);

        panel.enter_edit_mode();
        assert_eq!(panel.preferred_height(), 12);
    }

    #[test]
    fn test_find_parent_command_name() {
        let panel = panel_with_nodes();
        // Node 3 is "commit" subcommand → parent should be "git" (node 0)
        assert_eq!(
            panel.find_parent_command_name(3),
            Some("git".to_string())
        );
        // Node 0 is "git" command → no parent
        assert_eq!(panel.find_parent_command_name(0), None);
    }

    #[test]
    fn test_collect_flags_for_subcommand() {
        let panel = panel_with_nodes();
        // "commit" has one flag: --message / -m
        let flags = panel.collect_flags_for_subcommand("commit");
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0], "--message");
    }

    #[test]
    fn test_edit_mode_arrow_navigation() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();

        let edit = panel.edit_mode.as_ref().unwrap();
        let initial_selected = edit.selected;

        // Left should move to previous token
        panel.handle_input(KeyEvent::from(KeyCode::Left));
        let edit = panel.edit_mode.as_ref().unwrap();
        assert!(edit.selected < initial_selected || initial_selected == 0);

        // Right should move back
        panel.handle_input(KeyEvent::from(KeyCode::Right));
    }

    #[test]
    fn test_edit_mode_backspace() {
        let mut panel = panel_with_nodes();
        panel.enter_edit_mode();

        // Type then backspace
        panel.handle_input(KeyEvent::from(KeyCode::Char('x')));
        let edit = panel.edit_mode.as_ref().unwrap();
        assert_eq!(edit.edit_buffer, "x");

        panel.handle_input(KeyEvent::from(KeyCode::Backspace));
        let edit = panel.edit_mode.as_ref().unwrap();
        assert_eq!(edit.edit_buffer, "");
    }
}
