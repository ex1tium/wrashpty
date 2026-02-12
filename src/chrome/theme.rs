//! Color themes for the chrome layer.
//!
//! Provides two theme presets:
//! - **Amber**: Hardcoded RGB colors for a vintage VT220 terminal look
//! - **Terminal**: Uses ANSI colors, letting the terminal control appearance

use crate::config::ThemePreset;
use ratatui_core::style::Color;

/// Color palette for the UI theme.
///
/// All colors in the UI should reference fields from this struct
/// to ensure consistent theming across all components.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // === Context Bar Colors ===
    /// Main bar background.
    pub bar_bg: Color,
    /// Segment background (for powerline-style contrast).
    pub segment_bg: Color,
    /// Success status foreground.
    pub success_fg: Color,
    /// Failure status foreground.
    pub failure_fg: Color,
    /// Current working directory foreground.
    pub cwd_fg: Color,
    /// Git branch foreground (clean).
    pub git_fg: Color,
    /// Git dirty indicator foreground.
    pub git_dirty_fg: Color,
    /// Duration foreground (normal).
    pub duration_fg: Color,
    /// Duration foreground (slow, >= 0.5s).
    pub duration_slow_fg: Color,
    /// Clock foreground.
    pub clock_fg: Color,
    /// Separator foreground.
    pub separator_fg: Color,

    // === Panel Colors (shared across all panels) ===
    /// Panel background.
    pub panel_bg: Color,
    /// Panel border.
    pub panel_border: Color,
    /// Header/title foreground.
    pub header_fg: Color,
    /// Primary text foreground.
    pub text_primary: Color,
    /// Secondary/muted text foreground.
    pub text_secondary: Color,
    /// Highlighted/emphasized text foreground.
    pub text_highlight: Color,
    /// Selected item background.
    pub selection_bg: Color,
    /// Selected item foreground.
    pub selection_fg: Color,

    // === Semantic Colors (kept distinct for safety) ===
    /// Success indicator (exit code 0).
    pub semantic_success: Color,
    /// Error indicator (non-zero exit, failures).
    pub semantic_error: Color,
    /// Warning indicator.
    pub semantic_warning: Color,
    /// Info indicator.
    pub semantic_info: Color,

    // === Git File Status Colors (file browser) ===
    /// Modified file marker color.
    pub git_modified_fg: Color,
    /// Added/staged file marker color.
    pub git_added_fg: Color,
    /// Deleted file marker color.
    pub git_deleted_fg: Color,
    /// Untracked file marker color.
    pub git_untracked_fg: Color,
    /// Conflict file marker color.
    pub git_conflict_fg: Color,
    /// Renamed file marker color.
    pub git_renamed_fg: Color,

    // === File Browser Specific ===
    /// Directory icon/name color.
    pub dir_color: Color,
    /// Regular file color.
    pub file_color: Color,
    /// File permissions color.
    pub permissions_color: Color,
    /// File size color.
    pub file_size_color: Color,
    /// File date color.
    pub file_date_color: Color,

    // === Tab Bar ===
    /// Active tab background.
    pub tab_active_bg: Color,
    /// Active tab foreground.
    pub tab_active_fg: Color,
    /// Inactive tab background.
    pub tab_inactive_bg: Color,
    /// Inactive tab foreground.
    pub tab_inactive_fg: Color,

    // === Scroll Viewer Colors ===
    /// Search match background (current match).
    pub search_current_bg: Color,
    /// Search match foreground (current match).
    pub search_current_fg: Color,
    /// Search match background (other matches).
    pub search_other_bg: Color,
    /// Search match foreground (other matches).
    pub search_other_fg: Color,
    /// URL foreground color.
    pub url_fg: Color,
    /// Selection background in yank mode.
    pub yank_selection_bg: Color,
    /// Selection foreground in yank mode.
    pub yank_selection_fg: Color,
    /// Timestamp gutter foreground.
    pub timestamp_fg: Color,
    /// Boundary marker foreground (BEGIN/END).
    pub marker_fg: Color,
    /// Help bar background.
    pub help_bar_bg: Color,
    /// Help bar foreground.
    pub help_bar_fg: Color,
    /// Help bar key highlight.
    pub help_bar_key: Color,
}

impl Theme {
    /// Returns the appropriate theme based on preset.
    pub fn for_preset(preset: ThemePreset) -> &'static Self {
        match preset {
            ThemePreset::Amber => &AMBER_THEME,
            ThemePreset::Terminal => &TERMINAL_THEME,
        }
    }
}

