# Wrashpty — Scenarios and Solutions

This document catalogs every significant interaction scenario the wrapper must handle, including the happy path, edge cases, failure modes, and the design decisions that resolve each.

---

## 1. Core Interaction Scenarios

### 1.1 Simple Command Execution

**Scenario**: User types `ls -la` and presses Enter.

**Flow**:

1. Wrapper is in Edit mode. reedline displays the prompt and accepts input.
2. User types `ls -la`. As they type, the hinter shows a ghost-text suggestion if a history entry matches the prefix.
3. User presses Enter. reedline returns `Signal::Success("ls -la")`.
4. Wrapper writes `ls -la\n` to PTY master.
5. Bash receives the line via PTY slave stdin.
6. Bash's DEBUG trap fires → emits `PREEXEC` marker.
7. Wrapper detects `PREEXEC` in PTY output → transitions to Passthrough mode.
8. Bash executes `ls`, which writes output to the PTY slave stdout.
9. Wrapper reads output from PTY master, scans for markers (none expected), writes to real stdout.
10. `ls` exits. Bash's `PROMPT_COMMAND` runs → emits `PRECMD;0` marker.
11. Wrapper detects `PRECMD` → stores exit code 0.
12. Bash evaluates `PS1` → emits `PROMPT` marker.
13. Wrapper detects `PROMPT` → transitions to Edit mode with `last_exit_code = 0`.
14. reedline displays the prompt. Cycle repeats.

**What could go wrong**:

- The `PREEXEC` marker could be delayed if Bash buffers output. Mitigation: Bash flushes `printf` to a PTY (which is line-buffered by default), so markers appear promptly.
- The echo of `ls -la` appears in PTY output. Mitigation: The wrapper uses an `EchoGuard` RAII pattern that disables the PTY's `ECHO` flag in termios before injecting the command. The guard automatically re-enables ECHO when dropped (after receiving `PREEXEC` or on timeout/panic). This prevents the kernel's line discipline from echoing the command at all, eliminating the race condition between marker arrival and echo suppression.
- The wrapper panics between disabling ECHO and receiving PREEXEC. Mitigation: The `EchoGuard` implements `Drop` which restores ECHO even during unwinding, ensuring the terminal is never left with ECHO disabled.

---

### 1.2 Full-Screen Application (vim, htop)

**Scenario**: User types `vim file.txt`.

**Flow**:

1–7. Same as Simple Command Execution through `PREEXEC` detection.

8. Wrapper is now in Passthrough mode. Vim starts, sends escape sequences to configure the terminal (alternate screen buffer, cursor shape, mouse mode, etc.).
9. Wrapper forwards all bytes from PTY master to real stdout without interpretation. Vim's UI renders normally.
10. User interacts with Vim. Keystrokes from real stdin are forwarded to PTY master. Vim processes them.
11. User quits Vim (`:q`). Vim restores the terminal (exits alternate screen, resets cursor).
12. Bash regains control → `PRECMD;0` → `PROMPT` markers arrive.
13. Wrapper transitions to Edit mode.

**Critical requirement**: In Passthrough mode, the wrapper must not interpret, buffer, or modify any bytes except for marker scanning. If the wrapper tried to parse ANSI escape sequences, Vim's output would be corrupted.

**What could go wrong**:

- Vim (or any program) could output the exact bytes of a marker by coincidence. Mitigation: The OSC 777 sequence is long enough (minimum 14 bytes: `ESC ] 7 7 7 ; P R O M P T BEL`) that coincidental matches are astronomically unlikely. The semicolon-delimited structure adds further uniqueness.
- Vim could set terminal modes that conflict with the wrapper's expectations when it resumes Edit mode. Mitigation: On transition to Edit mode, the wrapper always re-enters raw mode from scratch, not relying on the previous state.

---

### 1.3 Interactive REPL (python3, node)

**Scenario**: User types `python3`. The Python REPL starts.

**Flow**:

1–7. Same through `PREEXEC`.

8. Python starts and takes over stdin/stdout on the PTY slave. It presents its own prompt (`>>> `).
9. Wrapper is in Passthrough. All Python interaction is forwarded transparently.
10. User types `exit()` or Ctrl+D in Python. Python exits.
11. Bash regains control → markers → Edit mode.

**Why this works**: The REPL runs as a child of Bash, which runs on the PTY slave. The wrapper doesn't know or care that a REPL is running; it just forwards bytes.

**What could go wrong**:

- The Python REPL could use Readline internally (it often does). This is fine because Readline operates on the PTY slave side, not the real terminal. There is no conflict with the wrapper's editor because the wrapper is in Passthrough mode.

---

### 1.4 Job Control: Ctrl+Z and fg

**Scenario**: User runs `sleep 100`, presses Ctrl+Z, then runs `fg`.

**Flow**:

1–7. `sleep 100` is dispatched. Wrapper enters Passthrough.
8. User presses Ctrl+Z. In Passthrough, this keystroke is forwarded to the PTY master.
9. The PTY line discipline translates Ctrl+Z into `SIGTSTP`, delivered to the foreground process group (which is `sleep`'s group, managed by Bash's job control).
10. `sleep` is stopped. Bash receives `SIGCHLD`, updates its job table.
11. Bash's prompt cycle runs → `PRECMD;148` (exit code 148 = 128 + SIGTSTP signal number 20) → `PROMPT`.
12. Wrapper detects markers → Edit mode.
13. User types `fg`. Command is injected.
14. Bash resumes `sleep` in the foreground → `PREEXEC` → Passthrough.
15. `sleep` continues running. Wrapper forwards bytes.

