# AGENTS.md

AI coding agents working on Wrashpty should follow these guidelines.

## Project Overview

Wrashpty is a terminal wrapper providing modern interactive shell features (autosuggestions, completions, history search) on top of stock Bash. It sits between the terminal emulator and a child Bash process, owning only the interactive editing phase while being transparent during command execution.

**Architecture**: State-machine-driven event processor with 5 modes:
- `Initializing` → `Edit` ↔ `Passthrough` → `Terminating`

**Key constraint**: No Bash modifications. All communication through PTY file descriptors only.

## Build & Test Commands

```bash
cargo build                    # Debug build
cargo build --release          # Optimized build (thin LTO)
cargo test                     # Run all tests
cargo test --all-features      # Run tests with all features
cargo clippy -- -D warnings    # Lint (must pass, CI enforced)
cargo fmt --check              # Format check (CI enforced)
cargo doc --no-deps --open     # Generate and view documentation
```

## Code Style

### Rust Conventions
- Edition 2024, minimum Rust 1.85
- Use `rustfmt` defaults (CI enforced)
- Prefer `clippy` suggestions; all warnings are errors in CI
- Use `thiserror` for library-style errors (typed, derivable)
- Use `anyhow` for application-level error propagation
- Prefer `Result<T, E>` over panics; reserve `unwrap()` for proven invariants

### Documentation Style
Write **concise, relevant** rustdoc comments:
- Document public APIs with `///` focusing on *what* and *why*
- Skip obvious getters/setters; document non-obvious behavior
- Use `# Examples` sparingly—only when usage is non-intuitive
- Avoid teaching-style commentary in code; explain concepts in chat summaries

```rust
// Good: Concise, explains the non-obvious
/// Restores terminal state using async-signal-safe syscalls.
/// Called from panic hook where std::io may be corrupted.

// Bad: Teaching style, excessive
/// This function restores the terminal. In Unix systems, terminals have
/// "modes" that control how input is processed. Raw mode disables...
```

### Module Documentation
Each module should have a `//!` header (1-3 lines) stating its purpose:
```rust
//! Streaming OSC 777 marker parser with zero-allocation hot path.
```

## Architecture Patterns

### State Machine
The `Mode` enum in `src/types.rs` drives all behavior. Transitions are explicit and documented. When adding features:
1. Identify which mode(s) the feature affects
2. Ensure transitions are handled (what happens on mode change?)
3. Consider cleanup requirements (RAII guards)

### Terminal Safety (Critical)
Terminal corruption is unacceptable. Follow defense-in-depth:

1. **RAII Guards**: Use Drop impls for terminal state (raw mode, echo flag, scroll regions)
2. **Panic Hook**: Async-signal-safe restoration via `libc::write` (no std::io)
3. **Fallback Sequences**: Always reset scroll region (`\x1b[r`) entering Passthrough

```rust
// Pattern: RAII guard for terminal state
struct TerminalGuard { original: Termios }
impl Drop for TerminalGuard {
    fn drop(&mut self) { /* restore original */ }
}
```

### Zero-Allocation Hot Paths
The marker parser and byte pump are latency-critical:
- Use fixed-size buffers, not `Vec`
- Avoid `String`/`format!` in parsing loops
- Benchmark with `cargo bench` before optimizing

### Signal Handling
- Use `signal-hook` crate for async-signal-safe delivery
- Convert signals to file descriptor events for `poll()` integration
- Mode-aware handling: reedline owns SIGWINCH in Edit mode

### Error Handling Strategy
```
┌─────────────────┬────────────────────────────────────┐
│ Layer           │ Error Type                         │
├─────────────────┼────────────────────────────────────┤
│ Public APIs     │ thiserror enums (typed, matchable) │
│ Internal/main   │ anyhow::Result (ergonomic chaining)│
│ Panic-worthy    │ Only true invariant violations     │
└─────────────────┴────────────────────────────────────┘
```

## Testing Guidelines

### Unit Tests
- Place in same file as implementation (`#[cfg(test)]` module)
- Test state transitions explicitly
- Use `proptest` for parser fuzzing (marker parser)

### Integration Tests
- Use `rexpect` for PTY-based end-to-end tests
- Test full-screen programs (vim, htop) work correctly
- Verify terminal restoration after panic/crash

### What to Test
- Mode transitions and their side effects
- Marker parser with malformed/partial input
- Echo suppression during command injection
- Signal handling (SIGWINCH resize propagation)

## Security Considerations

- **Session Tokens**: 64-bit random tokens validate markers; use constant-time comparison
- **No Command Injection**: Never interpolate user input into shell commands
- **Marker Spoofing**: Invalid tokens logged, rate-limited (>100 triggers warning)
- **Buffer Limits**: Fixed 80-byte marker buffer prevents overflow

## Project Structure

```
src/
├── main.rs      # Entry point, panic hook, logging setup
├── types.rs     # Shared types (Mode, ChromeMode, MarkerEvent)
├── app.rs       # State machine, main event loop
├── pty.rs       # PTY spawn, resize, command injection
├── terminal.rs  # Raw mode RAII guard
├── marker.rs    # OSC 777 streaming parser
├── pump.rs      # Bidirectional byte pump
├── signals.rs   # SIGWINCH/SIGCHLD handling
├── bashrc.rs    # Generated rcfile with markers
├── editor.rs    # reedline integration
├── chrome.rs    # UI bars and scroll regions
├── history.rs   # ~/.bash_history loading
├── suggest.rs   # Autosuggestion hinter
├── complete.rs  # Completion providers
└── prompt.rs    # Prompt rendering
```

## Dependencies

Core crates and their roles:
| Crate | Purpose |
|-------|---------|
| `portable-pty` | Cross-platform PTY abstraction |
| `reedline` | Line editor (Nushell-proven) |
| `crossterm` | Terminal control, raw mode |
| `nix` | POSIX syscalls (termios, signals) |
| `signal-hook` | Async-signal-safe signal handling |
| `libc` | Raw syscalls for panic hook |
| `tracing` | Structured logging (to file, never terminal) |

## Common Pitfalls

1. **Using std::io in panic hook**: Not async-signal-safe; use `libc::write`
2. **Forgetting scroll region reset**: Always `\x1b[r` before Passthrough
3. **Blocking in signal handler**: Use signal-hook's self-pipe pattern
4. **Echo during injection**: Disable PTY ECHO flag with RAII guard
5. **Logging to terminal**: Corrupts display; always log to `/tmp/wrashpty.log`

## PR Checklist

Before submitting:
- [ ] `cargo fmt` passes
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] `cargo doc` generates without warnings
- [ ] New public APIs have rustdoc comments
- [ ] Terminal safety: any terminal state changes use RAII guards
- [ ] No `unwrap()` on fallible operations (use `?` or handle error)

## Documentation Generation

Generate and serve documentation locally:
```bash
cargo doc --no-deps --document-private-items --open
```

For CI, documentation is built with:
```bash
cargo doc --no-deps --all-features
```

Documentation is generated from rustdoc comments (`///` and `//!`). Write docs that help future maintainers understand *intent*, not just API shape.
