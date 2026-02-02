//! PTY management, spawn, and resize.
//!
//! This module wraps portable-pty to create and manage the pseudo-terminal
//! that hosts the bash subprocess.

// Allow dead_code while module is not yet integrated into main application
#![allow(dead_code)]

use anyhow::{Context, Result};
use nix::sys::termios::{LocalFlags, SetArg, Termios, tcgetattr, tcsetattr};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use std::io::Write;
use std::os::fd::{BorrowedFd, RawFd};
use tracing::debug;

/// Wrapper around a pseudo-terminal with a spawned Bash process.
///
/// The `Pty` struct manages the lifecycle of a PTY master/slave pair and the
/// child Bash process. It provides methods for:
/// - Spawning Bash with a custom rcfile
/// - Resizing the terminal (for SIGWINCH handling)
/// - Writing commands to the PTY (for injection mode)
/// - Monitoring child process status
///
/// # Example
///
/// ```no_run
/// use wrashpty::pty::Pty;
///
/// let mut pty = Pty::spawn("/tmp/bashrc", 80, 24).unwrap();
/// pty.write_command("echo hello").unwrap();
/// let status = pty.wait().unwrap();
/// ```
pub struct Pty {
    /// PTY master for I/O operations
    master: Box<dyn MasterPty + Send>,
    /// Writer for sending input to the PTY (taken once from master)
    writer: Box<dyn Write + Send>,
    /// Child process handle for Bash
    child: Box<dyn Child + Send>,
    /// Current terminal dimensions
    size: PtySize,
}

impl Pty {
    /// Spawns a new Bash process on a pseudo-terminal.
    ///
    /// Creates a PTY with the specified dimensions and spawns Bash with
    /// `--noediting` (to prevent readline conflicts) and `--rcfile` pointing
    /// to the provided bashrc path.
    ///
    /// # Arguments
    ///
    /// * `bashrc_path` - Path to the bashrc file for Bash initialization
    /// * `cols` - Number of terminal columns
    /// * `rows` - Number of terminal rows
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - PTY allocation fails (e.g., system resource limits)
    /// - Bash cannot be spawned (e.g., bash not found)
    pub fn spawn(bashrc_path: &str, cols: u16, rows: u16) -> Result<Self> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size)
            .context("Failed to create PTY (check system resource limits)")?;

        let mut cmd = CommandBuilder::new("bash");
        cmd.arg("--noediting");
        cmd.arg("--rcfile");
        cmd.arg(bashrc_path);

        // Set TERM so ncurses-based applications (nano, htop, vim) work correctly.
        // Without this, control characters like Ctrl+X may not be interpreted properly.
        // Inherit TERM from parent if available, otherwise default to xterm-256color.
        // Treat empty or missing TERM as missing.
        let term = std::env::var("TERM")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "xterm-256color".to_string());
        cmd.env("TERM", term);

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn Bash")?;

        // Take the writer once during construction so it can be reused
        let writer = pair
            .master
            .take_writer()
            .context("Failed to get PTY writer")?;

        tracing::info!(rows, cols, "Spawned Bash on PTY ({}x{})", cols, rows);

        Ok(Self {
            master: pair.master,
            writer,
            child,
            size,
        })
    }

    /// Returns the raw file descriptor for the PTY master.
    ///
    /// This is used by the pump module for `poll()` in the event loop.
    ///
    /// # Panics
    ///
    /// Panics if the PTY master file descriptor is not available.
    pub fn master_fd(&self) -> RawFd {
        self.master
            .as_raw_fd()
            .expect("PTY master file descriptor not available")
    }

    /// Resizes the PTY to new dimensions.
    ///
    /// This should be called in response to SIGWINCH signals when the
    /// terminal window size changes.
    ///
    /// # Arguments
    ///
    /// * `cols` - New number of terminal columns
    /// * `rows` - New number of terminal rows
    ///
    /// # Errors
    ///
    /// Returns an error if the resize ioctl fails.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.size.cols = cols;
        self.size.rows = rows;

        self.master
            .resize(self.size)
            .context("Failed to resize PTY")?;

        tracing::debug!(cols, rows, "Resized PTY to {}x{}", cols, rows);

        Ok(())
    }

    /// Writes a command to the PTY with a trailing newline.
    ///
    /// This is used during Injecting mode to send commands from the LLM
    /// to the Bash subprocess.
    ///
    /// # Arguments
    ///
    /// * `cmd` - The command string to write (newline is appended automatically)
    ///
    /// # Errors
    ///
    /// Returns an error if writing to the PTY fails.
    ///
    /// # Note
    ///
    /// Echo suppression is not implemented in this version - it will be
    /// added in a future ticket using RAII guards.
    pub fn write_command(&mut self, cmd: &str) -> Result<()> {
        writeln!(self.writer, "{}", cmd).context("Failed to write command to PTY")?;

        self.writer.flush().context("Failed to flush PTY writer")?;

        tracing::debug!(command = cmd, "Wrote command to PTY");

        Ok(())
    }

    /// Checks if the child Bash process has exited without blocking.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(status))` - Child has exited with the given status
    /// - `Ok(None)` - Child is still running
    ///
    /// # Errors
    ///
    /// Returns an error if checking the child status fails.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .context("Failed to check child status")
    }

    /// Waits for the child Bash process to exit.
    ///
    /// This blocks until the child process terminates.
    ///
    /// # Returns
    ///
    /// The exit status of the child process.
    ///
    /// # Errors
    ///
    /// Returns an error if waiting for the child fails.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().context("Failed to wait for child")
    }

    /// Creates an echo guard for command injection.
    ///
    /// The returned guard temporarily disables echo on the PTY to prevent
    /// injected commands from being echoed back. When the guard is dropped,
    /// echo is automatically restored.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal attributes cannot be read or modified.
    pub fn create_echo_guard(&self) -> Result<EchoGuard> {
        EchoGuard::new(self.master_fd())
    }
}