/// Amber monochrome theme.
///
/// Hardcoded RGB colors for a vintage VT220 terminal aesthetic.
/// Warm orange/amber tones on pure black background.
pub static AMBER_THEME: Theme = Theme {
    // Context Bar
    bar_bg: Color::Rgb(0, 0, 0),               // Pure black
    segment_bg: Color::Rgb(51, 34, 0),         // #332200 - dark amber
    success_fg: Color::Rgb(255, 176, 0),       // #ffb000 - classic amber
    failure_fg: Color::Rgb(255, 102, 0),       // #ff6600 - orange-red
    cwd_fg: Color::Rgb(255, 215, 0),           // #ffd700 - bright amber
    git_fg: Color::Rgb(204, 136, 0),           // #cc8800 - medium amber
    git_dirty_fg: Color::Rgb(255, 140, 0),     // #ff8c00 - dark orange
    duration_fg: Color::Rgb(255, 176, 0),      // #ffb000 - amber
    duration_slow_fg: Color::Rgb(255, 102, 0), // #ff6600 - orange warning
    clock_fg: Color::Rgb(153, 102, 0),         // #996600 - dim amber
    separator_fg: Color::Rgb(255, 176, 0),     // #ffb000 - amber

    // Panels
    panel_bg: Color::Rgb(0, 0, 0),           // Pure black
    panel_border: Color::Rgb(102, 68, 0),    // #664400 - dark amber border
    header_fg: Color::Rgb(255, 215, 0),      // #ffd700 - bright amber
    text_primary: Color::Rgb(255, 176, 0),   // #ffb000 - standard amber
    text_secondary: Color::Rgb(153, 102, 0), // #996600 - dim amber
    text_highlight: Color::Rgb(255, 215, 0), // #ffd700 - bright amber
    selection_bg: Color::Rgb(51, 34, 0),     // #332200 - dark amber
    selection_fg: Color::Rgb(255, 215, 0),   // #ffd700 - bright amber

    // Semantic (slightly distinct for safety/visibility)
    semantic_success: Color::Rgb(0, 170, 0), // #00aa00 - green
    semantic_error: Color::Rgb(255, 68, 0),  // #ff4400 - red-orange
    semantic_warning: Color::Rgb(255, 136, 0), // #ff8800 - orange
    semantic_info: Color::Rgb(255, 176, 0),  // #ffb000 - amber

    // Git File Status
    git_modified_fg: Color::Rgb(255, 140, 0), // #ff8c00 - dark orange
    git_added_fg: Color::Rgb(0, 170, 0),      // #00aa00 - green (same as semantic_success)
    git_deleted_fg: Color::Rgb(255, 68, 0),   // #ff4400 - red-orange (same as semantic_error)
    git_untracked_fg: Color::Rgb(153, 102, 0), // #996600 - dim amber (same as text_secondary)
    git_conflict_fg: Color::Rgb(255, 34, 0),  // #ff2200 - bright red
    git_renamed_fg: Color::Rgb(204, 136, 0),  // #cc8800 - medium amber

    // File Browser
    dir_color: Color::Rgb(255, 215, 0),  // #ffd700 - bright amber
    file_color: Color::Rgb(204, 153, 0), // #cc9900 - medium amber
    permissions_color: Color::Rgb(204, 136, 0), // #cc8800 - medium amber
    file_size_color: Color::Rgb(153, 102, 0), // #996600 - dim amber
    file_date_color: Color::Rgb(204, 136, 0), // #cc8800 - medium amber

    // Tabs
    tab_active_bg: Color::Rgb(255, 176, 0), // #ffb000 - amber
    tab_active_fg: Color::Rgb(0, 0, 0),     // Black text on amber
    tab_inactive_bg: Color::Rgb(51, 34, 0), // #332200 - dark amber
    tab_inactive_fg: Color::Rgb(153, 102, 0), // #996600 - dim amber

    // Scroll Viewer
    search_current_bg: Color::Rgb(255, 215, 0), // #ffd700 - bright amber highlight
    search_current_fg: Color::Rgb(0, 0, 0),     // Black text on highlight
    search_other_bg: Color::Rgb(102, 68, 0),    // #664400 - dim amber
    search_other_fg: Color::Rgb(255, 215, 0),   // #ffd700 - bright amber
    url_fg: Color::Rgb(100, 180, 255),          // Light blue (stands out on amber)
    yank_selection_bg: Color::Rgb(51, 34, 0),   // #332200 - dark amber
    yank_selection_fg: Color::Rgb(255, 215, 0), // #ffd700 - bright amber
    timestamp_fg: Color::Rgb(153, 102, 0),      // #996600 - dim amber
    marker_fg: Color::Rgb(204, 136, 0),         // #cc8800 - medium amber
    help_bar_bg: Color::Rgb(51, 34, 0),         // #332200 - dark amber
    help_bar_fg: Color::Rgb(204, 153, 0),       // #cc9900 - medium amber
    help_bar_key: Color::Rgb(255, 215, 0),      // #ffd700 - bright amber
};

