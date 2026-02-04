//! Bootstrap module for seeding initial command knowledge.
//!
//! This module seeds the command hierarchy table with common commands
//! and their subcommands on first run. After bootstrap, the learned
//! data takes over completely.

use rusqlite::Connection;
use tracing::{debug, info};

use super::error::CIError;

/// Seeds the command hierarchy if the database is empty.
///
/// This should be called during CommandIntelligence initialization.
/// It only runs if ci_command_hierarchy is empty.
pub fn bootstrap_if_empty(conn: &Connection) -> Result<(), CIError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ci_command_hierarchy",
        [],
        |row| row.get(0),
    )?;

    if count == 0 {
        info!("Bootstrapping command hierarchy with initial knowledge");
        seed_command_knowledge(conn)?;
        info!("Bootstrap completed");
    } else {
        debug!("Command hierarchy already populated, skipping bootstrap");
    }

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

/// Seeds all command knowledge.
fn seed_all_commands(conn: &Connection, timestamp: i64) -> Result<(), CIError> {
    // Git
    seed_command_with_subcommands(conn, "git", &[
        "add", "commit", "push", "pull", "fetch", "merge", "rebase",
        "branch", "checkout", "switch", "status", "log", "diff",
        "remote", "stash", "reset", "tag", "clone", "init", "cherry-pick",
        "bisect", "blame", "show", "restore", "worktree",
    ], timestamp)?;

    // Git nested commands
    seed_nested_commands(conn, "git", "remote", &[
        "add", "remove", "-v", "show", "rename", "prune", "set-url",
    ], timestamp)?;
    seed_nested_commands(conn, "git", "stash", &[
        "list", "show", "pop", "apply", "drop", "clear", "push",
    ], timestamp)?;
    seed_nested_commands(conn, "git", "worktree", &[
        "add", "list", "remove", "prune",
    ], timestamp)?;
    seed_nested_commands(conn, "git", "bisect", &[
        "start", "good", "bad", "reset", "skip",
    ], timestamp)?;

    // Docker
    seed_command_with_subcommands(conn, "docker", &[
        "run", "build", "pull", "push", "ps", "images", "exec",
        "logs", "stop", "start", "restart", "rm", "rmi", "compose",
        "network", "volume", "system", "inspect", "tag", "save", "load",
    ], timestamp)?;

    // Docker nested commands
    seed_nested_commands(conn, "docker", "compose", &[
        "up", "down", "build", "logs", "ps", "exec", "restart", "pull",
    ], timestamp)?;
    seed_nested_commands(conn, "docker", "system", &[
        "prune", "df", "info", "events",
    ], timestamp)?;
    seed_nested_commands(conn, "docker", "network", &[
        "create", "ls", "rm", "inspect", "connect", "disconnect",
    ], timestamp)?;
    seed_nested_commands(conn, "docker", "volume", &[
        "create", "ls", "rm", "inspect", "prune",
    ], timestamp)?;

    // Cargo
    seed_command_with_subcommands(conn, "cargo", &[
        "build", "run", "test", "check", "clippy", "fmt", "doc",
        "clean", "update", "add", "remove", "publish", "bench",
        "tree", "audit", "outdated", "fix", "install", "uninstall",
    ], timestamp)?;

    // npm
    seed_command_with_subcommands(conn, "npm", &[
        "install", "run", "test", "build", "start", "dev", "add",
        "remove", "update", "audit", "publish", "init", "ci",
        "link", "unlink", "exec", "outdated",
    ], timestamp)?;

    // yarn
    seed_command_with_subcommands(conn, "yarn", &[
        "install", "run", "test", "build", "start", "dev", "add",
        "remove", "upgrade", "audit", "publish", "init", "link",
    ], timestamp)?;

    // pnpm
    seed_command_with_subcommands(conn, "pnpm", &[
        "install", "run", "test", "build", "start", "dev", "add",
        "remove", "update", "audit", "publish", "init", "exec",
    ], timestamp)?;

    // kubectl
    seed_command_with_subcommands(conn, "kubectl", &[
        "get", "describe", "logs", "exec", "apply", "delete",
        "create", "edit", "scale", "rollout", "port-forward",
        "config", "cluster-info", "top", "patch", "label",
    ], timestamp)?;

    // kubectl nested commands
    seed_nested_commands(conn, "kubectl", "rollout", &[
        "status", "history", "undo", "restart", "pause", "resume",
    ], timestamp)?;
    seed_nested_commands(conn, "kubectl", "config", &[
        "use-context", "get-contexts", "current-context", "view", "set-context",
    ], timestamp)?;

    // systemctl
    seed_command_with_subcommands(conn, "systemctl", &[
        "start", "stop", "restart", "status", "enable", "disable",
        "reload", "daemon-reload", "is-active", "is-enabled",
        "list-units", "list-unit-files", "mask", "unmask",
    ], timestamp)?;

    // Simple base commands (no subcommands)
    let simple_commands = [
        "ls", "cd", "cat", "vim", "nano", "grep", "find", "make",
        "python", "python3", "node", "go", "curl", "wget", "ssh",
        "scp", "rsync", "tar", "zip", "unzip", "chmod", "chown",
        "mkdir", "rm", "cp", "mv", "ln", "touch", "head", "tail",
        "less", "more", "diff", "sort", "uniq", "wc", "awk", "sed",
        "xargs", "tee", "sudo", "su", "htop", "top", "ps", "kill",
        "pkill", "man", "which", "whereis", "type", "echo", "printf",
        "env", "export", "alias", "history",
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
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [text],
            |row| row.get(0),
        )
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
    use crate::intelligence::schema;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_bootstrap_if_empty() {
        let conn = setup_test_db();

        // Should bootstrap when empty
        bootstrap_if_empty(&conn).unwrap();

        // Verify hierarchy was populated
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ci_command_hierarchy",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(count > 0);

        // Verify git exists
        let git_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ci_tokens WHERE text = 'git')",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(git_exists);

        // Verify git has subcommands
        let git_subcmds: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ci_command_hierarchy h
             JOIN ci_tokens t ON t.id = h.parent_token_id
             WHERE t.text = 'git' AND h.position = 1",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(git_subcmds > 0);
    }

    #[test]
    fn test_bootstrap_skips_if_populated() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Manually add one entry
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'test', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, base_command_id, frequency, success_count, last_seen, role)
             VALUES (1, 0, 1, 1, 1, ?1, 'command')",
            [now],
        ).unwrap();

        // Bootstrap should skip
        bootstrap_if_empty(&conn).unwrap();

        // Should still have only 1 entry
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ci_command_hierarchy",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_nested_commands_seeded() {
        let conn = setup_test_db();
        bootstrap_if_empty(&conn).unwrap();

        // Verify git remote add exists
        let remote_id: i64 = conn.query_row(
            "SELECT id FROM ci_tokens WHERE text = 'remote'",
            [],
            |row| row.get(0),
        ).unwrap();

        let add_after_remote: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM ci_command_hierarchy h
                JOIN ci_tokens t ON t.id = h.token_id
                WHERE h.parent_token_id = ?1 AND t.text = 'add'
            )",
            [remote_id],
            |row| row.get(0),
        ).unwrap();
        assert!(add_after_remote);
    }
}
