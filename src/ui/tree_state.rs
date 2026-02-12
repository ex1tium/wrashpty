//! Generic tree viewport manager for flat pre-order node lists.
//!
//! Combines [`ScrollableList`] for selection/scroll management with
//! [`TreeLine`] metadata computation for tree-prefix rendering. Does NOT
//! own the backing data or expansion state — those stay with the caller,
//! which provides them via closures to [`TreeViewState::rebuild`].

use super::scrollable_list::ScrollableList;
use super::tree_view::TreeLine;

/// Trait for items in a flat pre-order tree structure.
///
/// Implementors provide depth and child information so that
/// `TreeViewState` can compute tree metadata generically.
pub trait TreeItem {
    /// Nesting depth (0 = root level).
    fn depth(&self) -> usize;
    /// Whether this node can be expanded (has children in the full tree).
    fn has_children(&self) -> bool;
}

/// Manages visibility, tree-line metadata, and scroll state for a
/// flattened tree displayed in a TUI panel.
///
/// The caller owns the node list and expansion state, and provides
/// them to [`rebuild`](Self::rebuild) via closures whenever either changes.
pub struct TreeViewState {
    /// Indices into the backing node list for currently visible items.
    visible: Vec<usize>,
    /// `TreeLine` metadata parallel to `visible` (for `tree_prefix` rendering).
    tree_lines: Vec<TreeLine>,
    /// Selection and scroll management.
    scroll: ScrollableList,
}

impl TreeViewState {
    /// Creates a new empty tree view state.
    pub fn new() -> Self {
        Self {
            visible: Vec::new(),
            tree_lines: Vec::new(),
            scroll: ScrollableList::new(),
        }
    }

    /// Rebuilds the visible list and tree-line metadata from a flat
    /// pre-order node list.
    ///
    /// - `is_visible(idx)` — should this node appear in the view?
    /// - `is_expanded(idx)` — is this node currently expanded?
    ///
    /// The caller manages expansion state however it wants (`HashSet<usize>`,
    /// `HashSet<PathBuf>`, etc.) and provides it via the `is_expanded` closure.
    ///
    /// After rebuild, the current selection is clamped to the new visible count.
    pub fn rebuild<T: TreeItem>(
        &mut self,
        nodes: &[T],
        is_visible: impl Fn(usize) -> bool,
        is_expanded: impl Fn(usize) -> bool,
    ) {
        // Pass 1 — collect visible indices
        self.visible.clear();
        for i in 0..nodes.len() {
            if is_visible(i) {
                self.visible.push(i);
            }
        }

        // Pass 2 — compute TreeLine metadata
        self.tree_lines.clear();
        let vis_len = self.visible.len();

        // Stack tracks is_last for each ancestor depth during traversal.
        let mut ancestor_stack: Vec<bool> = Vec::new();

        for vi in 0..vis_len {
            let node_idx = self.visible[vi];
            let depth = nodes[node_idx].depth();
            let has_children = nodes[node_idx].has_children();
            let expanded = is_expanded(node_idx);

            // Determine is_last: scan forward for next node at depth <= current.
            let is_last = is_last_sibling(&self.visible, vi, depth, nodes);

            // Maintain ancestor_stack: truncate to current depth, then
            // update the entry for current depth - 1 (the parent's is_last).
            // For depth 0, ancestor_is_last is empty.
            ancestor_stack.truncate(depth);

            let ancestor_is_last = ancestor_stack.clone();

            // Push this node's is_last for its children to reference.
            if ancestor_stack.len() == depth {
                ancestor_stack.push(is_last);
            }

            self.tree_lines.push(TreeLine {
                depth,
                is_last,
                ancestor_is_last,
                has_children,
                is_expanded: expanded,
            });
        }

        // Clamp selection to new visible count.
        self.scroll.set_selection(self.scroll.selection(), vis_len);
    }

    // ── Accessors ──

    /// Returns the backing node index of the currently selected item,
    /// or `None` if the visible list is empty.
    pub fn selected_node_idx(&self) -> Option<usize> {
        let sel = self.scroll.selection();
        self.visible.get(sel).copied()
    }

    /// Returns the slice of backing node indices that are currently visible.
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// Returns the slice of `TreeLine` metadata parallel to `visible`.
    pub fn tree_lines(&self) -> &[TreeLine] {
        &self.tree_lines
    }

    /// Returns how many items are currently visible.
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// Returns the `TreeLine` for a given visible-list index.
    pub fn tree_line_at(&self, visible_idx: usize) -> Option<&TreeLine> {
        self.tree_lines.get(visible_idx)
    }

