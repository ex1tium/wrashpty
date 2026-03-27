//! Unified command editing state for browser panels.
//!
//! Provides a configurable token-based command editor with support for:
//! - Locked (non-editable) tokens (e.g., filename in file browser)
//! - Dangerous command detection and confirmation
//! - Quote style cycling
//! - Undo/redo stack
//! - Pluggable suggestion providers
//! - Intelligent suggestions from learned patterns

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Constraint, Layout, Rect};
use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::{Line, Span};
use ratatui_core::widgets::Widget;
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use super::theme::Theme;
use crate::history_store::HistoryStore;
use crate::intelligence::FileContext;

// ============================================================================
// Token Types and Classification
// ============================================================================

/// Token type for semantic classification and styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    /// First token - the command (ls, git, etc.)
    Command,
    /// Second token for compound commands (checkout, push)
    Subcommand,
    /// Starts with - or --
    Flag,
    /// Contains / or starts with . or ~
    Path,
    /// Contains :// or looks like git@...
    Url,
    /// Generic argument
    Argument,
    /// Non-editable token (e.g., filename in file browser)
    Locked,
    /// Pipe operator: |
    Pipe,
    /// Redirect operator: >, >>, <, 2>, 2>>, &>, 2>&1
    Redirect,
    /// Shell operator: &&, ||, ;, &
    Operator,
    /// Heredoc opening marker: <<EOF, <<'EOF', <<-EOF (editable, paired)
    HeredocMarker,
    /// Heredoc body content between marker and closing delimiter (locked)
    HeredocBody,
    /// Heredoc closing delimiter line, e.g. EOF (editable, paired with marker)
    HeredocDelimiter,
}

/// Quote style for tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteStyle {
    None,
    Single, // 'text'
    Double, // "text"
}

/// A token from a shell command.
#[derive(Debug, Clone)]
pub struct CommandToken {
    /// The text content of this token.
    pub text: String,
    /// Semantic type of this token.
    pub token_type: TokenType,
    /// Whether this token can be edited.
    pub locked: bool,
    /// Index of the paired token (for HeredocMarker ↔ HeredocDelimiter links).
    pub pair_index: Option<usize>,
}

impl CommandToken {
    /// Creates a new editable token.
    pub fn new(text: impl Into<String>, token_type: TokenType) -> Self {
        Self {
            text: text.into(),
            token_type,
            locked: false,
            pair_index: None,
        }
    }

    /// Creates a new locked (non-editable) token.
    pub fn locked(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            token_type: TokenType::Locked,
            locked: true,
            pair_index: None,
        }
    }
}

/// Classifies a token based on its content, position, and preceding token context.
///
/// Uses `prev_type` (when available) for context-aware classification:
/// after a pipe or operator the next word is a new command, etc.
pub fn classify_token(
    text: &str,
    position: usize,
    prev_token: Option<&str>,
    prev_type: Option<TokenType>,
) -> TokenType {
    // First token is always the command
    if position == 0 {
        return TokenType::Command;
    }

    // After a pipe or operator, the next word starts a new command
    if matches!(prev_type, Some(TokenType::Pipe | TokenType::Operator)) {
        return TokenType::Command;
    }

    if text.starts_with('-') {
        return TokenType::Flag;
    }
    if text.contains("://") || text.starts_with("git@") {
        return TokenType::Url;
    }
    if text.contains('/') || text.starts_with('.') || text.starts_with('~') {
        return TokenType::Path;
    }

    // Check for subcommand: token right after a Command that is a compound command
    if matches!(prev_type, Some(TokenType::Command)) {
        if let Some(cmd) = prev_token {
            if crate::intelligence::tokenizer::is_compound_command(cmd) {
                return TokenType::Subcommand;
            }
        }
    }

    TokenType::Argument
}

/// Extracts the bare delimiter from a heredoc marker token text.
///
/// Given `<<EOF`, `<<'EOF'`, `<<-"EOF"`, etc., returns the bare delimiter word.
/// Returns `None` if the marker text is malformed.
pub fn extract_heredoc_delimiter(marker_text: &str) -> Option<String> {
    let rest = marker_text.strip_prefix("<<")?;
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    let rest = rest.trim();

    if rest.is_empty() {
        return None;
    }

    // Check for quoted delimiter
    let first = rest.as_bytes()[0];
    if first == b'\'' || first == b'"' {
        let quote = first as char;
        let inner = rest
            .strip_prefix(quote)?
            .strip_suffix(quote)
            .unwrap_or(rest.strip_prefix(quote)?);
        if inner.is_empty() {
            return None;
        }
        return Some(inner.to_string());
    }

    Some(rest.to_string())
}

/// Reconstructs a heredoc marker with a new delimiter, preserving prefix and quote style.
///
/// Given old marker `<<'EOF'` and new delimiter `END`, returns `<<'END'`.
pub fn rebuild_heredoc_marker(old_marker: &str, new_delim: &str) -> String {
    let rest = old_marker.strip_prefix("<<").unwrap_or(old_marker);
    let (dash, rest) = if rest.starts_with('-') {
        ("-", &rest[1..])
    } else {
        ("", rest)
    };
    let rest = rest.trim();
    let first = rest.as_bytes().first().copied().unwrap_or(0);
    if first == b'\'' || first == b'"' {
        let q = first as char;
        format!("<<{dash}{q}{new_delim}{q}")
    } else {
        format!("<<{dash}{new_delim}")
    }
}

/// Returns the style for a token based on its type.
pub fn token_type_style(token_type: TokenType, theme: &Theme) -> Style {
    match token_type {
        TokenType::Command => Style::default()
            .fg(theme.semantic_success)
            .add_modifier(Modifier::BOLD),
        TokenType::Subcommand => Style::default().fg(theme.header_fg),
        TokenType::Flag => Style::default().fg(theme.text_highlight),
        TokenType::Path => Style::default().fg(theme.semantic_info),
        TokenType::Url => Style::default().fg(theme.git_fg),
        TokenType::Argument => Style::default().fg(theme.text_primary),
        TokenType::Locked => Style::default().fg(theme.text_highlight),
        TokenType::Pipe | TokenType::Operator => Style::default()
            .fg(theme.text_highlight)
            .add_modifier(Modifier::BOLD),
        TokenType::Redirect => Style::default().fg(theme.text_highlight),
        TokenType::HeredocMarker => Style::default()
            .fg(theme.semantic_info)
            .add_modifier(Modifier::ITALIC),
        TokenType::HeredocDelimiter => Style::default().fg(theme.semantic_info),
        TokenType::HeredocBody => Style::default().fg(theme.text_secondary),
    }
}

/// Returns a superscript representation of any positive number.
/// Converts each digit to its Unicode superscript equivalent.
pub fn superscript_number(n: usize) -> String {
    const SUPERSCRIPT_DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

    if n == 0 {
        return "⁰".to_string();
    }

    n.to_string()
        .chars()
        .map(|c| {
            c.to_digit(10)
                .map(|d| SUPERSCRIPT_DIGITS[d as usize])
                .unwrap_or(c)
        })
        .collect()
}

// ============================================================================
// Quote Handling
// ============================================================================

/// Splits a token into its inner text and detected quote style.
pub fn split_quotes(text: &str) -> (String, QuoteStyle) {
    if text.len() >= 2 {
        if text.starts_with('\'') && text.ends_with('\'') {
            return (text[1..text.len() - 1].to_string(), QuoteStyle::Single);
        }
        if text.starts_with('"') && text.ends_with('"') {
            return (text[1..text.len() - 1].to_string(), QuoteStyle::Double);
        }
    }
    (text.to_string(), QuoteStyle::None)
}

/// Applies a quote style to text.
pub fn apply_quotes(text: &str, style: QuoteStyle) -> String {
    match style {
        QuoteStyle::None => text.to_string(),
        QuoteStyle::Single => format!("'{}'", text),
        QuoteStyle::Double => format!("\"{}\"", text),
    }
}

/// POSIX-quotes a string if it contains shell metacharacters or spaces.
/// Returns the string unchanged if no quoting is needed.
pub fn quote_for_shell(text: &str) -> String {
    if text.is_empty() {
        return "''".to_string();
    }

    // Check if quoting is needed
    let needs_quoting = text.chars().any(|c| {
        matches!(
            c,
            ' ' | '\t'
                | '\n'
                | '"'
                | '\''
                | '\\'
                | '$'
                | '`'
                | '!'
                | '*'
                | '?'
                | '['
                | ']'
                | '('
                | ')'
                | '{'
                | '}'
                | '<'
                | '>'
                | '|'
                | '&'
                | ';'
                | '#'
                | '~'
        )
    });

    if !needs_quoting {
        return text.to_string();
    }

    // Use single quotes, escaping any embedded single quotes
    // In POSIX shell: 'foo'\''bar' produces foo'bar
    if text.contains('\'') {
        let escaped = text.replace('\'', "'\\''");
        format!("'{}'", escaped)
    } else {
        format!("'{}'", text)
    }
}

// ============================================================================
// Dangerous Command Detection
// ============================================================================

/// Result of dangerous command check.
#[derive(Debug, Clone)]
pub struct DangerWarning {
    /// Human-readable description of why this command is dangerous.
    pub message: &'static str,
}

/// Checks if a command is potentially dangerous.
pub fn check_dangerous_command(command: &str) -> Option<DangerWarning> {
    let lower = command.to_lowercase();

    if lower.contains("rm -rf") || lower.contains("rm -fr") {
        return Some(DangerWarning {
            message: "Recursive force delete",
        });
    }
    if lower.contains("dd if=") && lower.contains("of=/dev/") {
        return Some(DangerWarning {
            message: "Direct disk write",
        });
    }
    if lower.contains("mkfs") {
        return Some(DangerWarning {
            message: "Filesystem format",
        });
    }
    if lower.contains("> /dev/sd") || lower.contains(">/dev/sd") {
        return Some(DangerWarning {
            message: "Direct device write",
        });
    }
    if lower.contains("chmod -r 777") || lower.contains("chmod 777 -r") {
        return Some(DangerWarning {
            message: "Overly permissive chmod",
        });
    }
    if lower.contains(":(){ :|:& };:") {
        return Some(DangerWarning {
            message: "Fork bomb",
        });
    }

    None
}

// ============================================================================
// Tokenizer
// ============================================================================

