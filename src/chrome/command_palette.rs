//! Command palette panel for quick command access.

use std::any::Any;
use std::fs;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::list::{List, ListItem};
use ratatui_widgets::paragraph::Paragraph;
use regex::Regex;
use tracing::debug;

use super::panel::{Panel, PanelResult};
use super::theme::Theme;

/// Source of a command in the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSource {
    /// Command from shell history.
    History,
    /// Target from Makefile.
    Makefile,
    /// Script from package.json.
    PackageJson,
    /// Cargo command from Cargo.toml presence.
    CargoToml,
    /// Target from Justfile.
    JustFile,
    /// Executable script file.
    Script,
    /// User-defined command.
    UserDefined,
}

impl CommandSource {
    /// Returns the icon for this command source.
    fn icon(&self) -> &'static str {
        match self {
            CommandSource::History => "H",
            CommandSource::Makefile => "M",
            CommandSource::PackageJson => "N",
            CommandSource::CargoToml => "C",
            CommandSource::JustFile => "J",
            CommandSource::Script => "S",
            CommandSource::UserDefined => "U",
        }
    }
}

/// An item in the command palette.
#[derive(Debug, Clone)]
pub struct CommandItem {
    /// Display name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Actual command to execute.
    pub command: String,
    /// Where the command came from.
    pub source: CommandSource,
    /// Frecency score for ranking.
    pub frecency_score: f64,
}

/// Command palette panel.
pub struct CommandPalettePanel {
    /// All discovered commands.
    items: Vec<CommandItem>,
    /// Indices of filtered items.
    filtered: Vec<usize>,
    /// Currently selected index in filtered list.
    selection: usize,
    /// Scroll offset for display.
    scroll_offset: usize,
    /// Current filter text.
    filter: String,
    /// Theme for rendering.
    theme: &'static Theme,
}

impl CommandPalettePanel {
    /// Creates a new empty command palette.
    pub fn new(theme: &'static Theme) -> Self {
        Self {
            items: Vec::new(),
            filtered: Vec::new(),
            selection: 0,
            scroll_offset: 0,
            filter: String::new(),
            theme,
        }
    }

    /// Loads commands from the given directory.
    pub fn load_commands(&mut self, cwd: &Path) {
        self.items.clear();

        self.load_makefile_targets(cwd);
        self.load_package_json_scripts(cwd);
        self.load_cargo_commands(cwd);
        self.load_justfile_targets(cwd);
        self.load_detected_scripts(cwd);

        // Sort by frecency (higher is better)
        // Use total_cmp for NaN-safe comparison (treats NaN as greater than all values)
        self.items
            .sort_by(|a, b| b.frecency_score.total_cmp(&a.frecency_score));

        self.apply_filter();

        debug!(
            "Loaded {} commands from {}",
            self.items.len(),
            cwd.display()
        );
    }

    /// Loads Makefile targets.
    fn load_makefile_targets(&mut self, cwd: &Path) {
        let makefile_path = cwd.join("Makefile");
        if !makefile_path.exists() {
            return;
        }

        let content = match fs::read_to_string(&makefile_path) {
            Ok(c) => c,
            Err(_) => return,
        };

        // Match lines like "target:" at the start of a line
        let re = match Regex::new(r"(?m)^([a-zA-Z_][a-zA-Z0-9_-]*):\s*") {
            Ok(r) => r,
            Err(_) => return,
        };

        for cap in re.captures_iter(&content) {
            if let Some(target) = cap.get(1) {
                let name = target.as_str();
                // Skip targets starting with .
                if name.starts_with('.') {
                    continue;
                }

                self.items.push(CommandItem {
                    name: name.to_string(),
                    description: "Makefile target".to_string(),
                    command: format!("make {}", name),
                    source: CommandSource::Makefile,
                    frecency_score: 50.0,
                });
            }
        }
    }

    /// Loads package.json scripts.
    fn load_package_json_scripts(&mut self, cwd: &Path) {
        let package_path = cwd.join("package.json");
        if !package_path.exists() {
            return;
        }

        let content = match fs::read_to_string(&package_path) {
            Ok(c) => c,
            Err(_) => return,
        };

        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => return,
        };

