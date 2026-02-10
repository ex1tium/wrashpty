//! Scrollback viewer and scroll view mode handling.

use anyhow::{Context, Result};
use tracing::{debug, warn};

use crate::chrome::{GitInfo, ScrollInfo, TopbarState};
use crate::terminal::TerminalGuard;
use crate::types::Mode;

use super::App;

/// Actions recognized from scroll key sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrollAction {
    /// Page up (scroll back one screen)
    PageUp,
    /// Page down (scroll forward one screen)
    PageDown,
    /// Scroll up one line (Shift+PgUp)
    LineUp,
    /// Scroll down one line (Shift+PgDown)
    LineDown,
    /// Jump to top (oldest content)
    Home,
    /// Jump to bottom (live view)
    End,
}

fn filter_offset_for_line_with_viewport(
    filter: &crate::scrollback::features::FilterState,
    line_idx: usize,
    viewport_height: usize,
) -> usize {
    if let Some(pos) = filter
        .matching_lines
        .iter()
        .position(|&idx| idx == line_idx)
    {
        let total = filter.matching_lines.len();
        let pos_from_bottom = total.saturating_sub(pos + 1);
        pos_from_bottom.saturating_sub(viewport_height / 2)
    } else {
        0
    }
}

fn resolve_fold_target_line(
    cached_first_visible_line_idx: Option<usize>,
    total_lines: usize,
    offset: usize,
    viewport_height: usize,
) -> Option<usize> {
    if total_lines == 0 {
        return None;
    }

    if let Some(line_idx) = cached_first_visible_line_idx {
        if line_idx < total_lines {
            return Some(line_idx);
        }
    }

    let fallback = total_lines
        .saturating_sub(offset)
        .saturating_sub(viewport_height.max(1))
        .min(total_lines.saturating_sub(1));
    Some(fallback)
}

fn hidden_line_count_for_record(
    record: &crate::scrollback::CommandRecord,
    total_lines: usize,
) -> usize {
    record
        .prompt_line
        .unwrap_or(total_lines)
        .saturating_sub(record.output_start.saturating_add(1))
}

fn resolve_fold_target_record_index(
    boundaries: &crate::scrollback::CommandBoundaries,
    line_idx: usize,
    total_lines: usize,
) -> Option<usize> {
    if let Some((record_idx, record)) = boundaries.record_for_line(line_idx) {
        if hidden_line_count_for_record(record, total_lines) > 0 {
            return Some(record_idx);
        }
    }

    if let Some((record_idx, _)) = boundaries
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            hidden_line_count_for_record(record, total_lines) > 0 && record.output_start >= line_idx
        })
        .min_by_key(|(_, record)| record.output_start)
    {
        return Some(record_idx);
    }

    boundaries
        .records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, record)| {
            hidden_line_count_for_record(record, total_lines) > 0 && record.output_start < line_idx
        })
        .map(|(record_idx, _)| record_idx)
}

fn try_scroll_action_prefix_bytes(bytes: &[u8]) -> Option<(ScrollAction, usize)> {
    if bytes.starts_with(b"\x1b[5~") {
        return Some((ScrollAction::PageUp, 4));
    }
    if bytes.starts_with(b"\x1b[6~") {
        return Some((ScrollAction::PageDown, 4));
    }
    if bytes.starts_with(b"\x1b[5;2~") {
        return Some((ScrollAction::LineUp, 6));
    }
    if bytes.starts_with(b"\x1b[6;2~") {
        return Some((ScrollAction::LineDown, 6));
    }
    if bytes.starts_with(b"\x1b[1~") {
        return Some((ScrollAction::Home, 4));
    }
    if bytes.starts_with(b"\x1b[H") {
        return Some((ScrollAction::Home, 3));
    }
    if bytes.starts_with(b"\x1b[4~") {
        return Some((ScrollAction::End, 4));
    }
    if bytes.starts_with(b"\x1b[F") {
        return Some((ScrollAction::End, 3));
    }

    None
}

impl App {
    /// Processes captured bytes for the scrollback system.
    ///
    /// This method:
    /// 1. Feeds bytes through the alt-screen detector
    /// 2. Suspends/resumes capture when entering/exiting alt-screen
    /// 3. Parses output into lines via CaptureState
    /// 4. Stores lines in ScrollbackBuffer
    pub(super) fn capture_for_scrollback(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        // Check for alt-screen transitions
        for event in self.alt_screen_detector.parse_bytes(bytes) {
            match event {
                crate::scrollback::AltScreenEvent::Enter => {
                    debug!("Alt-screen entered, suspending scrollback capture");
                    self.scrollback_buffer.suspend_capture();
                }
                crate::scrollback::AltScreenEvent::Exit => {
                    debug!("Alt-screen exited, resuming scrollback capture");
                    self.scrollback_buffer.resume_capture();
                }
            }
        }

        // Optional raw byte dump for debugging capture issues.
        // Set WRASHPTY_CAPTURE_RAW=1 to write raw PTY bytes to a secure temp file.
        if self.raw_capture_fd.is_some() {
            use std::io::Write;
            if let Some(ref mut fd) = self.raw_capture_fd {
                let _ = fd.write_all(bytes);
            }
        }

        // Parse bytes into lines and add to buffer
        if self.scrollback_buffer.is_capture_active() {
            let captured_lines: Vec<_> = self.capture_state.feed(bytes).collect();
            for captured in captured_lines {
                self.apply_captured_line(captured);
            }
        }

        // If we're scrolled back and new output arrives, return to live view
        if self.scroll_state.is_scrolled() {
            debug!("New output arrived while scrolled, returning to live view");
            self.scroll_state = crate::types::ScrollState::Live;
        }
    }

    /// Returns whether scrolling is currently allowed.
    ///
    /// Scrolling is allowed when:
    /// - Not in alternate screen buffer (vim, htop, etc.)
    /// - Scrollback buffer has content
    /// - In Edit or Passthrough mode (not during mode transitions)
    #[inline]
    pub(super) fn is_scroll_allowed(&self) -> bool {
        !self.alt_screen_detector.is_in_alt_screen()
            && !self.scrollback_buffer.is_empty()
            && matches!(self.mode, Mode::Edit | Mode::Passthrough)
    }

    /// Scrolls the view up by the specified number of lines.
    pub(super) fn scroll_up(&mut self, lines: usize) {
        if !self.is_scroll_allowed() {
            return;
        }

        let total = self.scrollback_buffer.len();
        let folded = self
            .viewer_state
            .boundaries
            .folded_line_count_in_range(0, total);
        let visible_total = total.saturating_sub(folded);
        let max_offset =
            crate::scrollback::ScrollViewer::max_offset(visible_total, self.viewport_height());

        let current = self.scroll_state.offset();
        let new_offset = (current + lines).min(max_offset);
        // Use scrolled_at to stay in scroll mode
        self.scroll_state = crate::types::ScrollState::scrolled_at(new_offset);

        debug!(from = current, to = new_offset, "Scrolled up");
    }

    /// Scrolls the view down by the specified number of lines.
    ///
    /// Stays in scroll mode even at offset=0. Use `scroll_to_bottom()` to
    /// exit scroll mode and return to live view.
    pub(super) fn scroll_down(&mut self, lines: usize) {
        let current = self.scroll_state.offset();
        let new_offset = current.saturating_sub(lines);
        // Use scrolled_at to stay in scroll mode even at offset=0
        self.scroll_state = crate::types::ScrollState::scrolled_at(new_offset);

        debug!(from = current, to = new_offset, "Scrolled down");
    }

