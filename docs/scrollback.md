# Scrollback Architecture

The scrollback subsystem captures PTY output, stores it independently of any terminal emulator, and renders view modes (normal, search, filter) on demand.

## Core Types

- `ScrollbackBuffer`: fixed-capacity ring buffer storing captured terminal lines.
- `CaptureState`: streaming parser for PTY bytes into completed logical lines.
- `AltScreenDetector`: detects alternate-screen transitions to suspend/resume capture.
- `ScrollViewer`: stateless renderer for scrollback, search highlights, and filter views.
- `ViewerState`: unified state for scroll-view mode and display toggles.
- `CommandBoundaries`: index of command boundary markers for jump navigation.

## Integration Points

1. Capture path: PTY bytes are fed into `CaptureState`, then committed into `ScrollbackBuffer`.
2. View path: key handling updates offsets/modes and asks `ScrollViewer` to render a viewport.

## Behavioral Notes

- Capture is suspended while in alternate-screen applications (for example, full-screen TUIs).
- Scrollback rendering is decoupled from command execution mode transitions.
- Boundary markers enable command-level navigation (`Ctrl+P` / `Ctrl+N`) within scroll view.
