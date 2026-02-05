//! Current working directory segment.
//!
//! Displays the current directory name (just the final component).

use unicode_width::UnicodeWidthStr;

use super::{RenderedSegment, SegmentAlign, TopbarSegment, TopbarState, color_to_fg_ansi};
use crate::chrome::symbols::Symbols;
use crate::chrome::theme::Theme;

/// Segment displaying current working directory.
///
/// Shows only the final path component (directory name).
pub struct CwdSegment;

impl TopbarSegment for CwdSegment {
    fn id(&self) -> &'static str {
        "cwd"
    }

    fn render(
        &self,
        state: &TopbarState,
        theme: &Theme,
        symbols: &Symbols,
        separator: &str,
    ) -> Option<RenderedSegment> {
        let cwd_str = state
            .cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/");

        let sep_fg = color_to_fg_ansi(theme.separator_fg);
        let cwd_fg = color_to_fg_ansi(theme.cwd_fg);
        let separator_width = separator.width();
        let folder = symbols.folder;
        let folder_width = folder.width();

        let (icon_part, icon_width) = if !folder.is_empty() {
            (format!("{} ", folder), folder_width + 1)
        } else {
            (String::new(), 0)
        };

        let content = format!(
            " {}{} {}{}{} ",
            sep_fg, separator, cwd_fg, icon_part, cwd_str
        );
        let display_width = separator_width + icon_width + cwd_str.width() + 3;

        Some(RenderedSegment {
            content,
            display_width,
            priority: 2,
            align: SegmentAlign::Left,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SymbolSet, ThemePreset};
    use std::path::PathBuf;

    fn test_state() -> TopbarState {
        TopbarState {
            cwd: PathBuf::from("/home/user/project"),
            ..Default::default()
        }
    }

    #[test]
    fn test_cwd_segment_id() {
        assert_eq!(CwdSegment.id(), "cwd");
    }

    #[test]
    fn test_cwd_segment_renders() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let state = test_state();

        let rendered = CwdSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_some());

        let segment = rendered.unwrap();
        assert_eq!(segment.priority, 2);
        assert!(segment.content.contains("project"));
    }

    #[test]
    fn test_cwd_segment_root() {
        let theme = Theme::for_preset(ThemePreset::Amber);
        let symbols = Symbols::for_set(SymbolSet::Fallback);
        let mut state = test_state();
        state.cwd = PathBuf::from("/");

        let rendered = CwdSegment.render(&state, theme, symbols, "▶");
        assert!(rendered.is_some());
        assert!(rendered.unwrap().content.contains("/"));
    }
}
