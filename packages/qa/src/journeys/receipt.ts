import fs from 'node:fs';
import path from 'node:path';
import type { Manifest } from './manifest.js';
import { parseResults, unitsConsistent, readEvidence, type ParsedResults, type Units } from './protocol.js';
import { isHex, isInt, isObj, isSha, isStr, problem, sha256File, sha256Text, type Obj, type Problem } from './util.js';

export const RECEIPT_SCHEMA = 'rightkit.journeys.receipt/1';
export const RECEIPT_STATUSES = ['passed', 'failed', 'skipped', 'errored', 'missing'] as const;
export type ReceiptStatus = (typeof RECEIPT_STATUSES)[number];
export type Verdict = 'passed' | 'failed' | 'incomplete';

export interface ReceiptArtifact {
  role: string;
  path: string;
  size: number;
  sha256: string;
}
export interface ReceiptJourney {
  id: string;
  status: ReceiptStatus;
  release_critical: boolean;
  started_at: string;
  duration_ms: number;
  units: Units;
  evidence: { path: string; sha256: string; kind: string }[];
  reason: string | null;
}
export interface Counts {
  selected: number;
  executed: number;
  passed: number;
  failed: number;
  skipped: number;
  errored: number;
  missing: number;
  duplicate: number;
}
export interface Receipt {
  schema: typeof RECEIPT_SCHEMA;
  run_id: string;
  nonce: string;
  app: string;
  kind: 'functional' | 'visual';
  verdict: Verdict;
  started_at: string;
  ended_at: string;
  host: { os: string; arch: string; session: string };
  source: { head: string; tree: string; dirty: boolean; diff_sha256: string | null };
  manifest: { path: string; sha256: string };
  mapping: { path: string; sha256: string } | null;
  build: {
    command: string[];
    source_tree: string;
    tree_after_build: string;
    started_at: string;
    ended_at: string;
    broker_receipt: { id: string; path: string; sha256: string } | null;
    artifacts: (ReceiptArtifact & { produced_by: 'broker_receipt' | 'build_output' })[];
  };
  launched: ReceiptArtifact[];
  isolation: { data_dir: string; guard: 'passed'; qa_mode: 'runtime'; real_data_touched: false };
  adapter: { name: string; version: string; protocol: 1; argv: string[]; exit_code: number; results_sha256: string };
  selection: { ids: string[]; platform: string; tier: string };
  journeys: ReceiptJourney[];
  counts: Counts;
  problems: Problem[];
}

/** Problems that mean "no usable result" rather than "a journey or binding failed". */
const INCOMPLETE = new Set([
  'schema',
  'zero_selected',
  'zero_executed',
  'missing_result',
  'results_unavailable',
  'protocol_violation',
  'stale_results',
  'manifest_unverified',
  'evidence_unverified',
  'no_launch',
]);

export interface ValidateReceiptOptions {
  /** Contents of results.jsonl the adapter wrote. Required: without it the receipt cannot be proven. */
  resultsText?: string | null;
  /** The manifest bytes and parse of the commit being claimed. Required to bind manifest and selection. */
  manifestText?: string | null;
  manifest?: Manifest | null;
  /** Git tree of the commit being claimed; the receipt's source.tree must equal it. */
  tree?: string;
  /** Release signoff: a dirty source is refused. */
  requireClean?: boolean;
  /** Directory evidence paths resolve against. Required when any journey lists evidence. */
  evidenceRoot?: string;
  /** Also re-hash the launched and built files on this host. */
  checkFiles?: boolean;
}

export interface ReceiptVerdict {
  /** True only when verdict is "passed" and no problem was found. */
  ok: boolean;
  verdict: Verdict;
  /** The verdict recomputed from the evidence, before the receipt's own claim is compared. */
  computed: Verdict;
  problems: Problem[];
  counts: Counts | null;
}

function emptyCounts(): Counts {
  return { selected: 0, executed: 0, passed: 0, failed: 0, skipped: 0, errored: 0, missing: 0, duplicate: 0 };
}

