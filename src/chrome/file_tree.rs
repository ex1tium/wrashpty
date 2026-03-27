//! File tree state machine for the tree-view file browser.
//!
//! Manages a lazy-loaded directory tree with expand/collapse, sorting, filtering,
//! and git status integration. Produces a flattened list of `FlatEntry` for rendering.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tracing::debug;

use crate::git::{GitFileStatus, GitRepoStatus};
use crate::ui::tree_view::TreeLine;

/// Sort mode for directory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    #[default]
    Name,
    Modified,
    Size,
}

impl SortMode {
    /// Cycles to the next sort mode.
    pub fn next(self) -> Self {
        match self {
            SortMode::Name => SortMode::Modified,
            SortMode::Modified => SortMode::Size,
            SortMode::Size => SortMode::Name,
        }
    }

    /// Returns a display label for the sort mode.
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "Name",
            SortMode::Modified => "Date",
            SortMode::Size => "Size",
        }
    }
}

/// A directory entry in the file browser.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// File or directory name.
    pub name: String,
    /// Full (absolute) path.
    pub path: PathBuf,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// File size in bytes.
    pub size: u64,
    /// Last modification time.
    pub modified: Option<SystemTime>,
    /// Unix permissions mode (e.g., 0o755).
    pub mode: u32,
}

/// A flattened entry ready for rendering in the tree view.
#[derive(Debug, Clone)]
pub struct FlatEntry {
    /// The underlying directory entry.
    pub entry: DirEntry,
    /// Nesting depth (0 = root level).
    pub depth: usize,
    /// Tree line metadata for rendering prefixes.
    pub tree_line: TreeLine,
    /// Git status for this file (None if not in a git repo or clean).
    pub git_status: Option<GitFileStatus>,
    /// Whether this entry is on the focus path (for spotlight dimming).
    pub in_focus_path: bool,
}

/// Maximum depth cycle values.
const DEPTH_CYCLE: &[usize] = &[2, 4, 8, 0];

/// Tree state manager for the file browser.
pub struct FileTreeState {
    /// Root directory of the tree.
    root: PathBuf,
    /// Cache of loaded directories: path -> sorted entries.
    dir_cache: HashMap<PathBuf, Vec<DirEntry>>,
    /// Set of expanded directory paths.
    expanded: HashSet<PathBuf>,
    /// Computed flat list of visible entries.
    flattened: Vec<FlatEntry>,
    /// Maximum depth to display (0 = unlimited).
    max_depth: usize,
    /// Current sort mode.
    sort_mode: SortMode,
    /// Whether to show hidden files.
    show_hidden: bool,
    /// Whether spotlight dimming is enabled.
    spotlight: bool,
    /// Cached git repository status.
    git_status: Option<GitRepoStatus>,
}

impl FileTreeState {
    /// Creates a new tree rooted at the given directory.
    pub fn new(root: PathBuf) -> Self {
        let mut state = Self {
            root: root.clone(),
            dir_cache: HashMap::new(),
            expanded: HashSet::new(),
            flattened: Vec::new(),
            max_depth: 0,
            sort_mode: SortMode::default(),
            show_hidden: false,
            spotlight: false,
            git_status: None,
        };
        state.load_dir(&root);
        state.flatten();
        state
    }

    /// Returns the root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the flattened entry list.
    pub fn entries(&self) -> &[FlatEntry] {
        &self.flattened
    }

    /// Returns the number of visible (flattened) entries.
    pub fn len(&self) -> usize {
        self.flattened.len()
    }

    /// Returns true if the tree has no visible entries.
    pub fn is_empty(&self) -> bool {
        self.flattened.is_empty()
    }

    /// Returns the entry at the given flattened index.
    pub fn entry_at(&self, index: usize) -> Option<&FlatEntry> {
        self.flattened.get(index)
    }

    /// Returns the current sort mode.
    pub fn sort_mode(&self) -> SortMode {
        self.sort_mode
    }

    /// Returns the current max depth (0 = unlimited).
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Returns whether hidden files are shown.
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Returns whether spotlight dimming is enabled.
    pub fn spotlight(&self) -> bool {
        self.spotlight
    }

