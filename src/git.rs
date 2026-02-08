//! Lightweight git integration for chrome context bar and file browser.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Minimum interval between git status checks to avoid stalling prompts in large repos.
const GIT_CACHE_DURATION: Duration = Duration::from_secs(2);

/// Git repository information.
#[derive(Debug, Clone, Default)]
pub struct GitInfo {
    /// Current branch name, if in a git repository.
    pub branch: Option<String>,
    /// Whether the working directory has uncommitted changes.
    pub dirty: bool,
}

/// Cached git information with timestamp.
#[derive(Debug, Clone)]
pub struct CachedGitInfo {
    /// The cached git information.
    pub info: GitInfo,
    /// The directory this info was retrieved for.
    pub cwd: PathBuf,
    /// When this info was last updated.
    pub last_check: Instant,
}

impl CachedGitInfo {
    /// Creates a new cached git info entry.
    pub fn new(info: GitInfo, cwd: PathBuf) -> Self {
        Self {
            info,
            cwd,
            last_check: Instant::now(),
        }
    }

    /// Returns true if the cache is still valid for the given directory.
    ///
    /// Cache is valid if:
    /// - The directory matches the cached directory
    /// - Less than GIT_CACHE_DURATION has elapsed since last check
    pub fn is_valid_for(&self, cwd: &Path) -> bool {
        self.cwd == cwd && self.last_check.elapsed() < GIT_CACHE_DURATION
    }
}

/// Retrieves git information, using the cache if valid.
///
/// This function checks the cache first. If the cache is valid for the given
/// directory, it returns the cached info immediately. Otherwise, it performs
/// the git queries and updates the cache.
///
/// # Arguments
///
/// * `cwd` - Current working directory to check
/// * `cache` - Mutable reference to the cache (if any)
///
/// # Returns
///
/// GitInfo with branch and dirty status
pub fn get_git_info_cached(cwd: &Path, cache: &mut Option<CachedGitInfo>) -> GitInfo {
    // Check if cache is valid
    if let Some(cached) = cache.as_ref() {
        if cached.is_valid_for(cwd) {
            return cached.info.clone();
        }
    }

    // Cache miss or invalid - fetch fresh info
    let info = get_git_info(cwd);

    // Update cache
    *cache = Some(CachedGitInfo::new(info.clone(), cwd.to_path_buf()));

    info
}

/// Retrieves git information for the given directory.
///
/// This function performs quick git queries to determine branch name
/// and dirty status. It's designed to be fast for small repos but may
/// be slow for large repositories. Consider caching or async execution
/// for production use.
///
/// # Arguments
///
/// * `cwd` - Current working directory to check
///
/// # Returns
///
/// GitInfo with branch and dirty status, or default (None/false) if not a git repo
pub fn get_git_info(cwd: &Path) -> GitInfo {
    // Quick check: is this a git repo?
    // Walk up the directory tree to find .git
    let mut check_dir = Some(cwd);
    let mut is_git_repo = false;
    while let Some(dir) = check_dir {
        if dir.join(".git").exists() {
            is_git_repo = true;
            break;
        }
        check_dir = dir.parent();
    }

    if !is_git_repo {
        return GitInfo::default();
    }

    // Get branch name (fast)
    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        });

    // Check dirty status (fast for small repos)
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    GitInfo { branch, dirty }
}

// ─── Per-file git status ─────────────────────────────────────────────────────

/// Status of a single file in the git working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitFileStatus {
    /// File has been modified in the working tree.
    Modified,
    /// File has been added/staged.
    Added,
    /// File has been deleted.
    Deleted,
    /// File has been renamed.
    Renamed,
    /// File is untracked.
    Untracked,
    /// File has merge conflicts.
    Conflict,
}

/// Per-file git status for a repository.
#[derive(Debug, Clone)]
pub struct GitRepoStatus {
    /// Map from path (relative to repo root) to status.
    pub files: HashMap<PathBuf, GitFileStatus>,
    /// Repository root directory (absolute).
    pub repo_root: PathBuf,
}

