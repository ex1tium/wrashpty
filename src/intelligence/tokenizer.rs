//! Enhanced token analysis for the Command Intelligence Engine.

use crate::chrome::command_edit::{TokenType, tokenize_command};

use super::types::{AnalyzedToken, PositionType};

/// Analyzes a command string and returns classified tokens.
///
/// This builds on the existing `tokenize_command()` function but provides
/// additional analysis for the intelligence system.
pub fn analyze_command(command: &str) -> Vec<AnalyzedToken> {
    let tokens = tokenize_command(command);

    tokens
        .into_iter()
        .enumerate()
        .map(|(i, token)| AnalyzedToken {
            text: token.text,
            token_type: token.token_type,
            position: i,
        })
        .collect()
}

/// Determines the position type based on context.
///
/// This is used to provide specialized suggestions based on where
/// in the command the user is currently editing.
pub fn determine_position_type(
    preceding_tokens: &[AnalyzedToken],
    base_command: Option<&str>,
) -> PositionType {
    if preceding_tokens.is_empty() {
        return PositionType::Command;
    }

    let last_token = &preceding_tokens[preceding_tokens.len() - 1];

    // Check for pipe - after pipe we suggest pipeable commands
    if last_token.text == "|" || last_token.text.ends_with('|') {
        return PositionType::AfterPipe;
    }

    // Check for redirect
    if last_token.text == ">" || last_token.text == ">>" || last_token.text == "<" {
        return PositionType::AfterRedirect;
    }

    // Check for flag that expects a value
    if last_token.token_type == TokenType::Flag {
        // Common flags that expect values
        let flag = &last_token.text;
        if flag_expects_value(flag, base_command) {
            return PositionType::FlagValue { flag: flag.clone() };
        }
    }

    // Check for subcommand position
    if preceding_tokens.len() == 1 {
        if let Some(cmd) = base_command {
            if is_compound_command(cmd) {
                return PositionType::Subcommand;
            }
        }
    }

    PositionType::Argument
}

/// Returns true if the flag typically expects a value.
///
/// This is the canonical implementation - used by both tokenizer and types.
/// Includes command-specific flag knowledge.
pub fn flag_expects_value(flag: &str, base_command: Option<&str>) -> bool {
    // Common flags that always expect values
    let common_value_flags = [
        "-o",
        "-f",
        "-i",
        "-c",
        "-n",
        "-m",
        "-p",
        "-t",
        "-u",
        "-d",
        "--output",
        "--file",
        "--input",
        "--config",
        "--name",
        "--message",
        "--port",
        "--target",
        "--user",
        "--directory",
        "--format",
        "--filter",
        "--branch",
        "--remote",
    ];

    if common_value_flags.contains(&flag) {
        return true;
    }

    // Command-specific flags
    match base_command {
        Some("docker") | Some("podman") => matches!(
            flag,
            "-p" | "--port"
                | "-v"
                | "--volume"
                | "-e"
                | "--env"
                | "-w"
                | "--workdir"
                | "--name"
                | "-t"
                | "--tag"
                | "--network"
        ),
        Some("git") => matches!(
            flag,
            "-m" | "--message" | "-b" | "--branch" | "-C" | "--config"
        ),
        Some("cargo") => matches!(flag, "-p" | "--package" | "--target" | "--features"),
        Some("kubectl") => matches!(flag, "-n" | "--namespace" | "-l" | "--selector" | "-f"),
        _ => false,
    }
}

/// Returns true if the command has subcommands.
///
/// This is the canonical implementation - used by both tokenizer and command_edit.
pub fn is_compound_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "git"
            | "docker"
            | "kubectl"
            | "cargo"
            | "npm"
            | "yarn"
            | "pnpm"
            | "systemctl"
            | "journalctl"
            | "apt"
            | "brew"
            | "pacman"
            | "pip"
            | "pipx"
            | "poetry"
            | "go"
            | "rustup"
            | "podman"
            | "dnf"
            | "yum"
    )
}

/// Detects the value type for a token based on content patterns.
pub fn detect_value_type(value: &str) -> Option<&'static str> {
    // Port pattern: single port or port:port
    if value.chars().all(|c| c.is_ascii_digit() || c == ':') {
        if value.contains(':') {
            return Some("port_mapping");
        }
        if let Ok(n) = value.parse::<u32>() {
            if (1..=65535).contains(&n) {
                return Some("port");
            }
            return Some("number");
        }
    }

    // URL patterns (check before path since URLs contain '/')
    if value.contains("://") {
        return Some("url");
    }

    // Git URL pattern
    if value.starts_with("git@") {
        return Some("git_url");
    }

    // Docker image pattern: name:tag or registry/name:tag
    if value.contains('/') && value.contains(':') {
        return Some("image");
    }

    // Path patterns
    if value.contains('/') || value.starts_with('.') || value.starts_with('~') {
        return Some("path");
    }

    // Image with tag but no registry (e.g., "nginx:latest")
    if value.contains(':') && !value.contains(' ') {
        return Some("image");
    }

    None
}

/// Extracts the base command from tokens.
pub fn extract_base_command(tokens: &[AnalyzedToken]) -> Option<&str> {
    tokens.first().map(|t| t.text.as_str())
}

