# Dependency Notes

- `smallvec`: Used on hot paths where inline storage avoids heap allocations. Version `1.15.1` stays on the current stable major line with broad ecosystem use.
- `nu-ansi-term`: Provides ANSI styling helpers already familiar in shell-adjacent Rust projects. Version `0.50` is a stable release line and matches our prompt/chrome rendering needs.
- `arboard`: Powers clipboard copy/yank features without platform-specific wrappers. Version `3` is an actively maintained major release with wide cross-platform usage.
- `which`: Resolves executables consistently across platforms for discovery and execution helpers. Version `6` is a mature, commonly used release line with low maintenance risk.