/// Tokenizes a shell command into words, respecting quotes and splitting operators.
///
/// Handles:
/// - Quoted strings (single, double) and backslash escapes
/// - Newlines as token boundaries (for multi-line commands merged from history)
/// - Pipe `|`, redirect `>` `>>` `<` `2>` `&>` `2>&1`, operators `&&` `||` `;` `&`
/// - Heredoc markers `<<EOF` `<<'EOF'` `<<-EOF` (consumed as single token)
/// - Operators inside quotes are NOT split
pub fn tokenize_command(command: &str) -> Vec<CommandToken> {
    let chars: Vec<char> = command.chars().collect();
    let len = chars.len();
    // Raw tokens: (text, Some(type)) for structural tokens, (text, None) for words
    let mut raw: Vec<(String, Option<TokenType>)> = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape_next = false;
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        if escape_next {
            current.push(ch);
            escape_next = false;
            i += 1;
            continue;
        }

        // Inside quotes: only handle quote termination and escapes
        if in_single_quote {
            current.push(ch);
            if ch == '\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_double_quote {
            current.push(ch);
            if ch == '\\' {
                escape_next = true;
            } else if ch == '"' {
                in_double_quote = false;
            }
            i += 1;
            continue;
        }

        // Outside quotes — handle structural characters
        match ch {
            '\\' => {
                current.push(ch);
                escape_next = true;
                i += 1;
            }
            '\'' => {
                current.push(ch);
                in_single_quote = true;
                i += 1;
            }
            '"' => {
                current.push(ch);
                in_double_quote = true;
                i += 1;
            }
            ' ' | '\t' | '\n' => {
                // Whitespace and newlines flush the current token
                if !current.is_empty() {
                    raw.push((current.clone(), None));
                    current.clear();
                }
                i += 1;
            }
            ';' => {
                if !current.is_empty() {
                    raw.push((current.clone(), None));
                    current.clear();
                }
                raw.push((";".to_string(), Some(TokenType::Operator)));
                i += 1;
            }
            '&' => {
                if !current.is_empty() {
                    raw.push((current.clone(), None));
                    current.clear();
                }
                if i + 1 < len && chars[i + 1] == '&' {
                    raw.push(("&&".to_string(), Some(TokenType::Operator)));
                    i += 2;
                } else if i + 1 < len && chars[i + 1] == '>' {
                    // &> (redirect stdout+stderr)
                    if i + 2 < len && chars[i + 2] == '>' {
                        raw.push(("&>>".to_string(), Some(TokenType::Redirect)));
                        i += 3;
                    } else {
                        raw.push(("&>".to_string(), Some(TokenType::Redirect)));
                        i += 2;
                    }
                } else {
                    // Background &
                    raw.push(("&".to_string(), Some(TokenType::Operator)));
                    i += 1;
                }
            }
            '|' => {
                if !current.is_empty() {
                    raw.push((current.clone(), None));
                    current.clear();
                }
                if i + 1 < len && chars[i + 1] == '|' {
                    raw.push(("||".to_string(), Some(TokenType::Operator)));
                    i += 2;
                } else if i + 1 < len && chars[i + 1] == '&' {
                    // |& (pipe stderr too, bash extension)
                    raw.push(("|&".to_string(), Some(TokenType::Pipe)));
                    i += 2;
                } else {
                    raw.push(("|".to_string(), Some(TokenType::Pipe)));
                    i += 1;
                }
            }
            '<' => {
                if !current.is_empty() {
                    raw.push((current.clone(), None));
                    current.clear();
                }
                if i + 1 < len && chars[i + 1] == '<' {
                    if i + 2 < len && chars[i + 2] == '<' {
                        // <<< here-string
                        raw.push(("<<<".to_string(), Some(TokenType::Redirect)));
                        i += 3;
                    } else {
                        // << heredoc — consume optional - and delimiter
                        let mut marker = String::from("<<");
                        i += 2;
                        let strip_tabs = if i < len && chars[i] == '-' {
                            marker.push('-');
                            i += 1;
                            true
                        } else {
                            false
                        };
                        // Skip whitespace between << and delimiter
                        while i < len && (chars[i] == ' ' || chars[i] == '\t') {
                            i += 1;
                        }
                        // Consume the delimiter (possibly quoted)
                        if i < len && (chars[i] == '\'' || chars[i] == '"') {
                            let quote = chars[i];
                            marker.push(quote);
                            i += 1;
                            while i < len && chars[i] != quote {
                                marker.push(chars[i]);
                                i += 1;
                            }
                            if i < len {
                                marker.push(chars[i]); // closing quote
                                i += 1;
                            }
                        } else {
                            // Unquoted delimiter — word characters
                            while i < len
                                && !matches!(
                                    chars[i],
                                    ' ' | '\t' | '\n' | ';' | '&' | '|' | '>' | '<'
                                )
                            {
                                marker.push(chars[i]);
                                i += 1;
                            }
                        }

                        // Extract the bare delimiter for body collection
                        let bare_delim = extract_heredoc_delimiter(&marker);
                        raw.push((marker, Some(TokenType::HeredocMarker)));

                        // Now consume body + closing delimiter directly from char stream
                        if let Some(delim) = bare_delim {
                            // Skip to next newline (rest of the marker line is ignored)
                            while i < len && chars[i] != '\n' {
                                i += 1;
                            }
                            if i < len {
                                i += 1; // skip the \n
                            }

                            // Collect lines until we find the closing delimiter
                            let mut body_lines: Vec<String> = Vec::new();
                            let mut found_closing = false;
                            while i < len {
                                // Read one line
                                let line_start = i;
                                while i < len && chars[i] != '\n' {
                                    i += 1;
                                }
                                let line: String = chars[line_start..i].iter().collect();
                                if i < len {
                                    i += 1; // skip \n
                                }

                                // Check if this line is the closing delimiter
                                let check_line = if strip_tabs {
                                    line.trim_start_matches('\t')
                                } else {
                                    &line
                                };
                                if check_line == delim {
                                    // Emit each body line as a separate HeredocBody token
                                    for body_line in &body_lines {
                                        raw.push((body_line.clone(), Some(TokenType::HeredocBody)));
                                    }
                                    // Emit closing delimiter
                                    raw.push((line, Some(TokenType::HeredocDelimiter)));
                                    found_closing = true;
                                    break;
                                }
                                body_lines.push(line);
                            }

                            if !found_closing && !body_lines.is_empty() {
                                // Unterminated heredoc: push remaining as plain tokens
                                for line in body_lines {
                                    if !line.is_empty() {
                                        raw.push((line, None));
                                    }
                                }
                            }
                        }
                    }
                } else {
                    raw.push(("<".to_string(), Some(TokenType::Redirect)));
                    i += 1;
                }
            }
            '>' => {
                // Check for fd prefix: if current is a single digit, it's an fd redirect
                let mut op = String::new();
                if current.len() == 1 && current.chars().next().unwrap().is_ascii_digit() {
                    op.push_str(&current);
                    current.clear();
                } else if !current.is_empty() {
                    raw.push((current.clone(), None));
                    current.clear();
                }
                op.push('>');
                i += 1;
                if i < len && chars[i] == '>' {
                    op.push('>');
                    i += 1;
                }
                // Check for >&N (e.g. 2>&1)
                if i < len && chars[i] == '&' {
                    op.push('&');
                    i += 1;
                    // Consume the fd number (e.g. the "1" in 2>&1)
                    while i < len && chars[i].is_ascii_digit() {
                        op.push(chars[i]);
                        i += 1;
                    }
                }
                raw.push((op, Some(TokenType::Redirect)));
            }
            _ => {
                current.push(ch);
                i += 1;
            }
        }
    }

    if !current.is_empty() {
        raw.push((current, None));
    }

    // Classification pass: assign types to non-structural tokens
    let mut tokens: Vec<CommandToken> = Vec::with_capacity(raw.len());
    for (idx, (text, structural_type)) in raw.into_iter().enumerate() {
        if let Some(tt) = structural_type {
            let mut tok = CommandToken::new(text, tt);
            // Structural tokens are locked (non-editable) except heredoc parts
            if !matches!(
                tt,
                TokenType::HeredocMarker | TokenType::HeredocDelimiter | TokenType::HeredocBody
            ) {
                tok.locked = true;
            }
            tokens.push(tok);
        } else {
            let prev_text = if idx > 0 {
                tokens.last().map(|t| t.text.as_str())
            } else {
                None
            };
            // Can't borrow tokens and call classify_token at the same time,
            // so extract what we need first
            let prev_token_text: Option<String> = prev_text.map(|s| s.to_string());
            let prev_type = tokens.last().map(|t| t.token_type);
            let token_type = classify_token(&text, idx, prev_token_text.as_deref(), prev_type);
            tokens.push(CommandToken::new(text, token_type));
        }
    }

    // Post-processing: collect heredoc bodies
    collect_heredoc_bodies(&mut tokens);

    tokens
}

/// Post-processes tokens to set up heredoc pair indices and lock body tokens.
///
/// The tokenizer now collects heredoc body/delimiter inline during tokenization.
/// This pass links HeredocMarker ↔ HeredocDelimiter via pair_index, locks body
/// tokens, and degrades unmatched markers to Argument.
fn collect_heredoc_bodies(tokens: &mut Vec<CommandToken>) {
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].token_type != TokenType::HeredocMarker {
            i += 1;
            continue;
        }

        let marker_idx = i;

        // Find the matching HeredocDelimiter after this marker
        let mut delim_idx = None;
        for j in (marker_idx + 1)..tokens.len() {
            if tokens[j].token_type == TokenType::HeredocDelimiter {
                delim_idx = Some(j);
                break;
            }
            // Stop if we hit another marker (nested heredocs)
            if tokens[j].token_type == TokenType::HeredocMarker {
                break;
            }
        }

        match delim_idx {
            Some(di) => {
                // Set pair_index linking marker ↔ delimiter
                tokens[marker_idx].pair_index = Some(di);
                tokens[di].pair_index = Some(marker_idx);

                // Body tokens between marker and delimiter are editable
                // (not locked — user can navigate to and edit them)

                i = di + 1;
            }
            None => {
                // No closing delimiter found — degrade marker to Argument
                tokens[marker_idx].token_type = TokenType::Argument;
                i += 1;
            }
        }
    }
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for command edit behavior.
#[derive(Debug, Clone)]
pub struct EditConfig {
    /// Enable dangerous command detection and confirmation.
    pub danger_check: bool,
    /// Enable undo/redo functionality.
    pub enable_undo: bool,
    /// Enable quote style cycling (Ctrl+Q).
    pub enable_quotes: bool,
    /// Maximum undo stack size.
    pub max_undo_size: usize,
}

impl Default for EditConfig {
    fn default() -> Self {
        Self {
            danger_check: true,
            enable_undo: true,
            enable_quotes: true,
            max_undo_size: 50,
        }
    }
}

impl EditConfig {
    /// Config for history browser (full features).
    pub fn for_history() -> Self {
        Self::default()
    }

    /// Config for file browser (minimal features).
    pub fn for_file() -> Self {
        Self {
            danger_check: false,
            enable_undo: true,
            enable_quotes: false,
            max_undo_size: 20,
        }
    }

    /// Config for schema browser command crafting.
    pub fn for_schema() -> Self {
        Self {
            danger_check: true,
            enable_undo: true,
            enable_quotes: true,
            max_undo_size: 50,
        }
    }
}

// ============================================================================
// Confirmation State
// ============================================================================

/// State for dangerous command confirmation.
#[derive(Debug, Clone)]
pub struct ConfirmState {
    /// The command awaiting confirmation.
    pub command: String,
    /// Warning message to display.
    pub warning: DangerWarning,
}

// ============================================================================
// Command Edit State
// ============================================================================

/// Unified state for command editing.
///
/// This is the core editing state used by both history and file browsers.
/// Features can be enabled/disabled via `EditConfig`.
#[derive(Clone)]
pub struct CommandEditState {
    // --- Core State ---
    /// The original command (for revert and change detection).
    pub original: String,
    /// Tokenized parts of the command.
    pub tokens: Vec<CommandToken>,
    /// Index of the currently selected token (0-based).
    pub selected: usize,
    /// Current edit buffer for the selected token.
    pub edit_buffer: String,

