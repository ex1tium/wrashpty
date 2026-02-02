//! Completion providers for reedline.
//!
//! This module implements context-aware completion sources including:
//! - **FilesystemCompleter**: Path completion with directory detection and tilde expansion
//! - **PathCompleter**: Cached PATH executable completion with Unix permission checks
//! - **GitCompleter**: Git branch completion with timeout protection
//! - **WrashCompleter**: Context-aware orchestrator implementing reedline's Completer trait
//!
//! # Completion Strategy
//!
//! The `WrashCompleter` intelligently switches between completion sources based on context:
//! - If the partial contains `/`, `.`, or `~` → filesystem completion
//! - If at the first word (command position) → PATH executable completion
//! - If after `git checkout ` or `git branch ` → git branch completion
//! - Otherwise → filesystem completion (default)
//!
//! # Performance Characteristics
//!
//! - **PATH scanning**: Performed once at startup, cached in memory (typical: 1000-3000 executables)
//! - **Git commands**: 200ms timeout prevents blocking on slow repositories
//! - **Completion limits**: Filesystem (100), PATH (50) to prevent UI overflow

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use reedline::{Completer, Span, Suggestion};
use tracing::{debug, info};

/// Maximum number of filesystem completion suggestions.
const MAX_FILESYSTEM_SUGGESTIONS: usize = 100;

/// Maximum number of PATH executable suggestions.
const MAX_PATH_SUGGESTIONS: usize = 50;

/// Timeout for git commands in milliseconds.
const GIT_TIMEOUT_MS: u64 = 200;

/// Filesystem path completer with directory detection and tilde expansion.
///
/// Provides completion for filesystem paths, automatically detecting directories
/// (appending `/`) and expanding `~` to the user's home directory.
pub struct FilesystemCompleter;

impl FilesystemCompleter {
    /// Creates a new FilesystemCompleter.
    pub fn new() -> Self {
        Self
    }

    /// Completes a partial path, returning matching filesystem entries.
    ///
    /// # Arguments
    ///
    /// * `partial` - The partial path to complete
    ///
    /// # Returns
    ///
    /// A vector of suggestions sorted alphabetically, limited to MAX_FILESYSTEM_SUGGESTIONS.
    pub fn complete_path(&self, partial: &str) -> Vec<Suggestion> {
        debug!(partial = %partial, "Completing filesystem path");

        // Handle tilde expansion
        let expanded = if partial.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                if partial == "~" || partial == "~/" {
                    // Return home path with trailing slash to ensure parent detection works
                    format!("{}/", home.to_string_lossy())
                } else if let Some(suffix) = partial.strip_prefix("~/") {
                    if suffix.is_empty() {
                        // Handle edge case where strip_prefix returns empty string
                        format!("{}/", home.to_string_lossy())
                    } else {
                        home.join(suffix).to_string_lossy().to_string()
                    }
                } else {
                    // ~username style - not supported, return as-is
                    partial.to_string()
                }
            } else {
                partial.to_string()
            }
        } else {
            partial.to_string()
        };

        // Determine directory to scan and prefix to match
        let (dir_to_scan, prefix) = if expanded.ends_with('/') {
            // Partial is a directory path - scan it, match everything
            (PathBuf::from(&expanded), String::new())
        } else {
            // Partial is a partial filename - scan parent, match by prefix
            let path = Path::new(&expanded);
            let parent = path.parent().unwrap_or(Path::new("."));
            let file_prefix = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            (parent.to_path_buf(), file_prefix)
        };

        // Scan directory
        let entries = match fs::read_dir(&dir_to_scan) {
            Ok(entries) => entries,
            Err(e) => {
                debug!(dir = %dir_to_scan.display(), error = %e, "Failed to read directory");
                return Vec::new();
            }
        };

        let mut suggestions: Vec<Suggestion> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().to_string_lossy().to_string();

                // Filter by prefix
                if !prefix.is_empty() && !name.starts_with(&prefix) {
                    return None;
                }

                // Build the full completion value
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

                // Reconstruct the path for the suggestion
                let completion_value = if partial.starts_with('~') {
                    // Preserve tilde in the completion
                    if partial == "~" || partial == "~/" {
                        format!("~/{}{}", name, if is_dir { "/" } else { "" })
                    } else {
                        let base = if partial.ends_with('/') {
                            partial.to_string()
                        } else {
                            // Get the directory part of the partial
                            let path = Path::new(partial);
                            path.parent()
                                .map(|p| format!("{}/", p.display()))
                                .unwrap_or_else(|| "~/".to_string())
                        };
                        format!("{}{}{}", base, name, if is_dir { "/" } else { "" })
                    }
                } else if expanded.ends_with('/') {
                    format!("{}{}{}", partial, name, if is_dir { "/" } else { "" })
                } else {
                    let base_dir = Path::new(partial)
                        .parent()
                        .map(|p| {
                            let s = p.to_string_lossy();
                            if s.is_empty() {
                                String::new()
                            } else {
                                format!("{}/", s)
                            }
                        })
                        .unwrap_or_default();
                    format!("{}{}{}", base_dir, name, if is_dir { "/" } else { "" })
                };

                Some(Suggestion {
                    value: completion_value,
                    description: if is_dir {
                        Some("directory".to_string())
                    } else {
                        None
                    },
                    style: None,
                    extra: None,
                    span: Span::new(0, partial.len()),
                    append_whitespace: !is_dir,
                    match_indices: None,
                })
            })
            .collect();

        // Sort alphabetically
        suggestions.sort_by(|a, b| a.value.cmp(&b.value));

        // Limit results
        suggestions.truncate(MAX_FILESYSTEM_SUGGESTIONS);

        debug!(
            count = suggestions.len(),
            "Generated filesystem completions"
        );
        suggestions
    }
}

