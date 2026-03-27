//! PTY-driven integration tests for scroll view interactions.

use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

struct ScrollViewHarness {
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    #[allow(dead_code)]
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl ScrollViewHarness {
    fn spawn(cols: u16, rows: u16) -> Option<Self> {
        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = pty_system.openpty(size).ok()?;
        let debug_bin = std::env::current_dir().ok()?.join("target/debug/wrashpty");
        let release_bin = std::env::current_dir()
            .ok()?
            .join("target/release/wrashpty");
        let binary = if debug_bin.exists() {
            debug_bin
        } else if release_bin.exists() {
            release_bin
        } else {
            return None;
        };

        let mut command = CommandBuilder::new(&binary);
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command).ok()?;
        let reader = pair.master.try_clone_reader().ok()?;
        let writer = pair.master.take_writer().ok()?;

        Some(Self {
            master: pair.master,
            reader,
            writer,
            child,
        })
    }

    fn send(&mut self, input: &str) -> std::io::Result<()> {
        self.writer.write_all(input.as_bytes())?;
        self.writer.flush()
    }

    fn send_bytes(&mut self, input: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(input)?;
        self.writer.flush()
    }

    fn read_sanitized_output(&mut self, timeout_ms: u64) -> String {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};

        let fd = self.master.as_raw_fd().expect("PTY master fd");
        let flags = fcntl(fd, FcntlArg::F_GETFL).unwrap_or(0);
        let flags = OFlag::from_bits_truncate(flags);
        let _ = fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK));

        let mut output = Vec::new();
        let mut buf = [0u8; 4096];
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            match self.reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }

        let _ = fcntl(fd, FcntlArg::F_SETFL(flags));
        String::from_utf8_lossy(&wrashpty::scrollback::sanitize_for_display(&output)).to_string()
    }
}

#[test]
#[ignore = "requires PTY + fullscreen scroll mode environment"]
fn test_scroll_view_z_fold_when_long_output_shows_fold_badge() {
    let Some(mut harness) = ScrollViewHarness::spawn(80, 24) else {
        eprintln!("Skipping test: wrashpty binary not found");
        return;
    };

    thread::sleep(Duration::from_millis(1200));
    let startup = harness.read_sanitized_output(600);
    if startup.contains("Reedline read_line failed") || startup.contains("ENOTTY") {
        eprintln!("Skipping test: terminal environment not suitable");
        return;
    }

    harness
        .send("for i in $(seq 1 60); do echo fold_line_$i; done\n")
        .unwrap();
    thread::sleep(Duration::from_millis(1200));
    let _ = harness.read_sanitized_output(800);

    harness.send_bytes(b"\x1b[5~").unwrap(); // PageUp: enter scroll view
    thread::sleep(Duration::from_millis(300));
    let _ = harness.read_sanitized_output(600);

    harness.send("z").unwrap();
    thread::sleep(Duration::from_millis(300));
    let output = harness.read_sanitized_output(1000);

    assert!(
        output.contains("lines]"),
        "Expected fold badge in scroll view output, got: {output:?}"
    );
}

#[test]
#[ignore = "requires PTY + fullscreen scroll mode environment"]
fn test_scroll_view_legend_when_terminal_narrow_truncates_low_priority_entries() {
    let Some(mut harness) = ScrollViewHarness::spawn(24, 24) else {
        eprintln!("Skipping test: wrashpty binary not found");
        return;
    };

    thread::sleep(Duration::from_millis(1200));
    let startup = harness.read_sanitized_output(600);
    if startup.contains("Reedline read_line failed") || startup.contains("ENOTTY") {
        eprintln!("Skipping test: terminal environment not suitable");
        return;
    }

    harness
        .send("for i in $(seq 1 50); do echo legend_line_$i; done\n")
        .unwrap();
    thread::sleep(Duration::from_millis(1000));
    let _ = harness.read_sanitized_output(600);

    harness.send_bytes(b"\x1b[5~").unwrap(); // PageUp
    thread::sleep(Duration::from_millis(300));
    let _ = harness.read_sanitized_output(600);

    harness.send("?").unwrap(); // toggle legend bar
    thread::sleep(Duration::from_millis(300));
    let output = harness.read_sanitized_output(1000);

    assert!(
        output.contains("PgUp/Dn"),
        "Expected high-priority legend entry in narrow view, got: {output:?}"
    );
    assert!(
        !output.contains("Esc Exit"),
        "Expected overflow truncation to drop low-priority legend entries, got: {output:?}"
    );
}
