//! Passthrough byte pump for transparent I/O.
//!
//! This module handles bidirectional byte streaming between the user terminal
//! and the PTY during Passthrough mode, with marker detection spliced in.

// TODO: Implement in future Phase 0 tickets
// - Bidirectional pump loop
// - Marker detection integration
// - Non-blocking I/O with poll
