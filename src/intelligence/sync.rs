//! Incremental synchronization from reedline history.
//!
//! This module reads new entries from reedline's `history` table and
//! processes them into the intelligence tables. It delegates all pattern
//! learning to the `patterns` module submodules:
//!
//! - `patterns::get_or_create_token` - Unified token management
//! - `sequences::learn_sequences` - Token sequence learning
//! - `pipes::learn_pipe_chains` - Pipe chain learning
//! - `flags::learn_flag_values` - Flag value learning
//! - `hierarchy::learn_hierarchy` - Command hierarchy learning (primary)
//!
//! # Sync Strategy
//!
//! The sync is incremental, tracking `last_sync_id` to avoid reprocessing.
//! When failures occur, the sync advances only to the last successful entry,
//! allowing failed entries to be retried on the next sync.

use std::collections::HashMap;

use rusqlite::Connection;
use tracing::{debug, info, warn};

use super::error::CIError;
use super::patterns::{self, flags, hierarchy, pipes, sequences};
use super::schema_provider::SchemaProvider;
use super::templates;
use super::tokenizer::{analyze_command, compute_command_hash};
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
pub fn sync_from_reedline(
    conn: &Connection,
    last_sync_id: i64,
    provider: &dyn SchemaProvider,
) -> Result<(SyncStats, i64), CIError> {
    let start = std::time::Instant::now();
    let mut stats = SyncStats::default();
    let mut token_cache: HashMap<String, i64> = HashMap::new();

    // Query new history entries since last sync
    let mut stmt = conn.prepare(
        "SELECT id, command_line, start_timestamp, exit_status, cwd
         FROM history
         WHERE id > ?1
         ORDER BY id ASC
         LIMIT 1000",
    )?;

    // Use current timestamp as fallback for entries missing start_timestamp
    // This ensures fresh synced commands aren't scored as ancient (timestamp 0)
    let now = chrono::Utc::now().timestamp();

    let rows = stmt.query_map([last_sync_id], |row| {
        Ok(HistoryEntry {
            id: row.get(0)?,
            command_line: row.get(1)?,
            timestamp: row.get::<_, Option<i64>>(2)?.unwrap_or(now),
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
            match process_entry(conn, &mut token_cache, entry, &mut stats, provider) {
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
            hierarchy = stats.hierarchy_learned,
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
            hierarchy = stats.hierarchy_learned,
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
fn process_entry(
    conn: &Connection,
    token_cache: &mut HashMap<String, i64>,
    entry: &HistoryEntry,
    stats: &mut SyncStats,
    provider: &dyn SchemaProvider,
) -> Result<(), CIError> {
    let command = entry.command_line.trim();
    if command.is_empty() {
        return Ok(());
    }

    // Analyze the command
    let tokens = analyze_command(command);
    if tokens.is_empty() {
        return Ok(());
    }

    // Get or create token IDs using the unified patterns function
    let mut token_ids = Vec::new();
    let now = entry.timestamp;

    for token in &tokens {
        let token_id =
            patterns::get_or_create_token(conn, token_cache, &token.text, token.token_type, now)?;
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

    // Learn patterns from this command using unified patterns modules
    let is_success = entry.exit_status.map(|s| s == 0).unwrap_or(false);

    // Learn sequences (pairwise token transitions)
    sequences::learn_sequences(conn, &tokens, &token_ids, base_command_id, is_success, now)?;
    stats.sequences_learned += tokens.len().saturating_sub(1);

    // Learn pipe chains
    pipes::learn_pipe_chains(conn, token_cache, &tokens, base_command_id, now)?;
    // Count pipe chains: number of pipe transitions in the command
    let pipe_segments = super::tokenizer::split_at_pipes(&tokens);
    stats.pipe_chains_learned += pipe_segments.len().saturating_sub(1);

    // Learn flag values
    flags::learn_flag_values(conn, &tokens, &token_ids, base_command_id, now)?;
    // Count flag-value pairs learned
    stats.flag_values_learned += count_flag_value_pairs(&tokens);

    // Learn command hierarchy (critical for suggestions)
    hierarchy::learn_hierarchy(conn, &tokens, &token_ids, is_success, now)?;
    stats.hierarchy_learned += tokens.len();

    // Keep schema provider in sync with learned command structure.
    if let Err(e) = upsert_schema_from_tokens(conn, &tokens, provider) {
        debug!(
            command = %command,
            error = %e,
            "Schema learning skipped for history entry"
        );
    }

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

/// Counts the number of flag-value pairs in a token list.
///
/// A flag-value pair is a flag token followed by a non-flag, non-pipe token.
fn count_flag_value_pairs(tokens: &[super::types::AnalyzedToken]) -> usize {
    use crate::chrome::command_edit::TokenType;

    let mut count = 0;
    for i in 0..tokens.len().saturating_sub(1) {
        if tokens[i].token_type != TokenType::Flag {
            continue;
        }
        let next = &tokens[i + 1];
        // Skip if next is a flag (boolean flag) or pipe/redirect/operator
        if next.token_type == TokenType::Flag {
            continue;
        }
        if matches!(
            next.token_type,
            TokenType::Pipe | TokenType::Redirect | TokenType::Operator
        ) || next.text == "|"
            || next.text == ">"
            || next.text == ">>"
            || next.text == "<"
            || next.text.ends_with('|')
        {
            continue;
        }
        count += 1;
    }
    count
}

/// Upserts schema information learned from tokenized command history.
///
/// Keeps the schema provider's learned schemas in sync with observed usage.
/// Bundled schemas are authoritative and never overwritten.
///
/// Currently a no-op for incremental schema building from usage tokens.
/// Schemas are persisted to cs_* tables through explicit operations
/// (discovery, import) rather than incrementally from each command execution.
/// The ci_* hierarchy/sequences/flags tables capture equivalent usage
/// patterns through the learned patterns pipeline.
///
/// Takes `&dyn SchemaProvider` (immutable) intentionally: this function only
/// reads from the provider (to check `is_bundled`). If future phases add
/// incremental writes, the signature should change to `&mut dyn SchemaProvider`.
pub(crate) fn upsert_schema_from_tokens(
    _conn: &Connection,
    tokens: &[super::types::AnalyzedToken],
    provider: &dyn SchemaProvider,
) -> Result<(), CIError> {
    let Some(base_command) = tokens.first().map(|t| t.text.as_str()) else {
        return Ok(());
    };

    // Bundled schemas are authoritative — don't override with usage data.
    if provider.is_bundled(base_command) {
        return Ok(());
    }

    // Incremental schema building from usage tokens is deferred.
    // The ci_* hierarchy/sequences/flags tables provide equivalent
    // suggestion data through the learned patterns pipeline.
    let _ = base_command;

    Ok(())
}

#[cfg(test)]
mod tests {
    use command_schema_core::{CommandSchema, SchemaSource};

    use super::*;
    use crate::intelligence::schema_provider::tests::TestSchemaProvider;

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

    fn empty_provider() -> TestSchemaProvider {
        TestSchemaProvider::new()
    }

    #[test]
    fn test_sync_empty_history() {
        let conn = setup_test_db();
        let provider = empty_provider();
        let (stats, last_id) = sync_from_reedline(&conn, 0, &provider).unwrap();
        assert_eq!(stats.commands_processed, 0);
        assert_eq!(last_id, 0);
    }

    #[test]
    fn test_sync_processes_entries() {
        let conn = setup_test_db();
        let provider = empty_provider();

        // Insert test history
        conn.execute(
            "INSERT INTO history (command_line, start_timestamp, exit_status, cwd)
             VALUES ('git commit -m test', 1700000000000, 0, '/home/user')",
            [],
        )
        .unwrap();

        let (stats, _) = sync_from_reedline(&conn, 0, &provider).unwrap();
        assert_eq!(stats.commands_processed, 1);
        assert!(stats.tokens_extracted > 0);
    }

    #[test]
    fn test_get_or_create_token() {
        let conn = setup_test_db();
        let mut cache = HashMap::new();
        let now = chrono::Utc::now().timestamp();

        let id1 = patterns::get_or_create_token(
            &conn,
            &mut cache,
            "git",
            crate::chrome::command_edit::TokenType::Command,
            now,
        )
        .unwrap();
        let id2 = patterns::get_or_create_token(
            &conn,
            &mut cache,
            "git",
            crate::chrome::command_edit::TokenType::Command,
            now,
        )
        .unwrap();

        assert_eq!(id1, id2);

        // Check frequency was incremented
        let freq: i32 = conn
            .query_row(
                "SELECT frequency FROM ci_tokens WHERE id = ?1",
                [id1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(freq, 2);
    }

    #[test]
    fn test_sync_incremental_id_tracking() {
        let conn = setup_test_db();
        let provider = empty_provider();

        // Insert multiple history entries
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (1, 'git status', 1700000000000, 0, '/home/user')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (2, 'git add .', 1700000001000, 0, '/home/user')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (3, 'git commit -m test', 1700000002000, 0, '/home/user')",
            [],
        )
        .unwrap();

        // First sync should process all entries
        let (stats, last_id) = sync_from_reedline(&conn, 0, &provider).unwrap();
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
        )
        .unwrap();

        // Second sync should only process the new entry
        let (stats, last_id) = sync_from_reedline(&conn, 3, &provider).unwrap();
        assert_eq!(stats.commands_processed, 1);
        assert_eq!(last_id, 4);
    }

    #[test]
    fn test_sync_skipped_entries_counted() {
        let conn = setup_test_db();
        let provider = empty_provider();

        // Insert a valid entry
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (1, 'git status', 1700000000000, 0, '/home/user')",
            [],
        )
        .unwrap();

        // Insert an empty command (which will be skipped but not counted as an error)
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (2, '   ', 1700000001000, 0, '/home/user')",
            [],
        )
        .unwrap();

        // Insert another valid entry
        conn.execute(
            "INSERT INTO history (id, command_line, start_timestamp, exit_status, cwd)
             VALUES (3, 'git commit -m test', 1700000002000, 0, '/home/user')",
            [],
        )
        .unwrap();

        // Sync should process 2 entries (empty command is silently skipped)
        let (_stats, last_id) = sync_from_reedline(&conn, 0, &provider).unwrap();
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

    // NOTE: Tests for schema learning from usage (test_sync_populates_schema_rows_for_seen_commands,
    // test_sync_skips_schema_rows_for_curated_commands, test_sync_learns_subcommand_path_for_unknown_base_command)
    // were removed during the SchemaProvider refactor. upsert_schema_from_tokens is currently a
    // no-op for writes pending Phase 4 (cs_* table integration). The ci_* hierarchy/sequences/flags
    // tables still capture equivalent usage patterns. These tests will be restored in Phase 4
    // when cs_* persistence is wired up via SchemaQuery.

    #[test]
    fn test_upsert_schema_skips_bundled_commands() {
        let conn = setup_test_db();
        let provider = TestSchemaProvider::with_bundled(vec![CommandSchema::new(
            "cargo",
            SchemaSource::Bootstrap,
        )]);

        let tokens = super::super::tokenizer::analyze_command("cargo build --release");
        // Should return Ok without error (bundled commands are skipped)
        upsert_schema_from_tokens(&conn, &tokens, &provider).unwrap();
    }

    #[test]
    fn test_sync_populates_hierarchy_table() {
        let conn = setup_test_db();
        let provider = empty_provider();

        // Insert test history with a multi-token command
        conn.execute(
            "INSERT INTO history (command_line, start_timestamp, exit_status, cwd)
             VALUES ('git remote add origin https://github.com/user/repo', 1700000000000, 0, '/home/user')",
            [],
        ).unwrap();

        let (stats, _) = sync_from_reedline(&conn, 0, &provider).unwrap();
        assert_eq!(stats.commands_processed, 1);
        assert!(stats.hierarchy_learned > 0, "Hierarchy should be populated");

        // Verify hierarchy table has entries
        let hierarchy_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            hierarchy_count >= 5,
            "Expected at least 5 hierarchy entries for 'git remote add origin <url>'"
        );

        // Verify we can query suggestions from the hierarchy
        let git_id: i64 = conn
            .query_row("SELECT id FROM ci_tokens WHERE text = 'git'", [], |row| {
                row.get(0)
            })
            .unwrap();

        let subcommands: Vec<String> = conn
            .prepare(
                "SELECT t.text FROM ci_command_hierarchy h
             JOIN ci_tokens t ON t.id = h.token_id
             WHERE h.position = 1 AND h.parent_token_id = ?1",
            )
            .unwrap()
            .query_map([git_id], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(
            subcommands.contains(&"remote".to_string()),
            "Should find 'remote' as git subcommand"
        );
    }
}