    /// Returns a mutable slice of `TreeLine` metadata for post-processing.
    ///
    /// Useful for callers that need to adjust guide rail metadata after
    /// rebuild (e.g., removing depth-0 ancestor entries when root nodes
    /// are rendered as headers without branch connectors).
    pub fn tree_lines_mut(&mut self) -> &mut [TreeLine] {
        &mut self.tree_lines
    }

    // ── Navigation helpers ──

    /// Finds the visible-list index of the parent of the currently selected
    /// node (the nearest preceding node at a shallower depth).
    ///
    /// Returns `None` if the selection is at depth 0 or the visible list is empty.
    pub fn parent_visible_idx<T: TreeItem>(&self, nodes: &[T]) -> Option<usize> {
        if self.visible.is_empty() {
            return None;
        }
        let sel = self.scroll.selection();
        let current_depth = nodes[self.visible[sel]].depth();
        if current_depth == 0 {
            return None;
        }
        (0..sel)
            .rev()
            .find(|&i| nodes[self.visible[i]].depth() < current_depth)
    }

    /// Returns the visible-list index of the first child of the currently
    /// selected node (i.e. the next visible item, which is expected to be
    /// at a deeper depth if the current node is expanded).
    ///
    /// Returns `None` if there is no next visible item.
    pub fn first_child_visible_idx(&self) -> Option<usize> {
        let sel = self.scroll.selection();
        let next = sel + 1;
        if next < self.visible.len() {
            Some(next)
        } else {
            None
        }
    }

    // ── Scroll delegation ──

    /// Returns a shared reference to the inner `ScrollableList`.
    pub fn scroll(&self) -> &ScrollableList {
        &self.scroll
    }

    /// Returns a mutable reference to the inner `ScrollableList`.
    pub fn scroll_mut(&mut self) -> &mut ScrollableList {
        &mut self.scroll
    }
}

impl Default for TreeViewState {
    fn default() -> Self {
        Self::new()
    }
}

