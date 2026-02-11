//! Modular command separator renderer for scrollback boundaries.
//!
//! Separators are composed from independent metadata segments and truncated by
//! priority when terminal width is constrained.

use std::fmt;
use std::path::Path;

use super::boundaries::CommandRecord;
use crate::chrome::segments::{
    color_to_fg_ansi, format_duration, strip_ansi_width, truncate_ansi_content,
};
use crate::chrome::glyphs::GlyphSet;
use crate::chrome::theme::Theme;

/// Rendered separator segment with width metadata.
#[derive(Debug, Clone)]
pub struct RenderedSepPart {
    /// ANSI-formatted segment text.
    pub content: String,
    /// Display width excluding ANSI escapes.
    pub display_width: usize,
    /// Priority for overflow removal (higher gets dropped first).
    pub priority: u8,
}

impl RenderedSepPart {
    fn new(content: String, priority: u8) -> Self {
        Self {
            display_width: strip_ansi_width(&content),
            content,
            priority,
        }
    }
}

/// Trait implemented by all separator metadata segments.
pub trait SeparatorSegment: Send + Sync {
    /// Stable segment identifier.
    fn id(&self) -> &'static str;

    /// Renders a segment for the given command metadata.
    fn render(
        &self,
        record: &CommandRecord,
        theme: &Theme,
        glyphs: &GlyphSet,
    ) -> Option<RenderedSepPart>;
}

/// Registry that orchestrates separator segment rendering.
#[derive(Default)]
pub struct SeparatorRegistry {
    segments: Vec<Box<dyn SeparatorSegment>>,
}

impl fmt::Debug for SeparatorRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<&'static str> = self.segments.iter().map(|segment| segment.id()).collect();
        f.debug_struct("SeparatorRegistry")
            .field("segments", &ids)
            .finish()
    }
}

impl SeparatorRegistry {
    /// Creates an empty separator registry.
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Creates a registry with default command metadata segments.
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.add(Box::new(ExitCodeSegment));
        registry.add(Box::new(CommandTextSegment));
        registry.add(Box::new(FoldBadgeSegment));
        registry.add(Box::new(DurationSegment));
        registry.add(Box::new(TimestampSegment));
        registry.add(Box::new(CwdSegment));
        registry
    }

    /// Adds a segment to the registry.
    pub fn add(&mut self, segment: Box<dyn SeparatorSegment>) {
        self.segments.push(segment);
    }

    /// Renders a full separator line with optional metadata.
    pub fn render(
        &self,
        record: Option<&CommandRecord>,
        cols: usize,
        gutter_width: usize,
        theme: &Theme,
        glyphs: &GlyphSet,
    ) -> String {
        let dash_str = String::from(glyphs.separator.dash);
        let content_cols = cols.saturating_sub(gutter_width);
        if content_cols == 0 {
            return " ".repeat(gutter_width);
        }

        let mut line = String::new();
        if gutter_width > 0 {
            line.push_str(&" ".repeat(gutter_width));
        }

        let marker_fg = color_to_fg_ansi(theme.marker_fg);
        line.push_str(&marker_fg);

        if let Some(record) = record {
            let mut parts: Vec<RenderedSepPart> = self
                .segments
                .iter()
                .filter_map(|segment| segment.render(record, theme, glyphs))
                .collect();

            let command_cap = content_cols / 2;
            if command_cap > 0 {
                for part in &mut parts {
                    if part.priority == 1 && part.display_width > command_cap {
                        part.content = truncate_ansi_content(&part.content, command_cap);
                        part.display_width = strip_ansi_width(&part.content);
                    }
                }
            }

            if parts.is_empty() {
                line.push_str(&dash_str.repeat(content_cols));
                line.push_str("\x1b[39m");
                return line;
            }

            loop {
                let parts_width: usize = parts.iter().map(|part| part.display_width).sum();
                let gaps = parts.len().saturating_sub(1);
                // 2 spaces around content + 2 dashes per side minimum.
                let total = parts_width + gaps + 2 + 4;
                if total <= content_cols || parts.is_empty() {
                    break;
                }

                if let Some(idx) = parts
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, part)| part.priority)
                    .map(|(idx, _)| idx)
                {
                    parts.remove(idx);
                } else {
                    break;
                }
            }

            if parts.is_empty() {
                line.push_str(&dash_str.repeat(content_cols));
                line.push_str("\x1b[39m");
                return line;
            }

            let parts_width: usize = parts.iter().map(|part| part.display_width).sum();
            let gaps = parts.len().saturating_sub(1);
            let center_width = parts_width + gaps + 2;
            if center_width >= content_cols {
                line.push_str(&dash_str.repeat(content_cols));
                line.push_str("\x1b[39m");
                return line;
            }

            let dash_total = content_cols - center_width;
            let left_dashes = (dash_total / 2).max(2).min(dash_total);
            let right_dashes = dash_total.saturating_sub(left_dashes);

            line.push_str(&dash_str.repeat(left_dashes));
            line.push(' ');
            for (idx, part) in parts.iter().enumerate() {
                if idx > 0 {
                    line.push(' ');
                }
                line.push_str(&part.content);
            }
            line.push(' ');
            line.push_str(&marker_fg);
            line.push_str(&dash_str.repeat(right_dashes));
            line.push_str("\x1b[39m");
            return line;
        }

        line.push_str(&dash_str.repeat(content_cols));
        line.push_str("\x1b[39m");
        line
    }
}

