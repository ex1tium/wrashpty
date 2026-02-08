//! Constraint extraction and command hierarchy validation.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use command_schema_core::{FlagSchema, SubcommandSchema};

#[derive(Debug, Clone, Default)]
struct TrieNode {
    children: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CommandTrie {
    nodes: HashMap<String, TrieNode>,
}

impl CommandTrie {
    pub fn insert(&mut self, path: &[String]) -> Result<(), String> {
        if path.len() < 2 {
            return Ok(());
        }

        for window in path.windows(2) {
            let parent = window[0].trim();
            let child = window[1].trim();
            if parent == child {
                return Err(format!("self-cycle detected for command '{parent}'"));
            }

            self.nodes.entry(parent.to_string()).or_default();
            self.nodes.entry(child.to_string()).or_default();
            if let Some(node) = self.nodes.get_mut(parent) {
                node.children.insert(child.to_string());
            }
        }

        Ok(())
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();

        for name in self.nodes.keys() {
            self.visit(name, &mut visiting, &mut visited, &mut errors);
        }

        errors
    }

    fn visit(
        &self,
        node: &str,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
        errors: &mut Vec<String>,
    ) {
        if visited.contains(node) {
            return;
        }
        if !visiting.insert(node.to_string()) {
            errors.push(format!("cycle detected at command '{node}'"));
            return;
        }

        if let Some(entry) = self.nodes.get(node) {
            for child in &entry.children {
                self.visit(child, visiting, visited, errors);
            }
        }

        visiting.remove(node);
        visited.insert(node.to_string());
    }
}

pub fn validate_subcommand_hierarchy(
    command: &str,
    subcommands: &[SubcommandSchema],
) -> Vec<String> {
    let mut trie = CommandTrie::default();
    let root = command.to_string();

    for sub in subcommands {
        let path = vec![root.clone(), sub.name.clone()];
        if let Err(error) = trie.insert(&path) {
            return vec![error];
        }
    }

    trie.validate()
}

pub fn extract_flag_relationships(
    description: &str,
    all_flags: &[String],
) -> (Vec<String>, Vec<String>) {
    static FLAG_REF_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(--[a-zA-Z][-a-zA-Z0-9.]*|-[a-zA-Z0-9?@]{1,3})").unwrap());

    let lower = description.to_ascii_lowercase();
    let mut requires = Vec::new();
    let mut conflicts = Vec::new();

    if lower.contains("requires")
        || lower.contains("must be used with")
        || lower.contains("only with")
    {
        for capture in FLAG_REF_RE.captures_iter(description) {
            let candidate = capture[1].to_string();
            if all_flags.contains(&candidate) && !requires.contains(&candidate) {
                requires.push(candidate);
            }
        }
    }

    if lower.contains("conflicts")
        || lower.contains("conflicts with")
        || lower.contains("mutually exclusive")
        || lower.contains("cannot be used with")
    {
        for capture in FLAG_REF_RE.captures_iter(description) {
            let candidate = capture[1].to_string();
            if all_flags.contains(&candidate) && !conflicts.contains(&candidate) {
                conflicts.push(candidate);
            }
        }
    }

    (requires, conflicts)
}

pub fn apply_flag_relationships(flags: &mut [FlagSchema]) {
    let mut all = Vec::new();
    for flag in flags.iter() {
        if let Some(long) = &flag.long {
            all.push(long.clone());
        }
        if let Some(short) = &flag.short {
            all.push(short.clone());
        }
    }

    for flag in flags.iter_mut() {
        let Some(description) = flag.description.as_deref() else {
            continue;
        };

        let (requires, conflicts) = extract_flag_relationships(description, &all);

        for value in requires {
            if !flag.requires.contains(&value) {
                flag.requires.push(value);
            }
        }

        for value in conflicts {
            if !flag.conflicts_with.contains(&value) {
                flag.conflicts_with.push(value);
            }
        }

        if let Some(short) = flag.short.as_deref() {
            flag.requires.retain(|item| item != short);
            flag.conflicts_with.retain(|item| item != short);
        }
        if let Some(long) = flag.long.as_deref() {
            flag.requires.retain(|item| item != long);
            flag.conflicts_with.retain(|item| item != long);
        }
    }
}
