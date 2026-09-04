#!/usr/bin/env node
// Recreates the managed broker from nothing but this checkout: config.json, the
// launchd agent, and the first activation. Everything it writes was previously
// only ever hand-typed once per machine and never committed anywhere, so a
// reset machine had no path back except reconstructing this file from memory.
import { createHash } from 'node:crypto';
import { execFile as childExecFile, spawnSync } from 'node:child_process';
import { promisify } from 'node:util';
import { constants, mkdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { chmod, copyFile, lstat, mkdir, open, readFile, realpath, symlink } from 'node:fs/promises';
import { availableParallelism as cpuCount, homedir, totalmem } from 'node:os';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runMacOSCutover } from './runtime-cutover-macos-cli.mjs';
import { preserveOperatorTunedConfig, stripBom } from './config.mjs';
// A `file://${process.argv[1]}` comparison never matches on Windows, where
// argv[1] is a drive path: the CLI exited 0 having silently done nothing.
import { isDirectMain } from './bootstrap-windows-cli.mjs';

const execFile = promisify(childExecFile);
const PACKAGE_ROOT = fileURLToPath(new URL('..', import.meta.url));
const REPO_ROOT = resolve(PACKAGE_ROOT, '..', '..', '..', '..');
const LABEL = 'dev.rightkit.build-control-managed';

// One id per tool the broker may pin. `probe` resolves it on PATH; missing
// tools are simply omitted; nothing here requires every tool to exist.
const KNOWN_TOOLS = ['cargo', 'rustc', 'rustdoc', 'rustup', 'sccache', 'pnpm', 'npm', 'swift', 'swiftc', 'cmake', 'make', 'clang', 'gcc'];
const EXTERNAL_CARGO_TOOLS = Object.freeze([
  ['about', 'cargo-about', 'ANALYSIS'], ['outdated', 'cargo-outdated', 'ANALYSIS'], ['audit', 'cargo-audit', 'ANALYSIS'],
  ['deny', 'cargo-deny', 'ANALYSIS'], ['machete', 'cargo-machete', 'ANALYSIS'], ['upgrade', 'cargo-upgrade', 'WORKSPACE_WRITE'],
  ['set-version', 'cargo-set-version', 'WORKSPACE_WRITE'], ['nextest', 'cargo-nextest', 'BUILD']
]);
const STANDALONE_MANAGED_TOOLS = new Set(['sccache', ...EXTERNAL_CARGO_TOOLS.map(([, id]) => id)]);

export function parseArgs(argv) {
  const values = { force: false, drainTimeoutMs: 0, timeoutMs: 120_000 };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === '--force') { values.force = true; continue; }
    if (flag === '--external-root') { values.externalRoot = argv[++index]; continue; }
    if (flag === '--node') { values.nodePath = argv[++index]; continue; }
    if (flag === '--timeout-ms') {
      values.timeoutMs = Number(argv[++index]);
      if (!Number.isSafeInteger(values.timeoutMs) || values.timeoutMs < 100) throw new TypeError('--timeout-ms must be an integer of at least 100');
      continue;
    }
    if (flag === '--drain-timeout-ms') {
      values.drainTimeoutMs = Number(argv[++index]);
      if (!Number.isSafeInteger(values.drainTimeoutMs) || values.drainTimeoutMs < 0) throw new TypeError('--drain-timeout-ms must be a non-negative integer');
      continue;
    }
    throw new TypeError(`unknown bootstrap argument: ${flag}`);
  }
  return values;
}

export async function sha256File(path) { return createHash('sha256').update(await readFile(path)).digest('hex'); }

export async function which(name) {
  try { return (await execFile('which', [name])).stdout.trim() || null; }
  catch { return null; }
}

async function executablePrefix(path, bytes = 1_024) {
  const handle = await open(path, 'r');
  try { const buffer = Buffer.alloc(bytes); const { bytesRead } = await handle.read(buffer, 0, bytes, 0); return buffer.subarray(0, bytesRead).toString('utf8'); }
  finally { await handle.close(); }
}

