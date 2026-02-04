//! Export and import functionality for pattern sharing.

use rusqlite::Connection;
use tracing::info;

use super::error::CIError;
use super::types::{
    ConflictResolution, ExportOptions, ExportedFlagValue, ExportedPatterns, ExportedPipeChain,
    ExportedSequence, ExportedTemplate, ImportMode, ImportOptions, ImportStats, PatternExport,
    Placeholder, UserAlias, UserPattern,
};

/// Exports patterns to JSON.
pub fn export(conn: &Connection, options: ExportOptions) -> Result<String, CIError> {
    let mut patterns = ExportedPatterns::default();

    // Export sequences
    if options.include_learned_patterns {
        patterns.sequences = export_sequences(conn, options.min_frequency)?;
        patterns.pipe_chains = export_pipe_chains(conn, options.min_frequency)?;
        patterns.flag_values = export_flag_values(conn, options.min_frequency)?;
        patterns.templates = export_templates(conn, options.min_frequency)?;
    }

    // Export user patterns
    if options.include_user_patterns {
        patterns.user_patterns = export_user_patterns(conn)?;
        patterns.user_aliases = export_user_aliases(conn)?;
    }

    // Anonymize paths if requested
    if options.anonymize_paths {
        anonymize_patterns(&mut patterns);
    }

    let export = PatternExport {
        version: "1.0".to_string(),
        exported_at: chrono::Utc::now().timestamp(),
        machine_id: None,
        patterns,
    };

    let json = serde_json::to_string_pretty(&export)?;

    info!(
        sequences = export.patterns.sequences.len(),
        pipe_chains = export.patterns.pipe_chains.len(),
        templates = export.patterns.templates.len(),
        user_patterns = export.patterns.user_patterns.len(),
        "Exported patterns"
    );

    Ok(json)
}

/// Exports learned sequences.
fn export_sequences(conn: &Connection, min_frequency: u32) -> Result<Vec<ExportedSequence>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT ct.text, s.context_position, bt.text, nt.text, s.frequency, s.success_count
         FROM ci_sequences s
         JOIN ci_tokens ct ON ct.id = s.context_token_id
         JOIN ci_tokens nt ON nt.id = s.next_token_id
         LEFT JOIN ci_tokens bt ON bt.id = s.base_command_id
         WHERE s.frequency >= ?1
         ORDER BY s.frequency DESC
         LIMIT 10000"
    )?;

    let rows = stmt.query_map([min_frequency], |row| {
        Ok(ExportedSequence {
            context_token: row.get(0)?,
            context_position: row.get(1)?,
            base_command: row.get(2)?,
            next_token: row.get(3)?,
            frequency: row.get(4)?,
            success_count: row.get(5)?,
        })
    })?;

    let mut sequences = Vec::new();
    for row in rows.flatten() {
        sequences.push(row);
    }

    Ok(sequences)
}

/// Exports pipe chains.
fn export_pipe_chains(conn: &Connection, min_frequency: u32) -> Result<Vec<ExportedPipeChain>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT bt.text, pt.text, p.full_chain, p.frequency
         FROM ci_pipe_chains p
         JOIN ci_tokens pt ON pt.id = p.pipe_command_id
         LEFT JOIN ci_tokens bt ON bt.id = p.pre_pipe_base_cmd_id
         WHERE p.frequency >= ?1
         ORDER BY p.frequency DESC
         LIMIT 10000"
    )?;

    let rows = stmt.query_map([min_frequency], |row| {
        Ok(ExportedPipeChain {
            pre_pipe_base_cmd: row.get(0)?,
            pipe_command: row.get(1)?,
            full_chain: row.get(2)?,
            frequency: row.get(3)?,
        })
    })?;

    let mut chains = Vec::new();
    for row in rows.flatten() {
        chains.push(row);
    }

    Ok(chains)
}

/// Exports flag values.
fn export_flag_values(conn: &Connection, min_frequency: u32) -> Result<Vec<ExportedFlagValue>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT bt.text, st.text, f.flag_text, f.value_text, f.value_type, f.frequency
         FROM ci_flag_values f
         JOIN ci_tokens bt ON bt.id = f.base_command_id
         LEFT JOIN ci_tokens st ON st.id = f.subcommand_id
         WHERE f.frequency >= ?1
         ORDER BY f.frequency DESC
         LIMIT 10000"
    )?;

    let rows = stmt.query_map([min_frequency], |row| {
        Ok(ExportedFlagValue {
            base_command: row.get(0)?,
            subcommand: row.get(1)?,
            flag: row.get(2)?,
            value: row.get(3)?,
            value_type: row.get(4)?,
            frequency: row.get(5)?,
        })
    })?;

    let mut values = Vec::new();
    for row in rows.flatten() {
        values.push(row);
    }

    Ok(values)
}

