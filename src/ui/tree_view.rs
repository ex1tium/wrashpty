//! Generic tree rendering primitives.
//!
//! Pure functions that generate tree connector strings (│ ├─ └─ ▸ ▾) from
//! tree metadata. No state, no I/O — just layout helpers for any tree-shaped
//! data displayed in a TUI panel.

use crate::chrome::glyphs::TreeGlyphs;

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
pub fn tree_prefix(line: &TreeLine, chars: &TreeGlyphs) -> String {
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
    use crate::chrome::glyphs::{GlyphSet, GlyphTier};

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

    fn unicode_tree() -> &'static TreeGlyphs {
        &GlyphSet::for_tier(GlyphTier::Unicode).tree
    }

    fn ascii_tree() -> &'static TreeGlyphs {
        &GlyphSet::for_tier(GlyphTier::Ascii).tree
    }

    #[test]
    fn test_root_level_leaf() {
        let line = make_line(0, false, vec![], false, false);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, " ");
    }

    #[test]
    fn test_root_level_expanded() {
        let line = make_line(0, false, vec![], true, true);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "▾");
    }

    #[test]
    fn test_root_level_collapsed() {
        let line = make_line(0, false, vec![], true, false);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "▸");
    }

    #[test]
    fn test_depth1_not_last_leaf() {
        let line = make_line(1, false, vec![false], false, false);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "├─ ");
    }

    #[test]
    fn test_depth1_last_leaf() {
        let line = make_line(1, true, vec![false], false, false);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "└─ ");
    }

    #[test]
    fn test_depth1_not_last_collapsed() {
        let line = make_line(1, false, vec![false], true, false);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "├─▸");
    }

    #[test]
    fn test_depth1_last_expanded() {
        let line = make_line(1, true, vec![false], true, true);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "└─▾");
    }

    #[test]
    fn test_depth2_with_ancestor_not_last() {
        // ancestor at depth 0 is NOT last => show │
        let line = make_line(2, true, vec![false, false], false, false);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "│  └─ ");
    }

    #[test]
    fn test_depth2_with_ancestor_last() {
        // ancestor at depth 0 IS last => show blank
        let line = make_line(2, false, vec![true, false], true, true);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "   ├─▾");
    }

    #[test]
    fn test_depth3_mixed_ancestry() {
        // depth 0: not last (│), depth 1: last (blank)
        let line = make_line(3, true, vec![false, true, false], false, false);
        let prefix = tree_prefix(&line, unicode_tree());
        assert_eq!(prefix, "│     └─ ");
    }

    #[test]
    fn test_ascii_fallback() {
        let line = make_line(1, false, vec![false], true, false);
        let prefix = tree_prefix(&line, ascii_tree());
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
            let prefix = tree_prefix(&line, unicode_tree());
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
    fn test_tree_glyphs_for_nerdfont() {
        let chars = &GlyphSet::for_tier(GlyphTier::NerdFont).tree;
        assert_eq!(chars.vertical, "│");
    }

    #[test]
    fn test_tree_glyphs_for_ascii() {
        let chars = &GlyphSet::for_tier(GlyphTier::Ascii).tree;
        assert_eq!(chars.vertical, "|");
    }
}
