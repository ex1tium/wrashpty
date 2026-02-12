//! Panel trait and result types for expandable chrome panels.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;

use super::footer_bar::FooterEntry;
use super::glyphs::GlyphTier;
use super::theme::Theme;

/// Result of handling panel input.
#[derive(Debug, Clone)]
pub enum PanelResult {
    /// Keep panel open, re-render.
    Continue,
    /// Close panel, return to reedline.
    Dismiss,
    /// Close panel, execute command.
    Execute(String),
    /// Close panel, insert text into reedline buffer.
    InsertText(String),
}

/// Trait for expandable panels in the chrome layer.
///
/// Panels are rendered using ratatui widgets into a buffer, which is then
/// converted to ANSI sequences and written to the terminal.
///
/// Footer data is provided via `footer_entries()` and `border_info()` — the
/// compositor (`TabbedPanel`) renders them using `FooterBar` and `BorderLine`
/// widgets. This mirrors how `TopbarSegment::render()` provides data and
/// `TopbarRegistry` composes it.
pub trait Panel {
    /// Returns the preferred height for this panel.
    fn preferred_height(&self) -> u16;

    /// Returns the panel title for the tab bar.
    fn title(&self) -> &str;

    /// Renders the panel content into the given buffer area.
    fn render(&mut self, buffer: &mut Buffer, area: Rect);

    /// Handles a key input event.
    ///
    /// Returns a `PanelResult` indicating how to proceed.
    fn handle_input(&mut self, key: KeyEvent) -> PanelResult;

    /// Returns a mutable reference to self as `Any` for downcasting.
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Footer entries for this panel's current state.
    ///
    /// State-driven — returns different entries based on mode (edit, confirm,
    /// normal). Empty vec means "no footer" — compositor skips footer rendering.
    ///
    /// Mirrors `TopbarSegment::render()` returning `None` to hide a segment.
    fn footer_entries(&self) -> Vec<FooterEntry> {
        Vec::new()
    }

    /// Optional right-aligned info for the border line above footer.
    fn border_info(&self) -> Option<String> {
        None
    }

    /// Updates the glyph tier for runtime switching.
    ///
    /// Default is a no-op. Override in panels that cache glyph references.
    fn set_glyph_tier(&mut self, _tier: GlyphTier) {}

    /// Returns the panel's current theme.
    ///
    /// Used by the outer panel frame to keep border/header colors in sync
    /// when the theme changes at runtime (e.g. via the settings panel).
    fn theme(&self) -> &'static Theme;

    /// Updates the theme for runtime switching.
    ///
    /// Default is a no-op. Override in panels that cache theme references.
    fn set_theme(&mut self, _theme: &'static Theme) {}

    /// Returns true if the panel has active animations that require periodic redraws.
    ///
    /// Default implementation returns false. Override in panels with animations
    /// (e.g., loading spinners) that need to update even without user input.
    fn is_animating(&self) -> bool {
        false
    }
}
