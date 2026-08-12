import { createHash, randomUUID } from 'node:crypto';
import { spawn as nodeSpawn } from 'node:child_process';
import { lstat, mkdir, open, readFile, readdir, rename, rm, stat, statfs, unlink } from 'node:fs/promises';
import net from 'node:net';
import { dirname, isAbsolute, join, normalize, relative } from 'node:path';
import { externalRoots, immutableToolRoot, normalizeManagedAgentRequest, scanWorkspaceForUnmanagedOutputs, validateManagedRustEnvironment } from '@rightkit/build-control-core';

const byteCount = (value, field) => { if (!Number.isSafeInteger(value) || value < 0) throw new TypeError(`${field} must be a non-negative safe integer`); return value; };
const contained = (root, value) => { const diff = relative(root, value); return diff === '' || (!diff.startsWith('..') && !isAbsolute(diff)); };
const digest = (value) => createHash('sha256').update(value).digest('hex');
const dimension = (value, field) => { if (typeof value !== 'string' || !value) throw new TypeError(`${field} is required`); return value; };

/** State & execution runtime. Storage identity has already been attested by caller. */
export class BuildControlBroker {
  #root; #capacity; #used; #reservations = new Map(); #leases = new Map(); #entries = new Map(); #stateFile; #receiptsDir; #ready; #writes = Promise.resolve(); #reconciliation;
  constructor({ externalRoot, volumeId, capacityBytes, usedBytes = 0, stateFile, receiptsDir }) {
    if (!externalRoot || typeof externalRoot !== 'object') throw new TypeError('managed external root is required');
    if (!isAbsolute(externalRoot.path) || !externalRoot.volumeId || externalRoot.mounted !== true || externalRoot.writable !== true || externalRoot.internal === true) throw new TypeError('managed external root must be mounted, writable, external & volume-identified');
    this.#root = Object.freeze({ path: normalize(externalRoot.path), volumeId: String(externalRoot.volumeId) });
    if (volumeId !== undefined && volumeId !== this.#root.volumeId) throw new TypeError('external root volume identity mismatch');
    this.#capacity = byteCount(capacityBytes, 'capacityBytes'); this.#used = byteCount(usedBytes, 'usedBytes');
    if (this.#used > this.#capacity) throw new RangeError('used bytes exceed capacity');
    if (!stateFile || !receiptsDir) throw new TypeError('durable state file & receipts directory are required');
    this.#stateFile = normalize(stateFile); this.#receiptsDir = normalize(receiptsDir);
    if (!contained(this.#root.path, this.#stateFile)) throw new TypeError('state file must remain under external root');
    if (!contained(this.#root.path, this.#receiptsDir)) throw new TypeError('receipts directory must remain under external root');
    this.#ready = this.#load();
  }
  async ready() { await this.#ready; }
  async flush() { await this.#ready; await this.#writes; }
  get capacity() { return Object.freeze({ capacityBytes: this.#capacity, usedBytes: this.#used, reservedBytes: this.#reservedBytes(), availableBytes: this.#availableBytes(), volumeId: this.#root.volumeId }); }
  rootsFor({ repositoryIdentity, toolchain, targetTriple, buildProfile }) {
    const repository = dimension(repositoryIdentity, 'repositoryIdentity'); const tool = dimension(toolchain, 'toolchain'); const target = dimension(targetTriple, 'targetTriple'); const profile = dimension(buildProfile, 'buildProfile');
    const base = externalRoots({ externalRoot: this.#root.path, repositoryIdentity: repository, toolchain: tool, target, buildProfile: profile, policyProfile: 'MANAGED_AGENT' });
    const toolKey = digest(tool).slice(0, 24); const profileKey = digest(profile).slice(0, 24);
    const roots = { ...base, stage: join(base.build, 'stage'), target: join(base.targets, target, profileKey), cargoHome: base.cargoHome, packageCache: join(base.root, 'package-cache'), rustupHome: join(this.#root.path, 'rightkit-build-control', 'global', 'toolchains', toolKey), sccache: base.compilerCache, sccacheSocket: join(this.#root.path, '.rk-sccache.sock') };
    const pathValues = ['root', 'cargoHome', 'compilerCache', 'targets', 'build', 'temp', 'stage', 'target', 'packageCache', 'rustupHome', 'sccache', 'sccacheSocket'];
    if (!pathValues.every((key) => contained(this.#root.path, roots[key]))) throw new Error('computed root escapes managed external root');
    return Object.freeze(roots);
  }
  admit({ request, repositoryIdentity, toolchain, targetTriple, buildProfile, reservationBytes, target, writer = true, id = randomUUID() }) {
    const normalized = normalizeManagedAgentRequest(request); const reserve = byteCount(reservationBytes, 'reservationBytes'); const roots = this.rootsFor({ repositoryIdentity, toolchain, targetTriple, buildProfile });
    if (!id || this.#reservations.has(id)) throw new TypeError('unique request id is required');
    const targetName = dimension(target ?? targetTriple, 'target'); if (reserve > this.#availableBytes()) throw new RangeError('hard quota would be exceeded');
    const leaseKey = `${roots.namespace}:${targetName}:${buildProfile}`; if (writer && this.#leases.has(leaseKey)) throw new Error('exclusive writer lease conflict');
    const entry = Object.freeze({ id, request: normalized, repositoryIdentity, toolchain, targetTriple, buildProfile, target: targetName, writer: Boolean(writer), reservationBytes: reserve, leaseKey, roots, state: 'ACTIVE', admittedAt: new Date().toISOString() });
    this.#reservations.set(id, entry); this.#entries.set(id, entry); if (entry.writer) this.#leases.set(leaseKey, id); this.#queuePersist(entry); return entry;
  }
  release(id, { actualGrowthBytes = 0 } = {}) { const entry = this.#active(id); const growth = byteCount(actualGrowthBytes, 'actualGrowthBytes'); if (growth > entry.reservationBytes) throw new RangeError('actual growth exceeds reservation'); this.#finish(entry, 'RELEASED'); const value = Object.freeze({ ...entry, state: 'RELEASED', actualGrowthBytes: growth, lruAt: new Date().toISOString(), capacity: this.capacity }); this.#entries.set(id, value); this.#queuePersist(value); return value; }
  poison(id, reason) { const entry = this.#active(id); if (!['disconnect', 'cancel', 'unmanaged-output', 'execution-failure', 'reservation-overrun'].includes(reason)) throw new TypeError('recognized poison reason is required'); this.#finish(entry, 'POISONED'); const poisoned = Object.freeze({ ...entry, state: 'POISONED', poisonReason: reason, poisonedAt: new Date().toISOString(), lruAt: new Date().toISOString() }); this.#entries.set(id, poisoned); this.#queuePersist(poisoned); return poisoned; }
  gcCandidates() { return [...this.#entries.values()].filter((entry) => entry.state === 'POISONED' || entry.state === 'RELEASED').sort((a, b) => (a.lruAt ?? a.poisonedAt ?? a.admittedAt).localeCompare(b.lruAt ?? b.poisonedAt ?? b.admittedAt)).map((entry) => Object.freeze({ id: entry.id, namespace: entry.roots.namespace, leaseKey: entry.leaseKey, lruAt: entry.lruAt ?? entry.poisonedAt ?? entry.admittedAt, roots: entry.roots, reason: entry.poisonReason ?? 'released-retention', state: entry.state })); }
  async reconcileUsedBytes() { await this.ready(); const measured = await this.#measureUsedBytes(); this.#reconciliation = measured; if (measured.complete) this.#used = measured.bytes; this.#queuePersist({ id: `reconcile-${randomUUID()}`, state: 'RECONCILED', usedBytes: this.#used, complete: measured.complete, scannedEntries: measured.entries }); await this.flush(); return this.capacity; }
  executionEnvironment(id, extra = {}) { const entry = this.#active(id); if (!extra || typeof extra !== 'object' || Object.values(extra).some((value) => typeof value !== 'string')) throw new TypeError('extra environment values must be strings'); const r = entry.roots; const home = join(r.temp, 'home'); return Object.freeze({ ...baseEnvironment(), ...entry.request.environment, ...extra, CARGO_HOME: r.cargoHome, CARGO_TARGET_DIR: r.target, CARGO_BUILD_TARGET_DIR: r.target, CARGO_BUILD_BUILD_DIR: r.build, CARGO_INSTALL_ROOT: join(r.build, 'install'), RUSTUP_HOME: r.rustupHome, SCCACHE_DIR: r.sccache, ...(process.platform === 'win32' ? {} : { SCCACHE_SERVER_UDS: r.sccacheSocket }), npm_config_cache: r.packageCache, npm_config_store_dir: join(r.packageCache, 'pnpm-store'), npm_config_state_dir: join(r.packageCache, 'pnpm-state'), npm_config_node_gyp: r.temp, SWIFTPM_MODULECACHE_OVERRIDE: join(r.packageCache, 'swiftpm-module-cache'), SWIFTPM_PACKAGECACHE_PATH: join(r.packageCache, 'swiftpm-packages'), CMAKE_BUILD_DIR: r.target, HOME: home, XDG_CACHE_HOME: join(r.packageCache, 'xdg-cache'), XDG_CONFIG_HOME: join(home, '.config'), TMPDIR: r.temp, TMP: r.temp, TEMP: r.temp }); }
  #active(id) { const entry = this.#reservations.get(id); if (!entry) throw new Error('active reservation not found'); return entry; }
  #finish(entry, state) { this.#reservations.delete(entry.id); if (entry.writer && this.#leases.get(entry.leaseKey) === entry.id) this.#leases.delete(entry.leaseKey); this.#entries.set(entry.id, Object.freeze({ ...entry, state })); }
  #reservedBytes() { return [...this.#reservations.values()].reduce((total, entry) => total + entry.reservationBytes, 0); }
  #availableBytes() { return this.#capacity - this.#used - this.#reservedBytes(); }
  async #load() { try { const saved = JSON.parse(await readFile(this.#stateFile, 'utf8')); this.#used = Number.isSafeInteger(saved.usedBytes) ? byteCount(saved.usedBytes, 'persisted usedBytes') : 0; const recovered = []; let releasedReservationBytes = 0; for (const entry of saved.entries ?? []) if (entry?.id && ['ACTIVE', 'RELEASED', 'POISONED'].includes(entry.state)) { if (entry.state === 'ACTIVE') { const now = new Date().toISOString(); const reservationBytes = byteCount(entry.reservationBytes, 'recovered reservationBytes'); recovered.push(entry.id); releasedReservationBytes += reservationBytes; this.#entries.set(entry.id, Object.freeze({ ...entry, state: 'POISONED', poisonReason: 'broker-restart', poisonedAt: now, lruAt: now, recoveredReservationBytes: reservationBytes })); } else this.#entries.set(entry.id, Object.freeze(entry)); } this.#reconciliation = await this.#measureUsedBytes(); if (this.#reconciliation.complete) this.#used = this.#reconciliation.bytes; if (recovered.length) await this.#persist({ id: `recovery-${randomUUID()}`, state: 'RECOVERY', recovered, releasedReservationBytes, reconciliationComplete: this.#reconciliation.complete, scannedEntries: this.#reconciliation.entries }); } catch (error) { if (error.code !== 'ENOENT') throw error; this.#reconciliation = await this.#measureUsedBytes(); if (this.#reconciliation.complete) this.#used = this.#reconciliation.bytes; } }
  async #persist(receipt) { const state = { version: 1, usedBytes: this.#used, entries: [...this.#entries.values()] }; await durableWrite(this.#stateFile, `${JSON.stringify(state)}\n`); if (receipt) await durableWrite(join(this.#receiptsDir, `${receipt.id}.json`), `${JSON.stringify(receipt)}\n`); }
  #queuePersist(receipt) { this.#writes = this.#writes.then(() => this.#ready).then(() => this.#persist(receipt)); }
  async gcApply({ expiredBefore = null } = {}) { await this.ready(); const candidates = this.gcCandidates().filter((entry) => entry.state === 'POISONED' || (expiredBefore && entry.state === 'RELEASED' && entry.lruAt < expiredBefore)); const deleted = []; let reclaimedBytes = 0; const seenNamespaces = new Set(); for (const candidate of candidates) { if (seenNamespaces.has(candidate.namespace)) continue; seenNamespaces.add(candidate.namespace); const namespaceEntries = [...this.#entries.values()].filter((entry) => entry.roots?.namespace === candidate.namespace); const live = namespaceEntries.some((entry) => entry.state === 'ACTIVE' || (entry.state === 'RELEASED' && (!expiredBefore || (entry.lruAt ?? entry.admittedAt) >= expiredBefore))); if (!contained(this.#root.path, candidate.roots.root) || this.#leases.has(candidate.leaseKey) || live) continue; const bytes = await directorySize(candidate.roots.root); await rm(candidate.roots.root, { recursive: true, force: true }); for (const entry of namespaceEntries) { this.#entries.delete(entry.id); deleted.push(entry.id); } reclaimedBytes += bytes; } const measured = await this.#measureUsedBytes(); this.#reconciliation = measured; if (measured.complete) this.#used = measured.bytes; await this.#persist({ id: `gc-${randomUUID()}`, state: 'GC', reclaimedBytes, deleted, usedBytes: this.#used, complete: measured.complete, scannedEntries: measured.entries }); return { reclaimedBytes, deleted }; }
  async #measureUsedBytes() { return boundedDirectorySize(join(this.#root.path, 'rightkit-build-control'), { maxEntries: 20_000, deadlineMs: 1_000 }); }
}

export class LocalNdjsonBrokerService {
  #broker; #tools = new Map(); #spawn; #defaults; #outputPolicy; #bounds;
  constructor({ broker, tools = [], shims = {}, defaults = {}, outputPolicy = {}, bounds = {}, spawn = nodeSpawn }) { if (!(broker instanceof BuildControlBroker)) throw new TypeError('broker is required'); this.#broker = broker; this.#spawn = spawn; this.#defaults = Object.freeze({ reservationBytes: 0, writer: true, shims, ...defaults }); this.#outputPolicy = Object.freeze({ mode: outputPolicy.mode ?? 'enforce', allowedSourceDirs: outputPolicy.allowedSourceDirs ?? [] }); this.#bounds = Object.freeze({ pollIntervalMs: bounds.pollIntervalMs ?? 250, freeFloorBytes: bounds.freeFloorBytes ?? 0, quotaWatermarkBytes: bounds.quotaWatermarkBytes ?? null, retentionMs: bounds.retentionMs ?? null }); for (const tool of tools) this.registerTool(tool); }
  registerTool(tool) { const verified = immutableToolRoot(tool); this.#tools.set(tool.id ?? verified.path, Object.freeze({ id: tool.id ?? verified.path, ...verified })); }
  async handleNdjson(line, write, onDisconnect) {
    if (typeof line !== 'string' || typeof write !== 'function') throw new TypeError('NDJSON line & writer are required');
    let message;
    try { message = JSON.parse(line); } catch { throw new TypeError('invalid NDJSON message'); }
    return this.execute({ ...message, onDisconnect }, (event) => write(`${JSON.stringify(event)}\n`));
  }
  async execute(message, emit = () => {}) {
    await this.#broker.ready();
    let enriched;
    try { enriched = this.#enrich(message); } catch (error) { const value = { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: error.message }; emit(value); return value; }
    const tool = this.#tools.get(enriched.toolId);
    try {
      await rejectWorkspaceCargoConfiguration(enriched.request.workspace);
      if (!tool) throw new Error('unregistered tool'); const file = await readFile(tool.path); if (digest(file) !== tool.sha256) throw new Error('registered tool digest mismatch');
      const sccache = enriched.sccacheToolId ? this.#tools.get(enriched.sccacheToolId) : null;
      if (enriched.sccacheToolId && !sccache) throw new Error('unregistered sccache tool');
      if (sccache && digest(await readFile(sccache.path)) !== sccache.sha256) throw new Error('registered sccache digest mismatch');
      const rustc = await this.#verifiedOptionalTool(enriched.rustcToolId, 'rustc');
      const rustdoc = await this.#verifiedOptionalTool(enriched.rustdocToolId, 'rustdoc');
      const before = await localOutputs(enriched.request.workspace, this.#outputPolicy); if (before.length && this.#outputPolicy.mode === 'enforce') throw new Error(`unmanaged local outputs before execution: ${before.join(',')}`); await this.#broker.reconcileUsedBytes();
      const entry = this.#broker.admit(enriched); await this.#broker.flush(); await mkdirMany(directoryRoots(entry.roots)); const env = this.#broker.executionEnvironment(entry.id, { ...(sccache ? { RUSTC_WRAPPER: sccache.path } : {}), ...(rustc ? { RUSTC: rustc.path } : {}), ...(rustdoc ? { RUSTDOC: rustdoc.path } : {}) });
      return await this.#run(entry, tool, env, enriched, emit);
    } catch (error) { const value = { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: error.message }; emit(value); return value; }
  }
  #enrich(message) {
    if (!message || message.type !== 'MANAGED_AGENT' || message.protocol !== 'rightkit.build-control.client.v1') throw new TypeError('MANAGED_AGENT v1 payload is required');
    const toolId = this.#defaults.shims[message.shim]; if (!toolId) throw new Error(`no registered tool for shim ${message.shim ?? '<empty>'}`);
    const request = normalizeManagedAgentRequest(message.request);
    const enriched = { ...this.#defaults, ...message, toolId, request, repositoryIdentity: message.repositoryIdentity ?? this.#defaults.repositoryIdentity ?? request.workspace, toolchain: message.toolchain ?? this.#defaults.toolchain, targetTriple: message.targetTriple ?? this.#defaults.targetTriple, buildProfile: message.buildProfile ?? classifyBuildProfile(request.argv, this.#defaults.buildProfile), reservationBytes: message.reservationBytes ?? this.#defaults.reservationBytes };
    const roots = this.#broker.rootsFor(enriched);
    validateManagedRustEnvironment(message.observedEnvironment ?? {}, {
      CARGO_HOME: roots.cargoHome, CARGO_TARGET_DIR: roots.target, CARGO_BUILD_TARGET_DIR: roots.target,
      CARGO_BUILD_BUILD_DIR: roots.build, CARGO_INSTALL_ROOT: join(roots.build, 'install'), RUSTUP_HOME: roots.rustupHome
    });
    return enriched;
  }
  async #run(entry, tool, env, message, emit) {
    const beforeBytes = await managedSize(entry.roots);
    const child = this.#spawn(tool.path, entry.request.argv, { cwd: entry.request.workspace, env, stdio: ['ignore', 'pipe', 'pipe'] });
    let boundedOut = false; let polling = false; const poll = setInterval(() => { if (polling) return; polling = true; void (async () => { try { const growth = Math.max(0, (await managedSize(entry.roots)) - beforeBytes); const fs = await statfs(entry.roots.root).catch(() => null); const free = fs ? Number(fs.bavail) * Number(fs.bsize) : Infinity; if (growth > entry.reservationBytes || free < this.#bounds.freeFloorBytes) { boundedOut = true; child.kill('SIGTERM'); } } catch { boundedOut = true; child.kill('SIGTERM'); } finally { polling = false; } })(); }, this.#bounds.pollIntervalMs);
    const disconnect = () => { if (!child.killed) child.kill('SIGTERM'); try { this.#broker.poison(entry.id, 'disconnect'); } catch {} };
    const unsubscribe = message.onDisconnect?.(disconnect);
    child.stdout?.on('data', (data) => emit({ type: 'STDOUT', id: entry.id, data: data.toString() })); child.stderr?.on('data', (data) => emit({ type: 'STDERR', id: entry.id, data: data.toString() }));
    try {
      const result = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', (code, signal) => resolve({ code, signal })); });
      clearInterval(poll); const growth = Math.max(0, (await managedSize(entry.roots)) - beforeBytes); const after = await localOutputs(entry.request.workspace, this.#outputPolicy); const failed = boundedOut || growth > entry.reservationBytes || result.code !== 0 || (after.length && this.#outputPolicy.mode === 'enforce');
      if (failed) { const reason = boundedOut || growth > entry.reservationBytes ? 'reservation-overrun' : after.length ? 'unmanaged-output' : 'execution-failure'; const receipt = this.#broker.poison(entry.id, reason); await this.#broker.reconcileUsedBytes(); await this.#autoGc(); await this.#broker.flush(); const value = Object.freeze({ type: 'RESULT', id: entry.id, exitCode: reason === 'execution-failure' ? validExit(result.code) : 125, status: 'POISONED', receipt, violations: after, actualGrowthBytes: growth }); emit(value); return value; }
      const receipt = this.#broker.release(entry.id, { actualGrowthBytes: growth }); await this.#broker.reconcileUsedBytes(); await this.#autoGc(); await this.#broker.flush(); const value = Object.freeze({ type: 'RESULT', id: entry.id, exitCode: 0, status: 'RELEASED', receipt, violations: after }); emit(value); return value;
    } catch (error) { clearInterval(poll); try { this.#broker.poison(entry.id, 'execution-failure'); } catch {} const value = { type: 'RESULT', id: entry.id, exitCode: 125, status: 'POISONED', error: error.message }; emit(value); return value; } finally { if (typeof unsubscribe === 'function') unsubscribe(); }
  }
  async #verifiedOptionalTool(id, label) { if (!id) return null; const tool = this.#tools.get(id); if (!tool) throw new Error(`unregistered ${label} tool`); if (digest(await readFile(tool.path)) !== tool.sha256) throw new Error(`registered ${label} digest mismatch`); return tool; }
  async #autoGc() { if (this.#bounds.quotaWatermarkBytes === null && this.#bounds.retentionMs === null) return; const expiredBefore = this.#bounds.retentionMs === null ? null : new Date(Date.now() - this.#bounds.retentionMs).toISOString(); if (this.#bounds.quotaWatermarkBytes === null || this.#broker.capacity.usedBytes >= this.#bounds.quotaWatermarkBytes || expiredBefore) await this.#broker.gcApply({ expiredBefore }); }
}
async function mkdirMany(paths) { await Promise.all([...new Set(paths)].map((item) => mkdir(item, { recursive: true }))); }
function directoryRoots(roots) { return [...['root', 'cargoHome', 'compilerCache', 'targets', 'build', 'temp', 'stage', 'target', 'packageCache', 'rustupHome', 'sccache'].map((key) => roots[key]), dirname(roots.sccacheSocket)]; }
const baseEnvironment = () => Object.fromEntries(['PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'SystemRoot', 'ComSpec'].flatMap((key) => process.env[key] === undefined ? [] : [[key, process.env[key]]]));
const validExit = (code) => Number.isInteger(code) && code >= 0 && code <= 255 ? code : 125;
function classifyBuildProfile(argv, fallback = 'development') { const profile = argv.find((value) => value.startsWith('--profile='))?.slice('--profile='.length); if (profile) return profile; const index = argv.indexOf('--profile'); if (index >= 0 && argv[index + 1]) return argv[index + 1]; if (argv.includes('--release')) return 'release'; return ['test', 'bench', 'nextest'].includes(argv.find((value) => !value.startsWith('-'))) ? 'test' : fallback; }
async function localOutputs(workspace, policy) { return scanWorkspaceForUnmanagedOutputs(workspace, { allowedRelativePaths: policy.allowedSourceDirs }); }
const cargoConfigurationRedirect = /(?:^|\n)\s*(?:\[alias\]|(?:target-dir|build-dir|rustc|rustdoc|rustc-wrapper|rustc-workspace-wrapper|rustflags|rustdocflags)\s*=)/im;
export async function rejectWorkspaceCargoConfiguration(workspace, { maxBytes = 64 * 1024 } = {}) {
  for (const name of ['config', 'config.toml']) {
    const candidate = join(workspace, '.cargo', name);
    const metadata = await lstat(candidate).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error));
    if (!metadata) continue;
    if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > maxBytes) throw new Error(`managed Cargo configuration is not an admissible bounded file: .cargo/${name}`);
    if (cargoConfigurationRedirect.test(await readFile(candidate, 'utf8'))) throw new Error(`managed Cargo configuration cannot redirect tools, outputs, flags, or aliases: .cargo/${name}`);
  }
}
const boundedIo = (promise, deadlineMs) => Promise.race([promise, new Promise((_, reject) => setTimeout(() => reject(Object.assign(new Error('managed filesystem operation timed out'), { code: 'RK_FS_TIMEOUT' })), deadlineMs))]);
async function directorySize(root) { const measured = await boundedDirectorySize(root, { maxEntries: 20_000, deadlineMs: 1_000 }); if (!measured.complete) throw new Error('managed filesystem size scan exceeded bounds'); return measured.bytes; }
export async function boundedDirectorySize(root, { maxEntries, deadlineMs, operations = { stat, readdir } }) { let bytes = 0; let entries = 0; const deadline = Date.now() + deadlineMs; const remaining = () => Math.max(1, deadline - Date.now()); const stack = [root]; while (stack.length) { if (entries >= maxEntries || Date.now() >= deadline) return { bytes, entries, complete: false }; const item = stack.pop(); let metadata; try { metadata = await boundedIo(operations.stat(item), remaining()); } catch (error) { if (error.code === 'ENOENT') continue; if (error.code === 'RK_FS_TIMEOUT') return { bytes, entries, complete: false }; throw error; } entries += 1; if (!metadata.isDirectory()) { bytes += metadata.size; continue; } let children; try { children = await boundedIo(operations.readdir(item), remaining()); } catch (error) { if (error.code === 'RK_FS_TIMEOUT') return { bytes, entries, complete: false }; throw error; } for (const child of children) stack.push(join(item, child)); } return { bytes, entries, complete: true }; }
async function managedSize(roots) { const measured = await boundedDirectorySize(roots.root, { maxEntries: 20_000, deadlineMs: 1_000 }); if (!measured.complete) throw new Error('managed namespace size scan exceeded bounds'); return measured.bytes; }
async function durableWrite(path, data) { await mkdir(dirname(path), { recursive: true }); const temporary = `${path}.${randomUUID()}.tmp`; const handle = await open(temporary, 'wx', 0o600); try { await handle.writeFile(data, 'utf8'); await handle.sync(); } finally { await handle.close(); } await rename(temporary, path); const directory = await open(dirname(path), 'r'); try { await directory.sync(); } catch (error) { if (!['EINVAL', 'ENOTSUP', 'EPERM'].includes(error.code)) throw error; } finally { await directory.close(); } }
export const MAX_REQUEST_BYTES = 1024 * 1024;
export async function removeStaleUnixSocket(endpoint) { const info = await lstat(endpoint).catch((error) => error.code === 'ENOENT' ? null : Promise.reject(error)); if (!info) return false; if (!info.isSocket()) throw new Error('refusing to remove non-socket broker endpoint'); await unlink(endpoint); return true; }
export function createLocalNdjsonServer({ service, endpoint, maxRequestBytes = MAX_REQUEST_BYTES }) { if (!(service instanceof LocalNdjsonBrokerService) || !endpoint) throw new TypeError('service & endpoint are required'); if (!Number.isSafeInteger(maxRequestBytes) || maxRequestBytes < 1) throw new TypeError('positive request input cap is required'); return net.createServer((socket) => { let buffer = ''; let handled = false; const reject = (error) => socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 125, status: 'REJECTED', error })}\n`); socket.on('data', async (chunk) => { if (handled) return reject('one request per connection is required'); buffer += chunk.toString(); if (Buffer.byteLength(buffer, 'utf8') > maxRequestBytes) { handled = true; return reject('request exceeds input cap'); } const index = buffer.indexOf('\n'); if (index < 0) return; handled = true; const line = buffer.slice(0, index); if (!line || buffer.slice(index + 1).trim()) return reject('one non-empty request per connection is required'); try { await service.handleNdjson(line, (value) => socket.write(value), (handler) => { socket.once('close', handler); return () => socket.off('close', handler); }); socket.end(); } catch (error) { reject(error.message); } }); }); }
