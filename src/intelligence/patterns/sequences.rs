//! Token sequence learning.
//!
//! Learns what tokens typically follow other tokens in specific contexts.

use rusqlite::Connection;

use crate::intelligence::error::CIError;
use crate::intelligence::tokenizer::compute_token_hash;
use crate::intelligence::types::AnalyzedToken;

/// Learns token sequences from a command.
pub fn learn_sequences(
    conn: &Connection,
    tokens: &[AnalyzedToken],
    token_ids: &[i64],
    base_command_id: Option<i64>,
    is_success: bool,
    timestamp: i64,
) -> Result<(), CIError> {
    if tokens.len() < 2 {
        return Ok(());
    }

    let success_increment = if is_success { 1 } else { 0 };

    // Learn pairwise sequences
    for i in 0..tokens.len() - 1 {
        let context_id = token_ids[i];
        let next_id = token_ids[i + 1];

        conn.execute(
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
                success_increment,
                timestamp,
            ],
        )?;
    }

    Ok(())
}

/// Learns n-gram patterns from a command.
pub fn learn_ngrams(
    conn: &Connection,
    tokens: &[AnalyzedToken],
    token_ids: &[i64],
    timestamp: i64,
) -> Result<(), CIError> {
    // 2-grams: pairs of tokens predicting the next
    if tokens.len() >= 3 {
        for i in 0..tokens.len() - 2 {
            let context_tokens: Vec<&str> = tokens[i..i + 2]
                .iter()
                .map(|t| t.text.as_str())
                .collect();
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
            let context_tokens: Vec<&str> = tokens[i..i + 3]
                .iter()
                .map(|t| t.text.as_str())
                .collect();
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

/// Queries learned sequences for suggestions.
///
/// Returns tuples of (next_token, frequency, success_count, last_seen).
pub fn query_sequences(
    conn: &Connection,
    context_token: &str,
    position: usize,
    base_command: Option<&str>,
    limit: usize,
) -> Result<Vec<(String, u32, u32, i64)>, CIError> {
    // First, get the context token ID
    let context_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [context_token],
            |row| row.get(0),
        )
        .ok();

    let Some(context_id) = context_id else {
        return Ok(Vec::new());
    };

    // Get base command ID if provided
    let base_id: Option<i64> = base_command.and_then(|cmd| {
        conn.query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [cmd],
            |row| row.get(0),
        )
        .ok()
    });

    // Query sequences
    let mut stmt = if base_id.is_some() {
        conn.prepare(
            "SELECT t.text, s.frequency, s.success_count, s.last_seen
             FROM ci_sequences s
             JOIN ci_tokens t ON t.id = s.next_token_id
             WHERE s.context_token_id = ?1
               AND s.context_position = ?2
               AND s.base_command_id = ?3
             ORDER BY s.frequency DESC
             LIMIT ?4"
        )?
    } else {
        conn.prepare(
            "SELECT t.text, s.frequency, s.success_count, s.last_seen
             FROM ci_sequences s
             JOIN ci_tokens t ON t.id = s.next_token_id
             WHERE s.context_token_id = ?1
               AND s.context_position = ?2
             ORDER BY s.frequency DESC
             LIMIT ?3"
        )?
    };

    let mut results = Vec::new();
    if let Some(base_id) = base_id {
        let rows = stmt.query_map(
            rusqlite::params![context_id, position, base_id, limit],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, u32>(2)?, row.get::<_, i64>(3)?)),
        )?;
        for row in rows.flatten() {
            results.push(row);
        }
    } else {
        let rows = stmt.query_map(
            rusqlite::params![context_id, position, limit],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, u32>(2)?, row.get::<_, i64>(3)?)),
        )?;
        for row in rows.flatten() {
            results.push(row);
        }
    }

    Ok(results)
}

/// Queries n-gram patterns for suggestions.
///
/// Returns tuples of (next_token, frequency, last_seen).
pub fn query_ngrams(
    conn: &Connection,
    context_tokens: &[&str],
    limit: usize,
) -> Result<Vec<(String, u32, i64)>, CIError> {
    if context_tokens.is_empty() || context_tokens.len() > 3 {
        return Ok(Vec::new());
    }

    let pattern_hash = compute_token_hash(context_tokens);

    let mut stmt = conn.prepare(
        "SELECT t.text, n.frequency, n.last_seen
         FROM ci_ngrams n
         JOIN ci_tokens t ON t.id = n.next_token_id
         WHERE n.pattern_hash = ?1
         ORDER BY n.frequency DESC
         LIMIT ?2"
    )?;

    let rows = stmt.query_map(rusqlite::params![pattern_hash, limit], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;

    let mut results = Vec::new();
    for r in rows.flatten() {
        results.push(r);
    }

    Ok(results)
}

/// Queries base commands (first token) by frequency.
///
/// Returns tuples of (command, frequency, success_count, last_seen).
/// This aggregates ci_commands by base_command_id for top-level suggestions.
pub fn query_base_commands(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, u32, u32, i64)>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.text,
                COUNT(*) as frequency,
                SUM(CASE WHEN c.exit_status = 0 THEN 1 ELSE 0 END) as success_count,
                MAX(c.timestamp) as last_seen
         FROM ci_commands c
         JOIN ci_tokens t ON t.id = c.base_command_id
         WHERE c.base_command_id IS NOT NULL
         GROUP BY c.base_command_id
         ORDER BY frequency DESC
         LIMIT ?1"
    )?;

    let rows = stmt.query_map(rusqlite::params![limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::command_edit::TokenType;
    use crate::intelligence::schema;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_learn_sequences() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Create tokens first
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('commit', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();

        let tokens = vec![
            AnalyzedToken::new("git", TokenType::Command, 0),
            AnalyzedToken::new("commit", TokenType::Subcommand, 1),
        ];
        let token_ids = vec![1, 2];

        learn_sequences(&conn, &tokens, &token_ids, Some(1), true, now).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ci_sequences",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_query_sequences() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Create tokens
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (2, 'commit', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();

        // Create sequence
        conn.execute(
            "INSERT INTO ci_sequences (context_token_id, context_position, base_command_id, next_token_id, frequency, success_count, last_seen)
             VALUES (1, 0, 1, 2, 10, 8, ?1)",
            [now],
        ).unwrap();

        let results = query_sequences(&conn, "git", 0, Some("git"), 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "commit");
        assert_eq!(results[0].1, 10);
    }
}
