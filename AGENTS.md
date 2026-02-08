# AGENTS.md

AI coding agents working on Wrashpty should follow these guidelines.

> **For the Human Developer**: Agents include brief Rust teaching in task summaries (never in code). The codebase stays professional—learning happens in conversation.

---

## Agent Protocol

### When to Ask vs. Proceed

| Situation | Action |
|-----------|--------|
| Ambiguous requirements / multiple approaches | **Ask** |
| Changes touching >3 files / new dependency | **Ask** |
| Obvious bug fix / single-file change / tests | **Proceed** |

### Change Philosophy

1. **Minimal diffs**: Only change what's necessary
2. **Preserve patterns**: Match existing style
3. **Leave breadcrumbs**: `// TODO:` for out-of-scope issues

### Handoff Summary

Include: (1) what changed and why, (2) scope boundaries, (3) follow-up work, (4) **Rust teaching moment** (1-5 sentences)

---

## Project Overview

Wrashpty is a terminal wrapper providing modern interactive shell features (autosuggestions, completions, history search) on top of stock Bash. It sits between the terminal emulator and a child Bash process, owning only the interactive editing phase while being transparent during command execution.

**State Machine** (5 modes):
```
Initializing → Edit ↔ Passthrough → Terminating
                ↓
            Injecting → Passthrough
```

**Key constraint**: No Bash modifications. All communication through PTY file descriptors only.

---

## Build & Test Commands

```bash
cargo build                    # Debug build
cargo build --release          # Optimized build (thin LTO)
cargo test                     # Run all tests
cargo clippy -- -D warnings    # Lint (CI enforced)
cargo fmt --check              # Format check (CI enforced)
cargo doc --no-deps --open     # Generate docs
```

---

## Code Style

- **Edition 2024**, Rust 1.85+, rustfmt/clippy enforced
- **Errors**: `thiserror` for public APIs, `anyhow` internally
- **No panics**: Use `Result<T, E>`; reserve `unwrap()` for proven invariants

### Naming

| Item | Convention | Example |
|------|------------|---------|
| Types | `PascalCase` | `MarkerParser`, `PtyHandle` |
| Functions | `snake_case` | `parse_marker`, `enter_raw_mode` |
| Constants | `SCREAMING_SNAKE` | `MAX_MARKER_LEN` |

**Domain patterns**:
- PTY operations: `spawn_pty()`, `resize_pty()`, `write_to_child()`
- Terminal state: `enter_raw_mode()`, `disable_echo()` (return RAII guards)
- Parsing: `parse_*()` returns `Result`, `try_*()` returns `Option`
- State queries: `is_*()` for bools, `current_*()` for state

### Documentation

- Concise rustdoc (`///`) on public APIs focusing on *what* and *why*
- Module headers (`//!`) 1-3 lines stating purpose
- No teaching-style commentary in code—explain concepts in task summaries

---

## Architecture

### Terminal Safety (Critical)

Terminal corruption is unacceptable. Defense-in-depth:

1. **RAII Guards**: `Drop` impls for terminal state (raw mode, echo flag, scroll regions)
2. **Panic Hook**: Async-signal-safe restoration via `libc::write` (not std::io)
3. **Fallback Sequences**: Always reset scroll region (`\x1b[r`) before entering Passthrough

```rust
// Pattern: RAII guard for terminal state
struct TerminalGuard { original: Termios }
impl Drop for TerminalGuard {
    fn drop(&mut self) { /* restore original */ }
}
```

### Hot Paths

The marker parser and byte pump are latency-critical. Zero-allocation required:

- Fixed-size buffers (`[u8; 80]`), not `Vec`
- No `String`/`format!` in parsing loops
- Slices with lifetimes for zero-copy parsing

### Escape Sequences

```rust
const ESC: u8 = 0x1b;
const MARKER_PREFIX: &[u8] = b"\x1b]777;wrashpty;";
const RESET_SCROLL: &[u8] = b"\x1b[r";
```

OSC 777 format: `ESC ] 777 ; wrashpty ; <event> ; <token> ; <data> BEL`

### Signal Handling

- Use `signal-hook` crate for async-signal-safe delivery
- Convert signals to file descriptor events for `poll()` integration
- Mode-aware: reedline owns SIGWINCH in Edit mode

---

## Testing

- **Unit tests**: Same file as implementation (`#[cfg(test)]` module)
- **Property tests**: `proptest` for parser fuzzing
- **Integration tests**: `rexpect` for PTY-based end-to-end tests

### Test Naming (Required)

All test functions **must** follow the pattern `test_<fn>_<scenario>_<expected>`:

| Component | Meaning | Example fragment |
|-----------|---------|------------------|
| `<fn>` | Function or type under test | `parse_marker`, `shell_quote`, `render` |
| `<scenario>` | Input condition or setup | `empty_input`, `with_spaces`, `when_dirty` |
| `<expected>` | Observable outcome | `returns_none`, `shows_branch`, `preserves_count` |

**Good names**:
```
test_shell_quote_path_with_spaces_returns_single_quoted
test_render_when_dirty_shows_branch_and_dirty_symbol
test_undo_after_delete_restores_token_count
test_format_duration_with_minutes_formats_min_sec
test_check_dangerous_command_detects_rm_rf
```

**Bad names** (too vague—don't use):
```
test_shell_quote          # missing scenario + expected
test_undo                 # what scenario? what result?
test_format_duration      # which case?
test_new                  # what type? what state?
```

When writing or modifying tests, always apply this convention. When touching a test file, rename any non-conforming tests in the same `mod tests` block.

### What to Test

- Mode transitions and their side effects
- Marker parser with malformed/partial input
- Echo suppression during command injection
- Terminal restoration after panic/crash

---

## Security

- **Session Tokens**: 64-bit random tokens validate markers; use constant-time comparison
- **No Command Injection**: Never interpolate user input into shell commands
- **Marker Spoofing**: Invalid tokens logged and rate-limited (>100 triggers warning)
- **Buffer Limits**: Fixed 80-byte marker buffer prevents overflow
- **Sensitive Files**: Never commit `.env`, credentials, or tokens

---

## Dependencies

Core crates (already vetted):

| Crate | Purpose |
|-------|---------|
| `portable-pty` | Cross-platform PTY abstraction |
| `reedline` | Line editor (Nushell-proven) |
| `crossterm` | Terminal control, raw mode |
| `nix` | POSIX syscalls (termios, signals) |
| `signal-hook` | Async-signal-safe signal handling |
| `libc` | Raw syscalls for panic hook |
| `tracing` | Structured logging (to file, never terminal) |

### Adding New Dependencies

Before adding, verify: necessity, maintenance (<6 months active), `cargo tree` impact, `cargo audit` clean.

**Ask first**: async runtimes (`tokio`, `async-std`), serialization (`serde`), native libraries

---

## Common Pitfalls

1. **Using std::io in panic hook**: Not async-signal-safe; use `libc::write`
2. **Forgetting scroll region reset**: Always `\x1b[r` before Passthrough
3. **Blocking in signal handler**: Use signal-hook's self-pipe pattern
4. **Echo during injection**: Disable PTY ECHO flag with RAII guard
5. **Logging to terminal**: Corrupts display; always log to file via tracing

---

## Code Review

**Must block**:
- Terminal state leaks (no RAII guard)
- `unwrap()` on user/external input
- Panics in hot paths (parser, pump)
- Logging to stdout/stderr
- Unvetted dependencies

**Should fix**:
- Missing rustdoc on public items
- Inconsistent naming
- Overly complex code when simple alternative exists

**Must consider**:
- Best practices for the project domain and tooling
- Security, maintainability, and practicality

---

## Rust Teaching (For Summaries)

Include 1-5 sentences explaining Rust concepts used in the task. Focus on the "why." Main developer is familiar with Typescript, Go, Python and general programming concepts. Draw parallels for existing knowledge while adding new rust specific conventions, concepts and patterns.

### Examples

> **Rust concept**: `&[u8]` slices are borrowed views into existing data—no allocation needed. The lifetime `'a` in `fn parse<'a>(input: &'a [u8])` tells the compiler "the returned value lives as long as the input." This is how Rust achieves zero-copy parsing safely.

> **Rust concept**: `TerminalGuard` implements `Drop`, so cleanup runs automatically when it goes out of scope—even during panics. This is more reliable than try/finally because Rust enforces it at compile time.

> **Rust concept**: The `?` operator is syntactic sugar for early return on error. `file.read(&mut buf)?` propagates errors up the call stack. With `anyhow::Context`, add context: `.context("reading config")?`.

### Topics to Cover (when relevant)

| Concept | When to Explain |
|---------|-----------------|
| Ownership & borrowing | First use of `&` vs `&mut` |
| Lifetimes | Adding lifetime annotations |
| `Result`/`Option` | Error handling patterns |
| `Drop` trait | RAII guard implementation |
| Pattern matching | Complex `match` expressions |
| Traits | Implementing standard traits |
| Iterators | `.map()`, `.filter()`, combinators |
| Zero-cost abstractions | Performance-critical code |
| `unsafe` | If needed (explain why safe alternatives don't work) |

### Don't

- Explain basic syntax (`let`, `fn`, `struct`)
- Repeat the same concept across tasks
- Put teaching in code comments
- Condescend—assume intelligence, explain the "why"
