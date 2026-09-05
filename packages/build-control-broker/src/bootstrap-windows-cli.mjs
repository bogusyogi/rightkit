#!/usr/bin/env node
// Windows counterpart to bootstrap-macos-cli.mjs. Recreates config.json & the
// scheduled task from nothing but this checkout — see that file's header for
// why this exists: neither was ever committed anywhere before.
import { createHash } from 'node:crypto';
import { execFile as childExecFile } from 'node:child_process';
import { promisify } from 'node:util';
import { constants, mkdirSync, writeFileSync, existsSync } from 'node:fs';
import { copyFile, mkdir, open, readFile, realpath } from 'node:fs/promises';
import { availableParallelism as cpuCount, totalmem } from 'node:os';
import { dirname, join, resolve, win32 } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runWindowsCutover } from './runtime-cutover-windows-cli.mjs';
import { deriveCompileBudgetUnits, preserveOperatorTunedConfig, stripBom } from './config.mjs';

const execFile = promisify(childExecFile);
const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url));
const REPO_ROOT = resolve(PACKAGE_ROOT, '..', '..', '..', '..');
const TASK_NAME = 'RightKitManagedAgentBroker';

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

export function ensureRemoteRoutingRoots(externalRoot) {
  const inputs = join(externalRoot, 'remote-inputs');
  mkdirSync(inputs, { recursive: true });
  return inputs;
}

export function isDirectMain({ moduleUrl = import.meta.url, argv1 = process.argv[1], platform = process.platform } = {}) {
  if (typeof argv1 !== 'string' || !argv1) return false;
  const modulePath = decodeURIComponent(new URL(moduleUrl).pathname);
  if (platform === 'win32') {
    const normalize = (value) => win32.normalize(value.replace(/^\/(?=[A-Za-z]:)/, '')).toLowerCase();
    return normalize(modulePath) === normalize(win32.resolve(argv1));
  }
  return resolve(modulePath) === resolve(argv1);
}

export async function sha256File(path) { return createHash('sha256').update(await readFile(path)).digest('hex'); }

/** Windows has no single `which`; `where.exe` lists every match on PATH, one
 * per line — the first is what an unqualified invocation would actually run. */
export async function whereUnmanaged(name, run = execFile, { exists = existsSync } = {}) {
  let candidates;
  try { candidates = (await run('where', [name])).stdout.trim().split(/\r?\n/).filter(Boolean); }
  catch { return null; }
  for (const candidate of candidates) {
    if (exists(join(dirname(candidate), '.rightkit-managed-agent-shims.json'))) continue;
    const prefix = await executablePrefix(candidate).catch(() => '');
    if (prefix.includes('rightkit.build-control.agent-shims.install.v1') || prefix.includes('packages\\release\\cargo-guard.mjs')) continue;
    return realpath(candidate);
  }
  return null;
}

async function executablePrefix(path, bytes = 1_024) {
  const handle = await open(path, 'r');
  try { const buffer = Buffer.alloc(bytes); const { bytesRead } = await handle.read(buffer, 0, bytes, 0); return buffer.subarray(0, bytesRead).toString('utf8'); }
  finally { await handle.close(); }
}

async function rustupTool(rustupPath, id) {
  if (!rustupPath) return null;
  try { const path = (await execFile(rustupPath, ['which', id])).stdout.trim(); return path ? realpath(path) : null; }
  catch { return null; }
}

export async function discoverTools() {
  const tools = [];
  const rustupPath = await whereUnmanaged('rustup.exe').then((found) => found ?? whereUnmanaged('rustup'));
  for (const id of KNOWN_TOOLS) {
    const path = ['cargo', 'rustc', 'rustdoc'].includes(id)
      ? await rustupTool(rustupPath, id)
      : id === 'rustup'
        ? rustupPath
        : await whereUnmanaged(`${id}.exe`).then((found) => found ?? whereUnmanaged(id));
    if (!path) continue;
    const real = await realpath(path);
    tools.push({ id, path: real, root: dirname(real), sha256: await sha256File(real) });
  }
  for (const [, id] of EXTERNAL_CARGO_TOOLS) {
    const found = await whereUnmanaged(`${id}.exe`).then((value) => value ?? whereUnmanaged(id)); if (!found) continue;
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

export async function rustcVersionTriple(rustcPath = 'rustc') {
  try {
    const { stdout } = await execFile(rustcPath, ['-vV']);
    const version = stdout.match(/^release:\s*(\S+)/m)?.[1];
    const host = stdout.match(/^host:\s*(\S+)/m)?.[1];
    return version && host ? `stable-${version}-${host}` : null;
  } catch { return null; }
}

export async function sccacheVersion(path, run = execFile) {
  if (typeof path !== 'string' || !path) return null;
  try {
    const { stdout } = await run(path, ['--version']); const match = String(stdout).match(/sccache\s+(\d+)\.(\d+)\.(\d+)/i);
    return match ? `${match[1]}.${match[2]}.${match[3]}` : null;
  } catch { return null; }
}

const supportsSccacheVersion = (version) => {
  const match = String(version ?? '').match(/^(\d+)\.(\d+)\.(\d+)$/);
  return Boolean(match) && (Number(match[1]) > 0 || Number(match[2]) >= 17);
};

export async function supportsClientSideSccache(path, run = execFile) { return supportsSccacheVersion(await sccacheVersion(path, run)); }

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
    heartbeatIntervalMs: 5_000, processGraceMs: 5_000, jobDeadlineMs: 0,
    outputBufferBytes: 8 * 1024 * 1024, outputHeadBytes: 16_384, outputTailBytes: 16_384,
    controlConcurrency: 4, analysisConcurrency: 2, buildConcurrency: 3, workspaceWriteConcurrency: 4, targetCacheCapBytes: 150 * 1024 ** 3,
    workspaceDiscoveryTtlMs: 600_000, workspaceDiscoveryTimeoutMs: 30_000,
  };
}

