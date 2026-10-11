# Dependency Notes

## Inventory (direct dependencies)

Versions from the lockfile at the 2026-10 alignment pass (`cargo tree --depth 1`).

### Terminal, PTY, process

| Crate | Version | Purpose |
|---|---|---|
| `portable-pty` | 0.8.1 | PTY spawn/resize; platform-critical, caret-pinned |
| `reedline` | 0.45.0 | Line editor, SQLite-backed history; API-critical |
| `crossterm` | 0.29.0 | Terminal control, raw mode, events |
| `nix` | 0.27.1 | POSIX syscalls (termios, signals, poll, fs) |
| `signal-hook` | 0.3.18 | Async-signal-safe signal delivery |
| `libc` | 0.2.180 | Raw syscalls (panic hook writes) |
| `wait-timeout` | 0.2.1 | Child process timeouts |
| `which` | 6.0.3 | Executable lookup for discovery helpers |
| `getrandom` | 0.2.17 | Session-token entropy |
| `tempfile` | 3.24.0 | Generated bashrc, test fixtures |

### Storage & data

| Crate | Version | Purpose |
|---|---|---|
| `rusqlite` | 0.37.0 | SQLite (bundled) for history/settings/intelligence |
| `serde` / `serde_json` | 1.0.228 / 1.0.149 | Serialization, JSON output (`--help-format json`) |
| `toml` | 0.8.23 | Cargo.toml parsing for discovery |
| `regex` | 1.12.2 | Makefile/config parsing |
| `chrono` | 0.4.43 | Time formatting for the context bar |

### UI & rendering

| Crate | Version | Purpose |
|---|---|---|
| `ratatui-core` | 0.1.0 | TUI primitives (Buffer, Rect, Style, Layout) |
| `ratatui-widgets` | 0.3.0 | TUI widgets (Block, List, Paragraph, Tabs) |
| `nu-ansi-term` | 0.50.3 | ANSI styling for prompt/chrome |
| `unicode-width` | 0.2.2 | Display width for rendering |
| `unicode-segmentation` | 1.12.0 | Grapheme segmentation |
| `smallvec` | 1.15.1 | Inline storage on hot paths |
| `arboard` | 3.6.1 | Clipboard yank/copy |
| `dirs` | 5.0.1 | Home directory resolution |

### Errors, logging, CLI

| Crate | Version | Purpose |
|---|---|---|
| `anyhow` | 1.0.100 | Internal error handling |
| `thiserror` | 1.0.69 | Public error types |
| `tracing` / `tracing-subscriber` | 0.1.44 / 0.3.22 | Structured logging (file-only, `/tmp/wrashpty-<pid>.log`) |
| `once_cell` | 1.21.3 | Lazy statics |
| `clap` | 4.5.56 | CLI parsing (derive) |

### Command schemas (git-pinned)

| Crate | Version | Purpose |
|---|---|---|
| `command-schema-core` | git rev `de70103` | Schema types (always enabled) |
| `command-schema-discovery` | git rev `de70103` | `--help` extraction (default feature, optional) |
| `command-schema-sqlite` | git rev `de70103` | Schema SQLite storage (default feature, optional) |
| `command-schema-db` | git rev `de70103` | Bundled schema library (optional, `bundled-schemas` feature) |

Dev-dependencies: `rexpect` 0.5 (PTY end-to-end tests), `assert_cmd` 2 (CLI tests), `proptest` 1.4 (parser fuzzing), `serial_test` 3.2 (serialized env-mutating tests).

## Notes

- `ratatui-core` / `ratatui-widgets`: the ratatui project split into focused crates; these are the new split releases with active `0.x` minor churn. They back chrome/scrollback rendering — bump deliberately and re-run the scroll-view integration tests.
- `smallvec`: Used on hot paths where inline storage avoids heap allocations. Version `1.15.1` stays on the current stable major line with broad ecosystem use.
- `nu-ansi-term`: Provides ANSI styling helpers already familiar in shell-adjacent Rust projects. Version `0.50` is a stable release line and matches our prompt/chrome rendering needs.
- `arboard`: Powers clipboard copy/yank features without platform-specific wrappers. Version `3` is an actively maintained major release with wide cross-platform usage.
- `which`: Resolves executables consistently across platforms for discovery and execution helpers. Version `6` is a mature, commonly used release line with low maintenance risk.

## Supply chain

- The four `command-schema-*` crates are pinned to a **personal repository** (`github.com/ex1tium/command-schema`, rev `de701036…`) and ship in the default feature set. Risks: no crates.io provenance, single-maintainer trust, and the pinned sources require rustc ≥ 1.88 (let-chains). Mitigations if this becomes a concern: vendor the sources (`cargo vendor`) or migrate to path dependencies / crates.io publishes once the schema format stabilizes.
- MSRV: `rust-version = "1.88"` (Cargo.toml, driven by the git pins). Clippy's lint MSRV is pinned separately to 1.86 via `clippy.toml` — see the comment there for why.

## cargo audit (2026-10-11, report-only — no bumps made)

2 vulnerabilities, both in transitive dependencies:

| Crate | Version | Advisory | Fix |
|---|---|---|---|
| `crossbeam-epoch` | 0.9.18 | RUSTSEC-2026-0204 — invalid pointer dereference in `fmt::Pointer` impls | ≥ 0.9.20 |
| `time` | 0.3.45 | RUSTSEC-2026-0009 — DoS via stack exhaustion (severity 6.8 medium) | ≥ 0.3.47 |

5 allowed warnings: `serial` 0.4 unmaintained (pulled in by `rexpect`, dev-only), plus unsoundness advisories for `anyhow` 1.0.100, `lru` 0.16.3, `rand` 0.9.2, and `scc` 2.4.0 (all transitive). None are direct dependencies of wrashpty's runtime paths; re-run `cargo audit` after any dependency bump and triage reachability before acting.
