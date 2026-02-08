//! Structured extraction reporting for offline schema discovery.

use serde::{Deserialize, Serialize};

/// Structured failure code for extraction failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    /// Command is not installed on the system.
    NotInstalled,
    /// Probe was blocked by environment permissions.
    PermissionBlocked,
    /// Probe timed out before producing output.
    Timeout,
    /// Probe produced output that is not recognizable help text.
    NotHelpOutput,
    /// Help text was found but parsing failed to produce a schema.
    ParseFailed,
    /// Schema was extracted but rejected by quality policy.
    QualityRejected,
}

impl std::fmt::Display for FailureCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "not_installed"),
            Self::PermissionBlocked => write!(f, "permission_blocked"),
            Self::Timeout => write!(f, "timeout"),
            Self::NotHelpOutput => write!(f, "not_help_output"),
            Self::ParseFailed => write!(f, "parse_failed"),
            Self::QualityRejected => write!(f, "quality_rejected"),
        }
    }
}

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
    pub rejection_reason: Option<String>,
    pub output_source: Option<String>,
    pub output_len: usize,
    pub output_preview: Option<String>,
    pub accepted: bool,
}

/// Quality tier assigned to one extraction report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityTier {
    High,
    Medium,
    Low,
    Failed,
}

/// Per-command extraction report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionReport {
    pub command: String,
    pub success: bool,
    pub accepted_for_suggestions: bool,
    pub quality_tier: QualityTier,
    pub quality_reasons: Vec<String>,
    /// Structured failure code when extraction did not succeed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<FailureCode>,
    /// Human-readable detail about the failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_detail: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failure_code_display_matches_serde() {
        let codes = [
            (FailureCode::NotInstalled, "not_installed"),
            (FailureCode::PermissionBlocked, "permission_blocked"),
            (FailureCode::Timeout, "timeout"),
            (FailureCode::NotHelpOutput, "not_help_output"),
            (FailureCode::ParseFailed, "parse_failed"),
            (FailureCode::QualityRejected, "quality_rejected"),
        ];

        for (code, expected) in codes {
            assert_eq!(code.to_string(), expected);
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{expected}\""));
        }
    }

    #[test]
    fn test_failure_code_roundtrip_serde() {
        for code in [
            FailureCode::NotInstalled,
            FailureCode::PermissionBlocked,
            FailureCode::Timeout,
            FailureCode::NotHelpOutput,
            FailureCode::ParseFailed,
            FailureCode::QualityRejected,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            let back: FailureCode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, code);
        }
    }

    #[test]
    fn test_quality_tier_serde_snake_case() {
        let json = serde_json::to_string(&QualityTier::High).unwrap();
        assert_eq!(json, "\"high\"");
        let json = serde_json::to_string(&QualityTier::Failed).unwrap();
        assert_eq!(json, "\"failed\"");
    }

    #[test]
    fn test_extraction_report_omits_none_failure_fields() {
        let report = ExtractionReport {
            command: "test".to_string(),
            success: true,
            accepted_for_suggestions: true,
            quality_tier: QualityTier::High,
            quality_reasons: Vec::new(),
            failure_code: None,
            failure_detail: None,
            selected_format: None,
            format_scores: Vec::new(),
            parsers_used: Vec::new(),
            confidence: 0.9,
            coverage: 0.8,
            relevant_lines: 0,
            recognized_lines: 0,
            unresolved_lines: Vec::new(),
            probe_attempts: Vec::new(),
            warnings: Vec::new(),
            validation_errors: Vec::new(),
        };

        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("failure_code"));
        assert!(!json.contains("failure_detail"));
    }

    #[test]
    fn test_extraction_report_includes_failure_fields_when_set() {
        let report = ExtractionReport {
            command: "test".to_string(),
            success: false,
            accepted_for_suggestions: false,
            quality_tier: QualityTier::Failed,
            quality_reasons: Vec::new(),
            failure_code: Some(FailureCode::NotInstalled),
            failure_detail: Some("command not found".to_string()),
            selected_format: None,
            format_scores: Vec::new(),
            parsers_used: Vec::new(),
            confidence: 0.0,
            coverage: 0.0,
            relevant_lines: 0,
            recognized_lines: 0,
            unresolved_lines: Vec::new(),
            probe_attempts: Vec::new(),
            warnings: Vec::new(),
            validation_errors: Vec::new(),
        };

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"failure_code\":\"not_installed\""));
        assert!(json.contains("command not found"));
    }
}

/// Batch report for a full discovery run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionReportBundle {
    /// Schema contract version.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub schema_version: Option<String>,
    pub generated_at: String,
    pub version: String,
    pub reports: Vec<ExtractionReport>,
    pub failures: Vec<String>,
}
