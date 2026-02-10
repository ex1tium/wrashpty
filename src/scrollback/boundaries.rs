//! Command boundary tracking for navigating between command outputs.
//!
//! This module tracks where commands start and end in the scrollback buffer,
//! enabling Ctrl+P/N navigation to jump between command outputs.

use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveDateTime;

use crate::types::MarkerEvent;

/// Metadata about a completed or in-flight command captured from markers.
#[derive(Debug, Clone, Default)]
pub struct CommandRecord {
    /// Line index where this command's output begins.
    pub output_start: usize,
    /// Line index where this command's prompt boundary appears.
    pub prompt_line: Option<usize>,
    /// Command text submitted by the user.
    pub command_text: Option<String>,
    /// Exit code from PRECMD marker.
    pub exit_code: Option<i32>,
    /// Command duration measured in app state.
    pub duration: Option<Duration>,
    /// Working directory when command started.
    pub cwd: Option<PathBuf>,
    /// Local timestamp when command started.
    pub started_at: Option<NaiveDateTime>,
    /// Whether command output is folded in the viewer.
    pub folded: bool,
}

/// Index of command boundaries for Ctrl+P/N navigation.
///
/// Tracks line indices where commands start (after Preexec) and where
/// prompts appear (after Precmd). This enables jumping between command
/// outputs - a unique feature not found in standard terminal emulators.
#[derive(Debug, Clone, Default)]
pub struct CommandBoundaries {
    /// Line indices where command output starts (right after Preexec marker).
    /// These mark the beginning of a command's output.
    pub command_starts: Vec<usize>,

    /// Line indices where prompts appear (after Precmd/Prompt markers).
    /// These mark the end of a command's output and the start of the next prompt.
    pub prompt_lines: Vec<usize>,

    /// Rich command records keyed by prompt boundary.
    pub records: Vec<CommandRecord>,

    /// Pending command record started at PREEXEC and completed at PRECMD.
    pending: Option<CommandRecord>,
    /// True when pending record was seeded from injection before PREEXEC.
    pending_seeded: bool,
}

impl CommandBoundaries {
    /// Creates a new empty boundary index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a marker event at the given buffer line index.
    ///
    /// Call this from the PTY pump loop when a marker is detected.
    /// The line_index should be the current buffer length at the time
    /// the marker is received.
    pub fn record_marker(&mut self, event: &MarkerEvent, line_index: usize) {
        match event {
            MarkerEvent::Preexec => {
                self.start_record(line_index, None, None, None);
            }
            MarkerEvent::Precmd { exit_code } => {
                self.complete_record(line_index, *exit_code, None);
            }
            MarkerEvent::Prompt => self.record_prompt_line(line_index),
        }
    }

    /// Starts a pending command record on PREEXEC.
    pub fn start_record(
        &mut self,
        line_index: usize,
        command_text: Option<String>,
        cwd: Option<PathBuf>,
        started_at: Option<NaiveDateTime>,
    ) {
        // If we already seeded a pending record from command injection, anchor
        // that record to PREEXEC rather than creating a duplicate.
        if self.pending_seeded {
            if let Some(pending) = &mut self.pending {
                let previous_start = pending.output_start;
                pending.output_start = line_index;
                if pending.command_text.is_none() {
                    pending.command_text = command_text;
                }
                if pending.cwd.is_none() {
                    pending.cwd = cwd;
                }
                if pending.started_at.is_none() {
                    pending.started_at = started_at;
                }
                if let Some(last_start) = self.command_starts.last_mut() {
                    if *last_start == previous_start {
                        *last_start = line_index;
                    } else if *last_start != line_index {
                        self.command_starts.push(line_index);
                    }
                } else {
                    self.command_starts.push(line_index);
                }
                self.pending_seeded = false;
                return;
            }
            self.pending_seeded = false;
        }

        // Recover from malformed marker ordering by closing any stale pending record.
        if let Some(mut stale) = self.pending.take() {
            stale.prompt_line = Some(line_index);
            self.prompt_lines.push(line_index);
            self.records.push(stale);
        }

        self.command_starts.push(line_index);
        self.pending = Some(CommandRecord {
            output_start: line_index,
            command_text,
            cwd,
            started_at,
            ..Default::default()
        });
    }

