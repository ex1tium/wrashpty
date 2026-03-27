//! Unified glyph library with four capability tiers.
//!
//! Provides a single `GlyphSet` struct composed of category sub-structs
//! (tree connectors, borders, separators, indicators, icons, etc.) with
//! four static instances — ASCII, Unicode, Emoji, NerdFont — selectable
//! at runtime via `GlyphTier`.

use crate::git::GitFileStatus;

// ---------------------------------------------------------------------------
// GlyphTier enum
// ---------------------------------------------------------------------------

/// Glyph capability tier, from least to most capable.
///
/// Each tier is a strict superset of the one before in terms of terminal
/// requirements:
/// - `Ascii` works on any terminal, including serial consoles.
/// - `Unicode` requires a modern terminal with Unicode box-drawing support.
/// - `Emoji` requires emoji rendering (most modern terminals).
/// - `NerdFont` requires a patched Nerd Font installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlyphTier {
    /// Pure 7-bit ASCII — works on any terminal including serial.
    Ascii,
    /// Unicode box-drawing + geometric shapes — works in all modern terminals.
    #[default]
    Unicode,
    /// Unicode emoji — requires emoji rendering support.
    Emoji,
    /// Nerd Font patched glyphs — requires Nerd Font installation.
    NerdFont,
}

impl GlyphTier {
    /// Cycles to the next tier (wraps around).
    pub fn next(self) -> Self {
        match self {
            Self::Ascii => Self::Unicode,
            Self::Unicode => Self::Emoji,
            Self::Emoji => Self::NerdFont,
            Self::NerdFont => Self::Ascii,
        }
    }

    /// Cycles to the previous tier (wraps around).
    pub fn prev(self) -> Self {
        match self {
            Self::Ascii => Self::NerdFont,
            Self::Unicode => Self::Ascii,
            Self::Emoji => Self::Unicode,
            Self::NerdFont => Self::Emoji,
        }
    }

