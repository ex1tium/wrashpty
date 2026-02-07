//! In-memory curated schema index loaded from embedded build artifacts.

use std::collections::HashMap;

use command_schema_core::{CommandSchema, SchemaPackage};

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
    schemas: HashMap<String, CommandSchema>,
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
        let schemas = schemas
            .into_iter()
            .map(|schema| (schema.command.clone(), schema))
            .collect();

        Self {
            schemas,
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
        self.schemas.get(command)
    }

    /// Returns true if a schema exists for a base command.
    pub fn has_schema(&self, command: &str) -> bool {
        self.schemas.contains_key(command)
    }

    /// Inserts or replaces a runtime schema.
    pub fn add_user_schema(&mut self, schema: CommandSchema) {
        self.schemas.insert(schema.command.clone(), schema);
    }

    /// Iterates known command names.
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.schemas.keys().map(String::as_str)
    }

    /// Iterates all schemas.
    pub fn all_schemas(&self) -> impl Iterator<Item = &CommandSchema> {
        self.schemas.values()
    }

    /// Returns metadata for the currently loaded bundle.
    pub fn bundle_meta(&self) -> &BundleMeta {
        &self.bundle_meta
    }

    fn from_package(package: SchemaPackage) -> Self {
        let schema_count = package.schemas.len();
        let schemas = package
            .schemas
            .into_iter()
            .map(|schema| (schema.command.clone(), schema))
            .collect();

        let hash = package
            .bundle_hash
            .or_else(|| Some(EMBEDDED_SCHEMA_BUNDLE_HASH.to_string()));

        Self {
            schemas,
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
    fn test_from_embedded_has_expected_metadata() {
        let index = SchemaIndex::from_embedded().expect("embedded schema package should load");
        let meta = index.bundle_meta();

        assert!(!meta.version.is_empty());
        assert!(meta.schema_count > 0);
        assert!(meta.hash.as_deref().is_some_and(|value| !value.is_empty()));
    }
}
