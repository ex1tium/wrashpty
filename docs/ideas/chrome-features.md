# Chrome Feature Set Design Document

> **Status**: Ready for implementation
> **Priority**: Core feature development
> **Dependencies**: ratatui crate addition

## Executive Summary

This document defines the Chrome feature set for wrashpty - a system of expandable panels and context displays that operate in reserved terminal rows outside reedline's scroll region. The design leverages ratatui widgets rendered directly to terminal output without using ratatui's Terminal abstraction.

---

## Architecture Principles

### Separation of Concerns

```
┌────────────────────────────────────────────────────────────┐
│ CHROME LAYER (rows 1 to N when expanded)                   │
│ - Context bar (always row 1)                               │
│ - Expandable panels (rows 2-N, optional)                   │
│ - Owns input ONLY when panel is open                       │
│ - Renders ratatui widgets to Buffer → ANSI                 │
├────────────────────────────────────────────────────────────┤
│ REEDLINE LAYER (scroll region)                             │
│ - Line editing, cursor movement                            │
│ - Tab completion menu (inside scroll region)               │
│ - Hints, syntax highlighting                               │
│ - History search (Ctrl+R)                                  │
│ - Owns input during read_line()                            │
└────────────────────────────────────────────────────────────┘
```

### Key Constraints

1. **No overlap**: Chrome never renders inside scroll region; reedline never renders outside it
2. **Strategic rendering**: Render once at state transitions, not continuously
3. **Clean handoff**: Panel mode ends completely before reedline starts
4. **Graceful degradation**: All features work without panels; panels are enhancement

---

## Implementation Phases

### Phase A: Panel Infrastructure (Foundation)

#### A.1 Chrome State Extensions

```rust
pub struct Chrome {
    mode: ChromeMode,
    suspended: bool,

    // NEW: Panel state
    panel_state: PanelState,
    panel_height: u16,  // Current expanded height (1 = context bar only)
}

pub enum PanelState {
    Collapsed,                    // Normal: 1-row context bar
    Expanded { height: u16 },     // Panel visible, N rows reserved
}
```

#### A.2 Panel Lifecycle Methods

```rust
impl Chrome {
    /// Expands the chrome panel to the specified height.
    /// Adjusts scroll region from [2..rows] to [height+1..rows].
    pub fn expand_panel(&mut self, height: u16, total_rows: u16) -> io::Result<()>;

    /// Collapses the panel back to single context bar.
    /// Restores scroll region to [2..rows].
    pub fn collapse_panel(&mut self, total_rows: u16) -> io::Result<()>;

    /// Returns the current panel height (1 when collapsed).
    pub fn panel_height(&self) -> u16;

    /// Renders a ratatui Buffer to the panel area.
    pub fn render_panel_buffer(&self, buffer: &Buffer) -> io::Result<()>;
}
```

#### A.3 Buffer to ANSI Conversion

```rust
/// Converts a ratatui Buffer to ANSI escape sequences for direct stdout output.
/// This bypasses ratatui's Terminal abstraction entirely.
pub fn buffer_to_ansi(buffer: &Buffer, area: Rect) -> String {
    let mut output = String::new();
    let mut current_style = Style::default();

    for y in area.y..area.y + area.height {
        // Position cursor at start of row
        output.push_str(&format!("\x1b[{};1H", y + 1));

        for x in area.x..area.x + area.width {
            let cell = buffer.cell((x, y)).unwrap();

            // Apply style changes (optimize: only emit when style changes)
            if cell.style() != current_style {
                output.push_str(&style_to_ansi(cell.style()));
                current_style = cell.style();
            }

            output.push_str(cell.symbol());
        }
    }

    // Reset style at end
    output.push_str("\x1b[0m");
    output
}

fn style_to_ansi(style: Style) -> String {
    // Convert ratatui Style to ANSI escape codes
    // Handle: fg, bg, bold, italic, underline, etc.
}
```

#### A.4 Panel Input Loop

