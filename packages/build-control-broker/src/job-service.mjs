import { createHash, randomUUID, randomBytes, timingSafeEqual } from 'node:crypto';
import { spawn as nodeSpawn } from 'node:child_process';
import { mkdir, readFile, realpath, statfs as nodeStatfs } from 'node:fs/promises';
import net from 'node:net';
import { dirname, isAbsolute, join, normalize, relative } from 'node:path';
import {
  BUILD_CONTROL_PROTOCOL,
  SOURCE_RUNTIME_IDENTITY,
  externalRoots,
  injectBrokerEnvironment,
  normalizeCargoTargetArgv,
  normalizeCompilerOutputArgv,
  normalizeManagedAgentRequest,
  normalizeRequestEnvironment,
  normalizeRuntimeIdentity,
  validateManagedRustEnvironment,
  validateManagedRustupInvocation
} from '@rightkit/build-control-core';
import { createCommandCapabilityRegistry } from './command-capabilities.mjs';
import { BoundedEventLog } from './event-log.mjs';
import { rejectWorkspaceCargoConfiguration } from './utilities.mjs';
import { MaintenanceWorker } from './maintenance-worker.mjs';
import {
  managedProcessOwnership,
  readManagedProcessIdentity,
  reapPersistedProcessTree,
  spawnManagedProcess,
  startManagedProcess,
  terminateProcessTree
} from './process-tree.mjs';
import { brokerOwnedSccacheProof, brokerSccacheNotifyPath, brokerSccacheSocketPath, directRustcFallback, prepareBrokerSccacheSocket, sccacheSocketPathsWithinLimit, SccacheSupervisor } from './sccache-supervisor.mjs';
import { CapabilityScheduler, QueueCancelledError } from './scheduler.mjs';
import { DurableStateStore } from './state-store.mjs';
import { inspectManagedTargetStd, requireManagedTargetStd, rustupToolchainSelector } from './toolchain-components.mjs';
import { ImmutableToolRegistry, ToolChangedError } from './tool-registry.mjs';

const TERMINAL = new Set(['SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT']);
const HEAVY = new Set(['BUILD', 'WORKSPACE_WRITE']);
const PRIORITIES = new Set(['BATCH', 'INTERACTIVE', 'INSTALLER']);
const DEFAULT_OUTPUT_BYTES = 8 * 1024 * 1024;
/** Broker-owned sccache server ownership; survives restarts for orphan reclamation. */
const SCCACHE_OWNERSHIP_SETTING = 'sccache-server-ownership';

const integer = (value, field, fallback, minimum = 0) => {
  const selected = value ?? fallback;
  if (!Number.isSafeInteger(selected) || selected < minimum) throw new TypeError(`${field} must be an integer >= ${minimum}`);
  return selected;
};
const contained = (root, value) => { const diff = relative(normalize(root), normalize(value)); return diff === '' || (!diff.startsWith('..') && !isAbsolute(diff)); };
const nowIso = () => new Date().toISOString();

/**
 * Durable, socket-independent broker job service. Network connections observe
 * jobs; only an explicit CANCEL command or configured deadline cancels one.
 */
export class ManagedJobService {
  #config; #runtime; #store; #registry; #commands; #toolErrors; #events; #scheduler; #maintenance; #spawn; #statfs; #targetInspector; #sccache = null; #sccacheSocket = null; #jobs = new Map(); #stopping = false; #maintenanceTimer = null; #maintenanceRun = Promise.resolve(); #maintenanceCursor = null;

  static async create({ config, runtimeIdentity = SOURCE_RUNTIME_IDENTITY, spawn = nodeSpawn, statfs = nodeStatfs, scheduler = null, registry = null, eventLog = null, targetInspector = inspectManagedTargetStd, sccacheSupervisor = null, sccacheReaper = reapPersistedProcessTree, prepareSccacheSocket = prepareBrokerSccacheSocket } = {}) {
    if (!config?.broker?.externalRoot?.path) throw new TypeError('validated broker config is required');
    const root = normalize(config.broker.externalRoot.path);
    const databasePath = normalize(config.broker.databasePath ?? `${config.broker.stateFile}.sqlite`);
    const logPath = normalize(config.broker.eventLogPath ?? join(dirname(config.broker.stateFile), 'events.ndjson'));
    if (![databasePath, logPath].every((path) => contained(root, path))) throw new TypeError('broker database & event log must remain below external root');
    const store = await DurableStateStore.open({ databasePath, legacyStatePath: config.broker.stateFile });
    const toolRegistry = registry ?? new ImmutableToolRegistry();
    const selectedToolIds = selectedManagedToolIds(config); const toolErrors = new Map();
    for (const tool of config.tools ?? []) if (selectedToolIds.has(tool.id)) {
      try { await toolRegistry.register(tool); } catch (error) { if (error instanceof ToolChangedError || error.code === 'TOOL_CHANGED') toolErrors.set(tool.id, error); else throw error; }
    }
    let sccache = null; let sccacheSocket = null; let sccacheDegraded = null; let sccacheOrphanReap = null;
    if (config.sccache?.mode === 'managed') {
      const sccacheToolId = config.defaults?.sccacheToolId;
      if (!sccacheToolId) throw Object.assign(new Error('managed sccache requires a registered sccache tool'), { code: 'RK_SCCACHE_TOOL_MISSING' });
      const startupError = toolErrors.get(sccacheToolId); if (startupError) throw startupError;
      // The sccache tool is pinned through selectedManagedToolIds, so this
      // verify is the same hash-verified path every other managed tool takes.
      const sccacheTool = await toolRegistry.verify(sccacheToolId);
      const graceMs = integer(config.bounds?.processGraceMs, 'processGraceMs', 5_000);
      // Reclaim a prior broker's server BEFORE its socket is unlinked: an
      // unreaped orphan still holding that path would otherwise answer probes
      // this broker cannot account for. A pending reap degrades the cache for
      // this broker lifetime; it never blocks boot.
      const persisted = store.readSetting?.(SCCACHE_OWNERSHIP_SETTING) ?? null;
      if (persisted) {
        sccacheOrphanReap = await Promise.resolve().then(() => sccacheReaper(persisted, { graceMs })).catch((error) => ({ reaped: false, reason: error.message }));
        store.clearSetting?.(SCCACHE_OWNERSHIP_SETTING);
        if (!sccacheOrphanReap.reaped) sccacheDegraded = 'sccache-orphan-unreaped';
      }
      const notifyPath = brokerSccacheNotifyPath(root);
      if (!sccacheDegraded && !sccacheSocketPathsWithinLimit(brokerSccacheSocketPath(root), notifyPath)) sccacheDegraded = 'sccache-socket-path-too-long';
      sccacheSocket = await prepareSccacheSocket({ externalRoot: root });
      sccache = sccacheSupervisor ?? new SccacheSupervisor({
        tool: sccacheTool,
        mode: config.sccache.mode,
        ownershipProof: brokerOwnedSccacheProof({ externalRoot: root, socketPath: sccacheSocket, serverPid: () => sccache?.serverPid ?? null }),
        externalRoot: root,
        socketPath: sccacheSocket,
        notifyPath,
        cacheDir: join(root, 'rightkit-build-control', 'global', 'compiler-cache'),
        maxSizeBytes: config.sccache.maxSizeBytes,
        errorLogPath: join(dirname(config.broker.stateFile), 'sccache-server.log'),
        graceMs,
        degradedReason: sccacheDegraded,
        onOwnership: (ownership) => { store.writeSetting?.(SCCACHE_OWNERSHIP_SETTING, ownership); }
      });
    }
    const events = eventLog ?? new BoundedEventLog({ path: logPath, maxBytes: integer(config.bounds?.eventLogMaxBytes, 'eventLogMaxBytes', 8 * 1024 * 1024, 1024) });
    const laneScheduler = scheduler ?? new CapabilityScheduler({
      controlConcurrency: integer(config.bounds?.controlConcurrency, 'controlConcurrency', 4, 1),
      buildConcurrency: integer(config.bounds?.buildConcurrency, 'buildConcurrency', 1, 1),
      workspaceWriteConcurrency: integer(config.bounds?.workspaceWriteConcurrency, 'workspaceWriteConcurrency', 4, 1),
      maintenanceConcurrency: integer(config.bounds?.maintenanceConcurrency, 'maintenanceConcurrency', 1, 1),
      heartbeatIntervalMs: integer(config.bounds?.heartbeatIntervalMs, 'heartbeatIntervalMs', 5_000, 10),
      priorityAgingMs: integer(config.bounds?.priorityAgingMs, 'priorityAgingMs', 30_000, 1)
    });
    const base = join(root, 'rightkit-build-control');
    const maintenance = new MaintenanceWorker({
      workspacesRoot: join(base, 'workspaces'),
      trashRoot: join(base, 'trash'),
      stateStore: store,
      maxBatchEntries: integer(config.bounds?.maintenanceBatchEntries, 'maintenanceBatchEntries', 128, 1)
    });
    const service = new ManagedJobService({ config, runtimeIdentity, store, registry: toolRegistry, toolErrors, events, scheduler: laneScheduler, maintenance, spawn, statfs, targetInspector, sccache, sccacheSocket });
    if (sccacheDegraded) await service.#log({ type: 'SCCACHE_DEGRADED', reason: sccacheDegraded, reap: sccacheOrphanReap });
    await service.#recover();
    service.#startMaintenance();
    return service;
  }

