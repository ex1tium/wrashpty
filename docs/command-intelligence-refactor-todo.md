# Command Intelligence Refactor TODO

## Phase Checklist

- [x] Phase 1: Workspace + crate split (no runtime discovery dependency)
  - [x] Add workspace configuration in `Cargo.toml`
  - [x] Add `crates/command-schema-core`
  - [x] Add `crates/command-schema-discovery`
  - [x] Remove `src/intelligence/schema/parser.rs`
  - [x] Remove `src/intelligence/schema/extractor.rs`
  - [x] Remove runtime probing hook from `src/intelligence/sync.rs`
  - [x] Update `src/intelligence/schema/mod.rs` exports
- [x] Phase 2: Core package format/validation/merge
- [x] Phase 3: Discovery CLI + fixture tests
- [ ] Phase 4: Curated schema repository + validation workflow
- [ ] Phase 5: Build-time bundle + embedded `SchemaIndex`
- [ ] Phase 6: Suggestion pipeline migration to `SchemaIndex`
- [ ] Phase 7: Bootstrap simplification to schema-driven hierarchy seeding
- [ ] Phase 8: Runtime probing removal + legacy migration cutoff
- [ ] Phase 9: Explicit overlay strategy for uncurated commands
- [ ] Phase 10: Lean schema pack export/import
- [ ] Phase 11: Cleanup, docs, and quality gates

## Invariants

- Curated embedded schemas are structural source of truth.
- Runtime learning influences ranking only.
- `command-schema-discovery` is standalone and never linked into `wrashpty` runtime.
- No hidden parallel schema/probing systems in `src/`.

----

# Command Intelligence Engine: Production Readiness Plan

## Context

The intelligence module (`src/intelligence/`, ~10K lines, 25 files, 629+ tests) has a working suggestion pipeline but suffers from architectural ambiguity: schema data lives in SQLite alongside learned data, bootstrap seeds 250+ lines of hardcoded command lists, export/import covers learned patterns but not schemas, the parser/extractor are locked inside the monolith, and `sync.rs` silently probes `--help` at runtime.

**Goal:** One cohesive engine where curated schemas = structural truth (baked into binary), learned data = ranking signal only, and the schema subsystem is a reusable workspace crate. Zero hidden parallel systems.

**Key decisions (locked):**
- Offline discovery workflow: CLI tool probes `--help` → draft JSON → human curation → commit to repo → embed in binary
- No TUI review UI needed
- Two workspace crates: `command-schema-core` (types/validation) + `command-schema-discovery` (probing/parsing/standalone CLI binary)
- `command-schema-discovery` is **never** a runtime dependency of wrashpty — standalone tool only
- Single binary via `include_str!` embedding with deterministic build
- Export/import = curated schema packs only, dev/admin operation with provenance
- Runtime overlay layer is explicit (no silent structural mutation)

---

## Pipeline Visualization

```
OFFLINE (developer workflow):
  schema-discover CLI → probe --help (allowlist only, hard timeouts)
  → parse → draft JSON per tool → human curation in schemas/curated/*.json
  → CI validates (sorted, fail-fast) → commit

BUILD TIME:
  build.rs reads schemas/curated/*.json (sorted lexicographically)
  → validates every schema (fail on first invalid)
  → bundles into SchemaPackage with hash + version metadata
  → embeds via include_str!

RUNTIME:
  Startup: SchemaIndex::from_embedded() — immutable structural truth
         + optional: load user overlay from ci_command_schemas (explicit, labeled)
  User typing → SuggestionContext
  → SchemaIndex (in-memory) → structural candidates (subcommands, flags, values)
  → Learned hierarchy/patterns (SQLite) → ranking signal (frecency, success, recency)
  → Unified ranking → dedup → penalize failures → boost successes
  → Top-N suggestions to UI
```

---

## Phase 1: Workspace + Crate Split

Convert to workspace. Create both crates. Move code directly — no temporary shims, no discovery dep on wrashpty.

### 1a: Workspace conversion

**Modify:** [Cargo.toml](Cargo.toml)
- Add `[workspace]` with `members = [".", "crates/command-schema-core", "crates/command-schema-discovery"]`
- Add `[workspace.dependencies]` for shared deps: `serde`, `serde_json`, `thiserror`, `regex`, `wait-timeout`, `tracing`, `chrono`, `clap`
- Existing `[package]` stays, shared deps reference `workspace = true`

