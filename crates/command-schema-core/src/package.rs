use serde::{Deserialize, Serialize};

use crate::CommandSchema;

/// Serializable schema bundle used for curation and distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaPackage {
    /// Schema contract version (populated from [`SCHEMA_CONTRACT_VERSION`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub schema_version: Option<String>,
    /// Package format version (semver string).
    pub version: String,
    /// Optional package name.
    pub name: Option<String>,
    /// Optional package description.
    pub description: Option<String>,
    /// ISO-8601 timestamp for package creation.
    pub generated_at: String,
    /// Optional hash of deterministic bundle content.
    pub bundle_hash: Option<String>,
    /// Command schemas included in this package.
    pub schemas: Vec<CommandSchema>,
}

impl SchemaPackage {
    /// Creates a package with required fields.
    pub fn new(version: impl Into<String>, generated_at: impl Into<String>) -> Self {
        Self {
            schema_version: Some(crate::SCHEMA_CONTRACT_VERSION.to_string()),
            version: version.into(),
            name: None,
            description: None,
            generated_at: generated_at.into(),
            bundle_hash: None,
            schemas: Vec::new(),
        }
    }

    /// Returns the number of schemas in this package.
    pub fn schema_count(&self) -> usize {
        self.schemas.len()
    }
}
