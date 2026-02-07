//! Database schema creation and migration for the Command Intelligence Engine.

use rusqlite::Connection;
use tracing::{debug, info};

use super::error::CIError;

/// Current schema version.
pub const SCHEMA_VERSION: i32 = 2;

/// Creates the Command Intelligence schema in the given database connection.
///
/// All tables use the `ci_` prefix to avoid conflicts with reedline's tables.
pub fn create_schema(conn: &Connection) -> Result<(), CIError> {
    info!(
        "Creating Command Intelligence schema (version {})",
        SCHEMA_VERSION
    );

    // Check current schema version
    let current_version = get_schema_version(conn)?;
    if current_version == SCHEMA_VERSION {
        debug!("Schema is up to date");
        return Ok(());
    }

    if current_version > SCHEMA_VERSION {
        return Err(CIError::SchemaVersion {
            expected: SCHEMA_VERSION,
            found: current_version,
        });
    }

    // Create tables in a transaction
    conn.execute_batch("BEGIN TRANSACTION")?;

    // Create all tables
    if let Err(e) = create_tables(conn) {
        conn.execute_batch("ROLLBACK")?;
        return Err(e);
    }

    // Apply incremental migrations for existing databases.
    if let Err(e) = apply_migrations(conn, current_version, SCHEMA_VERSION) {
        conn.execute_batch("ROLLBACK")?;
        return Err(e);
    }

    // Set schema version - only commit if this succeeds too
    if let Err(e) = set_schema_version(conn, SCHEMA_VERSION) {
        conn.execute_batch("ROLLBACK")?;
        return Err(e);
    }

    // Both succeeded, commit the transaction
    conn.execute_batch("COMMIT")?;
    info!("Schema created successfully");

    Ok(())
}

/// Applies schema migrations from one version to another.
fn apply_migrations(conn: &Connection, from_version: i32, to_version: i32) -> Result<(), CIError> {
    if from_version < 2 && to_version >= 2 {
        // Phase 8b cutoff: purge legacy non-authoritative schema rows created by
        // historical bootstrap/help probing paths. Learned rows are preserved.
        let deleted = conn.execute(
            "DELETE FROM ci_command_schemas WHERE source IN ('bootstrap', 'help')",
            [],
        )?;
        debug!(
            deleted,
            "Applied schema v2 migration: purged bootstrap/help command schemas"
        );
    }

    Ok(())
}

/// Gets the current schema version.
fn get_schema_version(conn: &Connection) -> Result<i32, CIError> {
    // Create version table if it doesn't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_schema_version (
            version INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
        [],
    )?;

    let version: Option<i32> = conn
        .query_row(
            "SELECT version FROM ci_schema_version ORDER BY version DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();

    Ok(version.unwrap_or(0))
}

/// Sets the schema version.
fn set_schema_version(conn: &Connection, version: i32) -> Result<(), CIError> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO ci_schema_version (version, applied_at) VALUES (?1, ?2)",
        rusqlite::params![version, now],
    )?;
    Ok(())
}

/// Creates all intelligence tables.
fn create_tables(conn: &Connection) -> Result<(), CIError> {
    // Sync state tracking
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_sync_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )?;

    // Token vocabulary
    create_tokens_table(conn)?;

    // Processed commands
    create_commands_table(conn)?;

    // Pattern learning tables
    create_sequence_tables(conn)?;
    create_pipe_tables(conn)?;
    create_flag_tables(conn)?;

    // Command hierarchy table (unified learning)
    create_hierarchy_table(conn)?;

    // Session tracking tables
    create_session_tables(conn)?;

    // Template tables
    create_template_tables(conn)?;

    // Failure learning table
    create_variants_table(conn)?;

    // FTS5 full-text search
    create_fts_tables(conn)?;

    // User pattern tables
    create_user_pattern_tables(conn)?;

    // Suggestion cache
    create_cache_table(conn)?;

    // Command schemas (extracted from --help)
    create_command_schema_tables(conn)?;

    Ok(())
}

