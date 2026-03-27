//! Bootstrap module for seeding initial command knowledge.
//!
//! This module seeds the command hierarchy table from the schema provider
//! on first run. After bootstrap, learned data takes over for ranking.

use rusqlite::{Connection, OptionalExtension};
use tracing::{debug, info};

use command_schema_core::SubcommandSchema;

use super::error::CIError;
use super::schema_provider::SchemaProvider;

/// Seeds the command hierarchy from schema provider data if bootstrap has not run.
///
/// This should be called during CommandIntelligence initialization.
/// Uses `INSERT OR IGNORE` to merge bootstrap data with existing learned data
/// without overwriting user usage patterns.
pub fn bootstrap_if_empty(conn: &Connection, provider: &dyn SchemaProvider) -> Result<(), CIError> {
    let bootstrapped: bool = conn
        .query_row(
            "SELECT value = 'true' FROM ci_sync_state WHERE key = 'bootstrap.completed'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);

    if bootstrapped {
        debug!("Bootstrap already completed, skipping");
        return Ok(());
    }

    info!("Bootstrapping command hierarchy from schema provider");
    seed_command_knowledge(conn, provider)?;

    conn.execute(
        "INSERT OR REPLACE INTO ci_sync_state (key, value) VALUES ('bootstrap.completed', 'true')",
        [],
    )?;

    info!("Bootstrap completed");
    Ok(())
}

/// Seeds hierarchy knowledge from schema provider in a single transaction.
fn seed_command_knowledge(conn: &Connection, provider: &dyn SchemaProvider) -> Result<(), CIError> {
    let now = chrono::Utc::now().timestamp();

    let tx = conn.unchecked_transaction()?;
    seed_from_provider(&tx, provider, now)?;
    tx.commit()?;
    Ok(())
}

/// Seeds all known commands and nested subcommands from schema provider.
fn seed_from_provider(
    conn: &Connection,
    provider: &dyn SchemaProvider,
    timestamp: i64,
) -> Result<(), CIError> {
    let mut schemas: Vec<_> = provider.all_schemas().collect();
    schemas.sort_by(|a, b| a.command.cmp(&b.command));

    for schema in &schemas {
        let base_id = seed_base_command(conn, &schema.command, timestamp)?;
        seed_subcommands_recursive(conn, base_id, base_id, &schema.subcommands, 1, timestamp)?;
    }

    debug!(
        commands = schemas.len(),
        "Seeded hierarchy from schema provider"
    );
    Ok(())
}

/// Seeds nested subcommands recursively, preserving hierarchy depth.
fn seed_subcommands_recursive(
    conn: &Connection,
    parent_token_id: i64,
    base_command_id: i64,
    subcommands: &[SubcommandSchema],
    position: usize,
    timestamp: i64,
) -> Result<(), CIError> {
    for subcommand in subcommands {
        let token_id = get_or_create_token(conn, &subcommand.name, "Subcommand", timestamp)?;

        conn.execute(
            "INSERT OR IGNORE INTO ci_command_hierarchy
             (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, 'subcommand')",
            rusqlite::params![
                token_id,
                position as i64,
                parent_token_id,
                base_command_id,
                timestamp
            ],
        )?;

        if !subcommand.subcommands.is_empty() {
            seed_subcommands_recursive(
                conn,
                token_id,
                base_command_id,
                &subcommand.subcommands,
                position + 1,
                timestamp,
            )?;
        }
    }

    Ok(())
}

/// Seeds a base command into the hierarchy.
fn seed_base_command(conn: &Connection, command: &str, timestamp: i64) -> Result<i64, CIError> {
    let token_id = get_or_create_token(conn, command, "Command", timestamp)?;

    conn.execute(
        "INSERT OR IGNORE INTO ci_command_hierarchy
         (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
         VALUES (?1, 0, NULL, ?1, 1, 1, ?2, 'command')",
        rusqlite::params![token_id, timestamp],
    )?;

    Ok(token_id)
}

/// Gets or creates a token in the vocabulary.
fn get_or_create_token(
    conn: &Connection,
    text: &str,
    token_type: &str,
    timestamp: i64,
) -> Result<i64, CIError> {
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM ci_tokens WHERE text = ?1", [text], |row| {
            row.get(0)
        })
        .optional()?;

    if let Some(id) = existing {
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO ci_tokens (text, token_type, frequency, first_seen, last_seen)
         VALUES (?1, ?2, 1, ?3, ?3)",
        rusqlite::params![text, token_type, timestamp],
    )?;

    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use command_schema_core::{CommandSchema, SchemaSource};

    use super::*;
    use crate::intelligence::db_schema;
    use crate::intelligence::schema_provider::tests::TestSchemaProvider;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db_schema::create_schema(&conn).unwrap();
        conn
    }

    fn sample_provider() -> TestSchemaProvider {
        let mut git = CommandSchema::new("git", SchemaSource::Bootstrap);
        let mut remote = SubcommandSchema::new("remote");
        remote.subcommands = vec![
            SubcommandSchema::new("add"),
            SubcommandSchema::new("remove"),
        ];
        git.subcommands = vec![
            SubcommandSchema::new("commit"),
            SubcommandSchema::new("push"),
            SubcommandSchema::new("pull"),
            remote,
        ];

        let mut cargo = CommandSchema::new("cargo", SchemaSource::Bootstrap);
        cargo.subcommands = vec![
            SubcommandSchema::new("build"),
            SubcommandSchema::new("test"),
        ];

        TestSchemaProvider::from_schemas(vec![git, cargo])
    }

    #[test]
    fn test_bootstrap_if_empty() {
        let conn = setup_test_db();
        let provider = sample_provider();

        bootstrap_if_empty(&conn, &provider).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count > 0);

        let git_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ci_tokens WHERE text = 'git')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(git_exists);

        let git_subcmds: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ci_command_hierarchy h
                 JOIN ci_tokens t ON t.id = h.parent_token_id
                 WHERE t.text = 'git' AND h.position = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(git_subcmds > 0);
    }

    #[test]
    fn test_bootstrap_skips_if_already_completed() {
        let conn = setup_test_db();
        let provider = sample_provider();

        bootstrap_if_empty(&conn, &provider).unwrap();

        let count_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count_after_first > 0);

        bootstrap_if_empty(&conn, &provider).unwrap();

        let count_after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count_after_first, count_after_second);
    }

    #[test]
    fn test_bootstrap_merges_with_existing_data() {
        let conn = setup_test_db();
        let provider = sample_provider();
        let now = chrono::Utc::now().timestamp();

        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'mycommand', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, base_command_id, frequency, success_count, last_seen, role)
             VALUES (1, 0, 1, 100, 95, ?1, 'command')",
            [now],
        ).unwrap();

        bootstrap_if_empty(&conn, &provider).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count > 1, "Should have bootstrap data plus original entry");

        let original_freq: i64 = conn
            .query_row(
                "SELECT frequency FROM ci_command_hierarchy WHERE token_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(original_freq, 100, "Original frequency should be preserved");
    }

    #[test]
    fn test_nested_commands_seeded() {
        let conn = setup_test_db();
        let provider = sample_provider();
        bootstrap_if_empty(&conn, &provider).unwrap();

        let remote_id: i64 = conn
            .query_row(
                "SELECT id FROM ci_tokens WHERE text = 'remote'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        let add_after_remote: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM ci_command_hierarchy h
                    JOIN ci_tokens t ON t.id = h.token_id
                    WHERE h.parent_token_id = ?1 AND t.text = 'add'
                )",
                [remote_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(add_after_remote);
    }
}
