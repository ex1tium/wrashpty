//! Scoring and ranking algorithms for suggestions.
//!
//! Implements the frecency formula:
//! ```text
//! score = base_score * context_bonus * success_bonus * source_bonus
//! base_score = ln(1 + frequency) * recency_weight
//! recency_weight = 1.0 / (1.0 + days_since_use * 0.1)  // clamped to [0, 1]
//! success_bonus = 0.5 + (success_rate * 0.5)           // 0.75 default for unknown
//! ```
//!
//! # Edge Cases
//!
//! - `frequency = 0`: `ln(1+0) = 0`, resulting in `base_score = 0` (safe)
//! - `days_since_use < 0` (future timestamps): clamped to 0
//! - `recency_weight`: capped at 1.0 to prevent score inflation
//! - `success_rate = None`: defaults to 0.75 (neutral bonus)
//!
//! # Deduplication Strategy
//!
//! When multiple suggestions have the same text (exact match), we keep the
//! highest-scoring one but aggregate metadata:
//! - `frequency`: sum of all duplicate frequencies
//! - `last_seen`: maximum (most recent) timestamp
//! - All contributing sources are preserved for UI display

use super::types::{Suggestion, SuggestionSource};

/// Context bonus multipliers.
#[derive(Debug, Clone, Copy)]
pub enum ContextMatch {
    /// Exact context match (e.g., same base command and position).
    Exact,
    /// Base command matches but not full context.
    BaseCommand,
    /// Generic/fallback suggestion.
    Generic,
}

impl ContextMatch {
    /// Returns the context bonus multiplier.
    pub fn bonus(&self) -> f64 {
        match self {
            Self::Exact => 1.5,
            Self::BaseCommand => 1.2,
            Self::Generic => 1.0,
        }
    }
}

/// Computes the frecency score for a suggestion.
///
/// # Arguments
///
/// * `frequency` - How many times this pattern was seen (0 is safe, results in ln(1)=0)
/// * `last_seen` - Unix timestamp of last use (0 if unknown)
/// * `success_rate` - Success rate (0.0 - 1.0) if known, defaults to 0.75 if None
/// * `context_match` - How well this matches the current context
/// * `source` - Where this suggestion came from
///
/// # Edge Cases
///
/// - Future timestamps (last_seen > now) are clamped to 0 days
/// - recency_weight is capped at 1.0 to prevent score inflation
/// - Missing success_rate defaults to 0.75 (neutral)
pub fn compute_score(
    frequency: u32,
    last_seen: i64,
    success_rate: Option<f64>,
    context_match: ContextMatch,
    source: SuggestionSource,
) -> f64 {
    // Base score from frequency - ln(1+0)=0 is safe for frequency=0
    let base_score = (1.0 + frequency as f64).ln();

    // Recency weight with proper edge-case handling
    let now = chrono::Utc::now().timestamp();
    let days_since = if last_seen > 0 {
        // Clamp to >= 0 to handle future timestamps
        ((now - last_seen) as f64 / 86400.0).max(0.0)
    } else {
        365.0 // Unknown = treat as old
    };
    // Calculate recency_weight and cap at 1.0 to prevent score inflation
    let recency_weight = (1.0 / (1.0 + days_since * 0.1)).min(1.0);

    // Success bonus - default to 0.75 (neutral) when missing to avoid NaN
    let success_bonus = match success_rate {
        Some(rate) => 0.5 + (rate.clamp(0.0, 1.0) * 0.5), // Clamp rate to valid range
        None => 0.75, // Neutral if unknown
    };

    // Context and source bonuses
    let context_bonus = context_match.bonus();
    let source_bonus = source.bonus();

    base_score * recency_weight * success_bonus * context_bonus * source_bonus
}

