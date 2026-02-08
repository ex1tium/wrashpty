//! Output formatting for schemas and reports.

use command_schema_core::CommandSchema;

use crate::report::ExtractionReport;

/// Supported output formats.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Json,
    Yaml,
    Markdown,
    Table,
}

/// Formats a schema in the requested output format.
pub fn format_schema(schema: &CommandSchema, format: OutputFormat) -> Result<String, String> {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(schema)
            .map_err(|e| format!("JSON serialization failed: {e}")),
        OutputFormat::Yaml => serde_yaml::to_string(schema)
            .map_err(|e| format!("YAML serialization failed: {e}")),
        OutputFormat::Markdown => Ok(schema_to_markdown(schema)),
        OutputFormat::Table => Ok(schema_to_table(schema)),
    }
}

/// Formats an extraction report in the requested output format.
pub fn format_report(report: &ExtractionReport, format: OutputFormat) -> Result<String, String> {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(report)
            .map_err(|e| format!("JSON serialization failed: {e}")),
        OutputFormat::Yaml => {
            serde_yaml::to_string(report).map_err(|e| format!("YAML serialization failed: {e}"))
        }
        OutputFormat::Markdown => Ok(report_to_markdown(report)),
        OutputFormat::Table => Ok(report_to_table(report)),
    }
}

fn schema_to_markdown(schema: &CommandSchema) -> String {
    let mut out = String::new();

    out.push_str(&format!("# {}\n\n", schema.command));

    if let Some(ref desc) = schema.description {
        out.push_str(&format!("{desc}\n\n"));
    }

    if let Some(ref version) = schema.version {
        out.push_str(&format!("**Version:** {version}\n\n"));
    }

    out.push_str(&format!(
        "**Confidence:** {:.0}%\n\n",
        schema.confidence * 100.0
    ));

    if !schema.global_flags.is_empty() {
        out.push_str("## Global Flags\n\n");
        out.push_str("| Flag | Description |\n");
        out.push_str("|------|-------------|\n");
        for flag in &schema.global_flags {
            let name = match (&flag.short, &flag.long) {
                (Some(s), Some(l)) => format!("{s}, {l}"),
                (Some(s), None) => s.clone(),
                (None, Some(l)) => l.clone(),
                (None, None) => "?".to_string(),
            };
            let desc = flag.description.as_deref().unwrap_or("");
            out.push_str(&format!("| `{name}` | {desc} |\n"));
        }
        out.push('\n');
    }

    if !schema.positional.is_empty() {
        out.push_str("## Arguments\n\n");
        out.push_str("| Argument | Required | Description |\n");
        out.push_str("|----------|----------|-------------|\n");
        for arg in &schema.positional {
            let required = if arg.required { "yes" } else { "no" };
            let desc = arg.description.as_deref().unwrap_or("");
            out.push_str(&format!("| `{}` | {required} | {desc} |\n", arg.name));
        }
        out.push('\n');
    }

    if !schema.subcommands.is_empty() {
        out.push_str("## Subcommands\n\n");
        out.push_str("| Subcommand | Description |\n");
        out.push_str("|------------|-------------|\n");
        for sub in &schema.subcommands {
            let desc = sub.description.as_deref().unwrap_or("");
            out.push_str(&format!("| `{}` | {desc} |\n", sub.name));
        }
        out.push('\n');
    }

    out
}

