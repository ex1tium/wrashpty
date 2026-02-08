//! Integration tests for parse-stdin, parse-file, and multi-format output flows.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn schema_discover_bin() -> PathBuf {
    // `cargo test` places the binary in the target directory.
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_schema-discover"));
    // Fallback: if the env var is empty, try building first.
    if !path.exists() {
        path = PathBuf::from("target/debug/schema-discover");
    }
    path
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

// ---- parse-file tests ----

#[test]
fn test_parse_file_json_output() {
    let bin = schema_discover_bin();
    let output = Command::new(&bin)
        .args(["parse-file", "--command", "git", "--input"])
        .arg(fixture("git-help.txt").to_str().unwrap())
        .output()
        .expect("failed to run schema-discover");

    assert!(
        output.status.success(),
        "parse-file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Invalid JSON output: {e}\n{stdout}"));
    assert_eq!(parsed["command"], "git");
    assert!(parsed["global_flags"].is_array());
    assert!(parsed["subcommands"].is_array());
}

#[test]
fn test_parse_file_yaml_output() {
    let bin = schema_discover_bin();
    let output = Command::new(&bin)
        .args([
            "parse-file",
            "--command",
            "git",
            "--format",
            "yaml",
            "--input",
        ])
        .arg(fixture("git-help.txt").to_str().unwrap())
        .output()
        .expect("failed to run schema-discover");

    assert!(
        output.status.success(),
        "parse-file --format yaml failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("command: git"), "YAML should contain command field");
}

#[test]
fn test_parse_file_markdown_output() {
    let bin = schema_discover_bin();
    let output = Command::new(&bin)
        .args([
            "parse-file",
            "--command",
            "git",
            "--format",
            "markdown",
            "--input",
        ])
        .arg(fixture("git-help.txt").to_str().unwrap())
        .output()
        .expect("failed to run schema-discover");

    assert!(
        output.status.success(),
        "parse-file --format markdown failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("# git"), "Markdown should contain heading");
}

#[test]
fn test_parse_file_table_output() {
    let bin = schema_discover_bin();
    let output = Command::new(&bin)
        .args([
            "parse-file",
            "--command",
            "git",
            "--format",
            "table",
            "--input",
        ])
        .arg(fixture("git-help.txt").to_str().unwrap())
        .output()
        .expect("failed to run schema-discover");

    assert!(
        output.status.success(),
        "parse-file --format table failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Command: git"), "Table should contain command label");
}

#[test]
fn test_parse_file_with_report() {
    let bin = schema_discover_bin();
    let output = Command::new(&bin)
        .args([
            "parse-file",
            "--command",
            "git",
            "--with-report",
            "--input",
        ])
        .arg(fixture("git-help.txt").to_str().unwrap())
        .output()
        .expect("failed to run schema-discover");

    assert!(
        output.status.success(),
        "parse-file --with-report failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Invalid JSON with report: {e}\n{stdout}"));

    // Should have both schema and report fields
    assert!(parsed.get("report").is_some(), "should contain report field");
    assert!(parsed["report"]["command"] == "git");
}

// ---- parse-stdin tests ----

#[test]
fn test_parse_stdin_json_output() {
    let bin = schema_discover_bin();
    let help_text = fs::read_to_string(fixture("kubectl-help.txt"))
        .expect("fixture should be readable");

    let mut child = Command::new(&bin)
        .args(["parse-stdin", "--command", "kubectl"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn schema-discover");

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(help_text.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().expect("failed to wait");
    assert!(
        output.status.success(),
        "parse-stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Invalid JSON: {e}\n{stdout}"));
    assert_eq!(parsed["command"], "kubectl");
}

#[test]
fn test_parse_stdin_with_report_yaml() {
    let bin = schema_discover_bin();
    let help_text = fs::read_to_string(fixture("ls-help.txt"))
        .expect("fixture should be readable");

    let mut child = Command::new(&bin)
        .args([
            "parse-stdin",
            "--command",
            "ls",
            "--with-report",
            "--format",
            "yaml",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn schema-discover");

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(help_text.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().expect("failed to wait");
    assert!(
        output.status.success(),
        "parse-stdin --with-report --format yaml failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // YAML format with --with-report outputs schema then report
    assert!(stdout.contains("command:"), "YAML output should contain command field");
}

// ---- library-level integration tests ----

#[test]
fn test_parse_help_text_stamps_schema_version() {
    let help = fs::read_to_string(fixture("git-help.txt")).unwrap();
    let result = command_schema_discovery::parse_help_text("git", &help);
    assert!(result.success);
    let schema = result.schema.unwrap();
    assert_eq!(
        schema.schema_version,
        Some(command_schema_core::SCHEMA_CONTRACT_VERSION.to_string())
    );
}

#[test]
fn test_parse_help_text_with_report_stamps_version() {
    let help = fs::read_to_string(fixture("git-help.txt")).unwrap();
    let run = command_schema_discovery::parse_help_text_with_report(
        "git",
        &help,
        command_schema_discovery::extractor::ExtractionQualityPolicy::permissive(),
    );
    assert!(run.result.success);
    let schema = run.result.schema.unwrap();
    assert_eq!(
        schema.schema_version,
        Some(command_schema_core::SCHEMA_CONTRACT_VERSION.to_string())
    );
    assert_eq!(run.report.command, "git");
}

#[test]
fn test_parse_help_text_failure_returns_no_schema() {
    let result = command_schema_discovery::parse_help_text("fakecmd", "this is not help text at all");
    // May or may not succeed depending on parser heuristics, but if no schema
    // is produced, success should be false.
    if result.schema.is_none() {
        assert!(!result.success);
    }
}

#[test]
fn test_multi_format_output_consistency() {
    let help = fs::read_to_string(fixture("git-help.txt")).unwrap();
    let result = command_schema_discovery::parse_help_text("git", &help);
    let schema = result.schema.expect("git should parse");

    // All formats should produce non-empty output
    use command_schema_discovery::output::{OutputFormat, format_schema};
    for format in [OutputFormat::Json, OutputFormat::Yaml, OutputFormat::Markdown, OutputFormat::Table] {
        let output = format_schema(&schema, format).expect("formatting should succeed");
        assert!(!output.is_empty(), "format {:?} produced empty output", format);
        assert!(output.contains("git"), "format {:?} should mention 'git'", format);
    }
}
