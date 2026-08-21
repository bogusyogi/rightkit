#!/usr/bin/env node
// Windows counterpart to bootstrap-macos-cli.mjs. Recreates config.json & the
// scheduled task from nothing but this checkout — see that file's header for
// why this exists: neither was ever committed anywhere before.
import { createHash } from 'node:crypto';
import { execFile as childExecFile } from 'node:child_process';
import { promisify } from 'node:util';
import { constants, mkdirSync, writeFileSync, existsSync } from 'node:fs';
import { copyFile, mkdir, readFile, realpath } from 'node:fs/promises';
import { availableParallelism as cpuCount, totalmem } from 'node:os';
import { dirname, join, resolve, win32 } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runWindowsCutover } from './runtime-cutover-windows-cli.mjs';

const execFile = promisify(childExecFile);
const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url));
const REPO_ROOT = resolve(PACKAGE_ROOT, '..', '..', '..', '..');
const TASK_NAME = 'RightKitBuildControlManaged';

const KNOWN_TOOLS = ['cargo', 'rustc', 'rustdoc', 'rustup', 'sccache', 'pnpm', 'npm', 'cmake', 'make', 'clang', 'gcc'];
const EXTERNAL_CARGO_TOOLS = Object.freeze([
  ['about', 'cargo-about', 'ANALYSIS'], ['outdated', 'cargo-outdated', 'ANALYSIS'], ['audit', 'cargo-audit', 'ANALYSIS'],
  ['deny', 'cargo-deny', 'ANALYSIS'], ['machete', 'cargo-machete', 'ANALYSIS'], ['upgrade', 'cargo-upgrade', 'WORKSPACE_WRITE'],
  ['set-version', 'cargo-set-version', 'WORKSPACE_WRITE'], ['nextest', 'cargo-nextest', 'BUILD']
]);
const STANDALONE_MANAGED_TOOLS = new Set(['sccache', ...EXTERNAL_CARGO_TOOLS.map(([, id]) => id)]);

export function parseArgs(argv) {
  const values = { force: false };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === '--force') { values.force = true; continue; }
    if (flag === '--external-root') { values.externalRoot = argv[++index]; continue; }
    throw new TypeError(`unknown bootstrap argument: ${flag}`);
  }
  return values;
}

export function isDirectMain({ moduleUrl = import.meta.url, argv1 = process.argv[1], platform = process.platform } = {}) {
  if (typeof argv1 !== 'string' || !argv1) return false;
  const modulePath = fileURLToPath(moduleUrl);
  if (platform === 'win32') {
    const normalize = (value) => win32.normalize(value.replace(/^\/(?=[A-Za-z]:)/, '')).toLowerCase();
    return normalize(modulePath) === normalize(win32.resolve(argv1));
  }
  return resolve(modulePath) === resolve(argv1);
}

export async function sha256File(path) { return createHash('sha256').update(await readFile(path)).digest('hex'); }

/** Windows has no single `which`; `where.exe` lists every match on PATH, one
 * per line — the first is what an unqualified invocation would actually run. */
export async function where(name) {
  try { return (await execFile('where', [name])).stdout.trim().split(/\r?\n/)[0] || null; }
  catch { return null; }
}

export async function discoverTools() {
  const tools = [];
  for (const id of KNOWN_TOOLS) {
    const path = await where(`${id}.exe`).then((found) => found ?? where(id));
    if (!path) continue;
    const real = await realpath(path);
    tools.push({ id, path: real, root: dirname(real), sha256: await sha256File(real) });
  }
  for (const [, id] of EXTERNAL_CARGO_TOOLS) {
    const found = await where(`${id}.exe`).then((value) => value ?? where(id)); if (!found) continue;
    const real = await realpath(found); tools.push({ id, path: real, root: dirname(real), sha256: await sha256File(real) });
  }
  return tools;
}

export async function stageManagedTools(tools, externalRoot) {
  const staged = [];
  for (const tool of tools) {
    // rustc/rustdoc require sibling DLLs; package-manager shims may require
    // their install trees. Preserve bundle-relative paths while digest-pinning
    // them. Stage only standalone sccache & Cargo extension executables.
    if (!STANDALONE_MANAGED_TOOLS.has(tool.id)) { staged.push(tool); continue; }
    const directory = join(externalRoot, 'rightkit-build-control', 'global', 'tools', tool.id, tool.sha256);
    const target = join(directory, `${tool.id}.exe`); await mkdir(directory, { recursive: true });
    try { await copyFile(tool.path, target, constants.COPYFILE_EXCL); }
    catch (error) { if (error.code !== 'EEXIST' || await sha256File(target) !== tool.sha256) throw error; }
    staged.push({ ...tool, path: target, root: directory });
  }
  return staged;
}

