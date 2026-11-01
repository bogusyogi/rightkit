import { listen } from "@tauri-apps/api/event";

// The hub keeps running with its window closed (it serves the notch), and this page keeps
// running with it. Refreshing a page nobody can see kept the web view and the hub at about
// one percent of a core between them. The hub says when its window is hidden or shown
// (`hub-visibility`); until it says anything the page counts as shown, and any input in the
// page counts as shown too, so a missed event cannot leave a visible page frozen.
let shown = true;
const wakers = new Map<number, () => void>();

function setShown(next: boolean) {
  if (shown === next) return;
  shown = next;
  if (next) for (const wake of [...wakers.values()]) wake();
}

try {
  void listen<boolean>("hub-visibility", (event) => setShown(event.payload === true)).catch(() => {});
} catch {
  // Outside the hub (a test page) there is nobody to say; the page stays shown.
}
if (typeof window !== "undefined") {
  for (const name of ["focus", "pointerdown", "pointermove", "keydown"]) {
    window.addEventListener(name, () => setShown(true), { passive: true, capture: true });
  }
}

/** `setInterval` that skips its ticks while the hub's window is hidden and runs once when it is shown again. */
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
