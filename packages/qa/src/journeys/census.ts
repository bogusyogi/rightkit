import fs from 'node:fs';
import path from 'node:path';
import { changedPaths, showAt, type Git } from './git.js';
import { parseManifest } from './manifest.js';
import { normPath } from './util.js';

export type TestClass = 'pure' | 'contract' | 'pin';
export interface CensusCounts {
  tests: number;
  tagged: number;
  untagged: number;
  invalidTag: number;
  byClass: Record<TestClass, number>;
}
export interface CensusReport extends CensusCounts {
  journeys: number;
  /** Tests per journey; null when there are no journeys. */
  ratio: number | null;
  files: number;
}
export interface CensusDiff {
  base: string;
  added: CensusCounts;
  removed: CensusCounts;
  journeysBefore: number | null;
  journeysAfter: number;
  changedFiles: number;
}

const SKIP_DIRS = new Set(['node_modules', '.git', 'target', 'dist', 'build', 'rk-snapshot', '.next', 'coverage']);
const RUST_TEST = /^\s*#\[(?:[A-Za-z0-9_]+::)*test(?:\(.*\))?\]/;
const JS_TEST = /^\s*(?:it|test)(?:\.(?:only|skip|concurrent|todo|fails))?(?:\.each\b[^(]*)?\s*\(/;
const TAG = /rk-test:\s*(.*)$/;
const VALID_TAG = /^(pure|contract|pin\s+#\d+)\b/;

export const isRustFile = (f: string): boolean => f.endsWith('.rs');
export const isJsFile = (f: string): boolean => /\.(?:[cm]?[jt]sx?)$/.test(f) && !f.endsWith('.d.ts');

function emptyCounts(): CensusCounts {
  return { tests: 0, tagged: 0, untagged: 0, invalidTag: 0, byClass: { pure: 0, contract: 0, pin: 0 } };
}

function classify(tag: string): TestClass | null {
  const m = VALID_TAG.exec(tag.trim());
  return m ? (m[1]!.split(/\s/)[0] as TestClass) : null;
}

/** Count tests in one file's text. A tag is `// rk-test: <class>` on the test's line or on the lines directly above it
 * (blank lines and other attributes may sit between). */
export function countTests(text: string, kind: 'rust' | 'js'): CensusCounts {
  const counts = emptyCounts();
  const lines = text.split('\n');
  const isTest = kind === 'rust' ? RUST_TEST : JS_TEST;
  for (const [i, line] of lines.entries()) {
    if (!isTest.test(line)) continue;
    counts.tests++;
    let tag = TAG.exec(line)?.[1] ?? null;
    for (let k = i - 1; tag === null && k >= 0 && k >= i - 4; k--) {
      const prev = lines[k]!.trim();
      if (prev === '' || (kind === 'rust' && prev.startsWith('#['))) continue;
      const m = TAG.exec(prev);
      if (m && /^(\/\/|\/\*|\*)/.test(prev)) tag = m[1]!;
      break;
    }
    if (tag === null) counts.untagged++;
    else {
      const cls = classify(tag);
      if (cls) {
        counts.tagged++;
        counts.byClass[cls]++;
      } else {
        counts.invalidTag++;
        counts.untagged++;
      }
    }
  }
  return counts;
}

function add(a: CensusCounts, b: CensusCounts): void {
  a.tests += b.tests;
  a.tagged += b.tagged;
  a.untagged += b.untagged;
  a.invalidTag += b.invalidTag;
  for (const k of ['pure', 'contract', 'pin'] as const) a.byClass[k] += b.byClass[k];
}

function* walk(root: string, rel = ''): Generator<string> {
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(path.join(root, rel), { withFileTypes: true });
  } catch {
    return;
  }
  for (const e of entries) {
    if (e.isDirectory()) {
      if (!SKIP_DIRS.has(e.name)) yield* walk(root, rel ? `${rel}/${e.name}` : e.name);
    } else if (e.isFile()) {
      const r = rel ? `${rel}/${e.name}` : e.name;
      if (isRustFile(r) || isJsFile(r)) yield r;
    }
  }
}

function journeyCount(text: string | null): number | null {
  if (text === null) return null;
  const m = parseManifest(text);
  return m.value ? m.value.journeys.length : null;
}

/** Census of the working tree under `root`. Report-only: it never classifies a change as user-visible. */
export function census(root: string): CensusReport {
  const total = emptyCounts();
  let files = 0;
  for (const f of walk(root)) {
    let text: string;
    try {
      text = fs.readFileSync(path.join(root, f), 'utf8');
    } catch {
      continue;
    }
    const c = countTests(text, isRustFile(f) ? 'rust' : 'js');
    if (c.tests > 0) files++;
    add(total, c);
  }
  let manifestText: string | null = null;
  try {
    manifestText = fs.readFileSync(path.join(root, 'journeys.toml'), 'utf8');
  } catch {
    /* no manifest: zero journeys */
  }
  const journeys = journeyCount(manifestText) ?? 0;
  return { ...total, journeys, ratio: journeys === 0 ? null : Number((total.tests / journeys).toFixed(2)), files };
}

/** Diff test counts against `base` through git: only files that differ are read at both revisions. `prefix` is the
 * app root relative to the repository root ("" when they are the same). */
export function censusDiff(root: string, git: Git, base: string, prefix = ''): CensusDiff | null {
  const paths = changedPaths(git, base);
  if (paths === null) return null;
  const added = emptyCounts();
  const removed = emptyCounts();
  let changed = 0;
  const pre = prefix ? `${normPath(prefix).replace(/\/$/, '')}/` : '';
  for (const p of paths) {
    if (pre && !p.startsWith(pre)) continue;
    const rel = pre ? p.slice(pre.length) : p;
    if (rel.split('/').some((s) => SKIP_DIRS.has(s))) continue;
    if (!isRustFile(rel) && !isJsFile(rel)) continue;
    changed++;
    const kind = isRustFile(rel) ? 'rust' : 'js';
    const before = countTests(showAt(git, base, p) ?? '', kind);
    let nowText = '';
    try {
      nowText = fs.readFileSync(path.join(root, rel), 'utf8');
    } catch {
      /* deleted */
    }
    const after = countTests(nowText, kind);
    const grow = (a: CensusCounts, b: CensusCounts, into: CensusCounts): void => {
      const d = (x: number, y: number): number => Math.max(0, x - y);
      into.tests += d(a.tests, b.tests);
      into.tagged += d(a.tagged, b.tagged);
      into.untagged += d(a.untagged, b.untagged);
      into.invalidTag += d(a.invalidTag, b.invalidTag);
      for (const k of ['pure', 'contract', 'pin'] as const) into.byClass[k] += d(a.byClass[k], b.byClass[k]);
    };
    grow(after, before, added);
    grow(before, after, removed);
  }
  const jf = pre + 'journeys.toml';
  let nowManifest: string | null = null;
  try {
    nowManifest = fs.readFileSync(path.join(root, 'journeys.toml'), 'utf8');
  } catch {
    /* none */
  }
  return {
    base,
    added,
    removed,
    journeysBefore: journeyCount(showAt(git, base, jf)),
    journeysAfter: journeyCount(nowManifest) ?? 0,
    changedFiles: changed,
  };
}

export function formatCensus(r: CensusReport, d: CensusDiff | null): string {
  const lines = [
    `tests: ${r.tests} in ${r.files} files (tagged ${r.tagged}: pure ${r.byClass.pure}, contract ${r.byClass.contract}, pin ${r.byClass.pin}; untagged ${r.untagged}, of which malformed tag ${r.invalidTag})`,
    `journeys: ${r.journeys}`,
    `tests per journey: ${r.ratio === null ? 'n/a (no journeys)' : r.ratio}`,
  ];
  if (d) {
    lines.push(
      `vs ${d.base}: +${d.added.tests} tests (+${d.added.untagged} untagged), -${d.removed.tests} tests; journeys ${d.journeysBefore ?? 'none'} -> ${d.journeysAfter}`,
    );
  }
  lines.push('report only; no change is classified or blocked');
  return lines.join('\n');
}