/// Exports templates.
fn export_templates(conn: &Connection, min_frequency: u32) -> Result<Vec<ExportedTemplate>, CIError> {
    let mut stmt = conn.prepare(
        "SELECT t.template, bt.text, t.placeholders, t.frequency, t.example_command
         FROM ci_templates t
         LEFT JOIN ci_tokens bt ON bt.id = t.base_command_id
         WHERE t.frequency >= ?1
         ORDER BY t.frequency DESC
         LIMIT 1000"
    )?;

    let rows = stmt.query_map([min_frequency], |row| {
        let template: String = row.get(0)?;
        let base_command: Option<String> = row.get(1)?;
        let placeholders_json: String = row.get(2)?;
        let frequency: u32 = row.get(3)?;
        let example: Option<String> = row.get(4)?;

        let placeholders: Vec<Placeholder> =
            serde_json::from_str(&placeholders_json).unwrap_or_default();

        Ok(ExportedTemplate {
            template,
            base_command,
            placeholders,
            frequency,
            example,
        })
    })?;

    let mut templates = Vec::new();
    for row in rows.flatten() {
        templates.push(row);
    }

    Ok(templates)
}

/// Exports user patterns.
fn export_user_patterns(conn: &Connection) -> Result<Vec<UserPattern>, CIError> {
    super::user_patterns::list_patterns(conn, None)
}

/// Exports user aliases.
fn export_user_aliases(conn: &Connection) -> Result<Vec<UserAlias>, CIError> {
    super::user_patterns::list_aliases(conn)
}

/// Anonymizes paths in patterns.
fn anonymize_patterns(patterns: &mut ExportedPatterns) {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return;
    }

    // Anonymize flag values
    for fv in &mut patterns.flag_values {
        if fv.value.starts_with(&home) {
            fv.value = fv.value.replace(&home, "~");
        }
    }

    // Anonymize templates
    for t in &mut patterns.templates {
        if let Some(ref mut example) = t.example {
            if example.contains(&home) {
                *example = example.replace(&home, "~");
            }
        }
    }
}

// ============================================================================
// Import
// ============================================================================

