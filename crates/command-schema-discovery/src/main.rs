use std::fs;
use std::io::Read;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use command_schema_discovery::discover::{
    DiscoverConfig, build_report_bundle, bundle_schema_files, collect_schema_paths,
    discover_and_extract, failure_code_summary, load_and_validate_schemas,
};
use command_schema_discovery::extractor::{
    DEFAULT_MIN_CONFIDENCE, DEFAULT_MIN_COVERAGE, ExtractionQualityPolicy,
};

const PACKAGE_VERSION: &str = "1.0.0";

#[derive(Debug, Parser)]
#[command(name = "schema-discover")]
#[command(about = "Offline command schema discovery and bundling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Extract command schemas from local tool help output.
    Extract(ExtractArgs),
    /// Validate one or more schema JSON files.
    Validate(ValidateArgs),
    /// Bundle schema JSON files into a SchemaPackage file.
    Bundle(BundleArgs),
    /// Parse help text from stdin without executing commands.
    ParseStdin(ParseStdinArgs),
    /// Parse help text from a file without executing commands.
    ParseFile(ParseFileArgs),
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// Comma-separated explicit commands (e.g. git,docker,cargo).
    #[arg(long)]
    commands: Option<String>,
    /// Include installed commands from the curated allowlist.
    #[arg(long)]
    allowlist: bool,
    /// Include executables discovered on PATH.
    #[arg(long)]
    scan_path: bool,
    /// Comma-separated commands to exclude.
    #[arg(long)]
    exclude: Option<String>,
    /// Output directory for per-command JSON files.
    #[arg(long)]
    output: PathBuf,
    /// Minimum schema confidence (0.0-1.0) required for acceptance.
    #[arg(long, default_value_t = DEFAULT_MIN_CONFIDENCE)]
    min_confidence: f64,
    /// Minimum parser coverage (0.0-1.0) required for acceptance.
    #[arg(long, default_value_t = DEFAULT_MIN_COVERAGE)]
    min_coverage: f64,
    /// Keep low-quality schemas instead of rejecting them.
    #[arg(long)]
    allow_low_quality: bool,
    /// Only extract schemas for commands installed on the system.
    #[arg(long)]
    installed_only: bool,
    /// Number of parallel extraction jobs (default: number of CPUs).
    #[arg(long)]
    jobs: Option<usize>,
    /// Directory for caching extraction results.
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Disable caching entirely.
    #[arg(long)]
    no_cache: bool,
    /// Output format for schema and report files (default: json).
    #[arg(long, default_value = "json")]
    format: command_schema_discovery::output::OutputFormat,
}

#[derive(Debug, Args)]
struct ValidateArgs {
    /// Schema files and/or directories containing schema JSON files.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct BundleArgs {
    /// Schema files and/or directories containing schema JSON files.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output JSON bundle path.
    #[arg(long)]
    output: PathBuf,
    /// Optional bundle name metadata.
    #[arg(long)]
    name: Option<String>,
    /// Optional bundle description metadata.
    #[arg(long)]
    description: Option<String>,
}

#[derive(Debug, Args)]
struct ParseStdinArgs {
    /// Command name for the help text being parsed.
    #[arg(long)]
    command: String,
    /// Output both schema and extraction report.
    #[arg(long)]
    with_report: bool,
    /// Output format.
    #[arg(long, default_value = "json")]
    format: command_schema_discovery::output::OutputFormat,
}

