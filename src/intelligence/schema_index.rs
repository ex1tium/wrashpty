//! In-memory schema index with embedded curated schemas and runtime overlays.
//!
//! Embedded schemas are loaded from build artifacts and are authoritative.
//! Runtime overlays are learned/imported schemas for uncurated commands only.
//! Lookups always prefer embedded schemas to avoid silent structural mutation.

use std::collections::HashMap;

use command_schema_core::{CommandSchema, SchemaPackage};
use rusqlite::Connection;

use super::CIError;

include!(concat!(env!("OUT_DIR"), "/schema_meta.rs"));

/// Metadata for the embedded schema bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleMeta {
    /// Bundle version string.
    pub version: String,
    /// Deterministic content hash.
    pub hash: Option<String>,
    /// Build timestamp string.
    pub generated_at: String,
    /// Number of schemas in the bundle.
    pub schema_count: usize,
}

/// In-memory schema lookup index.
#[derive(Debug, Clone)]
pub struct SchemaIndex {
    /// Curated schemas embedded at build time (authoritative).
    embedded: HashMap<String, CommandSchema>,
    /// Runtime overlays for uncurated commands.
    overlays: HashMap<String, CommandSchema>,
    bundle_meta: BundleMeta,
}

impl SchemaIndex {
    /// Loads the embedded curated schema package generated at build time.
    pub fn from_embedded() -> Result<Self, CIError> {
        let raw = include_str!(concat!(env!("OUT_DIR"), "/embedded_schemas.json"));
        let package: SchemaPackage = serde_json::from_str(raw)?;
        Ok(Self::from_package(package))
    }

    /// Creates an index from explicit schemas (primarily for tests).
    pub fn from_schemas(schemas: Vec<CommandSchema>) -> Self {
        let schema_count = schemas.len();
        let embedded = schemas
            .into_iter()
            .map(|schema| (schema.command.clone(), schema))
            .collect();

        Self {
            embedded,
            overlays: HashMap::new(),
            bundle_meta: BundleMeta {
                version: "test".to_string(),
                hash: None,
                generated_at: "0".to_string(),
                schema_count,
            },
        }
    }

    /// Returns a schema for a base command.
    pub fn get(&self, command: &str) -> Option<&CommandSchema> {
        self.embedded
            .get(command)
            .or_else(|| self.overlays.get(command))
    }

    /// Returns true if a schema exists for a base command.
    pub fn has_schema(&self, command: &str) -> bool {
        self.is_curated(command) || self.overlays.contains_key(command)
    }

    /// Returns true if the command comes from the embedded curated bundle.
    pub fn is_curated(&self, command: &str) -> bool {
        self.embedded.contains_key(command)
    }

    /// Inserts or replaces a runtime overlay schema.
    pub fn add_user_schema(&mut self, schema: CommandSchema) {
        self.overlays.insert(schema.command.clone(), schema);
    }

    /// Loads learned runtime overlays from `ci_command_schemas`.
    ///
    /// Only base-command rows with `source = 'learned'` are loaded, and curated
    /// embedded schemas always remain authoritative.
    pub fn load_runtime_overlays(&mut self, conn: &Connection) -> Result<usize, CIError> {
        let mut stmt = conn.prepare(
            "SELECT command, schema_json
             FROM ci_command_schemas
             WHERE source = 'learned' AND subcommand IS NULL
             ORDER BY command",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut loaded = 0;
        for row in rows {
            let (command, schema_json) = row?;

            // Embedded curated schemas always win.
            if self.is_curated(&command) {
                continue;
            }

            let schema: CommandSchema = serde_json::from_str(&schema_json)?;
            self.overlays.insert(command, schema);
            loaded += 1;
        }

        Ok(loaded)
    }

    /// Iterates known command names.
    ///
    /// Embedded command names are returned first; overlay-only commands are
    /// appended for commands missing from the embedded set.
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.embedded
            .keys()
            .chain(
                self.overlays
                    .keys()
                    .filter(|command| !self.embedded.contains_key(*command)),
            )
            .map(String::as_str)
    }