### 1b: Create `command-schema-core`

Pure library crate. No I/O, no DB, no process spawning.

**Create:**
- `crates/command-schema-core/Cargo.toml` — deps: `serde`, `serde_json`, `thiserror`
- `crates/command-schema-core/src/lib.rs` — module declarations + re-exports
- `crates/command-schema-core/src/types.rs` — **moved** from [schema/types.rs](src/intelligence/schema/types.rs): `SchemaSource`, `ValueType`, `FlagSchema`, `ArgSchema`, `SubcommandSchema`, `CommandSchema`, `ExtractionResult`, `HelpFormat` + all impls and tests

**Modify:** [src/intelligence/schema/types.rs](src/intelligence/schema/types.rs) → becomes `pub use command_schema_core::*;` (thin re-export — only file in main crate that re-exports core types, no discovery re-exports)

**Add dep:** `command-schema-core = { path = "crates/command-schema-core" }` to root `[dependencies]`

### 1c: Create `command-schema-discovery`

Standalone crate. **NOT** a dependency of wrashpty — ever.

**Create:**
- `crates/command-schema-discovery/Cargo.toml` — deps: `command-schema-core`, `regex`, `wait-timeout`, `tracing`, `clap`
- `crates/command-schema-discovery/src/lib.rs`
- `crates/command-schema-discovery/src/parser.rs` — **moved** from [schema/parser.rs](src/intelligence/schema/parser.rs). Import paths changed: `use super::types::{...}` → `use command_schema_core::{...}`
- `crates/command-schema-discovery/src/extractor.rs` — **moved** from [schema/extractor.rs](src/intelligence/schema/extractor.rs). Import paths changed: `use super::parser::HelpParser` → `use crate::parser::HelpParser`, `use super::types::{...}` → `use command_schema_core::{...}`

**Delete from main crate immediately** (no shims):
- [src/intelligence/schema/parser.rs](src/intelligence/schema/parser.rs) — **DELETE** file entirely
- [src/intelligence/schema/extractor.rs](src/intelligence/schema/extractor.rs) — **DELETE** file entirely

**Fix consumers of deleted code (must happen in same commit):**
- [src/intelligence/sync.rs](src/intelligence/sync.rs): `maybe_extract_schema_for_command()` (lines 440-469) calls `extract_command_schema` — **DELETE the entire function** and its call site in `sync_entry()`. Runtime probing is gone.
- [src/intelligence/schema/mod.rs](src/intelligence/schema/mod.rs): Remove `pub mod parser;`, `pub mod extractor;`, and re-exports of `extract_command_schema`, `probe_command_help`, `HelpParser`. Keep only `pub mod storage;` and `pub use command_schema_core::*;`

**Verify:** `cargo build --workspace && cargo test --workspace` — main crate compiles without discovery crate. Discovery crate compiles independently. `grep -r "command_schema_discovery" src/` returns zero hits.

---

## Phase 2: Core Package Format, Validation, Merge, Versioning

New additions to core crate. No existing code changes.

**Create:**
- `crates/command-schema-core/src/package.rs`:
  ```rust
  pub struct SchemaPackage {
      pub version: String,          // semver, e.g. "1.0.0"
      pub name: Option<String>,
      pub description: Option<String>,
      pub generated_at: String,     // ISO 8601
      pub bundle_hash: Option<String>,  // SHA-256 of sorted schema content
      pub schemas: Vec<CommandSchema>,
  }
  ```
- `crates/command-schema-core/src/validate.rs`:
  - `validate_schema(schema: &CommandSchema) -> Vec<ValidationError>` — structural integrity, flag name format, cycle detection in nested subcommands
  - `validate_package(pkg: &SchemaPackage) -> Vec<ValidationError>` — duplicate command detection, version check, completeness
  - Fail-fast: first error is surfaced immediately in build context
- `crates/command-schema-core/src/merge.rs`:
  - `merge_schemas(base: &CommandSchema, overlay: &CommandSchema) -> CommandSchema`
  - `MergeStrategy` enum: `PreferBase`, `PreferOverlay`, `Union`
  - Flag dedup, subcommand union, description precedence

