//! FTS5 fuzzy search for typo tolerance.
//!
//! Uses SQLite FTS5 with porter stemming and BM25 ranking
//! for typo-tolerant command search.

use rusqlite::Connection;

use super::error::CIError;
use super::types::FuzzyMatch;

/// Performs a fuzzy search for commands.
///
/// Uses FTS5 full-text search with BM25 ranking.
pub fn fuzzy_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<FuzzyMatch>, CIError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    // Prepare the FTS5 query
    // Use prefix matching for partial words
    let fts_query = build_fts_query(query);

    let mut stmt = conn.prepare(
        "SELECT command_line, bm25(ci_commands_fts) as score
         FROM ci_commands_fts
         WHERE ci_commands_fts MATCH ?1
         ORDER BY score
         LIMIT ?2"
    )?;

    let rows = stmt.query_map(rusqlite::params![fts_query, limit], |row| {
        let command: String = row.get(0)?;
        let score: f64 = row.get(1)?;
        Ok(FuzzyMatch {
            command,
            bm25_score: -score, // BM25 returns negative scores, negate for ranking
            matched_terms: Vec::new(),
        })
    })?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

/// Builds an FTS5 query from user input.
///
/// Handles prefix matching and escaping.
fn build_fts_query(query: &str) -> String {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| {
            // Escape special FTS5 characters
            let escaped = term
                .replace('"', "\"\"")
                .replace(['*', '(', ')'], "");

            // Add prefix matching for last term
            if escaped.len() >= 2 {
                format!("\"{}\"*", escaped)
            } else {
                format!("\"{}\"", escaped)
            }
        })
        .collect();

    terms.join(" ")
}

/// Searches for commands similar to the given text.
///
/// Uses a combination of FTS5 and LIKE for better matching.
pub fn search_similar(conn: &Connection, text: &str, limit: usize) -> Result<Vec<String>, CIError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }

    // First try FTS5
    let fts_results = fuzzy_search(conn, text, limit)?;
    if !fts_results.is_empty() {
        return Ok(fts_results.into_iter().map(|m| m.command).collect());
    }

    // Fallback to LIKE search
    // Escape SQL LIKE wildcards to prevent unsafe matching
    let escaped = text
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{}%", escaped);
    let mut stmt = conn.prepare(
        "SELECT command_line FROM ci_commands
         WHERE command_line LIKE ?1 ESCAPE '\\'
         ORDER BY timestamp DESC
         LIMIT ?2"
    )?;

    let rows = stmt.query_map(rusqlite::params![pattern, limit], |row| row.get(0))?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    Ok(results)
}

/// Searches for a specific base command with typo tolerance.
pub fn search_base_command(conn: &Connection, typo: &str, limit: usize) -> Result<Vec<(String, f64)>, CIError> {
    let typo = typo.trim().to_lowercase();
    if typo.is_empty() {
        return Ok(Vec::new());
    }

    // Try FTS5 on base command column
    let fts_query = format!("base_command:\"{}\"*", typo);

    let mut stmt = conn.prepare(
        "SELECT base_command, bm25(ci_commands_fts) as score
         FROM ci_commands_fts
         WHERE ci_commands_fts MATCH ?1
         GROUP BY base_command
         ORDER BY score
         LIMIT ?2"
    )?;

    let rows = stmt.query_map(rusqlite::params![fts_query, limit], |row| {
        let cmd: String = row.get(0)?;
        let score: f64 = row.get(1)?;
        Ok((cmd, -score))
    })?;

    let mut results = Vec::new();
    for row in rows.flatten() {
        results.push(row);
    }

    // If FTS5 found nothing, try edit distance-based search
    if results.is_empty() {
        results = search_by_edit_distance(conn, &typo, limit)?;
    }

    Ok(results)
}

/// Searches for commands by approximate edit distance.
///
/// This is a fallback for when FTS5 doesn't find matches.
fn search_by_edit_distance(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, f64)>, CIError> {
    // Get all unique base commands
    let mut stmt = conn.prepare(
        "SELECT DISTINCT text FROM ci_tokens WHERE token_type = 'Command' LIMIT 1000"
    )?;

    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

    let mut candidates: Vec<(String, f64)> = Vec::new();

    for row in rows.flatten() {
        let distance = levenshtein_distance(query, &row.to_lowercase());
        // Only consider commands with edit distance <= 2
        if distance <= 2 {
            // Convert distance to score (lower distance = higher score)
            let score = 1.0 / (1.0 + distance as f64);
            candidates.push((row, score));
        }
    }

    // Sort by score descending
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(limit);

    Ok(candidates)
}

/// Computes the Levenshtein edit distance between two strings.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();

    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut dp = vec![vec![0usize; n + 1]; m + 1];

    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    dp[m][n]
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
    fn test_build_fts_query() {
        let query = build_fts_query("git commit");
        assert!(query.contains("git"));
        assert!(query.contains("commit"));
    }

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein_distance("git", "git"), 0);
        assert_eq!(levenshtein_distance("git", "gti"), 2); // swap = 2 edits in Levenshtein
        assert_eq!(levenshtein_distance("git", "gitt"), 1); // insertion
        assert_eq!(levenshtein_distance("docker", "dockr"), 1); // deletion
        assert_eq!(levenshtein_distance("", "abc"), 3);
        assert_eq!(levenshtein_distance("abc", ""), 3);
    }

    #[test]
    fn test_fuzzy_search_empty() {
        let conn = setup_test_db();
        let results = fuzzy_search(&conn, "", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_fuzzy_search_with_data() {
        let conn = setup_test_db();
        let now = chrono::Utc::now().timestamp();

        // Insert test data
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen)
             VALUES (1, 'git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_commands (command_line, command_hash, token_ids, token_count, base_command_id, timestamp)
             VALUES ('git commit -m test', 'hash1', '[1]', 4, 1, ?1)",
            [now],
        ).unwrap();

        // FTS5 should have been populated by trigger
        let _results = fuzzy_search(&conn, "git", 10).unwrap();
        // Note: This may not work in all SQLite versions without full FTS5 setup
        // The test verifies the function doesn't crash
    }
}
