//! Discovery helpers and file workflows for schema extraction.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use command_schema_core::{CommandSchema, SchemaPackage, validate_package, validate_schema};

use crate::extractor::{command_exists, extract_command_schema};

/// Default allowlist for schema extraction.
pub const DEFAULT_ALLOWLIST: &[&str] = &[
    "awk",
    "bash",
    "cat",
    "cd",
    "chmod",
    "chown",
    "cp",
    "curl",
    "docker",
    "du",
    "echo",
    "env",
    "find",
    "git",
    "grep",
    "head",
    "jq",
    "kill",
    "kubectl",
    "less",
    "ln",
    "ls",
    "make",
    "mkdir",
    "mv",
    "nano",
    "npm",
    "pnpm",
    "ps",
    "pwd",
    "rg",
    "rm",
    "rmdir",
    "sed",
    "ssh",
    "sudo",
    "systemctl",
    "tail",
    "tar",
    "touch",
    "vim",
    "wget",
    "whoami",
    "xargs",
    "yarn",
    "cargo",
    "rustc",
    "go",
    "python",
    "python3",
];

/// Tool discovery and extraction configuration.
#[derive(Debug, Clone, Default)]
pub struct DiscoverConfig {
    /// Explicit command names supplied by the caller.
    pub commands: Vec<String>,
    /// Include commands from [`DEFAULT_ALLOWLIST`].
    pub use_allowlist: bool,
    /// Include executables found on `PATH`.
    pub scan_path: bool,
    /// Commands to suppress from all discovery sources.
    pub excluded_commands: Vec<String>,
}

/// Aggregated output from a discovery + extraction run.
#[derive(Debug, Clone)]
pub struct DiscoverOutcome {
    /// Package containing all successfully extracted command schemas.
    pub package: SchemaPackage,
    /// Command names that failed extraction.
    pub failures: Vec<String>,
    /// Non-fatal extraction warnings.
    pub warnings: Vec<String>,
}

/// Returns a deterministic, deduplicated command list based on config.
pub fn discover_tools(config: &DiscoverConfig) -> Vec<String> {
    let excluded: BTreeSet<&str> = config
        .excluded_commands
        .iter()
        .map(String::as_str)
        .filter(|cmd| !cmd.is_empty())
        .collect();

    let mut commands: BTreeSet<String> = BTreeSet::new();

    for cmd in &config.commands {
        let trimmed = cmd.trim();
        if trimmed.is_empty() || excluded.contains(trimmed) {
            continue;
        }
        commands.insert(trimmed.to_string());
    }

    if config.use_allowlist {
        for &cmd in DEFAULT_ALLOWLIST {
            if excluded.contains(cmd) {
                continue;
            }
            if command_exists(cmd) {
                commands.insert(cmd.to_string());
            }
        }
    }

    if config.scan_path {
        for cmd in path_executables() {
            if !excluded.contains(cmd.as_str()) {
                commands.insert(cmd);
            }
        }
    }

    commands.into_iter().collect()
}

/// Discovers commands and extracts schemas into a package.
pub fn discover_and_extract(config: &DiscoverConfig, version: &str) -> DiscoverOutcome {
    let commands = discover_tools(config);
    let mut package = SchemaPackage::new(version, Utc::now().to_rfc3339());
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    for command in commands {
        let result = extract_command_schema(&command);
        let command_label = command.clone();

        if result.success {
            if let Some(schema) = result.schema {
                package.schemas.push(schema);
            } else {
                failures.push(command);
            }
        } else {
            failures.push(command);
        }

        warnings.extend(
            result
                .warnings
                .into_iter()
                .map(|warning| format!("{}: {}", command_label, warning)),
        );
    }

    DiscoverOutcome {
        package,
        failures,
        warnings,
    }
}

