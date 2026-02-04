//! User-defined patterns and aliases.
//!
//! Allows users to define custom suggestion rules and aliases.

use rusqlite::Connection;
use tracing::debug;

use super::error::CIError;
use super::scoring::{self, ContextMatch};
use super::types::{
    Suggestion, SuggestionContext, SuggestionMetadata, SuggestionSource, UserAlias, UserPattern,
    UserPatternType,
};

/// Adds a user-defined pattern.
pub fn add_pattern(conn: &Connection, pattern: UserPattern) -> Result<i64, CIError> {
    let now = chrono::Utc::now().timestamp();
    let pattern_type = format!("{:?}", pattern.pattern_type);

    conn.execute(
        "INSERT INTO ci_user_patterns
         (pattern_type, trigger_pattern, suggestion, description, priority, enabled, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            pattern_type,
            pattern.trigger,
            pattern.suggestion,
            pattern.description,
            pattern.priority,
            pattern.enabled,
            now,
        ],
    )?;

    let id = conn.last_insert_rowid();
    debug!(id, trigger = %pattern.trigger, "Added user pattern");

    Ok(id)
}

/// Updates an existing pattern.
pub fn update_pattern(conn: &Connection, id: i64, pattern: UserPattern) -> Result<(), CIError> {
    let pattern_type = format!("{:?}", pattern.pattern_type);

    conn.execute(
        "UPDATE ci_user_patterns SET
         pattern_type = ?1, trigger_pattern = ?2, suggestion = ?3,
         description = ?4, priority = ?5, enabled = ?6
         WHERE id = ?7",
        rusqlite::params![
            pattern_type,
            pattern.trigger,
            pattern.suggestion,
            pattern.description,
            pattern.priority,
            pattern.enabled,
            id,
        ],
    )?;

    Ok(())
}

/// Removes a pattern by ID.
pub fn remove_pattern(conn: &Connection, id: i64) -> Result<(), CIError> {
    conn.execute("DELETE FROM ci_user_patterns WHERE id = ?1", [id])?;
    debug!(id, "Removed user pattern");
    Ok(())
}

/// Lists user patterns.
pub fn list_patterns(
    conn: &Connection,
    pattern_type: Option<UserPatternType>,
) -> Result<Vec<UserPattern>, CIError> {
    let mut stmt = if let Some(pt) = pattern_type {
        let _type_str = format!("{:?}", pt);
        conn.prepare(
            "SELECT id, pattern_type, trigger_pattern, suggestion, description, priority, enabled, use_count
             FROM ci_user_patterns
             WHERE pattern_type = ?1
             ORDER BY priority DESC, use_count DESC"
        )?
    } else {
        conn.prepare(
            "SELECT id, pattern_type, trigger_pattern, suggestion, description, priority, enabled, use_count
             FROM ci_user_patterns
             ORDER BY priority DESC, use_count DESC"
        )?
    };

    let rows = if let Some(pt) = pattern_type {
        let type_str = format!("{:?}", pt);
        stmt.query_map([type_str], parse_pattern_row)?
    } else {
        stmt.query_map([], parse_pattern_row)?
    };

    let mut patterns = Vec::new();
    for row in rows.flatten() {
        patterns.push(row);
    }

    Ok(patterns)
}

fn parse_pattern_row(row: &rusqlite::Row) -> rusqlite::Result<UserPattern> {
    let type_str: String = row.get(1)?;
    let pattern_type = match type_str.as_str() {
        "Alias" => UserPatternType::Alias,
        "Sequence" => UserPatternType::Sequence,
        "FileType" => UserPatternType::FileType,
        "Trigger" => UserPatternType::Trigger,
        _ => UserPatternType::Trigger,
    };

    Ok(UserPattern {
        id: row.get(0)?,
        pattern_type,
        trigger: row.get(2)?,
        suggestion: row.get(3)?,
        description: row.get(4)?,
        priority: row.get(5)?,
        enabled: row.get(6)?,
        use_count: row.get(7)?,
    })
}

/// Enables or disables a pattern.
pub fn set_pattern_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<(), CIError> {
    conn.execute(
        "UPDATE ci_user_patterns SET enabled = ?1 WHERE id = ?2",
        rusqlite::params![enabled, id],
    )?;
    Ok(())
}

/// Increments the use count for a pattern.
pub fn record_pattern_use(conn: &Connection, id: i64) -> Result<(), CIError> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE ci_user_patterns SET use_count = use_count + 1, last_used = ?1 WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    Ok(())
}

// ============================================================================
// Alias Management
// ============================================================================

/// Adds a user-defined alias.
pub fn add_alias(
    conn: &Connection,
    alias: &str,
    expansion: &str,
    description: Option<&str>,
) -> Result<i64, CIError> {
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO ci_user_aliases (alias, expansion, description, enabled, created_at)
         VALUES (?1, ?2, ?3, 1, ?4)",
        rusqlite::params![alias, expansion, description, now],
    )?;

    let id = conn.last_insert_rowid();
    debug!(id, alias, "Added user alias");

    Ok(id)
}

/// Removes an alias.
pub fn remove_alias(conn: &Connection, alias: &str) -> Result<(), CIError> {
    conn.execute("DELETE FROM ci_user_aliases WHERE alias = ?1", [alias])?;
    debug!(alias, "Removed user alias");
    Ok(())
}

