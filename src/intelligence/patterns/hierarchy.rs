//! Command hierarchy learning.
//!
//! Learns the hierarchical structure of commands:
//! - What tokens appear at each position
//! - What tokens follow which parent tokens
//! - Token roles (subcommand, flag, argument, value)

use rusqlite::Connection;

use crate::chrome::command_edit::TokenType;
use crate::intelligence::error::CIError;
use crate::intelligence::types::AnalyzedToken;

/// Hierarchy query result: (token_text, frequency, success_count, last_seen, role).
pub type HierarchyResult = (String, u32, u32, i64, Option<String>);

/// Learns command hierarchy from a tokenized command.
///
/// For each token at position N, records:
/// - The token itself
/// - Its position
/// - Its parent (token at position N-1)
/// - The base command (token at position 0)
/// - Its role based on token type
pub fn learn_hierarchy(
    conn: &Connection,
    tokens: &[AnalyzedToken],
    token_ids: &[i64],
    is_success: bool,
    timestamp: i64,
) -> Result<(), CIError> {
    if tokens.is_empty() {
        return Ok(());
    }

    let success_increment = if is_success { 1 } else { 0 };
    let base_command_id = token_ids.first().copied();

    for (position, (token, &token_id)) in tokens.iter().zip(token_ids.iter()).enumerate() {
        let parent_id = if position == 0 {
            None
        } else {
            Some(token_ids[position - 1])
        };

        let role = classify_token_role(token, position);

        conn.execute(
            "INSERT INTO ci_command_hierarchy
             (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)
             ON CONFLICT(token_id, position, parent_token_id, base_command_id)
             DO UPDATE SET
                 frequency = frequency + 1,
                 success_count = success_count + ?5,
                 last_seen = ?6,
                 role = COALESCE(role, excluded.role)",
            rusqlite::params![
                token_id,
                position,
                parent_id,
                base_command_id,
                success_increment,
                timestamp,
                role,
            ],
        )?;
    }

    Ok(())
}

/// Classifies a token's role based on its type and position.
fn classify_token_role(token: &AnalyzedToken, position: usize) -> Option<&'static str> {
    match (token.token_type, position) {
        (TokenType::Command, 0) => Some("command"),
        (TokenType::Subcommand, _) => Some("subcommand"),
        (TokenType::Flag, _) => Some("flag"),
        (TokenType::Path, _) => Some("path"),
        (TokenType::Url, _) => Some("url"),
        (TokenType::Argument, _) => Some("argument"),
        (TokenType::Locked, _) => Some("locked"),
        // Command appearing after position 0 is treated as subcommand
        (TokenType::Command, _) => Some("subcommand"),
    }
}

/// Queries hierarchy for suggestions at a given position.
///
/// Returns tuples of (token_text, frequency, success_count, last_seen, role).
pub fn query_hierarchy(
    conn: &Connection,
    position: usize,
    parent_token: Option<&str>,
    base_command: Option<&str>,
    limit: usize,
) -> Result<Vec<HierarchyResult>, CIError> {
    // Position 0: query base commands (parent is NULL)
    if position == 0 {
        return query_base_commands(conn, limit);
    }

    // Get parent token ID
    let parent_id: Option<i64> = parent_token.and_then(|text| {
        conn.query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [text],
            |row| row.get(0),
        )
        .ok()
    });

    // Get base command ID
    let base_id: Option<i64> = base_command.and_then(|text| {
        conn.query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [text],
            |row| row.get(0),
        )
        .ok()
    });

    // Query with both parent and base command for best specificity
    if let (Some(parent_id), Some(base_id)) = (parent_id, base_id) {
        let results = query_with_context(conn, position, parent_id, base_id, limit)?;
        if !results.is_empty() {
            return Ok(results);
        }
    }

    // Fallback: query with just parent, but ONLY for the same base command
    // This prevents showing unrelated commands (e.g., kubectl suggestions for git)
    if let (Some(parent_id), Some(base_id)) = (parent_id, base_id) {
        let results = query_with_parent_and_base(conn, position, parent_id, base_id, limit)?;
        if !results.is_empty() {
            return Ok(results);
        }
    }

    // Fallback: query by base command only (any parent within this command)
    if let Some(base_id) = base_id {
        let results = query_by_base_command(conn, position, base_id, limit)?;
        if !results.is_empty() {
            return Ok(results);
        }
    }

    // Last resort: query by position only - but ONLY when we have no base command context
    // This is intentionally limited to avoid polluting git suggestions with docker/kubectl tokens
    if base_command.is_none() {
        return query_by_position(conn, position, limit);
    }

    // If we have a base command but found nothing, return empty rather than wrong suggestions
    Ok(Vec::new())
}

