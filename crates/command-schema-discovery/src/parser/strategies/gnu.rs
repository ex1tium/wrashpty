//! GNU sectionless-flag parser strategy.

use super::ParserStrategy;
use crate::parser::ast::{ArgCandidate, FlagCandidate, SourceSpan, SubcommandCandidate};
use crate::parser::{HelpParser, IndexedLine};

pub struct GnuStrategy;

impl ParserStrategy for GnuStrategy {
    fn name(&self) -> &'static str {
        "gnu"
    }

    fn parse_flags(&self, parser: &HelpParser, lines: &[IndexedLine]) -> Vec<FlagCandidate> {
        let (parsed, recognized) = parser.parse_sectionless_flags(lines);
        let spans = recognized
            .into_iter()
            .map(SourceSpan::single)
            .collect::<Vec<_>>();

        parsed
            .into_iter()
            .enumerate()
            .map(|(idx, flag)| {
                let span = spans.get(idx).copied().unwrap_or_else(SourceSpan::unknown);
                FlagCandidate::from_schema(flag, span, "gnu-sectionless-flags", 0.7)
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

    fn parse_args(&self, _parser: &HelpParser, _lines: &[IndexedLine]) -> Vec<ArgCandidate> {
        Vec::new()
    }
}
