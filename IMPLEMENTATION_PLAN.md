# Chrome Panel Enhancement Implementation Plan

## Overview

This plan covers enhancements to wrashpty's chrome panel system, focusing on the Edit Command feature with contextual token cycling and the Files tab improvements.

## Current Architecture

### Key Files
- `src/chrome/tabbed_panel.rs` - Tab management, currently orders: [Commands, Files, History, Help]
- `src/chrome/file_browser.rs` - File browser with `DirEntry` struct (name, path, is_dir, size, modified)
- `src/chrome/history_browser.rs` - History browser with `EditModeState` for token-based command editing
- `src/chrome/core.rs` - Chrome layer, scroll regions, panel height management
- `src/history_store.rs` - SQLite storage for command history

### Current Edit Command (History Tab)
- Tokenizes commands into: Command, Subcommand, Flag, Path, Url, Argument
- Token strip UI: `¹⟦git⟧  ²⟦remote⟧  ³⟦add⟧  ⁴⟦origin⟧`
- Left/Right arrows navigate tokens, typing edits current token
- No up/down cycling currently

---

## Phase 1: Quick Wins

### 1.1 Reorder Tabs - History First
**File:** `src/chrome/tabbed_panel.rs`

Change tab order from `[Commands, Files, History, Help]` to `[History, Files, Commands, Help]`.

Update the `panels` vector initialization and set `active_tab: 0` to open History by default.

### 1.2 File Metadata Display
**File:** `src/chrome/file_browser.rs`

#### 1.2.1 Extend `DirEntry` struct
```rust
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub mode: u32,  // NEW: Unix permissions (e.g., 0o755)
}
```

#### 1.2.2 Collect permissions
```rust
use std::os::unix::fs::MetadataExt;
let mode = metadata.as_ref().map(|m| m.mode() & 0o777).unwrap_or(0);
```

#### 1.2.3 Update rendering
Display format: `📄 filename.rs    755  Jan 15    1.2K`
- Permissions as 3-digit octal (755, 644, etc.)
- Date as compact format (Jan 15, or "Today", "Yesterday")
- Truncate filename if needed to fit

### 1.3 Remember Last Tab (State Persistence)
**Files:** `src/history_store.rs`, `src/chrome/tabbed_panel.rs`

#### 1.3.1 Add settings table to SQLite
```sql
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

#### 1.3.2 Add methods to HistoryStore
```rust
pub fn get_setting(&self, key: &str) -> Option<String>;
pub fn set_setting(&self, key: &str, value: &str) -> Result<()>;
```

#### 1.3.3 Load/save active tab
- On panel open: Load `"last_active_tab"` setting, default to 0 (History)
- On tab switch: Save new tab index to settings

---

## Phase 2: Command Knowledge Base

### 2.1 Create Knowledge Module
**New file:** `src/chrome/command_knowledge.rs`

This module provides contextual suggestions for command tokens based on the command chain.

#### 2.1.1 Core Data Structure
```rust
use std::collections::HashMap;

/// Knowledge base for command completion and cycling
pub struct CommandKnowledge {
    /// Primary commands (git, docker, cargo, npm, etc.)
    commands: Vec<&'static str>,

    /// Subcommands for each command
    /// Key: command name, Value: list of subcommands
    subcommands: HashMap<&'static str, Vec<&'static str>>,

    /// Nested subcommands (e.g., "git remote" → ["add", "remove", "-v"])
    /// Key: (command, subcommand), Value: list of options
    nested: HashMap<(&'static str, &'static str), Vec<&'static str>>,

