# Reedline Capabilities & Innovation Ideas

> **Status**: Reference documentation for feature development
> **Note**: All features described here can be implemented WITHOUT forking reedline

## What is Reedline?

Reedline is a modern line editor library for Rust, built as a readline/libedit replacement. It was created for Nushell and provides:

- Syntax highlighting
- Completions with menus
- History with search
- Hints (ghost text suggestions)
- Vi and Emacs edit modes
- Multiline editing
- Unicode support

## Architecture Overview

```text
┌─────────────────────────────────────────────────────────────┐
│                        Reedline                              │
│                                                              │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐      │
│  │  EditMode   │    │   Prompt    │    │  Painter    │      │
│  │ (vi/emacs)  │    │ (your impl) │    │ (internal)  │      │
│  └─────────────┘    └─────────────┘    └─────────────┘      │
│         │                  │                  │              │
│         ▼                  ▼                  ▼              │
│  ┌─────────────────────────────────────────────────┐        │
│  │                  LineBuffer                      │        │
│  │         (the text being edited)                  │        │
│  └─────────────────────────────────────────────────┘        │
│         │                  │                  │              │
│         ▼                  ▼                  ▼              │
│  ┌───────────┐    ┌─────────────┐    ┌─────────────┐        │
│  │ Completer │    │   Hinter    │    │ Highlighter │        │
│  └───────────┘    └─────────────┘    └─────────────┘        │
│         │                  │                  │              │
│         ▼                  ▼                  ▼              │
│  ┌───────────┐    ┌─────────────┐    ┌─────────────┐        │
│  │   Menu    │    │  Validator  │    │   History   │        │
│  └───────────┘    └─────────────┘    └─────────────┘        │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

## Extension Points (Public Traits)

### 1. Prompt Trait

Full control over prompt appearance.

```rust
pub trait Prompt {
    /// Left side of prompt (e.g., "~/foo $")
    fn render_prompt_left(&self) -> Cow<str>;

    /// Right side of prompt (e.g., timestamp, git info)
    fn render_prompt_right(&self) -> Cow<str>;

    /// Prompt indicator (e.g., ">", "$", "#")
    fn render_prompt_indicator(&self, edit_mode: PromptEditMode) -> Cow<str>;

    /// For multiline input (e.g., "... ")
    fn render_prompt_multiline_indicator(&self) -> Cow<str>;

    /// During history search (e.g., "(search)`term': ")
    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch
    ) -> Cow<str>;

    /// ANSI styling for left prompt
    fn get_prompt_left_style(&self) -> Style;

    /// ANSI styling for right prompt
    fn get_prompt_right_style(&self) -> Style;
}
```

#### Innovation Ideas for Prompt

| Feature | Description |
|---------|-------------|
| **Git integration** | Branch name, dirty status, ahead/behind counts |
| **Exit code display** | Color-coded last command result |
| **Command timing** | Show duration of last command |
| **Vi mode indicator** | NORMAL/INSERT/VISUAL mode display |
| **Kubernetes context** | Current kubectl context/namespace |
| **Python virtualenv** | Active venv name |
| **SSH indicator** | Show when in SSH session |
| **Root warning** | Highlight when running as root |
| **Async data** | Compute git status in background, update on next render |

#### Example Rich Prompt

```text
  ✓ ~/projects/wrashpty  main ●  2.3s  k8s:prod
  │
  └─▶ $
```

---

### 2. Completer Trait

Tab completion logic.

```rust
pub trait Completer {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion>;
}

pub struct Suggestion {
    pub value: String,              // The completion text
    pub description: Option<String>, // Shown in menu
    pub extra: Option<Vec<String>>,  // Additional context
    pub span: Span,                  // What range to replace
    pub append_whitespace: bool,     // Add space after completion?
}
```

#### Innovation Ideas for Completions

| Feature | Description |
|---------|-------------|
| **Contextual completions** | Detect `docker run` → complete image names |
| **Fuzzy matching** | Don't require exact prefix match |
| **Frecency scoring** | Rank by frequency + recency |
| **Rich descriptions** | Show file sizes, git status, man page excerpts |
| **SSH host completion** | Parse `~/.ssh/config` and `~/.ssh/known_hosts` |
| **Environment variables** | `$HO<tab>` → `$HOME` |
| **Brace expansion preview** | Show `file{1,2,3}.txt` expanded |
| **Git completions** | Branches, tags, remotes, commit hashes |
| **Docker completions** | Containers, images, volumes, networks |
| **Kubernetes completions** | Pods, deployments, services, namespaces |
| **npm/cargo completions** | Package names, scripts |
| **Make targets** | Parse Makefile for targets |
| **History-based** | Suggest based on what you've typed before in similar contexts |

#### Completion Context Detection

```rust
impl Completer for SmartCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let context = parse_command_context(line, pos);

        match context {
            Context::Command => self.complete_commands(line, pos),
            Context::GitSubcommand => self.complete_git(line, pos),
            Context::DockerSubcommand => self.complete_docker(line, pos),
            Context::FilePath => self.complete_paths(line, pos),
            Context::Flag { command } => self.complete_flags(command, line, pos),
            Context::EnvVar => self.complete_env_vars(line, pos),
            _ => self.complete_default(line, pos),
        }
    }
}
```

---

### 3. Hinter Trait

Ghost text suggestions (fish-style).

```rust
pub trait Hinter {
    fn handle(
        &mut self,
        line: &str,
        pos: usize,
        history: &dyn History,
        use_ansi_coloring: bool,
    ) -> String;  // Returns the hint (ghost text)

