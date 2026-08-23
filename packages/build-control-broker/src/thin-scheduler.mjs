const LANES = new Set(['CONTROL', 'ANALYSIS', 'WORKSPACE_WRITE', 'BUILD', 'TOOLCHAIN_WRITE']);
// Canonical order: human-interactive work outranks installer/release work,
// which outranks normal/background work. Priority never preempts active work.
const PRIORITY = new Map([['BATCH', 0], ['INSTALLER', 1], ['INTERACTIVE', 2]]);

export class ThinQueueCancelledError extends Error {
  constructor() { super('queued request cancelled'); this.name = 'ThinQueueCancelledError'; this.code = 'RK_QUEUE_CANCELLED'; }
}

export class ThinQueueUnavailableError extends Error {
  constructor() { super('requested lane/workspace capacity is currently occupied'); this.name = 'ThinQueueUnavailableError'; this.code = 'RK_QUEUE_UNAVAILABLE'; }
}

export class ThinScheduler {
  #limits; #queue = []; #active = new Map(); #workspace = new Map(); #toolchainActive = false; #sequence = 0; #now;
  constructor({ controlConcurrency = 4, analysisConcurrency = 2, workspaceWriteConcurrency = 4, buildConcurrency = 3, now = Date.now } = {}) {
    for (const [name, value] of Object.entries({ controlConcurrency, analysisConcurrency, workspaceWriteConcurrency, buildConcurrency })) if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${name} must be a positive integer`);
    this.#limits = Object.freeze({ CONTROL: controlConcurrency, ANALYSIS: analysisConcurrency, WORKSPACE_WRITE: workspaceWriteConcurrency, BUILD: buildConcurrency, TOOLCHAIN_WRITE: 1 });
    this.#now = now;
  }

  get snapshot() {
    return Object.freeze({
      active: Object.freeze([...this.#active.values()].map((item) => Object.freeze({ id: item.id, lane: item.lane, workspaceKey: item.workspaceKey }))),
      queued: Object.freeze(this.#queue.map((item) => Object.freeze({ id: item.id, lane: item.lane, workspaceKey: item.workspaceKey })))
    });
  }

  acquire({ id, capability, workspaceKey = null, priority = 'BATCH', onHeartbeat = () => {}, tryRun = false } = {}) {
    if (typeof id !== 'string' || !id || !capability || !LANES.has(capability.lane) || !PRIORITY.has(priority)) throw new TypeError('id, capability and priority are required');
    if (this.#active.has(id) || this.#queue.some((item) => item.id === id)) throw new TypeError('scheduler id must be unique');
    let resolvePermit; let rejectPermit;
    const permit = new Promise((resolve, reject) => { resolvePermit = resolve; rejectPermit = reject; });
    const ticket = { id, lane: capability.lane, access: capability.workspaceAccess, compile: capability.compile === true, workspaceKey, priority, sequence: ++this.#sequence, queuedAt: this.#now(), onHeartbeat, resolvePermit, rejectPermit, state: 'QUEUED' };
    // tryRun: never queue. Background maintenance (idle-generation eviction)
    // must not block waiting for a workspace whose write slot an admitted job
    // holds; it gives up immediately instead (v2.8 §10.2). The synchronous
    // check-then-start below is atomic in single-threaded JS, so a successful
    // tryRun owns the workspace writer slot before any queued ticket can.
    if (tryRun) {
      if (!this.#canRun(ticket)) { rejectPermit(new ThinQueueUnavailableError()); return Object.freeze({ id, permit, cancel: () => false }); }
      this.#start(ticket);
      return Object.freeze({ id, permit, cancel: () => false });
    }
    this.#queue.push(ticket);
    this.#emitHeartbeat(ticket);
    queueMicrotask(() => this.#drain());
    return Object.freeze({
      id,
      permit,
      cancel: () => {
        if (ticket.state !== 'QUEUED') return false;
        const index = this.#queue.indexOf(ticket); if (index < 0) return false;
        this.#queue.splice(index, 1); ticket.state = 'CANCELLED'; rejectPermit(new ThinQueueCancelledError()); this.#reindex(); queueMicrotask(() => this.#drain()); return true;
      }
    });
  }

  #laneCount(lane) { let count = 0; for (const item of this.#active.values()) if (item.lane === lane) count += 1; return count; }
  #workspaceState(key) { if (!key) return null; if (!this.#workspace.has(key)) this.#workspace.set(key, { readers: 0, writer: false }); return this.#workspace.get(key); }

  #canRun(ticket) {
    if (ticket.lane === 'TOOLCHAIN_WRITE') return this.#active.size === 0;
    if (this.#toolchainActive || this.#queue.some((item) => item.lane === 'TOOLCHAIN_WRITE')) return false;
    if (this.#laneCount(ticket.lane) >= this.#limits[ticket.lane]) return false;
    const state = this.#workspaceState(ticket.workspaceKey);
    if (ticket.access === 'read' && state?.writer) return false;
    if (ticket.access === 'write' && state && (state.writer || state.readers > 0)) return false;
    return true;
  }

  #compare(left, right) {
    const age = (ticket) => Math.floor(Math.max(0, this.#now() - ticket.queuedAt) / 30_000);
    const delta = (PRIORITY.get(right.priority) + age(right)) - (PRIORITY.get(left.priority) + age(left));
    return delta || left.sequence - right.sequence;
  }

  #drain() {
    let progressed = true;
    while (progressed) {
      progressed = false;
      const candidates = [...this.#queue].sort((a, b) => this.#compare(a, b));
      for (const ticket of candidates) {
        if (!this.#canRun(ticket)) continue;
        this.#start(ticket); progressed = true; break;
      }
    }
    this.#reindex();
  }

  #start(ticket) {
    const index = this.#queue.indexOf(ticket); if (index >= 0) this.#queue.splice(index, 1);
    ticket.state = 'ACTIVE'; this.#active.set(ticket.id, ticket);
    const state = this.#workspaceState(ticket.workspaceKey);
    if (ticket.access === 'read' && state) state.readers += 1;
    if (ticket.access === 'write' && state) state.writer = true;
    if (ticket.lane === 'TOOLCHAIN_WRITE') this.#toolchainActive = true;
    ticket.onHeartbeat(Object.freeze({ state: 'ACTIVE', lane: ticket.lane, position: 0, at: new Date(this.#now()).toISOString() }));
    let released = false;
    ticket.resolvePermit(Object.freeze({ release: () => {
      if (released) return false; released = true;
      this.#active.delete(ticket.id); ticket.state = 'RELEASED';
      const current = this.#workspaceState(ticket.workspaceKey);
      if (ticket.access === 'read' && current) current.readers = Math.max(0, current.readers - 1);
      if (ticket.access === 'write' && current) current.writer = false;
      if (ticket.lane === 'TOOLCHAIN_WRITE') this.#toolchainActive = false;
      queueMicrotask(() => this.#drain()); return true;
    } }));
  }

  #emitHeartbeat(ticket) {
    const index = this.#queue.indexOf(ticket);
    ticket.onHeartbeat(Object.freeze({ state: 'QUEUED', lane: ticket.lane, position: index < 0 ? 0 : index + 1, at: new Date(this.#now()).toISOString() }));
  }
  #reindex() { for (const ticket of this.#queue) this.#emitHeartbeat(ticket); }
}
