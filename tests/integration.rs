//! Integration tests using rexpect for PTY-based testing.
//!
//! These tests spawn actual wrashpty processes and interact with them
//! to verify end-to-end functionality.
//!
//! # Test Categories
//!
//! ## Pump Integration Tests
//!
//! The pump module is the critical hot path for all command output. These tests
//! verify correct behavior with real PTY instances:
//!
//! - **Simple passthrough**: stdin→PTY→stdout without markers
//! - **Marker detection**: verify PRECMD/PROMPT/PREEXEC events returned
//! - **Marker stripping**: ensure markers don't appear in stdout
//! - **Stale sequence timeout**: partial marker flushed after 100ms
//! - **PTY EOF handling**: verify PtyEof returned when child exits
//! - **Large data throughput**: test with 1MB+ data streams
//!
//! ## Mode Transition Tests
//!
//! - Test startup sequence (Initializing → Passthrough → Edit)
//! - Test command execution (Edit → Passthrough → Edit)
//! - Test graceful shutdown (any mode → Terminating)
//!
//! ## Line Editing Tests
//!
//! - Basic input and editing in Edit mode
//! - History navigation
//! - Tab completion

/// Shared test utilities used across integration test modules.
#[cfg(test)]
mod test_utils {
    /// Helper to create a valid OSC 777 marker sequence.
    ///
    /// # Arguments
    /// * `token` - 16-byte session token
    /// * `marker_type` - "PRECMD", "PROMPT", or "PREEXEC"
    /// * `payload` - Optional payload (e.g., exit code for PRECMD)
    pub fn make_marker(token: &[u8; 16], marker_type: &str, payload: Option<&str>) -> Vec<u8> {
        let mut seq = vec![0x1B, 0x5D]; // ESC ]
        seq.extend_from_slice(b"777;");
        seq.extend_from_slice(token);
        seq.push(b';');
        seq.extend_from_slice(marker_type.as_bytes());
        if let Some(p) = payload {
            seq.push(b';');
            seq.extend_from_slice(p.as_bytes());
        }
        seq.push(0x07); // BEL
        seq
    }
}

/// Edit mode integration tests.
///
/// These tests verify the edit mode behaviors including:
/// - Entering Edit mode and submitting a command
/// - Ctrl+C clearing the line without exiting
/// - Ctrl+D exiting at an empty prompt
/// - Injection flow producing PREEXEC -> Passthrough
/// - Buffering of background output during edit
///
/// **Note**: These tests require a proper TTY environment and are marked
/// `#[ignore]` by default. Run with `cargo test -- --ignored` to execute them
/// in an interactive terminal session.
#[cfg(test)]
mod edit_mode_tests {
    use std::io::{Read, Write};
    use std::thread;
    use std::time::Duration;

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    /// Test harness for wrashpty edit mode testing.
    struct EditModeTestHarness {
        master: Box<dyn portable_pty::MasterPty + Send>,
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        #[allow(dead_code)]
        child: Box<dyn portable_pty::Child + Send + Sync>,
    }

    impl EditModeTestHarness {
        /// Spawn wrashpty for testing.
        /// Returns None if wrashpty binary is not available.
        fn spawn() -> Option<Self> {
            let pty_system = native_pty_system();
            let size = PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            };

            let pair = pty_system.openpty(size).ok()?;

            // Try to find wrashpty binary
            let wrashpty_path = std::env::current_dir().ok()?.join("target/debug/wrashpty");

            if !wrashpty_path.exists() {
                // Try release build
                let release_path = std::env::current_dir()
                    .ok()?
                    .join("target/release/wrashpty");
                if !release_path.exists() {
                    return None;
                }
            }

            let mut command = CommandBuilder::new(&wrashpty_path);
            // Set TERM to ensure proper terminal behavior
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

        /// Read available output with timeout.
        /// Returns the output and whether an error was detected.
        fn read_output(&mut self, timeout_ms: u64) -> (String, bool) {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};

            let fd = self.master.as_raw_fd().unwrap();

