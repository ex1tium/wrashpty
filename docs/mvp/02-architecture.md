# Wrashpty — Architecture

## 1. Architectural Style

Wrashpty is a **state-machine-driven event processor**. The entire runtime is organized around a top-level finite state machine with five states (Initializing, Edit, Injecting, Passthrough, Terminating), where transitions are triggered by protocol markers embedded in the PTY byte stream.

This is not a client-server architecture, not a pipeline, and not an actor system. The core uses a **hybrid threading model**: a single main thread owns all terminal I/O, PTY operations, and state machine transitions, while detached worker threads handle optional background I/O (git status queries, completion scanning). Worker threads never touch the terminal, PTY, or mode state.

### Hybrid Threading Model

| Component | Thread | Rationale |
|-----------|--------|-----------|
| State machine | Main | Single point of control prevents race conditions |
| Terminal I/O | Main | Raw mode and escape sequences require serialization |
| PTY read/write | Main | Must coordinate with mode transitions |
| Signal handling | Main (via pipe) | Async-signal-safe delivery to main loop |
| Git status | Worker (optional) | Avoids blocking prompt on slow git repos |
| Completion scan | Worker (optional) | Avoids blocking on slow filesystems |

Worker threads communicate results via channels; the main thread polls these during safe points (between mode transitions). This provides the simplicity benefits of single-threaded design while allowing background I/O for optional features.

---

## 2. Top-Level Control Flow

The main loop is a two-phase alternation:

```
fn run() -> Result<ExitCode>:
    spawn child Bash on PTY
    
    loop:
        PASSTHROUGH PHASE:
            poll(stdin, pty_master)
            forward stdin → pty_master
            scan pty_master output for markers → stdout
            on PROMPT marker → break to EDIT PHASE
            on child exit → return exit code
        
        EDIT PHASE:
            reedline.read_line(prompt)
            on Success(line) → inject command to pty_master
            on CtrlD → inject "exit"
            on CtrlC → clear, re-enter edit
            → continue to PASSTHROUGH PHASE
```

There is no event bus, no message queue, no channel. The two phases share state through the `App` struct, and control flow is a plain Rust `loop` with `match` on mode. This is the simplest correct architecture for a program that must alternate between "I own the terminal" and "someone else owns the terminal."

### Why Not Async

Async Rust (tokio, async-std) adds: a runtime, pinning semantics, cancellation concerns, and the colored-function problem (async infects call signatures). None of this buys anything here. The program waits on exactly two file descriptors and a signal pipe. `nix::poll` handles this in three lines.

### Why Not Threads

A thread-per-concern model (one for stdin, one for PTY reads, one for UI) would work but introduces synchronization requirements for the mode state machine and terminal state. Since the two modes are mutually exclusive (only one side "owns" the terminal at a time), parallelism provides no throughput benefit and adds correctness risk.

### Why Not an Actor Model

The program has exactly one actor (itself) reacting to exactly two input sources (stdin and PTY) and one signal channel. Actor frameworks add abstraction without reducing complexity at this scale.

---

## 3. State Machine Design

### Mode Enum

```rust
pub enum Mode {
    /// Waiting for initial PROMPT marker after spawn
    Initializing,
    /// Transparent byte pump between terminal and PTY
    Passthrough,
    /// reedline owns the terminal, user is composing a command
    Edit { last_exit_code: i32 },
    /// Command submitted, waiting for PREEXEC confirmation
    Injecting { pending_line: String },
    /// Exit initiated, waiting for child to terminate
    Terminating { timeout_deadline: Instant },
}
```

**Why these states matter:**

- **Initializing**: Prevents undefined behavior if the user types before Bash is ready. Input is buffered or ignored until the first PROMPT arrives.
- **Injecting**: Provides a window to handle edge cases like Ctrl+C during command injection. The `pending_line` is kept for echo suppression matching.
- **Terminating**: Allows graceful shutdown with timeout. If child doesn't exit within 2 seconds, SIGTERM is sent; after another 2 seconds, SIGKILL.

The `last_exit_code` is carried into Edit mode because the prompt renderer needs it (to show success/failure indicators). It is extracted from the `PRECMD` marker that precedes every `PROMPT` marker.

### Chrome Mode

```rust
pub enum ChromeMode {
    Full,       // top bar + footer visible, scroll region active
    Headless,   // original behavior, full terminal
}
```

Chrome mode is orthogonal to the Edit/Passthrough mode — it affects viewport sizing and bar rendering but not control flow. The mode state machine, marker protocol, and byte pump are completely unaware of chrome.

When chrome is active, the terminal uses a scroll region (DECSTBM) to confine scrolling to the content area between the top bar and footer. The PTY reports a smaller window size (`rows - 2`) to child processes, and reedline renders within the same reduced viewport.

### Transition Rules

| From | Event | To | Side Effects |
|---|---|---|---|
| Initializing | `PROMPT` marker | Edit | First prompt ready, activate reedline |
| Initializing | Timeout (10s) | Passthrough | Log warning, fall back to passthrough-only mode |
| Passthrough | `PRECMD` marker | Passthrough (update exit code) | Store exit code, record command duration |
| Passthrough | `PROMPT` marker | Edit | Stop byte pump, enter raw mode, activate reedline |
| Edit | `reedline::Signal::Success(line)` | Injecting | Write line to PTY master, start echo suppression |
| Injecting | `PREEXEC` marker | Passthrough | Confirm injection, record command start time |
| Injecting | Timeout (configurable, default 500ms, max 3s) | Passthrough | Late-check PTY, then log warning and proceed without PREEXEC confirmation |
| Injecting | Ctrl+C from user | Edit | Cancel pending injection, return to edit |
| Passthrough | `PREEXEC` marker | Passthrough | Record command start time |
| Passthrough | Child process exits | Terminating | Begin shutdown sequence |
| Edit | `reedline::Signal::CtrlD` | Injecting | Inject "exit" command |
| Terminating | Child exited | (exit) | Restore terminal, return exit code |
| Terminating | Timeout (2s) | Terminating | Send SIGTERM to child |
| Terminating | Timeout (4s) | (exit) | Send SIGKILL, force exit |

### Illegal Transitions

The type system prevents some illegal states:

- **PROMPT in Edit mode**: Would mean Bash emitted a prompt without being asked. Log warning, re-enter Edit mode with same state.
- **PREEXEC in Edit mode**: Would mean DEBUG trap fired without command injection. Log and ignore.
- **PROMPT in Injecting mode**: Would mean Bash skipped execution. Log warning, transition to Edit mode.
- **Multiple PREEXEC markers**: The DEBUG trap guard should prevent this, but if it happens, only the first is honored.

---

## 4. Shell Integration Protocol

### Marker Design Rationale

The markers use OSC (Operating System Command) escape sequences in the private-use range (code 777). This range is designated for application-specific use by terminal emulators; unrecognized OSC codes are silently discarded by conforming terminals.

The marker format includes a session token for authentication:

```
ESC ] 777 ; <session_token> ; <type> [ ; <payload> ] BEL
```

Where:
- `<session_token>`: 16 hex characters (64 bits of entropy), unique per wrapper invocation
- `<type>`: One of `PRECMD`, `PROMPT`, `PREEXEC`
- `<payload>`: Optional, type-specific data (e.g., exit code for PRECMD)

**Example markers:**
```
\x1b]777;a1b2c3d4e5f67890;PROMPT\x07
\x1b]777;a1b2c3d4e5f67890;PRECMD;0\x07
\x1b]777;a1b2c3d4e5f67890;PREEXEC\x07
```

Three marker types:

| Marker | Emitted By | Meaning | Payload |
|---|---|---|---|
| `PRECMD` | `PROMPT_COMMAND` function | A command has finished executing | Exit code (integer) |
| `PROMPT` | `PS1` expansion | Bash is idle, waiting for input | None |
| `PREEXEC` | `DEBUG` trap | Bash is about to execute a command | None |

### Ordering Guarantee

Bash guarantees the following ordering for each command cycle:

```
PRECMD → PROMPT → [user input] → PREEXEC → [command runs] → PRECMD → PROMPT → ...
```

The first cycle after startup omits the initial PREEXEC (there is no previous command). The wrapper must handle this by starting in Passthrough mode and waiting for the first PROMPT.

### Generated Bashrc

The rcfile is generated as a temporary file at wrapper startup and passed to Bash via `--rcfile`. It has a specific structure:

1. **Define marker functions** (before user config, so they exist regardless).
2. **Source user's ~/.bashrc** (so aliases, PATH, functions, etc. are available).
3. **Override PROMPT_COMMAND and PS1** (after user config, to ensure markers survive).
4. **Install DEBUG trap with guard** (skip firing for the PROMPT_COMMAND function itself).

The override-after-source pattern is critical because many `.bashrc` files set their own `PROMPT_COMMAND`. Wrashpty's markers must be the final value.

### DEBUG Trap Guard

The Bash `DEBUG` trap fires before every simple command, including the body of `PROMPT_COMMAND`. Without a guard, the `PREEXEC` marker would fire spuriously when Bash runs the precmd function. The guard checks `$BASH_COMMAND` against the known precmd function name and suppresses the marker.

Additionally, pipelines (`a | b | c`) can cause the DEBUG trap to fire multiple times. The guard uses a flag variable to ensure PREEXEC is emitted at most once per command cycle.

**Session Token Integration**: The generated bashrc includes a session token variable that's embedded in all markers:

```bash
# Session token is generated at wrapper startup and embedded here
__wrash_token="a1b2c3d4e5f67890"  # 16 hex chars (8 bytes)

__wrash_preexec_fired=0
__wrash_preexec_trap() {
    [ "$BASH_COMMAND" = "__wrash_precmd" ] && return
    [ "$__wrash_preexec_fired" = "1" ] && return
    __wrash_preexec_fired=1
    printf '\e]777;%s;PREEXEC\a' "$__wrash_token"
}
__wrash_precmd() {
    local ec=$?
    __wrash_preexec_fired=0
    printf '\e]777;%s;PRECMD;%d\a' "$__wrash_token" "$ec"
}
# PS1 includes the PROMPT marker
PS1='\[\e]777;'"$__wrash_token"';PROMPT\a\]'"$PS1"
```

**Marker format with token**: `ESC ] 777 ; <16-hex-token> ; <type> [ ; <payload> ] BEL`

This ensures:
1. Only markers from this wrapper session are accepted
2. Subshells or nested Bash instances with stale rcfiles are ignored
3. Malicious programs cannot forge valid markers without the token

---

## 5. Marker Parser Design

The marker parser is the most correctness-critical piece of custom logic in the project. It must:

1. **Never lose bytes.** Every byte from the PTY must either be forwarded to stdout or recognized as part of a marker. Dropping bytes corrupts terminal output.
2. **Handle split reads.** A marker may span two or more `read()` calls. The parser must buffer partial sequences.
3. **Maintain throughput.** During `cat largefile`, the parser processes every byte. It must not allocate per-byte or per-read.
4. **Fail safe.** If a sequence looks like it might be a marker but turns out not to be (e.g., a program outputs `ESC ]` followed by non-marker text), the buffered bytes must be flushed as normal output.

### Parser State Machine

The parser has three states:

**Normal**: Pass bytes through. On seeing `ESC` (0x1B), transition to `EscSeen`.

**EscSeen**: If the next byte is `]` (0x5D), transition to `OscBody`. Otherwise, emit the buffered `ESC` and the current byte, return to `Normal`.

**OscBody**: Accumulate bytes until either `BEL` (0x07) or `ST` (ESC + `\`). On terminator, check if the accumulated body matches a known marker pattern. If yes, emit a `MarkerEvent`. If no, emit the entire buffered sequence (ESC, ], body, terminator) as normal output. If the buffer exceeds a safety limit (80 bytes), flush and return to Normal — this prevents unbounded buffering from a malformed sequence.

### Zero-Allocation Design with Security Hardening

The parser uses a fixed-size internal buffer and returns slices into either the input buffer or the internal buffer. No heap allocation occurs during parsing.

#### Buffer Size Rationale

The buffer size is carefully calculated based on maximum legitimate marker length:

```
Maximum marker: ESC ] 777 ; <16-hex-token> ; PRECMD ; <exit-code> BEL
                2   + 4   + 16             + 7      + 12          + 1 = 42 bytes

With safety margin: 80 bytes
```

The 80-byte buffer accommodates:
- OSC prefix: `ESC ]` (2 bytes)
- Code: `777;` (4 bytes)
- Session token: 16 hex characters + `;` (17 bytes)
- Marker type: `PREEXEC` (7 bytes max) + `;` (1 byte)
- Payload: exit code with negative values `-2147483648` (12 bytes max)
- Terminator: `BEL` (1 byte)
- Safety margin for future extensions

#### Timeout Design: External to Parser

**Critical Design Decision**: The parser does NOT track time internally. Calling `Instant::now()` on every `feed()` invocation would add syscall overhead in the hot passthrough path. Instead, timeout checking is delegated to the main poll loop.

```rust
/// Buffer size: 80 bytes accommodates max marker with safety margin
const MAX_MARKER_LEN: usize = 80;

/// Timeout checked externally by poll loop, not per-feed()
const STALE_SEQUENCE_TIMEOUT_MS: u64 = 100;

pub struct MarkerParser {
    buf: [u8; MAX_MARKER_LEN],
    buf_len: usize,
    state: ParserState,
    session_token_hex: [u8; 16],  // 8 bytes → 16 hex chars, stored as ASCII bytes
    security_event_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParserState {
    Normal,
    EscSeen,
    OscBody,
}
```

**Security measures:**

1. **Maximum length validation**: If the buffer fills before seeing `BEL`, flush as passthrough bytes and reset. This prevents unbounded memory growth from malformed input.

2. **External timeout for stale sequences**: The main poll loop tracks time between poll iterations. If the parser is mid-sequence (`is_mid_sequence()` returns true) and 100ms has elapsed since the last poll, the loop calls `flush_stale()` to reset the parser. This avoids per-call overhead.

3. **Session token validation**: After `BEL`, validate the marker structure and session token before emitting `MarkerEvent`. Invalid tokens are treated as passthrough bytes.

4. **Fixed-size token storage**: Session token is stored as `[u8; 16]` rather than `String` to avoid heap allocation in the parser.

```rust
pub enum ParseOutput<'a> {
    Bytes(&'a [u8]),
    Marker(MarkerEvent),
}

impl MarkerParser {
    pub fn new(session_token: [u8; 16]) -> Self {
        Self {
            buf: [0u8; MAX_MARKER_LEN],
            buf_len: 0,
            state: ParserState::Normal,
            session_token,
            security_event_count: 0,
        }
    }

    /// Returns true if parser is accumulating a potential marker sequence.
    /// Used by poll loop to determine if timeout check is needed.
    #[inline]
    pub fn is_mid_sequence(&self) -> bool {
        !matches!(self.state, ParserState::Normal)
    }

    /// Flush any accumulated bytes and reset to Normal state.
    /// Called by poll loop when stale sequence timeout expires.
    pub fn flush_stale(&mut self) -> Option<&[u8]> {
        if self.buf_len > 0 {
            let bytes = &self.buf[..self.buf_len];
            self.buf_len = 0;
            self.state = ParserState::Normal;
            Some(bytes)
        } else {
            self.state = ParserState::Normal;
            None
        }
    }

    /// Feed input bytes and iterate over output.
    /// No timeout checking here — that's done externally.
    pub fn feed<'a>(&'a mut self, input: &'a [u8]) -> impl Iterator<Item = ParseOutput<'a>> + 'a {
        MarkerIterator {
            parser: self,
            input,
            pos: 0,
        }
    }

    fn validate_marker(&mut self, body: &[u8]) -> Option<MarkerEvent> {
        // Expected format: "777;<token>;<type>[;<payload>]"
        let body_str = std::str::from_utf8(body).ok()?;
        let parts: Vec<&str> = body_str.splitn(4, ';').collect();

        if parts.len() < 3 || parts[0] != "777" {
            return None;
        }

        // Validate session token (constant-time comparison to prevent timing attacks)
        let received_token = parts[1].as_bytes();
        if received_token.len() != 16 || !constant_time_eq(received_token, &self.session_token) {
            self.security_event_count += 1;
            // Log security event (to file, not terminal — could be attack)
            // tracing::warn!(target: "security", "Invalid marker token received");
            if self.security_event_count > 100 {
                // Too many invalid tokens — emit security event for app to handle
                // tracing::error!(target: "security", "Possible marker spoofing attack: {} invalid tokens", self.security_event_count);
            }
            return None;  // Treat as passthrough, not a valid marker
        }

        // Parse marker type
        match parts[2] {
            "PRECMD" => {
                let exit_code = parts.get(3)
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(1);  // Default to 1 on parse failure
                Some(MarkerEvent::Precmd { exit_code })
            }
            "PROMPT" => Some(MarkerEvent::Prompt),
            "PREEXEC" => Some(MarkerEvent::Preexec),
            _ => None,
        }
    }
}

