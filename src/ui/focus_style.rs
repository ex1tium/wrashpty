//! Style helpers for focus/dimming effects.
//!
//! Terminal DIM (SGR 2) is binary — on or off — so these helpers are simple
//! wrappers. They exist as a shared vocabulary so all panels use the same
//! dimming approach.

use ratatui_core::style::{Modifier, Style};

/// Applies the DIM modifier to a style.
pub fn dim_style(style: Style) -> Style {
    style.add_modifier(Modifier::DIM)
}

/// Returns the style unchanged if `focused`, or dimmed if not.
pub fn apply_focus(style: Style, focused: bool) -> Style {
    if focused { style } else { dim_style(style) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_core::style::Color;

    #[test]
    fn test_dim_style_adds_modifier() {
        let base = Style::default().fg(Color::Red);
        let dimmed = dim_style(base);
        assert!(dimmed.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn test_apply_focus_true_returns_original() {
        let base = Style::default().fg(Color::Green);
        let result = apply_focus(base, true);
        // When focused, no DIM modifier should be added
        assert_eq!(result, base);
    }

    #[test]
    fn test_apply_focus_false_adds_dim() {
        let base = Style::default().fg(Color::Green);
        let result = apply_focus(base, false);
        assert_ne!(result, base);
    }
}
