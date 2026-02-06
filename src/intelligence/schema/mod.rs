//! Command schema extraction and storage.
//!
//! This module provides automatic extraction of CLI command schemas
//! by parsing --help output and man pages. Extracted schemas enable
//! intelligent, validated command suggestions.

mod extractor;
mod parser;
mod storage;
mod types;

pub use extractor::{extract_command_schema, probe_command_help};
pub use parser::HelpParser;
pub use storage::{SchemaStore, get_all_schemas, get_schema, store_schema};
pub use types::{
    ArgSchema, CommandSchema, ExtractionResult, FlagSchema, SchemaSource, SubcommandSchema,
    ValueType,
};