/// Terminal-native theme.
///
/// Uses standard ANSI colors, allowing the terminal emulator
/// (Konsole, iTerm2, etc.) to control the actual appearance
/// via its color scheme settings.
///
/// Design principles:
/// - Avoids `DarkGray` (ANSI 8) as background — in Solarized it maps to
///   base03 (the background itself), making elements invisible.
/// - Avoids `Black` (ANSI 0) as foreground on colored backgrounds — in
///   Solarized it maps to base02, nearly indistinguishable from background.
/// - Uses `Blue` (ANSI 4) for selection/active highlights — universally
///   recognized as "selected" and always a distinct hue.
/// - Uses `Gray` (ANSI 7) instead of `DarkGray` for dim/secondary text —
///   visible in all themes, subtle but readable.
pub static TERMINAL_THEME: Theme = Theme {
    // Context Bar
    bar_bg: Color::Reset,              // Terminal default
    segment_bg: Color::Reset,          // Transparent (DarkGray is bg in Solarized)
    success_fg: Color::Green,          // ANSI 2
    failure_fg: Color::Red,            // ANSI 1
    cwd_fg: Color::Cyan,               // ANSI 6
    git_fg: Color::Magenta,            // ANSI 5
    git_dirty_fg: Color::LightMagenta, // ANSI 13
    duration_fg: Color::White,         // ANSI 15
    duration_slow_fg: Color::Yellow,   // ANSI 3
    clock_fg: Color::Gray,             // ANSI 7 (was DarkGray/8)
    separator_fg: Color::Gray,         // ANSI 7 (was DarkGray/8)

    // Panels
    panel_bg: Color::Reset,           // Terminal default
    panel_border: Color::Gray,        // ANSI 7 (was DarkGray/8)
    header_fg: Color::Cyan,           // ANSI 6
    text_primary: Color::White,       // ANSI 15
    text_secondary: Color::Gray,      // ANSI 7 (was DarkGray/8)
    text_highlight: Color::LightCyan, // ANSI 14
    selection_bg: Color::Blue,        // ANSI 4 — universal selection color
    selection_fg: Color::White,       // ANSI 15

    // Semantic
    semantic_success: Color::Green,  // ANSI 2
    semantic_error: Color::Red,      // ANSI 1
    semantic_warning: Color::Yellow, // ANSI 3
    semantic_info: Color::Cyan,      // ANSI 6

    // Git File Status
    git_modified_fg: Color::Yellow,   // ANSI 3
    git_added_fg: Color::Green,       // ANSI 2
    git_deleted_fg: Color::Red,       // ANSI 1
    git_untracked_fg: Color::Gray,    // ANSI 7 (was DarkGray/8)
    git_conflict_fg: Color::LightRed, // ANSI 9
    git_renamed_fg: Color::Magenta,   // ANSI 5

    // File Browser
    dir_color: Color::Blue,            // ANSI 4
    file_color: Color::White,          // ANSI 15
    permissions_color: Color::Magenta, // ANSI 5
    file_size_color: Color::Gray,      // ANSI 7 (was DarkGray/8)
    file_date_color: Color::Cyan,      // ANSI 6

    // Tabs
    tab_active_bg: Color::Blue,    // ANSI 4 — universal active highlight
    tab_active_fg: Color::White,   // ANSI 15 (was Black/0, invisible in Solarized)
    tab_inactive_bg: Color::Reset, // Transparent (was DarkGray/8)
    tab_inactive_fg: Color::Gray,  // ANSI 7 — dimmer than active White

    // Scroll Viewer
    search_current_bg: Color::Yellow,  // ANSI 3
    search_current_fg: Color::Black,   // ANSI 0 — dark on bright yellow works in most themes
    search_other_bg: Color::Cyan,      // ANSI 6 — distinct from Blue selection
    search_other_fg: Color::White,     // ANSI 15
    url_fg: Color::LightBlue,          // ANSI 12
    yank_selection_bg: Color::Magenta, // ANSI 5 — distinct from Blue selection
    yank_selection_fg: Color::White,   // ANSI 15
    timestamp_fg: Color::Gray,         // ANSI 7 (was DarkGray/8)
    marker_fg: Color::Cyan,            // ANSI 6
    help_bar_bg: Color::Reset,         // Transparent (was DarkGray/8)
    help_bar_fg: Color::White,         // ANSI 15
    help_bar_key: Color::Cyan,         // ANSI 6 (was LightCyan/14, which is base1 in Solarized)
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_amber_theme_has_distinct_colors() {
        // Success and failure should be visually distinct
        assert_ne!(AMBER_THEME.success_fg, AMBER_THEME.failure_fg);

        // Primary and secondary text should differ
        assert_ne!(AMBER_THEME.text_primary, AMBER_THEME.text_secondary);
    }

    #[test]
    fn test_terminal_theme_uses_ansi_colors() {
        // Terminal theme should use standard ANSI colors, not RGB
        assert!(matches!(TERMINAL_THEME.success_fg, Color::Green));
        assert!(matches!(TERMINAL_THEME.failure_fg, Color::Red));
        assert!(matches!(TERMINAL_THEME.cwd_fg, Color::Cyan));
    }

    #[test]
    fn test_for_preset() {
        let amber = Theme::for_preset(ThemePreset::Amber);
        let terminal = Theme::for_preset(ThemePreset::Terminal);

        // Should return different themes
        assert_ne!(amber.success_fg, terminal.success_fg);
    }
}
