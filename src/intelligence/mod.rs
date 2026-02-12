//! Command Intelligence Engine for wrashpty.
//!
//! This module provides intelligent command suggestions using a trait-based
//! schema provider architecture with learned ranking signals from command
//! history. It integrates with reedline's SQLite database using the `ci_*`
//! table prefix for intelligence data and `cs_*` for command schemas.
//!
//! # Architecture
//!
//! The engine combines two sources of intelligence:
//!
//! - **Schema Provider**: A pluggable [`SchemaProvider`] trait backed by the
//!   `command-schema` library (when feature-enabled) or a zero-cost stub.
//!   Provides command tree structure, flags, and value candidates.
//! - **Learned Ranking Signals**: Runtime tables (`ci_command_hierarchy`,
//!   `ci_sequences`, `ci_pipe_chains`, variant success rates, etc.) influence
//!   ordering and personalization from observed usage.
//!
//! Schema mode controls how schemas feed into suggestions:
//! - `HistoryOnly`: Only ci_* learned patterns, no schema enrichment
//! - `SchemaEnabled`: Schemas from discovery + SQLite, manual scan available
//! - `FullLibrary`: Bundled schema library loaded (requires `bundled-schemas` feature)
//!
//! # Features
//!
//! - **Schema Provider**: Trait-based schema access via command-schema library
//! - **Hierarchy Learning**: Position-aware token relationships
//! - **Pattern Learning**: Token sequences, pipe chains, and flag values
//! - **Session Tracking**: Tracks command sequences within terminal sessions
//! - **Template Recognition**: Identifies command templates with placeholders
//! - **Failure Learning**: Prefers successful command variants
//! - **Fuzzy Search**: FTS5-powered typo tolerance
//! - **User Patterns**: Custom aliases and suggestion rules
//! - **Export/Import**: Pattern export/import plus schema-pack export/import
//!
//! # Example
//!
//! ```ignore
//! use wrashpty::intelligence::CommandIntelligence;
//!
//! let conn = Connection::open("history.db")?;
//! let mut ci = CommandIntelligence::new(conn)?;
//!
//! // Sync with reedline history
//! ci.sync()?;
//!
//! // Get suggestions
//! let context = SuggestionContext::default();
//! let suggestions = ci.suggest(&context, 10);
//! ```

pub mod bootstrap;
pub mod db_schema;
pub mod error;
pub mod schema_provider;
pub mod sync;
pub mod tokenizer;
pub mod types;

// Re-export command_schema_core types for convenience
pub use command_schema_core;

// Pattern learning submodule
pub mod patterns;

// Advanced features
pub mod export;
pub mod fuzzy;
pub mod scoring;
pub mod sessions;
pub mod suggest;
pub mod templates;
pub mod user_patterns;
pub mod variants;

use std::collections::HashMap;

use rusqlite::Connection;
use tracing::info;

pub use error::CIError;
pub use schema_provider::{SchemaMode, SchemaProvider};
pub use types::*;

/// The main Command Intelligence Engine.
///
/// This struct manages all intelligence operations and owns the
/// database connection for the intelligence tables.
pub struct CommandIntelligence {
    /// Database connection (shared with reedline).
    conn: Connection,

    /// Token ID cache for performance.
    token_cache: HashMap<String, i64>,

    /// Last synced reedline history ID.
    last_sync_id: i64,

    /// Current session context.
    current_session: Option<SessionContext>,

    /// Pluggable schema provider (FullSchemaProvider or StubSchemaProvider).
    schema_provider: Box<dyn SchemaProvider>,

    /// How schemas feed into suggestions.
    schema_mode: SchemaMode,

    /// Whether the intelligence system is enabled.
    enabled: bool,
}

impl CommandIntelligence {
    /// Creates a new CommandIntelligence instance.
    ///
    /// This initializes the schema if needed and loads the last sync state.
    pub fn new(conn: Connection) -> Result<Self, CIError> {
        Self::with_mode(conn, SchemaMode::default())
    }

    /// Creates a new CommandIntelligence with a specific schema mode.
    pub fn with_mode(conn: Connection, schema_mode: SchemaMode) -> Result<Self, CIError> {
        // Create ci_* schema if needed
        db_schema::create_schema(&conn)?;

        // Create the schema provider based on feature flags
        let schema_provider = Self::create_provider(&conn, schema_mode)?;

        // Bootstrap command hierarchy if empty (first run)
        bootstrap::bootstrap_if_empty(&conn, schema_provider.as_ref())?;

        // Load last sync ID
        let last_sync_id = sync::get_last_sync_id(&conn)?;

        info!(
            last_sync_id,
            schema_count = schema_provider.schema_count(),
            schema_mode = ?schema_mode,
            "Command Intelligence initialized"
        );

        Ok(Self {
            conn,
            token_cache: HashMap::new(),
            last_sync_id,
            current_session: None,
            schema_provider,
            schema_mode,
            enabled: true,
        })
    }