/** Bootstrap may itself run under RightKit, so PATH begins with agent shims.
 * Selecting one as its own managed tool creates recursive broker calls. */
export async function whichUnmanaged(name) {
  let candidates;
  try { candidates = (await execFile('which', ['-a', name])).stdout.trim().split(/\r?\n/).filter(Boolean); }
  catch { return null; }
  for (const candidate of candidates) {
    const prefix = await executablePrefix(candidate).catch(() => '');
    if (prefix.includes('rightkit.build-control.agent-shims.install.v1') || prefix.includes('/packages/release/cargo-guard.mjs')) continue;
    return realpath(candidate);
  }
  return null;
}

async function rustupTool(rustupPath, id) {
  if (!rustupPath) return null;
  try { const path = (await execFile(rustupPath, ['which', id])).stdout.trim(); return path ? realpath(path) : null; }
  catch { return null; }
}

/** Probe every known tool on PATH & hash what is actually installed. Never
 * invents a path: a tool absent from PATH is simply absent from the config. */
export async function discoverTools() {
  const tools = [];
  const rustupPath = await whichUnmanaged('rustup');
  for (const id of KNOWN_TOOLS) {
    const path = ['cargo', 'rustc', 'rustdoc'].includes(id) ? await rustupTool(rustupPath, id) : await whichUnmanaged(id);
    if (!path) continue;
    const real = await realpath(path);
    tools.push({ id, path: real, root: dirname(real), sha256: await sha256File(real) });
  }
  for (const [, id] of EXTERNAL_CARGO_TOOLS) {
    const found = await whichUnmanaged(id); if (!found) continue;
    const real = await realpath(found); tools.push({ id, path: real, root: dirname(real), sha256: await sha256File(real) });
  }
  return tools;
}

export async function stageManagedTools(tools, externalRoot) {
  const staged = [];
  for (const tool of tools) {
    // Rust toolchain binaries depend on sibling libraries, while package-manager
    // shims may resolve modules relative to their install tree. Keep those
    // bundle-relative paths intact; their digests remain pinned by the registry.
    // Only standalone sccache & Cargo extensions move into the managed root.
    if (!STANDALONE_MANAGED_TOOLS.has(tool.id)) { staged.push(tool); continue; }
    const directory = join(externalRoot, 'rightkit-build-control', 'global', 'tools', tool.id, tool.sha256);
    const target = join(directory, tool.id); await mkdir(directory, { recursive: true });
    try { await copyFile(tool.path, target, constants.COPYFILE_EXCL); await chmod(target, 0o755); }
    catch (error) { if (error.code !== 'EEXIST' || await sha256File(target) !== tool.sha256) throw error; }
    staged.push({ ...tool, path: target, root: directory });
  }
  return staged;
}

/** Rust 1.98's macOS rust-objcopy rpath resolves libLLVM beside the target
 * tools, while rustup installs that library at the toolchain root. DYLD_* is
 * consumed before rustc can forward it to rust-objcopy, so bootstrap repairs
 * the missing immutable companion link instead of accepting client loader
 * authority or wrapping the compiler. */
export async function ensureMacOSRustToolLoaderLinks(tools, targetTriples) {
  if (process.platform !== 'darwin') return [];
  const cargo = tools.find((tool) => tool.id === 'cargo');
  if (!cargo || !Array.isArray(targetTriples)) return [];
  const toolchainRoot = resolve(dirname(cargo.path), '..');
  const library = join(toolchainRoot, 'lib', 'libLLVM.dylib');
  try { await lstat(library); } catch (error) { if (error.code === 'ENOENT') return []; throw error; }
  const repaired = [];
  for (const triple of targetTriples) {
    const objcopy = join(toolchainRoot, 'lib', 'rustlib', triple, 'bin', 'rust-objcopy');
    try { await lstat(objcopy); } catch (error) { if (error.code === 'ENOENT') continue; throw error; }
    const target = join(toolchainRoot, 'lib', 'rustlib', triple, 'lib', 'libLLVM.dylib');
    await mkdir(dirname(target), { recursive: true });
    try { await symlink('../../../libLLVM.dylib', target, 'file'); }
    catch (error) {
      if (error.code !== 'EEXIST') throw error;
      if (await realpath(target) !== await realpath(library)) throw new Error(`rust-objcopy loader companion is not owned by this toolchain: ${target}`);
    }
    repaired.push(target);
  }
  return repaired;
}