    /// Completes the pending command record on PRECMD.
    pub fn complete_record(
        &mut self,
        line_index: usize,
        exit_code: i32,
        duration: Option<Duration>,
    ) {
        if let Some(mut record) = self.pending.take() {
            record.prompt_line = Some(line_index);
            record.exit_code = Some(exit_code);
            record.duration = duration;
            self.prompt_lines.push(line_index);
            self.records.push(record);
            self.pending_seeded = false;
            return;
        }

        self.record_prompt_line(line_index);
    }

    /// Seeds a pending command record when command injection starts.
    ///
    /// This preserves metadata even if PREEXEC markers are delayed, reordered,
    /// or dropped. PREEXEC later re-anchors this record to the true output start.
    pub fn seed_record(
        &mut self,
        line_index: usize,
        command_text: Option<String>,
        cwd: Option<PathBuf>,
        started_at: Option<NaiveDateTime>,
    ) {
        if let Some(mut stale) = self.pending.take() {
            stale.prompt_line = Some(line_index);
            self.prompt_lines.push(line_index);
            self.records.push(stale);
        }

        if self.command_starts.last().copied() != Some(line_index) {
            self.command_starts.push(line_index);
        }

        self.pending = Some(CommandRecord {
            output_start: line_index,
            command_text,
            cwd,
            started_at,
            ..Default::default()
        });
        self.pending_seeded = true;
    }

    /// Records a prompt separator boundary line.
    pub fn record_prompt_line(&mut self, line_index: usize) {
        if self.prompt_lines.last().copied() != Some(line_index) {
            self.prompt_lines.push(line_index);
        }
    }

    /// Returns whether a PREEXEC record is waiting for PRECMD completion.
    pub fn has_pending_record(&self) -> bool {
        self.pending.is_some()
    }

    /// Looks up the command record associated with a prompt boundary line.
    pub fn record_for_prompt_line(&self, prompt_line: usize) -> Option<&CommandRecord> {
        self.records
            .iter()
            .find(|record| record.prompt_line == Some(prompt_line))
    }

    /// Looks up the command record active for a given buffer line index.
    ///
    /// Returns `(record_index, record)` when the line belongs to a command's
    /// output range `[output_start, prompt_line)`.
    pub fn record_for_line(&self, line_idx: usize) -> Option<(usize, &CommandRecord)> {
        let pos = self
            .records
            .partition_point(|record| record.output_start <= line_idx);
        if pos == 0 {
            return None;
        }

        let idx = pos - 1;
        let record = self.records.get(idx)?;
        let end = record.prompt_line.unwrap_or(usize::MAX);
        if line_idx < end {
            Some((idx, record))
        } else {
            None
        }
    }

    /// Counts folded output lines overlapping `[start, end)`.
    pub fn folded_line_count_in_range(&self, start: usize, end: usize) -> usize {
        if start >= end {
            return 0;
        }

        self.records
            .iter()
            .filter(|record| record.folded)
            .map(|record| {
                let hidden_start = record.output_start.saturating_add(1);
                let hidden_end = record.prompt_line.unwrap_or(end);
                let overlap_start = start.max(hidden_start);
                let overlap_end = end.min(hidden_end);
                overlap_end.saturating_sub(overlap_start)
            })
            .sum()
    }

    /// Toggles folded state for the specified record index.
    pub fn toggle_fold(&mut self, record_idx: usize) -> Option<bool> {
        let record = self.records.get_mut(record_idx)?;
        record.folded = !record.folded;
        Some(record.folded)
    }

    /// Finds the previous command start before the given line.
    ///
    /// Returns the line index of the most recent command output start
    /// that is before `from_line`. Used for Ctrl+P navigation.
    pub fn prev_command(&self, from_line: usize) -> Option<usize> {
        self.command_starts
            .iter()
            .rev()
            .find(|&&line| line < from_line)
            .copied()
    }

    /// Finds the next command start after the given line.
    ///
    /// Returns the line index of the next command output start
    /// that is after `from_line`. Used for Ctrl+N navigation.
    pub fn next_command(&self, from_line: usize) -> Option<usize> {
        self.command_starts
            .iter()
            .find(|&&line| line > from_line)
            .copied()
    }

    /// Finds the previous prompt line before the given line.
    pub fn prev_prompt(&self, from_line: usize) -> Option<usize> {
        self.prompt_lines
            .iter()
            .rev()
            .find(|&&line| line < from_line)
            .copied()
    }