    // --- Configuration ---
    /// Feature configuration.
    pub config: EditConfig,

    // --- Undo State ---
    /// Undo stack for reverting changes (tokens, edit_buffer, selected).
    undo_stack: Vec<(Vec<CommandToken>, String, usize)>,

    // --- Original State ---
    /// Original tokens for revert (preserves locked state).
    original_tokens: Vec<CommandToken>,

    // --- Suggestion State ---
    /// Current suggestions for the selected token position.
    pub suggestions: Vec<String>,
    /// Index into suggestions (None = using custom/typed value).
    pub suggestion_index: Option<usize>,

    // --- Confirmation State ---
    /// Pending dangerous command confirmation.
    pub pending_confirm: Option<ConfirmState>,
    /// Skip dangerous command checks (toggled with Ctrl+!).
    pub skip_danger_check: bool,

    // --- Display Mode ---
    /// When true, the token strip uses a vertical stacked layout (one token per row).
    /// When false (default), uses horizontal single-row layout with scroll.
    pub strip_vertical: bool,

    // --- Context ---
    /// Optional context for suggestions (e.g., filename for file browser).
    context: Option<String>,

    // --- Intelligence Context ---
    /// History store for intelligent suggestions.
    history_store: Option<Arc<Mutex<HistoryStore>>>,
    /// Current working directory for context-aware suggestions.
    cwd: Option<PathBuf>,
    /// File context for file-type specific suggestions.
    file_context: Option<FileContext>,
    /// Last executed command for session-based suggestions.
    last_command: Option<String>,
}

impl std::fmt::Debug for CommandEditState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEditState")
            .field("original", &self.original)
            .field("tokens", &self.tokens)
            .field("selected", &self.selected)
            .field("edit_buffer", &self.edit_buffer)
            .field("config", &self.config)
            .field("suggestions", &self.suggestions)
            .field("suggestion_index", &self.suggestion_index)
            .field("pending_confirm", &self.pending_confirm)
            .field("skip_danger_check", &self.skip_danger_check)
            .field("strip_vertical", &self.strip_vertical)
            .field("context", &self.context)
            .field(
                "history_store",
                &self.history_store.as_ref().map(|_| "<HistoryStore>"),
            )
            .field("cwd", &self.cwd)
            .field("file_context", &self.file_context)
            .field("last_command", &self.last_command)
            .finish()
    }
}

impl CommandEditState {
    // ========================================================================
    // Constructors
    // ========================================================================

    /// Creates a new edit state from a list of tokens.
    pub fn new(mut tokens: Vec<CommandToken>, config: EditConfig) -> Self {
        // Guard against empty tokens to avoid panics in methods like insert_token_after
        if tokens.is_empty() {
            tokens.push(CommandToken::new(String::new(), TokenType::Command));
        }
        let original = tokens
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let edit_buffer = tokens.first().map(|t| t.text.clone()).unwrap_or_default();
        let original_tokens = tokens.clone();
        Self {
            original,
            tokens,
            selected: 0,
            edit_buffer,
            config,
            undo_stack: Vec::new(),
            original_tokens,
            suggestions: Vec::new(),
            suggestion_index: None,
            pending_confirm: None,
            skip_danger_check: false,
            strip_vertical: false,
            context: None,
            history_store: None,
            cwd: None,
            file_context: None,
            last_command: None,
        }
    }

    /// Creates edit state from a command string (for history browser).
    pub fn from_command(command: &str) -> Self {
        let mut tokens = tokenize_command(command);
        // Ensure tokens is never empty
        if tokens.is_empty() {
            tokens.push(CommandToken::new(String::new(), TokenType::Command));
        }
        Self::new(tokens, EditConfig::for_history())
    }

    /// Creates edit state for file editing with a locked filename token.
    pub fn for_file(filename: &str, filepath: &str) -> Self {
        let tokens = vec![
            CommandToken::new(String::new(), TokenType::Command),
            CommandToken::locked(filepath),
        ];
        let mut state = Self::new(tokens, EditConfig::for_file());
        state.context = Some(filename.to_string());
        state.update_suggestions();
        state
    }

    /// Creates edit state for schema-driven command crafting.
    ///
    /// Locked token(s) for the command path, an empty argument token for
    /// user input, and schema flags pre-populated as suggestions.
    pub fn for_schema(command: &str, subcommand: Option<&str>, flags: Vec<String>) -> Self {
        Self::for_schema_with_selections(command, subcommand, &[], flags)
    }

    /// Creates edit state with pre-selected flags already inserted as tokens.
    ///
    /// `selected_flags` are added as flag tokens between the command/subcommand
    /// and the empty argument token. Remaining `suggestions` are available for
    /// autocomplete.
    pub fn for_schema_with_selections(
        command: &str,
        subcommand: Option<&str>,
        selected_flags: &[String],
        suggestions: Vec<String>,
    ) -> Self {
        let mut tokens = vec![CommandToken::locked(command)];
        if let Some(sub) = subcommand {
            tokens.push(CommandToken::locked(sub));
        }
        for flag in selected_flags {
            tokens.push(CommandToken::new(flag.clone(), TokenType::Flag));
        }
        tokens.push(CommandToken::new(String::new(), TokenType::Argument));
        let last_idx = tokens.len() - 1;
        let mut state = Self::new(tokens, EditConfig::for_schema());
        // Select the empty argument token for editing
        state.selected = last_idx;
        state.edit_buffer.clear();
        state.suggestions = suggestions;
        state
    }

    // ========================================================================
    // Intelligence Context Setters
    // ========================================================================

    /// Sets the history store for intelligent suggestions.
    pub fn set_history_store(&mut self, store: Arc<Mutex<HistoryStore>>) {
        self.history_store = Some(store);
    }

