//! Offline command schema discovery and parsing.

pub mod cache;
pub mod discover;
pub mod extractor;
pub mod output;
pub mod parser;
pub mod report;
pub mod version;

use command_schema_core::ExtractionResult;
use extractor::{ExtractionQualityPolicy, ExtractionRun};
use parser::HelpParser;
use report::{ExtractionReport, FailureCode, QualityTier};

/// Parses pre-captured help text into a schema without executing any commands.
pub fn parse_help_text(command: &str, help_text: &str) -> ExtractionResult {
    let mut parser = HelpParser::new(command, help_text);
    let schema = parser.parse().map(|mut s| {
        s.schema_version = Some(command_schema_core::SCHEMA_CONTRACT_VERSION.to_string());
        s
    });
    let warnings = parser.warnings().to_vec();
    let detected_format = parser.detected_format();
    let success = schema.is_some();

    ExtractionResult {
        schema,
        raw_output: help_text.to_string(),
        detected_format,
        warnings,
        success,
    }
}

/// Parses pre-captured help text with full reporting and quality policy gating.
pub fn parse_help_text_with_report(
    command: &str,
    help_text: &str,
    policy: ExtractionQualityPolicy,
) -> ExtractionRun {
    let mut parser = HelpParser::new(command, help_text);
    let schema = parser.parse().map(|mut s| {
        s.schema_version = Some(command_schema_core::SCHEMA_CONTRACT_VERSION.to_string());
        s
    });
    let warnings = parser.warnings().to_vec();
    let detected_format = parser.detected_format();
    let diagnostics = parser.diagnostics().clone();

    let (success, failure_code, failure_detail) = match &schema {
        Some(s) => {
            let has_entities = !s.global_flags.is_empty()
                || !s.subcommands.is_empty()
                || !s.positional.is_empty();
            if has_entities {
                (true, None, None)
            } else {
                (
                    false,
                    Some(FailureCode::ParseFailed),
                    Some("Parsed schema contains no entities".to_string()),
                )
            }
        }
        None => (
            false,
            Some(FailureCode::ParseFailed),
            Some("Help text parsing produced no schema".to_string()),
        ),
    };

    let confidence = schema.as_ref().map_or(0.0, |s| s.confidence);

    let run = ExtractionRun {
        result: ExtractionResult {
            schema,
            raw_output: help_text.to_string(),
            detected_format,
            warnings: warnings.clone(),
            success,
        },
        report: ExtractionReport {
            command: command.to_string(),
            success,
            accepted_for_suggestions: false,
            quality_tier: QualityTier::Failed,
            quality_reasons: Vec::new(),
            failure_code,
            failure_detail,
            selected_format: detected_format.map(extractor::help_format_label),
            format_scores: extractor::to_format_score_reports(&diagnostics.format_scores),
            confidence,
            coverage: diagnostics.coverage(),
            relevant_lines: diagnostics.relevant_lines,
            recognized_lines: diagnostics.recognized_lines,
            unresolved_lines: diagnostics.unresolved_lines.clone(),
            parsers_used: diagnostics.parsers_used,
            probe_attempts: Vec::new(),
            warnings,
            validation_errors: Vec::new(),
        },
    };

    extractor::apply_quality_policy(run, policy)
}
