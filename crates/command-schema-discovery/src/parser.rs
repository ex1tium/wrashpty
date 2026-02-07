//! Help output parser for multiple CLI formats.
//!
//! Handles parsing of --help output from various CLI frameworks:
//! - Clap (Rust)
//! - Cobra (Go)
//! - Argparse (Python)
//! - GNU standard
//! - And more

use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use tracing::debug;

use command_schema_core::{
    ArgSchema, CommandSchema, FlagSchema, HelpFormat, SchemaSource, SubcommandSchema, ValueType,
};

/// Weighted score for a detected help-output format.
#[derive(Debug, Clone)]
pub struct FormatScore {
    pub format: HelpFormat,
    pub score: f64,
}

/// Diagnostics for a single parse run.
#[derive(Debug, Clone, Default)]
pub struct ParseDiagnostics {
    pub format_scores: Vec<FormatScore>,
    pub parsers_used: Vec<String>,
    pub relevant_lines: usize,
    pub recognized_lines: usize,
    pub unresolved_lines: Vec<String>,
}

impl ParseDiagnostics {
    pub fn coverage(&self) -> f64 {
        if self.relevant_lines == 0 {
            return 0.0;
        }
        self.recognized_lines as f64 / self.relevant_lines as f64
    }
}

#[derive(Debug, Clone)]
struct IndexedLine {
    index: usize,
    text: String,
}

#[derive(Debug, Clone)]
struct SectionEntry {
    index: usize,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Subcommands,
    Flags,
    Options,
    Arguments,
}

#[derive(Default)]
struct SectionBuckets {
    header_indices: Vec<usize>,
    subcommands: Vec<SectionEntry>,
    flags: Vec<SectionEntry>,
    options: Vec<SectionEntry>,
    arguments: Vec<SectionEntry>,
}

/// Regex patterns for parsing help output.
static PATTERNS: LazyLock<HelpPatterns> = LazyLock::new(HelpPatterns::new);

struct HelpPatterns {
    // Flag patterns: -x, --long, -x/--long, -x, --long
    short_flag: Regex,
    single_dash_word_flag: Regex,
    long_flag: Regex,
    combined_flag: Regex,
    flag_with_value: Regex,

    // Section headers
    subcommands_section: Regex,
    flags_section: Regex,
    options_section: Regex,
    arguments_section: Regex,
    column_break: Regex,

    // Value indicators
    choice_values: Regex,

    // Formatting artifacts
    line_of_dashes: Regex,

    // Version extraction
    version_number: Regex,
}

impl HelpPatterns {
    fn new() -> Self {
        Self {
            // -v, -x, -4, -0, -?, -@
            short_flag: Regex::new(r"^\s*(-[a-zA-Z0-9?@])(?:\s|,|\[|\||$)").unwrap(),
            // -chdir, -log-level, etc (single-dash long options used by some CLIs)
            single_dash_word_flag: Regex::new(
                r"^\s*(-[a-zA-Z][a-zA-Z0-9-]{1,})(?:\s|,|=|<|\[|\||$)"
            )
            .unwrap(),
            // --verbose, --help
            long_flag: Regex::new(r"^\s*(--[a-zA-Z][-a-zA-Z0-9.]*)(?:\s|=|\[|,|\||\)|$)").unwrap(),
            // -v, --verbose  OR  -v/--verbose
            combined_flag: Regex::new(
                r"^\s*(-[a-zA-Z0-9?@]{1,3})(?:\s*,\s*|\s*/\s*|\s+)(--[a-zA-Z][-a-zA-Z0-9.]*)"
            ).unwrap(),
            // --flag=VALUE, --flag <value>, --flag [value], -f VALUE
            // Only match: =VALUE, <VALUE>, [value], or ALLCAPS right after flag
            flag_with_value: Regex::new(
                r"(?:=([A-Za-z_]+)|[<\[]([A-Za-z_]+)[>\]]|(?:--[a-zA-Z][-a-zA-Z0-9.]*|-[a-zA-Z0-9]{1,3})\s+([A-Z][A-Z_]+)(?:\s|$))"
            ).unwrap(),

            // Section headers (case insensitive)
            subcommands_section: Regex::new(
                r"(?i)^(commands|all commands|subcommands|available commands|sub-commands)\s*:?\s*$"
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
            column_break: Regex::new(r"\t+| {2,}").unwrap(),

            // Value indicators
            choice_values: Regex::new(
                r"\{([^}]+)\}"
            ).unwrap(),

            line_of_dashes: Regex::new(r"^-{8,}$").unwrap(),

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
    diagnostics: ParseDiagnostics,
}

impl HelpParser {
    /// Creates a new parser for the given command and help output.
    pub fn new(command: &str, help_output: &str) -> Self {
        Self {
            command: command.to_string(),
            raw_output: help_output.to_string(),
            detected_format: None,
            warnings: Vec::new(),
            diagnostics: ParseDiagnostics::default(),
        }
    }

    /// Parses the help output and returns a command schema.
    pub fn parse(&mut self) -> Option<CommandSchema> {
        if self.raw_output.trim().is_empty() {
            self.warnings.push("Empty help output".to_string());
            return None;
        }

        let normalized = Self::normalize_help_output(&self.raw_output);
        let indexed_lines = Self::to_indexed_lines(&normalized);
        let line_refs: Vec<&str> = indexed_lines
            .iter()
            .map(|line| line.text.as_str())
            .collect();

        // Weighted format classification
        let format_scores = self.classify_formats(&line_refs);
        self.detected_format = format_scores.first().map(|score| score.format);
        debug!(format = ?self.detected_format, scores = ?format_scores.iter().map(|s| (s.format, s.score)).collect::<Vec<_>>(), "Detected help format");

        let mut schema = CommandSchema::new(&self.command, SchemaSource::HelpCommand);

        // Extract version if present
        schema.version = self.extract_version(&line_refs);

        // Extract description (usually first non-empty line)
        schema.description = self.extract_description(&line_refs);

        // Stage 1: parse explicit section blocks first (highest confidence).
        let sections = self.identify_sections(&indexed_lines);
        let mut recognized_indices: HashSet<usize> = HashSet::new();
        let mut parsers_used: Vec<String> = Vec::new();
        recognized_indices.extend(sections.header_indices.iter().copied());
        let keybinding_document = Self::looks_like_keybinding_document(&indexed_lines);

        // Capture usage rows as recognized structural context, even when we do
        // not derive additional schema entities from them.
        let usage_recognized = self.collect_usage_indices(&indexed_lines);
        if !usage_recognized.is_empty() {
            recognized_indices.extend(usage_recognized);
            parsers_used.push("usage-lines".to_string());
        }

        // Extract subcommands from explicit command sections.
        if !sections.subcommands.is_empty() {
            let refs: Vec<&str> = sections
                .subcommands
                .iter()
                .map(|entry| entry.text.as_str())
                .collect();
            let subcommands = self.parse_subcommands(&refs);
            if !subcommands.is_empty() {
                recognized_indices.extend(sections.subcommands.iter().map(|entry| entry.index));
                parsers_used.push("section-subcommands".to_string());
                schema.subcommands = subcommands;
            }
        }

        // Extract flags/options from explicit sections.
        if !sections.flags.is_empty() {
            let refs: Vec<&str> = sections
                .flags
                .iter()
                .map(|entry| entry.text.as_str())
                .collect();
            let flags = self.parse_flags(&refs);
            if !flags.is_empty() {
                recognized_indices.extend(sections.flags.iter().map(|entry| entry.index));
                parsers_used.push("section-flags".to_string());
                schema.global_flags.extend(flags);
            }
        }
        if !sections.options.is_empty() {
            let refs: Vec<&str> = sections
                .options
                .iter()
                .map(|entry| entry.text.as_str())
                .collect();
            let flags = self.parse_flags(&refs);
            if !flags.is_empty() {
                recognized_indices.extend(sections.options.iter().map(|entry| entry.index));
                parsers_used.push("section-options".to_string());
                schema.global_flags.extend(flags);
            }
        }
        if !sections.arguments.is_empty() {
            let refs: Vec<&str> = sections
                .arguments
                .iter()
                .map(|entry| entry.text.as_str())
                .collect();
            let args = self.parse_arguments_section(&refs);
            if !args.is_empty() {
                recognized_indices.extend(sections.arguments.iter().map(|entry| entry.index));
                parsers_used.push("section-arguments".to_string());
                schema.positional.extend(args);
            }
        }

        // Stage 2: format-aware and well-known structural fallbacks.

        // npm-style command lists (All commands:)
        if schema.subcommands.is_empty() {
            let (npm_subcommands, npm_recognized) = self.parse_npm_style_commands(&indexed_lines);
            if !npm_subcommands.is_empty() {
                recognized_indices.extend(npm_recognized);
                parsers_used.push("npm-command-list".to_string());
                schema.subcommands = npm_subcommands;
            }
        }

        // Generic two-column command rows when explicit command sections were not
        // identified (or were empty). This is still structural and should happen
        // before more permissive fallbacks.
        if schema.subcommands.is_empty() && !keybinding_document {
            let (generic_subcommands, generic_recognized) =
                self.parse_two_column_subcommands(&indexed_lines);
            if !generic_subcommands.is_empty() {
                recognized_indices.extend(generic_recognized);
                parsers_used.push("generic-two-column-subcommands".to_string());
                schema.subcommands = generic_subcommands;
            }
        } else if schema.subcommands.is_empty() && keybinding_document {
            parsers_used.push("generic-two-column-skipped:keybinding-doc".to_string());
        }

        // Stty-style named settings and similar rows are structural command
        // tokens, but often appear in mixed sections that the block parser does
        // not fully capture.
        let (named_settings, named_settings_recognized) =
            self.parse_named_setting_rows(&indexed_lines);
        if !named_settings.is_empty() {
            recognized_indices.extend(named_settings_recognized);
            parsers_used.push("named-setting-rows".to_string());
            schema.subcommands.extend(named_settings);
        }

        // Stage 3: flag extraction fallbacks.

        // GNU and many custom CLIs list additional flags outside explicit
        // "Flags/Options" sections, so always run this as a top-up pass.
        let (fallback_flags, fallback_recognized) = self.parse_sectionless_flags(&indexed_lines);
        if !fallback_flags.is_empty() {
            recognized_indices.extend(fallback_recognized);
            parsers_used.push("gnu-sectionless-flags".to_string());
            schema.global_flags.extend(fallback_flags);
        }

        // Compact usage fallback, e.g. tmux:
        // usage: tmux [-2CDlNuVv] [-c shell-command] ...
        if schema.global_flags.is_empty() {
            let (usage_flags, usage_recognized) = self.parse_usage_compact_flags(&indexed_lines);
            if !usage_flags.is_empty() {
                recognized_indices.extend(usage_recognized);
                parsers_used.push("usage-compact-flags".to_string());
                schema.global_flags.extend(usage_flags);
            }
        }

        if schema.positional.is_empty() {
            let (usage_args, usage_arg_recognized) =
                self.parse_usage_positionals(&indexed_lines, !schema.subcommands.is_empty());
            if !usage_args.is_empty() {
                recognized_indices.extend(usage_arg_recognized);
                parsers_used.push("usage-positionals".to_string());
                schema.positional.extend(usage_args);
            }
        }

        schema.global_flags = Self::dedupe_flags(schema.global_flags);
        schema.subcommands = Self::dedupe_subcommands(schema.subcommands);
        schema.positional = Self::dedupe_args(schema.positional);
        self.apply_flag_choice_hints(
            &indexed_lines,
            &mut schema.global_flags,
            &mut recognized_indices,
        );
        self.apply_choice_table_hints(
            &indexed_lines,
            &mut schema.global_flags,
            &mut recognized_indices,
        );

        // Calculate confidence based on what we extracted
        schema.confidence = self.calculate_confidence(&schema);
        self.diagnostics = self.build_diagnostics(
            &indexed_lines,
            recognized_indices,
            format_scores,
            parsers_used,
            schema.confidence,
        );

        Some(schema)
    }

    /// Extracts version string if present.
    fn extract_version(&self, lines: &[&str]) -> Option<String> {
        let base_command = self
            .command
            .split_whitespace()
            .next()
            .unwrap_or(&self.command)
            .to_ascii_lowercase();

        for line in lines.iter().take(5) {
            let trimmed = line.trim();
            let trimmed_lower = trimmed.to_ascii_lowercase();

            // Common pattern: "<command> 1.2.3 (...)"
            if trimmed_lower.starts_with(&(base_command.clone() + " "))
                && let Some(cap) = PATTERNS.version_number.captures(trimmed)
            {
                return Some(cap[1].to_string());
            }

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
            let lower = trimmed.to_lowercase();
            // Skip empty lines, usage lines, and section headers
            if trimmed.is_empty()
                || lower.starts_with("usage")
                || lower.starts_with("or:")
                || lower.starts_with("examples:")
                || lower.starts_with("example:")
                || trimmed.ends_with(':')
                || trimmed.starts_with('-')
                || trimmed.starts_with('[')
                || trimmed.starts_with('<')
            {
                continue;
            }
            // Found a description line
            if trimmed.len() > 10
                && !trimmed.contains("--")
                && !trimmed.contains('[')
                && !trimmed.contains(']')
                && !trimmed.contains("...")
            {
                return Some(trimmed.to_string());
            }
        }
        None
    }

    /// Identifies typed sections in the help output.
    fn identify_sections(&self, lines: &[IndexedLine]) -> SectionBuckets {
        let mut buckets = SectionBuckets::default();
        let mut current_section: Option<SectionKind> = None;

        for line in lines {
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(section) = self.detect_section_header(trimmed) {
                buckets.header_indices.push(line.index);
                current_section = Some(section);
                continue;
            }

            // Any unrecognized "X:" header terminates the current section.
            if trimmed.ends_with(':') && !trimmed.starts_with('-') && trimmed.len() < 40 {
                current_section = None;
                continue;
            }

            match current_section {
                Some(SectionKind::Subcommands) => buckets.subcommands.push(SectionEntry {
                    index: line.index,
                    text: trimmed.to_string(),
                }),
                Some(SectionKind::Flags) => buckets.flags.push(SectionEntry {
                    index: line.index,
                    text: trimmed.to_string(),
                }),
                Some(SectionKind::Options) => buckets.options.push(SectionEntry {
                    index: line.index,
                    text: trimmed.to_string(),
                }),
                Some(SectionKind::Arguments) => buckets.arguments.push(SectionEntry {
                    index: line.index,
                    text: trimmed.to_string(),
                }),
                None => {}
            }
        }

        buckets
    }

    fn detect_section_header(&self, trimmed: &str) -> Option<SectionKind> {
        if PATTERNS.subcommands_section.is_match(trimmed) {
            return Some(SectionKind::Subcommands);
        }
        let lower = trimmed.to_lowercase();
        if trimmed.ends_with(':')
            && trimmed.len() <= 64
            && Self::looks_like_subcommand_section_header(&lower)
        {
            return Some(SectionKind::Subcommands);
        }
        if trimmed.ends_with(':') && lower.contains("option") {
            return Some(SectionKind::Options);
        }
        if trimmed.ends_with(':') && lower.contains("flag") {
            return Some(SectionKind::Flags);
        }
        if PATTERNS.flags_section.is_match(trimmed) {
            return Some(SectionKind::Flags);
        }
        if PATTERNS.options_section.is_match(trimmed) {
            return Some(SectionKind::Options);
        }
        if PATTERNS.arguments_section.is_match(trimmed) {
            return Some(SectionKind::Arguments);
        }
        None
    }

    fn looks_like_subcommand_section_header(lower: &str) -> bool {
        let positives = [
            "command",
            "commands",
            "subcommand",
            "subcommands",
            "action",
            "actions",
            "workflow",
            "task",
            "tasks",
        ];
        if !positives.iter().any(|needle| lower.contains(needle)) {
            return false;
        }

        let negatives = [
            "variable",
            "option",
            "flag",
            "argument",
            "example",
            "column",
            "field",
            "property",
            "setting",
            "key",
            "keyboard",
        ];
        !negatives.iter().any(|needle| lower.contains(needle))
    }

    /// Parses subcommand lines.
    fn parse_subcommands(&self, lines: &[&str]) -> Vec<SubcommandSchema> {
        let mut subcommands = Vec::new();
        let mut seen_names = HashSet::new();

        for line in lines {
            // Common formats:
            // "  command     Description here"
            // "  command - Description here"
            // "    command    Description"

            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('-') {
                continue;
            }

            // npm-style "All commands" list can be comma-separated with no descriptions.
            if self.looks_like_command_list_line(trimmed) {
                for token in trimmed.split(',') {
                    let name = token.trim();
                    if Self::is_valid_command_name(name) && seen_names.insert(name.to_string()) {
                        subcommands.push(SubcommandSchema::new(name));
                    }
                }
                continue;
            }

            // Split on two-column boundaries or dash separator.
            let (name_part, description) =
                if let Some((head, desc)) = Self::split_dash_separator(trimmed) {
                    (head, Some(desc))
                } else if let Some((head, desc)) = Self::split_two_columns(trimmed) {
                    (head, Some(desc))
                } else {
                    (trimmed, None)
                };

            // Support alias forms such as "build, b".
            let mut names: Vec<&str> = name_part
                .split(',')
                .map(str::trim)
                .filter(|name| Self::is_valid_command_name(name))
                .collect();

            if names.is_empty() {
                if let Some(fallback_names) = self.parse_subcommand_name_candidates(name_part) {
                    for name in fallback_names {
                        if Self::is_valid_command_name(name) {
                            names.push(name);
                        }
                    }
                }
            }
            if names.is_empty() {
                continue;
            }

            let primary = names.remove(0);
            if !seen_names.insert(primary.to_string()) {
                continue;
            }

            let mut sub = SubcommandSchema::new(primary);
            if let Some(desc) = description.filter(|value| !value.is_empty()) {
                sub.description = Some(desc.to_string());
            }
            sub.aliases = names.into_iter().map(str::to_string).collect();
            subcommands.push(sub);
        }

        subcommands
    }

    fn parse_subcommand_name_candidates<'a>(&self, name_part: &'a str) -> Option<Vec<&'a str>> {
        // Handle rows like "start UNIT..." where first token is the real command
        // and the rest is usage placeholder syntax.
        let mut candidates = Vec::new();

        for segment in name_part.split(',') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            if Self::is_valid_command_name(segment) {
                candidates.push(segment);
                continue;
            }

            let mut tokens = segment.split_whitespace();
            let first = tokens.next()?;
            if !Self::is_valid_command_name(first) {
                return None;
            }

            let tail = segment[first.len()..].trim();
            if tail.is_empty() || Self::looks_like_argument_placeholder(tail) {
                candidates.push(first);
                continue;
            }

            return None;
        }

        if candidates.is_empty() {
            None
        } else {
            Some(candidates)
        }
    }

