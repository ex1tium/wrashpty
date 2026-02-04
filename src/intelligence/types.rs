//! Core types for the Command Intelligence Engine.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::chrome::command_edit::TokenType;

// ============================================================================
// Suggestion Types
// ============================================================================

/// A ranked suggestion from any source.
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// The suggested text.
    pub text: String,

    /// Where this suggestion came from.
    pub source: SuggestionSource,

    /// Computed relevance score (0.0 - 1.0+).
    pub score: f64,

    /// Additional context for display.
    pub metadata: SuggestionMetadata,
}

impl Suggestion {
    /// Creates a new suggestion with the given text, source, and score.
    pub fn new(text: impl Into<String>, source: SuggestionSource, score: f64) -> Self {
        Self {
            text: text.into(),
            source,
            score,
            metadata: SuggestionMetadata::default(),
        }
    }

    /// Sets the metadata for this suggestion.
    pub fn with_metadata(mut self, metadata: SuggestionMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Source of a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    /// From learned command hierarchy (primary source).
    LearnedHierarchy,

    /// From learned token sequences.
    LearnedSequence,

    /// From learned pipe patterns.
    LearnedPipe,

    /// From learned flag values.
    LearnedFlagValue,

    /// From session transition patterns (command-to-command workflow).
    SessionTransition,

    /// From template completion (full command templates).
    Template,

    /// From FTS5 fuzzy search.
    FuzzySearch,

    /// From historical frequency.
    HistoricalFrequency,

    /// User-defined pattern.
    UserPattern,

    /// User-defined alias.
    UserAlias,
}

impl SuggestionSource {
    /// Returns the source bonus multiplier for scoring.
    pub fn bonus(&self) -> f64 {
        match self {
            Self::UserPattern | Self::UserAlias => 2.0,
            Self::SessionTransition => 1.5,
            Self::LearnedHierarchy | Self::LearnedSequence | Self::LearnedPipe | Self::LearnedFlagValue => 1.2,
            Self::Template | Self::FuzzySearch | Self::HistoricalFrequency => 1.0,
        }
    }

    /// Returns a human-readable label for display.
    pub fn label(&self) -> &'static str {
        match self {
            Self::LearnedHierarchy => "learned",
            Self::LearnedSequence => "seq",
            Self::LearnedPipe => "pipe",
            Self::LearnedFlagValue => "flag",
            Self::SessionTransition => "session",
            Self::Template => "template",
            Self::FuzzySearch => "fuzzy",
            Self::HistoricalFrequency => "freq",
            Self::UserPattern => "pattern",
            Self::UserAlias => "alias",
        }
    }
}

/// Additional metadata for a suggestion.
#[derive(Debug, Clone, Default)]
pub struct SuggestionMetadata {
    /// How many times this pattern was seen.
    pub frequency: u32,

    /// Success rate (0.0 - 1.0) if known.
    pub success_rate: Option<f64>,

    /// When last used (unix timestamp).
    pub last_seen: Option<i64>,

    /// For templates: the filled preview.
    pub template_preview: Option<String>,

    /// For fuzzy: the match quality.
    pub fuzzy_score: Option<f64>,

    /// User-provided description.
    pub description: Option<String>,

    /// Token role from hierarchy (e.g., "subcommand", "flag", "argument").
    pub role: Option<String>,
}

// ============================================================================
// Context Types
// ============================================================================

/// Context for generating suggestions.
#[derive(Debug, Clone, Default)]
pub struct SuggestionContext {
    /// Tokens before the current edit position.
    pub preceding_tokens: Vec<AnalyzedToken>,

    /// The partial text being typed.
    pub partial: String,

    /// Current working directory.
    pub cwd: Option<PathBuf>,

    /// Position type for specialized suggestions.
    pub position: PositionType,

    /// File context if in file browser.
    pub file_context: Option<FileContext>,

    /// Current session for transition suggestions.
    pub session: Option<SessionContext>,

    /// Last executed command (for "next" suggestions).
    pub last_command: Option<String>,
}

/// Position type in a command for specialized suggestions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PositionType {
    /// First token position (command).
    #[default]
    Command,

    /// After a known command (subcommand).
    Subcommand,

    /// After a flag that expects a value.
    FlagValue {
        /// The flag that precedes this value.
        flag: String,
    },

    /// After a pipe operator.
    AfterPipe,

    /// Generic argument position.
    Argument,

    /// After redirect (>, >>).
    AfterRedirect,
}

