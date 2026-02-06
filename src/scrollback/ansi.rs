//! Shared ANSI escape sequence parsing helpers.
//!
//! Centralizes CSI/OSC sequence handling used across capture, buffer, and viewer
//! modules to eliminate duplication and ensure consistent behavior.

/// Returns the expected byte length of a UTF-8 sequence from its first byte.
///
/// Returns 1 for ASCII and invalid start bytes as a safe fallback.
#[inline]
pub(crate) fn utf8_sequence_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => 1,
    }
}

/// Returns true if `b` is a valid CSI final byte (terminates a CSI sequence).
///
/// CSI sequences are `ESC [ <params> <final>` where final is in 0x40..=0x7E.
#[inline]
pub(crate) fn is_csi_final_byte(b: u8) -> bool {
    (0x40..=0x7E).contains(&b)
}

/// Skips a CSI sequence body starting at `pos` (after the `ESC [` introducer).
///
/// Returns the position immediately after the final byte, or `content.len()`
/// if the sequence is unterminated.
pub(crate) fn skip_csi(content: &[u8], mut pos: usize) -> usize {
    while pos < content.len() && !is_csi_final_byte(content[pos]) {
        pos += 1;
    }
    if pos < content.len() {
        pos + 1 // skip final byte
    } else {
        pos
    }
}

/// Skips an OSC sequence body starting at `pos` (after the `ESC ]` introducer).
///
/// OSC sequences are terminated by BEL (0x07) or ST (`ESC \`).
/// Returns the position immediately after the terminator, or `content.len()`
/// if the sequence is unterminated.
pub(crate) fn skip_osc(content: &[u8], mut pos: usize) -> usize {
    while pos < content.len() {
        if content[pos] == 0x07 {
            return pos + 1;
        }
        if content[pos] == 0x1b && pos + 1 < content.len() && content[pos + 1] == b'\\' {
            return pos + 2;
        }
        pos += 1;
    }
    pos
}

