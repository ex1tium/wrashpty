use std::collections::HashMap;

use crate::{CommandSchema, FlagSchema, SubcommandSchema};

/// Schema merge behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Keep base values when conflicts occur.
    PreferBase,
    /// Keep overlay values when conflicts occur.
    PreferOverlay,
    /// Combine both with conflict-aware unions.
    Union,
}

/// Merges two command schemas into one schema.
pub fn merge_schemas(
    base: &CommandSchema,
    overlay: &CommandSchema,
    strategy: MergeStrategy,
) -> CommandSchema {
    let mut merged = base.clone();
    merged.command = base.command.clone();

    merged.description = match strategy {
        MergeStrategy::PreferBase => base.description.clone().or_else(|| overlay.description.clone()),
        MergeStrategy::PreferOverlay => {
            overlay.description.clone().or_else(|| base.description.clone())
        }
        MergeStrategy::Union => overlay.description.clone().or_else(|| base.description.clone()),
    };

    merged.global_flags = merge_flags(&base.global_flags, &overlay.global_flags, strategy);
    merged.subcommands = merge_subcommands(&base.subcommands, &overlay.subcommands, strategy);

    merged
}

fn merge_flags(base: &[FlagSchema], overlay: &[FlagSchema], strategy: MergeStrategy) -> Vec<FlagSchema> {
    let mut by_name: HashMap<String, FlagSchema> = HashMap::new();

    let insert = |map: &mut HashMap<String, FlagSchema>, flag: &FlagSchema| {
        let key = flag
            .long
            .clone()
            .or_else(|| flag.short.clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        map.insert(key, flag.clone());
    };

    match strategy {
        MergeStrategy::PreferBase => {
            for flag in base {
                insert(&mut by_name, flag);
            }
            for flag in overlay {
                let key = flag
                    .long
                    .clone()
                    .or_else(|| flag.short.clone())
                    .unwrap_or_else(|| "<unknown>".to_string());
                by_name.entry(key).or_insert_with(|| flag.clone());
            }
        }
        MergeStrategy::PreferOverlay | MergeStrategy::Union => {
            for flag in base {
                insert(&mut by_name, flag);
            }
            for flag in overlay {
                insert(&mut by_name, flag);
            }
        }
    }

    by_name.into_values().collect()
}

fn merge_subcommands(
    base: &[SubcommandSchema],
    overlay: &[SubcommandSchema],
    strategy: MergeStrategy,
) -> Vec<SubcommandSchema> {
    let mut map: HashMap<String, SubcommandSchema> = HashMap::new();

    for sub in base {
        map.insert(sub.name.clone(), sub.clone());
    }

    for sub in overlay {
        match map.get_mut(&sub.name) {
            Some(existing) => {
                *existing = merge_subcommand(existing, sub, strategy);
            }
            None => {
                map.insert(sub.name.clone(), sub.clone());
            }
        }
    }

    map.into_values().collect()
}

fn merge_subcommand(
    base: &SubcommandSchema,
    overlay: &SubcommandSchema,
    strategy: MergeStrategy,
) -> SubcommandSchema {
    let mut merged = base.clone();
    merged.description = match strategy {
        MergeStrategy::PreferBase => base.description.clone().or_else(|| overlay.description.clone()),
        MergeStrategy::PreferOverlay => {
            overlay.description.clone().or_else(|| base.description.clone())
        }
        MergeStrategy::Union => overlay.description.clone().or_else(|| base.description.clone()),
    };

    merged.flags = merge_flags(&base.flags, &overlay.flags, strategy);
    merged.subcommands = merge_subcommands(&base.subcommands, &overlay.subcommands, strategy);

    if strategy == MergeStrategy::PreferOverlay {
        merged.positional = overlay.positional.clone();
        merged.aliases = overlay.aliases.clone();
    } else {
        if merged.positional.is_empty() {
            merged.positional = overlay.positional.clone();
        }
        if merged.aliases.is_empty() {
            merged.aliases = overlay.aliases.clone();
        }
    }

    merged
}

#[cfg(test)]
mod tests {
    use crate::{SchemaSource, ValueType};

    use super::*;

    #[test]
    fn test_merge_prefer_base_keeps_base_description() {
        let mut base = CommandSchema::new("git", SchemaSource::Bootstrap);
        base.description = Some("base".to_string());
        let mut overlay = CommandSchema::new("git", SchemaSource::Learned);
        overlay.description = Some("overlay".to_string());

        let merged = merge_schemas(&base, &overlay, MergeStrategy::PreferBase);
        assert_eq!(merged.description.as_deref(), Some("base"));
    }

    #[test]
    fn test_merge_prefer_overlay_replaces_description() {
        let mut base = CommandSchema::new("git", SchemaSource::Bootstrap);
        base.description = Some("base".to_string());
        let mut overlay = CommandSchema::new("git", SchemaSource::Learned);
        overlay.description = Some("overlay".to_string());

        let merged = merge_schemas(&base, &overlay, MergeStrategy::PreferOverlay);
        assert_eq!(merged.description.as_deref(), Some("overlay"));
    }

    #[test]
    fn test_merge_union_deduplicates_flags() {
        let mut base = CommandSchema::new("git", SchemaSource::Bootstrap);
        base.global_flags
            .push(FlagSchema::boolean(Some("-v"), Some("--verbose")));

        let mut overlay = CommandSchema::new("git", SchemaSource::Learned);
        overlay.global_flags.push(FlagSchema::with_value(
            Some("-m"),
            Some("--message"),
            ValueType::String,
        ));
        overlay
            .global_flags
            .push(FlagSchema::boolean(Some("-v"), Some("--verbose")));

        let merged = merge_schemas(&base, &overlay, MergeStrategy::Union);
        assert_eq!(merged.global_flags.len(), 2);
    }
}

