import { createHash, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { execFile as nodeExecFile, spawn as nodeSpawn } from 'node:child_process';
import { lstat, readFile, realpath, statfs as nodeStatfs } from 'node:fs/promises';
import net from 'node:net';
import { cpus, freemem, homedir, totalmem } from 'node:os';
import { safeResourceSnapshot, summarizeBuildResources } from './resource-telemetry.mjs';
import { basename, delimiter, dirname, isAbsolute, join, normalize, relative, sep } from 'node:path';
import { promisify } from 'node:util';
import {
  BUILD_CONTROL_PROTOCOL,
  SOURCE_RUNTIME_IDENTITY,
  assertRepositoryExecutionAllowed,
  injectBrokerEnvironment,
  normalizeCargoTargetArgv,
  normalizeCompilerOutputArgv,
  normalizeDelegatedCompilerEnvironment,
  normalizeManagedAgentRequest,
  normalizeRequestEnvironment,
  normalizeRuntimeIdentity,
  resolveEffectiveJobs,
  validateManagedRustEnvironment,
  validateManagedRustupInvocation
} from '@rightkit/build-control-core';
import { ThinCache } from './thin-cache.mjs';
import { NativeOpenSslCache } from './native-openssl.mjs';
import { cargoCohort } from './cache-telemetry.mjs';
import { CargoFreshnessTracker, classifyColdStart, fingerprintCensus, fingerprintCensusStart, probeTargetAtStart, targetConsoleLine, wantsCargoJson } from './cargo-freshness.mjs';
import { ThinCommandPolicy, compileClass } from './thin-command-policy.mjs';
import { ThinLogger } from './thin-logger.mjs';
import { ThinScheduler, deriveCompileCosts } from './thin-scheduler.mjs';
import { acquireThinBrokerSingleton } from './thin-singleton.mjs';
import { ThinState } from './thin-state.mjs';
import { cutoverDrainPending } from './cutover-shared.mjs';
import { ImmutableToolRegistry, ToolChangedError } from './tool-registry.mjs';
import { rejectWorkspaceCargoConfiguration } from './utilities.mjs';
import {
  managedProcessOwnership,
  readManagedProcessIdentity,
  readManagedProcessResources,
  reapPersistedProcessTree,
  spawnManagedProcess,
  startManagedProcess,
  terminateProcessTree
} from './process-tree.mjs';

const TERMINAL = new Set(['SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT']);
const PRIORITIES = new Set(['BATCH', 'INTERACTIVE', 'INSTALLER']);
const DEFAULT_OUTPUT_BYTES = 8 * 1024 * 1024;
const CANCELLATION_SOURCES = new Set(['USER_CANCEL', 'OPERATOR_CANCEL', 'CLIENT_SIGNAL', 'PARENT_CANCEL', 'BROKER_SHUTDOWN', 'DEADLINE', 'STORAGE_FLOOR', 'CONTAINMENT_FAILURE', 'RELEASE_PRIORITY_CANCEL', 'UNKNOWN']);
const execFile = promisify(nodeExecFile);

function cacheBuildProfile(shim, argv) {
  if (shim !== 'cargo') return 'release';
  const separator = argv.indexOf('--');
  const cargoArgv = separator < 0 ? argv : argv.slice(0, separator);
  return cargoCohort({ argv: cargoArgv, profile: 'release' }).profile === 'release-iterate'
    ? 'release-iterate' : 'release';
}

const integer = (value, fallback, minimum = 0) => {
  const selected = value ?? fallback;
  if (!Number.isSafeInteger(selected) || selected < minimum) throw new TypeError(`integer >= ${minimum} is required`);
  return selected;
};

function compilationTarget(shim, argv) {
  if (!['cargo', 'rustc', 'rustdoc'].includes(shim)) return null;
  let target = null;
  for (let index = 0; index < argv.length && argv[index] !== '--'; index += 1) {
    const value = argv[index]; let found = null;
    if (value === '--target') found = argv[++index];
    else if (value.startsWith('--target=')) found = value.slice('--target='.length);
    if (found !== null) {
      if (!found || (target && target !== found)) throw new TypeError('one non-empty Cargo target is required');
      target = found;
    }
  }
  return target;
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const safeExit = (code, status) => status === 'SUCCEEDED' ? 0 : Number.isInteger(code) && code >= 1 && code <= 255 ? code : 125;
const appendTail = (current, chunk, limit) => chunk.length >= limit ? Buffer.from(chunk.subarray(chunk.length - limit)) : Buffer.concat([current, chunk]).subarray(Math.max(0, current.length + chunk.length - limit));
const stripAnsi = (value) => value.replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, '');
const pathIsWithin = (root, candidate) => {
  if (typeof root !== 'string' || !root) return false;
  const childPath = relative(normalize(root), candidate);
  return childPath === '' || (childPath !== '..' && !childPath.startsWith(`..${sep}`) && !isAbsolute(childPath));
};
const BROKER_ENDPOINT_ENVIRONMENT_KEY = 'RIGHTKIT_BUILD_BROKER_SOCKET';

export function brokerEndpointForPlatform(config, platform = process.platform) {
  const field = platform === 'win32' ? 'windowsPipe' : 'unixSocket';
  const endpoint = config?.[field];
  if (typeof endpoint !== 'string' || !endpoint) throw new TypeError(`validated broker ${field} is required`);
  return endpoint;
}

function brokerOwnedRuntimeEnvironment(environment, endpoint) {
  const runtime = Object.fromEntries(Object.entries(environment).filter(([key]) => key.toUpperCase() !== BROKER_ENDPOINT_ENVIRONMENT_KEY));
  runtime[BROKER_ENDPOINT_ENVIRONMENT_KEY] = endpoint;
  return runtime;
}

export async function resolveCargoWorkspace(workspace, { cargoPath, run = execFile, environment = process.env, timeoutMs = 30_000 } = {}) {
  if (typeof workspace !== 'string' || !isAbsolute(workspace) || typeof cargoPath !== 'string' || !cargoPath) throw new TypeError('absolute workspace & cargoPath are required');
  const executionCwd = await realpath(workspace);
  const startedAt = Date.now();
  let result;
  try {
    result = await run(cargoPath, ['locate-project', '--workspace', '--message-format', 'plain'], { cwd: executionCwd, env: environment, encoding: 'utf8', timeout: timeoutMs, maxBuffer: 64 * 1024 });
  } catch (error) {
    const timedOut = error?.killed === true || error?.signal === 'SIGTERM' || error?.code === 'ETIMEDOUT';
    if (timedOut) throw Object.assign(new Error(`Cargo workspace discovery exceeded ${timeoutMs}ms for ${executionCwd}`), { code: 'RK_WORKSPACE_DISCOVERY_TIMEOUT', timeoutMs, workspace: executionCwd, cause: error });
    const detail = String(error?.stderr ?? error?.message ?? '').trim().slice(0, 2_048);
    throw Object.assign(new Error(`Cargo workspace discovery failed for ${executionCwd}${detail ? `: ${detail}` : ''}`), { code: 'RK_WORKSPACE_DISCOVERY_FAILED', workspace: executionCwd, cause: error });
  }
  const manifest = String(result?.stdout ?? result).trim();
  if (!isAbsolute(manifest) || basename(manifest) !== 'Cargo.toml') throw Object.assign(new Error('Cargo workspace root could not be resolved'), { code: 'RK_WORKSPACE_UNRESOLVED' });
  return Object.freeze({ executionCwd, workspaceRoot: await realpath(dirname(manifest)), workspaceDiscoveryMs: Date.now() - startedAt });
}

// Bounded, cached, observable workspace discovery (v2.7 §7 / §22.4): `cargo
// locate-project --workspace` is classified CONTROL and must not become a hidden
// 10-second BUILD-lane blocker. The TTL bounds re-resolution; a low, explicit
// timeout bounds any single invocation. Empty/negative TTL disables caching.
export class CachedWorkspaceResolver {
  #next; #ttlMs; #timeoutMs; #concurrency; #active = 0; #waiting = []; #inflight = new Map(); #stats;
  constructor({ ttlMs = 10 * 60_000, timeoutMs = 30_000, concurrency = 4, now = Date.now, resolver = resolveCargoWorkspace } = {}) {
    this.#ttlMs = Number.isSafeInteger(ttlMs) && ttlMs > 0 ? ttlMs : 0;
    this.#timeoutMs = Number.isSafeInteger(timeoutMs) && timeoutMs > 0 ? timeoutMs : 30_000;
    this.#concurrency = Number.isSafeInteger(concurrency) && concurrency > 0 ? concurrency : 4;
    this.#stats = { lookup: 0, hit: 0, miss: 0, error: 0 };
    if (typeof resolver !== 'function') throw new TypeError('workspace resolver must be a function');
    this.#next = resolver;
  }
  get stats() { return Object.freeze({ ...this.#stats, active: this.#active, queued: this.#waiting.length, inflight: this.#inflight.size }); }
  async resolve(workspace, options = {}) {
    const executionCwd = await (options.realpath ?? realpath)(workspace);
    const now = options.now ?? Date.now;
    const key = `${this.#ttlMs}|${executionCwd}`;
    const cached = this.#cache.get(key);
    this.#stats.lookup += 1;
    if (cached && now() < cached.expiresAt && this.#ttlMs > 0) { this.#stats.hit += 1; return Object.freeze({ ...cached.value, workspaceDiscoveryMs: 0, workspaceDiscoveryCache: 'HIT' }); }
    this.#stats.miss += 1;
    if (this.#inflight.has(key)) return this.#inflight.get(key);
    const pending = this.#bounded(async () => {
      const value = await this.#next(executionCwd, { ...options, timeoutMs: this.#timeoutMs });
      const selected = Object.freeze({ ...value, workspaceDiscoveryCache: 'MISS' });
      if (this.#ttlMs > 0) this.#cache.set(key, { value: selected, expiresAt: now() + this.#ttlMs });
      return selected;
    });
    this.#inflight.set(key, pending);
    try {
      return await pending;
    } catch (error) { this.#stats.error += 1; throw error; }
    finally { this.#inflight.delete(key); }
  }
  async #bounded(action) {
    if (this.#active >= this.#concurrency) await new Promise((resolve) => this.#waiting.push(resolve));
    this.#active += 1;
    try { return await action(); }
    finally { this.#active -= 1; this.#waiting.shift()?.(); }
  }
  #cache = new Map();
}

export async function resolveRepositoryRoot(workspace, { inspect = lstat } = {}) {
  const fallback = await realpath(workspace); let candidate = fallback;
  while (true) {
    for (const marker of ['.rightkit-local-development.json', '.git']) {
      try { await inspect(join(candidate, marker)); return candidate; }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    const parent = dirname(candidate); if (parent === candidate) return fallback; candidate = parent;
  }
}

function selectedToolIds(config) {
  const ids = new Set(Object.values(config.shims ?? {}).filter(Boolean));
  for (const entry of config.externalCommands ?? []) ids.add(entry.toolId);
  if (config.defaults?.sccacheToolId) ids.add(config.defaults.sccacheToolId);
  return ids;
}

function registeredRustupContext(cargoPath) {
  const bin = dirname(cargoPath);
  const toolchainRoot = dirname(bin);
  const toolchainsRoot = dirname(toolchainRoot);
  if (basename(bin).toLowerCase() !== 'bin' || basename(toolchainsRoot).toLowerCase() !== 'toolchains') return Object.freeze({ toolchain: null, home: null });
  return Object.freeze({ toolchain: basename(toolchainRoot) || null, home: dirname(toolchainsRoot) });
}

function runtimeBase(toolchainLibraryPath, rustupToolchain, platform = process.platform) {
  const environment = Object.fromEntries(['PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'SystemRoot', 'ComSpec'].flatMap((key) => process.env[key] === undefined ? [] : [[key, process.env[key]]]));
  // Account identity is selected by the broker, never accepted from callers.
  // Git uses HOME to load sanctioned per-user SSH URL routing.
  if (platform === 'darwin') environment.HOME = homedir();
  if (rustupToolchain) environment.RUSTUP_TOOLCHAIN = rustupToolchain;
  if (platform === 'darwin' && toolchainLibraryPath) environment.DYLD_FALLBACK_LIBRARY_PATH = toolchainLibraryPath;
  return environment;
}

function hostResourceSnapshot() {
  return { logicalCpuCount: cpus().length, totalMemoryBytes: totalmem(), freeMemoryBytes: freemem() };
}

export class ThinJobService {
  #config; #runtime; #state; #cache; #policy; #scheduler; #logger; #singleton; #registry; #toolchainLibraryPath; #rustupToolchain; #rustupHome; #compilerEnvironment; #compilerIdentity; #spawn; #workspaceResolver; #repositoryResolver; #identity; #ownership; #start; #terminate; #readResources; #readHostResources; #statfs; #now; #platform; #nativeOpenSsl; #jobs = new Map(); #admissions = new Map(); #stopping = false;

  static async create({ config, runtimeIdentity = SOURCE_RUNTIME_IDENTITY, registry = null, scheduler = null, state = null, cache = null, logger = null, singleton = null, spawn = nodeSpawn, workspaceResolver = resolveCargoWorkspace, repositoryResolver = resolveRepositoryRoot, processIdentity = readManagedProcessIdentity, processOwnership = managedProcessOwnership, startProcess = startManagedProcess, terminateProcess = terminateProcessTree, readResources = readManagedProcessResources, readHostResources = hostResourceSnapshot, reapPersisted = reapPersistedProcessTree, statfs = nodeStatfs, now = Date.now, platform = process.platform, nativeOpenSsl = undefined } = {}) {
    if (!config?.broker?.externalRoot?.path) throw new TypeError('validated broker config is required');
    const externalRoot = normalize(config.broker.externalRoot.path);
    let ownedSingleton = singleton;
    let ownedLogger = logger;
    let ownedCache = cache;
    try {
      ownedSingleton ??= await acquireThinBrokerSingleton({ stateRoot: join(externalRoot, 'rightkit-build-control', 'state') });
      const thinState = state ?? await ThinState.open({ externalRoot });
      const toolRegistry = registry ?? new ImmutableToolRegistry();
      if (!registry) for (const tool of config.tools ?? []) if (selectedToolIds(config).has(tool.id)) await toolRegistry.register(tool);
      const cargoTool = await toolRegistry.verify(config.shims?.cargo);
      const rustcTool = await toolRegistry.verify(config.defaults?.rustcToolId ?? config.shims?.rustc);
      const rustdocTool = await toolRegistry.verify(config.defaults?.rustdocToolId ?? config.shims?.rustdoc);
      const sccacheTool = config.defaults?.sccacheToolId ? await toolRegistry.verify(config.defaults.sccacheToolId) : null;
      const thinLogger = logger ?? await ThinLogger.open({ path: join(thinState.paths.logs, 'events.ndjson') });
      ownedLogger = thinLogger;
      let service = null;
      const thinCache = cache ?? new ThinCache({
        paths: thinState.paths,
        enabled: config.sccache?.mode !== 'disabled' && Boolean(sccacheTool),
        sccachePath: sccacheTool?.path ?? null,
        verifyTool: config.defaults?.sccacheToolId ? () => toolRegistry.verify(config.defaults.sccacheToolId) : null,
        expectedVersion: config.sccache?.expectedVersion ?? null,
        clientSideSupported: config.sccache?.clientSide === true,
        maxSizeBytes: config.sccache?.maxSizeBytes,
        gatewayPort: config.sccache?.gatewayPort,
        healthLeaseMs: config.sccache?.healthLeaseMs,
        recoveryTimeoutMs: config.sccache?.recoveryTimeoutMs,
        statsIntervalMs: config.sccache?.statsIntervalMs,
        // v2.7 §12: SCCACHE_BASEDIRS is gated behind explicit experiment enablement.
        basedirs: config.sccache?.basedirs ?? null,
        basedirsEnabled: config.sccache?.basedirsEnabled === true,
        platform, spawn, terminate: terminateProcess,
        emit: (event) => thinLogger.append(event),
        onUnavailable: (event) => service?.cacheUnavailable(event)
      });
      ownedCache = thinCache;
      const thinScheduler = scheduler ?? new ThinScheduler({
        controlConcurrency: integer(config.bounds?.controlConcurrency, 4, 1),
        analysisConcurrency: integer(config.bounds?.analysisConcurrency, 2, 1),
        workspaceWriteConcurrency: integer(config.bounds?.workspaceWriteConcurrency, 4, 1),
        buildConcurrency: integer(config.bounds?.buildConcurrency, 3, 1),
        // Aggregate compiler budget (dual-host routing §5). With host sizing the
        // budget is GiB of build-allocatable memory & buildCostUnits weights each
        // build by its jobs; Windows freemem() is available physical memory, so it
        // also guards live admission. Darwin freemem() excludes reclaimable pages.
        compileBudget: config.bounds?.compileBudgetUnits ?? null,
        costUnitBytes: config.bounds?.buildCostUnits ? 1024 ** 3 : null,
        hostAvailableMemory: config.bounds?.buildCostUnits && process.platform === 'win32' ? freemem : null,
        now
      });
      // Bound, cache & make observable the CONTROL-class workspace discovery.
      const discoveryResolver = workspaceResolver instanceof CachedWorkspaceResolver
        ? workspaceResolver
        : new CachedWorkspaceResolver({ resolver: workspaceResolver, ttlMs: integer(config.bounds?.workspaceDiscoveryTtlMs, 10 * 60_000, 0), timeoutMs: integer(config.bounds?.workspaceDiscoveryTimeoutMs, 30_000, 1), concurrency: integer(config.bounds?.controlConcurrency, 4, 1) });
      const rustupContext = registeredRustupContext(cargoTool.path);
      service = new ThinJobService({ config, runtimeIdentity, state: thinState, cache: thinCache, policy: new ThinCommandPolicy({ externalCommands: config.externalCommands }), scheduler: thinScheduler, logger: thinLogger, singleton: ownedSingleton, registry: toolRegistry, toolchainLibraryPath: join(dirname(cargoTool.path), '..', 'lib'), rustupToolchain: rustupContext.toolchain, rustupHome: rustupContext.home, compilerEnvironment: Object.freeze({ RUSTC: rustcTool.path, RUSTDOC: rustdocTool.path }), compilerIdentity: Object.freeze({ rustc: rustcTool, rustdoc: rustdocTool }), spawn, workspaceResolver: discoveryResolver, repositoryResolver, processIdentity, processOwnership, startProcess, terminateProcess, readResources, readHostResources, statfs, now, platform, nativeOpenSsl: nativeOpenSsl === undefined ? (platform === 'win32' ? new NativeOpenSslCache({ root: join(dirname(thinState.paths.temporary), 'n') }) : null) : nativeOpenSsl });
      await thinState.recoverActive({ reap: (ownership) => reapPersisted(ownership, { graceMs: integer(config.bounds?.processGraceMs, 5_000, 1), platform }) });
      await thinCache.start?.();
      await service.#log({ type: 'BROKER_READY', runtime: service.#runtime });
      // Cache sizing may traverse large native target trees. Keep broker startup
      // & activation handshakes independent from that best-effort maintenance;
      // the maintenance GC also LRU-evicts idle current generations over cap.
      void service.#maintenanceGc();
      return service;
    } catch (error) {
      await ownedCache?.close?.().catch(() => {});
      await ownedLogger?.close?.().catch(() => {});
      await ownedSingleton?.release?.().catch(() => {});
      throw error;
    }
  }

  constructor({ config, runtimeIdentity, state, cache, policy, scheduler, logger, singleton, registry, toolchainLibraryPath, rustupToolchain, rustupHome, compilerEnvironment, compilerIdentity, spawn, workspaceResolver, repositoryResolver, processIdentity, processOwnership, startProcess, terminateProcess, readResources, readHostResources, statfs, now, platform = process.platform, nativeOpenSsl = null }) {
    this.#nativeOpenSsl = nativeOpenSsl;
    this.#config = config; this.#runtime = normalizeRuntimeIdentity(runtimeIdentity); this.#state = state; this.#cache = cache; this.#policy = policy; this.#scheduler = scheduler; this.#logger = logger; this.#singleton = singleton; this.#registry = registry; this.#toolchainLibraryPath = toolchainLibraryPath; this.#rustupToolchain = rustupToolchain; this.#rustupHome = rustupHome; this.#compilerEnvironment = compilerEnvironment; this.#compilerIdentity = compilerIdentity; this.#spawn = spawn; this.#workspaceResolver = workspaceResolver; this.#repositoryResolver = repositoryResolver; this.#identity = processIdentity; this.#ownership = processOwnership; this.#start = startProcess; this.#terminate = terminateProcess; this.#readResources = readResources ?? readManagedProcessResources; this.#readHostResources = readHostResources ?? hostResourceSnapshot; this.#statfs = statfs; this.#now = now; this.#platform = platform;
  }

  get runtimeIdentity() { return this.#runtime; }

  async recordTerminationSignal(signal, metadata = {}) {
    if (!['SIGINT', 'SIGTERM'].includes(signal)) throw new TypeError('broker termination signal must be SIGINT or SIGTERM');
    const activeRequestIds = [...this.#jobs.values()].filter((job) => !job.terminal).map((job) => job.id).sort();
    await this.#log({ type: 'BROKER_TERMINATION_SIGNAL', signal, ...metadata, activeRequestIds });
    await this.#logger.flush?.();
    return Object.freeze({ signal, activeRequestIds });
  }

  cacheUnavailable(event) {
    if (this.#stopping) return;
    const affected = [...this.#jobs.values()].filter((job) => !job.terminal && job.capability.compile);
    for (const job of affected) this.#requestCancel(job, 'sccache-unavailable');
    void this.#log({ type: 'CACHE_RUNTIME_BLOCKED', reason: event?.reason ?? 'sccache-unavailable', epoch: event?.epoch ?? null, gatewayPid: event?.gatewayPid ?? null, affectedJobIds: affected.map((job) => job.id) });
  }

  #gcInFlight = false; #gcQueued = false;

  /** Background maintenance GC (v2.8 §10.6): runs detached from broker
   * readiness and from request hot paths; repeated triggers coalesce. */
  async #maintenanceGc() {
    if (this.#gcInFlight) { this.#gcQueued = true; return; }
    this.#gcInFlight = true;
    try {
      do {
        this.#gcQueued = false;
        const gc = await this.#state.gc({ capBytes: this.#config.bounds?.targetCacheCapBytes ?? null, evictIdle: this.#evictIdleGeneration, freeFloorBytes: this.#config.bounds?.gcFreeFloorBytes ?? null, freeBytes: async () => { const stats = await nodeStatfs(this.#state.paths.base); return Number(stats.bavail) * Number(stats.bsize); } });
        if (gc.deleted?.length || gc.evicted?.length || gc.reclaimed?.deadRoots?.length || gc.reclaimed?.orphanLeaves?.length) await this.#log({ type: 'GENERATION_GC', deleted: gc.deleted ?? [], evicted: gc.evicted ?? [], reclaimed: gc.reclaimed ?? null, beforeBytes: gc.beforeBytes ?? null, afterBytes: gc.afterBytes ?? null, capBytes: gc.capBytes ?? null, effectiveCapBytes: gc.effectiveCapBytes ?? null, freeBytes: gc.freeBytes ?? null, freeFloorBytes: gc.freeFloorBytes ?? null });
        if (this.#nativeOpenSsl && this.#state.pinnedNativeKeys) { const native = await this.#nativeOpenSsl.gc({ pinned: await this.#state.pinnedNativeKeys('openssl') }); if (native.evicted.length) await this.#log({ type: 'NATIVE_OPENSSL_GC', ...native }); }
        if (gc.unenforceable) await this.#log({ type: 'TARGET_CACHE_CAP_UNENFORCEABLE', capBytes: gc.capBytes, targetBytes: gc.afterBytes, overByBytes: gc.unenforceable.overByBytes, idleCandidatesRemaining: gc.unenforceable.idleCandidatesRemaining });
      } while (this.#gcQueued);
    } catch (error) {
      await this.#log({ type: 'GENERATION_GC_FAILED', error: error.message }).catch(() => {});
    } finally { this.#gcInFlight = false; }
  }

  /** Aggregate compiler-budget admission (dual-host routing §5): map a
   * classified capability onto its command-class root cost. Returns null for
   * non-compiling roots and when no budget is configured. */
  #compileCost(capability) {
    const budget = this.#config.bounds?.compileBudgetUnits;
    if (!budget) return null;
    const classification = compileClass(capability);
    return classification ? deriveCompileCosts(budget, this.#config.bounds?.buildCostUnits ?? budget)[classification] : null;
  }

  /** Evict an idle current generation only while holding the same per-workspace
   * write exclusion BUILD/WORKSPACE_WRITE jobs use (v2.8 §10.3), so eviction
   * can never interleave with an admitted writer. tryRun keeps maintenance
   * non-blocking: when the workspace write slot is busy the eviction is
   * skipped (candidate stays warm) rather than queued behind the writer. */
  #evictIdleGeneration = async (candidate) => {
    const ticket = this.#scheduler.acquire({ id: `gc-evict-${candidate.workspaceKey}-${randomUUID().slice(0, 8)}`, capability: { lane: 'WORKSPACE_WRITE', workspaceAccess: 'write' }, workspaceKey: candidate.workspaceKey, tryRun: true });
    let permit;
    try { permit = await ticket.permit; } catch { return null; }
    try { return await this.#state.evictIdleWorkspace(candidate.canonicalRoot, { sizeBytes: candidate.sizeBytes, ...(candidate.reason ? { reason: candidate.reason } : {}) }); }
    catch { return null; }
    finally { permit.release(); }
  };

  async dispatch(message, emit = () => {}, onDisconnect = null) {
    this.#assertEnvelope(message); emit(Object.freeze({ type: 'HANDSHAKE', runtime: this.#runtime }));
    if (this.#stopping) return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: 'RightKit broker is stopping' });
    const submittedMessage = message.type === 'SUBMIT' ? message.job : message.type === 'MANAGED_AGENT' ? message : null;
    const rootAdmissionId = submittedMessage && submittedMessage.execution?.mode !== 'NESTED' ? randomUUID() : null;
    if (rootAdmissionId) this.#admissions.set(rootAdmissionId, Object.freeze({
      id: rootAdmissionId,
      state: 'ADMITTING',
      command: `${submittedMessage.shim} ${(submittedMessage.request?.argv ?? []).join(' ')}`.trim(),
      workspace: submittedMessage.request?.workspace ?? null,
      submittedAt: this.#now()
    }));
    if (await this.#activationPending()) {
      if (rootAdmissionId) this.#admissions.delete(rootAdmissionId);
      if (message.type === 'STATUS' && message.id === 'runtime-health-probe') return this.#emit(emit, { type: 'STATUS', id: message.id, state: 'ACTIVATION_PENDING', runtime: this.#runtime });
      return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: 'runtime activation is pending', code: 'RK_ACTIVATION_PENDING' });
    }
    if (rootAdmissionId && await cutoverDrainPending(this.#config.broker.externalRoot.path)) {
      this.#admissions.delete(rootAdmissionId);
      const inFlight = this.#inFlight();
      const detail = inFlight.length ? `; active broker work: ${this.#describeJobs(inFlight)}` : '';
      return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: `runtime cutover drain is pending${detail}; inspect with 'rightkit list' or 'rightkit status <request-id>'`, code: 'RK_CUTOVER_DRAIN_PENDING', inFlight });
    }
    try {
      if (message.type === 'DRAIN_STATUS') return this.#drainStatus(emit);
      if (message.type === 'LIST') return this.#list(emit);
      if (message.type === 'SUBMIT') return await this.#submit(message.job, emit, rootAdmissionId);
      if (message.type === 'WATCH') return this.#watch(message.id, emit, onDisconnect);
      if (message.type === 'STATUS') return await this.#status(message.id, emit);
      if (message.type === 'CANCEL') return this.#cancel(message, emit);
      if (message.type === 'MANAGED_AGENT') { const submitted = await this.#submit(message, emit, rootAdmissionId); return submitted.type === 'SUBMITTED' ? this.#watch(submitted.id, emit, onDisconnect) : submitted; }
      throw new TypeError('registered broker job operation is required');
    } catch (error) {
      if (rootAdmissionId) this.#admissions.delete(rootAdmissionId);
      if (error instanceof ToolChangedError || error.code === 'TOOL_CHANGED') emit(error.event);
      return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: error.message, code: error.code });
    }
  }

  async shutdown() {
    if (this.#stopping) return; this.#stopping = true;
    const open = [...this.#jobs.values()].filter((job) => !job.terminal);
    for (const job of open) this.#requestCancel(job, 'broker-shutdown');
    await Promise.allSettled(open.map((job) => job.done));
    await this.#cache.close?.();
    await this.#log({ type: 'BROKER_STOPPED' });
    await this.#logger.close();
    await this.#singleton.release();
  }

  async #submit(message, emit, reservedId = null) {
    let job;
    try {
      job = await this.#normalize(message, reservedId);
    } catch (error) {
      if (reservedId) this.#admissions.delete(reservedId);
      // Receipted pre-execution failure (v2.7 §20): a request that fails before
      // admission still persists a typed rejection receipt so callers can
      // STATUS/ATTACH well-formed failures instead of losing them to a socket.
      const shim = message?.shim; const argv = (message?.request?.argv ?? []);
      const attempted = Object.freeze({
        id: reservedId ?? randomUUID(), state: 'REJECTED', status: 'REJECTED',
        command: `${shim} ${argv.join(' ')}`.trim(),
        workspace: message?.request?.workspace ?? null,
        exitCode: 125, signal: null,
        reason: error.code ?? 'pre-execution-rejected', error: error.message,
        cache: { mode: 'direct', status: 'NOT_APPLICABLE', delta: null },
        cancellation: null, submittedAt: new Date(this.#now()).toISOString(), terminalAt: new Date(this.#now()).toISOString(),
        output: { bytes: 0, digest: '', head: '', tail: '' }
      });
      await this.#state.writeReceipt(attempted.id, attempted).catch(() => null);
      return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', id: attempted.id, error: error.message, code: error.code });
    }
    this.#jobs.set(job.id, job);
    if (reservedId) this.#admissions.delete(reservedId);
    this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'QUEUED', lane: job.capability.lane, priority: job.priority, at: this.#iso() } });
    await this.#log({ type: 'QUEUED', id: job.id, lane: job.capability.lane, command: job.command, workspace: job.workspaceRoot });
    const deadlineMs = integer(this.#config.bounds?.jobDeadlineMs, 0);
    if (deadlineMs > 0) { job.deadline = setTimeout(() => this.#requestCancel(job, 'deadline'), deadlineMs); job.deadline.unref?.(); }
    job.done = this.#execute(job).catch((error) => this.#finish(job, 'FAILED', { exitCode: 125, reason: error.code ?? 'broker-execution-failure', error: error.message }));
    return this.#emit(emit, { type: 'SUBMITTED', id: job.id, state: 'QUEUED', lane: job.capability.lane, priority: job.priority });
  }

  async #normalize(message, reservedId = null) {
    if (!message || message.type !== 'MANAGED_AGENT' || message.protocol !== BUILD_CONTROL_PROTOCOL) throw new TypeError('MANAGED_AGENT v2 payload is required');
    this.#assertRuntime(message.runtime);
    const request = normalizeManagedAgentRequest(message.request);
    validateManagedRustupInvocation(message.shim, request.argv);
    const parentId = message.execution?.mode === 'NESTED' ? message.execution.parentId : null;
    if (message.execution && !parentId) throw new TypeError('nested execution requires parentId');
    const parent = parentId ? this.#jobs.get(parentId) : null;
    if (parentId && (!parent || parent.terminal)) throw Object.assign(new Error('nested parent request is not active'), { code: 'RK_PARENT_NOT_ACTIVE' });
    const capability = parent ? this.#policy.classifyNested(parent.capability, message.shim, request.argv) : this.#policy.classify(message.shim, request.argv);
    const delegatedWorkspaceWrite = parent?.capability.external === true && parent.capability.lane === 'WORKSPACE_WRITE' && capability.lane === 'WORKSPACE_WRITE';
    const nestedCompiler = Boolean(parent) && ['rustc', 'rustdoc'].includes(message.shim);
    const delegatedCompiler = nestedCompiler && parent.capability.lane === 'BUILD' && parent.capability.compile === true;
    if (parent && !['CONTROL', 'ANALYSIS'].includes(capability.lane) && !delegatedWorkspaceWrite && !delegatedCompiler) throw Object.assign(new Error('nested non-control work is rejected'), { code: 'RK_NESTED_HEAVY_REJECTED' });
    if (parent) {
      const supplied = Buffer.from(message.execution?.capabilityToken ?? ''); const expected = Buffer.from(parent.rootToken);
      if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) throw Object.assign(new Error('nested capability token is invalid'), { code: 'RK_PARENT_CAPABILITY_INVALID' });
    }
    if (['cargo', 'rustc', 'rustdoc', 'rustup'].includes(message.shim)) {
      assertRepositoryExecutionAllowed({ workspace: request.workspace, environment: process.env, operation: message.shim, argv: request.argv, nested: delegatedCompiler });
    }
    const cargoTool = await this.#registry.verify(this.#config.shims?.cargo);
    if (message.shim === 'cargo' && capability.workspaceAccess !== 'none') await rejectWorkspaceCargoConfiguration(request.workspace);
    const executionCwd = await realpath(request.workspace);
    if (parent && capability.workspaceAccess !== 'none') {
      const managedCompilerRoot = delegatedCompiler && [parent.workspaceRoot, parent.executionEnvironment?.CARGO_HOME, parent.executionEnvironment?.CARGO_TARGET_DIR].some((root) => pathIsWithin(root, executionCwd));
      const sameRepository = delegatedCompiler && parent.repositoryRoot && pathIsWithin(parent.repositoryRoot, executionCwd) && await this.#repositoryResolver(executionCwd) === parent.repositoryRoot;
      if (!(managedCompilerRoot || sameRepository || (!delegatedCompiler && pathIsWithin(parent.workspaceRoot, executionCwd)))) throw Object.assign(new Error('nested workspace must remain inside parent repository, Cargo, or managed build roots'), { code: 'RK_NESTED_WORKSPACE_ESCAPE' });
    }
    const roots = capability.workspaceAccess === 'none'
      ? { executionCwd, workspaceRoot: null }
      : parent ? { executionCwd, workspaceRoot: parent.workspaceRoot }
        : await (typeof this.#workspaceResolver.resolve === 'function'
          ? this.#workspaceResolver.resolve(executionCwd, { cargoPath: cargoTool.path, environment: { ...process.env, CARGO_HOME: this.#state.paths.cargoHome } })
          : this.#workspaceResolver(executionCwd, { cargoPath: cargoTool.path, environment: { ...process.env, CARGO_HOME: this.#state.paths.cargoHome } }));
    const repositoryRoot = parent?.repositoryRoot ?? (roots.workspaceRoot ? await this.#repositoryResolver(roots.workspaceRoot) : null);
    // One repository owns one compiled-artifact pool even when it contains
    // several independent Cargo workspaces (for example engine, tray, & Hub).
    // Cargo's discovered root still governs resolution; repository root only
    // governs RightKit cache/scheduler state.
    const cacheRoot = repositoryRoot ?? roots.workspaceRoot;
    const workspace = cacheRoot ? await this.#state.workspace(cacheRoot) : null;
    const defaultTargetTriple = this.#config.defaults?.targetTriple;
    const rustupTargetTriple = message.shim === 'rustup' && request.argv.length === 3 && request.argv[0] === 'target' && request.argv[1] === 'add' ? request.argv[2] : null;
    const targetTriple = compilationTarget(message.shim, request.argv) ?? rustupTargetTriple ?? defaultTargetTriple;
    const allowedTargetTriples = this.#config.defaults?.targetTriples ?? [defaultTargetTriple];
    if (!allowedTargetTriples.includes(targetTriple)) throw Object.assign(new TypeError(`managed Rust target must be registered by broker: ${targetTriple}`), { code: 'RK_TARGET_NOT_REGISTERED' });
    let argv = request.argv;
    if (message.shim === 'cargo' && capability.compile && targetTriple) argv = normalizeCargoTargetArgv(argv, targetTriple);
    // Cargo already owns nested compiler output paths under its broker-owned
    // target generation. Rewriting them would make Cargo lose its artifacts.
    if (['rustc', 'rustdoc'].includes(message.shim) && capability.compile && !delegatedCompiler) argv = normalizeCompilerOutputArgv(argv, { compiler: message.shim, outputRoot: join(workspace.layout.target, 'rightkit-raw'), targetTriple });
    const toolId = capability.externalToolId ?? this.#config.shims?.[message.shim];
    if (!toolId) throw new TypeError(`no registered tool for shim ${message.shim ?? '<empty>'}`);
    const tool = await this.#registry.verify(toolId);
    if (capability.external && tool.sha256 !== capability.externalDigest) throw new ToolChangedError({ toolId, path: tool.path, expectedSha256: capability.externalDigest, observedSha256: tool.sha256, reason: 'capability-digest-mismatch' });
    const priority = message.priority ?? 'BATCH'; if (!PRIORITIES.has(priority)) throw new TypeError('priority must be BATCH, INTERACTIVE, or INSTALLER');
    const id = reservedId ?? randomUUID(); const rootToken = parent?.rootToken ?? randomBytes(32).toString('base64url');
    const expectedEnvironment = parent?.executionEnvironment ?? (workspace ? this.#managedEnvironment(workspace.layout, rootToken, parent?.id ?? id) : { CARGO_HOME: this.#state.paths.cargoHome });
    // Delegated compiler requests cannot select their process environment:
    // broker reuses parent runtime & injects it after stripping request control.
    // Cargo may synthesize controlled-looking values such as encoded rustflags,
    // so validating compiler observations against root Cargo input is incorrect.
    const observedEnvironment = nestedCompiler ? {} : { ...(message.observedEnvironment ?? {}) };
    validateManagedRustEnvironment(observedEnvironment, expectedEnvironment);
    let resolveDone; const done = new Promise((resolve) => { resolveDone = resolve; });
    const outputLimit = integer(this.#config.bounds?.outputBufferBytes, DEFAULT_OUTPUT_BYTES, 1024);
    const effectiveCapability = nestedCompiler ? Object.freeze({ ...capability, artifactWrite: false, nestedOverride: true, skipRootServices: true }) : capability;
    const requestEnvironment = nestedCompiler ? normalizeDelegatedCompilerEnvironment(request.environment) : normalizeRequestEnvironment(request.environment);
    return { id, parentId, rootToken, request: Object.freeze({ ...request, argv, workspace: roots.executionCwd, environment: requestEnvironment }), workspaceDiscoveryMs: roots.workspaceDiscoveryMs ?? null, workspaceDiscoveryCache: roots.workspaceDiscoveryCache ?? null, invokeArgv: argv, shim: message.shim, tool, capability: effectiveCapability, priority, targetTriple, workspaceRoot: cacheRoot, cargoWorkspaceRoot: roots.workspaceRoot, repositoryRoot, workspace, command: `${message.shim} ${argv.join(' ')}`.trim(), compilerChildren: [], resourceChildren: [], compilerRoundTrips: 0, compilerBrokerMs: 0, submittedAt: this.#now(), state: 'QUEUED', history: [], watchers: new Set(), outputBytes: 0, outputDigest: createHash('sha256'), outputHead: Buffer.alloc(0), outputTail: Buffer.alloc(0), outputLimit, lastOutputAt: null, residentOutput: '', residentTransition: null, residentAt: null, ticket: null, permit: null, child: null, ownership: null, termination: null, cancelReason: null, cancelRequests: [], terminal: null, cachePrepared: null, cacheResult: null, done, resolveDone };
  }

  async #execute(job) {
    try {
      if (!job.parentId) {
        job.ticket = this.#scheduler.acquire({ id: job.id, capability: job.capability, workspaceKey: job.workspace?.key ?? null, priority: job.priority, compileCost: this.#compileCost(job.capability), onHeartbeat: (heartbeat) => this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'HEARTBEAT', ...heartbeat } }) });
        job.permit = await job.ticket.permit;
      }
      if (job.cancelReason) throw Object.assign(new Error('queued request cancelled'), { code: 'RK_QUEUE_CANCELLED' });
      // A background GC may have evicted (rotated) this workspace's generation
      // while the request waited in queue. Re-resolve under the held workspace
      // permit so every path/env targets the current generation (v2.8 §10.3),
      // and relocate raw-compiler outputs that were normalized against the old
      // generation (normalization is idempotent under a fresh output root).
      if (job.workspace && !job.parentId) {
        job.workspace = await this.#state.workspace(job.workspaceRoot);
        if (job.shim === 'cargo' && job.capability.compile && job.targetTriple) {
          job.invokeArgv = normalizeCargoTargetArgv(job.invokeArgv, job.targetTriple, job.workspace.layout.target);
          job.request = Object.freeze({ ...job.request, argv: job.invokeArgv });
          job.command = `${job.shim} ${job.invokeArgv.join(' ')}`.trim();
        }
        if (['rustc', 'rustdoc'].includes(job.shim) && job.capability.compile) {
          job.invokeArgv = normalizeCompilerOutputArgv(job.invokeArgv, { compiler: job.shim, outputRoot: join(job.workspace.layout.target, 'rightkit-raw'), targetTriple: job.targetTriple });
          job.request = Object.freeze({ ...job.request, argv: job.invokeArgv });
          job.command = `${job.shim} ${job.invokeArgv.join(' ')}`.trim();
        }
      }
      job.queueMs = this.#now() - job.submittedAt;
      if (job.ticket) job.queueBlockers = job.ticket.blockers?.().map((blocker) => Object.freeze({ ...blocker, command: this.#jobs.get(blocker.id)?.command ?? null })) ?? [];
      job.rootSetupStartedAt ??= this.#now();
      if (job.capability.compile && !job.capability.skipRootServices) await this.#admitStorage(job);
      job.cachePrepared = (job.capability.compile && !job.capability.skipRootServices)
        ? await this.#cache.prepare({ layout: job.workspace.layout, cargoBuildJobs: this.#config.defaults?.cargoBuildJobs ?? 4, repositoryRoot: job.repositoryRoot, workspaceRoot: job.workspaceRoot, jobId: job.id, profile: cacheBuildProfile(job.shim, job.invokeArgv) })
        : job.parentId
          ? { mode: 'sccache-inherited', status: 'OK', environment: { }, before: null }
          : { mode: 'direct', status: 'NOT_APPLICABLE', environment: this.#managedEnvironment(job.workspace?.layout, job.rootToken, job.parentId ?? job.id), before: null };
      job.cacheResourceStart = job.cachePrepared.mode === 'sccache' ? safeResourceSnapshot(() => this.#cache.resourceUsage?.()) : null;
      const agentBin = join(this.#config.broker.externalRoot.path, 'agent-bin');
      // Cargo discovers bundled subcommands (notably cargo-clippy) through PATH.
      // Select its registered toolchain after managed shims, before ambient tools.
      const toolchainBin = join(this.#toolchainLibraryPath, '..', 'bin');
      const directCargoCompilers = !job.parentId && job.shim === 'cargo' ? this.#compilerEnvironment : {};
      const runtimeEnvironment = brokerOwnedRuntimeEnvironment({ ...runtimeBase(this.#toolchainLibraryPath, this.#rustupToolchain, this.#platform), ...(job.parentId ? this.#jobs.get(job.parentId)?.executionEnvironment ?? {} : {}), ...job.cachePrepared.environment, ...directCargoCompilers, PATH: `${agentBin}${delimiter}${toolchainBin}${delimiter}${process.env.PATH ?? ''}`, RIGHTKIT_PARENT_REQUEST_ID: job.parentId ?? job.id, RIGHTKIT_PARENT_CAPABILITY_TOKEN: job.rootToken }, brokerEndpointForPlatform(this.#config, this.#platform));
      // Loader authority remains broker-owned even when a cache adapter or
      // inherited parent environment contains a stale value.
      if (this.#platform === 'darwin' && this.#toolchainLibraryPath) runtimeEnvironment.DYLD_FALLBACK_LIBRARY_PATH = this.#toolchainLibraryPath;
      if (job.capability.external) runtimeEnvironment.CARGO = join(this.#config.broker.externalRoot.path, 'agent-bin', 'cargo');
      // Opt-in diagnostics only (RIGHTKIT_FINGERPRINT_LOG=1 on the broker); never enabled by default.
      if (process.env.RIGHTKIT_FINGERPRINT_LOG === '1' && !job.parentId && job.shim === 'cargo' && job.capability.compile) runtimeEnvironment.CARGO_LOG = 'cargo::core::compiler::fingerprint=info';
      job.executionEnvironment = Object.freeze({ ...runtimeEnvironment });
      let env = injectBrokerEnvironment({
        requestEnvironment: job.request.environment,
        runtimeEnvironment,
        normalizeEnvironment: job.capability.nestedOverride ? normalizeDelegatedCompilerEnvironment : normalizeRequestEnvironment
      });
      // Prebuilt OpenSSL (plan Step 4): fail-open; a skip is recorded, never a build failure.
      if (this.#nativeOpenSsl && !job.parentId && job.shim === 'cargo' && job.capability.compile && job.workspace && job.cargoWorkspaceRoot && job.targetTriple) {
        const native = await this.#nativeOpenSsl.prepare({ lockRoot: job.cargoWorkspaceRoot, targetTriple: job.targetTriple, environment: env, argv: job.invokeArgv, cargoPath: job.tool.path, producerEnvironment: env });
        if (native) {
          job.native = Object.freeze({ openssl: Object.freeze({ key: native.key ?? null, status: native.status, ...(native.skipped ? { skipped: native.skipped } : {}), ...(native.error ? { error: native.error } : {}), ...(native.recovered ? { recovered: native.recovered } : {}), buildMs: native.buildMs ?? null, verifyMs: native.verifyMs ?? null, path: native.path ?? null }) });
          if (native.status !== 'skipped') {
            env = Object.freeze({ ...env, ...native.environment });
            await this.#state.recordNativeReference?.(job.workspaceRoot, job.workspace.layout.generation, 'openssl', native.key);
          }
          await this.#log({ type: 'NATIVE_OPENSSL', id: job.id, ...job.native.openssl });
        }
      }
      const trackFreshness = !job.parentId && job.shim === 'cargo' && job.capability.compile && Boolean(job.workspace);
      if (trackFreshness) {
        job.jobsEvidence = resolveEffectiveJobs({ argv: job.invokeArgv, brokerEnvironment: job.executionEnvironment });
        job.targetAtStart = await probeTargetAtStart(job.workspace.layout, { state: job.workspace.state });
        job.freshnessTracker = wantsCargoJson(job.invokeArgv) ? new CargoFreshnessTracker() : null;
        // Without a JSON stream (e.g. the Tauri CLI's cargo build) fall back to a fingerprint census.
        job.fingerprintCensusStart = job.freshnessTracker ? null : await fingerprintCensusStart(job.workspace.layout.target, job.invokeArgv, job.targetTriple ?? null).catch(() => null);
        this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'STDERR', data: targetConsoleLine({ phase: 'start', target: job.targetAtStart }) } });
      }
      job.child = spawnManagedProcess(job.tool.path, job.invokeArgv, { cwd: job.request.workspace, env, stdio: ['ignore', 'pipe', 'pipe'], platform: this.#platform }, this.#spawn);
      job.exit = new Promise((resolve, reject) => { job.child.once('error', reject); job.child.once('close', (code, signal) => resolve({ code, signal })); });
      job.child.stdout?.on('data', (chunk) => this.#output(job, 'STDOUT', chunk));
      job.child.stderr?.on('data', (chunk) => this.#output(job, 'STDERR', chunk));
      const observed = await this.#identity(job.child.pid); if (observed.status !== 'OK') throw Object.assign(new Error(`process identity unavailable: ${observed.reason}`), { code: 'RK_PROCESS_IDENTITY_UNAVAILABLE' });
      job.ownership = this.#ownership(job.child, { identity: observed, platform: this.#platform });
      if (job.capability.artifactWrite) job.workspace = await this.#state.markRunning(job.workspaceRoot, { id: job.id, ownership: job.ownership, command: job.command });
      job.state = 'RUNNING'; job.startedAt = this.#now(); job.cargoRunStartedAt ??= this.#now();
      this.#sampleResourceState(job);
      this.#maybeMakeResident(job);
      await this.#log({ type: 'STARTED', id: job.id, pid: job.ownership.pid, pgid: job.ownership.processGroupId, workspace: job.workspaceRoot, generation: job.workspace?.layout.generation ?? null, cacheMode: job.cachePrepared.mode, tempDir: job.cachePrepared.environment.TMPDIR ?? null });
      await this.#start(job.child, { platform: this.#platform });
      this.#startSampling(job);
      if (job.cancelReason) this.#terminateJob(job);
      const result = await job.exit;
      await job.residentTransition;
      job.teardownStartedAt ??= this.#now();
      const reap = await (job.termination ?? this.#terminate(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 5_000, 1), platform: this.#platform }).catch((error) => ({ reaped: false, reason: error.message })));
      job.cacheResult = job.parentId
        ? Object.freeze({ mode: 'sccache-inherited', status: 'OK', delta: null })
        : await this.#cache.finish(job.cachePrepared);
      const status = job.cancelReason === 'deadline' ? 'TIMED_OUT' : job.cancelReason ? 'CANCELLED' : result.code === 0 ? 'SUCCEEDED' : 'FAILED';
      return this.#finish(job, status, { exitCode: safeExit(result.code, status), signal: result.signal, reap, reason: job.cancelReason ?? (result.code === 0 ? null : 'tool-exit') });
    } catch (error) {
      job.teardownStartedAt ??= this.#now();
      if (!job.parentId && job.cachePrepared && !job.cacheResult) job.cacheResult = await this.#cache.finish(job.cachePrepared).catch(() => null);
      if (error.code === 'RK_SCCACHE_UNAVAILABLE') {
        job.cachePrepared = error.cache;
        const cacheEvent = Object.freeze({ type: 'CACHE_ADMISSION_BLOCKED', id: job.id, command: job.command, workspace: job.workspaceRoot, generation: job.workspace?.layout.generation ?? null, cache: error.cache, code: error.code, error: error.message });
        this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: cacheEvent });
        await this.#log(cacheEvent);
        return this.#finish(job, 'REJECTED', { exitCode: 125, reason: error.code, error: error.message });
      }
      if (job.child) {
        const reap = await (job.termination ?? this.#terminate(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 5_000, 1), platform: this.#platform }).catch((cause) => ({ reaped: false, reason: cause.message })));
        return this.#finish(job, job.cancelReason === 'deadline' ? 'TIMED_OUT' : job.cancelReason || error.code === 'RK_QUEUE_CANCELLED' ? 'CANCELLED' : 'FAILED', { exitCode: 125, reason: job.cancelReason ?? error.code, error: error.message, reap });
      }
      return this.#finish(job, job.cancelReason === 'deadline' ? 'TIMED_OUT' : job.cancelReason || error.code === 'RK_QUEUE_CANCELLED' ? 'CANCELLED' : 'FAILED', { exitCode: 125, reason: job.cancelReason ?? error.code, error: error.message });
    } finally {
      if (job.sampleTimer) clearInterval(job.sampleTimer);
      job.permit?.release();
    }
  }

  async #finish(job, status, details) {
    if (job.terminal) return job.terminal;
    if (job.deadline) clearTimeout(job.deadline);
    // Rotation is only for uncertain writers (an unreaped process may still be
    // writing into the tree). A cleanly-reaped job cancelled for
    // sccache-unavailable leaves a trustworthy target: Cargo persists a crate's
    // fingerprint only after its compile succeeds, so an interrupted compile is
    // simply rebuilt. Rotating on cache loss discarded a warm target tree per
    // gateway blip — the dominant cause of full dep rebuilds (4 of 7 observed
    // rotations, 2026-08-29 telemetry).
    const uncertainWriter = job.capability.artifactWrite && !job.residentAt && details.reap && details.reap.reaped === false;
    const invalidWriter = uncertainWriter;
    if (invalidWriter) job.workspace = await this.#state.rotate(job.workspaceRoot, 'process-tree-unreaped');
    const terminalAt = this.#now();
    this.#sampleResourceState(job);
    // Roll delegated compiler children up into their admitted root so the root
    // receipt carries one authoritative compiler process & round-trip count and
    // broker-hop time (v2.7 §20), rather than fragmented per-child numbers.
    if (job.parentId) {
      const parent = this.#jobs.get(job.parentId);
      if (parent && !parent.terminal) {
        parent.compilerChildren?.push(job.id);
        parent.resourceChildren?.push(job.processResources);
        parent.compilerRoundTrips = (parent.compilerRoundTrips ?? 0) + 1;
        // The managed chain time for compilers is their broker run duration
        if (job.startedAt) {
          const accumulated = terminalAt - job.startedAt;
          parent.compilerBrokerMs = (parent.compilerBrokerMs ?? 0) + Math.max(0, accumulated);
        }
      }
    }
    // Aggregate the compiler subtree for this job's receipt stats
    // Aggregate the compiler subtree that ran under this admitted root so the
    // root receipt exposes compiler process count and broker round-trip count
    // once per Cargo root (v2.7 §20), instead of fragmenting them per child.
    const compilerProcessCount = job.parentId ? null
      : ['rustc', 'rustdoc'].includes(job.shim) && job.startedAt ? 1
        : Number.isSafeInteger(job.cacheResult?.delta?.requests_executed) ? job.cacheResult.delta.requests_executed
          : Number.isSafeInteger(job.cacheResult?.delta?.compile_requests) ? job.cacheResult.delta.compile_requests : null;
    const phaseTimings = this.#phaseTimings(job, terminalAt);
    const evidence = await this.#buildEvidence(job);
    if (evidence) this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'STDERR', data: targetConsoleLine({ phase: 'end', target: evidence.target, freshness: evidence.freshness, cold: evidence.cold }) } });
    const cacheCohort = job.parentId || !job.capability.compile ? null : cargoCohort({ argv: job.invokeArgv, command: job.command, workspace: job.workspaceRoot, profile: this.#config.defaults?.buildProfile ?? null, targetTriple: job.targetTriple ?? null, toolchain: this.#config.defaults?.toolchain ?? null, compilerIdentity: this.#compilerIdentity, cacheEpoch: job.cacheResult?.attribution?.epoch ?? job.cachePrepared?.attribution?.epoch ?? null, platform: this.#platform });
    const receipt = Object.freeze({
      id: job.id, state: status, status, command: job.command, argv: Object.freeze([...job.invokeArgv]), workspace: job.workspaceRoot, repository: job.repositoryRoot ?? null, profile: cacheCohort?.profile ?? this.#config.defaults?.buildProfile ?? null, targetTriple: job.targetTriple ?? null,
      generation: job.capability.artifactWrite ? (invalidWriter ? job.workspace.state.rotatedFrom : job.workspace.layout.generation) : null,
      resources: summarizeBuildResources({ processTree: job.processResources, delegated: job.resourceChildren, cacheBefore: job.cacheResourceStart, cacheAfter: job.cacheResourceEnd, cacheExpected: job.cachePrepared?.mode === 'sccache', cacheAttribution: job.cacheResult?.attribution, runMs: job.startedAt ? terminalAt - job.startedAt : null, host: job.hostResources }),
      // v2.7 §20 root timing
      ...phaseTimings,
      queueMs: job.queueMs ?? null,
      runMs: job.startedAt ? terminalAt - job.startedAt : 0,
      residentAt: job.residentAt ? new Date(job.residentAt).toISOString() : null,
      buildPermitMs: job.startedAt ? (job.residentAt ?? terminalAt) - job.startedAt : null,
      residentMs: job.residentAt ? terminalAt - job.residentAt : null,
      exitCode: details.exitCode ?? 125, signal: details.signal ?? null, reason: details.reason ?? null, error: details.error ?? null,
      // v2.7 §4.1: exact cancellation provenance (never inferred from SIGTERM alone)
      cancellation: job.cancelProvenance ? Object.freeze({ source: job.cancelProvenance, reason: job.cancelReason ?? null, requests: Object.freeze(job.cancelRequests.map((request) => Object.freeze({ ...request }))) }) : null,
      cache: job.cacheResult ?? (job.cachePrepared ? { mode: job.cachePrepared.mode, status: job.cachePrepared.status, healthStatus: job.cachePrepared.healthStatus ?? null, reason: job.cachePrepared.reason ?? null, delta: null } : { mode: 'direct', status: 'NOT_APPLICABLE', delta: null }),
      cacheCounterScope: job.parentId ? null : (job.cacheResult?.attribution?.scope ?? (job.cacheResult?.delta ? 'shared-cumulative-root-window' : null)),
      cacheCohort,
      // compilerProcessCount counts sccache compile requests (kept for compatibility); compilerRequestCount is the explicit name.
      compilerProcessCount, compilerRequestCount: compilerProcessCount, compilerRequestCountSource: compilerProcessCount === null ? null : ['rustc', 'rustdoc'].includes(job.shim) ? 'direct-compiler-invocation' : 'sccache-requests',
      ...(job.native ? { native: job.native } : {}),
      ...(evidence ? { jobs: evidence.jobs, target: evidence.target, freshness: evidence.freshness, coldStart: evidence.cold.coldStart, coldStartReason: evidence.cold.reason } : {}),
      waits: job.parentId ? null : Object.freeze({ brokerQueue: Object.freeze({ ms: job.queueMs ?? null, blockers: Object.freeze([...(job.queueBlockers ?? [])]) }) }),
      compilerBrokerRoundTrips: job.parentId ? null : (job.compilerRoundTrips ?? 0),
      compilerIdentity: job.parentId ? null : job.shim === 'cargo' && job.capability.compile ? this.#compilerIdentity : ['rustc', 'rustdoc'].includes(job.shim) ? Object.freeze({ [job.shim]: job.tool }) : null,
      ownership: job.ownership ?? null, reap: details.reap ?? null, submittedAt: new Date(job.submittedAt).toISOString(), terminalAt: new Date(terminalAt).toISOString(),
      output: { bytes: job.outputBytes, digest: job.outputDigest.digest('hex'), head: job.outputHead.toString('utf8'), tail: job.outputTail.toString('utf8') }
    });
    if (job.capability.artifactWrite && !uncertainWriter) await this.#state.finish(job.workspaceRoot, job.id, receipt); else await this.#state.writeReceipt(job.id, receipt);
    job.state = status; job.terminal = Object.freeze({ type: 'RESULT', id: job.id, exitCode: receipt.exitCode, status, receipt });
    await this.#log({ type: 'TERMINAL', id: job.id, status, exitCode: receipt.exitCode, signal: receipt.signal, queueMs: receipt.queueMs, runMs: receipt.runMs, cache: receipt.cache, resources: receipt.resources, cargoRunMs: receipt.cargoRunMs, generation: receipt.generation, uncertainWriter });
    this.#publish(job, job.terminal); job.resolveDone(job.terminal);
    // Terminal artifact writes mark the workspace size dirty (v2.8 §10.6):
    // schedule a detached maintenance GC so the target-cache budget stays
    // enforceable without a synchronous tree walk in the request hot path.
    if (job.capability.artifactWrite) void this.#maintenanceGc();
    return job.terminal;
  }

  /** Effective-jobs, target-state & freshness evidence for admitted Cargo compile roots. */
  async #buildEvidence(job) {
    if (!job.targetAtStart) return null;
    const freshness = job.freshnessTracker ? job.freshnessTracker.summary()
      : job.fingerprintCensusStart ? await fingerprintCensus(job.fingerprintCensusStart).catch((error) => Object.freeze({ available: false, reason: `fingerprint-census-failed:${error?.code ?? 'error'}` }))
        : Object.freeze({ available: false, reason: 'cargo-json-message-format-not-requested' });
    const cold = classifyColdStart(job.targetAtStart, freshness);
    return Object.freeze({ jobs: job.jobsEvidence ?? null, target: job.targetAtStart, freshness, cold });
  }

  #phaseTimings(job, terminalAt) {
    const duration = (start, end) => (start ? end - start : null);
    return Object.freeze({
      workspaceDiscoveryMs: job.workspaceDiscoveryMs ?? null,
      workspaceDiscoveryCache: job.workspaceDiscoveryCache ?? null,
      rootSetupMs: job.rootSetupStartedAt && job.cargoRunStartedAt ? job.cargoRunStartedAt - job.rootSetupStartedAt : null,
      cargoRunMs: job.cargoRunStartedAt && job.teardownStartedAt ? job.teardownStartedAt - job.cargoRunStartedAt : null,
      compilerBrokerMs: job.compilerBrokerMs ?? null,
      teardownMs: job.teardownStartedAt ? terminalAt - job.teardownStartedAt : null,
      totalMs: job.submittedAt ? terminalAt - job.submittedAt : null
    });
  }

  async #admitStorage(job) {
    const floor = integer(this.#config.bounds?.freeFloorBytes, 0);
    if (!floor) return;
    const value = await this.#statfs(this.#config.broker.externalRoot.path); const available = Number(value.bavail) * Number(value.bsize);
    if (!Number.isSafeInteger(available) || available < floor) throw Object.assign(new Error(`storage floor blocks build admission (${available} < ${floor})`), { code: 'RK_STORAGE_FLOOR' });
  }

  #managedEnvironment(layout, rootToken, rootId) {
    const toolchain = this.#config.defaults?.toolchain ?? 'default'; const toolchainKey = createHash('sha256').update(toolchain).digest('hex').slice(0, 24);
    return Object.freeze({
      CARGO_HOME: this.#state.paths.cargoHome,
      RUSTUP_HOME: this.#rustupHome ?? join(this.#state.paths.global, 'toolchains', toolchainKey),
      ...(layout ? { CARGO_TARGET_DIR: layout.target, CARGO_BUILD_TARGET_DIR: layout.target, TMPDIR: layout.temp, TMP: layout.temp, TEMP: layout.temp } : {}),
      RIGHTKIT_PARENT_REQUEST_ID: rootId,
      RIGHTKIT_PARENT_CAPABILITY_TOKEN: rootToken
    });
  }

  #sampleResourceState(job) {
    job.processResources = job.child
      ? safeResourceSnapshot(() => this.#readResources(job.child, { platform: this.#platform }))
      : { status: 'UNAVAILABLE', reason: 'process-not-started' };
    if (job.cachePrepared?.mode === 'sccache') job.cacheResourceEnd = safeResourceSnapshot(() => this.#cache.resourceUsage?.());
    try {
      const host = this.#readHostResources();
      if (!host || !Number.isFinite(host.freeMemoryBytes) || host.freeMemoryBytes < 0) throw new Error('invalid host memory snapshot');
      job.hostResources = { ...host, cargoBuildJobs: this.#config.defaults?.cargoBuildJobs ?? 4, compileBudgetUnits: this.#config.bounds?.compileBudgetUnits ?? null, samples: (job.hostResources?.samples ?? 0) + 1, minimumFreeMemoryBytes: Math.min(job.hostResources?.minimumFreeMemoryBytes ?? host.freeMemoryBytes, host.freeMemoryBytes) };
    } catch (error) {
      job.hostResources = { status: 'ERROR', reason: 'host-resource-query-failed', error: String(error?.message ?? error) };
    }
    return { processTree: job.processResources, sharedCompilerCache: job.cacheResourceEnd ?? { status: 'NOT_APPLICABLE' }, host: job.hostResources };
  }

  #startSampling(job) {
    const interval = integer(this.#config.bounds?.heartbeatIntervalMs, 5_000, 100);
    job.sampleTimer = setInterval(() => {
      const measured = this.#sampleResourceState(job);
      const sample = { resources: measured, type: 'RUNNING', id: job.id, elapsedMs: this.#now() - job.startedAt, outputBytes: job.outputBytes, lastOutputAt: job.lastOutputAt ? new Date(job.lastOutputAt).toISOString() : null };
      Promise.resolve(this.#statfs(this.#config.broker.externalRoot.path)).then((value) => {
        sample.freeDiskBytes = Number(value.bavail) * Number(value.bsize);
        const floor = integer(this.#config.bounds?.freeFloorBytes, 0); if (floor && sample.freeDiskBytes < floor) this.#requestCancel(job, 'storage-floor');
      }).catch((error) => { sample.storageError = error.message; }).finally(() => { void this.#log(sample); });
    }, interval); job.sampleTimer.unref?.();
  }

  #output(job, type, chunk) {
    const data = Buffer.from(chunk); if (type === 'STDOUT') job.freshnessTracker?.feed(data); job.outputBytes += data.length; job.outputDigest.update(data); job.lastOutputAt = this.#now();
    const headLimit = integer(this.#config.bounds?.outputHeadBytes, 16 * 1024); const tailLimit = integer(this.#config.bounds?.outputTailBytes, 16 * 1024);
    if (job.outputHead.length < headLimit) job.outputHead = Buffer.concat([job.outputHead, data.subarray(0, headLimit - job.outputHead.length)]);
    job.outputTail = appendTail(job.outputTail, data, tailLimit);
    this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type, data: data.toString('utf8') } });
    if (type === 'STDERR' && job.capability.residentAfterBuild && !job.residentTransition) {
      job.residentOutput = `${job.residentOutput}${data.toString('utf8')}`.slice(-16 * 1024);
      this.#maybeMakeResident(job);
    }
  }

  #maybeMakeResident(job) {
    if (job.state === 'RUNNING' && /(?:^|\r?\n)\s*Running\s+[`']/m.test(stripAnsi(job.residentOutput))) this.#makeResident(job);
  }

  #makeResident(job) {
    if (job.state !== 'RUNNING' || job.residentTransition || job.terminal || job.cancelReason) return;
    job.residentTransition = (async () => {
      job.workspace = await this.#state.markResident(job.workspaceRoot, job.id);
      job.residentAt = this.#now();
      job.state = 'RESIDENT';
      job.permit?.release(); job.permit = null;
      const event = { type: 'RESIDENT', at: new Date(job.residentAt).toISOString(), buildLaneReleased: true };
      this.#publish(job, { type: 'JOB_EVENT', id: job.id, event });
      await this.#log({ ...event, id: job.id, workspace: job.workspaceRoot, generation: job.workspace.layout.generation });
    })().catch(async (error) => {
      job.residentTransitionError = error.message;
      await this.#log({ type: 'RESIDENT_TRANSITION_FAILED', id: job.id, error: error.message });
    });
  }

  #jobSummary(job) {
    const now = this.#now(); const anchor = job.startedAt ?? job.submittedAt; const receipt = job.terminal?.receipt;
    return Object.freeze({
      id: job.id,
      state: job.state,
      command: job.command ?? null,
      workspace: job.workspaceRoot ?? job.workspace ?? null,
      lane: job.capability?.lane ?? null,
      priority: job.priority ?? null,
      submittedAt: typeof job.submittedAt === 'number' ? new Date(job.submittedAt).toISOString() : job.submittedAt ?? null,
      startedAt: job.startedAt ? new Date(job.startedAt).toISOString() : null,
      elapsedMs: Number.isSafeInteger(receipt?.totalMs) ? receipt.totalMs : typeof anchor === 'number' ? Math.max(0, now - anchor) : null,
      queueMs: Number.isSafeInteger(receipt?.queueMs) ? receipt.queueMs : job.startedAt ? Math.max(0, job.startedAt - job.submittedAt) : typeof job.submittedAt === 'number' ? Math.max(0, now - job.submittedAt) : null,
      outputBytes: job.outputBytes ?? 0,
      lastOutputAt: job.lastOutputAt ? new Date(job.lastOutputAt).toISOString() : null,
      attachedClients: job.watchers?.size ?? 0
    });
  }

  #inFlight() {
    const admitting = [...this.#admissions.values()].map((job) => this.#jobSummary(job));
    const active = [...this.#jobs.values()].filter((job) => !job.parentId && !job.terminal).map((job) => this.#jobSummary(job));
    return Object.freeze([...admitting, ...active]);
  }

  #describeJobs(jobs) {
    return jobs.map((job) => `${job.id} (${job.state}, ${job.elapsedMs ?? 0}ms, ${job.command ?? 'command pending'})`).join('; ');
  }

  #status(id, emit) {
    const live = this.#jobs.get(id);
    if (live) return this.#emit(emit, { type: 'STATUS', ...this.#jobSummary(live), receipt: live.terminal?.receipt ?? null });
    return this.#state.receipt(id).then((receipt) => { if (!receipt) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' }); return this.#emit(emit, { type: 'STATUS', id, state: receipt.state, receipt }); });
  }

  #list(emit) {
    const inFlight = this.#inFlight();
    const recent = [...this.#jobs.values()]
      .filter((job) => !job.parentId && job.terminal)
      .sort((left, right) => (right.terminal?.receipt?.terminalAt ?? '').localeCompare(left.terminal?.receipt?.terminalAt ?? ''))
      .slice(0, 20)
      .map((job) => this.#jobSummary(job));
    return this.#emit(emit, { type: 'LIST', inFlight, recent: Object.freeze(recent) });
  }

  #drainStatus(emit) {
    const inFlight = this.#inFlight();
    return this.#emit(emit, { type: 'DRAIN_STATUS', open: inFlight.length, inFlight });
  }

  async #watch(id, emit, onDisconnect) {
    const live = this.#jobs.get(id);
    if (!live) { const receipt = await this.#state.receipt(id); if (!receipt) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' }); return this.#emit(emit, { type: 'RESULT', id, exitCode: receipt.exitCode, status: receipt.state, receipt }); }
    for (const event of live.history) emit(event);
    if (live.terminal) return live.terminal;
    return new Promise((resolve) => {
      let unsubscribe = null;
      const watcher = (event) => { if (emit(event) === false) { cleanup(); resolve({ type: 'WATCH_DETACHED', id, reason: 'RK_WATCH_BACKPRESSURE' }); } else if (event.type === 'RESULT') { cleanup(); resolve(event); } };
      const cleanup = () => { live.watchers.delete(watcher); unsubscribe?.(); };
      live.watchers.add(watcher); unsubscribe = typeof onDisconnect === 'function' ? onDisconnect(() => { cleanup(); resolve({ type: 'DETACHED', id }); }) : null;
    });
  }

  #cancel(message, emit) {
    const job = this.#jobs.get(message.id); if (!job) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' });
    const source = message.source ?? 'USER_CANCEL';
    if (!CANCELLATION_SOURCES.has(source) || ['PARENT_CANCEL', 'BROKER_SHUTDOWN', 'DEADLINE', 'STORAGE_FLOOR', 'CONTAINMENT_FAILURE', 'UNKNOWN'].includes(source)) throw new TypeError('CANCEL source must be USER_CANCEL, OPERATOR_CANCEL, CLIENT_SIGNAL, or RELEASE_PRIORITY_CANCEL');
    const accepted = this.#requestCancel(job, 'explicit-cancel', source); return this.#emit(emit, { type: 'CANCEL_REQUESTED', id: message.id, accepted, state: job.state });
  }

  #cancelProvenance = Object.freeze({
    'explicit-cancel': 'USER_CANCEL',
    'broker-shutdown': 'BROKER_SHUTDOWN',
    'deadline': 'DEADLINE',
    'storage-floor': 'STORAGE_FLOOR',
    'parent-user-cancel': 'PARENT_CANCEL',
    'containment-failure': 'CONTAINMENT_FAILURE'
  });

  #requestCancel(job, reason, explicitSource = null) {
    if (job.terminal) return false;
    const provenance = explicitSource ?? this.#cancelProvenance[reason] ?? (reason?.startsWith?.('parent-') ? 'PARENT_CANCEL' : 'UNKNOWN');
    job.cancelRequests.push(Object.freeze({ source: provenance, reason, at: this.#iso() }));
    if (job.cancelReason) return false; job.cancelReason = reason;
    job.cancelProvenance = provenance;
    for (const child of this.#jobs.values()) if (child.parentId === job.id && !child.terminal) this.#requestCancel(child, `parent-${reason}`);
    if (job.ticket?.cancel()) return true; if (job.child) this.#terminateJob(job); return true;
  }

  #terminateJob(job) {
    if (!job.termination) job.termination = this.#terminate(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 5_000, 1), platform: this.#platform }).catch((error) => ({ reaped: false, reason: error.message }));
    void this.#log({ type: 'CANCEL_REQUESTED', id: job.id, reason: job.cancelReason }); return job.termination;
  }

  #publish(job, event) {
    const frozen = Object.freeze(event); const bytes = Buffer.byteLength(JSON.stringify(frozen));
    while (job.history.length && (job.history.length >= 512 || job.history.reduce((sum, value) => sum + Buffer.byteLength(JSON.stringify(value)), 0) + bytes > job.outputLimit)) job.history.shift();
    if (bytes <= job.outputLimit) job.history.push(frozen);
    for (const watcher of [...job.watchers]) try { watcher(frozen); } catch { job.watchers.delete(watcher); }
  }

  #emit(emit, value) { const frozen = Object.freeze(value); emit(frozen); return frozen; }
  #log(event) { return this.#logger.append(event).catch(() => null); }
  #iso() { return new Date(this.#now()).toISOString(); }

  #assertEnvelope(message) { if (!message || message.protocol !== BUILD_CONTROL_PROTOCOL) throw new TypeError('RightKit build-control v2 protocol is required'); this.#assertRuntime(message.runtime); }
  #assertRuntime(runtime) { const value = normalizeRuntimeIdentity(runtime); if (value.version !== this.#runtime.version || value.clientDigest !== this.#runtime.clientDigest || value.brokerDigest !== this.#runtime.brokerDigest) throw new TypeError('RightKit client/broker runtime digest mismatch'); }

  async #activationPending() {
    const path = this.#config.broker?.activationFencePath; if (!path) return false;
    try { const fence = JSON.parse(await readFile(path, 'utf8')); return fence?.state === 'PENDING'; }
    catch (error) { if (error.code === 'ENOENT') return false; throw Object.assign(new Error(`activation fence cannot be read: ${error.message}`), { code: 'RK_ACTIVATION_FENCE_INVALID' }); }
  }
}

export function createThinJobServer({ service, endpoint, maxRequestBytes = 1024 * 1024, maxPendingBytes = DEFAULT_OUTPUT_BYTES, netModule = net } = {}) {
  if (!service || typeof service.dispatch !== 'function' || typeof endpoint !== 'string' || !endpoint) throw new TypeError('service & endpoint are required');
  return netModule.createServer((socket) => {
    let buffer = ''; let handled = false; let detached = false; const disconnects = new Set();
    // Clients can disconnect between a bounded write being accepted & flushed.
    // EPIPE/ECONNRESET must detach only that watcher, never crash broker.
    socket.on('error', () => { detached = true; });
    const onDisconnect = (callback) => { disconnects.add(callback); return () => disconnects.delete(callback); };
    socket.once('close', () => { for (const callback of disconnects) callback(); disconnects.clear(); });
    const reject = (error) => { if (!socket.destroyed) socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 125, status: 'REJECTED', error })}\n`); };
    const emit = (event) => {
      if (socket.destroyed || detached) return false; const line = `${JSON.stringify(event)}\n`; const bytes = Buffer.byteLength(line);
      if (bytes > maxPendingBytes || socket.writableLength + bytes > maxPendingBytes) { detached = true; return false; }
      socket.write(line); return true;
    };
    socket.on('data', async (chunk) => {
      if (handled) return reject('one request per connection is required'); buffer += chunk.toString('utf8');
      if (Buffer.byteLength(buffer) > maxRequestBytes) { handled = true; return reject('request exceeds input cap'); }
      const index = buffer.indexOf('\n'); if (index < 0) return; handled = true;
      if (!buffer.slice(0, index) || buffer.slice(index + 1).trim()) return reject('one non-empty request per connection is required');
      try { await service.dispatch(JSON.parse(buffer.slice(0, index)), emit, onDisconnect); if (!socket.destroyed) socket.end(); }
      catch (error) { reject(error.message); }
    });
  });
}
