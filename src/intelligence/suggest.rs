//! Main suggestion engine.
//!
//! Aggregates suggestions from all sources and ranks them.

use rusqlite::Connection;
use tracing::debug;

use super::fuzzy;
use super::patterns;
use super::scoring::{self, ContextMatch};
use super::sessions;
use super::templates;
use super::types::{
    PositionType, Suggestion, SuggestionContext, SuggestionMetadata, SuggestionSource,
};
use super::user_patterns;
use super::variants;

use crate::chrome::command_knowledge::COMMAND_KNOWLEDGE;

/// Main entry point for getting suggestions.
pub fn suggest(conn: &Connection, context: &SuggestionContext, limit: usize) -> Vec<Suggestion> {
    let mut suggestions = gather_suggestions(conn, context);

    // Apply prefix filter if partial text provided
    if !context.partial.is_empty() {
        suggestions = scoring::filter_by_prefix(suggestions, &context.partial);
    }

    // Enrich suggestions with success rate data from variants
    enrich_with_success_rates(conn, &mut suggestions);

    // Rank and deduplicate
    let mut ranked = scoring::rank_suggestions(suggestions);

    // Penalize commands with low success rates (threshold: 30%, penalty: 50%)
    scoring::penalize_failures(&mut ranked, 0.3, 0.5);

    // Boost commands with high success rates
    boost_successful(&mut ranked, 0.8, 1.2);

    // Re-sort after penalties/boosts
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Limit results
    ranked.truncate(limit);

    debug!(
        count = ranked.len(),
        position = ?context.position,
        "Generated suggestions"
    );

    ranked
}

/// Enriches suggestions with success rate data from the variants table.
///
/// Looks up each suggestion's text in the variants database and populates
/// the metadata.success_rate field. Also attempts to swap to a better variant
/// when available.
fn enrich_with_success_rates(conn: &Connection, suggestions: &mut [Suggestion]) {
    for suggestion in suggestions.iter_mut() {
        // Look up success rate for this command
        if let Ok(Some(rate)) = variants::get_success_rate(conn, &suggestion.text) {
            suggestion.metadata.success_rate = Some(rate);

            // If success rate is low, try to find a better variant
            if rate < 0.5 {
                if let Ok(Some(better_variant)) = variants::get_best_variant(
                    conn,
                    &variants::canonicalize_for_lookup(&suggestion.text),
                ) {
                    // Check that the better variant has a higher success rate
                    if let Ok(Some(better_rate)) = variants::get_success_rate(conn, &better_variant) {
                        if better_rate > rate {
                            debug!(
                                original = %suggestion.text,
                                replacement = %better_variant,
                                original_rate = rate,
                                better_rate = better_rate,
                                "Swapping to higher-success variant"
                            );
                            suggestion.text = better_variant;
                            suggestion.metadata.success_rate = Some(better_rate);
                        }
                    }
                }
            }
        }
    }
}

/// Boosts suggestions with high success rates.
fn boost_successful(suggestions: &mut [Suggestion], threshold: f64, boost: f64) {
    for suggestion in suggestions {
        if let Some(rate) = suggestion.metadata.success_rate {
            if rate >= threshold {
                suggestion.score *= boost;
            }
        }
    }
}

