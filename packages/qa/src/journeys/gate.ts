import { changedPaths, showAt, type Git } from './git.js';
import { parseManifest, parseMap, type ChangeDecl, type JourneyDecl, type JourneyMap, type Manifest } from './manifest.js';
import type { Receipt } from './receipt.js';
import { canonicalJson, matchesAny, normPath, sha256Text } from './util.js';

export type PathClass = 'global' | 'mapped' | 'exempt' | 'unmapped' | 'coverage-file';

export interface ClassifiedPath {
  path: string;
  class: PathClass;
  behaviors: string[];
}

/**
 * Classify one changed path (D8). Precedence: coverage files, global, mapped, exempt, unmapped. A path that is both
 * mapped and exempt is mapped: an exempt glob can never hide product source from a behavior.
 */
export function classifyPath(map: JourneyMap, file: string): ClassifiedPath {
  const p = normPath(file);
  if (p === 'journeys.toml') return { path: p, class: 'global', behaviors: [] };
  if (p === 'journeys-map.toml') return { path: p, class: 'coverage-file', behaviors: [] };
  if (matchesAny(map.global, p)) return { path: p, class: 'global', behaviors: [] };
  const hit = map.behaviors.filter((b) => matchesAny(b.paths, p)).map((b) => b.id);
  if (hit.length > 0) return { path: p, class: 'mapped', behaviors: hit };
  if (map.exempt.some((e) => matchesAny(e.paths, p))) return { path: p, class: 'exempt', behaviors: [] };
  return { path: p, class: 'unmapped', behaviors: [] };
}

export interface CoverageChange {
  id: string;
  kind: ChangeDecl['kind'];
  sha256: string;
  detail: string;
}

const hashChange = (id: string, before: unknown, after: unknown): string => sha256Text(canonicalJson({ id, before, after }));

/** Coverage changes between a baseline and the current manifest and map (D8 coverage-owner validation). */
export function coverageChanges(
  baseM: Manifest | null,
  baseMap: JourneyMap | null,
  nowM: Manifest,
  nowMap: JourneyMap | null,
): CoverageChange[] {
  const out: CoverageChange[] = [];
  const push = (id: string, kind: ChangeDecl['kind'], before: unknown, after: unknown, detail: string): void =>
    void out.push({ id, kind, sha256: hashChange(id, before, after), detail });
  if (baseM) {
    for (const b of baseM.journeys) {
      const n = nowM.journeys.find((j) => j.id === b.id);
      if (!n) {
        push(b.id, 'removed', b, null, `${b.id} was removed`);
        continue;
      }
      if (b.release_critical && !n.release_critical) push(b.id, 'downgraded', b, n, `${b.id} is no longer release_critical`);
      const lost = b.platforms.filter((p) => !n.platforms.includes(p));
      if (lost.length > 0) push(b.id, 'platform-reduced', b, n, `${b.id} lost platform ${lost.join(', ')}`);
      for (const k of ['surface', 'entry', 'data'] as const) {
        if (b[k] !== n[k]) push(b.id, 'weakened', b, n, `${b.id} ${k}: ${b[k]} -> ${n[k]}`);
      }
    }
    if (canonicalJson([...baseM.coverage_owners].sort()) !== canonicalJson([...nowM.coverage_owners].sort())) {
      push('coverage_owners', 'weakened', baseM.coverage_owners, nowM.coverage_owners, 'coverage_owners changed');
    }
  }
  if (baseMap && nowMap) {
    for (const b of baseMap.behaviors) {
      const n = nowMap.behaviors.find((x) => x.id === b.id);
      if (!n) push(b.id, 'map-shrunk', b, null, `behavior ${b.id} was removed`);
      else if (b.paths.some((p) => !n.paths.includes(p)) || b.journeys.some((j) => !n.journeys.includes(j))) {
        push(b.id, 'map-shrunk', b, n, `behavior ${b.id} lost paths or journeys`);
      }
    }
    const baseExempt = new Set(baseMap.exempt.flatMap((e) => e.paths));
    const newExempt = nowMap.exempt.flatMap((e) => e.paths).filter((p) => !baseExempt.has(p));
    if (newExempt.length > 0) push('exempt', 'map-shrunk', [...baseExempt].sort(), nowMap.exempt.flatMap((e) => e.paths).sort(), `new exempt globs: ${newExempt.join(', ')}`);
    if (baseMap.global.some((p) => !nowMap.global.includes(p))) push('global', 'map-shrunk', baseMap.global, nowMap.global, 'global paths were removed');
  } else if (baseMap && !nowMap) {
    push('journeys-map', 'map-shrunk', baseMap, null, 'journeys-map.toml was removed');
  }
  return out;
}

