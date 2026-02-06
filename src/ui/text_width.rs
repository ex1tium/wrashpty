//! Unicode-safe text width utilities for terminal UI rendering.
//!
//! Terminal UIs need three distinct notions of string "length":
//!
//! 1. **Bytes** (`str::len()`) – for storage and valid UTF-8 slicing.
//! 2. **Display width** (`display_width()`) – how many terminal columns a string
//!    occupies, accounting for fullwidth CJK, zero-width combining marks, etc.
//! 3. **Grapheme clusters** – user-perceived characters for cursor movement
//!    and editing (e.g. an emoji ZWJ sequence is one "character" to the user).
//!
//! This module provides canonical helpers so that all UI code uses display
//! width for layout and grapheme clusters for editing, never raw byte/char
//! counts.

use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Returns the terminal display width of a string (number of columns).
///
/// This correctly handles:
/// - ASCII (1 column each)
/// - CJK fullwidth characters (2 columns each)
/// - Zero-width combining marks (0 columns)
/// - Emoji (varies by terminal, but `unicode-width` gives best-effort)
///
/// Does NOT handle ANSI escape sequences – strip those first if present.
#[inline]
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncates a string to fit within `max_cols` terminal columns.
///
/// Returns the longest prefix whose display width does not exceed `max_cols`.
/// Never splits a multi-byte character or a wide character in half.
///
/// Returns a borrowed slice when no truncation is needed, avoiding allocation.
pub fn truncate_to_width(s: &str, max_cols: usize) -> Cow<'_, str> {
    if display_width(s) <= max_cols {
        return Cow::Borrowed(s);
    }

    let mut current_width = 0;
    let mut last_valid_idx = 0;

    for (idx, ch) in s.char_indices() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > max_cols {
            break;
        }
        current_width += ch_width;
        last_valid_idx = idx + ch.len_utf8();
    }

    Cow::Borrowed(&s[..last_valid_idx])
}

/// Truncates a string to fit within `max_cols`, appending an ellipsis if truncated.
///
/// If the string fits, returns it as-is. Otherwise truncates and appends "…"
/// (the ellipsis itself takes 1 column).
pub fn truncate_with_ellipsis(s: &str, max_cols: usize) -> Cow<'_, str> {
    if display_width(s) <= max_cols {
        return Cow::Borrowed(s);
    }
    if max_cols == 0 {
        return Cow::Borrowed("");
    }

    // Reserve 1 column for the ellipsis
    let truncated = truncate_to_width(s, max_cols.saturating_sub(1));
    let mut result = truncated.into_owned();
    result.push('…');
    Cow::Owned(result)
}

/// Pads a string to exactly `cols` terminal columns using trailing spaces.
///
/// If the string is already wider than `cols`, it is truncated first.
pub fn pad_to_width(s: &str, cols: usize) -> String {
    let w = display_width(s);
    if w >= cols {
        let truncated = truncate_to_width(s, cols);
        return truncated.into_owned();
    }
    let mut result = String::with_capacity(s.len() + (cols - w));
    result.push_str(s);
    for _ in 0..(cols - w) {
        result.push(' ');
    }
    result
}

/// Right-aligns a string within `cols` terminal columns, padding with leading spaces.
///
/// If the string is already wider than `cols`, it is truncated.
pub fn pad_right_align(s: &str, cols: usize) -> String {
    let w = display_width(s);
    if w >= cols {
        let truncated = truncate_to_width(s, cols);
        return truncated.into_owned();
    }
    let mut result = String::with_capacity(s.len() + (cols - w));
    for _ in 0..(cols - w) {
        result.push(' ');
    }
    result.push_str(s);
    result
}

/// Returns an iterator over grapheme cluster byte boundaries.
///
/// Each yielded value is the byte offset of the start of a grapheme cluster.
/// The final boundary (equal to `s.len()`) is NOT included; use `s.len()`
/// directly when you need the end of the string.
pub fn grapheme_boundaries(s: &str) -> impl Iterator<Item = usize> + '_ {
    s.grapheme_indices(true).map(|(i, _)| i)
}

/// Returns the number of grapheme clusters in a string.
///
/// This is the user-perceived "character count" – what a person would count
/// as characters when looking at the string.
#[inline]
pub fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

/// Slices a string by grapheme cluster range `[start_g..end_g)`.
///
/// Returns the substring spanning from the start of grapheme `start_g` to
/// the start of grapheme `end_g` (or end of string if `end_g` exceeds count).
///
/// Panics: none (clamps to valid range).
pub fn slice_grapheme_range(s: &str, start_g: usize, end_g: usize) -> &str {
    let graphemes: Vec<(usize, &str)> = s.grapheme_indices(true).collect();
    let count = graphemes.len();

    let start_g = start_g.min(count);
    let end_g = end_g.min(count);

    if start_g >= end_g {
        return "";
    }

    let start_byte = graphemes[start_g].0;
    let end_byte = if end_g >= count {
        s.len()
    } else {
        graphemes[end_g].0
    };

    &s[start_byte..end_byte]
}

