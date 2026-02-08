//! NPM-style command-list parser strategy.

use super::ParserStrategy;
use crate::parser::ast::{ArgCandidate, FlagCandidate, SourceSpan, SubcommandCandidate};
use crate::parser::{HelpParser, IndexedLine};

pub struct NpmStrategy;

impl ParserStrategy for NpmStrategy {
    fn name(&self) -> &'static str {
        "npm"
    }

    fn parse_flags(&self, _parser: &HelpParser, _lines: &[IndexedLine]) -> Vec<FlagCandidate> {
        Vec::new()
    }

    fn parse_subcommands(
        &self,
        parser: &HelpParser,
        lines: &[IndexedLine],
    ) -> Vec<SubcommandCandidate> {
        let (parsed, recognized) = parser.parse_npm_style_commands(lines);
        let spans = recognized
            .into_iter()
            .map(SourceSpan::single)
            .collect::<Vec<_>>();

        parsed
            .into_iter()
            .enumerate()
            .map(|(idx, sub)| {
                let span = spans.get(idx).copied().unwrap_or_else(SourceSpan::unknown);
                SubcommandCandidate::from_schema(sub, span, "npm-command-list", 0.85)
            })
            .collect()
    }

    fn parse_args(&self, _parser: &HelpParser, _lines: &[IndexedLine]) -> Vec<ArgCandidate> {
        Vec::new()
    }
}
