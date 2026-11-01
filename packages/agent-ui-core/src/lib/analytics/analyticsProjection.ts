/**
 * Renderer-side projection of Waku's privacy-conscious analytics contract.
 *
 * The renderer never owns transport or credentials. It accepts only the small,
 * coarse event vocabulary below and hands already-redacted events to an
 * injected sink. The sink may be backed by a native worker; tracking is
 * bounded and never waits for network I/O.
 */

export const ANALYTICS_STORAGE_KEY = "coderight.analytics.enabled";
export const ANALYTICS_QUEUE_CAPACITY = 128;

export type AnalyticsEvent =
  | { name: "app.launched"; data: { taskCount: number; projectCount: number } }
  | { name: "project.added"; data: Record<string, never> }
  | { name: "right_panel.opened"; data: Record<string, never> }
  | {
      name: "provider.turn.sent";
      data: {
        provider: string;
        model: string;
        turnNumber: number;
        workspace: "worktree" | "projectless" | "workspace";
        projectless: boolean;
        attachmentCount: number;
        hasInput: boolean;
      };
    }
  | {
      name: "provider.turn.finished";
      data: {
        provider: string;
        turnNumber: number;
        outcome:
          | "completed"
          | "failed"
          | "cancelled"
          | "process_exited"
          | "start_failed"
          | "preparation_failed";
        durationSeconds: number;
      };
    }
  | {
      name: "provider.request.responded";
      data: { provider: string; kind: string; decision: string };
    }
  | {
      name: "provider.conversation.rolled_back";
      data: { provider: string; turns: number };
    }
  | {
      name: "provider.response.forked";
      data: { provider: string; turnNumber: number };
    };

export interface AnalyticsContext {
  clientType: "desktop";
  version: string;
  platform: string;
  arch: string;
  build: "debug" | "release";
}

export type ProjectedAnalyticsEvent = AnalyticsEvent & {
  data: AnalyticsEvent["data"] & AnalyticsContext;
};

export interface AnalyticsSink {
  track: (event: ProjectedAnalyticsEvent) => void | Promise<void>;
  setEnabled?: (enabled: boolean) => void | Promise<void>;
}

export function readAnalyticsConsent(storage: Storage | undefined = globalThis.localStorage): boolean {
  try {
    const value = storage?.getItem(ANALYTICS_STORAGE_KEY);
    if (value === null || value === undefined) return true;
    return !["0", "false", "off", "no"].includes(value.trim().toLowerCase());
  } catch {
    return true;
  }
}

export function writeAnalyticsConsent(
  enabled: boolean,
  storage: Storage | undefined = globalThis.localStorage,
): void {
  try {
    storage?.setItem(ANALYTICS_STORAGE_KEY, enabled ? "1" : "0");
  } catch {
    // Consent must not make the settings surface fail in restricted webviews.
  }
}

/** Add the Waku-compatible coarse metadata without accepting prompts, paths,
 * provider output, account identity, or other user content. */
export function projectAnalyticsEvent(
  event: AnalyticsEvent,
  context: Omit<AnalyticsContext, "clientType"> & { clientType?: "desktop" },
): ProjectedAnalyticsEvent {
  return {
    ...event,
    data: {
      ...event.data,
      clientType: "desktop",
      version: context.version,
      platform: context.platform,
      arch: context.arch,
      build: context.build,
    },
  } as ProjectedAnalyticsEvent;
}

/**
 * A non-blocking, bounded analytics handle. Disabling immediately drops both
 * future and queued events, matching Waku's worker-side opt-out behavior.
 */
export class AnalyticsProjection {
  private enabledValue: boolean;
  private readonly queue: ProjectedAnalyticsEvent[] = [];
  private flushing = false;

  constructor(
    private readonly sink: AnalyticsSink,
    enabled = readAnalyticsConsent(),
  ) {
    this.enabledValue = enabled;
    this.notifySink(enabled);
  }

  get enabled(): boolean {
    return this.enabledValue;
  }

  setEnabled(enabled: boolean): void {
    this.enabledValue = enabled;
    writeAnalyticsConsent(enabled);
    if (!enabled) this.queue.length = 0;
    this.notifySink(enabled);
    if (enabled) void this.flush();
  }

  track(event: ProjectedAnalyticsEvent): void {
    if (
      !this.enabledValue ||
      this.queue.length + (this.flushing ? 1 : 0) >= ANALYTICS_QUEUE_CAPACITY
    ) return;
    this.queue.push(event);
    void this.flush();
  }

  private notifySink(enabled: boolean): void {
    if (!this.sink.setEnabled) return;
    void Promise.resolve(this.sink.setEnabled(enabled)).catch(() => undefined);
  }

  private async flush(): Promise<void> {
    if (this.flushing || !this.enabledValue) return;
    this.flushing = true;
    try {
      while (this.enabledValue && this.queue.length > 0) {
        const event = this.queue.shift();
        if (event) {
          try {
            await this.sink.track(event);
          } catch {
            // One failed best-effort event must not stop later product events.
          }
        }
      }
    } finally {
      this.flushing = false;
    }
  }
}

export function createAnalyticsProjection(
  sink: AnalyticsSink,
  enabled = readAnalyticsConsent(),
): AnalyticsProjection {
  return new AnalyticsProjection(sink, enabled);
}
