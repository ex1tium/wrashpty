//! Help output parser for multiple CLI formats.
//!
//! Handles parsing of --help output from various CLI frameworks:
//! - Clap (Rust)
//! - Cobra (Go)
//! - Argparse (Python)
//! - GNU standard
//! - And more

use regex::Regex;
use std::sync::LazyLock;
use tracing::debug;

use command_schema_core::{
    CommandSchema, FlagSchema, HelpFormat, SchemaSource, SubcommandSchema, ValueType,
};

/// Regex patterns for parsing help output.
static PATTERNS: LazyLock<HelpPatterns> = LazyLock::new(HelpPatterns::new);

struct HelpPatterns {
    // Flag patterns: -x, --long, -x/--long, -x, --long
    short_flag: Regex,
    long_flag: Regex,
    combined_flag: Regex,
    flag_with_value: Regex,

    // Section headers
    subcommands_section: Regex,
    flags_section: Regex,
    options_section: Regex,
    arguments_section: Regex,

    // Value indicators
    choice_values: Regex,

    // Version extraction
    version_number: Regex,
}

impl HelpPatterns {
    fn new() -> Self {
        Self {
            // -v, -x
            short_flag: Regex::new(r"^\s*(-[a-zA-Z])(?:\s|,|$)").unwrap(),
            // --verbose, --help
            long_flag: Regex::new(r"^\s*(--[a-zA-Z][-a-zA-Z0-9]*)(?:\s|=|$)").unwrap(),
            // -v, --verbose  OR  -v/--verbose
            combined_flag: Regex::new(
                r"^\s*(-[a-zA-Z])(?:\s*,\s*|\s*/\s*|\s+)(--[a-zA-Z][-a-zA-Z0-9]*)"
            ).unwrap(),
            // --flag=VALUE, --flag <value>, --flag [value], -f VALUE
            // Only match: =VALUE, <VALUE>, [value], or ALLCAPS right after flag
            flag_with_value: Regex::new(
                r"(?:=([A-Za-z_]+)|[<\[]([A-Za-z_]+)[>\]]|(?:--[a-zA-Z][-a-zA-Z0-9]*|-[a-zA-Z])\s+([A-Z][A-Z_]+)(?:\s|$))"
            ).unwrap(),

            // Section headers (case insensitive)
            subcommands_section: Regex::new(
                r"(?i)^(commands|subcommands|available commands|sub-commands)\s*:?\s*$"
            ).unwrap(),
            flags_section: Regex::new(
                r"(?i)^(flags|global flags)\s*:?\s*$"
            ).unwrap(),
            options_section: Regex::new(
                r"(?i)^(options|optional arguments|opts)\s*:?\s*$"
            ).unwrap(),
            arguments_section: Regex::new(
                r"(?i)^(arguments|positional arguments|args)\s*:?\s*$"
            ).unwrap(),

            // Value indicators
            choice_values: Regex::new(
                r"\{([^}]+)\}"
            ).unwrap(),

            // Version number extraction
            version_number: Regex::new(r"(\d+\.\d+(?:\.\d+)?)").unwrap(),
        }
    }
}

/// Parser for CLI help output.
pub struct HelpParser {
    command: String,
    raw_output: String,
    detected_format: Option<HelpFormat>,
    warnings: Vec<String>,
}

impl HelpParser {
    /// Creates a new parser for the given command and help output.
    pub fn new(command: &str, help_output: &str) -> Self {
        Self {
            command: command.to_string(),
            raw_output: help_output.to_string(),
            detected_format: None,
            warnings: Vec::new(),
        }
    }

