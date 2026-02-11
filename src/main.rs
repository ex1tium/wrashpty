//! Wrashpty - A readline wrapper for bash with modern line editing.
//!
//! This is the entry point that sets up critical safety infrastructure
//! (panic hooks, logging) before any terminal operations begin.

use std::fs::File;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{debug, info};
use wrashpty::Config;
use wrashpty::app::App;
use wrashpty::bashrc;
use wrashpty::safety::install_panic_hook;
use wrashpty::types::ChromeMode;

/// Modern interactive shell on stock Bash
#[derive(Parser)]
#[command(name = "wrashpty")]
#[command(version)] // Automatically pulls version from Cargo.toml
#[command(about = "Modern interactive shell on stock Bash")]
struct Cli {
    /// Disable chrome layer (headless mode)
    #[arg(long)]
    no_chrome: bool,
}

/// RAII guard for cleaning up the generated bashrc file.
///
/// If dropped without being disarmed, removes the bashrc file to prevent leaks
/// when App creation fails after bashrc generation.
struct BashrcGuard {
    path: String,
    armed: bool,
}

impl BashrcGuard {
    fn new(path: String) -> Self {
        Self { path, armed: true }
    }

    /// Returns a reference to the path.
    fn path(&self) -> &str {
        &self.path
    }

    /// Disarms the guard, preventing cleanup on drop.
    ///
    /// Call this after App is successfully created (App handles cleanup).
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for BashrcGuard {
    fn drop(&mut self) {
        if self.armed {
            debug!("BashrcGuard cleanup: removing {}", self.path);
            if let Err(e) = std::fs::remove_file(&self.path) {
                debug!("Failed to remove bashrc file during cleanup: {}", e);
            }
        }
    }
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

    // Parse CLI arguments
    let cli = Cli::parse();

    // Determine chrome mode from CLI flag
    let chrome_mode = if cli.no_chrome {
        ChromeMode::Headless
    } else {
        ChromeMode::Full
    };
    info!(chrome_mode = ?chrome_mode, "Chrome mode configured");

    // Load configuration from environment (theme, nerdfonts detection)
    let config = Config::from_env();
    info!(glyph_tier = ?config.glyph_tier, theme = ?config.theme, "Config loaded from environment");

    // Validate bash is available
    validate_bash_version()?;

    // Generate bashrc with session token
    let (bashrc_path, session_token) = bashrc::generate().context("Failed to generate bashrc")?;

    // Wrap bashrc path in a guard to ensure cleanup if App creation fails
    let bashrc_guard = BashrcGuard::new(bashrc_path);

    // Create and run the application in a block to ensure Drop runs before exit
    let exit_code = {
        let mut app = App::new(bashrc_guard.path(), session_token, chrome_mode, &config)
            .context("Failed to initialize App")?;
        // App created successfully - disarm guard since App owns bashrc cleanup
        bashrc_guard.disarm();
        let code = app.run().context("App run failed")?;
        info!(exit_code = code, "Wrashpty exiting");
        code
        // app dropped here: terminal restored, bashrc deleted
    };

    // Exit with the shell's actual exit code
    std::process::exit(exit_code);
}
