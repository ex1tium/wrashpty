//! Integration tests for the OSC 777 marker parser.
//!
//! These tests complement the unit tests in `src/marker.rs` by testing the parser
//! with realistic shell output sequences, including interleaved ANSI escape codes
//! and multi-marker workflows.

use proptest::prelude::*;
use wrashpty::marker::{MAX_MARKER_LEN, MarkerParser, ParseOutput};
use wrashpty::types::MarkerEvent;

/// Helper to create a test session token.
fn make_token() -> [u8; 16] {
    *b"a1b2c3d4e5f67890"
}

/// Helper to create a valid marker sequence.
fn make_marker(token: &[u8; 16], marker_type: &str, payload: Option<&str>) -> Vec<u8> {
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

/// Owned version of ParseOutput for storing across parser calls in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnedOutput {
    Bytes(Vec<u8>),
    Marker(MarkerEvent),
}

impl<'a> From<ParseOutput<'a>> for OwnedOutput {
    fn from(output: ParseOutput<'a>) -> Self {
        match output {
            ParseOutput::Bytes(cow) => OwnedOutput::Bytes(cow.into_owned()),
            ParseOutput::Marker(m) => OwnedOutput::Marker(m),
        }
    }
}

/// Helper to extract bytes from output.
fn get_bytes(output: &OwnedOutput) -> Option<&[u8]> {
    match output {
        OwnedOutput::Bytes(b) => Some(b),
        _ => None,
    }
}

/// Helper to check if output is a specific marker.
fn is_marker(output: &OwnedOutput, expected: &MarkerEvent) -> bool {
    matches!(output, OwnedOutput::Marker(m) if m == expected)
}

/// Collect outputs into owned form for testing.
fn collect_outputs(parser: &mut MarkerParser, input: &[u8]) -> Vec<OwnedOutput> {
    parser.feed(input).map(OwnedOutput::from).collect()
}

// =============================================================================
// Integration Tests: Realistic Shell Output
// =============================================================================

#[test]
fn test_realistic_shell_workflow() {
    // Simulate a typical bash workflow:
    // 1. PRECMD with exit code from previous command
    // 2. PROMPT after prompt is rendered
    // 3. PREEXEC when command starts executing
    // 4. Command output
    // 5. Back to PRECMD

    let token = make_token();
    let mut parser = MarkerParser::new(token);

    let mut shell_output = Vec::new();

    // Previous command exit code 0
    shell_output.extend(make_marker(&token, "PRECMD", Some("0")));

    // PS1 prompt with ANSI colors
    shell_output.extend(b"\x1b[32muser@host\x1b[0m:\x1b[34m~/project\x1b[0m$ ");

    // PROMPT marker after prompt render
    shell_output.extend(make_marker(&token, "PROMPT", None));

    let outputs = collect_outputs(&mut parser, &shell_output);

    // Should have PRECMD marker at start and PROMPT marker at end
    // The middle contains bytes (may be split due to ESC handling)
    let markers: Vec<_> = outputs
        .iter()
        .filter(|o| matches!(o, OwnedOutput::Marker(_)))
        .collect();
    assert_eq!(markers.len(), 2);

    // First marker should be PRECMD
    assert!(is_marker(
        &outputs[0],
        &MarkerEvent::Precmd { exit_code: 0 }
    ));

    // Last output should be PROMPT marker
    assert!(is_marker(outputs.last().unwrap(), &MarkerEvent::Prompt));

    // Collect all bytes and verify the prompt content is preserved
    let bytes: Vec<u8> = outputs
        .iter()
        .filter_map(|o| match o {
            OwnedOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    // Verify the ANSI colored prompt is in the output
    assert!(bytes.windows(9).any(|w| w == b"user@host"));
    assert!(bytes.windows(9).any(|w| w == b"~/project"));
}

#[test]
fn test_command_execution_cycle() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // User types 'ls' and hits enter
    // PREEXEC fires before command runs
    let mut output = make_marker(&token, "PREEXEC", None);

    // Command output
    output.extend(b"file1.txt  file2.txt  dir1/\n");

    // Command completes, PRECMD fires with exit code
    output.extend(make_marker(&token, "PRECMD", Some("0")));

    let outputs = collect_outputs(&mut parser, &output);

    assert_eq!(outputs.len(), 3);
    assert!(is_marker(&outputs[0], &MarkerEvent::Preexec));
    assert_eq!(
        get_bytes(&outputs[1]),
        Some(b"file1.txt  file2.txt  dir1/\n".as_slice())
    );
    assert!(is_marker(
        &outputs[2],
        &MarkerEvent::Precmd { exit_code: 0 }
    ));
}

#[test]
fn test_failed_command_exit_code() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // Command fails with exit code 2 (e.g., ls misuse or file not found on some systems)
    let mut output = make_marker(&token, "PREEXEC", None);
    output.extend(b"ls: cannot access 'nonexistent': No such file or directory\n");
    output.extend(make_marker(&token, "PRECMD", Some("2")));

    let outputs = collect_outputs(&mut parser, &output);

    assert_eq!(outputs.len(), 3);
    assert!(is_marker(&outputs[0], &MarkerEvent::Preexec));
    assert!(matches!(outputs[1], OwnedOutput::Bytes(_)));
    assert!(is_marker(
        &outputs[2],
        &MarkerEvent::Precmd { exit_code: 2 }
    ));
}

