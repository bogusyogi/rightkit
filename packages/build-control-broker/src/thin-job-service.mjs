import { createHash, randomBytes, randomUUID, timingSafeEqual } from 'node:crypto';
import { execFile as nodeExecFile, spawn as nodeSpawn } from 'node:child_process';
import { readFile, realpath, statfs as nodeStatfs } from 'node:fs/promises';
import net from 'node:net';
import { basename, delimiter, dirname, isAbsolute, join, normalize, relative } from 'node:path';
import { promisify } from 'node:util';
import {
  BUILD_CONTROL_PROTOCOL,
  SOURCE_RUNTIME_IDENTITY,
  injectBrokerEnvironment,
  normalizeCargoTargetArgv,
  normalizeCompilerOutputArgv,
  normalizeManagedAgentRequest,
  normalizeRequestEnvironment,
  normalizeRuntimeIdentity,
  validateManagedRustEnvironment,
  validateManagedRustupInvocation
} from '@rightkit/build-control-core';
import { ThinCache } from './thin-cache.mjs';
import { ThinCommandPolicy } from './thin-command-policy.mjs';
import { ThinLogger } from './thin-logger.mjs';
import { ThinScheduler } from './thin-scheduler.mjs';
import { acquireThinBrokerSingleton } from './thin-singleton.mjs';
import { ThinState } from './thin-state.mjs';
import { ImmutableToolRegistry, ToolChangedError } from './tool-registry.mjs';
import { rejectWorkspaceCargoConfiguration } from './utilities.mjs';
import {
  managedProcessOwnership,
  readManagedProcessIdentity,
  reapPersistedProcessTree,
  spawnManagedProcess,
  startManagedProcess,
  terminateProcessTree
} from './process-tree.mjs';

const TERMINAL = new Set(['SUCCEEDED', 'FAILED', 'CANCELLED', 'TIMED_OUT']);
const PRIORITIES = new Set(['BATCH', 'INTERACTIVE', 'INSTALLER']);
const DEFAULT_OUTPUT_BYTES = 8 * 1024 * 1024;
const execFile = promisify(nodeExecFile);

