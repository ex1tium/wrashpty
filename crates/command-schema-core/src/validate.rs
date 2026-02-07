use std::collections::HashSet;

use thiserror::Error;

use crate::{CommandSchema, FlagSchema, SchemaPackage, SubcommandSchema};

/// Schema/package validation errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("package version cannot be empty")]
    EmptyPackageVersion,
    #[error("schema command cannot be empty")]
    EmptyCommandName,
    #[error("duplicate command in package: {0}")]
    DuplicateCommand(String),
    #[error("invalid short flag format: {0}")]
    InvalidShortFlag(String),
    #[error("invalid long flag format: {0}")]
    InvalidLongFlag(String),
    #[error("flag must define short or long form")]
    MissingFlagName,
    #[error("duplicate flag in scope: {0}")]
    DuplicateFlag(String),
    #[error("duplicate subcommand in scope: {0}")]
    DuplicateSubcommand(String),
    #[error("subcommand cycle detected at path: {0}")]
    SubcommandCycle(String),
}

/// Validates a full schema package.
pub fn validate_package(package: &SchemaPackage) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if package.version.trim().is_empty() {
        errors.push(ValidationError::EmptyPackageVersion);
        return errors;
    }

    let mut seen_commands: HashSet<&str> = HashSet::new();
    for schema in &package.schemas {
        let command = schema.command.as_str();
        if !seen_commands.insert(command) {
            errors.push(ValidationError::DuplicateCommand(command.to_string()));
            return errors;
        }
        errors.extend(validate_schema(schema));
        if !errors.is_empty() {
            return errors;
        }
    }

    errors
}

/// Validates a command schema.
pub fn validate_schema(schema: &CommandSchema) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if schema.command.trim().is_empty() {
        errors.push(ValidationError::EmptyCommandName);
        return errors;
    }

    errors.extend(validate_flags(&schema.global_flags));
    if !errors.is_empty() {
        return errors;
    }

    let mut path = vec![schema.command.clone()];
    errors.extend(validate_subcommands(&schema.subcommands, &mut path));

    errors
}

fn validate_subcommands(
    subcommands: &[SubcommandSchema],
    path: &mut Vec<String>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for sub in subcommands {
        let name = sub.name.trim();
        if name.is_empty() {
            errors.push(ValidationError::DuplicateSubcommand("<empty>".to_string()));
            return errors;
        }

        if !seen.insert(name) {
            errors.push(ValidationError::DuplicateSubcommand(name.to_string()));
            return errors;
        }

        if path.iter().any(|segment| segment == name) {
            let cycle_path = path
                .iter()
                .cloned()
                .chain(std::iter::once(name.to_string()))
                .collect::<Vec<_>>()
                .join(" ");
            errors.push(ValidationError::SubcommandCycle(cycle_path));
            return errors;
        }

        errors.extend(validate_flags(&sub.flags));
        if !errors.is_empty() {
            return errors;
        }

        path.push(name.to_string());
        errors.extend(validate_subcommands(&sub.subcommands, path));
        path.pop();
        if !errors.is_empty() {
            return errors;
        }
    }

    errors
}

fn validate_flags(flags: &[FlagSchema]) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let mut seen = HashSet::new();

    for flag in flags {
        if flag.short.is_none() && flag.long.is_none() {
            errors.push(ValidationError::MissingFlagName);
            return errors;
        }

        if let Some(short) = &flag.short {
            if !short.starts_with('-') || short.starts_with("--") || short.len() < 2 {
                errors.push(ValidationError::InvalidShortFlag(short.clone()));
                return errors;
            }
            if !seen.insert(short.clone()) {
                errors.push(ValidationError::DuplicateFlag(short.clone()));
                return errors;
            }
        }

        if let Some(long) = &flag.long {
            if !long.starts_with("--") || long.len() < 3 {
                errors.push(ValidationError::InvalidLongFlag(long.clone()));
                return errors;
            }
            if !seen.insert(long.clone()) {
                errors.push(ValidationError::DuplicateFlag(long.clone()));
                return errors;
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use crate::{SchemaSource, ValueType};

    use super::*;

    #[test]
    fn test_validate_package_rejects_duplicate_commands() {
        let mut package = SchemaPackage::new("1.0.0", "2026-02-07T00:00:00Z");
        package
            .schemas
            .push(CommandSchema::new("git", SchemaSource::Bootstrap));
        package
            .schemas
            .push(CommandSchema::new("git", SchemaSource::Bootstrap));

        let errors = validate_package(&package);
        assert_eq!(
            errors,
            vec![ValidationError::DuplicateCommand("git".to_string())]
        );
    }

    #[test]
    fn test_validate_schema_rejects_bad_short_flag() {
        let mut schema = CommandSchema::new("git", SchemaSource::Bootstrap);
        schema.global_flags.push(FlagSchema::with_value(
            Some("v"),
            Some("--verbose"),
            ValueType::Bool,
        ));

        let errors = validate_schema(&schema);
        assert_eq!(
            errors,
            vec![ValidationError::InvalidShortFlag("v".to_string())]
        );
    }

    #[test]
    fn test_validate_schema_rejects_subcommand_cycle() {
        let mut schema = CommandSchema::new("git", SchemaSource::Bootstrap);
        let mut remote = SubcommandSchema::new("remote");
        remote.subcommands.push(SubcommandSchema::new("git"));
        schema.subcommands.push(remote);

        let errors = validate_schema(&schema);
        assert_eq!(
            errors,
            vec![ValidationError::SubcommandCycle("git remote git".to_string())]
        );
    }

    #[test]
    fn test_validate_schema_accepts_valid_schema() {
        let mut schema = CommandSchema::new("git", SchemaSource::Bootstrap);
        schema
            .global_flags
            .push(FlagSchema::boolean(Some("-v"), Some("--verbose")));
        schema.subcommands.push(SubcommandSchema::new("commit"));

        let errors = validate_schema(&schema);
        assert!(errors.is_empty());
    }
}

