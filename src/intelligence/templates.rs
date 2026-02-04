//! Template recognition and completion.
//!
//! Extracts command templates with placeholders and provides
//! template-based completions.

use std::collections::HashMap;

use regex::Regex;
use rusqlite::Connection;
use tracing::debug;

use super::error::CIError;
use super::tokenizer::{analyze_command, compute_command_hash};
use super::types::{
    Placeholder, PlaceholderType, SuggestionContext, Template, TemplateCompletion,
};

/// Extracts a template from a command.
///
/// Replaces variable parts with typed placeholders.
pub fn extract_template(conn: &Connection, command: &str) -> Result<Option<Template>, CIError> {
    let tokens = analyze_command(command);
    if tokens.is_empty() {
        return Ok(None);
    }

    let mut template_parts = Vec::new();
    let mut placeholders = Vec::new();
    let mut placeholder_count = 0;

    // Build context array for context-aware placeholder detection
    let token_texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();

    for (i, token) in tokens.iter().enumerate() {
        // Use context from preceding tokens for context-aware extraction
        let context = &token_texts[..i];
        let (template_part, placeholder) = extract_placeholder_with_context(&token.text, i, context);

        if let Some(ph) = placeholder {
            placeholders.push(ph);
            placeholder_count += 1;
        }

        template_parts.push(template_part);
    }

    // Only consider it a template if it has placeholders
    if placeholder_count == 0 {
        return Ok(None);
    }

    let pattern = template_parts.join(" ");
    let template_hash = compute_command_hash(&pattern);
    let base_command = tokens.first().map(|t| t.text.clone()).unwrap_or_default();

    // Get base command ID
    let base_command_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [&base_command],
            |row| row.get(0),
        )
        .ok();

    let now = chrono::Utc::now().timestamp();
    let placeholders_json = serde_json::to_string(&placeholders)?;

    // Insert or update template
    conn.execute(
        "INSERT INTO ci_templates
         (template, template_hash, base_command_id, placeholder_count, placeholders, frequency, last_seen, example_command)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7)
         ON CONFLICT(template)
         DO UPDATE SET frequency = frequency + 1, last_seen = ?6",
        rusqlite::params![
            pattern,
            template_hash,
            base_command_id,
            placeholder_count,
            placeholders_json,
            now,
            command,
        ],
    )?;

    // Query the actual template data including the stored frequency
    let (template_id, frequency): (i64, u32) = conn.query_row(
        "SELECT id, frequency FROM ci_templates WHERE template = ?1",
        [&pattern],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    // Learn template values
    learn_template_values(conn, template_id, &tokens, &placeholders, now)?;

    debug!(template = %pattern, placeholders = placeholder_count, frequency = frequency, "Extracted template");

    Ok(Some(Template {
        id: template_id,
        pattern,
        base_command,
        placeholders,
        frequency,
    }))
}

/// Extracts a placeholder from a token if it matches a pattern.
///
/// # Arguments
///
/// * `text` - The token text to analyze
/// * `position` - Position in the command
/// * `context` - Optional context tokens (previous tokens) for contextual detection
fn extract_placeholder(text: &str, position: usize) -> (String, Option<Placeholder>) {
    extract_placeholder_with_context(text, position, &[])
}

