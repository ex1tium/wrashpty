//! Candidate merging and deterministic schema finalization.

use std::collections::HashMap;

use command_schema_core::{ArgSchema, CommandSchema, FlagSchema, SubcommandSchema};

use super::ast::{ArgCandidate, FlagCandidate, SubcommandCandidate};
use super::confidence::{score_arg_candidate, score_flag_candidate, score_subcommand_candidate};

pub use super::confidence::{HIGH_CONFIDENCE_THRESHOLD, MEDIUM_CONFIDENCE_THRESHOLD};

#[derive(Debug, Clone)]
pub struct GateResult<T, C> {
    pub accepted: Vec<T>,
    pub medium_confidence: Vec<C>,
    pub discarded: Vec<C>,
}

fn choose_best_candidate<C, F>(candidates: Vec<C>, mut score_fn: F) -> (Option<C>, Vec<C>, Vec<C>)
where
    C: Clone,
    F: FnMut(&C) -> f64,
{
    let mut best: Option<C> = None;
    let mut best_score = -1.0;
    let mut medium = Vec::new();
    let mut discarded = Vec::new();

    for candidate in candidates {
        let score = score_fn(&candidate);
        if score >= HIGH_CONFIDENCE_THRESHOLD {
            if score > best_score {
                if let Some(prev) = best.take() {
                    medium.push(prev);
                }
                best = Some(candidate);
                best_score = score;
            } else {
                medium.push(candidate);
            }
        } else if score >= MEDIUM_CONFIDENCE_THRESHOLD {
            medium.push(candidate);
        } else {
            discarded.push(candidate);
        }
    }

    (best, medium, discarded)
}

pub fn merge_flag_candidates(
    candidates: Vec<FlagCandidate>,
    threshold: f64,
) -> GateResult<FlagSchema, FlagCandidate> {
    let mut grouped: HashMap<String, Vec<FlagCandidate>> = HashMap::new();
    for candidate in candidates {
        grouped
            .entry(candidate.canonical_key())
            .or_default()
            .push(candidate);
    }

    let mut accepted = Vec::new();
    let mut medium_confidence = Vec::new();
    let mut discarded = Vec::new();

    for (_key, group) in grouped {
        let (best, mut medium, mut low) = choose_best_candidate(group, score_flag_candidate);
        if let Some(best_candidate) = best {
            let score = score_flag_candidate(&best_candidate);
            if score >= threshold {
                accepted.push(best_candidate.into_schema());
            } else if score >= MEDIUM_CONFIDENCE_THRESHOLD {
                medium.push(best_candidate);
            } else {
                low.push(best_candidate);
            }
        }

        medium_confidence.append(&mut medium);
        discarded.append(&mut low);
    }

    accepted.sort_by(|a, b| a.canonical_name().cmp(b.canonical_name()));
    GateResult {
        accepted,
        medium_confidence,
        discarded,
    }
}

pub fn merge_subcommand_candidates(
    candidates: Vec<SubcommandCandidate>,
    threshold: f64,
) -> GateResult<SubcommandSchema, SubcommandCandidate> {
    let mut grouped: HashMap<String, Vec<SubcommandCandidate>> = HashMap::new();
    for candidate in candidates {
        grouped
            .entry(candidate.canonical_key())
            .or_default()
            .push(candidate);
    }

    let mut accepted = Vec::new();
    let mut medium_confidence = Vec::new();
    let mut discarded = Vec::new();

    for (_key, group) in grouped {
        let (best, mut medium, mut low) = choose_best_candidate(group, score_subcommand_candidate);
        if let Some(best_candidate) = best {
            let score = score_subcommand_candidate(&best_candidate);
            if score >= threshold {
                accepted.push(best_candidate.into_schema());
            } else if score >= MEDIUM_CONFIDENCE_THRESHOLD {
                medium.push(best_candidate);
            } else {
                low.push(best_candidate);
            }
        }

        medium_confidence.append(&mut medium);
        discarded.append(&mut low);
    }

    accepted.sort_by(|a, b| a.name.cmp(&b.name));
    GateResult {
        accepted,
        medium_confidence,
        discarded,
    }
}

pub fn merge_arg_candidates(
    candidates: Vec<ArgCandidate>,
    threshold: f64,
) -> GateResult<ArgSchema, ArgCandidate> {
    let mut grouped: HashMap<String, Vec<ArgCandidate>> = HashMap::new();
    for candidate in candidates {
        grouped
            .entry(candidate.canonical_key())
            .or_default()
            .push(candidate);
    }

    let mut accepted = Vec::new();
    let mut medium_confidence = Vec::new();
    let mut discarded = Vec::new();

    for (_key, group) in grouped {
        let (best, mut medium, mut low) = choose_best_candidate(group, score_arg_candidate);
        if let Some(best_candidate) = best {
            let score = score_arg_candidate(&best_candidate);
            if score >= threshold {
                accepted.push(best_candidate.into_schema());
            } else if score >= MEDIUM_CONFIDENCE_THRESHOLD {
                medium.push(best_candidate);
            } else {
                low.push(best_candidate);
            }
        }

        medium_confidence.append(&mut medium);
        discarded.append(&mut low);
    }

    accepted.sort_by(|a, b| a.name.cmp(&b.name));
    GateResult {
        accepted,
        medium_confidence,
        discarded,
    }
}

pub fn finalize_schema(mut schema: CommandSchema) -> CommandSchema {
    schema.subcommands.sort_by(|a, b| a.name.cmp(&b.name));
    schema
        .global_flags
        .sort_by(|a, b| a.canonical_name().cmp(b.canonical_name()));
    schema.positional.sort_by(|a, b| a.name.cmp(&b.name));

    for subcmd in &mut schema.subcommands {
        subcmd
            .flags
            .sort_by(|a, b| a.canonical_name().cmp(b.canonical_name()));
        subcmd.aliases.sort();
        subcmd.subcommands.sort_by(|a, b| a.name.cmp(&b.name));
    }

    schema
}
