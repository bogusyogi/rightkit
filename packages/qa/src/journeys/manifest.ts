import fs from 'node:fs';
import path from 'node:path';
import { parseToml } from './toml.js';
import { globToRegExp, isObj, isStr, normPath, problem, type Obj, type Problem } from './util.js';

export const PLATFORMS = ['macos', 'windows', 'linux'] as const;
export const SURFACES = ['native-ui', 'web', 'engine', 'cli', 'installed'] as const;
export const RUNNERS = ['scenario', 'adapter'] as const;
export const TIERS = ['fast', 'full', 'paid'] as const;
export const DATA_KINDS = ['generated', 'seeded'] as const;
export const CHANGE_KINDS = ['removed', 'downgraded', 'platform-reduced', 'weakened', 'map-shrunk'] as const;

export type Platform = (typeof PLATFORMS)[number];
export type Surface = (typeof SURFACES)[number];
export type Tier = (typeof TIERS)[number];

export interface AdapterArtifact {
  role: string;
  cargo_package?: string;
  cargo_target?: string;
  path?: string;
}

export interface AdapterDecl {
  name: string;
  build: string[];
  executable: AdapterArtifact;
  artifacts: AdapterArtifact[];
  args: string[];
  /** Optional globs of the adapter's own source; a carried-forward receipt is refused without them (D8). */
  sources: string[];
  /** True when the adapter declares a dev server or mocked IPC; never valid proof for native-ui. */
  mocked: boolean;
}

export interface JourneyDecl {
  id: string;
  title: string;
  platforms: Platform[];
  surface: Surface;
  runner: (typeof RUNNERS)[number];
  adapter: string | null;
  entry: string;
  tier: Tier;
  data: (typeof DATA_KINDS)[number];
  release_critical: boolean;
}

export interface ChangeDecl {
  id: string;
  kind: (typeof CHANGE_KINDS)[number];
  before_after_sha256: string;
  approval: string;
  replacement: string | null;
  risk_accepted: boolean;
}

export interface Manifest {
  version: 1;
  app: string;
  coverage_owners: string[];
  adapters: AdapterDecl[];
  journeys: JourneyDecl[];
  changes: ChangeDecl[];
}

export interface BehaviorDecl {
  id: string;
  title: string;
  paths: string[];
  journeys: string[];
}

export interface JourneyMap {
  version: 1;
  behaviors: BehaviorDecl[];
  exempt: { paths: string[]; reason: string }[];
  global: string[];
}

const arr = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
const strArr = (v: unknown): string[] | null =>
  Array.isArray(v) && v.every((x) => typeof x === 'string') ? (v as string[]) : null;

export interface ParseResult<T> {
  value: T | null;
  problems: Problem[];
}

function readArtifact(raw: unknown, where: string, out: Problem[]): AdapterArtifact | null {
  if (!isObj(raw) || !isStr(raw.role)) {
    out.push(problem('schema', null, `${where}: needs a role`));
    return null;
  }
  const has = [raw.cargo_package, raw.cargo_target, raw.path].filter((x) => x !== undefined).length;
  if (has === 0) {
    out.push(problem('schema', null, `${where}: needs cargo_package, cargo_target or path`));
    return null;
  }
  for (const k of ['cargo_package', 'cargo_target', 'path'] as const) {
    if (raw[k] !== undefined && !isStr(raw[k])) out.push(problem('schema', null, `${where}: ${k} must be a string`));
  }
  return {
    role: raw.role,
    ...(isStr(raw.cargo_package) ? { cargo_package: raw.cargo_package } : {}),
    ...(isStr(raw.cargo_target) ? { cargo_target: raw.cargo_target } : {}),
    ...(isStr(raw.path) ? { path: raw.path } : {}),
  };
}

