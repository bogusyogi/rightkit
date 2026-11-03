import fs from 'node:fs';
import path from 'node:path';
import { parseToml } from './toml.js';
import { globToRegExp, isObj, isStr, normPath, problem, type Obj, type Problem } from './util.js';

export const PLATFORMS = ['macos', 'windows', 'linux'] as const;
export const SURFACES = ['native-ui', 'web', 'engine', 'cli', 'installed'] as const;
export const RUNNERS = ['scenario', 'adapter'] as const;
export const TIERS = ['fast', 'full', 'paid'] as const;
/** Every journey runs on the owner's real data under a snapshot and restore; the key is optional and may only say `real`. */
export const DATA_KINDS = ['real'] as const;
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
  data: (typeof DATA_KINDS)[number] | null;
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

/** One binary the `[build]` section produces (`qa_marker` default true: the runner launches and marks it). */
export interface BuildArtifact {
  role: string;
  cargo_target?: string;
  cargo_package?: string;
  path?: string;
  qa_marker: boolean;
}

/** `[build]`: the app's ONE QA build (ADR-0001 D18); every `runner = "scenario"` journey runs against it. */
export interface BuildDecl {
  command: string[];
  features: string[];
  artifacts: BuildArtifact[];
}

/** `[tests]`: the frozen untagged-test count that may only fall. */
export interface TestsDecl { untagged_baseline: number }

/** Per-kind budgets in milliseconds; an absent field is not enforced. */
export interface KindBudget { step_ms?: number; journey_ms?: number; suite_ms?: number }

/** `[budget]`: `warn_until` is the last day (YYYY-MM-DD, inclusive) on which exceeding a budget only warns. */
export interface BudgetDecl { warn_until: string; fast?: KindBudget; full?: KindBudget; paid?: KindBudget }

/** `[[bridge]]`: a bridged app's already-built libtest binary, refused after `removal_date`. */
export interface BridgeDecl {
  app: string;
  binary: string;
  sources: string[];
  tests: string[];
  ignored_required: string[];
  removal_date: string;
}

/** `[plugin]`: a `[build]` artifact role started per scenario by the runner. */
export interface PluginDecl { role: string; args: string[]; hello_timeout_ms?: number }

export interface Manifest {
  version: 1;
  app: string;
  coverage_owners: string[];
  adapters: AdapterDecl[];
  journeys: JourneyDecl[];
  changes: ChangeDecl[];
  build?: BuildDecl | null;
  budget?: BudgetDecl | null;
  tests?: TestsDecl | null;
  bridge?: BridgeDecl[];
  plugin?: PluginDecl | null;
}

/**
 * Journey and behavior ids: a letter or digit, then letters, digits, '.', '_' or '-'. This is the rule
 * `journeys/manifest.rs` (`id_ok`) applies, so both readers accept the same ids (J-001, smoke.launch, ...).
 */
export const isIdOk = (id: string): boolean => /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(id);

const ROLE_NAME = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,63}$/;
/** Roles the runner launches itself; a `[plugin]` may never take one of them (N3). */
const RUNNER_LAUNCHED_ROLES = ['engine', 'app', 'ui'];
const KIND_TABLES = ['fast', 'full', 'paid'] as const;
const DAY = /^(\d{4})-(\d{2})-(\d{2})$/;

/** A real calendar day, `YYYY-MM-DD`. */
export function isRealDay(value: string): boolean {
  const m = DAY.exec(value);
  if (!m) return false;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const date = new Date(Date.UTC(y, mo - 1, d));
  return date.getUTCFullYear() === y && date.getUTCMonth() === mo - 1 && date.getUTCDate() === d;
}

const testNameOk = (name: string): boolean => name.length > 0 && name.trim() === name && !name.startsWith('-') && !name.includes('\0');

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

function unknownKeys(table: Obj, allowed: readonly string[], where: string, out: Problem[]): void {
  for (const key of Object.keys(table)) if (!allowed.includes(key)) out.push(problem('schema', null, `${where}: unknown key ${key}`));
}

