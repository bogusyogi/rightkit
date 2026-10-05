import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile, readdir, realpath, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, normalize, relative, sep } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const SCHEMA_VERSION = 1;
const STATE_FILE = 'state.json';
const IDENTITY_FILE = 'identity.json';
const ADOPTION_FILE = 'adoption.json';
const REPOSITORY_MARKERS = Object.freeze(['.rightkit-local-development.json', '.git']);
const WINDOWS_RENAME_RETRY_CODES = new Set(['EPERM', 'EBUSY', 'ENOTEMPTY']);
const WINDOWS_RENAME_RETRY_DELAYS_MS = Object.freeze([10, 25, 50, 100, 200, 400, 800]);

function requireAbsolute(value, label) {
  if (typeof value !== 'string' || !isAbsolute(value)) throw new TypeError(`${label} must be an absolute path`);
  return normalize(value);
}

function generationName(value) {
  if (!Number.isSafeInteger(value) || value < 1) throw new TypeError('generation must be a positive integer');
  return String(value).padStart(3, '0');
}

async function readJson(path, { missing = null } = {}) {
  try { return JSON.parse(await readFile(path, 'utf8')); }
  catch (error) {
    if (error.code === 'ENOENT') return missing;
    throw new Error(`invalid thin state JSON at ${path}: ${error.message}`, { cause: error });
  }
}

export async function replaceAtomicFile(source, target, { platform = process.platform, renameFile = rename, wait = delay, retryDelays = WINDOWS_RENAME_RETRY_DELAYS_MS } = {}) {
  let retry = 0;
  for (;;) {
    try { return await renameFile(source, target); }
    catch (error) {
      if (platform !== 'win32' || !WINDOWS_RENAME_RETRY_CODES.has(error.code) || retry >= retryDelays.length) throw error;
      await wait(retryDelays[retry]); retry += 1;
    }
  }
}

export async function atomicJson(path, value, { platform = process.platform, renameFile = rename, removeFile = rm, wait = delay, retryDelays = WINDOWS_RENAME_RETRY_DELAYS_MS } = {}) {
  await mkdir(dirname(path), { recursive: true });
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  await writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  try { await replaceAtomicFile(temporary, path, { platform, renameFile, wait, retryDelays }); }
  catch (error) {
    try { await removeFile(temporary, { force: true }); }
    catch (cleanupError) { error.cleanupError = cleanupError; }
    throw error;
  }
}

// Windows caps a path at 260 chars (MAX_PATH) and Strawberry Perl — the perl
// OpenSSL's Configure runs under — is not long-path aware. A vendored-OpenSSL
// compile nests ~96 chars of openssl-src tree under CARGO_TARGET_DIR, so the
// managed target root must stay under ~163 chars. The old
// `workspaces/<32 hex>/generations/000001` layout spent 60 of them on segment
// names and a needlessly wide key, and every vendored-OpenSSL build on Windows
// failed with "There are files missing" from files that were plainly present.
// `ws/<12 hex>/g/001` spends 19. 48 bits stays collision-proof across one
// host's repository set; build-control-core's `externalRoots` carries the same
// reasoning for its own namespace.
function workspaceKey(canonicalRoot) {
  return createHash('sha256').update(canonicalRoot).digest('hex').slice(0, 12);
}

function receiptId(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,199}$/.test(value)) throw new TypeError('safe receipt id is required');
  return value;
}

const residents = (state) => Array.isArray(state?.residents) ? state.residents : [];

export class ThinState {
  #base; #now; #locks = new Map();

  static async open({ externalRoot, now = () => new Date() } = {}) {
    const root = requireAbsolute(externalRoot, 'externalRoot');
    const state = new ThinState({ base: join(root, 'rightkit-build-control'), now });
    await state.#initialize();
    return state;
  }

  constructor({ base, now }) { this.#base = base; this.#now = now; }

  /** Serialize state transitions per workspace key (v2.8 §10.3): eviction,
   * rotation, writer/resident transitions, and finish re-read-then-write the same state file,
   * so within the singleton broker their critical sections must not interleave. */
  async #transition(key, action) {
    const previous = this.#locks.get(key) ?? Promise.resolve();
    let release; const gate = new Promise((resolve) => { release = resolve; });
    this.#locks.set(key, previous.then(() => gate));
    await previous;
    try { return await action(); } finally { release(); }
  }