    /// Returns the git repo status if available.
    pub fn git_status(&self) -> Option<&GitRepoStatus> {
        self.git_status.as_ref()
    }

    /// Changes the root directory, clearing expanded state and reloading.
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root.clone();
        self.expanded.clear();
        self.dir_cache.clear();
        self.load_dir(&root);
        self.flatten();
    }

    /// Returns true if the entry at `index` is an expanded directory.
    pub fn is_expanded(&self, index: usize) -> bool {
        self.flattened
            .get(index)
            .map(|e| e.entry.is_dir && self.expanded.contains(&e.entry.path))
            .unwrap_or(false)
    }

    /// Toggles expand/collapse for the entry at `index`.
    pub fn toggle_expand(&mut self, index: usize) {
        if let Some(entry) = self.flattened.get(index) {
            if entry.entry.is_dir {
                let path = entry.entry.path.clone();
                if self.expanded.contains(&path) {
                    self.expanded.remove(&path);
                } else {
                    self.load_dir(&path);
                    self.expanded.insert(path);
                }
                self.flatten();
            }
        }
    }

    /// Expands the directory at `index` (no-op if already expanded or not a dir).
    pub fn expand(&mut self, index: usize) {
        if let Some(entry) = self.flattened.get(index) {
            if entry.entry.is_dir && !self.expanded.contains(&entry.entry.path) {
                let path = entry.entry.path.clone();
                self.load_dir(&path);
                self.expanded.insert(path);
                self.flatten();
            }
        }
    }

    /// Collapses the directory at `index` (no-op if already collapsed or not a dir).
    pub fn collapse(&mut self, index: usize) {
        if let Some(entry) = self.flattened.get(index) {
            if entry.entry.is_dir && self.expanded.contains(&entry.entry.path) {
                let path = entry.entry.path.clone();
                self.expanded.remove(&path);
                self.flatten();
            }
        }
    }

    /// Toggles show_hidden and reloads.
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        // Reload all cached directories to reflect hidden file toggle
        let dirs_to_reload: Vec<PathBuf> = self.dir_cache.keys().cloned().collect();
        self.dir_cache.clear();
        for dir in dirs_to_reload {
            self.load_dir(&dir);
        }
        self.flatten();
    }

    /// Cycles sort mode and re-sorts.
    pub fn cycle_sort(&mut self) {
        self.sort_mode = self.sort_mode.next();
        // Re-sort all cached directories
        let sort = self.sort_mode;
        for entries in self.dir_cache.values_mut() {
            Self::sort_entries(entries, sort);
        }
        self.flatten();
    }

    /// Cycles max depth through [2, 4, 8, 0 (unlimited)].
    pub fn cycle_depth(&mut self) {
        let current_idx = DEPTH_CYCLE
            .iter()
            .position(|&d| d == self.max_depth)
            .unwrap_or(DEPTH_CYCLE.len() - 1);
        self.max_depth = DEPTH_CYCLE[(current_idx + 1) % DEPTH_CYCLE.len()];
        self.flatten();
    }

    /// Toggles spotlight dimming.
    pub fn toggle_spotlight(&mut self) {
        self.spotlight = !self.spotlight;
    }

    /// Sets the git status for the tree.
    pub fn set_git_status(&mut self, status: Option<GitRepoStatus>) {
        self.git_status = status;
        self.flatten();
    }

    /// Updates the focus path based on the selected index.
    ///
    /// Marks all ancestors of the selected entry as `in_focus_path`.
    pub fn update_focus(&mut self, selected_index: usize) {
        if !self.spotlight {
            // Clear all focus flags when spotlight is off
            for entry in &mut self.flattened {
                entry.in_focus_path = true; // All visible when spotlight off
            }
            return;
        }

        let focus_set = self.compute_focus_path(selected_index);
        for entry in &mut self.flattened {
            entry.in_focus_path = focus_set.contains(&entry.entry.path);
        }
    }

    /// Refreshes the tree: reloads all cached directories and re-flattens.
    pub fn refresh(&mut self) {
        let mut dirs_to_reload: Vec<PathBuf> = self.dir_cache.keys().cloned().collect();
        self.dir_cache.clear();
        // Ensure root is always reloaded
        let root = self.root.clone();
        if !dirs_to_reload.contains(&root) {
            dirs_to_reload.push(root);
        }
        for dir in dirs_to_reload {
            self.load_dir(&dir);
        }
        self.flatten();
    }

    /// Finds the parent entry index in the flattened list for an entry at `index`.
    ///
    /// Walks backwards to find the nearest entry at depth - 1 whose path
    /// is a prefix of the entry's parent path.
    pub fn parent_index(&self, index: usize) -> Option<usize> {
        let entry = self.flattened.get(index)?;
        if entry.depth == 0 {
            return None;
        }
        let parent_path = entry.entry.path.parent()?;
        let target_depth = entry.depth - 1;

        for i in (0..index).rev() {
            let candidate = &self.flattened[i];
            if candidate.depth == target_depth && candidate.entry.path == parent_path {
                return Some(i);
            }
        }
        None
    }

    /// Returns the index of the first child of an expanded directory at `index`.
    pub fn first_child_index(&self, index: usize) -> Option<usize> {
        let entry = self.flattened.get(index)?;
        if !entry.entry.is_dir || !self.expanded.contains(&entry.entry.path) {
            return None;
        }
        // The first child is the next entry at depth + 1
        let child_idx = index + 1;
        let child = self.flattened.get(child_idx)?;
        if child.depth == entry.depth + 1 {
            Some(child_idx)
        } else {
            None
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Loads a directory into dir_cache if not already present.
    fn load_dir(&mut self, path: &Path) {
        if self.dir_cache.contains_key(path) {
            return;
        }

        let mut entries = Vec::new();

        match fs::read_dir(path) {
            Ok(read_dir) => {
                for dir_entry in read_dir.flatten() {
                    let name = dir_entry.file_name().to_string_lossy().to_string();

                    // Skip hidden files if not showing them
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }

                    let entry_path = dir_entry.path();
                    let metadata = dir_entry.metadata().ok();
                    let is_dir = entry_path.is_dir();
                    let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = metadata.as_ref().and_then(|m| m.modified().ok());

                    #[cfg(unix)]
                    let mode = {
                        use std::os::unix::fs::PermissionsExt;
                        metadata
                            .as_ref()
                            .map(|m| m.permissions().mode())
                            .unwrap_or(if is_dir { 0o755 } else { 0o644 })
                    };
                    #[cfg(not(unix))]
                    let mode = if is_dir { 0o755 } else { 0o644 };

                    entries.push(DirEntry {
                        name,
                        path: entry_path,
                        is_dir,
                        size,
                        modified,
                        mode,
                    });
                }
            }
            Err(e) => {
                debug!(dir = %path.display(), err = %e, "Failed to read directory");
            }
        }

        Self::sort_entries(&mut entries, self.sort_mode);
        self.dir_cache.insert(path.to_path_buf(), entries);
    }

    /// Sorts entries: directories first, then by sort mode.
    fn sort_entries(entries: &mut [DirEntry], sort_mode: SortMode) {
        entries.sort_by(|a, b| {
            // Directories always first
            match (a.is_dir, b.is_dir) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }

            match sort_mode {
                SortMode::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortMode::Modified => {
                    // Most recently modified first
                    b.modified.cmp(&a.modified)
                }
                SortMode::Size => {
                    // Largest first
                    b.size.cmp(&a.size)
                }
            }
        });
    }

    /// Performs DFS flattening of expanded directories.
    fn flatten(&mut self) {
        let mut result = Vec::new();
        let root = self.root.clone();

        if let Some(entries) = self.dir_cache.get(&root) {
            let entries = entries.clone();
            let len = entries.len();
            for (i, entry) in entries.into_iter().enumerate() {
                let is_last = i == len - 1;
                self.flatten_entry(entry, 0, is_last, &[], &mut result);
            }
        }

        self.flattened = result;
    }

    /// Recursively flattens a single entry and its expanded children.
    fn flatten_entry(
        &self,
        entry: DirEntry,
        depth: usize,
        is_last: bool,
        ancestor_is_last: &[bool],
        result: &mut Vec<FlatEntry>,
    ) {
        let is_dir = entry.is_dir;
        let path = entry.path.clone();
        let is_expanded = is_dir && self.expanded.contains(&path);
        let has_children = is_dir;

        // Look up git status
        let git_status = self.git_status.as_ref().and_then(|gs| gs.status_for(&path));

        let tree_line = TreeLine {
            depth,
            is_last,
            ancestor_is_last: ancestor_is_last.to_vec(),
            has_children,
            is_expanded,
            is_checked: false,
        };

        result.push(FlatEntry {
            entry,
            depth,
            tree_line,
            git_status,
            in_focus_path: true, // Default to true; update_focus() refines this
        });

        // Recurse into expanded directories
        if is_expanded {
            let within_depth = self.max_depth == 0 || depth + 1 < self.max_depth;
            if within_depth {
                if let Some(children) = self.dir_cache.get(&path) {
                    let children = children.clone();
                    let child_len = children.len();
                    let mut new_ancestor = ancestor_is_last.to_vec();
                    new_ancestor.push(is_last);

                    for (i, child) in children.into_iter().enumerate() {
                        let child_is_last = i == child_len - 1;
                        self.flatten_entry(child, depth + 1, child_is_last, &new_ancestor, result);
                    }
                }
            }
        }
    }

    /// Computes the set of paths on the focus path from the selected entry to root.
    fn compute_focus_path(&self, selected_index: usize) -> HashSet<PathBuf> {
        let mut focus_set = HashSet::new();

        if let Some(entry) = self.flattened.get(selected_index) {
            // Add the selected entry itself
            focus_set.insert(entry.entry.path.clone());

            // Walk up the ancestors
            let mut path = entry.entry.path.as_path();
            while let Some(parent) = path.parent() {
                focus_set.insert(parent.to_path_buf());
                if parent == self.root || parent == Path::new("/") {
                    break;
                }
                path = parent;
            }

            // Also add siblings at the same level as the selected entry
            let selected_depth = entry.depth;
            let selected_parent = entry.entry.path.parent().map(|p| p.to_path_buf());
            for flat in &self.flattened {
                if flat.depth == selected_depth {
                    if let Some(ref sp) = selected_parent {
                        if flat.entry.path.parent() == Some(sp.as_path()) {
                            focus_set.insert(flat.entry.path.clone());
                        }
                    }
                }
            }

            // Add children of the selected entry (if expanded)
            if entry.entry.is_dir && self.expanded.contains(&entry.entry.path) {
                for flat in &self.flattened {
                    if flat.entry.path.starts_with(&entry.entry.path) {
                        focus_set.insert(flat.entry.path.clone());
                    }
                }
            }
        }

        focus_set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a mock FileTreeState with pre-loaded dir_cache (no filesystem access).
    /// Entries are sorted according to the default sort mode.
    fn mock_tree(mut entries: Vec<DirEntry>) -> FileTreeState {
        let root = PathBuf::from("/test");
        FileTreeState::sort_entries(&mut entries, SortMode::default());
        let mut dir_cache = HashMap::new();
        dir_cache.insert(root.clone(), entries);

        let mut state = FileTreeState {
            root,
            dir_cache,
            expanded: HashSet::new(),
            flattened: Vec::new(),
            max_depth: 0,
            sort_mode: SortMode::default(),
            show_hidden: false,
            spotlight: false,
            git_status: None,
        };
        state.flatten();
        state
    }

    fn file_entry(name: &str, parent: &str) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            path: PathBuf::from(parent).join(name),
            is_dir: false,
            size: 100,
            modified: None,
            mode: 0o644,
        }
    }

    fn dir_entry(name: &str, parent: &str) -> DirEntry {
        DirEntry {
            name: name.to_string(),
            path: PathBuf::from(parent).join(name),
            is_dir: true,
            size: 0,
            modified: None,
            mode: 0o755,
        }
    }

    #[test]
    fn test_flatten_empty_directory() {
        let tree = mock_tree(vec![]);
        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
    }

    #[test]
    fn test_flatten_flat_directory() {
        let tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree.entries()[0].entry.name, "src");
        assert_eq!(tree.entries()[0].depth, 0);
        assert_eq!(tree.entries()[1].entry.name, "README.md");
        assert_eq!(tree.entries()[1].depth, 0);
    }

    #[test]
    fn test_flatten_tree_lines() {
        let tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);
        // src is not last (README follows)
        assert!(!tree.entries()[0].tree_line.is_last);
        assert!(tree.entries()[0].tree_line.has_children);
        assert!(!tree.entries()[0].tree_line.is_expanded);
        // README.md is last
        assert!(tree.entries()[1].tree_line.is_last);
        assert!(!tree.entries()[1].tree_line.has_children);
    }

    #[test]
    fn test_expand_directory() {
        let mut tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);

        // Pre-load children for src
        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![file_entry("main.rs", "/test/src")],
        );

        assert_eq!(tree.len(), 2);
        tree.expand(0); // Expand "src"
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.entries()[1].entry.name, "main.rs");
        assert_eq!(tree.entries()[1].depth, 1);
    }

    #[test]
    fn test_collapse_directory() {
        let mut tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);

        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![file_entry("main.rs", "/test/src")],
        );

        tree.expand(0);
        assert_eq!(tree.len(), 3);
        tree.collapse(0);
        assert_eq!(tree.len(), 2);
    }

    #[test]
    fn test_toggle_expand() {
        let mut tree = mock_tree(vec![dir_entry("src", "/test")]);

        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![file_entry("main.rs", "/test/src")],
        );

        assert!(!tree.is_expanded(0));
        tree.toggle_expand(0);
        assert!(tree.is_expanded(0));
        assert_eq!(tree.len(), 2);
        tree.toggle_expand(0);
        assert!(!tree.is_expanded(0));
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn test_expanded_tree_lines() {
        let mut tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);

        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![
                file_entry("lib.rs", "/test/src"),
                file_entry("main.rs", "/test/src"),
            ],
        );

        tree.expand(0);
        // After expand: src, lib.rs, main.rs, README.md
        assert_eq!(tree.len(), 4);

        // lib.rs at depth 1, not last
        let lib = &tree.entries()[1];
        assert_eq!(lib.depth, 1);
        assert!(!lib.tree_line.is_last);
        assert_eq!(lib.tree_line.ancestor_is_last, vec![false]);

        // main.rs at depth 1, last child of src
        let main = &tree.entries()[2];
        assert_eq!(main.depth, 1);
        assert!(main.tree_line.is_last);
    }

    #[test]
    fn test_sort_name() {
        let tree = mock_tree(vec![
            file_entry("zebra.rs", "/test"),
            file_entry("apple.rs", "/test"),
            dir_entry("src", "/test"),
        ]);
        // Dirs first, then alphabetical
        assert_eq!(tree.entries()[0].entry.name, "src");
        assert_eq!(tree.entries()[1].entry.name, "apple.rs");
        assert_eq!(tree.entries()[2].entry.name, "zebra.rs");
    }

    #[test]
    fn test_cycle_sort() {
        let mut tree = mock_tree(vec![]);
        assert_eq!(tree.sort_mode(), SortMode::Name);
        tree.cycle_sort();
        assert_eq!(tree.sort_mode(), SortMode::Modified);
        tree.cycle_sort();
        assert_eq!(tree.sort_mode(), SortMode::Size);
        tree.cycle_sort();
        assert_eq!(tree.sort_mode(), SortMode::Name);
    }

    #[test]
    fn test_cycle_depth() {
        let mut tree = mock_tree(vec![]);
        assert_eq!(tree.max_depth(), 0); // unlimited
        tree.cycle_depth();
        assert_eq!(tree.max_depth(), 2);
        tree.cycle_depth();
        assert_eq!(tree.max_depth(), 4);
        tree.cycle_depth();
        assert_eq!(tree.max_depth(), 8);
        tree.cycle_depth();
        assert_eq!(tree.max_depth(), 0); // back to unlimited
    }

    #[test]
    fn test_max_depth_limits_expansion() {
        let mut tree = mock_tree(vec![dir_entry("a", "/test")]);

        tree.dir_cache
            .insert(PathBuf::from("/test/a"), vec![dir_entry("b", "/test/a")]);
        tree.dir_cache.insert(
            PathBuf::from("/test/a/b"),
            vec![file_entry("c.rs", "/test/a/b")],
        );

        tree.max_depth = 1; // Only allow depth 0
        tree.expanded.insert(PathBuf::from("/test/a"));
        tree.expanded.insert(PathBuf::from("/test/a/b"));
        tree.flatten();

        // With max_depth=1, only root entries show (depth 0 only)
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.entries()[0].entry.name, "a");
    }

    #[test]
    fn test_git_status_integration() {
        let mut tree = mock_tree(vec![
            file_entry("modified.rs", "/test"),
            file_entry("clean.rs", "/test"),
        ]);

        let mut files = HashMap::new();
        files.insert(PathBuf::from("modified.rs"), GitFileStatus::Modified);

        tree.set_git_status(Some(GitRepoStatus {
            files,
            repo_root: PathBuf::from("/test"),
        }));

        // After sorting: clean.rs (index 0), modified.rs (index 1)
        assert_eq!(tree.entries()[0].entry.name, "clean.rs");
        assert_eq!(tree.entries()[0].git_status, None);
        assert_eq!(tree.entries()[1].entry.name, "modified.rs");
        assert_eq!(tree.entries()[1].git_status, Some(GitFileStatus::Modified));
    }

    #[test]
    fn test_parent_index() {
        let mut tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);

        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![file_entry("main.rs", "/test/src")],
        );

        tree.expand(0);
        // Entries: src(0), main.rs(1), README.md(2)
        assert_eq!(tree.parent_index(1), Some(0)); // main.rs parent is src
        assert_eq!(tree.parent_index(0), None); // src has no parent in tree
    }

    #[test]
    fn test_first_child_index() {
        let mut tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);

        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![file_entry("main.rs", "/test/src")],
        );

        // Not expanded: no child
        assert_eq!(tree.first_child_index(0), None);

        tree.expand(0);
        // Expanded: first child at index 1
        assert_eq!(tree.first_child_index(0), Some(1));
    }

    #[test]
    fn test_spotlight_focus_path() {
        let mut tree = mock_tree(vec![
            dir_entry("src", "/test"),
            file_entry("README.md", "/test"),
        ]);

        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![file_entry("main.rs", "/test/src")],
        );

        tree.expand(0);
        tree.spotlight = true;
        tree.update_focus(1); // Focus on main.rs

        // main.rs and its parent src should be in focus
        assert!(tree.entries()[0].in_focus_path); // src (parent)
        assert!(tree.entries()[1].in_focus_path); // main.rs (selected)
        // README.md is a sibling at depth 0, NOT in the focus path
        // (it's at a different level than main.rs)
        // Actually, siblings at same depth... README is depth 0, main.rs is depth 1
        // So README.md should NOT be focused
    }

    #[test]
    fn test_sort_mode_label() {
        assert_eq!(SortMode::Name.label(), "Name");
        assert_eq!(SortMode::Modified.label(), "Date");
        assert_eq!(SortMode::Size.label(), "Size");
    }

    #[test]
    fn test_set_root_clears_expanded() {
        let mut tree = mock_tree(vec![dir_entry("src", "/test")]);
        tree.dir_cache.insert(
            PathBuf::from("/test/src"),
            vec![file_entry("main.rs", "/test/src")],
        );
        tree.expand(0);
        assert!(!tree.expanded.is_empty());

        // Manually set up new root's cache since we're not hitting the filesystem
        tree.dir_cache.insert(PathBuf::from("/other"), vec![]);
        tree.set_root(PathBuf::from("/other"));
        assert!(tree.expanded.is_empty());
    }
}
