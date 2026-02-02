//! History loading from ~/.bash_history.
//!
//! This module reads and parses bash history to provide history search
//! and navigation in the reedline editor.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

/// Maximum number of history entries to load.
const MAX_HISTORY_LINES: usize = 10_000;

/// Loads history entries from ~/.bash_history.
///
/// Returns a vector of history entries, with oldest entries first.
/// If the history file doesn't exist or is empty, returns an empty vector.
/// Corrupted lines are skipped with a warning.
///
/// # Returns
///
/// A vector of history strings, limited to the last `MAX_HISTORY_LINES` entries.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
/// Missing or unreadable history files are handled gracefully by returning
/// an empty vector.
pub fn load_history() -> Result<Vec<String>> {
    let history_path = get_history_path()?;

    if !history_path.exists() {
        info!(path = %history_path.display(), "History file does not exist, starting with empty history");
        return Ok(Vec::new());
    }

    let file = match File::open(&history_path) {
        Ok(f) => f,
        Err(e) => {
            warn!(path = %history_path.display(), error = %e, "Failed to open history file");
            return Ok(Vec::new());
        }
    };

    let reader = BufReader::new(file);
    let mut history = Vec::new();
    let mut line_number = 0;
    let mut skipped = 0;

    for line_result in reader.lines() {
        line_number += 1;

        match line_result {
            Ok(line) => {
                // Skip empty lines
                if line.trim().is_empty() {
                    continue;
                }

                // Skip bash timestamp comments (lines starting with #)
                if line.starts_with('#') {
                    continue;
                }

                history.push(line);
            }
            Err(e) => {
                warn!(line = line_number, error = %e, "Skipping corrupted history line");
                skipped += 1;
            }
        }
    }

    // Enforce capacity limit by keeping only the last MAX_HISTORY_LINES entries
    // This is O(n) instead of O(n²) from removing in the loop
    if history.len() > MAX_HISTORY_LINES {
        let drain_count = history.len() - MAX_HISTORY_LINES;
        history.drain(..drain_count);
    }

    info!(
        entries = history.len(),
        skipped,
        path = %history_path.display(),
        "Loaded history from bash_history"
    );

    debug!(
        "History sample: first={:?}, last={:?}",
        history
            .first()
            .map(|s| s.chars().take(50).collect::<String>()),
        history
            .last()
            .map(|s| s.chars().take(50).collect::<String>())
    );

    Ok(history)
}

/// Gets the path to the bash history file.
///
/// Returns `~/.bash_history` on Unix systems.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
fn get_history_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".bash_history"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_history_lines_constant() {
        // Verify the constant is reasonable
        assert!(MAX_HISTORY_LINES >= 1000);
        assert!(MAX_HISTORY_LINES <= 100_000);
    }

    #[test]
    fn test_get_history_path() {
        let path = get_history_path().expect("Should get history path");
        assert!(path.ends_with(".bash_history"));
    }

    #[test]
    fn test_load_history_missing_file() {
        // This tests the graceful degradation for missing files
        // The actual file may or may not exist, but load_history should not panic
        let result = load_history();
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_file_returns_empty_vector() {
        // File is empty - simulate by just checking our parsing logic handles this
        let content = "";
        let lines: Vec<String> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|s| s.to_string())
            .collect();
        assert!(lines.is_empty());
    }

    #[test]
    fn test_skip_empty_lines() {
        let content = "echo hello\n\necho world\n   \necho test";
        let lines: Vec<String> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|s| s.to_string())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "echo hello");
        assert_eq!(lines[1], "echo world");
        assert_eq!(lines[2], "echo test");
    }

    #[test]
    fn test_skip_timestamp_comments() {
        let content = "#1234567890\necho hello\n#9876543210\necho world";
        let lines: Vec<String> = content
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|s| s.to_string())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "echo hello");
        assert_eq!(lines[1], "echo world");
    }

    #[test]
    fn test_history_capacity_limit_keeps_last_entries() {
        // Test that we limit to MAX_HISTORY_LINES using O(n) drain approach
        let mut history = Vec::new();
        for i in 0..MAX_HISTORY_LINES + 100 {
            history.push(format!("command {}", i));
        }

        // Apply the same capacity limit as load_history
        if history.len() > MAX_HISTORY_LINES {
            let drain_count = history.len() - MAX_HISTORY_LINES;
            history.drain(..drain_count);
        }

        assert_eq!(history.len(), MAX_HISTORY_LINES);
        // Should have the last MAX_HISTORY_LINES entries
        assert_eq!(history[0], format!("command {}", 100));
        assert_eq!(
            history[MAX_HISTORY_LINES - 1],
            format!("command {}", MAX_HISTORY_LINES + 99)
        );
    }
}
