// The setup microphone meter: a live level (0..1) from a native event, and the numbers a meter draws from it. The
// bars, colours and markup are the host's. No host names.
import { useEffect, useRef, useState } from "react";

/** Clamps a native level payload to 0..1; anything not a number is 0. */
export function clampMicLevel(value: unknown): number {
  return Math.max(0, Math.min(1, Number(value) || 0));
}

/** How many of `bars` light up for `level`: none at 0, at least one for any signal. Default 12 bars. */
export function micMeterActiveBars(level: number, bars = 12): number {
  return level <= 0 ? 0 : Math.max(1, Math.round(level * bars));
}

export interface MicMeterBarShape {
  /** Height with no signal, px. Default 9. */
  base?: number;
  /** Amplitude of the per-bar sine wobble, px. Default 4. */
  wave?: number;
  /** Extra height at full level, px. Default 17. */
  gain?: number;
  /** Phase step per bar. Default 0.9. */
  step?: number;
}

/** Height in px of the bar at zero-based `index` for `level`. */
export function micMeterBarHeight(index: number, level: number, shape: MicMeterBarShape = {}): number {
  const { base = 9, wave = 4, gain = 17, step = 0.9 } = shape;
  return base + Math.sin((index + 1) * step) * wave + level * gain;
}

/**
 * Subscribes to a native level event. `subscribe` registers the handler and resolves (or returns) the unsubscribe;
 * the handler gets the raw event payload. Return it the way the host's event helper does.
 */
export type MicLevelSubscribe<P = { level?: unknown }> = (handler: (payload: P) => void) => Promise<() => void> | (() => void);

/**
 * The live level, 0..1, while `active`; 0 when not. `readLevel` extracts the level from the payload (default `payload.level`).
 * Only the component that calls it re-renders on each event.
 */
export function useMicLevel<P = { level?: unknown }>(
  subscribe: MicLevelSubscribe<P>,
  options: { active?: boolean; readLevel?: (payload: P) => unknown } = {},
): number {
  const { active = true, readLevel } = options;
  const [level, setLevel] = useState(0);
  const subscribeRef = useRef(subscribe);
  subscribeRef.current = subscribe;
  const readRef = useRef(readLevel);
  readRef.current = readLevel;
  useEffect(() => {
    if (!active) {
      setLevel(0);
      return;
    }
    let disposed = false;
    let off: (() => void) | undefined;
    let pending: Promise<() => void> | (() => void);
    try {
      pending = subscribeRef.current((payload) => {
        if (disposed) return;
        const read = readRef.current;
        setLevel(clampMicLevel(read ? read(payload) : (payload as { level?: unknown }).level));
      });
    } catch {
      return undefined;
    }
    void Promise.resolve(pending)
      .then((unsubscribe) => {
        if (disposed) unsubscribe();
        else off = unsubscribe;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      off?.();
    };
  }, [active]);
  return active ? level : 0;
}