/// Constant-time byte comparison to prevent timing attacks on token validation
#[inline]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}
```

#### Poll Loop Timeout Integration

The main poll loop handles stale sequence detection:

```rust
impl App {
    fn run_passthrough_mode(&mut self) -> Result<PassthroughResult> {
        let mut last_poll_time = Instant::now();

        loop {
            // Check for stale parser state before each poll
            let elapsed = last_poll_time.elapsed();
            if self.marker_parser.is_mid_sequence()
               && elapsed > Duration::from_millis(STALE_SEQUENCE_TIMEOUT_MS)
            {
                // Flush stale bytes as passthrough output
                if let Some(stale_bytes) = self.marker_parser.flush_stale() {
                    write_all(STDOUT_FILENO, stale_bytes)?;
                }
            }

            // Poll with timeout
            let poll_timeout_ms = if self.marker_parser.is_mid_sequence() {
                // Shorter poll timeout when accumulating potential marker
                50
            } else {
                -1  // Block indefinitely when not mid-sequence
            };

            let ready = poll(&mut self.pollfds, poll_timeout_ms)?;
            last_poll_time = Instant::now();

            // ... rest of poll handling
        }
    }
}
```

This design keeps the hot path (normal byte passthrough) free of timing overhead while still detecting stale sequences.

The caller (the byte pump) writes `Bytes` variants directly to stdout and processes `Marker` variants as state transitions.

---

## 6. PTY Management

### Bash Version Validation

**Requirement**: Bash 4.0 or later is required for DEBUG trap functionality (`BASH_COMMAND` variable).

**Startup validation** (fail-fast):

```rust
fn validate_bash_version() -> Result<()> {
    let output = Command::new("bash")
        .args(["--version"])
        .output()
        .context("Failed to execute bash --version")?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    // Parse version from "GNU bash, version X.Y.Z..."
    let version = parse_bash_version(&version_str)?;

    if version < (4, 0) {
        anyhow::bail!(
            "Wrashpty requires Bash 4.0+, found {}.{}. \
             On macOS, install Bash via Homebrew: brew install bash",
            version.0, version.1
        );
    }
    Ok(())
}
```

**Policy**: Fail fast at startup with a clear error message. Do not attempt to run with older Bash versions, as DEBUG trap behavior differs and markers may not emit correctly.

### Spawning

The child Bash is spawned with:

- A new session (`setsid`) so it becomes the session leader and can manage its own process groups for job control.
- The PTY slave as its controlling terminal.
- `--noediting` to disable Readline.
- `--rcfile <tempfile>` to load the generated configuration.

The wrapper retains the PTY master file descriptor for reading child output and writing user input (or injected commands).

### Resize Propagation

#### SIGWINCH Race Condition with reedline

A naive approach where the wrapper handles `SIGWINCH` unconditionally has a race condition:

1. User resizes terminal while reedline is active in Edit mode
2. SIGWINCH arrives → wrapper's signal handler writes to pipe
3. Wrapper's poll loop reads signal event
4. Wrapper calls `pty.resize(new_cols, new_rows)`
5. **Race**: reedline is still blocking in `read_line()` with old terminal size
6. reedline's internal buffer and cursor calculations are now wrong
7. Result: Corrupted prompt rendering, cursor in wrong position

This occurs because reedline has its own internal SIGWINCH handler (via crossterm). When two handlers manage the same terminal, they conflict.

#### Mode-Aware SIGWINCH Handling

The solution is conditional signal handling based on current mode:

```rust
fn handle_sigwinch(&mut self) -> Result<()> {
    match self.mode {
        Mode::Edit => {
            // Let reedline handle SIGWINCH internally via crossterm.
            // Do NOT intercept or resize PTY here.
            // reedline will query the new size and redraw on its own.
            //
            // IMPORTANT: When transitioning back to Edit mode, we recalculate
            // effective_rows and resize PTY to (cols, rows-2) if chrome is active.
            // See transition_to_edit().
        }
        Mode::Passthrough | Mode::Injecting | Mode::Initializing => {
            // We own SIGWINCH in these modes
            let (cols, rows) = TerminalGuard::get_size()?;

            // CRITICAL: In Passthrough/Injecting, PTY always gets FULL rows.
            // Scroll regions are disabled in these modes, so the child needs
            // the full terminal. Chrome bars are not visible during Passthrough.
            // Only Edit mode uses rows-2 when chrome is active.
            self.pty.resize(cols, rows)?;

            // Note: We do NOT redraw chrome bars here because:
            // 1. Scroll regions are reset in Passthrough (bars not visible)
            // 2. Bars will be redrawn on transition back to Edit mode
        }
        Mode::Terminating { .. } => {
            // Ignore resize during shutdown
        }
    }
    Ok(())
}
```

#### PTY Resize After reedline Returns

After reedline returns (command submitted), the wrapper must synchronize the PTY size:

```rust
fn transition_to_injecting(&mut self, line: String) -> Result<()> {
    // Query current terminal size (reedline may have handled a resize)
    let (cols, rows) = TerminalGuard::get_size()?;

    // CRITICAL: Use FULL rows for Injecting/Passthrough.
    // Scroll regions are reset when entering Passthrough, so the child
    // needs the full terminal height. Chrome bars are not visible during
    // command execution; they're redrawn on transition back to Edit mode.
    self.pty.resize(cols, rows)?;

    // Now inject the command
    self.injector.inject(&line)?;
    self.mode = Mode::Injecting { pending_line: line };
    Ok(())
}
```

This ensures the PTY gets the full terminal size before entering Passthrough. Chrome bars are not visible during Passthrough (scroll regions are reset), so the child process should have access to the full terminal.

#### PTY Resize on Edit Mode Transition

When transitioning back to Edit mode (after command completion), the wrapper must resize the PTY to account for chrome if active:

```rust
fn transition_to_edit(&mut self) -> Result<()> {
    let (cols, rows) = TerminalGuard::get_size()?;

    if self.chrome.is_active() {
        // Chrome bars will be visible: resize PTY to rows-2
        let effective_rows = rows.saturating_sub(2);
        self.pty.resize(cols, effective_rows)?;

        // Re-establish scroll region and redraw bars
        self.chrome.enter_edit_mode(rows)?;
    }
    // If chrome is not active, PTY is already at full size from Passthrough

    // reedline will query terminal size on next read_line() call
    self.mode = Mode::Edit;
    Ok(())
}
```

This design ensures:
- **Passthrough/Injecting**: PTY always gets full rows (child has full terminal)
- **Edit mode with chrome**: PTY gets rows-2 (bars reserve 2 rows)
- **Edit mode without chrome**: PTY keeps full rows (no resize needed)

### Command Injection

When the user submits a line in Edit mode, the wrapper writes `line + "\n"` to the PTY master. Bash reads this from its stdin (the PTY slave) and executes it.

**Echo suppression**: Because the PTY normally has `ECHO` enabled (required for interactive programs in passthrough), the injected command would be echoed back. The wrapper must suppress this echo to avoid displaying the command twice.

#### Echo Suppression Race Condition

A naive byte-matching approach has a fundamental race condition:

1. Wrapper injects `ls -la\n` to PTY master
2. Bash's DEBUG trap fires and emits `PREEXEC` marker
3. Wrapper transitions to Passthrough mode
4. **Race**: The kernel's TTY line discipline echoes `ls -la\n` to PTY master
5. If the echo arrives before suppression starts, it appears in stdout

The race occurs because the `PREEXEC` marker is emitted by Bash's DEBUG trap, which fires *before* the command executes but *not necessarily before* the PTY's line discipline echoes the input. The echo is handled by the kernel's TTY layer, not by Bash.

#### Hybrid Echo Prevention with RAII Safety

The correct solution temporarily disables the PTY's `ECHO` flag during injection. **Critical**: This must use RAII to guarantee restoration even if the wrapper panics or encounters errors during injection.

```rust
use nix::sys::termios::{tcgetattr, tcsetattr, SetArg, LocalFlags, Termios};
use std::os::unix::io::RawFd;

/// RAII guard for PTY echo suppression.
/// Restores ECHO on drop, even if the wrapper panics between injection and PREEXEC.
pub struct EchoGuard {
    pty_slave_fd: RawFd,
    original: Termios,
    restored: bool,
}

impl EchoGuard {
    /// Create a new EchoGuard, immediately disabling ECHO on the PTY.
    /// Returns Err if termios operations fail.
    pub fn new(pty_slave_fd: RawFd) -> Result<Self> {
        let original = tcgetattr(pty_slave_fd)?;
        let mut no_echo = original.clone();
        no_echo.local_flags.remove(LocalFlags::ECHO);
        no_echo.local_flags.remove(LocalFlags::ECHONL);  // Also suppress newline echo
        tcsetattr(pty_slave_fd, SetArg::TCSANOW, &no_echo)?;

        Ok(Self {
            pty_slave_fd,
            original,
            restored: false,
        })
    }

    /// Explicitly restore ECHO. Call this when PREEXEC is received.
    /// Returns Ok(()) even if already restored (idempotent).
    pub fn restore(&mut self) -> Result<()> {
        if !self.restored {
            tcsetattr(self.pty_slave_fd, SetArg::TCSANOW, &self.original)?;
            self.restored = true;
        }
        Ok(())
    }

    /// Check if ECHO has been restored.
    pub fn is_restored(&self) -> bool {
        self.restored
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        if !self.restored {
            // Best-effort restoration on drop; ignore errors since we're in cleanup
            let _ = tcsetattr(self.pty_slave_fd, SetArg::TCSANOW, &self.original);
        }
    }
}

/// Command injector using EchoGuard for safe echo suppression.
pub struct CommandInjector {
    pty_master_fd: RawFd,
    pty_slave_fd: RawFd,  // Kept open (dup'd) for termios manipulation
    active_guard: Option<EchoGuard>,
}

impl CommandInjector {
    pub fn new(pty_master_fd: RawFd, pty_slave_fd: RawFd) -> Self {
        Self {
            pty_master_fd,
            pty_slave_fd,
            active_guard: None,
        }
    }

    /// Inject a command with echo suppression.
    /// The returned EchoGuard MUST be held until PREEXEC is received or timeout occurs.
    pub fn inject(&mut self, line: &str) -> Result<()> {
        // Create RAII guard that disables ECHO
        let guard = EchoGuard::new(self.pty_slave_fd)?;
        self.active_guard = Some(guard);

        // Write the command to PTY master
        let command = format!("{}\n", line);
        nix::unistd::write(self.pty_master_fd, command.as_bytes())?;

        Ok(())
    }

    /// Restore echo after PREEXEC received or timeout.
    /// Safe to call multiple times (idempotent).
    pub fn restore_echo(&mut self) -> Result<()> {
        if let Some(ref mut guard) = self.active_guard {
            guard.restore()?;
        }
        self.active_guard = None;
        Ok(())
    }

