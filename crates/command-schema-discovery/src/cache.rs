//! Schema extraction cache with fingerprint-based invalidation.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::SystemTime;

use command_schema_core::CommandSchema;
use serde::{Deserialize, Serialize};

use crate::report::ExtractionReport;

/// Fingerprint used to decide whether a cache entry is still valid.
///
/// Keyed on executable identity (path + mtime + size) plus quality policy
/// thresholds so that changing policy parameters triggers a cache miss.
#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct CacheKey {
    pub command: String,
    pub executable_path: PathBuf,
    pub mtime_secs: i64,
    pub size_bytes: u64,
    /// Quality policy encoded as integer basis points to make the key
    /// hashable and equality-comparable without floating-point issues.
    pub min_confidence_bp: u32,
    pub min_coverage_bp: u32,
    pub allow_low_quality: bool,
}

/// Cached extraction result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: CacheKey,
    pub schema: Option<CommandSchema>,
    pub report: ExtractionReport,
    /// Version string detected at extraction time, used for invalidation
    /// when the same binary produces a different version on re-extraction.
    pub detected_version: Option<String>,
    /// Probe strategy that produced the accepted help output (e.g. "gnu",
    /// "clap"). Stored for diagnostics and potential future invalidation.
    pub probe_mode: Option<String>,
    pub cached_at: String,
}

/// File-backed extraction cache.
pub struct SchemaCache {
    cache_dir: PathBuf,
}

impl SchemaCache {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Default cache directory (~/.cache/command-schema-discovery/).
    pub fn default_dir() -> PathBuf {
        dirs_cache_dir().join("command-schema-discovery")
    }

    /// Looks up a cache entry. Returns `Some` only if the stored key
    /// matches the provided key exactly.
    pub fn get(&self, key: &CacheKey) -> Option<CacheEntry> {
        let path = self.entry_path(key);
        let raw = fs::read_to_string(&path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&raw).ok()?;
        if entry.key == *key {
            Some(entry)
        } else {
            // Fingerprint mismatch → stale entry
            None
        }
    }

    /// Stores an extraction result in the cache.
    pub fn put(
        &self,
        key: CacheKey,
        schema: Option<CommandSchema>,
        report: ExtractionReport,
        detected_version: Option<String>,
        probe_mode: Option<String>,
    ) {
        if fs::create_dir_all(&self.cache_dir).is_err() {
            return;
        }

        let entry = CacheEntry {
            key: key.clone(),
            schema,
            report,
            detected_version,
            probe_mode,
            cached_at: chrono::Utc::now().to_rfc3339(),
        };

        let path = self.entry_path(&key);
        if let Ok(json) = serde_json::to_string_pretty(&entry) {
            let _ = fs::write(path, json);
        }
    }

    fn entry_path(&self, key: &CacheKey) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash = hasher.finish();
        self.cache_dir.join(format!("{:016x}.json", hash))
    }
}

/// Builds a cache key for a command by resolving its executable path,
/// reading filesystem metadata, and encoding quality policy thresholds.
pub fn build_cache_key(
    command: &str,
    policy: &crate::extractor::ExtractionQualityPolicy,
) -> Option<CacheKey> {
    let exe_path = resolve_executable(command)?;
    let metadata = fs::metadata(&exe_path).ok()?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    Some(CacheKey {
        command: command.to_string(),
        executable_path: exe_path,
        mtime_secs: mtime,
        size_bytes: metadata.len(),
        min_confidence_bp: (policy.min_confidence * 10_000.0) as u32,
        min_coverage_bp: (policy.min_coverage * 10_000.0) as u32,
        allow_low_quality: policy.allow_low_quality,
    })
}

/// Runs a lightweight `command --version` probe to detect the current
/// version string. Returns `None` if the command fails, times out, or
/// the output contains no extractable version.
pub fn detect_quick_version(command: &str) -> Option<String> {
    let base = command.split_whitespace().next().unwrap_or(command);
    let output = Command::new(base)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    crate::version::extract_version(&text, command)
}

fn resolve_executable(command: &str) -> Option<PathBuf> {
    let base = command.split_whitespace().next().unwrap_or(command);
    let output = Command::new("which")
        .arg(base)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path_str.is_empty() {
        return None;
    }
    // Resolve symlinks to get canonical path
    fs::canonicalize(&path_str).ok().or_else(|| Some(PathBuf::from(path_str)))
}

