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

/// Skips an ST-terminated sequence body starting at `pos`.
///
/// DCS (`ESC P`), SOS (`ESC X`), PM (`ESC ^`), and APC (`ESC _`) are all
/// terminated by ST (`ESC \`). Unlike OSC, they are NOT terminated by BEL.
/// Returns the position immediately after the ST, or `content.len()`
/// if the sequence is unterminated.
pub(crate) fn skip_st_terminated(content: &[u8], mut pos: usize) -> usize {
    while pos < content.len() {
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
            } else if matches!(next, b'P' | b'X' | b'^' | b'_') {
                // DCS (P), SOS (X), PM (^), APC (_): ST-terminated sequences.
                // Skip entire payload to avoid dumping sixel/DCS data into output.
                i = skip_st_terminated(content, i + 2);
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

        // Printable ASCII: pass through directly
        if b < 0x80 {
            out.push(b);
            i += 1;
            continue;
        }

        // UTF-8 multi-byte: validate before passing through
        let seq_len = utf8_sequence_len(b);
        if seq_len > 1 && i + seq_len <= content.len() {
            // Verify all continuation bytes have the form 10xxxxxx
            let valid = content[i + 1..i + seq_len]
                .iter()
                .all(|&c| (c & 0xC0) == 0x80);
            if valid {
                out.extend_from_slice(&content[i..i + seq_len]);
                i += seq_len;
                continue;
            }
        }

        // Invalid UTF-8 start byte or truncated sequence: skip
        i += 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- utf8_sequence_len ---

    #[test]
    fn test_utf8_sequence_len_ascii_returns_1() {
        assert_eq!(utf8_sequence_len(b'A'), 1);
        assert_eq!(utf8_sequence_len(b' '), 1);
        assert_eq!(utf8_sequence_len(0x7F), 1);
    }

    #[test]
    fn test_utf8_sequence_len_multibyte_returns_expected_lengths() {
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
    fn test_utf8_sequence_len_invalid_start_returns_1() {
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
    fn test_is_csi_final_byte_valid_range_returns_true() {
        assert!(is_csi_final_byte(0x40)); // '@' - lower bound
        assert!(is_csi_final_byte(0x7E)); // '~' - upper bound
        assert!(is_csi_final_byte(b'm')); // SGR
        assert!(is_csi_final_byte(b'H')); // CUP (cursor position)
        assert!(is_csi_final_byte(b'J')); // ED (erase display)
        assert!(is_csi_final_byte(b'K')); // EL (erase line)
    }

    #[test]
    fn test_is_csi_final_byte_non_final_returns_false() {
        assert!(!is_csi_final_byte(b';')); // parameter separator
        assert!(!is_csi_final_byte(b'?')); // private mode prefix
        assert!(!is_csi_final_byte(b'0')); // digit parameter
        assert!(!is_csi_final_byte(0x3F)); // just below range
        assert!(!is_csi_final_byte(0x7F)); // DEL, just above range
    }

    // --- skip_csi ---

    #[test]
    fn test_skip_csi_sgr_sequence_returns_pos_after_final() {
        // ESC[31m - "31m" is what skip_csi sees (pos starts after ESC[)
        let content = b"31m rest";
        assert_eq!(skip_csi(content, 0), 3); // skips "31m", lands after 'm'
    }

    #[test]
    fn test_skip_csi_cursor_move_returns_pos_after_final() {
        // ESC[10;5H
        let content = b"10;5H rest";
        assert_eq!(skip_csi(content, 0), 5); // skips "10;5H"
    }

    #[test]
    fn test_skip_csi_private_mode_returns_pos_after_final() {
        // ESC[?1049h (alt screen)
        let content = b"?1049h";
        assert_eq!(skip_csi(content, 0), 6);
    }

    #[test]
    fn test_skip_csi_unterminated_returns_content_len() {
        let content = b"31;42";
        assert_eq!(skip_csi(content, 0), 5); // returns content.len()
    }

    #[test]
    fn test_skip_csi_empty_returns_zero() {
        let content = b"";
        assert_eq!(skip_csi(content, 0), 0);
    }

    // --- skip_osc ---

    #[test]
    fn test_skip_osc_bel_terminated_returns_pos_after_bel() {
        // ESC]0;title BEL
        let content = b"0;my-title\x07 rest";
        assert_eq!(skip_osc(content, 0), 11); // skips up to and including BEL
    }

    #[test]
    fn test_skip_osc_st_terminated_returns_pos_after_st() {
        // ESC]0;title ESC\ (ST)
        let content = b"0;my-title\x1b\\ rest";
        assert_eq!(skip_osc(content, 0), 12); // skips up to and including ESC backslash
    }

    #[test]
    fn test_skip_osc_unterminated_returns_content_len() {
        let content = b"0;my-title";
        assert_eq!(skip_osc(content, 0), 10); // returns content.len()
    }

    #[test]
    fn test_skip_osc_empty_returns_zero() {
        let content = b"";
        assert_eq!(skip_osc(content, 0), 0);
    }

    // --- sanitize_for_display ---

    #[test]
    fn test_sanitize_for_display_sgr_preserved() {
        let input = b"\x1b[31mred\x1b[0m";
        assert_eq!(sanitize_for_display(input), b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_sanitize_for_display_complex_sgr_preserved() {
        // Bold + 256-color foreground + RGB background
        let input = b"\x1b[1;38;5;196;48;2;0;128;255mtext\x1b[0m";
        assert_eq!(
            sanitize_for_display(input),
            b"\x1b[1;38;5;196;48;2;0;128;255mtext\x1b[0m"
        );
    }

    #[test]
    fn test_sanitize_for_display_cursor_move_stripped() {
        let input = b"hello\x1b[10;5Hworld";
        assert_eq!(sanitize_for_display(input), b"helloworld");
    }

    #[test]
    fn test_sanitize_for_display_screen_clear_stripped() {
        let input = b"before\x1b[2Jafter";
        assert_eq!(sanitize_for_display(input), b"beforeafter");
    }

    #[test]
    fn test_sanitize_for_display_erase_line_stripped() {
        let input = b"text\x1b[K";
        assert_eq!(sanitize_for_display(input), b"text");
    }

    #[test]
    fn test_sanitize_for_display_osc_bel_stripped() {
        let input = b"text\x1b]0;my-title\x07more";
        assert_eq!(sanitize_for_display(input), b"textmore");
    }

    #[test]
    fn test_sanitize_for_display_osc_st_stripped() {
        let input = b"text\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\more";
        assert_eq!(sanitize_for_display(input), b"textlinkmore");
    }

    #[test]
    fn test_sanitize_for_display_bel_control_stripped() {
        let input = b"hello\x07world";
        assert_eq!(sanitize_for_display(input), b"helloworld");
    }

    #[test]
    fn test_sanitize_for_display_backspace_stripped() {
        let input = b"abc\x08d";
        assert_eq!(sanitize_for_display(input), b"abcd");
    }

    #[test]
    fn test_sanitize_for_display_tab_preserved() {
        let input = b"col1\tcol2";
        assert_eq!(sanitize_for_display(input), b"col1\tcol2");
    }

    #[test]
    fn test_sanitize_for_display_utf8_preserved() {
        let input = "hello 你好 🌍".as_bytes();
        assert_eq!(sanitize_for_display(input), input);
    }

    #[test]
    fn test_sanitize_for_display_mixed_sgr_and_cursor_keeps_sgr_only() {
        // SGR preserved, cursor move stripped
        let input = b"\x1b[31m\x1b[10;5Hred\x1b[0m";
        assert_eq!(sanitize_for_display(input), b"\x1b[31mred\x1b[0m");
    }

    #[test]
    fn test_sanitize_for_display_empty_returns_empty() {
        assert_eq!(sanitize_for_display(b""), b"");
    }

    #[test]
    fn test_sanitize_for_display_plain_text_unchanged() {
        let input = b"just plain text";
        assert_eq!(sanitize_for_display(input), input);
    }

    #[test]
    fn test_sanitize_for_display_private_mode_stripped() {
        // ESC[?25h (show cursor) and ESC[?25l (hide cursor)
        let input = b"\x1b[?25ltext\x1b[?25h";
        assert_eq!(sanitize_for_display(input), b"text");
    }

    #[test]
    fn test_sanitize_for_display_del_stripped() {
        let input = b"abc\x7Fd";
        assert_eq!(sanitize_for_display(input), b"abcd");
    }

    // --- skip_st_terminated ---

    #[test]
    fn test_skip_st_terminated_finds_st() {
        // DCS payload terminated by ESC backslash
        let content = b"some;dcs;payload\x1b\\ rest";
        assert_eq!(skip_st_terminated(content, 0), 18); // after ESC backslash
    }

    #[test]
    fn test_skip_st_terminated_unterminated_returns_len() {
        let content = b"unterminated payload";
        assert_eq!(skip_st_terminated(content, 0), content.len());
    }

    #[test]
    fn test_skip_st_terminated_empty_returns_zero() {
        assert_eq!(skip_st_terminated(b"", 0), 0);
    }

    #[test]
    fn test_skip_st_terminated_esc_alone_not_st() {
        // ESC without backslash is not ST
        let content = b"payload\x1bX more";
        assert_eq!(skip_st_terminated(content, 0), content.len());
    }

    // --- sanitize_for_display: DCS/SOS/PM/APC ---

    #[test]
    fn test_sanitize_for_display_dcs_stripped() {
        // DCS sequence: ESC P <payload> ESC backslash
        let input = b"before\x1bPq;1;1;10;#2NAAAAAA\x1b\\after";
        assert_eq!(sanitize_for_display(input), b"beforeafter");
    }

    #[test]
    fn test_sanitize_for_display_apc_stripped() {
        // APC sequence: ESC _ <payload> ESC backslash
        let input = b"text\x1b_some-apc-data\x1b\\more";
        assert_eq!(sanitize_for_display(input), b"textmore");
    }

    #[test]
    fn test_sanitize_for_display_sos_stripped() {
        // SOS sequence: ESC X <payload> ESC backslash
        let input = b"text\x1bXsos-payload\x1b\\more";
        assert_eq!(sanitize_for_display(input), b"textmore");
    }

    #[test]
    fn test_sanitize_for_display_pm_stripped() {
        // PM sequence: ESC ^ <payload> ESC backslash
        let input = b"text\x1b^pm-payload\x1b\\more";
        assert_eq!(sanitize_for_display(input), b"textmore");
    }

    #[test]
    fn test_sanitize_for_display_unterminated_dcs_strips_to_end() {
        // Unterminated DCS — everything after ESC P is consumed
        let input = b"before\x1bPunterminated";
        assert_eq!(sanitize_for_display(input), b"before");
    }

    #[test]
    fn test_sanitize_for_display_dcs_not_terminated_by_bel() {
        // Unlike OSC, DCS is NOT terminated by BEL — BEL is part of payload
        let input = b"before\x1bPpayload\x07after\x1b\\end";
        assert_eq!(sanitize_for_display(input), b"beforeend");
    }

    // --- sanitize_for_display: UTF-8 validation ---

    #[test]
    fn test_sanitize_for_display_valid_utf8_multibyte_preserved() {
        // 2-byte: é (U+00E9) = 0xC3 0xA9
        let input = b"caf\xc3\xa9";
        assert_eq!(sanitize_for_display(input), b"caf\xc3\xa9");
    }

    #[test]
    fn test_sanitize_for_display_valid_utf8_3byte_preserved() {
        // 3-byte: 你 (U+4F60) = 0xE4 0xBD 0xA0
        let input = b"hi\xe4\xbd\xa0";
        assert_eq!(sanitize_for_display(input), b"hi\xe4\xbd\xa0");
    }

    #[test]
    fn test_sanitize_for_display_valid_utf8_4byte_preserved() {
        // 4-byte: 🌍 (U+1F30D) = 0xF0 0x9F 0x8C 0x8D
        let input = b"earth\xf0\x9f\x8c\x8d";
        assert_eq!(sanitize_for_display(input), b"earth\xf0\x9f\x8c\x8d");
    }

    #[test]
    fn test_sanitize_for_display_truncated_utf8_stripped() {
        // Truncated 3-byte sequence: only 2 bytes present
        let input = b"text\xe4\xbd";
        assert_eq!(sanitize_for_display(input), b"text");
    }

    #[test]
    fn test_sanitize_for_display_invalid_continuation_stripped() {
        // 2-byte start followed by non-continuation byte
        let input = b"text\xc3Xmore";
        assert_eq!(sanitize_for_display(input), b"textXmore");
    }

    #[test]
    fn test_sanitize_for_display_bare_continuation_bytes_stripped() {
        // Bare continuation bytes (0x80-0xBF) without a valid start byte
        let input = b"text\x80\xBFmore";
        assert_eq!(sanitize_for_display(input), b"textmore");
    }

    #[test]
    fn test_sanitize_for_display_overlong_start_byte_stripped() {
        // 0xC0, 0xC1 are overlong encodings — utf8_sequence_len returns 1
        let input = b"text\xc0\x80more";
        assert_eq!(sanitize_for_display(input), b"textmore");
    }

    #[test]
    fn test_sanitize_for_display_mixed_valid_invalid_utf8() {
        // Valid 2-byte, then invalid continuation, then valid ASCII
        let input = b"\xc3\xa9\xc3Xok";
        assert_eq!(sanitize_for_display(input), b"\xc3\xa9Xok");
    }
}