/// Determines whether the node at `visible[vi]` is the last sibling at its
/// depth by scanning forward for the next node at depth <= current.
fn is_last_sibling<T: TreeItem>(visible: &[usize], vi: usize, depth: usize, nodes: &[T]) -> bool {
    for j in (vi + 1)..visible.len() {
        let d = nodes[visible[j]].depth();
        if d <= depth {
            // Found a node at same or shallower depth.
            // It's last only if that node is shallower (not a sibling).
            return d < depth;
        }
    }
    // No more nodes at same or shallower depth → last.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal test node for a flat pre-order tree.
    #[derive(Clone)]
    struct TestNode {
        depth: usize,
        has_children: bool,
    }

    impl TreeItem for TestNode {
        fn depth(&self) -> usize {
            self.depth
        }
        fn has_children(&self) -> bool {
            self.has_children
        }
    }

    fn n(depth: usize, has_children: bool) -> TestNode {
        TestNode {
            depth,
            has_children,
        }
    }

    // ── rebuild basics ──

    #[test]
    fn test_empty_nodes() {
        let mut tree = TreeViewState::new();
        let nodes: Vec<TestNode> = vec![];
        tree.rebuild(&nodes, |_| true, |_| false);
        assert_eq!(tree.visible_count(), 0);
        assert!(tree.selected_node_idx().is_none());
    }

    #[test]
    fn test_all_visible() {
        let nodes = vec![n(0, true), n(1, false), n(1, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        assert_eq!(tree.visible_count(), 4);
        assert_eq!(tree.visible(), &[0, 1, 2, 3]);
    }

    #[test]
    fn test_filtered_visibility() {
        let nodes = vec![n(0, true), n(1, false), n(1, false), n(0, false)];
        let mut tree = TreeViewState::new();
        // Only show root-level nodes
        tree.rebuild(&nodes, |i| nodes[i].depth == 0, |_| false);
        assert_eq!(tree.visible_count(), 2);
        assert_eq!(tree.visible(), &[0, 3]);
    }

    #[test]
    fn test_selection_clamped_on_rebuild() {
        let nodes = vec![n(0, false), n(0, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        tree.scroll_mut().set_selection(2, 3);
        assert_eq!(tree.scroll().selection(), 2);

        // Rebuild with fewer visible items
        tree.rebuild(&nodes, |i| i == 0, |_| false);
        assert_eq!(tree.visible_count(), 1);
        assert_eq!(tree.scroll().selection(), 0);
    }

    // ── TreeLine metadata ──

    #[test]
    fn test_root_is_last() {
        // Two root nodes: first is not last, second is last.
        let nodes = vec![n(0, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        assert!(!tree.tree_lines()[0].is_last);
        assert!(tree.tree_lines()[1].is_last);
    }

    #[test]
    fn test_child_is_last() {
        // Root with two children: child 0 not last, child 1 last.
        let nodes = vec![n(0, true), n(1, false), n(1, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |idx| idx == 0);
        assert!(!tree.tree_lines()[1].is_last);
        assert!(tree.tree_lines()[2].is_last);
    }

    #[test]
    fn test_ancestor_is_last_depth2() {
        // root0 (not last)
        //   child0 (last at depth 1 under root0)
        //     grandchild0 (last at depth 2)
        // root1 (last)
        let nodes = vec![
            n(0, true),  // 0
            n(1, true),  // 1
            n(2, false), // 2
            n(0, false), // 3
        ];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |idx| idx == 0 || idx == 1);

        // grandchild at visible index 2 (node 2)
        let tl = &tree.tree_lines()[2];
        assert_eq!(tl.depth, 2);
        assert!(tl.is_last);
        // ancestor_is_last[0] = false (root0 is not last root)
        // ancestor_is_last[1] = true (child0 is last child of root0)
        assert_eq!(tl.ancestor_is_last, vec![false, true]);
    }

    #[test]
    fn test_has_children_and_expanded() {
        let nodes = vec![n(0, true), n(1, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |idx| idx == 0);

        assert!(tree.tree_lines()[0].has_children);
        assert!(tree.tree_lines()[0].is_expanded);
        assert!(!tree.tree_lines()[1].has_children);
        assert!(!tree.tree_lines()[1].is_expanded);
        assert!(!tree.tree_lines()[2].has_children);
    }

    // ── Navigation helpers ──

    #[test]
    fn test_parent_visible_idx_at_root() {
        let nodes = vec![n(0, true), n(1, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        // Selection at root (index 0) → no parent
        assert!(tree.parent_visible_idx(&nodes).is_none());
    }

    #[test]
    fn test_parent_visible_idx_from_child() {
        let nodes = vec![n(0, true), n(1, false), n(1, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        // Select child at visible index 2 (node 2, depth 1)
        let count = tree.visible_count();
        tree.scroll_mut().set_selection(2, count);
        let parent = tree.parent_visible_idx(&nodes);
        assert_eq!(parent, Some(0)); // visible index 0 = node 0 at depth 0
    }

    #[test]
    fn test_parent_visible_idx_from_grandchild() {
        let nodes = vec![
            n(0, true),  // vis 0
            n(1, true),  // vis 1
            n(2, false), // vis 2
        ];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        let count = tree.visible_count();
        tree.scroll_mut().set_selection(2, count);
        // Grandchild → parent is vis 1 (depth 1)
        assert_eq!(tree.parent_visible_idx(&nodes), Some(1));
    }

    #[test]
    fn test_first_child_visible_idx() {
        let nodes = vec![n(0, true), n(1, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        // Selection at 0 → first child is vis 1
        assert_eq!(tree.first_child_visible_idx(), Some(1));
    }

    #[test]
    fn test_first_child_visible_idx_at_end() {
        let nodes = vec![n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        // Only one item → no child
        assert!(tree.first_child_visible_idx().is_none());
    }

    #[test]
    fn test_selected_node_idx() {
        let nodes = vec![n(0, false), n(0, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        assert_eq!(tree.selected_node_idx(), Some(0));
        let count = tree.visible_count();
        tree.scroll_mut().down(count);
        assert_eq!(tree.selected_node_idx(), Some(1));
    }

    #[test]
    fn test_selected_node_idx_with_filter() {
        // Nodes 0, 1, 2, 3 — only even indices visible
        let nodes = vec![n(0, false), n(0, false), n(0, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |i| i % 2 == 0, |_| false);
        assert_eq!(tree.visible(), &[0, 2]);
        assert_eq!(tree.selected_node_idx(), Some(0));
        let count = tree.visible_count();
        tree.scroll_mut().down(count);
        assert_eq!(tree.selected_node_idx(), Some(2));
    }

    // ── Scroll delegation ──

    #[test]
    fn test_scroll_up_down() {
        let nodes = vec![n(0, false); 10];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);

        let count = tree.visible_count();
        tree.scroll_mut().down(count);
        tree.scroll_mut().down(count);
        assert_eq!(tree.scroll().selection(), 2);

        tree.scroll_mut().up(count);
        assert_eq!(tree.scroll().selection(), 1);
    }

    #[test]
    fn test_scroll_page_navigation() {
        let nodes = vec![n(0, false); 20];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);

        let count = tree.visible_count();
        tree.scroll_mut().page_down(5, count);
        assert_eq!(tree.scroll().selection(), 5);

        tree.scroll_mut().page_up(3, count);
        assert_eq!(tree.scroll().selection(), 2);
    }

    #[test]
    fn test_scroll_home_end() {
        let nodes = vec![n(0, false); 10];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);

        let count = tree.visible_count();
        tree.scroll_mut().end(count);
        assert_eq!(tree.scroll().selection(), 9);

        tree.scroll_mut().home();
        assert_eq!(tree.scroll().selection(), 0);
    }

    #[test]
    fn test_ensure_visible() {
        let nodes = vec![n(0, false); 20];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);

        let count = tree.visible_count();
        tree.scroll_mut().set_selection(15, count);
        tree.scroll_mut().ensure_visible(5);
        assert!(tree.scroll().selection() >= tree.scroll().scroll_offset());
        assert!(tree.scroll().selection() < tree.scroll().scroll_offset() + 5);
    }

    // ── Complex tree structures ──

    #[test]
    fn test_multi_level_tree_metadata() {
        // Simulate a command tree:
        // git (0, has_children, expanded)
        //   add (1, has_children)
        //     --verbose (2, leaf)
        //     --dry-run (2, leaf)
        //   commit (1, has_children)
        //     --message (2, leaf)
        // ls (0, leaf)
        let nodes = vec![
            n(0, true),  // 0: git
            n(1, true),  // 1: add
            n(2, false), // 2: --verbose
            n(2, false), // 3: --dry-run
            n(1, true),  // 4: commit
            n(2, false), // 5: --message
            n(0, false), // 6: ls
        ];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |idx| idx == 0 || idx == 1 || idx == 4);

        assert_eq!(tree.visible_count(), 7);

        // git: depth 0, not last (ls follows), expanded
        let git = &tree.tree_lines()[0];
        assert_eq!(git.depth, 0);
        assert!(!git.is_last);
        assert!(git.is_expanded);
        assert!(git.ancestor_is_last.is_empty());

        // add: depth 1, not last (commit follows at depth 1)
        let add = &tree.tree_lines()[1];
        assert_eq!(add.depth, 1);
        assert!(!add.is_last);

        // --verbose: depth 2, not last (--dry-run follows)
        let verbose = &tree.tree_lines()[2];
        assert_eq!(verbose.depth, 2);
        assert!(!verbose.is_last);
        // ancestor_is_last: [git=false, add=false]
        assert_eq!(verbose.ancestor_is_last, vec![false, false]);

        // --dry-run: depth 2, last (next is commit at depth 1)
        let dry_run = &tree.tree_lines()[3];
        assert_eq!(dry_run.depth, 2);
        assert!(dry_run.is_last);

        // commit: depth 1, last (next is ls at depth 0)
        let commit = &tree.tree_lines()[4];
        assert_eq!(commit.depth, 1);
        assert!(commit.is_last);

        // --message: depth 2, last (nothing follows at depth <= 2 under commit)
        let message = &tree.tree_lines()[5];
        assert_eq!(message.depth, 2);
        assert!(message.is_last);
        // ancestor_is_last: [git=false, commit=true]
        assert_eq!(message.ancestor_is_last, vec![false, true]);

        // ls: depth 0, last
        let ls = &tree.tree_lines()[6];
        assert_eq!(ls.depth, 0);
        assert!(ls.is_last);
    }

    #[test]
    fn test_default_impl() {
        let tree = TreeViewState::default();
        assert_eq!(tree.visible_count(), 0);
    }

    #[test]
    fn test_tree_line_at_bounds() {
        let nodes = vec![n(0, false), n(0, false)];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        assert!(tree.tree_line_at(0).is_some());
        assert!(tree.tree_line_at(1).is_some());
        assert!(tree.tree_line_at(2).is_none());
    }

    #[test]
    fn test_parent_empty_tree() {
        let nodes: Vec<TestNode> = vec![];
        let mut tree = TreeViewState::new();
        tree.rebuild(&nodes, |_| true, |_| false);
        assert!(tree.parent_visible_idx(&nodes).is_none());
    }
}