impl Default for FilesystemCompleter {
    fn default() -> Self {
        Self::new()
    }
}

/// PATH executable completer with caching.
///
/// Scans all directories in $PATH at construction time and caches the list
/// of executable names for fast completion lookups.
pub struct PathCompleter {
    /// Cached list of executable names from $PATH.
    executables: Vec<String>,
}

impl PathCompleter {
    /// Creates a new PathCompleter, scanning $PATH for executables.
    pub fn new() -> Self {
        let executables = Self::scan_path();
        info!(count = executables.len(), "PathCompleter initialized");
        Self { executables }
    }

    /// Scans all directories in $PATH for executable files.
    ///
    /// Returns a sorted, deduplicated list of executable names.
    fn scan_path() -> Vec<String> {
        let path_var = match env::var("PATH") {
            Ok(p) => p,
            Err(_) => {
                debug!("$PATH not set, returning empty executable list");
                return Vec::new();
            }
        };

        let mut executables: Vec<String> = path_var
            .split(':')
            .filter(|dir| !dir.is_empty())
            .flat_map(|dir| {
                fs::read_dir(dir)
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| {
                        let entry = entry.ok()?;
                        let path = entry.path();

                        // Check if it's a file and executable
                        if path.is_file() && Self::is_executable(&path) {
                            entry.file_name().to_str().map(String::from)
                        } else {
                            None
                        }
                    })
            })
            .collect();

        // Sort and deduplicate
        executables.sort();
        executables.dedup();

        executables
    }

    /// Checks if a file has executable permissions.
    fn is_executable(path: &Path) -> bool {
        fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    /// Completes a partial executable name.
    ///
    /// # Arguments
    ///
    /// * `partial` - The partial executable name to complete
    ///
    /// # Returns
    ///
    /// A vector of suggestions for matching executables, limited to MAX_PATH_SUGGESTIONS.
    pub fn complete_executable(&self, partial: &str) -> Vec<Suggestion> {
        debug!(partial = %partial, "Completing executable");

        let suggestions: Vec<Suggestion> = self
            .executables
            .iter()
            .filter(|exe| exe.starts_with(partial))
            .take(MAX_PATH_SUGGESTIONS)
            .map(|exe| Suggestion {
                value: exe.clone(),
                description: Some("command".to_string()),
                style: None,
                extra: None,
                span: Span::new(0, partial.len()),
                append_whitespace: true,
                match_indices: None,
            })
            .collect();

        debug!(
            count = suggestions.len(),
            "Generated executable completions"
        );
        suggestions
    }
}

