//! Schema type definitions for command structure modeling.

use serde::{Deserialize, Serialize};

/// Source of schema information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SchemaSource {
    /// Extracted from --help output
    #[default]
    HelpCommand,
    /// Parsed from man page
    ManPage,
    /// Manually defined in bootstrap
    Bootstrap,
    /// Learned from user command history
    Learned,
}

/// Value type for flags and arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ValueType {
    /// Boolean flag (no value)
    Bool,
    /// String value
    String,
    /// Numeric value
    Number,
    /// File path
    File,
    /// Directory path
    Directory,
    /// URL
    Url,
    /// Git branch name (learned from history)
    Branch,
    /// Git remote name (learned from history)
    Remote,
    /// One of specific choices
    Choice(Vec<String>),
    /// Unknown/any type
    #[default]
    Any,
}

/// Schema for a command flag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagSchema {
    /// Short form (e.g., "-m")
    pub short: Option<String>,
    /// Long form (e.g., "--message")
    pub long: Option<String>,
    /// Type of value this flag accepts
    pub value_type: ValueType,
    /// Whether a value is required
    pub takes_value: bool,
    /// Description from help text
    pub description: Option<String>,
    /// Can this flag appear multiple times?
    pub multiple: bool,
    /// Flags this conflicts with (mutually exclusive)
    pub conflicts_with: Vec<String>,
    /// Flags this requires to also be present
    pub requires: Vec<String>,
}

impl FlagSchema {
    /// Creates a boolean flag (no value).
    pub fn boolean(short: Option<&str>, long: Option<&str>) -> Self {
        Self {
            short: short.map(String::from),
            long: long.map(String::from),
            value_type: ValueType::Bool,
            takes_value: false,
            description: None,
            multiple: false,
            conflicts_with: Vec::new(),
            requires: Vec::new(),
        }
    }

    /// Creates a flag that takes a value.
    pub fn with_value(short: Option<&str>, long: Option<&str>, value_type: ValueType) -> Self {
        Self {
            short: short.map(String::from),
            long: long.map(String::from),
            value_type,
            takes_value: true,
            description: None,
            multiple: false,
            conflicts_with: Vec::new(),
            requires: Vec::new(),
        }
    }

    /// Adds a description.
    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    /// Marks as allowing multiple occurrences.
    pub fn allow_multiple(mut self) -> Self {
        self.multiple = true;
        self
    }

    /// Returns the canonical name (long form preferred).
    pub fn canonical_name(&self) -> &str {
        self.long
            .as_deref()
            .or(self.short.as_deref())
            .unwrap_or("unknown")
    }

    /// Checks if this flag matches a given string.
    pub fn matches(&self, s: &str) -> bool {
        self.short.as_deref() == Some(s) || self.long.as_deref() == Some(s)
    }
}

/// Schema for a positional argument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSchema {
    /// Name of the argument (e.g., "file", "url")
    pub name: String,
    /// Type of value expected
    pub value_type: ValueType,
    /// Is this argument required?
    pub required: bool,
    /// Can multiple values be provided?
    pub multiple: bool,
    /// Description from help text
    pub description: Option<String>,
}

impl ArgSchema {
    /// Creates a required positional argument.
    pub fn required(name: &str, value_type: ValueType) -> Self {
        Self {
            name: name.to_string(),
            value_type,
            required: true,
            multiple: false,
            description: None,
        }
    }

    /// Creates an optional positional argument.
    pub fn optional(name: &str, value_type: ValueType) -> Self {
        Self {
            name: name.to_string(),
            value_type,
            required: false,
            multiple: false,
            description: None,
        }
    }

    /// Marks as accepting multiple values.
    pub fn allow_multiple(mut self) -> Self {
        self.multiple = true;
        self
    }
}

/// Schema for a subcommand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubcommandSchema {
    /// Name of the subcommand
    pub name: String,
    /// Short description
    pub description: Option<String>,
    /// Flags specific to this subcommand
    pub flags: Vec<FlagSchema>,
    /// Positional arguments
    pub positional: Vec<ArgSchema>,
    /// Nested subcommands (e.g., git remote add)
    pub subcommands: Vec<SubcommandSchema>,
    /// Aliases for this subcommand
    pub aliases: Vec<String>,
}

impl SubcommandSchema {
    /// Creates a new subcommand schema.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// Adds a flag to this subcommand.
    pub fn with_flag(mut self, flag: FlagSchema) -> Self {
        self.flags.push(flag);
        self
    }

