//! Reedline editor bridge.
//!
//! This module integrates reedline as the line editor, configuring it with
//! history support, tab completions, and autosuggestions, while managing
//! background output buffering during editing.
//!
//! # Features
//!
//! - **History**: File-backed history with Ctrl+R search and Up/Down prefix filtering
//!   (provided by reedline's default keybindings)
//! - **Completions**: Context-aware tab completion for paths, executables, and git branches
//! - **Autosuggestions**: Fish-style inline suggestions from command history

use std::collections::VecDeque;

use anyhow::{Context, Result};
use reedline::{
    ColumnarMenu, FileBackedHistory, HistoryItem, KeyCode, KeyModifiers, MenuBuilder, Prompt,
    Reedline, ReedlineEvent, ReedlineMenu, Signal, default_emacs_keybindings,
};
use tracing::{debug, info, warn};

use crate::complete::WrashCompleter;
use crate::suggest::HistoryHinter;

/// Maximum size of the pending output buffer (64KB).
const MAX_PENDING_OUTPUT: usize = 64 * 1024;

/// Buffer for PTY output received during Edit mode.
///
/// Uses a VecDeque for efficient push/pop operations. When the buffer
/// exceeds MAX_PENDING_OUTPUT, oldest bytes are dropped to prevent
/// memory exhaustion from runaway background jobs.
struct PendingOutputBuffer {
    /// The output buffer.
    buffer: VecDeque<u8>,
    /// Number of bytes dropped due to overflow.
    dropped_bytes: usize,
    /// Whether we've already warned about dropping bytes.
    drop_warned: bool,
}

impl PendingOutputBuffer {
    /// Creates a new empty buffer with default capacity.
    fn new() -> Self {
        Self {
            buffer: VecDeque::with_capacity(4 * 1024), // 4KB initial capacity
            dropped_bytes: 0,
            drop_warned: false,
        }
    }

    /// Adds data to the buffer, dropping oldest bytes if capacity is exceeded.
    fn push(&mut self, data: &[u8]) {
        let total_len = self.buffer.len() + data.len();
        let overflow = total_len.saturating_sub(MAX_PENDING_OUTPUT);

        if overflow > 0 {
            // Log warning on first drop
            if !self.drop_warned {
                warn!(
                    "Background output buffer full ({}KB), dropping oldest bytes",
                    MAX_PENDING_OUTPUT / 1024
                );
                self.drop_warned = true;
            }

            self.dropped_bytes += overflow;

            if overflow >= self.buffer.len() {
                // Need to drop entire buffer plus some of new data
                let data_to_skip = overflow - self.buffer.len();
                self.buffer.clear();
                self.buffer.extend(&data[data_to_skip..]);
            } else {
                // Only need to drop from existing buffer
                self.buffer.drain(..overflow);
                self.buffer.extend(data);
            }
        } else {
            self.buffer.extend(data);
        }
    }

    /// Drains all buffered data and returns it along with the drop count.
    ///
    /// Resets the dropped bytes counter and warning flag after drain.
    fn drain(&mut self) -> (Vec<u8>, usize) {
        let data: Vec<u8> = self.buffer.drain(..).collect();
        let dropped = self.dropped_bytes;

        // Reset counters
        self.dropped_bytes = 0;
        self.drop_warned = false;

        (data, dropped)
    }

    /// Returns whether the buffer is empty.
    fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Returns the current buffer size.
    fn len(&self) -> usize {
        self.buffer.len()
    }
}

/// Result of a line edit operation.
#[derive(Debug)]
pub enum EditorResult {
    /// User submitted a command.
    Command(String),
    /// User pressed Ctrl+C to clear the line.
    ClearLine,
    /// User pressed Ctrl+D to exit.
    Exit,
    /// A host command was requested via ExecuteHostCommand.
    HostCommand(String),
}

/// Reedline-based line editor with background output buffering.
///
/// The Editor wraps reedline to provide command line editing with history
/// support. It also buffers any PTY output received during editing (from
/// background jobs) to prevent terminal corruption.
pub struct Editor {
    /// The reedline instance.
    reedline: Reedline,
    /// Buffer for output received during editing.
    pending_output: PendingOutputBuffer,
}

