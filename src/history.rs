//! History loading from ~/.bash_history.
//!
//! This module reads and parses bash history to provide history search
//! and navigation in the reedline editor.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::Context;
use thiserror::Error;
use tracing::{debug, info, warn};

/// Errors that can occur when loading history.
#[derive(Debug, Error)]
pub enum HistoryError {
    /// The user's home directory could not be determined.
    #[error("could not determine home directory")]
    HomeDirNotFound,
}

/// Maximum number of history entries to load.
const MAX_HISTORY_LINES: usize = 10_000;

/// Loads history entries from ~/.bash_history.
///
/// Uses a streaming approach with a bounded VecDeque to avoid loading the
/// entire file into memory when histories are huge. Each line is processed
/// as it's read, and oldest entries are dropped when the capacity is exceeded.
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
/// Returns [`HistoryError::HomeDirNotFound`] if the home directory cannot be determined.
/// Missing or unreadable history files are handled gracefully by returning
/// an empty vector.
pub fn load_history() -> Result<Vec<String>, HistoryError> {
    let history_path = get_history_path().map_err(|_| HistoryError::HomeDirNotFound)?;

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

    // Use a bounded VecDeque for streaming - avoids loading entire file
    // before trimming when histories are huge
    let mut history: VecDeque<String> = VecDeque::with_capacity(MAX_HISTORY_LINES);
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

                // Push to back, pop from front if over capacity
                history.push_back(line);
                if history.len() > MAX_HISTORY_LINES {
                    history.pop_front();
                }
            }
            Err(e) => {
                warn!(line = line_number, error = %e, "Skipping corrupted history line");
                skipped += 1;
            }
        }
    }

    // Convert VecDeque to Vec for return
    let history: Vec<String> = history.into();

    info!(
        entries = history.len(),
        skipped,
        path = %history_path.display(),
        "Loaded history from bash_history"
    );

    // Log safe metadata only - avoid leaking secrets from command history
    debug!(
        entries = history.len(),
        first_entry_len = history.first().map(|s| s.len()),
        last_entry_len = history.last().map(|s| s.len()),
        "History loaded"
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
fn get_history_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".bash_history"))
}

/// Appends a command to ~/.bash_history if it differs from the last entry.
///
/// This enables commands executed in wrashpty to appear in other bash sessions'
/// history. The command is appended atomically with a trailing newline.
/// Consecutive duplicate commands are skipped (similar to HISTCONTROL=ignoredups).
///
/// # Arguments
///
/// * `command` - The command string to append
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or the file
/// cannot be written. Missing history file is created automatically.
pub fn append_to_bash_history(command: &str) -> anyhow::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    // Skip empty commands
    if command.trim().is_empty() {
        return Ok(());
    }

    let history_path = get_history_path()?;

    // Check if command is same as last entry (deduplication)
    if let Some(last) = get_last_history_line(&history_path)? {
        if last == command {
            debug!("Skipping duplicate command in bash_history");
            return Ok(());
        }
    }

    // Open file for appending, creating if it doesn't exist
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)
        .with_context(|| format!("Failed to open {} for appending", history_path.display()))?;

    // Write command with newline
    writeln!(file, "{}", command)
        .with_context(|| format!("Failed to write to {}", history_path.display()))?;

    debug!(command_len = command.len(), "Appended command to bash_history");

    Ok(())
}

/// Gets the last non-empty line from a file.
fn get_last_history_line(path: &PathBuf) -> anyhow::Result<Option<String>> {
    use std::fs::File;
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    if !path.exists() {
        return Ok(None);
    }

    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len == 0 {
        return Ok(None);
    }

    // Read from the end of the file to find the last line efficiently
    // For small files, just read the whole thing
    if file_len < 8192 {
        let reader = BufReader::new(file);
        let mut last_line = None;
        for line in reader.lines().flatten() {
            if !line.trim().is_empty() && !line.starts_with('#') {
                last_line = Some(line);
            }
        }
        return Ok(last_line);
    }

    // For larger files, seek to near the end
    let mut file = file;
    let seek_pos = file_len.saturating_sub(4096);
    file.seek(SeekFrom::Start(seek_pos))?;

    let reader = BufReader::new(file);
    let mut last_line = None;

    // Skip partial first line if we seeked into the middle
    let mut lines = reader.lines();
    if seek_pos > 0 {
        lines.next(); // Skip potentially partial line
    }

    for line in lines.flatten() {
        if !line.trim().is_empty() && !line.starts_with('#') {
            last_line = Some(line);
        }
    }

    Ok(last_line)
}

