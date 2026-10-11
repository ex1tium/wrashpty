# Wrashpty

A modern interactive shell experience on top of stock Bash — without forking or patching Bash or GNU Readline.

Wrashpty sits between your terminal emulator and a child Bash process, replacing only the editing phase with a compiled, native-speed line editor while being completely transparent during command execution. Your scripts, aliases, and muscle memory stay Bash. You get Fish/Zsh-level interactivity.

---

## How It Works

Wrashpty spawns Bash on a pseudo-terminal (PTY) and operates in two modes:

**Edit mode** — Wrashpty owns the terminal. You get a rich line editor powered by [reedline](https://github.com/nushell/reedline) with autosuggestions, tab completions, and interactive history search.

**Passthrough mode** — Wrashpty becomes an invisible byte pipe. Your commands, full-screen programs (vim, htop, ssh), interactive REPLs, and job control work exactly as they do in raw Bash.

Mode switching is driven by [shell integration markers](docs/mvp/02-architecture.md#4-shell-integration-protocol) — lightweight escape sequences that Bash emits at prompt boundaries. The wrapper detects them, strips them from output, and transitions between modes automatically.

```
┌──────────────────────────────────┐
│  Terminal Emulator               │
├──────────────────────────────────┤
│  Wrashpty                        │
│  ┌────────────┐ ┌──────────────┐ │
│  │ Edit mode  │ │ Passthrough  │ │
│  │ (reedline) │ │ (byte pump)  │ │
│  └────────────┘ └──────────────┘ │
├──────────────────────────────────┤
│  PTY                             │
├──────────────────────────────────┤
│  Bash --noediting                │
└──────────────────────────────────┘
```

## Features

- **Autosuggestions** — inline ghost text from command history, accepted with Right Arrow
- **Tab completion** — files, directories, PATH executables, and git branches in a navigable menu
- **History search** — interactive Ctrl+R search and prefix-filtered Up/Down navigation
- **Emacs keybindings** — reedline's default emacs mode (vi mode is a possible follow-up)
- **Full transparency** — vim, htop, ssh, python, job control (Ctrl+Z/fg/bg) all work unmodified
- **Crash safety** — terminal state is always restored, even on panic

## Requirements

- **Rust** 1.88+ (for building)
- **Bash** 4.0+ (for `BASH_COMMAND` in DEBUG traps)
- **Linux** (primary target; macOS may work with Homebrew Bash)

## Building

```bash
git clone https://github.com/ex1tium/wrashpty.git
cd wrashpty
cargo build --release
```

The binary is at `target/release/wrashpty`.

## Usage

```bash
# Run wrashpty (starts a wrapped Bash session)
./target/release/wrashpty

# Or install it somewhere on your PATH
cp target/release/wrashpty ~/.local/bin/
wrashpty
```

Once inside, use your shell normally. The enhanced editing is automatic.

| Key | Action |
|---|---|
| Tab | Completion menu |
| Right Arrow | Accept autosuggestion |
| Ctrl+Right | Accept next word of suggestion |
| Ctrl+R | Interactive history search |
| Up / Down | Prefix-filtered history navigation |
| Ctrl+Space | Open the command panel |
| PageUp / PageDown | Fullscreen scrollback viewer (page scroll; also intercepted during command execution) |
| Shift+PageUp / Shift+PageDown | Scrollback viewer, line scroll |
| Ctrl+D | Exit (at empty prompt) |
| Ctrl+C | Clear current line |

Built-in `:` colon commands (`:help`, `:panel`, `:settings`, …) are registered in `src/app/commands.rs`; press `:help` in-app for the full list.

Command-line options: `--no-chrome` (headless mode, no status bars/panels), `--help-format {json,gnu,npm,clap}` (machine-readable help), `-h` (GNU-style help), `-V` (version).

## Project Structure

```
wrashpty/
├── src/
│   ├── main.rs          # Entry point, CLI parsing, logging setup
│   ├── lib.rs           # Library root
│   ├── app/             # Event loop, mode state machine
│   │   ├── mod.rs       # App: run loop, marker handling, host commands
│   │   ├── transitions.rs # Mode transition side effects
│   │   ├── scroll_view.rs # Fullscreen scrollback viewer
│   │   ├── drain.rs     # PTY drain during injection/shutdown
│   │   └── commands.rs  # Colon-command registry (`:help`, `:panel`, …)
│   ├── pty.rs           # PTY spawn, resize, command injection
│   ├── pump.rs          # Passthrough byte pump + marker parsing
│   ├── marker.rs        # Streaming OSC 777 marker parser
│   ├── bashrc.rs        # Generated rcfile with shell integration markers
│   ├── editor.rs        # reedline integration bridge
│   ├── prompt.rs        # Prompt renderer (exit code)
│   ├── chrome/          # Status bars, panels, glyphs, themes, segments
│   ├── intelligence/    # Command intelligence (SQLite-backed)
│   ├── scrollback/      # Scrollback capture, alt-screen detection, viewer
│   ├── ui/              # Reusable TUI widgets
│   └── …                # git, history, completion, config, signals, safety, terminal
├── docs/
│   ├── dependencies.md
│   └── mvp/             # Historical MVP design docs (superseded by README where contradicted)
├── tests/
│   ├── integration.rs       # rexpect end-to-end tests
│   ├── marker_tests.rs      # Marker parser tests
│   ├── panic_test.rs        # Terminal restoration on panic
│   └── scroll_view_integration.rs
├── clippy.toml
└── Cargo.toml
```

## Architecture

Wrashpty is a single-threaded, state-machine-driven event processor. The full architecture is documented in [`docs/mvp/`](docs/mvp/):

- [**Project Summary**](docs/mvp/01-project-summary.md) — vision, goals, scope, technology rationale
- [**Architecture**](docs/mvp/02-architecture.md) — control flow, state machine, shell integration protocol, module design, safety model, error handling, testing strategy
- [**Scenarios and Solutions**](docs/mvp/03-scenarios-and-solutions.md) — 16 interaction scenarios with detailed flows, edge cases, and failure recovery

Key design decisions:

- **Main thread owns terminal** — State machine, PTY, and terminal I/O run on a single main thread; worker threads only for background I/O (git status)
- **reedline owns Edit mode entirely** — no custom event loop integration, just call `read_line()` and let it block
- **Shell integration via OSC 777 markers with session tokens** — cryptographically random tokens prevent marker spoofing
- **Five-layer terminal safety** — RAII guards (TerminalGuard, EchoGuard), fallback reset sequences, panic hook, signal handlers, explicit crossterm cleanup
- **Zero-allocation marker parser** — fixed 80-byte buffer, streaming state machine, handles split reads, timeout checking in poll loop

## Development

```bash
# Run in debug mode
cargo run

# Run with debug logging (logs go to file, not the controlled terminal)
WRASHPTY_LOG=debug cargo run

# Each instance writes its own log: /tmp/wrashpty-<pid>.log
# Valid levels: error, warn, info (default), debug, trace

# Watch the newest log in another terminal
tail -f "$(ls -t /tmp/wrashpty-*.log | head -1)"

# Run tests
cargo test

# Run the marker parser property tests
cargo test --test marker_tests
```

**Tips:**

- Always keep a second terminal open while developing. If Wrashpty corrupts terminal state, you need a recovery path.
- Test passthrough early and often: `vim`, `htop`, `less`, `ssh`, `python3`.
- Test job control: `sleep 100`, Ctrl+Z, `fg`.
- Benchmark throughput: `time cat /dev/urandom | head -c 100M > /dev/null`.

## Edit Mode Features

Edit mode provides a rich line editing experience:

- **Command editing** — Full line editing with cursor movement, delete, backspace
- **History navigation** — Up/Down arrows navigate through command history
- **History search** — Ctrl+R for interactive reverse search through history
- **Line clearing** — Ctrl+C clears the current line without exiting
- **Exit** — Ctrl+D at empty prompt exits the shell
- **Background output** — Output from background jobs is buffered during editing and displayed before the next prompt

History is loaded from `~/.bash_history` (last 10,000 entries) at startup.

## Roadmap

- [x] Architecture and design documentation
- [x] **Phase 0: Foundation** — PTY spawn, marker parser with session tokens, passthrough byte pump, terminal safety (RAII guards with fallback), SIGWINCH/SIGCHLD handling
- [x] **Phase 1: Edit Mode** — Mode state machine, reedline integration, EchoGuard for echo suppression, command injection with deadlock prevention, history loading
- [x] **Phase 2: Features** — Autosuggestions, filesystem/PATH/git completions, Ctrl+R history search, prefix-filtered navigation
- [x] **Product layer (shipped)** — Chrome status bars and panels (settings, help, commands/schema browser, file browser, history browser), fullscreen scrollback viewer with search/filter/fold/yank, SQLite-backed command intelligence
- [ ] **Deferred** — `--shell`, `--histfile`, `--preexec-timeout`, `--vi`, `--emacs`, `-c` CLI flags (designed in the MVP spec, never implemented; see docs/mvp/02-architecture.md §12) and vi editing mode

## License

MIT
