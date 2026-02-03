//! Command knowledge base for contextual suggestions.
//!
//! This module provides static knowledge about common CLI commands,
//! their subcommands, and file type associations.

use std::collections::HashMap;
use std::path::Path;

use once_cell::sync::Lazy;

/// Static command knowledge base singleton.
pub static COMMAND_KNOWLEDGE: Lazy<CommandKnowledge> = Lazy::new(CommandKnowledge::default);

/// Command knowledge base containing static information about CLI commands.
pub struct CommandKnowledge {
    /// Top-level commands.
    commands: Vec<&'static str>,
    /// Subcommands for each top-level command.
    subcommands: HashMap<&'static str, Vec<&'static str>>,
    /// Nested subcommands (e.g., git remote add).
    nested: HashMap<(&'static str, &'static str), Vec<&'static str>>,
    /// Commands recommended for specific file types.
    file_type_commands: HashMap<&'static str, Vec<&'static str>>,
    /// Commands recommended for specific filenames (without extension).
    filename_commands: HashMap<&'static str, Vec<&'static str>>,
}

impl Default for CommandKnowledge {
    fn default() -> Self {
        let mut subcommands = HashMap::new();
        let mut nested = HashMap::new();
        let mut file_type_commands = HashMap::new();
        let mut filename_commands = HashMap::new();

        // Git subcommands
        subcommands.insert(
            "git",
            vec![
                "add", "commit", "push", "pull", "fetch", "merge", "rebase",
                "branch", "checkout", "switch", "status", "log", "diff",
                "remote", "stash", "reset", "tag", "clone", "init", "cherry-pick",
                "bisect", "blame", "show", "restore", "worktree",
            ],
        );

        // Git nested commands
        nested.insert(
            ("git", "remote"),
            vec!["add", "remove", "-v", "show", "rename", "prune", "set-url"],
        );
        nested.insert(
            ("git", "stash"),
            vec!["list", "show", "pop", "apply", "drop", "clear", "push"],
        );
        nested.insert(
            ("git", "worktree"),
            vec!["add", "list", "remove", "prune"],
        );
        nested.insert(
            ("git", "bisect"),
            vec!["start", "good", "bad", "reset", "skip"],
        );

        // Docker subcommands
        subcommands.insert(
            "docker",
            vec![
                "run", "build", "pull", "push", "ps", "images", "exec",
                "logs", "stop", "start", "restart", "rm", "rmi", "compose",
                "network", "volume", "system", "inspect", "tag", "save", "load",
            ],
        );

        // Docker nested commands
        nested.insert(
            ("docker", "compose"),
            vec!["up", "down", "build", "logs", "ps", "exec", "restart", "pull"],
        );
        nested.insert(
            ("docker", "system"),
            vec!["prune", "df", "info", "events"],
        );
        nested.insert(
            ("docker", "network"),
            vec!["create", "ls", "rm", "inspect", "connect", "disconnect"],
        );
        nested.insert(
            ("docker", "volume"),
            vec!["create", "ls", "rm", "inspect", "prune"],
        );

        // Cargo subcommands
        subcommands.insert(
            "cargo",
            vec![
                "build", "run", "test", "check", "clippy", "fmt", "doc",
                "clean", "update", "add", "remove", "publish", "bench",
                "tree", "audit", "outdated", "fix", "install", "uninstall",
            ],
        );

        // npm subcommands
        subcommands.insert(
            "npm",
            vec![
                "install", "run", "test", "build", "start", "dev", "add",
                "remove", "update", "audit", "publish", "init", "ci",
                "link", "unlink", "exec", "outdated",
            ],
        );

        // yarn subcommands
        subcommands.insert(
            "yarn",
            vec![
                "install", "run", "test", "build", "start", "dev", "add",
                "remove", "upgrade", "audit", "publish", "init", "link",
            ],
        );

        // pnpm subcommands
        subcommands.insert(
            "pnpm",
            vec![
                "install", "run", "test", "build", "start", "dev", "add",
                "remove", "update", "audit", "publish", "init", "exec",
            ],
        );

        // kubectl subcommands
        subcommands.insert(
            "kubectl",
            vec![
                "get", "describe", "logs", "exec", "apply", "delete",
                "create", "edit", "scale", "rollout", "port-forward",
                "config", "cluster-info", "top", "patch", "label",
            ],
        );

        // kubectl nested commands
        nested.insert(
            ("kubectl", "rollout"),
            vec!["status", "history", "undo", "restart", "pause", "resume"],
        );
        nested.insert(
            ("kubectl", "config"),
            vec!["use-context", "get-contexts", "current-context", "view", "set-context"],
        );

        // systemctl subcommands
        subcommands.insert(
            "systemctl",
            vec![
                "start", "stop", "restart", "status", "enable", "disable",
                "reload", "daemon-reload", "is-active", "is-enabled",
                "list-units", "list-unit-files", "mask", "unmask",
            ],
        );

        // journalctl subcommands
        subcommands.insert(
            "journalctl",
            vec![
                "-f", "-u", "-b", "--since", "--until", "-n", "-p",
                "--disk-usage", "--vacuum-size", "--vacuum-time",
            ],
        );

        // File type commands
        file_type_commands.insert("rs", vec!["cargo run", "cargo test", "rustfmt", "vim", "nano", "cat"]);
        file_type_commands.insert("py", vec!["python", "python3", "pytest", "vim", "nano", "cat"]);
        file_type_commands.insert("js", vec!["node", "npm run", "vim", "nano", "cat"]);
        file_type_commands.insert("ts", vec!["npx ts-node", "npm run", "vim", "nano", "cat"]);
        file_type_commands.insert("sh", vec!["bash", "./", "chmod +x", "vim", "nano", "cat"]);
        file_type_commands.insert("bash", vec!["bash", "./", "chmod +x", "vim", "nano", "cat"]);
        file_type_commands.insert("zsh", vec!["zsh", "./", "chmod +x", "vim", "nano", "cat"]);
        file_type_commands.insert("json", vec!["cat", "jq .", "vim", "nano", "less"]);
        file_type_commands.insert("yaml", vec!["cat", "vim", "nano", "less"]);
        file_type_commands.insert("yml", vec!["cat", "vim", "nano", "less"]);
        file_type_commands.insert("toml", vec!["cat", "vim", "nano", "less"]);
        file_type_commands.insert("md", vec!["cat", "vim", "nano", "less", "glow"]);
        file_type_commands.insert("txt", vec!["cat", "vim", "nano", "less", "head", "tail"]);
        file_type_commands.insert("log", vec!["tail -f", "less", "cat", "grep"]);
        file_type_commands.insert("gz", vec!["tar -xzf", "tar -tzf", "zcat", "gunzip"]);
        file_type_commands.insert("tar", vec!["tar -xf", "tar -tf"]);
        file_type_commands.insert("zip", vec!["unzip", "unzip -l"]);
        file_type_commands.insert("png", vec!["xdg-open", "feh", "imv", "file"]);
        file_type_commands.insert("jpg", vec!["xdg-open", "feh", "imv", "file"]);
        file_type_commands.insert("jpeg", vec!["xdg-open", "feh", "imv", "file"]);
        file_type_commands.insert("gif", vec!["xdg-open", "feh", "imv", "file"]);
        file_type_commands.insert("svg", vec!["xdg-open", "inkscape", "file"]);
        file_type_commands.insert("pdf", vec!["xdg-open", "zathura", "evince", "file"]);
        file_type_commands.insert("html", vec!["xdg-open", "firefox", "chromium", "vim", "nano"]);
        file_type_commands.insert("css", vec!["vim", "nano", "cat", "less"]);
        file_type_commands.insert("sql", vec!["cat", "vim", "nano", "sqlite3"]);
        file_type_commands.insert("db", vec!["sqlite3"]);

        // Filename-specific commands (files without extensions)
        filename_commands.insert("Makefile", vec!["make", "make -n", "cat", "vim"]);
        filename_commands.insert("makefile", vec!["make", "make -n", "cat", "vim"]);
        filename_commands.insert("Dockerfile", vec!["docker build -t", "cat", "vim"]);
        filename_commands.insert("docker-compose.yml", vec!["docker compose up", "docker compose down", "cat"]);
        filename_commands.insert("docker-compose.yaml", vec!["docker compose up", "docker compose down", "cat"]);
        filename_commands.insert("Cargo.toml", vec!["cargo build", "cargo run", "cat", "vim"]);
        filename_commands.insert("package.json", vec!["npm install", "npm run", "cat", "vim"]);
        filename_commands.insert("requirements.txt", vec!["pip install -r", "cat", "vim"]);
        filename_commands.insert("Pipfile", vec!["pipenv install", "cat", "vim"]);
        filename_commands.insert("Gemfile", vec!["bundle install", "cat", "vim"]);
        filename_commands.insert("go.mod", vec!["go build", "go run .", "cat", "vim"]);
        filename_commands.insert(".env", vec!["cat", "vim", "source"]);
        filename_commands.insert(".gitignore", vec!["cat", "vim"]);

        // Common top-level commands
        let commands = vec![
            "git", "docker", "cargo", "npm", "yarn", "pnpm", "kubectl",
            "systemctl", "journalctl", "ls", "cd", "cat", "vim", "nano",
            "grep", "find", "make", "python", "python3", "node", "go",
            "curl", "wget", "ssh", "scp", "rsync", "tar", "zip", "unzip",
            "chmod", "chown", "mkdir", "rm", "cp", "mv", "ln", "touch",
            "head", "tail", "less", "more", "diff", "sort", "uniq", "wc",
            "awk", "sed", "xargs", "tee", "sudo", "su", "htop", "top",
            "ps", "kill", "pkill", "man", "which", "whereis", "type",
            "echo", "printf", "env", "export", "alias", "history",
        ];

        Self {
            commands,
            subcommands,
            nested,
            file_type_commands,
            filename_commands,
        }
    }
}

