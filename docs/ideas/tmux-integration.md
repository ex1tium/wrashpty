# tmux Integration Ideas

> **Status**: Future consideration, out of MVP scope
> **Priority**: Post-stabilization feature

## Overview

wrashpty instances running in different tmux panes can communicate and share data, enabling powerful cross-pane features without wrashpty needing to become a terminal multiplexer itself.

## How tmux Works (Context)

```text
┌─────────────────────────────────────────────────────────────┐
│                    tmux server                              │
│            (persistent background process)                  │
│                                                             │
│    ┌─────────────────┐        ┌─────────────────┐           │
│    │  Session A      │        │  Session B      │           │
│    │  ┌───────────┐  │        │  ┌───────────┐  │           │
│    │  │ Window 1  │  │        │  │ Window 1  │  │           │
│    │  │ ┌───┬───┐ │  │        │  │           │  │           │
│    │  │ │P1 │P2 │ │  │        │  │           │  │           │
│    │  │ └───┴───┘ │  │        │  └───────────┘  │           │
│    │  └───────────┘  │        └─────────────────┘           │
│    └─────────────────┘                                      │
│                                                             │
│    Each pane can run a wrashpty instance                    │
└─────────────────────────────────────────────────────────────┘
```

## Communication Channels

### 1. Environment Variables

Every pane gets these automatically:

```bash
$TMUX           # Socket path: /tmp/tmux-1000/default
$TMUX_PANE      # Pane ID: %3
```

Detection in Rust:

```rust
fn detect_tmux() -> Option<TmuxContext> {
    let socket = std::env::var("TMUX").ok()?;
    let pane_id = std::env::var("TMUX_PANE").ok()?;
    Some(TmuxContext { socket, pane_id })
}
```

### 2. tmux Control Commands

Send commands to tmux programmatically:

```rust
use std::process::Command;

fn tmux_command(args: &[&str]) -> io::Result<String> {
    let output = Command::new("tmux")
        .args(args)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// Examples:
tmux_command(&["list-panes", "-F", "#{pane_id}:#{pane_current_command}"])?;
tmux_command(&["display-message", "-p", "#{session_name}"])?;
```

### 3. tmux User Options (@variables)

tmux allows arbitrary key-value storage per pane/window/session:

```bash
# Set from wrashpty
tmux set-option -p @wrashpty_cwd "/home/user/project"
tmux set-option -p @wrashpty_last_cmd "make build"
tmux set-option -p @wrashpty_exit_code "0"

# Read from another wrashpty instance
tmux show-option -pv @wrashpty_cwd
```

### 4. Unix Socket IPC (Direct Communication)

wrashpty instances could communicate directly:

```text
~/.wrashpty/
├── sockets/
│   ├── %0.sock      # Pane %0's socket
│   ├── %1.sock      # Pane %1's socket
│   └── %3.sock      # Pane %3's socket
└── shared/
    └── history.db   # Shared history database
```

---

## Feature Ideas

### Feature 1: Shared History Across Panes

Use SQLite with WAL mode for concurrent access from multiple wrashpty instances.

```rust
impl History for SharedHistory {
    fn save(&mut self, entry: HistoryItem) -> Result<HistoryItemId> {
        // Include pane context
        let entry_with_context = entry
            .with_extra("tmux_pane", &self.pane_id)
            .with_extra("tmux_session", &self.session_name)
            .with_extra("cwd", &self.cwd);

        self.db.save(entry_with_context)
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        // Can filter by: all panes, same session, same directory
        self.db.search_with_context(query, self.context_filter)
    }
}
```

**User experience:**

```text
# Pane 1: ~/projects/foo
$ make build
$ make test

# Pane 2: ~/projects/foo (same project)
$ <up arrow>  # Shows "make test" from Pane 1!

# Pane 3: ~/projects/bar (different project)
$ <up arrow>  # Only shows commands from bar project
```

### Feature 2: Cross-Pane Command Broadcasting

```rust
fn broadcast_to_panes(command: &str) -> io::Result<()> {
    let current_pane = tmux_command(&["display-message", "-p", "#{pane_id}"])?
        .trim()
        .to_string();
    let panes = tmux_command(&["list-panes", "-F", "#{pane_id}"])?;

    for pane in panes.lines() {
        if pane != current_pane {
            tmux_command(&["send-keys", "-t", pane, command, "Enter"])?;
        }
    }
    Ok(())
}
```

**User experience:**

```bash
$ @all cd /new/directory    # All panes cd together
$ @all export DEBUG=1       # Set env in all panes
```

### Feature 3: Pane Status in tmux Status Line

tmux status line can read pane options:

```bash
# In .tmux.conf
set -g status-right '#(tmux show-option -pv @wrashpty_status)'
```

wrashpty updates this on state changes:

```rust
fn update_tmux_status(&self) -> io::Result<()> {
    let status = format!("{} {} {}",
        self.cwd.file_name(),
        self.git_branch.as_deref().unwrap_or(""),
        if self.last_exit == 0 { "✓" } else { "✗" }
    );
    tmux_command(&["set-option", "-p", "@wrashpty_status", &status])?;
    Ok(())
}
```

### Feature 4: Directory Sync

When you `cd` in one pane, others can optionally follow:

```rust
fn on_precmd(&mut self, cwd: &Path) -> io::Result<()> {
    if Some(cwd) != self.last_cwd.as_ref() {
        self.last_cwd = Some(cwd.to_owned());

        // Publish to tmux
        let Some(cwd_str) = cwd.to_str() else {
            tracing::warn!(path = %cwd.display(), "Skipping tmux cwd publish for non-UTF-8 path");
            return Ok(());
        };
        tmux_command(&["set-option", "-p", "@wrashpty_cwd", cwd_str])?;

        // Optionally notify linked panes
        if self.sync_enabled {
            self.broadcast_cwd_change(cwd);
        }
    }
    Ok(())
}
```

### Feature 5: Command Completion from Other Panes

```rust
impl Completer for CrossPaneCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let mut suggestions = vec![];

        // Get commands from other panes' recent history
        let panes = tmux_command(&["list-panes", "-F", "#{pane_id}"]).unwrap_or_default();
        for pane in panes.lines() {
            if let Ok(recent) = tmux_command(&["show-option", "-pv", "-t", pane, "@wrashpty_recent"]) {
                // Add to suggestions with "from pane X" description
            }
        }

        suggestions
    }
}
```

---

## Implementation Considerations

### Performance

- tmux commands spawn subprocesses - cache results where possible
- SQLite with WAL mode handles concurrent access well
- Consider debouncing status updates

### Fallback Behavior

- All tmux features should gracefully degrade when not in tmux
- Use `detect_tmux()` to conditionally enable features

### Configuration

```toml
# ~/.config/wrashpty/config.toml
[tmux]
enabled = true
shared_history = true
status_integration = true
directory_sync = false  # Opt-in, can be intrusive
broadcast_prefix = "@all"
```

---

## References

- tmux man page: `man tmux`
- tmux control mode: `tmux -C`
- tmux formats: `man tmux` → FORMATS section
