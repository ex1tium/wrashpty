//! Main suggestion engine.
//!
//! Aggregates suggestions from all sources and ranks them using frecency scoring.
//!
//! # Suggestion Sources (Priority Order)
//!
//! 1. **User Patterns** (`ci_user_patterns`): Highest priority. User-defined
//!    aliases, completions, and rules. Always included in results.
//!
//! 2. **Command Hierarchy** (`ci_command_hierarchy`): Primary learned source.
//!    Provides position-aware token suggestions based on:
//!    - Current position in command
//!    - Parent token context
//!    - Base command context
//!
//! 3. **Supplementary Sources** (context-specific):
//!    - Flag values (`ci_flag_values`) for `--flag <value>` positions
//!    - Pipe commands (`ci_pipe_chains`) for post-pipe positions
//!    - Command schemas (`SchemaProvider`) for subcommands/flags/value choices
//!    - Session transitions (`ci_transitions`) for likely next commands
//!    - Templates (`ci_templates`) for reusable command patterns
//!
//! # Scoring and Ranking
//!
//! All suggestions are scored using the frecency formula (see `scoring` module),
//! then deduplicated, penalized for low success rates, and boosted for high
//! success rates before final ranking.

use command_schema_core::{CommandSchema, FlagSchema, SubcommandSchema, ValueType};
use rusqlite::Connection;
use tracing::debug;

use crate::chrome::command_edit::TokenType;

use super::fuzzy;
use super::patterns;
use super::schema_provider::{SchemaMode, SchemaProvider};
use super::scoring::{self, ContextMatch};
use super::sessions;
use super::templates;
use super::types::{
    PositionType, Suggestion, SuggestionContext, SuggestionMetadata, SuggestionSource,
};
use super::user_patterns;
use super::variants;

