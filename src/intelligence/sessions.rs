//! Session and transition tracking.
//!
//! Tracks commands within terminal sessions and learns command-to-command
//! transitions for "next command" suggestions.

use rusqlite::Connection;
use tracing::debug;

use super::error::CIError;
use super::tokenizer::compute_command_hash;
use super::types::{Suggestion, SuggestionMetadata, SuggestionSource};

/// Starts a new session or resumes an existing one.
pub fn start_session(conn: &Connection, session_id: &str) -> Result<i64, CIError> {
    let now = chrono::Utc::now().timestamp();

    // Check if session exists
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        debug!(session_id, "Resuming existing session");
        return Ok(id);
    }

    // Create new session
    conn.execute(
        "INSERT INTO ci_sessions (session_id, start_time, command_count)
         VALUES (?1, ?2, 0)",
        rusqlite::params![session_id, now],
    )?;

    let id = conn.last_insert_rowid();
    debug!(session_id, db_id = id, "Created new session");

    Ok(id)
}

/// Ends a session.
pub fn end_session(conn: &Connection, session_id: &str) -> Result<(), CIError> {
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "UPDATE ci_sessions SET end_time = ?1 WHERE session_id = ?2",
        rusqlite::params![now, session_id],
    )?;

    debug!(session_id, "Ended session");
    Ok(())
}

/// Records a command-to-command transition.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `from_command` - The command executed before
/// * `to_command` - The command just executed
/// * `timestamp` - Current timestamp for the transition
/// * `prev_timestamp` - Optional timestamp of the previous command to compute time delta
pub fn record_transition(
    conn: &Connection,
    from_command: &str,
    to_command: &str,
    timestamp: i64,
    prev_timestamp: Option<i64>,
) -> Result<(), CIError> {
    let from_hash = compute_command_hash(from_command);
    let to_hash = compute_command_hash(to_command);

    // Get base command IDs
    let from_tokens = super::tokenizer::analyze_command(from_command);
    let to_tokens = super::tokenizer::analyze_command(to_command);

    let from_base_id: Option<i64> = from_tokens.first().and_then(|t| {
        conn.query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [&t.text],
            |row| row.get(0),
        )
        .ok()
    });

    let to_base_id: Option<i64> = to_tokens.first().and_then(|t| {
        conn.query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [&t.text],
            |row| row.get(0),
        )
        .ok()
    });

    // Compute time delta if previous timestamp is available
    let time_delta = prev_timestamp.map(|prev| timestamp - prev);

    // Insert or update transition with rolling average for time delta
    if let Some(delta) = time_delta {
        conn.execute(
            "INSERT INTO ci_transitions
             (from_command_hash, to_command_hash, from_base_cmd_id, to_base_cmd_id, frequency, avg_time_delta, last_seen)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)
             ON CONFLICT(from_command_hash, to_command_hash)
             DO UPDATE SET
                 frequency = frequency + 1,
                 avg_time_delta = COALESCE((avg_time_delta * (frequency - 1) + ?5) / frequency, ?5),
                 last_seen = ?6",
            rusqlite::params![from_hash, to_hash, from_base_id, to_base_id, delta, timestamp],
        )?;
    } else {
        conn.execute(
            "INSERT INTO ci_transitions
             (from_command_hash, to_command_hash, from_base_cmd_id, to_base_cmd_id, frequency, last_seen)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(from_command_hash, to_command_hash)
             DO UPDATE SET frequency = frequency + 1, last_seen = ?5",
            rusqlite::params![from_hash, to_hash, from_base_id, to_base_id, timestamp],
        )?;
    }

    debug!(
        from = %from_command,
        to = %to_command,
        time_delta = ?time_delta,
        "Recorded transition"
    );

    Ok(())
}

/// Gets suggestions for the next command based on the last command.
pub fn suggest_next(conn: &Connection, last_command: &str) -> Vec<Suggestion> {
    let last_hash = compute_command_hash(last_command);

    // Query transitions from the last command
    let mut stmt = match conn.prepare(
        "SELECT t.to_command_hash, tr.frequency, c.command_line
         FROM ci_transitions tr
         JOIN ci_commands c ON c.command_hash = tr.to_command_hash
         WHERE tr.from_command_hash = ?1
         ORDER BY tr.frequency DESC
         LIMIT 10"
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };

    let rows = match stmt.query_map([&last_hash], |row| {
        let _hash: String = row.get(0)?;
        let frequency: u32 = row.get(1)?;
        let command: String = row.get(2)?;
        Ok((command, frequency))
    }) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    let mut suggestions = Vec::new();
    for row in rows.flatten() {
        let (command, frequency) = row;
        suggestions.push(Suggestion {
            text: command,
            source: SuggestionSource::SessionTransition,
            score: frequency as f64 * SuggestionSource::SessionTransition.bonus(),
            metadata: SuggestionMetadata {
                frequency,
                ..Default::default()
            },
        });
    }

    suggestions
}

