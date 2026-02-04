//! Error types for the Command Intelligence Engine.

use thiserror::Error;

/// Errors that can occur in the Command Intelligence Engine.
#[derive(Debug, Error)]
pub enum CIError {
    /// SQLite database error.
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// Serialization/deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Invalid schema version.
    #[error("Invalid schema version: expected {expected}, found {found}")]
    SchemaVersion { expected: i32, found: i32 },

    /// Migration failed.
    #[error("Migration failed: {0}")]
    Migration(String),

    /// Sync error.
    #[error("Sync error: {0}")]
    Sync(String),

    /// Pattern not found.
    #[error("Pattern not found: {0}")]
    NotFound(String),

    /// Invalid import data.
    #[error("Invalid import data: {0}")]
    InvalidImport(String),

    /// Intelligence system is disabled.
    #[error("Intelligence system is disabled")]
    Disabled,

    /// Internal error.
    #[error("Internal error: {0}")]
    Internal(String),
}

impl CIError {
    /// Creates an internal error with the given message.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    /// Creates a sync error with the given message.
    pub fn sync(msg: impl Into<String>) -> Self {
        Self::Sync(msg.into())
    }

    /// Creates a migration error with the given message.
    pub fn migration(msg: impl Into<String>) -> Self {
        Self::Migration(msg.into())
    }
}