impl Default for PathCompleter {
    fn default() -> Self {
        Self::new()
    }
}

/// Git branch completer with timeout protection.
///
/// Provides completion for git branch names by executing `git branch`.
/// Uses a timeout to prevent blocking on slow or large repositories.
pub struct GitCompleter;

impl GitCompleter {
    /// Creates a new GitCompleter.
    pub fn new() -> Self {
        Self
    }

    /// Completes a partial git branch name.
    ///
    /// # Arguments
    ///
    /// * `partial` - The partial branch name to complete
    ///
    /// # Returns
    ///
    /// A vector of suggestions for matching branches. Returns empty Vec if:
    /// - Not in a git repository
    /// - Git command fails or times out
    /// - No branches match the partial
    pub fn complete_git_branch(&self, partial: &str) -> Vec<Suggestion> {
        debug!(partial = %partial, "Completing git branch");

        // Check if we're in a git repository
        if !Path::new(".git").exists() {
            // Try to find .git in parent directories
            let output = Command::new("git")
                .args(["rev-parse", "--git-dir"])
                .output();

            match output {
                Ok(cmd_output) => {
                    if !cmd_output.status.success() {
                        let stderr = String::from_utf8_lossy(&cmd_output.stderr);
                        debug!(stderr = %stderr, "Not in a git repository (git rev-parse failed)");
                        return Vec::new();
                    }
                }
                Err(e) => {
                    debug!(error = %e, "Failed to execute git rev-parse");
                    return Vec::new();
                }
            }
        }

        // Get list of branches with timeout
        let output = match Self::run_git_with_timeout(&["branch", "--format=%(refname:short)"]) {
            Some(output) => output,
            None => {
                debug!("Git command failed or timed out");
                return Vec::new();
            }
        };

        let suggestions: Vec<Suggestion> = output
            .lines()
            .filter(|branch| branch.starts_with(partial))
            .map(|branch| Suggestion {
                value: branch.to_string(),
                description: Some("git branch".to_string()),
                style: None,
                extra: None,
                span: Span::new(0, partial.len()),
                append_whitespace: true,
                match_indices: None,
            })
            .collect();

        debug!(
            count = suggestions.len(),
            "Generated git branch completions"
        );
        suggestions
    }

    /// Runs a git command with a timeout.
    ///
    /// Returns None if the command fails or times out.
    fn run_git_with_timeout(args: &[&str]) -> Option<String> {
        use std::process::Stdio;
        use std::thread;

        let mut child = Command::new("git")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        // Wait with timeout using a simple polling approach
        let timeout = Duration::from_millis(GIT_TIMEOUT_MS);
        let start = std::time::Instant::now();

        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => {
                    let output = child.wait_with_output().ok()?;
                    return String::from_utf8(output.stdout).ok();
                }
                Ok(Some(_)) => {
                    // Command failed
                    return None;
                }
                Ok(None) => {
                    // Still running
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        // Reap the child process to avoid zombies
                        let _ = child.wait();
                        debug!("Git command timed out after {}ms", GIT_TIMEOUT_MS);
                        return None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return None,
            }
        }
    }
}

impl Default for GitCompleter {
    fn default() -> Self {
        Self::new()
    }
}

/// Context-aware completer that orchestrates filesystem, PATH, and git completers.
///
/// This is the main completer used by the editor. It implements reedline's `Completer`
/// trait and delegates to the appropriate sub-completer based on the input context.
pub struct WrashCompleter {
    /// Filesystem path completer.
    filesystem: FilesystemCompleter,
    /// PATH executable completer.
    path: PathCompleter,
    /// Git branch completer.
    git: GitCompleter,
}

impl WrashCompleter {
    /// Creates a new WrashCompleter with all sub-completers initialized.
    pub fn new() -> Self {
        info!("Creating WrashCompleter with filesystem, PATH, and git completion");
        Self {
            filesystem: FilesystemCompleter::new(),
            path: PathCompleter::new(),
            git: GitCompleter::new(),
        }
    }