function externalCommands(tools) {
  const byId = new Map(tools.map((tool) => [tool.id, tool]));
  return EXTERNAL_CARGO_TOOLS.flatMap(([command, toolId, lane]) => byId.has(toolId) ? [{ shim: 'cargo', command, toolId, lane, digest: byId.get(toolId).sha256 }] : []);
}

export async function rustcVersionTriple() {
  try {
    const { stdout } = await execFile('rustc', ['-vV']);
    const version = stdout.match(/^release:\s*(\S+)/m)?.[1];
    const host = stdout.match(/^host:\s*(\S+)/m)?.[1];
    return version && host ? `stable-${version}-${host}` : null;
  } catch { return null; }
}

export async function supportsClientSideSccache(path, run = execFile) {
  if (typeof path !== 'string' || !path) return false;
  try {
    const { stdout } = await run(path, ['--version']); const match = String(stdout).match(/sccache\s+(\d+)\.(\d+)\.(\d+)/i);
    return Boolean(match) && (Number(match[1]) > 0 || Number(match[2]) >= 17);
  } catch { return false; }
}

export function shimTable(tools) {
  const present = new Set(tools.map((tool) => tool.id));
  const table = {};
  for (const id of ['cargo', 'rustc', 'rustdoc', 'rustup', 'pnpm', 'npm', 'cmake', 'make']) if (present.has(id)) table[id] = id;
  if (present.has('clang')) { table.cc = 'clang'; table['c++'] = 'clang'; table.clang = 'clang'; }
  if (present.has('gcc')) table.gcc = 'gcc';
  return table;
}

// Same values as bootstrap-macos-cli.mjs's defaultBounds: this checkout has
// actually run under them, not fresh guesses. See that file for the rationale.
export function defaultBounds() {
  return {
    heartbeatIntervalMs: 5_000, diskSampleIntervalMs: 5_000, processGraceMs: 5_000,
    retentionMs: 604_800_000, gcIntervalMs: 300_000, statfsTimeoutMs: 1_000, jobDeadlineMs: 0,
    outputBufferBytes: 8 * 1024 * 1024, outputHeadBytes: 16_384, outputTailBytes: 16_384,
    eventHistoryEntries: 4_096, eventHistoryBytes: 8 * 1024 * 1024, eventLogMaxBytes: 8 * 1024 * 1024,
    maintenanceBatchEntries: 128, controlConcurrency: 4, analysisConcurrency: 2, buildConcurrency: 1, workspaceWriteConcurrency: 4,
    maintenanceConcurrency: 1, priorityAgingMs: 30_000, targetCacheCapBytes: 150 * 1024 ** 3,
    targetCacheFloorMs: 86_400_000,
  };
}

// Same derivation as bootstrap-macos-cli.mjs's defaultCargoBuildJobs; each host
// derives its own value rather than inheriting a literal from the other.
export function defaultCargoBuildJobs({ cpus = cpuCount(), totalMemoryBytes = totalmem() } = {}) {
  const logical = Number.isSafeInteger(cpus) && cpus > 0 ? cpus : 1;
  const performanceCores = Math.max(1, Math.floor(logical / 2));
  const memoryGiB = Number.isFinite(totalMemoryBytes) && totalMemoryBytes > 0 ? totalMemoryBytes / 1024 ** 3 : 2;
  return Math.max(1, Math.min(4, performanceCores, Math.floor(memoryGiB / 2)));
}