impl Editor {
    /// Creates a new Editor with history loaded from ~/.bash_history.
    ///
    /// # Errors
    ///
    /// Returns an error if reedline cannot be created or history loading fails
    /// critically (note: missing history file is not an error).
    pub fn new() -> Result<Self> {
        // Load history from bash_history file
        let history_entries = crate::history::load_history().unwrap_or_else(|e| {
            warn!("Failed to load history: {}", e);
            Vec::new()
        });

        let entry_count = history_entries.len();

        // Create file-backed history
        // We use a temporary in-memory approach since FileBackedHistory
        // manages its own file. We'll populate it with loaded entries.
        let history = FileBackedHistory::with_file(
            entry_count.max(10_000),
            dirs::home_dir()
                .map(|h| h.join(".wrashpty_history"))
                .unwrap_or_else(|| "/tmp/.wrashpty_history".into()),
        )
        .context("Failed to create history storage")?;

        // Create completer for tab completion
        let completer = Box::new(WrashCompleter::new());

        // Create hinter for fish-style autosuggestions
        let hinter = Box::new(HistoryHinter::new());

        // Create a columnar completion menu
        let completion_menu = Box::new(
            ColumnarMenu::default()
                .with_name("completion_menu")
                .with_columns(4)
                .with_column_width(None) // auto-width
        );

        // Set up keybindings with Tab triggering the completion menu
        // and Ctrl+Space opening the panel
        let mut keybindings = default_emacs_keybindings();
        keybindings.add_binding(
            KeyModifiers::NONE,
            KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext,
            ]),
        );
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char(' '),
            ReedlineEvent::ExecuteHostCommand("open_panel".to_string()),
        );

        // Create reedline with history, completions, menu, and autosuggestions
        // Note: Ctrl+R history search and Up/Down prefix filtering are provided
        // by reedline's default keybindings when history is configured.
        let mut reedline = Reedline::create()
            .with_history(Box::new(history))
            .with_completer(completer)
            .with_hinter(hinter)
            .with_menu(ReedlineMenu::EngineCompleter(completion_menu))
            .with_edit_mode(Box::new(reedline::Emacs::new(keybindings)));

        // Populate reedline history with loaded bash_history entries
        // Use history_mut().save() to add each entry to the history store
        let mut saved_count = 0;
        let mut failed_count = 0;
        for entry in history_entries {
            let history_item = HistoryItem::from_command_line(&entry);
            if let Err(e) = reedline.history_mut().save(history_item) {
                // Log warning but don't abort - continue loading remaining entries
                if failed_count == 0 {
                    warn!("Failed to save history entry: {}", e);
                }
                failed_count += 1;
            } else {
                saved_count += 1;
            }
        }

        // Sync history to persist the loaded entries
        if let Err(e) = reedline.sync_history() {
            warn!("Failed to sync history after loading: {}", e);
        }

        info!(
            loaded = entry_count,
            saved = saved_count,
            failed = failed_count,
            "Editor created with history, completions, and autosuggestions"
        );

        Ok(Self {
            reedline,
            pending_output: PendingOutputBuffer::new(),
        })
    }

    /// Reads a line of input from the user.
    ///
    /// # Arguments
    ///
    /// * `prompt` - The prompt to display
    ///
    /// # Returns
    ///
    /// An EditorResult indicating what the user did:
    /// - Command(line): User submitted a command
    /// - ClearLine: User pressed Ctrl+C
    /// - Exit: User pressed Ctrl+D
    ///
    /// # Errors
    ///
    /// Returns an error if reedline encounters an I/O error.
    pub fn read_line(&mut self, prompt: &dyn Prompt) -> Result<EditorResult> {
        match self.reedline.read_line(prompt) {
            Ok(Signal::Success(line)) => {
                // Check if this is a host command (from ExecuteHostCommand event)
                // ExecuteHostCommand returns through Success in reedline 0.45
                if line == "open_panel" {
                    debug!("Host command: open_panel");
                    Ok(EditorResult::HostCommand(line))
                } else {
                    debug!(command = %line, "User submitted command");
                    Ok(EditorResult::Command(line))
                }
            }
            Ok(Signal::CtrlC) => {
                debug!("User pressed Ctrl+C");
                Ok(EditorResult::ClearLine)
            }
            Ok(Signal::CtrlD) => {
                debug!("User pressed Ctrl+D");
                Ok(EditorResult::Exit)
            }
            Err(e) => Err(e).context("Reedline read_line failed"),
        }
    }

    /// Buffers output received during editing.
    ///
    /// Call this when PTY output is received while in Edit mode to prevent
    /// it from corrupting the editor display.
    pub fn buffer_output(&mut self, data: &[u8]) {
        self.pending_output.push(data);
    }

    /// Flushes and returns all pending output.
    ///
    /// # Returns
    ///
    /// A tuple of (buffered_data, dropped_byte_count).
    pub fn flush_pending(&mut self) -> (Vec<u8>, usize) {
        self.pending_output.drain()
    }

    /// Returns whether there is pending output.
    pub fn has_pending(&self) -> bool {
        !self.pending_output.is_empty()
    }

    /// Returns the size of pending output in bytes.
    pub fn pending_size(&self) -> usize {
        self.pending_output.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // PendingOutputBuffer Tests
    // =========================================================================

    #[test]
    fn test_buffer_new() {
        let buffer = PendingOutputBuffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn test_buffer_push_under_limit() {
        let mut buffer = PendingOutputBuffer::new();
        let data = b"hello world";
        buffer.push(data);
        assert!(!buffer.is_empty());
        assert_eq!(buffer.len(), data.len());
    }

    #[test]
    fn test_buffer_drain() {
        let mut buffer = PendingOutputBuffer::new();
        buffer.push(b"hello");
        buffer.push(b" world");

        let (data, dropped) = buffer.drain();
        assert_eq!(data, b"hello world");
        assert_eq!(dropped, 0);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_buffer_overflow_drops_oldest() {
        let mut buffer = PendingOutputBuffer::new();

        // Fill buffer to capacity
        let fill_data = vec![b'A'; MAX_PENDING_OUTPUT];
        buffer.push(&fill_data);
        assert_eq!(buffer.len(), MAX_PENDING_OUTPUT);

        // Push more data - should drop oldest
        buffer.push(b"XYZ");
        assert_eq!(buffer.len(), MAX_PENDING_OUTPUT);

        let (data, dropped) = buffer.drain();
        assert_eq!(dropped, 3);
        // Last 3 bytes should be "XYZ"
        assert_eq!(&data[data.len() - 3..], b"XYZ");
    }

    #[test]
    fn test_buffer_drain_resets_counters() {
        let mut buffer = PendingOutputBuffer::new();

        // Fill and overflow
        let fill_data = vec![b'A'; MAX_PENDING_OUTPUT + 100];
        buffer.push(&fill_data);

        let (_, dropped) = buffer.drain();
        assert_eq!(dropped, 100);

        // After drain, counters should be reset
        buffer.push(b"test");
        let (_, dropped2) = buffer.drain();
        assert_eq!(dropped2, 0);
    }

    // =========================================================================
    // EditorResult Tests
    // =========================================================================

    #[test]
    fn test_editor_result_command() {
        let result = EditorResult::Command("echo hello".to_string());
        assert!(matches!(result, EditorResult::Command(s) if s == "echo hello"));
    }

    #[test]
    fn test_editor_result_clear_line() {
        let result = EditorResult::ClearLine;
        assert!(matches!(result, EditorResult::ClearLine));
    }

    #[test]
    fn test_editor_result_exit() {
        let result = EditorResult::Exit;
        assert!(matches!(result, EditorResult::Exit));
    }

    #[test]
    fn test_editor_result_debug() {
        // Test Debug implementation
        let cmd = EditorResult::Command("test".to_string());
        let debug_str = format!("{:?}", cmd);
        assert!(debug_str.contains("Command"));
        assert!(debug_str.contains("test"));
    }

    // =========================================================================
    // MAX_PENDING_OUTPUT Constant Tests
    // =========================================================================

    #[test]
    fn test_max_pending_output_constant() {
        // Verify the constant is 64KB
        assert_eq!(MAX_PENDING_OUTPUT, 64 * 1024);
    }
}
