import type { HistoryRow } from "../contract";

/** Mirror of `scraperight_protocol::HistoryPage`. */
export interface HistoryPage {
  rows: HistoryRow[];
  next_cursor: string | null;
  /** Rows matching the query across all pages; sent on the first page only. */
  total?: number | null;
}

/** Mirror of `scraperight_protocol::HistorySort` (snake_case values). */
export type HistorySort = "newest" | "oldest" | "longest" | "shortest";

/** Rows per page requested from the shell. */
export const HISTORY_PAGE_SIZE = 100;

/** Append `incoming` rows, skipping ids already present (newest-first order kept). */
export function mergeHistoryRows(current: HistoryRow[], incoming: HistoryRow[]): HistoryRow[] {
  if (!incoming.length) return current;
  const seen = new Set(current.map((row) => row.id));
  const fresh = incoming.filter((row) => !seen.has(row.id));
  return fresh.length ? [...current, ...fresh] : current;
}

/** True when a scroll container is within `threshold` px of its bottom. */
export function nearBottom(el: HTMLElement, threshold = 240): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < threshold;
}

/**
 * Notice shown when more history pages exist on the engine. The Library sorts
 * and filters only the loaded rows, so the user must see that cap (issue #228).
 */
export function loadedRowsNotice(loaded: number, hasMore: boolean): string | null {
  if (!hasMore) return null;
  return `Sort and filters apply to the ${loaded} newest transcripts loaded so far. More load as you scroll.`;
}

/** "N of M" for the list header, or just N when the engine sent no total. */
export function countLabel(loaded: number, total: number | null | undefined): string {
  return typeof total === "number" ? `${loaded} of ${total}` : `${loaded}`;
}