export async function buildConfig({ externalRoot }) {
  const tools = await stageManagedTools(await discoverTools(), externalRoot);
  const sccacheTool = tools.find((tool) => tool.id === 'sccache');
  const clientSideSccache = await supportsClientSideSccache(sccacheTool?.path);
  const toolchain = (await rustcVersionTriple()) ?? 'stable-x86_64-pc-windows-msvc';
  const targetTriple = 'x86_64-pc-windows-msvc';
  return {
    schemaVersion: 2,
    broker: {
      externalRoot: { path: externalRoot, volumeId: 'unknown', mounted: true, writable: true, internal: false },
      capacityBytes: 200 * 1024 ** 3,
      stateFile: join(externalRoot, 'state', 'broker.json'),
      databasePath: join(externalRoot, 'state', 'broker.sqlite'),
      eventLogPath: join(externalRoot, 'state', 'events.ndjson'),
      activationFencePath: join(externalRoot, 'state', 'runtime-activation.json'),
      receiptsDir: join(externalRoot, 'receipts'),
    },
    windowsPipe: '\\\\.\\pipe\\rightkit-build-control',
    tools,
    shims: shimTable(tools),
    externalCommands: externalCommands(tools),
    defaults: {
      toolchain, targetTriple, targetTriples: [targetTriple],
      buildProfile: 'development', reservationBytes: 16 * 1024 ** 3,
      // Only referenced when actually discovered: the field is schema-optional,
      // & pointing it at an unregistered tool id rejects the whole config.
      ...(tools.some((tool) => tool.id === 'rustc') ? { rustcToolId: 'rustc' } : {}),
      ...(tools.some((tool) => tool.id === 'rustdoc') ? { rustdocToolId: 'rustdoc' } : {}),
      ...(clientSideSccache ? { sccacheToolId: 'sccache' } : {}),
      cacheMode: clientSideSccache ? 'managed' : 'disabled', cargoBuildJobs: defaultCargoBuildJobs(),
    },
    // Doc section 3.1: never enable during an in-flight release; a deliberate,
    // separately-decided activation, not a bootstrap default.
    sccache: { mode: clientSideSccache ? 'managed' : 'disabled', clientSide: clientSideSccache, lifecycleOwner: null, requireOwnershipProof: false, maxSizeBytes: 20 * 1024 ** 3 },
    outputPolicy: { mode: 'enforce', allowedSourceDirs: [], scanRoot: 'repository' },
    bounds: defaultBounds(),
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const externalRoot = resolve(options.externalRoot ?? 'D:\\.rightkit-managed');
  const configPath = join(externalRoot, 'config.json');
  const v1StatePath = join(externalRoot, 'state', 'broker.json');

  mkdirSync(externalRoot, { recursive: true });
  mkdirSync(join(externalRoot, 'state'), { recursive: true });

  if (!existsSync(configPath) || options.force) {
    const config = await buildConfig({ externalRoot });
    writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
    console.log(`bootstrap-windows: wrote ${configPath} (${config.tools.length} tools discovered)`);
  } else {
    console.log(`bootstrap-windows: ${configPath} already exists; pass --force to regenerate`);
  }

  if (!existsSync(v1StatePath)) {
    writeFileSync(v1StatePath, `${JSON.stringify({ entries: [] }, null, 2)}\n`);
    console.log(`bootstrap-windows: wrote empty v1 drain state at ${v1StatePath}`);
  }

  const sourceRevision = (await execFile('git', ['-C', REPO_ROOT, 'rev-parse', 'HEAD'])).stdout.trim();
  const sourcePackagesRoot = join(REPO_ROOT, 'tools', 'rightkit', 'packages');
  const relativePackages = sourcePackagesRoot.startsWith(`${REPO_ROOT}\\`) ? sourcePackagesRoot.slice(REPO_ROOT.length + 1).replaceAll('\\', '/') : sourcePackagesRoot;
  const sourceTreeDigest = (await execFile('git', ['-C', REPO_ROOT, 'rev-parse', `HEAD:${relativePackages}`])).stdout.trim();

  const healthWorkspace = join(externalRoot, 'fixture-rust');
  if (!existsSync(healthWorkspace)) {
    console.log(`bootstrap-windows: ${healthWorkspace} is missing; the cutover health probe requires a tiny cargo workspace there — see docs/rules/rightkit-broker-recovery.md`);
    process.exitCode = 1;
    return;
  }

  const receipt = await runWindowsCutover({
    configPath, externalRoot, sourcePackagesRoot, sourceRevision, sourceTreeDigest,
    version: `bootstrap-${sourceRevision.slice(0, 8)}-${Date.now()}`,
    taskName: TASK_NAME, endpoint: '\\\\.\\pipe\\rightkit-build-control',
    v1StatePath, healthWorkspace, timeoutMs: 20_000,
  });
  console.log(JSON.stringify(receipt, null, 2));
  console.log('bootstrap-windows: broker activated');
}

if (isDirectMain()) main().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
