"""Python wrapper for the command-schema-discovery CLI."""

from command_schema_discovery.client import SchemaDiscovery
from command_schema_discovery.types import (
    ArgSchema,
    CommandSchema,
    ExtractionReport,
    FailureCode,
    FlagSchema,
    FormatScoreReport,
    ProbeAttemptReport,
    QualityTier,
    SubcommandSchema,
)

__all__ = [
    "ArgSchema",
    "CommandSchema",
    "ExtractionReport",
    "FailureCode",
    "FlagSchema",
    "FormatScoreReport",
    "ProbeAttemptReport",
    "QualityTier",
    "SchemaDiscovery",
    "SubcommandSchema",
]