    /// Adds a positional argument.
    pub fn with_arg(mut self, arg: ArgSchema) -> Self {
        self.positional.push(arg);
        self
    }

    /// Adds a nested subcommand.
    pub fn with_subcommand(mut self, sub: SubcommandSchema) -> Self {
        self.subcommands.push(sub);
        self
    }
}

/// Complete schema for a command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandSchema {
    /// The base command name (e.g., "git", "docker")
    pub command: String,
    /// Short description of the command
    pub description: Option<String>,
    /// Global flags (apply to all subcommands)
    pub global_flags: Vec<FlagSchema>,
    /// Subcommands
    pub subcommands: Vec<SubcommandSchema>,
    /// Positional arguments (for commands without subcommands)
    pub positional: Vec<ArgSchema>,
    /// Where this schema came from
    pub source: SchemaSource,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,
    /// Version string if detected
    pub version: Option<String>,
}

impl CommandSchema {
    /// Creates a new command schema.
    pub fn new(command: &str, source: SchemaSource) -> Self {
        Self {
            command: command.to_string(),
            source,
            confidence: 1.0,
            ..Default::default()
        }
    }

    /// Finds a subcommand by name.
    pub fn find_subcommand(&self, name: &str) -> Option<&SubcommandSchema> {
        self.subcommands
            .iter()
            .find(|s| s.name == name || s.aliases.contains(&name.to_string()))
    }

    /// Finds a global flag by short or long form.
    pub fn find_global_flag(&self, flag: &str) -> Option<&FlagSchema> {
        self.global_flags.iter().find(|f| f.matches(flag))
    }

    /// Gets all subcommand names.
    pub fn subcommand_names(&self) -> Vec<&str> {
        self.subcommands.iter().map(|s| s.name.as_str()).collect()
    }

    /// Gets all flags for a specific subcommand (global + subcommand-specific).
    pub fn flags_for_subcommand(&self, subcommand: &str) -> Vec<&FlagSchema> {
        let mut flags: Vec<&FlagSchema> = self.global_flags.iter().collect();
        if let Some(sub) = self.find_subcommand(subcommand) {
            flags.extend(sub.flags.iter());
        }
        flags
    }
}

/// Result of schema extraction attempt.
#[derive(Debug, Clone)]
pub struct ExtractionResult {
    /// The extracted schema (if successful)
    pub schema: Option<CommandSchema>,
    /// Raw help output that was parsed
    pub raw_output: String,
    /// Format that was detected
    pub detected_format: Option<HelpFormat>,
    /// Warnings encountered during parsing
    pub warnings: Vec<String>,
    /// Whether extraction was successful
    pub success: bool,
}

/// Detected help output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpFormat {
    /// Rust Clap library format
    Clap,
    /// Go Cobra library format
    Cobra,
    /// Python argparse format
    Argparse,
    /// Docopt format
    Docopt,
    /// GNU standard format
    Gnu,
    /// BSD style
    Bsd,
    /// Unknown/custom format
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flag_schema_creation() {
        let flag = FlagSchema::boolean(Some("-v"), Some("--verbose"))
            .with_description("Enable verbose output");

        assert_eq!(flag.short, Some("-v".to_string()));
        assert_eq!(flag.long, Some("--verbose".to_string()));
        assert!(!flag.takes_value);
        assert_eq!(flag.canonical_name(), "--verbose");
    }

    #[test]
    fn test_flag_with_value() {
        let flag = FlagSchema::with_value(Some("-m"), Some("--message"), ValueType::String);

        assert!(flag.takes_value);
        assert_eq!(flag.value_type, ValueType::String);
    }

    #[test]
    fn test_flag_matches() {
        let flag = FlagSchema::boolean(Some("-v"), Some("--verbose"));

        assert!(flag.matches("-v"));
        assert!(flag.matches("--verbose"));
        assert!(!flag.matches("-x"));
    }

    #[test]
    fn test_command_schema_find_subcommand() {
        let mut schema = CommandSchema::new("git", SchemaSource::Bootstrap);
        schema.subcommands.push(SubcommandSchema::new("commit"));
        schema.subcommands.push(SubcommandSchema::new("push"));

        assert!(schema.find_subcommand("commit").is_some());
        assert!(schema.find_subcommand("pull").is_none());
    }
}