/// RAII guard for echo suppression during command injection.
///
/// When created, this guard disables the ECHO flag on the PTY terminal.
/// When dropped, it restores the original terminal settings. This prevents
/// injected commands from being echoed back to the terminal.
///
/// # Example
///
/// ```no_run
/// use wrashpty::pty::Pty;
///
/// let pty = Pty::spawn("/tmp/bashrc", 80, 24).unwrap();
/// {
///     let _guard = pty.create_echo_guard().unwrap();
///     // Echo is now disabled
///     // ... inject command ...
/// } // Guard drops here, echo is restored
/// ```
pub struct EchoGuard {
    /// The PTY file descriptor.
    pty_fd: RawFd,
    /// Original terminal settings to restore on drop.
    original_termios: Termios,
}

impl EchoGuard {
    /// Creates a new echo guard for the given PTY file descriptor.
    ///
    /// # Arguments
    ///
    /// * `pty_fd` - The PTY master file descriptor
    ///
    /// # Errors
    ///
    /// Returns an error if terminal attributes cannot be read or modified.
    ///
    /// # Safety
    ///
    /// The caller must ensure the file descriptor is valid for the lifetime
    /// of the EchoGuard.
    fn new(pty_fd: RawFd) -> Result<Self> {
        // SAFETY: The file descriptor comes from the PTY which is owned by App.
        // The EchoGuard is short-lived (only during command injection) and the
        // PTY outlives it.
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(pty_fd) };

        // Save original settings
        let original_termios =
            tcgetattr(borrowed_fd).context("Failed to get PTY terminal attributes")?;

        // Create modified settings with ECHO disabled
        let mut no_echo = original_termios.clone();
        no_echo.local_flags &= !LocalFlags::ECHO;

