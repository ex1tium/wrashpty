export enum FailureCode {
  NotInstalled = "not_installed",
  PermissionBlocked = "permission_blocked",
  Timeout = "timeout",
  NotHelpOutput = "not_help_output",
  ParseFailed = "parse_failed",
  QualityRejected = "quality_rejected",
}

export enum QualityTier {
  High = "high",
  Medium = "medium",
  Low = "low",
  Failed = "failed",
}

export interface FlagSchema {
  short?: string | null;
  long?: string | null;
  value_type: string | { Choice: string[] };
  takes_value: boolean;
  description?: string | null;
  multiple: boolean;
  conflicts_with: string[];
  requires: string[];
}

export interface ArgSchema {
  name: string;
  value_type: string | { Choice: string[] };
  required: boolean;
  multiple: boolean;
  description?: string | null;
}

export interface SubcommandSchema {
  name: string;
  description?: string | null;
  flags: FlagSchema[];
  positional: ArgSchema[];
  subcommands: SubcommandSchema[];
  aliases: string[];
}

export interface CommandSchema {
  schema_version?: string | null;
  command: string;
  description?: string | null;
  global_flags: FlagSchema[];
  subcommands: SubcommandSchema[];
  positional: ArgSchema[];
  source: string;
  confidence: number;
  version?: string | null;
}

export interface FormatScoreReport {
  format: string;
  score: number;
}

export interface ProbeAttemptReport {
  help_flag: string;
  argv: string[];
  exit_code?: number | null;
  timed_out: boolean;
  error?: string | null;
  rejection_reason?: string | null;
  output_source?: string | null;
  output_len: number;
  output_preview?: string | null;
  accepted: boolean;
}

export interface ExtractionReport {
  command: string;
  success: boolean;
  accepted_for_suggestions: boolean;
  quality_tier: QualityTier;
  quality_reasons: string[];
  failure_code?: FailureCode | null;
  failure_detail?: string | null;
  selected_format?: string | null;
  format_scores: FormatScoreReport[];
  parsers_used: string[];
  confidence: number;
  coverage: number;
  relevant_lines: number;
  recognized_lines: number;
  unresolved_lines: string[];
  probe_attempts: ProbeAttemptReport[];
  warnings: string[];
  validation_errors: string[];
}

export interface ExtractionReportBundle {
  schema_version?: string | null;
  generated_at: string;
  version: string;
  reports: ExtractionReport[];
  failures: string[];
}

export interface ExtractOptions {
  installedOnly?: boolean;
  minConfidence?: number;
  minCoverage?: number;
  jobs?: number;
}

export interface ParseResult {
  schema: CommandSchema | null;
  report: ExtractionReport;
}