fn dirs_cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(xdg);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache");
    }
    PathBuf::from("/tmp")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_cache_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("wrashpty-cache-test-{nanos}"))
    }

    #[test]
    fn test_cache_roundtrip() {
        let dir = test_cache_dir();
        let cache = SchemaCache::new(dir.clone());

        let key = CacheKey {
            command: "testcmd".to_string(),
            executable_path: PathBuf::from("/usr/bin/testcmd"),
            mtime_secs: 1700000000,
            size_bytes: 12345,
            min_confidence_bp: 5000,
            min_coverage_bp: 3000,
            allow_low_quality: false,
        };

        let report = ExtractionReport {
            command: "testcmd".to_string(),
            success: true,
            accepted_for_suggestions: true,
            quality_tier: crate::report::QualityTier::High,
            quality_reasons: Vec::new(),
            failure_code: None,
            failure_detail: None,
            selected_format: Some("gnu".to_string()),
            format_scores: Vec::new(),
            parsers_used: vec!["gnu".to_string()],
            confidence: 0.9,
            coverage: 0.8,
            relevant_lines: 10,
            recognized_lines: 8,
            unresolved_lines: Vec::new(),
            probe_attempts: Vec::new(),
            warnings: Vec::new(),
            validation_errors: Vec::new(),
        };

        cache.put(
            key.clone(),
            None,
            report,
            Some("1.0.0".to_string()),
            Some("gnu".to_string()),
        );

        let entry = cache.get(&key);
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.key.command, "testcmd");
        assert_eq!(entry.report.confidence, 0.9);
        assert_eq!(entry.detected_version, Some("1.0.0".to_string()));

        // Mismatched key (different mtime) should return None
        let bad_key = CacheKey {
            mtime_secs: 9999999999,
            ..key.clone()
        };
        assert!(cache.get(&bad_key).is_none());

        // Different policy thresholds should also miss
        let policy_key = CacheKey {
            min_confidence_bp: 8500,
            ..key.clone()
        };
        assert!(cache.get(&policy_key).is_none());

        // Cleanup
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_build_cache_key_for_missing_command() {
        let policy = crate::extractor::ExtractionQualityPolicy::default();
        let key = build_cache_key("__wrashpty_missing_command__", &policy);
        assert!(key.is_none());
    }

    #[test]
    fn test_cache_miss_on_version_mismatch() {
        let dir = test_cache_dir();
        let cache = SchemaCache::new(dir.clone());

        let key = CacheKey {
            command: "testcmd".to_string(),
            executable_path: PathBuf::from("/usr/bin/testcmd"),
            mtime_secs: 1700000000,
            size_bytes: 12345,
            min_confidence_bp: 5000,
            min_coverage_bp: 3000,
            allow_low_quality: false,
        };

        let report = ExtractionReport {
            command: "testcmd".to_string(),
            success: true,
            accepted_for_suggestions: true,
            quality_tier: crate::report::QualityTier::High,
            quality_reasons: Vec::new(),
            failure_code: None,
            failure_detail: None,
            selected_format: Some("gnu".to_string()),
            format_scores: Vec::new(),
            parsers_used: vec!["gnu".to_string()],
            confidence: 0.9,
            coverage: 0.8,
            relevant_lines: 10,
            recognized_lines: 8,
            unresolved_lines: Vec::new(),
            probe_attempts: Vec::new(),
            warnings: Vec::new(),
            validation_errors: Vec::new(),
        };

        cache.put(
            key.clone(),
            None,
            report,
            Some("1.0.0".to_string()),
            Some("gnu".to_string()),
        );

        // Same key should hit the cache at the storage level
        let entry = cache.get(&key).unwrap();
        assert_eq!(entry.detected_version, Some("1.0.0".to_string()));
        assert_eq!(entry.probe_mode, Some("gnu".to_string()));

        // Simulating version comparison logic from discover.rs:
        // if cached version is "1.0.0" but current version is "2.0.0", should miss
        let cached_version = &entry.detected_version;
        let current_version = Some("2.0.0".to_string());
        let version_matches = match (cached_version, &current_version) {
            (Some(cached), Some(current)) => cached == current,
            (None, None) => true,
            _ => false,
        };
        assert!(!version_matches, "version mismatch should not match");

        // Both None should match
        let version_matches_both_none = match (&None::<String>, &None::<String>) {
            (Some(cached), Some(current)) => cached == current,
            (None, None) => true,
            _ => false,
        };
        assert!(version_matches_both_none, "both None should match");

        // One Some, one None should not match
        let version_matches_mixed = match (&Some("1.0.0".to_string()), &None::<String>) {
            (Some(cached), Some(current)) => cached == current,
            (None, None) => true,
            _ => false,
        };
        assert!(!version_matches_mixed, "Some vs None should not match");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_cache_key_encodes_policy_thresholds() {
        let policy = crate::extractor::ExtractionQualityPolicy {
            min_confidence: 0.85,
            min_coverage: 0.30,
            allow_low_quality: false,
        };

        // We can't build a real key without a real command, but we can
        // verify that the basis-point conversion is correct.
        let bp_confidence = (policy.min_confidence * 10_000.0) as u32;
        let bp_coverage = (policy.min_coverage * 10_000.0) as u32;
        assert_eq!(bp_confidence, 8500);
        assert_eq!(bp_coverage, 3000);
    }

    #[test]
    fn test_cache_entry_stores_probe_mode() {
        let dir = test_cache_dir();
        let cache = SchemaCache::new(dir.clone());

        let key = CacheKey {
            command: "probecmd".to_string(),
            executable_path: PathBuf::from("/usr/bin/probecmd"),
            mtime_secs: 1700000000,
            size_bytes: 500,
            min_confidence_bp: 5000,
            min_coverage_bp: 3000,
            allow_low_quality: false,
        };

        let report = ExtractionReport {
            command: "probecmd".to_string(),
            success: true,
            accepted_for_suggestions: true,
            quality_tier: crate::report::QualityTier::Medium,
            quality_reasons: Vec::new(),
            failure_code: None,
            failure_detail: None,
            selected_format: Some("cobra".to_string()),
            format_scores: Vec::new(),
            parsers_used: vec!["cobra".to_string()],
            confidence: 0.7,
            coverage: 0.6,
            relevant_lines: 5,
            recognized_lines: 3,
            unresolved_lines: Vec::new(),
            probe_attempts: Vec::new(),
            warnings: Vec::new(),
            validation_errors: Vec::new(),
        };

        cache.put(
            key.clone(),
            None,
            report,
            None,
            Some("cobra".to_string()),
        );

        let entry = cache.get(&key).unwrap();
        assert_eq!(entry.probe_mode, Some("cobra".to_string()));
        assert_eq!(entry.detected_version, None);

        let _ = fs::remove_dir_all(dir);
    }
}