/** Parse and shape-check `journeys.toml`. Structure only; file existence and cross-references are `validateManifest`. */
export function parseManifest(text: string): ParseResult<Manifest> {
  const problems: Problem[] = [];
  let raw: Obj;
  try {
    raw = parseToml(text);
  } catch (e) {
    return { value: null, problems: [problem('toml', null, (e as Error).message)] };
  }
  if (raw.version !== 1) problems.push(problem('schema', null, 'version must be 1'));
  if (!isStr(raw.app)) problems.push(problem('schema', null, 'app must be a non-empty string'));
  const owners = raw.coverage_owners === undefined ? [] : strArr(raw.coverage_owners);
  if (owners === null) problems.push(problem('schema', null, 'coverage_owners must be an array of strings'));

  const adapters: AdapterDecl[] = [];
  for (const [i, a] of arr(raw.adapter).entries()) {
    const where = `adapter[${i}]`;
    if (!isObj(a) || !isStr(a.name)) {
      problems.push(problem('schema', null, `${where}: needs a name`));
      continue;
    }
    const exe = readArtifact(a.executable, `${where}.executable`, problems);
    const build = a.build === undefined ? [] : strArr(a.build);
    const args = a.args === undefined ? [] : strArr(a.args);
    const sources = a.sources === undefined ? [] : strArr(a.sources);
    if (build === null) problems.push(problem('schema', null, `${where}: build must be an array of strings`));
    if (args === null) problems.push(problem('schema', null, `${where}: args must be an array of strings`));
    if (sources === null) problems.push(problem('schema', null, `${where}: sources must be an array of strings`));
    const artifacts: AdapterArtifact[] = [];
    for (const [j, x] of arr(a.artifacts).entries()) {
      const art = readArtifact(x, `${where}.artifacts[${j}]`, problems);
      if (art) artifacts.push(art);
    }
    if (exe && build !== null && args !== null && sources !== null) {
      const mocked = ['dev_server', 'mocked_ipc', 'mock_ipc'].some((k) => a[k] !== undefined && a[k] !== false);
      adapters.push({ name: a.name, build, executable: exe, artifacts, args, sources, mocked });
    }
  }

  const journeys: JourneyDecl[] = [];
  for (const [i, j] of arr(raw.journey).entries()) {
    const where = `journey[${i}]`;
    if (!isObj(j)) {
      problems.push(problem('schema', null, `${where}: not a table`));
      continue;
    }
    const id = isStr(j.id) ? j.id : null;
    const bad = (m: string): void => void problems.push(problem('schema', id, `${id ?? where}: ${m}`));
    if (!id) bad('id must be a non-empty string');
    else if (!/^J-\d{3,}$/.test(id)) bad('id must look like J-001');
    if (!isStr(j.title)) bad('title required');
    const platforms = strArr(j.platforms);
    if (!platforms || platforms.length === 0 || platforms.some((p) => !(PLATFORMS as readonly string[]).includes(p))) {
      bad(`platforms must be a non-empty subset of ${PLATFORMS.join('|')}`);
    }
    if (!(SURFACES as readonly string[]).includes(j.surface as string)) bad(`surface must be ${SURFACES.join('|')}`);
    const runner = j.runner === undefined ? 'scenario' : j.runner;
    if (!(RUNNERS as readonly string[]).includes(runner as string)) bad(`runner must be ${RUNNERS.join('|')}`);
    if (!isStr(j.entry)) bad('entry required');
    if (!(TIERS as readonly string[]).includes(j.tier as string)) bad(`tier must be ${TIERS.join('|')}`);
    if (j.data === 'real') problems.push(problem('real_data', id, `${id ?? where}: data = "real" is refused`));
    else if (!(DATA_KINDS as readonly string[]).includes(j.data as string)) bad(`data must be ${DATA_KINDS.join('|')}`);
    if (typeof j.release_critical !== 'boolean') bad('release_critical must be a boolean');
    if (j.adapter !== undefined && !isStr(j.adapter)) bad('adapter must be a string');
    if (id && platforms && isStr(j.entry) && isStr(j.title)) {
      journeys.push({
        id,
        title: j.title,
        platforms: platforms as Platform[],
        surface: j.surface as Surface,
        runner: runner as JourneyDecl['runner'],
        adapter: isStr(j.adapter) ? j.adapter : null,
        entry: j.entry,
        tier: j.tier as Tier,
        data: j.data as JourneyDecl['data'],
        release_critical: j.release_critical === true,
      });
    }
  }

  const changes: ChangeDecl[] = [];
  for (const [i, c] of arr(raw.change).entries()) {
    const where = `change[${i}]`;
    if (
      !isObj(c) ||
      !isStr(c.id) ||
      !(CHANGE_KINDS as readonly string[]).includes(c.kind as string) ||
      !isStr(c.before_after_sha256) ||
      !isStr(c.approval)
    ) {
      problems.push(problem('schema', null, `${where}: needs id, kind (${CHANGE_KINDS.join('|')}), before_after_sha256, approval`));
      continue;
    }
    changes.push({
      id: c.id,
      kind: c.kind as ChangeDecl['kind'],
      before_after_sha256: c.before_after_sha256,
      approval: c.approval,
      replacement: isStr(c.replacement) ? c.replacement : null,
      risk_accepted: c.risk_accepted === true,
    });
  }

  if (problems.length > 0 || owners === null) return { value: null, problems };
  return {
    value: { version: 1, app: raw.app as string, coverage_owners: owners, adapters, journeys, changes },
    problems,
  };
}

