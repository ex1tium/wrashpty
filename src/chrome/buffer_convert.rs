//! Buffer-to-ANSI conversion utilities.
//!
//! This module provides functions to convert ratatui `Buffer` contents to
//! ANSI escape sequences for direct terminal output, bypassing ratatui's
//! Terminal abstraction to avoid conflicts with reedline.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

/// Converts a ratatui buffer to ANSI escape sequences.
///
/// Iterates through the buffer cells in the specified area, emitting
/// cursor positioning and style changes as needed.
///
/// # Arguments
///
/// * `buffer` - The ratatui buffer to convert
/// * `area` - The area of the buffer to convert
///
/// # Returns
///
/// A string containing ANSI escape sequences that reproduce the buffer content.
pub fn buffer_to_ansi(buffer: &Buffer, area: Rect) -> String {
    let mut result = String::with_capacity(area.width as usize * area.height as usize * 4);
    let mut current_style: Option<Style> = None;

    for y in area.y..area.y + area.height {
        // Position cursor at start of row (1-indexed for terminal)
        result.push_str(&format!("\x1b[{};{}H", y + 1, area.x + 1));

        let mut skip = 0u16;
        for x in area.x..area.x + area.width {
            // Skip continuation cells left by wide characters.
            // Ratatui resets these to " " but the wide char already occupies the columns.
            if skip > 0 {
                skip -= 1;
                continue;
            }

            let cell = buffer.cell((x, y));
            if let Some(cell) = cell {
                let style = cell.style();

                // Emit style changes only when style differs from previous cell
                if current_style.as_ref() != Some(&style) {
                    result.push_str(&style_to_ansi(&style));
                    current_style = Some(style);
                }

                let symbol = cell.symbol();
                result.push_str(symbol);

                // If this symbol is wider than 1 column, skip the continuation cells
                let w = UnicodeWidthStr::width(symbol);
                if w > 1 {
                    skip = (w as u16) - 1;
                }
            }
        }
    }

    // Reset style at end
    result.push_str("\x1b[0m");

    result
}

/// Converts a ratatui `Style` to ANSI escape codes.
///
/// Handles foreground color, background color, and text modifiers.
///
/// # Arguments
///
/// * `style` - The ratatui style to convert
///
/// # Returns
///
/// A string containing ANSI escape codes for the style.
pub fn style_to_ansi(style: &Style) -> String {
    let mut codes = Vec::new();

    // Reset first to clear previous styles
    codes.push("0".to_string());

    // Handle modifiers
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".to_string());
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("2".to_string());
    }
    if style.add_modifier.contains(Modifier::ITALIC) {
        codes.push("3".to_string());
    }
    if style.add_modifier.contains(Modifier::UNDERLINED) {
        codes.push("4".to_string());
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        codes.push("7".to_string());
    }

    // Handle foreground color
    if let Some(fg) = style.fg {
        codes.push(color_to_ansi_fg(fg));
    }

    // Handle background color
    if let Some(bg) = style.bg {
        codes.push(color_to_ansi_bg(bg));
    }

    format!("\x1b[{}m", codes.join(";"))
}

/// Converts a ratatui color to ANSI foreground color code.
fn color_to_ansi_fg(color: Color) -> String {
    match color {
        Color::Reset => "39".to_string(),
        Color::Black => "30".to_string(),
        Color::Red => "31".to_string(),
        Color::Green => "32".to_string(),
        Color::Yellow => "33".to_string(),
        Color::Blue => "34".to_string(),
        Color::Magenta => "35".to_string(),
        Color::Cyan => "36".to_string(),
        Color::Gray => "37".to_string(),
        Color::DarkGray => "90".to_string(),
        Color::LightRed => "91".to_string(),
        Color::LightGreen => "92".to_string(),
        Color::LightYellow => "93".to_string(),
        Color::LightBlue => "94".to_string(),
        Color::LightMagenta => "95".to_string(),
        Color::LightCyan => "96".to_string(),
        Color::White => "97".to_string(),
        Color::Rgb(r, g, b) => format!("38;2;{};{};{}", r, g, b),
        Color::Indexed(i) => format!("38;5;{}", i),
    }
}

