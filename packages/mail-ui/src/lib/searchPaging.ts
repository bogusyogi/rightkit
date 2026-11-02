// Search "load more": keyset paging over the backend's newest-first pages.
import { searchThreads, type SearchCursor, type Thread } from "./ipc.js";

/** Threads per backend search page (db::SEARCH_PAGE_SIZE). */
export const SEARCH_PAGE_SIZE = 200;

type Row = Pick<Thread, "account_id" | "thread_id" | "last_message_at">;

/** A page shorter than the page size is the last one. */
export function hasMorePages(page: readonly unknown[]): boolean {
  return page.length >= SEARCH_PAGE_SIZE;
}

/** Cursor for the page after `page` (server rows only), or null when done. */
export function nextSearchCursor(page: readonly Row[]): SearchCursor | null {
  if (!hasMorePages(page)) return null;
  const last = page[page.length - 1];
  return {
    last_message_at: last.last_message_at,
    account_id: last.account_id,
    thread_id: last.thread_id,
  };
}

/** Append `page` to `prev`, skipping threads already present. */
export function appendPage<T extends Partial<Row>>(
  prev: readonly T[],
  page: readonly T[],
): T[] {
  const key = (t: T) => `${t.account_id}:${t.thread_id}`;
  const seen = new Set(prev.map(key));
  const out = [...prev];
  for (const t of page) {
    if (seen.has(key(t))) continue;
    seen.add(key(t));
    out.push(t);
  }
  return out;
}

interface PagerCtx {
  query: string;
  accountId: string | null;
  label: string | null;
  cursor: SearchCursor | null;
}

let pager: PagerCtx | null = null;

/** Register the first page's scope + cursor (null cursor = nothing more). */
export function startSearchPager(ctx: PagerCtx | null): void {
  pager = ctx;
}

/**
 * Fetch the next page for the active search. Returns null when there is
 * nothing to load or the search changed while the request was in flight.
 */
export async function fetchNextSearchPage(): Promise<{
  rows: Thread[];
  done: boolean;
} | null> {
  const ctx = pager;
  if (!ctx || !ctx.cursor) return null;
  const rows = await searchThreads(
    ctx.query,
    ctx.accountId,
    ctx.label,
    null,
    ctx.cursor,
  );
  if (pager !== ctx) return null;
  ctx.cursor = nextSearchCursor(rows);
  return { rows, done: ctx.cursor === null };
}