/// Main entry point for getting suggestions.
pub fn suggest(
    conn: &Connection,
    provider: &dyn SchemaProvider,
    schema_mode: SchemaMode,
    context: &SuggestionContext,
    limit: usize,
) -> Vec<Suggestion> {
    let mut suggestions = gather_suggestions(conn, provider, schema_mode, context);

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

    // Re-sort after penalties/boosts (tiebreak alphabetically for determinism)
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.text.cmp(&b.text))
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
                    if let Ok(Some(better_rate)) = variants::get_success_rate(conn, &better_variant)
                    {
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
/// The primary source is the learned command hierarchy, which provides
/// position-aware token suggestions. This unified approach eliminates
/// the need for separate token_mode handling - all token suggestions
/// come from the same learned source.
fn gather_suggestions(
    conn: &Connection,
    provider: &dyn SchemaProvider,
    schema_mode: SchemaMode,
    context: &SuggestionContext,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // 1. User patterns (highest priority) - always include, they're user-defined
    suggestions.extend(suggest_user_patterns(conn, context));

    // 2. Primary source: learned command hierarchy
    // This provides position-aware token suggestions for all positions
    suggestions.extend(suggest_from_hierarchy(conn, context));

    // 3. Schema source: gated on SchemaMode (skipped in HistoryOnly mode)
    if schema_mode.uses_schemas() {
        suggestions.extend(suggest_from_schema(provider, context));
    }

    // 4. Session transitions and template completions
    suggestions.extend(suggest_from_session_transitions(conn, context));
    suggestions.extend(suggest_from_templates(conn, context));

    // 5. Supplementary sources for specific contexts
    match &context.position {
        PositionType::FlagValue { flag } => {
            // Add specialized flag value suggestions
            suggestions.extend(suggest_flag_values(conn, context, flag));
        }
        PositionType::AfterPipe => {
            // Add learned pipe chain suggestions
            suggestions.extend(suggest_pipe_commands(conn, context));

            // Fallback to frequent commands if no learned pipe transitions exist yet.
            if suggestions.is_empty() {
                suggestions.extend(suggest_from_historical_frequency(conn, 10));
            }
        }
        PositionType::Command => {
            // For command position, add historical frequency suggestions as fallback
            suggestions.extend(suggest_from_historical_frequency(conn, 10));
        }
        _ => {}
    }

    // 6. Fuzzy search fallback: if partial text is provided and might be a typo
    if !context.partial.is_empty() && context.partial.len() >= 2 {
        suggestions.extend(suggest_from_fuzzy_search(conn, &context.partial, 5));
    }

    suggestions
}

/// Suggests from user patterns.
fn suggest_user_patterns(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    user_patterns::suggest_from_patterns(conn, context).unwrap_or_default()
}

/// Suggests likely next commands from session transition history.
fn suggest_from_session_transitions(
    conn: &Connection,
    context: &SuggestionContext,
) -> Vec<Suggestion> {
    if !matches!(context.position, PositionType::Command) {
        return Vec::new();
    }

    let transition_seed = context.last_command.as_deref().or_else(|| {
        context
            .session
            .as_ref()
            .and_then(|session| session.recent_commands.last().map(String::as_str))
    });

    transition_seed
        .map(|last| sessions::suggest_next(conn, last))
        .unwrap_or_default()
}

/// Suggests template-based command completions.
fn suggest_from_templates(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    if matches!(context.position, PositionType::AfterPipe) {
        return Vec::new();
    }

    let now = chrono::Utc::now().timestamp();
    let context_match = if context.preceding_tokens.is_empty() {
        ContextMatch::Generic
    } else {
        ContextMatch::Exact
    };

    templates::suggest_templates(conn, context)
        .into_iter()
        .filter_map(|completion| {
            let score = scoring::compute_score(
                completion.template.frequency,
                now,
                None,
                context_match,
                SuggestionSource::Template,
            ) * completion.confidence.max(0.1);

            let preview = completion.preview;
            let text = template_token_for_context(&preview, context)?;
            Some(Suggestion {
                text,
                source: SuggestionSource::Template,
                score,
                metadata: SuggestionMetadata {
                    frequency: completion.template.frequency,
                    last_seen: Some(now),
                    template_preview: Some(preview),
                    ..Default::default()
                },
            })
        })
        .collect()
}

fn template_token_for_context(preview: &str, context: &SuggestionContext) -> Option<String> {
    let token_index = context.preceding_tokens.len();
    preview
        .split_whitespace()
        .nth(token_index)
        .map(ToString::to_string)
}

/// Score multiplier applied to schema-sourced suggestions.
///
/// Schema suggestions are slightly deprioritized relative to learned hierarchy
/// suggestions (which have real frequency/recency data) to avoid overwhelming
/// the user with schema entries they've never used.
const SCHEMA_SCORE_FACTOR: f64 = 0.9;

/// Suggests from stored command schemas.
///
/// This integrates extracted/bootstrapped command schemas into the same
/// unified suggestion pipeline used by learned hierarchy patterns.
fn suggest_from_schema(
    provider: &dyn SchemaProvider,
    context: &SuggestionContext,
) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    let now = chrono::Utc::now().timestamp();

    if matches!(context.position, PositionType::AfterPipe) {
        return suggestions;
    }

    match &context.position {
        PositionType::Command => {
            let mut commands: Vec<&str> = provider.commands().collect();
            commands.sort_unstable();
            for command in commands.into_iter().take(40) {
                suggestions.push(schema_suggestion(
                    command.to_string(),
                    ContextMatch::Generic,
                    now,
                    "schema_command",
                    None,
                ));
            }
        }
        PositionType::FlagValue { flag } => {
            let Some(base_command) = context.preceding_tokens.first().map(|t| t.text.as_str())
            else {
                return suggestions;
            };

            if let Some(schema) = provider.get(base_command) {
                let path = resolve_subcommand_path(schema, context);
                let active_subcommand = find_subcommand_by_path(schema, &path);
                let available_flags = schema_flags_for_context(schema, active_subcommand);

                if let Some(flag_schema) = available_flags
                    .iter()
                    .find(|candidate| candidate.matches(flag))
                {
                    for value in schema_flag_value_candidates(flag_schema) {
                        suggestions.push(schema_suggestion(
                            value,
                            ContextMatch::Exact,
                            now,
                            "schema_flag_value",
                            flag_schema.description.clone(),
                        ));
                    }
                }
            }
        }
        PositionType::Subcommand | PositionType::Argument | PositionType::AfterRedirect => {
            let Some(base_command) = context.preceding_tokens.first().map(|t| t.text.as_str())
            else {
                return suggestions;
            };

            if let Some(schema) = provider.get(base_command) {
                let path = resolve_subcommand_path(schema, context);
                let active_subcommand = find_subcommand_by_path(schema, &path);
                let next_subcommands = active_subcommand
                    .map(|sub| sub.subcommands.as_slice())
                    .unwrap_or(schema.subcommands.as_slice());

                let context_match = if active_subcommand.is_some() {
                    ContextMatch::Exact
                } else {
                    ContextMatch::BaseCommand
                };

                for sub in next_subcommands {
                    suggestions.push(schema_suggestion(
                        sub.name.clone(),
                        context_match,
                        now,
                        "schema_subcommand",
                        sub.description.clone(),
                    ));
                }

                if wants_flag_suggestions(context) {
                    for flag in schema_flags_for_context(schema, active_subcommand) {
                        if let Some(flag_text) = schema_flag_text(flag) {
                            suggestions.push(schema_suggestion(
                                flag_text,
                                context_match,
                                now,
                                "schema_flag",
                                flag.description.clone(),
                            ));
                        }
                    }
                }
            }
        }
        // AfterPipe is handled by the early return above; this arm is
        // unreachable but required for exhaustiveness.
        PositionType::AfterPipe => {}
    }

    suggestions
}

