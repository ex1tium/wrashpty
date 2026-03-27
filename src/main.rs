//! Wrashpty - A readline wrapper for bash with modern line editing.
//!
//! This is the entry point that sets up critical safety infrastructure
//! (panic hooks, logging) before any terminal operations begin.

use std::fs::File;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use tracing::{debug, info};
use wrashpty::Config;
use wrashpty::app::App;
use wrashpty::app::commands::CommandRegistry;
use wrashpty::bashrc;
use wrashpty::safety::install_panic_hook;
use wrashpty::types::ChromeMode;

/// Modern interactive shell on stock Bash
#[derive(Parser)]
#[command(name = "wrashpty")]
#[command(version)] // Automatically pulls version from Cargo.toml
#[command(about = "Modern interactive shell on stock Bash")]
#[command(
    long_about = "Wrashpty provides modern line editing, autosuggestions, and command \
    intelligence on top of stock Bash without modifying the shell."
)]
#[command(author)]
#[command(disable_help_flag = true)]
struct Cli {
    /// Disable chrome layer (headless mode)
    #[arg(long, help = "Run without visual chrome (status bar, panels)")]
    no_chrome: bool,

    /// Output help in a specific machine-readable format
    #[arg(
        long,
        value_name = "FORMAT",
        help = "Output help in parseable format: json, gnu, npm, clap"
    )]
    help_format: Option<String>,

    /// Print help information (GNU-style)
    #[arg(long, short = 'h', action = clap::ArgAction::SetTrue)]
    help: bool,
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

/// Prints help in a machine-readable format, derived from the clap `Command`.
fn print_formatted_help(format: &str) {
    let cmd = Cli::command();
    let registry = CommandRegistry::new();
    let cmd_list = registry.command_list();

    match format {
        "json" => print_help_json(&cmd, &cmd_list),
        "gnu" => print_help_gnu(&cmd, &cmd_list),
        "npm" => print_help_npm(&cmd, &cmd_list),
        "clap" => print_help_clap(),
        _ => {
            eprintln!(
                "Unknown help format: '{}'. Supported formats: json, gnu, npm, clap",
                format
            );
            std::process::exit(1);
        }
    }
}

/// JSON format: command-schema-v1.json compliant structured output.
fn print_help_json(cmd: &clap::Command, colon_cmds: &[(&str, &[&str], &str)]) {
    let name = cmd.get_name();
    let version = cmd.get_version();
    let about = cmd.get_about().map(|s| s.to_string());

    // Build global_flags from clap arguments
    let mut global_flags = Vec::<serde_json::Value>::new();
    for arg in cmd.get_arguments() {
        let arg_name = arg.get_id().as_str();
        if arg_name == "help" || arg_name == "version" {
            continue;
        }
        let takes_value = arg.get_action().takes_values();
        let value_type = if takes_value { "String" } else { "Bool" };

        global_flags.push(serde_json::json!({
            "short": arg.get_short().map(|s| format!("-{s}")),
            "long": arg.get_long().map(|l| format!("--{l}")),
            "value_type": value_type,
            "takes_value": takes_value,
            "description": arg.get_help().map(|h| h.to_string()),
            "multiple": false,
            "conflicts_with": [],
            "requires": []
        }));
    }

    // Build subcommands from colon commands
    let subcommands: Vec<serde_json::Value> = colon_cmds
        .iter()
        .map(|(cmd_name, aliases, desc)| {
            let alias_list: Vec<String> = aliases.iter().map(|a| format!(":{a}")).collect();
            serde_json::json!({
                "name": format!(":{cmd_name}"),
                "description": desc,
                "flags": [],
                "positional": [],
                "subcommands": [],
                "aliases": alias_list
            })
        })
        .collect();

    let schema = serde_json::json!({
        "schema_version": "1.0.0",
        "command": name,
        "description": about,
        "version": version,
        "global_flags": global_flags,
        "subcommands": subcommands,
        "positional": [],
        "source": "Bootstrap",
        "confidence": 1.0
    });

    println!(
        "{}",
        serde_json::to_string(&schema).expect("JSON serialization failed")
    );
}

