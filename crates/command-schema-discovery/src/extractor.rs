//! Command schema extraction via help probing.
//!
//! Automatically extracts command schemas by running --help commands
//! and recursively probing subcommands.

use std::collections::HashSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::{debug, info};
use wait_timeout::ChildExt;

use super::parser::HelpParser;
use command_schema_core::{ExtractionResult, SubcommandSchema};

/// Maximum depth for recursive subcommand probing.
const MAX_PROBE_DEPTH: usize = 3;

/// Timeout for help commands (milliseconds).
const HELP_TIMEOUT_MS: u64 = 5000;

/// Help flags to try in order.
const HELP_FLAGS: &[&str] = &["--help", "-h", "help", "-?"];

/// Probes a command's help output and returns the raw text.
pub fn probe_command_help(command: &str) -> Option<String> {
    // Split command into parts (e.g., "git remote" -> ["git", "remote"])
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    for help_flag in HELP_FLAGS {
        let mut cmd_parts = parts.clone();

        // Special case: "help" subcommand goes after the command
        if *help_flag == "help" && parts.len() > 1 {
            // For "git remote", try "git help remote"
            // Insert "help" after the base command, keeping subcommand(s) intact
            cmd_parts.insert(1, "help");
        } else {
            cmd_parts.push(help_flag);
        }

        debug!(command = ?cmd_parts, "Probing help");

        let spawn_result = Command::new(cmd_parts[0])
            .args(&cmd_parts[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        match spawn_result {
            Ok(mut child) => {
                // Take ownership of pipes before wait_timeout() reaps the child
                let mut stdout_pipe = child.stdout.take();
                let mut stderr_pipe = child.stderr.take();

                let timeout = Duration::from_millis(HELP_TIMEOUT_MS);
                match child.wait_timeout(timeout) {
                    Ok(Some(_status)) => {
                        // Process completed within timeout, read from pipes directly
                        let mut stdout_buf = Vec::new();
                        let mut stderr_buf = Vec::new();

                        if let Some(ref mut pipe) = stdout_pipe {
                            if let Err(e) = pipe.read_to_end(&mut stdout_buf) {
                                debug!(command = ?cmd_parts, error = %e, "Failed to read stdout");
                            }
                        }
                        if let Some(ref mut pipe) = stderr_pipe {
                            if let Err(e) = pipe.read_to_end(&mut stderr_buf) {
                                debug!(command = ?cmd_parts, error = %e, "Failed to read stderr");
                            }
                        }

                        // Some commands output help to stderr
                        let stdout = String::from_utf8_lossy(&stdout_buf);
                        let stderr = String::from_utf8_lossy(&stderr_buf);

                        let help_text = if stdout.len() > stderr.len() {
                            stdout.to_string()
                        } else {
                            stderr.to_string()
                        };

                        // Validate it looks like help output
                        if is_help_output(&help_text) {
                            debug!(
                                command = command,
                                help_flag = help_flag,
                                length = help_text.len(),
                                "Got help output"
                            );
                            return Some(help_text);
                        }
                    }
                    Ok(None) => {
                        // Timeout expired, kill the process
                        debug!(
                            command = ?cmd_parts,
                            timeout_ms = HELP_TIMEOUT_MS,
                            "Help command timed out, killing process"
                        );
                        let _ = child.kill();
                        let _ = child.wait(); // Reap the zombie process
                    }
                    Err(e) => {
                        debug!(command = ?cmd_parts, error = %e, "Failed to wait on help command");
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                }
            }
            Err(e) => {
                debug!(command = ?cmd_parts, error = %e, "Failed to spawn help command");
            }
        }
    }

    None
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
    let mut warnings = Vec::new();

    // Get help output
    let raw_output = match probe_command_help(command) {
        Some(output) => output,
        None => {
            return ExtractionResult {
                schema: None,
                raw_output: String::new(),
                detected_format: None,
                warnings: vec![format!("Could not get help output for '{}'", command)],
                success: false,
            };
        }
    };

    // Parse the help output
    let mut parser = HelpParser::new(command, &raw_output);
    let mut schema = match parser.parse() {
        Some(s) => s,
        None => {
            return ExtractionResult {
                schema: None,
                raw_output,
                detected_format: parser.detected_format(),
                warnings: parser.warnings().to_vec(),
                success: false,
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

    ExtractionResult {
        schema: Some(schema),
        raw_output,
        detected_format: parser.detected_format(),
        warnings,
        success: true,
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
