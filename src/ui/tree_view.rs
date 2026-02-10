//! Generic tree rendering primitives.
//!
//! Pure functions that generate tree connector strings (│ ├─ └─ ▸ ▾) from
//! tree metadata. No state, no I/O — just layout helpers for any tree-shaped
//! data displayed in a TUI panel.

use crate::config::SymbolSet;

/// Metadata about a single row in a flattened tree view.
#[derive(Debug, Clone)]
pub struct TreeLine {
    /// Nesting depth (0 = root level).
    pub depth: usize,
    /// Whether this node is the last sibling at its depth.
    pub is_last: bool,
    /// For each ancestor depth `0..depth`, whether that ancestor was the last
    /// sibling. Controls whether the guide rail at that depth shows `│` or blank.
    pub ancestor_is_last: Vec<bool>,
    /// Whether this node can be expanded (i.e. has children).
    pub has_children: bool,
    /// Whether this node is currently expanded.
    pub is_expanded: bool,
}

/// Character set for tree connector drawing.
#[derive(Debug, Clone, Copy)]
pub struct TreeChars {
    /// Vertical guide rail: `"│"` or `"|"`.
    pub vertical: &'static str,
    /// Branch connector (non-last sibling): `"├"` or `"|"`.
    pub branch: &'static str,
    /// Corner connector (last sibling): `"└"` or `` "`" ``.
    pub corner: &'static str,
    /// Horizontal connector: `"─"` or `"-"`.
    pub horizontal: &'static str,
    /// Expanded indicator: `"▾"` or `"v"`.
    pub expanded: &'static str,
    /// Collapsed indicator: `"▸"` or `">"`.
    pub collapsed: &'static str,
}

/// Unicode box-drawing tree characters (works in virtually all modern terminals).
pub const UNICODE_TREE_CHARS: TreeChars = TreeChars {
    vertical: "│",
    branch: "├",
    corner: "└",
    horizontal: "─",
    expanded: "▾",
    collapsed: "▸",
};

/// ASCII-only fallback tree characters.
pub const ASCII_TREE_CHARS: TreeChars = TreeChars {
    vertical: "|",
    branch: "|",
    corner: "`",
    horizontal: "-",
    expanded: "v",
    collapsed: ">",
};

/// Returns the appropriate `TreeChars` for a symbol set.
pub fn tree_chars_for_set(set: SymbolSet) -> &'static TreeChars {
    match set {
        SymbolSet::NerdFont => &UNICODE_TREE_CHARS,
        SymbolSet::Fallback => &ASCII_TREE_CHARS,
    }
}

/// Generates the prefix string for a tree line.
///
/// The prefix consists of:
/// 1. Guide rails for each ancestor depth (`│  ` or `   `)
/// 2. A branch/corner connector at the current depth (`├─` or `└─`)
/// 3. An expand/collapse indicator if the node has children (`▸`/`▾`) or a space
///
/// # Display width
///
/// For depth > 0, the display width is always `depth * 3` columns.
/// For depth 0 (root-level items), only the expand indicator is shown (1 column).
pub fn tree_prefix(line: &TreeLine, chars: &TreeChars) -> String {
    let mut result = String::new();

    if line.depth > 0 {
        // Guide rails for ancestor depths 0..(depth-1)
        for d in 0..line.depth - 1 {
            let is_last_at_depth = line.ancestor_is_last.get(d).copied().unwrap_or(false);
            if is_last_at_depth {
                result.push_str("   ");
            } else {
                result.push_str(chars.vertical);
                result.push_str("  ");
            }
        }

        // Branch or corner at current depth
        if line.is_last {
            result.push_str(chars.corner);
            result.push_str(chars.horizontal);
        } else {
            result.push_str(chars.branch);
            result.push_str(chars.horizontal);
        }
    }

    // Expand/collapse indicator or space
    if line.has_children {
        if line.is_expanded {
            result.push_str(chars.expanded);
        } else {
            result.push_str(chars.collapsed);
        }
    } else {
        result.push(' ');
    }

    result
}

