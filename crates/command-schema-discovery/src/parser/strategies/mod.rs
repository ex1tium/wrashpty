//! Pluggable parser strategies for different help-output structures.

pub mod gnu;
pub mod npm;
pub mod section;
pub mod usage;

use super::ast::{ArgCandidate, FlagCandidate, SubcommandCandidate};
use super::{FormatScore, HelpParser, IndexedLine};

pub trait ParserStrategy {
    fn name(&self) -> &'static str;
    fn parse_flags(&self, parser: &HelpParser, lines: &[IndexedLine]) -> Vec<FlagCandidate>;
    fn parse_subcommands(
        &self,
        parser: &HelpParser,
        lines: &[IndexedLine],
    ) -> Vec<SubcommandCandidate>;
    fn parse_args(&self, parser: &HelpParser, lines: &[IndexedLine]) -> Vec<ArgCandidate>;
}

pub fn ranked_strategy_names(format_scores: &[FormatScore]) -> Vec<&'static str> {
    let mut names = vec!["section"]; // always run explicit sections first

    if format_scores
        .first()
        .is_some_and(|score| score.format_label() == "cobra")
    {
        names.push("npm");
    }

    names.push("gnu");
    names.push("usage");
    names
}

trait FormatScoreExt {
    fn format_label(&self) -> &'static str;
}

impl FormatScoreExt for FormatScore {
    fn format_label(&self) -> &'static str {
        match self.format {
            command_schema_core::HelpFormat::Clap => "clap",
            command_schema_core::HelpFormat::Cobra => "cobra",
            command_schema_core::HelpFormat::Argparse => "argparse",
            command_schema_core::HelpFormat::Docopt => "docopt",
            command_schema_core::HelpFormat::Gnu => "gnu",
            command_schema_core::HelpFormat::Bsd => "bsd",
            command_schema_core::HelpFormat::Unknown => "unknown",
        }
    }
}