    /// Iterates all schemas.
    ///
    /// Embedded schemas are yielded first, followed by overlay schemas that do
    /// not collide with embedded commands.
    pub fn all_schemas(&self) -> impl Iterator<Item = &CommandSchema> {
        self.embedded.values().chain(
            self.overlays
                .iter()
                .filter(|(command, _)| !self.embedded.contains_key(*command))
                .map(|(_, schema)| schema),
        )
    }

    /// Returns metadata for the currently loaded bundle.
    pub fn bundle_meta(&self) -> &BundleMeta {
        &self.bundle_meta
    }

    fn from_package(package: SchemaPackage) -> Self {
        let schema_count = package.schemas.len();
        let embedded = package
            .schemas
            .into_iter()
            .map(|schema| (schema.command.clone(), schema))
            .collect();

        let hash = package
            .bundle_hash
            .or_else(|| Some(EMBEDDED_SCHEMA_BUNDLE_HASH.to_string()));

        Self {
            embedded,
            overlays: HashMap::new(),
            bundle_meta: BundleMeta {
                version: package.version,
                hash,
                generated_at: package.generated_at,
                schema_count,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use command_schema_core::SchemaSource;
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn test_from_schemas_lookup_and_commands() {
        let index = SchemaIndex::from_schemas(vec![
            CommandSchema::new("git", SchemaSource::Bootstrap),
            CommandSchema::new("cargo", SchemaSource::Bootstrap),
        ]);

        assert!(index.has_schema("git"));
        assert!(!index.has_schema("docker"));

        let mut commands = index.commands().collect::<Vec<_>>();
        commands.sort();
        assert_eq!(commands, vec!["cargo", "git"]);
    }

    #[test]
    fn test_add_user_schema() {
        let mut index = SchemaIndex::from_schemas(vec![]);
        index.add_user_schema(CommandSchema::new("custom-tool", SchemaSource::Learned));

        assert!(index.get("custom-tool").is_some());
        assert!(index.has_schema("custom-tool"));
    }

    #[test]
    fn test_embedded_precedence_over_overlay() {
        let mut index =
            SchemaIndex::from_schemas(vec![CommandSchema::new("git", SchemaSource::Bootstrap)]);

        index.add_user_schema(CommandSchema::new("git", SchemaSource::Learned));

        let schema = index.get("git").expect("git schema should exist");
        assert_eq!(schema.source, SchemaSource::Bootstrap);
        assert!(index.is_curated("git"));
    }

    #[test]
    fn test_load_runtime_overlays_skips_curated_and_loads_uncurated() {
        let mut index =
            SchemaIndex::from_schemas(vec![CommandSchema::new("cargo", SchemaSource::Bootstrap)]);
        let conn = Connection::open_in_memory().unwrap();
        crate::intelligence::db_schema::create_schema(&conn).unwrap();

        let learned_cargo = CommandSchema::new("cargo", SchemaSource::Learned);
        let learned_tool = CommandSchema::new("tool", SchemaSource::Learned);
        let learned_cargo_json = serde_json::to_string(&learned_cargo).unwrap();
        let learned_tool_json = serde_json::to_string(&learned_tool).unwrap();

        conn.execute(
            "INSERT INTO ci_command_schemas
             (command, subcommand, schema_json, source, confidence, extracted_at)
             VALUES ('cargo', NULL, ?1, 'learned', 1.0, 0)",
            [learned_cargo_json],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ci_command_schemas
             (command, subcommand, schema_json, source, confidence, extracted_at)
             VALUES ('tool', NULL, ?1, 'learned', 1.0, 0)",
            [learned_tool_json],
        )
        .unwrap();

        let loaded = index.load_runtime_overlays(&conn).unwrap();
        assert_eq!(loaded, 1);

        let cargo_schema = index.get("cargo").expect("cargo schema should exist");
        assert_eq!(cargo_schema.source, SchemaSource::Bootstrap);

        let tool_schema = index.get("tool").expect("tool overlay should exist");
        assert_eq!(tool_schema.source, SchemaSource::Learned);
    }

    #[test]
    fn test_from_embedded_has_expected_metadata() {
        let index = SchemaIndex::from_embedded().expect("embedded schema package should load");
        let meta = index.bundle_meta();

        assert!(!meta.version.is_empty());
        // schema_count may be 0 when no curated schemas are present
        assert!(meta.hash.as_deref().is_some_and(|value| !value.is_empty()));
    }
}