impl GitRepoStatus {
    /// Looks up the status for a file given its absolute path.
    pub fn status_for(&self, abs_path: &Path) -> Option<GitFileStatus> {
        let rel_path = abs_path.strip_prefix(&self.repo_root).ok()?;
        self.files.get(rel_path).copied()
    }

    /// Returns a count of files for each status type.
    pub fn summary(&self) -> HashMap<GitFileStatus, usize> {
        let mut counts = HashMap::new();
        for status in self.files.values() {
            *counts.entry(*status).or_insert(0) += 1;
        }
        counts
    }
}

/// Cached per-file git status with TTL.
pub struct CachedGitRepoStatus {
    /// The cached status.
    pub status: GitRepoStatus,
    /// The directory this was retrieved for.
    pub cwd: PathBuf,
    /// When this was last updated.
    pub last_check: Instant,
}

impl CachedGitRepoStatus {
    /// Creates a new cached status.
    pub fn new(status: GitRepoStatus, cwd: PathBuf) -> Self {
        Self {
            status,
            cwd,
            last_check: Instant::now(),
        }
    }

    /// Returns true if the cache is still valid for the given directory.
    pub fn is_valid_for(&self, cwd: &Path) -> bool {
        self.cwd == cwd && self.last_check.elapsed() < GIT_CACHE_DURATION
    }
}

/// Retrieves per-file git status, using the cache if valid.
pub fn get_git_repo_status_cached(
    cwd: &Path,
    cache: &mut Option<CachedGitRepoStatus>,
) -> Option<GitRepoStatus> {
    if let Some(cached) = cache.as_ref() {
        if cached.is_valid_for(cwd) {
            return Some(cached.status.clone());
        }
    }

    let status = get_git_repo_status(cwd)?;
    *cache = Some(CachedGitRepoStatus::new(status.clone(), cwd.to_path_buf()));
    Some(status)
}

/// Finds the git repository root for a directory.
///
/// Walks up the directory tree looking for `.git`. Returns the directory
/// containing `.git`, or `None` if not in a git repo.
pub fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    let mut check_dir = Some(cwd);
    while let Some(dir) = check_dir {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        check_dir = dir.parent();
    }
    None
}

/// Retrieves per-file git status by running `git status --porcelain=v2`.
///
/// Returns `None` if not in a git repo or if the command fails.
pub fn get_git_repo_status(cwd: &Path) -> Option<GitRepoStatus> {
    let repo_root = find_git_root(cwd)?;

    let output = Command::new("git")
        .args(["status", "--porcelain=v2", "--untracked-files=normal"])
        .current_dir(&repo_root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files = parse_porcelain_v2(&stdout);

    Some(GitRepoStatus { files, repo_root })
}

/// Parses `git status --porcelain=v2` output into a file status map.
fn parse_porcelain_v2(output: &str) -> HashMap<PathBuf, GitFileStatus> {
    let mut files = HashMap::new();

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        match line.as_bytes().first() {
            Some(b'1') => {
                // Ordinary changed entry: "1 XY sub mH mI mW hH hI path"
                // We care about X (index) and Y (worktree) at position 2 and 3
                if let Some(status) = parse_ordinary_entry(line) {
                    if let Some(path) = extract_path_from_ordinary(line) {
                        files.insert(PathBuf::from(path), status);
                    }
                }
            }
            Some(b'2') => {
                // Renamed/copied entry: "2 XY sub mH mI mW hH hI X{score} path\torigPath"
                if let Some(path) = extract_path_from_renamed(line) {
                    files.insert(PathBuf::from(path), GitFileStatus::Renamed);
                }
            }
            Some(b'u') => {
                // Unmerged entry: "u XY sub m1 m2 m3 mW h1 h2 h3 path"
                if let Some(path) = extract_path_from_unmerged(line) {
                    files.insert(PathBuf::from(path), GitFileStatus::Conflict);
                }
            }
            Some(b'?') => {
                // Untracked: "? path"
                if line.len() > 2 {
                    files.insert(PathBuf::from(&line[2..]), GitFileStatus::Untracked);
                }
            }
            _ => {
                // Ignore headers and unknown lines
            }
        }
    }

    files
}