/// Gets the database ID for a session given its string identifier.
pub fn get_session_db_id(conn: &Connection, session_id: &str) -> Option<i64> {
    conn.query_row(
        "SELECT id FROM ci_sessions WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )
    .ok()
}

/// Gets the timestamp of the last command in a session.
pub fn get_last_command_timestamp(conn: &Connection, session_id: &str) -> Option<i64> {
    let db_session_id = get_session_db_id(conn, session_id)?;

    conn.query_row(
        "SELECT timestamp FROM ci_session_commands
         WHERE session_id = ?1
         ORDER BY sequence_number DESC
         LIMIT 1",
        [db_session_id],
        |row| row.get(0),
    )
    .ok()
}

/// Gets the most recent commands in a session.
pub fn get_session_commands(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> Result<Vec<String>, CIError> {
    let db_session_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .ok();

    let Some(db_id) = db_session_id else {
        return Ok(Vec::new());
    };

    let mut stmt = conn.prepare(
        "SELECT c.command_line
         FROM ci_session_commands sc
         JOIN ci_commands c ON c.id = sc.command_id
         WHERE sc.session_id = ?1
         ORDER BY sc.sequence_number DESC
         LIMIT ?2"
    )?;

    let rows = stmt.query_map(rusqlite::params![db_id, limit], |row| row.get(0))?;

    let mut commands = Vec::new();
    for row in rows.flatten() {
        commands.push(row);
    }

    // Reverse to get chronological order
    commands.reverse();

    Ok(commands)
}

/// Adds a command to a session.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `session_id` - The session identifier string
/// * `command` - The command text
/// * `timestamp` - Timestamp of the command execution
///
/// # Returns
///
/// The sequence number assigned to this command in the session, or an error.
pub fn add_session_command(
    conn: &Connection,
    session_id: &str,
    command: &str,
    timestamp: i64,
) -> Result<i64, CIError> {
    // Get session database ID
    let db_session_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .ok();

    let Some(session_db_id) = db_session_id else {
        return Err(CIError::NotFound(format!("Session not found: {}", session_id)));
    };

    // Get command ID
    let command_hash = compute_command_hash(command);
    let command_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_commands WHERE command_hash = ?1",
            [&command_hash],
            |row| row.get(0),
        )
        .ok();

    let Some(cmd_id) = command_id else {
        // Command not in ci_commands yet - this shouldn't happen if called
        // after learn_command, but return 0 to indicate no sequence was added
        debug!(command = %command, "Command not found in ci_commands, skipping session tracking");
        return Ok(0);
    };

    // Get next sequence number
    let seq_num: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(sequence_number), 0) + 1 FROM ci_session_commands WHERE session_id = ?1",
            [session_db_id],
            |row| row.get(0),
        )
        .unwrap_or(1);

    // Insert session command
    conn.execute(
        "INSERT INTO ci_session_commands (session_id, sequence_number, command_id, timestamp)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![session_db_id, seq_num, cmd_id, timestamp],
    )?;

    // Update session command count
    conn.execute(
        "UPDATE ci_sessions SET command_count = command_count + 1 WHERE id = ?1",
        [session_db_id],
    )?;

    debug!(
        session_id = %session_id,
        command = %command,
        sequence_number = seq_num,
        "Added command to session"
    );

    Ok(seq_num)
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
    fn test_start_session() {
        let conn = setup_test_db();

        let id1 = start_session(&conn, "test-session").unwrap();
        let id2 = start_session(&conn, "test-session").unwrap();

        // Same session ID should return same database ID
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_end_session() {
        let conn = setup_test_db();

        start_session(&conn, "test-session").unwrap();
        end_session(&conn, "test-session").unwrap();

        // Verify end_time was set
        let end_time: Option<i64> = conn
            .query_row(
                "SELECT end_time FROM ci_sessions WHERE session_id = 'test-session'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(end_time.is_some());
    }

    #[test]
    fn test_record_transition() {
        let conn = setup_test_db();

        // Need to create tokens first
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('cargo', 'Command', ?1, ?1)",
            [now],
        ).unwrap();

        record_transition(&conn, "git pull", "cargo build", now, None).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_transitions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Record same transition again with a time delta
        let later = now + 10;
        record_transition(&conn, "git pull", "cargo build", later, Some(now)).unwrap();

        let (freq, avg_delta): (i64, Option<i64>) = conn
            .query_row(
                "SELECT frequency, avg_time_delta FROM ci_transitions LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(freq, 2);
        // The avg_time_delta should be set after the second call with time delta
        assert!(avg_delta.is_some());
    }

    #[test]
    fn test_add_session_command() {
        let conn = setup_test_db();

        // Start a session
        start_session(&conn, "test-session").unwrap();

        // Create a command entry with the correct hash
        let now = chrono::Utc::now().timestamp();
        let command_hash = compute_command_hash("git status");
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_commands (command_line, command_hash, token_ids, token_count, base_command_id, timestamp)
             VALUES ('git status', ?1, '[1]', 1, 1, ?2)",
            rusqlite::params![command_hash, now],
        ).unwrap();

        // Add command to session
        let seq_num = add_session_command(&conn, "test-session", "git status", now).unwrap();
        assert_eq!(seq_num, 1);

        // Verify session_commands entry
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_session_commands", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Verify session command_count was incremented
        let cmd_count: i64 = conn
            .query_row(
                "SELECT command_count FROM ci_sessions WHERE session_id = 'test-session'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cmd_count, 1);
    }
}