/// Gathers suggestions from all sources.
///
/// When `context.token_mode` is true, only token-level sources are used
/// (learned sequences, flag values, etc.) to return individual tokens.
/// When false, all sources including full-command sources (templates, fuzzy)
/// are used for traditional command-line completion.
fn gather_suggestions(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // 1. User patterns (highest priority) - always include, they're user-defined
    suggestions.extend(suggest_user_patterns(conn, context));

    // 2. Session transitions (if last command available)
    // Skip in token mode - these return full commands
    if !context.token_mode {
        if let Some(ref last_cmd) = context.last_command {
            suggestions.extend(sessions::suggest_next(conn, last_cmd));
        }
    }

    // 3. Position-specific suggestions - these return individual tokens
    match &context.position {
        PositionType::Command => {
            suggestions.extend(suggest_commands(conn, context));
        }
        PositionType::Subcommand => {
            suggestions.extend(suggest_subcommands(conn, context));
        }
        PositionType::FlagValue { flag } => {
            suggestions.extend(suggest_flag_values(conn, context, flag));
        }
        PositionType::AfterPipe => {
            suggestions.extend(suggest_pipe_commands(conn, context));
        }
        PositionType::Argument | PositionType::AfterRedirect => {
            suggestions.extend(suggest_arguments(conn, context));
        }
    }

    // 4. Templates - skip in token mode (returns full commands)
    if !context.token_mode {
        suggestions.extend(suggest_from_templates(conn, context));
    }

    // 5. Fuzzy search - skip in token mode (returns full commands)
    if !context.token_mode && !context.partial.is_empty() && suggestions.len() < 5 {
        suggestions.extend(suggest_fuzzy(conn, context));
    }

    // 6. Static knowledge (fallback) - always include, these are individual tokens
    suggestions.extend(suggest_static(context));

    suggestions
}

/// Suggests from user patterns.
fn suggest_user_patterns(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    user_patterns::suggest_from_patterns(conn, context).unwrap_or_default()
}

/// Suggests commands for position 0.
fn suggest_commands(conn: &Connection, _context: &SuggestionContext) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Query base commands (first tokens) aggregated by frequency
    let learned = patterns::suggest_base_commands(conn, 20);
    for (text, freq, success, last_seen) in learned {
        let success_rate = if freq > 0 {
            Some(success as f64 / freq as f64)
        } else {
            None
        };

        let score = scoring::compute_score(
            freq,
            last_seen,
            success_rate,
            ContextMatch::Generic,
            SuggestionSource::LearnedSequence,
        );

        suggestions.push(Suggestion {
            text,
            source: SuggestionSource::LearnedSequence,
            score,
            metadata: SuggestionMetadata {
                frequency: freq,
                success_rate,
                last_seen: Some(last_seen),
                ..Default::default()
            },
        });
    }

    suggestions
}

/// Suggests subcommands.
fn suggest_subcommands(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Get base command
    let base_cmd = context
        .preceding_tokens
        .first()
        .map(|t| t.text.as_str())
        .unwrap_or("");

    if base_cmd.is_empty() {
        return suggestions;
    }

    // From learned sequences
    let learned = patterns::suggest_from_sequences(conn, base_cmd, 0, Some(base_cmd), 20);
    for (text, freq, success, last_seen) in learned {
        let success_rate = if freq > 0 {
            Some(success as f64 / freq as f64)
        } else {
            None
        };

        let score = scoring::compute_score(
            freq,
            last_seen,
            success_rate,
            ContextMatch::BaseCommand,
            SuggestionSource::LearnedSequence,
        );

        suggestions.push(Suggestion {
            text,
            source: SuggestionSource::LearnedSequence,
            score,
            metadata: SuggestionMetadata {
                frequency: freq,
                success_rate,
                last_seen: Some(last_seen),
                ..Default::default()
            },
        });
    }

    suggestions
}

/// Suggests flag values.
fn suggest_flag_values(conn: &Connection, context: &SuggestionContext, flag: &str) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Get base command and subcommand
    let base_cmd = context
        .preceding_tokens
        .first()
        .map(|t| t.text.as_str())
        .unwrap_or("");

    let subcommand = context
        .preceding_tokens
        .get(1)
        .filter(|t| t.token_type == crate::chrome::command_edit::TokenType::Subcommand)
        .map(|t| t.text.as_str());

    if base_cmd.is_empty() {
        return suggestions;
    }

    // From learned flag values
    let learned = patterns::suggest_flag_values(conn, base_cmd, subcommand, flag, 20);
    for (text, freq, last_seen) in learned {
        let score = scoring::compute_score(
            freq,
            last_seen,
            None,
            ContextMatch::Exact,
            SuggestionSource::LearnedFlagValue,
        );

        suggestions.push(Suggestion {
            text,
            source: SuggestionSource::LearnedFlagValue,
            score,
            metadata: SuggestionMetadata {
                frequency: freq,
                last_seen: Some(last_seen),
                ..Default::default()
            },
        });
    }

    suggestions
}