/// Queries base commands (position 0, no parent).
fn query_base_commands(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<HierarchyResult>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.text, h.frequency, h.success_count, h.last_seen, h.role
         FROM ci_command_hierarchy h
         JOIN ci_tokens t ON t.id = h.token_id
         WHERE h.position = 0
           AND h.parent_token_id IS NULL
         ORDER BY h.frequency DESC
         LIMIT ?1",
    )?;

    let rows = stmt.query_map(rusqlite::params![limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

/// Queries with full context (parent + base command).
fn query_with_context(
    conn: &Connection,
    position: usize,
    parent_id: i64,
    base_id: i64,
    limit: usize,
) -> Result<Vec<HierarchyResult>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.text, h.frequency, h.success_count, h.last_seen, h.role
         FROM ci_command_hierarchy h
         JOIN ci_tokens t ON t.id = h.token_id
         WHERE h.position = ?1
           AND h.parent_token_id = ?2
           AND h.base_command_id = ?3
         ORDER BY h.frequency DESC
         LIMIT ?4",
    )?;

    let rows = stmt.query_map(rusqlite::params![position, parent_id, base_id, limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

/// Queries with parent token, filtered to only the given base command.
/// This prevents cross-command pollution (e.g., kubectl tokens appearing for git).
fn query_with_parent_and_base(
    conn: &Connection,
    position: usize,
    parent_id: i64,
    base_id: i64,
    limit: usize,
) -> Result<Vec<HierarchyResult>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.text, SUM(h.frequency) as freq, SUM(h.success_count) as success,
                MAX(h.last_seen) as last_seen, h.role
         FROM ci_command_hierarchy h
         JOIN ci_tokens t ON t.id = h.token_id
         WHERE h.position = ?1
           AND h.parent_token_id = ?2
           AND h.base_command_id = ?3
         GROUP BY h.token_id
         ORDER BY freq DESC
         LIMIT ?4",
    )?;

    let rows = stmt.query_map(rusqlite::params![position, parent_id, base_id, limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

/// Queries by base command only (any position within that command's hierarchy).
/// Used when we know the base command but the specific parent/position combo isn't found.
fn query_by_base_command(
    conn: &Connection,
    position: usize,
    base_id: i64,
    limit: usize,
) -> Result<Vec<HierarchyResult>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.text, SUM(h.frequency) as freq, SUM(h.success_count) as success,
                MAX(h.last_seen) as last_seen, h.role
         FROM ci_command_hierarchy h
         JOIN ci_tokens t ON t.id = h.token_id
         WHERE h.position = ?1
           AND h.base_command_id = ?2
         GROUP BY h.token_id
         ORDER BY freq DESC
         LIMIT ?3",
    )?;

    let rows = stmt.query_map(rusqlite::params![position, base_id, limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

/// Queries by position only (least specific).
fn query_by_position(
    conn: &Connection,
    position: usize,
    limit: usize,
) -> Result<Vec<HierarchyResult>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.text, SUM(h.frequency) as freq, SUM(h.success_count) as success,
                MAX(h.last_seen) as last_seen, h.role
         FROM ci_command_hierarchy h
         JOIN ci_tokens t ON t.id = h.token_id
         WHERE h.position = ?1
         GROUP BY h.token_id
         ORDER BY freq DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![position, limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
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
    use crate::intelligence::schema;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_learn_hierarchy() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Create tokens first
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (2, 'remote', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (3, 'add', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();

        let tokens = vec![
            AnalyzedToken::new("git", TokenType::Command, 0),
            AnalyzedToken::new("remote", TokenType::Subcommand, 1),
            AnalyzedToken::new("add", TokenType::Subcommand, 2),
        ];
        let token_ids = vec![1, 2, 3];

        learn_hierarchy(&conn, &tokens, &token_ids, true, now).unwrap();

        // Verify hierarchy was created
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ci_command_hierarchy",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 3);

        // Verify relationships
        let parent_of_remote: Option<i64> = conn.query_row(
            "SELECT parent_token_id FROM ci_command_hierarchy WHERE token_id = 2",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(parent_of_remote, Some(1)); // parent is git

        let parent_of_add: Option<i64> = conn.query_row(
            "SELECT parent_token_id FROM ci_command_hierarchy WHERE token_id = 3",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(parent_of_add, Some(2)); // parent is remote
    }

    #[test]
    fn test_query_hierarchy() {
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
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (3, 'push', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();

        // Create hierarchy entries
        // git at position 0
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (1, 0, NULL, 1, 100, 90, ?1, 'command')",
            [now],
        ).unwrap();
        // commit after git
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (2, 1, 1, 1, 50, 45, ?1, 'subcommand')",
            [now],
        ).unwrap();
        // push after git
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (3, 1, 1, 1, 30, 28, ?1, 'subcommand')",
            [now],
        ).unwrap();

        // Query position 1 with parent=git
        let results = query_hierarchy(&conn, 1, Some("git"), Some("git"), 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "commit"); // higher frequency
        assert_eq!(results[1].0, "push");
    }

    #[test]
    fn test_query_base_commands() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Create tokens
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (2, 'cargo', 'Command', ?1, ?1)",
            [now],
        ).unwrap();

        // Create hierarchy entries for base commands
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (1, 0, NULL, 1, 100, 90, ?1, 'command')",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (2, 0, NULL, 2, 50, 45, ?1, 'command')",
            [now],
        ).unwrap();

        // Query position 0 (base commands)
        let results = query_hierarchy(&conn, 0, None, None, 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "git"); // higher frequency
        assert_eq!(results[1].0, "cargo");
    }
}