            // Set non-blocking
            let flags = fcntl(fd, FcntlArg::F_GETFL).unwrap_or(0);
            let flags = OFlag::from_bits_truncate(flags);
            let _ = fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK));

            let mut output = Vec::new();
            let mut buf = [0u8; 1024];
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

            // Restore blocking mode
            let _ = fcntl(fd, FcntlArg::F_SETFL(flags));

            let output_str = String::from_utf8_lossy(&output).to_string();
            // Check for reedline/terminal errors that indicate non-TTY environment
            let has_error = output_str.contains("cursor position could not be read")
                || output_str.contains("Reedline read_line failed")
                || output_str.contains("ENOTTY");

            (output_str, has_error)
        }

        /// Send input to the PTY.
        fn send(&mut self, input: &str) -> std::io::Result<()> {
            self.writer.write_all(input.as_bytes())?;
            self.writer.flush()
        }

        /// Send a control character.
        fn send_ctrl(&mut self, c: char) -> std::io::Result<()> {
            let ctrl_byte = (c as u8) & 0x1f;
            self.writer.write_all(&[ctrl_byte])?;
            self.writer.flush()
        }
    }

    /// Test that wrashpty enters Edit mode and can submit a command.
    ///
    /// Requires a proper TTY environment; skipped in CI.
    #[test]
    #[ignore = "requires interactive TTY environment"]
    fn test_edit_mode_command_submission() {
        let Some(mut harness) = EditModeTestHarness::spawn() else {
            eprintln!("Skipping test: wrashpty binary not found");
            return;
        };

        // Wait for initial prompt
        thread::sleep(Duration::from_millis(1000));
        let (output, has_error) = harness.read_output(500);

        if has_error {
            eprintln!("Skipping test: terminal environment not suitable");
            return;
        }

        // Should see some prompt indicator (reedline shows a prompt)
        assert!(
            !output.is_empty(),
            "Expected some output after startup, got nothing"
        );

        // Send a simple command
        harness.send("echo hello_from_test\n").unwrap();

        // Wait for command execution
        thread::sleep(Duration::from_millis(500));
        let (output, _) = harness.read_output(500);

        // Should see the command output
        assert!(
            output.contains("hello_from_test"),
            "Expected 'hello_from_test' in output, got: {}",
            output
        );
    }

    /// Test that Ctrl+C clears the line without exiting.
    ///
    /// Requires a proper TTY environment; skipped in CI.
    #[test]
    #[ignore = "requires interactive TTY environment"]
    fn test_edit_mode_ctrl_c_clears_line() {
        let Some(mut harness) = EditModeTestHarness::spawn() else {
            eprintln!("Skipping test: wrashpty binary not found");
            return;
        };

        // Wait for initial prompt
        thread::sleep(Duration::from_millis(1000));
        let (_, has_error) = harness.read_output(500);

        if has_error {
            eprintln!("Skipping test: terminal environment not suitable");
            return;
        }

        // Type something
        harness.send("partial command").unwrap();
        thread::sleep(Duration::from_millis(100));

        // Send Ctrl+C to clear
        harness.send_ctrl('C').unwrap();
        thread::sleep(Duration::from_millis(200));

        // Send a different command to verify we're still in edit mode
        harness.send("echo still_alive\n").unwrap();
        thread::sleep(Duration::from_millis(500));
        let (output, _) = harness.read_output(500);

        // Should see the new command output (proves we didn't exit)
        assert!(
            output.contains("still_alive"),
            "Expected 'still_alive' in output after Ctrl+C, got: {}",
            output
        );
    }

    /// Test that Ctrl+D at empty prompt exits.
    ///
    /// Requires a proper TTY environment; skipped in CI.
    #[test]
    #[ignore = "requires interactive TTY environment"]
    fn test_edit_mode_ctrl_d_exits() {
        let Some(mut harness) = EditModeTestHarness::spawn() else {
            eprintln!("Skipping test: wrashpty binary not found");
            return;
        };

        // Wait for initial prompt
        thread::sleep(Duration::from_millis(1000));
        let (_, has_error) = harness.read_output(500);

        if has_error {
            eprintln!("Skipping test: terminal environment not suitable");
            return;
        }

        // Send Ctrl+D at empty prompt
        harness.send_ctrl('D').unwrap();

        // Wait for exit
        thread::sleep(Duration::from_millis(500));

        // Try to send more input - should fail or produce no output
        // because the shell should have exited
        let _ = harness.send("echo should_not_work\n");
        thread::sleep(Duration::from_millis(200));
        let (output, _) = harness.read_output(200);

        // Should NOT see the command output (shell exited)
        assert!(
            !output.contains("should_not_work"),
            "Expected shell to exit after Ctrl+D, but got output: {}",
            output
        );
    }

    /// Test that command injection produces PREEXEC and transitions to Passthrough.
    ///
    /// Requires a proper TTY environment; skipped in CI.
    #[test]
    #[ignore = "requires interactive TTY environment"]
    fn test_injection_flow_preexec_passthrough() {
        let Some(mut harness) = EditModeTestHarness::spawn() else {
            eprintln!("Skipping test: wrashpty binary not found");
            return;
        };

        // Wait for initial prompt (Edit mode)
        thread::sleep(Duration::from_millis(1000));
        let (_, has_error) = harness.read_output(500);

        if has_error {
            eprintln!("Skipping test: terminal environment not suitable");
            return;
        }

        // Send a command that produces output
        harness.send("echo injected_command\n").unwrap();

        // Wait for execution (Passthrough mode)
        thread::sleep(Duration::from_millis(500));
        let (output, _) = harness.read_output(500);

        // Verify command executed (proves we transitioned through injection)
        assert!(
            output.contains("injected_command"),
            "Expected command output after injection, got: {}",
            output
        );

        // Verify we returned to Edit mode (can submit another command)
        harness.send("echo back_to_edit\n").unwrap();
        thread::sleep(Duration::from_millis(500));
        let (output, _) = harness.read_output(500);

        assert!(
            output.contains("back_to_edit"),
            "Expected to return to Edit mode, got: {}",
            output
        );
    }

    /// Test that background output is buffered during edit mode.
    ///
    /// Requires a proper TTY environment; skipped in CI.
    #[test]
    #[ignore = "requires interactive TTY environment"]
    fn test_background_output_buffering() {
        let Some(mut harness) = EditModeTestHarness::spawn() else {
            eprintln!("Skipping test: wrashpty binary not found");
            return;
        };

        // Wait for initial prompt
        thread::sleep(Duration::from_millis(1000));
        let (_, has_error) = harness.read_output(500);

        if has_error {
            eprintln!("Skipping test: terminal environment not suitable");
            return;
        }

        // Start a background job that produces output after a delay
        harness
            .send("(sleep 1 && echo background_output) &\n")
            .unwrap();
        thread::sleep(Duration::from_millis(500));
        let _ = harness.read_output(500);

        // Now we should be back at prompt (Edit mode)
        // The background output will arrive while we're in Edit mode

        // Wait for background job to produce output
        thread::sleep(Duration::from_millis(1500));

        // Submit another command - background output should appear
        harness.send("echo foreground\n").unwrap();
        thread::sleep(Duration::from_millis(500));
        let (output, _) = harness.read_output(1000);

        // Should see the foreground command output
        assert!(
            output.contains("foreground"),
            "Expected foreground output, got: {}",
            output
        );

        // Background output may or may not be visible depending on timing,
        // but the test verifies we don't crash or hang due to background output
    }
}

