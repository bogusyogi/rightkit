// `right-update gate`: run the manifest's repository gates and each changed tool's journey.
//
// Every command is bounded by a timeout, spawned from an argv array (no shell), and has its
// stdout and stderr saved under a fresh run directory. The result is written to
// `.rightkit/update/gate-report.json`, bound to the apply-report it ran against.

import fs from "node:fs";
import path from "node:path";
import { runCommand, type RunFn } from "./exec.js";
import { loadManifest } from "./manifest.js";
import {
  loadApplyReport,
  type ApplyChange,
  type GateReport,
  type GateResult,
  type JourneyResult,
} from "./reports.js";
import { APPLY_REPORT, GATE_REPORT, GATE_RUNS_DIR, deliveryDir, ensureStateDir, nowIso, stateFile, writeJsonAtomic, type HostEnv } from "./state.js";
import type { GateDef, ToolSource } from "./types.js";

export const DEFAULT_TIMEOUT_SEC = 1800;
const MAX_REPORT_BYTES = 8 * 1024 * 1024;

export interface GateDeps {
  run?: RunFn;
  host?: HostEnv;
  now?: () => Date;
}

export interface GateOutcome {
  /** 0 passed, 1 a gate or journey failed, 2 error, 3 a journey report is missing or malformed. */
  exitCode: 0 | 1 | 2 | 3;
  report?: GateReport;
  reportPath?: string;
  errors: string[];
}

export interface Placeholders {
  candidate: string;
  out: string;
  delivery: string;
  model: string;
}

/** Substitute the four journey placeholders. Other text is left alone. */
export function fillPlaceholders(text: string, values: Placeholders): string {
  return text
    .split("{candidate}").join(values.candidate)
    .split("{out}").join(values.out)
    .split("{delivery}").join(values.delivery)
    .split("{model}").join(values.model);
}

const KEEP_RUN_DIRS = 20;

/** Keep the newest run directories (names sort by timestamp) so unattended runs do not grow without bound. */
function pruneRunDirs(dir: string, keep: number): void {
  try {
    const names = fs.readdirSync(dir, { withFileTypes: true }).filter((d) => d.isDirectory()).map((d) => d.name).sort();
    for (const name of names.slice(0, Math.max(0, names.length - keep))) fs.rmSync(path.join(dir, name), { recursive: true, force: true });
  } catch {
    // Retention is best effort.
  }
}

function safeName(name: string): string {
  return name.replace(/[^A-Za-z0-9._-]/g, "_");
}

async function runGateDef(repo: string, runDir: string, def: GateDef, run: RunFn): Promise<GateResult> {
  const stdoutPath = path.join(runDir, "gates", `${safeName(def.name)}.stdout.log`);
  const stderrPath = path.join(runDir, "gates", `${safeName(def.name)}.stderr.log`);
  const cwd = def.cwd ? path.join(repo, ...def.cwd.split(/[\\/]/)) : repo;
  const result = await run(def.command, { cwd, timeoutMs: (def.timeoutSec ?? DEFAULT_TIMEOUT_SEC) * 1000, stdoutPath, stderrPath });
  const ok = result.code === 0 && !result.timedOut && !result.error;
  return {
    name: def.name,
    ok,
    exitCode: result.code,
    signal: result.signal,
    timedOut: result.timedOut,
    durationMs: result.durationMs,
    stdoutPath: path.relative(repo, stdoutPath).split(path.sep).join("/"),
    stderrPath: path.relative(repo, stderrPath).split(path.sep).join("/"),
    ...(ok ? {} : { detail: result.error ?? (result.timedOut ? `timed out after ${def.timeoutSec ?? DEFAULT_TIMEOUT_SEC}s` : `exit ${result.code ?? result.signal}`) }),
  };
}

/** Read a journey report; it passes only when it is a JSON object with a boolean `ok`. */
function readJourneyReport(file: string): { state: "missing" | "malformed" | "ok"; ok?: boolean; detail?: string } {
  let stat: fs.Stats;
  try {
    stat = fs.statSync(file);
  } catch {
    return { state: "missing", detail: `report not found: ${file}` };
  }
  if (!stat.isFile() || stat.size > MAX_REPORT_BYTES) return { state: "malformed", detail: "report is not a regular file within the size bound" };
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8")) as unknown;
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed) || typeof (parsed as { ok?: unknown }).ok !== "boolean") {
      return { state: "malformed", detail: 'report must be a JSON object with a boolean "ok"' };
    }
    return { state: "ok", ok: (parsed as { ok: boolean }).ok };
  } catch (error) {
    return { state: "malformed", detail: `report is not valid JSON: ${(error as Error).message}` };
  }
}