// =============================================================================
// Integration Tests: ANSI Escape Sequences
// =============================================================================

#[test]
fn test_ansi_csi_sequences_passthrough() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // Various ANSI CSI sequences that should pass through
    let ansi_sequences = [
        b"\x1b[H".to_vec(),      // Cursor home
        b"\x1b[2J".to_vec(),     // Clear screen
        b"\x1b[31m".to_vec(),    // Set foreground red
        b"\x1b[0m".to_vec(),     // Reset attributes
        b"\x1b[10;20H".to_vec(), // Move cursor to row 10, col 20
        b"\x1b[?1049h".to_vec(), // Enable alternate screen
        b"\x1b[?25l".to_vec(),   // Hide cursor
    ];

    for seq in &ansi_sequences {
        let outputs = collect_outputs(&mut parser, seq);
        assert!(!outputs.is_empty());
        // All should be bytes, not markers
        for output in outputs {
            assert!(matches!(output, OwnedOutput::Bytes(_)));
        }
        // Parser should not be stuck mid-sequence
        // (CSI sequences start with ESC [ not ESC ])
        assert!(!parser.is_mid_sequence());
    }
}

#[test]
fn test_osc_sequences_passthrough() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // Non-777 OSC sequences should pass through
    let osc_sequences = [
        b"\x1b]0;Window Title\x07".to_vec(), // Set window title (OSC 0)
        b"\x1b]2;Tab Title\x07".to_vec(),    // Set window title (OSC 2)
        b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07".to_vec(), // Hyperlink (OSC 8)
        b"\x1b]52;c;SGVsbG8=\x07".to_vec(),  // Clipboard (OSC 52)
    ];

    for seq in &osc_sequences {
        let outputs = collect_outputs(&mut parser, seq);
        assert!(!outputs.is_empty());
        // All should be bytes, not markers
        for output in &outputs {
            assert!(matches!(output, OwnedOutput::Bytes(_)));
        }
        assert!(!parser.is_mid_sequence());
    }
}