/// Collects JSON schema file paths from input files and/or directories.
pub fn collect_schema_paths(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    if inputs.is_empty() {
        return Err("No schema paths were provided".to_string());
    }

    let mut paths = BTreeSet::new();

    for input in inputs {
        if input.is_dir() {
            let entries = fs::read_dir(input)
                .map_err(|err| format!("Failed to read directory '{}': {err}", input.display()))?;

            for entry in entries {
                let entry = entry.map_err(|err| {
                    format!(
                        "Failed to read directory entry in '{}': {err}",
                        input.display()
                    )
                })?;
                let path = entry.path();
                if path.extension() == Some(OsStr::new("json")) {
                    paths.insert(path);
                }
            }
            continue;
        }

        if input.is_file() {
            if input.extension() != Some(OsStr::new("json")) {
                return Err(format!(
                    "Schema file '{}' must end in .json",
                    input.display()
                ));
            }
            paths.insert(input.clone());
            continue;
        }

        return Err(format!("Schema path '{}' does not exist", input.display()));
    }

    if paths.is_empty() {
        return Err("No schema JSON files found in provided paths".to_string());
    }

    Ok(paths.into_iter().collect())
}

/// Loads and validates all command schemas from files.
pub fn load_and_validate_schemas(paths: &[PathBuf]) -> Result<Vec<CommandSchema>, String> {
    let mut schemas = Vec::with_capacity(paths.len());

    for path in paths {
        let raw = fs::read_to_string(path)
            .map_err(|err| format!("Failed to read '{}': {err}", path.display()))?;
        let schema: CommandSchema = serde_json::from_str(&raw)
            .map_err(|err| format!("Invalid schema JSON '{}': {err}", path.display()))?;

        let errors = validate_schema(&schema);
        if let Some(first) = errors.first() {
            return Err(format!(
                "Schema validation failed for '{}': {first}",
                path.display()
            ));
        }

        schemas.push(schema);
    }

    Ok(schemas)
}

/// Bundles multiple schema files into a validated [`SchemaPackage`].
pub fn bundle_schema_files(
    paths: &[PathBuf],
    version: &str,
    name: Option<String>,
    description: Option<String>,
) -> Result<SchemaPackage, String> {
    let schemas = load_and_validate_schemas(paths)?;

    let mut package = SchemaPackage::new(version, Utc::now().to_rfc3339());
    package.name = name;
    package.description = description;
    package.schemas = schemas;

    let errors = validate_package(&package);
    if let Some(first) = errors.first() {
        return Err(format!("Schema package validation failed: {first}"));
    }

    Ok(package)
}

fn path_executables() -> Vec<String> {
    let Some(path_env) = env::var_os("PATH") else {
        return Vec::new();
    };

    let mut commands = BTreeSet::new();

    for dir in env::split_paths(&path_env) {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !is_executable(&path) {
                continue;
            }

            if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
                commands.insert(name.to_string());
            }
        }
    }

    commands.into_iter().collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };

    metadata.is_file() && (metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use command_schema_core::{FlagSchema, SchemaSource};

    use super::*;

    #[test]
    fn test_discover_tools_dedupes_and_applies_exclusions() {
        let config = DiscoverConfig {
            commands: vec!["git".to_string(), "git".to_string(), "cargo".to_string()],
            use_allowlist: false,
            scan_path: false,
            excluded_commands: vec!["cargo".to_string()],
        };

        assert_eq!(discover_tools(&config), vec!["git".to_string()]);
    }

    #[test]
    fn test_collect_schema_paths_from_dir_filters_non_json() {
        let root = unique_tmp_dir();
        fs::create_dir_all(&root).unwrap();

        let json_path = root.join("git.json");
        let txt_path = root.join("notes.txt");
        fs::write(&json_path, "{}").unwrap();
        fs::write(&txt_path, "ignore").unwrap();

        let paths = collect_schema_paths(&[root]).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], json_path);
    }

    #[test]
    fn test_bundle_schema_files_rejects_duplicate_commands() {
        let root = unique_tmp_dir();
        fs::create_dir_all(&root).unwrap();

        let schema = CommandSchema {
            command: "git".to_string(),
            description: Some("Git tool".to_string()),
            global_flags: vec![FlagSchema::boolean(Some("-v"), Some("--verbose"))],
            subcommands: Vec::new(),
            positional: Vec::new(),
            source: SchemaSource::Bootstrap,
            confidence: 1.0,
            version: None,
        };

        let file_a = root.join("a.json");
        let file_b = root.join("b.json");
        let raw = serde_json::to_string_pretty(&schema).unwrap();
        fs::write(&file_a, &raw).unwrap();
        fs::write(&file_b, &raw).unwrap();

        let result = bundle_schema_files(&[file_a, file_b], "1.0.0", None, None);
        assert!(result.is_err());
    }

    fn unique_tmp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("wrashpty-schema-discovery-{nanos}"))
    }
}