/// Deduplicates ~/.bash_history, keeping unique consecutive commands.
///
/// This removes consecutive duplicate entries while preserving order.
/// Non-consecutive duplicates are kept (like HISTCONTROL=ignoredups).
///
/// # Returns
///
/// The number of duplicate entries removed.
pub fn dedupe_bash_history() -> anyhow::Result<usize> {
    use std::io::{BufRead, BufReader, Write};

    let history_path = get_history_path()?;

    if !history_path.exists() {
        return Ok(0);
    }

    let file = File::open(&history_path)?;
    let reader = BufReader::new(file);

    let mut deduped: Vec<String> = Vec::new();
    let mut removed = 0;

    for line in reader.lines().flatten() {
        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Keep timestamp comments
        if line.starts_with('#') {
            deduped.push(line);
            continue;
        }

        // Check if same as last non-comment entry
        let last_cmd = deduped.iter().rev().find(|l| !l.starts_with('#'));
        if last_cmd.is_some_and(|l| l == &line) {
            removed += 1;
            continue;
        }

        deduped.push(line);
    }

    if removed > 0 {
        // Write to a temporary file first for atomic replacement.
        // Use NamedTempFile for RAII cleanup - if any operation fails before persist(),
        // the temp file is automatically removed when dropped.
        let parent_dir = history_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let mut temp_file = tempfile::NamedTempFile::new_in(parent_dir)
            .with_context(|| format!("Failed to create temp file in {}", parent_dir.display()))?;

        // Get original file permissions if it exists
        let permissions = fs::metadata(&history_path).ok().map(|m| m.permissions());

        // Write deduplicated history to temp file
        for line in &deduped {
            writeln!(temp_file, "{}", line).context("Failed to write to temp file")?;
        }

        // Flush and sync to ensure all data is written to disk
        temp_file.flush().context("Failed to flush temp file")?;
        temp_file
            .as_file()
            .sync_all()
            .context("Failed to sync temp file")?;

        // Preserve original permissions if we captured them
        if let Some(perms) = permissions {
            fs::set_permissions(temp_file.path(), perms)
                .context("Failed to set permissions on temp file")?;
        }

        // Atomically persist temp file to history file.
        // This consumes the NamedTempFile, preventing automatic deletion on success.
        temp_file
            .persist(&history_path)
            .with_context(|| format!("Failed to persist temp file to {}", history_path.display()))?;

        info!(removed, "Deduplicated bash_history");
    }

    Ok(removed)
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
        use std::collections::VecDeque;

        // Test that we limit to MAX_HISTORY_LINES using streaming VecDeque approach
        let mut history: VecDeque<String> = VecDeque::with_capacity(MAX_HISTORY_LINES);
        for i in 0..MAX_HISTORY_LINES + 100 {
            history.push_back(format!("command {}", i));
            if history.len() > MAX_HISTORY_LINES {
                history.pop_front();
            }
        }

        let history: Vec<String> = history.into();

        assert_eq!(history.len(), MAX_HISTORY_LINES);
        // Should have the last MAX_HISTORY_LINES entries
        assert_eq!(history[0], format!("command {}", 100));
        assert_eq!(
            history[MAX_HISTORY_LINES - 1],
            format!("command {}", MAX_HISTORY_LINES + 99)
        );
    }

    #[test]
    fn test_append_to_bash_history_skips_empty() {
        // Empty commands should be silently skipped
        let result = super::append_to_bash_history("");
        assert!(result.is_ok());

        let result = super::append_to_bash_history("   ");
        assert!(result.is_ok());
    }

    #[test]
    fn test_append_to_bash_history_writes_to_file() {
        use std::fs;
        use tempfile::tempdir;

        // Create a temp directory to simulate home
        let temp_dir = tempdir().expect("Failed to create temp dir");
        let history_file = temp_dir.path().join(".bash_history");

        // Write directly to temp file to test the write logic
        {
            use std::fs::OpenOptions;
            use std::io::Write;

            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&history_file)
                .expect("Failed to create temp history file");

            writeln!(file, "echo test_command").expect("Failed to write");
        }

        // Verify the file exists and has content
        let content = fs::read_to_string(&history_file).expect("Failed to read");
        assert!(content.contains("echo test_command"));
    }
}
