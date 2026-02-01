# Wrashpty — Project Summary

## 1. Vision

Wrashpty is a terminal wrapper that delivers a modern interactive shell experience — autosuggestions, rich completions, history search — on top of stock Bash, without forking or patching Bash or GNU Readline.

It achieves this by sitting between the terminal emulator and a child Bash process, owning the editing phase of the shell interaction loop while being completely transparent during command execution. The user gets Fish/Zsh-level interactivity; their scripts, aliases, and muscle memory stay Bash.

---

## 2. Problem Statement

Bash is the most widely deployed interactive shell on Linux. Its scripting semantics are the de facto standard. Yet its interactive experience is decades behind Fish and Zsh: no inline suggestions, no fuzzy history search, limited completion UI, no syntax awareness during editing.

Existing solutions fall into two categories, each with significant trade-offs:

**Configuration-layer tools** (ble.sh, bash-it, Oh My Bash) operate within Bash's supported extension points. They are impressive but constrained by Readline's rendering model — every visual trick is a workaround, and performance degrades because the logic runs in Bash itself (an interpreted language optimized for scripting, not for per-keystroke computation).

**Alternative shells** (Zsh, Fish, Nushell) provide excellent interactivity but require the user to adopt a different shell language, re-learn configuration idioms, and accept that many system scripts assume Bash.

Wrashpty occupies a third position: a compiled, native-speed wrapper that replaces only the interactive editing layer while delegating all language semantics — expansion, execution, job control, scripting — to a real Bash instance.

---

## 3. Goals

### Primary Goals (MVP Core)

1. **Transparent passthrough.** Any program that runs correctly under Bash must run identically under Wrashpty. Full-screen applications (vim, htop, less), interactive REPLs (python, node), remote sessions (ssh), and job control (Ctrl+Z, fg, bg) must work without the user noticing the wrapper exists.

2. **Modern line editing.** Replace Readline's editing interface with reedline, providing Emacs and Vi keybinding modes, kill ring, undo/redo, and multi-line editing out of the box.

3. **Autosuggestions.** Inline ghost-text suggestions from command history, accepted with a single keystroke.

4. **History search.** Interactive, filterable history search bound to Ctrl+R, with prefix-matching on Up/Down arrows.

5. **Tab completion.** Filesystem paths, executable names from PATH, and git branch names presented in a navigable menu.

6. **Resilience.** Terminal state must be restored on crash, panic, or unexpected child termination. A corrupted terminal is the single worst failure mode for this class of tool.

### Secondary Goals (MVP Required, Phase-Gated)

7. **Chrome layer.** A toggleable UI frame with a customizable top bar (menu, project name) and footer (status, hints, git info). Uses terminal scroll regions to reserve screen real estate without interfering with reedline or passthrough programs.

**Chrome Layer Implementation Note**: The chrome layer is architecturally more complex and carries higher risk of terminal corruption (scroll region interaction with alternate screen programs). It is **required for complete MVP** but will only commence after Phases 0-2 achieve production stability. This phase-gating ensures core functionality is solid before adding visual chrome. If Chrome introduces critical instability during Phase 3, it becomes a post-MVP feature with the core wrapper shipping as v1.0. See Section 9 (Implementation Phases) for phase gates and validation criteria.

### Non-Goals (Explicitly Out of Scope for MVP)

- Replacing Bash as a scripting language.
- Per-keystroke syntax highlighting (requires deeper Readline integration than the wrapper model supports).
- Bash-native programmable completions (would require IPC with the child Bash for `compgen` queries).
- Multi-shell support (Zsh, Fish backend). Architecture permits this later, but MVP targets Bash only.
- Plugin system or configuration file format. Hardcoded behavior is acceptable for personal use.

### Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| **Linux** (Ubuntu 20.04+, Arch, Fedora) | **Primary** | Required for MVP. All testing and validation targets Linux. |
| **macOS** (with Homebrew Bash 4.0+) | Best-effort | May work but not blocking. PRs welcome. macOS ships Bash 3.2; user must install Bash 4.0+ via Homebrew. |
| **Windows** | Not supported | PTY semantics incompatible. WSL2 may work (untested). |

**Note**: The Bash version check at startup will provide a helpful error message if Bash 4.0+ is not available, including macOS-specific instructions for Homebrew installation.

---

## 4. Scope

### What Wrashpty Replaces

The interactive editing phase only: prompt display, keystroke handling, line composition, completion, and suggestion rendering. This phase begins when Bash is idle and waiting for input, and ends the moment the user submits a command.

### What Wrashpty Does Not Replace