/// File context for file browser suggestions.
#[derive(Debug, Clone)]
pub struct FileContext {
    /// The filename (without path).
    pub filename: String,

    /// File extension if any.
    pub extension: Option<String>,

    /// Whether this is a directory.
    pub is_directory: bool,
}

impl FileContext {
    /// Creates a FileContext from a filename and directory flag.
    pub fn new(filename: impl Into<String>, is_directory: bool) -> Self {
        let filename = filename.into();
        let extension = std::path::Path::new(&filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string());
        Self {
            filename,
            extension,
            is_directory,
        }
    }

    /// Creates a FileContext from a path.
    pub fn from_path(path: &std::path::Path) -> Self {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string());
        let is_directory = path.is_dir();
        Self {
            filename,
            extension,
            is_directory,
        }
    }
}

/// Session context for transition suggestions.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// Unique session identifier.
    pub session_id: String,

    /// Recent commands in this session (last N).
    pub recent_commands: Vec<String>,

    /// Total command count in session.
    pub command_count: u32,
}

impl SessionContext {
    /// Creates a new session context with the given ID.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            recent_commands: Vec::new(),
            command_count: 0,
        }
    }

    /// Adds a command to the session history.
    pub fn add_command(&mut self, command: impl Into<String>) {
        let cmd = command.into();
        self.recent_commands.push(cmd);
        self.command_count += 1;

        // Keep only last 10 commands for context
        if self.recent_commands.len() > 10 {
            self.recent_commands.remove(0);
        }
    }

    /// Returns the last executed command in this session.
    pub fn last_command(&self) -> Option<&str> {
        self.recent_commands.last().map(|s| s.as_str())
    }
}

/// An analyzed token with classification.
#[derive(Debug, Clone)]
pub struct AnalyzedToken {
    /// The token text.
    pub text: String,

    /// Semantic type of this token.
    pub token_type: TokenType,

    /// Position in the command (0-indexed).
    pub position: usize,
}

impl AnalyzedToken {
    /// Creates a new analyzed token.
    pub fn new(text: impl Into<String>, token_type: TokenType, position: usize) -> Self {
        Self {
            text: text.into(),
            token_type,
            position,
        }
    }
}

// ============================================================================
// Template Types
// ============================================================================

/// A recognized command template.
#[derive(Debug, Clone)]
pub struct Template {
    /// Database ID.
    pub id: i64,

    /// Template pattern with placeholders (e.g., "docker run -p <PORT>:<PORT> <IMAGE>").
    pub pattern: String,

    /// Base command name.
    pub base_command: String,

    /// Placeholders in this template.
    pub placeholders: Vec<Placeholder>,

    /// How many times this template was seen.
    pub frequency: u32,
}

/// A placeholder in a template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Placeholder {
    /// Placeholder name (e.g., "PORT", "IMAGE").
    pub name: String,

    /// Type of value expected.
    pub placeholder_type: PlaceholderType,

    /// Token position in template.
    pub position: usize,
}

/// Type of a template placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaceholderType {
    /// Port number (e.g., 8080, 8080:8080).
    Port,

    /// File or directory path.
    Path,

    /// Docker image name.
    Image,

    /// Git branch name.
    Branch,

    /// URL or git remote.
    Url,

    /// Numeric value.
    Number,

    /// Quoted string.
    Quoted,

    /// Any value (generic).
    Generic,
}

impl PlaceholderType {
    /// Returns the placeholder marker string.
    pub fn marker(&self) -> &'static str {
        match self {
            Self::Port => "<PORT>",
            Self::Path => "<PATH>",
            Self::Image => "<IMAGE>",
            Self::Branch => "<BRANCH>",
            Self::Url => "<URL>",
            Self::Number => "<NUMBER>",
            Self::Quoted => "<QUOTED>",
            Self::Generic => "<VALUE>",
        }
    }
}

/// A filled template ready for insertion.
#[derive(Debug, Clone)]
pub struct TemplateCompletion {
    /// The template being completed.
    pub template: Template,

    /// Values filled for each placeholder.
    pub filled_values: HashMap<String, String>,

    /// Complete command preview.
    pub preview: String,

    /// Confidence score for this completion.
    pub confidence: f64,
}

// ============================================================================
// User Pattern Types
// ============================================================================