export interface TombstoneFinding {
  change: CoverageChange;
  status: 'ok' | 'missing' | 'hash-mismatch' | 'approval-malformed' | 'unresolved';
  detail: string;
}

const APPROVAL = /^https:\/\/github\.com\/[^/]+\/[^/]+\/issues\/\d+#issuecomment-\d+$/;

export function checkTombstones(changes: CoverageChange[], tombstones: ChangeDecl[]): TombstoneFinding[] {
  return changes.map((c) => {
    const t = tombstones.find((x) => x.id === c.id && x.kind === c.kind);
    if (!t) return { change: c, status: 'missing', detail: `no [[change]] for ${c.id} ${c.kind}` };
    if (t.before_after_sha256 !== c.sha256) return { change: c, status: 'hash-mismatch', detail: `before_after_sha256 should be ${c.sha256}` };
    if (!APPROVAL.test(t.approval)) return { change: c, status: 'approval-malformed', detail: 'approval is not a GitHub issue comment URL' };
    if (!t.replacement && !t.risk_accepted) return { change: c, status: 'unresolved', detail: 'needs replacement or risk_accepted' };
    return { change: c, status: 'ok', detail: 'tombstone present; approval comment author not fetched in this phase' };
  });
}

export type Coverage = 'valid' | 'carried-forward' | 'stale' | 'unknown';
export interface CoverageRow {
  journey: string;
  platform: string;
  status: Coverage;
  detail: string;
}

export interface GateInput {
  root: string;
  git: Git;
  base: string;
  /** App root relative to the repository root, "" when equal. */
  prefix?: string;
  manifest: Manifest;
  map: JourneyMap | null;
  /** Git tree of the commit being claimed; defaults to HEAD's tree. */
  claimedTree?: string;
  /** Receipts that already passed `validateReceipt` (the gate does not trust unvalidated ones). */
  receipts?: Receipt[];
}

export interface GateReport {
  base: string;
  claimedTree: string | null;
  changed: ClassifiedPath[];
  unmapped: string[];
  required: { journey: string; behaviors: string[] }[];
  coverage: CoverageRow[];
  tombstones: TombstoneFinding[];
  findings: string[];
  note: string;
}

/** Advisory gate: report what phase 2 would fail. Never throws on findings and the CLI always exits 0 (phase 1). */
export function runGate(input: GateInput): GateReport {
  const { git, base, manifest, map } = input;
  const pre = input.prefix ? `${normPath(input.prefix).replace(/\/$/, '')}/` : '';
  const findings: string[] = [];
  const paths = changedPaths(git, base);
  const claimedTree = input.claimedTree ?? git.run(['rev-parse', 'HEAD^{tree}'])?.trim() ?? null;
  if (paths === null) findings.push(`cannot diff against ${base}; nothing was classified`);
  const rel = (paths ?? []).filter((p) => !pre || p.startsWith(pre)).map((p) => (pre ? p.slice(pre.length) : p));
  if (!map) findings.push('journeys-map.toml is missing: every changed path is unclassified');
  const changed = map ? rel.map((p) => classifyPath(map, p)) : rel.map((p) => ({ path: p, class: 'unmapped' as PathClass, behaviors: [] }));
  const unmapped = changed.filter((c) => c.class === 'unmapped').map((c) => c.path);
  for (const p of unmapped) findings.push(`unmapped product path ${p}: phase 2 fails until journeys-map.toml covers or exempts it`);

  const touched = new Set<string>();
  const globalTouched = changed.some((c) => c.class === 'global');
  for (const c of changed) for (const b of c.behaviors) touched.add(b);
  const required: GateReport['required'] = [];
  if (map) {
    const byJourney = new Map<string, Set<string>>();
    for (const b of map.behaviors) {
      if (!globalTouched && !touched.has(b.id)) continue;
      for (const j of b.journeys) byJourney.set(j, new Set([...(byJourney.get(j) ?? []), b.id]));
    }
    for (const [journey, behaviors] of byJourney) required.push({ journey, behaviors: [...behaviors].sort() });
  }

  const coverage: CoverageRow[] = [];
  for (const req of required) {
    const decl = manifest.journeys.find((j) => j.id === req.journey);
    if (!decl) {
      findings.push(`required journey ${req.journey} is not in journeys.toml`);
      continue;
    }
    for (const platform of decl.platforms) coverage.push(coverageFor(input, decl, platform, claimedTree, map));
  }
  for (const c of coverage) {
    if (c.status === 'stale' || c.status === 'unknown') findings.push(`journey ${c.journey} on ${c.platform} is ${c.status}: ${c.detail}`);
  }

  const baseM = parseManifest(showAt(git, base, `${pre}journeys.toml`) ?? '').value;
  const baseMap = parseMap(showAt(git, base, `${pre}journeys-map.toml`) ?? '').value;
  const tombstones = checkTombstones(coverageChanges(baseM, baseMap, manifest, map), manifest.changes);
  for (const t of tombstones) if (t.status !== 'ok') findings.push(`coverage change ${t.change.id} ${t.change.kind}: ${t.detail} (${t.change.detail})`);

  return {
    base,
    claimedTree,
    changed,
    unmapped,
    required,
    coverage,
    tombstones,
    findings,
    note: 'advisory: phase 1 always exits 0; findings are what phase 2 would fail',
  };
}