Command parsing, variable expansion, globbing, pipeline construction, process spawning, job control, signal delivery to child processes, and every other aspect of shell semantics. Bash handles all of this, running on a PTY that Wrashpty manages.

### Boundary Visualization

```
┌─────────────────────────────────────────────────────┐
│  User's Terminal Emulator                           │
├─────────────────────────────────────────────────────┤
│  Wrashpty (this project)                            │
│  ┌─────────────────────────────────────────────┐    │
│  │ Chrome Layer (optional)                     │    │
│  │ ┌─────────────────────────────────────────┐ │    │
│  │ │ Top Bar: menu, project name, tabs       │ │    │
│  │ └─────────────────────────────────────────┘ │    │
│  │ ┌─────────────────────────────────────────┐ │    │
│  │ │ Content Area (scroll region)            │ │    │
│  │ │  ┌───────────┐  ┌────────────────────┐  │ │    │
│  │ │  │ Edit Mode │  │ Passthrough Mode   │  │ │    │
│  │ │  │ (reedline)│  │ (byte pump)        │  │ │    │
│  │ │  └───────────┘  └────────────────────┘  │ │    │
│  │ └─────────────────────────────────────────┘ │    │
│  │ ┌─────────────────────────────────────────┐ │    │
│  │ │ Footer: status, hints, git info         │ │    │
│  │ └─────────────────────────────────────────┘ │    │
│  └─────────────────────────────────────────────┘    │
├─────────────────────────────────────────────────────┤
│  PTY pair (kernel)                                  │
├─────────────────────────────────────────────────────┤
│  Bash --noediting (child process)                   │
│  ┌─────────────────────────────────────────────┐    │
│  │ All shell semantics: parsing, expansion,    │    │
│  │ execution, job control, builtins, scripting │    │
│  └─────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────┘
```

The chrome layer (top bar + footer) is optional and can be toggled at runtime. When disabled, the wrapper operates in "headless" mode with the full terminal available to the content area — identical to the original two-mode architecture.

---

## 5. Technical Constraints

### Constraint 1: No Bash Modifications

Wrashpty operates with an unmodified Bash binary. The only configuration injected is a generated rcfile that sets `PROMPT_COMMAND`, `PS1`, and a `DEBUG` trap to emit shell integration markers. These are standard Bash features used by many tools (VS Code integrated terminal, iTerm2, Warp).

### Constraint 2: PTY as the Only Interface

All communication with Bash occurs through the PTY file descriptor pair. There is no shared memory, no socket protocol, no custom IPC. This constraint simplifies the model but means that all state inference must come from the byte stream.

### Constraint 3: Readline Must Be Disabled

Bash is started with `--noediting` (or equivalent configuration) so that Readline does not compete with Wrashpty's editor for terminal control. This is essential: two line editors fighting over the same terminal is a guaranteed corruption source.

### Constraint 4: Marker Protocol Must Be Invisible

The OSC escape sequences used for shell integration must be silently ignored by any standard terminal emulator. They must not appear in command output, log files, or piped streams. The `777` private-use OSC namespace satisfies this.

---

## 6. Technology Selection

### Language: Rust

Chosen for: memory safety without garbage collection pauses (important for keystroke-frequency operations), excellent ecosystem for terminal and systems programming, strong type system for encoding state machine invariants, and RAII for terminal state cleanup.

### Core Dependencies

| Crate | Role | Justification |
|---|---|---|
| `portable-pty` | PTY abstraction | Cross-platform PTY creation, read/write, resize. Avoids raw `libc` PTY calls. |
| `reedline` | Line editor | Complete editor with completions, hints, history, Vi/Emacs modes. Used by Nushell in production. Eliminates the need to write a line editor from scratch. |
| `crossterm` | Terminal control | Raw mode, key events, terminal size queries. Dependency of reedline; using the same crate avoids conflicts. |
| `nix` | POSIX syscalls | Typed wrappers for `termios`, `tcsetpgrp`, `poll`, signal handling. Safer than raw `libc`. |
| `signal-hook` | Signal handling | Async-signal-safe signal delivery. Converts signals to file descriptor events compatible with `poll`. |
| `getrandom` | Session tokens | Cryptographically secure random bytes for marker session tokens. Minimal dependency (~100 lines), uses OS CSPRNG. |
| `anyhow` | Application errors | Ergonomic error handling with context for a binary crate. |
| `thiserror` | Library errors | Typed, derivable errors for the marker parser and other modules with structured failure modes. |
| `tempfile` | Temporary rcfile | Secure temporary file creation for the generated Bash configuration. |
| `tracing` | Diagnostics | Structured logging to file (never to the terminal the wrapper controls). |
| `dirs` | Path resolution | Locate `~/.bash_history`, `~/.bashrc`, and XDG directories. |
| `unicode-width` | Display width | Calculate correct display width for CJK and emoji characters in chrome bars. |

