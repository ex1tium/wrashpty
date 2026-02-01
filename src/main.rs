//! Wrashpty - A readline wrapper for bash with modern line editing.
//!
//! This is the entry point that sets up critical safety infrastructure
//! (panic hooks, logging) before any terminal operations begin.

mod app;
mod bashrc;
mod chrome;
mod complete;
mod editor;
mod history;
mod marker;
mod prompt;
mod pty;
mod pump;
mod signals;
mod suggest;
mod terminal;
mod types;

use std::fs::File;
use std::panic;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing::info;

/// Install a panic hook that performs async-signal-safe terminal restoration.
///
/// This is the first layer of the five-layer terminal safety system. When a
/// panic occurs, we must restore the terminal to a usable state before
/// displaying the panic message. The restoration sequence uses only
/// async-signal-safe operations (direct write syscalls).
fn install_panic_hook() {
    let original_hook = panic::take_hook();

    panic::set_hook(Box::new(move |panic_info| {
        // Async-signal-safe terminal restoration sequence:
        // - \x1b[r    : Reset scroll region to full screen (DECSTBM)
        // - \x1b[?25h : Show cursor (DECTCEM)
        //
        // We use libc::write directly to STDOUT_FILENO and STDERR_FILENO
        // without any buffering or allocation. This is async-signal-safe
        // and works even if the standard library's state is corrupted.
        let restore_sequence = b"\x1b[r\x1b[?25h";
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

/// Set up file-based logging.
///
/// Logging goes to /tmp/wrashpty.log, never to the controlled terminal.
/// This prevents log output from corrupting the terminal display.
fn setup_logging() -> Result<()> {
    let log_file = File::create("/tmp/wrashpty.log")
        .context("Failed to create log file at /tmp/wrashpty.log")?;

    // Mutex<W> implements MakeWriter for W: Write + 'static
    let log_file = Mutex::new(log_file);

    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(log_file)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .context("Failed to set global tracing subscriber")?;

    Ok(())
}

/// Validate that bash is available and log its version.
///
/// This performs basic version detection. Full version parsing and
/// compatibility checks are deferred to later implementation phases.
fn validate_bash_version() -> Result<()> {
    let output = Command::new("bash")
        .arg("--version")
        .output()
        .context("Failed to execute 'bash --version'")?;

    if !output.status.success() {
        anyhow::bail!("bash --version exited with non-zero status");
    }

    let version_output = String::from_utf8_lossy(&output.stdout);
    let first_line = version_output.lines().next().unwrap_or("unknown");

    info!(bash_version = %first_line, "Detected bash version");

    Ok(())
}

fn main() -> Result<()> {
    // Install panic hook first - before any terminal operations
    install_panic_hook();

    // Set up file-based logging second
    setup_logging()?;

    info!("Wrashpty starting up");

    // Validate bash is available
    validate_bash_version()?;

    // Bootstrap complete message
    println!("Wrashpty v0.1.0 - Bootstrap complete");

    // TODO: Initialize App and enter main event loop
    // This will be implemented in future tickets as part of Phase 0-3 development.

    Ok(())
}
