#!/usr/bin/env node
import { execFile as nodeExecFile } from 'node:child_process';
import { rename, readFile, readdir, realpath, writeFile } from 'node:fs/promises';
import { arch, hostname } from 'node:os';
import { join, resolve, win32 } from 'node:path';
import { promisify } from 'node:util';
import { randomUUID } from 'node:crypto';
import { clearCacheProofTarget, initializeCacheProofTarget } from './cache-proof.mjs';
import { classifyCacheReuse } from './cache-telemetry.mjs';
import { thinCacheStatsDelta } from './thin-cache.mjs';

const execFile = promisify(nodeExecFile);

export function parseCacheProofArgs(argv) {
  const options = { iterations: 3 };
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === '--external-root') { options.externalRoot = argv[++index]; continue; }
    if (value === '--rightkit') { options.rightkit = argv[++index]; continue; }
    if (value === '--iterations') { options.iterations = Number(argv[++index]); continue; }
    throw new TypeError(`unknown cache proof argument: ${value}`);
  }
  if (!Number.isSafeInteger(options.iterations) || options.iterations < 1 || options.iterations > 10) throw new TypeError('--iterations must be an integer from 1 to 10');
  return options;
}

export function isDirectCacheProofMain({ moduleUrl = import.meta.url, argv1 = process.argv[1], platform = process.platform } = {}) {
  if (typeof argv1 !== 'string' || !argv1) return false;
  const modulePath = decodeURIComponent(new URL(moduleUrl).pathname);
  if (platform === 'win32') return win32.normalize(modulePath.replace(/^\/(?=[A-Za-z]:)/, '')).toLowerCase() === win32.normalize(argv1).toLowerCase();
  return resolve(modulePath) === resolve(argv1);
}

async function liveFixtureLayout(managedRoot, fixtureWorkspace) {
  const workspaceStore = join(managedRoot, 'rightkit-build-control', 'workspaces');
  const canonical = await realpath(fixtureWorkspace);
  for (const entry of await readdir(workspaceStore, { withFileTypes: true })) {
    if (!entry.isDirectory() || !/^[a-f0-9]{32}$/.test(entry.name)) continue;
    const directory = join(workspaceStore, entry.name);
    const identity = await readFile(join(directory, 'identity.json'), 'utf8').then(JSON.parse).catch(() => null);
    if (identity?.canonicalRoot !== canonical) continue;
    const state = JSON.parse(await readFile(join(directory, 'state.json'), 'utf8'));
    if (!Number.isSafeInteger(state.currentGeneration) || state.currentGeneration < 1 || state.active || (state.residents?.length ?? 0) > 0) throw new Error('fixture workspace is not idle');
    return { target: join(directory, 'generations', String(state.currentGeneration).padStart(6, '0'), 'target'), state };
  }
  throw new Error('managed fixture workspace has no broker generation; run one RightKit Cargo command first');
}

async function assertBrokerIdle(managedRoot) {
  const active = await readdir(join(managedRoot, 'rightkit-build-control', 'global', 'active')).catch((error) => error.code === 'ENOENT' ? [] : Promise.reject(error));
  if (active.some((entry) => entry.endsWith('.json'))) throw new Error(`cache proof requires idle broker; active roots: ${active.join(',')}`);
}

function cacheEnvironment(config, managedRoot) {
  const tool = config.tools.find((entry) => entry.id === config.defaults.sccacheToolId);
  if (!tool) throw new Error('configured sccache tool is missing');
  const environment = {
    ...process.env,
    SCCACHE_DIR: join(managedRoot, 'rightkit-build-control', 'global', 'compiler-cache'),
    SCCACHE_CACHE_SIZE: String(config.sccache.maxSizeBytes),
    SCCACHE_CLIENT_SIDE: '1', SCCACHE_START_SERVER: '0', SCCACHE_NO_DAEMON: '1', SCCACHE_LOCAL_RW_MODE: 'READ_WRITE'
  };
  if (process.platform === 'win32') environment.SCCACHE_SERVER_PORT = String(config.sccache.gatewayPort);
  else environment.SCCACHE_SERVER_UDS = join(managedRoot, 'tmp', 'sccache-gateway.sock');
  return { tool: tool.path, environment };
}

