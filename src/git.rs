//! Lightweight git integration for chrome context bar.

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
            if info.branch.is_some() {
                assert!(!info.branch.as_ref().unwrap().is_empty());
            }
        }
    }
}
