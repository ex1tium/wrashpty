# Command Schema Discovery: Production Plan

## Objective

Make `command-schema-discovery` production-ready as:

1. A general-purpose CLI tool.
2. A reusable Rust library.
3. A stable integration surface for other languages via JSON artifacts and thin wrappers.

The parser remains generalized-first, with targeted command adapters only for proven edge cases.

## Current State (Snapshot)

- Parser and extractor hardening is in place (confidence gates, diagnostics, false-positive filtering, deterministic output, targeted probe adapters).
- Quality metadata is now emitted in extraction reports:
  - `accepted_for_suggestions`
  - `quality_tier`
  - `quality_reasons`
  - probe metadata (`rejection_reason`, `output_preview`)
- CLI quality controls are available:
  - `--min-confidence`
  - `--min-coverage`
  - `--allow-low-quality`

Recent run context:

- `make schema-extract-all` (broad list): `154` extracted, `65` failures.
- Failure analysis: almost all failures were probe-level `command not found`, not parser-quality regressions.

## Scope Decisions

Included:

- Better failure taxonomy and observability.
- Installed-only workflows.
- Parse-only interfaces (`stdin`/file input).
- Caching and concurrency with robust version/fingerprint invalidation.
- Multi-format output and stable schema contract.
- Polyglot wrappers (Python/Node) built on CLI.

Explicitly excluded for now:

- Adapter config file system.
- Long-running service/HTTP mode.

## Target Outcomes

1. Clear separation between:
   - probe environment failures,
   - parser extraction failures,
   - quality-gate rejections.
2. Deterministic, versioned machine-readable outputs safe for downstream automation.
3. Fast repeat extraction using cache + controlled parallelism.
4. Easy adoption from Rust, shell, Python, and Node.

## Roadmap

## Phase 1: Reliability and Observability (P0)

### 1. Structured Failure Taxonomy

Add explicit report fields:

- `failure_code`: `not_installed`, `permission_blocked`, `timeout`, `not_help_output`, `parse_failed`, `quality_rejected`.
- `failure_detail`: short normalized detail string.

Acceptance criteria:

- `extraction-report.json` can be aggregated without parsing warning text.
- Top-level failure summary groups by `failure_code`.

### 2. Installed-Only Extraction Mode

CLI addition:

- `--installed-only` to filter command lists before probing.

Makefile integration:

- Prefer installed-only targets for quality benchmarking and CI.

Acceptance criteria:

- Missing-command noise is eliminated when installed-only mode is enabled.

### 3. Parse-Only Interfaces

CLI additions:

- `parse-stdin --command <name>`
- `parse-file --command <name> --input <path>`

Acceptance criteria:

- Help text can be parsed without executing the target command.
- Output supports both schema and extraction report payloads.

## Phase 2: Performance and Cache Correctness (P0)

### 4. Concurrency Control

CLI addition:

- `--jobs <n>` for bounded parallel probing/parsing.

Acceptance criteria:

- Throughput improves on multi-core systems without unstable behavior.
- Output remains deterministic.

### 5. Cache Layer with Fingerprint Invalidation

Cache key components:

- Command name.
- Resolved executable path.
- Executable fingerprint (`mtime + size` or content hash).
- Probe mode.
- Normalized version string (when detected).

Cache payload:

- Probe output.
- Parsed schema.
- Extraction report summary.

Acceptance criteria:

- Repeated runs avoid redundant probes.
- Cache invalidates automatically on executable/version change.

### 6. Version Extraction Hardening

Improve normalization for patterns:

- `v1.0.1`
- `1.0.1`
- `1.0.1v`
- optional suffixes (`-rc1`, `+meta`)

Confidence boosts when token is near:

- words like `version`, `ver`,
- or command banner/tool name (`apt 2.8.3` style).

Guardrails:

- avoid dates and unrelated dotted numbers.

Acceptance criteria:

- Version extraction improves cache correctness with low false positives.
- Tests cover banner-style and keyword-adjacent patterns.

## Phase 3: Product Surface and Polyglot Adoption (P1)

### 7. Output Modes for Human and Machine Consumers

Add `--format` modes:

- `json` (default)
- `yaml`
- `markdown`
- `table`

Acceptance criteria:

- JSON remains canonical and complete.
- Markdown/table summarize key fields for manual review.

### 8. Versioned JSON Contract

Publish JSON Schema docs for:

- `CommandSchema`
- `ExtractionReport`
- `ExtractionReportBundle`

Acceptance criteria:

- Contract versions are explicit and documented.
- Downstream tools can validate payloads using published schemas.

### 9. Thin SDK Wrappers

Provide:

- Python package shelling out to CLI with typed response models.
- Node package shelling out to CLI with typed response models.

Acceptance criteria:

- One-command extract/parse API for both languages.
- Errors map to `failure_code` taxonomy.

## Testing and CI Strategy

- Unit tests:
  - failure classification,
  - version parsing,
  - cache key generation,
  - cache invalidation behavior.
- Integration tests:
  - `extract` installed-only workflow,
  - `parse-stdin` and `parse-file`,
  - multi-format output snapshots.
- CI matrix:
  - at least one Debian-like and one Fedora-like environment.

## Definition of Done

1. Installed-only extraction provides stable quality metrics.
2. Failure taxonomy is structured and consumed by CLI summaries.
3. Parse-only interfaces are available and documented.
4. Cache + concurrency deliver measurable performance gains with deterministic output.
5. Version extraction is robust enough to support cache invalidation safely.
6. JSON contract is documented and versioned.
7. Python/Node wrappers are usable for basic extract/parse workflows.

## Interface Guidance (Recommended Defaults)

- For production schema generation:
  - `extract --installed-only --min-confidence 0.60 --min-coverage 0.20`
- For strict curation:
  - raise confidence threshold and disable `--allow-low-quality`.
- For parser benchmarking:
  - use `parse-file` fixtures to decouple probe environment variability.
