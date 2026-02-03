//! History store with SQLite-backed storage and rich metadata.
//!
//! This module provides a centralized history store that wraps reedline's
//! `SqliteBackedHistory` and provides rich query capabilities for the history panel.
//! The SQLite database is shared between reedline (for line editing) and the panel
//! (for metadata-rich browsing).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use reedline::{History, HistoryItem, HistoryItemId, SqliteBackedHistory};
use rusqlite::Connection;
use tracing::{debug, info, warn};

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
}

impl HistoryStore {
    /// Creates a new history store with the given session token.
    ///
    /// Creates the data directory at `~/.local/share/wrashpty/` if needed,
    /// initializes the SQLite database, and performs first-run migration
    /// from `~/.bash_history`.
    pub fn new(_session_token: [u8; 16]) -> Result<Self> {
        // Create data directory
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("wrashpty");

        std::fs::create_dir_all(&data_dir)
            .context("Failed to create wrashpty data directory")?;

        let db_path = data_dir.join("history.db");

        // Check if this is a first run (database doesn't exist)
        let is_first_run = !db_path.exists();

        // Create the reedline history instance
        // Pass None for session_id to use all sessions, and None for timestamp retention
        let reedline_history = SqliteBackedHistory::with_file(
            db_path.clone(),
            None,  // No session filtering
            None,  // No timestamp retention filtering
        )
        .context("Failed to create SQLite history")?;

        let mut store = Self {
            db_path,
            last_command_id: None,
            reedline_history: Some(reedline_history),
        };

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

    /// Migrates history entries from ~/.bash_history to the SQLite database.
    fn migrate_from_bash_history(&mut self) -> Result<()> {
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
    /// Returns an error if creating a new SQLite history connection fails.
    pub fn create_reedline_history(&mut self) -> Result<Box<dyn History>> {
        if let Some(history) = self.reedline_history.take() {
            Ok(Box::new(history))
        } else {
            // Create a new instance if already taken
            let history = SqliteBackedHistory::with_file(
                self.db_path.clone(),
                None,
                None,
            )
            .context("Failed to create SQLite history")?;
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

    /// Updates metadata for the last executed command.
    ///
    /// # Arguments
    ///
    /// * `exit_status` - Exit code of the command
    /// * `duration` - How long the command took to execute
    /// * `cwd` - Working directory where the command was run
    pub fn update_last_command(
        &mut self,
        exit_status: Option<i32>,
        duration: Option<Duration>,
        cwd: Option<PathBuf>,
    ) -> Result<()> {
        // Open a direct connection to update metadata
        let conn = Connection::open(&self.db_path)
            .context("Failed to open history database")?;

        // Get the ID of the most recent command
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

        // Update the entry with metadata
        let duration_ms = duration.map(|d| d.as_millis() as i64);
        let cwd_str = cwd.map(|p| p.to_string_lossy().to_string());

        conn.execute(
            "UPDATE history SET exit_status = ?1, duration_ms = ?2, cwd = ?3 WHERE id = ?4",
            rusqlite::params![exit_status, duration_ms, cwd_str, id],
        )
        .context("Failed to update history metadata")?;

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
    pub fn query(
        &self,
        search: &str,
        filter: &FilterMode,
        sort: &SortMode,
        current_cwd: Option<&PathBuf>,
        limit: usize,
    ) -> Result<Vec<HistoryRecord>> {
        let conn = Connection::open(&self.db_path)
            .context("Failed to open history database")?;

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
            // Full aggregation for frequency/frecency - groups all occurrences
            String::from(
                "SELECT command_line, MAX(COALESCE(start_timestamp, 0)) as start_timestamp,
                 MAX(cwd) as cwd, MAX(exit_status) as exit_status, MAX(duration_ms) as duration_ms,
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
                 FROM history WHERE 1=1"
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
                SortMode::Frequency => sql.push_str(" ORDER BY exec_count DESC, start_timestamp DESC"),
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
    /// Returns an error if confirmation doesn't match or clearing fails.
    pub fn wipe(&self, confirmation: &str) -> Result<()> {
        if confirmation != "wipe" {
            anyhow::bail!("Confirmation must be 'wipe' to delete history");
        }

        // Open a connection and clear the table
        let conn = Connection::open(&self.db_path)
            .context("Failed to open history database")?;

        // Delete all entries
        conn.execute("DELETE FROM history", [])
            .context("Failed to clear history table")?;

        // VACUUM to ensure data is completely erased and disk space is reclaimed
        conn.execute("VACUUM", [])
            .context("Failed to vacuum database")?;

        info!(path = %self.db_path.display(), "History cleared");
        Ok(())
    }

    /// Returns the path to the database file.
    pub fn db_path(&self) -> &PathBuf {
        &self.db_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_mode_next() {
        assert_eq!(SortMode::Recency.next(), SortMode::Frequency);
        assert_eq!(SortMode::Frequency.next(), SortMode::Frecency);
        assert_eq!(SortMode::Frecency.next(), SortMode::Recency);
    }

    #[test]
    fn test_sort_mode_name() {
        assert_eq!(SortMode::Recency.name(), "Recent");
        assert_eq!(SortMode::Frequency.name(), "Frequent");
        assert_eq!(SortMode::Frecency.name(), "Frecent");
    }

    #[test]
    fn test_filter_mode_default() {
        let filter = FilterMode::default();
        assert!(!filter.dedupe);
        assert!(!filter.current_dir_only);
        assert!(!filter.failed_only);
    }
}
