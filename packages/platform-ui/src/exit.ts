/* Headless exit-transition state machine: open -> closing -> closed. The app maps `closing` to its own class. */
import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { useLatest } from "./focus.js";

export type ExitPhase = "open" | "closing" | "closed";

export interface ExitTransitionOptions {
  /** Length of the exit in ms; 0 or less finishes at once. Default 80. */
  exitMs?: number;
  /**
   * What `close(action)` does with an async `action`. `"immediately"` (default): the exit starts, then `action`
   * runs. `"settled"`: `action` runs first and the exit starts only after the value it returns settles (fulfilled
   * or rejected), the phase staying `open` with `pending` true meanwhile.
   */
  closeAfter?: "immediately" | "settled";
  /**
   * `close(action)` called while the exit is running or a `"settled"` action is pending. `"drop"` (default): the
   * action is ignored. `"run"`: the action still runs (the close itself is never repeated), as a wrapper that always
   * calls its action would. A double Enter or double click then executes the action twice; this exists for parity
   * with such wrappers, not as a recommendation. A finished (`closed`) transition always drops.
   */
  onActionWhileClosing?: "drop" | "run";
  /**
   * An `action` that throws synchronously. `"close"` (default): the throw becomes a rejected promise from `close()`
   * and the close still happens. `"propagate"`: the throw is rethrown synchronously to the caller of `close()`, the
   * transition does not start and state is unchanged (the menu stays open), as for a wrapper that calls its action
   * before wrapping it in a promise. In this mode the action runs before `onExitStart`, also for `"immediately"`.
   * Asynchronous rejections behave the same in both modes: the close happens and the promise rejects.
   */
  onSyncThrow?: "close" | "propagate";
  /** Start in `open` (default) or `closed`. Read once, when the hook mounts. */
  initialOpen?: boolean;
  /** Called once when the exit starts (phase becomes `closing`, or `closed` when `exitMs` <= 0). */
  onExitStart?: () => void;
  /** Called once when the exit finishes (phase becomes `closed`), e.g. to unmount. */
  onClosed?: () => void;
}

export interface ExitState {
  phase: ExitPhase;
  /** A `"settled"` action is still running. */
  pending: boolean;
}

export interface ExitTransition {
  phase: ExitPhase;
  /** `phase === "closing"`: apply the exit class while true. */
  closing: boolean;
  /** `phase === "open"`. */
  isOpen: boolean;
  pending: boolean;
  /** Start (or restart) an open instance. Any exit or action in flight belongs to the old instance and is ignored. */
  open(): void;
  /**
   * Close, optionally running `action` (the chosen command). Ignored unless `open` and not `pending` (see `onActionWhileClosing` for the action). Always
   * returns a promise: it resolves once the exit has started (`"settled"`: after `action` settled) and rejects
   * with `action`'s error, but the close still happens.
   */
  close(action?: () => unknown): Promise<void>;
}

type Outcome = { ok: true; value: unknown } | { ok: false; error: unknown };
const noop = () => undefined;
const call = (action: () => unknown): Outcome => {
  try {
    return { ok: true, value: action() };
  } catch (error) {
    return { ok: false, error };
  }
};

type Core = {
  getState(): ExitState;
  subscribe(listener: () => void): () => void;
  open(): void;
  close(action?: () => unknown): Promise<void>;
  /** Clear the timer and orphan anything in flight. The core stays usable. */
  dispose(): void;
};

/** Framework-free core of {@link useExitTransition}. `options` is read at each call, so it can follow props. */
export function createExitTransition(options: () => ExitTransitionOptions): Core {
  let state: ExitState = { phase: (options().initialOpen ?? true) ? "open" : "closed", pending: false };
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const listeners = new Set<() => void>();
  const set = (next: ExitState) => {
    if (next.phase === state.phase && next.pending === state.pending) return;
    state = next;
    for (const l of [...listeners]) l();
  };
  const clear = () => {
    if (timer != null) clearTimeout(timer);
    timer = null;
  };
  const finish = (gen: number) => {
    if (gen !== generation) return;
    timer = null;
    set({ phase: "closed", pending: false });
    options().onClosed?.();
  };
  const startExit = (gen: number) => {
    if (gen !== generation || state.phase !== "open") return;
    const { exitMs = 80, onExitStart } = options();
    if (exitMs > 0) {
      set({ phase: "closing", pending: false });
      onExitStart?.();
      timer = setTimeout(() => finish(gen), exitMs);
    } else {
      onExitStart?.();
      finish(gen);
    }
  };
  return {
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    open() {
      generation++;
      clear();
      set({ phase: "open", pending: false });
    },
    close(action) {
      const propagate = options().onSyncThrow === "propagate";
      const done = (r: Outcome): Promise<void> => (r.ok ? Promise.resolve(r.value).then(noop) : Promise.reject(r.error));
      if (state.phase !== "open" || state.pending) {
        if (!action || state.phase === "closed" || options().onActionWhileClosing !== "run") return Promise.resolve();
        const r = call(action);
        if (!r.ok && propagate) throw r.error;
        return done(r);
      }
      const gen = generation;
      if (!action) {
        startExit(gen);
        return Promise.resolve();
      }
      if (options().closeAfter === "settled") {
        let r: Outcome;
        if (propagate) {
          r = call(action);
          if (!r.ok) throw r.error;
          set({ phase: "open", pending: true });
        } else {
          set({ phase: "open", pending: true });
          r = call(action);
        }
        // A newer open() or an unmount orphaned this close: it must not touch the new instance.
        return done(r).finally(() => {
          if (gen === generation) {
            set({ phase: "open", pending: false });
            startExit(gen);
          }
        });
      }
      if (propagate) {
        const r = call(action);
        if (!r.ok) throw r.error;
        startExit(gen);
        return done(r);
      }
      startExit(gen);
      return done(call(action));
    },
    dispose() {
      generation++;
      clear();
    },
  };
}

/**
 * Exit state for a modal, popover or menu. `open()` after a close, or while one is closing, starts a fresh
 * instance: the old timer and any unsettled action can no longer close it. Timers are cleared on unmount.
 */
export function useExitTransition(options: ExitTransitionOptions = {}): ExitTransition {
  const latest = useLatest(options);
  const [core] = useState(() => createExitTransition(() => latest.current));
  const state = useSyncExternalStore(core.subscribe, core.getState, core.getState);
  useEffect(() => () => core.dispose(), [core]);
  return useMemo(
    () => ({
      phase: state.phase,
      closing: state.phase === "closing",
      isOpen: state.phase === "open",
      pending: state.pending,
      open: core.open,
      close: core.close,
    }),
    [state, core],
  );
}