function readBuild(raw: unknown, out: Problem[]): BuildDecl | null {
  if (!isObj(raw)) { out.push(problem('schema', null, '[build] must be a table')); return null; }
  unknownKeys(raw, ['command', 'features', 'artifacts'], '[build]', out);
  const command = raw.command === undefined ? [] : strArr(raw.command);
  const features = raw.features === undefined ? [] : strArr(raw.features);
  if (!command) out.push(problem('schema', null, '[build] command must be an array of strings'));
  if (!features) out.push(problem('schema', null, '[build] features must be an array of strings'));
  const artifacts: BuildArtifact[] = [];
  for (const [i, x] of arr(raw.artifacts).entries()) {
    const where = `[build] artifacts[${i}]`;
    if (!isObj(x) || !isStr(x.role)) { out.push(problem('schema', null, `${where}: needs a role`)); continue; }
    unknownKeys(x, ['role', 'cargo_target', 'cargo_package', 'path', 'qa_marker'], where, out);
    for (const k of ['cargo_package', 'cargo_target', 'path'] as const) {
      if (x[k] !== undefined && !isStr(x[k])) out.push(problem('schema', null, `${where}: ${k} must be a string`));
    }
    if (x.qa_marker !== undefined && typeof x.qa_marker !== 'boolean') out.push(problem('schema', null, `${where}: qa_marker must be a boolean`));
    artifacts.push({
      role: x.role,
      ...(isStr(x.cargo_target) ? { cargo_target: x.cargo_target } : {}),
      ...(isStr(x.cargo_package) ? { cargo_package: x.cargo_package } : {}),
      ...(isStr(x.path) ? { path: x.path } : {}),
      qa_marker: x.qa_marker !== false,
    });
  }
  if (!command || !features) return null;
  return { command, features, artifacts };
}

function readBudget(raw: unknown, out: Problem[]): BudgetDecl | null {
  if (!isObj(raw)) { out.push(problem('schema', null, '[budget] must be a table')); return null; }
  unknownKeys(raw, ['warn_until', ...KIND_TABLES], '[budget]', out);
  if (!isStr(raw.warn_until)) { out.push(problem('schema', null, '[budget] warn_until is required (YYYY-MM-DD)')); return null; }
  const decl: BudgetDecl = { warn_until: raw.warn_until };
  for (const kind of KIND_TABLES) {
    const table = raw[kind];
    if (table === undefined) continue;
    if (!isObj(table)) { out.push(problem('schema', null, `[budget.${kind}] must be a table`)); continue; }
    unknownKeys(table, ['step_ms', 'journey_ms', 'suite_ms'], `[budget.${kind}]`, out);
    const kb: KindBudget = {};
    for (const field of ['step_ms', 'journey_ms', 'suite_ms'] as const) {
      const value = table[field];
      if (value === undefined) continue;
      if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) out.push(problem('schema', null, `[budget.${kind}] ${field} must be a whole number of milliseconds`));
      else kb[field] = value;
    }
    decl[kind] = kb;
  }
  return decl;
}

function readTests(raw: unknown, out: Problem[]): TestsDecl | null {
  if (!isObj(raw)) { out.push(problem('schema', null, '[tests] must be a table')); return null; }
  unknownKeys(raw, ['untagged_baseline'], '[tests]', out);
  const n = raw.untagged_baseline;
  if (typeof n !== 'number' || !Number.isInteger(n) || n < 0) { out.push(problem('schema', null, '[tests] untagged_baseline must be a whole number')); return null; }
  return { untagged_baseline: n };
}

function readBridges(raw: unknown, out: Problem[]): BridgeDecl[] {
  const bridges: BridgeDecl[] = [];
  for (const [i, b] of arr(raw).entries()) {
    const where = `[[bridge]][${i}]`;
    if (!isObj(b)) { out.push(problem('schema', null, `${where}: not a table`)); continue; }
    unknownKeys(b, ['app', 'binary', 'sources', 'tests', 'ignored_required', 'removal_date'], where, out);
    const sources = b.sources === undefined ? [] : strArr(b.sources);
    const tests = b.tests === undefined ? [] : strArr(b.tests);
    const ignored = b.ignored_required === undefined ? [] : strArr(b.ignored_required);
    if (typeof b.app !== 'string' || typeof b.binary !== 'string' || typeof b.removal_date !== 'string') {
      out.push(problem('schema', null, `${where}: needs app, binary and removal_date as strings`));
      continue;
    }
    if (!sources || !tests || !ignored) { out.push(problem('schema', null, `${where}: sources, tests and ignored_required must be arrays of strings`)); continue; }
    bridges.push({ app: b.app, binary: b.binary, sources, tests, ignored_required: ignored, removal_date: b.removal_date });
  }
  return bridges;
}