fn schema_suggestion(
    text: String,
    context_match: ContextMatch,
    last_seen: i64,
    role: &str,
    description: Option<String>,
) -> Suggestion {
    Suggestion {
        text,
        source: SuggestionSource::Schema,
        score: scoring::compute_score(1, last_seen, None, context_match, SuggestionSource::Schema)
            * SCHEMA_SCORE_FACTOR,
        metadata: SuggestionMetadata {
            frequency: 1,
            last_seen: Some(last_seen),
            role: Some(role.to_string()),
            description,
            ..Default::default()
        },
    }
}

fn wants_flag_suggestions(context: &SuggestionContext) -> bool {
    context.partial.starts_with('-')
}

fn schema_flag_text(flag: &FlagSchema) -> Option<String> {
    flag.long.clone().or_else(|| flag.short.clone())
}

fn schema_flags_for_context<'a>(
    schema: &'a CommandSchema,
    active_subcommand: Option<&'a SubcommandSchema>,
) -> Vec<&'a FlagSchema> {
    let mut flags: Vec<&FlagSchema> = schema.global_flags.iter().collect();
    if let Some(subcommand) = active_subcommand {
        flags.extend(subcommand.flags.iter());
    }
    flags
}

fn schema_flag_value_candidates(flag: &FlagSchema) -> Vec<String> {
    match &flag.value_type {
        ValueType::Choice(choices) => choices.clone(),
        ValueType::Branch => vec!["main".to_string(), "master".to_string()],
        ValueType::Remote => vec!["origin".to_string(), "upstream".to_string()],
        _ => Vec::new(),
    }
}

fn resolve_subcommand_path(schema: &CommandSchema, context: &SuggestionContext) -> Vec<String> {
    let mut path = Vec::new();
    let mut current_subcommands = &schema.subcommands;

    for token in context.preceding_tokens.iter().skip(1) {
        let text = token.text.as_str();
        // Break on structural tokens (type-first, text-fallback for old DB data)
        if matches!(
            token.token_type,
            TokenType::Pipe
                | TokenType::Redirect
                | TokenType::Operator
                | TokenType::Flag
                | TokenType::HeredocMarker
                | TokenType::HeredocBody
                | TokenType::HeredocDelimiter
        ) || text == "|"
            || text == ">"
            || text == ">>"
            || text == "<"
        {
            break;
        }

        let Some(next) = current_subcommands
            .iter()
            .find(|sub| sub.name == text || sub.aliases.iter().any(|alias| alias == text))
        else {
            break;
        };

        path.push(next.name.clone());
        current_subcommands = &next.subcommands;
    }

    path
}

fn find_subcommand_by_path<'a>(
    schema: &'a CommandSchema,
    path: &[String],
) -> Option<&'a SubcommandSchema> {
    let first = path.first()?;
    let mut current = schema
        .subcommands
        .iter()
        .find(|sub| sub.name == *first || sub.aliases.iter().any(|alias| alias == first))?;

    for segment in &path[1..] {
        current = current
            .subcommands
            .iter()
            .find(|sub| sub.name == *segment || sub.aliases.iter().any(|alias| alias == segment))?;
    }

    Some(current)
}