    /// Common flags per command
    flags: HashMap<&'static str, Vec<&'static str>>,
}
```

#### 2.1.2 Contextual Suggestion Method
```rust
impl CommandKnowledge {
    /// Get suggestions for a token position given preceding tokens
    ///
    /// # Arguments
    /// * `preceding_tokens` - Tokens before the current position (e.g., ["git", "remote"])
    ///
    /// # Returns
    /// * List of suggestions for the current position, or empty if no suggestions
    pub fn suggestions_for_position(&self, preceding_tokens: &[&str]) -> Vec<&'static str> {
        match preceding_tokens.len() {
            0 => self.commands.clone(),  // Position 0: all commands
            1 => {
                // Position 1: subcommands for the command
                let cmd = preceding_tokens[0];
                self.subcommands.get(cmd).cloned().unwrap_or_default()
            }
            2 => {
                // Position 2: nested options
                let cmd = preceding_tokens[0];
                let sub = preceding_tokens[1];
                self.nested.get(&(cmd, sub)).cloned().unwrap_or_default()
            }
            _ => vec![]  // Beyond our knowledge
        }
    }

    /// Check if a command has known subcommands (i.e., cycling makes sense)
    pub fn has_subcommands(&self, command: &str) -> bool {
        self.subcommands.contains_key(command)
    }
}
```

#### 2.1.3 Initial Knowledge Data
```rust
impl Default for CommandKnowledge {
    fn default() -> Self {
        let mut subcommands = HashMap::new();
        let mut nested = HashMap::new();
        let mut flags = HashMap::new();

        // Git
        subcommands.insert("git", vec![
            "add", "commit", "push", "pull", "fetch", "checkout", "branch",
            "merge", "rebase", "reset", "stash", "log", "diff", "status",
            "remote", "clone", "init", "tag", "cherry-pick", "bisect"
        ]);
        nested.insert(("git", "remote"), vec!["add", "remove", "rename", "-v", "show", "prune"]);
        nested.insert(("git", "branch"), vec!["-d", "-D", "-m", "-a", "-r", "--list"]);
        nested.insert(("git", "stash"), vec!["pop", "apply", "drop", "list", "show", "clear"]);
        nested.insert(("git", "reset"), vec!["--soft", "--hard", "--mixed", "HEAD~1"]);
        flags.insert("git", vec!["--version", "--help", "-C"]);

        // Docker
        subcommands.insert("docker", vec![
            "run", "build", "pull", "push", "exec", "ps", "images",
            "container", "image", "volume", "network", "compose",
            "stop", "start", "rm", "rmi", "logs", "inspect"
        ]);
        nested.insert(("docker", "compose"), vec!["up", "down", "build", "logs", "ps", "exec", "-f"]);
        nested.insert(("docker", "container"), vec!["ls", "rm", "prune", "inspect", "logs"]);

        // Cargo
        subcommands.insert("cargo", vec![
            "build", "run", "test", "check", "clippy", "fmt", "doc",
            "add", "remove", "update", "publish", "install", "new", "init"
        ]);
        nested.insert(("cargo", "build"), vec!["--release", "--features", "--all-features"]);
        nested.insert(("cargo", "run"), vec!["--release", "--bin", "--example"]);
        nested.insert(("cargo", "test"), vec!["--release", "--lib", "--doc", "--", "--nocapture"]);

        // npm/yarn/pnpm
        subcommands.insert("npm", vec![
            "install", "run", "test", "build", "start", "publish",
            "init", "update", "outdated", "audit", "ci", "exec"
        ]);
        subcommands.insert("yarn", vec![
            "install", "add", "remove", "run", "build", "test", "start"
        ]);
        subcommands.insert("pnpm", vec![
            "install", "add", "remove", "run", "build", "test", "start"
        ]);

        // Kubectl
        subcommands.insert("kubectl", vec![
            "get", "describe", "apply", "delete", "logs", "exec",
            "create", "edit", "scale", "rollout", "port-forward"
        ]);
        nested.insert(("kubectl", "get"), vec!["pods", "services", "deployments", "nodes", "namespaces", "-o", "yaml", "json"]);

        // Systemctl
        subcommands.insert("systemctl", vec![
            "start", "stop", "restart", "status", "enable", "disable",
            "daemon-reload", "list-units", "list-unit-files"
        ]);

        // Simple commands (no subcommands - cycling doesn't apply after position 0)
        // nano, vim, cat, less, grep, find, etc. - these take files/patterns, not subcommands

        let commands = vec![
            "git", "docker", "cargo", "npm", "yarn", "pnpm", "kubectl", "systemctl",
            "cat", "less", "vim", "nano", "grep", "find", "ls", "cd", "mkdir", "rm",
            "cp", "mv", "chmod", "chown", "curl", "wget", "ssh", "scp", "rsync",
            "python", "python3", "node", "ruby", "go", "rustc", "gcc", "make"
        ];

        Self { commands, subcommands, nested, flags }
    }
}
```

### 2.2 History-Based Suggestions
**File:** `src/history_store.rs`

Add method to query what tokens have historically appeared at position N given preceding tokens:

```rust
/// Get historically used tokens at a position given preceding context
///
/// Example: tokens_at_position(&["git", "remote"]) returns tokens that
/// appeared at position 2 when the command started with "git remote"
pub fn tokens_at_position(&self, preceding: &[&str]) -> Result<Vec<String>> {
    // Query history for commands matching the prefix pattern
    // Extract and count unique tokens at the target position
    // Return sorted by frequency (most common first)
}
```

---

## Phase 3: Enhanced Edit Command UI (3-Row Depth)

### 3.1 Increase Panel Height
**File:** `src/chrome/core.rs`

Increase the default/minimum panel height to accommodate the new UI:
- Current: ~8 rows for edit mode
- New: ~11 rows (add 2 for prev/next suggestion rows, 1 buffer)

### 3.2 Token Cycling State
**File:** `src/chrome/history_browser.rs`

Extend `EditModeState`:

```rust
struct EditModeState {
    // ... existing fields ...

