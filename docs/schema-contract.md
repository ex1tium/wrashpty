# Command Schema Contract v1.0.0

This document defines the JSON schema contract for command schema extraction output.

## CommandSchema

Top-level schema for a single command.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema_version` | string | no | Schema contract version (e.g., "1.0.0") |
| `command` | string | yes | Base command name (e.g., "git", "docker") |
| `description` | string | no | Short description of the command |
| `global_flags` | FlagSchema[] | yes | Flags that apply to all subcommands |
| `subcommands` | SubcommandSchema[] | yes | Available subcommands |
| `positional` | ArgSchema[] | yes | Positional arguments (for commands without subcommands) |
| `source` | SchemaSource | yes | Where this schema came from |
| `confidence` | number | yes | Confidence score (0.0 - 1.0) |
| `version` | string | no | Detected command version string |

## FlagSchema

Schema for a command flag.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `short` | string | no | Short form (e.g., "-m") |
| `long` | string | no | Long form (e.g., "--message") |
| `value_type` | ValueType | yes | Type of value this flag accepts |
| `takes_value` | boolean | yes | Whether a value is required |
| `description` | string | no | Description from help text |
| `multiple` | boolean | yes | Can this flag appear multiple times? |
| `conflicts_with` | string[] | yes | Flags this conflicts with |
| `requires` | string[] | yes | Flags this requires |

## ArgSchema

Schema for a positional argument.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Name of the argument |
| `value_type` | ValueType | yes | Type of value expected |
| `required` | boolean | yes | Is this argument required? |
| `multiple` | boolean | yes | Can multiple values be provided? |
| `description` | string | no | Description from help text |

## SubcommandSchema

Schema for a subcommand.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Name of the subcommand |
| `description` | string | no | Short description |
| `flags` | FlagSchema[] | yes | Flags specific to this subcommand |
| `positional` | ArgSchema[] | yes | Positional arguments |
| `subcommands` | SubcommandSchema[] | yes | Nested subcommands |
| `aliases` | string[] | yes | Aliases for this subcommand |

## SchemaSource

Enum indicating origin of the schema.

| Value | Description |
|-------|-------------|
| `HelpCommand` | Extracted from --help output |
| `ManPage` | Parsed from man page |
| `Bootstrap` | Manually defined |
| `Learned` | Learned from user history |

## ValueType

Enum indicating expected value type.

| Value | Description |
|-------|-------------|
| `Bool` | Boolean flag (no value) |
| `String` | String value |
| `Number` | Numeric value |
| `File` | File path |
| `Directory` | Directory path |
| `Url` | URL |
| `Branch` | Git branch name |
| `Remote` | Git remote name |
| `Choice` | One of specific choices (includes list) |
| `Any` | Unknown/any type |

## ExtractionReport

Per-command extraction diagnostics report.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `command` | string | yes | Command name |
| `success` | boolean | yes | Whether extraction succeeded |
| `accepted_for_suggestions` | boolean | yes | Whether schema passed quality gates |
| `quality_tier` | QualityTier | yes | Quality tier assignment |
| `quality_reasons` | string[] | yes | Reasons for quality tier |
| `failure_code` | FailureCode | no | Structured failure code |
| `failure_detail` | string | no | Human-readable failure detail |
| `selected_format` | string | no | Detected help format |
| `format_scores` | FormatScoreReport[] | yes | Format detection scores |
| `parsers_used` | string[] | yes | Parser strategies used |
| `confidence` | number | yes | Schema confidence (0.0-1.0) |
| `coverage` | number | yes | Parser coverage (0.0-1.0) |
| `relevant_lines` | integer | yes | Lines considered relevant |
| `recognized_lines` | integer | yes | Lines successfully recognized |
| `unresolved_lines` | string[] | yes | Lines not recognized |
| `probe_attempts` | ProbeAttemptReport[] | yes | Details per help-flag probe |
| `warnings` | string[] | yes | Non-fatal warnings |
| `validation_errors` | string[] | yes | Schema validation errors |

## QualityTier

| Value | Description |
|-------|-------------|
| `high` | confidence >= 0.85 and coverage >= 0.6 |
| `medium` | passes minimum thresholds |
| `low` | below thresholds (accepted with --allow-low-quality) |
| `failed` | extraction failed |

## FailureCode

| Value | Description |
|-------|-------------|
| `not_installed` | Command not found on the system |
| `permission_blocked` | Blocked by environment permissions |
| `timeout` | Probe timed out |
| `not_help_output` | Output not recognizable as help text |
| `parse_failed` | Help text found but parsing failed |
| `quality_rejected` | Schema rejected by quality policy |

## FormatScoreReport

Weighted score entry for a detected output format.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `format` | string | yes | Format name |
| `score` | number | yes | Detection score |

## ProbeAttemptReport

Probe attempt metadata for one help-flag invocation.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `help_flag` | string | yes | Help flag used (e.g., "--help") |
| `argv` | string[] | yes | Full command argv |
| `exit_code` | integer | no | Process exit code |
| `timed_out` | boolean | yes | Whether the probe timed out |
| `error` | string | no | Error message if probe failed |
| `rejection_reason` | string | no | Why the output was rejected |
| `output_source` | string | no | Which output stream was used |
| `output_len` | integer | yes | Length of captured output |
| `output_preview` | string | no | Truncated preview of output |
| `accepted` | boolean | yes | Whether this probe was accepted |

## ExtractionReportBundle

Batch report for a full discovery run.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema_version` | string | no | Schema contract version (e.g., "1.0.0") |
| `generated_at` | string (ISO 8601) | yes | Timestamp of generation |
| `version` | string | yes | Tool version |
| `reports` | ExtractionReport[] | yes | Per-command reports |
| `failures` | string[] | yes | Failed command names |