    /// Sets the current working directory for context-aware suggestions.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = Some(cwd);
    }

    /// Sets the file context for file-type specific suggestions.
    pub fn set_file_context(&mut self, file_context: FileContext) {
        self.file_context = Some(file_context);
    }

    /// Sets the last executed command for session-based suggestions.
    pub fn set_last_command(&mut self, last_command: String) {
        self.last_command = Some(last_command);
    }

    /// Configures all intelligence context at once.
    pub fn set_intelligence_context(
        &mut self,
        history_store: Arc<Mutex<HistoryStore>>,
        cwd: Option<PathBuf>,
        file_context: Option<FileContext>,
        last_command: Option<String>,
    ) {
        self.history_store = Some(history_store);
        self.cwd = cwd;
        self.file_context = file_context;
        self.last_command = last_command;
    }

    // ========================================================================
    // Token Access
    // ========================================================================

    /// Returns the number of tokens.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Returns true if the selected token is locked.
    pub fn is_selected_locked(&self) -> bool {
        self.tokens
            .get(self.selected)
            .map(|t| t.locked)
            .unwrap_or(false)
    }

    /// Returns the currently selected token, if any.
    pub fn selected_token(&self) -> Option<&CommandToken> {
        self.tokens.get(self.selected)
    }

    /// Returns the type hint for the current token position.
    pub fn type_hint(&self) -> &'static str {
        match self.selected_token().map(|t| t.token_type) {
            Some(TokenType::Command) => "cmd",
            Some(TokenType::Subcommand) => "sub",
            Some(TokenType::Flag) => "flag",
            Some(TokenType::Path) => "path",
            Some(TokenType::Url) => "url",
            Some(TokenType::Locked) => "file",
            Some(TokenType::Pipe) => "pipe",
            Some(TokenType::Redirect) => "redir",
            Some(TokenType::Operator) => "op",
            Some(TokenType::HeredocMarker) => "heredoc",
            Some(TokenType::HeredocBody) => "body",
            Some(TokenType::HeredocDelimiter) => "delim",
            Some(TokenType::Argument) | None => "arg",
        }
    }

    // ========================================================================
    // Navigation
    // ========================================================================

    /// Saves current edit to the selected token, syncing heredoc pairs.
    fn save_current_edit(&mut self) {
        let sel = self.selected;
        if let Some(token) = self.tokens.get_mut(sel) {
            if !token.locked {
                token.text = self.edit_buffer.clone();
            }
        }
        // Sync heredoc pair if this is a marker or delimiter
        self.sync_heredoc_pair(sel);
    }

    /// Syncs heredoc pair when a marker or delimiter token has been edited.
    fn sync_heredoc_pair(&mut self, edited_idx: usize) {
        let (token_type, pair_idx, new_text) = {
            let Some(token) = self.tokens.get(edited_idx) else {
                return;
            };
            if !matches!(
                token.token_type,
                TokenType::HeredocMarker | TokenType::HeredocDelimiter
            ) {
                return;
            }
            let Some(pi) = token.pair_index else {
                return;
            };
            (token.token_type, pi, token.text.clone())
        };

        match token_type {
            TokenType::HeredocMarker => {
                // Extract new delimiter from marker, update the closing delimiter
                if let Some(new_delim) = extract_heredoc_delimiter(&new_text) {
                    if let Some(delim_token) = self.tokens.get_mut(pair_idx) {
                        delim_token.text = new_delim;
                    }
                }
            }
            TokenType::HeredocDelimiter => {
                // Update marker to use new delimiter, preserving quote style
                if let Some(marker_token) = self.tokens.get(pair_idx) {
                    let new_marker = rebuild_heredoc_marker(&marker_token.text, &new_text);
                    self.tokens[pair_idx].text = new_marker;
                }
            }
            _ => {}
        }
    }

    /// Selects a token by index.
    pub fn select(&mut self, index: usize) {
        if index < self.tokens.len() {
            self.save_current_edit();
            self.reclassify_tokens();
            self.selected = index;
            self.edit_buffer = self.tokens[index].text.clone();
            self.suggestion_index = None;
        }
    }

    /// Moves to the next editable token, skipping locked structural tokens.
    pub fn next(&mut self) {
        let mut target = self.selected + 1;
        while target < self.tokens.len() && self.is_structural_locked(target) {
            target += 1;
        }
        if target < self.tokens.len() {
            self.select(target);
        }
    }

    /// Moves to the previous editable token, skipping locked structural tokens.
    pub fn prev(&mut self) {
        if self.selected == 0 {
            return;
        }
        let mut target = self.selected - 1;
        while target > 0 && self.is_structural_locked(target) {
            target -= 1;
        }
        if !self.is_structural_locked(target) {
            self.select(target);
        }
    }

    /// Returns true if the token at the given index is a locked structural token
    /// that should be skipped during navigation.
    fn is_structural_locked(&self, idx: usize) -> bool {
        self.tokens
            .get(idx)
            .map(|t| {
                t.locked
                    && matches!(
                        t.token_type,
                        TokenType::Pipe | TokenType::Redirect | TokenType::Operator
                    )
            })
            .unwrap_or(false)
    }

    // ========================================================================
    // Editing
    // ========================================================================

    /// Toggles the lock state of the currently selected token.
    pub fn toggle_lock(&mut self) {
        if let Some(token) = self.tokens.get_mut(self.selected) {
            if token.locked {
                // Unlock: load token text into edit buffer for editing
                token.locked = false;
                self.edit_buffer = token.text.clone();
            } else {
                // Lock: save current edit buffer to token first
                token.text = self.edit_buffer.clone();
                token.locked = true;
            }
        }
    }

    /// Types a character into the edit buffer (if not locked).
    pub fn type_char(&mut self, c: char) {
        if !self.is_selected_locked() {
            self.save_undo();
            self.edit_buffer.push(c);
            self.suggestion_index = None;
        }
    }

    /// Deletes a character from the edit buffer (if not locked).
    pub fn backspace(&mut self) {
        if !self.is_selected_locked() {
            self.save_undo();
            self.edit_buffer.pop();
            self.suggestion_index = None;
        }
    }

    /// Builds the final command from the edited tokens.
    /// Locked tokens (e.g., file paths) are POSIX-quoted if they contain spaces or special chars.
    /// Heredoc body and delimiter tokens are joined with `\n` instead of space.
    pub fn build_command(&mut self) -> String {
        self.save_current_edit();
        let non_empty: Vec<&CommandToken> =
            self.tokens.iter().filter(|t| !t.text.is_empty()).collect();
        let mut result = String::new();
        let mut prev_type: Option<TokenType> = None;
        for t in &non_empty {
            if !result.is_empty() {
                // Use newline before heredoc body, delimiter, or after heredoc marker
                if matches!(
                    t.token_type,
                    TokenType::HeredocBody | TokenType::HeredocDelimiter
                ) || matches!(prev_type, Some(TokenType::HeredocMarker))
                {
                    result.push('\n');
                } else {
                    result.push(' ');
                }
            }
            // Don't POSIX-quote structural tokens, heredoc body, or delimiter
            if matches!(
                t.token_type,
                TokenType::HeredocBody
                    | TokenType::HeredocDelimiter
                    | TokenType::Pipe
                    | TokenType::Redirect
                    | TokenType::Operator
                    | TokenType::HeredocMarker
            ) {
                result.push_str(&t.text);
            } else if t.locked {
                result.push_str(&quote_for_shell(&t.text));
            } else {
                result.push_str(&t.text);
            }
            prev_type = Some(t.token_type);
        }
        result
    }

    /// Reclassifies all non-locked tokens based on their position.
    pub fn reclassify_tokens(&mut self) {
        for i in 0..self.tokens.len() {
            if self.tokens[i].locked {
                continue;
            }
            // Skip structural tokens — their type is fixed
            if matches!(
                self.tokens[i].token_type,
                TokenType::Pipe
                    | TokenType::Redirect
                    | TokenType::Operator
                    | TokenType::HeredocMarker
                    | TokenType::HeredocBody
                    | TokenType::HeredocDelimiter
            ) {
                continue;
            }
            let prev_text: Option<String> = if i > 0 {
                Some(self.tokens[i - 1].text.clone())
            } else {
                None
            };
            let prev_type = if i > 0 {
                Some(self.tokens[i - 1].token_type)
            } else {
                None
            };
            self.tokens[i].token_type =
                classify_token(&self.tokens[i].text, i, prev_text.as_deref(), prev_type);
        }
    }

    // ========================================================================
    // Token Mutation
    // ========================================================================

    /// Saves current state to undo stack.
    fn save_undo(&mut self) {
        if !self.config.enable_undo {
            return;
        }
        self.undo_stack
            .push((self.tokens.clone(), self.edit_buffer.clone(), self.selected));
        if self.undo_stack.len() > self.config.max_undo_size {
            self.undo_stack.remove(0);
        }
    }

    /// Restores previous state from undo stack.
    pub fn undo(&mut self) {
        if let Some((tokens, edit_buffer, selected)) = self.undo_stack.pop() {
            self.tokens = tokens;
            self.selected = selected.min(self.tokens.len().saturating_sub(1));
            self.edit_buffer = edit_buffer;
            self.reclassify_tokens();
        }
    }

    /// Deletes the currently selected token (if not locked).
    pub fn delete_token(&mut self) {
        if self.is_selected_locked() {
            return;
        }
        // Don't delete if only one non-locked token remains
        let non_locked_count = self.tokens.iter().filter(|t| !t.locked).count();
        if non_locked_count <= 1 {
            return;
        }
        self.save_undo();
        self.tokens.remove(self.selected);
        if self.selected >= self.tokens.len() {
            self.selected = self.tokens.len().saturating_sub(1);
        }
        self.edit_buffer = self
            .tokens
            .get(self.selected)
            .map(|t| t.text.clone())
            .unwrap_or_default();
        self.reclassify_tokens();
        self.suggestion_index = None;
    }

    /// Inserts a new token after the current one.
    pub fn insert_token_after(&mut self) {
        self.save_undo();
        self.save_current_edit();
        let new_token = CommandToken::new(String::new(), TokenType::Argument);
        let insert_pos = self.selected + 1;
        self.tokens.insert(insert_pos, new_token);
        self.selected = insert_pos;
        self.edit_buffer.clear();
        self.reclassify_tokens();
        self.suggestion_index = None;
    }

    /// Inserts a new token before the current one.
    pub fn insert_token_before(&mut self) {
        self.save_undo();
        self.save_current_edit();
        let new_token = CommandToken::new(String::new(), TokenType::Argument);
        self.tokens.insert(self.selected, new_token);
        // selected now points to the new empty token
        self.edit_buffer.clear();
        self.reclassify_tokens();
        self.suggestion_index = None;
    }

    // ========================================================================
    // Quote Cycling
    // ========================================================================

    /// Cycles through quote styles for current token.
    pub fn cycle_quote(&mut self) {
        if !self.config.enable_quotes || self.is_selected_locked() {
            return;
        }
        self.save_undo();
        let (inner, current_style) = split_quotes(&self.edit_buffer);
        let new_style = match current_style {
            QuoteStyle::None => QuoteStyle::Single,
            QuoteStyle::Single => QuoteStyle::Double,
            QuoteStyle::Double => QuoteStyle::None,
        };
        self.edit_buffer = apply_quotes(&inner, new_style);
    }

    // ========================================================================
    // Suggestions
    // ========================================================================

    /// Cycles through suggestions in the given direction.
    pub fn cycle_suggestion(&mut self, direction: i32) {
        if self.suggestions.is_empty() || self.is_selected_locked() {
            return;
        }

        let new_index = match self.suggestion_index {
            None => {
                if direction > 0 {
                    0
                } else {
                    self.suggestions.len() - 1
                }
            }
            Some(idx) => {
                let len = self.suggestions.len();
                if direction > 0 {
                    (idx + 1) % len
                } else {
                    (idx + len - 1) % len
                }
            }
        };

        self.suggestion_index = Some(new_index);
        self.edit_buffer = self.suggestions[new_index].clone();
    }

    /// Returns the previous suggestion for display.
    pub fn prev_suggestion(&self) -> Option<&str> {
        if self.suggestions.is_empty() {
            return None;
        }
        let idx = self.suggestion_index.unwrap_or(0);
        let len = self.suggestions.len();
        let prev_idx = if idx == 0 { len - 1 } else { idx - 1 };
        self.suggestions.get(prev_idx).map(|s| s.as_str())
    }

    /// Returns the next suggestion for display.
    pub fn next_suggestion(&self) -> Option<&str> {
        if self.suggestions.is_empty() {
            return None;
        }
        let len = self.suggestions.len();
        // Treat None as one-before-start so (len - 1 + 1) % len = 0
        let idx = self.suggestion_index.unwrap_or(len - 1);
        let next_idx = (idx + 1) % len;
        self.suggestions.get(next_idx).map(|s| s.as_str())
    }

    /// Updates suggestions based on current context.
    ///
    /// When on a locked token, suggestions are preserved (not cleared) so
    /// the suggestion rows remain visible for context. Users can't cycle
    /// suggestions on locked tokens, but they can still see what was suggested.
    ///
    /// Suggestions are gathered from the learned command hierarchy, which includes
    /// bootstrapped knowledge for common commands. The hierarchy learns from user
    /// command history and provides position-aware token suggestions.
    pub fn update_suggestions(&mut self) {
        // Suppress suggestions for locked tokens and heredoc body lines
        let is_body = self
            .tokens
            .get(self.selected)
            .map(|t| t.token_type == TokenType::HeredocBody)
            .unwrap_or(false);
        if is_body {
            self.suggestions.clear();
            self.suggestion_index = None;
            return;
        }
        if self.is_selected_locked() {
            // Don't clear suggestions on locked tokens - preserve them for display
            return;
        }

        self.suggestions.clear();

        // Primary source: intelligent suggestions from learned hierarchy
        // The hierarchy includes bootstrapped data, so it handles all cases
        if let Some(ref store) = self.history_store {
            if let Ok(store) = store.lock() {
                if store.is_intelligence_enabled() {
                    let tokens = &self.tokens[..self.selected];

                    // Only apply prefix filter when user has modified the text
                    // If edit_buffer matches the original token, show all suggestions
                    let current_token_text = self
                        .tokens
                        .get(self.selected)
                        .map(|t| t.text.as_str())
                        .unwrap_or("");
                    let partial = if self.edit_buffer == current_token_text {
                        "" // Show all suggestions - user hasn't started typing
                    } else {
                        &self.edit_buffer // Filter by what user is typing
                    };

                    let intelligent_suggestions = store.intelligent_suggest(
                        tokens,
                        partial,
                        self.cwd.clone(),
                        self.file_context.clone(),
                        self.last_command.clone(),
                    );

                    self.suggestions = intelligent_suggestions
                        .into_iter()
                        .map(|s| s.text)
                        .collect();
                }
            }
        }

        self.suggestion_index = None;
    }

    /// Adds external suggestions (e.g., from history) to the front.
    pub fn add_suggestions(&mut self, external: Vec<String>) {
        // Merge: external first (more relevant), then existing
        let mut merged = Vec::new();
        for sugg in external {
            if !merged.contains(&sugg) {
                merged.push(sugg);
            }
        }
        for sugg in &self.suggestions {
            if !merged.contains(sugg) {
                merged.push(sugg.clone());
            }
        }
        self.suggestions = merged;
    }

    // ========================================================================
    // Change Detection
    // ========================================================================

    /// Returns true if there are any changes from the original.
    pub fn is_changed(&self) -> bool {
        let current: String = self
            .tokens
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.selected && !t.locked {
                    self.edit_buffer.clone()
                } else {
                    t.text.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        current != self.original
    }

    /// Reverts all changes back to the original command.
    pub fn revert(&mut self) {
        self.tokens = self.original_tokens.clone();
        if self.tokens.is_empty() {
            self.tokens
                .push(CommandToken::new(String::new(), TokenType::Command));
        }
        self.selected = 0;
        self.edit_buffer = self
            .tokens
            .first()
            .map(|t| t.text.clone())
            .unwrap_or_default();
        self.undo_stack.clear();
        self.suggestion_index = None;
    }

    // ========================================================================
    // Dangerous Command Handling
    // ========================================================================

    /// Checks if current command is dangerous and needs confirmation.
    /// Returns the command if safe to execute, None if confirmation needed.
    pub fn check_and_prepare_execute(&mut self) -> Option<String> {
        let command = self.build_command();

        if !self.config.danger_check || self.skip_danger_check {
            return Some(command);
        }

        if let Some(warning) = check_dangerous_command(&command) {
            self.pending_confirm = Some(ConfirmState { command, warning });
            return None;
        }

        Some(command)
    }

    /// Confirms and returns the pending dangerous command.
    pub fn confirm_dangerous(&mut self) -> Option<String> {
        self.pending_confirm.take().map(|c| c.command)
    }

    /// Cancels the pending dangerous command confirmation.
    pub fn cancel_confirm(&mut self) {
        self.pending_confirm = None;
    }

    /// Returns true if waiting for confirmation.
    pub fn is_confirming(&self) -> bool {
        self.pending_confirm.is_some()
    }

    /// Toggles the skip_danger_check flag.
    pub fn toggle_danger_check(&mut self) {
        self.skip_danger_check = !self.skip_danger_check;
    }

    /// Toggles the token strip display mode between horizontal and vertical.
    pub fn toggle_strip_mode(&mut self) {
        self.strip_vertical = !self.strip_vertical;
    }
}