    /// Suggestions for the currently selected token
    current_suggestions: Vec<String>,

    /// Index into current_suggestions (None = using custom/typed value)
    suggestion_index: Option<usize>,

    /// Reference to command knowledge base
    knowledge: &'static CommandKnowledge,
}
```

### 3.3 Suggestion Computation
When selected token changes or preceding tokens are modified:

```rust
fn update_suggestions(&mut self) {
    // Get preceding tokens (tokens before selected index)
    let preceding: Vec<&str> = self.tokens[..self.selected]
        .iter()
        .map(|t| t.text.as_str())
        .collect();

    // Get suggestions from knowledge base
    let mut suggestions = self.knowledge.suggestions_for_position(&preceding);

    // Merge with history-based suggestions (deduplicate, history items first)
    // ...

    self.current_suggestions = suggestions;
    self.suggestion_index = None;  // Start with current value, not a suggestion
}
```

### 3.4 Up/Down Cycling Logic
```rust
fn cycle_suggestion(&mut self, direction: i32) {
    if self.current_suggestions.is_empty() {
        return;  // No suggestions available for this position
    }

    match self.suggestion_index {
        None => {
            // First press: enter suggestion mode
            self.suggestion_index = Some(if direction > 0 { 0 } else {
                self.current_suggestions.len() - 1
            });
        }
        Some(idx) => {
            // Cycle through suggestions
            let new_idx = (idx as i32 + direction)
                .rem_euclid(self.current_suggestions.len() as i32) as usize;
            self.suggestion_index = Some(new_idx);
        }
    }

    // Update the current token with the selected suggestion
    if let Some(idx) = self.suggestion_index {
        self.tokens[self.selected].text = self.current_suggestions[idx].clone();
        self.edit_buffer = self.current_suggestions[idx].clone();
    }
}
```

### 3.5 Three-Row Rendering
**File:** `src/chrome/history_browser.rs`

Update the token strip rendering to show 3 rows:

```rust
fn render_token_strip(&self, area: Rect, buf: &mut Buffer) {
    let suggestions = &self.current_suggestions;
    let current_idx = self.suggestion_index.unwrap_or(0);

    // Calculate prev/next indices
    let prev_idx = current_idx.checked_sub(1).unwrap_or(suggestions.len().saturating_sub(1));
    let next_idx = (current_idx + 1) % suggestions.len().max(1);

    // Row 1 (top): Previous suggestion - dim gray, no brackets
    // Only show for the selected token column
    let prev_row = area.y;
    // Render tokens, but for selected token show suggestions[prev_idx] in DarkGray

    // Row 2 (middle): Current tokens - bold, bright, with brackets
    let current_row = area.y + 1;
    // Existing token strip rendering with ⟦brackets⟧

    // Row 3 (bottom): Next suggestion - dim gray, no brackets
    let next_row = area.y + 2;
    // Render tokens, but for selected token show suggestions[next_idx] in DarkGray
}
```

Visual result:
```
              remote                                    ← dim (prev suggestion)
   ¹⟦git⟧    ²⟦add⟧     ³⟦origin⟧  ⁴⟦git@github...⟧   ← bold (current)
              fetch                                     ← dim (next suggestion)
