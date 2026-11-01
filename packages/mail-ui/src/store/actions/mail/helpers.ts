import { appendDiag, type Thread } from "../../../lib/ipc.js";
import { keyOf } from "../../../lib/mail.js";

export const SLOW_SCOPE_MS = 250;

export function nowMs() {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

export function perfVisibility() {
  return typeof document === "undefined" ? "unknown" : document.visibilityState;
}

export function appendSlowDiag(line: string) {
  appendDiag(`${new Date().toISOString()} ${line}`).catch(() => {});
}

function sameThread(a: Thread, b: Thread): boolean {
  const aKeys = Object.keys(a) as (keyof Thread)[];
  const bKeys = Object.keys(b) as (keyof Thread)[];
  if (aKeys.length !== bKeys.length) return false;
  return aKeys.every((key) => Object.is(a[key], b[key]));
}

export function mergeThreadRows(prev: Thread[], next: Thread[]): Thread[] {
  const prevByKey = new Map(prev.map((t) => [keyOf(t), t]));
  let changed = prev.length !== next.length;
  const merged = next.map((row, index) => {
    const old = prevByKey.get(keyOf(row));
    if (old && sameThread(old, row)) {
      if (prev[index] !== old) changed = true;
      return old;
    }
    changed = true;
    return row;
  });
  return changed ? merged : prev;
}
