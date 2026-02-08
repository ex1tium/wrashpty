import { spawn } from "child_process";
import { readFileSync } from "fs";
import {
  CommandSchema,
  ExtractOptions,
  ExtractionReport,
  ParseResult,
} from "./types";

export class SchemaDiscoveryError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SchemaDiscoveryError";
  }
}

export class SchemaDiscovery {
  constructor(private cliPath: string = "schema-discover") {}

  async extract(
    commands: string[],
    output: string = "/tmp/schema-output",
    options?: ExtractOptions,
  ): Promise<{ reports: ExtractionReport[]; failures: string[] }> {
    const args = [
      "extract",
      "--commands",
      commands.join(","),
      "--output",
      output,
    ];

    if (options?.installedOnly) args.push("--installed-only");
    if (options?.minConfidence !== undefined)
      args.push("--min-confidence", String(options.minConfidence));
    if (options?.minCoverage !== undefined)
      args.push("--min-coverage", String(options.minCoverage));
    if (options?.jobs !== undefined)
      args.push("--jobs", String(options.jobs));

    await this.runCli(args);

    const reportPath = `${output}/extraction-report.json`;
    const raw = readFileSync(reportPath, "utf-8");
    return JSON.parse(raw);
  }

  async parseStdin(
    command: string,
    helpText: string,
  ): Promise<CommandSchema> {
    const stdout = await this.runCli(
      ["parse-stdin", "--command", command, "--format", "json"],
      helpText,
    );
    return JSON.parse(stdout);
  }

  async parseFile(
    command: string,
    inputPath: string,
  ): Promise<CommandSchema> {
    const stdout = await this.runCli([
      "parse-file",
      "--command",
      command,
      "--input",
      inputPath,
      "--format",
      "json",
    ]);
    return JSON.parse(stdout);
  }

  async parseStdinWithReport(
    command: string,
    helpText: string,
  ): Promise<ParseResult> {
    const stdout = await this.runCli(
      [
        "parse-stdin",
        "--command",
        command,
        "--with-report",
        "--format",
        "json",
      ],
      helpText,
    );
    return JSON.parse(stdout);
  }

  private runCli(args: string[], stdin?: string): Promise<string> {
    return new Promise((resolve, reject) => {
      const proc = spawn(this.cliPath, args, {
        stdio: ["pipe", "pipe", "pipe"],
      });

      let stdout = "";
      let stderr = "";

      proc.stdout.on("data", (data: Buffer) => {
        stdout += data.toString();
      });
      proc.stderr.on("data", (data: Buffer) => {
        stderr += data.toString();
      });

      proc.on("close", (code: number | null) => {
        if (code !== 0) {
          reject(
            new SchemaDiscoveryError(stderr.trim() || `Exit code ${code}`),
          );
        } else {
          resolve(stdout);
        }
      });

      proc.on("error", (err: Error) => {
        reject(new SchemaDiscoveryError(`Failed to spawn CLI: ${err.message}`));
      });

      if (stdin !== undefined) {
        proc.stdin.write(stdin);
        proc.stdin.end();
      } else {
        proc.stdin.end();
      }
    });
  }
}