        if let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) {
            for (name, value) in scripts {
                let desc = value.as_str().unwrap_or("").to_string();
                self.items.push(CommandItem {
                    name: name.clone(),
                    description: crate::ui::text_width::truncate_to_width(&desc, 50).into_owned(),
                    command: format!("npm run {}", name),
                    source: CommandSource::PackageJson,
                    frecency_score: 60.0,
                });
            }
        }
    }

    /// Loads Cargo commands if Cargo.toml exists.
    fn load_cargo_commands(&mut self, cwd: &Path) {
        let cargo_path = cwd.join("Cargo.toml");
        if !cargo_path.exists() {
            return;
        }

        let commands = [
            ("build", "Compile the current package"),
            ("test", "Run the tests"),
            ("run", "Run a binary or example"),
            ("check", "Analyze the current package"),
            ("clippy", "Run clippy lints"),
            ("fmt", "Format the current package"),
        ];

        for (name, desc) in commands {
            self.items.push(CommandItem {
                name: name.to_string(),
                description: desc.to_string(),
                command: format!("cargo {}", name),
                source: CommandSource::CargoToml,
                frecency_score: 70.0,
            });
        }
    }

    /// Loads Justfile targets.
    fn load_justfile_targets(&mut self, cwd: &Path) {
        let justfile_path = cwd.join("justfile");
        let justfile_alt = cwd.join("Justfile");

        let content = if justfile_path.exists() {
            fs::read_to_string(&justfile_path).ok()
        } else if justfile_alt.exists() {
            fs::read_to_string(&justfile_alt).ok()
        } else {
            None
        };

        let content = match content {
            Some(c) => c,
            None => return,
        };

        // Match recipe names (simplified - just names at start of line followed by :)
        let re = match Regex::new(r"(?m)^([a-zA-Z_][a-zA-Z0-9_-]*):\s*") {
            Ok(r) => r,
            Err(_) => return,
        };

        for cap in re.captures_iter(&content) {
            if let Some(target) = cap.get(1) {
                let name = target.as_str();
                self.items.push(CommandItem {
                    name: name.to_string(),
                    description: "Just recipe".to_string(),
                    command: format!("just {}", name),
                    source: CommandSource::JustFile,
                    frecency_score: 55.0,
                });
            }
        }
    }

    /// Detects executable scripts in common directories.
    fn load_detected_scripts(&mut self, cwd: &Path) {
        let script_dirs = [".", "scripts", "bin", ".scripts"];
        let script_extensions = ["sh", "bash", "zsh", "py", "rb", "pl"];

        for dir_name in script_dirs {
            let dir_path = cwd.join(dir_name);
            if !dir_path.is_dir() {
                continue;
            }

            let entries = match fs::read_dir(&dir_path) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

                if !script_extensions.contains(&extension) {
                    continue;
                }

                // Check if executable
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = path.metadata() {
                        if metadata.permissions().mode() & 0o111 == 0 {
                            continue; // Not executable
                        }
                    }
                }

                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("script");

                let relative_path = if dir_name == "." {
                    format!("./{}", name)
                } else {
                    format!("{}/{}", dir_name, name)
                };

                self.items.push(CommandItem {
                    name: name.to_string(),
                    description: format!("Script in {}/", dir_name),
                    command: relative_path,
                    source: CommandSource::Script,
                    frecency_score: 40.0,
                });
            }
        }
    }

    /// Applies the current filter to the item list.
    fn apply_filter(&mut self) {
        self.filtered.clear();

        if self.filter.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            let filter_lower = self.filter.to_lowercase();
            for (i, item) in self.items.iter().enumerate() {
                if item.name.to_lowercase().contains(&filter_lower)
                    || item.description.to_lowercase().contains(&filter_lower)
                {
                    self.filtered.push(i);
                }
            }
        }

        self.selection = 0;
        self.scroll_offset = 0;
    }

    /// Ensures the selection is visible in the scroll window.
    fn ensure_visible(&mut self, visible_count: usize) {
        if self.selection < self.scroll_offset {
            self.scroll_offset = self.selection;
        } else if self.selection >= self.scroll_offset + visible_count {
            self.scroll_offset = self.selection.saturating_sub(visible_count - 1);
        }
    }

    /// Returns the currently selected command, if any.
    fn selected_command(&self) -> Option<&CommandItem> {
        self.filtered
            .get(self.selection)
            .and_then(|&i| self.items.get(i))
    }
}

// Note: Default is removed since CommandPalettePanel now requires a theme parameter

impl Panel for CommandPalettePanel {
    fn preferred_height(&self) -> u16 {
        10
    }

    fn title(&self) -> &str {
        "Commands"
    }

    fn render(&mut self, buffer: &mut Buffer, area: Rect) {
        if area.height < 3 || area.width < 10 {
            return;
        }

        // Create layout: filter input at top, list below
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);

