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
    CommandSchema, FlagSchema, HelpFormat, SchemaSource, SubcommandSchema, ValueType,
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
    subcommands: Vec<SectionEntry>,
    flags: Vec<SectionEntry>,
    options: Vec<SectionEntry>,
}

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

        // Parse sections
        let sections = self.identify_sections(&indexed_lines);
        let mut recognized_indices: HashSet<usize> = HashSet::new();
        let mut parsers_used: Vec<String> = Vec::new();

        // Extract subcommands
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

        // npm-style command lists (All commands:)
        if schema.subcommands.is_empty() {
            let (npm_subcommands, npm_recognized) = self.parse_npm_style_commands(&indexed_lines);
            if !npm_subcommands.is_empty() {
                recognized_indices.extend(npm_recognized);
                parsers_used.push("npm-command-list".to_string());
                schema.subcommands = npm_subcommands;
            }
        }

        // GNU and many custom CLIs list flags without explicit "Flags/Options" sections.
        if schema.global_flags.is_empty() {
            let (fallback_flags, fallback_recognized) =
                self.parse_sectionless_flags(&indexed_lines);
            if !fallback_flags.is_empty() {
                recognized_indices.extend(fallback_recognized);
                parsers_used.push("gnu-sectionless-flags".to_string());
                schema.global_flags.extend(fallback_flags);
            }
        }

        schema.global_flags = Self::dedupe_flags(schema.global_flags);
        schema.subcommands = Self::dedupe_subcommands(schema.subcommands);

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
                Some(SectionKind::Arguments) | None => {}
            }
        }

        buckets
    }

    fn detect_section_header(&self, trimmed: &str) -> Option<SectionKind> {
        if PATTERNS.subcommands_section.is_match(trimmed) {
            return Some(SectionKind::Subcommands);
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
                    if self.is_valid_command_name(name) && seen_names.insert(name.to_string()) {
                        subcommands.push(SubcommandSchema::new(name));
                    }
                }
                continue;
            }

            // Split on multiple spaces or dash separator
            let (name_part, description) = if let Some((head, desc)) = trimmed.split_once(" - ") {
                (head.trim(), Some(desc.trim()))
            } else if let Some((head, desc)) = trimmed.split_once("  ") {
                (head.trim(), Some(desc.trim()))
            } else {
                (trimmed, None)
            };

            // Support alias forms such as "build, b".
            let mut names: Vec<&str> = name_part
                .split(',')
                .map(str::trim)
                .filter(|name| self.is_valid_command_name(name))
                .collect();

            if names.is_empty() {
                // Fallback: some tools use single-space separation.
                if self.is_valid_command_name(name_part) {
                    names.push(name_part);
                } else {
                    continue;
                }
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

    /// Parses flag/option lines.
    fn parse_flags(&self, lines: &[&str]) -> Vec<FlagSchema> {
        lines
            .iter()
            .filter_map(|line| self.parse_flag_line(line))
            .collect()
    }

    /// Parses a single flag line.
    fn parse_flag_line(&self, line: &str) -> Option<FlagSchema> {
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

    fn normalize_help_output(raw: &str) -> String {
        static ANSI_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap());

        let stripped = ANSI_RE.replace_all(raw, "");
        let replaced = stripped.replace("\r\n", "\n").replace('\r', "\n");

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
                && !trimmed_start.starts_with('-')
                && !trimmed_start.ends_with(':')
                && normalized.last().is_some_and(|prev| {
                    let prev_trimmed = prev.trim();
                    let prev_is_flag = prev_trimmed.starts_with('-');
                    prev_is_flag
                        && !prev_trimmed.is_empty()
                        && !prev_trimmed.ends_with(':')
                        && !Self::looks_like_subcommand_entry(trimmed_start)
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
                if self.is_valid_command_name(name) && seen.insert(name.to_string()) {
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
            if !trimmed.starts_with('-') {
                continue;
            }
            if let Some(flag) = self.parse_flag_line(trimmed) {
                flags.push(flag);
                recognized.insert(line.index);
            }
        }

        (flags, recognized)
    }

    fn looks_like_command_list_line(&self, line: &str) -> bool {
        line.contains(',')
            && line
                .split(',')
                .filter(|part| !part.trim().is_empty())
                .all(|part| {
                    let token = part.trim();
                    self.is_valid_command_name(token)
                })
    }

    fn is_valid_command_name(&self, value: &str) -> bool {
        !value.is_empty()
            && value.len() < 50
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    fn dedupe_flags(flags: Vec<FlagSchema>) -> Vec<FlagSchema> {
        let mut seen = HashSet::new();
        let mut deduped = Vec::new();

        for flag in flags {
            let key = (
                flag.short.clone().unwrap_or_default(),
                flag.long.clone().unwrap_or_default(),
            );
            if seen.insert(key) {
                deduped.push(flag);
            }
        }

        deduped
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

    fn build_diagnostics(
        &self,
        lines: &[IndexedLine],
        recognized_indices: HashSet<usize>,
        format_scores: Vec<FormatScore>,
        parsers_used: Vec<String>,
        confidence: f64,
    ) -> ParseDiagnostics {
        let relevant_lines = lines
            .iter()
            .filter(|line| Self::is_relevant_line(line.text.trim()))
            .count();

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
            recognized_lines: recognized_indices.len(),
            unresolved_lines,
        }
    }

    fn is_relevant_line(trimmed: &str) -> bool {
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.eq_ignore_ascii_case("usage:")
            || trimmed.eq_ignore_ascii_case("options:")
            || trimmed.eq_ignore_ascii_case("flags:")
            || trimmed.eq_ignore_ascii_case("commands:")
            || trimmed.eq_ignore_ascii_case("all commands:")
        {
            return false;
        }
        true
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
        assert!(!diagnostics.format_scores.is_empty());
    }
}
