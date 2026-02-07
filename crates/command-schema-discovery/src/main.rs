use std::fs;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use command_schema_discovery::discover::{
    DiscoverConfig, build_report_bundle, bundle_schema_files, collect_schema_paths,
    discover_and_extract, load_and_validate_schemas,
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

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Extract(args) => run_extract(args),
        Command::Validate(args) => run_validate(args),
        Command::Bundle(args) => run_bundle(args),
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
    };

    let outcome = discover_and_extract(&config, PACKAGE_VERSION);

    let mut written = 0usize;
    for schema in &outcome.package.schemas {
        let path = args.output.join(format!("{}.json", schema.command));
        let raw = serde_json::to_string_pretty(schema)
            .map_err(|err| format!("Failed to serialize schema '{}': {err}", schema.command))?;
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
    let report_path = args.output.join("extraction-report.json");
    let report_json = serde_json::to_string_pretty(&report_bundle)
        .map_err(|err| format!("Failed to serialize extraction report: {err}"))?;
    fs::write(&report_path, report_json)
        .map_err(|err| format!("Failed to write '{}': {err}", report_path.display()))?;

    if !outcome.failures.is_empty() {
        eprintln!(
            "{} extraction failure(s): {}",
            outcome.failures.len(),
            outcome.failures.join(", ")
        );
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