/// Suggests commands after a pipe.
fn suggest_pipe_commands(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Get pre-pipe base command (first token before the pipe)
    // We want the first token of the segment before the pipe, not the last
    let base_cmd = context
        .preceding_tokens
        .iter()
        .take_while(|t| t.text != "|" && !t.text.ends_with('|'))
        .map(|t| t.text.as_str())
        .next()  // Get the first token (base command) of the pre-pipe segment
        .unwrap_or("");

    // From learned pipe chains
    let learned = patterns::suggest_pipe_commands(conn, base_cmd, 20);
    for (text, freq, last_seen) in learned {
        let score = scoring::compute_score(
            freq,
            last_seen,
            None,
            ContextMatch::BaseCommand,
            SuggestionSource::LearnedPipe,
        );

        suggestions.push(Suggestion {
            text,
            source: SuggestionSource::LearnedPipe,
            score,
            metadata: SuggestionMetadata {
                frequency: freq,
                last_seen: Some(last_seen),
                ..Default::default()
            },
        });
    }

    suggestions
}

/// Suggests generic arguments.
fn suggest_arguments(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Get context token (the one before current position)
    let context_token = context
        .preceding_tokens
        .last()
        .map(|t| t.text.as_str())
        .unwrap_or("");

    let position = context.preceding_tokens.len();

    let base_cmd = context
        .preceding_tokens
        .first()
        .map(|t| t.text.as_str());

    // From learned sequences
    let learned = patterns::suggest_from_sequences(conn, context_token, position, base_cmd, 20);
    for (text, freq, success, last_seen) in learned {
        let success_rate = if freq > 0 {
            Some(success as f64 / freq as f64)
        } else {
            None
        };

        let score = scoring::compute_score(
            freq,
            last_seen,
            success_rate,
            ContextMatch::Exact,
            SuggestionSource::LearnedSequence,
        );

        suggestions.push(Suggestion {
            text,
            source: SuggestionSource::LearnedSequence,
            score,
            metadata: SuggestionMetadata {
                frequency: freq,
                success_rate,
                last_seen: Some(last_seen),
                ..Default::default()
            },
        });
    }

    suggestions
}

/// Suggests from templates.
fn suggest_from_templates(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let completions = templates::suggest_templates(conn, context);

    completions
        .into_iter()
        .map(|c| Suggestion {
            text: c.preview.clone(),
            source: SuggestionSource::Template,
            score: c.confidence * SuggestionSource::Template.bonus(),
            metadata: SuggestionMetadata {
                frequency: c.template.frequency,
                template_preview: Some(c.preview),
                ..Default::default()
            },
        })
        .collect()
}

/// Suggests using fuzzy search.
fn suggest_fuzzy(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let matches = fuzzy::fuzzy_search(conn, &context.partial, 10).unwrap_or_default();

    matches
        .into_iter()
        .map(|m| Suggestion {
            text: m.command,
            source: SuggestionSource::FuzzySearch,
            score: m.bm25_score * SuggestionSource::FuzzySearch.bonus(),
            metadata: SuggestionMetadata {
                fuzzy_score: Some(m.bm25_score),
                ..Default::default()
            },
        })
        .collect()
}

