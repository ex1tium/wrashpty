//! Core schema types and shared schema package primitives.

mod merge;
mod package;
mod types;
mod validate;

pub use merge::{MergeStrategy, merge_schemas};
pub use package::SchemaPackage;
pub use types::*;
pub use validate::{ValidationError, validate_package, validate_schema};