    /// Short human-readable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ascii => "ASCII",
            Self::Unicode => "Unicode",
            Self::Emoji => "Emoji",
            Self::NerdFont => "NerdFont",
        }
    }

    /// Parses a tier from a persisted label string.
    pub fn from_label(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "ascii" => Some(Self::Ascii),
            "unicode" => Some(Self::Unicode),
            "emoji" => Some(Self::Emoji),
            "nerdfont" | "nerd" => Some(Self::NerdFont),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-struct definitions
// ---------------------------------------------------------------------------

/// Tree connector characters for hierarchical views.
#[derive(Debug, Clone, Copy)]
pub struct TreeGlyphs {
    /// Vertical guide rail: `│` or `|`.
    pub vertical: &'static str,
    /// Branch connector (non-last sibling): `├` or `|`.
    pub branch: &'static str,
    /// Corner connector (last sibling): `└` or `` ` ``.
    pub corner: &'static str,
    /// Horizontal connector: `─` or `-`.
    pub horizontal: &'static str,
    /// Expanded indicator: `▾` or `v`.
    pub expanded: &'static str,
    /// Collapsed indicator: `▸` or `>`.
    pub collapsed: &'static str,
    /// Cross junction: `┼` or `+`.
    pub cross: &'static str,
    /// Tee pointing right: `├` or `|`.
    pub tee_right: &'static str,
    /// Tee pointing down: `┬` or `-`.
    pub tee_down: &'static str,
}

/// Box-drawing and border characters.
#[derive(Debug, Clone, Copy)]
pub struct BorderGlyphs {
    /// Horizontal line: `─` or `-`.
    pub horizontal: char,
    /// Vertical line: `│` or `|`.
    pub vertical: char,
    /// Top-left corner: `┌` or `+`.
    pub top_left: char,
    /// Top-right corner: `┐` or `+`.
    pub top_right: char,
    /// Bottom-left corner: `└` or `+`.
    pub bottom_left: char,
    /// Bottom-right corner: `┘` or `+`.
    pub bottom_right: char,
    /// Left tee: `├` or `|`.
    pub tee_left: char,
    /// Right tee: `┤` or `|`.
    pub tee_right: char,
    /// Top tee: `┬` or `-`.
    pub tee_top: char,
    /// Bottom tee: `┴` or `-`.
    pub tee_bottom: char,
    /// Cross junction: `┼` or `+`.
    pub cross: char,
    /// Bold horizontal: `━` or `=`.
    pub horizontal_bold: char,
    /// Bold vertical: `┃` or `|`.
    pub vertical_bold: char,
    /// Rounded top-left: `╭` or `.`.
    pub rounded_tl: char,
    /// Rounded top-right: `╮` or `.`.
    pub rounded_tr: char,
    /// Rounded bottom-left: `╰` or `` ` ``.
    pub rounded_bl: char,
    /// Rounded bottom-right: `╯` or `'`.
    pub rounded_br: char,
}

/// Separator and divider characters.
#[derive(Debug, Clone, Copy)]
pub struct SeparatorGlyphs {
    /// Dashed horizontal line: `╌` or `-`.
    pub dash: char,
    /// Dot separator: `···` or `...`.
    pub dot: &'static str,
    /// Powerline right arrow.
    pub powerline_right: &'static str,
    /// Powerline left arrow.
    pub powerline_left: &'static str,
    /// Powerline thin separator.
    pub powerline_thin: &'static str,
}

/// Status and state indicator symbols.
#[derive(Debug, Clone, Copy)]
pub struct IndicatorGlyphs {
    /// Success/check: `✓` or `+`.
    pub success: &'static str,
    /// Failure/cross: `✗` or `x`.
    pub failure: &'static str,
    /// Warning: `⚠` or `!`.
    pub warning: &'static str,
    /// Information: `ℹ` or `i`.
    pub info: &'static str,
    /// Filled dot: `●` or `*`.
    pub dot_filled: &'static str,
    /// Empty dot: `○` or `o`.
    pub dot_empty: &'static str,
    /// Half dot: `◐` or `*`.
    pub dot_half: &'static str,
    /// Star: `★` or `*`.
    pub star: &'static str,
    /// Diamond: `◆` or `*`.
    pub diamond: &'static str,
    /// Checked box: `☑` or `[x]`.
    pub check_box: &'static str,
    /// Empty box: `☐` or `[ ]`.
    pub empty_box: &'static str,
    /// Lock indicator: `ˡ` or `L`.
    pub lock: &'static str,
}

/// Progress bar characters.
#[derive(Debug, Clone, Copy)]
pub struct ProgressGlyphs {
    /// Eight sub-character blocks for smooth progress bars.
    /// Index 0 = thinnest (▏), index 7 = full block (█).
    pub bar: [char; 8],
    /// Light shade: `░` or `.`.
    pub shade_light: char,
    /// Medium shade: `▒` or `:`.
    pub shade_medium: char,
    /// Heavy shade: `▓` or `#`.
    pub shade_heavy: char,
    /// Full block: `█` or `#`.
    pub block_full: char,
}

/// Navigation arrows and directional indicators.
#[derive(Debug, Clone, Copy)]
pub struct NavGlyphs {
    /// Right arrow: `→` or `>`.
    pub arrow_right: &'static str,
    /// Left arrow: `←` or `<`.
    pub arrow_left: &'static str,
    /// Up arrow: `↑` or `^`.
    pub arrow_up: &'static str,
    /// Down arrow: `↓` or `v`.
    pub arrow_down: &'static str,
    /// Right chevron: `❯` or `>`.
    pub chevron_right: &'static str,
    /// Left chevron: `❮` or `<`.
    pub chevron_left: &'static str,
    /// Right triangle: `▶` or `>`.
    pub triangle_right: &'static str,
    /// Left triangle: `◀` or `<`.
    pub triangle_left: &'static str,
    /// Up triangle: `▲` or `^`.
    pub triangle_up: &'static str,
    /// Down triangle: `▼` or `v`.
    pub triangle_down: &'static str,
    /// Ellipsis: `…` or `...`.
    pub ellipsis: &'static str,
}

/// Bullet point styles.
#[derive(Debug, Clone, Copy)]
pub struct BulletGlyphs {
    /// Disc bullet: `•` or `*`.
    pub disc: &'static str,
    /// Circle bullet: `◦` or `o`.
    pub circle: &'static str,
    /// Square bullet: `▪` or `-`.
    pub square: &'static str,
    /// Dash bullet: `‣` or `-`.
    pub dash: &'static str,
    /// Arrow bullet: `‣` or `>`.
    pub arrow: &'static str,
}

/// Icon glyphs for UI elements.
///
/// The ASCII and Unicode tiers use empty strings for most icons, relying on
/// textual context. The Emoji tier uses standard Unicode emoji. The NerdFont
/// tier uses Nerd Font patched glyphs from the Private Use Area.
#[derive(Debug, Clone, Copy)]
pub struct IconGlyphs {
    // Git
    /// Git branch icon.
    pub git_branch: &'static str,
    /// Git dirty/modified indicator.
    pub git_dirty: &'static str,
    /// Git modified file marker.
    pub git_modified: &'static str,
    /// Git added file marker.
    pub git_added: &'static str,
    /// Git deleted file marker.
    pub git_deleted: &'static str,
    /// Git untracked file marker.
    pub git_untracked: &'static str,
    /// Git conflict file marker.
    pub git_conflict: &'static str,
    /// Git renamed file marker.
    pub git_renamed: &'static str,

    // Files
    /// Folder/directory icon.
    pub folder: &'static str,
    /// Regular file icon.
    pub file: &'static str,
    /// Executable/script icon.
    pub executable: &'static str,
    /// Link/symlink icon.
    pub link: &'static str,
    /// Home directory icon.
    pub home: &'static str,

    // UI
    /// Search/filter icon.
    pub search: &'static str,
    /// History icon.
    pub history: &'static str,
    /// Help/info icon.
    pub help: &'static str,
    /// Clock icon.
    pub clock: &'static str,
    /// Stopwatch/timer icon.
    pub stopwatch: &'static str,
    /// Prompt chevron/arrow.
    pub prompt: &'static str,
}

impl IconGlyphs {
    /// Returns the status marker string for a git file status.
    pub fn git_status_marker(&self, status: GitFileStatus) -> &'static str {
        match status {
            GitFileStatus::Modified => self.git_modified,
            GitFileStatus::Added => self.git_added,
            GitFileStatus::Deleted => self.git_deleted,
            GitFileStatus::Untracked => self.git_untracked,
            GitFileStatus::Conflict => self.git_conflict,
            GitFileStatus::Renamed => self.git_renamed,
        }
    }
}