/// Converts a ratatui color to ANSI background color code.
fn color_to_ansi_bg(color: Color) -> String {
    match color {
        Color::Reset => "49".to_string(),
        Color::Black => "40".to_string(),
        Color::Red => "41".to_string(),
        Color::Green => "42".to_string(),
        Color::Yellow => "43".to_string(),
        Color::Blue => "44".to_string(),
        Color::Magenta => "45".to_string(),
        Color::Cyan => "46".to_string(),
        Color::Gray => "47".to_string(),
        Color::DarkGray => "100".to_string(),
        Color::LightRed => "101".to_string(),
        Color::LightGreen => "102".to_string(),
        Color::LightYellow => "103".to_string(),
        Color::LightBlue => "104".to_string(),
        Color::LightMagenta => "105".to_string(),
        Color::LightCyan => "106".to_string(),
        Color::White => "107".to_string(),
        Color::Rgb(r, g, b) => format!("48;2;{};{};{}", r, g, b),
        Color::Indexed(i) => format!("48;5;{}", i),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_to_ansi_simple() {
        let area = Rect::new(0, 0, 5, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "hello", Style::default());

        let ansi = buffer_to_ansi(&buffer, area);

        // Should contain cursor positioning
        assert!(ansi.contains("\x1b[1;1H"));
        // Should contain the text
        assert!(ansi.contains("hello"));
        // Should end with reset
        assert!(ansi.ends_with("\x1b[0m"));
    }

    #[test]
    fn test_buffer_to_ansi_with_colors() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "test", Style::default().fg(Color::Red));

        let ansi = buffer_to_ansi(&buffer, area);

        // Should contain red foreground color code
        assert!(ansi.contains("31"));
    }

    #[test]
    fn test_buffer_to_ansi_multi_row() {
        let area = Rect::new(0, 0, 3, 2);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "abc", Style::default());
        buffer.set_string(0, 1, "def", Style::default());

        let ansi = buffer_to_ansi(&buffer, area);

        // Should contain cursor positioning for both rows
        assert!(ansi.contains("\x1b[1;1H"));
        assert!(ansi.contains("\x1b[2;1H"));
        // Should contain both strings
        assert!(ansi.contains("abc"));
        assert!(ansi.contains("def"));
    }

    #[test]
    fn test_style_to_ansi_default() {
        let style = Style::default();
        let ansi = style_to_ansi(&style);
        // Should contain reset
        assert!(ansi.contains("0"));
    }

    #[test]
    fn test_style_to_ansi_bold() {
        let style = Style::default().add_modifier(Modifier::BOLD);
        let ansi = style_to_ansi(&style);
        // Should contain bold code
        assert!(ansi.contains(";1"));
    }

    #[test]
    fn test_style_to_ansi_fg_color() {
        let style = Style::default().fg(Color::Green);
        let ansi = style_to_ansi(&style);
        // Should contain green foreground code
        assert!(ansi.contains("32"));
    }

    #[test]
    fn test_style_to_ansi_bg_color() {
        let style = Style::default().bg(Color::Blue);
        let ansi = style_to_ansi(&style);
        // Should contain blue background code
        assert!(ansi.contains("44"));
    }

    #[test]
    fn test_color_to_ansi_fg_rgb() {
        let code = color_to_ansi_fg(Color::Rgb(255, 128, 64));
        assert_eq!(code, "38;2;255;128;64");
    }

    #[test]
    fn test_color_to_ansi_bg_indexed() {
        let code = color_to_ansi_bg(Color::Indexed(202));
        assert_eq!(code, "48;5;202");
    }

    #[test]
    fn test_buffer_to_ansi_wide_char_no_duplicate() {
        // CJK character "你" occupies 2 columns. Ratatui stores it in cell 0
        // and resets cell 1 to " ". Our converter must skip the continuation cell.
        let area = Rect::new(0, 0, 4, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "你a", Style::default());

        let ansi = buffer_to_ansi(&buffer, area);

        // Should contain "你" exactly once and "a" once, no spurious space between them
        assert!(
            ansi.contains("你a"),
            "Wide char followed by ASCII: got {ansi:?}"
        );
        // The continuation space should NOT appear
        let after_cursor = ansi.split("\x1b[1;1H").nth(1).unwrap_or("");
        // Strip style codes to get visible content
        let visible: String = crate::chrome::test_utils::strip_ansi_for_test(after_cursor);
        assert_eq!(
            visible, "你a ",
            "Expected 'you' + 'a' + trailing space, got: {visible:?}"
        );
    }

    #[test]
    fn test_buffer_to_ansi_mixed_wide_and_ascii() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buffer = Buffer::empty(area);
        buffer.set_string(0, 0, "a你好", Style::default());

        let ansi = buffer_to_ansi(&buffer, area);

        let after_cursor = ansi.split("\x1b[1;1H").nth(1).unwrap_or("");
        let visible: String = crate::chrome::test_utils::strip_ansi_for_test(after_cursor);
        // "a" (1 col) + "你" (2 cols) + "好" (2 cols) = 5 cols, 1 trailing space
        assert_eq!(visible, "a你好 ", "Mixed content: got {visible:?}");
    }
}