```

Only the currently selected token shows the prev/next suggestions; other tokens show static (grayed out to match).

---

## Phase 4: Files Tab Edit Command Mode

### 4.1 File Edit Mode State
**File:** `src/chrome/file_browser.rs`

The Files tab edit mode differs from History:
- **History**: Starts with existing command, edit any token
- **Files**: Starts with filename, add prefix (command) and suffix (arguments)

```rust
struct FileEditModeState {
    /// The filename (immutable, always in the middle)
    filename: String,

    /// Full path to the file
    filepath: PathBuf,

    /// Prefix tokens (before filename) - e.g., ["vim"], ["git", "add"]
    prefix_tokens: Vec<String>,

    /// Suffix tokens (after filename) - e.g., arguments, flags
    suffix_tokens: Vec<String>,

    /// Which section is selected: Prefix, Filename (view only), or Suffix
    selected_section: FileEditSection,

    /// Index within the selected section
    selected_index: usize,

    /// Current edit buffer
    edit_buffer: String,

    /// Suggestions based on file type and history
    suggestions: Vec<String>,

    /// Current suggestion index
    suggestion_index: Option<usize>,
}

enum FileEditSection {
    Prefix,   // Cursor in prefix area (command, subcommands)
    Filename, // Cursor on filename (non-editable, visual only)
    Suffix,   // Cursor in suffix area (additional args)
}
```

### 4.2 File Type Command Recommendations
**File:** `src/chrome/command_knowledge.rs`

Add file-type-based command suggestions:

```rust
impl CommandKnowledge {
    /// Get recommended commands for a file based on its extension
    pub fn commands_for_filetype(&self, filename: &str) -> Vec<&'static str> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        match ext {
            // Source code
            "rs" => vec!["cargo run --bin", "rustfmt", "cargo test", "vim", "cat"],
            "py" => vec!["python", "python3", "pytest", "vim", "cat"],
            "js" | "ts" => vec!["node", "npm run", "vim", "cat"],
            "go" => vec!["go run", "go build", "go test", "vim", "cat"],
            "rb" => vec!["ruby", "vim", "cat"],
            "sh" | "bash" => vec!["bash", "./", "chmod +x", "vim", "cat"],

            // Config/data
            "json" => vec!["cat", "jq", "vim", "less"],
            "yaml" | "yml" => vec!["cat", "yq", "vim", "less"],
            "toml" => vec!["cat", "vim", "less"],
            "xml" => vec!["cat", "xmllint", "vim", "less"],

            // Documents
            "md" | "txt" => vec!["cat", "less", "vim", "bat"],
            "pdf" => vec!["xdg-open", "evince", "zathura"],

            // Archives
            "tar" | "tar.gz" | "tgz" => vec!["tar -xzf", "tar -tzf"],
            "zip" => vec!["unzip", "unzip -l"],
            "gz" => vec!["gunzip", "zcat"],

            // Images
            "png" | "jpg" | "jpeg" | "gif" | "webp" => vec!["xdg-open", "feh", "imv"],