/// Returns the display width of a tree prefix for a given depth.
///
/// Depth 0: 1 column (just the expand indicator).
/// Depth > 0: `depth * 3` columns — each level adds 3 columns
/// (guide rail or connector + horizontal + indicator/space).
pub fn tree_prefix_width(depth: usize) -> usize {
    if depth == 0 { 1 } else { depth * 3 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_line(
        depth: usize,
        is_last: bool,
        ancestor_is_last: Vec<bool>,
        has_children: bool,
        is_expanded: bool,
    ) -> TreeLine {
        TreeLine {
            depth,
            is_last,
            ancestor_is_last,
            has_children,
            is_expanded,
        }
    }

    #[test]
    fn test_root_level_leaf() {
        let line = make_line(0, false, vec![], false, false);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, " ");
    }

    #[test]
    fn test_root_level_expanded() {
        let line = make_line(0, false, vec![], true, true);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "▾");
    }

    #[test]
    fn test_root_level_collapsed() {
        let line = make_line(0, false, vec![], true, false);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "▸");
    }

    #[test]
    fn test_depth1_not_last_leaf() {
        let line = make_line(1, false, vec![false], false, false);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "├─ ");
    }

    #[test]
    fn test_depth1_last_leaf() {
        let line = make_line(1, true, vec![false], false, false);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "└─ ");
    }

    #[test]
    fn test_depth1_not_last_collapsed() {
        let line = make_line(1, false, vec![false], true, false);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "├─▸");
    }

    #[test]
    fn test_depth1_last_expanded() {
        let line = make_line(1, true, vec![false], true, true);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "└─▾");
    }

    #[test]
    fn test_depth2_with_ancestor_not_last() {
        // ancestor at depth 0 is NOT last => show │
        let line = make_line(2, true, vec![false, false], false, false);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "│  └─ ");
    }

    #[test]
    fn test_depth2_with_ancestor_last() {
        // ancestor at depth 0 IS last => show blank
        let line = make_line(2, false, vec![true, false], true, true);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "   ├─▾");
    }

    #[test]
    fn test_depth3_mixed_ancestry() {
        // depth 0: not last (│), depth 1: last (blank)
        let line = make_line(3, true, vec![false, true, false], false, false);
        let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
        assert_eq!(prefix, "│     └─ ");
    }

    #[test]
    fn test_ascii_fallback() {
        let line = make_line(1, false, vec![false], true, false);
        let prefix = tree_prefix(&line, &ASCII_TREE_CHARS);
        assert_eq!(prefix, "|->");
    }

    #[test]
    fn test_tree_prefix_width_depth0() {
        assert_eq!(tree_prefix_width(0), 1);
    }

    #[test]
    fn test_tree_prefix_width_depth1() {
        assert_eq!(tree_prefix_width(1), 3); // 1*3
    }

    #[test]
    fn test_tree_prefix_width_depth2() {
        assert_eq!(tree_prefix_width(2), 6); // 2*3
    }

    #[test]
    fn test_prefix_display_width_matches() {
        // Verify that the actual display width of generated prefixes matches
        // the predicted width from tree_prefix_width.
        use unicode_width::UnicodeWidthStr;

        for depth in 0..=4 {
            let line = make_line(depth, false, vec![false; depth], true, false);
            let prefix = tree_prefix(&line, &UNICODE_TREE_CHARS);
            let actual_width = UnicodeWidthStr::width(prefix.as_str());
            let expected_width = tree_prefix_width(depth);
            assert_eq!(
                actual_width, expected_width,
                "Mismatch at depth {}: prefix={:?} actual_w={} expected_w={}",
                depth, prefix, actual_width, expected_width
            );
        }
    }

    #[test]
    fn test_tree_chars_for_set_nerdfont() {
        let chars = tree_chars_for_set(SymbolSet::NerdFont);
        assert_eq!(chars.vertical, "│");
    }

    #[test]
    fn test_tree_chars_for_set_fallback() {
        let chars = tree_chars_for_set(SymbolSet::Fallback);
        assert_eq!(chars.vertical, "|");
    }
}
