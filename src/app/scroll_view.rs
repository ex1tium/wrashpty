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
            for captured in self.capture_state.feed(bytes) {
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

        let max_offset = crate::scrollback::ScrollViewer::max_offset(
            self.scrollback_buffer.len(),
            self.viewport_height(),
        );

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

        let max_offset = crate::scrollback::ScrollViewer::max_offset(
            self.scrollback_buffer.len(),
            self.viewport_height(),
        );
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
        self.viewer_state
            .boundaries
            .record_marker(event, line_index);
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
        &self,
        search: &crate::scrollback::features::SearchState,
    ) -> Result<()> {
        use std::io::Write;

        let (cols, rows) = TerminalGuard::get_size()?;
        let offset = self.scroll_state.offset();
        let mut out = std::io::stdout();

        // Render using the search-aware viewer
        let boundary_lines = if self.viewer_state.is_command_separators_shown() {
            &self.viewer_state.boundaries.prompt_lines
        } else {
            &[] as &[usize]
        };
        crate::scrollback::ScrollViewer::render_with_search(
            &mut out,
            &self.scrollback_buffer,
            offset,
            cols,
            rows,
            self.viewer_state.is_line_numbers_shown(),
            self.viewer_state.is_timestamps_shown(),
            true, // show boundary markers
            search,
            self.chrome.theme(),
            boundary_lines,
        )?;

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
        &self,
        filter: &crate::scrollback::features::FilterState,
        filter_offset: usize,
    ) -> Result<()> {
        use std::io::Write;

        let (cols, rows) = TerminalGuard::get_size()?;
        let mut out = std::io::stdout();

        // Render using the filter-aware viewer
        crate::scrollback::ScrollViewer::render_with_filter(
            &mut out,
            &self.scrollback_buffer,
            filter,
            filter_offset,
            cols,
            rows,
            self.viewer_state.is_line_numbers_shown(),
            self.viewer_state.is_timestamps_shown(),
        )?;

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
        &self,
        filter: &crate::scrollback::features::FilterState,
        filter_offset: usize,
        search: &crate::scrollback::features::SearchState,
    ) -> Result<()> {
        use std::io::Write;

        let (cols, rows) = TerminalGuard::get_size()?;
        let mut out = std::io::stdout();

        // Render using the combined filter+search viewer
        crate::scrollback::ScrollViewer::render_with_filter_and_search(
            &mut out,
            &self.scrollback_buffer,
            filter,
            filter_offset,
            cols,
            rows,
            self.viewer_state.is_line_numbers_shown(),
            self.viewer_state.is_timestamps_shown(),
            search,
            self.chrome.theme(),
        )?;

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
    pub(super) fn viewport_height(&self) -> usize {
        match TerminalGuard::get_size() {
            Ok((_, rows)) => {
                // Reserve 1 row for topbar when scrolled (scroll info shown in topbar)
                if self.scroll_state.is_scrolled() {
                    (rows as usize).saturating_sub(1)
                } else {
                    rows as usize
                }
            }
            Err(_) => 24, // Fallback
        }
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
                search_active: matches!(
                    self.viewer_state.mode,
                    crate::scrollback::ScrollViewMode::Search(_)
                ),
                filter_active: matches!(
                    self.viewer_state.mode,
                    crate::scrollback::ScrollViewMode::Filter(_)
                ),
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
            &self.viewer_state.boundaries.prompt_lines
        } else {
            &[] as &[usize]
        };
        crate::scrollback::ScrollViewer::render_with_chrome(
            &mut stdout,
            &self.scrollback_buffer,
            offset,
            cols,
            content_rows,
            self.viewer_state.is_line_numbers_shown(),
            self.viewer_state.is_timestamps_shown(),
            true, // show_boundary_markers
            Some(self.chrome.theme()),
            boundary_lines,
        )?;

        // Render help bar if enabled
        if self.viewer_state.is_help_bar_shown() {
            crate::scrollback::features::HelpBar::render(
                &mut stdout,
                &self.viewer_state.mode,
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
                    write!(stdout, "\x1b[{};1H\x1b[2K", row)?;
                }
                write!(stdout, "\x1b[2;1H")?;

                // Draw topbar immediately so it's visible
                let timestamp = chrono::Local::now().format("%H:%M").to_string();
                let state = self.topbar_state(&timestamp);
                if let Err(e) = self.chrome.render_context_bar(cols, &state) {
                    warn!("Failed to render context bar: {}", e);
                }

                // Move cursor back to row 2 for reedline prompt
                write!(stdout, "\x1b[2;1H")?;
            }
        } else {
            // No chrome - just clear screen and go home
            write!(stdout, "\x1b[2J\x1b[H")?;
        }

        stdout.flush()?;
        Ok(())
    }

    /// Runs the scroll view mode, handling scroll keys until user exits.
    ///
    /// This is called from HostCommand handlers when user presses PageUp/PageDown
    /// in Edit mode. Renders scrollback content and handles scroll navigation
    /// until user presses Esc or any non-scroll key.
    pub(super) fn run_scroll_view(&mut self) -> Result<()> {
        use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

        // RAII guard to ensure raw mode is disabled even on panic
        struct RawModeGuard;
        impl Drop for RawModeGuard {
            fn drop(&mut self) {
                let _ = disable_raw_mode();
            }
        }

        // Enable raw mode for crossterm event capture
        // Reedline may have disabled raw mode before returning via ExecuteHostCommand
        if let Err(e) = enable_raw_mode() {
            warn!("Failed to enable raw mode for scroll view: {}", e);
            return Ok(());
        }

        // Guard ensures disable_raw_mode is called even if run_scroll_view_inner panics
        let _guard = RawModeGuard;

        self.run_scroll_view_inner()
    }

    /// Inner scroll view loop (separated for RAII cleanup).
    pub(super) fn run_scroll_view_inner(&mut self) -> Result<()> {
        use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

        // Flush any partial line from capture so it appears in scrollback
        if let Some(captured) = self.capture_state.flush() {
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

        // Initial render
        if let Err(e) = self.render_scrollback_view() {
            warn!("Failed to render scrollback: {}", e);
            self.scroll_to_bottom();
            return Ok(());
        }

        loop {
            self.handle_signals()?;
            if self.should_shutdown() {
                self.scroll_to_bottom();
                break;
            }

            // Wait for input (with periodic checks for signals)
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
                            1 // Shift+PgUp: one line
                        } else {
                            self.viewport_height() // PgUp: one page
                        };
                        self.scroll_up(lines);
                        if let Err(e) = self.render_scrollback_view() {
                            warn!("Failed to render scrollback: {}", e);
                            self.scroll_to_bottom();
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
                        1 // Shift+PgDown: one line
                    } else {
                        self.viewport_height() // PgDown: one page
                    };
                    self.scroll_down(lines);
                    // Always render - we stay at offset=0 instead of auto-exiting
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Up, ..
                }) => {
                    // Up arrow: scroll up one line
                    if self.is_scroll_allowed() {
                        self.scroll_up(1);
                        if let Err(e) = self.render_scrollback_view() {
                            warn!("Failed to render scrollback: {}", e);
                            self.scroll_to_bottom();
                            break;
                        }
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Down,
                    ..
                }) => {
                    // Down arrow: scroll down one line (stays at offset=0 at bottom)
                    self.scroll_down(1);
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Home,
                    ..
                }) => {
                    // Home: jump to top (oldest content)
                    self.scroll_to_top();
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::End, ..
                }) => {
                    // End: jump to bottom (live view)
                    self.scroll_to_bottom();
                    break;
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('l'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+L: toggle line numbers
                    self.viewer_state.toggle_line_numbers();
                    debug!(
                        show_line_numbers = self.viewer_state.is_line_numbers_shown(),
                        "Toggled line numbers"
                    );
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('u'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+U: half-page up
                    if self.is_scroll_allowed() {
                        let half_page = self.viewport_height() / 2;
                        self.scroll_up(half_page.max(1));
                        if let Err(e) = self.render_scrollback_view() {
                            warn!("Failed to render scrollback: {}", e);
                            self.scroll_to_bottom();
                            break;
                        }
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('d'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+D: half-page down
                    let half_page = self.viewport_height() / 2;
                    self.scroll_down(half_page.max(1));
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('t'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+T: toggle timestamp gutter
                    self.viewer_state.toggle_timestamps();
                    debug!(
                        show_timestamps = self.viewer_state.is_timestamps_shown(),
                        "Toggled timestamps"
                    );
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('b'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+B: toggle command boundary separators
                    self.viewer_state.toggle_command_separators();
                    debug!(
                        show_separators = self.viewer_state.is_command_separators_shown(),
                        "Toggled command separators"
                    );
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('p'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+P: jump to previous command boundary
                    self.jump_to_prev_command();
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('n'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+N: jump to next command boundary
                    self.jump_to_next_command();
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('s'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+S: incremental search
                    match self.run_search_mode() {
                        Ok(_) => {
                            // Re-render without search highlights
                            if let Err(e) = self.render_scrollback_view() {
                                warn!("Failed to render scrollback: {}", e);
                                self.scroll_to_bottom();
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Search mode error: {}", e);
                            if let Err(e) = self.render_scrollback_view() {
                                warn!("Failed to render scrollback: {}", e);
                                self.scroll_to_bottom();
                                break;
                            }
                        }
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('f'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+F: filter mode
                    match self.run_filter_mode() {
                        Ok(_) => {
                            // Re-render full view
                            if let Err(e) = self.render_scrollback_view() {
                                warn!("Failed to render scrollback: {}", e);
                                self.scroll_to_bottom();
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Filter mode error: {}", e);
                            if let Err(e) = self.render_scrollback_view() {
                                warn!("Failed to render scrollback: {}", e);
                                self.scroll_to_bottom();
                                break;
                            }
                        }
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('g'),
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+G: go to line
                    match self.run_goto_line_mode() {
                        Ok(true) => {
                            // User submitted a line number, re-render
                            if let Err(e) = self.render_scrollback_view() {
                                warn!("Failed to render scrollback: {}", e);
                                self.scroll_to_bottom();
                                break;
                            }
                        }
                        Ok(false) => {
                            // User cancelled, just re-render
                            if let Err(e) = self.render_scrollback_view() {
                                warn!("Failed to render scrollback: {}", e);
                                self.scroll_to_bottom();
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Go-to-line mode error: {}", e);
                            if let Err(e) = self.render_scrollback_view() {
                                warn!("Failed to render scrollback: {}", e);
                                self.scroll_to_bottom();
                                break;
                            }
                        }
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
                    // Toggle help bar
                    self.viewer_state.toggle_help_bar();
                    debug!(
                        show_help = self.viewer_state.is_help_bar_shown(),
                        "Toggled help bar"
                    );
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to render scrollback: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Esc, ..
                }) => {
                    // Exit scroll view
                    self.scroll_to_bottom();
                    break;
                }
                Event::Key(_) => {
                    // Any other key exits scroll view
                    self.scroll_to_bottom();
                    break;
                }
                Event::Resize(cols, _rows) => {
                    if self.should_shutdown() {
                        self.scroll_to_bottom();
                        break;
                    }
                    // Handle resize while in scroll view
                    self.capture_state.set_terminal_width(cols);
                    if let Err(e) = self.render_scrollback_view() {
                        warn!("Failed to re-render after resize: {}", e);
                        self.scroll_to_bottom();
                        break;
                    }
                }
                _ => {
                    // Ignore other events (mouse, focus, etc.)
                }
            }
        }

        // Clear scroll view and restore normal terminal
        if let Err(e) = self.clear_scrollback_view() {
            warn!("Failed to clear scrollback view: {}", e);
        }

        Ok(())
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::{
        ScrollAction, filter_offset_for_line_with_viewport, try_scroll_action_prefix_bytes,
    };
    use crate::scrollback::features::FilterState;

    #[test]
    fn test_filter_offset_for_line_centered_returns_expected_offset() {
        let filter = FilterState {
            matching_lines: (0..20).collect(),
            ..FilterState::default()
        };

        assert_eq!(filter_offset_for_line_with_viewport(&filter, 9, 10), 5);
    }

    #[test]
    fn test_filter_offset_for_line_bottom_returns_zero_offset() {
        let filter = FilterState {
            matching_lines: (0..20).collect(),
            ..FilterState::default()
        };

        assert_eq!(filter_offset_for_line_with_viewport(&filter, 19, 10), 0);
    }

    #[test]
    fn test_filter_offset_for_line_out_of_range_returns_zero_offset() {
        let filter = FilterState {
            matching_lines: vec![2, 4, 6, 8],
            ..FilterState::default()
        };

        assert_eq!(filter_offset_for_line_with_viewport(&filter, 99, 10), 0);
    }

    #[test]
    fn test_try_scroll_action_prefix_page_keys_return_expected_action_and_len() {
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
    fn test_try_scroll_action_prefix_line_keys_return_expected_action_and_len() {
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
    fn test_try_scroll_action_prefix_home_end_keys_return_expected_action_and_len() {
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
    fn test_try_scroll_action_prefix_non_scroll_input_returns_none() {
        assert_eq!(try_scroll_action_prefix_bytes(b"abc"), None);
        assert_eq!(try_scroll_action_prefix_bytes(b"\x1b[9~"), None);
        assert_eq!(try_scroll_action_prefix_bytes(b"\x1b["), None);
    }
}