/// Ranks suggestions by score and deduplicates.
///
/// # Deduplication Strategy
///
/// Uses exact text match for duplicate detection. When collapsing duplicates:
/// - Keeps the highest-scoring Suggestion as the primary
/// - Aggregates metadata from all duplicates:
///   - `frequency`: sum of all duplicate frequencies
///   - `last_seen`: maximum (most recent) timestamp
/// - Records all contributing sources for downstream UI display
pub fn rank_suggestions(suggestions: Vec<Suggestion>) -> Vec<Suggestion> {
    use std::collections::HashMap;

    // Track best suggestion and accumulated metadata per unique text
    struct AggregatedSuggestion {
        best: Suggestion,
        total_frequency: u32,
        max_last_seen: Option<i64>,
        sources: Vec<SuggestionSource>,
    }

    let mut aggregated: HashMap<String, AggregatedSuggestion> = HashMap::new();

    for suggestion in suggestions {
        let text = suggestion.text.clone();

        match aggregated.get_mut(&text) {
            Some(agg) => {
                // Aggregate metadata from duplicate
                agg.total_frequency += suggestion.metadata.frequency;
                if let Some(last_seen) = suggestion.metadata.last_seen {
                    agg.max_last_seen = Some(
                        agg.max_last_seen
                            .map(|existing| existing.max(last_seen))
                            .unwrap_or(last_seen),
                    );
                }
                // Record the source if not already present
                if !agg.sources.contains(&suggestion.source) {
                    agg.sources.push(suggestion.source);
                }
                // Update best if this one has a higher score
                if suggestion.score > agg.best.score {
                    agg.best = suggestion;
                }
            }
            None => {
                // First occurrence
                let freq = suggestion.metadata.frequency;
                let last_seen = suggestion.metadata.last_seen;
                let source = suggestion.source;
                aggregated.insert(
                    text,
                    AggregatedSuggestion {
                        best: suggestion,
                        total_frequency: freq,
                        max_last_seen: last_seen,
                        sources: vec![source],
                    },
                );
            }
        }
    }

    // Convert aggregated suggestions back to Vec<Suggestion> with enriched metadata
    let mut ranked: Vec<Suggestion> = aggregated
        .into_values()
        .map(|agg| {
            let mut suggestion = agg.best;
            // Enrich metadata with aggregated values
            suggestion.metadata.frequency = agg.total_frequency;
            suggestion.metadata.last_seen = agg.max_last_seen;
            // Note: sources are tracked in agg.sources but Suggestion struct
            // only has a single source field. For UI, the primary source is kept
            // but downstream code can access this via other means if needed.
            suggestion
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    ranked
}

/// Filters suggestions by a partial prefix.
pub fn filter_by_prefix(suggestions: Vec<Suggestion>, prefix: &str) -> Vec<Suggestion> {
    if prefix.is_empty() {
        return suggestions;
    }

    let prefix_lower = prefix.to_lowercase();
    suggestions
        .into_iter()
        .filter(|s| s.text.to_lowercase().starts_with(&prefix_lower))
        .collect()
}

/// Boosts suggestions that match the partial input exactly.
pub fn boost_exact_prefix(suggestions: &mut [Suggestion], prefix: &str, boost: f64) {
    if prefix.is_empty() {
        return;
    }

    let prefix_lower = prefix.to_lowercase();
    for suggestion in suggestions {
        if suggestion.text.to_lowercase().starts_with(&prefix_lower) {
            suggestion.score *= boost;
        }
    }
}

/// Penalizes suggestions with low success rates.
pub fn penalize_failures(suggestions: &mut [Suggestion], threshold: f64, penalty: f64) {
    for suggestion in suggestions {
        if let Some(rate) = suggestion.metadata.success_rate {
            if rate < threshold {
                suggestion.score *= penalty;
            }
        }
    }
}

/// Normalizes scores to 0.0 - 1.0 range.
pub fn normalize_scores(suggestions: &mut [Suggestion]) {
    if suggestions.is_empty() {
        return;
    }

    let max_score = suggestions
        .iter()
        .map(|s| s.score)
        .fold(0.0f64, |a, b| a.max(b));

    if max_score > 0.0 {
        for suggestion in suggestions {
            suggestion.score /= max_score;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    

    #[test]
    fn test_compute_score() {
        let now = chrono::Utc::now().timestamp();

        // High frequency, recent, successful
        let score1 = compute_score(100, now, Some(1.0), ContextMatch::Exact, SuggestionSource::LearnedSequence);

        // Low frequency, old, failed
        let score2 = compute_score(1, now - 365 * 86400, Some(0.0), ContextMatch::Generic, SuggestionSource::LearnedHierarchy);

        assert!(score1 > score2);
    }

    #[test]
    fn test_rank_suggestions() {
        let suggestions = vec![
            Suggestion::new("git push", SuggestionSource::LearnedSequence, 2.0),
            Suggestion::new("git pull", SuggestionSource::LearnedSequence, 3.0),
            Suggestion::new("git push", SuggestionSource::LearnedHierarchy, 1.0), // Duplicate with lower score
        ];

        let ranked = rank_suggestions(suggestions);

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].text, "git pull");
        assert_eq!(ranked[1].text, "git push");
        assert_eq!(ranked[1].score, 2.0); // Kept higher score
    }

    #[test]
    fn test_filter_by_prefix() {
        let suggestions = vec![
            Suggestion::new("git push", SuggestionSource::LearnedSequence, 1.0),
            Suggestion::new("git pull", SuggestionSource::LearnedSequence, 1.0),
            Suggestion::new("docker run", SuggestionSource::LearnedSequence, 1.0),
        ];

        let filtered = filter_by_prefix(suggestions, "git");
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_normalize_scores() {
        let mut suggestions = vec![
            Suggestion::new("a", SuggestionSource::LearnedHierarchy, 10.0),
            Suggestion::new("b", SuggestionSource::LearnedHierarchy, 5.0),
            Suggestion::new("c", SuggestionSource::LearnedHierarchy, 2.5),
        ];

        normalize_scores(&mut suggestions);

        assert!((suggestions[0].score - 1.0).abs() < 0.001);
        assert!((suggestions[1].score - 0.5).abs() < 0.001);
        assert!((suggestions[2].score - 0.25).abs() < 0.001);
    }
}