/// GNU-style help output.
fn print_help_gnu(cmd: &clap::Command, colon_cmds: &[(&str, &[&str], &str)]) {
    let name = cmd.get_name();
    let about = cmd.get_about().map(|s| s.to_string()).unwrap_or_default();

    println!("Usage: {name} [OPTIONS]");
    println!();
    println!("{about}");
    println!();

    // Options section
    let mut has_options = false;
    for arg in cmd.get_arguments() {
        let arg_name = arg.get_id().as_str();
        if arg_name == "help" || arg_name == "version" {
            continue;
        }
        if !has_options {
            println!("Options:");
            has_options = true;
        }
        let long = arg.get_long().map(|l| format!("--{l}")).unwrap_or_default();
        let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
        let takes_val = arg.get_action().takes_values();
        if takes_val {
            let val_name = arg
                .get_value_names()
                .map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| "VALUE".to_string());
            println!("  {:<24} {}", format!("{long}={val_name}"), help);
        } else {
            println!("  {:<24} {}", long, help);
        }
    }
    println!("  {:<24} Print help information", "--help");
    println!("  {:<24} Print version information", "--version");

    // Colon commands section
    if !colon_cmds.is_empty() {
        println!();
        println!("Built-in commands:");
        for (cmd_name, aliases, desc) in colon_cmds {
            let label = if aliases.is_empty() {
                format!(":{cmd_name}")
            } else {
                let alias_list = aliases
                    .iter()
                    .map(|a| format!(":{a}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(":{cmd_name} ({alias_list})")
            };
            println!("  {:<28} {}", label, desc);
        }
    }
}

/// NPM-style help output.
fn print_help_npm(cmd: &clap::Command, colon_cmds: &[(&str, &[&str], &str)]) {
    let name = cmd.get_name();
    let version = cmd.get_version().unwrap_or("unknown");
    let about = cmd.get_about().map(|s| s.to_string()).unwrap_or_default();

    println!("{name}@{version}");
    println!();
    println!("{about}");
    println!();
    println!("Usage:");
    println!("  {name} [options]");
    println!();

    // Options
    println!("Options:");
    for arg in cmd.get_arguments() {
        let arg_name = arg.get_id().as_str();
        if arg_name == "help" || arg_name == "version" {
            continue;
        }
        let long = arg.get_long().map(|l| format!("--{l}")).unwrap_or_default();
        let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
        let takes_val = arg.get_action().takes_values();
        if takes_val {
            let val_name = arg
                .get_value_names()
                .map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_else(|| "VALUE".to_string());
            println!("  {:<24} {}", format!("{long} <{val_name}>"), help);
        } else {
            println!("  {:<24} {}", long, help);
        }
    }
    println!("  {:<24} Print help", "--help");
    println!("  {:<24} Print version", "--version");

    // Colon commands
    if !colon_cmds.is_empty() {
        println!();
        println!("Commands:");
        for (cmd_name, aliases, desc) in colon_cmds {
            let label = if aliases.is_empty() {
                format!(":{cmd_name}")
            } else {
                let alias_list = aliases
                    .iter()
                    .map(|a| format!(":{a}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(":{cmd_name} ({alias_list})")
            };
            println!("  {:<28} {}", label, desc);
        }
    }
}

/// Clap-style help output (standard --help equivalent).
fn print_help_clap() {
    let mut cmd = Cli::command();
    cmd.print_help().expect("Failed to print help");
    println!();
}

fn main() -> Result<()> {
    // Install panic hook first - before any terminal operations
    install_panic_hook();

    // Set up file-based logging second
    setup_logging()?;

    info!("Wrashpty starting up");

    // Parse CLI arguments
    let cli = Cli::parse();

    // Handle --help (GNU-style by default)
    if cli.help {
        let cmd = Cli::command();
        let registry = CommandRegistry::new();
        let cmd_list = registry.command_list();
        print_help_gnu(&cmd, &cmd_list);
        std::process::exit(0);
    }

    // Handle --help-format before normal operation
    if let Some(ref format) = cli.help_format {
        print_formatted_help(format);
        std::process::exit(0);
    }

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