    /// Extracts the word at the cursor position.
    ///
    /// Returns (word_start_position, word) where word is the text from the last
    /// whitespace before pos to pos.
    fn word_at_cursor<'a>(&self, line: &'a str, pos: usize) -> (usize, &'a str) {
        let safe_pos = pos.min(line.len());
        // Ensure we're at a valid UTF-8 boundary to prevent panics
        let before_cursor = if line.is_char_boundary(safe_pos) {
            &line[..safe_pos]
        } else {
            // Fall back to the previous valid boundary
            let valid_pos = line[..safe_pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            &line[..valid_pos]
        };

        // Find the start of the current word (last whitespace + 1)
        let word_start = before_cursor
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);

        (word_start, &before_cursor[word_start..])
    }

    /// Determines if the context suggests git branch completion.
    fn is_git_branch_context(&self, line: &str) -> bool {
        let trimmed = line.trim_start();
        trimmed.starts_with("git checkout ")
            || trimmed.starts_with("git branch ")
            || trimmed.starts_with("git switch ")
            || trimmed.starts_with("git merge ")
            || trimmed.starts_with("git rebase ")
    }

    /// Determines if the partial looks like a filesystem path.
    fn is_path_like(&self, partial: &str) -> bool {
        partial.contains('/') || partial.contains('.') || partial.starts_with('~')
    }
}

impl Default for WrashCompleter {
    fn default() -> Self {
        Self::new()
    }
}

