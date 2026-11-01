import { useCallback, useEffect, useRef, useState } from "react";
import type { IframeFind, SearchProvider } from "../lib/searchUtils";

export function useReaderFindProvider(findOpen: boolean) {
  const findRegistry = useRef<Map<string, IframeFind>>(new Map());
  const registerFind = useCallback((id: string, api: IframeFind | null) => {
    if (api) findRegistry.current.set(id, api);
    else findRegistry.current.delete(id);
  }, []);
  const [findProvider] = useState<SearchProvider>(() => {
    let counts: { api: IframeFind; count: number }[] = [];
    let total = 0;
    let active = -1;
    const ordered = () =>
      [...findRegistry.current.values()].sort(
        (a, b) => (a.el()?.getBoundingClientRect().top ?? 0) - (b.el()?.getBoundingClientRect().top ?? 0)
      );
    const repaint = () => {
      let acc = 0;
      for (const c of counts) {
        const localActive = active >= acc && active < acc + c.count ? active - acc : -1;
        c.api.paint(localActive);
        acc += c.count;
      }
    };
    return {
      supportsMatchCase: true,
      supportsWholeWord: true,
      find: (q, opts) => {
        counts = ordered().map((api) => ({ api, count: api.search(q, opts) }));
        total = counts.reduce((s, c) => s + c.count, 0);
        active = total > 0 ? 0 : -1;
        repaint();
        return { count: total, activeIndex: active };
      },
      next: () => {
        if (total) {
          active = (active + 1) % total;
          repaint();
        }
        return { count: total, activeIndex: active };
      },
      prev: () => {
        if (total) {
          active = (active - 1 + total) % total;
          repaint();
        }
        return { count: total, activeIndex: active };
      },
      close: () => {
        ordered().forEach((api) => api.clear());
        counts = [];
        total = 0;
        active = -1;
      },
    };
  });

  useEffect(() => {
    if (!findOpen) findProvider.close();
  }, [findOpen, findProvider]);

  return { findProvider, registerFind };
}