struct ExitCodeSegment;

impl SeparatorSegment for ExitCodeSegment {
    fn id(&self) -> &'static str {
        "exit_code"
    }

    fn render(
        &self,
        record: &CommandRecord,
        theme: &Theme,
        glyphs: &GlyphSet,
    ) -> Option<RenderedSepPart> {
        let code = record.exit_code?;
        if code == 0 {
            let fg = color_to_fg_ansi(theme.semantic_success);
            let success = glyphs.indicator.success;
            return Some(RenderedSepPart::new(format!("{fg}{success}"), 0));
        }

        let fg = color_to_fg_ansi(theme.semantic_error);
        let failure = glyphs.indicator.failure;
        Some(RenderedSepPart::new(format!("{fg}{failure} {code}"), 0))
    }
}

struct CommandTextSegment;

impl SeparatorSegment for CommandTextSegment {
    fn id(&self) -> &'static str {
        "command_text"
    }

    fn render(
        &self,
        record: &CommandRecord,
        theme: &Theme,
        _glyphs: &GlyphSet,
    ) -> Option<RenderedSepPart> {
        let command = record.command_text.as_deref()?.trim();
        if command.is_empty() {
            return None;
        }

        let fg = color_to_fg_ansi(theme.marker_fg);
        Some(RenderedSepPart::new(format!("{fg}{command}"), 1))
    }
}

struct FoldBadgeSegment;

impl SeparatorSegment for FoldBadgeSegment {
    fn id(&self) -> &'static str {
        "fold_badge"
    }

    fn render(
        &self,
        record: &CommandRecord,
        theme: &Theme,
        _glyphs: &GlyphSet,
    ) -> Option<RenderedSepPart> {
        if !record.folded {
            return None;
        }

        let prompt_line = record.prompt_line?;
        let folded_lines = prompt_line.saturating_sub(record.output_start.saturating_add(1));
        if folded_lines == 0 {
            return None;
        }
        let fg = color_to_fg_ansi(theme.text_secondary);
        Some(RenderedSepPart::new(
            format!("{fg}[{folded_lines} lines]"),
            0,
        ))
    }
}

struct DurationSegment;

impl SeparatorSegment for DurationSegment {
    fn id(&self) -> &'static str {
        "duration"
    }

    fn render(
        &self,
        record: &CommandRecord,
        theme: &Theme,
        _glyphs: &GlyphSet,
    ) -> Option<RenderedSepPart> {
        let duration = record.duration?;
        if duration.as_secs_f64() < 0.5 {
            return None;
        }

        let fg = color_to_fg_ansi(theme.text_secondary);
        Some(RenderedSepPart::new(
            format!("{fg}{}", format_duration(duration)),
            3,
        ))
    }
}

struct TimestampSegment;

impl SeparatorSegment for TimestampSegment {
    fn id(&self) -> &'static str {
        "timestamp"
    }

    fn render(
        &self,
        record: &CommandRecord,
        theme: &Theme,
        _glyphs: &GlyphSet,
    ) -> Option<RenderedSepPart> {
        let started_at = record.started_at?;
        let fg = color_to_fg_ansi(theme.text_secondary);
        Some(RenderedSepPart::new(
            format!("{fg}{}", started_at.format("%H:%M")),
            2,
        ))
    }
}