/// Parses the X/Y status from an ordinary changed entry line.
///
/// Format: `"1 XY ..."` where X=index status, Y=worktree status.
/// We prioritize worktree status (Y) since that's what users see.
fn parse_ordinary_entry(line: &str) -> Option<GitFileStatus> {
    let bytes = line.as_bytes();
    if bytes.len() < 4 {
        return None;
    }

    let x = bytes[2]; // Index status
    let y = bytes[3]; // Worktree status

    // Prioritize worktree status, fall back to index status
    match y {
        b'M' => Some(GitFileStatus::Modified),
        b'D' => Some(GitFileStatus::Deleted),
        b'A' => Some(GitFileStatus::Added),
        b'.' => {
            // Worktree clean, check index
            match x {
                b'M' => Some(GitFileStatus::Modified),
                b'A' => Some(GitFileStatus::Added),
                b'D' => Some(GitFileStatus::Deleted),
                b'R' => Some(GitFileStatus::Renamed),
                _ => None,
            }
        }
        _ => Some(GitFileStatus::Modified), // Fallback for other statuses
    }
}

/// Extracts the file path by skipping `n` space-separated fields.
fn extract_path_after_fields(line: &str, n: usize) -> Option<&str> {
    let mut fields = 0;
    for (i, c) in line.char_indices() {
        if c == ' ' {
            fields += 1;
            if fields == n {
                return Some(&line[i + 1..]);
            }
        }
    }
    None
}

/// Extracts path from ordinary entry: `"1 XY sub mH mI mW hH hI path"` (8 fields before path).
fn extract_path_from_ordinary(line: &str) -> Option<&str> {
    extract_path_after_fields(line, 8)
}

/// Extracts path from renamed entry: `"2 XY sub mH mI mW hH hI Xscore path\torigPath"` (9 fields before path).
fn extract_path_from_renamed(line: &str) -> Option<&str> {
    let after_fields = extract_path_after_fields(line, 9)?;
    // The path is before the tab, origPath is after
    Some(after_fields.split('\t').next().unwrap_or(after_fields))
}