const integer = (value, fallback, minimum = 0) => {
  const selected = value ?? fallback;
  if (!Number.isSafeInteger(selected) || selected < minimum) throw new TypeError(`integer >= ${minimum} is required`);
  return selected;
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const safeExit = (code, status) => status === 'SUCCEEDED' ? 0 : Number.isInteger(code) && code >= 1 && code <= 255 ? code : 125;
const appendTail = (current, chunk, limit) => chunk.length >= limit ? Buffer.from(chunk.subarray(chunk.length - limit)) : Buffer.concat([current, chunk]).subarray(Math.max(0, current.length + chunk.length - limit));

export async function resolveCargoWorkspace(workspace, { cargoPath, run = execFile, environment = process.env } = {}) {
  if (typeof workspace !== 'string' || !isAbsolute(workspace) || typeof cargoPath !== 'string' || !cargoPath) throw new TypeError('absolute workspace & cargoPath are required');
  const executionCwd = await realpath(workspace);
  const result = await run(cargoPath, ['locate-project', '--workspace', '--message-format', 'plain'], { cwd: executionCwd, env: environment, encoding: 'utf8', timeout: 10_000, maxBuffer: 64 * 1024 });
  const manifest = String(result?.stdout ?? result).trim();
  if (!isAbsolute(manifest) || basename(manifest) !== 'Cargo.toml') throw Object.assign(new Error('Cargo workspace root could not be resolved'), { code: 'RK_WORKSPACE_UNRESOLVED' });
  return Object.freeze({ executionCwd, workspaceRoot: await realpath(dirname(manifest)) });
}

function selectedToolIds(config) {
  const ids = new Set(Object.values(config.shims ?? {}).filter(Boolean));
  for (const entry of config.externalCommands ?? []) ids.add(entry.toolId);
  if (config.defaults?.sccacheToolId) ids.add(config.defaults.sccacheToolId);
  return ids;
}

function runtimeBase() {
  return Object.fromEntries(['PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'SystemRoot', 'ComSpec'].flatMap((key) => process.env[key] === undefined ? [] : [[key, process.env[key]]]));
}

export class ThinJobService {
  #config; #runtime; #state; #cache; #policy; #scheduler; #logger; #singleton; #registry; #spawn; #workspaceResolver; #identity; #ownership; #start; #terminate; #statfs; #now; #jobs = new Map(); #stopping = false;

  static async create({ config, runtimeIdentity = SOURCE_RUNTIME_IDENTITY, registry = null, scheduler = null, state = null, cache = null, logger = null, singleton = null, spawn = nodeSpawn, workspaceResolver = resolveCargoWorkspace, processIdentity = readManagedProcessIdentity, processOwnership = managedProcessOwnership, startProcess = startManagedProcess, terminateProcess = terminateProcessTree, reapPersisted = reapPersistedProcessTree, statfs = nodeStatfs, now = Date.now } = {}) {
    if (!config?.broker?.externalRoot?.path) throw new TypeError('validated broker config is required');
    const externalRoot = normalize(config.broker.externalRoot.path);
    let ownedSingleton = singleton;
    try {
      ownedSingleton ??= await acquireThinBrokerSingleton({ stateRoot: join(externalRoot, 'rightkit-build-control', 'state') });
      const thinState = state ?? await ThinState.open({ externalRoot });
      const toolRegistry = registry ?? new ImmutableToolRegistry();
      if (!registry) for (const tool of config.tools ?? []) if (selectedToolIds(config).has(tool.id)) await toolRegistry.register(tool);
      const cargoTool = await toolRegistry.verify(config.shims?.cargo);
      const sccacheTool = config.defaults?.sccacheToolId ? await toolRegistry.verify(config.defaults.sccacheToolId) : null;
      const thinCache = cache ?? new ThinCache({
        paths: thinState.paths,
        enabled: config.sccache?.mode !== 'disabled' && Boolean(sccacheTool),
        sccachePath: sccacheTool?.path ?? null,
        clientSideSupported: config.sccache?.clientSide === true,
        maxSizeBytes: config.sccache?.maxSizeBytes,
        timeoutMs: config.bounds?.statfsTimeoutMs ?? 2_000
      });
      const thinLogger = logger ?? await ThinLogger.open({ path: join(thinState.paths.logs, 'events.ndjson') });
      const thinScheduler = scheduler ?? new ThinScheduler({
        controlConcurrency: integer(config.bounds?.controlConcurrency, 4, 1),
        analysisConcurrency: integer(config.bounds?.analysisConcurrency, 2, 1),
        workspaceWriteConcurrency: integer(config.bounds?.workspaceWriteConcurrency, 4, 1),
        buildConcurrency: integer(config.bounds?.buildConcurrency, 1, 1), now
      });
      const service = new ThinJobService({ config, runtimeIdentity, state: thinState, cache: thinCache, policy: new ThinCommandPolicy({ externalCommands: config.externalCommands }), scheduler: thinScheduler, logger: thinLogger, singleton: ownedSingleton, registry: toolRegistry, spawn, workspaceResolver, processIdentity, processOwnership, startProcess, terminateProcess, statfs, now });
      await thinState.recoverActive({ reap: (ownership) => reapPersisted(ownership, { graceMs: integer(config.bounds?.processGraceMs, 5_000, 1) }) });
      await service.#log({ type: 'BROKER_READY', runtime: service.#runtime });
      return service;
    } catch (error) {
      await logger?.close?.().catch(() => {});
      await ownedSingleton?.release?.().catch(() => {});
      throw error;
    }
  }

  constructor({ config, runtimeIdentity, state, cache, policy, scheduler, logger, singleton, registry, spawn, workspaceResolver, processIdentity, processOwnership, startProcess, terminateProcess, statfs, now }) {
    this.#config = config; this.#runtime = normalizeRuntimeIdentity(runtimeIdentity); this.#state = state; this.#cache = cache; this.#policy = policy; this.#scheduler = scheduler; this.#logger = logger; this.#singleton = singleton; this.#registry = registry; this.#spawn = spawn; this.#workspaceResolver = workspaceResolver; this.#identity = processIdentity; this.#ownership = processOwnership; this.#start = startProcess; this.#terminate = terminateProcess; this.#statfs = statfs; this.#now = now;
  }

  get runtimeIdentity() { return this.#runtime; }

  async dispatch(message, emit = () => {}, onDisconnect = null) {
    this.#assertEnvelope(message); emit(Object.freeze({ type: 'HANDSHAKE', runtime: this.#runtime }));
    if (this.#stopping) return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: 'RightKit broker is stopping' });
    if (await this.#activationPending()) {
      if (message.type === 'STATUS' && message.id === 'runtime-health-probe') return this.#emit(emit, { type: 'STATUS', id: message.id, state: 'ACTIVATION_PENDING', runtime: this.#runtime });
      return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: 'runtime activation is pending', code: 'RK_ACTIVATION_PENDING' });
    }
    try {
      if (message.type === 'SUBMIT') return await this.#submit(message.job, emit);
      if (message.type === 'WATCH') return this.#watch(message.id, emit, onDisconnect);
      if (message.type === 'STATUS') return await this.#status(message.id, emit);
      if (message.type === 'CANCEL') return this.#cancel(message.id, emit);
      if (message.type === 'MANAGED_AGENT') { const submitted = await this.#submit(message, emit); return submitted.type === 'SUBMITTED' ? this.#watch(submitted.id, emit, onDisconnect) : submitted; }
      throw new TypeError('registered broker job operation is required');
    } catch (error) {
      if (error instanceof ToolChangedError || error.code === 'TOOL_CHANGED') emit(error.event);
      return this.#emit(emit, { type: 'RESULT', exitCode: 125, status: 'REJECTED', error: error.message, code: error.code });
    }
  }

  async shutdown() {
    if (this.#stopping) return; this.#stopping = true;
    const open = [...this.#jobs.values()].filter((job) => !job.terminal);
    for (const job of open) this.#requestCancel(job, 'broker-shutdown');
    await Promise.allSettled(open.map((job) => job.done));
    await this.#log({ type: 'BROKER_STOPPED' });
    await this.#logger.close();
    await this.#singleton.release();
  }

  async #submit(message, emit) {
    const job = await this.#normalize(message);
    this.#jobs.set(job.id, job);
    this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'QUEUED', lane: job.capability.lane, priority: job.priority, at: this.#iso() } });
    await this.#log({ type: 'QUEUED', id: job.id, lane: job.capability.lane, command: job.command, workspace: job.workspaceRoot });
    const deadlineMs = integer(this.#config.bounds?.jobDeadlineMs, 0);
    if (deadlineMs > 0) { job.deadline = setTimeout(() => this.#requestCancel(job, 'deadline'), deadlineMs); job.deadline.unref?.(); }
    job.done = this.#execute(job).catch((error) => this.#finish(job, 'FAILED', { exitCode: 125, reason: error.code ?? 'broker-execution-failure', error: error.message }));
    return this.#emit(emit, { type: 'SUBMITTED', id: job.id, state: 'QUEUED', lane: job.capability.lane, priority: job.priority });
  }

  async #normalize(message) {
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
    const delegatedCompiler = parent?.capability.lane === 'BUILD' && parent.capability.compile === true && ['rustc', 'rustdoc'].includes(message.shim) && capability.lane === 'BUILD';
    if (parent && !['CONTROL', 'ANALYSIS'].includes(capability.lane) && !delegatedWorkspaceWrite && !delegatedCompiler) throw Object.assign(new Error('nested non-control work is rejected'), { code: 'RK_NESTED_HEAVY_REJECTED' });
    if (parent) {
      const supplied = Buffer.from(message.execution?.capabilityToken ?? ''); const expected = Buffer.from(parent.rootToken);
      if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) throw Object.assign(new Error('nested capability token is invalid'), { code: 'RK_PARENT_CAPABILITY_INVALID' });
    }
    const cargoTool = await this.#registry.verify(this.#config.shims?.cargo);
    if (message.shim === 'cargo' && capability.workspaceAccess !== 'none') await rejectWorkspaceCargoConfiguration(request.workspace);
    const executionCwd = await realpath(request.workspace);
    if (parent && capability.workspaceAccess !== 'none') {
      const childPath = relative(parent.workspaceRoot, executionCwd);
      if (childPath === '..' || childPath.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || isAbsolute(childPath)) throw Object.assign(new Error('nested workspace must remain inside parent Cargo workspace'), { code: 'RK_NESTED_WORKSPACE_ESCAPE' });
    }
    const roots = capability.workspaceAccess === 'none'
      ? { executionCwd, workspaceRoot: null }
      : parent ? { executionCwd, workspaceRoot: parent.workspaceRoot }
        : await this.#workspaceResolver(executionCwd, { cargoPath: cargoTool.path, environment: { ...process.env, CARGO_HOME: this.#state.paths.cargoHome } });
    const workspace = roots.workspaceRoot ? await this.#state.workspace(roots.workspaceRoot) : null;
    const targetTriple = this.#config.defaults?.targetTriple;
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
    const id = randomUUID(); const rootToken = parent?.rootToken ?? randomBytes(32).toString('base64url');
    const expectedEnvironment = parent?.executionEnvironment ?? (workspace ? this.#managedEnvironment(workspace.layout, rootToken, parent?.id ?? id) : { CARGO_HOME: this.#state.paths.cargoHome });
    validateManagedRustEnvironment(message.observedEnvironment ?? {}, expectedEnvironment);
    let resolveDone; const done = new Promise((resolve) => { resolveDone = resolve; });
    const outputLimit = integer(this.#config.bounds?.outputBufferBytes, DEFAULT_OUTPUT_BYTES, 1024);
    const effectiveCapability = delegatedCompiler ? Object.freeze({ ...capability, artifactWrite: false, nestedOverride: true }) : capability;
    return { id, parentId, rootToken, request: Object.freeze({ ...request, argv, workspace: roots.executionCwd, environment: normalizeRequestEnvironment(request.environment) }), invokeArgv: argv, shim: message.shim, tool, capability: effectiveCapability, priority, workspaceRoot: roots.workspaceRoot, workspace, command: `${message.shim} ${argv.join(' ')}`.trim(), submittedAt: this.#now(), state: 'QUEUED', history: [], watchers: new Set(), outputBytes: 0, outputDigest: createHash('sha256'), outputHead: Buffer.alloc(0), outputTail: Buffer.alloc(0), outputLimit, lastOutputAt: null, ticket: null, permit: null, child: null, ownership: null, termination: null, cancelReason: null, terminal: null, cachePrepared: null, cacheResult: null, done, resolveDone };
  }

  async #execute(job) {
    try {
      if (!job.parentId) {
        job.ticket = this.#scheduler.acquire({ id: job.id, capability: job.capability, workspaceKey: job.workspace?.key ?? null, priority: job.priority, onHeartbeat: (heartbeat) => this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type: 'HEARTBEAT', ...heartbeat } }) });
        job.permit = await job.ticket.permit;
      }
      if (job.cancelReason) throw Object.assign(new Error('queued request cancelled'), { code: 'RK_QUEUE_CANCELLED' });
      job.queueMs = this.#now() - job.submittedAt;
      if (job.capability.compile) await this.#admitStorage(job);
      job.cachePrepared = job.capability.compile ? await this.#cache.prepare({ layout: job.workspace.layout, cargoBuildJobs: this.#config.defaults?.cargoBuildJobs ?? 4 }) : { mode: 'direct', status: 'NOT_APPLICABLE', environment: this.#managedEnvironment(job.workspace?.layout, job.rootToken, job.parentId ?? job.id), before: null };
      const agentBin = join(this.#config.broker.externalRoot.path, 'agent-bin');
      const runtimeEnvironment = { ...runtimeBase(), ...(job.parentId ? this.#jobs.get(job.parentId)?.executionEnvironment ?? {} : {}), ...job.cachePrepared.environment, PATH: `${agentBin}${delimiter}${process.env.PATH ?? ''}`, RIGHTKIT_PARENT_REQUEST_ID: job.parentId ?? job.id, RIGHTKIT_PARENT_CAPABILITY_TOKEN: job.rootToken };
      if (job.capability.external) runtimeEnvironment.CARGO = join(this.#config.broker.externalRoot.path, 'agent-bin', 'cargo');
      job.executionEnvironment = Object.freeze({ ...runtimeEnvironment });
      const env = injectBrokerEnvironment({ requestEnvironment: job.request.environment, runtimeEnvironment });
      job.child = spawnManagedProcess(job.tool.path, job.invokeArgv, { cwd: job.request.workspace, env, stdio: ['ignore', 'pipe', 'pipe'] }, this.#spawn);
      job.exit = new Promise((resolve, reject) => { job.child.once('error', reject); job.child.once('close', (code, signal) => resolve({ code, signal })); });
      job.child.stdout?.on('data', (chunk) => this.#output(job, 'STDOUT', chunk));
      job.child.stderr?.on('data', (chunk) => this.#output(job, 'STDERR', chunk));
      const observed = await this.#identity(job.child.pid); if (observed.status !== 'OK') throw Object.assign(new Error(`process identity unavailable: ${observed.reason}`), { code: 'RK_PROCESS_IDENTITY_UNAVAILABLE' });
      job.ownership = this.#ownership(job.child, { identity: observed });
      if (job.capability.artifactWrite) job.workspace = await this.#state.markRunning(job.workspaceRoot, { id: job.id, ownership: job.ownership, command: job.command });
      job.state = 'RUNNING'; job.startedAt = this.#now();
      await this.#log({ type: 'STARTED', id: job.id, pid: job.ownership.pid, pgid: job.ownership.processGroupId, workspace: job.workspaceRoot, generation: job.workspace?.layout.generation ?? null, cacheMode: job.cachePrepared.mode, tempDir: job.cachePrepared.environment.TMPDIR ?? null });
      await this.#start(job.child);
      this.#startSampling(job);
      if (job.cancelReason) this.#terminateJob(job);
      const result = await job.exit;
      const reap = await (job.termination ?? this.#terminate(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 5_000, 1) }).catch((error) => ({ reaped: false, reason: error.message })));
      job.cacheResult = await this.#cache.finish(job.cachePrepared);
      const status = job.cancelReason === 'deadline' ? 'TIMED_OUT' : job.cancelReason ? 'CANCELLED' : result.code === 0 ? 'SUCCEEDED' : 'FAILED';
      return this.#finish(job, status, { exitCode: safeExit(result.code, status), signal: result.signal, reap, reason: job.cancelReason ?? (result.code === 0 ? null : 'tool-exit') });
    } catch (error) {
      if (job.child) {
        const reap = await (job.termination ?? this.#terminate(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 5_000, 1) }).catch((cause) => ({ reaped: false, reason: cause.message })));
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
    const uncertainWriter = job.capability.artifactWrite && details.reap && details.reap.reaped === false;
    if (uncertainWriter) job.workspace = await this.#state.rotate(job.workspaceRoot, 'process-tree-unreaped');
    const terminalAt = this.#now();
    const receipt = Object.freeze({ id: job.id, state: status, status, command: job.command, workspace: job.workspaceRoot, generation: job.capability.artifactWrite ? (uncertainWriter ? job.workspace.state.rotatedFrom : job.workspace.layout.generation) : null, queueMs: job.queueMs ?? null, runMs: job.startedAt ? terminalAt - job.startedAt : 0, exitCode: details.exitCode ?? 125, signal: details.signal ?? null, reason: details.reason ?? null, error: details.error ?? null, cache: job.cacheResult ?? (job.cachePrepared ? { mode: job.cachePrepared.mode, status: job.cachePrepared.status, reason: job.cachePrepared.reason ?? null, delta: null } : { mode: 'direct', status: 'NOT_APPLICABLE', delta: null }), reap: details.reap ?? null, submittedAt: new Date(job.submittedAt).toISOString(), terminalAt: new Date(terminalAt).toISOString(), output: { bytes: job.outputBytes, digest: job.outputDigest.digest('hex'), head: job.outputHead.toString('utf8'), tail: job.outputTail.toString('utf8') } });
    if (job.capability.artifactWrite && !uncertainWriter) await this.#state.finish(job.workspaceRoot, job.id, receipt); else await this.#state.writeReceipt(job.id, receipt);
    job.state = status; job.terminal = Object.freeze({ type: 'RESULT', id: job.id, exitCode: receipt.exitCode, status, receipt });
    await this.#log({ type: 'TERMINAL', id: job.id, status, exitCode: receipt.exitCode, signal: receipt.signal, queueMs: receipt.queueMs, runMs: receipt.runMs, cache: receipt.cache, generation: receipt.generation, uncertainWriter });
    this.#publish(job, job.terminal); job.resolveDone(job.terminal); return job.terminal;
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
      RUSTUP_HOME: join(this.#state.paths.global, 'toolchains', toolchainKey),
      ...(layout ? { CARGO_TARGET_DIR: layout.target, CARGO_BUILD_TARGET_DIR: layout.target, CARGO_INSTALL_ROOT: this.#state.paths.tools, TMPDIR: layout.temp, TMP: layout.temp, TEMP: layout.temp } : {}),
      RIGHTKIT_PARENT_REQUEST_ID: rootId,
      RIGHTKIT_PARENT_CAPABILITY_TOKEN: rootToken
    });
  }

  #startSampling(job) {
    const interval = integer(this.#config.bounds?.heartbeatIntervalMs, 5_000, 100);
    job.sampleTimer = setInterval(() => {
      const sample = { type: 'RUNNING', id: job.id, elapsedMs: this.#now() - job.startedAt, outputBytes: job.outputBytes, lastOutputAt: job.lastOutputAt ? new Date(job.lastOutputAt).toISOString() : null };
      Promise.resolve(this.#statfs(this.#config.broker.externalRoot.path)).then((value) => {
        sample.freeDiskBytes = Number(value.bavail) * Number(value.bsize);
        const floor = integer(this.#config.bounds?.freeFloorBytes, 0); if (floor && sample.freeDiskBytes < floor) this.#requestCancel(job, 'storage-floor');
      }).catch((error) => { sample.storageError = error.message; }).finally(() => { void this.#log(sample); });
    }, interval); job.sampleTimer.unref?.();
  }

  #output(job, type, chunk) {
    const data = Buffer.from(chunk); job.outputBytes += data.length; job.outputDigest.update(data); job.lastOutputAt = this.#now();
    const headLimit = integer(this.#config.bounds?.outputHeadBytes, 16 * 1024); const tailLimit = integer(this.#config.bounds?.outputTailBytes, 16 * 1024);
    if (job.outputHead.length < headLimit) job.outputHead = Buffer.concat([job.outputHead, data.subarray(0, headLimit - job.outputHead.length)]);
    job.outputTail = appendTail(job.outputTail, data, tailLimit);
    this.#publish(job, { type: 'JOB_EVENT', id: job.id, event: { type, data: data.toString('utf8') } });
  }

  #status(id, emit) {
    const live = this.#jobs.get(id);
    if (live) return this.#emit(emit, { type: 'STATUS', id, state: live.state, receipt: live.terminal?.receipt ?? null });
    return this.#state.receipt(id).then((receipt) => { if (!receipt) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' }); return this.#emit(emit, { type: 'STATUS', id, state: receipt.state, receipt }); });
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

  #cancel(id, emit) {
    const job = this.#jobs.get(id); if (!job) throw Object.assign(new Error('job not found'), { code: 'RK_JOB_NOT_FOUND' });
    const accepted = this.#requestCancel(job, 'explicit-cancel'); return this.#emit(emit, { type: 'CANCEL_REQUESTED', id, accepted, state: job.state });
  }

  #requestCancel(job, reason) {
    if (job.terminal || job.cancelReason) return false; job.cancelReason = reason;
    for (const child of this.#jobs.values()) if (child.parentId === job.id && !child.terminal) this.#requestCancel(child, `parent-${reason}`);
    if (job.ticket?.cancel()) return true; if (job.child) this.#terminateJob(job); return true;
  }

  #terminateJob(job) {
    if (!job.termination) job.termination = this.#terminate(job.child, { graceMs: integer(this.#config.bounds?.processGraceMs, 5_000, 1) }).catch((error) => ({ reaped: false, reason: error.message }));
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
