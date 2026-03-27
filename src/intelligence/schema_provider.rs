//! Trait-based schema provider abstraction.
//!
//! This module defines the [`SchemaProvider`] trait and provides two
//! implementations gated behind the `command-schema` Cargo feature:
//!
//! - [`FullSchemaProvider`] — backed by `command-schema-db` (bundled),
//!   `command-schema-sqlite` (learned), and `command-schema-discovery`
//!   (manual scan).
//! - `StubSchemaProvider` — zero-cost stub when the feature is compiled out.
//!
//! The lookup order for `FullSchemaProvider` is:
//! **learned** (SQLite `cs_*` tables) → **bundled** (in-memory `SchemaDatabase`) → `None`.

use command_schema_core::CommandSchema;

use super::error::CIError;

/// Result of a schema search query.
#[derive(Debug, Clone)]
pub struct SchemaSearchResult {
    pub command: String,
    pub description: Option<String>,
    pub source: SchemaSearchSource,
    pub confidence: f64,
    pub subcommand_count: usize,
    pub flag_count: usize,
}

/// Where a search result came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaSearchSource {
    Learned,
    Bundled,
}

/// Controls how command schemas feed into the suggestion engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaMode {
    /// Only learned history patterns (ci_* tables). No schema enrichment.
    HistoryOnly,
    /// Schemas feed into suggestions. Manual discovery available.
    #[default]
    SchemaEnabled,
    /// Full bundled library loaded. Schema browser can search all commands.
    /// Falls back to SchemaEnabled if bundled-schemas feature not compiled.
    FullLibrary,
}

impl SchemaMode {
    /// Returns true if schema-based suggestions should be generated.
    pub fn uses_schemas(&self) -> bool {
        matches!(self, Self::SchemaEnabled | Self::FullLibrary)
    }

    /// Serializes to a settings-compatible string.
    pub fn as_setting(&self) -> &'static str {
        match self {
            Self::HistoryOnly => "history-only",
            Self::SchemaEnabled => "schema-enabled",
            Self::FullLibrary => "full-library",
        }
    }

    /// Deserializes from a settings string.
    ///
    /// Unknown values fall back to `SchemaEnabled` for forward-compatibility.
    pub fn from_setting(s: &str) -> Self {
        match s {
            "history-only" => Self::HistoryOnly,
            "schema-enabled" => Self::SchemaEnabled,
            "full-library" => Self::FullLibrary,
            other => {
                tracing::debug!(
                    value = other,
                    "Unknown schema mode setting, defaulting to SchemaEnabled"
                );
                Self::SchemaEnabled
            }
        }
    }
}

/// Trait abstracting schema lookup, discovery, storage, and search.
pub trait SchemaProvider: Send {
    /// Returns a schema for a base command, if available.
    fn get(&self, command: &str) -> Option<&CommandSchema>;

    /// Returns true if a schema exists for the given command.
    fn has(&self, command: &str) -> bool {
        self.get(command).is_some()
    }

    /// Returns true if the given command has a bundled (authoritative) schema.
    fn is_bundled(&self, command: &str) -> bool;

    /// Iterates over all known command names.
    fn commands(&self) -> Box<dyn Iterator<Item = &str> + '_>;

    /// Iterates over all known schemas.
    fn all_schemas(&self) -> Box<dyn Iterator<Item = &CommandSchema> + '_>;

    /// Searches schemas by command name or description substring.
    fn search(&self, query: &str) -> Vec<SchemaSearchResult>;

    /// Discovers a command's schema by running `--help` and parsing the output.
    /// This is a manual trigger only — never called automatically.
    fn discover(&mut self, command: &str) -> Result<CommandSchema, CIError>;

    /// Stores a learned schema, persisting to SQLite `cs_*` tables when available.
    ///
    /// The `FullSchemaProvider` implementation auto-persists to the database
    /// for cross-session durability. If persistence fails, the in-memory cache
    /// is still updated and the error is logged.
    fn store(&mut self, schema: CommandSchema) -> Result<(), CIError>;

    /// Inserts or updates a schema as a runtime overlay for non-bundled commands.
    fn add_overlay(&mut self, schema: CommandSchema);

    /// Returns the total number of available schemas.
    fn schema_count(&self) -> usize;

    /// Returns true if a bundled schema database is loaded.
    fn is_bundled_available(&self) -> bool;
}

