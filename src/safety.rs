//! Terminal safety infrastructure.
//!
//! This module provides the panic hook that performs async-signal-safe terminal
//! restoration. It is the first layer of the five-layer terminal safety system.

use std::panic;

/// Install a panic hook that performs async-signal-safe terminal restoration.
///
/// This is the first layer of the five-layer terminal safety system. When a
/// panic occurs, we must restore the terminal to a usable state before
/// displaying the panic message. The restoration sequence uses only
/// async-signal-safe operations (direct write syscalls).
///
/// # Safety Model
///
/// The panic hook uses `libc::write` directly to avoid any allocation or
/// buffering. This is critical because:
/// - The allocator may be in an inconsistent state during a panic
/// - Standard library I/O may be corrupted
/// - We need to work even in signal handlers (async-signal-safe)
///
/// # Example
///
/// ```no_run
/// use wrashpty::safety::install_panic_hook;
///
/// // Install before any terminal operations
/// install_panic_hook();
/// ```
pub fn install_panic_hook() {
    let original_hook = panic::take_hook();

    panic::set_hook(Box::new(move |panic_info| {
        // Async-signal-safe terminal restoration sequence:
        // - \x1b[r    : Reset scroll region to full screen (DECSTBM)
        // - \x1b[2J   : Clear entire screen (ED)
        // - \x1b[H    : Move cursor to home position (CUP)
        // - \x1b[?25h : Show cursor (DECTCEM)
        //
        // The screen clear prevents "ghost" content from remaining after crash.
        //
        // We use libc::write directly to STDOUT_FILENO and STDERR_FILENO
        // without any buffering or allocation. This is async-signal-safe
        // and works even if the standard library's state is corrupted.
        let restore_sequence = b"\x1b[r\x1b[2J\x1b[H\x1b[?25h";
        unsafe {
            // Write to both stdout and stderr to maximize chances of restoration
            libc::write(
                libc::STDOUT_FILENO,
                restore_sequence.as_ptr() as *const libc::c_void,
                restore_sequence.len(),
            );
            libc::write(
                libc::STDERR_FILENO,
                restore_sequence.as_ptr() as *const libc::c_void,
                restore_sequence.len(),
            );
        }

        // Chain to the original panic hook for message display
        original_hook(panic_info);
    }));
}
