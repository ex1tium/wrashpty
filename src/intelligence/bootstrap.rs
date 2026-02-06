//! Bootstrap module for seeding initial command knowledge.
//!
//! This module seeds the command hierarchy table with common commands
//! and their subcommands on first run. After bootstrap, the learned
//! data takes over completely.

use rusqlite::Connection;
use tracing::{debug, info};

use super::error::CIError;
use super::schema::{store_schema, CommandSchema, SchemaSource, SubcommandSchema};

/// Seeds the command hierarchy, merging with existing data.
///
/// This should be called during CommandIntelligence initialization.
/// Uses INSERT OR IGNORE to merge bootstrap data with existing learned data
/// without overwriting user's actual usage patterns.
pub fn bootstrap_if_empty(conn: &Connection) -> Result<(), CIError> {
    // Check if bootstrap has already been run by looking for a marker in sync_state
    let bootstrapped: bool = conn
        .query_row(
            "SELECT value = 'true' FROM ci_sync_state WHERE key = 'bootstrap.completed'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if bootstrapped {
        debug!("Bootstrap already completed, skipping");
        return Ok(());
    }

    info!("Bootstrapping command hierarchy with initial knowledge");
    seed_command_knowledge(conn)?;
    seed_bootstrap_schemas(conn)?;

    // Mark bootstrap as completed
    conn.execute(
        "INSERT OR REPLACE INTO ci_sync_state (key, value) VALUES ('bootstrap.completed', 'true')",
        [],
    )?;

    info!("Bootstrap completed");
    Ok(())
}

/// Seeds the command knowledge into the database.
fn seed_command_knowledge(conn: &Connection) -> Result<(), CIError> {
    let now = chrono::Utc::now().timestamp();

    // Seed in a transaction for atomicity
    conn.execute_batch("BEGIN TRANSACTION")?;

    if let Err(e) = seed_all_commands(conn, now) {
        conn.execute_batch("ROLLBACK")?;
        return Err(e);
    }

    conn.execute_batch("COMMIT")?;
    Ok(())
}

/// Seeds schema rows based on the bootstrapped hierarchy.
///
/// This keeps schema-backed suggestions aligned with the same canonical
/// bootstrap data used for hierarchy suggestions.
fn seed_bootstrap_schemas(conn: &Connection) -> Result<(), CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.text
         FROM ci_command_hierarchy h
         JOIN ci_tokens t ON t.id = h.token_id
         WHERE h.position = 0
         ORDER BY t.text",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (base_token_id, command) = row?;
        let mut schema = CommandSchema::new(&command, SchemaSource::Bootstrap);
        schema.subcommands = load_nested_subcommands(conn, base_token_id)?;
        store_schema(conn, &schema)?;
    }

    debug!("Seeded bootstrap schemas");
    Ok(())
}

