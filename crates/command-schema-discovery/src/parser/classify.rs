//! Format classification with weighted scoring.
//!
//! Detects help-output formats and applies hard-negative filtering helpers
//! used to suppress false positives.

use command_schema_core::HelpFormat;

use super::{FormatScore, IndexedLine};

pub fn classify_formats(lines: &[&str]) -> Vec<FormatScore> {
    let mut scores = vec![
        FormatScore {
            format: HelpFormat::Clap,
            score: 0.0,
        },
        FormatScore {
            format: HelpFormat::Cobra,
            score: 0.0,
        },
        FormatScore {
            format: HelpFormat::Gnu,
            score: 0.0,
        },
        FormatScore {
            format: HelpFormat::Argparse,
            score: 0.0,
        },
        FormatScore {
            format: HelpFormat::Docopt,
            score: 0.0,
        },
        FormatScore {
            format: HelpFormat::Bsd,
            score: 0.0,
        },
        FormatScore {
            format: HelpFormat::Unknown,
            score: 0.05,
        },
    ];

    let output = lines.join("\n");
    for score in &mut scores {
        score.score += match score.format {
            HelpFormat::Clap => {
                let mut s = 0.0;
                if output.contains("USAGE:") {
                    s += 0.35;
                }
                if output.contains("FLAGS:") {
                    s += 0.25;
                }
                if output.contains("OPTIONS:") {
                    s += 0.2;
                }
                if output.contains("SUBCOMMANDS:") || output.contains("Commands:") {
                    s += 0.2;
                }
                s
            }
            HelpFormat::Cobra => {
                let mut s = 0.0;
                if output.contains("Available Commands:") {
                    s += 0.5;
                }
                if output.contains("Use \"") && output.contains("--help") {
                    s += 0.35;
                }
                if output.contains("Flags:") {
                    s += 0.15;
                }
                s
            }
            HelpFormat::Gnu => {
                let mut s = 0.0;
                if output.contains("Usage:") {
                    s += 0.25;
                }
                if output.contains("--help") {
                    s += 0.2;
                }
                if output.contains("--version") {
                    s += 0.2;
                }
                if lines.iter().any(|line| line.trim_start().starts_with('-')) {
                    s += 0.2;
                }
                s
            }
            HelpFormat::Argparse => {
                let mut s = 0.0;
                if output.contains("positional arguments:") {
                    s += 0.45;
                }
                if output.contains("optional arguments:") {
                    s += 0.45;
                }
                s
            }
            HelpFormat::Docopt => {
                if output.starts_with("Usage:") {
                    0.75
                } else {
                    0.0
                }
            }
            HelpFormat::Bsd => {
                if output.contains("SYNOPSIS") || output.contains("DESCRIPTION") {
                    0.45
                } else {
                    0.0
                }
            }
            HelpFormat::Unknown => 0.0,
        };
    }

    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scores
}

pub fn is_placeholder_token(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "COMMAND"
            | "FILE"
            | "PATH"
            | "URL"
            | "ARG"
            | "OPTION"
            | "SUBCOMMAND"
            | "CMD"
            | "ARGS"
            | "OPTIONS"
    )
}

pub fn is_env_var_row(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("export ") {
        return true;
    }

    let Some((left, _)) = trimmed.split_once('=') else {
        return false;
    };

    let key = left.trim();
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

pub fn is_keybinding_row(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.contains("Ctrl+") || trimmed.contains("ctrl+") || trimmed.contains('^') {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    lower.contains("esc-")
        || lower.contains("arrow")
        || lower.contains("backspace")
        || lower.contains("delete")
}

pub fn is_prose_header(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "name  description"
            | "name description"
            | "command  description"
            | "command description"
            | "option  description"
            | "option description"
    )
}

pub fn count_filter_hits(lines: &[IndexedLine]) -> usize {
    lines
        .iter()
        .filter(|line| {
            is_env_var_row(line.text.as_str())
                || is_keybinding_row(line.text.as_str())
                || is_prose_header(line.text.as_str())
        })
        .count()
}
