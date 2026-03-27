//! Prompt rendering for reedline.
//!
//! This module implements the reedline Prompt trait, providing a shell
//! prompt that indicates success or failure of the last command.

use std::borrow::Cow;

use reedline::{Prompt, PromptEditMode, PromptHistorySearch};

/// A minimal prompt that shows `$ ` on success or `! ` on failure.
///
/// This prompt intentionally keeps things simple for the MVP. Future
/// enhancements could add working directory, git status, or other
/// shell information.
pub struct WrashPrompt {
    /// Exit code of the last command (0 = success).
    exit_code: i32,
}

impl WrashPrompt {
    /// Creates a new prompt with the given exit code.
    ///
    /// # Arguments
    ///
    /// * `exit_code` - The exit code of the last command (0 = success)
    pub fn new(exit_code: i32) -> Self {
        Self { exit_code }
    }
}

impl Prompt for WrashPrompt {
    /// Returns the left side of the prompt.
    ///
    /// Shows `$ ` for successful commands (exit code 0) or `! ` for failures.
    fn render_prompt_left(&self) -> Cow<'_, str> {
        if self.exit_code == 0 {
            Cow::Borrowed("$ ")
        } else {
            Cow::Borrowed("! ")
        }
    }

    /// Returns the right side of the prompt.
    ///
    /// Currently empty for the MVP.
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    /// Returns the prompt indicator (after the left prompt).
    ///
    /// We include the indicator in the left prompt itself, so this is empty.
    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    /// Returns the multiline continuation prompt.
    ///
    /// Shows `> ` for continuation lines.
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("> ")
    }

    /// Returns the history search indicator.
    ///
    /// Shows `(search) ` when in history search mode.
    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("(search) ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_success() {
        let prompt = WrashPrompt::new(0);
        assert_eq!(prompt.render_prompt_left(), "$ ");
    }

    #[test]
    fn test_prompt_failure() {
        let prompt = WrashPrompt::new(1);
        assert_eq!(prompt.render_prompt_left(), "! ");
    }

    #[test]
    fn test_prompt_various_exit_codes() {
        // Exit code 0 should show success
        let prompt = WrashPrompt::new(0);
        assert_eq!(prompt.render_prompt_left(), "$ ");

        // Any non-zero exit code should show failure
        for code in [1, 2, 42, 127, 128, 130, 255] {
            let prompt = WrashPrompt::new(code);
            assert_eq!(
                prompt.render_prompt_left(),
                "! ",
                "Exit code {} should show failure prompt",
                code
            );
        }
    }

    #[test]
    fn test_prompt_negative_exit_code() {
        // Negative exit codes (shouldn't normally happen, but handle gracefully)
        let prompt = WrashPrompt::new(-1);
        assert_eq!(prompt.render_prompt_left(), "! ");
    }

    #[test]
    fn test_prompt_right_is_empty() {
        let prompt = WrashPrompt::new(0);
        assert_eq!(prompt.render_prompt_right(), "");
    }

    #[test]
    fn test_prompt_indicator_is_empty() {
        let prompt = WrashPrompt::new(0);
        assert_eq!(prompt.render_prompt_indicator(PromptEditMode::Default), "");
    }

    #[test]
    fn test_multiline_indicator() {
        let prompt = WrashPrompt::new(0);
        assert_eq!(prompt.render_prompt_multiline_indicator(), "> ");
    }

    #[test]
    fn test_history_search_indicator() {
        let prompt = WrashPrompt::new(0);
        let indicator = prompt.render_prompt_history_search_indicator(PromptHistorySearch::new(
            reedline::PromptHistorySearchStatus::Passing,
            String::new(),
        ));
        assert_eq!(indicator, "(search) ");
    }
}