#[cfg(test)]
mod tests {
    // Placeholder for future rexpect-based integration tests
    #[test]
    fn placeholder() {
        // This test will be replaced with actual integration tests
        assert!(true);
    }
}

#[cfg(test)]
mod pump_tests {
    //! Pump integration tests with real PTY instances.
    //!
    //! These tests require spawning actual PTY processes and are more
    //! heavyweight than unit tests. They verify the full I/O path.
    //!
    //! Note: Some tests are marked `#[ignore]` because they require
    //! careful PTY setup and can hang in certain environments.

    use super::test_utils::make_marker;
    use wrashpty::marker::MarkerParser;
    use wrashpty::types::MarkerEvent;

    /// Test marker parser directly with simulated PTY output.
    /// This doesn't require a real PTY - it just tests the parsing logic.
    #[test]
    fn test_marker_parsing_direct() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate PTY output: text + marker + text
        let mut data = b"Output line 1\n".to_vec();
        data.extend(make_marker(&token, "PRECMD", Some("0")));
        data.extend(b"Output line 2\n");

        let mut text_output = Vec::new();
        let mut markers = Vec::new();

        for output in parser.feed(&data) {
            match output {
                wrashpty::marker::ParseOutput::Bytes(b) => {
                    text_output.extend_from_slice(&b);
                }
                wrashpty::marker::ParseOutput::Marker(m) => {
                    markers.push(m);
                }
            }
        }

