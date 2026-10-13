export interface CheckGateOptions {
  /** Minimum ms between real checks. A call inside the window returns the cached result. */
  minIntervalMs: number;
  /** Injectable clock for tests and hosts with their own time source. */
  now?: () => number;
}

export interface CheckGate<T> {
  /** Run `fn` unless a result is fresh; concurrent calls share one in-flight run. */
  run(fn: () => Promise<T>, options?: { force?: boolean }): Promise<T>;
  /** Drop the cached result so the next run is real. */
  reset(): void;
}

/**
 * Debounce and cache policy for update checks. Failures are never cached, so a
 * failed check can be retried immediately. Hosts own scheduling; this only
 * collapses redundant checks.
 */
export function createCheckGate<T>(options: CheckGateOptions): CheckGate<T> {
  const now = options.now ?? (() => Date.now());
  let cached: { at: number; value: T } | null = null;
  let inflight: Promise<T> | null = null;
  return {
    run(fn, runOptions) {
      if (!runOptions?.force) {
        if (inflight) return inflight;
        if (cached && now() - cached.at < options.minIntervalMs) return Promise.resolve(cached.value);
      }
      const started = fn().then(
        (value) => {
          cached = { at: now(), value };
          if (inflight === started) inflight = null;
          return value;
        },
        (error) => {
          if (inflight === started) inflight = null;
          throw error;
        },
      );
      inflight = started;
      return started;
    },
    reset() {
      cached = null;
    },
  };
}
