//! History store with SQLite-backed storage and rich metadata.
//!
//! This module provides a centralized history store that wraps reedline's
//! `SqliteBackedHistory` and provides rich query capabilities for the history panel.
//! The SQLite database is shared between reedline (for line editing) and the panel
//! (for metadata-rich browsing).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reedline::{History, HistoryItem, HistoryItemId, SqliteBackedHistory};
use rusqlite::Connection;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::chrome::command_edit::CommandToken;
use crate::intelligence::{CommandIntelligence, FileContext, Suggestion, build_context};

/// Errors that can occur when interacting with the history store.
#[derive(Debug, Error)]
pub enum HistoryStoreError {
    /// I/O error (e.g., creating directories, file operations).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// SQLite database error.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Error from reedline's history implementation.
    #[error("Reedline history error: {0}")]
    Reedline(#[from] reedline::ReedlineError),

    /// Confirmation required but not provided correctly.
    #[error("Confirmation required: {0}")]
    ConfirmationRequired(&'static str),

    /// Internal error for unexpected conditions.
    #[error("{0}")]
    Internal(String),
}

impl HistoryStoreError {
    /// Creates an internal error with a message.
    fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

/// Sort mode for history queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortMode {
    /// Sort by most recent first.
    #[default]
    Recency,
    /// Sort by execution frequency.
    Frequency,
    /// Sort by frecency (frequency weighted by recency).
    Frecency,
}

impl SortMode {
    /// Cycles to the next sort mode.
    pub fn next(self) -> Self {
        match self {
            SortMode::Recency => SortMode::Frequency,
            SortMode::Frequency => SortMode::Frecency,
            SortMode::Frecency => SortMode::Recency,
        }
    }

    /// Returns a human-readable name for the sort mode.
    pub fn name(&self) -> &'static str {
        match self {
            SortMode::Recency => "Recent",
            SortMode::Frequency => "Frequent",
            SortMode::Frecency => "Frecent",
        }
    }
}

/// Filter mode for history queries.
#[derive(Debug, Clone, Default)]
pub struct FilterMode {
    /// Deduplicate entries, showing only unique commands.
    pub dedupe: bool,
    /// Only show commands run in the current directory.
    pub current_dir_only: bool,
    /// Only show commands that failed (non-zero exit status).
    pub failed_only: bool,
}

/// A history record with full metadata.
#[derive(Debug, Clone)]
pub struct HistoryRecord {
    /// The command that was executed.
    pub command: String,
    /// Timestamp of execution.
    pub timestamp: Option<DateTime<Utc>>,
    /// Working directory where command was run.
    pub cwd: Option<PathBuf>,
    /// Exit status of the command.
    pub exit_status: Option<i64>,
    /// Duration of command execution.
    pub duration: Option<Duration>,
    /// Frecency score for sorting.
    pub frecency_score: f64,
    /// Number of times this command was executed.
    pub execution_count: u32,
}

/// Centralized history store wrapping SQLite-backed history.
pub struct HistoryStore {
    /// Path to the SQLite database.
    db_path: PathBuf,
    /// ID of the last command executed (for metadata updates).
    last_command_id: Option<HistoryItemId>,
    /// The reedline history instance (for getting last command ID).
    reedline_history: Option<SqliteBackedHistory>,
    /// Command Intelligence engine for learned suggestions.
    intelligence: Option<CommandIntelligence>,
    /// Last command executed (for learning on completion).
    last_command_text: Option<String>,
}

impl HistoryStore {
    /// Creates a new history store with the given session token.
    ///
    /// Creates the data directory at `~/.local/share/wrashpty/` if needed,
    /// initializes the SQLite database, and performs first-run migration
    /// from `~/.bash_history`.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryStoreError::Io`] if the data directory cannot be created,
    /// or [`HistoryStoreError::Reedline`] if the SQLite history cannot be initialized.
    pub fn new(_session_token: [u8; 16]) -> Result<Self, HistoryStoreError> {
        // Create data directory
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("wrashpty");

        std::fs::create_dir_all(&data_dir)?;

        // Set restrictive permissions on Unix (owner read/write/execute only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&data_dir, dir_perms)?;
        }

        let db_path = data_dir.join("history.db");

        // Check if this is a first run (database doesn't exist)
        let is_first_run = !db_path.exists();