// ---------------------------------------------------------------------------
// GlyphSet
// ---------------------------------------------------------------------------

/// Complete set of glyphs organized by category.
#[derive(Debug, Clone, Copy)]
pub struct GlyphSet {
    /// Tree connector characters for hierarchical views.
    pub tree: TreeGlyphs,
    /// Box-drawing and border characters.
    pub border: BorderGlyphs,
    /// Separator and divider characters.
    pub separator: SeparatorGlyphs,
    /// Status and state indicators.
    pub indicator: IndicatorGlyphs,
    /// Progress bar characters.
    pub progress: ProgressGlyphs,
    /// Navigation arrows and directional indicators.
    pub nav: NavGlyphs,
    /// Bullet point styles.
    pub bullet: BulletGlyphs,
    /// Icon glyphs for UI elements.
    pub icon: IconGlyphs,
}

impl GlyphSet {
    /// Returns the static glyph set for a given tier.
    pub fn for_tier(tier: GlyphTier) -> &'static Self {
        match tier {
            GlyphTier::Ascii => &ASCII_GLYPHS,
            GlyphTier::Unicode => &UNICODE_GLYPHS,
            GlyphTier::Emoji => &EMOJI_GLYPHS,
            GlyphTier::NerdFont => &NERD_FONT_GLYPHS,
        }
    }
}

// ---------------------------------------------------------------------------
// Tier 1: ASCII — pure 7-bit ASCII, serial-safe
// ---------------------------------------------------------------------------

