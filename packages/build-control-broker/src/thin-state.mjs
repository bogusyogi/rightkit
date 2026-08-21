import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, realpath, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, normalize } from 'node:path';

const SCHEMA_VERSION = 1;
const STATE_FILE = 'state.json';
const IDENTITY_FILE = 'identity.json';

function requireAbsolute(value, label) {
  if (typeof value !== 'string' || !isAbsolute(value)) throw new TypeError(`${label} must be an absolute path`);
  return normalize(value);
}

function generationName(value) {
  if (!Number.isSafeInteger(value) || value < 1) throw new TypeError('generation must be a positive integer');
  return String(value).padStart(6, '0');
}

async function readJson(path, { missing = null } = {}) {
  try { return JSON.parse(await readFile(path, 'utf8')); }
  catch (error) {
    if (error.code === 'ENOENT') return missing;
    throw new Error(`invalid thin state JSON at ${path}: ${error.message}`, { cause: error });
  }
}

async function atomicJson(path, value) {
  await mkdir(dirname(path), { recursive: true });
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  await writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
  await rename(temporary, path);
}

function workspaceKey(canonicalRoot) {
  return createHash('sha256').update(canonicalRoot).digest('hex').slice(0, 32);
}

function receiptId(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,199}$/.test(value)) throw new TypeError('safe receipt id is required');
  return value;
}

export class ThinState {
  #base; #now;

  static async open({ externalRoot, now = () => new Date() } = {}) {
    const root = requireAbsolute(externalRoot, 'externalRoot');
    const state = new ThinState({ base: join(root, 'rightkit-build-control'), now });
    await state.#initialize();
    return state;
  }

  constructor({ base, now }) { this.#base = base; this.#now = now; }

  get paths() {
    return Object.freeze({
      base: this.#base,
      state: join(this.#base, 'state'),
      global: join(this.#base, 'global'),
      cargoHome: join(this.#base, 'global', 'cargo-home'),
      compilerCache: join(this.#base, 'global', 'compiler-cache'),
      tools: join(this.#base, 'global', 'tools'),
      active: join(this.#base, 'global', 'active'),
      workspaces: join(this.#base, 'workspaces'),
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
    const statePath = join(directory, STATE_FILE);
    await mkdir(directory, { recursive: true });
    const identity = await readJson(identityPath);
    if (identity && identity.canonicalRoot !== canonicalRoot) throw new Error(`workspace identity collision for ${canonicalRoot}`);
    if (!identity) await atomicJson(identityPath, { schemaVersion: SCHEMA_VERSION, key, canonicalRoot });
    let stored = await readJson(statePath);
    if (!stored) {
      stored = { schemaVersion: SCHEMA_VERSION, currentGeneration: 1, active: null, updatedAt: this.#timestamp() };
      await atomicJson(statePath, stored);
    }
    this.#validateState(stored, statePath);
    const workspace = { key, canonicalRoot, directory, identityPath, statePath };
    await this.#ensureGeneration(workspace, stored.currentGeneration);
    return Object.freeze({ ...workspace, state: Object.freeze({ ...stored }), layout: this.#layout(workspace, stored.currentGeneration) });
  }

  async markRunning(workspaceRoot, { id, ownership = null, command = null } = {}) {
    receiptId(id);
    const workspace = await this.workspace(workspaceRoot);
    const state = await readJson(workspace.statePath);
    if (state.active) throw Object.assign(new Error(`workspace already has active root ${state.active.id}`), { code: 'RK_WORKSPACE_ACTIVE' });
    const active = { id, generation: state.currentGeneration, startedAt: this.#timestamp(), ownership, command };
    const next = { ...state, active, updatedAt: this.#timestamp() };
    await atomicJson(workspace.statePath, next);
    await atomicJson(join(this.paths.active, `${id}.json`), { workspaceKey: workspace.key, ...active });
    return Object.freeze({ ...workspace, state: Object.freeze(next), layout: this.#layout(workspace, state.currentGeneration) });
  }

  async updateOwnership(workspaceRoot, id, ownership) {
    receiptId(id);
    const workspace = await this.workspace(workspaceRoot);
    const state = await readJson(workspace.statePath);
    if (state.active?.id !== id) throw Object.assign(new Error(`job ${id} is not active`), { code: 'RK_JOB_NOT_ACTIVE' });
    const active = { ...state.active, ownership };
    const next = { ...state, active, updatedAt: this.#timestamp() };
    await atomicJson(workspace.statePath, next);
    await atomicJson(join(this.paths.active, `${id}.json`), { workspaceKey: workspace.key, ...active });
    return Object.freeze(next);
  }

  async finish(workspaceRoot, id, receipt) {
    receiptId(id);
    const workspace = await this.workspace(workspaceRoot);
    const state = await readJson(workspace.statePath);
    if (state.active?.id === id) await atomicJson(workspace.statePath, { ...state, active: null, updatedAt: this.#timestamp() });
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
    const state = await readJson(workspace.statePath);
    const currentGeneration = state.currentGeneration + 1;
    const next = { ...state, currentGeneration, active: null, rotatedFrom: state.currentGeneration, rotationReason: reason, updatedAt: this.#timestamp() };
    await this.#ensureGeneration(workspace, currentGeneration);
    await atomicJson(workspace.statePath, next);
    if (state.active?.id) await rm(join(this.paths.active, `${state.active.id}.json`), { force: true });
    return Object.freeze({ ...workspace, state: Object.freeze(next), layout: this.#layout(workspace, currentGeneration) });
  }

  async recoverActive({ reap = async () => ({ reaped: false, reason: 'reap-not-configured' }) } = {}) {
    const recovered = [];
    for (const entry of await readdir(this.paths.workspaces, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const directory = join(this.paths.workspaces, entry.name);
      const identity = await readJson(join(directory, IDENTITY_FILE));
      const state = await readJson(join(directory, STATE_FILE));
      if (!identity || !state?.active) continue;
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
      recovered.push(Object.freeze({ workspaceKey: entry.name, active: Object.freeze(active), newGeneration: rotated.state.currentGeneration, reap: Object.freeze({ ...reapResult }) }));
    }
    return Object.freeze(recovered);
  }

  #layout(workspace, generation) {
    const root = join(workspace.directory, 'generations', generationName(generation));
    return Object.freeze({ generation, root, target: join(root, 'target'), temp: join(root, 'tmp') });
  }

  async #ensureGeneration(workspace, generation) {
    const layout = this.#layout(workspace, generation);
    await Promise.all([layout.target, layout.temp].map((path) => mkdir(path, { recursive: true })));
  }

  #timestamp() {
    const value = this.#now();
    return (value instanceof Date ? value : new Date(value)).toISOString();
  }

  #validateState(state, path) {
    if (state?.schemaVersion !== SCHEMA_VERSION || !Number.isSafeInteger(state.currentGeneration) || state.currentGeneration < 1 || (state.active !== null && typeof state.active !== 'object')) throw new Error(`unsupported thin state at ${path}`);
  }
}

export const thinWorkspaceKey = workspaceKey;
export const thinGenerationName = generationName;