#[test]
fn test_markers_interleaved_with_ansi() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // Realistic prompt with colors and markers
    let mut output = Vec::new();

    // PRECMD
    output.extend(make_marker(&token, "PRECMD", Some("0")));

    // Colored prompt: green user@host, blue path
    output.extend(b"\x1b[1;32muser@host\x1b[0m");
    output.extend(b":");
    output.extend(b"\x1b[1;34m~/project\x1b[0m");
    output.extend(b"$ ");

    // PROMPT
    output.extend(make_marker(&token, "PROMPT", None));

    let outputs = collect_outputs(&mut parser, &output);

    // Should get: PRECMD marker, bytes (prompt), PROMPT marker
    let markers: Vec<_> = outputs
        .iter()
        .filter(|o| matches!(o, OwnedOutput::Marker(_)))
        .collect();
    assert_eq!(markers.len(), 2);

    // Verify bytes contain the ANSI codes
    let bytes: Vec<u8> = outputs
        .iter()
        .filter_map(|o| match o {
            OwnedOutput::Bytes(b) => Some(b.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    assert!(bytes.windows(4).any(|w| w == b"\x1b[1;"));
}

// =============================================================================
// Integration Tests: Edge Cases with Real Data
// =============================================================================

#[test]
fn test_large_command_output() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // Simulate large command output (like `cat large_file.txt`)
    let mut output = make_marker(&token, "PREEXEC", None);

    // Add 100KB of random output
    let large_content: Vec<u8> = (0..100_000)
        .map(|i| ((i % 94) as u8) + 32) // Printable ASCII
        .collect();
    output.extend(&large_content);

    output.extend(make_marker(&token, "PRECMD", Some("0")));

    let outputs = collect_outputs(&mut parser, &output);

    // Verify we got markers and the content
    let markers: Vec<_> = outputs
        .iter()
        .filter(|o| matches!(o, OwnedOutput::Marker(_)))
        .collect();
    assert_eq!(markers.len(), 2);

    let total_bytes: usize = outputs
        .iter()
        .filter_map(|o| match o {
            OwnedOutput::Bytes(b) => Some(b.len()),
            _ => None,
        })
        .sum();
    assert_eq!(total_bytes, large_content.len());
}

#[test]
fn test_rapid_marker_sequence() {
    // Test that rapid successive markers are all captured
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // 100 rapid PRECMD/PROMPT cycles
    let mut output = Vec::new();
    for i in 0..100 {
        output.extend(make_marker(&token, "PRECMD", Some(&i.to_string())));
        output.extend(make_marker(&token, "PROMPT", None));
    }

    let outputs = collect_outputs(&mut parser, &output);

    let markers: Vec<_> = outputs
        .iter()
        .filter(|o| matches!(o, OwnedOutput::Marker(_)))
        .collect();

    // Should have 200 markers (100 PRECMD + 100 PROMPT)
    assert_eq!(markers.len(), 200);
}

#[test]
fn test_split_across_multiple_reads() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // Create a marker and split it at various points
    let marker = make_marker(&token, "PROMPT", None);

    // Feed one byte at a time (simulating worst-case fragmentation)
    let mut all_outputs = Vec::new();
    for byte in &marker {
        let outputs = collect_outputs(&mut parser, std::slice::from_ref(byte));
        all_outputs.extend(outputs);
    }

    // Should get exactly one marker
    assert_eq!(all_outputs.len(), 1);
    assert!(is_marker(&all_outputs[0], &MarkerEvent::Prompt));
}

#[test]
fn test_marker_at_buffer_boundary() {
    let token = make_token();
    let mut parser = MarkerParser::new(token);

    // Create input where marker starts at various positions relative to
    // typical read buffer sizes (4096, 8192, etc.)
    let prefix = vec![b'x'; 4090]; // Just before 4096 boundary
    let marker = make_marker(&token, "PREEXEC", None);

    let mut input = prefix.clone();
    input.extend(&marker);
    input.extend(b"suffix");

    // Split at exactly 4096 bytes
    let part1 = &input[..4096];
    let part2 = &input[4096..];

    let outputs1 = collect_outputs(&mut parser, part1);
    let outputs2 = collect_outputs(&mut parser, part2);

    // Combine and verify
    let all_outputs: Vec<_> = outputs1.into_iter().chain(outputs2).collect();

    // Should have prefix bytes, marker, suffix bytes
    let markers: Vec<_> = all_outputs
        .iter()
        .filter(|o| matches!(o, OwnedOutput::Marker(_)))
        .collect();
    assert_eq!(markers.len(), 1);
}

// =============================================================================
// Property-Based Integration Tests
// =============================================================================

