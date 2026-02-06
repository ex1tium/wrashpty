//! Schema storage and retrieval from SQLite database.

use rusqlite::{Connection, OptionalExtension};
use tracing::{debug, info};

use super::types::{CommandSchema, SchemaSource, SubcommandSchema};
use crate::intelligence::error::CIError;

/// Creates the schema storage tables.
pub fn create_schema_tables(conn: &Connection) -> Result<(), CIError> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ci_command_schemas (
            id INTEGER PRIMARY KEY,
            command TEXT NOT NULL,
            subcommand TEXT,
            schema_json TEXT NOT NULL,
            source TEXT NOT NULL,
            confidence REAL DEFAULT 1.0,
            extracted_at INTEGER NOT NULL,
            last_validated INTEGER,
            UNIQUE(command, subcommand)
        )",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_schema_command ON ci_command_schemas(command)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_schema_source ON ci_command_schemas(source)",
        [],
    )?;

    debug!("Created schema storage tables");
    Ok(())
}

/// Schema storage operations.
pub struct SchemaStore<'a> {
    conn: &'a Connection,
}

impl<'a> SchemaStore<'a> {
    /// Creates a new schema store.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Stores a command schema.
    pub fn store(&self, schema: &CommandSchema) -> Result<(), CIError> {
        let now = chrono::Utc::now().timestamp();
        let source = match schema.source {
            SchemaSource::HelpCommand => "help",
            SchemaSource::ManPage => "man",
            SchemaSource::Bootstrap => "bootstrap",
            SchemaSource::Learned => "learned",
        };

        // Store main command schema
        let schema_json = serde_json::to_string(schema)
            .map_err(|e| CIError::Internal(format!("JSON serialization failed: {}", e)))?;

        self.conn.execute(
            "INSERT OR REPLACE INTO ci_command_schemas
             (command, subcommand, schema_json, source, confidence, extracted_at)
             VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
            rusqlite::params![schema.command, schema_json, source, schema.confidence, now,],
        )?;

        // Store each subcommand schema separately for quick lookup
        for subcmd in &schema.subcommands {
            self.store_subcommand(&schema.command, subcmd, None, source, now)?;
        }

        info!(
            command = %schema.command,
            subcommands = schema.subcommands.len(),
            "Stored command schema"
        );

        Ok(())
    }

    /// Stores a subcommand schema.
    ///
    /// The `parent_path` parameter tracks the full path of parent subcommands
    /// for nested hierarchies (e.g., "remote add" for "git remote add").
    fn store_subcommand(
        &self,
        base_command: &str,
        subcmd: &SubcommandSchema,
        parent_path: Option<&str>,
        source: &str,
        timestamp: i64,
    ) -> Result<(), CIError> {
        // Build the full subcommand path
        let full_subcommand = match parent_path {
            Some(parent) => format!("{} {}", parent, subcmd.name),
            None => subcmd.name.clone(),
        };

        let schema_json = serde_json::to_string(subcmd)
            .map_err(|e| CIError::Internal(format!("JSON serialization failed: {}", e)))?;

        self.conn.execute(
            "INSERT OR REPLACE INTO ci_command_schemas
             (command, subcommand, schema_json, source, confidence, extracted_at)
             VALUES (?1, ?2, ?3, ?4, 1.0, ?5)",
            rusqlite::params![
                base_command,
                full_subcommand,
                schema_json,
                source,
                timestamp,
            ],
        )?;

        // Recursively store nested subcommands with the updated path
        for nested in &subcmd.subcommands {
            self.store_subcommand(
                base_command,
                nested,
                Some(&full_subcommand),
                source,
                timestamp,
            )?;
        }

        Ok(())
    }

    /// Retrieves a command schema.
    pub fn get(&self, command: &str) -> Result<Option<CommandSchema>, CIError> {
        let result: Option<String> = self
            .conn
            .query_row(
                "SELECT schema_json FROM ci_command_schemas
                 WHERE command = ?1 AND subcommand IS NULL",
                [command],
                |row| row.get(0),
            )
            .optional()?;

        match result {
            Some(json) => {
                let schema: CommandSchema = serde_json::from_str(&json)
                    .map_err(|e| CIError::Internal(format!("JSON parse failed: {}", e)))?;
                Ok(Some(schema))
            }
            None => Ok(None),
        }
    }

    /// Retrieves a subcommand schema.
    pub fn get_subcommand(
        &self,
        command: &str,
        subcommand: &str,
    ) -> Result<Option<SubcommandSchema>, CIError> {
        let result: Option<String> = self
            .conn
            .query_row(
                "SELECT schema_json FROM ci_command_schemas
                 WHERE command = ?1 AND subcommand = ?2",
                [command, subcommand],
                |row| row.get(0),
            )
            .optional()?;

        match result {
            Some(json) => {
                let schema: SubcommandSchema = serde_json::from_str(&json)
                    .map_err(|e| CIError::Internal(format!("JSON parse failed: {}", e)))?;
                Ok(Some(schema))
            }
            None => Ok(None),
        }
    }

    /// Lists all stored command names.
    pub fn list_commands(&self) -> Result<Vec<String>, CIError> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT command FROM ci_command_schemas ORDER BY command")?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        let mut commands = Vec::new();
        for row in rows.flatten() {
            commands.push(row);
        }