/// Finds the byte offset of the grapheme cluster at the given grapheme index.
///
/// Returns `s.len()` if `grapheme_idx` is past the end of the string.
pub fn grapheme_byte_offset(s: &str, grapheme_idx: usize) -> usize {
    s.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Returns the display width of the substring from byte 0 to `byte_offset`.
///
/// Useful for computing cursor column from a byte cursor position.
/// Clamps `byte_offset` to a valid char boundary.
pub fn display_width_to_byte(s: &str, byte_offset: usize) -> usize {
    let clamped = clamp_to_char_boundary(s, byte_offset);
    display_width(&s[..clamped])
}

/// Clamps a byte offset to the nearest valid char boundary at or before `offset`.
fn clamp_to_char_boundary(s: &str, offset: usize) -> usize {
    let mut pos = offset.min(s.len());
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── display_width ───────────────────────────────────────────────

    #[test]
    fn display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
        assert_eq!(display_width(" "), 1);
    }

    #[test]
    fn display_width_cjk_fullwidth() {
        // Each CJK character occupies 2 columns
        assert_eq!(display_width("你好"), 4);
        assert_eq!(display_width("日本語"), 6);
        assert_eq!(display_width("a你b"), 4); // 1 + 2 + 1
    }

    #[test]
    fn display_width_combining_marks() {
        // e + combining acute accent = 1 column
        assert_eq!(display_width("e\u{0301}"), 1);
        // Multiple combining marks still 1 column for the base
        assert_eq!(display_width("a\u{0300}\u{0301}"), 1);
    }

    #[test]
    fn display_width_emoji_single_codepoint() {
        // Basic emoji - width depends on unicode-width version
        // Most single emoji are width 2
        let w = display_width("⭐");
        assert!(w == 1 || w == 2, "star emoji width: {}", w);
    }

    #[test]
    fn display_width_variation_selectors() {
        // Text presentation selector (VS15) = \u{FE0E}
        // Emoji presentation selector (VS16) = \u{FE0F}
        let plain = display_width("☺");
        let with_vs = display_width("☺\u{FE0F}");
        // VS shouldn't add width (it's zero-width)
        assert!(with_vs <= plain + 1, "VS16 should not add significant width");
    }

    #[test]
    fn display_width_mixed_content() {
        // "hello" (5) + "世界" (4) = 9
        assert_eq!(display_width("hello世界"), 9);
    }

    // ─── truncate_to_width ───────────────────────────────────────────

    #[test]
    fn truncate_ascii_no_truncation() {
        let result = truncate_to_width("hello", 10);
        assert_eq!(&*result, "hello");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn truncate_ascii_exact() {
        let result = truncate_to_width("hello", 5);
        assert_eq!(&*result, "hello");
    }

    #[test]
    fn truncate_ascii_shorter() {
        let result = truncate_to_width("hello", 3);
        assert_eq!(&*result, "hel");
    }

    #[test]
    fn truncate_cjk_avoids_half_split() {
        // "你好" = 4 columns, truncating to 3 should give only "你" (2 cols)
        // because "好" needs 2 cols and 2+2=4 > 3
        let result = truncate_to_width("你好", 3);
        assert_eq!(&*result, "你");
        assert_eq!(display_width(&*result), 2);
    }

    #[test]
    fn truncate_cjk_exact() {
        let result = truncate_to_width("你好", 4);
        assert_eq!(&*result, "你好");
    }

    #[test]
    fn truncate_mixed_cjk_ascii() {
        // "a你b" = 4 cols, truncate to 3 should give "a你" (3 cols)
        let result = truncate_to_width("a你b", 3);
        assert_eq!(&*result, "a你");
    }

    #[test]
    fn truncate_zero_width() {
        let result = truncate_to_width("hello", 0);
        assert_eq!(&*result, "");
    }

    #[test]
    fn truncate_combining_marks_kept_with_base() {
        // "é" as e + combining acute: truncating to 1 col should keep both
        let s = "e\u{0301}x";
        let result = truncate_to_width(s, 1);
        // Should include "e" + combining mark (1 col total)
        assert_eq!(&*result, "e\u{0301}");
    }

    // ─── truncate_with_ellipsis ──────────────────────────────────────

    #[test]
    fn ellipsis_no_truncation() {
        let result = truncate_with_ellipsis("hi", 10);
        assert_eq!(&*result, "hi");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn ellipsis_truncation() {
        let result = truncate_with_ellipsis("hello world", 8);
        assert_eq!(&*result, "hello w…");
        assert!(display_width(&*result) <= 8);
    }

    #[test]
    fn ellipsis_cjk_truncation() {
        // "你好世界" = 8 cols, truncate to 5 should give "你好…" (4+1=5)
        let result = truncate_with_ellipsis("你好世界", 5);
        assert_eq!(&*result, "你好…");
    }

    #[test]
    fn ellipsis_zero_width() {
        let result = truncate_with_ellipsis("hello", 0);
        assert_eq!(&*result, "");
    }

    // ─── pad_to_width ────────────────────────────────────────────────

    #[test]
    fn pad_ascii() {
        let result = pad_to_width("hi", 5);
        assert_eq!(result, "hi   ");
        assert_eq!(display_width(&result), 5);
    }

    #[test]
    fn pad_cjk() {
        let result = pad_to_width("你", 5);
        assert_eq!(result, "你   ");
        assert_eq!(display_width(&result), 5);
    }

    #[test]
    fn pad_already_exact() {
        let result = pad_to_width("hello", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn pad_truncates_when_too_wide() {
        let result = pad_to_width("hello world", 5);
        assert_eq!(result, "hello");
    }

    // ─── pad_right_align ─────────────────────────────────────────────

    #[test]
    fn right_align_ascii() {
        let result = pad_right_align("hi", 5);
        assert_eq!(result, "   hi");
    }

    #[test]
    fn right_align_cjk() {
        let result = pad_right_align("你", 5);
        assert_eq!(result, "   你");
        assert_eq!(display_width(&result), 5);
    }

    // ─── grapheme helpers ────────────────────────────────────────────

    #[test]
    fn grapheme_count_ascii() {
        assert_eq!(grapheme_count("hello"), 5);
    }

    #[test]
    fn grapheme_count_combining() {
        // "é" (e + combining acute) = 1 grapheme
        assert_eq!(grapheme_count("e\u{0301}"), 1);
    }

    #[test]
    fn grapheme_count_cjk() {
        assert_eq!(grapheme_count("你好"), 2);
    }

    #[test]
    fn grapheme_count_emoji_flag() {
        // Flag emoji (regional indicators) = 1 grapheme
        assert_eq!(grapheme_count("🇺🇸"), 1);
    }

    #[test]
    fn grapheme_count_zwj_sequence() {
        // Family emoji (ZWJ sequence) = 1 grapheme
        assert_eq!(grapheme_count("👨\u{200D}👩\u{200D}👧"), 1);
    }

    #[test]
    fn slice_grapheme_range_ascii() {
        assert_eq!(slice_grapheme_range("hello", 1, 3), "el");
    }

    #[test]
    fn slice_grapheme_range_combining() {
        let s = "ae\u{0301}b"; // a + é + b = 3 graphemes
        assert_eq!(slice_grapheme_range(s, 1, 2), "e\u{0301}");
    }

    #[test]
    fn slice_grapheme_range_cjk() {
        let s = "a你好b";
        assert_eq!(slice_grapheme_range(s, 1, 3), "你好");
    }

    #[test]
    fn slice_grapheme_range_past_end_clamps() {
        assert_eq!(slice_grapheme_range("hi", 0, 100), "hi");
    }

    #[test]
    fn slice_grapheme_range_empty() {
        assert_eq!(slice_grapheme_range("hi", 2, 2), "");
        assert_eq!(slice_grapheme_range("hi", 3, 1), "");
    }

    #[test]
    fn grapheme_byte_offset_basic() {
        assert_eq!(grapheme_byte_offset("hello", 0), 0);
        assert_eq!(grapheme_byte_offset("hello", 2), 2);
        assert_eq!(grapheme_byte_offset("hello", 5), 5); // past end
    }

    #[test]
    fn grapheme_byte_offset_combining() {
        let s = "e\u{0301}x"; // é + x = 2 graphemes
        assert_eq!(grapheme_byte_offset(s, 0), 0);
        assert_eq!(grapheme_byte_offset(s, 1), 3); // past e + combining (2+1 bytes? no, e=1, \u{0301}=2)
        // e = 1 byte, \u{0301} = 2 bytes, x = 1 byte
        // grapheme 0 starts at 0, grapheme 1 starts at 3
        assert_eq!(grapheme_byte_offset(s, 1), 3);
    }

    // ─── display_width_to_byte ───────────────────────────────────────

    #[test]
    fn display_width_to_byte_ascii() {
        assert_eq!(display_width_to_byte("hello", 3), 3);
    }

    #[test]
    fn display_width_to_byte_cjk() {
        // "你好" each char is 3 bytes, 2 display cols
        let s = "你好";
        assert_eq!(display_width_to_byte(s, 3), 2); // first 3 bytes = "你" = 2 cols
        assert_eq!(display_width_to_byte(s, 6), 4); // all 6 bytes = "你好" = 4 cols
    }

    #[test]
    fn display_width_to_byte_zero() {
        assert_eq!(display_width_to_byte("hello", 0), 0);
    }

    // ─── grapheme_boundaries ─────────────────────────────────────────

    #[test]
    fn grapheme_boundaries_ascii() {
        let bounds: Vec<usize> = grapheme_boundaries("abc").collect();
        assert_eq!(bounds, vec![0, 1, 2]);
    }

    #[test]
    fn grapheme_boundaries_combining() {
        let s = "e\u{0301}x";
        let bounds: Vec<usize> = grapheme_boundaries(s).collect();
        // Two graphemes: "e\u{0301}" at 0, "x" at 3
        assert_eq!(bounds, vec![0, 3]);
    }

    #[test]
    fn grapheme_boundaries_empty() {
        let bounds: Vec<usize> = grapheme_boundaries("").collect();
        assert!(bounds.is_empty());
    }
}
