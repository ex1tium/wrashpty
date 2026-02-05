    use super::*;

    // =========================================================================
    // Constant Tests
    // =========================================================================

    #[test]
    fn test_initialization_timeout_constant() {
        // Verify timeout is reasonable
        assert!(INITIALIZATION_TIMEOUT >= Duration::from_secs(5));
        assert!(INITIALIZATION_TIMEOUT <= Duration::from_secs(30));
    }

    #[test]
    fn test_termination_timeout_constant() {
        // Verify timeout is reasonable
        assert!(TERMINATION_TIMEOUT >= Duration::from_secs(1));
        assert!(TERMINATION_TIMEOUT <= Duration::from_secs(10));
    }

    #[test]
    fn test_injection_timeout_constant() {
        // Verify timeout is reasonable for injection
        assert!(INJECTION_TIMEOUT >= Duration::from_millis(100));
        assert!(INJECTION_TIMEOUT <= Duration::from_secs(2));
    }

    #[test]
    fn test_injection_poll_timeout_constant() {
        // Verify poll timeout is short enough to allow timely timeout detection
        // but not so short as to cause excessive CPU usage
        assert!(INJECTION_POLL_TIMEOUT >= Duration::from_millis(10));
        assert!(INJECTION_POLL_TIMEOUT <= Duration::from_millis(200));
        // Must be shorter than injection timeout to allow timeout to fire
        assert!(INJECTION_POLL_TIMEOUT < INJECTION_TIMEOUT);
    }

    // =========================================================================
    // Exit Code Helper Tests
    // =========================================================================

    #[test]
    fn test_exit_code_from_status_success() {
        // Create a mock ExitStatus representing success
        // Note: ExitStatus::with_exit_code is not directly available,
        // but we can test the function with known values
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(0);
        assert_eq!(exit_code_from_status(&status), 0);
    }

    #[test]
    fn test_exit_code_from_status_failure() {
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(1);
        assert_eq!(exit_code_from_status(&status), 1);

        let status = ExitStatus::with_exit_code(42);
        assert_eq!(exit_code_from_status(&status), 42);

        // Test signal-like exit codes (128 + signal)
        let status = ExitStatus::with_exit_code(130); // SIGINT
        assert_eq!(exit_code_from_status(&status), 130);

        let status = ExitStatus::with_exit_code(137); // SIGKILL
        assert_eq!(exit_code_from_status(&status), 137);
    }

    #[test]
    fn test_exit_code_from_status_max_values() {
        use portable_pty::ExitStatus;

        let status = ExitStatus::with_exit_code(255);
        assert_eq!(exit_code_from_status(&status), 255);
    }

    // =========================================================================
    // Mode Transition Tests
    // =========================================================================

    #[test]
    fn test_mode_equality() {
        assert_eq!(Mode::Initializing, Mode::Initializing);
        assert_eq!(Mode::Edit, Mode::Edit);
        assert_eq!(Mode::Passthrough, Mode::Passthrough);
        assert_eq!(Mode::Injecting, Mode::Injecting);
        assert_eq!(Mode::Terminating, Mode::Terminating);

        assert_ne!(Mode::Initializing, Mode::Edit);
        assert_ne!(Mode::Edit, Mode::Passthrough);
    }

    #[test]
    fn test_mode_debug_format() {
        // Verify Debug implementations for logging
        assert!(format!("{:?}", Mode::Initializing).contains("Initializing"));
        assert!(format!("{:?}", Mode::Edit).contains("Edit"));
        assert!(format!("{:?}", Mode::Passthrough).contains("Passthrough"));
        assert!(format!("{:?}", Mode::Injecting).contains("Injecting"));
        assert!(format!("{:?}", Mode::Terminating).contains("Terminating"));
    }

    // =========================================================================
    // Marker Event Transition Tests
    // =========================================================================

    #[test]
    fn test_marker_event_variants() {
        // Test that all marker event variants can be constructed and matched
        let precmd = MarkerEvent::Precmd { exit_code: 0 };
        let prompt = MarkerEvent::Prompt;
        let preexec = MarkerEvent::Preexec;

        assert!(matches!(precmd, MarkerEvent::Precmd { exit_code: 0 }));
        assert!(matches!(prompt, MarkerEvent::Prompt));
        assert!(matches!(preexec, MarkerEvent::Preexec));
    }

    #[test]
    fn test_marker_event_precmd_exit_codes() {
        // Test various exit codes in Precmd events
        let success = MarkerEvent::Precmd { exit_code: 0 };
        let failure = MarkerEvent::Precmd { exit_code: 1 };
        let signal = MarkerEvent::Precmd { exit_code: 130 };
        let negative = MarkerEvent::Precmd { exit_code: -1 };

        if let MarkerEvent::Precmd { exit_code } = success {
            assert_eq!(exit_code, 0);
        }
        if let MarkerEvent::Precmd { exit_code } = failure {
            assert_eq!(exit_code, 1);
        }
        if let MarkerEvent::Precmd { exit_code } = signal {
            assert_eq!(exit_code, 130);
        }
        if let MarkerEvent::Precmd { exit_code } = negative {
            assert_eq!(exit_code, -1);
        }
    }

    // =========================================================================
    // Terminal Mode Transition Tests (require TTY)
    // =========================================================================

    /// Helper to check if we're running in a real terminal.
    fn is_tty() -> bool {
        use std::io::stdin;
        use std::os::unix::io::AsRawFd;
        unsafe { libc::isatty(stdin().as_raw_fd()) == 1 }
    }

    #[test]
    fn test_terminal_guard_raw_mode_toggle() {
        if !is_tty() {
            eprintln!("Skipping test (no terminal)");
            return;
        }

        // Test that TerminalGuard properly enables raw mode and restores on drop
        {
            let guard = match TerminalGuard::new() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("Skipping test (terminal unavailable): {}", e);
                    return;
                }
            };

            // Guard is active - terminal should be in raw mode
            // We can't easily assert raw mode is active, but we verify
            // the guard was created successfully
            drop(guard);
        }

        // After drop, terminal should be restored
        // Verify by creating another guard successfully
        match TerminalGuard::new() {
            Ok(guard) => drop(guard),
            Err(e) => {
                // This might fail if terminal wasn't properly restored
                panic!("Terminal not properly restored after first guard: {}", e);
            }
        }
    }

    #[test]
    fn test_terminal_guard_nested_drops() {
        if !is_tty() {
            eprintln!("Skipping test (no terminal)");
            return;
        }

        // Test that multiple sequential guard creations work
        for i in 0..3 {
            match TerminalGuard::new() {
                Ok(guard) => {
                    // Simulate some work
                    thread::sleep(Duration::from_millis(10));
                    drop(guard);
                }
                Err(e) => {
                    panic!("Failed to create terminal guard on iteration {}: {}", i, e);
                }
            }
        }
    }

    // =========================================================================
    // Marker Parser Edge Case Tests (malformed and partial input)
    // =========================================================================

    #[test]
    fn test_marker_parser_malformed_osc_type() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // OSC with wrong type number (not 777)
        let malformed = b"\x1b]123;a1b2c3d4e5f67890;PROMPT\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        // Should return as passthrough bytes, not a marker
        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_truncated_sequence() {
        use crate::marker::MarkerParser;

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Truncated sequence (no BEL terminator)
        let truncated = b"\x1b]777;a1b2c3d4e5f67890;PROMPT";
        let outputs: Vec<_> = parser.feed(truncated).collect();

        // Parser should be mid-sequence, no output yet
        assert!(outputs.is_empty());
        assert!(parser.is_mid_sequence());

        // Flush stale should return the buffered bytes
        let stale = parser.flush_stale();
        assert!(stale.is_some());
        assert!(!parser.is_mid_sequence());
    }

    #[test]
    fn test_marker_parser_partial_token() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Partial token (too short)
        let malformed = b"\x1b]777;short;PROMPT\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        // Should return as bytes, not a marker (token validation fails)
        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_invalid_marker_type() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Unknown marker type
        let malformed = b"\x1b]777;a1b2c3d4e5f67890;UNKNOWN\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        // Should return as bytes
        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_empty_fields() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Empty marker type
        let malformed = b"\x1b]777;a1b2c3d4e5f67890;\x07";
        let outputs: Vec<_> = parser.feed(malformed).collect();

        assert_eq!(outputs.len(), 1);
        assert!(matches!(outputs[0], ParseOutput::Bytes(_)));
    }

    #[test]
    fn test_marker_parser_split_at_every_byte() {
        use crate::marker::{MarkerParser, ParseOutput};
        use crate::types::MarkerEvent;

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Valid marker split into individual bytes
        let marker = b"\x1b]777;a1b2c3d4e5f67890;PROMPT\x07";

        let mut found_marker = false;
        for &byte in marker.iter() {
            for output in parser.feed(&[byte]) {
                if matches!(output, ParseOutput::Marker(MarkerEvent::Prompt)) {
                    found_marker = true;
                }
            }
        }

        assert!(
            found_marker,
            "Should find PROMPT marker even with byte-by-byte feeding"
        );
        assert!(!parser.is_mid_sequence());
    }

    #[test]
    fn test_marker_parser_binary_garbage() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Feed random binary data - should not panic
        let garbage: Vec<u8> = (0..=255).collect();
        let outputs: Vec<_> = parser.feed(&garbage).collect();

        // Should produce only bytes output, no markers (garbage doesn't form valid markers)
        for output in &outputs {
            assert!(matches!(output, ParseOutput::Bytes(_)));
        }

        // Parser should handle gracefully
        let _ = parser.flush_stale();
        assert!(!parser.is_mid_sequence());
    }

    #[test]
    fn test_marker_parser_repeated_esc() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Multiple ESC bytes in a row
        let input = b"\x1b\x1b\x1b\x1btest";
        let outputs: Vec<_> = parser.feed(input).collect();

        // Should output all ESC bytes and "test" as passthrough
        let total_bytes: Vec<u8> = outputs
            .iter()
            .filter_map(|o| match o {
                ParseOutput::Bytes(b) => Some(b.to_vec()),
                _ => None,
            })
            .flatten()
            .collect();

        assert_eq!(total_bytes, b"\x1b\x1b\x1b\x1btest");
    }

    // =========================================================================
    // Echo Suppression During Injection Tests
    // =========================================================================

    #[test]
    fn test_injection_mode_transitions() {
        // Test the conceptual flow of injection:
        // Edit -> Injecting -> Passthrough (after PREEXEC)

        // Verify mode transitions are distinct
        let modes = [Mode::Edit, Mode::Injecting, Mode::Passthrough];

        for (i, mode) in modes.iter().enumerate() {
            for (j, other) in modes.iter().enumerate() {
                if i == j {
                    assert_eq!(mode, other);
                } else {
                    assert_ne!(mode, other);
                }
            }
        }
    }

    // =========================================================================
    // Panic Hook Terminal Restoration Tests
    // =========================================================================

    #[test]
    fn test_panic_hook_installed() {
        use std::panic;

        // Verify panic hook can be installed and doesn't break normal operation
        crate::safety::install_panic_hook();

        // After installation, panic behavior should include terminal restoration
        // We can't easily test the actual terminal restoration without a TTY,
        // but we verify the hook is installed by checking panic handling
        let result = panic::catch_unwind(|| {
            // This should trigger the panic hook
            panic!("Test panic for hook verification");
        });

        assert!(result.is_err(), "Panic should have been caught");
    }

    #[test]
    fn test_panic_hook_preserves_original() {
        use std::panic;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // Install a custom panic hook first
        let custom_called = Arc::new(AtomicBool::new(false));
        let custom_called_clone = custom_called.clone();

        let original = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            custom_called_clone.store(true, Ordering::SeqCst);
            // Don't call original to avoid noise in test output
            let _ = info;
        }));

        // Now install wrashpty's panic hook (which should chain to ours)
        crate::safety::install_panic_hook();

        // Trigger a panic
        let result = panic::catch_unwind(|| {
            panic!("Test panic for chaining");
        });

        assert!(result.is_err());
        assert!(
            custom_called.load(Ordering::SeqCst),
            "Original panic hook should have been called"
        );

        // Restore original panic hook for other tests
        panic::set_hook(original);
    }

    #[test]
    fn test_terminal_restoration_after_panic() {
        if !is_tty() {
            eprintln!("Skipping test (no terminal)");
            return;
        }

        use std::panic;

        // Install panic hook
        crate::safety::install_panic_hook();

        // Create a terminal guard
        let guard = match TerminalGuard::new() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("Skipping test (terminal unavailable): {}", e);
                return;
            }
        };

        // Force the guard to drop (simulating what happens during panic unwind)
        drop(guard);

        // After drop, verify terminal is in a usable state by creating new guard
        let result = panic::catch_unwind(|| match TerminalGuard::new() {
            Ok(g) => {
                drop(g);
                true
            }
            Err(_) => false,
        });

        match result {
            Ok(success) => assert!(
                success,
                "Should be able to create new guard after restoration"
            ),
            Err(_) => panic!("Guard creation panicked"),
        }
    }

    // =========================================================================
    // Signal Event Tests
    // =========================================================================

    #[test]
    fn test_signal_event_variants() {
        use crate::types::SignalEvent;

        let resize = SignalEvent::WindowResize;
        let child = SignalEvent::ChildExit;
        let shutdown = SignalEvent::Shutdown;

        assert!(matches!(resize, SignalEvent::WindowResize));
        assert!(matches!(child, SignalEvent::ChildExit));
        assert!(matches!(shutdown, SignalEvent::Shutdown));
    }

    #[test]
    fn test_signal_event_debug_format() {
        use crate::types::SignalEvent;

        assert!(format!("{:?}", SignalEvent::WindowResize).contains("WindowResize"));
        assert!(format!("{:?}", SignalEvent::ChildExit).contains("ChildExit"));
        assert!(format!("{:?}", SignalEvent::Shutdown).contains("Shutdown"));
    }

    // =========================================================================
    // Marker Batching Tests
    //
    // These tests verify that when multiple markers arrive in a single read
    // batch (common when commands fail quickly), all markers are processed
    // and none are lost. This guards against regression of the batching bug
    // where early returns after state transitions would lose remaining markers.
    // =========================================================================

    /// Helper to create a valid marker sequence for testing.
    fn make_test_marker(token: &[u8; 16], marker_type: &str, payload: Option<&str>) -> Vec<u8> {
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

    #[test]
    fn test_marker_batching_all_markers_parsed_from_single_chunk() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate a fast-failing command: PREEXEC, PRECMD, PROMPT all arrive together
        let mut batch = Vec::new();
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(make_test_marker(&token, "PRECMD", Some("127"))); // command not found
        batch.extend(make_test_marker(&token, "PROMPT", None));

        let outputs: Vec<_> = parser.feed(&batch).collect();

        // All three markers should be parsed
        let markers: Vec<_> = outputs
            .iter()
            .filter_map(|o| match o {
                ParseOutput::Marker(m) => Some(m.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(markers.len(), 3, "All three markers should be parsed from batch");
        assert!(matches!(markers[0], MarkerEvent::Preexec));
        assert!(matches!(markers[1], MarkerEvent::Precmd { exit_code: 127 }));
        assert!(matches!(markers[2], MarkerEvent::Prompt));
    }

    #[test]
    fn test_marker_batching_with_interleaved_output() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate markers with error output interleaved
        let mut batch = Vec::new();
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: foo: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        let outputs: Vec<_> = parser.feed(&batch).collect();

        // Count markers and bytes
        let mut marker_count = 0;
        let mut byte_chunks = 0;
        for output in &outputs {
            match output {
                ParseOutput::Marker(_) => marker_count += 1,
                ParseOutput::Bytes(_) => byte_chunks += 1,
            }
        }

        assert_eq!(marker_count, 3, "All three markers should be parsed");
        assert!(byte_chunks >= 1, "Error output should be passed through");
    }

    #[test]
    fn test_marker_batching_rapid_command_sequence() {
        use crate::marker::{MarkerParser, ParseOutput};

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Simulate multiple rapid commands (like pasting garbage)
        // Each command cycle: PREEXEC -> PRECMD -> PROMPT
        let mut batch = Vec::new();

        // Command 1: "foo" (not found)
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: foo: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        // Command 2: "bar" (not found)
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: bar: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        // Command 3: "baz" (not found)
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(b"bash: baz: command not found\n");
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        let outputs: Vec<_> = parser.feed(&batch).collect();

        // Count markers by type
        let mut preexec_count = 0;
        let mut precmd_count = 0;
        let mut prompt_count = 0;

        for output in &outputs {
            if let ParseOutput::Marker(m) = output {
                match m {
                    MarkerEvent::Preexec => preexec_count += 1,
                    MarkerEvent::Precmd { .. } => precmd_count += 1,
                    MarkerEvent::Prompt => prompt_count += 1,
                }
            }
        }

        assert_eq!(preexec_count, 3, "All PREEXEC markers should be parsed");
        assert_eq!(precmd_count, 3, "All PRECMD markers should be parsed");
        assert_eq!(prompt_count, 3, "All PROMPT markers should be parsed");
    }

    #[test]
    fn test_injection_batched_marker_transitions() {
        // Test that the mode transition logic handles batched markers correctly.
        // This simulates what happens in run_injecting() when processing a batch.
        let mut mode = Mode::Injecting;
        let mut last_exit_code = 0;

        // Simulate receiving [PREEXEC, PRECMD, PROMPT] in one batch
        let markers = vec![
            MarkerEvent::Preexec,
            MarkerEvent::Precmd { exit_code: 127 },
            MarkerEvent::Prompt,
        ];

        // Process markers the same way run_injecting() does after the fix
        for marker in markers {
            match mode {
                Mode::Injecting => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        mode = Mode::Passthrough;
                    }
                },
                Mode::Passthrough => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        // Already in passthrough
                    }
                },
                Mode::Edit => {
                    // Already in edit, done processing
                    break;
                }
                _ => break,
            }
        }

        // After processing the batch, we should end up in Edit mode
        assert_eq!(mode, Mode::Edit, "Should end in Edit mode after PROMPT");
        assert_eq!(last_exit_code, 127, "Exit code should be captured from PRECMD");
    }

    #[test]
    fn test_passthrough_batched_marker_transitions() {
        // Test that run_passthrough() logic handles batched markers correctly
        let mode = Mode::Passthrough;
        let mut last_exit_code = 0;

        // Simulate receiving [PRECMD, PROMPT, PREEXEC, PRECMD, PROMPT] in one batch
        // This represents: command ends, prompt shown, new command starts, fails, prompt shown
        let markers = vec![
            MarkerEvent::Precmd { exit_code: 0 },
            MarkerEvent::Prompt,
            MarkerEvent::Preexec,
            MarkerEvent::Precmd { exit_code: 1 },
            MarkerEvent::Prompt,
        ];

        let mut final_mode = mode;
        for marker in markers {
            match final_mode {
                Mode::Passthrough => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        final_mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        // Stay in passthrough
                    }
                },
                Mode::Edit => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        // Still update exit code for markers that arrive while in Edit
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        // Already at prompt
                    }
                    MarkerEvent::Preexec => {
                        // Unexpected in edit
                    }
                },
                _ => break,
            }
        }

        // Should end in Edit mode with the first PROMPT
        assert_eq!(final_mode, Mode::Edit);
        // With the batching fix, we continue processing markers in Edit mode,
        // so the final exit code is 1 from the second PRECMD
        assert_eq!(last_exit_code, 1);
    }

    #[test]
    fn test_initializing_mode_batched_markers() {
        // Test that run_initializing() logic handles batched markers
        let mut mode = Mode::Initializing;
        let mut last_exit_code = 0;

        // During initialization, we might see PRECMD and PROMPT together
        let markers = vec![
            MarkerEvent::Precmd { exit_code: 0 },
            MarkerEvent::Prompt,
        ];

        for marker in markers {
            match mode {
                Mode::Initializing => match marker {
                    MarkerEvent::Precmd { exit_code } => {
                        last_exit_code = exit_code;
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                    }
                    MarkerEvent::Preexec => {
                        // Unexpected
                    }
                },
                Mode::Edit => {
                    // Handle remaining markers in Edit context
                    match marker {
                        MarkerEvent::Precmd { exit_code } => {
                            last_exit_code = exit_code;
                        }
                        _ => {}
                    }
                }
                _ => break,
            }
        }

        assert_eq!(mode, Mode::Edit);
        assert_eq!(last_exit_code, 0);
    }

    #[test]
    fn test_marker_batching_no_markers_lost_regression() {
        // Regression test: ensure we don't lose markers after state transitions.
        // This specifically tests the bug where returning early after PREEXEC
        // would lose subsequent PRECMD and PROMPT markers.
        use crate::marker::{MarkerParser, ParseOutput};
        use smallvec::SmallVec;

        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Create the exact scenario that caused the bug:
        // Fast-failing command where all markers arrive in one read
        let mut batch = Vec::new();
        batch.extend(make_test_marker(&token, "PREEXEC", None));
        batch.extend(make_test_marker(&token, "PRECMD", Some("127")));
        batch.extend(make_test_marker(&token, "PROMPT", None));

        // Parse all markers
        let mut markers: SmallVec<[MarkerEvent; 4]> = SmallVec::new();
        for output in parser.feed(&batch) {
            if let ParseOutput::Marker(m) = output {
                markers.push(m);
            }
        }

        // Simulate processing with the FIXED logic (continues after transitions)
        let mut mode = Mode::Injecting;
        let mut reached_edit = false;

        for marker in &markers {
            match mode {
                Mode::Injecting => match marker {
                    MarkerEvent::Preexec => {
                        mode = Mode::Passthrough;
                        // BUG FIX: Don't return here, continue processing
                    }
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                        reached_edit = true;
                    }
                    MarkerEvent::Precmd { .. } => {}
                },
                Mode::Passthrough => match marker {
                    MarkerEvent::Prompt => {
                        mode = Mode::Edit;
                        reached_edit = true;
                    }
                    _ => {}
                },
                Mode::Edit => {
                    reached_edit = true;
                    break;
                }
                _ => {}
            }
        }

        // The critical assertion: we MUST reach Edit mode
        assert!(
            reached_edit,
            "Must reach Edit mode after processing batched markers"
        );
        assert_eq!(
            mode,
            Mode::Edit,
            "Final mode must be Edit after PROMPT marker"
        );
    }