    /// Creates the appropriate schema provider based on feature flags.
    fn create_provider(
        conn: &Connection,
        mode: SchemaMode,
    ) -> Result<Box<dyn SchemaProvider>, CIError> {
        #[cfg(feature = "command-schema")]
        {
            let provider = schema_provider::FullSchemaProvider::new(conn, mode)?;
            Ok(Box::new(provider))
        }
        #[cfg(not(feature = "command-schema"))]
        {
            let _ = (conn, mode);
            Ok(Box::new(schema_provider::StubSchemaProvider::new()))
        }
    }

    /// Creates a new CommandIntelligence from an existing database path.
    pub fn from_path(path: &std::path::Path) -> Result<Self, CIError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(250))?;
        Self::new(conn)
    }

    /// Returns whether the intelligence system is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enables or disables the intelligence system.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns the current schema mode.
    pub fn schema_mode(&self) -> SchemaMode {
        self.schema_mode
    }

    /// Sets the schema mode, rebuilding the provider if necessary.
    ///
    /// Switching to/from `FullLibrary` requires rebuilding the schema provider
    /// to load or unload the bundled database. Other transitions only update the
    /// mode flag (the suggestion engine gates on `SchemaMode::uses_schemas()`).
    pub fn set_schema_mode(&mut self, mode: SchemaMode) {
        let needs_rebuild =
            (mode == SchemaMode::FullLibrary) != (self.schema_mode == SchemaMode::FullLibrary);
        let prev = self.schema_mode;
        self.schema_mode = mode;

        if needs_rebuild {
            match Self::create_provider(&self.conn, mode) {
                Ok(provider) => {
                    self.schema_provider = provider;
                    info!(
                        schema_mode = ?mode,
                        count = self.schema_provider.schema_count(),
                        "Rebuilt schema provider for mode change"
                    );
                }
                Err(e) => {
                    self.schema_mode = prev;
                    tracing::warn!(
                        error = %e,
                        reverted_to = ?prev,
                        "Failed to rebuild schema provider on mode change, reverting"
                    );
                }
            }
        }
    }

    /// Returns a reference to the schema provider.
    pub fn schema_provider(&self) -> &dyn SchemaProvider {
        self.schema_provider.as_ref()
    }

    /// Returns a mutable reference to the schema provider.
    pub fn schema_provider_mut(&mut self) -> &mut dyn SchemaProvider {
        self.schema_provider.as_mut()
    }

    /// Synchronizes with reedline's history.
    ///
    /// This reads new entries since the last sync and processes them
    /// into the intelligence tables.
    pub fn sync(&mut self) -> Result<SyncStats, CIError> {
        if !self.enabled {
            return Err(CIError::Disabled);
        }

        let (stats, new_last_id) =
            sync::sync_from_reedline(&self.conn, self.last_sync_id, self.schema_provider.as_ref())?;
        self.last_sync_id = new_last_id;

        Ok(stats)
    }

    /// Analyzes a command string and returns classified tokens.
    pub fn analyze(&self, command: &str) -> Vec<AnalyzedToken> {
        tokenizer::analyze_command(command)
    }

    /// Gets or creates a token ID, using the cache.
    ///
    /// Delegates to the canonical patterns::get_or_create_token implementation.
    pub fn get_or_create_token(
        &mut self,
        text: &str,
        token_type: crate::chrome::command_edit::TokenType,
    ) -> Result<i64, CIError> {
        let now = chrono::Utc::now().timestamp();
        patterns::get_or_create_token(&self.conn, &mut self.token_cache, text, token_type, now)
    }

    /// Learns from a command execution.
    ///
    /// This should be called after a command completes to update patterns.
    /// Note: patterns::learn_command already handles variant execution recording,
    /// so we don't duplicate that call here.
    pub fn learn_command(
        &mut self,
        command: &str,
        exit_status: Option<i32>,
    ) -> Result<(), CIError> {
        if !self.enabled {
            return Ok(());
        }

        let now = chrono::Utc::now().timestamp();

        // Get session database ID if we have an active session
        let session_db_id = self
            .current_session
            .as_ref()
            .and_then(|session| sessions::get_session_db_id(&self.conn, &session.session_id));

        // patterns::learn_command handles sequences, hierarchy, pipes, flags, and variant recording
        // Pass session_db_id so ci_commands.session_id is set
        patterns::learn_command(
            &mut self.conn,
            &mut self.token_cache,
            command,
            exit_status,
            session_db_id,
            self.schema_provider.as_ref(),
        )?;

        // Track session command if active
        if let Some(ref mut session) = self.current_session {
            // Get the timestamp of the previous command for transition time delta
            let prev_timestamp =
                sessions::get_last_command_timestamp(&self.conn, &session.session_id);

            // Record transition from previous command to this one
            if let Some(last) = session.recent_commands.last().cloned() {
                sessions::record_transition(&self.conn, &last, command, now, prev_timestamp)?;
            }

            // Add command to session tracking table (must be after learn_command so command exists)
            sessions::add_session_command(&self.conn, &session.session_id, command, now)?;

            // Update in-memory session context
            session.add_command(command.to_string());
        }

        Ok(())
    }

    /// Gets suggestions for the given context.
    pub fn suggest(&self, context: &SuggestionContext, limit: usize) -> Vec<Suggestion> {
        if !self.enabled {
            return Vec::new();
        }

        suggest::suggest(
            &self.conn,
            self.schema_provider.as_ref(),
            self.schema_mode,
            context,
            limit,
        )
    }

    /// Starts a new session.
    pub fn start_session(&mut self, session_id: &str) -> Result<(), CIError> {
        if !self.enabled {
            return Ok(());
        }

        sessions::start_session(&self.conn, session_id)?;
        self.current_session = Some(SessionContext::new(session_id));

        info!(session_id, "Started intelligence session");
        Ok(())
    }

    /// Ends the current session.
    pub fn end_session(&mut self) -> Result<(), CIError> {
        if let Some(session) = self.current_session.take() {
            sessions::end_session(&self.conn, &session.session_id)?;
            info!(session_id = %session.session_id, "Ended intelligence session");
        }
        Ok(())
    }

    /// Returns the current session context.
    pub fn current_session(&self) -> Option<&SessionContext> {
        self.current_session.as_ref()
    }

    /// Gets session-based "next command" suggestions.
    pub fn suggest_next_in_session(&self, last_command: &str) -> Vec<Suggestion> {
        if !self.enabled {
            return Vec::new();
        }

        sessions::suggest_next(&self.conn, last_command)
    }

    // ========================================================================
    // Template Methods
    // ========================================================================

    /// Extracts a template from a command.
    pub fn extract_template(&mut self, command: &str) -> Option<Template> {
        if !self.enabled {
            return None;
        }

        templates::extract_template(&self.conn, command)
            .ok()
            .flatten()
    }

    /// Gets template completions for the given context.
    pub fn suggest_templates(&self, context: &SuggestionContext) -> Vec<TemplateCompletion> {
        if !self.enabled {
            return Vec::new();
        }

        templates::suggest_templates(&self.conn, context)
    }

    // ========================================================================
    // User Pattern Methods
    // ========================================================================

    /// Adds a user-defined pattern.
    pub fn add_user_pattern(&mut self, pattern: UserPattern) -> Result<i64, CIError> {
        user_patterns::add_pattern(&self.conn, pattern)
    }

    /// Removes a user pattern by ID.
    pub fn remove_user_pattern(&mut self, id: i64) -> Result<(), CIError> {
        user_patterns::remove_pattern(&self.conn, id)
    }

    /// Lists user patterns.
    pub fn list_user_patterns(&self, pattern_type: Option<UserPatternType>) -> Vec<UserPattern> {
        user_patterns::list_patterns(&self.conn, pattern_type).unwrap_or_default()
    }

    /// Adds a user-defined alias.
    pub fn add_alias(
        &mut self,
        alias: &str,
        expansion: &str,
        description: Option<&str>,
    ) -> Result<i64, CIError> {
        user_patterns::add_alias(&self.conn, alias, expansion, description)
    }

    /// Removes an alias.
    pub fn remove_alias(&mut self, alias: &str) -> Result<(), CIError> {
        user_patterns::remove_alias(&self.conn, alias)
    }

    /// Lists all aliases.
    pub fn list_aliases(&self) -> Vec<UserAlias> {
        user_patterns::list_aliases(&self.conn).unwrap_or_default()
    }

    /// Expands an alias if it exists.
    pub fn expand_alias(&self, text: &str) -> Option<String> {
        user_patterns::expand_alias(&self.conn, text).ok().flatten()
    }

    // ========================================================================
    // Fuzzy Search Methods
    // ========================================================================

    /// Performs a fuzzy search for commands.
    pub fn fuzzy_search(&self, query: &str, limit: usize) -> Vec<FuzzyMatch> {
        if !self.enabled {
            return Vec::new();
        }

        fuzzy::fuzzy_search(&self.conn, query, limit).unwrap_or_default()
    }

    // ========================================================================
    // Export/Import Methods
    // ========================================================================

    /// Exports patterns to JSON.
    pub fn export(&self, options: ExportOptions) -> Result<String, CIError> {
        export::export(&self.conn, options)
    }

    /// Imports patterns from JSON.
    pub fn import(&mut self, json: &str, options: ImportOptions) -> Result<ImportStats, CIError> {
        export::import(&self.conn, json, options)
    }

    /// Exports schemas as a schema-pack JSON document.
    pub fn export_schema_pack(&self) -> Result<String, CIError> {
        export::export_schema_pack(self.schema_provider.as_ref())
    }

    /// Imports schemas as runtime overlays.
    pub fn import_schema_pack(
        &mut self,
        json: &str,
    ) -> Result<export::SchemaPackImportStats, CIError> {
        export::import_schema_pack(&self.conn, self.schema_provider.as_mut(), json)
    }

    // ========================================================================
    // Utility Methods
    // ========================================================================

    /// Gets the success rate for a command pattern.
    pub fn get_success_rate(&self, command: &str) -> Option<f64> {
        variants::get_success_rate(&self.conn, command)
            .ok()
            .flatten()
    }

    /// Clears the token cache.
    pub fn clear_cache(&mut self) {
        self.token_cache.clear();
    }

    /// Resets the intelligence database, deleting all learned patterns.
    ///
    /// This drops and recreates all `ci_*` tables, giving a clean slate.
    /// Schemas are then reloaded and hierarchy is re-seeded.
    pub fn reset(&mut self) -> Result<(), CIError> {
        db_schema::reset_database(&self.conn)?;

        // Recreate the schema provider
        self.schema_provider = Self::create_provider(&self.conn, self.schema_mode)?;

        // Re-bootstrap hierarchy from schema provider.
        bootstrap::bootstrap_if_empty(&self.conn, self.schema_provider.as_ref())?;

        // Clear in-memory state
        self.token_cache.clear();
        self.last_sync_id = 0;
        self.current_session = None;

        info!("Command Intelligence database reset complete");
        Ok(())
    }

    /// Gets statistics about the intelligence database.
    pub fn stats(&self) -> Result<IntelligenceStats, CIError> {
        let token_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM ci_tokens", [], |row| row.get(0))?;

        let command_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM ci_commands", [], |row| row.get(0))?;

        let sequence_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM ci_sequences", [], |row| row.get(0))?;

        let template_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM ci_templates", [], |row| row.get(0))?;

        let user_pattern_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM ci_user_patterns", [], |row| {
                    row.get(0)
                })?;

        Ok(IntelligenceStats {
            token_count: token_count as usize,
            command_count: command_count as usize,
            sequence_count: sequence_count as usize,
            template_count: template_count as usize,
            user_pattern_count: user_pattern_count as usize,
            schema_count: self.schema_provider.schema_count(),
            last_sync_id: self.last_sync_id,
            schema_mode: self.schema_mode,
        })
    }
}