async function stats(cache) {
  const { stdout } = await execFile(cache.tool, ['--show-stats', '--stats-format=json'], { env: cache.environment, encoding: 'utf8', maxBuffer: 256 * 1024 });
  return JSON.parse(stdout).stats;
}

async function runFixture(rightkitScript, fixtureWorkspace) {
  const result = await execFile(process.execPath, [rightkitScript, 'cargo', 'build', '--lib'], { cwd: fixtureWorkspace, encoding: 'utf8', maxBuffer: 8 * 1024 ** 2 });
  const id = result.stderr.match(/rightkit: request ([A-Za-z0-9-]+)/)?.[1] ?? null;
  if (!id) throw new Error('RightKit cache proof did not receive request id');
  return { id, stdout: result.stdout, stderr: result.stderr };
}

export async function runCacheProof({ externalRoot = process.platform === 'win32' ? 'D:\\.rightkit-managed' : '/Volumes/D/.rightkit-managed', rightkit = join(externalRoot, 'runtime-current', 'build-control-clients', 'src', 'rightkit.mjs'), iterations = 3 } = {}) {
  const managedRoot = resolve(externalRoot); const fixtureWorkspace = join(managedRoot, 'fixture-rust');
  const config = JSON.parse(await readFile(join(managedRoot, 'config.json'), 'utf8')); const cache = cacheEnvironment(config, managedRoot);
  await assertBrokerIdle(managedRoot);
  await runFixture(rightkit, fixtureWorkspace);
  let layout = await liveFixtureLayout(managedRoot, fixtureWorkspace);
  const targetOptions = { target: layout.target, expectedTarget: layout.target, managedRoot, fixtureWorkspace };
  await initializeCacheProofTarget(targetOptions); await clearCacheProofTarget(targetOptions);
  const prime = await runFixture(rightkit, fixtureWorkspace); await assertBrokerIdle(managedRoot);
  const samples = [];
  for (let index = 1; index <= iterations; index += 1) {
    layout = await liveFixtureLayout(managedRoot, fixtureWorkspace);
    if (layout.target !== targetOptions.target) throw new Error('fixture generation rotated during cache proof');
    await clearCacheProofTarget(targetOptions); const before = await stats(cache);
    const run = await runFixture(rightkit, fixtureWorkspace); await assertBrokerIdle(managedRoot);
    const after = await stats(cache); const delta = thinCacheStatsDelta(before, after); const reuse = classifyCacheReuse(delta);
    const sample = Object.freeze({ iteration: index, requestId: run.id, reuse, delta }); samples.push(sample);
    if (reuse.status !== 'PROVEN' || reuse.hits < 1 || reuse.misses !== 0) throw Object.assign(new Error(`cache proof iteration ${index} failed: ${reuse.status}, ${reuse.hits} hits, ${reuse.misses} misses`), { sample });
  }
  const report = Object.freeze({ schema: 'rightkit.cache-proof.v1', status: 'PROVEN', generatedAt: new Date().toISOString(), host: { name: hostname(), platform: process.platform, arch: arch() }, toolchain: config.defaults.toolchain, targetTriple: config.defaults.targetTriple, fixtureWorkspace, target: targetOptions.target, primeRequestId: prime.id, iterations: Object.freeze(samples) });
  const reportPath = join(managedRoot, 'rightkit-build-control', 'cache-proof-latest.json'); const temporary = `${reportPath}.${randomUUID()}.tmp`;
  await writeFile(temporary, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 }); await rename(temporary, reportPath);
  return { report, reportPath };
}

if (isDirectCacheProofMain()) {
  runCacheProof(parseCacheProofArgs(process.argv.slice(2))).then(({ report, reportPath }) => process.stdout.write(`${JSON.stringify({ ...report, reportPath }, null, 2)}\n`)).catch((error) => { process.stderr.write(`${error.stack ?? error}\n`); process.exitCode = 1; });
}