/// Imports patterns from JSON.
///
/// All imports are performed within a single transaction to ensure atomicity.
/// If any critical error occurs, the entire import is rolled back.
pub fn import(conn: &Connection, json: &str, options: ImportOptions) -> Result<ImportStats, CIError> {
    let export: PatternExport = serde_json::from_str(json)?;

    // Validate version
    if !export.version.starts_with("1.") {
        return Err(CIError::InvalidImport(format!(
            "Unsupported export version: {}",
            export.version
        )));
    }

    let mut stats = ImportStats::default();

    // Start transaction for atomic import
    conn.execute_batch("BEGIN TRANSACTION")?;

    let result = import_within_transaction(conn, &export, &options, &mut stats);

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            info!(
                sequences = stats.sequences_imported,
                pipe_chains = stats.pipe_chains_imported,
                templates = stats.templates_imported,
                user_patterns = stats.user_patterns_imported,
                skipped = stats.skipped,
                "Imported patterns"
            );
            Ok(stats)
        }
        Err(e) => {
            // Rollback on any error
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Performs the actual import within a transaction context.
fn import_within_transaction(
    conn: &Connection,
    export: &PatternExport,
    options: &ImportOptions,
    stats: &mut ImportStats,
) -> Result<(), CIError> {
    // Handle replace mode
    if options.mode == ImportMode::Replace {
        clear_learned_patterns(conn)?;
        clear_user_patterns(conn)?;
    }

    // Import sequences
    for seq in &export.patterns.sequences {
        match import_sequence(conn, seq, options) {
            Ok(imported) => {
                if imported {
                    stats.sequences_imported += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            Err(_) => {
                stats.skipped += 1;
            }
        }
    }

    // Import pipe chains
    for chain in &export.patterns.pipe_chains {
        match import_pipe_chain(conn, chain, options) {
            Ok(imported) => {
                if imported {
                    stats.pipe_chains_imported += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            Err(_) => {
                stats.skipped += 1;
            }
        }
    }

    // Import templates
    for template in &export.patterns.templates {
        match import_template(conn, template, options) {
            Ok(imported) => {
                if imported {
                    stats.templates_imported += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            Err(_) => {
                stats.skipped += 1;
            }
        }
    }

    // Import user patterns
    for pattern in &export.patterns.user_patterns {
        match import_user_pattern(conn, pattern, options) {
            Ok(imported) => {
                if imported {
                    stats.user_patterns_imported += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            Err(_) => {
                stats.skipped += 1;
            }
        }
    }

    // Import aliases
    for alias in &export.patterns.user_aliases {
        match import_alias(conn, alias, options) {
            Ok(imported) => {
                if imported {
                    stats.user_patterns_imported += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            Err(_) => {
                stats.skipped += 1;
            }
        }
    }

    Ok(())
}

/// Clears all learned patterns.
fn clear_learned_patterns(conn: &Connection) -> Result<(), CIError> {
    conn.execute("DELETE FROM ci_sequences", [])?;
    conn.execute("DELETE FROM ci_ngrams", [])?;
    conn.execute("DELETE FROM ci_pipe_chains", [])?;
    conn.execute("DELETE FROM ci_flag_values", [])?;
    conn.execute("DELETE FROM ci_templates", [])?;
    conn.execute("DELETE FROM ci_template_values", [])?;
    Ok(())
}

/// Clears all user patterns.
fn clear_user_patterns(conn: &Connection) -> Result<(), CIError> {
    conn.execute("DELETE FROM ci_user_patterns", [])?;
    conn.execute("DELETE FROM ci_user_aliases", [])?;
    Ok(())
}

/// Imports a sequence.
fn import_sequence(
    conn: &Connection,
    seq: &ExportedSequence,
    options: &ImportOptions,
) -> Result<bool, CIError> {
    let now = chrono::Utc::now().timestamp();

    // Get or create tokens
    let context_id = get_or_create_import_token(conn, &seq.context_token, "Argument", now)?;
    let next_id = get_or_create_import_token(conn, &seq.next_token, "Argument", now)?;
    let base_id = seq
        .base_command
        .as_ref()
        .map(|cmd| get_or_create_import_token(conn, cmd, "Command", now))
        .transpose()?;

    // Check for existing
    let existing: Option<i64> = conn
        .query_row(
            "SELECT frequency FROM ci_sequences
             WHERE context_token_id = ?1 AND context_position = ?2
               AND base_command_id IS ?3 AND next_token_id = ?4",
            rusqlite::params![context_id, seq.context_position, base_id, next_id],
            |row| row.get(0),
        )
        .ok();

    if let Some(_existing_freq) = existing {
        match options.conflict_resolution {
            ConflictResolution::KeepExisting => return Ok(false),
            ConflictResolution::UseImported => {
                conn.execute(
                    "UPDATE ci_sequences SET frequency = ?1, success_count = ?2, last_seen = ?3
                     WHERE context_token_id = ?4 AND context_position = ?5
                       AND base_command_id IS ?6 AND next_token_id = ?7",
                    rusqlite::params![
                        seq.frequency,
                        seq.success_count,
                        now,
                        context_id,
                        seq.context_position,
                        base_id,
                        next_id,
                    ],
                )?;
            }
            ConflictResolution::MergeFrequency => {
                conn.execute(
                    "UPDATE ci_sequences SET frequency = frequency + ?1,
                     success_count = success_count + ?2, last_seen = ?3
                     WHERE context_token_id = ?4 AND context_position = ?5
                       AND base_command_id IS ?6 AND next_token_id = ?7",
                    rusqlite::params![
                        seq.frequency,
                        seq.success_count,
                        now,
                        context_id,
                        seq.context_position,
                        base_id,
                        next_id,
                    ],
                )?;
            }
        }
    } else {
        conn.execute(
            "INSERT INTO ci_sequences
             (context_token_id, context_position, base_command_id, next_token_id,
              frequency, success_count, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                context_id,
                seq.context_position,
                base_id,
                next_id,
                seq.frequency,
                seq.success_count,
                now,
            ],
        )?;
    }

    Ok(true)
}

/// Imports a pipe chain.
fn import_pipe_chain(
    conn: &Connection,
    chain: &ExportedPipeChain,
    _options: &ImportOptions,
) -> Result<bool, CIError> {
    let now = chrono::Utc::now().timestamp();

    let base_id = chain
        .pre_pipe_base_cmd
        .as_ref()
        .map(|cmd| get_or_create_import_token(conn, cmd, "Command", now))
        .transpose()?;

    let pipe_cmd_id = get_or_create_import_token(conn, &chain.pipe_command, "Command", now)?;

    // Generate hash
    let pre_pipe_hash = super::tokenizer::compute_command_hash(
        chain.pre_pipe_base_cmd.as_deref().unwrap_or(""),
    );

    // Insert
    conn.execute(
        "INSERT OR IGNORE INTO ci_pipe_chains
         (pre_pipe_base_cmd_id, pre_pipe_hash, pipe_command_id, full_chain, frequency, last_seen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![base_id, pre_pipe_hash, pipe_cmd_id, chain.full_chain, chain.frequency, now],
    )?;

    Ok(true)
}

/// Imports a template.
fn import_template(
    conn: &Connection,
    template: &ExportedTemplate,
    _options: &ImportOptions,
) -> Result<bool, CIError> {
    let now = chrono::Utc::now().timestamp();

    let base_id = template
        .base_command
        .as_ref()
        .map(|cmd| get_or_create_import_token(conn, cmd, "Command", now))
        .transpose()?;

    let template_hash = super::tokenizer::compute_command_hash(&template.template);
    let placeholders_json = serde_json::to_string(&template.placeholders)?;

    conn.execute(
        "INSERT OR IGNORE INTO ci_templates
         (template, template_hash, base_command_id, placeholder_count, placeholders,
          frequency, last_seen, example_command)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            template.template,
            template_hash,
            base_id,
            template.placeholders.len(),
            placeholders_json,
            template.frequency,
            now,
            template.example,
        ],
    )?;

    Ok(true)
}

/// Imports a user pattern.
fn import_user_pattern(
    conn: &Connection,
    pattern: &UserPattern,
    options: &ImportOptions,
) -> Result<bool, CIError> {
    // Check for existing by trigger
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_user_patterns WHERE trigger_pattern = ?1",
            [&pattern.trigger],
            |row| row.get(0),
        )
        .ok();

    if existing.is_some() && options.conflict_resolution == ConflictResolution::KeepExisting {
        return Ok(false);
    }

    super::user_patterns::add_pattern(conn, pattern.clone())?;
    Ok(true)
}

/// Imports an alias.
fn import_alias(
    conn: &Connection,
    alias: &UserAlias,
    options: &ImportOptions,
) -> Result<bool, CIError> {
    // Check for existing
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_user_aliases WHERE alias = ?1",
            [&alias.alias],
            |row| row.get(0),
        )
        .ok();

    if existing.is_some() && options.conflict_resolution == ConflictResolution::KeepExisting {
        return Ok(false);
    }

    super::user_patterns::add_alias(conn, &alias.alias, &alias.expansion, alias.description.as_deref())?;
    Ok(true)
}

/// Gets or creates a token for import.
fn get_or_create_import_token(
    conn: &Connection,
    text: &str,
    token_type: &str,
    timestamp: i64,
) -> Result<i64, CIError> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM ci_tokens WHERE text = ?1",
            [text],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO ci_tokens (text, token_type, frequency, first_seen, last_seen)
         VALUES (?1, ?2, 1, ?3, ?3)",
        rusqlite::params![text, token_type, timestamp],
    )?;

    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::db_schema;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db_schema::create_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn test_export_empty() {
        let conn = setup_test_db();
        let options = ExportOptions::default();

        let json = export(&conn, options).unwrap();
        let parsed: PatternExport = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.version, "1.0");
        assert!(parsed.patterns.sequences.is_empty());
    }

    #[test]
    fn test_export_import_roundtrip() {
        let conn = setup_test_db();

        // Add some test data
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen)
             VALUES (1, 'git', 'Command', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_tokens (id, text, token_type, first_seen, last_seen)
             VALUES (2, 'commit', 'Subcommand', ?1, ?1)",
            [now],
        ).unwrap();
        conn.execute(
            "INSERT INTO ci_sequences (context_token_id, context_position, base_command_id, next_token_id, frequency, success_count, last_seen)
             VALUES (1, 0, 1, 2, 10, 8, ?1)",
            [now],
        ).unwrap();

        // Export
        let options = ExportOptions {
            include_learned_patterns: true,
            include_user_patterns: true,
            min_frequency: 0,
            anonymize_paths: false,
        };
        let json = export(&conn, options).unwrap();

        // Create new DB and import
        let conn2 = setup_test_db();
        let import_options = ImportOptions {
            mode: ImportMode::Merge,
            conflict_resolution: ConflictResolution::UseImported,
        };
        let stats = import(&conn2, &json, import_options).unwrap();

        assert!(stats.sequences_imported > 0);
    }
}