function readPlugin(raw: unknown, out: Problem[]): PluginDecl | null {
  if (!isObj(raw) || !isStr(raw.role)) { out.push(problem('schema', null, '[plugin] needs a role')); return null; }
  unknownKeys(raw, ['role', 'args', 'hello_timeout_ms'], '[plugin]', out);
  const args = raw.args === undefined ? [] : strArr(raw.args);
  if (!args) { out.push(problem('schema', null, '[plugin] args must be an array of strings')); return null; }
  if (raw.hello_timeout_ms !== undefined && (typeof raw.hello_timeout_ms !== 'number' || !Number.isInteger(raw.hello_timeout_ms))) {
    out.push(problem('schema', null, '[plugin] hello_timeout_ms must be a whole number'));
    return null;
  }
  return { role: raw.role, args, ...(raw.hello_timeout_ms !== undefined ? { hello_timeout_ms: raw.hello_timeout_ms as number } : {}) };
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
    else if (!isIdOk(id)) bad('id must be letters, digits, dot, underscore or hyphen, starting with a letter or digit');
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
    if (j.data !== undefined && !(DATA_KINDS as readonly string[]).includes(j.data as string)) {
      problems.push(problem('data', id, `${id ?? where}: data = ${JSON.stringify(j.data)} is refused; QA runs on the owner's real data under a snapshot and restore (delete the key or write data = "real")`));
    }
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
        data: j.data === 'real' ? 'real' : null,
        release_critical: j.release_critical === true,
      });
    }
  }

  const build = raw.build === undefined ? null : readBuild(raw.build, problems);
  const budget = raw.budget === undefined ? null : readBudget(raw.budget, problems);
  const tests = raw.tests === undefined ? null : readTests(raw.tests, problems);
  const bridge = readBridges(raw.bridge, problems);
  const plugin = raw.plugin === undefined ? null : readPlugin(raw.plugin, problems);

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
    value: { version: 1, app: raw.app as string, coverage_owners: owners, adapters, journeys, changes, build, budget, tests, bridge, plugin },
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
  /** Today as `YYYY-MM-DD` for bridge expiry; defaults to the current UTC day. */
  today?: string;
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

/** Manifest-level validation: unique immutable ids, entries exist, adapters resolve, native-ui binds a built binary. */
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
    // runner "scenario" launches the app's one [build] binary (checked below); only an adapter needs its own artifact here.
    if (j.runner === 'scenario' && !m.build) {
      out.push(problem('scenario_no_build', j.id, `${j.id}: runner = "scenario" needs a [build] section, the app's one QA build`));
    }
    if (j.runner === 'scenario' && !m.tests) {
      out.push(problem('scenario_no_tests', j.id, `${j.id}: runner = "scenario" needs a [tests] section with untagged_baseline`));
    }
    if (j.surface === 'native-ui' && j.runner === 'adapter') {
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
  out.push(...validateWave2(m, opts.today ?? new Date().toISOString().slice(0, 10)));
  return out;
}