### Dependency Versioning

**Strategy**: Use Cargo.lock for reproducible builds. Specify minimum versions in Cargo.toml and commit Cargo.lock to the repository.

| Crate | Minimum Version | Notes |
|-------|-----------------|-------|
| `reedline` | 0.28 | Required for current API |
| `portable-pty` | 0.8 | Required for PTY features |
| `crossterm` | 0.27 | Must match reedline's dependency |
| `nix` | 0.27 | Required for termios API |
| `signal-hook` | 0.3 | Stable API |
| `getrandom` | 0.2 | Stable API |
| `tempfile` | 3.0 | Stable API |
| `tracing` | 0.1 | Stable API |

### Build Configuration

Release builds use thin LTO and optimization level 3. The wrapper is a latency-sensitive program — every keystroke passes through it — so binary performance matters, but full LTO's compile-time cost is not justified for a single-binary project.

---

## 7. Glossary

| Term | Definition |
|---|---|
| **PTY** | Pseudo-terminal. A kernel device pair (master + slave) that emulates a hardware terminal. The master side is held by the controlling program; the slave side is the child's stdin/stdout/stderr. |
| **Readline** | GNU Readline. The line-editing library that Bash uses for interactive input. Wrashpty replaces its role. |
| **reedline** | A Rust line-editor library created for and used by Nushell. Provides editing, completions, hints, and history. |
| **OSC** | Operating System Command. A class of terminal escape sequences (ESC ] ... BEL) used for out-of-band communication between programs and terminal emulators. |
| **Shell integration** | A protocol where a shell emits markers into its output stream to signal state transitions (prompt ready, command executing, command finished). Used by VS Code, iTerm2, Warp, and others. |
| **Passthrough mode** | The wrapper state where it acts as a transparent byte pipe between the real terminal and the PTY, performing no interpretation except marker scanning. |
| **Edit mode** | The wrapper state where it owns the terminal, runs the reedline editor, and presents completions/suggestions. |
| **Raw mode** | A terminal configuration where the kernel performs no line buffering, echo, or signal interpretation — every byte is delivered immediately to the reading program. |
| **SIGWINCH** | The Unix signal delivered to a process when its controlling terminal changes size. |
| **Job control** | The shell's ability to manage foreground and background processes, suspend them (Ctrl+Z), and resume them (fg/bg). |
| **Chrome** | The persistent UI frame surrounding the main content area — top bar and footer. Term originates from web browsers (address bar, tabs, status bar). |
| **Scroll region** | A terminal feature (DECSTBM) that confines scrolling to a subregion of the screen, allowing fixed bars above and below the scrollable area. |
| **ChromeMode** | Toggle state determining whether chrome is visible (Full) or hidden (Headless). |
| **Alternate screen** | A secondary terminal buffer that programs can switch to for full-screen UI, preserving the main buffer's contents. Used for modals and pickers. |

---

## 8. Success Criteria

The MVP is complete when:

**Core MVP (Required)**:
1. A user can start `wrashpty`, get a prompt, type commands, and see output — indistinguishable from using Bash directly, except for the enhanced editing experience.
2. `vim`, `htop`, `less`, `ssh`, `python3`, and `man` work perfectly through the wrapper.
3. `Ctrl+Z` suspends a foreground job and `fg` resumes it.
4. `Ctrl+R` opens an interactive history search.
5. Tab completion shows files, directories, and executables in a navigable menu.
6. Typing a partial command shows a ghost-text suggestion from history.
7. Killing the wrapper (Ctrl+C at an empty prompt, or `exit`) restores the terminal to its original state.
8. A panic anywhere in the code restores the terminal to its original state.
9. `cat /dev/urandom | head -c 100M > /dev/null` completes in under 2x the time of running it in raw Bash (passthrough overhead is negligible).

**Extended MVP (Required, after Phases 0-2 pass validation)**:
10. Chrome mode can be toggled at runtime without corrupting terminal state or interrupting the child process.
11. Full-screen programs (`vim`, `htop`) render correctly within the scroll region when chrome is active.

**Note**: Success criteria 10-11 are required for complete MVP but are phase-gated. If Phase 3 introduces critical instability that cannot be resolved, the core wrapper (criteria 1-9) ships as v1.0, with Chrome as a post-MVP feature.

---

## 9. Implementation Phases