pub static ASCII_GLYPHS: GlyphSet = GlyphSet {
    tree: TreeGlyphs {
        vertical: "|",
        branch: "|",
        corner: "`",
        horizontal: "-",
        expanded: "v",
        collapsed: ">",
        cross: "+",
        tee_right: "|",
        tee_down: "-",
    },
    border: BorderGlyphs {
        horizontal: '-',
        vertical: '|',
        top_left: '+',
        top_right: '+',
        bottom_left: '+',
        bottom_right: '+',
        tee_left: '|',
        tee_right: '|',
        tee_top: '-',
        tee_bottom: '-',
        cross: '+',
        horizontal_bold: '=',
        vertical_bold: '|',
        rounded_tl: '.',
        rounded_tr: '.',
        rounded_bl: '`',
        rounded_br: '\'',
    },
    separator: SeparatorGlyphs {
        dash: '-',
        dot: "...",
        powerline_right: ">",
        powerline_left: "<",
        powerline_thin: "|",
    },
    indicator: IndicatorGlyphs {
        success: "+",
        failure: "x",
        warning: "!",
        info: "i",
        dot_filled: "*",
        dot_empty: "o",
        dot_half: "*",
        star: "*",
        diamond: "*",
        check_box: "[x]",
        empty_box: "[ ]",
        lock: "L",
    },
    progress: ProgressGlyphs {
        bar: ['-', '-', '-', '-', '#', '#', '#', '#'],
        shade_light: '.',
        shade_medium: ':',
        shade_heavy: '#',
        block_full: '#',
    },
    nav: NavGlyphs {
        arrow_right: ">",
        arrow_left: "<",
        arrow_up: "^",
        arrow_down: "v",
        chevron_right: ">",
        chevron_left: "<",
        triangle_right: ">",
        triangle_left: "<",
        triangle_up: "^",
        triangle_down: "v",
        ellipsis: "...",
    },
    bullet: BulletGlyphs {
        disc: "*",
        circle: "o",
        square: "-",
        dash: "-",
        arrow: ">",
    },
    icon: IconGlyphs {
        git_branch: "",
        git_dirty: "*",
        git_modified: "*",
        git_added: "+",
        git_deleted: "x",
        git_untracked: "?",
        git_conflict: "!",
        git_renamed: "r",
        folder: "[D]",
        file: "[F]",
        executable: "[X]",
        link: "@",
        home: "~",
        search: "",
        history: "",
        help: "?",
        clock: "",
        stopwatch: "",
        prompt: ">",
    },
};

// ---------------------------------------------------------------------------
// Tier 2: Unicode — box-drawing + geometric shapes, modern terminals
// ---------------------------------------------------------------------------

