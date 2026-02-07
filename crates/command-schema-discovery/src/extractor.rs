//! Command schema extraction via help probing.
//!
//! Automatically extracts command schemas by running --help commands
//! and recursively probing subcommands.

use std::collections::HashSet;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tracing::{debug, info};

use super::parser::{FormatScore, HelpParser};
use super::report::{ExtractionReport, FormatScoreReport, ProbeAttemptReport};
use command_schema_core::{ExtractionResult, HelpFormat, SubcommandSchema, validate_schema};

/// Maximum depth for recursive subcommand probing.
const MAX_PROBE_DEPTH: usize = 3;

/// Timeout for help commands (milliseconds).
const HELP_TIMEOUT_MS: u64 = 5000;

/// Help flags to try in order.
const HELP_FLAGS: &[&str] = &["--help", "-h", "help", "-?"];

/// Extraction output with both schema result and diagnostics report.
pub struct ExtractionRun {
    pub result: ExtractionResult,
    pub report: ExtractionReport,
}

#[derive(Debug, Clone)]
struct ProbeAttempt {
    help_flag: String,
    argv: Vec<String>,
    exit_code: Option<i32>,
    timed_out: bool,
    error: Option<String>,
    output_source: Option<String>,
    output_len: usize,
    accepted: bool,
}

impl ProbeAttempt {
    fn new(help_flag: &str, argv: Vec<String>) -> Self {
        Self {
            help_flag: help_flag.to_string(),
            argv,
            exit_code: None,
            timed_out: false,
            error: None,
            output_source: None,
            output_len: 0,
            accepted: false,
        }
    }
}

#[derive(Debug, Clone)]
struct ProbeRun {
    help_output: Option<String>,
    attempts: Vec<ProbeAttempt>,
}

/// Probes a command's help output and returns the raw text.
pub fn probe_command_help(command: &str) -> Option<String> {
    probe_command_help_with_metadata(command).help_output
}

