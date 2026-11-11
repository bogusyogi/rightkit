import type { Subscribe, Unsubscribe } from "./host.js";

// The host may keep running with its window closed, and this page keeps running with it. Refreshing a page
// nobody can see costs CPU for nothing. The host says when its window is hidden or shown (the `visibility`
// event of `SystemEventMap`); until it says anything the page counts as shown, and any input in the page
// counts as shown too, so a missed event cannot leave a visible page frozen.
let shown = true;
const wakers = new Map<number, () => void>();
let stopWatching: Unsubscribe | null = null;

function setShown(next: boolean) {
  if (shown === next) return;
  shown = next;
  if (next) for (const wake of [...wakers.values()]) wake();
}

/** Follows the host's `visibility` event (`configureSystemHost` calls this). */
export function watchVisibility(subscribe: Subscribe): void {
  stopWatching?.();
  stopWatching = null;
  void subscribe("visibility", (event) => setShown(event.payload === true))
    .then((stop) => {
      stopWatching = stop;
    })
    .catch(() => {});
}

if (typeof window !== "undefined") {
  for (const name of ["focus", "pointerdown", "pointermove", "keydown"]) {
    window.addEventListener(name, () => setShown(true), { passive: true, capture: true });
  }
}

/** `setInterval` that skips its ticks while the host's window is hidden and runs once when it is shown again. */
export function visibleInterval(tick: () => void, ms: number): number {
  const id = setInterval(() => {
    if (shown) tick();
  }, ms) as unknown as number;
  wakers.set(id, tick);
  return id;
}

export function clearVisibleInterval(id: number | null | undefined) {
  if (id == null) return;
  clearInterval(id);
  wakers.delete(id);
}