const unitsOf = (v: unknown): Units | null =>
  isObj(v) && isInt(v.total) && isInt(v.passed) && isInt(v.failed) && isInt(v.skipped)
    ? { total: v.total, passed: v.passed, failed: v.failed, skipped: v.skipped }
    : null;

function shapeProblems(r: Obj): Problem[] {
  const out: Problem[] = [];
  const bad = (m: string): void => void out.push(problem('schema', null, m));
  if (r.schema !== RECEIPT_SCHEMA) bad(`schema must be ${RECEIPT_SCHEMA}`);
  for (const k of ['run_id', 'nonce', 'app', 'started_at', 'ended_at'] as const) if (!isStr(r[k])) bad(`${k} missing`);
  if (r.kind !== 'functional' && r.kind !== 'visual') bad('kind must be functional|visual');
  if (r.verdict !== 'passed' && r.verdict !== 'failed' && r.verdict !== 'incomplete') bad('verdict invalid');
  if (!isObj(r.host) || !isStr(r.host.os) || !isStr(r.host.arch) || !isStr(r.host.session)) bad('host invalid');
  const s = r.source;
  if (!isObj(s) || !isHex(s.head, 40) || !isHex(s.tree, 40) || typeof s.dirty !== 'boolean') bad('source invalid');
  else if (s.dirty && !isSha(s.diff_sha256)) bad('source.diff_sha256 is required when dirty');
  if (!isObj(r.manifest) || !isStr(r.manifest.path) || !isSha(r.manifest.sha256)) bad('manifest invalid');
  const b = r.build;
  if (
    !isObj(b) ||
    !Array.isArray(b.command) ||
    !isStr(b.source_tree) ||
    !isStr(b.tree_after_build) ||
    !Array.isArray(b.artifacts) ||
    !b.artifacts.every((a) => isObj(a) && isStr(a.role) && isStr(a.path) && isInt(a.size) && isSha(a.sha256) && (a.produced_by === 'build_output' || a.produced_by === 'broker_receipt'))
  ) {
    bad('build invalid');
  }
  if (!Array.isArray(r.launched) || !r.launched.every((a) => isObj(a) && isStr(a.role) && isStr(a.path) && isInt(a.size) && isSha(a.sha256))) bad('launched invalid');
  if (!isObj(r.isolation) || !isStr(r.isolation.data_dir)) bad('isolation invalid');
  const a = r.adapter;
  if (!isObj(a) || !isStr(a.name) || !isStr(a.version) || a.protocol !== 1 || !Array.isArray(a.argv) || typeof a.exit_code !== 'number' || !isSha(a.results_sha256)) bad('adapter invalid');
  const sel = r.selection;
  if (!isObj(sel) || !Array.isArray(sel.ids) || !sel.ids.every(isStr) || !isStr(sel.platform) || !isStr(sel.tier)) bad('selection invalid');
  if (!Array.isArray(r.journeys)) bad('journeys must be an array');
  else {
    for (const [i, j] of r.journeys.entries()) {
      if (
        !isObj(j) ||
        !isStr(j.id) ||
        !(RECEIPT_STATUSES as readonly string[]).includes(j.status as string) ||
        typeof j.release_critical !== 'boolean' ||
        !unitsOf(j.units) ||
        !readEvidence(j.evidence) ||
        !isInt(j.duration_ms) ||
        (j.reason !== null && typeof j.reason !== 'string')
      ) {
        bad(`journeys[${i}] invalid`);
      }
    }
  }
  if (!isObj(r.counts) || !(['selected', 'executed', 'passed', 'failed', 'skipped', 'errored', 'missing', 'duplicate'] as const).every((k) => isInt((r.counts as Obj)[k]))) bad('counts invalid');
  if (!Array.isArray(r.problems)) bad('problems must be an array');
  return out;
}