// ============================================================================
// Full implementation (command-schema feature enabled)
// ============================================================================

#[cfg(feature = "command-schema")]
mod full {
    use std::collections::HashMap;

    use command_schema_core::CommandSchema;
    use command_schema_sqlite::{Migration, SchemaQuery};
    use rusqlite::Connection;
    #[cfg(feature = "bundled-schemas")]
    use tracing::warn;
    use tracing::{debug, info};

    use super::{SchemaMode, SchemaProvider, SchemaSearchResult, SchemaSearchSource};
    use crate::intelligence::error::CIError;

    /// Full schema provider backed by command-schema crates.
    ///
    /// Lookup order: learned → bundled → None.
    pub struct FullSchemaProvider {
        /// Bundled schemas from `command-schema-db`.
        #[cfg(feature = "bundled-schemas")]
        bundled: Option<command_schema_db::SchemaDatabase>,

        /// Learned schemas populated from cs_* tables, discovery, or import.
        learned: HashMap<String, CommandSchema>,

        /// Database path for automatic persistence in `store()`.
        db_path: Option<std::path::PathBuf>,
    }

    impl FullSchemaProvider {
        /// Creates a new provider, running cs_* table migration and loading
        /// any previously learned schemas from SQLite.
        pub fn new(conn: &Connection, mode: SchemaMode) -> Result<Self, CIError> {
            // Ensure cs_* tables exist (idempotent)
            let migration =
                Migration::new(conn, "cs_").map_err(|e| CIError::Internal(e.to_string()))?;
            migration
                .up()
                .map_err(|e| CIError::Internal(e.to_string()))?;

            // Load learned schemas from cs_* tables
            let query =
                SchemaQuery::new(conn, "cs_").map_err(|e| CIError::Internal(e.to_string()))?;
            let learned_schemas = query
                .get_all_schemas()
                .map_err(|e| CIError::Internal(e.to_string()))?;

            let learned: HashMap<String, CommandSchema> = learned_schemas
                .into_iter()
                .map(|s| (s.command.clone(), s))
                .collect();

            if !learned.is_empty() {
                info!(
                    count = learned.len(),
                    "Loaded learned schemas from cs_* tables"
                );
            }

            // Load bundled schemas if available and mode requests it
            #[cfg(not(feature = "bundled-schemas"))]
            let _ = mode;
            #[cfg(feature = "bundled-schemas")]
            let bundled = if mode == SchemaMode::FullLibrary {
                match command_schema_db::SchemaDatabase::bundled() {
                    Ok(db) => {
                        info!(count = db.len(), "Loaded bundled schema database");
                        Some(db)
                    }
                    Err(e) => {
                        warn!("Failed to load bundled schemas: {e}");
                        None
                    }
                }
            } else {
                None
            };

            let db_path = conn.path().map(std::path::PathBuf::from);

            Ok(Self {
                #[cfg(feature = "bundled-schemas")]
                bundled,
                learned,
                db_path,
            })
        }
    }

    /// Persists a single schema to the cs_* SQLite tables.
    ///
    /// Called by `CommandIntelligence` after discovery or import operations
    /// to ensure learned schemas survive restarts. Creates a temporary
    /// `SchemaQuery` for the write operation.
    pub fn persist_schema(conn: &Connection, schema: &CommandSchema) -> Result<(), CIError> {
        let query = SchemaQuery::new(conn, "cs_").map_err(|e| CIError::Internal(e.to_string()))?;

        // Try insert first; if the command already exists, update instead
        match query.insert_schema(schema) {
            Ok(()) => {
                debug!(
                    command = schema.command,
                    "Persisted new schema to cs_* tables"
                );
                Ok(())
            }
            Err(e) => {
                // Only fall back to update for unique-constraint violations (duplicate command).
                // Any other error is unexpected and should propagate.
                let is_constraint = matches!(
                    &e,
                    command_schema_sqlite::SqliteError::DatabaseError(
                        rusqlite::Error::SqliteFailure(err, _)
                    ) if err.code == rusqlite::ErrorCode::ConstraintViolation
                );
                if is_constraint {
                    query
                        .update_schema(schema)
                        .map_err(|e| CIError::Internal(e.to_string()))?;
                    debug!(
                        command = schema.command,
                        "Updated existing schema in cs_* tables"
                    );
                    Ok(())
                } else {
                    Err(CIError::Internal(e.to_string()))
                }
            }
        }
    }