impl Completer for WrashCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let (word_start, partial) = self.word_at_cursor(line, pos);

        debug!(
            line = %line,
            pos = pos,
            word_start = word_start,
            partial = %partial,
            "Completing"
        );

        // Determine completion type based on context
        let mut suggestions = if self.is_git_branch_context(line) && word_start > 0 {
            // Git branch completion
            self.git.complete_git_branch(partial)
        } else if self.is_path_like(partial) {
            // Filesystem completion for paths
            self.filesystem.complete_path(partial)
        } else if word_start == 0 {
            // First word - command completion from PATH
            self.path.complete_executable(partial)
        } else {
            // Default to filesystem completion
            self.filesystem.complete_path(partial)
        };

        // Adjust spans to be relative to word_start
        for suggestion in &mut suggestions {
            suggestion.span = Span::new(word_start, pos);
        }

        suggestions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // FilesystemCompleter Tests
    // =========================================================================

    #[test]
    fn test_filesystem_completer_new() {
        let completer = FilesystemCompleter::new();
        // Should create without panicking
        drop(completer);
    }

    #[test]
    fn test_filesystem_complete_tmp() {
        let completer = FilesystemCompleter::new();
        let suggestions = completer.complete_path("/tmp");
        // /tmp should exist and have some contents (or be empty)
        // We just verify it doesn't panic and returns a Vec
        assert!(suggestions.len() <= MAX_FILESYSTEM_SUGGESTIONS);
    }

    #[test]
    fn test_filesystem_complete_nonexistent() {
        let completer = FilesystemCompleter::new();
        let suggestions = completer.complete_path("/nonexistent_path_12345");
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_filesystem_complete_tilde() {
        let completer = FilesystemCompleter::new();
        let suggestions = completer.complete_path("~");
        // Should expand ~ to home directory and list contents
        // Just verify it doesn't panic
        assert!(suggestions.len() <= MAX_FILESYSTEM_SUGGESTIONS);
    }

    #[test]
    fn test_filesystem_complete_relative() {
        let completer = FilesystemCompleter::new();
        let suggestions = completer.complete_path("./");
        // Should list current directory contents
        assert!(suggestions.len() <= MAX_FILESYSTEM_SUGGESTIONS);
    }

    #[test]
    fn test_filesystem_directory_has_trailing_slash() {
        let completer = FilesystemCompleter::new();
        let suggestions = completer.complete_path("/");
        // Directories should have trailing slash
        for suggestion in &suggestions {
            if suggestion.description.as_deref() == Some("directory") {
                assert!(
                    suggestion.value.ends_with('/'),
                    "Directory {} should end with /",
                    suggestion.value
                );
            }
        }
    }

    // =========================================================================
    // PathCompleter Tests
    // =========================================================================

    #[test]
    #[serial_test::serial]
    fn test_path_completer_new() {
        use std::fs::File;
        use std::io::Write;
        use tempfile::TempDir;

        // Save original PATH
        let original_path = env::var_os("PATH");

        // Check if PATH is missing or empty
        let path_is_usable = original_path
            .as_ref()
            .map(|p| !p.is_empty())
            .unwrap_or(false);

        if path_is_usable {
            // PATH is available, use it directly
            let completer = PathCompleter::new();
            // Should find at least some executables
            assert!(!completer.executables.is_empty());
        } else {
            // PATH is missing/empty - create a temp directory with a dummy executable
            let temp_dir = TempDir::new().expect("Failed to create temp directory");
            let dummy_exe_path = temp_dir.path().join("dummy_test_exe");

            // Create a dummy executable file
            {
                let mut file =
                    File::create(&dummy_exe_path).expect("Failed to create dummy executable");
                file.write_all(b"#!/bin/sh\n")
                    .expect("Failed to write to dummy executable");
            }

            // Make it executable (Unix)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(&dummy_exe_path)
                    .expect("Failed to get metadata")
                    .permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&dummy_exe_path, perms).expect("Failed to set permissions");
            }

            // Set PATH to the temp directory
            // SAFETY: This test is single-threaded and we restore the original PATH afterwards
            unsafe {
                env::set_var("PATH", temp_dir.path());
            }

            let completer = PathCompleter::new();

            // Restore original PATH
            // SAFETY: This test is single-threaded and we're restoring the original value
            unsafe {
                match original_path {
                    Some(p) => env::set_var("PATH", p),
                    None => env::remove_var("PATH"),
                }
            }

            // Assert that we found the dummy executable
            assert!(
                !completer.executables.is_empty(),
                "PathCompleter should find executables in the temp PATH"
            );
            assert!(
                completer
                    .executables
                    .contains(&"dummy_test_exe".to_string()),
                "PathCompleter should find the dummy_test_exe"
            );
        }
    }

    #[test]
    fn test_path_complete_ls() {
        let completer = PathCompleter::new();
        let suggestions = completer.complete_executable("ls");
        // ls should be found on most systems
        let has_ls = suggestions.iter().any(|s| s.value == "ls");
        assert!(has_ls, "Expected to find 'ls' command");
    }

    #[test]
    fn test_path_complete_empty() {
        let completer = PathCompleter::new();
        let suggestions = completer.complete_executable("");
        // Should return up to MAX_PATH_SUGGESTIONS
        assert!(suggestions.len() <= MAX_PATH_SUGGESTIONS);
    }

    #[test]
    fn test_path_complete_nonexistent() {
        let completer = PathCompleter::new();
        let suggestions = completer.complete_executable("zzznonexistentcommand");
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_path_executables_are_sorted() {
        let completer = PathCompleter::new();
        let mut sorted = completer.executables.clone();
        sorted.sort();
        assert_eq!(completer.executables, sorted);
    }

    #[test]
    fn test_path_executables_no_duplicates() {
        let completer = PathCompleter::new();
        let mut deduped = completer.executables.clone();
        deduped.dedup();
        assert_eq!(completer.executables.len(), deduped.len());
    }

    // =========================================================================
    // GitCompleter Tests
    // =========================================================================

    #[test]
    fn test_git_completer_new() {
        let completer = GitCompleter::new();
        // Should create without panicking
        drop(completer);
    }

    #[test]
    fn test_git_complete_in_git_repo() {
        let completer = GitCompleter::new();
        // This test runs in the wrashpty repo, so .git should exist
        let suggestions = completer.complete_git_branch("");
        // Should find at least one branch (main or master)
        // If not in a git repo, this will be empty which is fine
        if Path::new(".git").exists() {
            assert!(
                !suggestions.is_empty(),
                "Expected branches in git repository"
            );
        }
    }

    #[test]
    fn test_git_complete_partial_branch() {
        let completer = GitCompleter::new();
        let suggestions = completer.complete_git_branch("mai");
        // If 'main' branch exists, should be in suggestions
        // This is repo-dependent, so we just verify it doesn't panic
        for suggestion in &suggestions {
            assert!(suggestion.value.starts_with("mai"));
        }
    }

    // =========================================================================
    // WrashCompleter Tests
    // =========================================================================

    #[test]
    fn test_wrash_completer_new() {
        let completer = WrashCompleter::new();
        // Should create without panicking
        drop(completer);
    }

    #[test]
    fn test_wrash_word_at_cursor_first_word() {
        let completer = WrashCompleter::new();
        let (start, word) = completer.word_at_cursor("echo", 4);
        assert_eq!(start, 0);
        assert_eq!(word, "echo");
    }

    #[test]
    fn test_wrash_word_at_cursor_second_word() {
        let completer = WrashCompleter::new();
        let (start, word) = completer.word_at_cursor("echo hello", 10);
        assert_eq!(start, 5);
        assert_eq!(word, "hello");
    }

    #[test]
    fn test_wrash_word_at_cursor_partial() {
        let completer = WrashCompleter::new();
        let (start, word) = completer.word_at_cursor("echo hel", 8);
        assert_eq!(start, 5);
        assert_eq!(word, "hel");
    }

    #[test]
    fn test_wrash_word_at_cursor_empty() {
        let completer = WrashCompleter::new();
        let (start, word) = completer.word_at_cursor("", 0);
        assert_eq!(start, 0);
        assert_eq!(word, "");
    }

    #[test]
    fn test_wrash_is_git_context() {
        let completer = WrashCompleter::new();
        assert!(completer.is_git_branch_context("git checkout "));
        assert!(completer.is_git_branch_context("git branch "));
        assert!(completer.is_git_branch_context("git switch "));
        assert!(completer.is_git_branch_context("git merge "));
        assert!(completer.is_git_branch_context("  git checkout "));
        assert!(!completer.is_git_branch_context("echo git checkout"));
        assert!(!completer.is_git_branch_context("git"));
    }

    #[test]
    fn test_wrash_is_path_like() {
        let completer = WrashCompleter::new();
        assert!(completer.is_path_like("/usr"));
        assert!(completer.is_path_like("./foo"));
        assert!(completer.is_path_like("~/Documents"));
        assert!(completer.is_path_like("file.txt"));
        assert!(!completer.is_path_like("echo"));
        assert!(!completer.is_path_like("git"));
    }

    #[test]
    fn test_wrash_complete_first_word() {
        let mut completer = WrashCompleter::new();
        let suggestions = completer.complete("ec", 2);
        // Should use PATH completer for first word
        // Verify we get command suggestions
        for suggestion in &suggestions {
            assert!(
                suggestion.description.as_deref() == Some("command"),
                "First word should be completed as command"
            );
        }
    }

    #[test]
    fn test_wrash_complete_path_argument() {
        let mut completer = WrashCompleter::new();
        let suggestions = completer.complete("ls /", 4);
        // Should use filesystem completer for path argument
        // Spans should be adjusted to word position
        for suggestion in &suggestions {
            assert_eq!(
                suggestion.span.start, 3,
                "Span start should be at word start"
            );
        }
    }

    #[test]
    fn test_wrash_complete_git_branch() {
        let mut completer = WrashCompleter::new();
        let suggestions = completer.complete("git checkout ", 13);
        // If in git repo, should get branch suggestions
        // Otherwise empty is fine
        for suggestion in &suggestions {
            assert!(
                suggestion.description.as_deref() == Some("git branch"),
                "Git context should get branch completions"
            );
        }
    }

    // =========================================================================
    // is_executable Tests
    // =========================================================================

    #[test]
    fn test_is_executable() {
        // /bin/sh should be executable on Unix systems
        assert!(PathCompleter::is_executable(Path::new("/bin/sh")));
    }

    #[test]
    fn test_is_not_executable() {
        // /etc/passwd is typically not executable
        if Path::new("/etc/passwd").exists() {
            assert!(!PathCompleter::is_executable(Path::new("/etc/passwd")));
        }
    }
}
