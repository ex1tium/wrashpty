//! Application configuration loaded from environment variables.
//!
//! This module handles detection of glyph tier and theme preferences.

pub use crate::chrome::glyphs::GlyphTier;

/// Theme preset for color scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemePreset {
    /// Amber monochrome - hardcoded RGB for vintage VT220 look.
    #[default]
    Amber,
    /// Terminal-native - uses ANSI colors, terminal controls appearance.
    /// Works with Konsole themes, iTerm2 profiles, etc.
    Terminal,
}

/// Configuration for the internal scrollback system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbackConfig {
    /// Whether scrollback is enabled.
    pub enabled: bool,
    /// Maximum number of lines to store.
    pub max_lines: usize,
    /// Maximum bytes per line before truncation.
    pub max_line_bytes: usize,
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_lines: 10_000,
            max_line_bytes: 4096,
        }
    }
}

impl ScrollbackConfig {
    /// Loads scrollback configuration from environment variables.
    ///
    /// # Environment Variables
    ///
    /// - `WRASHPTY_SCROLLBACK`: Set to `0`, `false`, or `no` to disable.
    ///   Defaults to enabled.
    ///
    /// - `WRASHPTY_SCROLLBACK_LINES`: Maximum lines to store (e.g., `50000`).
    ///   Defaults to 10,000.
    ///
    /// - `WRASHPTY_SCROLLBACK_LINE_BYTES`: Maximum bytes per line before
    ///   truncation (e.g., `8192`). Defaults to 4,096.
    pub fn from_env() -> Self {
        let enabled = !matches!(
            std::env::var("WRASHPTY_SCROLLBACK")
                .as_deref()
                .map(str::to_lowercase)
                .as_deref(),
            Ok("0") | Ok("false") | Ok("no") | Ok("off")
        );

        let max_lines = std::env::var("WRASHPTY_SCROLLBACK_LINES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10_000)
            .clamp(100, 1_000_000); // Between 100 and 1 million lines

        let max_line_bytes = std::env::var("WRASHPTY_SCROLLBACK_LINE_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4096)
            .clamp(256, 65_536); // Between 256 bytes and 64 KiB

        Self {
            enabled,
            max_lines,
            max_line_bytes,
        }
    }
}

/// Application-wide configuration loaded from environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// Which glyph tier to use for UI rendering.
    pub glyph_tier: GlyphTier,
    /// Which color theme to use.
    pub theme: ThemePreset,
    /// Scrollback buffer configuration.
    pub scrollback: ScrollbackConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            glyph_tier: GlyphTier::default(),
            theme: ThemePreset::Amber,
            scrollback: ScrollbackConfig::default(),
        }
    }
}

impl Config {
    /// Loads configuration from environment variables.
    ///
    /// # Environment Variables
    ///
    /// - `WRASHPTY_GLYPH_TIER`: Set to `ascii`, `unicode`, `emoji`, or `nerdfont`.
    ///   Falls back to `WRASHPTY_NERD_FONTS` for backward compatibility.
    ///   Defaults to `unicode`.
    ///
    /// - `WRASHPTY_THEME`: Set to `amber` or `retro` for amber monochrome theme.
    ///   Set to `terminal` or `native` to use terminal's color scheme.
    ///   Defaults to `amber`.
    pub fn from_env() -> Self {
        let glyph_tier = Self::detect_glyph_tier();
        let theme = Self::detect_theme();
        let scrollback = ScrollbackConfig::from_env();
        Self {
            glyph_tier,
            theme,
            scrollback,
        }
    }

    /// Detects glyph tier preference from environment.
    ///
    /// Checks `WRASHPTY_GLYPH_TIER` first, then falls back to the legacy
    /// `WRASHPTY_NERD_FONTS` variable. Defaults to `Unicode`.
    fn detect_glyph_tier() -> GlyphTier {
        // Check new env var first
        if let Ok(val) = std::env::var("WRASHPTY_GLYPH_TIER") {
            match val.to_lowercase().as_str() {
                "ascii" => return GlyphTier::Ascii,
                "unicode" => return GlyphTier::Unicode,
                "emoji" => return GlyphTier::Emoji,
                "nerd" | "nerdfont" => return GlyphTier::NerdFont,
                _ => {}
            }
        }

        // Legacy fallback
        match std::env::var("WRASHPTY_NERD_FONTS")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            Ok("0") | Ok("false") | Ok("no") | Ok("off") => GlyphTier::Ascii,
            Ok("1") | Ok("true") | Ok("yes") | Ok("on") => GlyphTier::NerdFont,
            _ => GlyphTier::Unicode, // Default
        }
    }

    /// Detects theme preference from environment.
    fn detect_theme() -> ThemePreset {
        match std::env::var("WRASHPTY_THEME")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            Ok("terminal") | Ok("native") | Ok("ansi") => ThemePreset::Terminal,
            Ok("amber") | Ok("retro") | Ok("vt220") => ThemePreset::Amber,
            _ => ThemePreset::Amber, // Default to amber
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.glyph_tier, GlyphTier::Unicode);
        assert_eq!(config.theme, ThemePreset::Amber);
        assert!(config.scrollback.enabled);
        assert_eq!(config.scrollback.max_lines, 10_000);
    }

    #[test]
    fn test_glyph_tier_default() {
        assert_eq!(GlyphTier::default(), GlyphTier::Unicode);
    }

    #[test]
    fn test_theme_preset_default() {
        assert_eq!(ThemePreset::default(), ThemePreset::Amber);
    }

    #[test]
    fn test_scrollback_config_default() {
        let config = ScrollbackConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_lines, 10_000);
        assert_eq!(config.max_line_bytes, 4096);
    }
}