async function runJourney(repo: string, runDir: string, source: ToolSource, change: ApplyChange, run: RunFn, host: HostEnv): Promise<JourneyResult> {
  const stage = change.tool!;
  const outDir = path.join(runDir, "journeys", safeName(source.name));
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.mkdirSync(outDir, { recursive: true });
  const values: Placeholders = {
    candidate: path.join(repo, ...stage.candidateDir.split("/")),
    out: outDir,
    delivery: deliveryDir(source.name, stage.version, host),
    model: source.journey.model ?? "",
  };
  const argv = source.journey.command.map((arg) => fillPlaceholders(arg, values));
  const reportRaw = fillPlaceholders(source.journey.report, values);
  const reportPath = path.isAbsolute(reportRaw) ? reportRaw : path.join(repo, reportRaw);
  const stdoutPath = path.join(outDir, "journey.stdout.log");
  const stderrPath = path.join(outDir, "journey.stderr.log");
  const timeoutSec = source.journey.timeoutSec ?? DEFAULT_TIMEOUT_SEC;
  const result = await run(argv, {
    cwd: repo,
    timeoutMs: timeoutSec * 1000,
    stdin: JSON.stringify(source.journey.inputs ?? {}),
    stdoutPath,
    stderrPath,
  });

  const base = {
    name: source.name,
    exitCode: result.code,
    timedOut: result.timedOut,
    durationMs: result.durationMs,
    outDir: path.relative(repo, outDir).split(path.sep).join("/"),
    reportPath: path.relative(repo, reportPath).split(path.sep).join("/"),
    stdoutPath: path.relative(repo, stdoutPath).split(path.sep).join("/"),
    stderrPath: path.relative(repo, stderrPath).split(path.sep).join("/"),
  };
  if (result.error) return { ...base, ok: false, inconclusive: false, detail: `could not start journey: ${result.error}` };
  if (result.timedOut) return { ...base, ok: false, inconclusive: false, detail: `timed out after ${timeoutSec}s` };
  if (result.code === 3) return { ...base, ok: false, inconclusive: true, detail: "journey reported inconclusive (exit 3)" };
  if (result.code !== 0) return { ...base, ok: false, inconclusive: false, detail: `journey exited ${result.code ?? result.signal}` };
  const report = readJourneyReport(reportPath);
  if (report.state !== "ok") return { ...base, ok: false, inconclusive: true, detail: report.detail };
  if (!report.ok) return { ...base, ok: false, inconclusive: false, detail: 'journey report says "ok": false' };
  return { ...base, ok: true, inconclusive: false };
}

export async function runGateCommand(options: { repo: string }, deps: GateDeps = {}): Promise<GateOutcome> {
  const repo = path.resolve(options.repo);
  const run = deps.run ?? runCommand;
  const host = deps.host ?? {};
  const loaded = loadManifest(repo);
  if (!loaded.valid || !loaded.manifest) return { exitCode: 2, errors: loaded.errors };
  const manifest = loaded.manifest;

  let applyReportSha256: string | null = null;
  let toolChanges: ApplyChange[] = [];
  const applyFile = stateFile(repo, APPLY_REPORT);
  if (fs.existsSync(applyFile)) {
    const apply = loadApplyReport(applyFile);
    if (!apply.ok) return { exitCode: 2, errors: [apply.error] };
    applyReportSha256 = apply.sha256;
    toolChanges = apply.value.changes.filter((c) => c.kind === "tool" && c.status === "staged" && c.tool);
  }

  const sources = new Map(manifest.sources.filter((s): s is ToolSource => s.kind === "tool").map((s) => [s.name, s]));
  for (const change of toolChanges) {
    if (!sources.has(change.name)) return { exitCode: 2, errors: [`apply-report names tool ${change.name}, which is not in the manifest`] };
    const candidate = path.join(repo, ...change.tool!.candidateDir.split("/"));
    if (!fs.existsSync(candidate)) return { exitCode: 2, errors: [`staged candidate for ${change.name} is missing: ${change.tool!.candidateDir}`] };
  }

  ensureStateDir(repo);
  const started = (deps.now?.() ?? new Date());
  const runId = started.toISOString().replace(/[-:]/g, "").replace(/\.\d+Z$/, "Z");
  const runDir = path.join(repo, ...GATE_RUNS_DIR.split("/"), runId);
  fs.mkdirSync(runDir, { recursive: true });

  // Repository gates share the working tree, so they run in declared order. Journeys have their own
  // output directories and run concurrently with each other and with the gate lane.
  const gateLane = (async () => {
    const results: GateResult[] = [];
    for (const def of manifest.gates ?? []) results.push(await runGateDef(repo, runDir, def, run));
    return results;
  })();
  const journeyLane = Promise.all(toolChanges.map((change) => runJourney(repo, runDir, sources.get(change.name)!, change, run, host)));
  const [gates, journeys] = await Promise.all([gateLane, journeyLane]);

  const failed = gates.some((g) => !g.ok) || journeys.some((j) => !j.ok && !j.inconclusive);
  const inconclusive = !failed && journeys.some((j) => j.inconclusive);
  const exitCode: 0 | 1 | 3 = failed ? 1 : inconclusive ? 3 : 0;
  const warnings: string[] = [];
  if ((manifest.gates ?? []).length === 0) warnings.push("the manifest declares no gates");

  const report: GateReport = {
    schemaVersion: 1,
    repo,
    startedAt: started.toISOString(),
    finishedAt: nowIso(),
    applyReportSha256,
    passed: exitCode === 0,
    exitCode,
    gates,
    journeys,
    warnings,
  };
  const reportPath = stateFile(repo, GATE_REPORT);
  writeJsonAtomic(reportPath, report);
  pruneRunDirs(path.join(repo, ...GATE_RUNS_DIR.split("/")), KEEP_RUN_DIRS);
  return { exitCode, report, reportPath, errors: [] };
}