/** `[build]`, `[budget]`, `[tests]`, `[[bridge]]` and `[plugin]`: the same checks `journeys/manifest.rs` makes on them. */
function validateWave2(m: Manifest, today: string): Problem[] {
  const out: Problem[] = [];
  const bad = (code: string, detail: string) => out.push(problem(code, null, detail));
  const b = m.build;
  if (b) {
    if (b.command.length === 0 || b.command.some((w) => w.trim() === '')) bad('build_command', '[build] command is empty or holds an empty word');
    if (b.command.includes('--all-features') || b.command.includes('--no-default-features')) {
      bad('build_command', '[build] command names --all-features or --no-default-features; name the QA features with --features');
    }
    const asked = new Set<string>();
    b.command.forEach((word, i) => {
      if (word === '--features' && b.command[i + 1] !== undefined) b.command[i + 1].split(/[\s,]+/).filter(Boolean).forEach((f) => asked.add(f));
      else if (word.startsWith('--features=')) word.slice('--features='.length).split(/[\s,]+/).filter(Boolean).forEach((f) => asked.add(f));
    });
    const declared = new Set(b.features);
    if (declared.size !== asked.size || [...declared].some((f) => !asked.has(f))) {
      bad('build_features', `[build] features ${JSON.stringify([...declared].sort())} must equal the --features of the command ${JSON.stringify([...asked].sort())}`);
    }
    if (b.artifacts.length === 0) bad('build_artifacts', '[build] declares no artifacts, so no binary is bound to the build');
    const roles = new Set<string>();
    for (const [i, a] of b.artifacts.entries()) {
      const what = `[build] artifacts[${i}]`;
      if (!ROLE_NAME.test(a.role)) bad('build_role', `${what}: role ${JSON.stringify(a.role)} must be letters, digits, '.', '_' or '-' (at most 64) and not start with '.'`);
      if (roles.has(a.role)) bad('build_role', `${what}: role ${JSON.stringify(a.role)} is declared twice`);
      roles.add(a.role);
      const named = (v?: string) => v !== undefined && v.trim() !== '';
      if (!named(a.cargo_target) && !named(a.cargo_package) && !named(a.path)) bad('build_artifact', `${what}: names none of cargo_target, cargo_package or path`);
      if (a.path !== undefined && (path.isAbsolute(a.path) || a.path.split(/[\\/]+/).includes('..'))) bad('build_artifact', `${what}: path ${a.path} must be relative and inside the app root`);
      if (!named(a.cargo_target) && !named(a.cargo_package) && named(a.path)) {
        bad('build_artifact', `${what}: names only a path, but the build lane binds a binary only by cargo_target or cargo_package`);
      }
    }
    if (m.plugin) {
      const p = m.plugin;
      const art = b.artifacts.find((x) => x.role === p.role);
      if (!ROLE_NAME.test(p.role)) bad('plugin_role', `[plugin] role ${JSON.stringify(p.role)} is not a valid role name`);
      else if (!art) bad('plugin_role', `[plugin] role ${JSON.stringify(p.role)} is not a [build] artifact role`);
      if (RUNNER_LAUNCHED_ROLES.includes(p.role)) bad('plugin_role', `[plugin] role ${JSON.stringify(p.role)} is launched by the runner (engine or UI); a plugin must be its own binary`);
      if (art && art.qa_marker) bad('plugin_role', `[plugin] role ${JSON.stringify(p.role)} is QA-marked (launched by the runner); set qa_marker = false on the plugin artifact`);
      if (p.hello_timeout_ms !== undefined && (p.hello_timeout_ms < 1 || p.hello_timeout_ms > 60_000)) bad('plugin_timeout', '[plugin] hello_timeout_ms must be between 1 and 60000');
    }
  } else if (m.plugin) {
    bad('plugin_build', '[plugin] needs a [build] section: the plugin is a build-record binary');
  }
  if (m.budget) {
    if (!isRealDay(m.budget.warn_until)) bad('budget', `[budget] warn_until ${JSON.stringify(m.budget.warn_until)} is not a real YYYY-MM-DD day`);
    for (const kind of KIND_TABLES) {
      const kb = m.budget[kind];
      if (!kb) continue;
      for (const field of ['step_ms', 'journey_ms', 'suite_ms'] as const) {
        if (kb[field] === 0) bad('budget', `[budget.${kind}] ${field} must be above 0`);
      }
    }
  }
  const seen = new Set<string>();
  for (const br of m.bridge ?? []) {
    const what = `[[bridge]] app ${JSON.stringify(br.app)}`;
    if (br.app.trim() === '') bad('bridge', '[[bridge]] with an empty app name');
    else if (seen.has(br.app)) bad('bridge', `${what}: duplicate app (one bridge entry per app)`);
    seen.add(br.app);
    if (!isIdOk(br.binary)) bad('bridge', `${what}: binary ${JSON.stringify(br.binary)} must be a cargo target name`);
    if (!isRealDay(br.removal_date)) bad('bridge', `${what}: removal_date ${JSON.stringify(br.removal_date)} is not a real YYYY-MM-DD day`);
    else if (br.removal_date < today) bad('bridge_expired', `${what}: removal_date ${br.removal_date} has passed; the entry is refused`);
    for (const n of [...br.tests, ...br.ignored_required]) if (!testNameOk(n)) bad('bridge', `${what}: ${JSON.stringify(n)} is not a usable test name`);
    for (const src of br.sources) {
      if (path.isAbsolute(src) || src.split(/[\\/]+/).includes('..')) bad('bridge', `${what}: source ${src} must be a relative path inside the app root`);
      else if (/[*?[{]/.test(src)) bad('bridge', `${what}: source ${src} must be a literal file path (the lint matches exact paths)`);
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