    impl SchemaProvider for FullSchemaProvider {
        fn get(&self, command: &str) -> Option<&CommandSchema> {
            // Learned schemas take priority (they represent user's actual environment)
            if let Some(schema) = self.learned.get(command) {
                return Some(schema);
            }

            // Fall back to bundled
            #[cfg(feature = "bundled-schemas")]
            if let Some(ref db) = self.bundled {
                return db.get(command);
            }

            None
        }

        fn is_bundled(&self, command: &str) -> bool {
            #[cfg(feature = "bundled-schemas")]
            if let Some(ref db) = self.bundled {
                return db.get(command).is_some();
            }
            let _ = command;
            false
        }

        fn commands(&self) -> Box<dyn Iterator<Item = &str> + '_> {
            let learned_keys = self.learned.keys().map(String::as_str);

            #[cfg(feature = "bundled-schemas")]
            {
                if let Some(ref db) = self.bundled {
                    let bundled_keys = db
                        .commands()
                        .filter(move |cmd| !self.learned.contains_key(*cmd));
                    return Box::new(learned_keys.chain(bundled_keys));
                }
            }

            Box::new(learned_keys)
        }

        fn all_schemas(&self) -> Box<dyn Iterator<Item = &CommandSchema> + '_> {
            let learned_vals = self.learned.values();

            #[cfg(feature = "bundled-schemas")]
            {
                if let Some(ref db) = self.bundled {
                    let bundled_vals = db.commands().filter_map(move |cmd| {
                        if self.learned.contains_key(cmd) {
                            None
                        } else {
                            db.get(cmd)
                        }
                    });
                    return Box::new(learned_vals.chain(bundled_vals));
                }
            }

            Box::new(learned_vals)
        }

        fn search(&self, query: &str) -> Vec<SchemaSearchResult> {
            let query_lower = query.to_lowercase();
            let mut results = Vec::new();

            for schema in self.all_schemas() {
                let cmd_match = schema.command.to_lowercase().contains(&query_lower);
                let desc_match = schema
                    .description
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase().contains(&query_lower));

                if cmd_match || desc_match {
                    let source = if self.learned.contains_key(&schema.command) {
                        SchemaSearchSource::Learned
                    } else {
                        SchemaSearchSource::Bundled
                    };

                    results.push(SchemaSearchResult {
                        command: schema.command.clone(),
                        description: schema.description.clone(),
                        source,
                        confidence: schema.confidence,
                        subcommand_count: schema.subcommands.len(),
                        flag_count: schema.global_flags.len(),
                    });
                }
            }

            // Pre-compute prefix matches to avoid O(N log N) allocations in the comparator
            let mut tagged: Vec<(bool, SchemaSearchResult)> = results
                .into_iter()
                .map(|r| {
                    let is_prefix = r.command.to_lowercase().starts_with(&query_lower);
                    (is_prefix, r)
                })
                .collect();

            // Sort: prefix matches first, then by confidence
            tagged.sort_by(|(a_prefix, a), (b_prefix, b)| {
                b_prefix.cmp(a_prefix).then(
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            });
            results = tagged.into_iter().map(|(_, r)| r).collect();

            results
        }

        fn discover(&mut self, command: &str) -> Result<CommandSchema, CIError> {
            let result = command_schema_discovery::extractor::extract_command_schema(command);
            if !result.success {
                let detail = result
                    .warnings
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "Unknown extraction failure".to_string());
                return Err(CIError::Internal(format!(
                    "Schema discovery failed for '{command}': {detail}"
                )));
            }

            let schema = result.schema.ok_or_else(|| {
                CIError::Internal(format!("Discovery produced no schema for '{command}'"))
            })?;

            debug!(
                command = command,
                confidence = schema.confidence,
                subcommands = schema.subcommands.len(),
                flags = schema.global_flags.len(),
                "Discovered command schema"
            );

            Ok(schema)
        }

        fn store(&mut self, schema: CommandSchema) -> Result<(), CIError> {
            // Persist to SQLite cs_* tables for cross-session durability
            if let Some(ref path) = self.db_path {
                match Connection::open(path) {
                    Ok(persist_conn) => {
                        if let Err(e) = persist_schema(&persist_conn, &schema) {
                            debug!(command = schema.command, error = %e, "Failed to persist schema (in-memory cache still updated)");
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "Failed to open connection for schema persistence");
                    }
                }
            }

            // Always update in-memory cache
            self.learned.insert(schema.command.clone(), schema);
            Ok(())
        }

        fn add_overlay(&mut self, schema: CommandSchema) {
            // Skip if this command is bundled (bundled schemas are authoritative for structure)
            if self.is_bundled(&schema.command) {
                debug!(
                    command = schema.command,
                    "Skipping overlay for bundled command"
                );
                return;
            }
            self.learned.insert(schema.command.clone(), schema);
        }

        fn schema_count(&self) -> usize {
            let learned = self.learned.len();

            #[cfg(feature = "bundled-schemas")]
            {
                if let Some(ref db) = self.bundled {
                    // Count unique commands across both sources
                    let bundled_unique = db
                        .commands()
                        .filter(|cmd| !self.learned.contains_key(*cmd))
                        .count();
                    return learned + bundled_unique;
                }
            }

            learned
        }

        fn is_bundled_available(&self) -> bool {
            #[cfg(feature = "bundled-schemas")]
            let result = self.bundled.is_some();
            #[cfg(not(feature = "bundled-schemas"))]
            let result = false;
            result
        }
    }
}