#[derive(Debug, Args)]
struct ParseFileArgs {
    /// Command name for the help text being parsed.
    #[arg(long)]
    command: String,
    /// Path to file containing help text.
    #[arg(long)]
    input: PathBuf,
    /// Output both schema and extraction report.
    #[arg(long)]
    with_report: bool,
    /// Output format.
    #[arg(long, default_value = "json")]
    format: command_schema_discovery::output::OutputFormat,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Extract(args) => run_extract(args),
        Command::Validate(args) => run_validate(args),
        Command::Bundle(args) => run_bundle(args),
        Command::ParseStdin(args) => run_parse_stdin(args),
        Command::ParseFile(args) => run_parse_file(args),
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run_extract(args: ExtractArgs) -> Result<(), String> {
    let commands = parse_csv_list(args.commands);
    let excluded_commands = parse_csv_list(args.exclude);

    if commands.is_empty() && !args.allowlist && !args.scan_path {
        return Err(
            "Specify at least one discovery source: --commands, --allowlist, or --scan-path"
                .to_string(),
        );
    }
    if !(0.0..=1.0).contains(&args.min_confidence) {
        return Err("--min-confidence must be between 0.0 and 1.0".to_string());
    }
    if !(0.0..=1.0).contains(&args.min_coverage) {
        return Err("--min-coverage must be between 0.0 and 1.0".to_string());
    }

    fs::create_dir_all(&args.output).map_err(|err| {
        format!(
            "Failed to create output directory '{}': {err}",
            args.output.display()
        )
    })?;

    let config = DiscoverConfig {
        commands,
        use_allowlist: args.allowlist,
        scan_path: args.scan_path,
        excluded_commands,
        quality_policy: ExtractionQualityPolicy {
            min_confidence: args.min_confidence,
            min_coverage: args.min_coverage,
            allow_low_quality: args.allow_low_quality,
        },
        installed_only: args.installed_only,
        jobs: args.jobs,
        cache_dir: if args.no_cache {
            None
        } else {
            Some(
                args.cache_dir
                    .unwrap_or_else(command_schema_discovery::cache::SchemaCache::default_dir),
            )
        },
    };

    let format = args.format;
    let outcome = discover_and_extract(&config, PACKAGE_VERSION);

    let ext = format_extension(format);

    let mut written = 0usize;
    for schema in &outcome.package.schemas {
        let path = args.output.join(format!("{}.{ext}", schema.command));
        let raw = command_schema_discovery::output::format_schema(schema, format)?;
        fs::write(&path, raw)
            .map_err(|err| format!("Failed to write '{}': {err}", path.display()))?;
        written += 1;
    }

    println!("Extracted and wrote {written} schema file(s).");

    let report_bundle = build_report_bundle(
        PACKAGE_VERSION,
        outcome.reports.clone(),
        outcome.failures.clone(),
    );
    let report_path = args.output.join(format!("extraction-report.{ext}"));
    let report_raw = format_report_bundle(&report_bundle, format)?;
    fs::write(&report_path, report_raw)
        .map_err(|err| format!("Failed to write '{}': {err}", report_path.display()))?;

    if !outcome.failures.is_empty() {
        let summary = failure_code_summary(&outcome.reports);
        if summary.is_empty() {
            eprintln!(
                "{} extraction failure(s): {}",
                outcome.failures.len(),
                outcome.failures.join(", ")
            );
        } else {
            let breakdown: Vec<String> = summary
                .iter()
                .map(|(code, count)| format!("{count} {code}"))
                .collect();
            eprintln!(
                "{} extraction failure(s) ({}): {}",
                outcome.failures.len(),
                breakdown.join(", "),
                outcome.failures.join(", ")
            );
        }
    }

    if !outcome.warnings.is_empty() {
        eprintln!(
            "{} warning(s) emitted during extraction.",
            outcome.warnings.len()
        );
    }

    Ok(())
}

fn run_validate(args: ValidateArgs) -> Result<(), String> {
    let paths = collect_schema_paths(&args.inputs)?;
    let schemas = load_and_validate_schemas(&paths)?;
    println!(
        "Validated {} schema file(s) for {} command(s).",
        paths.len(),
        schemas.len()
    );
    Ok(())
}

fn run_bundle(args: BundleArgs) -> Result<(), String> {
    let paths = collect_schema_paths(&args.inputs)?;
    let package = bundle_schema_files(&paths, PACKAGE_VERSION, args.name, args.description)?;

    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "Failed to create output directory '{}': {err}",
                    parent.display()
                )
            })?;
        }
    }

    let raw = serde_json::to_string_pretty(&package)
        .map_err(|err| format!("Failed to serialize schema bundle: {err}"))?;
    fs::write(&args.output, raw)
        .map_err(|err| format!("Failed to write '{}': {err}", args.output.display()))?;

    println!(
        "Bundled {} schema(s) into '{}'.",
        package.schema_count(),
        args.output.display()
    );

    Ok(())
}