  constructor({ config, runtimeIdentity, store, registry, toolErrors, events, scheduler, maintenance, spawn, statfs, targetInspector, sccache = null, sccacheSocket = null }) {
    this.#config = config; this.#runtime = normalizeRuntimeIdentity(runtimeIdentity); this.#store = store; this.#registry = registry; this.#commands = createCommandCapabilityRegistry({ externalCommands: config.externalCommands }); this.#toolErrors = toolErrors; this.#events = events; this.#scheduler = scheduler; this.#maintenance = maintenance; this.#spawn = spawn; this.#statfs = statfs; this.#targetInspector = targetInspector; this.#sccache = sccache; this.#sccacheSocket = sccacheSocket;
  }

  get runtimeIdentity() { return this.#runtime; }

  async dispatch(message, emit = () => {}, onDisconnect = null) {
    this.#assertEnvelope(message);
    emit(Object.freeze({ type: 'HANDSHAKE', runtime: this.#runtime }));
    if (this.#stopping) return this.#emitResult(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: 'RightKit broker is stopping' });
    if (await this.#activationPending()) {
      if (message.type === 'STATUS' && message.id === 'runtime-health-probe') return this.#emitResult(emit, Object.freeze({ type: 'STATUS', id: message.id, state: 'ACTIVATION_PENDING', runtime: this.#runtime }));
      return this.#emitResult(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: 'runtime activation is pending', code: 'RK_ACTIVATION_PENDING' });
    }
    try {
      if (message.type === 'SUBMIT') return await this.#submit(message.job, emit);
      if (message.type === 'WATCH') return await this.#watch(message.id, emit, onDisconnect);
      if (message.type === 'STATUS') return this.#status(message.id, emit);
      if (message.type === 'CANCEL') return this.#cancel(message.id, emit);
      if (message.type === 'MANAGED_AGENT') {
        const submitted = await this.#submit(message, emit);
        if (submitted.type !== 'SUBMITTED') return submitted;
        return this.#watch(submitted.id, emit, onDisconnect);
      }
      throw new TypeError('registered broker job operation is required');
    } catch (error) {
      if (error instanceof ToolChangedError || error.code === 'TOOL_CHANGED') { emit(error.event); return this.#emitResult(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: error.message, code: 'TOOL_CHANGED' }); }
      return this.#emitResult(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: error.message, code: error.code });
    }
  }

  async shutdown() {
    this.#stopping = true;
    if (this.#maintenanceTimer) clearInterval(this.#maintenanceTimer);
    const open = [...this.#jobs.values()].filter((job) => !job.terminal);
    for (const job of open) this.#requestCancel(job, 'broker-shutdown');
    await Promise.allSettled(open.map((job) => job.done));
    // Before the unreaped-trees gate, never after: an sccache server this
    // broker cannot reap must be reported by that gate, not skipped past it.
    await this.#shutdownSccache();
    const unreaped = this.#store.entries({ states: ['STARTING', 'RUNNING', 'REAPING'] });
    if (unreaped.length) throw new Error(`RightKit shutdown blocked by ${unreaped.length} unreaped process tree(s)`);
    await this.#maintenanceRun;
    await this.#events.flush();
    this.#store.close();
  }

  /** Terminate the broker-owned sccache tree & remove both of its sockets. */
  async #shutdownSccache() {
    if (typeof this.#sccache?.shutdown !== 'function') return null;
    try {
      const outcome = await this.#sccache.shutdown();
      // Keep the ownership row when the tree survived: the next broker reaps it.
      if (!outcome?.reap || outcome.reap.reaped) this.#store.clearSetting?.(SCCACHE_OWNERSHIP_SETTING);
      else await this.#log({ type: 'SCCACHE_SHUTDOWN_DEGRADED', reason: outcome.reap.reason });
      return outcome;
    } catch (error) { await this.#log({ type: 'SCCACHE_SHUTDOWN_DEGRADED', reason: error.message }); return null; }
  }

  async #submit(message, emit) {
    const job = await this.#normalizeJob(message);
    this.#store.create({ id: job.id, data: job.data, occurredAt: job.submittedAt });
    await this.#transition(job, 'SUBMITTED', 'CLASSIFIED', { capability: job.capability });
    await this.#transition(job, 'CLASSIFIED', 'QUEUED', { priority: job.priority });
    this.#jobs.set(job.id, job);
    this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'QUEUED', lane: job.capability.lane, priority: job.priority, at: nowIso() } });
    const deadlineMs = integer(this.#config.bounds?.jobDeadlineMs, 'jobDeadlineMs', 0);
    if (deadlineMs > 0) { job.deadlineTimer = setTimeout(() => this.#requestCancel(job, 'deadline'), deadlineMs); job.deadlineTimer.unref?.(); }
    job.done = this.#execute(job).catch((error) => this.#terminalizeUnexpected(job, error));
    const submitted = Object.freeze({ type: 'SUBMITTED', id: job.id, state: 'QUEUED', lane: job.capability.lane, priority: job.priority });
    emit(submitted); return submitted;
  }

  async #normalizeJob(message) {
    if (!message || message.type !== 'MANAGED_AGENT' || message.protocol !== BUILD_CONTROL_PROTOCOL) throw new TypeError('MANAGED_AGENT v2 payload is required');
    this.#assertRuntime(message.runtime);
    const roots = await resolveRequestRoots(message.request?.workspace);
    const rawRequest = normalizeManagedAgentRequest({ ...message.request, workspace: roots.executionCwd });
    validateManagedRustupInvocation(message.shim, rawRequest.argv);
    const capability = this.#commands.classify(message.shim, rawRequest.argv);
    const defaultTargetTriple = this.#config.defaults?.targetTriple;
    const allowedTargetTriples = this.#config.defaults?.targetTriples ?? [defaultTargetTriple];
    const explicitTargetTriple = ['cargo', 'rustc', 'rustdoc'].includes(message.shim) ? compilationTarget(message.shim, rawRequest.argv) : null;
    const targetTriple = capability.lane === 'TOOLCHAIN_WRITE' ? rawRequest.argv[2] : explicitTargetTriple ?? defaultTargetTriple;
    if (!allowedTargetTriples.includes(targetTriple)) throw Object.assign(new TypeError(`managed Rust target must be registered by broker: ${targetTriple}`), { code: 'RK_TARGET_NOT_REGISTERED' });
    const argv = message.shim === 'cargo' && capability.lane === 'BUILD' ? normalizeCargoTargetArgv(rawRequest.argv, targetTriple) : rawRequest.argv;
    const request = Object.freeze({ ...rawRequest, argv, environment: normalizeRequestEnvironment(rawRequest.environment) });
    if (['cargo', 'rustc', 'rustdoc'].includes(message.shim)) await rejectWorkspaceCargoConfiguration(request.workspace);
    const toolId = capability.externalToolId ?? this.#config.shims?.[message.shim]; if (!toolId) throw new TypeError(`no registered tool for shim ${message.shim ?? '<empty>'}`);
    const registeredTool = await this.#verifyTool(toolId);
    if (capability.external && registeredTool.sha256 !== capability.externalDigest) {
      throw new ToolChangedError({ toolId, path: registeredTool.path, expectedSha256: capability.externalDigest, observedSha256: registeredTool.sha256, reason: 'capability-digest-mismatch' });
    }
    const priority = message.priority ?? 'BATCH'; if (!PRIORITIES.has(priority)) throw new TypeError('priority must be BATCH, INTERACTIVE, or INSTALLER');
    const layout = storageLayout(this.#config, { repositoryIdentity: roots.repositoryRoot, buildProfile: classifyBuildProfile(request.argv, this.#config.defaults?.buildProfile), targetTriple });
    validateManagedRustEnvironment(message.observedEnvironment ?? {}, managedRustEnvironment(layout));
    const checksTargetStd = Array.isArray(this.#config.defaults?.targetTriples) && (capability.lane === 'TOOLCHAIN_WRITE' || (capability.lane === 'BUILD' && ['cargo', 'rustc', 'rustdoc'].includes(message.shim)));
    let toolchainEvidence = null;
    if (checksTargetStd) {
      const rustupTool = message.shim === 'rustup' ? registeredTool : await this.#verifyTool(this.#config.shims?.rustup);
      toolchainEvidence = await this.#targetInspector({ rustupPath: rustupTool.path, rustupHome: layout.rustupHome, pinnedToolchain: this.#config.defaults.toolchain, target: targetTriple });
      if (capability.lane !== 'TOOLCHAIN_WRITE') requireManagedTargetStd(toolchainEvidence);
    }
    const parentId = message.execution?.mode === 'NESTED' ? message.execution.parentId : null;
    if (message.execution && (!parentId || message.execution.mode !== 'NESTED')) throw new TypeError('nested execution requires parentId');
    if (parentId && (typeof message.execution.capabilityToken !== 'string' || message.execution.capabilityToken.length < 32)) throw Object.assign(new TypeError('nested execution requires parent capability token'), { code: 'RK_PARENT_CAPABILITY_REQUIRED' });
    const id = randomUUID(); const submittedAt = nowIso();
    const parentCapabilityToken = randomBytes(32).toString('base64url');
    const suppliedCapabilityToken = message.execution?.capabilityToken ?? null;
    // Cargo's plugin convention invokes `cargo-<cmd>` with `<cmd>` as its first
    // argument, so the subcommand must be forwarded, not stripped. Stripping it
    // left strict clap plugins (outdated, nextest) parsing their own name as
    // missing and failing with cargo's own "Usage: cargo <COMMAND>"; it only
    // appeared to work because cargo-about tolerates the absent subcommand. The
    // old filter also removed the first argv occurrence anywhere in the line, so
    // `outdated --package outdated` dropped the wrong token.
    const directArgv = request.argv;
    const invokeArgv = capability.lane === 'TOOLCHAIN_WRITE' ? [...directArgv, '--toolchain', rustupToolchainSelector(this.#config.defaults.toolchain)] : ['rustc', 'rustdoc'].includes(message.shim) && capability.lane === 'BUILD' ? normalizeCompilerOutputArgv(directArgv, { compiler: message.shim, outputRoot: join(layout.build, 'raw-compiler'), targetTriple }) : directArgv;
    const diagnostics = message.shim === 'rustc' && this.#config.sccache?.mode === 'disabled' ? Object.freeze(['direct-rustc-with-sccache-disabled']) : Object.freeze([]);
    const data = Object.freeze({ id, request, invokeArgv, shim: message.shim, toolId, repositoryIdentity: roots.repositoryRoot, capability, priority, parentId, layout, diagnostics, toolchainEvidence, submittedAt });
    let resolveDone; const done = new Promise((resolve) => { resolveDone = resolve; });
    const outputLimit = integer(this.#config.bounds?.outputBufferBytes, 'outputBufferBytes', DEFAULT_OUTPUT_BYTES, 1024);
    return { id, data, request, invokeArgv, shim: message.shim, toolId, repositoryIdentity: roots.repositoryRoot, capability, priority, parentId, layout, diagnostics, toolchainEvidence, targetTriple, submittedAt, parentCapabilityToken, suppliedCapabilityToken, state: 'QUEUED', history: [], historyBytes: 0, historyDroppedBytes: 0, historyDroppedEvents: 0, historyLimit: integer(this.#config.bounds?.eventHistoryEntries, 'eventHistoryEntries', 4096, 16), historyByteLimit: integer(this.#config.bounds?.eventHistoryBytes, 'eventHistoryBytes', outputLimit, 1024), watchers: new Set(), children: new Set(), parentJob: null, rootJob: null, domainJob: null, reservationOwner: null, outputBytes: 0, outputLimit, outputHead: Buffer.alloc(0), outputTail: Buffer.alloc(0), outputDigest: createHash('sha256'), ticket: null, permit: null, child: null, termination: null, cancelReason: null, terminal: null, eventLogFailures: [], done, resolveDone };
  }

  async #execute(job) {
    try {
      const parent = job.parentId ? this.#store.entry(job.parentId) : null;
      // Nested execution is an explicit capability issued by this live broker,
      // never a client-forged permit.  Bind it to a live root request from the
      // same repository & keep nested work CONTROL-only so it cannot evade the
      // scheduler's heavy lane permit.
      if (job.parentId) {
        const liveParent = this.#jobs.get(job.parentId);
        const root = liveParent?.rootJob ?? liveParent;
        if (!liveParent || !root || root.parentId || liveParent.repositoryIdentity !== job.repositoryIdentity || root.repositoryIdentity !== job.repositoryIdentity || !parent || !['STARTING', 'RUNNING', 'REAPING'].includes(parent.state) || !['STARTING', 'RUNNING', 'REAPING'].includes(root.state)) {
          throw Object.assign(new Error('nested parent request is not active or trusted'), { code: 'RK_PARENT_NOT_ACTIVE' });
        }
        const left = Buffer.from(job.suppliedCapabilityToken); const right = Buffer.from(root.parentCapabilityToken); if (left.length !== right.length || !timingSafeEqual(left, right)) throw Object.assign(new Error('nested capability token is invalid'), { code: 'RK_PARENT_CAPABILITY_INVALID' });
        job.layout = root.layout;
        job.parentJob = liveParent;
        job.rootJob = root;
        job.domainJob = root;
        if (HEAVY.has(job.capability.lane) && !HEAVY.has(root.capability.lane)) {
          throw Object.assign(new Error('nested heavy work requires a heavy root request'), { code: 'RK_NESTED_HEAVY_ROOT_REQUIRED' });
        }
        if (HEAVY.has(job.capability.lane)) job.reservationOwner = root;
        liveParent.children.add(job);
      }
      if (!job.parentId) {
        job.ticket = this.#scheduler.acquire({ id: job.id, lane: job.capability.lane, priority: job.priority, repositoryIdentity: job.repositoryIdentity, onHeartbeat: (heartbeat) => this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'HEARTBEAT', ...heartbeat } }) });
        job.permit = await job.ticket.permit;
      }
      if (job.cancelReason) throw new QueueCancelledError();
      if (HEAVY.has(job.capability.lane) && !job.parentId) { const amount = integer(this.#config.defaults?.reservationBytes, 'reservationBytes', 0); if (amount > 0 && Number.isSafeInteger(this.#config.broker.capacityBytes)) { this.#store.reserveRequest({ requestId: job.id, domain: this.#config.broker.externalRoot.path, capacity: this.#config.broker.capacityBytes, amount }); job.reservationAmount = amount; } await this.#heavyPreflight(job); }
      await this.#transition(job, 'QUEUED', 'ADMITTED');
      if (HEAVY.has(job.capability.lane)) await this.#prepareNamespace(job);
      const tool = await this.#verifyInvocationTool(job);
      if (job.diagnostics.length) await this.#log({ type: 'DIRECT_RUSTC_DIAGNOSTIC', id: job.id, diagnostics: job.diagnostics });
      const env = await this.#executionEnvironment(job);
      await this.#reportCacheMode(job);
      job.child = spawnManagedProcess(tool.path, job.invokeArgv, { cwd: job.request.workspace, env, stdio: ['ignore', 'pipe', 'pipe'] }, this.#spawn);
      // Own exit before publishing STARTING/RUNNING. Cancellation may arrive as
      // soon as durable state becomes visible; attaching later can lose close.
      job.exit = new Promise((resolve, reject) => { job.child.once('error', reject); job.child.once('close', (code, signal) => resolve({ code, signal })); });
      // Capture launcher output before START. Fast probes can write & exit before
      // ownership transitions finish; attaching after RUNNING loses valid data.
      job.child.stdout?.on('data', (chunk) => this.#output(job, 'STDOUT', chunk));
      job.child.stderr?.on('data', (chunk) => this.#output(job, 'STDERR', chunk));
      const observed = await readManagedProcessIdentity(job.child.pid);
      if (observed.status !== 'OK') throw Object.assign(new Error(`process identity unavailable: ${observed.reason}`), { code: 'RK_PROCESS_IDENTITY_UNAVAILABLE' });
      const ownership = managedProcessOwnership(job.child, { identity: observed });
      await this.#transition(job, 'ADMITTED', 'STARTING', { ownership, launcherStartedAt: nowIso() });
      await startManagedProcess(job.child);
      await this.#transition(job, 'STARTING', 'RUNNING', { startedAt: nowIso() });
      if (HEAVY.has(job.capability.lane)) this.#startStorageSampling(job);
      if (job.cancelReason) this.#terminate(job);
      const result = await job.exit;
      if (result.code === 0 && job.capability.lane === 'TOOLCHAIN_WRITE') {
        const rustupTool = await this.#verifyTool(this.#config.shims.rustup);
        job.toolchainEvidence = await this.#targetInspector({ rustupPath: rustupTool.path, rustupHome: job.layout.rustupHome, pinnedToolchain: this.#config.defaults.toolchain, target: job.targetTriple });
        requireManagedTargetStd(job.toolchainEvidence);
      }
      await this.#transition(job, 'RUNNING', 'REAPING', { exitCode: result.code, signal: result.signal });
      const reap = job.termination ? await job.termination : await terminateProcessTree(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 'processGraceMs', 5_000) });
      if (!reap.reaped) throw Object.assign(new Error(`process tree remains live: ${reap.reason}`), { code: 'RK_REAP_PENDING', reap });
      const state = job.cancelReason ? (job.cancelReason === 'deadline' ? 'TIMED_OUT' : 'CANCELLED') : result.code === 0 ? 'SUCCEEDED' : 'FAILED';
      await this.#awaitDescendants(job);
      return this.#finish(job, state, { exitCode: validExit(result.code, state), signal: result.signal, reap, reason: job.cancelReason ?? (result.signal ? 'signal-exit' : result.code === 0 ? null : 'tool-exit') });
    } catch (error) {
      if (error instanceof QueueCancelledError || error.code === 'RK_QUEUE_CANCELLED') { this.#cancelDescendants(job, job.cancelReason ?? 'parent-cancel'); await this.#awaitDescendants(job); return this.#finish(job, 'CANCELLED', { exitCode: 125, reason: job.cancelReason ?? 'cancel' }); }
      if (error instanceof ToolChangedError || error.code === 'TOOL_CHANGED') this.#publish(job, error.event);
      let recoveryReap = null;
      if (job.child) {
        const current = this.#store.entry(job.id)?.state;
        if (current === 'RUNNING') await this.#transition(job, 'RUNNING', 'REAPING', { failureCode: error.code });
        recoveryReap = job.termination ? await job.termination : await terminateProcessTree(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 'processGraceMs', 5_000) }).catch((cause) => ({ reaped: false, reason: cause.message }));
        if (!recoveryReap.reaped) return this.#reapPending(job, error, recoveryReap);
      }
      this.#cancelDescendants(job, `parent-${error.code ?? 'failure'}`);
      await this.#awaitDescendants(job);
      return this.#finish(job, job.cancelReason === 'deadline' ? 'TIMED_OUT' : job.cancelReason ? 'CANCELLED' : 'FAILED', { exitCode: 125, reason: error.code ?? 'broker-execution-failure', error: error.message, reap: recoveryReap });
    } finally {
      if (job.storageTimer) clearInterval(job.storageTimer);
      if (job.reservationAmount) {
        let released = false;
        for (let attempt = 1; attempt <= 3 && !released; attempt += 1) {
          try { this.#store.releaseReservation({ requestId: job.id }); released = true; }
          catch (error) { if (attempt < 3) await new Promise((resolve) => setTimeout(resolve, 25 * attempt)); else { job.holdPermit = true; await this.#log({ type: 'RESERVATION_RELEASE_DEGRADED', id: job.id, error: error.message }); } }
        }
        if (released) job.reservationAmount = 0;
      }
      if (!job.holdPermit) job.permit?.release();
    }
  }

  async #heavyPreflight(job) {
    const timeoutMs = integer(this.#config.bounds?.statfsTimeoutMs, 'statfsTimeoutMs', 1_000, 1);
    let available;
    try { available = await boundedStatfs(this.#config.broker.externalRoot.path, this.#statfs, timeoutMs); }
    catch (error) { await this.#log({ type: 'STORAGE_DEGRADED', id: job.id, phase: 'admission', reason: error.message }); throw Object.assign(new Error('storage probe unavailable; new heavy work blocked'), { code: 'RK_STORAGE_DEGRADED' }); }
    const floor = integer(this.#config.bounds?.freeFloorBytes, 'freeFloorBytes', 0);
    const reserved = this.#store.reservationTotals({ domain: this.#config.broker.externalRoot.path }).reduce((sum, row) => sum + Number(row.reserved), 0);
    if (available - reserved < floor) throw Object.assign(new Error(`storage floor blocks heavy admission (${available} - ${reserved} < ${floor})`), { code: 'RK_STORAGE_FLOOR' });
  }

  async #prepareNamespace(job) {
    // A namespace gated by recovery may still hold a live orphaned compiler.
    // Refuse the job rather than hand a second writer the same directory.
    const gate = this.#store.readSetting(namespaceQuarantineKey(job.layout.namespace));
    if (gate) throw Object.assign(new Error(`namespace is quarantined by recovery: ${gate.reason ?? gate.code ?? 'unproven reap'}`), { code: 'RK_NAMESPACE_QUARANTINED', quarantine: gate });
    // Durable activation linearizes before mkdir. GC may only claim terminal
    // generations, so source can never be recreated between lease & rename.
    const claimed = this.#store.activateNamespace({ namespace: job.layout.namespace, repositoryIdentity: job.repositoryIdentity });
    job.generation = claimed.generation;
    await Promise.all(directoryRoots(job.layout).map((path) => mkdir(path, { recursive: true })));
  }

  async #transition(job, from, to, data = {}) {
    const outcome = this.#store.transition({ id: job.id, from, to, data, occurredAt: nowIso() });
    job.state = to;
    await this.#requiredLog(job, { type: 'LIFECYCLE', id: job.id, from, to, lane: job.capability.lane, repositoryIdentity: job.repositoryIdentity });
    return outcome;
  }

  async #finish(job, state, details) {
    if (job.terminal) return job.terminal;
    if (job.deadlineTimer) clearTimeout(job.deadlineTimer);
    const current = this.#store.entry(job.id);
    if (!current || TERMINAL.has(current.state)) return this.#terminalFromStore(job.id);
    const output = Object.freeze({ bytes: job.outputBytes, digest: job.outputDigest.digest('hex'), head: job.outputHead.toString('utf8'), tail: job.outputTail.toString('utf8') });
    const terminalAt = nowIso();
    // Attempt & fsync terminal evidence before committing one terminal receipt.
    // Its durability field is therefore final at first persistence; no crash
    // window or correction transaction can leave STATUS overstating evidence.
    await this.#requiredLog(job, { type: 'TERMINAL', id: job.id, state, exitCode: details.exitCode ?? 125, signal: details.signal ?? null, reason: details.reason ?? null, toolchainEvidence: job.toolchainEvidence ?? null });
    const receipt = Object.freeze({ id: job.id, state, exitCode: details.exitCode ?? 125, signal: details.signal ?? null, reason: details.reason ?? null, error: details.error ?? null, diagnostics: job.diagnostics ?? [], capability: job.capability, priority: job.priority, repositoryIdentity: job.repositoryIdentity, namespace: HEAVY.has(job.capability.lane) ? job.layout.namespace : null, generation: job.generation ?? null, parentId: job.parentId, toolchainEvidence: job.toolchainEvidence ?? null, submittedAt: job.submittedAt, terminalAt, reap: details.reap ?? null, output, eventLog: eventLogStatus(job.eventLogFailures) });
    const outcome = this.#store.recoverOpenRequest({ id: job.id, state, data: { ...details, terminalAt: receipt.terminalAt }, receipt, occurredAt: receipt.terminalAt });
    if (HEAVY.has(job.capability.lane) && job.generation !== undefined) {
      const namespaceState = this.#namespaceHasLiveDescendants(job) ? 'ACTIVE' : state;
      this.#store.upsertNamespace({ namespace: job.layout.namespace, generation: job.generation, state: namespaceState, repositoryIdentity: job.repositoryIdentity, ...(namespaceState === state ? { terminalAt: receipt.terminalAt, lruAt: receipt.terminalAt } : {}) });
    }
    job.terminal = Object.freeze({ type: 'RESULT', id: job.id, exitCode: receipt.exitCode, status: state, receipt: outcome.receipt });
    this.#publish(job, job.terminal); job.resolveDone(job.terminal); queueMicrotask(() => { if (job.watchers.size === 0) this.#jobs.delete(job.id); }); return job.terminal;
  }

  async #terminalizeUnexpected(job, error) {
    if (error.code === 'RK_REAP_BLOCKED' || this.#store.entry(job.id)?.state === 'REAPING') { job.holdPermit = true; await this.#log({ type: 'BROKER_INVARIANT_FAILURE', id: job.id, error: error.message, state: 'REAPING' }); job.resolveDone(null); return null; }
    try { return this.#finish(job, 'FAILED', { exitCode: 125, reason: error.code ?? 'unhandled-broker-error', error: error.message }); }
    catch (terminalError) { await this.#log({ type: 'BROKER_INVARIANT_FAILURE', id: job.id, error: terminalError.message, cause: error.message }); job.resolveDone(null); return null; }
  }

  #status(id, emit) {
    const entry = this.#store.entry(id); if (!entry) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' });
    const receipt = this.#store.receipt(id)?.receipt ?? null; const value = Object.freeze({ type: 'STATUS', id, state: entry.state, receipt }); emit(value); return value;
  }

  async #watch(id, emit, onDisconnect) {
    const entry = this.#store.entry(id); if (!entry) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' });
    const live = this.#jobs.get(id);
    if (!live) return this.#emitResult(emit, this.#terminalFromStore(id));
    if (live.historyDroppedEvents > 0) emit(Object.freeze({ type: 'JOB_EVENT', id, event: Object.freeze({ type: 'HISTORY_DROPPED', reason: 'history-byte-cap', events: live.historyDroppedEvents, bytes: live.historyDroppedBytes }) }));
    for (const event of live.history) emit(event);
    if (live.terminal) return live.terminal;
    return new Promise((resolve) => {
      let unsubscribe = null;
      const watcher = (event) => {
        const delivered = emit(event);
        if (delivered === false) {
          cleanup();
          const detached = Object.freeze({ type: 'WATCH_DETACHED', id, reason: 'RK_WATCH_BACKPRESSURE' });
          void this.#log({ type: 'WATCH_DETACHED', id, reason: detached.reason });
          resolve(detached);
          return;
        }
        if (event.type === 'RESULT') { cleanup(); resolve(event); }
      };
      const cleanup = () => { live.watchers.delete(watcher); unsubscribe?.(); };
      live.watchers.add(watcher);
      unsubscribe = typeof onDisconnect === 'function' ? onDisconnect(() => { cleanup(); resolve(Object.freeze({ type: 'DETACHED', id })); }) : null;
    });
  }

  #cancel(id, emit) {
    const entry = this.#store.entry(id); if (!entry) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' });
    const live = this.#jobs.get(id); const accepted = live && !live.terminal ? this.#requestCancel(live, 'explicit-cancel') : false;
    const value = Object.freeze({ type: 'CANCEL_REQUESTED', id, accepted: Boolean(accepted), state: this.#store.entry(id).state }); emit(value); return value;
  }

  #requestCancel(job, reason) {
    if (job.terminal || job.cancelReason) return false;
    job.cancelReason = reason;
    for (const child of this.#jobs.values()) if (child.parentId === job.id && !child.terminal) this.#requestCancel(child, `parent-${reason}`);
    if (job.ticket?.cancel()) return true;
    if (job.child) this.#terminate(job);
    return true;
  }

  #cancelDescendants(job, reason) {
    for (const child of job.children) if (!child.terminal) this.#requestCancel(child, reason);
  }

  async #awaitDescendants(job) {
    while (true) {
      const pending = [...job.children].filter((child) => !child.terminal);
      if (!pending.length) return;
      const outcomes = await Promise.allSettled(pending.map((child) => child.done));
      // A child whose owned tree cannot be reaped deliberately resolves its
      // promise without a terminal receipt.  Do not spin while its domain is
      // held in REAPING; propagate invariant failure to the parent instead.
      if (outcomes.some((outcome, index) => outcome.status === 'fulfilled' && outcome.value === null && !pending[index].terminal)) {
        job.holdPermit = true;
        throw Object.assign(new Error('descendant process tree remains unreaped'), { code: 'RK_REAP_BLOCKED' });
      }
    }
  }

  #namespaceHasLiveDescendants(job) {
    const entries = this.#store.entries({});
    return entries.some((entry) => entry.id !== job.id && !TERMINAL.has(entry.state) && entry.data?.layout?.namespace === job.layout.namespace);
  }

  #terminate(job) {
    if (!job.termination) job.termination = terminateProcessTree(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 'processGraceMs', 5_000) }).catch((error) => ({ reaped: false, reason: error.message }));
    return job.termination;
  }

  async #reapPending(job, cause, initial) {
    let result = initial;
    for (let attempt = 1; attempt <= 20 && !result.reaped; attempt += 1) {
      await this.#log({ type: 'REAP_RETRY', id: job.id, attempt, reason: result.reason });
      await new Promise((resolve) => setTimeout(resolve, 50));
      job.termination = null;
      result = await terminateProcessTree(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 'processGraceMs', 5_000) }).catch((error) => ({ reaped: false, reason: error.message }));
    }
    if (result.reaped) return this.#finish(job, job.cancelReason === 'deadline' ? 'TIMED_OUT' : job.cancelReason ? 'CANCELLED' : 'FAILED', { exitCode: 125, reason: cause.code ?? 'broker-execution-failure', error: cause.message, reap: result });
    job.holdPermit = true; this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'REAP_PENDING', reason: result.reason } });
    throw Object.assign(new Error(`owned process tree remains live after bounded retries: ${result.reason}`), { code: 'RK_REAP_BLOCKED' });
  }

  #startStorageSampling(job) {
    const interval = integer(this.#config.bounds?.diskSampleIntervalMs, 'diskSampleIntervalMs', 5_000, 1);
    const timeout = integer(this.#config.bounds?.statfsTimeoutMs, 'statfsTimeoutMs', 1_000, 1);
    const floor = integer(this.#config.bounds?.freeFloorBytes, 'freeFloorBytes', 0);
    let polling = false;
    job.storageTimer = setInterval(() => {
      if (polling) return; polling = true;
      boundedStatfs(this.#config.broker.externalRoot.path, this.#statfs, timeout)
        .then((availableBytes) => { job.lastAvailableBytes = availableBytes; if (availableBytes < floor) void this.#log({ type: 'STORAGE_LOW_ACTIVE', id: job.id, availableBytes, floorBytes: floor }); })
        .catch((error) => { job.storageDegraded = error.message; void this.#log({ type: 'STORAGE_DEGRADED', id: job.id, phase: 'active', reason: error.message }); })
        .finally(() => { polling = false; });
    }, interval);
    job.storageTimer.unref?.();
  }

  #output(job, type, chunk) {
    const bytes = chunk.length; job.outputBytes += bytes; job.outputDigest.update(chunk);
    const headLimit = integer(this.#config.bounds?.outputHeadBytes, 'outputHeadBytes', 16 * 1024, 0); const tailLimit = integer(this.#config.bounds?.outputTailBytes, 'outputTailBytes', 16 * 1024, 0);
    if (job.outputHead.length < headLimit) job.outputHead = Buffer.concat([job.outputHead, chunk.subarray(0, headLimit - job.outputHead.length)]);
    job.outputTail = appendBoundedTail(job.outputTail, chunk, tailLimit);
    // Attached watchers receive full chunks.  Retention is independently byte-bounded.
    this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type, data: chunk.toString('utf8') } });
  }

  #publish(job, event) {
    const frozen = Object.freeze(event);
    this.#retainHistory(job, frozen);
    for (const watcher of [...job.watchers]) { try { watcher(frozen); } catch { job.watchers.delete(watcher); } }
  }

  #retainHistory(job, event) {
    const bytes = serializedBytes(event);
    if (bytes > job.historyByteLimit) {
      job.historyDroppedEvents += 1; job.historyDroppedBytes += bytes;
      return;
    }
    while (job.history.length && (job.history.length >= job.historyLimit || job.historyBytes + bytes > job.historyByteLimit)) {
      const removed = job.history.shift(); const removedBytes = serializedBytes(removed);
      job.historyBytes -= removedBytes; job.historyDroppedEvents += 1; job.historyDroppedBytes += removedBytes;
    }
    if (job.historyBytes + bytes > job.historyByteLimit) {
      job.historyDroppedEvents += 1; job.historyDroppedBytes += bytes;
      return;
    }
    job.history.push(event); job.historyBytes += bytes;
  }

  #terminalFromStore(id) {
    const receipt = this.#store.receipt(id)?.receipt;
    if (!receipt) throw Object.assign(new Error('job is not terminal'), { code: 'RK_JOB_NOT_TERMINAL' });
    return Object.freeze({ type: 'RESULT', id, exitCode: receipt.exitCode ?? 125, status: receipt.state, receipt });
  }

  #emitResult(emit, value) { emit(value); return value; }

  #assertEnvelope(message) {
    if (!message || message.protocol !== BUILD_CONTROL_PROTOCOL) throw new TypeError('RightKit build-control v2 protocol is required');
    this.#assertRuntime(message.runtime);
  }

  #assertRuntime(runtime) {
    const value = normalizeRuntimeIdentity(runtime);
    if (value.version !== this.#runtime.version || value.clientDigest !== this.#runtime.clientDigest || value.brokerDigest !== this.#runtime.brokerDigest) throw new TypeError('RightKit client/broker runtime digest mismatch');
  }

  async #activationPending() {
    const path = this.#config.broker?.activationFencePath;
    if (typeof path !== 'string' || !path) return false;
    let fence;
    try { fence = JSON.parse(await readFile(path, 'utf8')); }
    catch (error) { if (error.code === 'ENOENT') return false; throw Object.assign(new Error(`activation fence cannot be read: ${error.message}`), { code: 'RK_ACTIVATION_FENCE_INVALID' }); }
    if (fence?.schema !== 'rightkit.build-control.runtime-activation.v1' || !['PENDING', 'COMMITTED'].includes(fence?.state)) throw Object.assign(new Error('activation fence state is invalid'), { code: 'RK_ACTIVATION_FENCE_INVALID' });
    const runtime = normalizeRuntimeIdentity(fence.runtime);
    if (runtime.version !== this.#runtime.version || runtime.clientDigest !== this.#runtime.clientDigest || runtime.brokerDigest !== this.#runtime.brokerDigest) throw Object.assign(new Error('activation fence runtime identity mismatch'), { code: 'RK_ACTIVATION_FENCE_INVALID' });
    return fence.state === 'PENDING';
  }

  async #recover() {
    const queued = this.#store.recoverOpenRequests();
    for (const item of queued) { this.#store.releaseReservation?.({ requestId: item.entry.id }); await this.#log({ type: 'RECOVERED', id: item.entry.id, from: item.receipt.recoveredFrom, to: 'CANCELLED' }); }
    for (const entry of this.#store.entries({ states: ['ADMITTED', 'STARTING', 'RUNNING', 'REAPING'] })) {
      const ownership = entry.data.ownership;
      if (!ownership && entry.state === 'ADMITTED') {
        const receipt = { id: entry.id, state: 'CANCELLED', exitCode: 125, reason: 'broker-restart-before-launch', recoveredFrom: entry.state, terminalAt: nowIso() };
        this.#store.recoverOpenRequest({ id: entry.id, state: 'CANCELLED', data: { reason: receipt.reason }, receipt, occurredAt: receipt.terminalAt }); this.#store.releaseReservation?.({ requestId: entry.id });
        await this.#log({ type: 'RECOVERED', id: entry.id, from: entry.state, to: 'CANCELLED' });
        continue;
      }
      // Ambiguous ownership used to throw here, which bricked every subsequent
      // start: one unrecoverable row kept the broker from ever binding again.
      // Recovery now quarantines the row instead, so startup always completes
      // and the unresolved tree is surfaced as evidence rather than a crash.
      if (!ownership) { await this.#quarantineRecovery(entry, { code: 'RK_RECOVERY_OWNERSHIP_AMBIGUOUS', reason: 'ownership-missing' }); continue; }
      let reap;
      try { reap = await reapPersistedProcessTree(ownership, { graceMs: integer(this.#config.bounds?.processGraceMs, 'processGraceMs', 5_000) }); }
      catch (error) { reap = Object.freeze({ status: 'REAP_FAILED', reaped: false, escalated: false, reason: error?.message ?? 'reap-failed' }); }
      if (!reap.reaped) { await this.#quarantineRecovery(entry, { code: 'RK_RECOVERY_REAP_INCOMPLETE', reason: reap.reason ?? 'reap-incomplete', reap, ownership }); continue; }
      const receipt = { id: entry.id, state: 'CANCELLED', exitCode: 125, reason: 'broker-restart', recoveredFrom: entry.state, reap, terminalAt: nowIso() };
      this.#store.recoverOpenRequest({ id: entry.id, state: 'CANCELLED', data: { reason: 'broker-restart', reap }, receipt, occurredAt: receipt.terminalAt }); this.#store.releaseReservation?.({ requestId: entry.id });
      await this.#log({ type: 'RECOVERED', id: entry.id, from: entry.state, to: 'CANCELLED', reap });
    }
  }

  /**
   * Terminalize one open request that recovery could not prove clean.
   *
   * The receipt records the typed cause and, when the reap escalated, that a
   * descendant may still be live — an operator signal, not a clean cancel.
   * Quarantine never rethrows: a failure to record one row must not stop the
   * broker from recovering every other row and starting.
   */
  async #quarantineRecovery(entry, { code, reason, reap = null, ownership = null }) {
    const quarantine = { code, reason, recoveredFrom: entry.state, processMayBeLive: reap?.escalated === true, ownership: ownership ? { pid: ownership.pid, platform: ownership.platform, processGroupId: ownership.processGroupId ?? null } : null };
    // A namespace is one directory per repository/target; its generation is not
    // part of the path, so a later job hashing to it would reuse the very
    // directory a surviving compiler may still be writing into. Terminalizing
    // the row without gating the namespace would trade a broker that cannot
    // start for silent concurrent writers, which is worse. Gate heavy work on
    // that namespace until an operator proves the tree is gone.
    const namespace = entry.data?.layout?.namespace ?? null;
    // Gate on ANY unproven reap, not only an escalated one: "could not prove
    // the tree is gone" is exactly as unsafe for directory reuse as "proved it
    // is still alive". processMayBeLive stays the stronger operator signal.
    if (typeof namespace === 'string' && namespace) {
      try { this.#store.writeSetting(namespaceQuarantineKey(namespace), { namespace, requestId: entry.id, code, reason, ownership: quarantine.ownership, at: nowIso() }); }
      catch (error) { await this.#log({ type: 'RECOVERY_NAMESPACE_GATE_FAILED', id: entry.id, namespace, error: error?.message ?? String(error) }); }
      quarantine.namespaceGated = namespace;
    }
    const receipt = { id: entry.id, state: 'CANCELLED', exitCode: 125, reason: 'broker-restart-quarantined', recoveredFrom: entry.state, quarantine, reap, terminalAt: nowIso() };
    try {
      this.#store.recoverOpenRequest({ id: entry.id, state: 'CANCELLED', data: { reason: receipt.reason, quarantine, reap }, receipt, occurredAt: receipt.terminalAt });
      this.#store.releaseReservation?.({ requestId: entry.id });
    } catch (error) {
      // The row stays open and is retried on the next start; that is recoverable,
      // whereas refusing to start is not. #log already swallows its own failures.
      await this.#log({ type: 'RECOVERY_QUARANTINE_FAILED', id: entry.id, code, reason, error: error?.message ?? String(error) });
      return null;
    }
    await this.#log({ type: 'RECOVERY_QUARANTINED', id: entry.id, from: entry.state, to: 'CANCELLED', code, reason, processMayBeLive: quarantine.processMayBeLive, reap });
    return receipt;
  }

  #startMaintenance() {
    const interval = this.#config.bounds?.gcIntervalMs; const retention = this.#config.bounds?.retentionMs;
    if (!Number.isSafeInteger(interval) || interval < 1 || !Number.isSafeInteger(retention) || retention < 0) return;
    const run = () => {
      if (this.#stopping) return;
      this.#maintenanceRun = this.#maintenanceRun.then(async () => {
        const permit = await this.#scheduler.acquire({ id: `maintenance-${randomUUID()}`, lane: 'MAINTENANCE', priority: 'BATCH', repositoryIdentity: 'machine', onHeartbeat: () => {} }).permit;
        try {
          const expiredBefore = new Date(Date.now() - retention).toISOString();
          const outcome = await this.#maintenance.retentionBatch({ expiredBefore, inventoryCursor: this.#maintenanceCursor }); this.#maintenanceCursor = outcome.nextCursor ?? null;
          // Age retention frees nothing while entries are young, so the size cap
          // runs after it and reclaims least-recently-used roots on its own.
          const capBytes = this.#config.bounds?.targetCacheCapBytes;
          // Roots used inside the floor stay protected, so a workspace that sits
          // permanently over cap cannot evict the output it just built.
          const floorMs = this.#config.bounds?.targetCacheFloorMs ?? 3_600_000;
          const evictableBefore = new Date(Date.now() - floorMs).toISOString();
          const capped = Number.isSafeInteger(capBytes) && capBytes > 0 ? await this.#maintenance.capBatch({ capBytes, evictableBefore }) : null;
          // Pinned trees plus unreclaimable bytes exceed the cap. Logging keeps
          // the tick intact; throwing here would land in the catch below and
          // null the inventory cursor every tick.
          if (capped?.capUnsatisfiable) await this.#log({ type: 'MAINTENANCE_CAP_UNSATISFIABLE', capBytes: capped.capBytes, totalBytes: capped.totalBytes, projectedBytes: capped.projectedBytes, pinnedCount: capped.pinnedCount, pinnedBytes: capped.pinnedBytes, orphanBytes: capped.orphanBytes });
          if (typeof this.#maintenance.reconcileTrashIntent === 'function') {
            for (const intent of this.#store.pendingTrashIntents?.() ?? []) await this.#maintenance.reconcileTrashIntent(intent);
          }
          for (const moved of [...outcome.moved, ...(capped?.moved ?? [])]) await this.#maintenance.deleteTrashBatch({ namespace: moved.namespace, generation: moved.generation, trashId: moved.trashId });
          for (const pending of this.#store.pendingMaintenance?.() ?? []) {
            const result = await this.#maintenance.deleteTrashBatch({ namespace: pending.namespace, generation: pending.generation, trashId: pending.trashId, cursor: pending.cursor });
            if (!result.complete) this.#store.updateMaintenanceCursor({ namespace: pending.namespace, generation: pending.generation, trashId: pending.trashId, cursor: result.nextCursor });
          }
          await this.#log({ type: 'MAINTENANCE', moved: outcome.moved.length, skipped: outcome.skipped.length, inventoryTruncated: outcome.inventoryTruncated, cap: capped ? { totalBytes: capped.totalBytes, capBytes: capped.capBytes, withinCap: capped.withinCap, moved: capped.moved.length, reclaimedBytes: capped.reclaimedBytes } : null });
        } finally { permit.release(); }
      }).catch((error) => {
        // A failed tick leaves its inventory cursor unconsumed. Keeping the
        // stale token would make every later tick fail as expired, wedging
        // maintenance until the broker restarts; restart the sweep instead.
        this.#maintenanceCursor = null;
        return this.#log({ type: 'MAINTENANCE_DEGRADED', error: error.message });
      });
    };
    this.#maintenanceTimer = setInterval(run, interval); this.#maintenanceTimer.unref?.(); run();
  }

  async #requiredLog(job, event) {
    try { return await this.#events.appendResult(event); }
    catch (error) {
      job.eventLogFailures.push(Object.freeze({ code: error.code ?? 'RK_EVENT_LOG_DURABILITY_UNCONFIRMED', message: error.message, event: event.type }));
      return null;
    }
  }
  #log(event) { return this.#events.append(event).catch(() => null); }
  #verifyTool(toolId) { const startup = this.#toolErrors.get(toolId); if (startup) throw startup; return this.#registry.verify(toolId); }
  async #verifyInvocationTool(job) {
    const tool = await this.#verifyTool(job.capability.externalToolId ?? job.toolId);
    if (job.capability.external && tool.sha256 !== job.capability.externalDigest) {
      throw new ToolChangedError({ toolId: job.capability.externalToolId, path: tool.path, expectedSha256: job.capability.externalDigest, observedSha256: tool.sha256, reason: 'capability-digest-mismatch' });
    }
    return tool;
  }
  async #executionEnvironment(job) {
    const extra = {};
    if (HEAVY.has(job.capability.lane)) {
      if (this.#config.defaults?.rustcToolId) extra.RUSTC = (await this.#verifyTool(this.#config.defaults.rustcToolId)).path;
      if (this.#config.defaults?.rustdocToolId) extra.RUSTDOC = (await this.#verifyTool(this.#config.defaults.rustdocToolId)).path;
      if (Number.isSafeInteger(this.#config.defaults?.cargoBuildJobs) && this.#config.defaults.cargoBuildJobs > 0) extra.CARGO_BUILD_JOBS = String(this.#config.defaults.cargoBuildJobs);
    }
    const environment = { ...executionEnvironment(this.#config, job), ...extra };
    if (!this.#sccache || !HEAVY.has(job.capability.lane)) return Object.freeze(environment);
    // Cache routing is broker-injected only. `RUSTC_WRAPPER` &
    // `SCCACHE_SERVER_UDS` stay in forbiddenRequestEnvironment so no client can
    // supply them, and they are added only once this broker can prove it owns
    // the server they point at.
    const tool = await this.#verifyTool(this.#config.defaults.sccacheToolId);
    const candidate = { ...environment, RUSTC_WRAPPER: tool.path, SCCACHE_SERVER_UDS: this.#sccacheSocket };
    const sample = await this.#sccache.sample(candidate);
    if (sample.status === 'OK') { job.cacheSample = sample; return Object.freeze(candidate); }
    job.cacheFallback = directRustcFallback(candidate, sample);
    return job.cacheFallback.environment;
  }

  /**
   * Silent degradation is the original incident: a build that quietly loses
   * caching looks identical to one that kept it. Report the mode before the
   * process starts so the receipt & event log always name it.
   */
  async #reportCacheMode(job) {
    if (!job.cacheFallback) return;
    const { status, reason } = job.cacheFallback;
    job.diagnostics = Object.freeze([...job.diagnostics, `sccache-fallback:${reason}`]);
    await this.#requiredLog(job, { type: 'SCCACHE_FALLBACK', id: job.id, mode: status, reason });
    this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'SCCACHE_FALLBACK', mode: status, reason, at: nowIso() } });
  }
}

export function createManagedJobServer({ service, endpoint, maxRequestBytes = 1024 * 1024, maxPendingBytes = DEFAULT_OUTPUT_BYTES, netModule } = {}) {
  if (!(service instanceof ManagedJobService) || typeof endpoint !== 'string' || !endpoint) throw new TypeError('managed job service & endpoint are required');
  if (!Number.isSafeInteger(maxRequestBytes) || maxRequestBytes < 1) throw new TypeError('positive request cap is required');
  if (!Number.isSafeInteger(maxPendingBytes) || maxPendingBytes < 1) throw new TypeError('positive response pending cap is required');
  const networking = netModule ?? net;
  return networking.createServer((socket) => {
    let buffer = ''; let handled = false; let detached = false;
    // A peer that closes mid-write makes the socket emit 'error' (EPIPE /
    // ECONNRESET). With no listener Node raises an unhandled 'error' event and
    // takes the whole broker down, which then refuses to restart because the
    // build it was serving is still running and its process group reads as
    // ambiguous. Absorb it and stop emitting to this peer instead.
    socket.on('error', () => { detached = true; });
    const disconnect = (handler) => { if (socket.destroyed) { queueMicrotask(handler); return () => {}; } socket.once('close', handler); return () => socket.off('close', handler); };
    const reject = (error) => { if (!socket.destroyed) socket.end(`${JSON.stringify({ type: 'RESULT', exitCode: 125, status: 'REJECTED', error })}\n`); };
    const emit = (event) => {
      if (socket.destroyed || detached) return false;
      const line = `${JSON.stringify(event)}\n`;
      if (Buffer.byteLength(line) > maxPendingBytes || socket.writableLength + Buffer.byteLength(line) > maxPendingBytes) { detached = true; return false; }
      // `write(false)` means this bounded frame was accepted & callers should
      // wait for drain; it is not data loss. The explicit writableLength cap
      // above remains the authority for detaching a slow watcher.
      socket.write(line);
      return true;
    };
    socket.on('data', async (chunk) => {
      if (handled) return reject('one request per connection is required');
      buffer += chunk.toString('utf8'); if (Buffer.byteLength(buffer) > maxRequestBytes) { handled = true; return reject('request exceeds input cap'); }
      const index = buffer.indexOf('\n'); if (index < 0) return;
      handled = true; if (!buffer.slice(0, index) || buffer.slice(index + 1).trim()) return reject('one non-empty request per connection is required');
      try { await service.dispatch(JSON.parse(buffer.slice(0, index)), emit, disconnect); if (!socket.destroyed) socket.end(); }
      catch (error) { reject(error.message); }
    });
  });
}

export async function resolveRequestRoots(workspace) {
  if (typeof workspace !== 'string' || !isAbsolute(workspace)) throw new TypeError('absolute workspace is required');
  const executionCwd = await realpath(workspace); let repositoryRoot = executionCwd;
  for (let current = executionCwd;; current = dirname(current)) {
    try { await realpath(join(current, '.git')); repositoryRoot = current; break; } catch (error) { if (error.code !== 'ENOENT') throw error; }
    const parent = dirname(current); if (parent === current) break;
  }
  return Object.freeze({ executionCwd, repositoryRoot });
}

function storageLayout(config, { repositoryIdentity, buildProfile, targetTriple }) {
  const root = normalize(config.broker.externalRoot.path); const toolchain = config.defaults?.toolchain;
  if (![toolchain, targetTriple].every((value) => typeof value === 'string' && value)) throw new TypeError('broker toolchain & targetTriple are required');
  const base = externalRoots({ externalRoot: root, repositoryIdentity, toolchain, target: targetTriple, buildProfile: 'release', policyProfile: 'MANAGED_AGENT' });
  const global = join(root, 'rightkit-build-control', 'global');
  const toolKey = createHash('sha256').update(toolchain).digest('hex').slice(0, 24);
  // CARGO_TARGET_DIR is the namespace target root; Cargo creates <triple>/<profile> beneath it.
  return Object.freeze({ ...base, buildProfile, target: base.targets, cargoHome: join(global, 'cargo-home'), rustupHome: join(global, 'toolchains', toolKey), sccache: join(global, 'compiler-cache'), packageCache: join(base.root, 'package-cache'), temp: base.temp });
}

function managedRustEnvironment(layout) { return { CARGO_HOME: layout.cargoHome, CARGO_TARGET_DIR: layout.target, CARGO_BUILD_TARGET_DIR: layout.target, CARGO_BUILD_BUILD_DIR: layout.build, CARGO_INSTALL_ROOT: join(layout.build, 'install'), RUSTUP_HOME: layout.rustupHome }; }
function executionEnvironment(config, job) {
  const base = Object.fromEntries(['PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'SystemRoot', 'ComSpec'].flatMap((key) => process.env[key] === undefined ? [] : [[key, process.env[key]]]));
  const inherited = job.parentJob?.request?.environment ?? {};
  const root = job.rootJob ?? job;
  const requestEnvironment = { ...inherited, ...job.request.environment };
  const common = { ...base, RIGHTKIT_PARENT_REQUEST_ID: root.id, RIGHTKIT_PARENT_CAPABILITY_TOKEN: root.parentCapabilityToken };
  if (!HEAVY.has(job.capability.lane)) return injectBrokerEnvironment({ requestEnvironment, runtimeEnvironment: { ...common, CARGO_HOME: job.layout.cargoHome, RUSTUP_HOME: job.layout.rustupHome } });
  const home = join(job.layout.temp, 'home');
  // Normalized config always carries a cache bound; an unnormalized one simply
  // leaves sccache on its own default rather than failing the build.
  // Under a broker-owned persistent server these two are INERT: a client cannot
  // reconfigure a running server's storage, which the server-start environment
  // owns. They stay because they still bind a client that runs without one, and
  // a test asserts both environments agree on cache dir & size.
  const cacheSize = config.sccache?.maxSizeBytes;
  const sccacheBound = Number.isSafeInteger(cacheSize) && cacheSize > 0 ? { SCCACHE_CACHE_SIZE: String(cacheSize) } : {};
  return injectBrokerEnvironment({ requestEnvironment, runtimeEnvironment: { ...common, ...managedRustEnvironment(job.layout), SCCACHE_DIR: job.layout.sccache, ...sccacheBound, npm_config_cache: job.layout.packageCache, HOME: home, XDG_CACHE_HOME: join(job.layout.packageCache, 'xdg'), XDG_CONFIG_HOME: join(home, '.config'), TMPDIR: job.layout.temp, TMP: job.layout.temp, TEMP: job.layout.temp } });
}
function directoryRoots(layout) { return [...new Set([layout.root, layout.cargoHome, layout.rustupHome, layout.sccache, layout.packageCache, layout.targets, layout.build, layout.target, layout.temp])]; }
function compilationTarget(shim, argv) { if (!['cargo', 'rustc', 'rustdoc'].includes(shim)) return null; let target = null; for (let index = 0; index < argv.length && argv[index] !== '--'; index += 1) { const value = argv[index]; let found = null; if (value === '--target') found = argv[++index]; else if (value.startsWith('--target=')) found = value.slice(9); if (found !== null) { if (!found || (target && target !== found)) throw new TypeError('one non-empty Cargo target is required'); target = found; } } return target; }
function classifyBuildProfile(argv, fallback = 'development') { const assigned = argv.find((value) => value.startsWith('--profile='))?.slice(10); if (assigned) return assigned; const index = argv.indexOf('--profile'); if (index >= 0 && argv[index + 1]) return argv[index + 1]; if (argv.includes('--release')) return 'release'; const command = argv.find((value) => !value.startsWith('-')); return ['test', 'bench', 'nextest'].includes(command) ? 'test' : fallback; }
function validExit(code, state) { if (state === 'SUCCEEDED') return 0; return Number.isInteger(code) && code >= 1 && code <= 255 ? code : 125; }
function appendBoundedTail(current, chunk, limit) {
  if (limit === 0) return Buffer.alloc(0);
  if (chunk.length >= limit) return Buffer.from(chunk.subarray(chunk.length - limit));
  const start = Math.max(0, current.length + chunk.length - limit);
  return Buffer.concat([current, chunk]).subarray(start);
}
function serializedBytes(value) { return Buffer.byteLength(JSON.stringify(value)); }
const namespaceQuarantineKey = (namespace) => `namespace-quarantine:${namespace}`;
function eventLogStatus(failures) { return failures.length ? Object.freeze({ durable: false, code: 'RK_EVENT_LOG_DURABILITY_UNCONFIRMED', failures: Object.freeze([...failures]) }) : Object.freeze({ durable: true }); }
async function boundedStatfs(root, operation, timeoutMs) { let timer; try { const value = await Promise.race([operation(root), new Promise((_, reject) => { timer = setTimeout(() => reject(Object.assign(new Error('statfs timed out'), { code: 'RK_STATFS_TIMEOUT' })), timeoutMs); timer.unref?.(); })]); const bytes = Number(value.bavail) * Number(value.bsize); if (!Number.isSafeInteger(bytes) || bytes < 0) throw new TypeError('statfs returned invalid free bytes'); return bytes; } finally { clearTimeout(timer); } }
function selectedManagedToolIds(config) {
  const ids = new Set(['cargo', 'rustc', 'rustdoc', 'rustup'].map((shim) => config.shims?.[shim]).filter(Boolean));
  for (const key of ['rustcToolId', 'rustdocToolId']) if (config.defaults?.[key]) ids.add(config.defaults[key]);
  for (const entry of config.externalCommands ?? []) if (entry?.toolId) ids.add(entry.toolId);
  if ((config.sccache?.mode === 'managed' || config.defaults?.cacheMode === 'managed') && config.defaults?.sccacheToolId) ids.add(config.defaults.sccacheToolId);
  return ids;
}
