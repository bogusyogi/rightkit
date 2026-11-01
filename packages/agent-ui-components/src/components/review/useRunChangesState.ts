// I1 — React hook for the run-changes state surface.
//
// Thin re-export of a local reducer-driven store so the I2 shell
// lane can swap the source (Zustand, context, props) without
// rewriting every consumer. The hook uses `useSyncExternalStore`
// against the module-scoped store, which I2 will lift onto
// `useContextStore`.

import { useSyncExternalStore } from "react";
import {
  reducer,
  INITIAL_RUN_CHANGES_STATE,
  type RunChangesAction,
  type RunChangesState,
} from "./runChangesStore";

type Listener = () => void;
const listeners = new Set<Listener>();
let current: RunChangesState = INITIAL_RUN_CHANGES_STATE;

function notify() {
  for (const listener of listeners) listener();
}

function subscribe(listener: Listener) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function getSnapshot() {
  return current;
}

/** Read-only imperative snapshot for ReviewPane and WorkbenchContent. */
export function getRunChangesStateSnapshot(): RunChangesState {
  return current;
}

export function dispatch(action: RunChangesAction) {
  current = reducer(current, action);
  notify();
}

export function resetStore(nextState: RunChangesState = INITIAL_RUN_CHANGES_STATE) {
  current = nextState;
  notify();
}

export function useRunChangesState(): RunChangesState {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