**Verify:** `cargo test -p command-schema-core` with new unit tests

---

## Phase 3: Discovery CLI + Fixture Tests

Standalone binary in discovery crate. Safety-first defaults.

**Create:**
- `crates/command-schema-discovery/src/discover.rs`:
  - `DEFAULT_ALLOWLIST: &[&str]` — curated list (git, docker, cargo, npm, yarn, pnpm, kubectl, systemctl + ~65 simple commands)
  - `discover_tools(config: &DiscoverConfig) -> Vec<String>` — **allowlist-only by default**, full PATH scan requires explicit `--scan-path` flag
  - `DiscoverConfig`: allowlist, opt-in PATH scan flag, max_depth (default 3), timeout_ms (default 5000), excluded commands
  - Hard timeout/depth limits enforced. No shell interpolation — `Command::new()` only, never `sh -c`.
- `crates/command-schema-discovery/src/main.rs` — standalone binary via `[[bin]]`:
  - `schema-discover extract --commands git,docker --output schemas/curated/`
  - `schema-discover extract --allowlist --output schemas/curated/` (default allowlist)
  - `schema-discover extract --scan-path --output schemas/curated/` (explicit opt-in for full PATH scan)
  - `schema-discover validate schemas/curated/*.json` — validate existing schemas
  - `schema-discover bundle schemas/curated/ --output schemas/bundle.json` — deterministic bundle
- `crates/command-schema-discovery/tests/fixtures/` — golden test corpus with captured `--help` outputs from git, docker, cargo, kubectl, systemctl, ripgrep

**Verify:** `cargo build -p command-schema-discovery` builds CLI. `cargo test -p command-schema-discovery` passes (unit + golden fixture tests). Manual: `cargo run -p command-schema-discovery -- extract --commands git` produces valid JSON.

---

## Phase 4: Curated Schema Repository + Build Validation

Generate initial curated schemas, establish the repo directory, add CI-ready validation.

**Create:**
- `schemas/curated/` directory
- Generate schemas: `cargo run -p command-schema-discovery -- extract --allowlist --output schemas/curated/`
- Hand-review each JSON file. Fix inaccuracies.
- Initial set: `git.json`, `docker.json`, `cargo.json`, `npm.json`, `kubectl.json`, `systemctl.json`, `yarn.json`, `pnpm.json`, `core_commands.json` (ls, cat, grep, find, etc.)
- Add `schemas/` to `.gitignore` exception (ensure tracked)

**Validation workflow** (can be run in CI):
```
cargo run -p command-schema-discovery -- validate schemas/curated/*.json
```
Fails on first invalid schema. Checks: structural integrity, no duplicates across files, flag consistency.

**Verify:** All curated JSONs pass validation. Schemas cover at minimum the same ~65 commands currently in [bootstrap.rs](src/intelligence/bootstrap.rs).

---

## Phase 5: Build-Time Bundling + Embedded SchemaIndex

Deterministic build embedding. Fail-fast validation. In-memory lookup at runtime.

**Create:** `build.rs` at project root:
- Reads all `schemas/curated/*.json` files **sorted lexicographically** (deterministic)
- Deserializes each as `CommandSchema`
- Validates every schema via `command_schema_core::validate::validate_schema()` — **fails build on first invalid schema**
- Bundles into `SchemaPackage` with:
  - `version`: from `crates/command-schema-core/` version
  - `bundle_hash`: SHA-256 of serialized sorted content
  - `generated_at`: build timestamp
- Serializes to `$OUT_DIR/embedded_schemas.json`
- Generates `$OUT_DIR/schema_meta.rs` with const metadata (hash, version, schema count)