/// Extracts a placeholder from a token with context awareness.
///
/// Uses surrounding tokens to detect context-specific placeholders like branch names.
fn extract_placeholder_with_context(
    text: &str,
    position: usize,
    context: &[&str],
) -> (String, Option<Placeholder>) {
    // Port mapping pattern: 8080:8080
    if Regex::new(r"^\d+:\d+$").ok().and_then(|r| r.captures(text)).is_some() {
        return (
            "<PORT>:<PORT>".to_string(),
            Some(Placeholder {
                name: "PORT".to_string(),
                placeholder_type: PlaceholderType::Port,
                position,
            }),
        );
    }

    // Single port pattern: 8080 (only if it looks like a port)
    if Regex::new(r"^\d{2,5}$").ok().and_then(|r| r.captures(text)).is_some() {
        if let Ok(n) = text.parse::<u32>() {
            if (1..=65535).contains(&n) {
                // Check if context suggests this is a port (after -p flag)
                let is_port_context = context.last().map(|t| *t == "-p" || *t == "--port").unwrap_or(false);
                if is_port_context || n >= 1024 {
                    return (
                        "<PORT>".to_string(),
                        Some(Placeholder {
                            name: "PORT".to_string(),
                            placeholder_type: PlaceholderType::Port,
                            position,
                        }),
                    );
                }
            }
        }
    }

    // Generic number pattern: any purely numeric value
    if Regex::new(r"^\d+$").ok().and_then(|r| r.captures(text)).is_some() {
        return (
            "<NUMBER>".to_string(),
            Some(Placeholder {
                name: "NUMBER".to_string(),
                placeholder_type: PlaceholderType::Number,
                position,
            }),
        );
    }

    // Branch detection: after git checkout/switch/branch commands
    if is_branch_context(context) && looks_like_branch_name(text) {
        return (
            "<BRANCH>".to_string(),
            Some(Placeholder {
                name: "BRANCH".to_string(),
                placeholder_type: PlaceholderType::Branch,
                position,
            }),
        );
    }

    // Docker image pattern: registry/name:tag or name:tag
    // Match either full registry/name:tag format OR simple name:tag format
    if Regex::new(r"^([\w\-\.]+/)?[\w\-\.]+:[\w\-\.]+$")
        .ok()
        .and_then(|r| r.captures(text)).is_some()
    {
        return (
            "<IMAGE>".to_string(),
            Some(Placeholder {
                name: "IMAGE".to_string(),
                placeholder_type: PlaceholderType::Image,
                position,
            }),
        );
    }

    // URL pattern
    if text.contains("://") {
        return (
            "<URL>".to_string(),
            Some(Placeholder {
                name: "URL".to_string(),
                placeholder_type: PlaceholderType::Url,
                position,
            }),
        );
    }

    // Git URL pattern
    if text.starts_with("git@") {
        return (
            "<URL>".to_string(),
            Some(Placeholder {
                name: "URL".to_string(),
                placeholder_type: PlaceholderType::Url,
                position,
            }),
        );
    }

    // Path pattern (but not flags)
    if (text.contains('/') || text.starts_with('.') || text.starts_with('~'))
        && !text.starts_with('-')
    {
        return (
            "<PATH>".to_string(),
            Some(Placeholder {
                name: "PATH".to_string(),
                placeholder_type: PlaceholderType::Path,
                position,
            }),
        );
    }

    // Quoted string pattern
    if (text.starts_with('"') && text.ends_with('"'))
        || (text.starts_with('\'') && text.ends_with('\''))
    {
        return (
            "<QUOTED>".to_string(),
            Some(Placeholder {
                name: "QUOTED".to_string(),
                placeholder_type: PlaceholderType::Quoted,
                position,
            }),
        );
    }

    // Keep the token as-is (no placeholder)
    (text.to_string(), None)
}

/// Checks if the context indicates a branch name is expected.
fn is_branch_context(context: &[&str]) -> bool {
    if context.is_empty() {
        return false;
    }

    // Check for git branch commands
    let has_git = context.first().map(|t| *t == "git").unwrap_or(false);
    if !has_git {
        return false;
    }

    // Git subcommands that take branch names
    let branch_commands = ["checkout", "switch", "branch", "merge", "rebase", "cherry-pick"];

    // Check if any of the branch commands appear in context
    context.iter().any(|t| branch_commands.contains(t))
}

/// Checks if a token looks like a git branch name.
fn looks_like_branch_name(text: &str) -> bool {
    // Skip if it looks like a flag
    if text.starts_with('-') {
        return false;
    }

    // Skip if it looks like a path
    if text.contains('/') && !text.starts_with("origin/") && !text.starts_with("feature/")
        && !text.starts_with("bugfix/") && !text.starts_with("release/") && !text.starts_with("hotfix/") {
        return false;
    }

    // Skip if it looks like a commit hash (all hex and >= 7 chars)
    if text.len() >= 7 && text.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }

    // Common branch name patterns
    let branch_patterns = [
        "main", "master", "develop", "development", "staging", "production",
    ];

    // Check if it's a common branch name or looks like one
    if branch_patterns.contains(&text) {
        return true;
    }

    // Check for prefixed branch names (feature/xxx, bugfix/xxx, etc.)
    if text.starts_with("origin/")
        || text.starts_with("feature/")
        || text.starts_with("bugfix/")
        || text.starts_with("release/")
        || text.starts_with("hotfix/")
    {
        return true;
    }

    // Check for typical branch name format (alphanumeric with dashes/underscores)
    let valid_branch = Regex::new(r"^[a-zA-Z][a-zA-Z0-9\-_]*$")
        .ok()
        .map(|r| r.is_match(text))
        .unwrap_or(false);

    valid_branch && text.len() >= 2
}

