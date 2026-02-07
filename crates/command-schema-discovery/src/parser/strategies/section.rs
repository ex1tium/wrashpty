//! Section-based parser strategy.

use super::ParserStrategy;
use crate::parser::ast::{ArgCandidate, FlagCandidate, SourceSpan, SubcommandCandidate};
use crate::parser::{HelpParser, IndexedLine};

pub struct SectionStrategy;

impl ParserStrategy for SectionStrategy {
    fn name(&self) -> &'static str {
        "section"
    }

    fn parse_flags(&self, parser: &HelpParser, lines: &[IndexedLine]) -> Vec<FlagCandidate> {
        let sections = parser.identify_sections(lines);
        let mut out = Vec::new();

        if !sections.flags.is_empty() {
            let refs = sections
                .flags
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>();
            let parsed = parser.parse_flags(&refs);
            for (idx, flag) in parsed.into_iter().enumerate() {
                let span = sections
                    .flags
                    .get(idx)
                    .map(|entry| SourceSpan::single(entry.index))
                    .unwrap_or_else(SourceSpan::unknown);
                out.push(FlagCandidate::from_schema(flag, span, "section-flags", 0.9));
            }
        }

        if !sections.options.is_empty() {
            let refs = sections
                .options
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>();
            let parsed = parser.parse_flags(&refs);
            for (idx, flag) in parsed.into_iter().enumerate() {
                let span = sections
                    .options
                    .get(idx)
                    .map(|entry| SourceSpan::single(entry.index))
                    .unwrap_or_else(SourceSpan::unknown);
                out.push(FlagCandidate::from_schema(
                    flag,
                    span,
                    "section-options",
                    0.88,
                ));
            }
        }

        out
    }

    fn parse_subcommands(
        &self,
        parser: &HelpParser,
        lines: &[IndexedLine],
    ) -> Vec<SubcommandCandidate> {
        let sections = parser.identify_sections(lines);
        let mut out = Vec::new();

        if !sections.subcommands.is_empty() {
            let refs = sections
                .subcommands
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>();
            let parsed = parser.parse_subcommands(&refs);
            for (idx, sub) in parsed.into_iter().enumerate() {
                let span = sections
                    .subcommands
                    .get(idx)
                    .map(|entry| SourceSpan::single(entry.index))
                    .unwrap_or_else(SourceSpan::unknown);
                out.push(SubcommandCandidate::from_schema(
                    sub,
                    span,
                    "section-subcommands",
                    0.9,
                ));
            }
            return out;
        }

        if !HelpParser::looks_like_keybinding_document(lines) {
            let (parsed, recognized) = parser.parse_two_column_subcommands(lines);
            let spans = recognized
                .into_iter()
                .map(SourceSpan::single)
                .collect::<Vec<_>>();
            for (idx, sub) in parsed.into_iter().enumerate() {
                let span = spans.get(idx).copied().unwrap_or_else(SourceSpan::unknown);
                out.push(SubcommandCandidate::from_schema(
                    sub,
                    span,
                    "generic-two-column-subcommands",
                    0.8,
                ));
            }
        }

        let (named, recognized) = parser.parse_named_setting_rows(lines);
        let spans = recognized
            .into_iter()
            .map(SourceSpan::single)
            .collect::<Vec<_>>();
        for (idx, sub) in named.into_iter().enumerate() {
            let span = spans.get(idx).copied().unwrap_or_else(SourceSpan::unknown);
            out.push(SubcommandCandidate::from_schema(
                sub,
                span,
                "named-setting-rows",
                0.72,
            ));
        }

        out
    }

    fn parse_args(&self, parser: &HelpParser, lines: &[IndexedLine]) -> Vec<ArgCandidate> {
        let sections = parser.identify_sections(lines);
        let mut out = Vec::new();
        if sections.arguments.is_empty() {
            return out;
        }

        let refs = sections
            .arguments
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>();
        let parsed = parser.parse_arguments_section(&refs);
        for (idx, arg) in parsed.into_iter().enumerate() {
            let span = sections
                .arguments
                .get(idx)
                .map(|entry| SourceSpan::single(entry.index))
                .unwrap_or_else(SourceSpan::unknown);
            out.push(ArgCandidate::from_schema(
                arg,
                span,
                "section-arguments",
                0.82,
            ));
        }

        out
    }
}
