//! Wrashpty - A readline wrapper for bash with modern line editing.
//!
//! This is the entry point that sets up critical safety infrastructure
//! (panic hooks, logging) before any terminal operations begin.

use std::fs::File;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing::info;
use wrashpty::safety::install_panic_hook;

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
