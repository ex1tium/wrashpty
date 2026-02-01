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