// ============================================================================
// Adaptive Edit Mode Layout
// ============================================================================

/// Layout rectangles for the adaptive edit mode UI.
///
/// The edit mode progressively drops optional elements based on available height:
/// - Height >= 11: Full layout (spacers + prev/next suggestions)
/// - Height 7-8: Drop spacer rows
/// - Height 5-6: Also drop prev/next suggestion rows
/// - Height < 5: Too small for edit mode (returns None)
///
/// Border and keybind rows are rendered externally by TabbedPanel's footer
/// compositor (FooterBar + BorderLine widgets).
///
/// Optional fields (`prev_suggestion`, `next_suggestion`) are `None` when
/// the terminal is too short to display them.
pub struct EditModeLayout {
    /// Title row showing the edit mode header.
    pub title: Rect,
    /// Horizontal separator below the title.
    pub separator: Rect,
    /// Row for the previous suggestion hint (hidden when height < 7).
    pub prev_suggestion: Option<Rect>,
    /// Scrollable token strip showing bracketed command tokens.
    pub token_strip: Rect,
    /// Row for the next suggestion hint (hidden when height < 7).
    pub next_suggestion: Option<Rect>,
    /// Text input area for editing the selected token.
    pub edit_input: Rect,
    /// Preview of the assembled command result.
    pub result_preview: Rect,
}

/// Computes the adaptive edit mode layout for the given area.
///
/// Returns `None` if the area is too small (height < 5) for a usable edit mode.
/// Border and keybind rows are rendered externally by TabbedPanel's footer compositor.
///
/// When `vertical_strip` is true, the token strip gets multiple rows (scaled by
/// available height) for the vertical stacked token layout.
pub fn compute_edit_mode_layout(area: Rect, vertical_strip: bool) -> Option<EditModeLayout> {
    if area.height < 5 {
        return None;
    }

    let show_suggestions = area.height >= 7;
    let show_spacers = area.height >= 9;

    // Token strip rows: 1 for horizontal mode, scaled for vertical
    let token_strip_rows: u16 = if vertical_strip {
        if area.height >= 14 {
            5
        } else if area.height >= 12 {
            4
        } else if area.height >= 10 {
            3
        } else if area.height >= 8 {
            2
        } else {
            1
        }
    } else {
        1
    };

    let mut constraints = Vec::new();
    constraints.push(Constraint::Length(1)); // title
    constraints.push(Constraint::Length(1)); // separator
    if show_suggestions {
        constraints.push(Constraint::Length(1)); // prev suggestion
    }
    constraints.push(Constraint::Length(token_strip_rows)); // token strip
    if show_suggestions {
        constraints.push(Constraint::Length(1)); // next suggestion
    }
    if show_spacers {
        constraints.push(Constraint::Length(1)); // spacer
    }
    constraints.push(Constraint::Length(1)); // edit input
    if show_spacers {
        constraints.push(Constraint::Length(1)); // spacer
    }
    constraints.push(Constraint::Min(1)); // result preview

    let chunks = Layout::vertical(constraints).split(area);

    let mut idx = 0;
    let title = chunks[idx];
    idx += 1;
    let separator = chunks[idx];
    idx += 1;
    let prev_suggestion = if show_suggestions {
        let r = chunks[idx];
        idx += 1;
        Some(r)
    } else {
        None
    };
    let token_strip = chunks[idx];
    idx += 1;
    let next_suggestion = if show_suggestions {
        let r = chunks[idx];
        idx += 1;
        Some(r)
    } else {
        None
    };
    if show_spacers {
        idx += 1; // spacer
    }
    let edit_input = chunks[idx];
    idx += 1;
    if show_spacers {
        idx += 1; // spacer
    }
    let result_preview = chunks[idx];

    Some(EditModeLayout {
        title,
        separator,
        prev_suggestion,
        token_strip,
        next_suggestion,
        edit_input,
        result_preview,
    })
}

