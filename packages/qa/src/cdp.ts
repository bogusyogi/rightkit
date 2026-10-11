/** Minimal flattened-session CDP transport using Node's built-in WebSocket. */
export class TimeoutError extends Error {
  /** Set when the wait was given failureArtifacts and the artifacts were written. */
  artifacts?: { json: string; screenshot?: string };
  constructor(message: string) { super(message); this.name = 'TimeoutError'; }
}

export interface QaConnectOptions {
  /** Default 30 seconds; applies to connection & each CDP command. */
  timeout?: number;
  /** Off by default. Receives method names only, never evaluated source or input text. */
  debug?: (message: string) => void;
}

export type CdpParams = Record<string, any>;
/** Receives one event's params, from the session it subscribed to. */
export type CdpListener = (params: CdpParams) => void;
/** Receives one event from any session. `sessionId` is the session it arrived on; undefined for the root. */
export type CdpAnyListener = (params: CdpParams, sessionId: string | undefined) => void;

/** @internal All operations use finite deadlines. */
export function validateTimeout(timeout: number): number {
  if (!Number.isFinite(timeout) || timeout <= 0 || timeout > 2_147_483_647) throw new Error('Timeout must be a positive finite number below 2147483648ms');
  return timeout;
}

export class CdpClient {
  private nextId = 0;
  private pending = new Map<number, { resolve: (value: CdpParams) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout>; session?: string }>();
  private listeners = new Map<string, Set<CdpListener>>();
  private anyListeners = new Map<string, Set<CdpAnyListener>>();
  private closed?: Error;
  private constructor(private socket: WebSocket, readonly timeout: number, private debug?: QaConnectOptions['debug']) {
    socket.addEventListener('message', (event) => {
      let message: CdpParams;
      try {
        message = JSON.parse(String(event.data));
      } catch (error) { this.fail(new Error(`Invalid CDP message: ${String(error)}`)); this.socket.close(); return; }
      if (message.id !== undefined) {
        const pending = this.pending.get(message.id);
        if (!pending) return;
        this.pending.delete(message.id); clearTimeout(pending.timer);
        if (message.error) pending.reject(new Error(`CDP: ${message.error.message}`));
        else pending.resolve(message.result ?? {});
      } else if (message.method) {
        this.emit(message.method, message.params ?? {}, message.sessionId);
      }
    });
    socket.addEventListener('close', () => this.fail(new Error('CDP socket closed')));
    socket.addEventListener('error', () => this.fail(new Error('CDP socket error')));
  }
  static async connect(endpoint: string, options: QaConnectOptions = {}): Promise<CdpClient> {
    const timeout = validateTimeout(options.timeout ?? 30_000);
    const socket = new WebSocket(endpoint);
    const client = new CdpClient(socket, timeout, options.debug);
    await new Promise<void>((resolve, reject) => {
      const done = (error?: Error) => {
        clearTimeout(timer);
        socket.removeEventListener('open', open); socket.removeEventListener('error', errorHandler); socket.removeEventListener('close', close);
        if (error) { client.close(); reject(error); } else resolve();
      };
      const open = () => done();
      const errorHandler = () => done(new Error('CDP connection failed'));
      const close = () => done(new Error('CDP socket closed while connecting'));
      const timer = setTimeout(() => done(new TimeoutError(`CDP connect timed out after ${timeout}ms`)), timeout);
      socket.addEventListener('open', open); socket.addEventListener('error', errorHandler); socket.addEventListener('close', close);
    });
    return client;
  }
  send(method: string, params: CdpParams = {}, session?: string, timeout = this.timeout): Promise<CdpParams> {
    if (this.closed) return Promise.reject(this.closed);
    validateTimeout(timeout);
    this.debug?.(`CDP ${method}`);
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id); reject(new TimeoutError(`${method} timed out after ${timeout}ms`));
      }, timeout);
      this.pending.set(id, { resolve, reject, timer, session });
      try { this.socket.send(JSON.stringify({ id, method, params, ...(session ? { sessionId: session } : {}) })); }
      catch (error) { clearTimeout(timer); this.pending.delete(id); reject(error); }
    });
  }
  /** Subscribes to one event method on one session (the root when `session` is omitted). Returns the unsubscribe function. */
  on(method: string, listener: CdpListener, session?: string): () => void {
    const key = `${session ?? ''}:${method}`;
    const listeners = this.listeners.get(key) ?? new Set<CdpListener>();
    this.listeners.set(key, listeners);
    listeners.add(listener);
    return () => { listeners.delete(listener); if (!listeners.size && this.listeners.get(key) === listeners) this.listeners.delete(key); };
  }
  /**
   * Subscribes to one event method on every session: the root and each flattened child session.
   * The listener gets the params and the sessionId the event arrived on (undefined for the root).
   * Returns the unsubscribe function.
   */
  onAny(method: string, listener: CdpAnyListener): () => void {
    const listeners = this.anyListeners.get(method) ?? new Set<CdpAnyListener>();
    this.anyListeners.set(method, listeners);
    listeners.add(listener);
    return () => { listeners.delete(listener); if (!listeners.size && this.anyListeners.get(method) === listeners) this.anyListeners.delete(method); };
  }
  private emit(method: string, params: CdpParams, session?: string) {
    for (const listener of [...(this.listeners.get(`${session ?? ''}:${method}`) ?? [])]) {
      // A user subscription cannot corrupt transport state or suppress internal listeners.
      try { listener(params); } catch { this.debug?.(`CDP listener failed: ${method}`); }
    }
    for (const listener of [...(this.anyListeners.get(method) ?? [])]) {
      try { listener(params, session); } catch { this.debug?.(`CDP listener failed: ${method}`); }
    }
  }
  /** Rejects pending commands sent on `session`; undefined selects the commands of the root session. */
  rejectSession(session: string | undefined, error: Error) {
    for (const [id, pending] of this.pending) if (pending.session === session) {
      clearTimeout(pending.timer); this.pending.delete(id); pending.reject(error);
    }
  }
  private fail(error: Error) {
    if (this.closed) return;
    this.closed = error;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); }
    this.pending.clear(); this.emit('Disconnected', { error }); this.listeners.clear(); this.anyListeners.clear();
  }
  /** Synchronous and idempotent: rejects pending commands and closes the socket. There is nothing to await. */
  close() { this.fail(new Error('CDP socket closed')); this.socket.close(); }
}

/**
 * Connects a CDP client to a WebSocket endpoint: a browser socket, or a page-level `webSocketDebuggerUrl`
 * whose commands run on the root session. It uses the same transport as QaBrowserSession. Close it with
 * `close()`, which needs no await.
 */
export function connectCdp(endpoint: string, options: QaConnectOptions = {}): Promise<CdpClient> {
  return CdpClient.connect(endpoint, options);
}
