import type {
  LoadedDocument,
  SearchOptions,
  SearchProvider,
  SearchResult,
} from "@rightkit/document-core/documents/types";

interface CrossDocMatch {
  path: string;
  displayName: string;
  matchIndexWithinDoc: number;
  globalIndex: number;
}

/**
 * Build a synthetic SearchProvider that aggregates matches across multiple warm tabs.
 * The provider fans out find() to all registered providers, builds a flat match list in
 * tab order, and navigates between tabs on next/prev.
 */
export function buildCrossDocProvider(
  providers: Map<string, SearchProvider>,
  openDocs: LoadedDocument[],
  activate: (path: string) => Promise<void>,
): SearchProvider {
  let matches: CrossDocMatch[] = [];
  let globalActive = -1;
  let lastQuery = "";
  let lastOptions: SearchOptions = { matchCase: false, wholeWord: false };

  const orderedDocs = openDocs.filter((d) => providers.has(d.path));

  const find = async (query: string, options: SearchOptions): Promise<SearchResult> => {
    matches = [];
    globalActive = -1;
    lastQuery = query;
    lastOptions = options;

    if (!query) return { count: 0, activeIndex: -1 };

    // Each provider.find() is an independent per-doc search (some cross a render/IPC boundary,
    // e.g. docx) — fan them out concurrently, then stitch the results back together in the
    // existing orderedDocs order so matches/globalIndex stay deterministic regardless of which
    // provider actually finishes first.
    const perDocResults = await Promise.all(
      orderedDocs.map(async (doc) => {
        const provider = providers.get(doc.path);
        if (!provider) return null;
        try {
          return { doc, res: await provider.find(query, options) };
        } catch {
          // Skip providers that fail (e.g. cold docx tabs).
          return null;
        }
      }),
    );
    for (const entry of perDocResults) {
      if (!entry) continue;
      for (let i = 0; i < entry.res.count; i++) {
        matches.push({
          path: entry.doc.path,
          displayName: entry.doc.displayName,
          matchIndexWithinDoc: i,
          globalIndex: matches.length,
        });
      }
    }

    globalActive = matches.length > 0 ? 0 : -1;
    if (globalActive >= 0) {
      await navigateTo(globalActive);
    }
    return { count: matches.length, activeIndex: globalActive };
  };

  const navigateTo = async (idx: number) => {
    if (idx < 0 || idx >= matches.length) return;
    const m = matches[idx];
    await activate(m.path);
    const provider = providers.get(m.path);
    if (!provider) return;
    if (provider.seek) {
      provider.seek(m.matchIndexWithinDoc);
      return;
    }
    await provider.find(lastQuery, lastOptions);
    for (let i = 0; i < m.matchIndexWithinDoc; i++) {
      await provider.next();
    }
  };

  const step = async (dir: 1 | -1): Promise<SearchResult> => {
    if (matches.length === 0) return { count: 0, activeIndex: -1 };
    globalActive = (((globalActive + dir) % matches.length) + matches.length) % matches.length;
    await navigateTo(globalActive);
    return { count: matches.length, activeIndex: globalActive };
  };

  return {
    kind: "source",
    canReplace: false,
    supportsMatchCase: true,
    supportsWholeWord: true,
    find,
    next: () => step(1),
    prev: () => step(-1),
    close: () => {
      matches = [];
      globalActive = -1;
      // Close all sub-providers' find state.
      for (const doc of orderedDocs) {
        providers.get(doc.path)?.close();
      }
    },
  };
}