    /// Scrolls to the top (oldest content).
    pub(super) fn scroll_to_top(&mut self) {
        if !self.is_scroll_allowed() {
            return;
        }

        let total = self.scrollback_buffer.len();
        let folded = self
            .viewer_state
            .boundaries
            .folded_line_count_in_range(0, total);
        let visible_total = total.saturating_sub(folded);
        let max_offset =
            crate::scrollback::ScrollViewer::max_offset(visible_total, self.viewport_height());
        // Use scrolled_at to stay in scroll mode
        self.scroll_state = crate::types::ScrollState::scrolled_at(max_offset);

        debug!(offset = max_offset, "Scrolled to top");
    }

    /// Scrolls to the bottom (live view).
    pub(super) fn scroll_to_bottom(&mut self) {
        self.scroll_state = crate::types::ScrollState::Live;
        debug!("Scrolled to bottom (live view)");
    }

    /// Records a marker event for command boundary navigation.
    ///
    /// Call this when a marker is detected during PTY output processing.
    /// The boundary is recorded at the current buffer length.
    pub(super) fn record_command_boundary(&mut self, event: &crate::types::MarkerEvent) {
        let line_index = self.scrollback_buffer.len();
        match event {
            crate::types::MarkerEvent::Preexec => {
                self.viewer_state.boundaries.start_record(
                    line_index,
                    self.last_command.clone(),
                    Some(self.current_cwd.clone()),
                    Some(chrono::Local::now().naive_local()),
                );
            }
            crate::types::MarkerEvent::Precmd { exit_code } => {
                let duration = self.command_start_time.map(|start| start.elapsed());
                self.viewer_state
                    .boundaries
                    .complete_record(line_index, *exit_code, duration);
            }
            crate::types::MarkerEvent::Prompt => {
                // PRECMD should complete the same boundary first. Fallback:
                // complete any pending record if marker ordering is unexpected.
                if self.viewer_state.boundaries.has_pending_record() {
                    let duration = self.command_start_time.map(|start| start.elapsed());
                    self.viewer_state.boundaries.complete_record(
                        line_index,
                        self.last_exit_code,
                        duration,
                    );
                } else if self
                    .viewer_state
                    .boundaries
                    .record_for_prompt_line(line_index)
                    .is_none()
                {
                    self.viewer_state.boundaries.record_prompt_line(line_index);
                }
            }
        }
    }

    /// Jumps to the previous command boundary (Ctrl+P in scroll view).
    pub(super) fn jump_to_prev_command(&mut self) {
        let total = self.scrollback_buffer.len();
        if total == 0 {
            return;
        }

        let offset = self.scroll_state.offset();
        let viewport = self.viewport_height();
        let max_offset = crate::scrollback::ScrollViewer::max_offset(total, viewport);

        // Calculate first visible line (1-indexed, at top of viewport)
        // offset=0 means we see the newest lines at bottom
        // first_visible = total - offset - viewport + 1 (clamped to 1)
        let first_visible = total
            .saturating_sub(offset)
            .saturating_sub(viewport)
            .saturating_add(1)
            .max(1);

        debug!(
            total,
            offset,
            first_visible,
            command_count = self.viewer_state.boundaries.command_count(),
            "Looking for previous command"
        );

        // Find command that starts before our current first visible line
        if let Some(boundary) = self.viewer_state.boundaries.prev_command(first_visible) {
            // Boundary points to where output STARTS, add 1 to skip the prompt/command line
            // that was captured before the Preexec marker
            let target_line = boundary.saturating_add(1);
            // Calculate offset to show target_line near the top of viewport
            // offset = total - (target_line + viewport - 1) = total - target_line - viewport + 1
            let new_offset = total
                .saturating_sub(target_line)
                .saturating_sub(viewport)
                .saturating_add(1);
            self.scroll_state = crate::types::ScrollState::scrolled_at(new_offset.min(max_offset));
            debug!(
                boundary,
                target_line, new_offset, "Jumped to previous command"
            );
        } else {
            debug!("No previous command found");
        }
    }

    /// Jumps to the next command boundary (Ctrl+N in scroll view).
    pub(super) fn jump_to_next_command(&mut self) {
        let total = self.scrollback_buffer.len();
        if total == 0 {
            return;
        }

        let offset = self.scroll_state.offset();
        let viewport = self.viewport_height();

        // Calculate first visible line (1-indexed)
        let first_visible = total
            .saturating_sub(offset)
            .saturating_sub(viewport)
            .saturating_add(1)
            .max(1);

        debug!(
            total,
            offset,
            first_visible,
            command_count = self.viewer_state.boundaries.command_count(),
            "Looking for next command"
        );

        // Find command that starts after our current first visible line
        if let Some(boundary) = self.viewer_state.boundaries.next_command(first_visible) {
            // Boundary points to where output STARTS, add 1 to skip the prompt/command line
            let target_line = boundary.saturating_add(1);
            // Calculate offset to show target_line near the top of viewport
            let new_offset = total
                .saturating_sub(target_line)
                .saturating_sub(viewport)
                .saturating_add(1);
            self.scroll_state = crate::types::ScrollState::scrolled_at(new_offset.max(0));
            debug!(target_line, new_offset, "Jumped to next command");
        } else {
            debug!("No next command found");
        }
    }