function coverageFor(input: GateInput, decl: JourneyDecl, platform: string, claimedTree: string | null, map: JourneyMap | null): CoverageRow {
  const row = (status: Coverage, detail: string): CoverageRow => ({ journey: decl.id, platform, status, detail });
  const mine = (input.receipts ?? []).filter(
    (r) => r.selection.platform === platform && r.journeys.some((j) => j.id === decl.id && j.status === 'passed'),
  );
  if (mine.length === 0) return row('unknown', 'no validated receipt for this platform');
  if (!claimedTree) return row('unknown', 'claimed tree unknown');
  if (mine.some((r) => r.source.tree === claimedTree)) return row('valid', 'receipt for the claimed tree');
  const adapter = decl.adapter ? input.manifest.adapters.find((a) => a.name === decl.adapter) : undefined;
  if (adapter && adapter.sources.length === 0) return row('stale', 'adapter declares no sources, so carry-forward cannot be proven');
  const guarded = [
    ...(map?.behaviors.filter((b) => b.journeys.includes(decl.id)).flatMap((b) => b.paths) ?? []),
    ...(map?.global ?? []),
    'journeys.toml',
    decl.entry,
    ...(adapter?.sources ?? []),
  ];
  for (const r of mine) {
    const diff = changedPaths(input.git, r.source.tree, claimedTree);
    if (diff === null) continue;
    const pre = input.prefix ? `${normPath(input.prefix).replace(/\/$/, '')}/` : '';
    const rel = diff.filter((p) => !pre || p.startsWith(pre)).map((p) => (pre ? p.slice(pre.length) : p));
    const hit = rel.find((p) => matchesAny(guarded, p));
    if (!hit) return row('carried-forward', `receipt tree ${r.source.tree.slice(0, 12)} has no changes to this journey's paths`);
  }
  return row('stale', 'a mapped, global, entry or adapter path changed since every receipt');
}

export function formatGate(r: GateReport): string {
  const lines = [`gate (advisory) base=${r.base} tree=${r.claimedTree ?? 'unknown'}`];
  const count = (c: PathClass): number => r.changed.filter((x) => x.class === c).length;
  lines.push(`changed paths: ${r.changed.length} (mapped ${count('mapped')}, exempt ${count('exempt')}, unmapped ${count('unmapped')}, global ${count('global')})`);
  for (const q of r.required) lines.push(`requires ${q.journey} (${q.behaviors.join(', ')})`);
  for (const c of r.coverage) lines.push(`  ${c.journey} ${c.platform}: ${c.status} - ${c.detail}`);
  if (r.findings.length === 0) lines.push('findings: none');
  else for (const f of r.findings) lines.push(`would fail in phase 2: ${f}`);
  lines.push(r.note);
  return lines.join('\n');
}