impl CommandKnowledge {
    /// Returns suggestions for the next token based on preceding tokens.
    ///
    /// # Arguments
    ///
    /// * `preceding_tokens` - Tokens that come before the position being suggested
    ///
    /// # Returns
    ///
    /// A vector of suggested tokens for the next position.
    pub fn suggestions_for_position(&self, preceding_tokens: &[&str]) -> Vec<&'static str> {
        match preceding_tokens.len() {
            0 => {
                // First token: suggest top-level commands
                self.commands.clone()
            }
            1 => {
                // Second token: suggest subcommands for the first token
                let cmd = preceding_tokens[0];
                self.subcommands
                    .get(cmd)
                    .cloned()
                    .unwrap_or_default()
            }
            2 => {
                // Third token: check for nested subcommands
                let cmd = preceding_tokens[0];
                let subcmd = preceding_tokens[1];
                self.nested
                    .get(&(cmd, subcmd))
                    .cloned()
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// Returns whether a command has known subcommands.
    pub fn has_subcommands(&self, command: &str) -> bool {
        self.subcommands.contains_key(command)
    }

    /// Returns command recommendations for a file based on its type/name.
    ///
    /// # Arguments
    ///
    /// * `filename` - The name of the file (can include path)
    ///
    /// # Returns
    ///
    /// A vector of recommended commands for this file type.
    pub fn commands_for_filetype(&self, filename: &str) -> Vec<&'static str> {
        // First check for exact filename matches
        let base_name = Path::new(filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(filename);

        if let Some(commands) = self.filename_commands.get(base_name) {
            return commands.clone();
        }

        // Then check extension
        let extension = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // Handle compound extensions like .tar.gz
        if filename.ends_with(".tar.gz") || filename.ends_with(".tgz") {
            return self.file_type_commands
                .get("gz")
                .cloned()
                .unwrap_or_default();
        }

        self.file_type_commands
            .get(extension)
            .cloned()
            .unwrap_or_else(|| vec!["cat", "vim", "nano", "less"])
    }

    /// Returns commands that can receive piped input.
    ///
    /// These commands are suggested after a `|` in the suffix position.
    pub fn pipeable_commands(&self) -> Vec<&'static str> {
        vec![
            "grep", "grep -i", "grep -v",
            "head", "head -n 10",
            "tail", "tail -n 10",
            "sort", "sort -r", "sort -n",
            "uniq", "uniq -c",
            "wc", "wc -l",
            "less", "more",
            "cat",
            "tee",
            "xargs",
            "awk", "sed",
            "cut", "cut -d' ' -f1",
            "tr",
            "jq", "jq .",
            "bat",
            "fzf",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suggestions_for_first_token() {
        let suggestions = COMMAND_KNOWLEDGE.suggestions_for_position(&[]);
        assert!(suggestions.contains(&"git"));
        assert!(suggestions.contains(&"docker"));
        assert!(suggestions.contains(&"cargo"));
    }

    #[test]
    fn test_suggestions_for_git_subcommands() {
        let suggestions = COMMAND_KNOWLEDGE.suggestions_for_position(&["git"]);
        assert!(suggestions.contains(&"commit"));
        assert!(suggestions.contains(&"push"));
        assert!(suggestions.contains(&"pull"));
        assert!(suggestions.contains(&"remote"));
    }

    #[test]
    fn test_suggestions_for_git_remote() {
        let suggestions = COMMAND_KNOWLEDGE.suggestions_for_position(&["git", "remote"]);
        assert!(suggestions.contains(&"add"));
        assert!(suggestions.contains(&"remove"));
        assert!(suggestions.contains(&"-v"));
    }

    #[test]
    fn test_suggestions_for_unknown_command() {
        let suggestions = COMMAND_KNOWLEDGE.suggestions_for_position(&["unknown_cmd"]);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_has_subcommands() {
        assert!(COMMAND_KNOWLEDGE.has_subcommands("git"));
        assert!(COMMAND_KNOWLEDGE.has_subcommands("docker"));
        assert!(!COMMAND_KNOWLEDGE.has_subcommands("ls"));
    }

    #[test]
    fn test_commands_for_rust_file() {
        let commands = COMMAND_KNOWLEDGE.commands_for_filetype("main.rs");
        assert!(commands.contains(&"cargo run"));
        assert!(commands.contains(&"cargo test"));
    }

    #[test]
    fn test_commands_for_shell_script() {
        let commands = COMMAND_KNOWLEDGE.commands_for_filetype("script.sh");
        assert!(commands.contains(&"bash"));
        assert!(commands.contains(&"./"));
    }

    #[test]
    fn test_commands_for_makefile() {
        let commands = COMMAND_KNOWLEDGE.commands_for_filetype("Makefile");
        assert!(commands.contains(&"make"));
    }

    #[test]
    fn test_commands_for_dockerfile() {
        let commands = COMMAND_KNOWLEDGE.commands_for_filetype("Dockerfile");
        assert!(commands.contains(&"docker build -t"));
    }

    #[test]
    fn test_commands_for_tar_gz() {
        let commands = COMMAND_KNOWLEDGE.commands_for_filetype("archive.tar.gz");
        assert!(commands.contains(&"tar -xzf"));
    }

    #[test]
    fn test_commands_for_unknown_extension() {
        let commands = COMMAND_KNOWLEDGE.commands_for_filetype("file.xyz");
        // Should return default commands
        assert!(commands.contains(&"cat"));
        assert!(commands.contains(&"vim"));
    }
}
