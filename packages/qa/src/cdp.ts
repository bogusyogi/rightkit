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
type Listener = (params: CdpParams) => void;

/** @internal All operations use finite deadlines. */
export function validateTimeout(timeout: number): number {
  if (!Number.isFinite(timeout) || timeout <= 0 || timeout > 2_147_483_647) throw new Error('Timeout must be a positive finite number below 2147483648ms');
  return timeout;
}

export class CdpClient {
  private nextId = 0;
  private pending = new Map<number, { resolve: (value: CdpParams) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout>; session?: string }>();
  private listeners = new Map<string, Set<Listener>>();
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
  on(method: string, listener: Listener, session?: string): () => void {
    const key = `${session ?? ''}:${method}`;
    let listeners = this.listeners.get(key);
    if (!listeners) this.listeners.set(key, listeners = new Set());
    listeners.add(listener);
    return () => { listeners.delete(listener); if (!listeners.size) this.listeners.delete(key); };
  }
  private emit(method: string, params: CdpParams, session?: string) {
    for (const listener of [...(this.listeners.get(`${session ?? ''}:${method}`) ?? [])]) {
      // A user subscription cannot corrupt transport state or suppress internal listeners.
      try { listener(params); } catch { this.debug?.(`CDP listener failed: ${method}`); }
    }
  }
  rejectSession(session: string, error: Error) {
    for (const [id, pending] of this.pending) if (pending.session === session) {
      clearTimeout(pending.timer); this.pending.delete(id); pending.reject(error);
    }
  }
  private fail(error: Error) {
    if (this.closed) return;
    this.closed = error;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); }
    this.pending.clear(); this.emit('Disconnected', { error }); this.listeners.clear();
  }
  close() { this.fail(new Error('CDP socket closed')); this.socket.close(); }
}
