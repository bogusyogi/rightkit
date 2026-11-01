const TAB_WARM_ORDER_KEY = "vr.session.tab-warm-order";
const MAX_SAVED_PATHS = 64;

export function loadTabWarmOrder(storage: Pick<Storage, "getItem"> = localStorage): string[] {
  try {
    const parsed: unknown = JSON.parse(storage.getItem(TAB_WARM_ORDER_KEY) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return [...new Set(parsed.filter((value): value is string => typeof value === "string"))].slice(
      0,
      MAX_SAVED_PATHS,
    );
  } catch {
    return [];
  }
}

export function saveTabWarmOrder(
  paths: readonly string[],
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  try {
    storage.setItem(
      TAB_WARM_ORDER_KEY,
      JSON.stringify([...new Set(paths)].slice(0, MAX_SAVED_PATHS)),
    );
  } catch {
    // Storage can be unavailable in hardened webviews. Warm-order persistence is optional.
  }
}

/**
 * Restored tabs warm in prior activation order. Tabs absent from that order use proximity to the
 * active tab, alternating forward/backward, so first idle work benefits likely next clicks.
 */
export function restoredTabWarmOrder(
  openPaths: readonly string[],
  activePath: string | null,
  savedOrder: readonly string[],
): string[] {
  const open = new Set(openPaths);
  const result: string[] = [];
  const add = (path: string | undefined) => {
    if (!path || path === activePath || !open.has(path) || result.includes(path)) return;
    result.push(path);
  };

  for (const path of savedOrder) add(path);

  const activeIndex = activePath ? openPaths.indexOf(activePath) : -1;
  if (activeIndex >= 0) {
    for (let distance = 1; distance < openPaths.length; distance++) {
      add(openPaths[activeIndex + distance]);
      add(openPaths[activeIndex - distance]);
    }
  } else {
    for (const path of openPaths) add(path);
  }
  return result;
}