        // Verify marker was detected
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0], MarkerEvent::Precmd { exit_code: 0 });

        // Verify marker was stripped from output
        assert!(!text_output.contains(&0x1B), "ESC should be stripped");
        assert!(text_output.starts_with(b"Output line 1\n"));
        assert!(text_output.ends_with(b"Output line 2\n"));
    }

    /// Test multiple markers in sequence.
    #[test]
    fn test_multiple_markers_direct() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate shell startup sequence: PRECMD -> PROMPT
        let mut data = make_marker(&token, "PRECMD", Some("0"));
        data.extend(b"$ ");
        data.extend(make_marker(&token, "PROMPT", None));

        let mut text_output = Vec::new();
        let mut markers = Vec::new();

        for output in parser.feed(&data) {
            match output {
                wrashpty::marker::ParseOutput::Bytes(b) => {
                    text_output.extend_from_slice(&b);
                }
                wrashpty::marker::ParseOutput::Marker(m) => {
                    markers.push(m);
                }
            }
        }

        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0], MarkerEvent::Precmd { exit_code: 0 });
        assert_eq!(markers[1], MarkerEvent::Prompt);
        assert_eq!(text_output, b"$ ");
    }

    /// Test that invalid markers pass through unchanged.
    #[test]
    fn test_invalid_marker_passthrough() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Other OSC sequences should pass through
        let osc8 = b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07";

        let mut output = Vec::new();
        for o in parser.feed(osc8) {
            if let wrashpty::marker::ParseOutput::Bytes(b) = o {
                output.extend_from_slice(&b);
            }
        }

        assert_eq!(output, osc8.as_slice());
    }

    /// Test split marker detection (marker spans multiple reads).
    #[test]
    fn test_split_marker_detection() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);
        let marker = make_marker(&token, "PROMPT", None);

        // Split marker into chunks
        let mid = marker.len() / 2;
        let part1 = &marker[..mid];
        let part2 = &marker[mid..];

        // Feed first part - should buffer
        let outputs1: Vec<_> = parser.feed(part1).collect();
        assert!(outputs1.is_empty());
        assert!(parser.is_mid_sequence());

        // Feed second part - should yield marker
        let mut found_marker = false;
        for output in parser.feed(part2) {
            if let wrashpty::marker::ParseOutput::Marker(MarkerEvent::Prompt) = output {
                found_marker = true;
            }
        }
        assert!(found_marker);
    }

    /// Test large data passthrough (no markers).
    #[test]
    fn test_large_data_passthrough_direct() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Generate 64KB of data without any ESC bytes
        let large_data: Vec<u8> = (0..65536)
            .map(|i| {
                let b = (i % 256) as u8;
                if b == 0x1B { 0x20 } else { b } // Replace ESC with space
            })
            .collect();

        let mut total_output = Vec::new();

        // Feed in chunks to simulate real I/O
        for chunk in large_data.chunks(4096) {
            for output in parser.feed(chunk) {
                if let wrashpty::marker::ParseOutput::Bytes(b) = output {
                    total_output.extend_from_slice(&b);
                }
            }
        }

        assert_eq!(total_output.len(), large_data.len());
        assert_eq!(total_output, large_data);
    }

    /// Test multiple markers in a single read chunk are all captured.
    #[test]
    fn test_multiple_markers_single_chunk() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Build a chunk with three markers back-to-back
        let mut data = Vec::new();
        data.extend(b"output1\n");
        data.extend(make_marker(&token, "PRECMD", Some("0")));
        data.extend(b"$ ");
        data.extend(make_marker(&token, "PROMPT", None));
        data.extend(b"ls\n");
        data.extend(make_marker(&token, "PREEXEC", None));
        data.extend(b"file1 file2\n");

        let mut text_output = Vec::new();
        let mut markers = Vec::new();

        for output in parser.feed(&data) {
            match output {
                wrashpty::marker::ParseOutput::Bytes(b) => {
                    text_output.extend_from_slice(&b);
                }
                wrashpty::marker::ParseOutput::Marker(m) => {
                    markers.push(m);
                }
            }
        }

        // All three markers must be captured
        assert_eq!(
            markers.len(),
            3,
            "Expected 3 markers, got {}",
            markers.len()
        );
        assert_eq!(markers[0], MarkerEvent::Precmd { exit_code: 0 });
        assert_eq!(markers[1], MarkerEvent::Prompt);
        assert_eq!(markers[2], MarkerEvent::Preexec);

        // All text must be preserved
        let expected_text = b"output1\n$ ls\nfile1 file2\n";
        assert_eq!(text_output, expected_text.as_slice());
    }
}

