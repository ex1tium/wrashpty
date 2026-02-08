//! Structured extraction reporting for offline schema discovery.

use serde::{Deserialize, Serialize};

/// Weighted score entry for a detected output format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatScoreReport {
    pub format: String,
    pub score: f64,
}

/// Probe attempt metadata for one help-flag invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeAttemptReport {
    pub help_flag: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub error: Option<String>,
    pub output_source: Option<String>,
    pub output_len: usize,
    pub accepted: bool,
}

/// Per-command extraction report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionReport {
    pub command: String,
    pub success: bool,
    pub selected_format: Option<String>,
    pub format_scores: Vec<FormatScoreReport>,
    pub parsers_used: Vec<String>,
    pub confidence: f64,
    pub coverage: f64,
    pub relevant_lines: usize,
    pub recognized_lines: usize,
    pub unresolved_lines: Vec<String>,
    pub probe_attempts: Vec<ProbeAttemptReport>,
    pub warnings: Vec<String>,
    pub validation_errors: Vec<String>,
}

/// Batch report for a full discovery run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionReportBundle {
    pub generated_at: String,
    pub version: String,
    pub reports: Vec<ExtractionReport>,
    pub failures: Vec<String>,
}
