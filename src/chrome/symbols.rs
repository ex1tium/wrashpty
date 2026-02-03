//! Symbol sets for UI rendering.
//!
//! Provides abstractions for icons and symbols used throughout the UI,
//! with both Nerd Font and fallback (standard Unicode) variants.

use crate::config::SymbolSet;

/// Icons and symbols for the context bar and panels.
#[derive(Debug, Clone, Copy)]
pub struct Symbols {
    // Status indicators
    /// Success indicator (checkmark).
    pub success: &'static str,
    /// Failure indicator (cross).
    pub failure: &'static str,

    // Git symbols
    /// Git branch icon.
    pub git_branch: &'static str,
    /// Git dirty/modified indicator.
    pub git_dirty: &'static str,

    // Directory/path
    /// Folder/directory icon.
    pub folder: &'static str,
    /// Home directory icon.
    pub home: &'static str,

    // Time
    /// Clock icon.
    pub clock: &'static str,
    /// Stopwatch/timer icon.
    pub stopwatch: &'static str,

    // Separators (powerline-style)
    /// Right-pointing separator.
    pub separator_right: &'static str,
    /// Left-pointing separator.
    pub separator_left: &'static str,
    /// Thin/soft separator.
    pub separator_thin: &'static str,

    // File types
    /// File icon.
    pub file: &'static str,
    /// Executable/script icon.
    pub executable: &'static str,
    /// Link/symlink icon.
    pub link: &'static str,

    // Prompt
    /// Prompt chevron/arrow.
    pub prompt_chevron: &'static str,

    // Misc
    /// Search/filter icon.
    pub search: &'static str,
    /// History icon.
    pub history: &'static str,
    /// Help/info icon.
    pub help: &'static str,
}

impl Symbols {
    /// Returns the appropriate symbol set based on configuration.
    pub fn for_set(set: SymbolSet) -> &'static Self {
        match set {
            SymbolSet::NerdFont => &NERD_FONT_SYMBOLS,
            SymbolSet::Fallback => &FALLBACK_SYMBOLS,
        }
    }
}

/// Nerd Font symbol set.
///
/// Requires a Nerd Font to be installed and configured in the terminal.
/// Codepoints from: <https://www.nerdfonts.com/cheat-sheet>
pub static NERD_FONT_SYMBOLS: Symbols = Symbols {
    // Status (Font Awesome)
    success: "\u{f00c}", //  (fa-check)
    failure: "\u{f00d}", //  (fa-times)

    // Git (Devicons/Octicons)
    git_branch: "\u{e725}", //  (git-branch)
    git_dirty: "\u{f069}",  //  (fa-asterisk)

    // Directory (Font Awesome)
    folder: "\u{f07b}", //  (fa-folder)
    home: "\u{f015}",   //  (fa-home)

    // Time (Font Awesome)
    clock: "\u{f017}",     //  (fa-clock-o)
    stopwatch: "\u{f252}", //  (fa-hourglass-half)

    // Powerline separators
    separator_right: "\u{e0b0}", //  (powerline right)
    separator_left: "\u{e0b2}",  //  (powerline left)
    separator_thin: "\u{e0b1}",  //  (powerline right thin)

    // File types (Seti-UI / Custom)
    file: "\u{f15b}",       //  (fa-file)
    executable: "\u{f489}", //  (terminal)
    link: "\u{f0c1}",       //  (fa-link)

    // Prompt
    prompt_chevron: "\u{e0b0}", //  (same as separator)

    // Misc
    search: "\u{f002}",  //  (fa-search)
    history: "\u{f1da}", //  (fa-history)
    help: "\u{f059}",    //  (fa-question-circle)
};

/// Fallback symbol set using basic Unicode.
///
/// These symbols work in virtually all terminals and fonts.
pub static FALLBACK_SYMBOLS: Symbols = Symbols {
    // Status
    success: "\u{2713}", // ✓ (check mark)
    failure: "\u{2717}", // ✗ (ballot x)

    // Git
    git_branch: "",    // No icon, text-only
    git_dirty: "\u{25cf}", // ● (black circle)

    // Directory
    folder: "",      // No icon
    home: "~",       // Tilde for home

    // Time
    clock: "",     // No icon
    stopwatch: "", // No icon

    // Separators (standard Unicode triangles)
    separator_right: "\u{25b6}", // ▶ (right-pointing triangle)
    separator_left: "\u{25c0}",  // ◀ (left-pointing triangle)
    separator_thin: "|",         // Simple pipe

    // File types
    file: "",       // No icon
    executable: "", // No icon
    link: "@",      // @ for symlinks (like ls)

    // Prompt
    prompt_chevron: ">", // Simple chevron

    // Misc
    search: "",  // No icon
    history: "", // No icon
    help: "?",   // Question mark
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nerd_font_symbols_non_empty() {
        // Key symbols should be non-empty in nerdfont set
        assert!(!NERD_FONT_SYMBOLS.success.is_empty());
        assert!(!NERD_FONT_SYMBOLS.failure.is_empty());
        assert!(!NERD_FONT_SYMBOLS.git_branch.is_empty());
        assert!(!NERD_FONT_SYMBOLS.separator_right.is_empty());
    }

    #[test]
    fn test_fallback_status_symbols() {
        // Status symbols must always be present in fallback
        assert!(!FALLBACK_SYMBOLS.success.is_empty());
        assert!(!FALLBACK_SYMBOLS.failure.is_empty());
    }

    #[test]
    fn test_for_set() {
        let nerd = Symbols::for_set(SymbolSet::NerdFont);
        let fallback = Symbols::for_set(SymbolSet::Fallback);

        // They should point to different static instances
        assert_ne!(nerd.success, fallback.success);
    }
}