function externalCommands(tools) {
  const byId = new Map(tools.map((tool) => [tool.id, tool]));
  return EXTERNAL_CARGO_TOOLS.flatMap(([command, toolId, lane]) => byId.has(toolId) ? [{ shim: 'cargo', command, toolId, lane, digest: byId.get(toolId).sha256 }] : []);
}

export async function resolveVolumeId(path) {
  try {
    const { stdout } = await execFile('diskutil', ['info', '-plist', path]);
    const match = stdout.match(/<key>VolumeUUID<\/key>\s*<string>([^<]+)<\/string>/);
    return match?.[1] ?? null;
  } catch { return null; }
}

export async function rustcVersionTriple(path = 'rustc') {
  try {
    const { stdout } = await execFile(path, ['-vV']);
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
  for (const id of ['cargo', 'rustc', 'rustdoc', 'rustup', 'pnpm', 'npm', 'swift', 'swiftc', 'cmake', 'make']) if (present.has(id)) table[id] = id;
  if (present.has('clang')) { table.cc = 'clang'; table['c++'] = 'clang'; table.clang = 'clang'; }
  if (present.has('gcc')) table.gcc = 'gcc';
  return table;
}

/** Doc section 3.3: one fixed heavy-build permit "initially"; raise only after
 * a measured safe run. These are the same bounds this checkout has actually run
 * under, not fresh guesses. */
export function defaultBounds() {
  return {
    heartbeatIntervalMs: 5_000, processGraceMs: 5_000, jobDeadlineMs: 0,
    outputBufferBytes: 8 * 1024 * 1024, outputHeadBytes: 16_384, outputTailBytes: 16_384,
    controlConcurrency: 4, analysisConcurrency: 2, buildConcurrency: 3, workspaceWriteConcurrency: 4, targetCacheCapBytes: 150 * 1024 ** 3,
    workspaceDiscoveryTtlMs: 600_000, workspaceDiscoveryTimeoutMs: 30_000,
  };
}

/** Parallel cargo jobs this host can link without swapping: memory binds before
 * CPU, so the performance-core count is clamped by ~2 GiB of RAM per job. */
export function defaultCargoBuildJobs({ cpus = cpuCount(), totalMemoryBytes = totalmem() } = {}) {
  const logical = Number.isSafeInteger(cpus) && cpus > 0 ? cpus : 1;
  // Apple silicon reports performance + efficiency cores as one logical count;
  // half is the performance share on every shipping configuration here.
  const performanceCores = Math.max(1, Math.floor(logical / 2));
  const memoryGiB = Number.isFinite(totalMemoryBytes) && totalMemoryBytes > 0 ? totalMemoryBytes / 1024 ** 3 : 2;
  return Math.max(1, Math.min(4, performanceCores, Math.floor(memoryGiB / 2)));
}

export async function buildConfig({ externalRoot }) {
  const tools = await stageManagedTools(await discoverTools(), externalRoot);
  const sccacheTool = tools.find((tool) => tool.id === 'sccache');
  const expectedSccacheVersion = await sccacheVersion(sccacheTool?.path);
  const managedSccache = supportsSccacheVersion(expectedSccacheVersion);
  const toolchain = (await rustcVersionTriple(tools.find((tool) => tool.id === 'rustc')?.path)) ?? 'stable-aarch64-apple-darwin';
  const targetTriple = process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
  const otherTriple = targetTriple === 'aarch64-apple-darwin' ? 'x86_64-apple-darwin' : 'aarch64-apple-darwin';
  const targetTriples = [targetTriple, otherTriple, 'aarch64-apple-ios', 'aarch64-apple-ios-sim'];
  return {
    schemaVersion: 2,
    broker: {
      externalRoot: { path: externalRoot, volumeId: (await resolveVolumeId(externalRoot)) ?? 'unknown', mounted: true, writable: true, internal: false },
      capacityBytes: 200 * 1024 ** 3,
      stateFile: join(externalRoot, 'state', 'broker.json'),
      activationFencePath: join(externalRoot, 'state', 'runtime-activation.json'),
      receiptsDir: join(externalRoot, 'receipts'),
    },
    unixSocket: join(externalRoot, 'broker.sock'),
    windowsPipe: '\\\\.\\pipe\\rightkit-build-control',
    tools,
    shims: shimTable(tools),
    externalCommands: externalCommands(tools),
    defaults: {
      toolchain, targetTriple, targetTriples,
      buildProfile: 'development',
      // Only referenced when actually discovered: the field is schema-optional,
      // & pointing it at an unregistered tool id rejects the whole config.
      ...(tools.some((tool) => tool.id === 'rustc') ? { rustcToolId: 'rustc' } : {}),
      ...(tools.some((tool) => tool.id === 'rustdoc') ? { rustdocToolId: 'rustdoc' } : {}),
      ...(managedSccache ? { sccacheToolId: 'sccache' } : {}),
      cacheMode: managedSccache ? 'managed' : 'disabled', cargoBuildJobs: defaultCargoBuildJobs(),
    },
    // Doc section 3.1: never enable during an in-flight release; a deliberate,
    // separately-decided activation, not a bootstrap default.
    // Client-side compilation keeps rustc inside requesting job containment.
    // Broker still owns one storage gateway; telemetry failure is isolated from
    // gateway health so a slow stats query cannot cancel active builds.
    sccache: { mode: managedSccache ? 'managed' : 'disabled', clientSide: managedSccache, lifecycleOwner: managedSccache ? 'broker' : null, requireOwnershipProof: managedSccache, ...(managedSccache ? { expectedVersion: expectedSccacheVersion } : {}), maxSizeBytes: 20 * 1024 ** 3, gatewayPort: 4_227, healthLeaseMs: 10_000, recoveryTimeoutMs: 10_000, statsIntervalMs: 60_000 },
    outputPolicy: { mode: 'enforce', allowedSourceDirs: [], scanRoot: 'repository' },
    bounds: defaultBounds(),
  };
}

export function renderPlist({ nodePath, brokerScript, configPath, home }) {
  const logs = join(home, 'Library', 'Logs', 'RightKit');
  const escape = (value) => value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
\t<key>EnvironmentVariables</key>
\t<dict>
\t\t<key>PATH</key>
\t\t<string>${escape(process.env.PATH ?? '/usr/bin:/bin:/usr/sbin:/sbin')}</string>
\t\t<key>RIGHTKIT_BUILD_BROKER_SOCKET</key>
\t\t<string>${escape(join(dirname(configPath), 'broker.sock'))}</string>
\t</dict>
\t<key>KeepAlive</key>
\t<true/>
\t<key>Label</key>
\t<string>${escape(LABEL)}</string>
\t<key>ProcessType</key>
\t<string>Background</string>
\t<key>ProgramArguments</key>
\t<array>
\t\t<string>${escape(nodePath)}</string>
\t\t<string>${escape(brokerScript)}</string>
\t\t<string>${escape(configPath)}</string>
\t</array>
\t<key>RunAtLoad</key>
\t<true/>
\t<key>StandardErrorPath</key>
\t<string>${escape(join(logs, 'build-control.stderr.log'))}</string>
\t<key>StandardOutPath</key>
\t<string>${escape(join(logs, 'build-control.stdout.log'))}</string>
</dict>
</plist>
`;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const externalRoot = resolve(options.externalRoot ?? '/Volumes/D/.rightkit-managed');
  const home = homedir();
  const configPath = join(externalRoot, 'config.json');
  const plistPath = join(home, 'Library', 'LaunchAgents', `${LABEL}.plist`);
  const nodePath = await realpath(options.nodePath ?? process.execPath);

  mkdirSync(externalRoot, { recursive: true });
  mkdirSync(join(externalRoot, 'state'), { recursive: true });
  mkdirSync(join(home, 'Library', 'LaunchAgents'), { recursive: true });
  mkdirSync(join(home, 'Library', 'Logs', 'RightKit'), { recursive: true });

  if (!existsSync(configPath) || options.force) {
    const config = await buildConfig({ externalRoot });
    let preserved = [];
    if (existsSync(configPath)) {
      let previous = null;
      try { previous = JSON.parse(stripBom(readFileSync(configPath, 'utf8'))); } catch { /* unreadable previous config: regenerate from defaults */ }
      preserved = preserveOperatorTunedConfig(config, previous);
    }
    writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);
    console.log(`bootstrap-macos: wrote ${configPath} (${config.tools.length} tools discovered)`);
    if (preserved.length) console.log(`bootstrap-macos: preserved operator-tuned ${preserved.join(', ')} from existing config`);
  } else {
    console.log(`bootstrap-macos: ${configPath} already exists; pass --force to regenerate`);
  }

  const activeConfig = JSON.parse(await readFile(configPath, 'utf8'));
  const loaderLinks = await ensureMacOSRustToolLoaderLinks(activeConfig.tools ?? [], activeConfig.defaults?.targetTriples ?? [activeConfig.defaults?.targetTriple]);
  if (loaderLinks.length) console.log(`bootstrap-macos: verified ${loaderLinks.length} macOS Rust loader companion link(s)`);

  const brokerScript = join(externalRoot, 'runtime-current', 'build-control-broker', 'src', 'broker.mjs');
  writeFileSync(plistPath, renderPlist({ nodePath, brokerScript, configPath, home }));
  console.log(`bootstrap-macos: wrote ${plistPath}`);

  const uid = Number((await execFile('id', ['-u'])).stdout.trim());
  const sourceRevision = (await execFile('git', ['-C', REPO_ROOT, 'rev-parse', 'HEAD'])).stdout.trim();
  const sourcePackagesRoot = join(REPO_ROOT, 'tools', 'rightkit', 'packages');
  const sourceTreeDigest = (await execFile('git', ['-C', REPO_ROOT, 'rev-parse', `HEAD:${resolveRelative(REPO_ROOT, sourcePackagesRoot)}`])).stdout.trim();

  const healthWorkspace = join(externalRoot, 'fixture-rust');
  if (!existsSync(healthWorkspace)) {
    console.log(`bootstrap-macos: ${healthWorkspace} is missing; the cutover health probe requires a tiny cargo workspace there — see docs/rules/rightkit-broker-recovery.md`);
    process.exitCode = 1;
    return;
  }

  const receipt = await runMacOSCutover({
    configPath, externalRoot, sourcePackagesRoot, sourceRevision, sourceTreeDigest,
    version: `bootstrap-${sourceRevision.slice(0, 8)}-${Date.now()}`,
    launchdLabel: LABEL, uid, endpoint: join(externalRoot, 'broker.sock'),
    plistPath, healthWorkspace, timeoutMs: options.timeoutMs, drainTimeoutMs: options.drainTimeoutMs,
  });
  console.log(JSON.stringify(receipt, null, 2));
  console.log('bootstrap-macos: broker activated');
}

export function resolveRelative(root, target) {
  const rel = target.startsWith(`${root}/`) ? target.slice(root.length + 1) : target;
  return rel.replaceAll('\\', '/');
}

if (isDirectMain()) main().catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
