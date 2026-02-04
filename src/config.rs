//! Application configuration loaded from environment variables.
//!
//! This module handles detection of nerdfont support and theme preferences.

/// Symbol set preference for UI rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SymbolSet {
    /// Rich Nerd Font symbols (requires font installation).
    NerdFont,
    /// Safe ASCII/basic Unicode fallback.
    #[default]
    Fallback,
}

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

/// Application-wide configuration loaded from environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// Which symbol set to use for UI rendering.
    pub symbol_set: SymbolSet,
    /// Which color theme to use.
    pub theme: ThemePreset,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            symbol_set: SymbolSet::Fallback,
            theme: ThemePreset::Amber,
        }
    }
}

impl Config {
    /// Loads configuration from environment variables.
    ///
    /// # Environment Variables
    ///
    /// - `WRASHPTY_NERD_FONTS`: Set to `1`, `true`, or `yes` to enable nerdfonts.
    ///   Set to `0`, `false`, or `no` to disable (overrides terminal hints).
    ///   If unset, auto-detects based on terminal.
    ///
    /// - `WRASHPTY_THEME`: Set to `amber` or `retro` for amber monochrome theme.
    ///   Set to `terminal` or `native` to use terminal's color scheme.
    ///   Defaults to `amber`.
    pub fn from_env() -> Self {
        let symbol_set = Self::detect_symbol_set();
        let theme = Self::detect_theme();
        Self { symbol_set, theme }
    }

    /// Detects whether to use nerdfonts based on environment.
    fn detect_symbol_set() -> SymbolSet {
        // 1. Explicit env var takes priority
        if let Ok(val) = std::env::var("WRASHPTY_NERD_FONTS") {
            return match val.to_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => SymbolSet::NerdFont,
                "0" | "false" | "no" | "off" => SymbolSet::Fallback,
                _ => SymbolSet::Fallback,
            };
        }

        // 2. Terminal hints - modern terminals likely have nerdfont support
        if Self::is_nerdfont_capable_terminal() {
            return SymbolSet::NerdFont;
        }

        // 3. Default to fallback for safety
        SymbolSet::Fallback
    }

    /// Checks for known nerdfont-capable terminals via environment variables.
    fn is_nerdfont_capable_terminal() -> bool {
        // Kitty terminal
        if std::env::var("KITTY_WINDOW_ID").is_ok() {
            return true;
        }

        // iTerm2 on macOS
        if std::env::var("ITERM_SESSION_ID").is_ok() {
            return true;
        }

        // Windows Terminal
        if std::env::var("WT_SESSION").is_ok() {
            return true;
        }

        // WezTerm
        if std::env::var("WEZTERM_PANE").is_ok() {
            return true;
        }

        // Alacritty (check TERM)
        if let Ok(term) = std::env::var("TERM") {
            if term.contains("alacritty") {
                return true;
            }
        }

        // Ghostty
        if std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
            return true;
        }

        false
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
        assert_eq!(config.symbol_set, SymbolSet::Fallback);
        assert_eq!(config.theme, ThemePreset::Amber);
    }

    #[test]
    fn test_symbol_set_default() {
        assert_eq!(SymbolSet::default(), SymbolSet::Fallback);
    }

    #[test]
    fn test_theme_preset_default() {
        assert_eq!(ThemePreset::default(), ThemePreset::Amber);
    }
}
