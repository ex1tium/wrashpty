//! Git branch segment.
//!
//! Displays the current git branch with optional dirty indicator.

use unicode_width::UnicodeWidthStr;

use super::{RenderedSegment, SegmentAlign, TopbarSegment, TopbarState, color_to_fg_ansi};
use crate::chrome::glyphs::GlyphSet;
use crate::chrome::theme::Theme;

/// Segment displaying git branch and dirty status.
///
/// Only renders when inside a git repository.
/// Shows dirty indicator (●) when there are uncommitted changes.
pub struct GitSegment;

impl TopbarSegment for GitSegment {
    fn id(&self) -> &'static str {
        "git"
    }

    fn render(
        &self,
        state: &TopbarState,
        theme: &Theme,
        glyphs: &GlyphSet,
        separator: &str,
    ) -> Option<RenderedSegment> {
        let branch = state.git.branch.as_ref()?;

        let sep_fg = color_to_fg_ansi(theme.separator_fg);
        let separator_width = separator.width();
        let git_branch_icon = glyphs.icon.git_branch;
        let git_branch_width = git_branch_icon.width();
        let dirty_icon = glyphs.icon.git_dirty;
        let dirty_width = if state.git.dirty {
            dirty_icon.width()
        } else {
            0
        };

        let (git_fg, dirty_part) = if state.git.dirty {
            (color_to_fg_ansi(theme.git_dirty_fg), dirty_icon)
        } else {
            (color_to_fg_ansi(theme.git_fg), "")
        };

        let (icon_part, icon_width) = if !git_branch_icon.is_empty() {
            (format!("{} ", git_branch_icon), git_branch_width + 1)
        } else {
            (String::new(), 0)
        };

        let content = format!(
            " {}{} {}{}{}{} ",
            sep_fg, separator, git_fg, icon_part, branch, dirty_part
        );
        let display_width = separator_width + icon_width + branch.width() + dirty_width + 3;

        Some(RenderedSegment {
            content,
            display_width,
            priority: 4,
            align: SegmentAlign::Left,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::segments::GitInfo;
    use crate::chrome::glyphs::GlyphSet;
    use crate::config::ThemePreset;

    fn test_state() -> TopbarState {
        TopbarState::default()
    }

    #[test]
    fn test_id_returns_git() {
        assert_eq!(GitSegment.id(), "git");
    }

    #[test]
    fn test_render_when_not_in_repo_is_hidden() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let glyphs = GlyphSet::for_tier(crate::chrome::glyphs::GlyphTier::Unicode);
        let state = test_state();

        let rendered = GitSegment.render(&state, theme, glyphs, "▶");
        assert!(rendered.is_none());
    }

    #[test]
    fn test_render_when_clean_shows_branch() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let glyphs = GlyphSet::for_tier(crate::chrome::glyphs::GlyphTier::Unicode);
        let mut state = test_state();
        state.git = GitInfo {
            branch: Some("main".to_string()),
            dirty: false,
        };

        let rendered = GitSegment.render(&state, theme, glyphs, "▶");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert_eq!(segment.priority, 4);
        assert!(segment.content.contains("main"));
    }

    #[test]
    fn test_render_when_dirty_shows_branch_and_dirty_symbol() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let glyphs = GlyphSet::for_tier(crate::chrome::glyphs::GlyphTier::Unicode);
        let mut state = test_state();
        state.git = GitInfo {
            branch: Some("feature".to_string()),
            dirty: true,
        };

        let rendered = GitSegment.render(&state, theme, glyphs, "▶");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert!(segment.content.contains("feature"));
        assert!(segment.content.contains(glyphs.icon.git_dirty));
    }
}