/// Renders the shared edit mode UI elements (token strip, suggestions, edit input,
/// and result preview) using the computed layout.
///
/// The caller is responsible for rendering the title and separator, as those
/// differ between history browser and file browser. Border and keybind hints
/// are rendered externally by TabbedPanel's footer compositor.
///
/// Supports two token strip display modes:
/// - **Horizontal** (default): Single-row strip with horizontal scrolling,
///   suggestions aligned under the selected token.
/// - **Vertical**: One token per row, vertically scrolled, suggestions left-aligned.
pub fn render_edit_mode_shared(
    buffer: &mut Buffer,
    theme: &Theme,
    glyphs: &crate::chrome::glyphs::GlyphSet,
    edit_state: &CommandEditState,
    layout: &EditModeLayout,
) {
    let bracket_style = Style::default().fg(theme.text_secondary);
    let bracket_selected_style = Style::default().fg(theme.header_fg);

    if edit_state.strip_vertical {
        // === VERTICAL MODE: one token per row, vertically scrolled ===

        // Previous suggestion row (left-aligned)
        if let Some(prev_area) = layout.prev_suggestion {
            if let Some(prev_sugg) = edit_state.prev_suggestion() {
                let prev_line = Line::from(vec![
                    Span::styled("   ", Style::default()),
                    Span::styled(prev_sugg, Style::default().fg(theme.text_secondary)),
                ]);
                Paragraph::new(prev_line).render(prev_area, buffer);
            }
        }

        let mut token_lines: Vec<Line<'_>> = Vec::new();
        for (i, token) in edit_state.tokens.iter().enumerate() {
            let is_selected = i == edit_state.selected;
            let slot_num = i + 1;
            let mut row_spans = Vec::new();

            if is_selected {
                row_spans.push(Span::styled(
                    " \u{25b8} ",
                    Style::default()
                        .fg(theme.text_highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                row_spans.push(Span::styled("   ", Style::default()));
            }

            let num_style = if is_selected {
                Style::default()
                    .fg(theme.text_highlight)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_secondary)
            };
            row_spans.push(Span::styled(superscript_number(slot_num), num_style));

            let bstyle = if is_selected {
                bracket_selected_style
            } else {
                bracket_style
            };
            row_spans.push(Span::styled("\u{27e6}", bstyle));

            let base_style = token_type_style(token.token_type, theme);
            let token_style = if is_selected {
                base_style.add_modifier(Modifier::BOLD)
            } else {
                base_style
            };

            let display_text = if is_selected {
                if edit_state.edit_buffer.is_empty() {
                    "_".to_string()
                } else {
                    edit_state.edit_buffer.clone()
                }
            } else if token.text.is_empty() {
                "_".to_string()
            } else {
                token.text.clone()
            };
            row_spans.push(Span::styled(display_text, token_style));
            row_spans.push(Span::styled("\u{27e7}", bstyle));

            if token.locked {
                row_spans.push(Span::styled(
                    glyphs.indicator.lock,
                    Style::default().fg(theme.text_secondary),
                ));
            }

            token_lines.push(Line::from(row_spans));
        }

        let strip_height = layout.token_strip.height as usize;
        let v_scroll = if strip_height > 0 && edit_state.selected >= strip_height {
            edit_state
                .selected
                .saturating_sub(strip_height / 2)
                .min(token_lines.len().saturating_sub(strip_height))
        } else {
            0
        };
        Paragraph::new(token_lines)
            .scroll((v_scroll as u16, 0))
            .render(layout.token_strip, buffer);

        // Next suggestion row (left-aligned)
        if let Some(next_area) = layout.next_suggestion {
            if let Some(next_sugg) = edit_state.next_suggestion() {
                let next_line = Line::from(vec![
                    Span::styled("   ", Style::default()),
                    Span::styled(next_sugg, Style::default().fg(theme.text_secondary)),
                ]);
                Paragraph::new(next_line).render(next_area, buffer);
            }
        }
    } else {
        // === HORIZONTAL MODE (default): single-row strip with horizontal scrolling ===

        // Calculate x-positions for the selected token (for suggestion alignment)
        let mut selected_x_start: usize = 3; // Initial padding "   "
        let mut selected_x_end: usize = 3;
        for (i, token) in edit_state.tokens.iter().enumerate() {
            let display_text = if i == edit_state.selected {
                if edit_state.edit_buffer.is_empty() {
                    "_"
                } else {
                    &edit_state.edit_buffer
                }
            } else if token.text.is_empty() {
                "_"
            } else {
                &token.text
            };
            let slot_num = i + 1;
            let superscript_len = superscript_number(slot_num).chars().count();
            let text_display_width = crate::ui::text_width::display_width(display_text);
            let lock_width = if token.locked {
                crate::ui::text_width::display_width(glyphs.indicator.lock)
            } else {
                0
            };
            // superscript + ⟦ + text + ⟧ + lock + gap
            let gap_width = if matches!(
                token.token_type,
                TokenType::Pipe | TokenType::Redirect | TokenType::Operator
            ) {
                1
            } else {
                3
            };
            let token_width = superscript_len + 1 + text_display_width + 1 + lock_width + gap_width;

            if i == edit_state.selected {
                selected_x_end = selected_x_start + superscript_len + 1 + text_display_width + 1;
                break;
            }
            selected_x_start += token_width;
        }
        let superscript_len = superscript_number(edit_state.selected + 1).chars().count();
        let selected_x_offset = selected_x_start + superscript_len + 1;

        // Calculate horizontal scroll offset to keep selected token visible
        let viewport_width = layout.token_strip.width as usize;
        let left_context = viewport_width / 3;
        let right_margin = 8;
        let scroll_offset = if selected_x_end > viewport_width.saturating_sub(right_margin) {
            selected_x_start.saturating_sub(left_context)
        } else {
            0
        };

        // Previous suggestion row (aligned under selected token)
        if let Some(prev_area) = layout.prev_suggestion {
            if let Some(prev_sugg) = edit_state.prev_suggestion() {
                let adjusted_offset = selected_x_offset.saturating_sub(scroll_offset);
                let padding = " ".repeat(adjusted_offset);
                let prev_line = Line::from(vec![
                    Span::styled(padding, Style::default()),
                    Span::styled(prev_sugg, Style::default().fg(theme.text_secondary)),
                ]);
                Paragraph::new(prev_line).render(prev_area, buffer);
            }
        }

        // Build horizontal token strip
        let mut spans = Vec::new();
        spans.push(Span::styled("   ", Style::default()));

        for (i, token) in edit_state.tokens.iter().enumerate() {
            let is_selected = i == edit_state.selected;
            let slot_num = i + 1;

            let num_style = if is_selected {
                Style::default()
                    .fg(theme.text_highlight)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_secondary)
            };
            spans.push(Span::styled(superscript_number(slot_num), num_style));

            let bstyle = if is_selected {
                bracket_selected_style
            } else {
                bracket_style
            };
            spans.push(Span::styled("\u{27e6}", bstyle));

            let base_style = token_type_style(token.token_type, theme);
            let token_style = if is_selected {
                base_style.add_modifier(Modifier::BOLD)
            } else {
                base_style
            };

            let display_text = if is_selected {
                if edit_state.edit_buffer.is_empty() {
                    "_".to_string()
                } else {
                    edit_state.edit_buffer.clone()
                }
            } else if token.text.is_empty() {
                "_".to_string()
            } else {
                token.text.clone()
            };
            spans.push(Span::styled(display_text, token_style));
            spans.push(Span::styled("\u{27e7}", bstyle));

            // Lock indicator after bracket
            if token.locked {
                spans.push(Span::styled(
                    glyphs.indicator.lock,
                    Style::default().fg(theme.text_secondary),
                ));
            }

            // Tighter spacing for structural tokens
            let gap = if matches!(
                token.token_type,
                TokenType::Pipe | TokenType::Redirect | TokenType::Operator
            ) {
                " "
            } else {
                "   "
            };
            spans.push(Span::raw(gap));
        }

        let token_line = Line::from(spans);
        let scroll_offset_u16 = (scroll_offset.min(u16::MAX as usize)) as u16;
        Paragraph::new(token_line)
            .scroll((0, scroll_offset_u16))
            .render(layout.token_strip, buffer);

        // Next suggestion row (aligned under selected token)
        if let Some(next_area) = layout.next_suggestion {
            if let Some(next_sugg) = edit_state.next_suggestion() {
                let adjusted_offset = selected_x_offset.saturating_sub(scroll_offset);
                let padding = " ".repeat(adjusted_offset);
                let next_line = Line::from(vec![
                    Span::styled(padding, Style::default()),
                    Span::styled(next_sugg, Style::default().fg(theme.text_secondary)),
                ]);
                Paragraph::new(next_line).render(next_area, buffer);
            }
        }
    }

    // Edit input line with type hint and cycling indicator
    let type_hint = edit_state.type_hint();
    let cycling_indicator = if edit_state.suggestion_index.is_some() {
        format!(
            " [{}/{}]",
            edit_state.suggestion_index.unwrap_or(0) + 1,
            edit_state.suggestions.len()
        )
    } else {
        String::new()
    };
    let edit_label = format!(
        "   {} {} > ",
        superscript_number(edit_state.selected + 1),
        type_hint
    );
    let edit_line = Line::from(vec![
        Span::styled(edit_label, Style::default().fg(theme.git_fg)),
        Span::styled(
            &edit_state.edit_buffer,
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            String::from(glyphs.progress.block_full),
            Style::default().fg(theme.header_fg),
        ),
        Span::styled(cycling_indicator, Style::default().fg(theme.text_secondary)),
    ]);
    Paragraph::new(edit_line).render(layout.edit_input, buffer);

    // Result preview — shows the actual command that will be executed (no abbreviation).
    // For heredoc pairs, live-sync the paired token so the preview updates in real-time.
    let live_pair_text: Option<(usize, String)> = {
        let sel = edit_state.selected;
        edit_state.tokens.get(sel).and_then(|tok| {
            let pi = tok.pair_index?;
            match tok.token_type {
                TokenType::HeredocDelimiter => {
                    // Editing delimiter → update marker in preview
                    let marker = &edit_state.tokens.get(pi)?.text;
                    Some((pi, rebuild_heredoc_marker(marker, &edit_state.edit_buffer)))
                }
                TokenType::HeredocMarker => {
                    // Editing marker → update delimiter in preview
                    let new_delim = extract_heredoc_delimiter(&edit_state.edit_buffer)?;
                    Some((pi, new_delim))
                }
                _ => None,
            }
        })
    };

    let result_preview: String = {
        let mut result = String::new();
        let mut prev_tt: Option<TokenType> = None;
        for (i, t) in edit_state.tokens.iter().enumerate() {
            let text: &str = if i == edit_state.selected {
                &edit_state.edit_buffer
            } else if let Some((pair_idx, ref synced)) = live_pair_text {
                if i == pair_idx { synced } else { &t.text }
            } else {
                &t.text
            };
            if text.is_empty() {
                continue;
            }
            if !result.is_empty() {
                if matches!(
                    t.token_type,
                    TokenType::HeredocBody | TokenType::HeredocDelimiter
                ) || matches!(prev_tt, Some(TokenType::HeredocMarker))
                {
                    result.push('\n');
                } else {
                    result.push(' ');
                }
            }
            result.push_str(text);
            prev_tt = Some(t.token_type);
        }
        result
    };

    let preview_changed = result_preview != edit_state.original;
    let preview_style = if preview_changed {
        Style::default().fg(theme.semantic_success)
    } else {
        Style::default().fg(theme.text_primary)
    };
    // Split result by newlines so heredoc content renders on separate lines
    let result_lines: Vec<&str> = result_preview.split('\n').collect();
    let mut paragraph_lines: Vec<Line<'_>> = Vec::new();
    for (line_idx, line_text) in result_lines.iter().enumerate() {
        if line_idx == 0 {
            paragraph_lines.push(Line::from(vec![
                Span::styled("  Result: ", Style::default().fg(theme.text_secondary)),
                Span::styled(*line_text, preview_style),
            ]));
        } else {
            paragraph_lines.push(Line::from(Span::styled(
                format!("          {line_text}"),
                preview_style,
            )));
        }
    }
    Paragraph::new(paragraph_lines)
        .wrap(Wrap { trim: false })
        .render(layout.result_preview, buffer);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_command_simple_classifies_tokens() {
        let tokens = tokenize_command("ls -la /tmp");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].text, "ls");
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].text, "-la");
        assert_eq!(tokens[1].token_type, TokenType::Flag);
        assert_eq!(tokens[2].text, "/tmp");
        assert_eq!(tokens[2].token_type, TokenType::Path);
    }

    #[test]
    fn test_tokenize_command_quoted_preserves_quotes() {
        let tokens = tokenize_command("echo \"hello world\"");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[1].text, "\"hello world\"");
    }

    #[test]
    fn test_tokenize_command_git_checkout_flags_identified() {
        let tokens = tokenize_command("git checkout -b feature");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].token_type, TokenType::Subcommand);
        assert_eq!(tokens[2].token_type, TokenType::Flag);
    }

    #[test]
    fn test_for_file_creates_locked_token() {
        let state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        assert_eq!(state.tokens.len(), 2);
        assert!(!state.tokens[0].locked);
        assert!(state.tokens[1].locked);
        assert_eq!(state.tokens[1].text, "/path/to/test.rs");
    }

    #[test]
    fn test_for_file_starts_with_empty_suggestions_without_intelligence() {
        // Without intelligence configured, for_file creates state but suggestions are empty
        // Suggestions require a HistoryStore with intelligence enabled
        let state = CommandEditState::for_file("test.rs", "/path/to/test.rs");

        // State is correctly initialized
        assert_eq!(state.tokens.len(), 2);
        assert_eq!(state.tokens[0].text, "");
        assert_eq!(state.tokens[1].text, "/path/to/test.rs");

        // Without intelligence, suggestions are empty (no static fallback)
        assert!(
            state.suggestions.is_empty(),
            "Without intelligence, suggestions should be empty"
        );
    }

    #[test]
    fn test_next_prev_suggestion_cycles_through_list() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        // Manually populate suggestions to test cycling mechanics
        state.suggestions = vec!["cat".to_string(), "less".to_string(), "vim".to_string()];

        // Should return suggestions for display
        assert!(
            state.next_suggestion().is_some(),
            "next_suggestion should return Some when suggestions exist"
        );
        assert!(
            state.prev_suggestion().is_some(),
            "prev_suggestion should return Some when suggestions exist"
        );

        // Verify cycling works correctly
        assert_eq!(state.next_suggestion(), Some("cat"));
        assert_eq!(state.prev_suggestion(), Some("vim"));
    }

    #[test]
    fn test_suggestions_preserved_on_locked_token() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        // Manually populate suggestions to test preservation behavior
        state.suggestions = vec!["cat".to_string(), "less".to_string(), "vim".to_string()];
        let initial_count = state.suggestions.len();

        // Move to locked token (filename)
        state.next();
        state.update_suggestions();

        // Suggestions should be preserved, not cleared (key behavior being tested)
        assert_eq!(
            state.suggestions.len(),
            initial_count,
            "Suggestions should be preserved when on locked token"
        );
        assert!(
            state.next_suggestion().is_some(),
            "next_suggestion should still work on locked token"
        );
    }

    #[test]
    fn test_history_browser_suggestions_cycling() {
        // Test suggestion cycling in history browser context
        let mut state =
            CommandEditState::from_command("git remote add origin git@github.com:user/repo.git");

        // Manually populate suggestions to test cycling mechanics
        state.suggestions = vec![
            "status".to_string(),
            "push".to_string(),
            "pull".to_string(),
            "commit".to_string(),
        ];

        // Both prev and next should return values
        let prev = state.prev_suggestion();
        let next = state.next_suggestion();

        assert!(
            prev.is_some(),
            "prev_suggestion should return Some when suggestions exist"
        );
        assert!(
            next.is_some(),
            "next_suggestion should return Some when suggestions exist"
        );

        // They should be different (cycling through list)
        assert_ne!(
            prev, next,
            "prev and next should show different suggestions"
        );

        // Test cycling updates edit_buffer
        state.cycle_suggestion(1);
        assert!(
            !state.edit_buffer.is_empty(),
            "edit_buffer should update after cycling"
        );
    }

    #[test]
    fn test_update_suggestions_clears_without_intelligence() {
        // Without intelligence configured, update_suggestions clears suggestions
        let mut state = CommandEditState::from_command("git status");

        // Manually populate some suggestions
        state.suggestions = vec!["push".to_string(), "pull".to_string()];

        // Call update_suggestions without history_store set
        state.update_suggestions();

        // Without intelligence, suggestions are cleared
        assert!(
            state.suggestions.is_empty(),
            "Without intelligence, update_suggestions should clear suggestions"
        );
    }

    #[test]
    fn test_file_browser_build_command_after_cycling() {
        // Simulate file browser edit mode flow
        let mut state = CommandEditState::for_file("test.txt", "/path/to/test.txt");

        // Initially token 0 is empty, token 1 is the locked filepath
        assert_eq!(state.tokens[0].text, "");
        assert_eq!(state.tokens[1].text, "/path/to/test.txt");
        assert!(state.tokens[1].locked);

        // Manually populate suggestions to test cycling behavior
        state.suggestions = vec!["cat".to_string(), "less".to_string(), "vim".to_string()];

        // User cycles through suggestions to select "cat"
        state.cycle_suggestion(1); // Cycle to first suggestion
        assert_eq!(
            state.edit_buffer, "cat",
            "Edit buffer should have 'cat' after cycling"
        );

        // User presses Enter - build_command should create the full command
        let command = state.build_command();

        assert!(!command.is_empty(), "Command should not be empty");
        assert!(
            command.starts_with("cat"),
            "Command should start with 'cat', got: {}",
            command
        );
        assert!(
            command.contains("/path/to/test.txt"),
            "Command should contain the filepath, got: {}",
            command
        );
        assert_eq!(
            command, "cat /path/to/test.txt",
            "Full command should be 'cat /path/to/test.txt'"
        );
    }

    #[test]
    fn test_insert_token_after_increments_count_and_selection() {
        let mut state = CommandEditState::from_command("git push");
        assert_eq!(state.token_count(), 2);
        state.select(1);
        state.insert_token_after();
        assert_eq!(state.token_count(), 3);
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn test_delete_token_locked_token_preserves_count() {
        let mut state = CommandEditState::for_file("test.rs", "/path/to/test.rs");
        state.select(1); // Select locked token
        state.delete_token();
        assert_eq!(state.tokens.len(), 2); // Should not delete
    }

    #[test]
    fn test_undo_after_delete_restores_token_count() {
        let mut state = CommandEditState::from_command("git push origin main");
        assert_eq!(state.token_count(), 4);
        state.select(2);
        state.delete_token();
        assert_eq!(state.token_count(), 3);
        state.undo();
        assert_eq!(state.token_count(), 4);
    }

    #[test]
    fn test_cycle_quote_selected_token_updates_edit_buffer_and_quotes() {
        let mut state = CommandEditState::from_command("echo hello");
        state.select(1);
        assert_eq!(state.edit_buffer, "hello");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "'hello'");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "\"hello\"");

        state.cycle_quote();
        assert_eq!(state.edit_buffer, "hello");
    }

    #[test]
    fn test_check_dangerous_command_detects_rm_rf_and_allows_safe() {
        assert!(check_dangerous_command("rm -rf /").is_some());
        assert!(check_dangerous_command("sudo rm -rf /tmp").is_some());
        assert!(check_dangerous_command("ls -la").is_none());
    }

    #[test]
    fn test_is_changed_after_edit_returns_true() {
        let mut state = CommandEditState::from_command("echo hello");
        assert!(!state.is_changed());

        state.select(1);
        state.edit_buffer = "world".to_string();
        assert!(state.is_changed());
    }

    #[test]
    fn test_revert_after_edit_restores_original_token() {
        let mut state = CommandEditState::from_command("echo hello");
        state.select(1);
        state.edit_buffer = "world".to_string();
        state.save_current_edit();

        state.revert();
        assert!(!state.is_changed());
        assert_eq!(state.tokens[1].text, "hello");
    }

    #[test]
    fn test_edit_config_for_history_enables_danger_check_and_quotes() {
        let config = EditConfig::for_history();
        assert!(config.danger_check);
        assert!(config.enable_quotes);
    }

    #[test]
    fn test_edit_config_for_file_disables_danger_check_and_quotes() {
        let config = EditConfig::for_file();
        assert!(!config.danger_check);
        assert!(!config.enable_quotes);
    }

    #[test]
    fn test_superscript_number_formats_digits_correctly() {
        assert_eq!(superscript_number(0), "⁰");
        assert_eq!(superscript_number(1), "¹");
        assert_eq!(superscript_number(10), "¹⁰");
        assert_eq!(superscript_number(20), "²⁰");
        assert_eq!(superscript_number(21), "²¹");
        assert_eq!(superscript_number(99), "⁹⁹");
        assert_eq!(superscript_number(123), "¹²³");
    }

    #[test]
    fn test_split_quotes_returns_text_and_style() {
        assert_eq!(
            split_quotes("hello"),
            ("hello".to_string(), QuoteStyle::None)
        );
        assert_eq!(
            split_quotes("'hello'"),
            ("hello".to_string(), QuoteStyle::Single)
        );
        assert_eq!(
            split_quotes("\"hello\""),
            ("hello".to_string(), QuoteStyle::Double)
        );
    }

    #[test]
    fn test_apply_quotes_wraps_text_with_style() {
        assert_eq!(apply_quotes("hello", QuoteStyle::None), "hello");
        assert_eq!(apply_quotes("hello", QuoteStyle::Single), "'hello'");
        assert_eq!(apply_quotes("hello", QuoteStyle::Double), "\"hello\"");
    }

    // ── Operator-aware tokenizer tests ──

    #[test]
    fn test_tokenize_redirect_splits_from_path() {
        let tokens = tokenize_command("echo hello >/dev/null");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].text, "echo");
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].text, "hello");
        assert_eq!(tokens[2].text, ">");
        assert_eq!(tokens[2].token_type, TokenType::Redirect);
        assert_eq!(tokens[3].text, "/dev/null");
        assert_eq!(tokens[3].token_type, TokenType::Path);
    }

    #[test]
    fn test_tokenize_pipe_classifies_next_as_command() {
        let tokens = tokenize_command("ls -la | grep test");
        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[2].text, "|");
        assert_eq!(tokens[2].token_type, TokenType::Pipe);
        assert_eq!(tokens[3].text, "grep");
        assert_eq!(tokens[3].token_type, TokenType::Command);
    }

    #[test]
    fn test_tokenize_and_operator_classifies_next_as_command() {
        let tokens = tokenize_command("mkdir foo && cd foo");
        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[2].text, "&&");
        assert_eq!(tokens[2].token_type, TokenType::Operator);
        assert_eq!(tokens[3].text, "cd");
        assert_eq!(tokens[3].token_type, TokenType::Command);
    }

    #[test]
    fn test_tokenize_fd_redirect() {
        let tokens = tokenize_command("cmd 2>/dev/null");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].text, "2>");
        assert_eq!(tokens[1].token_type, TokenType::Redirect);
        assert_eq!(tokens[2].text, "/dev/null");
    }

    #[test]
    fn test_tokenize_fd_redirect_with_ampersand() {
        let tokens = tokenize_command("cmd 2>&1");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[1].text, "2>&1");
        assert_eq!(tokens[1].token_type, TokenType::Redirect);
    }

    #[test]
    fn test_tokenize_newline_splits_tokens() {
        let tokens = tokenize_command("echo hello\necho world");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].text, "echo");
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[2].text, "echo");
        // After newline without operator, second "echo" is classified as Argument
        // (newline is just whitespace, not a structural operator like ; or &&)
    }

    #[test]
    fn test_tokenize_operators_inside_quotes_not_split() {
        let tokens = tokenize_command("echo '|&&||;'");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "echo");
        assert_eq!(tokens[1].text, "'|&&||;'");
        assert_eq!(tokens[1].token_type, TokenType::Argument);
    }

    #[test]
    fn test_tokenize_semicolon_operator() {
        let tokens = tokenize_command("echo a; echo b");
        assert_eq!(tokens.len(), 5);
        assert_eq!(tokens[2].text, ";");
        assert_eq!(tokens[2].token_type, TokenType::Operator);
        assert_eq!(tokens[3].text, "echo");
        assert_eq!(tokens[3].token_type, TokenType::Command);
    }

    #[test]
    fn test_tokenize_or_operator() {
        let tokens = tokenize_command("test -f file || echo missing");
        assert_eq!(tokens.len(), 6);
        assert_eq!(tokens[3].text, "||");
        assert_eq!(tokens[3].token_type, TokenType::Operator);
        assert_eq!(tokens[4].text, "echo");
        assert_eq!(tokens[4].token_type, TokenType::Command);
    }

    #[test]
    fn test_tokenize_background_ampersand() {
        let tokens = tokenize_command("sleep 10 &");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[2].text, "&");
        assert_eq!(tokens[2].token_type, TokenType::Operator);
    }

    #[test]
    fn test_tokenize_heredoc_marker_unterminated_degrades() {
        // Without a closing delimiter, heredoc marker degrades to Argument
        let tokens = tokenize_command("cat <<EOF");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].text, "cat");
        assert_eq!(tokens[1].text, "<<EOF");
        assert_eq!(tokens[1].token_type, TokenType::Argument);
    }

    #[test]
    fn test_tokenize_heredoc_marker_quoted_unterminated_degrades() {
        let tokens = tokenize_command("cat <<'EOF'");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[1].text, "<<'EOF'");
        assert_eq!(tokens[1].token_type, TokenType::Argument);
    }

    #[test]
    fn test_tokenize_heredoc_marker_with_dash_unterminated_degrades() {
        let tokens = tokenize_command("cat <<-EOF");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[1].text, "<<-EOF");
        assert_eq!(tokens[1].token_type, TokenType::Argument);
    }

    #[test]
    fn test_tokenize_here_string() {
        let tokens = tokenize_command("cat <<< 'hello'");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].text, "<<<");
        assert_eq!(tokens[1].token_type, TokenType::Redirect);
    }

    #[test]
    fn test_tokenize_append_redirect() {
        let tokens = tokenize_command("echo text >> file.log");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[2].text, ">>");
        assert_eq!(tokens[2].token_type, TokenType::Redirect);
    }

    #[test]
    fn test_tokenize_ampersand_redirect() {
        let tokens = tokenize_command("cmd &>/dev/null");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].text, "&>");
        assert_eq!(tokens[1].token_type, TokenType::Redirect);
        assert_eq!(tokens[2].text, "/dev/null");
    }

    #[test]
    fn test_tokenize_pipe_ampersand() {
        let tokens = tokenize_command("cmd1 |& cmd2");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].text, "|&");
        assert_eq!(tokens[1].token_type, TokenType::Pipe);
        assert_eq!(tokens[2].text, "cmd2");
        assert_eq!(tokens[2].token_type, TokenType::Command);
    }

    #[test]
    fn test_tokenize_complex_pipeline() {
        let tokens = tokenize_command("cat file | grep foo | wc -l");
        assert_eq!(tokens.len(), 8);
        assert_eq!(tokens[0].token_type, TokenType::Command); // cat
        assert_eq!(tokens[2].token_type, TokenType::Pipe); // |
        assert_eq!(tokens[3].token_type, TokenType::Command); // grep
        assert_eq!(tokens[5].token_type, TokenType::Pipe); // |
        assert_eq!(tokens[6].token_type, TokenType::Command); // wc
    }

    #[test]
    fn test_tokenize_backward_compatible_simple_commands() {
        // Ensure simple commands still tokenize exactly as before
        let tokens = tokenize_command("git commit -m 'test message'");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].token_type, TokenType::Subcommand);
        assert_eq!(tokens[2].token_type, TokenType::Flag);
        assert_eq!(tokens[3].text, "'test message'");
    }

    #[test]
    fn test_tokenize_redirect_input() {
        let tokens = tokenize_command("sort < input.txt");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].text, "<");
        assert_eq!(tokens[1].token_type, TokenType::Redirect);
    }

    #[test]
    fn test_tokenize_structural_tokens_are_locked() {
        let tokens = tokenize_command("echo hello | grep world > out.txt && cat out.txt");
        // Pipes, redirects, and operators should be locked
        for t in &tokens {
            if matches!(
                t.token_type,
                TokenType::Pipe | TokenType::Redirect | TokenType::Operator
            ) {
                assert!(t.locked, "Token '{}' should be locked", t.text);
            }
        }
    }

    // ── Heredoc body collection tests ──

    #[test]
    fn test_tokenize_heredoc_full_with_body() {
        let tokens = tokenize_command("cat <<'EOF'\nline1\nline2\nEOF");
        assert_eq!(tokens.len(), 5); // cat, <<'EOF', line1, line2, EOF
        assert_eq!(tokens[0].text, "cat");
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].text, "<<'EOF'");
        assert_eq!(tokens[1].token_type, TokenType::HeredocMarker);
        assert_eq!(tokens[2].text, "line1");
        assert_eq!(tokens[2].token_type, TokenType::HeredocBody);
        assert!(!tokens[2].locked); // body lines are editable
        assert_eq!(tokens[3].text, "line2");
        assert_eq!(tokens[3].token_type, TokenType::HeredocBody);
        assert_eq!(tokens[4].text, "EOF");
        assert_eq!(tokens[4].token_type, TokenType::HeredocDelimiter);
        // pair_index links marker(1) ↔ delimiter(4)
        assert_eq!(tokens[1].pair_index, Some(4));
        assert_eq!(tokens[4].pair_index, Some(1));
    }

    #[test]
    fn test_tokenize_heredoc_pair_index_links_marker_to_delimiter() {
        let tokens = tokenize_command("cat <<EOF\nhello\nEOF");
        let marker_idx = 1;
        let delim_idx = 3;
        assert_eq!(tokens[marker_idx].token_type, TokenType::HeredocMarker);
        assert_eq!(tokens[delim_idx].token_type, TokenType::HeredocDelimiter);
        assert_eq!(tokens[marker_idx].pair_index, Some(delim_idx));
        assert_eq!(tokens[delim_idx].pair_index, Some(marker_idx));
    }

    #[test]
    fn test_tokenize_heredoc_build_command_roundtrip() {
        let tokens = tokenize_command("cat <<'EOF'\nline1\nline2\nEOF");
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        let result = state.build_command();
        assert_eq!(result, "cat <<'EOF'\nline1\nline2\nEOF");
    }

    #[test]
    fn test_tokenize_heredoc_with_redirect() {
        // Real-world: tee file >/dev/null <<'EOF'\nbody\nEOF
        let tokens = tokenize_command("sudo tee /etc/file >/dev/null <<'EOF'\nTypes: deb\nEOF");
        // sudo, tee, /etc/file, >, /dev/null, <<'EOF', body, EOF
        assert!(
            tokens
                .iter()
                .any(|t| t.token_type == TokenType::HeredocMarker)
        );
        assert!(
            tokens
                .iter()
                .any(|t| t.token_type == TokenType::HeredocBody)
        );
        assert!(
            tokens
                .iter()
                .any(|t| t.token_type == TokenType::HeredocDelimiter)
        );
        assert!(tokens.iter().any(|t| t.token_type == TokenType::Redirect));
    }

    #[test]
    fn test_tokenize_heredoc_empty_body() {
        let tokens = tokenize_command("cat <<EOF\nEOF");
        // cat, <<EOF, EOF (no body token for empty body)
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[1].token_type, TokenType::HeredocMarker);
        assert_eq!(tokens[2].token_type, TokenType::HeredocDelimiter);
        assert_eq!(tokens[2].text, "EOF");
    }

    #[test]
    fn test_extract_heredoc_delimiter_variants() {
        assert_eq!(extract_heredoc_delimiter("<<EOF"), Some("EOF".to_string()));
        assert_eq!(
            extract_heredoc_delimiter("<<'EOF'"),
            Some("EOF".to_string())
        );
        assert_eq!(
            extract_heredoc_delimiter("<<\"EOF\""),
            Some("EOF".to_string())
        );
        assert_eq!(extract_heredoc_delimiter("<<-EOF"), Some("EOF".to_string()));
        assert_eq!(
            extract_heredoc_delimiter("<<-'HEREDOC'"),
            Some("HEREDOC".to_string())
        );
        assert_eq!(extract_heredoc_delimiter("<<"), None);
    }

    #[test]
    fn test_rebuild_heredoc_marker() {
        assert_eq!(rebuild_heredoc_marker("<<'EOF'", "END"), "<<'END'");
        assert_eq!(rebuild_heredoc_marker("<<EOF", "END"), "<<END");
        assert_eq!(rebuild_heredoc_marker("<<-'EOF'", "END"), "<<-'END'");
        assert_eq!(rebuild_heredoc_marker("<<\"EOF\"", "END"), "<<\"END\"");
    }

    // ── Phase 5: Navigation + pair sync tests ──

    #[test]
    fn test_navigation_skips_structural_locked_tokens() {
        let tokens = tokenize_command("echo hello | grep world");
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        // Start at "echo" (index 0)
        assert_eq!(state.selected_token().unwrap().text, "echo");
        state.next(); // Should skip to "hello" (index 1)
        assert_eq!(state.selected_token().unwrap().text, "hello");
        state.next(); // Should skip pipe (index 2, locked) → "grep" (index 3)
        assert_eq!(state.selected_token().unwrap().text, "grep");
        state.next(); // → "world" (index 4)
        assert_eq!(state.selected_token().unwrap().text, "world");
        state.prev(); // Should skip pipe → "grep"... wait, going backward from 4 → 3 is grep
        assert_eq!(state.selected_token().unwrap().text, "grep");
        state.prev(); // → "hello" (skip pipe)
        assert_eq!(state.selected_token().unwrap().text, "hello");
    }

    #[test]
    fn test_navigation_visits_each_heredoc_body_line() {
        let tokens = tokenize_command("cat <<EOF\nline1\nline2\nline3\nEOF");
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        assert_eq!(state.selected_token().unwrap().text, "cat");
        state.next(); // → <<EOF (HeredocMarker, editable)
        assert_eq!(state.selected_token().unwrap().text, "<<EOF");
        assert_eq!(
            state.selected_token().unwrap().token_type,
            TokenType::HeredocMarker
        );
        state.next(); // → line1 (body line 1)
        assert_eq!(state.selected_token().unwrap().text, "line1");
        assert_eq!(
            state.selected_token().unwrap().token_type,
            TokenType::HeredocBody
        );
        state.next(); // → line2 (body line 2)
        assert_eq!(state.selected_token().unwrap().text, "line2");
        state.next(); // → line3 (body line 3)
        assert_eq!(state.selected_token().unwrap().text, "line3");
        state.next(); // → EOF delimiter
        assert_eq!(state.selected_token().unwrap().text, "EOF");
        assert_eq!(
            state.selected_token().unwrap().token_type,
            TokenType::HeredocDelimiter
        );
    }

    #[test]
    fn test_heredoc_pair_sync_marker_to_delimiter() {
        let tokens = tokenize_command("cat <<'EOF'\nhello\nEOF");
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        // Select the marker token (index 1)
        state.select(1);
        assert_eq!(state.edit_buffer, "<<'EOF'");
        // Edit the marker to use a different delimiter
        state.edit_buffer = "<<'HEREDOC'".to_string();
        state.save_current_edit();
        // The closing delimiter should be updated
        let delim = state
            .tokens
            .iter()
            .find(|t| t.token_type == TokenType::HeredocDelimiter)
            .unwrap();
        assert_eq!(delim.text, "HEREDOC");
    }

    #[test]
    fn test_heredoc_pair_sync_delimiter_to_marker() {
        let tokens = tokenize_command("cat <<'EOF'\nhello\nEOF");
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        // Find the delimiter token index
        let delim_idx = state
            .tokens
            .iter()
            .position(|t| t.token_type == TokenType::HeredocDelimiter)
            .unwrap();
        state.select(delim_idx);
        assert_eq!(state.edit_buffer, "EOF");
        // Edit the delimiter
        state.edit_buffer = "END".to_string();
        state.save_current_edit();
        // The marker should be updated
        let marker = state
            .tokens
            .iter()
            .find(|t| t.token_type == TokenType::HeredocMarker)
            .unwrap();
        assert_eq!(marker.text, "<<'END'");
    }

    #[test]
    fn test_delete_pipe_reclassifies_tokens() {
        let tokens = tokenize_command("echo hello | grep world");
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        // "grep" at index 3 should be Command (after pipe)
        assert_eq!(state.tokens[3].token_type, TokenType::Command);
        // Delete the pipe token (index 2) — need to select it first
        // The pipe is locked, so delete_token won't work. This is by design:
        // structural tokens protect the command structure. Users remove them
        // via other editing operations, not direct deletion.
        // Instead, test that reclassify works if we manually remove it.
        state.tokens.remove(2); // Remove the pipe
        state.reclassify_tokens();
        // "grep" should now be Argument (no longer after a pipe)
        assert_eq!(state.tokens[2].text, "grep");
        assert_eq!(state.tokens[2].token_type, TokenType::Argument);
    }

    #[test]
    fn test_complex_heredoc_roundtrip() {
        // The original motivating case from the bug report
        let cmd = "sudo tee /etc/apt/sources.list.d/vscode.sources >/dev/null <<'EOF'\nTypes: deb\nURIs: https://packages.microsoft.com/repos/code\nSuites: stable\nComponents: main\nEOF";
        let tokens = tokenize_command(cmd);

        // Verify key token types
        assert!(
            tokens
                .iter()
                .any(|t| t.text == "sudo" && t.token_type == TokenType::Command)
        );
        assert!(
            tokens
                .iter()
                .any(|t| t.text == ">" && t.token_type == TokenType::Redirect)
        );
        assert!(
            tokens
                .iter()
                .any(|t| t.text == "<<'EOF'" && t.token_type == TokenType::HeredocMarker)
        );
        assert!(
            tokens
                .iter()
                .any(|t| t.token_type == TokenType::HeredocBody)
        );
        assert!(
            tokens
                .iter()
                .any(|t| t.text == "EOF" && t.token_type == TokenType::HeredocDelimiter)
        );

        // Round-trip through build_command — note: `>/dev/null` becomes `> /dev/null`
        // (semantically identical shell syntax, operators are now separate tokens)
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        let result = state.build_command();
        let expected = "sudo tee /etc/apt/sources.list.d/vscode.sources > /dev/null <<'EOF'\nTypes: deb\nURIs: https://packages.microsoft.com/repos/code\nSuites: stable\nComponents: main\nEOF";
        assert_eq!(result, expected);
    }

    #[test]
    fn test_toggle_lock_prevents_editing() {
        let tokens = tokenize_command("ls -la /tmp");
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        state.select(1); // select -la
        assert!(!state.is_selected_locked());
        state.toggle_lock();
        assert!(state.is_selected_locked());
        // Typing should be a no-op on locked token
        state.type_char('x');
        assert_eq!(state.edit_buffer, "-la"); // unchanged
        // Unlock
        state.toggle_lock();
        assert!(!state.is_selected_locked());
        state.type_char('x');
        assert_eq!(state.edit_buffer, "-lax"); // now editable
    }

    #[test]
    fn test_heredoc_per_line_body_build_roundtrip() {
        let cmd = "cat <<EOF\nalpha\nbeta\ngamma\nEOF";
        let tokens = tokenize_command(cmd);
        // 6 tokens: cat, <<EOF, alpha, beta, gamma, EOF
        assert_eq!(tokens.len(), 6);
        assert_eq!(tokens[2].token_type, TokenType::HeredocBody);
        assert_eq!(tokens[3].token_type, TokenType::HeredocBody);
        assert_eq!(tokens[4].token_type, TokenType::HeredocBody);
        let mut state = CommandEditState::new(tokens, EditConfig::default());
        assert_eq!(state.build_command(), cmd);
    }
}