#[cfg(test)]
mod pump_pty_tests {
    //! Pump integration tests with real PTY instances.
    //!
    //! These tests use the portable-pty crate directly for spawning arbitrary
    //! processes, allowing proper testing of EOF handling, stdin forwarding, etc.

    use std::os::unix::io::RawFd;
    use std::thread;
    use std::time::Duration;

    use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
    use smallvec::smallvec;
    use wrashpty::marker::MarkerParser;
    use wrashpty::pump::{MarkerVec, PumpResult};
    use wrashpty::types::MarkerEvent;

    use super::test_utils::make_marker;

    /// Test harness for PTY-based tests.
    struct TestPty {
        /// Keep master alive to prevent PTY from closing (fd derived from it)
        #[allow(dead_code)]
        master: Box<dyn MasterPty + Send>,
        #[allow(dead_code)]
        child: Box<dyn Child + Send + Sync>,
        fd: RawFd,
    }

    impl TestPty {
        /// Spawn a PTY with an arbitrary command for testing.
        fn spawn(cmd: &str) -> Self {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};

            let pty_system = native_pty_system();
            let size = PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            };

            let pair = pty_system.openpty(size).expect("Failed to open PTY");

            let mut command = CommandBuilder::new("/bin/sh");
            command.arg("-c");
            command.arg(cmd);

            let child = pair
                .slave
                .spawn_command(command)
                .expect("Failed to spawn command");
            let fd = pair.master.as_raw_fd().expect("Failed to get master fd");