fn probe_command_help_with_metadata(command: &str) -> ProbeRun {
    // Split command into parts (e.g., "git remote" -> ["git", "remote"])
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return ProbeRun {
            help_output: None,
            attempts: Vec::new(),
        };
    }

    let mut attempts = Vec::with_capacity(HELP_FLAGS.len());

    for help_flag in HELP_FLAGS {
        let mut cmd_parts: Vec<String> = parts.iter().map(|part| (*part).to_string()).collect();

        // Special case: "help" subcommand goes after the command
        if *help_flag == "help" && parts.len() > 1 {
            // For "git remote", try "git help remote"
            // Insert "help" after the base command, keeping subcommand(s) intact
            cmd_parts.insert(1, "help".to_string());
        } else {
            cmd_parts.push((*help_flag).to_string());
        }

        debug!(command = ?cmd_parts, "Probing help");
        let mut attempt = ProbeAttempt::new(help_flag, cmd_parts.clone());

        let spawn_result = Command::new(&cmd_parts[0])
            .args(&cmd_parts[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        match spawn_result {
            Ok(mut child) => {
                // Take ownership of pipes before waiting so we can read both streams.
                let mut stdout_pipe = child.stdout.take();
                let mut stderr_pipe = child.stderr.take();

                let timeout = Duration::from_millis(HELP_TIMEOUT_MS);
                match wait_for_child_with_timeout(&mut child, timeout) {
                    Ok(Some(status)) => {
                        attempt.exit_code = status.code();
                        // Process completed within timeout, read from pipes directly
                        let mut stdout_buf = Vec::new();
                        let mut stderr_buf = Vec::new();
                        let mut io_errors = Vec::new();

                        if let Some(ref mut pipe) = stdout_pipe {
                            if let Err(e) = pipe.read_to_end(&mut stdout_buf) {
                                debug!(command = ?cmd_parts, error = %e, "Failed to read stdout");
                                io_errors.push(format!("stdout read failed: {e}"));
                            }
                        }
                        if let Some(ref mut pipe) = stderr_pipe {
                            if let Err(e) = pipe.read_to_end(&mut stderr_buf) {
                                debug!(command = ?cmd_parts, error = %e, "Failed to read stderr");
                                io_errors.push(format!("stderr read failed: {e}"));
                            }
                        }
                        if !io_errors.is_empty() {
                            attempt.error = Some(io_errors.join("; "));
                        }

                        // Some commands output help to stderr
                        let stdout = String::from_utf8_lossy(&stdout_buf);
                        let stderr = String::from_utf8_lossy(&stderr_buf);
                        let (help_text, output_source) = if stdout.len() > stderr.len() {
                            (stdout.to_string(), "stdout")
                        } else {
                            (stderr.to_string(), "stderr")
                        };
                        attempt.output_source = Some(output_source.to_string());
                        attempt.output_len = help_text.len();

                        // Validate it looks like help output
                        if is_help_output(&help_text) {
                            attempt.accepted = true;
                            attempts.push(attempt);
                            debug!(
                                command = command,
                                help_flag = help_flag,
                                length = help_text.len(),
                                "Got help output"
                            );
                            return ProbeRun {
                                help_output: Some(help_text),
                                attempts,
                            };
                        }
                        attempts.push(attempt);
                    }
                    Ok(None) => {
                        // Timeout expired, kill the process
                        attempt.timed_out = true;
                        debug!(
                            command = ?cmd_parts,
                            timeout_ms = HELP_TIMEOUT_MS,
                            "Help command timed out, killing process"
                        );
                        let _ = child.kill();
                        let _ = child.wait(); // Reap the zombie process
                        attempts.push(attempt);
                    }
                    Err(e) => {
                        attempt.error = Some(format!("wait failed: {e}"));
                        debug!(command = ?cmd_parts, error = %e, "Failed to wait on help command");
                        let _ = child.kill();
                        let _ = child.wait();
                        attempts.push(attempt);
                    }
                }
            }
            Err(e) => {
                attempt.error = Some(format!("spawn failed: {e}"));
                debug!(command = ?cmd_parts, error = %e, "Failed to spawn help command");
                attempts.push(attempt);
            }
        }
    }

    ProbeRun {
        help_output: None,
        attempts,
    }
}

fn wait_for_child_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if start.elapsed() >= timeout {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Checks if text looks like help output.
fn is_help_output(text: &str) -> bool {
    let text_lower = text.to_lowercase();

    // Must have some minimum content
    if text.len() < 40 {
        return false;
    }

    // Should contain help-like keywords
    let help_indicators = [
        "usage",
        "options",
        "commands",
        "flags",
        "--help",
        "-h",
        "arguments",
        "description",
    ];

    help_indicators.iter().any(|&ind| text_lower.contains(ind))
}

/// Extracts a complete command schema including subcommands.
pub fn extract_command_schema(command: &str) -> ExtractionResult {
    extract_command_schema_with_report(command).result
}

/// Extracts a complete command schema including subcommands and a diagnostics report.
pub fn extract_command_schema_with_report(command: &str) -> ExtractionRun {
    let mut warnings = Vec::new();
    let probe_run = probe_command_help_with_metadata(command);
    let probe_attempts = to_probe_attempt_reports(&probe_run.attempts);

    // Get help output
    let raw_output = match probe_run.help_output {
        Some(output) => output,
        None => {
            let failure_warning = format!("Could not get help output for '{}'", command);
            return ExtractionRun {
                result: ExtractionResult {
                    schema: None,
                    raw_output: String::new(),
                    detected_format: None,
                    warnings: vec![failure_warning.clone()],
                    success: false,
                },
                report: ExtractionReport {
                    command: command.to_string(),
                    success: false,
                    selected_format: None,
                    format_scores: Vec::new(),
                    parsers_used: vec!["probe-failed".to_string()],
                    confidence: 0.0,
                    coverage: 0.0,
                    relevant_lines: 0,
                    recognized_lines: 0,
                    unresolved_lines: Vec::new(),
                    probe_attempts,
                    warnings: vec![failure_warning],
                    validation_errors: Vec::new(),
                },
            };
        }
    };

    // Parse the help output
    let mut parser = HelpParser::new(command, &raw_output);
    let mut schema = match parser.parse() {
        Some(s) => s,
        None => {
            let diagnostics = parser.diagnostics().clone();
            let parser_warnings = parser.warnings().to_vec();
            let coverage = diagnostics.coverage();
            return ExtractionRun {
                result: ExtractionResult {
                    schema: None,
                    raw_output,
                    detected_format: parser.detected_format(),
                    warnings: parser_warnings.clone(),
                    success: false,
                },
                report: ExtractionReport {
                    command: command.to_string(),
                    success: false,
                    selected_format: parser.detected_format().map(help_format_label),
                    format_scores: to_format_score_reports(&diagnostics.format_scores),
                    parsers_used: diagnostics.parsers_used,
                    confidence: 0.0,
                    coverage,
                    relevant_lines: diagnostics.relevant_lines,
                    recognized_lines: diagnostics.recognized_lines,
                    unresolved_lines: diagnostics.unresolved_lines,
                    probe_attempts,
                    warnings: parser_warnings,
                    validation_errors: Vec::new(),
                },
            };
        }
    };

    warnings.extend(parser.warnings().iter().cloned());

    // Recursively probe subcommands
    let mut probed_subcommands = HashSet::new();
    probe_subcommands_recursive(
        command,
        &mut schema.subcommands,
        &mut probed_subcommands,
        1,
        &mut warnings,
    );

    info!(
        command = command,
        subcommands = schema.subcommands.len(),
        flags = schema.global_flags.len(),
        confidence = schema.confidence,
        "Extracted command schema"
    );

    let diagnostics = parser.diagnostics().clone();
    let validation_errors = validate_schema(&schema)
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    let success = validation_errors.is_empty();

    let report = ExtractionReport {
        command: command.to_string(),
        success,
        selected_format: parser.detected_format().map(help_format_label),
        format_scores: to_format_score_reports(&diagnostics.format_scores),
        parsers_used: diagnostics.parsers_used.clone(),
        confidence: schema.confidence,
        coverage: diagnostics.coverage(),
        relevant_lines: diagnostics.relevant_lines,
        recognized_lines: diagnostics.recognized_lines,
        unresolved_lines: diagnostics.unresolved_lines,
        probe_attempts,
        warnings: warnings.clone(),
        validation_errors: validation_errors.clone(),
    };

    ExtractionRun {
        result: ExtractionResult {
            schema: if success { Some(schema) } else { None },
            raw_output,
            detected_format: parser.detected_format(),
            warnings,
            success,
        },
        report,
    }
}

/// Recursively probes subcommands to get their schemas.
fn probe_subcommands_recursive(
    base_command: &str,
    subcommands: &mut [SubcommandSchema],
    probed: &mut HashSet<String>,
    depth: usize,
    warnings: &mut Vec<String>,
) {
    if depth > MAX_PROBE_DEPTH {
        return;
    }

    for subcmd in subcommands.iter_mut() {
        let full_command = format!("{} {}", base_command, subcmd.name);

        // Skip if already probed (avoid cycles)
        if probed.contains(&full_command) {
            continue;
        }
        probed.insert(full_command.clone());

        // Skip common non-probeable subcommands
        if should_skip_subcommand(&subcmd.name) {
            continue;
        }

        debug!(
            command = %full_command,
            depth = depth,
            "Probing subcommand"
        );

        // Get help for this subcommand
        if let Some(help_output) = probe_command_help(&full_command) {
            let mut parser = HelpParser::new(&full_command, &help_output);
            if let Some(sub_schema) = parser.parse() {
                // Merge extracted info into subcommand
                subcmd.flags = sub_schema.global_flags;
                subcmd.positional = sub_schema.positional;
                subcmd.description = sub_schema.description.or(subcmd.description.take());

                // Add nested subcommands
                subcmd.subcommands = sub_schema.subcommands;

                // Recurse into nested subcommands
                if !subcmd.subcommands.is_empty() {
                    probe_subcommands_recursive(
                        &full_command,
                        &mut subcmd.subcommands,
                        probed,
                        depth + 1,
                        warnings,
                    );
                }
            }
            warnings.extend(parser.warnings().iter().cloned());
        }
    }
}

/// Determines if a subcommand should be skipped during probing.
fn should_skip_subcommand(name: &str) -> bool {
    // Skip help-related subcommands (they don't have meaningful help of their own)
    let skip_list = ["help", "version", "completion", "completions"];
    skip_list.contains(&name)
}

/// Extracts schemas for multiple commands.
pub fn extract_multiple_schemas(commands: &[&str]) -> Vec<ExtractionResult> {
    commands
        .iter()
        .map(|cmd| extract_command_schema(cmd))
        .collect()
}

fn to_format_score_reports(scores: &[FormatScore]) -> Vec<FormatScoreReport> {
    scores
        .iter()
        .map(|entry| FormatScoreReport {
            format: help_format_label(entry.format),
            score: entry.score,
        })
        .collect()
}

fn to_probe_attempt_reports(attempts: &[ProbeAttempt]) -> Vec<ProbeAttemptReport> {
    attempts
        .iter()
        .map(|attempt| ProbeAttemptReport {
            help_flag: attempt.help_flag.clone(),
            argv: attempt.argv.clone(),
            exit_code: attempt.exit_code,
            timed_out: attempt.timed_out,
            error: attempt.error.clone(),
            output_source: attempt.output_source.clone(),
            output_len: attempt.output_len,
            accepted: attempt.accepted,
        })
        .collect()
}

fn help_format_label(format: HelpFormat) -> String {
    match format {
        HelpFormat::Clap => "clap",
        HelpFormat::Cobra => "cobra",
        HelpFormat::Argparse => "argparse",
        HelpFormat::Docopt => "docopt",
        HelpFormat::Gnu => "gnu",
        HelpFormat::Bsd => "bsd",
        HelpFormat::Unknown => "unknown",
    }
    .to_string()
}

/// Probes a command to check if it exists and has help.
pub fn command_exists(command: &str) -> bool {
    Command::new("which")
        .arg(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_help_output() {
        assert!(is_help_output(
            "Usage: mycommand [options]\n\nOptions:\n  --help"
        ));
        assert!(is_help_output(
            "USAGE:\n    myapp [FLAGS]\n\nFLAGS:\n    -h, --help"
        ));
        assert!(!is_help_output("error: command not found"));
        assert!(!is_help_output("short"));
    }

    #[test]
    fn test_should_skip_subcommand() {
        assert!(should_skip_subcommand("help"));
        assert!(should_skip_subcommand("version"));
        assert!(should_skip_subcommand("completion"));
        assert!(!should_skip_subcommand("build"));
        assert!(!should_skip_subcommand("run"));
    }

    #[test]
    fn test_extract_report_contains_probe_attempt_metadata_on_probe_failure() {
        let run = extract_command_schema_with_report("__wrashpty_missing_command__");
        assert!(!run.result.success);
        assert_eq!(run.report.probe_attempts.len(), HELP_FLAGS.len());

        for (index, attempt) in run.report.probe_attempts.iter().enumerate() {
            assert_eq!(attempt.help_flag, HELP_FLAGS[index]);
            assert!(attempt.error.is_some() || attempt.timed_out || attempt.exit_code.is_some());
        }
    }

    // Integration tests - only run if commands are available
    #[test]
    #[ignore] // Run with: cargo test -- --ignored
    fn test_extract_git_schema() {
        if !command_exists("git") {
            return;
        }

        let result = extract_command_schema("git");
        assert!(result.success);

        let schema = result.schema.unwrap();
        assert!(!schema.subcommands.is_empty());

        // Should have common git subcommands
        assert!(schema.find_subcommand("commit").is_some());
        assert!(schema.find_subcommand("push").is_some());
    }

    #[test]
    #[ignore]
    fn test_extract_cargo_schema() {
        if !command_exists("cargo") {
            return;
        }

        let result = extract_command_schema("cargo");
        assert!(result.success);

        let schema = result.schema.unwrap();
        assert!(schema.find_subcommand("build").is_some());
        assert!(schema.find_subcommand("test").is_some());
    }

    #[test]
    #[ignore]
    fn test_probe_git_commit_help() {
        if !command_exists("git") {
            return;
        }

        let help = probe_command_help("git commit");
        assert!(help.is_some());

        let help_text = help.unwrap();
        assert!(help_text.contains("-m") || help_text.contains("--message"));
    }
}
