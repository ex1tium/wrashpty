//! Diagnostics for parser candidate gating and discard policies.

use super::ast::{ArgCandidate, FlagCandidate, SubcommandCandidate};

#[derive(Debug, Clone, Default)]
pub struct CandidateDiagnostics {
    pub medium_flags: Vec<FlagCandidate>,
    pub discarded_flags: Vec<FlagCandidate>,
    pub medium_subcommands: Vec<SubcommandCandidate>,
    pub discarded_subcommands: Vec<SubcommandCandidate>,
    pub medium_args: Vec<ArgCandidate>,
    pub discarded_args: Vec<ArgCandidate>,
    pub false_positive_filter_hits: usize,
}

impl CandidateDiagnostics {
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if !self.medium_flags.is_empty()
            || !self.medium_subcommands.is_empty()
            || !self.medium_args.is_empty()
        {
            warnings.push(format!(
                "Medium-confidence findings kept in diagnostics: {} flags, {} subcommands, {} args",
                self.medium_flags.len(),
                self.medium_subcommands.len(),
                self.medium_args.len()
            ));
        }

        if !self.discarded_flags.is_empty()
            || !self.discarded_subcommands.is_empty()
            || !self.discarded_args.is_empty()
        {
            warnings.push(format!(
                "Discarded low-confidence findings: {} flags, {} subcommands, {} args",
                self.discarded_flags.len(),
                self.discarded_subcommands.len(),
                self.discarded_args.len()
            ));
        }

        if self.false_positive_filter_hits > 0 {
            warnings.push(format!(
                "False-positive filters matched {} rows",
                self.false_positive_filter_hits
            ));
        }

        warnings
    }
}