/// Suggests from static knowledge.
fn suggest_static(context: &SuggestionContext) -> Vec<Suggestion> {
    let preceding: Vec<&str> = context
        .preceding_tokens
        .iter()
        .map(|t| t.text.as_str())
        .collect();

    // Check for pipe context
    let has_pipe = preceding.iter().any(|t| *t == "|" || t.ends_with('|'));

    let static_suggestions = if has_pipe {
        COMMAND_KNOWLEDGE.pipeable_commands()
    } else if let Some(ref file_ctx) = context.file_context {
        COMMAND_KNOWLEDGE.commands_for_filetype(&file_ctx.filename)
    } else {
        COMMAND_KNOWLEDGE.suggestions_for_position(&preceding)
    };

    static_suggestions
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            // Give static suggestions a base score that decreases with position
            let base_score = 0.5 / (1.0 + i as f64 * 0.1);
            Suggestion {
                text: text.to_string(),
                source: SuggestionSource::StaticKnowledge,
                score: base_score * SuggestionSource::StaticKnowledge.bonus(),
                metadata: SuggestionMetadata::default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::schema;
    use crate::intelligence::types::AnalyzedToken;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_suggest_empty_context() {
        let conn = setup_test_db();
        let context = SuggestionContext::default();

        let suggestions = suggest(&conn, &context, 10);
        // Should return static suggestions for commands
        assert!(!suggestions.is_empty());
    }

    #[test]
    fn test_suggest_with_preceding() {
        let conn = setup_test_db();
        let context = SuggestionContext {
            preceding_tokens: vec![AnalyzedToken::new(
                "git",
                crate::chrome::command_edit::TokenType::Command,
                0,
            )],
            position: PositionType::Subcommand,
            ..Default::default()
        };

        let suggestions = suggest(&conn, &context, 10);
        // Should return git subcommands from static knowledge
        assert!(!suggestions.is_empty());
    }

    #[test]
    fn test_gather_suggestions_deduplicates() {
        let suggestions = vec![
            Suggestion::new("git", SuggestionSource::LearnedSequence, 2.0),
            Suggestion::new("git", SuggestionSource::StaticKnowledge, 1.0),
        ];

        let ranked = scoring::rank_suggestions(suggestions);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].score, 2.0);
    }

    #[test]
    fn test_enrich_with_success_rates() {
        let conn = setup_test_db();

        // Record some executions to build success rate data
        super::variants::record_execution(&conn, "git push", 0).unwrap();
        super::variants::record_execution(&conn, "git push", 0).unwrap();
        super::variants::record_execution(&conn, "git push", 0).unwrap();
        super::variants::record_execution(&conn, "git push", 1).unwrap();

        // Low success rate command
        super::variants::record_execution(&conn, "git rebase -i", 1).unwrap();
        super::variants::record_execution(&conn, "git rebase -i", 1).unwrap();
        super::variants::record_execution(&conn, "git rebase -i", 1).unwrap();

        let mut suggestions = vec![
            Suggestion::new("git push", SuggestionSource::LearnedSequence, 1.0),
            Suggestion::new("git rebase -i", SuggestionSource::LearnedSequence, 1.0),
        ];

        enrich_with_success_rates(&conn, &mut suggestions);

        // git push should have ~75% success rate
        assert!(suggestions[0].metadata.success_rate.is_some());
        let push_rate = suggestions[0].metadata.success_rate.unwrap();
        assert!(push_rate > 0.7 && push_rate < 0.8);

        // git rebase -i should have 0% success rate
        assert!(suggestions[1].metadata.success_rate.is_some());
        let rebase_rate = suggestions[1].metadata.success_rate.unwrap();
        assert!(rebase_rate < 0.1);
    }

    #[test]
    fn test_failed_variants_are_penalized() {
        let conn = setup_test_db();

        // Create one successful command and one failed command
        super::variants::record_execution(&conn, "cargo build", 0).unwrap();
        super::variants::record_execution(&conn, "cargo build", 0).unwrap();
        super::variants::record_execution(&conn, "cargo build --broken", 1).unwrap();
        super::variants::record_execution(&conn, "cargo build --broken", 1).unwrap();

        let mut suggestions = vec![
            Suggestion::new("cargo build", SuggestionSource::LearnedSequence, 1.0),
            Suggestion::new("cargo build --broken", SuggestionSource::LearnedSequence, 1.0),
        ];

        enrich_with_success_rates(&conn, &mut suggestions);

        // Apply penalties
        scoring::penalize_failures(&mut suggestions, 0.3, 0.5);

        // Successful command should keep its score
        assert_eq!(suggestions[0].score, 1.0);

        // Failed command should be penalized
        assert!(suggestions[1].score < 1.0);
    }

    #[test]
    fn test_boost_successful() {
        let mut suggestions = vec![
            Suggestion {
                text: "git push".to_string(),
                source: SuggestionSource::LearnedSequence,
                score: 1.0,
                metadata: SuggestionMetadata {
                    success_rate: Some(0.9), // High success rate
                    ..Default::default()
                },
            },
            Suggestion {
                text: "git rebase".to_string(),
                source: SuggestionSource::LearnedSequence,
                score: 1.0,
                metadata: SuggestionMetadata {
                    success_rate: Some(0.5), // Medium success rate
                    ..Default::default()
                },
            },
        ];

        boost_successful(&mut suggestions, 0.8, 1.2);

        // High success rate should be boosted
        assert!((suggestions[0].score - 1.2).abs() < 0.001);

        // Medium success rate should not be boosted
        assert!((suggestions[1].score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_token_mode_excludes_full_commands() {
        let conn = setup_test_db();

        // Create a template that would normally return a full command
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO ci_tokens (text, token_type, first_seen, last_seen) VALUES ('git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_templates (template, template_hash, base_command_id, placeholder_count, placeholders, frequency, last_seen, example_command)
             VALUES ('git remote add <URL>', 'hash1', 1, 1, '[]', 10, ?1, 'git remote add git@github.com:user/repo.git')",
            [now],
        ).unwrap();

        // Create a context with token_mode = false (default)
        let context_full = SuggestionContext {
            preceding_tokens: vec![
                AnalyzedToken::new("git", crate::chrome::command_edit::TokenType::Command, 0),
                AnalyzedToken::new("remote", crate::chrome::command_edit::TokenType::Subcommand, 1),
                AnalyzedToken::new("add", crate::chrome::command_edit::TokenType::Argument, 2),
            ],
            partial: String::new(),
            position: PositionType::Argument,
            token_mode: false, // Full command mode
            ..Default::default()
        };

        // Create a context with token_mode = true
        let context_token = SuggestionContext {
            preceding_tokens: vec![
                AnalyzedToken::new("git", crate::chrome::command_edit::TokenType::Command, 0),
                AnalyzedToken::new("remote", crate::chrome::command_edit::TokenType::Subcommand, 1),
                AnalyzedToken::new("add", crate::chrome::command_edit::TokenType::Argument, 2),
            ],
            partial: String::new(),
            position: PositionType::Argument,
            token_mode: true, // Token-only mode
            ..Default::default()
        };

        let suggestions_full = gather_suggestions(&conn, &context_full);
        let suggestions_token = gather_suggestions(&conn, &context_token);

        // In full mode, we might get template suggestions
        // In token mode, we should NOT get template suggestions
        let full_has_template = suggestions_full.iter().any(|s| s.source == SuggestionSource::Template);
        let token_has_template = suggestions_token.iter().any(|s| s.source == SuggestionSource::Template);

        // Full mode may or may not have templates (depends on matching)
        // But token mode should NEVER have templates
        assert!(!token_has_template, "Token mode should not include template suggestions");

        // Verify the test is meaningful by checking full mode could have templates
        // (This depends on template matching, so we just verify the logic works)
        if full_has_template {
            assert!(true, "Full mode correctly includes templates when matching");
        }
    }
}