/// Learns values for template placeholders.
fn learn_template_values(
    conn: &Connection,
    template_id: i64,
    tokens: &[super::types::AnalyzedToken],
    placeholders: &[Placeholder],
    timestamp: i64,
) -> Result<(), CIError> {
    for placeholder in placeholders {
        if placeholder.position >= tokens.len() {
            continue;
        }

        let value = &tokens[placeholder.position].text;
        let value_type = format!("{:?}", placeholder.placeholder_type);

        conn.execute(
            "INSERT INTO ci_template_values
             (template_id, placeholder_name, value_text, value_type, frequency, last_seen)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(template_id, placeholder_name, value_text)
             DO UPDATE SET frequency = frequency + 1, last_seen = ?5",
            rusqlite::params![template_id, placeholder.name, value, value_type, timestamp],
        )?;
    }

    Ok(())
}

/// Gets template completions for the given context.
pub fn suggest_templates(conn: &Connection, context: &SuggestionContext) -> Vec<TemplateCompletion> {
    let partial = &context.partial;
    if partial.is_empty() && context.preceding_tokens.is_empty() {
        return Vec::new();
    }

    // Build a partial pattern to match
    let partial_pattern = if context.preceding_tokens.is_empty() {
        format!("{}%", partial)
    } else {
        let prefix: String = context
            .preceding_tokens
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} {}%", prefix, partial)
    };

    // Query matching templates
    let mut stmt = match conn.prepare(
        "SELECT id, template, base_command_id, placeholders, frequency, example_command
         FROM ci_templates
         WHERE template LIKE ?1
         ORDER BY frequency DESC
         LIMIT 10"
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };

    let rows = match stmt.query_map([&partial_pattern], |row| {
        let id: i64 = row.get(0)?;
        let pattern: String = row.get(1)?;
        let base_id: Option<i64> = row.get(2)?;
        let placeholders_json: String = row.get(3)?;
        let frequency: u32 = row.get(4)?;
        let example: Option<String> = row.get(5)?;
        Ok((id, pattern, base_id, placeholders_json, frequency, example))
    }) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    let mut completions = Vec::new();

    for row in rows.flatten() {
        let (id, pattern, _base_id, placeholders_json, frequency, example) = row;

        let placeholders: Vec<Placeholder> = serde_json::from_str(&placeholders_json)
            .unwrap_or_default();

        // Get most common values for each placeholder
        let filled_values = get_common_placeholder_values(conn, id, &placeholders);

        // Build preview
        let preview = if let Some(ex) = example {
            ex
        } else {
            build_preview(&pattern, &filled_values)
        };

        let base_command = pattern
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();

        completions.push(TemplateCompletion {
            template: Template {
                id,
                pattern,
                base_command,
                placeholders,
                frequency,
            },
            filled_values,
            preview,
            confidence: frequency as f64 / 100.0,
        });
    }

    completions
}

/// Gets the most common values for template placeholders.
fn get_common_placeholder_values(
    conn: &Connection,
    template_id: i64,
    placeholders: &[Placeholder],
) -> HashMap<String, String> {
    let mut values = HashMap::new();

    for placeholder in placeholders {
        let value: Option<String> = conn
            .query_row(
                "SELECT value_text FROM ci_template_values
                 WHERE template_id = ?1 AND placeholder_name = ?2
                 ORDER BY frequency DESC
                 LIMIT 1",
                rusqlite::params![template_id, placeholder.name],
                |row| row.get(0),
            )
            .ok();

        if let Some(v) = value {
            values.insert(placeholder.name.clone(), v);
        } else {
            // Use placeholder marker as default
            values.insert(
                placeholder.name.clone(),
                placeholder.placeholder_type.marker().to_string(),
            );
        }
    }

    values
}

/// Builds a preview by filling placeholders with values.
fn build_preview(pattern: &str, values: &HashMap<String, String>) -> String {
    let mut result = pattern.to_string();

    for (name, value) in values {
        let placeholder = format!("<{}>", name);
        result = result.replace(&placeholder, value);
    }

    result
}