fn run_parse_stdin(args: ParseStdinArgs) -> Result<(), String> {
    let mut help_text = String::new();
    std::io::stdin()
        .read_to_string(&mut help_text)
        .map_err(|err| format!("Failed to read stdin: {err}"))?;
    run_parse_help_text(&args.command, &help_text, args.with_report, args.format)
}

fn run_parse_file(args: ParseFileArgs) -> Result<(), String> {
    let help_text = fs::read_to_string(&args.input)
        .map_err(|err| format!("Failed to read '{}': {err}", args.input.display()))?;
    run_parse_help_text(&args.command, &help_text, args.with_report, args.format)
}

fn run_parse_help_text(
    command: &str,
    help_text: &str,
    with_report: bool,
    format: command_schema_discovery::output::OutputFormat,
) -> Result<(), String> {
    use command_schema_discovery::output::{OutputFormat, format_report, format_schema};

    if with_report {
        let run = command_schema_discovery::parse_help_text_with_report(
            command,
            help_text,
            ExtractionQualityPolicy::permissive(),
        );

        match format {
            OutputFormat::Json => {
                #[derive(serde::Serialize)]
                struct ParseOutput {
                    #[serde(skip_serializing_if = "Option::is_none")]
                    schema: Option<command_schema_core::CommandSchema>,
                    report: command_schema_discovery::report::ExtractionReport,
                }

                let output = ParseOutput {
                    schema: run.result.schema,
                    report: run.report,
                };
                let json = serde_json::to_string_pretty(&output)
                    .map_err(|e| format!("Failed to serialize output: {e}"))?;
                println!("{json}");
            }
            _ => {
                if let Some(ref schema) = run.result.schema {
                    print!("{}", format_schema(schema, format)?);
                }
                print!("{}", format_report(&run.report, format)?);
            }
        }
    } else {
        let result = command_schema_discovery::parse_help_text(command, help_text);
        match result.schema {
            Some(schema) => {
                let output = format_schema(&schema, format)?;
                println!("{output}");
            }
            None => {
                return Err(format!(
                    "Failed to parse help text for '{}': {}",
                    command,
                    result.warnings.join("; ")
                ));
            }
        }
    }
    Ok(())
}

/// Returns the file extension for the given output format.
fn format_extension(format: command_schema_discovery::output::OutputFormat) -> &'static str {
    use command_schema_discovery::output::OutputFormat;
    match format {
        OutputFormat::Json => "json",
        OutputFormat::Yaml => "yaml",
        OutputFormat::Markdown => "md",
        OutputFormat::Table => "txt",
    }
}

/// Formats an [`ExtractionReportBundle`] in the requested output format.
fn format_report_bundle(
    bundle: &command_schema_discovery::report::ExtractionReportBundle,
    format: command_schema_discovery::output::OutputFormat,
) -> Result<String, String> {
    use command_schema_discovery::output::OutputFormat;
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(bundle)
            .map_err(|e| format!("JSON serialization failed: {e}")),
        OutputFormat::Yaml => serde_yaml::to_string(bundle)
            .map_err(|e| format!("YAML serialization failed: {e}")),
        OutputFormat::Markdown | OutputFormat::Table => {
            let mut out = String::new();
            for report in &bundle.reports {
                out.push_str(
                    &command_schema_discovery::output::format_report(report, format)?,
                );
            }
            Ok(out)
        }
    }
}

fn parse_csv_list(raw: Option<String>) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::parse_csv_list;

    #[test]
    fn test_parse_csv_list_trims_and_drops_empty() {
        let parsed = parse_csv_list(Some(" git, docker, ,cargo ".to_string()));
        assert_eq!(parsed, vec!["git", "docker", "cargo"]);
    }

    #[test]
    fn test_parse_csv_list_none_is_empty() {
        let parsed = parse_csv_list(None);
        assert!(parsed.is_empty());
    }
}