```rust
pub enum PanelResult {
    Continue,                     // Keep panel open, re-render
    Dismiss,                      // Close panel, return to reedline
    Execute(String),              // Close panel, execute command
    InsertText(String),           // Close panel, insert into reedline buffer
}

impl App {
    /// Runs the panel input loop until dismissed.
    /// Returns control to normal edit flow after panel closes.
    fn run_panel_mode<P: Panel>(&mut self, panel: &mut P) -> Result<PanelResult> {
        let (cols, rows) = TerminalGuard::get_size()?;
        let panel_height = panel.preferred_height().min(rows / 2);

        self.chrome.expand_panel(panel_height, rows)?;

        loop {
            // Render panel
            let area = Rect::new(0, 0, cols, panel_height);
            let mut buffer = Buffer::empty(area);
            panel.render(&mut buffer, area);
            self.chrome.render_panel_buffer(&buffer)?;

            // Wait for input
            if crossterm::event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = crossterm::event::read()? {
                    match panel.handle_input(key)? {
                        PanelResult::Continue => continue,
                        result => {
                            self.chrome.collapse_panel(rows)?;
                            return Ok(result);
                        }
                    }
                }
            }
        }
    }
}
```

#### A.5 Panel Trait

```rust
/// Trait for implementing chrome panels.
pub trait Panel {
    /// Returns the preferred height for this panel.
    fn preferred_height(&self) -> u16;

    /// Renders the panel content to the buffer.
    fn render(&self, buffer: &mut Buffer, area: Rect);

    /// Handles a key event, returning the result.
    fn handle_input(&mut self, key: KeyEvent) -> Result<PanelResult>;

    /// Returns the panel title for the tab bar.
    fn title(&self) -> &str;
}
```

---

### Phase B: Context Bar Polish

#### B.1 ANSI Color Support

Replace reverse video with proper colors:

```rust
fn format_context_bar_styled(&self, max_width: usize, ctx: &ChromeContext) -> String {
    let mut output = String::new();

    // Status icon with color
    let (icon, color) = if ctx.last_exit_code == 0 {
        ("✓", "\x1b[32m")  // Green
    } else {
        ("✗", "\x1b[31m")  // Red
    };
    output.push_str(&format!(" {}{}\x1b[0m ", color, icon));

    // Duration (dim if short)
    if let Some(dur) = ctx.last_duration {
        let secs = dur.as_secs_f64();
        if secs >= 0.5 {  // Only show if >= 500ms
            output.push_str(&format!("\x1b[33m{:.1}s\x1b[0m ", secs));
        }
    }

    // CWD (cyan)
    output.push_str(&format!("\x1b[36m{}\x1b[0m ", ctx.cwd_display()));

    // Git (magenta, bold if dirty)
    if let Some(branch) = ctx.git_branch {
        let dirty_marker = if ctx.git_dirty { "●" } else { "" };
        let style = if ctx.git_dirty { "\x1b[1;35m" } else { "\x1b[35m" };
        output.push_str(&format!("{}git:{}{}\x1b[0m ", style, branch, dirty_marker));
    }

    // Timestamp (dim)
    output.push_str(&format!("\x1b[2m{}\x1b[0m", ctx.timestamp));

    // Pad and return
    self.pad_to_width(output, max_width)
}
```

#### B.2 Priority-Based Truncation

When space is limited, truncate in priority order:

```rust
struct ContextSegment {
    content: String,
    priority: u8,      // Lower = more important, keep longer
    min_width: usize,  // Minimum displayable width
}

fn format_with_priority_truncation(&self, max_width: usize, ctx: &ChromeContext) -> String {
    let segments = vec![
        ContextSegment { content: status_segment(ctx), priority: 0, min_width: 3 },
        ContextSegment { content: timestamp_segment(ctx), priority: 1, min_width: 5 },
        ContextSegment { content: cwd_segment(ctx), priority: 2, min_width: 8 },
        ContextSegment { content: duration_segment(ctx), priority: 3, min_width: 4 },
        ContextSegment { content: git_segment(ctx), priority: 4, min_width: 6 },
    ];

    // Start with all segments, remove lowest priority until fits
    let mut active_segments = segments.clone();
    while total_width(&active_segments) > max_width && !active_segments.is_empty() {
        // Remove highest priority number (least important)
        let max_priority = active_segments.iter().map(|s| s.priority).max().unwrap();
        active_segments.retain(|s| s.priority != max_priority);
    }

    // If still too wide, truncate lowest priority remaining segment
    // ...
}
```