/// Gets all values for a specific placeholder.
pub fn get_placeholder_values(
    conn: &Connection,
    template_id: i64,
    placeholder_name: &str,
    limit: usize,
) -> Result<Vec<(String, u32)>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT value_text, frequency
         FROM ci_template_values
         WHERE template_id = ?1 AND placeholder_name = ?2
         ORDER BY frequency DESC
         LIMIT ?3"
    )?;

    let rows = stmt.query_map(
        rusqlite::params![template_id, placeholder_name, limit],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::db_schema;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db_schema::create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_extract_placeholder() {
        let (text, ph) = extract_placeholder("8080:8080", 0);
        assert_eq!(text, "<PORT>:<PORT>");
        assert!(ph.is_some());
        assert_eq!(ph.unwrap().placeholder_type, PlaceholderType::Port);

        let (text, ph) = extract_placeholder("/path/to/file", 0);
        assert_eq!(text, "<PATH>");
        assert!(ph.is_some());

        let (text, ph) = extract_placeholder("git", 0);
        assert_eq!(text, "git");
        assert!(ph.is_none());
    }

    #[test]
    fn test_extract_placeholder_number() {
        // Simple number should be detected as NUMBER
        let (text, ph) = extract_placeholder("42", 0);
        assert_eq!(text, "<NUMBER>");
        assert!(ph.is_some());
        assert_eq!(ph.unwrap().placeholder_type, PlaceholderType::Number);

        // Large numbers should also work
        let (text, ph) = extract_placeholder("123456", 0);
        assert_eq!(text, "<NUMBER>");
        assert!(ph.is_some());
    }

    #[test]
    fn test_extract_placeholder_branch_context() {
        // Branch name after git checkout should be detected
        let context = ["git", "checkout"];
        let (text, ph) = extract_placeholder_with_context("feature-branch", 2, &context);
        assert_eq!(text, "<BRANCH>");
        assert!(ph.is_some());
        assert_eq!(ph.unwrap().placeholder_type, PlaceholderType::Branch);

        // Branch name after git switch
        let context = ["git", "switch"];
        let (text, ph) = extract_placeholder_with_context("main", 2, &context);
        assert_eq!(text, "<BRANCH>");
        assert!(ph.is_some());

        // Branch name after git branch -d
        let context = ["git", "branch", "-d"];
        let (text, ph) = extract_placeholder_with_context("old-branch", 3, &context);
        assert_eq!(text, "<BRANCH>");
        assert!(ph.is_some());

        // Origin-prefixed branch
        let context = ["git", "checkout"];
        let (text, ph) = extract_placeholder_with_context("origin/main", 2, &context);
        assert_eq!(text, "<BRANCH>");
        assert!(ph.is_some());

        // Feature branch prefix
        let context = ["git", "checkout"];
        let (text, ph) = extract_placeholder_with_context("feature/new-feature", 2, &context);
        assert_eq!(text, "<BRANCH>");
        assert!(ph.is_some());
    }

    #[test]
    fn test_extract_placeholder_no_branch_without_context() {
        // Without git context, should not be detected as branch
        let (text, ph) = extract_placeholder("feature-branch", 0);
        // Could be a generic token, not specifically a branch
        assert!(text != "<BRANCH>" || ph.is_none() || ph.as_ref().unwrap().placeholder_type != PlaceholderType::Branch);
    }

    #[test]
    fn test_template_frequency_persisted() {
        let conn = setup_test_db();

        // Create base command token
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('docker', 'Command', ?1, ?1)",
            [now],
        ).unwrap();

        // Extract template first time
        let template1 = extract_template(&conn, "docker run -p 8080:8080 nginx:latest").unwrap();
        assert!(template1.is_some());
        assert_eq!(template1.as_ref().unwrap().frequency, 1);

        // Extract same template again
        let template2 = extract_template(&conn, "docker run -p 9090:9090 redis:alpine").unwrap();
        assert!(template2.is_some());
        // Frequency should be incremented and returned
        assert_eq!(template2.unwrap().frequency, 2);

        // Extract again
        let template3 = extract_template(&conn, "docker run -p 3000:3000 node:18").unwrap();
        assert!(template3.is_some());
        assert_eq!(template3.unwrap().frequency, 3);
    }

    #[test]
    fn test_extract_template() {
        let conn = setup_test_db();

        // Create base command token
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('docker', 'Command', ?1, ?1)",
            [now],
        ).unwrap();

        let template = extract_template(&conn, "docker run -p 8080:8080 nginx:latest").unwrap();
        assert!(template.is_some());

        let t = template.unwrap();
        assert!(t.pattern.contains("<PORT>"));
        assert!(t.pattern.contains("<IMAGE>"));
        assert!(!t.placeholders.is_empty());
    }

    #[test]
    fn test_build_preview() {
        let pattern = "docker run -p <PORT>:<PORT> <IMAGE>";
        let mut values = HashMap::new();
        values.insert("PORT".to_string(), "8080".to_string());
        values.insert("IMAGE".to_string(), "nginx".to_string());

        let preview = build_preview(pattern, &values);
        assert_eq!(preview, "docker run -p 8080:8080 nginx");
    }
}