    fn complete_hint(&self) -> String;  // Full text when accepting hint
}
```

#### Innovation Ideas for Hints

| Feature | Description |
|---------|-------------|
| **Multi-source hints** | Combine history + AI + command docs |
| **Contextual hints** | In git repo → suggest git commands |
| **Dangerous command warnings** | `rm -rf /` → show warning as hint |
| **Typo correction** | `gti status` → hint shows "git status" |
| **Alias expansion** | Show what alias expands to |
| **Command explanation** | Brief description of what command does |
| **Argument hints** | Show expected argument types |
| **Recent file hints** | Suggest recently accessed files |

#### Example: Warning Hinter

```rust
impl Hinter for WarningHinter {
    fn handle(&mut self, line: &str, pos: usize, ...) -> String {
        if line.contains("rm -rf /") || line.contains("rm -rf ~") {
            return "\x1b[31m ⚠ DANGER: This will delete important files!\x1b[0m".into();
        }
        if line.starts_with("sudo rm") {
            return "\x1b[33m ⚠ Running rm as root\x1b[0m".into();
        }
        // Fall back to history-based hints
        self.history_hinter.handle(line, pos, ...)
    }
}
```

---

### 4. Highlighter Trait

Syntax coloring for input.

```rust
pub trait Highlighter {
    fn highlight(&self, line: &str, cursor: usize) -> StyledText;
}
```

#### Innovation Ideas for Highlighting

| Feature | Description |
|---------|-------------|
| **Command validation** | Red if command doesn't exist in PATH |
| **Path validation** | Red/strikethrough if file doesn't exist |
| **Syntax highlighting** | Colors for commands, args, flags, strings, pipes |
| **Bracket matching** | Highlight matching `()`, `[]`, `{}` |
| **Quote matching** | Highlight unclosed quotes in warning color |
| **Variable highlighting** | Different color for `$VAR` |
| **Glob highlighting** | Highlight `*`, `?`, `[...]` patterns |
| **Error preview** | Highlight syntax errors before execution |

#### Example: Validating Highlighter

```rust
impl Highlighter for ValidatingHighlighter {
    fn highlight(&self, line: &str, cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        let tokens = tokenize_shell(line);

        for token in tokens {
            let style = match token.kind {
                TokenKind::Command => {
                    if command_exists(&token.text) {
                        Style::new().fg(Color::Green).bold()
                    } else {
                        Style::new().fg(Color::Red).bold()
                    }
                }
                TokenKind::Path => {
                    if path_exists(&token.text) {
                        Style::new().fg(Color::Cyan)
                    } else {
                        Style::new().fg(Color::Red).italic()
                    }
                }
                TokenKind::Flag => Style::new().fg(Color::Yellow),
                TokenKind::String => Style::new().fg(Color::Green),
                TokenKind::Pipe | TokenKind::Redirect => Style::new().fg(Color::Magenta),
                TokenKind::Variable => Style::new().fg(Color::Blue),
                _ => Style::new(),
            };
            styled.push((style, token.text));
        }

        styled
    }
}
```

---

### 5. Validator Trait

Multiline input handling.

```rust
pub trait Validator {
    fn validate(&self, line: &str) -> ValidationResult;
}