export interface ValidateOptions {
  /** App root that `entry` paths and artifact paths resolve against. */
  root: string;
  /** Existence probe; defaults to the filesystem. */
  exists?: (absolute: string) => boolean;
  /** Glob probe: does anything match? Defaults to a filesystem walk. */
  anyMatch?: (root: string, glob: string) => boolean;
}

function walkMatches(root: string, glob: string): boolean {
  const re = globToRegExp(normPath(glob));
  const stack = [''];
  let visited = 0;
  while (stack.length > 0 && visited < 50_000) {
    const rel = stack.pop()!;
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(path.join(root, rel), { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      if (e.name === 'node_modules' || e.name === '.git' || e.name === 'target') continue;
      const r = rel ? `${rel}/${e.name}` : e.name;
      visited++;
      if (re.test(r)) return true;
      if (e.isDirectory()) stack.push(r);
    }
  }
  return false;
}

const MOCK_MARKERS = /\b(vite|mock|mocked|msw|dev-server|devserver)\b/i;

/** Manifest-level validation: unique immutable ids, entries exist, no real data, adapters resolve, native-ui binds a built binary. */
export function validateManifest(m: Manifest, opts: ValidateOptions): Problem[] {
  const out: Problem[] = [];
  const exists = opts.exists ?? ((p: string) => fs.existsSync(p));
  const anyMatch = opts.anyMatch ?? walkMatches;
  const seen = new Set<string>();
  const adapterNames = new Set<string>();
  for (const a of m.adapters) {
    if (adapterNames.has(a.name)) out.push(problem('duplicate_adapter', null, `adapter ${a.name} is declared twice`));
    adapterNames.add(a.name);
    if (a.executable.cargo_target && a.build.length === 0) {
      out.push(problem('adapter_build', null, `adapter ${a.name}: cargo_target needs a build argv`));
    }
    if (a.executable.path && !exists(path.resolve(opts.root, a.executable.path)) && a.build.length === 0) {
      out.push(problem('adapter_executable', null, `adapter ${a.name}: executable ${a.executable.path} does not exist`));
    }
  }
  for (const j of m.journeys) {
    if (seen.has(j.id)) out.push(problem('duplicate_id', j.id, `${j.id} is declared twice (ids are immutable and never reused)`));
    seen.add(j.id);
    const adapter = j.adapter ? m.adapters.find((a) => a.name === j.adapter) : undefined;
    if (j.runner === 'adapter') {
      if (!j.adapter) out.push(problem('adapter_missing', j.id, `${j.id}: runner = "adapter" needs adapter`));
      else if (!adapter) out.push(problem('adapter_unresolved', j.id, `${j.id}: adapter ${j.adapter} is not declared`));
    } else if (j.adapter) {
      out.push(problem('adapter_unexpected', j.id, `${j.id}: adapter is only for runner = "adapter"`));
    }
    const hasGlob = /[*?]/.test(j.entry);
    const entryOk = hasGlob ? anyMatch(opts.root, j.entry) : exists(path.resolve(opts.root, j.entry));
    if (!entryOk) out.push(problem('entry_missing', j.id, `${j.id}: entry ${j.entry} does not exist`));
    if (j.surface === 'native-ui') {
      const launches = adapter ? [adapter.executable, ...adapter.artifacts] : [];
      const binary = launches.some((x) => x.role === 'app' || x.role === 'engine' || x.role === 'binary');
      if (j.runner !== 'adapter' || !adapter || !binary) {
        out.push(
          problem(
            'native_ui_no_binary',
            j.id,
            `${j.id}: a native-ui journey must launch the built app binary: runner = "adapter" with an adapter that declares an artifact whose role is app, engine or binary`,
          ),
        );
      } else if (adapter.mocked || [...adapter.build, ...adapter.args].some((a) => MOCK_MARKERS.test(a))) {
        out.push(problem('native_ui_mocked', j.id, `${j.id}: native-ui must not use a dev server or mocked IPC; declare it as surface "web"`));
      }
    }
  }
  for (const c of m.changes) {
    if (!c.replacement && !c.risk_accepted) {
      out.push(problem('change_unresolved', c.id, `change ${c.id}: needs a replacement journey or risk_accepted = true`));
    }
    if (c.replacement && !seen.has(c.replacement)) {
      out.push(problem('change_replacement', c.id, `change ${c.id}: replacement ${c.replacement} is not a declared journey`));
    }
  }
  return out;
}

export function parseMap(text: string): ParseResult<JourneyMap> {
  const problems: Problem[] = [];
  let raw: Obj;
  try {
    raw = parseToml(text);
  } catch (e) {
    return { value: null, problems: [problem('toml', null, (e as Error).message)] };
  }
  if (raw.version !== 1) problems.push(problem('schema', null, 'map: version must be 1'));
  const behaviors: BehaviorDecl[] = [];
  for (const [i, b] of arr(raw.behavior).entries()) {
    const paths = isObj(b) ? strArr(b.paths) : null;
    const js = isObj(b) ? strArr(b.journeys) : null;
    if (!isObj(b) || !isStr(b.id) || !isStr(b.title) || !paths || paths.length === 0 || !js) {
      problems.push(problem('schema', null, `map: behavior[${i}] needs id, title, paths, journeys`));
      continue;
    }
    behaviors.push({ id: b.id, title: b.title, paths, journeys: js });
  }
  const exempt: JourneyMap['exempt'] = [];
  for (const [i, e] of arr(raw.exempt).entries()) {
    const paths = isObj(e) ? strArr(e.paths) : null;
    if (!isObj(e) || !paths || !isStr(e.reason)) {
      problems.push(problem('schema', null, `map: exempt[${i}] needs paths and reason`));
      continue;
    }
    exempt.push({ paths, reason: e.reason });
  }
  let globalPaths: string[] = [];
  if (raw.global !== undefined) {
    const g = isObj(raw.global) ? strArr(raw.global.paths) : null;
    if (!g) problems.push(problem('schema', null, 'map: global.paths must be an array of strings'));
    else globalPaths = g;
  }
  if (problems.length > 0) return { value: null, problems };
  return { value: { version: 1, behaviors, exempt, global: globalPaths }, problems };
}

export function validateMap(map: JourneyMap, manifest: Manifest): Problem[] {
  const out: Problem[] = [];
  const ids = new Set(manifest.journeys.map((j) => j.id));
  const seen = new Set<string>();
  for (const b of map.behaviors) {
    if (seen.has(b.id)) out.push(problem('duplicate_id', b.id, `behavior ${b.id} is declared twice`));
    seen.add(b.id);
    if (b.journeys.length === 0) out.push(problem('behavior_uncovered', b.id, `behavior ${b.id} lists no journey`));
    for (const j of b.journeys) {
      if (!ids.has(j)) out.push(problem('behavior_journey_unknown', b.id, `behavior ${b.id}: journey ${j} is not in journeys.toml`));
    }
  }
  return out;
}

export interface LoadedManifest {
  manifest: Manifest | null;
  map: JourneyMap | null;
  problems: Problem[];
  manifestText: string | null;
  mapText: string | null;
}

/** `journeys check`: read journeys.toml (required) and journeys-map.toml (when present) from the app root. */
export function checkRoot(root: string, opts: Partial<ValidateOptions> = {}): LoadedManifest {
  const out: LoadedManifest = { manifest: null, map: null, problems: [], manifestText: null, mapText: null };
  const mp = path.join(root, 'journeys.toml');
  try {
    out.manifestText = fs.readFileSync(mp, 'utf8');
  } catch {
    out.problems.push(problem('manifest_missing', null, `${mp} not found`));
    return out;
  }
  const parsed = parseManifest(out.manifestText);
  out.problems.push(...parsed.problems);
  out.manifest = parsed.value;
  if (parsed.value) out.problems.push(...validateManifest(parsed.value, { root, ...opts }));
  const mapPath = path.join(root, 'journeys-map.toml');
  if (fs.existsSync(mapPath)) {
    out.mapText = fs.readFileSync(mapPath, 'utf8');
    const pm = parseMap(out.mapText);
    out.problems.push(...pm.problems);
    out.map = pm.value;
    if (pm.value && parsed.value) out.problems.push(...validateMap(pm.value, parsed.value));
  }
  return out;
}
