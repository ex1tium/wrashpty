# Refactor command intelligence to trait-based SchemaProvider with feature-gated compilation tiers

## Summary

This refactor replaces the monolithic `SchemaIndex` with a pluggable `SchemaProvider` trait, introduces feature-gated compilation tiers, delegates schema bundling to the external `command-schema-db` crate, and adds a new Schema Browser panel for interactive command exploration.

### Architectural changes

- **`SchemaProvider` trait** (`src/intelligence/schema_provider.rs`) — Unified interface for schema lookup, discovery, storage, and search with two implementations:
  - `FullSchemaProvider` (feature-gated) backed by `command-schema-db` + learned schemas from `cs_*` SQLite tables
  - `StubSchemaProvider` — zero-cost stub when `command-schema` feature is disabled
- **Lookup priority inverted** — Learned schemas now take priority over bundled (user's actual environment wins), reversing the old "curated always wins" policy
- **`SchemaMode` enum** — Three runtime modes (`HistoryOnly`, `SchemaEnabled`, `FullLibrary`) control schema contribution to suggestions, persisted in settings table across sessions
- **Database schema v3** — Drops legacy `ci_command_schemas` and `ci_suggestion_cache` tables; schema storage now owned by `cs_*` tables from `command-schema-sqlite`
- **Build script gutted** — Schema embedding delegated to `command-schema-db`'s `bundled-schemas` feature; `build.rs` reduced from 122 lines to 4

### Three compilation tiers

| Tier | Flags | Behavior |
|---|---|---|
| Minimal | `--no-default-features` | `StubSchemaProvider`, zero schema overhead |
| Default | _(none)_ | `FullSchemaProvider` with discovery + SQLite, no bundled schemas |
| Full | `--all-features` | Full + 975 bundled command schemas |

### New UI: Schema Browser panel

- **`src/chrome/commands_panel.rs`** — Compound container with inner Tab/Shift-Tab between Discover and Schema Browser sub-panels
- **`src/chrome/schema_browser.rs`** (1087 lines) — Tree view for exploring commands/subcommands/flags with:
  - Incremental type-to-filter search
  - Expand/collapse navigation (Right/Left/Enter)
  - Flat `Vec<TreeNode>` + filtered indices for efficient rendering

### Other changes

- **Suggest** now takes `&dyn SchemaProvider` + `SchemaMode`; schema suggestions gated behind `schema_mode.uses_schemas()` with a 0.9x score factor
- **Sync** is now read-only for schemas — incremental schema building from usage tokens deferred to a future phase; writes happen only on explicit discovery/import
- **Export/import** updated to use `persist_schema()` standalone function + `provider.add_overlay()`
- **`CursorHideGuard`** RAII guard in `src/scrollback/viewer.rs` ensures cursor restoration on panic
- **Dependencies** moved from git to path (`../command-schema/*`); `command-schema-core` is always available, heavy crates are optional

### Files changed

| | Added | Removed | Modified |
|---|---|---|---|
| New | `schema_provider.rs`, `commands_panel.rs`, `schema_browser.rs` | | |
| Deleted | | `schema/storage.rs`, `schema/mod.rs`, `schema_index.rs` | |
| Modified | | | `mod.rs`, `suggest.rs`, `sync.rs`, `db_schema.rs`, `bootstrap.rs`, `export.rs`, `history_store.rs`, `build.rs`, `Cargo.toml`, `viewer.rs`, `tabbed_panel.rs` |

**+2,759 / -1,347** across 21 files

### Migration path

- **Fresh installs** get v3 schema directly (no legacy tables)
- **Upgrades from v1/v2** drop `ci_command_schemas` + `ci_suggestion_cache`; learned schemas must be re-discovered via `--help` extraction or import

## Test plan

- [ ] `cargo test --lib` — all 721+ tests pass
- [ ] `cargo clippy` — zero warnings
- [ ] `cargo build --no-default-features` — StubProvider tier compiles
- [ ] `cargo build` — default tier compiles
- [ ] `cargo build --all-features` — bundled-schemas tier compiles
- [ ] Manual: open Schema Browser panel, verify tree navigation and search
- [ ] Manual: toggle schema mode between HistoryOnly/SchemaEnabled/FullLibrary, verify suggestions change accordingly
- [ ] Manual: upgrade from existing DB, verify v3 migration runs cleanly