The MVP should be implemented in phases, with each phase fully tested before proceeding to the next. This ordering minimizes risk and ensures a working subset is available at each checkpoint.

### Phase 0: Foundation (Critical Path)

**Goal**: Establish PTY plumbing and basic passthrough. At the end of this phase, wrashpty is a "dumb" terminal multiplexer.

**Deliverables**:
1. PTY spawn with Bash `--noediting`
2. Generated bashrc with marker functions (PRECMD, PROMPT, PREEXEC)
3. Streaming marker parser with session token validation
4. Passthrough byte pump (stdin → PTY, PTY → stdout)
5. Terminal state safety (RAII guard, panic hook, signal handlers)
6. SIGWINCH propagation to PTY
7. SIGCHLD handling and clean shutdown

**Exit Criteria**:
- `wrashpty` starts Bash, user can type commands, output appears
- Markers are detected and stripped from output
- `vim`, `htop`, `ssh` work perfectly
- Ctrl+C, Ctrl+Z work as expected
- Terminal is always restored on exit/crash

**Estimated Effort**: 1-2 weeks

### Phase 1: Edit Mode Integration

**Goal**: Integrate reedline for command editing. At the end of this phase, wrashpty provides enhanced editing.

**Deliverables**:
1. Mode state machine (Initializing → Edit ↔ Passthrough → Terminating)
2. reedline integration with custom prompt
3. Command injection with RAII echo suppression (EchoGuard)
4. Injecting mode with PTY output buffering (avoid deadlock)
5. History loading from `~/.bash_history`
6. Basic prompt with cwd and exit code

**Exit Criteria**:
- Mode transitions work correctly for all marker sequences
- Editing in reedline, command execution in Bash
- Echo suppression works reliably
- History is available in reedline

**Estimated Effort**: 1-2 weeks

### Phase 2: Interactive Features

**Goal**: Add autosuggestions, completions, and history search. At the end of this phase, wrashpty is feature-complete for core MVP.

**Deliverables**:
1. History-based autosuggestions (ghost text)
2. Filesystem path completion
3. PATH executable completion
4. Git branch completion (optional)
5. Ctrl+R interactive history search
6. Prefix-filtered Up/Down navigation

**Exit Criteria**:
- All success criteria 1-9 are met
- Performance is acceptable (no perceptible lag)
- Edge cases (large history, slow filesystem) handled gracefully

**Estimated Effort**: 1-2 weeks

### Phase 3: Chrome Layer (Required, Phase-Gated)

**Goal**: Add UI chrome to complete the MVP. This phase is **deliberately last** because chrome has the highest risk of terminal corruption.

**Phase Gate**: Phase 3 may only begin after Phases 0-2 pass all validation criteria. This is a hard gate.

**Deliverables**:
1. Top bar and footer rendering
2. Scroll region management (DECSTBM)
3. Chrome toggle at runtime
4. Git status caching with background refresh
5. Minimum terminal size handling

**Note**: CSI-based alternate screen detection is explicitly **excluded from MVP**. The scroll-region-reset-on-Passthrough approach is the only supported method. See Architecture doc for rationale.

**Exit Criteria**:
- Success criteria 10-11 are met
- No terminal corruption in any tested scenario
- Chrome degrades gracefully on small terminals
- Full-screen programs work correctly with chrome active

**Estimated Effort**: 1-2 weeks

**Stability Gate**: If Phase 3 introduces critical instability that cannot be resolved within the estimated timeframe, the core wrapper (Phases 0-2) ships as v1.0 with Chrome becoming a post-v1.0 feature Epic. This is a fallback, not an expectation.

---

## 10. Risk Mitigation Checkpoints

Before proceeding from each phase, verify:

| Checkpoint | Validation | Gate Type |
|------------|------------|-----------|
| Phase 0 → 1 | Manual test protocol: vim, htop, ssh, job control | Hard gate |
| Phase 1 → 2 | Fuzz test marker parser; test echo suppression timing | Hard gate |
| Phase 2 → 3 | Full success criteria 1-9; performance benchmarks | **Hard gate** |
| Phase 3 → Release | Extended testing with diverse terminal emulators | Release criteria |

**Phase 2 → 3 is a critical gate**: Phase 3 (Chrome) may only begin after Phases 0-2 achieve production stability. All success criteria 1-9 must pass. This protects against shipping unstable Chrome on top of unstable core.

**High-Risk Components Requiring Extra Validation**:
1. SIGWINCH coordination with reedline (test resize during editing)
2. Echo suppression RAII (test panic during injection)
3. Child exit during Edit mode (kill Bash while typing)
4. Scroll region reset on Passthrough transition (test vim, htop, tmux nesting)