/// Loads nested subcommands for a hierarchy token.
fn load_nested_subcommands(
    conn: &Connection,
    parent_token_id: i64,
) -> Result<Vec<SubcommandSchema>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.text
         FROM ci_command_hierarchy h
         JOIN ci_tokens t ON t.id = h.token_id
         WHERE h.parent_token_id = ?1
         ORDER BY h.frequency DESC, t.text ASC",
    )?;

    let rows = stmt.query_map(rusqlite::params![parent_token_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut subcommands = Vec::new();
    for row in rows {
        let (token_id, name) = row?;
        if name.starts_with('-') {
            // Hierarchy may contain command-like flags; schema subcommands should not.
            continue;
        }

        let mut sub = SubcommandSchema::new(&name);
        sub.subcommands = load_nested_subcommands(conn, token_id)?;
        subcommands.push(sub);
    }

    Ok(subcommands)
}

/// Seeds all command knowledge.
fn seed_all_commands(conn: &Connection, timestamp: i64) -> Result<(), CIError> {
    // Git
    seed_command_with_subcommands(
        conn,
        "git",
        &[
            "add",
            "commit",
            "push",
            "pull",
            "fetch",
            "merge",
            "rebase",
            "branch",
            "checkout",
            "switch",
            "status",
            "log",
            "diff",
            "remote",
            "stash",
            "reset",
            "tag",
            "clone",
            "init",
            "cherry-pick",
            "bisect",
            "blame",
            "show",
            "restore",
            "worktree",
        ],
        timestamp,
    )?;

    // Git nested commands
    seed_nested_commands(
        conn,
        "git",
        "remote",
        &["add", "remove", "-v", "show", "rename", "prune", "set-url"],
        timestamp,
    )?;
    seed_nested_commands(
        conn,
        "git",
        "stash",
        &["list", "show", "pop", "apply", "drop", "clear", "push"],
        timestamp,
    )?;
    seed_nested_commands(
        conn,
        "git",
        "worktree",
        &["add", "list", "remove", "prune"],
        timestamp,
    )?;
    seed_nested_commands(
        conn,
        "git",
        "bisect",
        &["start", "good", "bad", "reset", "skip"],
        timestamp,
    )?;

    // Docker
    seed_command_with_subcommands(
        conn,
        "docker",
        &[
            "run", "build", "pull", "push", "ps", "images", "exec", "logs", "stop", "start",
            "restart", "rm", "rmi", "compose", "network", "volume", "system", "inspect", "tag",
            "save", "load",
        ],
        timestamp,
    )?;

    // Docker nested commands
    seed_nested_commands(
        conn,
        "docker",
        "compose",
        &[
            "up", "down", "build", "logs", "ps", "exec", "restart", "pull",
        ],
        timestamp,
    )?;
    seed_nested_commands(
        conn,
        "docker",
        "system",
        &["prune", "df", "info", "events"],
        timestamp,
    )?;
    seed_nested_commands(
        conn,
        "docker",
        "network",
        &["create", "ls", "rm", "inspect", "connect", "disconnect"],
        timestamp,
    )?;
    seed_nested_commands(
        conn,
        "docker",
        "volume",
        &["create", "ls", "rm", "inspect", "prune"],
        timestamp,
    )?;

    // Cargo
    seed_command_with_subcommands(
        conn,
        "cargo",
        &[
            "build",
            "run",
            "test",
            "check",
            "clippy",
            "fmt",
            "doc",
            "clean",
            "update",
            "add",
            "remove",
            "publish",
            "bench",
            "tree",
            "audit",
            "outdated",
            "fix",
            "install",
            "uninstall",
        ],
        timestamp,
    )?;

    // npm
    seed_command_with_subcommands(
        conn,
        "npm",
        &[
            "install", "run", "test", "build", "start", "dev", "add", "remove", "update", "audit",
            "publish", "init", "ci", "link", "unlink", "exec", "outdated",
        ],
        timestamp,
    )?;

    // yarn
    seed_command_with_subcommands(
        conn,
        "yarn",
        &[
            "install", "run", "test", "build", "start", "dev", "add", "remove", "upgrade", "audit",
            "publish", "init", "link",
        ],
        timestamp,
    )?;

    // pnpm
    seed_command_with_subcommands(
        conn,
        "pnpm",
        &[
            "install", "run", "test", "build", "start", "dev", "add", "remove", "update", "audit",
            "publish", "init", "exec",
        ],
        timestamp,
    )?;

    // kubectl
    seed_command_with_subcommands(
        conn,
        "kubectl",
        &[
            "get",
            "describe",
            "logs",
            "exec",
            "apply",
            "delete",
            "create",
            "edit",
            "scale",
            "rollout",
            "port-forward",
            "config",
            "cluster-info",
            "top",
            "patch",
            "label",
        ],
        timestamp,
    )?;

    // kubectl nested commands
    seed_nested_commands(
        conn,
        "kubectl",
        "rollout",
        &["status", "history", "undo", "restart", "pause", "resume"],
        timestamp,
    )?;
    seed_nested_commands(
        conn,
        "kubectl",
        "config",
        &[
            "use-context",
            "get-contexts",
            "current-context",
            "view",
            "set-context",
        ],
        timestamp,
    )?;

    // systemctl
    seed_command_with_subcommands(
        conn,
        "systemctl",
        &[
            "start",
            "stop",
            "restart",
            "status",
            "enable",
            "disable",
            "reload",
            "daemon-reload",
            "is-active",
            "is-enabled",
            "list-units",
            "list-unit-files",
            "mask",
            "unmask",
        ],
        timestamp,
    )?;

    // Simple base commands (no subcommands)
    let simple_commands = [
        "ls", "cd", "cat", "vim", "nano", "grep", "find", "make", "python", "python3", "node",
        "go", "curl", "wget", "ssh", "scp", "rsync", "tar", "zip", "unzip", "chmod", "chown",
        "mkdir", "rm", "cp", "mv", "ln", "touch", "head", "tail", "less", "more", "diff", "sort",
        "uniq", "wc", "awk", "sed", "xargs", "tee", "sudo", "su", "htop", "top", "ps", "kill",
        "pkill", "man", "which", "whereis", "type", "echo", "printf", "env", "export", "alias",
        "history",
    ];

    for cmd in &simple_commands {
        seed_base_command(conn, cmd, timestamp)?;
    }

    debug!("Seeded {} base commands", simple_commands.len() + 8); // +8 for commands with subcommands
    Ok(())
}

/// Seeds a base command into the hierarchy.
fn seed_base_command(conn: &Connection, command: &str, timestamp: i64) -> Result<i64, CIError> {
    // Get or create token
    let token_id = get_or_create_token(conn, command, "Command", timestamp)?;

    // Insert hierarchy entry (position 0, no parent)
    conn.execute(
        "INSERT OR IGNORE INTO ci_command_hierarchy
         (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
         VALUES (?1, 0, NULL, ?1, 1, 1, ?2, 'command')",
        rusqlite::params![token_id, timestamp],
    )?;

    Ok(token_id)
}

/// Seeds a command with its subcommands.
fn seed_command_with_subcommands(
    conn: &Connection,
    command: &str,
    subcommands: &[&str],
    timestamp: i64,
) -> Result<(), CIError> {
    let base_id = seed_base_command(conn, command, timestamp)?;

    for subcmd in subcommands {
        let token_id = get_or_create_token(conn, subcmd, "Subcommand", timestamp)?;

        conn.execute(
            "INSERT OR IGNORE INTO ci_command_hierarchy
             (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (?1, 1, ?2, ?2, 1, 1, ?3, 'subcommand')",
            rusqlite::params![token_id, base_id, timestamp],
        )?;
    }

    Ok(())
}

/// Seeds nested commands (e.g., git remote add).
fn seed_nested_commands(
    conn: &Connection,
    command: &str,
    subcommand: &str,
    nested: &[&str],
    timestamp: i64,
) -> Result<(), CIError> {
    // Get base command ID
    let base_id: i64 = conn.query_row(
        "SELECT id FROM ci_tokens WHERE text = ?1",
        [command],
        |row| row.get(0),
    )?;

    // Get subcommand ID
    let subcmd_id: i64 = conn.query_row(
        "SELECT id FROM ci_tokens WHERE text = ?1",
        [subcommand],
        |row| row.get(0),
    )?;

    for nested_cmd in nested {
        let token_id = get_or_create_token(conn, nested_cmd, "Subcommand", timestamp)?;

        conn.execute(
            "INSERT OR IGNORE INTO ci_command_hierarchy
             (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (?1, 2, ?2, ?3, 1, 1, ?4, 'subcommand')",
            rusqlite::params![token_id, subcmd_id, base_id, timestamp],
        )?;
    }

    Ok(())
}

/// Gets or creates a token in the vocabulary.
fn get_or_create_token(
    conn: &Connection,
    text: &str,
    token_type: &str,
    timestamp: i64,
) -> Result<i64, CIError> {
    // Try to get existing
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM ci_tokens WHERE text = ?1", [text], |row| {
            row.get(0)
        })
        .ok();

    if let Some(id) = existing {
        return Ok(id);
    }

    // Create new
    conn.execute(
        "INSERT INTO ci_tokens (text, token_type, frequency, first_seen, last_seen)
         VALUES (?1, ?2, 1, ?3, ?3)",
        rusqlite::params![text, token_type, timestamp],
    )?;

    Ok(conn.last_insert_rowid())
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
    fn test_bootstrap_if_empty() {
        let conn = setup_test_db();

        // Should bootstrap when empty
        bootstrap_if_empty(&conn).unwrap();

        // Verify hierarchy was populated
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count > 0);

        // Verify git exists
        let git_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ci_tokens WHERE text = 'git')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(git_exists);

        // Verify git has subcommands
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

        // Run bootstrap first time
        bootstrap_if_empty(&conn).unwrap();

        let count_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count_after_first > 0);

        // Run bootstrap again - should skip
        bootstrap_if_empty(&conn).unwrap();

        // Count should be the same (no duplicate entries)
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
        let now = chrono::Utc::now().timestamp();

        // Manually add one learned entry
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'mycommand', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, base_command_id, frequency, success_count, last_seen, role)
             VALUES (1, 0, 1, 100, 95, ?1, 'command')",
            [now],
        ).unwrap();

        // Bootstrap should merge, not replace
        bootstrap_if_empty(&conn).unwrap();

        // Should have original entry plus bootstrapped entries
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_hierarchy", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(count > 1, "Should have bootstrap data plus original entry");

        // Original entry should still exist with its frequency
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
        bootstrap_if_empty(&conn).unwrap();

        // Verify git remote add exists
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

    #[test]
    fn test_bootstrap_seeds_command_schemas() {
        let conn = setup_test_db();
        bootstrap_if_empty(&conn).unwrap();

        let command_schema_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ci_command_schemas WHERE subcommand IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            command_schema_count > 0,
            "bootstrap should create base command schemas"
        );

        let has_nested_git_remote_add: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM ci_command_schemas
                    WHERE command = 'git' AND subcommand = 'remote add'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_nested_git_remote_add);
    }
}
