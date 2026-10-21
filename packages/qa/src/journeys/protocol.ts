import { randomBytes, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { isInt, isObj, isSha, isStr, problem, type Obj, type Problem } from './util.js';

export const PROTOCOL_VERSION = 1;
export const ENV_PROTOCOL = 'RIGHTKIT_JOURNEYS_PROTOCOL';
export const ENV_SELECTION = 'RIGHTKIT_JOURNEYS_SELECTION';
export const ENV_RESULTS = 'RIGHTKIT_JOURNEYS_RESULTS';

export const JOURNEY_STATUSES = ['passed', 'failed', 'skipped', 'errored'] as const;
export type JourneyStatus = (typeof JOURNEY_STATUSES)[number];
export const EVIDENCE_KINDS = ['log', 'screenshot', 'report', 'trace'] as const;

export interface Selection {
  protocol: 1;
  run_id: string;
  nonce: string;
  app: string;
  platform: string;
  tier: string;
  manifest_sha256: string;
  journeys: { id: string; entry: string }[];
}

export interface Units {
  total: number;
  passed: number;
  failed: number;
  skipped: number;
}
export interface Evidence {
  path: string;
  sha256: string;
  kind: (typeof EVIDENCE_KINDS)[number];
}
export interface ArtifactDigest {
  role: string;
  path: string;
  sha256: string;
}
export interface Isolation {
  data_dir: string;
  guard: 'passed';
  qa_mode: 'runtime';
}
export interface StartRecord {
  type: 'start';
  protocol: 1;
  run_id: string;
  nonce: string;
  adapter: string;
  adapter_version: string;
  started_at: string;
  artifacts: ArtifactDigest[];
  isolation: Isolation;
}
export interface JourneyRecord {
  type: 'journey';
  run_id: string;
  nonce: string;
  id: string;
  status: JourneyStatus;
  started_at: string;
  duration_ms: number;
  units: Units;
  evidence: Evidence[];
  reason: string | null;
}
export interface EndRecord {
  type: 'end';
  run_id: string;
  nonce: string;
  selected: number;
  reported: number;
  ended_at: string;
}

export function newSelection(input: Omit<Selection, 'protocol' | 'run_id' | 'nonce'> & { run_id?: string; nonce?: string }): Selection {
  return {
    protocol: 1,
    run_id: input.run_id ?? randomUUID(),
    nonce: input.nonce ?? randomBytes(16).toString('hex'),
    app: input.app,
    platform: input.platform,
    tier: input.tier,
    manifest_sha256: input.manifest_sha256,
    journeys: input.journeys,
  };
}

export interface RunFiles {
  selectionPath: string;
  resultsPath: string;
}

/** Runner side: write `selection.json` and pre-create an empty `results.jsonl` in the run root. */
export function writeRunFiles(runRoot: string, selection: Selection): RunFiles {
  fs.mkdirSync(runRoot, { recursive: true });
  const selectionPath = path.join(runRoot, 'selection.json');
  const resultsPath = path.join(runRoot, 'results.jsonl');
  fs.writeFileSync(selectionPath, `${JSON.stringify(selection, null, 2)}\n`);
  fs.writeFileSync(resultsPath, '');
  return { selectionPath, resultsPath };
}

/** The protocol environment of D4. Isolation variables come from the guard and are added by the caller. */
export function adapterEnv(files: RunFiles): Record<string, string> {
  return {
    [ENV_PROTOCOL]: String(PROTOCOL_VERSION),
    [ENV_SELECTION]: path.resolve(files.selectionPath),
    [ENV_RESULTS]: path.resolve(files.resultsPath),
  };
}

export function readSelection(file: string): Selection {
  const raw: unknown = JSON.parse(fs.readFileSync(file, 'utf8'));
  if (
    !isObj(raw) ||
    raw.protocol !== 1 ||
    !isStr(raw.run_id) ||
    !isStr(raw.nonce) ||
    !isStr(raw.app) ||
    !isStr(raw.platform) ||
    !isStr(raw.tier) ||
    !isSha(raw.manifest_sha256) ||
    !Array.isArray(raw.journeys) ||
    !raw.journeys.every((j) => isObj(j) && isStr(j.id) && isStr(j.entry))
  ) {
    throw new Error(`invalid journeys selection: ${file}`);
  }
  return raw as unknown as Selection;
}

const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$/;

export function unitsConsistent(units: Units): boolean {
  return (
    units.total >= 1 &&
    units.failed === 0 &&
    units.skipped === 0 &&
    units.passed === units.total
  );
}

function readUnits(v: unknown): Units | null {
  if (!isObj(v) || !isInt(v.total) || !isInt(v.passed) || !isInt(v.failed) || !isInt(v.skipped)) return null;
  return { total: v.total, passed: v.passed, failed: v.failed, skipped: v.skipped };
}

export function readEvidence(v: unknown): Evidence[] | null {
  if (!Array.isArray(v)) return null;
  const out: Evidence[] = [];
  for (const e of v) {
    if (!isObj(e) || !isStr(e.path) || !isSha(e.sha256) || !(EVIDENCE_KINDS as readonly string[]).includes(e.kind as string)) {
      return null;
    }
    out.push({ path: e.path, sha256: e.sha256, kind: e.kind as Evidence['kind'] });
  }
  return out;
}

export interface ParsedResults {
  start: StartRecord | null;
  end: EndRecord | null;
  /** First record per selected id, in file order. */
  journeys: JourneyRecord[];
  duplicates: number;
  /** True when a run_id or nonce disagreed and the whole file was discarded. */
  discarded: boolean;
  problems: Problem[];
}

/**
 * Validate `results.jsonl` against a selection (D4, D5). The file is discarded whole when any record carries
 * another run_id or nonce. Unselected, duplicate, malformed and missing records are problems, never ignored.
 */
export function parseResults(text: string, selection: Pick<Selection, 'run_id' | 'nonce'> & { ids: string[] }): ParsedResults {
  const res: ParsedResults = { start: null, end: null, journeys: [], duplicates: 0, discarded: false, problems: [] };
  const add = (code: string, id: string | null, detail: string): void => void res.problems.push(problem(code, id, detail));
  const lines = text.split('\n').filter((l) => l.trim() !== '');
  if (lines.length === 0) {
    add('protocol_violation', null, 'results file is empty: no start record');
    return res;
  }
  const records: Obj[] = [];
  for (const [i, line] of lines.entries()) {
    try {
      const v: unknown = JSON.parse(line);
      if (!isObj(v)) throw new Error('not an object');
      records.push(v);
    } catch (e) {
      add('protocol_violation', null, `line ${i + 1}: not a JSON object (${(e as Error).message})`);
    }
  }
  if (res.problems.length > 0) return res;
  if (records.some((r) => r.run_id !== selection.run_id || r.nonce !== selection.nonce)) {
    res.discarded = true;
    add('stale_results', null, 'a record carries a run_id or nonce that differs from the selection; the whole file is discarded');
    return res;
  }
  const selected = new Set(selection.ids);
  const seen = new Set<string>();
  let ended = false;
  for (const [i, r] of records.entries()) {
    const where = `line ${i + 1}`;
    if (ended) {
      add('protocol_violation', null, `${where}: record after end`);
      continue;
    }
    if (i === 0 && r.type !== 'start') add('protocol_violation', null, 'first record must be start');
    if (r.type === 'start') {
      const iso = isObj(r.isolation) ? r.isolation : null;
      const arts = Array.isArray(r.artifacts) ? r.artifacts : null;
      if (
        i !== 0 ||
        res.start ||
        r.protocol !== 1 ||
        !isStr(r.adapter) ||
        !isStr(r.adapter_version) ||
        !isStr(r.started_at) ||
        !RFC3339.test(r.started_at) ||
        !arts ||
        !arts.every((a) => isObj(a) && isStr(a.role) && isStr(a.path) && isSha(a.sha256)) ||
        !iso ||
        !isStr(iso.data_dir)
      ) {
        add('protocol_violation', null, `${where}: malformed or repeated start record`);
        continue;
      }
      if (iso.guard !== 'passed' || iso.qa_mode !== 'runtime') {
        add('isolation', null, `${where}: start.isolation must be guard "passed" and qa_mode "runtime"`);
      }
      res.start = r as unknown as StartRecord;
    } else if (r.type === 'journey') {
      const id = isStr(r.id) ? r.id : null;
      const units = readUnits(r.units);
      const evidence = readEvidence(r.evidence);
      const status = r.status as JourneyStatus;
      if (
        !id ||
        !(JOURNEY_STATUSES as readonly string[]).includes(status) ||
        !isStr(r.started_at) ||
        !RFC3339.test(r.started_at) ||
        !isInt(r.duration_ms) ||
        !units ||
        !evidence ||
        (r.reason !== null && typeof r.reason !== 'string') ||
        (status !== 'passed' && !isStr(r.reason))
      ) {
        add('protocol_violation', id, `${where}: malformed journey record (status, units, evidence and a reason for non-passed are required)`);
        continue;
      }
      if (!selected.has(id)) {
        add('unselected_result', id, `${where}: ${id} is not in the selection`);
        continue;
      }
      if (seen.has(id)) {
        res.duplicates++;
        add('duplicate_result', id, `${where}: second record for ${id}`);
        continue;
      }
      seen.add(id);
      res.journeys.push(r as unknown as JourneyRecord);
    } else if (r.type === 'end') {
      if (!isInt(r.selected) || !isInt(r.reported) || !isStr(r.ended_at)) {
        add('protocol_violation', null, `${where}: malformed end record`);
      } else {
        res.end = r as unknown as EndRecord;
        if (r.selected !== selection.ids.length || r.reported !== selection.ids.length) {
          add('protocol_violation', null, `${where}: end.selected=${r.selected} end.reported=${r.reported}, selection has ${selection.ids.length}`);
        }
      }
      ended = true;
    } else add('protocol_violation', null, `${where}: unknown record type`);
  }
  if (!res.start) add('protocol_violation', null, 'no valid start record');
  if (!res.end) add('protocol_violation', null, 'no end record: the adapter did not finish the protocol');
  return res;
}

// ---- Adapter-side helper (Node). Same three calls as the Rust helper: read selection, report, end. ----

export interface JourneyReport {
  id: string;
  status: JourneyStatus;
  started_at?: string;
  duration_ms: number;
  units: Units;
  evidence?: Evidence[];
  reason?: string | null;
}

/** Reads the selection the runner wrote and appends protocol records, one flushed JSON line each. */
export class Reporter {
  readonly selection: Selection;
  private reported = 0;
  private constructor(
    private readonly resultsPath: string,
    selection: Selection,
  ) {
    this.selection = selection;
  }

  static fromEnv(env: NodeJS.ProcessEnv = process.env): Reporter {
    if (env[ENV_PROTOCOL] !== String(PROTOCOL_VERSION)) throw new Error(`${ENV_PROTOCOL} must be ${PROTOCOL_VERSION}`);
    const sel = env[ENV_SELECTION];
    const res = env[ENV_RESULTS];
    if (!sel || !res) throw new Error(`${ENV_SELECTION} and ${ENV_RESULTS} must be set`);
    return new Reporter(res, readSelection(sel));
  }

  private write(record: Obj): void {
    fs.appendFileSync(this.resultsPath, `${JSON.stringify({ ...record, run_id: this.selection.run_id, nonce: this.selection.nonce })}\n`);
  }

  start(input: { adapter: string; adapter_version: string; artifacts: ArtifactDigest[]; isolation: Isolation }): void {
    this.write({ type: 'start', protocol: 1, ...input, started_at: new Date().toISOString() });
  }

  journey(report: JourneyReport): void {
    this.reported++;
    this.write({
      type: 'journey',
      id: report.id,
      status: report.status,
      started_at: report.started_at ?? new Date().toISOString(),
      duration_ms: report.duration_ms,
      units: report.units,
      evidence: report.evidence ?? [],
      reason: report.reason ?? null,
    });
  }

  end(): void {
    this.write({ type: 'end', selected: this.selection.journeys.length, reported: this.reported, ended_at: new Date().toISOString() });
  }
}
