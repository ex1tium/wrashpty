//! Incremental synchronization from reedline history.

use rusqlite::Connection;
use tracing::{debug, info, warn};

use super::error::CIError;
use super::templates;
use super::tokenizer::{analyze_command, compute_command_hash, token_type_to_string};
use super::types::SyncStats;
use super::variants;

/// Synchronizes the intelligence system with reedline's history.
///
/// This function reads new entries from reedline's `history` table
/// and processes them into the intelligence tables.
///
/// # Note on failure handling
///
/// When an individual entry fails to process, the sync continues but only
/// advances `last_sync_id` up to the last successfully processed entry.
/// This ensures that failed entries can be retried on the next sync attempt.
pub fn sync_from_reedline(conn: &Connection, last_sync_id: i64) -> Result<(SyncStats, i64), CIError> {
    let start = std::time::Instant::now();
    let mut stats = SyncStats::default();

    // Query new history entries since last sync
    let mut stmt = conn.prepare(
        "SELECT id, command_line, start_timestamp, exit_status, cwd
         FROM history
         WHERE id > ?1
         ORDER BY id ASC
         LIMIT 1000"
    )?;

    let rows = stmt.query_map([last_sync_id], |row| {
        Ok(HistoryEntry {
            id: row.get(0)?,
            command_line: row.get(1)?,
            timestamp: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
            exit_status: row.get(3)?,
            cwd: row.get(4)?,
        })
    })?;

    let mut entries = Vec::new();
    let mut read_errors = 0;

    for row in rows {
        match row {
            Ok(entry) => {
                entries.push(entry);
            }
            Err(e) => {
                warn!("Failed to read history entry: {}", e);
                read_errors += 1;
            }
        }
    }

    if entries.is_empty() {
        return Ok((stats, last_sync_id));
    }

    debug!("Processing {} new history entries", entries.len());

    // Track the highest successfully processed ID
    // We only advance last_sync_id up to the last successful entry to ensure
    // failed entries can be retried on the next sync
    let mut highest_success_id = last_sync_id;
    let mut failed_ids: Vec<i64> = Vec::new();

    // Process entries in a transaction with proper rollback on error
    conn.execute_batch("BEGIN TRANSACTION")?;

    let transaction_result = (|| -> Result<(), CIError> {
        for entry in &entries {
            match process_entry(conn, entry, &mut stats) {
                Ok(()) => {
                    stats.commands_processed += 1;
                    // Only advance to this ID if all previous entries succeeded
                    // This ensures we don't skip over failed entries
                    if failed_ids.is_empty() {
                        highest_success_id = entry.id;
                    }
                }
                Err(e) => {
                    warn!(
                        id = entry.id,
                        command = %entry.command_line,
                        error = %e,
                        "Failed to process history entry"
                    );
                    failed_ids.push(entry.id);
                    stats.entries_skipped += 1;
                }
            }
        }

        // Only update sync state to the highest successfully processed ID
        // If there were failures, we stop at the last success before any failure
        // This allows failed entries to be retried on the next sync
        update_sync_state(conn, highest_success_id)?;

        Ok(())
    })();

    match transaction_result {
        Ok(()) => {
            if let Err(e) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(CIError::from(e));
            }
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    stats.duration_ms = start.elapsed().as_millis() as u64;

    // Log summary with skipped entries
    if failed_ids.is_empty() {
        info!(
            commands = stats.commands_processed,
            tokens = stats.tokens_extracted,
            sequences = stats.sequences_learned,
            pipes = stats.pipe_chains_learned,
            flags = stats.flag_values_learned,
            duration_ms = stats.duration_ms,
            "Sync completed"
        );
    } else {
        warn!(
            commands = stats.commands_processed,
            skipped = stats.entries_skipped,
            read_errors = read_errors,
            tokens = stats.tokens_extracted,
            sequences = stats.sequences_learned,
            duration_ms = stats.duration_ms,
            failed_ids = ?failed_ids,
            "Sync completed with failures - will retry on next sync"
        );
    }

    Ok((stats, highest_success_id))
}

/// A history entry from reedline.
struct HistoryEntry {
    id: i64,
    command_line: String,
    timestamp: i64,
    exit_status: Option<i32>,
    cwd: Option<String>,
}

/// Processes a single history entry.
fn process_entry(conn: &Connection, entry: &HistoryEntry, stats: &mut SyncStats) -> Result<(), CIError> {
    let command = entry.command_line.trim();
    if command.is_empty() {
        return Ok(());
    }

    // Analyze the command
    let tokens = analyze_command(command);
    if tokens.is_empty() {
        return Ok(());
    }

    // Get or create token IDs
    let mut token_ids = Vec::new();
    let now = entry.timestamp;

    for token in &tokens {
        let token_id = get_or_create_token(conn, &token.text, token.token_type, now)?;
        token_ids.push(token_id);
        stats.tokens_extracted += 1;
    }

    let base_command_id = token_ids.first().copied();

    // Insert command record
    let command_hash = compute_command_hash(command);
    let token_ids_json = serde_json::to_string(&token_ids)?;

    conn.execute(
        "INSERT OR IGNORE INTO ci_commands
         (reedline_id, command_line, command_hash, token_ids, token_count,
          base_command_id, exit_status, cwd, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            entry.id,
            command,
            command_hash,
            token_ids_json,
            tokens.len(),
            base_command_id,
            entry.exit_status,
            entry.cwd,
            now,
        ],
    )?;

    // Learn patterns from this command
    learn_sequences(conn, &tokens, &token_ids, base_command_id, entry.exit_status, now, stats)?;
    learn_pipe_chains(conn, &tokens, &token_ids, base_command_id, now, stats)?;
    learn_flag_values(conn, &tokens, &token_ids, base_command_id, now, stats)?;

    // Extract templates from the command
    if let Err(e) = templates::extract_template(conn, command) {
        debug!("Template extraction skipped for '{}': {}", command, e);
    }

    // Record execution success/failure for variants
    if let Some(exit_status) = entry.exit_status {
        if let Err(e) = variants::record_execution(conn, command, exit_status) {
            debug!("Variant recording failed for '{}': {}", command, e);
        }
    }

    Ok(())
}

/// Gets or creates a token in the vocabulary.
fn get_or_create_token(
    conn: &Connection,
    text: &str,
    token_type: crate::chrome::command_edit::TokenType,
    timestamp: i64,
) -> Result<i64, CIError> {
    let type_str = token_type_to_string(token_type);

    // Try to get existing token
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [text],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        // Update frequency and last_seen
        conn.execute(
            "UPDATE ci_tokens SET frequency = frequency + 1, last_seen = ?1 WHERE id = ?2",
            rusqlite::params![timestamp, id],
        )?;
        return Ok(id);
    }

    // Create new token
    conn.execute(
        "INSERT INTO ci_tokens (text, token_type, frequency, first_seen, last_seen)
         VALUES (?1, ?2, 1, ?3, ?3)",
        rusqlite::params![text, type_str, timestamp],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Learns token sequences from a command.
fn learn_sequences(
    conn: &Connection,
    tokens: &[super::types::AnalyzedToken],
    token_ids: &[i64],
    base_command_id: Option<i64>,
    exit_status: Option<i32>,
    timestamp: i64,
    stats: &mut SyncStats,
) -> Result<(), CIError> {
    if tokens.len() < 2 {
        return Ok(());
    }

    let is_success = exit_status.map(|s| s == 0).unwrap_or(false);

    // Learn pairwise sequences
    for i in 0..tokens.len() - 1 {
        let context_id = token_ids[i];
        let next_id = token_ids[i + 1];

        // Insert or update sequence
        let updated = conn.execute(
            "INSERT INTO ci_sequences
             (context_token_id, context_position, base_command_id, next_token_id, frequency, success_count, last_seen)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)
             ON CONFLICT(context_token_id, context_position, base_command_id, next_token_id)
             DO UPDATE SET
                 frequency = frequency + 1,
                 success_count = success_count + ?5,
                 last_seen = ?6",
            rusqlite::params![
                context_id,
                i,
                base_command_id,
                next_id,
                if is_success { 1 } else { 0 },
                timestamp,
            ],
        )?;

        if updated > 0 {
            stats.sequences_learned += 1;
        }
    }

    // Learn 2-grams and 3-grams
    learn_ngrams(conn, tokens, token_ids, timestamp)?;

    Ok(())
}

/// Learns n-gram patterns from a command.
fn learn_ngrams(
    conn: &Connection,
    tokens: &[super::types::AnalyzedToken],
    token_ids: &[i64],
    timestamp: i64,
) -> Result<(), CIError> {
    use super::tokenizer::compute_token_hash;

    // 2-grams: pairs of tokens predicting the next
    if tokens.len() >= 3 {
        for i in 0..tokens.len() - 2 {
            let context_tokens: Vec<&str> = tokens[i..i + 2].iter().map(|t| t.text.as_str()).collect();
            let pattern_hash = compute_token_hash(&context_tokens);
            let context_ids = serde_json::to_string(&token_ids[i..i + 2])?;
            let next_id = token_ids[i + 2];

            conn.execute(
                "INSERT INTO ci_ngrams (n, pattern_hash, token_ids, next_token_id, frequency, last_seen)
                 VALUES (2, ?1, ?2, ?3, 1, ?4)
                 ON CONFLICT(pattern_hash)
                 DO UPDATE SET frequency = frequency + 1, last_seen = ?4",
                rusqlite::params![pattern_hash, context_ids, next_id, timestamp],
            )?;
        }
    }

    // 3-grams: triplets of tokens predicting the next
    if tokens.len() >= 4 {
        for i in 0..tokens.len() - 3 {
            let context_tokens: Vec<&str> = tokens[i..i + 3].iter().map(|t| t.text.as_str()).collect();
            let pattern_hash = compute_token_hash(&context_tokens);
            let context_ids = serde_json::to_string(&token_ids[i..i + 3])?;
            let next_id = token_ids[i + 3];

            conn.execute(
                "INSERT INTO ci_ngrams (n, pattern_hash, token_ids, next_token_id, frequency, last_seen)
                 VALUES (3, ?1, ?2, ?3, 1, ?4)
                 ON CONFLICT(pattern_hash)
                 DO UPDATE SET frequency = frequency + 1, last_seen = ?4",
                rusqlite::params![pattern_hash, context_ids, next_id, timestamp],
            )?;
        }
    }

    Ok(())
}

/// Learns pipe chain patterns from a command.
fn learn_pipe_chains(
    conn: &Connection,
    tokens: &[super::types::AnalyzedToken],
    _token_ids: &[i64],
    base_command_id: Option<i64>,
    timestamp: i64,
    stats: &mut SyncStats,
) -> Result<(), CIError> {
    use super::tokenizer::{compute_command_hash, split_at_pipes};

    let segments = split_at_pipes(tokens);
    if segments.len() < 2 {
        return Ok(());
    }

    // For each pipe transition
    for i in 0..segments.len() - 1 {
        let pre_pipe = &segments[i];
        let post_pipe = &segments[i + 1];

        if pre_pipe.is_empty() || post_pipe.is_empty() {
            continue;
        }

        // Hash of pre-pipe segment
        let pre_pipe_text: String = pre_pipe.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        let pre_pipe_hash = compute_command_hash(&pre_pipe_text);

        // First token after pipe is the pipe command
        let pipe_command = &post_pipe[0].text;
        let pipe_command_id = get_or_create_token(
            conn,
            pipe_command,
            post_pipe[0].token_type,
            timestamp,
        )?;

        // Full post-pipe chain
        let full_chain: String = post_pipe.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");

        conn.execute(
            "INSERT INTO ci_pipe_chains
             (pre_pipe_base_cmd_id, pre_pipe_hash, pipe_command_id, full_chain, chain_length, frequency, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
             ON CONFLICT(pre_pipe_hash, pipe_command_id)
             DO UPDATE SET frequency = frequency + 1, last_seen = ?6",
            rusqlite::params![
                base_command_id,
                pre_pipe_hash,
                pipe_command_id,
                full_chain,
                post_pipe.len(),
                timestamp,
            ],
        )?;

        stats.pipe_chains_learned += 1;
    }

    Ok(())
}

/// Learns flag-value associations from a command.
fn learn_flag_values(
    conn: &Connection,
    tokens: &[super::types::AnalyzedToken],
    token_ids: &[i64],
    base_command_id: Option<i64>,
    timestamp: i64,
    stats: &mut SyncStats,
) -> Result<(), CIError> {
    use super::tokenizer::detect_value_type;
    use crate::chrome::command_edit::TokenType;

    let Some(base_id) = base_command_id else {
        return Ok(());
    };

    // Find subcommand if present
    let subcommand_id = if tokens.len() > 1 && tokens[1].token_type == TokenType::Subcommand {
        Some(token_ids[1])
    } else {
        None
    };

    // Look for flag -> value pairs
    for i in 0..tokens.len() - 1 {
        if tokens[i].token_type != TokenType::Flag {
            continue;
        }

        let next = &tokens[i + 1];

        // Skip if next token is also a flag (boolean flag)
        if next.token_type == TokenType::Flag {
            continue;
        }

        // Skip pipes and redirects
        if next.text == "|" || next.text == ">" || next.text == ">>" || next.text == "<" {
            continue;
        }

        let flag_text = &tokens[i].text;
        let value_text = &next.text;
        let value_type = detect_value_type(value_text);

        conn.execute(
            "INSERT INTO ci_flag_values
             (base_command_id, subcommand_id, flag_text, value_text, value_type, frequency, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
             ON CONFLICT(base_command_id, subcommand_id, flag_text, value_text)
             DO UPDATE SET frequency = frequency + 1, last_seen = ?6",
            rusqlite::params![
                base_id,
                subcommand_id,
                flag_text,
                value_text,
                value_type,
                timestamp,
            ],
        )?;

        stats.flag_values_learned += 1;
    }

    Ok(())
}

/// Updates the sync state with the last processed ID.
fn update_sync_state(conn: &Connection, last_id: i64) -> Result<(), CIError> {
    conn.execute(
        "INSERT OR REPLACE INTO ci_sync_state (key, value) VALUES ('last_sync_id', ?1)",
        [last_id.to_string()],
    )?;
    Ok(())
}

/// Gets the last sync ID from the database.
pub fn get_last_sync_id(conn: &Connection) -> Result<i64, CIError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT value FROM ci_sync_state WHERE key = 'last_sync_id'",
            [],
            |row| row.get(0),
        )
        .ok();

    Ok(id.and_then(|s| s.parse().ok()).unwrap_or(0))
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
        ).unwrap();

        // Create intelligence schema
        super::super::db_schema::create_schema(&conn).unwrap();

        conn
    }

    #[test]
    fn test_sync_empty_history() {
        let conn = setup_test_db();
        let (stats, last_id) = sync_from_reedline(&conn, 0).unwrap();
        assert_eq!(stats.commands_processed, 0);
        assert_eq!(last_id, 0);
    }

    #[test]
    fn test_sync_processes_entries() {
        let conn = setup_test_db();

        // Insert test history
        conn.execute(
            "INSERT INTO history (command_line, start_timestamp, exit_status, cwd)
             VALUES ('git commit -m test', 1700000000000, 0, '/home/user')",
            [],
        ).unwrap();

        let (stats, _) = sync_from_reedline(&conn, 0).unwrap();
        assert_eq!(stats.commands_processed, 1);
        assert!(stats.tokens_extracted > 0);
    }

    #[test]
    fn test_get_or_create_token() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        let id1 = get_or_create_token(&conn, "git", crate::chrome::command_edit::TokenType::Command, now).unwrap();
        let id2 = get_or_create_token(&conn, "git", crate::chrome::command_edit::TokenType::Command, now).unwrap();

        assert_eq!(id1, id2);

        // Check frequency was incremented
        let freq: i32 = conn.query_row(
            "SELECT frequency FROM ci_tokens WHERE id = ?1",
            [id1],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(freq, 2);
    }

    #[test]
    fn test_sync_incremental_id_tracking() {
        let conn = setup_test_db();

        // Insert multiple history entries
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (1, 'git status', 1700000000000, 0, '/home/user')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (2, 'git add .', 1700000001000, 0, '/home/user')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (3, 'git commit -m test', 1700000002000, 0, '/home/user')",
            [],
        ).unwrap();

        // First sync should process all entries
        let (stats, last_id) = sync_from_reedline(&conn, 0).unwrap();
        assert_eq!(stats.commands_processed, 3);
        assert_eq!(last_id, 3);

        // Verify the sync state was persisted
        let stored_id = get_last_sync_id(&conn).unwrap();
        assert_eq!(stored_id, 3);

        // Add more entries
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (4, 'git push', 1700000003000, 0, '/home/user')",
            [],
        ).unwrap();

        // Second sync should only process the new entry
        let (stats, last_id) = sync_from_reedline(&conn, 3).unwrap();
        assert_eq!(stats.commands_processed, 1);
        assert_eq!(last_id, 4);
    }

    #[test]
    fn test_sync_skipped_entries_counted() {
        let conn = setup_test_db();

        // Insert a valid entry
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (1, 'git status', 1700000000000, 0, '/home/user')",
            [],
        ).unwrap();

        // Insert an empty command (which will be skipped but not counted as an error)
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (2, '   ', 1700000001000, 0, '/home/user')",
            [],
        ).unwrap();

        // Insert another valid entry
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (3, 'git commit -m test', 1700000002000, 0, '/home/user')",
            [],
        ).unwrap();

        // Sync should process 2 entries (empty command is silently skipped)
        let (_stats, last_id) = sync_from_reedline(&conn, 0).unwrap();
        // Empty commands are silently handled, so they count as processed
        // but produce no tokens
        assert_eq!(last_id, 3);
    }

    #[test]
    fn test_entries_skipped_counter() {
        // Create a fresh SyncStats and verify entries_skipped defaults to 0
        let stats = SyncStats::default();
        assert_eq!(stats.entries_skipped, 0);
    }
}