/**
 * Re-check a receipt offline against every D5 pass condition. The receipt's own verdict, counts and problems are
 * claims, never inputs: everything is recomputed from the journey records, the results file, the manifest and the
 * digests. Anything that cannot be proven yields "incomplete", never "passed".
 */
export function validateReceipt(input: unknown, opts: ValidateReceiptOptions = {}): ReceiptVerdict {
  if (!isObj(input)) {
    return { ok: false, verdict: 'incomplete', computed: 'incomplete', problems: [problem('schema', null, 'receipt is not a JSON object')], counts: null };
  }
  const shape = shapeProblems(input);
  if (shape.length > 0) return { ok: false, verdict: 'incomplete', computed: 'incomplete', problems: shape, counts: null };
  const r = input as unknown as Receipt;
  const out: Problem[] = [];
  const add = (code: string, id: string | null, detail: string): void => void out.push(problem(code, id, detail));

  // Rule 1: selection and execution.
  const ids = r.selection.ids;
  const idSet = new Set(ids);
  if (idSet.size !== ids.length) add('protocol_violation', null, 'selection.ids contains a duplicate');
  if (ids.length === 0) add('zero_selected', null, 'no journey was selected');

  // Rule 2: exactly one record per selected id, from the receipt entries.
  const entries = new Map<string, ReceiptJourney[]>();
  for (const j of r.journeys) entries.set(j.id, [...(entries.get(j.id) ?? []), j]);
  let duplicates = 0;
  for (const [id, list] of entries) {
    if (!idSet.has(id)) add('unselected_result', id, `${id} is in the receipt but not in the selection`);
    if (list.length > 1) {
      duplicates += list.length - 1;
      add('duplicate_result', id, `${id} appears ${list.length} times`);
    }
  }
  const counts = emptyCounts();
  counts.selected = ids.length;
  counts.duplicate = duplicates;
  for (const id of idSet) {
    const list = entries.get(id);
    const first = list?.[0];
    if (!first || first.status === 'missing') {
      counts.missing++;
      add('missing_result', id, `${id} has no result`);
      continue;
    }
    counts.executed++;
    // Rule 3: only a consistent "passed" is a pass.
    if (first.status === 'passed') {
      counts.passed++;
      if (!unitsConsistent(first.units)) {
        add('journey_inconsistent', id, `${id} is "passed" but units are ${JSON.stringify(first.units)}`);
      }
    } else if (first.status === 'failed') {
      counts.failed++;
      add('journey_failed', id, `${id} failed${first.reason ? `: ${first.reason}` : ''}`);
    } else if (first.status === 'skipped') {
      counts.skipped++;
      add('skipped', id, `${id} was skipped; skipped is not a pass`);
    } else {
      counts.errored++;
      add('journey_errored', id, `${id} errored${first.reason ? `: ${first.reason}` : ''}`);
    }
  }
  if (ids.length > 0 && counts.executed === 0) add('zero_executed', null, 'no selected journey executed');
  if (ids.length > 0 && counts.executed !== counts.selected && counts.missing === 0) add('zero_executed', null, 'executed != selected');
  for (const k of ['selected', 'executed', 'passed', 'failed', 'skipped', 'errored', 'missing'] as const) {
    if (r.counts[k] !== counts[k]) add('counts_mismatch', null, `counts.${k} claims ${r.counts[k]}, records say ${counts[k]}`);
  }

  // Rule 4: results file, start/end, nonce and run_id.
  let parsed: ParsedResults | null = null;
  if (opts.resultsText === undefined || opts.resultsText === null) {
    add('results_unavailable', null, 'results.jsonl was not supplied; the receipt cannot be proven');
  } else {
    if (sha256Text(opts.resultsText) !== r.adapter.results_sha256) {
      add('adapter_exit', null, 'adapter.results_sha256 does not match the results file read');
    }
    parsed = parseResults(opts.resultsText, { run_id: r.run_id, nonce: r.nonce, ids });
    out.push(...parsed.problems);
    if (counts.duplicate !== parsed.duplicates && parsed.duplicates > 0) counts.duplicate = Math.max(counts.duplicate, parsed.duplicates);
    if (!parsed.discarded) {
      const byId = new Map(parsed.journeys.map((j) => [j.id, j]));
      for (const id of idSet) {
        const rec = byId.get(id);
        const claimed = entries.get(id)?.[0];
        if (!rec) {
          if (claimed && claimed.status !== 'missing') add('receipt_results_mismatch', id, `${id} is in the receipt but has no record in results.jsonl`);
        } else if (!claimed || claimed.status !== rec.status) {
          add('receipt_results_mismatch', id, `${id}: receipt says ${claimed?.status ?? 'nothing'}, results say ${rec.status}`);
        }
        if (rec && rec.status === 'passed' && !unitsConsistent(rec.units)) {
          add('journey_inconsistent', id, `${id}: results record is "passed" with units ${JSON.stringify(rec.units)}`);
        }
      }
    }
    if (parsed.start) {
      const startBy = new Map(parsed.start.artifacts.map((a) => [a.role, a.sha256]));
      for (const l of r.launched) {
        if (startBy.get(l.role) !== l.sha256) add('artifact_mismatch', null, `launched ${l.role} digest differs from the adapter's start record`);
      }
      for (const [role] of startBy) {
        if (!r.launched.some((l) => l.role === role)) add('artifact_mismatch', null, `adapter reported ${role} but the runner did not launch-measure it`);
      }
      if (parsed.start.isolation.data_dir !== r.isolation.data_dir) add('isolation', null, 'start.isolation.data_dir differs from the receipt');
      if (parsed.start.adapter !== r.adapter.name || parsed.start.adapter_version !== r.adapter.version) {
        add('protocol_violation', null, 'start record adapter name or version differs from the receipt');
      }
    }
  }

  // Rule 5: artifact provenance and bindings.
  const b = r.build;
  if (b.source_tree !== r.source.tree || b.tree_after_build !== r.source.tree) {
    add('source_mismatch', null, 'build.source_tree, build.tree_after_build and source.tree must be equal');
  }
  if (opts.tree !== undefined && opts.tree !== r.source.tree) add('source_mismatch', null, `receipt source.tree ${r.source.tree} is not the claimed tree ${opts.tree}`);
  if (r.launched.length === 0) add('no_launch', null, 'no launched artifact was recorded: the adapter never started');
  for (const l of r.launched) {
    const built = b.artifacts.filter((a) => a.role === l.role);
    if (built.length === 0) {
      add('artifact_not_from_build', null, `launched ${l.role} has no build artifact of that role`);
      continue;
    }
    if (!built.some((a) => a.sha256 === l.sha256)) add('artifact_mismatch', null, `launched ${l.role} sha256 differs from the built artifact`);
  }
  for (const a of b.artifacts) {
    if (a.produced_by === 'broker_receipt' && (!b.broker_receipt || !isStr(b.broker_receipt.id) || !isSha(b.broker_receipt.sha256))) {
      add('artifact_not_from_build', null, `artifact ${a.role} claims a broker receipt that is not recorded`);
    }
  }
  if (opts.manifestText === undefined || opts.manifestText === null) {
    add('manifest_unverified', null, 'the manifest was not supplied; selection and bindings cannot be checked');
  } else if (sha256Text(opts.manifestText) !== r.manifest.sha256) {
    add('source_mismatch', null, 'manifest sha256 differs from the receipt');
  }
  if (opts.manifest) {
    if (opts.manifest.app !== r.app) add('source_mismatch', null, `manifest app ${opts.manifest.app} differs from receipt app ${r.app}`);
    for (const id of ids) {
      const decl = opts.manifest.journeys.find((j) => j.id === id);
      if (!decl) {
        add('selection_invalid', id, `${id} is not declared in the manifest`);
        continue;
      }
      if (!decl.platforms.includes(r.selection.platform as never)) add('selection_invalid', id, `${id} does not declare platform ${r.selection.platform}`);
      if (decl.release_critical !== entries.get(id)?.[0]?.release_critical && entries.get(id)) add('selection_invalid', id, `${id} release_critical differs from the manifest`);
    }
  }
  if (r.source.dirty && opts.requireClean) add('dirty_source', null, 'source tree was dirty; release signoff requires a clean tree');

  // Rule 6: isolation.
  const iso = r.isolation as Obj;
  if (iso.guard !== 'passed' || iso.qa_mode !== 'runtime' || iso.real_data_touched !== false) {
    add('isolation', null, 'isolation must be guard "passed", qa_mode "runtime" and real_data_touched false');
  }

  // Rule 7: adapter exit and evidence digests.
  // Nothing launched is no_launch (incomplete); the adapter never ran, so its exit code says nothing.
  if (r.adapter.exit_code !== 0 && r.launched.length > 0) add('adapter_exit', null, `adapter exited ${r.adapter.exit_code}`);
  for (const j of r.journeys) {
    for (const e of j.evidence) {
      const root = opts.evidenceRoot;
      if (!root) {
        add('evidence_unverified', j.id, 'evidenceRoot was not supplied; evidence digests cannot be checked');
        break;
      }
      const full = path.resolve(root, e.path);
      const rel = path.relative(path.resolve(root), full);
      if (path.isAbsolute(e.path) || rel.startsWith('..') || path.isAbsolute(rel)) {
        add('evidence_mismatch', j.id, `evidence path ${e.path} escapes the run root`);
        continue;
      }
      const digest = sha256File(full);
      if (digest === null) add('evidence_mismatch', j.id, `evidence ${e.path} is missing`);
      else if (digest !== e.sha256) add('evidence_mismatch', j.id, `evidence ${e.path} digest differs`);
    }
  }
  if (opts.checkFiles) {
    for (const l of [...r.launched, ...b.artifacts]) {
      if (sha256File(l.path) !== l.sha256) add('artifact_mismatch', null, `${l.role} at ${l.path} does not hash to the recorded sha256 on this host`);
    }
  }

  let verdict: Verdict = out.length === 0 ? 'passed' : out.some((p) => INCOMPLETE.has(p.code)) && !out.some((p) => !INCOMPLETE.has(p.code)) ? 'incomplete' : 'failed';
  if (out.length > 0 && verdict === 'passed') verdict = 'failed';
  const computed = verdict;
  if (r.verdict !== verdict) {
    add('verdict_mismatch', null, `receipt claims ${r.verdict}, recomputed ${verdict}`);
    if (verdict === 'passed') verdict = 'failed';
  }
  return { ok: verdict === 'passed' && out.length === 0, verdict, computed, problems: out, counts };
}