pub static UNICODE_GLYPHS: GlyphSet = GlyphSet {
    tree: TreeGlyphs {
        vertical: "│",
        branch: "├",
        corner: "└",
        horizontal: "─",
        expanded: "▾",
        collapsed: "▸",
        cross: "┼",
        tee_right: "├",
        tee_down: "┬",
    },
    border: BorderGlyphs {
        horizontal: '─',
        vertical: '│',
        top_left: '┌',
        top_right: '┐',
        bottom_left: '└',
        bottom_right: '┘',
        tee_left: '├',
        tee_right: '┤',
        tee_top: '┬',
        tee_bottom: '┴',
        cross: '┼',
        horizontal_bold: '━',
        vertical_bold: '┃',
        rounded_tl: '╭',
        rounded_tr: '╮',
        rounded_bl: '╰',
        rounded_br: '╯',
    },
    separator: SeparatorGlyphs {
        dash: '╌',
        dot: "···",
        powerline_right: "\u{25b6}", // ▶
        powerline_left: "\u{25c0}",  // ◀
        powerline_thin: "|",
    },
    indicator: IndicatorGlyphs {
        success: "\u{2713}",    // ✓
        failure: "\u{2717}",    // ✗
        warning: "\u{26a0}",    // ⚠
        info: "\u{2139}",       // ℹ
        dot_filled: "\u{25cf}", // ●
        dot_empty: "\u{25cb}",  // ○
        dot_half: "\u{25d0}",   // ◐
        star: "\u{2605}",       // ★
        diamond: "\u{25c6}",    // ◆
        check_box: "\u{2611}",  // ☑
        empty_box: "\u{2610}",  // ☐
        lock: "\u{02E1}",       // ˡ (modifier letter small l)
    },
    progress: ProgressGlyphs {
        bar: ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'],
        shade_light: '░',
        shade_medium: '▒',
        shade_heavy: '▓',
        block_full: '█',
    },
    nav: NavGlyphs {
        arrow_right: "→",
        arrow_left: "←",
        arrow_up: "↑",
        arrow_down: "↓",
        chevron_right: "❯",
        chevron_left: "❮",
        triangle_right: "▶",
        triangle_left: "◀",
        triangle_up: "▲",
        triangle_down: "▼",
        ellipsis: "…",
    },
    bullet: BulletGlyphs {
        disc: "•",
        circle: "◦",
        square: "▪",
        dash: "‣",
        arrow: "‣",
    },
    icon: IconGlyphs {
        git_branch: "",
        git_dirty: "\u{25cf}",    // ●
        git_modified: "\u{25cf}", // ●
        git_added: "+",
        git_deleted: "x",
        git_untracked: "?",
        git_conflict: "!",
        git_renamed: "r",
        folder: "\u{1f5c0}",    // 🗀 (file folder, text presentation)
        file: "\u{1f5ce}",      // 🗎 (document, text presentation)
        executable: "\u{2699}", // ⚙ (gear)
        link: "\u{2192}",       // → (rightwards arrow)
        home: "~",
        search: "",
        history: "",
        help: "?",
        clock: "",
        stopwatch: "",
        prompt: ">",
    },
};

// ---------------------------------------------------------------------------
// Tier 3: Emoji — standard Unicode emoji, most modern terminals
// ---------------------------------------------------------------------------

pub static EMOJI_GLYPHS: GlyphSet = GlyphSet {
    // Tree and border share the Unicode tier (emoji doesn't improve these)
    tree: UNICODE_GLYPHS.tree,
    border: UNICODE_GLYPHS.border,
    separator: SeparatorGlyphs {
        dash: '╌',
        dot: "···",
        powerline_right: "▶",
        powerline_left: "◀",
        powerline_thin: "|",
    },
    indicator: IndicatorGlyphs {
        success: "✅",
        failure: "❌",
        warning: "⚠\u{fe0f}", // ⚠️ (with emoji presentation selector)
        info: "ℹ\u{fe0f}",    // ℹ️
        dot_filled: "🔵",
        dot_empty: "⚪",
        dot_half: "🔘",
        star: "⭐",
        diamond: "💠",
        check_box: "☑\u{fe0f}", // ☑️
        empty_box: "☐",
        lock: "\u{1f512}", // 🔒
    },
    progress: UNICODE_GLYPHS.progress,
    nav: NavGlyphs {
        arrow_right: "➡\u{fe0f}",
        arrow_left: "⬅\u{fe0f}",
        arrow_up: "⬆\u{fe0f}",
        arrow_down: "⬇\u{fe0f}",
        chevron_right: "❯",
        chevron_left: "❮",
        triangle_right: "▶\u{fe0f}",
        triangle_left: "◀\u{fe0f}",
        triangle_up: "🔼",
        triangle_down: "🔽",
        ellipsis: "…",
    },
    bullet: UNICODE_GLYPHS.bullet,
    icon: IconGlyphs {
        git_branch: "🔀",
        git_dirty: "✏\u{fe0f}",
        git_modified: "\u{25cf}", // ●
        git_added: "✚",
        git_deleted: "✖",
        git_untracked: "?",
        git_conflict: "!",
        git_renamed: "→",
        folder: "📁",
        file: "📄",
        executable: "⚡",
        link: "🔗",
        home: "🏠",
        search: "🔍",
        history: "📜",
        help: "❓",
        clock: "🕐",
        stopwatch: "⏱\u{fe0f}",
        prompt: "❯",
    },
};

// ---------------------------------------------------------------------------
// Tier 4: NerdFont — patched font glyphs from the Private Use Area
// ---------------------------------------------------------------------------