            // Default
            _ => vec!["cat", "less", "vim", "file"],
        }
    }
}
```

### 4.3 Rendering File Edit Mode
```
┌─────────────────────────────────────────────────────────────────────┐
│ Edit Command for: main.rs                                           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│        rustfmt                                      ← dim (prev)     │
│   ¹⟦cargo run --bin⟧    main.rs                    ← bold (current) │
│        cargo test                                   ← dim (next)     │
│                                                                      │
│   ¹ prefix > cargo run --bin█                                        │
│                                                                      │
│ Result: cargo run --bin main.rs                                      │
├─────────────────────────────────────────────────────────────────────┤
│ ↔ Nav  ↕ Cycle  Tab Section  Enter Run  Esc Back                    │
└─────────────────────────────────────────────────────────────────────┘
```

The filename is displayed but not editable. User starts in prefix section (slot 1), can Tab to suffix section (slot 3).

### 4.4 Navigation in File Edit Mode
- **Left/Right**: Move between tokens within current section
- **Tab/Shift+Tab**: Move between Prefix → Filename → Suffix sections
- **Up/Down**: Cycle through suggestions for current position
- **Typing**: Add/modify token in current position
- **Ctrl+A/I**: Insert new token after/before current
- **Ctrl+D**: Delete current token
- **Enter**: Execute the composed command
- **Esc**: Exit edit mode

---

## Shared Components

### TokenCycler Trait
To share cycling logic between History and Files edit modes:

```rust
/// Trait for components that support token cycling with suggestions
trait TokenCycler {
    /// Get suggestions for the current position
    fn current_suggestions(&self) -> &[String];

    /// Get the current suggestion index (None = using typed value)
    fn suggestion_index(&self) -> Option<usize>;

    /// Set the suggestion index
    fn set_suggestion_index(&mut self, idx: Option<usize>);

    /// Apply the selected suggestion to the current token
    fn apply_suggestion(&mut self, suggestion: &str);

    /// Cycle to next/previous suggestion
    fn cycle(&mut self, direction: i32) {
        let suggestions = self.current_suggestions();
        if suggestions.is_empty() {
            return;
        }

        let new_idx = match self.suggestion_index() {
            None => {
                if direction > 0 { 0 } else { suggestions.len() - 1 }
            }
            Some(idx) => {
                (idx as i32 + direction).rem_euclid(suggestions.len() as i32) as usize
            }
        };

        self.set_suggestion_index(Some(new_idx));
        self.apply_suggestion(&suggestions[new_idx].clone());
    }
}
```

### ThreeRowTokenDisplay
Shared rendering helper for the 3-row depth effect:

```rust
struct ThreeRowTokenDisplay<'a> {
    /// All tokens to display
    tokens: &'a [String],

    /// Which token index is selected
    selected: usize,

    /// Suggestions for the selected token
    suggestions: &'a [String],

    /// Current suggestion index (for prev/next calculation)
    suggestion_index: Option<usize>,
}

impl ThreeRowTokenDisplay<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        // Row 0: Previous suggestions (dim)
        // Row 1: Current tokens (bold, with brackets on selected)
        // Row 2: Next suggestions (dim)
    }
}
```

---

## Implementation Order

1. **Phase 1.1**: Reorder tabs (5 min)
2. **Phase 1.2**: File metadata display (30 min)
3. **Phase 1.3**: Settings persistence (45 min)
4. **Phase 2.1**: Command knowledge base (2 hrs)
5. **Phase 3.1**: Increase panel height (15 min)
6. **Phase 3.2-3.4**: Token cycling state and logic (1.5 hrs)
7. **Phase 3.5**: Three-row rendering (1.5 hrs)
8. **Phase 4**: Files tab edit mode (3 hrs)

Total estimated time: ~10-12 hours

---

## Testing Considerations

1. **Unit tests** for `CommandKnowledge`:
   - Verify suggestions for known commands
   - Verify empty suggestions for unknown commands
   - Verify file type recommendations

2. **Integration tests** for cycling:
   - Cycling wraps around correctly
   - Context updates when preceding tokens change
   - History-based suggestions merge correctly

3. **Visual testing** (manual):
   - Three-row display aligns correctly
   - Dim/bold styling is visible
   - Panel height is sufficient
   - Scrolling works when many tokens

---

## Notes for Implementers

1. **The `CommandKnowledge` should be a static singleton** - initialize once with `lazy_static!` or `once_cell`

2. **Contextual awareness is critical**: When user changes token N, recalculate suggestions for all tokens > N because the context has changed

3. **Handle edge cases**:
   - Empty suggestions list (no prev/next to show)
   - Single suggestion (prev == next == current)
   - Very long tokens (truncate in display)

4. **Preserve user edits**: If user types a custom value not in suggestions, track that `suggestion_index = None` means "using custom value". Pressing up/down enters suggestion mode.

5. **File edit mode filename is immutable**: The filename token cannot be edited, only moved between prefix/suffix sections.
