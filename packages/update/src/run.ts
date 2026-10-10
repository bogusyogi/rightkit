// CLI runner, separated from the bin entry so tests can drive it with injected IO and deps.

import fs from "node:fs";
import path from "node:path";
import { checkSources, buildReport, type CheckDeps } from "./check.js";
import { loadManifest, MANIFEST_RELATIVE_PATH } from "./manifest.js";
import { type CheckEntry, type CheckReport } from "./types.js";

export interface CliIo {
  stdout: (text: string) => void;
  stderr: (text: string) => void;
  deps?: CheckDeps;
}

const USAGE = `right-update: per-repo dependency updater

Usage:
  right-update check [--repo <path>] [--json]

Commands:
  check    Compare every pinned dependency with its upstream latest (implemented).
  apply    Not implemented yet.
  gate     Not implemented yet.
  land     Not implemented yet.

Exit codes for check: 0 all current, 1 any outdated, 2 error.
`;

export function packageVersion(): string {
  const pkg = JSON.parse(fs.readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { version: string };
  return pkg.version;
}

/** Run one check against a repository. Never throws. */
export async function runCheck(repo: string, deps: CheckDeps = {}): Promise<CheckReport> {
  const loaded = loadManifest(repo);
  if (!loaded.valid || !loaded.manifest) {
    return { schemaVersion: 1, repo, exitCode: 2, errors: loaded.errors, entries: [] };
  }
  const entries: CheckEntry[] = await checkSources(repo, loaded.manifest, deps);
  return buildReport(repo, entries);
}

function formatText(report: CheckReport): string {
  const lines: string[] = [`right-update check: ${report.repo} (${MANIFEST_RELATIVE_PATH})`];
  for (const err of report.errors) lines.push(`  error: ${err}`);
  if (report.entries.length > 0) {
    const header = ["status", "kind", "name", "pinned", "latest", "policy"];
    const rows = report.entries.map((e) => [e.status, e.kind, e.name, e.pinned ?? "-", e.latest ?? "-", e.policy]);
    const widths = header.map((h, i) => Math.max(h.length, ...rows.map((r) => r[i].length)));
    const fmt = (cols: string[]) => "  " + cols.map((c, i) => c.padEnd(widths[i])).join("  ");
    lines.push(fmt(header), ...rows.map(fmt));
    for (const e of report.entries) {
      if (e.error) lines.push(`  error ${e.kind}:${e.name}: ${e.error}`);
    }
  }
  const counts = { current: 0, outdated: 0, unknown: 0 };
  for (const e of report.entries) counts[e.status] += 1;
  lines.push(`  ${counts.current} current, ${counts.outdated} outdated, ${counts.unknown} unknown, exit ${report.exitCode}`);
  return lines.join("\n") + "\n";
}

/** Dispatch argv. Returns the process exit code. */
export async function runCli(argv: string[], io: CliIo): Promise<number> {
  const [command, ...rest] = argv;
  if (!command || command === "-h" || command === "--help") {
    io.stdout(USAGE);
    return command ? 0 : 2;
  }
  if (command === "--version" || command === "-v") {
    io.stdout(`${packageVersion()}\n`);
    return 0;
  }
  if (command === "apply" || command === "gate" || command === "land") {
    io.stderr(`right-update: '${command}' is not implemented yet (milestone 1 ships 'check' only)\n`);
    return 2;
  }
  if (command !== "check") {
    io.stderr(`right-update: unknown command: ${command}\n\n${USAGE}`);
    return 2;
  }

  let repoArg: string | undefined;
  let json = false;
  for (let i = 0; i < rest.length; i++) {
    const arg = rest[i];
    if (arg === "--json") {
      json = true;
    } else if (arg === "--repo") {
      repoArg = rest[++i];
      if (!repoArg) {
        io.stderr("right-update: --repo requires a path\n");
        return 2;
      }
    } else if (arg.startsWith("--repo=")) {
      repoArg = arg.slice("--repo=".length);
    } else {
      io.stderr(`right-update: unknown argument: ${arg}\n`);
      return 2;
    }
  }

  const repo = path.resolve(repoArg ?? process.cwd());
  const report = await runCheck(repo, io.deps);
  io.stdout(json ? `${JSON.stringify(report, null, 2)}\n` : formatText(report));
  return report.exitCode;
}