        // Create the reedline history instance
        // Second parameter is optional session_id metadata; None means no session metadata
        // is recorded with history entries for this instance.
        let reedline_history = SqliteBackedHistory::with_file(
            db_path.clone(),
            None, // No session_id metadata is recorded.
            None, // No session_timestamp metadata is recorded.
        )?;

        // Set restrictive permissions on the DB file (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&db_path, file_perms)?;
        }

        let mut store = Self {
            db_path: db_path.clone(),
            last_command_id: None,
            reedline_history: Some(reedline_history),
            intelligence: None,
            last_command_text: None,
        };

        // Create settings table if it doesn't exist
        if let Err(e) = store.create_settings_table() {
            warn!("Failed to create settings table: {}", e);
        }

        // Initialize Command Intelligence lazily (separate connection)
        match CommandIntelligence::from_path(&db_path) {
            Ok(ci) => {
                store.intelligence = Some(ci);
                info!("Command Intelligence initialized");
            }
            Err(e) => {
                warn!("Failed to initialize Command Intelligence: {}", e);
            }
        }

        // Load intelligence settings (restore user preference)
        store.load_intelligence_settings();

        // Migrate from bash_history on first run
        if is_first_run {
            if let Err(e) = store.migrate_from_bash_history() {
                warn!("Failed to migrate bash history: {}", e);
            }
        }

        info!(
            db_path = %store.db_path.display(),
            "History store initialized"
        );