#### B.3 Notification Overlay

```rust
pub struct Notification {
    message: String,
    style: NotificationStyle,
    expires_at: Instant,
}

pub enum NotificationStyle {
    Info,     // Blue background
    Success,  // Green background
    Warning,  // Yellow background
    Error,    // Red background
}

impl Chrome {
    /// Queues a notification to display in the context bar area.
    pub fn notify(&mut self, message: impl Into<String>, style: NotificationStyle, duration: Duration) {
        self.notifications.push_back(Notification {
            message: message.into(),
            style,
            expires_at: Instant::now() + duration,
        });
    }

    /// Renders context bar with notification overlay if any active.
    fn render_with_notifications(&self, cols: u16, ctx: &ChromeContext) -> io::Result<()> {
        // Expire old notifications
        self.notifications.retain(|n| n.expires_at > Instant::now());

        if let Some(notif) = self.notifications.front() {
            // Render notification instead of normal bar
            self.render_notification(cols, notif)
        } else {
            // Normal context bar
            self.render_context_bar(cols, ctx)
        }
    }
}
```

---

### Phase C: Panel Implementations

#### C.1 Tab System (Domain Grouping)

All panels share a tabbed interface for domain grouping:

```rust
pub struct TabbedPanel {
    tabs: Vec<Box<dyn Panel>>,
    active_tab: usize,
}

impl TabbedPanel {
    pub fn new() -> Self {
        Self {
            tabs: vec![
                Box::new(CommandPalettePanel::new()),
                Box::new(FileBrowserPanel::new()),
                Box::new(HistoryBrowserPanel::new()),
                Box::new(HelpPanel::new()),
                // Future: GitPanel, BookmarksPanel, etc.
            ],
            active_tab: 0,
        }
    }
}

impl Panel for TabbedPanel {
    fn render(&self, buffer: &mut Buffer, area: Rect) {
        // Tab bar at top
        let tab_area = Rect::new(area.x, area.y, area.width, 1);
        let content_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);

        // Render tab bar using ratatui Tabs widget
        let titles: Vec<&str> = self.tabs.iter().map(|t| t.title()).collect();
        let tabs = Tabs::new(titles)
            .select(self.active_tab)
            .style(Style::default().fg(Color::White))
            .highlight_style(Style::default().fg(Color::Yellow).bold());
        tabs.render(tab_area, buffer);

        // Render active panel content
        self.tabs[self.active_tab].render(buffer, content_area);
    }

    fn handle_input(&mut self, key: KeyEvent) -> Result<PanelResult> {
        match key.code {
            // Tab switching
            KeyCode::Tab => {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
                Ok(PanelResult::Continue)
            }
            KeyCode::BackTab => {
                self.active_tab = self.active_tab.checked_sub(1)
                    .unwrap_or(self.tabs.len() - 1);
                Ok(PanelResult::Continue)
            }
            // Delegate to active panel
            _ => self.tabs[self.active_tab].handle_input(key),
        }
    }
}
```

#### C.2 Command Palette Panel