// ============================================================================
// Stub implementation (command-schema feature disabled)
// ============================================================================

#[cfg(not(feature = "command-schema"))]
mod stub {
    use command_schema_core::CommandSchema;

    use super::{SchemaProvider, SchemaSearchResult};
    use crate::intelligence::error::CIError;

    /// Zero-cost stub provider when command-schema feature is compiled out.
    #[derive(Default)]
    pub struct StubSchemaProvider;

    impl StubSchemaProvider {
        pub fn new() -> Self {
            Self
        }
    }

    impl SchemaProvider for StubSchemaProvider {
        fn get(&self, _command: &str) -> Option<&CommandSchema> {
            None
        }

        fn is_bundled(&self, _command: &str) -> bool {
            false
        }

        fn commands(&self) -> Box<dyn Iterator<Item = &str> + '_> {
            Box::new(std::iter::empty())
        }

        fn all_schemas(&self) -> Box<dyn Iterator<Item = &CommandSchema> + '_> {
            Box::new(std::iter::empty())
        }

        fn search(&self, _query: &str) -> Vec<SchemaSearchResult> {
            Vec::new()
        }

        fn discover(&mut self, _command: &str) -> Result<CommandSchema, CIError> {
            Err(CIError::Internal(
                "Schema discovery requires the command-schema feature".to_string(),
            ))
        }

        fn store(&mut self, _schema: CommandSchema) -> Result<(), CIError> {
            Ok(())
        }

        fn add_overlay(&mut self, _schema: CommandSchema) {}

        fn schema_count(&self) -> usize {
            0
        }

        fn is_bundled_available(&self) -> bool {
            false
        }
    }
}

// ============================================================================
// Public re-exports
// ============================================================================

#[cfg(feature = "command-schema")]
pub use full::FullSchemaProvider;

#[cfg(feature = "command-schema")]
pub use full::persist_schema;

#[cfg(not(feature = "command-schema"))]
pub use stub::StubSchemaProvider;

/// Type alias for the default provider based on feature flags.
#[cfg(feature = "command-schema")]
pub type DefaultSchemaProvider = FullSchemaProvider;

#[cfg(not(feature = "command-schema"))]
pub type DefaultSchemaProvider = StubSchemaProvider;

#[cfg(test)]
pub(crate) mod tests {
    use command_schema_core::{CommandSchema, SchemaSource};

    use super::*;

    /// A test-only in-memory provider that doesn't need SQLite.
    pub struct TestSchemaProvider {
        schemas: std::collections::HashMap<String, CommandSchema>,
        bundled: std::collections::HashSet<String>,
    }

    impl TestSchemaProvider {
        pub fn new() -> Self {
            Self {
                schemas: std::collections::HashMap::new(),
                bundled: std::collections::HashSet::new(),
            }
        }

        pub fn from_schemas(schemas: Vec<CommandSchema>) -> Self {
            let map = schemas
                .into_iter()
                .map(|s| (s.command.clone(), s))
                .collect();
            Self {
                schemas: map,
                bundled: std::collections::HashSet::new(),
            }
        }

