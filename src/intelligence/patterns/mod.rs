//! Pattern learning subsystem for the Command Intelligence Engine.
//!
//! This module coordinates all pattern learning from commands.
//! It is the canonical location for the `get_or_create_token` function
//! and delegates to specialized submodules for each pattern type.
//!
//! # Learning Pipeline
//!
//! When a command is learned (via `learn_command`):
//! 1. **Tokenization**: Command is split into `AnalyzedToken`s
//! 2. **Token Storage**: Each token gets an ID via `get_or_create_token`
//! 3. **Hierarchy Learning**: Position-aware parent-child relationships (primary)
//! 4. **Sequence Learning**: Pairwise token transitions
//! 5. **Pipe Learning**: Post-pipe command patterns
//! 6. **Flag Learning**: Common values for flags
//!
//! # Pattern Types
//!
//! - **Hierarchy** (`ci_command_hierarchy`): Primary suggestion source.
//!   Tracks which tokens appear at which positions with parent context.
//! - **Sequences** (`ci_sequences`): Token-to-token transitions.
//! - **Pipes** (`ci_pipe_chains`): Commands that follow pipes.
//! - **Flags** (`ci_flag_values`): Common values for specific flags.

pub mod flags;
pub mod hierarchy;
pub mod pipes;
pub mod sequences;

use std::collections::HashMap;

use rusqlite::Connection;
use tracing::debug;

use super::error::CIError;
use super::templates;
use super::tokenizer::{analyze_command, compute_command_hash, token_type_to_string};
use super::variants;

/// Learns patterns from a command.
///
/// This is the main entry point for pattern learning, called when a command
/// completes execution.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `token_cache` - Cache for token IDs
/// * `command` - The command text to learn from
/// * `exit_status` - Optional exit status (0 = success)
/// * `session_db_id` - Optional database ID of the current session
pub fn learn_command(
    conn: &mut Connection,
    token_cache: &mut HashMap<String, i64>,
    command: &str,
    exit_status: Option<i32>,
    session_db_id: Option<i64>,
) -> Result<(), CIError> {
    let command = command.trim();
    if command.is_empty() {
        return Ok(());
    }

    let tokens = analyze_command(command);
    if tokens.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().timestamp();
    let is_success = exit_status.map(|s| s == 0).unwrap_or(false);

    // Get or create token IDs
    let mut token_ids = Vec::new();
    for token in &tokens {
        let token_id = get_or_create_token(conn, token_cache, &token.text, token.token_type, now)?;
        token_ids.push(token_id);
    }

    let base_command_id = token_ids.first().copied();

    // Insert command record if not exists
    let command_hash = compute_command_hash(command);
    let token_ids_json = serde_json::to_string(&token_ids)?;

    conn.execute(
        "INSERT OR IGNORE INTO ci_commands
         (command_line, command_hash, token_ids, token_count, base_command_id, exit_status, timestamp, session_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            command,
            command_hash,
            token_ids_json,
            tokens.len(),
            base_command_id,
            exit_status,
            now,
            session_db_id,
        ],
    )?;

    // Learn all pattern types
    sequences::learn_sequences(conn, &tokens, &token_ids, base_command_id, is_success, now)?;
    pipes::learn_pipe_chains(conn, token_cache, &tokens, base_command_id, now)?;
    flags::learn_flag_values(conn, &tokens, &token_ids, base_command_id, now)?;

    // Learn command hierarchy (unified position-aware learning)
    hierarchy::learn_hierarchy(conn, &tokens, &token_ids, is_success, now)?;

    // Extract templates from the command
    if let Err(e) = templates::extract_template(conn, command) {
        debug!("Template extraction skipped for '{}': {}", command, e);
    }

    // Record execution success/failure for variants
    if let Some(status) = exit_status {
        if let Err(e) = variants::record_execution(conn, command, status) {
            debug!("Variant recording failed for '{}': {}", command, e);
        }
    }

    debug!(command = %command, "Learned patterns from command");

    Ok(())
}