proptest! {
    /// Test that valid markers embedded in random data are always extracted.
    #[test]
    fn test_markers_extracted_from_noise(
        prefix in prop::collection::vec(any::<u8>(), 0..500),
        suffix in prop::collection::vec(any::<u8>(), 0..500),
        exit_code in 0i32..256i32
    ) {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Filter prefix/suffix to not contain ESC (to avoid confusing the parser)
        let prefix: Vec<u8> = prefix.into_iter().filter(|&b| b != 0x1B).collect();
        let suffix: Vec<u8> = suffix.into_iter().filter(|&b| b != 0x1B).collect();

        let mut input = prefix.clone();
        let marker = make_marker(&token, "PRECMD", Some(&exit_code.to_string()));
        input.extend(&marker);
        input.extend(&suffix);

        let outputs = collect_outputs(&mut parser, &input);

        // Find the marker
        let found_marker = outputs.iter().any(|o| {
            matches!(o, OwnedOutput::Marker(MarkerEvent::Precmd { exit_code: ec }) if *ec == exit_code)
        });
        prop_assert!(found_marker, "Marker with exit code {} not found", exit_code);

        // Verify prefix and suffix bytes are in output
        let output_bytes: Vec<u8> = outputs
            .iter()
            .filter_map(|o| match o {
                OwnedOutput::Bytes(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();

        if !prefix.is_empty() {
            prop_assert!(output_bytes.windows(prefix.len()).any(|w| w == prefix.as_slice()));
        }
        if !suffix.is_empty() {
            prop_assert!(output_bytes.windows(suffix.len()).any(|w| w == suffix.as_slice()));
        }
    }

    /// Test that splitting input at any point produces consistent results.
    #[test]
    fn test_split_point_invariance(
        data in prop::collection::vec(any::<u8>(), 1..200),
        split_point in 0usize..200usize
    ) {
        let token = *b"a1b2c3d4e5f67890";

        // Parse all at once
        let mut parser1 = MarkerParser::new(token);
        let outputs1 = collect_outputs(&mut parser1, &data);
        let remaining1 = parser1.flush_stale().map(|s| s.to_vec());

        // Parse split at split_point
        let split_point = split_point % (data.len() + 1);
        let mut parser2 = MarkerParser::new(token);
        let outputs2a = collect_outputs(&mut parser2, &data[..split_point]);
        let outputs2b = collect_outputs(&mut parser2, &data[split_point..]);
        let remaining2 = parser2.flush_stale().map(|s| s.to_vec());

        // Collect all bytes from both approaches
        let bytes1: Vec<u8> = outputs1
            .iter()
            .filter_map(|o| match o {
                OwnedOutput::Bytes(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .chain(remaining1.into_iter().flatten())
            .collect();

        let bytes2: Vec<u8> = outputs2a
            .iter()
            .chain(outputs2b.iter())
            .filter_map(|o| match o {
                OwnedOutput::Bytes(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .chain(remaining2.into_iter().flatten())
            .collect();

        // Both approaches should yield the same bytes
        prop_assert_eq!(bytes1, bytes2);

        // Both approaches should find the same number of markers
        let markers1 = outputs1
            .iter()
            .filter(|o| matches!(o, OwnedOutput::Marker(_)))
            .count();
        let markers2 = outputs2a
            .iter()
            .chain(outputs2b.iter())
            .filter(|o| matches!(o, OwnedOutput::Marker(_)))
            .count();
        prop_assert_eq!(markers1, markers2);
    }

    /// Test that buffer overflow is handled gracefully.
    #[test]
    fn test_overflow_handling(
        body_len in MAX_MARKER_LEN..MAX_MARKER_LEN + 100
    ) {
        let token = *b"a1b2c3d4e5f67890";
        let mut parser = MarkerParser::new(token);

        // Create an OSC sequence that exceeds buffer
        let mut huge_seq = vec![0x1B, 0x5D]; // ESC ]
        huge_seq.extend(vec![b'x'; body_len]);
        huge_seq.push(0x07);

        // Should not panic
        let outputs = collect_outputs(&mut parser, &huge_seq);

        // Should return bytes (not markers)
        for output in &outputs {
            prop_assert!(matches!(output, OwnedOutput::Bytes(_)));
        }

        // Parser should be in clean state
        prop_assert!(!parser.is_mid_sequence());
    }
}
