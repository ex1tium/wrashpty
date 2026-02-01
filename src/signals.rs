//! SIGWINCH and SIGCHLD signal handling.
//!
//! This module sets up async-signal-safe signal handlers using signal-hook,
//! converting signals into events for the main event loop.

// TODO: Implement in future Phase 0 tickets
// - SIGWINCH handling for terminal resize
// - SIGCHLD handling for child process events
// - Signal-to-event conversion
// - Integration with signal-hook crate