/// A user-defined suggestion pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPattern {
    /// Database ID.
    #[serde(default)]
    pub id: i64,

    /// Type of pattern.
    pub pattern_type: UserPatternType,

    /// What triggers this pattern.
    pub trigger: String,

    /// What to suggest.
    pub suggestion: String,

    /// User-provided description.
    pub description: Option<String>,

    /// Priority (higher = shown first).
    #[serde(default)]
    pub priority: i32,

    /// Whether this pattern is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Number of times this pattern was used.
    #[serde(default)]
    pub use_count: u32,
}

fn default_true() -> bool {
    true
}

/// Type of user-defined pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserPatternType {
    /// Simple alias expansion.
    Alias,

    /// After command X, suggest Y.
    Sequence,

    /// For files matching pattern, suggest command.
    FileType,

    /// Custom trigger condition.
    Trigger,
}

/// A user-defined alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAlias {
    /// Database ID.
    #[serde(default)]
    pub id: i64,

    /// Short alias name.
    pub alias: String,

    /// Full command expansion.
    pub expansion: String,

    /// User-provided description.
    pub description: Option<String>,

    /// Whether this alias is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Number of times this alias was used.
    #[serde(default)]
    pub use_count: u32,
}

// ============================================================================
// Export/Import Types
// ============================================================================

/// Export format for pattern sharing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternExport {
    /// Schema version.
    pub version: String,

    /// Export timestamp.
    pub exported_at: i64,

    /// Optional machine identifier.
    pub machine_id: Option<String>,

    /// Exported patterns.
    pub patterns: ExportedPatterns,
}

/// Container for all exported patterns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportedPatterns {
    /// Exported sequences.
    pub sequences: Vec<ExportedSequence>,

    /// Exported pipe chains.
    pub pipe_chains: Vec<ExportedPipeChain>,

    /// Exported flag values.
    pub flag_values: Vec<ExportedFlagValue>,

    /// Exported templates.
    pub templates: Vec<ExportedTemplate>,

    /// User-defined patterns.
    pub user_patterns: Vec<UserPattern>,

    /// User-defined aliases.
    pub user_aliases: Vec<UserAlias>,
}

/// An exported sequence pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedSequence {
    /// Context token text.
    pub context_token: String,

    /// Position in command.
    pub context_position: usize,

    /// Base command (if any).
    pub base_command: Option<String>,

    /// Next token text.
    pub next_token: String,

    /// Frequency count.
    pub frequency: u32,

    /// Success count.
    pub success_count: u32,
}

/// An exported pipe chain pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedPipeChain {
    /// Base command before pipe.
    pub pre_pipe_base_cmd: Option<String>,

    /// Command after pipe.
    pub pipe_command: String,

    /// Full chain (if available).
    pub full_chain: Option<String>,

    /// Frequency count.
    pub frequency: u32,
}

/// An exported flag value pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedFlagValue {
    /// Base command.
    pub base_command: String,

    /// Subcommand (if any).
    pub subcommand: Option<String>,

    /// Flag text.
    pub flag: String,

    /// Value text.
    pub value: String,

    /// Value type.
    pub value_type: Option<String>,

    /// Frequency count.
    pub frequency: u32,
}

/// An exported template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedTemplate {
    /// Template pattern.
    pub template: String,

    /// Base command.
    pub base_command: Option<String>,

    /// Placeholders.
    pub placeholders: Vec<Placeholder>,

    /// Frequency count.
    pub frequency: u32,

    /// Example command.
    pub example: Option<String>,
}

/// Options for exporting patterns.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Include user-defined patterns.
    pub include_user_patterns: bool,

    /// Include learned patterns.
    pub include_learned_patterns: bool,

    /// Minimum frequency threshold.
    pub min_frequency: u32,

    /// Anonymize paths (replace home dir with ~).
    pub anonymize_paths: bool,
}

/// Options for importing patterns.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Import mode.
    pub mode: ImportMode,

    /// How to resolve conflicts.
    pub conflict_resolution: ConflictResolution,
}

/// Import mode for patterns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImportMode {
    /// Merge with existing patterns.
    #[default]
    Merge,

    /// Replace all existing patterns.
    Replace,

    /// Append without merging.
    Append,
}

/// How to resolve conflicts during import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConflictResolution {
    /// Keep existing patterns.
    #[default]
    KeepExisting,

    /// Use imported patterns.
    UseImported,

    /// Merge frequency counts.
    MergeFrequency,
}

/// Statistics from an import operation.
#[derive(Debug, Clone, Default)]
pub struct ImportStats {
    /// Number of sequences imported.
    pub sequences_imported: usize,