    /// Runs the go-to-line mini-input mode.
    ///
    /// Returns Ok(true) if user submitted a valid line number,
    /// Ok(false) if user cancelled.
    pub(super) fn run_goto_line_mode(&mut self) -> Result<bool> {
        use crate::chrome::segments::{color_to_bg_ansi, color_to_fg_ansi};
        use crossterm::event::{self, Event, KeyEventKind};

        let mut input = crate::scrollback::MiniInput::with_hint("Go to line", "line number");
        let total = self.scrollback_buffer.len();

        // Get theme colors for consistent topbar styling
        let theme = self.chrome.theme();
        let bg_ansi = color_to_bg_ansi(theme.bar_bg);
        let label_ansi = color_to_fg_ansi(theme.text_secondary);
        let text_ansi = color_to_fg_ansi(theme.text_primary);

        loop {
            // Render the mini-input with topbar styling
            let (cols, _) = TerminalGuard::get_size()?;
            let status = format!("/{}", total);
            input.render_styled(
                &mut std::io::stdout(),
                cols,
                Some(&status),
                Some(&bg_ansi),
                Some(&label_ansi),
                Some(&text_ansi),
            )?;
            self.render_legend_for_context(crate::scrollback::HelpContext::GoToLine)?;

            // Wait for input
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Only handle press events
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    match input.handle_input(key) {
                        crate::scrollback::MiniInputResult::Submit => {
                            // Parse and jump to line
                            if let Ok(line_num) = input.text().parse::<usize>() {
                                if line_num > 0 && line_num <= total {
                                    self.scroll_to_line(line_num);
                                    return Ok(true);
                                }
                            }
                            // Invalid input, just return without changing position
                            return Ok(false);
                        }
                        crate::scrollback::MiniInputResult::Cancel => {
                            return Ok(false);
                        }
                        crate::scrollback::MiniInputResult::Continue
                        | crate::scrollback::MiniInputResult::Changed => {
                            // Keep editing
                        }
                    }
                }
            }
        }
    }

    /// Runs the incremental search mode (Ctrl+S).
    ///
    /// Returns true if search was performed (user may have scrolled to a match),
    /// false if cancelled.
    pub(super) fn run_search_mode(&mut self) -> Result<bool> {
        use crate::chrome::segments::{color_to_bg_ansi, color_to_fg_ansi};
        use crate::scrollback::features::SearchState;
        use crossterm::event::{self, Event, KeyCode, KeyEventKind};

        let mut search = SearchState::new();
        let mut input = crate::scrollback::MiniInput::with_hint("Search", "pattern");

        // Get theme colors for consistent topbar styling
        let theme = self.chrome.theme();
        let bg_ansi = color_to_bg_ansi(theme.bar_bg);
        let label_ansi = color_to_fg_ansi(theme.text_secondary);
        let text_ansi = color_to_fg_ansi(theme.text_primary);

        // Track if we found any matches
        let mut scrolled_to_match = false;

        loop {
            // Render search input with match count status
            let (cols, _) = TerminalGuard::get_size()?;
            let status = search.status();
            let status_ref = if status.is_empty() {
                None
            } else {
                Some(status.as_str())
            };
            input.render_styled(
                &mut std::io::stdout(),
                cols,
                status_ref,
                Some(&bg_ansi),
                Some(&label_ansi),
                Some(&text_ansi),
            )?;
            self.render_legend_for_context(crate::scrollback::HelpContext::Search)?;

            // Wait for input
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Only handle press events
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    // Handle Ctrl+F: Enter filter-within-search mode
                    if key.code == KeyCode::Char('f')
                        && key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        if search.is_match_available() {
                            // Cache search results for filter mode
                            let search_lines = search.matched_line_indices();
                            self.viewer_state.last_search_lines = Some(search_lines);

                            // Run filter mode with search results as base
                            match self.run_filter_mode() {
                                Ok(_navigated) => {
                                    // Re-render search view after returning from filter
                                    self.render_scrollback_with_search(&search)?;
                                }
                                Err(e) => {
                                    warn!("Filter within search failed: {}", e);
                                }
                            }
                        }
                        continue;
                    }

                    // Handle Up/Down arrows for match navigation while in search mode
                    match key.code {
                        KeyCode::Down => {
                            // Next match (down = forward in buffer)
                            search.next_match();
                            if let Some(m) = search.current() {
                                self.scroll_to_line(m.line + 1); // Convert 0-indexed to 1-indexed
                                scrolled_to_match = true;
                            }
                            // Re-render scrollback with highlights
                            self.render_scrollback_with_search(&search)?;
                            continue;
                        }
                        KeyCode::Up => {
                            // Previous match (up = backward in buffer)
                            search.prev_match();
                            if let Some(m) = search.current() {
                                self.scroll_to_line(m.line + 1);
                                scrolled_to_match = true;
                            }
                            self.render_scrollback_with_search(&search)?;
                            continue;
                        }
                        _ => {}
                    }

                    match input.handle_input(key) {
                        crate::scrollback::MiniInputResult::Submit => {
                            // Store search results for filter+search combination
                            if search.is_match_available() {
                                self.viewer_state.last_search_lines =
                                    Some(search.matched_line_indices());
                            } else {
                                self.viewer_state.last_search_lines = None;
                            }
                            // Exit search mode, keep current position
                            return Ok(scrolled_to_match);
                        }
                        crate::scrollback::MiniInputResult::Cancel => {
                            // Clear search results on cancel
                            self.viewer_state.last_search_lines = None;
                            return Ok(false);
                        }
                        crate::scrollback::MiniInputResult::Changed => {
                            // Update search query and re-search
                            search.query = input.text().to_string();
                            search.cursor = input.cursor;

                            // Calculate viewport position for "nearest match" selection
                            let total = self.scrollback_buffer.len();
                            let viewport = self.viewport_height();
                            let offset = self.scroll_state.offset();
                            let first_visible =
                                total.saturating_sub(offset).saturating_sub(viewport).max(0);

                            // Perform incremental search
                            search.perform_search(&self.scrollback_buffer, first_visible);

                            // Jump to current match if any
                            if let Some(m) = search.current() {
                                self.scroll_to_line(m.line + 1);
                                scrolled_to_match = true;
                            }

                            // Re-render with highlights
                            self.render_scrollback_with_search(&search)?;
                        }
                        crate::scrollback::MiniInputResult::Continue => {
                            // Keep editing, no change to query
                        }
                    }
                }
            }
        }
    }

    /// Renders scrollback view with search highlights.
    pub(super) fn render_scrollback_with_search(
        &mut self,
        search: &crate::scrollback::features::SearchState,
    ) -> Result<()> {
        use std::io::Write;

        let (cols, rows) = TerminalGuard::get_size()?;
        let offset = self.scroll_state.offset();
        let mut out = std::io::stdout();
        let content_rows = if self.viewer_state.is_help_bar_shown() {
            rows.saturating_sub(2)
        } else {
            rows.saturating_sub(1)
        };

        let boundary_lines = if self.viewer_state.is_command_separators_shown() {
            &self.viewer_state.boundaries.command_starts
        } else {
            &[] as &[usize]
        };
        let stats = crate::scrollback::ScrollViewer::render(
            &mut out,
            &self.scrollback_buffer,
            offset,
            cols,
            content_rows,
            &crate::scrollback::RenderConfig {
                start_row: 2,
                show_line_numbers: self.viewer_state.is_line_numbers_shown(),
                show_timestamps: self.viewer_state.is_timestamps_shown(),
                boundary_markers: true,
                boundary_lines,
                records: &self.viewer_state.boundaries.records,
                search: Some(search),
                separator_registry: Some(&self.viewer_state.separator_registry),
                symbols: Some(self.chrome.symbols()),
                collapsed_commands: Some(&self.viewer_state.collapsed_commands),
                sticky_header: self.viewer_state.display.sticky_headers
                    && self.viewer_state.is_command_separators_shown(),
                theme: Some(self.chrome.theme()),
                ..Default::default()
            },
        )?;
        let first_visible_idx = stats.first_visible_line.saturating_sub(1);
        self.viewer_state.last_first_visible_line_idx =
            (first_visible_idx < self.scrollback_buffer.len()).then_some(first_visible_idx);

        if self.viewer_state.is_help_bar_shown() {
            crate::scrollback::features::LegendBar::render(
                &mut out,
                crate::scrollback::HelpContext::Search,
                &self.viewer_state.display,
                cols,
                rows,
                self.chrome.theme(),
            )?;
        }

        out.flush()?;
        Ok(())
    }

    /// Runs the filter mode (Ctrl+F).
    ///
    /// In filter mode, only lines matching the pattern are displayed.
    /// The user can scroll through filtered results.
    /// Returns true if user navigated to a position, false if cancelled.
    pub(super) fn run_filter_mode(&mut self) -> Result<bool> {
        use crate::chrome::segments::{color_to_bg_ansi, color_to_fg_ansi};
        use crate::scrollback::features::FilterState;
        use crossterm::event::{self, Event, KeyCode, KeyEventKind};

        let mut filter = FilterState::new();

        // Check if we have search results to use as base filter
        let using_search_results = self.viewer_state.last_search_lines.is_some();
        let hint = if using_search_results {
            "filter search results"
        } else {
            "pattern"
        };
        let mut input = crate::scrollback::MiniInput::with_hint("Filter", hint);

        // Pre-populate filter with search results if available
        if let Some(ref lines) = self.viewer_state.last_search_lines {
            filter.matching_lines = lines.clone();
        }

        // Track filter-specific scroll offset
        let mut filter_offset: usize = 0;
        let mut navigated = false;

        // Initial render if we have pre-populated results
        if using_search_results && !filter.matching_lines.is_empty() {
            self.render_scrollback_with_filter(&filter, filter_offset)?;
        }

        // Get theme colors for consistent topbar styling
        let theme = self.chrome.theme();
        let bg_ansi = color_to_bg_ansi(theme.bar_bg);
        let label_ansi = color_to_fg_ansi(theme.text_secondary);
        let text_ansi = color_to_fg_ansi(theme.text_primary);

        loop {
            // Render filter input with match count status
            let (cols, _) = TerminalGuard::get_size()?;
            let status = filter.status();
            let status_ref = if status.is_empty() {
                None
            } else {
                Some(status.as_str())
            };
            input.render_styled(
                &mut std::io::stdout(),
                cols,
                status_ref,
                Some(&bg_ansi),
                Some(&label_ansi),
                Some(&text_ansi),
            )?;
            self.render_legend_for_context(crate::scrollback::HelpContext::Filter)?;

            // Wait for input
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Only handle press events
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    // Handle navigation keys while in filter mode
                    match key.code {
                        KeyCode::PageUp => {
                            // Scroll up in filtered view
                            let viewport = self.viewport_height();
                            filter_offset = filter_offset.saturating_add(viewport);
                            let max = crate::scrollback::ScrollViewer::max_offset(
                                filter.match_count(),
                                viewport,
                            );
                            filter_offset = filter_offset.min(max);
                            self.render_scrollback_with_filter(&filter, filter_offset)?;
                            navigated = true;
                            continue;
                        }
                        KeyCode::PageDown => {
                            // Scroll down in filtered view
                            let viewport = self.viewport_height();
                            filter_offset = filter_offset.saturating_sub(viewport);
                            self.render_scrollback_with_filter(&filter, filter_offset)?;
                            navigated = true;
                            continue;
                        }
                        KeyCode::Up => {
                            filter_offset = filter_offset.saturating_add(1);
                            let max = crate::scrollback::ScrollViewer::max_offset(
                                filter.match_count(),
                                self.viewport_height(),
                            );
                            filter_offset = filter_offset.min(max);
                            self.render_scrollback_with_filter(&filter, filter_offset)?;
                            navigated = true;
                            continue;
                        }
                        KeyCode::Down => {
                            filter_offset = filter_offset.saturating_sub(1);
                            self.render_scrollback_with_filter(&filter, filter_offset)?;
                            navigated = true;
                            continue;
                        }
                        KeyCode::Home => {
                            let max = crate::scrollback::ScrollViewer::max_offset(
                                filter.match_count(),
                                self.viewport_height(),
                            );
                            filter_offset = max;
                            self.render_scrollback_with_filter(&filter, filter_offset)?;
                            navigated = true;
                            continue;
                        }
                        KeyCode::End => {
                            filter_offset = 0;
                            self.render_scrollback_with_filter(&filter, filter_offset)?;
                            navigated = true;
                            continue;
                        }
                        KeyCode::Char('s')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            // Ctrl+S: Enter search-within-filter mode
                            if filter.is_any_match() {
                                // Run search within the filtered lines
                                match self.run_search_within_filter_mode(&filter, filter_offset) {
                                    Ok((new_offset, did_navigate)) => {
                                        filter_offset = new_offset;
                                        if did_navigate {
                                            navigated = true;
                                        }
                                        // Re-render filter view after returning from search
                                        self.render_scrollback_with_filter(&filter, filter_offset)?;
                                    }
                                    Err(e) => {
                                        warn!("Search within filter failed: {}", e);
                                    }
                                }
                            }
                            continue;
                        }
                        _ => {}
                    }

                    match input.handle_input(key) {
                        crate::scrollback::MiniInputResult::Submit => {
                            // Exit filter mode, restore full view at current position
                            return Ok(navigated);
                        }
                        crate::scrollback::MiniInputResult::Cancel => {
                            return Ok(false);
                        }
                        crate::scrollback::MiniInputResult::Changed => {
                            // Update filter pattern and re-filter
                            filter.pattern = input.text().to_string();
                            filter.cursor = input.cursor;

                            if using_search_results {
                                // Filter within search results
                                if filter.pattern.is_empty() {
                                    // Restore original search results
                                    if let Some(ref lines) = self.viewer_state.last_search_lines {
                                        filter.matching_lines = lines.clone();
                                    }
                                } else {
                                    // Filter search results by additional pattern
                                    if let Some(lines) =
                                        self.viewer_state.last_search_lines.as_ref()
                                    {
                                        filter
                                            .perform_filter_within(&self.scrollback_buffer, lines);
                                    } else {
                                        filter.clear_matches();
                                    }
                                }
                            } else {
                                // Normal filtering - search entire buffer
                                filter.perform_filter(&self.scrollback_buffer);
                            }

                            // Reset to bottom of filtered view
                            filter_offset = 0;

                            // Re-render with filter
                            self.render_scrollback_with_filter(&filter, filter_offset)?;
                        }
                        crate::scrollback::MiniInputResult::Continue => {
                            // Keep editing, no change to pattern
                        }
                    }
                }
            }
        }
    }

    /// Renders scrollback view with filter active (only matching lines).
    pub(super) fn render_scrollback_with_filter(
        &mut self,
        filter: &crate::scrollback::features::FilterState,
        filter_offset: usize,
    ) -> Result<()> {
        use std::io::Write;

        let (cols, rows) = TerminalGuard::get_size()?;
        let mut out = std::io::stdout();
        let content_rows = if self.viewer_state.is_help_bar_shown() {
            rows.saturating_sub(2)
        } else {
            rows.saturating_sub(1)
        };

        let stats = crate::scrollback::ScrollViewer::render(
            &mut out,
            &self.scrollback_buffer,
            0,
            cols,
            content_rows,
            &crate::scrollback::RenderConfig {
                start_row: 2,
                show_line_numbers: self.viewer_state.is_line_numbers_shown(),
                show_timestamps: self.viewer_state.is_timestamps_shown(),
                records: &self.viewer_state.boundaries.records,
                filter: Some(filter),
                filter_offset,
                separator_registry: Some(&self.viewer_state.separator_registry),
                symbols: Some(self.chrome.symbols()),
                collapsed_commands: Some(&self.viewer_state.collapsed_commands),
                sticky_header: false,
                ..Default::default()
            },
        )?;
        let first_visible_idx = stats.first_visible_line.saturating_sub(1);
        self.viewer_state.last_first_visible_line_idx =
            (first_visible_idx < self.scrollback_buffer.len()).then_some(first_visible_idx);

        if self.viewer_state.is_help_bar_shown() {
            crate::scrollback::features::LegendBar::render(
                &mut out,
                crate::scrollback::HelpContext::Filter,
                &self.viewer_state.display,
                cols,
                rows,
                self.chrome.theme(),
            )?;
        }

        out.flush()?;
        Ok(())
    }

    /// Runs search mode within a filtered view (Ctrl+S while in filter mode).
    ///
    /// Searches only within the lines that passed the filter.
    /// Returns (new_filter_offset, navigated_to_match).
    pub(super) fn run_search_within_filter_mode(
        &mut self,
        filter: &crate::scrollback::features::FilterState,
        initial_offset: usize,
    ) -> Result<(usize, bool)> {
        use crate::chrome::segments::{color_to_bg_ansi, color_to_fg_ansi};
        use crate::scrollback::features::SearchState;
        use crossterm::event::{self, Event, KeyCode, KeyEventKind};

        let mut search = SearchState::new();
        let mut input = crate::scrollback::MiniInput::with_hint("Search", "in filtered");

        // Get theme colors for consistent topbar styling
        let theme = self.chrome.theme();
        let bg_ansi = color_to_bg_ansi(theme.bar_bg);
        let label_ansi = color_to_fg_ansi(theme.text_secondary);
        let text_ansi = color_to_fg_ansi(theme.text_primary);

        let mut filter_offset = initial_offset;
        let mut scrolled_to_match = false;

        loop {
            // Render search input with match count status
            let (cols, _) = TerminalGuard::get_size()?;
            let status = search.status();
            let status_ref = if status.is_empty() {
                None
            } else {
                Some(status.as_str())
            };
            input.render_styled(
                &mut std::io::stdout(),
                cols,
                status_ref,
                Some(&bg_ansi),
                Some(&label_ansi),
                Some(&text_ansi),
            )?;
            self.render_legend_for_context(crate::scrollback::HelpContext::Search)?;

            // Wait for input
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Only handle press events
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }

                    // Handle Up/Down arrows for match navigation
                    match key.code {
                        KeyCode::Down => {
                            // Next match
                            search.next_match();
                            if let Some(m) = search.current() {
                                // Calculate filter offset to show this match
                                filter_offset = self.filter_offset_for_line(filter, m.line);
                                scrolled_to_match = true;
                            }
                            self.render_scrollback_with_filter_and_search(
                                filter,
                                filter_offset,
                                &search,
                            )?;
                            continue;
                        }
                        KeyCode::Up => {
                            // Previous match
                            search.prev_match();
                            if let Some(m) = search.current() {
                                filter_offset = self.filter_offset_for_line(filter, m.line);
                                scrolled_to_match = true;
                            }
                            self.render_scrollback_with_filter_and_search(
                                filter,
                                filter_offset,
                                &search,
                            )?;
                            continue;
                        }
                        _ => {}
                    }

                    match input.handle_input(key) {
                        crate::scrollback::MiniInputResult::Submit => {
                            // Exit search mode, keep current filter offset
                            return Ok((filter_offset, scrolled_to_match));
                        }
                        crate::scrollback::MiniInputResult::Cancel => {
                            return Ok((initial_offset, false));
                        }
                        crate::scrollback::MiniInputResult::Changed => {
                            // Update search query and re-search within filtered lines
                            search.query = input.text().to_string();
                            search.cursor = input.cursor;

                            // Get first visible line in filtered view for "nearest match" selection
                            let first_visible =
                                if let Some(&first_idx) = filter.matching_lines.first() {
                                    first_idx
                                } else {
                                    0
                                };

                            // Perform search only within filtered lines
                            search.perform_search_within(
                                &self.scrollback_buffer,
                                &filter.matching_lines,
                                first_visible,
                            );

                            // Jump to current match if any
                            if let Some(m) = search.current() {
                                filter_offset = self.filter_offset_for_line(filter, m.line);
                                scrolled_to_match = true;
                            }

                            // Re-render with highlights
                            self.render_scrollback_with_filter_and_search(
                                filter,
                                filter_offset,
                                &search,
                            )?;
                        }
                        crate::scrollback::MiniInputResult::Continue => {
                            // Keep editing, no change to query
                        }
                    }
                }
            }
        }
    }

    /// Calculates filter offset to show a specific line (by original buffer index).
    pub(super) fn filter_offset_for_line(
        &self,
        filter: &crate::scrollback::features::FilterState,
        line_idx: usize,
    ) -> usize {
        filter_offset_for_line_with_viewport(filter, line_idx, self.viewport_height())
    }

    /// Renders scrollback with filter AND search active.
    pub(super) fn render_scrollback_with_filter_and_search(
        &mut self,
        filter: &crate::scrollback::features::FilterState,
        filter_offset: usize,
        search: &crate::scrollback::features::SearchState,
    ) -> Result<()> {
        use std::io::Write;

        let (cols, rows) = TerminalGuard::get_size()?;
        let mut out = std::io::stdout();
        let content_rows = if self.viewer_state.is_help_bar_shown() {
            rows.saturating_sub(2)
        } else {
            rows.saturating_sub(1)
        };

        let stats = crate::scrollback::ScrollViewer::render(
            &mut out,
            &self.scrollback_buffer,
            0,
            cols,
            content_rows,
            &crate::scrollback::RenderConfig {
                start_row: 2,
                show_line_numbers: self.viewer_state.is_line_numbers_shown(),
                show_timestamps: self.viewer_state.is_timestamps_shown(),
                records: &self.viewer_state.boundaries.records,
                filter: Some(filter),
                filter_offset,
                search: Some(search),
                separator_registry: Some(&self.viewer_state.separator_registry),
                symbols: Some(self.chrome.symbols()),
                collapsed_commands: Some(&self.viewer_state.collapsed_commands),
                sticky_header: false,
                theme: Some(self.chrome.theme()),
                ..Default::default()
            },
        )?;
        let first_visible_idx = stats.first_visible_line.saturating_sub(1);
        self.viewer_state.last_first_visible_line_idx =
            (first_visible_idx < self.scrollback_buffer.len()).then_some(first_visible_idx);

        if self.viewer_state.is_help_bar_shown() {
            crate::scrollback::features::LegendBar::render(
                &mut out,
                crate::scrollback::HelpContext::Search,
                &self.viewer_state.display,
                cols,
                rows,
                self.chrome.theme(),
            )?;
        }

        out.flush()?;
        Ok(())
    }

    /// Scrolls to show a specific line number (1-indexed).
    pub(super) fn scroll_to_line(&mut self, line_num: usize) {
        let total = self.scrollback_buffer.len();
        let viewport = self.viewport_height();
        let max_offset = crate::scrollback::ScrollViewer::max_offset(total, viewport);

        // Calculate offset to show line_num near the top of viewport
        // offset = total - line_at_bottom
        // line_at_bottom = line_num + viewport - 1 (to show line_num at top)
        let line_at_bottom = line_num.saturating_add(viewport).saturating_sub(1);
        let offset = total.saturating_sub(line_at_bottom);

        self.scroll_state = crate::types::ScrollState::scrolled_at(offset.min(max_offset));
        debug!(line_num, offset, "Scrolled to line");
    }

    /// Returns the viewport height for scroll calculations.
    ///
    /// Accounts for rows reserved by the topbar (when scrolled) and the
    /// legend/help bar (when visible).
    pub(super) fn viewport_height(&self) -> usize {
        let rows = match TerminalGuard::get_size() {
            Ok((_, rows)) => rows as usize,
            Err(_) => 24, // Fallback
        };
        let mut reserved = 0;
        // Reserve 1 row for topbar when scrolled (scroll info shown in topbar)
        if self.scroll_state.is_scrolled() {
            reserved += 1;
        }
        // Reserve 1 row for the legend/help bar when visible
        if self.viewer_state.is_help_bar_shown() {
            reserved += 1;
        }
        rows.saturating_sub(reserved)
    }

    /// Creates a TopbarState with current environment and UI state.
    ///
    /// This combines environment state (cwd, git, exit code) with UI mode
    /// state (scroll position) into a unified state for the segment system.
    pub(super) fn topbar_state(&self, timestamp: &str) -> TopbarState {
        // Calculate scroll info if scrolled
        let scroll = if self.scroll_state.is_scrolled() {
            let offset = self.scroll_state.offset();
            let total = self.scrollback_buffer.len();
            let viewport = self.viewport_height();
            let max_offset = crate::scrollback::ScrollViewer::max_offset(total, viewport);

            // Calculate percentage (0 = at bottom, 100 = at top)
            let percentage = if max_offset == 0 {
                0
            } else {
                ((offset * 100) / max_offset).min(100) as u8
            };

            // Calculate current line - the last visible line at bottom of viewport
            // At offset=0 (bottom), current_line = total (you're at the latest content)
            // As you scroll up, current_line decreases
            // At max_offset (top, where BEGIN shows), display line 1 for better UX
            let current_line = if offset >= max_offset && total > 0 {
                1 // At the very top (BEGIN visible), show line 1
            } else {
                total.saturating_sub(offset).max(1)
            };

            Some(ScrollInfo {
                percentage,
                total_lines: total,
                current_line,
                search_active: false,
                filter_active: false,
                timestamps_on: self.viewer_state.is_timestamps_shown(),
                line_numbers_on: self.viewer_state.is_line_numbers_shown(),
            })
        } else {
            None
        };

        TopbarState {
            cwd: self.current_cwd.clone(),
            git: GitInfo {
                branch: self.git_branch.clone(),
                dirty: self.git_dirty,
            },
            exit_code: self.last_exit_code,
            last_duration: self.last_command_duration,
            timestamp: timestamp.to_string(),
            scroll,
        }
    }

    /// Processes stdin bytes for scroll key handling.
    ///
    /// Detects PgUp/PgDown/Home/End sequences and handles scrolling.
    /// Returns bytes that should be forwarded to the PTY.
    ///
    /// This function handles multiple accumulated scroll sequences (e.g., when
    /// the user presses PgUp rapidly). It processes all leading scroll keys
    /// and returns only the non-scroll remainder.
    ///
    /// # Behavior
    ///
    /// - PgUp: Scroll up one page (consumed, not forwarded)
    /// - PgDown when scrolled: Scroll down one page (consumed)
    /// - PgDown at bottom: Forward to PTY (shell might use it)
    /// - Home: Jump to top (oldest content)
    /// - End: Jump to bottom (live view)
    /// - Any other key while scrolled: Return to live view, forward key
    /// - Any other key not scrolled: Forward key as-is
    pub(super) fn process_stdin_for_scroll(&mut self, bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }

        let mut remaining = bytes;
        let mut did_scroll = false;
        let mut needs_render = false;

        // Process all leading scroll sequences
        loop {
            if remaining.is_empty() {
                break;
            }

            match self.try_scroll_action_prefix(remaining) {
                Some((action, consumed)) => {
                    let should_handle = match action {
                        ScrollAction::PageUp | ScrollAction::LineUp | ScrollAction::Home => {
                            self.is_scroll_allowed()
                        }
                        ScrollAction::PageDown | ScrollAction::LineDown | ScrollAction::End => {
                            self.scroll_state.is_scrolled()
                        }
                    };

                    // If this sequence should be forwarded, don't consume it.
                    if !should_handle {
                        break;
                    }

                    // Consume the handled sequence.
                    remaining = &remaining[consumed..];
                    did_scroll = true;

                    // Apply the scroll action
                    match action {
                        ScrollAction::PageUp => {
                            if self.is_scroll_allowed() {
                                self.scroll_up(self.viewport_height());
                                needs_render = true;
                            }
                        }
                        ScrollAction::PageDown => {
                            self.scroll_down(self.viewport_height());
                            // Always render - we stay at offset=0 instead of auto-exiting
                            needs_render = true;
                        }
                        ScrollAction::LineUp => {
                            if self.is_scroll_allowed() {
                                self.scroll_up(1);
                                needs_render = true;
                            }
                        }
                        ScrollAction::LineDown => {
                            self.scroll_down(1);
                            // Always render - we stay at offset=0 instead of auto-exiting
                            needs_render = true;
                        }
                        ScrollAction::Home => {
                            if self.is_scroll_allowed() {
                                self.scroll_to_top();
                                needs_render = true;
                            }
                        }
                        ScrollAction::End => {
                            self.scroll_to_bottom();
                            needs_render = false;
                        }
                    }
                }
                None => {
                    // Not a scroll sequence at start - stop processing
                    break;
                }
            }
        }

        // Render scrollback view if we scrolled (only render once at the end)
        if needs_render {
            if let Err(e) = self.render_scrollback_view() {
                warn!("Failed to render scrollback: {}", e);
                self.scroll_to_bottom();
            }
        } else if did_scroll && !self.scroll_state.is_scrolled() {
            // We scrolled but ended up at bottom - clear the view
            if let Err(e) = self.clear_scrollback_view() {
                warn!("Failed to clear scrollback view: {}", e);
            }
        }

        // Handle remaining bytes
        if remaining.is_empty() {
            Vec::new()
        } else {
            // Non-scroll key(s) remain
            if self.scroll_state.is_scrolled() {
                // Return to live view before forwarding
                self.scroll_to_bottom();
                if let Err(e) = self.clear_scrollback_view() {
                    warn!("Failed to clear scrollback view: {}", e);
                }
            }
            remaining.to_vec()
        }
    }

    /// Tries to parse a scroll action at the start of `bytes`.
    ///
    /// Returns the action and how many bytes were consumed.
    /// This allows processing multiple accumulated scroll sequences.
    pub(super) fn try_scroll_action_prefix(&self, bytes: &[u8]) -> Option<(ScrollAction, usize)> {
        let parsed = try_scroll_action_prefix_bytes(bytes);
        if let Some((action, _)) = parsed {
            debug!(?action, "Detected scroll action prefix");
        }
        parsed
    }

    /// Renders the scrollback view to the terminal.
    ///
    /// This replaces the terminal content with scrollback buffer content
    /// while preserving the topbar with scroll information.
    pub(super) fn render_scrollback_view(&mut self) -> std::io::Result<()> {
        use std::io::Write;

        let (cols, rows) = match TerminalGuard::get_size() {
            Ok(size) => size,
            Err(_) => return Ok(()), // Can't render without size
        };

        let offset = self.scroll_state.offset();
        let mut stdout = std::io::stdout();

        // Adjust content rows if help bar is shown
        let content_rows = if self.viewer_state.is_help_bar_shown() {
            rows.saturating_sub(1) // Reserve last row for help bar
        } else {
            rows
        };

        // Render scrollback content (starting at row 2 to preserve topbar)
        // Show boundary markers (BEGIN/END) at buffer boundaries
        let boundary_lines = if self.viewer_state.is_command_separators_shown() {
            &self.viewer_state.boundaries.command_starts
        } else {
            &[] as &[usize]
        };
        let stats = crate::scrollback::ScrollViewer::render(
            &mut stdout,
            &self.scrollback_buffer,
            offset,
            cols,
            content_rows.saturating_sub(1),
            &crate::scrollback::RenderConfig {
                start_row: 2,
                show_line_numbers: self.viewer_state.is_line_numbers_shown(),
                show_timestamps: self.viewer_state.is_timestamps_shown(),
                boundary_markers: true,
                boundary_lines,
                records: &self.viewer_state.boundaries.records,
                separator_registry: Some(&self.viewer_state.separator_registry),
                symbols: Some(self.chrome.symbols()),
                collapsed_commands: Some(&self.viewer_state.collapsed_commands),
                sticky_header: self.viewer_state.display.sticky_headers
                    && self.viewer_state.is_command_separators_shown(),
                theme: Some(self.chrome.theme()),
                ..Default::default()
            },
        )?;
        let first_visible_idx = stats.first_visible_line.saturating_sub(1);
        self.viewer_state.last_first_visible_line_idx =
            (first_visible_idx < self.scrollback_buffer.len()).then_some(first_visible_idx);

        // Render help bar if enabled
        if self.viewer_state.is_help_bar_shown() {
            crate::scrollback::features::LegendBar::render(
                &mut stdout,
                crate::scrollback::HelpContext::Normal,
                &self.viewer_state.display,
                cols,
                rows,
                self.chrome.theme(),
            )?;
        }

        // Render the topbar with scroll info
        self.render_scroll_topbar(cols)?;

        stdout.flush()?;
        Ok(())
    }

    /// Renders the topbar with scroll information.
    pub(super) fn render_scroll_topbar(&self, cols: u16) -> std::io::Result<()> {
        use std::io::Write;

        let timestamp = chrono::Local::now().format("%H:%M").to_string();
        let state = self.topbar_state(&timestamp);

        self.chrome.render_context_bar(cols, &state)?;
        std::io::stdout().flush()
    }

    /// Clears the scrollback view and restores normal terminal display.
    ///
    /// Called when returning to live view from scrolled state.
    /// Note: The topbar will be redrawn by the main edit loop before reedline
    /// takes control, so we just need to restore scroll region and clear content.
    pub(super) fn clear_scrollback_view(&mut self) -> std::io::Result<()> {
        use crossterm::cursor::MoveTo;
        use crossterm::terminal::{Clear, ClearType};
        use std::io::Write;

        let mut stdout = std::io::stdout();

        if self.chrome.is_active() {
            if let Ok((cols, rows)) = TerminalGuard::get_size() {
                // Reset scroll region first (DECSTBM resets cursor to home)
                if let Err(e) = self.chrome.setup_scroll_region(rows) {
                    warn!("Failed to restore scroll region: {}", e);
                }

                // Clear the content area (rows 2 to N), leaving topbar row alone
                // Position cursor at row 2 column 1
                for row in 2..=rows {
                    crossterm::queue!(stdout, MoveTo(0, row - 1), Clear(ClearType::CurrentLine))?;
                }
                crossterm::queue!(stdout, MoveTo(0, 1))?;

                // Draw topbar immediately so it's visible
                let timestamp = chrono::Local::now().format("%H:%M").to_string();
                let state = self.topbar_state(&timestamp);
                if let Err(e) = self.chrome.render_context_bar(cols, &state) {
                    warn!("Failed to render context bar: {}", e);
                }

                // Move cursor back to row 2 for reedline prompt
                crossterm::queue!(stdout, MoveTo(0, 1))?;
            }
        } else {
            // No chrome - just clear screen and go home
            crossterm::queue!(stdout, Clear(ClearType::All), MoveTo(0, 0))?;
        }

        stdout.flush()?;
        Ok(())
    }

    /// Runs the scroll view mode, handling scroll keys until user exits.
    ///
    /// This is called from HostCommand handlers when user presses PageUp/PageDown
    /// in Edit mode. Enters the alternate screen buffer so the main screen
    /// (prompt, previous output) is preserved and restored on exit.
    pub(super) fn run_scroll_view(&mut self) -> Result<()> {
        // Ensure raw mode is active - reedline may have toggled terminal modes
        self.terminal_guard
            .ensure_raw_mode()
            .context("Failed to ensure raw mode for scroll view")?;

        // Enter alternate screen — saves main screen atomically
        crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnterAlternateScreen
        )?;
        let mut alt_guard = super::AltScreenGuard::new();
        alt_guard.active = true;

        let result = self.run_scroll_view_inner();

        // Guard drops → LeaveAlternateScreen restores main screen
        drop(alt_guard);

        // Re-establish chrome after main screen restore
        if self.chrome.is_active() {
            if let Ok((cols, rows)) = TerminalGuard::get_size() {
                let _ = self.chrome.setup_scroll_region_preserve_cursor(rows);
                let ts = chrono::Local::now().format("%H:%M").to_string();
                let state = self.topbar_state(&ts);
                let _ = self.chrome.render_context_bar(cols, &state);
            }
        }

        result
    }

    /// Applies a single captured line to the scrollback buffer.
    ///
    /// Handles all three variants: Append, Overwrite, and EraseBelow.
    /// Adjusts command boundaries when lines are dropped from the ring buffer.
    fn apply_captured_line(&mut self, captured: crate::scrollback::CapturedLine) {
        match captured {
            crate::scrollback::CapturedLine::Append(content) => {
                let dropped = self.scrollback_buffer.push_line(content);
                if dropped > 0 {
                    self.viewer_state
                        .boundaries
                        .adjust_for_dropped_lines(dropped);
                }
            }
            crate::scrollback::CapturedLine::Overwrite {
                lines_back,
                content,
            } => {
                let len = self.scrollback_buffer.len();
                if lines_back > 0 && lines_back <= len {
                    self.scrollback_buffer
                        .replace_line(len - lines_back, content);
                }
            }
            crate::scrollback::CapturedLine::EraseBelow { lines_back } => {
                let len = self.scrollback_buffer.len();
                if lines_back > 0 && lines_back <= len {
                    self.scrollback_buffer.erase_from(len - lines_back);
                }
            }
        }
    }

    /// Renders the bottom legend for a modal scrollback context when enabled.
    fn render_legend_for_context(&self, ctx: crate::scrollback::HelpContext) -> Result<()> {
        use std::io::Write;

        if !self.viewer_state.is_help_bar_shown() {
            return Ok(());
        }

        let (cols, rows) = TerminalGuard::get_size()?;
        let mut out = std::io::stdout();
        crate::scrollback::features::LegendBar::render(
            &mut out,
            ctx,
            &self.viewer_state.display,
            cols,
            rows,
            self.chrome.theme(),
        )?;
        out.flush()?;
        Ok(())
    }

    /// Renders scrollback view, returning true if a render error occurred
    /// and the loop should exit.
    fn try_render(&mut self) -> bool {
        if let Err(e) = self.render_scrollback_view() {
            warn!("Failed to render scrollback: {}", e);
            self.scroll_to_bottom();
            true
        } else {
            false
        }
    }

    /// Inner scroll view loop (separated for RAII alt-screen cleanup).
    pub(super) fn run_scroll_view_inner(&mut self) -> Result<()> {
        use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

        // Flush any partial line from capture so it appears in scrollback
        if let Some(captured) = self.capture_state.flush() {
            self.apply_captured_line(captured);
        }

        // Initial render
        if self.try_render() {
            return Ok(());
        }

        loop {
            self.handle_signals()?;
            if self.should_shutdown() {
                self.scroll_to_bottom();
                break;
            }

            let has_event = event::poll(std::time::Duration::from_millis(100))
                .context("Failed to poll for events")?;

            if !has_event {
                if self.should_shutdown() {
                    self.scroll_to_bottom();
                    break;
                }
                continue;
            }

            let evt = event::read().context("Failed to read event")?;

            match evt {
                Event::Key(KeyEvent {
                    code: KeyCode::PageUp,
                    modifiers,
                    ..
                }) => {
                    if self.is_scroll_allowed() {
                        let lines = if modifiers.contains(KeyModifiers::SHIFT) {
                            1
                        } else {
                            self.viewport_height()
                        };
                        self.scroll_up(lines);
                        if self.try_render() {
                            break;
                        }
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::PageDown,
                    modifiers,
                    ..
                }) => {
                    let lines = if modifiers.contains(KeyModifiers::SHIFT) {
                        1
                    } else {
                        self.viewport_height()
                    };
                    self.scroll_down(lines);
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Up, ..
                }) => {
                    if self.is_scroll_allowed() {
                        self.scroll_up(1);
                        if self.try_render() {
                            break;
                        }
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Down,
                    ..
                }) => {
                    self.scroll_down(1);
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Home,
                    ..
                }) => {
                    self.scroll_to_top();
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::End, ..
                }) => {
                    self.scroll_to_bottom();
                    break;
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('l'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.viewer_state.toggle_line_numbers();
                    debug!(
                        show_line_numbers = self.viewer_state.is_line_numbers_shown(),
                        "Toggled line numbers"
                    );
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('u'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if self.is_scroll_allowed() {
                        let half_page = self.viewport_height() / 2;
                        self.scroll_up(half_page.max(1));
                        if self.try_render() {
                            break;
                        }
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('d'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    let half_page = self.viewport_height() / 2;
                    self.scroll_down(half_page.max(1));
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('t'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.viewer_state.toggle_timestamps();
                    debug!(
                        show_timestamps = self.viewer_state.is_timestamps_shown(),
                        "Toggled timestamps"
                    );
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('b'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.viewer_state.toggle_command_separators();
                    debug!(
                        show_separators = self.viewer_state.is_command_separators_shown(),
                        "Toggled command separators"
                    );
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('p'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.jump_to_prev_command();
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('n'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.jump_to_next_command();
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('z'),
                    ..
                }) => {
                    let total = self.scrollback_buffer.len();
                    let offset = self.scroll_state.offset();
                    let viewport = self.viewport_height().max(1);
                    if let Some(first_visible) = resolve_fold_target_line(
                        self.viewer_state.last_first_visible_line_idx,
                        total,
                        offset,
                        viewport,
                    ) {
                        if let Some(record_idx) = resolve_fold_target_record_index(
                            &self.viewer_state.boundaries,
                            first_visible,
                            total,
                        ) {
                            let _ = self.viewer_state.boundaries.toggle_fold(record_idx);

                            let folded = self
                                .viewer_state
                                .boundaries
                                .folded_line_count_in_range(0, total);
                            let visible_total = total.saturating_sub(folded);
                            let max_offset = crate::scrollback::ScrollViewer::max_offset(
                                visible_total,
                                viewport,
                            );
                            let clamped_offset = offset.min(max_offset);
                            self.scroll_state =
                                crate::types::ScrollState::scrolled_at(clamped_offset);
                        }
                    }
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('x'),
                    ..
                }) => {
                    let total = self.scrollback_buffer.len();
                    let offset = self.scroll_state.offset();
                    let viewport = self.viewport_height().max(1);
                    let mut changed = false;
                    if let Some(first_visible) = resolve_fold_target_line(
                        self.viewer_state.last_first_visible_line_idx,
                        total,
                        offset,
                        viewport,
                    ) {
                        if let Some(record_idx) = resolve_fold_target_record_index(
                            &self.viewer_state.boundaries,
                            first_visible,
                            total,
                        ) {
                            if let Some(record) =
                                self.viewer_state.boundaries.records.get_mut(record_idx)
                            {
                                if record.folded {
                                    record.folded = false;
                                    changed = true;
                                }
                            }
                        }
                    }
                    if changed {
                        let folded = self
                            .viewer_state
                            .boundaries
                            .folded_line_count_in_range(0, total);
                        let visible_total = total.saturating_sub(folded);
                        let max_offset =
                            crate::scrollback::ScrollViewer::max_offset(visible_total, viewport);
                        self.scroll_state =
                            crate::types::ScrollState::scrolled_at(offset.min(max_offset));
                    }
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('X'),
                    ..
                }) => {
                    let total = self.scrollback_buffer.len();
                    let offset = self.scroll_state.offset();
                    let viewport = self.viewport_height().max(1);
                    let mut changed = false;
                    for record in &mut self.viewer_state.boundaries.records {
                        if record.folded {
                            record.folded = false;
                            changed = true;
                        }
                    }
                    if changed {
                        let max_offset =
                            crate::scrollback::ScrollViewer::max_offset(total, viewport);
                        self.scroll_state =
                            crate::types::ScrollState::scrolled_at(offset.min(max_offset));
                    }
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('s'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Err(e) = self.run_search_mode() {
                        warn!("Search mode error: {}", e);
                    }
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('f'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Err(e) = self.run_filter_mode() {
                        warn!("Filter mode error: {}", e);
                    }
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('g'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Err(e) = self.run_goto_line_mode() {
                        warn!("Go-to-line mode error: {}", e);
                    }
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('?'),
                    ..
                })
                | Event::Key(KeyEvent {
                    code: KeyCode::F(1),
                    ..
                }) => {
                    self.viewer_state.toggle_help_bar();
                    debug!(
                        show_help = self.viewer_state.is_help_bar_shown(),
                        "Toggled help bar"
                    );
                    if self.try_render() {
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) => {
                    self.scroll_to_bottom();
                    break;
                }
                Event::Key(_) => {
                    self.scroll_to_bottom();
                    break;
                }
                Event::Resize(cols, _rows) => {
                    if self.should_shutdown() {
                        self.scroll_to_bottom();
                        break;
                    }
                    self.capture_state.set_terminal_width(cols);
                    if self.try_render() {
                        break;
                    }
                }
                _ => {}
            }
        }

        // Alt screen exit in run_scroll_view() handles restoration.
        Ok(())
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::{
        ScrollAction, filter_offset_for_line_with_viewport, resolve_fold_target_line,
        resolve_fold_target_record_index, try_scroll_action_prefix_bytes,
    };
    use crate::scrollback::features::FilterState;
    use crate::scrollback::{CommandBoundaries, CommandRecord};

    #[test]
    fn test_filter_offset_for_line_with_viewport_centered_returns_5() {
        let filter = FilterState {
            matching_lines: (0..20).collect(),
            ..FilterState::default()
        };

        assert_eq!(filter_offset_for_line_with_viewport(&filter, 9, 10), 5);
    }

    #[test]
    fn test_filter_offset_for_line_with_viewport_bottom_returns_0() {
        let filter = FilterState {
            matching_lines: (0..20).collect(),
            ..FilterState::default()
        };

        assert_eq!(filter_offset_for_line_with_viewport(&filter, 19, 10), 0);
    }

    #[test]
    fn test_filter_offset_for_line_with_viewport_out_of_range_returns_0() {
        let filter = FilterState {
            matching_lines: vec![2, 4, 6, 8],
            ..FilterState::default()
        };

        assert_eq!(filter_offset_for_line_with_viewport(&filter, 99, 10), 0);
    }

    #[test]
    fn test_try_scroll_action_prefix_bytes_page_keys_returns_pageup_and_len4() {
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[5~rest"),
            Some((ScrollAction::PageUp, 4))
        );
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[6~"),
            Some((ScrollAction::PageDown, 4))
        );
    }

    #[test]
    fn test_try_scroll_action_prefix_bytes_line_keys_returns_lineup_and_len6() {
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[5;2~"),
            Some((ScrollAction::LineUp, 6))
        );
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[6;2~"),
            Some((ScrollAction::LineDown, 6))
        );
    }

    #[test]
    fn test_try_scroll_action_prefix_bytes_home_end_keys_returns_home_end_and_lens() {
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[1~"),
            Some((ScrollAction::Home, 4))
        );
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[H"),
            Some((ScrollAction::Home, 3))
        );
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[4~"),
            Some((ScrollAction::End, 4))
        );
        assert_eq!(
            try_scroll_action_prefix_bytes(b"\x1b[F"),
            Some((ScrollAction::End, 3))
        );
    }

    #[test]
    fn test_try_scroll_action_prefix_bytes_non_scroll_input_returns_none() {
        assert_eq!(try_scroll_action_prefix_bytes(b"abc"), None);
        assert_eq!(try_scroll_action_prefix_bytes(b"\x1b[9~"), None);
        assert_eq!(try_scroll_action_prefix_bytes(b"\x1b["), None);
    }

    #[test]
    fn test_resolve_fold_target_line_with_valid_cached_line_returns_cached_line() {
        let line = resolve_fold_target_line(Some(12), 40, 3, 20);
        assert_eq!(line, Some(12));
    }

    #[test]
    fn test_resolve_fold_target_line_with_invalid_cached_line_returns_fallback_line() {
        let line = resolve_fold_target_line(Some(99), 10, 0, 5);
        assert_eq!(line, Some(5));
    }

    #[test]
    fn test_resolve_fold_target_line_with_empty_buffer_returns_none() {
        let line = resolve_fold_target_line(None, 0, 0, 10);
        assert_eq!(line, None);
    }

    #[test]
    fn test_resolve_fold_target_record_index_with_non_command_top_chooses_next_foldable() {
        let mut boundaries = CommandBoundaries::new();
        boundaries.records = vec![
            CommandRecord {
                output_start: 10,
                prompt_line: Some(15),
                folded: true,
                ..Default::default()
            },
            CommandRecord {
                output_start: 20,
                prompt_line: Some(30),
                folded: false,
                ..Default::default()
            },
        ];

        let target = resolve_fold_target_record_index(&boundaries, 2, 40);
        assert_eq!(target, Some(0));
    }

    #[test]
    fn test_resolve_fold_target_record_index_with_no_next_foldable_chooses_previous_foldable() {
        let mut boundaries = CommandBoundaries::new();
        boundaries.records = vec![
            CommandRecord {
                output_start: 10,
                prompt_line: Some(15),
                folded: true,
                ..Default::default()
            },
            CommandRecord {
                output_start: 20,
                prompt_line: Some(21),
                folded: false,
                ..Default::default()
            },
        ];

        let target = resolve_fold_target_record_index(&boundaries, 39, 40);
        assert_eq!(target, Some(0));
    }
}