pub enum ValidationResult {
    Complete,    // Input is complete, can execute
    Incomplete,  // Need more input (unclosed quote, pipe at end, etc.)
}
```

#### Innovation Ideas for Validation

| Feature | Description |
|---------|-------------|
| **Shell syntax validation** | Detect unclosed quotes, incomplete pipes |
| **Here-doc detection** | `cat << EOF` triggers multiline mode |
| **Bracket balancing** | `{`, `(`, `[` need closing |
| **Backslash continuation** | Line ending with `\` continues |
| **Subshell detection** | `$(` needs closing `)` |

---

### 6. Menu Trait

Completion menu rendering.

```rust
pub trait Menu {
    fn is_active(&self) -> bool;
    fn menu_event(&mut self, event: MenuEvent, editor: &mut Editor);
    fn can_partially_complete(&mut self, ...) -> bool;
    fn update_values(&mut self, completer: &mut dyn Completer, line: &str, pos: usize);
    fn replace_in_buffer(&self, editor: &mut Editor);
    fn menu_required_lines(&self, terminal_cols: u16) -> u16;
    fn menu_string(&self, ...) -> String;
    fn min_rows(&self) -> u16;
    fn get_values(&self) -> &[Suggestion];
}
```

#### Built-in Menus

- `ColumnarMenu` - Multi-column completion list
- `ListMenu` - Single column with descriptions
- `IdeMenu` - IDE-style dropdown

#### Innovation Ideas for Menus

| Feature | Description |
|---------|-------------|
| **Preview menu** | Show file contents or man page excerpts |
| **Grouped completions** | Separate sections for files/commands/history |
| **Icons** | File type icons (if terminal supports) |
| **Fuzzy search in menu** | Filter completions by typing |
| **Scrollable preview** | Arrow keys scroll preview content |

---

### 7. History Trait

Command history storage and retrieval.

```rust
pub trait History {
    fn save(&mut self, entry: HistoryItem) -> Result<HistoryItemId>;
    fn load(&self, id: HistoryItemId) -> Result<HistoryItem>;
    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>>;
    fn count(&self) -> Result<u64>;
    fn update(&mut self, id: HistoryItemId, updater: impl FnOnce(&mut HistoryItem));
    // ... more methods
}

pub struct HistoryItem {
    pub id: Option<HistoryItemId>,
    pub command_line: String,
    pub start_timestamp: Option<SystemTime>,
    pub duration: Option<Duration>,
    pub exit_status: Option<i64>,
    pub cwd: Option<String>,
    pub session_id: Option<HistorySessionId>,
    pub hostname: Option<String>,
    pub more_info: Option<HashMap<String, String>>,
}
```

#### Built-in Implementations

- `FileBackedHistory` - Simple file storage
- `SqliteBackedHistory` - SQLite database

#### Innovation Ideas for History

| Feature | Description |
|---------|-------------|
| **Context-aware search** | Filter by directory, git repo, project |
| **Frecency ranking** | Combine frequency + recency for better suggestions |
| **Failed command filtering** | Option to hide/deprioritize failed commands |
| **Time-based search** | "What did I run yesterday?" |
| **Session isolation** | Option to keep sessions separate |
| **Shared history** | SQLite + WAL for tmux pane sharing |
| **History analytics** | Most used commands, patterns, time of day |
| **Sensitive command filtering** | Don't save commands with passwords/tokens |

---

### 8. Keybindings Configuration

Configure without implementing traits:

```rust
let mut keybindings = default_emacs_keybindings();

// Add custom binding
keybindings.add_binding(
    KeyModifiers::CONTROL,
    KeyCode::Char('t'),
    ReedlineEvent::ExecuteHostCommand("toggle_preview".to_string()),
);

// Remove a binding
keybindings.remove_binding(KeyModifiers::CONTROL, KeyCode::Char('r'));

// Rebind
keybindings.add_binding(
    KeyModifiers::CONTROL,
    KeyCode::Char('r'),
    ReedlineEvent::SearchHistory,
);
```

#### Available ReedlineEvents

- `Edit(EditCommand)` - Text manipulation
- `Repaint` - Redraw screen
- `PreviousHistory` / `NextHistory` - Navigate history
- `SearchHistory` - Ctrl+R search
- `Complete` - Trigger completion
- `ExecuteHostCommand(String)` - Custom command to handle
- `Menu(MenuEvent)` - Menu navigation
- And more...

---

## What Reedline Handles Internally (Cannot Customize)

- Terminal rendering mechanics (uses crossterm internally)
- Cursor positioning during editing
- Screen clearing/scrolling behavior
- Signal handling during `read_line()`
- The actual painting/drawing logic

---

## Implementation Priority for wrashpty

### Phase 1: Core Polish
1. Rich `Prompt` implementation (git, timing, exit codes)
2. Enhanced `Completer` (fuzzy matching, frecency)
3. `Highlighter` (command/path validation)

### Phase 2: Advanced Features
4. Multi-source `Hinter` (history + warnings)
5. Smart `Validator` (shell syntax)
6. Context-aware `History` search

### Phase 3: Nice-to-Have
7. Custom `Menu` with previews
8. Advanced keybindings
9. Async data fetching for prompt

---

## References

- Reedline docs: https://docs.rs/reedline/latest/reedline/
- Reedline repo: https://github.com/nushell/reedline
- Nushell (reference implementation): https://github.com/nushell/nushell