  get paths() {
    return Object.freeze({
      base: this.#base,
      state: join(this.#base, 'state'),
      global: join(this.#base, 'global'),
      cargoHome: join(this.#base, 'global', 'cargo-home'),
      compilerCache: join(this.#base, 'global', 'compiler-cache'),
      // Keep process temporary paths shallow. sccache creates a startup socket
      // below TMPDIR, and macOS rejects Unix socket paths over 103 bytes.
      temporary: join(dirname(this.#base), 'tmp'),
      tools: join(this.#base, 'global', 'tools'),
      active: join(this.#base, 'global', 'active'),
      workspaces: join(this.#base, 'ws'),
      trash: join(this.#base, 'trash'),
      logs: join(this.#base, 'logs'),
      receipts: join(this.#base, 'receipts')
    });
  }

  async #initialize() {
    const paths = this.paths;
    await Promise.all(Object.values(paths).slice(1).map((path) => mkdir(path, { recursive: true })));
  }

  async workspace(root) {
    const requested = requireAbsolute(root, 'workspace root');
    const canonicalRoot = await realpath(requested);
    const key = workspaceKey(canonicalRoot);
    const directory = join(this.paths.workspaces, key);
    const identityPath = join(directory, IDENTITY_FILE);
    await mkdir(directory, { recursive: true });
    const identity = await readJson(identityPath);
    if (identity && identity.canonicalRoot !== canonicalRoot) throw new Error(`workspace identity collision for ${canonicalRoot}`);
    if (!identity) await atomicJson(identityPath, { schemaVersion: SCHEMA_VERSION, key, canonicalRoot });
    const adopted = await this.#adoption({ key, canonicalRoot, directory });
    if (adopted) {
      const adoptedDirectory = join(this.paths.workspaces, adopted.key);
      return this.#open({ key: adopted.key, canonicalRoot: adopted.canonicalRoot, directory: adoptedDirectory, identityPath: join(adoptedDirectory, IDENTITY_FILE), statePath: join(adoptedDirectory, STATE_FILE) });
    }
    return this.#open({ key, canonicalRoot, directory, identityPath, statePath: join(directory, STATE_FILE) });
  }

  /** Cache-root adoption (repository-root keying, 267bae38): the first time a
   * cache root is seen without a warm target, when exactly one warm Cargo
   * workspace of the same repository already exists below it, keep using that
   * workspace's key instead of starting cold. Nothing moves; the decision is
   * recorded once in adoption.json and never re-guessed. */
  async #adoption({ key, canonicalRoot, directory }) {
    const path = join(directory, ADOPTION_FILE);
    // A decline is tied to the generation it judged: after eviction rotates this root, decide again.
    const stale = async (decision) => !decision || (decision.result !== 'adopted' && Number.isSafeInteger(decision.ownGeneration) && decision.ownGeneration !== (await readJson(join(directory, STATE_FILE)))?.currentGeneration);
    const recorded = await readJson(path);
    if (await stale(recorded)) {
      return this.#transition(key, async () => {
        const again = await readJson(path);
        const decision = (await stale(again)) ? await this.#decideAdoption({ key, canonicalRoot, directory }) : again;
        if (decision !== again) await atomicJson(path, decision);
        return this.#adoptedTarget(decision);
      });
    }
    return this.#adoptedTarget(recorded);
  }

  async #adoptedTarget(decision) {
    if (decision?.result !== 'adopted' || typeof decision.key !== 'string' || !/^[0-9a-f]{12}$/.test(decision.key)) return null;
    const identity = await readJson(join(this.paths.workspaces, decision.key, IDENTITY_FILE));
    return identity?.canonicalRoot === decision.canonicalRoot ? Object.freeze({ key: decision.key, canonicalRoot: decision.canonicalRoot }) : null;
  }

  async #decideAdoption({ key, canonicalRoot, directory }) {
    const decidedAt = this.#timestamp();
    const own = await readJson(join(directory, STATE_FILE));
    const ownGeneration = own?.currentGeneration ?? 1; // a new root starts at generation 1
    // A non-empty tree is not necessarily warm (a lone `cargo check` leaves one): compare sizes.
    const ownBytes = own ? await directorySize(join(directory, 'g', generationName(own.currentGeneration))) : 0;
    const candidates = [];
    for (const entry of await readdir(this.paths.workspaces, { withFileTypes: true })) {
      if (!entry.isDirectory() || entry.name === key) continue;
      const candidateDirectory = join(this.paths.workspaces, entry.name);
      const identity = await readJson(join(candidateDirectory, IDENTITY_FILE)).catch(() => null);
      const candidateRoot = identity?.canonicalRoot;
      if (typeof candidateRoot !== 'string' || !isStrictlyWithin(canonicalRoot, candidateRoot)) continue;
      if ((await readJson(join(candidateDirectory, ADOPTION_FILE)).catch(() => null))?.result === 'adopted') continue;
      if (!(await pathExists(candidateRoot)) || await insideNestedRepository(canonicalRoot, candidateRoot)) continue;
      const state = await readJson(join(candidateDirectory, STATE_FILE)).catch(() => null);
      if (!state || !Number.isSafeInteger(state.currentGeneration) || state.currentGeneration < 1) continue;
      const generationPath = join(candidateDirectory, 'g', generationName(state.currentGeneration));
      if (!(await warmGeneration(generationPath))) continue;
      const sizeBytes = state.lastMeasuredGeneration === state.currentGeneration && Number.isSafeInteger(state.lastMeasuredBytes) && !state.sizeDirty ? state.lastMeasuredBytes : await directorySize(generationPath);
      candidates.push({ key: entry.name, canonicalRoot: candidateRoot, lastUsedAt: state.lastUsedAt ?? null, sizeBytes });
    }
    const base = { schemaVersion: SCHEMA_VERSION, ownGeneration, ownBytes, decidedAt };
    if (candidates.length === 1 && candidates[0].sizeBytes > 2 * ownBytes) return { ...base, result: 'adopted', key: candidates[0].key, canonicalRoot: candidates[0].canonicalRoot, reason: ownBytes ? 'single-larger-warm-workspace-below-cache-root' : 'single-warm-workspace-below-cache-root' };
    if (ownBytes > 0 && candidates.every((candidate) => candidate.sizeBytes <= 2 * ownBytes)) return { ...base, result: 'own-target-warm', candidates };
    return { ...base, result: candidates.length ? 'ambiguous' : 'none', candidates };
  }

  async #open(workspace) {
    const { key, canonicalRoot, directory, identityPath, statePath } = workspace;
    let stored = await readJson(statePath);
    if (!stored) {
      stored = { schemaVersion: SCHEMA_VERSION, currentGeneration: 1, active: null, residents: [], updatedAt: this.#timestamp() };
      await atomicJson(statePath, stored);
    }
    this.#validateState(stored, statePath);
    const name = generationName(stored.currentGeneration);
    if (!isLeafEntry(stored.leaves?.[name])) {
      // First sight of this generation: select once from what exists on disk, persist, never re-guess.
      const selected = await this.#selectLeaf(workspace, stored.currentGeneration);
      stored = { ...stored, leaves: { ...(stored.leaves ?? {}), [name]: selected } };
      await atomicJson(statePath, stored);
    }
    await this.#ensureGeneration(workspace, stored.currentGeneration, stored);
    return Object.freeze({ ...workspace, state: Object.freeze({ ...stored }), layout: await this.#layoutWithOrphan(workspace, stored.currentGeneration, stored) });
  }

  async markRunning(workspaceRoot, { id, ownership = null, command = null } = {}) {
    receiptId(id);
    const workspace = await this.workspace(workspaceRoot);
    return this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      if (state.active) throw Object.assign(new Error(`workspace already has active root ${state.active.id}`), { code: 'RK_WORKSPACE_ACTIVE' });
      const active = { id, generation: state.currentGeneration, startedAt: this.#timestamp(), ownership, command };
      const next = { ...state, active, lastUsedAt: this.#timestamp(), updatedAt: this.#timestamp() };
      await atomicJson(workspace.statePath, next);
      await atomicJson(join(this.paths.active, `${id}.json`), { workspaceKey: workspace.key, ...active });
      return Object.freeze({ ...workspace, state: Object.freeze(next), layout: this.#layout(workspace, state.currentGeneration, next, workspace.layout?.orphanLeaf) });
    });
  }

  async markResident(workspaceRoot, id) {
    receiptId(id);
    const workspace = await this.workspace(workspaceRoot);
    return this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      if (state.active?.id !== id) throw Object.assign(new Error(`job ${id} is not active`), { code: 'RK_JOB_NOT_ACTIVE' });
      const resident = { ...state.active, residentAt: this.#timestamp() };
      const next = { ...state, active: null, residents: [...residents(state), resident], sizeDirty: true, updatedAt: this.#timestamp() };
      await atomicJson(workspace.statePath, next);
      await atomicJson(join(this.paths.active, `${id}.json`), { workspaceKey: workspace.key, mode: 'RESIDENT', ...resident });
      return Object.freeze({ ...workspace, state: Object.freeze(next), layout: this.#layout(workspace, state.currentGeneration, next, workspace.layout?.orphanLeaf) });
    });
  }

  async updateOwnership(workspaceRoot, id, ownership) {
    receiptId(id);
    const workspace = await this.workspace(workspaceRoot);
    return this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      if (state.active?.id !== id) throw Object.assign(new Error(`job ${id} is not active`), { code: 'RK_JOB_NOT_ACTIVE' });
      const active = { ...state.active, ownership };
      const next = { ...state, active, updatedAt: this.#timestamp() };
      await atomicJson(workspace.statePath, next);
      await atomicJson(join(this.paths.active, `${id}.json`), { workspaceKey: workspace.key, ...active });
      return Object.freeze(next);
    });
  }

  async finish(workspaceRoot, id, receipt) {
    receiptId(id);
    const workspace = await this.workspace(workspaceRoot);
    await this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      const next = {
        ...state,
        active: state.active?.id === id ? null : state.active,
        residents: residents(state).filter((entry) => entry.id !== id),
        sizeDirty: true,
        updatedAt: this.#timestamp()
      };
      await atomicJson(workspace.statePath, next);
    });
    await this.writeReceipt(id, receipt);
    await rm(join(this.paths.active, `${id}.json`), { force: true });
    return this.receipt(id);
  }

  async receipt(id) { return readJson(join(this.paths.receipts, `${receiptId(id)}.json`)); }

  async writeReceipt(id, receipt) {
    receiptId(id);
    await atomicJson(join(this.paths.receipts, `${id}.json`), { ...receipt, id });
    return this.receipt(id);
  }

  async rotate(workspaceRoot, reason = 'manual') {
    const workspace = await this.workspace(workspaceRoot);
    return this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      const currentGeneration = state.currentGeneration + 1;
      const next = { ...state, currentGeneration, active: null, residents: residents(state), rotatedFrom: state.currentGeneration, rotationReason: reason, leaves: { ...(state.leaves ?? {}), [generationName(currentGeneration)]: newGenerationLeaf(this.#timestamp()) }, sizeDirty: true, updatedAt: this.#timestamp() };
      await this.#ensureGeneration(workspace, currentGeneration, next);
      await atomicJson(workspace.statePath, next);
      if (state.active?.id) await rm(join(this.paths.active, `${state.active.id}.json`), { force: true });
      return Object.freeze({ ...workspace, state: Object.freeze(next), layout: this.#layout(workspace, currentGeneration, next, null) });
    });
  }

  /** Pin a native cache entry to a workspace generation for as long as that generation is not retired. */
  async recordNativeReference(workspaceRoot, generation, kind, key) {
    const workspace = await this.workspace(workspaceRoot);
    return this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      const name = generationName(generation);
      if (state.natives?.[name]?.[kind] === key) return state;
      const next = { ...state, natives: { ...(state.natives ?? {}), [name]: { ...(state.natives?.[name] ?? {}), [kind]: key } }, updatedAt: this.#timestamp() };
      await atomicJson(workspace.statePath, next);
      return next;
    });
  }

  /** Keys referenced by any non-retired generation (current, active, or resident) across all workspaces. */
  async pinnedNativeKeys(kind) {
    const pinned = new Set();
    for (const entry of await readdir(this.paths.workspaces, { withFileTypes: true }).catch(() => [])) {
      if (!entry.isDirectory()) continue;
      const state = await readJson(join(this.paths.workspaces, entry.name, STATE_FILE), { missing: null }).catch(() => null);
      if (!state?.natives) continue;
      const live = new Set([state.currentGeneration, state.active?.generation, ...residents(state).map((resident) => resident.generation)].filter((value) => Number.isSafeInteger(value)));
      for (const generation of live) { const key = state.natives[generationName(generation)]?.[kind]; if (key) pinned.add(key); }
    }
    return pinned;
  }

  async recoverActive({ reap = async () => ({ reaped: false, reason: 'reap-not-configured' }) } = {}) {
    const recovered = [];
    for (const entry of await readdir(this.paths.workspaces, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const directory = join(this.paths.workspaces, entry.name);
      const identity = await readJson(join(directory, IDENTITY_FILE));
      const state = await readJson(join(directory, STATE_FILE));
      if (!identity || (!state?.active && residents(state).length === 0)) continue;
      if (state.active) {
        const active = state.active;
        const reapResult = await Promise.resolve().then(() => reap(active.ownership)).catch((error) => ({ reaped: false, reason: error.message }));
        const rotated = await this.rotate(identity.canonicalRoot, 'broker-restart-active-writer');
        const receipt = {
          id: active.id,
          state: 'ABANDONED',
          status: 'ABANDONED',
          workspace: identity.canonicalRoot,
          generation: active.generation,
          recoveredAt: this.#timestamp(),
          reap: reapResult
        };
        await atomicJson(join(this.paths.receipts, `${receiptId(active.id)}.json`), receipt);
        recovered.push(Object.freeze({ workspaceKey: entry.name, mode: 'WRITER', active: Object.freeze(active), newGeneration: rotated.state.currentGeneration, reap: Object.freeze({ ...reapResult }) }));
      }
      const currentState = await readJson(join(directory, STATE_FILE));
      for (const resident of residents(currentState)) {
        const reapResult = await Promise.resolve().then(() => reap(resident.ownership)).catch((error) => ({ reaped: false, reason: error.message }));
        let current = await this.workspace(identity.canonicalRoot);
        if (reapResult.reaped === false && current.state.currentGeneration === resident.generation) current = await this.rotate(identity.canonicalRoot, 'broker-restart-unreaped-resident');
        const receipt = {
          id: resident.id,
          state: 'ABANDONED',
          status: 'ABANDONED',
          workspace: identity.canonicalRoot,
          generation: resident.generation,
          recoveredAt: this.#timestamp(),
          processMayBeLive: reapResult.reaped === false,
          reap: reapResult
        };
        if (reapResult.reaped === false) await this.writeReceipt(resident.id, receipt);
        else await this.finish(identity.canonicalRoot, resident.id, receipt);
        recovered.push(Object.freeze({ workspaceKey: entry.name, mode: 'RESIDENT', active: Object.freeze(resident), newGeneration: current.state.currentGeneration, reap: Object.freeze({ ...reapResult }) }));
      }
    }
    return Object.freeze(recovered);
  }

  /** Record an advisory measured size for the current generation (v2.8 §10.4).
   * Size is advisory; correctness comes from exclusion, rotation, and the
   * free-disk floor — never from byte-exact accounting. */
  async recordMeasured(workspaceRoot, generation, sizeBytes) {
    const workspace = await this.workspace(workspaceRoot);
    if (!Number.isSafeInteger(generation) || generation < 1 || !Number.isSafeInteger(sizeBytes) || sizeBytes < 0) throw new TypeError('generation & non-negative sizeBytes are required');
    return this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      if (state.currentGeneration !== generation) return Object.freeze({ ...state });
      const next = { ...state, lastMeasuredGeneration: generation, lastMeasuredBytes: sizeBytes, lastMeasuredAt: this.#timestamp(), sizeDirty: false, updatedAt: this.#timestamp() };
      await atomicJson(workspace.statePath, next);
      return Object.freeze(next);
    });
  }

  /** Retire an idle current generation by advancing the workspace to a fresh
   * generation, then renaming the retired generation into trash (v2.8 §10.3).
   * A current generation is never deleted in place: the state transition to
   * generation N+1 is persisted before the old directory is renamed. The
   * caller must already hold the per-workspace write exclusion (scheduler)
   * when invoked from the broker. Returns null when the workspace has an active
   * writer or resident executable — an in-use generation is never evicted. */
  async evictIdleWorkspace(workspaceRoot, { reason = 'cache-budget-eviction', sizeBytes = null } = {}) {
    const workspace = await this.workspace(workspaceRoot);
    return this.#transition(workspace.key, async () => {
      const state = await readJson(workspace.statePath);
      if (state.active || residents(state).length > 0) return null;
      const currentGeneration = state.currentGeneration;
      const oldRoot = this.#layout(workspace, currentGeneration, state).root;
      const measured = Number.isSafeInteger(sizeBytes) && sizeBytes >= 0 ? sizeBytes : await directorySize(oldRoot);
      const nextGeneration = currentGeneration + 1;
      // The fresh generation starts empty, so its measured size is 0 & clean.
      const next = { ...state, currentGeneration: nextGeneration, active: null, rotatedFrom: currentGeneration, rotationReason: reason, leaves: { ...(state.leaves ?? {}), [generationName(nextGeneration)]: newGenerationLeaf(this.#timestamp()) }, lastMeasuredGeneration: nextGeneration, lastMeasuredBytes: 0, lastMeasuredAt: this.#timestamp(), sizeDirty: false, updatedAt: this.#timestamp() };
      await this.#ensureGeneration(workspace, nextGeneration, next);
      await atomicJson(workspace.statePath, next);
      const trash = join(this.paths.trash, `${workspace.key}-${generationName(currentGeneration)}-${randomUUID()}`);
      await rename(oldRoot, trash);
      await rm(trash, { recursive: true, force: true });
      return Object.freeze({ workspaceKey: workspace.key, canonicalRoot: workspace.canonicalRoot, evictedGeneration: currentGeneration, newGeneration: nextGeneration, sizeBytes: measured, reason });
    });
  }

  /** Delete oldest retired generations first when total generation storage is
   * above cap; then, when an evictIdle callback is supplied, retire idle
   * current generations least-recently-used first until under cap. Current and
   * active-writer & resident generations are never direct deletion candidates. Sizes are
   * cached per current generation and invalidated by the sizeDirty flag so GC
   * does not repeatedly walk warm target trees (v2.8 §10.5). */
  async gc({ capBytes, evictIdle = null, freeBytes = null, freeFloorBytes = null, recentMs = 72 * 3_600_000, staleMs = 14 * 86_400_000 } = {}) {
    if (!Number.isSafeInteger(capBytes) || capBytes < 1) throw new TypeError('gc capBytes must be a positive integer or null');
    if (evictIdle !== null && typeof evictIdle !== 'function') throw new TypeError('evictIdle must be a callback or null');
    if (freeBytes !== null && typeof freeBytes !== 'function') throw new TypeError('freeBytes must be a callback or null');
    const retired = []; const idleCurrent = []; const deadRoots = []; const orphanLeaves = []; const adoptedKeys = new Set(); let total = 0; let advisory = 0;
    for (const workspaceEntry of await readdir(this.paths.workspaces, { withFileTypes: true })) {
      if (!workspaceEntry.isDirectory()) continue;
      const directory = join(this.paths.workspaces, workspaceEntry.name);
      const adoption = await readJson(join(directory, ADOPTION_FILE)).catch(() => null);
      if (adoption?.result === 'adopted' && typeof adoption.key === 'string') adoptedKeys.add(adoption.key);
      const state = await readJson(join(directory, STATE_FILE), { missing: null });
      if (!state) continue;
      const identity = await readJson(join(directory, IDENTITY_FILE), { missing: null });
      const generations = join(directory, 'g');
      let workspaceBytes = 0; let anyActive = false; let currentIdle = null;
      for (const generationEntry of await readdir(generations, { withFileTypes: true }).catch((error) => error.code === 'ENOENT' ? [] : Promise.reject(error))) {
        if (!generationEntry.isDirectory() || !/^\d{3}$/.test(generationEntry.name)) continue;
        const generation = Number(generationEntry.name); const path = join(generations, generationEntry.name); const metadata = await lstat(path);
        const isCurrent = generation === state.currentGeneration;
        const isActive = generation === state.active?.generation || residents(state).some((resident) => resident.generation === generation);
        anyActive ||= isActive;
        let sizeBytes;
        if (isCurrent && !state.sizeDirty && state.lastMeasuredGeneration === generation && Number.isSafeInteger(state.lastMeasuredBytes)) sizeBytes = state.lastMeasuredBytes;
        else if (isActive) {
          // Leased/active trees are being written by a compiler; walking them races rustc* temp
          // churn. Use the advisory size (last measurement, else 0) and never scan.
          sizeBytes = state.lastMeasuredGeneration === generation && Number.isSafeInteger(state.lastMeasuredBytes) ? state.lastMeasuredBytes : 0;
          advisory += 1;
        } else sizeBytes = await directorySize(path);
        total += sizeBytes; workspaceBytes += sizeBytes;
        if (isCurrent) {
          if (!isActive && (state.lastMeasuredGeneration !== generation || state.sizeDirty)) await this.recordMeasured(identity?.canonicalRoot ?? workspaceEntry.name, generation, sizeBytes).catch(() => null);
          if (!isActive && identity?.canonicalRoot) currentIdle = { workspaceKey: workspaceEntry.name, canonicalRoot: identity.canonicalRoot, generation, sizeBytes, lastUsedAt: state.lastUsedAt ?? null, updatedAt: state.updatedAt ?? null, modifiedMs: metadata.mtimeMs };
          // A persisted leaf makes its sibling leaf unreachable: reclaim it on its own (design §7).
          const entry = state.leaves?.[generationEntry.name];
          if (!isActive && isLeafEntry(entry)) {
            const other = join(path, entry.leaf === 'target' ? 't' : 'target');
            if (await nonEmptyDir(other)) orphanLeaves.push({ workspaceKey: workspaceEntry.name, generation, leaf: entry.leaf, path: other, sizeBytes: await directorySize(other) });
          }
        } else if (!isActive) retired.push({ workspaceKey: workspaceEntry.name, generation, path, sizeBytes, modifiedMs: metadata.mtimeMs });
      }
      // A workspace whose source root no longer exists can never build again, and eviction through
      // workspace() fails on realpath, so it is reclaimed whole here instead.
      if (!anyActive && typeof identity?.canonicalRoot === 'string' && !(await pathExists(identity.canonicalRoot))) deadRoots.push({ workspaceKey: workspaceEntry.name, canonicalRoot: identity.canonicalRoot, directory, sizeBytes: workspaceBytes });
      else if (currentIdle) idleCurrent.push(currentIdle);
    }
    const beforeBytes = total; const deleted = []; const evicted = []; const reclaimed = { deadRoots: [], orphanLeaves: [] };
    const discard = async (label, path) => {
      const trash = join(this.paths.trash, `${label}-${randomUUID()}`);
      try { await rename(path, trash); } catch (error) { if (error?.code === 'ENOENT') return; throw error; }
      await rm(trash, { recursive: true, force: true });
    };
    for (const candidate of deadRoots) {
      if (adoptedKeys.has(candidate.workspaceKey)) continue;
      const removed = await this.#transition(candidate.workspaceKey, async () => {
        const fresh = await readJson(join(candidate.directory, STATE_FILE), { missing: null });
        if (fresh && (fresh.active || residents(fresh).length > 0)) return false;
        if (await pathExists(candidate.canonicalRoot)) return false;
        await discard(candidate.workspaceKey, candidate.directory); return true;
      });
      if (!removed) continue;
      total -= candidate.sizeBytes;
      reclaimed.deadRoots.push(Object.freeze({ workspaceKey: candidate.workspaceKey, canonicalRoot: candidate.canonicalRoot, sizeBytes: candidate.sizeBytes }));
    }
    for (const candidate of orphanLeaves) {
      const removed = await this.#transition(candidate.workspaceKey, async () => {
        const statePath = join(this.paths.workspaces, candidate.workspaceKey, STATE_FILE);
        const fresh = await readJson(statePath, { missing: null });
        if (!fresh || fresh.active || residents(fresh).length > 0 || fresh.currentGeneration !== candidate.generation || fresh.leaves?.[generationName(candidate.generation)]?.leaf !== candidate.leaf) return false;
        await discard(`${candidate.workspaceKey}-${generationName(candidate.generation)}-orphan`, candidate.path);
        await atomicJson(statePath, { ...fresh, sizeDirty: true, updatedAt: this.#timestamp() });
        return true;
      });
      if (!removed) continue;
      total -= candidate.sizeBytes;
      reclaimed.orphanLeaves.push(Object.freeze({ workspaceKey: candidate.workspaceKey, generation: candidate.generation, path: candidate.path, sizeBytes: candidate.sizeBytes }));
    }
    // Effective cap: the configured ceiling, further bounded by keeping the managed volume's free floor.
    const free = freeBytes ? await Promise.resolve(freeBytes()).catch(() => null) : null;
    const floor = Number.isSafeInteger(freeFloorBytes) && freeFloorBytes > 0 && Number.isFinite(free) ? freeFloorBytes : null;
    const effectiveCapBytes = floor === null ? capBytes : Math.max(0, Math.min(capBytes, total + free - floor));
    const pressure = floor !== null && free < floor;
    // Hysteresis: once over cap, reclaim to 90 % so a small overage does not re-trigger eviction on every build.
    const goal = total > effectiveCapBytes ? Math.floor(effectiveCapBytes * 0.9) : null;
    if (goal !== null) for (const candidate of retired.sort((left, right) => left.modifiedMs - right.modifiedMs || left.path.localeCompare(right.path))) {
      if (total <= goal) break;
      // Revalidate under the per-workspace exclusion: the generation may have become current/active since the scan.
      const removed = await this.#transition(candidate.workspaceKey, async () => {
        const fresh = await readJson(join(this.paths.workspaces, candidate.workspaceKey, STATE_FILE), { missing: null });
        if (!fresh || fresh.currentGeneration === candidate.generation || fresh.active?.generation === candidate.generation || residents(fresh).some((resident) => resident.generation === candidate.generation)) return false;
        await discard(`${candidate.workspaceKey}-${generationName(candidate.generation)}`, candidate.path);
        return true;
      });
      if (!removed) continue;
      total -= candidate.sizeBytes;
      deleted.push(Object.freeze({ workspaceKey: candidate.workspaceKey, generation: candidate.generation, sizeBytes: candidate.sizeBytes }));
    }
    if (evictIdle) {
      const now = Date.parse(this.#timestamp());
      const lru = (candidate) => Date.parse(candidate.lastUsedAt ?? '') || Date.parse(candidate.updatedAt ?? '') || candidate.modifiedMs;
      // Empty generations reclaim nothing; rotating them only churns generation numbers.
      const ordered = idleCurrent.filter((candidate) => candidate.sizeBytes > 0).sort((left, right) => lru(left) - lru(right) || left.workspaceKey.localeCompare(right.workspaceKey));
      const evict = async (candidate, reason) => {
        const result = await Promise.resolve(evictIdle({ ...candidate, reason })).catch(() => null);
        if (!result) return;
        total -= candidate.sizeBytes; evicted.push(Object.freeze(result)); candidate.evicted = true;
      };
      // Stale trees are retired regardless of cap; recently used trees are protected unless the disk floor is breached.
      for (const candidate of ordered) if (now - lru(candidate) > staleMs) await evict(candidate, 'stale-idle-eviction');
      if (goal !== null) for (const candidate of ordered) {
        if (total <= goal) break;
        if (candidate.evicted || (!pressure && now - lru(candidate) < recentMs)) continue;
        await evict(candidate, pressure ? 'disk-floor-eviction' : 'cache-budget-eviction');
      }
    }
    const unenforceable = total > effectiveCapBytes ? Object.freeze({ overByBytes: total - effectiveCapBytes, idleCandidatesRemaining: idleCurrent.filter((candidate) => !candidate.evicted && candidate.sizeBytes > 0).length }) : null;
    return Object.freeze({ capBytes, effectiveCapBytes, freeBytes: free, freeFloorBytes: floor, beforeBytes, afterBytes: total, deleted: Object.freeze(deleted), evicted: Object.freeze(evicted), reclaimed: Object.freeze(reclaimed), unenforceable, advisorySizedTrees: advisory });
  }

  #layout(workspace, generation, state = null, orphanLeaf = null) {
    const root = join(workspace.directory, 'g', generationName(generation));
    // The leaf is chosen once per generation (see #selectLeaf) and persisted in state.leaves; it is never
    // re-derived from the platform, so an existing warm tree is never abandoned for a different leaf name.
    const entry = state?.leaves?.[generationName(generation)];
    const leaf = isLeafEntry(entry) ? entry.leaf : DEFAULT_LEAF;
    const leafReason = isLeafEntry(entry) ? `persisted (${entry.reason ?? 'unrecorded'})` : 'default-unpersisted';
    return Object.freeze({ generation, root, target: join(root, leaf), leaf, leafReason, orphanLeaf, temp: this.paths.temporary });
  }

  async #layoutWithOrphan(workspace, generation, state) {
    const base = this.#layout(workspace, generation, state);
    const other = base.leaf === 'target' ? 't' : 'target';
    const otherPath = join(base.root, other);
    const orphanLeaf = (await nonEmptyDir(otherPath)) ? Object.freeze({ leaf: other, path: otherPath, sizeBytes: null }) : null;
    return Object.freeze({ ...base, orphanLeaf });
  }

  /** Pick the leaf for a generation that has none recorded: the one existing leaf; when both hold data, the
   * one with newer build activity; otherwise `target`. Never creates or deletes anything. */
  async #selectLeaf(workspace, generation) {
    const root = join(workspace.directory, 'g', generationName(generation));
    const at = this.#timestamp();
    const [hasTarget, hasT] = await Promise.all([nonEmptyDir(join(root, 'target')), nonEmptyDir(join(root, 't'))]);
    if (hasTarget && !hasT) return { leaf: 'target', reason: 'only-target-nonempty', selectedAt: at };
    if (hasT && !hasTarget) return { leaf: 't', reason: 'only-t-nonempty', selectedAt: at };
    if (hasT && hasTarget) {
      const [targetMs, tMs] = await Promise.all([latestBuildActivityMs(join(root, 'target')), latestBuildActivityMs(join(root, 't'))]);
      const leaf = tMs > targetMs ? 't' : 'target';
      return { leaf, reason: `both-nonempty-newer-activity:${leaf} (target=${targetMs}, t=${tMs})`, selectedAt: at };
    }
    return { leaf: DEFAULT_LEAF, reason: 'neither-exists-default-target', selectedAt: at };
  }

  async #ensureGeneration(workspace, generation, state = null) {
    const layout = this.#layout(workspace, generation, state);
    await Promise.all([layout.target, layout.temp].map((path) => mkdir(path, { recursive: true })));
  }

  #timestamp() {
    const value = this.#now();
    return (value instanceof Date ? value : new Date(value)).toISOString();
  }

  #validateState(state, path) {
    if (state?.schemaVersion !== SCHEMA_VERSION || !Number.isSafeInteger(state.currentGeneration) || state.currentGeneration < 1 || (state.active !== null && typeof state.active !== 'object') || (state.residents !== undefined && !Array.isArray(state.residents))) throw new Error(`unsupported thin state at ${path}`);
  }
}

const DEFAULT_LEAF = 'target';
const isLeafEntry = (value) => value !== null && typeof value === 'object' && (value.leaf === 'target' || value.leaf === 't');
const newGenerationLeaf = (selectedAt) => ({ leaf: DEFAULT_LEAF, reason: 'new-generation-default-target', selectedAt });

const isStrictlyWithin = (root, candidate) => { const child = relative(root, candidate); return child !== '' && child !== '..' && !child.startsWith(`..${sep}`) && !isAbsolute(child); };

async function pathExists(path) {
  try { await lstat(path); return true; }
  catch (error) { if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') return false; throw error; }
}

/** True when a repository marker sits between `root` (exclusive) and `candidate` (inclusive). */
async function insideNestedRepository(root, candidate) {
  for (let current = candidate; isStrictlyWithin(root, current); current = dirname(current)) {
    for (const marker of REPOSITORY_MARKERS) if (await pathExists(join(current, marker))) return true;
  }
  return false;
}

async function warmGeneration(root) {
  return (await nonEmptyDir(join(root, 'target'))) || (await nonEmptyDir(join(root, 't')));
}

async function nonEmptyDir(path) {
  try { return (await readdir(path)).length > 0; }
  catch (error) { if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') return false; throw error; }
}

async function mtimeMs(path) {
  try { return (await lstat(path)).mtimeMs; } catch { return 0; }
}

/** Newest build activity inside a target leaf: `.rustc_info.json` plus the `.fingerprint` directories (and their
 * immediate entries) under `<leaf>/<triple>/<profile>/` and `<leaf>/<profile>/`. Bounded; never a full walk. */
export async function latestBuildActivityMs(leaf, { readdirFn = readdir } = {}) {
  let newest = await mtimeMs(join(leaf, '.rustc_info.json'));
  const fingerprintDirs = [];
  let level1 = [];
  try { level1 = await readdirFn(leaf, { withFileTypes: true }); } catch { return newest; }
  for (const first of level1) {
    if (!first.isDirectory()) continue;
    fingerprintDirs.push(join(leaf, first.name, '.fingerprint'));
    let level2 = [];
    try { level2 = await readdirFn(join(leaf, first.name), { withFileTypes: true }); } catch { continue; }
    for (const second of level2) if (second.isDirectory()) fingerprintDirs.push(join(leaf, first.name, second.name, '.fingerprint'));
  }
  for (const directory of fingerprintDirs) {
    newest = Math.max(newest, await mtimeMs(directory));
    let entries = [];
    try { entries = await readdirFn(directory); } catch { continue; }
    for (const name of entries) newest = Math.max(newest, await mtimeMs(join(directory, name)));
  }
  return newest;
}

export const thinWorkspaceKey = workspaceKey;
export const thinGenerationName = generationName;

const VANISHED = new Set(['ENOENT', 'ENOTDIR']);

/** Recursive size that tolerates entries disappearing mid-scan (rustc* temp
 * dirs, cargo lock files). Vanished entries contribute 0. */
export async function directorySize(root, { readdirFn = readdir, lstatFn = lstat } = {}) {
  let total = 0; let entries;
  try { entries = await readdirFn(root, { withFileTypes: true }); }
  catch (error) { if (VANISHED.has(error?.code)) return 0; throw error; }
  for (const entry of entries) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) total += await directorySize(path, { readdirFn, lstatFn });
    else if (entry.isFile()) {
      try { total += (await lstatFn(path)).size; }
      catch (error) { if (!VANISHED.has(error?.code)) throw error; }
    }
  }
  return total;
}