/// Creates command schema storage tables.
fn create_command_schema_tables(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_command_schemas (
            id INTEGER PRIMARY KEY,
            command TEXT NOT NULL,
            subcommand TEXT,
            schema_json TEXT NOT NULL,
            source TEXT NOT NULL,
            confidence REAL DEFAULT 1.0,
            extracted_at INTEGER NOT NULL,
            last_validated INTEGER,
            UNIQUE(command, subcommand)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_schema_command ON ci_command_schemas(command)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_schema_source ON ci_command_schemas(source)",
        [],
    )?;

    debug!("Created command schema tables");
    Ok(())
}

/// Creates the token vocabulary table.
fn create_tokens_table(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_tokens (
            id INTEGER PRIMARY KEY,
            text TEXT NOT NULL UNIQUE,
            token_type TEXT NOT NULL,
            frequency INTEGER DEFAULT 1,
            first_seen INTEGER NOT NULL,
            last_seen INTEGER NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_tokens_text ON ci_tokens(text)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_tokens_type ON ci_tokens(token_type)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_tokens_freq ON ci_tokens(frequency DESC)",
        [],
    )?;

    debug!("Created ci_tokens table");
    Ok(())
}

/// Creates the commands table.
fn create_commands_table(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_commands (
            id INTEGER PRIMARY KEY,
            reedline_id INTEGER UNIQUE,
            command_line TEXT NOT NULL,
            command_hash TEXT NOT NULL,
            token_ids TEXT NOT NULL,
            token_count INTEGER NOT NULL,
            base_command_id INTEGER,
            exit_status INTEGER,
            cwd TEXT,
            timestamp INTEGER NOT NULL,
            session_id INTEGER,
            FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (session_id) REFERENCES ci_sessions(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_commands_hash ON ci_commands(command_hash)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_commands_base ON ci_commands(base_command_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_commands_ts ON ci_commands(timestamp DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_commands_session ON ci_commands(session_id)",
        [],
    )?;

    debug!("Created ci_commands table");
    Ok(())
}

/// Creates sequence learning tables.
fn create_sequence_tables(conn: &Connection) -> Result<(), CIError> {
    // Token sequences
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_sequences (
            id INTEGER PRIMARY KEY,
            context_token_id INTEGER NOT NULL,
            context_position INTEGER NOT NULL,
            base_command_id INTEGER,
            next_token_id INTEGER NOT NULL,
            frequency INTEGER DEFAULT 1,
            success_count INTEGER DEFAULT 0,
            last_seen INTEGER NOT NULL,
            UNIQUE(context_token_id, context_position, base_command_id, next_token_id),
            FOREIGN KEY (context_token_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (next_token_id) REFERENCES ci_tokens(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_seq_context ON ci_sequences(context_token_id, base_command_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_seq_freq ON ci_sequences(frequency DESC)",
        [],
    )?;

    debug!("Created sequence tables");
    Ok(())
}

/// Creates pipe chain tables.
fn create_pipe_tables(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_pipe_chains (
            id INTEGER PRIMARY KEY,
            pre_pipe_base_cmd_id INTEGER,
            pre_pipe_hash TEXT NOT NULL,
            pipe_command_id INTEGER NOT NULL,
            full_chain TEXT,
            chain_length INTEGER DEFAULT 1,
            frequency INTEGER DEFAULT 1,
            last_seen INTEGER NOT NULL,
            UNIQUE(pre_pipe_hash, pipe_command_id),
            FOREIGN KEY (pre_pipe_base_cmd_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (pipe_command_id) REFERENCES ci_tokens(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_pipes_hash ON ci_pipe_chains(pre_pipe_hash)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_pipes_freq ON ci_pipe_chains(frequency DESC)",
        [],
    )?;

    debug!("Created pipe chain tables");
    Ok(())
}

/// Creates flag value tables.
fn create_flag_tables(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_flag_values (
            id INTEGER PRIMARY KEY,
            base_command_id INTEGER NOT NULL,
            subcommand_id INTEGER,
            flag_text TEXT NOT NULL,
            value_text TEXT NOT NULL,
            value_type TEXT,
            frequency INTEGER DEFAULT 1,
            last_seen INTEGER NOT NULL,
            UNIQUE(base_command_id, subcommand_id, flag_text, value_text),
            FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (subcommand_id) REFERENCES ci_tokens(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_flags_cmd ON ci_flag_values(base_command_id, flag_text)",
        [],
    )?;

    debug!("Created flag value tables");
    Ok(())
}

/// Creates the command hierarchy table for unified position-aware learning.
fn create_hierarchy_table(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_command_hierarchy (
            id INTEGER PRIMARY KEY,

            -- The token that appears at this position
            token_id INTEGER NOT NULL,

            -- Position in command (0 = base, 1 = subcommand, etc.)
            position INTEGER NOT NULL,

            -- Parent token ID (NULL for position 0)
            parent_token_id INTEGER,

            -- Base command ID (for fast filtering)
            base_command_id INTEGER,

            -- Statistics
            frequency INTEGER DEFAULT 1,
            success_count INTEGER DEFAULT 0,
            last_seen INTEGER NOT NULL,

            -- Semantic classification learned over time
            role TEXT,

            UNIQUE(token_id, position, parent_token_id, base_command_id),
            FOREIGN KEY (token_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (parent_token_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_hierarchy_parent ON ci_command_hierarchy(parent_token_id, position)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_hierarchy_base ON ci_command_hierarchy(base_command_id, position)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_hierarchy_lookup ON ci_command_hierarchy(position, parent_token_id, base_command_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_hierarchy_freq ON ci_command_hierarchy(frequency DESC)",
        [],
    )?;

    debug!("Created command hierarchy table");
    Ok(())
}

/// Creates session tracking tables.
fn create_session_tables(conn: &Connection) -> Result<(), CIError> {
    // Terminal sessions
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_sessions (
            id INTEGER PRIMARY KEY,
            session_id TEXT NOT NULL UNIQUE,
            start_time INTEGER NOT NULL,
            end_time INTEGER,
            command_count INTEGER DEFAULT 0,
            cwd_at_start TEXT
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_sessions_uuid ON ci_sessions(session_id)",
        [],
    )?;

    // Commands within sessions
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_session_commands (
            id INTEGER PRIMARY KEY,
            session_id INTEGER NOT NULL,
            sequence_number INTEGER NOT NULL,
            command_id INTEGER NOT NULL,
            timestamp INTEGER NOT NULL,
            UNIQUE(session_id, sequence_number),
            FOREIGN KEY (session_id) REFERENCES ci_sessions(id),
            FOREIGN KEY (command_id) REFERENCES ci_commands(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_session_cmds ON ci_session_commands(session_id, sequence_number)",
        [],
    )?;

    // Command transitions
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_transitions (
            id INTEGER PRIMARY KEY,
            from_command_hash TEXT NOT NULL,
            to_command_hash TEXT NOT NULL,
            from_base_cmd_id INTEGER,
            to_base_cmd_id INTEGER,
            frequency INTEGER DEFAULT 1,
            avg_time_delta INTEGER,
            last_seen INTEGER NOT NULL,
            UNIQUE(from_command_hash, to_command_hash),
            FOREIGN KEY (from_base_cmd_id) REFERENCES ci_tokens(id),
            FOREIGN KEY (to_base_cmd_id) REFERENCES ci_tokens(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_trans_from ON ci_transitions(from_command_hash)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_trans_base ON ci_transitions(from_base_cmd_id)",
        [],
    )?;

    debug!("Created session tables");
    Ok(())
}

/// Creates template tables.
fn create_template_tables(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_templates (
            id INTEGER PRIMARY KEY,
            template TEXT NOT NULL UNIQUE,
            template_hash TEXT NOT NULL,
            base_command_id INTEGER,
            placeholder_count INTEGER NOT NULL,
            placeholders TEXT NOT NULL,
            frequency INTEGER DEFAULT 1,
            last_seen INTEGER NOT NULL,
            example_command TEXT,
            FOREIGN KEY (base_command_id) REFERENCES ci_tokens(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_templates_hash ON ci_templates(template_hash)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_templates_cmd ON ci_templates(base_command_id)",
        [],
    )?;

    // Template values
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_template_values (
            id INTEGER PRIMARY KEY,
            template_id INTEGER NOT NULL,
            placeholder_name TEXT NOT NULL,
            value_text TEXT NOT NULL,
            value_type TEXT,
            frequency INTEGER DEFAULT 1,
            last_seen INTEGER NOT NULL,
            UNIQUE(template_id, placeholder_name, value_text),
            FOREIGN KEY (template_id) REFERENCES ci_templates(id)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_tpl_vals ON ci_template_values(template_id, placeholder_name)",
        [],
    )?;

    debug!("Created template tables");
    Ok(())
}

/// Creates command variants table for failure learning.
fn create_variants_table(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_command_variants (
            id INTEGER PRIMARY KEY,
            canonical_pattern TEXT NOT NULL,
            variant_hash TEXT NOT NULL,
            variant_command TEXT NOT NULL,
            success_count INTEGER DEFAULT 0,
            failure_count INTEGER DEFAULT 0,
            last_success INTEGER,
            last_failure INTEGER,
            success_rate REAL GENERATED ALWAYS AS (
                CAST(success_count AS REAL) / NULLIF(success_count + failure_count, 0)
            ) STORED,
            UNIQUE(canonical_pattern, variant_hash)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_variants_pattern ON ci_command_variants(canonical_pattern)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_variants_success ON ci_command_variants(success_rate DESC)",
        [],
    )?;

    debug!("Created variants table");
    Ok(())
}

/// Creates FTS5 full-text search tables.
fn create_fts_tables(conn: &Connection) -> Result<(), CIError> {
    // Create FTS5 virtual table
    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS ci_commands_fts USING fts5(
            command_line,
            base_command,
            content='ci_commands',
            content_rowid='id',
            tokenize='porter unicode61'
        )",
        [],
    )?;

    // Triggers to keep FTS in sync
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS ci_commands_fts_ai AFTER INSERT ON ci_commands BEGIN
            INSERT INTO ci_commands_fts(rowid, command_line, base_command)
            SELECT new.id, new.command_line,
                   (SELECT text FROM ci_tokens WHERE id = new.base_command_id);
        END",
        [],
    )?;

    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS ci_commands_fts_ad AFTER DELETE ON ci_commands BEGIN
            INSERT INTO ci_commands_fts(ci_commands_fts, rowid, command_line, base_command)
            VALUES('delete', old.id, old.command_line,
                   (SELECT text FROM ci_tokens WHERE id = old.base_command_id));
        END",
        [],
    )?;

    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS ci_commands_fts_au AFTER UPDATE ON ci_commands BEGIN
            INSERT INTO ci_commands_fts(ci_commands_fts, rowid, command_line, base_command)
            VALUES('delete', old.id, old.command_line,
                   (SELECT text FROM ci_tokens WHERE id = old.base_command_id));
            INSERT INTO ci_commands_fts(rowid, command_line, base_command)
            SELECT new.id, new.command_line,
                   (SELECT text FROM ci_tokens WHERE id = new.base_command_id);
        END",
        [],
    )?;

    debug!("Created FTS5 tables and triggers");
    Ok(())
}

/// Creates user pattern tables.
fn create_user_pattern_tables(conn: &Connection) -> Result<(), CIError> {
    // User patterns
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_user_patterns (
            id INTEGER PRIMARY KEY,
            pattern_type TEXT NOT NULL,
            trigger_pattern TEXT NOT NULL,
            suggestion TEXT NOT NULL,
            description TEXT,
            priority INTEGER DEFAULT 0,
            enabled INTEGER DEFAULT 1,
            created_at INTEGER NOT NULL,
            last_used INTEGER,
            use_count INTEGER DEFAULT 0
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_user_patterns_trigger ON ci_user_patterns(trigger_pattern, enabled)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_user_patterns_type ON ci_user_patterns(pattern_type, enabled)",
        [],
    )?;

    // User aliases
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_user_aliases (
            id INTEGER PRIMARY KEY,
            alias TEXT NOT NULL UNIQUE,
            expansion TEXT NOT NULL,
            description TEXT,
            enabled INTEGER DEFAULT 1,
            created_at INTEGER NOT NULL,
            last_used INTEGER,
            use_count INTEGER DEFAULT 0
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_user_aliases_name ON ci_user_aliases(alias, enabled)",
        [],
    )?;

    debug!("Created user pattern tables");
    Ok(())
}

/// Resets the database by dropping and recreating all ci_* tables.
///
/// This is useful for starting fresh without any learned patterns.
/// WARNING: This will delete all learned data including user patterns.
pub fn reset_database(conn: &Connection) -> Result<(), CIError> {
    info!("Resetting Command Intelligence database");

    // Drop all ci_* tables in reverse dependency order
    let tables = [
        // First drop tables with foreign keys
        "ci_session_commands",
        "ci_template_values",
        "ci_transitions",
        "ci_sequences",
        "ci_pipe_chains",
        "ci_flag_values",
        "ci_command_hierarchy",
        "ci_command_variants",
        "ci_suggestion_cache",
        // Drop FTS triggers first
        "ci_commands_fts_ai",
        "ci_commands_fts_ad",
        "ci_commands_fts_au",
        // Then parent tables
        "ci_commands",
        "ci_commands_fts",
        "ci_sessions",
        "ci_templates",
        "ci_tokens",
        "ci_user_patterns",
        "ci_user_aliases",
        "ci_sync_state",
        "ci_schema_version",
        "ci_command_schemas",
    ];

    conn.execute_batch("BEGIN TRANSACTION")?;

    for table in &tables {
        // Try as trigger first, then table
        let _ = conn.execute(&format!("DROP TRIGGER IF EXISTS {}", table), []);
        let _ = conn.execute(&format!("DROP TABLE IF EXISTS {}", table), []);
    }

    conn.execute_batch("COMMIT")?;

    // Recreate the schema
    create_schema(conn)?;

    info!("Database reset complete");
    Ok(())
}

/// Creates suggestion cache table.
fn create_cache_table(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_suggestion_cache (
            cache_key TEXT PRIMARY KEY,
            suggestions TEXT NOT NULL,
            computed_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_ci_cache_expires ON ci_suggestion_cache(expires_at)",
        [],
    )?;

    debug!("Created cache table");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_schema() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        // Verify tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'ci_%'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"ci_tokens".to_string()));
        assert!(tables.contains(&"ci_commands".to_string()));
        assert!(tables.contains(&"ci_sequences".to_string()));
        assert!(tables.contains(&"ci_sessions".to_string()));
        assert!(tables.contains(&"ci_templates".to_string()));
    }

    #[test]
    fn test_schema_version() {
        let conn = Connection::open_in_memory().unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, 0);

        set_schema_version(&conn, 1).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn test_idempotent_schema_creation() {
        let conn = Connection::open_in_memory().unwrap();

        // Create schema twice - should not error
        create_schema(&conn).unwrap();
        create_schema(&conn).unwrap();
    }

    #[test]
    fn test_migration_v2_purges_bootstrap_and_help_schema_rows() {
        let conn = Connection::open_in_memory().unwrap();

        conn.execute(
            "CREATE TABLE ci_schema_version (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ci_schema_version (version, applied_at) VALUES (1, 0)",
            [],
        )
        .unwrap();

        conn.execute(
            "CREATE TABLE ci_command_schemas (
                id INTEGER PRIMARY KEY,
                command TEXT NOT NULL,
                subcommand TEXT,
                schema_json TEXT NOT NULL,
                source TEXT NOT NULL,
                confidence REAL DEFAULT 1.0,
                extracted_at INTEGER NOT NULL,
                last_validated INTEGER,
                UNIQUE(command, subcommand)
            )",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO ci_command_schemas (command, subcommand, schema_json, source, confidence, extracted_at)
             VALUES ('git', NULL, '{}', 'bootstrap', 1.0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ci_command_schemas (command, subcommand, schema_json, source, confidence, extracted_at)
             VALUES ('kubectl', NULL, '{}', 'help', 1.0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ci_command_schemas (command, subcommand, schema_json, source, confidence, extracted_at)
             VALUES ('custom-tool', NULL, '{}', 'learned', 1.0, 0)",
            [],
        )
        .unwrap();

        create_schema(&conn).unwrap();

        let sources: Vec<String> = conn
            .prepare("SELECT source FROM ci_command_schemas ORDER BY command")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|row| row.ok())
            .collect();
        assert_eq!(sources, vec!["learned".to_string()]);

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