    fn looks_like_argument_placeholder(value: &str) -> bool {
        if value.is_empty() {
            return false;
        }

        let has_placeholder_markers = value.contains("...")
            || value.contains('<')
            || value.contains('>')
            || value.contains('[');
        if !has_placeholder_markers {
            // Without explicit placeholder markers, only accept tails that are
            // placeholder-like (e.g. "UNIT", "PATH FILE") and reject prose
            // such as "APT has Super Cow Powers."
            let has_lowercase = value.chars().any(|ch| ch.is_ascii_lowercase());
            if has_lowercase {
                return false;
            }
        }

        if !has_placeholder_markers && !value.chars().any(|ch| ch.is_ascii_uppercase()) {
            return false;
        }

        value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '_' | '-' | '.' | '[' | ']' | '<' | '>' | '/' | ':' | '|' | '+' | '?'
                )
                || ch.is_whitespace()
        })
    }

    fn parse_arguments_section(&self, lines: &[&str]) -> Vec<ArgSchema> {
        let mut positional = Vec::new();
        let mut seen = HashSet::new();

        for line in lines {
            let trimmed = line.trim();
            if trimmed.is_empty() || Self::looks_like_flag_row_start(trimmed) {
                continue;
            }

            let (left, description) = if let Some((name_col, desc_col)) = Self::split_two_columns(trimmed)
            {
                (name_col, Some(desc_col))
            } else {
                (trimmed, None)
            };

            for mut arg in Self::parse_argument_tokens(left) {
                if arg.description.is_none() {
                    arg.description = description.map(ToOwned::to_owned);
                }
                if arg.value_type == ValueType::String
                    && let Some(desc) = arg.description.as_deref()
                {
                    arg.value_type = self.infer_value_type(desc);
                }
                let key = arg.name.to_ascii_lowercase();
                if seen.insert(key) {
                    positional.push(arg);
                }
            }
        }

        positional
    }

    fn parse_argument_tokens(value: &str) -> Vec<ArgSchema> {
        let mut args = Vec::new();

        for raw in value.split_whitespace() {
            let token = raw.trim_matches(|ch| matches!(ch, ',' | ';' | ':'));
            if token.is_empty() || token.starts_with('-') || token == "|" || token == "or" {
                continue;
            }

            let mut multiple = token.contains("...");
            let required = !token.starts_with('[');

            let mut cleaned = token.trim_matches(|ch| matches!(ch, '[' | ']' | '<' | '>' | '(' | ')' | '{' | '}'));
            cleaned = cleaned.trim_start_matches('+');
            cleaned = cleaned.trim_end_matches("...");
            cleaned = cleaned.trim_matches(|ch| matches!(ch, ',' | ';' | ':'));

            if cleaned.is_empty() {
                continue;
            }
            if Self::is_placeholder_keyword(cleaned) {
                continue;
            }
            if !Self::looks_like_argument_name(cleaned) {
                continue;
            }
            if raw.ends_with("...") {
                multiple = true;
            }

            args.push(ArgSchema {
                name: cleaned.to_string(),
                value_type: Self::infer_argument_value_type(cleaned),
                required,
                multiple,
                description: None,
            });
        }

        args
    }

    fn parse_usage_positionals(
        &self,
        lines: &[IndexedLine],
        has_subcommands: bool,
    ) -> (Vec<ArgSchema>, HashSet<usize>) {
        let mut usage_text = String::new();
        let mut recognized = HashSet::new();
        let mut in_usage = false;

        for line in lines {
            let raw = line.text.as_str();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                if in_usage {
                    break;
                }
                continue;
            }

            if trimmed.to_ascii_lowercase().starts_with("usage:") {
                in_usage = true;
                recognized.insert(line.index);
                usage_text.push_str(trimmed);
                usage_text.push(' ');
                continue;
            }

            if !in_usage {
                continue;
            }

            if raw.starts_with(' ') || raw.starts_with('\t') {
                recognized.insert(line.index);
                usage_text.push_str(trimmed);
                usage_text.push(' ');
                continue;
            }

            break;
        }

        if usage_text.is_empty() {
            return (Vec::new(), HashSet::new());
        }

        let usage_lower = usage_text.to_ascii_lowercase();
        let mut args = Vec::new();
        let mut seen = HashSet::new();
        for raw in usage_text.split_whitespace() {
            let token = raw.trim_matches(|ch| matches!(ch, ',' | ';' | ':'));
            if token.eq_ignore_ascii_case("usage:") || token.starts_with('-') {
                continue;
            }
            if token.eq_ignore_ascii_case(&self.command) {
                continue;
            }
            if token.contains("::=") {
                continue;
            }

            let has_placeholder_markers =
                token.contains('<') || token.contains('[') || token.contains("...");
            let looks_upper_placeholder = token.chars().any(|ch| ch.is_ascii_uppercase())
                && token
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || matches!(ch, '_' | '[' | ']' | '.' | '+' | '<' | '>'));
            if !has_placeholder_markers && !looks_upper_placeholder {
                continue;
            }

            for mut arg in Self::parse_argument_tokens(token) {
                let lower = arg.name.to_ascii_lowercase();
                if has_subcommands && matches!(lower.as_str(), "command" | "subcommand" | "cmd") {
                    continue;
                }
                if matches!(
                    lower.as_str(),
                    "usage" | "options" | "option" | "flags" | "flag" | "args" | "arguments"
                ) {
                    continue;
                }
                // Avoid parsing grammar rows from network/route tools.
                if usage_lower.contains(":=") && arg.name.chars().all(|ch| ch.is_ascii_uppercase()) {
                    continue;
                }
                let key = arg.name.to_ascii_lowercase();
                if seen.insert(key) {
                    if token.starts_with('[') {
                        arg.required = false;
                    }
                    args.push(arg);
                }
            }
        }

        if args.is_empty() {
            (args, HashSet::new())
        } else {
            (args, recognized)
        }
    }

    fn infer_argument_value_type(name: &str) -> ValueType {
        let lower = name.to_ascii_lowercase();
        if lower.contains("file") {
            return ValueType::File;
        }
        if lower.contains("dir") || lower.contains("path") {
            return ValueType::Directory;
        }
        if lower.contains("url") || lower.contains("uri") {
            return ValueType::Url;
        }
        if lower.contains("num") || lower.contains("count") || lower.contains("size") {
            return ValueType::Number;
        }
        ValueType::String
    }

    fn is_placeholder_keyword(token: &str) -> bool {
        matches!(
            token.to_ascii_lowercase().as_str(),
            "options"
                | "option"
                | "flags"
                | "flag"
                | "args"
                | "arguments"
                | "usage"
                | "command"
                | "subcommand"
                | "commands"
        )
    }

    fn looks_like_argument_name(token: &str) -> bool {
        if token.is_empty() || token.len() > 64 {
            return false;
        }
        token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    }

    /// Parses flag/option lines.
    fn parse_flags(&self, lines: &[&str]) -> Vec<FlagSchema> {
        let mut flags = Vec::new();
        for line in lines {
            flags.extend(self.parse_flag_entries_from_line(line));
        }
        flags
    }

    fn parse_flag_entries_from_line(&self, line: &str) -> Vec<FlagSchema> {
        let trimmed = line.trim();
        if !Self::looks_like_flag_row_start(trimmed) {
            return Vec::new();
        }

        if let Some(flags) = self.parse_compact_short_cluster_flags(trimmed) {
            return Self::dedupe_flags(flags);
        }

        Self::split_packed_option_entries(trimmed)
            .into_iter()
            .filter_map(|entry| self.parse_flag_line(entry))
            .collect()
    }

    fn parse_compact_short_cluster_flags(&self, line: &str) -> Option<Vec<FlagSchema>> {
        let definition_part = Self::split_two_columns(line).map_or(line, |(left, _)| left);
        if !definition_part.contains(" or -") {
            return None;
        }

        let mut segments = definition_part.split(" or ").map(str::trim);
        let first = segments.next()?;
        if !Self::is_compact_short_cluster(first) {
            return None;
        }

        let mut flags = Vec::new();
        for ch in first.chars().skip(1) {
            flags.push(FlagSchema {
                short: Some(format!("-{ch}")),
                long: None,
                value_type: ValueType::Bool,
                takes_value: false,
                description: None,
                multiple: false,
                conflicts_with: Vec::new(),
                requires: Vec::new(),
            });
        }

        for segment in segments {
            if !segment.starts_with('-') {
                continue;
            }

            if let Some(mut flag) = self.parse_flag_line(segment) {
                // "or -c command" and similar forms indicate a value-taking flag
                // even when the value placeholder is lowercase prose.
                let mut parts = segment.split_whitespace();
                let _flag_token = parts.next();
                if let Some(value_hint) = parts.next()
                    && !value_hint.starts_with('-')
                    && !flag.takes_value
                {
                    flag.takes_value = true;
                    flag.value_type = self.infer_value_type(segment);
                }
                flags.push(flag);
            }
        }

        Some(flags)
    }

    /// Parses a single flag line.
    fn parse_flag_line(&self, line: &str) -> Option<FlagSchema> {
        let trimmed = line.trim();
        if !Self::looks_like_flag_row_start(trimmed) {
            return None;
        }

        let mut short: Option<String> = None;
        let mut long: Option<String> = None;
        let mut takes_value = false;
        let mut value_type = ValueType::Bool;
        let mut description: Option<String> = None;
        let mut inferred_multiple = false;

        // Try combined format first: -m, --message
        if let Some(caps) = PATTERNS.combined_flag.captures(trimmed) {
            short = Some(caps[1].to_string());
            long = Some(caps[2].to_string());
        } else if let Some(caps) = PATTERNS.long_flag.captures(trimmed) {
            long = Some(caps[1].to_string());
        } else if let Some(caps) = PATTERNS.single_dash_word_flag.captures(trimmed) {
            short = Some(caps[1].to_string());
        } else if let Some(caps) = PATTERNS.short_flag.captures(trimmed) {
            short = Some(caps[1].to_string());
        } else {
            return None;
        }

        if let Some(short_flag) = short.as_mut() {
            let (normalized, multiple) = Self::normalize_flag_token(short_flag);
            *short_flag = normalized;
            inferred_multiple = inferred_multiple || multiple;
        }
        if let Some(long_flag) = long.as_mut() {
            let (normalized, multiple) = Self::normalize_flag_token(long_flag);
            *long_flag = normalized;
            inferred_multiple = inferred_multiple || multiple;
        }

        // Check for value indicator
        if PATTERNS.flag_with_value.is_match(trimmed) {
            takes_value = true;
            value_type = self.infer_value_type(trimmed);
        }

        let (definition_part, parsed_description) =
            if let Some((def, desc)) = Self::split_two_columns(trimmed) {
                (def, Some(desc))
            } else {
                (trimmed, None)
            };

        // Also check for explicit value indicators in the flag definition itself.
        if (definition_part.contains('=') || definition_part.contains('<')) && !takes_value {
            takes_value = true;
            value_type = ValueType::String;
        }

        // Extract description from the second column if present.
        if let Some(desc) = parsed_description {
            if !desc.is_empty() && !desc.starts_with('-') {
                description = Some(desc.to_string());
            }
        }

        let mut conflicts_with = Vec::new();
        let mut requires = Vec::new();
        if let Some(desc) = description.as_deref() {
            let (parsed_conflicts, parsed_requires) = Self::extract_flag_relationships(desc);
            conflicts_with = parsed_conflicts;
            requires = parsed_requires;
        }

        let multiple = Self::infer_multiple_flag_occurrences(
            definition_part,
            description.as_deref(),
            inferred_multiple,
        );

        if let Some(this_short) = short.as_deref() {
            conflicts_with.retain(|item| item != this_short);
            requires.retain(|item| item != this_short);
        }
        if let Some(this_long) = long.as_deref() {
            conflicts_with.retain(|item| item != this_long);
            requires.retain(|item| item != this_long);
        }

        Some(FlagSchema {
            short,
            long,
            value_type,
            takes_value,
            description,
            multiple,
            conflicts_with,
            requires,
        })
    }

    fn normalize_flag_token(raw: &str) -> (String, bool) {
        let mut token = raw
            .trim()
            .trim_end_matches(',')
            .trim_end_matches(';')
            .to_string();
        let mut multiple = false;

        if token.starts_with("--[no-]") {
            token = format!("--{}", token.trim_start_matches("--[no-]"));
        }

        if token.ends_with("...") {
            token.truncate(token.len().saturating_sub(3));
            multiple = true;
        }
        while token.ends_with('.') {
            token.pop();
            multiple = true;
        }

        token = token
            .trim_end_matches(',')
            .trim_end_matches(';')
            .trim()
            .to_string();
        (token, multiple)
    }

    fn infer_multiple_flag_occurrences(
        definition: &str,
        description: Option<&str>,
        explicit: bool,
    ) -> bool {
        if explicit {
            return true;
        }

        let definition_lower = definition.to_ascii_lowercase();
        if definition_lower.contains("...")
            && (definition.contains('<')
                || definition.contains('[')
                || definition.contains('=')
                || definition.split_whitespace().count() == 1)
        {
            return true;
        }

        let description_lower = description.unwrap_or_default().to_ascii_lowercase();
        description_lower.contains("multiple times")
            || description_lower.contains("more than once")
            || description_lower.contains("repeatable")
            || description_lower.contains("can be used multiple times")
            || description_lower.contains("may be repeated")
    }

    fn extract_flag_relationships(description: &str) -> (Vec<String>, Vec<String>) {
        static FLAG_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(--[a-zA-Z][-a-zA-Z0-9.]*|-[a-zA-Z0-9?@]{1,3})").unwrap()
        });
        let lower = description.to_ascii_lowercase();
        let mut conflicts = Vec::new();
        let mut requires = Vec::new();

        let is_conflict = lower.contains("conflict")
            || lower.contains("cannot be used with")
            || lower.contains("mutually exclusive")
            || lower.contains("incompatible with")
            || lower.contains("overrides ");
        if is_conflict {
            for capture in FLAG_REF_RE.captures_iter(description) {
                let (normalized, _) = Self::normalize_flag_token(&capture[1]);
                if !normalized.is_empty() && !conflicts.contains(&normalized) {
                    conflicts.push(normalized);
                }
            }
        }

        let is_requirement = lower.contains("requires ")
            || lower.contains("require ")
            || lower.contains("must be used with")
            || lower.contains("only with")
            || lower.contains("equivalent to specifying both");
        if is_requirement {
            for capture in FLAG_REF_RE.captures_iter(description) {
                let (normalized, _) = Self::normalize_flag_token(&capture[1]);
                if !normalized.is_empty() && !requires.contains(&normalized) {
                    requires.push(normalized);
                }
            }
        }

        (conflicts, requires)
    }

    fn normalize_help_output(raw: &str) -> String {
        static ANSI_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap());
        static OVERSTRIKE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r".\x08").unwrap());

        let stripped = ANSI_RE.replace_all(raw, "");
        let mut cleaned = stripped.into_owned();
        while OVERSTRIKE_RE.is_match(&cleaned) {
            cleaned = OVERSTRIKE_RE.replace_all(&cleaned, "").into_owned();
        }
        let replaced = cleaned.replace("\r\n", "\n").replace('\r', "\n");

        let mut normalized: Vec<String> = Vec::new();
        for line in replaced.lines() {
            let trimmed_end = line.trim_end();
            let trimmed_start = trimmed_end.trim_start();

            // Keep paragraph boundaries.
            if trimmed_end.is_empty() {
                normalized.push(String::new());
                continue;
            }

            // Merge wrapped description lines into previous line.
            let is_wrapped_continuation = line.starts_with(' ')
                && !trimmed_start.ends_with(':')
                && normalized.last().is_some_and(|prev| {
                    let prev_trimmed = prev.trim();
                    let prev_is_flag = Self::looks_like_flag_row_start(prev_trimmed);
                    let prev_is_two_column_subcommand = Self::split_two_columns(prev_trimmed)
                        .is_some_and(|(left, _)| {
                            !left.starts_with('-')
                                && left
                                    .chars()
                                    .next()
                                    .is_some_and(|ch| ch.is_ascii_alphanumeric())
                        });
                    let starts_new_flag_row = Self::looks_like_flag_row_start(trimmed_start)
                        && !trimmed_start.contains(';');
                    let looks_like_subcommand = Self::looks_like_subcommand_entry(trimmed_start);
                    (prev_is_flag || prev_is_two_column_subcommand)
                        && !prev_trimmed.is_empty()
                        && (!prev_trimmed.ends_with(':') || prev_is_flag)
                        && (!looks_like_subcommand || prev_is_flag)
                        && !starts_new_flag_row
                });

            if is_wrapped_continuation {
                if let Some(prev) = normalized.last_mut() {
                    prev.push(' ');
                    prev.push_str(trimmed_start);
                }
                continue;
            }

            normalized.push(trimmed_end.to_string());
        }

        normalized.join("\n")
    }

    fn split_two_columns(line: &str) -> Option<(&str, &str)> {
        let capture = PATTERNS.column_break.find(line)?;
        let left = line[..capture.start()].trim();
        let right = line[capture.end()..].trim();
        if left.is_empty() || right.is_empty() {
            return None;
        }
        Some((left, right))
    }

    fn split_dash_separator(line: &str) -> Option<(&str, &str)> {
        let (head, tail) = line.split_once(" - ")?;
        let left = head.trim();
        let right = tail.trim();
        if left.is_empty() || right.is_empty() {
            return None;
        }
        Some((left, right))
    }

    fn split_packed_option_entries(line: &str) -> Vec<&str> {
        // Some CLIs compact multiple flag rows into one line:
        // "-f ...   -u ...". Split those rows before parsing.
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let mut entries = Vec::new();
        let mut start = 0usize;
        let bytes = trimmed.as_bytes();
        let mut idx = 0usize;
        while idx < bytes.len() {
            if bytes[idx] != b' ' {
                idx += 1;
                continue;
            }

            let run_start = idx;
            while idx < bytes.len() && bytes[idx] == b' ' {
                idx += 1;
            }
            let run_len = idx - run_start;
            if run_len < 2 || idx + 1 >= bytes.len() || bytes[idx] != b'-' {
                continue;
            }

            let next = bytes[idx + 1] as char;
            if next.is_ascii_whitespace() || next == '-' {
                continue;
            }

            let entry = trimmed[start..run_start].trim();
            if !entry.is_empty() {
                entries.push(entry);
            }
            start = idx;
        }

        let tail = trimmed[start..].trim();
        if !tail.is_empty() {
            entries.push(tail);
        }

        if entries.is_empty() {
            vec![trimmed]
        } else {
            entries
        }
    }

    fn looks_like_subcommand_entry(trimmed: &str) -> bool {
        let starts_like_name = trimmed
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric());

        if !starts_like_name {
            return false;
        }

        // "build, b    Build package" / "serve  Start server" patterns
        if trimmed.contains("  ") {
            return true;
        }

        // "access, adduser, audit" style command lists.
        trimmed
            .split(',')
            .filter(|part| !part.trim().is_empty())
            .all(|part| {
                part.trim()
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            })
    }

    fn to_indexed_lines(normalized: &str) -> Vec<IndexedLine> {
        normalized
            .lines()
            .enumerate()
            .map(|(index, text)| IndexedLine {
                index,
                text: text.to_string(),
            })
            .collect()
    }

    fn parse_two_column_subcommands(
        &self,
        lines: &[IndexedLine],
    ) -> (Vec<SubcommandSchema>, HashSet<usize>) {
        let mut recognized = HashSet::new();
        let mut subcommands = Vec::new();
        let mut current_block: Vec<SectionEntry> = Vec::new();
        let mut current_header: Option<String> = None;

        let flush_block = |parser: &HelpParser,
                           block: &mut Vec<SectionEntry>,
                           header: Option<&str>,
                           recognized_set: &mut HashSet<usize>,
                           out_subcommands: &mut Vec<SubcommandSchema>| {
            if block.len() >= 2 {
                if header.is_some_and(Self::is_non_command_block_header) {
                    block.clear();
                    return;
                }
                if Self::block_looks_like_keybinding_table(block) {
                    block.clear();
                    return;
                }
                let refs = block
                    .iter()
                    .map(|entry| entry.text.as_str())
                    .collect::<Vec<_>>();
                let parsed = parser.parse_subcommands(&refs);
                if !parsed.is_empty() {
                    recognized_set.extend(block.iter().map(|entry| entry.index));
                    out_subcommands.extend(parsed);
                }
            }
            block.clear();
        };

        for line in lines {
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                flush_block(
                    self,
                    &mut current_block,
                    current_header.as_deref(),
                    &mut recognized,
                    &mut subcommands,
                );
                current_header = None;
                continue;
            }

            if self.detect_section_header(trimmed).is_some()
                || trimmed.starts_with('-')
            {
                flush_block(
                    self,
                    &mut current_block,
                    current_header.as_deref(),
                    &mut recognized,
                    &mut subcommands,
                );
                current_header = None;
                continue;
            }

            if Self::is_block_header(trimmed) {
                flush_block(
                    self,
                    &mut current_block,
                    current_header.as_deref(),
                    &mut recognized,
                    &mut subcommands,
                );
                current_header = Some(trimmed.to_ascii_lowercase());
                continue;
            }

            let is_command_row = Self::split_two_columns(trimmed)
                .is_some_and(|(left, _)| {
                    self.is_generic_subcommand_name_column(left, current_header.as_deref())
                });

            if is_command_row {
                current_block.push(SectionEntry {
                    index: line.index,
                    text: trimmed.to_string(),
                });
            } else {
                flush_block(
                    self,
                    &mut current_block,
                    current_header.as_deref(),
                    &mut recognized,
                    &mut subcommands,
                );
            }
        }

        flush_block(
            self,
            &mut current_block,
            current_header.as_deref(),
            &mut recognized,
            &mut subcommands,
        );
        (Self::dedupe_subcommands(subcommands), recognized)
    }

    fn is_block_header(trimmed: &str) -> bool {
        if trimmed.ends_with(':') && trimmed.len() < 64 {
            return true;
        }

        let lower = trimmed.to_ascii_lowercase();
        lower.contains("summary of") && lower.contains("commands")
    }

    fn is_non_command_block_header(header: &str) -> bool {
        let lower = header.to_ascii_lowercase();
        if lower.contains("summary of") && lower.contains("commands") {
            return true;
        }
        let command_like = ["command", "subcommand", "action", "workflow", "task"];
        if command_like.iter().any(|needle| lower.contains(needle)) {
            return false;
        }

        let non_command_like = [
            "value",
            "values",
            "column",
            "columns",
            "field",
            "fields",
            "variable",
            "variables",
            "environment",
            "format",
            "formats",
            "style",
            "styles",
            "attribute",
            "attributes",
            "modifiers",
            "setting",
            "settings",
            "keys",
            "key",
        ];
        non_command_like
            .iter()
            .any(|needle| lower.contains(needle))
    }

    fn block_looks_like_keybinding_table(block: &[SectionEntry]) -> bool {
        let mut marker_rows = 0usize;
        let mut short_key_rows = 0usize;

        for entry in block {
            let Some((left, right)) = Self::split_two_columns(entry.text.as_str()) else {
                continue;
            };
            let left_trimmed = left.trim();
            let left_lower = left_trimmed.to_ascii_lowercase();
            let right_trimmed = right.trim();

            let explicit_marker = left_trimmed.contains("ESC")
                || left_lower.contains("ctrl")
                || left_lower.contains("arrow")
                || left_lower.contains("backspace")
                || left_lower.contains("delete")
                || left_trimmed.contains('^')
                || right_trimmed.contains('^');
            if explicit_marker {
                marker_rows += 1;
            }

            let single_token = !left_trimmed.contains(',')
                && left_trimmed
                    .split_whitespace()
                    .all(|token| token.len() <= 3)
                && left_trimmed
                    .chars()
                    .any(|ch| ch.is_ascii_alphabetic() || matches!(ch, '^' | '-'));
            if single_token {
                short_key_rows += 1;
            }
        }

        marker_rows > 0 || (block.len() >= 4 && short_key_rows * 2 >= block.len())
    }

    fn is_subcommand_name_column(&self, left: &str) -> bool {
        let lower = left.to_lowercase();
        let excluded = [
            "usage",
            "options",
            "flags",
            "commands",
            "all commands",
            "arguments",
            "examples",
            "example",
        ];
        if excluded.contains(&lower.as_str()) {
            return false;
        }
        if left.starts_with('-') {
            return false;
        }

        let names = left
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if names.is_empty() {
            return false;
        }

        names
            .iter()
            .all(|name| Self::is_valid_command_name(name) || name.chars().all(|ch| ch == '.'))
    }

    fn is_generic_subcommand_name_column(&self, left: &str, header: Option<&str>) -> bool {
        if header.is_some_and(Self::is_non_command_block_header) {
            return false;
        }
        if !self.is_subcommand_name_column(left) {
            return false;
        }

        let names = left
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if names.is_empty() {
            return false;
        }

        if names.iter().any(|name| {
            !name
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_lowercase())
        }) {
            return false;
        }

        if names
            .iter()
            .all(|name| !name.chars().any(|ch| ch.is_ascii_lowercase()))
        {
            return false;
        }

        if names
            .iter()
            .any(|name| Self::looks_like_placeholder_subcommand_token(name))
        {
            return false;
        }

        if names
            .iter()
            .any(|name| Self::looks_like_non_command_value_token(name))
        {
            return false;
        }

        true
    }

    fn looks_like_keybinding_document(lines: &[IndexedLine]) -> bool {
        let keybinding_markers = lines
            .iter()
            .map(|line| line.text.to_ascii_lowercase())
            .filter(|line| {
                line.contains("esc-")
                    || line.contains("ctrl-")
                    || line.contains("^")
                    || line.contains("leftarrow")
                    || line.contains("rightarrow")
                    || line.contains("summary of less commands")
            })
            .count();
        keybinding_markers >= 8
    }

    fn looks_like_placeholder_subcommand_token(token: &str) -> bool {
        let token = token.trim();
        if token.is_empty() {
            return true;
        }
        if token == "_" {
            return true;
        }
        if token.chars().all(|ch| ch.is_ascii_digit()) {
            return true;
        }
        if token.ends_with("...") {
            return true;
        }

        token.len() <= 4
            && token
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '-')
    }

    fn looks_like_non_command_value_token(token: &str) -> bool {
        let lower = token.trim().to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "none"
                | "off"
                | "numbered"
                | "existing"
                | "simple"
                | "never"
                | "nil"
                | "all"
                | "auto"
                | "always"
                | "default"
                | "older"
                | "warn"
                | "warn-nopipe"
                | "exit"
                | "exit-nopipe"
                | "once"
                | "pages"
                | "or"
                | "while"
                | "gnu"
                | "report"
                | "full"
        )
    }

    fn classify_formats(&self, lines: &[&str]) -> Vec<FormatScore> {
        let mut scores = vec![
            FormatScore {
                format: HelpFormat::Clap,
                score: 0.0,
            },
            FormatScore {
                format: HelpFormat::Cobra,
                score: 0.0,
            },
            FormatScore {
                format: HelpFormat::Gnu,
                score: 0.0,
            },
            FormatScore {
                format: HelpFormat::Argparse,
                score: 0.0,
            },
            FormatScore {
                format: HelpFormat::Docopt,
                score: 0.0,
            },
            FormatScore {
                format: HelpFormat::Unknown,
                score: 0.05,
            },
        ];

        let output = lines.join("\n");
        for score in &mut scores {
            score.score += match score.format {
                HelpFormat::Clap => {
                    let mut s = 0.0;
                    if output.contains("USAGE:") {
                        s += 0.35;
                    }
                    if output.contains("FLAGS:") {
                        s += 0.25;
                    }
                    if output.contains("OPTIONS:") {
                        s += 0.2;
                    }
                    if output.contains("SUBCOMMANDS:") || output.contains("Commands:") {
                        s += 0.2;
                    }
                    s
                }
                HelpFormat::Cobra => {
                    let mut s = 0.0;
                    if output.contains("Available Commands:") {
                        s += 0.5;
                    }
                    if output.contains("Use \"") && output.contains("--help") {
                        s += 0.35;
                    }
                    if output.contains("Flags:") {
                        s += 0.15;
                    }
                    s
                }
                HelpFormat::Gnu => {
                    let mut s = 0.0;
                    if output.contains("Usage:") {
                        s += 0.25;
                    }
                    if output.contains("--help") {
                        s += 0.2;
                    }
                    if output.contains("--version") {
                        s += 0.2;
                    }
                    if lines.iter().any(|line| line.trim_start().starts_with('-')) {
                        s += 0.2;
                    }
                    s
                }
                HelpFormat::Argparse => {
                    let mut s = 0.0;
                    if output.contains("positional arguments:") {
                        s += 0.45;
                    }
                    if output.contains("optional arguments:") {
                        s += 0.45;
                    }
                    s
                }
                HelpFormat::Docopt => {
                    if output.starts_with("Usage:") {
                        0.75
                    } else {
                        0.0
                    }
                }
                HelpFormat::Unknown | HelpFormat::Bsd => 0.0,
            };
        }

        scores.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scores
    }

    fn parse_npm_style_commands(
        &self,
        lines: &[IndexedLine],
    ) -> (Vec<SubcommandSchema>, HashSet<usize>) {
        let mut commands = Vec::new();
        let mut recognized = HashSet::new();
        let mut seen = HashSet::new();
        let mut in_all_commands = false;

        for line in lines {
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                continue;
            }

            if trimmed.eq_ignore_ascii_case("All commands:") {
                in_all_commands = true;
                recognized.insert(line.index);
                continue;
            }

            if !in_all_commands {
                continue;
            }

            if trimmed.ends_with(':') && !trimmed.contains(',') {
                break;
            }

            if !self.looks_like_command_list_line(trimmed) {
                continue;
            }

            recognized.insert(line.index);
            for token in trimmed.split(',') {
                let name = token.trim();
                if Self::is_valid_command_name(name) && seen.insert(name.to_string()) {
                    commands.push(SubcommandSchema::new(name));
                }
            }
        }

        (commands, recognized)
    }

    fn parse_sectionless_flags(&self, lines: &[IndexedLine]) -> (Vec<FlagSchema>, HashSet<usize>) {
        let mut flags = Vec::new();
        let mut recognized = HashSet::new();

        for line in lines {
            let trimmed = line.text.trim();
            if !Self::looks_like_flag_row_start(trimmed) {
                continue;
            }
            let parsed = self.parse_flag_entries_from_line(trimmed);
            if !parsed.is_empty() {
                flags.extend(parsed);
                recognized.insert(line.index);
            }
        }

        (flags, recognized)
    }

    fn parse_usage_compact_flags(
        &self,
        lines: &[IndexedLine],
    ) -> (Vec<FlagSchema>, HashSet<usize>) {
        static BRACKET_GROUP_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\[([^\]]+)\]").unwrap());

        let mut usage_text = String::new();
        let mut recognized = HashSet::new();
        let mut in_usage = false;

        for line in lines {
            let raw = line.text.as_str();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                if in_usage {
                    break;
                }
                continue;
            }

            if trimmed.to_lowercase().starts_with("usage:") {
                in_usage = true;
                recognized.insert(line.index);
                usage_text.push_str(trimmed);
                usage_text.push(' ');
                continue;
            }

            if !in_usage {
                continue;
            }

            // Continuation lines in usage blocks are typically indented.
            if raw.starts_with(' ') || raw.starts_with('\t') {
                recognized.insert(line.index);
                usage_text.push_str(trimmed);
                usage_text.push(' ');
                continue;
            }

            break;
        }

        if usage_text.is_empty() {
            return (Vec::new(), HashSet::new());
        }

        let mut flags = Vec::new();
        for capture in BRACKET_GROUP_RE.captures_iter(&usage_text) {
            let Some(group) = capture.get(1).map(|value| value.as_str().trim()) else {
                continue;
            };

            if group.is_empty() || !group.starts_with('-') {
                continue;
            }

            let tokens = group.split_whitespace().collect::<Vec<_>>();
            if tokens.is_empty() {
                continue;
            }

            let first = tokens[0];
            if first.starts_with("--") {
                let Some(long_name) = Self::normalize_long_flag_token(first) else {
                    continue;
                };
                let takes_value = tokens.get(1).is_some_and(|next| !next.starts_with('-'));
                flags.push(FlagSchema {
                    short: None,
                    long: Some(long_name),
                    value_type: if takes_value {
                        self.infer_value_type(group)
                    } else {
                        ValueType::Bool
                    },
                    takes_value,
                    description: None,
                    multiple: false,
                    conflicts_with: Vec::new(),
                    requires: Vec::new(),
                });
                continue;
            }

            if first.starts_with('-') && first.len() == 2 {
                let takes_value = tokens.get(1).is_some_and(|next| !next.starts_with('-'));
                flags.push(FlagSchema {
                    short: Some(first.to_string()),
                    long: None,
                    value_type: if takes_value {
                        self.infer_value_type(group)
                    } else {
                        ValueType::Bool
                    },
                    takes_value,
                    description: None,
                    multiple: false,
                    conflicts_with: Vec::new(),
                    requires: Vec::new(),
                });
                continue;
            }

            // Compact short cluster, e.g. -2CDlNuVv
            if first.starts_with('-')
                && first.len() > 2
                && first.chars().skip(1).all(|ch| ch.is_ascii_alphanumeric())
                && !first.contains('=')
            {
                for ch in first.chars().skip(1) {
                    flags.push(FlagSchema {
                        short: Some(format!("-{ch}")),
                        long: None,
                        value_type: ValueType::Bool,
                        takes_value: false,
                        description: None,
                        multiple: false,
                        conflicts_with: Vec::new(),
                        requires: Vec::new(),
                    });
                }
            }
        }

        (Self::dedupe_flags(flags), recognized)
    }

    fn parse_named_setting_rows(
        &self,
        lines: &[IndexedLine],
    ) -> (Vec<SubcommandSchema>, HashSet<usize>) {
        let mut recognized = HashSet::new();
        let mut subcommands = Vec::new();
        let mut seen = HashSet::new();

        for line in lines {
            let trimmed = line.text.trim();
            let Some((left, right)) = Self::split_two_columns(trimmed) else {
                continue;
            };
            if left.starts_with('-') || left.contains(' ') {
                continue;
            }
            if !Self::is_valid_command_name(left) {
                continue;
            }
            if left.chars().any(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit()) {
                continue;
            }
            if !left.chars().any(|ch| ch.is_ascii_lowercase()) {
                continue;
            }

            let right_lower = right.to_ascii_lowercase();
            let looks_like_setting = right_lower.starts_with("same as")
                || right_lower.starts_with("print ")
                || right_lower.starts_with("set ")
                || right_lower.starts_with("tell ");
            if !looks_like_setting {
                continue;
            }

            if seen.insert(left.to_string()) {
                let mut sub = SubcommandSchema::new(left);
                sub.description = Some(right.to_string());
                subcommands.push(sub);
                recognized.insert(line.index);
            }
        }

        (subcommands, recognized)
    }

    fn collect_usage_indices(&self, lines: &[IndexedLine]) -> HashSet<usize> {
        let mut recognized = HashSet::new();
        let mut in_usage = false;

        for line in lines {
            let raw = line.text.as_str();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                if in_usage {
                    break;
                }
                continue;
            }

            if trimmed.to_ascii_lowercase().starts_with("usage:") {
                in_usage = true;
                recognized.insert(line.index);
                continue;
            }

            if in_usage && (raw.starts_with(' ') || raw.starts_with('\t')) {
                recognized.insert(line.index);
                continue;
            }

            if in_usage {
                break;
            }
        }

        recognized
    }

    fn trim_value_suffix(flag: &str) -> &str {
        flag.find(&['[', '<', '='][..])
            .map_or(flag, |index| &flag[..index])
    }

    fn normalize_long_flag_token(token: &str) -> Option<String> {
        if !token.starts_with("--") {
            return None;
        }

        // Common optional-negation notation in usage strings: --[no-]verify
        if let Some(suffix) = token.strip_prefix("--[no-]") {
            let clean = Self::trim_value_suffix(suffix);
            if clean.is_empty() {
                return None;
            }
            return Some(format!("--{clean}"));
        }

        let clean = Self::trim_value_suffix(token);
        if clean.len() <= 2 {
            return None;
        }
        Some(clean.to_string())
    }

    fn looks_like_command_list_line(&self, line: &str) -> bool {
        line.contains(',')
            && line
                .split(',')
                .filter(|part| !part.trim().is_empty())
                .all(|part| {
                    let token = part.trim();
                    Self::is_valid_command_name(token)
                })
    }

    fn is_valid_command_name(value: &str) -> bool {
        !value.is_empty()
            && value.len() < 50
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    fn dedupe_flags(flags: Vec<FlagSchema>) -> Vec<FlagSchema> {
        let mut deduped: Vec<FlagSchema> = Vec::new();

        for flag in flags {
            if let Some(existing) = deduped
                .iter_mut()
                .find(|existing| Self::flags_overlap(existing, &flag))
            {
                Self::merge_flags(existing, flag);
            } else {
                deduped.push(flag);
            }
        }

        deduped
    }

    fn flags_overlap(left: &FlagSchema, right: &FlagSchema) -> bool {
        match (&left.long, &right.long) {
            (Some(left_long), Some(right_long)) if left_long == right_long => true,
            _ => match (&left.short, &right.short) {
                (Some(left_short), Some(right_short)) => left_short == right_short,
                _ => false,
            },
        }
    }

    fn merge_flags(target: &mut FlagSchema, incoming: FlagSchema) {
        if target.short.is_none() {
            target.short = incoming.short.clone();
        }
        if target.long.is_none() {
            target.long = incoming.long.clone();
        }

        if incoming.takes_value {
            target.takes_value = true;
            if target.value_type == ValueType::Bool || target.value_type == ValueType::String {
                target.value_type = incoming.value_type;
            }
        }

        if let Some(incoming_desc) = incoming.description {
            let replace = target
                .description
                .as_ref()
                .is_none_or(|existing| incoming_desc.len() > existing.len());
            if replace {
                target.description = Some(incoming_desc);
            }
        }

        target.multiple = target.multiple || incoming.multiple;
        Self::merge_string_list(&mut target.conflicts_with, incoming.conflicts_with);
        Self::merge_string_list(&mut target.requires, incoming.requires);
    }

    fn merge_string_list(target: &mut Vec<String>, incoming: Vec<String>) {
        for item in incoming {
            if !target.contains(&item) {
                target.push(item);
            }
        }
    }

    fn dedupe_subcommands(subcommands: Vec<SubcommandSchema>) -> Vec<SubcommandSchema> {
        let mut seen = HashSet::new();
        let mut deduped = Vec::new();

        for sub in subcommands {
            if seen.insert(sub.name.clone()) {
                deduped.push(sub);
            }
        }

        deduped
    }

    fn dedupe_args(args: Vec<ArgSchema>) -> Vec<ArgSchema> {
        let mut deduped: Vec<ArgSchema> = Vec::new();
        for arg in args {
            if let Some(existing) = deduped
                .iter_mut()
                .find(|item| item.name.eq_ignore_ascii_case(&arg.name))
            {
                existing.required = existing.required || arg.required;
                existing.multiple = existing.multiple || arg.multiple;
                if existing.description.is_none() {
                    existing.description = arg.description;
                }
                if existing.value_type == ValueType::String && arg.value_type != ValueType::String {
                    existing.value_type = arg.value_type;
                }
            } else {
                deduped.push(arg);
            }
        }
        deduped
    }

    fn apply_flag_choice_hints(
        &self,
        lines: &[IndexedLine],
        flags: &mut Vec<FlagSchema>,
        recognized: &mut HashSet<usize>,
    ) {
        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.text.trim();
            let Some(rest) = trimmed.strip_prefix("Valid arguments for ") else {
                continue;
            };
            let flag_name = rest.trim_end_matches(':').trim();
            if !flag_name.starts_with('-') {
                continue;
            }

            let mut next_index = idx + 1;
            while next_index < lines.len() && lines[next_index].text.trim().is_empty() {
                next_index += 1;
            }
            if next_index >= lines.len() {
                continue;
            }

            let choice_line = lines[next_index].text.trim();
            if !choice_line.contains(',') {
                continue;
            }
            let choices = choice_line
                .split(',')
                .map(str::trim)
                .filter(|token| Self::is_valid_command_name(token))
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if choices.is_empty() {
                continue;
            }

            if let Some(flag) = flags.iter_mut().find(|flag| {
                flag.short.as_deref() == Some(flag_name) || flag.long.as_deref() == Some(flag_name)
            }) {
                flag.takes_value = true;
                flag.value_type = ValueType::Choice(choices);
            } else {
                flags.push(FlagSchema {
                    short: Some(flag_name.to_string()),
                    long: None,
                    value_type: ValueType::Choice(choices),
                    takes_value: true,
                    description: Some(format!("Valid arguments for {flag_name}")),
                    multiple: false,
                    conflicts_with: Vec::new(),
                    requires: Vec::new(),
                });
            }
            recognized.insert(line.index);
            recognized.insert(lines[next_index].index);
        }
    }

    fn apply_choice_table_hints(
        &self,
        lines: &[IndexedLine],
        flags: &mut Vec<FlagSchema>,
        recognized: &mut HashSet<usize>,
    ) {
        static VALID_ARGUMENTS_FOR_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?i)^valid arguments for\s+((?:--?)[a-zA-Z0-9?@][a-zA-Z0-9?@.-]*)\s*:\s*$")
                .unwrap()
        });
        static PLACEHOLDER_VALUES_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^([A-Z][A-Z0-9_-]{1,})\s+is one of the following\s*:\s*$").unwrap()
        });
        static PLACEHOLDER_DETERMINES_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^([A-Z][A-Z0-9_-]{1,})\s+determines\b.*:\s*$").unwrap()
        });
        static GENERIC_VALUES_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"(?i)^.*\b(here are the values|possible values|available values)\b.*:?\s*$",
            )
            .unwrap()
        });
        static FLAG_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(--[a-zA-Z][-a-zA-Z0-9.]*|-[a-zA-Z0-9?@]{1,3})").unwrap()
        });

        #[derive(Clone)]
        enum ChoiceTarget {
            Flag(String),
            Placeholder(String),
        }

        for idx in 0..lines.len() {
            let trimmed = lines[idx].text.trim();
            if trimmed.is_empty() {
                continue;
            }

            let target = if let Some(cap) = VALID_ARGUMENTS_FOR_RE.captures(trimmed) {
                let (normalized, _) = Self::normalize_flag_token(&cap[1]);
                Some(ChoiceTarget::Flag(normalized))
            } else if let Some(cap) = PLACEHOLDER_VALUES_RE.captures(trimmed) {
                Some(ChoiceTarget::Placeholder(
                    cap.get(1).map_or("", |m| m.as_str()).to_string(),
                ))
            } else if let Some(cap) = PLACEHOLDER_DETERMINES_RE.captures(trimmed) {
                Some(ChoiceTarget::Placeholder(
                    cap.get(1).map_or("", |m| m.as_str()).to_string(),
                ))
            } else if GENERIC_VALUES_HEADER_RE.is_match(trimmed) {
                let context_start = idx.saturating_sub(3);
                let mut from_context: Option<ChoiceTarget> = None;
                for context_line in lines[context_start..idx].iter().rev() {
                    let context = context_line.text.trim();
                    if context.is_empty() {
                        continue;
                    }
                    if let Some(cap) = PLACEHOLDER_VALUES_RE.captures(context) {
                        from_context = Some(ChoiceTarget::Placeholder(
                            cap.get(1).map_or("", |m| m.as_str()).to_string(),
                        ));
                        break;
                    }
                    if let Some(cap) = PLACEHOLDER_DETERMINES_RE.captures(context) {
                        from_context = Some(ChoiceTarget::Placeholder(
                            cap.get(1).map_or("", |m| m.as_str()).to_string(),
                        ));
                        break;
                    }
                    if let Some(cap) = FLAG_REF_RE.captures(context) {
                        let (normalized, _) = Self::normalize_flag_token(&cap[1]);
                        from_context = Some(ChoiceTarget::Flag(normalized));
                        break;
                    }
                }
                from_context
            } else {
                None
            };

            let Some(target) = target else {
                continue;
            };

            let mut choices: Vec<String> = Vec::new();
            let mut recognized_rows: Vec<usize> = Vec::new();
            let mut probe = idx + 1;
            let mut started_rows = false;
            while probe < lines.len() {
                let row = lines[probe].text.trim();
                if row.is_empty() {
                    if started_rows {
                        break;
                    }
                    probe += 1;
                    continue;
                }
                if Self::is_usage_line(row) || Self::is_section_header_line(row) {
                    break;
                }
                if row.starts_with('-') {
                    break;
                }
                let Some((left, _)) = Self::split_two_columns(row) else {
                    break;
                };
                let row_choices = Self::parse_choice_tokens(left);
                if row_choices.is_empty() {
                    break;
                }
                started_rows = true;
                for choice in row_choices {
                    if !choices.contains(&choice) {
                        choices.push(choice);
                    }
                }
                recognized_rows.push(lines[probe].index);
                probe += 1;
            }

            if choices.len() < 2 {
                continue;
            }

            let target_index = match target {
                ChoiceTarget::Flag(flag_name) => flags.iter().position(|flag| {
                    flag.short.as_deref() == Some(flag_name.as_str())
                        || flag.long.as_deref() == Some(flag_name.as_str())
                }),
                ChoiceTarget::Placeholder(placeholder) => {
                    self.resolve_flag_for_placeholder(
                        lines,
                        idx,
                        placeholder.as_str(),
                        flags,
                        &FLAG_REF_RE,
                    )
                }
            };

            let Some(flag_index) = target_index else {
                continue;
            };

            let flag = &mut flags[flag_index];
            flag.takes_value = true;
            match &mut flag.value_type {
                ValueType::Choice(existing) => {
                    for choice in choices {
                        if !existing.contains(&choice) {
                            existing.push(choice);
                        }
                    }
                }
                _ => {
                    flag.value_type = ValueType::Choice(choices);
                }
            }
            recognized.insert(lines[idx].index);
            recognized.extend(recognized_rows);
        }
    }

    fn resolve_flag_for_placeholder(
        &self,
        lines: &[IndexedLine],
        idx: usize,
        placeholder: &str,
        flags: &[FlagSchema],
        flag_ref_re: &Regex,
    ) -> Option<usize> {
        let placeholder_lower = placeholder.to_ascii_lowercase();
        let mut candidates = flags
            .iter()
            .enumerate()
            .filter(|(_, flag)| {
                flag.takes_value
                    && (flag
                        .long
                        .as_deref()
                        .is_some_and(|long| long.trim_start_matches("--").contains(&placeholder_lower))
                        || flag
                            .description
                            .as_deref()
                            .is_some_and(|desc| desc.to_ascii_lowercase().contains(&placeholder_lower)))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();
        if candidates.len() == 1 {
            return candidates.into_iter().next();
        }

        let context_start = idx.saturating_sub(3);
        let mut referenced = Vec::new();
        for line in &lines[context_start..=idx] {
            for cap in flag_ref_re.captures_iter(line.text.as_str()) {
                let (normalized, _) = Self::normalize_flag_token(&cap[1]);
                if let Some(found) = flags.iter().position(|flag| {
                    flag.short.as_deref() == Some(normalized.as_str())
                        || flag.long.as_deref() == Some(normalized.as_str())
                }) {
                    if !referenced.contains(&found) {
                        referenced.push(found);
                    }
                }
            }
        }
        if referenced.len() == 1 {
            return referenced.into_iter().next();
        }

        None
    }

    fn parse_choice_tokens(left_column: &str) -> Vec<String> {
        let mut choices = Vec::new();
        for raw in left_column.split(',') {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }
            if !token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
            {
                continue;
            }
            if token.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            if Self::looks_like_placeholder_subcommand_token(token) {
                continue;
            }
            if !choices.contains(&token.to_string()) {
                choices.push(token.to_string());
            }
        }
        choices
    }

    fn build_diagnostics(
        &self,
        lines: &[IndexedLine],
        recognized_indices: HashSet<usize>,
        format_scores: Vec<FormatScore>,
        parsers_used: Vec<String>,
        confidence: f64,
    ) -> ParseDiagnostics {
        let relevant_indices = lines
            .iter()
            .filter(|line| Self::is_relevant_line(line.text.trim()))
            .map(|line| line.index)
            .collect::<HashSet<_>>();

        let relevant_lines = relevant_indices.len();
        let recognized_lines = relevant_indices.intersection(&recognized_indices).count();

        let unresolved_lines = lines
            .iter()
            .filter(|line| {
                let trimmed = line.text.trim();
                Self::is_relevant_line(trimmed) && !recognized_indices.contains(&line.index)
            })
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();

        let mut parsers_used = parsers_used;
        if parsers_used.is_empty() {
            parsers_used.push("none".to_string());
        }
        if confidence >= 0.85 {
            parsers_used.push("confidence:auto-accept".to_string());
        } else if confidence >= 0.65 {
            parsers_used.push("confidence:draft".to_string());
        } else {
            parsers_used.push("confidence:reject".to_string());
        }

        ParseDiagnostics {
            format_scores,
            parsers_used,
            relevant_lines,
            recognized_lines,
            unresolved_lines,
        }
    }

    fn is_relevant_line(trimmed: &str) -> bool {
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.starts_with("---") {
            return false;
        }
        if trimmed.starts_with("-<") || trimmed.starts_with("--<") {
            return false;
        }
        if PATTERNS.line_of_dashes.is_match(trimmed) {
            return false;
        }
        if Self::looks_like_keybinding_row(trimmed) {
            return false;
        }
        if Self::is_usage_line(trimmed)
            || Self::is_section_header_line(trimmed)
            || Self::looks_like_flag_row_start(trimmed)
            || Self::looks_like_structured_two_column(trimmed)
            || Self::looks_like_comma_command_list(trimmed)
        {
            return true;
        }
        false
    }

    fn looks_like_flag_row_start(trimmed: &str) -> bool {
        let Some(rest) = trimmed.strip_prefix('-') else {
            return false;
        };
        if rest.is_empty() {
            return false;
        }

        // Long form: --flag
        if let Some(long) = rest.strip_prefix('-') {
            return long
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric());
        }

        let mut chars = rest.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if first.is_whitespace() {
            return false;
        }

        // "-20 ..." is often prose/ranges, not a flag row.
        if first.is_ascii_digit() && chars.next().is_some_and(|ch| ch.is_ascii_digit()) {
            return false;
        }

        true
    }

    fn is_compact_short_cluster(token: &str) -> bool {
        token.starts_with('-')
            && token.len() > 2
            && !token.starts_with("--")
            && token.chars().skip(1).all(|ch| ch.is_ascii_alphanumeric())
    }

    fn is_usage_line(trimmed: &str) -> bool {
        let lower = trimmed.to_ascii_lowercase();
        lower.starts_with("usage:") || lower.starts_with("or:")
    }

    fn is_section_header_line(trimmed: &str) -> bool {
        if PATTERNS.subcommands_section.is_match(trimmed)
            || PATTERNS.flags_section.is_match(trimmed)
            || PATTERNS.options_section.is_match(trimmed)
            || PATTERNS.arguments_section.is_match(trimmed)
        {
            return true;
        }

        let lower = trimmed.to_ascii_lowercase();
        trimmed.ends_with(':')
            && (lower.contains("command")
                || lower.contains("action")
                || lower.contains("option")
                || lower.contains("flag")
                || lower.contains("argument"))
    }

    fn looks_like_structured_two_column(trimmed: &str) -> bool {
        let Some((left, right)) = Self::split_two_columns(trimmed) else {
            return false;
        };

        // Grammar-like rows (e.g. "OBJECT := ...") are usage prose, not a
        // subcommand/option table.
        if right.contains(":=") {
            return false;
        }

        if left == "-" {
            return false;
        }

        if left.starts_with('-') {
            return Self::looks_like_flag_row_start(left);
        }

        let left_tokens = left
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        if left_tokens.is_empty() {
            return false;
        }
        if left_tokens
            .iter()
            .all(|token| Self::looks_like_non_command_value_token(token))
        {
            return false;
        }
        if right.trim_start().starts_with(':') {
            return false;
        }

        left_tokens
            .into_iter()
            .all(Self::looks_like_command_token)
    }

    fn looks_like_comma_command_list(trimmed: &str) -> bool {
        trimmed.contains(',')
            && trimmed
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .all(Self::looks_like_command_token)
    }

    fn looks_like_command_token(token: &str) -> bool {
        let token = token.trim();
        if token.is_empty() {
            return false;
        }
        if token.starts_with('-') {
            return false;
        }
        if token == "_" {
            return false;
        }
        if token.chars().all(|ch| ch == '.') {
            return true;
        }
        if token.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        if Self::looks_like_placeholder_subcommand_token(token)
            || Self::looks_like_non_command_value_token(token)
        {
            return false;
        }

        if token.chars().any(|ch| ch.is_whitespace()) {
            return false;
        }
        if token.chars().any(|ch| ch.is_ascii_uppercase()) {
            return false;
        }
        Self::is_valid_command_name(token)
    }

    fn looks_like_keybinding_row(trimmed: &str) -> bool {
        let Some((left, right)) = Self::split_two_columns(trimmed) else {
            return false;
        };
        let lower = left.to_ascii_lowercase();
        if lower.contains("esc-")
            || lower.contains("ctrl")
            || lower.contains("arrow")
            || left.contains('^')
        {
            return true;
        }

        let compact_key_tokens = left
            .split_whitespace()
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        let compact_keys = compact_key_tokens.len() >= 3
            && compact_key_tokens.iter().all(|token| {
                token.len() <= 3
                    && token
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '^' | '-' | ':'))
            });
        if compact_keys {
            return true;
        }

        let right_lower = right.to_ascii_lowercase();
        let keybinding_verb = [
            "display",
            "forward",
            "backward",
            "exit",
            "repaint",
            "repeat",
            "edit",
            "move cursor",
            "go to",
            "print version",
        ]
        .iter()
        .any(|needle| right_lower.contains(needle));
        let short_token_keys = left
            .split_whitespace()
            .filter(|token| !token.is_empty())
            .all(|token| token.len() <= 2 && token.chars().all(|ch| ch.is_ascii_alphanumeric()));

        keybinding_verb && short_token_keys
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

    /// Returns diagnostics for the most recent parse call.
    pub fn diagnostics(&self) -> &ParseDiagnostics {
        &self.diagnostics
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

    const CARGO_HELP: &str = r#"
Rust's package manager

Usage: cargo [+toolchain] [OPTIONS] [COMMAND]

Options:
  -V, --version                  Print version info and exit
      --list                     List installed commands
  -h, --help                     Print help

Commands:
    build, b    Compile the current package
    run, r      Run a binary or example of the local package
    test, t     Run the tests
    ...         See all commands with --list
"#;

    const NPM_HELP: &str = r#"
npm <command>

All commands:

    access, adduser, audit, cache, ci, config, install, run, test,
    uninstall, update, version, view, whoami
"#;

    const GNU_HELP: &str = r#"
Usage: cat [OPTION]... [FILE]...
Concatenate FILE(s) to standard output.

  -A, --show-all           equivalent to -vET
  -b, --number-nonblank    number nonempty output lines, overrides -n
      --help        display this help and exit
      --version     output version information and exit
"#;

    const TMUX_HELP: &str = r#"
usage: tmux [-2CDlNuVv] [-c shell-command] [-f file] [-L socket-name]
            [-S socket-path] [-T features] [command [flags]]
"#;

    const GENERIC_TWO_COLUMN_HELP: &str = r#"
Tool for service management

Actions
  start            Start the service
  stop             Stop the service
  restart          Restart the service

Misc text
"#;

    const GIT_STYLE_COMMANDS_HELP: &str = r#"
These are common Git commands used in various situations:
start a working area (see also: git help tutorial)
   clone     Clone a repository into a new directory
   init      Create an empty Git repository or reinitialize an existing one
work on the current change (see also: git help everyday)
   add       Add file contents to the index
"#;

    const SYSTEMCTL_STYLE_HELP: &str = r#"
systemctl [OPTIONS...] COMMAND ...

Unit Commands:
  start UNIT...      Start (activate) one or more units
  stop UNIT...       Stop (deactivate) one or more units
"#;

    const APT_STYLE_HELP_WITH_PROSE: &str = r#"
apt 2.8.3 (amd64)

Usage: apt [options] command

Commands:
  list      list packages based on package names
  search    search in package descriptions
  show      show package details

This APT has Super Cow Powers.
"#;

    const TERRAFORM_STYLE_HELP: &str = r#"
Usage: terraform [global options] <subcommand> [args]

Main commands:
  init          Prepare your working directory for other commands
  plan          Show changes required by the current configuration

Global options (use these before the subcommand, if any):
  -chdir=DIR    Switch to a different working directory before executing the given subcommand.
  -help         Show this help output or the help for a specified subcommand.
  -version      An alias for the "version" subcommand.
"#;

    const WRAPPED_TWO_COLUMN_HELP: &str = r#"
Unit Commands:
  list-paths [PATTERN...]             List path units currently in memory,
                                      ordered by path
"#;

    const MIXED_TWO_COLUMN_AND_FLAGS_HELP: &str = r#"
Usage: svcctl [OPTIONS] <command>

Common commands:
  start             Start the service
  stop              Stop the service

  -v, --verbose     Enable verbose output
"#;

    const VALUE_TABLE_HELP: &str = r#"
The version control method may be selected via the --backup option.
Here are the values:
  none, off       never make backups
  numbered, t     make numbered backups
  existing, nil   numbered if numbered backups exist, simple otherwise
  simple, never   always make simple backups
"#;

    const COLUMN_TABLE_HELP: &str = r#"
Available output columns:
  NAME        device name
  SIZE        device size
  MOUNTPOINT  where mounted
"#;

    const KEYBINDING_TABLE_HELP: &str = r#"
SUMMARY OF LESS COMMANDS
  h                 Display this help.
  q                 Exit.
  e                 Forward one line.
  ESC-SPACE         Forward one window, but don't stop at end-of-file.
  ctrl-LeftArrow    Move cursor left one word.
"#;

    const NODE_ENVIRONMENT_HEADER_HELP: &str = r#"
Environment variables:
  NODE_PATH      ':'-separated list of directories prefixed to the module search path
  NODE_OPTIONS   set CLI options for launched processes
"#;

    const LONG_SENTENCE_COMMAND_HEADER_HELP: &str = r#"
To remove a file whose name starts with '-', for example '-foo', use one of these commands:
  rm -- -foo
  rm ./-foo
"#;

    const DENSE_KEYBINDING_DOC_HELP: &str = r#"
SUMMARY OF LESS COMMANDS
  e  ^E  j  ^N  CR  *  Forward one line.
  y  ^Y  k  ^K  ^P  *  Backward one line.
  ESC-SPACE         *  Forward one window.
  ESC-(  LeftArrow  *  Left one half screen width.
  ESC-}  ^RightArrow   Right to last column displayed.
  p  %              *  Go to beginning of file.
  t                 *  Go to next tag.
  V                    Print version number of less.
"#;

    const NUMERIC_FLAGS_HELP: &str = r#"
Usage: sockctl [OPTIONS]

  -4, --ipv4          display only IP version 4 sockets
  -6, --ipv6          display only IP version 6 sockets
  -0, --packet        display PACKET sockets
"#;

    const OPTIONS_PLUS_SECTIONLESS_HELP: &str = r#"
Usage: datactl [OPTIONS]

Options:
  -v, --verbose       Enable verbose logging

Advanced:
  -0, --null          end each output line with NUL, not newline
"#;

    const EXAMPLE_INVOCATION_HELP: &str = r#"
Examples:
  tar -cf archive.tar foo bar  # Create archive.tar from files foo and bar.
"#;

    const LONG_ALIAS_FLAG_HELP: &str = r#"
Options:
  --old-archive, --portability same as --format=v7
"#;

    const USAGE_GRAMMAR_HELP: &str = r#"
where  OBJECT := { address | route } OPTIONS := { -4 | -6 | -0 | -j[son] }
"#;

    const DOTTED_LONG_FLAGS_HELP: &str = r#"
Options:
  --tls-min-v1.2  set default TLS minimum to TLSv1.2
  --tls-max-v1.3  set default TLS maximum to TLSv1.3
"#;

    const MULTI_CHAR_SHORT_ALIAS_HELP: &str = r#"
Options:
  -nH, --no-host-directories       don't create host directories
  -nv, --no-verbose                turn off verboseness
"#;

    const NAMED_SETTINGS_HELP: &str = r#"
Special settings:
   speed         print the terminal speed
   cbreak        same as -icanon
   sane          same as cread -ignbrk brkint -inlcr -igncr icrnl icanon
"#;

    const FLAG_CHOICE_HINT_HELP: &str = r#"
Options:
  -D debugopts

Valid arguments for -D:
exec, opt, rates, search, stat, time, tree, all, help
"#;

    const PACKED_MULTI_FLAG_ROW_HELP: &str = r#"
Usage: zip [options]

  -@   read names from stdin        -o   make zipfile as old as latest entry
  -?|-h list help
"#;

    const FLAG_DESCRIPTION_WRAP_AFTER_COLON_HELP: &str = r#"
Options:
  --quoting-style=WORD   use quoting style WORD for entry names:
                         literal, locale, shell, shell-always,
                         shell-escape, shell-escape-always, c, escape
"#;

    const DASH_BULLET_PROSE_HELP: &str = r#"
nice - run a program with modified scheduling priority

-20 (most favorable to the process) to 19 (least favorable to the process).
  -    the exit status of COMMAND otherwise
"#;

    const BASH_SHELL_OPTIONS_HELP: &str = r#"
Shell options:
  -ilrsD or -c command or -O shopt_option        (invocation only)
  -abefhkmnptuvxBCEHPT or -o option
"#;

    const ARGUMENTS_SECTION_HELP: &str = r#"
Usage: sample [OPTIONS] <SOURCE> <DEST>

Arguments:
  <SOURCE>    Source file path
  <DEST>      Destination file path
"#;

    const USAGE_POSITIONAL_HELP: &str = r#"
Usage: cargo [+toolchain] [OPTIONS] [COMMAND]
"#;

    const FLAG_RELATION_AND_MULTIPLE_HELP: &str = r#"
Options:
  --verbose...               Increase output verbosity (can be used multiple times)
  --locked                   Assert lockfile is unchanged (conflicts with --offline)
  --frozen                   Requires --locked and --offline
"#;

    const APT_VERSION_BANNER_HELP: &str = r#"
apt 2.8.3 (amd64)

Usage: apt [options] command
"#;

    const LESS_DOTTED_HELP_ROW: &str = r#"
Options:
  -a, --alpha        ........  Toggle alpha mode.
"#;

    const CONTEXTUAL_VALUES_TABLE_HELP: &str = r#"
Options:
  --backup[=CONTROL]       make a backup of destination file

The version control method may be selected via the --backup option or through
the VERSION_CONTROL environment variable.  Here are the values:
  none, off       never make backups
  numbered, t     make numbered backups
  existing, nil   numbered if backups exist, simple otherwise
"#;

    const PLACEHOLDER_VALUES_TABLE_HELP: &str = r#"
Options:
  --output-error[=MODE]   set behavior on write error. See MODE below

MODE determines behavior with write errors on the outputs:
  warn           diagnose errors writing to any output
  warn-nopipe    diagnose errors writing to output not a pipe
  exit           exit on error writing to any output
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

    #[test]
    fn test_parse_subcommand_aliases_from_cargo_help() {
        let mut parser = HelpParser::new("cargo", CARGO_HELP);
        let schema = parser.parse().unwrap();

        let build = schema.find_subcommand("build").expect("build must exist");
        assert!(build.aliases.contains(&"b".to_string()));

        let run = schema.find_subcommand("run").expect("run must exist");
        assert!(run.aliases.contains(&"r".to_string()));

        let test = schema.find_subcommand("test").expect("test must exist");
        assert!(test.aliases.contains(&"t".to_string()));
    }

    #[test]
    fn test_parse_npm_all_commands_comma_list() {
        let mut parser = HelpParser::new("npm", NPM_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("install").is_some());
        assert!(schema.find_subcommand("run").is_some());
        assert!(schema.find_subcommand("update").is_some());
    }

    #[test]
    fn test_parse_gnu_flags_without_explicit_sections() {
        let mut parser = HelpParser::new("cat", GNU_HELP);
        let schema = parser.parse().unwrap();

        let show_all = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--show-all"));
        assert!(show_all.is_some());

        let help = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--help"));
        assert!(help.is_some());
    }

    #[test]
    fn test_parse_exposes_diagnostics_with_coverage() {
        let mut parser = HelpParser::new("cat", GNU_HELP);
        let schema = parser.parse().unwrap();
        assert!(!schema.global_flags.is_empty());

        let diagnostics = parser.diagnostics();
        assert!(diagnostics.relevant_lines > 0);
        assert!(diagnostics.coverage() > 0.0);
        assert!(diagnostics.coverage() <= 1.0);
        assert!(!diagnostics.format_scores.is_empty());
    }

    #[test]
    fn test_parse_tmux_usage_compact_flags() {
        let mut parser = HelpParser::new("tmux", TMUX_HELP);
        let schema = parser.parse().unwrap();

        let has_short = |name: &str| {
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some(name))
        };
        let parsed_shorts = schema
            .global_flags
            .iter()
            .filter_map(|flag| flag.short.clone())
            .collect::<Vec<_>>();

        assert!(has_short("-2"), "{parsed_shorts:?}");
        assert!(has_short("-C"), "{parsed_shorts:?}");
        assert!(has_short("-v"), "{parsed_shorts:?}");
        assert!(has_short("-c"), "{parsed_shorts:?}");
        assert!(has_short("-f"), "{parsed_shorts:?}");

        let c_flag = schema
            .global_flags
            .iter()
            .find(|flag| flag.short.as_deref() == Some("-c"))
            .expect("-c flag should exist");
        assert!(c_flag.takes_value);
    }

    #[test]
    fn test_parse_generic_two_column_subcommands_without_section_header() {
        let mut parser = HelpParser::new("svcctl", GENERIC_TWO_COLUMN_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("start").is_some());
        assert!(schema.find_subcommand("stop").is_some());
        assert!(schema.find_subcommand("restart").is_some());
    }

    #[test]
    fn test_parse_git_style_commands_header_and_rows() {
        let mut parser = HelpParser::new("git", GIT_STYLE_COMMANDS_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("clone").is_some());
        assert!(schema.find_subcommand("init").is_some());
        assert!(schema.find_subcommand("add").is_some());
    }

    #[test]
    fn test_parse_subcommands_with_placeholder_tail() {
        let mut parser = HelpParser::new("systemctl", SYSTEMCTL_STYLE_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("start").is_some());
        assert!(schema.find_subcommand("stop").is_some());
    }

    #[test]
    fn test_does_not_parse_prose_line_as_subcommand() {
        let mut parser = HelpParser::new("apt", APT_STYLE_HELP_WITH_PROSE);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("list").is_some());
        assert!(schema.find_subcommand("search").is_some());
        assert!(schema.find_subcommand("show").is_some());
        assert!(schema.find_subcommand("This").is_none());
    }

    #[test]
    fn test_parse_section_header_with_global_options_and_single_dash_word_flags() {
        let mut parser = HelpParser::new("terraform", TERRAFORM_STYLE_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("init").is_some());
        assert!(schema.find_subcommand("plan").is_some());

        let chdir = schema
            .global_flags
            .iter()
            .find(|flag| flag.short.as_deref() == Some("-chdir"));
        assert!(chdir.is_some());
        assert!(chdir.expect("chdir flag should exist").takes_value);

        let help = schema
            .global_flags
            .iter()
            .find(|flag| flag.short.as_deref() == Some("-help"));
        assert!(help.is_some());
        assert!(!help.expect("help flag should exist").takes_value);
    }

    #[test]
    fn test_parse_wrapped_two_column_subcommand_description() {
        let mut parser = HelpParser::new("systemctl", WRAPPED_TWO_COLUMN_HELP);
        let schema = parser.parse().unwrap();

        let list_paths = schema
            .find_subcommand("list-paths")
            .expect("list-paths subcommand should exist");
        let description = list_paths.description.as_deref().unwrap_or_default();
        assert!(description.contains("List path units currently in memory"));
        assert!(description.contains("ordered by path"));
    }

    #[test]
    fn test_parse_two_column_subcommands_when_flags_exist() {
        let mut parser = HelpParser::new("svcctl", MIXED_TWO_COLUMN_AND_FLAGS_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("start").is_some());
        assert!(schema.find_subcommand("stop").is_some());
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.long.as_deref() == Some("--verbose"))
        );
    }

    #[test]
    fn test_generic_two_column_skips_choice_value_blocks() {
        let mut parser = HelpParser::new("cp", VALUE_TABLE_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.subcommands.is_empty());
    }

    #[test]
    fn test_generic_two_column_skips_column_header_tables() {
        let mut parser = HelpParser::new("lsblk", COLUMN_TABLE_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.subcommands.is_empty());
    }

    #[test]
    fn test_generic_two_column_skips_keybinding_tables() {
        let mut parser = HelpParser::new("less", KEYBINDING_TABLE_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.subcommands.is_empty());
    }

    #[test]
    fn test_environment_variable_header_is_not_subcommand_section() {
        let mut parser = HelpParser::new("node", NODE_ENVIRONMENT_HEADER_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.subcommands.is_empty());
    }

    #[test]
    fn test_long_sentence_with_commands_colon_is_not_subcommand_section() {
        let mut parser = HelpParser::new("rm", LONG_SENTENCE_COMMAND_HEADER_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.subcommands.is_empty());
    }

    #[test]
    fn test_skip_generic_subcommands_for_dense_keybinding_docs() {
        let mut parser = HelpParser::new("less", DENSE_KEYBINDING_DOC_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.subcommands.is_empty());
    }

    #[test]
    fn test_diagnostics_filters_prose_and_recognizes_headers() {
        let mut parser = HelpParser::new("terraform", TERRAFORM_STYLE_HELP);
        let _schema = parser.parse().unwrap();
        let diagnostics = parser.diagnostics();

        assert!(!diagnostics.unresolved_lines.iter().any(|line| {
            line.contains("available commands for execution")
                || line.contains("less common or more advanced commands")
        }));
        assert!(
            !diagnostics
                .unresolved_lines
                .iter()
                .any(|line| line.trim() == "Main commands:")
        );
    }

    #[test]
    fn test_parse_numeric_short_flags() {
        let mut parser = HelpParser::new("sockctl", NUMERIC_FLAGS_HELP);
        let schema = parser.parse().unwrap();

        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-4")
                    && flag.long.as_deref() == Some("--ipv4"))
        );
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-6")
                    && flag.long.as_deref() == Some("--ipv6"))
        );
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-0")
                    && flag.long.as_deref() == Some("--packet"))
        );
    }

    #[test]
    fn test_parse_sectionless_flags_as_top_up_when_options_exist() {
        let mut parser = HelpParser::new("datactl", OPTIONS_PLUS_SECTIONLESS_HELP);
        let schema = parser.parse().unwrap();

        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.long.as_deref() == Some("--verbose"))
        );
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-0")
                    && flag.long.as_deref() == Some("--null"))
        );
    }

    #[test]
    fn test_diagnostics_skips_example_invocation_lines() {
        let mut parser = HelpParser::new("tar", EXAMPLE_INVOCATION_HELP);
        let _schema = parser.parse().unwrap();
        let diagnostics = parser.diagnostics();

        assert!(diagnostics.unresolved_lines.is_empty());
    }

    #[test]
    fn test_parse_long_flag_with_comma_aliases() {
        let mut parser = HelpParser::new("tar", LONG_ALIAS_FLAG_HELP);
        let schema = parser.parse().unwrap();

        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.long.as_deref() == Some("--old-archive"))
        );
    }

    #[test]
    fn test_diagnostics_skips_usage_grammar_lines() {
        let mut parser = HelpParser::new("ip", USAGE_GRAMMAR_HELP);
        let _schema = parser.parse().unwrap();
        let diagnostics = parser.diagnostics();

        assert!(diagnostics.unresolved_lines.is_empty());
    }

    #[test]
    fn test_parse_dotted_long_flags() {
        let mut parser = HelpParser::new("node", DOTTED_LONG_FLAGS_HELP);
        let schema = parser.parse().unwrap();

        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.long.as_deref() == Some("--tls-min-v1.2"))
        );
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.long.as_deref() == Some("--tls-max-v1.3"))
        );
    }

    #[test]
    fn test_parse_multi_char_short_alias_flags() {
        let mut parser = HelpParser::new("wget", MULTI_CHAR_SHORT_ALIAS_HELP);
        let schema = parser.parse().unwrap();

        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-nH")
                    && flag.long.as_deref() == Some("--no-host-directories"))
        );
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-nv")
                    && flag.long.as_deref() == Some("--no-verbose"))
        );
    }

    #[test]
    fn test_parse_named_setting_rows_as_subcommands() {
        let mut parser = HelpParser::new("stty", NAMED_SETTINGS_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("speed").is_some());
        assert!(schema.find_subcommand("cbreak").is_some());
        assert!(schema.find_subcommand("sane").is_some());
    }

    #[test]
    fn test_named_setting_rows_skip_placeholder_style_tokens() {
        let help = r#"
Special settings:
   N             set the input and output speeds to N bauds
   csN           set character size to N bits
   speed         print the terminal speed
"#;
        let mut parser = HelpParser::new("stty", help);
        let schema = parser.parse().unwrap();

        assert!(schema.find_subcommand("N").is_none());
        assert!(schema.find_subcommand("csN").is_none());
        assert!(schema.find_subcommand("speed").is_some());
    }

    #[test]
    fn test_apply_flag_choice_hints_for_valid_arguments_block() {
        let mut parser = HelpParser::new("find", FLAG_CHOICE_HINT_HELP);
        let schema = parser.parse().unwrap();

        let debug = schema
            .global_flags
            .iter()
            .find(|flag| flag.short.as_deref() == Some("-D"))
            .expect("expected -D flag to be present");
        assert!(debug.takes_value);
        match &debug.value_type {
            ValueType::Choice(values) => {
                assert!(values.contains(&"exec".to_string()));
                assert!(values.contains(&"help".to_string()));
            }
            other => panic!("expected Choice value type, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_packed_multi_flag_rows_and_symbolic_short_flags() {
        let mut parser = HelpParser::new("zip", PACKED_MULTI_FLAG_ROW_HELP);
        let schema = parser.parse().unwrap();

        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-@"))
        );
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-o"))
        );
        assert!(
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some("-?"))
        );
    }

    #[test]
    fn test_merge_wrapped_flag_description_after_colon() {
        let mut parser = HelpParser::new("ls", FLAG_DESCRIPTION_WRAP_AFTER_COLON_HELP);
        let schema = parser.parse().unwrap();
        let diagnostics = parser.diagnostics();

        let quoting_style = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--quoting-style"))
            .expect("--quoting-style flag should exist");
        let description = quoting_style.description.as_deref().unwrap_or_default();
        assert!(
            description.contains("literal, locale"),
            "description was: {description}"
        );
        assert!(
            diagnostics.unresolved_lines.is_empty(),
            "unresolved lines: {:?}",
            diagnostics.unresolved_lines
        );
    }

    #[test]
    fn test_diagnostics_ignore_dash_bullet_prose_rows() {
        let mut parser = HelpParser::new("nice", DASH_BULLET_PROSE_HELP);
        let _schema = parser.parse().unwrap();
        let diagnostics = parser.diagnostics();

        assert!(
            diagnostics.unresolved_lines.is_empty(),
            "unresolved lines: {:?}",
            diagnostics.unresolved_lines
        );
    }

    #[test]
    fn test_parse_compact_short_clusters_into_individual_flags() {
        let mut parser = HelpParser::new("bash", BASH_SHELL_OPTIONS_HELP);
        let schema = parser.parse().unwrap();

        let has_short = |name: &str| {
            schema
                .global_flags
                .iter()
                .any(|flag| flag.short.as_deref() == Some(name))
        };

        assert!(has_short("-i"));
        assert!(has_short("-l"));
        assert!(has_short("-r"));
        assert!(has_short("-s"));
        assert!(has_short("-D"));
        assert!(has_short("-a"));
        assert!(has_short("-b"));
        assert!(has_short("-e"));
        assert!(has_short("-T"));

        assert!(!has_short("-ilrsD"));
        assert!(!has_short("-abefhkmnptuvxBCEHPT"));
    }

    #[test]
    fn test_parse_arguments_section_into_positionals() {
        let mut parser = HelpParser::new("sample", ARGUMENTS_SECTION_HELP);
        let schema = parser.parse().unwrap();

        let source = schema
            .positional
            .iter()
            .find(|arg| arg.name == "SOURCE")
            .expect("SOURCE positional should exist");
        assert!(source.required);
        assert_eq!(source.value_type, ValueType::File);

        let dest = schema
            .positional
            .iter()
            .find(|arg| arg.name == "DEST")
            .expect("DEST positional should exist");
        assert!(dest.required);
        assert_eq!(dest.value_type, ValueType::File);
    }

    #[test]
    fn test_parse_usage_positionals_when_argument_section_missing() {
        let mut parser = HelpParser::new("cargo", USAGE_POSITIONAL_HELP);
        let schema = parser.parse().unwrap();

        assert!(schema.positional.iter().any(|arg| arg.name == "toolchain"));
        assert!(
            schema
                .positional
                .iter()
                .all(|arg| !matches!(arg.name.as_str(), "COMMAND" | "command"))
        );
    }

    #[test]
    fn test_parse_flag_relations_and_multiple_from_description() {
        let mut parser = HelpParser::new("sample", FLAG_RELATION_AND_MULTIPLE_HELP);
        let schema = parser.parse().unwrap();

        let verbose = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--verbose"))
            .expect("--verbose should exist");
        assert!(verbose.multiple);

        let locked = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--locked"))
            .expect("--locked should exist");
        assert!(locked.conflicts_with.contains(&"--offline".to_string()));

        let frozen = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--frozen"))
            .expect("--frozen should exist");
        assert!(frozen.requires.contains(&"--locked".to_string()));
        assert!(frozen.requires.contains(&"--offline".to_string()));
    }

    #[test]
    fn test_extract_version_from_command_banner_without_word_version() {
        let mut parser = HelpParser::new("apt", APT_VERSION_BANNER_HELP);
        let schema = parser.parse().unwrap();
        assert_eq!(schema.version.as_deref(), Some("2.8.3"));
    }

    #[test]
    fn test_dotted_flag_descriptions_do_not_mark_multiple() {
        let mut parser = HelpParser::new("less", LESS_DOTTED_HELP_ROW);
        let schema = parser.parse().unwrap();
        let alpha = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--alpha"))
            .expect("--alpha flag should exist");
        assert!(!alpha.multiple);
    }

    #[test]
    fn test_parse_contextual_values_table_for_flag_choices() {
        let mut parser = HelpParser::new("cp", CONTEXTUAL_VALUES_TABLE_HELP);
        let schema = parser.parse().unwrap();
        let backup = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--backup"))
            .expect("--backup should exist");
        match &backup.value_type {
            ValueType::Choice(values) => {
                assert!(values.contains(&"none".to_string()));
                assert!(values.contains(&"off".to_string()));
                assert!(values.contains(&"numbered".to_string()));
                assert!(values.contains(&"nil".to_string()));
            }
            other => panic!("expected Choice for --backup, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_placeholder_values_table_for_flag_choices() {
        let mut parser = HelpParser::new("tee", PLACEHOLDER_VALUES_TABLE_HELP);
        let schema = parser.parse().unwrap();
        let output_error = schema
            .global_flags
            .iter()
            .find(|flag| flag.long.as_deref() == Some("--output-error"))
            .expect("--output-error should exist");
        match &output_error.value_type {
            ValueType::Choice(values) => {
                assert!(values.contains(&"warn".to_string()));
                assert!(values.contains(&"warn-nopipe".to_string()));
                assert!(values.contains(&"exit".to_string()));
            }
            other => panic!("expected Choice for --output-error, got {other:?}"),
        }
    }
}
