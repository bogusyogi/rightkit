/**
 * Palette / Jump-to ranking. Merges ViewRight's subsequence scorer (typo-tolerant, rewards early
 * hits) with HeardRight/CodeRight's substring + keyword matching. Higher score is better; null is
 * no match.
 */

function subsequence(query: string, text: string): number | null {
  let qi = 0;
  let firstHit = -1;
  let gaps = 0;
  for (let li = 0; li < text.length && qi < query.length; li++) {
    if (query[qi] === text[li]) {
      if (firstHit < 0) firstHit = li;
      qi++;
    } else if (firstHit >= 0) {
      gaps++;
    }
  }
  return qi === query.length ? 40 - Math.min(30, firstHit + gaps) : null;
}

export function matchScore(query: string, label: string, keywords: readonly string[] = [], hint = ""): number | null {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const l = label.toLowerCase();
  if (l === q) return 200;
  if (l.startsWith(q)) return 160 - Math.min(20, l.length - q.length);
  const word = l.split(/[\s/_.:›-]+/).findIndex((w) => w.startsWith(q));
  if (word >= 0) return 120 - word;
  const at = l.indexOf(q);
  if (at >= 0) return 100 - Math.min(40, at);
  if (keywords.some((k) => k.toLowerCase().includes(q))) return 70;
  if (hint && hint.toLowerCase().includes(q)) return 60;
  return subsequence(q, l);
}

export interface Rankable {
  label: string;
  keywords?: readonly string[];
  hint?: string;
}

/** Stable sort by score; ties keep input order. Empty query keeps everything in input order. */
export function rankItems<T extends Rankable>(items: readonly T[], query: string, limit = 50): T[] {
  const scored: { item: T; score: number; index: number }[] = [];
  items.forEach((item, index) => {
    const score = matchScore(query, item.label, item.keywords, item.hint);
    if (score !== null) scored.push({ item, score, index });
  });
  if (query.trim()) scored.sort((a, b) => b.score - a.score || a.index - b.index);
  return scored.slice(0, limit).map((entry) => entry.item);
}

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