        // Render filter input
        let filter_text = if self.filter.is_empty() {
            Span::styled(
                "Type to filter...",
                Style::default().fg(self.theme.text_secondary),
            )
        } else {
            Span::styled(&self.filter, Style::default().fg(self.theme.text_primary))
        };
        let filter_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(self.theme.text_highlight)),
            filter_text,
        ]);
        Paragraph::new(filter_line).render(chunks[0], buffer);

        // Show help message if no commands found
        if self.items.is_empty() {
            let help_lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "No commands detected in this directory.",
                    Style::default().fg(self.theme.text_secondary),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "This panel auto-discovers commands from:",
                    Style::default().fg(self.theme.text_primary),
                )),
                Line::from(Span::styled(
                    "  - Makefile targets",
                    Style::default().fg(self.theme.text_secondary),
                )),
                Line::from(Span::styled(
                    "  - package.json scripts (npm)",
                    Style::default().fg(self.theme.text_secondary),
                )),
                Line::from(Span::styled(
                    "  - Cargo.toml (cargo commands)",
                    Style::default().fg(self.theme.text_secondary),
                )),
                Line::from(Span::styled(
                    "  - justfile recipes",
                    Style::default().fg(self.theme.text_secondary),
                )),
                Line::from(Span::styled(
                    "  - Scripts in ./scripts/, ./bin/",
                    Style::default().fg(self.theme.text_secondary),
                )),
            ];
            Paragraph::new(help_lines).render(chunks[1], buffer);
            return;
        }

        // Calculate visible items
        let visible_height = chunks[1].height as usize;
        self.ensure_visible(visible_height);

        // Render item list
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .skip(self.scroll_offset)
            .take(visible_height)
            .enumerate()
            .map(|(display_idx, &item_idx)| {
                let item = &self.items[item_idx];
                let actual_idx = self.scroll_offset + display_idx;
                let is_selected = actual_idx == self.selection;

                // Use theme-based colors for source icons (amber tints)
                let icon_style = Style::default().fg(self.theme.text_secondary);
                let name_style = if is_selected {
                    Style::default()
                        .fg(self.theme.selection_fg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(self.theme.text_primary)
                };
                let desc_style = Style::default().fg(self.theme.text_secondary);

                let line = Line::from(vec![
                    Span::styled(format!("[{}] ", item.source.icon()), icon_style),
                    Span::styled(&item.name, name_style),
                    Span::styled(format!("  {}", item.description), desc_style),
                ]);

                if is_selected {
                    ListItem::new(line).style(Style::default().bg(self.theme.selection_bg))
                } else {
                    ListItem::new(line)
                }
            })
            .collect();

        let list = List::new(items);
        list.render(chunks[1], buffer);
    }

    fn handle_input(&mut self, key: KeyEvent) -> PanelResult {
        match key.code {
            KeyCode::Esc => PanelResult::Dismiss,
            KeyCode::Enter => {
                if let Some(cmd) = self.selected_command() {
                    PanelResult::Execute(cmd.command.clone())
                } else {
                    PanelResult::Dismiss
                }
            }
            KeyCode::Up => {
                if self.selection > 0 {
                    self.selection -= 1;
                }
                PanelResult::Continue
            }
            KeyCode::Down => {
                if self.selection + 1 < self.filtered.len() {
                    self.selection += 1;
                }
                PanelResult::Continue
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.apply_filter();
                PanelResult::Continue
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.apply_filter();
                PanelResult::Continue
            }
            _ => PanelResult::Continue,
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::AMBER_THEME;
    use super::*;

    #[test]
    fn test_command_palette_panel_new_initial_state_empty_selection() {
        let panel = CommandPalettePanel::new(&AMBER_THEME);
        assert!(panel.items.is_empty());
        assert!(panel.filtered.is_empty());
        assert_eq!(panel.selection, 0);
    }

    #[test]
    fn test_command_source_icon_variants_return_expected_icons() {
        assert_eq!(CommandSource::Makefile.icon(), "M");
        assert_eq!(CommandSource::PackageJson.icon(), "N");
        assert_eq!(CommandSource::CargoToml.icon(), "C");
    }

    #[test]
    fn test_command_palette_panel_apply_filter_no_filter_returns_all_items() {
        let mut panel = CommandPalettePanel::new(&AMBER_THEME);
        panel.items.push(CommandItem {
            name: "test".to_string(),
            description: "Test command".to_string(),
            command: "test".to_string(),
            source: CommandSource::UserDefined,
            frecency_score: 1.0,
        });
        panel.apply_filter();
        assert_eq!(panel.filtered.len(), 1);
    }

    #[test]
    fn test_command_palette_panel_apply_filter_with_matching_filter_returns_matching_index() {
        let mut panel = CommandPalettePanel::new(&AMBER_THEME);
        panel.items.push(CommandItem {
            name: "build".to_string(),
            description: "Build project".to_string(),
            command: "make build".to_string(),
            source: CommandSource::Makefile,
            frecency_score: 1.0,
        });
        panel.items.push(CommandItem {
            name: "test".to_string(),
            description: "Run tests".to_string(),
            command: "make test".to_string(),
            source: CommandSource::Makefile,
            frecency_score: 1.0,
        });
        panel.filter = "build".to_string();
        panel.apply_filter();
        assert_eq!(panel.filtered.len(), 1);
        assert_eq!(panel.filtered[0], 0);
    }
}
