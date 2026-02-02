# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Reedline integration for rich line editing in Edit mode
- Custom prompt showing success ($) or failure (!) indicator
- History loading from ~/.bash_history (last 10,000 lines)
- Background output buffering during editing (64KB max)
- Echo suppression during command injection (EchoGuard)
- Ctrl+C clears line, Ctrl+D exits at empty prompt
- Injection timeout to prevent deadlocks (500ms)

### Changed

- Upgraded reedline from 0.28 to 0.45
- Edit mode now uses reedline instead of stub implementation

## [0.1.0] - Unreleased

### Added

- Project bootstrap with complete module structure
- Panic hook with async-signal-safe terminal restoration
- File-based logging to /tmp/wrashpty.log
- Bash version validation
- CI workflow with test, clippy, and format checks
