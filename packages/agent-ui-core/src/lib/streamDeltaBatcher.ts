export type StreamDeltaType = "assistant" | "reasoning";

export interface QueuedStreamDelta {
  occurredAt: number;
  text: string;
  type: StreamDeltaType;
}

export interface StreamDeltaBatcherOptions {
  /** Receives one coalesced batch per session. The batch is already removed from the queue. */
  onFlush: (sessionId: string, deltas: QueuedStreamDelta[]) => void;
  flushGapMs?: number;
  maxFlushGapMs?: number;
  now?: () => number;
}

const DEFAULT_FLUSH_GAP_MS = 33;
const DEFAULT_MAX_FLUSH_GAP_MS = 250;

type TimerHandle = number;

/**
 * Renderer-only stream delta queue.
 *
 * This is the extracted, framework-neutral part of Hermes Agent's
 * `useMessageStream`: adjacent deltas of the same kind are coalesced, a
 * bounded timer (not rAF) delivers them, and the next delay adapts to the
 * measured render cost. The daemon remains the owner of event order; this
 * class only controls when the renderer publishes its projection.
 */
export class StreamDeltaBatcher {
  private readonly queues = new Map<string, QueuedStreamDelta[]>();
  private readonly onFlush: StreamDeltaBatcherOptions["onFlush"];
  private readonly flushGapMs: number;
  private readonly maxFlushGapMs: number;
  private readonly now: () => number;
  private flushHandle: TimerHandle | null = null;
  private measureFrameHandle: number | null = null;
  private lastFlushAt = 0;
  private lastFlushCost = 0;
  private disposed = false;

  constructor(options: StreamDeltaBatcherOptions) {
    this.onFlush = options.onFlush;
    this.flushGapMs = Math.max(0, options.flushGapMs ?? DEFAULT_FLUSH_GAP_MS);
    this.maxFlushGapMs = Math.max(this.flushGapMs, options.maxFlushGapMs ?? DEFAULT_MAX_FLUSH_GAP_MS);
    this.now = options.now ?? (() => (typeof performance !== "undefined" ? performance.now() : Date.now()));
  }

  enqueue(
    sessionId: string,
    type: StreamDeltaType,
    text: string,
    occurredAt = Date.now() / 1000,
  ): void {
    if (this.disposed || !text) return;

    const queued = this.queues.get(sessionId) ?? [];
    const tail = queued.at(-1);
    if (tail?.type === type) {
      tail.text += text;
    } else {
      queued.push({ occurredAt, text, type });
    }
    this.queues.set(sessionId, queued);
    this.schedule();
  }

  /** Donor-compatible name used by terminal/tool event call sites. */
  queueDelta(
    sessionId: string,
    type: StreamDeltaType,
    text: string,
    occurredAt = Date.now() / 1000,
  ): void {
    this.enqueue(sessionId, type, text, occurredAt);
  }

  /** Flush one session before a terminal/tool frame, or all sessions when omitted. */
  flush(sessionId?: string): void {
    if (this.disposed && this.queues.size === 0) return;

    const ids = sessionId === undefined ? [...this.queues.keys()] : [sessionId];
    for (const id of ids) {
      const queued = this.queues.get(id);
      if (!queued) continue;
      this.queues.delete(id);
      this.onFlush(id, queued);
    }

    if (this.queues.size === 0 && this.flushHandle !== null) {
      globalThis.clearTimeout(this.flushHandle);
      this.flushHandle = null;
    }
  }

  /** Cancel future scheduling and deliver the final queued batches exactly once. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    if (this.flushHandle !== null) {
      globalThis.clearTimeout(this.flushHandle);
      this.flushHandle = null;
    }
    this.cancelMeasurement();
    this.flush();
  }

  /** Donor-compatible name used by stream terminal reconciliation. */
  flushQueuedDeltas(sessionId?: string): void {
    this.flush(sessionId);
  }

  /** True when at least one session has deltas waiting for publication. */
  get pending(): boolean {
    return this.queues.size > 0;
  }

  /** Install the visibility/focus backstops used by the donor hook. */
  attachLifecycleListeners(
    target: Pick<Window, "addEventListener" | "removeEventListener" | "setTimeout" | "clearTimeout"> &
      { document?: Pick<Document, "addEventListener" | "removeEventListener" | "visibilityState"> } = window,
  ): () => void {
    const documentTarget = target.document ?? document;
    const flushPending = () => {
      if (this.flushHandle !== null) {
        target.clearTimeout(this.flushHandle);
        this.flushHandle = null;
      }
      this.flush();
    };
    const flushWhenVisible = () => {
      if (documentTarget.visibilityState === "visible") flushPending();
    };

    documentTarget.addEventListener("visibilitychange", flushWhenVisible);
    target.addEventListener("focus", flushPending);
    return () => {
      documentTarget.removeEventListener("visibilitychange", flushWhenVisible);
      target.removeEventListener("focus", flushPending);
    };
  }

  private schedule(): void {
    if (this.disposed || this.flushHandle !== null) return;
    if (typeof window === "undefined") {
      this.runFlush();
      return;
    }

    const sinceLast = this.now() - this.lastFlushAt;
    const adaptiveFloor = Math.min(
      Math.max(this.flushGapMs, this.lastFlushCost * 3),
      this.maxFlushGapMs,
    );
    this.flushHandle = window.setTimeout(
      () => this.runFlush(),
      Math.max(0, adaptiveFloor - sinceLast),
    );
  }

  private runFlush(): void {
    this.flushHandle = null;
    const startedAt = this.now();
    this.lastFlushAt = startedAt;
    this.flush();
    const writeCost = Math.max(0, this.now() - startedAt);
    this.lastFlushCost = writeCost;

    if (typeof window === "undefined") return;
    this.cancelMeasurement();
    const flushStartedAt = startedAt;
    this.measureFrameHandle = window.requestAnimationFrame((frameStart) => {
      this.measureFrameHandle = null;
      if (this.lastFlushAt !== flushStartedAt) return;
      this.lastFlushCost = writeCost + Math.max(0, this.now() - frameStart);
    });
  }

  private cancelMeasurement(): void {
    if (this.measureFrameHandle !== null && typeof window !== "undefined") {
      window.cancelAnimationFrame(this.measureFrameHandle);
    }
    this.measureFrameHandle = null;
  }
}

/** Hook-friendly construction helper for callers that keep the instance in a ref. */
export function createStreamDeltaBatcher(
  options: StreamDeltaBatcherOptions,
): StreamDeltaBatcher {
  return new StreamDeltaBatcher(options);
}

export { DEFAULT_FLUSH_GAP_MS as STREAM_DELTA_FLUSH_MS, DEFAULT_MAX_FLUSH_GAP_MS as MAX_STREAM_FLUSH_GAP_MS };