/// Statistics about the intelligence database.
#[derive(Debug, Clone, Default)]
pub struct IntelligenceStats {
    /// Number of unique tokens.
    pub token_count: usize,

    /// Number of processed commands.
    pub command_count: usize,

    /// Number of learned sequences.
    pub sequence_count: usize,

    /// Number of recognized templates.
    pub template_count: usize,

    /// Number of user-defined patterns.
    pub user_pattern_count: usize,

    /// Number of available command schemas.
    pub schema_count: usize,

    /// Last synced reedline history ID.
    pub last_sync_id: i64,

    /// Current schema mode.
    pub schema_mode: SchemaMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_ci() -> CommandIntelligence {
        let conn = Connection::open_in_memory().unwrap();

        // Create reedline-like history table
        conn.execute(
            "CREATE TABLE history (
                id INTEGER PRIMARY KEY,
                command_line TEXT NOT NULL,
                start_timestamp INTEGER,
                exit_status INTEGER,
                cwd TEXT
            )",
            [],
        )
        .unwrap();

        CommandIntelligence::new(conn).unwrap()
    }

    #[test]
    fn test_new_creates_schema() {
        let ci = setup_test_ci();
        assert!(ci.is_enabled());
    }

    #[test]
    fn test_analyze() {
        let ci = setup_test_ci();
        let tokens = ci.analyze("git commit -m 'test'");
        assert_eq!(tokens.len(), 4);
    }

    #[test]
    fn test_enable_disable() {
        let mut ci = setup_test_ci();
        assert!(ci.is_enabled());

        ci.set_enabled(false);
        assert!(!ci.is_enabled());

        // Operations should return early when disabled
        assert!(ci.suggest(&SuggestionContext::default(), 10).is_empty());
    }

    #[test]
    fn test_session_lifecycle() {
        let mut ci = setup_test_ci();

        ci.start_session("test-session").unwrap();
        assert!(ci.current_session().is_some());

        ci.end_session().unwrap();
        assert!(ci.current_session().is_none());
    }

    #[test]
    fn test_schema_mode() {
        let ci = setup_test_ci();
        assert_eq!(ci.schema_mode(), SchemaMode::SchemaEnabled);
    }
}
