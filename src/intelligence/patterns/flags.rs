//! Flag-value association learning.
//!
//! Learns common values for specific flags in different command contexts.

use rusqlite::Connection;

use crate::chrome::command_edit::TokenType;
use crate::intelligence::error::CIError;
use crate::intelligence::tokenizer::detect_value_type;
use crate::intelligence::types::AnalyzedToken;

/// Learns flag-value associations from a command.
pub fn learn_flag_values(
    conn: &Connection,
    tokens: &[AnalyzedToken],
    token_ids: &[i64],
    base_command_id: Option<i64>,
    timestamp: i64,
) -> Result<(), CIError> {
    let Some(base_id) = base_command_id else {
        return Ok(());
    };

    // Find subcommand if present
    // Safely check both tokens and token_ids have elements at index 1
    let subcommand_id =
        if tokens.len() > 1 && token_ids.len() > 1 && tokens[1].token_type == TokenType::Subcommand
        {
            Some(token_ids[1])
        } else {
            None
        };

    // Look for flag -> value pairs
    for i in 0..tokens.len() - 1 {
        if tokens[i].token_type != TokenType::Flag {
            continue;
        }

        let next = &tokens[i + 1];

        // Skip if next token is also a flag (boolean flag)
        if next.token_type == TokenType::Flag {
            continue;
        }

        // Skip pipes, redirects, and operators (type-first, text-fallback for old DB data)
        if matches!(
            next.token_type,
            TokenType::Pipe | TokenType::Redirect | TokenType::Operator
        ) || next.text == "|"
            || next.text == ">"
            || next.text == ">>"
            || next.text == "<"
            || next.text.ends_with('|')
        {
            continue;
        }

        let flag_text = &tokens[i].text;
        let value_text = &next.text;
        let value_type = detect_value_type(value_text);

        conn.execute(
            "INSERT INTO ci_flag_values
             (base_command_id, subcommand_id, flag_text, value_text, value_type, frequency, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
             ON CONFLICT(base_command_id, subcommand_id, flag_text, value_text)
             DO UPDATE SET frequency = frequency + 1, last_seen = ?6",
            rusqlite::params![base_id, subcommand_id, flag_text, value_text, value_type, timestamp],
        )?;
    }

    Ok(())
}

/// Queries common values for a specific flag.
///
/// Returns tuples of (value, frequency, last_seen).
pub fn query_flag_values(
    conn: &Connection,
    base_command: &str,
    subcommand: Option<&str>,
    flag: &str,
    limit: usize,
) -> Result<Vec<(String, u32, i64)>, CIError> {
    // Get base command ID
    let base_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [base_command],
            |row| row.get(0),
        )
        .ok();

    let Some(base_id) = base_id else {
        return Ok(Vec::new());
    };

    // Get subcommand ID if provided
    let subcommand_id: Option<i64> = subcommand.and_then(|sub| {
        conn.query_row("SELECT id FROM ci_tokens WHERE text = ?1", [sub], |row| {
            row.get(0)
        })
        .ok()
    });

    let mut stmt = if subcommand_id.is_some() {
        conn.prepare(
            "SELECT value_text, frequency, last_seen
             FROM ci_flag_values
             WHERE base_command_id = ?1
               AND subcommand_id = ?2
               AND flag_text = ?3
             ORDER BY frequency DESC
             LIMIT ?4",
        )?
    } else {
        conn.prepare(
            "SELECT value_text, frequency, last_seen
             FROM ci_flag_values
             WHERE base_command_id = ?1
               AND subcommand_id IS NULL
               AND flag_text = ?2
             ORDER BY frequency DESC
             LIMIT ?3",
        )?
    };

    let mut results = Vec::new();
    if let Some(sub_id) = subcommand_id {
        let rows = stmt.query_map(rusqlite::params![base_id, sub_id, flag, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows.flatten() {
            results.push(row);
        }
    } else {
        let rows = stmt.query_map(rusqlite::params![base_id, flag, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows.flatten() {
            results.push(row);
        }
    }

    // If no results with exact subcommand match, try without subcommand filter
    if results.is_empty() && subcommand.is_some() {
        let mut stmt = conn.prepare(
            "SELECT value_text, frequency, last_seen
             FROM ci_flag_values
             WHERE base_command_id = ?1
               AND flag_text = ?2
             ORDER BY frequency DESC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(rusqlite::params![base_id, flag, limit], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;

        for r in rows.flatten() {
            results.push(r);
        }
    }

    Ok(results)
}

/// Queries all flags used with a command.
pub fn query_flags_for_command(
    conn: &Connection,
    base_command: &str,
    subcommand: Option<&str>,
    limit: usize,
) -> Result<Vec<(String, u32)>, CIError> {
    // Get base command ID
    let base_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [base_command],
            |row| row.get(0),
        )
        .ok();

    let Some(base_id) = base_id else {
        return Ok(Vec::new());
    };

    // Get subcommand ID if provided
    let subcommand_id: Option<i64> = subcommand.and_then(|sub| {
        conn.query_row("SELECT id FROM ci_tokens WHERE text = ?1", [sub], |row| {
            row.get(0)
        })
        .ok()
    });

    let mut stmt = if subcommand_id.is_some() {
        conn.prepare(
            "SELECT flag_text, SUM(frequency) as total_freq
             FROM ci_flag_values
             WHERE base_command_id = ?1 AND subcommand_id = ?2
             GROUP BY flag_text
             ORDER BY total_freq DESC
             LIMIT ?3",
        )?
    } else {
        conn.prepare(
            "SELECT flag_text, SUM(frequency) as total_freq
             FROM ci_flag_values
             WHERE base_command_id = ?1
             GROUP BY flag_text
             ORDER BY total_freq DESC
             LIMIT ?2",
        )?
    };

    let mut results = Vec::new();
    if let Some(sub_id) = subcommand_id {
        let rows = stmt.query_map(rusqlite::params![base_id, sub_id, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        })?;
        for row in rows.flatten() {
            results.push(row);
        }
    } else {
        let rows = stmt.query_map(rusqlite::params![base_id, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
        })?;
        for row in rows.flatten() {
            results.push(row);
        }
    }

    Ok(results)
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
    fn test_learn_flag_values() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Create tokens
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'docker', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (2, 'run', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (3, '-p', 'Flag', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (4, '8080:8080', 'Argument', ?1, ?1)",
            [now],
        ).unwrap();

        let tokens = vec![
            AnalyzedToken::new("docker", TokenType::Command, 0),
            AnalyzedToken::new("run", TokenType::Subcommand, 1),
            AnalyzedToken::new("-p", TokenType::Flag, 2),
            AnalyzedToken::new("8080:8080", TokenType::Argument, 3),
        ];
        let token_ids = vec![1, 2, 3, 4];

        learn_flag_values(&conn, &tokens, &token_ids, Some(1), now).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ci_flag_values", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_query_flag_values() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Create tokens
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'docker', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (2, 'run', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();

        // Insert flag values directly
        conn.execute(
            "INSERT INTO ci_flag_values (base_command_id, subcommand_id, flag_text, value_text, value_type, frequency, last_seen)
             VALUES (1, 2, '-p', '8080:8080', 'port_mapping', 5, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_flag_values (base_command_id, subcommand_id, flag_text, value_text, value_type, frequency, last_seen)
             VALUES (1, 2, '-p', '3000:3000', 'port_mapping', 3, ?1)",
            [now],
        ).unwrap();

        let results = query_flag_values(&conn, "docker", Some("run"), "-p", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "8080:8080");
        assert_eq!(results[0].1, 5);
    }
}
