// CLI runner, separated from the bin entry so tests can drive it with injected IO and deps.

import fs from "node:fs";
import path from "node:path";
import { checkSources, buildReport, type CheckDeps } from "./check.js";
import { runApply, type ApplyDeps, type ApplyOutcome } from "./apply.js";
import { jsonLineSink, noEvents, type EventSink } from "./events.js";
import { runGateCommand, type GateDeps, type GateOutcome } from "./gate.js";
import { dirtyPaths, isRepoRoot } from "./git.js";
import { runDiscard, runLand, type LandDeps, type LandOutcome } from "./land.js";
import { runCommand } from "./exec.js";
import { loadManifest, MANIFEST_RELATIVE_PATH } from "./manifest.js";
import { runSchedule, type ScheduleDeps, type ScheduleResult } from "./schedule.js";
import { STATE_DIR } from "./state.js";
import { type CheckEntry, type CheckReport } from "./types.js";
import { packageVersion } from "./version.js";

export { packageVersion };

export interface CliDeps extends CheckDeps, ApplyDeps, GateDeps, LandDeps {}

export interface CliIo {
  stdout: (text: string) => void;
  stderr: (text: string) => void;
  deps?: CliDeps;
  schedule?: ScheduleDeps;
  now?: () => Date;
}

const USAGE = `right-update: per-repo dependency updater

Usage:
  right-update check    [--repo <path>] [--json] [--quiet]
  right-update apply    [--repo <path>] [--only <name>...] [--dry-run] [--json] [--quiet]
  right-update gate     [--repo <path>] [--json] [--quiet]
  right-update land     [--repo <path>] [--json] [--quiet]
  right-update discard  [--repo <path>] [--json]
  right-update run      [--repo <path>] [--dry-run] [--quiet]
  right-update schedule install|remove|status [--repo <path>] [--interval <minutes>] [--json]

Commands:
  check     Compare every pinned dependency with its upstream latest.
  apply     Rewrite pins to the latest allowed version; breaking bumps are reported, not applied.
  gate      Run the manifest gates and each changed tool's journey.
  land      Commit the verified changes (and push when policy.land is "push").
  discard   Undo an apply that has not been landed.
  run       check, apply, gate and land in one process; what a schedule runs.
  schedule  Install, remove or inspect the recurring run (launchd on macOS, Task Scheduler on Windows).

--quiet prints only JSON lifecycle events (one per line) instead of text.

Exit codes:
  check  0 all current, 1 any outdated, 2 error
  apply  0 done, 1 done with rolled-back rows, 2 refused or error
  gate   0 passed, 1 a gate or journey failed, 2 error, 3 a journey report is missing or malformed
  land   0 landed or nothing to land, 1 refused by a precondition, 2 failed part-way
`;

interface Parsed {
  repo?: string;
  json: boolean;
  quiet: boolean;
  dryRun: boolean;
  only: string[];
  interval?: number;
  positional: string[];
}

type FlagSpec = { bool: string[]; value: string[]; multi?: string[] };

/** Parse argv for one command. Returns an error string for unknown or incomplete flags. */
function parseArgs(args: string[], spec: FlagSpec): Parsed | string {
  const out: Parsed = { json: false, quiet: false, dryRun: false, only: [], positional: [] };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (!arg.startsWith("--")) {
      out.positional.push(arg);
      continue;
    }
    const eq = arg.indexOf("=");
    const name = eq === -1 ? arg.slice(2) : arg.slice(2, eq);
    const inline = eq === -1 ? undefined : arg.slice(eq + 1);
    if (spec.bool.includes(name) && inline === undefined) {
      if (name === "json") out.json = true;
      else if (name === "quiet") out.quiet = true;
      else if (name === "dry-run") out.dryRun = true;
      continue;
    }
    if (spec.multi?.includes(name)) {
      const values: string[] = inline !== undefined ? [inline] : [];
      while (inline === undefined && i + 1 < args.length && !args[i + 1].startsWith("--")) values.push(args[++i]);
      if (values.length === 0) return `--${name} requires at least one name`;
      out.only.push(...values.flatMap((v) => v.split(",")).filter(Boolean));
      continue;
    }
    if (spec.value.includes(name)) {
      const value = inline ?? args[++i];
      if (value === undefined || (inline === undefined && value.startsWith("--"))) return `--${name} requires a value`;
      if (name === "repo") out.repo = value;
      else if (name === "interval") {
        if (!/^\d+$/.test(value)) return "--interval must be a whole number of minutes";
        out.interval = Number(value);
      }
      continue;
    }
    return `unknown argument: ${arg}`;
  }
  return out;
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

