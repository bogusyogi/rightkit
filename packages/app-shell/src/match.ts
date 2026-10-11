/**
 * Palette / Jump-to ranking. The scorer and ranker live in `@rightkit/platform-ui/headless` since 0.2.2
 * (so locked-UI apps can use them without this package); they are re-exported here unchanged.
 */
export { matchScore, rankItems, type Rankable } from "@rightkit/platform-ui/headless";

export interface PaletteGroup<T> {
  name: string;
  items: T[];
}

/** Group by `group` preserving first-seen group order and rank order within each group. */
export function groupByName<T extends { group?: string }>(items: readonly T[], fallback = "Commands"): PaletteGroup<T>[] {
  const order: string[] = [];
  const map = new Map<string, T[]>();
  for (const item of items) {
    const name = item.group ?? fallback;
    if (!map.has(name)) {
      map.set(name, []);
      order.push(name);
    }
    map.get(name)!.push(item);
  }
  return order.map((name) => ({ name, items: map.get(name)! }));
}

/** Pointer/keyboard selection model (HeardRight): a resting mouse must not steal the keyboard selection. */
export interface PaletteSelection {
  active: number;
  pointer: { x: number; y: number } | null;
  keyboardDriving: boolean;
}

export const initialPaletteSelection: PaletteSelection = { active: 0, pointer: null, keyboardDriving: false };

export function selectByPointer(current: PaletteSelection, row: number, next: { x: number; y: number }): PaletteSelection {
  if (current.keyboardDriving) {
    if (current.pointer === null) return { ...current, pointer: next };
    if (current.pointer.x === next.x && current.pointer.y === next.y) return current;
  } else if (current.pointer?.x === next.x && current.pointer.y === next.y) {
    return current;
  }
  return { active: row, pointer: next, keyboardDriving: false };
}

export function selectByKeyboard(current: PaletteSelection, direction: "down" | "up", count: number): PaletteSelection {
  const delta = direction === "down" ? 1 : -1;
  const active = count === 0 ? 0 : (current.active + delta + count) % count;
  return { ...current, active, keyboardDriving: true };
}
