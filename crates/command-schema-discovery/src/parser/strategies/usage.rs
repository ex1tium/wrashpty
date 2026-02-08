//! Usage-line parser strategy.

use super::ParserStrategy;
use crate::parser::ast::{ArgCandidate, FlagCandidate, SourceSpan, SubcommandCandidate};
use crate::parser::{HelpParser, IndexedLine};

pub struct UsageStrategy;

impl ParserStrategy for UsageStrategy {
    fn name(&self) -> &'static str {
        "usage"
    }

    fn parse_flags(&self, parser: &HelpParser, lines: &[IndexedLine]) -> Vec<FlagCandidate> {
        let (parsed, recognized) = parser.parse_usage_compact_flags(lines);
        let spans = recognized
            .into_iter()
            .map(SourceSpan::single)
            .collect::<Vec<_>>();

        parsed
            .into_iter()
            .enumerate()
            .map(|(idx, flag)| {
                let span = spans.get(idx).copied().unwrap_or_else(SourceSpan::unknown);
                FlagCandidate::from_schema(flag, span, "usage-compact-flags", 0.75)
            })
            .collect()
    }

    fn parse_subcommands(
        &self,
        _parser: &HelpParser,
        _lines: &[IndexedLine],
    ) -> Vec<SubcommandCandidate> {
        Vec::new()
    }

    fn parse_args(&self, parser: &HelpParser, lines: &[IndexedLine]) -> Vec<ArgCandidate> {
        let (parsed, recognized) = parser.parse_usage_positionals(lines, false);
        let spans = recognized
            .into_iter()
            .map(SourceSpan::single)
            .collect::<Vec<_>>();

        parsed
            .into_iter()
            .enumerate()
            .map(|(idx, arg)| {
                let span = spans.get(idx).copied().unwrap_or_else(SourceSpan::unknown);
                ArgCandidate::from_schema(arg, span, "usage-positionals", 0.72)
            })
            .collect()
    }
}
