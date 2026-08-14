import { createHash } from 'node:crypto';
import { lstat as defaultLstat, readFile as defaultReadFile } from 'node:fs/promises';
import { immutableToolRoot } from '@rightkit/build-control-core';

const digest = /^[a-f0-9]{64}$/;

function identity(path, metadata) {
  if (!metadata?.isFile?.() || metadata.isSymbolicLink?.() || !Number.isSafeInteger(Number(metadata.size)) || Number(metadata.size) < 0) throw new TypeError(`immutable tool is not a regular file: ${path}`);
  const mtimeNs = metadata.mtimeNs ?? BigInt(Math.trunc(Number(metadata.mtimeMs) * 1_000_000)); const ctimeNs = metadata.ctimeNs ?? BigInt(Math.trunc(Number(metadata.ctimeMs) * 1_000_000));
  return Object.freeze({ path, size: BigInt(metadata.size), mtimeNs: BigInt(mtimeNs), ctimeNs: BigInt(ctimeNs) });
}

const sameIdentity = (left, right) => left.path === right.path && left.size === right.size && left.mtimeNs === right.mtimeNs && left.ctimeNs === right.ctimeNs;

async function sha256(path, readFile) { return createHash('sha256').update(await readFile(path)).digest('hex'); }

/** Typed rejection for a changed, missing, or digest-mismatched immutable executable. */
export class ToolChangedError extends Error {
  constructor({ toolId, path, expectedSha256, observedSha256 = null, reason }) {
    super(`immutable tool changed: ${toolId} (${reason})`); this.name = 'ToolChangedError'; this.code = 'TOOL_CHANGED';
    this.event = Object.freeze({ type: 'TOOL_CHANGED', toolId, path, expectedSha256, observedSha256, reason, registrationAction: 're-register immutable tool digest, stage RightKit runtime, & retry' });
  }
}

/**
 * Verifies each registered executable once, then hashes again only after its
 * stable file identity changes. Call verify() before every process spawn.
 */
export class ImmutableToolRegistry {
  #records = new Map(); #lstat; #readFile;
  constructor({ lstat = defaultLstat, readFile = defaultReadFile } = {}) {
    if (typeof lstat !== 'function' || typeof readFile !== 'function') throw new TypeError('lstat & readFile functions are required');
    this.#lstat = lstat; this.#readFile = readFile;
  }
  get size() { return this.#records.size; }
  async register(tool) {
    const normalized = immutableToolRoot(tool); const toolId = typeof tool?.id === 'string' && tool.id ? tool.id : normalized.path;
    if (this.#records.has(toolId)) throw new TypeError(`tool is already registered: ${toolId}`);
    const record = Object.freeze({ id: toolId, ...normalized }); const verifiedIdentity = await this.#verify(record); this.#records.set(toolId, { tool: record, identity: verifiedIdentity }); return record;
  }
  async registerAll(tools) {
    if (!Array.isArray(tools)) throw new TypeError('tools must be an array');
    const registered = []; for (const tool of tools) registered.push(await this.register(tool)); return Object.freeze(registered);
  }
  async verify(toolId) {
    const record = this.#records.get(toolId); if (!record) throw new TypeError(`registered tool is required: ${toolId}`);
    const current = await this.#identity(record.tool);
    if (!sameIdentity(record.identity, current)) record.identity = await this.#verify(record.tool, current);
    return record.tool;
  }
  async #identity(tool) {
    try { return identity(tool.path, await this.#lstat(tool.path, { bigint: true })); }
    catch (error) { if (error instanceof ToolChangedError) throw error; throw new ToolChangedError({ toolId: tool.id, path: tool.path, expectedSha256: tool.sha256, reason: error.code ?? 'unavailable' }); }
  }
  async #verify(tool, observedIdentity = null) {
    const before = observedIdentity ?? await this.#identity(tool);
    let observedSha256;
    try { observedSha256 = await sha256(tool.path, this.#readFile); }
    catch (error) { throw new ToolChangedError({ toolId: tool.id, path: tool.path, expectedSha256: tool.sha256, reason: error.code ?? 'unreadable' }); }
    const after = await this.#identity(tool);
    if (!sameIdentity(before, after)) throw new ToolChangedError({ toolId: tool.id, path: tool.path, expectedSha256: tool.sha256, observedSha256, reason: 'identity-changed-during-verification' });
    if (!digest.test(observedSha256) || observedSha256 !== tool.sha256) throw new ToolChangedError({ toolId: tool.id, path: tool.path, expectedSha256: tool.sha256, observedSha256, reason: observedIdentity ? 'identity-digest-mismatch' : 'startup-digest-mismatch' });
    return after;
  }
}
