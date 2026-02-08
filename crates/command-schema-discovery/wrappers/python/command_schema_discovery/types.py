"""Type definitions matching Rust schema structures."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, List, Optional


class FailureCode(str, Enum):
    NOT_INSTALLED = "not_installed"
    PERMISSION_BLOCKED = "permission_blocked"
    TIMEOUT = "timeout"
    NOT_HELP_OUTPUT = "not_help_output"
    PARSE_FAILED = "parse_failed"
    QUALITY_REJECTED = "quality_rejected"


class QualityTier(str, Enum):
    HIGH = "high"
    MEDIUM = "medium"
    LOW = "low"
    FAILED = "failed"


@dataclass
class FlagSchema:
    short: Optional[str] = None
    long: Optional[str] = None
    value_type: Any = "Any"
    takes_value: bool = False
    description: Optional[str] = None
    multiple: bool = False
    conflicts_with: List[str] = field(default_factory=list)
    requires: List[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "FlagSchema":
        return cls(
            short=data.get("short"),
            long=data.get("long"),
            value_type=data.get("value_type", "Any"),
            takes_value=data.get("takes_value", False),
            description=data.get("description"),
            multiple=data.get("multiple", False),
            conflicts_with=data.get("conflicts_with", []),
            requires=data.get("requires", []),
        )


@dataclass
class ArgSchema:
    name: str = ""
    value_type: Any = "Any"
    required: bool = False
    multiple: bool = False
    description: Optional[str] = None

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "ArgSchema":
        return cls(
            name=data.get("name", ""),
            value_type=data.get("value_type", "Any"),
            required=data.get("required", False),
            multiple=data.get("multiple", False),
            description=data.get("description"),
        )


@dataclass
class SubcommandSchema:
    name: str = ""
    description: Optional[str] = None
    flags: List[FlagSchema] = field(default_factory=list)
    positional: List[ArgSchema] = field(default_factory=list)
    subcommands: List["SubcommandSchema"] = field(default_factory=list)
    aliases: List[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "SubcommandSchema":
        return cls(
            name=data.get("name", ""),
            description=data.get("description"),
            flags=[FlagSchema.from_dict(f) for f in data.get("flags", [])],
            positional=[ArgSchema.from_dict(a) for a in data.get("positional", [])],
            subcommands=[
                SubcommandSchema.from_dict(s) for s in data.get("subcommands", [])
            ],
            aliases=data.get("aliases", []),
        )


@dataclass
class CommandSchema:
    command: str = ""
    description: Optional[str] = None
    global_flags: List[FlagSchema] = field(default_factory=list)
    subcommands: List[SubcommandSchema] = field(default_factory=list)
    positional: List[ArgSchema] = field(default_factory=list)
    source: str = "HelpCommand"
    confidence: float = 0.0
    version: Optional[str] = None

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "CommandSchema":
        return cls(
            command=data.get("command", ""),
            description=data.get("description"),
            global_flags=[
                FlagSchema.from_dict(f) for f in data.get("global_flags", [])
            ],
            subcommands=[
                SubcommandSchema.from_dict(s) for s in data.get("subcommands", [])
            ],
            positional=[ArgSchema.from_dict(a) for a in data.get("positional", [])],
            source=data.get("source", "HelpCommand"),
            confidence=data.get("confidence", 0.0),
            version=data.get("version"),
        )


@dataclass
class FormatScoreReport:
    format: str = ""
    score: float = 0.0

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "FormatScoreReport":
        return cls(
            format=data.get("format", ""),
            score=data.get("score", 0.0),
        )


@dataclass
class ProbeAttemptReport:
    help_flag: str = ""
    argv: List[str] = field(default_factory=list)
    exit_code: Optional[int] = None
    timed_out: bool = False
    error: Optional[str] = None
    rejection_reason: Optional[str] = None
    output_source: Optional[str] = None
    output_len: int = 0
    output_preview: Optional[str] = None
    accepted: bool = False

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "ProbeAttemptReport":
        return cls(
            help_flag=data.get("help_flag", ""),
            argv=data.get("argv", []),
            exit_code=data.get("exit_code"),
            timed_out=data.get("timed_out", False),
            error=data.get("error"),
            rejection_reason=data.get("rejection_reason"),
            output_source=data.get("output_source"),
            output_len=data.get("output_len", 0),
            output_preview=data.get("output_preview"),
            accepted=data.get("accepted", False),
        )


@dataclass
class ExtractionReport:
    command: str = ""
    success: bool = False
    accepted_for_suggestions: bool = False
    quality_tier: Optional[QualityTier] = None
    quality_reasons: List[str] = field(default_factory=list)
    failure_code: Optional[FailureCode] = None
    failure_detail: Optional[str] = None
    selected_format: Optional[str] = None
    format_scores: List[FormatScoreReport] = field(default_factory=list)
    parsers_used: List[str] = field(default_factory=list)
    confidence: float = 0.0
    coverage: float = 0.0
    relevant_lines: int = 0
    recognized_lines: int = 0
    unresolved_lines: List[str] = field(default_factory=list)
    probe_attempts: List[ProbeAttemptReport] = field(default_factory=list)
    warnings: List[str] = field(default_factory=list)
    validation_errors: List[str] = field(default_factory=list)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "ExtractionReport":
        tier = data.get("quality_tier")
        fc = data.get("failure_code")
        return cls(
            command=data.get("command", ""),
            success=data.get("success", False),
            accepted_for_suggestions=data.get("accepted_for_suggestions", False),
            quality_tier=QualityTier(tier) if tier else None,
            quality_reasons=data.get("quality_reasons", []),
            failure_code=FailureCode(fc) if fc else None,
            failure_detail=data.get("failure_detail"),
            selected_format=data.get("selected_format"),
            format_scores=[
                FormatScoreReport.from_dict(f) for f in data.get("format_scores", [])
            ],
            parsers_used=data.get("parsers_used", []),
            confidence=data.get("confidence", 0.0),
            coverage=data.get("coverage", 0.0),
            relevant_lines=data.get("relevant_lines", 0),
            recognized_lines=data.get("recognized_lines", 0),
            unresolved_lines=data.get("unresolved_lines", []),
            probe_attempts=[
                ProbeAttemptReport.from_dict(p)
                for p in data.get("probe_attempts", [])
            ],
            warnings=data.get("warnings", []),
            validation_errors=data.get("validation_errors", []),
        )
