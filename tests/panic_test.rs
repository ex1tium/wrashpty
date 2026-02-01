//! Manual panic test for terminal restoration.
//!
//! This test verifies that the terminal is properly restored after a panic,
//! exercising the full integration between the panic hook (from `safety.rs`)
//! and `TerminalGuard::Drop`.
//!
//! # Running the Test
//!
//! ```bash
//! cargo test panic_test -- --ignored --nocapture
//! ```
//!
//! # Expected Behavior
//!
//! 1. Panic hook is installed (async-signal-safe terminal restoration)
//! 2. Terminal enters raw mode via TerminalGuard
//! 3. Message is printed
//! 4. After 1 second delay, panic occurs
//! 5. Panic hook runs first (writes restore sequence via libc::write)
//! 6. TerminalGuard::Drop runs during unwinding (restores termios settings)
//! 7. Terminal should be restored (cursor visible, input works, no corruption)
//!
//! If the terminal is corrupted after the test, the safety system has failed.

use std::thread;
use std::time::Duration;
use wrashpty::safety::install_panic_hook;
use wrashpty::terminal::TerminalGuard;

#[test]
#[ignore] // Run manually: cargo test panic_test -- --ignored --nocapture
fn test_panic_hook_when_panic_occurs_restores_terminal() {
    // Install panic hook first - same order as main.rs
    install_panic_hook();

    // Create terminal guard - enters raw mode
    // Gracefully skip test in non-TTY environments (e.g., CI)
    let _guard = match TerminalGuard::new() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("Skipping test (no terminal available): {}", e);
            return;
        }
    };

    // Print message (note: in raw mode, \r\n needed for proper newline)
    print!("Terminal in raw mode. About to panic in 1 second...\r\n");

    // Give user time to see the message
    thread::sleep(Duration::from_secs(1));

    // Panic! The terminal should be restored by:
    // 1. Panic hook (async-signal-safe libc::write of escape sequences)
    // 2. TerminalGuard::Drop during unwinding (restores termios settings)
    panic!("Test panic - terminal should be restored");
}