    /// Finds the next prompt line after the given line.
    pub fn next_prompt(&self, from_line: usize) -> Option<usize> {
        self.prompt_lines
            .iter()
            .find(|&&line| line > from_line)
            .copied()
    }

    /// Returns the total count of recorded command starts.
    pub fn command_count(&self) -> usize {
        self.command_starts.len()
    }

    /// Returns true if there are any command boundaries recorded.
    pub fn has_boundaries(&self) -> bool {
        !self.command_starts.is_empty()
    }

    /// Clears all recorded boundaries.
    /// Call this when the scrollback buffer is cleared.
    pub fn clear(&mut self) {
        self.command_starts.clear();
        self.prompt_lines.clear();
        self.records.clear();
        self.pending = None;
        self.pending_seeded = false;
    }

    /// Adjusts all indices after a buffer trim operation.
    ///
    /// When the scrollback buffer drops old lines, call this with
    /// the number of lines dropped to keep indices valid.
    pub fn adjust_for_dropped_lines(&mut self, dropped_count: usize) {
        if dropped_count == 0 {
            return;
        }

        // Remove boundaries that are now out of range
        self.command_starts.retain(|&line| line >= dropped_count);
        self.prompt_lines.retain(|&line| line >= dropped_count);
        self.records
            .retain(|record| record.prompt_line.unwrap_or(record.output_start) >= dropped_count);

        // Adjust remaining indices
        for line in &mut self.command_starts {
            *line -= dropped_count;
        }
        for line in &mut self.prompt_lines {
            *line -= dropped_count;
        }
        for record in &mut self.records {
            record.output_start = record.output_start.saturating_sub(dropped_count);
            if let Some(prompt_line) = &mut record.prompt_line {
                *prompt_line = prompt_line.saturating_sub(dropped_count);
            }
        }
        if let Some(pending) = &mut self.pending {
            pending.output_start = pending.output_start.saturating_sub(dropped_count);
            if let Some(prompt_line) = &mut pending.prompt_line {
                *prompt_line = prompt_line.saturating_sub(dropped_count);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_markers() {
        let mut bounds = CommandBoundaries::new();

        bounds.record_marker(&MarkerEvent::Preexec, 10);
        bounds.record_marker(&MarkerEvent::Precmd { exit_code: 0 }, 20);
        bounds.record_marker(&MarkerEvent::Preexec, 25);
        bounds.record_marker(&MarkerEvent::Precmd { exit_code: 1 }, 50);

        assert_eq!(bounds.command_starts, vec![10, 25]);
        assert_eq!(bounds.prompt_lines, vec![20, 50]);
    }

    #[test]
    fn test_prev_next_command() {
        let mut bounds = CommandBoundaries::new();
        bounds.command_starts = vec![10, 30, 60, 100];

        // Previous command from middle
        assert_eq!(bounds.prev_command(50), Some(30));
        assert_eq!(bounds.prev_command(30), Some(10));
        assert_eq!(bounds.prev_command(10), None);

        // Next command from middle
        assert_eq!(bounds.next_command(20), Some(30));
        assert_eq!(bounds.next_command(60), Some(100));
        assert_eq!(bounds.next_command(100), None);
    }

    #[test]
    fn test_adjust_for_dropped_lines() {
        let mut bounds = CommandBoundaries::new();
        bounds.command_starts = vec![10, 30, 60];
        bounds.prompt_lines = vec![20, 50];

        // Drop first 25 lines
        bounds.adjust_for_dropped_lines(25);

        // 10 and 20 should be removed, others adjusted
        assert_eq!(bounds.command_starts, vec![5, 35]); // 30-25=5, 60-25=35
        assert_eq!(bounds.prompt_lines, vec![25]); // 50-25=25
    }

    #[test]
    fn test_clear() {
        let mut bounds = CommandBoundaries::new();
        bounds.command_starts = vec![10, 30];
        bounds.prompt_lines = vec![20];

        bounds.clear();
        assert!(bounds.command_starts.is_empty());
        assert!(bounds.prompt_lines.is_empty());
    }

    #[test]
    fn test_folded_line_count_in_range_when_folded_excludes_output_start_line() {
        let mut bounds = CommandBoundaries::new();
        bounds.records.push(CommandRecord {
            output_start: 10,
            prompt_line: Some(15),
            folded: true,
            ..Default::default()
        });

        assert_eq!(bounds.folded_line_count_in_range(0, 30), 4);
    }
}
