//! Help-text normalization utilities.

use regex::Regex;
use std::sync::LazyLock;

use super::{HelpParser, IndexedLine};

pub fn normalize_help_output(raw: &str) -> String {
    static ANSI_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap());
    static OVERSTRIKE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r".\x08").unwrap());

    let stripped = ANSI_RE.replace_all(raw, "");
    let mut cleaned = stripped.into_owned();
    while OVERSTRIKE_RE.is_match(&cleaned) {
        cleaned = OVERSTRIKE_RE.replace_all(&cleaned, "").into_owned();
    }
    let replaced = cleaned.replace("\r\n", "\n").replace('\r', "\n");

    let mut normalized: Vec<String> = Vec::new();
    for line in replaced.lines() {
        let trimmed_end = line.trim_end();
        let trimmed_start = trimmed_end.trim_start();

        if trimmed_end.is_empty() {
            normalized.push(String::new());
            continue;
        }

        let is_wrapped_continuation = line.starts_with(' ')
            && !trimmed_start.ends_with(':')
            && normalized.last().is_some_and(|prev| {
                let prev_trimmed = prev.trim();
                let prev_is_flag = HelpParser::looks_like_flag_row_start(prev_trimmed);
                let prev_is_two_column_subcommand = HelpParser::split_two_columns(prev_trimmed)
                    .is_some_and(|(left, _)| {
                        !left.starts_with('-')
                            && left
                                .chars()
                                .next()
                                .is_some_and(|ch| ch.is_ascii_alphanumeric())
                    });
                let starts_new_flag_row = HelpParser::looks_like_flag_row_start(trimmed_start)
                    && !trimmed_start.contains(';');
                let looks_like_subcommand = HelpParser::looks_like_subcommand_entry(trimmed_start);
                (prev_is_flag || prev_is_two_column_subcommand)
                    && !prev_trimmed.is_empty()
                    && (!prev_trimmed.ends_with(':') || prev_is_flag)
                    && (!looks_like_subcommand || prev_is_flag)
                    && !starts_new_flag_row
            });

        if is_wrapped_continuation {
            if let Some(prev) = normalized.last_mut() {
                prev.push(' ');
                prev.push_str(trimmed_start);
            }
            continue;
        }

        normalized.push(trimmed_end.to_string());
    }

    normalized.join("\n")
}

pub fn to_indexed_lines(normalized: &str) -> Vec<IndexedLine> {
    normalized
        .lines()
        .enumerate()
        .map(|(index, text)| IndexedLine {
            index,
            text: text.to_string(),
        })
        .collect()
}
