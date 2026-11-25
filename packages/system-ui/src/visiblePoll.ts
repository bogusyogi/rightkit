import { useEffect, useLayoutEffect, useRef } from "react";
import { clearVisibleInterval, visibleInterval } from "./visible.js";

/** What one read ended with. */
export type PollOutcome<T> = { ok: true; value: T } | { ok: false; error: unknown };

/**
 * Reads at once, then on every tick of `visibleInterval` (which skips ticks while the host's window
 * is hidden and runs once when it is shown again), and hands each outcome to `apply`. A reply that
 * arrives after `stop`, and a reply older than one already applied, is ignored, so a slow read can
 * never overwrite a newer result. Returns `stop`.
 */
export function startVisiblePoll<T>(
  read: () => Promise<T>,
  ms: number,
  apply: (outcome: PollOutcome<T>) => void,
): () => void {
  let alive = true;
  let issued = 0;
  let applied = 0;
  const settle = (mine: number, outcome: PollOutcome<T>) => {
    if (!alive || mine < applied) return;
    applied = mine;
    apply(outcome);
  };
  const probe = () => {
    const mine = ++issued;
    let pending: Promise<T>;
    try {
      pending = read();
    } catch (error) {
      pending = Promise.reject(error);
    }
    pending.then(
      (value) => settle(mine, { ok: true, value }),
      (error: unknown) => settle(mine, { ok: false, error }),
    );
  };
  probe();
  const timer = visibleInterval(probe, ms);
  return () => {
    alive = false;
    clearVisibleInterval(timer);
  };
}

/**
 * `startVisiblePoll` for a component: it polls while mounted. `read` and `apply` may change on
 * every render without restarting the poll (the latest committed ones are used); a new `ms`
 * restarts it.
 */
export function useVisiblePoll<T>(
  read: () => Promise<T>,
  ms: number,
  apply: (outcome: PollOutcome<T>) => void,
): void {
  const latest = useRef({ read, apply });
  useLayoutEffect(() => {
    latest.current = { read, apply };
  });
  useEffect(
    () =>
      startVisiblePoll(
        () => latest.current.read(),
        ms,
        (outcome) => latest.current.apply(outcome),
      ),
    [ms],
  );
}
