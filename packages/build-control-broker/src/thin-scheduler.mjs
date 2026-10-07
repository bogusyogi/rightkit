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

/** Aggregate compiler-budget root costs (dual-host routing §5). A build-class
 * root costs `buildCost` units (host sizing: its Cargo jobs times measured
 * per-job memory, in GiB of a GiB-denominated budget); a test root about one
 * half of that and an analysis root about one third. Without `buildCost` a
 * build costs the whole budget (legacy: one heavy build at a time). */
export function deriveCompileCosts(compileBudgetUnits, buildCost = compileBudgetUnits) {
  if (!Number.isSafeInteger(compileBudgetUnits) || compileBudgetUnits < 1) throw new TypeError('compileBudgetUnits must be a positive integer');
  if (!Number.isSafeInteger(buildCost) || buildCost < 1) throw new TypeError('buildCost must be a positive integer');
  const build = Math.min(compileBudgetUnits, buildCost);
  return Object.freeze({
    ANALYSIS: Math.max(1, Math.round(build / 3)),
    TEST: Math.max(1, Math.round(build / 2)),
    BUILD: build,
  });
}

export class ThinScheduler {
  #limits; #compileBudget; #costUnitBytes; #hostAvailableMemory; #queue = []; #active = new Map(); #workspace = new Map(); #toolchainActive = false; #sequence = 0; #now;
  constructor({ controlConcurrency = 4, analysisConcurrency = 2, workspaceWriteConcurrency = 4, buildConcurrency = 3, compileBudget = null, costUnitBytes = null, hostAvailableMemory = null, now = Date.now } = {}) {
    for (const [name, value] of Object.entries({ controlConcurrency, analysisConcurrency, workspaceWriteConcurrency, buildConcurrency })) if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${name} must be a positive integer`);
    if (compileBudget !== null && (!Number.isSafeInteger(compileBudget) || compileBudget < 1)) throw new TypeError('compileBudget must be a positive integer or null');
    this.#limits = Object.freeze({ CONTROL: controlConcurrency, ANALYSIS: analysisConcurrency, WORKSPACE_WRITE: workspaceWriteConcurrency, BUILD: buildConcurrency, TOOLCHAIN_WRITE: 1 });
    this.#compileBudget = compileBudget;
    // Live guard for memory-denominated budgets: a root joining other compile
    // roots also needs its projected memory actually available on the host.
    this.#costUnitBytes = Number.isSafeInteger(costUnitBytes) && costUnitBytes > 0 ? costUnitBytes : null;
    this.#hostAvailableMemory = typeof hostAvailableMemory === 'function' ? hostAvailableMemory : null;
    this.#now = now;
  }

  get snapshot() {
    return Object.freeze({
      compileBudget: this.#compileBudget,
      active: Object.freeze([...this.#active.values()].map((item) => Object.freeze({ id: item.id, lane: item.lane, workspaceKey: item.workspaceKey }))),
      queued: Object.freeze(this.#queue.map((item) => Object.freeze({ id: item.id, lane: item.lane, workspaceKey: item.workspaceKey })))
    });
  }

  acquire({ id, capability, workspaceKey = null, priority = 'BATCH', compileCost = null, onHeartbeat = () => {}, tryRun = false } = {}) {
    if (typeof id !== 'string' || !id || !capability || !LANES.has(capability.lane) || !PRIORITY.has(priority)) throw new TypeError('id, capability and priority are required');
    if (compileCost !== null && (!Number.isSafeInteger(compileCost) || compileCost < 1)) throw new TypeError('compileCost must be a positive integer or null');
    if (compileCost !== null && this.#compileBudget === null) throw new TypeError('compileCost requires a configured compileBudget');
    if (this.#active.has(id) || this.#queue.some((item) => item.id === id)) throw new TypeError('scheduler id must be unique');
    let resolvePermit; let rejectPermit;
    const permit = new Promise((resolve, reject) => { resolvePermit = resolve; rejectPermit = reject; });
    const ticket = { id, lane: capability.lane, access: capability.workspaceAccess, compile: capability.compile === true, cost: compileCost, workspaceKey, priority, sequence: ++this.#sequence, queuedAt: this.#now(), blockers: new Map(), onHeartbeat, resolvePermit, rejectPermit, state: 'QUEUED' };
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
    this.#noteBlockers(ticket);
    this.#emitHeartbeat(ticket);
    queueMicrotask(() => this.#drain());
    return Object.freeze({
      id,
      permit,
      /** Every active holder that blocked this ticket while it waited: [{ id, reason }] (wait attribution). */
      blockers: () => Object.freeze([...ticket.blockers].map(([blockerId, reason]) => Object.freeze({ id: blockerId, reason }))),
      cancel: () => {
        if (ticket.state !== 'QUEUED') return false;
        const index = this.#queue.indexOf(ticket); if (index < 0) return false;
        this.#queue.splice(index, 1); ticket.state = 'CANCELLED'; rejectPermit(new ThinQueueCancelledError()); this.#reindex(); queueMicrotask(() => this.#drain()); return true;
      }
    });
  }

  #memoryShort(ticket) {
    if (!this.#costUnitBytes || !this.#hostAvailableMemory) return false;
    let available; try { available = this.#hostAvailableMemory(); } catch { return false; }
    return Number.isFinite(available) && available < ticket.cost * this.#costUnitBytes;
  }

  #budgeted(ticket) { return ticket.cost !== null && this.#compileBudget !== null; }

  #laneCount(lane) { let count = 0; for (const item of this.#active.values()) if (item.lane === lane) count += 1; return count; }
  #workspaceState(key) { if (!key) return null; if (!this.#workspace.has(key)) this.#workspace.set(key, { readers: 0, writer: false }); return this.#workspace.get(key); }

  #canRun(ticket) {
    if (ticket.lane === 'TOOLCHAIN_WRITE') return this.#active.size === 0;
    if (this.#toolchainActive || this.#queue.some((item) => item.lane === 'TOOLCHAIN_WRITE')) return false;
    // A costed compile root is governed by the memory budget alone; the lane count cap only bounds
    // roots without a cost (no budget configured), so cheap test/check roots are not capped at the
    // number of full builds that fit.
    if (!this.#budgeted(ticket) && this.#laneCount(ticket.lane) >= this.#limits[ticket.lane]) return false;
    // Aggregate compiler budget (dual-host routing §5): the sum of admitted
    // root costs stays within one machine-wide budget. Lane caps remain the
    // secondary bound above; budget admission never replaces them.
    if (ticket.cost !== null) {
      let used = 0;
      for (const item of this.#active.values()) used += item.cost ?? 0;
      if (used + ticket.cost > this.#compileBudget) return false;
      if (used > 0 && this.#memoryShort(ticket)) return false;
    }
    const state = this.#workspaceState(ticket.workspaceKey);
    if (ticket.access === 'read' && state?.writer) return false;
    if (ticket.access === 'write' && state && (state.writer || state.readers > 0)) return false;
    return true;
  }

  /** Record which active tickets currently block `ticket` (identity only; no behaviour change). */
  #noteBlockers(ticket) {
    const note = (other, reason) => { if (other.id !== ticket.id && !ticket.blockers.has(other.id)) ticket.blockers.set(other.id, reason); };
    if (ticket.lane === 'TOOLCHAIN_WRITE') { for (const other of this.#active.values()) note(other, 'toolchain-exclusive'); return; }
    if (this.#toolchainActive) for (const other of this.#active.values()) if (other.lane === 'TOOLCHAIN_WRITE') note(other, 'toolchain-write-active');
    if (!this.#budgeted(ticket) && this.#laneCount(ticket.lane) >= this.#limits[ticket.lane]) for (const other of this.#active.values()) if (other.lane === ticket.lane) note(other, `lane-cap:${ticket.lane}`);
    const state = this.#workspaceState(ticket.workspaceKey);
    if (state && ((ticket.access === 'read' && state.writer) || (ticket.access === 'write' && (state.writer || state.readers > 0)))) {
      for (const other of this.#active.values()) if (other.workspaceKey === ticket.workspaceKey && (other.access === 'write' || ticket.access === 'write')) note(other, 'workspace-exclusion');
    }
    if (ticket.cost !== null) {
      let used = 0; for (const other of this.#active.values()) used += other.cost ?? 0;
      const reason = used + ticket.cost > this.#compileBudget ? 'compile-budget' : used > 0 && this.#memoryShort(ticket) ? 'host-memory' : null;
      if (reason) for (const other of this.#active.values()) if (other.cost) note(other, reason);
    }
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
  #reindex() { for (const ticket of this.#queue) { this.#noteBlockers(ticket); this.#emitHeartbeat(ticket); } }
}
