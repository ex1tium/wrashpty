//! UI utilities for terminal rendering.
//!
//! Shared helpers for Unicode-safe text width calculation, truncation,
//! padding, tree rendering, viewport management, and style effects
//! used across all UI rendering paths.

pub mod filter_input;
pub mod focus_style;
pub mod input_widgets;
pub mod loading_widget;
pub mod scrollable_list;
pub mod scrolling_text;
pub mod text_width;
pub mod tree_state;
pub mod tree_view;

// Re-export public types for convenience
pub use loading_widget::{LoadingWidget, LoadingWidgetOptions, SpinnerStyle};
pub use scrolling_text::ScrollingText;