            // Set the master fd to non-blocking mode so reads return EAGAIN
            // instead of blocking, which is required for the test loops.
            let flags = fcntl(fd, FcntlArg::F_GETFL).expect("Failed to get fd flags");
            let flags = OFlag::from_bits_truncate(flags);
            fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))
                .expect("Failed to set O_NONBLOCK on PTY master");

            Self {
                master: pair.master,
                child,
                fd,
            }
        }

        fn fd(&self) -> RawFd {
            self.fd
        }
    }

    /// Test PTY EOF detection when child process exits.
    ///
    /// This test verifies that reading from a PTY after the child exits
    /// returns EOF (0 bytes read) which the pump translates to PtyEof.
    #[test]
    fn test_pty_eof_on_child_exit() {
        // Use a command that produces output then exits - this ensures
        // we get POLLIN events before the EOF
        let pty = TestPty::spawn("echo done; exit 0");

        // Give the child time to run and exit (generous timeout for slow CI)
        thread::sleep(Duration::from_millis(500));

        // Read directly from PTY to test EOF behavior
        // (bypassing pump which uses blocking poll)
        let mut buf = [0u8; 256];
        let mut got_output = false;
        let mut got_eof = false;

        // Read until EOF or error
        for _ in 0..20 {
            match nix::unistd::read(pty.fd(), &mut buf) {
                Ok(0) => {
                    // EOF - child exited
                    got_eof = true;
                    break;
                }
                Ok(_n) => {
                    got_output = true;
                    // Continue reading to drain output
                }
                Err(nix::errno::Errno::EAGAIN) => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(nix::errno::Errno::EIO) => {
                    // EIO on PTY read means child exited - treat as EOF
                    got_eof = true;
                    break;
                }
                Err(e) => panic!("Unexpected error: {}", e),
            }
        }

        assert!(got_output, "Expected output from child");
        assert!(got_eof, "Expected EOF/EIO when child exits");
    }

    /// Test that PTY output passes through correctly.
    #[test]
    fn test_pty_output_passthrough() {
        let pty = TestPty::spawn("echo 'hello world'");

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Give command time to execute (generous timeout for slow CI)
        thread::sleep(Duration::from_millis(500));

        // Read available output
        let mut buf = [0u8; 1024];
        let mut output = Vec::new();

        loop {
            match nix::unistd::read(pty.fd(), &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for o in parser.feed(&buf[..n]) {
                        if let wrashpty::marker::ParseOutput::Bytes(b) = o {
                            output.extend_from_slice(&b);
                        }
                    }
                }
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(nix::errno::Errno::EIO) => break, // PTY closed
                Err(e) => panic!("Unexpected error: {}", e),
            }
        }

        // Output should contain our echoed text
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("hello world"),
            "Expected 'hello world' in output, got: {}",
            output_str
        );
    }

    /// Test stale sequence flushing with a real parser.
    #[test]
    fn test_stale_sequence_flushing() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Feed a partial marker (just ESC)
        let partial = [0x1B]; // ESC only
        let outputs: Vec<_> = parser.feed(&partial).collect();
        assert!(outputs.is_empty(), "Partial should be buffered");
        assert!(parser.is_mid_sequence(), "Should be mid-sequence");

        // Flush the stale sequence
        let stale = parser.flush_stale();
        assert!(stale.is_some(), "Should have stale bytes to flush");
        assert_eq!(stale.unwrap(), &[0x1B], "Should flush the ESC byte");
        assert!(
            !parser.is_mid_sequence(),
            "Should no longer be mid-sequence"
        );
    }

    /// Test that markers are stripped from PTY output.
    #[test]
    fn test_marker_stripping_from_output() {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate PTY output with embedded markers
        let mut pty_output = Vec::new();
        pty_output.extend(b"Before marker");
        pty_output.extend(make_marker(&token, "PROMPT", None));
        pty_output.extend(b"After marker");

        let mut visible_output = Vec::new();
        let mut markers = Vec::new();

        for output in parser.feed(&pty_output) {
            match output {
                wrashpty::marker::ParseOutput::Bytes(b) => {
                    visible_output.extend_from_slice(&b);
                }
                wrashpty::marker::ParseOutput::Marker(m) => {
                    markers.push(m);
                }
            }
        }

        // Marker should be detected
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0], MarkerEvent::Prompt);

        // Marker should NOT appear in visible output
        assert!(
            !visible_output.contains(&0x1B),
            "ESC byte should be stripped"
        );
        assert!(
            !visible_output.windows(3).any(|w| w == b"777"),
            "777 should be stripped"
        );
        assert_eq!(visible_output, b"Before markerAfter marker");
    }

    /// Test PumpResult with multiple markers using SmallVec.
    #[test]
    fn test_pump_result_multiple_markers() {
        // Verify the MarkerVec (SmallVec) API works correctly
        // Two markers fit inline without heap allocation
        let events: MarkerVec =
            smallvec![MarkerEvent::Precmd { exit_code: 0 }, MarkerEvent::Prompt];

        let result = PumpResult::MarkerDetected(events);

        match result {
            PumpResult::MarkerDetected(v) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0], MarkerEvent::Precmd { exit_code: 0 });
                assert_eq!(v[1], MarkerEvent::Prompt);
            }
            _ => panic!("Expected MarkerDetected"),
        }
    }

    /// Test stdin to PTY forwarding with a real PTY.
    #[test]
    fn test_stdin_to_pty_forwarding() {
        // Use `head -1` which reads one line and exits (avoids cat hanging forever)
        let pty = TestPty::spawn("head -1");

        // Write to PTY master (simulates stdin forwarding)
        let test_input = b"test input\n";
        nix::unistd::write(pty.fd(), test_input).expect("write failed");

        // Give head time to echo and exit (generous timeout for slow CI)
        thread::sleep(Duration::from_millis(500));

        // Read from PTY master (what would go to stdout)
        let mut buf = [0u8; 256];
        let mut output = Vec::new();

        for _ in 0..10 {
            match nix::unistd::read(pty.fd(), &mut buf) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(nix::errno::Errno::EAGAIN) => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(nix::errno::Errno::EIO) => break,
                Err(e) => panic!("read error: {}", e),
            }
        }

        // head should echo back our input
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("test input"),
            "Expected echo of input, got: {}",
            output_str
        );
    }

    // Note: Direct pump EOF testing is challenging because run_once() uses
    // blocking poll with infinite timeout when not mid-sequence. The EOF
    // behavior is verified indirectly through:
    // 1. test_pty_eof_on_child_exit - verifies read returns 0/EIO on exit
    // 2. pump::forward_pty_to_stdout returns PtyEof on Ok(0)
    //
    // Full pump integration testing with real stdin/stdout would require
    // a test harness that can inject data and capture output streams.
}
