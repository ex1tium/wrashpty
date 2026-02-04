//! Panel trait and result types for expandable chrome panels.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;

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
}