/// Gets or creates a token in the vocabulary.
///
/// This is the canonical implementation for token management.
/// It uses a cache for performance and updates frequency/last_seen on each access.
pub fn get_or_create_token(
    conn: &Connection,
    cache: &mut HashMap<String, i64>,
    text: &str,
    token_type: crate::chrome::command_edit::TokenType,
    timestamp: i64,
) -> Result<i64, CIError> {
    // Check cache first
    if let Some(&id) = cache.get(text) {
        // Update frequency
        conn.execute(
            "UPDATE ci_tokens SET frequency = frequency + 1, last_seen = ?1 WHERE id = ?2",
            rusqlite::params![timestamp, id],
        )?;
        return Ok(id);
    }

    let type_str = token_type_to_string(token_type);

    // Try to get existing token
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM ci_tokens WHERE text = ?1", [text], |row| {
            row.get(0)
        })
        .ok();

    let id = if let Some(id) = existing {
        // Update frequency and last_seen
        conn.execute(
            "UPDATE ci_tokens SET frequency = frequency + 1, last_seen = ?1 WHERE id = ?2",
            rusqlite::params![timestamp, id],
        )?;
        id
    } else {
        // Create new token
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, frequency, first_seen, last_seen)
             VALUES (?1, ?2, 1, ?3, ?3)",
            rusqlite::params![text, type_str, timestamp],
        )?;
        conn.last_insert_rowid()
    };

    // Update cache
    cache.insert(text.to_string(), id);

    Ok(id)
}

/// Queries suggestions for post-pipe commands.
///
/// Returns tuples of (pipe_command, frequency, last_seen).
pub fn suggest_pipe_commands(
    conn: &Connection,
    pre_pipe_command: &str,
    limit: usize,
) -> Vec<(String, u32, i64)> {
    pipes::query_pipe_chains(conn, pre_pipe_command, limit).unwrap_or_default()
}

/// Queries common values for a flag.
///
/// Returns tuples of (value, frequency, last_seen).
pub fn suggest_flag_values(
    conn: &Connection,
    base_command: &str,
    subcommand: Option<&str>,
    flag: &str,
    limit: usize,
) -> Vec<(String, u32, i64)> {
    flags::query_flag_values(conn, base_command, subcommand, flag, limit).unwrap_or_default()
}

/// Queries command hierarchy for suggestions at a given position.
///
/// Returns tuples of (token_text, frequency, success_count, last_seen, role).
pub fn suggest_from_hierarchy(
    conn: &Connection,
    position: usize,
    parent_token: Option<&str>,
    base_command: Option<&str>,
    limit: usize,
) -> Vec<hierarchy::HierarchyResult> {
    hierarchy::query_hierarchy(conn, position, parent_token, base_command, limit)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();

        // Create reedline-like history table
        conn.execute(
            "CREATE TABLE history (
                id INTEGER PRIMARY KEY,
                command_line TEXT NOT NULL,
                start_timestamp INTEGER,
                exit_status INTEGER,
                cwd TEXT
            )",
            [],
        )
        .unwrap();

        // Create intelligence schema
        super::super::db_schema::create_schema(&conn).unwrap();

        conn
    }

    #[test]
    fn test_learn_command() {
        let mut conn = setup_test_db();
        let mut cache = HashMap::new();

        learn_command(&mut conn, &mut cache, "git commit -m 'test'", Some(0), None).unwrap();

        // Verify tokens were created
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_tokens", [], |row| row.get(0))
            .unwrap();
        assert!(count >= 4);
    }

    #[test]
    fn test_learn_command_with_session() {
        let mut conn = setup_test_db();
        let mut cache = HashMap::new();

        // Create a session
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO ci_sessions (session_id, start_time, command_count) VALUES ('test-session', ?1, 0)",
            [now],
        ).unwrap();

        let session_db_id: i64 = conn
            .query_row(
                "SELECT id FROM ci_sessions WHERE session_id = 'test-session'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        learn_command(
            &mut conn,
            &mut cache,
            "git status",
            Some(0),
            Some(session_db_id),
        )
        .unwrap();

        // Verify command was created with session_id
        let cmd_session_id: Option<i64> = conn
            .query_row(
                "SELECT session_id FROM ci_commands WHERE command_line = 'git status'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cmd_session_id, Some(session_db_id));
    }

    #[test]
    fn test_token_cache() {
        let conn = setup_test_db();
        let mut cache = HashMap::new();

        let now = chrono::Utc::now().timestamp();
        let id1 = get_or_create_token(
            &conn,
            &mut cache,
            "git",
            crate::chrome::command_edit::TokenType::Command,
            now,
        )
        .unwrap();
        let id2 = get_or_create_token(
            &conn,
            &mut cache,
            "git",
            crate::chrome::command_edit::TokenType::Command,
            now,
        )
        .unwrap();

        assert_eq!(id1, id2);
        assert!(cache.contains_key("git"));
    }
}