```rust
pub struct CommandPalettePanel {
    items: Vec<CommandItem>,
    filtered: Vec<usize>,  // Indices into items
    selection: usize,
    filter: String,
    scroll_offset: usize,
}

pub struct CommandItem {
    name: String,
    description: String,
    command: String,
    source: CommandSource,
    frecency_score: f64,
}

pub enum CommandSource {
    History,           // From shell history
    Makefile,          // Parsed from Makefile
    PackageJson,       // npm scripts
    CargoToml,         // cargo commands
    JustFile,          // just recipes
    Script,            // Detected executable scripts
    UserDefined,       // User-configured commands
}

impl CommandPalettePanel {
    /// Loads commands from all available sources.
    pub fn load_commands(&mut self, cwd: &Path) {
        self.items.clear();

        // Load from each source
        self.load_history_commands();
        self.load_makefile_targets(cwd);
        self.load_package_json_scripts(cwd);
        self.load_cargo_commands(cwd);
        self.load_justfile_recipes(cwd);
        self.load_detected_scripts(cwd);
        self.load_user_commands();

        // Sort by frecency
        self.items.sort_by(|a, b| b.frecency_score.partial_cmp(&a.frecency_score).unwrap());

        // Initialize filter
        self.apply_filter();
    }

    fn load_makefile_targets(&mut self, cwd: &Path) {
        let makefile = cwd.join("Makefile");
        if !makefile.exists() {
            return;
        }

        // Parse Makefile for targets
        // Regex: ^([a-zA-Z_][a-zA-Z0-9_-]*):
        if let Ok(content) = std::fs::read_to_string(&makefile) {
            let target_re = regex::Regex::new(r"(?m)^([a-zA-Z_][a-zA-Z0-9_-]*):").unwrap();
            for cap in target_re.captures_iter(&content) {
                let target = &cap[1];
                // Skip internal targets (start with .)
                if !target.starts_with('.') {
                    self.items.push(CommandItem {
                        name: format!("make {}", target),
                        description: "Makefile target".into(),
                        command: format!("make {}", target),
                        source: CommandSource::Makefile,
                        frecency_score: 0.5,  // Default score
                    });
                }
            }
        }
    }

    fn load_detected_scripts(&mut self, cwd: &Path) {
        // Look for executable scripts in common locations
        let script_dirs = [".", "scripts", "bin", ".scripts"];
        let script_extensions = ["sh", "bash", "zsh", "py", "rb", "pl"];

        for dir in &script_dirs {
            let path = cwd.join(dir);
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if self.is_executable_script(&path, &script_extensions) {
                        let name = path.file_name().unwrap().to_string_lossy();
                        self.items.push(CommandItem {
                            name: name.to_string(),
                            description: format!("Script in {}/", dir),
                            command: path.to_string_lossy().to_string(),
                            source: CommandSource::Script,
                            frecency_score: 0.3,
                        });
                    }
                }
            }
        }
    }
}

impl Panel for CommandPalettePanel {
    fn preferred_height(&self) -> u16 { 10 }
    fn title(&self) -> &str { "Commands" }

    fn render(&self, buffer: &mut Buffer, area: Rect) {
        // Filter input at top
        let filter_area = Rect::new(area.x, area.y, area.width, 1);
        let list_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);

        // Render filter input
        Paragraph::new(format!("> {}", self.filter))
            .style(Style::default().fg(Color::Yellow))
            .render(filter_area, buffer);

        // Render command list
        let visible_items: Vec<ListItem> = self.filtered.iter()
            .skip(self.scroll_offset)
            .take(list_area.height as usize)
            .enumerate()
            .map(|(i, &idx)| {
                let item = &self.items[idx];
                let style = if i + self.scroll_offset == self.selection {
                    Style::default().bg(Color::Blue).fg(Color::White)
                } else {
                    Style::default()
                };
                let source_icon = match item.source {
                    CommandSource::Makefile => "⚙",
                    CommandSource::Script => "📜",
                    CommandSource::History => "⏱",
                    CommandSource::PackageJson => "📦",
                    _ => "•",
                };
                ListItem::new(format!("{} {} - {}", source_icon, item.name, item.description))
                    .style(style)
            })
            .collect();

        let list = List::new(visible_items)
            .block(Block::default().borders(Borders::NONE));
        list.render(list_area, buffer);

        // Scrollbar
        if self.filtered.len() > list_area.height as usize {
            let scrollbar = Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight);
            let mut scrollbar_state = ScrollbarState::new(self.filtered.len())
                .position(self.scroll_offset);
            scrollbar.render(list_area, buffer, &mut scrollbar_state);
        }
    }

    fn handle_input(&mut self, key: KeyEvent) -> Result<PanelResult> {
        match key.code {
            KeyCode::Esc => Ok(PanelResult::Dismiss),
            KeyCode::Enter => {
                if let Some(&idx) = self.filtered.get(self.selection) {
                    let cmd = self.items[idx].command.clone();
                    Ok(PanelResult::Execute(cmd))
                } else {
                    Ok(PanelResult::Dismiss)
                }
            }
            KeyCode::Up => {
                self.selection = self.selection.saturating_sub(1);
                self.ensure_visible();
                Ok(PanelResult::Continue)
            }
            KeyCode::Down => {
                self.selection = (self.selection + 1).min(self.filtered.len().saturating_sub(1));
                self.ensure_visible();
                Ok(PanelResult::Continue)
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.apply_filter();
                Ok(PanelResult::Continue)
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.apply_filter();
                Ok(PanelResult::Continue)
            }
            _ => Ok(PanelResult::Continue),
        }
    }
}
```