/// Lists all aliases.
pub fn list_aliases(conn: &Connection) -> Result<Vec<UserAlias>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT id, alias, expansion, description, enabled, use_count
         FROM ci_user_aliases
         ORDER BY use_count DESC, alias"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(UserAlias {
            id: row.get(0)?,
            alias: row.get(1)?,
            expansion: row.get(2)?,
            description: row.get(3)?,
            enabled: row.get(4)?,
            use_count: row.get(5)?,
        })
    })?;

    let mut aliases = Vec::new();
    for row in rows.flatten() {
        aliases.push(row);
    }

    Ok(aliases)
}

/// Expands an alias if it exists.
pub fn expand_alias(conn: &Connection, text: &str) -> Result<Option<String>, CIError> {
    let expansion: Option<String> = conn
        .query_row(
            "SELECT expansion FROM ci_user_aliases WHERE alias = ?1 AND enabled = 1",
            [text],
            |row| row.get(0),
        )
        .ok();

    if expansion.is_some() {
        // Record usage
        let now = chrono::Utc::now().timestamp();
        let _ = conn.execute(
            "UPDATE ci_user_aliases SET use_count = use_count + 1, last_used = ?1 WHERE alias = ?2",
            rusqlite::params![now, text],
        );
    }

    Ok(expansion)
}

// ============================================================================
// Suggestion Integration
// ============================================================================

/// Gets suggestions from user patterns.
pub fn suggest_from_patterns(
    conn: &Connection,
    context: &SuggestionContext,
) -> Result<Vec<Suggestion>, CIError> {
    let mut suggestions = Vec::new();

    // Build context string for matching
    let context_str = if context.preceding_tokens.is_empty() {
        context.partial.clone()
    } else {
        let prefix: String = context
            .preceding_tokens
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} {}", prefix, context.partial)
    };

    // Query matching patterns
    let mut stmt = conn.prepare(
        "SELECT id, pattern_type, trigger_pattern, suggestion, description, priority, use_count
         FROM ci_user_patterns
         WHERE enabled = 1
           AND (?1 LIKE trigger_pattern || '%' OR trigger_pattern LIKE ?1 || '%')
         ORDER BY priority DESC, use_count DESC
         LIMIT 10"
    )?;

    let rows = stmt.query_map([&context_str], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i32>(5)?,
            row.get::<_, u32>(6)?,
        ))
    })?;

    for row in rows.flatten() {
        let (_id, type_str, _trigger, suggestion, description, priority, use_count) = row;

        let source = if type_str == "Alias" {
            SuggestionSource::UserAlias
        } else {
            SuggestionSource::UserPattern
        };

        let score = scoring::compute_score(
            use_count,
            0,
            None,
            ContextMatch::Exact,
            source,
        ) + (priority as f64 * 0.5);

        suggestions.push(Suggestion {
            text: suggestion,
            source,
            score,
            metadata: SuggestionMetadata {
                frequency: use_count,
                description,
                ..Default::default()
            },
        });
    }

    // Also check aliases
    let mut alias_stmt = conn.prepare(
        "SELECT alias, expansion, description, use_count
         FROM ci_user_aliases
         WHERE enabled = 1 AND alias LIKE ?1 || '%'
         ORDER BY use_count DESC
         LIMIT 10"
    )?;

    let alias_rows = alias_stmt.query_map([&context.partial], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, u32>(3)?,
        ))
    })?;

    for row in alias_rows.flatten() {
        let (alias, expansion, description, use_count) = row;

        let score = scoring::compute_score(
            use_count,
            0,
            None,
            ContextMatch::Exact,
            SuggestionSource::UserAlias,
        );

        // Show alias with expansion preview
        let display = format!("{} -> {}", alias, expansion);
        suggestions.push(Suggestion {
            text: expansion,
            source: SuggestionSource::UserAlias,
            score,
            metadata: SuggestionMetadata {
                frequency: use_count,
                description: Some(description.unwrap_or(display)),
                ..Default::default()
            },
        });
    }

    Ok(suggestions)
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
    fn test_add_and_list_patterns() {
        let conn = setup_test_db();

        let pattern = UserPattern {
            id: 0,
            pattern_type: UserPatternType::Sequence,
            trigger: "git pull".to_string(),
            suggestion: "cargo build".to_string(),
            description: Some("Build after pull".to_string()),
            priority: 10,
            enabled: true,
            use_count: 0,
        };

        let id = add_pattern(&conn, pattern).unwrap();
        assert!(id > 0);

        let patterns = list_patterns(&conn, None).unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].trigger, "git pull");
    }

    #[test]
    fn test_add_and_expand_alias() {
        let conn = setup_test_db();

        add_alias(&conn, "gs", "git status", Some("Git status shortcut")).unwrap();

        let expansion = expand_alias(&conn, "gs").unwrap();
        assert_eq!(expansion, Some("git status".to_string()));

        // Check use count was incremented
        let aliases = list_aliases(&conn).unwrap();
        assert_eq!(aliases[0].use_count, 1);
    }

    #[test]
    fn test_remove_pattern() {
        let conn = setup_test_db();

        let pattern = UserPattern {
            id: 0,
            pattern_type: UserPatternType::Trigger,
            trigger: "test".to_string(),
            suggestion: "result".to_string(),
            description: None,
            priority: 0,
            enabled: true,
            use_count: 0,
        };

        let id = add_pattern(&conn, pattern).unwrap();
        remove_pattern(&conn, id).unwrap();

        let patterns = list_patterns(&conn, None).unwrap();
        assert!(patterns.is_empty());
    }
}