function formatCheck(report: CheckReport): string {
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

function formatApply(outcome: ApplyOutcome): string {
  const lines: string[] = [];
  const report = outcome.report;
  lines.push(`right-update apply${report?.dryRun ? " (dry run)" : ""}: ${report?.repo ?? ""}`.trimEnd());
  for (const err of outcome.errors) lines.push(`  error: ${err}`);
  if (report) {
    for (const c of report.changes) lines.push(`  ${c.status.padEnd(7)} ${c.kind} ${c.name} ${c.before} -> ${c.after}${c.files.length ? ` [${c.files.map((f) => f.path).join(", ")}]` : ""}`);
    for (const r of report.needsMigration) lines.push(`  needs-migration ${r.kind} ${r.name} ${r.before ?? "-"}: ${r.reason}`);
    for (const r of report.held) lines.push(`  held ${r.kind} ${r.name}: ${r.reason}`);
    for (const r of report.skipped) lines.push(`  skipped ${r.kind} ${r.name}: ${r.reason}`);
    for (const r of report.failed) lines.push(`  failed ${r.kind} ${r.name}: ${r.reason}`);
    lines.push(`  ${report.changes.length} changed, ${report.needsMigration.length} need migration, ${report.held.length} held, ${report.skipped.length} skipped, ${report.failed.length} failed, exit ${outcome.exitCode}`);
    if (outcome.reportPath) lines.push(`  report: ${outcome.reportPath}`);
  }
  return lines.join("\n") + "\n";
}

function formatGate(outcome: GateOutcome): string {
  const lines = ["right-update gate"];
  for (const err of outcome.errors) lines.push(`  error: ${err}`);
  const report = outcome.report;
  if (report) {
    for (const g of report.gates) lines.push(`  gate ${g.ok ? "ok  " : "FAIL"} ${g.name} (${g.durationMs} ms)${g.detail ? `: ${g.detail}` : ""}`);
    for (const j of report.journeys) lines.push(`  journey ${j.ok ? "ok  " : j.inconclusive ? "????" : "FAIL"} ${j.name} (${j.durationMs} ms)${j.detail ? `: ${j.detail}` : ""}`);
    for (const w of report.warnings) lines.push(`  warning: ${w}`);
    lines.push(`  ${report.passed ? "passed" : "not passed"}, exit ${outcome.exitCode}`);
  }
  return lines.join("\n") + "\n";
}

function formatLand(outcome: LandOutcome): string {
  const lines = ["right-update land"];
  for (const r of outcome.refusals) lines.push(`  refused: ${r}`);
  for (const e of outcome.errors) lines.push(`  error: ${e}`);
  for (const d of outcome.delivered) lines.push(`  delivered: ${d}`);
  for (const n of outcome.notes) lines.push(`  note: ${n}`);
  if (outcome.committed) lines.push(`  committed ${outcome.commit}${outcome.pushed ? ` and pushed (${outcome.pushVia})` : " (not pushed)"}`);
  lines.push(`  ${outcome.changes} change(s), ${outcome.held} held or awaiting migration, exit ${outcome.exitCode}`);
  return lines.join("\n") + "\n";
}

function formatSchedule(result: ScheduleResult): string {
  return `right-update schedule ${result.action}: ${result.message}\n`;
}

async function emitAround<T extends { exitCode: number }>(
  command: string,
  repo: string,
  events: EventSink,
  body: () => Promise<T>,
  extra: (result: T) => Record<string, unknown> = () => ({}),
): Promise<T> {
  const started = Date.now();
  events("update.run.start", { command, repo, version: packageVersion() });
  const result = await body();
  events("update.run.end", { command, repo, exitCode: result.exitCode, durationMs: Date.now() - started, ...extra(result) });
  return result;
}

function gateEvent(outcome: GateOutcome): Record<string, unknown> {
  return {
    ok: outcome.exitCode === 0,
    exitCode: outcome.exitCode,
    errors: outcome.errors,
    gates: (outcome.report?.gates ?? []).map((g) => ({ name: g.name, ok: g.ok, exitCode: g.exitCode, timedOut: g.timedOut, durationMs: g.durationMs })),
    journeys: (outcome.report?.journeys ?? []).map((j) => ({ name: j.name, ok: j.ok, inconclusive: j.inconclusive, exitCode: j.exitCode, durationMs: j.durationMs })),
  };
}

function landEvent(outcome: LandOutcome): Record<string, unknown> {
  return {
    ok: outcome.exitCode === 0,
    exitCode: outcome.exitCode,
    committed: outcome.committed,
    commit: outcome.commit ?? null,
    pushed: outcome.pushed,
    changes: outcome.changes,
    held: outcome.held,
    delivered: outcome.delivered,
    refusals: outcome.refusals,
    errors: outcome.errors,
  };
}

interface RunSummary {
  exitCode: number;
  outcome: string;
}

/** The scheduled pipeline: check, apply, gate, land. Failed or refused work is discarded so the tree is left clean. */
async function runPipeline(repo: string, dryRun: boolean, io: CliIo, events: EventSink): Promise<RunSummary> {
  const deps = io.deps ?? {};
  const check = await runCheck(repo, deps);
  if (check.exitCode === 2) return { exitCode: 2, outcome: `check failed: ${[...check.errors, ...check.entries.filter((e) => e.error).map((e) => `${e.name}: ${e.error}`)].slice(0, 5).join("; ")}` };
  if (check.exitCode === 0) return { exitCode: 0, outcome: "all current" };

  if (!dryRun) {
    const run = deps.run ?? runCommand;
    if (await isRepoRoot({ run, repo })) {
      const dirty = await dirtyPaths({ run, repo });
      const foreign = dirty.paths.filter((p) => !p.startsWith(`${STATE_DIR}/`));
      if (!dirty.ok || foreign.length > 0) {
        return { exitCode: 1, outcome: `refused: working tree is not clean (${foreign.slice(0, 5).join(", ") || dirty.error})` };
      }
    }
  }

  const apply = await runApply({ repo, dryRun }, deps);
  if (apply.exitCode === 2 || !apply.report) return { exitCode: 2, outcome: `apply failed: ${apply.errors.join("; ")}` };
  if (dryRun) return { exitCode: apply.exitCode, outcome: `dry run: ${apply.report.changes.length} change(s) planned` };
  if (apply.report.changes.length === 0) {
    return { exitCode: apply.exitCode, outcome: `nothing applied (${apply.report.needsMigration.length} need migration, ${apply.report.held.length} held, ${apply.report.failed.length} failed)` };
  }

  const gate = await runGateCommand({ repo }, deps);
  events("update.gate.result", gateEvent(gate));
  if (gate.exitCode !== 0) {
    const undone = await runDiscard({ repo }, { run: deps.run });
    return { exitCode: gate.exitCode, outcome: `gate did not pass (exit ${gate.exitCode}); ${undone.exitCode === 0 ? "applied changes were discarded" : `discard failed: ${undone.errors.join("; ")}`}` };
  }

  const land = await runLand({ repo }, deps);
  events("update.land.result", landEvent(land));
  if (land.exitCode === 1) {
    const undone = await runDiscard({ repo }, { run: deps.run });
    return { exitCode: 1, outcome: `land refused: ${land.refusals.join("; ")}; ${undone.exitCode === 0 ? "applied changes were discarded" : `discard failed: ${undone.errors.join("; ")}`}` };
  }
  if (land.exitCode !== 0) return { exitCode: land.exitCode, outcome: `land failed: ${land.errors.join("; ")}` };
  return { exitCode: 0, outcome: land.committed ? `landed ${land.commit}${land.pushed ? " and pushed" : ""}` : "nothing to land" };
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

  const specs: Record<string, FlagSpec> = {
    check: { bool: ["json", "quiet"], value: ["repo"] },
    apply: { bool: ["json", "quiet", "dry-run"], value: ["repo"], multi: ["only"] },
    gate: { bool: ["json", "quiet"], value: ["repo"] },
    land: { bool: ["json", "quiet"], value: ["repo"] },
    discard: { bool: ["json"], value: ["repo"] },
    run: { bool: ["json", "quiet", "dry-run"], value: ["repo"] },
    schedule: { bool: ["json"], value: ["repo", "interval"] },
  };
  const spec = specs[command];
  if (!spec) {
    io.stderr(`right-update: unknown command: ${command}\n\n${USAGE}`);
    return 2;
  }
  const parsed = parseArgs(rest, spec);
  if (typeof parsed === "string") {
    io.stderr(`right-update: ${parsed}\n`);
    return 2;
  }
  if (command !== "schedule" && parsed.positional.length > 0) {
    io.stderr(`right-update: unknown argument: ${parsed.positional[0]}\n`);
    return 2;
  }
  const repo = path.resolve(parsed.repo ?? process.cwd());
  const deps = io.deps ?? {};
  const events: EventSink = parsed.quiet ? jsonLineSink(io.stdout, io.now) : noEvents;
  const print = (text: string, json: unknown) => {
    if (parsed.quiet) return;
    io.stdout(parsed.json ? `${JSON.stringify(json, null, 2)}\n` : text);
  };

  switch (command) {
    case "check": {
      const report = await emitAround("check", repo, events, () => runCheck(repo, deps), (r) => ({ entries: r.entries.length, outdated: r.entries.filter((e) => e.status === "outdated").length }));
      print(formatCheck(report), report);
      return report.exitCode;
    }
    case "apply": {
      const outcome = await emitAround("apply", repo, events, () => runApply({ repo, only: parsed.only, dryRun: parsed.dryRun }, deps), (o) => ({ changes: o.report?.changes.length ?? 0, errors: o.errors }));
      print(formatApply(outcome), { exitCode: outcome.exitCode, errors: outcome.errors, reportPath: outcome.reportPath ?? null, report: outcome.report ?? null });
      if (parsed.quiet || (!outcome.report && outcome.errors.length > 0)) for (const e of outcome.errors) io.stderr(`right-update: ${e}\n`);
      return outcome.exitCode;
    }
    case "gate": {
      const outcome = await emitAround("gate", repo, events, async () => {
        const result = await runGateCommand({ repo }, deps);
        events("update.gate.result", gateEvent(result));
        return result;
      });
      print(formatGate(outcome), { exitCode: outcome.exitCode, errors: outcome.errors, reportPath: outcome.reportPath ?? null, report: outcome.report ?? null });
      if (parsed.quiet) for (const e of outcome.errors) io.stderr(`right-update: ${e}\n`);
      return outcome.exitCode;
    }
    case "land": {
      const outcome = await emitAround("land", repo, events, async () => {
        const result = await runLand({ repo }, deps);
        events("update.land.result", landEvent(result));
        return result;
      });
      print(formatLand(outcome), outcome);
      return outcome.exitCode;
    }
    case "discard": {
      const outcome = await runDiscard({ repo }, { run: deps.run });
      io.stdout(parsed.json ? `${JSON.stringify(outcome, null, 2)}\n` : `right-update discard: ${outcome.exitCode === 0 ? `restored ${outcome.restored.length} file(s)` : outcome.errors.join("; ")}\n`);
      return outcome.exitCode;
    }
    case "run": {
      let summary: RunSummary = { exitCode: 2, outcome: "not run" };
      const result = await emitAround(
        "run",
        repo,
        events,
        async () => {
          summary = await runPipeline(repo, parsed.dryRun, io, events);
          return { exitCode: summary.exitCode };
        },
        () => ({ outcome: summary.outcome }),
      );
      print(`right-update run: ${summary.outcome} (exit ${result.exitCode})\n`, summary);
      return result.exitCode;
    }
    case "schedule": {
      const [action, ...extra] = parsed.positional;
      if (!action || !["install", "remove", "status"].includes(action) || extra.length > 0) {
        io.stderr("right-update: schedule needs one of: install, remove, status\n");
        return 2;
      }
      const result = await runSchedule(action as "install" | "remove" | "status", { repo, intervalMinutes: parsed.interval }, io.schedule ?? {});
      io.stdout(parsed.json ? `${JSON.stringify(result, null, 2)}\n` : formatSchedule(result));
      return result.exitCode;
    }
  }
  return 2;
}