    /// Parses the help output and returns a command schema.
    pub fn parse(&mut self) -> Option<CommandSchema> {
        if self.raw_output.trim().is_empty() {
            self.warnings.push("Empty help output".to_string());
            return None;
        }

        // Detect format
        self.detected_format = Some(self.detect_format());
        debug!(format = ?self.detected_format, "Detected help format");

        let mut schema = CommandSchema::new(&self.command, SchemaSource::HelpCommand);

        // Parse based on detected format - clone to owned strings to avoid borrow issues
        let lines: Vec<String> = self.raw_output.lines().map(String::from).collect();
        let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();

        // Extract version if present
        schema.version = self.extract_version(&line_refs);

        // Extract description (usually first non-empty line)
        schema.description = self.extract_description(&line_refs);

        // Parse sections - clone the section data to avoid borrow conflicts
        let sections = self.identify_sections_owned(&lines);

        // Extract subcommands
        if let Some(subcmd_lines) = sections.get("subcommands") {
            let refs: Vec<&str> = subcmd_lines.iter().map(|s| s.as_str()).collect();
            schema.subcommands = self.parse_subcommands(&refs);
        }

        // Extract flags/options
        if let Some(flag_lines) = sections.get("flags") {
            let refs: Vec<&str> = flag_lines.iter().map(|s| s.as_str()).collect();
            schema.global_flags.extend(self.parse_flags(&refs));
        }
        if let Some(option_lines) = sections.get("options") {
            let refs: Vec<&str> = option_lines.iter().map(|s| s.as_str()).collect();
            schema.global_flags.extend(self.parse_flags(&refs));
        }

        // Calculate confidence based on what we extracted
        schema.confidence = self.calculate_confidence(&schema);

        Some(schema)
    }

    /// Detects the help output format.
    fn detect_format(&self) -> HelpFormat {
        let output = &self.raw_output;

        // Clap (Rust) indicators
        if output.contains("USAGE:") && output.contains("FLAGS:") {
            return HelpFormat::Clap;
        }

        // Cobra (Go) indicators
        if output.contains("Available Commands:") && output.contains("Use \"") {
            return HelpFormat::Cobra;
        }

        // Argparse (Python) indicators
        if output.contains("positional arguments:") || output.contains("optional arguments:") {
            return HelpFormat::Argparse;
        }

        // Docopt indicators
        if output.contains("Usage:") && output.starts_with("Usage:") {
            return HelpFormat::Docopt;
        }

        // GNU style indicators
        if output.contains("--help") && output.contains("--version") {
            return HelpFormat::Gnu;
        }

        HelpFormat::Unknown
    }

    /// Extracts version string if present.
    fn extract_version(&self, lines: &[&str]) -> Option<String> {
        for line in lines.iter().take(5) {
            let line_lower = line.to_lowercase();
            if line_lower.contains("version") || line_lower.contains(" v") {
                // Try to extract version number using pre-compiled regex
                if let Some(cap) = PATTERNS.version_number.captures(line) {
                    return Some(cap[1].to_string());
                }
            }
        }
        None
    }

    /// Extracts description from help output.
    fn extract_description(&self, lines: &[&str]) -> Option<String> {
        for line in lines.iter().take(10) {
            let trimmed = line.trim();
            // Skip empty lines, usage lines, and section headers
            if trimmed.is_empty()
                || trimmed.to_lowercase().starts_with("usage")
                || trimmed.ends_with(':')
                || trimmed.starts_with('-')
            {
                continue;
            }
            // Found a description line
            if trimmed.len() > 10 && !trimmed.contains("--") {
                return Some(trimmed.to_string());
            }
        }
        None
    }

    /// Identifies sections in the help output (returns owned strings).
    fn identify_sections_owned(
        &self,
        lines: &[String],
    ) -> std::collections::HashMap<&'static str, Vec<String>> {
        let mut sections = std::collections::HashMap::new();
        let mut current_section: Option<&'static str> = None;
        let mut section_lines: Vec<String> = Vec::new();

        for line in lines {
            let trimmed = line.trim();

            // Check for section headers
            if PATTERNS.subcommands_section.is_match(trimmed) {
                if let Some(sec) = current_section {
                    sections.insert(sec, std::mem::take(&mut section_lines));
                }
                current_section = Some("subcommands");
                continue;
            }
            if PATTERNS.flags_section.is_match(trimmed) {
                if let Some(sec) = current_section {
                    sections.insert(sec, std::mem::take(&mut section_lines));
                }
                current_section = Some("flags");
                continue;
            }
            if PATTERNS.options_section.is_match(trimmed) {
                if let Some(sec) = current_section {
                    sections.insert(sec, std::mem::take(&mut section_lines));
                }
                current_section = Some("options");
                continue;
            }
            if PATTERNS.arguments_section.is_match(trimmed) {
                if let Some(sec) = current_section {
                    sections.insert(sec, std::mem::take(&mut section_lines));
                }
                current_section = Some("arguments");
                continue;
            }

            // Check for new section (line ending with colon, not a flag)
            if trimmed.ends_with(':') && !trimmed.starts_with('-') && trimmed.len() < 30 {
                if let Some(sec) = current_section {
                    sections.insert(sec, std::mem::take(&mut section_lines));
                }
                current_section = None; // Unknown section
                continue;
            }

            // Add line to current section
            if current_section.is_some() && !trimmed.is_empty() {
                section_lines.push(trimmed.to_string());
            }
        }