struct CwdSegment;

impl SeparatorSegment for CwdSegment {
    fn id(&self) -> &'static str {
        "cwd"
    }

    fn render(
        &self,
        record: &CommandRecord,
        theme: &Theme,
        _glyphs: &GlyphSet,
    ) -> Option<RenderedSepPart> {
        let cwd = record.cwd.as_ref()?;
        let display = format_cwd(cwd);
        if display.is_empty() {
            return None;
        }

        let fg = color_to_fg_ansi(theme.text_secondary);
        Some(RenderedSepPart::new(format!("{fg}{display}"), 4))
    }
}

fn format_cwd(path: &Path) -> String {
    let as_str = path.to_string_lossy();
    if as_str.is_empty() {
        return String::new();
    }

    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if as_str.starts_with(home_str.as_ref()) {
            let suffix = &as_str[home_str.len()..];
            if suffix.is_empty() {
                return "~".to_string();
            }
            if suffix.starts_with('/') {
                return format!("~{suffix}");
            }
        }
    }

    as_str.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::glyphs::GlyphTier;
    use crate::config::ThemePreset;
    use chrono::NaiveDate;

    #[test]
    fn test_separator_registry_render_without_record_returns_dash_line() {
        let registry = SeparatorRegistry::with_defaults();
        let theme = Theme::for_preset(ThemePreset::Amber);
        let glyphs = GlyphSet::for_tier(GlyphTier::Unicode);
        let rendered = registry.render(None, 20, 0, theme, glyphs);
        assert!(rendered.contains(glyphs.separator.dash));
    }

    #[test]
    fn test_separator_registry_render_with_exit_code_contains_status_symbol() {
        let registry = SeparatorRegistry::with_defaults();
        let theme = Theme::for_preset(ThemePreset::Amber);
        let glyphs = GlyphSet::for_tier(GlyphTier::Unicode);
        let record = CommandRecord {
            output_start: 10,
            prompt_line: Some(20),
            command_text: Some("ls".to_string()),
            exit_code: Some(0),
            ..Default::default()
        };

        let rendered = registry.render(Some(&record), 40, 0, theme, glyphs);
        assert!(rendered.contains(glyphs.indicator.success));
    }

    #[test]
    fn test_separator_registry_render_with_cwd_reapplies_marker_color_before_right_dashes() {
        let registry = SeparatorRegistry::with_defaults();
        let theme = Theme::for_preset(ThemePreset::Amber);
        let glyphs = GlyphSet::for_tier(GlyphTier::Unicode);
        let started_at = NaiveDate::from_ymd_opt(2026, 2, 8)
            .and_then(|date| date.and_hms_opt(0, 58, 0))
            .expect("valid test timestamp");
        let record = CommandRecord {
            output_start: 10,
            prompt_line: Some(20),
            command_text: Some("ping 8.8.8.8".to_string()),
            exit_code: Some(0),
            duration: Some(std::time::Duration::from_secs(2)),
            cwd: Some(std::path::PathBuf::from("/tmp")),
            started_at: Some(started_at),
            ..Default::default()
        };

        let marker_fg = color_to_fg_ansi(theme.marker_fg);
        let rendered = registry.render(Some(&record), 120, 0, theme, glyphs);
        assert!(
            rendered.contains(&format!("/tmp {}{}", marker_fg, glyphs.separator.dash)),
            "separator should restore marker color before trailing dashes: {rendered:?}"
        );
    }

    #[test]
    fn test_separator_registry_render_when_folded_shows_hidden_line_count_badge() {
        let registry = SeparatorRegistry::with_defaults();
        let theme = Theme::for_preset(ThemePreset::Amber);
        let glyphs = GlyphSet::for_tier(GlyphTier::Unicode);
        let record = CommandRecord {
            output_start: 10,
            prompt_line: Some(15),
            folded: true,
            ..Default::default()
        };

        let rendered = registry.render(Some(&record), 80, 0, theme, glyphs);
        assert!(rendered.contains("[4 lines]"));
    }
}