#### C.3 File Browser Panel

```rust
pub struct FileBrowserPanel {
    current_dir: PathBuf,
    entries: Vec<DirEntry>,
    selection: usize,
    scroll_offset: usize,
    show_hidden: bool,
}

pub struct DirEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    modified: SystemTime,
}

impl Panel for FileBrowserPanel {
    fn preferred_height(&self) -> u16 { 12 }
    fn title(&self) -> &str { "Files" }

    fn render(&self, buffer: &mut Buffer, area: Rect) {
        // Path header
        let header_area = Rect::new(area.x, area.y, area.width, 1);
        let table_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);

        Paragraph::new(self.current_dir.to_string_lossy().to_string())
            .style(Style::default().fg(Color::Cyan).bold())
            .render(header_area, buffer);

        // File table
        let rows: Vec<Row> = self.entries.iter()
            .skip(self.scroll_offset)
            .take(table_area.height as usize)
            .enumerate()
            .map(|(i, entry)| {
                let style = if i + self.scroll_offset == self.selection {
                    Style::default().bg(Color::Blue)
                } else {
                    Style::default()
                };

                let icon = if entry.is_dir { "▸" } else { " " };
                let name_style = if entry.is_dir {
                    style.fg(Color::Blue).bold()
                } else {
                    style
                };

                Row::new(vec![
                    Cell::from(icon),
                    Cell::from(entry.name.clone()).style(name_style),
                    Cell::from(format_size(entry.size)),
                    Cell::from(format_time(entry.modified)),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(rows, [
            Constraint::Length(2),
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(12),
        ])
        .header(Row::new(vec!["", "Name", "Size", "Modified"])
            .style(Style::default().bold()));

        table.render(table_area, buffer);
    }

    fn handle_input(&mut self, key: KeyEvent) -> Result<PanelResult> {
        match key.code {
            KeyCode::Esc => Ok(PanelResult::Dismiss),
            KeyCode::Enter => {
                if let Some(entry) = self.entries.get(self.selection) {
                    if entry.is_dir {
                        // Navigate into directory
                        self.navigate_to(&entry.path)?;
                        Ok(PanelResult::Continue)
                    } else {
                        // Insert path into command line
                        Ok(PanelResult::InsertText(entry.path.to_string_lossy().to_string()))
                    }
                } else {
                    Ok(PanelResult::Continue)
                }
            }
            KeyCode::Backspace => {
                // Go up one directory
                if let Some(parent) = self.current_dir.parent() {
                    self.navigate_to(parent)?;
                }
                Ok(PanelResult::Continue)
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.show_hidden = !self.show_hidden;
                self.refresh()?;
                Ok(PanelResult::Continue)
            }
            KeyCode::Up => {
                self.selection = self.selection.saturating_sub(1);
                self.ensure_visible();
                Ok(PanelResult::Continue)
            }
            KeyCode::Down => {
                self.selection = (self.selection + 1).min(self.entries.len().saturating_sub(1));
                self.ensure_visible();
                Ok(PanelResult::Continue)
            }
            _ => Ok(PanelResult::Continue),
        }
    }
}
```

#### C.4 History Browser Panel