/// Extracts the subcommand from tokens if present.
pub fn extract_subcommand(tokens: &[AnalyzedToken]) -> Option<&str> {
    if tokens.len() < 2 {
        return None;
    }

    let base_cmd = tokens.first()?;
    if is_compound_command(&base_cmd.text) {
        let second = &tokens[1];
        if second.token_type == TokenType::Subcommand {
            return Some(&second.text);
        }
    }

    None
}

/// Finds pipe positions in a command.
pub fn find_pipe_positions(tokens: &[AnalyzedToken]) -> Vec<usize> {
    tokens
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            if t.text == "|" || t.text.ends_with('|') {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

/// Splits a command at pipe positions.
pub fn split_at_pipes(tokens: &[AnalyzedToken]) -> Vec<Vec<AnalyzedToken>> {
    let mut segments = Vec::new();
    let mut current_segment = Vec::new();

    for token in tokens {
        if token.text == "|" {
            if !current_segment.is_empty() {
                segments.push(current_segment);
                current_segment = Vec::new();
            }
        } else if token.text.ends_with('|') {
            // Token like "foo|" - split it
            let text_without_pipe = token.text.trim_end_matches('|');
            if !text_without_pipe.is_empty() {
                current_segment.push(AnalyzedToken {
                    text: text_without_pipe.to_string(),
                    token_type: token.token_type,
                    position: token.position,
                });
            }
            if !current_segment.is_empty() {
                segments.push(current_segment);
                current_segment = Vec::new();
            }
        } else {
            current_segment.push(token.clone());
        }
    }

    if !current_segment.is_empty() {
        segments.push(current_segment);
    }

    segments
}

/// Computes a hash for a command (for deduplication and pattern matching).
pub fn compute_command_hash(command: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    command.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Computes a hash for a token sequence (for n-gram patterns).
pub fn compute_token_hash(tokens: &[&str]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    tokens.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Converts a TokenType to a string for database storage.
pub fn token_type_to_string(token_type: TokenType) -> &'static str {
    match token_type {
        TokenType::Command => "Command",
        TokenType::Subcommand => "Subcommand",
        TokenType::Flag => "Flag",
        TokenType::Path => "Path",
        TokenType::Url => "Url",
        TokenType::Argument => "Argument",
        TokenType::Locked => "Locked",
    }
}

/// Parses a TokenType from a string.
pub fn token_type_from_string(s: &str) -> TokenType {
    match s {
        "Command" => TokenType::Command,
        "Subcommand" => TokenType::Subcommand,
        "Flag" => TokenType::Flag,
        "Path" => TokenType::Path,
        "Url" => TokenType::Url,
        "Argument" => TokenType::Argument,
        "Locked" => TokenType::Locked,
        _ => TokenType::Argument,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_command() {
        let tokens = analyze_command("git commit -m 'test'");
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].text, "git");
        assert_eq!(tokens[0].token_type, TokenType::Command);
        assert_eq!(tokens[1].text, "commit");
        assert_eq!(tokens[1].token_type, TokenType::Subcommand);
        assert_eq!(tokens[2].text, "-m");
        assert_eq!(tokens[2].token_type, TokenType::Flag);
    }

    #[test]
    fn test_determine_position_type_command() {
        let pos = determine_position_type(&[], None);
        assert_eq!(pos, PositionType::Command);
    }

    #[test]
    fn test_determine_position_type_after_pipe() {
        let tokens = vec![
            AnalyzedToken::new("cat", TokenType::Command, 0),
            AnalyzedToken::new("file.txt", TokenType::Path, 1),
            AnalyzedToken::new("|", TokenType::Argument, 2),
        ];
        let pos = determine_position_type(&tokens, Some("cat"));
        assert_eq!(pos, PositionType::AfterPipe);
    }

    #[test]
    fn test_determine_position_type_flag_value() {
        let tokens = vec![
            AnalyzedToken::new("docker", TokenType::Command, 0),
            AnalyzedToken::new("run", TokenType::Subcommand, 1),
            AnalyzedToken::new("-p", TokenType::Flag, 2),
        ];
        let pos = determine_position_type(&tokens, Some("docker"));
        assert!(matches!(pos, PositionType::FlagValue { .. }));
    }

    #[test]
    fn test_detect_value_type() {
        assert_eq!(detect_value_type("8080"), Some("port"));
        assert_eq!(detect_value_type("8080:80"), Some("port_mapping"));
        assert_eq!(detect_value_type("/path/to/file"), Some("path"));
        assert_eq!(detect_value_type("https://example.com"), Some("url"));
        assert_eq!(
            detect_value_type("git@github.com:user/repo"),
            Some("git_url")
        );
    }

    #[test]
    fn test_split_at_pipes() {
        let tokens = analyze_command("cat file.txt | grep test | wc -l");
        let segments = split_at_pipes(&tokens);
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0][0].text, "cat");
        assert_eq!(segments[1][0].text, "grep");
        assert_eq!(segments[2][0].text, "wc");
    }

    #[test]
    fn test_token_type_roundtrip() {
        for token_type in [
            TokenType::Command,
            TokenType::Subcommand,
            TokenType::Flag,
            TokenType::Path,
            TokenType::Url,
            TokenType::Argument,
        ] {
            let s = token_type_to_string(token_type);
            let parsed = token_type_from_string(s);
            assert_eq!(token_type, parsed);
        }
    }
}
