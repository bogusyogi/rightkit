/**
 * Renderer-only guards for asynchronous runtime work.
 *
 * This carries Hermes Agent's stale-frame discipline into the the host app
 * renderer: a session switch advances a generation, and each new request
 * advances a request token. Both are required before an asynchronous result
 * may update UI state. The daemon remains authoritative for event ordering;
 * these guards only stop an old renderer callback from crossing a UI boundary.
 */

export interface RuntimeScope {
  sessionId: string;
  generation: number;
}

export interface RuntimeRequestToken extends RuntimeScope {
  requestId: number;
}

export interface RuntimeGuardSnapshot extends RuntimeScope {
  requestId: number;
}

export interface SessionFrame {
  sessionId: string;
}

export class RuntimeGuards {
  private activeSessionId: string | null = null;
  private generation = 0;
  private requestId = 0;
  private activeRequestId = 0;

  /** Start (or re-open) a renderer session and invalidate every old callback. */
  openSession(sessionId: string): RuntimeScope {
    if (!sessionId) throw new Error("Cannot open a runtime guard without a session id");
    this.activeSessionId = sessionId;
    this.generation += 1;
    this.requestId = 0;
    this.activeRequestId = 0;
    return this.scope();
  }

  /** Invalidate the current session, including callbacks for the same id. */
  closeSession(): void {
    this.activeSessionId = null;
    this.generation += 1;
    this.requestId = 0;
    this.activeRequestId = 0;
  }

  /** Begin a latest-wins request in the current session. */
  beginRequest(sessionId = this.activeSessionId): RuntimeRequestToken {
    if (!sessionId || sessionId !== this.activeSessionId) {
      throw new Error("Cannot begin a runtime request outside the active session");
    }
    const requestId = ++this.requestId;
    this.activeRequestId = requestId;
    return { sessionId, generation: this.generation, requestId };
  }

  scope(): RuntimeScope {
    if (this.activeSessionId === null) {
      throw new Error("Cannot create a runtime scope before opening a session");
    }
    return { sessionId: this.activeSessionId, generation: this.generation };
  }

  snapshot(): RuntimeGuardSnapshot {
    return { ...this.scope(), requestId: this.activeRequestId };
  }

  /** True only for the newest request in the current session generation. */
  isCurrent(token: RuntimeRequestToken): boolean {
    return (
      this.activeSessionId !== null &&
      token.sessionId === this.activeSessionId &&
      token.generation === this.generation &&
      token.requestId === this.activeRequestId
    );
  }

  /** True only for a frame from the current session generation. */
  acceptsFrame(scope: RuntimeScope, frame: SessionFrame): boolean {
    return (
      this.activeSessionId !== null &&
      scope.sessionId === this.activeSessionId &&
      frame.sessionId === this.activeSessionId &&
      scope.generation === this.generation
    );
  }

  /** Invalidate a request without changing the active session. */
  invalidateRequest(): void {
    this.activeRequestId = ++this.requestId;
  }
}

export function createRuntimeGuards(): RuntimeGuards {
  return new RuntimeGuards();
}