        Ok(commands)
    }

    /// Lists all subcommands for a command.
    pub fn list_subcommands(&self, command: &str) -> Result<Vec<String>, CIError> {
        let mut stmt = self.conn.prepare(
            "SELECT subcommand FROM ci_command_schemas
             WHERE command = ?1 AND subcommand IS NOT NULL
             ORDER BY subcommand",
        )?;

        let rows = stmt.query_map([command], |row| row.get(0))?;

        let mut subcommands = Vec::new();
        for row in rows.flatten() {
            subcommands.push(row);
        }

        Ok(subcommands)
    }

    /// Checks if a schema exists for a command.
    pub fn has_schema(&self, command: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM ci_command_schemas WHERE command = ?1 LIMIT 1",
                [command],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// Gets schema stats for a command.
    pub fn get_stats(&self, command: &str) -> Result<SchemaStats, CIError> {
        let subcommand_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ci_command_schemas
             WHERE command = ?1 AND subcommand IS NOT NULL",
            [command],
            |row| row.get(0),
        )?;

        let confidence: f64 = self
            .conn
            .query_row(
                "SELECT confidence FROM ci_command_schemas
                 WHERE command = ?1 AND subcommand IS NULL",
                [command],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0.0);

        let source: String = self
            .conn
            .query_row(
                "SELECT source FROM ci_command_schemas
                 WHERE command = ?1 AND subcommand IS NULL",
                [command],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| "unknown".to_string());

        Ok(SchemaStats {
            command: command.to_string(),
            subcommand_count: subcommand_count as usize,
            confidence,
            source,
        })
    }

    /// Deletes all schemas for a command.
    pub fn delete(&self, command: &str) -> Result<usize, CIError> {
        let deleted = self.conn.execute(
            "DELETE FROM ci_command_schemas WHERE command = ?1",
            [command],
        )?;

        info!(
            command = command,
            deleted = deleted,
            "Deleted command schemas"
        );
        Ok(deleted)
    }
}

/// Stats about a stored schema.
#[derive(Debug, Clone)]
pub struct SchemaStats {
    pub command: String,
    pub subcommand_count: usize,
    pub confidence: f64,
    pub source: String,
}

/// Convenience function to store a schema.
pub fn store_schema(conn: &Connection, schema: &CommandSchema) -> Result<(), CIError> {
    SchemaStore::new(conn).store(schema)
}

/// Convenience function to get a schema.
pub fn get_schema(conn: &Connection, command: &str) -> Result<Option<CommandSchema>, CIError> {
    SchemaStore::new(conn).get(command)
}

/// Convenience function to get all stored command names.
pub fn get_all_schemas(conn: &Connection) -> Result<Vec<String>, CIError> {
    SchemaStore::new(conn).list_commands()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::schema::types::{FlagSchema, ValueType};

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_schema_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn test_store_and_retrieve_schema() {
        let conn = setup_test_db();
        let store = SchemaStore::new(&conn);

        let mut schema = CommandSchema::new("testcmd", SchemaSource::Bootstrap);
        schema
            .global_flags
            .push(FlagSchema::boolean(Some("-v"), Some("--verbose")));
        schema.subcommands.push(SubcommandSchema::new("build"));
        schema.subcommands.push(SubcommandSchema::new("run"));

        store.store(&schema).unwrap();

        let retrieved = store.get("testcmd").unwrap().unwrap();
        assert_eq!(retrieved.command, "testcmd");
        assert_eq!(retrieved.subcommands.len(), 2);
        assert_eq!(retrieved.global_flags.len(), 1);
    }

    #[test]
    fn test_get_subcommand() {
        let conn = setup_test_db();
        let store = SchemaStore::new(&conn);

        let mut schema = CommandSchema::new("git", SchemaSource::Bootstrap);
        let mut commit = SubcommandSchema::new("commit");
        commit.flags.push(FlagSchema::with_value(
            Some("-m"),
            Some("--message"),
            ValueType::String,
        ));
        schema.subcommands.push(commit);

        store.store(&schema).unwrap();

        let subcmd = store.get_subcommand("git", "commit").unwrap().unwrap();
        assert_eq!(subcmd.name, "commit");
        assert_eq!(subcmd.flags.len(), 1);
    }

    #[test]
    fn test_list_commands() {
        let conn = setup_test_db();
        let store = SchemaStore::new(&conn);

        store
            .store(&CommandSchema::new("git", SchemaSource::Bootstrap))
            .unwrap();
        store
            .store(&CommandSchema::new("docker", SchemaSource::Bootstrap))
            .unwrap();
        store
            .store(&CommandSchema::new("cargo", SchemaSource::Bootstrap))
            .unwrap();

        let commands = store.list_commands().unwrap();
        assert_eq!(commands.len(), 3);
        assert!(commands.contains(&"git".to_string()));
        assert!(commands.contains(&"docker".to_string()));
    }

    #[test]
    fn test_has_schema() {
        let conn = setup_test_db();
        let store = SchemaStore::new(&conn);

        assert!(!store.has_schema("git"));

        store
            .store(&CommandSchema::new("git", SchemaSource::Bootstrap))
            .unwrap();

        assert!(store.has_schema("git"));
        assert!(!store.has_schema("nonexistent"));
    }

    #[test]
    fn test_delete_schema() {
        let conn = setup_test_db();
        let store = SchemaStore::new(&conn);

        let mut schema = CommandSchema::new("git", SchemaSource::Bootstrap);
        schema.subcommands.push(SubcommandSchema::new("commit"));
        schema.subcommands.push(SubcommandSchema::new("push"));
        store.store(&schema).unwrap();

        assert!(store.has_schema("git"));

        let deleted = store.delete("git").unwrap();
        assert!(deleted > 0);
        assert!(!store.has_schema("git"));
    }
}