**Create:** [src/intelligence/schema_index.rs](src/intelligence/schema_index.rs):
```rust
pub struct SchemaIndex {
    embedded: HashMap<String, CommandSchema>,  // immutable, from build
    overlays: HashMap<String, CommandSchema>,  // explicit user overlays (labeled)
}

impl SchemaIndex {
    /// Loads embedded schemas only. No runtime overlays.
    pub fn from_embedded() -> Self { /* include_str! + deserialize */ }

    /// Testing constructor.
    pub fn from_schemas(schemas: Vec<CommandSchema>) -> Self { ... }

    /// Loads user overlay schemas from ci_command_schemas (explicit, labeled).
    /// Only loads rows NOT already in embedded set.
    pub fn load_runtime_overlays(&mut self, conn: &Connection) { ... }

    /// Gets schema: embedded takes priority over overlay.
    pub fn get(&self, command: &str) -> Option<&CommandSchema> {
        self.embedded.get(command).or_else(|| self.overlays.get(command))
    }

    /// Whether command has an embedded (curated) schema.
    pub fn is_curated(&self, command: &str) -> bool {
        self.embedded.contains_key(command)
    }

    /// All known command names (embedded + overlays).
    pub fn commands(&self) -> impl Iterator<Item = &str> { ... }

    /// Bundle metadata (hash, version, count).
    pub fn bundle_meta(&self) -> &BundleMeta { ... }
}
```

**Add build-dep:** `command-schema-core = { path = "crates/command-schema-core" }` in `[build-dependencies]`

**Verify:** `cargo build` succeeds. `SchemaIndex::from_embedded()` loads without panic. Unit tests verify: lookup by name, curated vs overlay distinction, bundle metadata present.

---

## Phase 6: Suggest Pipeline Migration to SchemaIndex

Core pipeline change. Replace SQLite-backed schema suggestions with in-memory SchemaIndex. Structural candidates only.

**Modify:** [src/intelligence/types.rs](src/intelligence/types.rs)
- Add `SuggestionSource::Schema` variant
- `bonus()` → `Self::Schema => 1.1` (above base, below learned hierarchy)
- `label()` → `Self::Schema => "schema"`

**Modify:** [src/intelligence/suggest.rs](src/intelligence/suggest.rs)
- `suggest()` gains `schema_index: &SchemaIndex` parameter
- `gather_suggestions()`: pass `schema_index` through
- `suggest_from_schema()`: takes `&SchemaIndex` instead of `&Connection`
  - Replace `SchemaStore::new(conn).list_commands()` → `schema_index.commands()`
  - Replace `store.get(base_command)` → `schema_index.get(base_command)`
  - Replace `SuggestionSource::LearnedHierarchy` → `SuggestionSource::Schema` in `schema_suggestion()`
- Remove `use super::schema::{SchemaStore, ...}` from suggest.rs

**Modify:** [src/intelligence/mod.rs](src/intelligence/mod.rs)
- Add `schema_index: schema_index::SchemaIndex` field to `CommandIntelligence`
- Initialize in `new()`: `let mut schema_index = SchemaIndex::from_embedded();`
- Optionally: `schema_index.load_runtime_overlays(&conn);` — loads uncurated learned schemas as explicit overlay
- Pass `&self.schema_index` to `suggest::suggest()`

**Update tests:** Schema tests in suggest.rs use `SchemaIndex::from_schemas(vec![...])` instead of `store_schema(&conn, ...)`

**Verify:** `cargo test --lib` — all suggestion tests pass. Schema suggestions labeled `SuggestionSource::Schema`.

---

## Phase 7: Bootstrap Simplification to Schema-Driven Hierarchy Seed

Replace ~250 lines of hardcoded command lists. Hierarchy seeded from embedded schemas.