    /// Number of pipe chains imported.
    pub pipe_chains_imported: usize,

    /// Number of templates imported.
    pub templates_imported: usize,

    /// Number of user patterns imported.
    pub user_patterns_imported: usize,

    /// Number of conflicts resolved.
    pub conflicts_resolved: usize,

    /// Number of items skipped.
    pub skipped: usize,
}

// ============================================================================
// Sync Types
// ============================================================================

/// Statistics from a sync operation.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    /// Number of commands processed.
    pub commands_processed: usize,

    /// Number of tokens extracted.
    pub tokens_extracted: usize,

    /// Number of sequences learned.
    pub sequences_learned: usize,

    /// Number of pipe chains learned.
    pub pipe_chains_learned: usize,

    /// Number of flag values learned.
    pub flag_values_learned: usize,

    /// Number of entries skipped due to errors.
    pub entries_skipped: usize,

    /// Duration of sync in milliseconds.
    pub duration_ms: u64,
}

// ============================================================================
// Fuzzy Search Types
// ============================================================================

/// A fuzzy search match result.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    /// Matched command.
    pub command: String,

    /// BM25 relevance score.
    pub bm25_score: f64,

    /// Terms that matched.
    pub matched_terms: Vec<String>,
}

// ============================================================================
// Context Building
// ============================================================================

use crate::chrome::command_edit::CommandToken;

/// Builds a SuggestionContext from command tokens and context.
///
/// This is the main integration point between the command editor and
/// the intelligence system.
///
/// # Arguments
///
/// * `tokens` - The tokens preceding the current edit position
/// * `partial` - The partial text being typed
/// * `cwd` - Current working directory
/// * `file_context` - File context if in file browser
/// * `session` - Current session context
/// * `last_command` - Last executed command for transition suggestions
pub fn build_context(
    tokens: &[CommandToken],
    partial: &str,
    cwd: Option<PathBuf>,
    file_context: Option<FileContext>,
    session: Option<SessionContext>,
    last_command: Option<String>,
) -> SuggestionContext {
    // Convert CommandTokens to AnalyzedTokens
    let preceding_tokens: Vec<AnalyzedToken> = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| AnalyzedToken {
            text: t.text.clone(),
            token_type: t.token_type,
            position: i,
        })
        .collect();

    // Determine position type
    let position = determine_position_type(tokens, partial);

    SuggestionContext {
        preceding_tokens,
        partial: partial.to_string(),
        cwd,
        position,
        file_context,
        session,
        last_command,
    }
}

/// Determines the position type based on preceding tokens.
fn determine_position_type(tokens: &[CommandToken], partial: &str) -> PositionType {
    if tokens.is_empty() {
        return PositionType::Command;
    }

    let last_token = &tokens[tokens.len() - 1];

    // After pipe
    if last_token.text == "|" || last_token.text.ends_with('|') {
        return PositionType::AfterPipe;
    }

    // After redirect
    if last_token.text == ">" || last_token.text == ">>" || last_token.text == "<" {
        return PositionType::AfterRedirect;
    }

    // After flag (potential flag value)
    if last_token.token_type == TokenType::Flag {
        // Check if this flag typically takes a value
        if flag_expects_value(&last_token.text) && partial.is_empty() {
            return PositionType::FlagValue {
                flag: last_token.text.clone(),
            };
        }
    }

    // First position after command (subcommand)
    if tokens.len() == 1 {
        let cmd = &tokens[0].text;
        if is_compound_command(cmd) {
            return PositionType::Subcommand;
        }
    }

    PositionType::Argument
}

/// Returns true if the command has subcommands.
fn is_compound_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "git" | "docker" | "kubectl" | "cargo" | "npm" | "yarn"
        | "systemctl" | "journalctl" | "apt" | "brew" | "pacman"
        | "podman" | "dnf" | "yum" | "pip" | "pipx"
    )
}

/// Returns true if a flag typically expects a value.
fn flag_expects_value(flag: &str) -> bool {
    // Flags that commonly take values
    let value_flags = [
        "-m", "--message",
        "-f", "--file",
        "-o", "--output",
        "-i", "--input",
        "-c", "--config",
        "-d", "--directory",
        "-p", "--port",
        "-u", "--user",
        "-n", "--name",
        "-t", "--tag",
        "--format",
        "--filter",
        "--branch",
        "--remote",
    ];

    value_flags.contains(&flag)
}