/// Primary suggestion source: learned command hierarchy.
///
/// Queries the command hierarchy for tokens at the current position,
/// using parent token and base command for context-aware suggestions.
fn suggest_from_hierarchy(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    let position = context.preceding_tokens.len();

    // Get parent token (last preceding token)
    let parent_token = context.preceding_tokens.last().map(|t| t.text.as_str());

    // Get base command (first token)
    let base_command = context.preceding_tokens.first().map(|t| t.text.as_str());

    // Query hierarchy for tokens at this position
    let learned = patterns::suggest_from_hierarchy(conn, position, parent_token, base_command, 30);

    for (text, freq, success, last_seen, role) in learned {
        let success_rate = if freq > 0 {
            Some(success as f64 / freq as f64)
        } else {
            None
        };

        // Context match depends on how specific the query was
        let context_match = if position == 0 {
            ContextMatch::Generic
        } else if base_command.is_some() && parent_token.is_some() {
            ContextMatch::Exact
        } else {
            ContextMatch::BaseCommand
        };

        let score = scoring::compute_score(
            freq,
            last_seen,
            success_rate,
            context_match,
            SuggestionSource::LearnedHierarchy,
        );

        suggestions.push(Suggestion {
            text,
            source: SuggestionSource::LearnedHierarchy,
            score,
            metadata: SuggestionMetadata {
                frequency: freq,
                success_rate,
                last_seen: Some(last_seen),
                role,
                ..Default::default()
            },
        });
    }

    suggestions
}

/// Suggests flag values (supplementary source for flag-value positions).
fn suggest_flag_values(
    conn: &Connection,
    context: &SuggestionContext,
    flag: &str,
) -> Vec<Suggestion> {
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

/// Suggests commands after a pipe (supplementary source for pipe positions).
fn suggest_pipe_commands(conn: &Connection, context: &SuggestionContext) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Get pre-pipe base command (first token before the pipe)
    let base_cmd = context
        .preceding_tokens
        .iter()
        .take_while(|t| {
            t.token_type != TokenType::Pipe && t.text != "|" && !t.text.ends_with('|')
        })
        .map(|t| t.text.as_str())
        .next()
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

/// Suggests based on fuzzy search for typo correction.
///
/// Uses FTS5 full-text search to find commands similar to the partial input.
/// This helps when the user makes typos in command names.
fn suggest_from_fuzzy_search(conn: &Connection, partial: &str, limit: usize) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Use fuzzy search to find similar base commands
    if let Ok(matches) = fuzzy::search_base_command(conn, partial, limit) {
        for (text, score) in matches {
            // Convert fuzzy score to suggestion score
            // Fuzzy matches are lower priority than exact hierarchy matches
            let suggestion_score = score * 0.5; // Penalize fuzzy matches

            suggestions.push(Suggestion {
                text,
                source: SuggestionSource::FuzzySearch,
                score: suggestion_score,
                metadata: SuggestionMetadata {
                    fuzzy_score: Some(score),
                    ..Default::default()
                },
            });
        }
    }

    suggestions
}