fn schema_to_table(schema: &CommandSchema) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "Command: {}  Confidence: {:.0}%",
        schema.command,
        schema.confidence * 100.0
    ));
    if let Some(ref version) = schema.version {
        out.push_str(&format!("  Version: {version}"));
    }
    out.push('\n');

    if let Some(ref desc) = schema.description {
        out.push_str(&format!("  {desc}\n"));
    }

    if !schema.global_flags.is_empty() {
        out.push_str("\nFlags:\n");
        let max_name = schema
            .global_flags
            .iter()
            .map(|f| {
                let name = match (&f.short, &f.long) {
                    (Some(s), Some(l)) => format!("{s}, {l}"),
                    (Some(s), None) => s.clone(),
                    (None, Some(l)) => l.clone(),
                    _ => "?".to_string(),
                };
                name.len()
            })
            .max()
            .unwrap_or(4);

        for flag in &schema.global_flags {
            let name = match (&flag.short, &flag.long) {
                (Some(s), Some(l)) => format!("{s}, {l}"),
                (Some(s), None) => s.clone(),
                (None, Some(l)) => l.clone(),
                _ => "?".to_string(),
            };
            let desc = flag.description.as_deref().unwrap_or("");
            out.push_str(&format!("  {:<width$}  {desc}\n", name, width = max_name));
        }
    }

    if !schema.subcommands.is_empty() {
        out.push_str("\nSubcommands:\n");
        let max_name = schema
            .subcommands
            .iter()
            .map(|s| s.name.len())
            .max()
            .unwrap_or(4);

        for sub in &schema.subcommands {
            let desc = sub.description.as_deref().unwrap_or("");
            out.push_str(&format!(
                "  {:<width$}  {desc}\n",
                sub.name,
                width = max_name
            ));
        }
    }

    out
}

fn report_to_markdown(report: &ExtractionReport) -> String {
    let mut out = String::new();

    out.push_str(&format!("# Extraction Report: {}\n\n", report.command));
    out.push_str(&format!(
        "- **Success:** {}\n",
        if report.success { "yes" } else { "no" }
    ));
    out.push_str(&format!("- **Quality Tier:** {:?}\n", report.quality_tier));
    out.push_str(&format!("- **Confidence:** {:.2}\n", report.confidence));
    out.push_str(&format!("- **Coverage:** {:.2}\n", report.coverage));

    if let Some(ref code) = report.failure_code {
        out.push_str(&format!("- **Failure Code:** {code}\n"));
    }
    if let Some(ref detail) = report.failure_detail {
        out.push_str(&format!("- **Failure Detail:** {detail}\n"));
    }

    if !report.warnings.is_empty() {
        out.push_str("\n## Warnings\n\n");
        for w in &report.warnings {
            out.push_str(&format!("- {w}\n"));
        }
    }

    out
}

