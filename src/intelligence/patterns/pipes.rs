//! Pipe chain pattern learning.
//!
//! Learns what commands typically follow pipes in different contexts.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::intelligence::error::CIError;
use crate::intelligence::tokenizer::{compute_command_hash, split_at_pipes, token_type_to_string};
use crate::intelligence::types::AnalyzedToken;

/// Learns pipe chain patterns from a command.
pub fn learn_pipe_chains(
    conn: &Connection,
    token_cache: &mut HashMap<String, i64>,
    tokens: &[AnalyzedToken],
    base_command_id: Option<i64>,
    timestamp: i64,
) -> Result<(), CIError> {
    let segments = split_at_pipes(tokens);
    if segments.len() < 2 {
        return Ok(());
    }

    // For each pipe transition
    for i in 0..segments.len() - 1 {
        let pre_pipe = &segments[i];
        let post_pipe = &segments[i + 1];

        if pre_pipe.is_empty() || post_pipe.is_empty() {
            continue;
        }

        // Hash of pre-pipe segment
        let pre_pipe_text: String = pre_pipe
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let pre_pipe_hash = compute_command_hash(&pre_pipe_text);

        // First token after pipe is the pipe command
        let pipe_command = &post_pipe[0];
        let pipe_command_id = get_or_create_token(conn, token_cache, &pipe_command.text, pipe_command.token_type, timestamp)?;

        // Full post-pipe chain
        let full_chain: String = post_pipe
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        conn.execute(
            "INSERT INTO ci_pipe_chains
             (pre_pipe_base_cmd_id, pre_pipe_hash, pipe_command_id, full_chain, chain_length, frequency, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
             ON CONFLICT(pre_pipe_hash, pipe_command_id)
             DO UPDATE SET frequency = frequency + 1, last_seen = ?6",
            rusqlite::params![
                base_command_id,
                pre_pipe_hash,
                pipe_command_id,
                full_chain,
                post_pipe.len(),
                timestamp,
            ],
        )?;
    }

    Ok(())
}

/// Gets or creates a token (helper for pipe learning).
///
/// Uses INSERT OR IGNORE + SELECT pattern to avoid race conditions
/// in concurrent access scenarios.
fn get_or_create_token(
    conn: &Connection,
    cache: &mut HashMap<String, i64>,
    text: &str,
    token_type: crate::chrome::command_edit::TokenType,
    timestamp: i64,
) -> Result<i64, CIError> {
    if let Some(&id) = cache.get(text) {
        // Update frequency/last_seen even on cache hit
        conn.execute(
            "UPDATE ci_tokens SET frequency = frequency + 1, last_seen = ?1 WHERE id = ?2",
            rusqlite::params![timestamp, id],
        )?;
        return Ok(id);
    }

    let type_str = token_type_to_string(token_type);

    // Use INSERT OR IGNORE to safely handle concurrent inserts
    conn.execute(
        "INSERT OR IGNORE INTO ci_tokens (text, token_type, frequency, first_seen, last_seen)
         VALUES (?1, ?2, 1, ?3, ?3)",
        rusqlite::params![text, type_str, timestamp],
    )?;

    // Always SELECT to get the id (whether we just inserted or it already existed)
    let id: i64 = conn.query_row(
        "SELECT id FROM ci_tokens WHERE text = ?1",
        [text],
        |row| row.get(0),
    )?;

    // Update frequency if the row already existed (INSERT OR IGNORE doesn't update)
    conn.execute(
        "UPDATE ci_tokens SET frequency = frequency + 1, last_seen = ?1 WHERE id = ?2 AND first_seen < ?1",
        rusqlite::params![timestamp, id],
    )?;

    cache.insert(text.to_string(), id);
    Ok(id)
}

/// Queries pipe chain suggestions.
///
/// Returns tuples of (pipe_command, frequency, last_seen).
pub fn query_pipe_chains(
    conn: &Connection,
    pre_pipe_command: &str,
    limit: usize,
) -> Result<Vec<(String, u32, i64)>, CIError> {
    // Get base command ID for the pre-pipe command
    let base_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [pre_pipe_command],
            |row| row.get(0),
        )
        .ok();

    let mut stmt = if base_id.is_some() {
        conn.prepare(
            "SELECT t.text, p.frequency, p.last_seen
             FROM ci_pipe_chains p
             JOIN ci_tokens t ON t.id = p.pipe_command_id
             WHERE p.pre_pipe_base_cmd_id = ?1
             ORDER BY p.frequency DESC
             LIMIT ?2"
        )?
    } else {
        // Fallback: query all pipe chains
        conn.prepare(
            "SELECT t.text, SUM(p.frequency) as total_freq, MAX(p.last_seen) as last_seen
             FROM ci_pipe_chains p
             JOIN ci_tokens t ON t.id = p.pipe_command_id
             GROUP BY t.text
             ORDER BY total_freq DESC
             LIMIT ?1"
        )?
    };

    let mut results = Vec::new();
    if let Some(base_id) = base_id {
        let rows = stmt.query_map(rusqlite::params![base_id, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, i64>(2)?))
        })?;
        for row in rows.flatten() {
            results.push(row);
        }
    } else {
        let rows = stmt.query_map(rusqlite::params![limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, i64>(2)?))
        })?;
        for row in rows.flatten() {
            results.push(row);
        }
    }

    Ok(results)
}

/// Queries the full chain for a pipe command.
pub fn query_full_chain(
    conn: &Connection,
    pre_pipe_hash: &str,
    pipe_command: &str,
) -> Result<Option<String>, CIError> {
    let pipe_cmd_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [pipe_command],
            |row| row.get(0),
        )
        .ok();

    let Some(pipe_cmd_id) = pipe_cmd_id else {
        return Ok(None);
    };

    let chain: Option<String> = conn
        .query_row(
            "SELECT full_chain FROM ci_pipe_chains WHERE pre_pipe_hash = ?1 AND pipe_command_id = ?2",
            rusqlite::params![pre_pipe_hash, pipe_cmd_id],
            |row| row.get(0),
        )
        .ok();

    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    use crate::intelligence::schema;
    use crate::intelligence::tokenizer::analyze_command;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_learn_pipe_chains() {
        let conn = setup_test_db();
        let mut cache = HashMap::new();
        let now = chrono::Utc::now().timestamp();

        // Create base command token
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'cat', 'Command', ?1, ?1)",
            [now],
        ).unwrap();

        let tokens = analyze_command("cat file.txt | grep test | wc -l");
        learn_pipe_chains(&conn, &mut cache, &tokens, Some(1), now).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ci_pipe_chains",
            [],
            |row| row.get(0),
        ).unwrap();
        // Should have 2 pipe transitions: cat->grep, grep->wc
        assert_eq!(count, 2);
    }

    #[test]
    fn test_query_pipe_chains() {
        let conn = setup_test_db();
        let mut cache = HashMap::new();
        let now = chrono::Utc::now().timestamp();

        // Create base command token
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'cat', 'Command', ?1, ?1)",
            [now],
        ).unwrap();

        // Learn some pipe patterns
        let tokens = analyze_command("cat file.txt | grep test");
        learn_pipe_chains(&conn, &mut cache, &tokens, Some(1), now).unwrap();

        // Learn same pattern again to increase frequency
        learn_pipe_chains(&conn, &mut cache, &tokens, Some(1), now).unwrap();

        let results = query_pipe_chains(&conn, "cat", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0, "grep");
        assert_eq!(results[0].1, 2);
    }
}