pub static NERD_FONT_GLYPHS: GlyphSet = GlyphSet {
    tree: UNICODE_GLYPHS.tree,
    border: UNICODE_GLYPHS.border,
    separator: SeparatorGlyphs {
        dash: '╌',
        dot: "···",
        powerline_right: "\u{e0b0}", //  (powerline right)
        powerline_left: "\u{e0b2}",  //  (powerline left)
        powerline_thin: "\u{e0b1}",  //  (powerline right thin)
    },
    indicator: IndicatorGlyphs {
        success: "\u{f00c}",    //  (fa-check)
        failure: "\u{f00d}",    //  (fa-times)
        warning: "\u{f071}",    //  (fa-exclamation-triangle)
        info: "\u{f05a}",       //  (fa-info-circle)
        dot_filled: "\u{f111}", //  (fa-circle)
        dot_empty: "\u{f10c}",  //  (fa-circle-thin)
        dot_half: "\u{f042}",   //  (fa-adjust)
        star: "\u{f005}",       //  (fa-star)
        diamond: "\u{f219}",    //  (fa-diamond)
        check_box: "\u{f046}",  //  (fa-check-square-o)
        empty_box: "\u{f096}",  //  (fa-square-o)
        lock: "\u{f023}",       //  (fa-lock)
    },
    progress: UNICODE_GLYPHS.progress,
    nav: NavGlyphs {
        arrow_right: "\u{f061}",    //  (fa-arrow-right)
        arrow_left: "\u{f060}",     //  (fa-arrow-left)
        arrow_up: "\u{f062}",       //  (fa-arrow-up)
        arrow_down: "\u{f063}",     //  (fa-arrow-down)
        chevron_right: "\u{f054}",  //  (fa-chevron-right)
        chevron_left: "\u{f053}",   //  (fa-chevron-left)
        triangle_right: "\u{e0b0}", //  (powerline right)
        triangle_left: "\u{e0b2}",  //  (powerline left)
        triangle_up: "▲",
        triangle_down: "▼",
        ellipsis: "…",
    },
    bullet: BulletGlyphs {
        disc: "\u{f111}",   //  (fa-circle)
        circle: "\u{f10c}", //  (fa-circle-thin)
        square: "\u{f0c8}", //  (fa-square)
        dash: "\u{f101}",   //  (fa-angle-double-right)
        arrow: "\u{f054}",  //  (fa-chevron-right)
    },
    icon: IconGlyphs {
        git_branch: "\u{e725}",   //  (git-branch)
        git_dirty: "\u{f069}",    //  (fa-asterisk)
        git_modified: "\u{25cf}", // ● (black circle)
        git_added: "\u{271a}",    // ✚ (heavy greek cross)
        git_deleted: "\u{2716}",  // ✖ (heavy multiplication x)
        git_untracked: "?",
        git_conflict: "!",
        git_renamed: "\u{2192}", // → (rightwards arrow)
        folder: "\u{f07b}",      //  (fa-folder)
        file: "\u{f15b}",        //  (fa-file)
        executable: "\u{f489}",  //  (terminal)
        link: "\u{f0c1}",        //  (fa-link)
        home: "\u{f015}",        //  (fa-home)
        search: "\u{f002}",      //  (fa-search)
        history: "\u{f1da}",     //  (fa-history)
        help: "\u{f059}",        //  (fa-question-circle)
        clock: "\u{f017}",       //  (fa-clock-o)
        stopwatch: "\u{f252}",   //  (fa-hourglass-half)
        prompt: "\u{e0b0}",      //  (same as separator)
    },
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glyph_tier_default() {
        assert_eq!(GlyphTier::default(), GlyphTier::Unicode);
    }

    #[test]
    fn test_glyph_tier_next_cycles() {
        assert_eq!(GlyphTier::Ascii.next(), GlyphTier::Unicode);
        assert_eq!(GlyphTier::Unicode.next(), GlyphTier::Emoji);
        assert_eq!(GlyphTier::Emoji.next(), GlyphTier::NerdFont);
        assert_eq!(GlyphTier::NerdFont.next(), GlyphTier::Ascii);
    }

    #[test]
    fn test_glyph_tier_prev_cycles() {
        assert_eq!(GlyphTier::Ascii.prev(), GlyphTier::NerdFont);
        assert_eq!(GlyphTier::Unicode.prev(), GlyphTier::Ascii);
        assert_eq!(GlyphTier::Emoji.prev(), GlyphTier::Unicode);
        assert_eq!(GlyphTier::NerdFont.prev(), GlyphTier::Emoji);
    }

    #[test]
    fn test_glyph_tier_label() {
        assert_eq!(GlyphTier::Ascii.label(), "ASCII");
        assert_eq!(GlyphTier::Unicode.label(), "Unicode");
        assert_eq!(GlyphTier::Emoji.label(), "Emoji");
        assert_eq!(GlyphTier::NerdFont.label(), "NerdFont");
    }

    #[test]
    fn test_glyph_tier_from_label() {
        assert_eq!(GlyphTier::from_label("ascii"), Some(GlyphTier::Ascii));
        assert_eq!(GlyphTier::from_label("Unicode"), Some(GlyphTier::Unicode));
        assert_eq!(GlyphTier::from_label("EMOJI"), Some(GlyphTier::Emoji));
        assert_eq!(GlyphTier::from_label("NerdFont"), Some(GlyphTier::NerdFont));
        assert_eq!(GlyphTier::from_label("nerd"), Some(GlyphTier::NerdFont));
        assert_eq!(GlyphTier::from_label("bogus"), None);
    }

    #[test]
    fn test_glyph_tier_roundtrip() {
        for tier in [
            GlyphTier::Ascii,
            GlyphTier::Unicode,
            GlyphTier::Emoji,
            GlyphTier::NerdFont,
        ] {
            assert_eq!(GlyphTier::from_label(tier.label()), Some(tier));
        }
    }

    #[test]
    fn test_for_tier_returns_correct_set() {
        let ascii = GlyphSet::for_tier(GlyphTier::Ascii);
        assert_eq!(ascii.border.horizontal, '-');
        assert_eq!(ascii.tree.vertical, "|");
        assert_eq!(ascii.icon.folder, "[D]");
        assert_eq!(ascii.icon.file, "[F]");

        let unicode = GlyphSet::for_tier(GlyphTier::Unicode);
        assert_eq!(unicode.border.horizontal, '─');
        assert_eq!(unicode.tree.vertical, "│");
        assert_eq!(unicode.icon.folder, "\u{1f5c0}"); // 🗀
        assert_eq!(unicode.icon.file, "\u{1f5ce}"); // 🗎

        let emoji = GlyphSet::for_tier(GlyphTier::Emoji);
        assert_eq!(emoji.border.horizontal, '─'); // Same as unicode
        assert_eq!(emoji.icon.folder, "📁");

        let nerd = GlyphSet::for_tier(GlyphTier::NerdFont);
        assert_eq!(nerd.border.horizontal, '─');
        assert!(!nerd.icon.folder.is_empty());
        assert!(!nerd.icon.git_branch.is_empty());
    }

    #[test]
    fn test_ascii_tree_chars_match_legacy() {
        // Verify ASCII tree chars match the old ASCII_TREE_CHARS
        let g = &ASCII_GLYPHS;
        assert_eq!(g.tree.vertical, "|");
        assert_eq!(g.tree.branch, "|");
        assert_eq!(g.tree.corner, "`");
        assert_eq!(g.tree.horizontal, "-");
        assert_eq!(g.tree.expanded, "v");
        assert_eq!(g.tree.collapsed, ">");
    }

    #[test]
    fn test_unicode_tree_chars_match_legacy() {
        // Verify Unicode tree chars match the old UNICODE_TREE_CHARS
        let g = &UNICODE_GLYPHS;
        assert_eq!(g.tree.vertical, "│");
        assert_eq!(g.tree.branch, "├");
        assert_eq!(g.tree.corner, "└");
        assert_eq!(g.tree.horizontal, "─");
        assert_eq!(g.tree.expanded, "▾");
        assert_eq!(g.tree.collapsed, "▸");
    }

    #[test]
    fn test_nerdfont_icons_match_legacy() {
        // Verify NerdFont icons match the old NERD_FONT_SYMBOLS
        let g = &NERD_FONT_GLYPHS;
        assert_eq!(g.icon.git_branch, "\u{e725}");
        assert_eq!(g.icon.folder, "\u{f07b}");
        assert_eq!(g.icon.search, "\u{f002}");
        assert_eq!(g.separator.powerline_right, "\u{e0b0}");
        assert_eq!(g.separator.powerline_left, "\u{e0b2}");
        assert_eq!(g.separator.powerline_thin, "\u{e0b1}");
    }

    #[test]
    fn test_unicode_indicators_match_legacy_fallback() {
        // Verify Unicode indicators match the old FALLBACK_SYMBOLS
        let g = &UNICODE_GLYPHS;
        assert_eq!(g.indicator.success, "\u{2713}"); // ✓
        assert_eq!(g.indicator.failure, "\u{2717}"); // ✗
        assert_eq!(g.indicator.info, "\u{2139}"); // ℹ
        assert_eq!(g.indicator.warning, "\u{26a0}"); // ⚠
    }

    #[test]
    fn test_git_status_marker() {
        let icons = &UNICODE_GLYPHS.icon;
        assert_eq!(icons.git_status_marker(GitFileStatus::Modified), "\u{25cf}");
        assert_eq!(icons.git_status_marker(GitFileStatus::Added), "+");
        assert_eq!(icons.git_status_marker(GitFileStatus::Deleted), "x");
        assert_eq!(icons.git_status_marker(GitFileStatus::Untracked), "?");
        assert_eq!(icons.git_status_marker(GitFileStatus::Conflict), "!");
        assert_eq!(icons.git_status_marker(GitFileStatus::Renamed), "r");
    }

    #[test]
    fn test_nerd_git_status_marker() {
        let icons = &NERD_FONT_GLYPHS.icon;
        assert_eq!(icons.git_status_marker(GitFileStatus::Modified), "\u{25cf}");
        assert_eq!(icons.git_status_marker(GitFileStatus::Added), "\u{271a}");
        assert_eq!(icons.git_status_marker(GitFileStatus::Deleted), "\u{2716}");
        assert_eq!(icons.git_status_marker(GitFileStatus::Renamed), "\u{2192}");
    }

    #[test]
    fn test_progress_bar_eight_chars() {
        for tier in [
            GlyphTier::Ascii,
            GlyphTier::Unicode,
            GlyphTier::Emoji,
            GlyphTier::NerdFont,
        ] {
            let g = GlyphSet::for_tier(tier);
            assert_eq!(g.progress.bar.len(), 8, "tier {:?}", tier);
        }
    }

    #[test]
    fn test_all_tiers_have_nonempty_border_horizontal() {
        for tier in [
            GlyphTier::Ascii,
            GlyphTier::Unicode,
            GlyphTier::Emoji,
            GlyphTier::NerdFont,
        ] {
            let g = GlyphSet::for_tier(tier);
            assert!(g.border.horizontal != '\0', "tier {:?}", tier);
        }
    }

    #[test]
    fn test_all_tiers_have_nonempty_folder_and_file_icons() {
        for tier in [
            GlyphTier::Ascii,
            GlyphTier::Unicode,
            GlyphTier::Emoji,
            GlyphTier::NerdFont,
        ] {
            let g = GlyphSet::for_tier(tier);
            assert!(!g.icon.folder.is_empty(), "tier {:?} folder", tier);
            assert!(!g.icon.file.is_empty(), "tier {:?} file", tier);
        }
    }

    #[test]
    fn test_all_tiers_have_nonempty_success_failure() {
        for tier in [
            GlyphTier::Ascii,
            GlyphTier::Unicode,
            GlyphTier::Emoji,
            GlyphTier::NerdFont,
        ] {
            let g = GlyphSet::for_tier(tier);
            assert!(!g.indicator.success.is_empty(), "tier {:?}", tier);
            assert!(!g.indicator.failure.is_empty(), "tier {:?}", tier);
        }
    }
}