// Windows host policy: high-memory x86 workstations may use up to 16 Cargo
// compiler jobs per admitted build. Logical CPUs and an approximately 2 GiB
// per-job memory ceiling still bound smaller hosts.
export function defaultCargoBuildJobs({ cpus = cpuCount(), totalMemoryBytes = totalmem() } = {}) {
  const logical = Number.isSafeInteger(cpus) && cpus > 0 ? cpus : 1;
  const memoryGiB = Number.isFinite(totalMemoryBytes) && totalMemoryBytes > 0 ? totalMemoryBytes / 1024 ** 3 : 2;
  return Math.max(1, Math.min(16, logical, Math.floor(memoryGiB / 2)));
}

export async function buildConfig({ externalRoot }) {
  const tools = await stageManagedTools(await discoverTools(), externalRoot);
  const sccacheTool = tools.find((tool) => tool.id === 'sccache');
  const expectedSccacheVersion = await sccacheVersion(sccacheTool?.path);
  const clientSideSccache = supportsSccacheVersion(expectedSccacheVersion);
  const toolchain = (await rustcVersionTriple(tools.find((tool) => tool.id === 'rustc')?.path)) ?? 'stable-x86_64-pc-windows-msvc';
  const targetTriple = 'x86_64-pc-windows-msvc';
  return {
    schemaVersion: 2,
    broker: {
      externalRoot: { path: externalRoot, volumeId: 'unknown', mounted: true, writable: true, internal: false },
      capacityBytes: 200 * 1024 ** 3,
      stateFile: join(externalRoot, 'state', 'broker.json'),
      activationFencePath: join(externalRoot, 'state', 'runtime-activation.json'),
      receiptsDir: join(externalRoot, 'receipts'),
    },
    windowsPipe: '\\\\.\\pipe\\rightkit-build-control',
    tools,
    shims: shimTable(tools),
    externalCommands: externalCommands(tools),
    defaults: {
      toolchain, targetTriple, targetTriples: [targetTriple],
      buildProfile: 'development',
      // Only referenced when actually discovered: the field is schema-optional,
      // & pointing it at an unregistered tool id rejects the whole config.
      ...(tools.some((tool) => tool.id === 'rustc') ? { rustcToolId: 'rustc' } : {}),
      ...(tools.some((tool) => tool.id === 'rustdoc') ? { rustdocToolId: 'rustdoc' } : {}),
      ...(clientSideSccache ? { sccacheToolId: 'sccache' } : {}),
      cacheMode: clientSideSccache ? 'managed' : 'disabled', cargoBuildJobs: defaultCargoBuildJobs(),
    },
    // Doc section 3.1: never enable during an in-flight release; a deliberate,
    // separately-decided activation, not a bootstrap default.
    sccache: { mode: clientSideSccache ? 'managed' : 'disabled', clientSide: clientSideSccache, lifecycleOwner: clientSideSccache ? 'broker' : null, requireOwnershipProof: clientSideSccache, ...(clientSideSccache ? { expectedVersion: expectedSccacheVersion } : {}), maxSizeBytes: 20 * 1024 ** 3, gatewayPort: 4_227, healthLeaseMs: 10_000, recoveryTimeoutMs: 10_000, statsIntervalMs: 60_000 },
    outputPolicy: { mode: 'enforce', allowedSourceDirs: [], scanRoot: 'repository' },
    bounds: { ...defaultBounds(), compileBudgetUnits: deriveCompileBudgetUnits() },
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const externalRoot = resolve(options.externalRoot ?? 'D:\\.rightkit-managed');
  const configPath = join(externalRoot, 'config.json');

  mkdirSync(externalRoot, { recursive: true });
  mkdirSync(join(externalRoot, 'state'), { recursive: true });
  ensureRemoteRoutingRoots(externalRoot);

  if (!existsSync(configPath) || options.force) {
    const config = await buildConfig({ externalRoot });
    let preserved = [];
    if (existsSync(configPath)) {
      let previous = null;
      try { previous = JSON.parse(stripBom(await readFile(configPath, 'utf8'))); } catch { /* unreadable previous config: regenerate from defaults */ }
      preserved = preserveOperatorTunedConfig(config, previous);
    }
    writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
    console.log(`bootstrap-windows: wrote ${configPath} (${config.tools.length} tools discovered)`);
    if (preserved.length) console.log(`bootstrap-windows: preserved operator-tuned ${preserved.join(', ')} from existing config`);
  } else {
    console.log(`bootstrap-windows: ${configPath} already exists; pass --force to regenerate`);
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
    healthWorkspace, timeoutMs: 20_000,
  });
  console.log(JSON.stringify(receipt, null, 2));
  console.log('bootstrap-windows: broker activated');
}

if (isDirectMain()) main().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