fn report_to_table(report: &ExtractionReport) -> String {
    let mut out = String::new();
    let status = if report.success { "OK" } else { "FAIL" };
    out.push_str(&format!(
        "{:<20} {:<6} {:<10} conf={:.2} cov={:.2}",
        report.command,
        status,
        format!("{:?}", report.quality_tier),
        report.confidence,
        report.coverage,
    ));
    if let Some(ref code) = report.failure_code {
        out.push_str(&format!("  [{code}]"));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use command_schema_core::{FlagSchema, SchemaSource, SubcommandSchema};

    #[test]
    fn test_format_schema_json() {
        let schema = CommandSchema::new("test", SchemaSource::HelpCommand);
        let result = format_schema(&schema, OutputFormat::Json);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("\"command\": \"test\""));
    }

    #[test]
    fn test_format_schema_yaml() {
        let schema = CommandSchema::new("test", SchemaSource::HelpCommand);
        let result = format_schema(&schema, OutputFormat::Yaml);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("command: test"));
    }

    #[test]
    fn test_format_schema_markdown() {
        let mut schema = CommandSchema::new("test", SchemaSource::HelpCommand);
        schema.global_flags.push(FlagSchema::boolean(Some("-v"), Some("--verbose")));
        schema.subcommands.push(SubcommandSchema::new("build"));

        let result = format_schema(&schema, OutputFormat::Markdown);
        assert!(result.is_ok());
        let md = result.unwrap();
        assert!(md.contains("# test"));
        assert!(md.contains("--verbose"));
        assert!(md.contains("build"));
    }

    #[test]
    fn test_format_schema_table() {
        let schema = CommandSchema::new("test", SchemaSource::HelpCommand);
        let result = format_schema(&schema, OutputFormat::Table);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Command: test"));
    }

    fn sample_report() -> ExtractionReport {
        ExtractionReport {
            command: "mycmd".to_string(),
            success: true,
            accepted_for_suggestions: true,
            quality_tier: crate::report::QualityTier::High,
            quality_reasons: vec!["good".to_string()],
            failure_code: None,
            failure_detail: None,
            selected_format: Some("gnu".to_string()),
            format_scores: Vec::new(),
            parsers_used: vec!["gnu".to_string()],
            confidence: 0.92,
            coverage: 0.85,
            relevant_lines: 20,
            recognized_lines: 17,
            unresolved_lines: Vec::new(),
            probe_attempts: Vec::new(),
            warnings: Vec::new(),
            validation_errors: Vec::new(),
        }
    }

    #[test]
    fn test_format_report_json() {
        let report = sample_report();
        let result = format_report(&report, OutputFormat::Json);
        assert!(result.is_ok());
        let json = result.unwrap();
        assert!(json.contains("\"command\": \"mycmd\""));
        assert!(json.contains("\"confidence\": 0.92"));
    }

    #[test]
    fn test_format_report_yaml() {
        let report = sample_report();
        let result = format_report(&report, OutputFormat::Yaml);
        assert!(result.is_ok());
        let yaml = result.unwrap();
        assert!(yaml.contains("command: mycmd"));
    }

    #[test]
    fn test_format_report_markdown() {
        let report = sample_report();
        let result = format_report(&report, OutputFormat::Markdown);
        assert!(result.is_ok());
        let md = result.unwrap();
        assert!(md.contains("# Extraction Report: mycmd"));
        assert!(md.contains("**Success:** yes"));
        assert!(md.contains("**Confidence:** 0.92"));
    }

    #[test]
    fn test_format_report_markdown_with_failure() {
        let mut report = sample_report();
        report.success = false;
        report.failure_code = Some(crate::report::FailureCode::ParseFailed);
        report.failure_detail = Some("could not parse".to_string());
        report.warnings = vec!["some warning".to_string()];
        let md = format_report(&report, OutputFormat::Markdown).unwrap();
        assert!(md.contains("**Success:** no"));
        assert!(md.contains("**Failure Code:** parse_failed"));
        assert!(md.contains("could not parse"));
        assert!(md.contains("some warning"));
    }

    #[test]
    fn test_format_report_table() {
        let report = sample_report();
        let result = format_report(&report, OutputFormat::Table);
        assert!(result.is_ok());
        let table = result.unwrap();
        assert!(table.contains("mycmd"));
        assert!(table.contains("OK"));
    }

    #[test]
    fn test_format_report_table_failure() {
        let mut report = sample_report();
        report.success = false;
        report.failure_code = Some(crate::report::FailureCode::NotInstalled);
        let table = format_report(&report, OutputFormat::Table).unwrap();
        assert!(table.contains("FAIL"));
        assert!(table.contains("[not_installed]"));
    }

    #[test]
    fn test_format_schema_markdown_with_positional_args() {
        let mut schema = CommandSchema::new("test", SchemaSource::HelpCommand);
        schema.positional.push(command_schema_core::ArgSchema {
            name: "file".to_string(),
            value_type: command_schema_core::ValueType::File,
            required: true,
            multiple: false,
            description: Some("Input file".to_string()),
        });
        let md = format_schema(&schema, OutputFormat::Markdown).unwrap();
        assert!(md.contains("## Arguments"));
        assert!(md.contains("`file`"));
        assert!(md.contains("yes"));
        assert!(md.contains("Input file"));
    }

    #[test]
    fn test_format_schema_table_with_version() {
        let mut schema = CommandSchema::new("test", SchemaSource::HelpCommand);
        schema.version = Some("1.2.3".to_string());
        let table = format_schema(&schema, OutputFormat::Table).unwrap();
        assert!(table.contains("Version: 1.2.3"));
    }
}