        /// Creates a provider where all schemas are treated as bundled (authoritative).
        pub fn with_bundled(schemas: Vec<CommandSchema>) -> Self {
            let bundled = schemas.iter().map(|s| s.command.clone()).collect();
            let map = schemas
                .into_iter()
                .map(|s| (s.command.clone(), s))
                .collect();
            Self {
                schemas: map,
                bundled,
            }
        }
    }

    impl SchemaProvider for TestSchemaProvider {
        fn get(&self, command: &str) -> Option<&CommandSchema> {
            self.schemas.get(command)
        }

        fn is_bundled(&self, command: &str) -> bool {
            self.bundled.contains(command)
        }

        fn commands(&self) -> Box<dyn Iterator<Item = &str> + '_> {
            Box::new(self.schemas.keys().map(String::as_str))
        }

        fn all_schemas(&self) -> Box<dyn Iterator<Item = &CommandSchema> + '_> {
            Box::new(self.schemas.values())
        }

        fn search(&self, query: &str) -> Vec<SchemaSearchResult> {
            let query_lower = query.to_lowercase();
            self.schemas
                .values()
                .filter(|s| {
                    s.command.to_lowercase().contains(&query_lower)
                        || s.description
                            .as_ref()
                            .is_some_and(|d| d.to_lowercase().contains(&query_lower))
                })
                .map(|s| SchemaSearchResult {
                    command: s.command.clone(),
                    description: s.description.clone(),
                    source: SchemaSearchSource::Learned,
                    confidence: s.confidence,
                    subcommand_count: s.subcommands.len(),
                    flag_count: s.global_flags.len(),
                })
                .collect()
        }

        fn discover(&mut self, _command: &str) -> Result<CommandSchema, CIError> {
            Err(CIError::Internal("Not available in test".to_string()))
        }

        fn store(&mut self, schema: CommandSchema) -> Result<(), CIError> {
            self.schemas.insert(schema.command.clone(), schema);
            Ok(())
        }

        fn add_overlay(&mut self, schema: CommandSchema) {
            self.schemas.insert(schema.command.clone(), schema);
        }

        fn schema_count(&self) -> usize {
            self.schemas.len()
        }

        fn is_bundled_available(&self) -> bool {
            false
        }
    }

    #[test]
    fn test_schema_mode_roundtrip() {
        assert_eq!(
            SchemaMode::from_setting("history-only"),
            SchemaMode::HistoryOnly
        );
        assert_eq!(
            SchemaMode::from_setting("schema-enabled"),
            SchemaMode::SchemaEnabled
        );
        assert_eq!(
            SchemaMode::from_setting("full-library"),
            SchemaMode::FullLibrary
        );
        assert_eq!(
            SchemaMode::from_setting("unknown"),
            SchemaMode::SchemaEnabled
        );
    }

    #[test]
    fn test_schema_mode_uses_schemas() {
        assert!(!SchemaMode::HistoryOnly.uses_schemas());
        assert!(SchemaMode::SchemaEnabled.uses_schemas());
        assert!(SchemaMode::FullLibrary.uses_schemas());
    }

    #[test]
    fn test_test_provider_basic_operations() {
        let mut provider = TestSchemaProvider::new();
        assert_eq!(provider.schema_count(), 0);
        assert!(!provider.has("git"));

        let git = CommandSchema::new("git", SchemaSource::Learned);
        provider.store(git).unwrap();

        assert!(provider.has("git"));
        assert_eq!(provider.schema_count(), 1);
        assert_eq!(provider.get("git").unwrap().command, "git");
    }

    #[test]
    fn test_test_provider_search() {
        let provider = TestSchemaProvider::from_schemas(vec![
            CommandSchema::new("git", SchemaSource::Learned),
            CommandSchema::new("gitk", SchemaSource::Learned),
            CommandSchema::new("cargo", SchemaSource::Learned),
        ]);

        let results = provider.search("git");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_test_provider_commands_iterator() {
        let provider = TestSchemaProvider::from_schemas(vec![
            CommandSchema::new("git", SchemaSource::Learned),
            CommandSchema::new("cargo", SchemaSource::Learned),
        ]);

        let mut cmds: Vec<&str> = provider.commands().collect();
        cmds.sort();
        assert_eq!(cmds, vec!["cargo", "git"]);
    }
}