/** Read a receipt and its sibling files (results.jsonl beside it by default), for the CLI and callers. */
export function validateReceiptFile(
  file: string,
  opts: { resultsPath?: string; manifestPath?: string; tree?: string; requireClean?: boolean; checkFiles?: boolean; manifest?: Manifest | null } = {},
): ReceiptVerdict {
  let raw: unknown;
  try {
    raw = JSON.parse(fs.readFileSync(file, 'utf8'));
  } catch (e) {
    return { ok: false, verdict: 'incomplete', computed: 'incomplete', problems: [problem('schema', null, `cannot read receipt: ${(e as Error).message}`)], counts: null };
  }
  const dir = path.dirname(path.resolve(file));
  const read = (p: string | undefined): string | null => {
    if (!p) return null;
    try {
      return fs.readFileSync(p, 'utf8');
    } catch {
      return null;
    }
  };
  return validateReceipt(raw, {
    resultsText: read(opts.resultsPath ?? path.join(dir, 'results.jsonl')),
    manifestText: read(opts.manifestPath),
    manifest: opts.manifest ?? null,
    evidenceRoot: dir,
    ...(opts.tree !== undefined ? { tree: opts.tree } : {}),
    ...(opts.requireClean ? { requireClean: true } : {}),
    ...(opts.checkFiles ? { checkFiles: true } : {}),
  });
}