```rust
pub struct HistoryBrowserPanel {
    entries: Vec<HistoryEntry>,
    filtered: Vec<usize>,
    selection: usize,
    scroll_offset: usize,
    filter: String,
}

pub struct HistoryEntry {
    command: String,
    duration: Option<Duration>,
    exit_code: Option<i32>,
    timestamp: SystemTime,
    cwd: Option<PathBuf>,
}

impl Panel for HistoryBrowserPanel {
    fn preferred_height(&self) -> u16 { 10 }
    fn title(&self) -> &str { "History" }

    fn render(&self, buffer: &mut Buffer, area: Rect) {
        // Similar structure to CommandPalettePanel
        // Table with: Command, Duration, Exit, Time
        let filter_area = Rect::new(area.x, area.y, area.width, 1);
        let table_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);

        // Filter input
        Paragraph::new(format!("Filter: {}", self.filter))
            .style(Style::default().fg(Color::Yellow))
            .render(filter_area, buffer);

        // History table
        let rows: Vec<Row> = self.filtered.iter()
            .skip(self.scroll_offset)
            .take(table_area.height as usize)
            .enumerate()
            .map(|(i, &idx)| {
                let entry = &self.entries[idx];
                let style = if i + self.scroll_offset == self.selection {
                    Style::default().bg(Color::Blue)
                } else {
                    Style::default()
                };

                let exit_style = match entry.exit_code {
                    Some(0) => Style::default().fg(Color::Green),
                    Some(_) => Style::default().fg(Color::Red),
                    None => Style::default().fg(Color::Gray),
                };

                Row::new(vec![
                    Cell::from(truncate_command(&entry.command, 40)),
                    Cell::from(format_duration_short(entry.duration)),
                    Cell::from(format_exit_code(entry.exit_code)).style(exit_style),
                    Cell::from(format_relative_time(entry.timestamp)),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(rows, [
            Constraint::Min(30),
            Constraint::Length(8),
            Constraint::Length(4),
            Constraint::Length(10),
        ]);

        table.render(table_area, buffer);
    }
}
```

#### C.5 Help Panel

```rust
pub struct HelpPanel {
    sections: Vec<HelpSection>,
    scroll_offset: usize,
}

pub struct HelpSection {
    title: String,
    entries: Vec<(String, String)>,  // (key, description)
}

impl HelpPanel {
    pub fn new() -> Self {
        Self {
            sections: vec![
                HelpSection {
                    title: "Panel Navigation".into(),
                    entries: vec![
                        ("Tab".into(), "Next tab".into()),
                        ("Shift+Tab".into(), "Previous tab".into()),
                        ("Esc".into(), "Close panel".into()),
                        ("↑/↓".into(), "Navigate items".into()),
                        ("Enter".into(), "Select/Execute".into()),
                    ],
                },
                HelpSection {
                    title: "Command Palette".into(),
                    entries: vec![
                        ("Ctrl+Space".into(), "Open command palette".into()),
                        ("Type".into(), "Filter commands".into()),
                    ],
                },
                HelpSection {
                    title: "File Browser".into(),
                    entries: vec![
                        ("Enter".into(), "Open directory / Insert path".into()),
                        ("Backspace".into(), "Go to parent directory".into()),
                        ("Ctrl+H".into(), "Toggle hidden files".into()),
                    ],
                },
                // More sections...
            ],
            scroll_offset: 0,
        }
    }
}

impl Panel for HelpPanel {
    fn preferred_height(&self) -> u16 { 15 }
    fn title(&self) -> &str { "Help" }

    fn render(&self, buffer: &mut Buffer, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        for section in &self.sections {
            lines.push(Line::from(section.title.clone())
                .style(Style::default().bold().fg(Color::Yellow)));

            for (key, desc) in &section.entries {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:15}", key), Style::default().fg(Color::Cyan)),
                    Span::raw(desc.clone()),
                ]));
            }

            lines.push(Line::from(""));  // Blank line between sections
        }

        let paragraph = Paragraph::new(lines)
            .scroll((self.scroll_offset as u16, 0));
        paragraph.render(area, buffer);

        // Scrollbar if needed
        if lines.len() > area.height as usize {
            let scrollbar = Scrollbar::default()
                .orientation(ScrollbarOrientation::VerticalRight);
            let mut state = ScrollbarState::new(lines.len())
                .position(self.scroll_offset);
            scrollbar.render(area, buffer, &mut state);
        }
    }
}
```

---

## Integration Points