        // Don't forget the last section
        if let Some(sec) = current_section {
            sections.insert(sec, section_lines);
        }

        sections
    }

    /// Parses subcommand lines.
    fn parse_subcommands(&mut self, lines: &[&str]) -> Vec<SubcommandSchema> {
        let mut subcommands = Vec::new();

        for line in lines {
            // Common formats:
            // "  command     Description here"
            // "  command - Description here"
            // "    command    Description"

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('-') {
                continue;
            }

            // Split on multiple spaces or dash separator
            let parts: Vec<&str> = if trimmed.contains(" - ") {
                trimmed.splitn(2, " - ").collect()
            } else {
                trimmed.splitn(2, "  ").collect()
            };

            if let Some(name) = parts.first() {
                let name = name.trim();
                // Validate it looks like a command name
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                    && name.len() < 30
                {
                    let mut sub = SubcommandSchema::new(name);
                    if parts.len() > 1 {
                        sub.description = Some(parts[1].trim().to_string());
                    }
                    subcommands.push(sub);
                }
            }
        }

        subcommands
    }

    /// Parses flag/option lines.
    fn parse_flags(&mut self, lines: &[&str]) -> Vec<FlagSchema> {
        let mut flags = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];

            if let Some(flag) = self.parse_flag_line(line) {
                flags.push(flag);
            }

            i += 1;
        }

        flags
    }

    /// Parses a single flag line.
    fn parse_flag_line(&mut self, line: &str) -> Option<FlagSchema> {
        let trimmed = line.trim();
        if !trimmed.starts_with('-') {
            return None;
        }

        let mut short: Option<String> = None;
        let mut long: Option<String> = None;
        let mut takes_value = false;
        let mut value_type = ValueType::Bool;
        let mut description: Option<String> = None;

        // Try combined format first: -m, --message
        if let Some(caps) = PATTERNS.combined_flag.captures(trimmed) {
            short = Some(caps[1].to_string());
            long = Some(caps[2].to_string());
        } else if let Some(caps) = PATTERNS.long_flag.captures(trimmed) {
            long = Some(caps[1].to_string());
        } else if let Some(caps) = PATTERNS.short_flag.captures(trimmed) {
            short = Some(caps[1].to_string());
        } else {
            return None;
        }

        // Check for value indicator
        if PATTERNS.flag_with_value.is_match(trimmed) {
            takes_value = true;
            value_type = self.infer_value_type(trimmed);
        }

        let definition_part = trimmed
            .split_once("  ")
            .map_or(trimmed, |(def, _)| def.trim());

        // Also check for = or < > indicators in the flag definition itself.
        if (definition_part.contains('=')
            || definition_part.contains('<')
            || definition_part.contains('['))
            && !takes_value
        {
            takes_value = true;
            value_type = ValueType::String;
        }

        // Extract description (everything after the flag definition)
        // Usually separated by multiple spaces
        if let Some(desc_start) = trimmed.find("  ") {
            let desc = trimmed[desc_start..].trim();
            if !desc.is_empty() && !desc.starts_with('-') {
                description = Some(desc.to_string());
            }
        }

        Some(FlagSchema {
            short,
            long,
            value_type,
            takes_value,
            description,
            multiple: false,
            conflicts_with: Vec::new(),
            requires: Vec::new(),
        })
    }

    /// Infers value type from context clues.
    fn infer_value_type(&self, line: &str) -> ValueType {
        let line_lower = line.to_lowercase();

        // Check for file/path indicators
        if line_lower.contains("file") || line_lower.contains("path") {
            return ValueType::File;
        }
        if line_lower.contains("dir") || line_lower.contains("directory") {
            return ValueType::Directory;
        }
        if line_lower.contains("url") || line_lower.contains("uri") {
            return ValueType::Url;
        }
        if line_lower.contains("number")
            || line_lower.contains("count")
            || line_lower.contains("num")
        {
            return ValueType::Number;
        }

        // Check for choice values: {a,b,c} or (a|b|c)
        if let Some(caps) = PATTERNS.choice_values.captures(line) {
            let choices: Vec<String> = caps[1]
                .split([',', '|'])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !choices.is_empty() {
                return ValueType::Choice(choices);
            }
        }

        ValueType::String
    }

    /// Calculates confidence score based on extraction quality.
    fn calculate_confidence(&self, schema: &CommandSchema) -> f64 {
        let mut confidence: f64 = 0.5; // Base confidence

        // More subcommands = more confidence
        if !schema.subcommands.is_empty() {
            confidence += 0.2;
        }

        // More flags = more confidence
        if schema.global_flags.len() > 3 {
            confidence += 0.15;
        }

        // Known format = more confidence
        if self.detected_format != Some(HelpFormat::Unknown) {
            confidence += 0.1;
        }

        // Has description = more confidence
        if schema.description.is_some() {
            confidence += 0.05;
        }

        confidence.min(1.0_f64)
    }

    /// Returns any warnings encountered during parsing.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Returns the detected format.
    pub fn detected_format(&self) -> Option<HelpFormat> {
        self.detected_format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAP_HELP: &str = r#"
myapp 1.0.0
A sample application

USAGE:
    myapp [OPTIONS] <SUBCOMMAND>

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information
    -v, --verbose    Enable verbose output

OPTIONS:
    -c, --config <FILE>    Config file path
    -n, --count <NUM>      Number of iterations

SUBCOMMANDS:
    init      Initialize a new project
    build     Build the project
    run       Run the project
    help      Prints this message
"#;

    const COBRA_HELP: &str = r#"
A CLI tool for doing things

Usage:
  mytool [command]

Available Commands:
  completion  Generate completion script
  help        Help about any command
  serve       Start the server
  version     Print version info

Flags:
  -h, --help      help for mytool
  -v, --verbose   verbose output

Use "mytool [command] --help" for more information about a command.
"#;

    #[test]
    fn test_detect_clap_format() {
        let mut parser = HelpParser::new("myapp", CLAP_HELP);
        parser.parse();
        assert_eq!(parser.detected_format(), Some(HelpFormat::Clap));
    }

    #[test]
    fn test_detect_cobra_format() {
        let mut parser = HelpParser::new("mytool", COBRA_HELP);
        parser.parse();
        assert_eq!(parser.detected_format(), Some(HelpFormat::Cobra));
    }

    #[test]
    fn test_parse_clap_subcommands() {
        let mut parser = HelpParser::new("myapp", CLAP_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("init").is_some());
        assert!(schema.find_subcommand("build").is_some());
        assert!(schema.find_subcommand("run").is_some());
    }

    #[test]
    fn test_parse_clap_flags() {
        let mut parser = HelpParser::new("myapp", CLAP_HELP);
        let schema = parser.parse().unwrap();

        // Check for verbose flag
        let verbose = schema
            .global_flags
            .iter()
            .find(|f| f.long.as_deref() == Some("--verbose"));
        assert!(verbose.is_some());
        assert!(!verbose.unwrap().takes_value);

        // Check for config flag with value
        let config = schema
            .global_flags
            .iter()
            .find(|f| f.long.as_deref() == Some("--config"));
        assert!(config.is_some());
        assert!(config.unwrap().takes_value);
    }

    #[test]
    fn test_parse_cobra_subcommands() {
        let mut parser = HelpParser::new("mytool", COBRA_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("serve").is_some());
        assert!(schema.find_subcommand("version").is_some());
    }

    #[test]
    fn test_infer_file_value_type() {
        let parser = HelpParser::new("test", "");

        let vtype = parser.infer_value_type("--config <FILE>  Configuration file");
        assert_eq!(vtype, ValueType::File);
    }

    #[test]
    fn test_confidence_calculation() {
        let mut parser = HelpParser::new("myapp", CLAP_HELP);
        let schema = parser.parse().unwrap();

        // Should have decent confidence with subcommands and flags
        assert!(schema.confidence > 0.7);
    }

    #[test]
    fn test_parse_flag_line_description_brackets_do_not_imply_value() {
        let help = r#"
Flags:
  --color    Enable color output [default: auto]
"#;
        let mut parser = HelpParser::new("myapp", help);
        let schema = parser.parse().unwrap();

        let color = schema
            .global_flags
            .iter()
            .find(|f| f.long.as_deref() == Some("--color"))
            .unwrap();
        assert!(!color.takes_value);
        assert_eq!(color.value_type, ValueType::Bool);
    }
}