/// Sanitizes captured line content for safe display in scrollback.
///
/// Preserves SGR (Select Graphic Rendition) sequences for colors/styles
/// and strips all other ANSI control sequences that could corrupt the
/// scrollback view (cursor moves, screen clears, mode changes, etc.).
///
/// Specifically:
/// - **Preserves**: SGR sequences (`ESC[...m`) for foreground/background colors,
///   bold, italic, underline, reverse video, etc.
/// - **Strips**: All other CSI sequences (cursor movement, erase, scroll, etc.)
/// - **Strips**: OSC sequences (window title, hyperlinks, etc.)
/// - **Strips**: Other 2-byte escape sequences
/// - **Strips**: Control characters except TAB (0x09)
/// - **Preserves**: Printable ASCII, valid UTF-8, and TAB
pub(crate) fn sanitize_for_display(content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len());
    let mut i = 0;

    while i < content.len() {
        let b = content[i];

        // ESC sequence start
        if b == 0x1b && i + 1 < content.len() {
            let next = content[i + 1];
            if next == b'[' {
                // CSI sequence: find the final byte
                let body_start = i + 2;
                let end = skip_csi(content, body_start);
                // Check if final byte is 'm' (SGR) - preserve only SGR
                if end > body_start && content[end - 1] == b'm' {
                    out.extend_from_slice(&content[i..end]);
                }
                // All other CSI sequences are silently dropped
                i = end;
                continue;
            } else if next == b']' {
                // OSC sequence: skip entirely
                i = skip_osc(content, i + 2);
                continue;
            } else {
                // Other 2-byte escape: skip
                i += 2;
                continue;
            }
        }

        // TAB: preserve (needed for layout)
        if b == 0x09 {
            out.push(b);
            i += 1;
            continue;
        }

        // Other control characters: strip
        if b < 0x20 || b == 0x7F {
            i += 1;
            continue;
        }

        // Printable ASCII and UTF-8: pass through
        out.push(b);
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- utf8_sequence_len ---

    #[test]
    fn test_utf8_len_ascii() {
        assert_eq!(utf8_sequence_len(b'A'), 1);
        assert_eq!(utf8_sequence_len(b' '), 1);
        assert_eq!(utf8_sequence_len(0x7F), 1);
    }

    #[test]
    fn test_utf8_len_multibyte() {
        // 2-byte: U+00C0..U+07FF (first byte 0xC2..0xDF)
        assert_eq!(utf8_sequence_len(0xC2), 2);
        assert_eq!(utf8_sequence_len(0xDF), 2);
        // 3-byte: U+0800..U+FFFF (first byte 0xE0..0xEF) -- includes CJK
        assert_eq!(utf8_sequence_len(0xE0), 3);
        assert_eq!(utf8_sequence_len(0xEF), 3);
        // 4-byte: U+10000..U+10FFFF (first byte 0xF0..0xF4) -- includes emoji
        assert_eq!(utf8_sequence_len(0xF0), 4);
        assert_eq!(utf8_sequence_len(0xF4), 4);
    }

    #[test]
    fn test_utf8_len_invalid_start() {
        // Continuation bytes and other invalid start bytes return 1
        assert_eq!(utf8_sequence_len(0x80), 1);
        assert_eq!(utf8_sequence_len(0xBF), 1);
        assert_eq!(utf8_sequence_len(0xC0), 1); // overlong
        assert_eq!(utf8_sequence_len(0xC1), 1); // overlong
        assert_eq!(utf8_sequence_len(0xF5), 1); // beyond valid range
        assert_eq!(utf8_sequence_len(0xFF), 1);
    }

    // --- is_csi_final_byte ---

    #[test]
    fn test_csi_final_byte_boundaries() {
        assert!(is_csi_final_byte(0x40)); // '@' - lower bound
        assert!(is_csi_final_byte(0x7E)); // '~' - upper bound
        assert!(is_csi_final_byte(b'm')); // SGR
        assert!(is_csi_final_byte(b'H')); // CUP (cursor position)
        assert!(is_csi_final_byte(b'J')); // ED (erase display)
        assert!(is_csi_final_byte(b'K')); // EL (erase line)
        assert!(!is_csi_final_byte(b';')); // parameter separator
        assert!(!is_csi_final_byte(b'?')); // private mode prefix
        assert!(!is_csi_final_byte(b'0')); // digit parameter
        assert!(!is_csi_final_byte(0x3F)); // just below range
        assert!(!is_csi_final_byte(0x7F)); // DEL, just above range
    }

    // --- skip_csi ---

    #[test]
    fn test_skip_csi_sgr() {
        // ESC[31m - "31m" is what skip_csi sees (pos starts after ESC[)
        let content = b"31m rest";
        assert_eq!(skip_csi(content, 0), 3); // skips "31m", lands after 'm'
    }

    #[test]
    fn test_skip_csi_cursor_move() {
        // ESC[10;5H
        let content = b"10;5H rest";
        assert_eq!(skip_csi(content, 0), 5); // skips "10;5H"
    }

    #[test]
    fn test_skip_csi_private_mode() {
        // ESC[?1049h (alt screen)
        let content = b"?1049h";
        assert_eq!(skip_csi(content, 0), 6);
    }

    #[test]
    fn test_skip_csi_unterminated() {
        let content = b"31;42";
        assert_eq!(skip_csi(content, 0), 5); // returns content.len()
    }

    #[test]
    fn test_skip_csi_empty() {
        let content = b"";
        assert_eq!(skip_csi(content, 0), 0);
    }

    // --- skip_osc ---

    #[test]
    fn test_skip_osc_bel_terminated() {
        // ESC]0;title BEL
        let content = b"0;my-title\x07 rest";
        assert_eq!(skip_osc(content, 0), 11); // skips up to and including BEL
    }

    #[test]
    fn test_skip_osc_st_terminated() {
        // ESC]0;title ESC\ (ST)
        let content = b"0;my-title\x1b\\ rest";
        assert_eq!(skip_osc(content, 0), 12); // skips up to and including ESC backslash
    }

    #[test]
    fn test_skip_osc_unterminated() {
        let content = b"0;my-title";
        assert_eq!(skip_osc(content, 0), 10); // returns content.len()
    }

    #[test]
    fn test_skip_osc_empty() {
        let content = b"";
        assert_eq!(skip_osc(content, 0), 0);
    }

    // --- sanitize_for_display ---

    #[test]
    fn test_sanitize_preserves_sgr() {
        let input = b"\x1b[31mred\x1b[0m";
        assert_eq!(sanitize_for_display(input), b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_sanitize_preserves_complex_sgr() {
        // Bold + 256-color foreground + RGB background
        let input = b"\x1b[1;38;5;196;48;2;0;128;255mtext\x1b[0m";
        assert_eq!(
            sanitize_for_display(input),
            b"\x1b[1;38;5;196;48;2;0;128;255mtext\x1b[0m"
        );
    }

    #[test]
    fn test_sanitize_strips_cursor_move() {
        let input = b"hello\x1b[10;5Hworld";
        assert_eq!(sanitize_for_display(input), b"helloworld");
    }

    #[test]
    fn test_sanitize_strips_screen_clear() {
        let input = b"before\x1b[2Jafter";
        assert_eq!(sanitize_for_display(input), b"beforeafter");
    }

    #[test]
    fn test_sanitize_strips_erase_line() {
        let input = b"text\x1b[K";
        assert_eq!(sanitize_for_display(input), b"text");
    }

    #[test]
    fn test_sanitize_strips_osc() {
        let input = b"text\x1b]0;my-title\x07more";
        assert_eq!(sanitize_for_display(input), b"textmore");
    }

    #[test]
    fn test_sanitize_strips_osc_st() {
        let input = b"text\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\more";
        assert_eq!(sanitize_for_display(input), b"textlinkmore");
    }

    #[test]
    fn test_sanitize_strips_bel() {
        let input = b"hello\x07world";
        assert_eq!(sanitize_for_display(input), b"helloworld");
    }

    #[test]
    fn test_sanitize_strips_backspace() {
        let input = b"abc\x08d";
        assert_eq!(sanitize_for_display(input), b"abcd");
    }

    #[test]
    fn test_sanitize_preserves_tab() {
        let input = b"col1\tcol2";
        assert_eq!(sanitize_for_display(input), b"col1\tcol2");
    }

    #[test]
    fn test_sanitize_preserves_utf8() {
        let input = "hello 你好 🌍".as_bytes();
        assert_eq!(sanitize_for_display(input), input);
    }

    #[test]
    fn test_sanitize_mixed_sgr_and_cursor() {
        // SGR preserved, cursor move stripped
        let input = b"\x1b[31m\x1b[10;5Hred\x1b[0m";
        assert_eq!(sanitize_for_display(input), b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_sanitize_empty() {
        assert_eq!(sanitize_for_display(b""), b"");
    }

    #[test]
    fn test_sanitize_plain_text() {
        let input = b"just plain text";
        assert_eq!(sanitize_for_display(input), input);
    }

    #[test]
    fn test_sanitize_strips_private_mode() {
        // ESC[?25h (show cursor) and ESC[?25l (hide cursor)
        let input = b"\x1b[?25ltext\x1b[?25h";
        assert_eq!(sanitize_for_display(input), b"text");
    }

    #[test]
    fn test_sanitize_strips_del() {
        let input = b"abc\x7Fd";
        assert_eq!(sanitize_for_display(input), b"abcd");
    }
}