### Reedline ExecuteHostCommand

```rust
// In editor setup
keybindings.add_binding(
    KeyModifiers::CONTROL,
    KeyCode::Char(' '),
    ReedlineEvent::ExecuteHostCommand("open_panel".into()),
);

// In App::run_edit()
match self.editor.read_line(&prompt)? {
    Signal::Success(line) => self.execute_command(line),
    Signal::CtrlC => {},
    Signal::CtrlD => return Ok(()),
    Signal::ExecuteHostCommand(cmd) => {
        match cmd.as_str() {
            "open_panel" => {
                let mut panel = TabbedPanel::new();
                panel.load_context(&self.current_cwd);
                match self.run_panel_mode(&mut panel)? {
                    PanelResult::Execute(cmd) => {
                        self.pending_command = Some(cmd);
                    }
                    PanelResult::InsertText(text) => {
                        // Would need reedline API to insert text
                        // For now, just execute
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
```

### PTY Size Coordination

```rust
impl App {
    fn run_panel_mode<P: Panel>(&mut self, panel: &mut P) -> Result<PanelResult> {
        let (cols, rows) = TerminalGuard::get_size()?;
        let panel_height = panel.preferred_height().min(rows / 2);

        // Expand chrome panel
        self.chrome.expand_panel(panel_height, rows)?;

        // Resize PTY to remaining space
        let effective_rows = rows.saturating_sub(panel_height);
        self.pty.resize(cols, effective_rows)?;

        // ... panel input loop ...

        // Collapse and restore PTY size
        self.chrome.collapse_panel(rows)?;
        let effective_rows = if self.chrome.is_active() {
            rows.saturating_sub(1)
        } else {
            rows
        };
        self.pty.resize(cols, effective_rows)?;

        Ok(result)
    }
}
```

---

## Dependencies

### Cargo.toml Additions

```toml
[dependencies]
# TUI widgets
ratatui = "0.29"

# Regex for Makefile parsing
regex = "1.10"
```

---

## Future Extensibility

### Reserved Tab Slots

The tabbed panel system should reserve slots for future panels:

```rust
pub enum PanelId {
    Commands,      // Command palette
    Files,         // File browser
    History,       // History browser
    Help,          // Help/keybindings

    // Future
    Git,           // Git status, staging
    Bookmarks,     // Directory bookmarks
    Jobs,          // Background job management
    Snippets,      // Code snippets
    Remote,        // SSH host management
}
```

### Plugin System (Future)

```rust
pub trait PanelPlugin: Panel {
    fn id(&self) -> &str;
    fn load(&mut self, ctx: &PluginContext) -> Result<()>;
    fn unload(&mut self) -> Result<()>;
}
```

---

## Testing Strategy

### Unit Tests

- Buffer to ANSI conversion correctness
- Panel state transitions
- Input handling for each panel type
- Truncation and filtering logic

### Integration Tests

- Panel open/close preserves terminal state
- Scroll region correctly adjusted
- PTY size synchronized with panel expansion
- No visual artifacts on rapid open/close

### Manual Testing Checklist

- [ ] Panel opens with Ctrl+Space
- [ ] Tab switching works
- [ ] Escape closes panel cleanly
- [ ] Command execution from palette works
- [ ] File browser navigation works
- [ ] History filtering works
- [ ] Scrollbar appears when needed
- [ ] Resize during panel mode handled
- [ ] Context bar still visible during panel mode
- [ ] No flickering on any operation

---

## Implementation Order

1. **A.1-A.3**: Chrome state, lifecycle methods, buffer conversion
2. **A.4-A.5**: Panel trait and input loop
3. **C.1**: Tab system skeleton
4. **C.2**: Command palette (core feature)
5. **B.1-B.2**: Context bar polish
6. **C.3**: File browser
7. **C.4**: History browser
8. **C.5**: Help panel
9. **B.3**: Notification system

---

## Success Criteria

- Panel opens in <50ms
- No visual glitches during transitions
- All panels functional with keyboard-only navigation
- Makefile targets detected and displayed
- Scripts in standard locations discovered
- Smooth scrolling in all list views
- Tab switching is instantaneous
