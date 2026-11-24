// View-model for a searchable, categorised command catalog (voice commands, actions): a standalone filter, search
// across description, category and phrases, a featured slice and grouping by category. Imports only `react`.
// Loading, copy, chips and layout belong to the host.
import { useEffect, useMemo, useState } from "react";

/** The fields the view-model reads; a host entry may carry more. */
export interface CatalogEntryLike {
  /** Category name; entries are grouped by it. */
  category: string;
  /** What the entry does; searched. */
  description: string;
  /** Spoken or typed phrases; searched. */
  phrases: readonly string[];
}

/** Options of {@link buildCatalogView}. */
export interface CatalogViewOptions {
  /** The search text (trimmed and lower-cased here). Empty shows everything. */
  query: string;
  /** Categories left out of the list (documented elsewhere). Default: none. */
  excludeCategories?: readonly string[];
  /** Size of the featured slice: the first N entries in catalog order. Default 0 (no featured view). */
  featuredCount?: number;
  /** True expands the featured view to the whole catalog. Default false. */
  showAll?: boolean;
}

/** Result of {@link buildCatalogView}. */
export interface CatalogView<E extends CatalogEntryLike> {
  /** The normalised query (trimmed, lower-case). */
  q: string;
  /** Entries after the excluded categories were removed, in catalog order. */
  standalone: readonly E[];
  /** `standalone` filtered by the query. */
  matching: readonly E[];
  /** The first `featuredCount` standalone entries. A search ignores it and covers the whole catalog. */
  featured: readonly E[];
  /** One `[category, entries]` pair per category of `matching`, in first-seen order (categories need not be contiguous). */
  groups: readonly (readonly [string, E[]])[];
  /** True when the featured slice should show: no query, not expanded, and a slice exists. */
  showFeatured: boolean;
  /** True when a show-all / show-fewer toggle makes sense: no query and more entries than the featured slice. */
  canToggle: boolean;
}

/** Pure form of {@link useCatalogView}. */
export function buildCatalogView<E extends CatalogEntryLike>(entries: readonly E[], options: CatalogViewOptions): CatalogView<E> {
  const { excludeCategories = [], featuredCount = 0, showAll = false } = options;
  const q = options.query.trim().toLowerCase();
  const standalone = entries.filter((entry) => !excludeCategories.includes(entry.category));
  const matching = !q
    ? standalone
    : standalone.filter(
        (entry) =>
          entry.description.toLowerCase().includes(q) ||
          entry.category.toLowerCase().includes(q) ||
          entry.phrases.some((phrase) => phrase.toLowerCase().includes(q)),
      );
  const featured = standalone.slice(0, featuredCount);
  const byCategory = new Map<string, E[]>();
  for (const entry of matching) {
    const list = byCategory.get(entry.category) ?? [];
    list.push(entry);
    byCategory.set(entry.category, list);
  }
  return {
    q,
    standalone,
    matching,
    featured,
    groups: [...byCategory.entries()],
    showFeatured: !q && !showAll && featured.length > 0,
    canToggle: !q && standalone.length > featuredCount,
  };
}

/** {@link buildCatalogView}, memoised on `entries` and the options' values. */
export function useCatalogView<E extends CatalogEntryLike>(entries: readonly E[], options: CatalogViewOptions): CatalogView<E> {
  const { query, featuredCount, showAll, excludeCategories } = options;
  const excludeKey = (excludeCategories ?? []).join("\u0000");
  return useMemo(
    () => buildCatalogView(entries, { query, featuredCount, showAll, excludeCategories }),
    // `excludeKey` stands for the array's contents, so an inline array does not rebuild every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [entries, query, featuredCount, showAll, excludeKey],
  );
}

/**
 * A list loaded while `enabled`: `[]` while disabled and when the load fails. A newer run or unmount discards a
 * late result. `load` is read when `enabled` changes, not on every render (pass a stable or inline function).
 */
export function useLoadedList<T>(load: () => Promise<readonly T[]>, enabled: boolean): readonly T[] {
  const [items, setItems] = useState<readonly T[]>([]);
  useEffect(() => {
    let cancelled = false;
    if (!enabled) {
      setItems([]);
      return () => {
        cancelled = true;
      };
    }
    void load().then(
      (next) => {
        if (!cancelled) setItems(next);
      },
      () => {
        if (!cancelled) setItems([]);
      },
    );
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled]);
  return items;
}
