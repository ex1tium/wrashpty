//! Success/failure variant tracking.
//!
//! Tracks command variants and their success rates to prefer
//! working commands over failed ones.

use rusqlite::Connection;
use tracing::debug;

use super::error::CIError;
use super::tokenizer::{analyze_command, compute_command_hash, detect_value_type};

/// Records the execution result of a command.
pub fn record_execution(conn: &Connection, command: &str, exit_status: i32) -> Result<(), CIError> {
    let canonical = canonicalize(command);
    let variant_hash = compute_command_hash(command);
    let now = chrono::Utc::now().timestamp();

    let is_success = exit_status == 0;

    if is_success {
        conn.execute(
            "INSERT INTO ci_command_variants
             (canonical_pattern, variant_hash, variant_command, success_count, last_success)
             VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(canonical_pattern, variant_hash)
             DO UPDATE SET success_count = success_count + 1, last_success = ?4",
            rusqlite::params![canonical, variant_hash, command, now],
        )?;
    } else {
        conn.execute(
            "INSERT INTO ci_command_variants
             (canonical_pattern, variant_hash, variant_command, failure_count, last_failure)
             VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(canonical_pattern, variant_hash)
             DO UPDATE SET failure_count = failure_count + 1, last_failure = ?4",
            rusqlite::params![canonical, variant_hash, command, now],
        )?;
    }

    debug!(
        command = %command,
        canonical = %canonical,
        success = is_success,
        "Recorded execution result"
    );

    Ok(())
}

/// Gets the success rate for a command pattern.
pub fn get_success_rate(conn: &Connection, command: &str) -> Result<Option<f64>, CIError> {
    let canonical = canonicalize(command);

    let rate: Option<f64> = conn
        .query_row(
            "SELECT AVG(success_rate) FROM ci_command_variants WHERE canonical_pattern = ?1",
            [&canonical],
            |row| row.get(0),
        )
        .ok()
        .flatten();

    Ok(rate)
}

/// Gets successful variants of a command pattern.
pub fn get_successful_variants(
    conn: &Connection,
    pattern: &str,
    limit: usize,
) -> Result<Vec<CommandVariant>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT variant_command, success_count, failure_count, success_rate, last_success
         FROM ci_command_variants
         WHERE canonical_pattern = ?1 AND success_rate > 0.5
         ORDER BY success_rate DESC, success_count DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![pattern, limit], |row| {
        Ok(CommandVariant {
            command: row.get(0)?,
            success_count: row.get(1)?,
            failure_count: row.get(2)?,
            success_rate: row.get(3)?,
            last_success: row.get(4)?,
        })
    })?;

    let mut variants = Vec::new();
    for row in rows.flatten() {
        variants.push(row);
    }

    Ok(variants)
}

/// A command variant with success/failure statistics.
#[derive(Debug, Clone)]
pub struct CommandVariant {
    /// The actual command.
    pub command: String,

    /// Number of successful executions.
    pub success_count: u32,

    /// Number of failed executions.
    pub failure_count: u32,

    /// Success rate (0.0 - 1.0).
    pub success_rate: Option<f64>,

    /// Last successful execution timestamp.
    pub last_success: Option<i64>,
}

/// Canonicalizes a command to a pattern for looking up variants.
///
/// This is the public interface for suggest.rs to use when swapping variants.
pub fn canonicalize_for_lookup(command: &str) -> String {
    canonicalize(command)
}

/// Canonicalizes a command to a pattern.
///
/// Replaces variable parts with type markers to group similar commands.
fn canonicalize(command: &str) -> String {
    let tokens = analyze_command(command);
    if tokens.is_empty() {
        return command.to_string();
    }

    let mut parts = Vec::new();

    for token in &tokens {
        let text = &token.text;

        // Keep command and subcommand as-is
        if token.position < 2 && !text.starts_with('-') {
            parts.push(text.clone());
            continue;
        }

        // Keep flags as-is
        if text.starts_with('-') {
            parts.push(text.clone());
            continue;
        }

        // Replace values with type markers
        if let Some(value_type) = detect_value_type(text) {
            parts.push(format!("<{}>", value_type.to_uppercase()));
        } else {
            // Keep short tokens as-is, replace long ones with generic marker
            if text.len() <= 10 {
                parts.push(text.clone());
            } else {
                parts.push("<VALUE>".to_string());
            }
        }
    }

    parts.join("_")
}

/// Gets the best variant for a canonical pattern.
pub fn get_best_variant(conn: &Connection, pattern: &str) -> Result<Option<String>, CIError> {
    let variant: Option<String> = conn
        .query_row(
            "SELECT variant_command
             FROM ci_command_variants
             WHERE canonical_pattern = ?1
             ORDER BY success_rate DESC, success_count DESC
             LIMIT 1",
            [pattern],
            |row| row.get(0),
        )
        .ok();

    Ok(variant)
}

/// Checks if a command has a high failure rate.
pub fn is_likely_to_fail(conn: &Connection, command: &str) -> Result<bool, CIError> {
    let rate = get_success_rate(conn, command)?;

    Ok(rate.map(|r| r < 0.3).unwrap_or(false))
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
    fn test_canonicalize() {
        let canonical = canonicalize("docker run -p 8080:8080 nginx:latest");
        assert!(canonical.contains("docker"));
        assert!(canonical.contains("run"));
        assert!(canonical.contains("-p"));
        assert!(canonical.contains("<"));
    }

    #[test]
    fn test_record_execution() {
        let conn = setup_test_db();

        record_execution(&conn, "git commit -m 'test'", 0).unwrap();
        record_execution(&conn, "git commit -m 'test'", 0).unwrap();
        record_execution(&conn, "git commit -m 'test'", 1).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_command_variants", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);

        let (success, failure): (i64, i64) = conn
            .query_row(
                "SELECT success_count, failure_count FROM ci_command_variants LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(success, 2);
        assert_eq!(failure, 1);
    }

    #[test]
    fn test_get_success_rate() {
        let conn = setup_test_db();

        // Record some executions
        record_execution(&conn, "git push", 0).unwrap();
        record_execution(&conn, "git push", 0).unwrap();
        record_execution(&conn, "git push", 0).unwrap();
        record_execution(&conn, "git push", 1).unwrap();

        let rate = get_success_rate(&conn, "git push").unwrap();
        assert!(rate.is_some());
        let r = rate.unwrap();
        assert!(r > 0.7 && r < 0.8); // 3/4 = 0.75
    }
}