        Ok(store)
    }

    /// Creates the settings table if it doesn't exist.
    fn create_settings_table(&self) -> Result<(), HistoryStoreError> {
        let conn = self.open_connection()?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    /// Gets a setting value by key.
    ///
    /// Returns `Ok(Some(value))` if the key exists, `Ok(None)` if the key
    /// does not exist, and propagates database errors.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, HistoryStoreError> {
        let conn = self.open_connection()?;
        match conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        }) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(HistoryStoreError::from(e)),
        }
    }

    /// Sets a setting value.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), HistoryStoreError> {
        let conn = self.open_connection()?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            [key, value],
        )?;
        Ok(())
    }

    /// Migrates history entries from ~/.bash_history to the SQLite database.
    fn migrate_from_bash_history(&mut self) -> Result<(), HistoryStoreError> {
        let entries = crate::history::load_history().unwrap_or_else(|e| {
            warn!("Failed to load bash history for migration: {}", e);
            Vec::new()
        });

        if entries.is_empty() {
            return Ok(());
        }

        let count = entries.len();
        info!(count, "Migrating entries from bash_history");

        // Save entries to the history database
        if let Some(ref mut history) = self.reedline_history {
            let mut saved = 0;
            for entry in entries {
                let item = HistoryItem::from_command_line(&entry);
                if history.save(item).is_ok() {
                    saved += 1;
                }
            }
            info!(saved, total = count, "Migration complete");
        }

        Ok(())
    }

    /// Creates a reedline history instance for use by the editor.
    ///
    /// Note: This transfers ownership of the internal history instance to reedline.
    /// After calling this, the history store will create a new connection for queries.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryStoreError::Reedline`] if creating a new SQLite history connection fails.
    pub fn create_reedline_history(&mut self) -> Result<Box<dyn History>, HistoryStoreError> {
        if let Some(history) = self.reedline_history.take() {
            Ok(Box::new(history))
        } else {
            // Create a new instance if already taken
            let history = SqliteBackedHistory::with_file(self.db_path.clone(), None, None)?;
            Ok(Box::new(history))
        }
    }

    /// Sets the last command ID for metadata updates.
    ///
    /// This should be called after a command is submitted to reedline
    /// so we know which entry to update with execution metadata.
    pub fn set_last_command_id(&mut self, id: HistoryItemId) {
        self.last_command_id = Some(id);
    }

    /// Opens a connection to the history database with a busy timeout.
    ///
    /// This helper ensures all database operations use a consistent busy timeout
    /// to avoid SQLITE_BUSY errors under concurrent access (e.g., reedline writing
    /// while the panel queries).
    fn open_connection(&self) -> Result<Connection, HistoryStoreError> {
        let conn = Connection::open(&self.db_path)?;
        // Set 250ms busy timeout to handle concurrent access from reedline
        conn.busy_timeout(std::time::Duration::from_millis(250))?;
        Ok(conn)
    }

    /// Updates metadata for the last executed command.
    ///
    /// Uses `self.last_command_id` if set (preferred), otherwise falls back to
    /// querying for the most recent entry. Using the stored ID avoids race
    /// conditions with concurrent writers.
    ///
    /// # Arguments
    ///
    /// * `exit_status` - Exit code of the command
    /// * `duration` - How long the command took to execute
    /// * `cwd` - Working directory where the command was run
    ///
    /// # Errors
    ///
    /// Returns [`HistoryStoreError::Sqlite`] if the database operation fails.
    pub fn update_last_command(
        &mut self,
        exit_status: Option<i32>,
        duration: Option<Duration>,
        cwd: Option<PathBuf>,
    ) -> Result<(), HistoryStoreError> {
        // Open a connection with busy timeout for concurrent access
        let conn = self.open_connection()?;

        // Prefer stored ID to avoid race with concurrent writers.
        // Read without consuming - only clear after successful update.
        let (id, used_stored_id): (i64, bool) =
            if let Some(stored_id) = self.last_command_id.as_ref().map(|id| id.0) {
                (stored_id, true)
            } else {
                // Fallback: query for the most recent command (legacy behavior)
                let last_id: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM history ORDER BY id DESC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .ok();

                let Some(id) = last_id else {
                    debug!("No recent command found to update");
                    return Ok(());
                };
                (id, false)
            };

        // Update the entry with metadata
        let duration_ms = duration.map(|d| d.as_millis() as i64);
        let cwd_str = cwd.map(|p| p.to_string_lossy().to_string());

        conn.execute(
            "UPDATE history SET exit_status = ?1, duration_ms = ?2, cwd = ?3 WHERE id = ?4",
            rusqlite::params![exit_status, duration_ms, cwd_str, id],
        )?;

        // Only clear last_command_id after successful update
        if used_stored_id {
            self.last_command_id = None;
        }

        debug!(
            id,
            exit_status,
            duration_ms,
            cwd = cwd_str.as_deref(),
            "Updated history metadata"
        );

        Ok(())
    }

    /// Queries history entries with filtering and sorting.
    ///
    /// # Arguments
    ///
    /// * `search` - Optional search string to filter commands
    /// * `filter` - Filter mode settings
    /// * `sort` - Sort mode
    /// * `current_cwd` - Current working directory for "here" filter
    /// * `limit` - Maximum number of results to return
    ///
    /// # Errors
    ///
    /// Returns [`HistoryStoreError::Sqlite`] if the database query fails.
    pub fn query(
        &self,
        search: &str,
        filter: &FilterMode,
        sort: &SortMode,
        current_cwd: Option<&PathBuf>,
        limit: usize,
    ) -> Result<Vec<HistoryRecord>, HistoryStoreError> {
        let conn = self.open_connection()?;

        // For dedupe with recency sort, we want to show unique commands ordered by their
        // most recent execution. We use a subquery to first get the most recent execution
        // of each command, then apply the limit.
        //
        // For frequency/frecency sorts, we always need aggregation.
        let needs_full_aggregation = matches!(sort, SortMode::Frequency | SortMode::Frecency);

        // Build the query dynamically based on filters
        // Note: start_timestamp is stored in milliseconds by reedline
        // Use COALESCE to handle NULL timestamps (treat as very old: 0)
        let mut sql = if needs_full_aggregation {
            // Full aggregation for frequency/frecency - groups all occurrences.
            // Return NULL for cwd/exit_status/duration_ms since MAX() across different
            // executions would misrepresent the most recent run's metadata.
            String::from(
                "SELECT command_line, MAX(COALESCE(start_timestamp, 0)) as start_timestamp,
                 NULL as cwd, NULL as exit_status, NULL as duration_ms,
                 COUNT(*) as exec_count,
                 COALESCE(COUNT(*) * (1.0 / (1.0 + COALESCE((julianday('now') - julianday(datetime(MAX(COALESCE(start_timestamp, 0))/1000, 'unixepoch'))), 365) * 24)) * 100, 0.0) as frecency
                 FROM history WHERE 1=1"
            )
        } else if filter.dedupe {
            // Dedupe with recency: use ROW_NUMBER to get only the most recent of each command
            // This preserves the same ordering as non-dedupe but filters out older duplicates
            String::from(
                "WITH ranked AS (
                    SELECT command_line, COALESCE(start_timestamp, 0) as start_timestamp,
                           cwd, exit_status, duration_ms,
                           ROW_NUMBER() OVER (PARTITION BY command_line ORDER BY COALESCE(start_timestamp, 0) DESC) as rn
                    FROM history WHERE 1=1"
            )
        } else {
            // No aggregation - show all entries
            String::from(
                "SELECT command_line, COALESCE(start_timestamp, 0) as start_timestamp,
                 cwd, exit_status, duration_ms,
                 1 as exec_count,
                 1.0 as frecency
                 FROM history WHERE 1=1",
            )
        };
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        // Add search filter
        if !search.is_empty() {
            sql.push_str(" AND command_line LIKE ?");
            params.push(Box::new(format!("%{}%", search)));
        }

        // Add current directory filter
        if filter.current_dir_only {
            if let Some(cwd) = current_cwd {
                sql.push_str(" AND cwd = ?");
                params.push(Box::new(cwd.to_string_lossy().to_string()));
            }
        }

        // Add failed filter
        if filter.failed_only {
            sql.push_str(" AND exit_status IS NOT NULL AND exit_status != 0");
        }

        // Complete the query based on mode
        if needs_full_aggregation {
            sql.push_str(" GROUP BY command_line");
            match sort {
                SortMode::Frequency => {
                    sql.push_str(" ORDER BY exec_count DESC, start_timestamp DESC")
                }
                SortMode::Frecency => sql.push_str(" ORDER BY frecency DESC"),
                SortMode::Recency => sql.push_str(" ORDER BY start_timestamp DESC"),
            }
        } else if filter.dedupe {
            // Close the CTE and select only the most recent of each command
            sql.push_str(") SELECT command_line, start_timestamp, cwd, exit_status, duration_ms, 1 as exec_count, 1.0 as frecency FROM ranked WHERE rn = 1 ORDER BY start_timestamp DESC");
        } else {
            sql.push_str(" ORDER BY start_timestamp DESC");
        }

        sql.push_str(&format!(" LIMIT {}", limit));

        debug!(sql = %sql, "Executing history query");

        // Execute query
        let mut stmt = conn.prepare(&sql)?;

        // Convert params to references for binding
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            let command: String = row.get(0)?;
            let timestamp_ms: Option<i64> = row.get(1)?;
            let cwd: Option<String> = row.get(2)?;
            let exit_status: Option<i64> = row.get(3)?;
            let duration_ms: Option<i64> = row.get(4)?;
            let exec_count: Option<u32> = row.get(5)?;
            let frecency: Option<f64> = row.get(6)?;

            Ok(HistoryRecord {
                command,
                // start_timestamp is stored in milliseconds by reedline
                // 0 means no timestamp (migrated entry)
                timestamp: timestamp_ms.filter(|&ms| ms > 0).map(|ms| {
                    DateTime::from_timestamp(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
                        .unwrap_or_default()
                }),
                cwd: cwd.map(PathBuf::from),
                exit_status,
                duration: duration_ms.map(|ms| Duration::from_millis(ms as u64)),
                frecency_score: frecency.unwrap_or(0.0),
                execution_count: exec_count.unwrap_or(1),
            })
        })?;

        let mut records = Vec::new();
        let mut parse_errors = 0;
        for row in rows {
            match row {
                Ok(record) => records.push(record),
                Err(e) => {
                    parse_errors += 1;
                    debug!(error = %e, "Failed to parse history row");
                }
            }
        }
        if parse_errors > 0 {
            warn!(count = parse_errors, "Some history rows failed to parse");
        }
        debug!(count = records.len(), "Query returned records");

        Ok(records)
    }

    /// Clears all history entries from the database.
    ///
    /// This deletes all entries from the history table and vacuums the database
    /// to ensure data is completely erased. This is safer than deleting the file
    /// because other connections (like reedline's) remain valid.
    ///
    /// # Arguments
    ///
    /// * `confirmation` - Must be "wipe" to proceed
    ///
    /// # Errors
    ///
    /// Returns [`HistoryStoreError::ConfirmationRequired`] if confirmation doesn't match,
    /// or [`HistoryStoreError::Sqlite`] if the database operation fails.
    pub fn wipe(&self, confirmation: &str) -> Result<(), HistoryStoreError> {
        if confirmation != "wipe" {
            return Err(HistoryStoreError::ConfirmationRequired(
                "Confirmation must be 'wipe' to delete history",
            ));
        }

        // Open a connection with busy timeout and clear the table
        let conn = self.open_connection()?;

        // Delete all entries
        conn.execute("DELETE FROM history", [])?;

        // VACUUM to ensure data is completely erased and disk space is reclaimed
        conn.execute("VACUUM", [])?;

        info!(path = %self.db_path.display(), "History cleared");
        Ok(())
    }

    /// Deduplicates the SQLite history, removing consecutive duplicate commands.
    ///
    /// Keeps the most recent occurrence of each consecutive duplicate sequence.
    /// Non-consecutive duplicates are preserved (like HISTCONTROL=ignoredups).
    ///
    /// # Returns
    ///
    /// The number of duplicate entries removed.
    pub fn dedupe(&self) -> Result<usize, HistoryStoreError> {
        let conn = self.open_connection()?;

        // Find IDs of entries to delete (all but the last in each consecutive duplicate sequence)
        // This query finds entries where the next entry (by timestamp) has the same command
        let removed: usize = conn.execute(
            "DELETE FROM history WHERE id IN (
                SELECT h1.id FROM history h1
                INNER JOIN history h2 ON h1.command_line = h2.command_line
                WHERE h2.start_timestamp > h1.start_timestamp
                AND NOT EXISTS (
                    SELECT 1 FROM history h3
                    WHERE h3.start_timestamp > h1.start_timestamp
                    AND h3.start_timestamp < h2.start_timestamp
                    AND h3.command_line != h1.command_line
                )
            )",
            [],
        )?;

        if removed > 0 {
            // Reclaim space
            conn.execute("VACUUM", [])?;
            info!(removed, "Deduplicated SQLite history");
        }

        Ok(removed)
    }

    /// Deduplicates both SQLite and bash_history.
    ///
    /// # Returns
    ///
    /// A tuple of (sqlite_removed, bash_removed).
    pub fn dedupe_all(&self) -> Result<(usize, usize), HistoryStoreError> {
        let sqlite_removed = self.dedupe()?;

        let bash_removed = crate::history::dedupe_bash_history().map_err(|e| {
            HistoryStoreError::Internal(format!("bash_history dedupe failed: {}", e))
        })?;

        Ok((sqlite_removed, bash_removed))
    }

    /// Returns the path to the database file.
    pub fn db_path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Queries historical tokens at a specific position in commands.
    ///
    /// Returns tokens that appear after the specified prefix tokens,
    /// sorted by frequency (most common first).
    ///
    /// # Arguments
    ///
    /// * `preceding` - The tokens that precede the position being queried
    /// * `limit` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A vector of token strings sorted by frequency.
    pub fn tokens_at_position(
        &self,
        preceding: &[&str],
        limit: usize,
    ) -> Result<Vec<String>, HistoryStoreError> {
        let conn = self.open_connection()?;

        fn escape_like_prefix(input: &str) -> String {
            let mut escaped = String::with_capacity(input.len());
            for ch in input.chars() {
                match ch {
                    '%' | '_' | '\\' => {
                        escaped.push('\\');
                        escaped.push(ch);
                    }
                    _ => escaped.push(ch),
                }
            }
            escaped
        }

        // Build the prefix pattern for LIKE matching
        let prefix = if preceding.is_empty() {
            String::new()
        } else {
            format!("{} ", preceding.join(" "))
        };

        let escaped_prefix = escape_like_prefix(&prefix);
        let pattern = format!("{}%", escaped_prefix);

        // Query commands that match the prefix
        let mut stmt = conn.prepare(
            "SELECT command_line FROM history WHERE command_line LIKE ?1 ESCAPE '\\' LIMIT 1000",
        )?;

        let rows = stmt.query_map([&pattern], |row| {
            let cmd: String = row.get(0)?;
            Ok(cmd)
        })?;

        // Extract next token after prefix and count frequencies
        let mut token_counts: HashMap<String, u32> = HashMap::new();
        let prefix_len = prefix.len();

        for row in rows.flatten() {
            if row.len() <= prefix_len {
                continue;
            }

            // Get the remainder after the prefix
            let remainder = &row[prefix_len..];

            // Extract the first token (up to next space or end)
            let next_token = remainder.split_whitespace().next().map(|s| s.to_string());

            if let Some(token) = next_token {
                if !token.is_empty() {
                    *token_counts.entry(token).or_insert(0) += 1;
                }
            }
        }

        // Sort by frequency (descending)
        let mut tokens: Vec<(String, u32)> = token_counts.into_iter().collect();
        tokens.sort_by(|a, b| b.1.cmp(&a.1));

        // Return just the tokens, limited
        Ok(tokens.into_iter().take(limit).map(|(t, _)| t).collect())
    }

    // ========================================================================
    // Command Intelligence Integration
    // ========================================================================

    /// Starts an intelligence session.
    ///
    /// Should be called when the terminal opens.
    pub fn start_intelligence_session(&mut self, session_id: &str) {
        if let Some(ref mut ci) = self.intelligence {
            if let Err(e) = ci.start_session(session_id) {
                warn!("Failed to start intelligence session: {}", e);
            }
        }
    }

    /// Ends the intelligence session.
    ///
    /// Should be called when the terminal closes.
    pub fn end_intelligence_session(&mut self) {
        if let Some(ref mut ci) = self.intelligence {
            if let Err(e) = ci.end_session() {
                warn!("Failed to end intelligence session: {}", e);
            }
        }
    }

    /// Syncs intelligence with history.
    ///
    /// Should be called periodically to learn from new history entries.
    pub fn sync_intelligence(&mut self) {
        if let Some(ref mut ci) = self.intelligence {
            match ci.sync() {
                Ok(stats) => {
                    if stats.commands_processed > 0 {
                        debug!(
                            commands = stats.commands_processed,
                            tokens = stats.tokens_extracted,
                            "Intelligence synced"
                        );
                    }
                }
                Err(e) => {
                    debug!("Intelligence sync skipped: {}", e);
                }
            }
        }
    }

    /// Records a command submission for later learning.
    ///
    /// Should be called when the user submits a command.
    pub fn record_command_submission(&mut self, command: &str) {
        self.last_command_text = Some(command.to_string());
    }

    /// Saves a command to both SQLite history and ~/.bash_history.
    ///
    /// This method should be called for commands that bypass reedline's normal
    /// input flow (e.g., commands executed from the panel). It ensures the command
    /// appears in:
    /// - wrashpty's SQLite history (for reedline Up/Down navigation)
    /// - ~/.bash_history (for other bash sessions)
    ///
    /// # Arguments
    ///
    /// * `command` - The command to save
    /// * `cwd` - Optional current working directory
    ///
    /// # Returns
    ///
    /// The history item ID if successfully saved to SQLite.
    pub fn save_command(
        &mut self,
        command: &str,
        cwd: Option<&PathBuf>,
    ) -> Result<Option<HistoryItemId>, HistoryStoreError> {
        // Skip empty commands
        if command.trim().is_empty() {
            return Ok(None);
        }

        // Save to SQLite history
        let conn = self.open_connection()?;
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let cwd_str = cwd.map(|p| p.to_string_lossy().to_string());

        conn.execute(
            "INSERT INTO history (command_line, start_timestamp, cwd) VALUES (?1, ?2, ?3)",
            rusqlite::params![command, timestamp_ms, cwd_str],
        )?;

        let id = conn.last_insert_rowid();
        debug!(
            id,
            command_len = command.len(),
            "Saved command to SQLite history"
        );

        // Also record for intelligence learning
        self.last_command_text = Some(command.to_string());

        // Append to bash_history for other sessions
        if let Err(e) = crate::history::append_to_bash_history(command) {
            warn!("Failed to append to bash_history: {}", e);
            // Don't fail the whole operation - SQLite save succeeded
        }

        Ok(Some(HistoryItemId::new(id)))
    }

    /// Appends a command to ~/.bash_history only.
    ///
    /// Use this for commands that are already saved to SQLite by reedline
    /// but need to be synced to bash_history for other sessions.
    ///
    /// # Arguments
    ///
    /// * `command` - The command to append
    pub fn sync_to_bash_history(&self, command: &str) {
        if command.trim().is_empty() {
            return;
        }

        if let Err(e) = crate::history::append_to_bash_history(command) {
            warn!("Failed to sync to bash_history: {}", e);
        }
    }

    /// Learns from command completion.
    ///
    /// Should be called when a command finishes executing with its exit status.
    pub fn learn_command_completion(&mut self, exit_status: Option<i32>) {
        let command = match self.last_command_text.take() {
            Some(cmd) => cmd,
            None => return,
        };

        if let Some(ref mut ci) = self.intelligence {
            if let Err(e) = ci.learn_command(&command, exit_status) {
                debug!("Failed to learn from command: {}", e);
            }
        }
    }

    /// Gets intelligent suggestions for the given context.
    /// Gets intelligent suggestions for the current editing context.
    ///
    /// Uses the unified command hierarchy as the primary source, providing
    /// position-aware token suggestions that naturally adapt to any command.
    ///
    /// # Arguments
    ///
    /// * `tokens` - The tokens preceding the current edit position
    /// * `partial` - The partial text being typed
    /// * `cwd` - Current working directory
    /// * `file_context` - File context if in file browser
    /// * `last_command` - Last executed command for transition suggestions
    pub fn intelligent_suggest(
        &self,
        tokens: &[CommandToken],
        partial: &str,
        cwd: Option<PathBuf>,
        file_context: Option<FileContext>,
        last_command: Option<String>,
    ) -> Vec<Suggestion> {
        let Some(ref ci) = self.intelligence else {
            return Vec::new();
        };

        let session = ci.current_session().cloned();
        let context = build_context(tokens, partial, cwd, file_context, session, last_command);

        ci.suggest(&context, 20)
    }

    /// Expands an alias if it exists.
    pub fn expand_alias(&self, text: &str) -> Option<String> {
        self.intelligence
            .as_ref()
            .and_then(|ci| ci.expand_alias(text))
    }

    /// Returns whether intelligence is available and enabled.
    pub fn is_intelligence_enabled(&self) -> bool {
        self.intelligence
            .as_ref()
            .map(|ci| ci.is_enabled())
            .unwrap_or(false)
    }

    /// Returns whether intelligence is available and enabled.
    #[deprecated(note = "Use is_intelligence_enabled() instead")]
    pub fn has_intelligence(&self) -> bool {
        self.is_intelligence_enabled()
    }

    /// Enables or disables intelligence.
    ///
    /// The setting is persisted to the database on a best-effort basis.
    pub fn set_intelligence_enabled(&mut self, enabled: bool) {
        if let Some(ref mut ci) = self.intelligence {
            ci.set_enabled(enabled);
        }

        // Persist the setting
        let value = if enabled { "true" } else { "false" };
        if let Err(e) = self.set_setting("intelligence.enabled", value) {
            warn!("Failed to persist intelligence.enabled setting: {}", e);
        }
    }

    /// Resets the intelligence database, deleting all learned patterns.
    ///
    /// This drops and recreates all `ci_*` tables, giving a clean slate.
    /// Returns an error if intelligence is not available.
    pub fn reset_intelligence(&mut self) -> Result<(), HistoryStoreError> {
        if let Some(ref mut ci) = self.intelligence {
            ci.reset().map_err(|e| {
                HistoryStoreError::internal(format!("Intelligence reset failed: {}", e))
            })
        } else {
            Err(HistoryStoreError::internal("Intelligence not available"))
        }
    }

    /// Loads intelligence enabled state from settings.
    ///
    /// Should be called after initialization to restore user preference.
    /// Intelligence defaults to disabled unless explicitly set to "true".
    pub fn load_intelligence_settings(&mut self) {
        if let Ok(Some(value)) = self.get_setting("intelligence.enabled") {
            // Explicit opt-in: only enable if exactly "true"
            let enabled = value == "true";
            if !enabled && value != "false" {
                // Log invalid values but don't enable
                debug!(
                    setting_value = %value,
                    "Invalid intelligence.enabled setting value, defaulting to disabled"
                );
            }
            if let Some(ref mut ci) = self.intelligence {
                ci.set_enabled(enabled);
            }
        }
        // If setting is missing, intelligence remains in its default state (disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_mode_next_cycles_through_all_modes() {
        assert_eq!(SortMode::Recency.next(), SortMode::Frequency);
        assert_eq!(SortMode::Frequency.next(), SortMode::Frecency);
        assert_eq!(SortMode::Frecency.next(), SortMode::Recency);
    }

    #[test]
    fn test_sort_mode_name_returns_display_strings() {
        assert_eq!(SortMode::Recency.name(), "Recent");
        assert_eq!(SortMode::Frequency.name(), "Frequent");
        assert_eq!(SortMode::Frecency.name(), "Frecent");
    }

    #[test]
    fn test_filter_mode_default_all_flags_false() {
        let filter = FilterMode::default();
        assert!(!filter.dedupe);
        assert!(!filter.current_dir_only);
        assert!(!filter.failed_only);
    }
}