    /// Check if echo suppression is currently active.
    pub fn is_echo_suppressed(&self) -> bool {
        self.active_guard.as_ref().map(|g| !g.is_restored()).unwrap_or(false)
    }
}
```

**Why this RAII approach is essential:**

1. **Panic safety**: If the wrapper panics between `inject()` and `restore_echo()`, the `Drop` impl ensures ECHO is restored. Without this, a panic would leave the terminal with ECHO disabled permanently.

2. **Error recovery**: If any operation fails during Injecting mode, the guard's destructor runs during unwinding, restoring ECHO.

3. **Explicit lifecycle**: The guard makes the echo suppression window explicit in the code. The caller must hold the guard until ready to restore.

4. **Idempotent restoration**: Calling `restore()` multiple times is safe, which simplifies error handling paths.

**Why this is safe for normal operation:**

- No user input can arrive during injection (we're transitioning from Edit mode)
- The ECHO flag only affects the line discipline, not program behavior
- Interactive programs in Passthrough mode get ECHO restored before they run
- Bash sees the command normally; only the kernel echo is suppressed

#### Timing Sequence

```
1. Edit mode: User presses Enter
2. Wrapper: EchoGuard::new() → ECHO off, guard created
3. Wrapper: write("ls -la\n") to PTY master
4. Bash: DEBUG trap fires → emits PREEXEC marker
5. Wrapper: Detects PREEXEC → guard.restore() → ECHO on
6. Wrapper: Drop guard, transition to Passthrough
7. Bash: Executes ls command
```

**Failure scenarios and recovery:**

| Scenario | Recovery |
|----------|----------|
| PREEXEC not received within 500ms | Timeout triggers `guard.restore()`, proceed in degraded mode |
| Wrapper panics during Injecting | `Drop` impl restores ECHO automatically |
| `tcsetattr` fails during restore | Log error, but ECHO is already partially restored by child's termios inheritance |
| Child exits during Injecting | Guard drops during cleanup, ECHO restored |

**Note**: The PTY slave fd must be kept open by the wrapper (not just passed to the child) to allow termios manipulation. This is done by `dup()`ing the slave before passing it to Bash. The duplicated fd is stored in `CommandInjector` and closed only when the wrapper exits.

### Injecting Mode: Avoiding Deadlock

#### The Deadlock Problem

Injecting mode has a potential deadlock when the injected command produces output before emitting the `PREEXEC` marker:

1. Wrapper injects command: `cat /dev/urandom | head -c 1M`
2. Bash starts executing immediately
3. Output floods the PTY master buffer (typically 4KB on Linux)
4. Buffer fills before `PREEXEC` marker is emitted
5. Bash blocks on write to PTY slave
6. **Deadlock**: Bash can't emit `PREEXEC` because buffer is full, and wrapper is waiting for `PREEXEC` before reading PTY output

This is a classic producer-consumer deadlock.

#### Solution: Read PTY During Injection

The wrapper must not wait idle for `PREEXEC`. Instead, it actively reads and buffers PTY output during Injecting mode:

```rust
impl App {
    fn run_injecting_mode(&mut self) -> Result<InjectionResult> {
        let mut output_buffer = Vec::with_capacity(8192);
        // Configurable timeout with 3000ms hard cap (prevents indefinite hangs)
        let timeout_ms = self.config.preexec_timeout_ms.min(3000);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        loop {
            // Poll stdin + pty_master + signal_fd
            let ready = poll(&mut self.pollfds, 50)?;

            // Handle signals including SIGWINCH
            if self.pollfds[2].revents().contains(PollFlags::POLLIN) {
                match self.signals.read_pending() {
                    Some(SignalEvent::ChildExited) => {
                        return Ok(InjectionResult::ChildExited);
                    }
                    Some(SignalEvent::Resize) => {
                        // SIGWINCH during Injecting: propagate to PTY immediately
                        // The child hasn't started rendering yet, so resize is safe
                        let (cols, rows) = TerminalGuard::get_size()?;
                        // Use FULL rows: Injecting leads to Passthrough where
                        // scroll regions are reset and chrome is not visible
                        self.pty.resize(cols, rows)?;
                        // Note: Chrome bars are not redrawn during Injecting
                        // They will be redrawn on transition back to Edit mode
                    }
                    Some(SignalEvent::Terminate) | Some(SignalEvent::Hangup) => {
                        return Ok(InjectionResult::Shutdown);
                    }
                    None => {}
                }
            }

            // Buffer user input (don't forward yet)
            if self.pollfds[0].revents().contains(PollFlags::POLLIN) {
                let n = read(STDIN_FILENO, &mut self.stdin_buf)?;
                self.pending_user_input.extend_from_slice(&self.stdin_buf[..n]);
            }

            // Read PTY output and scan for PREEXEC
            if self.pollfds[1].revents().contains(PollFlags::POLLIN) {
                let n = read(self.pty_fd, &mut self.pty_buf)?;
                let chunk = &self.pty_buf[..n];

                // Feed through marker parser
                for output in self.marker_parser.feed(chunk) {
                    match output {
                        ParseOutput::Bytes(bytes) => {
                            output_buffer.extend_from_slice(bytes);
                        }
                        ParseOutput::Marker(MarkerEvent::Preexec) => {
                            // Success: flush buffered output and transition
                            self.flush_output_buffer(&output_buffer)?;
                            self.restore_echo()?;
                            return Ok(InjectionResult::PreexecReceived);
                        }
                        ParseOutput::Marker(_) => {
                            // Unexpected marker during injection - log and continue
                        }
                    }
                }
            }

            // Timeout check
            if Instant::now() > deadline {
                // Late-check: One final non-blocking PTY read before restoring echo
                // This catches the case where PREEXEC arrived during our last iteration
                // but we hit the timeout check before processing it
                if let Ok(n) = read_nonblocking(self.pty_fd, &mut self.pty_buf) {
                    if n > 0 {
                        for output in self.marker_parser.feed(&self.pty_buf[..n]) {
                            match output {
                                ParseOutput::Bytes(bytes) => output_buffer.extend_from_slice(bytes),
                                ParseOutput::Marker(MarkerEvent::Preexec) => {
                                    self.flush_output_buffer(&output_buffer)?;
                                    self.restore_echo()?;
                                    return Ok(InjectionResult::PreexecReceived);
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // Proceed without PREEXEC confirmation (degraded mode)
                self.flush_output_buffer(&output_buffer)?;
                self.restore_echo()?;
                log::warn!("PREEXEC timeout ({}ms); proceeding in degraded mode",
                           self.config.preexec_timeout_ms);
                return Ok(InjectionResult::Timeout);
            }
        }
    }

    fn flush_output_buffer(&self, buffer: &[u8]) -> Result<()> {
        // Note: echo suppression already handled by termios ECHO flag
        write_all(STDOUT_FILENO, buffer)?;
        Ok(())
    }
}
```

**Key properties:**

- **No deadlock**: PTY output is continuously drained
- **Buffered user input**: User keystrokes during injection are preserved and processed after transition
- **Configurable timeout**: Default 500ms, configurable via `--preexec-timeout`, hard cap at 3000ms to prevent indefinite hangs
- **Late-check before timeout**: One final non-blocking PTY read before declaring timeout, catching late-arriving PREEXEC markers
- **Echo already suppressed**: The termios approach means no echo bytes to filter from the buffer
- **SIGWINCH handled**: Terminal resizes during Injecting mode are propagated to the PTY immediately. This is safe because the child command hasn't started rendering output yet. Without this, a resize during injection would leave the PTY with stale dimensions, causing layout corruption when the command starts executing.

**Timeout configuration**:
```bash
# Increase timeout for slow systems or complex prompt commands
wrashpty --preexec-timeout 1500

# Maximum allowed value (hard cap)
wrashpty --preexec-timeout 3000  # Values > 3000 are clamped to 3000
```

The 3000ms hard cap exists because longer waits indicate a fundamental problem with shell integration (missing DEBUG trap, Bash not sourcing the rcfile, etc.) and the user experience degrades rapidly with longer waits.

---

## 7. Chrome Layer

### Purpose

The chrome layer provides a persistent UI frame around the main content area: a customizable top bar (menu, project name, tabs) and footer (status line, keybinding hints, git info). This is optional and can be toggled at runtime via keybinding or CLI flag (`--no-chrome`).

### Implementation via Scroll Regions

Terminal scroll regions (VT100's DECSTBM escape sequence) confine all scrolling to a subregion of the screen. This is the same mechanism tmux and screen use for their status bars.

**Layout when chrome is active:**

```
Row 0        │ Top bar (menu, project name)                 │  ← outside scroll region
─────────────┼──────────────────────────────────────────────┤
Row 1        │                                              │
  ...        │ Scroll region (reedline + command output)    │  ← DECSTBM region
Row N-2      │                                              │
─────────────┼──────────────────────────────────────────────┤
Row N-1      │ Footer (status, hints, git info)             │  ← outside scroll region
```

### Integration with Existing Modes

**Edit mode**: reedline sees a terminal of size `(cols, rows - 2)`. It renders the prompt, completions, and suggestions within the scroll region. It never touches row 0 or the last row because the terminal enforces the margins.

**Passthrough mode**: The PTY slave's window size is set to `(cols, rows - 2)` via TIOCSWINSZ. Child programs (vim, htop) believe the terminal is that size and render within the scroll region. The bars remain untouched.

### Alternate Screen Buffer Handling

#### The Scroll Region Corruption Problem

Scroll regions have a critical flaw when combined with alternate screen buffers:

1. Chrome is enabled: scroll region set to rows 2..(height-1)
2. User runs vim
3. Vim switches to alternate screen buffer (`\x1b[?1049h`)
4. Vim exits, restoring main screen buffer (`\x1b[?1049l`)
5. **Bug**: Scroll region is still active on main screen
6. User's shell output is now confined to middle rows
7. Top bar and footer are gone, but scroll region persists
8. Terminal appears corrupted

This occurs because alternate screen buffer switches don't automatically reset scroll regions.

#### MVP REQUIREMENT: Disable Scroll Regions in Passthrough

**MANDATORY for MVP**: Chrome scroll regions are disabled during Passthrough mode and redrawn only when entering Edit mode. This eliminates the corruption risk entirely.

```rust
impl Chrome {
    /// REQUIRED: Called on every transition to Passthrough mode.
    /// Always resets scroll region to prevent corruption.
    fn enter_passthrough_mode(&mut self) -> Result<()> {
        // Always reset scroll region, even if chrome appears inactive
        // This is defensive against state machine bugs
        write!(stdout(), "\x1b[r")?;
        stdout().flush()?;

        if self.is_active() {
            // Optionally clear bar areas (they won't update during Passthrough)
            // Bars will be redrawn when returning to Edit mode
        }
        Ok(())
    }

    /// Called when transitioning from Passthrough to Edit mode.
    fn enter_edit_mode(&mut self, total_rows: u16) -> Result<()> {
        if !self.is_active() { return Ok(()); }

        // Re-establish scroll region for controlled Edit mode environment
        self.setup_scroll_region(total_rows)?;
        self.draw_top_bar()?;
        self.draw_footer(total_rows)?;
        Ok(())
    }
}
```

**State machine enforcement:**

| Transition | Scroll Region Action |
|------------|---------------------|
| Any → Passthrough | **ALWAYS** reset to full screen (`\x1b[r`) |
| Any → Edit (chrome active) | Establish scroll region, draw bars |
| Any → Edit (chrome inactive) | No scroll region changes |
| Passthrough → Passthrough | No action (already reset) |

This approach:
- Eliminates all corruption scenarios from alternate screen programs
- Simplifies implementation (no CSI parsing required for MVP)
- Bars are visible during Edit mode, which is when status info is most useful
- Any future alt-screen detection is purely additive

#### FUTURE/OPT-IN: CSI-Based Alternate Screen Detection

**Status**: NOT IMPLEMENTED for MVP. Gated behind opt-in flag `--experimental-alt-screen-detection`.

For future versions where chrome bars should remain visible during simple commands (not full-screen apps), a CSI sequence parser can detect alternate screen transitions. **This is complex and risky; MVP disables it by default.**

**CRITICAL ARCHITECTURE CONSTRAINT**: CSI alt-screen detection, if implemented, MUST be integrated into the streaming byte pump path—the same path that handles OSC marker parsing. There is no post-hoc alternative.

**Why non-streaming detection is impossible:**
1. **Split sequences**: A `\x1b[?1049h` sequence can be split across read boundaries (`\x1b[?10` in one read, `49h` in the next). Only a stateful streaming parser handles this correctly.
2. **Timing**: By the time a post-hoc regex scan could find the sequence, the bytes have already been forwarded to stdout. The terminal has already switched screens, but chrome state is stale.
3. **Interleaving**: Alt-screen sequences can be interleaved with output data. A non-streaming approach would need to buffer all output, adding latency and memory pressure.
4. **Correctness**: Missing a single transition (enter or exit) leaves chrome in the wrong state indefinitely.

**For MVP**: The answer is simple—don't implement CSI detection. The scroll-region-reset-on-Passthrough approach handles all cases safely, at the cost of bars not being visible during command execution.

**Prerequisites before enabling (post-MVP):**
1. Integration with the streaming marker parser (shared state machine, unified buffer)
2. Comprehensive tests with sequences split across read boundaries
3. Testing with edge cases: vim, htop, tmux, nested sessions, rapid alt-screen toggling

**Robust Solution: CSI Sequence Parser** (for future implementation)

Extend the marker parser's state machine to also track CSI sequences (ESC [ ... final byte). This handles split reads properly.

```rust
/// Alternate screen escape sequence codes (DEC private modes)
const ALT_SCREEN_1049: u16 = 1049;  // xterm: save cursor + switch to alt screen
const ALT_SCREEN_47: u16 = 47;      // legacy: switch to alt screen buffer
const ALT_SCREEN_1047: u16 = 1047;  // switch to alt screen (no cursor save)

/// CSI sequence parser state
#[derive(Debug, Clone)]
enum CsiState {
    /// Not in a CSI sequence
    Normal,
    /// Saw ESC
    EscSeen,
    /// Saw ESC [, accumulating parameters
    CsiBody {
        params: [u8; 16],   // Raw parameter bytes
        param_len: usize,
        is_private: bool,   // Saw '?' prefix
    },
}

pub struct AltScreenDetector {
    state: CsiState,
    in_alt_screen: bool,
}

impl AltScreenDetector {
    pub fn new() -> Self {
        Self {
            state: CsiState::Normal,
            in_alt_screen: false,
        }
    }

    /// Process a byte, returning any detected events.
    /// Call this for every byte of PTY output (can be integrated with marker parser).
    pub fn feed_byte(&mut self, byte: u8) -> Option<AltScreenEvent> {
        match &mut self.state {
            CsiState::Normal => {
                if byte == 0x1b {
                    self.state = CsiState::EscSeen;
                }
                None
            }
            CsiState::EscSeen => {
                if byte == b'[' {
                    self.state = CsiState::CsiBody {
                        params: [0; 16],
                        param_len: 0,
                        is_private: false,
                    };
                } else {
                    self.state = CsiState::Normal;
                }
                None
            }
            CsiState::CsiBody { params, param_len, is_private } => {
                match byte {
                    // Private mode prefix
                    b'?' if *param_len == 0 => {
                        *is_private = true;
                        None
                    }
                    // Parameter bytes (digits and semicolons)
                    b'0'..=b'9' | b';' => {
                        if *param_len < params.len() {
                            params[*param_len] = byte;
                            *param_len += 1;
                        }
                        None
                    }
                    // Final byte: 'h' = set mode, 'l' = reset mode
                    b'h' | b'l' if *is_private => {
                        let entering = byte == b'h';
                        let event = self.check_alt_screen_mode(
                            &params[..*param_len],
                            entering
                        );
                        self.state = CsiState::Normal;
                        event
                    }
                    // Any other final byte ends the sequence
                    0x40..=0x7e => {
                        self.state = CsiState::Normal;
                        None
                    }
                    // Invalid byte resets parser
                    _ => {
                        self.state = CsiState::Normal;
                        None
                    }
                }
            }
        }
    }

    fn check_alt_screen_mode(&mut self, params: &[u8], entering: bool) -> Option<AltScreenEvent> {
        // Parse parameter as number
        let param_str = std::str::from_utf8(params).ok()?;
        let mode: u16 = param_str.parse().ok()?;

        match mode {
            ALT_SCREEN_1049 | ALT_SCREEN_47 | ALT_SCREEN_1047 => {
                if entering && !self.in_alt_screen {
                    self.in_alt_screen = true;
                    Some(AltScreenEvent::Enter)
                } else if !entering && self.in_alt_screen {
                    self.in_alt_screen = false;
                    Some(AltScreenEvent::Exit)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn is_in_alt_screen(&self) -> bool {
        self.in_alt_screen
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AltScreenEvent {
    Enter,
    Exit,
}
```

**Integration with byte pump:**

```rust
impl Pump {
    fn process_pty_output(&mut self, data: &[u8]) -> Result<Vec<AltScreenEvent>> {
        let mut events = Vec::new();

        for &byte in data {
            // Check for alt screen transitions
            if let Some(event) = self.alt_screen_detector.feed_byte(byte) {
                events.push(event);
            }
        }

        // Forward all bytes to stdout (alt screen detection is passive)
        write_all(STDOUT_FILENO, data)?;

        Ok(events)
    }
}
```

**Why this is necessary:**

| Approach | Handles Split Reads | Memory | Complexity |
|----------|---------------------|--------|------------|
| `windows().any()` | ❌ No | O(1) | Simple |
| Ring buffer + search | ⚠️ Partial | O(n) | Medium |
| CSI parser | ✅ Yes | O(1) | Medium |

The CSI parser correctly handles all edge cases:
- Sequence split across reads: `\x1b` in one read, `[?1049h` in next
- Multiple modes set in one sequence: `\x1b[?1049;47h`
- Interleaved with other CSI sequences

#### Chrome Behavior During Alternate Screen

```rust
impl Chrome {
    fn handle_alt_screen_enter(&mut self) -> Result<()> {
        if !self.is_active() { return Ok(()); }

        // Reset scroll region before alt screen program runs
        // This ensures the program gets a clean full-screen environment
        write!(stdout(), "\x1b[r")?;  // Reset to full screen
        stdout().flush()?;

        // Don't draw bars - they would be overwritten anyway
        self.alt_screen_active = true;
        Ok(())
    }

    fn handle_alt_screen_exit(&mut self) -> Result<()> {
        if !self.is_active() { return Ok(()); }

        self.alt_screen_active = false;

        // Re-establish scroll region now that we're back on main screen
        let (cols, rows) = TerminalGuard::get_size()?;
        self.setup_scroll_region(rows)?;
        self.draw_top_bar(cols)?;
        self.draw_footer(cols, rows)?;
        Ok(())
    }

    fn setup_scroll_region(&self, total_rows: u16) -> Result<()> {
        // DECSTBM: set scroll region to rows 2 through (height-1)
        write!(stdout(), "\x1b[2;{}r", total_rows - 1)?;
        stdout().flush()
    }
}
```

#### Reminder: MVP Enforces Scroll Region Reset

As specified in "MVP REQUIREMENT: Disable Scroll Regions in Passthrough" above, the scroll region is **always** reset on every transition to Passthrough mode. This is not optional for MVP.

**Tradeoffs (informational):**

| Approach | Pros | Cons | MVP Status |
|---|---|---|---|
| Always reset in Passthrough | Simpler, corruption-proof | Bars not visible during commands | **REQUIRED** |
| CSI-based detection | Bars visible during simple commands | Complex, potential missed sequences | Future opt-in |

Any CSI-based alternate screen detection for preserving bars during simple commands is gated behind `--experimental-alt-screen-detection` and requires the prerequisites listed above.

### Toggle Sequence

```
Chrome ON → OFF:
  1. Clear row 0 and row N-1
  2. Reset scroll region to full screen: DECSTBM(0, rows-1)
  3. Resize PTY to (cols, rows)         — child gets SIGWINCH
  4. Notify reedline of new size        — prompt redraws
  5. Set chrome_mode = Headless

Chrome OFF → ON:
  1. Set scroll region: DECSTBM(1, rows-2)
  2. Draw top bar at row 0
  3. Draw footer at row N-1
  4. Resize PTY to (cols, rows-2)       — child gets SIGWINCH
  5. Notify reedline of new size        — prompt redraws
  6. Set chrome_mode = Full
```

### SIGWINCH Handling with Chrome

SIGWINCH handling depends on the current mode:

**In Edit mode** (chrome visible, scroll regions active):
```
on SIGWINCH in Edit mode:
    // reedline handles this internally via crossterm
    // Wrapper does NOT resize PTY here; reedline owns the terminal
    // PTY will be resized on transition to Injecting/Passthrough
```

**In Passthrough/Injecting mode** (chrome NOT visible, scroll regions reset):
```
on SIGWINCH in Passthrough/Injecting:
    (cols, rows) = query real terminal size
    resize PTY to (cols, rows)  // FULL rows, no subtraction
    // Chrome bars are not visible; they'll be redrawn on Edit transition
```

**On transition back to Edit mode** (chrome re-established):
```
on transition_to_edit:
    (cols, rows) = query real terminal size
    if chrome_active:
        effective_rows = rows - 2
        resize PTY to (cols, effective_rows)
        set scroll region to DECSTBM(1, effective_rows)
        redraw top bar at row 0
        redraw footer at row (rows - 1)
    else:
        // PTY already at full size from Passthrough
    notify reedline of (cols, effective_rows)
```

This design ensures the child process always has the full terminal during command execution (Passthrough), and only shrinks the PTY when chrome bars are actually visible (Edit mode).

### Minimum Terminal Size

Chrome requires a minimum terminal size to function:

| Dimension | Minimum | Behavior Below Minimum |
|---|---|---|
| Rows | 5 | Chrome auto-disables, full terminal given to content |
| Columns | 20 | Chrome auto-disables, full terminal given to content |

**Startup behavior**:
```rust
fn init_chrome(&mut self) -> Result<()> {
    let (cols, rows) = TerminalGuard::get_size()?;

    if rows < 5 || cols < 20 {
        // Terminal starts below minimum: chrome disabled from startup
        self.chrome_suspended = true;
        self.chrome_was_requested = self.config.chrome_enabled;
        log::info!("Terminal too small ({}x{}); chrome disabled", cols, rows);
        return Ok(());
    }

    if self.config.chrome_enabled {
        self.enable_chrome()?;
    }
    Ok(())
}
```

**Dynamic resize behavior**:
1. **Resize below minimum**: Chrome temporarily disabled (scroll region reset, bars cleared), `chrome_suspended` flag set
2. **Resize above minimum**: Chrome re-enabled if `chrome_was_requested` is true (user originally wanted chrome)
3. **Never lose user preference**: If user starts with chrome enabled but terminal is too small, chrome activates when terminal becomes large enough

**Example state transitions**:
```
Start at 80x24 with --chrome → chrome enabled
Resize to 15x4 → chrome suspended, full terminal
Resize to 80x24 → chrome re-enabled automatically

Start at 15x4 with --chrome → chrome disabled from startup
Resize to 80x24 → chrome enabled (user preference preserved)
```

This prevents unusable UI states and ensures the wrapper degrades gracefully on very small terminals.

### Git Status Strategy

Git status in the footer must be fast to avoid blocking the prompt.

#### Hybrid Threading Model Details

The architecture uses a **hybrid threading model**: the main event loop, state machine, and terminal control all run on a single main thread, while background I/O for optional features uses detached worker threads. This is intentional:

| Aspect | Main Thread | Worker Threads |
|--------|-------------|----------------|
| Terminal I/O | ✓ All | ✗ Never |
| State machine | ✓ All | ✗ Never |
| PTY operations | ✓ All | ✗ Never |
| Git status | ✗ Result polling only | ✓ Command execution |
| Completion scan | ✗ Result polling only | ✓ Filesystem traversal |

Worker threads never touch the terminal, PTY, or mode state. They only produce data that the main thread polls for during safe points (between mode transitions).

#### Option A: Thread-Based Git Status (Recommended)

```rust
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

pub struct GitStatusCache {
    cached: GitStatus,
    cwd: PathBuf,
    last_check: Instant,
    pending_rx: Option<Receiver<GitStatus>>,
    pending_handle: Option<JoinHandle<()>>,
}

impl GitStatusCache {
    pub fn new() -> Self {
        Self {
            cached: GitStatus::default(),
            cwd: PathBuf::new(),
            last_check: Instant::now(),
            pending_rx: None,
            pending_handle: None,
        }
    }

    /// Get cached status, triggering background refresh if stale.
    pub fn get(&mut self, cwd: &Path) -> &GitStatus {
        // Poll for completed refresh first
        self.poll_pending();

        // Check if refresh needed
        let stale = self.cwd != cwd || self.last_check.elapsed() > Duration::from_secs(2);
        if stale && self.pending_rx.is_none() {
            self.spawn_refresh(cwd);
        }

        &self.cached
    }

    fn spawn_refresh(&mut self, cwd: &Path) {
        let (tx, rx) = channel();
        let cwd = cwd.to_owned();

        let handle = thread::spawn(move || {
            // Timeout wrapper: kill git if it takes too long
            let status = match with_timeout(Duration::from_millis(200), || get_git_status(&cwd)) {
                Some(s) => s,
                None => GitStatus::default(),  // Timeout
            };
            let _ = tx.send(status);  // Ignore error if receiver dropped
        });

        self.pending_rx = Some(rx);
        self.pending_handle = Some(handle);
        self.cwd = cwd.to_owned();
    }

    fn poll_pending(&mut self) {
        if let Some(ref rx) = self.pending_rx {
            match rx.try_recv() {
                Ok(status) => {
                    self.cached = status;
                    self.last_check = Instant::now();
                    self.pending_rx = None;
                    self.pending_handle = None;
                }
                Err(TryRecvError::Empty) => {
                    // Still running, continue
                }
                Err(TryRecvError::Disconnected) => {
                    // Thread died, clear pending
                    self.pending_rx = None;
                    self.pending_handle = None;
                }
            }
        }
    }
}

#[derive(Default, Clone)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
}
```

#### Option B: Non-Blocking Process (Stricter Single-Thread)

For a stricter single-threaded design, spawn `git` as a child process and poll its output:

```rust
use std::process::{Command, Stdio, Child};

pub struct GitStatusCache {
    cached: GitStatus,
    pending_child: Option<Child>,
    pending_output: Vec<u8>,
    cwd: PathBuf,
    last_check: Instant,
}

impl GitStatusCache {
    fn spawn_refresh(&mut self, cwd: &Path) {
        let child = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        if let Ok(c) = child {
            self.pending_child = Some(c);
            self.pending_output.clear();
            self.cwd = cwd.to_owned();
        }
    }

    fn poll_pending(&mut self) {
        if let Some(ref mut child) = self.pending_child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    // Child finished; read output
                    if status.success() {
                        if let Some(stdout) = child.stdout.as_mut() {
                            // Read available output
                            let mut buf = [0u8; 256];
                            if let Ok(n) = std::io::Read::read(stdout, &mut buf) {
                                self.pending_output.extend_from_slice(&buf[..n]);
                            }
                        }
                        self.parse_output();
                    }
                    self.pending_child = None;
                    self.last_check = Instant::now();
                }
                Ok(None) => {
                    // Still running
                }
                Err(_) => {
                    self.pending_child = None;
                }
            }
        }
    }
}
```

**Recommended approach for MVP**: Use Option A (thread-based). It's simpler and `git` commands are I/O-bound anyway. The thread isolation is clean: worker produces data, main thread polls result.

**Key properties:**
- **Never blocks prompt**: Returns cached value immediately
- **Background refresh**: Updates asynchronously between commands
- **Timeout**: Individual git commands timeout after 200ms
- **Cwd tracking**: Invalidates cache on directory change
- **Thread safety**: Worker thread never touches terminal or shared state

### Wide Character Handling

Bar content uses `unicode-width` crate for correct display width calculation:

```rust
use unicode_width::UnicodeWidthStr;

fn truncate_to_width(s: &str, max_width: usize) -> String {
    let mut width = 0;
    let mut result = String::new();

    for ch in s.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        width += ch_width;
        result.push(ch);
    }

    // Pad with spaces to exact width
    while width < max_width {
        result.push(' ');
        width += 1;
    }
    result
}
```

**Considerations:**
- CJK characters are width 2
- Emoji may be width 2 (depending on terminal)
- Combining characters are width 0
- Terminal may have bugs with certain Unicode; use conservative subset in chrome

### Terminal Capability Detection

Before enabling chrome, detect terminal capabilities:

```rust
fn detect_capabilities() -> TerminalCaps {
    TerminalCaps {
        // Check TERM variable for known-good terminals
        scroll_regions: matches!(
            std::env::var("TERM").as_deref(),
            Ok("xterm" | "xterm-256color" | "screen" | "tmux" |
               "alacritty" | "kitty" | "foot" | "wezterm" | _)
        ),

        // Check for known-bad terminals
        force_headless: matches!(
            std::env::var("TERM").as_deref(),
            Ok("dumb" | "cons25" | "emacs")
        ),

        // Detect nested multiplexer
        nested_multiplexer: std::env::var("TMUX").is_ok()
                          || std::env::var("STY").is_ok(),

        // Color support
        colors: detect_color_support(),
    }
}
```

If `force_headless` is true, chrome is disabled regardless of `--no-chrome` flag.

### Alternate Screen Modals

For discrete interactive actions (pickers, confirmations, command palette), the wrapper uses alternate screen modals rather than floating overlays:

1. Switch to the terminal's alternate screen buffer
2. Render the modal using crossterm (or ratatui for complex layouts)
3. Capture user selection
4. Switch back to the main screen buffer

The main screen's state (scroll region, bars, reedline buffer) is preserved because the alternate buffer is independent. This is how fzf and lazygit work when invoked from a shell.

---

## 8. Terminal State Safety

Terminal state corruption — where raw mode persists after the program exits, leaving the terminal unusable — is the most severe failure mode. The architecture employs defense in depth:

### Layer 1: RAII Guard with Fallback Reset

A `TerminalGuard` struct saves the original termios settings on construction and restores them on `Drop`. This covers normal exit paths and unwinding from panics (since Rust runs destructors during unwinding by default).

**Critical**: If termios restoration fails, the guard writes terminal reset escape sequences as a fallback. This handles edge cases where the fd is no longer valid but stdout still works.

```rust
use std::io::Write;

pub struct TerminalGuard {
    original: Termios,
    fd: RawFd,
    scroll_region_active: bool,
    terminal_rows: u16,
}

impl TerminalGuard {
    pub fn new(fd: RawFd) -> Result<Self> {
        let original = tcgetattr(fd)?;
        Ok(Self {
            original,
            fd,
            scroll_region_active: false,
            terminal_rows: 24,  // Default; updated on resize
        })
    }

    pub fn set_scroll_region_active(&mut self, active: bool, rows: u16) {
        self.scroll_region_active = active;
        self.terminal_rows = rows;
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Attempt 1: Standard termios restoration
        let termios_result = tcsetattr(self.fd, SetArg::TCSANOW, &self.original);

        // Attempt 2: If termios fails, try escape sequence reset
        if termios_result.is_err() {
            // Write terminal reset sequences directly to stdout
            // These work even if the fd is invalid, as long as stdout is connected
            let mut reset_sequence = Vec::with_capacity(64);

            // DECSTBM reset (scroll region to full screen)
            if self.scroll_region_active {
                reset_sequence.extend_from_slice(b"\x1b[r");
            }

            // RIS (Reset to Initial State) - full terminal reset
            // This is aggressive but ensures the terminal is usable
            reset_sequence.extend_from_slice(b"\x1b[!p");  // Soft reset (DECSTR)
            reset_sequence.extend_from_slice(b"\x1b[?47l"); // Exit alternate screen if active
            reset_sequence.extend_from_slice(b"\x1b[?1049l"); // Exit alternate screen (xterm)
            reset_sequence.extend_from_slice(b"\x1b[?25h"); // Show cursor
            reset_sequence.extend_from_slice(b"\x1b[0m");   // Reset attributes

            // Best-effort write; ignore errors since we're in cleanup
            let _ = std::io::stdout().write_all(&reset_sequence);
            let _ = std::io::stdout().flush();
        }

        // Attempt 3: Reset scroll region even if termios succeeded
        // (termios restore doesn't affect scroll regions)
        if self.scroll_region_active {
            let _ = std::io::stdout().write_all(b"\x1b[r");
            let _ = std::io::stdout().flush();
        }
    }
}
```

**Fallback sequence explanation:**

| Sequence | Purpose |
|----------|---------|
| `\x1b[r` | Reset scroll region to full screen (DECSTBM) |
| `\x1b[!p` | Soft terminal reset (DECSTR) - resets modes without clearing screen |
| `\x1b[?47l` | Exit alternate screen buffer (legacy) |
| `\x1b[?1049l` | Exit alternate screen buffer (xterm) |
| `\x1b[?25h` | Show cursor (in case it was hidden) |
| `\x1b[0m` | Reset text attributes (colors, bold, etc.) |

This ensures that even in worst-case scenarios (fd closed, termios corrupted), the terminal will be usable.

### Layer 2: Panic Hook

A custom panic hook is installed before any terminal manipulation occurs. It restores terminal state before the default hook prints the panic message. This ensures the panic backtrace itself is readable.

```rust
let original_termios = tcgetattr(stdin_fd)?;
std::panic::set_hook(Box::new(move |info| {
    let _ = tcsetattr(stdin_fd, SetArg::TCSANOW, &original_termios);
    eprintln!("{info}");
}));
```

### Layer 3: Signal Handlers

`SIGTERM` and `SIGINT` (when not handled by reedline) trigger cleanup via the same restoration path. `signal-hook` is used to convert signals into events that the main loop processes, rather than performing cleanup inside signal handlers (which are restricted in what system calls they may make).

### Layer 4: Crossterm Cleanup

`crossterm::terminal::disable_raw_mode()` is called explicitly on the transition from Edit to Passthrough, and again during shutdown. This is belt-and-suspenders with the termios guard.

### Layer 5: Scroll Region Reset

When chrome is active, the TerminalGuard also saves and restores the scroll region. On drop, it executes `DECSTBM(0, rows-1)` to reset to full screen before restoring termios.

### Terminal Restoration Priority

The restoration mechanisms have a defined priority order to handle race conditions between panics and signals:

| Priority | Mechanism | When It Runs | Async-Signal-Safe |
|----------|-----------|--------------|-------------------|
| 1 (Highest) | Panic hook | Before default panic handler | Yes (uses `write_all` directly) |
| 2 | RAII guards (`TerminalGuard`, `EchoGuard`) | During stack unwinding | Yes (uses `tcsetattr`, `write`) |
| 3 | Signal handlers | On SIGTERM/SIGHUP delivery | Yes (sets atomic flag only) |
| 4 | Explicit cleanup | Normal shutdown path | N/A |
| 5 (Lowest) | Terminal emulator | On process exit | N/A |

**Race condition handling**:

```rust
// Atomic flag prevents double-restoration
static TERMINAL_RESTORED: AtomicBool = AtomicBool::new(false);

fn restore_terminal_once(original: &Termios) {
    // CAS ensures only one restoration attempt succeeds
    if TERMINAL_RESTORED.compare_exchange(
        false, true,
        Ordering::SeqCst, Ordering::SeqCst
    ).is_ok() {
        let _ = tcsetattr(STDIN_FILENO, SetArg::TCSANOW, original);
        let _ = std::io::stdout().write_all(b"\x1b[r\x1b[?25h");
        let _ = std::io::stdout().flush();
    }
}
```

**Why panic hook has highest priority**: Panics can occur inside signal handlers or during RAII cleanup. The panic hook must run first to ensure the terminal is usable before the panic message is printed. The hook uses only async-signal-safe operations (`write_all` with direct fd access, no allocations).

---

## 9. Security Considerations

### Tempfile Security

The generated rcfile uses the `tempfile` crate which provides:
- **Atomic creation**: File is created with `O_EXCL` flag, preventing race conditions
- **Restrictive permissions**: File is created with mode 0600 (owner read/write only)
- **Unpredictable names**: Uses OS-provided random naming
- **Secure directory**: Uses `$TMPDIR` or `/tmp` with sticky bit

**Additional measure**: The rcfile is written and closed before passing to Bash. Bash opens it read-only, so even if an attacker can predict the name, they cannot inject content after creation.

### PROMPT_COMMAND Capture

The generated bashrc captures the user's existing `PROMPT_COMMAND` at generation time, not at runtime:

```bash
# WRONG (vulnerable to injection):
# eval "${PROMPT_COMMAND}"  # Could be modified after wrashpty starts

# CORRECT (captured at rcfile generation):
__user_prompt_command='<literal value captured at startup>'
```

This prevents an attacker from modifying `PROMPT_COMMAND` in a subshell or via environment manipulation to inject code into the precmd function.

### Marker Authentication

To prevent marker spoofing by malicious programs, markers include a session-unique token:

```
ESC ] 777 ; <session_token> ; <type> [ ; <payload> ] BEL
```

The session token is a 16-character hex string (64 bits of entropy) generated at wrapper startup and embedded in both the rcfile and the marker parser. A program that doesn't know the token cannot forge valid markers.

**Token generation using `getrandom`:**

Use `getrandom` instead of `rand` for security-critical randomness. This provides cryptographically secure random bytes directly from the OS (`/dev/urandom` on Linux, `getentropy` on macOS).

```rust
/// Generate a cryptographically secure session token.
/// Returns 8 random bytes (64 bits of entropy) as a 16-character hex string.
/// Falls back to timestamp-based token if getrandom fails (extremely rare).
fn generate_session_token() -> SessionToken {
    let mut bytes = [0u8; 8];

    match getrandom::getrandom(&mut bytes) {
        Ok(()) => SessionToken { bytes },
        Err(e) => {
            // Extremely rare: getrandom failed (broken /dev/urandom, sandboxed environment)
            // Fall back to timestamp-based token with warning
            log::warn!(
                "getrandom failed ({}), using timestamp-based token. \
                 Security note: timestamp tokens are weaker but better than no authentication.",
                e
            );

            // Use current timestamp as weak entropy source
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();

            let nanos = now.as_nanos() as u64;
            bytes.copy_from_slice(&nanos.to_le_bytes());
            SessionToken { bytes }
        }
    }
}

/// Session token wrapper providing both raw bytes and hex string access.
#[derive(Clone)]
pub struct SessionToken {
    bytes: [u8; 8],
}

impl SessionToken {
    /// Get raw bytes for efficient comparison in marker parser.
    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.bytes
    }

    /// Get hex string for embedding in bashrc.
    pub fn as_hex(&self) -> String {
        const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
        let mut hex = String::with_capacity(16);
        for byte in &self.bytes {
            hex.push(HEX_CHARS[(byte >> 4) as usize] as char);
            hex.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
        }
        hex
    }
}

// Embed in rcfile:
// __wrash_token="<hex_string>"
// printf '\e]777;%s;PROMPT\a' "$__wrash_token"
```

**Why `getrandom` over `rand`:**

| Aspect | `rand` | `getrandom` |
|--------|--------|-------------|
| Cryptographic quality | Optional (depends on RNG choice) | Always (OS-provided CSPRNG) |
| Dependencies | Large (`rand` + `rand_core` + algorithm crates) | Minimal (~100 lines) |
| Initialization | May need seeding | Ready immediately |
| Security audit surface | Large | Small |

For a security token that prevents spoofing, cryptographic randomness is required. `getrandom` provides this with minimal dependency overhead.

**64 bits vs 128 bits**: 64 bits of entropy (2^64 possibilities) is sufficient for session tokens because:
1. Tokens are ephemeral (new token each wrapper invocation)
2. Attacker has no oracle to test tokens (wrong tokens are silently ignored)
3. Brute-force requires ~10^19 attempts on average

If paranoid, increase to 16 bytes (128 bits) by doubling the array size.

**Security event handling:**

When the parser sees a well-formed OSC 777 marker with an invalid token:

1. The marker is treated as passthrough bytes (not a `MarkerEvent`)
2. A security event counter is incremented
3. Events are logged to file (not terminal — could be part of an attack)
4. If counter exceeds 100 events, log a warning about possible attack

This prevents both denial-of-service (stuck in wrong mode) and information disclosure (attacker learning token through timing attacks).

### PATH Completion Security

The completer scans directories in `$PATH` for executables. Risks and mitigations:

| Risk | Mitigation |
|---|---|
| Slow/hanging filesystem (NFS, FUSE) | Timeout of 100ms per directory; skip on timeout |
| Attacker-controlled PATH entry | Only read directory listings; never execute or stat individual files beyond `readdir` |
| Information disclosure | Completion candidates are only shown to the user, never logged or transmitted |
| Symlink attacks | Follow symlinks for executable check (standard behavior); no security impact since we only report names |

### Input Validation

All data received from the PTY is treated as untrusted:
- **Marker parser**: Bounds-checked buffer, rejects oversized sequences
- **Exit code parsing**: Validates integer format, defaults to 1 on parse failure
- **No shell expansion**: Marker payloads are never passed to shell interpreters

### Signal Handler Safety

Signal handlers only write to a pipe and return immediately. All cleanup logic runs in the main loop where it's safe to call any function. This avoids async-signal-safety violations.

---

## 10. Module Architecture

### Dependency Direction

Modules are organized in layers with strict dependency direction: higher layers depend on lower layers, never the reverse.

```
Layer 3 (Features):    suggest, complete, prompt, history
Layer 1.5 (UI):        chrome (top bar, footer, scroll regions)
Layer 2 (Editor):      editor (integrates reedline with Layer 1 + 3)
Layer 1 (Platform):    pty, terminal, marker, pump, signals, bashrc
Layer 0 (Core):        app (orchestrator), main (entry)
```

Layer 0 depends on everything. Layer 1 modules depend on external crates and the standard library only. Layer 3 modules depend on reedline's trait definitions and standard library only. Layer 2 bridges Layer 1 and Layer 3.

This means that `marker.rs`, `pump.rs`, `pty.rs`, and `terminal.rs` can be tested in isolation without reedline or any UI concerns.

### Preventing Dependency Cycles

#### The Cycle Risk

Without careful design, modules can develop circular dependencies that prevent compilation:

```
app.rs → editor.rs (creates EditorHandle)
editor.rs → app.rs (needs Mode, ChromeMode for display) ← CYCLE!
```

```
pump.rs → marker.rs (uses MarkerParser)
marker.rs → pump.rs (needs PumpEvent types) ← CYCLE!
```

#### Solution: Shared Types Module

Introduce a `types.rs` module at the foundation layer with no dependencies on other project modules:

```rust
// src/types.rs — Layer -1 (Foundation)
// All modules can import from here; this module imports from no other project module

/// Application mode state machine
pub enum Mode {
    Initializing,
    Passthrough,
    Edit { last_exit_code: i32 },
    Injecting { pending_line: String },
    Terminating { timeout_deadline: std::time::Instant },
}

/// Chrome visibility state
pub enum ChromeMode {
    Full,
    Headless,
}

/// Events emitted by the marker parser
pub enum MarkerEvent {
    Precmd { exit_code: i32 },
    Prompt,
    Preexec,
}

/// Events from the byte pump
pub enum PumpEvent {
    Continue,
    Marker(MarkerEvent),
    ChildExited,
}

/// Signal events from the signal handler
pub enum SignalEvent {
    Resize,
    ChildExited,
    Terminate,
    Hangup,
    Interrupt,
}

/// Alternate screen transition events
pub enum AltScreenEvent {
    Enter,
    Exit,
}

/// Session token for marker authentication
pub struct SessionToken {
    pub bytes: [u8; 8],
}

/// Git status for chrome footer
#[derive(Default, Clone)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
}
```

**Module Dependency Order** (must be followed to prevent circular imports):

```
Layer -1: types.rs, error.rs
    ↓
Layer 0:  marker.rs, terminal.rs, pty.rs, bashrc.rs, signals.rs
    ↓
Layer 1:  pump.rs, chrome.rs
    ↓
Layer 2:  history.rs, suggest.rs, complete.rs, prompt.rs
    ↓
Layer 3:  editor.rs
    ↓
Layer 4:  app.rs
    ↓
Layer 5:  main.rs
```

**Rule**: Modules may only import from layers below them. No circular dependencies allowed. If two modules need to share types, those types belong in `types.rs`.

#### Dependency Injection for Cross-Layer State

Instead of modules importing each other's types, use method parameters:

```rust
// WRONG: editor.rs imports app.rs types
use crate::app::{Mode, ChromeMode};

impl EditorHandle {
    fn render_prompt(&self) {
        match self.app.mode {  // Requires reference to App
            // ...
        }
    }
}

// CORRECT: editor.rs only imports types.rs
use crate::types::{Mode, ChromeMode};

impl EditorHandle {
    fn render_prompt(&mut self, exit_code: i32, duration: Duration) {
        // State passed in, no dependency on App
        self.prompt.set_exit_code(exit_code);
        self.prompt.set_duration(duration);
    }
}
```

#### Module Visibility and Layering

Enforce layering through Rust's visibility system:

```rust
// src/lib.rs or src/main.rs

// Foundation layer - no internal dependencies
pub mod types;
pub mod error;

// Platform layer - depends only on types/error
mod platform {
    pub mod pty;
    pub mod terminal;
    pub mod signals;
    pub mod bashrc;
}

// Protocol layer - depends on types and platform
mod protocol {
    pub mod marker;
    pub mod pump;
}

// UI layer - depends on types and platform
mod ui {
    pub mod chrome;
    pub mod editor;
}

// Features layer - depends on types and reedline traits
mod features {
    pub mod history;
    pub mod suggest;
    pub mod complete;
    pub mod prompt;
}

// Orchestration layer - depends on everything
mod app;
```

#### Trait Objects for Cross-Layer Communication

When modules need to communicate without direct coupling, use traits:

```rust
// In types.rs
pub trait StatusProvider {
    fn exit_code(&self) -> i32;
    fn command_duration(&self) -> Duration;
    fn git_status(&self) -> Option<&GitStatus>;
}

// In app.rs
impl StatusProvider for App {
    fn exit_code(&self) -> i32 { self.last_exit_code }
    fn command_duration(&self) -> Duration { self.last_duration }
    fn git_status(&self) -> Option<&GitStatus> { self.git_cache.get() }
}

// In chrome.rs - no dependency on App
impl Chrome {
    pub fn draw_footer(&self, status: &dyn StatusProvider) {
        // Uses trait, not concrete App type
    }
}
```

#### Recommended Module Structure

```
src/
├── main.rs              # Entry point, minimal code
├── lib.rs               # Module declarations and re-exports
├── types.rs             # Shared types (Mode, MarkerEvent, etc.)
├── error.rs             # Error types (WrapperError, etc.)
├── platform/
│   ├── mod.rs
│   ├── pty.rs           # PTY spawning and management
│   ├── terminal.rs      # Real terminal control
│   ├── signals.rs       # Signal handling
│   └── bashrc.rs        # RC file generation
├── protocol/
│   ├── mod.rs
│   ├── marker.rs        # Marker parser
│   └── pump.rs          # Byte pump
├── ui/
│   ├── mod.rs
│   ├── editor.rs        # reedline integration
│   └── chrome.rs        # Top bar and footer
├── features/
│   ├── mod.rs
│   ├── history.rs       # History provider
│   ├── suggest.rs       # Autosuggestion hinter
│   ├── complete.rs      # Completion providers
│   └── prompt.rs        # Prompt renderer
└── app.rs               # Orchestrator
```

This structure makes dependency violations obvious: if `marker.rs` tries to import from `ui/`, it's clearly crossing layers.

### Module Responsibilities and Public APIs

Each module is described below with its public surface, internal concerns, and integration points.

---

#### `app.rs` — Orchestrator

**Responsibility**: Owns the top-level event loop, mode state machine, chrome state, and module lifetimes.

**Public API**:
```rust
pub struct App { /* private fields */ }

impl App {
    pub fn new(config: AppConfig) -> Result<Self>;
    pub fn run(&mut self) -> Result<ExitCode>;
    pub fn toggle_chrome(&mut self) -> Result<()>;
}

pub struct AppConfig {
    pub chrome_enabled: bool,   // --no-chrome flag sets this to false
    // ... other config
}
```

**Internal state** includes:
- `mode: Mode` — Edit or Passthrough
- `chrome_mode: ChromeMode` — Full or Headless
- `last_exit_code: i32`
- `command_start_time: Instant`

**Internal concerns**: Mode transitions, poll loop construction, reedline lifecycle management, signal dispatch, chrome toggle coordination.

**Depends on**: Every other module including `chrome`.

---

#### `pty.rs` — PTY and Child Process

**Responsibility**: Spawn Bash on a PTY pair. Provide read/write access to the PTY master. Resize the PTY.

**Public API**:
```rust
pub struct PtyHandle { /* private fields */ }

impl PtyHandle {
    pub fn spawn(config: &PtyConfig) -> Result<Self>;
    pub fn master_fd(&self) -> RawFd;
    pub fn write_command(&mut self, line: &str) -> Result<()>;
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()>;
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>>;
}
```

**Internal concerns**: Session setup (`setsid`), PTY allocation, child process lifecycle.

**Depends on**: `portable-pty`, `nix`, `bashrc` (for the rcfile path).

---

#### `bashrc.rs` — RC File Generator

**Responsibility**: Generate a temporary Bash rcfile with shell integration markers.

**Public API**:
```rust
pub struct GeneratedRc { /* private fields — holds tempfile */ }

impl GeneratedRc {
    pub fn create() -> Result<Self>;
    pub fn path(&self) -> &Path;
}

impl Drop for GeneratedRc {
    // Tempfile cleaned up automatically
}
```

**Internal concerns**: Marker function definitions, user bashrc sourcing, PROMPT_COMMAND override ordering, DEBUG trap guard.

**Depends on**: `tempfile`, `dirs`.

---

#### `marker.rs` — Streaming Marker Parser

**Responsibility**: Scan a byte stream for OSC 777 markers. Separate marker events from passthrough bytes.

**Public API**:
```rust
pub enum MarkerEvent {
    Precmd { exit_code: i32 },
    Prompt,
    Preexec,
}

pub enum ParseOutput<'a> {
    Bytes(&'a [u8]),
    Marker(MarkerEvent),
}

pub struct MarkerParser { /* private state */ }

impl MarkerParser {
    pub fn new() -> Self;
    pub fn feed<'a>(&'a mut self, input: &'a [u8]) -> MarkerIter<'a>;
    pub fn flush(&mut self) -> Option<&[u8]>;
}
```

**Internal concerns**: Three-state FSM, fixed-size internal buffer, split-read handling, malformed sequence recovery.

**Depends on**: `thiserror` (for parse error types). No other dependencies.

---

#### `terminal.rs` — Real Terminal Control

**Responsibility**: Manage the real terminal's raw/cooked mode. Query terminal size. Manage scroll regions.

**Public API**:
```rust
pub struct TerminalGuard { /* private fields */ }

impl TerminalGuard {
    pub fn enter_raw_mode() -> Result<Self>;
    pub fn get_size() -> Result<(u16, u16)>;
    pub fn set_scroll_region(top: u16, bottom: u16) -> Result<()>;
    pub fn reset_scroll_region() -> Result<()>;
}

impl Drop for TerminalGuard {
    // Restores original termios and scroll region
}
```

**Internal concerns**: termios save/restore, panic hook coordination, crossterm interop, DECSTBM escape sequences.

**Depends on**: `crossterm`, `nix`.

---

#### `chrome.rs` — UI Frame (Top Bar + Footer)

**Responsibility**: Render and manage the persistent UI chrome. Toggle chrome visibility. Calculate effective viewport size.

**Public API**:
```rust
pub struct Chrome { /* private fields */ }

impl Chrome {
    pub fn new() -> Self;
    pub fn set_active(&mut self, active: bool) -> Result<()>;
    pub fn is_active(&self) -> bool;
    pub fn effective_rows(&self, total_rows: u16) -> u16;
    pub fn draw_top_bar(&self, cols: u16) -> Result<()>;
    pub fn draw_footer(&self, cols: u16) -> Result<()>;
    pub fn update_status(&mut self, status: ChromeStatus);
}

pub struct ChromeStatus {
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub last_exit_code: i32,
    pub last_duration: Duration,
    pub cwd: PathBuf,
}
```

**Internal concerns**: Cursor positioning for bar rendering, styled output, scroll region coordination with terminal.rs, status formatting.

**Depends on**: `crossterm`, `terminal` module.

---

#### `pump.rs` — Passthrough Byte Pump

**Responsibility**: In passthrough mode, efficiently shuttle bytes between stdin and the PTY master, and between the PTY master and stdout, while scanning for markers.

**Public API**:
```rust
pub struct Pump { /* private fields */ }

impl Pump {
    pub fn new(pty_master_fd: RawFd, parser: &mut MarkerParser) -> Self;
    pub fn poll_once(
        &mut self,
        parser: &mut MarkerParser,
        timeout_ms: i32,
    ) -> Result<PumpEvent>;
}

pub enum PumpEvent {
    Continue,
    Marker(MarkerEvent),
    ChildOutput,             // bytes already written to stdout
    ChildExited,
}
```

**Internal concerns**: Buffer management, `poll()` on multiple fds, non-blocking I/O, echo suppression for injected commands.

**Depends on**: `nix` (for `poll`), `marker` module.

---

#### `editor.rs` — Reedline Integration Bridge

**Responsibility**: Configure reedline with the appropriate completers, hinters, history, and prompt. Mediate between reedline's blocking read loop and the rest of the application.

**Public API**:
```rust
pub struct EditorHandle { /* private fields */ }

impl EditorHandle {
    pub fn new(
        history: HistoryHandle,
        completer: Box<dyn Completer>,
        hinter: Box<dyn Hinter>,
        prompt: WrashPrompt,
    ) -> Result<Self>;
    
    pub fn read_line(&mut self) -> Result<EditorResult>;
    pub fn update_prompt(&mut self, exit_code: i32, duration: Duration);
}

pub enum EditorResult {
    Command(String),
    Exit,
    Interrupted,
}
```

**Internal concerns**: reedline configuration, keybinding setup, Vi/Emacs mode selection.

**Depends on**: `reedline`, `history`, `suggest`, `complete`, `prompt` modules.

---

#### `history.rs` — History Provider

**Responsibility**: Load and index Bash's HISTFILE. Provide reedline-compatible history access.

**Public API**:
```rust
pub struct HistoryHandle { /* private fields */ }

impl HistoryHandle {
    pub fn load_from_file(path: &Path) -> Result<Self>;
    pub fn reload(&mut self) -> Result<()>;
    pub fn add_entry(&mut self, line: &str);
    pub fn prefix_search(&self, prefix: &str) -> Vec<&str>;
}

// Also implements reedline::History trait
```

**Internal concerns**: HISTFILE format parsing, deduplication, in-memory indexing.

**HISTFILE Error Handling** (graceful degradation):

| Condition | Behavior |
|-----------|----------|
| Missing HISTFILE | Silently use empty history (common case, not an error) |
| Corrupted lines (invalid UTF-8) | Skip invalid lines with warning logged to file; continue with valid entries |
| Very large HISTFILE (>10MB) | Load only the last 10,000 lines for predictable startup time |
| Permission denied | Log warning, use empty history |
| Locked by another process | Retry 3 times with 100ms delay, then proceed with empty history |

**Large file handling**:
```rust
const MAX_HISTORY_LINES: usize = 10_000;

fn load_history(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    // Read all lines, keep only last MAX_HISTORY_LINES
    let lines: Vec<String> = reader
        .lines()
        .filter_map(|l| l.ok())  // Skip invalid UTF-8 lines
        .collect();

    let start = lines.len().saturating_sub(MAX_HISTORY_LINES);
    Ok(lines[start..].to_vec())
}
```

**Depends on**: `reedline` (trait), `dirs`.

---

#### `suggest.rs` — Autosuggestion Hinter

**Responsibility**: Provide inline ghost-text suggestions based on history prefix matching.

**Public API**:
```rust
pub struct HistoryHinter { /* private fields */ }

// Implements reedline::Hinter trait
```

**Internal concerns**: Most-recent-match selection, minimum prefix length before suggesting.

**Depends on**: `reedline` (trait), `history` module.

---

#### `complete.rs` — Completion Providers

**Responsibility**: Provide tab-completion candidates from multiple sources.

**Public API**:
```rust
pub struct WrashCompleter { /* private fields */ }

// Implements reedline::Completer trait

impl WrashCompleter {
    pub fn new() -> Self;
}
```

**Completion sources (internal)**:
- Filesystem: files and directories relative to cwd
- Commands: executables found in `$PATH`
- Git branches: when cwd is inside a git repository

**Depends on**: `reedline` (trait).

---

#### `prompt.rs` — Prompt Renderer

**Responsibility**: Render the shell prompt with context (cwd, exit code, duration).

**Public API**:
```rust
pub struct WrashPrompt { /* private fields */ }

impl WrashPrompt {
    pub fn new() -> Self;
    pub fn set_exit_code(&mut self, code: i32);
    pub fn set_duration(&mut self, duration: Duration);
}

// Implements reedline::Prompt trait
```

**Depends on**: `reedline` (trait).

---

#### `signals.rs` — Signal Handling

**Responsibility**: Convert Unix signals into events the main loop can process.

**Public API**:
```rust
pub struct SignalHandler { /* private fields */ }

impl SignalHandler {
    pub fn new() -> Result<Self>;
    pub fn signal_fd(&self) -> RawFd;   // for inclusion in poll()
    pub fn read_pending(&mut self) -> Option<SignalEvent>;
}

pub enum SignalEvent {
    Resize,              // SIGWINCH
    ChildExited,         // SIGCHLD
    Terminate,           // SIGTERM
    Hangup,              // SIGHUP - terminal disconnected
    Interrupt,           // SIGINT - when not in reedline
}
```

**Signal handling strategy:**

| Signal | When Received | Action |
|---|---|---|
| SIGWINCH | Any mode | Resize PTY, update chrome, notify reedline |
| SIGCHLD | Any mode | Check `try_wait()`, transition to Terminating if child exited |
| SIGTERM | Any mode | Initiate graceful shutdown |
| SIGHUP | Any mode | Terminal gone; send SIGHUP to child, exit immediately |
| SIGINT | Passthrough | Forward to child via PTY |
| SIGINT | Edit | Handled by reedline (clear line) |
| SIGINT | Initializing | Initiate shutdown |

**Internal concerns**: `signal-hook` pipe-based delivery, async-signal-safety.

**Depends on**: `signal-hook`, `nix`.

---

## 9. Error Handling Strategy

### Philosophy

Wrashpty is a personal tool, not a library. The error handling strategy optimizes for debuggability and safe recovery, not for fine-grained programmatic error discrimination.

### Crate-Level Approach

- **`anyhow::Result`** is the return type for all fallible functions in `app.rs`, `main.rs`, and module constructors. Context is added via `.context("descriptive message")` at every call site that could fail in non-obvious ways.
- **`thiserror`** is used in `marker.rs` for the parser's error type, because parser failures have a small, known set of variants that callers need to match on (e.g., "buffer overflow" vs. "invalid exit code in PRECMD payload").
- **Panics** are reserved for programming errors (logic bugs), never for runtime conditions. The only panics should be from `unreachable!()` in exhaustive matches.

### Recovery Semantics

| Failure | Severity | Response |
|---|---|---|
| PTY read returns EOF | Fatal | Child has exited. Clean up and exit. |
| PTY write fails | Fatal | Child is gone. Clean up and exit. |
| PTY read returns EIO | Fatal | PTY disconnected. Clean up and exit. |
| Marker parser encounters malformed sequence | Recoverable | Flush buffered bytes as output, log warning, continue. |
| Marker parser at EOF with partial buffer | Recoverable | Flush partial buffer as output, log debug message. |
| reedline returns ReedlineError::IO | Evaluate | Check if terminal still connected; if not, initiate shutdown. |
| reedline returns other errors | Recoverable | Log, clear line, re-enter Edit mode. |
| HISTFILE not found | Degraded | Proceed without history. Log once at startup. |
| HISTFILE parse error on line N | Degraded | Skip malformed lines, log count at end. Proceed with valid entries. |
| HISTFILE locked by another process | Degraded | Retry 3 times with 100ms delay, then proceed without history. |
| SIGWINCH handler fails to query size | Degraded | Skip resize. Log. |
| Git status command hangs | Degraded | Timeout after 200ms, show "git: ?" in footer. |
| Completion provider hangs | Degraded | Timeout after 100ms per provider, return partial results. |
| Panic anywhere | Terminal restoration must succeed | Panic hook + RAII guard. |
| SIGHUP received | Fatal | Clean shutdown: send SIGHUP to child, wait 1s, exit. |
| SIGTERM received | Fatal | Clean shutdown: send SIGTERM to child, wait 2s, exit. |
| Child killed by signal | Fatal | Report signal in exit code (128 + signal), clean up. |

### Detailed Recovery Semantics

#### Transient Errors (EAGAIN, EINTR)

System calls may return `EAGAIN` (would block) or `EINTR` (interrupted by signal). These are transient and should be retried:

```rust
fn read_with_retry(fd: RawFd, buf: &mut [u8]) -> Result<usize> {
    let mut retries = 0;
    loop {
        match nix::unistd::read(fd, buf) {
            Ok(n) => return Ok(n),
            Err(nix::errno::Errno::EAGAIN) |
            Err(nix::errno::Errno::EINTR) => {
                retries += 1;
                if retries > 3 {
                    // Log warning and continue in degraded mode
                    log::warn!("Repeated transient errors on fd {}", fd);
                    return Ok(0);  // Treat as no data available
                }
                // Brief sleep before retry to avoid busy loop
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => return Err(e.into()),
        }
    }
}
```

**Policy**: Retry up to 3 times with 1ms delay. If still failing, log and continue in degraded mode.

#### reedline Errors

reedline can return several error types:

| Error | Cause | Recovery |
|---|---|---|
| `ReedlineError::IO(e)` where `e.kind() == BrokenPipe` | Terminal disconnected | Initiate shutdown |
| `ReedlineError::IO(e)` where `e.kind() == Interrupted` | Signal during read | Retry (handled internally by reedline) |
| `ReedlineError::IO(e)` other | Unknown I/O error | Log, attempt to re-enter Edit mode |
| Other reedline errors | Internal reedline issue | Log, fall back to raw line input |

**Fallback to raw line input:**

```rust
fn fallback_read_line() -> Result<String> {
    // Simple line reading without reedline features
    let mut line = String::new();
    // Read one byte at a time until newline
    loop {
        let mut buf = [0u8; 1];
        match nix::unistd::read(STDIN_FILENO, &mut buf)? {
            0 => return Err(anyhow!("EOF")),
            1 => {
                if buf[0] == b'\n' {
                    return Ok(line);
                }
                if buf[0] >= 32 && buf[0] < 127 {
                    line.push(buf[0] as char);
                }
            }
            _ => unreachable!(),
        }
    }
}
```

This ensures the user can still enter commands even if reedline fails.

#### Security Errors (Invalid Session Token)

When the marker parser sees a well-formed OSC 777 marker with an invalid session token:

```rust
#[derive(Debug)]
enum SecurityEvent {
    InvalidToken { received: String, expected: String },
    TooManyInvalid { count: u32 },
}

impl MarkerParser {
    fn handle_invalid_token(&mut self, received: &str) {
        self.security_event_count += 1;

        // Log to file, NOT to terminal (could be part of attack)
        log::security("Invalid marker token: received {}, expected {}",
                      received, self.session_token);

        // Rate limiting: if too many invalid tokens, take action
        if self.security_event_count > 100 {
            log::error!("Security: >100 invalid tokens detected, possible attack");
            // Could emit a special event for the app to handle
            // (e.g., display warning, refuse to execute commands)
        }

        // Treat the marker as passthrough bytes, continue normally
    }
}
```

**Policy**:
- Invalid tokens are logged to file (not terminal)
- Marker is treated as passthrough output
- If >100 invalid tokens in a session, log a security warning
- Do NOT exit — that would be a denial-of-service vector

#### Terminal Query Failures

Terminal size queries can fail on exotic terminals or edge cases:

```rust
fn get_terminal_size() -> Result<(u16, u16)> {
    match TerminalGuard::get_size() {
        Ok(size) => Ok(size),
        Err(e) => {
            log::warn!("Failed to query terminal size: {}", e);

            // Try environment variables as fallback
            let cols = std::env::var("COLUMNS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(80);
            let rows = std::env::var("LINES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(24);

            log::info!("Using fallback terminal size: {}x{}", cols, rows);
            Ok((cols, rows))
        }
    }
}
```

**Policy**: On failure, use environment variables or default to 80x24. Log the issue but continue.

#### Child Process Unexpected Exit During Edit Mode

If the child Bash exits while reedline is blocking on input, the wrapper must detect this and initiate shutdown. This is challenging because reedline's `read_line()` blocks waiting for user input.

**Solution: Signal-Based Interruption via SIGCHLD**

reedline (via crossterm) uses a signal-safe event loop that can be interrupted by signals. The wrapper installs a SIGCHLD handler that sets a flag, and reedline's read will return with an interrupted error.

```rust
use std::sync::atomic::{AtomicBool, Ordering};

/// Global flag set by SIGCHLD handler
static CHILD_EXITED: AtomicBool = AtomicBool::new(false);

/// Install SIGCHLD handler before entering Edit mode
fn install_sigchld_handler() -> Result<()> {
    unsafe {
        signal_hook::low_level::register(signal_hook::consts::SIGCHLD, || {
            CHILD_EXITED.store(true, Ordering::SeqCst);
        })?;
    }
    Ok(())
}

/// Edit mode loop with child exit detection
fn run_edit_mode(&mut self) -> Result<EditModeResult> {
    // Clear flag before entering edit
    CHILD_EXITED.store(false, Ordering::SeqCst);

    loop {
        match self.editor.read_line() {
            Ok(Signal::Success(line)) => {
                return Ok(EditModeResult::Command(line));
            }
            Ok(Signal::CtrlC) => {
                // Clear line, continue editing
                continue;
            }
            Ok(Signal::CtrlD) => {
                return Ok(EditModeResult::Exit);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                // Signal interrupted the read - check if child exited
                if CHILD_EXITED.load(Ordering::SeqCst) {
                    // Verify child actually exited
                    if let Ok(Some(status)) = self.pty.try_wait() {
                        return Ok(EditModeResult::ChildExited(status));
                    }
                }
                // False alarm or other signal - continue editing
                continue;
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    }
}

pub enum EditModeResult {
    Command(String),
    Exit,
    ChildExited(ExitStatus),
}
```

**Key design points:**

1. **Atomic flag**: SIGCHLD handler only sets an atomic bool (async-signal-safe)
2. **Verification**: After interrupted read, verify child actually exited via `try_wait()`
3. **Graceful handling**: On child exit, return to main loop which initiates shutdown
4. **No threads**: Maintains single-threaded design using signal interruption

**Alternative approach (if reedline doesn't cooperate):**

If reedline doesn't properly return on signal interruption, use a dedicated monitoring thread that sends SIGINT to the main thread:

```rust
fn spawn_child_monitor(child_pid: Pid, main_thread: ThreadId) -> JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            match nix::sys::wait::waitpid(child_pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(_, _)) |
                Ok(WaitStatus::Signaled(_, _, _)) => {
                    // Child exited - send SIGINT to main thread to interrupt reedline
                    // Note: pthread_kill is not easily available in Rust
                    // Alternative: write to a self-pipe that's polled
                    break;
                }
                _ => continue,
            }
        }
    })
}
```

**Recommended approach for MVP**: Use the signal-based approach first. Test whether reedline returns `ErrorKind::Interrupted` on SIGCHLD. If not, fall back to the monitoring thread approach.

**Policy**: If child exits during Edit mode, abandon the current edit and initiate graceful shutdown.

### Error Classification

Errors are classified by whether they affect the PTY connection:

```rust
enum ErrorClass {
    /// PTY is gone, must shut down
    PtyDisconnected,
    /// Error in optional feature, continue with degraded functionality
    Degraded { feature: &'static str },
    /// Transient error, retry or ignore
    Transient,
    /// Bug in wrashpty, should not happen
    Internal,
}
```

### Buffer Overflow Prevention

The marker parser uses a fixed 80-byte buffer (legitimate markers with session tokens are ~45 bytes max). If an OSC sequence exceeds this:

1. The buffer is flushed as normal output
2. The parser returns to Normal state
3. A counter tracks "oversized sequences per minute"
4. If rate exceeds 10/minute, log a warning (possible attack or malformed program)

Additionally, a 100ms timeout prevents indefinite accumulation if a malformed sequence never terminates. See the marker parser section for implementation details.

---

## 10. Testing Strategy

### Unit Tests

**Marker parser**: This is the highest-value target for unit testing. Tests should cover:

- Complete markers in a single read.
- Markers split across two reads at every possible byte boundary.
- Markers embedded within normal output.
- Malformed sequences (truncated, oversized, wrong code).
- High-throughput: feed megabytes of non-marker data and verify byte-for-byte passthrough.
- Session token validation (accept matching, reject non-matching).
- Property-based tests with `proptest`: generate random byte streams with randomly inserted markers; verify all markers are detected and all non-marker bytes are preserved in order.

**Echo suppressor**: Test the byte-matching algorithm:
- Exact match suppression
- Partial match with timeout
- Mismatch recovery (previously suppressed bytes emitted)
- Multi-line command handling

**History indexing**: Prefix search correctness, deduplication, empty-file handling, malformed line recovery.

**Bashrc generation**: Verify the generated script is valid Bash (parse with `bash -n`). Check PROMPT_COMMAND chaining.

**Chrome layout**: Test `effective_rows()` calculation, wide character truncation, minimum size handling.

### Fuzz Testing

The marker parser processes untrusted input from the PTY. Fuzz testing is mandatory using **both** frameworks:

#### Framework 1: proptest (CI, stable Rust)

Property-based testing runs on every CI commit. Works with stable Rust.

```rust
// tests/marker_proptest.rs
use proptest::prelude::*;
use wrashpty::marker::{MarkerParser, ParseOutput};

proptest! {
    #[test]
    fn parser_never_loses_bytes(data: Vec<u8>) {
        let mut parser = MarkerParser::new_with_token([0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE]);
        let mut output_bytes = 0;
        let mut marker_count = 0;

        for chunk in data.chunks(64) {
            for output in parser.feed(chunk) {
                match output {
                    ParseOutput::Bytes(b) => output_bytes += b.len(),
                    ParseOutput::Marker(_) => marker_count += 1,
                }
            }
        }

        // All input bytes accounted for (either output or consumed as markers)
        prop_assert!(output_bytes <= data.len());
    }

    #[test]
    fn parser_handles_split_reads(data: Vec<u8>, split_points: Vec<usize>) {
        // Test that splitting input at arbitrary points produces same result
        // ... implementation
    }
}
```

#### Framework 2: cargo-fuzz (deep testing, nightly Rust)

Extended fuzzing campaigns for thorough coverage. Requires nightly Rust.

```rust
// fuzz/fuzz_targets/marker_parser.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use wrashpty::marker::{MarkerParser, ParseOutput};

fuzz_target!(|data: &[u8]| {
    let mut parser = MarkerParser::new_with_token([0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE]);
    let mut total_bytes = 0;
    let mut total_markers = 0;

    for chunk in data.chunks(64) {
        for output in parser.feed(chunk) {
            match output {
                ParseOutput::Bytes(b) => total_bytes += b.len(),
                ParseOutput::Marker(_) => total_markers += 1,
            }
        }
    }
});
```

**Fuzzing targets**:
- `marker_parser`: Core marker parsing
- `bashrc_generation`: Generated shell script validity
- `escape_sequence_handling`: Terminal escape sequence edge cases

**Usage**:
```bash
# proptest (CI)
cargo test marker_proptest

# cargo-fuzz (manual, deep testing)
cargo +nightly fuzz run marker_parser -- -max_total_time=3600
```

Run cargo-fuzz for at least 1 hour before release.

### Integration Test Framework

**Test dependencies** (add to `[dev-dependencies]` in Cargo.toml):

| Crate | Version | Purpose |
|-------|---------|---------|
| `rexpect` | 0.5 | Automated PTY-based testing (expect-style) |
| `assert_cmd` | 2.0 | Command-line integration testing |
| `proptest` | 1.0 | Property-based testing for fuzzing |
| `insta` | 1.0 | Snapshot testing for chrome layouts |

Use `rexpect` crate for automated PTY-based testing:

```rust
use rexpect::spawn;

#[test]
fn test_simple_command() {
    let mut p = spawn("cargo run --release", Some(5000)).unwrap();

    // Wait for prompt
    p.exp_regex(r"\$ ").unwrap();

    // Type command
    p.send_line("echo hello").unwrap();

    // Expect output
    p.exp_string("hello").unwrap();

    // Expect next prompt
    p.exp_regex(r"\$ ").unwrap();

    // Clean exit
    p.send_control('d').unwrap();
    p.exp_eof().unwrap();
}

#[test]
fn test_mode_transitions() {
    let mut p = spawn("cargo run --release", Some(5000)).unwrap();
    p.exp_regex(r"\$ ").unwrap();  // Edit mode

    p.send_line("cat").unwrap();    // Start command
    p.send_line("test line").unwrap();  // In passthrough (cat echoes)
    p.exp_string("test line").unwrap();
    p.send_control('d').unwrap();   // EOF to cat

    p.exp_regex(r"\$ ").unwrap();  // Back to Edit mode
}
```

### Chrome State Testing

Visual state is hard to test automatically. Use snapshot testing:

```rust
#[test]
fn test_chrome_layout() {
    let chrome = Chrome::new();
    let mut buffer = Vec::new();

    chrome.draw_to_buffer(&mut buffer, 80, 24);

    // Compare to known-good snapshot
    insta::assert_snapshot!(String::from_utf8_lossy(&buffer));
}
```

### Integration Tests

**PTY spawn**: Spawn a simple shell script on a PTY, verify output arrives correctly.

**Resize propagation**: Spawn a child, send TIOCSWINSZ, verify the child receives SIGWINCH.

**Full cycle**: Spawn Bash with the generated rcfile, inject a command, verify PRECMD/PROMPT/PREEXEC markers arrive in order.

**State machine coverage**: Use the rexpect framework to exercise all mode transitions:
- Initializing → Edit
- Edit → Injecting → Passthrough
- Passthrough → Edit
- Any → Terminating → exit

**Chrome integration**: Test scroll region behavior with a program that queries terminal size.

### Manual Testing Protocol

Wrashpty is a terminal program. Some behaviors can only be validated interactively:

1. **Full-screen apps**: vim, htop, less, man, tmux (nested multiplexer).
2. **Interactive REPLs**: python3, node, irb.
3. **Job control**: Run `sleep 100`, press Ctrl+Z, run `fg`.
4. **Signal handling**: Ctrl+C during a long-running command. Ctrl+C at the prompt.
5. **Terminal resize**: Resize the window while in Edit mode and while in Passthrough mode.
6. **Rapid input**: Paste a large block of text. Hold down a key.
7. **SSH**: Run `ssh localhost` through the wrapper. Verify nested terminal works.
8. **Throughput**: `time cat /dev/urandom | head -c 100M > /dev/null` compared to raw Bash.
9. **Chrome toggle**: Toggle chrome on/off while editing and while in passthrough. Verify bars appear/disappear and child programs resize correctly.
10. **Chrome with full-screen apps**: Run vim with chrome active. Verify vim uses the correct (smaller) viewport and bars are preserved.
11. **Alternate screen modals**: Trigger a picker (if implemented). Verify main screen state is preserved on return.
12. **Nested wrashpty**: Run `wrashpty` inside `wrashpty`. Verify both instances work independently.
13. **Shell escape**: Run `exec zsh` and verify passthrough continues working.
14. **SIGHUP**: Kill the terminal emulator window. Verify wrashpty exits cleanly and child receives SIGHUP.

---

## 11. Performance Considerations

### Passthrough Latency

Every byte of command output passes through the marker parser. The parser is O(n) with no allocation and a minimal branch per byte (check for ESC). On modern hardware, this adds negligible latency. The dominant cost is the syscall overhead of `read()` and `write()`. Using 8KB buffers amortizes this effectively.

### Keystroke Latency

In Edit mode, reedline handles key events. Autosuggestions require a history lookup per keystroke. With an in-memory `Vec<String>` of history entries and a linear prefix scan, this is fast for typical history sizes (< 50K entries).

**For larger histories (50K+ entries):**
- Build a prefix trie on first load (O(n) construction, O(k) lookup where k = prefix length)
- Use binary search on sorted entries for prefix queries
- Lazy-load history: load last 10K entries immediately, rest in background

### Completion Latency

Completion providers have individual timeouts to prevent UI freezes:

| Provider | Timeout | Fallback |
|---|---|---|
| Filesystem | 100ms per directory | Skip slow directories |
| PATH executables | 100ms total | Return partial results |
| Git branches | 100ms | Skip git completions |

Providers run sequentially (not parallel) to avoid overwhelming slow filesystems.

### Git Status Latency

Git status is computed asynchronously to never block the prompt:

1. On directory change, spawn background thread for git status
2. Return cached/stale value for immediate display
3. When background thread completes, update cache
4. Force refresh on explicit keybinding (e.g., Ctrl+G)

**Timeout**: Individual git commands timeout after 200ms. On timeout, display "git: ?" in footer.

### Startup Time

The wrapper must spawn Bash, generate a tempfile, and wait for the first PROMPT marker. This adds ~50-100ms to shell startup. For an interactive tool launched once per terminal session, this is acceptable.

**Startup timeout**: If the first PROMPT marker doesn't arrive within 10 seconds, log a warning and fall back to passthrough-only mode. This handles cases where the user's bashrc has long-running initialization.

---

## 12. Configuration

### CLI Arguments

```
wrashpty [OPTIONS]

Options:
  --no-chrome             Start with chrome disabled
  --shell <PATH>          Path to shell binary (default: bash)
  --histfile <PATH>       Path to history file (default: ~/.bash_history)
  --preexec-timeout <MS>  PREEXEC marker timeout in ms (default: 500, max: 3000)
  --vi                    Use vi keybinding mode
  --emacs                 Use emacs keybinding mode (default)
  -c <COMMAND>            Execute command and exit
  -h, --help              Print help
  -V, --version           Print version
```

### Environment Variables

| Variable | Effect |
|---|---|
| `WRASH_CHROME` | Set to "0" to disable chrome by default |
| `WRASH_LOG` | Set to "debug", "info", "warn", or "error" for log level |
| `WRASH_LOG_FILE` | Path to log file (default: no file logging) |
| `SHELL` | Fallback shell if --shell not specified and bash not found |

### Default Keybindings

| Key | Mode | Action |
|---|---|---|
| Ctrl+Shift+H | Any | Toggle chrome |
| Ctrl+R | Edit | History search |
| Tab | Edit | Trigger completion |
| Ctrl+C | Edit | Clear line |
| Ctrl+D | Edit (empty line) | Exit |
| Ctrl+L | Edit | Clear screen (within scroll region if chrome active) |
| Ctrl+Shift+L | Edit | Flush buffered background output above prompt |
| Up/Down | Edit | History navigation |

Keybindings are not configurable in MVP. Future versions may add a config file.

---

## 13. Dependency Management

### Version Pinning Strategy

Critical dependencies are pinned to exact versions in `Cargo.toml`:

```toml
[dependencies]
reedline = "=0.28.0"      # Exact version, API-critical
crossterm = "=0.27.0"     # Must match reedline's crossterm
portable-pty = "=0.8.1"   # Platform-critical
nix = "0.27"              # SemVer, breaking changes rare
signal-hook = "0.3"       # SemVer, stable API
```

### Crossterm Version Alignment

Both wrashpty and reedline depend on crossterm. Version mismatch can cause:
- Terminal state conflicts (two raw mode managers)
- Incompatible event types
- Runtime panics from version mismatches

**Solution**: Check reedline's crossterm version before release:

```bash
cargo tree -i crossterm
# Verify single version in tree
```

### Reedline Compatibility

Reedline is central to Edit mode. Breaking changes require coordinated updates:

1. Pin to exact version
2. Test thoroughly before upgrading
3. Monitor reedline changelog for breaking changes
4. Have fallback: if reedline update breaks things, can revert and wait

### Minimum Rust Version

MSRV is the latest stable Rust at time of release. No attempt to support older compilers — this is a personal tool, not a library.

---

## 14. Platform Compatibility

### Linux (Primary Target)

Fully supported. Uses:
- `/dev/ptmx` for PTY allocation
- `ioctl` for terminal control
- `poll` for I/O multiplexing
- `/proc/self/fd` for fd introspection (optional)

### macOS (Best-Effort)

**Status**: Best-effort support. Not blocking for MVP, but PRs welcome.

Mostly compatible. Differences:
- PTY allocation via `posix_openpt()`
- Signal handling semantics slightly different
- Ships with Bash 3.2 (GPLv2), need Bash 4+ for full functionality

**Critical**: macOS users must install Bash 4.0+ via Homebrew (`brew install bash`). The startup version check (see "Bash Version Validation" above) will fail fast with a helpful error message including Homebrew installation instructions.

### Non-Standard Bash Configurations

Handle gracefully:
- `bash --posix`: Some features disabled, but markers should work
- `bash --norc`: Our rcfile is still loaded via `--rcfile`
- `bash --noprofile`: Login shell behavior, should work
- `set -e` in bashrc: Guard marker functions against errors

### Encoding

Assume UTF-8 unless `$LC_ALL`, `$LC_CTYPE`, or `$LANG` indicate otherwise.

**Non-UTF8 handling:**
- Marker parser operates on bytes, not characters — no encoding issues
- Chrome content uses UTF-8 only; non-UTF8 cwd is escaped or replaced with "?"
- History may contain non-UTF8; treat as opaque bytes for search

---

## 15. Rust Implementation Patterns

This section documents recommended Rust patterns for implementing wrashpty correctly and idiomatically.

### Builder Pattern for Complex Configuration

Use the builder pattern for `PtyConfig` and `EditorConfig` to provide ergonomic construction with validation:

```rust
pub struct PtyConfig {
    shell_path: PathBuf,
    rcfile_path: PathBuf,
    initial_size: (u16, u16),
    env_vars: HashMap<String, String>,
}

impl PtyConfig {
    pub fn builder() -> PtyConfigBuilder {
        PtyConfigBuilder::default()
    }
}

#[derive(Default)]
pub struct PtyConfigBuilder {
    shell_path: Option<PathBuf>,
    rcfile_path: Option<PathBuf>,
    initial_size: (u16, u16),
    env_vars: HashMap<String, String>,
}

impl PtyConfigBuilder {
    pub fn shell_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.shell_path = Some(path.into());
        self
    }

    pub fn rcfile(mut self, path: impl Into<PathBuf>) -> Self {
        self.rcfile_path = Some(path.into());
        self
    }

    pub fn initial_size(mut self, cols: u16, rows: u16) -> Self {
        self.initial_size = (cols, rows);
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> Result<PtyConfig> {
        Ok(PtyConfig {
            shell_path: self.shell_path
                .ok_or_else(|| anyhow!("shell_path required"))?,
            rcfile_path: self.rcfile_path
                .ok_or_else(|| anyhow!("rcfile_path required"))?,
            initial_size: self.initial_size,
            env_vars: self.env_vars,
        })
    }
}

// Usage:
let config = PtyConfig::builder()
    .shell_path("/bin/bash")
    .rcfile(rcfile.path())
    .initial_size(80, 24)
    .build()?;
let pty = PtyHandle::spawn(config)?;
```

**Benefits:**
- Required fields are validated at build time
- Optional fields have sensible defaults
- Fluent API is easy to read and extend
- Adding new optional fields doesn't break existing code

### Typestate Pattern for State Machine Safety

Use Rust's type system to make illegal state transitions unrepresentable:

```rust
// Instead of runtime checks for mode:
struct App {
    mode: Mode,
    editor: Option<EditorHandle>,  // Only Some in Edit mode - runtime check
    pump: Option<Pump>,            // Only Some in Passthrough - runtime check
}

// Use typestate pattern:
pub struct App<S: AppState> {
    pty: PtyHandle,
    terminal: TerminalGuard,
    marker_parser: MarkerParser,
    chrome: Chrome,
    state: S,
}

pub trait AppState {}

pub struct Initializing {
    timeout_deadline: Instant,
}
impl AppState for Initializing {}

pub struct Edit {
    editor: EditorHandle,
    last_exit_code: i32,
    last_duration: Duration,
}
impl AppState for Edit {}

pub struct Passthrough {
    pump: Pump,
    command_start: Instant,
}
impl AppState for Passthrough {}

pub struct Injecting {
    pending_line: String,
    output_buffer: Vec<u8>,
    deadline: Instant,
}
impl AppState for Injecting {}

// Transitions are type-safe:
impl App<Initializing> {
    pub fn wait_for_prompt(self) -> Result<InitResult> {
        // ... polling logic ...
        match event {
            MarkerEvent::Prompt => {
                let editor = EditorHandle::new()?;
                Ok(InitResult::Ready(App {
                    pty: self.pty,
                    terminal: self.terminal,
                    marker_parser: self.marker_parser,
                    chrome: self.chrome,
                    state: Edit {
                        editor,
                        last_exit_code: 0,
                        last_duration: Duration::ZERO,
                    },
                }))
            }
            _ => Ok(InitResult::Continue(self)),
        }
    }
}

impl App<Edit> {
    pub fn read_command(self) -> Result<EditResult> {
        match self.state.editor.read_line()? {
            EditorResult::Command(line) => {
                let pump = Pump::new(self.pty.master_fd());
                Ok(EditResult::Execute(App {
                    pty: self.pty,
                    terminal: self.terminal,
                    marker_parser: self.marker_parser,
                    chrome: self.chrome,
                    state: Injecting {
                        pending_line: line,
                        output_buffer: Vec::new(),
                        deadline: Instant::now() + Duration::from_millis(500),
                    },
                }))
            }
            EditorResult::Exit => Ok(EditResult::Exit),
            EditorResult::Interrupted => Ok(EditResult::Continue(self)),
        }
    }
}

// Compiler enforces: can't call edit methods on Passthrough, etc.
```

**Trade-off:** More complex types, but impossible to access `editor` in Passthrough mode. For terminal safety, this is worth it.

**Simpler alternative** if typestate is too complex:

```rust
enum AppState {
    Initializing(InitializingState),
    Edit(EditState),
    Passthrough(PassthroughState),
    Injecting(InjectingState),
}

struct App {
    pty: PtyHandle,
    terminal: TerminalGuard,
    state: AppState,
}

impl App {
    fn editor(&mut self) -> Result<&mut EditorHandle> {
        match &mut self.state {
            AppState::Edit(s) => Ok(&mut s.editor),
            _ => Err(anyhow!("Not in Edit mode")),
        }
    }
}
```

### Comprehensive Logging Strategy

**Critical:** Never log to stdout/stderr — we control the terminal. Always log to a file.

```rust
use tracing::{debug, info, warn, error, instrument, span, Level};
use tracing_subscriber::prelude::*;
use std::fs::File;

fn setup_logging() -> Result<()> {
    // Determine log path
    let log_path = std::env::var("WRASH_LOG_FILE")
        .unwrap_or_else(|_| "/tmp/wrashpty.log".into());

    let log_level = std::env::var("WRASH_LOG")
        .unwrap_or_else(|_| "info".into());

    let level = match log_level.as_str() {
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };

    let file = File::create(&log_path)
        .context("Failed to create log file")?;

    let subscriber = tracing_subscriber::fmt()
        .with_writer(file)
        .with_max_level(level)
        .with_ansi(false)  // No ANSI in log files
        .with_target(true)
        .with_thread_ids(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    info!(path = %log_path, "Logging initialized");
    Ok(())
}

// Usage in modules:
#[instrument(skip(self), fields(mode = ?self.mode))]
fn handle_marker(&mut self, marker: MarkerEvent) -> Result<()> {
    debug!(?marker, "Received marker");

    match (&self.mode, &marker) {
        (Mode::Initializing, MarkerEvent::Prompt) => {
            info!("First prompt received, entering Edit mode");
            self.transition_to_edit()?;
        }
        (Mode::Passthrough, MarkerEvent::Prompt) => {
            let duration = self.command_start.elapsed();
            info!(?duration, exit_code = self.last_exit_code, "Command finished");
            self.transition_to_edit()?;
        }
        (mode, marker) => {
            warn!(?mode, ?marker, "Unexpected marker in current mode");
        }
    }
    Ok(())
}

// Security events get special treatment:
fn log_security_event(event: &SecurityEvent) {
    // Security events always logged, regardless of level
    error!(target: "security", ?event, "Security event detected");
}
```

**Log levels:**
- `ERROR`: Unrecoverable errors, security events
- `WARN`: Unexpected states, timeouts, degraded mode
- `INFO`: Mode transitions, startup/shutdown, command execution
- `DEBUG`: Marker parsing, byte counts, state machine details

### Integration Test Harness

Use `rexpect` for automated PTY-based testing:

```rust
// tests/integration.rs
use rexpect::spawn;
use std::time::Duration;

const TIMEOUT_MS: u64 = 5000;

fn spawn_wrashpty() -> rexpect::PtySession {
    spawn("./target/debug/wrashpty", Some(TIMEOUT_MS)).unwrap()
}

#[test]
fn test_simple_command() {
    let mut p = spawn_wrashpty();

    // Wait for prompt
    p.exp_regex(r"\$ ").unwrap();

    // Send command
    p.send_line("echo hello").unwrap();

    // Expect output
    p.exp_string("hello").unwrap();

    // Expect next prompt
    p.exp_regex(r"\$ ").unwrap();

    // Exit
    p.send_line("exit").unwrap();
    p.exp_eof().unwrap();
}

#[test]
fn test_vim_passthrough() {
    let mut p = spawn_wrashpty();
    p.exp_regex(r"\$ ").unwrap();

    // Start vim with a temp file
    p.send_line("vim /tmp/wrashpty_test.txt").unwrap();

    // Wait for vim to start (look for status line)
    std::thread::sleep(Duration::from_millis(500));

    // Enter insert mode
    p.send("i").unwrap();

    // Type text
    p.send("Hello from vim").unwrap();

    // Exit insert mode and save
    p.send("\x1b:wq\n").unwrap();

    // Back to shell prompt
    p.exp_regex(r"\$ ").unwrap();

    // Verify file was written
    p.send_line("cat /tmp/wrashpty_test.txt").unwrap();
    p.exp_string("Hello from vim").unwrap();
}

#[test]
fn test_ctrl_z_job_control() {
    let mut p = spawn_wrashpty();
    p.exp_regex(r"\$ ").unwrap();

    // Start long-running command
    p.send_line("sleep 100").unwrap();

    // Wait a bit
    std::thread::sleep(Duration::from_millis(500));

    // Send Ctrl+Z
    p.send("\x1a").unwrap();

    // Expect job suspended message
    p.exp_regex(r"\[1\].*Stopped").unwrap();

    // Expect prompt back
    p.exp_regex(r"\$ ").unwrap();

    // Resume job
    p.send_line("fg").unwrap();

    // Kill it with Ctrl+C
    std::thread::sleep(Duration::from_millis(200));
    p.send("\x03").unwrap();

    // Expect prompt
    p.exp_regex(r"\$ ").unwrap();
}

#[test]
fn test_resize_during_edit() {
    let mut p = spawn_wrashpty();
    p.exp_regex(r"\$ ").unwrap();

    // Type partial command
    p.send("echo te").unwrap();

    // Simulate resize (this is tricky in tests - may need ioctl)
    // For now, just verify the partial input survives
    p.send("st\n").unwrap();
    p.exp_string("test").unwrap();
}

#[test]
fn test_marker_spoofing_rejected() {
    let mut p = spawn_wrashpty();
    p.exp_regex(r"\$ ").unwrap();

    // Try to inject a fake marker
    p.send_line("printf '\\033]777;fake;PROMPT\\a'").unwrap();

    // Should still get normal prompt, not be confused
    p.exp_regex(r"\$ ").unwrap();

    // Verify we can still run commands
    p.send_line("echo works").unwrap();
    p.exp_string("works").unwrap();
}
```

### Graceful Degradation Patterns

When features fail, fall back gracefully:

```rust
impl App {
    fn try_enter_edit_mode(&mut self) -> Result<()> {
        match EditorHandle::new() {
            Ok(editor) => {
                self.state = AppState::Edit(EditState { editor, .. });
                info!("Entered Edit mode with full editor");
                Ok(())
            }
            Err(e) => {
                warn!("Failed to initialize editor: {}. Using fallback.", e);
                self.state = AppState::RawInput(RawInputState::new());
                Ok(())
            }
        }
    }
}

// RawInput mode: simple line reading without reedline
struct RawInputState {
    buffer: String,
}

impl RawInputState {
    fn read_line(&mut self) -> Result<String> {
        // Minimal prompt
        write!(io::stdout(), "$ ")?;
        io::stdout().flush()?;

        self.buffer.clear();
        loop {
            let mut byte = [0u8; 1];
            match nix::unistd::read(STDIN_FILENO, &mut byte)? {
                0 => return Err(anyhow!("EOF")),
                1 => {
                    let ch = byte[0];
                    if ch == b'\n' || ch == b'\r' {
                        writeln!(io::stdout())?;
                        return Ok(std::mem::take(&mut self.buffer));
                    } else if ch == 0x7f || ch == 0x08 {
                        // Backspace
                        if self.buffer.pop().is_some() {
                            write!(io::stdout(), "\x08 \x08")?;
                            io::stdout().flush()?;
                        }
                    } else if ch >= 32 && ch < 127 {
                        self.buffer.push(ch as char);
                        write!(io::stdout(), "{}", ch as char)?;
                        io::stdout().flush()?;
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

// Degradation levels:
enum EditorCapability {
    Full,           // reedline with all features
    NoHistory,      // reedline without history
    NoCompletions,  // reedline without completions
    RawInput,       // Fallback line input
}

impl App {
    fn initialize_editor(&mut self) -> EditorCapability {
        // Try full editor
        let history = match HistoryHandle::load() {
            Ok(h) => Some(h),
            Err(e) => {
                warn!("History unavailable: {}", e);
                None
            }
        };

        let completer = match WrashCompleter::new() {
            Ok(c) => Some(Box::new(c) as Box<dyn Completer>),
            Err(e) => {
                warn!("Completions unavailable: {}", e);
                None
            }
        };

        match EditorHandle::new_with_features(history, completer) {
            Ok(editor) => {
                if history.is_some() && completer.is_some() {
                    EditorCapability::Full
                } else if history.is_some() {
                    EditorCapability::NoCompletions
                } else if completer.is_some() {
                    EditorCapability::NoHistory
                } else {
                    EditorCapability::NoHistory
                }
            }
            Err(e) => {
                warn!("Editor unavailable, using raw input: {}", e);
                EditorCapability::RawInput
            }
        }
    }
}
```

**Principle:** Never let a feature failure crash the wrapper. Degrade gracefully and inform the user via logging.

### Hot Path Optimization

The passthrough byte pump is the hottest path. Optimize carefully:

```rust
// Cargo.toml
[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1  # Better optimization, slower compile
panic = "abort"    # Smaller binary, no unwinding overhead

// In pump.rs:
const READ_BUF_SIZE: usize = 8192;  // Tune based on profiling

pub struct Pump {
    pty_fd: RawFd,
    buf: [u8; READ_BUF_SIZE],  // Reuse buffer, avoid allocations
    marker_parser: MarkerParser,
}

impl Pump {
    #[inline]
    pub fn poll_once(&mut self) -> Result<PumpEvent> {
        // Read directly into reusable buffer
        let n = nix::unistd::read(self.pty_fd, &mut self.buf)?;
        if n == 0 {
            return Ok(PumpEvent::ChildExited);
        }

        // Process in place, avoid copying
        for output in self.marker_parser.feed(&self.buf[..n]) {
            match output {
                ParseOutput::Bytes(bytes) => {
                    // Write directly to stdout fd, bypassing BufWriter
                    nix::unistd::write(STDOUT_FILENO, bytes)?;
                }
                ParseOutput::Marker(m) => {
                    return Ok(PumpEvent::Marker(m));
                }
            }
        }

        Ok(PumpEvent::Continue)
    }
}

// Benchmarks (benches/pump.rs):
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_passthrough_throughput(c: &mut Criterion) {
    // Create a pipe to simulate PTY
    let (read_fd, write_fd) = nix::unistd::pipe().unwrap();

    // Pre-fill with test data
    let test_data = vec![b'x'; 8192];

    c.bench_function("passthrough_8k", |b| {
        b.iter(|| {
            // Write test data
            nix::unistd::write(write_fd, &test_data).unwrap();

            // Read and process
            let mut buf = [0u8; 8192];
            let n = nix::unistd::read(read_fd, &mut buf).unwrap();
            black_box(n);
        });
    });
}

fn bench_marker_parsing(c: &mut Criterion) {
    let mut parser = MarkerParser::new(SessionToken::generate());
    let marker_input = b"\x1b]777;abcdef0123456789;PRECMD;0\x07";

    c.bench_function("parse_precmd_marker", |b| {
        b.iter(|| {
            for output in parser.feed(black_box(marker_input)) {
                black_box(output);
            }
        });
    });
}

criterion_group!(benches, bench_passthrough_throughput, bench_marker_parsing);
criterion_main!(benches);
```

**Performance target:** `cat /dev/urandom | head -c 100M > /dev/null` should complete in under 2x the time of raw Bash.

### Unsafe Code Boundaries

Minimize `unsafe` and document all uses:

```rust
// Rule 1: Never use unsafe unless absolutely necessary
// Rule 2: Isolate unsafe in small, well-documented functions
// Rule 3: Provide safe wrappers

// Example: If raw fd manipulation is needed
mod raw_fd_ops {
    use std::os::unix::io::RawFd;

    /// Close a file descriptor.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// 1. `fd` is a valid, open file descriptor
    /// 2. `fd` is not closed elsewhere (no double-close)
    /// 3. No other code holds references to this fd
    pub unsafe fn close_fd(fd: RawFd) -> std::io::Result<()> {
        if libc::close(fd) == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

// Safe wrapper:
pub struct OwnedFd {
    fd: RawFd,
    closed: bool,
}

impl OwnedFd {
    pub fn new(fd: RawFd) -> Self {
        Self { fd, closed: false }
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for OwnedFd {
    fn drop(&mut self) {
        if !self.closed && self.fd >= 0 {
            // SAFETY: We own this fd, it's valid, and Drop only runs once
            unsafe {
                let _ = raw_fd_ops::close_fd(self.fd);
            }
            self.closed = true;
        }
    }
}

// Prefer safe abstractions:
// - Use `portable-pty` instead of raw PTY syscalls
// - Use `nix` instead of raw libc calls
// - Use `crossterm` instead of raw terminal escape sequences
```

---

## 16. Risk Assessment and Implementation Timeline

### Risk Matrix

| Component | Risk Level | Impact if Fails | Mitigation Status |
|---|---|---|---|
| Echo suppression | HIGH | Visible command echo | ✅ Fixed: termios ECHO flag |
| SIGWINCH handling | HIGH | Corrupted display | ✅ Fixed: mode-aware handling |
| Marker parser overflow | HIGH | DoS attack | ✅ Fixed: 64-byte limit + timeout |
| Injecting deadlock | MEDIUM | Wrapper hangs | ✅ Fixed: drain PTY during injection |
| Chrome scroll regions | MEDIUM | Display corruption | ✅ Fixed: disable in Passthrough |
| Error recovery | MEDIUM | Crashes | ✅ Fixed: explicit recovery semantics |
| Module dependencies | LOW | Won't compile | ✅ Fixed: shared types module |
| reedline integration | LOW | No editing features | Standard pattern, well-tested |

### Implementation Priority

**Phase 1: Foundation (Week 1-2)**
- [ ] Project setup with module structure
- [ ] Shared types module (`types.rs`, `error.rs`)
- [ ] Marker parser with session token validation
- [ ] Unit tests for marker parser

**Phase 2: Platform Layer (Week 3-4)**
- [ ] PTY management (`pty.rs`)
- [ ] Terminal control (`terminal.rs`)
- [ ] Signal handling (`signals.rs`)
- [ ] Bashrc generation (`bashrc.rs`)
- [ ] Integration tests for PTY spawn

**Phase 3: Protocol Layer (Week 5-6)**
- [ ] Byte pump (`pump.rs`)
- [ ] Echo prevention via termios
- [ ] Injecting mode with deadlock prevention
- [ ] Mode-aware SIGWINCH handling

**Phase 4: Editor Integration (Week 7-8)**
- [ ] reedline integration (`editor.rs`)
- [ ] History provider (`history.rs`)
- [ ] Autosuggestion hinter (`suggest.rs`)
- [ ] Completion providers (`complete.rs`)

**Phase 5: Orchestration (Week 9-10)**
- [ ] App state machine (`app.rs`)
- [ ] Mode transitions
- [ ] Graceful degradation
- [ ] Comprehensive logging

**Phase 6: Chrome Layer (Week 11-12)**
- [ ] Top bar and footer (`chrome.rs`)
- [ ] Git status caching
- [ ] Toggle functionality
- [ ] Scroll region management

**Phase 7: Polish (Week 13-14)**
- [ ] Integration test suite
- [ ] Manual testing protocol
- [ ] Performance benchmarks
- [ ] Documentation

### Testing Milestones

Before each phase is complete, verify:

1. **After Phase 2**: Can spawn Bash, read output, send input
2. **After Phase 3**: Markers detected, mode transitions work
3. **After Phase 4**: Can edit commands with history/completion
4. **After Phase 5**: Full command cycle works end-to-end
5. **After Phase 6**: Chrome renders correctly, toggles work

### Manual Testing Protocol

Before MVP release, manually verify:

- [ ] Simple commands: `ls`, `echo`, `cat`
- [ ] Full-screen apps: vim, htop, less, man
- [ ] Interactive REPLs: python3, node, irb
- [ ] Job control: Ctrl+Z, fg, bg, jobs
- [ ] Signal handling: Ctrl+C during command, Ctrl+C at prompt
- [ ] Terminal resize: during Edit, during Passthrough
- [ ] SSH: `ssh localhost` through wrapper
- [ ] Nested wrapper: `wrashpty` inside `wrashpty`
- [ ] Shell replacement: `exec zsh` inside wrapper
- [ ] Chrome toggle: on/off during Edit and Passthrough
- [ ] Panic recovery: inject panic, verify terminal restored

---

## 17. Future Extension Points

The architecture is designed to permit (but not require) these future additions:

- **Bash-native completions**: Inject `compgen` queries to the child Bash via a side channel (a second PTY or a named pipe) to access Bash's programmable completion system.
- **Syntax highlighting**: reedline supports the `Highlighter` trait. A simple highlighter that colors known commands (from PATH) vs. unknown commands can be added without architectural changes.
- **Configuration file**: A TOML config loaded at startup to control keybindings, prompt format, completion sources, and enabled features.
- **Multi-shell backend**: The shell integration protocol is generic. A Zsh or Fish backend would require a different rcfile generator but the same wrapper architecture.
- **Plugin system**: Completion providers and hinters implement reedline traits, which are already trait-object-compatible. A dynamic loading system (via `libloading` or WASM) could load third-party providers.