**Critical design choice**: The wrapper never calls `tcsetpgrp()` itself. Bash is the session leader on the PTY slave and manages which process group is in the foreground. The wrapper doesn't participate in job control; it just forwards signals via the PTY.

**What could go wrong**:

- `SIGTSTP` could be intercepted by the wrapper process itself (since the wrapper's stdin is the real terminal). Mitigation: In Passthrough mode, the wrapper's real terminal should have `ISIG` disabled (because we don't process Ctrl+C/Ctrl+Z ourselves), and keystrokes are forwarded raw.

---

### 1.5 Pipeline with Multiple Commands

**Scenario**: User types `cat file.txt | grep pattern | sort | head`.

**Flow**:

1. Wrapper is in Edit mode. Command is composed.
2. Command is injected to PTY master.
3. Bash's DEBUG trap fires. The guard ensures `PREEXEC` is emitted only once despite the pipeline having four simple commands.
4. Wrapper enters Passthrough.
5. Bash constructs the pipeline, spawning four processes. All run on the PTY slave's process group.
6. Output flows through the pipeline; final `head` output reaches the PTY slave stdout → wrapper → real stdout.
7. Pipeline completes. `PRECMD` with the exit code of the last command in the pipeline (from `$?`) → `PROMPT`.
8. Wrapper returns to Edit mode.

**The DEBUG trap guard is essential here.** Without it, the trap fires for `cat`, `grep`, `sort`, and `head`, emitting four `PREEXEC` markers. The wrapper would see the first and transition to Passthrough; the remaining three would appear as garbage in the output stream.

---

### 1.6 Empty Command (User Presses Enter with Nothing Typed)

**Scenario**: User presses Enter at an empty prompt.

**Flow**:

1. reedline returns `Signal::Success("")`.
2. Wrapper injects `\n` to the PTY master (an empty command).
3. Bash does nothing meaningful (empty command is a no-op).
4. `PRECMD;0` → `PROMPT` → Edit mode.

**Design choice**: Always inject the line, even if empty. This keeps the wrapper's logic uniform and avoids special-casing. Bash handles empty input gracefully.

---

### 1.7 Ctrl+C During Editing

**Scenario**: User is partway through typing a command and presses Ctrl+C.

**Flow**:

1. reedline intercepts Ctrl+C and returns `Signal::CtrlC`.
2. Wrapper clears the current line (or lets reedline handle the clear) and re-enters Edit mode.
3. No command is sent to Bash. No mode transition occurs.

**Key point**: In Edit mode, Ctrl+C is handled by reedline, not by the terminal's signal machinery. The wrapper's real terminal is in raw mode with `ISIG` disabled, so no `SIGINT` is generated.

---

### 1.8 Ctrl+C During Command Execution

**Scenario**: User runs `sleep 100`, then presses Ctrl+C while it's running.

**Flow**:

1. Wrapper is in Passthrough mode. User presses Ctrl+C.
2. The keystroke (byte 0x03) is forwarded to the PTY master.
3. The PTY line discipline on the slave side interprets it as SIGINT and delivers it to the foreground process group.
4. `sleep` receives SIGINT and terminates.
5. Bash updates job table. `PRECMD;130` (128 + signal 2) → `PROMPT`.
6. Wrapper transitions to Edit mode with exit code 130.

**This is entirely transparent to the wrapper.** It doesn't know or care that a signal was sent. It just forwarded a byte and later received markers indicating the command finished.

---

### 1.9 Ctrl+D at Empty Prompt

**Scenario**: User presses Ctrl+D with no text in the editor (standard "exit" gesture).

**Flow**:

1. reedline returns `Signal::CtrlD`.
2. Wrapper injects `exit\n` to the PTY master.
3. Bash processes `exit` → terminates.
4. PTY master read returns EOF (or error).
5. Wrapper detects child exit → cleans up → exits with Bash's exit code.

---

### 1.10 SSH Session Through the Wrapper

**Scenario**: User types `ssh remote-host`.

**Flow**:

1. Command injected. `PREEXEC` → Passthrough.
2. SSH starts, negotiates with the remote host, allocates a remote PTY.
3. The remote shell's output flows: remote PTY → SSH → local PTY slave → PTY master → wrapper → real stdout.
4. User interacts normally. All keystrokes forwarded.
5. User types `exit` on the remote shell. SSH exits.
6. `PRECMD` → `PROMPT` → Edit mode.

**Why this works**: SSH is just another program running on the PTY. The wrapper's Passthrough mode is a byte pipe. SSH handles its own terminal negotiation on the PTY slave side.

**Nested wrappers**: If the user runs `wrashpty` inside an SSH session that's already inside `wrashpty`, it works because each wrapper manages its own PTY pair independently. Marker sequences from the inner wrapper are contained within the inner PTY and never reach the outer wrapper's parser.

---

## 2. Editor Feature Scenarios

### 2.1 Autosuggestion Acceptance

**Scenario**: User's history contains `docker compose up -d`. User types `doc`.

**Flow**:

1. User types `d`. Hinter searches history for entries starting with `d`. Finds many matches; shows most recent.
2. User types `o`, `c`. Hinter narrows to entries starting with `doc`. Shows `docker compose up -d` as ghost text.
3. User presses Right Arrow (or End). reedline accepts the full suggestion, filling the line to `docker compose up -d`.
4. User presses Enter to execute.

**Alternative**: User presses Ctrl+Right to accept one word at a time (`docker` → `docker compose` → etc.).

---

### 2.2 Tab Completion with Menu

**Scenario**: User types `cd /usr/lo` and presses Tab.

**Flow**:

1. Completer receives the partial input `cd /usr/lo`.
2. Filesystem completer scans `/usr/` for entries starting with `lo`. Finds `local/`, `locale/`.
3. reedline displays a completion menu with the two options.
4. User navigates with arrow keys or Tab cycling, selects `local/`.
5. reedline inserts the completion: line becomes `cd /usr/local/`.

---

### 2.3 Ctrl+R History Search

**Scenario**: User presses Ctrl+R and types `deploy`.

**Flow**:

1. reedline activates its built-in reverse search mode.
2. User types `deploy`. reedline filters history to entries containing "deploy", showing the most recent match inline.
3. User presses Enter to accept the matched line and execute it immediately.
4. Or user presses Escape to return to normal editing with the matched line in the buffer.

---

### 2.4 Git Branch Completion

**Scenario**: User types `git checkout fea` and presses Tab. Current repository has branches `feature/login`, `feature/api`, `fix/typo`.

**Flow**:

1. Completer detects that the command starts with `git checkout`.
2. Git branch completer runs `git branch --list` (or reads `.git/refs/heads`).
3. Filters to branches starting with `fea`: `feature/login`, `feature/api`.
4. reedline shows the menu. User selects `feature/login`.
5. Line becomes `git checkout feature/login`.

---

## 3. Failure and Recovery Scenarios

### 3.1 Wrapper Panics Mid-Edit

**Scenario**: A bug in the completer causes an index-out-of-bounds panic while the user is typing.

**Recovery flow**:

1. Rust begins unwinding the stack.
2. `EchoGuard::drop()` fires (if mid-injection) → restores PTY ECHO flag.
3. `TerminalGuard::drop()` fires → attempts to restore original termios settings.
4. If termios restoration fails, `TerminalGuard::drop()` writes fallback terminal reset escape sequences (DECSTR, scroll region reset, cursor show, attribute reset).
5. Custom panic hook fires → calls `crossterm::terminal::disable_raw_mode()` as belt-and-suspenders.
6. Panic message prints to stderr (now readable because terminal is restored).
7. Child Bash is still running on the PTY. When the wrapper process exits, the PTY master closes.
8. The kernel sends `SIGHUP` to the child's process group. Bash terminates.
9. User's terminal is usable. They can start a new shell.

**Defense in depth**: The recovery flow has multiple fallback layers:
- Layer 1: RAII guards (`EchoGuard`, `TerminalGuard`)
- Layer 2: Fallback reset sequences in `TerminalGuard::drop()`
- Layer 3: Panic hook with explicit crossterm cleanup
- Layer 4: Terminal emulator's own reset on process exit

**Verification**: This scenario should be explicitly tested during development by inserting `panic!()` calls at various points, including during echo suppression, during passthrough, and during chrome rendering.

---

### 3.2 Child Bash Crashes

**Scenario**: Bash receives a SIGSEGV (unlikely but possible with certain loadable builtins or corruption).

**Recovery flow**:

1. Bash terminates. PTY slave is closed.
2. Wrapper's next `read()` on PTY master returns EOF or `EIO`.
3. Wrapper detects child exit via `try_wait()`.
4. Wrapper restores terminal state and exits with code 1.

---

### 3.3 Marker Never Arrives (Hung State)

**Scenario**: The user's `.bashrc` contains an infinite loop or a command that never completes in PROMPT_COMMAND.

**Detection**: The wrapper starts in Passthrough mode and waits for the first `PROMPT` marker. If it never arrives, the wrapper stays in Passthrough indefinitely.

**Behavior**: This is actually correct. The user can still interact with whatever is happening (because Passthrough forwards everything). If they manage to break out of the loop, markers will eventually arrive and Edit mode will activate.

**Mitigation**: A startup timeout (e.g., 10 seconds without seeing `PROMPT`) could log a diagnostic message suggesting the user check their `.bashrc`.

---

### 3.4 Marker Spoofing / Marker Appears in Normal Output

**Scenario**: A malicious or buggy program prints bytes that look like a marker to stdout.

**Format**: Markers include a session token: `ESC ] 777 ; <16-hex-chars-token> ; PROMPT BEL`

**Attack vectors**:
1. **Accidental collision**: A program happens to output bytes matching the marker format
2. **Deliberate spoofing**: A malicious program tries to forge markers to hijack the wrapper state

**Defense: Session Token**

Each wrapper session generates a random 64-bit token (8 bytes = 16 hex characters) at startup using `getrandom`. This token is:
- Embedded in the generated bashrc
- Known only to the marker parser
- Required for any marker to be accepted

A program without the token cannot forge valid markers. Even if a program outputs `ESC ] 777 ; PROMPT BEL`, the parser rejects it because:
- The expected format is `ESC ] 777 ; <token> ; <type> BEL`
- Missing or wrong token → treated as passthrough bytes

**Security event handling**:
- Invalid tokens are logged to file (not terminal)
- Rate limiting: >100 invalid tokens triggers a warning
- No denial-of-service: invalid markers don't crash or block the wrapper

**Probability of accidental collision**: With 64-bit tokens (16 hex characters), probability is ~2^-64 per marker attempt. With no oracle to test tokens (wrong tokens are silently ignored), brute-force requires ~10^19 attempts on average — effectively impossible.

---

### 3.5 Very Large Output (cat bigfile)

**Scenario**: User runs `cat` on a multi-gigabyte file.

**Flow**:

1. Wrapper is in Passthrough. Bytes flow from PTY master to stdout at the speed of the PTY buffer and read/write syscalls.
2. Marker parser scans every byte but does no allocation and minimal branching.
3. Throughput should be within 2x of raw Bash (the overhead is one extra read+write syscall pair per buffer).

**Performance target**: `cat /dev/urandom | head -c 100M > /dev/null` should complete in under 2x the time of the same command in raw Bash.

---

### 3.6 Rapid Paste (Bracketed Paste)

**Scenario**: User pastes a large block of text (e.g., a multi-line script) into the prompt.

**MVP Implementation**: Let reedline handle bracketed paste entirely.

**In Edit mode**:
- reedline (via crossterm) enables bracketed paste on the real terminal (`ESC [?2004h`)
- When user pastes, terminal sends `ESC [200~` ... pasted text ... `ESC [201~`
- reedline detects these brackets and inserts the entire text as a block
- This prevents each character from being processed individually (avoiding autosuggestion flicker)

**In Passthrough mode**:
- Wrapper disables bracketed paste on PTY slave (Bash doesn't need it)
- Raw paste goes directly to child program
- This is the expected behavior—the child program handles paste its own way

**Configuration**:
```rust
// On entering Edit mode
crossterm::execute!(stdout(), EnableBracketedPaste)?;

// On entering Passthrough mode
crossterm::execute!(stdout(), DisableBracketedPaste)?;
```

**Why reedline handles this**: reedline already has robust bracketed paste support via crossterm. The wrapper doesn't need custom handling—just enable/disable based on mode.

---

### 3.7 Terminal Resize During Edit Mode

**Scenario**: User resizes the terminal window while typing a command.

**Flow**:

1. `SIGWINCH` is delivered to the wrapper.
2. Signal handler sets a flag (or writes to a pipe).
3. **Critical**: The wrapper does NOT process this signal during Edit mode.
4. reedline handles `SIGWINCH` internally via crossterm's signal handling.
5. reedline queries the new size and redraws the prompt and current input at the new width.

**Why the wrapper doesn't intercept SIGWINCH in Edit mode**: reedline has its own internal `SIGWINCH` handler (via crossterm). If the wrapper also handled the signal, both would attempt to manage the terminal size, causing race conditions and corrupted rendering. By letting reedline own `SIGWINCH` during Edit mode, we avoid conflicts.

**PTY synchronization**: When reedline returns (user submits a command), the wrapper queries the current terminal size and resizes the PTY before transitioning to Passthrough. This ensures the PTY is synchronized regardless of any resizes that occurred during editing.

---

### 3.8 Terminal Resize During Passthrough (Full-Screen App)

**Scenario**: User resizes the terminal while Vim is running.

**Flow**:

1. `SIGWINCH` delivered to wrapper.
2. Wrapper owns signal handling in Passthrough mode.
3. Wrapper queries real terminal size via `ioctl(TIOCGWINSZ)`.
4. Wrapper resizes the PTY slave via `ioctl(TIOCSWINSZ)`.
5. Kernel delivers `SIGWINCH` to the foreground process group on the PTY (Vim's group).
6. Vim queries the new size and redraws.
7. If chrome is active, wrapper redraws top bar and footer.

**The wrapper handles resize in Passthrough because reedline is not active.** Child programs don't know about the wrapper; they only see the PTY size and receive `SIGWINCH` from the kernel.

---

### 3.9 Background Job Produces Output

**Scenario**: User runs `(sleep 5 && echo "done") &`, then starts typing a new command. After 5 seconds, "done" appears.

**Flow**:

1. The background job's output goes to the PTY slave stdout.
2. Wrapper is in Edit mode (reedline is active).
3. The PTY master becomes readable with the background job's output.

**Challenge**: reedline owns the terminal display. If the wrapper writes background output directly to stdout, it will corrupt reedline's rendering.

**Design decision for MVP**: In Edit mode, buffer any PTY output using a **bounded ring buffer** and display it only after the next command completes (i.e., when transitioning back through Passthrough). This is the same behavior as Bash with Readline — background output interleaves messily.

#### Bounded Ring Buffer for Background Output

**Critical**: The buffer must have a maximum size to prevent memory exhaustion from runaway background jobs.

```rust
use std::collections::VecDeque;

const MAX_PENDING_OUTPUT: usize = 64 * 1024;  // 64KB maximum

pub struct PendingOutputBuffer {
    buffer: VecDeque<u8>,
    dropped_bytes: usize,
    drop_warned: bool,
}

impl PendingOutputBuffer {
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(4096),  // Start small
            dropped_bytes: 0,
            drop_warned: false,
        }
    }

    /// Push new data, dropping oldest bytes if buffer would exceed maximum.
    pub fn push(&mut self, data: &[u8]) {
        let new_len = self.buffer.len() + data.len();

        if new_len > MAX_PENDING_OUTPUT {
            // Calculate how many bytes to drop from front
            let overflow = new_len - MAX_PENDING_OUTPUT;
            let to_drop = overflow.min(self.buffer.len());
            self.buffer.drain(..to_drop);
            self.dropped_bytes += to_drop;

            // Log warning once per overflow event
            if !self.drop_warned {
                // tracing::warn!("Background output buffer full; dropping oldest bytes");
                self.drop_warned = true;
            }
        }

        self.buffer.extend(data);
    }

    /// Drain all buffered data for output.
    /// Returns (data, dropped_count) where dropped_count indicates lost bytes.
    pub fn drain(&mut self) -> (Vec<u8>, usize) {
        let data: Vec<u8> = self.buffer.drain(..).collect();
        let dropped = self.dropped_bytes;
        self.dropped_bytes = 0;
        self.drop_warned = false;
        (data, dropped)
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }
}
```

**Buffer overflow handling**: When the buffer is full and new data arrives:
1. Oldest bytes are dropped (FIFO eviction)
2. A warning is logged (once per overflow event)
3. When drained, the caller is informed of dropped byte count
4. Caller displays "[N bytes dropped]" message to user

This prevents a malicious or buggy background job from consuming unbounded memory:
```bash
(while true; do echo spam; done) &  # Won't exhaust memory
```

#### Buffer Status Visibility (MVP Requirement)

Users must be able to see when background output is buffered. This prevents confusion when output appears to be "lost" during Edit mode.

**Status indicator in prompt or chrome footer**:
- When `buffer.len() > 0`: Show `[N bytes buffered]` or compact indicator like `📥 4.2KB`
- When `dropped_bytes > 0`: Show `[N bytes dropped]` or warning indicator like `⚠️ 12KB lost`
- The indicator updates on each PTY read during Edit mode

**Flush keybinding (Ctrl+L variant)**:
- `Ctrl+Shift+L` or configurable key: Immediately flush buffered output above the prompt line
- Useful when user wants to see background job output without submitting a command
- After flush, reedline redraws the prompt (already supported via `Signal::Refresh`)

**Example flush implementation**:
```rust
impl App {
    fn handle_flush_keybinding(&mut self) -> Result<()> {
        let (data, dropped) = self.pending_output.drain();
        if !data.is_empty() {
            // Clear current line, output buffered data, then refresh prompt
            write!(stdout(), "\r\x1b[K")?;  // Clear line
            stdout().write_all(&data)?;
            if dropped > 0 {
                writeln!(stdout(), "\n[{} bytes were dropped due to buffer overflow]", dropped)?;
            }
            stdout().flush()?;
            // reedline will redraw prompt after we return
        }
        Ok(())
    }
}
```

#### Buffer Size (MVP: Fixed 64KB)

**MVP Implementation**: Buffer size is hardcoded to 64KB. This simplifies implementation and testing.

```rust
const MAX_PENDING_OUTPUT: usize = 64 * 1024;  // Fixed for MVP

pub struct PendingOutputBuffer {
    buffer: VecDeque<u8>,
    dropped_bytes: usize,
    drop_warned: bool,
}

impl PendingOutputBuffer {
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(4096),  // Start small, grow as needed
            dropped_bytes: 0,
            drop_warned: false,
        }
    }
}
```

**Rationale**: 64KB is sufficient for typical background output while preventing memory exhaustion. The fixed size avoids configuration complexity for MVP.

**Future enhancement** (post-MVP):
- CLI flag: `--pending-buffer-size 128K`
- Config file: `pending_output.max_size = "128KB"`
- Range: 4KB minimum, 1MB maximum

**Future improvement**: Redraw reedline after injecting the background output above the prompt line (like Fish does).

---

### 3.10 User's Bashrc Overrides PROMPT_COMMAND

**Scenario**: User's `~/.bashrc` sets `PROMPT_COMMAND='update_terminal_title'`.

**Mitigation**: The generated rcfile sources the user's `.bashrc` first, then overrides `PROMPT_COMMAND` after. The user's function still runs if it is called from within the wrapper's `__wrash_precmd` function. The generated rcfile can be structured to chain the user's existing `PROMPT_COMMAND`:

```bash
# Session token is embedded at rcfile generation time
__wrash_token="a1b2c3d4e5f67890"

# After sourcing user's bashrc
__user_prompt_command="${PROMPT_COMMAND}"
PROMPT_COMMAND='__wrash_precmd'
__wrash_precmd() {
    local ec=$?
    # Run user's original PROMPT_COMMAND
    eval "${__user_prompt_command}"
    # Emit markers with session token (preserving exit code)
    printf '\e]777;%s;PRECMD;%d\a' "$__wrash_token" "$ec"
}
```

This preserves the user's prompt customizations (terminal title, etc.) while ensuring authenticated markers are always emitted.

---

### 3.11 Command Produces Incomplete Escape Sequence at Read Boundary

**Scenario**: A program outputs `ESC ]` followed by more bytes, but a read boundary falls between `ESC` and `]`, or between `]` and the rest.

**Flow**:

1. First read: contains `...normal bytes... ESC`. Parser transitions to `EscSeen` state with `ESC` buffered.
2. Second read: starts with `]`. Parser transitions to `OscBody`.
3. Subsequent reads: accumulate the OSC body until `BEL`.
4. Parser determines it's not a `777;...` marker → flushes the entire sequence as normal output.

**Correctness guarantee**: The parser's internal buffer retains partial sequences across reads. No bytes are lost. This is the primary correctness requirement of the streaming parser design.

---

### 3.12 Multiple Wrashpty Instances

**Scenario**: User opens multiple terminal tabs, each running Wrashpty.

**Behavior**: Each instance is independent. Each spawns its own Bash child on its own PTY pair. There is no shared state between instances.

**HISTFILE contention**: Multiple Bash instances writing to `~/.bash_history` is a standard Bash concern (not introduced by Wrashpty). Bash handles this via `histappend` and related settings. Wrashpty's history module reads the file but does not write to it (Bash handles persistence).

---

## 4. Startup and Shutdown

### 4.1 Startup Sequence

1. Parse CLI arguments.
2. Install panic hook with terminal restoration.
3. Generate temporary rcfile.
4. Spawn Bash on PTY with `--noediting --rcfile <tmpfile>`.
5. Enter Passthrough mode.
6. Wait for initial `PRECMD` + `PROMPT` markers (Bash initialization complete).
7. Transition to Edit mode. User sees the prompt.

**Startup failure modes**:
- Bash not found on PATH → exit with descriptive error.
- Tempfile creation fails → exit with error.
- PTY allocation fails → exit with error.
- Initial markers never arrive (timeout) → warn, remain in Passthrough.

### 4.2 Shutdown Sequence

Triggered by: Ctrl+D at empty prompt, `exit` command, child exit, or wrapper receiving SIGTERM.

1. If in Edit mode, inject `exit\n` to Bash.
2. Wait for child to exit (with timeout).
3. If child doesn't exit within timeout, send SIGTERM to child's process group.
4. Restore terminal state (TerminalGuard drop).
5. Clean up tempfile (GeneratedRc drop).
6. Exit with child's exit code.

---

## 5. Chrome Layer Scenarios

### 5.1 Chrome Toggle During Edit Mode

**Scenario**: User is editing a command and presses the chrome toggle keybinding (e.g., Ctrl+Shift+H).

**Flow (OFF → ON)**:

1. Wrapper saves current cursor position within reedline.
2. Set scroll region: DECSTBM(1, rows-2).
3. Draw top bar at row 0.
4. Draw footer at row N-1.
5. Resize PTY to (cols, rows-2). Child receives SIGWINCH but is idle (waiting for input).
6. Notify reedline of new viewport size.
7. reedline redraws the prompt within the smaller region.

**Flow (ON → OFF)**:

1. Clear row 0 and row N-1.
2. Reset scroll region to full screen.
3. Resize PTY to (cols, rows).
4. Notify reedline of new viewport size.
5. reedline redraws at full size.

**Key invariant**: The command being edited is preserved. Only the viewport changes.

---

### 5.2 Chrome Toggle During Passthrough Mode

**Scenario**: User is running `htop` and toggles chrome.

**Flow (OFF → ON)**:

1. Set scroll region: DECSTBM(1, rows-2).
2. Draw top bar at row 0.
3. Draw footer at row N-1.
4. Resize PTY to (cols, rows-2).
5. Kernel delivers SIGWINCH to htop's process group.
6. htop redraws at the new size within the scroll region.

**Flow (ON → OFF)**:

1. Clear row 0 and row N-1.
2. Reset scroll region to full screen.
3. Resize PTY to (cols, rows).
4. htop receives SIGWINCH and redraws at full size.

**The toggle is transparent to the child program.** It just sees a resize event and adapts accordingly.

---

### 5.3 Terminal Resize with Chrome Active

**Scenario**: User resizes the terminal window while chrome is visible and a program is running.

**Flow**:

1. SIGWINCH delivered to wrapper.
2. Query real terminal size: (new_cols, new_rows).
3. Calculate effective_rows = new_rows - 2.
4. Resize PTY to (new_cols, effective_rows).
5. Reset scroll region to DECSTBM(1, effective_rows).
6. Redraw top bar at row 0.
7. Redraw footer at row (new_rows - 1).
8. Child program receives SIGWINCH and redraws.

**Critical detail**: The footer position is calculated from the real terminal height (row N-1), not the scroll region height. The scroll region defines where content scrolls; the bars are outside it.

---

### 5.4 Full-Screen App with Chrome

**Scenario**: User runs `vim file.txt` with chrome enabled.

**MVP REQUIREMENT: Scroll Region Reset on Passthrough Transition**

Chrome scroll regions MUST be disabled on every transition to Passthrough mode. This is a mandatory safety requirement, not an optimization. The wrapper calls `enter_passthrough_mode()` which emits `\x1b[r` (reset scroll region to full screen) before any Passthrough I/O begins.

**Flow (Mandatory MVP implementation)**:

1. Command injected. PREEXEC → Passthrough mode.
2. **MANDATORY**: Wrapper calls `enter_passthrough_mode()` — resets scroll region to full screen via `\x1b[r`.
3. Vim starts. It queries terminal size via ioctl on its fd — gets (cols, rows), full terminal size.
4. Vim configures the terminal (alternate screen, etc.) and uses the full screen.
5. Top bar and footer are not visible during Passthrough (by design).
6. User quits vim. PRECMD → PROMPT → Edit mode.
7. On entering Edit mode, wrapper re-establishes scroll region via DECSTBM.
8. Top bar and footer are redrawn. Prompt renders within the scroll region.

**Why this is mandatory (not optional)**: Programs using alternate screen buffers don't reset scroll regions when exiting. If the wrapper kept scroll regions active during Passthrough, vim's exit could leave the terminal in a corrupted state with the scroll region still active but the bars gone. This corruption is difficult for users to recover from. By unconditionally disabling scroll regions on Passthrough transition, we eliminate this class of bugs entirely.

**Acceptable tradeoff**: The bars aren't visible during command execution. This is acceptable because:
- Status information (git, exit code, duration) is only useful after a command completes
- Full-screen apps like vim would obscure the bars anyway
- The simpler implementation eliminates terminal corruption risks

**Future opt-in (not MVP)**: CSI-based alternate screen detection (`\x1b[?1049h`/`\x1b[?1049l`) could allow keeping scroll regions active during non-full-screen commands. This MUST be gated behind an explicit opt-in configuration flag (`chrome.smart_scroll_regions = true`) and MUST NOT be the default. The streaming marker parser would need to be extended to detect these sequences in-band. See Architecture doc section on "Future: CSI Alternate Screen Detection" for details.

---

### 5.5 Alternate Screen Modal (Picker/Confirmation)

**Scenario**: User triggers a file picker or confirmation dialog (e.g., for a destructive operation).

**Flow**:

1. User presses keybinding (e.g., Ctrl+F for file picker).
2. Wrapper switches to the alternate screen buffer via escape sequence.
3. Wrapper renders the modal UI (file list, search input, etc.) using crossterm or ratatui.
4. User navigates, makes a selection, presses Enter.
5. Wrapper captures the selection.
6. Wrapper switches back to the main screen buffer.
7. Main screen is exactly as it was: bars, prompt, any partial input.
8. Wrapper uses the selection (e.g., inserts file path into command line).

**Why this works**: The alternate screen buffer is independent of the main buffer. Switching to it doesn't affect scroll region, cursor position, or content on the main screen. The terminal emulator maintains both buffers.

**Modal types this enables**:
- File/directory picker (like fzf)
- Git branch selector
- History search with preview
- Command palette
- Confirmation dialogs for destructive operations

---

### 5.6 Status Update in Footer

**Scenario**: User runs a command that changes directory. Git status in footer needs to update.

**Flow**:

1. User runs `cd ~/projects/my-repo`.
2. Command executes. PRECMD marker arrives with exit code 0.
3. Wrapper updates internal state: new cwd, new git status (if applicable).
4. On transition to Edit mode, footer is redrawn with updated git branch/dirty state.
5. User sees updated footer while editing next command.

**Update triggers**:
- After every PRECMD (command finished)
- On chrome toggle (redraw with current state)
- On explicit refresh keybinding (if implemented)

**Footer never updates mid-command**: During Passthrough, the wrapper doesn't redraw the footer because scroll regions are disabled in Passthrough mode (for robustness), and writing to fixed terminal positions could interfere with the child's output.

---

## 6. Edge Cases and Recovery Scenarios

### 6.1 Terminal Disconnect (SIGHUP)

**Scenario**: User closes the terminal emulator window, or SSH connection drops.

**Flow**:

1. Kernel sends SIGHUP to wrashpty (as the session leader's child).
2. Signal handler writes to signal pipe.
3. Main loop detects SIGHUP event.
4. Wrapper sends SIGHUP to child's process group.
5. Wrapper waits up to 1 second for child to exit.
6. Wrapper exits (terminal is gone, no state to restore).

**Key points:**
- No terminal restoration needed (terminal is gone)
- Child should receive SIGHUP and clean up
- Exit code reflects that we received SIGHUP (128 + 1 = 129)

---

### 6.2 Nested Wrashpty

**Scenario**: User runs `wrashpty` inside an existing wrashpty session.

**Flow**:

1. Inner wrashpty spawns on a new PTY pair.
2. Inner Bash starts with its own rcfile.
3. Markers from inner wrashpty are just bytes to outer wrashpty.
4. Each instance operates independently.

**Why this works**: Each PTY pair is isolated. The outer wrapper sees the inner wrapper's escape sequences as normal output bytes. The inner wrapper's markers don't match the outer wrapper's session token (and vice versa).

**User experience**: Works correctly. The inner prompt appears within the outer's scroll region. Chrome from both instances can be active simultaneously (each managing its own terminal).

---

### 6.3 User Runs `exec zsh` or Other Shell

**Scenario**: User types `exec zsh` to replace Bash with Zsh.

**Flow**:

1. Command is injected. PREEXEC marker emitted.
2. Bash execs Zsh. The Bash process is replaced.
3. No more markers are emitted (Zsh doesn't have our rcfile).
4. Wrapper remains in Passthrough mode indefinitely.

**Behavior**: This is acceptable. The wrapper becomes a transparent pipe to Zsh. The user loses wrashpty features but the session continues working. They can `exit` normally.

**Detection**: If no PROMPT marker is seen for 60 seconds after a command, log a notice: "Shell may have been replaced; operating in passthrough-only mode."

---

### 6.4 Subshell Marker Interference

**Scenario**: User runs `bash` (a subshell) or `( cd /tmp && some_command )`.

**Flow for `bash` subshell:**

1. Parent Bash runs child Bash.
2. Child Bash sources user's bashrc (which may set PROMPT_COMMAND).
3. If user's bashrc has wrashpty markers from a previous install, child emits markers.
4. These markers have wrong session token → rejected by parser.

**Flow for `( subshell )`:**

1. Subshell inherits parent's environment but doesn't source bashrc.
2. No markers are emitted from subshell.
3. Wrapper stays in Passthrough for duration of subshell.
4. When subshell exits, parent Bash continues → PRECMD/PROMPT.

**Session token prevents interference**: Even if a subshell somehow emits markers, they won't have the correct session token and will be treated as normal output.

---

### 6.5 Non-UTF8 Encoding

**Scenario**: System locale is Latin-1, or command output contains invalid UTF-8.

**Flow**:

1. PTY output arrives as raw bytes.
2. Marker parser operates on bytes, not characters — no encoding issues.
3. Bytes are forwarded to stdout unchanged.

**Chrome handling:**
- Footer displays cwd; if cwd contains non-UTF8, replace with `?` or escape
- Git branch names are typically ASCII; non-UTF8 is unlikely
- Use `String::from_utf8_lossy()` for display, preserving valid parts

---

### 6.6 Multi-line Command (Heredoc, Continuation)

**Scenario**: User types a heredoc or uses backslash continuation.

```bash
cat << EOF
line 1
line 2
EOF
```

**Flow**:

1. User types first line, presses Enter.
2. reedline recognizes incomplete input (unmatched `<<`), shows continuation prompt.
3. User types until heredoc is complete.
4. reedline returns the complete multi-line string.
5. Wrapper injects entire string (including internal newlines).
6. Echo suppression matches the full multi-line string.

**Key point**: reedline handles the continuation prompts; Bash never sees PS2 because input comes as a complete unit.

---

### 6.7 Command Timeout (Initializing Mode)

**Scenario**: User's bashrc takes more than 10 seconds to execute.

**Flow**:

1. Wrapper starts in Initializing mode.
2. Bash sources bashrc (long-running operations).
3. 10-second timeout expires.
4. Wrapper logs warning: "Startup timeout; falling back to passthrough-only mode."
5. Wrapper enters Passthrough mode.
6. If PROMPT marker eventually arrives, wrapper transitions to Edit mode.

**User experience**: The wrapper works, just with delayed feature activation. User can still interact during the long startup via passthrough.

---

### 6.8 Panic During Edit Mode

**Scenario**: A bug in the completer causes a panic while user is editing.

**Flow**:

1. Panic occurs. Rust begins unwinding.
2. Custom panic hook fires first:
   - Resets scroll region to full screen
   - Restores termios settings
   - Writes panic message to stderr
3. TerminalGuard::drop() fires (belt-and-suspenders restoration).
4. Child Bash is still running on PTY.
5. Wrapper process exits; PTY master closes.
6. Kernel sends SIGHUP to child Bash; it terminates.
7. User's terminal is usable; they see the panic message.

---

### 6.9 Very Small Terminal (Chrome Auto-Disable)

**Scenario**: Terminal is resized to 4 rows or 15 columns while chrome is active.

**Flow**:

1. SIGWINCH arrives with new size.
2. Wrapper checks: rows < 5 or cols < 20?
3. If yes, set `chrome_suspended = true`.
4. Reset scroll region to full screen.
5. Resize PTY to full terminal size.
6. Clear bars (if any visible content remains).

**On resize back to usable size:**

1. SIGWINCH with rows ≥ 5 and cols ≥ 20.
2. If `chrome_suspended && chrome_mode == Full`, re-enable chrome.
3. Set scroll region, draw bars.

**Flag tracking**: `chrome_suspended` is separate from `chrome_mode`. If user explicitly disabled chrome, resize doesn't re-enable it.

---

### 6.10 Background Job Output During Edit

**Scenario**: User runs `(sleep 2 && echo "done") &`, then starts typing. Two seconds later, "done" appears.

**MVP Behavior**:

1. Background job writes to PTY slave stdout.
2. PTY master becomes readable.
3. Wrapper is in Edit mode (reedline blocking on input).
4. Wrapper buffers the PTY output in a **bounded `PendingOutputBuffer`** (max 64KB).
5. If buffer fills, oldest bytes are dropped (ring buffer semantics).
6. When user submits a command, wrapper enters Passthrough.
7. Buffered output is flushed to stdout before the new command's output.
8. If bytes were dropped, wrapper optionally displays "[N bytes dropped]" indicator.

**Protection against runaway jobs**: The bounded buffer prevents memory exhaustion:
```bash
(while true; do echo spam; done) &  # Will not exhaust memory
```

**Limitation**: The buffered output appears after the next command starts, not inline. This matches Bash+Readline behavior.

**Future improvement**: Interrupt reedline on PTY activity, redraw prompt after injecting output.

---

## 7. Platform-Specific Considerations

### Linux (Primary Target)

PTY allocation via `posix_openpt` or `/dev/ptmx`. Well-supported by `portable-pty`. No known issues.

### macOS (Best-Effort)

**Status**: Best-effort support. Not blocking for MVP, but PRs welcome.

PTY semantics are slightly different (master fd behavior, some ioctl differences). `portable-pty` abstracts most of this. `TIOCSWINSZ` works the same.

**Critical requirement**: macOS ships with Bash 3.2 (GPLv2). User **must** install Bash 4.0+ from Homebrew (`brew install bash`) for Wrashpty to function. The startup version check will fail fast with a helpful error message including Homebrew installation instructions.

### WSL (Not Targeted)

WSL2 has good PTY support. WSL1 has limited PTY compatibility. Not a priority but likely works on WSL2 without changes.
