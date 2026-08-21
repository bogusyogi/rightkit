import { classifyManagedCommand } from './command-capabilities.mjs';

export class QueueCancelledError extends Error {
  constructor(message = 'queued request cancelled') { super(message); this.name = 'QueueCancelledError'; this.code = 'RK_QUEUE_CANCELLED'; }
}

const LANES = new Set(['CONTROL', 'BUILD', 'WORKSPACE_WRITE', 'MAINTENANCE', 'TOOLCHAIN_WRITE']);
const PRIORITIES = new Map([['BATCH', 0], ['INTERACTIVE', 1], ['INSTALLER', 2]]);

/**
 * Independent bounded lanes. BUILD priority is queue-only:
 * an installer can move ahead of queued work, never interrupt an active build.
 * Waiting work gains priority each aging interval so a stream of installers
 * cannot starve an older request.
 */
export class CapabilityScheduler {
  #active = { CONTROL: [], BUILD: [], WORKSPACE_WRITE: [], MAINTENANCE: [], TOOLCHAIN_WRITE: [] }; #queue = { CONTROL: [], BUILD: [], WORKSPACE_WRITE: [], MAINTENANCE: [], TOOLCHAIN_WRITE: [] }; #limits; #heartbeatIntervalMs; #priorityAgingMs; #now;
  constructor({ controlConcurrency = 4, buildConcurrency = 1, workspaceWriteConcurrency = 4, maintenanceConcurrency = 1, heartbeatIntervalMs = 5_000, priorityAgingMs = 30_000, now = () => Date.now() } = {}) {
    if (!Number.isSafeInteger(controlConcurrency) || controlConcurrency < 1) throw new TypeError('controlConcurrency must be a positive integer');
    if (!Number.isSafeInteger(buildConcurrency) || buildConcurrency < 1) throw new TypeError('buildConcurrency must be a positive integer');
    if (!Number.isSafeInteger(workspaceWriteConcurrency) || workspaceWriteConcurrency < 1) throw new TypeError('workspaceWriteConcurrency must be a positive integer');
    if (!Number.isSafeInteger(maintenanceConcurrency) || maintenanceConcurrency < 1) throw new TypeError('maintenanceConcurrency must be a positive integer');
    if (!Number.isSafeInteger(heartbeatIntervalMs) || heartbeatIntervalMs < 10) throw new TypeError('heartbeatIntervalMs must be at least 10');
    if (!Number.isSafeInteger(priorityAgingMs) || priorityAgingMs < 1) throw new TypeError('priorityAgingMs must be a positive integer');
    if (typeof now !== 'function') throw new TypeError('now must be a function');
    this.#limits = Object.freeze({ CONTROL: controlConcurrency, BUILD: buildConcurrency, WORKSPACE_WRITE: workspaceWriteConcurrency, MAINTENANCE: maintenanceConcurrency, TOOLCHAIN_WRITE: 1 });
    this.#heartbeatIntervalMs = heartbeatIntervalMs; this.#priorityAgingMs = priorityAgingMs; this.#now = now;
  }
  get snapshot() {
    const lane = (name) => Object.freeze({ active: Object.freeze(this.#active[name].map(({ id }) => id)), queued: Object.freeze(this.#queue[name].map(({ id }) => id)) });
    return Object.freeze({ CONTROL: lane('CONTROL'), BUILD: lane('BUILD'), WORKSPACE_WRITE: lane('WORKSPACE_WRITE'), MAINTENANCE: lane('MAINTENANCE'), TOOLCHAIN_WRITE: lane('TOOLCHAIN_WRITE') });
  }
  acquire({ id, lane, priority = 'BATCH', repositoryIdentity, onHeartbeat = () => {} }) {
    if (typeof id !== 'string' || !id || typeof repositoryIdentity !== 'string' || !repositoryIdentity || typeof onHeartbeat !== 'function') throw new TypeError('id, repositoryIdentity, & heartbeat callback are required');
    if (!LANES.has(lane)) throw new TypeError('lane must be CONTROL, BUILD, WORKSPACE_WRITE, MAINTENANCE, or TOOLCHAIN_WRITE');
    if (!PRIORITIES.has(priority)) throw new TypeError('priority must be BATCH, INTERACTIVE, or INSTALLER');
    if ([...LANES].some((name) => this.#active[name].some((item) => item.id === id) || this.#queue[name].some((item) => item.id === id))) throw new TypeError('scheduler request id must be unique');
    let resolvePermit; let rejectPermit; let settled = false;
    const permit = new Promise((resolve, reject) => { resolvePermit = resolve; rejectPermit = reject; });
    const ticket = { id, lane, priority, repositoryIdentity, onHeartbeat, enqueuedAt: this.#now(), sequence: ++this.#sequence, resolvePermit, rejectPermit, timer: null, get settled() { return settled; }, set settled(value) { settled = value; } };
    const timer = setInterval(() => this.#heartbeat(ticket), this.#heartbeatIntervalMs); timer.unref?.(); ticket.timer = timer;
    this.#queue[lane].push(ticket); this.#heartbeat(ticket); this.#drain(lane);
    const cancel = () => {
      if (settled || this.#active[lane].includes(ticket)) return false;
      const index = this.#queue[lane].indexOf(ticket); if (index < 0) return false;
      this.#queue[lane].splice(index, 1); settled = true; clearInterval(timer); rejectPermit(new QueueCancelledError()); this.#reindex(lane); queueMicrotask(() => this.#drainAll()); return true;
    };
    return Object.freeze({ id, lane, permit, cancel });
  }
  #sequence = 0;
  #drain(lane) {
    while (this.#active[lane].length < this.#limits[lane] && this.#queue[lane].length) {
      const ticket = this.#next(lane); if (!ticket) return; this.#active[lane].push(ticket); ticket.settled = true; clearInterval(ticket.timer);
      ticket.onHeartbeat(Object.freeze({ state: 'ACTIVE', lane, position: 0, at: new Date(this.#now()).toISOString() }));
      let released = false;
      const release = () => {
        if (released) return false;
        const index = this.#active[lane].indexOf(ticket); if (index < 0) return false;
        released = true; this.#active[lane].splice(index, 1); this.#reindex(lane); queueMicrotask(() => this.#drainRelated(lane)); return true;
      };
      ticket.resolvePermit(Object.freeze({ release }));
    }
  }
  #next(lane) {
    const queue = this.#queue[lane];
    let selected = -1;
    for (let index = 0; index < queue.length; index += 1) {
      if (!this.#canRun(queue[index])) continue;
      if (selected < 0 || this.#compare(queue[index], queue[selected]) < 0) selected = index;
    }
    if (selected < 0) return null;
    return queue.splice(selected, 1)[0];
  }
  #canRun(ticket) {
    if (ticket.lane === 'TOOLCHAIN_WRITE') return [...LANES].every((lane) => this.#active[lane].length === 0);
    if (this.#active.TOOLCHAIN_WRITE.length || this.#queue.TOOLCHAIN_WRITE.length) return false;
    if (ticket.lane === 'BUILD') return !this.#active.BUILD.some(({ repositoryIdentity }) => repositoryIdentity === ticket.repositoryIdentity)
      && !this.#active.WORKSPACE_WRITE.some(({ repositoryIdentity }) => repositoryIdentity === ticket.repositoryIdentity);
    if (ticket.lane === 'WORKSPACE_WRITE') return !this.#active.WORKSPACE_WRITE.some(({ repositoryIdentity }) => repositoryIdentity === ticket.repositoryIdentity) && !this.#active.BUILD.some(({ repositoryIdentity }) => repositoryIdentity === ticket.repositoryIdentity);
    return true;
  }
  #drainRelated(lane) {
    this.#drainAll();
  }
  #drainAll() { for (const name of ['TOOLCHAIN_WRITE', 'CONTROL', 'BUILD', 'WORKSPACE_WRITE', 'MAINTENANCE']) this.#drain(name); }
  #compare(left, right) {
    const score = (ticket) => PRIORITIES.get(ticket.priority) + Math.floor(Math.max(0, this.#now() - ticket.enqueuedAt) / this.#priorityAgingMs);
    const difference = score(right) - score(left);
    return difference || left.sequence - right.sequence;
  }
  #heartbeat(ticket) {
    if (ticket.settled) return;
    const index = this.#queue[ticket.lane].indexOf(ticket); if (index < 0) return;
    ticket.onHeartbeat(Object.freeze({ state: 'QUEUED', lane: ticket.lane, position: index + 1, at: new Date(this.#now()).toISOString() }));
  }
  #reindex(lane) { for (const ticket of this.#queue[lane]) this.#heartbeat(ticket); }
}

/** One machine-wide FIFO lane for build-capable Rust work. */
export class MachineFifoScheduler {
  #active = null; #queue = []; #heartbeatIntervalMs;
  constructor({ heartbeatIntervalMs = 5_000 } = {}) {
    if (!Number.isSafeInteger(heartbeatIntervalMs) || heartbeatIntervalMs < 10) throw new TypeError('heartbeatIntervalMs must be at least 10');
    this.#heartbeatIntervalMs = heartbeatIntervalMs;
  }
  get snapshot() { return Object.freeze({ active: this.#active?.id ?? null, queued: Object.freeze(this.#queue.map(({ id }) => id)) }); }
  acquire({ id, repositoryIdentity, onHeartbeat = () => {} }) {
    if (typeof id !== 'string' || !id || typeof repositoryIdentity !== 'string' || !repositoryIdentity || typeof onHeartbeat !== 'function') throw new TypeError('id, repositoryIdentity, & heartbeat callback are required');
    if (this.#active?.id === id || this.#queue.some((item) => item.id === id)) throw new TypeError('scheduler request id must be unique');
    let resolvePermit; let rejectPermit; let settled = false;
    const permit = new Promise((resolve, reject) => { resolvePermit = resolve; rejectPermit = reject; });
    const ticket = { id, repositoryIdentity, onHeartbeat, resolvePermit, rejectPermit, timer: null, get settled() { return settled; }, set settled(value) { settled = value; } };
    this.#queue.push(ticket); const timer = setInterval(() => this.#heartbeat(ticket), this.#heartbeatIntervalMs); timer.unref?.(); ticket.timer = timer; this.#heartbeat(ticket); this.#drain();
    const cancel = () => {
      if (settled || this.#active === ticket) return false;
      const index = this.#queue.indexOf(ticket); if (index < 0) return false;
      this.#queue.splice(index, 1); settled = true; clearInterval(timer); rejectPermit(new QueueCancelledError()); this.#reindex(); return true;
    };
    return Object.freeze({ id, permit, cancel });
  }
  #drain() {
    if (this.#active || !this.#queue.length) return;
    const ticket = this.#queue.shift(); this.#active = ticket; ticket.settled = true; clearInterval(ticket.timer);
    ticket.onHeartbeat(Object.freeze({ state: 'ACTIVE', position: 0, at: new Date().toISOString() }));
    const release = () => { if (this.#active !== ticket) return false; this.#active = null; this.#reindex(); queueMicrotask(() => this.#drain()); return true; };
    ticket.resolvePermit(Object.freeze({ release }));
  }
  #heartbeat(ticket) {
    if (ticket.settled) return;
    const index = this.#queue.indexOf(ticket); if (index < 0) return;
    ticket.onHeartbeat(Object.freeze({ state: 'QUEUED', position: index + 1, at: new Date().toISOString() }));
  }
  #reindex() { for (const ticket of this.#queue) this.#heartbeat(ticket); }
}

export function isBuildCapableRust(shim, argv = []) {
  try { return classifyManagedCommand(shim, argv).lane === 'BUILD'; } catch { return false; }
}