/// Suggests based on historical command frequency.
///
/// Queries the most frequently used commands as a fallback when
/// no context-specific suggestions are available.
fn suggest_from_historical_frequency(conn: &Connection, limit: usize) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    // Query most frequently used base commands
    let query = "SELECT t.text, COUNT(*) as freq, MAX(c.timestamp) as last_seen
                 FROM ci_commands c
                 JOIN ci_tokens t ON t.id = c.base_command_id
                 WHERE c.base_command_id IS NOT NULL
                 GROUP BY c.base_command_id
                 ORDER BY freq DESC
                 LIMIT ?1";

    if let Ok(mut stmt) = conn.prepare(query) {
        if let Ok(rows) = stmt.query_map(rusqlite::params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, i64>(2)?,
            ))
        }) {
            for row in rows.flatten() {
                let (text, freq, last_seen) = row;

                let score = scoring::compute_score(
                    freq,
                    last_seen,
                    None,
                    ContextMatch::Generic,
                    SuggestionSource::HistoricalFrequency,
                );

                suggestions.push(Suggestion {
                    text,
                    source: SuggestionSource::HistoricalFrequency,
                    score,
                    metadata: SuggestionMetadata {
                        frequency: freq,
                        last_seen: Some(last_seen),
                        ..Default::default()
                    },
                });
            }
        }
    }

    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::db_schema;
    use crate::intelligence::schema_provider::tests::TestSchemaProvider;
    use crate::intelligence::tokenizer::compute_command_hash;
    use crate::intelligence::types::AnalyzedToken;
    use command_schema_core::{
        CommandSchema, FlagSchema, SchemaSource, SubcommandSchema, ValueType,
    };

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db_schema::create_schema(&conn).unwrap();
        conn
    }

    fn empty_provider() -> TestSchemaProvider {
        TestSchemaProvider::new()
    }

    fn bootstrap_provider() -> TestSchemaProvider {
        let mut git = CommandSchema::new("git", SchemaSource::Bootstrap);
        let mut remote = SubcommandSchema::new("remote");
        remote.subcommands = vec![
            SubcommandSchema::new("add"),
            SubcommandSchema::new("remove"),
        ];
        git.subcommands = vec![
            SubcommandSchema::new("commit"),
            SubcommandSchema::new("push"),
            SubcommandSchema::new("pull"),
            remote,
        ];

        TestSchemaProvider::from_schemas(vec![git])
    }

    #[test]
    fn test_suggest_empty_context() {
        let conn = setup_test_db();
        let provider = empty_provider();
        let context = SuggestionContext::default();

        // Bootstrap the database with some commands
        let bootstrap = bootstrap_provider();
        crate::intelligence::bootstrap::bootstrap_if_empty(&conn, &bootstrap).unwrap();

        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);
        // Should return suggestions from bootstrapped hierarchy
        assert!(!suggestions.is_empty());
    }

    #[test]
    fn test_suggest_with_preceding() {
        let conn = setup_test_db();
        let provider = empty_provider();

        // Bootstrap the database
        let bootstrap = bootstrap_provider();
        crate::intelligence::bootstrap::bootstrap_if_empty(&conn, &bootstrap).unwrap();

        let context = SuggestionContext {
            preceding_tokens: vec![AnalyzedToken::new(
                "git",
                crate::chrome::command_edit::TokenType::Command,
                0,
            )],
            position: PositionType::Subcommand,
            ..Default::default()
        };

        // Request enough to include all git subcommands (22 bootstrapped)
        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 30);
        // Should return git subcommands from bootstrapped hierarchy
        assert!(!suggestions.is_empty());
        // Verify we get git subcommands
        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert!(
            texts.contains(&"commit"),
            "expected 'commit' in {:?}",
            texts
        );
        assert!(texts.contains(&"push"), "expected 'push' in {:?}", texts);
        assert!(texts.contains(&"pull"), "expected 'pull' in {:?}", texts);
    }

    #[test]
    fn test_gather_suggestions_deduplicates() {
        let suggestions = vec![
            Suggestion::new("git", SuggestionSource::LearnedHierarchy, 2.0),
            Suggestion::new("git", SuggestionSource::LearnedSequence, 1.0),
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
            Suggestion::new(
                "cargo build --broken",
                SuggestionSource::LearnedSequence,
                1.0,
            ),
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
    fn test_hierarchy_returns_tokens_not_full_commands() {
        let conn = setup_test_db();
        let provider = empty_provider();

        // Bootstrap to get initial data
        let bootstrap = bootstrap_provider();
        crate::intelligence::bootstrap::bootstrap_if_empty(&conn, &bootstrap).unwrap();

        // Context: git remote add (position 3)
        let context = SuggestionContext {
            preceding_tokens: vec![
                AnalyzedToken::new("git", crate::chrome::command_edit::TokenType::Command, 0),
                AnalyzedToken::new(
                    "remote",
                    crate::chrome::command_edit::TokenType::Subcommand,
                    1,
                ),
                AnalyzedToken::new("add", crate::chrome::command_edit::TokenType::Argument, 2),
            ],
            partial: String::new(),
            position: PositionType::Argument,
            ..Default::default()
        };

        let suggestions = gather_suggestions(&conn, &provider, SchemaMode::SchemaEnabled, &context);

        // All suggestions should be individual tokens, not full commands
        for suggestion in &suggestions {
            // A full command would contain spaces
            assert!(
                !suggestion.text.contains(' '),
                "Suggestion '{}' should be a single token, not a full command",
                suggestion.text
            );
        }
    }

    #[test]
    fn test_hierarchy_provides_position_aware_suggestions() {
        let conn = setup_test_db();
        let provider = empty_provider();
        let now = chrono::Utc::now().timestamp();

        // Create tokens
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (1, 'git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (2, 'remote', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (3, 'add', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen) VALUES (4, 'origin', 'Argument', ?1, ?1)",
            [now],
        ).unwrap();

        // Create hierarchy: git -> remote -> add -> origin
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (1, 0, NULL, 1, 100, 90, ?1, 'command')",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (2, 1, 1, 1, 50, 45, ?1, 'subcommand')",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (3, 2, 2, 1, 30, 28, ?1, 'subcommand')",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_command_hierarchy (token_id, position, parent_token_id, base_command_id, frequency, success_count, last_seen, role)
             VALUES (4, 3, 3, 1, 20, 18, ?1, 'argument')",
            [now],
        ).unwrap();

        // Query position 3 (after git remote add)
        let context = SuggestionContext {
            preceding_tokens: vec![
                AnalyzedToken::new("git", crate::chrome::command_edit::TokenType::Command, 0),
                AnalyzedToken::new(
                    "remote",
                    crate::chrome::command_edit::TokenType::Subcommand,
                    1,
                ),
                AnalyzedToken::new("add", crate::chrome::command_edit::TokenType::Argument, 2),
            ],
            partial: String::new(),
            position: PositionType::Argument,
            ..Default::default()
        };

        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);

        // Should get 'origin' as a suggestion
        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert!(
            texts.contains(&"origin"),
            "Expected 'origin' in suggestions, got: {:?}",
            texts
        );
    }

    #[test]
    fn test_schema_suggests_subcommands_without_learned_hierarchy() {
        let conn = setup_test_db();

        let mut schema = command_schema_core::CommandSchema::new("tool", SchemaSource::Bootstrap);
        schema.subcommands.push(SubcommandSchema::new("build"));
        schema.subcommands.push(SubcommandSchema::new("deploy"));
        let provider = TestSchemaProvider::from_schemas(vec![schema]);

        let context = SuggestionContext {
            preceding_tokens: vec![AnalyzedToken::new(
                "tool",
                crate::chrome::command_edit::TokenType::Command,
                0,
            )],
            position: PositionType::Subcommand,
            ..Default::default()
        };

        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);
        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert!(texts.contains(&"build"), "expected 'build' in {:?}", texts);
        assert!(
            texts.contains(&"deploy"),
            "expected 'deploy' in {:?}",
            texts
        );
    }

    #[test]
    fn test_schema_suggests_nested_subcommands() {
        let conn = setup_test_db();

        let mut schema = command_schema_core::CommandSchema::new("git", SchemaSource::Bootstrap);
        let mut remote = SubcommandSchema::new("remote");
        remote.subcommands.push(SubcommandSchema::new("add"));
        remote.subcommands.push(SubcommandSchema::new("remove"));
        schema.subcommands.push(remote);
        let provider = TestSchemaProvider::from_schemas(vec![schema]);

        let context = SuggestionContext {
            preceding_tokens: vec![
                AnalyzedToken::new("git", crate::chrome::command_edit::TokenType::Command, 0),
                AnalyzedToken::new(
                    "remote",
                    crate::chrome::command_edit::TokenType::Subcommand,
                    1,
                ),
            ],
            position: PositionType::Argument,
            ..Default::default()
        };

        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);
        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert!(texts.contains(&"add"), "expected 'add' in {:?}", texts);
        assert!(
            texts.contains(&"remove"),
            "expected 'remove' in {:?}",
            texts
        );
    }

    #[test]
    fn test_schema_suggests_choice_values_for_flag_value_position() {
        let conn = setup_test_db();

        let mut schema = command_schema_core::CommandSchema::new("tool", SchemaSource::Bootstrap);
        schema.global_flags.push(FlagSchema::with_value(
            None,
            Some("--format"),
            ValueType::Choice(vec!["json".to_string(), "yaml".to_string()]),
        ));
        let provider = TestSchemaProvider::from_schemas(vec![schema]);

        let context = SuggestionContext {
            preceding_tokens: vec![
                AnalyzedToken::new("tool", crate::chrome::command_edit::TokenType::Command, 0),
                AnalyzedToken::new("--format", crate::chrome::command_edit::TokenType::Flag, 1),
            ],
            position: PositionType::FlagValue {
                flag: "--format".to_string(),
            },
            ..Default::default()
        };

        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);
        let texts: Vec<&str> = suggestions.iter().map(|s| s.text.as_str()).collect();
        assert!(texts.contains(&"json"), "expected 'json' in {:?}", texts);
        assert!(texts.contains(&"yaml"), "expected 'yaml' in {:?}", texts);
    }

    #[test]
    fn test_session_transition_source_is_used_in_main_suggest_pipeline() {
        let conn = setup_test_db();
        let provider = empty_provider();
        let now = chrono::Utc::now().timestamp();
        let from_command = "git status";
        let to_command = "git commit -m test";
        let from_hash = compute_command_hash(from_command);
        let to_hash = compute_command_hash(to_command);

        conn.execute(
            "INSERT INTO ci_commands
             (command_line, command_hash, token_ids, token_count, timestamp)
             VALUES (?1, ?2, '[]', 0, ?3)",
            rusqlite::params![to_command, to_hash, now],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO ci_transitions
             (from_command_hash, to_command_hash, frequency, last_seen)
             VALUES (?1, ?2, 4, ?3)",
            rusqlite::params![from_hash, to_hash, now],
        )
        .unwrap();

        let context = SuggestionContext {
            position: PositionType::Command,
            last_command: Some(from_command.to_string()),
            ..Default::default()
        };

        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);
        let transition = suggestions
            .iter()
            .find(|s| s.text == to_command && s.source == SuggestionSource::SessionTransition);
        assert!(
            transition.is_some(),
            "expected session transition suggestion in {:?}",
            suggestions
                .iter()
                .map(|s| (&s.text, s.source))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_history_only_mode_suppresses_schema_suggestions() {
        let conn = setup_test_db();

        let mut schema = command_schema_core::CommandSchema::new("mytool", SchemaSource::Bootstrap);
        schema
            .subcommands
            .push(SubcommandSchema::new("schema-only-sub"));
        let provider = TestSchemaProvider::from_schemas(vec![schema]);

        let context = SuggestionContext {
            preceding_tokens: vec![AnalyzedToken::new(
                "mytool",
                crate::chrome::command_edit::TokenType::Command,
                0,
            )],
            position: PositionType::Subcommand,
            ..Default::default()
        };

        // With SchemaEnabled, schema suggestions should appear
        let enabled = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);
        assert!(
            enabled.iter().any(|s| s.text == "schema-only-sub"),
            "SchemaEnabled should include schema suggestions, got: {:?}",
            enabled.iter().map(|s| &s.text).collect::<Vec<_>>()
        );

        // With HistoryOnly, schema suggestions should be suppressed
        let history_only = suggest(&conn, &provider, SchemaMode::HistoryOnly, &context, 10);
        assert!(
            !history_only.iter().any(|s| s.text == "schema-only-sub"),
            "HistoryOnly should suppress schema suggestions, got: {:?}",
            history_only.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_template_source_suggests_next_token_in_main_pipeline() {
        let conn = setup_test_db();
        let provider = empty_provider();
        let now = chrono::Utc::now().timestamp();
        let template = "git checkout <BRANCH>";
        let template_hash = compute_command_hash(template);

        conn.execute(
            "INSERT INTO ci_templates
             (template, template_hash, base_command_id, placeholder_count, placeholders, frequency, last_seen, example_command)
             VALUES (?1, ?2, NULL, 1, '[]', 6, ?3, 'git checkout main')",
            rusqlite::params![template, template_hash, now],
        )
        .unwrap();

        let context = SuggestionContext {
            preceding_tokens: vec![AnalyzedToken::new(
                "git",
                crate::chrome::command_edit::TokenType::Command,
                0,
            )],
            partial: "ch".to_string(),
            position: PositionType::Subcommand,
            ..Default::default()
        };

        let suggestions = suggest(&conn, &provider, SchemaMode::SchemaEnabled, &context, 10);
        let template_suggestion = suggestions
            .iter()
            .find(|s| s.text == "checkout" && s.source == SuggestionSource::Template);
        assert!(
            template_suggestion.is_some(),
            "expected template-derived token suggestion in {:?}",
            suggestions
                .iter()
                .map(|s| (&s.text, s.source))
                .collect::<Vec<_>>()
        );
        assert!(
            template_suggestion
                .and_then(|s| s.metadata.template_preview.as_deref())
                .is_some(),
            "template suggestion should preserve preview metadata"
        );
    }
}
