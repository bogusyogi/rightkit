import type { ProtocolEvent, TranscriptMessage } from "../types";
import { isInternalTranscriptMessage } from "../transcriptMessages";

/**
 * Append a transcript row without allowing replay to duplicate it.
 *
 * This is the direct port of Hermes Agent's id-based `appendMessage`: event
 * sequence numbers are transport cursors, not message identity. Internal
 * daemon rows remain filtered at the renderer boundary.
 */
export function appendTranscriptMessage(
  rows: TranscriptMessage[],
  message: TranscriptMessage,
): TranscriptMessage[] {
  if (isInternalTranscriptMessage(message)) return rows;
  return rows.some((row) => row.id === message.id)
    ? rows
    : [...rows.filter((row) => row.id !== message.id), message];
}

/** Event identity is its durable sequence number, so replayed frames are ignored. */
export function appendProtocolEvent(
  rows: ProtocolEvent[],
  event: ProtocolEvent,
): ProtocolEvent[] {
  return rows.some((row) => row.seq === event.seq) ? rows : [...rows, event];
}

/**
 * Recover the durable event cursor from the visible transcript before opening
 * SSE. This preserves Hermes Agent's conservative parsing: an unrecognised id
 * never advances the cursor past real durable state.
 */
export function computeHighWaterSeqFromTranscript(rows: TranscriptMessage[]): number {
  let high = 0;
  for (const row of rows) {
    const id = row.id ?? "";
    const lastColon = id.lastIndexOf(":");
    const candidate = lastColon >= 0 ? id.slice(lastColon + 1) : id;
    const match = candidate.match(/(\d+)$/);
    if (!match) continue;
    const parsed = Number.parseInt(match[1], 10);
    if (Number.isFinite(parsed) && parsed > high) high = parsed;
  }
  return high;
}

/**
 * Frames that must flush the renderer queue before their own projection is
 * applied. A terminal frame cannot be allowed to overtake text already
 * received from the daemon.
 */
export function isTerminalStreamEvent(event: ProtocolEvent): boolean {
  switch (event.$kind) {
    case "assistantMessageCompleted":
    case "sessionTurnFinished":
    case "runInterrupted":
    case "runFailed":
    case "budgetExceeded":
      return true;
    default:
      return false;
  }
}

/** Tool rows are synchronous projections and must not overtake earlier text. */
export function shouldFlushBeforeEvent(event: ProtocolEvent): boolean {
  return isTerminalStreamEvent(event) || event.$kind === "toolCallStarted" || event.$kind === "toolResultAdded";
}

export interface StreamScope {
  sessionId: string;
  generation: number;
}

export interface StreamReconcilerSnapshot extends StreamScope {
  lastSeq: number;
}

/**
 * Owns only renderer-side stream admission. It does not own the SSE transport
 * or daemon state. A generation token rejects frames from a disposed stream,
 * while the per-session sequence cursor rejects replay and late frames.
 */
export class StreamReconciler {
  private activeSessionId: string | null = null;
  private generation = 0;
  private lastSeq = 0;

  openSession(sessionId: string, afterSeq = 0): StreamScope {
    this.activeSessionId = sessionId;
    this.lastSeq = Math.max(0, afterSeq);
    this.generation += 1;
    return this.scope();
  }

  closeSession(): void {
    this.activeSessionId = null;
    this.lastSeq = 0;
    this.generation += 1;
  }

  seedFromTranscript(sessionId: string, rows: TranscriptMessage[]): StreamScope {
    return this.openSession(sessionId, computeHighWaterSeqFromTranscript(rows));
  }

  scope(): StreamScope {
    if (this.activeSessionId === null) {
      throw new Error("Cannot create a stream scope before opening a session");
    }
    return { sessionId: this.activeSessionId, generation: this.generation };
  }

  snapshot(): StreamReconcilerSnapshot {
    return { ...this.scope(), lastSeq: this.lastSeq };
  }

  /** Returns true only for a current-session, current-generation, new frame. */
  accept(event: ProtocolEvent, scope: StreamScope = this.scope()): boolean {
    if (
      this.activeSessionId === null ||
      scope.generation !== this.generation ||
      scope.sessionId !== this.activeSessionId ||
      event.sessionId !== this.activeSessionId ||
      event.seq <= this.lastSeq
    ) {
      return false;
    }
    this.lastSeq = event.seq;
    return true;
  }

  /** Sort a catch-up page before admission; daemon order remains authoritative. */
  acceptHistory(
    events: ProtocolEvent[],
    onEvent?: (event: ProtocolEvent) => void,
    scope: StreamScope = this.scope(),
  ): ProtocolEvent[] {
    const accepted: ProtocolEvent[] = [];
    for (const event of [...events].sort((a, b) => a.seq - b.seq)) {
      if (!this.accept(event, scope)) continue;
      accepted.push(event);
      onEvent?.(event);
    }
    return accepted;
  }
}

/** Stable factory for hook refs and tests. */
export function createStreamReconciler(): StreamReconciler {
  return new StreamReconciler();
}