/// Extracts path from unmerged entry: `"u XY sub m1 m2 m3 mW h1 h2 h3 path"` (10 fields before path).
fn extract_path_from_unmerged(line: &str) -> Option<&str> {
    extract_path_after_fields(line, 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_git_info_default() {
        let info = GitInfo::default();
        assert!(info.branch.is_none());
        assert!(!info.dirty);
    }

    #[test]
    fn test_get_git_info_non_repo() {
        // /tmp is unlikely to be a git repo
        let info = get_git_info(Path::new("/tmp"));
        assert!(info.branch.is_none());
        assert!(!info.dirty);
    }

    #[test]
    fn test_get_git_info_current_dir() {
        // This test assumes we're running in a git repo (the wrashpty project)
        if let Ok(cwd) = env::current_dir() {
            let info = get_git_info(&cwd);
            // If we're in a git repo, we should have a branch
            // (This may or may not be the case depending on test environment)
            if let Some(branch) = &info.branch {
                assert!(!branch.is_empty());
            }
        }
    }

    // ── Per-file status parsing tests ────────────────────────────────────────

    #[test]
    fn test_parse_porcelain_v2_modified_worktree() {
        let output = "1 .M N... 100644 100644 100644 abc123 def456 src/main.rs\n";
        let files = parse_porcelain_v2(output);
        assert_eq!(
            files.get(Path::new("src/main.rs")),
            Some(&GitFileStatus::Modified)
        );
    }

    #[test]
    fn test_parse_porcelain_v2_added_index() {
        let output = "1 A. N... 000000 100644 100644 abc123 def456 new_file.rs\n";
        let files = parse_porcelain_v2(output);
        assert_eq!(
            files.get(Path::new("new_file.rs")),
            Some(&GitFileStatus::Added)
        );
    }

    #[test]
    fn test_parse_porcelain_v2_deleted_worktree() {
        let output = "1 .D N... 100644 100644 000000 abc123 def456 removed.rs\n";
        let files = parse_porcelain_v2(output);
        assert_eq!(
            files.get(Path::new("removed.rs")),
            Some(&GitFileStatus::Deleted)
        );
    }

    #[test]
    fn test_parse_porcelain_v2_untracked() {
        let output = "? notes.txt\n";
        let files = parse_porcelain_v2(output);
        assert_eq!(
            files.get(Path::new("notes.txt")),
            Some(&GitFileStatus::Untracked)
        );
    }

    #[test]
    fn test_parse_porcelain_v2_renamed() {
        let output =
            "2 R. N... 100644 100644 100644 abc123 def456 R100 new_name.rs\told_name.rs\n";
        let files = parse_porcelain_v2(output);
        assert_eq!(
            files.get(Path::new("new_name.rs")),
            Some(&GitFileStatus::Renamed)
        );
    }

    #[test]
    fn test_parse_porcelain_v2_conflict() {
        let output = "u UU N... 100644 100644 100644 100644 abc123 def456 ghi789 conflict.rs\n";
        let files = parse_porcelain_v2(output);
        assert_eq!(
            files.get(Path::new("conflict.rs")),
            Some(&GitFileStatus::Conflict)
        );
    }

    #[test]
    fn test_parse_porcelain_v2_multiple_entries() {
        let output = "\
1 .M N... 100644 100644 100644 abc123 def456 src/lib.rs
1 A. N... 000000 100644 100644 abc123 def456 src/new.rs
? untracked.txt
";
        let files = parse_porcelain_v2(output);
        assert_eq!(files.len(), 3);
        assert_eq!(
            files.get(Path::new("src/lib.rs")),
            Some(&GitFileStatus::Modified)
        );
        assert_eq!(
            files.get(Path::new("src/new.rs")),
            Some(&GitFileStatus::Added)
        );
        assert_eq!(
            files.get(Path::new("untracked.txt")),
            Some(&GitFileStatus::Untracked)
        );
    }

    #[test]
    fn test_parse_porcelain_v2_empty() {
        let files = parse_porcelain_v2("");
        assert!(files.is_empty());
    }

    #[test]
    fn test_find_git_root_in_repo() {
        if let Ok(cwd) = env::current_dir() {
            if cwd.join(".git").exists() || find_git_root(&cwd).is_some() {
                let root = find_git_root(&cwd);
                assert!(root.is_some());
                assert!(root.unwrap().join(".git").exists());
            }
        }
    }

    #[test]
    fn test_find_git_root_outside_repo() {
        assert!(find_git_root(Path::new("/tmp")).is_none());
    }

    #[test]
    fn test_git_repo_status_for_absolute_path() {
        let mut files = HashMap::new();
        files.insert(PathBuf::from("src/main.rs"), GitFileStatus::Modified);
        let status = GitRepoStatus {
            files,
            repo_root: PathBuf::from("/home/user/project"),
        };
        assert_eq!(
            status.status_for(Path::new("/home/user/project/src/main.rs")),
            Some(GitFileStatus::Modified)
        );
        assert_eq!(
            status.status_for(Path::new("/home/user/project/src/other.rs")),
            None
        );
    }

    #[test]
    fn test_git_repo_status_summary() {
        let mut files = HashMap::new();
        files.insert(PathBuf::from("a.rs"), GitFileStatus::Modified);
        files.insert(PathBuf::from("b.rs"), GitFileStatus::Modified);
        files.insert(PathBuf::from("c.rs"), GitFileStatus::Added);
        files.insert(PathBuf::from("d.txt"), GitFileStatus::Untracked);
        let status = GitRepoStatus {
            files,
            repo_root: PathBuf::from("/repo"),
        };
        let summary = status.summary();
        assert_eq!(summary.get(&GitFileStatus::Modified), Some(&2));
        assert_eq!(summary.get(&GitFileStatus::Added), Some(&1));
        assert_eq!(summary.get(&GitFileStatus::Untracked), Some(&1));
    }
}