        // Apply the no-echo settings
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(pty_fd) };
        tcsetattr(borrowed_fd, SetArg::TCSANOW, &no_echo).context("Failed to disable PTY echo")?;

        debug!("Echo suppression enabled on PTY");

        Ok(Self {
            pty_fd,
            original_termios,
        })
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        // SAFETY: The file descriptor was valid when the guard was created,
        // and this is best-effort cleanup during Drop.
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(self.pty_fd) };

        // Restore original settings - ignore errors in Drop
        if let Err(e) = tcsetattr(borrowed_fd, SetArg::TCSANOW, &self.original_termios) {
            // Log at debug level since this is best-effort cleanup
            debug!("Failed to restore PTY echo (best-effort): {}", e);
        } else {
            debug!("Echo suppression disabled, original settings restored");
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        tracing::info!("Cleaning up PTY");
        // Actual child cleanup is handled automatically by portable-pty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_pty_spawn() {
        // Create temporary empty bashrc file
        let bashrc = NamedTempFile::new().expect("Failed to create temp file");
        let bashrc_path = bashrc.path().to_str().expect("Invalid path");

        // Spawn PTY with 80 cols, 24 rows
        let mut pty = Pty::spawn(bashrc_path, 80, 24).expect("Failed to spawn PTY");

        // Child should still be running
        let status = pty.try_wait().expect("Failed to check status");
        assert!(status.is_none(), "Child should still be running");

        // Send exit command
        pty.write_command("exit").expect("Failed to write command");

        // Wait for child to exit
        let exit_status = pty.wait().expect("Failed to wait for child");
        assert!(exit_status.success(), "Child should exit successfully");
    }

    #[test]
    fn test_pty_resize() {
        // Create temporary empty bashrc file
        let bashrc = NamedTempFile::new().expect("Failed to create temp file");
        let bashrc_path = bashrc.path().to_str().expect("Invalid path");

        // Spawn PTY with 80 cols, 24 rows
        let mut pty = Pty::spawn(bashrc_path, 80, 24).expect("Failed to spawn PTY");

        // Resize to new dimensions
        pty.resize(100, 40).expect("Failed to resize PTY");

        // Verify internal size was updated
        assert_eq!(pty.size.cols, 100);
        assert_eq!(pty.size.rows, 40);

        // Clean up: send exit command and wait
        pty.write_command("exit").expect("Failed to write command");
        let exit_status = pty.wait().expect("Failed to wait for child");
        assert!(exit_status.success(), "Child should exit successfully");
    }

    #[test]
    fn test_pty_master_fd() {
        // Create temporary empty bashrc file
        let bashrc = NamedTempFile::new().expect("Failed to create temp file");
        let bashrc_path = bashrc.path().to_str().expect("Invalid path");

        // Spawn PTY
        let mut pty = Pty::spawn(bashrc_path, 80, 24).expect("Failed to spawn PTY");

        // Get master FD - should be a valid positive integer
        let fd = pty.master_fd();
        assert!(fd >= 0, "File descriptor should be non-negative");

        // Clean up
        pty.write_command("exit").expect("Failed to write command");
        pty.wait().expect("Failed to wait for child");
    }

    #[test]
    fn test_multiple_write_commands() {
        // Create temporary empty bashrc file
        let bashrc = NamedTempFile::new().expect("Failed to create temp file");
        let bashrc_path = bashrc.path().to_str().expect("Invalid path");

        // Spawn PTY
        let mut pty = Pty::spawn(bashrc_path, 80, 24).expect("Failed to spawn PTY");

        // Try multiple sequential write_command calls
        pty.write_command("echo first")
            .expect("First write_command failed");
        pty.write_command("echo second")
            .expect("Second write_command failed");
        pty.write_command("exit")
            .expect("Exit write_command failed");

        let exit_status = pty.wait().expect("Failed to wait for child");
        assert!(exit_status.success(), "Child should exit successfully");
    }

    #[test]
    fn test_echo_guard_creation_succeeds() {
        // Create temporary empty bashrc file
        let bashrc = NamedTempFile::new().expect("Failed to create temp file");
        let bashrc_path = bashrc.path().to_str().expect("Invalid path");

        let mut pty = Pty::spawn(bashrc_path, 80, 24).expect("Failed to spawn PTY");

        // Create guard - should succeed
        {
            let _guard = pty
                .create_echo_guard()
                .expect("Failed to create echo guard");
            // Guard is active here - echo should be suppressed
        }
        // Guard dropped - echo should be restored

        // Clean up
        pty.write_command("exit").expect("Failed to write command");
        pty.wait().expect("Failed to wait for child");
    }

    #[test]
    fn test_echo_guard_multiple_sequential_guards() {
        // Create temporary empty bashrc file
        let bashrc = NamedTempFile::new().expect("Failed to create temp file");
        let bashrc_path = bashrc.path().to_str().expect("Invalid path");

        let mut pty = Pty::spawn(bashrc_path, 80, 24).expect("Failed to spawn PTY");

        // Create and drop multiple guards sequentially
        for i in 0..3 {
            let _guard = pty
                .create_echo_guard()
                .unwrap_or_else(|_| panic!("Failed to create echo guard iteration {}", i));
            // Guard drops at end of iteration
        }

        // PTY should still be functional
        pty.write_command("exit").expect("Failed to write command");
        let exit_status = pty.wait().expect("Failed to wait for child");
        assert!(exit_status.success(), "Child should exit successfully");
    }

    #[test]
    fn test_echo_guard_write_command_during_suppression() {
        // Create temporary empty bashrc file
        let bashrc = NamedTempFile::new().expect("Failed to create temp file");
        let bashrc_path = bashrc.path().to_str().expect("Invalid path");

        let mut pty = Pty::spawn(bashrc_path, 80, 24).expect("Failed to spawn PTY");

        // Create guard and write command while echo is suppressed
        {
            let _guard = pty
                .create_echo_guard()
                .expect("Failed to create echo guard");
            pty.write_command("echo test")
                .expect("Failed to write command with echo guard active");
        }
        // Guard drops here

        // Clean up
        pty.write_command("exit").expect("Failed to write command");
        let exit_status = pty.wait().expect("Failed to wait for child");
        assert!(exit_status.success(), "Child should exit successfully");
    }
}
