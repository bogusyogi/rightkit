// Pure save/list/run/remove logic for lane C6 saved searches, extracted out
// of AppSearchTopbar/Sidebar so it is unit-testable without mounting React
// (this repo's test runner has no jsdom — see tests/shortcutFocusGuard.test.ts
// for the same constraint). Persists as ONE preference key via src/lib/preferences.ts.
import type { SavedSearch } from "../store/types";

export const SAVED_SEARCHES_KEY = "savedSearches";

/** True if `query` (trimmed) is already saved. */
export function isQuerySaved(list: SavedSearch[], query: string): boolean {
  const q = query.trim();
  return q.length > 0 && list.some((s) => s.query === q);
}

/**
 * Toggle save state for `query`: adds {name: query, query} if absent, removes
 * the matching entry if present. Returns the next list (does not mutate the
 * input). No-ops (returns the same list) for a blank query.
 */
export function toggleSavedSearch(list: SavedSearch[], query: string): SavedSearch[] {
  const q = query.trim();
  if (!q) return list;
  const existing = list.find((s) => s.query === q);
  if (existing) return list.filter((s) => s.query !== q);
  return [...list, { name: q, query: q }];
}

/** Remove the saved search matching `query` exactly. Returns the next list. */
export function removeSavedSearch(list: SavedSearch[], query: string): SavedSearch[] {
  return list.filter((s) => s.query !== query);
}
