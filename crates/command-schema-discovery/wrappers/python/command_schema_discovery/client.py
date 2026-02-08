"""Client wrapper for the schema-discover CLI."""

from __future__ import annotations

import json
import subprocess
from typing import Any, Dict, List, Optional

from command_schema_discovery.types import CommandSchema, ExtractionReport


class SchemaDiscoveryError(Exception):
    """Raised when the CLI returns an error."""


class SchemaDiscovery:
    """Thin wrapper around the schema-discover binary."""

    def __init__(self, cli_path: str = "schema-discover"):
        self.cli_path = cli_path

    def extract(
        self,
        commands: List[str],
        output: str = "/tmp/schema-output",
        *,
        installed_only: bool = False,
        min_confidence: Optional[float] = None,
        min_coverage: Optional[float] = None,
        jobs: Optional[int] = None,
    ) -> Dict[str, Any]:
        """Extract schemas for the given commands."""
        args = [
            self.cli_path,
            "extract",
            "--commands",
            ",".join(commands),
            "--output",
            output,
        ]
        if installed_only:
            args.append("--installed-only")
        if min_confidence is not None:
            args.extend(["--min-confidence", str(min_confidence)])
        if min_coverage is not None:
            args.extend(["--min-coverage", str(min_coverage)])
        if jobs is not None:
            args.extend(["--jobs", str(jobs)])

        result = subprocess.run(args, capture_output=True, text=True)
        if result.returncode != 0:
            raise SchemaDiscoveryError(result.stderr.strip())

        report_path = f"{output}/extraction-report.json"
        try:
            with open(report_path) as f:
                return json.load(f)
        except (FileNotFoundError, json.JSONDecodeError) as e:
            raise SchemaDiscoveryError(f"Failed to read report: {e}") from e

    def parse_stdin(self, command: str, help_text: str) -> CommandSchema:
        """Parse help text without executing the command."""
        args = [
            self.cli_path,
            "parse-stdin",
            "--command",
            command,
            "--format",
            "json",
        ]
        result = subprocess.run(
            args,
            input=help_text,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise SchemaDiscoveryError(result.stderr.strip())

        data = json.loads(result.stdout)
        return CommandSchema.from_dict(data)

    def parse_file(self, command: str, input_path: str) -> CommandSchema:
        """Parse help text from a file."""
        args = [
            self.cli_path,
            "parse-file",
            "--command",
            command,
            "--input",
            input_path,
            "--format",
            "json",
        ]
        result = subprocess.run(args, capture_output=True, text=True)
        if result.returncode != 0:
            raise SchemaDiscoveryError(result.stderr.strip())

        data = json.loads(result.stdout)
        return CommandSchema.from_dict(data)

    def parse_stdin_with_report(
        self, command: str, help_text: str
    ) -> Dict[str, Any]:
        """Parse help text and return both schema and report."""
        args = [
            self.cli_path,
            "parse-stdin",
            "--command",
            command,
            "--with-report",
            "--format",
            "json",
        ]
        result = subprocess.run(
            args,
            input=help_text,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise SchemaDiscoveryError(result.stderr.strip())

        data = json.loads(result.stdout)
        return {
            "schema": (
                CommandSchema.from_dict(data["schema"])
                if data.get("schema")
                else None
            ),
            "report": ExtractionReport.from_dict(data["report"]),
        }