**Modify:** [src/intelligence/bootstrap.rs](src/intelligence/bootstrap.rs)
- **DELETE:** `seed_all_commands()`, `seed_command_with_subcommands()`, `seed_nested_commands()` (~250 lines of hardcoded lists)
- **DELETE:** `seed_bootstrap_schemas()` and `load_nested_subcommands()` (no longer storing schemas in DB — they're embedded)
- **REPLACE** with `seed_from_schema_index(conn, schema_index)`:
  - Iterates `schema_index.commands()` for all embedded schemas
  - Seeds `ci_command_hierarchy` entries: base command at position 0, subcommands at position 1, nested at position 2+
  - Recursive: walks `CommandSchema.subcommands` tree
- `bootstrap_if_empty()` gains `schema_index: &SchemaIndex` parameter

**Modify:** [src/intelligence/mod.rs](src/intelligence/mod.rs) — pass `&self.schema_index` to `bootstrap_if_empty()`

**Keep:** `get_or_create_token()` helper (still needed for hierarchy seeding)

**Update tests:** Bootstrap tests verify hierarchy populated from embedded schemas. Remove `test_bootstrap_seeds_command_schemas`.

**Verify:** `cargo test --lib` — bootstrap creates hierarchy structure from embedded schemas.

---

## Phase 8: Runtime Probing Removal + Parser/Extractor Cleanup

Complete removal of all help extraction logic from the main crate. Migration cutoff for legacy behavior.

### 8a: Remove `upsert_schema_from_tokens` for curated commands

**Modify:** [src/intelligence/sync.rs](src/intelligence/sync.rs)
- `upsert_schema_from_tokens()` gains `schema_index: &SchemaIndex` parameter
- First line: `if schema_index.is_curated(base_command) { return Ok(()); }` — embedded schema is authoritative, never overwritten by learned data
- Thread through `CommandIntelligence::sync()` and `learn_command()`

**Modify:** [src/intelligence/patterns/mod.rs](src/intelligence/patterns/mod.rs)
- `learn_command()` call to `upsert_schema_from_tokens()` (line 110) gains same guard

### 8b: Migration cutoff for legacy data

**Modify:** [src/intelligence/db_schema.rs](src/intelligence/db_schema.rs)
- Add migration: on schema version bump, execute:
  ```sql
  -- Mark all bootstrap/help-extracted schema rows as non-authoritative
  DELETE FROM ci_command_schemas WHERE source IN ('bootstrap', 'help');
  -- Only 'learned' source rows for uncurated commands survive
  ```
- This runs once on first startup after upgrade. Legacy data from old bootstrap seeding and runtime help probing is purged.
- Future: only `source = 'learned'` rows for commands not in `SchemaIndex.embedded` will exist.

### 8c: Verify no extraction code remains in main crate

Files already deleted in Phase 1c: `schema/parser.rs`, `schema/extractor.rs`.
Function already deleted in Phase 1c: `maybe_extract_schema_for_command()`.

**Verify:**
- `grep -r "probe_command_help\|extract_command_schema\|HelpParser" src/` → zero hits
- `grep -r "command_schema_discovery" src/` → zero hits
- `grep -r "maybe_extract_schema" src/` → zero hits

---

## Phase 9: Decide Overlay Strategy — Explicit User Overlay Layer

Resolve `ci_command_schemas` role. No silent structural mutation.

**Decision:** Keep `ci_command_schemas` as explicit overlay for uncurated commands. Make the overlay visible and labeled.

### Implementation:

**Modify:** [src/intelligence/schema_index.rs](src/intelligence/schema_index.rs)
- `load_runtime_overlays(&mut self, conn: &Connection)` — loads from `ci_command_schemas WHERE source = 'learned'` AND command NOT in `self.embedded`
- Overlay schemas are labeled: `SchemaSource::Learned` preserved, distinguishable from `SchemaSource::Bootstrap`/`SchemaSource::HelpCommand` in embedded set
- `get()` returns embedded first, overlay second — embedded always wins

**Modify:** [src/intelligence/schema/storage.rs](src/intelligence/schema/storage.rs)
- Mark `SchemaStore` as `pub(crate)`
- Remove convenience functions `store_schema`, `get_schema`, `get_all_schemas` from public API
- Only consumer: `sync.rs::upsert_schema_from_tokens` (gated to uncurated commands only)

**Modify:** [src/intelligence/schema/mod.rs](src/intelligence/schema/mod.rs) — strip to:
```rust
pub(crate) mod storage;
pub use command_schema_core::*;
```

**Rule:** `upsert_schema_from_tokens` is the ONLY writer to `ci_command_schemas`. It only writes for commands where `!schema_index.is_curated(cmd)`. It sets `source = 'learned'`. No other code path writes to this table.

**Verify:** `cargo test --lib`. No hidden parallel schema systems. Only one reader (`load_runtime_overlays`) and one writer (`upsert_schema_from_tokens`), both gated.

---

## Phase 10: Lean Schema Export/Import — Curated Packs Only

Export/import is a dev/admin operation for sharing curated schema packs. Not for runtime structural mutation.

**Modify:** [src/intelligence/export.rs](src/intelligence/export.rs)
- Add `export_schema_pack(schema_index: &SchemaIndex) -> Result<String, CIError>`:
  - Serializes embedded schemas as `SchemaPackage` JSON
  - Includes `bundle_hash`, `version`, provenance metadata
- Add `import_schema_pack(schema_index: &mut SchemaIndex, json: &str) -> Result<ImportStats, CIError>`:
  - Validates package via `command_schema_core::validate::validate_package`
  - Writes to **overlay layer only** — never mutates embedded schemas
  - Tags imported schemas with `SchemaSource::Learned` + import provenance (timestamp, package name)
  - Returns stats: imported count, skipped (already curated), conflicts

**Modify:** [src/intelligence/mod.rs](src/intelligence/mod.rs)
- Add `export_schema_pack()` and `import_schema_pack()` methods to `CommandIntelligence`
- Import is gated: only writes to overlay, never overwrites curated

**Semantics:** Import is for adding schemas for tools not in the curated set. To update curated schemas, use the discovery CLI → curate → rebuild workflow. Runtime import cannot silently change structural truth.

**Verify:** Roundtrip tests. Import of a schema for a curated command is skipped (not an error, just reported).

---

## Phase 11: Final Dead-Code Purge, Docs, Perf/Regression Gates

### 11a: Dead code purge

- `cargo clippy --workspace -- -D warnings` — zero warnings
- `cargo fmt --check` — clean formatting
- Remove any unused imports, dead functions, unreachable match arms
- Verify `schema/` submodule only contains `storage.rs` (pub(crate)) and `mod.rs`

### 11b: Direct imports everywhere

- [suggest.rs](src/intelligence/suggest.rs): `use command_schema_core::{CommandSchema, FlagSchema, SubcommandSchema, ValueType}`
- [sync.rs](src/intelligence/sync.rs): `use command_schema_core::{CommandSchema, FlagSchema, SchemaSource, ValueType}`
- [bootstrap.rs](src/intelligence/bootstrap.rs): `use command_schema_core::{CommandSchema, SubcommandSchema}`
- Delete [schema/types.rs](src/intelligence/schema/types.rs) re-export shim if still present

### 11c: Parser hardening (optional, in discovery crate)

- Multi-strategy parsing: Clap → Cobra → GNU/getopt → man-text heuristic (fallback chain)
- Per-component confidence scoring
- Warning capture for unparseable sections
- Add golden fixture tests for more tool outputs

### 11d: Documentation

- Update module-level doc comments in `mod.rs` to reflect new architecture
- Document overlay semantics in `schema_index.rs`
- Document build.rs schema validation in comments

---

## Phase Dependency Graph

```
Phase 1  (workspace + crate split + delete extraction from main crate)
  ├→ Phase 2  (core: package/validate/merge/versioning)
  └→ Phase 3  (discovery: CLI + fixtures)
       └→ Phase 4  (curated schema repo + validation workflow)
            └→ Phase 5  (build.rs bundling + SchemaIndex)
                 └→ Phase 6  (suggest pipeline → SchemaIndex)
                      └→ Phase 7  (bootstrap simplification)
                           └→ Phase 8  (runtime probing removal + migration cutoff)
                                └→ Phase 9  (overlay strategy)
                                     └→ Phase 10  (lean export/import)
                                          └→ Phase 11  (dead-code purge, docs, gates)
```

---

## Production Gates (must all pass)

1. `cargo test --workspace` — all tests pass
2. `cargo clippy --workspace -- -D warnings` — zero warnings
3. `cargo fmt --check` — clean
4. `grep -r "probe_command_help\|extract_command_schema\|HelpParser\|maybe_extract" src/` → zero hits
5. `grep -r "command_schema_discovery" src/` → zero hits (only in `crates/` and `build.rs`)
6. Binary starts with embedded schemas, no runtime help probing
7. Suggestion correctness: `git ` → subcommands, `git commit --` → flags, nested subcommands, value choices
8. Discovery roundtrip: extract → curate → validate → bundle → embed — reproducible (same input → same bundle hash)
9. No duplicate suggestion engines or alternate schema sources active by default
10. Overlay layer is explicit: only uncurated commands, only `source = 'learned'`, only via gated write path

---

## Extraction Logic Cleanup Summary

After Phase 8, the main `wrashpty` binary contains **zero** help parsing/probing code:

| Logic | Current Location | After Migration |
|-------|-----------------|-----------------|
| `HelpParser` (help text → schema) | `schema/parser.rs` | `crates/command-schema-discovery/` only |
| `probe_command_help()` (run --help) | `schema/extractor.rs` | `crates/command-schema-discovery/` only |
| `extract_command_schema()` (recursive probe) | `schema/extractor.rs` | `crates/command-schema-discovery/` only |
| `maybe_extract_schema_for_command()` (runtime probe) | `sync.rs:440-469` | **DELETED** in Phase 1c |
| `seed_bootstrap_schemas()` (hierarchy→schema) | `bootstrap.rs:67-89` | **DELETED** in Phase 7 |
| `load_nested_subcommands()` (hierarchy DB→schema) | `bootstrap.rs:92-122` | **DELETED** in Phase 7 |
| Hard-coded command lists (250+ lines) | `bootstrap.rs:125-381` | **DELETED** in Phase 7 |
| `upsert_schema_from_tokens()` (learn from usage) | `sync.rs:344-437` | **KEPT** — gated to uncurated commands only |
| `SchemaStore` (SQLite CRUD) | `schema/storage.rs` | **KEPT** as `pub(crate)` — overlay cache only |
| Schema types (`CommandSchema`, etc.) | `schema/types.rs` | `crates/command-schema-core/` only |
| Legacy `ci_command_schemas` rows (bootstrap/help) | SQLite | **PURGED** on upgrade migration (Phase 8b) |

The `wrashpty` binary depends on `command-schema-core` (types) but **NOT** on `command-schema-discovery`.

---

## Critical Files Reference

| File | Role | Phases |
|------|------|--------|
| [Cargo.toml](Cargo.toml) | Workspace manifest | 1 |
| [src/intelligence/schema/types.rs](src/intelligence/schema/types.rs) | Schema types (→ core crate, then delete) | 1, 11 |
| [src/intelligence/schema/parser.rs](src/intelligence/schema/parser.rs) | Help parser (→ discovery crate, then delete) | 1 |
| [src/intelligence/schema/extractor.rs](src/intelligence/schema/extractor.rs) | Help extractor (→ discovery crate, then delete) | 1 |
| [src/intelligence/schema/storage.rs](src/intelligence/schema/storage.rs) | SQLite schema store (→ pub(crate)) | 9 |
| [src/intelligence/schema/mod.rs](src/intelligence/schema/mod.rs) | Schema module (strip down) | 1, 9 |
| [src/intelligence/suggest.rs](src/intelligence/suggest.rs) | Suggestion pipeline | 6 |
| [src/intelligence/mod.rs](src/intelligence/mod.rs) | Engine orchestrator | 5, 6, 7, 8, 9, 10 |
| [src/intelligence/bootstrap.rs](src/intelligence/bootstrap.rs) | Seed data (→ simplify) | 7 |
| [src/intelligence/sync.rs](src/intelligence/sync.rs) | History sync (remove probing) | 1, 8 |
| [src/intelligence/patterns/mod.rs](src/intelligence/patterns/mod.rs) | Learning (gate schema writes) | 8 |
| [src/intelligence/export.rs](src/intelligence/export.rs) | Export/import | 10 |
| [src/intelligence/types.rs](src/intelligence/types.rs) | Suggestion types | 6 |
| [src/intelligence/db_schema.rs](src/intelligence/db_schema.rs) | DB migrations | 8 |
| `build.rs` (new) | Deterministic schema embedding | 5 |
| `src/intelligence/schema_index.rs` (new) | In-memory lookup + overlay | 5, 6, 9 |

## Reusable Existing Code

- Suggestion helpers (keep in suggest.rs): `resolve_subcommand_path()`, `find_subcommand_by_path()`, `schema_flags_for_context()`, `schema_flag_value_candidates()`, `wants_flag_suggestions()`, `schema_flag_text()` — [suggest.rs:393-530](src/intelligence/suggest.rs)
- `get_or_create_token()` in [bootstrap.rs:459](src/intelligence/bootstrap.rs) — reused for hierarchy seeding
- `compute_command_hash()` in [tokenizer.rs](src/intelligence/tokenizer.rs) — stays as-is
- `scoring::compute_score()` in [scoring.rs](src/intelligence/scoring.rs) — stays as-is
